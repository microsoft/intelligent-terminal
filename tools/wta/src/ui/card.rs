use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::theme;

/// Horizontal chrome between `main_area.width` and a card's inner text:
/// 2 (h_rec/h_perm outer padding) + 2 (border) + 4 (inset, 2 each side) = 8.
pub const CARD_H_CHROME: u16 = 8;

/// Minimum `area.{width,height}` for `render_card_shell` to paint anything:
/// 2 borders + content(1) + divider(1) + buttons(1) = 5. Callers reserving
/// fewer rows than this would leave the card invisible — clamp to 0 instead.
pub const CARD_MIN_SIZE: u16 = 5;

/// Shared style for lightweight cards embedded in transcript line streams.
///
/// Unlike [`render_card_shell`], transcript cards do not own a frame area or
/// contain buttons. They return bordered [`Line`]s that participate in normal
/// chat scrolling.
#[derive(Clone, Copy)]
pub struct TranscriptCardStyle {
    pub max_width: usize,
    pub horizontal_padding: usize,
    pub border_style: Style,
}

pub const TRANSCRIPT_CARD_MAX_WIDTH: usize = 96;
pub const TRANSCRIPT_CARD: TranscriptCardStyle = TranscriptCardStyle {
    max_width: TRANSCRIPT_CARD_MAX_WIDTH,
    horizontal_padding: 1,
    border_style: theme::CARD_BORDER,
};

pub struct TranscriptCardRow {
    text: String,
    style: Style,
    indent: usize,
}

impl TranscriptCardRow {
    pub fn new(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
            indent: 0,
        }
    }

    pub fn indented(text: impl Into<String>, indent: usize, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
            indent,
        }
    }

    pub fn blank() -> Self {
        Self::new(String::new(), Style::default())
    }
}

impl TranscriptCardStyle {
    pub fn content_width(self, available_width: usize) -> usize {
        available_width
            .min(self.max_width)
            .saturating_sub(2 + self.horizontal_padding * 2)
            .max(1)
    }

    pub fn lines(
        self,
        rows: impl IntoIterator<Item = TranscriptCardRow>,
        available_width: usize,
    ) -> Vec<Line<'static>> {
        let card_width = available_width.min(self.max_width);
        let content_width = self.content_width(available_width);
        let mut content = Vec::new();
        for row in rows {
            let indent = row.indent.min(content_width.saturating_sub(1));
            let wrap_width = content_width.saturating_sub(indent).max(1);
            let wrapped = textwrap::wrap(&row.text, wrap_width);
            if wrapped.is_empty() {
                content.push((String::new(), row.style));
            } else {
                content.extend(wrapped.into_iter().map(|line| {
                    (
                        format!("{}{line}", " ".repeat(indent)),
                        row.style,
                    )
                }));
            }
        }

        let minimum_width = 2 + self.horizontal_padding * 2 + 1;
        if card_width < minimum_width {
            return content
                .into_iter()
                .map(|(text, style)| Line::from(Span::styled(text, style)))
                .collect();
        }

        let horizontal = "─".repeat(card_width.saturating_sub(2));
        let side_padding = " ".repeat(self.horizontal_padding);
        let mut lines = vec![Line::from(Span::styled(
            format!("┌{horizontal}┐"),
            self.border_style,
        ))];
        lines.extend(content.into_iter().map(|(text, style)| {
            let trailing = " ".repeat(content_width.saturating_sub(text.width()));
            Line::from(vec![
                Span::styled(format!("│{side_padding}"), self.border_style),
                Span::styled(text, style),
                Span::raw(trailing),
                Span::styled(format!("{side_padding}│"), self.border_style),
            ])
        }));
        lines.push(Line::from(Span::styled(
            format!("└{horizontal}┘"),
            self.border_style,
        )));
        lines
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn transcript_card_wraps_indents_and_preserves_width() {
        let lines = TRANSCRIPT_CARD.lines(
            [
                TranscriptCardRow::new("Title", Style::default()),
                TranscriptCardRow::indented("a long directory path", 2, Style::default()),
            ],
            16,
        );

        assert!(line_text(&lines[0]).starts_with('┌'));
        assert!(line_text(&lines[1]).contains("Title"));
        assert!(lines[2..lines.len() - 1]
            .iter()
            .all(|line| line_text(line).starts_with("│   ")));
        assert!(lines.iter().all(|line| line_text(line).width() == 16));
    }

    #[test]
    fn transcript_card_caps_width_and_handles_wide_characters() {
        let lines = TRANSCRIPT_CARD.lines(
            [TranscriptCardRow::new("目录", Style::default())],
            TRANSCRIPT_CARD_MAX_WIDTH + 20,
        );

        assert!(lines
            .iter()
            .all(|line| line_text(line).width() == TRANSCRIPT_CARD_MAX_WIDTH));
    }
}
