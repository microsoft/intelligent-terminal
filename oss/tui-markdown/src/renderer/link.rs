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
    use ratatui_core::style::Stylize;
    use ratatui_core::text::{Line, Span, Text};

    use rstest::rstest;

    use crate::from_str;
    use crate::renderer::test_support::{with_tracing, DefaultGuard};

    #[rstest]
    fn link_uses_default_style(_with_tracing: DefaultGuard) {
        assert_eq!(
            from_str("[Link](https://example.com)"),
            Text::from(Line::from_iter([
                Span::from("Link").blue().underlined(),
                Span::from(" ("),
                Span::from("https://example.com").blue().underlined(),
                Span::from(")")
            ]))
        );
    }

    #[rstest]
    fn link_combines_with_bold_style(_with_tracing: DefaultGuard) {
        assert_eq!(
            from_str("[**Bold link**](https://example.com)"),
            Text::from(Line::from_iter([
                Span::from("Bold link").blue().bold().underlined(),
                Span::from(" ("),
                Span::from("https://example.com").blue().underlined(),
                Span::from(")")
            ]))
        );
    }

    #[rstest]
    fn consecutive_links_restore_surrounding_style(_with_tracing: DefaultGuard) {
        insta::assert_debug_snapshot!(
            "consecutive_links_restore_surrounding_style",
            from_str("[One](one) and [Two](two) after")
        );
    }
}
