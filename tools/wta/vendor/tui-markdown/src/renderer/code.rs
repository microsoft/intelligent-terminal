//! Markdown inline and fenced code rendering.
//!
//! Inline code and unrecognized fences use the style sheet's code style. With `highlight-code`
//! enabled, a recognized fenced language uses the selected syntax-highlighting theme.

#[cfg(feature = "highlight-code")]
use std::sync::LazyLock;

#[cfg(feature = "highlight-code")]
use ansi_to_tui::IntoText;
use pulldown_cmark::{CodeBlockKind, CowStr, Event};
#[cfg(feature = "highlight-code")]
use ratatui_core::text::Text;
use ratatui_core::text::{Line, Span};
#[cfg(feature = "highlight-code")]
use syntect::{
    easy::HighlightLines,
    parsing::SyntaxSet,
    util::{as_24_bit_terminal_escaped, LinesWithEndings},
};
#[cfg(feature = "highlight-code")]
use tracing::{debug, instrument, warn};

use super::TextWriter;
#[cfg(feature = "highlight-code")]
use crate::code_theme::{self, CodeTheme};
use crate::StyleSheet;

#[cfg(feature = "highlight-code")]
static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);

impl<'a, 'theme, I, S> TextWriter<'a, 'theme, I, S>
where
    I: Iterator<Item = Event<'a>>,
    S: StyleSheet,
{
    pub fn code(&mut self, code: CowStr<'a>) {
        let style = if self.images.is_empty() {
            self.styles.code()
        } else {
            let inline_style = self.inline_styles.last().copied().unwrap_or_default();
            inline_style.patch(self.styles.code())
        };

        self.push_span(Span::styled(code, style));
    }

    pub fn start_codeblock(&mut self, kind: CodeBlockKind<'_>) {
        if !self.text.lines.is_empty() {
            self.push_line(Line::default());
        }
        let lang = match kind {
            CodeBlockKind::Fenced(ref lang) => lang.as_ref(),
            CodeBlockKind::Indented => "",
        };

        #[cfg(not(feature = "highlight-code"))]
        self.line_styles.push(self.styles.code());

        #[cfg(feature = "highlight-code")]
        self.set_code_highlighter(lang);

        let fence = self.styles.code_block_fence();
        if !fence.is_empty() {
            let span = Span::from(format!("{fence}{lang}"));
            self.push_line(span.into());
        }
        self.needs_newline = true;
    }

    pub fn end_codeblock(&mut self) {
        let fence = self.styles.code_block_fence();
        if !fence.is_empty() {
            let span = Span::from(fence.to_owned());
            self.push_line(span.into());
        }
        self.needs_newline = true;

        #[cfg(not(feature = "highlight-code"))]
        self.line_styles.pop();

        #[cfg(feature = "highlight-code")]
        self.clear_code_highlighter();
    }

    #[cfg(feature = "highlight-code")]
    pub fn with_code_theme(mut self, theme: Option<&'theme CodeTheme>) -> Self {
        self.code_theme = theme;
        self
    }

    #[cfg(feature = "highlight-code")]
    pub fn push_highlighted_text(&mut self, text: &str) -> bool {
        let Some(highlighter) = &mut self.code_highlighter else {
            return false;
        };
        let text: Text = LinesWithEndings::from(text)
            .filter_map(|line| highlighter.highlight_line(line, &SYNTAX_SET).ok())
            .filter_map(|part| as_24_bit_terminal_escaped(&part, false).into_text().ok())
            .flatten()
            .collect();

        for line in text.lines {
            self.text.push_line(line);
        }
        self.needs_newline = false;
        true
    }

    #[cfg(not(feature = "highlight-code"))]
    pub fn push_highlighted_text(&mut self, _text: &str) -> bool {
        false
    }

    #[cfg(feature = "highlight-code")]
    #[instrument(level = "trace", skip(self))]
    fn set_code_highlighter(&mut self, lang: &str) {
        if let Some(syntax) = SYNTAX_SET.find_syntax_by_token(lang) {
            debug!("Starting code block with syntax: {:?}", lang);
            let code_theme = match self.code_theme {
                Some(code_theme) => code_theme,
                None => code_theme::default(),
            };
            let theme = code_theme::theme(code_theme);
            let highlighter = HighlightLines::new(syntax, theme);
            self.code_highlighter = Some(highlighter);
        } else {
            warn!("Could not find syntax for code block: {:?}", lang);
        }
    }

    #[cfg(feature = "highlight-code")]
    #[instrument(level = "trace", skip(self))]
    fn clear_code_highlighter(&mut self) {
        self.code_highlighter = None;
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

    #[derive(Clone, Copy)]
    struct CustomCodeBlockFence(&'static str);

    impl StyleSheet for CustomCodeBlockFence {
        fn code_block_fence(&self) -> &str {
            self.0
        }
    }

    #[cfg_attr(not(feature = "highlight-code"), ignore)]
    #[rstest]
    fn highlighted_code(_with_tracing: DefaultGuard) {
        // Assert no extra newlines are added
        let highlighted_code = from_str(indoc! {"
            ```rust
            fn main() {
                println!(\"Hello, highlighted code!\");
            }
            ```"});

        insta::assert_snapshot!(highlighted_code);
        insta::assert_debug_snapshot!(highlighted_code);
    }

    #[cfg_attr(not(feature = "highlight-code"), ignore)]
    #[rstest]
    fn highlighted_code_with_indentation(_with_tracing: DefaultGuard) {
        // Assert no extra newlines are added
        let highlighted_code_indented = from_str(indoc! {"
            ```rust
            fn main() {
                // This is a comment
                HelloWorldBuilder::new()
                    .with_text(\"Hello, highlighted code!\")
                    .build()
                    .show();
                            
            }
            ```"});

        insta::assert_snapshot!(highlighted_code_indented);
        insta::assert_debug_snapshot!(highlighted_code_indented);
    }

    #[cfg_attr(feature = "highlight-code", ignore)]
    #[rstest]
    fn unhighlighted_code(_with_tracing: DefaultGuard) {
        // Assert no extra newlines are added
        let unhiglighted_code = from_str(indoc! {"
            ```rust
            fn main() {
                println!(\"Hello, unhighlighted code!\");
            }
            ```"});

        insta::assert_snapshot!(unhiglighted_code);

        // Code highlighting is complex, assert on on the debug snapshot
        insta::assert_debug_snapshot!(unhiglighted_code);
    }

    #[rstest]
    fn inline_code(_with_tracing: DefaultGuard) {
        let text = from_str("Example of `Inline code`");
        insta::assert_snapshot!(text);

        assert_eq!(
            text,
            Line::from_iter([
                Span::from("Example of "),
                Span::styled("Inline code", Style::new().white().on_black())
            ])
            .into()
        );
    }

    #[rstest]
    fn fenced_code_style_does_not_leak_into_following_paragraph(_with_tracing: DefaultGuard) {
        let markdown = indoc! {"
            ```rust
            fn main() {}
            ```

            After
        "};
        let text = from_str(markdown);

        assert_eq!(text.lines.last(), Some(&Line::from("After")));
    }

    #[rstest]
    fn custom_code_block_fence(_with_tracing: DefaultGuard) {
        let options = Options::new(CustomCodeBlockFence("~~~"));
        let markdown = "```not-a-language\ncode\n```";

        assert_eq!(
            from_str_with_options(markdown, &options).to_string(),
            "~~~not-a-language\ncode\n~~~"
        );
    }

    #[rstest]
    fn empty_code_block_fence_preserves_spacing(_with_tracing: DefaultGuard) {
        let options = Options::new(CustomCodeBlockFence(""));
        let markdown = "Before\n\n```not-a-language\ncode\n```\n\nAfter";

        assert_eq!(
            from_str_with_options(markdown, &options).to_string(),
            "Before\n\ncode\n\nAfter"
        );
    }

    #[rstest]
    fn empty_code_block_fence_applies_to_indented_code(_with_tracing: DefaultGuard) {
        let options = Options::new(CustomCodeBlockFence(""));

        assert_eq!(
            from_str_with_options("    indented code", &options).to_string(),
            "indented code"
        );
    }

    #[cfg(feature = "highlight-code")]
    #[rstest]
    fn empty_code_block_fence_applies_to_highlighted_code(_with_tracing: DefaultGuard) {
        let options = Options::new(CustomCodeBlockFence(""));
        let markdown = "```rust\nfn main() {}\n```";
        let text = from_str_with_options(markdown, &options);

        assert_eq!(text.to_string(), "fn main() {}");
    }

    #[cfg(feature = "highlight-code")]
    mod code_theme {
        use pretty_assertions::assert_eq;

        use super::*;
        use crate::{BuiltinCodeTheme, Options};

        #[rstest]
        fn different_theme_produces_different_output(_with_tracing: DefaultGuard) {
            let input = indoc! {"
                ```rust
                fn main() {}
                ```
            "};
            let default_out = from_str(input);
            let options = Options::default().code_theme(BuiltinCodeTheme::InspiredGitHub);
            let custom_out = from_str_with_options(input, &options);

            assert_ne!(default_out, custom_out);
        }

        #[rstest]
        fn explicit_default_theme_matches_implicit_default(_with_tracing: DefaultGuard) {
            let input = indoc! {"
                ```rust
                fn main() {}
                ```
            "};
            let implicit = from_str(input);
            let options = Options::default().code_theme(BuiltinCodeTheme::default());
            let explicit = from_str_with_options(input, &options);

            assert_eq!(explicit, implicit);
        }

        #[rstest]
        fn selected_theme_does_not_change_unrecognized_code(_with_tracing: DefaultGuard) {
            let input = indoc! {"
                ```not-a-language
                some code
                ```
            "};
            let default_out = from_str(input);
            let options = Options::default().code_theme(BuiltinCodeTheme::InspiredGitHub);
            let selected_out = from_str_with_options(input, &options);

            assert_eq!(selected_out, default_out);
        }
    }
}
