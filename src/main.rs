mod cli;
mod filter;
mod recorder;
mod storage;
mod tui;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Command, Format};
use storage::models::SearchFilter;

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Start { name, tag, group, no_filter, dir } => {
            recorder::start_session(name, tag, group, no_filter, dir)?;
        }
        Command::Stop => {
            recorder::stop_session()?;
        }
        Command::List { group } => {
            storage::list_sessions(group)?;
        }
        Command::Search { query, group, terminal, format, failed, cwd, since } => {
            let filter = SearchFilter {
                group,
                terminal,
                failed,
                cwd: cwd.as_deref().map(cli::absolute_dir).transpose()?,
                since,
            };
            match format {
                Format::Text => tui::search::run(query, filter)?,
                Format::Json => storage::search_json(query, &filter)?,
            }
        }
        Command::Last { n, session, command } => {
            storage::print_last(n, session, command)?;
        }
        Command::Context { session, last, failed, max_lines, output } => {
            storage::show_context(session, last, failed, max_lines, output)?;
        }

        Command::Annotate { id, note } => {
            storage::annotate_session(&id, &note)?;
        }
        Command::Rename { id, name } => {
            storage::rename_session(&id, &name)?;
        }
        Command::Export { id, output } => {
            storage::export_session(&id, output)?;
        }
        Command::Import { file } => {
            storage::import_session(&file)?;
        }
        Command::Delete { ids, force } => {
            storage::delete_sessions(&ids, force)?;
        }
        Command::View { id } => {
            tui::view::run(&id)?;
        }
        Command::Stats => {
            storage::show_stats()?;
        }
        Command::Extract { id, output } => {
            storage::extract_commands(&id, output)?;
        }
    }

    Ok(())
}
