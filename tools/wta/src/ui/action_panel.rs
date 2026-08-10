use crate::app::PermissionState;
use crate::coordinator::{OpenTarget, RecommendationChoice, RecommendationSet, RecommendedAction};

use super::card::{card_content_width, CARD_MIN_SIZE};

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
    pub recommendation_natural_height: Option<u16>,
    pub permission_natural_height: Option<u16>,
}

/// Allocate vertical rows for the chat view.
///
/// Input and the active action form the hard interactive base. A pending
/// permission is modal and suppresses recommendations. Recommendations use a
/// two-row summary until a five-row card shell fits. Activity, chat, and
/// optional hints yield first if the host reports fewer than seven rows.
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
            let reserve_hint = u16::from(preferred_action_budget > CARD_MIN_SIZE);
            let panel_budget = preferred_action_budget.saturating_sub(reserve_hint);
            result.recommendation_height = natural_height.min(panel_budget).max(CARD_MIN_SIZE);
            result.recommendation_mode = PanelMode::Full;
            result.recommendation_hint_height = reserve_hint;
        } else if emergency_action_budget >= COMPACT_RECOMMENDATION_HEIGHT {
            result.recommendation_height = COMPACT_RECOMMENDATION_HEIGHT;
            result.recommendation_mode = PanelMode::Compact;
        }
    }

    let action_rows = result
        .permission_height
        .saturating_add(result.recommendation_height)
        .saturating_add(result.recommendation_hint_height);
    let natural_content_without_hint = request
        .input_height
        .saturating_add(ACTIVITY_HEIGHT)
        .saturating_add(action_rows)
        .saturating_add(request.chat_natural_height.max(CHAT_MIN_HEIGHT));
    let compact = result.permission_mode == PanelMode::Compact
        || result.recommendation_mode == PanelMode::Compact;
    if request.hint_requested && !compact && request.available_rows > natural_content_without_hint {
        result.hint_height = 1;
    }

    let base_remaining = request
        .available_rows
        .saturating_sub(action_rows)
        .saturating_sub(result.hint_height)
        .saturating_sub(super::input::INPUT_MIN_HEIGHT);
    result.chat_height = CHAT_MIN_HEIGHT.min(base_remaining);
    result.activity_height = ACTIVITY_HEIGHT.min(base_remaining.saturating_sub(result.chat_height));

    let input_capacity = request
        .available_rows
        .saturating_sub(result.activity_height)
        .saturating_sub(action_rows)
        .saturating_sub(result.hint_height)
        .saturating_sub(result.chat_height)
        .max(super::input::INPUT_MIN_HEIGHT);
    result.input_height = request.input_height.min(input_capacity);

    let chat_capacity = request
        .available_rows
        .saturating_sub(result.input_height)
        .saturating_sub(result.activity_height)
        .saturating_sub(action_rows)
        .saturating_sub(result.hint_height);
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
    let text = choice
        .actions
        .iter()
        .find_map(|action| match action {
            RecommendedAction::Send { input, .. } => Some(input.clone()),
            RecommendedAction::OpenAndSend { agent, input, .. } => {
                let label = agent.as_deref().unwrap_or("agent");
                Some(format!("{label}: {input}"))
            }
            RecommendedAction::Open {
                target, cwd, title, ..
            } => {
                let kind = match target {
                    OpenTarget::Tab => "tab",
                    OpenTarget::Panel => "panel",
                };
                Some(match (title.as_deref(), cwd.as_deref()) {
                    (Some(title), Some(cwd)) if !title.is_empty() && !cwd.is_empty() => {
                        format!("New {kind} ({title}) in {cwd}")
                    }
                    (Some(title), _) if !title.is_empty() => format!("New {kind} ({title})"),
                    (_, Some(cwd)) if !cwd.is_empty() => format!("New {kind} in {cwd}"),
                    _ => format!("New {kind} (empty)"),
                })
            }
        })
        .unwrap_or_else(|| choice.title.clone());

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
            content_lines += super::command_format::command_display_lines(target).len();
        } else {
            content_lines += wrapped_line_count(target, inner_width);
        }
    }
    CARD_MIN_SIZE as usize + content_lines.saturating_sub(1)
}

fn wrapped_line_count(text: &str, width: usize) -> usize {
    text.lines()
        .map(|line| {
            let chars = line.chars().count();
            if chars == 0 {
                1
            } else {
                chars.div_ceil(width)
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
            recommendation_natural_height: Some(6),
            permission_natural_height: None,
        }
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
        assert_eq!(ten.recommendation_hint_height, 0);

        let eleven = plan(recommendation_request(11));
        assert_eq!(eleven.recommendation_height, 5);
        assert_eq!(eleven.recommendation_hint_height, 1);

        let twelve = plan(recommendation_request(12));
        assert_eq!(twelve.recommendation_height, 6);
        assert_eq!(twelve.recommendation_hint_height, 1);
        assert_eq!(twelve.chat_height, 1);

        let thirteen = plan(recommendation_request(13));
        assert_eq!(thirteen.recommendation_height, 6);
        assert_eq!(thirteen.recommendation_hint_height, 1);
        assert_eq!(thirteen.chat_height, 2);
    }

    #[test]
    fn permission_is_modal_and_uses_compact_until_full_card_fits() {
        let request = |rows| LayoutRequest {
            available_rows: rows,
            input_height: 3,
            chat_natural_height: 4,
            hint_requested: true,
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
            recommendation_natural_height: Some(6),
            permission_natural_height: None,
        });

        assert_eq!(layout.recommendation_mode, PanelMode::Compact);
        assert_eq!(layout.recommendation_height, 2);
        assert_eq!(layout.input_height, 3);
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
}
