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
        let line = if selected {
            selected_freeform_line(marker, &request.input, request.cursor_pos, value_width)
        } else {
            let value = if request.input.is_empty() {
                if value_width > 0 {
                    "_".to_string()
                } else {
                    String::new()
                }
            } else {
                tail_to_width(&request.input, value_width)
            };
            Line::styled(format!("{marker}{value}"), theme::INPUT_TEXT)
        };
        let start = lines.len();
        lines.push(line);
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

fn selected_freeform_line(
    marker: &'static str,
    value: &str,
    cursor_pos: usize,
    width: usize,
) -> Line<'static> {
    let mut spans = vec![Span::styled(marker, theme::SELECTED)];
    if width == 0 {
        return Line::from(spans);
    }
    if value.is_empty() {
        spans.push(Span::styled(
            "_",
            theme::SELECTED.add_modifier(Modifier::REVERSED),
        ));
        return Line::from(spans);
    }

    let mut cursor_pos = cursor_pos.min(value.len());
    while cursor_pos > 0 && !value.is_char_boundary(cursor_pos) {
        cursor_pos -= 1;
    }
    let before = &value[..cursor_pos];
    let after = &value[cursor_pos..];
    let mut after_chars = after.chars();
    let caret_character = after_chars.next();
    let after_caret = after_chars.as_str();
    let raw_caret = caret_character
        .map(|character| character.to_string())
        .unwrap_or_else(|| " ".to_string());
    let caret_text = if raw_caret.width() <= width {
        raw_caret
    } else {
        "…".to_string()
    };
    let caret_width = caret_text.width();
    let text_width = width.saturating_sub(caret_width);
    let mut after_context_width = after_caret
        .chars()
        .next()
        .map(|character| UnicodeWidthChar::width(character).unwrap_or(0).max(1))
        .unwrap_or(0)
        .min(text_width);
    if after_caret.chars().nth(1).is_some() && after_context_width < text_width {
        after_context_width += 1;
    }
    let visible_before = tail_to_width(before, text_width.saturating_sub(after_context_width));
    let remaining = text_width.saturating_sub(visible_before.width());
    let visible_after = head_to_width(after_caret, remaining);

    if !visible_before.is_empty() {
        spans.push(Span::styled(visible_before, theme::SELECTED));
    }
    spans.push(Span::styled(
        caret_text,
        theme::SELECTED.add_modifier(Modifier::REVERSED),
    ));
    if !visible_after.is_empty() {
        spans.push(Span::styled(visible_after, theme::SELECTED));
    }
    Line::from(spans)
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
        let (lines, _, _) = content_lines(&state(2), 11);
        let freeform = lines.last().unwrap();
        assert!(freeform.width() <= 11);
        assert!(freeform.to_string().starts_with("● …"));
        assert!(freeform.to_string().ends_with(' '));
    }

    #[test]
    fn freeform_renders_the_caret_at_the_cursor() {
        let mut request = state(2);
        request.input = "abc".into();
        request.cursor_pos = 2;
        let (lines, _, _) = content_lines(&request, 8);
        let freeform = lines.last().unwrap();

        assert_eq!(freeform.to_string(), "● abc");
        assert_eq!(freeform.width(), "● abc".width());
        let caret = freeform
            .spans
            .iter()
            .find(|span| span.style.add_modifier.contains(Modifier::REVERSED))
            .expect("selected freeform input should paint a reverse-video caret");
        assert_eq!(caret.content.as_ref(), "c");
    }

    #[test]
    fn freeform_keeps_context_around_a_scrolled_caret() {
        let mut request = state(2);
        request.input = "zero alpha beta gamma".into();
        request.cursor_pos = "zero alpha ".len();
        let (lines, _, _) = content_lines(&request, 13);
        let freeform = lines.last().unwrap();
        assert_eq!(freeform.to_string(), "● … alpha be…");
        assert_eq!(freeform.width(), 13);
        let caret = freeform
            .spans
            .iter()
            .find(|span| span.style.add_modifier.contains(Modifier::REVERSED))
            .expect("scrolled freeform input should paint a reverse-video caret");
        assert_eq!(caret.content.as_ref(), "b");
    }

    #[test]
    fn empty_freeform_keeps_the_caret_in_a_narrow_row() {
        let mut request = state(2);
        request.input.clear();
        request.cursor_pos = 0;
        let (lines, _, _) = content_lines(&request, 3);
        let freeform = lines.last().unwrap();
        assert_eq!(freeform.width(), 3);
        assert_eq!(freeform.to_string(), "● _");
        let caret = freeform
            .spans
            .iter()
            .find(|span| span.style.add_modifier.contains(Modifier::REVERSED))
            .expect("empty freeform input should reverse its placeholder");
        assert_eq!(caret.content.as_ref(), "_");
    }

    #[test]
    fn freeform_caret_preserves_a_zero_width_character() {
        let mut request = state(2);
        request.input = "e\u{301}x".into();
        request.cursor_pos = "e".len();
        let (lines, _, _) = content_lines(&request, 8);
        let freeform = lines.last().unwrap();

        assert_eq!(freeform.to_string(), "● e\u{301}x");
        assert_eq!(freeform.width(), "● e\u{301}x".width());
        let caret = freeform
            .spans
            .iter()
            .find(|span| span.style.add_modifier.contains(Modifier::REVERSED))
            .expect("zero-width cursor character should remain in a caret span");
        assert_eq!(caret.content.as_ref(), "\u{301}");
    }

    #[test]
    fn freeform_caret_reverses_a_wide_character_without_shifting_text() {
        let mut request = state(2);
        request.input = "a界b".into();
        request.cursor_pos = "a".len();
        let (lines, _, _) = content_lines(&request, 8);
        let freeform = lines.last().unwrap();

        assert_eq!(freeform.to_string(), "● a界b");
        assert_eq!(freeform.width(), "● a界b".width());
        let caret = freeform
            .spans
            .iter()
            .find(|span| span.style.add_modifier.contains(Modifier::REVERSED))
            .expect("wide cursor character should be reversed in place");
        assert_eq!(caret.content.as_ref(), "界");
    }

    #[test]
    fn freeform_end_caret_follows_a_wide_character() {
        let mut request = state(2);
        request.input = "a界".into();
        request.cursor_pos = request.input.len();
        let (lines, _, _) = content_lines(&request, 6);
        let freeform = lines.last().unwrap();

        assert_eq!(freeform.to_string(), "● a界 ");
        assert_eq!(freeform.width(), 6);
        let caret = freeform
            .spans
            .iter()
            .find(|span| span.style.add_modifier.contains(Modifier::REVERSED))
            .expect("end caret should reverse a trailing space");
        assert_eq!(caret.content.as_ref(), " ");
    }
}
