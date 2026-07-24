//! Wire schema for the direct WTA CLI terminal-action proposal flow
//! (`wta propose-terminal-actions`). See
//! `doc/specs/WTA-CLI-terminal-action-proposals.md`.
//!
//! An agent session that can execute tools directly (rather than relying on
//! the ACP `create_terminal`/helper-proxy path) submits a proposal as one
//! compact JSON object matching [`ProposalWire`]. This module owns:
//!
//! * the strict (`deny_unknown_fields`) wire types — deliberately narrower
//!   than [`crate::coordinator::RecommendationSet`]: they never accept a
//!   session/helper/window/tab/pane id, and Open/OpenAndSend carry a
//!   `delegate: bool` flag instead of a free-text `agent` id, so a proposal
//!   can ask for "the user's configured delegate" but never name an
//!   arbitrary agent;
//! * origin-aware policy (`ProposalOrigin::TerminalAgent` vs `::Autofix`);
//! * size/count bounds enforced *before* `serde_json` ever sees the bytes;
//! * conversion into [`crate::coordinator::RecommendationSet`], which then
//!   flows through the exact same card-surfacing / execution code as the
//!   long-standing assistant-text JSON fallback.
//!
//! The proposal travels over the owning Helper's direct proposal pipe; Master
//! is not involved. The Helper invokes this module from App's direct proposal
//! validation path and is solely responsible for decoding and policy checks.

use serde::{Deserialize, Serialize};

use crate::coordinator::{
    validate_recommendation_set, OpenTarget, RecommendationChoice, RecommendationSet,
    RecommendedAction,
};

/// The only wire schema version this build understands. Bumped only on a
/// breaking change to [`ProposalWire`]; an older/newer CLI talking to this
/// helper gets [`ProposalError::UnsupportedSchemaVersion`].
pub const SCHEMA_VERSION: u32 = 1;

/// Hard cap on the raw JSON payload size, enforced by the CLI (before
/// sending) and again here (before `serde_json` parses it) — a proposal is
/// a handful of short strings, never a multi-megabyte blob. Keeps a
/// misbehaving/compromised agent from pushing an oversized payload through
/// the named pipe or holding the bounded pending-proposal map open with a
/// slow parse.
pub const MAX_PAYLOAD_BYTES: usize = 8 * 1024;

/// Max choices per proposal — matches the long-standing fallback-JSON
/// policy in [`crate::coordinator::validate_recommendation_set`] (1..=3).
pub const MAX_CHOICES: usize = 3;
/// Max actions per choice.
pub const MAX_ACTIONS_PER_CHOICE: usize = 3;
/// Character caps on free-text fields. Generous enough for a real
/// recommendation, small enough that a runaway proposal can't bloat chat
/// history or the pending-proposal map.
pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_RATIONALE_CHARS: usize = 2000;
pub const MAX_INPUT_CHARS: usize = 8000;

/// Disposition returned to the CLI (and, before that, decided by the
/// owning helper). All five are "protocol-complete" outcomes: the CLI
/// exits 0 and prints this as compact JSON for every one of them. A
/// non-zero CLI exit is reserved for transport/IO failures that never
/// reached this far (can't read stdin/payload file, can't reach master at
/// all).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    /// The recommendation card is now visible in the agent pane.
    Presented,
    /// A card was already showing for this turn (eager text-fallback
    /// surface, or an earlier proposal) — this proposal was not the one
    /// that ended up on screen.
    Duplicate,
    /// The route/turn was valid when minted but is no longer current by
    /// the time the proposal arrived (token expired/consumed already, or
    /// the turn moved on before the helper could act).
    Stale,
    /// The route was fresh and reached the owning helper, but the payload
    /// failed origin/schema/coordinator-target policy.
    Rejected,
    /// The owning helper/session could not be reached at all (disconnected,
    /// shut down, or the response timed out).
    Unavailable,
}

impl ProposalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ProposalStatus::Presented => "presented",
            ProposalStatus::Duplicate => "duplicate",
            ProposalStatus::Stale => "stale",
            ProposalStatus::Rejected => "rejected",
            ProposalStatus::Unavailable => "unavailable",
        }
    }
}

