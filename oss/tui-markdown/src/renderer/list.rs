//! Markdown list and task-item rendering.

use pulldown_cmark::Event;
use ratatui_core::text::{Line, Span};

use super::TextWriter;
use crate::StyleSheet;

/// Records how an active list item occupies the rendered output.
///
/// List markers are written as soon as pulldown-cmark emits an item start, but tables are buffered
/// until the matching table end. Remembering the marker's location lets the completed table attach
/// to that already-written marker instead of appearing as a separate, unindented block. These
/// values form a stack because an outer item remains active while a nested item is parsed.
#[derive(Clone, Copy, Debug)]
pub struct ListItemLayout {
    /// Output line containing the list marker.
    pub marker_line: usize,
    /// Number of spans on `marker_line` immediately after writing the marker.
    ///
    /// Comparing this with the eventual span count distinguishes a table that is the item's first
    /// content from a table following text on the same line.
    pub marker_span_count: usize,
    /// Display width reserved by the complete marker, including nesting indentation.
    ///
    /// This uses terminal display width rather than bytes so continuation lines align after
    /// unordered markers and multi-digit ordered markers alike.
    pub continuation_width: usize,
}

impl<'a, 'theme, I, S> TextWriter<'a, 'theme, I, S>
where
    I: Iterator<Item = Event<'a>>,
    S: StyleSheet,
{
    pub fn start_list(&mut self, index: Option<u64>) {
        if self.list_indices.is_empty() && self.needs_newline {
            self.push_line(Line::default());
        }
        self.list_indices.push(index);
    }

    pub fn end_list(&mut self) {
        self.list_indices.pop();
        self.needs_newline = true;
    }

    pub fn start_item(&mut self) {
        let marker_line = self.text.lines.len();
        self.push_line(Line::default());
        let width = self.list_indices.len() * 4 - 3;
        if let Some(last_index) = self.list_indices.last_mut() {
            let span = match last_index {
                None => Span::from(" ".repeat(width - 1) + "- "),
                Some(index) => {
                    *index += 1;
                    Span::styled(
                        format!("{:width$}. ", *index - 1),
                        self.styles.list_marker(),
                    )
                }
            };
            let continuation_width = span.width();
            self.push_span(span);
            let marker_span_count = self.text.lines[marker_line].spans.len();
            self.list_items.push(ListItemLayout {
                marker_line,
                marker_span_count,
                continuation_width,
            });
        }
        self.needs_newline = false;
    }

    pub fn end_item(&mut self) {
        self.list_items.pop();
    }

    pub fn task_list_marker(&mut self, checked: bool) {
        let marker = if checked { 'x' } else { ' ' };
        let marker_span = Span::from(format!("[{marker}] "));
        if let Some(line) = self.text.lines.last_mut() {
            if let Some(first_span) = line.spans.first_mut() {
                let content = first_span.content.to_mut();
                if content.ends_with("- ") {
                    let len = content.len();
                    content.truncate(len - 2);
                    content.push_str("- [");
                    content.push(marker);
                    content.push_str("] ");
                    return;
                }
            }
            // A hidden heading marker can leave the line empty before its task text arrives.
            let index = line.spans.len().min(1);
            line.spans.insert(index, marker_span);
        } else {
            self.push_span(marker_span);
        }
    }
}

#[cfg(test)]
mod tests {
    use indoc::indoc;
    use ratatui_core::style::{Color, Modifier};
    use rstest::rstest;

    use crate::renderer::test_support::{with_tracing, DefaultGuard};
    use crate::renderer::*;

