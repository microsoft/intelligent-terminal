use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::prelude::*;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme;

#[derive(Clone, Debug)]
struct StyledRun {
    text: String,
    style: Style,
}

#[derive(Clone, Copy, Debug)]
struct ListState {
    next: Option<u64>,
}

#[derive(Clone, Debug)]
struct StyledChar {
    ch: char,
    style: Style,
}

pub(crate) fn render_agent_markdown_lines(
    text: &str,
    wrap_width: usize,
    dot_style: Style,
    base_style: Style,
) -> Vec<Line<'static>> {
    let mut runs = markdown_to_runs(text, base_style);
    trim_trailing_newlines(&mut runs);

    if visible_text_is_empty(&runs) && !text.trim().is_empty() {
        runs = vec![StyledRun {
            text: text.to_string(),
            style: base_style,
        }];
    }

    dot_prefixed_wrapped_lines(&runs, wrap_width, dot_style)
}

pub(crate) fn agent_markdown_height(text: &str, wrap_width: usize) -> usize {
    let mut runs = markdown_to_runs(text, theme::AGENT_TEXT);
    trim_trailing_newlines(&mut runs);

    if visible_text_is_empty(&runs) && !text.trim().is_empty() {
        runs = vec![StyledRun {
            text: text.to_string(),
            style: theme::AGENT_TEXT,
        }];
    }

    dot_prefixed_wrapped_line_count(&runs, wrap_width)
}

fn markdown_to_runs(text: &str, base_style: Style) -> Vec<StyledRun> {
    let parser = Parser::new_ext(text, Options::empty());
    let mut runs = Vec::new();
    let mut styles = vec![base_style];
    let mut lists: Vec<ListState> = Vec::new();
    let mut quote_depth = 0usize;

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => ensure_quote_prefix(&mut runs, quote_depth),
                Tag::Heading { .. } => {
                    ensure_line_start(&mut runs, *styles.last().unwrap());
                    ensure_quote_prefix(&mut runs, quote_depth);
                    push_style(&mut styles, |s| s.add_modifier(Modifier::BOLD));
                }
                Tag::BlockQuote => {
                    quote_depth += 1;
                    ensure_line_start(&mut runs, *styles.last().unwrap());
                }
                Tag::CodeBlock(_) => {
                    ensure_line_start(&mut runs, *styles.last().unwrap());
                    ensure_quote_prefix(&mut runs, quote_depth);
                    push_style(&mut styles, code_style);
                }
                Tag::List(start) => {
                    ensure_line_start(&mut runs, *styles.last().unwrap());
                    ensure_quote_prefix(&mut runs, quote_depth);
                    lists.push(ListState { next: start });
                }
                Tag::Item => {
                    ensure_line_start(&mut runs, *styles.last().unwrap());
                    ensure_quote_prefix(&mut runs, quote_depth);
                    let indent = "  ".repeat(lists.len().saturating_sub(1));
                    let marker = if let Some(list) = lists.last_mut() {
                        if let Some(n) = list.next {
                            list.next = Some(n.saturating_add(1));
                            format!("{indent}{n}. ")
                        } else {
                            format!("{indent}- ")
                        }
                    } else {
                        format!("{indent}- ")
                    };
                    append_text(&mut runs, &marker, *styles.last().unwrap());
                }
                Tag::Emphasis => push_style(&mut styles, |s| s.add_modifier(Modifier::ITALIC)),
                Tag::Strong => push_style(&mut styles, |s| s.add_modifier(Modifier::BOLD)),
                Tag::Link { .. } | Tag::Image { .. } => push_style(&mut styles, link_style),
                Tag::FootnoteDefinition(label) => {
                    ensure_line_start(&mut runs, *styles.last().unwrap());
                    ensure_quote_prefix(&mut runs, quote_depth);
                    append_text(&mut runs, &format!("[^{label}]: "), theme::DIM);
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => {
                    if lists.is_empty() {
                        append_paragraph_break(&mut runs, *styles.last().unwrap());
                    } else {
                        append_newline(&mut runs, *styles.last().unwrap());
                    }
                }
                TagEnd::Item
                | TagEnd::FootnoteDefinition => append_newline(&mut runs, *styles.last().unwrap()),
                TagEnd::Heading(_) | TagEnd::CodeBlock => {
                    append_newline(&mut runs, *styles.last().unwrap());
                    pop_style(&mut styles);
                }
                TagEnd::List(_) => {
                    lists.pop();
                    append_newline(&mut runs, *styles.last().unwrap());
                }
                TagEnd::BlockQuote => {
                    quote_depth = quote_depth.saturating_sub(1);
                    append_newline(&mut runs, *styles.last().unwrap());
                }
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Link | TagEnd::Image => {
                    pop_style(&mut styles);
                }
                _ => {}
            },
            Event::Text(text) => append_text_with_quote_prefix(
                &mut runs,
                &text,
                *styles.last().unwrap(),
                quote_depth,
            ),
            Event::Code(code) => append_text_with_quote_prefix(
                &mut runs,
                &code,
                code_style(*styles.last().unwrap()),
                quote_depth,
            ),
            Event::Html(html) | Event::InlineHtml(html) => {
                append_text_with_quote_prefix(
                    &mut runs,
                    &html,
                    *styles.last().unwrap(),
                    quote_depth,
                )
            }
            Event::FootnoteReference(label) => {
                append_text(&mut runs, &format!("[^{label}]"), *styles.last().unwrap())
            }
            Event::SoftBreak => append_text(&mut runs, " ", *styles.last().unwrap()),
            Event::HardBreak => {
                append_newline(&mut runs, *styles.last().unwrap());
                ensure_quote_prefix(&mut runs, quote_depth);
            }
            Event::Rule => {
                ensure_line_start(&mut runs, *styles.last().unwrap());
                ensure_quote_prefix(&mut runs, quote_depth);
                append_text(&mut runs, "---", theme::DIM);
                append_newline(&mut runs, *styles.last().unwrap());
            }
            Event::TaskListMarker(checked) => {
                append_text(
                    &mut runs,
                    if checked { "[x] " } else { "[ ] " },
                    *styles.last().unwrap(),
                );
            }
        }
    }

    runs
}

