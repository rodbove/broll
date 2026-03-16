use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::path::PathBuf;

use super::models::{Annotation, Chunk, ChunkKind, SearchHit, Session, SessionExport};

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

            CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
                content,
                content_rowid='id',
                tokenize='unicode61'
            );

            CREATE INDEX IF NOT EXISTS chunks_session_idx ON chunks(session_id);
            CREATE INDEX IF NOT EXISTS chunks_timestamp_idx ON chunks(timestamp DESC);

            CREATE TRIGGER IF NOT EXISTS chunks_ai AFTER INSERT ON chunks BEGIN
                INSERT INTO chunks_fts(rowid, content) VALUES (new.id, new.content);
            END;

            CREATE TABLE IF NOT EXISTS annotations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id),
                created_at TEXT NOT NULL,
                content TEXT NOT NULL
            );
            ",
        )?;

        // Add name column to existing databases that don't have it yet
        let has_name = self.conn
            .prepare("SELECT name FROM pragma_table_info('sessions') WHERE name = 'name'")
            .and_then(|mut s| s.query_row([], |_| Ok(())))
            .is_ok();
        if !has_name {
            self.conn.execute_batch("ALTER TABLE sessions ADD COLUMN name TEXT")?;
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
                "DELETE FROM chunks_fts WHERE rowid IN (SELECT id FROM chunks WHERE session_id = ?1)",
                params![full_id],
            )?;
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
            anyhow::bail!(
                "Session {} already exists in the database",
                &export.session.id[..8]
            );
        }

        self.create_session(&export.session)?;

        for chunk in &export.chunks {
            self.insert_chunk(chunk)?;
        }

        for ann in &export.annotations {
            self.conn.execute(
                "INSERT INTO annotations (session_id, created_at, content) VALUES (?1, ?2, ?3)",
                params![
                    ann.session_id,
                    ann.created_at.to_rfc3339(),
                    ann.content,
                ],
            )?;
        }

        Ok(())
    }

    pub fn end_session(&self, session_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE sessions SET ended_at = ?1 WHERE id = ?2",
            params![now, session_id],
        )?;
        Ok(())
    }

    pub fn insert_chunk(&self, chunk: &Chunk) -> Result<()> {
        self.conn.execute(
            "INSERT INTO chunks (session_id, timestamp, content, kind)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                chunk.session_id,
                chunk.timestamp.to_rfc3339(),
                chunk.content,
                chunk.kind.as_str(),
            ],
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
            "SELECT id, session_id, timestamp, content, kind
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

    pub fn search(
        &self,
        query: &str,
        group: Option<&str>,
        terminal: Option<&str>,
    ) -> Result<Vec<SearchHit>> {
        let mut hits = Vec::new();

        // Add prefix wildcard to each term so "Document" matches "Documents"
        let fts_query: String = query
            .split_whitespace()
            .map(|word| {
                let clean: String = word.chars().filter(|c| *c != '"' && *c != '*').collect();
                format!("\"{}\"*", clean)
            })
            .collect::<Vec<_>>()
            .join(" ");

        let mut where_clauses = vec!["chunks_fts MATCH ?1".to_string()];
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> =
            vec![Box::new(fts_query)];
        let mut idx = 2;

        if let Some(g) = group {
            where_clauses.push(format!("s.grp = ?{idx}"));
            param_values.push(Box::new(g.to_string()));
            idx += 1;
        }

        if let Some(t) = terminal {
            where_clauses.push(format!("s.terminal_label = ?{idx}"));
            param_values.push(Box::new(t.to_string()));
        }

        let sql = format!(
            "SELECT c.id, c.session_id, c.timestamp, c.content, c.kind,
                    s.id, s.started_at, s.ended_at, s.grp, s.terminal_label, s.tags, s.shell, s.name
             FROM chunks_fts fts
             JOIN chunks c ON c.id = fts.rowid
             JOIN sessions s ON s.id = c.session_id
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
                },
                SessionRow {
                    id: row.get(5)?,
                    started_at: row.get(6)?,
                    ended_at: row.get(7)?,
                    grp: row.get(8)?,
                    terminal_label: row.get(9)?,
                    tags: row.get(10)?,
                    shell: row.get(11)?,
                    name: row.get(12)?,
                },
            ))
        })?;

        for row in rows {
            let (chunk_row, session_row) = row?;
            let chunk = parse_chunk(chunk_row)?;
            let session = parse_session(session_row)?;
            hits.push(SearchHit { session, chunk });
        }

        Ok(hits)
    }

    pub fn resolve_session_id(&self, prefix: &str) -> Result<String> {
        // First try exact name match
        let mut name_stmt = self
            .conn
            .prepare("SELECT id FROM sessions WHERE name = ?1 LIMIT 1")?;
        let name_ids: Vec<String> = name_stmt
            .query_map(params![prefix], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        if name_ids.len() == 1 {
            return Ok(name_ids.into_iter().next().unwrap());
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
}
