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
    query: &str,
    search_focused: bool,
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
    let search_area = Rect {
        height: inner.height.min(1),
        ..inner
    };
    let list_area = Rect {
        y: inner.y.saturating_add(2),
        height: inner.height.saturating_sub(2),
        ..inner
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

    let search_value = if query.is_empty() {
        Span::styled(
            if search_focused {
                "Search title or CWD"
            } else {
                "Press / to search title or CWD"
            },
            Style::default().fg(Color::DarkGray),
        )
    } else {
        Span::raw(query.to_string())
    };
    let search_label_style = if search_focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let mut search_spans = vec![Span::styled("Search: ", search_label_style), search_value];
    if search_focused {
        search_spans.push(Span::styled("▏", Style::default().fg(Color::Cyan)));
    }
    frame.render_widget(Paragraph::new(Line::from(search_spans)), search_area);

    let visible_sessions = sessions
        .iter()
        .filter(|session| matches_query(session, query))
        .collect::<Vec<_>>();
    if loading {
        frame.render_widget(Paragraph::new("Loading shell sessions..."), list_area);
    } else if let Some(error) = error {
        frame.render_widget(
            Paragraph::new(error.to_string()).style(Style::default().fg(Color::Red)),
            list_area,
        );
    } else if sessions.is_empty() {
        frame.render_widget(
            Paragraph::new("No saved shell sessions").style(Style::default().fg(Color::DarkGray)),
            list_area,
        );
    } else if visible_sessions.is_empty() {
        frame.render_widget(
            Paragraph::new("No matching shell sessions")
                .style(Style::default().fg(Color::DarkGray)),
            list_area,
        );
    } else {
        let selected = list_state.selected();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs() as i64);
        let row_width = list_area.width.saturating_sub(2) as usize;
        let rows = visible_sessions
            .into_iter()
            .enumerate()
            .map(|(index, session)| {
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
                let mut spans = vec![Span::styled(marker, style)];
                spans.extend(highlight_matches(&row.name, query, style));
                if !row.cwd.is_empty() {
                    spans.push(Span::raw("  "));
                    spans.extend(highlight_matches(
                        &row.cwd,
                        query,
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                spans.push(Span::raw(row.padding));
                spans.push(Span::styled(row.age, style));
                ListItem::new(Line::from(spans))
            });
        frame.render_stateful_widget(List::new(rows), list_area, list_state);
    }

    if area.height > 0 {
        let hint_area = Rect {
            x: area.x,
            y: area.y + area.height - 1,
            width: area.width,
            height: 1,
        };
        let (hint, style) = if delete_in_flight {
            (
                "Deleting shell session...".to_string(),
                Style::default().fg(Color::Yellow),
            )
        } else if let Some(id) = delete_confirmation {
            let name = sessions
                .iter()
                .find(|session| session.id == id)
                .map(|session| session.name.as_str())
                .unwrap_or(id);
            (
                format!("Delete \"{name}\"? Y Confirm - N/Esc Cancel"),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            (
                "(↑ ↓ Navigate • Enter Restore • Esc Back • D Delete • F5 Refresh)".to_string(),
                Style::default().fg(Color::DarkGray),
            )
        };
        frame.render_widget(Paragraph::new(hint).style(style), hint_area);
    }
}

pub(crate) fn matches_query(
    session: &crate::shell_session_store::ShellSessionSummary,
    query: &str,
) -> bool {
    if query.is_empty() {
        return true;
    }
    !case_insensitive_match_ranges(&session.name, query).is_empty()
        || !case_insensitive_match_ranges(&session.active_pane_cwd, query).is_empty()
}

fn highlight_matches(text: &str, query: &str, base_style: Style) -> Vec<Span<'static>> {
    let ranges = case_insensitive_match_ranges(text, query);
    if ranges.is_empty() {
        return vec![Span::styled(text.to_string(), base_style)];
    }

    let highlight_style = base_style
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let mut spans = Vec::with_capacity(ranges.len() * 2 + 1);
    let mut cursor = 0;
    for (start, end) in ranges {
        if cursor < start {
            spans.push(Span::styled(text[cursor..start].to_string(), base_style));
        }
        spans.push(Span::styled(text[start..end].to_string(), highlight_style));
        cursor = end;
    }
    if cursor < text.len() {
        spans.push(Span::styled(text[cursor..].to_string(), base_style));
    }
    spans
}

fn case_insensitive_match_ranges(text: &str, query: &str) -> Vec<(usize, usize)> {
    if query.is_empty() {
        return Vec::new();
    }

    let mut normalized = String::new();
    let mut original_ranges = Vec::new();
    for (start, character) in text.char_indices() {
        let end = start + character.len_utf8();
        let folded = character.to_lowercase().to_string();
        original_ranges.extend(std::iter::repeat_n((start, end), folded.len()));
        normalized.push_str(&folded);
    }

    let folded_query = query.to_lowercase();
    normalized
        .match_indices(&folded_query)
        .filter_map(|(start, matched)| {
            let end = start + matched.len();
            Some((
                original_ranges.get(start)?.0,
                original_ranges.get(end - 1)?.1,
            ))
        })
        .collect()
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
        super::layout::truncate_to_width(&session.active_pane_cwd, left_width - name_width - 2)
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
    fn search_matches_title_and_cwd_case_insensitively() {
        let mut item = summary();
        item.name = "PowerShell".to_string();
        assert!(matches_query(&item, "po"));
        assert!(matches_query(&item, "POWER"));

        item.name = "cmd".to_string();
        item.active_pane_cwd = r"C:\repos\portal".to_string();
        assert!(matches_query(&item, "PO"));
        assert!(!matches_query(&item, "bash"));
    }

    #[test]
    fn search_highlights_each_matching_title_and_cwd_segment() {
        let title = highlight_matches("PowerShell empower", "po", Style::default());
        let highlighted = title
            .iter()
            .filter(|span| span.style.fg == Some(Color::Yellow))
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>();
        assert_eq!(highlighted, vec!["Po", "po"]);

        let cwd = highlight_matches(
            r"C:\repos\Portal",
            "po",
            Style::default().fg(Color::DarkGray),
        );
        let highlighted = cwd
            .iter()
            .filter(|span| span.style.fg == Some(Color::Yellow))
            .collect::<Vec<_>>();
        assert_eq!(
            highlighted
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<Vec<_>>(),
            vec!["po", "Po"]
        );
        assert!(highlighted
            .iter()
            .all(|span| span.style.add_modifier.contains(Modifier::UNDERLINED)));
    }

    #[test]
    fn search_highlighting_preserves_unicode_boundaries() {
        let spans = highlight_matches("İstanbul", "i\u{307}", Style::default());
        assert_eq!(
            spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            "İstanbul"
        );
        assert_eq!(spans[0].content.as_ref(), "İ");
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
