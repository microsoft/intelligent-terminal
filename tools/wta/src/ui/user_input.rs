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
        let value_width = width.saturating_sub(marker.width());
        let value = if request.input.is_empty() {
            match (selected, value_width) {
                (true, 2..) => "_█".to_string(),
                (true, 1) => "█".to_string(),
                (false, 1..) => "_".to_string(),
                _ => String::new(),
            }
        } else if selected {
            input_with_caret(&request.input, request.cursor_pos, value_width)
        } else {
            tail_to_width(&request.input, value_width)
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

fn head_to_width(value: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut kept = String::new();
    let mut used: usize = 0;
    let mut omitted = false;
    for character in value.chars() {
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
    kept
}

fn input_with_caret(value: &str, cursor_pos: usize, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut cursor_pos = cursor_pos.min(value.len());
    while cursor_pos > 0 && !value.is_char_boundary(cursor_pos) {
        cursor_pos -= 1;
    }
    let before = &value[..cursor_pos];
    let after = &value[cursor_pos..];
    let text_width = width.saturating_sub(1);
    let mut after_context_width = after
        .chars()
        .next()
        .map(|character| UnicodeWidthChar::width(character).unwrap_or(0).max(1))
        .unwrap_or(0)
        .min(text_width);
    if after.chars().nth(1).is_some() && after_context_width < text_width {
        after_context_width += 1;
    }
    let visible_before = tail_to_width(before, text_width.saturating_sub(after_context_width));
    let remaining = text_width.saturating_sub(visible_before.width());
    let visible_after = head_to_width(after, remaining);
    format!("{visible_before}█{visible_after}")
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
            cursor_pos: "abcdefghijklmnopqrstuvwxyz".len(),
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
        let visible = input_with_caret("abcdefghijklmnopqrstuvwxyz", 26, 9);
        assert!(visible.width() <= 9);
        assert!(visible.starts_with('…'));
        assert!(visible.ends_with('█'));
    }

    #[test]
    fn freeform_renders_the_caret_at_the_cursor() {
        assert_eq!(input_with_caret("abc", 2, 8), "ab█c");
    }

    #[test]
    fn freeform_keeps_context_around_a_scrolled_caret() {
        let visible = input_with_caret("abcdefghij", 5, 6);
        assert_eq!(visible, "…de█f…");
        assert_eq!(visible.width(), 6);
    }

    #[test]
    fn empty_freeform_keeps_the_caret_in_a_narrow_row() {
        let mut request = state(2);
        request.input.clear();
        request.cursor_pos = 0;
        let (lines, _, _) = content_lines(&request, 3);
        let freeform = lines.last().unwrap();
        assert_eq!(freeform.width(), 3);
        assert_eq!(freeform.to_string(), "● █");
    }
}
