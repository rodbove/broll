use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::{
    DefaultTerminal,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

use crate::storage::models::SearchHit;
use crate::storage::Database;

struct SearchApp {
    hits: Vec<SearchHit>,
    list_state: ListState,
    query: String,
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

            let list_title = format!(" Results for '{}' ({}) ", app.query, app.hits.len());
            let list = List::new(items)
                .block(
                    Block::default()
                        .title(list_title)
                        .borders(Borders::ALL),
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

            let preview = Paragraph::new(preview_content).block(
                Block::default()
                    .title(" Preview ")
                    .title_bottom(" ↑/↓ navigate | q quit ")
                    .borders(Borders::ALL),
            );

            frame.render_widget(preview, chunks[1]);
        })?;

        if let Event::Key(key) = event::read()? {
            match (key.code, key.modifiers) {
                (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => break,
                (KeyCode::Char('c'), KeyModifiers::CONTROL) => break,
                (KeyCode::Down, _) | (KeyCode::Char('j'), _) => app.next(),
                (KeyCode::Up, _) | (KeyCode::Char('k'), _) => app.previous(),
                _ => {}
            }
        }
    }

    Ok(())
}
