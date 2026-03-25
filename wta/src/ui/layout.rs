use ratatui::prelude::*;

use crate::app::App;

use super::{chat, debug_panel, input, permission, recommendations, status_bar};

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();

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

    // Layout: status bar | recommendations | chat | input
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // status bar
            recommendations_height,
            Constraint::Min(1),    // chat area
            Constraint::Length(3), // input box
        ])
        .split(main_area);

    status_bar::render(frame, app, chunks[0]);
    recommendations::render(frame, app, chunks[1]);
    chat::render(frame, app, chunks[2]);
    input::render(frame, app, chunks[3]);

    // Debug panel (right side)
    if let Some(debug_area) = debug_area {
        debug_panel::render(frame, app, debug_area);
    }

    // Permission modal overlay (rendered last, on top)
    if app.permission.is_some() {
        permission::render(frame, app, area);
    }
}