fn push_style(styles: &mut Vec<Style>, f: impl FnOnce(Style) -> Style) {
    let current = *styles.last().unwrap();
    styles.push(f(current));
}

fn pop_style(styles: &mut Vec<Style>) {
    if styles.len() > 1 {
        styles.pop();
    }
}

fn code_style(style: Style) -> Style {
    style.add_modifier(Modifier::REVERSED)
}

fn link_style(style: Style) -> Style {
    style.add_modifier(Modifier::UNDERLINED)
}

fn append_text(runs: &mut Vec<StyledRun>, text: &str, style: Style) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = runs.last_mut() {
        if last.style == style {
            last.text.push_str(text);
            return;
        }
    }
    runs.push(StyledRun {
        text: text.to_string(),
        style,
    });
}

fn append_text_with_quote_prefix(
    runs: &mut Vec<StyledRun>,
    text: &str,
    style: Style,
    quote_depth: usize,
) {
    for (idx, part) in text.split('\n').enumerate() {
        if idx > 0 {
            append_forced_newline(runs, style);
            if !part.is_empty() {
                ensure_quote_prefix(runs, quote_depth);
            }
        }
        append_text(runs, part, style);
    }
}

fn ensure_quote_prefix(runs: &mut Vec<StyledRun>, quote_depth: usize) {
    if quote_depth > 0 && is_at_line_start(runs) {
        append_text(runs, &"> ".repeat(quote_depth), theme::DIM);
    }
}

fn append_newline(runs: &mut Vec<StyledRun>, style: Style) {
    if !is_at_line_start(runs) {
        append_text(runs, "\n", style);
    }
}

fn append_forced_newline(runs: &mut Vec<StyledRun>, style: Style) {
    append_text(runs, "\n", style);
}

fn append_paragraph_break(runs: &mut Vec<StyledRun>, style: Style) {
    append_newline(runs, style);
    append_forced_newline(runs, style);
}

fn ensure_line_start(runs: &mut Vec<StyledRun>, style: Style) {
    if !is_at_line_start(runs) {
        append_newline(runs, style);
    }
}

fn is_at_line_start(runs: &[StyledRun]) -> bool {
    runs
        .last()
        .map(|r| r.text.ends_with('\n'))
        .unwrap_or(true)
}

fn visible_text_is_empty(runs: &[StyledRun]) -> bool {
    runs.iter().all(|r| r.text.trim().is_empty())
}

fn trim_trailing_newlines(runs: &mut Vec<StyledRun>) {
    while let Some(last) = runs.last_mut() {
        let trimmed = last.text.trim_end_matches('\n');
        if trimmed.len() == last.text.len() {
            break;
        }
        last.text.truncate(trimmed.len());
        if last.text.is_empty() {
            runs.pop();
        } else {
            break;
        }
    }
}

