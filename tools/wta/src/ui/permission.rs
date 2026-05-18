use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::App;
use crate::theme;

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let perm = match &app.current_tab().permission {
        Some(p) => p,
        None => return,
    };

    // Center modal: 60 wide, 4 + options.len() tall
    let height = (4 + perm.options.len()) as u16;
    let width = 60.min(area.width.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2 + area.x;
    let y = (area.height.saturating_sub(height)) / 2 + area.y;
    let modal_area = Rect::new(x, y, width, height);

    // Clear background
    frame.render_widget(Clear, modal_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Permission ")
        .border_style(theme::PERMISSION)
        .style(Style::default().bg(Color::Black));

    let inner = block.inner(modal_area);
    frame.render_widget(block, modal_area);

    // Description line
    let desc = Paragraph::new(Span::styled(&perm.description, theme::PERMISSION));
    let desc_area = Rect::new(inner.x, inner.y, inner.width, 1);
    frame.render_widget(desc, desc_area);

    // Options
    for (i, opt) in perm.options.iter().enumerate() {
        let style = if i == perm.selected {
            theme::SELECTED
        } else {
            theme::PERMISSION
        };
        let label = format!("  [{}] {}", i + 1, opt.name);
        let p = Paragraph::new(Span::styled(label, style));
        let opt_area = Rect::new(inner.x, inner.y + 1 + i as u16, inner.width, 1);
        frame.render_widget(p, opt_area);
    }

    // Hint
    let hint_y = inner.y + 1 + perm.options.len() as u16;
    if hint_y < inner.y + inner.height {
        let hint = Paragraph::new(Span::styled(
            "  Up/Down to select, Enter to confirm, y/n quick",
            theme::DIM,
        ));
        let hint_area = Rect::new(inner.x, hint_y, inner.width, 1);
        frame.render_widget(hint, hint_area);
    }
}
