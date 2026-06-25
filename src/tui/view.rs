use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseEventKind};
use ratatui::{
    DefaultTerminal,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
};
use std::collections::HashMap;
use std::time::Instant;

use super::clipboard;
use super::highlight::highlight_line;
use crate::storage::models::{Annotation, Chunk, ChunkKind};
use crate::storage::Database;

#[derive(PartialEq)]
enum Mode {
    Normal,
    SearchInput,
    Visual,
}

struct ViewApp {
    lines: Vec<Line<'static>>,
    title: String,
    scroll: usize,
    cursor: usize,
    visible_height: usize,
    mode: Mode,
    search_input: String,
    matches: Vec<usize>,
    current_match: Option<usize>,
    /// Line indices to highlight (e.g. the chunk that brought us here from search)
    highlight_lines: Vec<usize>,
    /// Whether this view was opened from the search TUI
    from_search: bool,
    /// Visual mode anchor line
    visual_anchor: usize,
    /// Pending 'y' keypress for yy combo
    pending_y: bool,
    /// Count prefix for yank (e.g. 3yy)
    count_prefix: Option<usize>,
    /// Status message shown temporarily
    status_message: Option<(String, Instant)>,
}

impl ViewApp {
    /// Extract plain text from a styled Line by concatenating span contents.
    fn plain_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn search(&mut self) {
        self.matches.clear();
        self.current_match = None;
        if self.search_input.is_empty() {
            return;
        }
        let query = self.search_input.to_lowercase();
        for (i, line) in self.lines.iter().enumerate() {
            let text = Self::plain_text(line);
            if text.to_lowercase().contains(&query) {
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

    fn yank_lines(&mut self, start: usize, end: usize) {
        let end = end.min(self.lines.len());
        if start >= end {
            return;
        }
        // Strip the [HH:MM:SS] timestamp prefix from yanked lines
        let text: String = self.lines[start..end]
            .iter()
            .map(|line| {
                let plain = Self::plain_text(line);
                if plain.starts_with('[')
                    && let Some(pos) = plain.find("] ")
                {
                    return plain[pos + 2..].to_string();
                }
                plain
            })
            .collect::<Vec<_>>()
            .join("\n");
        let msg = clipboard::yank_status(&text, end - start);
        self.status_message = Some((msg, Instant::now()));
    }

    fn yank_current(&mut self, count: usize) {
        let start = self.cursor;
        self.yank_lines(start, start + count);
    }

    fn ensure_cursor_visible(&mut self) {
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + self.visible_height {
            self.scroll = self.cursor.saturating_sub(self.visible_height - 1);
        }
    }

    fn move_cursor_down(&mut self, n: usize) {
        self.cursor = (self.cursor + n).min(self.lines.len().saturating_sub(1));
        self.ensure_cursor_visible();
    }

    fn move_cursor_up(&mut self, n: usize) {
        self.cursor = self.cursor.saturating_sub(n);
        self.ensure_cursor_visible();
    }
}

fn build_session_lines(
    chunks: &[Chunk],
    annotations: &[Annotation],
) -> (
    Vec<Line<'static>>,
    HashMap<i64, usize>,
) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut chunk_line_map: HashMap<i64, usize> = HashMap::new();

    if !annotations.is_empty() {
        let note_style = Style::default().fg(Color::Magenta);
        let label_style = Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD);
        let dim_style = Style::default().fg(Color::DarkGray);

        lines.push(Line::styled("── Notes ──", label_style));

        for ann in annotations {
            let time = ann.created_at.format("%Y-%m-%d %H:%M").to_string();
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
        lines.push(Line::raw(""));
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
            let is_input = chunk.kind == ChunkKind::Input;
            let mut spans = vec![Span::styled(format!("[{timestamp}] "), prefix_style)];
            spans.extend(highlight_line(line_text, style, is_input));
            lines.push(Line::from(spans));
        }
    }

    (lines, chunk_line_map)
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
    let (lines, _) = build_session_lines(&chunks, &annotations);
    let mut app = ViewApp {
        lines,
        title,
        scroll: 0,
        cursor: 0,
        visible_height: 0,
        mode: Mode::Normal,
        search_input: String::new(),
        matches: Vec::new(),
        current_match: None,
        highlight_lines: Vec::new(),
        from_search: false,
        visual_anchor: 0,
        pending_y: false,
        count_prefix: None,
        status_message: None,
    };

    let mut terminal = ratatui::init();
    crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture)?;
    let result = run_loop(&mut terminal, &mut app);
    crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture)?;
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
    let (lines, chunk_line_map) = build_session_lines(&chunks, &annotations);
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
        title,
        scroll: initial_scroll,
        cursor: initial_scroll,
        visible_height: 0,
        mode: Mode::Normal,
        search_input: String::new(),
        matches: Vec::new(),
        current_match: None,
        highlight_lines,
        from_search: true,
        visual_anchor: 0,
        pending_y: false,
        count_prefix: None,
        status_message: None,
    };

    run_loop(terminal, &mut app)
}

