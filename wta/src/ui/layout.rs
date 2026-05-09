use ratatui::prelude::*;
use crate::app::{App, AppMode, View};

use super::{agents_view, chat, command_popup, debug_panel, input, permission, recommendations, setup};

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    // Setup mode (preflight failed): full-pane wizard, nothing else drawn.
    if app.mode == AppMode::Setup {
        setup::render(frame, app, area);
        return;
    }

    // Agents view (F2) takes over the full pane area; chat / input / debug
    // panel are not drawn in this mode.
    if app.current_view == View::Agents {
        agents_view::render(frame, area, &app.agent_sessions, &mut app.agents_list_state);
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

    // The host (Windows Terminal) renders the agent bar in XAML above this
    // pane, so wta uses the full pane area for chat / recommendations / input.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            rec_height,
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
    input::render(frame, app, chunks[2]);

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
    if app.current_view == View::Agents || app.mode == AppMode::Setup {
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
