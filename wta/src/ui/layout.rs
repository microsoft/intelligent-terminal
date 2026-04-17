use ratatui::prelude::*;

use crate::app::{App, AppMode};

use super::{chat, debug_panel, input, permission, recommendations, setup};

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();

    // If in Setup mode, render the setup wizard full-screen
    if app.mode == AppMode::Setup {
        setup::render(frame, app, area);
        return;
    }

    // Split horizontally if debug panel is visible
    let (main_area, debug_area) = if app.show_debug_panel {
        let h = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(area);
        (h[0], Some(h[1]))
    } else {
        (area, None)
    };

    let recommendations_height = if app.recommendations.is_some() {
        Constraint::Length(8)
    } else {
        Constraint::Length(0)
    };

    let input_height = input::input_height(&app.input, app.cursor_pos, main_area.width);

    // Layout: recommendations | chat | input
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            recommendations_height,
            Constraint::Min(1),            // chat area
            Constraint::Length(input_height),
        ])
        .split(main_area);

    recommendations::render(frame, app, chunks[0]);
    chat::render(frame, app, chunks[1]);
    input::render(frame, app, chunks[2]);

    // Debug panel (right side)
    if let Some(debug_area) = debug_area {
        debug_panel::render(frame, app, debug_area);
    }

    // Permission modal overlay (rendered last, on top)
    if app.permission.is_some() {
        permission::render(frame, app, area);
    }
}

pub fn input_cursor_position(app: &App, area: Rect) -> Option<Position> {
    // No cursor in setup mode
    if app.mode == AppMode::Setup {
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

    let recommendations_height = if app.recommendations.is_some() {
        Constraint::Length(8)
    } else {
        Constraint::Length(0)
    };

    let input_height = input::input_height(&app.input, app.cursor_pos, main_area.width);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            recommendations_height,
            Constraint::Min(1),
            Constraint::Length(input_height),
        ])
        .split(main_area);

    input::cursor_position(app, chunks[2])
}