fn apply_line_bg(line: &Line<'static>, bg: Color) -> Line<'static> {
    Line::from(
        line.spans
            .iter()
            .map(|s| Span::styled(s.content.clone(), s.style.bg(bg)))
            .collect::<Vec<_>>(),
    )
}

fn run_loop(terminal: &mut DefaultTerminal, app: &mut ViewApp) -> Result<()> {
    loop {
        // Expire status message after 3 seconds
        if let Some((_, when)) = &app.status_message
            && when.elapsed().as_secs() >= 3
        {
            app.status_message = None;
        }

        terminal.draw(|frame| {
            let area = frame.area();
            let total_lines = app.lines.len();

            let has_status_bar = app.mode == Mode::SearchInput || app.status_message.is_some();
            let layout = if has_status_bar {
                Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area)
            } else {
                Layout::vertical([Constraint::Min(1)]).split(area)
            };

            let visible_height = layout[0].height.saturating_sub(2) as usize;
            app.visible_height = visible_height;
            let max_scroll = total_lines.saturating_sub(visible_height);
            app.scroll = app.scroll.min(max_scroll);
            app.cursor = app.cursor.min(total_lines.saturating_sub(1));

            // Determine visual selection range
            let visual_range = if app.mode == Mode::Visual {
                let a = app.visual_anchor.min(app.cursor);
                let b = app.visual_anchor.max(app.cursor);
                Some((a, b))
            } else {
                None
            };

            let has_search_matches = !app.matches.is_empty();
            let has_highlights = !app.highlight_lines.is_empty();
            // Only build the visible window. Cloning/restyling the whole buffer
            // every frame is O(total_lines); slicing keeps it O(visible_height)
            // regardless of session size. Rendered with no scroll offset since the
            // slice already starts at app.scroll.
            let start = app.scroll;
            let end = (start + visible_height).min(total_lines);
            let display_lines: Vec<Line> = app.lines[start..end]
                .iter()
                .enumerate()
                .map(|(li, line)| {
                    let i = start + li;
                    // Visual selection takes highest priority
                    if let Some((va, vb)) = visual_range
                        && i >= va && i <= vb
                    {
                        return apply_line_bg(line, Color::Rgb(60, 60, 100));
                    }
                    // Cursor line
                    if i == app.cursor && app.mode != Mode::SearchInput {
                        return apply_line_bg(line, Color::Rgb(30, 30, 50));
                    }
                    // Search matches
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
                        return apply_line_bg(line, bg);
                    }
                    // Chunk highlights from search jump
                    if has_highlights && app.highlight_lines.binary_search(&i).is_ok() {
                        return apply_line_bg(line, Color::Rgb(50, 50, 70));
                    }
                    line.clone()
                })
                .collect();

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

            let bottom_hint = match app.mode {
                Mode::SearchInput => " Enter confirm | Esc cancel ".to_string(),
                Mode::Visual => " V/Esc cancel | j/k select | y yank ".to_string(),
                Mode::Normal => {
                    if !app.matches.is_empty() {
                        format!(
                            " yy yank | V visual | n/N match | / search | {} {} ",
                            exit_hint, match_info
                        )
                    } else {
                        format!(" yy yank | V visual | / search | {} ", exit_hint)
                    }
                }
            };

            let block = Block::default()
                .title(app.title.as_str())
                .title_bottom(bottom_hint)
                .borders(Borders::ALL);

            // display_lines is already sliced to start at app.scroll, so no offset.
            let paragraph = Paragraph::new(display_lines).block(block);

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

            // Bottom bar: search input or status message
            if has_status_bar {
                if app.mode == Mode::SearchInput {
                    let search_line = Line::from(vec![
                        Span::styled("/", Style::default().fg(Color::Yellow)),
                        Span::raw(app.search_input.as_str()),
                        Span::styled("█", Style::default().fg(Color::Yellow)),
                    ]);
                    frame.render_widget(Paragraph::new(search_line), layout[1]);
                } else if let Some((ref msg, _)) = app.status_message {
                    let status_line = Line::from(Span::styled(
                        format!(" {msg}"),
                        Style::default().fg(Color::Green),
                    ));
                    frame.render_widget(Paragraph::new(status_line), layout[1]);
                }
            }
        })?;

        match event::read()? {
            Event::Mouse(mouse) => {
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        app.move_cursor_up(3);
                    }
                    MouseEventKind::ScrollDown => {
                        app.move_cursor_down(3);
                    }
                    MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                        // Click to position cursor (row is relative to terminal, adjust for border)
                        let clicked_line = app.scroll + mouse.row.saturating_sub(1) as usize;
                        if clicked_line < app.lines.len() {
                            app.cursor = clicked_line;
                        }
                    }
                    _ => {}
                }
            }
            Event::Key(key) => {
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
                Mode::Visual => match (key.code, key.modifiers) {
                    (KeyCode::Esc, _) | (KeyCode::Char('V'), _) | (KeyCode::Char('v'), _) => {
                        app.mode = Mode::Normal;
                    }
                    (KeyCode::Char('y'), _) => {
                        let a = app.visual_anchor.min(app.cursor);
                        let b = app.visual_anchor.max(app.cursor);
                        app.yank_lines(a, b + 1);
                        app.mode = Mode::Normal;
                    }
                    (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                        app.move_cursor_down(1);
                    }
                    (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                        app.move_cursor_up(1);
                    }
                    (KeyCode::Char('G'), _) | (KeyCode::End, _) => {
                        app.cursor = app.lines.len().saturating_sub(1);
                        app.ensure_cursor_visible();
                    }
                    (KeyCode::Char('g'), _) | (KeyCode::Home, _) => {
                        app.cursor = 0;
                        app.ensure_cursor_visible();
                    }
                    _ => {}
                },
                Mode::Normal => {
                    // Handle digit prefix for count (e.g. 3yy)
                    if let KeyCode::Char(c) = key.code
                        && c.is_ascii_digit() && !app.pending_y
                    {
                        let digit = c.to_digit(10).unwrap() as usize;
                        app.count_prefix = Some(
                            app.count_prefix.unwrap_or(0) * 10 + digit,
                        );
                        continue;
                    }

                    let count = app.count_prefix.take().unwrap_or(1);

                    if app.pending_y {
                        app.pending_y = false;
                        if key.code == KeyCode::Char('y') {
                            app.yank_current(count);
                            continue;
                        }
                        // Any other key after 'y' cancels the pending yank
                    }

                    match (key.code, key.modifiers) {
                        (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => break,
                        (KeyCode::Char('c'), KeyModifiers::CONTROL) => break,
                        (KeyCode::Char('/'), _) => {
                            app.clear_search();
                            app.mode = Mode::SearchInput;
                        }
                        (KeyCode::Char('n'), KeyModifiers::NONE) => {
                            app.next_match();
                            app.cursor = app.scroll;
                        }
                        (KeyCode::Char('N'), _) => {
                            app.prev_match();
                            app.cursor = app.scroll;
                        }
                        (KeyCode::Char('y'), _) => {
                            app.pending_y = true;
                            app.count_prefix = if count > 1 { Some(count) } else { None };
                        }
                        (KeyCode::Char('Y'), _) => {
                            app.yank_current(count);
                        }
                        (KeyCode::Char('V'), _) => {
                            app.visual_anchor = app.cursor;
                            app.mode = Mode::Visual;
                        }
                        (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                            app.move_cursor_down(count);
                        }
                        (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                            app.move_cursor_up(count);
                        }
                        (KeyCode::PageDown, _) | (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                            app.move_cursor_down(20);
                        }
                        (KeyCode::PageUp, _) | (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                            app.move_cursor_up(20);
                        }
                        (KeyCode::Home, _) | (KeyCode::Char('g'), _) => {
                            app.cursor = 0;
                            app.scroll = 0;
                        }
                        (KeyCode::End, _) | (KeyCode::Char('G'), _) => {
                            app.cursor = app.lines.len().saturating_sub(1);
                            app.ensure_cursor_visible();
                        }
                        _ => {}
                    }
                },
            }
        }
            _ => {}
        }
    }

    Ok(())
}