/// Why a proposal failed before ever reaching the "did the helper accept
/// it" decision. Distinct from [`ProposalStatus`]: this is the *local*
/// (CLI or master, pre-relay) or *decode* failure classification;
/// `to_status` collapses it onto the wire disposition so callers don't
/// need two vocabularies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposalError {
    /// Raw payload exceeded [`MAX_PAYLOAD_BYTES`] — rejected before parsing.
    TooLarge { size: usize },
    /// `serde_json` (or the strict wire schema's `deny_unknown_fields`)
    /// rejected the payload outright.
    Malformed(String),
    /// `schema_version` in the payload doesn't match [`SCHEMA_VERSION`].
    UnsupportedSchemaVersion(u32),
    /// Decoded fine, but violates origin/shape/count/length policy (wrong
    /// action for the declared origin, too many choices, empty title,
    /// oversized field, etc.) or the coordinator-target filter rejected
    /// every choice.
    PolicyViolation(String),
}

impl ProposalError {
    /// Collapse onto the wire disposition. Every variant here maps to
    /// `Rejected` except the size cap, which is its own thing conceptually
    /// but still a policy rejection from the caller's point of view — no
    /// separate status exists for it in the five-way table the spec
    /// defines, so it also reports `Rejected` with a specific reason.
    pub fn to_status(&self) -> ProposalStatus {
        ProposalStatus::Rejected
    }

    pub fn reason(&self) -> String {
        match self {
            ProposalError::TooLarge { size } => {
                format!("payload too large ({size} bytes, max {MAX_PAYLOAD_BYTES})")
            }
            ProposalError::Malformed(msg) => format!("malformed payload: {msg}"),
            ProposalError::UnsupportedSchemaVersion(v) => {
                format!("unsupported schema_version {v} (expected {SCHEMA_VERSION})")
            }
            ProposalError::PolicyViolation(msg) => msg.clone(),
        }
    }
}

/// Which system prompt asked for this proposal. Validated against the
/// owning helper's OWN authoritative `TurnState::is_autofix()` — a
/// mismatch (e.g. `autofix` origin claimed on a plain chat turn) is a
/// policy violation, never trusted from the payload alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalOrigin {
    TerminalAgent,
    Autofix,
}

/// Top-level proposal payload. `deny_unknown_fields` so a future field a
/// model hallucinates (or an attempt to sneak in e.g. `session_id`) is a
/// hard parse failure, not silently ignored.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalWire {
    pub schema_version: u32,
    pub origin: ProposalOrigin,
    #[serde(default)]
    pub recommended_choice: Option<usize>,
    pub choices: Vec<ProposalChoiceWire>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalChoiceWire {
    pub choice: usize,
    pub title: String,
    #[serde(default)]
    pub rationale: String,
    pub actions: Vec<ProposalActionWire>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalOpenTargetWire {
    Tab,
    Panel,
}

impl From<ProposalOpenTargetWire> for OpenTarget {
    fn from(value: ProposalOpenTargetWire) -> Self {
        match value {
            ProposalOpenTargetWire::Tab => OpenTarget::Tab,
            ProposalOpenTargetWire::Panel => OpenTarget::Panel,
        }
    }
}

/// Action wire shape. Deliberately has no session/helper/window/tab/pane id
/// field. The helper captures the active working pane for the prompt and
/// supplies it separately as trusted metadata; model-authored JSON cannot
/// redirect a send or panel action to another pane. Autofix continues to bind
/// its failing pane at card-execution time.
///
/// `agent: Option<String>` from [`RecommendedAction`] is intentionally
/// *not* exposed on `OpenAndSend` here — `delegate: bool` replaces it so a
/// proposal can ask for "the user's configured delegate" but can never
/// name an arbitrary agent id. `Open` never carries an agent selector at
/// all (mirrors [`RecommendedAction::Open`], which has no `agent` field —
/// a bare `Open` just opens a plain shell target, no agent involved).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProposalActionWire {
    Send {
        input: String,
    },
    Open {
        target: ProposalOpenTargetWire,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        direction: Option<String>,
        #[serde(default)]
        profile: Option<String>,
    },
    OpenAndSend {
        target: ProposalOpenTargetWire,
        input: String,
        #[serde(default)]
        delegate: bool,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        direction: Option<String>,
        #[serde(default)]
        profile: Option<String>,
    },
}

/// Decode raw bytes into a [`ProposalWire`], enforcing the size cap before
/// `serde_json` ever touches the buffer. Used by both the CLI (a cheap
/// local pre-check so an oversized payload never reaches the pipe) and the
/// owning helper (the authoritative decode).
pub fn parse_proposal_payload(bytes: &[u8]) -> Result<ProposalWire, ProposalError> {
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(ProposalError::TooLarge { size: bytes.len() });
    }
    let wire: ProposalWire =
        serde_json::from_slice(bytes).map_err(|e| ProposalError::Malformed(e.to_string()))?;
    if wire.schema_version != SCHEMA_VERSION {
        return Err(ProposalError::UnsupportedSchemaVersion(wire.schema_version));
    }
    Ok(wire)
}

