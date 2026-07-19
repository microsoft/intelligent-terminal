//! `/shell-sessions` restore picker modal.
//!
//! Opened by the `/shell-sessions` slash command (`App::cmd_shell_sessions`),
//! this overlay lists the durable shell sessions Windows Terminal saved on tab
//! close (fetched from master's SQLite store via the `shell_sessions/list` ext
//! method — see `master/shell_sessions_db.rs`). Enter asks WT to restore the
//! highlighted tab (its layout, working directory, and scrollback). Modeled on
//! the `/model` picker (`model_popup.rs`): anchored above the input box, arrow
//! keys move the highlight, Enter commits, Esc dismisses (all handled in
//! `App::handle_key`).

use ratatui::prelude::*;
use ratatui::widgets::{Clear, List, ListItem, ListState};

use super::popup;
use crate::session_registry::ShellSessionInfo;
use crate::theme;

const POPUP_MAX_VISIBLE: usize = 8;

/// Per-frame state captured from the [`App`](crate::app::App).
pub struct ShellSessionsPopupState<'a> {
    /// Saved sessions, newest first (sorted by master's `list`).
    pub sessions: &'a [ShellSessionInfo],
    /// Highlighted row, an index into `sessions`.
    pub selected: usize,
}

/// Render the shell-session picker just above `input_area`, falling back to
/// below when there isn't room. No-op on an empty list.
pub fn render_popup(frame: &mut Frame, state: ShellSessionsPopupState<'_>, input_area: Rect) {
    if state.sessions.is_empty() {
        return;
    }

    let visible = state.sessions.len().min(POPUP_MAX_VISIBLE) as u16;
    let area = popup::anchored_above(frame, input_area, visible);

    frame.render_widget(Clear, area);

    let items: Vec<ListItem> = state
        .sessions
        .iter()
        .map(|entry| {
            let mut spans = vec![Span::styled(format!(" {}", entry.name), theme::INPUT_TEXT)];
            // Show the working directory dimmed after the name, when known.
            if !entry.cwd.is_empty() {
                spans.push(Span::styled(format!("  {}", entry.cwd), theme::DIM));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items)
        // TODO(localize): extract to a `shell_sessions_picker.title` catalog key.
        .block(popup::block("Restore shell session (↑ ↓ • Enter • Esc)".to_string()))
        .highlight_style(theme::SELECTED)
        .highlight_symbol("> ");

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected.min(state.sessions.len() - 1)));

    frame.render_stateful_widget(list, area, &mut list_state);
}
