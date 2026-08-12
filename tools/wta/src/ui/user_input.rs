use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph, Wrap};

use crate::app::UserInputState;
use crate::theme;

use super::popup;

const MAX_VISIBLE_ROWS: u16 = 14;

pub fn render(frame: &mut Frame, request: &UserInputState, input_area: Rect) {
    let content_rows = (request.request.choices.len() as u16)
        .saturating_add(u16::from(request.request.allow_freeform))
        .saturating_add(4)
        .min(MAX_VISIBLE_ROWS);
    let area = popup::anchored_above(frame, input_area, content_rows);
    frame.render_widget(Clear, area);

    let block = popup::block(" ? ".to_string());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }

    let mut lines = vec![
        Line::styled(request.request.question.as_str(), theme::INPUT_TEXT),
        Line::from(""),
    ];
    for (index, choice) in request.request.choices.iter().enumerate() {
        let marker = if request.selected == index { "● " } else { "○ " };
        let style = if request.selected == index {
            theme::SELECTED
        } else {
            theme::INPUT_TEXT
        };
        lines.push(Line::styled(format!("{marker}{choice}"), style));
    }
    if request.request.allow_freeform {
        let selected = request.freeform_selected();
        let marker = if selected { "● " } else { "○ " };
        let value = if request.input.is_empty() {
            "_".to_string()
        } else if selected {
            format!("{}█", request.input)
        } else {
            request.input.clone()
        };
        lines.push(Line::styled(
            format!("{marker}{value}"),
            if selected {
                theme::SELECTED
            } else {
                theme::INPUT_TEXT
            },
        ));
    }
    lines.push(Line::from(""));
    lines.push(Line::styled("↑ ↓   ↵   Esc", theme::DIM));

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}
