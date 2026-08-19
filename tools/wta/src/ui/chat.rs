use std::borrow::Cow;
#[cfg(test)]
use std::cell::Cell;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use tui_markdown::{Options as MarkdownOptions, StyleSheet};
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

#[cfg(test)]
use crate::app::CompletedTurn;
use crate::app::{
    App, ChatMessage, NoticeKind, PlanEntryStatus, ToolCallContent, ToolCallKind, ToolCallLocation,
    ToolCallOutput,
};
use crate::theme;
use crate::ui::shimmer;
use crate::ui_trace;

fn activity_label() -> String {
    t!("chat.activity_thinking").into_owned()
}

const MAX_RENDER_LINE_CHARS: usize = 4096;
const MAX_TOOL_OUTPUT_LINES: usize = 4;
const MAX_TOOL_OUTPUT_LINE_CHARS: usize = 240;
const MAX_TOOL_PREVIEW_LINES: usize = 2;
const MAX_TOOL_DETAIL_OUTPUT_LINES: usize = 12;
const MAX_TOOL_DETAIL_LINES: usize = 32;

#[cfg(test)]
thread_local! {
    static COMPLETED_TURN_LINE_BUILD_COUNT: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_completed_turn_line_build_count() {
    COMPLETED_TURN_LINE_BUILD_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn completed_turn_line_build_count() -> usize {
    COMPLETED_TURN_LINE_BUILD_COUNT.with(Cell::get)
}

#[cfg(test)]
fn record_completed_turn_line_build() {
    COMPLETED_TURN_LINE_BUILD_COUNT.with(|count| count.set(count.get() + 1));
}

fn tool_output_lines(output: &ToolCallOutput) -> Vec<String> {
    let mut lines = output.text.lines().rev();
    let mut tail: Vec<String> = lines
        .by_ref()
        .take(MAX_TOOL_OUTPUT_LINES)
        .map(|line| {
            let mut chars = line.chars();
            let head: String = chars.by_ref().take(MAX_TOOL_OUTPUT_LINE_CHARS).collect();
            if chars.next().is_some() {
                format!("{head}…")
            } else {
                head
            }
        })
        .collect();
    let omitted = output.truncated || lines.next().is_some();
    tail.reverse();

    let mut lines = Vec::with_capacity(MAX_TOOL_OUTPUT_LINES + usize::from(omitted));
    if omitted {
        lines.push("…".to_string());
    }
    lines.extend(tail);
    lines
}

fn full_output_lines(output: &ToolCallOutput, prefix: &str) -> Vec<String> {
    let mut source = output.text.lines().rev();
    let mut lines: Vec<String> = source
        .by_ref()
        .take(MAX_TOOL_DETAIL_OUTPUT_LINES)
        .map(|line| {
            let mut chars = line.chars();
            let head: String = chars.by_ref().take(MAX_TOOL_OUTPUT_LINE_CHARS).collect();
            let suffix = if chars.next().is_some() { "…" } else { "" };
            format!("{prefix}{head}{suffix}")
        })
        .collect();
    let omitted = output.truncated || source.next().is_some();
    lines.reverse();
    if omitted {
        lines.insert(0, format!("{prefix}…"));
    }
    if lines.is_empty() {
        lines.push(prefix.trim_end().to_string());
    }
    lines
}

fn preview_output_lines(output: &ToolCallOutput, prefix: &str) -> Vec<String> {
    let mut source = output.text.lines().rev();
    let mut lines: Vec<String> = source
        .by_ref()
        .take(MAX_TOOL_PREVIEW_LINES)
        .map(|line| {
            let mut chars = line.chars();
            let head: String = chars.by_ref().take(MAX_TOOL_OUTPUT_LINE_CHARS).collect();
            let suffix = if chars.next().is_some() { "…" } else { "" };
            format!("{prefix}{head}{suffix}")
        })
        .collect();
    let omitted = output.truncated || source.next().is_some();
    lines.reverse();
    if omitted {
        lines.insert(0, format!("{prefix}…"));
    }
    lines
}

fn tool_detail_lines(
    content: &[ToolCallContent],
    locations: &[ToolCallLocation],
    detailed: bool,
) -> Vec<String> {
    let mut lines = Vec::new();
    let mut omitted = false;
    if detailed {
        for location in locations.iter().take(MAX_TOOL_DETAIL_LINES) {
            let suffix = location
                .line
                .map_or_else(String::new, |line| format!(":{line}"));
            lines.push(format!("    {}{suffix}", location.path));
        }
        omitted = locations.len() > MAX_TOOL_DETAIL_LINES;
    }
    for item in content {
        if lines.len() >= MAX_TOOL_DETAIL_LINES {
            omitted = true;
            break;
        }
        match item {
            ToolCallContent::Text(output) => {
                if detailed {
                    lines.extend(full_output_lines(output, "    │ "));
                } else {
                    lines.extend(preview_output_lines(output, "    │ "));
                }
            }
            ToolCallContent::Diff {
                path,
                old_text,
                new_text,
            } => {
                lines.push(format!("    Δ {path}"));
                if detailed {
                    if let Some(old_text) = old_text {
                        lines.extend(full_output_lines(old_text, "    - "));
                    }
                    lines.extend(full_output_lines(new_text, "    + "));
                }
            }
            ToolCallContent::Terminal {
                id,
                output,
                exit_code,
            } => {
                let status = exit_code.map_or_else(String::new, |code| format!(" · exit {code}"));
                lines.push(format!("    $ {id}{status}"));
                if detailed {
                    if let Some(output) = output {
                        lines.extend(full_output_lines(output, "    │ "));
                    }
                }
            }
            ToolCallContent::Attachment { label, uri } => {
                let target = uri
                    .as_deref()
                    .map_or_else(String::new, |uri| format!(" · {uri}"));
                lines.push(format!("    ↳ {label}{target}"));
            }
        }
        if lines.len() > MAX_TOOL_DETAIL_LINES {
            omitted = true;
            break;
        }
    }
    if omitted {
        lines.truncate(MAX_TOOL_DETAIL_LINES.saturating_sub(1));
        lines.push("    …".to_string());
    }
    lines
}

/// Estimate the chat block's natural height (in visual rows) given the
/// rendering width. Counts wraps for each message + completed turn. Used by
/// `layout::render` to size the
/// chat area so the rec panel sits directly below content instead of being
/// pushed to the pane bottom by a `Min(1)` spacer.
pub fn estimated_block_height(app: &App, area_width: u16) -> u16 {
    let tab = app.current_tab();
    let wrap_width = (area_width as usize).max(1);
    // Fetch once for the pending-height calculation.
    let pending_text = pending_render_text(tab);

    let streaming_index = tab.streaming_agent_message_index();
    let permission_tool_call_id = permission_tool_call_id(tab);
    let messages: usize = tab
        .messages
        .iter()
        .enumerate()
        .filter(|(index, _)| Some(*index) != streaming_index)
        .map(|(index, message)| {
            rendered_lines_height(
                &build_message_lines(
                    message,
                    index + 1 == tab.messages.len(),
                    tab.turn.is_streaming(),
                    permission_tool_call_id,
                    tab.activity_frame,
                    wrap_width,
                ),
                wrap_width,
            )
        })
        .sum();
    let turns: usize = tab
        .completed_turns
        .iter()
        .map(|turn| {
            rendered_lines_height(
                &build_completed_turn_lines(turn, false, false, wrap_width),
                wrap_width,
            )
        })
        .sum();
    let pending = pending_text
        .map(|_| rendered_lines_height(&build_pending_stream_lines(app, wrap_width), wrap_width))
        .unwrap_or(0);
    // Welcome overlay sits above all chat content when `show_welcome_hint`
    // is on; must be counted here or else any pushed message will scroll
    // it off the top of the visible chat block. Always a single row —
    // terminal min-width guarantees the localized title fits without
    // wrapping.
    let welcome = if app.show_welcome_hint && app.state == crate::app::ConnectionState::Connected {
        1
    } else {
        0
    };

    (messages + turns + pending + welcome)
        .max(1)
        .min(u16::MAX as usize) as u16
}

#[derive(Clone)]
struct AgentMarkdownStyleSheet;

impl StyleSheet for AgentMarkdownStyleSheet {
    fn heading(&self, _level: u8) -> Style {
        theme::MARKDOWN_HEADING
    }

    fn code(&self) -> Style {
        theme::MARKDOWN_CODE
    }

    fn link(&self) -> Style {
        theme::MARKDOWN_LINK
    }

    fn blockquote(&self) -> Style {
        theme::MARKDOWN_QUOTE
    }

    fn heading_meta(&self) -> Style {
        theme::MARKDOWN_META
    }

    fn metadata_block(&self) -> Style {
        theme::MARKDOWN_META
    }

    fn table_header(&self) -> Style {
        theme::MARKDOWN_TABLE_HEADER
    }

    fn table_cell(&self) -> Style {
        theme::AGENT_TEXT
    }

    fn table_border(&self) -> Style {
        theme::MARKDOWN_TABLE_BORDER
    }

    fn heading_marker(&self, _level: u8) -> &str {
        ""
    }

    fn code_block_fence(&self) -> &str {
        ""
    }
}

#[derive(Clone, Copy)]
struct StyledChar {
    value: char,
    style: Style,
}

fn styled_chars_to_line(chars: &[StyledChar]) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for styled in chars {
        if let Some(span) = spans.last_mut().filter(|span| span.style == styled.style) {
            span.content.to_mut().push(styled.value);
        } else {
            spans.push(Span::styled(styled.value.to_string(), styled.style));
        }
    }
    Line::from(spans)
}

fn wrap_markdown_line(line: Line<'_>, width: usize) -> Vec<Line<'static>> {
    let base_style = theme::AGENT_TEXT.patch(line.style);
    let chars: Vec<StyledChar> = line
        .spans
        .into_iter()
        .flat_map(|span| {
            let style = base_style.patch(span.style);
            span.content
                .into_owned()
                .chars()
                .map(move |value| StyledChar { value, style })
                .collect::<Vec<_>>()
        })
        .collect();

    if chars.is_empty() {
        return vec![Line::default()];
    }

    let width = width.max(1);
    let mut wrapped = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let mut end = start;
        let mut cells = 0;
        let mut last_whitespace = None;
        while end < chars.len() {
            let char_width = UnicodeWidthChar::width(chars[end].value).unwrap_or(0);
            if end > start && cells + char_width > width {
                break;
            }
            cells += char_width;
            if chars[end].value.is_whitespace() {
                last_whitespace = Some(end);
            }
            end += 1;
            if cells >= width {
                break;
            }
        }

        let next = if end < chars.len() {
            last_whitespace
                .filter(|index| *index > start)
                .unwrap_or(end)
        } else {
            end
        };
        wrapped.push(styled_chars_to_line(&chars[start..next]));
        start = next;
        while start < chars.len() && chars[start].value.is_whitespace() {
            start += 1;
        }
    }
    wrapped
}

fn agent_markdown_lines(text: &str, wrap_width: usize, dot_style: Style) -> Vec<Line<'static>> {
    let options = MarkdownOptions::new(AgentMarkdownStyleSheet);
    let markdown = tui_markdown::from_str_with_options(text.trim_start_matches('\n'), &options);
    let body_width = wrap_width.saturating_sub(2).max(1);
    let mut lines = Vec::new();
    let mut first_row = true;

    for logical_line in markdown.lines {
        for mut line in wrap_markdown_line(logical_line, body_width) {
            if line.width() == 0 {
                if !first_row {
                    lines.push(Line::default());
                }
                continue;
            }

            let mut spans = Vec::with_capacity(line.spans.len() + 1);
            if first_row {
                spans.push(Span::styled("● ", dot_style));
                first_row = false;
            } else {
                spans.push(Span::raw("  "));
            }
            spans.append(&mut line.spans);
            lines.push(Line::from(spans));
        }
    }
    lines
}

