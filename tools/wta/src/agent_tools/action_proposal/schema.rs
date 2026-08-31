//! Wire schemas for the session MCP and fallback WTA CLI terminal-action
//! proposal flows. See
//! `doc/specs/WTA-terminal-action-proposals.md`.
//!
//! Each preferred MCP tool accepts an intent-specific flat payload that omits
//! origin, schema, choice numbering, and all routing fields. The retained CLI
//! accepts the versioned [`ProposalWire`]. This module owns:
//!
//! * the strict (`deny_unknown_fields`) wire types — deliberately narrower
//!   than [`crate::coordinator::RecommendationSet`]: they never accept a
//!   session/helper/window/tab/pane id, and delegation is a distinct public
//!   intent that can ask for "the user's configured delegate" but never name
//!   an arbitrary agent;
//! * origin-aware policy (`ProposalOrigin::TerminalAgent` vs `::Autofix`);
//! * size/count bounds enforced *before* `serde_json` ever sees the bytes;
//! * conversion into [`crate::coordinator::RecommendationSet`], which then
//!   flows through the shared card-surfacing and execution pipeline.
//!
//! The proposal travels over the owning Helper's direct proposal pipe; Master
//! is not involved. The Helper invokes this module from App's direct proposal
//! validation path and is solely responsible for decoding and policy checks.

use serde::{Deserialize, Deserializer, Serialize};

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

fn deserialize_present_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

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

/// Payload for `run_command`. Required fields are plain (not `Option`), so
/// serde rejects a missing one directly — there is no post-parse
/// "is this field valid for this variant" pass, because the tool a model
/// chose already fixes the shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpRunCommandWire {
    pub summary: String,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub reason: Option<String>,
    pub command: String,
}

/// Payload for `open_workspace`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpOpenWorkspaceWire {
    pub summary: String,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub reason: Option<String>,
    pub placement: McpWorkspacePlacementWire,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub working_directory: Option<String>,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub split_direction: Option<McpSplitDirectionWire>,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub profile: Option<String>,
}

/// Payload for `run_command_in_workspace`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpRunCommandInWorkspaceWire {
    pub summary: String,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub reason: Option<String>,
    pub command: String,
    pub placement: McpWorkspacePlacementWire,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub working_directory: Option<String>,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub split_direction: Option<McpSplitDirectionWire>,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub profile: Option<String>,
}

/// Payload for `delegate_task`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpDelegateTaskWire {
    pub summary: String,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub reason: Option<String>,
    pub task: String,
    pub placement: McpWorkspacePlacementWire,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub working_directory: Option<String>,
    #[serde(default, deserialize_with = "deserialize_present_option")]
    pub split_direction: Option<McpSplitDirectionWire>,
}

/// Which action tool a `tools/call` selected. Replaces the former `type`
/// discriminator field: the choice now lives in the tool name, so an
/// invalid field/variant combination is unrepresentable rather than
/// rejected after the fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpActionTool {
    RunCommand,
    OpenWorkspace,
    RunCommandInWorkspace,
    DelegateTask,
}

impl McpActionTool {
    pub const ALL: [Self; 4] = [
        Self::RunCommand,
        Self::OpenWorkspace,
        Self::RunCommandInWorkspace,
        Self::DelegateTask,
    ];

    pub fn tool_name(self) -> &'static str {
        match self {
            Self::RunCommand => "run_command",
            Self::OpenWorkspace => "open_workspace",
            Self::RunCommandInWorkspace => "run_command_in_workspace",
            Self::DelegateTask => "delegate_task",
        }
    }

    pub fn from_tool_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tool| tool.tool_name() == name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpWorkspacePlacementWire {
    NewTab,
    NewSplit,
}

