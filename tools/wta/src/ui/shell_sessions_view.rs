use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
    Frame,
};
use std::time::{SystemTime, UNIX_EPOCH};
use unicode_width::UnicodeWidthStr;

pub fn render(
    frame: &mut Frame,
    area: Rect,
    sessions: &[crate::shell_session_store::ShellSessionSummary],
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
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs() as i64);
        let row_width = inner.width.saturating_sub(2) as usize;
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
            let row = format_row(session, row_width, now);
            let mut spans = vec![
                Span::styled(marker, style),
                Span::styled(row.name, style),
            ];
            if !row.cwd.is_empty() {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    row.cwd,
                    Style::default().fg(Color::DarkGray),
                ));
            }
            spans.push(Span::raw(row.padding));
            spans.push(Span::styled(row.age, style));
            ListItem::new(Line::from(spans))
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

struct FormattedRow {
    name: String,
    cwd: String,
    padding: String,
    age: String,
}

fn format_row(
    session: &crate::shell_session_store::ShellSessionSummary,
    width: usize,
    now: i64,
) -> FormattedRow {
    let age = format_relative_age(session.last_used_at, now);
    let age_width = UnicodeWidthStr::width(age.as_str());
    if width <= age_width + 2 {
        return FormattedRow {
            name: super::layout::truncate_to_width(&session.name, width),
            cwd: String::new(),
            padding: String::new(),
            age: String::new(),
        };
    }

    let left_width = width - age_width - 2;
    let name = super::layout::truncate_to_width(&session.name, left_width);
    let name_width = UnicodeWidthStr::width(name.as_str());
    let cwd = if session.active_pane_cwd.is_empty() || name_width + 2 >= left_width {
        String::new()
    } else {
        super::layout::truncate_to_width(
            &session.active_pane_cwd,
            left_width - name_width - 2,
        )
    };
    let separator_width = usize::from(!cwd.is_empty()) * 2;
    let content_width = name_width + separator_width + UnicodeWidthStr::width(cwd.as_str());
    let gap = width
        .saturating_sub(content_width)
        .saturating_sub(age_width);
    FormattedRow {
        name,
        cwd,
        padding: " ".repeat(gap),
        age,
    }
}

fn format_relative_age(last_used_at: i64, now: i64) -> String {
    let elapsed = now.saturating_sub(last_used_at);
    if elapsed < 60 {
        "just now".to_string()
    } else if elapsed < 3_600 {
        relative_unit(elapsed / 60, "minute")
    } else if elapsed < 86_400 {
        relative_unit(elapsed / 3_600, "hour")
    } else {
        relative_unit(elapsed / 86_400, "day")
    }
}

fn relative_unit(value: i64, unit: &str) -> String {
    let suffix = if value == 1 { "" } else { "s" };
    format!("{value} {unit}{suffix} ago")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary() -> crate::shell_session_store::ShellSessionSummary {
        crate::shell_session_store::ShellSessionSummary {
            id: "id".to_string(),
            name: "2panes".to_string(),
            active_pane_cwd: r"C:\Windows\system32".to_string(),
            last_used_at: 86_400,
        }
    }

    #[test]
    fn relative_age_uses_readable_units() {
        assert_eq!(format_relative_age(100, 100), "just now");
        assert_eq!(format_relative_age(100, 160), "1 minute ago");
        assert_eq!(format_relative_age(100, 7_300), "2 hours ago");
        assert_eq!(format_relative_age(100, 86_500), "1 day ago");
    }

    #[test]
    fn row_places_age_at_right_edge() {
        let row = format_row(&summary(), 50, 172_800);
        assert_eq!(row.name, "2panes");
        assert_eq!(row.cwd, r"C:\Windows\system32");
        assert_eq!(row.age, "1 day ago");
        assert_eq!(
            UnicodeWidthStr::width(row.name.as_str())
                + 2
                + UnicodeWidthStr::width(row.cwd.as_str())
                + row.padding.len()
                + UnicodeWidthStr::width(row.age.as_str()),
            50
        );
    }

    #[test]
    fn narrow_row_truncates_without_overflowing() {
        let row = format_row(&summary(), 10, 172_800);
        assert!(UnicodeWidthStr::width(row.name.as_str()) <= 10);
        assert!(row.cwd.is_empty());
        assert!(row.age.is_empty());
    }
}
