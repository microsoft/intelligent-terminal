//! Message presentation shared by the Agent Pane and Agent Center.

use std::borrow::Cow;

use ratatui::prelude::*;

use crate::theme;

pub(super) const MAX_RENDER_LINE_CHARS: usize = 4096;

#[derive(Clone, Copy)]
pub(crate) enum MessageRole {
    User,
    Assistant,
    System,
    Unknown,
}

/// Render source text without interpreting Markdown or changing its role.
/// Callers omit the final spacer while the last assistant message is streaming.
pub(crate) fn message_lines<'a>(
    role: MessageRole,
    text: &'a str,
    width: usize,
    trailing_blank: bool,
) -> Vec<Line<'a>> {
    let mut lines = Vec::new();
    match role {
        MessageRole::User => push_prompt_prefixed_lines(&mut lines, text, width),
        MessageRole::Assistant => {
            push_dot_prefixed_lines(&mut lines, text, width, theme::DOT_AGENT, theme::AGENT_TEXT);
        }
        MessageRole::System | MessageRole::Unknown => {
            let style = if matches!(role, MessageRole::Unknown) {
                lines.push(Line::from(Span::styled(
                    t!("agent_center.ui_role_unknown").into_owned(),
                    theme::DIM,
                )));
                theme::DIM
            } else {
                theme::SYSTEM_TEXT
            };
            // Unprefixed messages retain Paragraph's wrapping behavior.
            for line_text in text.lines() {
                lines.push(Line::from(Span::styled(
                    truncate_render_text(line_text),
                    style,
                )));
            }
        }
    }
    if trailing_blank {
        lines.push(Line::default());
    }
    lines
}

// Render a multi-line text block with a colored dot prefix on the first
// visual row and a 2-cell hanging indent on every continuation row (both
// for explicit \n breaks AND for soft-wrapped continuations of long
// paragraphs). Without this, ratatui's Paragraph word-wrap pushes
// continuation rows back to column 0 and the bullet alignment breaks.
pub(super) fn push_dot_prefixed_lines<'a>(
    lines: &mut Vec<Line<'a>>,
    text: &str,
    wrap_width: usize,
    dot_style: Style,
    text_style: Style,
) {
    // Reserve 2 cells for either "● " or the continuation indent.
    let body_width = wrap_width.saturating_sub(2).max(1);
    let mut first_row = true;

    for paragraph in text.trim_end_matches(['\r', '\n']).split('\n') {
        if paragraph.is_empty() {
            // Skip leading blanks so the dot lands on the first content row
            // — many models prefix prose with `\n` / `\n\n`, which would
            // otherwise burn the dot on an empty line. Blank lines between
            // paragraphs are still preserved.
            if first_row {
                continue;
            }
            lines.push(Line::default());
            continue;
        }

        let wrapped = textwrap::wrap(paragraph, body_width);
        for piece in wrapped {
            let piece_str = truncate_render_text(&piece).into_owned();
            if first_row {
                lines.push(Line::from(vec![
                    Span::styled("● ", dot_style),
                    Span::styled(piece_str, text_style),
                ]));
                first_row = false;
            } else {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(piece_str, text_style),
                ]));
            }
        }
    }
}

pub(super) fn push_prefixed_lines<'a>(
    lines: &mut Vec<Line<'a>>,
    marker: &'static str,
    text: &str,
    wrap_width: usize,
    style: Style,
) {
    let body_width = wrap_width.saturating_sub(2).max(1);
    let mut first_row = true;

    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            if first_row {
                continue;
            }
            lines.push(Line::default());
            continue;
        }

        for piece in textwrap::wrap(paragraph, body_width) {
            let piece_str = truncate_render_text(&piece).into_owned();
            if first_row {
                lines.push(Line::from(vec![
                    Span::styled(format!("{marker} "), style),
                    Span::styled(piece_str, style),
                ]));
                first_row = false;
            } else {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(piece_str, style),
                ]));
            }
        }
    }
}

