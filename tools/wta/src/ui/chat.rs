use std::borrow::Cow;
#[cfg(test)]
use std::cell::Cell;

use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};
use unicode_width::UnicodeWidthStr;

#[cfg(test)]
use crate::app::CompletedTurn;
use crate::app::{
    App, ChatMessage, NoticeKind, PlanEntryStatus, ToolCallContent, ToolCallKind, ToolCallLocation,
    ToolCallOutput,
};
use crate::theme;
use crate::ui::line_diff::{self, DiffLineKind};
use crate::ui::shimmer;
use crate::ui::tool_presentation::{ToolPhase, ToolPresentation};
use crate::ui_trace;

fn activity_label() -> String {
    t!("chat.activity_thinking").into_owned()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolDetailLevel {
    Compact,
    Preview,
    Detailed,
}

#[derive(Clone, Copy)]
enum ToolDisplay {
    ActiveTurn,
    Completed { expanded: bool },
}

impl ToolDisplay {
    fn detail_level(self, phase: ToolPhase<'_>) -> ToolDetailLevel {
        match self {
            Self::Completed { expanded: true } => ToolDetailLevel::Detailed,
            _ if phase.is_successful() => ToolDetailLevel::Compact,
            _ => ToolDetailLevel::Preview,
        }
    }
}

fn tool_presentation_from_message(message: &ChatMessage) -> Option<ToolPresentation<'_>> {
    let ChatMessage::ToolCall {
        title,
        status,
        kind,
        location,
        location_is_command,
        cwd,
        exit_code,
        locations,
        ..
    } = message
    else {
        return None;
    };
    Some(ToolPresentation::new(
        title,
        status,
        *kind,
        location.as_deref(),
        *location_is_command,
        cwd.as_deref(),
        *exit_code,
        locations,
    ))
}

fn compact_group_kind(message: &ChatMessage, expanded: bool) -> Option<ToolCallKind> {
    if expanded {
        return None;
    }
    let ChatMessage::ToolCall { status, kind, .. } = message else {
        return None;
    };
    let groupable_kind = matches!(
        kind,
        ToolCallKind::Read | ToolCallKind::Edit | ToolCallKind::Delete | ToolCallKind::Search
    );
    (groupable_kind && ToolPhase::from_status(status).is_successful()).then_some(*kind)
}

#[cfg(test)]
fn previous_message_group_start(messages: &[ChatMessage], end: usize) -> usize {
    previous_message_group_start_with_expansion(messages, end, |_| false)
}

fn previous_message_group_start_with_expansion(
    messages: &[ChatMessage],
    end: usize,
    expanded: impl Fn(&str) -> bool,
) -> usize {
    let group_kind = |message: &ChatMessage| {
        let is_expanded = matches!(message, ChatMessage::ToolCall { id, .. } if expanded(id));
        compact_group_kind(message, is_expanded)
    };
    let last = end.saturating_sub(1);
    let Some(kind) = messages.get(last).and_then(group_kind) else {
        return last;
    };
    let mut start = last;
    while start > 0 && messages.get(start - 1).and_then(group_kind) == Some(kind) {
        start -= 1;
    }
    start
}

fn build_compact_tool_group_lines<'a>(messages: &'a [ChatMessage]) -> Vec<Line<'a>> {
    const MAX_TARGETS: usize = 3;

    let Some(first) = messages.first().and_then(tool_presentation_from_message) else {
        return Vec::new();
    };
    let (mut summary, represented) = if matches!(
        first.kind,
        ToolCallKind::Read | ToolCallKind::Edit | ToolCallKind::Delete
    ) {
        let presentations = messages
            .iter()
            .filter_map(tool_presentation_from_message)
            .collect::<Vec<_>>();
        let mut targets = Vec::with_capacity(MAX_TARGETS);
        for target in presentations
            .iter()
            .filter_map(|presentation| presentation.target_name())
        {
            if !targets.contains(&target) {
                targets.push(target);
                if targets.len() == MAX_TARGETS {
                    break;
                }
            }
        }
        if !targets.is_empty() {
            (targets.join(", "), targets.len())
        } else {
            (first.primary_text(true).into_owned(), 1)
        }
    } else {
        (first.primary_text(true).into_owned(), 1)
    };

    if let Some(target) = first.secondary_target() {
        summary.push_str(" · ");
        summary.push_str(&target);
    }
    let remaining = messages.len().saturating_sub(represented);
    if remaining > 0 {
        summary.push_str(&format!(" · +{remaining}"));
    }

    vec![Line::from(vec![
        Span::styled("✓", theme::TOOL_CALL_SUCCESS),
        Span::raw(" "),
        Span::styled(first.kind_label(), theme::TOOL_CALL_KIND),
        Span::styled(" · ", theme::DIM),
        Span::styled(summary, theme::TOOL_CALL_TITLE),
    ])]
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
    static TOOL_DETAIL_BUILD_COUNT: Cell<usize> = const { Cell::new(0) };
    static RENDERED_HEIGHT_LINE_SCAN_COUNT: Cell<usize> = const { Cell::new(0) };
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
pub(crate) fn reset_tool_detail_build_count() {
    TOOL_DETAIL_BUILD_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn tool_detail_build_count() -> usize {
    TOOL_DETAIL_BUILD_COUNT.with(Cell::get)
}

#[cfg(test)]
fn reset_rendered_height_line_scan_count() {
    RENDERED_HEIGHT_LINE_SCAN_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
fn rendered_height_line_scan_count() -> usize {
    RENDERED_HEIGHT_LINE_SCAN_COUNT.with(Cell::get)
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

fn full_output_lines(output: &ToolCallOutput, prefix: &str, wrap_width: usize) -> Vec<String> {
    let mut lines = if output.text.is_empty() {
        Vec::new()
    } else {
        wrapped_tool_text_lines(&output.text, prefix, wrap_width, usize::MAX, false)
    };
    let omitted = output.truncated || lines.len() > MAX_TOOL_DETAIL_OUTPUT_LINES;
    if lines.len() > MAX_TOOL_DETAIL_OUTPUT_LINES {
        lines.drain(..lines.len() - MAX_TOOL_DETAIL_OUTPUT_LINES);
    }
    if omitted {
        lines.insert(0, format!("{prefix}…"));
    }
    lines
}

fn wrapped_tool_text_lines(
    text: &str,
    prefix: &str,
    wrap_width: usize,
    max_lines: usize,
    truncated: bool,
) -> Vec<String> {
    let width = wrap_width.saturating_sub(prefix.width()).max(1);
    let mut lines = Vec::new();
    let mut omitted = truncated;
    'source: for paragraph in text.split('\n') {
        let paragraph = paragraph.strip_suffix('\r').unwrap_or(paragraph);
        let pieces = textwrap::wrap(paragraph, width);
        for piece in pieces {
            if lines.len() == max_lines {
                omitted = true;
                break 'source;
            }
            lines.push(format!("{prefix}{piece}"));
        }
    }
    if omitted {
        lines.push(format!("{prefix}…"));
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

fn truncate_tool_detail_text(text: &str) -> Cow<'_, str> {
    let mut chars = text.chars();
    let head = chars
        .by_ref()
        .take(MAX_TOOL_OUTPUT_LINE_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        Cow::Owned(format!("{head}…"))
    } else {
        Cow::Borrowed(text)
    }
}

struct ToolDetailLine {
    text: String,
    style: Style,
}

impl ToolDetailLine {
    fn dim(text: String) -> Self {
        Self {
            text,
            style: theme::DIM,
        }
    }
}

fn diff_detail_lines(
    path: &str,
    old_text: Option<&ToolCallOutput>,
    new_text: &ToolCallOutput,
    detailed: bool,
    max_lines: usize,
) -> Vec<ToolDetailLine> {
    if max_lines == 0 {
        return Vec::new();
    }
    let mut lines = vec![ToolDetailLine {
        text: format!("    Δ {}", truncate_tool_detail_text(path)),
        style: theme::TOOL_DIFF_HEADER,
    }];
    if !detailed || max_lines == 1 {
        return lines;
    }

    let source_truncated = old_text.is_some_and(|output| output.truncated) || new_text.truncated;
    lines.extend(
        line_diff::preview(
            old_text.map(|output| output.text.as_str()),
            &new_text.text,
            source_truncated,
            max_lines - 1,
        )
        .into_iter()
        .map(|line| {
            let (marker, text, style) = match line.kind {
                DiffLineKind::Context => ("│ ", truncate_tool_detail_text(line.text), theme::DIM),
                DiffLineKind::Added => (
                    "+ ",
                    truncate_tool_detail_text(line.text),
                    theme::TOOL_DIFF_ADDED,
                ),
                DiffLineKind::Removed => (
                    "- ",
                    truncate_tool_detail_text(line.text),
                    theme::TOOL_DIFF_REMOVED,
                ),
                DiffLineKind::Omitted => ("│ ", Cow::Borrowed("…"), theme::DIM),
            };
            ToolDetailLine {
                text: format!("    {marker}{text}"),
                style,
            }
        }),
    );
    lines
}

#[cfg(test)]
fn tool_detail_lines(
    content: &[ToolCallContent],
    locations: &[ToolCallLocation],
    detailed: bool,
) -> Vec<ToolDetailLine> {
    tool_detail_lines_with_width(content, locations, detailed, MAX_TOOL_OUTPUT_LINE_CHARS + 6)
}

fn tool_detail_lines_with_width<'a>(
    content: impl IntoIterator<Item = &'a ToolCallContent>,
    locations: &[ToolCallLocation],
    detailed: bool,
    wrap_width: usize,
) -> Vec<ToolDetailLine> {
    #[cfg(test)]
    TOOL_DETAIL_BUILD_COUNT.with(|count| count.set(count.get() + 1));

    let mut lines = Vec::new();
    let mut omitted = false;
    if detailed {
        for location in locations.iter().take(MAX_TOOL_DETAIL_LINES) {
            let suffix = location
                .line
                .map_or_else(String::new, |line| format!(":{line}"));
            lines.push(ToolDetailLine::dim(format!(
                "    {}{suffix}",
                location.path
            )));
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
                    lines.extend(
                        full_output_lines(output, "    │ ", wrap_width)
                            .into_iter()
                            .map(ToolDetailLine::dim),
                    );
                } else {
                    lines.extend(
                        preview_output_lines(output, "    │ ")
                            .into_iter()
                            .map(ToolDetailLine::dim),
                    );
                }
            }
            ToolCallContent::Diff {
                path,
                old_text,
                new_text,
            } => {
                lines.extend(diff_detail_lines(
                    path,
                    old_text.as_ref(),
                    new_text,
                    detailed,
                    MAX_TOOL_DETAIL_LINES.saturating_sub(lines.len()),
                ));
            }
            ToolCallContent::Terminal {
                id,
                output,
                exit_code,
            } => {
                let status = exit_code.map_or_else(String::new, |code| format!(" · exit {code}"));
                lines.push(ToolDetailLine::dim(format!("    $ {id}{status}")));
                if detailed {
                    if let Some(output) = output {
                        lines.extend(
                            full_output_lines(output, "    │ ", wrap_width)
                                .into_iter()
                                .map(ToolDetailLine::dim),
                        );
                    }
                }
            }
            ToolCallContent::Attachment { label, uri } => {
                let target = uri
                    .as_deref()
                    .map_or_else(String::new, |uri| format!(" · {uri}"));
                lines.push(ToolDetailLine::dim(format!("    ↳ {label}{target}")));
            }
        }
        if lines.len() > MAX_TOOL_DETAIL_LINES {
            omitted = true;
            break;
        }
    }
    if omitted {
        lines.truncate(MAX_TOOL_DETAIL_LINES.saturating_sub(1));
        lines.push(ToolDetailLine::dim("    …".to_string()));
    }
    lines
}

fn cap_tool_detail_lines(lines: &mut Vec<ToolDetailLine>) {
    if lines.len() > MAX_TOOL_DETAIL_LINES {
        lines.truncate(MAX_TOOL_DETAIL_LINES.saturating_sub(1));
        lines.push(ToolDetailLine::dim("    …".to_string()));
    }
}

fn restyle_tool_detail_lines(lines: &mut [ToolDetailLine], has_prior_child: bool) {
    let mut needs_branch = !has_prior_child;
    for line in lines {
        if let Some(content) = line.text.strip_prefix("    │ ").map(str::to_string) {
            line.text = format!("{}{content}", if needs_branch { "  └ " } else { "    " });
        } else if needs_branch {
            if let Some(content) = line.text.strip_prefix("    ").map(str::to_string) {
                line.text = format!("  └ {content}");
            }
        }
        needs_branch = false;
    }
}

