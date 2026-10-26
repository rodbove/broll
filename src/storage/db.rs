use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::PathBuf;

use super::models::{
    Annotation, Chunk, ChunkKind, SearchFilter, SearchHit, Session, SessionExport,
};

/// Current database schema version (PRAGMA user_version). Bump when adding a migration step.
const SCHEMA_VERSION: i64 = 3;

const FTS_DDL: &str = "CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
    content,
    content='chunks',
    content_rowid='id',
    tokenize='unicode61'
)";

pub struct Database {
    conn: Connection,
}

impl Database {
    /// Open (or create) the database at the default location.
    pub fn open() -> Result<Self> {
        let path = Self::db_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&path)
            .with_context(|| format!("Failed to open database at {}", path.display()))?;
        // WAL lets a reader (search/view/list) run concurrently with the recorder's
        // writes; busy_timeout retries instead of failing with SQLITE_BUSY on contention.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        // Enforce the declared foreign keys. Enforcement is forward-only: existing
        // rows are not revalidated, so this is safe to enable on older databases.
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    /// Open an in-memory database for testing.
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn db_path() -> Result<PathBuf> {
        let data_dir = dirs::data_dir()
            .context("Could not determine data directory")?
            .join("broll");
        Ok(data_dir.join("broll.db"))
    }

    fn migrate(&self) -> Result<()> {
        // Skip all migration work (including the pragma_table_info probes below)
        // once the schema is known current.
        let version: i64 = self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version >= SCHEMA_VERSION {
            return Ok(());
        }

        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                started_at TEXT NOT NULL,
                ended_at TEXT,
                grp TEXT,
                terminal_label TEXT NOT NULL,
                tags TEXT NOT NULL DEFAULT '[]',
                shell TEXT NOT NULL,
                name TEXT
            );

