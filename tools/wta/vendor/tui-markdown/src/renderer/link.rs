//! Markdown link rendering.
//!
//! Links render as `label (destination)`. The link style applies to the label and destination while
//! nested inline formatting remains on the label.

use pulldown_cmark::{CowStr, Event};
use ratatui_core::text::Span;
use tracing::instrument;

use super::TextWriter;
use crate::StyleSheet;

impl<'a, 'theme, I, S> TextWriter<'a, 'theme, I, S>
where
    I: Iterator<Item = Event<'a>>,
    S: StyleSheet,
{
    /// Stores the destination and applies the link style to the label.
    #[instrument(level = "trace", skip(self))]
    pub fn push_link(&mut self, dest_url: CowStr<'a>) {
        self.link = Some(dest_url);
        self.push_inline_style(self.styles.link());
    }

    /// Restores the enclosing style and appends the destination.
    #[instrument(level = "trace", skip(self))]
    pub fn pop_link(&mut self) {
        self.pop_inline_style();
        if let Some(link) = self.link.take() {
            self.push_span(" (".into());
            self.push_span(Span::styled(link, self.styles.link()));
            self.push_span(")".into());
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    use super::*;
    use crate::renderer::test_support::{with_tracing, DefaultGuard};
    use crate::renderer::*;

    #[rstest]
    fn link_uses_default_style(_with_tracing: DefaultGuard) {
        let link_style = Style::new().blue().underlined();
        assert_eq!(
            from_str("[Link](https://example.com)"),
            Text::from(Line::from_iter([
                Span::styled("Link", link_style),
                Span::from(" ("),
                Span::styled("https://example.com", link_style),
                Span::from(")")
            ]))
        );
    }

    #[rstest]
    fn link_combines_with_bold_style(_with_tracing: DefaultGuard) {
        let link_style = Style::new().blue().underlined();
        assert_eq!(
            from_str("[**Bold link**](https://example.com)"),
            Text::from(Line::from_iter([
                Span::styled("Bold link", link_style.bold()),
                Span::from(" ("),
                Span::styled("https://example.com", link_style),
                Span::from(")"),
            ]))
        );
    }

    #[rstest]
    fn consecutive_links_restore_surrounding_style(_with_tracing: DefaultGuard) {
        let link_style = Style::new().blue().underlined();
        assert_eq!(
            from_str("[One](one) and [Two](two) after"),
            Text::from(Line::from_iter([
                Span::styled("One", link_style),
                Span::raw(" ("),
                Span::styled("one", link_style),
                Span::raw(")"),
                Span::raw(" and "),
                Span::styled("Two", link_style),
                Span::raw(" ("),
                Span::styled("two", link_style),
                Span::raw(")"),
                Span::raw(" after"),
            ]))
        );
    }
}
