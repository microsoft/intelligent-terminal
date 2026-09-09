use crate::app::{App, QueueControl};
use ratatui::prelude::*;
use unicode_width::UnicodeWidthStr;

struct Control {
    action: QueueControl,
    shortcut: &'static str,
    label: String,
    enabled: bool,
}

fn controls(app: &App) -> Vec<Control> {
    let available = app.pending_queue_controls_available();
    let mut controls = Vec::new();
    if app.pending_queue_paused() {
        controls.push(Control {
            action: QueueControl::SendRemaining,
            shortcut: "Alt+S",
            label: format!("[Alt+S {}]", t!("queue.send_remaining")),
            enabled: available && app.pending_queue_can_resume(),
        });
    }
    if app.pending_queue_can_recall() {
        controls.push(Control {
            action: QueueControl::Recall,
            shortcut: "Alt+R",
            label: format!("[Alt+R {}]", t!("queue.recall")),
            enabled: available,
        });
    }
    controls.push(Control {
        action: QueueControl::Discard,
        shortcut: "Alt+D",
        label: format!("[Alt+D {}]", t!("queue.discard")),
        enabled: available,
    });
    controls
}

fn place_controls(
    controls: &[Control],
    width: u16,
    compact: bool,
) -> Vec<(usize, u16, u16, String)> {
    if width == 0 {
        return Vec::new();
    }
    let mut x = 0u16;
    let mut y = 0u16;
    controls
        .iter()
        .enumerate()
        .map(|(index, control)| {
            let label = if compact {
                format!("[{}]", control.shortcut)
            } else {
                control.label.clone()
            };
            let label = super::layout::truncate_to_width(&label, usize::from(width));
            let length = label.width().min(usize::from(width)) as u16;
            if x > 0 && x.saturating_add(length) > width {
                x = 0;
                y += 1;
            }
            let position = (index, x, y, label);
            x = x.saturating_add(length).saturating_add(u16::from(!compact));
            position
        })
        .collect()
}

pub(super) fn natural_height(app: &App, count: usize, width: u16) -> u16 {
    if count == 0 {
        return 0;
    }
    let control_rows = place_controls(&controls(app), width, false)
        .last()
        .map_or(0, |(_, _, y, _)| y + 1);
    (count.saturating_add(1).min(u16::MAX as usize) as u16).saturating_add(control_rows)
}

pub(super) fn render(frame: &mut Frame, app: &mut App, previews: &[String], area: Rect) {
    if previews.is_empty() || area.is_empty() {
        return;
    }
    let stopped = app.pending_queue_paused();
    let header = if stopped {
        t!("queue.paused_header", count = previews.len()).into_owned()
    } else {
        t!("queue.header", count = previews.len()).into_owned()
    };
    let controls = controls(app);
    let mut placements = place_controls(&controls, area.width, false);
    let available_control_rows = area.height.saturating_sub(1);
    let inline_full_width = header.width()
        + 1
        + controls
            .iter()
            .map(|control| control.label.width() + 1)
            .sum::<usize>()
            .saturating_sub(1);
    if placements
        .last()
        .is_some_and(|(_, _, y, _)| *y >= available_control_rows)
        && !(area.height == 1 && inline_full_width <= usize::from(area.width))
    {
        placements = place_controls(&controls, area.width, true);
    }
    // In a one-row emergency layout, share the line rather than hiding all actions.
    let inline = area.height == 1;
    let controls_width = if inline {
        placements
            .iter()
            .filter(|(_, _, y, _)| *y == 0)
            .map(|(_, x, _, label)| x.saturating_add(label.width() as u16))
            .max()
            .unwrap_or(0)
    } else {
        0
    };
    let header_width = if inline {
        area.width.saturating_sub(controls_width.saturating_add(1))
    } else {
        area.width
    };
    frame.render_widget(
        Line::styled(
            super::layout::truncate_to_width(&header, usize::from(header_width)),
            Style::default().fg(if stopped {
                Color::Yellow
            } else {
                Color::DarkGray
            }),
        ),
        Rect::new(area.x, area.y, header_width, 1),
    );
    let control_rows = placements
        .last()
        .map_or(0, |(_, _, y, _)| y + 1)
        .min(if inline { 1 } else { available_control_rows });
    let visible = previews
        .len()
        .min(usize::from(area.height.saturating_sub(1 + control_rows)));
    let hidden = previews.len().saturating_sub(visible);
    for (index, preview) in previews.iter().take(visible).enumerate() {
        let text = if index + 1 == visible && hidden > 0 {
            let suffix = format!(" +{hidden}");
            let width = usize::from(area.width).saturating_sub(suffix.len());
            format!(
                "{}{suffix}",
                super::layout::truncate_to_width(preview, width)
            )
        } else {
            preview.clone()
        };
        frame.render_widget(
            Line::styled(
                super::layout::truncate_to_width(&text, usize::from(area.width)),
                Style::default().fg(Color::DarkGray),
            ),
            Rect::new(area.x, area.y + 1 + index as u16, area.width, 1),
        );
    }
    for (index, x, y, label) in placements {
        if y >= control_rows {
            continue;
        }
        let control = &controls[index];
        let hit_area = Rect::new(
            area.x
                + x
                + if inline {
                    area.width.saturating_sub(controls_width)
                } else {
                    0
                },
            area.bottom() - control_rows + y,
            label.width().min(usize::from(area.width)) as u16,
            1,
        );
        frame.render_widget(
            Line::styled(
                label,
                if control.enabled {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ),
            hit_area,
        );
        app.register_queue_control(hit_area, control.action, control.enabled);
    }
}
