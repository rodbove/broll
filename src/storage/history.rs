use std::fmt::Write;

use super::models::{Chunk, ChunkKind, Session};
use crate::filter;

pub struct CommandRecord {
    pub command: Chunk,
    pub output: Vec<Chunk>,
}

impl CommandRecord {
    pub fn output_text(&self) -> String {
        self.output
            .iter()
            .map(|c| c.content.trim_end_matches('\n'))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn failed(&self) -> bool {
        self.command.exit_code.is_some_and(|c| c != 0)
    }
}

pub fn group_commands(chunks: Vec<Chunk>, drop_running: bool) -> Vec<CommandRecord> {
    let mut records: Vec<CommandRecord> = Vec::new();
    for chunk in chunks {
        match chunk.kind {
            ChunkKind::Input => records.push(CommandRecord {
                command: chunk,
                output: Vec::new(),
            }),
            ChunkKind::Output => {
                if let Some(last) = records.last_mut() {
                    last.output.push(chunk);
                }
            }
        }
    }
    if drop_running && records.last().is_some_and(|r| r.command.exit_code.is_none()) {
        records.pop();
    }
    records
}

pub fn truncate_middle(text: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if max_lines == 0 || lines.len() <= max_lines {
        return text.to_string();
    }
    let head = max_lines.div_ceil(2);
    let tail = max_lines - head;
    let omitted = lines.len() - head - tail;
    let mut out: Vec<String> = lines[..head].iter().map(|l| l.to_string()).collect();
    out.push(format!("... [{omitted} lines omitted] ..."));
    out.extend(lines[lines.len() - tail..].iter().map(|l| l.to_string()));
    out.join("\n")
}

fn fence_for(body: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in body.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat((longest + 1).max(3))
}

pub fn render_context(session: &Session, records: &[&CommandRecord], max_lines: usize) -> String {
    let label = session
        .name
        .clone()
        .unwrap_or_else(|| session.id.chars().take(8).collect());
    let shell = std::path::Path::new(&session.shell)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&session.shell);

    let mut out = String::new();
    let _ = writeln!(out, "## Terminal context: {label}\n");
    let _ = writeln!(
        out,
        "Shell: {shell} | Commands: {} | Session started: {}\n",
        records.len(),
        session.started_at.format("%Y-%m-%d %H:%M:%S UTC"),
    );

    for (i, record) in records.iter().enumerate() {
        let status = match record.command.exit_code {
            Some(0) => "ok".to_string(),
            Some(code) => format!("exit {code}"),
            None => "exit unknown".to_string(),
        };
        let mut meta = vec![status];
        if let Some(cwd) = &record.command.cwd {
            meta.push(format!("`{cwd}`"));
        }
        meta.push(record.command.timestamp.format("%H:%M:%S").to_string());
        let _ = writeln!(out, "### {}. {}\n", i + 1, meta.join(" | "));

        let mut body = String::new();
        for (n, line) in record.command.content.lines().enumerate() {
            let prefix = if n == 0 { "$ " } else { "> " };
            let _ = writeln!(body, "{prefix}{line}");
        }
        let output = truncate_middle(&record.output_text(), max_lines);
        if !output.trim().is_empty() {
            let _ = writeln!(body, "{output}");
        }
        let body = filter::redact(&body);

        let fence = fence_for(&body);
        let _ = writeln!(out, "{fence}console\n{}{fence}\n", body);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn chunk(id: i64, content: &str, kind: ChunkKind, exit_code: Option<i64>) -> Chunk {
        Chunk {
            id,
            session_id: "s".to_string(),
            timestamp: Utc::now(),
            content: content.to_string(),
            kind,
            cwd: Some("/work".to_string()),
            exit_code,
        }
    }

    fn session() -> Session {
        Session {
            id: "abcdef1234".to_string(),
            name: None,
            started_at: Utc::now(),
            ended_at: None,
            group: None,
            terminal_label: "t".to_string(),
            tags: vec![],
            shell: "/bin/zsh".to_string(),
        }
    }

    #[test]
    fn groups_output_under_preceding_command() {
        let records = group_commands(
            vec![
                chunk(1, "orphan", ChunkKind::Output, None),
                chunk(2, "ls", ChunkKind::Input, Some(0)),
                chunk(3, "a", ChunkKind::Output, None),
                chunk(4, "b", ChunkKind::Output, None),
                chunk(5, "pwd", ChunkKind::Input, Some(0)),
            ],
            false,
        );
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].command.content, "ls");
        assert_eq!(records[0].output_text(), "a\nb");
        assert!(records[1].output.is_empty());
    }

    #[test]
    fn drops_trailing_running_command_only_when_asked() {
        let chunks = || {
            vec![
                chunk(1, "make", ChunkKind::Input, Some(2)),
                chunk(2, "broll last", ChunkKind::Input, None),
            ]
        };
        assert_eq!(group_commands(chunks(), false).len(), 2);
        let records = group_commands(chunks(), true);
        assert_eq!(records.len(), 1);
        assert!(records[0].failed());
    }

    #[test]
    fn truncate_keeps_head_and_tail() {
        let text = (1..=10).map(|n| n.to_string()).collect::<Vec<_>>().join("\n");
        let out = truncate_middle(&text, 4);
        assert_eq!(out, "1\n2\n... [6 lines omitted] ...\n9\n10");
        assert_eq!(truncate_middle(&text, 0), text);
        assert_eq!(truncate_middle(&text, 10), text);
    }

    #[test]
    fn fence_outgrows_backticks_in_body() {
        assert_eq!(fence_for("plain"), "```");
        assert_eq!(fence_for("has ```` four"), "`````");
    }

    #[test]
    fn context_renders_metadata_and_redacts() {
        let mut records = group_commands(
            vec![
                chunk(1, "curl -H 'Authorization: Bearer abcdefghijklmnopqrstuvwxyz123456'", ChunkKind::Input, Some(7)),
                chunk(2, "connection refused", ChunkKind::Output, None),
            ],
            false,
        );
        let record = records.remove(0);
        let md = render_context(&session(), &[&record], 40);
        assert!(md.contains("## Terminal context: abcdef12"));
        assert!(md.contains("### 1. exit 7 | `/work`"));
        assert!(md.contains("connection refused"));
        assert!(!md.contains("abcdefghijklmnopqrstuvwxyz123456"));
    }
}