impl From<McpWorkspacePlacementWire> for ProposalOpenTargetWire {
    fn from(value: McpWorkspacePlacementWire) -> Self {
        match value {
            McpWorkspacePlacementWire::NewTab => ProposalOpenTargetWire::Tab,
            McpWorkspacePlacementWire::NewSplit => ProposalOpenTargetWire::Panel,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpSplitDirectionWire {
    Right,
    Left,
    Up,
    Down,
    Auto,
}

impl McpSplitDirectionWire {
    fn into_string(self) -> String {
        match self {
            Self::Right => "right",
            Self::Left => "left",
            Self::Up => "up",
            Self::Down => "down",
            Self::Auto => "auto",
        }
        .to_string()
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
/// `agent: Option<String>` from [`RecommendedAction`] is intentionally not
/// exposed here. The public `delegate_task` tool selects the configured
/// delegate by choosing its tool name, while the other tools can never name
/// an arbitrary agent id. `Open` never carries an agent selector at all
/// (mirrors [`RecommendedAction::Open`], which has no `agent` field — a bare
/// `Open` just opens a plain shell target, no agent involved).
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

/// Decode a `tools/call` payload for one of the four action tools.
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
        McpActionTool::RunCommand => {
            let wire: McpRunCommandWire = serde_json::from_slice(bytes).map_err(malformed)?;
            validate_mcp_required_text("summary", &wire.summary, MAX_TITLE_CHARS)?;
            validate_mcp_optional_nonempty_text(
                "reason",
                wire.reason.as_deref(),
                MAX_RATIONALE_CHARS,
            )?;
            validate_mcp_required_text("command", &wire.command, MAX_INPUT_CHARS)?;
            (
                wire.summary,
                wire.reason.unwrap_or_default(),
                ProposalActionWire::Send {
                    input: wire.command,
                },
            )
        }
        McpActionTool::OpenWorkspace => {
            let wire: McpOpenWorkspaceWire = serde_json::from_slice(bytes).map_err(malformed)?;
            validate_mcp_required_text("summary", &wire.summary, MAX_TITLE_CHARS)?;
            validate_mcp_optional_nonempty_text(
                "reason",
                wire.reason.as_deref(),
                MAX_RATIONALE_CHARS,
            )?;
            validate_mcp_optional_nonempty_text(
                "working_directory",
                wire.working_directory.as_deref(),
                MAX_INPUT_CHARS,
            )?;
            validate_mcp_optional_nonempty_text(
                "profile",
                wire.profile.as_deref(),
                MAX_TITLE_CHARS,
            )?;
            (
                wire.summary.clone(),
                wire.reason.unwrap_or_default(),
                ProposalActionWire::Open {
                    target: wire.placement.into(),
                    cwd: wire.working_directory,
                    title: Some(wire.summary),
                    direction: wire.split_direction.map(McpSplitDirectionWire::into_string),
                    profile: wire.profile,
                },
            )
        }
        McpActionTool::RunCommandInWorkspace => {
            let wire: McpRunCommandInWorkspaceWire =
                serde_json::from_slice(bytes).map_err(malformed)?;
            validate_mcp_required_text("summary", &wire.summary, MAX_TITLE_CHARS)?;
            validate_mcp_optional_nonempty_text(
                "reason",
                wire.reason.as_deref(),
                MAX_RATIONALE_CHARS,
            )?;
            validate_mcp_required_text("command", &wire.command, MAX_INPUT_CHARS)?;
            validate_mcp_optional_nonempty_text(
                "working_directory",
                wire.working_directory.as_deref(),
                MAX_INPUT_CHARS,
            )?;
            validate_mcp_optional_nonempty_text(
                "profile",
                wire.profile.as_deref(),
                MAX_TITLE_CHARS,
            )?;
            (
                wire.summary.clone(),
                wire.reason.unwrap_or_default(),
                ProposalActionWire::OpenAndSend {
                    target: wire.placement.into(),
                    input: wire.command,
                    delegate: false,
                    cwd: wire.working_directory,
                    title: Some(wire.summary),
                    direction: wire.split_direction.map(McpSplitDirectionWire::into_string),
                    profile: wire.profile,
                },
            )
        }
        McpActionTool::DelegateTask => {
            let wire: McpDelegateTaskWire = serde_json::from_slice(bytes).map_err(malformed)?;
            validate_mcp_required_text("summary", &wire.summary, MAX_TITLE_CHARS)?;
            validate_mcp_optional_nonempty_text(
                "reason",
                wire.reason.as_deref(),
                MAX_RATIONALE_CHARS,
            )?;
            validate_mcp_required_text("task", &wire.task, MAX_INPUT_CHARS)?;
            validate_mcp_optional_nonempty_text(
                "working_directory",
                wire.working_directory.as_deref(),
                MAX_INPUT_CHARS,
            )?;
            (
                wire.summary.clone(),
                wire.reason.unwrap_or_default(),
                ProposalActionWire::OpenAndSend {
                    target: wire.placement.into(),
                    input: wire.task,
                    delegate: true,
                    cwd: wire.working_directory,
                    title: Some(wire.summary),
                    direction: wire.split_direction.map(McpSplitDirectionWire::into_string),
                    profile: None,
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

fn validate_mcp_required_text(
    field: &str,
    value: &str,
    max_chars: usize,
) -> Result<(), ProposalError> {
    if value.trim().is_empty() {
        return Err(ProposalError::Malformed(format!(
            "field `{field}` must not be empty or whitespace-only"
        )));
    }
    validate_mcp_optional_text(field, Some(value), max_chars)
}

fn validate_mcp_optional_nonempty_text(
    field: &str,
    value: Option<&str>,
    max_chars: usize,
) -> Result<(), ProposalError> {
    if value.is_some_and(|value| value.trim().is_empty()) {
        return Err(ProposalError::Malformed(format!(
            "field `{field}` must not be empty or whitespace-only"
        )));
    }
    validate_mcp_optional_text(field, value, max_chars)
}

fn validate_mcp_optional_text(
    field: &str,
    value: Option<&str>,
    max_chars: usize,
) -> Result<(), ProposalError> {
    if value.is_some_and(|value| value.chars().count() > max_chars) {
        return Err(ProposalError::Malformed(format!(
            "field `{field}` exceeds {max_chars} characters"
        )));
    }
    Ok(())
}

fn mcp_summary_property() -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "minLength": 1,
        "pattern": r"\S",
        "maxLength": MAX_TITLE_CHARS,
        "description": "Concise user-visible summary of the proposed operation."
    })
}

fn mcp_reason_property() -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "minLength": 1,
        "pattern": r"\S",
        "maxLength": MAX_RATIONALE_CHARS,
        "description": "Why the operation is needed. Omit when the summary is sufficient."
    })
}

fn mcp_command_property(destination: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "minLength": 1,
        "pattern": r"\S",
        "maxLength": MAX_INPUT_CHARS,
        "description": format!("Exact shell command to run {destination}.")
    })
}