            CREATE TABLE IF NOT EXISTS chunks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id),
                timestamp TEXT NOT NULL,
                content TEXT NOT NULL,
                kind TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS chunks_session_idx ON chunks(session_id);
            CREATE INDEX IF NOT EXISTS chunks_timestamp_idx ON chunks(timestamp DESC);

            CREATE TABLE IF NOT EXISTS annotations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id),
                created_at TEXT NOT NULL,
                content TEXT NOT NULL
            );
            ",
        )?;

        // v1: add the name column to databases predating it.
        self.add_column_if_missing("sessions", "name", "TEXT")?;
        // v2: per-command metadata captured by the shell hooks.
        self.add_column_if_missing("chunks", "cwd", "TEXT")?;
        self.add_column_if_missing("chunks", "exit_code", "INTEGER")?;
        let rebuilt_fts = self.migrate_fts()?;
        self.conn.execute_batch(
            "
            CREATE TRIGGER IF NOT EXISTS chunks_ai AFTER INSERT ON chunks BEGIN
                INSERT INTO chunks_fts(rowid, content) VALUES (new.id, new.content);
            END;

            CREATE TRIGGER IF NOT EXISTS chunks_ad AFTER DELETE ON chunks BEGIN
                INSERT INTO chunks_fts(chunks_fts, rowid, content)
                VALUES ('delete', old.id, old.content);
            END;

            CREATE TRIGGER IF NOT EXISTS chunks_au AFTER UPDATE OF content ON chunks BEGIN
                INSERT INTO chunks_fts(chunks_fts, rowid, content)
                VALUES ('delete', old.id, old.content);
                INSERT INTO chunks_fts(rowid, content) VALUES (new.id, new.content);
            END;

            CREATE INDEX IF NOT EXISTS chunks_command_idx ON chunks(session_id, kind, id);
            ",
        )?;

        // Mark the schema current so future opens skip this work.
        self.conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;

        if rebuilt_fts {
            self.conn.execute_batch("VACUUM")?;
        }
        Ok(())
    }

    fn migrate_fts(&self) -> Result<bool> {
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'chunks_fts'",
                [],
                |row| row.get(0),
            )
            .optional()?;

        match existing {
            None => {
                self.conn.execute_batch(FTS_DDL)?;
                Ok(false)
            }
            Some(sql) if sql.contains("content='chunks'") => Ok(false),
            Some(_) => {
                self.conn.execute_batch("BEGIN")?;
                let result = (|| -> Result<()> {
                    self.conn.execute_batch("DROP TABLE chunks_fts")?;
                    self.conn.execute_batch(FTS_DDL)?;
                    self.conn
                        .execute_batch("INSERT INTO chunks_fts(chunks_fts) VALUES ('rebuild')")?;
                    Ok(())
                })();
                match result {
                    Ok(()) => {
                        self.conn.execute_batch("COMMIT")?;
                        Ok(true)
                    }
                    Err(e) => {
                        let _ = self.conn.execute_batch("ROLLBACK");
                        Err(e)
                    }
                }
            }
        }
    }

    /// Add a column to a table if it isn't already present (idempotent ALTER).
    fn add_column_if_missing(&self, table: &str, column: &str, decl: &str) -> Result<()> {
        let exists = self
            .conn
            .prepare(&format!("SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1"))?
            .exists(params![column])?;
        if !exists {
            self.conn
                .execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
        }
        Ok(())
    }

    pub fn create_session(&self, session: &Session) -> Result<()> {
        let tags_json = serde_json::to_string(&session.tags)?;
        self.conn.execute(
            "INSERT INTO sessions (id, started_at, ended_at, grp, terminal_label, tags, shell, name)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                session.id,
                session.started_at.to_rfc3339(),
                session.ended_at.map(|t| t.to_rfc3339()),
                session.group,
                session.terminal_label,
                tags_json,
                session.shell,
                session.name,
            ],
        )?;
        Ok(())
    }

    pub fn rename_session(&self, prefix: &str, new_name: &str) -> Result<String> {
        let full_id = self.resolve_session_id(prefix)?;
        self.conn.execute(
            "UPDATE sessions SET name = ?1 WHERE id = ?2",
            params![new_name, full_id],
        )?;
        Ok(full_id)
    }

    pub fn delete_session(&self, prefix: &str) -> Result<(String, Option<String>)> {
        let full_id = self.resolve_session_id(prefix)?;
        let session = self.get_session_by_id(&full_id)?;

        self.conn.execute_batch("BEGIN")?;
        let result = (|| -> Result<()> {
            self.conn.execute(
                "DELETE FROM annotations WHERE session_id = ?1",
                params![full_id],
            )?;
            self.conn.execute(
                "DELETE FROM chunks WHERE session_id = ?1",
                params![full_id],
            )?;
            self.conn.execute(
                "DELETE FROM sessions WHERE id = ?1",
                params![full_id],
            )?;
            Ok(())
        })();

        match result {
            Ok(()) => {
                self.conn.execute_batch("COMMIT")?;
                Ok((full_id, session.name))
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    pub fn add_annotation(&self, prefix: &str, content: &str) -> Result<String> {
        let full_id = self.resolve_session_id(prefix)?;
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO annotations (session_id, created_at, content) VALUES (?1, ?2, ?3)",
            params![full_id, now, content],
        )?;
        Ok(full_id)
    }

    pub fn get_annotations(&self, session_id: &str) -> Result<Vec<Annotation>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, created_at, content
             FROM annotations WHERE session_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;

        let mut annotations = Vec::new();
        for row in rows {
            let (id, session_id, created_at, content) = row?;
            annotations.push(Annotation {
                id,
                session_id,
                created_at: parse_dt(&created_at)?,
                content,
            });
        }
        Ok(annotations)
    }

    pub fn export_session(&self, prefix: &str) -> Result<SessionExport> {
        let full_id = self.resolve_session_id(prefix)?;
        let session = self.get_session_by_id(&full_id)?;
        let chunks = self.get_session_chunks(&full_id)?;
        let annotations = self.get_annotations(&full_id)?;

        Ok(SessionExport {
            version: 1,
            session,
            chunks,
            annotations,
        })
    }

    pub fn import_session(&self, export: &SessionExport) -> Result<()> {
        // Check if session already exists
        let exists = self
            .conn
            .prepare("SELECT 1 FROM sessions WHERE id = ?1")?
            .exists(params![export.session.id])?;
        if exists {
            // id comes from an external file, so slice safely on a char boundary.
            let short = export.session.id.get(..8).unwrap_or(&export.session.id);
            anyhow::bail!("Session {} already exists in the database", short);
        }

        // Import the session, its chunks, and annotations atomically so a failure
        // partway through doesn't leave a half-imported session behind.
        self.conn.execute_batch("BEGIN")?;
        let result = (|| -> Result<()> {
            self.create_session(&export.session)?;
            for chunk in &export.chunks {
                self.insert_chunk(chunk)?;
            }
            for ann in &export.annotations {
                self.conn.execute(
                    "INSERT INTO annotations (session_id, created_at, content) VALUES (?1, ?2, ?3)",
                    params![ann.session_id, ann.created_at.to_rfc3339(), ann.content],
                )?;
            }
            Ok(())
        })();

        match result {
            Ok(()) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(())
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    pub fn end_session(&self, session_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE sessions SET ended_at = ?1 WHERE id = ?2",
            params![now, session_id],
        )?;
        Ok(())
    }

    /// Insert a chunk and return its row id (used to later attach an exit code).
    pub fn insert_chunk(&self, chunk: &Chunk) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO chunks (session_id, timestamp, content, kind, cwd, exit_code)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                chunk.session_id,
                chunk.timestamp.to_rfc3339(),
                chunk.content,
                chunk.kind.as_str(),
                chunk.cwd,
                chunk.exit_code,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Record the exit code of a previously inserted command (input) chunk.
    pub fn set_chunk_exit_code(&self, chunk_id: i64, exit_code: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE chunks SET exit_code = ?1 WHERE id = ?2",
            params![exit_code, chunk_id],
        )?;
        Ok(())
    }

    pub fn list_sessions(&self, group: Option<&str>) -> Result<Vec<Session>> {
        let mut sessions = Vec::new();
        let (query, param_values): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = match group {
            Some(g) => (
                "SELECT id, started_at, ended_at, grp, terminal_label, tags, shell, name
                 FROM sessions WHERE grp = ?1 ORDER BY started_at DESC",
                vec![Box::new(g.to_string())],
            ),
            None => (
                "SELECT id, started_at, ended_at, grp, terminal_label, tags, shell, name
                 FROM sessions ORDER BY started_at DESC",
                vec![],
            ),
        };

        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self.conn.prepare(query)?;
        let rows = stmt.query_map(params_ref.as_slice(), |row| {
            Ok(SessionRow {
                id: row.get(0)?,
                started_at: row.get(1)?,
                ended_at: row.get(2)?,
                grp: row.get(3)?,
                terminal_label: row.get(4)?,
                tags: row.get(5)?,
                shell: row.get(6)?,
                name: row.get(7)?,
            })
        })?;

        for row in rows {
            sessions.push(parse_session(row?)?);
        }

        Ok(sessions)
    }

    pub fn get_session_chunks(&self, session_id: &str) -> Result<Vec<Chunk>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, timestamp, content, kind, cwd, exit_code
             FROM chunks WHERE session_id = ?1 ORDER BY id ASC",
        )?;

        let mut chunks = Vec::new();
        let rows = stmt
            .query_map(params![session_id], |row| {
                Ok(ChunkRow {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    timestamp: row.get(2)?,
                    content: row.get(3)?,
                    kind: row.get(4)?,
                    cwd: row.get(5)?,
                    exit_code: row.get(6)?,
                })
            })?;

        for row in rows {
            chunks.push(parse_chunk(row?)?);
        }

        Ok(chunks)
    }

    pub fn get_commands(&self, session_id: &str) -> Result<Vec<String>> {
        // Resolve prefix match
        let full_id = self.resolve_session_id(session_id)?;

        let mut stmt = self.conn.prepare(
            "SELECT content FROM chunks
             WHERE session_id = ?1 AND kind = 'input'
             ORDER BY id ASC",
        )?;

        let commands: Vec<String> = stmt
            .query_map(params![full_id], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(commands)
    }

    pub fn search(&self, query: &str, filter: &SearchFilter) -> Result<Vec<SearchHit>> {
        let mut hits = Vec::new();

        // Add prefix wildcard to each term so "Document" matches "Documents".
        // Drop terms that are empty after cleaning so we never build a degenerate
        // FTS expression (e.g. `MATCH ''` or `""*`), which SQLite rejects as an error.
        let fts_query: String = query
            .split_whitespace()
            .filter_map(|word| {
                let clean: String = word.chars().filter(|c| *c != '"' && *c != '*').collect();
                (!clean.is_empty()).then(|| format!("\"{clean}\"*"))
            })
            .collect::<Vec<_>>()
            .join(" ");

        let use_fts = !fts_query.is_empty();
        if !use_fts && !filter.has_metadata_filters() {
            return Ok(hits);
        }

        let mut where_clauses: Vec<String> = Vec::new();
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        let from = if use_fts {
            param_values.push(Box::new(fts_query));
            where_clauses.push("chunks_fts MATCH ?1".to_string());
            "chunks_fts fts JOIN chunks c ON c.id = fts.rowid"
        } else {
            where_clauses.push("c.kind = 'input'".to_string());
            "chunks c"
        };

        if let Some(g) = &filter.group {
            param_values.push(Box::new(g.clone()));
            where_clauses.push(format!("s.grp = ?{}", param_values.len()));
        }

        if let Some(t) = &filter.terminal {
            param_values.push(Box::new(t.clone()));
            where_clauses.push(format!("s.terminal_label = ?{}", param_values.len()));
        }

        if filter.failed {
            where_clauses.push("cmd.exit_code IS NOT NULL AND cmd.exit_code <> 0".to_string());
        }

        if let Some(dir) = filter.cwd.as_deref().map(|d| d.trim_end_matches('/'))
            && !dir.is_empty()
        {
            param_values.push(Box::new(dir.to_string()));
            let n = param_values.len();
            where_clauses.push(format!(
                "(cmd.cwd = ?{n} OR substr(cmd.cwd, 1, length(?{n}) + 1) = ?{n} || '/')"
            ));
        }

        if let Some(since) = filter.since {
            param_values.push(Box::new(since.to_rfc3339()));
            where_clauses.push(format!("c.timestamp >= ?{}", param_values.len()));
        }

        let sql = format!(
            "SELECT c.id, c.session_id, c.timestamp, c.content, c.kind, c.cwd, c.exit_code,
                    s.id, s.started_at, s.ended_at, s.grp, s.terminal_label, s.tags, s.shell, s.name,
                    cmd.cwd, cmd.exit_code
             FROM {from}
             JOIN sessions s ON s.id = c.session_id
             LEFT JOIN chunks cmd ON cmd.id = (
                 SELECT MAX(i.id) FROM chunks i
                 WHERE i.session_id = c.session_id AND i.kind = 'input' AND i.id <= c.id
             )
             WHERE {}
             ORDER BY c.timestamp DESC
             LIMIT 100",
            where_clauses.join(" AND ")
        );

        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_ref.as_slice(), |row| {
            Ok((
                ChunkRow {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    timestamp: row.get(2)?,
                    content: row.get(3)?,
                    kind: row.get(4)?,
                    cwd: row.get(5)?,
                    exit_code: row.get(6)?,
                },
                SessionRow {
                    id: row.get(7)?,
                    started_at: row.get(8)?,
                    ended_at: row.get(9)?,
                    grp: row.get(10)?,
                    terminal_label: row.get(11)?,
                    tags: row.get(12)?,
                    shell: row.get(13)?,
                    name: row.get(14)?,
                },
                row.get::<_, Option<String>>(15)?,
                row.get::<_, Option<i64>>(16)?,
            ))
        })?;

        for row in rows {
            let (chunk_row, session_row, cwd, exit_code) = row?;
            let chunk = parse_chunk(chunk_row)?;
            let session = parse_session(session_row)?;
            hits.push(SearchHit {
                session,
                chunk,
                cwd,
                exit_code,
            });
        }

        Ok(hits)
    }

    pub fn latest_session_id(&self) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM sessions ORDER BY started_at DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn resolve_session_id(&self, prefix: &str) -> Result<String> {
        // First try exact name match. Names aren't unique, so fetch up to 2 to
        // detect collisions instead of silently returning an arbitrary match.
        let mut name_stmt = self
            .conn
            .prepare("SELECT id FROM sessions WHERE name = ?1 LIMIT 2")?;
        let name_ids: Vec<String> = name_stmt
            .query_map(params![prefix], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        match name_ids.len() {
            1 => return Ok(name_ids.into_iter().next().unwrap()),
            n if n > 1 => {
                anyhow::bail!("Ambiguous session name '{prefix}', use the session id instead")
            }
            _ => {} // no name match: fall back to id-prefix matching
        }

        // Fall back to ID prefix match
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM sessions WHERE id LIKE ?1 LIMIT 2")?;
        let pattern = format!("{prefix}%");
        let ids: Vec<String> = stmt
            .query_map(params![pattern], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();

        match ids.len() {
            0 => anyhow::bail!("No session found matching '{prefix}'"),
            1 => Ok(ids.into_iter().next().unwrap()),
            _ => anyhow::bail!("Ambiguous session prefix '{prefix}', be more specific"),
        }
    }

    pub fn get_session_by_id(&self, id: &str) -> Result<Session> {
        let row = self.conn.query_row(
            "SELECT id, started_at, ended_at, grp, terminal_label, tags, shell, name
             FROM sessions WHERE id = ?1",
            params![id],
            |row| {
                Ok(SessionRow {
                    id: row.get(0)?,
                    started_at: row.get(1)?,
                    ended_at: row.get(2)?,
                    grp: row.get(3)?,
                    terminal_label: row.get(4)?,
                    tags: row.get(5)?,
                    shell: row.get(6)?,
                    name: row.get(7)?,
                })
            },
        )?;

        parse_session(row)
    }

    pub fn stats(&self) -> Result<Stats> {
        let session_count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))?;
        let chunk_count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |row| row.get(0))?;
        let input_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM chunks WHERE kind = 'input'",
            [],
            |row| row.get(0),
        )?;
        let annotation_count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM annotations", [], |row| row.get(0))?;

        let oldest_session: Option<String> = self
            .conn
            .query_row(
                "SELECT MIN(started_at) FROM sessions",
                [],
                |row| row.get(0),
            )?;
        let newest_session: Option<String> = self
            .conn
            .query_row(
                "SELECT MAX(started_at) FROM sessions",
                [],
                |row| row.get(0),
            )?;

        let db_size = Self::db_path()
            .ok()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .unwrap_or(0);

        Ok(Stats {
            session_count: session_count as u64,
            chunk_count: chunk_count as u64,
            command_count: input_count as u64,
            annotation_count: annotation_count as u64,
            oldest_session: oldest_session.and_then(|s| parse_dt(&s).ok()),
            newest_session: newest_session.and_then(|s| parse_dt(&s).ok()),
            db_size_bytes: db_size,
        })
    }
}

