use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
    Frame,
};

use crate::app::SaveWorkspaceSelectState;

const ACCENT_CYAN: Color = Color::Cyan;
const MUTED: Color = Color::Rgb(0x8b, 0x8b, 0x8b);

pub fn render(f: &mut Frame, area: Rect, view: &SaveWorkspaceSelectState) {
    if view.loading {
        f.render_widget(
            Paragraph::new("Loading tabs…").style(Style::default().fg(MUTED)),
            area,
        );
        return;
    }

    // Reserve the last row for a hint line.
    let (body, hint) = if area.height >= 2 {
        (
            Rect {
                height: area.height - 1,
                ..area
            },
            Some(Rect {
                x: area.x,
                y: area.y + area.height - 1,
                width: area.width,
                height: 1,
            }),
        )
    } else {
        (area, None)
    };

    if view.in_title_input {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Saved workspace name: ", Style::default().fg(MUTED)),
                Span::styled(
                    format!("{}\u{2588}", view.title_input),
                    Style::default().fg(ACCENT_CYAN).add_modifier(Modifier::BOLD),
                ),
            ])),
            body,
        );
        if let Some(hint) = hint {
            f.render_widget(
                Paragraph::new("Enter save · Esc cancel").style(Style::default().fg(MUTED)),
                hint,
            );
        }
        return;
    }

    if view.rows.is_empty() {
        f.render_widget(
            Paragraph::new("No tabs to save.").style(Style::default().fg(MUTED)),
            body,
        );
        return;
    }

    let items: Vec<ListItem> = view
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let cursor = if i == view.selected { "> " } else { "  " };
            let checked = view.checked.get(i).copied().unwrap_or(false);
            let box_ = if checked { "[x] " } else { "[ ] " };
            let mut style = Style::default();
            if i == view.selected {
                style = style.fg(ACCENT_CYAN).add_modifier(Modifier::BOLD);
            }
            let title = if r.title.is_empty() {
                "(untitled)"
            } else {
                r.title.as_str()
            };
            let cwd = if r.cwd.is_empty() {
                String::new()
            } else {
                format!("   {}", r.cwd)
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{cursor}{box_}{}. {title}", i + 1), style),
                Span::styled(cwd, Style::default().fg(MUTED)),
            ]))
        })
        .collect();

    // Carve a top instruction line out of the body when there's room.
    let (header_area, list_area) = if body.height >= 2 {
        (
            Rect { height: 1, ..body },
            Rect {
                y: body.y + 1,
                height: body.height - 1,
                ..body
            },
        )
    } else {
        (Rect { height: 0, ..body }, body)
    };
    if header_area.height >= 1 {
        f.render_widget(
            Paragraph::new("Select tabs to save (all checked by default) — Space to check/uncheck, Enter to name & save.")
                .style(Style::default().fg(MUTED)),
            header_area,
        );
    }

    let mut state = ListState::default();
    state.select(Some(view.selected.min(view.rows.len().saturating_sub(1))));
    f.render_stateful_widget(List::new(items), list_area, &mut state);

    if let Some(hint) = hint {
        f.render_widget(
            Paragraph::new("Space toggle · ↑/↓ select · Enter confirm · Esc cancel")
                .style(Style::default().fg(MUTED)),
            hint,
        );
    }
}
