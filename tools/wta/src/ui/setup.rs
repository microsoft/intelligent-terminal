use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::app::{App, SetupFailureKind, SetupOption, SetupPhase};

const PROGRESS_SPINNER: &[char] = &['\u{25DC}', '\u{25DD}', '\u{25DE}', '\u{25DF}'];

// Muted secondary text. Dimmed default fg (not a fixed gray) so it tracks the
// color scheme and stays readable on light schemes (#234). Figma reference was
// rgba(255,255,255,0.6) ≈ #999999, which only worked on a dark background.
const DIM_TEXT: Style = Style::new().fg(Color::Reset).add_modifier(Modifier::DIM);
// Named ANSI (not fixed RGB) so the selection accent follows the color scheme
// and stays readable on light schemes (#234).
const SELECTED_COLOR: Color = Color::Cyan;

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let setup = match &app.setup {
        Some(s) => s,
        None => return,
    };

    // Horizontal padding (matching chat area)
    let padded = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);
    let area = padded[1];

    let spinner_char = PROGRESS_SPINNER[app.activity_frame as usize % PROGRESS_SPINNER.len()];
    let mut lines: Vec<Line> = Vec::new();

    match &setup.phase {
        SetupPhase::Installing => {
            lines.push(Line::from(vec![
                Span::styled(
                    t!("setup.status.installing_copilot_cli").into_owned(),
                    Style::new().fg(Color::Reset),
                ),
                Span::raw("  "),
                Span::styled(spinner_char.to_string(), Style::new().fg(SELECTED_COLOR)),
            ]));
        }
        SetupPhase::Reconnecting => {
            lines.push(Line::from(vec![
                Span::styled(
                    t!(
                        "setup.status.connecting_agent",
                        agent = &setup.preflight.display_name
                    )
                    .into_owned(),
                    Style::new().fg(Color::Reset),
                ),
                Span::raw("  "),
                Span::styled(spinner_char.to_string(), Style::new().fg(SELECTED_COLOR)),
            ]));
        }
        SetupPhase::Ready | SetupPhase::Failed { .. } => {
            // Title — bold, scheme default foreground, with bullet
            lines.push(Line::from(vec![
                Span::styled(
                    "\u{25CF} ",
                    Style::new().fg(Color::Reset).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    setup.title.clone(),
                    Style::new().fg(Color::Reset).add_modifier(Modifier::BOLD),
                ),
            ]));
            lines.push(Line::from(Span::styled(
                format!("  {}", &setup.subtitle),
                DIM_TEXT,
            )));
            lines.push(Line::from(""));
        }
    }

    if setup.is_busy() {
        let paragraph = Paragraph::new(lines)
            .alignment(crate::rtl::text_alignment())
            .wrap(ratatui::widgets::Wrap { trim: false });
        frame.render_widget(paragraph, area);
        return;
    }

    for (i, opt) in setup.options.iter().enumerate() {
        let is_selected = i == setup.selected_index;

        let (label, status_text) = match opt {
            SetupOption::ChooseAgentSource => {
                (t!("agent_picker.title").into_owned(), String::new())
            }
            SetupOption::Install { display_name, .. } => (
                t!("setup.option.install", agent = display_name.as_str()).into_owned(),
                format!("  {}", t!("setup.option.install_hint")),
            ),
            SetupOption::SignIn { display_name, .. } => (
                t!("setup.option.signin", agent = display_name.as_str()).into_owned(),
                String::new(),
            ),
            SetupOption::Recheck => (
                t!("setup.option.retry_detection").into_owned(),
                String::new(),
            ),
            SetupOption::Retry => {
                let label = match setup.reason {
                    crate::app::SetupReason::AgentMissing => {
                        t!("setup.option.retry_detection").into_owned()
                    }
                    crate::app::SetupReason::AgentError => {
                        t!("setup.option.retry_auth").into_owned()
                    }
                };
                (label, String::new())
            }
            SetupOption::RetryConnection => (
                t!("setup.option.retry_connection").into_owned(),
                String::new(),
            ),
        };

        let status_style = if is_selected {
            Style::new().fg(SELECTED_COLOR)
        } else {
            Style::new().fg(Color::Reset)
        };

        if is_selected {
            lines.push(Line::from(vec![
                Span::styled(
                    "  > ",
                    Style::new().fg(SELECTED_COLOR).add_modifier(Modifier::BOLD),
                ),
                Span::styled(label, Style::new().fg(SELECTED_COLOR)),
                Span::styled(status_text, status_style),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(label, Style::new().fg(Color::Reset)),
                Span::styled(status_text, status_style),
            ]));
        }
    }

    if let SetupPhase::Failed { kind, message } = &setup.phase {
        lines.push(Line::from(""));
        let prefix = match kind {
            SetupFailureKind::Install => t!("setup.status.install_failed").into_owned(),
            SetupFailureKind::Detection | SetupFailureKind::Connection => String::new(),
        };
        lines.push(Line::from(vec![
            Span::styled("  ", DIM_TEXT),
            Span::styled(prefix, Style::new().fg(Color::Red)),
            Span::styled(message.clone(), Style::new().fg(Color::Red)),
        ]));
    }

    let paragraph = Paragraph::new(lines)
        .alignment(crate::rtl::text_alignment())
        .wrap(ratatui::widgets::Wrap { trim: false });
    frame.render_widget(paragraph, area);
}
