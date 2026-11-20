//! Markdown image fallback rendering.

use pulldown_cmark::{CowStr, Event};
use ratatui_core::style::Style;
use ratatui_core::text::Span;
use tracing::instrument;

use super::TextWriter;
use crate::{ImageFallback, StyleSheet};

const IMAGE_INDICATOR: &str = "[img]";

/// An image whose description is still being emitted by pulldown-cmark.
///
/// Image descriptions arrive as the same inline event stream used for ordinary text: formatting,
/// code, HTML, math, and even nested images are separate events between the image's start and end.
/// Buffer the rendered spans until the end event so the fallback marker stays at the correct image
/// boundary and the destination is used only when the description produced no content. A stack is
/// required because pulldown-cmark can emit nested image events inside a description.
#[derive(Debug)]
pub struct PendingImage<'a> {
    destination: CowStr<'a>,
    style: Style,
    description: Vec<Span<'a>>,
}

impl<'a> PendingImage<'a> {
    fn new(destination: CowStr<'a>, style: Style) -> Self {
        Self {
            destination,
            style,
            description: Vec::new(),
        }
    }

    pub fn push_span(&mut self, span: Span<'a>) {
        self.description.push(span);
    }

    fn into_fallback(self, fallback: ImageFallback) -> Vec<Span<'a>> {
        let Self {
            destination,
            style,
            description,
        } = self;
        let mut content = match fallback {
            ImageFallback::AltText if description.is_empty() => {
                Self::destination_span(destination, style)
            }
            ImageFallback::AltText => description,
            ImageFallback::Url => Self::destination_span(destination, style),
            ImageFallback::AltTextAndUrl if description.is_empty() => {
                Self::destination_span(destination, style)
            }
            ImageFallback::AltTextAndUrl if destination.is_empty() => description,
            ImageFallback::AltTextAndUrl => {
                let mut description = description;
                let destination = format!(" ({destination})");
                description.push(Span::styled(destination, style));
                description
            }
        };

        let indicator = if content.is_empty() {
            IMAGE_INDICATOR.to_owned()
        } else {
            format!("{IMAGE_INDICATOR} ")
        };
        content.insert(0, Span::styled(indicator, style));
        content
    }

    fn destination_span(destination: CowStr<'a>, style: Style) -> Vec<Span<'a>> {
        if destination.is_empty() {
            Vec::new()
        } else {
            vec![Span::styled(destination, style)]
        }
    }
}

impl<'a, 'theme, I, S> TextWriter<'a, 'theme, I, S>
where
    I: Iterator<Item = Event<'a>>,
    S: StyleSheet,
{
    /// Begins collecting the rendered image description.
    #[instrument(level = "trace", skip(self))]
    pub fn start_image(&mut self, dest_url: CowStr<'a>) {
        self.push_inline_style(self.styles.image_alt());
        let style = self.inline_styles.last().copied().unwrap_or_default();
        self.images.push(PendingImage::new(dest_url, style));
    }

    /// Finishes the current image and emits its text fallback to the enclosing output.
    ///
    /// Pop the image before emitting so a nested image becomes part of its parent's description,
    /// while an outer image continues through the usual table or document span sink.
    #[instrument(level = "trace", skip(self))]
    pub fn end_image(&mut self) {
        self.pop_inline_style();
        if let Some(image) = self.images.pop() {
            for span in image.into_fallback(self.image_fallback) {
                self.push_span(span);
            }
        }
    }

    pub fn image_description_break(&mut self) {
        // Image descriptions are inline content. Keep a break readable without allowing it to
        // split the surrounding document, and retain the image style in case it has a background.
        let style = self.inline_styles.last().copied().unwrap_or_default();
        self.push_span(Span::styled(" ", style));
    }
}

#[cfg(test)]
mod tests {
    use indoc::indoc;
    use itertools::Itertools;
    use ratatui_core::style::Stylize;
    use ratatui_core::text::{Line, Span, Text};

    use rstest::rstest;

    use super::*;
    use crate::renderer::test_support::{with_tracing, DefaultGuard};
    use crate::{from_str, from_str_with_options, Options};

    mod image {
        use super::*;
        use pretty_assertions::assert_eq;

        #[derive(Clone)]
        struct UnstyledImageStyleSheet;

        impl StyleSheet for UnstyledImageStyleSheet {
            fn heading(&self, _level: u8) -> Style {
                Style::default()
            }

            fn code(&self) -> Style {
                Style::default()
            }

            fn link(&self) -> Style {
                Style::default()
            }

