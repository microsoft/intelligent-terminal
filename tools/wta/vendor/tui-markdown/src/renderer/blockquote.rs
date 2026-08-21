//! Markdown blockquote and GFM alert rendering.
//!
//! Plain blockquotes use the configured blockquote style and `>` prefix. A recognized GFM alert
//! adds a styled icon and label, then renders its body with the alert style.

use pulldown_cmark::{BlockQuoteKind, Event};
use ratatui_core::style::Style;
use ratatui_core::text::{Line, Span};

use super::TextWriter;
use crate::{AlertKind, StyleSheet};

impl<'a, 'theme, I, S> TextWriter<'a, 'theme, I, S>
where
    I: Iterator<Item = Event<'a>>,
    S: StyleSheet,
{
    pub fn start_blockquote(&mut self, kind: Option<BlockQuoteKind>) {
        if self.needs_newline {
            self.push_line(Line::default());
            self.needs_newline = false;
        }

        match kind {
            Some(kind) => self.start_alert(alert_kind(kind)),
            None => self.start_plain_blockquote(),
        }
    }

    pub fn end_blockquote(&mut self) {
        self.line_prefixes.pop();
        self.line_styles.pop();
        self.needs_newline = true;
    }

    fn start_alert(&mut self, kind: AlertKind) {
        let style = self.styles.alert(kind);
        self.push_blockquote_style(style);
        self.push_line(Line::default());
        self.push_span(Span::styled(self.alert_heading(kind), style.bold()));
        self.needs_newline = false;
    }

    fn start_plain_blockquote(&mut self) {
        self.push_blockquote_style(self.styles.blockquote());
    }

    fn push_blockquote_style(&mut self, style: Style) {
        self.line_prefixes.push(Span::from(">"));
        self.line_styles.push(style);
    }

    fn alert_heading(&self, kind: AlertKind) -> String {
        let icon = self.styles.alert_icon(kind);
        let label = self.styles.alert_label(kind);
        // Either component may be intentionally suppressed; add a separator only when both exist.
        match (icon.is_empty(), label.is_empty()) {
            (false, false) => format!("{icon} {label}"),
            (false, true) => icon.to_owned(),
            (true, false) => label.to_owned(),
            (true, true) => String::new(),
        }
    }
}

fn alert_kind(kind: BlockQuoteKind) -> AlertKind {
    match kind {
        BlockQuoteKind::Note => AlertKind::Note,
        BlockQuoteKind::Tip => AlertKind::Tip,
        BlockQuoteKind::Important => AlertKind::Important,
        BlockQuoteKind::Warning => AlertKind::Warning,
        BlockQuoteKind::Caution => AlertKind::Caution,
    }
}

#[cfg(test)]
mod tests {
    use indoc::{formatdoc, indoc};
    use rstest::rstest;

    use super::*;
    use crate::renderer::test_support::{with_tracing, DefaultGuard};
    use crate::renderer::*;
    use crate::*;

    mod gfm_alerts {
        use pretty_assertions::assert_eq;

        use super::*;

        #[derive(Clone)]
        struct CustomAlertStyleSheet;

        impl StyleSheet for CustomAlertStyleSheet {
            fn alert(&self, kind: AlertKind) -> Style {
                match kind {
                    AlertKind::Note => Style::new().on_red(),
                    _ => Style::default(),
                }
            }
        }

        #[derive(Clone)]
        struct CustomAlertHeadingStyleSheet;

        impl StyleSheet for CustomAlertHeadingStyleSheet {
            fn alert_icon(&self, kind: AlertKind) -> &str {
                match kind {
                    AlertKind::Note => "!!",
                    AlertKind::Caution => "",
                    _ => DefaultStyleSheet.alert_icon(kind),
                }
            }

            fn alert_label(&self, kind: AlertKind) -> &str {
                match kind {
                    AlertKind::Tip => "Hint",
                    AlertKind::Important => "",
                    _ => kind.label(),
                }
            }
        }

