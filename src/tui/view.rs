use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::{
    DefaultTerminal,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
};

use crate::storage::models::ChunkKind;
use crate::storage::Database;

pub fn run(session_id: &str) -> Result<()> {
    let db = Database::open()?;
    let full_id = db.resolve_session_id(session_id)?;
    let chunks = db.get_session_chunks(&full_id)?;

    if chunks.is_empty() {
        println!("No output recorded for session {session_id}.");
        return Ok(());
    }

    // Build displayable lines with timestamps
    let mut lines: Vec<Line> = Vec::new();
    for chunk in &chunks {
        let timestamp = chunk.timestamp.format("%H:%M:%S").to_string();
        let style = match chunk.kind {
            ChunkKind::Input => Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            ChunkKind::Output => Style::default().fg(Color::White),
        };
        let prefix_style = Style::default().fg(Color::DarkGray);

        let clean = strip_ansi_escapes::strip_str(&chunk.content);
        for line_text in clean.lines() {
            if line_text.trim().is_empty() {
                continue;
            }
            lines.push(Line::from(vec![
                Span::styled(format!("[{timestamp}] "), prefix_style),
                Span::styled(line_text.to_string(), style),
            ]));
        }
    }

    let mut terminal = ratatui::init();
    let result = run_loop(&mut terminal, &lines, &full_id);
    ratatui::restore();
    result
}

fn run_loop(terminal: &mut DefaultTerminal, lines: &[Line], session_id: &str) -> Result<()> {
    let total_lines = lines.len();
    // Paragraph::scroll() takes u16, so we track scroll as usize and cast when rendering
    let mut scroll: usize = 0;

    loop {
        terminal.draw(|frame| {
            let area = frame.area();

            let chunks = Layout::vertical([Constraint::Min(1)]).split(area);

            let visible_height = chunks[0].height.saturating_sub(2) as usize;
            let max_scroll = total_lines.saturating_sub(visible_height);

            // Clamp scroll to valid range
            scroll = scroll.min(max_scroll);

            let title = format!(" broll view — session {} ", &session_id[..8]);
            let block = Block::default()
                .title(title)
                .title_bottom(" ↑/↓ scroll | q quit ")
                .borders(Borders::ALL);

            let paragraph = Paragraph::new(lines.to_vec())
                .block(block)
                .scroll((scroll as u16, 0));

            frame.render_widget(paragraph, chunks[0]);

            // Scrollbar
            let mut scrollbar_state =
                ScrollbarState::new(max_scroll).position(scroll);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight),
                chunks[0],
                &mut scrollbar_state,
            );
        })?;

        if let Event::Key(key) = event::read()? {
            match (key.code, key.modifiers) {
                (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => break,
                (KeyCode::Char('c'), KeyModifiers::CONTROL) => break,
                (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                    scroll = scroll.saturating_add(1);
                }
                (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                    scroll = scroll.saturating_sub(1);
                }
                (KeyCode::PageDown, _) | (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                    scroll = scroll.saturating_add(20);
                }
                (KeyCode::PageUp, _) | (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                    scroll = scroll.saturating_sub(20);
                }
                (KeyCode::Home, _) | (KeyCode::Char('g'), _) => {
                    scroll = 0;
                }
                (KeyCode::End, _) | (KeyCode::Char('G'), _) => {
                    scroll = total_lines;
                }
                _ => {}
            }
        }
    }

    Ok(())
}