/// Convert a decoded [`ProposalWire`] into a [`RecommendationSet`], applying
/// origin policy and the shared count/length/coordinator-target validation
/// that the assistant-text fallback path already enforces.
///
/// * `is_autofix_turn` — the owning turn's OWN `TurnState::is_autofix()`
///   (never taken from the payload). A mismatch against `wire.origin` is a
///   [`ProposalError::PolicyViolation`].
/// * `configured_delegate_id` — the helper's currently configured delegate
///   agent id (`App.delegate_agents`), substituted for `delegate: true`
///   actions. `None` means no delegate is configured — an action with
///   `delegate: true` is then a policy violation rather than silently
///   falling back to "no agent" (which would defeat the point of asking
///   for the delegate).
/// * `coordinator_target` — this pane's own id, filtered out of `Send`
///   targets exactly as [`crate::coordinator::validate_recommendation_set_for_coordinator_target`]
///   already does for the text-fallback path.
pub fn build_recommendation_set(
    wire: &ProposalWire,
    is_autofix_turn: bool,
    configured_delegate_id: Option<&str>,
    trusted_active_target: Option<&str>,
    coordinator_target: Option<&str>,
) -> Result<RecommendationSet, ProposalError> {
    let origin_is_autofix = matches!(wire.origin, ProposalOrigin::Autofix);
    if origin_is_autofix != is_autofix_turn {
        return Err(ProposalError::PolicyViolation(format!(
            "origin {:?} does not match the current turn (is_autofix={})",
            wire.origin, is_autofix_turn
        )));
    }

    if wire.choices.is_empty() || wire.choices.len() > MAX_CHOICES {
        return Err(ProposalError::PolicyViolation(format!(
            "expected 1 to {MAX_CHOICES} choices, got {}",
            wire.choices.len()
        )));
    }

    if origin_is_autofix {
        // Autofix MVP policy: exactly one choice, exactly one Send action.
        // No Open/OpenAndSend — autofix never spawns a new pane. `parent`
        // is stripped/ignored unconditionally; the real failing pane is
        // bound by the caller (App::turn_execute_card's existing autofill),
        // exactly like today's manual `/fix` flow.
        if wire.choices.len() != 1 {
            return Err(ProposalError::PolicyViolation(format!(
                "autofix proposals must have exactly one choice, got {}",
                wire.choices.len()
            )));
        }
        let choice = &wire.choices[0];
        if choice.actions.len() != 1 {
            return Err(ProposalError::PolicyViolation(format!(
                "autofix proposals must have exactly one action, got {}",
                choice.actions.len()
            )));
        }
        let ProposalActionWire::Send { input, .. } = &choice.actions[0] else {
            return Err(ProposalError::PolicyViolation(
                "autofix proposals must use a single send action".to_string(),
            ));
        };
        check_len("title", &choice.title, MAX_TITLE_CHARS)?;
        check_len("rationale", &choice.rationale, MAX_RATIONALE_CHARS)?;
        check_len("input", input, MAX_INPUT_CHARS)?;
        let set = RecommendationSet {
            recommended_choice: Some(choice.choice),
            choices: vec![RecommendationChoice {
                choice: choice.choice,
                title: choice.title.clone(),
                rationale: choice.rationale.clone(),
                actions: vec![RecommendedAction::Send {
                    parent: String::new(),
                    input: input.clone(),
                }],
            }],
        };
        validate_recommendation_set(&set)
            .map_err(|e| ProposalError::PolicyViolation(e.to_string()))?;
        return Ok(set);
    }

    // Terminal Agent origin: "current policies" — same 1..=3 choices /
    // 1..=3 actions / Send+Open+OpenAndSend shape as the text-fallback
    // path, converted 1:1 apart from `delegate` resolution.
    let mut choices = Vec::with_capacity(wire.choices.len());
    for choice in &wire.choices {
        if choice.actions.is_empty() || choice.actions.len() > MAX_ACTIONS_PER_CHOICE {
            return Err(ProposalError::PolicyViolation(format!(
                "choice {} must have 1 to {MAX_ACTIONS_PER_CHOICE} actions, got {}",
                choice.choice,
                choice.actions.len()
            )));
        }
        check_len("title", &choice.title, MAX_TITLE_CHARS)?;
        check_len("rationale", &choice.rationale, MAX_RATIONALE_CHARS)?;
        let mut actions = Vec::with_capacity(choice.actions.len());
        for action in &choice.actions {
            actions.push(convert_terminal_agent_action(
                action,
                configured_delegate_id,
                trusted_active_target,
            )?);
        }
        choices.push(RecommendationChoice {
            choice: choice.choice,
            title: choice.title.clone(),
            rationale: choice.rationale.clone(),
            actions,
        });
    }
    let set = RecommendationSet {
        recommended_choice: wire.recommended_choice,
        choices,
    };
    validate_recommendation_set(&set).map_err(|e| ProposalError::PolicyViolation(e.to_string()))?;
    let set = crate::coordinator::validate_recommendation_set_for_coordinator_target(
        &set,
        coordinator_target,
    )
    .map_err(|e| ProposalError::PolicyViolation(e.to_string()))?;
    Ok(set)
}

