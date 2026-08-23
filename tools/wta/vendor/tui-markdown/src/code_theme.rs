//! Syntax-highlighting themes for fenced code blocks.
//!
//! [`CodeTheme`] represents bundled themes converted from [`BuiltinCodeTheme`] and TextMate themes
//! parsed with [`CodeTheme::from_textmate`] or loaded with [`CodeTheme::from_file`]. Select any of
//! them with [`Options::code_theme`](crate::Options::code_theme). The renderer consults the theme
//! when a fenced code block names a recognized language.
//!
//! A configured [`CodeTheme`] owns its theme data. [`Options`](crate::Options) stores no theme by
//! default; the renderer instead borrows a shared [`CodeTheme`] for
//! [`BuiltinCodeTheme::Base16OceanDark`] when it first encounters a recognized fenced language.

use std::error::Error;
use std::fmt;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use syntect::highlighting::{Theme, ThemeSet};

/// An owned syntax-highlighting theme for fenced code blocks.
///
/// Convert a [`BuiltinCodeTheme`] into this type, parse TextMate source with
/// [`CodeTheme::from_textmate`], or load a TextMate file with [`CodeTheme::from_file`]. Then pass
/// the theme to [`Options::code_theme`](crate::Options::code_theme).
///
/// The renderer consults the theme only for fenced code blocks whose language is recognized.
///
/// This type hides the syntax-highlighting implementation so applications do not need to depend on
/// its types or version.
#[derive(Clone, Debug)]
pub struct CodeTheme {
    theme: Theme,
}

impl CodeTheme {
    /// Parses a TextMate syntax-highlighting theme.
    ///
    /// This constructor parses `source` immediately and does not access the filesystem. The
    /// returned theme owns the parsed data and does not borrow `source`. Applications can combine
    /// this method with [`include_str!`] to compile a theme into their binary.
    ///
    /// # Errors
    ///
    /// Returns [`CodeThemeLoadError`] when `source` is not a valid TextMate theme.
    pub fn from_textmate(source: &str) -> Result<Self, CodeThemeLoadError> {
        let mut reader = Cursor::new(source);
        let theme = ThemeSet::load_from_reader(&mut reader)
            .map_err(|source| CodeThemeLoadError { path: None, source })?;
        Ok(Self { theme })
    }

    /// Loads a TextMate syntax-highlighting theme from disk.
    ///
    /// This function reads and parses the file synchronously. The resulting `CodeTheme` owns the
    /// parsed theme, so rendering does not access the file again. Use [`CodeTheme::from_textmate`]
    /// with [`include_str!`] when the theme should be compiled into the application instead.
    ///
    /// # Errors
    ///
    /// Returns [`CodeThemeLoadError`] when the file cannot be read or its contents are not a valid
    /// TextMate theme. The error message includes the requested path.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use tui_markdown::{CodeTheme, Options};
    ///
    /// let theme = CodeTheme::from_file("themes/solarized.tmTheme")?;
    /// let options = Options::default().code_theme(theme);
    /// # Ok::<(), tui_markdown::CodeThemeLoadError>(())
    /// ```
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, CodeThemeLoadError> {
        let path = path.as_ref();
        let theme = ThemeSet::get_theme(path).map_err(|source| CodeThemeLoadError {
            path: Some(path.to_owned()),
            source,
        })?;
        Ok(Self { theme })
    }
}

/// A syntax-highlighting theme bundled with tui-markdown.
///
/// Pass a variant directly to [`Options::code_theme`](crate::Options::code_theme), or convert it
/// into an owned [`CodeTheme`].
#[non_exhaustive]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinCodeTheme {
    /// The dark Base16 Eighties theme.
    Base16EightiesDark,
    /// The dark Base16 Mocha theme.
    Base16MochaDark,
    /// The default dark Base16 Ocean theme.
    #[default]
    Base16OceanDark,
    /// The light Base16 Ocean theme.
    Base16OceanLight,
    /// The light Inspired GitHub theme.
    InspiredGitHub,
    /// The dark Solarized theme.
    SolarizedDark,
    /// The light Solarized theme.
    SolarizedLight,
}

impl BuiltinCodeTheme {
    fn syntect_name(self) -> &'static str {
        match self {
            Self::Base16EightiesDark => "base16-eighties.dark",
            Self::Base16MochaDark => "base16-mocha.dark",
            Self::Base16OceanDark => "base16-ocean.dark",
            Self::Base16OceanLight => "base16-ocean.light",
            Self::InspiredGitHub => "InspiredGitHub",
            Self::SolarizedDark => "Solarized (dark)",
            Self::SolarizedLight => "Solarized (light)",
        }
    }
}

impl From<BuiltinCodeTheme> for CodeTheme {
    fn from(theme: BuiltinCodeTheme) -> Self {
        let theme = builtin_theme(theme).clone();
        Self { theme }
    }
}

