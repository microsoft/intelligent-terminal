use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, ConnectionState};
use crate::theme;

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let title = if app.state == ConnectionState::Connected {
        if app.recommendations.is_some() && app.input.is_empty() {
            " Enter executes selected recommendation "
        } else {
            " > "
        }
    } else {
        " (not connected) "
    };

    let block = Block::default().borders(Borders::ALL).title(title);

    let paragraph = Paragraph::new(Span::styled(&app.input, theme::INPUT_TEXT)).block(block);

    frame.render_widget(paragraph, area);

    // Place cursor
    let inner_x = area.x + 1 + app.cursor_pos as u16;
    let inner_y = area.y + 1;
    if inner_x < area.x + area.width - 1 {
        frame.set_cursor_position((inner_x, inner_y));
    }
}