/// Mirrors `push_dot_prefixed_lines`, but for the user's own submitted
/// prompt: splits on embedded `\n` (from Shift+Enter multi-line input) and
/// wraps each paragraph so every line is a real `ratatui::Line` — ratatui
/// does not turn an embedded `\n` inside a single `Span`/`Line` into
/// multiple rows, so without this split any line after the first would
/// never appear in the rendered transcript (see issue #492). The first
/// rendered row gets the `"> "` prompt marker; continuation rows get a
/// matching 2-cell indent. Height measurement consumes these same rendered
/// lines and counts their terminal display width.
pub(super) fn push_prompt_prefixed_lines<'a>(
    lines: &mut Vec<Line<'a>>,
    text: &'a str,
    wrap_width: usize,
) {
    let body_width = wrap_width.saturating_sub(2).max(1);
    let mut first_row = true;

    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            // Unlike `push_dot_prefixed_lines`, the prompt marker must never
            // be dropped: an empty submitted prompt, or one starting with a
            // newline, still needs a "> " row so the transcript shows the
            // user turn happened at all.
            if first_row {
                lines.push(Line::from(Span::styled("> ", theme::USER_PROMPT)));
                first_row = false;
            } else {
                lines.push(Line::default());
            }
            continue;
        }

        // `textwrap::wrap` borrows from `paragraph` (itself borrowed from the
        // `'a` input) whenever a piece needs no reflowing, so the typical
        // short single-line prompt renders with zero allocations here;
        // `truncate_render_cow` preserves that borrow unless the piece is
        // actually rewrapped or exceeds `MAX_RENDER_LINE_CHARS`.
        let wrapped = textwrap::wrap(paragraph, body_width);
        for piece in wrapped {
            let piece_str = truncate_render_cow(piece);
            if first_row {
                lines.push(Line::from(vec![
                    Span::styled("> ", theme::USER_PROMPT),
                    Span::styled(piece_str, theme::USER_PROMPT),
                ]));
                first_row = false;
            } else {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(piece_str, theme::USER_PROMPT),
                ]));
            }
        }
    }
}

/// Applies `truncate_render_text`'s length cap to an already-computed
/// `Cow`, without forcing an allocation when the input is borrowed and
/// under the limit (unlike `truncate_render_text(&cow).into_owned()`).
fn truncate_render_cow<'a>(text: Cow<'a, str>) -> Cow<'a, str> {
    match text {
        Cow::Borrowed(s) => truncate_render_text(s),
        Cow::Owned(s) => match truncate_render_text(&s) {
            Cow::Borrowed(_) => Cow::Owned(s),
            Cow::Owned(truncated) => Cow::Owned(truncated),
        },
    }
}

