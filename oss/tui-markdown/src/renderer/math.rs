//! Markdown inline and display math rendering.
//!
//! Inline math retains `$` delimiters and its position in the surrounding line. Display math keeps
//! `$$` delimiters and writes each source line as a physical Ratatui line.

use pulldown_cmark::{CowStr, Event};
use ratatui_core::text::{Line, Span};

use super::TextWriter;
use crate::StyleSheet;

impl<'a, 'theme, I, S> TextWriter<'a, 'theme, I, S>
where
    I: Iterator<Item = Event<'a>>,
    S: StyleSheet,
{
    pub fn inline_math(&mut self, math: CowStr<'a>) {
        let inline_style = self.inline_styles.last().copied().unwrap_or_default();
        let style = inline_style.patch(self.styles.math_inline());
        self.push_span(Span::styled(format!("${math}$"), style));
    }

    pub fn display_math(&mut self, math: CowStr<'a>) {
        if self.needs_newline {
            self.push_line(Line::default());
        }
        let style = self.styles.math_display();
        let display_math = format!("$${math}$$");
        for (index, line) in display_math.lines().enumerate() {
            if index > 0 {
                self.push_line(Line::default());
            }
            self.push_span(Span::styled(line.to_owned(), style));
        }
        self.needs_newline = true;
    }
}

#[cfg(test)]
mod tests {
    use indoc::indoc;
    use ratatui_core::style::Style;
    use ratatui_core::style::Stylize;
    use ratatui_core::text::{Line, Span, Text};
    use rstest::rstest;

    use super::*;
    use crate::renderer::test_support::{with_tracing, DefaultGuard};
    use crate::{from_str, from_str_with_options, Options};

    mod math {
        use super::*;
        use pretty_assertions::assert_eq;

        #[rstest]
        fn inline_math_has_exact_output_and_style(_with_tracing: DefaultGuard) {
            assert_eq!(
                from_str("The formula $E=mc^2$ is famous."),
                Text::from(Line::from_iter([
                    Span::from("The formula "),
                    Span::from("$E=mc^2$").magenta().italic(),
                    Span::from(" is famous.")
                ]))
            );
        }

        #[rstest]
        fn inline_math_combines_with_enclosing_style(_with_tracing: DefaultGuard) {
            insta::assert_debug_snapshot!(
                from_str("**$x$**"),
                @r#"Text::from(Line::from(Span::from("$x$").magenta().bold().italic()))"#
            );
        }

        #[rstest]
        fn multiline_display_math_styles_every_line(_with_tracing: DefaultGuard) {
            insta::assert_debug_snapshot!(from_str(indoc!("
                Before

                $$
                x = y
                y = z
                $$

                After
            ")), @r#"
            Text::from_iter([
                Line::from("Before"),
                Line::default(),
                Line::from(Span::from("$$").magenta()),
                Line::from(Span::from("x = y").magenta()),
                Line::from(Span::from("y = z").magenta()),
                Line::from(Span::from("$$").magenta()),
                Line::default(),
                Line::from("After"),
            ])
            "#);
        }

        #[rstest]
        fn multiline_display_math_uses_custom_style(_with_tracing: DefaultGuard) {
            #[derive(Clone, Copy)]
            struct CustomMathStyle;

            impl StyleSheet for CustomMathStyle {
                fn math_display(&self) -> Style {
                    Style::new().red().bold()
                }
            }

            let options = Options::new(CustomMathStyle);

            insta::assert_debug_snapshot!(from_str_with_options(indoc!("
                $$
                x = y
                y = z
                $$
            "), &options), @r#"
            Text::from_iter([
                Line::from(Span::from("$$").red().bold()),
                Line::from(Span::from("x = y").red().bold()),
                Line::from(Span::from("y = z").red().bold()),
                Line::from(Span::from("$$").red().bold()),
            ])
            "#);
        }
    }
}
