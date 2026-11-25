use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph, Wrap};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::App;
use crate::coordinator::{OpenTarget, RecommendationChoice, RecommendedAction};
use crate::theme;
use crate::ui::action_panel::{recommendation_card_height, PanelMode};
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
/// renders" failure mode. Returns the bounds of cards with painted content.
pub(crate) fn render(frame: &mut Frame, app: &App, area: Rect, mode: PanelMode) -> Option<Rect> {
    let Some(recs) = app.current_tab().turn.recommendations() else {
        return None;
    };
    if mode == PanelMode::Hidden || area.width == 0 || area.height == 0 {
        return None;
    }
    if mode == PanelMode::Compact {
        let painted = render_compact(frame, app, area);
        return painted.then_some(area);
    }

    let mut rendered: Option<Rect> = None;
    let rec_scroll = app.current_tab().rec_scroll.offset;
    let cards_bottom = area.y.saturating_add(area.height);

    // `area` is `h_rec[1]` (post-padding), but card-height prediction and
    // scroll bounds both root their wrap math at `main_area.width` (see
    // `CARD_H_CHROME`). Use the same basis here or
    // wrap rows go 2 cells narrower at render than at predict, clipping the
    // bottom card and undercounting `rec_scroll.max`.
    let panel_width = app.main_area_width();

    let mut canvas_top = 0usize;
    for (idx, choice) in recs.choices.iter().enumerate() {
        let h = recommendation_card_height(choice, panel_width);
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
            let painted = render_card(frame, app, card_area, choice, idx);
            if painted {
                rendered = Some(rendered.map_or(card_area, |bounds| bounds.union(card_area)));
            }
        }
        canvas_top += h;
    }
    rendered
}

fn render_compact(frame: &mut Frame, app: &App, area: Rect) -> bool {
    let Some(recommendations) = app.current_tab().turn.recommendations() else {
        return false;
    };
    let selected = app
        .current_tab()
        .selected_recommendation
        .min(recommendations.choices.len().saturating_sub(1));
    let Some(choice) = recommendations.choices.get(selected) else {
        return false;
    };
    let (summary, buttons, body_kind) = extract_card_content(choice);
    let marker = "○";
    let position = if recommendations.choices.len() > 1 {
        format!(" ↑↓ {}/{} ", selected + 1, recommendations.choices.len())
    } else {
        " ".to_string()
    };
    let prefix = format!("{marker}{position}");
    let summary_width =
        (area.width as usize).saturating_sub(UnicodeWidthStr::width(prefix.as_str()));
    let summary = truncate_compact(&summary.replace(['\r', '\n'], " "), summary_width);
    let body_style = match body_kind {
        CardBodyKind::Code => theme::CARD_CODE,
        CardBodyKind::Description => theme::CARD_DESCRIPTION,
    };
    frame.render_widget(
        compact_summary_paragraph(
            Line::from(vec![
                Span::styled(prefix, theme::TOOL_CALL_PENDING),
                Span::styled(summary, body_style),
            ]),
            crate::rtl::text_alignment(),
        ),
        Rect { height: 1, ..area },
    );

    if area.height > 1 {
        let button_area = Rect {
            y: area.y + 1,
            height: 1,
            ..area
        };
        let focused = (app.current_tab().recommendation_focus
            == crate::app::RecommendationFocus::Button)
            .then_some(app.current_tab().selected_button);
        card::render_buttons(frame, button_area, &buttons, focused);
    }
    summary_width > 1
}

fn truncate_compact(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(text) <= width {
        return text.to_string();
    }
    let content_width = width.saturating_sub(1);
    let mut used = 0;
    let mut result = String::new();
    for character in text.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used + character_width > content_width {
            break;
        }
        result.push(character);
        used += character_width;
    }
    result.push('…');
    result
}

fn compact_summary_paragraph(line: Line<'static>, alignment: Alignment) -> Paragraph<'static> {
    Paragraph::new(line).alignment(alignment)
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
    )))
    .alignment(crate::rtl::text_alignment());
    frame.render_widget(hint, area);
}

fn render_card(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    choice: &RecommendationChoice,
    idx: usize,
) -> bool {
    if area.width < CARD_MIN_SIZE || area.height < CARD_MIN_SIZE {
        return false;
    }

    // A selected card only paints button focus while recommendation navigation
    // targets it; Up/Down can move that focus to the input while cards remain
    // visible.
    let is_selected = idx == app.current_tab().selected_recommendation;
    let border_style = if is_selected {
        theme::CARD_BORDER_SELECTED
    } else {
        theme::CARD_BORDER
    };

    let Some((content_area, button_area)) = card::render_card_shell(frame, area, border_style)
    else {
        return false;
    };

    let (command_text, buttons, body_kind) = extract_card_content(choice);
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
        let focused = if is_selected
            && app.current_tab().recommendation_focus == crate::app::RecommendationFocus::Button
        {
            Some(app.current_tab().selected_button)
        } else {
            None
        };
        card::render_buttons(frame, button_inner, &buttons, focused);
    }
    content_inner.width > 0 && content_inner.height > 0
}

