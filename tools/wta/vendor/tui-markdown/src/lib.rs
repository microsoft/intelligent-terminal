//! Convert Markdown into Ratatui [`Text`](ratatui_core::text::Text).
//!
//! [`from_str`] renders with the default styles and options. [`from_str_with_options`] accepts an
//! [`Options`] value for custom [`StyleSheet`] styles and symbols, image fallback mode, and, when
//! the `highlight-code` feature is enabled, syntax-highlighting theme.
//!
//! The returned text may borrow from the Markdown input. It contains terminal text and styles only;
//! image syntax produces a configurable text fallback and does not read or render image resources.
//!
//! # Markdown output
//!
//! Tables use Unicode box-drawing borders, terminal display widths, and the alignment declared by
//! the Markdown delimiter row. Raw HTML stays visible as literal text. Math retains its delimiters,
//! and images render as `[img]` followed by their description or destination.
//!
//! # Syntax highlighting
//!
//! The default `highlight-code` feature highlights fenced code blocks whose language is recognized.
//! It uses `Base16OceanDark` unless [`Options`] selects another [`CodeTheme`]. Themes can come from
//! the built-in set, TextMate source bundled with the application, or a TextMate file read before
//! rendering. Unrecognized code fences use [`StyleSheet::code`] instead.
#![cfg_attr(feature = "document-features", doc = "\n# Features")]
#![cfg_attr(feature = "document-features", doc = document_features::document_features!())]
//!
//! # Example
//!
//! ~~~
//! use ratatui::text::Text;
//! use tui_markdown::from_str;
//!
//! # fn draw(frame: &mut ratatui::Frame) {
//! let markdown = r#"
//! This is a simple markdown renderer for Ratatui.
//!
//! - List item 1
//! - List item 2
//!
//! ```rust
//! fn main() {
//!     println!("Hello, world!");
//! }
//! ```
//! "#;
//!
//! let text = from_str(markdown);
//! frame.render_widget(text, frame.area());
//! # }
//! ~~~

#[cfg(feature = "highlight-code")]
mod code_theme;
mod options;
mod renderer;
mod style_sheet;

#[doc(inline)]
#[cfg(feature = "highlight-code")]
pub use crate::code_theme::{BuiltinCodeTheme, CodeTheme, CodeThemeLoadError};
pub use crate::options::{ImageFallback, Options};
pub use crate::renderer::{from_str, from_str_with_options};
pub use crate::style_sheet::{AlertKind, DefaultStyleSheet, StyleSheet};
