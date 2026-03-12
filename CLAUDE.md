# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is broll?

A Rust CLI tool that records terminal sessions and makes them searchable. It spawns a PTY sub-shell with shell hooks (zsh preexec/precmd, bash PROMPT_COMMAND) that emit OSC markers to distinguish commands from output. Captured content is stored in SQLite with FTS5 full-text search indexing. Sensitive content (tokens, passwords, AWS keys, JWTs, database URLs) is automatically redacted via regex patterns.

## Build & Run

```bash
cargo build --release          # Build optimized binary
cargo build                    # Debug build
cargo test                     # Run tests (unit tests in filter module)
cargo clippy                   # Lint
```

Requires Rust 1.85+ (edition 2024). No CI/CD, Makefile, or justfile exists.

## Architecture

**Entry point**: `src/main.rs` — clap-based CLI routing to subcommands (start, stop, list, search, view, extract, annotate, rename, delete, export, import).

**Core modules:**

- **`src/recorder/`** — Spawns PTY via `portable-pty`, installs shell hooks, manages multi-threaded I/O (stdin→PTY, PTY→stdout+storage) with mpsc channels. State machine tracks preexec/precmd OSC markers to split input from output. This is the most complex module (~500 lines).
- **`src/storage/`** — SQLite backend (`rusqlite` with bundled SQLite). Two tables: `sessions` and `chunks` (with `kind` = 'input'|'output'). FTS5 virtual table `chunks_fts` with trigger-based auto-indexing. Prefix-matching search queries. DB location: `~/Library/Application Support/broll/broll.db` (macOS) or `~/.local/share/broll/broll.db` (Linux).
- **`src/tui/`** — Two ratatui-based TUI modes: `search.rs` (two-panel search with Tab focus switching) and `view.rs` (scrollable session viewer with timestamps and syntax highlighting via `highlight.rs`).
- **`src/filter/`** — `redact()` function with regex patterns for 7 categories of secrets. Has the only unit tests in the project.
- **`src/cli/`** — clap derive-based `Command` enum defining all subcommands and their arguments.

**Key design patterns:**
- Shell hooks inject OSC escape sequences that the recorder's state machine parses to delimit commands vs output
- vt100 virtual terminal renders captured bytes to preserve terminal formatting/column alignment
- RAII guard manages terminal raw mode
- Session IDs support prefix matching for user convenience
- Recording sessions set terminal title via OSC and export `BROLL_SESSION` env var for prompt integration

## Commits

- Follow **conventional commits** pattern: `feat:`, `fix:`, `docs:`, `chore:`, etc.
- **Never** include `Co-Authored-By` lines or credit Claude as a collaborator in commit messages
