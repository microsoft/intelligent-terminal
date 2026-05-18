use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::{rec_card_height, App};
use crate::coordinator::{OpenTarget, RecommendationChoice, RecommendedAction};
use crate::theme;

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

    let mut canvas_top = 0usize;
    for (idx, choice) in recs.choices.iter().enumerate() {
        let h = rec_card_height(choice, area.width);
        if canvas_top >= rec_scroll {
            let card_h = h.saturating_sub(1) as u16; // last canvas row is inter-card gap
            let y = area.y + (canvas_top - rec_scroll) as u16;
            let available = cards_bottom.saturating_sub(y);
            if available < 4 {
                break; // render_card bails below 4 — nothing useful to draw
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
        "(↑ ↓ to navigate • Enter to select • Esc to cancel)",
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
    if area.width < 4 || area.height < 4 {
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

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height < 3 || inner.width == 0 {
        return;
    }

    let inner_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);
    let content_area = inner_chunks[0];
    let divider_y = inner_chunks[1].y;
    let button_area = inner_chunks[2];

    let (command_text, buttons, body_kind) = extract_card_content(choice, app, is_selected);
    let body_style = match body_kind {
        CardBodyKind::Code => theme::CARD_CODE,
        CardBodyKind::Description => theme::CARD_DESCRIPTION,
    };
    let content_inner = inset_horizontal(content_area, 2);
    if content_inner.width > 0 {
        let content = Paragraph::new(command_text)
            .style(body_style)
            .wrap(Wrap { trim: false });
        frame.render_widget(content, content_inner);
    }

    render_divider(frame.buffer_mut(), area, divider_y, border_style);

    let button_inner = inset_horizontal(button_area, 2);
    if button_inner.width > 0 {
        render_buttons(
            frame,
            button_inner,
            &buttons,
            is_selected,
            app.current_tab().selected_button,
        );
    }
}

fn inset_horizontal(r: Rect, n: u16) -> Rect {
    Rect {
        x: r.x.saturating_add(n),
        y: r.y,
        width: r.width.saturating_sub(n.saturating_mul(2)),
        height: r.height,
    }
}

fn render_divider(buf: &mut Buffer, area: Rect, y: u16, border_style: Style) {
    if y < area.y || y >= area.y.saturating_add(area.height) {
        return;
    }
    if area.width < 2 {
        return;
    }
    let left = area.x;
    let right = area.x.saturating_add(area.width).saturating_sub(1);
    if left >= right {
        return;
    }
    buf.set_string(left, y, "├", border_style);
    let middle_width = area.width.saturating_sub(2) as usize;
    if middle_width > 0 {
        buf.set_string(left.saturating_add(1), y, "─".repeat(middle_width), border_style);
    }
    buf.set_string(right, y, "┤", border_style);
}

fn render_buttons(
    frame: &mut Frame,
    area: Rect,
    buttons: &[String],
    is_selected: bool,
    focused_button: usize,
) {
    let mut pieces: Vec<(String, Style)> = Vec::new();
    for (i, label) in buttons.iter().enumerate() {
        if i > 0 {
            pieces.push(("   ".into(), Style::default()));
        }
        let style = if is_selected && i == focused_button {
            theme::BUTTON_FOCUSED
        } else {
            theme::BUTTON_PLAIN
        };
        pieces.push((label.clone(), style));
    }

    // Left-align buttons so they sit under the command text column.
    let mut spans: Vec<Span> = Vec::with_capacity(pieces.len());
    for (text, style) in pieces {
        spans.push(Span::styled(text, style));
    }

    let para = Paragraph::new(Line::from(spans));
    frame.render_widget(para, area);
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
                    vec!["[ Run command ]".into(), "Insert in Terminal".into()],
                    CardBodyKind::Code,
                );
            }
            RecommendedAction::OpenAndSend {
                target,
                input,
                agent,
                ..
            } => {
                let agent_label = agent.as_deref().unwrap_or("agent");
                let display = format!("{}: {}", agent_label, input);
                let target_label = match target {
                    OpenTarget::Tab => "Open in New Tab ↵",
                    OpenTarget::Panel => "Open in New Panel ↵",
                };
                return (display, vec![target_label.into()], CardBodyKind::Code);
            }
            RecommendedAction::Open {
                target,
                cwd,
                title,
                direction,
                ..
            } => {
                let kind = match target {
                    OpenTarget::Tab => "tab".to_string(),
                    OpenTarget::Panel => match direction.as_deref() {
                        Some(d) if !d.is_empty() => format!("panel ({})", d),
                        _ => "panel".to_string(),
                    },
                };
                let display = match (title.as_deref(), cwd.as_deref()) {
                    (Some(t), Some(c)) if !t.is_empty() && !c.is_empty() => {
                        format!("New {} ({}) in {}", kind, t, c)
                    }
                    (Some(t), _) if !t.is_empty() => format!("New {} ({})", kind, t),
                    (_, Some(c)) if !c.is_empty() => format!("New {} in {}", kind, c),
                    _ => format!("New empty {}", kind),
                };
                let button = match target {
                    OpenTarget::Tab => "Open Tab ↵",
                    OpenTarget::Panel => "Open Panel ↵",
                };
                return (display, vec![button.into()], CardBodyKind::Description);
            }
        }
    }

    (
        choice.title.clone(),
        vec!["Execute ↵".into()],
        CardBodyKind::Description,
    )
}
