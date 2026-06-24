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
    let tab = app.current_tab();
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::INPUT_BORDER)
        .style(Style::new().bg(theme::INPUT_BG))
        .padding(Padding::new(INPUT_LEFT_PAD, 0, 0, 0));
    // Queued Alt+V images surface as a title on the top border so the user can
    // see what will be sent without spending an inner text row.
    if !tab.pending_images.is_empty() {
        let items = tab
            .pending_images
            .iter()
            .enumerate()
            .map(|(i, img)| format!("[{}] {}", i + 1, img.label))
            .collect::<Vec<_>>()
            .join(", ");
        let title = t!("input.image_attachments", items = items).into_owned();
        block = block.title(Span::styled(title, theme::INPUT_TEXT));
    }
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
        // Paint the first cell of the placeholder as the caret using reverse
        // video (swap the scheme's fg/bg) so it reads as a solid block in the
        // scheme's own colors. A hardcoded white block was invisible on light
        // schemes once the pane background follows the scheme (#234). The OS
        // cursor stays hidden (`terminal.hide_cursor`), so this painted cell
        // is the only caret.
        let mut placeholder_spans = vec![Span::styled(INPUT_PROMPT, theme::DIM)];
        let mut chars = placeholder.chars();
        if let Some(first) = chars.next() {
            let first_style = if input_active {
                Style::new().add_modifier(Modifier::REVERSED)
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
                    let mut spans = vec![prefix];
                    push_caret_spans(&mut spans, line, viewport.cursor_col);
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
    // Reverse video so the caret block uses the scheme's own fg/bg and stays
    // visible on light schemes too (a hardcoded white block vanished on a
    // light background once the pane follows the scheme — #234).
    spans.push(Span::styled(
        caret_text,
        Style::new().add_modifier(Modifier::REVERSED),
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

    let (mut cursor_row, mut cursor_col) = cursor.unwrap_or((row, col));

    // When the caret is at the very end of the input and sits at the right
    // edge of a full line, show it at the start of a fresh next line instead
    // of the overflow column (where the appended caret cell would be clipped).
    // The next typed glyph wraps down there anyway, so the caret just leads
    // it. Gated on end-of-input: a caret in the middle of text that happens to
    // land on a wrap boundary (e.g. just before a `\n` or wrapped content) is
    // left where it is so it doesn't jump onto the following line's glyph.
    if cursor_col >= max_width && cursor_pos == input.len() {
        cursor_row += 1;
        cursor_col = 0;
        if lines.len() <= cursor_row {
            lines.push(String::new());
        }
    }

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

    #[test]
    fn caret_at_end_of_full_line_moves_to_next_line() {
        // "abcdefgh" exactly fills width 8 with the cursor at the end. Rather
        // than stranding the caret in the overflow column (where it would be
        // clipped), it shows at the start of a fresh empty line below, and the
        // box grows a row to make room.
        let viewport = input_viewport("abcdefgh", 8, 8);

        assert_eq!(
            viewport.visible_lines,
            vec!["abcdefgh".to_string(), String::new()]
        );
        assert_eq!(viewport.cursor_row, 1);
        assert_eq!(viewport.cursor_col, 0);
        // inner width 8 (= 13 - 5 borders/pad/prefix): 2 rows + 2 borders.
        assert_eq!(input_height("abcdefgh", 8, 13), 4);
    }

    #[test]
    fn caret_mid_text_at_wrap_boundary_does_not_jump_to_next_line() {
        // Caret just before a hard '\n' that lands on the wrap boundary is
        // mid-text, not end-of-input, so it must stay on its own line instead
        // of jumping onto the following line's glyph.
        let viewport = input_viewport("abcdefgh\nx", 8, 8);
        assert_eq!(viewport.cursor_row, 0);
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
