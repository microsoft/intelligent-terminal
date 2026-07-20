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
    // Resolve the pending text once so the reveal and height calculations use
    // the same streaming snapshot.
    let pending_text = pending_render_text(tab);

    // Reserve the row only when the shimmer will actually render; mirrors
    // the suppression rule in `build_activity_line` below.
    let reveal_catching_up = pending_text
        .as_deref()
        .is_some_and(|text| tab.reveal_chars < text.chars().count());
    let activity = if tab.turn.spinner_label().is_some() && !reveal_catching_up {
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

    for (idx, msg) in app.current_tab().messages.iter().enumerate().rev() {
        let is_last_message = idx + 1 == app.current_tab().messages.len();
        let mut message_lines = build_message_lines(msg, is_last_message, app.current_tab().turn.is_streaming(), wrap_width);
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

    let mut lines = vec![Line::from(vec![
        Span::styled(chevron, chevron_style),
        Span::styled("> ", prompt_style),
        Span::styled(truncate_render_text(&turn.prompt), prompt_style),
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
            lines.extend(build_message_lines(msg, false, false, wrap_width));
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
    if tab.turn.spinner_label().is_none() {
        return None;
    }
    // While the reveal is still catching up, the growing text is itself the
    // activity signal, so skip the shimmer to avoid a duplicate. Once it
    // catches up (e.g. the model narrated a step, then went quiet for a
    // tool call / permission round-trip), the text goes static with no
    // cursor of its own, so fall back to the shimmer so busy always shows
    // *something* (issue #189 covered the empty-buffer case; this covers
    // the non-empty-but-stalled one).
    if is_reveal_catching_up(tab) {
        return None;
    }
    let label = activity_label();
    Some(Line::from(shimmer::shimmer_spans(
        &label,
        tab.activity_frame,
    )))
}

/// True while the reveal cursor hasn't caught up to the pending stream text,
/// i.e. the growing text is still its own activity signal. `false` when
/// there's no pending text, or the reveal has fully caught up.
fn is_reveal_catching_up(tab: &crate::app::TabSession) -> bool {
    match pending_render_text(tab) {
        Some(text) => tab.reveal_chars < text.chars().count(),
        None => false,
    }
}

/// Assistant text is always user-visible Markdown. Typed MCP requests, not
/// assistant-text parsing, are the only path that can create action cards.
pub(crate) fn user_visible_stream_text(text: &str) -> Option<Cow<'_, str>> {
    if text.trim().is_empty() {
        return None;
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
    wrap_width: usize,
) -> Vec<Line<'a>> {
    let mut lines = Vec::new();
    match msg {
        ChatMessage::User(text) => {
            lines.push(Line::from(vec![
                Span::styled("> ", theme::USER_PROMPT),
                Span::styled(truncate_render_text(text), theme::USER_PROMPT),
            ]));
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
        ChatMessage::ToolCall { title, status, .. } => {
            lines.push(Line::from(Span::styled(
                format!(
                    "[{}] {}",
                    truncate_render_text(title),
                    truncate_render_text(status)
                ),
                theme::TOOL_CALL,
            )));
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

    // ── user_visible_stream_text ────────────────────────────────────────────

    #[test]
    fn stream_text_pure_prose_passes_through() {
        assert_eq!(
            user_visible_stream_text("just talking").as_deref(),
            Some("just talking")
        );
    }

    #[test]
    fn stream_text_json_is_visible_markdown() {
        assert_eq!(
            user_visible_stream_text(r#"{"explanation":"why blue"}"#).as_deref(),
            Some(r#"{"explanation":"why blue"}"#)
        );
    }

    #[test]
    fn stream_text_fenced_json_is_visible_markdown() {
        let buf = "Here is the plan.\n```json\n{\"choices\":[]}\n```";
        assert_eq!(user_visible_stream_text(buf).as_deref(), Some(buf));
    }

    #[test]
    fn stream_text_empty_is_none() {
        assert_eq!(user_visible_stream_text("   \n  "), None);
    }

    // ── is_reveal_catching_up ────────────────────────────────────────────────

    fn streaming_tab(buf: &str, reveal_chars: usize) -> crate::app::TabSession {
        crate::app::TabSession {
            turn: crate::app::TurnState::Streaming {
                prompt: crate::app::SubmittedPrompt {
                    id: 1,
                    text: "hi".into(),
                    submitted_at_unix_s: 0.0,
                    autofix: None,
                },
                buf: buf.to_string(),
            },
            reveal_chars,
            ..Default::default()
        }
    }

    #[test]
    fn reveal_catching_up_true_while_behind_visible_length() {
        // "hello" is 5 chars; reveal_chars=2 means the typewriter is still
        // mid-reveal, so the growing text is still its own activity signal.
        let tab = streaming_tab("hello", 2);
        assert!(is_reveal_catching_up(&tab));
    }

    #[test]
    fn reveal_catching_up_false_once_reveal_equals_visible_length() {
        // reveal_chars caught up exactly to the visible length: the boundary
        // case (`<` vs `>=`) that must flip to false, not stay true.
        let tab = streaming_tab("hello", 5);
        assert!(!is_reveal_catching_up(&tab));
    }

    #[test]
    fn reveal_catching_up_false_once_reveal_exceeds_visible_length() {
        let tab = streaming_tab("hello", 99);
        assert!(!is_reveal_catching_up(&tab));
    }

    #[test]
    fn reveal_catching_up_false_when_buffer_empty() {
        // Empty buffer (no narration streamed yet) has no pending text to
        // catch up to — the empty-buffer case fixed for issue #189 must not
        // be treated as "still revealing".
        let tab = streaming_tab("", 0);
        assert!(!is_reveal_catching_up(&tab));
    }

    #[test]
    fn reveal_catching_up_false_when_not_streaming() {
        // Idle (no turn in flight) has no `buffer()` at all: `pending_render_text`
        // returns None, so there's nothing to catch up to.
        let tab = crate::app::TabSession::default();
        assert!(!is_reveal_catching_up(&tab));
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
}
