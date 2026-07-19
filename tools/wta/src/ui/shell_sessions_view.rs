use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
    Frame,
};

pub fn render(
    frame: &mut Frame,
    area: Rect,
    sessions: &[crate::shell_session_store::ShellSessionRecord],
    list_state: &mut ListState,
    loading: bool,
    error: Option<&str>,
    delete_confirmation: Option<&str>,
    delete_in_flight: bool,
) {
    let inner = Rect {
        x: area.x.saturating_add(2),
        y: area.y,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };

    for y in inner.y..inner.y.saturating_add(inner.height) {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "┃",
                Style::default().fg(Color::DarkGray),
            ))),
            Rect {
                x: area.x,
                y,
                width: 1,
                height: 1,
            },
        );
    }

    if loading {
        frame.render_widget(Paragraph::new("Loading shell sessions..."), inner);
    } else if let Some(error) = error {
        frame.render_widget(
            Paragraph::new(error.to_string()).style(Style::default().fg(Color::Red)),
            inner,
        );
    } else if sessions.is_empty() {
        frame.render_widget(
            Paragraph::new("No saved shell sessions")
                .style(Style::default().fg(Color::DarkGray)),
            inner,
        );
    } else {
        let selected = list_state.selected();
        let rows = sessions.iter().enumerate().map(|(index, session)| {
            let is_selected = selected == Some(index);
            let marker = if is_selected { "> " } else { "  " };
            let style = if is_selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let short_id = session.id.get(..8).unwrap_or(&session.id);
            let last_used = crate::format_epoch_seconds_utc(session.last_used_at);
            let label = format!(
                "{}  [last used {} UTC · {} · r{}]",
                session.name, last_used, short_id, session.revision
            );
            ListItem::new(Line::from(vec![
                Span::styled(marker, style),
                Span::styled(label, style),
            ]))
        });
        frame.render_stateful_widget(List::new(rows), inner, list_state);
    }

    if area.height > 0 {
        let hint_area = Rect {
            x: area.x,
            y: area.y + area.height - 1,
            width: area.width,
            height: 1,
        };
        let (hint, style) = if delete_in_flight {
            ("Deleting shell session...".to_string(), Style::default().fg(Color::Yellow))
        } else if let Some(id) = delete_confirmation {
            let name = sessions
                .iter()
                .find(|session| session.id == id)
                .map(|session| session.name.as_str())
                .unwrap_or(id);
            (
                format!("Delete \"{name}\"? Y Confirm - N/Esc Cancel"),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            )
        } else {
            (
                "Up/Down Navigate - Enter Restore - D Delete - Esc Back - F5 Refresh".to_string(),
                Style::default().fg(Color::DarkGray),
            )
        };
        frame.render_widget(Paragraph::new(hint).style(style), hint_area);
    }
}
