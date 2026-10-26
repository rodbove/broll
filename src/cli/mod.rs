use chrono::{DateTime, Local, NaiveDate, TimeDelta, TimeZone, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};

/// Output format for commands that can print machine-readable results.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum, Default)]
pub enum Format {
    /// Interactive TUI (default).
    #[default]
    Text,
    /// Machine-readable JSON printed to stdout, for piping/scripting.
    Json,
}

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

    /// Search session output (opens TUI, or prints JSON with --format json)
    Search {
        /// Text to search for (opens live search if omitted; required for --format json)
        query: Option<String>,

        /// Filter by group
        #[arg(short, long)]
        group: Option<String>,

        /// Filter by terminal label
        #[arg(short, long)]
        terminal: Option<String>,

        /// Output format: text (interactive TUI) or json (pipe-friendly)
        #[arg(short, long, value_enum, default_value_t = Format::Text)]
        format: Format,

        /// Only match commands that exited with a non-zero status
        #[arg(long, default_value_t = false)]
        failed: bool,

        /// Only match commands run in this directory or below it
        #[arg(long)]
        cwd: Option<PathBuf>,

        /// Only match output newer than this (e.g. 30m, 2h, 3d, 1w, or 2026-10-01)
        #[arg(long, value_parser = parse_since)]
        since: Option<DateTime<Utc>>,
    },

    /// Print the output of a recent command (current session, or the latest one)
    Last {
        /// Which command, counting back from the most recent (1 = last)
        #[arg(default_value_t = 1)]
        n: usize,

        /// Session ID, prefix, or name
        #[arg(short, long)]
        session: Option<String>,

        /// Also print the command line before its output
        #[arg(short, long, default_value_t = false)]
        command: bool,
    },

    /// Print recent commands and output as markdown (for bug reports or AI assistants)
    Context {
        /// Session ID, prefix, or name (defaults to the current or latest session)
        #[arg(short, long)]
        session: Option<String>,

        /// Number of recent commands to include
        #[arg(short = 'n', long, default_value_t = 10)]
        last: usize,

        /// Only include commands that exited with a non-zero status
        #[arg(long, default_value_t = false)]
        failed: bool,

        /// Max output lines per command, keeping head and tail (0 = no limit)
        #[arg(long, default_value_t = 40)]
        max_lines: usize,

        /// Output file (defaults to stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,
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

    /// Delete one or more recorded sessions and all their data
    Delete {
        /// Session IDs, prefixes, or names
        #[arg(required = true)]
        ids: Vec<String>,

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

    /// Show storage statistics
    Stats,

    /// Extract commands from a session as a script
    Extract {
        /// Session ID, prefix, or name
        id: String,

        /// Output file (defaults to stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

pub fn parse_since(s: &str) -> Result<DateTime<Utc>, String> {
    parse_since_at(s, Utc::now())
}

fn parse_since_at(s: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    let s = s.trim();
    if let Ok(date) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let midnight = date.and_hms_opt(0, 0, 0).ok_or("invalid date")?;
        return Local
            .from_local_datetime(&midnight)
            .earliest()
            .map(|d| d.with_timezone(&Utc))
            .ok_or_else(|| format!("invalid local date '{s}'"));
    }

    let unit_at = s
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| format!("missing unit in '{s}' (use s, m, h, d, or w)"))?;
    let (amount, unit) = s.split_at(unit_at);
    let amount: i64 = amount
        .parse()
        .map_err(|_| format!("invalid duration '{s}' (e.g. 30m, 2h, 3d, 1w, or 2026-10-01)"))?;
    let delta = match unit {
        "s" => TimeDelta::try_seconds(amount),
        "m" => TimeDelta::try_minutes(amount),
        "h" => TimeDelta::try_hours(amount),
        "d" => TimeDelta::try_days(amount),
        "w" => TimeDelta::try_weeks(amount),
        _ => return Err(format!("unknown unit '{unit}' (use s, m, h, d, or w)")),
    }
    .ok_or_else(|| format!("duration '{s}' is too large"))?;
    now.checked_sub_signed(delta)
        .ok_or_else(|| format!("duration '{s}' is too large"))
}

pub fn absolute_dir(path: &Path) -> anyhow::Result<String> {
    let abs = std::path::absolute(path)?;
    let text = abs.to_string_lossy();
    let trimmed = text.trim_end_matches('/');
    Ok(if trimmed.is_empty() { "/".to_string() } else { trimmed.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn since_parses_relative_units() {
        let now = Utc::now();
        assert_eq!(parse_since_at("30m", now).unwrap(), now - TimeDelta::minutes(30));
        assert_eq!(parse_since_at("2h", now).unwrap(), now - TimeDelta::hours(2));
        assert_eq!(parse_since_at("3d", now).unwrap(), now - TimeDelta::days(3));
        assert_eq!(parse_since_at("1w", now).unwrap(), now - TimeDelta::weeks(1));
    }

    #[test]
    fn since_parses_calendar_date() {
        let parsed = parse_since_at("2026-10-01", Utc::now()).unwrap();
        let local = parsed.with_timezone(&Local);
        assert_eq!(local.date_naive(), NaiveDate::from_ymd_opt(2026, 10, 1).unwrap());
    }

    #[test]
    fn since_rejects_bad_input() {
        let now = Utc::now();
        for bad in ["", "10", "m", "5y", "abc", "99999999999999w"] {
            assert!(parse_since_at(bad, now).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn absolute_dir_trims_trailing_slash() {
        assert_eq!(absolute_dir(Path::new("/tmp/x/")).unwrap(), "/tmp/x");
        assert_eq!(absolute_dir(Path::new("/")).unwrap(), "/");
    }
}
