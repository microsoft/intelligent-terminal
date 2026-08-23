//! Style sheet abstraction for tui-markdown.
//!
//! [`StyleSheet`] supplies the styles and alert text used while Markdown events are rendered.
//! Every choice has a default. Implementations only need to override the styles or text they want
//! to customize.
//!
//! [`DefaultStyleSheet`] is used by [`crate::Options::default`]. Applications can pass another
//! implementation to [`crate::Options::new`].

use ratatui_core::style::Style;

/// The kind of a GitHub Flavored Markdown alert.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AlertKind {
    /// Supplementary information.
    Note,
    /// Helpful advice.
    Tip,
    /// Information essential to success.
    Important,
    /// Urgent information that needs attention.
    Warning,
    /// A risk or negative outcome.
    Caution,
}

impl AlertKind {
    /// The canonical English label for this alert kind.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Note => "Note",
            Self::Tip => "Tip",
            Self::Important => "Important",
            Self::Warning => "Warning",
            Self::Caution => "Caution",
        }
    }
}

/// Visual styles and symbols consumed by the renderer.
///
/// Every method has a default, which [`DefaultStyleSheet`] uses unchanged. Implementations only
/// need to override the choices they want to customize.
pub trait StyleSheet: Clone + Send + Sync + 'static {
    /// Style for a Markdown heading.
    ///
    /// `level` is one-based (`1` for `# H1`, …).
    fn heading(&self, level: u8) -> Style {
        match level {
            1 => Style::new().on_cyan().bold().underlined(),
            2 => Style::new().cyan().bold(),
            3 => Style::new().cyan().bold().italic(),
            4 => Style::new().light_cyan().italic(),
            5 => Style::new().light_cyan().italic(),
            _ => Style::new().light_cyan().italic(),
        }
    }

    /// Style for inline `code` spans and fenced code blocks when syntax highlighting is disabled.
    fn code(&self) -> Style {
        Style::new().white().on_black()
    }

    /// Style for an inline or reference link's visible label and appended destination.
    fn link(&self) -> Style {
        Style::new().blue().underlined()
    }

    /// Base style applied to blockquotes (`>` prefix and body text).
    fn blockquote(&self) -> Style {
        Style::new().green()
    }

    /// Style for heading attribute metadata appended to the heading text.
    fn heading_meta(&self) -> Style {
        Style::new().dim()
    }

    /// Style for metadata blocks (front matter).
    fn metadata_block(&self) -> Style {
        Style::new().light_yellow()
    }

    /// Marker displayed before a Markdown heading.
    ///
    /// `level` is one-based (`1` for an H1, …). The renderer adds one separating space after a
    /// non-empty marker. Return an empty string to omit the marker and its separator.
    fn heading_marker(&self, level: u8) -> &str {
        match level {
            1 => "#",
            2 => "##",
            3 => "###",
            4 => "####",
            5 => "#####",
            _ => "######",
        }
    }

    /// Delimiter displayed above and below block code.
    ///
    /// The renderer appends fenced-code info to the opening delimiter. Return an empty string to
    /// omit both delimiter lines.
    fn code_block_fence(&self) -> &str {
        "```"
    }

    /// Style for raw HTML blocks and inline HTML tags.
    fn html(&self) -> Style {
        Style::new().dim()
    }

    /// Style for inline math (`$...$`).
    fn math_inline(&self) -> Style {
        Style::new().italic().magenta()
    }

    /// Style for display math (`$$...$$`).
    fn math_display(&self) -> Style {
        Style::new().magenta()
    }

    /// Style for footnote references, rendered as `[label]`.
    fn footnote_ref(&self) -> Style {
        Style::new().dim().italic()
    }

    /// Style for footnote definitions, including the `[label]: ` prefix.
    fn footnote_def(&self) -> Style {
        Style::new().dim()
    }

    /// Style for definition list terms.
    fn definition_term(&self) -> Style {
        Style::new().bold()
    }

    /// Style for definition list descriptions, including the `: ` prefix.
    fn definition_description(&self) -> Style {
        Style::default()
    }
    /// Style for a GFM alert heading and body.
    ///
    /// The generated icon and label are bold in addition to this base style.
    fn alert(&self, kind: AlertKind) -> Style {
        use ratatui_core::style::Color;

        match kind {
            AlertKind::Note => Style::new().fg(Color::Blue),
            AlertKind::Tip => Style::new().fg(Color::Green),
            AlertKind::Important => Style::new().fg(Color::Magenta),
            AlertKind::Warning => Style::new().fg(Color::Yellow),
            AlertKind::Caution => Style::new().fg(Color::Red),
        }
    }

    /// Icon displayed before a GFM alert label.
    ///
    /// Return an empty string to render the label without an icon.
    fn alert_icon(&self, kind: AlertKind) -> &str {
        match kind {
            AlertKind::Note => "\u{2139}\u{FE0F}",
            AlertKind::Tip => "\u{1F4A1}",
            AlertKind::Important => "\u{2757}",
            AlertKind::Warning => "\u{26A0}\u{FE0F}",
            AlertKind::Caution => "\u{1F534}",
        }
    }

    /// Label displayed after a GFM alert icon.
    ///
    /// Return an empty string to render the icon without a label.
    fn alert_label(&self, kind: AlertKind) -> &str {
        kind.label()
    }

    /// Style patched onto cells in the table header row.
    ///
    /// Properties set by this style override the same properties in an inline style. The style
    /// covers cell padding, while borders use [`Self::table_border`].
    fn table_header(&self) -> Style {
        Style::new().bold().cyan()
    }

    /// Style patched onto ordinary table cells.
    ///
    /// Properties set by this style override the same properties in an inline style. The style
    /// covers cell padding, while borders use [`Self::table_border`].
    fn table_cell(&self) -> Style {
        Style::default()
    }

    /// Style for the Unicode box-drawing characters around table cells.
    ///
    /// This changes the presentation of the borders, not the box-drawing characters themselves.
    fn table_border(&self) -> Style {
        Style::new().dark_gray()
    }

    /// Style for the `[img]` marker and text used to represent Markdown images.
    ///
    /// The default is dim and italic. The renderer patches this over any enclosing inline style.
    fn image_alt(&self) -> Style {
        Style::new().dim().italic()
    }
}

/// The default style set
///
/// This style sheet will be used by default if the user does not provide their own implementation.
///
/// Styles are:
/// - H1: white on cyan, bold, underlined
/// - H2: cyan, bold
/// - H3: cyan, bold, italic
/// - H4-H6: light cyan, italic
/// - code: white on black
/// - link: blue, underlined
/// - blockquote: green
/// - metadata block: light yellow
/// - heading markers: one to six `#` characters
/// - code block fences: three backticks
/// - raw HTML: dim
/// - inline math: magenta, italic
/// - display math: magenta
/// - footnote references: dim, italic
/// - footnote definitions: dim
/// - definition list terms: bold
/// - definition list descriptions: the surrounding style
/// - note alerts: blue
/// - tip alerts: green
/// - important alerts: magenta
/// - warning alerts: yellow
/// - caution alerts: red
/// - table headers: bold cyan
/// - table cells: the surrounding style
/// - table borders: dark gray
/// - image fallback text: dim and italic
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultStyleSheet;

impl StyleSheet for DefaultStyleSheet {}