/// An error returned when a syntax-highlighting theme cannot be parsed or loaded.
///
/// Errors from [`CodeTheme::from_file`] include the requested path. Errors from
/// [`CodeTheme::from_textmate`] identify invalid TextMate source. [`Error::source`] provides the
/// underlying parsing error without making the parser part of tui-markdown's public API.
#[non_exhaustive]
#[derive(Debug)]
pub struct CodeThemeLoadError {
    path: Option<PathBuf>,
    source: syntect::LoadingError,
}

impl fmt::Display for CodeThemeLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(path) = &self.path {
            write!(
                formatter,
                "failed to load code theme from `{}`: {}",
                path.display(),
                self.source
            )
        } else {
            write!(
                formatter,
                "failed to parse TextMate code theme: {}",
                self.source
            )
        }
    }
}

impl Error for CodeThemeLoadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

/// Returns the syntax-highlighting data for a code theme.
pub fn theme(code_theme: &CodeTheme) -> &Theme {
    &code_theme.theme
}

/// Returns the lazily initialized default code theme.
///
/// The renderer calls this only after recognizing a fenced language, so ordinary Markdown and
/// unrecognized code fences do not initialize the bundled theme set.
pub fn default() -> &'static CodeTheme {
    &DEFAULT_THEME
}

fn builtin_theme(code_theme: BuiltinCodeTheme) -> &'static Theme {
    THEMES
        .themes
        .get(code_theme.syntect_name())
        .expect("every BuiltinCodeTheme variant must map to a bundled theme")
}

static DEFAULT_THEME: LazyLock<CodeTheme> = LazyLock::new(|| BuiltinCodeTheme::default().into());
static THEMES: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use indoc::indoc;
    use ratatui_core::style::Color;

    use crate::{from_str_with_options, Options};

    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/code_theme/fixtures")
            .join(name)
    }

    #[test]
    fn every_builtin_theme_can_be_selected() {
        let themes = [
            BuiltinCodeTheme::Base16EightiesDark,
            BuiltinCodeTheme::Base16MochaDark,
            BuiltinCodeTheme::Base16OceanDark,
            BuiltinCodeTheme::Base16OceanLight,
            BuiltinCodeTheme::InspiredGitHub,
            BuiltinCodeTheme::SolarizedDark,
            BuiltinCodeTheme::SolarizedLight,
        ];

        for built_in in themes {
            let code_theme = CodeTheme::from(built_in);
            let _ = theme(&code_theme);
        }
    }

    #[test]
    fn configured_theme_is_borrowed_directly() {
        let code_theme = CodeTheme::from(BuiltinCodeTheme::SolarizedDark);

        assert!(std::ptr::eq(theme(&code_theme), &code_theme.theme));
    }

    #[test]
    fn default_theme_is_shared() {
        assert!(std::ptr::eq(default(), default()));
    }

    #[test]
    fn loaded_theme_applies_its_foreground_color() {
        let theme = CodeTheme::from_file(fixture("custom.tmTheme")).unwrap();

        assert_eq!(
            rendered_keyword_foreground(theme),
            Some(Color::Rgb(255, 255, 255))
        );
    }

    #[test]
    fn embedded_theme_applies_its_foreground_color() {
        let source = include_str!("code_theme/fixtures/custom.tmTheme");
        let theme = CodeTheme::from_textmate(source).unwrap();

        assert_eq!(
            rendered_keyword_foreground(theme),
            Some(Color::Rgb(255, 255, 255))
        );
    }

    fn rendered_keyword_foreground(theme: CodeTheme) -> Option<Color> {
        let input = indoc! {"
            ```rust
            fn main() {}
            ```
        "};
        let options = Options::default().code_theme(theme);
        let rendered = from_str_with_options(input, &options);
        rendered
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content == "fn")
            .expect("Rust highlighting should emit the `fn` keyword")
            .style
            .fg
    }

    #[test]
    fn missing_theme_reports_its_path_and_read_error() {
        let path = fixture("missing.tmTheme");
        let error = CodeTheme::from_file(&path).unwrap_err();

        let prefix = format!("failed to load code theme from `{}`:", path.display());
        assert!(error.to_string().starts_with(&prefix));
        assert!(matches!(&error.source, syntect::LoadingError::Io(_)));
        assert!(error.source().is_some());
    }

    #[test]
    fn malformed_theme_reports_its_path_and_parse_error() {
        let path = fixture("invalid.tmTheme");
        let error = CodeTheme::from_file(&path).unwrap_err();

        let prefix = format!("failed to load code theme from `{}`:", path.display());
        assert!(error.to_string().starts_with(&prefix));
        assert!(matches!(
            &error.source,
            syntect::LoadingError::ReadSettings(_)
        ));
        assert!(error.source().is_some());
    }

    #[test]
    fn malformed_embedded_theme_reports_a_parse_error() {
        let error = CodeTheme::from_textmate("this is not a TextMate theme").unwrap_err();

        assert!(error
            .to_string()
            .starts_with("failed to parse TextMate code theme:"));
        assert!(matches!(
            &error.source,
            syntect::LoadingError::ReadSettings(_)
        ));
        assert!(error.source().is_some());
    }
}
