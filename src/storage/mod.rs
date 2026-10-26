pub mod db;
pub mod history;
pub mod models;

use anyhow::{Context, Result};
use std::path::PathBuf;

pub use db::Database;
use history::CommandRecord;
use models::SearchFilter;

/// List recorded sessions, optionally filtered by group.
pub fn list_sessions(group: Option<String>) -> Result<()> {
    let db = Database::open()?;
    let sessions = db.list_sessions(group.as_deref())?;

    if sessions.is_empty() {
        println!("No sessions found.");
        return Ok(());
    }

    println!(
        "{:<10} {:<16} {:<20} {:<12} {:<10} TAGS",
        "ID", "NAME", "DATE", "DURATION", "GROUP"
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

/// Run a search headlessly and print the results as JSON to stdout.
pub fn search_json(query: Option<String>, filter: &SearchFilter) -> Result<()> {
    let query = query.unwrap_or_default();
    if query.trim().is_empty() && !filter.has_metadata_filters() {
        anyhow::bail!("a search query or a --failed/--cwd/--since filter is required with --format json");
    }
    let db = Database::open()?;
    let hits = db.search(&query, filter)?;
    println!("{}", serde_json::to_string_pretty(&hits)?);
    Ok(())
}

/// Add a note to a recorded session.
pub fn annotate_session(id: &str, note: &str) -> Result<()> {
    let db = Database::open()?;
    let full_id = db.add_annotation(id, note)?;
    println!("Added note to session {}", &full_id[..8]);
    Ok(())
}

/// Rename a recorded session.
pub fn rename_session(id: &str, new_name: &str) -> Result<()> {
    let db = Database::open()?;
    let full_id = db.rename_session(id, new_name)?;
    println!("Renamed session {} to \"{}\"", &full_id[..8], new_name);
    Ok(())
}

/// Delete one or more recorded sessions and all their data.
pub fn delete_sessions(ids: &[String], force: bool) -> Result<()> {
    let db = Database::open()?;

    if !force {
        // Resolve all first to show the user what they're deleting
        let mut resolved: Vec<(String, String)> = Vec::new();
        for id in ids {
            let full_id = db.resolve_session_id(id)?;
            let session = db.get_session_by_id(&full_id)?;
            let label = session
                .name
                .unwrap_or_else(|| full_id[..8].to_string());
            resolved.push((full_id, label));
        }
        let labels: Vec<&str> = resolved.iter().map(|(_, l)| l.as_str()).collect();
        eprint!("Delete session{} {} ? [y/N] ",
            if ids.len() > 1 { "s" } else { "" },
            labels.join(", "),
        );
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    for id in ids {
        let (full_id, name) = db.delete_session(id)?;
        let label = name
            .as_deref()
            .unwrap_or(&full_id[..8]);
        println!("Deleted session {}", label);
    }
    Ok(())
}

/// Export a session as a portable JSON file.
pub fn export_session(id: &str, output: Option<PathBuf>) -> Result<()> {
    let db = Database::open()?;
    let export = db.export_session(id)?;
    let json = serde_json::to_string_pretty(&export)?;

    let label = export
        .session
        .name
        .as_deref()
        .unwrap_or(&export.session.id[..8]);

    match output {
        Some(path) => {
            std::fs::write(&path, &json)?;
            println!(
                "Exported session {} to {} ({} chunks, {} notes)",
                label,
                path.display(),
                export.chunks.len(),
                export.annotations.len(),
            );
        }
        None => print!("{json}"),
    }

    Ok(())
}

/// Import a session from a JSON file.
pub fn import_session(file: &std::path::Path) -> Result<()> {
    let content = std::fs::read_to_string(file)
        .with_context(|| format!("Failed to read {}", file.display()))?;
    let export: models::SessionExport = serde_json::from_str(&content)
        .with_context(|| format!("Invalid session JSON in {}", file.display()))?;

    let label = export
        .session
        .name
        .clone()
        .unwrap_or_else(|| export.session.id[..8].to_string());

    let db = Database::open()?;
    db.import_session(&export)?;

    println!(
        "Imported session {} ({} chunks, {} notes)",
        label,
        export.chunks.len(),
        export.annotations.len(),
    );

    Ok(())
}

/// Show storage statistics.
pub fn show_stats() -> Result<()> {
    let db = Database::open()?;
    let stats = db.stats()?;

    println!("broll statistics");
    println!("{}", "-".repeat(40));
    println!("Sessions:      {}", stats.session_count);
    println!("Commands:      {}", stats.command_count);
    println!("Output chunks: {}", stats.chunk_count.saturating_sub(stats.command_count));
    println!("Annotations:   {}", stats.annotation_count);
    println!("Database size: {}", format_bytes(stats.db_size_bytes));

    if let Some(oldest) = stats.oldest_session {
        println!("Oldest:        {}", oldest.format("%Y-%m-%d %H:%M:%S"));
    }
    if let Some(newest) = stats.newest_session {
        println!("Newest:        {}", newest.format("%Y-%m-%d %H:%M:%S"));
    }

    Ok(())
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
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

fn resolve_target_session(db: &Database, id: Option<&str>) -> Result<(String, bool)> {
    let current = std::env::var(crate::recorder::SESSION_ENV_VAR).ok();
    let full_id = match (id, &current) {
        (Some(id), _) => db.resolve_session_id(id)?,
        (None, Some(current)) => current.clone(),
        (None, None) => db
            .latest_session_id()?
            .context("No sessions recorded yet.")?,
    };
    let in_session = current.as_deref() == Some(full_id.as_str());
    Ok((full_id, in_session))
}

fn load_commands(db: &Database, id: Option<&str>) -> Result<(String, Vec<CommandRecord>)> {
    let (full_id, in_session) = resolve_target_session(db, id)?;
    let chunks = db.get_session_chunks(&full_id)?;
    Ok((full_id, history::group_commands(chunks, in_session)))
}

pub fn print_last(n: usize, session: Option<String>, with_command: bool) -> Result<()> {
    anyhow::ensure!(n >= 1, "N must be 1 or greater");
    let db = Database::open()?;
    let (_, records) = load_commands(&db, session.as_deref())?;
    let record = records
        .len()
        .checked_sub(n)
        .and_then(|i| records.get(i))
        .with_context(|| format!("Only {} finished command(s) in this session", records.len()))?;

    if with_command {
        println!("$ {}", record.command.content);
    }
    let output = record.output_text();
    if !output.is_empty() {
        println!("{output}");
    }
    Ok(())
}

pub fn show_context(
    session: Option<String>,
    last: usize,
    failed: bool,
    max_lines: usize,
    output: Option<PathBuf>,
) -> Result<()> {
    let db = Database::open()?;
    let (full_id, records) = load_commands(&db, session.as_deref())?;
    let selected: Vec<&CommandRecord> = records.iter().filter(|r| !failed || r.failed()).collect();
    if selected.is_empty() {
        eprintln!("No matching commands found.");
        return Ok(());
    }
    let start = selected.len().saturating_sub(last.max(1));
    let session = db.get_session_by_id(&full_id)?;
    let markdown = history::render_context(&session, &selected[start..], max_lines);

    match output {
        Some(path) => {
            std::fs::write(&path, &markdown)?;
            println!(
                "Wrote {} command(s) to {}",
                selected.len() - start,
                path.display()
            );
        }
        None => print!("{markdown}"),
    }
    Ok(())
}
