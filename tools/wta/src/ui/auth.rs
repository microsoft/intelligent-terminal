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
                t!("auth.agent_selected", name = &auth.agent_name).into_owned(),
                Style::new().fg(Color::White),
            ),
        ]));
        lines.push(Line::from(""));

        if auth.status_message.is_empty() {
            lines.push(Line::from(vec![
                Span::styled(
                    t!("auth.checking_authentication", spinner = spinner_char.to_string()).into_owned(),
                    Style::new().fg(Color::Yellow),
                ),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::styled(
                    t!("auth.waiting_for_authorization", spinner = spinner_char.to_string()).into_owned(),
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
                t!("auth.code_copied_hint").into_owned(),
                DIM_TEXT,
            )));
        }
    } else {
        if auth.status_message.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("● ", Style::new().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(
                    t!("auth.agent_selected", name = &auth.agent_name).into_owned(),
                    Style::new().fg(Color::White),
                ),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::styled("● ", Style::new().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(
                    t!("auth.agent_selected_with_status", name = &auth.agent_name).into_owned(),
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
                t!("auth.sign_in_prompt").into_owned(),
                Style::new().fg(Color::White),
            ),
        ]));

        lines.push(Line::from(""));

        lines.push(Line::from(Span::styled(
            t!("auth.card_connect", name = &auth.agent_name).into_owned(),
            Style::new().fg(Color::White),
        )));

        lines.push(Line::from(""));

        let button_text = if auth.agent_name.contains("Copilot") {
            t!("auth.button_sign_in_github").into_owned()
        } else {
            t!("auth.button_sign_in_with", name = &auth.agent_name).into_owned()
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
            t!("auth.hint_footer").into_owned(),
            DIM_TEXT,
        )));
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, area);
}
