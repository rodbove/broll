use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::{
    DefaultTerminal,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState,
    },
};
use std::time::{Duration, Instant};

use super::clipboard;
use crate::storage::models::SearchHit;
use crate::storage::Database;

#[derive(PartialEq)]
enum Focus {
    Input,
    Results,
    Preview,
}

enum Action {
    Quit,
    ViewSession { session_id: String, chunk_id: i64 },
}

struct SearchApp {
    db: Database,
    hits: Vec<SearchHit>,
    list_state: ListState,
    input: String,
    cursor_pos: usize,
    last_searched: String,
    last_input_time: Option<Instant>,
    group: Option<String>,
    terminal_filter: Option<String>,
    preview_scroll: usize,
    focus: Focus,
    pending_y: bool,
    status_message: Option<(String, Instant)>,
}

const DEBOUNCE_MS: u64 = 150;

impl SearchApp {
    fn new(
        db: Database,
        query: Option<String>,
        group: Option<String>,
        terminal_filter: Option<String>,
    ) -> Self {
        let input = query.unwrap_or_default();
        Self {
            db,
            hits: Vec::new(),
            list_state: ListState::default(),
            cursor_pos: input.len(),
            input,
            last_searched: String::new(),
            last_input_time: None,
            group,
            terminal_filter,
            preview_scroll: 0,
            focus: Focus::Input,
            pending_y: false,
            status_message: None,
        }
    }

    fn execute_search(&mut self) {
        let query = self.input.trim();
        if query.is_empty() {
            self.hits.clear();
            self.list_state.select(None);
            self.last_searched.clear();
            return;
        }
        if query == self.last_searched {
            return;
        }
        self.last_searched = query.to_string();
        match self.db.search(
            query,
            self.group.as_deref(),
            self.terminal_filter.as_deref(),
        ) {
            Ok(hits) => {
                self.hits = hits;
                if self.hits.is_empty() {
                    self.list_state.select(None);
                } else {
                    self.list_state.select(Some(0));
                }
            }
            Err(_) => {
                // FTS5 query error (e.g. special chars) — keep previous results
            }
        }
        self.preview_scroll = 0;
    }

    fn selected_hit(&self) -> Option<&SearchHit> {
        self.list_state.selected().and_then(|i| self.hits.get(i))
    }

    fn next(&mut self) {
        if self.hits.is_empty() {
            return;
        }
        let i = self
            .list_state
            .selected()
            .map(|i| (i + 1).min(self.hits.len() - 1))
            .unwrap_or(0);
        self.list_state.select(Some(i));
        self.preview_scroll = 0;
    }

    fn previous(&mut self) {
        if self.hits.is_empty() {
            return;
        }
        let i = self
            .list_state
            .selected()
            .map(|i| i.saturating_sub(1))
            .unwrap_or(0);
        self.list_state.select(Some(i));
        self.preview_scroll = 0;
    }
}

pub fn run(
    query: Option<String>,
    group: Option<String>,
    terminal_filter: Option<String>,
) -> Result<()> {
    let db = Database::open()?;
    let mut terminal = ratatui::init();
    let mut app = SearchApp::new(db, query, group, terminal_filter);

    // If an initial query was provided, run the search immediately
    if !app.input.is_empty() {
        app.execute_search();
        // Start in results mode if we have results
        if !app.hits.is_empty() {
            app.focus = Focus::Results;
        }
    }

    let result = loop {
        match run_loop(&mut terminal, &mut app)? {
            Action::Quit => break Ok(()),
            Action::ViewSession {
                session_id,
                chunk_id,
            } => {
                super::view::run_in_terminal(&mut terminal, &session_id, Some(chunk_id))?;
            }
        }
    };

    ratatui::restore();
    result
}

