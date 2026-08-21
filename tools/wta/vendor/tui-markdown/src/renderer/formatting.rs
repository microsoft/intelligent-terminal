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
    use rstest::rstest;

    use super::*;
    use crate::renderer::test_support::{with_tracing, DefaultGuard};
    use crate::renderer::*;

    #[rstest]
    fn superscript(_with_tracing: DefaultGuard) {
        assert_eq!(
            from_str("H ^2^ O"),
            Text::from(Line::from_iter([
                Span::from("H "),
                Span::styled("2", Style::new().dim().italic()),
                Span::from(" O"),
            ]))
        );
    }

    #[rstest]
    fn subscript(_with_tracing: DefaultGuard) {
        assert_eq!(
            from_str("H ~2~ O"),
            Text::from(Line::from_iter([
                Span::from("H "),
                Span::styled("2", Style::new().dim().italic()),
                Span::from(" O"),
            ]))
        );
    }

    #[rstest]
    fn strong(_with_tracing: DefaultGuard) {
        assert_eq!(
            from_str("**Strong**"),
            Text::from(Line::from("Strong".bold()))
        );
    }

    #[rstest]
    fn emphasis(_with_tracing: DefaultGuard) {
        assert_eq!(
            from_str("*Emphasis*"),
            Text::from(Line::from("Emphasis".italic()))
        );
    }

    #[rstest]
    fn strikethrough(_with_tracing: DefaultGuard) {
        assert_eq!(
            from_str("~~Strikethrough~~"),
            Text::from(Line::from("Strikethrough".crossed_out()))
        );
    }

    #[rstest]
    fn strong_emphasis(_with_tracing: DefaultGuard) {
        assert_eq!(
            from_str("**Strong *emphasis***"),
            Text::from(Line::from_iter([
                "Strong ".bold(),
                "emphasis".bold().italic()
            ]))
        );
    }

    #[rstest]
    fn formatting_does_not_leak_into_following_text(_with_tracing: DefaultGuard) {
        assert_eq!(
            from_str("Before **strong** after"),
            Text::from(Line::from_iter([
                Span::raw("Before "),
                Span::raw("strong").bold(),
                Span::raw(" after"),
            ]))
        );
    }
}
