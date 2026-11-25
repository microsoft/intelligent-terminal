use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::coordinator::{OpenTarget, RecommendationChoice, RecommendedAction};
use crate::theme;
use crate::ui::action_panel::{recommendation_card_height, PanelMode};
use crate::ui::card::{self, CARD_MIN_SIZE};

/// Render the recommendations panel. Callers (layout.rs) must call
/// `App::sync_rec_scroll_max` first so `rec_scroll.offset` is already clamped
/// when we paint.
///
/// Cards are positioned in a virtual canvas (stacked top-to-bottom by their
/// natural heights), then shifted up by `rec_scroll`. The navigation hint is
/// rendered separately by `render_hint` so it can sit directly above the
/// input box (see `layout.rs`).
///
/// The selected card scrolls its body independently of its shell/buttons.
/// Returns the selected
/// actionable card's painted bounds, not unrelated cards.
pub(crate) fn render(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    mode: PanelMode,
) -> Option<Rect> {
    let Some(recs) = app.current_tab().turn.recommendations().cloned() else {
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
            let card_h = h.saturating_sub(1).min(u16::MAX as usize) as u16; // last canvas row is inter-card gap
            let y =
                area.y.saturating_add(
                    canvas_top.saturating_sub(rec_scroll).min(u16::MAX as usize) as u16
                );
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
            if painted && idx == app.current_tab().selected_recommendation {
                rendered = Some(rendered.map_or(card_area, |bounds| bounds.union(card_area)));
            }
        }
        canvas_top += h;
    }
    rendered
}

fn render_compact(frame: &mut Frame, app: &mut App, area: Rect) -> bool {
    let Some(recommendations) = app.current_tab().turn.recommendations().cloned() else {
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
    let lines = full_command_lines(&summary, summary_width.max(1));
    let body_fits_width = command_lines_fit_width(&lines, summary_width);
    app.current_tab_mut()
        .rec_body_scroll
        .set_max(lines.len().saturating_sub(1));
    let offset = app.current_tab().rec_body_scroll.offset;
    let summary = lines[offset].clone();
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
        return summary_width >= 2
            && render_action_buttons(frame, button_area, &buttons, focused)
            && body_fits_width;
    }
    false
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
    app: &mut App,
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
    let mut body_fits_width = false;
    if content_inner.width > 0 {
        let lines = full_command_lines(&command_text, content_inner.width as usize);
        body_fits_width = command_lines_fit_width(&lines, content_inner.width as usize);
        let max = lines.len().saturating_sub(content_inner.height as usize);
        let body_offset = if is_selected {
            app.current_tab_mut().rec_body_scroll.set_max(max);
            app.current_tab().rec_body_scroll.offset
        } else {
            0
        };
        let content = Paragraph::new(
            lines
                .into_iter()
                .skip(body_offset.min(max))
                .take(content_inner.height as usize)
                .map(Line::from)
                .collect::<Vec<_>>(),
        )
        .style(body_style);
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
        return content_inner.width >= 2
            && content_inner.height > 0
            && render_action_buttons(frame, button_inner, &buttons, focused)
            && body_fits_width;
    }
    false
}

/// Paint the selected action completely or do not make this card actionable.
fn render_action_buttons(
    frame: &mut Frame,
    buttons: Rect,
    labels: &[String],
    focused: Option<usize>,
) -> bool {
    let selected = focused.unwrap_or(0);
    let Some(_) = labels.get(selected) else {
        return false;
    };
    let labels_width = labels
        .iter()
        .map(|label| painted_text_width(label))
        .sum::<usize>()
        + 3 * labels.len().saturating_sub(1);
    if labels_width <= buttons.width as usize {
        card::render_buttons(frame, buttons, labels, focused);
        return true;
    }
    // Keep ordinary wide cards unchanged; shorten only cramped Send actions.
    let short = if labels.len() == 2 {
        vec![
            t!("chat.tool_kind.run").into_owned(),
            t!("chat.tool_kind.insert").into_owned(),
        ]
    } else {
        labels.to_vec()
    };
    let label = &short[selected];
    let short_width = short
        .iter()
        .map(|label| painted_text_width(label))
        .sum::<usize>()
        + 3 * short.len().saturating_sub(1);
    if short_width <= buttons.width as usize {
        card::render_buttons(frame, buttons, &short, focused);
    } else {
        card::render_buttons(
            frame,
            buttons,
            std::slice::from_ref(label),
            focused.map(|_| 0),
        );
    }
    painted_text_width(label) <= buttons.width as usize
}

pub(super) fn recommendation_display_text(choice: &RecommendationChoice) -> String {
    if let [action] = choice.actions.as_slice() {
        return recommendation_action_display_text(action);
    }
    if choice.actions.is_empty() {
        return choice.title.clone();
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

fn command_lines_fit_width(lines: &[String], width: usize) -> bool {
    lines.iter().all(|line| painted_text_width(line) <= width)
}

fn painted_text_width(text: &str) -> usize {
    Span::raw(text)
        .styled_graphemes(Style::default())
        .map(|grapheme| grapheme.symbol.width())
        .sum()
}

pub(super) fn full_command_lines(text: &str, width: usize) -> Vec<String> {
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
    let (buttons, body_kind) = if choice.supports_insert() {
        (
            action_buttons(
                choice
                    .actions
                    .iter()
                    .find(|action| matches!(action, RecommendedAction::Send { .. })),
            )
            .0,
            CardBodyKind::Code,
        )
    } else if choice.actions.len() > 1 {
        (
            vec![t!("recommendations.button_execute").into_owned()],
            CardBodyKind::Code,
        )
    } else {
        action_buttons(choice.actions.first())
    };
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
