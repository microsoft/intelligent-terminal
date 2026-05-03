use ratatui::prelude::*;
use crate::app::{App, AppMode};

use super::{chat, debug_panel, input, permission, recommendations, setup};

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();

    if app.mode == AppMode::Setup {
        setup::render(frame, app, area);
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

    let rec_height = if app.recommendations.is_some() {
        Constraint::Length(app.rec_panel_height())
    } else {
        Constraint::Length(0)
    };
    let input_height = input::input_height(&app.input, app.cursor_pos, main_area.width);

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

    match app.current_view {
        crate::app::View::Chat => {
            chat::render(frame, app, h_chat[1]);
            recommendations::render(frame, app, h_rec[1]);
            input::render(frame, app, chunks[2]);
        }
        crate::app::View::Agents => {
            let mut state = app.agents_list_state.clone();
            super::agents_view::render(
                frame,
                chunks[0],
                &app.agent_sessions,
                &mut state,
            );
        }
    }

    if let Some(debug_area) = debug_area {
        debug_panel::render(frame, app, debug_area);
    }

    if app.permission.is_some() {
        permission::render(frame, app, area);
    }
}

pub fn input_cursor_position(app: &App, area: Rect) -> Option<Position> {
    if app.mode == AppMode::Setup {
        return None;
    }

    if app.current_view == crate::app::View::Agents {
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

    let rec_height = if app.recommendations.is_some() {
        Constraint::Length(app.rec_panel_height())
    } else {
        Constraint::Length(0)
    };
    let input_height = input::input_height(&app.input, app.cursor_pos, main_area.width);

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