#[cfg(test)]
fn message_height(msg: &ChatMessage, wrap_width: usize) -> usize {
    rendered_lines_height(
        &build_message_lines(msg, false, false, None, 0, wrap_width),
        wrap_width,
    )
}

#[cfg(test)]
fn turn_height(turn: &CompletedTurn, wrap_width: usize) -> usize {
    rendered_lines_height(
        &build_completed_turn_lines(turn, false, false, wrap_width),
        wrap_width,
    )
}

fn rendered_lines_height(lines: &[Line<'_>], wrap_width: usize) -> usize {
    let width = wrap_width.max(1);
    lines
        .iter()
        .map(|line| {
            let text = match line.spans.as_slice() {
                [] => return 1,
                [span] => Cow::Borrowed(span.content.as_ref()),
                spans => Cow::Owned(
                    spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect::<String>(),
                ),
            };
            let display_width = UnicodeWidthStr::width(text.as_ref());
            if display_width == 0 {
                1
            } else if display_width <= width {
                1
            } else {
                textwrap::wrap(text.as_ref(), width).len().max(1)
            }
        })
        .sum()
}

fn starts_with_ignore_ascii_case(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
}

fn tool_call_presentation(status: &str) -> (&'static str, Style, Option<&str>) {
    if status.eq_ignore_ascii_case("pending") {
        ("○", theme::TOOL_CALL_PENDING, None)
    } else if status.eq_ignore_ascii_case("inprogress") || status.eq_ignore_ascii_case("running") {
        ("●", theme::TOOL_CALL_RUNNING, None)
    } else if status.eq_ignore_ascii_case("completed") || status.eq_ignore_ascii_case("exited (0)")
    {
        ("✓", theme::TOOL_CALL_SUCCESS, None)
    } else if status.eq_ignore_ascii_case("failed") {
        ("✗", theme::TOOL_CALL_FAILURE, None)
    } else if let Some((kind, reason)) = status.split_once(':') {
        if kind.eq_ignore_ascii_case("failed") {
            ("✗", theme::TOOL_CALL_FAILURE, Some(reason.trim()))
        } else {
            ("•", theme::DIM, Some(status))
        }
    } else if starts_with_ignore_ascii_case(status, "exited (") {
        ("✗", theme::TOOL_CALL_FAILURE, Some(status))
    } else if status.eq_ignore_ascii_case("cancelled") || status.eq_ignore_ascii_case("canceled") {
        ("−", theme::TOOL_CALL_CANCELED, None)
    } else {
        ("•", theme::DIM, Some(status))
    }
}

fn is_active_tool_call_status(status: &str) -> bool {
    status.eq_ignore_ascii_case("pending")
        || status.eq_ignore_ascii_case("inprogress")
        || status.eq_ignore_ascii_case("running")
}

fn should_show_turn_activity(tab: &crate::app::TabSession) -> bool {
    tab.should_show_thinking()
}

pub(crate) fn should_show_activity(app: &App) -> bool {
    matches!(app.state, crate::app::ConnectionState::Connecting(_))
        || should_show_turn_activity(app.current_tab())
}

fn permission_tool_call_id(tab: &crate::app::TabSession) -> Option<&str> {
    tab.permission
        .front()
        .map(|permission| permission.tool_call_id.as_str())
}

fn breathing_dot(frame: usize) -> &'static str {
    match frame % crate::ui::ACTIVITY_CYCLE_FRAMES {
        0..=4 => "●",
        5..=8 => "•",
        9..=13 => "·",
        _ => "•",
    }
}

