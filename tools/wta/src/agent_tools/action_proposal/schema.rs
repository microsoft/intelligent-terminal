//! Wire schemas for the session MCP and fallback WTA CLI terminal-action
//! proposal flows. See
//! `doc/specs/WTA-terminal-action-proposals.md`.
//!
//! The preferred MCP tool accepts [`McpProposalWire`], which omits origin,
//! schema, choice numbering, and all routing fields. The retained CLI accepts
//! the versioned [`ProposalWire`]. This module owns:
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
//!   flows through the shared card-surfacing and execution pipeline.
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

/// Max choices per proposal, enforced consistently by
/// [`crate::coordinator::validate_recommendation_set`] (1..=3).
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
    /// A card was already showing for this turn, so this proposal was not
    /// surfaced.
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
/// (CLI or master, pre-relay) or *decode* failure classification. Callers
/// retain the variant when deciding whether a rejection is retryable.
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

/// Payload for `terminal_send`. Required fields are plain (not `Option`), so
/// serde rejects a missing one directly — there is no post-parse
/// "is this field valid for this variant" pass, because the tool a model
/// chose already fixes the shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpSendWire {
    pub title: String,
    #[serde(default)]
    pub rationale: String,
    pub input: String,
}

/// Payload for `terminal_open`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpOpenWire {
    pub title: String,
    #[serde(default)]
    pub rationale: String,
    pub target: ProposalOpenTargetWire,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
}

/// Payload for `terminal_open_and_send`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpOpenAndSendWire {
    pub title: String,
    #[serde(default)]
    pub rationale: String,
    pub target: ProposalOpenTargetWire,
    pub input: String,
    #[serde(default)]
    pub delegate: bool,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
}

/// Which action tool a `tools/call` selected. Replaces the former `type`
/// discriminator field: the choice now lives in the tool name, so an
/// invalid field/variant combination is unrepresentable rather than
/// rejected after the fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpActionTool {
    Send,
    Open,
    OpenAndSend,
}

impl McpActionTool {
    pub const ALL: [Self; 3] = [Self::Send, Self::Open, Self::OpenAndSend];

    pub fn tool_name(self) -> &'static str {
        match self {
            Self::Send => "terminal_send",
            Self::Open => "terminal_open",
            Self::OpenAndSend => "terminal_open_and_send",
        }
    }

    pub fn from_tool_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tool| tool.tool_name() == name)
    }
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

/// Decode a `tools/call` payload for one of the three action tools.
///
/// The selected tool fixes the action shape, so this is a plain deserialize
/// plus a wrap — no per-variant field-applicability pass. `serde` enforces
/// whether a field is required (required fields are not `Option`) and
/// `deny_unknown_fields`
/// rejects anything the chosen tool does not accept.
pub fn parse_mcp_action_payload(
    tool: McpActionTool,
    bytes: &[u8],
    is_autofix_turn: bool,
) -> Result<ProposalWire, ProposalError> {
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(ProposalError::TooLarge { size: bytes.len() });
    }
    let malformed = |e: serde_json::Error| ProposalError::Malformed(e.to_string());
    let (title, rationale, action) = match tool {
        McpActionTool::Send => {
            let wire: McpSendWire = serde_json::from_slice(bytes).map_err(malformed)?;
            (
                wire.title,
                wire.rationale,
                ProposalActionWire::Send { input: wire.input },
            )
        }
        McpActionTool::Open => {
            let wire: McpOpenWire = serde_json::from_slice(bytes).map_err(malformed)?;
            (
                wire.title.clone(),
                wire.rationale,
                ProposalActionWire::Open {
                    target: wire.target,
                    cwd: wire.cwd,
                    title: Some(wire.title),
                    direction: wire.direction,
                    profile: wire.profile,
                },
            )
        }
        McpActionTool::OpenAndSend => {
            let wire: McpOpenAndSendWire = serde_json::from_slice(bytes).map_err(malformed)?;
            (
                wire.title.clone(),
                wire.rationale,
                ProposalActionWire::OpenAndSend {
                    target: wire.target,
                    input: wire.input,
                    delegate: wire.delegate,
                    cwd: wire.cwd,
                    title: Some(wire.title),
                    direction: wire.direction,
                    profile: wire.profile,
                },
            )
        }
    };
    Ok(ProposalWire {
        schema_version: SCHEMA_VERSION,
        origin: if is_autofix_turn {
            ProposalOrigin::Autofix
        } else {
            ProposalOrigin::TerminalAgent
        },
        recommended_choice: Some(1),
        choices: vec![ProposalChoiceWire {
            choice: 1,
            title,
            rationale,
            actions: vec![action],
        }],
    })
}

fn mcp_title_property() -> serde_json::Value {
    serde_json::json!({ "type": "string", "minLength": 1, "maxLength": MAX_TITLE_CHARS })
}

