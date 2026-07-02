use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
    Frame,
};

use crate::app::SavedTabsViewState;

const ACCENT_CYAN: Color = Color::Cyan;
const MUTED: Color = Color::Rgb(0x8b, 0x8b, 0x8b);

pub fn render(f: &mut Frame, area: Rect, view: &SavedTabsViewState) {
    if view.loading {
        f.render_widget(
            Paragraph::new("Loading saved tabs…").style(Style::default().fg(MUTED)),
            area,
        );
        return;
    }
    if view.entries.is_empty() {
        f.render_widget(
            Paragraph::new("No saved tabs yet.").style(Style::default().fg(MUTED)),
            area,
        );
        return;
    }
    let items: Vec<ListItem> = view
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let marker = if i == view.selected { "> " } else { "  " };
            let open = if e.is_open { "  (open)" } else { "" };
            let mut style = Style::default();
            if i == view.selected {
                style = style.fg(ACCENT_CYAN).add_modifier(Modifier::BOLD);
            }
            ListItem::new(Line::from(vec![
                Span::styled(format!("{marker}{}", e.title), style),
                Span::styled(open, Style::default().fg(MUTED)),
            ]))
        })
        .collect();
    let mut state = ListState::default();
    state.select(Some(view.selected));
    f.render_stateful_widget(List::new(items), area, &mut state);

    let hint = Rect {
        x: area.x,
        y: area.y + area.height.saturating_sub(1),
        width: area.width,
        height: 1,
    };
    f.render_widget(
        Paragraph::new("↑/↓ select · Enter restore · D delete · Esc close")
            .style(Style::default().fg(MUTED)),
        hint,
    );
}
