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
        for line in html.lines() {
            if self.needs_newline {
                self.push_line(Line::default());
                self.needs_newline = false;
            }
            self.push_span(Span::styled(line.to_owned(), style));
            self.needs_newline = true;
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
    use rstest::rstest;

    use super::*;
    use crate::renderer::test_support::{with_tracing, DefaultGuard};
    use crate::renderer::*;

    mod html {
        use pretty_assertions::assert_eq;

        use super::*;

        #[rstest]
        fn inline_html_tag(_with_tracing: DefaultGuard) {
            assert_eq!(
                from_str("Hello <em>world</em>"),
                Text::from(Line::from_iter([
                    Span::from("Hello "),
                    Span::styled("<em>", Style::new().dim()),
                    Span::from("world"),
                    Span::styled("</em>", Style::new().dim()),
                ]))
            );
        }

        #[rstest]
        fn inline_html_combines_with_emphasis(_with_tracing: DefaultGuard) {
            let italic = Style::new().italic();
            let html = italic.dim();
            assert_eq!(
                from_str("*Hello <em>world</em>*"),
                Text::from(Line::from_iter([
                    Span::styled("Hello ", italic),
                    Span::styled("<em>", html),
                    Span::styled("world", italic),
                    Span::styled("</em>", html),
                ]))
            );
        }

        #[rstest]
        fn html_block_preserves_paragraph_spacing(_with_tracing: DefaultGuard) {
            let markdown = indoc! {"
                Before

                <div>
                Custom HTML
                </div>

                After
            "};

            assert_eq!(
                from_str(markdown),
                Text::from_iter([
                    Line::from("Before"),
                    Line::default(),
                    Line::from(Span::styled("<div>", Style::new().dim())),
                    Line::from(Span::styled("Custom HTML", Style::new().dim()))
                        .style(Style::new().dim()),
                    Line::from(Span::styled("</div>", Style::new().dim()))
                        .style(Style::new().dim()),
                    Line::default(),
                    Line::from("After"),
                ])
            );
        }
    }
}