pub fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    let render_started = std::time::Instant::now();

    let inner = Block::default().borders(Borders::NONE);
    let inner_area = inner.inner(area);
    let visible_height = inner_area.height as usize;
    let wrap_width = inner_area.width as usize;
    let selection_pending = app.current_tab().completed_turn_selection_visible_pending;
    let selection_target_idx = selection_pending
        .then_some(app.current_tab().selected_completed_turn_idx)
        .flatten()
        .filter(|index| *index < app.current_tab().completed_turns.len());
    let mut effective_offset = app.current_tab().chat_scroll.offset;
    let mut requested_lines = visible_height
        .saturating_add(effective_offset)
        .saturating_add(32);

    let mut reversed_lines: Vec<Line> = Vec::new();
    let mut turn_hit_offsets = Vec::new();

    let mut pending_lines = build_pending_stream_lines(app, wrap_width);
    reversed_lines.extend(pending_lines.drain(..).rev());

    let mut truncated = false;

    let tab = app.current_tab();
    let permission_tool_call_id = permission_tool_call_id(tab);
    let streaming_index = tab.streaming_agent_message_index();
    for (idx, msg) in tab.messages.iter().enumerate().rev() {
        if Some(idx) == streaming_index {
            continue;
        }
        let is_last_message = idx + 1 == tab.messages.len();
        let mut message_lines = build_message_lines(
            msg,
            is_last_message,
            tab.turn.is_streaming(),
            permission_tool_call_id,
            tab.activity_frame,
            wrap_width,
        );
        reversed_lines.extend(message_lines.drain(..).rev());
        if reversed_lines.len() >= requested_lines && selection_target_idx.is_none() {
            truncated = true;
            break;
        }
    }

    if !truncated {
        let selected_idx = app.current_tab().selected_completed_turn_idx;
        let pane_focused = app.pane_focused;
        let mut selection_reached = selection_target_idx.is_none();
        let mut rendered_rows_below = rendered_lines_height(&reversed_lines, wrap_width);
        for (idx, turn) in app.current_tab().completed_turns.iter().enumerate().rev() {
            let is_selected = selected_idx == Some(idx);
            let (mut turn_lines, prompt_rows) = build_completed_turn_lines_with_prompt_rows(
                turn,
                is_selected,
                pane_focused,
                wrap_width,
            );
            let turn_height = rendered_lines_height(&turn_lines, wrap_width);
            turn_hit_offsets.push((
                idx,
                rendered_rows_below,
                turn_height,
                turn.expanded,
                prompt_rows,
            ));
            if selection_target_idx == Some(idx) {
                let selected_end = rendered_rows_below.saturating_add(turn_height);
                let viewport_height = visible_height.max(1);
                effective_offset = if rendered_rows_below < effective_offset {
                    rendered_rows_below
                } else if selected_end > effective_offset.saturating_add(viewport_height) {
                    selected_end.saturating_sub(viewport_height)
                } else {
                    effective_offset
                };
                requested_lines = visible_height
                    .saturating_add(effective_offset)
                    .saturating_add(32);
                selection_reached = true;
            }
            reversed_lines.extend(turn_lines.drain(..).rev());
            rendered_rows_below = rendered_rows_below.saturating_add(turn_height);
            if reversed_lines.len() >= requested_lines && selection_reached {
                truncated = true;
                break;
            }
        }
    }

    // First-run welcome: shown once until user sends first message
    if app.show_welcome_hint && app.state == crate::app::ConnectionState::Connected {
        let mut welcome_lines = vec![Line::from(vec![
            Span::styled(
                "● ",
                Style::new().fg(Color::Reset).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                t!("chat.welcome_title").into_owned(),
                Style::new().fg(Color::Reset).add_modifier(Modifier::BOLD),
            ),
        ])];
        reversed_lines.extend(welcome_lines.drain(..).rev());
    }

    let lines: Vec<Line> = reversed_lines.into_iter().rev().collect();

    let total_lines = rendered_lines_height(&lines, wrap_width);
    let scroll = total_lines.saturating_sub(visible_height.saturating_add(effective_offset));

    let paragraph = Paragraph::new(lines)
        .block(inner)
        .alignment(crate::rtl::text_alignment())
        .wrap(Wrap { trim: false })
        .scroll((scroll.min(u16::MAX as usize) as u16, 0));

    frame.render_widget(paragraph, area);

    let mut completed_turn_hits = Vec::new();
    let buffer = frame.buffer_mut();
    for (turn_index, rows_below, turn_height, expanded, prompt_rows) in turn_hit_offsets {
        let header_from_top = total_lines.saturating_sub(rows_below.saturating_add(turn_height));
        if let Some(header_row) = header_from_top
            .checked_sub(scroll)
            .filter(|row| *row < visible_height)
        {
            let row = inner_area.y.saturating_add(header_row as u16);
            let symbol = if expanded { "▼" } else { "▶" };
            if let Some(column) = (inner_area.x..inner_area.x.saturating_add(inner_area.width))
                .find(|column| {
                    buffer
                        .cell((*column, row))
                        .is_some_and(|cell| cell.symbol() == symbol)
                })
            {
                completed_turn_hits.push(crate::app::CompletedTurnHitRegion {
                    start_column: column,
                    end_column: column.saturating_add(1),
                    row,
                    turn_index,
                    kind: crate::app::CompletedTurnHitKind::Triangle,
                });
            }
        }

        for prompt_row in prompt_rows {
            let Some(visible_row) = header_from_top
                .saturating_add(prompt_row.row_offset)
                .checked_sub(scroll)
            else {
                continue;
            };
            if visible_row >= visible_height {
                continue;
            }
            let start = inner_area.x as usize;
            let end = inner_area.x.saturating_add(inner_area.width) as usize;
            if start < end {
                completed_turn_hits.push(crate::app::CompletedTurnHitRegion {
                    start_column: start as u16,
                    end_column: end as u16,
                    row: inner_area.y.saturating_add(visible_row as u16),
                    turn_index,
                    kind: crate::app::CompletedTurnHitKind::UserInput,
                });
                app.completed_turn_action_links.push(
                    crate::action_links::CompletedTurnActionLink {
                        start_column: start as u16,
                        end_column: end as u16,
                        row: inner_area.y.saturating_add(visible_row as u16),
                        action: if expanded {
                            crate::action_links::CompletedTurnAction::Collapse
                        } else {
                            crate::action_links::CompletedTurnAction::Expand
                        },
                    },
                );
            }
        }
    }
    app.completed_turn_hits = completed_turn_hits;

    if selection_pending {
        let tab = app.current_tab_mut();
        tab.chat_scroll.offset = effective_offset;
        tab.completed_turn_selection_visible_pending = false;
    }

    // Update the scroll bound only when the build saw all of history;
    // otherwise the true max is still unknown and the stored value (possibly
    // stale) is the best we have. Either way `Scroll::by` itself doesn't
    // clamp, so wheel-up keeps working even with a stale bound.
    if !truncated {
        app.current_tab_mut()
            .chat_scroll
            .set_max(total_lines.saturating_sub(visible_height));
    }

    ui_trace::log_slow("chat_render", render_started.elapsed(), || {
        format!(
            "messages={} pending_chars={} requested_lines={} visible_height={} area={}x{}",
            app.current_tab().messages.len(),
            app.current_tab()
                .streaming_agent_text()
                .map(|text| text.chars().count())
                .unwrap_or(0),
            requested_lines,
            visible_height,
            area.width,
            area.height
        )
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PromptRowGeometry {
    row_offset: usize,
    line_width: usize,
    body_start: usize,
    body_width: usize,
}

fn completed_turn_prompt_rows(lines: &[Line<'_>], wrap_width: usize) -> Vec<PromptRowGeometry> {
    let width = wrap_width.max(1);
    let mut rows = Vec::new();
    for line in lines {
        let line_width = line
            .spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum::<usize>();
        let body_start = line
            .spans
            .iter()
            .take(2)
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum::<usize>();
        if line_width <= width {
            rows.push(PromptRowGeometry {
                row_offset: rows.len(),
                line_width,
                body_start,
                body_width: line_width.saturating_sub(body_start),
            });
            continue;
        }

        let text = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        let pieces = textwrap::wrap(&text, width);
        if pieces.is_empty() {
            continue;
        }
        for (piece_index, piece) in pieces.into_iter().enumerate() {
            let line_width = UnicodeWidthStr::width(piece.as_ref()).min(width);
            let body_start = if piece_index == 0 {
                body_start.min(line_width)
            } else {
                0
            };
            rows.push(PromptRowGeometry {
                row_offset: rows.len(),
                line_width,
                body_start,
                body_width: line_width.saturating_sub(body_start),
            });
        }
    }
    rows
}

fn build_completed_turn_lines<'a>(
    turn: &'a crate::app::CompletedTurn,
    is_selected: bool,
    pane_focused: bool,
    wrap_width: usize,
) -> Vec<Line<'a>> {
    build_completed_turn_lines_with_prompt_rows(turn, is_selected, pane_focused, wrap_width).0
}

fn build_completed_turn_lines_with_prompt_rows<'a>(
    turn: &'a crate::app::CompletedTurn,
    is_selected: bool,
    pane_focused: bool,
    wrap_width: usize,
) -> (Vec<Line<'a>>, Vec<PromptRowGeometry>) {
    #[cfg(test)]
    record_completed_turn_line_build();

    let chevron = if turn.expanded { "▼ " } else { "▶ " };
    // Selected row highlights the current Tab target. When the pane is focused
    // it's the live, active selection (bright SELECTED bar); when the pane is
    // unfocused the selection is preserved but muted (SELECTED_INACTIVE), so
    // it reads as "not active" and matches the hidden caret. Unselected rows
    // render in the standard dim USER_PROMPT style.
    let selected_style = if pane_focused {
        theme::SELECTED
    } else {
        theme::SELECTED_INACTIVE
    };
    let prompt_style = if is_selected {
        selected_style
    } else {
        theme::USER_PROMPT
    };
    let chevron_style = if is_selected {
        selected_style
    } else {
        theme::DIM
    };

    let mut lines = if turn.expanded {
        let mut prompt_lines = Vec::new();
        push_prompt_prefixed_lines(
            &mut prompt_lines,
            &turn.prompt,
            wrap_width.saturating_sub(2).max(1),
        );
        for (index, line) in prompt_lines.iter_mut().enumerate() {
            for span in &mut line.spans {
                span.style = prompt_style;
            }
            line.spans.insert(
                0,
                if index == 0 {
                    Span::styled(chevron, chevron_style)
                } else {
                    Span::styled("  ", chevron_style)
                },
            );
        }
        prompt_lines
    } else {
        // Collapsed turns are a single-line summary. Replace embedded newlines
        // with spaces so Ratatui does not run adjacent source lines together.
        let collapsed_prompt = collapse_newlines_for_preview(&turn.prompt);
        let prompt_text: Cow<'a, str> = match collapsed_prompt {
            Cow::Borrowed(_) => truncate_render_text(&turn.prompt),
            Cow::Owned(collapsed) => match truncate_render_text(&collapsed) {
                Cow::Borrowed(_) => Cow::Owned(collapsed),
                Cow::Owned(truncated) => Cow::Owned(truncated),
            },
        };
        vec![Line::from(vec![
            Span::styled(chevron, chevron_style),
            Span::styled("> ", prompt_style),
            Span::styled(prompt_text, prompt_style),
        ])]
    };

    let prompt_rows = completed_turn_prompt_rows(&lines, wrap_width);

    // Index of the line that should receive an inline trailing marker (eg
    // "(canceled)" / "→ executed: …"). Expanded turns attach it to the
    // first detail row (after all expanded prompt rows); collapsed turns
    // put it next to the prompt header.
    let marker_target_idx = if turn.expanded && !turn.details.is_empty() {
        Some(lines.len())
    } else {
        Some(0)
    };

    if turn.expanded {
        // Render the captured details — the agent reply, tool calls,
        // plans, etc. — using the same builder as the active turn so the
        // formatting matches. `is_last_message=false` and
        // `agent_streaming=false` together suppress the streaming-cursor
        // path; details are always finalized by the time they land here.
        for msg in turn.details.iter() {
            lines.extend(build_message_lines_with_details(
                msg, false, false, None, 0, wrap_width, true,
            ));
        }
    }

    if let (Some(marker), Some(idx)) = (turn.trailing_marker.as_deref(), marker_target_idx) {
        if let Some(line) = lines.get_mut(idx) {
            line.spans.push(Span::raw("  "));
            line.spans.push(Span::styled(marker, theme::DIM));
        }
    }

    // Push a trailing blank only if the last detail (or the prompt header
    // for collapsed turns) didn't already supply one. Agent / Error /
    // System / Plan / AgentEvent trail a blank via build_message_lines.
    // ToolCall only does so when it renders command details; collapsed
    // turns stop at the prompt header.
    if lines.last().map_or(true, |l| !l.spans.is_empty()) {
        lines.push(Line::default());
    }
    (lines, prompt_rows)
}

