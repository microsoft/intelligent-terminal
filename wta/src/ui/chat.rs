use std::borrow::Cow;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::{App, ChatMessage, PlanEntryStatus};
use crate::theme;
use crate::ui_trace;

const ACTIVITY_LABEL: &str = "thinking";
const ACTIVITY_ICON_FRAMES: [&str; 4] = [".", "o", "O", "o"];
const ACTIVITY_HIGHLIGHT_WINDOWS: [(usize, usize); 10] = [
    (0, 1),
    (0, 2),
    (0, 3),
    (1, 4),
    (2, 5),
    (3, 6),
    (4, 7),
    (5, 8),
    (6, 8),
    (7, 8),
];
const ACTIVITY_PREVIEW_MAX_CHARS: usize = 180;
const MAX_RENDER_LINE_CHARS: usize = 4096;

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let render_started = std::time::Instant::now();
    let inner = Block::default().borders(Borders::NONE);
    let inner_area = inner.inner(area);
    let visible_height = inner_area.height as usize;
    let requested_lines = visible_height
        .saturating_add(app.current_tab().scroll_offset)
        .saturating_add(32);

    let mut reversed_lines: Vec<Line> = Vec::new();

    if let Some(activity_line) = build_activity_line(app) {
        reversed_lines.push(activity_line);
    }

    let mut pending_lines = build_pending_stream_lines(app);
    reversed_lines.extend(pending_lines.drain(..).rev());

    for (idx, msg) in app.current_tab().messages.iter().enumerate().rev() {
        let is_last_message = idx + 1 == app.current_tab().messages.len();
        let mut message_lines = build_message_lines(msg, is_last_message, app.current_tab().agent_streaming);
        reversed_lines.extend(message_lines.drain(..).rev());
        if reversed_lines.len() >= requested_lines {
            break;
        }
    }

    let selected_idx = app.current_tab().selected_completed_turn_idx;
    for (idx, turn) in app.current_tab().completed_turns.iter().enumerate().rev() {
        let is_selected = selected_idx == Some(idx);
        let mut turn_lines = build_completed_turn_lines(turn, is_selected);
        reversed_lines.extend(turn_lines.drain(..).rev());
        if reversed_lines.len() >= requested_lines {
            break;
        }
    }

    let lines: Vec<Line> = reversed_lines.into_iter().rev().collect();

    let total_lines = lines.len();
    let scroll = total_lines.saturating_sub(visible_height.saturating_add(app.current_tab().scroll_offset));

    let paragraph = Paragraph::new(lines)
        .block(inner)
        .wrap(Wrap { trim: false })
        .scroll((scroll as u16, 0));

    frame.render_widget(paragraph, area);

    ui_trace::log_slow("chat_render", render_started.elapsed(), || {
        format!(
            "messages={} pending_chars={} requested_lines={} visible_height={} area={}x{}",
            app.current_tab().messages.len(),
            app.current_tab().pending_agent_response.chars().count(),
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
            lines.extend(build_message_lines(msg, false, false));
        }
    }

    lines.push(Line::default());
    lines
}

fn build_activity_line(app: &App) -> Option<Line<'static>> {
    if !(app.current_tab().prompt_in_flight || app.current_tab().agent_streaming || app.current_tab().progress_status.is_some()) {
        return None;
    }

    let mut spans = Vec::new();
    let icon = ACTIVITY_ICON_FRAMES[app.current_tab().activity_frame % ACTIVITY_ICON_FRAMES.len()];
    spans.push(Span::styled(icon.to_string(), theme::IN_PROGRESS));
    spans.push(Span::raw(" "));
    spans.extend(animated_activity_label(app.current_tab().activity_frame));

    if let Some((preview, style)) = activity_preview(app) {
        spans.push(Span::styled(" ", theme::DIM));
        spans.push(Span::styled(preview, style));
    }

    Some(Line::from(spans))
}