fn convert_terminal_agent_action(
    action: &ProposalActionWire,
    configured_delegate_id: Option<&str>,
    trusted_active_target: Option<&str>,
) -> Result<RecommendedAction, ProposalError> {
    match action {
        ProposalActionWire::Send { input } => {
            check_len("input", input, MAX_INPUT_CHARS)?;
            Ok(RecommendedAction::Send {
                parent: require_active_target(trusted_active_target)?,
                input: input.clone(),
            })
        }
        ProposalActionWire::Open {
            target,
            cwd,
            title,
            direction,
            profile,
        } => Ok(RecommendedAction::Open {
            target: (*target).into(),
            parent: panel_parent(*target, trusted_active_target)?,
            cwd: cwd.clone(),
            title: title.clone(),
            direction: direction.clone(),
            profile: profile.clone(),
        }),
        ProposalActionWire::OpenAndSend {
            target,
            input,
            delegate,
            cwd,
            title,
            direction,
            profile,
        } => {
            check_len("input", input, MAX_INPUT_CHARS)?;
            Ok(RecommendedAction::OpenAndSend {
                target: (*target).into(),
                parent: panel_parent(*target, trusted_active_target)?,
                input: input.clone(),
                cwd: cwd.clone(),
                title: title.clone(),
                direction: direction.clone(),
                profile: profile.clone(),
                agent: resolve_delegate(*delegate, configured_delegate_id)?,
            })
        }
    }
}

fn require_active_target(active_target: Option<&str>) -> Result<String, ProposalError> {
    active_target
        .filter(|target| !target.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            ProposalError::PolicyViolation(
                "the prompt has no active pane for this action".to_string(),
            )
        })
}

fn panel_parent(
    target: ProposalOpenTargetWire,
    active_target: Option<&str>,
) -> Result<Option<String>, ProposalError> {
    match target {
        ProposalOpenTargetWire::Tab => Ok(None),
        ProposalOpenTargetWire::Panel => require_active_target(active_target).map(Some),
    }
}

/// `delegate: false` -> no agent override (the opened pane gets the
/// default agent). `delegate: true` -> the helper's own configured
/// delegate id — never a string taken from the payload. `delegate: true`
/// with no configured delegate is a policy violation: silently falling
/// back to "no agent" would make the flag a no-op the caller can't detect.
fn resolve_delegate(
    delegate: bool,
    configured_delegate_id: Option<&str>,
) -> Result<Option<String>, ProposalError> {
    if !delegate {
        return Ok(None);
    }
    configured_delegate_id
        .map(|id| Some(id.to_string()))
        .ok_or_else(|| {
            ProposalError::PolicyViolation(
                "delegate: true requested but no delegate agent is configured".to_string(),
            )
        })
}

