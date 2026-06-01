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

    // The caret is painted as a buffer cell (not the OS cursor) in every
    // state, but only when the input box is the live caret target: the pane
    // has XAML focus *and* the TUI's arrow keys land in the input (not in a
    // recommendation card or a selected completed turn). See
    // TabSession::input_has_nav_focus.
    let input_active = app.pane_focused && tab.input_has_nav_focus();

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
                // Paint the caret as an inverse cell on the row/column the
                // cursor sits on. This replaces the OS block cursor so there
                // is nothing for WT to blink or tear, and lets `draw_frame`
                // keep the OS cursor hidden in every state.
                if input_active && i == viewport.cursor_row {
                    // Clamp to the last visible cell. At the end of a line that
                    // already fills `text_width`, `cursor_col == text_width`;
                    // an appended caret cell would be clipped off the right
                    // edge and vanish, so sit the caret on the last glyph
                    // instead (matches the OS-cursor clamp this replaced). The
                    // `min` is an upper bound only — short lines keep the caret
                    // in the blank cell right after their text.
                    let caret_col =
                        viewport.cursor_col.min((text_width as usize).saturating_sub(1));
                    let mut spans = vec![prefix];
                    push_caret_spans(&mut spans, line, caret_col);
                    Line::from(spans)
                } else {
                    Line::from(vec![prefix, Span::styled(line.clone(), theme::INPUT_TEXT)])
                }
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

/// Split `line` at the caret display-column and push up to three spans onto
/// `spans`: the text before the caret, the caret cell (the glyph under it, or
/// a space when the caret sits past the last char at end of line) painted as
/// an inverse block, and the text after. `caret_col` is a display-cell column
/// produced by `wrap_input`, so it always lands on a char boundary.
fn push_caret_spans(spans: &mut Vec<Span<'static>>, line: &str, caret_col: usize) {
    let mut before = String::new();
    let mut col = 0usize;
    let mut chars = line.chars();
    let mut caret_ch: Option<char> = None;
    for ch in chars.by_ref() {
        if col >= caret_col {
            caret_ch = Some(ch);
            break;
        }
        before.push(ch);
        col += char_display_width(ch);
    }
    let after: String = chars.collect();
    let caret_text = caret_ch.map(|c| c.to_string()).unwrap_or_else(|| " ".to_string());

    if !before.is_empty() {
        spans.push(Span::styled(before, theme::INPUT_TEXT));
    }
    spans.push(Span::styled(
        caret_text,
        Style::new().fg(Color::Black).bg(Color::White),
    ));
    if !after.is_empty() {
        spans.push(Span::styled(after, theme::INPUT_TEXT));
    }
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
    use super::{input_height, input_viewport, push_caret_spans};

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

    fn spans_text(spans: &[ratatui::text::Span]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn caret_at_end_of_full_line_sits_on_last_glyph() {
        // Caret column clamped to the last cell of a 4-wide line. The caret
        // must land on the final glyph (not an appended cell that would be
        // clipped past the right edge), and total rendered width stays 4.
        let mut spans = Vec::new();
        push_caret_spans(&mut spans, "abcd", 3);

        assert_eq!(spans_text(&spans), "abcd");
        assert_eq!(spans_text(&spans).chars().count(), 4);
        // before = "abc", caret cell = "d" (no trailing span).
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[1].content.as_ref(), "d");
    }

    #[test]
    fn caret_past_end_of_short_line_uses_blank_cell() {
        // Short line: the caret sits in the blank cell right after the text.
        let mut spans = Vec::new();
        push_caret_spans(&mut spans, "ab", 2);

        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content.as_ref(), "ab");
        assert_eq!(spans[1].content.as_ref(), " ");
    }

    #[test]
    fn caret_in_middle_splits_before_glyph_after() {
        let mut spans = Vec::new();
        push_caret_spans(&mut spans, "abcd", 1);

        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content.as_ref(), "a");
        assert_eq!(spans[1].content.as_ref(), "b");
        assert_eq!(spans[2].content.as_ref(), "cd");
    }
}
