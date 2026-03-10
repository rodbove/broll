use anyhow::{Context, Result};
use chrono::Utc;
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::filter;
use crate::storage::models::{Chunk, ChunkKind, Session};
use crate::storage::Database;

/// Marker env var so we can detect nested sessions and stop gracefully.
const SESSION_ENV_VAR: &str = "BROLL_SESSION_ID";

/// Unique markers emitted by shell hooks to delimit command output.
/// Uses OSC sequences that terminals silently ignore.
/// PRECMD marker = prompt is about to be shown (command finished).
/// PREEXEC marker = command is about to execute (contains the command text, percent-encoded).
/// Format: \x1b]777;broll-exec;ENCODED_COMMAND\x07
const PREEXEC_MARKER_PREFIX: &str = "\x1b]777;broll-exec;";
const PREEXEC_MARKER_END: char = '\x07';
const PRECMD_MARKER: &str = "\x1b]777;broll-cmd\x07";

/// RAII guard that restores terminal from raw mode on drop.
struct RawModeGuard;

impl RawModeGuard {
    fn enable() -> Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// How long to wait for more data before flushing an incomplete line to DB.
const INCOMPLETE_LINE_TIMEOUT: Duration = Duration::from_secs(2);

/// Create a temporary rc file that sources the user's config then installs hooks.
/// The preexec hook emits the command text directly inside the OSC marker.
/// Only BEL (\x07) could break the marker, so we strip it with parameter expansion.
fn create_hook_rc(shell_name: &str) -> Option<tempfile::TempDir> {
    // Use broll's data directory for temp files to avoid system TMPDIR permission issues
    let broll_data = dirs::data_dir()?.join("broll");
    std::fs::create_dir_all(&broll_data).ok()?;
    let tmp_dir = tempfile::Builder::new()
        .prefix("broll-")
        .tempdir_in(&broll_data)
        .ok()?;

    let hook_code = match shell_name {
        "zsh" => {
            let user_zshrc = dirs::home_dir()
                .map(|h| h.join(".zshrc"))
                .filter(|p| p.exists());
            let source_line = user_zshrc
                .map(|p| format!("[[ -f \"{}\" ]] && source \"{0}\"\n", p.display()))
                .unwrap_or_default();

            let rc_path = tmp_dir.path().join(".zshrc");
            // zsh preexec receives the command line as $1
            // Strip BEL chars to avoid breaking the OSC sequence
            let content = format!(
                concat!(
                    "{}", // source user's zshrc
                    "_broll_preexec() {{\n",
                    "  local cmd=\"${{1//$'\\a'/}}\"\n",
                    "  printf '\\e]777;broll-exec;%s\\a' \"$cmd\"\n",
                    "}}\n",
                    "_broll_precmd() {{ printf '\\e]777;broll-cmd\\a'; }}\n",
                    "autoload -Uz add-zsh-hook\n",
                    "add-zsh-hook preexec _broll_preexec\n",
                    "add-zsh-hook precmd _broll_precmd\n",
                ),
                source_line,
            );
            std::fs::write(&rc_path, content).ok()?;
            Some(())
        }
        "bash" => {
            let user_bashrc = dirs::home_dir()
                .map(|h| h.join(".bashrc"))
                .filter(|p| p.exists());
            let source_line = user_bashrc
                .map(|p| format!("[[ -f \"{}\" ]] && source \"{0}\"\n", p.display()))
                .unwrap_or_default();

            let rc_path = tmp_dir.path().join(".bashrc");
            // bash: emit the last command in precmd (before the precmd marker)
            // using fc -ln -1 to get the full command line
            let content = format!(
                concat!(
                    "{}", // source user's bashrc
                    "_broll_last_hist=\"\"\n",
                    "_broll_precmd() {{\n",
                    "  local cmd\n",
                    "  cmd=$(fc -ln -1 2>/dev/null | sed 's/^[[:space:]]*//')\n",
                    "  cmd=\"${{cmd//$'\\a'/}}\"\n",
                    "  if [[ -n \"$cmd\" && \"$cmd\" != \"$_broll_last_hist\" ]]; then\n",
                    "    _broll_last_hist=\"$cmd\"\n",
                    "    printf '\\e]777;broll-exec;%s\\a' \"$cmd\"\n",
                    "  fi\n",
                    "  printf '\\e]777;broll-cmd\\a'\n",
                    "}}\n",
                    "PROMPT_COMMAND=\"_broll_precmd;${{PROMPT_COMMAND}}\"\n",
                ),
                source_line,
            );
            std::fs::write(&rc_path, content).ok()?;
            Some(())
        }
        _ => None,
    };

    hook_code.map(|_| tmp_dir)
}

/// States for tracking what part of the PTY output we're in.
#[derive(PartialEq)]
enum CaptureState {
    /// Between precmd (prompt shown) and preexec (command started).
    /// This is prompt + user typing — skip this.
    Idle,
    /// Between preexec (command started) and precmd (command finished).
    /// This is real command output — capture this.
    Capturing,
}

/// Start a recording session by spawning a sub-shell in a PTY.
pub fn start_session(
    tag: Option<String>,
    group: Option<String>,
    no_filter: bool,
    dir: Option<std::path::PathBuf>,
) -> Result<()> {
    if std::env::var(SESSION_ENV_VAR).is_ok() {
        anyhow::bail!("Already inside a broll session. Run `exit` or `broll stop` first.");
    }

    let session_id = Uuid::new_v4().to_string();
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let terminal_label = format!("term-{}", &session_id[..8]);
    let tags = tag.map(|t| vec![t]).unwrap_or_default();

    let session = Session {
        id: session_id.clone(),
        started_at: Utc::now(),
        ended_at: None,
        group,
        terminal_label: terminal_label.clone(),
        tags,
        shell: shell.clone(),
    };

    let db = Database::open()?;
    db.create_session(&session)?;

    println!("broll: recording started (session {})", &session_id[..8]);
    println!("broll: exit the shell or run `broll stop` to end recording");

    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));

    let pty_system = NativePtySystem::default();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("Failed to open PTY")?;

    let shell_name = std::path::Path::new(&shell)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("sh")
        .to_string();

    let mut cmd = CommandBuilder::new(&shell);
    cmd.env(SESSION_ENV_VAR, &session_id);

    // Set working directory (defaults to current directory)
    let work_dir = dir.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into()));
    cmd.cwd(&work_dir);

    // Create temp rc file with hooks; keep _tmp_dir alive until session ends
    let _tmp_dir = create_hook_rc(&shell_name);
    let has_hooks = _tmp_dir.is_some();
    if let Some(ref tmp) = _tmp_dir {
        match shell_name.as_str() {
            "zsh" => {
                cmd.env("ZDOTDIR", tmp.path().to_str().unwrap_or("/tmp"));
            }
            "bash" => {
                let rc_path = tmp.path().join(".bashrc");
                cmd.args(["--rcfile", rc_path.to_str().unwrap_or("/tmp/.bashrc")]);
            }
            _ => {}
        }
    }

    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;

    let _raw_guard = RawModeGuard::enable()?;

    let resize_flag = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGWINCH, Arc::clone(&resize_flag))?;

    let (tx, rx) = mpsc::channel::<Vec<u8>>();

    // Spawn thread to forward stdin -> PTY
    let mut pty_writer = writer;
    let _stdin_handle = std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 1024];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if pty_writer.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Spawn storage thread. Uses markers to only capture real command output.
    // Renders output through a virtual terminal (vt100) to preserve column layout.
    let storage_session_id = session_id.clone();
    let storage_handle = std::thread::spawn(move || {
        let mut raw_buf = String::new();
        let mut last_data_at = Instant::now();
        let mut state = CaptureState::Idle;
        // Accumulate raw bytes for current command output to render through vt100
        let mut cmd_output_bytes: Vec<u8> = Vec::new();

        /// Render raw terminal bytes through a virtual terminal and return plain text lines.
        fn render_vt(raw: &[u8], term_cols: u16) -> String {
            let mut parser = vt100::Parser::new(500, term_cols, 0);
            parser.process(raw);
            let screen = parser.screen();
            let mut lines: Vec<String> = Vec::new();
            for row in 0..screen.size().0 {
                let line = screen.contents_between(
                    row, 0,
                    row, screen.size().1 - 1,
                );
                lines.push(line.trim_end().to_string());
            }
            // Trim trailing empty lines
            while lines.last().is_some_and(|l: &String| l.is_empty()) {
                lines.pop();
            }
            lines.join("\n")
        }

        /// Lines emitted by macOS zsh session save/restore — not real command output.
        fn is_shell_noise(line: &str) -> bool {
            let trimmed = line.trim();
            trimmed == "Saving session..."
                || trimmed.starts_with("...saving history...")
                || trimmed.starts_with("...copying shared history...")
                || trimmed.starts_with("...completed.")
                || trimmed.starts_with("Deleting expired sessions...")
        }

        let store_chunk = |content_raw: &str, kind: ChunkKind, db: &Database, sid: &str, no_filt: bool| {
            let content: String = content_raw
                .lines()
                .filter(|l| !is_shell_noise(l))
                .collect::<Vec<_>>()
                .join("\n");
            let content = if no_filt {
                content
            } else {
                filter::redact(&content)
            };
            let trimmed = content.trim();
            if trimmed.is_empty() {
                return;
            }

            let chunk = Chunk {
                id: 0,
                session_id: sid.to_string(),
                timestamp: Utc::now(),
                content,
                kind,
            };
            if let Err(e) = db.insert_chunk(&chunk) {
                eprintln!("broll: failed to store chunk: {e}");
            }
        };

        if let Ok(db) = Database::open() {
            loop {
                match rx.recv_timeout(INCOMPLETE_LINE_TIMEOUT) {
                    Ok(data) => {
                        last_data_at = Instant::now();
                        let text = String::from_utf8_lossy(&data);
                        raw_buf.push_str(&text);

                        if has_hooks {
                            // Process buffer looking for markers
                            loop {
                                if state == CaptureState::Idle {
                                    // Look for preexec marker: \x1b]777;broll-exec;ENCODED\x07
                                    if let Some(pos) = raw_buf.find(PREEXEC_MARKER_PREFIX) {
                                        let after_prefix = pos + PREEXEC_MARKER_PREFIX.len();
                                        // Find the BEL terminator
                                        if let Some(end) = raw_buf[after_prefix..].find(PREEXEC_MARKER_END) {
                                            let command = raw_buf[after_prefix..after_prefix + end].to_string();
                                            // Store the command as input
                                            store_chunk(
                                                &command,
                                                ChunkKind::Input,
                                                &db,
                                                &storage_session_id,
                                                no_filter,
                                            );
                                            raw_buf.drain(..after_prefix + end + 1);
                                            state = CaptureState::Capturing;
                                            cmd_output_bytes.clear();
                                        } else {
                                            // BEL not yet received — keep buffering
                                            break;
                                        }
                                    } else {
                                        let keep = PREEXEC_MARKER_PREFIX.len();
                                        if raw_buf.len() > keep {
                                            raw_buf.drain(..raw_buf.len() - keep);
                                        }
                                        break;
                                    }
                                } else {
                                    if let Some(pos) = raw_buf.find(PRECMD_MARKER) {
                                        let output: String = raw_buf.drain(..pos).collect();
                                        raw_buf.drain(..PRECMD_MARKER.len());

                                        cmd_output_bytes.extend_from_slice(output.as_bytes());
                                        let rendered = render_vt(&cmd_output_bytes, cols);
                                        store_chunk(
                                            &rendered,
                                            ChunkKind::Output,
                                            &db,
                                            &storage_session_id,
                                            no_filter,
                                        );
                                        cmd_output_bytes.clear();
                                        state = CaptureState::Idle;
                                    } else {
                                        // No end marker yet — buffer the raw bytes
                                        let buffered: String = raw_buf.drain(..).collect();
                                        cmd_output_bytes.extend_from_slice(buffered.as_bytes());
                                        break;
                                    }
                                }
                            }
                        } else {
                            // No hooks: accumulate output, render through vt100
                            // on newlines to preserve column layout
                            cmd_output_bytes.extend_from_slice(raw_buf.as_bytes());
                            raw_buf.clear();
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        // Flush accumulated output on timeout
                        if last_data_at.elapsed() >= INCOMPLETE_LINE_TIMEOUT {
                            cmd_output_bytes.extend_from_slice(raw_buf.as_bytes());
                            raw_buf.clear();
                            if !cmd_output_bytes.is_empty() {
                                if !has_hooks || state == CaptureState::Capturing {
                                    let rendered = render_vt(&cmd_output_bytes, cols);
                                    store_chunk(
                                        &rendered,
                                        ChunkKind::Output,
                                        &db,
                                        &storage_session_id,
                                        no_filter,
                                    );
                                }
                                cmd_output_bytes.clear();
                            }
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        cmd_output_bytes.extend_from_slice(raw_buf.as_bytes());
                        if !cmd_output_bytes.is_empty()
                            && (!has_hooks || state == CaptureState::Capturing)
                        {
                            let rendered = render_vt(&cmd_output_bytes, cols);
                            store_chunk(
                                &rendered,
                                ChunkKind::Output,
                                &db,
                                &storage_session_id,
                                no_filter,
                            );
                        }
                        break;
                    }
                }
            }
        }
    });

    // Main thread: read PTY output -> stdout + send to storage
    let mut stdout = std::io::stdout();
    // Ensure cursor is on a fresh line after the "recording started" messages,
    // so the shell's first prompt renders correctly in raw mode.
    let _ = stdout.write_all(b"\r\n");
    let _ = stdout.flush();
    let mut buf = [0u8; 4096];

    loop {
        if resize_flag.swap(false, Ordering::Relaxed) {
            if let Ok((cols, rows)) = crossterm::terminal::size() {
                let _ = pair.master.resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                });
            }
        }

        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let raw = &buf[..n];

                // Strip markers before writing to stdout so they stay invisible
                let text = String::from_utf8_lossy(raw);
                if text.contains("\x1b]777;broll") {
                    let mut cleaned = text.replace(PRECMD_MARKER, "");
                    // Strip variable-length preexec markers: \x1b]777;broll-exec;...\x07
                    while let Some(start) = cleaned.find(PREEXEC_MARKER_PREFIX) {
                        if let Some(end) = cleaned[start..].find(PREEXEC_MARKER_END) {
                            cleaned.replace_range(start..start + end + 1, "");
                        } else {
                            // Incomplete marker at end — truncate it
                            cleaned.truncate(start);
                            break;
                        }
                    }
                    stdout.write_all(cleaned.as_bytes())?;
                } else {
                    stdout.write_all(raw)?;
                }
                stdout.flush()?;

                // Send original bytes (with markers) to storage for processing
                let _ = tx.send(raw.to_vec());
            }
            Err(_) => break,
        }
    }

    drop(tx);

    let _ = child.wait();

    // Drop raw mode BEFORE printing the exit message so \n works normally
    drop(_raw_guard);

    // Don't join stdin_handle — it blocks on stdin.read() until user presses a key.
    // It will be cleaned up when the process exits.
    let _ = storage_handle.join();

    db.end_session(&session_id)?;
    eprintln!("broll: session {} ended", &session_id[..8]);

    Ok(())
}

/// Stop the current recording session (called from within a sub-shell).
pub fn stop_session() -> Result<()> {
    match std::env::var(SESSION_ENV_VAR) {
        Ok(_id) => {
            println!("broll: stopping session, exit the shell to finalize.");
            std::process::exit(0);
        }
        Err(_) => {
            anyhow::bail!("Not inside a broll recording session.");
        }
    }
}
