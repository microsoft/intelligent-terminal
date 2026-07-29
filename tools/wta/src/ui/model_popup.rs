//! `/model` picker modal.
//!
//! Opened by the `/model` slash command (`App::cmd_model`), this overlay
//! lists the configured BYOK models. Cloud/native models are intentionally
//! omitted; restart-required choices remain visible but dimmed and must be
//! changed in Settings. Modeled on the
//! slash-command
//! autocomplete popup (`command_popup.rs`): anchored above the input box,
//! arrow keys move the highlight, Enter commits, Esc dismisses (all handled
//! in `App::handle_key`).

use ratatui::prelude::*;
use ratatui::widgets::{Clear, List, ListItem, ListState};

use super::popup;
use crate::app::AcpModelInfo;
use crate::theme;

const POPUP_MAX_VISIBLE: usize = 8;
/// Marker drawn next to the model the pane is currently on.
const CURRENT_MARKER: &str = "● ";
const CURRENT_PAD: &str = "  ";

/// Per-frame state captured from the [`App`](crate::app::App).
pub struct ModelPopupState<'a> {
    pub models: &'a [AcpModelInfo],
    pub selected: usize,
    pub pane_focused: bool,
    /// Id of the model the pane is currently effectively on, if any — drawn
    /// with a leading marker so the user can see "where we are".
    pub current_id: Option<&'a str>,
    /// Rows that require an agent restart and can only be changed in Settings.
    pub disabled: Vec<bool>,
}

/// Render the model picker just above `input_area`, falling back to below
/// when there isn't room. No-op on an empty model list.
pub fn render_popup(frame: &mut Frame, state: ModelPopupState<'_>, input_area: Rect) {
    if state.models.is_empty() {
        return;
    }

    let visible = state.models.len().min(POPUP_MAX_VISIBLE) as u16;
    let area = popup::anchored_above(frame, input_area, visible);

    frame.render_widget(Clear, area);

    let items: Vec<ListItem> = state
        .models
        .iter()
        .enumerate()
        .map(|(index, m)| {
            let is_current = state.current_id == Some(m.id.as_str());
            let disabled = state.disabled.get(index).copied().unwrap_or(false);
            let marker = if is_current {
                CURRENT_MARKER
            } else {
                CURRENT_PAD
            };
            let mut spans = vec![Span::styled(
                format!(" {marker}{}", m.name),
                if disabled {
                    theme::DIM
                } else {
                    theme::INPUT_TEXT
                },
            )];
            // Show the raw id when it differs from the display name, plus the
            // optional one-line description, both dimmed. Custom BYOK entries
            // need their selection ids visible because display names can match.
            if m.id != m.name {
                spans.push(Span::styled(format!("  ({})", m.id), theme::DIM));
            }
            if let Some(desc) = m.description.as_deref().filter(|d| !d.is_empty()) {
                spans.push(Span::styled(format!("  — {}", desc), theme::DIM));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let selected_style = if state.pane_focused {
        theme::SELECTED
    } else {
        theme::SELECTED_INACTIVE
    };
    let list = List::new(items)
        .block(popup::block(t!("model_picker.title").into_owned()))
        .highlight_style(selected_style)
        .highlight_symbol("> ");

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected.min(state.models.len() - 1)));

    frame.render_stateful_widget(list, area, &mut list_state);
}
