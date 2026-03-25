use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::App;
use crate::theme;

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let Some(recommendations) = &app.recommendations else {
        return;
    };

    let mut lines = Vec::new();
    for (idx, choice) in recommendations.choices.iter().enumerate() {
        let selected = idx == app.selected_recommendation;
        let recommended = recommendations.recommended_choice == Some(choice.choice);
        let prefix = if selected { ">" } else { " " };
        let marker = if recommended { "*" } else { " " };
        let title = format!("{}{} {}. {}", prefix, marker, choice.choice, choice.title);
        lines.push(Line::from(Span::styled(
            title,
            if selected {
                theme::SELECTED
            } else {
                theme::RECOMMENDATION_TITLE
            },
        )));

        if !choice.rationale.trim().is_empty() {
            lines.push(Line::from(Span::styled(
                format!("   {}", choice.rationale),
                theme::RECOMMENDATION_DETAIL,
            )));
        }
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "Up/Down: select | Enter: execute selected | Type to ask follow-up",
        theme::DIM,
    )));

    let paragraph = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(" Next Steps "))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}
