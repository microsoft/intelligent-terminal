use ratatui::prelude::*;
use ratatui::widgets::{Paragraph, Wrap};

use crate::app::App;

// Dimmed default fg (not a fixed gray) so muted hints track the color scheme
// and stay readable on light schemes (#234).
const DIM_TEXT: Style = Style::new().fg(Color::Reset).add_modifier(Modifier::DIM);
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
            Span::styled("● ", Style::new().fg(Color::Reset).add_modifier(Modifier::BOLD)),
            Span::styled(
                t!("auth.agent_selected", name = &auth.agent_name).into_owned(),
                Style::new().fg(Color::Reset),
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
                Style::new().fg(Color::Reset),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                t!("auth.code_copied_hint").into_owned(),
                DIM_TEXT,
            )));
        }
    } else {
        // Concise header: a single line (which agent + why). Any failure
        // status is shown at the *bottom* of the screen, not here.
        lines.push(Line::from(Span::styled(
            t!("auth.card_connect", name = &auth.agent_name).into_owned(),
            Style::new().fg(Color::Reset),
        )));

        lines.push(Line::from(""));

        // Footer. For Copilot it carries the GitHub Enterprise affordance —
        // collapsed it's just an "E" key hint; expanded it shows the domain
        // input plus a github.com fallback hint. For other agents the sign-in
        // is a copy-the-command flow: before sign-in show the copy/paste
        // instruction; after Enter (command copied) replace it with the
        // "command copied — retry" status, then an Esc hint.
        if auth.agent_id == "copilot" {
            if auth.enterprise_mode {
                lines.push(Line::from(vec![
                    Span::styled(
                        t!("auth.enterprise_domain_label").into_owned(),
                        Style::new().fg(Color::Reset),
                    ),
                    Span::styled(
                        format!("{}\u{2588}", auth.enterprise_host),
                        Style::new().fg(Color::Reset).add_modifier(Modifier::BOLD),
                    ),
                ]));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    t!("auth.enterprise_hint_footer").into_owned(),
                    DIM_TEXT,
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    t!("auth.enterprise_prompt").into_owned(),
                    DIM_TEXT,
                )));
            }
            // Failure feedback at the bottom: a prior attempt set status_message
            // (e.g. an unreachable enterprise host). Show the reason in yellow
            // followed by a dim, situation-specific guidance line.
            if !auth.status_message.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("  {}", auth.status_message),
                    Style::new().fg(Color::Yellow),
                )));
                let help = if auth.enterprise_mode {
                    t!("auth.login_failed_help_enterprise").into_owned()
                } else {
                    t!("auth.login_failed_help_default").into_owned()
                };
                lines.push(Line::from(Span::styled(help, DIM_TEXT)));
            }
        } else {
            if auth.status_message.is_empty() {
                lines.push(Line::from(Span::styled(
                    t!("auth.hint_footer").into_owned(),
                    DIM_TEXT,
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    format!("  {}", auth.status_message),
                    Style::new().fg(Color::Yellow),
                )));
            }
            lines.push(Line::from(Span::styled(
                t!("auth.hint_footer_back").into_owned(),
                DIM_TEXT,
            )));
        }
    }

    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .alignment(crate::rtl::text_alignment());
    frame.render_widget(paragraph, area);
}