fn mcp_workspace_properties() -> serde_json::Map<String, serde_json::Value> {
    let serde_json::Value::Object(properties) = serde_json::json!({
        "placement": {
            "type": "string",
            "enum": ["new_tab", "new_split"],
            "description": "Where to create the workspace: new_tab for an independent tab or new_split for a split beside the active pane."
        },
        "working_directory": {
            "type": "string",
            "minLength": 1,
            "pattern": r"\S",
            "maxLength": MAX_INPUT_CHARS,
            "description": "Working directory for the new workspace. Omit to use the terminal's default behavior."
        },
        "split_direction": {
            "type": "string",
            "enum": ["right", "left", "up", "down", "auto"],
            "description": "Preferred split direction hint; auto lets Intelligent Terminal choose."
        }
    }) else {
        unreachable!("workspace properties are an object literal")
    };
    properties
}

fn mcp_profile_property() -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "minLength": 1,
        "pattern": r"\S",
        "maxLength": MAX_TITLE_CHARS,
        "description": "Terminal profile to use for the new workspace."
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
    properties.insert("summary".to_string(), mcp_summary_property());
    properties.insert("reason".to_string(), mcp_reason_property());

    let required = match tool {
        McpActionTool::RunCommand => {
            properties.insert(
                "command".to_string(),
                mcp_command_property("in the user's current active shell"),
            );
            vec!["summary", "command"]
        }
        McpActionTool::OpenWorkspace => {
            properties.extend(mcp_workspace_properties());
            properties.insert("profile".to_string(), mcp_profile_property());
            vec!["summary", "placement"]
        }
        McpActionTool::RunCommandInWorkspace => {
            properties.insert(
                "command".to_string(),
                mcp_command_property("in the new workspace"),
            );
            properties.extend(mcp_workspace_properties());
            properties.insert("profile".to_string(), mcp_profile_property());
            vec!["summary", "command", "placement"]
        }
        McpActionTool::DelegateTask => {
            properties.insert(
                "task".to_string(),
                serde_json::json!({
                    "type": "string",
                    "minLength": 1,
                    "pattern": r"\S",
                    "maxLength": MAX_INPUT_CHARS,
                    "description": "Self-contained task for the configured delegate agent, including the goal, relevant context, constraints, and completion criteria."
                }),
            );
            properties.extend(mcp_workspace_properties());
            vec!["summary", "task", "placement"]
        }
    };

    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": required
    })
}

