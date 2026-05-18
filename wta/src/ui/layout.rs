use ratatui::prelude::*;
use crate::app::{App, AppMode, View, DEFAULT_TAB_ID};

use super::{auth, agents_view, chat, command_popup, debug_panel, input, permission, recommendations, setup};

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    // Auth mode: show auth screen above the input box
    if app.mode == AppMode::Auth {
        let input_height = {
            let tab = app.current_tab();
            input::input_height(&tab.input, tab.cursor_pos, area.width)
        };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(input_height),
            ])
            .split(area);
        auth::render(frame, app, chunks[0]);
        input::render(frame, app, chunks[1]);
        return;
    }

    // Setup mode: unified setup wizard (FRE + preflight) with input box
    if app.mode == AppMode::Setup {
        let input_height = {
            let tab = app.current_tab();
            input::input_height(&tab.input, tab.cursor_pos, area.width)
        };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(input_height),
            ])
            .split(area);
        setup::render(frame, app, chunks[0]);
        input::render(frame, app, chunks[1]);
        return;
    }

    // Agents view (F2) takes over the full pane area; chat / input / debug
    // panel are not drawn in this mode. Per-tab: the active tab's
    // TabSession owns the open state and selection cursor. Disjoint-field
    // borrow (agent_sessions vs. tab_sessions[id]) lets us pass both refs
    // through without going through current_tab_mut() (which would borrow
    // the whole App and conflict with &app.agent_sessions).
    if app.current_tab().current_view == View::Agents {
        let tab_id = app.tab_id.as_deref().unwrap_or(DEFAULT_TAB_ID).to_string();
        let tab = app.tab_sessions.entry(tab_id).or_default();
        agents_view::render(frame, area, &app.agent_sessions, &mut tab.agents_list_state, app.history_load_state);
        return;
    }

    let (main_area, debug_area) = if app.show_debug_panel {
        let h = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(area);
        (h[0], Some(h[1]))
    } else {
        (area, None)
    };

    let rec_height = if app.current_tab().recommendations.is_some() {
        Constraint::Length(app.rec_panel_height())
    } else {
        Constraint::Length(0)
    };
    let input_height = {
        let tab = app.current_tab();
        input::input_height(&tab.input, tab.cursor_pos, main_area.width)
    };

    // Show shortcut hint above input when chat is empty (first-run welcome)
    let show_hint = app.current_tab().completed_turns.is_empty()
        && app.current_tab().messages.is_empty()
        && app.state == crate::app::ConnectionState::Connected;
    let hint_height = if show_hint { Constraint::Length(1) } else { Constraint::Length(0) };

    // The host (Windows Terminal) renders the agent bar in XAML above this
    // pane, so wta uses the full pane area for chat / recommendations / input.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            rec_height,
            hint_height,
            Constraint::Length(input_height),
        ])
        .split(main_area);

    // Horizontal padding for chat and recommendations only
    let h_chat = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
        .split(chunks[0]);
    let h_rec = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
        .split(chunks[1]);

    chat::render(frame, app, h_chat[1]);
    recommendations::render(frame, app, h_rec[1]);

    // Shortcut hint just above input
    if show_hint {
        let hint = ratatui::widgets::Paragraph::new(ratatui::text::Line::from(
            ratatui::text::Span::styled(
                "(Ctrl+Shift+. to show/hide agent pane \u{2022} Ctrl+Alt+/ to show/hide agent session)",
                ratatui::style::Style::new().fg(ratatui::style::Color::DarkGray),
            ),
        ));
        let h_hint = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
            .split(chunks[2]);
        frame.render_widget(hint, h_hint[1]);
    }

    input::render(frame, app, chunks[3]);

    if let Some(debug_area) = debug_area {
        debug_panel::render(frame, app, debug_area);
    }

    // Slash-command autocomplete: anchored above the input box. Drawn
    // before permission/help so those overlays still cover it if they
    // happen to be visible at the same time.
    if let Some(popup_state) = app.command_popup_state() {
        command_popup::render_popup(frame, popup_state, chunks[2]);
    }

    if app.current_tab().permission.is_some() {
        permission::render(frame, app, area);
    }

    // `/help` overlay sits on top of everything (including permission) so
    // the user can always dismiss it with Esc.
    command_popup::render_help_overlay(frame, app, area);
}

pub fn input_cursor_position(app: &App, area: Rect) -> Option<Position> {
    // Agents view / Setup view: no input box, so no cursor.
    if app.current_tab().current_view == View::Agents || app.mode == AppMode::Setup {
        return None;
    }

    // Placeholder state: the input box renders its own static white-bg /
    // black-fg cell at the prompt position (see ui::input::render). Hide
    // the real terminal cursor so WT doesn't overlay its focused-pane
    // block on top — that block fully replaces the cell content and would
    // hide the painted glyph (in unfocused panes WT draws a hollow outline
    // and the cell shows through, which is what we already get for free
    // by hiding the cursor in both focus states).
    if app.current_tab().input.is_empty() {
        return None;
    }

    let main_area = if app.show_debug_panel {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(area)[0]
    } else {
        area
    };

    let rec_height = if app.current_tab().recommendations.is_some() {
        Constraint::Length(app.rec_panel_height())
    } else {
        Constraint::Length(0)
    };
    let input_height = {
        let tab = app.current_tab();
        input::input_height(&tab.input, tab.cursor_pos, main_area.width)
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            rec_height,
            Constraint::Length(input_height),
        ])
        .split(main_area);

    input::cursor_position(app, chunks[2])
}
