use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::{
    DefaultTerminal,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
};
use std::collections::HashMap;

use super::highlight::highlight_line;
use crate::storage::models::{Annotation, Chunk, ChunkKind};
use crate::storage::Database;

#[derive(PartialEq)]
enum Mode {
    Normal,
    SearchInput,
}

struct ViewApp {
    lines: Vec<Line<'static>>,
    plain_lines: Vec<String>,
    title: String,
    scroll: usize,
    mode: Mode,
    search_input: String,
    matches: Vec<usize>,
    current_match: Option<usize>,
    /// Line indices to highlight (e.g. the chunk that brought us here from search)
    highlight_lines: Vec<usize>,
    /// Whether this view was opened from the search TUI
    from_search: bool,
}

impl ViewApp {
    fn search(&mut self) {
        self.matches.clear();
        self.current_match = None;
        if self.search_input.is_empty() {
            return;
        }
        let query = self.search_input.to_lowercase();
        for (i, line) in self.plain_lines.iter().enumerate() {
            if line.to_lowercase().contains(&query) {
                self.matches.push(i);
            }
        }
        if !self.matches.is_empty() {
            let first = self
                .matches
                .iter()
                .position(|&m| m >= self.scroll)
                .unwrap_or(0);
            self.current_match = Some(first);
            self.scroll = self.matches[first];
        }
    }

    fn next_match(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        let next = match self.current_match {
            Some(i) => (i + 1) % self.matches.len(),
            None => 0,
        };
        self.current_match = Some(next);
        self.scroll = self.matches[next];
    }

    fn prev_match(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        let prev = match self.current_match {
            Some(0) => self.matches.len() - 1,
            Some(i) => i - 1,
            None => 0,
        };
        self.current_match = Some(prev);
        self.scroll = self.matches[prev];
    }

    fn clear_search(&mut self) {
        self.search_input.clear();
        self.matches.clear();
        self.current_match = None;
    }
}

fn build_session_lines(
    chunks: &[Chunk],
    annotations: &[Annotation],
) -> (
    Vec<Line<'static>>,
    Vec<String>,
    HashMap<i64, usize>,
) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut plain_lines: Vec<String> = Vec::new();
    let mut chunk_line_map: HashMap<i64, usize> = HashMap::new();

    if !annotations.is_empty() {
        let note_style = Style::default().fg(Color::Magenta);
        let label_style = Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD);
        let dim_style = Style::default().fg(Color::DarkGray);

        lines.push(Line::styled("── Notes ──", label_style));
        plain_lines.push("── Notes ──".to_string());

        for ann in annotations {
            let time = ann.created_at.format("%Y-%m-%d %H:%M").to_string();
            let text = format!("[{}] {}", time, ann.content);
            plain_lines.push(text);
            lines.push(Line::from(vec![
                Span::styled(
                    format!("[{}] ", time),
                    dim_style,
                ),
                Span::styled(ann.content.clone(), note_style),
            ]));
        }

        lines.push(Line::styled(
            "─".repeat(40),
            dim_style,
        ));
        plain_lines.push("─".repeat(40));
        lines.push(Line::raw(""));
        plain_lines.push(String::new());
    }

    for chunk in chunks {
        let timestamp = chunk.timestamp.format("%H:%M:%S").to_string();
        let style = match chunk.kind {
            ChunkKind::Input => Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            ChunkKind::Output => Style::default().fg(Color::White),
        };
        let prefix_style = Style::default().fg(Color::DarkGray);

        let clean = strip_ansi_escapes::strip_str(&chunk.content);
        let mut first = true;
        for line_text in clean.lines() {
            if line_text.trim().is_empty() {
                continue;
            }
            if first {
                chunk_line_map.insert(chunk.id, lines.len());
                first = false;
            }
            plain_lines.push(format!("[{}] {}", timestamp, line_text));
            let is_input = chunk.kind == ChunkKind::Input;
            let mut spans = vec![Span::styled(format!("[{timestamp}] "), prefix_style)];
            spans.extend(highlight_line(line_text, style, is_input));
            lines.push(Line::from(spans));
        }
    }

    (lines, plain_lines, chunk_line_map)
}