fn check_len(field: &str, value: &str, max_chars: usize) -> Result<(), ProposalError> {
    if value.chars().count() > max_chars {
        return Err(ProposalError::PolicyViolation(format!(
            "{field} exceeds {max_chars} characters"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal_agent_wire() -> ProposalWire {
        ProposalWire {
            schema_version: SCHEMA_VERSION,
            origin: ProposalOrigin::TerminalAgent,
            recommended_choice: Some(1),
            choices: vec![ProposalChoiceWire {
                choice: 1,
                title: "Run tests".to_string(),
                rationale: "verify the fix".to_string(),
                actions: vec![ProposalActionWire::Send {
                    input: "cargo test".to_string(),
                }],
            }],
        }
    }

    fn autofix_wire() -> ProposalWire {
        ProposalWire {
            schema_version: SCHEMA_VERSION,
            origin: ProposalOrigin::Autofix,
            recommended_choice: Some(1),
            choices: vec![ProposalChoiceWire {
                choice: 1,
                title: "Fix typo".to_string(),
                rationale: String::new(),
                actions: vec![ProposalActionWire::Send {
                    input: "git status".to_string(),
                }],
            }],
        }
    }

    #[test]
    fn round_trips_through_json() {
        let wire = terminal_agent_wire();
        let json = serde_json::to_string(&wire).unwrap();
        let parsed = parse_proposal_payload(json.as_bytes()).unwrap();
        assert_eq!(parsed.schema_version, SCHEMA_VERSION);
        assert_eq!(parsed.choices.len(), 1);
    }

    #[test]
    fn rejects_oversized_payload_before_parsing() {
        let huge = "x".repeat(MAX_PAYLOAD_BYTES + 1);
        let err = parse_proposal_payload(huge.as_bytes()).unwrap_err();
        assert!(matches!(err, ProposalError::TooLarge { .. }));
    }

    #[test]
    fn rejects_unsupported_schema_version() {
        let mut wire = terminal_agent_wire();
        wire.schema_version = 99;
        let json = serde_json::to_string(&wire).unwrap();
        let err = parse_proposal_payload(json.as_bytes()).unwrap_err();
        assert!(matches!(err, ProposalError::UnsupportedSchemaVersion(99)));
    }

    #[test]
    fn rejects_unknown_top_level_field() {
        let mut value: serde_json::Value = serde_json::to_value(terminal_agent_wire()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("session_id".to_string(), serde_json::json!("sneaky"));
        let bytes = serde_json::to_vec(&value).unwrap();
        let err = parse_proposal_payload(&bytes).unwrap_err();
        assert!(matches!(err, ProposalError::Malformed(_)));
    }

    #[test]
    fn rejects_unknown_action_field() {
        let json = r#"{
            "schema_version": 1,
            "origin": "terminal_agent",
            "choices": [{
                "choice": 1,
                "title": "x",
                "actions": [{"type": "send", "input": "echo hi", "pane_id": "sneaky"}]
            }]
        }"#;
        let err = parse_proposal_payload(json.as_bytes()).unwrap_err();
        assert!(matches!(err, ProposalError::Malformed(_)));
    }

    #[test]
    fn terminal_agent_converts_cleanly() {
        let wire = terminal_agent_wire();
        let set = build_recommendation_set(&wire, false, None, Some("pane-123"), None).unwrap();
        assert_eq!(set.choices.len(), 1);
        match &set.choices[0].actions[0] {
            RecommendedAction::Send { parent, input } => {
                assert_eq!(parent, "pane-123");
                assert_eq!(input, "cargo test");
            }
            other => panic!("unexpected action {other:?}"),
        }
    }

    #[test]
    fn terminal_agent_send_requires_trusted_active_target() {
        let wire = terminal_agent_wire();
        let err = build_recommendation_set(&wire, false, None, None, None).unwrap_err();
        assert!(matches!(err, ProposalError::PolicyViolation(_)));
    }

    #[test]
    fn terminal_agent_panel_injects_trusted_parent() {
        let mut wire = terminal_agent_wire();
        wire.choices[0].actions = vec![ProposalActionWire::Open {
            target: ProposalOpenTargetWire::Panel,
            cwd: None,
            title: None,
            direction: Some("right".to_string()),
            profile: None,
        }];
        let set = build_recommendation_set(&wire, false, None, Some("pane-123"), None).unwrap();
        match &set.choices[0].actions[0] {
            RecommendedAction::Open { parent, .. } => {
                assert_eq!(parent.as_deref(), Some("pane-123"));
            }
            other => panic!("unexpected action {other:?}"),
        }
    }

    #[test]
    fn origin_mismatch_is_rejected() {
        let wire = terminal_agent_wire();
        let err = build_recommendation_set(&wire, true, None, None, None).unwrap_err();
        assert!(matches!(err, ProposalError::PolicyViolation(_)));
    }

    #[test]
    fn autofix_leaves_parent_for_execution_time_binding() {
        let wire = autofix_wire();
        let set = build_recommendation_set(&wire, true, None, None, None).unwrap();
        match &set.choices[0].actions[0] {
            RecommendedAction::Send { parent, .. } => assert_eq!(parent, ""),
            other => panic!("unexpected action {other:?}"),
        }
    }

    #[test]
    fn autofix_rejects_open_action() {
        let mut wire = autofix_wire();
        wire.choices[0].actions = vec![ProposalActionWire::Open {
            target: ProposalOpenTargetWire::Tab,
            cwd: None,
            title: None,
            direction: None,
            profile: None,
        }];
        let err = build_recommendation_set(&wire, true, None, None, None).unwrap_err();
        assert!(matches!(err, ProposalError::PolicyViolation(_)));
    }

    #[test]
    fn autofix_rejects_multiple_choices() {
        let mut wire = autofix_wire();
        let mut second = wire.choices[0].clone();
        second.choice = 2;
        wire.choices.push(second);
        let err = build_recommendation_set(&wire, true, None, None, None).unwrap_err();
        assert!(matches!(err, ProposalError::PolicyViolation(_)));
    }

    #[test]
    fn delegate_true_resolves_configured_delegate_id() {
        let mut wire = terminal_agent_wire();
        wire.choices[0].actions = vec![ProposalActionWire::OpenAndSend {
            target: ProposalOpenTargetWire::Tab,
            input: "echo hi".to_string(),
            delegate: true,
            cwd: None,
            title: None,
            direction: None,
            profile: None,
        }];
        let set = build_recommendation_set(&wire, false, Some("claude"), None, None).unwrap();
        match &set.choices[0].actions[0] {
            RecommendedAction::OpenAndSend { agent, .. } => {
                assert_eq!(agent.as_deref(), Some("claude"));
            }
            other => panic!("unexpected action {other:?}"),
        }
    }

    #[test]
    fn delegate_true_without_configured_delegate_is_rejected() {
        let mut wire = terminal_agent_wire();
        wire.choices[0].actions = vec![ProposalActionWire::OpenAndSend {
            target: ProposalOpenTargetWire::Tab,
            input: "echo hi".to_string(),
            delegate: true,
            cwd: None,
            title: None,
            direction: None,
            profile: None,
        }];
        let err = build_recommendation_set(&wire, false, None, None, None).unwrap_err();
        assert!(matches!(err, ProposalError::PolicyViolation(_)));
    }

    #[test]
    fn delegate_false_never_sets_an_agent_id() {
        let mut wire = terminal_agent_wire();
        wire.choices[0].actions = vec![ProposalActionWire::OpenAndSend {
            target: ProposalOpenTargetWire::Tab,
            input: "echo hi".to_string(),
            delegate: false,
            cwd: None,
            title: None,
            direction: None,
            profile: None,
        }];
        let set = build_recommendation_set(&wire, false, Some("claude"), None, None).unwrap();
        match &set.choices[0].actions[0] {
            RecommendedAction::OpenAndSend { agent, .. } => assert_eq!(agent, &None),
            other => panic!("unexpected action {other:?}"),
        }
    }

    #[test]
    fn coordinator_target_filters_self_targeted_choices() {
        let wire = terminal_agent_wire();
        let err = build_recommendation_set(&wire, false, None, Some("pane-123"), Some("pane-123"))
            .unwrap_err();
        assert!(matches!(err, ProposalError::PolicyViolation(_)));
    }

    #[test]
    fn title_length_cap_is_enforced() {
        let mut wire = terminal_agent_wire();
        wire.choices[0].title = "x".repeat(MAX_TITLE_CHARS + 1);
        let err = build_recommendation_set(&wire, false, None, Some("pane-123"), None).unwrap_err();
        assert!(matches!(err, ProposalError::PolicyViolation(_)));
    }

    #[test]
    fn too_many_choices_is_rejected() {
        let mut wire = terminal_agent_wire();
        for i in 2..=(MAX_CHOICES as usize + 1) {
            let mut extra = wire.choices[0].clone();
            extra.choice = i;
            wire.choices.push(extra);
        }
        let err = build_recommendation_set(&wire, false, None, Some("pane-123"), None).unwrap_err();
        assert!(matches!(err, ProposalError::PolicyViolation(_)));
    }

    #[test]
    fn status_as_str_matches_wire_disposition_table() {
        assert_eq!(ProposalStatus::Presented.as_str(), "presented");
        assert_eq!(ProposalStatus::Duplicate.as_str(), "duplicate");
        assert_eq!(ProposalStatus::Stale.as_str(), "stale");
        assert_eq!(ProposalStatus::Rejected.as_str(), "rejected");
        assert_eq!(ProposalStatus::Unavailable.as_str(), "unavailable");
    }
}
