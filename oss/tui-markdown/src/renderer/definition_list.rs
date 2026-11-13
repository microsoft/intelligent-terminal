//! Markdown definition-list rendering.
//!
//! Terms and descriptions have separate styles. Each description starts with `: `, and paragraph
//! handling preserves blank lines inside multi-paragraph descriptions.

use pulldown_cmark::Event;
use ratatui_core::text::{Line, Span};

use super::TextWriter;
use crate::StyleSheet;

impl<'a, 'theme, I, S> TextWriter<'a, 'theme, I, S>
where
    I: Iterator<Item = Event<'a>>,
    S: StyleSheet,
{
    pub fn start_definition_list(&mut self) {
        if self.needs_newline {
            self.push_line(Line::default());
        }
        self.needs_newline = false;
    }

    pub fn end_definition_list(&mut self) {
        self.needs_newline = true;
    }

    pub fn start_definition_title(&mut self) {
        // Definition-list terms contain inline events without an ordinary paragraph start, so the
        // term handler owns the output line and applies the term style before consuming them.
        self.push_line(Line::default());
        self.push_inline_style(self.styles.definition_term());
        self.needs_newline = false;
    }

    pub fn end_definition_title(&mut self) {
        self.pop_inline_style();
        self.needs_newline = false;
    }

    pub fn start_definition_description(&mut self) {
        // A tight description contains inline events without an ordinary paragraph start. Create
        // its line here and write the visible Markdown `: ` marker before consuming the content so
        // the marker and content share the description style.
        self.push_line(Line::default());
        self.push_span(Span::styled(": ", self.styles.definition_description()));
        self.push_inline_style(self.styles.definition_description());
        self.in_definition_description = true;
        self.needs_newline = false;
    }

    pub fn end_definition_description(&mut self) {
        self.pop_inline_style();
        self.in_definition_description = false;
        self.needs_newline = false;
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

    mod definition_list {
        use super::*;

        #[derive(Clone)]
        struct CustomDefinitionStyleSheet;

        impl StyleSheet for CustomDefinitionStyleSheet {
            fn definition_term(&self) -> Style {
                Style::new().red().underlined()
            }

            fn definition_description(&self) -> Style {
                Style::new().blue().italic()
            }
        }

        #[rstest]
        fn exact_output_and_default_styles(_with_tracing: DefaultGuard) {
            insta::assert_debug_snapshot!(from_str(indoc!("
                Term
                : Definition
            ")), @r#"
            Text::from_iter([
                Line::from(Span::from("Term").bold()),
                Line::from_iter([
                    Span::from(": "),
                    Span::from("Definition"),
                ]),
            ])
            "#);
        }

        #[rstest]
        fn custom_styles_apply_to_terms_and_definitions(_with_tracing: DefaultGuard) {
            let options = Options::new(CustomDefinitionStyleSheet);

            insta::assert_debug_snapshot!(from_str_with_options(indoc!("
                Term
                : Definition
            "), &options), @r#"
            Text::from_iter([
                Line::from(Span::from("Term").red().underlined()),
                Line::from_iter([
                    Span::from(": ").blue().italic(),
                    Span::from("Definition").blue().italic(),
                ]),
            ])
            "#);
        }

        #[rstest]
        fn inline_formatting_combines_with_definition_styles(_with_tracing: DefaultGuard) {
            let options = Options::new(CustomDefinitionStyleSheet);

            insta::assert_debug_snapshot!(from_str_with_options(indoc!("
                *Term*
                : **Description**
            "), &options), @r#"
            Text::from_iter([
                Line::from(Span::from("Term").red().italic().underlined()),
                Line::from_iter([
                    Span::from(": ").blue().italic(),
                    Span::from("Description").blue().bold().italic(),
                ]),
            ])
            "#);
        }

        #[rstest]
        fn multiline_definition(_with_tracing: DefaultGuard) {
            insta::assert_debug_snapshot!(from_str(indoc!("
                Term
                : First line
                  second line
            ")), @r#"
            Text::from_iter([
                Line::from(Span::from("Term").bold()),
                Line::from_iter([
                    Span::from(": "),
                    Span::from("First line"),
                    Span::from(" "),
                    Span::from("second line"),
                ]),
            ])
            "#);
        }

        #[rstest]
        fn multiple_descriptions(_with_tracing: DefaultGuard) {
            insta::assert_debug_snapshot!(from_str(indoc!("
                Term
                : First description
                : Second description
            ")), @r#"
            Text::from_iter([
                Line::from(Span::from("Term").bold()),
                Line::from_iter([
                    Span::from(": "),
                    Span::from("First description"),
                ]),
                Line::from_iter([
                    Span::from(": "),
                    Span::from("Second description"),
                ]),
            ])
            "#);
        }

        #[rstest]
        fn multiple_description_paragraphs_keep_prefix_and_blank_line(_with_tracing: DefaultGuard) {
            insta::assert_debug_snapshot!(from_str(indoc!("
                Term
                : First paragraph.

                  Second paragraph.
            ")), @r#"
            Text::from_iter([
                Line::from(Span::from("Term").bold()),
                Line::from_iter([
                    Span::from(": "),
                    Span::from("First paragraph."),
                ]),
                Line::default(),
                Line::from("Second paragraph."),
            ])
            "#);
        }

        #[rstest]
        fn repeated_items_do_not_leak_into_following_paragraph(_with_tracing: DefaultGuard) {
            insta::assert_debug_snapshot!(
                "repeated_items_do_not_leak_into_following_paragraph",
                from_str(indoc!(
                    "
                    Term one
                    : First description.

                    Term two
                    : Second description.

                    After.
                "
                ))
            );
        }
    }
}
