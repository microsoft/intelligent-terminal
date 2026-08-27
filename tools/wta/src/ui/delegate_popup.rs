use ratatui::prelude::*;
use ratatui::widgets::{Clear, List, ListItem, ListState};

use super::popup;
use crate::theme;

pub struct DelegatePopupState {
    pub selected: usize,
    pub pane_focused: bool,
}

pub fn render_popup(frame: &mut Frame, state: DelegatePopupState, input_area: Rect) {
    let area = popup::anchored_above(frame, input_area, 2);
    frame.render_widget(Clear, area);
    let items = [
        ListItem::new(t!("delegate_picker.tab").into_owned()),
        ListItem::new(t!("delegate_picker.panel").into_owned()),
    ];
    let mut list_state = ListState::default();
    list_state.select(Some(state.selected.min(items.len() - 1)));
    frame.render_stateful_widget(
        List::new(items)
            .block(popup::block(t!("delegate_picker.title").into_owned()))
            .highlight_style(if state.pane_focused {
                theme::SELECTED
            } else {
                theme::SELECTED_INACTIVE
            })
            .highlight_symbol("> "),
        area,
        &mut list_state,
    );
}
