use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthChar;

use crate::app::{App, ConnectionState};
use crate::theme;

pub(crate) const INPUT_MIN_HEIGHT: u16 = 3;
pub(crate) const INPUT_MAX_HEIGHT: u16 = 8;
const INPUT_MIN_INNER_ROWS: usize = (INPUT_MIN_HEIGHT - 2) as usize;
const INPUT_MAX_INNER_ROWS: usize = (INPUT_MAX_HEIGHT - 2) as usize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputViewport {
    pub visible_lines: Vec<String>,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub scroll_row: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WrappedInput {
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
}

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default().borders(Borders::ALL);
    let viewport = input_viewport(&app.input, app.cursor_pos, area.width);

    let lines: Vec<Line> = if app.input.is_empty() {
        // Show a placeholder reflecting connection state.
        let placeholder = match &app.state {
            ConnectionState::Connected => ">  Ask anything, / for commands..".to_string(),
            ConnectionState::Connecting(_) => "connecting...".to_string(),
            ConnectionState::Disconnected => "disconnect".to_string(),
            ConnectionState::Failed(_) => "disconnect".to_string(),
        };
        let mut placeholder_lines = vec![Line::from(Span::styled(placeholder, theme::DIM))];
        // Keep the same number of visible rows so layout doesn't jump.
        while placeholder_lines.len() < viewport.visible_lines.len() {
            placeholder_lines.push(Line::default());
        }
        placeholder_lines
    } else {
        viewport
            .visible_lines
            .iter()
            .map(|line| Line::from(Span::styled(line.clone(), theme::INPUT_TEXT)))
            .collect()
    };

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

pub(crate) fn input_height(input: &str, cursor_pos: usize, total_width: u16) -> u16 {
    let viewport = input_viewport(input, cursor_pos, total_width);
    (viewport.visible_lines.len() as u16 + 2).clamp(INPUT_MIN_HEIGHT, INPUT_MAX_HEIGHT)
}

pub(crate) fn cursor_position(app: &App, area: Rect) -> Option<Position> {
    if area.width <= 2 || area.height <= 2 {
        return None;
    }

    let viewport = input_viewport(&app.input, app.cursor_pos, area.width);
    let inner_width = area.width.saturating_sub(2) as usize;
    let cursor_col = viewport.cursor_col.min(inner_width.saturating_sub(1));
    let cursor_row = viewport
        .cursor_row
        .min(viewport.visible_lines.len().saturating_sub(1));

    Some(Position::new(
        area.x + 1 + cursor_col as u16,
        area.y + 1 + cursor_row as u16,
    ))
}

pub(crate) fn input_viewport(input: &str, cursor_pos: usize, total_width: u16) -> InputViewport {
    let inner_width = total_width.saturating_sub(2).max(1) as usize;
    let wrapped = wrap_input(input, cursor_pos, inner_width);
    let visible_rows = wrapped
        .lines
        .len()
        .clamp(INPUT_MIN_INNER_ROWS, INPUT_MAX_INNER_ROWS);
    let scroll_row = if wrapped.cursor_row + 1 > visible_rows {
        wrapped.cursor_row + 1 - visible_rows
    } else {
        0
    };
    let visible_lines = wrapped.lines[scroll_row..scroll_row + visible_rows].to_vec();

    InputViewport {
        visible_lines,
        cursor_row: wrapped.cursor_row.saturating_sub(scroll_row),
        cursor_col: wrapped.cursor_col,
        scroll_row,
    }
}

fn wrap_input(input: &str, cursor_pos: usize, max_width: usize) -> WrappedInput {
    let cursor_pos = clamp_cursor_to_boundary(input, cursor_pos);
    let max_width = max_width.max(1);

    let mut lines = vec![String::new()];
    let mut row = 0usize;
    let mut col = 0usize;
    let mut cursor = if cursor_pos == 0 {
        Some((0usize, 0usize))
    } else {
        None
    };

    for (idx, ch) in input.char_indices() {
        if cursor.is_none() && idx == cursor_pos {
            cursor = Some((row, col));
        }

        if ch == '\n' {
            row += 1;
            lines.push(String::new());
            col = 0;

            if cursor.is_none() && idx + ch.len_utf8() == cursor_pos {
                cursor = Some((row, col));
            }
            continue;
        }

        let char_width = char_display_width(ch);
        if col > 0 && col + char_width > max_width {
            row += 1;
            lines.push(String::new());
            col = 0;
        }

        lines[row].push(ch);
        col += char_width;

        if cursor.is_none() && idx + ch.len_utf8() == cursor_pos {
            cursor = Some((row, col));
        }
    }

    let (cursor_row, cursor_col) = cursor.unwrap_or((row, col));

    WrappedInput {
        lines,
        cursor_row,
        cursor_col,
    }
}

fn char_display_width(ch: char) -> usize {
    match ch {
        '\t' => 4,
        _ => UnicodeWidthChar::width(ch).unwrap_or(0).max(1),
    }
}

fn clamp_cursor_to_boundary(input: &str, cursor_pos: usize) -> usize {
    let mut clamped = cursor_pos.min(input.len());
    while clamped > 0 && !input.is_char_boundary(clamped) {
        clamped -= 1;
    }
    clamped
}

#[cfg(test)]
mod tests {
    use super::{input_height, input_viewport};

    #[test]
    fn empty_input_uses_single_visible_row() {
        let viewport = input_viewport("", 0, 20);

        assert_eq!(viewport.visible_lines, vec![String::new()]);
        assert_eq!(viewport.cursor_row, 0);
        assert_eq!(viewport.cursor_col, 0);
        assert_eq!(input_height("", 0, 20), 3);
    }

    #[test]
    fn long_input_wraps_and_grows_box() {
        let viewport = input_viewport("abcdefghij", 10, 8);

        assert_eq!(
            viewport.visible_lines,
            vec!["abcdef".to_string(), "ghij".to_string()]
        );
        assert_eq!(viewport.cursor_row, 1);
        assert_eq!(viewport.cursor_col, 4);
        assert_eq!(input_height("abcdefghij", 10, 8), 4);
    }

    #[test]
    fn viewport_scrolls_when_wrapped_content_exceeds_max_height() {
        let viewport = input_viewport("abcdefghijklmnopqrstuvwxyz0123456789!", 37, 8);

        assert_eq!(viewport.visible_lines.len(), 6);
        assert!(viewport.scroll_row > 0);
        assert_eq!(viewport.cursor_row, 5);
    }
}
