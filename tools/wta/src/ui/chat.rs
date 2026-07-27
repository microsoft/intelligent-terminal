use std::borrow::Cow;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::{App, ChatMessage, CompletedTurn, PlanEntryStatus};
use crate::theme;
use crate::ui::shimmer;
use crate::ui_trace;

fn activity_label() -> String { t!("chat.activity_thinking").into_owned() }

const MAX_RENDER_LINE_CHARS: usize = 4096;

/// Estimate the chat block's natural height (in visual rows) given the
/// rendering width. Counts wraps for each message + completed turn plus the
/// pinned activity row when active. Used by `layout::render` to size the
/// chat area so the rec panel sits directly below content instead of being
/// pushed to the pane bottom by a `Min(1)` spacer.
pub fn estimated_block_height(app: &App, area_width: u16) -> u16 {
    let tab = app.current_tab();
    let wrap_width = (area_width as usize).max(1);
    // Fetch once for the pending-height calculation. `pending_render_text`
    // re-parses the streaming buffer on every call (and allocates on the
    // JSON-wrapper path via `extract_json_string_field`).
    let pending_text = pending_render_text(tab);

    // Connecting and Thinking both pin one activity row in `render`; reserve
    // that same row here so the surrounding layout cannot jump or clip.
    let activity = if matches!(app.state, crate::app::ConnectionState::Connecting(_))
        || should_show_turn_activity(tab)
    {
        1usize
    } else {
        0
    };

    let messages: usize = tab.messages.iter().map(|m| message_height(m, wrap_width)).sum();
    let turns: usize = tab.completed_turns.iter().map(|t| turn_height(t, wrap_width)).sum();
    let pending = pending_text
        .as_deref()
        .map(|text| {
            let body_width = wrap_width.saturating_sub(2).max(1);
            dot_wrap_count(text, body_width)
        })
        .unwrap_or(0);
    // Welcome overlay sits above all chat content when `show_welcome_hint`
    // is on; must be counted here or else any pushed message will scroll
    // it off the top of the visible chat block. Always a single row —
    // terminal min-width guarantees the localized title fits without
    // wrapping.
    let welcome = if app.show_welcome_hint
        && app.state == crate::app::ConnectionState::Connected
    {
        1
    } else {
        0
    };

    (activity + messages + turns + pending + welcome).max(1).min(u16::MAX as usize) as u16
}

fn wrap_count(text: &str, width: usize) -> usize {
    let w = width.max(1);
    text.split('\n')
        .map(|line| {
            let chars = line.chars().count();
            if chars == 0 { 1 } else { chars.div_ceil(w) }
        })
        .sum::<usize>()
        .max(1)
}

/// Mirrors `push_dot_prefixed_lines`: leading blank paragraphs are skipped
/// (the dot lands on the first content row), so they must not be counted
/// against the chat-area height either.
fn dot_wrap_count(text: &str, width: usize) -> usize {
    wrap_count(text.trim_start_matches('\n'), width)
}

fn message_height(msg: &ChatMessage, wrap_width: usize) -> usize {
    // Most variants render with a 2-cell prefix ("● " for agent/error,
    // "> " for user) and a trailing blank line.
    let body_width = wrap_width.saturating_sub(2).max(1);
    match msg {
        ChatMessage::Agent(t) | ChatMessage::Error(t) => dot_wrap_count(t, body_width) + 1,
        ChatMessage::User(t) => wrap_count(t, body_width) + 1,
        ChatMessage::System(t) | ChatMessage::AgentEvent(t) => wrap_count(t, wrap_width) + 1,
        ChatMessage::ToolCall { .. } => 1,
        ChatMessage::Plan(entries) => 2 + entries.len(), // header + each entry + blank
        // Disclaimer is a single dim row — terminal min-width guarantees the
        // short text fits without wrapping, and no trailing blank is needed.
        ChatMessage::Disclaimer => 1,
    }
}

