//! Shared chrome for the input-anchored popups (`/`-autocomplete and the
//! `/model` picker).
//!
//! Both popups want the same thing: a bordered, titled box pinned directly
//! above the input box, using the input-box theme colors. This module owns
//! that shared logic so the two call sites stay in lockstep — see
//! [`command_popup`](super::command_popup) and
//! [`model_popup`](super::model_popup). The centered `/help` overlay reuses
//! [`block`] for its frame but does its own centering.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders};

use crate::theme;

/// Border rows a popup adds around its content (top + bottom edge).
pub const BORDER_HEIGHT: u16 = 2;

/// Rect for a popup holding `content_rows` rows of content, pinned just above
/// `input_area`.
///
/// `input_area` must be the **input box** (not a filler region): the popup's
/// bottom edge lands on the input box's top edge, so it always reads as
/// "right above where I'm typing" regardless of how much empty space sits
/// above the input. Falls back to anchoring below — clamped into the frame —
/// only when there genuinely isn't room above (input box near the top).
pub fn anchored_above(frame: &Frame, input_area: Rect, content_rows: u16) -> Rect {
    let height = content_rows + BORDER_HEIGHT;
    let width = input_area.width;

    if input_area.y >= height {
        Rect::new(input_area.x, input_area.y - height, width, height)
    } else {
        let frame_area = frame.area();
        let y = (input_area.y + input_area.height)
            .min(frame_area.y + frame_area.height.saturating_sub(height));
        Rect::new(input_area.x, y, width, height)
    }
}

/// The bordered, titled block shared by every popup — input-box border color
/// and background so the popup reads as an extension of the input chrome.
pub fn block(title: String) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(theme::INPUT_BORDER)
        .style(Style::default().bg(theme::INPUT_BG))
        .title(title)
}
