use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::app::App;

const SPINNER: &[char] = &['\u{280B}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283C}', '\u{2834}', '\u{2826}', '\u{2827}', '\u{2807}', '\u{280F}'];

// Figma: rgba(255,255,255,0.6) ≈ #999999
const DIM_TEXT: Style = Style::new().fg(Color::Rgb(153, 153, 153));

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let setup = match &app.setup {
        Some(s) => s,
        None => return,
    };

    // Horizontal padding (matching chat area)
    let padded = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
        .split(area);
    let area = padded[1];

    let mut lines: Vec<Line> = Vec::new();

    // "● Welcome to Intelligent Terminal!" — white bold
    lines.push(Line::from(vec![
        Span::styled("● ", Style::new().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(
            setup.reason.title(),
            Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
    ]));

    // "  Getting started" — dim
    lines.push(Line::from(Span::styled("  Getting started", DIM_TEXT)));

    // Blank line
    lines.push(Line::from(""));

    // Description — dim
    lines.push(Line::from(Span::styled(
        "  Choose the default agent CLI you would like to use in Intelligent Terminal. You can",
        DIM_TEXT,
    )));
    lines.push(Line::from(Span::styled(
        "  navigate to Settings to configure and set up your workspace.",
        DIM_TEXT,
    )));

    // Blank line before agent list
    lines.push(Line::from(""));

    // Agent list
    let spinner_char = SPINNER[app.activity_frame as usize % SPINNER.len()];

    for (i, agent) in setup.agents.iter().enumerate() {
        let is_selected = i == setup.selected_index;
        let is_installing = agent.status == "Installing...";

        // Status text: use spinner for installing agents
        let status_text = if is_installing {
            format!("  {} Installing...", spinner_char)
        } else {
            format!("  ({})", &agent.status)
        };

        let status_style = if is_installing {
            Style::new().fg(Color::Yellow)
        } else if is_selected {
            Style::new().fg(Color::Rgb(96, 205, 255))
        } else {
            Style::new().fg(Color::White)
        };

        if is_selected {
            lines.push(Line::from(vec![
                Span::styled(
                    "  > ",
                    Style::new().fg(Color::Rgb(96, 205, 255)).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    &agent.name,
                    Style::new().fg(Color::Rgb(96, 205, 255)),
                ),
                Span::styled(status_text, status_style),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(&agent.name, Style::new().fg(Color::White)),
                Span::styled(status_text, status_style),
            ]));
        }
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, area);
}