pub fn run(session_id: &str) -> Result<()> {
    let db = Database::open()?;
    let full_id = db.resolve_session_id(session_id)?;
    let chunks = db.get_session_chunks(&full_id)?;

    if chunks.is_empty() {
        println!("No output recorded for session {session_id}.");
        return Ok(());
    }

    let session = db.get_session_by_id(&full_id)?;
    let title = match session.name {
        Some(name) => format!(" broll view — {} ({}) ", name, &full_id[..8]),
        None => format!(" broll view — session {} ", &full_id[..8]),
    };

    let annotations = db.get_annotations(&full_id)?;
    let (lines, plain_lines, _) = build_session_lines(&chunks, &annotations);
    let mut app = ViewApp {
        lines,
        plain_lines,
        title,
        scroll: 0,
        mode: Mode::Normal,
        search_input: String::new(),
        matches: Vec::new(),
        current_match: None,
        highlight_lines: Vec::new(),
        from_search: false,
    };

    let mut terminal = ratatui::init();
    let result = run_loop(&mut terminal, &mut app);
    ratatui::restore();
    result
}

/// Run view mode within an existing terminal (called from search TUI).
pub fn run_in_terminal(
    terminal: &mut DefaultTerminal,
    session_id: &str,
    scroll_to_chunk: Option<i64>,
) -> Result<()> {
    let db = Database::open()?;
    let full_id = db.resolve_session_id(session_id)?;
    let chunks = db.get_session_chunks(&full_id)?;

    if chunks.is_empty() {
        return Ok(());
    }

    let session = db.get_session_by_id(&full_id)?;
    let title = match session.name {
        Some(name) => format!(" broll view — {} ({}) ", name, &full_id[..8]),
        None => format!(" broll view — session {} ", &full_id[..8]),
    };

    let annotations = db.get_annotations(&full_id)?;
    let (lines, plain_lines, chunk_line_map) = build_session_lines(&chunks, &annotations);
    let initial_scroll = scroll_to_chunk
        .and_then(|cid| chunk_line_map.get(&cid).copied())
        .unwrap_or(0);

    // Find all line indices belonging to the target chunk for highlighting
    let highlight_lines = if let Some(cid) = scroll_to_chunk {
        if let Some(&start) = chunk_line_map.get(&cid) {
            // Find next chunk's start line to determine range
            let next_start = chunk_line_map
                .values()
                .filter(|&&v| v > start)
                .min()
                .copied()
                .unwrap_or(lines.len());
            (start..next_start).collect()
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    let mut app = ViewApp {
        lines,
        plain_lines,
        title,
        scroll: initial_scroll,
        mode: Mode::Normal,
        search_input: String::new(),
        matches: Vec::new(),
        current_match: None,
        highlight_lines,
        from_search: true,
    };

    run_loop(terminal, &mut app)
}

fn run_loop(terminal: &mut DefaultTerminal, app: &mut ViewApp) -> Result<()> {
    loop {
        terminal.draw(|frame| {
            let area = frame.area();
            let total_lines = app.lines.len();

            let layout = if app.mode == Mode::SearchInput {
                Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area)
            } else {
                Layout::vertical([Constraint::Min(1)]).split(area)
            };

            let visible_height = layout[0].height.saturating_sub(2) as usize;
            let max_scroll = total_lines.saturating_sub(visible_height);
            app.scroll = app.scroll.min(max_scroll);

            // Build display lines with search/highlight styling
            let has_search_matches = !app.matches.is_empty();
            let has_highlights = !app.highlight_lines.is_empty();
            let display_lines: Vec<Line> = if !has_search_matches && !has_highlights {
                app.lines.clone()
            } else {
                app.lines
                    .iter()
                    .enumerate()
                    .map(|(i, line)| {
                        // Search matches take priority
                        if has_search_matches && app.matches.binary_search(&i).is_ok() {
                            let is_current = app
                                .current_match
                                .map(|m| app.matches[m] == i)
                                .unwrap_or(false);
                            let bg = if is_current {
                                Color::Rgb(180, 140, 0)
                            } else {
                                Color::Rgb(50, 50, 70)
                            };
                            Line::from(
                                line.spans
                                    .iter()
                                    .map(|s| {
                                        Span::styled(s.content.clone(), s.style.bg(bg))
                                    })
                                    .collect::<Vec<_>>(),
                            )
                        } else if has_highlights
                            && app.highlight_lines.binary_search(&i).is_ok()
                        {
                            Line::from(
                                line.spans
                                    .iter()
                                    .map(|s| {
                                        Span::styled(
                                            s.content.clone(),
                                            s.style.bg(Color::Rgb(50, 50, 70)),
                                        )
                                    })
                                    .collect::<Vec<_>>(),
                            )
                        } else {
                            line.clone()
                        }
                    })
                    .collect()
            };

            let match_info = if !app.matches.is_empty() {
                let pos = app.current_match.map(|m| m + 1).unwrap_or(0);
                format!(" [{}/{}]", pos, app.matches.len())
            } else if !app.search_input.is_empty() && app.mode != Mode::SearchInput {
                " [no matches]".to_string()
            } else {
                String::new()
            };

            let exit_hint = if app.from_search {
                "Esc back"
            } else {
                "q quit"
            };

            let bottom_hint = if app.mode == Mode::SearchInput {
                " Enter confirm | Esc cancel ".to_string()
            } else if !app.matches.is_empty() {
                format!(
                    " n/N next/prev | / search | ↑/↓ scroll | {}{}  ",
                    exit_hint, match_info
                )
            } else {
                format!(" / search | ↑/↓ scroll | {} ", exit_hint)
            };

            let block = Block::default()
                .title(app.title.as_str())
                .title_bottom(bottom_hint)
                .borders(Borders::ALL);

            let paragraph = Paragraph::new(display_lines)
                .block(block)
                .scroll((app.scroll as u16, 0));

            frame.render_widget(paragraph, layout[0]);

            // Scrollbar
            if max_scroll > 0 {
                let mut scrollbar_state =
                    ScrollbarState::new(max_scroll).position(app.scroll);
                frame.render_stateful_widget(
                    Scrollbar::new(ScrollbarOrientation::VerticalRight),
                    layout[0],
                    &mut scrollbar_state,
                );
            }

            // Search input bar
            if app.mode == Mode::SearchInput {
                let search_line = Line::from(vec![
                    Span::styled("/", Style::default().fg(Color::Yellow)),
                    Span::raw(app.search_input.as_str()),
                    Span::styled("█", Style::default().fg(Color::Yellow)),
                ]);
                frame.render_widget(Paragraph::new(search_line), layout[1]);
            }
        })?;

        if let Event::Key(key) = event::read()? {
            match app.mode {
                Mode::SearchInput => match key.code {
                    KeyCode::Enter => {
                        app.mode = Mode::Normal;
                        app.search();
                    }
                    KeyCode::Esc => {
                        app.mode = Mode::Normal;
                        app.clear_search();
                    }
                    KeyCode::Backspace => {
                        app.search_input.pop();
                    }
                    KeyCode::Char(c) => {
                        app.search_input.push(c);
                    }
                    _ => {}
                },
                Mode::Normal => match (key.code, key.modifiers) {
                    (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => break,
                    (KeyCode::Char('c'), KeyModifiers::CONTROL) => break,
                    (KeyCode::Char('/'), _) => {
                        app.clear_search();
                        app.mode = Mode::SearchInput;
                    }
                    (KeyCode::Char('n'), KeyModifiers::NONE) => app.next_match(),
                    (KeyCode::Char('N'), _) => app.prev_match(),
                    (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                        app.scroll = app.scroll.saturating_add(1);
                    }
                    (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                        app.scroll = app.scroll.saturating_sub(1);
                    }
                    (KeyCode::PageDown, _) | (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                        app.scroll = app.scroll.saturating_add(20);
                    }
                    (KeyCode::PageUp, _) | (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                        app.scroll = app.scroll.saturating_sub(20);
                    }
                    (KeyCode::Home, _) | (KeyCode::Char('g'), _) => {
                        app.scroll = 0;
                    }
                    (KeyCode::End, _) | (KeyCode::Char('G'), _) => {
                        app.scroll = app.lines.len();
                    }
                    _ => {}
                },
            }
        }
    }

    Ok(())
}