pub fn render_activity(frame: &mut Frame, app: &App, area: Rect) {
    // While the helper is still establishing its connection to the agent,
    // show an animated "Connecting to agent…" line (F7). The handshake
    // (pipe connect → ACP init → session/new) can take tens of seconds on a
    // cold start; without an animated indicator the pane looked frozen. Uses
    // the app-level `activity_frame`, which is advanced on Tick while the
    // state is `Connecting` (see handle_event). Takes precedence over the
    // turn spinner because no turn can be in flight before we're connected.
    if matches!(app.state, crate::app::ConnectionState::Connecting(_)) {
        let label = t!("connection.connecting_activity").into_owned();
        let line = Line::from(shimmer::shimmer_spans(&label, app.activity_frame as usize));
        frame.render_widget(Paragraph::new(line), area);
        return;
    }
    let tab = app.current_tab();
    if !should_show_turn_activity(tab) {
        return;
    }
    let label = activity_label();
    let line = Line::from(shimmer::shimmer_spans(&label, tab.activity_frame));
    frame.render_widget(Paragraph::new(line), area);
}

/// Return non-empty assistant text for streaming and transcript rendering.
/// Typed proposal payloads travel through the direct Helper channel, so ACP
/// assistant text is always user-visible chat content.
pub(crate) fn user_visible_stream_text(text: &str) -> Option<Cow<'_, str>> {
    (!text.trim().is_empty()).then_some(Cow::Borrowed(text))
}

fn pending_render_text(tab: &crate::app::TabSession) -> Option<Cow<'_, str>> {
    user_visible_stream_text(tab.streaming_agent_text()?)
}

fn build_pending_stream_lines<'a>(app: &App, wrap_width: usize) -> Vec<Line<'a>> {
    let tab = app.current_tab();
    let Some(text) = pending_render_text(tab) else {
        return Vec::new();
    };
    // Typewriter smoothing: only reveal the first `reveal_chars` characters of
    // the streaming text. The reveal cursor is advanced toward the full length
    // by the `RevealTick` animation (`App::advance_reveal`), turning the
    // upstream ~90-char-every-~100ms bursts into a smooth character flow. The
    // full text is always in the ordered transcript, and finalize moves that
    // transcript to history unchanged.
    let revealed: Cow<'_, str> = {
        let total = text.chars().count();
        let shown = tab.reveal_chars.max(1).min(total);
        if shown >= total {
            text
        } else {
            Cow::Owned(text.chars().take(shown).collect())
        }
    };
    let mut lines = Vec::new();
    push_dot_prefixed_lines(
        &mut lines,
        &revealed,
        wrap_width,
        theme::DOT_AGENT,
        theme::AGENT_TEXT,
    );
    lines
}

fn build_message_lines<'a>(
    msg: &'a ChatMessage,
    is_last_message: bool,
    agent_streaming: bool,
    permission_tool_call_id: Option<&str>,
    activity_frame: usize,
    wrap_width: usize,
) -> Vec<Line<'a>> {
    build_message_lines_with_details(
        msg,
        is_last_message,
        agent_streaming,
        permission_tool_call_id,
        activity_frame,
        wrap_width,
        false,
    )
}