fn turn_height(turn: &CompletedTurn, wrap_width: usize) -> usize {
    // Collapsed view = single Line "▶ > <prompt>" + trailing blank.
    let chars = "▶ > ".chars().count() + turn.prompt.chars().count();
    let prompt_rows = chars.div_ceil(wrap_width.max(1)).max(1);
    let mut h = prompt_rows + 1;
    if turn.expanded {
        h += turn
            .details
            .iter()
            .map(|m| message_height(m, wrap_width))
            .sum::<usize>();
    }
    h
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
    } else if status.eq_ignore_ascii_case("completed") || status.eq_ignore_ascii_case("exited (0)") {
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

    // Pin the activity indicator to a dedicated bottom row when active so a
    // long user prompt that wraps past the chat height can never push it
    // off-screen. The remaining rows scroll normally.
    let activity_line = build_activity_line(app);
    let (chat_area, activity_area) = match (&activity_line, area.height) {
        (Some(_), h) if h > 0 => (
            Rect { height: h - 1, ..area },
            Some(Rect { x: area.x, y: area.y + h - 1, width: area.width, height: 1 }),
        ),
        _ => (area, None),
    };

    let inner = Block::default().borders(Borders::NONE);
    let inner_area = inner.inner(chat_area);
    let visible_height = inner_area.height as usize;
    let wrap_width = inner_area.width as usize;
    let requested_lines = visible_height
        .saturating_add(app.current_tab().chat_scroll.offset)
        .saturating_add(32);

    let mut reversed_lines: Vec<Line> = Vec::new();

    let mut pending_lines = build_pending_stream_lines(app, wrap_width);
    reversed_lines.extend(pending_lines.drain(..).rev());

    let mut truncated = false;

    let tab = app.current_tab();
    let permission_tool_call_id = permission_tool_call_id(tab);
    for (idx, msg) in tab.messages.iter().enumerate().rev() {
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
        if reversed_lines.len() >= requested_lines {
            truncated = true;
            break;
        }
    }

    if !truncated {
        let selected_idx = app.current_tab().selected_completed_turn_idx;
        let pane_focused = app.pane_focused;
        for (idx, turn) in app.current_tab().completed_turns.iter().enumerate().rev() {
            let is_selected = selected_idx == Some(idx);
            let mut turn_lines = build_completed_turn_lines(turn, is_selected, pane_focused, wrap_width);
            reversed_lines.extend(turn_lines.drain(..).rev());
            if reversed_lines.len() >= requested_lines {
                truncated = true;
                break;
            }
        }
    }

    // First-run welcome: shown once until user sends first message
    if app.show_welcome_hint
        && app.state == crate::app::ConnectionState::Connected
    {
        let mut welcome_lines = vec![
            Line::from(vec![
                Span::styled("● ", Style::new().fg(Color::Reset).add_modifier(Modifier::BOLD)),
                Span::styled(
                    t!("chat.welcome_title").into_owned(),
                    Style::new().fg(Color::Reset).add_modifier(Modifier::BOLD),
                ),
            ]),
        ];
        reversed_lines.extend(welcome_lines.drain(..).rev());
    }

    let lines: Vec<Line> = reversed_lines.into_iter().rev().collect();

    let total_lines = lines.len();
    let scroll = total_lines.saturating_sub(visible_height.saturating_add(app.current_tab().chat_scroll.offset));

    let paragraph = Paragraph::new(lines)
        .block(inner)
        .alignment(crate::rtl::text_alignment())
        .wrap(Wrap { trim: false })
        .scroll((scroll as u16, 0));

    frame.render_widget(paragraph, chat_area);

    if let (Some(line), Some(act_area)) = (activity_line, activity_area) {
        frame.render_widget(Paragraph::new(line), act_area);
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
            app.current_tab().turn.buffer().map(|b| b.chars().count()).unwrap_or(0),
            requested_lines,
            visible_height,
            area.width,
            area.height
        )
    });
}