            fn blockquote(&self) -> Style {
                Style::default()
            }

            fn heading_meta(&self) -> Style {
                Style::default()
            }

            fn metadata_block(&self) -> Style {
                Style::default()
            }

            fn image_alt(&self) -> Style {
                Style::default()
            }
        }

        #[rstest]
        fn image_with_alt(_with_tracing: DefaultGuard) {
            assert_eq!(
                from_str("![Alt text](https://example.com/image.png)"),
                Text::from(Line::from_iter([
                    Span::from("[img] ").dim().italic(),
                    Span::from("Alt text").dim().italic()
                ]))
            );
        }

        #[rstest]
        fn image_without_alt(_with_tracing: DefaultGuard) {
            assert_eq!(
                from_str("![](https://example.com/image.png)"),
                Text::from(Line::from_iter([
                    Span::from("[img] ").dim().italic(),
                    Span::from("https://example.com/image.png").dim().italic()
                ]))
            );
        }

        #[rstest]
        fn image_with_title(_with_tracing: DefaultGuard) {
            assert_eq!(
                from_str("![Alt](https://example.com/img.png \"My Title\")"),
                Text::from(Line::from_iter([
                    Span::from("[img] ").dim().italic(),
                    Span::from("Alt").dim().italic()
                ]))
            );
        }

        #[rstest]
        fn image_in_paragraph(_with_tracing: DefaultGuard) {
            assert_eq!(
                from_str("Before ![photo](url.png) after"),
                Text::from(Line::from_iter([
                    Span::from("Before "),
                    Span::from("[img] ").dim().italic(),
                    Span::from("photo").dim().italic(),
                    Span::from(" after")
                ]))
            );
        }

        #[rstest]
        fn multiple_images_in_paragraph(_with_tracing: DefaultGuard) {
            insta::assert_snapshot!(
                from_str("![first](first.png) and ![second](second.png)"),
                @"[img] first and [img] second"
            );
        }

        #[rstest]
        fn formatted_alt_text_keeps_prefix_before_content(_with_tracing: DefaultGuard) {
            assert_eq!(
                from_str("![**bold**](image.png)"),
                Text::from(Line::from_iter([
                    Span::from("[img] ").dim().italic(),
                    Span::from("bold").bold().dim().italic()
                ]))
            );
        }

        #[rstest]
        fn inline_code_counts_as_alt_text(_with_tracing: DefaultGuard) {
            assert_eq!(
                from_str("![`code`](image.png)"),
                Text::from(Line::from_iter([
                    Span::from("[img] ").dim().italic(),
                    Span::from("code").white().on_black().dim().italic()
                ]))
            );
        }

        #[rstest]
        fn marker_and_alt_compose_with_enclosing_style(_with_tracing: DefaultGuard) {
            assert_eq!(
                from_str("**![diagram](diagram.png)**"),
                Text::from(Line::from_iter([
                    Span::from("[img] ").bold().dim().italic(),
                    Span::from("diagram").bold().dim().italic()
                ]))
            );
        }

        #[rstest]
        fn marker_and_url_compose_with_enclosing_style(_with_tracing: DefaultGuard) {
            assert_eq!(
                from_str("**![](diagram.png)**"),
                Text::from(Line::from_iter([
                    Span::from("[img] ").bold().dim().italic(),
                    Span::from("diagram.png").bold().dim().italic()
                ]))
            );
        }

        #[rstest]
        fn inline_math_counts_as_alt_text(_with_tracing: DefaultGuard) {
            assert_eq!(
                from_str("![$x$](equation.png)"),
                Text::from(Line::from_iter([
                    Span::from("[img] ").dim().italic(),
                    Span::from("$x$").magenta().dim().italic()
                ]))
            );
        }

        #[rstest]
        fn inline_html_counts_as_alt_text(_with_tracing: DefaultGuard) {
            assert_eq!(
                from_str("![<br>](break.png)"),
                Text::from(Line::from_iter([
                    Span::from("[img] ").dim().italic(),
                    Span::from("<br>").dim().italic()
                ]))
            );
        }

        #[rstest]
        fn nested_image_description_preserves_order_and_style(_with_tracing: DefaultGuard) {
            assert_eq!(
                from_str("![outer ![inner](inner.png) `code`](outer.png)"),
                Text::from(Line::from_iter([
                    Span::from("[img] ").dim().italic(),
                    Span::from("outer ").dim().italic(),
                    Span::from("[img] ").dim().italic(),
                    Span::from("inner").dim().italic(),
                    Span::from(" ").dim().italic(),
                    Span::from("code").white().on_black().dim().italic()
                ]))
            );
        }

