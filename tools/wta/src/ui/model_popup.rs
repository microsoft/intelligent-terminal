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

/// Per-frame state captured from the [`App`](crate::app::App).
pub struct ModelPopupState<'a> {
    pub models: &'a [AcpModelInfo],
    pub selected: usize,
    pub pane_focused: bool,
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
            let disabled = state.disabled.get(index).copied().unwrap_or(false);
            let spans = vec![Span::styled(
                m.name.as_str(),
                if disabled {
                    theme::DIM
                } else {
                    theme::INPUT_TEXT
                },
            )];
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
