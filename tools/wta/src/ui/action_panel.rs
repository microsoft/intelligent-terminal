use crate::app::PermissionState;
use crate::coordinator::{RecommendationChoice, RecommendationSet};
use unicode_width::UnicodeWidthStr;

use super::card::{card_content_width, CARD_MIN_SIZE};
use super::recommendations::recommendation_display_text;

pub(crate) const COMPACT_RECOMMENDATION_HEIGHT: u16 = 2;
const COMPACT_PERMISSION_HEIGHT: u16 = 1;
const ACTIVITY_HEIGHT: u16 = 1;
const CHAT_MIN_HEIGHT: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanelMode {
    Hidden,
    Compact,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActionPanelLayout {
    pub chat_height: u16,
    pub recommendation_height: u16,
    pub recommendation_mode: PanelMode,
    pub permission_height: u16,
    pub permission_mode: PanelMode,
    pub hint_height: u16,
    pub recommendation_hint_height: u16,
    pub activity_height: u16,
    pub input_height: u16,
}

pub(crate) struct LayoutRequest {
    pub available_rows: u16,
    pub input_height: u16,
    pub chat_natural_height: u16,
    pub hint_requested: bool,
    pub activity_requested: bool,
    pub recommendation_natural_height: Option<u16>,
    pub permission_natural_height: Option<u16>,
}

/// Allocate vertical rows for the chat view.
///
/// Input and the active action form the hard interactive base. A pending
/// permission is modal and suppresses recommendations. Recommendations use a
/// two-row summary until a five-row card shell fits. Activity and navigation
/// hints share one status row, with activity taking precedence. Chat and the
/// status row yield first if the host reports fewer than seven rows.
pub(crate) fn plan(request: LayoutRequest) -> ActionPanelLayout {
    let mut result = ActionPanelLayout {
        chat_height: 0,
        recommendation_height: 0,
        recommendation_mode: PanelMode::Hidden,
        permission_height: 0,
        permission_mode: PanelMode::Hidden,
        hint_height: 0,
        recommendation_hint_height: 0,
        activity_height: 0,
        input_height: super::input::INPUT_MIN_HEIGHT,
    };

    let preferred_base = super::input::INPUT_MIN_HEIGHT
        .saturating_add(ACTIVITY_HEIGHT)
        .saturating_add(CHAT_MIN_HEIGHT);
    let preferred_action_budget = request.available_rows.saturating_sub(preferred_base);
    let emergency_action_budget = request
        .available_rows
        .saturating_sub(super::input::INPUT_MIN_HEIGHT);

    if let Some(natural_height) = request.permission_natural_height {
        if preferred_action_budget >= natural_height {
            result.permission_height = natural_height;
            result.permission_mode = PanelMode::Full;
        } else if emergency_action_budget >= COMPACT_PERMISSION_HEIGHT {
            result.permission_height = COMPACT_PERMISSION_HEIGHT;
            result.permission_mode = PanelMode::Compact;
        }
    } else if let Some(natural_height) = request.recommendation_natural_height {
        if preferred_action_budget >= CARD_MIN_SIZE {
            result.recommendation_height =
                natural_height.min(preferred_action_budget).max(CARD_MIN_SIZE);
            result.recommendation_mode = PanelMode::Full;
        } else if emergency_action_budget >= COMPACT_RECOMMENDATION_HEIGHT {
            result.recommendation_height = COMPACT_RECOMMENDATION_HEIGHT;
            result.recommendation_mode = PanelMode::Compact;
        }
    }

    let action_rows = result
        .permission_height
        .saturating_add(result.recommendation_height);
    let natural_content_without_hint = request
        .input_height
        .saturating_add(action_rows)
        .saturating_add(request.chat_natural_height.max(CHAT_MIN_HEIGHT));
    let compact = result.permission_mode == PanelMode::Compact
        || result.recommendation_mode == PanelMode::Compact;
    let generic_hint_requested = request.hint_requested
        && !compact
        && request.available_rows > natural_content_without_hint;

    let base_remaining = request
        .available_rows
        .saturating_sub(action_rows)
        .saturating_sub(super::input::INPUT_MIN_HEIGHT);
    result.chat_height = CHAT_MIN_HEIGHT.min(base_remaining);
    let status_height =
        ACTIVITY_HEIGHT.min(base_remaining.saturating_sub(result.chat_height));
    if request.activity_requested {
        result.activity_height = status_height;
    } else if result.recommendation_mode == PanelMode::Full {
        result.recommendation_hint_height = status_height;
    } else if generic_hint_requested {
        result.hint_height = status_height;
    }
    let allocated_status_height = result
        .activity_height
        .saturating_add(result.recommendation_hint_height)
        .saturating_add(result.hint_height);

    let input_capacity = request
        .available_rows
        .saturating_sub(allocated_status_height)
        .saturating_sub(action_rows)
        .saturating_sub(result.chat_height);
    result.input_height = request.input_height.min(input_capacity);

    let chat_capacity = request
        .available_rows
        .saturating_sub(result.input_height)
        .saturating_sub(allocated_status_height)
        .saturating_sub(action_rows);
    result.chat_height = request.chat_natural_height.min(chat_capacity);
    result
}

pub(crate) fn recommendation_panel_height(
    recommendations: &RecommendationSet,
    panel_width: u16,
) -> u16 {
    recommendations
        .choices
        .iter()
        .map(|choice| recommendation_card_height(choice, panel_width))
        .sum::<usize>()
        .min(u16::MAX as usize) as u16
}

/// Rendered recommendation-card height, including one inter-card gap row.
pub(crate) fn recommendation_card_height(choice: &RecommendationChoice, panel_width: u16) -> usize {
    let inner_width = card_content_width(panel_width);
    let text = recommendation_display_text(choice);

    let content_lines = wrapped_line_count(&text, inner_width);
    CARD_MIN_SIZE as usize + content_lines.saturating_sub(1) + 1
}

/// Natural height of the blocking permission card.
pub(crate) fn permission_card_height(permission: &PermissionState, panel_width: u16) -> usize {
    let inner_width = card_content_width(panel_width);
    let header = match &permission.kind_label {
        Some(icon) => format!("{icon} {}", permission.title),
        None => permission.title.clone(),
    };
    let mut content_lines = wrapped_line_count(&header, inner_width);
    if let Some(target) = &permission.target {
        if permission.target_is_command {
            content_lines += super::command_format::command_display_lines(target)
                .iter()
                .map(|line| wrapped_line_count(&line.rendered_text(""), inner_width))
                .sum::<usize>();
        } else {
            content_lines += wrapped_line_count(target, inner_width);
        }
    }
    CARD_MIN_SIZE as usize + content_lines.saturating_sub(1)
}

fn wrapped_line_count(text: &str, width: usize) -> usize {
    let width = width.max(1);
    text.lines()
        .map(|line| {
            let display_width = UnicodeWidthStr::width(line);
            if display_width == 0 {
                1
            } else {
                display_width.div_ceil(width)
            }
        })
        .sum::<usize>()
        .max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recommendation_request(rows: u16) -> LayoutRequest {
        LayoutRequest {
            available_rows: rows,
            input_height: 3,
            chat_natural_height: 4,
            hint_requested: true,
            activity_requested: false,
            recommendation_natural_height: Some(6),
            permission_natural_height: None,
        }
    }

    fn allocated_height(layout: ActionPanelLayout) -> u32 {
        [
            layout.chat_height,
            layout.recommendation_height,
            layout.permission_height,
            layout.hint_height,
            layout.recommendation_hint_height,
            layout.activity_height,
            layout.input_height,
        ]
        .into_iter()
        .map(u32::from)
        .sum()
    }

    #[test]
    fn input_shrinks_to_fit_hosts_below_its_minimum_height() {
        for rows in 0..super::super::input::INPUT_MIN_HEIGHT {
            let layout = plan(recommendation_request(rows));

            assert_eq!(layout.input_height, rows);
            assert_eq!(allocated_height(layout), u32::from(rows));
        }
    }

    #[test]
    fn planned_heights_never_exceed_available_rows() {
        for rows in 0..=32 {
            let requests = [
                recommendation_request(rows),
                LayoutRequest {
                    available_rows: rows,
                    input_height: 8,
                    chat_natural_height: 20,
                    hint_requested: true,
                    activity_requested: true,
                    recommendation_natural_height: None,
                    permission_natural_height: Some(12),
                },
                LayoutRequest {
                    available_rows: rows,
                    input_height: 8,
                    chat_natural_height: 20,
                    hint_requested: true,
                    activity_requested: true,
                    recommendation_natural_height: None,
                    permission_natural_height: None,
                },
            ];

            for request in requests {
                let available_rows = request.available_rows;
                let layout = plan(request);
                assert!(allocated_height(layout) <= u32::from(available_rows));
            }
        }

        let five = plan(recommendation_request(5));
        assert_eq!(five.recommendation_mode, PanelMode::Compact);
        assert_eq!(five.recommendation_height, COMPACT_RECOMMENDATION_HEIGHT);
        assert_eq!(five.input_height, super::super::input::INPUT_MIN_HEIGHT);
    }

    #[test]
    fn recommendations_degrade_and_restore_between_seven_and_thirteen_rows() {
        for rows in 7..=9 {
            let layout = plan(recommendation_request(rows));
            assert_eq!(layout.recommendation_mode, PanelMode::Compact);
            assert_eq!(layout.recommendation_height, 2);
            assert_eq!(layout.recommendation_hint_height, 0);
            assert_eq!(layout.hint_height, 0);
        }

        let ten = plan(recommendation_request(10));
        assert_eq!(ten.recommendation_mode, PanelMode::Full);
        assert_eq!(ten.recommendation_height, 5);
        assert_eq!(ten.recommendation_hint_height, 1);

        let eleven = plan(recommendation_request(11));
        assert_eq!(eleven.recommendation_height, 6);
        assert_eq!(eleven.recommendation_hint_height, 1);

        let twelve = plan(recommendation_request(12));
        assert_eq!(twelve.recommendation_height, 6);
        assert_eq!(twelve.recommendation_hint_height, 1);
        assert_eq!(twelve.chat_height, 2);

        let thirteen = plan(recommendation_request(13));
        assert_eq!(thirteen.recommendation_height, 6);
        assert_eq!(thirteen.recommendation_hint_height, 1);
        assert_eq!(thirteen.chat_height, 3);
    }

    #[test]
    fn activity_replaces_navigation_and_generic_hints() {
        let mut request = recommendation_request(13);
        request.activity_requested = true;

        let layout = plan(request);

        assert_eq!(layout.activity_height, 1);
        assert_eq!(layout.recommendation_hint_height, 0);
        assert_eq!(layout.hint_height, 0);
    }

    #[test]
    fn permission_is_modal_and_uses_compact_until_full_card_fits() {
        let request = |rows| LayoutRequest {
            available_rows: rows,
            input_height: 3,
            chat_natural_height: 4,
            hint_requested: true,
            activity_requested: false,
            recommendation_natural_height: Some(6),
            permission_natural_height: Some(5),
        };

        let short = plan(request(9));
        assert_eq!(short.permission_mode, PanelMode::Compact);
        assert_eq!(short.permission_height, 1);
        assert_eq!(short.recommendation_mode, PanelMode::Hidden);

        let full = plan(request(10));
        assert_eq!(full.permission_mode, PanelMode::Full);
        assert_eq!(full.permission_height, 5);
        assert_eq!(full.recommendation_mode, PanelMode::Hidden);
    }

    #[test]
    fn expanded_input_cannot_displace_a_compact_action() {
        let layout = plan(LayoutRequest {
            available_rows: 7,
            input_height: 8,
            chat_natural_height: 4,
            hint_requested: false,
            activity_requested: false,
            recommendation_natural_height: Some(6),
            permission_natural_height: None,
        });

        assert_eq!(layout.recommendation_mode, PanelMode::Compact);
        assert_eq!(layout.recommendation_height, 2);
        assert_eq!(layout.input_height, 4);
        assert_eq!(layout.chat_height, 1);
    }

    #[test]
    fn compact_recommendation_survives_below_the_host_floor() {
        let layout = plan(recommendation_request(6));

        assert_eq!(layout.recommendation_mode, PanelMode::Compact);
        assert_eq!(layout.recommendation_height, 2);
        assert_eq!(layout.input_height, 3);
        assert_eq!(layout.chat_height, 1);
        assert_eq!(layout.activity_height, 0);
    }

    #[test]
    fn wrapped_line_count_uses_display_width_for_cjk() {
        assert_eq!(wrapped_line_count("你好", 3), 2);
    }

    #[test]
    fn wrapped_line_count_does_not_charge_combining_marks() {
        assert_eq!(wrapped_line_count("e\u{301}e\u{301}", 1), 2);
    }

    #[test]
    fn wrapped_line_count_handles_zero_width() {
        assert_eq!(wrapped_line_count("abc", 0), 3);
    }
}
