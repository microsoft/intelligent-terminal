//! Markdown inline formatting.
//!
//! The inline style stack patches nested formatting over its enclosing style. Closing a formatting
//! tag restores the previous style.

use pulldown_cmark::Event;
use ratatui_core::style::Style;
use tracing::{debug, instrument};

use super::TextWriter;
use crate::StyleSheet;

impl<'a, 'theme, I, S> TextWriter<'a, 'theme, I, S>
where
    I: Iterator<Item = Event<'a>>,
    S: StyleSheet,
{
    #[instrument(level = "trace", skip(self))]
    pub fn push_inline_style(&mut self, style: Style) {
        let current_style = self.inline_styles.last().copied().unwrap_or_default();
        let style = current_style.patch(style);
        self.inline_styles.push(style);
        debug!("Pushed inline style: {:?}", style);
        debug!("Current inline styles: {:?}", self.inline_styles);
    }

    #[instrument(level = "trace", skip(self))]
    pub fn pop_inline_style(&mut self) {
        self.inline_styles.pop();
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use ratatui_core::style::Stylize;
    use ratatui_core::text::{Line, Span, Text};
    use rstest::rstest;

    use crate::from_str;
    use crate::renderer::test_support::{with_tracing, DefaultGuard};

    #[rstest]
    #[case::superscript(
        "H ^2^ O",
        Line::from_iter([Span::raw("H "), "2".dim().italic(), Span::raw(" O")])
    )]
    #[case::subscript(
        "H ~2~ O",
        Line::from_iter([Span::raw("H "), "2".dim().italic(), Span::raw(" O")])
    )]
    #[case::strong("**Strong**", Line::from("Strong".bold()))]
    #[case::emphasis("*Emphasis*", Line::from("Emphasis".italic()))]
    #[case::strikethrough("~~Strikethrough~~", Line::from("Strikethrough".crossed_out()))]
    #[case::strong_emphasis(
        "**Strong *emphasis***",
        Line::from_iter(["Strong ".bold(), "emphasis".bold().italic()])
    )]
    #[case::style_scope(
        "Before **strong** after",
        Line::from_iter([Span::raw("Before "), "strong".bold(), Span::raw(" after")])
    )]
    fn inline_formatting(
        _with_tracing: DefaultGuard,
        #[case] markdown: &str,
        #[case] expected: Line<'static>,
    ) {
        assert_eq!(from_str(markdown), Text::from(expected));
    }
}
