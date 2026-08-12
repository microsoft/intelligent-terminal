use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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

    let [body, hint] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);
    let (lines, selected_start, selected_end) = content_lines(request, body.width as usize);
    let visible_rows = body.height as usize;
    let scroll = if selected_end > visible_rows {
        selected_end.saturating_sub(visible_rows)
    } else {
        selected_start.min(lines.len().saturating_sub(visible_rows))
    };

    frame.render_widget(
        Paragraph::new(lines).scroll((scroll.min(u16::MAX as usize) as u16, 0)),
        body,
    );
    frame.render_widget(
        Paragraph::new(Line::styled("↑ ↓   ↵   Esc", theme::DIM)),
        hint,
    );
}

fn content_lines(request: &UserInputState, width: usize) -> (Vec<Line<'static>>, usize, usize) {
    let mut lines = wrap_text(&request.request.question, width)
        .into_iter()
        .map(|line| Line::styled(line, theme::INPUT_TEXT))
        .collect::<Vec<_>>();
    lines.push(Line::from(""));
    let mut selected_start = 0;
    let mut selected_end = 1;

    for (index, choice) in request.request.choices.iter().enumerate() {
        let selected = request.selected == index;
        let marker = if selected { "● " } else { "○ " };
        let style = if request.selected == index {
            theme::SELECTED
        } else {
            theme::INPUT_TEXT
        };
        let start = lines.len();
        push_wrapped_option(&mut lines, marker, choice, width, style);
        if selected {
            selected_start = start;
            selected_end = lines.len();
        }
    }
    if request.request.allow_freeform {
        let selected = request.freeform_selected();
        let marker = if selected { "● " } else { "○ " };
        let value = if request.input.is_empty() {
            "_".to_string()
        } else {
            tail_to_width(
                &request.input,
                width.saturating_sub(marker.width() + usize::from(selected)),
            )
        };
        let value = if selected {
            format!("{value}█")
        } else {
            value
        };
        let start = lines.len();
        lines.push(Line::styled(
            format!("{marker}{value}"),
            if selected {
                theme::SELECTED
            } else {
                theme::INPUT_TEXT
            },
        ));
        if selected {
            selected_start = start;
            selected_end = lines.len();
        }
    }
    (lines, selected_start, selected_end)
}

fn push_wrapped_option(
    lines: &mut Vec<Line<'static>>,
    marker: &str,
    value: &str,
    width: usize,
    style: Style,
) {
    let content_width = width.saturating_sub(marker.width()).max(1);
    let wrapped = wrap_text(value, content_width);
    for (index, line) in wrapped.into_iter().enumerate() {
        let prefix = if index == 0 { marker } else { "  " };
        lines.push(Line::styled(format!("{prefix}{line}"), style));
    }
}

fn wrap_text(value: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut output = Vec::new();
    for source_line in value.split('\n') {
        let mut line = String::new();
        let mut line_width: usize = 0;
        for character in source_line.chars() {
            let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
            if line_width > 0 && line_width.saturating_add(character_width) > width {
                output.push(std::mem::take(&mut line));
                line_width = 0;
            }
            line.push(character);
            line_width = line_width.saturating_add(character_width);
        }
        output.push(line);
    }
    output
}

fn tail_to_width(value: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut kept = Vec::new();
    let mut used: usize = 0;
    let mut omitted = false;
    for character in value.chars().rev() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used.saturating_add(character_width) > width {
            omitted = true;
            break;
        }
        kept.push(character);
        used = used.saturating_add(character_width);
    }
    if omitted {
        while used >= width {
            let Some(character) = kept.pop() else {
                break;
            };
            used = used.saturating_sub(UnicodeWidthChar::width(character).unwrap_or(0));
        }
        kept.push('…');
    }
    kept.into_iter().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_tools::user_input::UserInputRequest;

    fn state(selected: usize) -> UserInputState {
        UserInputState {
            request_id: "request".into(),
            request: UserInputRequest {
                question: "A long question that wraps".into(),
                choices: vec!["first long choice".into(), "second long choice".into()],
                allow_freeform: true,
            },
            selected,
            input: "abcdefghijklmnopqrstuvwxyz".into(),
            responder: None,
        }
    }

    #[test]
    fn selected_option_range_tracks_wrapped_rows() {
        let (_, start, end) = content_lines(&state(1), 8);
        assert!(end > start + 1);
    }

    #[test]
    fn freeform_keeps_the_visible_tail_bounded() {
        let visible = tail_to_width("abcdefghijklmnopqrstuvwxyz", 8);
        assert!(visible.width() <= 8);
        assert!(visible.starts_with('…'));
        assert!(visible.ends_with('z'));
    }
}
