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
    use ratatui_core::style::Stylize;
    use ratatui_core::text::{Line, Span, Text};
    use rstest::rstest;

    use super::*;
    use crate::renderer::test_support::{with_tracing, DefaultGuard};
    use crate::{from_str, from_str_with_options, DefaultStyleSheet, Options};

    mod gfm_alerts {
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
        #[case("NOTE")]
        #[case("TIP")]
        #[case("IMPORTANT")]
        #[case("WARNING")]
        #[case("CAUTION")]
        fn alert_kind_renders_exact_output(_with_tracing: DefaultGuard, #[case] marker: &str) {
            insta::assert_debug_snapshot!(
                format!("alert_{marker}"),
                from_str(&formatdoc! {"
                    > [!{marker}]
                    > Body
                "})
            );
        }

        #[rstest]
        fn custom_alert_style_applies_to_header_and_body(_with_tracing: DefaultGuard) {
            let options = Options::new(CustomAlertStyleSheet);

            insta::assert_debug_snapshot!(from_str_with_options(indoc!("
                > [!NOTE]
                > Body
            "), &options), @r#"
            Text::from_iter([
                Line::from_iter([
                    Span::from(">"),
                    Span::from(" "),
                    Span::from("ℹ\u{fe0f} Note").on_red().bold(),
                ]).on_red(),
                Line::from_iter([
                    Span::from(">"),
                    Span::from(" "),
                    Span::from("Body"),
                ]).on_red(),
            ])
            "#);
        }

        #[rstest]
        fn custom_alert_icon_replaces_default(_with_tracing: DefaultGuard) {
            let options = Options::new(CustomAlertHeadingStyleSheet);

            insta::assert_debug_snapshot!(from_str_with_options(indoc!("
                > [!NOTE]
                > Body
            "), &options), @r#"
            Text::from_iter([
                Line::from_iter([
                    Span::from(">"),
                    Span::from(" "),
                    Span::from("!! Note").blue().bold(),
                ]).blue(),
                Line::from_iter([
                    Span::from(">"),
                    Span::from(" "),
                    Span::from("Body"),
                ]).blue(),
            ])
            "#);
        }

        #[rstest]
        fn empty_alert_icon_suppresses_icon_and_separator(_with_tracing: DefaultGuard) {
            let options = Options::new(CustomAlertHeadingStyleSheet);

            insta::assert_debug_snapshot!(from_str_with_options(indoc!("
                > [!CAUTION]
                > Body
            "), &options), @r#"
            Text::from_iter([
                Line::from_iter([
                    Span::from(">"),
                    Span::from(" "),
                    Span::from("Caution").red().bold(),
                ]).red(),
                Line::from_iter([
                    Span::from(">"),
                    Span::from(" "),
                    Span::from("Body"),
                ]).red(),
            ])
            "#);
        }

        #[rstest]
        fn custom_alert_label_replaces_default(_with_tracing: DefaultGuard) {
            let options = Options::new(CustomAlertHeadingStyleSheet);

            insta::assert_debug_snapshot!(from_str_with_options(indoc!("
                > [!TIP]
                > Body
            "), &options), @r#"
            Text::from_iter([
                Line::from_iter([
                    Span::from(">"),
                    Span::from(" "),
                    Span::from("💡 Hint").green().bold(),
                ]).green(),
                Line::from_iter([
                    Span::from(">"),
                    Span::from(" "),
                    Span::from("Body"),
                ]).green(),
            ])
            "#);
        }

        #[rstest]
        fn empty_alert_label_suppresses_label_and_separator(_with_tracing: DefaultGuard) {
            let options = Options::new(CustomAlertHeadingStyleSheet);

            insta::assert_debug_snapshot!(from_str_with_options(indoc!("
                > [!IMPORTANT]
                > Body
            "), &options), @r#"
            Text::from_iter([
                Line::from_iter([
                    Span::from(">"),
                    Span::from(" "),
                    Span::from("❗").magenta().bold(),
                ]).magenta(),
                Line::from_iter([
                    Span::from(">"),
                    Span::from(" "),
                    Span::from("Body"),
                ]).magenta(),
            ])
            "#);
        }

        #[rstest]
        fn ordinary_blockquote_keeps_standard_prefix(_with_tracing: DefaultGuard) {
            assert_eq!(
                from_str("> Ordinary"),
                Text::from(
                    Line::from_iter([Span::from(">"), Span::from(" "), Span::from("Ordinary")])
                        .green()
                )
            );
        }

        #[rstest]
        fn nested_blockquote_keeps_each_standard_prefix(_with_tracing: DefaultGuard) {
            insta::assert_debug_snapshot!(
                "nested_blockquote_keeps_each_standard_prefix",
                from_str(indoc!(
                    "
                    > Parent
                    >> Child
                "
                ))
            );
        }

        #[rstest]
        fn alert_preserves_nested_blockquote(_with_tracing: DefaultGuard) {
            insta::assert_debug_snapshot!(from_str(indoc!(
                "
                > [!NOTE]
                > Parent
                >> Child
            "
            )));
        }
    }

    mod blockquote {
        use super::*;
        use pretty_assertions::assert_eq;

        /// I was having difficulty getting the right number of newlines between paragraphs, so this
        /// test is to help debug and ensure that.
        #[rstest]
        fn after_paragraph(_with_tracing: DefaultGuard) {
            insta::assert_debug_snapshot!(from_str(indoc!("
                Hello, world!

                > Blockquote
            ")), @r#"
            Text::from_iter([
                Line::from("Hello, world!"),
                Line::default(),
                Line::from_iter([
                    Span::from(">"),
                    Span::from(" "),
                    Span::from("Blockquote"),
                ]).green(),
            ])
            "#);
        }

        #[rstest]
        fn style_does_not_leak_into_following_paragraph(_with_tracing: DefaultGuard) {
            insta::assert_debug_snapshot!(from_str(indoc!("
                > Blockquote

                After
            ")), @r#"
            Text::from_iter([
                Line::from_iter([
                    Span::from(">"),
                    Span::from(" "),
                    Span::from("Blockquote"),
                ]).green(),
                Line::default(),
                Line::from("After"),
            ])
            "#);
        }

        #[rstest]
        fn single(_with_tracing: DefaultGuard) {
            assert_eq!(
                from_str("> Blockquote"),
                Text::from(
                    Line::from_iter([Span::from(">"), Span::from(" "), Span::from("Blockquote")])
                        .green()
                )
            );
        }

        #[rstest]
        fn soft_break(_with_tracing: DefaultGuard) {
            assert_eq!(
                from_str(indoc! {"
                    > Blockquote 1
                    > Blockquote 2
                "}),
                Text::from(
                    Line::from_iter([
                        Span::from(">"),
                        Span::from(" "),
                        Span::from("Blockquote 1"),
                        Span::from(" "),
                        Span::from("Blockquote 2")
                    ])
                    .green()
                )
            );
        }

        #[rstest]
        fn multiple(_with_tracing: DefaultGuard) {
            insta::assert_debug_snapshot!(from_str(indoc!(
                "
                > Blockquote 1
                >
                > Blockquote 2
            "
            )));
        }

        #[rstest]
        fn multiple_with_break(_with_tracing: DefaultGuard) {
            insta::assert_debug_snapshot!(from_str(indoc!(
                "
                > Blockquote 1

                > Blockquote 2
            "
            )));
        }

        #[rstest]
        fn nested(_with_tracing: DefaultGuard) {
            insta::assert_debug_snapshot!(from_str(indoc!(
                "
                > Blockquote 1
                >> Nested Blockquote
            "
            )));
        }
    }
}
