use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::{App, ChatMessage, PlanEntryStatus};
use crate::theme;

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let inner = Block::default().borders(Borders::NONE);
    let inner_area = inner.inner(area);

    // Build all lines from messages
    let mut lines: Vec<Line> = Vec::new();

    for msg in &app.messages {
        match msg {
            ChatMessage::User(text) => {
                lines.push(Line::from(vec![
                    Span::styled("> ", theme::USER_PROMPT),
                    Span::styled(text.as_str(), theme::USER_PROMPT),
                ]));
                lines.push(Line::default()); // blank line
            }
            ChatMessage::Agent(text) => {
                for line_text in text.lines() {
                    lines.push(Line::from(Span::styled(line_text, theme::AGENT_TEXT)));
                }
                if !app.agent_streaming
                    || !matches!(app.messages.last(), Some(ChatMessage::Agent(_)))
                {
                    lines.push(Line::default());
                }
            }
            ChatMessage::System(text) => {
                for line_text in text.lines() {
                    lines.push(Line::from(Span::styled(line_text, theme::SYSTEM_TEXT)));
                }
                lines.push(Line::default());
            }
            ChatMessage::ToolCall { title, status, .. } => {
                lines.push(Line::from(Span::styled(
                    format!("[{}] {}", title, status),
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
                        format!("  {} {}", marker, entry.content),
                        theme::PLAN_STYLE,
                    )));
                }
                lines.push(Line::default());
            }
            ChatMessage::Error(text) => {
                lines.push(Line::from(Span::styled(
                    format!("Error: {}", text),
                    theme::ERROR_STYLE,
                )));
                lines.push(Line::default());
            }
        }
    }

    // Streaming indicator
    if app.agent_streaming {
        lines.push(Line::from(Span::styled("...", theme::DIM)));
    }

    // If no messages, show welcome
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "Type a message and press Enter to begin.",
            theme::DIM,
        )));
    }

    // Calculate scroll: we want to show the bottom of the content
    let visible_height = inner_area.height as usize;
    let total_lines = lines.len();
    let max_scroll = total_lines.saturating_sub(visible_height);
    let scroll = if app.scroll_offset == 0 {
        max_scroll // auto-scroll to bottom
    } else {
        max_scroll.saturating_sub(app.scroll_offset)
    };

    let paragraph = Paragraph::new(lines)
        .block(inner)
        .wrap(Wrap { trim: false })
        .scroll((scroll as u16, 0));

    frame.render_widget(paragraph, area);
}