fn build_completed_turn_lines<'a>(
    turn: &'a crate::app::CompletedTurn,
    is_selected: bool,
    pane_focused: bool,
    wrap_width: usize,
) -> Vec<Line<'a>> {
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

    // The collapsed header is always a single `Line` by design (see
    // `turn_height`'s "Collapsed view = single Line" comment above), so a
    // multi-line prompt (Shift+Enter) can't keep its line breaks here. Without
    // this, the embedded '\n' would vanish invisibly and run the two lines
    // together with no separator at all (e.g. "remember,And ..."), since
    // ratatui doesn't render embedded newlines as whitespace. Replace each
    // '\n' with a space so the collapsed preview stays readable.
    // Only allocate when the collapse step actually rewrote the text (i.e.
    // the prompt had an embedded '\n'); the common single-line, non-wrapped
    // prompt stays a zero-copy borrow of `turn.prompt` for the `'a` lifetime.
    let collapsed_prompt = collapse_newlines_for_preview(&turn.prompt);
    let prompt_text: Cow<'a, str> = match collapsed_prompt {
        Cow::Borrowed(_) => truncate_render_text(&turn.prompt),
        // `collapsed` is already an owned `String`; only clone again if
        // truncation actually shortens it; otherwise reuse it as-is instead
        // of cloning a second time via `truncate_render_text(..).into_owned()`.
        Cow::Owned(collapsed) => match truncate_render_text(&collapsed) {
            Cow::Borrowed(_) => Cow::Owned(collapsed),
            Cow::Owned(truncated) => Cow::Owned(truncated),
        },
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(chevron, chevron_style),
        Span::styled("> ", prompt_style),
        Span::styled(prompt_text, prompt_style),
    ])];

    // Index of the line that should receive an inline trailing marker (eg
    // "(canceled)" / "→ executed: …"). Expanded turns attach it to the
    // first detail row (right after the header chevron line); collapsed
    // turns put it next to the prompt header.
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
            lines.extend(build_message_lines(msg, false, false, None, 0, wrap_width));
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
    // System / Plan / AgentEvent all trail a blank via build_message_lines;
    // ToolCall does not, and collapsed turns stop at the prompt header.
    if lines.last().map_or(true, |l| !l.spans.is_empty()) {
        lines.push(Line::default());
    }
    lines
}

fn build_activity_line(app: &App) -> Option<Line<'static>> {
    // While the helper is still establishing its connection to the agent,
    // show an animated "Connecting to agent…" line (F7). The handshake
    // (pipe connect → ACP init → session/new) can take tens of seconds on a
    // cold start; without an animated indicator the pane looked frozen. Uses
    // the app-level `activity_frame`, which is advanced on Tick while the
    // state is `Connecting` (see handle_event). Takes precedence over the
    // turn spinner because no turn can be in flight before we're connected.
    if matches!(app.state, crate::app::ConnectionState::Connecting(_)) {
        let label = t!("connection.connecting_activity").into_owned();
        return Some(Line::from(shimmer::shimmer_spans(&label, app.activity_frame as usize)));
    }
    let tab = app.current_tab();
    if !should_show_turn_activity(tab) {
        return None;
    }
    let label = activity_label();
    Some(Line::from(shimmer::shimmer_spans(
        &label,
        tab.activity_frame,
    )))
}

