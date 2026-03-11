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
        /// Give the session a name for easier identification and lookup
        #[arg(short, long)]
        name: Option<String>,

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
        /// Session ID, prefix, or name
        id: String,
    },

    /// Add a note to a recorded session
    Annotate {
        /// Session ID, prefix, or name
        id: String,

        /// Note text to attach
        note: String,
    },

    /// Rename a recorded session
    Rename {
        /// Session ID, prefix, or current name
        id: String,

        /// New name for the session
        name: String,
    },

    /// Delete a recorded session and all its data
    Delete {
        /// Session ID, prefix, or name
        id: String,

        /// Skip confirmation prompt
        #[arg(short, long, default_value_t = false)]
        force: bool,
    },

    /// Export a session as a portable JSON file
    Export {
        /// Session ID, prefix, or name
        id: String,

        /// Output file (defaults to stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Import a session from a JSON file
    Import {
        /// Path to the JSON file to import
        file: PathBuf,
    },

    /// Extract commands from a session as a script
    Extract {
        /// Session ID, prefix, or name
        id: String,

        /// Output file (defaults to stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}
