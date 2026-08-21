//! Markdown list and task-item rendering.

use pulldown_cmark::Event;
use ratatui_core::style::Stylize;
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
                    format!("{:width$}. ", *index - 1).light_blue()
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
            line.spans.insert(1, marker_span);
        } else {
            self.push_span(marker_span);
        }
    }
}

#[cfg(test)]
mod tests {
    use indoc::indoc;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    use super::*;
    use crate::renderer::test_support::{with_tracing, DefaultGuard};
    use crate::renderer::*;

    #[rstest]
    fn list_single(_with_tracing: DefaultGuard) {
        assert_eq!(
            from_str(indoc! {"
                - List item 1
            "}),
            Text::from_iter([Line::from_iter(["- ", "List item 1"])])
        );
    }

    #[rstest]
    fn list_multiple(_with_tracing: DefaultGuard) {
        assert_eq!(
            from_str(indoc! {"
                - List item 1
                - List item 2
            "}),
            Text::from_iter([
                Line::from_iter(["- ", "List item 1"]),
                Line::from_iter(["- ", "List item 2"]),
            ])
        );
    }

    #[rstest]
    fn list_ordered(_with_tracing: DefaultGuard) {
        assert_eq!(
            from_str(indoc! {"
                1. List item 1
                2. List item 2
            "}),
            Text::from_iter([
                Line::from_iter(["1. ".light_blue(), "List item 1".into()]),
                Line::from_iter(["2. ".light_blue(), "List item 2".into()]),
            ])
        );
    }

    #[rstest]
    fn styled_list_items_keep_content_on_marker_line(_with_tracing: DefaultGuard) {
        let markdown = indoc! {"
            - *Emphasis* and **strong**
            - Before **strong *emphasis*** after

            1. **Strong**
            2. Before *emphasis* after
        "};

        assert_eq!(
            from_str(markdown),
            Text::from_iter([
                Line::from_iter([
                    Span::raw("- "),
                    Span::raw("Emphasis").italic(),
                    Span::raw(" and "),
                    Span::raw("strong").bold(),
                ]),
                Line::from_iter([
                    Span::raw("- "),
                    Span::raw("Before "),
                    Span::raw("strong ").bold(),
                    Span::raw("emphasis").bold().italic(),
                    Span::raw(" after"),
                ]),
                Line::default(),
                Line::from_iter([Span::raw("1. ").light_blue(), Span::raw("Strong").bold()]),
                Line::from_iter([
                    Span::raw("2. ").light_blue(),
                    Span::raw("Before "),
                    Span::raw("emphasis").italic(),
                    Span::raw(" after"),
                ]),
            ])
        );
    }

    #[rstest]
    fn loose_styled_list_items_keep_first_paragraph_on_marker_line(_with_tracing: DefaultGuard) {
        let markdown = indoc! {"
            - *Emphasized first item.*

            - **Strong second item.**

            1. **Strong first item.**

            2. *Emphasized second item.*
        "};

        // Regression: loose lists emit paragraph boundaries after their item markers.
        assert_eq!(
            from_str(markdown),
            Text::from_iter([
                Line::from_iter([
                    Span::raw("- "),
                    Span::raw("Emphasized first item.").italic(),
                ]),
                Line::from_iter([Span::raw("- "), Span::raw("Strong second item.").bold(),]),
                Line::default(),
                Line::from_iter([
                    Span::raw("1. ").light_blue(),
                    Span::raw("Strong first item.").bold(),
                ]),
                Line::from_iter([
                    Span::raw("2. ").light_blue(),
                    Span::raw("Emphasized second item.").italic(),
                ]),
            ])
        );
    }

    #[rstest]
    fn later_styled_paragraph_in_list_stays_separate(_with_tracing: DefaultGuard) {
        let markdown = indoc! {"
            - **First paragraph.**

              *Second paragraph.*
        "};

        assert_eq!(
            from_str(markdown),
            Text::from_iter([
                Line::from_iter([Span::raw("- "), Span::raw("First paragraph.").bold(),]),
                Line::default(),
                Line::from(Span::raw("Second paragraph.").italic()),
            ])
        );
    }

    #[rstest]
    fn ordered_list_respects_start_index(_with_tracing: DefaultGuard) {
        let markdown = indoc! {"
            10. Tenth
            11. Eleventh
        "};

        assert_eq!(
            from_str(markdown),
            Text::from_iter([
                Line::from_iter(["10. ".light_blue(), "Tenth".into()]),
                Line::from_iter(["11. ".light_blue(), "Eleventh".into()]),
            ])
        );
    }

    #[rstest]
    fn list_nested(_with_tracing: DefaultGuard) {
        assert_eq!(
            from_str(indoc! {"
                - List item 1
                  - Nested list item 1
            "}),
            Text::from_iter([
                Line::from_iter(["- ", "List item 1"]),
                Line::from_iter(["    - ", "Nested list item 1"]),
            ])
        );
    }

    #[rstest]
    fn list_task_items(_with_tracing: DefaultGuard) {
        assert_eq!(
            from_str(indoc! {"
                - [ ] Incomplete
                - [x] Complete
            "}),
            Text::from_iter([
                Line::from_iter(["- [ ] ", "Incomplete"]),
                Line::from_iter(["- [x] ", "Complete"]),
            ])
        );
    }

    #[rstest]
    fn list_task_items_ordered(_with_tracing: DefaultGuard) {
        assert_eq!(
            from_str(indoc! {"
                1. [ ] Incomplete
                2. [x] Complete
            "}),
            Text::from_iter([
                Line::from_iter(["1. ".light_blue(), "[ ] ".into(), "Incomplete".into(),]),
                Line::from_iter(["2. ".light_blue(), "[x] ".into(), "Complete".into(),]),
            ])
        );
    }

    #[rstest]
    fn list_does_not_indent_following_paragraph(_with_tracing: DefaultGuard) {
        let markdown = indoc! {"
            - Item

            After
        "};

        assert_eq!(
            from_str(markdown),
            Text::from_iter([
                Line::from_iter(["- ", "Item"]),
                Line::default(),
                Line::from("After"),
            ])
        );
    }
}
