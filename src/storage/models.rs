use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub name: Option<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub group: Option<String>,
    pub terminal_label: String,
    pub tags: Vec<String>,
    pub shell: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub id: i64,
    pub session_id: String,
    pub timestamp: DateTime<Utc>,
    pub content: String,
    /// "input" for commands, "output" for terminal output
    pub kind: ChunkKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ChunkKind {
    Input,
    Output,
}

impl ChunkKind {
    pub fn as_str(&self) -> &str {
        match self {
            ChunkKind::Input => "input",
            ChunkKind::Output => "output",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "input" => ChunkKind::Input,
            _ => ChunkKind::Output,
        }
    }
}

/// A search result pointing to a specific chunk with context.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub session: Session,
    pub chunk: Chunk,
}
