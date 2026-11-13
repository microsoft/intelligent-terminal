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
    use ratatui_core::style::Style;
    use rstest::rstest;

    use super::*;
    use crate::renderer::test_support::{with_tracing, DefaultGuard};
    use crate::{from_str, from_str_with_options, Options};

    mod footnotes {
        use super::*;

        #[rstest]
        fn multiline_definition_has_exact_layout(_with_tracing: DefaultGuard) {
            insta::assert_debug_snapshot!(
                "multiline_definition_has_exact_layout",
                from_str(indoc!(
                    "
                    Text[^one]

                    [^one]: First line
                        continued line.
                "
                ))
            );
        }

        #[rstest]
        fn multiple_definitions_have_exact_layout(_with_tracing: DefaultGuard) {
            insta::assert_debug_snapshot!(
                "multiple_definitions_have_exact_layout",
                from_str(indoc!(
                    "
                    First[^a] second[^b].

                    [^a]: Alpha.

                    [^b]: Beta.
                "
                ))
            );
        }

        #[rstest]
        fn reference_combines_with_enclosing_style(_with_tracing: DefaultGuard) {
            insta::assert_debug_snapshot!(from_str(indoc!("
                **Text[^one]**

                [^one]: Note.
            ")), @r#"
            Text::from_iter([
                Line::from_iter([
                    Span::from("Text").bold(),
                    Span::from("[one]").bold().dim().italic(),
                ]),
                Line::default(),
                Line::from_iter([
                    Span::from("[one]: ").dim(),
                    Span::from("Note."),
                ]).dim(),
            ])
            "#);
        }

        #[rstest]
        fn multiple_definition_paragraphs_keep_blank_line(_with_tracing: DefaultGuard) {
            insta::assert_debug_snapshot!(
                "multiple_definition_paragraphs_keep_blank_line",
                from_str(indoc!(
                    "
                    Text[^one]

                    [^one]: First paragraph.

                        Second paragraph.
                "
                ))
            );
        }

        #[rstest]
        fn definition_style_does_not_leak_into_following_paragraph(_with_tracing: DefaultGuard) {
            insta::assert_debug_snapshot!(
                "definition_style_does_not_leak_into_following_paragraph",
                from_str(indoc!(
                    "
                    Text[^one]

                    [^one]: First paragraph.

                        Second paragraph.

                    After.
                "
                ))
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

            let options = Options::new(CustomFootnoteStyle);

            insta::assert_debug_snapshot!(from_str_with_options(indoc!("
                **Text[^one]**

                [^one]: Note.
            "), &options), @r#"
            Text::from_iter([
                Line::from_iter([
                    Span::from("Text").bold(),
                    Span::from("[one]").red().bold().underlined(),
                ]),
                Line::default(),
                Line::from_iter([
                    Span::from("[one]: ").blue().underlined(),
                    Span::from("Note."),
                ]).blue().underlined(),
            ])
            "#);
        }
    }
}