/// Incrementally extracts a JSON string field's decoded value from a
/// possibly-truncated text. Handles `\"`, `\\`, `\n`, `\t`, `\uXXXX` and
/// UTF-16 surrogate pairs (e.g. emoji). Returns the partial value if the
/// closing quote hasn't arrived yet.
pub(crate) fn extract_json_string_field(text: &str, field: &str) -> Option<String> {
    let key = format!("\"{field}\"");
    // Find the occurrence of `"field"` that is actually a *key* (followed by
    // `:`), not the same token appearing earlier as a string value. Without
    // this, `{"kind":"explanation","explanation":"real"}` would stop at the
    // value and return None.
    let mut search_from = 0;
    let rest = loop {
        let rel = text[search_from..].find(&key)?;
        let abs = search_from + rel;
        let after = text[abs + key.len()..].trim_start();
        if let Some(r) = after.strip_prefix(':') {
            break r.trim_start();
        }
        search_from = abs + key.len();
    };
    let body = rest.strip_prefix('"')?;

    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next() {
                None => return Some(out),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('/') => out.push('/'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('b') => out.push('\u{08}'),
                Some('f') => out.push('\u{0C}'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if hex.len() < 4 {
                        return Some(out);
                    }
                    let Some(code) = u32::from_str_radix(&hex, 16).ok() else {
                        continue;
                    };
                    match code {
                        // High surrogate: pair it with the following
                        // `\uXXXX` low surrogate to recover the non-BMP scalar
                        // (e.g. emoji). If the low half hasn't streamed in yet
                        // (or is malformed), drop the lone surrogate — the next
                        // frame re-runs over the now-complete buffer.
                        0xD800..=0xDBFF => {
                            let mut lookahead = chars.clone();
                            if lookahead.next() == Some('\\')
                                && lookahead.next() == Some('u')
                            {
                                let lo_hex: String = lookahead.by_ref().take(4).collect();
                                if lo_hex.len() == 4 {
                                    if let Some(lo @ 0xDC00..=0xDFFF) =
                                        u32::from_str_radix(&lo_hex, 16).ok()
                                    {
                                        let scalar = 0x1_0000
                                            + ((code - 0xD800) << 10)
                                            + (lo - 0xDC00);
                                        if let Some(ch) = char::from_u32(scalar) {
                                            out.push(ch);
                                        }
                                        chars = lookahead; // consume the low half
                                    }
                                }
                            }
                        }
                        // Lone low surrogate or any non-scalar: skip. Valid
                        // scalars get pushed.
                        _ => {
                            if let Some(ch) = char::from_u32(code) {
                                out.push(ch);
                            }
                        }
                    }
                }
                Some(other) => out.push(other),
            },
            c => out.push(c),
        }
    }
    Some(out)
}

/// Resolves the user-visible portion of a streaming buffer:
///
/// - Buffer starts with a JSON wrapper (autofix): extract the `explanation`
///   field so the user sees flowing markdown rather than raw JSON syntax.
///   fix actions lack this field and yield None — the card surfaces on
///   finalize.
/// - Buffer is mixed prose followed by a fenced JSON block (planner
///   terminal-task mode): render only the prose prefix; the recommendation
///   card replaces it on eager/end-of-turn finalize.
/// - Pure prose: stream as-is.
///
/// Callers outside the render path (e.g. turn-cancel / ignore commits) use
/// this to record exactly what the user saw during streaming, instead of the
/// raw buffer (which may contain JSON the UI deliberately hid).
pub(crate) fn user_visible_stream_text(text: &str) -> Option<Cow<'_, str>> {
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("```") || trimmed.starts_with('{') {
        return extract_json_string_field(text, "explanation")
            .filter(|s| !s.is_empty())
            .map(Cow::Owned);
    }
    if let Some(fence_pos) = text.find("```") {
        let prose = text[..fence_pos].trim_end();
        return if prose.is_empty() {
            None
        } else {
            Some(Cow::Borrowed(prose))
        };
    }
    Some(Cow::Borrowed(text))
}