pub struct Stats {
    pub session_count: u64,
    pub chunk_count: u64,
    pub command_count: u64,
    pub annotation_count: u64,
    pub oldest_session: Option<DateTime<Utc>>,
    pub newest_session: Option<DateTime<Utc>>,
    pub db_size_bytes: u64,
}

fn parse_dt(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .with_context(|| format!("Invalid timestamp in database: '{s}'"))
}

fn parse_dt_opt(s: &Option<String>) -> Result<Option<DateTime<Utc>>> {
    match s {
        Some(s) => Ok(Some(parse_dt(s)?)),
        None => Ok(None),
    }
}

fn parse_session(row: SessionRow) -> Result<Session> {
    Ok(Session {
        started_at: parse_dt(&row.started_at)?,
        ended_at: parse_dt_opt(&row.ended_at)?,
        tags: serde_json::from_str(&row.tags)
            .with_context(|| format!("Invalid tags JSON in session '{}'", row.id))?,
        id: row.id,
        name: row.name,
        group: row.grp,
        terminal_label: row.terminal_label,
        shell: row.shell,
    })
}

fn parse_chunk(row: ChunkRow) -> Result<Chunk> {
    Ok(Chunk {
        timestamp: parse_dt(&row.timestamp)?,
        id: row.id,
        session_id: row.session_id,
        content: row.content,
        kind: ChunkKind::from_str(&row.kind),
        cwd: row.cwd,
        exit_code: row.exit_code,
    })
}