/// A read-only viewport over the exact bound payload. Scrolling never changes
/// the payload, and approval does not depend on visiting any particular row.
pub(crate) fn render_review(frame: &mut Frame, app: &mut App, area: Rect) {
    let Some(view) = app.recommendation_full_view.as_ref() else {
        return;
    };
    let binding = view.binding.clone();
    frame.render_widget(Clear, area);
    let inner = area;
    let choice_count = app
        .current_tab()
        .turn
        .recommendations()
        .map_or(0, |recs| recs.choices.len());
    let multiple_choices = choice_count > 1;
    let hint_rows = u16::from(inner.height >= 3);
    let [heading, body, buttons, hint] = Layout::vertical([
        Constraint::Length(u16::from(multiple_choices && inner.height >= 4)),
        Constraint::Min(0),
        Constraint::Length(u16::from(inner.height >= 2)),
        Constraint::Length(hint_rows),
    ])
    .areas(inner);
    if heading.height > 0 {
        frame.render_widget(
            Paragraph::new(format!(
                "{}/{} · {}",
                app.current_tab().selected_recommendation + 1,
                choice_count,
                binding.choice.title
            ))
            .style(theme::CARD_DESCRIPTION),
            heading,
        );
    }
    if body.width < 2 || body.height == 0 || buttons.height == 0 {
        return;
    }
    let (_, mut labels, _) = extract_card_content(&binding.choice);
    let text = full_review_text(&binding.choice);
    // Match the existing choice-level Run/Insert behavior: Insert applies to
    // Send actions, while workspace actions retain their normal behavior.
    if binding
        .choice
        .actions
        .iter()
        .any(|action| matches!(action, RecommendedAction::Send { .. }))
    {
        labels = vec![
            t!("chat.tool_kind.run").into_owned(),
            t!("chat.tool_kind.insert").into_owned(),
        ];
    } else if binding.choice.actions.len() > 1 {
        labels = vec![t!("recommendations.button_execute").into_owned()];
    }
    let lines = full_command_lines(&text, body.width as usize);
    let Some(view) = app.recommendation_full_view.as_mut() else {
        return;
    };
    view.page_rows = body.height as usize;
    view.scroll_max = lines.len().saturating_sub(view.page_rows);
    view.scroll = view.scroll.min(view.scroll_max);
    frame.render_widget(
        Paragraph::new(
            lines
                .into_iter()
                .skip(view.scroll)
                .take(view.page_rows)
                .map(|line| Line::styled(line, theme::CARD_CODE))
                .collect::<Vec<_>>(),
        ),
        body,
    );
    let selected = app.current_tab().selected_button.min(labels.len() - 1);
    let labels_width = labels.iter().map(|label| label.width()).sum::<usize>()
        + 3 * labels.len().saturating_sub(1);
    if labels_width <= buttons.width as usize {
        card::render_buttons(frame, buttons, &labels, Some(selected));
    } else {
        // Keep the focused action visible even when the labels cannot fit together.
        let label = &labels[selected];
        let focused_label = if label.width() + 4 <= buttons.width as usize {
            format!("← {label} →")
        } else {
            label.clone()
        };
        card::render_buttons(frame, buttons, &[focused_label], Some(0));
    }
    frame.render_widget(
        Paragraph::new(if multiple_choices {
            t!("recommendations.nav_hint").into_owned()
        } else {
            t!("recommendations.full_view_hint").into_owned()
        })
        .style(theme::DIM),
        hint,
    );
    app.recommendation_rendered = true;
    if labels[selected].width() <= buttons.width as usize {
        app.recommendation_approval = Some(binding);
    }
}