/// Estimate the chat block's natural height (in visual rows), saturated at
/// `max_height`. Layout only needs an exact height while the content fits;
/// once the chat fills the available rows, measuring older history cannot
/// change the layout.
pub fn estimated_block_height(app: &App, area_width: u16, max_height: u16) -> u16 {
    let tab = app.current_tab();
    let wrap_width = (area_width as usize).max(1);
    let max_height = max_height.max(1) as usize;
    // Fetch once for the pending-height calculation.
    let pending_text = pending_render_text(tab);

    let streaming_index = tab.streaming_agent_message_index();
    let permission_tool_call_id = permission_tool_call_id(tab);
    let mut height = pending_text
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

    height = height.saturating_add(welcome);
    if height >= max_height {
        return max_height as u16;
    }

    let mut end = tab.messages.len();
    while end > 0 {
        let index = end - 1;
        if Some(index) == streaming_index {
            end = index;
            continue;
        }
        let start = previous_message_group_start_with_expansion(&tab.messages, end, |id| {
            tab.completed_tool_call_expanded(id)
        });
        let (message_lines, _) =
            build_active_message_group(tab, start, end, permission_tool_call_id, wrap_width);
        height = height.saturating_add(rendered_lines_height(&message_lines, wrap_width));
        if height >= max_height {
            return max_height as u16;
        }
        end = start;
    }

    for index in (0..tab.completed_turns.len()).rev() {
        height = height.saturating_add(completed_turn_height(tab, index, wrap_width));
        if height >= max_height {
            return max_height as u16;
        }
    }

    height.max(1) as u16
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
    #[cfg(test)]
    RENDERED_HEIGHT_LINE_SCAN_COUNT.with(|count| count.set(count.get() + lines.len()));

    let width = wrap_width.clamp(1, u16::MAX as usize) as u16;
    lines
        .iter()
        .map(|line| {
            if line.width() <= usize::from(width) {
                1
            } else {
                Paragraph::new(line.clone())
                    .wrap(Wrap { trim: false })
                    .line_count(width)
            }
        })
        .sum()
}

fn completed_turn_height(tab: &crate::app::TabSession, index: usize, wrap_width: usize) -> usize {
    if let Some(height) = tab.cached_completed_turn_height(index, wrap_width) {
        return height;
    }
    if tab.completed_turns.get(index).is_none() {
        return 0;
    }
    let height = rendered_lines_height(
        &build_completed_turn_lines_for_tab(tab, index, false, false, wrap_width).lines,
        wrap_width,
    );
    tab.cache_completed_turn_height(index, wrap_width, height);
    height
}

fn tool_call_presentation(phase: ToolPhase<'_>) -> (&'static str, Style, Option<&str>) {
    match phase {
        ToolPhase::Pending => ("○", theme::TOOL_CALL_PENDING, None),
        ToolPhase::Running => ("●", theme::TOOL_CALL_RUNNING, None),
        ToolPhase::Succeeded => ("✓", theme::TOOL_CALL_SUCCESS, None),
        ToolPhase::Failed(detail) => ("✗", theme::TOOL_CALL_FAILURE, detail),
        ToolPhase::Canceled => ("−", theme::TOOL_CALL_CANCELED, None),
        ToolPhase::Unknown(status) => ("•", theme::DIM, Some(status)),
    }
}

fn rendered_tool_call_marker(
    phase: ToolPhase<'_>,
    permission_active: bool,
    activity_frame: usize,
) -> &'static str {
    if permission_active || phase.is_active() {
        breathing_dot(activity_frame)
    } else {
        tool_call_presentation(phase).0
    }
}

fn should_show_turn_activity(tab: &crate::app::TabSession) -> bool {
    tab.should_show_thinking()
}

pub(crate) fn should_show_activity(app: &App) -> bool {
    matches!(app.state, crate::app::ConnectionState::Connecting(_))
        // A resume outlives the connecting state it starts in: `session/load`
        // only runs once the handshake is done, and it can take tens of
        // seconds. Without this the status row loses its height the instant
        // the connection completes, so `render_activity` draws the resume
        // shimmer into a zero-height area and the pane goes blank for the rest
        // of the load — with no way to tell a slow resume from a hang.
        || app.resume_in_flight()
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

const CHAT_RENDER_MARGIN_ROWS: usize = 32;

struct CompletedTurnHitOffset {
    turn_index: Option<usize>,
    rows_below: usize,
    turn_height: usize,
    expanded: bool,
    prompt_rows: Vec<PromptRowGeometry>,
    tool_rows: Vec<ToolRowGeometry>,
}

struct ToolRowGeometry {
    hit_kind: crate::app::CompletedTurnHitKind,
    row_offset: usize,
    header_width: usize,
    expanded: bool,
    marker: &'static str,
}

struct MessageRowGeometry {
    start: usize,
    end: usize,
    row_offset: usize,
    height: usize,
    thought_header_height: Option<usize>,
}

struct CompletedTurnLines<'a> {
    lines: Vec<Line<'a>>,
    prompt_rows: Vec<PromptRowGeometry>,
    tool_rows: Vec<ToolRowGeometry>,
    message_rows: Vec<MessageRowGeometry>,
}

struct ReadingRegion {
    turn_index: usize,
    message_index: Option<usize>,
    rows_below: usize,
    height: usize,
    thought_header_height: Option<usize>,
}

impl ReadingRegion {
    fn position(
        &self,
        offset: usize,
        visible_height: usize,
    ) -> Option<crate::app::ChatReadingPosition> {
        let top = offset.saturating_add(visible_height);
        let end = self.rows_below.saturating_add(self.height);
        (top > self.rows_below && top <= end).then(|| crate::app::ChatReadingPosition {
            turn_index: self.turn_index,
            message_index: self.message_index,
            row_offset: end - top,
            scroll_offset: offset,
            thought_source: None,
        })
    }
}

fn reading_position_row_offset(
    position: crate::app::ChatReadingPosition,
    message: &ChatMessage,
    wrap_width: usize,
    thought_header_height: Option<usize>,
) -> usize {
    if let (
        Some((anchor_id, byte)),
        ChatMessage::Thought {
            id, expanded: true, ..
        },
    ) = (position.thought_source, message)
    {
        if anchor_id == *id {
            return thought_source_rows(message, wrap_width, thought_header_height)
                .into_iter()
                .take_while(|(start, _)| *start <= byte)
                .last()
                .map_or(position.row_offset, |(_, row)| row);
        }
    }
    position.row_offset
}

fn capture_thought_source(
    position: &mut crate::app::ChatReadingPosition,
    previous: Option<crate::app::ChatReadingPosition>,
    tab: &crate::app::TabSession,
    wrap_width: usize,
    thought_header_height: Option<usize>,
) {
    let messages = if position.turn_index == tab.completed_turns.len() {
        &tab.messages
    } else if let Some(turn) = tab.completed_turns.get(position.turn_index) {
        &turn.details
    } else {
        return;
    };
    let Some(message) = position.message_index.and_then(|index| messages.get(index)) else {
        return;
    };
    let ChatMessage::Thought {
        id, expanded: true, ..
    } = message
    else {
        return;
    };
    // Rewrapping can move the source boundary into the middle of a row. Keep
    // that boundary rather than repeatedly anchoring earlier text on each draw.
    if let Some(previous) = previous.filter(|previous| {
        previous.turn_index == position.turn_index
            && previous.message_index == position.message_index
            && previous
                .thought_source
                .is_some_and(|(anchor_id, _)| anchor_id == *id)
            && reading_position_row_offset(*previous, message, wrap_width, thought_header_height)
                == position.row_offset
    }) {
        position.thought_source = previous.thought_source;
    } else {
        position.thought_source = thought_source_rows(message, wrap_width, thought_header_height)
            .into_iter()
            .take_while(|(_, row)| *row <= position.row_offset)
            .last()
            .map(|(byte, _)| (*id, byte));
    }
}

fn build_active_message_group<'a>(
    tab: &'a crate::app::TabSession,
    start: usize,
    end: usize,
    permission_tool_call_id: Option<&str>,
    wrap_width: usize,
) -> (Vec<Line<'a>>, Option<ToolRowGeometry>) {
    let message = &tab.messages[start];
    let expanded =
        matches!(message, ChatMessage::ToolCall { id, .. } if tab.completed_tool_call_expanded(id));
    let grouped = end - start > 1;
    let lines = if grouped {
        build_compact_tool_group_lines(&tab.messages[start..end])
    } else {
        build_message_lines_with_details(
            message,
            end == tab.messages.len(),
            tab.turn.is_streaming(),
            permission_tool_call_id,
            tab.activity_frame,
            wrap_width,
            ToolDisplay::Completed { expanded },
        )
    };
    let geometry = if let Some(presentation) = tool_presentation_from_message(message) {
        Some(ToolRowGeometry {
            hit_kind: if grouped {
                crate::app::CompletedTurnHitKind::ActiveToolGroup {
                    first_detail_index: start,
                    detail_count: end - start,
                }
            } else {
                crate::app::CompletedTurnHitKind::ActiveToolCall {
                    detail_index: start,
                }
            },
            row_offset: 0,
            header_width: lines
                .first()
                .map_or(1, |line| line.width().min(wrap_width))
                .max(1),
            expanded,
            marker: rendered_tool_call_marker(
                presentation.phase,
                matches!(message, ChatMessage::ToolCall { id, .. } if permission_tool_call_id == Some(id.as_str())),
                tab.activity_frame,
            ),
        })
    } else {
        thought_row_geometry(message, start, true, 0, &lines, wrap_width)
    };
    (lines, geometry)
}

struct PlannedCompletedTurn {
    index: usize,
    rows_below: usize,
    height: usize,
}

