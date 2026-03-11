pub mod db;
pub mod models;

use anyhow::Result;
use std::path::PathBuf;

pub use db::Database;

/// List recorded sessions, optionally filtered by group.
pub fn list_sessions(group: Option<String>) -> Result<()> {
    let db = Database::open()?;
    let sessions = db.list_sessions(group.as_deref())?;

    if sessions.is_empty() {
        println!("No sessions found.");
        return Ok(());
    }

    println!(
        "{:<10} {:<16} {:<20} {:<12} {:<10} {}",
        "ID", "NAME", "DATE", "DURATION", "GROUP", "TAGS"
    );
    println!("{}", "-".repeat(86));

    for session in sessions {
        let short_id = &session.id[..8];
        let name = session.name.as_deref().unwrap_or("-");
        let duration = session
            .ended_at
            .map(|end| {
                let dur = end - session.started_at;
                format!("{}s", dur.num_seconds())
            })
            .unwrap_or_else(|| "recording".into());
        let group = session.group.as_deref().unwrap_or("-");
        let tags = session.tags.join(", ");

        println!(
            "{:<10} {:<16} {:<20} {:<12} {:<10} {}",
            short_id,
            name,
            session.started_at.format("%Y-%m-%d %H:%M:%S"),
            duration,
            group,
            tags,
        );
    }

    Ok(())
}

/// Rename a recorded session.
pub fn rename_session(id: &str, new_name: &str) -> Result<()> {
    let db = Database::open()?;
    let full_id = db.rename_session(id, new_name)?;
    println!("Renamed session {} to \"{}\"", &full_id[..8], new_name);
    Ok(())
}

/// Extract commands from a session and write them as a shell script.
pub fn extract_commands(id: &str, output: Option<PathBuf>) -> Result<()> {
    let db = Database::open()?;
    let commands = db.get_commands(id)?;

    if commands.is_empty() {
        println!("No commands found for session {id}.");
        return Ok(());
    }

    let script = format!("#!/usr/bin/env bash\n\n{}\n", commands.join("\n"));

    match output {
        Some(path) => {
            std::fs::write(&path, &script)?;
            println!("Extracted {} commands to {}", commands.len(), path.display());
        }
        None => print!("{script}"),
    }

    Ok(())
}
