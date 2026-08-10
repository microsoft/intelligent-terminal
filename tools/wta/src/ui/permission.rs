use ratatui::prelude::*;
use ratatui::widgets::{Paragraph, Wrap};

use crate::app::{App, PermissionState};
use crate::theme;
use crate::ui::card::{self, CARD_MIN_SIZE};

/// Render the permission card. The action-panel planner reserves either at
/// least `CARD_MIN_SIZE` rows or a one-row compact fallback. The agent flow
/// is blocked on this prompt, so it must remain visible.
pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let Some(perm) = app.current_tab().permission.front() else {
        return;
    };

    if area.height < CARD_MIN_SIZE {
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
        // Header: optional kind glyph + the agent's own title. Unlike the
        // chat tool-call card, the target line below is never deduped
        // against the title — this card is a decision point, so
        // restating exactly what's being authorized is intentional even
        // if it repeats a preceding chat card (see
        // `client.rs::request_permission`).
        let header = match &perm.kind_label {
            Some(icon) => format!("{icon} {}", perm.title),
            None => perm.title.clone(),
        };
        let mut lines = vec![Line::styled(header, theme::CARD_DESCRIPTION)];
        if let Some(target) = &perm.target {
            if perm.target_is_command {
                // A command target can be several `;`-chained statements
                // (agents commonly batch multiple checks into one tool
                // call) — split it the same way as the chat tool-call
                // card so it doesn't render as an unreadable wall of
                // text (see `ui/command_format.rs`).
                for entry in crate::ui::command_format::command_display_lines(target) {
                    let text = match entry {
                        crate::ui::command_format::CommandLine::Statement(s) => format!("$ {s}"),
                        crate::ui::command_format::CommandLine::Folded { remaining } => {
                            format!("… (+{remaining} more)")
                        }
                    };
                    lines.push(Line::styled(text, theme::CARD_CODE));
                }
            } else {
                // Paths are shown as-is — the code styling alone
                // distinguishes it from the title.
                lines.push(Line::styled(target.clone(), theme::CARD_CODE));
            }
        }
        let content = Paragraph::new(lines)
            .alignment(crate::rtl::text_alignment())
            .wrap(Wrap { trim: false });
        frame.render_widget(content, content_inner);
    }

    let button_inner = card::inset_horizontal(button_area, 2);
    if button_inner.width > 0 {
        // Mark the targets of the `y` / `n` quick-keys so users can discover
        // them without a separate hint line. Position-based to stay in sync
        // with the matching logic in `App::handle_key` (both go through
        // `PermissionState::allow_index` / `reject_index`).
        let y_idx = perm.allow_index();
        let n_idx = perm.reject_index();
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
    let default_desc = t!("permission.compact.default_description").into_owned();
    let desc_one_line = perm
        .description
        .lines()
        .next()
        .unwrap_or(default_desc.as_str());
    let prefix_owned = t!("permission.compact.prefix").into_owned();
    let prefix = prefix_owned.as_str();
    let separator = "  ";
    let hint_owned = t!("permission.compact.hint").into_owned();
    let hint = hint_owned.as_str();
    // Reserve every non-description column up front (prefix + separator +
    // hint). Previously we only subtracted prefix+hint and let description
    // eat the separator — and forced `.max(1)` so 1 char of description
    // would render even when there was genuinely no room, pushing the hint
    // off-screen.
    let overhead =
        prefix.chars().count() + separator.chars().count() + hint.chars().count();
    let budget = (area.width as usize).saturating_sub(overhead);
    let total = desc_one_line.chars().count();
    let desc: String = if budget == 0 {
        // No room — show prefix + hint only; the hint must stay visible.
        String::new()
    } else if total <= budget {
        desc_one_line.to_string()
    } else {
        // Reserve one column for '…'.
        let take = budget.saturating_sub(1);
        let mut s: String = desc_one_line.chars().take(take).collect();
        s.push('…');
        s
    };
    let line = Line::from(vec![
        Span::styled(prefix, theme::BADGE_ACTIONABLE),
        Span::styled(desc, theme::CARD_DESCRIPTION),
        Span::raw(separator),
        Span::styled(hint, theme::DIM),
    ]);
    frame.render_widget(
        Paragraph::new(line).alignment(crate::rtl::text_alignment()),
        area,
    );
}
