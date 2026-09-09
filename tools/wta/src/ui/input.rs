use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Padding, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, AppMode, ConnectionState, PendingInputQueueSnapshot};
use crate::theme;

pub(crate) const INPUT_MIN_HEIGHT: u16 = 3;
pub(crate) const INPUT_MAX_HEIGHT: u16 = 8;
const INPUT_LEFT_PAD: u16 = 1;
const INPUT_QUEUE_STATUS_ROWS: usize = 1;
const INPUT_QUEUE_PREVIEW: usize = 2;
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
    pub visible_line_starts: Vec<usize>,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub scroll_row: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WrappedInput {
    lines: Vec<String>,
    line_starts: Vec<usize>,
    cursor_row: usize,
    cursor_col: usize,
}

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let tab = app.current_tab();
    let queue = app.current_tab_pending_input_queue_snapshot(INPUT_QUEUE_PREVIEW);
    let inner_rows = area.height.saturating_sub(2) as usize;
    let queue_status_visible = queue.count > 0 && inner_rows > INPUT_MIN_INNER_ROWS;
    let border_style = if app.pane_focused {
        theme::INPUT_BORDER_FOCUSED
    } else {
        theme::INPUT_BORDER
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .style(Style::new().bg(theme::INPUT_BG))
        .padding(Padding::new(INPUT_LEFT_PAD, 0, 0, 0));
    let content_width = area.width.saturating_sub(INPUT_LEFT_PAD + 2) as usize;
    let text_width = content_width.saturating_sub(INPUT_PROMPT_WIDTH as usize) as u16;
    if queue.count > 0 && !queue_status_visible {
        block = block.title(queue_status_text(app, &queue, content_width));
    }
    let viewport = input_viewport_with_max_rows(
        &tab.input,
        tab.cursor_pos,
        text_width,
        inner_rows.saturating_sub(usize::from(queue_status_visible)),
    );
    let attachment_ranges = tab.attachments.token_ranges().collect::<Vec<_>>();
    let prepared_command_range = app.prepared_command_range();
    let ghost_suffix = app.command_ghost_suffix();

    // The caret is painted as a buffer cell (not the OS cursor) in every
    // state, but only when the input box is the live caret target: the pane
    // has XAML focus *and* the TUI's arrow keys land in the input (not in a
    // recommendation card or a selected completed turn). See
    // TabSession::input_has_nav_focus.
    let input_active = app.pane_focused && tab.input_has_nav_focus();

    let mut lines = Vec::new();
    if queue_status_visible {
        let status_style = if queue.is_full {
            theme::BADGE_ACTIONABLE
        } else {
            theme::DIM
        };
        lines.push(Line::from(Span::styled(
            queue_status_text(app, &queue, content_width),
            status_style,
        )));
    }

    let input_lines: Vec<Line> = if tab.input.is_empty() {
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
                    push_caret_spans(
                        &mut spans,
                        line,
                        viewport.visible_line_starts[i],
                        &attachment_ranges,
                        prepared_command_range.as_ref(),
                        viewport.cursor_col,
                        ghost_suffix,
                    );
                    Line::from(spans)
                } else {
                    let mut spans = vec![prefix];
                    push_styled_input(
                        &mut spans,
                        line,
                        viewport.visible_line_starts[i],
                        &attachment_ranges,
                        prepared_command_range.as_ref(),
                    );
                    if i == viewport.cursor_row {
                        if let Some(suffix) = ghost_suffix {
                            spans.push(Span::styled(suffix.to_string(), theme::DIM));
                        }
                    }
                    Line::from(spans)
                }
            })
            .collect()
    };
    lines.extend(input_lines);

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

pub(crate) fn input_height_for_app(app: &App, total_width: u16) -> u16 {
    let tab = app.current_tab();
    let queue_status_rows = usize::from(app.current_tab_pending_input_queue_snapshot(0).count > 0)
        * INPUT_QUEUE_STATUS_ROWS;
    let viewport = input_viewport_with_max_rows(
        &tab.input,
        tab.cursor_pos,
        total_width.saturating_sub(INPUT_LEFT_PAD + 2 + INPUT_PROMPT_WIDTH),
        INPUT_MAX_INNER_ROWS.saturating_sub(queue_status_rows),
    );
    (viewport.visible_lines.len() as u16 + 2 + queue_status_rows as u16).clamp(
        INPUT_MIN_HEIGHT + queue_status_rows as u16,
        INPUT_MAX_HEIGHT,
    )
}

