use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::theme;

/// Horizontal chrome between `main_area.width` and a card's inner text:
/// 2 (h_rec/h_perm outer padding) + 2 (border) + 4 (inset, 2 each side) = 8.
pub const CARD_H_CHROME: u16 = 8;

/// Minimum `area.{width,height}` for `render_card_shell` to paint anything:
/// 2 borders + content(1) + divider(1) + buttons(1) = 5. Callers reserving
/// fewer rows than this would leave the card invisible — clamp to 0 instead.
pub const CARD_MIN_SIZE: u16 = 5;

/// Wrap width inside a card given the outer panel width. Floors at 1 so
/// `div_ceil` callers don't divide by zero on absurdly narrow terminals.
pub fn card_content_width(panel_width: u16) -> usize {
    (panel_width as usize).saturating_sub(CARD_H_CHROME as usize).max(1)
}

pub fn inset_horizontal(r: Rect, n: u16) -> Rect {
    Rect {
        x: r.x.saturating_add(n),
        y: r.y,
        width: r.width.saturating_sub(n.saturating_mul(2)),
        height: r.height,
    }
}

/// Paint the card chrome (outer border + middle divider) and return the
/// inner content/button regions. Returns `None` when `area` is smaller than
/// `CARD_MIN_SIZE` in either dimension.
pub fn render_card_shell(
    frame: &mut Frame,
    area: Rect,
    border_style: Style,
) -> Option<(Rect, Rect)> {
    if area.width < CARD_MIN_SIZE || area.height < CARD_MIN_SIZE {
        return None;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let inner_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);
    let content_area = inner_chunks[0];
    let divider_y = inner_chunks[1].y;
    let button_area = inner_chunks[2];

    render_divider(frame.buffer_mut(), area, divider_y, border_style);
    Some((content_area, button_area))
}

pub fn render_divider(buf: &mut Buffer, area: Rect, y: u16, border_style: Style) {
    if y < area.y || y >= area.y.saturating_add(area.height) {
        return;
    }
    if area.width < 2 {
        return;
    }
    let left = area.x;
    let right = area.x.saturating_add(area.width).saturating_sub(1);
    if left >= right {
        return;
    }
    buf.set_string(left, y, "├", border_style);
    let middle_width = area.width.saturating_sub(2) as usize;
    if middle_width > 0 {
        buf.set_string(
            left.saturating_add(1),
            y,
            "─".repeat(middle_width),
            border_style,
        );
    }
    buf.set_string(right, y, "┤", border_style);
}

/// Render a left-aligned button row. `focused` is the index of the focused
/// button (rendered with `BUTTON_FOCUSED`); pass `None` when the card has
/// focus elsewhere — all buttons render with `BUTTON_PLAIN`.
pub fn render_buttons(
    frame: &mut Frame,
    area: Rect,
    buttons: &[String],
    focused: Option<usize>,
) {
    let mut spans: Vec<Span> = Vec::new();
    for (i, label) in buttons.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("   "));
        }
        let style = if focused == Some(i) {
            theme::BUTTON_FOCUSED
        } else {
            theme::BUTTON_PLAIN
        };
        spans.push(Span::styled(label.clone(), style));
    }
    let para = Paragraph::new(Line::from(spans));
    frame.render_widget(para, area);
}

