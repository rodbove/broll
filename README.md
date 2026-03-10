# broll

A terminal session recorder with searchable, timestamped output. Think of it as a flight recorder for your shell — record sessions, search through output history, and extract commands as scripts.

Unlike `asciinema` (video-like playback) or shell history (commands only), broll captures **both commands and output** with timestamps, stores them in a searchable SQLite database, and provides a TUI for browsing.

## Features

- **Record** terminal sessions via PTY sub-shell — works with any shell
- **Search** across all recorded sessions with full-text search (SQLite FTS5)
- **View** sessions in a scrollable TUI with timestamped, color-coded output
- **Extract** commands from any session as a runnable shell script
- **Filter** sensitive content (passwords, tokens, AWS keys, JWTs) automatically
- **Group** sessions to correlate multiple terminals working on the same task
- **Tag** sessions for easy organization and lookup

## Installation

### From source

```bash
git clone https://github.com/rodbove/broll.git
cd broll
cargo build --release
cp target/release/broll ~/.local/bin/
```

Requires Rust 1.85+ (edition 2024).

## Usage

### Record a session

```bash
# Start recording (spawns a sub-shell)
broll start

# Tag and group sessions for organization
broll start --tag "api-debug" --group "deploy-v2"

# Disable sensitive content filtering
broll start --no-filter

# Stop recording (or just `exit` the shell)
broll stop
```

### Search output

```bash
# Full-text search across all sessions (opens TUI)
broll search "connection refused"

# Filter by group or terminal
broll search "error" --group deploy-v2
broll search "panic" --terminal term-a3f2
```

The search TUI has two panels — results list on the left, preview on the right. Press `Tab` to switch focus between panels, `j/k` or arrows to navigate.

### View a session

```bash
# List all recorded sessions
broll list

# List sessions in a group
broll list --group deploy-v2

# View a session (use ID prefix)
broll view a3f2
```

The view TUI shows timestamped output with commands highlighted in green. Navigate with `j/k`, `PgUp/PgDn`, `g/G` for top/bottom.

### Extract commands

```bash
# Print commands to stdout
broll extract a3f2

# Save as a script
broll extract a3f2 --output reproduce.sh
```

## How it works

1. `broll start` spawns your shell inside a PTY (pseudo-terminal)
2. Everything you type is captured as **input** chunks; all output is captured as **output** chunks
3. Each chunk gets a timestamp and is stored in a local SQLite database with FTS5 indexing
4. Sensitive content (env vars with secret-like names, bearer tokens, AWS keys, JWTs, DB connection strings) is redacted before storage
5. When the shell exits, the session is finalized

Data is stored locally at:
- **macOS**: `~/Library/Application Support/broll/broll.db`
- **Linux**: `~/.local/share/broll/broll.db`

## Session groups

When working across multiple terminals on the same task, use groups to correlate them:

```bash
# Terminal 1: API server
broll start --group "debug-auth" --tag "server"

# Terminal 2: curl testing
broll start --group "debug-auth" --tag "client"

# Later: search across both
broll search "401" --group debug-auth
```

## Sensitive content filtering

Filtering is **on by default**. The following patterns are redacted:

| Pattern | Example |
|---------|---------|
| Secret env vars | `export API_KEY=sk-abc123` |
| Bearer tokens | `Authorization: Bearer eyJ...` |
| AWS access keys | `AKIAIOSFODNN7EXAMPLE` |
| Long secret values | `token: aGVsbG8gd29ybGQgdGhpcyBpcyBh...` |
| Database URLs | `postgres://user:pass@host/db` |
| JWTs | `eyJhbG...eyJzdW...signature` |
| Private key blocks | `-----BEGIN PRIVATE KEY-----` |

Use `--no-filter` to disable when you need to capture everything.

## Keybindings

### View TUI

| Key | Action |
|-----|--------|
| `j` / `Down` | Scroll down |
| `k` / `Up` | Scroll up |
| `Ctrl-d` / `PgDn` | Page down |
| `Ctrl-u` / `PgUp` | Page up |
| `g` / `Home` | Go to top |
| `G` / `End` | Go to bottom |
| `q` / `Esc` | Quit |

### Search TUI

| Key | Action |
|-----|--------|
| `Tab` | Switch focus (results / preview) |
| `j` / `Down` | Navigate results or scroll preview |
| `k` / `Up` | Navigate results or scroll preview |
| `q` / `Esc` | Quit |

## Tech stack

- **Rust** with edition 2024
- **ratatui** + **crossterm** for TUI
- **portable-pty** for PTY sub-shell
- **rusqlite** with bundled SQLite + FTS5 for storage and search
- **signal-hook** for terminal resize handling

## License

MIT
