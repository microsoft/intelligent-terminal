//! `/model` picker modal.
//!
//! Opened by the `/model` slash command (`App::cmd_model`), this overlay
//! lists the models the connected ACP agent advertised for the session and
//! lets the user pin *this pane* to one of them — a per-pane override that
//! wins over the global `acpModel` setting. Modeled on the slash-command
//! autocomplete popup (`command_popup.rs`): anchored above the input box,
//! arrow keys move the highlight, Enter commits, Esc dismisses (all handled
//! in `App::handle_key`).

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};

use crate::app::AcpModelInfo;
use crate::theme;

const POPUP_MAX_VISIBLE: usize = 8;
const POPUP_BORDER_HEIGHT: u16 = 2;
/// Marker drawn next to the model the pane is currently on.
const CURRENT_MARKER: &str = "● ";
const CURRENT_PAD: &str = "  ";

/// Per-frame state captured from the [`App`](crate::app::App).
pub struct ModelPopupState<'a> {
    pub models: &'a [AcpModelInfo],
    pub selected: usize,
    /// Id of the model the pane is currently effectively on, if any — drawn
    /// with a leading marker so the user can see "where we are".
    pub current_id: Option<&'a str>,
}

/// Render the model picker just above `input_area`, falling back to below
/// when there isn't room. No-op on an empty model list.
pub fn render_popup(frame: &mut Frame, state: ModelPopupState<'_>, input_area: Rect) {
    if state.models.is_empty() {
        return;
    }

    let visible = state.models.len().min(POPUP_MAX_VISIBLE) as u16;
    let height = visible + POPUP_BORDER_HEIGHT;
    let width = input_area.width;

    // Prefer above; fall back to below if there's no room.
    let area = if input_area.y >= height {
        Rect::new(input_area.x, input_area.y - height, width, height)
    } else {
        let frame_area = frame.area();
        let y = (input_area.y + input_area.height)
            .min(frame_area.y + frame_area.height.saturating_sub(height));
        Rect::new(input_area.x, y, width, height)
    };

    frame.render_widget(Clear, area);

    let items: Vec<ListItem> = state
        .models
        .iter()
        .map(|m| {
            let is_current = state.current_id == Some(m.id.as_str());
            let marker = if is_current { CURRENT_MARKER } else { CURRENT_PAD };
            let mut spans = vec![
                Span::styled(format!(" {}{}", marker, m.name), theme::INPUT_TEXT),
            ];
            // Show the raw id when it differs from the display name, plus the
            // optional one-line description, both dimmed.
            if m.id != m.name {
                spans.push(Span::styled(format!("  ({})", m.id), theme::DIM));
            }
            if let Some(desc) = m.description.as_deref().filter(|d| !d.is_empty()) {
                spans.push(Span::styled(format!("  — {}", desc), theme::DIM));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::INPUT_BORDER)
        .style(Style::default().bg(theme::INPUT_BG))
        .title(t!("model_picker.title").into_owned());

    let list = List::new(items)
        .block(block)
        .highlight_style(theme::SELECTED)
        .highlight_symbol("> ");

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected.min(state.models.len() - 1)));

    frame.render_stateful_widget(list, area, &mut list_state);
}