fn pending_render_text(tab: &crate::app::TabSession) -> Option<Cow<'_, str>> {
    // Pending text is only meaningful while the turn is actively streaming.
    user_visible_stream_text(tab.turn.buffer()?)
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
    // full text is always in `turn.buffer()`, and finalize commits it in full,
    // so this never drops or delays the final content.
    let revealed: Cow<'_, str> = {
        let total = text.chars().count();
        let shown = tab.reveal_chars.min(total);
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
    let mut lines = Vec::new();
    match msg {
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
        ChatMessage::ToolCall {
            id,
            title,
            status,
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
            if let Some(detail) = detail.filter(|detail| !detail.is_empty()) {
                spans.push(Span::styled(
                    format!(" · {}", truncate_render_text(detail)),
                    theme::DIM,
                ));
            }
            lines.push(Line::from(spans));
        }
        ChatMessage::Plan(entries) => {
            lines.push(Line::from(Span::styled(t!("chat.plan_header").into_owned(), theme::PLAN_STYLE)));
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

/// Mirrors `push_dot_prefixed_lines`, but for the user's own submitted
/// prompt: splits on embedded `\n` (from Shift+Enter multi-line input) and
/// wraps each paragraph so every line is a real `ratatui::Line` — ratatui
/// does not turn an embedded `\n` inside a single `Span`/`Line` into
/// multiple rows, so without this split any line after the first would
/// never appear in the rendered transcript (see issue #492). The first
/// rendered row gets the `"> "` prompt marker; continuation rows get a
/// matching 2-cell indent, consistent with `message_height`'s
/// `wrap_count`-based row estimate for `ChatMessage::User`.
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
        };
        let lines = build_message_lines(&message, false, false, None, 0, 80);
        let line = &lines[0];

        assert_eq!(line_text(line), expected_text);
        assert_eq!(line.spans[0].style, expected_marker_style);
        assert_eq!(line.spans[2].style, theme::TOOL_CALL_TITLE);
        assert_eq!(line.spans.get(3).map(|span| span.style), expected_detail_style);
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

    // ── extract_json_string_field: escape decoding ──────────────────────────

    #[test]
    fn json_field_basic_value() {
        assert_eq!(
            extract_json_string_field(r#"{"explanation":"hello"}"#, "explanation")
                .as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn json_field_decodes_escapes() {
        // \" \\ \/ \n \r \t all per RFC 8259.
        let raw = r#"{"explanation":"a\"b\\c\/d\ne\tf"}"#;
        assert_eq!(
            extract_json_string_field(raw, "explanation").as_deref(),
            Some("a\"b\\c/d\ne\tf")
        );
    }

    #[test]
    fn json_field_decodes_bmp_unicode_escape() {
        // \u0041 = 'A', \u00e9 = 'é'
        assert_eq!(
            extract_json_string_field(r#"{"explanation":"\u0041\u00e9"}"#, "explanation")
                .as_deref(),
            Some("Aé")
        );
    }

    #[test]
    fn json_field_tolerates_whitespace_around_colon() {
        assert_eq!(
            extract_json_string_field("{ \"explanation\" : \"v\" }", "explanation")
                .as_deref(),
            Some("v")
        );
    }

    #[test]
    fn json_field_returns_partial_when_unterminated() {
        // Streaming: the closing quote hasn't arrived yet — show what we have.
        assert_eq!(
            extract_json_string_field(r#"{"explanation":"hello world"#, "explanation")
                .as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn json_field_absent_returns_none() {
        assert_eq!(
            extract_json_string_field(r#"{"command":"ls"}"#, "explanation"),
            None
        );
    }

    // ── extract_json_string_field: ADVERSARIAL (expected to expose gaps) ─────

    /// A non-BMP character (emoji) encoded as a UTF-16 surrogate pair must
    /// decode to the actual character. Agents routinely emit emoji in prose.
    #[test]
    fn json_field_decodes_surrogate_pair_emoji() {
        // U+1F600 😀 = \uD83D\uDE00 in UTF-16.
        assert_eq!(
            extract_json_string_field(r#"{"explanation":"\uD83D\uDE00"}"#, "explanation")
                .as_deref(),
            Some("😀")
        );
    }

    /// When the field name also appears earlier as a *value*, extraction must
    /// still find the real key=value pair, not give up at the first textual
    /// match.
    #[test]
    fn json_field_skips_name_appearing_as_value() {
        let raw = r#"{"kind":"explanation","explanation":"real"}"#;
        assert_eq!(
            extract_json_string_field(raw, "explanation").as_deref(),
            Some("real")
        );
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
    fn stream_text_json_wrapper_extracts_explanation() {
        assert_eq!(
            user_visible_stream_text(r#"{"explanation":"why blue"}"#).as_deref(),
            Some("why blue")
        );
    }

    #[test]
    fn stream_text_json_without_explanation_is_hidden() {
        // A fix-action wrapper (no explanation) must not leak raw JSON.
        assert_eq!(user_visible_stream_text(r#"{"command":"ls"}"#), None);
    }

    #[test]
    fn stream_text_prose_then_fence_shows_prose_prefix_only() {
        let buf = "Here is the plan.\n```json\n{\"choices\":[]}\n```";
        assert_eq!(
            user_visible_stream_text(buf).as_deref(),
            Some("Here is the plan.")
        );
    }

    #[test]
    fn stream_text_empty_is_none() {
        assert_eq!(user_visible_stream_text("   \n  "), None);
    }

    fn streaming_tab(buf: &str, reveal_chars: usize) -> crate::app::TabSession {
        let mut tab = crate::app::TabSession::default();
        tab.turn = crate::app::TurnState::Streaming {
            prompt: crate::app::SubmittedPrompt {
                id: 1,
                text: "hi".into(),
                submitted_at_unix_s: 0.0,
                autofix: None,
            },
            buf: buf.to_string(),
        };
        tab.reveal_chars = reveal_chars;
        tab
    }

    #[test]
    fn thinking_activity_is_a_one_way_per_prompt_latch() {
        let mut tab = streaming_tab("", 0);
        tab.waiting_for_first_visible_activity = true;
        assert!(should_show_turn_activity(&tab));

        tab.mark_visible_agent_activity();
        assert!(!should_show_turn_activity(&tab));

        tab.tool_calls
            .insert("tool".into(), ("Run tests".into(), "Completed".into()));
        assert!(
            !should_show_turn_activity(&tab),
            "later activity changes must not re-enable Thinking"
        );
    }

    #[test]
    fn breathing_dot_shrinks_then_grows() {
        assert_eq!(breathing_dot(0), "●");
        assert_eq!(breathing_dot(5), "•");
        assert_eq!(breathing_dot(9), "·");
        assert_eq!(breathing_dot(14), "•");
        assert_eq!(
            breathing_dot(crate::ui::ACTIVITY_CYCLE_FRAMES),
            "●"
        );
    }

    #[test]
    fn permission_animates_only_its_matching_tool_call() {
        let matching = ChatMessage::ToolCall {
            id: "tool-2".into(),
            title: "Read Cargo.toml".into(),
            status: "Completed".into(),
        };
        let other = ChatMessage::ToolCall {
            id: "tool-1".into(),
            title: "Find files".into(),
            status: "Completed".into(),
        };

        let matching_lines =
            build_message_lines(&matching, false, false, Some("tool-2"), 9, 80);
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
        let over: String =
            std::iter::repeat('é').take(MAX_RENDER_LINE_CHARS + 10).collect();
        let _ = truncate_render_text(&over).into_owned(); // must not panic
    }

    // ── push_dot_prefixed_lines ─────────────────────────────────────────────

    #[test]
    fn dot_prefix_skips_leading_blank_lines() {
        // Models often prefix prose with \n / \n\n; the dot must land on the
        // first content row, not burn on an empty line.
        let mut lines = Vec::new();
        push_dot_prefixed_lines(&mut lines, "\n\nHello", 40, theme::DOT_AGENT, theme::AGENT_TEXT);
        assert_eq!(lines.len(), 1, "leading blanks must be dropped");
        assert_eq!(line_text(&lines[0]), "● Hello");
    }

    #[test]
    fn dot_prefix_preserves_paragraph_break_and_indents_continuation() {
        let mut lines = Vec::new();
        push_dot_prefixed_lines(&mut lines, "A\n\nB", 40, theme::DOT_AGENT, theme::AGENT_TEXT);
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert_eq!(texts, vec!["● A".to_string(), String::new(), "  B".to_string()]);
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
        assert!(line_text(&lines[0]).starts_with("● "), "first row gets the dot");
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
        assert_eq!(texts, vec!["> line one".to_string(), "  line two".to_string()]);
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
        assert_eq!(texts, vec!["> A".to_string(), String::new(), "  B".to_string()]);
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
