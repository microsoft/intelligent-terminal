use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::app::{App, ConnectionState};
use crate::theme;

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let (status_text, status_style) = match &app.state {
        ConnectionState::Disconnected => ("Disconnected", theme::STATUS_DISCONNECTED),
        ConnectionState::Connecting(stage) => (stage.as_str(), theme::STATUS_CONNECTING),
        ConnectionState::Connected => ("Connected", theme::STATUS_CONNECTED),
        ConnectionState::Failed(msg) => {
            // We can't return a reference to msg, so handle inline
            let text = format!("[wta] {} | Failed: {}", app.agent_name, msg);
            let p = Paragraph::new(text).style(theme::STATUS_FAILED);
            frame.render_widget(p, area);
            return;
        }
    };

    let name = if app.agent_name.is_empty() {
        "agent"
    } else {
        &app.agent_name
    };

    let session_info = if app.session_id.is_empty() {
        String::new()
    } else {
        let short = if app.session_id.len() > 8 {
            &app.session_id[..8]
        } else {
            &app.session_id
        };
        format!(" | session: {}", short)
    };

    let wt_info = if app.wt_connected {
        " | WT:pipe"
    } else {
        " | WT:local"
    };

    let pane_info = match (&app.pane_id, &app.tab_id) {
        (Some(p), Some(t)) => format!(" | pane:{} tab:{}", p, t),
        (Some(p), None) => format!(" | pane:{}", p),
        _ => String::new(),
    };

    let debug_hint = if app.show_debug_panel {
        ""
    } else {
        " | F12:debug"
    };
    let recommendation_hint = if app.recommendations.is_some() {
        " | recs:ready"
    } else {
        ""
    };

    let text = format!(
        "[wta] {} | {}{}{}{}{}{}",
        name, status_text, session_info, wt_info, pane_info, recommendation_hint, debug_hint
    );
    let p = Paragraph::new(text).style(status_style);
    frame.render_widget(p, area);
}
