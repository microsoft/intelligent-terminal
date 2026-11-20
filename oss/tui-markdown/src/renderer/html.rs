//! Raw Markdown HTML rendering.
//!
//! HTML remains visible as literal text. Inline tags compose with enclosing formatting, while HTML
//! blocks preserve their physical lines and surrounding block spacing.

use pulldown_cmark::{CowStr, Event};
use ratatui_core::text::{Line, Span};

use super::TextWriter;
use crate::StyleSheet;

impl<'a, 'theme, I, S> TextWriter<'a, 'theme, I, S>
where
    I: Iterator<Item = Event<'a>>,
    S: StyleSheet,
{
    pub fn start_html_block(&mut self) {
        if self.needs_newline {
            self.push_line(Line::default());
        }
        self.push_line(Line::default());
        self.line_styles.push(self.styles.html());
        self.needs_newline = false;
    }

    pub fn end_html_block(&mut self) {
        self.line_styles.pop();
        self.needs_newline = true;
    }

    pub fn html_block(&mut self, html: CowStr<'a>) {
        let style = self.styles.html();
        for part in html.split_inclusive('\n') {
            if self.needs_newline {
                self.push_line(Line::default());
                self.needs_newline = false;
            }
            let content = part.strip_suffix('\n').unwrap_or(part);
            let content = content.strip_suffix('\r').unwrap_or(content);
            // A newline-only event can finish content from the preceding parser event.
            if !content.is_empty()
                || self
                    .text
                    .lines
                    .last()
                    .is_none_or(|line| line.spans.is_empty())
            {
                self.push_span(Span::styled(content.to_owned(), style));
            }
            self.needs_newline = part.ends_with('\n');
        }
    }

    pub fn inline_html(&mut self, html: CowStr<'a>) {
        let inline_style = self.inline_styles.last().copied().unwrap_or_default();
        let style = inline_style.patch(self.styles.html());
        self.push_span(Span::styled(html, style));
    }
}

#[cfg(test)]
mod tests {
    use indoc::indoc;
    use ratatui_core::style::Stylize;
    use ratatui_core::text::{Line, Span, Text};
    use rstest::rstest;

    use crate::from_str;
    use crate::renderer::test_support::{with_tracing, DefaultGuard};

    mod html {
        use super::*;
        use pretty_assertions::assert_eq;

        #[rstest]
        fn inline_html_tag(_with_tracing: DefaultGuard) {
            assert_eq!(
                from_str("Hello <em>world</em>"),
                Text::from(Line::from_iter([
                    Span::from("Hello "),
                    Span::from("<em>").dim(),
                    Span::from("world"),
                    Span::from("</em>").dim()
                ]))
            );
        }

        #[rstest]
        fn inline_html_combines_with_emphasis(_with_tracing: DefaultGuard) {
            assert_eq!(
                from_str("*Hello <em>world</em>*"),
                Text::from(Line::from_iter([
                    Span::from("Hello ").italic(),
                    Span::from("<em>").dim().italic(),
                    Span::from("world").italic(),
                    Span::from("</em>").dim().italic()
                ]))
            );
        }

        #[rstest]
        fn html_block_preserves_paragraph_spacing(_with_tracing: DefaultGuard) {
            insta::assert_debug_snapshot!(from_str(indoc!("
                Before

                <div>
                Custom HTML
                </div>

                After
            ")), @r#"
            Text::from_iter([
                Line::from("Before"),
                Line::default(),
                Line::from(Span::from("<div>").dim()),
                Line::from(Span::from("Custom HTML").dim()).dim(),
                Line::from(Span::from("</div>").dim()).dim(),
                Line::default(),
                Line::from("After"),
            ])
            "#);
        }
    }

    #[rstest]
    fn html_block_preserves_literal_lines(#[values("\n", "\r\n")] newline: &str) {
        let lf = "Before\n\n<pre>\nfirst\n\nlast\n</pre>\n\nAfter";
        let source = lf.replace('\n', newline);
        let text = from_str(&source);
        assert_eq!(text.to_string(), lf);
        assert_eq!(text, from_str(lf));
    }

    #[rstest]
    fn quoted_html_preserves_prefixes_and_blank_lines(#[values("\n", "\r\n")] newline: &str) {
        let markdown = indoc! {"
            > <pre>
            > first
            >
            > last
            > </pre>

            After"};
        let source = markdown.replace('\n', newline);

        let text = from_str(&source);

        assert_eq!(
            text.to_string(),
            indoc! {"
            > <pre>
            > first
            > 
            > last
            > </pre>

            After"}
        );
        assert_eq!(text, from_str(markdown));
    }

    #[rstest]
    fn list_html_preserves_blank_lines(#[values("\n", "\r\n")] newline: &str) {
        let markdown = indoc! {"
            - Before

              <pre>
              first

              last
              </pre>

            After"};
        let source = markdown.replace('\n', newline);

        let text = from_str(&source);

        assert_eq!(
            text.to_string(),
            indoc! {"
            - Before

            <pre>
            first

            last
            </pre>

            After"}
        );
        assert_eq!(text, from_str(markdown));
    }
}