struct CompletedTurnViewportPlan {
    turns: Vec<PlannedCompletedTurn>,
    skip_base_lines: bool,
    skipped_rows_below: usize,
    effective_offset: usize,
    requested_rows: usize,
    truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScrollbarMetrics {
    pub content_length: usize,
    pub position: usize,
}

pub(crate) fn scrollbar_metrics(
    total_rows: usize,
    visible_rows: usize,
    offset_from_bottom: usize,
) -> Option<ScrollbarMetrics> {
    if visible_rows == 0 || total_rows <= visible_rows {
        return None;
    }
    let max_offset = total_rows - visible_rows;
    Some(ScrollbarMetrics {
        content_length: max_offset.saturating_add(1),
        position: max_offset.saturating_sub(offset_from_bottom.min(max_offset)),
    })
}

fn plan_completed_turn_viewport(
    tab: &crate::app::TabSession,
    base_rendered_rows: usize,
    visible_height: usize,
    wrap_width: usize,
    mut effective_offset: usize,
    selection_target_idx: Option<usize>,
    viewport_anchor: Option<crate::app::CompletedTurnViewportAnchor>,
) -> CompletedTurnViewportPlan {
    let mut turns = Vec::new();
    let mut skip_base_lines;
    let mut skipped_rows_below;
    let mut requested_rows;
    let mut truncated;

    let target_idx = viewport_anchor
        .map(|anchor| anchor.index)
        .or(selection_target_idx)
        .filter(|index| *index < tab.completed_turns.len());
    if let Some(target_idx) = target_idx {
        let mut target_rows_below = base_rendered_rows;
        for index in ((target_idx + 1)..tab.completed_turns.len()).rev() {
            target_rows_below =
                target_rows_below.saturating_add(completed_turn_height(tab, index, wrap_width));
        }
        let target_height = completed_turn_height(tab, target_idx, wrap_width);
        if let Some(anchor) = viewport_anchor.filter(|anchor| anchor.index == target_idx) {
            effective_offset = anchor
                .row
                .saturating_add(target_rows_below)
                .saturating_add(target_height)
                .saturating_sub(anchor.row_offset)
                .saturating_sub(visible_height);
        } else {
            let target_end = target_rows_below.saturating_add(target_height);
            let viewport_height = visible_height.max(1);
            effective_offset = if target_rows_below < effective_offset {
                target_rows_below
            } else if target_end > effective_offset.saturating_add(viewport_height) {
                target_end.saturating_sub(viewport_height)
            } else {
                effective_offset
            };
        }
    }

    loop {
        turns.clear();
        skipped_rows_below = 0;
        truncated = false;
        skip_base_lines = false;
        let mut rendered_rows_below = base_rendered_rows;

        if rendered_rows_below <= effective_offset {
            skipped_rows_below = rendered_rows_below;
            skip_base_lines = true;
        }
        requested_rows = visible_height
            .saturating_add(effective_offset.saturating_sub(skipped_rows_below))
            .saturating_add(CHAT_RENDER_MARGIN_ROWS);
        let mut built_rows = if skip_base_lines {
            0
        } else {
            base_rendered_rows
        };

        for index in (0..tab.completed_turns.len()).rev() {
            let turn_height = completed_turn_height(tab, index, wrap_width);
            let turn_end = rendered_rows_below.saturating_add(turn_height);
            if turn_end <= effective_offset {
                skipped_rows_below = turn_end;
                rendered_rows_below = turn_end;
                requested_rows = visible_height
                    .saturating_add(effective_offset.saturating_sub(skipped_rows_below))
                    .saturating_add(CHAT_RENDER_MARGIN_ROWS);
                continue;
            }

            turns.push(PlannedCompletedTurn {
                index,
                rows_below: rendered_rows_below.saturating_sub(skipped_rows_below),
                height: turn_height,
            });
            rendered_rows_below = turn_end;
            built_rows = built_rows.saturating_add(turn_height);
            if built_rows >= requested_rows && index > 0 {
                truncated = true;
                break;
            }
        }

        if !truncated {
            let max_offset = rendered_rows_below.saturating_sub(visible_height);
            if effective_offset > max_offset {
                effective_offset = max_offset;
                continue;
            }
        }
        break;
    }

    CompletedTurnViewportPlan {
        turns,
        skip_base_lines,
        skipped_rows_below,
        effective_offset,
        requested_rows,
        truncated,
    }
}

pub fn render(frame: &mut Frame, app: &mut App, area: Rect, scrollbar_area: Rect) {
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
    let mut viewport_anchor = app.current_tab().completed_turn_viewport_anchor();
    let active_anchor = app.current_tab_mut().active_tool_viewport_anchor.take();
    let reading_position = app.current_tab().chat_reading_position.filter(|position| {
        app.current_tab().chat_scroll.offset > 0
            && position.scroll_offset == app.current_tab().chat_scroll.offset
            && !selection_pending
            && viewport_anchor.is_none()
            && active_anchor.is_none()
    });
    let mut reading_position_applied = false;
    let mut active_anchor_applied = false;
    let mut effective_offset = app.current_tab().chat_scroll.offset;
    let mut requested_rows = visible_height
        .saturating_add(effective_offset)
        .saturating_add(CHAT_RENDER_MARGIN_ROWS);

    let mut reversed_lines: Vec<Line> = Vec::new();
    let mut turn_hit_offsets = Vec::new();
    let mut reading_regions = Vec::new();
    let mut skipped_rows_below = 0;

    let mut pending_lines = build_pending_stream_lines(app, wrap_width);
    let mut newer_rows = rendered_lines_height(&pending_lines, wrap_width);
    reversed_lines.extend(pending_lines.drain(..).rev());

    let mut truncated = false;

    let tab = app.current_tab();
    let permission_tool_call_id = permission_tool_call_id(tab);
    let streaming_index = tab.streaming_agent_message_index();
    if let Some(index) = streaming_index {
        if let Some(position) = reading_position.filter(|position| {
            position.turn_index == tab.completed_turns.len()
                && position.message_index == Some(index)
        }) {
            effective_offset = newer_rows
                .saturating_sub(position.row_offset.min(newer_rows.saturating_sub(1)))
                .saturating_sub(visible_height);
            requested_rows = visible_height
                .saturating_add(effective_offset)
                .saturating_add(CHAT_RENDER_MARGIN_ROWS);
            reading_position_applied = true;
        }
        reading_regions.push(ReadingRegion {
            turn_index: tab.completed_turns.len(),
            message_index: Some(index),
            rows_below: 0,
            height: newer_rows,
            thought_header_height: None,
        });
    }
    let mut end = tab.messages.len();
    while end > 0 {
        let idx = end - 1;
        if Some(idx) == streaming_index {
            end = idx;
            continue;
        }
        let start = previous_message_group_start_with_expansion(&tab.messages, end, |id| {
            tab.completed_tool_call_expanded(id)
        });
        let (mut message_lines, geometry) =
            build_active_message_group(tab, start, end, permission_tool_call_id, wrap_width);
        let message_height = rendered_lines_height(&message_lines, wrap_width);
        if let Some(position) = reading_position.filter(|position| {
            position.turn_index == tab.completed_turns.len()
                && position
                    .message_index
                    .is_some_and(|index| (start..end).contains(&index))
        }) {
            let row_offset =
                reading_position_row_offset(position, &tab.messages[start], wrap_width, None);
            effective_offset = newer_rows
                .saturating_add(message_height)
                .saturating_sub(row_offset.min(message_height.saturating_sub(1)))
                .saturating_sub(visible_height);
            requested_rows = visible_height
                .saturating_add(effective_offset)
                .saturating_add(CHAT_RENDER_MARGIN_ROWS);
            reading_position_applied = true;
        }
        reading_regions.push(ReadingRegion {
            turn_index: tab.completed_turns.len(),
            message_index: Some(start),
            rows_below: newer_rows,
            height: message_height,
            thought_header_height: None,
        });
        if let Some((id, row)) = &active_anchor {
            if tab.messages[start..end].iter().any(|message| {
                matches!(message, ChatMessage::ToolCall { id: tool_id, .. } if tool_id == id)
            }) {
                effective_offset = newer_rows.saturating_add(message_height)
                    .saturating_add(row.saturating_sub(inner_area.y) as usize)
                    .saturating_sub(visible_height);
                requested_rows = visible_height.saturating_add(effective_offset)
                    .saturating_add(CHAT_RENDER_MARGIN_ROWS);
                active_anchor_applied = true;
            }
        }
        if let Some(geometry) = geometry {
            turn_hit_offsets.push(CompletedTurnHitOffset {
                turn_index: None,
                rows_below: newer_rows,
                turn_height: message_height,
                expanded: false,
                prompt_rows: Vec::new(),
                tool_rows: vec![geometry],
            });
        }
        newer_rows = newer_rows.saturating_add(message_height);
        reversed_lines.extend(message_lines.drain(..).rev());
        if newer_rows >= requested_rows
            && selection_target_idx.is_none()
            && viewport_anchor.is_none()
            && (active_anchor.is_none() || active_anchor_applied)
            && (reading_position.is_none() || reading_position_applied)
        {
            truncated = true;
            break;
        }
        end = start;
    }

    if !truncated {
        // Resolve the one anchored completed turn with the same geometry used
        // to draw it. Other history still uses the cached-height lazy plan.
        let mut anchored_turn = reading_position
            .filter(|position| position.turn_index < tab.completed_turns.len())
            .map(|position| {
                let built = build_completed_turn_lines_for_tab(
                    tab,
                    position.turn_index,
                    tab.selected_completed_turn_idx == Some(position.turn_index),
                    app.pane_focused,
                    wrap_width,
                );
                let height = rendered_lines_height(&built.lines, wrap_width);
                let row_offset = position
                    .message_index
                    .and_then(|index| {
                        built
                            .message_rows
                            .iter()
                            .find(|row| (row.start..row.end).contains(&index))
                    })
                    .map_or_else(
                        || position.row_offset.min(height.saturating_sub(1)),
                        |row| {
                            let row_offset = reading_position_row_offset(
                                position,
                                &tab.completed_turns[position.turn_index].details[row.start],
                                wrap_width,
                                row.thought_header_height,
                            );
                            row.row_offset
                                .saturating_add(row_offset.min(row.height.saturating_sub(1)))
                        },
                    );
                viewport_anchor = Some(crate::app::CompletedTurnViewportAnchor {
                    index: position.turn_index,
                    row: 0,
                    row_offset,
                });
                tab.cache_completed_turn_height(position.turn_index, wrap_width, height);
                (position.turn_index, built)
            });
        let plan = plan_completed_turn_viewport(
            app.current_tab(),
            newer_rows,
            visible_height,
            wrap_width,
            effective_offset,
            selection_target_idx,
            viewport_anchor,
        );
        if plan.skip_base_lines {
            reversed_lines.clear();
            turn_hit_offsets.clear();
            reading_regions.clear();
        }
        let tab = app.current_tab();
        for planned in plan.turns {
            let turn = &tab.completed_turns[planned.index];
            let CompletedTurnLines {
                lines: mut turn_lines,
                prompt_rows,
                tool_rows,
                message_rows,
            } = if anchored_turn
                .as_ref()
                .is_some_and(|(index, _)| *index == planned.index)
            {
                anchored_turn.take().expect("anchored turn exists").1
            } else {
                build_completed_turn_lines_for_tab(
                    tab,
                    planned.index,
                    tab.selected_completed_turn_idx == Some(planned.index),
                    app.pane_focused,
                    wrap_width,
                )
            };
            for row in message_rows {
                reading_regions.push(ReadingRegion {
                    turn_index: planned.index,
                    message_index: Some(row.start),
                    rows_below: planned.rows_below.saturating_add(
                        planned
                            .height
                            .saturating_sub(row.row_offset.saturating_add(row.height)),
                    ),
                    height: row.height,
                    thought_header_height: row.thought_header_height,
                });
            }
            reading_regions.push(ReadingRegion {
                turn_index: planned.index,
                message_index: None,
                rows_below: planned.rows_below,
                height: planned.height,
                thought_header_height: None,
            });
            turn_hit_offsets.push(CompletedTurnHitOffset {
                turn_index: Some(planned.index),
                rows_below: planned.rows_below,
                turn_height: planned.height,
                expanded: turn.expanded,
                prompt_rows,
                tool_rows,
            });
            reversed_lines.extend(turn_lines.drain(..).rev());
        }
        skipped_rows_below = plan.skipped_rows_below;
        effective_offset = plan.effective_offset;
        requested_rows = plan.requested_rows;
        truncated = plan.truncated;
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
    let local_offset = effective_offset.saturating_sub(skipped_rows_below);
    let scroll = total_lines.saturating_sub(visible_height.saturating_add(local_offset));

    let paragraph = Paragraph::new(lines)
        .block(inner)
        .alignment(crate::rtl::text_alignment())
        .wrap(Wrap { trim: false })
        .scroll((scroll.min(u16::MAX as usize) as u16, 0));

    frame.render_widget(paragraph, area);

    let mut completed_turn_hits = Vec::new();
    let mut visible_completed_turn_anchors = Vec::with_capacity(turn_hit_offsets.len());
    let buffer = frame.buffer_mut();
    for hit_offset in turn_hit_offsets {
        let CompletedTurnHitOffset {
            turn_index,
            rows_below,
            turn_height,
            expanded,
            prompt_rows,
            tool_rows,
        } = hit_offset;
        let active = turn_index.is_none();
        let header_from_top = total_lines.saturating_sub(rows_below.saturating_add(turn_height));
        let mut visible_anchor = None;
        if let Some((turn_index, header_row)) = turn_index.zip(
            header_from_top
                .checked_sub(scroll)
                .filter(|row| *row < visible_height),
        ) {
            visible_anchor = Some(crate::app::CompletedTurnViewportAnchor {
                index: turn_index,
                row: header_row,
                row_offset: 0,
            });
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
            let Some(turn_index) = turn_index else {
                continue;
            };
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

        for tool_row in tool_rows {
            let Some(visible_row) = header_from_top
                .saturating_add(tool_row.row_offset)
                .checked_sub(scroll)
            else {
                continue;
            };
            if visible_row >= visible_height {
                continue;
            }
            if let Some(turn_index) = turn_index {
                visible_anchor.get_or_insert(crate::app::CompletedTurnViewportAnchor {
                    index: turn_index,
                    row: visible_row,
                    row_offset: tool_row.row_offset,
                });
            }
            let row = inner_area.y.saturating_add(visible_row as u16);
            if let Some((start_column, end_column)) = tool_header_hit_columns(
                buffer,
                inner_area,
                row,
                tool_row.marker,
                tool_row.header_width,
            ) {
                completed_turn_hits.push(crate::app::CompletedTurnHitRegion {
                    start_column,
                    end_column,
                    row,
                    turn_index: turn_index.unwrap_or(0),
                    kind: tool_row.hit_kind,
                });
                app.completed_turn_action_links.push(
                    crate::action_links::CompletedTurnActionLink {
                        start_column,
                        end_column,
                        row,
                        action: if tool_row.expanded {
                            crate::action_links::CompletedTurnAction::Collapse
                        } else {
                            crate::action_links::CompletedTurnAction::Expand
                        },
                    },
                );
            }
        }
        if let Some(anchor) = visible_anchor.filter(|_| !active) {
            visible_completed_turn_anchors.push(anchor);
        }
    }
    app.completed_turn_hits = completed_turn_hits;
    app.current_tab_mut()
        .finish_completed_turn_layout(visible_completed_turn_anchors);

    if selection_pending {
        let tab = app.current_tab_mut();
        tab.completed_turn_selection_visible_pending = false;
    }
    app.current_tab_mut().chat_scroll.offset = effective_offset;
    app.current_tab_mut().chat_reading_position = reading_regions
        .iter()
        .find_map(|region| {
            region
                .position(local_offset, visible_height)
                .map(|position| (position, region.thought_header_height))
        })
        .map(|(mut position, thought_header_height)| {
            position.scroll_offset = effective_offset;
            capture_thought_source(
                &mut position,
                reading_position,
                app.current_tab(),
                wrap_width,
                thought_header_height,
            );
            position
        });

    // Update the scroll bound only when the build saw all of history;
    // otherwise the true max is still unknown and the stored value (possibly
    // stale) is the best we have.
    if !truncated {
        app.current_tab_mut().chat_scroll.set_max(
            total_lines
                .saturating_add(effective_offset.saturating_sub(local_offset))
                .saturating_sub(visible_height),
        );
    }

    let rendered_total_rows = total_lines.saturating_add(skipped_rows_below);
    let estimated_total_rows = if truncated {
        newer_rows
            .saturating_add(
                app.current_tab()
                    .estimated_completed_turn_height(wrap_width),
            )
            .max(rendered_total_rows)
            .max(
                effective_offset
                    .saturating_add(visible_height)
                    .saturating_add(1),
            )
    } else {
        rendered_total_rows
    };
    if let Some(metrics) = scrollbar_metrics(estimated_total_rows, visible_height, effective_offset)
        .filter(|_| scrollbar_area.width > 0)
    {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("│"))
            .thumb_symbol("┃")
            .track_style(Style::default().fg(Color::DarkGray))
            .thumb_style(Style::default().fg(Color::Gray));
        let mut scrollbar_state = ScrollbarState::new(metrics.content_length)
            .position(metrics.position)
            .viewport_content_length(visible_height);
        frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
    }

    ui_trace::log_slow("chat_render", render_started.elapsed(), || {
        format!(
            "messages={} pending_chars={} requested_rows={} visible_height={} area={}x{}",
            app.current_tab().messages.len(),
            app.current_tab()
                .streaming_agent_text()
                .map(|text| text.chars().count())
                .unwrap_or(0),
            requested_rows,
            visible_height,
            area.width,
            area.height
        )
    });
}

fn tool_header_hit_columns(
    buffer: &Buffer,
    inner_area: Rect,
    row: u16,
    marker: &str,
    header_width: usize,
) -> Option<(u16, u16)> {
    let inner_end = inner_area.x.saturating_add(inner_area.width);
    let start_column = (inner_area.x..inner_end).find(|column| {
        buffer
            .cell((*column, row))
            .is_some_and(|cell| cell.symbol() == marker)
    })?;
    let width = header_width.min(inner_end.saturating_sub(start_column) as usize) as u16;
    let end_column = start_column.saturating_add(width);
    (start_column < end_column).then_some((start_column, end_column))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PromptRowGeometry {
    row_offset: usize,
    line_width: usize,
    body_start: usize,
    body_width: usize,
}

fn rendered_line_widths(line: &Line<'_>, wrap_width: usize) -> Vec<usize> {
    if line.width() <= wrap_width {
        return vec![line.width()];
    }
    // Use the renderer's row count, including rows containing only whitespace.
    let width = wrap_width.clamp(1, u16::MAX as usize) as u16;
    let paragraph = Paragraph::new(line.clone()).wrap(Wrap { trim: false });
    let height = paragraph.line_count(width).min(u16::MAX as usize) as u16;
    let area = Rect::new(0, 0, width, height);
    let mut buffer = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(paragraph, area, &mut buffer);
    (0..height)
        .map(|row| {
            (0..width)
                .rev()
                .find_map(|column| {
                    let symbol = buffer[(column, row)].symbol();
                    (!symbol.trim().is_empty())
                        .then(|| usize::from(column) + UnicodeWidthStr::width(symbol))
                })
                .unwrap_or(0)
        })
        .collect()
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

        for (piece_index, line_width) in rendered_line_widths(line, width).into_iter().enumerate() {
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

#[cfg(test)]
fn build_completed_turn_lines<'a>(
    turn: &'a crate::app::CompletedTurn,
    is_selected: bool,
    pane_focused: bool,
    wrap_width: usize,
) -> Vec<Line<'a>> {
    build_completed_turn_lines_with_prompt_rows(turn, is_selected, pane_focused, wrap_width).0
}

#[cfg(test)]
fn build_completed_turn_lines_with_prompt_rows<'a>(
    turn: &'a crate::app::CompletedTurn,
    is_selected: bool,
    pane_focused: bool,
    wrap_width: usize,
) -> (Vec<Line<'a>>, Vec<PromptRowGeometry>) {
    let built = build_completed_turn_lines_with_geometry(
        turn,
        is_selected,
        pane_focused,
        wrap_width,
        |_| false,
    );
    (built.lines, built.prompt_rows)
}

fn build_completed_turn_lines_for_tab<'a>(
    tab: &'a crate::app::TabSession,
    turn_index: usize,
    is_selected: bool,
    pane_focused: bool,
    wrap_width: usize,
) -> CompletedTurnLines<'a> {
    let turn = &tab.completed_turns[turn_index];
    build_completed_turn_lines_with_geometry(turn, is_selected, pane_focused, wrap_width, |id| {
        tab.completed_tool_call_expanded(id)
    })
}