#[cfg(test)]
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
fn push_caret_spans(
    spans: &mut Vec<Span<'static>>,
    line: &str,
    line_start: usize,
    attachment_ranges: &[std::ops::Range<usize>],
    prepared_command_range: Option<&std::ops::Range<usize>>,
    caret_col: usize,
    ghost_suffix: Option<&str>,
) {
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
    let mut ghost_chars = ghost_suffix.unwrap_or_default().chars();
    let ghost_caret = if caret_ch.is_none() {
        ghost_chars.next()
    } else {
        None
    };
    let caret_text = caret_ch
        .or(ghost_caret)
        .map(|c| c.to_string())
        .unwrap_or_else(|| " ".to_string());

    push_styled_input(
        spans,
        &before,
        line_start,
        attachment_ranges,
        prepared_command_range,
    );
    // Reverse video so the caret block uses the scheme's own fg/bg and stays
    // visible on light schemes too (a hardcoded white block vanished on a
    // light background once the pane follows the scheme — #234).
    spans.push(Span::styled(
        caret_text,
        if ghost_caret.is_some() {
            theme::DIM.add_modifier(Modifier::REVERSED)
        } else {
            Style::new().add_modifier(Modifier::REVERSED)
        },
    ));
    if !after.is_empty() {
        let after_start =
            line_start + before.len() + caret_ch.map(|ch| ch.len_utf8()).unwrap_or_default();
        push_styled_input(
            spans,
            &after,
            after_start,
            attachment_ranges,
            prepared_command_range,
        );
    }

    let ghost_rest: String = ghost_chars.collect();
    if !ghost_rest.is_empty() {
        spans.push(Span::styled(ghost_rest, theme::DIM));
    }
}

fn push_styled_input(
    spans: &mut Vec<Span<'static>>,
    text: &str,
    source_start: usize,
    attachment_ranges: &[std::ops::Range<usize>],
    prepared_command_range: Option<&std::ops::Range<usize>>,
) {
    if text.is_empty() {
        return;
    }

    let mut run = String::new();
    let mut run_style = None;
    for (offset, ch) in text.char_indices() {
        let source_pos = source_start + offset;
        let style = if attachment_ranges
            .iter()
            .any(|range| range.contains(&source_pos))
        {
            theme::ATTACHMENT_TOKEN
        } else if prepared_command_range.is_some_and(|range| range.contains(&source_pos)) {
            theme::COMMAND_TOKEN
        } else {
            theme::INPUT_TEXT
        };
        if run_style.is_some_and(|current| current != style) {
            spans.push(Span::styled(
                std::mem::take(&mut run),
                run_style.unwrap_or(theme::INPUT_TEXT),
            ));
        }
        run_style = Some(style);
        run.push(ch);
    }
    spans.push(Span::styled(run, run_style.unwrap_or(theme::INPUT_TEXT)));
}

#[cfg(test)]
pub(crate) fn input_viewport(input: &str, cursor_pos: usize, total_width: u16) -> InputViewport {
    input_viewport_with_max_rows(input, cursor_pos, total_width, INPUT_MAX_INNER_ROWS)
}

fn input_viewport_with_max_rows(
    input: &str,
    cursor_pos: usize,
    total_width: u16,
    max_visible_rows: usize,
) -> InputViewport {
    let inner_width = total_width.max(1) as usize;
    let wrapped = wrap_input(input, cursor_pos, inner_width);
    let max_visible_rows = max_visible_rows.clamp(INPUT_MIN_INNER_ROWS, INPUT_MAX_INNER_ROWS);
    let visible_rows = wrapped
        .lines
        .len()
        .clamp(INPUT_MIN_INNER_ROWS, max_visible_rows);
    let scroll_row = if wrapped.cursor_row + 1 > visible_rows {
        wrapped.cursor_row + 1 - visible_rows
    } else {
        0
    };
    let visible_lines = wrapped.lines[scroll_row..scroll_row + visible_rows].to_vec();
    let visible_line_starts = wrapped.line_starts[scroll_row..scroll_row + visible_rows].to_vec();

    InputViewport {
        visible_lines,
        visible_line_starts,
        cursor_row: wrapped.cursor_row.saturating_sub(scroll_row),
        cursor_col: wrapped.cursor_col,
        scroll_row,
    }
}

