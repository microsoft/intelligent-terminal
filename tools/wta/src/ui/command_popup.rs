//! Slash-command autocomplete popup and `/help` overlay.
//!
//! The popup is anchored to the input box (passed in as `input_area`). When
//! the user types `/` the overlay materializes above the input border with
//! a filtered list of `CommandSpec`s. `/help` opens a centered overlay that
//! lists every command with full descriptions.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::app::App;
use crate::commands::{CommandSpec, REGISTRY};
use crate::theme;

const POPUP_MAX_VISIBLE: usize = 6;
const POPUP_BORDER_HEIGHT: u16 = 2;

/// Per-frame state captured from the [`App`] so callers don't need to know
/// the popup internals.
pub struct PopupState<'a> {
    pub candidates: &'a [&'static CommandSpec],
    pub selected: usize,
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
    let height = visible + POPUP_BORDER_HEIGHT;
    let width = input_area.width;

    // Prefer above; fall back to below if there's no room.
    let area = if input_area.y >= height {
        Rect::new(input_area.x, input_area.y - height, width, height)
    } else {
        // Anchor right below; clamp to frame so we don't render off-screen.
        let frame_area = frame.area();
        let y = (input_area.y + input_area.height).min(frame_area.y + frame_area.height.saturating_sub(height));
        Rect::new(input_area.x, y, width, height)
    };

    frame.render_widget(Clear, area);

    let items: Vec<ListItem> = state
        .candidates
        .iter()
        .map(|spec| {
            let line = Line::from(vec![
                Span::styled(format!(" /{:<8} ", spec.name), theme::INPUT_TEXT),
                Span::styled(spec.summary.to_string(), theme::DIM),
            ]);
            ListItem::new(line)
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::INPUT_BORDER)
        .style(Style::default().bg(theme::INPUT_BG))
        .title(" / commands ");

    let list = List::new(items)
        .block(block)
        .highlight_style(theme::SELECTED)
        .highlight_symbol("> ");

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected.min(state.candidates.len() - 1)));

    frame.render_stateful_widget(list, area, &mut list_state);
}

/// Render the `/help` overlay — a centered modal listing every command.
/// No-op when `app.help_overlay_visible` is false.
pub fn render_help_overlay(frame: &mut Frame, app: &App, area: Rect) {
    if !app.help_overlay_visible {
        return;
    }

    let lines: Vec<Line> = std::iter::once(Line::from(Span::styled(
        "Slash commands — type / in the input box.".to_string(),
        theme::DIM,
    )))
    .chain(std::iter::once(Line::default()))
    .chain(REGISTRY.iter().map(|spec| {
        Line::from(vec![
            Span::styled(format!("  /{:<8}  ", spec.name), theme::INPUT_TEXT),
            Span::styled(spec.summary.to_string(), theme::DIM),
        ])
    }))
    .chain(std::iter::once(Line::default()))
    .chain(std::iter::once(Line::from(Span::styled(
        "  //text     Send a literal '/' (escapes the command parser)".to_string(),
        theme::DIM,
    ))))
    .chain(std::iter::once(Line::from(Span::styled(
        "  Esc        Close this help".to_string(),
        theme::DIM,
    ))))
    .collect();

    let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let width = 64.min(area.width.saturating_sub(4));
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    let modal = Rect::new(x, y, width, height);

    frame.render_widget(Clear, modal);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::INPUT_BORDER)
        .style(Style::default().bg(theme::INPUT_BG))
        .title(" Help ");

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, modal);
}
