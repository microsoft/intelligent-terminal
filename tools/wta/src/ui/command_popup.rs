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
use crate::app::{App, AvailableAgent};
use crate::commands::{CommandSpec, MovePositionSpec, YoloOptionSpec, REGISTRY};
use crate::theme;

const POPUP_MAX_VISIBLE: usize = 6;

/// Per-frame state captured from the [`App`] so callers don't need to know
/// the popup internals.
pub struct PopupState<'a> {
    pub candidates: PopupCandidates<'a>,
    pub selected: usize,
    pub pane_focused: bool,
    /// Text after the leading `/`, used to highlight the matching part of
    /// command-name candidates.
    pub command_query: &'a str,
    /// Effective model for the active pane (per-pane `/model` override, else
    /// the global one). Appended to the `/model` row so the user sees what
    /// they're currently on while typing the command. `None` when no model
    /// is known yet.
    pub current_model: Option<String>,
}

pub enum PopupCandidates<'a> {
    Commands(Cow<'a, [&'static CommandSpec]>),
    MovePositions(&'a [&'static MovePositionSpec]),
    YoloOptions(&'a [&'static YoloOptionSpec]),
    Agents(Vec<&'a AvailableAgent>),
}

/// Render the autocomplete popup just above `input_area`. If there isn't
/// enough room above, fall back to anchoring just below.
///
/// No-op when `state.candidates` is empty.
pub fn render_popup(frame: &mut Frame, state: PopupState<'_>, input_area: Rect) {
    let candidate_count = match &state.candidates {
        PopupCandidates::Commands(candidates) => candidates.len(),
        PopupCandidates::MovePositions(candidates) => candidates.len(),
        PopupCandidates::YoloOptions(candidates) => candidates.len(),
        PopupCandidates::Agents(candidates) => candidates.len(),
    };
    if candidate_count == 0 {
        return;
    }

    let visible = candidate_count.min(POPUP_MAX_VISIBLE) as u16;
    let area = popup::anchored_above(frame, input_area, visible);

    frame.render_widget(Clear, area);

    let items: Vec<ListItem> = match &state.candidates {
        PopupCandidates::Commands(candidates) => candidates
            .iter()
            .map(|spec| {
                let mut spans = command_name_spans(spec.name, state.command_query);
                spans.push(Span::styled(spec.summary(), theme::DIM));
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
            .collect(),
        PopupCandidates::MovePositions(candidates) => candidates
            .iter()
            .map(|position| {
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!(" /move {:<6} ", position.name),
                        theme::INPUT_TEXT,
                    ),
                    Span::styled(format!("({})", position.alias), theme::DIM),
                ]))
            })
            .collect(),
        PopupCandidates::YoloOptions(candidates) => candidates
            .iter()
            .map(|option| {
                ListItem::new(Line::from(Span::styled(
                    format!(" /yolo {} ", option.name),
                    theme::INPUT_TEXT,
                )))
            })
            .collect(),
        PopupCandidates::Agents(candidates) => candidates
            .iter()
            .map(|agent| {
                ListItem::new(Line::from(vec![
                    Span::styled(format!(" /agent {:<8} ", agent.id), theme::INPUT_TEXT),
                    Span::styled(agent.display_name.as_str(), theme::DIM),
                ]))
            })
            .collect(),
    };

    let selected_style = if state.pane_focused {
        theme::SELECTED
    } else {
        theme::SELECTED_INACTIVE
    };
    let list = List::new(items)
        .block(popup::block(t!("commands.popup_title").into_owned()))
        .highlight_style(selected_style)
        .highlight_symbol("> ");

    let mut list_state = ListState::default();
    list_state.select(popup_highlight(candidate_count, state.selected));

    frame.render_stateful_widget(list, area, &mut list_state);
}

fn command_name_spans(name: &str, query: &str) -> Vec<Span<'static>> {
    let padded_name = format!("{name:<8} ");
    let needle = query.trim().to_ascii_lowercase();
    let Some(start) = (!needle.is_empty())
        .then(|| name.to_ascii_lowercase().find(&needle))
        .flatten()
    else {
        return vec![Span::styled(
            format!(" /{padded_name}"),
            theme::INPUT_TEXT,
        )];
    };
    let end = start + needle.len();

    vec![
        Span::styled(format!(" /{}", &name[..start]), theme::INPUT_TEXT),
        Span::styled(name[start..end].to_string(), theme::SEARCH_MATCH),
        Span::styled(
            format!("{}{}", &name[end..], &padded_name[name.len()..]),
            theme::INPUT_TEXT,
        ),
    ]
}

/// Which row the command popup highlights: the user's cursor index, clamped
/// into range. `None` for an empty list. The degraded (transport-lost) case
/// needs no special handling here — the App pre-filters the candidate list to
/// just `/restart`, so the normal clamp lands on it. Pure so it can be
/// unit-tested without a render frame.
pub(crate) fn popup_highlight(
    candidate_count: usize,
    selected: usize,
) -> Option<usize> {
    if candidate_count == 0 {
        return None;
    }
    Some(selected.min(candidate_count - 1))
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
    use super::{command_name_spans, popup_highlight};
    use crate::commands;
    use crate::theme;

    fn spec(name: &str) -> &'static commands::CommandSpec {
        commands::lookup(name).expect("registered command")
    }

    #[test]
    fn highlight_follows_cursor() {
        let candidates = vec![spec("help"), spec("new"), spec("restart")];
        assert_eq!(popup_highlight(candidates.len(), 1), Some(1));
    }

    #[test]
    fn highlight_clamps_out_of_range_cursor() {
        // The App collapses the list to a single command (/restart) when the
        // transport is lost; a stale larger `selected` must clamp onto it.
        let candidates = vec![spec("restart")];
        assert_eq!(popup_highlight(candidates.len(), 9), Some(0));
    }

    #[test]
    fn empty_candidates_highlight_nothing() {
        assert_eq!(popup_highlight(0, 0), None);
    }

    #[test]
    fn command_name_highlights_matching_substring() {
        let spans = command_name_spans("clear", "LEAR");

        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, " /c");
        assert_eq!(spans[1].content, "lear");
        assert_eq!(spans[1].style, theme::SEARCH_MATCH);
        assert_eq!(spans[2].content, "    ");
    }

    #[test]
    fn empty_query_keeps_command_name_plain() {
        let spans = command_name_spans("clear", "");

        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, " /clear    ");
        assert_eq!(spans[0].style, theme::INPUT_TEXT);
    }
}
