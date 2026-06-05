//! Slash-command autocomplete popup and `/help` overlay.
//!
//! The popup is anchored to the input box (passed in as `input_area`). When
//! the user types `/` the overlay materializes above the input border with
//! a filtered list of `CommandSpec`s. `/help` opens a centered overlay that
//! lists every command with full descriptions.

use ratatui::prelude::*;
use ratatui::widgets::{Clear, List, ListItem, ListState, Paragraph};

use      ::popup;
use creta::app::App;
use creta::commands::{CommandSpec, REGISTRY};
use creta::theme;

      POPUP_MAX_VISIBLE: usize = 5;

/// Per-frame state captured from the [`App`] so callers don't need to know
/// the popup internals.
           PopupState<'a> {
        candidates: &'a [&'static CommandSpec],
        selected: usize,
    /// Effective model for the active pane (per-pane `/model` override, else
    /// the global one). Appended to the `/model` row so the user sees what
    /// they're currently on while typing the command. `None` when no model
    /// is known yet.
        current_model: Option<String>,
}

/// Render the autocomplete popup just above `input_area`. If there isn't
/// enough room above, fall back to anchoring just below.
///
/// No-op when `state.candidates` is empty.
    fn render_popup(frame: &mut Frame, state: PopupState<'_>, input_area: Rect) {
       state.candidates.is_empty() {
              ;
    }

        visible = state.candidates.len().min(POPUP_MAX_VISIBLE) as u16;
        area = popup::anchored_above(frame, input_area, visible);

    frame.render_widget(Clear, area);

    .     items: Vec<ListItem> = state
        .candidates
        .iter()
        .map(|spec| {
                    spans = vec![
                Span::styled(format!(" /{:<8} ", spec.name), theme::INPUT_TEXT),
                Span::styled(spec.summary(), theme::DIM),
            ];
            // The `/model` row shows the pane's current model so the user can
            // see what they're on before opening the picker.
               spec.name == "model" {
                     Some(model) = state.current_model.as_deref() {
                    spans.push(Span::styled("  → ", theme::DIM));
                    spans.push(Span::styled(model, theme::INPUT_TEXT));
                }
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

        list = List::new(items)
        .block(popup::block(t!("commands.popup_title").into_owned()))
        .highlight_style(theme::SELECTED)
        .highlight_symbol("> ");

           list_state = ListState::default();
    list_state.select(Some(state.selected.min(state.candidates.len() - 1)));

    frame.render_stateful_widget(list, area, &mut list_state);
}

/// Render the `/help` overlay — a centered modal listing every command.
/// No-op when `app.help_overlay_visible` is false.
       render_help_overlay(frame: &mut Frame, app: &App, area: Rect) {
       !app.help_overlay_visible {
              ;
    }

        lines: Vec<Line> = std::iter::once(Line::from(Span::styled(
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

        height = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
        width = 64.min(area.width.saturating_sub(4));
        x = area.x + area.width.saturating_sub(width) / 2;
        y = area.y + area.height.saturating_sub(height) / 2;
        modal = Rect::new(x, y, width, height);

    frame.render_widget(Clear, modal);

        paragraph =
        Paragraph::new(lines).block(popup::block(t!("commands.help_title").into_owned()));
    frame.render_widget(paragraph, modal);
}
