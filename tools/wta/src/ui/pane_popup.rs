use ratatui::prelude::*;
use ratatui::widgets::{Clear, List, ListItem, ListState, Paragraph};

use super::popup;
use crate::theme;

const POPUP_MAX_VISIBLE: usize = 8;

pub struct PanePopupCandidate {
    pub label: String,
    pub detail: String,
}

pub struct PanePopupState {
    pub candidates: Vec<PanePopupCandidate>,
    pub selected: usize,
    pub pane_focused: bool,
}

pub fn render_popup(frame: &mut Frame, state: PanePopupState, input_area: Rect) {
    let area = popup::anchored_above(
        frame,
        input_area,
        state.candidates.len().clamp(1, POPUP_MAX_VISIBLE) as u16,
    );
    frame.render_widget(Clear, area);
    if state.candidates.is_empty() {
        frame.render_widget(
            Paragraph::new(t!("pane_picker.empty").into_owned())
                .block(popup::block(t!("pane_picker.title").into_owned()))
                .style(theme::DIM),
            area,
        );
        return;
    }
    let items = state
        .candidates
        .iter()
        .map(|candidate| {
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {} ", candidate.label), theme::INPUT_TEXT),
                Span::styled(candidate.detail.as_str(), theme::DIM),
            ]))
        })
        .collect::<Vec<_>>();
    let mut list_state = ListState::default();
    list_state.select(Some(
        state.selected.min(state.candidates.len().saturating_sub(1)),
    ));
    frame.render_stateful_widget(
        List::new(items)
            .block(popup::block(t!("pane_picker.title").into_owned()))
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