// Internal row types for SQLite mapping
struct SessionRow {
    id: String,
    started_at: String,
    ended_at: Option<String>,
    grp: Option<String>,
    terminal_label: String,
    tags: String,
    shell: String,
    name: Option<String>,
}

struct ChunkRow {
    id: i64,
    session_id: String,
    timestamp: String,
    content: String,
    kind: String,
    cwd: Option<String>,
    exit_code: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::models::{Chunk, ChunkKind, Session};
    use chrono::Utc;

    fn make_session(id: &str, name: Option<&str>) -> Session {
        Session {
            id: id.to_string(),
            name: name.map(|s| s.to_string()),
            started_at: Utc::now(),
            ended_at: None,
            group: None,
            terminal_label: "term-test".to_string(),
            tags: vec![],
            shell: "/bin/zsh".to_string(),
        }
    }

    fn make_chunk(session_id: &str, content: &str, kind: ChunkKind) -> Chunk {
        Chunk {
            id: 0,
            session_id: session_id.to_string(),
            timestamp: Utc::now(),
            content: content.to_string(),
            kind,
            cwd: None,
            exit_code: None,
        }
    }

    // --- resolve_session_id ---

    #[test]
    fn resolve_by_exact_name() {
        let db = Database::open_in_memory().unwrap();
        let session = make_session("aaaa-bbbb-cccc-dddd", Some("my-session"));
        db.create_session(&session).unwrap();

        let resolved = db.resolve_session_id("my-session").unwrap();
        assert_eq!(resolved, "aaaa-bbbb-cccc-dddd");
    }

