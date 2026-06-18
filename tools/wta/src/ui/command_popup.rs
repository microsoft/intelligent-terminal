//! Slash-command autocomplete popup and `/help` overlay.
//!
//! The popup is anchored to the input box (passed in as `input_area`). When
//! the user types `/` the overlay materializes above the input border with
//! a filtered list of `CommandSpec`s. `/help` opens a centered overlay that
//! lists every command with full descriptions.

use ratatui::prelude::*;
use ratatui::widgets::{Clear, List, ListItem, ListState, Paragraph};

use super::popup;
use crate::app::App;
use crate::commands::{CommandKind, CommandSpec, REGISTRY};
use crate::theme;

const POPUP_MAX_VISIBLE: usize = 6;

/// Per-frame state captured from the [`App`] so callers don't need to know
/// the popup internals.
pub struct PopupState<'a> {
    pub candidates: &'a [&'static CommandSpec],
    pub selected: usize,
    /// Effective model for the active pane (per-pane `/model` override, else
    /// the global one). Appended to the `/model` row so the user sees what
    /// they're currently on while typing the command. `None` when no model
    /// is known yet.
    pub current_model: Option<String>,
    /// True when the helper's transport to wta-master is lost. The popup then
    /// greys out every command except `/restart` and only lets `/restart` be
    /// selected/run — it's the sole command that can recover the dead
    /// connection. Mirrors `App::transport_lost`.
    pub transport_lost: bool,
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
            // Degraded transport: every command but /restart is unrunnable
            // (they'd hit the dead pipe), so grey them out to signal disabled.
            let disabled = state.transport_lost && spec.kind != CommandKind::Restart;
            let name_style = if disabled { theme::DIM } else { theme::INPUT_TEXT };
            let mut spans = vec![
                Span::styled(format!(" /{:<8} ", spec.name), name_style),
                Span::styled(spec.summary(), theme::DIM),
            ];
            // The `/model` row shows the pane's current model so the user can
            // see what they're on before opening the picker.
            if spec.name == "model" {
                if let Some(model) = state.current_model.as_deref() {
                    spans.push(Span::styled("  → ", theme::DIM));
                    spans.push(Span::styled(model, name_style));
                }
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items)
        .block(popup::block(t!("commands.popup_title").into_owned()))
        .highlight_style(theme::SELECTED)
        .highlight_symbol("> ");

    // Selection: normally the user's cursor. When degraded, only /restart is
    // selectable — highlight it wherever it sits in the filtered list, and if
    // it isn't listed at all (e.g. the user typed "/new") highlight nothing so
    // there's no runnable target.
    let mut list_state = ListState::default();
    list_state.select(popup_highlight(
        state.candidates,
        state.selected,
        state.transport_lost,
    ));

    frame.render_stateful_widget(list, area, &mut list_state);
}

/// Which row the command popup highlights.
///
/// Normally the user's cursor index (clamped into range). When the transport
/// to master is lost, only `/restart` is selectable, so highlight it wherever
/// it sits in the filtered list — or `None` if it isn't listed (e.g. the user
/// typed `/new`), so the popup offers no runnable target. `None` for an empty
/// list. Pure so it can be unit-tested without a render frame.
pub(crate) fn popup_highlight(
    candidates: &[&'static CommandSpec],
    selected: usize,
    transport_lost: bool,
) -> Option<usize> {
    if candidates.is_empty() {
        return None;
    }
    if transport_lost {
        candidates
            .iter()
            .position(|s| s.kind == CommandKind::Restart)
    } else {
        Some(selected.min(candidates.len() - 1))
    }
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
    fn highlight_follows_cursor_when_connected() {
        let cands = vec![spec("help"), spec("new"), spec("restart")];
        assert_eq!(popup_highlight(&cands, 1, false), Some(1));
    }

    #[test]
    fn highlight_clamps_out_of_range_cursor() {
        let cands = vec![spec("help"), spec("new")];
        assert_eq!(popup_highlight(&cands, 9, false), Some(1));
    }

    #[test]
    fn degraded_highlights_restart_wherever_it_sits() {
        // restart is last here; degraded must jump the highlight onto it
        // regardless of the stored cursor index.
        let cands = vec![spec("help"), spec("new"), spec("restart")];
        assert_eq!(popup_highlight(&cands, 0, true), Some(2));
    }

    #[test]
    fn degraded_highlights_nothing_when_restart_absent() {
        // e.g. the user typed "/new" — restart isn't in the filtered list, so
        // there must be no runnable target highlighted.
        let cands = vec![spec("new")];
        assert_eq!(popup_highlight(&cands, 0, true), None);
    }

    #[test]
    fn empty_candidates_highlight_nothing() {
        assert_eq!(popup_highlight(&[], 0, false), None);
        assert_eq!(popup_highlight(&[], 0, true), None);
    }
}
