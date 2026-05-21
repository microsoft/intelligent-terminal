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

    let activity = if tab.turn.spinner_label().is_some()
        && pending_stream_height(tab, wrap_width) == 0
    {
        1usize
    } else {
        0
    };

    let messages: usize = tab.messages.iter().map(|m| message_height(m, wrap_width)).sum();
    let turns: usize = tab.completed_turns.iter().map(|t| turn_height(t, wrap_width)).sum();
    let pending = pending_stream_height(tab, wrap_width);

    (activity + messages + turns + pending).max(1).min(u16::MAX as usize) as u16
}

fn pending_stream_height(tab: &crate::app::TabSession, wrap_width: usize) -> usize {
    let Some(text) = pending_render_text(tab) else {
        return 0;
    };
    let body_width = wrap_width.saturating_sub(2).max(1);
    wrap_count(&text, body_width)
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

fn message_height(msg: &ChatMessage, wrap_width: usize) -> usize {
    // Most variants render with a 2-cell prefix ("● " for agent/error,
    // "> " for user) and a trailing blank line.
    let body_width = wrap_width.saturating_sub(2).max(1);
    match msg {
        ChatMessage::User(t) | ChatMessage::Agent(t) | ChatMessage::Error(t) => {
            wrap_count(t, body_width) + 1
        }
        ChatMessage::System(t) | ChatMessage::AgentEvent(t) => wrap_count(t, wrap_width) + 1,
        ChatMessage::ToolCall { .. } => 1,
        ChatMessage::Plan(entries) => 2 + entries.len(), // header + each entry + blank
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
        for (idx, turn) in app.current_tab().completed_turns.iter().enumerate().rev() {
            let is_selected = selected_idx == Some(idx);
            let mut turn_lines = build_completed_turn_lines(turn, is_selected, wrap_width);
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
                Span::styled("● ", Style::new().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(
                    t!("chat.welcome_title").into_owned(),
                    Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
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
    wrap_width: usize,
) -> Vec<Line<'a>> {
    let chevron = if turn.expanded { "▼ " } else { "▶ " };
    // Selected row uses the SELECTED theme (reverse video) to make the
    // current Tab target visible. Unselected rows render in the standard
    // dim USER_PROMPT style — same as before this feature existed.
    let prompt_style = if is_selected {
        theme::SELECTED
    } else {
        theme::USER_PROMPT
    };
    let chevron_style = if is_selected {
        theme::SELECTED
    } else {
        theme::DIM
    };

    let mut lines = vec![Line::from(vec![
        Span::styled(chevron, chevron_style),
        Span::styled("> ", prompt_style),
        Span::styled(truncate_render_text(&turn.prompt), prompt_style),
    ])];

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

    lines.push(Line::default());
    lines
}

fn build_activity_line(app: &App) -> Option<Line<'static>> {
    let tab = app.current_tab();
    if tab.turn.spinner_label().is_none() || pending_render_text(tab).is_some() {
        return None;
    }
    let label = activity_label();
    Some(Line::from(shimmer::shimmer_spans(
        &label,
        tab.activity_frame,
    )))
}

/// Incrementally extracts a JSON string field's decoded value from a
/// possibly-truncated text. Handles `\"`, `\\`, `\n`, `\t`, `\u{XXXX}` etc.
/// Returns the partial value if the closing quote hasn't arrived yet.
fn extract_json_string_field(text: &str, field: &str) -> Option<String> {
    let key = format!("\"{field}\"");
    let start = text.find(&key)?;
    let rest = text[start + key.len()..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
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
                    if let Some(ch) = u32::from_str_radix(&hex, 16)
                        .ok()
                        .and_then(char::from_u32)
                    {
                        out.push(ch);
                    }
                }
                Some(other) => out.push(other),
            },
            c => out.push(c),
        }
    }
    Some(out)
}

/// Resolves what (if anything) the pending stream should render.
///
/// - Buffer starts with a JSON wrapper (autofix): extract the `explanation`
///   field so the user sees flowing markdown rather than raw JSON syntax.
///   fix actions lack this field and yield None — the card surfaces on
///   finalize.
/// - Buffer is mixed prose followed by a fenced JSON block (planner
///   terminal-task mode): render only the prose prefix; the recommendation
///   card replaces it on eager/end-of-turn finalize.
/// - Pure prose: stream as-is.
fn pending_render_text(tab: &crate::app::TabSession) -> Option<Cow<'_, str>> {
    // Pending text is only meaningful while the turn is actively streaming.
    let text = tab.turn.buffer()?;
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

fn build_pending_stream_lines<'a>(app: &App, wrap_width: usize) -> Vec<Line<'a>> {
    let Some(text) = pending_render_text(app.current_tab()) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    push_dot_prefixed_lines(
        &mut lines,
        &text,
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
            // Preserve blank lines between paragraphs.
            if first_row {
                lines.push(Line::from(vec![
                    Span::styled("● ", dot_style),
                    Span::styled(String::new(), text_style),
                ]));
                first_row = false;
            } else {
                lines.push(Line::default());
            }
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
