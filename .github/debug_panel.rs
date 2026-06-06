use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::{App, DebugDir};
use crate::theme;

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Debug (F12) ")
        .borders(Borders::ALL)
        .border_style(theme::DIM);

    let inner = block.inner(area);

    // Build lines from debug messages
    let lines: Vec<Line> = app
        .debug_messages
        .iter()
        .flat_map(|msg| {
            let (arrow, style) = match msg.direction {
                DebugDir::Sent => (">>> ", theme::DEBUG_SENT),
                DebugDir::Received => ("<<< ", theme::DEBUG_RECEIVED),
            };
            // Truncate long content for display
            let content = if msg.content.len() > 200 {
                format!("{}...", &msg.content[..200])
            } else {
                msg.content.clone()
            };
            // Format timestamp as relative seconds (last 4 digits)
            let ts = format!("{:.1}", msg.timestamp % 10000.0);
            let header = Line::from(vec![
                Span::styled(format!("[{}] ", ts), theme::DIM),
                Span::styled(arrow, style),
            ]);
            let body = Line::from(Span::styled(content, style));
            vec![header, body]
        })
        .collect();

    let total_lines = lines.len() as u16;
    let visible = inner.height;

    // Auto-scroll to bottom when debug_scroll == 0
    let scroll = if app.debug_scroll == 0 {
        total_lines.saturating_sub(visible)
    } else {
        total_lines
            .saturating_sub(visible)
            .saturating_sub(app.debug_scroll as u16)
    };

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    frame.render_widget(paragraph, area);
}