    #[test]
    fn resolve_by_id_prefix() {
        let db = Database::open_in_memory().unwrap();
        let session = make_session("abcdef12-3456-7890-abcd-ef1234567890", None);
        db.create_session(&session).unwrap();

        let resolved = db.resolve_session_id("abcdef12").unwrap();
        assert_eq!(resolved, "abcdef12-3456-7890-abcd-ef1234567890");
    }

    #[test]
    fn resolve_ambiguous_prefix_fails() {
        let db = Database::open_in_memory().unwrap();
        db.create_session(&make_session("abc-1111", None)).unwrap();
        db.create_session(&make_session("abc-2222", None)).unwrap();

        let result = db.resolve_session_id("abc");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Ambiguous"));
    }

    #[test]
    fn import_short_id_does_not_panic() {
        use crate::storage::models::SessionExport;
        // An id shorter than 8 bytes would panic the old `id[..8]` slice on the
        // duplicate-detection path.
        let export = SessionExport {
            version: 1,
            session: make_session("ab", Some("short")),
            chunks: vec![],
            annotations: vec![],
        };
        let db = Database::open_in_memory().unwrap();
        db.import_session(&export).unwrap();
        // Re-importing must error gracefully, not panic.
        let result = db.import_session(&export);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    #[test]
    fn resolve_ambiguous_name_fails() {
        let db = Database::open_in_memory().unwrap();
        db.create_session(&make_session("id-1111", Some("dup"))).unwrap();
        db.create_session(&make_session("id-2222", Some("dup"))).unwrap();

        let result = db.resolve_session_id("dup");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Ambiguous"));
    }

    #[test]
    fn resolve_no_match_fails() {
        let db = Database::open_in_memory().unwrap();
        let result = db.resolve_session_id("nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No session found"));
    }

    // --- search ---

