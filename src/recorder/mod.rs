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

/// Unique marker emitted by shell hooks to delimit command output.
/// Uses an OSC sequence that terminals silently ignore.
/// PRECMD marker = prompt is about to be shown (command finished).
/// PREEXEC marker = command is about to execute.
const PREEXEC_MARKER: &str = "\x1b]777;broll-exec\x07";
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

/// Get the history file path for the current shell.
fn history_file_path(shell: &str) -> Option<std::path::PathBuf> {
    if let Ok(histfile) = std::env::var("HISTFILE") {
        let path = std::path::PathBuf::from(histfile);
        if path.exists() {
            return Some(path);
        }
    }

    let home = dirs::home_dir()?;
    let shell_name = std::path::Path::new(shell).file_name()?.to_str()?;

    let path = match shell_name {
        "zsh" => home.join(".zsh_history"),
        "bash" => home.join(".bash_history"),
        "fish" => home.join(".local/share/fish/fish_history"),
        _ => return None,
    };

    if path.exists() {
        Some(path)
    } else {
        None
    }
}

/// Read commands from a history file.
/// Handles zsh extended history format (`: timestamp:0;command`).
fn read_history_commands(path: &std::path::Path) -> Vec<String> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            if line.starts_with(": ") {
                line.splitn(2, ';').nth(1).map(|s| s.to_string())
            } else {
                Some(line.to_string())
            }
        })
        .collect()
}

