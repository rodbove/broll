use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::{
    DefaultTerminal,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
};

use crate::storage::models::SearchHit;
use crate::storage::Database;

#[derive(PartialEq)]
enum Focus {
    Results,
    Preview,
}

struct SearchApp {
    hits: Vec<SearchHit>,
    list_state: ListState,
    query: String,
    preview_scroll: usize,
    focus: Focus,
}

impl SearchApp {
    fn new(query: String, hits: Vec<SearchHit>) -> Self {
        let mut list_state = ListState::default();
        if !hits.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            hits,
            list_state,
            query,
            preview_scroll: 0,
            focus: Focus::Results,
        }
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

pub fn run(query: String, group: Option<String>, terminal_filter: Option<String>) -> Result<()> {
    let db = Database::open()?;
    let hits = db.search(&query, group.as_deref(), terminal_filter.as_deref())?;

    if hits.is_empty() {
        println!("No results found for '{query}'.");
        return Ok(());
    }

    let mut terminal = ratatui::init();
    let result = run_loop(&mut terminal, SearchApp::new(query, hits));
    ratatui::restore();
    result
}

fn run_loop(terminal: &mut DefaultTerminal, mut app: SearchApp) -> Result<()> {
    loop {
        terminal.draw(|frame| {
            let area = frame.area();

            // Split: left panel (results list) | right panel (preview)
            let chunks = Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                .split(area);

            // Left: results list
            let items: Vec<ListItem> = app
                .hits
                .iter()
                .map(|hit| {
                    let session_short = &hit.session.id[..8];
                    let time = hit.chunk.timestamp.format("%Y-%m-%d %H:%M:%S");
                    let clean = strip_ansi_escapes::strip_str(&hit.chunk.content);
                    let preview: String = clean.chars().take(60).collect();
                    let preview = preview.replace('\n', " ");
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            format!("[{session_short}] "),
                            Style::default().fg(Color::Cyan),
                        ),
                        Span::styled(format!("{time} "), Style::default().fg(Color::DarkGray)),
                        Span::raw(preview),
                    ]))
                })
                .collect();

            let count_display = if app.hits.len() >= 100 {
                "100+ results".to_string()
            } else {
                format!("{}", app.hits.len())
            };
            let list_title = format!(" Results for '{}' ({}) ", app.query, count_display);
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
            } else {
                vec![Line::raw("No selection")]
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
            let preview = Paragraph::new(preview_content)
                .block(
                    Block::default()
                        .title(" Preview ")
                        .title_bottom(" Tab switch | ↑/↓ navigate | q quit ")
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
        })?;

        if let Event::Key(key) = event::read()? {
            match (key.code, key.modifiers) {
                (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => break,
                (KeyCode::Char('c'), KeyModifiers::CONTROL) => break,
                (KeyCode::Tab, _) => {
                    app.focus = match app.focus {
                        Focus::Results => Focus::Preview,
                        Focus::Preview => Focus::Results,
                    };
                }
                (KeyCode::Down, _) | (KeyCode::Char('j'), _) => match app.focus {
                    Focus::Results => app.next(),
                    Focus::Preview => {
                        app.preview_scroll = app.preview_scroll.saturating_add(1);
                    }
                },
                (KeyCode::Up, _) | (KeyCode::Char('k'), _) => match app.focus {
                    Focus::Results => app.previous(),
                    Focus::Preview => {
                        app.preview_scroll = app.preview_scroll.saturating_sub(1);
                    }
                },
                _ => {}
            }
        }
    }

    Ok(())
}
