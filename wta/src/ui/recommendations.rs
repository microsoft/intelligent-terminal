use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Padding, Paragraph, Wrap};

use crate::app::App;
use crate::coordinator::{OpenTarget, RecommendedAction};
use crate::theme;

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let Some(recommendations) = &app.recommendations else {
        return;
    };

    let mut lines: Vec<Line> = Vec::new();

    for (idx, choice) in recommendations.choices.iter().enumerate() {
        let is_selected = idx == app.selected_recommendation;
        let is_recommended = recommendations.recommended_choice == Some(choice.choice);

        // Title line: "* 1. Install missing build tools" or "  2. Explain further"
        let marker = if is_recommended { "* " } else { "  " };
        let title_style = if is_selected {
            theme::RECOMMENDATION_TITLE
        } else {
            theme::RECOMMENDATION_DETAIL
        };
        lines.push(Line::from(Span::styled(
            format!("{}{}. {}", marker, choice.choice, choice.title),
            title_style,
        )));

        // Determine card content based on action type
        let (command_text, buttons) = extract_card_content(choice, app, is_selected);
        let border_style = if is_selected {
            theme::CARD_BORDER_SELECTED
        } else {
            theme::CARD_BORDER
        };

        // Top border
        let card_width = area.width.saturating_sub(4) as usize; // indent 2 + margin
        let inner_width = card_width.saturating_sub(2); // minus left/right border chars
        lines.push(Line::from(Span::styled(
            format!("  ┌{}┐", "─".repeat(inner_width)),
            border_style,
        )));

        // Command/content lines
        for cmd_line in wrap_text(&command_text, inner_width.saturating_sub(2)) {
            let padded = format!(" {} ", pad_right(&cmd_line, inner_width.saturating_sub(2)));
            lines.push(Line::from(vec![
                Span::styled("  │", border_style),
                Span::styled(padded, theme::CARD_CODE),
                Span::styled("│", border_style),
            ]));
        }

        // Separator line
        lines.push(Line::from(Span::styled(
            format!("  ├{}┤", "─".repeat(inner_width)),
            border_style,
        )));

        // Button row
        let button_spans = build_button_spans(
            &buttons,
            is_selected,
            app.selected_button,
            inner_width,
            border_style,
        );
        lines.push(Line::from(button_spans));

        // Bottom border
        lines.push(Line::from(Span::styled(
            format!("  └{}┘", "─".repeat(inner_width)),
            border_style,
        )));

        // Spacing between cards
        lines.push(Line::default());
    }

    // Hint line
    lines.push(Line::from(Span::styled(
        "↑↓: switch | ←→: button | Enter: activate | Esc: dismiss",
        theme::DIM,
    )));

    let paragraph = Paragraph::new(lines)
        .block(Block::default().borders(Borders::NONE).padding(Padding::zero()))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

/// Extracts the display text and button labels from a choice's actions.
fn extract_card_content(
    choice: &crate::coordinator::RecommendationChoice,
    _app: &App,
    _is_selected: bool,
) -> (String, Vec<String>) {
    // Find the primary action
    for action in &choice.actions {
        match action {
            RecommendedAction::Send { input, .. } => {
                return (
                    input.clone(),
                    vec!["Insert in Terminal".into(), "Run ↵".into()],
                );
            }
            RecommendedAction::OpenAndSend {
                target,
                input,
                agent,
                ..
            } => {
                let agent_label = agent.as_deref().unwrap_or("agent");
                let display = format!("{}: {}", agent_label, input);
                let target_label = match target {
                    OpenTarget::Tab => "Open in New Tab ↵",
                    OpenTarget::Panel => "Open in New Panel ↵",
                };
                return (display, vec![target_label.into()]);
            }
        }
    }

    // Fallback: just show the title
    (choice.title.clone(), vec!["Execute ↵".into()])
}

/// Builds styled spans for the button row inside a card.
fn build_button_spans<'a>(
    buttons: &[String],
    is_selected: bool,
    focused_button: usize,
    inner_width: usize,
    border_style: Style,
) -> Vec<Span<'a>> {
    let mut spans = Vec::new();
    spans.push(Span::styled("  │", border_style));

    // Build button text pieces
    let mut button_pieces: Vec<(String, Style)> = Vec::new();
    for (i, label) in buttons.iter().enumerate() {
        if i > 0 {
            button_pieces.push((" ".into(), theme::DIM));
        }
        let style = if is_selected && i == focused_button {
            theme::BUTTON_FOCUSED
        } else {
            theme::BUTTON
        };
        button_pieces.push((format!("[{}]", label), style));
    }

    // Calculate total button text width
    let buttons_width: usize = button_pieces.iter().map(|(t, _)| t.len()).sum();
    // Right-align: pad left
    let pad_left = inner_width.saturating_sub(buttons_width + 1);
    spans.push(Span::raw(" ".repeat(pad_left)));

    for (text, style) in button_pieces {
        spans.push(Span::styled(text, style));
    }

    // Fill remaining space
    let used: usize = pad_left + buttons_width;
    if used < inner_width {
        spans.push(Span::raw(" ".repeat(inner_width - used)));
    }

    spans.push(Span::styled("│", border_style));
    spans
}

/// Simple text wrapping.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    for raw_line in text.lines() {
        if raw_line.is_empty() {
            lines.push(String::new());
            continue;
        }
        let chars: Vec<char> = raw_line.chars().collect();
        for chunk in chars.chunks(width) {
            lines.push(chunk.iter().collect());
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Pads a string with spaces to the right to reach the target width.
fn pad_right(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - len))
    }
}
