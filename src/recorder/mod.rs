use anyhow::{Context, Result};
use chrono::Utc;
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use std::io::{Read, Write};
use std::sync::mpsc;
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
/// This covers streaming output that doesn't end with newlines (progress bars, etc).
const INCOMPLETE_LINE_TIMEOUT: Duration = Duration::from_secs(2);

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

    // Channels: one for PTY output, one for stdin (user input)
    enum StorageMsg {
        Output(Vec<u8>),
        Input(Vec<u8>),
    }
    let (tx, rx) = mpsc::channel::<StorageMsg>();

    // Spawn thread to forward stdin -> PTY and capture commands
    let input_tx = tx.clone();
    let stdin_handle = std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 1024];
        let mut cmd_buf = Vec::new();
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let data = &buf[..n];
                    if writer.write_all(data).is_err() {
                        break;
                    }
                    for &byte in data {
                        match byte {
                            b'\r' | b'\n' => {
                                // Enter pressed — flush the accumulated command
                                if !cmd_buf.is_empty() {
                                    let _ = input_tx.send(StorageMsg::Input(cmd_buf.clone()));
                                    cmd_buf.clear();
                                }
                            }
                            0x7f | 0x08 => {
                                // Backspace/delete — remove last char
                                cmd_buf.pop();
                            }
                            0x15 => {
                                // Ctrl-U — clear line
                                cmd_buf.clear();
                            }
                            0x17 => {
                                // Ctrl-W — delete last word
                                while cmd_buf.last().is_some_and(|&b| b == b' ') {
                                    cmd_buf.pop();
                                }
                                while cmd_buf.last().is_some_and(|&b| b != b' ') {
                                    cmd_buf.pop();
                                }
                            }
                            0x03 => {
                                // Ctrl-C — discard current input
                                cmd_buf.clear();
                            }
                            b if b >= 0x20 => {
                                // Printable characters
                                cmd_buf.push(byte);
                            }
                            _ => {
                                // Ignore other control chars (arrows, etc.)
                            }
                        }
                    }
                }
            }
        }
    });

    // Spawn storage thread that handles both input commands and output lines.
    let storage_session_id = session_id.clone();
    let storage_handle = std::thread::spawn(move || {
        let mut line_buf = String::new();
        let mut last_data_at = Instant::now();
        // Track recent commands so we can suppress their PTY echo in the output
        let mut recent_commands: Vec<String> = Vec::new();

        let store_chunk =
            |content: &str, kind: ChunkKind, db: &Database, sid: &str, no_filt: bool| {
                let clean = strip_ansi_escapes::strip_str(content);
                let content = if no_filt {
                    clean.to_string()
                } else {
                    filter::redact(&clean)
                };
                if !content.trim().is_empty() {
                    let chunk = Chunk {
                        id: 0,
                        session_id: sid.to_string(),
                        timestamp: Utc::now(),
                        content,
                        kind,
                    };
                    let _ = db.insert_chunk(&chunk);
                }
            };

        /// Check if an output line is just the echo of a recent command.
        fn is_echo(line: &str, recent_commands: &mut Vec<String>) -> bool {
            let cleaned = strip_ansi_escapes::strip_str(line);
            let trimmed = cleaned.trim();
            if let Some(pos) = recent_commands
                .iter()
                .position(|cmd| trimmed.ends_with(cmd.trim()))
            {
                recent_commands.remove(pos);
                return true;
            }
            false
        }

        if let Ok(db) = Database::open() {
            loop {
                match rx.recv_timeout(INCOMPLETE_LINE_TIMEOUT) {
                    Ok(StorageMsg::Input(data)) => {
                        // Complete command from stdin — store immediately
                        let cmd = String::from_utf8_lossy(&data).to_string();
                        recent_commands.push(cmd.clone());
                        // Keep only the last few to avoid unbounded growth
                        if recent_commands.len() > 10 {
                            recent_commands.remove(0);
                        }
                        store_chunk(
                            &cmd,
                            ChunkKind::Input,
                            &db,
                            &storage_session_id,
                            no_filter,
                        );
                    }
                    Ok(StorageMsg::Output(data)) => {
                        last_data_at = Instant::now();
                        let text = String::from_utf8_lossy(&data);
                        line_buf.push_str(&text);

                        // Flush all complete lines, skipping echoed commands
                        while let Some(pos) = line_buf.find('\n') {
                            let line: String = line_buf.drain(..=pos).collect();
                            if !is_echo(&line, &mut recent_commands) {
                                store_chunk(
                                    &line,
                                    ChunkKind::Output,
                                    &db,
                                    &storage_session_id,
                                    no_filter,
                                );
                            }
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if !line_buf.is_empty()
                            && last_data_at.elapsed() >= INCOMPLETE_LINE_TIMEOUT
                        {
                            let leftover: String = line_buf.drain(..).collect();
                            store_chunk(
                                &leftover,
                                ChunkKind::Output,
                                &db,
                                &storage_session_id,
                                no_filter,
                            );
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        if !line_buf.is_empty() {
                            store_chunk(
                                &line_buf,
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
    let mut buf = [0u8; 4096];

    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let raw = &buf[..n];

                // Write to stdout immediately (with ANSI codes for proper display)
                stdout.write_all(raw)?;
                stdout.flush()?;

                // Send raw bytes to storage thread for buffering
                let _ = tx.send(StorageMsg::Output(raw.to_vec()));
            }
            Err(_) => break,
        }
    }

    // Signal storage thread to finish
    drop(tx);

    // Restore terminal (guard handles this, but drop explicitly for clarity)
    drop(_raw_guard);

    let _ = child.wait();
    let _ = stdin_handle.join();
    let _ = storage_handle.join();

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
