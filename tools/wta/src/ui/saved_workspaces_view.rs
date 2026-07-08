use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
    Frame,
};
use unicode_width::UnicodeWidthStr;

use crate::app::SavedWorkspacesViewState;

const ACCENT_CYAN: Color = Color::Cyan;
const MUTED: Color = Color::Rgb(0x8b, 0x8b, 0x8b);

pub fn render(f: &mut Frame, area: Rect, view: &SavedWorkspacesViewState) {
    if view.loading {
        f.render_widget(
            Paragraph::new("Loading saved workspaces…").style(Style::default().fg(MUTED)),
            area,
        );
        return;
    }
    if view.entries.is_empty() {
        f.render_widget(
            Paragraph::new("No saved workspaces yet.").style(Style::default().fg(MUTED)),
            area,
        );
        return;
    }
    let row_width = area.width as usize;
    let items: Vec<ListItem> = view
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let marker = if i == view.selected { "> " } else { "  " };
            let mut style = Style::default();
            if i == view.selected {
                style = style.fg(ACCENT_CYAN).add_modifier(Modifier::BOLD);
            }
            let left = format!("{marker}{}", e.title);
            let open = if e.is_open { " (open)" } else { "" };
            let saved = format_saved_time(&e.saved_at);
            let right = if saved.is_empty() {
                open.to_string()
            } else {
                format!("{saved}{open}")
            };
            // Right-align the saved-time/meta at the row's right edge.
            let pad = row_width
                .saturating_sub(UnicodeWidthStr::width(left.as_str()))
                .saturating_sub(UnicodeWidthStr::width(right.as_str()))
                .max(1);
            ListItem::new(Line::from(vec![
                Span::styled(left, style),
                Span::styled(
                    format!("{}{right}", " ".repeat(pad)),
                    Style::default().fg(MUTED),
                ),
            ]))
        })
        .collect();
    let mut state = ListState::default();
    state.select(Some(view.selected));

    let (list_area, hint_area) = if area.height >= 2 {
        let hint = Rect {
            x: area.x,
            y: area.y + area.height - 1,
            width: area.width,
            height: 1,
        };
        let list = Rect {
            height: area.height - 1,
            ..area
        };
        (list, Some(hint))
    } else {
        (area, None)
    };
    f.render_stateful_widget(List::new(items), list_area, &mut state);

    if let Some(hint) = hint_area {
        let line = if view.confirm_delete {
            let title = view
                .entries
                .get(view.selected)
                .map(|e| e.title.as_str())
                .unwrap_or("");
            Paragraph::new(format!("Delete \"{title}\"?  y = yes · n = no"))
                .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
        } else {
            Paragraph::new("↑/↓ select · Enter restore · D delete · Esc close")
                .style(Style::default().fg(MUTED))
        };
        f.render_widget(line, hint);
    }
}

/// Absolute local time `YYYY-MM-DD HH:MM` from an epoch-milliseconds string
/// (`savedAt`). Empty when unparseable.
fn format_saved_time(saved_at_ms: &str) -> String {
    use chrono::{Local, TimeZone};
    let ms = match saved_at_ms.parse::<i64>() {
        Ok(v) if v > 0 => v,
        _ => return String::new(),
    };
    match Local.timestamp_millis_opt(ms).single() {
        Some(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        None => String::new(),
    }
}
