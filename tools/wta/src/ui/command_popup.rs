//! Slash-command autocomplete popup and `/help` overlay.
//!
//! The popup is anchored to the input box (passed in as `input_area`). When
//! the user types `/` the overlay materializes above the input border with
//! a filtered list of `CommandSpec`s. `/help` opens a centered overlay that
//! lists every command with full descriptions.

use std::borrow::Cow;

use ratatui::prelude::*;
use ratatui::widgets::{Clear, List, ListItem, ListState, Paragraph};

use super::popup;
use crate::app::App;
use crate::commands::{CommandSpec, REGISTRY};
use crate::theme;

const POPUP_MAX_VISIBLE: usize = 6;

/// Per-frame state captured from the [`App`] so callers don't need to know
/// the popup internals.
pub struct PopupState<'a> {
    /// The commands to show. Borrowed in the normal case (the candidates
    /// already live on `TabSession`, so no per-frame allocation on the render
    /// hot path); owned only when the App has to filter — in the degraded
    /// (transport-lost) case it collapses to just `/restart` (the popup simply
    /// *doesn't show the other commands* rather than greying them).
    pub candidates: Cow<'a, [&'static CommandSpec]>,
    pub selected: usize,
    /// Effective model for the active pane (per-pane `/model` override, else
    /// the global one). Appended to the `/model` row so the user sees what
    /// they're currently on while typing the command. `None` when no model
    /// is known yet.
    pub current_model: Option<String>,
}

/// Render the autocomplete popup just above `input_area`. If there isn't
/// enough room above, fall back to anchoring just below.
///
/// No-op when `state.candidates` is empty.
pub fn render_popup(frame: &mut Frame, state: PopupState<'_>, input_area: Rect) {
    if state.candidates.is_empty() {
        return;
    }

    let visible = state.candidates.len().min(POPUP_MAX_VISIBLE) as u16;
    let area = popup::anchored_above(frame, input_area, visible);

    frame.render_widget(Clear, area);

    let items: Vec<ListItem> = state
        .candidates
        .iter()
        .map(|spec| {
            let mut spans = vec![
                Span::styled(format!(" /{:<8} ", spec.name), theme::INPUT_TEXT),
                Span::styled(spec.summary(), theme::DIM),
            ];
            // The `/model` row shows the pane's current model so the user can
            // see what they're on before opening the picker.
            if spec.name == "model" {
                if let Some(model) = state.current_model.as_deref() {
                    spans.push(Span::styled("  → ", theme::DIM));
                    spans.push(Span::styled(model, theme::INPUT_TEXT));
                }
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items)
        .block(popup::block(t!("commands.popup_title").into_owned()))
        .highlight_style(theme::SELECTED)
        .highlight_symbol("> ");

    let mut list_state = ListState::default();
    list_state.select(popup_highlight(
        &state.candidates,
        state.selected,
    ));

    frame.render_stateful_widget(list, area, &mut list_state);
}

/// Which row the command popup highlights: the user's cursor index, clamped
/// into range. `None` for an empty list. The degraded (transport-lost) case
/// needs no special handling here — the App pre-filters the candidate list to
/// just `/restart`, so the normal clamp lands on it. Pure so it can be
/// unit-tested without a render frame.
pub(crate) fn popup_highlight(
    candidates: &[&'static CommandSpec],
    selected: usize,
) -> Option<usize> {
    if candidates.is_empty() {
        return None;
    }
    Some(selected.min(candidates.len() - 1))
}

/// Render the `/help` overlay — a centered modal listing every command.
/// No-op when `app.help_overlay_visible` is false.
pub fn render_help_overlay(frame: &mut Frame, app: &App, area: Rect) {
    if !app.help_overlay_visible {
        return;
    }

    let lines: Vec<Line> = std::iter::once(Line::from(Span::styled(
        t!("commands.help_header").into_owned(),
        theme::DIM,
    )))
    .chain(std::iter::once(Line::default()))
    .chain(REGISTRY.iter().map(|spec| {
        Line::from(vec![
            Span::styled(format!("  /{:<8}  ", spec.name), theme::INPUT_TEXT),
            Span::styled(spec.summary(), theme::DIM),
        ])
    }))
    .chain(std::iter::once(Line::default()))
    .chain(std::iter::once(Line::from(Span::styled(
        t!("commands.help_escape_hint").into_owned(),
        theme::DIM,
    ))))
    .chain(std::iter::once(Line::from(Span::styled(
        t!("commands.help_close_hint").into_owned(),
        theme::DIM,
    ))))
    .collect();

    let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let width = 64.min(area.width.saturating_sub(4));
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    let modal = Rect::new(x, y, width, height);

    frame.render_widget(Clear, modal);

    let paragraph =
        Paragraph::new(lines).block(popup::block(t!("commands.help_title").into_owned()));
    frame.render_widget(paragraph, modal);
}

#[cfg(test)]
mod tests {
    use super::popup_highlight;
    use crate::commands;

    fn spec(name: &str) -> &'static commands::CommandSpec {
        commands::lookup(name).expect("registered command")
    }

    #[test]
    fn highlight_follows_cursor() {
        let cands = vec![spec("help"), spec("new"), spec("restart")];
        assert_eq!(popup_highlight(&cands, 1), Some(1));
    }

    #[test]
    fn highlight_clamps_out_of_range_cursor() {
        // The App collapses the list to a single command (/restart) when the
        // transport is lost; a stale larger `selected` must clamp onto it.
        let cands = vec![spec("restart")];
        assert_eq!(popup_highlight(&cands, 9), Some(0));
    }

    #[test]
    fn empty_candidates_highlight_nothing() {
        assert_eq!(popup_highlight(&[], 0), None);
    }
}