        #[rstest]
        #[case("NOTE", "\u{2139}\u{FE0F} Note", Style::new().blue())]
        #[case("TIP", "\u{1F4A1} Tip", Style::new().green())]
        #[case("IMPORTANT", "\u{2757} Important", Style::new().magenta())]
        #[case("WARNING", "\u{26A0}\u{FE0F} Warning", Style::new().yellow())]
        #[case("CAUTION", "\u{1F534} Caution", Style::new().red())]
        fn alert_kind_renders_exact_output(
            _with_tracing: DefaultGuard,
            #[case] marker: &str,
            #[case] heading: &str,
            #[case] style: Style,
        ) {
            let markdown = formatdoc! {"
                > [!{marker}]
                > Body
            "};

            assert_eq!(
                from_str(&markdown),
                Text::from_iter([
                    Line::from_iter([
                        Span::raw(">"),
                        Span::raw(" "),
                        Span::styled(heading.to_owned(), style.bold()),
                    ])
                    .style(style),
                    Line::from_iter([Span::raw(">"), Span::raw(" "), Span::raw("Body")])
                        .style(style),
                ])
            );
        }

        #[rstest]
        fn custom_alert_style_applies_to_header_and_body(_with_tracing: DefaultGuard) {
            let markdown = indoc! {"
                > [!NOTE]
                > Body
            "};
            let options = Options::new(CustomAlertStyleSheet);
            let style = Style::new().on_red();

            assert_eq!(
                from_str_with_options(markdown, &options),
                Text::from_iter([
                    Line::from_iter([
                        Span::raw(">"),
                        Span::raw(" "),
                        Span::styled("\u{2139}\u{FE0F} Note", style.bold()),
                    ])
                    .style(style),
                    Line::from_iter([Span::raw(">"), Span::raw(" "), Span::raw("Body")])
                        .style(style),
                ])
            );
        }

        #[rstest]
        fn custom_alert_icon_replaces_default(_with_tracing: DefaultGuard) {
            let markdown = indoc! {"
                > [!NOTE]
                > Body
            "};
            let options = Options::new(CustomAlertHeadingStyleSheet);
            let style = DefaultStyleSheet.alert(AlertKind::Note);

            assert_eq!(
                from_str_with_options(markdown, &options),
                Text::from_iter([
                    Line::from_iter([
                        Span::raw(">"),
                        Span::raw(" "),
                        Span::styled("!! Note", style.bold()),
                    ])
                    .style(style),
                    Line::from_iter([Span::raw(">"), Span::raw(" "), Span::raw("Body")])
                        .style(style),
                ])
            );
        }

        #[rstest]
        fn empty_alert_icon_suppresses_icon_and_separator(_with_tracing: DefaultGuard) {
            let markdown = indoc! {"
                > [!CAUTION]
                > Body
            "};
            let options = Options::new(CustomAlertHeadingStyleSheet);
            let style = DefaultStyleSheet.alert(AlertKind::Caution);

            assert_eq!(
                from_str_with_options(markdown, &options),
                Text::from_iter([
                    Line::from_iter([
                        Span::raw(">"),
                        Span::raw(" "),
                        Span::styled("Caution", style.bold()),
                    ])
                    .style(style),
                    Line::from_iter([Span::raw(">"), Span::raw(" "), Span::raw("Body")])
                        .style(style),
                ])
            );
        }

        #[rstest]
        fn custom_alert_label_replaces_default(_with_tracing: DefaultGuard) {
            let markdown = indoc! {"
                > [!TIP]
                > Body
            "};
            let options = Options::new(CustomAlertHeadingStyleSheet);
            let style = DefaultStyleSheet.alert(AlertKind::Tip);

            assert_eq!(
                from_str_with_options(markdown, &options),
                Text::from_iter([
                    Line::from_iter([
                        Span::raw(">"),
                        Span::raw(" "),
                        Span::styled("💡 Hint", style.bold()),
                    ])
                    .style(style),
                    Line::from_iter([Span::raw(">"), Span::raw(" "), Span::raw("Body")])
                        .style(style),
                ])
            );
        }

        #[rstest]
        fn empty_alert_label_suppresses_label_and_separator(_with_tracing: DefaultGuard) {
            let markdown = indoc! {"
                > [!IMPORTANT]
                > Body
            "};
            let options = Options::new(CustomAlertHeadingStyleSheet);
            let style = DefaultStyleSheet.alert(AlertKind::Important);

            assert_eq!(
                from_str_with_options(markdown, &options),
                Text::from_iter([
                    Line::from_iter([
                        Span::raw(">"),
                        Span::raw(" "),
                        Span::styled("❗", style.bold()),
                    ])
                    .style(style),
                    Line::from_iter([Span::raw(">"), Span::raw(" "), Span::raw("Body")])
                        .style(style),
                ])
            );
        }

        #[rstest]
        fn ordinary_blockquote_keeps_standard_prefix(_with_tracing: DefaultGuard) {
            let style = DefaultStyleSheet.blockquote();
            assert_eq!(
                from_str("> Ordinary"),
                Text::from(Line::from_iter([">", " ", "Ordinary"]).style(style))
            );
        }

        #[rstest]
        fn nested_blockquote_keeps_each_standard_prefix(_with_tracing: DefaultGuard) {
            let markdown = indoc! {"
                > Parent
                >> Child
            "};
            let style = DefaultStyleSheet.blockquote();

            assert_eq!(
                from_str(markdown),
                Text::from_iter([
                    Line::from_iter([">", " ", "Parent"]).style(style),
                    Line::from_iter([">", " "]).style(style),
                    Line::from_iter([">", ">", " ", "Child"]).style(style),
                ])
            );
        }

        #[rstest]
        fn alert_preserves_nested_blockquote(_with_tracing: DefaultGuard) {
            let markdown = indoc! {"
                > [!NOTE]
                > Parent
                >> Child
            "};
            let alert_style = Style::new().blue();
            let blockquote_style = DefaultStyleSheet.blockquote();

            assert_eq!(
                from_str(markdown),
                Text::from_iter([
                    Line::from_iter([
                        Span::raw(">"),
                        Span::raw(" "),
                        Span::styled("\u{2139}\u{FE0F} Note", alert_style.bold()),
                    ])
                    .style(alert_style),
                    Line::from_iter([Span::raw(">"), Span::raw(" "), Span::raw("Parent")])
                        .style(alert_style),
                    Line::from_iter([Span::raw(">"), Span::raw(" ")]).style(alert_style),
                    Line::from_iter([
                        Span::raw(">"),
                        Span::raw(">"),
                        Span::raw(" "),
                        Span::raw("Child"),
                    ])
                    .style(blockquote_style),
                ])
            );
        }
    }

    mod blockquote {
        use pretty_assertions::assert_eq;
        use ratatui::style::Color;

        use super::*;

        const STYLE: Style = Style::new().fg(Color::Green);

        /// I was having difficulty getting the right number of newlines between paragraphs, so this
        /// test is to help debug and ensure that.
        #[rstest]
        fn after_paragraph(_with_tracing: DefaultGuard) {
            let markdown = indoc! {"
                Hello, world!

                > Blockquote
            "};

            assert_eq!(
                from_str(markdown),
                Text::from_iter([
                    Line::from("Hello, world!"),
                    Line::default(),
                    Line::from_iter([">", " ", "Blockquote"]).style(STYLE),
                ])
            );
        }

        #[rstest]
        fn style_does_not_leak_into_following_paragraph(_with_tracing: DefaultGuard) {
            let markdown = indoc! {"
                > Blockquote

                After
            "};

            assert_eq!(
                from_str(markdown),
                Text::from_iter([
                    Line::from_iter([">", " ", "Blockquote"]).style(STYLE),
                    Line::default(),
                    Line::from("After"),
                ])
            );
        }

        #[rstest]
        fn single(_with_tracing: DefaultGuard) {
            assert_eq!(
                from_str("> Blockquote"),
                Text::from(Line::from_iter([">", " ", "Blockquote"]).style(STYLE))
            );
        }

        #[rstest]
        fn soft_break(_with_tracing: DefaultGuard) {
            let markdown = indoc! {"
                > Blockquote 1
                > Blockquote 2
            "};

            assert_eq!(
                from_str(markdown),
                Text::from(
                    Line::from_iter([">", " ", "Blockquote 1", " ", "Blockquote 2"]).style(STYLE)
                )
            );
        }

        #[rstest]
        fn multiple(_with_tracing: DefaultGuard) {
            let markdown = indoc! {"
                > Blockquote 1
                >
                > Blockquote 2
            "};

            assert_eq!(
                from_str(markdown),
                Text::from_iter([
                    Line::from_iter([">", " ", "Blockquote 1"]).style(STYLE),
                    Line::from_iter([">", " "]).style(STYLE),
                    Line::from_iter([">", " ", "Blockquote 2"]).style(STYLE),
                ])
            );
        }

        #[rstest]
        fn multiple_with_break(_with_tracing: DefaultGuard) {
            let markdown = indoc! {"
                > Blockquote 1

                > Blockquote 2
            "};

            assert_eq!(
                from_str(markdown),
                Text::from_iter([
                    Line::from_iter([">", " ", "Blockquote 1"]).style(STYLE),
                    Line::default(),
                    Line::from_iter([">", " ", "Blockquote 2"]).style(STYLE),
                ])
            );
        }

        #[rstest]
        fn nested(_with_tracing: DefaultGuard) {
            let markdown = indoc! {"
                > Blockquote 1
                >> Nested Blockquote
            "};

            assert_eq!(
                from_str(markdown),
                Text::from_iter([
                    Line::from_iter([">", " ", "Blockquote 1"]).style(STYLE),
                    Line::from_iter([">", " "]).style(STYLE),
                    Line::from_iter([">", ">", " ", "Nested Blockquote"]).style(STYLE),
                ])
            );
        }
    }
}