fn rendered_header_width(lines: &[Line<'_>], wrap_width: usize) -> usize {
    lines.first().map_or(1, |line| {
        rendered_line_widths(line, wrap_width)
            .first()
            .copied()
            .unwrap_or(0)
            .max(1)
    })
}

fn build_completed_turn_lines_with_geometry<'a>(
    turn: &'a crate::app::CompletedTurn,
    is_selected: bool,
    pane_focused: bool,
    wrap_width: usize,
    tool_expanded: impl Fn(&str) -> bool,
) -> CompletedTurnLines<'a> {
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

    let prompt_line_count = lines.len();
    let mut trailing_marker = turn.trailing_marker.as_deref();
    // Apply the marker before measuring its line: it can wrap both detail
    // headers and prompts. Empty details must not consume the pending marker.
    let mut append_marker = |lines: &mut [Line<'a>]| {
        if let Some(line) = lines.first_mut() {
            if let Some(marker) = trailing_marker.take() {
                line.spans.push(Span::raw("  "));
                line.spans.push(Span::styled(marker, theme::DIM));
            }
        }
    };
    if !turn.expanded || turn.details.is_empty() {
        append_marker(&mut lines);
    }

    let mut tool_rows = Vec::new();
    let mut message_rows = Vec::new();
    if turn.expanded {
        // Render the captured details — the agent reply, tool calls,
        // plans, etc. — using the same builder as the active turn so the
        // formatting matches. `is_last_message=false` and
        // `agent_streaming=false` together suppress the streaming-cursor
        // path; details are always finalized by the time they land here.
        let mut rendered_height = rendered_lines_height(&lines, wrap_width);
        let mut detail_index = 0;
        while detail_index < turn.details.len() {
            let msg = &turn.details[detail_index];
            let expanded = match msg {
                ChatMessage::ToolCall { id, .. } => tool_expanded(id),
                _ => false,
            };
            let group_kind = compact_group_kind(msg, expanded);
            let mut group_end = detail_index + 1;
            if let Some(group_kind) = group_kind {
                while group_end < turn.details.len() {
                    let next = &turn.details[group_end];
                    let next_expanded = match next {
                        ChatMessage::ToolCall { id, .. } => tool_expanded(id),
                        _ => false,
                    };
                    if compact_group_kind(next, next_expanded) != Some(group_kind) {
                        break;
                    }
                    group_end += 1;
                }
            }

            if group_end - detail_index > 1 {
                let mut message_lines =
                    build_compact_tool_group_lines(&turn.details[detail_index..group_end]);
                append_marker(&mut message_lines);
                let message_height = rendered_lines_height(&message_lines, wrap_width);
                message_rows.push(MessageRowGeometry {
                    start: detail_index,
                    end: group_end,
                    row_offset: rendered_height,
                    height: message_height,
                    thought_header_height: None,
                });
                tool_rows.push(ToolRowGeometry {
                    hit_kind: crate::app::CompletedTurnHitKind::ToolGroup {
                        first_detail_index: detail_index,
                        detail_count: group_end - detail_index,
                    },
                    row_offset: rendered_height,
                    header_width: rendered_header_width(&message_lines, wrap_width),
                    expanded: false,
                    marker: "✓",
                });
                rendered_height = rendered_height.saturating_add(message_height);
                lines.extend(message_lines);
                detail_index = group_end;
                continue;
            }

            let tool_geometry = match msg {
                ChatMessage::ToolCall { id, .. } => Some((
                    tool_expanded(id),
                    rendered_tool_call_marker(
                        tool_presentation_from_message(msg)
                            .expect("tool call presentation")
                            .phase,
                        false,
                        0,
                    ),
                )),
                _ => None,
            };
            let display = tool_geometry.map_or(ToolDisplay::ActiveTurn, |(expanded, _)| {
                ToolDisplay::Completed { expanded }
            });
            let mut message_lines =
                build_message_lines_with_details(msg, false, false, None, 0, wrap_width, display);
            append_marker(&mut message_lines);
            let message_height = rendered_lines_height(&message_lines, wrap_width);
            message_rows.push(MessageRowGeometry {
                start: detail_index,
                end: detail_index + 1,
                row_offset: rendered_height,
                height: message_height,
                thought_header_height: matches!(msg, ChatMessage::Thought { expanded: true, .. })
                    .then(|| {
                        rendered_lines_height(
                            &message_lines[..message_lines.len().min(1)],
                            wrap_width,
                        )
                    }),
            });
            if let Some(mut geometry) = thought_row_geometry(
                msg,
                detail_index,
                false,
                rendered_height,
                &message_lines,
                wrap_width,
            ) {
                geometry.header_width = rendered_header_width(&message_lines, wrap_width);
                tool_rows.push(geometry);
            }
            if let Some((expanded, marker)) = tool_geometry {
                tool_rows.push(ToolRowGeometry {
                    hit_kind: crate::app::CompletedTurnHitKind::ToolCall { detail_index },
                    row_offset: rendered_height,
                    header_width: rendered_header_width(&message_lines, wrap_width),
                    expanded,
                    marker,
                });
            }
            rendered_height = rendered_height.saturating_add(message_height);
            lines.extend(message_lines);
            detail_index += 1;
        }
    }

    if lines.len() == prompt_line_count {
        // An expanded transcript can contain only empty details.
        append_marker(&mut lines);
        if !message_rows.is_empty() {
            let prompt_height = rendered_lines_height(&lines, wrap_width);
            for row in &mut message_rows {
                row.row_offset = prompt_height;
            }
        }
    }
    let prompt_rows = completed_turn_prompt_rows(&lines[..prompt_line_count], wrap_width);

    // Push a trailing blank only if the last detail (or the prompt header
    // for collapsed turns) didn't already supply one. Agent / Error /
    // System / Plan / AgentEvent trail a blank via build_message_lines.
    // ToolCall only does so when it renders command details; collapsed
    // turns stop at the prompt header.
    if lines.last().map_or(true, |l| !l.spans.is_empty()) {
        lines.push(Line::default());
    }
    CompletedTurnLines {
        lines,
        prompt_rows,
        tool_rows,
        message_rows,
    }
}

pub fn render_activity(frame: &mut Frame, app: &App, area: Rect) {
    let tab = app.current_tab();
    // A queued restore spans connection setup and session/load. Keep its
    // context without hiding setup stages; put the stage first for narrow panes.
    // Both shimmers use the app counter because no turn is in flight.
    if tab.loading_session {
        let short_id: String = tab
            .loading_target_session_id
            .as_deref()
            .unwrap_or_default()
            .chars()
            .take(8)
            .collect();
        let label = if let crate::app::ConnectionState::Connecting(stage) = &app.state {
            t!(
                "connection.resuming_stage",
                stage = stage.as_str(),
                session_id = short_id.as_str()
            )
            .into_owned()
        } else {
            t!("system.resuming_session", session_id = short_id).into_owned()
        };
        let line = Line::from(shimmer::shimmer_spans(&label, app.activity_frame as usize));
        frame.render_widget(Paragraph::new(line), area);
        return;
    }
    // While the helper is still establishing its connection to the agent,
    // show its current observable stage. The handshake
    // (pipe connect → ACP init → session/new) can take tens of seconds on a
    // cold start; without an animated indicator the pane looked frozen. Uses
    // the app-level `activity_frame`, which is advanced on Tick while the
    // state is `Connecting` (see handle_event). Takes precedence over the
    // turn spinner because no turn can be in flight before we're connected.
    if let crate::app::ConnectionState::Connecting(stage) = &app.state {
        let line = Line::from(shimmer::shimmer_spans(stage, app.activity_frame as usize));
        frame.render_widget(Paragraph::new(line), area);
        return;
    }
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

pub(crate) fn pending_render_text(tab: &crate::app::TabSession) -> Option<Cow<'_, str>> {
    tab.streaming_agent_text()
        .and_then(user_visible_stream_text)
}

fn build_pending_stream_lines<'a>(app: &App, wrap_width: usize) -> Vec<Line<'a>> {
    let tab = app.current_tab();
    let agent_text = tab
        .streaming_agent_text()
        .and_then(user_visible_stream_text);
    let mut lines = Vec::new();
    if let Some(text) = agent_text.as_ref() {
        let total = text.chars().count();
        let shown = tab.reveal_chars.max(1).min(total);
        let revealed = if shown >= total {
            Cow::Borrowed(text.as_ref())
        } else {
            Cow::Owned(text.chars().take(shown).collect())
        };
        push_dot_prefixed_lines(
            &mut lines,
            &revealed,
            wrap_width,
            theme::DOT_AGENT,
            theme::AGENT_TEXT,
        );
    }
    lines
}

#[cfg(test)]
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
        ToolDisplay::ActiveTurn,
    )
}

fn thought_gutter(wrap_width: usize) -> &'static str {
    if wrap_width <= 3 {
        ""
    } else {
        "│ "
    }
}

/// Retain source lengths and fit every piece on one rendered body row.
/// Optimal fit can intentionally overflow, causing a second unanchored row.
fn wrap_thought_text(text: &str, wrap_width: usize) -> Vec<(usize, Cow<'_, str>)> {
    let width = wrap_width
        .saturating_sub(thought_gutter(wrap_width).width())
        .max(1);
    let options = textwrap::Options::new(width).wrap_algorithm(textwrap::WrapAlgorithm::FirstFit);
    let mut rows = Vec::new();
    let mut paragraph_start = 0;
    for paragraph in text.split('\n') {
        let content = paragraph.strip_suffix('\r').unwrap_or(paragraph);
        if content.len() < width {
            // Match textwrap's short-line path, including its whitespace handling.
            rows.push((
                paragraph_start,
                Cow::Borrowed(content.trim_end_matches(' ')),
            ));
        } else {
            let words = textwrap::core::break_words(
                textwrap::word_splitters::split_words(
                    // Ratatui displays escape payloads as plain text. End a
                    // fragment at ESC so textwrap cannot hide their width.
                    content
                        .split_inclusive('\x1b')
                        .flat_map(|part| options.word_separator.find_words(part)),
                    &options.word_splitter,
                ),
                width,
            );
            let mut byte = 0;
            for line in options.wrap_algorithm.wrap(&words, &[width, width]) {
                // textwrap's fragments partition the original UTF-8 source in
                // order, including discarded whitespace. No text search is needed
                // even for identical lines or empty wrapped rows.
                let consumed = line
                    .iter()
                    .map(|word| word.len() + word.whitespace.len())
                    .sum::<usize>();
                let trailing = line.last().map_or(0, |word| word.whitespace.len());
                let mut piece = Cow::Borrowed(&content[byte..byte + consumed - trailing]);
                if let Some(last) = line.last() {
                    if !last.penalty.is_empty() {
                        piece.to_mut().push_str(last.penalty);
                    }
                }
                rows.push((paragraph_start + byte, piece));
                byte += consumed;
            }
        }
        paragraph_start += paragraph.len() + 1;
    }
    rows
}

