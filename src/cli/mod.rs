use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "broll", about = "Terminal session recorder with searchable output")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Start recording a new session (spawns a sub-shell)
    Start {
        /// Tag the session for easier lookup
        #[arg(short, long)]
        tag: Option<String>,

        /// Group ID to correlate multiple terminal sessions
        #[arg(short, long)]
        group: Option<String>,

        /// Disable sensitive content filtering
        #[arg(long, default_value_t = false)]
        no_filter: bool,

        /// Working directory for the session (defaults to current directory)
        #[arg(short, long)]
        dir: Option<PathBuf>,
    },

    /// Stop the current recording session
    Stop,

    /// List past recorded sessions
    List {
        /// Filter by group
        #[arg(short, long)]
        group: Option<String>,
    },

    /// Search session output (opens TUI)
    Search {
        /// Text or regex to search for
        query: String,

        /// Filter by group
        #[arg(short, long)]
        group: Option<String>,

        /// Filter by terminal label
        #[arg(short, long)]
        terminal: Option<String>,
    },

    /// View a recorded session (opens TUI)
    View {
        /// Session ID (or prefix)
        id: String,
    },

    /// Extract commands from a session as a script
    Extract {
        /// Session ID (or prefix)
        id: String,

        /// Output file (defaults to stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}
