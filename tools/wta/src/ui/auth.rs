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
                t!("auth.agent_selected", agent = &auth.agent_name).into_owned(),
                Style::new().fg(Color::White),
            ),
        ]));
        lines.push(Line::from(""));

        if auth.status_message.is_empty() {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {} {}", spinner_char, t!("auth.checking")),
                    Style::new().fg(Color::Yellow),
                ),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {} {}", spinner_char, t!("auth.waiting")),
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
                format!("  {}", t!("auth.code_copied")),
                DIM_TEXT,
            )));
        }
    } else {
        if auth.status_message.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("● ", Style::new().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(
                    t!("auth.agent_selected", agent = &auth.agent_name).into_owned(),
                    Style::new().fg(Color::White),
                ),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::styled("● ", Style::new().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(
                    t!("auth.agent_selected_reason", agent = &auth.agent_name).into_owned(),
                    Style::new().fg(Color::White),
                ),
                Span::styled(
                    &auth.status_message,
                    Style::new().fg(Color::Yellow),
                ),
            ]));
        }

        lines.push(Line::from(""));

        lines.push(Line::from(vec![
            Span::styled("● ", Style::new().fg(Color::White).add_modifier(Modifier::BOLD)),
            Span::styled(
                t!("auth.signin_prompt").into_owned(),
                Style::new().fg(Color::White),
            ),
        ]));

        lines.push(Line::from(""));

        lines.push(Line::from(Span::styled(
            format!("  {}", t!("auth.connect_agent", agent = &auth.agent_name)),
            Style::new().fg(Color::White),
        )));

        lines.push(Line::from(""));

        let button_text = if auth.agent_name.contains("Copilot") {
            t!("auth.signin_github").into_owned()
        } else {
            t!("auth.signin_agent", agent = &auth.agent_name).into_owned()
        };
        lines.push(Line::from(vec![
            Span::raw("                          "),
            Span::styled(
                button_text,
                Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
        ]));

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  {}", t!("auth.hint")),
            DIM_TEXT,
        )));
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, area);
}