fn thought_source_rows(
    message: &ChatMessage,
    wrap_width: usize,
    thought_header_height: Option<usize>,
) -> Vec<(usize, usize)> {
    let ChatMessage::Thought {
        text,
        expanded: true,
        ..
    } = message
    else {
        return Vec::new();
    };
    if text.trim().is_empty() {
        return Vec::new();
    }
    let lines = build_message_lines_with_details(
        message,
        false,
        false,
        None,
        0,
        wrap_width,
        ToolDisplay::ActiveTurn,
    );
    // Completed turns can wrap an inline marker onto extra header rows.
    let mut row =
        thought_header_height.unwrap_or_else(|| rendered_lines_height(&lines[..1], wrap_width));
    wrap_thought_text(text, wrap_width)
        .into_iter()
        .zip(&lines[1..])
        .map(|((byte, _), line)| {
            let start = row;
            row += rendered_lines_height(std::slice::from_ref(line), wrap_width);
            (byte, start)
        })
        .collect()
}

fn build_message_lines_with_details<'a>(
    msg: &'a ChatMessage,
    is_last_message: bool,
    agent_streaming: bool,
    permission_tool_call_id: Option<&str>,
    activity_frame: usize,
    wrap_width: usize,
    tool_display: ToolDisplay,
) -> Vec<Line<'a>> {
    let mut lines = Vec::new();
    match msg {
        ChatMessage::Thought {
            text,
            expanded,
            duration_ms,
            ..
        } => {
            if text.trim().is_empty() {
                return lines;
            }
            let style = theme::DIM.add_modifier(Modifier::ITALIC);
            let marker = if *expanded { "▼" } else { "▶" };
            let duration = duration_ms
                .map(|ms| format!(" · {:.1}s", ms as f64 / 1000.0))
                .unwrap_or_default();
            lines.push(Line::from(Span::styled(
                format!("{marker} {}{duration}", t!("chat.tool_kind.think")),
                style,
            )));
            if *expanded {
                for (_, piece) in wrap_thought_text(text, wrap_width) {
                    lines.push(Line::from(vec![
                        Span::styled(thought_gutter(wrap_width), style),
                        Span::styled(piece.into_owned(), style),
                    ]));
                }
            }
        }
        ChatMessage::User(text) => {
            push_prompt_prefixed_lines(&mut lines, text, wrap_width);
            lines.push(Line::default());
        }
        ChatMessage::Agent(text) => {
            push_dot_prefixed_lines(
                &mut lines,
                text,
                wrap_width,
                theme::DOT_AGENT,
                theme::AGENT_TEXT,
            );
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
            query,
            location,
            location_is_command,
            cwd,
            output,
            exit_code,
            content,
            locations,
        } => {
            let presentation = ToolPresentation::new(
                title,
                status,
                *kind,
                location.as_deref(),
                *location_is_command,
                cwd.as_deref(),
                *exit_code,
                locations,
            );
            let detail_level = tool_display.detail_level(presentation.phase);
            let (_, marker_style, detail) = tool_call_presentation(presentation.phase);
            let marker = rendered_tool_call_marker(
                presentation.phase,
                permission_tool_call_id == Some(id.as_str()),
                activity_frame,
            );
            let primary_text =
                match presentation.primary_text(detail_level == ToolDetailLevel::Compact) {
                    Cow::Borrowed(text) => truncate_render_text(text),
                    Cow::Owned(text) => Cow::Owned(truncate_render_text(&text).into_owned()),
                };
            let mut spans = vec![
                Span::styled(marker, marker_style),
                Span::raw(" "),
                Span::styled(presentation.kind_label(), theme::TOOL_CALL_KIND),
            ];
            if !primary_text.is_empty() {
                spans.push(Span::styled(" · ", theme::DIM));
                spans.push(Span::styled(primary_text, theme::TOOL_CALL_TITLE));
            }
            let display_target = presentation.display_target();
            let target = display_target.as_deref();
            if let Some(target) = presentation.secondary_target() {
                spans.push(Span::styled(
                    format!(" · {}", truncate_render_text(&target)),
                    theme::DIM,
                ));
            }
            if presentation.kind == ToolCallKind::Execute
                && detail_level != ToolDetailLevel::Compact
            {
                if let Some(cwd) = presentation
                    .cwd
                    .filter(|cwd| !cwd.is_empty())
                    .filter(|cwd| !presentation.title.contains(cwd))
                {
                    spans.push(Span::styled(
                        format!(" · {}", truncate_render_text(cwd)),
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
            if let Some(exit_code) = presentation.visible_exit_code() {
                spans.push(Span::styled(format!(" · exit {exit_code}"), theme::DIM));
            }
            lines.push(Line::from(spans));
            let mut rendered_command = false;
            if presentation.target_is_command && detail_level != ToolDetailLevel::Compact {
                if let Some(command) = target {
                    for (index, entry) in crate::ui::command_format::command_display_lines(command)
                        .into_iter()
                        .enumerate()
                    {
                        rendered_command = true;
                        lines.push(Line::from(Span::styled(
                            entry.rendered_text(if index == 0 { "  └ " } else { "    " }),
                            theme::CARD_CODE,
                        )));
                    }
                }
            }
            let mut rendered_output = false;
            if detail_level == ToolDetailLevel::Preview
                && (*kind == ToolCallKind::Execute || *location_is_command)
            {
                if let Some(output) = output {
                    for (index, line) in tool_output_lines(output).into_iter().enumerate() {
                        rendered_output = true;
                        lines.push(Line::from(Span::styled(
                            format!(
                                "{}{line}",
                                if index == 0 && !rendered_command {
                                    "  └ "
                                } else {
                                    "    "
                                }
                            ),
                            theme::DIM,
                        )));
                    }
                }
            }
            // A raw-output-only update can supersede an earlier text progress block.
            let prefer_search_output = *kind == ToolCallKind::Search && output.is_some();
            let visible_content = || {
                content.iter().filter(|item| {
                    !prefer_search_output || !matches!(item, ToolCallContent::Text(_))
                })
            };
            let has_text_content =
                visible_content().any(|item| matches!(item, ToolCallContent::Text(_)));
            let mut detail_lines = match detail_level {
                ToolDetailLevel::Compact => Vec::new(),
                ToolDetailLevel::Preview => {
                    tool_detail_lines_with_width(visible_content(), locations, false, wrap_width)
                }
                ToolDetailLevel::Detailed => {
                    tool_detail_lines_with_width(visible_content(), locations, true, wrap_width)
                }
            };
            if !has_text_content && detail_level != ToolDetailLevel::Compact {
                if let Some(output) = output {
                    if detail_level == ToolDetailLevel::Detailed {
                        detail_lines.extend(
                            full_output_lines(output, "    │ ", wrap_width)
                                .into_iter()
                                .map(ToolDetailLine::dim),
                        );
                    } else if *kind != ToolCallKind::Execute && !*location_is_command {
                        detail_lines.extend(
                            preview_output_lines(output, "    │ ")
                                .into_iter()
                                .map(ToolDetailLine::dim),
                        );
                    }
                }
            }
            cap_tool_detail_lines(&mut detail_lines);
            if detail_level == ToolDetailLevel::Detailed && *kind == ToolCallKind::Search {
                // Queries are already bounded at ingestion. Keep every retained line scrollable.
                let text = query.as_ref().map_or_else(
                    || truncate_render_text(title),
                    |query| Cow::Borrowed(query.text.as_str()),
                );
                let mut query_lines = wrapped_tool_text_lines(
                    &text,
                    "    │ ",
                    wrap_width,
                    usize::MAX,
                    query.as_ref().is_some_and(|query| query.truncated),
                )
                .into_iter()
                .map(ToolDetailLine::dim)
                .collect::<Vec<_>>();
                query_lines.append(&mut detail_lines);
                detail_lines = query_lines;
            }
            restyle_tool_detail_lines(&mut detail_lines, rendered_command || rendered_output);
            let rendered_details = !detail_lines.is_empty();
            for line in detail_lines {
                lines.push(Line::from(Span::styled(line.text, line.style)));
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
                    theme::DISCLAIMER_TEXT,
                ),
            ]));
        }
    }
    lines
}