fn animated_activity_label(frame: usize) -> Vec<Span<'static>> {
    let (start, end) = ACTIVITY_HIGHLIGHT_WINDOWS[frame % ACTIVITY_HIGHLIGHT_WINDOWS.len()];
    let prefix = &ACTIVITY_LABEL[..start];
    let highlighted = &ACTIVITY_LABEL[start..end];
    let suffix = &ACTIVITY_LABEL[end..];

    let mut spans = Vec::new();
    if !prefix.is_empty() {
        spans.push(Span::styled(prefix.to_string(), theme::DIM));
    }
    spans.push(Span::styled(highlighted.to_string(), theme::IN_PROGRESS));
    if !suffix.is_empty() {
        spans.push(Span::styled(suffix.to_string(), theme::DIM));
    }
    spans
}

fn activity_preview(app: &App) -> Option<(String, Style)> {
    app.current_tab().progress_status
        .as_deref()
        // "Thinking..." is redundant now that the animated label says "thinking".
        .filter(|s| *s != "Thinking...")
        .map(single_line_tail_preview)
        .filter(|text| !text.is_empty())
        .map(|text| (text, theme::DIM))
}

fn build_pending_stream_lines(_app: &App) -> Vec<Line<'_>> {
    // Don't render the raw agent response (typically JSON) while streaming.
    // The activity indicator is sufficient feedback; the final parsed
    // recommendations will appear when the response is finalized.
    Vec::new()
}

fn build_pending_text_lines<'a>(label: &str, text: &'a str, style: Style) -> Vec<Line<'a>> {
    let mut lines = Vec::new();
    lines.push(Line::from(Span::styled(format!("{label}:"), theme::DIM)));

    for line_text in text.lines() {
        lines.push(Line::from(Span::styled(
            truncate_render_text(line_text),
            style,
        )));
    }

    if text.lines().next().is_none() {
        lines.push(Line::from(Span::styled("", style)));
    }

    lines.push(Line::default());
    lines
}

fn single_line_tail_preview(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = collapsed.chars().collect();

    if chars.len() <= ACTIVITY_PREVIEW_MAX_CHARS {
        return collapsed;
    }

    let tail: String = chars[chars.len().saturating_sub(ACTIVITY_PREVIEW_MAX_CHARS)..]
        .iter()
        .collect();
    format!("...{tail}")
}

fn build_message_lines<'a>(
    msg: &'a ChatMessage,
    is_last_message: bool,
    agent_streaming: bool,
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
            for (i, line_text) in text.lines().enumerate() {
                if i == 0 {
                    // First line gets green dot indicator
                    lines.push(Line::from(vec![
                        Span::styled("● ", theme::DOT_AGENT),
                        Span::styled(truncate_render_text(line_text), theme::AGENT_TEXT),
                    ]));
                } else {
                    // Subsequent lines indented to align with text after dot
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(truncate_render_text(line_text), theme::AGENT_TEXT),
                    ]));
                }
            }
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
            lines.push(Line::from(Span::styled("Plan:", theme::PLAN_STYLE)));
            for entry in entries {
                let marker = match entry.status {
                    PlanEntryStatus::Completed => "[x]",
                    PlanEntryStatus::InProgress => "[>]",
                    PlanEntryStatus::Pending => "[ ]",
                };
                lines.push(Line::from(Span::styled(
                    format!("  {} {}", marker, truncate_render_text(&entry.content)),
                    theme::PLAN_STYLE,
                )));
            }
            lines.push(Line::default());
        }
        ChatMessage::Error(text) => {
            for (i, line_text) in text.lines().enumerate() {
                if i == 0 {
                    lines.push(Line::from(vec![
                        Span::styled("● ", theme::DOT_ERROR),
                        Span::styled(
                            truncate_render_text(line_text),
                            theme::ERROR_STYLE,
                        ),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(
                            truncate_render_text(line_text),
                            theme::ERROR_STYLE,
                        ),
                    ]));
                }
            }
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
