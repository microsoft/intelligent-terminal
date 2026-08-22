//! Rendering configuration for tui-markdown.
//!
//! Options control the renderer's style sheet, image fallback content, and syntax-highlighting
//! theme. [`Options`] is non-exhaustive, allowing new rendering choices to be added without
//! breaking existing code.

#[cfg(feature = "highlight-code")]
use crate::CodeTheme;
use crate::{DefaultStyleSheet, StyleSheet};

/// Text used to represent Markdown images in rendered terminal output.
///
/// This option does not load or render image resources. It controls whether the text fallback
/// contains the image description, destination, or both. [`AltText`](Self::AltText) is the
/// default.
///
/// # Example
///
/// ```
/// use tui_markdown::{from_str_with_options, ImageFallback, Options};
///
/// let options = Options::default().image_fallback(ImageFallback::AltTextAndUrl);
/// let text = from_str_with_options("![Architecture diagram](diagram.png)", &options);
///
/// assert_eq!(
///     text.to_string(),
///     "[img] Architecture diagram (diagram.png)"
/// );
/// ```
#[non_exhaustive]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ImageFallback {
    /// Show `[img]` followed by the description, or the destination when the description is empty.
    #[default]
    AltText,
    /// Show `[img]` followed by the destination, ignoring the description.
    Url,
    /// Show `[img] {description} ({destination})`, omitting either value when it is empty.
    AltTextAndUrl,
}

/// Rendering options for [`crate::from_str_with_options`].
///
/// `S` is the style sheet consulted while Markdown events are rendered. [`Options::default`] uses
/// [`DefaultStyleSheet`]. Use [`Options::new`] to supply another [`StyleSheet`].
/// [`StyleSheet::heading_marker`] and [`StyleSheet::code_block_fence`] customize or hide the
/// corresponding presentation symbols.
///
/// # Example
///
/// ```
/// use tui_markdown::Options;
///
/// let options = Options::default();
///
/// // or with a custom style sheet
///
/// use ratatui_core::style::{Style, Stylize};
/// use tui_markdown::StyleSheet;
///
/// #[derive(Debug, Clone)]
/// struct MyStyleSheet;
///
/// impl StyleSheet for MyStyleSheet {
///     fn heading(&self, _level: u8) -> Style {
///         Style::new().bold()
///     }
/// }
///
/// let options = Options::new(MyStyleSheet);
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Options<S: StyleSheet = DefaultStyleSheet> {
    /// The [`StyleSheet`] implementation that will be consulted every time the renderer needs a
    /// style or symbol choice.
    pub(crate) styles: S,
    /// The content to render in place of images.
    pub(crate) image_fallback: ImageFallback,
    /// Explicit syntax-highlighting theme for fenced code blocks.
    ///
    /// When absent, the renderer uses the shared built-in default.
    #[cfg(feature = "highlight-code")]
    code_theme: Option<CodeTheme>,
}

impl<S: StyleSheet> Options<S> {
    /// Creates rendering options that use `styles`.
    ///
    /// Image fallback and syntax-highlighting settings retain their defaults.
    pub fn new(styles: S) -> Self {
        Self {
            styles,
            image_fallback: ImageFallback::default(),
            #[cfg(feature = "highlight-code")]
            code_theme: None,
        }
    }

    /// Selects the text used to represent Markdown images.
    ///
    /// See [`ImageFallback`] for the exact output of each mode.
    #[must_use]
    pub fn image_fallback(mut self, image_fallback: ImageFallback) -> Self {
        self.image_fallback = image_fallback;
        self
    }

    /// Selects the syntax-highlighting theme for fenced code blocks.
    ///
    /// By default, no explicit theme is stored and the renderer borrows its shared
    /// [`Base16OceanDark`](crate::BuiltinCodeTheme::Base16OceanDark) theme.
    /// Pass a [`BuiltinCodeTheme`](crate::BuiltinCodeTheme) directly, or pass an owned
    /// [`CodeTheme`]. Construct custom themes from TextMate source with
    /// [`CodeTheme::from_textmate`](crate::CodeTheme::from_textmate), or load a TextMate file with
    /// [`CodeTheme::from_file`](crate::CodeTheme::from_file). The selected theme applies when a
    /// fenced code block names a recognized language.
    ///
    /// # Example
    ///
    /// ```
    /// use tui_markdown::{BuiltinCodeTheme, Options};
    ///
    /// let options = Options::default().code_theme(BuiltinCodeTheme::SolarizedDark);
    /// ```
    #[cfg(feature = "highlight-code")]
    #[must_use]
    pub fn code_theme(mut self, code_theme: impl Into<CodeTheme>) -> Self {
        self.code_theme = Some(code_theme.into());
        self
    }

    /// Returns the explicitly configured syntax-highlighting theme.
    ///
    /// Returns `None` when the renderer will use the shared
    /// [`Base16OceanDark`](crate::BuiltinCodeTheme::Base16OceanDark) default.
    #[cfg(feature = "highlight-code")]
    #[must_use]
    pub fn selected_code_theme(&self) -> Option<&CodeTheme> {
        self.code_theme.as_ref()
    }
}

impl Default for Options<DefaultStyleSheet> {
    fn default() -> Self {
        Self::new(DefaultStyleSheet)
    }
}

#[cfg(test)]
mod tests {
    use ratatui_core::style::Style;

    use super::*;

    #[test]
    fn default() {
        let options: Options = Default::default();
        assert_eq!(
            options.styles.heading(1),
            Style::new().on_cyan().bold().underlined()
        );
    }

    #[test]
    fn custom_style_sheet() {
        #[derive(Debug, Clone)]
        struct CustomStyleSheet;

        impl StyleSheet for CustomStyleSheet {
            fn heading(&self, level: u8) -> Style {
                match level {
                    1 => Style::new().red().bold(),
                    _ => Style::new().green(),
                }
            }
        }

        let options = Options {
            styles: CustomStyleSheet,
            image_fallback: ImageFallback::default(),
            #[cfg(feature = "highlight-code")]
            code_theme: None,
        };

        assert_eq!(options.styles.heading(1), Style::new().red().bold());
        assert_eq!(options.styles.heading(2), Style::new().green());
        assert_eq!(options.styles.code(), Style::new().white().on_black());
        assert_eq!(options.styles.link(), Style::new().blue().underlined());
        assert_eq!(options.styles.blockquote(), Style::new().green());
        assert_eq!(options.styles.heading_meta(), Style::new().dim());
        assert_eq!(options.styles.metadata_block(), Style::new().light_yellow());
        assert_eq!(options.styles.image_alt(), Style::new().dim().italic());
    }

    #[test]
    fn image_fallback_defaults_to_alt_text() {
        let options = Options::default();

        assert_eq!(options.image_fallback, ImageFallback::AltText);
    }

    #[test]
    fn image_fallback_setter_updates_mode() {
        let options = Options::default().image_fallback(ImageFallback::AltTextAndUrl);

        assert_eq!(options.image_fallback, ImageFallback::AltTextAndUrl);
    }

    #[test]
    #[cfg(feature = "highlight-code")]
    fn default_has_no_explicit_code_theme() {
        let options: Options = Options::default();

        assert!(options.selected_code_theme().is_none());
    }

    #[test]
    #[cfg(feature = "highlight-code")]
    fn code_theme_selects_theme() {
        let options = Options::default().code_theme(crate::BuiltinCodeTheme::SolarizedDark);

        assert!(options.selected_code_theme().is_some());
    }
}