fn build_message_lines_with_details<'a>(
    msg: &'a ChatMessage,
    is_last_message: bool,
    agent_streaming: bool,
    permission_tool_call_id: Option<&str>,
    activity_frame: usize,
    wrap_width: usize,
    detailed_tools: bool,
) -> Vec<Line<'a>> {
    let mut lines = Vec::new();
    match msg {
        ChatMessage::User(text) => {
            push_prompt_prefixed_lines(&mut lines, text, wrap_width);
            lines.push(Line::default());
        }
        ChatMessage::Agent(text) => {
            lines.extend(agent_markdown_lines(text, wrap_width, theme::DOT_AGENT));
            if !agent_streaming || !is_last_message {
                lines.push(Line::default());
            }
        }
        ChatMessage::System(text) => {
            for line_text in text.lines() {
                lines.push(Line::from(Span::styled(
                    truncate_render_text(line_text),
                    theme::SYSTEM_TEXT,
                )));
            }
            lines.push(Line::default());
        }
        ChatMessage::Notice { kind, text } => {
            let (marker, style) = match kind {
                NoticeKind::Success => ("✓", theme::NOTICE_SUCCESS),
                NoticeKind::Info => ("i", theme::NOTICE_INFO),
                NoticeKind::Warning => ("!", theme::NOTICE_WARNING),
                NoticeKind::Error => ("×", theme::NOTICE_ERROR),
            };
            push_prefixed_lines(&mut lines, marker, text, wrap_width, style);
            lines.push(Line::default());
        }
        ChatMessage::ToolCall {
            id,
            title,
            status,
            kind,
            location,
            location_is_command,
            cwd,
            output,
            exit_code,
            content,
            locations,
        } => {
            let (marker, marker_style, detail) = tool_call_presentation(status);
            let marker = if permission_tool_call_id == Some(id.as_str())
                || is_active_tool_call_status(status)
            {
                breathing_dot(activity_frame)
            } else {
                marker
            };
            let mut spans = vec![
                Span::styled(marker, marker_style),
                Span::raw(" "),
                Span::styled(truncate_render_text(title), theme::TOOL_CALL_TITLE),
            ];
            let location = location.as_deref().filter(|l| !l.is_empty());
            // Path hint pulled from the ACP `locations`/`raw_input` fields
            // (see `client.rs::tool_call_location_hint`) — surfaces *what*
            // the tool touched, which the agent's `title` alone often
            // doesn't (e.g. a generic "Access paths outside trusted
            // directories" permission title). Rendered inline since a
            // path is normally short enough to fit on the title's line.
            if !location_is_command {
                if let Some(location) = location {
                    spans.push(Span::styled(
                        format!(" ({})", truncate_render_text(location)),
                        theme::DIM,
                    ));
                }
            }
            if *kind == ToolCallKind::Execute {
                if let Some(cwd) = cwd
                    .as_deref()
                    .filter(|cwd| !cwd.is_empty())
                    .filter(|cwd| !title.contains(cwd))
                {
                    spans.push(Span::styled(
                        format!(" ({})", truncate_render_text(cwd)),
                        theme::DIM,
                    ));
                }
            }
            if let Some(detail) = detail.filter(|detail| !detail.is_empty()) {
                spans.push(Span::styled(
                    format!(" · {}", truncate_render_text(detail)),
                    theme::DIM,
                ));
            }
            if !detailed_tools && (*kind == ToolCallKind::Execute || *location_is_command) {
                if let Some(exit_code) = exit_code.filter(|_| {
                    !starts_with_ignore_ascii_case(status, "exited (")
                        && !starts_with_ignore_ascii_case(status, "failed:")
                }) {
                    spans.push(Span::styled(format!(" · exit {exit_code}"), theme::DIM));
                }
            }
            lines.push(Line::from(spans));
            // A command target can be several `;`-chained PowerShell
            // statements crammed into one `raw_input.command` (agents
            // commonly batch multiple checks into a single tool call) —
            // rendering that as one long line, which then wraps at the
            // terminal edge with no hanging indent, reads as an
            // unreadable wall of text. Splitting on top-level `;`
            // restores the sequence of discrete steps, one per code-
            // styled line (mirrors how `execute`-kind cards look in Zed /
            // opencode); a long remainder folds into a single "+N more"
            // row instead of growing the card unboundedly.
            let mut rendered_command = false;
            if *location_is_command {
                if let Some(command) = location {
                    for entry in crate::ui::command_format::command_display_lines(command) {
                        rendered_command = true;
                        lines.push(Line::from(Span::styled(
                            entry.rendered_text("    "),
                            theme::CARD_CODE,
                        )));
                    }
                }
            }
            let mut rendered_output = false;
            if !detailed_tools && (*kind == ToolCallKind::Execute || *location_is_command) {
                if let Some(output) = output {
                    for line in tool_output_lines(output) {
                        rendered_output = true;
                        lines.push(Line::from(Span::styled(
                            format!("    │ {line}"),
                            theme::DIM,
                        )));
                    }
                }
            }
            let has_text_content = content
                .iter()
                .any(|item| matches!(item, ToolCallContent::Text(_)));
            let mut detail_lines = tool_detail_lines(content, locations, detailed_tools);
            if !has_text_content {
                if let Some(output) = output {
                    if detailed_tools {
                        detail_lines.extend(full_output_lines(output, "    │ "));
                    } else if *kind != ToolCallKind::Execute && !*location_is_command {
                        detail_lines.extend(preview_output_lines(output, "    │ "));
                    }
                }
            }
            let rendered_details = !detail_lines.is_empty();
            for line in detail_lines {
                lines.push(Line::from(Span::styled(line, theme::DIM)));
            }
            if rendered_command || rendered_output || rendered_details {
                lines.push(Line::default());
            }
        }
        ChatMessage::Plan(entries) => {
            lines.push(Line::from(Span::styled(
                t!("chat.plan_header").into_owned(),
                theme::PLAN_STYLE,
            )));
            for entry in entries {
                let marker = match entry.status {
                    PlanEntryStatus::Completed => t!("chat.plan_marker_completed").into_owned(),
                    PlanEntryStatus::InProgress => t!("chat.plan_marker_in_progress").into_owned(),
                    PlanEntryStatus::Pending => t!("chat.plan_marker_pending").into_owned(),
                };
                lines.push(Line::from(Span::styled(
                    format!("  {} {}", marker, truncate_render_text(&entry.content)),
                    theme::PLAN_STYLE,
                )));
            }
            lines.push(Line::default());
        }
        ChatMessage::Error(text) => {
            push_dot_prefixed_lines(
                &mut lines,
                text,
                wrap_width,
                theme::DOT_ERROR,
                theme::ERROR_STYLE,
            );
            lines.push(Line::default());
        }
        ChatMessage::AgentEvent(text) => {
            for (i, line_text) in text.lines().enumerate() {
                if i == 0 {
                    lines.push(Line::from(Span::styled(
                        truncate_render_text(line_text),
                        theme::AGENT_EVENT_HEADER,
                    )));
                } else {
                    lines.push(Line::from(Span::styled(
                        truncate_render_text(line_text),
                        theme::AGENT_EVENT_DETAIL,
                    )));
                }
            }
            lines.push(Line::default());
        }
        ChatMessage::Disclaimer => {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    t!("chat.welcome_disclaimer").into_owned(),
                    Style::new().fg(Color::Reset).add_modifier(Modifier::BOLD),
                ),
            ]));
        }
    }
    lines
}

// Render a multi-line text block with a colored dot prefix on the first
// visual row and a 2-cell hanging indent on every continuation row (both
// for explicit \n breaks AND for soft-wrapped continuations of long
// paragraphs). Without this, ratatui's Paragraph word-wrap pushes
// continuation rows back to column 0 and the bullet alignment breaks.
fn push_dot_prefixed_lines<'a>(
    lines: &mut Vec<Line<'a>>,
    text: &str,
    wrap_width: usize,
    dot_style: Style,
    text_style: Style,
) {
    // Reserve 2 cells for either "● " or the continuation indent.
    let body_width = wrap_width.saturating_sub(2).max(1);
    let mut first_row = true;

    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            // Skip leading blanks so the dot lands on the first content row
            // — many models prefix prose with `\n` / `\n\n`, which would
            // otherwise burn the dot on an empty line. Blank lines between
            // paragraphs are still preserved.
            if first_row {
                continue;
            }
            lines.push(Line::default());
            continue;
        }

        let wrapped = textwrap::wrap(paragraph, body_width);
        for piece in wrapped {
            let piece_str = truncate_render_text(&piece).into_owned();
            if first_row {
                lines.push(Line::from(vec![
                    Span::styled("● ", dot_style),
                    Span::styled(piece_str, text_style),
                ]));
                first_row = false;
            } else {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(piece_str, text_style),
                ]));
            }
        }
    }
}

fn push_prefixed_lines<'a>(
    lines: &mut Vec<Line<'a>>,
    marker: &'static str,
    text: &str,
    wrap_width: usize,
    style: Style,
) {
    let body_width = wrap_width.saturating_sub(2).max(1);
    let mut first_row = true;

    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            if first_row {
                continue;
            }
            lines.push(Line::default());
            continue;
        }

        for piece in textwrap::wrap(paragraph, body_width) {
            let piece_str = truncate_render_text(&piece).into_owned();
            if first_row {
                lines.push(Line::from(vec![
                    Span::styled(format!("{marker} "), style),
                    Span::styled(piece_str, style),
                ]));
                first_row = false;
            } else {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(piece_str, style),
                ]));
            }
        }
    }
}

/// Mirrors `push_dot_prefixed_lines`, but for the user's own submitted
/// prompt: splits on embedded `\n` (from Shift+Enter multi-line input) and
/// wraps each paragraph so every line is a real `ratatui::Line` — ratatui
/// does not turn an embedded `\n` inside a single `Span`/`Line` into
/// multiple rows, so without this split any line after the first would
/// never appear in the rendered transcript (see issue #492). The first
/// rendered row gets the `"> "` prompt marker; continuation rows get a
/// matching 2-cell indent. Height measurement consumes these same rendered
/// lines and counts their terminal display width.
fn push_prompt_prefixed_lines<'a>(lines: &mut Vec<Line<'a>>, text: &'a str, wrap_width: usize) {
    let body_width = wrap_width.saturating_sub(2).max(1);
    let mut first_row = true;

    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            // Unlike `push_dot_prefixed_lines`, the prompt marker must never
            // be dropped: an empty submitted prompt, or one starting with a
            // newline, still needs a "> " row so the transcript shows the
            // user turn happened at all.
            if first_row {
                lines.push(Line::from(Span::styled("> ", theme::USER_PROMPT)));
                first_row = false;
            } else {
                lines.push(Line::default());
            }
            continue;
        }

        // `textwrap::wrap` borrows from `paragraph` (itself borrowed from the
        // `'a` input) whenever a piece needs no reflowing, so the typical
        // short single-line prompt renders with zero allocations here;
        // `truncate_render_cow` preserves that borrow unless the piece is
        // actually rewrapped or exceeds `MAX_RENDER_LINE_CHARS`.
        let wrapped = textwrap::wrap(paragraph, body_width);
        for piece in wrapped {
            let piece_str = truncate_render_cow(piece);
            if first_row {
                lines.push(Line::from(vec![
                    Span::styled("> ", theme::USER_PROMPT),
                    Span::styled(piece_str, theme::USER_PROMPT),
                ]));
                first_row = false;
            } else {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(piece_str, theme::USER_PROMPT),
                ]));
            }
        }
    }
}

/// Applies `truncate_render_text`'s length cap to an already-computed
/// `Cow`, without forcing an allocation when the input is borrowed and
/// under the limit (unlike `truncate_render_text(&cow).into_owned()`).
fn truncate_render_cow<'a>(text: Cow<'a, str>) -> Cow<'a, str> {
    match text {
        Cow::Borrowed(s) => truncate_render_text(s),
        Cow::Owned(s) => match truncate_render_text(&s) {
            Cow::Borrowed(_) => Cow::Owned(s),
            Cow::Owned(truncated) => Cow::Owned(truncated),
        },
    }
}

/// Collapses embedded newlines (from a Shift+Enter multi-line prompt) into
/// single spaces so a single-line preview (the folded completed-turn header)
/// doesn't silently run separate lines together with no visible separator.
fn collapse_newlines_for_preview(text: &str) -> Cow<'_, str> {
    if !text.contains('\n') {
        return Cow::Borrowed(text);
    }
    Cow::Owned(text.replace('\n', " "))
}