/// User-facing description for one action tool.
pub fn mcp_action_description(tool: McpActionTool) -> &'static str {
    match tool {
        McpActionTool::RunCommand => {
            "Hand off one shell command for user confirmation and execution in the user's current active shell. Use when running that command in the user's pane is the requested outcome, not for Agent-owned investigation or validation."
        }
        McpActionTool::OpenWorkspace => {
            "Hand off creation of an empty terminal workspace without running a command or starting an agent. Use when opening a new tab or split is the requested outcome, not for Agent-owned investigation."
        }
        McpActionTool::RunCommandInWorkspace => {
            "Hand off creation of a new terminal workspace and execution of one shell command there. Use when the requested outcome should run in a new tab or split, not for Agent-owned investigation or validation."
        }
        McpActionTool::DelegateTask => {
            "Hand off a self-contained task to the configured delegate agent in a new workspace. Use only when another agent should own the requested work, not for this Agent's own work or a plain shell command."
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
    fn run_command_converts_to_one_recommended_action() {
        let payload = br#"{
            "summary": "Run tests",
            "reason": "Verify the fix",
            "command": "cargo test"
        }"#;
        let wire = parse_mcp_action_payload(McpActionTool::RunCommand, payload, false).unwrap();
        assert_eq!(wire.origin, ProposalOrigin::TerminalAgent);
        assert_eq!(wire.recommended_choice, Some(1));
        assert_eq!(wire.choices.len(), 1);
        assert_eq!(wire.choices[0].choice, 1);
        assert_eq!(wire.choices[0].title, "Run tests");
        assert_eq!(wire.choices[0].rationale, "Verify the fix");
        assert_eq!(wire.choices[0].actions.len(), 1);
        assert!(matches!(
            &wire.choices[0].actions[0],
            ProposalActionWire::Send { input } if input == "cargo test"
        ));
    }

    #[test]
    fn delegate_task_converts_to_configured_delegate_action() {
        let payload = br#"{
            "summary": "Investigate tests",
            "task": "Investigate and fix the failing tests",
            "placement": "new_split",
            "split_direction": "right"
        }"#;
        let wire = parse_mcp_action_payload(McpActionTool::DelegateTask, payload, false).unwrap();
        assert!(matches!(
            &wire.choices[0].actions[0],
            ProposalActionWire::OpenAndSend {
                target: ProposalOpenTargetWire::Panel,
                input,
                delegate: true,
                direction: Some(direction),
                title: Some(title),
                profile: None,
                ..
            } if input == "Investigate and fix the failing tests"
                && direction == "right"
                && title == "Investigate tests"
        ));
    }

    #[test]
    fn run_command_in_workspace_tab_accepts_direction() {
        let payload = br#"{
            "summary": "Project walkthrough",
            "command": "walkthrough",
            "placement": "new_tab",
            "split_direction": "auto"
        }"#;
        let wire =
            parse_mcp_action_payload(McpActionTool::RunCommandInWorkspace, payload, false).unwrap();
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
    fn open_workspace_maps_public_fields_to_existing_open_action() {
        let payload = br#"{
            "summary": "Open build shell",
            "reason": "Keep the build separate",
            "placement": "new_split",
            "working_directory": "C:\\repo",
            "split_direction": "down",
            "profile": "PowerShell"
        }"#;
        let wire = parse_mcp_action_payload(McpActionTool::OpenWorkspace, payload, false).unwrap();
        assert_eq!(wire.choices[0].title, "Open build shell");
        assert_eq!(wire.choices[0].rationale, "Keep the build separate");
        assert!(matches!(
            &wire.choices[0].actions[0],
            ProposalActionWire::Open {
                target: ProposalOpenTargetWire::Panel,
                cwd: Some(cwd),
                title: Some(title),
                direction: Some(direction),
                profile: Some(profile),
            } if cwd == r"C:\repo"
                && title == "Open build shell"
                && direction == "down"
                && profile == "PowerShell"
        ));
    }

    #[test]
    fn run_command_in_workspace_never_delegates() {
        let payload = br#"{
            "summary": "Run build",
            "command": "cargo build",
            "placement": "new_tab",
            "profile": "PowerShell"
        }"#;
        let wire =
            parse_mcp_action_payload(McpActionTool::RunCommandInWorkspace, payload, false).unwrap();
        assert!(matches!(
            &wire.choices[0].actions[0],
            ProposalActionWire::OpenAndSend {
                target: ProposalOpenTargetWire::Tab,
                input,
                delegate: false,
                profile: Some(profile),
                ..
            } if input == "cargo build" && profile == "PowerShell"
        ));
    }

    #[test]
    fn delegate_task_rejects_profile_agent_and_delegate_fields() {
        for extra in [
            r#""profile":"PowerShell""#,
            r#""agent":"claude""#,
            r#""delegate":true"#,
        ] {
            let payload = format!(
                r#"{{
                    "summary":"Investigate",
                    "task":"Find and fix the issue",
                    "placement":"new_tab",
                    {extra}
                }}"#
            );
            assert!(
                matches!(
                    parse_mcp_action_payload(
                        McpActionTool::DelegateTask,
                        payload.as_bytes(),
                        false
                    ),
                    Err(ProposalError::Malformed(_))
                ),
                "{extra}"
            );
        }
    }

    #[test]
    fn public_action_fields_reject_null_and_invalid_enums() {
        for (tool, payload) in [
            (
                McpActionTool::RunCommand,
                &br#"{"summary":"Run","command":"echo hi","reason":null}"#[..],
            ),
            (
                McpActionTool::OpenWorkspace,
                &br#"{"summary":"Open","placement":"tab"}"#[..],
            ),
            (
                McpActionTool::OpenWorkspace,
                &br#"{"summary":"Open","placement":"new_tab","working_directory":null}"#[..],
            ),
            (
                McpActionTool::RunCommandInWorkspace,
                &br#"{"summary":"Run","command":"echo hi","placement":"new_split","split_direction":"center"}"#[..],
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
    fn public_action_text_constraints_match_the_advertised_schema() {
        let long_profile = "p".repeat(MAX_TITLE_CHARS + 1);
        let long_working_directory = "w".repeat(MAX_INPUT_CHARS + 1);
        for (tool, payload) in [
            (
                McpActionTool::RunCommand,
                json!({"summary":"Run","command":""}),
            ),
            (
                McpActionTool::OpenWorkspace,
                json!({"summary":"","placement":"new_tab"}),
            ),
            (
                McpActionTool::OpenWorkspace,
                json!({
                    "summary":"Open",
                    "placement":"new_tab",
                    "working_directory":""
                }),
            ),
            (
                McpActionTool::RunCommandInWorkspace,
                json!({
                    "summary":"Run",
                    "command":"echo hi",
                    "placement":"new_tab",
                    "profile":long_profile
                }),
            ),
            (
                McpActionTool::DelegateTask,
                json!({
                    "summary":"Delegate",
                    "task":"Investigate",
                    "placement":"new_tab",
                    "working_directory":long_working_directory
                }),
            ),
        ] {
            let payload = serde_json::to_vec(&payload).unwrap();
            assert!(
                matches!(
                    parse_mcp_action_payload(tool, &payload, false),
                    Err(ProposalError::Malformed(_))
                ),
                "{}",
                tool.tool_name()
            );
        }
    }

    #[test]
    fn public_action_required_text_rejects_whitespace_only_values() {
        for (tool, payload) in [
            (
                McpActionTool::RunCommand,
                json!({"summary":" \t ","command":"echo hi"}),
            ),
            (
                McpActionTool::RunCommand,
                json!({"summary":"Run","command":"\r\n"}),
            ),
            (
                McpActionTool::OpenWorkspace,
                json!({"summary":"\n","placement":"new_tab"}),
            ),
            (
                McpActionTool::RunCommandInWorkspace,
                json!({"summary":"Run","command":" ","placement":"new_tab"}),
            ),
            (
                McpActionTool::DelegateTask,
                json!({"summary":"Delegate","task":"\t","placement":"new_tab"}),
            ),
        ] {
            let payload = serde_json::to_vec(&payload).unwrap();
            assert!(
                matches!(
                    parse_mcp_action_payload(tool, &payload, false),
                    Err(ProposalError::Malformed(_))
                ),
                "{}",
                tool.tool_name()
            );
        }
    }

    #[test]
    fn public_action_optional_nonempty_text_rejects_whitespace_only_values() {
        for (tool, payload) in [
            (
                McpActionTool::OpenWorkspace,
                json!({
                    "summary":"Open",
                    "placement":"new_tab",
                    "working_directory":" \t "
                }),
            ),
            (
                McpActionTool::RunCommandInWorkspace,
                json!({
                    "summary":"Run",
                    "command":"echo hi",
                    "placement":"new_tab",
                    "profile":"\r\n"
                }),
            ),
        ] {
            let payload = serde_json::to_vec(&payload).unwrap();
            assert!(
                matches!(
                    parse_mcp_action_payload(tool, &payload, false),
                    Err(ProposalError::Malformed(_))
                ),
                "{}",
                tool.tool_name()
            );
        }
    }

    #[test]
    fn reason_schema_and_runtime_require_nonempty_text_when_present() {
        for (tool, mut payload) in [
            (
                McpActionTool::RunCommand,
                json!({"summary":"Run","command":"echo hi"}),
            ),
            (
                McpActionTool::OpenWorkspace,
                json!({"summary":"Open","placement":"new_tab"}),
            ),
            (
                McpActionTool::RunCommandInWorkspace,
                json!({"summary":"Run","command":"echo hi","placement":"new_tab"}),
            ),
            (
                McpActionTool::DelegateTask,
                json!({"summary":"Delegate","task":"Investigate","placement":"new_tab"}),
            ),
        ] {
            let schema = mcp_action_input_schema(tool);
            assert_eq!(
                schema.pointer("/properties/reason/minLength"),
                Some(&json!(1)),
                "{}",
                tool.tool_name()
            );
            assert_eq!(
                schema.pointer("/properties/reason/pattern"),
                Some(&json!(r"\S")),
                "{}",
                tool.tool_name()
            );

            for reason in ["", " \t\r\n "] {
                payload["reason"] = json!(reason);
                let bytes = serde_json::to_vec(&payload).unwrap();
                assert!(
                    matches!(
                        parse_mcp_action_payload(tool, &bytes, false),
                        Err(ProposalError::Malformed(_))
                    ),
                    "{} accepted {reason:?}",
                    tool.tool_name()
                );
            }
        }
    }

    #[test]
    fn public_schema_marks_nonempty_text_as_non_whitespace() {
        for (tool, properties) in [
            (McpActionTool::RunCommand, &["summary", "command"][..]),
            (
                McpActionTool::OpenWorkspace,
                &["summary", "working_directory", "profile"][..],
            ),
            (
                McpActionTool::RunCommandInWorkspace,
                &["summary", "command", "working_directory", "profile"][..],
            ),
            (
                McpActionTool::DelegateTask,
                &["summary", "task", "working_directory"][..],
            ),
        ] {
            let schema = mcp_action_input_schema(tool);
            for property in properties {
                assert_eq!(
                    schema.pointer(&format!("/properties/{property}/pattern")),
                    Some(&json!(r"\S")),
                    "{} property {property}",
                    tool.tool_name()
                );
            }
        }
    }

    #[test]
    fn accepted_public_text_preserves_original_whitespace() {
        let run = parse_mcp_action_payload(
            McpActionTool::RunCommand,
            br#"{"summary":" Run ","reason":" Because ","command":" echo hi "}"#,
            false,
        )
        .unwrap();
        assert_eq!(run.choices[0].title, " Run ");
        assert_eq!(run.choices[0].rationale, " Because ");
        assert!(matches!(
            &run.choices[0].actions[0],
            ProposalActionWire::Send { input } if input == " echo hi "
        ));

        let open = parse_mcp_action_payload(
            McpActionTool::OpenWorkspace,
            br#"{"summary":" Open ","placement":"new_tab","working_directory":" C:\\repo ","profile":" PowerShell "}"#,
            false,
        )
        .unwrap();
        assert!(matches!(
            &open.choices[0].actions[0],
            ProposalActionWire::Open {
                cwd: Some(cwd),
                profile: Some(profile),
                ..
            } if cwd == r" C:\repo " && profile == " PowerShell "
        ));
    }

    #[test]
    fn removed_tool_names_are_not_recognized() {
        for name in ["terminal_send", "terminal_open", "terminal_open_and_send"] {
            assert_eq!(McpActionTool::from_tool_name(name), None, "{name}");
        }
    }

    #[test]
    fn public_action_descriptions_distinguish_terminal_handoffs_from_agent_owned_work() {
        for (tool, expected_outcome) in [
            (McpActionTool::RunCommand, "requested outcome"),
            (McpActionTool::OpenWorkspace, "requested outcome"),
            (McpActionTool::RunCommandInWorkspace, "requested outcome"),
            (McpActionTool::DelegateTask, "another agent should own"),
        ] {
            let description = mcp_action_description(tool);
            assert!(description.contains("Hand off"), "{}", tool.tool_name());
            assert!(
                description.contains(expected_outcome),
                "{}",
                tool.tool_name()
            );
            assert!(
                description.contains("Agent-owned") || description.contains("this Agent's own"),
                "{}",
                tool.tool_name()
            );
        }
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
        let err = parse_mcp_action_payload(McpActionTool::RunCommand, payload, false).unwrap_err();
        assert!(matches!(err, ProposalError::Malformed(_)));
    }

    #[test]
    fn every_action_tool_schema_is_strict_provider_safe() {
        for tool in McpActionTool::ALL {
            let schema = mcp_action_input_schema(tool);
            let name = tool.tool_name();
            assert_eq!(schema.get("type").and_then(Value::as_str), Some("object"));
            assert_eq!(schema.get("additionalProperties"), Some(&json!(false)));
            assert!(schema.pointer("/properties/summary").is_some(), "{name}");
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
            for property in schema
                .pointer("/properties")
                .and_then(Value::as_object)
                .expect("properties")
                .values()
            {
                assert!(
                    property
                        .get("description")
                        .and_then(Value::as_str)
                        .is_some(),
                    "{name}: every public property needs a description"
                );
            }
        }
    }

    /// The point of the split: each tool advertises exactly the fields it
    /// accepts, so `additionalProperties: false` alone rejects a field that
    /// belongs to a different action — no separate applicability pass.
    #[test]
    fn each_tool_advertises_exactly_the_fields_it_accepts() {
        let expected: [(McpActionTool, &[&str], &[&str]); 4] = [
            (
                McpActionTool::RunCommand,
                &["summary", "reason", "command"],
                &["summary", "command"],
            ),
            (
                McpActionTool::OpenWorkspace,
                &[
                    "summary",
                    "reason",
                    "placement",
                    "working_directory",
                    "split_direction",
                    "profile",
                ],
                &["summary", "placement"],
            ),
            (
                McpActionTool::RunCommandInWorkspace,
                &[
                    "summary",
                    "reason",
                    "command",
                    "placement",
                    "working_directory",
                    "split_direction",
                    "profile",
                ],
                &["summary", "command", "placement"],
            ),
            (
                McpActionTool::DelegateTask,
                &[
                    "summary",
                    "reason",
                    "task",
                    "placement",
                    "working_directory",
                    "split_direction",
                ],
                &["summary", "task", "placement"],
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
    fn run_command_rejects_a_workspace_only_field_as_unknown() {
        let err = parse_mcp_action_payload(
            McpActionTool::RunCommand,
            br#"{"summary":"Show weather quickly","command":"curl example.com","working_directory":"C:\\repo"}"#,
            false,
        )
        .unwrap_err();
        let ProposalError::Malformed(message) = err else {
            panic!("expected a malformed payload error");
        };
        assert!(message.contains("working_directory"), "{message}");

        // The same payload without the stray field parses.
        parse_mcp_action_payload(
            McpActionTool::RunCommand,
            br#"{"summary":"Show weather quickly","command":"curl example.com"}"#,
            false,
        )
        .expect("run_command payload without the stray field must parse");
    }

    #[test]
    fn missing_required_field_is_rejected_by_serde() {
        for (tool, payload) in [
            (
                McpActionTool::RunCommand,
                &br#"{"summary":"Run tests"}"#[..],
            ),
            (
                McpActionTool::OpenWorkspace,
                &br#"{"summary":"New tab"}"#[..],
            ),
            (
                McpActionTool::RunCommandInWorkspace,
                &br#"{"summary":"New tab","placement":"new_tab"}"#[..],
            ),
            (
                McpActionTool::DelegateTask,
                &br#"{"summary":"Delegate","placement":"new_tab"}"#[..],
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
