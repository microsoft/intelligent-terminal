//! `/agent` custom-agent picker modal.
//!
//! Opened by the `/agent` slash command (`App::cmd_agent`), this overlay lists
//! the custom agents discovered from `.github/agents/` (project) and
//! `~/.github/agents/` (user), plus the built-in default `terminal-agent`, and
//! lets the user switch *this pane* to one of them. Selecting an agent starts a
//! fresh session bound to that agent's `.agent.md` system prompt. Modeled on
//! the `/model` picker (`model_popup.rs`): anchored above the input box, arrow
//! keys move the highlight, Enter commits, Esc dismisses (all handled in
//! `App::handle_key`).

use ratatui::prelude::*;
use ratatui::widgets::{Clear, List, ListItem, ListState};

use super::popup;
use crate::custom_agents::CustomAgent;
use crate::theme;

const POPUP_MAX_VISIBLE: usize = 8;
/// Marker drawn next to the agent the pane is currently on.
const CURRENT_MARKER: &str = "● ";
const CURRENT_PAD: &str = "  ";

/// Per-frame state captured from the [`App`](crate::app::App).
pub struct AgentPopupState<'a> {
    pub agents: &'a [CustomAgent],
    pub selected: usize,
    /// Id of the agent the pane is currently on, drawn with a leading marker.
    pub current_id: Option<&'a str>,
}

/// Render the agent picker just above `input_area`, falling back to below when
/// there isn't room. No-op on an empty list (never happens — the built-in is
/// always present — but guarded for safety).
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
        .map(|a| {
            let is_current = state.current_id == Some(a.id.as_str());
            let marker = if is_current { CURRENT_MARKER } else { CURRENT_PAD };
            let mut spans = vec![Span::styled(
                format!(" {}{}", marker, a.display_name),
                theme::INPUT_TEXT,
            )];
            // Show the raw id when it differs from the display name, plus the
            // optional one-line description, both dimmed.
            if !a.id.eq_ignore_ascii_case(&a.display_name) {
                spans.push(Span::styled(format!("  ({})", a.id), theme::DIM));
            }
            if !a.description.is_empty() {
                spans.push(Span::styled(format!("  — {}", a.description), theme::DIM));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items)
        .block(popup::block(t!("agent_picker.title").into_owned()))
        .highlight_style(theme::SELECTED)
        .highlight_symbol("> ");

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected.min(state.agents.len() - 1)));

    frame.render_stateful_widget(list, area, &mut list_state);
}
