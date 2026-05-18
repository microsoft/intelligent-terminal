use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::app::{App, SetupOption};

const SPINNER: &[char] = &[
    '\u{280B}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283C}', '\u{2834}', '\u{2826}',
    '\u{2827}', '\u{2807}', '\u{280F}',
];

// Figma: rgba(255,255,255,0.6) ≈ #999999
const DIM_TEXT: Style = Style::new().fg(Color::Rgb(153, 153, 153));
const SELECTED_COLOR: Color = Color::Rgb(96, 205, 255);

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

    let mut lines: Vec<Line> = Vec::new();

    // Title — bold white with bullet
    lines.push(Line::from(vec![
        Span::styled(
            "\u{25CF} ",
            Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            &setup.title,
            Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
    ]));

    // Subtitle — dim
    lines.push(Line::from(Span::styled(
        format!("  {}", &setup.subtitle),
        DIM_TEXT,
    )));

    // Blank line
    lines.push(Line::from(""));

    // Description for FRE
    if setup.reason == crate::app::SetupReason::FirstRun
        || setup.reason == crate::app::SetupReason::SwitchAgent
    {
        lines.push(Line::from(Span::styled(
            "  Choose the default agent CLI you would like to use in Intelligent Terminal. You can",
            DIM_TEXT,
        )));
        lines.push(Line::from(Span::styled(
            "  navigate to Settings to configure and set up your workspace.",
            DIM_TEXT,
        )));
        lines.push(Line::from(""));
    }

    // Info messages (e.g. "Copied to clipboard") — shown before options
    if !setup.install_in_progress && setup.install_error.is_none() && !setup.install_log.is_empty() {
        for (i, log_line) in setup.install_log.iter().enumerate() {
            let prefix = if i == 0 { "  \u{2714} " } else { "    " };
            let style = if i == 0 { Style::new().fg(Color::Green) } else { DIM_TEXT };
            lines.push(Line::from(vec![
                Span::styled(prefix, style),
                Span::styled(log_line.clone(), style),
            ]));
        }
        lines.push(Line::from(""));
    }

    // Options list
    let spinner_char = SPINNER[app.activity_frame as usize % SPINNER.len()];

    for (i, opt) in setup.options.iter().enumerate() {
        let is_selected = i == setup.selected_index;

        let (label, status_text) = match opt {
            SetupOption::SelectAgent { agent } => {
                let is_installing = setup.install_in_progress
                    && agent.can_auto_install()
                    && !agent.cli_found;
                let status = if is_installing {
                    format!("  {} Installing...", spinner_char)
                } else {
                    format!("  ({})", agent.status_label())
                };
                (agent.display_name.clone(), status)
            }
            SetupOption::Reinstall { display_name, .. } => {
                let status = if setup.install_in_progress {
                    format!("  {} installing...", spinner_char)
                } else {
                    "  (Will be installed automatically)".to_string()
                };
                (format!("Reinstall {}", display_name), status)
            }
            SetupOption::SignIn { display_name, .. } => {
                (format!("Sign in to {}", display_name), String::new())
            }
            SetupOption::SwitchAgent { agent } => (
                format!("Switch to {}", agent.display_name),
                format!("  ({})", agent.status_label()),
            ),
            SetupOption::Retry => {
                let label = match setup.reason {
                    crate::app::SetupReason::AgentMissing => "Retry detection",
                    crate::app::SetupReason::AgentError => "I've signed in — retry connection",
                    _ => "Retry connection",
                };
                (label.to_string(), String::new())
            }
        };

        let is_installing_select = matches!(opt, SetupOption::SelectAgent { ref agent } if
            setup.install_in_progress && agent.can_auto_install() && !agent.cli_found);
        let is_installing_opt = is_installing_select
            || (matches!(opt, SetupOption::Reinstall { .. }) && setup.install_in_progress);
        let status_style = if is_installing_opt {
            Style::new().fg(Color::Yellow)
        } else if is_selected {
            Style::new().fg(SELECTED_COLOR)
        } else {
            Style::new().fg(Color::White)
        };

        if is_selected {
            lines.push(Line::from(vec![
                Span::styled(
                    "  > ",
                    Style::new()
                        .fg(SELECTED_COLOR)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(label, Style::new().fg(SELECTED_COLOR)),
                Span::styled(status_text, status_style),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(label, Style::new().fg(Color::White)),
                Span::styled(status_text, status_style),
            ]));
        }
    }

    // Install progress or info messages (shown below options)
    if setup.install_in_progress {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("  ", DIM_TEXT),
            Span::styled(
                format!("{}", spinner_char),
                Style::new().fg(Color::Yellow),
            ),
            Span::styled(
                " Installing via winget...",
                Style::new().fg(Color::White),
            ),
        ]));
        for log_line in setup.install_log.iter() {
            lines.push(Line::from(vec![
                Span::styled("    ", DIM_TEXT),
                Span::styled(log_line.clone(), DIM_TEXT),
            ]));
        }
    }


    // Install error
    if let Some(ref err) = setup.install_error {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("  ", DIM_TEXT),
            Span::styled("Install failed: ", Style::new().fg(Color::Red)),
            Span::styled(err.clone(), Style::new().fg(Color::Red)),
        ]));
        for log_line in setup
            .install_log
            .iter()
            .rev()
            .take(3)
            .collect::<Vec<_>>()
            .iter()
            .rev()
        {
            lines.push(Line::from(vec![
                Span::styled("    ", DIM_TEXT),
                Span::styled((*log_line).clone(), DIM_TEXT),
            ]));
        }
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, area);
}