fn full_review_text(choice: &RecommendationChoice) -> String {
    if let [action] = choice.actions.as_slice() {
        return match action {
            RecommendedAction::Send { input, .. }
            | RecommendedAction::OpenAndSend {
                input, agent: None, ..
            } => input.clone(),
            _ => recommendation_action_display_text(action),
        };
    }
    choice
        .actions
        .iter()
        .enumerate()
        .map(|(index, action)| {
            let (labels, _) = action_buttons(Some(action));
            format!(
                "{}. {}\n{}",
                index + 1,
                labels[0],
                recommendation_action_display_text(action)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn full_command_lines(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    // Cell wrapping, rather than word wrapping, preserves whitespace and long
    // unbroken shell arguments at every scroll position.
    for logical_line in text.split('\n') {
        let mut line = String::new();
        let mut used = 0;
        let mut consumed = 0;
        let span = Span::raw(logical_line);
        for grapheme in span.styled_graphemes(Style::default()) {
            let cells = grapheme.symbol.width();
            if used + cells > width && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            // Ratatui filters nonprinting controls during segmentation. Keep
            // their original bytes rather than rewriting the bound text.
            let end = grapheme.symbol.as_ptr().addr() - logical_line.as_ptr().addr()
                + grapheme.symbol.len();
            line.push_str(&logical_line[consumed..end]);
            consumed = end;
            used += cells;
        }
        line.push_str(&logical_line[consumed..]);
        lines.push(line);
    }
    lines
}

enum CardBodyKind {
    Code,
    Description,
}

fn extract_card_content(choice: &RecommendationChoice) -> (String, Vec<String>, CardBodyKind) {
    let display = recommendation_display_text(choice);
    let (buttons, body_kind) = action_buttons(choice.actions.first());
    (display, buttons, body_kind)
}

fn action_buttons(action: Option<&RecommendedAction>) -> (Vec<String>, CardBodyKind) {
    match action {
        Some(RecommendedAction::Send { .. }) => (
            vec![
                t!("recommendations.button_run_command").into_owned(),
                t!("recommendations.button_insert_in_terminal").into_owned(),
            ],
            CardBodyKind::Code,
        ),
        Some(RecommendedAction::OpenAndSend { target, .. }) => {
            let target_label = match target {
                OpenTarget::Tab => t!("recommendations.button_open_in_new_tab").into_owned(),
                OpenTarget::Panel => t!("recommendations.button_open_in_new_panel").into_owned(),
            };
            (vec![target_label], CardBodyKind::Code)
        }
        Some(RecommendedAction::Open { target, .. }) => {
            let button = match target {
                OpenTarget::Tab => t!("recommendations.button_open_tab").into_owned(),
                OpenTarget::Panel => t!("recommendations.button_open_panel").into_owned(),
            };
            (vec![button], CardBodyKind::Description)
        }
        None => (
            vec![t!("recommendations.button_execute").into_owned()],
            CardBodyKind::Description,
        ),
    }
}

pub(super) fn recommendation_display_text(choice: &RecommendationChoice) -> String {
    choice
        .actions
        .first()
        .map(recommendation_action_display_text)
        .unwrap_or_else(|| choice.title.clone())
}

fn recommendation_action_display_text(action: &RecommendedAction) -> String {
    match action {
        RecommendedAction::Send { input, .. } => input.clone(),
        RecommendedAction::OpenAndSend { input, agent, .. } => {
            let fallback = t!("recommendations.agent_fallback").into_owned();
            let agent_label = agent.as_deref().unwrap_or(&fallback);
            t!(
                "recommendations.open_and_send_display",
                agent = agent_label,
                input = input.as_str()
            )
            .into_owned()
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
                    Some(direction) if !direction.is_empty() => t!(
                        "recommendations.open_kind_panel_direction",
                        direction = direction
                    )
                    .into_owned(),
                    _ => t!("recommendations.open_kind_panel").into_owned(),
                },
            };
            match (title.as_deref(), cwd.as_deref()) {
                (Some(title), Some(cwd)) if !title.is_empty() && !cwd.is_empty() => t!(
                    "recommendations.open_new_with_title_and_cwd",
                    kind = kind.as_str(),
                    title = title,
                    cwd = cwd
                )
                .into_owned(),
                (Some(title), _) if !title.is_empty() => t!(
                    "recommendations.open_new_with_title",
                    kind = kind.as_str(),
                    title = title
                )
                .into_owned(),
                (_, Some(cwd)) if !cwd.is_empty() => t!(
                    "recommendations.open_new_with_cwd",
                    kind = kind.as_str(),
                    cwd = cwd
                )
                .into_owned(),
                _ => t!("recommendations.open_new_empty", kind = kind.as_str()).into_owned(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_command_wrap_preserves_spaces_long_tokens_and_unicode() {
        let command = format!(
            "  echo '{}你好 e\u{301}👨‍👩‍👧‍👦\u{85}\u{2028}\u{2029}'  ; tail",
            "x".repeat(300)
        );
        for width in [2, 3, 10, 79] {
            let lines = full_command_lines(&command, width);
            assert_eq!(lines.concat(), command);
            assert!(lines.iter().all(|line| Span::raw(line.as_str())
                .styled_graphemes(Style::default())
                .map(|grapheme| grapheme.symbol.width())
                .sum::<usize>()
                <= width));
        }
    }

    #[test]
    fn compact_summary_paragraph_right_aligns_for_rtl() {
        let area = Rect::new(0, 0, 8, 1);
        let mut buffer = Buffer::empty(area);

        compact_summary_paragraph(
            Line::from("RTL"),
            crate::rtl::text_alignment_for_locale("qps-plocm"),
        )
        .render(area, &mut buffer);

        assert_eq!(buffer[(4, 0)].symbol(), " ");
        assert_eq!(buffer[(5, 0)].symbol(), "R");
        assert_eq!(buffer[(6, 0)].symbol(), "T");
        assert_eq!(buffer[(7, 0)].symbol(), "L");
    }
}
