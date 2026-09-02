//! Slash-command autocomplete popup and `/help` overlay.
//!
//! The popup is anchored to the input box (passed in as `input_area`). When
//! the user types `/` the overlay materializes above the input border with
//! a filtered list of `CommandSpec`s. `/help` opens a centered overlay that
//! lists every command with full descriptions.

use ratatui::prelude::*;
use ratatui::widgets::{Clear, List, ListItem, ListState, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::popup;
use crate::app::{App, AvailableAgent};
use crate::app_contracts::{AcpSessionCommand, CompletionBehavior};
use crate::commands::{CommandSpec, MovePositionSpec, REGISTRY};
use crate::theme;

const POPUP_MAX_VISIBLE: usize = 6;
const SOURCE_LABEL_MAX_WIDTH: usize = 12;

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
    /// Friendly name of the connected Agent, used to identify commands
    /// advertised through ACP.
    pub agent_label: &'a str,
}

pub enum PopupCandidates<'a> {
    Commands(Vec<CommandCandidate<'a>>),
    MovePositions(&'a [&'static MovePositionSpec]),
    Agents(Vec<&'a AvailableAgent>),
}

#[derive(Clone, Copy)]
pub enum CommandCandidate<'a> {
    Client(&'static CommandSpec),
    Agent(&'a AcpSessionCommand),
}

impl<'a> CommandCandidate<'a> {
    pub fn name(self) -> &'a str {
        match self {
            Self::Client(spec) => spec.name,
            Self::Agent(command) => command.name.as_str(),
        }
    }

    pub fn completion_behavior(self) -> CompletionBehavior {
        match self {
            Self::Client(spec) => spec.completion_behavior,
            Self::Agent(command) => command.completion_behavior,
        }
    }
}

/// Render the autocomplete popup just above `input_area`. If there isn't
/// enough room above, fall back to anchoring just below.
///
/// No-op when `state.candidates` is empty.
pub fn render_popup(frame: &mut Frame, state: PopupState<'_>, input_area: Rect) {
    let candidate_count = match &state.candidates {
        PopupCandidates::Commands(candidates) => candidates.len(),
        PopupCandidates::MovePositions(candidates) => candidates.len(),
        PopupCandidates::Agents(candidates) => candidates.len(),
    };
    if candidate_count == 0 {
        return;
    }

    let visible = candidate_count.min(POPUP_MAX_VISIBLE) as u16;
    let area = popup::anchored_above(frame, input_area, visible);

    frame.render_widget(Clear, area);

    let items: Vec<ListItem> = match &state.candidates {
        PopupCandidates::Commands(candidates) => {
            let agent_label = truncate_source_label(state.agent_label);
            let source_width = candidates
                .iter()
                .map(|candidate| match candidate {
                    CommandCandidate::Client(_) => UnicodeWidthStr::width("IT"),
                    CommandCandidate::Agent(_) => UnicodeWidthStr::width(agent_label.as_str()),
                })
                .max()
                .unwrap_or_default();
            candidates
                .iter()
                .map(|candidate| {
                    let (name, summary) = match candidate {
                        CommandCandidate::Client(spec) => (spec.name, spec.summary()),
                        CommandCandidate::Agent(command) => {
                            (command.name.as_str(), command.description.clone().into())
                        }
                    };
                    let mut spans = command_name_spans(name, state.command_query);
                    spans.push(source_badge_span(
                        candidate,
                        agent_label.as_str(),
                        source_width,
                    ));
                    spans.push(Span::styled(summary, theme::DIM));
                    // The `/model` row shows the pane's current model so the user can
                    // see what they're on before opening the picker.
                    if matches!(candidate, CommandCandidate::Client(spec) if spec.name == "model") {
                        if let Some(model) = state.current_model.as_deref() {
                            spans.push(Span::styled("  → ", theme::DIM));
                            spans.push(Span::styled(model, theme::INPUT_TEXT));
                        }
                    }
                    ListItem::new(Line::from(spans))
                })
                .collect()
        }
        PopupCandidates::MovePositions(candidates) => candidates
            .iter()
            .map(|position| {
                ListItem::new(Line::from(vec![
                    Span::styled(format!(" /move {:<6} ", position.name), theme::INPUT_TEXT),
                    Span::styled(format!("({})", position.alias), theme::DIM),
                ]))
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

fn source_badge_span(
    candidate: &CommandCandidate<'_>,
    agent_label: &str,
    column_width: usize,
) -> Span<'static> {
    let label = match candidate {
        CommandCandidate::Client(_) => "IT",
        CommandCandidate::Agent(_) => agent_label,
    };
    let padding = column_width.saturating_sub(UnicodeWidthStr::width(label));
    Span::styled(format!("[{label}]{}  ", " ".repeat(padding)), theme::DIM)
}

fn truncate_source_label(label: &str) -> String {
    let label = label.trim();
    let label = if label.is_empty() { "Agent" } else { label };
    if UnicodeWidthStr::width(label) <= SOURCE_LABEL_MAX_WIDTH {
        return label.to_string();
    }

    let budget = SOURCE_LABEL_MAX_WIDTH - 1;
    let mut truncated = String::new();
    let mut width = 0;
    for character in label.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if width + character_width > budget {
            break;
        }
        truncated.push(character);
        width += character_width;
    }
    truncated.push('…');
    truncated
}

fn command_name_spans(name: &str, query: &str) -> Vec<Span<'static>> {
    let padded_name = format!("{name:<8} ");
    let needle = query.trim().to_ascii_lowercase();
    let Some(start) = (!needle.is_empty())
        .then(|| name.to_ascii_lowercase().find(&needle))
        .flatten()
    else {
        return vec![Span::styled(format!(" /{padded_name}"), theme::INPUT_TEXT)];
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
pub(crate) fn popup_highlight(candidate_count: usize, selected: usize) -> Option<usize> {
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
    use super::{
        command_name_spans, popup_highlight, source_badge_span, truncate_source_label,
        CommandCandidate,
    };
    use crate::app_contracts::{AcpSessionCommand, CompletionBehavior};
    use crate::commands;
    use crate::theme;
    use unicode_width::UnicodeWidthStr;

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

    #[test]
    fn source_badges_distinguish_client_and_agent_commands() {
        let client = CommandCandidate::Client(spec("model"));
        let agent_command = AcpSessionCommand {
            name: "usage".into(),
            description: "Show usage".into(),
            input_hint: None,
            completion_behavior: CompletionBehavior::ExecuteImmediately,
        };
        let agent = CommandCandidate::Agent(&agent_command);

        assert_eq!(
            source_badge_span(&client, "Copilot", 7).content,
            "[IT]       "
        );
        assert_eq!(
            source_badge_span(&agent, "Copilot", 7).content,
            "[Copilot]  "
        );
    }

    #[test]
    fn source_label_is_width_limited() {
        let label = truncate_source_label("Very Long Agent Name");
        assert_eq!(label, "Very Long A…");
        assert!(UnicodeWidthStr::width(label.as_str()) <= 12);
    }
}
