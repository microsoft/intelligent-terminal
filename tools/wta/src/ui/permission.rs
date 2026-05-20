use ratatui::prelude::*;
use ratatui::widgets::{Paragraph, Wrap};

use crate::app::{App, PermissionState};
use crate::theme;
use crate::ui::card::{self, CARD_MIN_HEIGHT};

/// Render the permission card. Embedded above the input box; `layout.rs`
/// reserves the row budget via `App::permission_panel_height`, which is
/// either ≥ `CARD_MIN_HEIGHT` (full card) or exactly 1 (compact fallback —
/// the agent flow is blocked on this prompt, so we must remain visible).
pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let perm = match &app.current_tab().permission {
        Some(p) => p,
        None => return,
    };

    if area.height < CARD_MIN_HEIGHT {
        render_compact(frame, perm, area);
        return;
    }

    let Some((content_area, button_area)) =
        card::render_card_shell(frame, area, theme::CARD_BORDER)
    else {
        render_compact(frame, perm, area);
        return;
    };

    let content_inner = card::inset_horizontal(content_area, 2);
    if content_inner.width > 0 {
        let content = Paragraph::new(perm.description.clone())
            .style(theme::CARD_DESCRIPTION)
            .wrap(Wrap { trim: false });
        frame.render_widget(content, content_inner);
    }

    let button_inner = card::inset_horizontal(button_area, 2);
    if button_inner.width > 0 {
        // Mark the targets of the `y` / `n` quick-keys so users can discover
        // them without a separate hint line. Position-based to stay in sync
        // with the matching logic in `App::handle_key`.
        let y_idx = perm.options.iter().position(|o| o.kind.contains("allow"));
        let n_idx = perm.options.iter().position(|o| o.kind.contains("reject"));
        let labels: Vec<String> = perm
            .options
            .iter()
            .enumerate()
            .map(|(i, o)| {
                if Some(i) == y_idx {
                    format!("[Y] {}", o.name)
                } else if Some(i) == n_idx {
                    format!("[N] {}", o.name)
                } else {
                    o.name.clone()
                }
            })
            .collect();
        card::render_buttons(frame, button_inner, &labels, Some(perm.selected));
    }
}

/// 1-row fallback when the panel can't fit a full card. Keeps the user
/// informed that a permission is pending and what to press — the agent is
/// blocked until they answer, so silently hiding the card would deadlock the
/// flow.
fn render_compact(frame: &mut Frame, perm: &PermissionState, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let desc_one_line = perm
        .description
        .lines()
        .next()
        .unwrap_or("Permission requested");
    let hint = "[Y/N to answer · resize for full card]";
    let budget = area.width.saturating_sub(hint.chars().count() as u16 + 4) as usize;
    let mut desc: String = desc_one_line.chars().take(budget.max(1)).collect();
    if desc_one_line.chars().count() > budget {
        desc.push('…');
    }
    let line = Line::from(vec![
        Span::styled("[!] ", theme::BADGE_ACTIONABLE),
        Span::styled(desc, theme::CARD_DESCRIPTION),
        Span::raw("  "),
        Span::styled(hint, theme::DIM),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}