fn thought_row_geometry(
    message: &ChatMessage,
    detail_index: usize,
    active: bool,
    row_offset: usize,
    lines: &[Line<'_>],
    wrap_width: usize,
) -> Option<ToolRowGeometry> {
    let ChatMessage::Thought {
        id, text, expanded, ..
    } = message
    else {
        return None;
    };
    if text.trim().is_empty() {
        return None;
    }
    Some(ToolRowGeometry {
        hit_kind: crate::app::CompletedTurnHitKind::Thought {
            id: *id,
            detail_index,
            active,
        },
        row_offset,
        header_width: lines
            .first()
            .map_or(1, |line| line.width().min(wrap_width))
            .max(1),
        expanded: *expanded,
        marker: if *expanded { "▼" } else { "▶" },
    })
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

    for paragraph in text.trim_end_matches(['\r', '\n']).split('\n') {
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
    fn expanded_search_tool_wraps_exact_query_and_returned_result_instead_of_clipping() {
        let query = format!("QUERY_START {} QUERY_END", "路径\\full/terms ".repeat(180));
        let result = "RESULT_START returned text RESULT_END".to_string();
        let mut message = ChatMessage::ToolCall {
            id: "search".into(),
            title: "Searching for 'QUERY_START...'".into(),
            status: "Completed".into(),
            kind: ToolCallKind::Search,
            query: Some(ToolCallOutput {
                text: query,
                truncated: false,
            }),
            location: None,
            location_is_command: false,
            cwd: None,
            output: None,
            exit_code: None,
            content: vec![ToolCallContent::Text(ToolCallOutput {
                text: result,
                truncated: false,
            })],
            locations: Vec::new(),
        };
        for status in ["InProgress", "Completed"] {
            if let ChatMessage::ToolCall {
                status: current, ..
            } = &mut message
            {
                *current = status.into();
            }
            let compact = build_message_lines(&message, true, true, None, 0, 24);
            assert!(!compact
                .iter()
                .any(|line| line.to_string().contains("QUERY_END")));
            let lines = build_message_lines_with_details(
                &message,
                true,
                true,
                None,
                0,
                24,
                ToolDisplay::Completed { expanded: true },
            );
            let text = lines
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            for expected in [
                "QUERY_START",
                "QUERY_END",
                "RESULT_START",
                "RESULT_END",
                "full/terms",
                "路",
                "径",
            ] {
                assert!(text.contains(expected), "{text}");
            }
            assert!(lines.len() > MAX_TOOL_DETAIL_LINES);
            assert!(lines.iter().skip(1).all(|line| line.width() <= 24));
        }
    }

    #[test]
    fn expanded_search_tool_falls_back_to_provider_title_without_inventing_results() {
        let title = format!("Searching {} TITLE_END", "provider title ".repeat(20));
        let message = ChatMessage::ToolCall {
            id: "search".into(),
            title: title.clone(),
            status: "Completed".into(),
            kind: ToolCallKind::Search,
            query: None,
            location: None,
            location_is_command: false,
            cwd: None,
            output: None,
            exit_code: None,
            content: Vec::new(),
            locations: Vec::new(),
        };
        let lines = build_message_lines_with_details(
            &message,
            false,
            false,
            None,
            0,
            48,
            ToolDisplay::Completed { expanded: true },
        );
        let text = lines
            .iter()
            .skip(1)
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("TITLE_END"));
        assert!(!text.contains("result"));
        assert!(lines.iter().skip(1).all(|line| line.width() <= 48));
    }

    #[test]
    fn wrapped_tool_text_crlf_matches_lf_preserves_blank_and_trailing_lines() {
        let paragraphs = ["first", "", "next", ""];
        let expected = ["│ first", "│ ", "│ next", "│ "];
        for separator in ["\n", "\r\n"] {
            let text = paragraphs.join(separator);
            assert_eq!(
                wrapped_tool_text_lines(&text, "│ ", 20, usize::MAX, false),
                expected
            );
            assert_eq!(
                wrapped_tool_text_lines(&text, "│ ", 20, 3, false),
                ["│ first", "│ ", "│ next", "│ …"]
            );
        }
    }

    #[test]
    fn expanded_search_crlf_query_and_results_match_lf_without_control_spans() {
        let paragraphs = [
            "first paragraph with wrapped words 界",
            "",
            "next paragraph",
            "",
        ];
        for width in [12, 24, 48] {
            let render = |separator: &str| {
                let text = paragraphs.join(separator);
                let message = ChatMessage::ToolCall {
                    id: "search".into(),
                    title: "Search".into(),
                    status: "Completed".into(),
                    kind: ToolCallKind::Search,
                    query: Some(ToolCallOutput {
                        text: text.clone(),
                        truncated: false,
                    }),
                    location: None,
                    location_is_command: false,
                    cwd: None,
                    output: Some(ToolCallOutput {
                        text,
                        truncated: false,
                    }),
                    exit_code: None,
                    content: Vec::new(),
                    locations: Vec::new(),
                };
                let lines = build_message_lines_with_details(
                    &message,
                    false,
                    false,
                    None,
                    0,
                    width,
                    ToolDisplay::Completed { expanded: true },
                );
                assert!(lines.iter().skip(1).all(|line| line.width() <= width));
                assert!(lines
                    .iter()
                    .flat_map(|line| &line.spans)
                    .all(|span| !span.content.contains('\r')));
                lines.iter().map(ToString::to_string).collect::<Vec<_>>()
            };
            assert_eq!(render("\r\n"), render("\n"));
        }
        let output = ToolCallOutput {
            text: (0..20).map(|_| "result").collect::<Vec<_>>().join("\r\n"),
            truncated: false,
        };
        let lines = full_output_lines(&output, "│ ", 12);
        assert_eq!(lines.len(), MAX_TOOL_DETAIL_OUTPUT_LINES + 1);
        assert_eq!(lines[0], "│ …");
        assert!(lines
            .iter()
            .all(|line| line.width() <= 12 && !line.contains('\r')));
    }

    #[test]
    fn empty_expanded_tool_output_has_no_placeholder_but_preserves_truncation() {
        let mut output = ToolCallOutput {
            text: String::new(),
            truncated: false,
        };
        assert!(full_output_lines(&output, "    │ ", 40).is_empty());
        assert!(tool_detail_lines_with_width(
            [ToolCallContent::Text(output.clone())].iter(),
            &[],
            true,
            40
        )
        .is_empty());

        output.truncated = true;
        assert_eq!(full_output_lines(&output, "    │ ", 40), ["    │ …"]);
    }

    #[test]
    fn wrapped_tool_text_preserves_unicode_and_marks_both_input_and_visual_limits() {
        let text = "界".repeat(4000);
        let lines =
            wrapped_tool_text_lines(&text, "    │ ", 30, MAX_TOOL_DETAIL_OUTPUT_LINES, false);
        assert_eq!(lines.len(), MAX_TOOL_DETAIL_OUTPUT_LINES + 1);
        assert_eq!(lines.last().unwrap(), "    │ …");
        assert!(lines.iter().all(|line| line.width() <= 30));
        assert_eq!(
            wrapped_tool_text_lines("retained", "    │ ", 40, 12, true),
            ["    │ retained", "    │ …"]
        );
    }

    #[test]
    fn completed_tool_geometry_scans_rendered_lines_linearly() {
        let turn = CompletedTurn {
            prompt: "tools".into(),
            details: (0..100)
                .map(|index| ChatMessage::ToolCall {
                    id: format!("tool-{index}"),
                    query: None,
                    title: format!("Read file {index}"),
                    status: "Completed".into(),
                    kind: ToolCallKind::Read,
                    location: Some(format!(r"C:\repo\file-{index}.txt")),
                    location_is_command: false,
                    cwd: None,
                    output: None,
                    exit_code: None,
                    content: Vec::new(),
                    locations: Vec::new(),
                })
                .collect(),
            expanded: true,
            trailing_marker: None,
        };

        reset_rendered_height_line_scan_count();
        build_completed_turn_lines_with_geometry(&turn, false, false, 80, |_| false);

        assert!(
            rendered_height_line_scan_count() <= 110,
            "tool geometry must measure the prompt once and each message once",
        );
    }

    #[test]
    fn rendered_geometry_matches_paragraph_wrapping() {
        for width in [48u16, 36, 12, 4] {
            for text in [
                format!("✓ {}", "a".repeat(usize::from(width) - 1)),
                "word ".repeat(12),
                "界".repeat(12),
                " ".repeat(usize::from(width) * 3),
                "word-with-hyphens ".repeat(4),
                String::new(),
            ] {
                let line = Line::from(text.clone());
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 80)).unwrap();
                terminal
                    .draw(|frame| {
                        frame.render_widget(
                            Paragraph::new(vec![line.clone(), Line::from("NEXT")])
                                .wrap(Wrap { trim: false }),
                            frame.area(),
                        );
                    })
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let height = (0..80)
                    .position(|row| {
                        (0..width)
                            .map(|column| buffer[(column, row)].symbol())
                            .collect::<String>()
                            .starts_with("NEXT")
                    })
                    .unwrap();
                assert_eq!(
                    rendered_lines_height(std::slice::from_ref(&line), usize::from(width)),
                    height,
                    "width={width}, text={text:?}"
                );
                assert_eq!(
                    rendered_line_widths(&line, usize::from(width)).len(),
                    height
                );
            }
        }
    }

    #[test]
    fn completed_marker_geometry_matches_final_buffer() {
        let _locale = crate::test_support::lock_locale();
        rust_i18n::set_locale("en-US");
        let tool = |id: &str, kind| ChatMessage::ToolCall {
            id: id.into(),
            query: None,
            title: format!("HEADER_{id}"),
            status: "Completed".into(),
            kind,
            location: None,
            location_is_command: false,
            cwd: None,
            output: None,
            exit_code: None,
            content: Vec::new(),
            locations: Vec::new(),
        };
        let empty = ChatMessage::Thought {
            id: Default::default(),
            text: String::new(),
            expanded: false,
            duration_ms: None,
        };
        for first in [
            vec![ChatMessage::System("界".repeat(18))],
            vec![ChatMessage::System("short".into())],
            vec![empty.clone(), ChatMessage::System("界".repeat(18))],
            vec![
                tool("one", ToolCallKind::Read),
                tool("two", ToolCallKind::Read),
            ],
            vec![
                empty.clone(),
                tool("one", ToolCallKind::Read),
                tool("two", ToolCallKind::Read),
            ],
            vec![tool("first", ToolCallKind::Other)],
            vec![ChatMessage::Thought {
                id: Default::default(),
                text: "retained reasoning".into(),
                expanded: false,
                duration_ms: Some(12345),
            }],
        ] {
            let later_index = first.len();
            let mut details = first;
            details.extend([
                ChatMessage::System("LATER_MESSAGE".into()),
                tool("later", ToolCallKind::Other),
            ]);
            let turn = CompletedTurn {
                prompt: "history".into(),
                details,
                expanded: true,
                trailing_marker: Some("MARKER".repeat(4)),
            };
            let built =
                build_completed_turn_lines_with_geometry(&turn, false, false, 36, |_| false);
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(36, 30)).unwrap();
            terminal
                .draw(|frame| {
                    frame.render_widget(
                        Paragraph::new(built.lines.clone()).wrap(Wrap { trim: false }),
                        frame.area(),
                    );
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let rows = (0..30)
                .map(|y| (0..36).map(|x| buffer[(x, y)].symbol()).collect::<String>())
                .collect::<Vec<_>>();
            let later_row = rows
                .iter()
                .position(|row| row.contains("LATER_MESSAGE"))
                .unwrap();
            let tool_row = rows
                .iter()
                .position(|row| row.contains("HEADER_later"))
                .unwrap();
            let geometry = built
                .message_rows
                .iter()
                .find(|row| row.start == later_index)
                .unwrap();
            assert_eq!(geometry.row_offset, later_row, "{rows:?}");
            assert_eq!(geometry.height, tool_row - later_row);
            let first_geometry = built
                .message_rows
                .iter()
                .find(|row| row.height > 0)
                .unwrap();
            assert_eq!(first_geometry.row_offset + first_geometry.height, later_row);
            let tool_geometry = built
                .tool_rows
                .iter()
                .find(|row| {
                    row.hit_kind
                        == (crate::app::CompletedTurnHitKind::ToolCall {
                            detail_index: later_index + 1,
                        })
                })
                .unwrap();
            assert_eq!(tool_geometry.row_offset, tool_row, "{rows:?}");
            for geometry in &built.tool_rows {
                assert_eq!(
                    geometry.header_width,
                    rows[geometry.row_offset].trim_end().width(),
                    "{rows:?}"
                );
            }
            assert!(line_text(&built.lines[1]).contains("MARKER"));
            assert!(!line_text(&built.lines[0]).contains("MARKER"));
        }
        for (expanded, details) in [
            (false, vec![tool("hidden", ToolCallKind::Other)]),
            (true, vec![]),
            (true, vec![empty]),
        ] {
            for prompt in ["short".into(), "界".repeat(16)] {
                let turn = CompletedTurn {
                    prompt,
                    details: details.clone(),
                    expanded,
                    trailing_marker: Some("MARKER".into()),
                };
                let built =
                    build_completed_turn_lines_with_geometry(&turn, false, false, 36, |_| false);
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(36, 10)).unwrap();
                terminal
                    .draw(|frame| {
                        frame.render_widget(
                            Paragraph::new(built.lines.clone()).wrap(Wrap { trim: false }),
                            frame.area(),
                        );
                    })
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let rows = (0..10)
                    .map(|y| (0..36).map(|x| buffer[(x, y)].symbol()).collect::<String>())
                    .collect::<Vec<_>>();
                let nonempty_rows = rows.iter().take_while(|row| !row.trim().is_empty()).count();
                assert!(rows.iter().any(|row| row.contains("MARKER")), "{rows:?}");
                assert_eq!(built.prompt_rows.len(), nonempty_rows, "{rows:?}");
                for geometry in &built.prompt_rows {
                    let rendered_width = (0..36)
                        .rev()
                        .find_map(|x| {
                            let symbol = buffer[(x, geometry.row_offset as u16)].symbol();
                            (!symbol.trim().is_empty()).then(|| usize::from(x) + symbol.width())
                        })
                        .unwrap();
                    assert_eq!(geometry.line_width, rendered_width, "{rows:?}");
                    assert_eq!(
                        geometry.body_start + geometry.body_width,
                        geometry.line_width
                    );
                }
            }
        }
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
                    query: None,
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
                    query: None,
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
            query: None,
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
        assert_eq!(line.spans[2].style, theme::TOOL_CALL_KIND);
        assert_eq!(line.spans[4].style, theme::TOOL_CALL_TITLE);
        assert_eq!(
            line.spans.get(5).map(|span| span.style),
            expected_detail_style
        );
    }

    /// A `location` hint renders as a dim secondary subject right after the
    /// title, before the status detail — guards against the card silently
    /// dropping the path/command info that `client.rs` now forwards.
    #[test]
    fn tool_call_renders_location_hint_between_title_and_status_detail() {
        let message = ChatMessage::ToolCall {
            id: "tool".into(),
            query: None,
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
            r"● Tool · Access paths outside trusted directories · C:\src\rust-app"
        );
        assert_eq!(
            lines.len(),
            1,
            "path-only tool calls should remain compact without a paragraph break"
        );
        assert_eq!(message_height(&message, 80), 1);
    }

    /// A command-kind location (`location_is_command`) must NOT be inlined
    /// while the tool is active — it gets its own subordinate
    /// `CARD_CODE`-styled `$ command` line instead, since commands can be
    /// long one-liners that would overflow or wrap awkwardly inline.
    #[test]
    fn tool_call_command_location_renders_as_separate_code_line() {
        let message = ChatMessage::ToolCall {
            id: "tool".into(),
            query: None,
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
        assert_eq!(line_text(&lines[0]), "● Run · Run command");
        assert_eq!(line_text(&lines[1]), "  └ $ cargo test --workspace");
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
            query: None,
            title: "Check installed PowerToys and Foundry Local packages".into(),
            status: "Running".into(),
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
            "  └ $ winget list --name PowerToys 2>$null"
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
    fn running_execute_tool_call_renders_cwd_reported_output_tail() {
        let cwd = concat!("C:", "\\", "repo");
        let message = ChatMessage::ToolCall {
            id: "tool".into(),
            query: None,
            title: "bash".into(),
            status: "Running".into(),
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

        assert_eq!(rendered[0], format!("● Run · bash · {cwd}"));
        assert_eq!(rendered[1], "  └ $ cargo test");
        assert_eq!(rendered[2], "    …");
        assert_eq!(rendered[3], "    line 2");
        assert_eq!(rendered[6], "    line 5");
        assert!(rendered[7].is_empty());
        assert_eq!(lines.len(), message_height(&message, 120));
    }

    #[test]
    fn successful_active_tool_call_collapses_immediately() {
        let cwd = concat!("C:", "\\", "repo");
        let message = ChatMessage::ToolCall {
            id: "tool".into(),
            query: None,
            title: "Run tests".into(),
            status: "Completed".into(),
            kind: ToolCallKind::Execute,
            location: Some("cargo test".into()),
            location_is_command: true,
            cwd: Some(cwd.into()),
            output: Some(ToolCallOutput {
                text: ["line 1", "line 2", "line 3"].join("\n"),
                truncated: false,
            }),
            exit_code: Some(0),
            content: Vec::new(),
            locations: Vec::new(),
        };

        let rendered: Vec<String> = build_message_lines(&message, false, false, None, 0, 120)
            .iter()
            .map(line_text)
            .collect();

        assert_eq!(rendered, vec!["✓ Run · cargo test"]);
    }

    #[test]
    fn successful_long_command_stays_out_of_compact_header() {
        let message = ChatMessage::ToolCall {
            id: "tool".into(),
            query: None,
            title: "Resolve cargo in active terminal context".into(),
            status: "Completed".into(),
            kind: ToolCallKind::Execute,
            location: Some(
                "wta.exe resolve-command cargo --shell pwsh --cwd C:\\repo --json".into(),
            ),
            location_is_command: true,
            cwd: Some(r"C:\repo".into()),
            output: None,
            exit_code: Some(0),
            content: Vec::new(),
            locations: Vec::new(),
        };

        let rendered: Vec<String> = build_message_lines(&message, false, false, None, 0, 120)
            .iter()
            .map(line_text)
            .collect();

        assert_eq!(
            rendered,
            vec!["✓ Run · Resolve cargo in active terminal context"]
        );
    }

    #[test]
    fn successful_read_replaces_embedded_full_path_with_compact_subject() {
        let path = r"C:\Users\kaitao\codes\rust-app\src\main.rs";
        let message = ChatMessage::ToolCall {
            id: "tool".into(),
            query: None,
            title: format!("Viewing {path}"),
            status: "Completed".into(),
            kind: ToolCallKind::Read,
            location: None,
            location_is_command: false,
            cwd: None,
            output: None,
            exit_code: None,
            content: Vec::new(),
            locations: vec![ToolCallLocation {
                path: path.into(),
                line: None,
            }],
        };

        let rendered: Vec<String> = build_message_lines(&message, false, false, None, 0, 120)
            .iter()
            .map(line_text)
            .collect();

        assert_eq!(rendered, vec![r"✓ Read · src\main.rs"]);
    }

    #[test]
    fn successful_search_compacts_workspace_subject() {
        let message = ChatMessage::ToolCall {
            id: "tool".into(),
            query: None,
            title: "Finding files matching **/*.rs".into(),
            status: "Completed".into(),
            kind: ToolCallKind::Search,
            location: Some(r"C:\Users\kaitao\codes\rust-app".into()),
            location_is_command: false,
            cwd: None,
            output: None,
            exit_code: None,
            content: Vec::new(),
            locations: Vec::new(),
        };

        let rendered: Vec<String> = build_message_lines(&message, false, false, None, 0, 120)
            .iter()
            .map(line_text)
            .collect();

        assert_eq!(
            rendered,
            vec!["✓ Search · Finding files matching **/*.rs · rust-app"]
        );
    }

    #[test]
    fn successful_edit_does_not_treat_snapshots_as_line_counts() {
        let message = ChatMessage::ToolCall {
            id: "tool".into(),
            query: None,
            title: "Update source".into(),
            status: "Completed".into(),
            kind: ToolCallKind::Edit,
            location: Some(r"C:\repo\src\chat.rs".into()),
            location_is_command: false,
            cwd: None,
            output: None,
            exit_code: None,
            content: vec![ToolCallContent::Diff {
                path: r"C:\repo\src\chat.rs".into(),
                old_text: Some(ToolCallOutput {
                    text: "old one\nold two".into(),
                    truncated: false,
                }),
                new_text: ToolCallOutput {
                    text: ["new one", "new two", "new three"].join("\n"),
                    truncated: false,
                },
            }],
            locations: Vec::new(),
        };

        let rendered: Vec<String> = build_message_lines(&message, false, false, None, 0, 120)
            .iter()
            .map(line_text)
            .collect();

        assert_eq!(rendered, vec![r"✓ Edit · src\chat.rs"]);
    }

    #[test]
    fn tool_header_hit_uses_rendered_marker_offset_and_caps_to_inner_area() {
        use ratatui::widgets::Paragraph;

        let inner_area = Rect::new(3, 2, 20, 1);
        let header = "✓ Edit file";
        let header_width = UnicodeWidthStr::width(header);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 30, 5));
        Paragraph::new(header)
            .alignment(Alignment::Right)
            .render(inner_area, &mut buffer);

        let (start_column, end_column) =
            tool_header_hit_columns(&buffer, inner_area, inner_area.y, "✓", header_width)
                .expect("right-aligned marker should be found");

        assert_eq!(
            start_column,
            inner_area.x + inner_area.width - header_width as u16
        );
        assert!(start_column > inner_area.x);
        assert_eq!(end_column, inner_area.x + inner_area.width);
    }

    #[test]
    fn failed_active_tool_call_keeps_diagnostic_preview() {
        let message = ChatMessage::ToolCall {
            id: "tool".into(),
            query: None,
            title: "Run tests".into(),
            status: "Failed".into(),
            kind: ToolCallKind::Execute,
            location: Some("cargo test".into()),
            location_is_command: true,
            cwd: None,
            output: Some(ToolCallOutput {
                text: "diagnostic".into(),
                truncated: false,
            }),
            exit_code: Some(1),
            content: Vec::new(),
            locations: Vec::new(),
        };

        let rendered: Vec<String> = build_message_lines(&message, false, false, None, 0, 120)
            .iter()
            .map(line_text)
            .collect();

        assert_eq!(rendered[0], "✗ Run · Run tests · exit 1");
        assert_eq!(rendered[1], "  └ $ cargo test");
        assert!(rendered.iter().any(|line| line.contains("diagnostic")));
    }

    #[test]
    fn nonzero_completed_command_keeps_output_preview() {
        let message = ChatMessage::ToolCall {
            id: "tool".into(),
            query: None,
            title: "Run integration command".into(),
            status: "Completed".into(),
            kind: ToolCallKind::Execute,
            location: Some("echo TOOL_DETAIL_MARKER".into()),
            location_is_command: true,
            cwd: None,
            output: Some(ToolCallOutput {
                text: "TOOL_OUTPUT_MARKER".into(),
                truncated: false,
            }),
            exit_code: Some(7),
            content: Vec::new(),
            locations: Vec::new(),
        };

        let rendered: Vec<String> = build_message_lines(&message, false, false, None, 0, 120)
            .iter()
            .map(line_text)
            .collect();

        assert_eq!(rendered[0], "✗ Run · Run integration command · exit 7");
        assert!(rendered
            .iter()
            .any(|line| line.contains("TOOL_OUTPUT_MARKER")));
    }

    #[test]
    fn successful_truncated_tool_call_stays_compact_until_expanded() {
        let message = ChatMessage::ToolCall {
            id: "tool".into(),
            query: None,
            title: "Locate project directory".into(),
            status: "Completed".into(),
            kind: ToolCallKind::Execute,
            location: Some("Get-ChildItem".into()),
            location_is_command: true,
            cwd: None,
            output: Some(ToolCallOutput {
                text: "truncated output".into(),
                truncated: true,
            }),
            exit_code: Some(0),
            content: Vec::new(),
            locations: Vec::new(),
        };

        for display in [
            ToolDisplay::ActiveTurn,
            ToolDisplay::Completed { expanded: false },
        ] {
            let rendered: Vec<String> =
                build_message_lines_with_details(&message, false, false, None, 0, 120, display)
                    .iter()
                    .map(line_text)
                    .collect();

            assert_eq!(rendered, vec!["✓ Run · Get-ChildItem"]);
        }

        let expanded: Vec<String> = build_message_lines_with_details(
            &message,
            false,
            false,
            None,
            0,
            120,
            ToolDisplay::Completed { expanded: true },
        )
        .iter()
        .map(line_text)
        .collect();
        assert_eq!(expanded[0], "✓ Run · Locate project directory");
        assert_eq!(expanded[1], "  └ $ Get-ChildItem");
        assert!(expanded
            .iter()
            .any(|line| line.contains("truncated output")));
    }

    #[test]
    fn successful_truncated_reads_group_and_deduplicate_targets() {
        let messages = (0..4)
            .map(|index| ChatMessage::ToolCall {
                id: format!("read-{index}"),
                query: None,
                title: "Read project".into(),
                status: "Completed".into(),
                kind: ToolCallKind::Read,
                location: Some(r"C:\codes\rust-app".into()),
                location_is_command: false,
                cwd: None,
                output: Some(ToolCallOutput {
                    text: "extension summary".into(),
                    truncated: true,
                }),
                exit_code: None,
                content: Vec::new(),
                locations: Vec::new(),
            })
            .collect::<Vec<_>>();

        assert_eq!(previous_message_group_start(&messages, messages.len()), 0);
        let rendered = build_compact_tool_group_lines(&messages);
        assert_eq!(line_text(&rendered[0]), "✓ Read · rust-app · +3");
    }

    #[test]
    fn successful_file_mutations_group_by_kind_and_deduplicate_targets() {
        for (kind, label) in [
            (ToolCallKind::Edit, "Edit"),
            (ToolCallKind::Delete, "Delete"),
        ] {
            let messages = [
                (r"C:\repo\src\chat.rs", "Completed"),
                (r"C:\repo\src\chat.rs", "Completed"),
                (r"C:\repo\src\app.rs", "Completed"),
            ]
            .into_iter()
            .enumerate()
            .map(|(index, (path, status))| ChatMessage::ToolCall {
                id: format!("mutation-{index}"),
                query: None,
                title: "Mutate file".into(),
                status: status.into(),
                kind,
                location: Some(path.into()),
                location_is_command: false,
                cwd: None,
                output: None,
                exit_code: None,
                content: Vec::new(),
                locations: Vec::new(),
            })
            .collect::<Vec<_>>();

            assert_eq!(previous_message_group_start(&messages, messages.len()), 0);
            let rendered = build_compact_tool_group_lines(&messages);
            assert_eq!(
                line_text(&rendered[0]),
                format!("✓ {label} · chat.rs, app.rs · +1")
            );
        }
    }

    #[test]
    fn failed_file_mutation_breaks_compact_group() {
        let messages = ["Completed", "Failed", "Completed"]
            .into_iter()
            .enumerate()
            .map(|(index, status)| ChatMessage::ToolCall {
                id: format!("edit-{index}"),
                query: None,
                title: "Edit file".into(),
                status: status.into(),
                kind: ToolCallKind::Edit,
                location: Some(format!("file-{index}.rs")),
                location_is_command: false,
                cwd: None,
                output: None,
                exit_code: None,
                content: Vec::new(),
                locations: Vec::new(),
            })
            .collect::<Vec<_>>();

        assert_eq!(previous_message_group_start(&messages, messages.len()), 2);
    }

    #[test]
    fn running_non_execute_tool_call_shows_bounded_output_preview() {
        let location = concat!("C:", "\\", "repo", "\\", "large.txt");
        let message = ChatMessage::ToolCall {
            id: "tool".into(),
            query: None,
            title: "Read file".into(),
            status: "Running".into(),
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

        assert_eq!(rendered[1], "  └ …");
        assert_eq!(rendered[2], "    line 3");
        assert_eq!(rendered[3], "    line 4");
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
        assert_eq!(lines[0].text, "    │ …");
        assert!(lines
            .last()
            .is_some_and(|line| line.text.ends_with("object-199.o")));
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
        assert_eq!(lines.last().map(|line| line.text.as_str()), Some("    …"));
    }

    #[test]
    fn expanded_diff_details_render_only_real_changes() {
        let old = ["before", "same", "old value", "after"].join("\n");
        let new = ["before", "same", "new value", "after"].join("\n");
        let content = ToolCallContent::Diff {
            path: "src/main.rs".into(),
            old_text: Some(ToolCallOutput {
                text: old,
                truncated: false,
            }),
            new_text: ToolCallOutput {
                text: new,
                truncated: false,
            },
        };

        let lines = tool_detail_lines(&[content], &[], true);

        assert_eq!(
            lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec![
                "    Δ src/main.rs",
                "    │ before",
                "    │ same",
                "    - old value",
                "    + new value",
                "    │ after",
            ]
        );
        assert_eq!(lines[2].style, theme::DIM);
        assert_eq!(lines[3].style, theme::TOOL_DIFF_REMOVED);
        assert_eq!(lines[4].style, theme::TOOL_DIFF_ADDED);
    }

    #[test]
    fn expanded_new_file_diff_renders_additions() {
        let content = ToolCallContent::Diff {
            path: "src/new.rs".into(),
            old_text: None,
            new_text: ToolCallOutput {
                text: ["first", "second"].join("\n"),
                truncated: false,
            },
        };

        let lines = tool_detail_lines(&[content], &[], true);

        assert_eq!(
            lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec!["    Δ src/new.rs", "    + first", "    + second",]
        );
    }

    #[test]
    fn expanded_diff_details_respect_the_global_line_cap() {
        let content = ToolCallContent::Diff {
            path: "src/large.rs".into(),
            old_text: Some(ToolCallOutput {
                text: (0..300)
                    .map(|index| format!("old {index}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                truncated: false,
            }),
            new_text: ToolCallOutput {
                text: (0..300)
                    .map(|index| format!("new {index}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                truncated: false,
            },
        };

        let lines = tool_detail_lines(&[content], &[], true);

        let removed = lines
            .iter()
            .filter(|line| line.text.starts_with("    - "))
            .count();
        let added = lines
            .iter()
            .filter(|line| line.text.starts_with("    + "))
            .count();
        assert!(lines.len() <= MAX_TOOL_DETAIL_LINES);
        assert!(removed > 0);
        assert!(added > 0);
        assert!(removed.abs_diff(added) <= 1);
        assert!(lines.iter().any(|line| line.text == "    │ …"));
    }

    #[test]
    fn ordinary_marker_like_location_keeps_dim_style() {
        let locations = vec![ToolCallLocation {
            path: "+ notes.txt".into(),
            line: None,
        }];

        let lines = tool_detail_lines(&[], &locations, true);

        assert_eq!(lines[0].text, "    + notes.txt");
        assert_eq!(lines[0].style, theme::DIM);
    }

    #[test]
    fn diff_header_path_is_bounded() {
        let path = "x".repeat(MAX_TOOL_OUTPUT_LINE_CHARS + 20);
        let new_text = ToolCallOutput {
            text: "content".into(),
            truncated: false,
        };

        let lines = diff_detail_lines(&path, None, &new_text, true, 4);

        assert!(lines[0].text.ends_with('…'));
        assert_eq!(
            lines[0].text.chars().count(),
            "    Δ ".chars().count() + MAX_TOOL_OUTPUT_LINE_CHARS + 1
        );
        assert_eq!(lines[0].style, theme::TOOL_DIFF_HEADER);
    }

    #[test]
    fn diff_with_fallback_output_respects_the_global_detail_cap() {
        let old = (0..100)
            .map(|index| format!("old {index}"))
            .collect::<Vec<_>>();
        let new = (0..100)
            .map(|index| format!("new {index}"))
            .collect::<Vec<_>>();
        let message = ChatMessage::ToolCall {
            id: "tool".into(),
            query: None,
            title: "Update source".into(),
            status: "Completed".into(),
            kind: ToolCallKind::Edit,
            location: Some("src/main.rs".into()),
            location_is_command: false,
            cwd: None,
            output: Some(ToolCallOutput {
                text: (0..MAX_TOOL_DETAIL_OUTPUT_LINES)
                    .map(|index| format!("raw output {index}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                truncated: false,
            }),
            exit_code: None,
            content: vec![ToolCallContent::Diff {
                path: "src/main.rs".into(),
                old_text: Some(ToolCallOutput {
                    text: old.join("\n"),
                    truncated: false,
                }),
                new_text: ToolCallOutput {
                    text: new.join("\n"),
                    truncated: false,
                },
            }],
            locations: Vec::new(),
        };

        let rendered = build_message_lines_with_details(
            &message,
            false,
            false,
            None,
            0,
            120,
            ToolDisplay::Completed { expanded: true },
        )
        .iter()
        .map(line_text)
        .collect::<Vec<_>>();

        assert_eq!(rendered.len(), MAX_TOOL_DETAIL_LINES + 2);
        assert_eq!(
            rendered.get(rendered.len() - 2).map(String::as_str),
            Some("    …")
        );
    }

    #[test]
    fn tool_call_uses_semantic_status_markers() {
        assert_tool_call(
            "Pending",
            "● Tool · Run: cargo test",
            theme::TOOL_CALL_PENDING,
            None,
        );
        assert_tool_call(
            "running",
            "● Tool · Run: cargo test",
            theme::TOOL_CALL_RUNNING,
            None,
        );
        assert_tool_call(
            "Completed",
            "✓ Tool · Run: cargo test",
            theme::TOOL_CALL_SUCCESS,
            None,
        );
        assert_tool_call(
            "Failed: exit code 1",
            "✗ Tool · Run: cargo test · exit code 1",
            theme::TOOL_CALL_FAILURE,
            Some(theme::DIM),
        );
        assert_tool_call(
            "Canceled",
            "− Tool · Run: cargo test",
            theme::TOOL_CALL_CANCELED,
            None,
        );
        assert_tool_call(
            "Exited (1)",
            "✗ Tool · Run: cargo test · Exited (1)",
            theme::TOOL_CALL_FAILURE,
            Some(theme::DIM),
        );
        // "Exited (0)" is a success alias (distinct from the generic
        // "exited (" failure prefix matched above) and carries no detail.
        assert_tool_call(
            "Exited (0)",
            "✓ Tool · Run: cargo test",
            theme::TOOL_CALL_SUCCESS,
            None,
        );
        // Status matching is case-insensitive across the success paths.
        assert_tool_call(
            "COMPLETED",
            "✓ Tool · Run: cargo test",
            theme::TOOL_CALL_SUCCESS,
            None,
        );
        assert_tool_call(
            "eXiTeD (0)",
            "✓ Tool · Run: cargo test",
            theme::TOOL_CALL_SUCCESS,
            None,
        );
        // Unknown/future statuses fall back to a dim marker with the raw
        // status surfaced as dim detail text, instead of panicking or
        // silently dropping the status.
        assert_tool_call(
            "SomeFutureStatus",
            "• Tool · Run: cargo test · SomeFutureStatus",
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

    #[test]
    fn thought_source_rows_cover_actual_rendered_body_rows() {
        let _locale = crate::test_support::lock_locale();
        rust_i18n::set_locale("en-US");
        let text = [format!("a {}", "a".repeat(45)), "tail".into()].join("\n");
        let message = ChatMessage::Thought {
            id: Default::default(),
            text,
            expanded: true,
            duration_ms: None,
        };
        let width = 48;
        let lines = build_message_lines(&message, false, false, None, 0, width);
        let header = rendered_lines_height(&lines[..1], width);
        let height = rendered_lines_height(&lines, width);
        let area = Rect::new(0, 0, width as u16, height as u16);
        let mut buffer = Buffer::empty(area);
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, &mut buffer);
        let rows = thought_source_rows(&message, width, None);
        assert_eq!(
            rows.iter().map(|(_, row)| *row).collect::<Vec<_>>(),
            (header..height).collect::<Vec<_>>(),
            "{buffer:?}",
        );
    }

    #[test]
    fn thought_first_fit_body_rows_match_ratatui_geometry() {
        let _locale = crate::test_support::lock_locale();
        rust_i18n::set_locale("en-US");
        for text in [
            [format!("a {}", "a".repeat(45)), "tail".into()].join("\n"),
            "界e\u{301} alpha-beta 👩‍💻 🙂\r\n\r\nend\r\n".into(),
            [
                format!("  {}  ", ["same"; 6].join(" ")),
                ["", "value", "end"].join("\t"),
            ]
            .join("\n"),
            "\u{1b}[31mcolored\u{1b}[0m text".into(),
        ] {
            for width in [1, 2, 3, 4, 7, 12, 20, 48] {
                let message = ChatMessage::Thought {
                    id: Default::default(),
                    text: text.clone(),
                    expanded: true,
                    duration_ms: None,
                };
                let lines = build_message_lines(&message, false, false, None, 0, width);
                let header = rendered_lines_height(&lines[..1], width);
                let height = rendered_lines_height(&lines, width);
                let area = Rect::new(0, 0, width as u16, height as u16);
                let mut buffer = Buffer::empty(area);
                Paragraph::new(lines.clone())
                    .wrap(Wrap { trim: false })
                    .render(area, &mut buffer);
                let rows = thought_source_rows(&message, width, None);
                assert_eq!(
                    rows.iter().map(|(_, row)| *row).collect::<Vec<_>>(),
                    (header..height).collect::<Vec<_>>(),
                    "width={width}, text={text:?}, buffer={buffer:?}",
                );
                assert!(rows.iter().all(|(byte, _)| text.is_char_boundary(*byte)));
                for (index, line) in lines[1..].iter().enumerate() {
                    let area = Rect::new(0, 0, width as u16, 2);
                    let mut single = Buffer::empty(area);
                    Paragraph::new(line.clone())
                        .wrap(Wrap { trim: false })
                        .render(area, &mut single);
                    for x in 0..width as u16 {
                        assert_eq!(single[(x, 0)], buffer[(x, (header + index) as u16)]);
                        assert_eq!(single[(x, 1)].symbol(), " ");
                    }
                }
            }
        }
    }

    #[test]
    fn thought_source_wrapping_matches_textwrap_after_every_utf8_head_cut() {
        let repeated = ["repeated"; 2].join(" ");
        let padded = format!("  {repeated}  repeated ");
        let text = [
            padded.as_str(),
            "",
            "界e\u{301} alpha-beta 界e\u{301} internationalization",
            "",
            repeated.as_str(),
            "",
        ]
        .join("\r\n");
        for cut in text
            .char_indices()
            .map(|(byte, _)| byte)
            .chain([text.len()])
        {
            let retained = &text[cut..];
            for width in [1, 2, 3, 4, 7, 12, 20, 48] {
                let actual = wrap_thought_text(retained, width);
                let expected = retained
                    .split('\n')
                    .flat_map(|paragraph| {
                        textwrap::wrap(
                            paragraph.strip_suffix('\r').unwrap_or(paragraph),
                            textwrap::Options::new(
                                width.saturating_sub(thought_gutter(width).width()).max(1),
                            )
                            .wrap_algorithm(textwrap::WrapAlgorithm::FirstFit),
                        )
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    actual.iter().map(|(_, piece)| piece).collect::<Vec<_>>(),
                    expected.iter().collect::<Vec<_>>(),
                    "cut={cut}, width={width}",
                );
                assert!(actual
                    .iter()
                    .all(|(byte, _)| retained.is_char_boundary(*byte)));
                assert!(actual.windows(2).all(|rows| rows[0].0 <= rows[1].0));
            }
        }
    }

    #[test]
    fn thought_source_offsets_distinguish_repeated_and_empty_rows() {
        let text = ["same", "", "same", "", "same", ""].join("\r\n");
        let rows = wrap_thought_text(&text, 20);
        assert_eq!(
            rows.iter().map(|(byte, _)| *byte).collect::<Vec<_>>(),
            [0, 6, 8, 14, 16, 22],
        );
        let repeated = ["same"; 3].join(" ");
        let rows = wrap_thought_text(&repeated, 7);
        assert_eq!(
            rows,
            [
                (0, Cow::Borrowed("same")),
                (5, Cow::Borrowed("same")),
                (10, Cow::Borrowed("same"))
            ]
        );

        // A CRLF can be split by the retention boundary. The empty first
        // paragraph and the next identical line still have distinct positions.
        let rows = wrap_thought_text(&text[5..], 20);
        assert_eq!(
            rows.iter().map(|(byte, _)| *byte).collect::<Vec<_>>(),
            [0, 1, 3, 9, 11, 17],
        );
    }

    #[test]
    fn thought_render_has_muted_italic_header_and_rule_on_every_body_row() {
        let _locale = crate::test_support::lock_locale();
        rust_i18n::set_locale("en-US");
        let mut message = ChatMessage::Thought {
            id: Default::default(),
            text: ["思考 reasoning that wraps", "", "next"].join("\n"),
            expanded: true,
            duration_ms: Some(3000),
        };
        let lines = build_message_lines(&message, false, false, None, 0, 20);
        assert_eq!(lines[0].to_string(), "▼ Think · 3.0s");
        assert!(lines.len() > 3);
        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                assert!(line.to_string().starts_with('│'));
            }
            assert!(line.width() <= 20);
            for span in &line.spans {
                assert_eq!(span.style.fg, theme::DIM.fg);
                assert!(span.style.add_modifier.contains(Modifier::ITALIC));
            }
        }
        let geometry = thought_row_geometry(&message, 2, false, 4, &lines, 20).unwrap();
        assert_eq!(geometry.row_offset, 4);
        assert_eq!(geometry.header_width, lines[0].width());
        assert!(matches!(
            geometry.hit_kind,
            crate::app::CompletedTurnHitKind::Thought {
                detail_index: 2,
                active: false,
                ..
            }
        ));
        if let ChatMessage::Thought { expanded, .. } = &mut message {
            *expanded = false;
        }
        let lines = build_message_lines(&message, false, false, None, 0, 20);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].to_string(), "▶ Think · 3.0s");
    }

    #[test]
    fn thought_render_crlf_preserves_blank_lines_and_wrapping_without_control_characters() {
        let _locale = crate::test_support::lock_locale();
        rust_i18n::set_locale("en-US");
        let paragraphs = ["", "思考 reasoning that wraps", "", "next", ""];
        let render = |text| {
            let message = ChatMessage::Thought {
                id: Default::default(),
                text,
                expanded: true,
                duration_ms: None,
            };
            build_message_lines(&message, false, false, None, 0, 20)
                .iter()
                .map(|line| {
                    assert!(line.width() <= 20);
                    assert!(line.spans.iter().all(|span| !span.content.contains('\r')));
                    line.to_string()
                })
                .collect::<Vec<_>>()
        };
        let lines = render(paragraphs.join("\r\n"));
        assert_eq!(lines, render(paragraphs.join("\n")));
        assert_eq!(
            lines,
            [
                "▼ Think",
                "│ ",
                "│ 思考 reasoning",
                "│ that wraps",
                "│ ",
                "│ next",
                "│ "
            ]
        );
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
            query: None,
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
            query: None,
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
                query: None,
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
    fn dot_prefix_discards_trailing_blank_lines() {
        let mut lines = Vec::new();
        push_dot_prefixed_lines(&mut lines, "A\n\n", 40, theme::DOT_AGENT, theme::AGENT_TEXT);

        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "● A");
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