fn mcp_target_properties() -> serde_json::Value {
    serde_json::json!({
        "target": { "type": "string", "enum": ["tab", "panel"] },
        "cwd": { "type": "string", "minLength": 1, "maxLength": MAX_INPUT_CHARS },
        "direction": { "type": "string", "enum": ["right", "left", "up", "down", "auto"] },
        "profile": { "type": "string", "minLength": 1, "maxLength": MAX_TITLE_CHARS }
    })
}

/// Build the schema for one action tool.
///
/// Every advertised property is genuinely accepted by that tool and every
/// required one is listed, so `additionalProperties: false` plus `required`
/// expresses the whole contract. No property needs prose explaining when it
/// does or does not apply.
pub fn mcp_action_input_schema(tool: McpActionTool) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    properties.insert("title".to_string(), mcp_title_property());
    properties.insert(
        "rationale".to_string(),
        serde_json::json!({ "type": "string", "maxLength": MAX_RATIONALE_CHARS }),
    );

    let mut required = vec!["title"];
    if matches!(tool, McpActionTool::Send | McpActionTool::OpenAndSend) {
        properties.insert(
            "input".to_string(),
            serde_json::json!({ "type": "string", "minLength": 1, "maxLength": MAX_INPUT_CHARS }),
        );
        required.push("input");
    }
    if matches!(tool, McpActionTool::Open | McpActionTool::OpenAndSend) {
        let Some(target) = mcp_target_properties().as_object().cloned() else {
            unreachable!("mcp_target_properties builds an object literal")
        };
        properties.extend(target);
        required.push("target");
    }
    if matches!(tool, McpActionTool::OpenAndSend) {
        properties.insert(
            "delegate".to_string(),
            serde_json::json!({ "type": "boolean", "description": "Use the configured delegate agent" }),
        );
    }

    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": required
    })
}

/// User-facing description for one action tool.
pub fn mcp_action_description(tool: McpActionTool) -> &'static str {
    // Routing guidance appears only on the two tools that create a target.
    // "At most one action per turn" lives in prompts/terminal-agent.md rather
    // than being repeated on all three tools here.
    match tool {
        McpActionTool::Send => "Run one bounded command in the user's current terminal pane.",
        McpActionTool::Open => {
            "Open an empty tab or panel in the user's terminal. Panel for related parallel work; \
             tab for independent, different-environment, or long-running work."
        }
        McpActionTool::OpenAndSend => {
            "Open a tab or panel in the user's terminal and run input in it. Panel for related \
             parallel work; tab for independent, different-environment, or long-running work."
        }
    }
}

