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

/// How long to buffer output before flushing to DB.
const FLUSH_INTERVAL: Duration = Duration::from_millis(100);

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

    // Put terminal into raw mode
    crossterm::terminal::enable_raw_mode()?;

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

    // Channel for sending captured output to the storage thread
    let (tx, rx) = mpsc::channel::<Vec<u8>>();

    // Spawn storage thread that buffers and coalesces chunks
    let storage_session_id = session_id.clone();
    let storage_handle = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let mut last_flush = Instant::now();

        let flush = |buf: &mut Vec<u8>, db: &Database, sid: &str, no_filt: bool| {
            if buf.is_empty() {
                return;
            }
            let text = String::from_utf8_lossy(buf).to_string();
            // Strip ANSI escapes before storing
            let clean = strip_ansi_escapes::strip_str(&text);
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
                    kind: ChunkKind::Output,
                };
                let _ = db.insert_chunk(&chunk);
            }
            buf.clear();
        };

        if let Ok(db) = Database::open() {
            loop {
                match rx.recv_timeout(FLUSH_INTERVAL) {
                    Ok(data) => {
                        buffer.extend_from_slice(&data);
                        // Flush if buffer has a complete line or enough time passed
                        if buffer.contains(&b'\n') || last_flush.elapsed() >= FLUSH_INTERVAL {
                            flush(&mut buffer, &db, &storage_session_id, no_filter);
                            last_flush = Instant::now();
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        // Flush whatever we have buffered
                        flush(&mut buffer, &db, &storage_session_id, no_filter);
                        last_flush = Instant::now();
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        // Final flush
                        flush(&mut buffer, &db, &storage_session_id, no_filter);
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
                let _ = tx.send(raw.to_vec());
            }
            Err(_) => break,
        }
    }

    // Signal storage thread to finish
    drop(tx);

    // Restore terminal
    crossterm::terminal::disable_raw_mode()?;

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
