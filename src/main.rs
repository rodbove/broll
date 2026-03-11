mod cli;
mod filter;
mod recorder;
mod storage;
mod tui;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Command};

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
        Command::Search { query, group, terminal } => {
            tui::search::run(query, group, terminal)?;
        }
        Command::Rename { id, name } => {
            storage::rename_session(&id, &name)?;
        }
        Command::View { id } => {
            tui::view::run(&id)?;
        }
        Command::Extract { id, output } => {
            storage::extract_commands(&id, output)?;
        }
    }

    Ok(())
}
