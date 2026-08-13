//! Session configuration picker driven by ACP `configOptions`.

use ratatui::prelude::*;
use ratatui::widgets::{Clear, List, ListItem, ListState};

use super::popup;
use crate::app_contracts::AcpSessionConfigOption;
use crate::theme;

const POPUP_MAX_VISIBLE: usize = 8;

pub struct ConfigPopupState<'a> {
    pub options: &'a [AcpSessionConfigOption],
    pub value_option: Option<&'a AcpSessionConfigOption>,
    pub selected: usize,
    pub pending_config_id: Option<&'a str>,
    pub pane_focused: bool,
}

pub fn render_popup(frame: &mut Frame, state: ConfigPopupState<'_>, input_area: Rect) {
    let row_count = state
        .value_option
        .map(|option| option.values.len())
        .unwrap_or(state.options.len());
    if row_count == 0 {
        return;
    }

    let visible = row_count.min(POPUP_MAX_VISIBLE) as u16;
    let area = popup::anchored_above(frame, input_area, visible);
    frame.render_widget(Clear, area);

    let (title, items) = match state.value_option {
        Some(option) => {
            let items: Vec<ListItem<'_>> = option
                .values
                .iter()
                .map(|value| {
                    let mut spans = vec![Span::styled(&value.name, theme::INPUT_TEXT)];
                    if let Some(description) = value.description.as_deref() {
                        spans.push(Span::styled(format!(" - {description}"), theme::DIM));
                    }
                    ListItem::new(Line::from(spans))
                })
                .collect();
            (option.name.clone(), items)
        }
        None => {
            let items: Vec<ListItem<'_>> = state
                .options
                .iter()
                .map(|option| {
                    let pending = state.pending_config_id == Some(option.id.as_str());
                    let mut spans = vec![
                        Span::styled(&option.name, theme::INPUT_TEXT),
                        Span::raw("  "),
                        Span::styled(option.current_value_name(), theme::DIM),
                    ];
                    if pending {
                        spans.push(Span::styled(" ...", theme::IN_PROGRESS));
                    } else if let Some(description) = option.description.as_deref() {
                        spans.push(Span::styled(format!(" - {description}"), theme::DIM));
                    }
                    ListItem::new(Line::from(spans))
                })
                .collect();
            (t!("config_picker.title").into_owned(), items)
        }
    };

    let selected_style = if state.pane_focused {
        theme::SELECTED
    } else {
        theme::SELECTED_INACTIVE
    };
    let list = List::new(items)
        .block(popup::block(title))
        .highlight_style(selected_style)
        .highlight_symbol("> ");

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected.min(row_count - 1)));
    frame.render_stateful_widget(list, area, &mut list_state);
}
