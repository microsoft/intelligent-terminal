use ratatui::prelude::*;
use ratatui::widgets::{Paragraph, Wrap};

use crate::app::{rec_card_height, App};
use crate::coordinator::{OpenTarget, RecommendationChoice, RecommendedAction};
use crate::theme;
use crate::ui::card::{self, CARD_MIN_SIZE};

/// Render the recommendations panel. Pure: callers (layout.rs) must call
/// `App::sync_rec_scroll_max` first so `rec_scroll.offset` is already clamped
/// when we paint.
///
/// Cards are positioned in a virtual canvas (stacked top-to-bottom by their
/// natural heights), then shifted up by `rec_scroll`. The navigation hint is
/// rendered separately by `render_hint` so it can sit directly above the
/// input box (see `layout.rs`).
///
/// Cards taller than the remaining cards region render **truncated** at the
/// height that fits — `render_card` lets cassowary squash the inner content
/// area, so the user keeps the border, button, and as many content rows as
/// fit. This avoids the previous "tall card in squashed pane → nothing
/// renders" failure mode.
pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let Some(recs) = app.current_tab().turn.recommendations() else { return };
    if area.width == 0 || area.height == 0 {
        return;
    }

    let rec_scroll = app.current_tab().rec_scroll.offset;
    let cards_bottom = area.y.saturating_add(area.height);

    // `area` is `h_rec[1]` (post-padding), but `rec_card_height` /
    // `rec_panel_height` / `sync_rec_scroll_max` all root their wrap math at
    // `main_area.width` (see `CARD_H_CHROME`). Use the same basis here or
    // wrap rows go 2 cells narrower at render than at predict, clipping the
    // bottom card and undercounting `rec_scroll.max`.
    let panel_width = app.main_area_width();

    let mut canvas_top = 0usize;
    for (idx, choice) in recs.choices.iter().enumerate() {
        let h = rec_card_height(choice, panel_width);
        if canvas_top >= rec_scroll {
            let card_h = h.saturating_sub(1) as u16; // last canvas row is inter-card gap
            let y = area.y + (canvas_top - rec_scroll) as u16;
            let available = cards_bottom.saturating_sub(y);
            if available < CARD_MIN_SIZE {
                break; // card shell bails below this — nothing useful to draw
            }
            let render_h = card_h.min(available);
            // Cards use the full h_rec[1] width so their left border sits in
            // the same column as the chat's green dot (column 1 of main_area)
            // and the right border is symmetric on the opposite edge.
            let card_area = Rect {
                x: area.x,
                y,
                width: area.width,
                height: render_h,
            };
            render_card(frame, app, card_area, choice, idx);
        }
        canvas_top += h;
    }
}

/// Render the recommendations navigation hint. Called by `layout.rs` to
/// place this row directly above the input box, regardless of how tall the
/// rec panel is.
pub fn render_hint(frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let hint = Paragraph::new(Line::from(Span::styled(
        t!("recommendations.nav_hint").into_owned(),
        theme::DIM,
    )));
    frame.render_widget(hint, area);
}

fn render_card(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    choice: &RecommendationChoice,
    idx: usize,
) {
    if area.width < CARD_MIN_SIZE || area.height < CARD_MIN_SIZE {
        return;
    }

    // The key handlers already gate Up/Down/Left/Right/Tab/Enter on
    // `input.is_empty()` — i.e. you can navigate cards only when no text is
    // in the prompt box. Mirror that here so the focus highlight matches:
    // typing should visibly take focus away from the cards, otherwise the
    // user sees "two focuses" and can't tell whether Enter will activate
    // the card or submit the prompt.
    let nav_mode = app.current_tab().input.is_empty();
    let is_selected = nav_mode && idx == app.current_tab().selected_recommendation;
    let border_style = if is_selected {
        theme::CARD_BORDER_SELECTED
    } else {
        theme::CARD_BORDER
    };

    let Some((content_area, button_area)) = card::render_card_shell(frame, area, border_style)
    else {
        return;
    };

    let (command_text, buttons, body_kind) = extract_card_content(choice, app, is_selected);
    let body_style = match body_kind {
        CardBodyKind::Code => theme::CARD_CODE,
        CardBodyKind::Description => theme::CARD_DESCRIPTION,
    };
    let content_inner = card::inset_horizontal(content_area, 2);
    if content_inner.width > 0 {
        let content = Paragraph::new(command_text)
            .style(body_style)
            .wrap(Wrap { trim: false });
        frame.render_widget(content, content_inner);
    }

    let button_inner = card::inset_horizontal(button_area, 2);
    if button_inner.width > 0 {
        let focused = if is_selected {
            Some(app.current_tab().selected_button)
        } else {
            None
        };
        card::render_buttons(frame, button_inner, &buttons, focused);
    }
}

enum CardBodyKind {
    Code,
    Description,
}

fn extract_card_content(
    choice: &RecommendationChoice,
    _app: &App,
    _is_selected: bool,
) -> (String, Vec<String>, CardBodyKind) {
    for action in &choice.actions {
        match action {
            RecommendedAction::Send { input, .. } => {
                return (
                    input.clone(),
                    vec![
                        t!("recommendations.button_run_command").into_owned(),
                        t!("recommendations.button_insert_in_terminal").into_owned(),
                    ],
                    CardBodyKind::Code,
                );
            }
            RecommendedAction::OpenAndSend {
                target,
                input,
                agent,
                ..
            } => {
                let fallback = t!("recommendations.agent_fallback").into_owned();
                let agent_label = agent.as_deref().unwrap_or(&fallback);
                let display = t!("recommendations.open_and_send_display",
                    agent = agent_label, input = input.as_str()).into_owned();
                let target_label = match target {
                    OpenTarget::Tab => t!("recommendations.button_open_in_new_tab").into_owned(),
                    OpenTarget::Panel => t!("recommendations.button_open_in_new_panel").into_owned(),
                };
                return (display, vec![target_label], CardBodyKind::Code);
            }
            RecommendedAction::Open {
                target,
                cwd,
                title,
                direction,
                ..
            } => {
                let kind = match target {
                    OpenTarget::Tab => t!("recommendations.open_kind_tab").into_owned(),
                    OpenTarget::Panel => match direction.as_deref() {
                        Some(d) if !d.is_empty() => {
                            t!("recommendations.open_kind_panel_direction", direction = d).into_owned()
                        }
                        _ => t!("recommendations.open_kind_panel").into_owned(),
                    },
                };
                let display = match (title.as_deref(), cwd.as_deref()) {
                    (Some(t), Some(c)) if !t.is_empty() && !c.is_empty() => {
                        t!("recommendations.open_new_with_title_and_cwd",
                            kind = kind.as_str(), title = t, cwd = c).into_owned()
                    }
                    (Some(t), _) if !t.is_empty() => {
                        t!("recommendations.open_new_with_title",
                            kind = kind.as_str(), title = t).into_owned()
                    }
                    (_, Some(c)) if !c.is_empty() => {
                        t!("recommendations.open_new_with_cwd",
                            kind = kind.as_str(), cwd = c).into_owned()
                    }
                    _ => t!("recommendations.open_new_empty", kind = kind.as_str()).into_owned(),
                };
                let button = match target {
                    OpenTarget::Tab => t!("recommendations.button_open_tab").into_owned(),
                    OpenTarget::Panel => t!("recommendations.button_open_panel").into_owned(),
                };
                return (display, vec![button], CardBodyKind::Description);
            }
        }
    }

    (
        choice.title.clone(),
        vec![t!("recommendations.button_execute").into_owned()],
        CardBodyKind::Description,
    )
}
