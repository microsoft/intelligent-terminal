//! Markdown inline and fenced code rendering.
//!
//! Inline code and unrecognized fences use the style sheet's code style. With `highlight-code`
//! enabled, a recognized fenced language uses the selected syntax-highlighting theme.

#[cfg(feature = "highlight-code")]
use std::sync::LazyLock;

use pulldown_cmark::{CodeBlockKind, CowStr, Event};
#[cfg(feature = "highlight-code")]
use ratatui_core::style::{Color, Style};
use ratatui_core::text::{Line, Span};
#[cfg(feature = "highlight-code")]
use syntect::{easy::HighlightLines, parsing::SyntaxSet};
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
        self.code_line = Some(String::new());
    }

    pub fn end_codeblock(&mut self) {
        if let Some(line) = self.code_line.take().filter(|line| !line.is_empty()) {
            self.render_code_line(&line);
        }
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

    pub fn append_code_block_text(&mut self, text: &str) {
        let Some(mut line) = self.code_line.take() else {
            return;
        };
        for part in text.split_inclusive('\n') {
            line.push_str(part);
            if part.ends_with('\n') {
                self.render_code_line(&line);
                line.clear();
            }
        }
        self.code_line = Some(line);
    }

    fn render_code_line(&mut self, line: &str) {
        if !self.push_highlighted_line(line) {
            let content = line.strip_suffix('\n').unwrap_or(line);
            let content = content.strip_suffix('\r').unwrap_or(content);
            let style = self.inline_styles.last().copied().unwrap_or_default();
            #[cfg(feature = "highlight-code")]
            let style = style.patch(self.styles.code());
            self.push_line(Line::from(Span::styled(content.to_owned(), style)));
        }
        self.needs_newline = false;
    }

    #[cfg(feature = "highlight-code")]
    pub fn with_code_theme(mut self, theme: Option<&'theme CodeTheme>) -> Self {
        self.code_theme = theme;
        self
    }

    #[cfg(feature = "highlight-code")]
    fn push_highlighted_line(&mut self, line: &str) -> bool {
        let Some(highlighter) = &mut self.code_highlighter else {
            return false;
        };
        // Preserve the existing behavior: highlighting failures omit the line.
        let Ok(parts) = highlighter.highlight_line(line, &SYNTAX_SET) else {
            return true;
        };
        let spans = parts
            .into_iter()
            .filter_map(|(style, content)| {
                // Syntect needs line endings for parsing, but Ratatui lines do not include them.
                let content = content.strip_suffix('\n').unwrap_or(content);
                let content = content.strip_suffix('\r').unwrap_or(content);
                if content.is_empty() {
                    return None;
                }
                let foreground = style.foreground;
                let background = style.background;
                let style = Style::default()
                    .fg(Color::Rgb(foreground.r, foreground.g, foreground.b))
                    .bg(Color::Rgb(background.r, background.g, background.b));
                Some(Span::styled(content.to_owned(), style))
            })
            .collect::<Vec<_>>();
        self.push_line(Line::from(spans));
        true
    }

    #[cfg(not(feature = "highlight-code"))]
    fn push_highlighted_line(&mut self, _line: &str) -> bool {
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
mod tests;
