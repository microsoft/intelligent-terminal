//! `/agent` picker modal.

use ratatui::prelude::*;
use ratatui::widgets::{Clear, List, ListItem, ListState};

use super::popup;
use crate::app::AvailableAgent;
use crate::theme;

const POPUP_MAX_VISIBLE: usize = 8;
const CURRENT_MARKER: &str = "● ";
const CURRENT_PAD: &str = "  ";

pub struct AgentPopupState<'a> {
    pub agents: &'a [AvailableAgent],
    pub selected: usize,
    pub pane_focused: bool,
    pub current_id: &'a str,
}

pub fn render_popup(frame: &mut Frame, state: AgentPopupState<'_>, input_area: Rect) {
    if state.agents.is_empty() {
        return;
    }

    let visible = state.agents.len().min(POPUP_MAX_VISIBLE) as u16;
    let area = popup::anchored_above(frame, input_area, visible);
    frame.render_widget(Clear, area);

    let items: Vec<ListItem> = state
        .agents
        .iter()
        .map(|agent| {
            let marker = if agent.id == state.current_id {
                CURRENT_MARKER
            } else {
                CURRENT_PAD
            };
            let spans = vec![
                Span::styled(
                    format!(" {}{}", marker, agent.display_name),
                    theme::INPUT_TEXT,
                ),
                Span::styled(format!("  ({})", agent.id), theme::DIM),
            ];
            ListItem::new(Line::from(spans))
        })
        .collect();

    let selected_style = if state.pane_focused {
        theme::SELECTED
    } else {
        theme::SELECTED_INACTIVE
    };
    let list = List::new(items)
        .block(popup::block(t!("agent_picker.title").into_owned()))
        .highlight_style(selected_style)
        .highlight_symbol("> ");
    let mut list_state = ListState::default();
    list_state.select(Some(state.selected.min(state.agents.len() - 1)));
    frame.render_stateful_widget(list, area, &mut list_state);
}
