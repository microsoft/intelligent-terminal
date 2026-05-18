use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::App;

const DIM_TEXT: Style = Style::new().fg(Color::Rgb(153, 153, 153));
const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let auth = match &app.auth {
        Some(a) => a,
        None => return,
    };

    let padded = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
        .split(area);
    let area = padded[1];

    let mut lines: Vec<Line> = Vec::new();

    if auth.checking {
        let spinner_char = SPINNER[app.activity_frame as usize % SPINNER.len()];

        lines.push(Line::from(vec![
            Span::styled("● ", Style::new().fg(Color::White).add_modifier(Modifier::BOLD)),
            Span::styled(
                format!("Agent CLI {} is selected", auth.agent_name),
                Style::new().fg(Color::White),
            ),
        ]));
        lines.push(Line::from(""));

        if auth.status_message.is_empty() {
            // Still checking auth
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {} Checking authentication...", spinner_char),
                    Style::new().fg(Color::Yellow),
                ),
            ]));
        } else {
            // Got device code — show it prominently
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {} Waiting for authorization...", spinner_char),
                    Style::new().fg(Color::Yellow),
                ),
            ]));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("  {}", auth.status_message),
                Style::new().fg(Color::White),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  Code copied to clipboard — paste it in your browser.",
                DIM_TEXT,
            )));
        }
    } else {
        // "● Agent CLI <name> is selected" + optional reason on same line
        if auth.status_message.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("● ", Style::new().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(
                    format!("Agent CLI {} is selected", auth.agent_name),
                    Style::new().fg(Color::White),
                ),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::styled("● ", Style::new().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(
                    format!("Agent CLI {} — ", auth.agent_name),
                    Style::new().fg(Color::White),
                ),
                Span::styled(
                    &auth.status_message,
                    Style::new().fg(Color::Yellow),
                ),
            ]));
        }

        // Blank line
        lines.push(Line::from(""));

        // "● Sign in to use your agent:"
        lines.push(Line::from(vec![
            Span::styled("● ", Style::new().fg(Color::White).add_modifier(Modifier::BOLD)),
            Span::styled(
                "Sign in to use your agent:",
                Style::new().fg(Color::White),
            ),
        ]));

        // Blank line
        lines.push(Line::from(""));

        // Card: "Connect <agent> to enable agent features"
        lines.push(Line::from(Span::styled(
            format!("  Connect {} to enable agent features", auth.agent_name),
            Style::new().fg(Color::White),
        )));

        // Blank line
        lines.push(Line::from(""));

        // Sign-in button — agent-specific text
        let button_text = if auth.agent_name.contains("Copilot") {
            "[ Sign in with GitHub ]".to_string()
        } else {
            format!("[ Sign in with {} ]", auth.agent_name)
        };
        lines.push(Line::from(vec![
            Span::raw("                          "),
            Span::styled(
                button_text,
                Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
        ]));

        // Hint
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  Press Enter to sign in, Esc to go back",
            DIM_TEXT,
        )));
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, area);
}