fn dot_prefixed_wrapped_lines(
    runs: &[StyledRun],
    wrap_width: usize,
    dot_style: Style,
) -> Vec<Line<'static>> {
    let body_width = wrap_width.saturating_sub(2).max(1);
    let logical_lines = split_runs_on_newlines(runs);
    let mut out = Vec::new();
    let mut first_row = true;

    for logical in logical_lines {
        if runs_visible_width(&logical) == 0 {
            if first_row {
                continue;
            }
            out.push(Line::default());
            continue;
        }

        for wrapped in wrap_runs_to_width(&logical, body_width) {
            if first_row {
                let mut spans = vec![Span::styled("● ", dot_style)];
                spans.extend(runs_to_spans(wrapped));
                out.push(Line::from(spans));
                first_row = false;
            } else {
                let mut spans = vec![Span::raw("  ")];
                spans.extend(runs_to_spans(wrapped));
                out.push(Line::from(spans));
            }
        }
    }

    out
}

fn dot_prefixed_wrapped_line_count(runs: &[StyledRun], wrap_width: usize) -> usize {
    let body_width = wrap_width.saturating_sub(2).max(1);
    let logical_lines = split_runs_on_newlines(runs);
    let mut count = 0usize;
    let mut first_row = true;

    for logical in logical_lines {
        if runs_visible_width(&logical) == 0 {
            if first_row {
                continue;
            }
            count += 1;
            continue;
        }

        let wrapped = wrap_runs_to_width(&logical, body_width);
        if !wrapped.is_empty() {
            first_row = false;
            count += wrapped.len();
        }
    }

    count
}

fn split_runs_on_newlines(runs: &[StyledRun]) -> Vec<Vec<StyledRun>> {
    let mut lines: Vec<Vec<StyledRun>> = vec![Vec::new()];
    for run in runs {
        for (idx, part) in run.text.split('\n').enumerate() {
            if idx > 0 {
                lines.push(Vec::new());
            }
            if !part.is_empty() {
                lines.last_mut().unwrap().push(StyledRun {
                    text: part.to_string(),
                    style: run.style,
                });
            }
        }
    }
    lines
}

fn runs_visible_width(runs: &[StyledRun]) -> usize {
    runs.iter().map(|r| UnicodeWidthStr::width(r.text.as_str())).sum()
}

fn wrap_runs_to_width(runs: &[StyledRun], width: usize) -> Vec<Vec<StyledRun>> {
    let chars = styled_chars(runs);
    if chars.is_empty() {
        return vec![Vec::new()];
    }

    let mut lines = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let mut row_width = 0usize;
        let mut cursor = start;
        let mut last_space = None;
        let mut seen_non_whitespace = false;
        while cursor < chars.len() {
            let ch_width = char_width(chars[cursor].ch);
            if row_width + ch_width > width && cursor > start {
                break;
            }
            row_width += ch_width;
            if chars[cursor].ch.is_whitespace() {
                if seen_non_whitespace && !is_code_style(chars[cursor].style) {
                    last_space = Some(cursor);
                }
            } else {
                seen_non_whitespace = true;
            }
            cursor += 1;
            if row_width >= width {
                break;
            }
        }

        let (end, next) = if cursor >= chars.len() {
            (chars.len(), chars.len())
        } else if let Some(space) = last_space {
            if space > start {
                (space, space + 1)
            } else {
                (cursor.max(start + 1), cursor.max(start + 1))
            }
        } else {
            (cursor.max(start + 1), cursor.max(start + 1))
        };

        let line = chars_to_runs(&chars[start..trim_trailing_space(&chars[start..end]) + start]);
        if !line.is_empty() {
            lines.push(line);
        }
        start = next;
    }

    if lines.is_empty() {
        vec![Vec::new()]
    } else {
        lines
    }
}

fn styled_chars(runs: &[StyledRun]) -> Vec<StyledChar> {
    runs.iter()
        .flat_map(|run| {
            run.text.chars().map(|ch| StyledChar {
                ch,
                style: run.style,
            })
        })
        .collect()
}

fn trim_trailing_space(chars: &[StyledChar]) -> usize {
    if chars.iter().all(|ch| ch.ch.is_whitespace()) {
        return chars.len();
    }

    let mut end = chars.len();
    while end > 0 && chars[end - 1].ch.is_whitespace() && !is_code_style(chars[end - 1].style) {
        end -= 1;
    }
    end
}

fn is_code_style(style: Style) -> bool {
    style.add_modifier.contains(Modifier::REVERSED)
}

