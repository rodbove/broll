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
    // Check HISTFILE env var first (works for both bash and zsh)
    if let Ok(histfile) = std::env::var("HISTFILE") {
        let path = std::path::PathBuf::from(histfile);
        if path.exists() {
            return Some(path);
        }
    }

    let home = dirs::home_dir()?;
    let shell_name = std::path::Path::new(shell)
        .file_name()?
        .to_str()?;

    let path = match shell_name {
        "zsh" => home.join(".zsh_history"),
        "bash" => home.join(".bash_history"),
        "fish" => home.join(".local/share/fish/fish_history"),
        _ => return None,
    };

    if path.exists() { Some(path) } else { None }
}

/// Read commands from a history file, returning the lines.
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
            // zsh extended history format: ": 1234567890:0;actual command"
            if line.starts_with(": ") {
                line.splitn(2, ';').nth(1).map(|s| s.to_string())
            } else {
                Some(line.to_string())
            }
        })
        .collect()
}

/// Start a recording session by spawning a sub-shell in a PTY.
pub fn start_session(
    tag: Option<String>,
    group: Option<String>,
    no_filter: bool,
) -> Result<()> {
    // Prevent nested recordings
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

    // Snapshot shell history before starting so we can diff later
    let history_path = history_file_path(&shell);
    let history_before_count = history_path
        .as_ref()
        .map(|p| read_history_commands(p).len())
        .unwrap_or(0);

    println!("broll: recording started (session {})", &session_id[..8]);
    println!("broll: exit the shell or run `broll stop` to end recording");

    // Get terminal size
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

    let mut cmd = CommandBuilder::new(&shell);
    cmd.env(SESSION_ENV_VAR, &session_id);

    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave); // Release slave side

    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;

    // Put terminal into raw mode (guard restores on drop, even on panic/error)
    let _raw_guard = RawModeGuard::enable()?;

    // Handle terminal resize (SIGWINCH)
    let resize_flag = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGWINCH, Arc::clone(&resize_flag))?;

    let (tx, rx) = mpsc::channel::<Vec<u8>>();

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

    // Spawn storage thread that buffers output by complete lines.
    let storage_session_id = session_id.clone();
    let storage_handle = std::thread::spawn(move || {
        let mut line_buf = String::new();
        let mut last_data_at = Instant::now();

        let store_line = |line: &str, db: &Database, sid: &str, no_filt: bool| {
            let clean = strip_ansi_escapes::strip_str(line);

            // Handle carriage returns: when the shell does tab completion or
            // redraws the prompt, it sends \r to return to line start and
            // overwrites. We simulate this by only keeping the final segment.
            let resolved = if clean.contains('\r') {
                let mut result = String::new();
                for segment in clean.split('\r') {
                    if !segment.is_empty() {
                        // \r means "go to column 0", so new content overwrites
                        let overwrite_len = segment.len().min(result.len());
                        result.replace_range(..overwrite_len, segment);
                        if segment.len() > overwrite_len {
                            // Segment is longer than existing content
                            result = segment.to_string();
                        }
                    }
                }
                result
            } else {
                clean.to_string()
            };

            let content = if no_filt {
                resolved
            } else {
                filter::redact(&resolved)
            };

            // Skip lines that are just a shell prompt character
            let trimmed = content.trim();
            if trimmed.is_empty()
                || trimmed == "%"
                || trimmed == "$"
                || trimmed == ">"
                || trimmed == "#"
            {
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
                        line_buf.push_str(&text);

                        while let Some(pos) = line_buf.find('\n') {
                            let line: String = line_buf.drain(..=pos).collect();
                            store_line(&line, &db, &storage_session_id, no_filter);
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if !line_buf.is_empty()
                            && last_data_at.elapsed() >= INCOMPLETE_LINE_TIMEOUT
                        {
                            let leftover: String = line_buf.drain(..).collect();
                            store_line(&leftover, &db, &storage_session_id, no_filter);
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        if !line_buf.is_empty() {
                            store_line(&line_buf, &db, &storage_session_id, no_filter);
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
        // Check for terminal resize
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
                stdout.write_all(raw)?;
                stdout.flush()?;
                let _ = tx.send(raw.to_vec());
            }
            Err(_) => break,
        }
    }

    // Signal storage thread to finish
    drop(tx);

    // Restore terminal
    drop(_raw_guard);

    let _ = child.wait();
    let _ = stdin_handle.join();
    let _ = storage_handle.join();

    // Extract new commands from shell history by diffing before/after
    if let Some(ref hist_path) = history_path {
        let all_commands = read_history_commands(hist_path);
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

    db.end_session(&session_id)?;
    println!("\nbroll: session {} ended", &session_id[..8]);

    Ok(())
}

/// Stop the current recording session (called from within a sub-shell).
pub fn stop_session() -> Result<()> {
    match std::env::var(SESSION_ENV_VAR) {
        Ok(_id) => {
            // Just exit the sub-shell — the parent recorder will handle cleanup
            println!("broll: stopping session, exit the shell to finalize.");
            std::process::exit(0);
        }
        Err(_) => {
            anyhow::bail!("Not inside a broll recording session.");
        }
    }
}