    #[test]
    fn search_returns_matching_chunks() {
        let db = Database::open_in_memory().unwrap();
        let session = make_session("sess-0001", Some("test"));
        db.create_session(&session).unwrap();
        db.insert_chunk(&make_chunk("sess-0001", "hello world", ChunkKind::Output))
            .unwrap();
        db.insert_chunk(&make_chunk("sess-0001", "goodbye moon", ChunkKind::Output))
            .unwrap();

        let hits = db.search("hello", &SearchFilter::default()).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].chunk.content.contains("hello"));
        assert_eq!(hits[0].session.id, "sess-0001");
    }

    #[test]
    fn search_no_results() {
        let db = Database::open_in_memory().unwrap();
        let session = make_session("sess-0002", None);
        db.create_session(&session).unwrap();
        db.insert_chunk(&make_chunk("sess-0002", "some output", ChunkKind::Output))
            .unwrap();

        let hits = db.search("nonexistent", &SearchFilter::default()).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn chunk_cwd_and_exit_code_roundtrip() {
        let db = Database::open_in_memory().unwrap();
        db.create_session(&make_session("sess-meta", None)).unwrap();

        let mut cmd = make_chunk("sess-meta", "ls -la", ChunkKind::Input);
        cmd.cwd = Some("/home/user/project".to_string());
        let id = db.insert_chunk(&cmd).unwrap();
        // Exit code arrives after the command finishes.
        db.set_chunk_exit_code(id, 2).unwrap();

        let chunks = db.get_session_chunks("sess-meta").unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].cwd.as_deref(), Some("/home/user/project"));
        assert_eq!(chunks[0].exit_code, Some(2));
    }

    #[test]
    fn search_hits_serialize_to_json() {
        let db = Database::open_in_memory().unwrap();
        db.create_session(&make_session("sess-json", Some("jtest"))).unwrap();
        db.insert_chunk(&make_chunk("sess-json", "needle here", ChunkKind::Output))
            .unwrap();

        let hits = db.search("needle", &SearchFilter::default()).unwrap();
        let json = serde_json::to_string(&hits).unwrap();
        // Lock the JSON shape used by `broll search --format json`.
        assert!(json.contains("\"session\""));
        assert!(json.contains("\"chunk\""));
        assert!(json.contains("\"id\":\"sess-json\""));
        assert!(json.contains("needle here"));
    }

    #[test]
    fn search_empty_or_degenerate_query_returns_empty() {
        let db = Database::open_in_memory().unwrap();
        db.create_session(&make_session("sess-0003", None)).unwrap();
        db.insert_chunk(&make_chunk("sess-0003", "some output", ChunkKind::Output))
            .unwrap();

        // Empty, whitespace-only, and quote/star-only queries must not error.
        for q in ["", "   ", "\"", "*", " \" * "] {
            let hits = db.search(q, &SearchFilter::default()).unwrap();
            assert!(hits.is_empty(), "query {q:?} should return no hits");
        }
    }

    #[test]
    fn search_filters_by_group() {
        let db = Database::open_in_memory().unwrap();
        let mut s1 = make_session("sess-g1", None);
        s1.group = Some("alpha".to_string());
        let mut s2 = make_session("sess-g2", None);
        s2.group = Some("beta".to_string());
        db.create_session(&s1).unwrap();
        db.create_session(&s2).unwrap();
        db.insert_chunk(&make_chunk("sess-g1", "error found", ChunkKind::Output))
            .unwrap();
        db.insert_chunk(&make_chunk("sess-g2", "error found", ChunkKind::Output))
            .unwrap();

        let filter = SearchFilter {
            group: Some("alpha".to_string()),
            ..Default::default()
        };
        let hits = db.search("error", &filter).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session.id, "sess-g1");
    }

    // --- delete_session (transactional) ---

    #[test]
    fn delete_removes_all_related_data() {
        let db = Database::open_in_memory().unwrap();
        let session = make_session("sess-del", Some("deleteme"));
        db.create_session(&session).unwrap();
        db.insert_chunk(&make_chunk("sess-del", "output1", ChunkKind::Output))
            .unwrap();
        db.insert_chunk(&make_chunk("sess-del", "output2", ChunkKind::Output))
            .unwrap();
        db.add_annotation("deleteme", "a note").unwrap();

        let (full_id, name) = db.delete_session("deleteme").unwrap();
        assert_eq!(full_id, "sess-del");
        assert_eq!(name.as_deref(), Some("deleteme"));

        // Session gone
        assert!(db.get_session_by_id("sess-del").is_err());
        // Chunks gone
        let chunks = db.get_session_chunks("sess-del").unwrap();
        assert!(chunks.is_empty());
        // Annotations gone
        let annotations = db.get_annotations("sess-del").unwrap();
        assert!(annotations.is_empty());
    }

    // --- import/export roundtrip ---

    #[test]
    fn export_import_roundtrip() {
        let db = Database::open_in_memory().unwrap();
        let session = make_session("sess-exp", Some("exported"));
        db.create_session(&session).unwrap();
        db.insert_chunk(&make_chunk("sess-exp", "line one", ChunkKind::Input))
            .unwrap();
        db.insert_chunk(&make_chunk("sess-exp", "line two output", ChunkKind::Output))
            .unwrap();
        db.add_annotation("exported", "test note").unwrap();

        let export = db.export_session("exported").unwrap();
        assert_eq!(export.session.id, "sess-exp");
        assert_eq!(export.chunks.len(), 2);
        assert_eq!(export.annotations.len(), 1);

        // Import into a fresh database
        let db2 = Database::open_in_memory().unwrap();
        db2.import_session(&export).unwrap();

        let imported = db2.get_session_by_id("sess-exp").unwrap();
        assert_eq!(imported.name.as_deref(), Some("exported"));
        let chunks = db2.get_session_chunks("sess-exp").unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].kind, ChunkKind::Input);
        assert_eq!(chunks[1].kind, ChunkKind::Output);
    }

    #[test]
    fn import_duplicate_fails() {
        let db = Database::open_in_memory().unwrap();
        let session = make_session("sess-dup", None);
        db.create_session(&session).unwrap();

        let export = SessionExport {
            version: 1,
            session: make_session("sess-dup", None),
            chunks: vec![],
            annotations: vec![],
        };

        let result = db.import_session(&export);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    // --- rename / annotate ---

    #[test]
    fn rename_session() {
        let db = Database::open_in_memory().unwrap();
        db.create_session(&make_session("sess-ren", Some("old-name")))
            .unwrap();

        db.rename_session("old-name", "new-name").unwrap();
        let session = db.get_session_by_id("sess-ren").unwrap();
        assert_eq!(session.name.as_deref(), Some("new-name"));
    }

    #[test]
    fn end_session_sets_ended_at() {
        let db = Database::open_in_memory().unwrap();
        let session = make_session("sess-end", None);
        db.create_session(&session).unwrap();
        assert!(db.get_session_by_id("sess-end").unwrap().ended_at.is_none());

        db.end_session("sess-end").unwrap();
        assert!(db.get_session_by_id("sess-end").unwrap().ended_at.is_some());
    }

    // --- list_sessions ---

    #[test]
    fn list_sessions_returns_all() {
        let db = Database::open_in_memory().unwrap();
        db.create_session(&make_session("sess-l1", None)).unwrap();
        db.create_session(&make_session("sess-l2", None)).unwrap();

        let sessions = db.list_sessions(None).unwrap();
        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn list_sessions_filters_by_group() {
        let db = Database::open_in_memory().unwrap();
        let mut s1 = make_session("sess-lg1", None);
        s1.group = Some("grp-a".to_string());
        let s2 = make_session("sess-lg2", None);
        db.create_session(&s1).unwrap();
        db.create_session(&s2).unwrap();

        let sessions = db.list_sessions(Some("grp-a")).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "sess-lg1");
    }

    // --- get_commands ---

    #[test]
    fn get_commands_returns_only_input_chunks() {
        let db = Database::open_in_memory().unwrap();
        db.create_session(&make_session("sess-cmd", None)).unwrap();
        db.insert_chunk(&make_chunk("sess-cmd", "ls -la", ChunkKind::Input))
            .unwrap();
        db.insert_chunk(&make_chunk("sess-cmd", "file listing...", ChunkKind::Output))
            .unwrap();
        db.insert_chunk(&make_chunk("sess-cmd", "pwd", ChunkKind::Input))
            .unwrap();

        let commands = db.get_commands("sess-cmd").unwrap();
        assert_eq!(commands, vec!["ls -la", "pwd"]);
    }

    fn insert_command(
        db: &Database,
        session_id: &str,
        cmd: &str,
        cwd: &str,
        exit: Option<i64>,
        output: &str,
    ) -> i64 {
        let mut input = make_chunk(session_id, cmd, ChunkKind::Input);
        input.cwd = Some(cwd.to_string());
        let id = db.insert_chunk(&input).unwrap();
        db.insert_chunk(&make_chunk(session_id, output, ChunkKind::Output))
            .unwrap();
        if let Some(code) = exit {
            db.set_chunk_exit_code(id, code).unwrap();
        }
        id
    }

    #[test]
    fn search_hit_carries_owning_command_metadata() {
        let db = Database::open_in_memory().unwrap();
        db.create_session(&make_session("sess-own", None)).unwrap();
        insert_command(&db, "sess-own", "cargo build", "/proj", Some(101), "boom happened");

        let hits = db.search("boom", &SearchFilter::default()).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.kind, ChunkKind::Output);
        assert_eq!(hits[0].cwd.as_deref(), Some("/proj"));
        assert_eq!(hits[0].exit_code, Some(101));
    }

    #[test]
    fn search_failed_filter_matches_output_of_failed_commands() {
        let db = Database::open_in_memory().unwrap();
        db.create_session(&make_session("sess-fail", None)).unwrap();
        insert_command(&db, "sess-fail", "make ok", "/p", Some(0), "result alpha");
        insert_command(&db, "sess-fail", "make bad", "/p", Some(2), "result beta");
        insert_command(&db, "sess-fail", "make running", "/p", None, "result gamma");

        let filter = SearchFilter {
            failed: true,
            ..Default::default()
        };
        let hits = db.search("result", &filter).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].chunk.content.contains("beta"));
    }

    #[test]
    fn search_cwd_filter_matches_directory_and_children_only() {
        let db = Database::open_in_memory().unwrap();
        db.create_session(&make_session("sess-cwd", None)).unwrap();
        insert_command(&db, "sess-cwd", "ls", "/home/u/app", Some(0), "hit root");
        insert_command(&db, "sess-cwd", "ls", "/home/u/app/src", Some(0), "hit child");
        insert_command(&db, "sess-cwd", "ls", "/home/u/application", Some(0), "hit sibling");

        let filter = SearchFilter {
            cwd: Some("/home/u/app/".to_string()),
            ..Default::default()
        };
        let mut found: Vec<String> = db
            .search("hit", &filter)
            .unwrap()
            .into_iter()
            .map(|h| h.chunk.content)
            .collect();
        found.sort();
        assert_eq!(found, vec!["hit child", "hit root"]);
    }

    #[test]
    fn search_since_filter_excludes_older_chunks() {
        let db = Database::open_in_memory().unwrap();
        db.create_session(&make_session("sess-since", None)).unwrap();
        let mut old = make_chunk("sess-since", "stale entry", ChunkKind::Output);
        old.timestamp = Utc::now() - chrono::TimeDelta::days(3);
        db.insert_chunk(&old).unwrap();
        db.insert_chunk(&make_chunk("sess-since", "fresh entry", ChunkKind::Output))
            .unwrap();

        let filter = SearchFilter {
            since: Some(Utc::now() - chrono::TimeDelta::days(1)),
            ..Default::default()
        };
        let hits = db.search("entry", &filter).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.content, "fresh entry");
    }

    #[test]
    fn search_without_query_lists_commands_matching_filters() {
        let db = Database::open_in_memory().unwrap();
        db.create_session(&make_session("sess-noq", None)).unwrap();
        insert_command(&db, "sess-noq", "true", "/p", Some(0), "fine");
        insert_command(&db, "sess-noq", "false", "/p", Some(1), "nope");

        let filter = SearchFilter {
            failed: true,
            ..Default::default()
        };
        let hits = db.search("", &filter).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.kind, ChunkKind::Input);
        assert_eq!(hits[0].chunk.content, "false");
        assert_eq!(hits[0].exit_code, Some(1));
    }

    fn fts_integrity_ok(db: &Database) -> bool {
        db.conn
            .execute_batch("INSERT INTO chunks_fts(chunks_fts) VALUES ('integrity-check')")
            .is_ok()
    }

    #[test]
    fn delete_keeps_fts_index_consistent() {
        let db = Database::open_in_memory().unwrap();
        db.create_session(&make_session("sess-fts1", None)).unwrap();
        db.create_session(&make_session("sess-fts2", None)).unwrap();
        db.insert_chunk(&make_chunk("sess-fts1", "shared token", ChunkKind::Output))
            .unwrap();
        db.insert_chunk(&make_chunk("sess-fts2", "shared token", ChunkKind::Output))
            .unwrap();

        db.delete_session("sess-fts1").unwrap();

        let hits = db.search("shared", &SearchFilter::default()).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session.id, "sess-fts2");
        assert!(fts_integrity_ok(&db));
    }

    #[test]
    fn migrates_legacy_fts_table_to_external_content() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE sessions (
                id TEXT PRIMARY KEY, started_at TEXT NOT NULL, ended_at TEXT, grp TEXT,
                terminal_label TEXT NOT NULL, tags TEXT NOT NULL DEFAULT '[]',
                shell TEXT NOT NULL, name TEXT
            );
            CREATE TABLE chunks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id),
                timestamp TEXT NOT NULL, content TEXT NOT NULL, kind TEXT NOT NULL,
                cwd TEXT, exit_code INTEGER
            );
            CREATE VIRTUAL TABLE chunks_fts USING fts5(
                content, content_rowid='id', tokenize='unicode61'
            );
            CREATE TRIGGER chunks_ai AFTER INSERT ON chunks BEGIN
                INSERT INTO chunks_fts(rowid, content) VALUES (new.id, new.content);
            END;
            INSERT INTO sessions (id, started_at, terminal_label, shell)
                VALUES ('legacy-1', '2026-01-01T00:00:00+00:00', 't', '/bin/zsh');
            INSERT INTO chunks (session_id, timestamp, content, kind)
                VALUES ('legacy-1', '2026-01-01T00:00:01+00:00', 'legacy needle', 'output');
            PRAGMA user_version = 2;
            ",
        )
        .unwrap();
        let db = Database { conn };
        db.migrate().unwrap();

        let sql: String = db
            .conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = 'chunks_fts'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(sql.contains("content='chunks'"));

        let hits = db.search("needle", &SearchFilter::default()).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session.id, "legacy-1");
        assert!(fts_integrity_ok(&db));
    }

    #[test]
    fn latest_session_id_picks_most_recent() {
        let db = Database::open_in_memory().unwrap();
        assert!(db.latest_session_id().unwrap().is_none());

        let mut older = make_session("sess-old", None);
        older.started_at = Utc::now() - chrono::TimeDelta::hours(1);
        db.create_session(&older).unwrap();
        db.create_session(&make_session("sess-new", None)).unwrap();

        assert_eq!(db.latest_session_id().unwrap().as_deref(), Some("sess-new"));
    }
}