        #[rstest]
        fn empty_description_and_destination_omit_trailing_space(_with_tracing: DefaultGuard) {
            assert_eq!(
                from_str("Before ![]() after"),
                Text::from(Line::from_iter([
                    Span::from("Before "),
                    Span::from("[img]").dim().italic(),
                    Span::from(" after")
                ]))
            );
        }

        #[rstest]
        fn multiline_description_renders_as_styled_inline_text(_with_tracing: DefaultGuard) {
            assert_eq!(
                from_str(indoc! {"
                    ![first line
                    second line](image.png)
                "}),
                Text::from(Line::from_iter([
                    Span::from("[img] ").dim().italic(),
                    Span::from("first line").dim().italic(),
                    Span::from(" ").dim().italic(),
                    Span::from("second line").dim().italic()
                ]))
            );
        }

        #[rstest]
        fn hard_break_in_description_stays_inline(_with_tracing: DefaultGuard) {
            assert_eq!(
                from_str(indoc! {"
                    ![first line  
                    second line](image.png)
                "}),
                Text::from(Line::from_iter([
                    Span::from("[img] ").dim().italic(),
                    Span::from("first line").dim().italic(),
                    Span::from(" ").dim().italic(),
                    Span::from("second line").dim().italic()
                ]))
            );
        }

        #[rstest]
        fn image_fallback_stays_inside_table_cell(_with_tracing: DefaultGuard) {
            let rendered = from_str(indoc! {"
                | Image |
                |-------|
                | ![photo](photo.png) |
            "})
            .lines
            .iter()
            .map(ToString::to_string)
            .collect_vec();
            insta::assert_debug_snapshot!(rendered, @r#"
            [
                "┌─────────────┐",
                "│ Image       │",
                "├─────────────┤",
                "│ [img] photo │",
                "└─────────────┘",
            ]
            "#);
        }

        #[rstest]
        fn url_fallback_uses_destination(_with_tracing: DefaultGuard) {
            let options = Options::default().image_fallback(ImageFallback::Url);
            assert_eq!(
                from_str_with_options("![diagram](diagram.png)", &options),
                Text::from(Line::from_iter([
                    Span::from("[img] ").dim().italic(),
                    Span::from("diagram.png").dim().italic()
                ]))
            );
        }

        #[rstest]
        fn url_fallback_discards_complete_formatted_description(_with_tracing: DefaultGuard) {
            let options = Options::default().image_fallback(ImageFallback::Url);
            insta::assert_snapshot!(from_str_with_options(
                "![**bold** $x$ <br> `code`](diagram.png)",
                &options
            ), @"[img] diagram.png");
        }

        #[rstest]
        fn alt_text_and_url_fallback_preserves_formatted_description(_with_tracing: DefaultGuard) {
            let options = Options::default().image_fallback(ImageFallback::AltTextAndUrl);
            assert_eq!(
                from_str_with_options("![**diagram**](diagram.png)", &options),
                Text::from(Line::from_iter([
                    Span::from("[img] ").dim().italic(),
                    Span::from("diagram").bold().dim().italic(),
                    Span::from(" (diagram.png)").dim().italic()
                ]))
            );
        }

        #[rstest]
        fn alt_text_and_url_fallback_uses_destination_for_empty_description(
            _with_tracing: DefaultGuard,
        ) {
            let options = Options::default().image_fallback(ImageFallback::AltTextAndUrl);
            let text = from_str_with_options("![](diagram.png)", &options);
            insta::assert_snapshot!(text, @"[img] diagram.png");
        }

        #[rstest]
        fn configured_fallback_omits_space_when_destination_is_empty(
            #[values(ImageFallback::Url, ImageFallback::AltTextAndUrl)] fallback: ImageFallback,
            _with_tracing: DefaultGuard,
        ) {
            let options = Options::default().image_fallback(fallback);
            insta::assert_debug_snapshot!(
                format!("empty_destination_{fallback:?}"),
                from_str_with_options("![]()", &options)
            );
        }

        #[rstest]
        fn unstyled_fallback_does_not_modify_surrounding_text(_with_tracing: DefaultGuard) {
            let options = Options::new(UnstyledImageStyleSheet);
            assert_eq!(
                from_str_with_options("Before ![photo](photo.png) after", &options),
                Text::from(Line::from_iter([
                    Span::from("Before "),
                    Span::from("[img] "),
                    Span::from("photo"),
                    Span::from(" after")
                ]))
            );
        }
    }
}