fn chars_to_runs(chars: &[StyledChar]) -> Vec<StyledRun> {
    let mut runs: Vec<StyledRun> = Vec::new();
    for ch in chars {
        if let Some(last) = runs.last_mut() {
            if last.style == ch.style {
                last.text.push(ch.ch);
                continue;
            }
        }
        runs.push(StyledRun {
            text: ch.ch.to_string(),
            style: ch.style,
        });
    }
    runs
}

fn runs_to_spans(runs: Vec<StyledRun>) -> Vec<Span<'static>> {
    runs.into_iter()
        .map(|r| Span::styled(r.text, r.style))
        .collect()
}

pub(crate) fn line_display_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

fn char_width(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_text(text: &str, width: usize) -> Vec<Line<'static>> {
        render_agent_markdown_lines(text, width, theme::DOT_AGENT, theme::AGENT_TEXT)
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn bold_removes_markers_and_applies_style() {
        let lines = render_text("hello **bold**", 80);
        assert_eq!(line_text(&lines[0]), "● hello bold");
        assert!(lines[0].spans.iter().any(|s| {
            s.content.as_ref() == "bold" && s.style.add_modifier.contains(Modifier::BOLD)
        }));
    }

    #[test]
    fn italic_code_and_link_render_as_styled_visible_text() {
        let lines = render_text("*it* `code` [site](https://example.com)", 80);
        assert_eq!(line_text(&lines[0]), "● it code site");
        assert!(lines[0].spans.iter().any(|s| {
            s.content.as_ref() == "it" && s.style.add_modifier.contains(Modifier::ITALIC)
        }));
        assert!(lines[0].spans.iter().any(|s| {
            s.content.as_ref() == "code" && s.style.add_modifier.contains(Modifier::REVERSED)
        }));
        assert!(lines[0].spans.iter().any(|s| {
            s.content.as_ref() == "site" && s.style.add_modifier.contains(Modifier::UNDERLINED)
        }));
    }

    #[test]
    fn preserves_basic_block_markers_and_content() {
        let source = r#"# Title

- item
> quote
---
```text
code
```"#;
        let lines = render_text(source, 80);
        let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(text.contains("Title"));
        assert!(text.contains("- item"));
        assert!(text.contains("> quote"));
        assert!(text.contains("---"));
        assert!(text.contains("code"));
    }

    #[test]
    fn paragraph_break_preserves_blank_line() {
        let source = r#"first

second"#;
        let lines = render_text(source, 80);
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(
            texts.windows(3).any(|w| {
                w[0].contains("first") && w[1].is_empty() && w[2].contains("second")
            }),
            "separate paragraphs must keep a blank line: {texts:?}"
        );
    }

    #[test]
    fn list_items_stay_compact_without_extra_blank_lines() {
        let lines = render_text("- first\n- second", 80);
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        let first = texts.iter().position(|line| line.contains("- first")).unwrap();
        let second = texts.iter().position(|line| line.contains("- second")).unwrap();
        assert_eq!(
            second,
            first + 1,
            "adjacent list items should not get an inserted blank line: {texts:?}"
        );
    }

    #[test]
    fn code_block_preserves_leading_indentation() {
        let source = r#"```text
    indented
```"#;
        let lines = render_text(source, 80);
        let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(
            text.contains("    indented"),
            "code block indentation must be preserved: {text:?}"
        );
    }

    #[test]
    fn code_block_preserves_blank_lines() {
        let source = r#"```text
alpha

beta
```"#;
        let lines = render_text(source, 80);
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(
            texts.windows(3).any(|w| {
                w[0].contains("alpha") && w[1].is_empty() && w[2].contains("beta")
            }),
            "code block blank line must be preserved: {texts:?}"
        );
    }

    #[test]
    fn wrapped_code_block_preserves_leading_indentation() {
        let source = r#"```text
    indented
```"#;
        let lines = render_text(source, 12);
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(
            texts.iter().any(|line| line.contains("    ")),
            "wrapped code must keep leading spaces: {texts:?}"
        );
        for line in lines {
            assert!(
                line_display_width(&line) <= 12,
                "line exceeded width: {:?}",
                line_text(&line)
            );
        }
    }

    #[test]
    fn trim_trailing_space_preserves_whitespace_only_slices() {
        let spaces = vec![
            StyledChar { ch: ' ', style: theme::AGENT_TEXT },
            StyledChar { ch: ' ', style: theme::AGENT_TEXT },
            StyledChar { ch: ' ', style: theme::AGENT_TEXT },
            StyledChar { ch: ' ', style: theme::AGENT_TEXT },
        ];
        assert_eq!(
            trim_trailing_space(&spaces),
            spaces.len(),
            "pure indentation is content in code/list blocks and must not be dropped"
        );

        let mixed = vec![
            StyledChar { ch: 'a', style: theme::AGENT_TEXT },
            StyledChar { ch: ' ', style: theme::AGENT_TEXT },
            StyledChar { ch: ' ', style: theme::AGENT_TEXT },
        ];
        assert_eq!(
            trim_trailing_space(&mixed),
            1,
            "trailing separator whitespace after content should still be trimmed"
        );

        let code = vec![
            StyledChar { ch: 'a', style: code_style(theme::AGENT_TEXT) },
            StyledChar { ch: ' ', style: code_style(theme::AGENT_TEXT) },
            StyledChar { ch: ' ', style: code_style(theme::AGENT_TEXT) },
        ];
        assert_eq!(
            trim_trailing_space(&code),
            code.len(),
            "trailing spaces are code content and must not be trimmed"
        );
    }

    #[test]
    fn huge_ordered_list_start_does_not_overflow() {
        let lines = render_text("18446744073709551615. item", 80);
        let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(
            text.contains("18446744073709551615. item"),
            "huge ordered-list marker should render without overflow: {text:?}"
        );
    }

    #[test]
    fn nested_list_preserves_child_indentation() {
        let lines = render_text("- parent\n  - child", 80);
        let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(text.contains("- parent"));
        assert!(
            text.contains("    - child"),
            "nested child marker must remain indented: {text:?}"
        );
    }

    #[test]
    fn blockquote_prefix_repeats_for_child_blocks() {
        let lines = render_text("> first\n>\n> - item", 80);
        let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(text.contains("> first"));
        assert!(
            text.contains("> - item"),
            "quoted list item must keep quote marker: {text:?}"
        );
    }

    #[test]
    fn long_agent_reply_is_not_globally_truncated() {
        let long = format!("start {} end", "x ".repeat(2500));
        let lines = render_text(&long, 40);
        let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(text.contains("start"));
        assert!(text.contains("end"));
        assert!(
            !text.contains("chars omitted"),
            "markdown renderer must not replace the middle of long replies"
        );
    }

    #[test]
    fn block_styles_do_not_leak_to_following_text() {
        let source = r#"# Title
regular

`code` regular2"#;
        let lines = render_text(source, 80);
        let plain_span = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.content.as_ref().contains("regular"))
            .expect("plain text span exists");
        assert!(
            !plain_span.style.add_modifier.contains(Modifier::BOLD),
            "heading bold style must not leak into following paragraph"
        );
        let plain2_span = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.content.as_ref() == " regular2")
            .expect("post-code text span exists");
        assert!(
            !plain2_span.style.add_modifier.contains(Modifier::REVERSED),
            "inline code style must not leak into following text"
        );
    }

    #[test]
    fn html_and_task_markers_stay_visible() {
        let lines = render_text("<br>\n- [x] done", 80);
        let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(text.contains("<br>"));
        assert!(text.contains("[x] done"));
    }

    #[test]
    fn incomplete_markdown_remains_visible() {
        let lines = render_text("hello **unterminated", 80);
        let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(text.contains("hello"));
        assert!(text.contains("unterminated"));
    }

    #[test]
    fn hard_wraps_long_unbreakable_text_and_preserves_width() {
        let lines = render_text("`abcdefghijklmnopqrstuvwxyz`", 10);
        assert!(lines.len() > 1);
        for line in lines {
            assert!(
                line_display_width(&line) <= 10,
                "line exceeded width: {:?}",
                line_text(&line)
            );
        }
    }

    #[test]
    fn wrapped_inline_code_preserves_spaces() {
        let lines = render_text("`a  b`", 5);
        let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(
            text.contains("a  \n  b"),
            "inline code spaces must survive wrapping: {text:?}"
        );
        for line in lines {
            assert!(
                line_display_width(&line) <= 5,
                "line exceeded width: {:?}",
                line_text(&line)
            );
        }
    }

    #[test]
    fn wide_chars_do_not_exceed_width() {
        let lines = render_text("**你好你好🙂🙂**", 8);
        for line in lines {
            assert!(
                line_display_width(&line) <= 8,
                "line exceeded width: {:?}",
                line_text(&line)
            );
        }
    }

    #[test]
    fn height_matches_rendered_line_count() {
        let text = "hello **bold** and a very long token";
        let lines = render_text(text, 12);
        assert_eq!(
            agent_markdown_height(text, 12),
            lines.len(),
            "height must use the same wrapping as render"
        );
    }
}