    #[rstest]
    fn list_single(_with_tracing: DefaultGuard) {
        insta::assert_debug_snapshot!(from_str(indoc!("
            - List item 1
        ")), @r#"Text::from(Line::from_iter([Span::from("- "), Span::from("List item 1")]))"#);
    }

    #[rstest]
    fn list_multiple(_with_tracing: DefaultGuard) {
        insta::assert_debug_snapshot!(from_str(indoc!("
            - List item 1
            - List item 2
        ")), @r#"
        Text::from_iter([
            Line::from_iter([
                Span::from("- "),
                Span::from("List item 1"),
            ]),
            Line::from_iter([
                Span::from("- "),
                Span::from("List item 2"),
            ]),
        ])
        "#);
    }

    #[rstest]
    fn list_ordered(_with_tracing: DefaultGuard) {
        insta::assert_debug_snapshot!(from_str(indoc!("
            1. List item 1
            2. List item 2
        ")), @r#"
        Text::from_iter([
            Line::from_iter([
                Span::from("1. ").light_blue(),
                Span::from("List item 1"),
            ]),
            Line::from_iter([
                Span::from("2. ").light_blue(),
                Span::from("List item 2"),
            ]),
        ])
        "#);
    }

    #[rstest]
    fn styled_list_items_keep_content_on_marker_line(_with_tracing: DefaultGuard) {
        insta::assert_debug_snapshot!(
            "styled_list_items_keep_content_on_marker_line",
            from_str(indoc!(
                "
                - *Emphasis* and **strong**
                - Before **strong *emphasis*** after

                1. **Strong**
                2. Before *emphasis* after
            "
            ))
        );
    }

    #[rstest]
    fn loose_styled_list_items_keep_first_paragraph_on_marker_line(_with_tracing: DefaultGuard) {
        // Regression: loose lists emit paragraph boundaries after their item markers.
        insta::assert_debug_snapshot!(
            "loose_styled_list_items_keep_first_paragraph_on_marker_line",
            from_str(indoc!(
                "
                - *Emphasized first item.*

                - **Strong second item.**

                1. **Strong first item.**

                2. *Emphasized second item.*
            "
            ))
        );
    }

    #[rstest]
    fn later_styled_paragraph_in_list_stays_separate(_with_tracing: DefaultGuard) {
        insta::assert_debug_snapshot!(from_str(indoc!("
            - **First paragraph.**

              *Second paragraph.*
        ")), @r#"
        Text::from_iter([
            Line::from_iter([
                Span::from("- "),
                Span::from("First paragraph.").bold(),
            ]),
            Line::default(),
            Line::from(Span::from("Second paragraph.").italic()),
        ])
        "#);
    }

    #[rstest]
    fn ordered_list_respects_start_index(_with_tracing: DefaultGuard) {
        insta::assert_debug_snapshot!(from_str(indoc!("
            10. Tenth
            11. Eleventh
        ")), @r#"
        Text::from_iter([
            Line::from_iter([
                Span::from("10. ").light_blue(),
                Span::from("Tenth"),
            ]),
            Line::from_iter([
                Span::from("11. ").light_blue(),
                Span::from("Eleventh"),
            ]),
        ])
        "#);
    }

    #[rstest]
    fn list_nested(_with_tracing: DefaultGuard) {
        insta::assert_debug_snapshot!(from_str(indoc!("
            - List item 1
              - Nested list item 1
        ")), @r#"
        Text::from_iter([
            Line::from_iter([
                Span::from("- "),
                Span::from("List item 1"),
            ]),
            Line::from_iter([
                Span::from("    - "),
                Span::from("Nested list item 1"),
            ]),
        ])
        "#);
    }

    #[rstest]
    fn list_task_items(_with_tracing: DefaultGuard) {
        insta::assert_debug_snapshot!(from_str(indoc!("
            - [ ] Incomplete
            - [x] Complete
        ")), @r#"
        Text::from_iter([
            Line::from_iter([
                Span::from("- [ ] "),
                Span::from("Incomplete"),
            ]),
            Line::from_iter([
                Span::from("- [x] "),
                Span::from("Complete"),
            ]),
        ])
        "#);
    }

    #[rstest]
    fn list_task_items_ordered(_with_tracing: DefaultGuard) {
        insta::assert_debug_snapshot!(from_str(indoc!("
            1. [ ] Incomplete
            2. [x] Complete
        ")), @r#"
        Text::from_iter([
            Line::from_iter([
                Span::from("1. ").light_blue(),
                Span::from("[ ] "),
                Span::from("Incomplete"),
            ]),
            Line::from_iter([
                Span::from("2. ").light_blue(),
                Span::from("[x] "),
                Span::from("Complete"),
            ]),
        ])
        "#);
    }

    #[rstest]
    fn list_does_not_indent_following_paragraph(_with_tracing: DefaultGuard) {
        insta::assert_debug_snapshot!(from_str(indoc!("
            - Item

            After
        ")), @r#"
        Text::from_iter([
            Line::from_iter([
                Span::from("- "),
                Span::from("Item"),
            ]),
            Line::default(),
            Line::from("After"),
        ])
        "#);
    }

    #[rstest]
    fn ordered_list_marker_uses_style_sheet(_with_tracing: DefaultGuard) {
        #[derive(Clone, Copy)]
        struct CustomListMarkerStyle;

        impl StyleSheet for CustomListMarkerStyle {
            fn list_marker(&self) -> Style {
                Style::new().magenta().bold()
            }
        }

        let options = Options::new(CustomListMarkerStyle);
        insta::assert_debug_snapshot!(from_str_with_options(indoc!("
            1. List item 1
            2. List item 2
        "), &options), @r#"
        Text::from_iter([
            Line::from_iter([
                Span::from("1. ").magenta().bold(),
                Span::from("List item 1"),
            ]),
            Line::from_iter([
                Span::from("2. ").magenta().bold(),
                Span::from("List item 2"),
            ]),
        ])
        "#);
    }

    #[rstest]
    fn unordered_list_marker_is_not_affected_by_style_sheet(_with_tracing: DefaultGuard) {
        #[derive(Clone, Copy)]
        struct CustomListMarkerStyle;

        impl StyleSheet for CustomListMarkerStyle {
            fn list_marker(&self) -> Style {
                Style::new().magenta().bold()
            }
        }

        let options = Options::new(CustomListMarkerStyle);

        insta::assert_debug_snapshot!(
            from_str_with_options(indoc!("
                - List item 1
            "), &options),
            @r#"Text::from(Line::from_iter([Span::from("- "), Span::from("List item 1")]))"#
        );
    }

    #[derive(Clone, Copy)]
    struct HeadingStyles {
        hidden: bool,
    }

    impl StyleSheet for HeadingStyles {
        fn heading_marker(&self, level: u8) -> &str {
            if self.hidden {
                ""
            } else if level == 1 {
                "#"
            } else {
                "##"
            }
        }

        fn heading(&self, _: u8) -> Style {
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        }
    }

    #[rstest]
    #[case::unordered("-", "  ")]
    #[case::ordered("1.", "   ")]
    #[case::multi_digit_ordered("10.", "    ")]
    fn task_list_setext_heading_keeps_checkbox_text_and_style(
        #[case] list_marker: &str,
        #[case] indent: &str,
        #[values("[ ]", "[x]")] checkbox: &str,
        #[values("---", "===")] underline: &str,
        #[values(false, true)] hidden: bool,
    ) {
        let source = format!("{list_marker} {checkbox} task\n{indent}{underline}");
        let styles = HeadingStyles { hidden };
        let level = if underline == "===" { 1 } else { 2 };
        let marker = styles.heading_marker(level);
        let prefix = if hidden {
            String::new()
        } else {
            format!("{marker} ")
        };
        let text = from_str_with_options(&source, &Options::new(styles));

        assert_eq!(
            text.to_string(),
            format!("{list_marker} \n{prefix}{checkbox} task")
        );
        assert_eq!(text.lines[1].style, styles.heading(level));
    }

    #[rstest]
    #[case::unchecked("- [ ] task\n  ---", "- \n## [ ] task")]
    #[case::checked("- [x] task\n  ===", "- \n# [x] task")]
    fn default_heading_markers_keep_existing_task_output(
        #[case] source: &str,
        #[case] expected: &str,
    ) {
        assert_eq!(from_str(source).to_string(), expected);
    }

    #[rstest]
    fn ordinary_tasks_keep_markers_and_nested_indentation(#[values(false, true)] hidden: bool) {
        let options = Options::new(HeadingStyles { hidden });
        assert_eq!(
            from_str_with_options("- [ ] first\n  - [x] nested\n- [x] last", &options).to_string(),
            "- [ ] first\n    - [x] nested\n- [x] last"
        );
        assert_eq!(
            from_str_with_options("1. [ ] first\n2. [x] last", &options).to_string(),
            "1. [ ] first\n2. [x] last"
        );
    }

    #[test]
    fn nested_task_heading_can_hide_its_marker() {
        let options = Options::new(HeadingStyles { hidden: true });
        let source = "- outer\n  - [x] task\n    ---";
        assert_eq!(
            from_str_with_options(source, &options).to_string(),
            "- outer\n    - \n[x] task"
        );
    }
}
