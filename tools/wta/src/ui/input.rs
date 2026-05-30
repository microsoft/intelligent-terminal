use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Padding, Paragraph};
use unicode_width::UnicodeWidthChar;

use crate::app::{App, ConnectionState};
use crate::theme;

pub(crate) const INPUT_MIN_HEIGHT: u16 = 3;
pub(crate) const INPUT_MAX_HEIGHT: u16 = 8;
const INPUT_LEFT_PAD: u16 = 1;
// Persistent prompt prefix: rendered in its own column at the very left of
// every visible line so it stays put when the user types, and so the
// placeholder, typed text and cursor all align under it. Width matches the
// span's literal cell width.
const INPUT_PROMPT: &str = "> ";
const INPUT_PROMPT_WIDTH: u16 = 2;
// Continuation lines (wrap rows past the first) get a space-only prefix of
// the same width so typed text stays vertically aligned with the column
// right of "> ".
const INPUT_PROMPT_CONT: &str = "  ";
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
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::INPUT_BORDER)
        .style(Style::new().bg(theme::INPUT_BG))
        .padding(Padding::new(INPUT_LEFT_PAD, 0, 0, 0));
    let tab = app.current_tab();
    let text_width = area
        .width
        .saturating_sub(INPUT_LEFT_PAD + 2 + INPUT_PROMPT_WIDTH);
    let viewport = input_viewport(&tab.input, tab.cursor_pos, text_width);

    let lines: Vec<Line> = if tab.input.is_empty() {
        // Show a placeholder reflecting connection state. The "> " is its
        // own span so the placeholder/typed text/cursor all sit in the same
        // column regardless of whether the input is empty.
        let placeholder = match &app.state {
            ConnectionState::Connected => t!("input.placeholder.connected").into_owned(),
            ConnectionState::Connecting(_) => t!("input.placeholder.connecting").into_owned(),
            ConnectionState::Disconnected => t!("input.placeholder.disconnected").into_owned(),
            ConnectionState::Failed(_) => t!("input.placeholder.disconnected").into_owned(),
        };
        // Paint the first cell of the placeholder as "white block with
        // black glyph" directly in the buffer. The WT block cursor lands
        // on this exact cell (input is empty ⇒ cursor_pos == 0) and is
        // alpha-overlaid onto an already-white cell — same color in, same
        // color out — so the visible result is a stable white block with
        // a readable black character. Setting only fg=Black wouldn't work:
        // the glyph would be painted onto the black cell bg first (Black
        // on Black = invisible) before the cursor overlay had anything to
        // reveal.
        //
        // Only paint the white block when the input cell is actually the
        // live caret target: the pane has XAML focus *and* the TUI's arrow
        // keys land in the input (not in a recommendation card or a
        // selected completed turn). See TabSession::input_has_nav_focus.
        let input_active = app.pane_focused && tab.input_has_nav_focus();
        let mut placeholder_spans = vec![Span::styled(INPUT_PROMPT, theme::DIM)];
        let mut chars = placeholder.chars();
        if let Some(first) = chars.next() {
            let first_style = if input_active {
                Style::new().fg(Color::Black).bg(Color::White)
            } else {
                theme::DIM
            };
            placeholder_spans.push(Span::styled(first.to_string(), first_style));
            let rest: String = chars.collect();
            if !rest.is_empty() {
                placeholder_spans.push(Span::styled(rest, theme::DIM));
            }
        }
        let mut placeholder_lines = vec![Line::from(placeholder_spans)];
        // Keep the same number of visible rows so layout doesn't jump.
        while placeholder_lines.len() < viewport.visible_lines.len() {
            placeholder_lines.push(Line::default());
        }
        placeholder_lines
    } else {
        viewport
            .visible_lines
            .iter()
            .enumerate()
            .map(|(i, line)| {
                // The "> " marker only marks wrap-row 0 of the input;
                // continuations get a same-width space prefix so text stays
                // column-aligned.
                let absolute_row = viewport.scroll_row + i;
                let prefix = if absolute_row == 0 {
                    Span::styled(INPUT_PROMPT, theme::DIM)
                } else {
                    Span::raw(INPUT_PROMPT_CONT)
                };
                Line::from(vec![prefix, Span::styled(line.clone(), theme::INPUT_TEXT)])
            })
            .collect()
    };

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

pub(crate) fn input_height(input: &str, cursor_pos: usize, total_width: u16) -> u16 {
    let viewport = input_viewport(
        input,
        cursor_pos,
        total_width.saturating_sub(INPUT_LEFT_PAD + 2 + INPUT_PROMPT_WIDTH),
    );
    (viewport.visible_lines.len() as u16 + 2).clamp(INPUT_MIN_HEIGHT, INPUT_MAX_HEIGHT)
}

pub(crate) fn cursor_position(app: &App, area: Rect) -> Option<Position> {
    if area.width <= INPUT_LEFT_PAD + 2 + INPUT_PROMPT_WIDTH || area.height <= 2 {
        return None;
    }

    let text_width = area
        .width
        .saturating_sub(INPUT_LEFT_PAD + 2 + INPUT_PROMPT_WIDTH);
    let tab = app.current_tab();
    let viewport = input_viewport(&tab.input, tab.cursor_pos, text_width);
    let cursor_col = viewport.cursor_col.min(text_width.saturating_sub(1) as usize);
    let cursor_row = viewport
        .cursor_row
        .min(viewport.visible_lines.len().saturating_sub(1));

    // +1 for the left `│` border, then inner padding, then the "> " prefix,
    // then the column inside the text.
    Some(Position::new(
        area.x + 1 + INPUT_LEFT_PAD + INPUT_PROMPT_WIDTH + cursor_col as u16,
        area.y + 1 + cursor_row as u16,
    ))
}

pub(crate) fn input_viewport(input: &str, cursor_pos: usize, total_width: u16) -> InputViewport {
    let inner_width = total_width.max(1) as usize;
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
        // `input_viewport` doesn't subtract borders/padding itself, so this
        // call wraps at exactly width=8.
        let viewport = input_viewport("abcdefghij", 10, 8);

        assert_eq!(
            viewport.visible_lines,
            vec!["abcdefgh".to_string(), "ij".to_string()]
        );
        assert_eq!(viewport.cursor_row, 1);
        assert_eq!(viewport.cursor_col, 2);

        // `input_height` subtracts INPUT_LEFT_PAD + 2 (borders) +
        // INPUT_PROMPT_WIDTH from the total width before wrapping, so the
        // usable inner text width here is 8 - 5 = 3. "abcdefghij" wraps to
        // 4 rows of width 3 → box height = 4 + 2 (borders) = 6.
        assert_eq!(input_height("abcdefghij", 10, 8), 6);
    }

    #[test]
    fn viewport_scrolls_when_wrapped_content_exceeds_max_height() {
        let viewport = input_viewport(
            "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOP!",
            53,
            8,
        );

        assert_eq!(viewport.visible_lines.len(), 6);
        assert!(viewport.scroll_row > 0);
        assert_eq!(viewport.cursor_row, 5);
    }
}
