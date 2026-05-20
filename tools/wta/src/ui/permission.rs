use ratatui::prelude::*;
use ratatui::widgets::{Paragraph, Wrap};

use crate::app::App;
use crate::theme;
use crate::ui::card;

/// Render the permission card. Embedded above the input box in the same
/// chrome as recommendation cards — `layout.rs` reserves the row budget via
/// `App::permission_panel_height`, so this just paints into the slot.
pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let perm = match &app.current_tab().permission {
        Some(p) => p,
        None => return,
    };

    let Some((content_area, button_area)) =
        card::render_card_shell(frame, area, theme::CARD_BORDER)
    else {
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