fn run_loop(terminal: &mut DefaultTerminal, app: &mut SearchApp) -> Result<Action> {
    loop {
        // Check if we need to fire a debounced search
        if app
            .last_input_time
            .is_some_and(|t| t.elapsed() >= Duration::from_millis(DEBOUNCE_MS))
        {
            app.last_input_time = None;
            app.execute_search();
        }

        // Expire status message after 3 seconds
        if app
            .status_message
            .as_ref()
            .is_some_and(|(_, when)| when.elapsed().as_secs() >= 3)
        {
            app.status_message = None;
        }

        terminal.draw(|frame| {
            let area = frame.area();

            let has_status = app.status_message.is_some();

            // Layout: input bar on top, main content, optional status
            let mut constraints = vec![Constraint::Length(3), Constraint::Min(1)];
            if has_status {
                constraints.push(Constraint::Length(1));
            }
            let outer = Layout::vertical(constraints).split(area);

            // Input bar
            let input_border_style = if app.focus == Focus::Input {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            let input_block = Block::default()
                .title(" Search ")
                .borders(Borders::ALL)
                .border_style(input_border_style);
            let input_widget = Paragraph::new(Line::from(vec![
                Span::styled("> ", Style::default().fg(Color::Yellow)),
                Span::raw(&app.input),
            ]))
            .block(input_block);
            frame.render_widget(input_widget, outer[0]);

            // Place cursor in input when focused
            if app.focus == Focus::Input {
                frame.set_cursor_position((
                    outer[0].x + 2 + app.cursor_pos as u16 + 1, // +1 for border, +2 for "> "
                    outer[0].y + 1,
                ));
            }

            // Split main area: left panel (results list) | right panel (preview)
            let chunks =
                Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                    .split(outer[1]);

            // Left: results list
            let items: Vec<ListItem> = app
                .hits
                .iter()
                .map(|hit| {
                    let session_label = match &hit.session.name {
                        Some(name) => name.clone(),
                        None => hit.session.id[..8].to_string(),
                    };
                    let time = hit.chunk.timestamp.format("%Y-%m-%d %H:%M:%S");
                    let clean = strip_ansi_escapes::strip_str(&hit.chunk.content);
                    let preview: String = clean.chars().take(60).collect();
                    let preview = preview.replace('\n', " ");
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            format!("[{session_label}] "),
                            Style::default().fg(Color::Cyan),
                        ),
                        Span::styled(format!("{time} "), Style::default().fg(Color::DarkGray)),
                        Span::raw(preview),
                    ]))
                })
                .collect();

            let count_display = if app.hits.len() >= 100 {
                "100+".to_string()
            } else {
                format!("{}", app.hits.len())
            };
            let list_title = if app.input.trim().is_empty() {
                " Results ".to_string()
            } else {
                format!(" Results ({}) ", count_display)
            };
            let results_border_style = if app.focus == Focus::Results {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            let list = List::new(items)
                .block(
                    Block::default()
                        .title(list_title)
                        .borders(Borders::ALL)
                        .border_style(results_border_style),
                )
                .highlight_style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("▶ ");

            frame.render_stateful_widget(list, chunks[0], &mut app.list_state);

            // Right: preview of selected hit
            let preview_content = if let Some(hit) = app.selected_hit() {
                let mut lines = Vec::new();
                let session_short = &hit.session.id[..8];
                let time = hit.chunk.timestamp.format("%Y-%m-%d %H:%M:%S");

                lines.push(Line::from(vec![
                    Span::styled("Session: ", Style::default().fg(Color::Yellow)),
                    Span::raw(session_short.to_string()),
                    Span::raw("  "),
                    Span::styled("Time: ", Style::default().fg(Color::Yellow)),
                    Span::raw(time.to_string()),
                ]));

                if let Some(ref name) = hit.session.name {
                    lines.push(Line::from(vec![
                        Span::styled("Name: ", Style::default().fg(Color::Yellow)),
                        Span::raw(name.clone()),
                    ]));
                }

                if let Some(ref group) = hit.session.group {
                    lines.push(Line::from(vec![
                        Span::styled("Group: ", Style::default().fg(Color::Yellow)),
                        Span::raw(group.clone()),
                    ]));
                }

                lines.push(Line::raw(""));
                lines.push(Line::styled(
                    "─".repeat(chunks[1].width as usize - 2),
                    Style::default().fg(Color::DarkGray),
                ));
                lines.push(Line::raw(""));

                let clean = strip_ansi_escapes::strip_str(&hit.chunk.content);
                for text_line in clean.lines() {
                    lines.push(Line::raw(text_line.to_string()));
                }

                lines
            } else if app.input.trim().is_empty() {
                vec![Line::styled(
                    "Type to search...",
                    Style::default().fg(Color::DarkGray),
                )]
            } else {
                vec![Line::raw("No results")]
            };

            let preview_total_lines = preview_content.len();
            let preview_visible = chunks[1].height.saturating_sub(2) as usize;
            let preview_max_scroll = preview_total_lines.saturating_sub(preview_visible);
            app.preview_scroll = app.preview_scroll.min(preview_max_scroll);

            let preview_border_style = if app.focus == Focus::Preview {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            let help_text = match app.focus {
                Focus::Input => " Type to search | ↓/Tab navigate | Esc quit ",
                _ => " / search | Tab focus | ↑/↓ nav | Enter view | yy/Y yank | q quit ",
            };
            let preview = Paragraph::new(preview_content)
                .block(
                    Block::default()
                        .title(" Preview ")
                        .title_bottom(help_text)
                        .borders(Borders::ALL)
                        .border_style(preview_border_style),
                )
                .scroll((app.preview_scroll as u16, 0));

            frame.render_widget(preview, chunks[1]);

            // Preview scrollbar
            if preview_max_scroll > 0 {
                let mut scrollbar_state = ScrollbarState::new(preview_max_scroll)
                    .position(app.preview_scroll);
                frame.render_stateful_widget(
                    Scrollbar::new(ScrollbarOrientation::VerticalRight),
                    chunks[1],
                    &mut scrollbar_state,
                );
            }

            // Status message
            if let Some((ref msg, _)) = app.status_message {
                let status_line = Line::from(Span::styled(
                    format!(" {msg}"),
                    Style::default().fg(Color::Green),
                ));
                frame.render_widget(Paragraph::new(status_line), outer[outer.len() - 1]);
            }
        })?;

        // Use a short poll timeout so debounce fires promptly
        let poll_timeout = if app.last_input_time.is_some() {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(100)
        };

        if !event::poll(poll_timeout)? {
            continue;
        }

        if let Event::Key(key) = event::read()? {
            // Input mode handling
            if app.focus == Focus::Input {
                match (key.code, key.modifiers) {
                    (KeyCode::Esc, _) => {
                        if app.input.is_empty() {
                            return Ok(Action::Quit);
                        }
                        // If we have results, move to results; otherwise quit
                        if !app.hits.is_empty() {
                            app.focus = Focus::Results;
                        } else {
                            return Ok(Action::Quit);
                        }
                    }
                    (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(Action::Quit),
                    (KeyCode::Enter, _) | (KeyCode::Down, _) => {
                        // Execute search immediately and move focus to results
                        app.last_input_time = None;
                        app.execute_search();
                        if !app.hits.is_empty() {
                            app.focus = Focus::Results;
                        }
                    }
                    (KeyCode::Tab, _) => {
                        app.last_input_time = None;
                        app.execute_search();
                        if !app.hits.is_empty() {
                            app.focus = Focus::Results;
                        }
                    }
                    (KeyCode::Char('a'), KeyModifiers::CONTROL) | (KeyCode::Home, _) => {
                        app.cursor_pos = 0;
                    }
                    (KeyCode::Char('e'), KeyModifiers::CONTROL) | (KeyCode::End, _) => {
                        app.cursor_pos = app.input.len();
                    }
                    (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                        app.input.drain(..app.cursor_pos);
                        app.cursor_pos = 0;
                        app.last_input_time = Some(Instant::now());
                    }
                    (KeyCode::Char('w'), KeyModifiers::CONTROL) => {
                        // Delete word backwards
                        let new_pos = app.input[..app.cursor_pos]
                            .rfind(|c: char| c.is_whitespace())
                            .map(|i| i + 1)
                            .unwrap_or(0);
                        app.input.drain(new_pos..app.cursor_pos);
                        app.cursor_pos = new_pos;
                        app.last_input_time = Some(Instant::now());
                    }
                    (KeyCode::Char(c), _) => {
                        app.input.insert(app.cursor_pos, c);
                        app.cursor_pos += 1;
                        app.last_input_time = Some(Instant::now());
                    }
                    (KeyCode::Backspace, _) => {
                        if app.cursor_pos > 0 {
                            app.cursor_pos -= 1;
                            app.input.remove(app.cursor_pos);
                            app.last_input_time = Some(Instant::now());
                        }
                    }
                    (KeyCode::Delete, _) => {
                        if app.cursor_pos < app.input.len() {
                            app.input.remove(app.cursor_pos);
                            app.last_input_time = Some(Instant::now());
                        }
                    }
                    (KeyCode::Left, _) => {
                        app.cursor_pos = app.cursor_pos.saturating_sub(1);
                    }
                    (KeyCode::Right, _) => {
                        app.cursor_pos = (app.cursor_pos + 1).min(app.input.len());
                    }
                    _ => {}
                }
                continue;
            }

            // Results/Preview mode handling

            // Handle pending 'y' for yy combo
            if app.pending_y {
                app.pending_y = false;
                if key.code == KeyCode::Char('y') {
                    // yy: yank first line of selected result
                    if let Some(hit) = app.selected_hit() {
                        let clean = strip_ansi_escapes::strip_str(&hit.chunk.content);
                        let first_line = clean.lines().next().unwrap_or("").to_string();
                        clipboard::copy_to_clipboard(&first_line);
                        app.status_message = Some(("1 line yanked".to_string(), Instant::now()));
                    }
                    continue;
                }
                // Fall through to normal handling if not 'y'
            }

            match (key.code, key.modifiers) {
                (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => return Ok(Action::Quit),
                (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(Action::Quit),
                (KeyCode::Char('/'), _) | (KeyCode::Char('i'), _) => {
                    app.focus = Focus::Input;
                }
                (KeyCode::Enter, _) => {
                    if let Some(hit) = app.selected_hit() {
                        return Ok(Action::ViewSession {
                            session_id: hit.session.id.clone(),
                            chunk_id: hit.chunk.id,
                        });
                    }
                }
                (KeyCode::Char('y'), _) => {
                    app.pending_y = true;
                }
                (KeyCode::Char('Y'), _) => {
                    // Y: yank full chunk
                    if let Some(hit) = app.selected_hit() {
                        let clean = strip_ansi_escapes::strip_str(&hit.chunk.content);
                        let text = clean.trim().to_string();
                        let line_count = text.lines().count();
                        clipboard::copy_to_clipboard(&text);
                        app.status_message = Some((
                            format!(
                                "{line_count} line{} yanked",
                                if line_count == 1 { "" } else { "s" }
                            ),
                            Instant::now(),
                        ));
                    }
                }
                (KeyCode::Tab, _) => {
                    app.focus = match app.focus {
                        Focus::Results => Focus::Preview,
                        _ => Focus::Results,
                    };
                }
                (KeyCode::Down, _) | (KeyCode::Char('j'), _) => match app.focus {
                    Focus::Results => app.next(),
                    _ => {
                        app.preview_scroll = app.preview_scroll.saturating_add(1);
                    }
                },
                (KeyCode::Up, _) | (KeyCode::Char('k'), _) => match app.focus {
                    Focus::Results => app.previous(),
                    _ => {
                        app.preview_scroll = app.preview_scroll.saturating_sub(1);
                    }
                },
                _ => {}
            }
        }
    }
}
