# b.roll

A terminal session recorder with searchable, timestamped output. Think of it as a flight recorder for your shell — record sessions, search through output history, and extract commands as scripts.

Unlike `asciinema` (video-like playback) or shell history (commands only), broll captures **both commands and output** with timestamps, stores them in a searchable SQLite database, and provides a TUI for browsing. Commands are captured in real-time via shell hooks (preexec/precmd), so tab completion, arrow keys, and readline editing all work transparently.

## Features

- **Record** terminal sessions via PTY sub-shell — works with zsh and bash
- **Name** sessions for easy identification and lookup later
- **Search** across all recorded sessions with prefix-matching full-text search (SQLite FTS5)
- **View** sessions in a scrollable TUI with timestamped, color-coded output, syntax highlighting, and in-session search (`/`)
- **Jump** from search results directly into the full session view, scrolled to the match
- **Extract** commands from any session as a runnable shell script
- **Filter** sensitive content (passwords, tokens, AWS keys, JWTs) automatically
- **Group** sessions to correlate multiple terminals working on the same task
- **Tag** sessions for easy organization and lookup
- **Annotate** sessions with notes after the fact
- **Rename** and **delete** sessions to keep things organized
- **Syntax highlighting** in view mode — JSON, log levels, URLs, and file:line references are automatically color-coded
- **Export/import** sessions as portable JSON files for sharing

## Installation

### From crates.io

```bash
cargo install broll
```

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
# Start recording (spawns a sub-shell in the current directory)
broll start

# Name the session for easy identification and lookup
broll start --name "api-debug-march"

# Tag and group sessions for organization
broll start --tag "api-debug" --group "deploy-v2"

# Combine name, tag, and group
broll start --name "fix-auth" --tag "server" --group "debug-auth"

# Start in a specific directory
broll start --dir /var/log

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

The search TUI has two panels — results list on the left, preview on the right. Press `Tab` to switch focus between panels, `j/k` or arrows to navigate. Press `Enter` on any result to open the full session in view mode, scrolled to the matching output with it highlighted. Press `Esc` to return to search results.

### View a session

```bash
# List all recorded sessions
broll list

# List sessions in a group
broll list --group deploy-v2

# View a session by ID prefix or name
broll view a3f2
broll view fix-auth
```

The view TUI shows timestamped output with commands highlighted in green and automatic syntax highlighting for output — JSON is color-coded (keys, strings, numbers, bools), log levels are highlighted (ERROR in red, WARN in yellow, INFO in green), and URLs and file:line references stand out. Navigate with `j/k`, `PgUp/PgDn`, `g/G` for top/bottom. Press `/` to search within the session — matching lines are highlighted, and `n/N` jumps between matches.

### Extract commands

```bash
# Print commands to stdout (by ID prefix or name)
broll extract a3f2
broll extract fix-auth

# Save as a script
broll extract a3f2 --output reproduce.sh
```

### Manage sessions

```bash
# Add a note to a session
broll annotate a3f2 "root cause: postgres wasn't running"
broll annotate fix-auth "resolved by rotating JWT signing key"

# Rename a session
broll rename a3f2 "deploy-postmortem"

# Delete a session (prompts for confirmation)
broll delete a3f2

# Delete multiple sessions at once
broll delete a3f2 7e91 c0ff

# Delete without confirmation
broll delete old-session --force
```

Notes are displayed at the top of the view TUI in a distinct colored section.

## How it works

1. `broll start` spawns your shell inside a PTY (pseudo-terminal) in the current directory
2. Shell hooks (preexec/precmd) capture commands in real-time with the exact text you typed
3. Output is rendered through a virtual terminal (vt100) to preserve column layout and formatting
4. Each chunk gets a timestamp and is stored in a local SQLite database with FTS5 indexing
5. Sensitive content (env vars with secret-like names, bearer tokens, AWS keys, JWTs, DB connection strings) is redacted before storage
6. When the shell exits, the session is finalized

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
| `/` | Search within session |
| `n` | Next search match |
| `N` | Previous search match |
| `q` / `Esc` | Quit (or go back to search) |

### Search TUI

| Key | Action |
|-----|--------|
| `Tab` | Switch focus (results / preview) |
| `j` / `Down` | Navigate results or scroll preview |
| `k` / `Up` | Navigate results or scroll preview |
| `Enter` | Open full session view at match |
| `q` / `Esc` | Quit |

## Tech stack

- **Rust** with edition 2024
- **ratatui** + **crossterm** for TUI
- **portable-pty** for PTY sub-shell
- **vt100** for virtual terminal rendering (preserves column layout)
- **rusqlite** with bundled SQLite + FTS5 for storage and search
- **signal-hook** for terminal resize handling

## License

MIT