/// Convert a decoded [`ProposalWire`] into a [`RecommendationSet`], applying
/// origin policy and shared count, length, and coordinator-target validation.
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
///   targets by [`crate::coordinator::validate_recommendation_set_for_coordinator_target`].
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

    // Terminal Agent origin: 1..=3 choices, 1..=3 actions, and the
    // Send+Open+OpenAndSend shape consumed by the shared card pipeline.
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
    use serde_json::{json, Value};

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
    fn flat_mcp_send_converts_to_one_recommended_action() {
        let payload = br#"{
            "title": "Run tests",
            "rationale": "Verify the fix",
            "input": "cargo test"
        }"#;
        let wire = parse_mcp_action_payload(McpActionTool::Send, payload, false).unwrap();
        assert_eq!(wire.origin, ProposalOrigin::TerminalAgent);
        assert_eq!(wire.recommended_choice, Some(1));
        assert_eq!(wire.choices.len(), 1);
        assert_eq!(wire.choices[0].choice, 1);
        assert_eq!(wire.choices[0].title, "Run tests");
        assert_eq!(wire.choices[0].actions.len(), 1);
        assert!(matches!(
            &wire.choices[0].actions[0],
            ProposalActionWire::Send { input } if input == "cargo test"
        ));
    }

    #[test]
    fn flat_mcp_open_and_send_converts_to_one_action() {
        let payload = br#"{
            "title": "Run tests",
            "input": "cargo test",
            "target": "panel",
            "direction": "right",
            "delegate": true
        }"#;
        let wire = parse_mcp_action_payload(McpActionTool::OpenAndSend, payload, false).unwrap();
        assert!(matches!(
            &wire.choices[0].actions[0],
            ProposalActionWire::OpenAndSend {
                target: ProposalOpenTargetWire::Panel,
                input,
                delegate: true,
                direction: Some(direction),
                title: Some(title),
                ..
            } if input == "cargo test" && direction == "right" && title == "Run tests"
        ));
    }

    #[test]
    fn flat_mcp_open_and_send_tab_accepts_direction() {
        let payload = br#"{
            "title": "Project walkthrough",
            "input": "Walk through this project",
            "target": "tab",
            "direction": "auto"
        }"#;
        let wire = parse_mcp_action_payload(McpActionTool::OpenAndSend, payload, false).unwrap();
        let set = build_recommendation_set(&wire, false, None, None, None).unwrap();
        assert!(matches!(
            &set.choices[0].actions[0],
            RecommendedAction::OpenAndSend {
                target: OpenTarget::Tab,
                direction: Some(direction),
                ..
            } if direction == "auto"
        ));
    }

    #[test]
    fn flat_mcp_rejects_nested_legacy_payload() {
        let payload = br#"{
            "recommended_choice": 1,
            "choices": [{
                "title": "Run tests",
                "actions": [{"type": "send", "input": "cargo test"}]
            }]
        }"#;
        let err = parse_mcp_action_payload(McpActionTool::Send, payload, false).unwrap_err();
        assert!(matches!(err, ProposalError::Malformed(_)));
    }

    #[test]
    fn every_action_tool_schema_is_strict_provider_safe() {
        for tool in McpActionTool::ALL {
            let schema = mcp_action_input_schema(tool);
            let name = tool.tool_name();
            assert_eq!(schema.get("type").and_then(Value::as_str), Some("object"));
            assert_eq!(schema.get("additionalProperties"), Some(&json!(false)));
            assert!(schema.pointer("/properties/title").is_some(), "{name}");
            assert!(schema.pointer("/properties/choices").is_none(), "{name}");
            assert!(schema.pointer("/properties/actions").is_none(), "{name}");
            // The discriminator is gone: the tool name carries the shape.
            assert!(schema.pointer("/properties/type").is_none(), "{name}");
            for keyword in ["oneOf", "anyOf", "allOf", "enum", "const", "not"] {
                assert!(
                    schema.get(keyword).is_none(),
                    "{name}: top-level {keyword} is rejected by strict providers"
                );
            }
            let serialized = schema.to_string();
            for keyword in ["oneOf", "anyOf", "allOf"] {
                assert!(
                    !serialized.contains(&format!("\"{keyword}\"")),
                    "{name}: {keyword} must not appear at any depth"
                );
            }
        }
    }

    /// The point of the split: each tool advertises exactly the fields it
    /// accepts, so `additionalProperties: false` alone rejects a field that
    /// belongs to a different action — no separate applicability pass.
    #[test]
    fn each_tool_advertises_exactly_the_fields_it_accepts() {
        let expected: [(McpActionTool, &[&str], &[&str]); 3] = [
            (
                McpActionTool::Send,
                &["title", "rationale", "input"],
                &["title", "input"],
            ),
            (
                McpActionTool::Open,
                &[
                    "title",
                    "rationale",
                    "target",
                    "cwd",
                    "direction",
                    "profile",
                ],
                &["title", "target"],
            ),
            (
                McpActionTool::OpenAndSend,
                &[
                    "title",
                    "rationale",
                    "input",
                    "target",
                    "cwd",
                    "direction",
                    "profile",
                    "delegate",
                ],
                &["title", "target", "input"],
            ),
        ];
        for (tool, properties, required) in expected {
            let schema = mcp_action_input_schema(tool);
            let advertised: std::collections::BTreeSet<_> = schema
                .pointer("/properties")
                .and_then(Value::as_object)
                .expect("properties")
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(
                advertised,
                properties.iter().copied().collect(),
                "{}",
                tool.tool_name()
            );
            let advertised_required: std::collections::BTreeSet<_> = schema
                .pointer("/required")
                .and_then(Value::as_array)
                .expect("required")
                .iter()
                .filter_map(Value::as_str)
                .collect();
            assert_eq!(
                advertised_required,
                required.iter().copied().collect(),
                "{}",
                tool.tool_name()
            );
        }
    }

    /// A field belonging to another action is now rejected by `serde`'s
    /// `deny_unknown_fields`, not by a hand-written applicability check.
    #[test]
    fn send_rejects_a_target_only_field_as_unknown() {
        let err = parse_mcp_action_payload(
            McpActionTool::Send,
            br#"{"title":"Show weather quickly","input":"curl example.com","cwd":"C:\\repo"}"#,
            false,
        )
        .unwrap_err();
        let ProposalError::Malformed(message) = err else {
            panic!("expected a malformed payload error");
        };
        assert!(message.contains("cwd"), "{message}");

        // The same payload without the stray field parses.
        parse_mcp_action_payload(
            McpActionTool::Send,
            br#"{"title":"Show weather quickly","input":"curl example.com"}"#,
            false,
        )
        .expect("send payload without the stray field must parse");
    }

    #[test]
    fn missing_required_field_is_rejected_by_serde() {
        for (tool, payload) in [
            (McpActionTool::Send, &br#"{"title":"Run tests"}"#[..]),
            (McpActionTool::Open, &br#"{"title":"New tab"}"#[..]),
            (
                McpActionTool::OpenAndSend,
                &br#"{"title":"New tab","target":"tab"}"#[..],
            ),
        ] {
            assert!(
                matches!(
                    parse_mcp_action_payload(tool, payload, false),
                    Err(ProposalError::Malformed(_))
                ),
                "{}",
                tool.tool_name()
            );
        }
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