/// Generate shell init code that installs preexec/precmd hooks to emit markers.
fn shell_hook_init(shell_name: &str) -> Option<String> {
    match shell_name {
        "zsh" => Some(format!(
            concat!(
                "_broll_preexec() {{ printf '{}'; }}; ",
                "_broll_precmd() {{ printf '{}'; }}; ",
                "autoload -Uz add-zsh-hook; ",
                "add-zsh-hook preexec _broll_preexec; ",
                "add-zsh-hook precmd _broll_precmd",
            ),
            PREEXEC_MARKER, PRECMD_MARKER,
        )),
        "bash" => {
            // Bash doesn't have native preexec. Use DEBUG trap for preexec
            // and PROMPT_COMMAND for precmd.
            Some(format!(
                concat!(
                    r#"_broll_preexec() {{ printf '{}'; }}; "#,
                    r#"trap '_broll_preexec' DEBUG; "#,
                    r#"PROMPT_COMMAND="_broll_precmd;${{PROMPT_COMMAND}}"; "#,
                    r#"_broll_precmd() {{ printf '{}'; }}"#,
                ),
                PREEXEC_MARKER, PRECMD_MARKER,
            ))
        }
        _ => None,
    }
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

    // Snapshot shell history before starting so we can diff later for commands
    let history_path = history_file_path(&shell);
    let history_before_count = history_path
        .as_ref()
        .map(|p| read_history_commands(p).len())
        .unwrap_or(0);

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

    // Install shell hooks via an rc file that sources the user's config then adds hooks
    let hook_init = shell_hook_init(&shell_name);
    if let Some(ref init) = hook_init {
        cmd.env("BROLL_HOOK_INIT", init);
    }

    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;

    let _raw_guard = RawModeGuard::enable()?;

    let resize_flag = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGWINCH, Arc::clone(&resize_flag))?;

    let (tx, rx) = mpsc::channel::<Vec<u8>>();

    // Send hook init command to the shell as if the user typed it
    if let Some(ref init) = hook_init {
        let init_cmd = format!("{}\r", init);
        let _ = writer.write_all(init_cmd.as_bytes());
        let _ = writer.flush();
    }

    // Spawn thread to forward stdin -> PTY
    let stdin_handle = std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 1024];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if writer.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Spawn storage thread. Uses markers to only capture real command output.
    let storage_session_id = session_id.clone();
    let has_hooks = hook_init.is_some();
    let storage_handle = std::thread::spawn(move || {
        let mut raw_buf = String::new();
        let mut last_data_at = Instant::now();
        let mut state = CaptureState::Idle;

        let store_output = |content: &str, db: &Database, sid: &str, no_filt: bool| {
            let clean = strip_ansi_escapes::strip_str(content);
            let content = if no_filt {
                clean.to_string()
            } else {
                filter::redact(&clean)
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
                kind: ChunkKind::Output,
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
                                    // Look for preexec marker (command about to run)
                                    if let Some(pos) = raw_buf.find(PREEXEC_MARKER) {
                                        // Discard everything up to and including the marker
                                        raw_buf.drain(..pos + PREEXEC_MARKER.len());
                                        state = CaptureState::Capturing;
                                    } else {
                                        // No marker yet — discard processed content but keep
                                        // a tail in case a marker spans two reads
                                        let keep = PREEXEC_MARKER.len();
                                        if raw_buf.len() > keep {
                                            raw_buf.drain(..raw_buf.len() - keep);
                                        }
                                        break;
                                    }
                                } else {
                                    // Capturing: look for precmd marker (command finished)
                                    if let Some(pos) = raw_buf.find(PRECMD_MARKER) {
                                        // Store everything before the marker as output
                                        let output: String = raw_buf.drain(..pos).collect();
                                        // Skip the marker itself
                                        raw_buf.drain(..PRECMD_MARKER.len());

                                        // Store line by line
                                        for line in output.split('\n') {
                                            store_output(
                                                line,
                                                &db,
                                                &storage_session_id,
                                                no_filter,
                                            );
                                        }
                                        state = CaptureState::Idle;
                                    } else {
                                        // No end marker yet. Flush complete lines, keep the rest.
                                        while let Some(pos) = raw_buf.find('\n') {
                                            let line: String =
                                                raw_buf.drain(..=pos).collect();
                                            store_output(
                                                &line,
                                                &db,
                                                &storage_session_id,
                                                no_filter,
                                            );
                                        }
                                        break;
                                    }
                                }
                            }
                        } else {
                            // No hooks: fallback to storing all output lines
                            while let Some(pos) = raw_buf.find('\n') {
                                let line: String = raw_buf.drain(..=pos).collect();
                                store_output(&line, &db, &storage_session_id, no_filter);
                            }
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if !raw_buf.is_empty()
                            && last_data_at.elapsed() >= INCOMPLETE_LINE_TIMEOUT
                        {
                            if !has_hooks || state == CaptureState::Capturing {
                                let leftover: String = raw_buf.drain(..).collect();
                                store_output(
                                    &leftover,
                                    &db,
                                    &storage_session_id,
                                    no_filter,
                                );
                            } else {
                                raw_buf.clear();
                            }
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        if !raw_buf.is_empty()
                            && (!has_hooks || state == CaptureState::Capturing)
                        {
                            store_output(
                                &raw_buf,
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
                    let cleaned = text
                        .replace(PREEXEC_MARKER, "")
                        .replace(PRECMD_MARKER, "");
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
    drop(_raw_guard);

    let _ = child.wait();
    let _ = stdin_handle.join();
    let _ = storage_handle.join();

    // Extract commands from shell history diff
    if let Some(ref hist_path) = history_path {
        let all_commands = read_history_commands(hist_path);
        if history_before_count < all_commands.len() {
            let new_commands = &all_commands[history_before_count..];

            for cmd_text in new_commands {
                let content = if no_filter {
                    cmd_text.clone()
                } else {
                    filter::redact(cmd_text)
                };
                let chunk = Chunk {
                    id: 0,
                    session_id: session_id.clone(),
                    timestamp: Utc::now(),
                    content,
                    kind: ChunkKind::Input,
                };
                if let Err(e) = db.insert_chunk(&chunk) {
                    eprintln!("broll: failed to store command: {e}");
                }
            }

            if !new_commands.is_empty() {
                eprintln!(
                    "broll: captured {} commands from shell history",
                    new_commands.len()
                );
            }
        }
    }

    db.end_session(&session_id)?;
    println!("\nbroll: session {} ended", &session_id[..8]);

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
