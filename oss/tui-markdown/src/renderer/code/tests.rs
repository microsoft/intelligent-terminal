use indoc::indoc;
use pretty_assertions::assert_eq;
use pulldown_cmark::Parser;
use ratatui_core::style::Stylize;
use ratatui_core::text::{Line, Span, Text};
use rstest::rstest;

use super::*;
use crate::renderer::test_support::{with_tracing, DefaultGuard};
use crate::{from_str, from_str_with_options, Options};

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

    insta::assert_snapshot!(highlighted_code, @r#"
    ```rust
    fn main() {
        println!("Hello, highlighted code!");
    }
    ```
    "#);
    insta::assert_debug_snapshot!("highlighted_code-2", highlighted_code);
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

    insta::assert_snapshot!(highlighted_code_indented, @r#"
    ```rust
    fn main() {
        // This is a comment
        HelloWorldBuilder::new()
            .with_text("Hello, highlighted code!")
            .build()
            .show();
                    
    }
    ```
    "#);
    insta::assert_debug_snapshot!(
        "highlighted_code_with_indentation-2",
        highlighted_code_indented
    );
}

#[cfg_attr(feature = "highlight-code", ignore)]
#[rstest]
fn unhighlighted_code(_with_tracing: DefaultGuard) {
    // Assert no extra newlines are added
    let unhighlighted_code = from_str(indoc! {"
        ```rust
        fn main() {
            println!(\"Hello, unhighlighted code!\");
        }
        ```"});

    insta::assert_snapshot!(unhighlighted_code, @r#"
    ```rust
    fn main() {
        println!("Hello, unhighlighted code!");
    }
    ```
    "#);

    // Also verify line and span styles.
    insta::assert_debug_snapshot!(unhighlighted_code, @r#"
    Text::from_iter([
        Line::from("```rust").white().on_black(),
        Line::from("fn main() {").white().on_black(),
        Line::from("    println!("Hello, unhighlighted code!");").white().on_black(),
        Line::from("}").white().on_black(),
        Line::from("```").white().on_black(),
    ])
    "#);
}

#[rstest]
fn inline_code(_with_tracing: DefaultGuard) {
    let text = from_str("Example of `Inline code`");
    insta::assert_snapshot!(text, @"Example of Inline code");

    assert_eq!(
        text,
        Text::from(Line::from_iter([
            Span::from("Example of "),
            Span::from("Inline code").white().on_black()
        ]))
    );
}

#[rstest]
fn fenced_code_style_does_not_leak_into_following_paragraph(_with_tracing: DefaultGuard) {
    let text = from_str(indoc! {"
                ```rust
                fn main() {}
                ```

                After
    "});

    assert_eq!(text.lines.last(), Some(&Line::from("After")));
}

#[rstest]
fn custom_code_block_fence(_with_tracing: DefaultGuard) {
    let options = Options::new(CustomCodeBlockFence("~~~"));

    let text = from_str_with_options("```not-a-language\ncode\n```", &options);
    assert_eq!(text.to_string(), "~~~not-a-language\ncode\n~~~");
}

#[rstest]
fn empty_code_block_fence_preserves_spacing(_with_tracing: DefaultGuard) {
    let options = Options::new(CustomCodeBlockFence(""));

    let text = from_str_with_options("Before\n\n```not-a-language\ncode\n```\n\nAfter", &options);
    assert_eq!(text.to_string(), "Before\n\ncode\n\nAfter");
}

#[rstest]
fn empty_code_block_fence_applies_to_indented_code(_with_tracing: DefaultGuard) {
    let options = Options::new(CustomCodeBlockFence(""));

    let text = from_str_with_options("    indented code", &options);
    assert_eq!(text.to_string(), "indented code");
}

#[cfg(feature = "highlight-code")]
#[rstest]
fn empty_code_block_fence_applies_to_highlighted_code(_with_tracing: DefaultGuard) {
    let options = Options::new(CustomCodeBlockFence(""));

    let text = from_str_with_options("```rust\nfn main() {}\n```", &options);

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

#[rstest]
#[case::plain("not-a-language")]
#[case::unlabelled("")]
#[case::highlighted("rust")]
fn code_block_preserves_literal_lines(
    #[case] language: &str,
    #[values("\n", "\r\n")] newline: &str,
) {
    let options = Options::new(CustomCodeBlockFence(""));
    let source = format!(
        "Before{newline}{newline}```{language}{newline}let first = 1;{newline}{newline}let last = 2;{newline}```{newline}{newline}After"
    );
    let text = from_str_with_options(&source, &options);
    assert_eq!(
        text.to_string(),
        "Before\n\nlet first = 1;\n\nlet last = 2;\n\nAfter"
    );
    assert_eq!(
        text,
        from_str_with_options(&source.replace("\r\n", "\n"), &options)
    );
}

#[rstest]
#[case::empty("", "")]
#[case::blank_line("\n", "")]
#[case::leading_blank("\nfirst\n", "\nfirst")]
#[case::trailing_blank("first\n\n", "first\n")]
#[case::multiple_blank_lines("first\n\n\nlast\n", "first\n\n\nlast")]
fn plain_code_keeps_empty_line_positions(
    #[case] body: &str,
    #[case] expected: &str,
    #[values("\n", "\r\n")] newline: &str,
) {
    let options = Options::new(CustomCodeBlockFence(""));
    let source = format!("```not-a-language\n{body}```").replace('\n', newline);
    assert_eq!(
        from_str_with_options(&source, &options).to_string(),
        expected
    );
}

#[rstest]
#[case::plain("not-a-language")]
#[case::highlighted("rust")]
fn unclosed_code_keeps_the_final_line(
    #[case] language: &str,
    #[values("\n", "\r\n")] newline: &str,
) {
    let options = Options::new(CustomCodeBlockFence(""));
    let source = format!("```{language}{newline}let first = 1;{newline}let last = 2;");
    let text = from_str_with_options(&source, &options);
    assert_eq!(text.to_string(), "let first = 1;\nlet last = 2;");
    assert_eq!(
        text,
        from_str_with_options(&source.replace("\r\n", "\n"), &options)
    );
}

#[rstest]
fn indented_code_preserves_its_blank_line(#[values("\n", "\r\n")] newline: &str) {
    let options = Options::new(CustomCodeBlockFence(""));
    let source = "Before\n\n    first\n\n    last\n\nAfter".replace('\n', newline);
    assert_eq!(
        from_str_with_options(&source, &options).to_string(),
        "Before\n\nfirst\n\nlast\n\nAfter"
    );
}

#[cfg(feature = "highlight-code")]
#[rstest]
#[case::comment("/* first\n\nlast */\n")]
#[case::raw_string("let value = r#\"first\n\nlast\"#;\n")]
fn highlighted_code_keeps_multiline_state(
    #[case] body: &str,
    #[values("\n", "\r\n")] newline: &str,
) {
    let options = Options::new(CustomCodeBlockFence(""));
    let lf = format!("```rust\n{body}```");
    let source = lf.replace('\n', newline);
    let text = from_str_with_options(&source, &options);
    assert_eq!(text.to_string(), body.strip_suffix('\n').unwrap());
    assert_eq!(text, from_str_with_options(&lf, &options));
}

#[rstest]
#[case::literal_spaces("first  second")]
#[case::lf("first\nsecond")]
#[case::crlf("first\r\nsecond")]
fn inline_code_preserves_the_parsers_space_normalization(#[case] code: &str) {
    let source = format!("Before `{code}` after");
    let parsed = Parser::new(&source)
        .find_map(|event| match event {
            Event::Code(code) => Some(code),
            _ => None,
        })
        .expect("the fixture contains an inline code span");
    assert_eq!(
        from_str(&source).to_string(),
        format!("Before {parsed} after")
    );
}

#[rstest]
fn visible_code_fences_preserve_blank_lines(#[values("\n", "\r\n")] newline: &str) {
    let markdown = indoc! {"
        Before

        ```

        first

        last

        ```

        After"};
    let source = markdown.replace('\n', newline);

    let text = from_str(&source);

    assert_eq!(text.to_string(), markdown);
    assert_eq!(text, from_str(markdown));
}

#[rstest]
fn quoted_code_preserves_prefixes_and_blank_lines(#[values("\n", "\r\n")] newline: &str) {
    let markdown = indoc! {"
        > ```
        > first
        >
        > last
        > ```

        After"};
    let source = markdown.replace('\n', newline);

    let text = from_str(&source);

    assert_eq!(
        text.to_string(),
        indoc! {"
        > ```
        > first
        > 
        > last
        > ```

        After"}
    );
    assert_eq!(text, from_str(markdown));
}

#[rstest]
fn list_code_preserves_blank_lines(#[values("\n", "\r\n")] newline: &str) {
    let markdown = indoc! {"
        - Before

          ```
          first

          last
          ```

        After"};
    let source = markdown.replace('\n', newline);

    let text = from_str(&source);

    assert_eq!(
        text.to_string(),
        indoc! {"
        - Before

        ```
        first

        last
        ```

        After"}
    );
    assert_eq!(text, from_str(markdown));
}

#[cfg(feature = "highlight-code")]
#[rstest]
fn multiline_comment_keeps_comment_color_after_blank_line(#[values("\n", "\r\n")] newline: &str) {
    use ratatui_core::style::Color;

    let markdown = indoc! {"
        ```rust
        /* first

        last */
        ```"};
    let source = markdown.replace('\n', newline);

    let options = Options::default().code_theme(crate::BuiltinCodeTheme::Base16OceanDark);
    let text = from_str_with_options(&source, &options);

    assert_eq!(text.to_string(), markdown);
    // Base16OceanDark's comment color must continue after the empty line.
    assert_eq!(text.lines[3].spans[0].content, "last ");
    assert_eq!(
        text.lines[3].spans[0].style.fg,
        Some(Color::Rgb(101, 115, 126))
    );
}

#[rstest]
fn visible_rust_fences_preserve_blank_lines(#[values("\n", "\r\n")] newline: &str) {
    let markdown = indoc! {"
        Before

        ```rust

        let first = 1;

        let last = 2;

        ```

        After"};
    let source = markdown.replace('\n', newline);

    let text = from_str(&source);

    assert_eq!(text.to_string(), markdown);
    assert_eq!(text, from_str(markdown));
}