fn truncate_render_text(text: &str) -> Cow<'_, str> {
    let char_count = text.chars().count();
    if char_count <= MAX_RENDER_LINE_CHARS {
        return Cow::Borrowed(text);
    }

    let head_chars = MAX_RENDER_LINE_CHARS * 3 / 4;
    let tail_chars = MAX_RENDER_LINE_CHARS / 4;
    let omitted = char_count.saturating_sub(head_chars + tail_chars);
    let head: String = text.chars().take(head_chars).collect();
    let tail: String = text
        .chars()
        .skip(char_count.saturating_sub(tail_chars))
        .collect();

    Cow::Owned(format!("{head} ...<{omitted} chars omitted>... {tail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn completed_turn_build_counter_does_not_leak_across_threads() {
        reset_completed_turn_line_build_count();
        std::thread::spawn(|| {
            reset_completed_turn_line_build_count();
            record_completed_turn_line_build();
            assert_eq!(completed_turn_line_build_count(), 1);
        })
        .join()
        .expect("counter thread must finish");

        assert_eq!(completed_turn_line_build_count(), 0);
    }

    #[test]
    fn notices_render_distinct_markers_and_hanging_indents() {
        let cases = [
            (NoticeKind::Success, "✓"),
            (NoticeKind::Info, "i"),
            (NoticeKind::Warning, "!"),
            (NoticeKind::Error, "×"),
        ];

        for (kind, marker) in cases {
            let message = ChatMessage::Notice {
                kind,
                text: "A notice that wraps onto another line".into(),
            };
            let lines = build_message_lines(&message, false, false, None, 0, 20);
            assert!(line_text(&lines[0]).starts_with(&format!("{marker} ")));
            assert!(
                line_text(&lines[1]).starts_with("  "),
                "continuation rows must align with the notice body"
            );
            assert!(line_text(lines.last().expect("trailing row")).is_empty());
        }
    }

    #[test]
    fn notice_prefix_skips_leading_blank_lines() {
        let message = ChatMessage::info("\n\nNotice text");
        let lines = build_message_lines(&message, false, false, None, 0, 20);

        assert_eq!(line_text(&lines[0]), "i Notice text");
        assert_eq!(lines.len(), message_height(&message, 20));
    }

    #[test]
    fn message_height_uses_terminal_display_width_for_cjk() {
        let message = ChatMessage::Agent("你好".into());
        let lines = build_message_lines(&message, false, false, None, 0, 4);

        assert_eq!(lines.len(), 3, "two CJK glyphs wrap into two body rows");
        assert_eq!(message_height(&message, 4), lines.len());
    }

    #[test]
    fn rendered_height_accounts_for_word_wrap_gaps() {
        let lines = vec![Line::from("aaa aaa aaa aaa")];

        assert_eq!(rendered_lines_height(&lines, 5), 4);
    }

    #[test]
    fn agent_message_renders_multiline_markdown_with_theme_relative_styles() {
        let message =
            ChatMessage::Agent("# Heading\n\nFirst **bold** line\n\nSecond paragraph".into());

        let lines = build_message_lines(&message, false, false, None, 0, 80);

        assert_eq!(line_text(&lines[0]), "● Heading");
        assert!(lines
            .iter()
            .any(|line| line_text(line) == "  Second paragraph"));
        assert!(lines.iter().flat_map(|line| &line.spans).any(|span| {
            span.content == "bold"
                && span.style.fg == Some(Color::Reset)
                && span.style.add_modifier.contains(Modifier::BOLD)
        }));
    }

    #[test]
    fn expanded_turn_height_matches_rendered_detail_endings() {
        let cases = [
            (
                "agent text",
                vec![ChatMessage::Agent(
                    "I checked the working tree and found one change.".into(),
                )],
            ),
            (
                "compact tool call",
                vec![ChatMessage::ToolCall {
                    id: "tool".into(),
                    title: "Read source".into(),
                    status: "Completed".into(),
                    kind: ToolCallKind::Read,
                    location: Some(r"C:\src\main.rs".into()),
                    location_is_command: false,
                    cwd: None,
                    output: None,
                    exit_code: None,
                    content: Vec::new(),
                    locations: Vec::new(),
                }],
            ),
            (
                "command tool call",
                vec![ChatMessage::ToolCall {
                    id: "tool".into(),
                    title: "Run tests".into(),
                    status: "Completed".into(),
                    kind: ToolCallKind::Execute,
                    location: Some("cargo test --workspace".into()),
                    location_is_command: true,
                    cwd: None,
                    output: None,
                    exit_code: None,
                    content: Vec::new(),
                    locations: Vec::new(),
                }],
            ),
            ("disclaimer", vec![ChatMessage::Disclaimer]),
            ("empty details", Vec::new()),
        ];

        for (name, details) in cases {
            let turn = CompletedTurn {
                prompt: "What changed?".into(),
                details,
                expanded: true,
                trailing_marker: None,
            };

            assert_eq!(
                turn_height(&turn, 80),
                build_completed_turn_lines(&turn, false, true, 80).len(),
                "{name}"
            );
        }
    }

    #[test]
    fn expanded_completed_turn_restores_multiline_prompt() {
        let mut turn = CompletedTurn {
            prompt: ["line one", "line two"].join("\n"),
            details: Vec::new(),
            expanded: false,
            trailing_marker: None,
        };

        let collapsed = build_completed_turn_lines(&turn, false, true, 80);
        assert_eq!(line_text(&collapsed[0]), "▶ > line one line two");

        turn.expanded = true;
        let expanded = build_completed_turn_lines(&turn, true, true, 80);
        let texts: Vec<String> = expanded.iter().map(line_text).collect();
        assert_eq!(
            texts,
            vec![
                "▼ > line one".to_string(),
                "    line two".to_string(),
                String::new(),
            ]
        );
        assert_eq!(expanded[1].spans[0].style, theme::SELECTED);
        assert_eq!(turn_height(&turn, 80), expanded.len());
    }

    fn assert_tool_call(
        status: &str,
        expected_text: &str,
        expected_marker_style: Style,
        expected_detail_style: Option<Style>,
    ) {
        let message = ChatMessage::ToolCall {
            id: "tool".into(),
            title: "Run: cargo test".into(),
            status: status.into(),
            kind: ToolCallKind::Other,
            location: None,
            location_is_command: false,
            cwd: None,
            output: None,
            exit_code: None,
            content: Vec::new(),
            locations: Vec::new(),
        };
        let lines = build_message_lines(&message, false, false, None, 0, 80);
        let line = &lines[0];

        assert_eq!(line_text(line), expected_text);
        assert_eq!(line.spans[0].style, expected_marker_style);
        assert_eq!(line.spans[2].style, theme::TOOL_CALL_TITLE);
        assert_eq!(
            line.spans.get(3).map(|span| span.style),
            expected_detail_style
        );
    }

    /// A `location` hint renders as a dim `(path)` suffix right after the
    /// title, before the status detail — guards against the card silently
    /// dropping the path/command info that `client.rs` now forwards.
    #[test]
    fn tool_call_renders_location_hint_between_title_and_status_detail() {
        let message = ChatMessage::ToolCall {
            id: "tool".into(),
            title: "Access paths outside trusted directories".into(),
            status: "Pending".into(),
            kind: ToolCallKind::Other,
            location: Some(r"C:\src\rust-app".into()),
            location_is_command: false,
            cwd: None,
            output: None,
            exit_code: None,
            content: Vec::new(),
            locations: Vec::new(),
        };
        let lines = build_message_lines(&message, false, false, None, 0, 80);
        let line = &lines[0];

        assert_eq!(
            line_text(line),
            r"● Access paths outside trusted directories (C:\src\rust-app)"
        );
        assert_eq!(
            lines.len(),
            1,
            "path-only tool calls should remain compact without a paragraph break"
        );
        assert_eq!(message_height(&message, 80), 1);
    }

    /// A command-kind location (`location_is_command`) must NOT be inlined
    /// as a `(hint)` suffix on the title line — it gets its own
    /// `CARD_CODE`-styled `$ command` line instead, since commands can be
    /// long one-liners that would overflow or wrap awkwardly inline.
    #[test]
    fn tool_call_command_location_renders_as_separate_code_line() {
        let message = ChatMessage::ToolCall {
            id: "tool".into(),
            title: "Run command".into(),
            status: "Pending".into(),
            kind: ToolCallKind::Execute,
            location: Some("cargo test --workspace".into()),
            location_is_command: true,
            cwd: None,
            output: None,
            exit_code: None,
            content: Vec::new(),
            locations: Vec::new(),
        };
        let lines = build_message_lines(&message, false, false, None, 0, 80);

        assert_eq!(
            lines.len(),
            3,
            "expected a title line, command line, and paragraph break"
        );
        assert_eq!(line_text(&lines[0]), "● Run command");
        assert_eq!(line_text(&lines[1]), "    $ cargo test --workspace");
        assert_eq!(lines[1].spans[0].style, theme::CARD_CODE);
        assert!(lines[2].spans.is_empty());

        assert_eq!(
            message_height(&message, 80),
            3,
            "the height budget must account for the extra command line"
        );
    }

    /// A multi-statement command (`;`-chained, the pattern agents commonly
    /// emit when batching several checks into one tool call) must render
    /// as one code-styled line **per statement**, not one giant crammed
    /// line — this was the exact bug reported: `winget list ...; winget
    /// list ...` rendered as a single unreadable wrapped line with
    /// misaligned continuation.
    #[test]
    fn tool_call_multi_statement_command_renders_one_line_per_statement() {
        let message = ChatMessage::ToolCall {
            id: "tool".into(),
            title: "Check installed PowerToys and Foundry Local packages".into(),
            status: "Completed".into(),
            kind: ToolCallKind::Execute,
            location: Some(
                "winget list --name PowerToys 2>$null; winget list --name Foundry 2>$null".into(),
            ),
            location_is_command: true,
            cwd: None,
            output: None,
            exit_code: None,
            content: Vec::new(),
            locations: Vec::new(),
        };
        let lines = build_message_lines(&message, false, false, None, 0, 80);

        assert_eq!(
            lines.len(),
            4,
            "expected a title line, one line per statement, and a paragraph break"
        );
        assert_eq!(
            line_text(&lines[1]),
            "    $ winget list --name PowerToys 2>$null"
        );
        assert_eq!(
            line_text(&lines[2]),
            "    $ winget list --name Foundry 2>$null"
        );

        assert_eq!(
            message_height(&message, 80),
            4,
            "the height budget must count one row per split statement"
        );
    }

    #[test]
    fn execute_tool_call_renders_cwd_reported_output_tail_and_exit_code() {
        let cwd = concat!("C:", "\\", "repo");
        let message = ChatMessage::ToolCall {
            id: "tool".into(),
            title: "bash".into(),
            status: "Completed".into(),
            kind: ToolCallKind::Execute,
            location: Some("cargo test".into()),
            location_is_command: true,
            cwd: Some(cwd.into()),
            output: Some(ToolCallOutput {
                text: ["line 1", "line 2", "line 3", "line 4", "line 5"].join("\n"),
                truncated: false,
            }),
            exit_code: Some(0),
            content: Vec::new(),
            locations: Vec::new(),
        };
        let lines = build_message_lines(&message, false, false, None, 0, 120);
        let rendered: Vec<String> = lines.iter().map(line_text).collect();

        assert_eq!(rendered[0], format!("✓ bash ({cwd}) · exit 0"));
        assert_eq!(rendered[1], "    $ cargo test");
        assert_eq!(rendered[2], "    │ …");
        assert_eq!(rendered[3], "    │ line 2");
        assert_eq!(rendered[6], "    │ line 5");
        assert!(rendered[7].is_empty());
        assert_eq!(lines.len(), message_height(&message, 120));
    }

    #[test]
    fn completed_non_execute_tool_call_shows_bounded_output_preview() {
        let location = concat!("C:", "\\", "repo", "\\", "large.txt");
        let message = ChatMessage::ToolCall {
            id: "tool".into(),
            title: "Read file".into(),
            status: "Completed".into(),
            kind: ToolCallKind::Read,
            location: Some(location.into()),
            location_is_command: false,
            cwd: None,
            output: Some(ToolCallOutput {
                text: ["line 1", "line 2", "line 3", "line 4"].join("\n"),
                truncated: false,
            }),
            exit_code: Some(200),
            content: Vec::new(),
            locations: Vec::new(),
        };
        let lines = build_message_lines(&message, false, false, None, 0, 120);
        let rendered: Vec<String> = lines.iter().map(line_text).collect();

        assert_eq!(rendered[1], "    │ …");
        assert_eq!(rendered[2], "    │ line 3");
        assert_eq!(rendered[3], "    │ line 4");
        assert!(!rendered.iter().any(|line| line.contains("line 1")));
        assert!(!rendered[0].contains("exit 200"));
        assert!(rendered[4].is_empty());
        assert_eq!(lines.len(), message_height(&message, 120));
    }

    #[test]
    fn expanded_tool_output_is_bounded_for_large_file_lists() {
        let output = ToolCallOutput {
            text: (0..200)
                .map(|index| format!("debug/incremental/object-{index:03}.o"))
                .collect::<Vec<_>>()
                .join("\n"),
            truncated: false,
        };
        let lines = tool_detail_lines(&[ToolCallContent::Text(output)], &[], true);

        assert_eq!(lines.len(), MAX_TOOL_DETAIL_OUTPUT_LINES + 1);
        assert_eq!(lines[0], "    │ …");
        assert!(lines
            .last()
            .is_some_and(|line| line.ends_with("object-199.o")));
    }

    #[test]
    fn tool_detail_lines_strictly_caps_locations_including_ellipsis() {
        let locations: Vec<ToolCallLocation> = (0..=MAX_TOOL_DETAIL_LINES)
            .map(|index| ToolCallLocation {
                path: format!("file-{index}.rs"),
                line: None,
            })
            .collect();

        let lines = tool_detail_lines(&[], &locations, true);

        assert_eq!(lines.len(), MAX_TOOL_DETAIL_LINES);
        assert_eq!(lines.last().map(String::as_str), Some("    …"));
    }

    #[test]
    fn tool_call_uses_semantic_status_markers() {
        assert_tool_call(
            "Pending",
            "● Run: cargo test",
            theme::TOOL_CALL_PENDING,
            None,
        );
        assert_tool_call(
            "running",
            "● Run: cargo test",
            theme::TOOL_CALL_RUNNING,
            None,
        );
        assert_tool_call(
            "Completed",
            "✓ Run: cargo test",
            theme::TOOL_CALL_SUCCESS,
            None,
        );
        assert_tool_call(
            "Failed: exit code 1",
            "✗ Run: cargo test · exit code 1",
            theme::TOOL_CALL_FAILURE,
            Some(theme::DIM),
        );
        assert_tool_call(
            "Canceled",
            "− Run: cargo test",
            theme::TOOL_CALL_CANCELED,
            None,
        );
        assert_tool_call(
            "Exited (1)",
            "✗ Run: cargo test · Exited (1)",
            theme::TOOL_CALL_FAILURE,
            Some(theme::DIM),
        );
        // "Exited (0)" is a success alias (distinct from the generic
        // "exited (" failure prefix matched above) and carries no detail.
        assert_tool_call(
            "Exited (0)",
            "✓ Run: cargo test",
            theme::TOOL_CALL_SUCCESS,
            None,
        );
        // Status matching is case-insensitive across the success paths.
        assert_tool_call(
            "COMPLETED",
            "✓ Run: cargo test",
            theme::TOOL_CALL_SUCCESS,
            None,
        );
        assert_tool_call(
            "eXiTeD (0)",
            "✓ Run: cargo test",
            theme::TOOL_CALL_SUCCESS,
            None,
        );
        // Unknown/future statuses fall back to a dim marker with the raw
        // status surfaced as dim detail text, instead of panicking or
        // silently dropping the status.
        assert_tool_call(
            "SomeFutureStatus",
            "• Run: cargo test · SomeFutureStatus",
            theme::DIM,
            Some(theme::DIM),
        );
        assert_ne!(theme::TOOL_CALL_CANCELED, theme::DIM);
    }

    // ── user_visible_stream_text ────────────────────────────────────────────

    #[test]
    fn stream_text_pure_prose_passes_through() {
        assert_eq!(
            user_visible_stream_text("just talking").as_deref(),
            Some("just talking")
        );
    }

    #[test]
    fn stream_text_json_passes_through_verbatim() {
        let text = r#"{"explanation":"why blue","command":"ls"}"#;
        assert_eq!(user_visible_stream_text(text).as_deref(), Some(text));
    }

    #[test]
    fn stream_text_prose_then_fence_passes_through_verbatim() {
        let text = "Here is the plan.\n```json\n{\"choices\":[]}\n```";
        assert_eq!(user_visible_stream_text(text).as_deref(), Some(text));
    }

    #[test]
    fn stream_text_blank_is_none() {
        assert_eq!(user_visible_stream_text("   \n  "), None);
    }

    fn streaming_tab(buf: &str, reveal_chars: usize) -> crate::app::TabSession {
        let mut tab = crate::app::TabSession::default();
        tab.turn = crate::app::TurnState::Streaming {
            prompt: crate::app::SubmittedPrompt {
                id: 1,
                text: "hi".into(),
                submitted_at_unix_s: 0.0,
                context: crate::app::TurnContext::default(),
                autofix: None,
            },
        };
        if !buf.is_empty() {
            tab.messages
                .push(crate::app::ChatMessage::Agent(buf.to_string()));
        }
        tab.reveal_chars = reveal_chars;
        tab
    }

    #[test]
    fn thinking_activity_follows_turn_lifecycle() {
        let mut tab = streaming_tab("", 0);
        assert!(should_show_turn_activity(&tab));

        tab.turn = crate::app::TurnState::Idle;
        assert!(!should_show_turn_activity(&tab));
    }

    #[test]
    fn breathing_dot_shrinks_then_grows() {
        assert_eq!(breathing_dot(0), "●");
        assert_eq!(breathing_dot(5), "•");
        assert_eq!(breathing_dot(9), "·");
        assert_eq!(breathing_dot(14), "•");
        assert_eq!(breathing_dot(crate::ui::ACTIVITY_CYCLE_FRAMES), "●");
    }

    #[test]
    fn permission_animates_only_its_matching_tool_call() {
        let matching = ChatMessage::ToolCall {
            id: "tool-2".into(),
            title: "Read Cargo.toml".into(),
            status: "Completed".into(),
            kind: ToolCallKind::Read,
            location: None,
            location_is_command: false,
            cwd: None,
            output: None,
            exit_code: None,
            content: Vec::new(),
            locations: Vec::new(),
        };
        let other = ChatMessage::ToolCall {
            id: "tool-1".into(),
            title: "Find files".into(),
            status: "Completed".into(),
            kind: ToolCallKind::Search,
            location: None,
            location_is_command: false,
            cwd: None,
            output: None,
            exit_code: None,
            content: Vec::new(),
            locations: Vec::new(),
        };

        let matching_lines = build_message_lines(&matching, false, false, Some("tool-2"), 9, 80);
        let other_lines = build_message_lines(&other, false, false, Some("tool-2"), 9, 80);

        assert_eq!(matching_lines[0].spans[0].content, "·");
        assert_eq!(other_lines[0].spans[0].content, "✓");
    }

    #[test]
    fn active_tool_call_breathes_without_permission() {
        for status in ["Pending", "InProgress", "running"] {
            let message = ChatMessage::ToolCall {
                id: "tool".into(),
                title: "Find files".into(),
                status: status.into(),
                kind: ToolCallKind::Search,
                location: None,
                location_is_command: false,
                cwd: None,
                output: None,
                exit_code: None,
                content: Vec::new(),
                locations: Vec::new(),
            };
            let lines = build_message_lines(&message, false, false, None, 9, 80);
            assert_eq!(lines[0].spans[0].content, "·", "{status} should breathe");
        }
    }

    #[test]
    fn permission_animation_follows_fifo_front() {
        let mut tab = streaming_tab("", 0);
        for id in ["tool-1", "tool-2"] {
            tab.permission.push_back(crate::app::PermissionState {
                tool_call_id: id.into(),
                description: "Allow access?".into(),
                title: "Allow access?".into(),
                kind_label: None,
                target: None,
                target_is_command: false,
                options: Vec::new(),
                selected: 0,
                responder: None,
            });
        }

        assert_eq!(permission_tool_call_id(&tab), Some("tool-1"));
        tab.permission.pop_front();
        assert_eq!(permission_tool_call_id(&tab), Some("tool-2"));
    }

    // ── truncate_render_text ────────────────────────────────────────────────

    #[test]
    fn truncate_passes_short_text_unchanged_borrowed() {
        let s = "short";
        match truncate_render_text(s) {
            Cow::Borrowed(b) => assert_eq!(b, "short"),
            Cow::Owned(_) => panic!("short text must not allocate"),
        }
    }

    #[test]
    fn truncate_long_text_keeps_head_tail_and_reports_omission() {
        let s: String = std::iter::repeat('x').take(5000).collect();
        let out = truncate_render_text(&s).into_owned();
        // 5000 - (3072 + 1024) = 904 omitted.
        assert!(
            out.contains("<904 chars omitted>"),
            "expected omission marker, got: {}",
            &out[..out.len().min(80)]
        );
        assert!(out.starts_with('x'));
        assert!(out.ends_with('x'));
        assert!(
            out.chars().count() < s.chars().count(),
            "truncated output must be shorter than the input"
        );
    }

    #[test]
    fn truncate_is_char_safe_at_boundary() {
        // Multi-byte chars just below and above the limit must not panic and
        // must round-trip below the threshold.
        let under: String = std::iter::repeat('é').take(MAX_RENDER_LINE_CHARS).collect();
        assert!(matches!(truncate_render_text(&under), Cow::Borrowed(_)));
        let over: String = std::iter::repeat('é')
            .take(MAX_RENDER_LINE_CHARS + 10)
            .collect();
        let _ = truncate_render_text(&over).into_owned(); // must not panic
    }

    // ── push_dot_prefixed_lines ─────────────────────────────────────────────

    #[test]
    fn dot_prefix_skips_leading_blank_lines() {
        // Models often prefix prose with \n / \n\n; the dot must land on the
        // first content row, not burn on an empty line.
        let mut lines = Vec::new();
        push_dot_prefixed_lines(
            &mut lines,
            "\n\nHello",
            40,
            theme::DOT_AGENT,
            theme::AGENT_TEXT,
        );
        assert_eq!(lines.len(), 1, "leading blanks must be dropped");
        assert_eq!(line_text(&lines[0]), "● Hello");
    }

    #[test]
    fn dot_prefix_preserves_paragraph_break_and_indents_continuation() {
        let mut lines = Vec::new();
        push_dot_prefixed_lines(
            &mut lines,
            "A\n\nB",
            40,
            theme::DOT_AGENT,
            theme::AGENT_TEXT,
        );
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert_eq!(
            texts,
            vec!["● A".to_string(), String::new(), "  B".to_string()]
        );
    }

    #[test]
    fn dot_prefix_wraps_long_paragraph_with_hanging_indent() {
        let mut lines = Vec::new();
        // wrap_width 12 → body_width 10; "aaaa bbbb cccc" wraps to 2 rows.
        push_dot_prefixed_lines(
            &mut lines,
            "aaaa bbbb cccc",
            12,
            theme::DOT_AGENT,
            theme::AGENT_TEXT,
        );
        assert!(lines.len() >= 2, "long paragraph must wrap");
        assert!(
            line_text(&lines[0]).starts_with("● "),
            "first row gets the dot"
        );
        assert!(
            line_text(&lines[1]).starts_with("  "),
            "continuation rows get a 2-cell hanging indent"
        );
    }

    // ── push_prompt_prefixed_lines (regression: issue #492) ─────────────────
    //
    // Multi-line prompts (Shift+Enter) must render as multiple ratatui Lines:
    // ratatui does not split an embedded '\n' inside a single Span/Line into
    // separate rows, so lines after the first were silently dropped from the
    // transcript before this helper existed.

    #[test]
    fn prompt_prefix_renders_each_embedded_newline_as_its_own_line() {
        let mut lines = Vec::new();
        push_prompt_prefixed_lines(&mut lines, concat!("line one\n", "line two"), 40);
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert_eq!(
            texts,
            vec!["> line one".to_string(), "  line two".to_string()]
        );
    }

    #[test]
    fn prompt_prefix_single_line_keeps_prior_rendering() {
        let mut lines = Vec::new();
        push_prompt_prefixed_lines(&mut lines, "hello", 40);
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "> hello");
    }

    #[test]
    fn prompt_prefix_preserves_blank_line_between_paragraphs() {
        let mut lines = Vec::new();
        push_prompt_prefixed_lines(&mut lines, "A\n\nB", 40);
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert_eq!(
            texts,
            vec!["> A".to_string(), String::new(), "  B".to_string()]
        );
    }

    #[test]
    fn prompt_prefix_keeps_marker_on_empty_prompt() {
        // Prompt submission doesn't validate non-empty input, so an empty
        // `ChatMessage::User` must still render its "> " marker instead of
        // silently disappearing from the transcript.
        let mut lines = Vec::new();
        push_prompt_prefixed_lines(&mut lines, "", 40);
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "> ");
    }

    #[test]
    fn prompt_prefix_keeps_marker_when_text_starts_with_newline() {
        let mut lines = Vec::new();
        push_prompt_prefixed_lines(&mut lines, concat!("\n", "second line"), 40);
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert_eq!(texts, vec!["> ".to_string(), "  second line".to_string()]);
    }

    // ── collapse_newlines_for_preview ────────────────────────────────────────

    #[test]
    fn collapse_newlines_replaces_embedded_newline_with_space() {
        assert_eq!(
            collapse_newlines_for_preview("remember,\nAnd I would like"),
            "remember, And I would like"
        );
    }

    #[test]
    fn collapse_newlines_borrows_when_no_newline_present() {
        assert!(matches!(
            collapse_newlines_for_preview("no newline here"),
            Cow::Borrowed(_)
        ));
    }
}