pub(super) fn truncate_render_text(text: &str) -> Cow<'_, str> {
    let char_count = text.chars().count();
    if char_count <= MAX_RENDER_LINE_CHARS {
        return Cow::Borrowed(text);
    }

    let head_chars = MAX_RENDER_LINE_CHARS * 3 / 4;
    let tail_chars = MAX_RENDER_LINE_CHARS / 4;
    let omitted = char_count.saturating_sub(head_chars + tail_chars);
    let head: String = text.chars().take(head_chars).collect();
    let tail: String = text
        .chars()
        .skip(char_count.saturating_sub(tail_chars))
        .collect();

    Cow::Owned(format!("{head} ...<{omitted} chars omitted>... {tail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn user_keeps_empty_and_leading_newline_markers() {
        assert_eq!(
            texts(&message_lines(MessageRole::User, "", 40, false)),
            ["> "]
        );
        assert_eq!(
            texts(&message_lines(MessageRole::User, "\nA\n\nB\n", 40, false)),
            ["> ", "  A", "", "  B", ""]
        );
    }

    #[test]
    fn assistant_keeps_paragraphs_but_not_leading_or_trailing_blank_lines() {
        assert_eq!(
            texts(&message_lines(
                MessageRole::Assistant,
                "\n\nA\n\nB\n\n",
                40,
                false,
            )),
            ["● A", "", "  B"]
        );
    }

    #[test]
    fn unicode_wrapping_preserves_text_and_hanging_indent() {
        let text = "你好世界你好世界";
        for role in [MessageRole::User, MessageRole::Assistant] {
            let lines = message_lines(role, text, 8, false);
            assert!(lines.len() > 1);
            assert!(lines.iter().all(|line| line.width() <= 8));
            assert!(texts(&lines)[1..].iter().all(|line| line.starts_with("  ")));
            let body: String = lines
                .iter()
                .flat_map(|line| line.spans.iter().skip(1))
                .map(|span| span.content.as_ref())
                .collect();
            assert_eq!(body, text);
        }
    }

    #[test]
    fn source_literals_are_not_interpreted_as_markdown_or_escapes() {
        let text = r#"  **literal**  `code` C:\work\file \n <tag>"#;
        for role in [MessageRole::User, MessageRole::Assistant] {
            let lines = message_lines(role, text, 120, false);
            assert_eq!(lines.len(), 1);
            assert_eq!(lines[0].spans[1].content, text);
        }
        let lines = message_lines(MessageRole::System, text, 120, false);
        assert_eq!(texts(&lines), [text]);
        assert_eq!(lines[0].spans[0].style, theme::SYSTEM_TEXT);
    }

    #[test]
    fn short_user_text_remains_borrowed() {
        let lines = message_lines(MessageRole::User, "short prompt", 80, false);
        assert!(matches!(
            lines[0].spans[1].content,
            Cow::Borrowed("short prompt")
        ));
        assert_eq!(lines[0].spans[0].style, theme::USER_PROMPT);
    }

    #[test]
    fn trailing_blank_flag_adds_exactly_one_spacer() {
        for role in [
            MessageRole::User,
            MessageRole::Assistant,
            MessageRole::System,
            MessageRole::Unknown,
        ] {
            for text in ["", "answer", "answer\n\n"] {
                let streaming = message_lines(role, text, 40, false);
                let completed = message_lines(role, text, 40, true);
                assert_eq!(completed.len(), streaming.len() + 1);
                assert_eq!(&completed[..streaming.len()], streaming.as_slice());
                assert_eq!(completed.last(), Some(&Line::default()));
            }
        }
    }

    #[test]
    fn system_keeps_explicit_lines_and_borrowed_source() {
        let lines = message_lines(MessageRole::System, "first  \n\n  second\n", 3, false);
        assert_eq!(texts(&lines), ["first  ", "", "  second"]);
        assert!(lines.iter().all(|line| {
            line.spans[0].style == theme::SYSTEM_TEXT
                && matches!(line.spans[0].content, Cow::Borrowed(_))
        }));
    }

    #[test]
    fn unknown_has_localized_heading_and_does_not_impersonate_assistant() {
        let lines = message_lines(MessageRole::Unknown, "recorded", 80, false);
        assert_eq!(
            texts(&lines),
            [
                t!("agent_center.ui_role_unknown").into_owned(),
                "recorded".into()
            ]
        );
        assert!(lines.iter().all(|line| line.spans[0].style == theme::DIM));
    }

    #[test]
    fn long_unicode_source_keeps_head_tail_and_omission_marker() {
        let text = format!("HEAD{}TAIL", "界".repeat(MAX_RENDER_LINE_CHARS));
        let lines = message_lines(MessageRole::System, &text, 80, false);
        let rendered = &lines[0].spans[0].content;
        assert!(rendered.starts_with("HEAD"));
        assert!(rendered.ends_with("TAIL"));
        assert!(rendered.contains("<8 chars omitted>"));
    }
}
