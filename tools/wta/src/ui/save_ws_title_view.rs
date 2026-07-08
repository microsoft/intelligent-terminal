use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::SaveWorkspaceSelectState;

const ACCENT_CYAN: Color = Color::Cyan;
const MUTED: Color = Color::Rgb(0x8b, 0x8b, 0x8b);

pub fn render(f: &mut Frame, area: Rect, view: &SaveWorkspaceSelectState) {
    if view.loading {
        f.render_widget(
            Paragraph::new(t!("commands.save_ws.loading_tabs").into_owned())
                .style(Style::default().fg(MUTED)),
            area,
        );
        return;
    }

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

    if view.rows.is_empty() {
        f.render_widget(
            Paragraph::new(t!("commands.save_ws.no_tabs").into_owned())
                .style(Style::default().fg(MUTED)),
            body,
        );
        return;
    }

    let header = t!("commands.save_ws.summary").into_owned();
    let label = t!("commands.save_ws.title_prompt").into_owned();
    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(header, Style::default().fg(MUTED))),
            Line::from(vec![
                Span::styled(label, Style::default().fg(MUTED)),
                Span::styled(
                    format!("{}\u{2588}", view.title_input),
                    Style::default().fg(ACCENT_CYAN).add_modifier(Modifier::BOLD),
                ),
            ]),
        ]),
        body,
    );

    if let Some(hint) = hint {
        f.render_widget(
            Paragraph::new(t!("commands.save_ws.title_hint").into_owned())
                .style(Style::default().fg(MUTED)),
            hint,
        );
    }
}
