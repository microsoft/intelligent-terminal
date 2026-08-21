//! Markdown footnote rendering.
//!
//! References render inline as `[label]`. Definitions start with `[label]: ` and retain paragraph
//! boundaries without leaking their line style into following content.

use pulldown_cmark::{CowStr, Event};
use ratatui_core::text::{Line, Span};

use super::TextWriter;
use crate::StyleSheet;

impl<'a, 'theme, I, S> TextWriter<'a, 'theme, I, S>
where
    I: Iterator<Item = Event<'a>>,
    S: StyleSheet,
{
    pub fn footnote_reference(&mut self, label: CowStr<'a>) {
        // A reference can appear inside other inline formatting, as in `**Text[^label]**`.
        // Styling it with only `footnote_ref()` would make `[label]` dim and italic but drop the
        // surrounding bold style. Start with the active inline style and patch the footnote style
        // over it so the reference adds its own appearance without losing enclosing formatting.
        let inline_style = self.inline_styles.last().copied().unwrap_or_default();
        let style = inline_style.patch(self.styles.footnote_ref());
        self.push_span(Span::styled(format!("[{label}]"), style));
    }

    pub fn start_footnote_definition(&mut self, label: CowStr<'a>) {
        if self.needs_newline {
            self.push_line(Line::default());
        }
        let style = self.styles.footnote_def();
        self.line_styles.push(style);
        self.push_line(Line::default());
        self.push_span(Span::styled(format!("[{label}]: "), style));
        self.in_footnote_definition = true;
        self.needs_newline = false;
    }

    pub fn end_footnote_definition(&mut self) {
        self.line_styles.pop();
        self.in_footnote_definition = false;
        self.needs_newline = true;
    }
}

#[cfg(test)]
mod tests {
    use indoc::indoc;
    use rstest::rstest;

    use super::*;
    use crate::renderer::test_support::{with_tracing, DefaultGuard};
    use crate::renderer::*;

    mod footnotes {
        use pretty_assertions::assert_eq;

        use super::*;

        #[rstest]
        fn multiline_definition_has_exact_layout(_with_tracing: DefaultGuard) {
            let markdown = indoc! {"
                Text[^one]

                [^one]: First line
                    continued line.
            "};
            let reference_style = Style::new().dim().italic();
            let definition_style = Style::new().dim();

            assert_eq!(
                from_str(markdown),
                Text::from_iter([
                    Line::from_iter([Span::raw("Text"), Span::styled("[one]", reference_style),]),
                    Line::default(),
                    Line::from_iter([
                        Span::styled("[one]: ", definition_style),
                        Span::raw("First line"),
                        Span::raw(" "),
                        Span::raw("continued line."),
                    ])
                    .style(definition_style),
                ])
            );
        }

        #[rstest]
        fn multiple_definitions_have_exact_layout(_with_tracing: DefaultGuard) {
            let markdown = indoc! {"
                First[^a] second[^b].

                [^a]: Alpha.

                [^b]: Beta.
            "};
            let reference_style = Style::new().dim().italic();
            let definition_style = Style::new().dim();

            assert_eq!(
                from_str(markdown),
                Text::from_iter([
                    Line::from_iter([
                        Span::raw("First"),
                        Span::styled("[a]", reference_style),
                        Span::raw(" second"),
                        Span::styled("[b]", reference_style),
                        Span::raw("."),
                    ]),
                    Line::default(),
                    Line::from_iter(
                        [Span::styled("[a]: ", definition_style), Span::raw("Alpha."),]
                    )
                    .style(definition_style),
                    Line::default(),
                    Line::from_iter([Span::styled("[b]: ", definition_style), Span::raw("Beta."),])
                        .style(definition_style),
                ])
            );
        }

        #[rstest]
        fn reference_combines_with_enclosing_style(_with_tracing: DefaultGuard) {
            let markdown = indoc! {"
                **Text[^one]**

                [^one]: Note.
            "};
            let reference_style = Style::new().bold().dim().italic();

            assert_eq!(
                from_str(markdown),
                Text::from_iter([
                    Line::from_iter([
                        Span::styled("Text", Style::new().bold()),
                        Span::styled("[one]", reference_style),
                    ]),
                    Line::default(),
                    Line::from_iter([
                        Span::styled("[one]: ", Style::new().dim()),
                        Span::raw("Note."),
                    ])
                    .style(Style::new().dim()),
                ])
            );
        }

        #[rstest]
        fn multiple_definition_paragraphs_keep_blank_line(_with_tracing: DefaultGuard) {
            let markdown = indoc! {"
                Text[^one]

                [^one]: First paragraph.

                    Second paragraph.
            "};
            let definition_style = Style::new().dim();

            assert_eq!(
                from_str(markdown),
                Text::from_iter([
                    Line::from_iter([
                        Span::raw("Text"),
                        Span::styled("[one]", definition_style.italic()),
                    ]),
                    Line::default(),
                    Line::from_iter([
                        Span::styled("[one]: ", definition_style),
                        Span::raw("First paragraph."),
                    ])
                    .style(definition_style),
                    Line::default().style(definition_style),
                    Line::from("Second paragraph.").style(definition_style),
                ])
            );
        }

        #[rstest]
        fn definition_style_does_not_leak_into_following_paragraph(_with_tracing: DefaultGuard) {
            let markdown = indoc! {"
                Text[^one]

                [^one]: First paragraph.

                    Second paragraph.

                After.
            "};
            let definition_style = Style::new().dim();

            assert_eq!(
                from_str(markdown),
                Text::from_iter([
                    Line::from_iter([
                        Span::raw("Text"),
                        Span::styled("[one]", definition_style.italic()),
                    ]),
                    Line::default(),
                    Line::from_iter([
                        Span::styled("[one]: ", definition_style),
                        Span::raw("First paragraph."),
                    ])
                    .style(definition_style),
                    Line::default().style(definition_style),
                    Line::from("Second paragraph.").style(definition_style),
                    Line::default(),
                    Line::from("After."),
                ])
            );
        }

        #[rstest]
        fn custom_styles_compose_with_enclosing_formatting(_with_tracing: DefaultGuard) {
            #[derive(Clone, Copy)]
            struct CustomFootnoteStyle;

            impl StyleSheet for CustomFootnoteStyle {
                fn footnote_ref(&self) -> Style {
                    Style::new().red().underlined()
                }

                fn footnote_def(&self) -> Style {
                    Style::new().blue().underlined()
                }
            }

            let markdown = indoc! {"
                **Text[^one]**

                [^one]: Note.
            "};
            let options = Options::new(CustomFootnoteStyle);
            let reference_style = Style::new().red().bold().underlined();
            let definition_style = Style::new().blue().underlined();

            assert_eq!(
                from_str_with_options(markdown, &options),
                Text::from_iter([
                    Line::from_iter([
                        Span::styled("Text", Style::new().bold()),
                        Span::styled("[one]", reference_style),
                    ]),
                    Line::default(),
                    Line::from_iter([
                        Span::styled("[one]: ", definition_style),
                        Span::raw("Note."),
                    ])
                    .style(definition_style),
                ])
            );
        }
    }
}