fn wrap_input(input: &str, cursor_pos: usize, max_width: usize) -> WrappedInput {
    let cursor_pos = clamp_cursor_to_boundary(input, cursor_pos);
    let max_width = max_width.max(1);

    let mut lines = vec![String::new()];
    let mut line_starts = vec![0usize];
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
            line_starts.push(idx + ch.len_utf8());
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
            line_starts.push(idx);
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
            line_starts.push(input.len());
        }
    }

    WrappedInput {
        lines,
        line_starts,
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

fn queue_status_text(app: &App, queue: &PendingInputQueueSnapshot<'_>, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }

    let status_prefix = if queue.is_full {
        t!(
            "input.queue.status_full",
            count = queue.count.to_string(),
            capacity = queue.capacity.to_string(),
            items = ""
        )
        .into_owned()
    } else {
        t!(
            "input.queue.status",
            count = queue.count.to_string(),
            items = ""
        )
        .into_owned()
    };
    let hint = queue_status_hint(app, queue).map(|key| t!(key).into_owned());
    format_queue_status(&status_prefix, queue, hint.as_deref(), max_width)
}

fn queue_status_hint(app: &App, queue: &PendingInputQueueSnapshot<'_>) -> Option<&'static str> {
    if app.mode != AppMode::Chat || app.help_overlay_visible || app.show_notification_banner {
        return None;
    }

    let tab = app.current_tab();
    let has_draft = !tab.input.is_empty() || !tab.attachments.is_empty();
    if has_draft {
        let enter_queues_draft = !queue.is_full
            && app.state == ConnectionState::Connected
            && tab.input_has_nav_focus()
            && app.command_popup_state().is_none()
            && !matches!(
                crate::commands::classify(&tab.input),
                crate::commands::ParseOutcome::Command(_)
            );
        enter_queues_draft.then_some("input.queue.hint_with_draft")
    } else {
        let escape_removes_newest = tab.selected_completed_turn_idx.is_none()
            && !tab.paste_pending
            && tab.user_input.is_empty()
            && tab.permission.is_empty()
            && !tab.model_picker_open
            && !tab.config_picker.is_open()
            && !tab.agent_picker_open;
        escape_removes_newest.then_some("input.queue.hint_without_draft")
    }
}

fn format_queue_status(
    status_prefix: &str,
    queue: &PendingInputQueueSnapshot<'_>,
    hint: Option<&str>,
    max_width: usize,
) -> String {
    let bare_prefix = status_prefix.trim_end();
    let bare_prefix_width = UnicodeWidthStr::width(bare_prefix);
    if bare_prefix_width >= max_width {
        return truncate_to_width(bare_prefix, max_width);
    }

    let Some(hint) = hint else {
        let preview_budget = max_width.saturating_sub(UnicodeWidthStr::width(status_prefix));
        let preview = queue_preview_list(queue, preview_budget);
        if preview.is_empty() {
            return bare_prefix.to_string();
        }
        return format!("{status_prefix}{preview}");
    };

    const HINT_SEPARATOR: &str = "  ";
    let hint_width = UnicodeWidthStr::width(hint);
    let separator_width = UnicodeWidthStr::width(HINT_SEPARATOR);
    if bare_prefix_width + separator_width >= max_width {
        return truncate_to_width(bare_prefix, max_width);
    }

    let status_prefix_width = UnicodeWidthStr::width(status_prefix);
    let total_budget = max_width.saturating_sub(status_prefix_width + separator_width);
    if total_budget == 0 {
        return bare_prefix.to_string();
    }

    let full_preview = queue_preview_list(queue, total_budget);
    if full_preview.is_empty() {
        return format!(
            "{bare_prefix}{HINT_SEPARATOR}{}",
            truncate_to_width(
                hint,
                max_width.saturating_sub(bare_prefix_width + separator_width)
            )
        );
    }

    if hint_width <= total_budget {
        let preview_budget = total_budget.saturating_sub(hint_width);
        let preview = queue_preview_list(queue, preview_budget);
        return if preview.is_empty() {
            format!("{bare_prefix}{HINT_SEPARATOR}{hint}")
        } else {
            format!("{status_prefix}{preview}{HINT_SEPARATOR}{hint}")
        };
    }

    let minimum_hint_width = hint_focus_width(hint).min(total_budget);
    let max_preview_budget = total_budget.saturating_sub(minimum_hint_width);
    if max_preview_budget == 0 {
        return format!(
            "{bare_prefix}{HINT_SEPARATOR}{}",
            truncate_to_width(hint, total_budget)
        );
    }

    let preferred_preview_budget = total_budget / 3;
    let preview_budget = preview_width(&full_preview)
        .min(max_preview_budget)
        .min(preferred_preview_budget.max(1));
    let preview_budget = preview_budget.max(1).min(max_preview_budget);
    let preview = queue_preview_list(queue, preview_budget);
    if preview.is_empty() {
        return format!(
            "{bare_prefix}{HINT_SEPARATOR}{}",
            truncate_to_width(hint, total_budget)
        );
    }

    let hint_budget = total_budget.saturating_sub(preview_width(&preview));
    if hint_budget == 0 {
        return format!("{status_prefix}{preview}");
    }

    format!(
        "{status_prefix}{preview}{HINT_SEPARATOR}{}",
        truncate_to_width(hint, hint_budget)
    )
}

fn queue_preview_list(queue: &PendingInputQueueSnapshot<'_>, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }

    const SEPARATOR: &str = " • ";
    let mut preview = String::new();
    let mut remaining = max_width;
    let mut has_any = false;

    for display_text in &queue.display_texts {
        let Some(item) = queued_preview(display_text) else {
            continue;
        };
        if has_any && !push_if_fits(&mut preview, SEPARATOR, &mut remaining) {
            return preview;
        }
        if !push_truncated(&mut preview, item, &mut remaining) {
            return preview;
        }
        has_any = true;
    }

    if queue.hidden > 0 {
        if has_any && !push_if_fits(&mut preview, SEPARATOR, &mut remaining) {
            return preview;
        }
        let hidden = format!("+{}", queue.hidden);
        let _ = push_truncated(&mut preview, &hidden, &mut remaining);
    }

    preview
}

fn queued_preview(display_text: &str) -> Option<&str> {
    let preview = display_text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_else(|| display_text.trim());
    (!preview.is_empty()).then_some(preview)
}

fn hint_focus_width(hint: &str) -> usize {
    hint.split_whitespace()
        .next()
        .map(preview_width)
        .unwrap_or_default()
}

fn preview_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn push_if_fits(out: &mut String, text: &str, remaining: &mut usize) -> bool {
    let width = preview_width(text);
    if width > *remaining {
        return false;
    }
    out.push_str(text);
    *remaining -= width;
    true
}

fn push_truncated(out: &mut String, text: &str, remaining: &mut usize) -> bool {
    if *remaining == 0 {
        return false;
    }

    let width = preview_width(text);
    if width <= *remaining {
        out.push_str(text);
        *remaining -= width;
        return true;
    }

    out.push_str(&truncate_to_width(text, *remaining));
    *remaining = 0;
    false
}

fn truncate_to_width(text: &str, max: usize) -> String {
    if UnicodeWidthStr::width(text) <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }

    let budget = max.saturating_sub(1);
    let mut width = 0usize;
    let mut out = String::new();
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > budget {
            break;
        }
        out.push(ch);
        width += ch_width;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::{input_height, input_viewport, push_caret_spans, push_styled_input};
    use crate::theme;

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
        push_caret_spans(&mut spans, "ab", 0, &[], None, 2, None);

        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content.as_ref(), "ab");
        assert_eq!(spans[1].content.as_ref(), " ");
    }

    #[test]
    fn caret_in_middle_splits_before_glyph_after() {
        let mut spans = Vec::new();
        push_caret_spans(&mut spans, "abcd", 0, &[], None, 1, None);

        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content.as_ref(), "a");
        assert_eq!(spans[1].content.as_ref(), "b");
        assert_eq!(spans[2].content.as_ref(), "cd");
    }

    #[test]
    fn ghost_suffix_starts_under_end_caret() {
        let mut spans = Vec::new();
        push_caret_spans(&mut spans, "/agent co", 0, &[], None, 9, Some("pilot"));

        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content.as_ref(), "/agent co");
        assert_eq!(spans[1].content.as_ref(), "p");
        assert_eq!(spans[2].content.as_ref(), "ilot");
    }

    #[test]
    fn attachment_range_renders_as_a_distinct_chip() {
        let token = "[image: image-1.png]";
        let text = format!("a{token}b");
        let mut spans = Vec::new();

        push_styled_input(&mut spans, &text, 0, &[1..1 + token.len()], None);

        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content.as_ref(), "a");
        assert_eq!(spans[1].content.as_ref(), token);
        assert_eq!(spans[1].style, theme::ATTACHMENT_TOKEN);
        assert_eq!(spans[2].content.as_ref(), "b");
    }

    #[test]
    fn prepared_command_range_renders_as_a_distinct_token() {
        let mut spans = Vec::new();

        push_styled_input(&mut spans, "/intent describe it", 0, &[], Some(&(0..7)));

        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content.as_ref(), "/intent");
        assert_eq!(spans[0].style, theme::COMMAND_TOKEN);
        assert_eq!(spans[1].content.as_ref(), " describe it");
        assert_eq!(spans[1].style, theme::INPUT_TEXT);
    }
}
