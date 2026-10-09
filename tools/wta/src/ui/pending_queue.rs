use ratatui::prelude::*;

pub(super) fn render(frame: &mut Frame, count: usize, area: Rect) {
    if count == 0 || area.is_empty() {
        return;
    }
    let label = if count == 1 {
        t!("queue.header_one", count = count)
    } else {
        t!("queue.header", count = count)
    };
    frame.render_widget(
        Line::styled(
            super::layout::truncate_to_width(&label, usize::from(area.width)),
            Style::default().fg(Color::DarkGray),
        ),
        Rect::new(area.x, area.y, area.width, 1),
    );
}
