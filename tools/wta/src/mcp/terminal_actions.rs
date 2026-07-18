use std::sync::Arc;

use agent_client_protocol as acp;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{RouteRegistry, Tool, ToolContext};

pub const INTELLTERM_METHOD_PROPOSE_TERMINAL_ACTIONS: &str =
    "_intellterm.wta/propose_terminal_actions";
pub const TOOL_NAME: &str = "propose_terminal_actions";

const MAX_CWD_CHARS: usize = 4 * 1024;
const MAX_INPUT_CHARS: usize = 16 * 1024;
const MAX_PROFILE_CHARS: usize = 512;
const MAX_RATIONALE_CHARS: usize = 2 * 1024;
const MAX_TITLE_CHARS: usize = 160;
const MAX_CHOICES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreferredInputAction {
    Insert,
    Execute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposedOpenTarget {
    Tab,
    Panel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposedDestination {
    Shell,
    Delegate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposedSplitDirection {
    Right,
    Left,
    Up,
    Down,
    Auto,
}

impl ProposedSplitDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Right => "right",
            Self::Left => "left",
            Self::Up => "up",
            Self::Down => "down",
            Self::Auto => "auto",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProposedTerminalAction {
    SendInput {
        input: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preferred_action: Option<PreferredInputAction>,
    },
    Open {
        target: ProposedOpenTarget,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        direction: Option<ProposedSplitDirection>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<String>,
    },
    OpenAndSend {
        target: ProposedOpenTarget,
        destination: ProposedDestination,
        input: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        direction: Option<ProposedSplitDirection>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<String>,
    },
}

fn validate_text(
    name: &str,
    text: &str,
    max_chars: usize,
    allow_blank: bool,
) -> Result<(), String> {
    if text.contains('\0') {
        return Err(format!("{name} must not contain NUL"));
    }
    if !allow_blank && text.trim().is_empty() {
        return Err(format!("{name} must not be empty"));
    }
    if text.chars().count() > max_chars {
        return Err(format!("{name} exceeds the {max_chars}-character limit"));
    }
    Ok(())
}

fn validate_single_line_text(
    name: &str,
    text: &str,
    max_chars: usize,
    allow_blank: bool,
) -> Result<(), String> {
    validate_text(name, text, max_chars, allow_blank)?;
    if text.contains('\r') || text.contains('\n') {
        return Err(format!("{name} must be a single line"));
    }
    Ok(())
}

impl ProposedTerminalAction {
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::SendInput { input, .. } => {
                validate_single_line_text("input", input, MAX_INPUT_CHARS, false)
            }
            Self::Open {
                target,
                cwd,
                title,
                direction,
                profile,
            } => validate_open_fields(*target, cwd, title, *direction, profile),
            Self::OpenAndSend {
                target,
                input,
                cwd,
                title,
                direction,
                profile,
                ..
            } => {
                validate_text("input", input, MAX_INPUT_CHARS, false)?;
                validate_open_fields(*target, cwd, title, *direction, profile)
            }
        }
    }

    pub fn requires_active_pane(&self) -> bool {
        match self {
            Self::SendInput { .. } => true,
            Self::Open { target, .. } | Self::OpenAndSend { target, .. } => {
                *target == ProposedOpenTarget::Panel
            }
        }
    }
}

fn validate_open_fields(
    target: ProposedOpenTarget,
    cwd: &Option<String>,
    title: &Option<String>,
    direction: Option<ProposedSplitDirection>,
    profile: &Option<String>,
) -> Result<(), String> {
    if target == ProposedOpenTarget::Tab && direction.is_some() {
        return Err("direction is only valid when target is panel".to_string());
    }
    if let Some(cwd) = cwd {
        validate_single_line_text("cwd", cwd, MAX_CWD_CHARS, false)?;
    }
    if let Some(title) = title {
        validate_single_line_text("title", title, MAX_TITLE_CHARS, false)?;
    }
    if let Some(profile) = profile {
        validate_single_line_text("profile", profile, MAX_PROFILE_CHARS, false)?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedTerminalChoice {
    pub title: String,
    #[serde(default)]
    pub rationale: String,
    pub action: ProposedTerminalAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalActionsProposal {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended_choice: Option<usize>,
    pub choices: Vec<ProposedTerminalChoice>,
}

impl TerminalActionsProposal {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_CHOICES).contains(&self.choices.len()) {
            return Err(format!(
                "choices must contain 1 to {MAX_CHOICES} items"
            ));
        }
        if self
            .recommended_choice
            .is_some_and(|choice| choice == 0 || choice > self.choices.len())
        {
            return Err("recommended_choice must identify one of the supplied choices".to_string());
        }
        for choice in &self.choices {
            validate_text("title", &choice.title, MAX_TITLE_CHARS, false)?;
            validate_text(
                "rationale",
                &choice.rationale,
                MAX_RATIONALE_CHARS,
                true,
            )?;
            choice.action.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalActionsProposalParams {
    pub session_id: acp::schema::v1::SessionId,
    pub proposal: TerminalActionsProposal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalDisposition {
    Accepted,
    Stale,
    ContextUnavailable,
    TargetUnavailable,
    DelegateUnavailable,
    Duplicate,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalActionsProposalResponse {
    pub disposition: ProposalDisposition,
}

pub fn build_terminal_actions_proposal_request(
    params: &TerminalActionsProposalParams,
) -> acp::schema::v1::ExtRequest {
    let raw = serde_json::value::to_raw_value(params)
        .expect("TerminalActionsProposalParams serialization is infallible");
    acp::schema::v1::ExtRequest::new(
        INTELLTERM_METHOD_PROPOSE_TERMINAL_ACTIONS,
        Arc::from(raw),
    )
}

pub fn parse_terminal_actions_proposal_params(
    raw: &serde_json::value::RawValue,
) -> Result<TerminalActionsProposalParams, serde_json::Error> {
    serde_json::from_str(raw.get())
}

pub fn build_terminal_actions_proposal_response(
    response: &TerminalActionsProposalResponse,
) -> acp::schema::v1::ExtResponse {
    let raw = serde_json::value::to_raw_value(response)
        .expect("TerminalActionsProposalResponse serialization is infallible");
    acp::schema::v1::ExtResponse::new(Arc::from(raw))
}

pub fn parse_terminal_actions_proposal_response(
    raw: &serde_json::value::RawValue,
) -> Result<TerminalActionsProposalResponse, serde_json::Error> {
    serde_json::from_str(raw.get())
}

pub struct ProposeTerminalActions {
    routes: RouteRegistry,
}

impl ProposeTerminalActions {
    pub fn new(routes: RouteRegistry) -> Self {
        Self { routes }
    }
}

#[async_trait]
impl Tool for ProposeTerminalActions {
    fn name(&self) -> &'static str {
        TOOL_NAME
    }

    fn description(&self) -> &'static str {
        "Propose typed terminal actions for the current Intelligent Terminal turn. \
         Autofix accepts exactly one send_input choice; normal Terminal Agent turns \
         accept one to three send_input, open, or open_and_send choices. The helper \
         injects trusted pane and delegate routing and shows confirmation cards; this \
         tool never executes an action. Call it exactly once without assistant prose. \
         If no proposal is appropriate or the tool is unavailable, answer in Markdown."
    }

    fn input_schema(&self) -> serde_json::Value {
        let target = serde_json::json!({
            "type": "string",
            "enum": ["tab", "panel"]
        });
        let direction = serde_json::json!({
            "type": "string",
            "enum": ["right", "left", "up", "down", "auto"]
        });
        let common_open_properties = serde_json::json!({
            "target": target,
            "cwd": {
                "type": "string",
                "minLength": 1,
                "maxLength": MAX_CWD_CHARS,
                "pattern": "^[^\\r\\n]*$"
            },
            "title": {
                "type": "string",
                "minLength": 1,
                "maxLength": MAX_TITLE_CHARS,
                "pattern": "^[^\\r\\n]*$"
            },
            "direction": direction,
            "profile": {
                "type": "string",
                "minLength": 1,
                "maxLength": MAX_PROFILE_CHARS,
                "pattern": "^[^\\r\\n]*$"
            }
        });
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "recommended_choice": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_CHOICES
                },
                "choices": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_CHOICES,
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "title": {
                                "type": "string",
                                "minLength": 1,
                                "maxLength": MAX_TITLE_CHARS
                            },
                            "rationale": {
                                "type": "string",
                                "maxLength": MAX_RATIONALE_CHARS
                            },
                            "action": {
                                "oneOf": [
                                    {
                                        "type": "object",
                                        "additionalProperties": false,
                                        "properties": {
                                            "type": { "const": "send_input" },
                                            "input": {
                                                "type": "string",
                                                "minLength": 1,
                                                "maxLength": MAX_INPUT_CHARS,
                                                "pattern": "^[^\\r\\n]*$"
                                            },
                                            "preferred_action": {
                                                "type": "string",
                                                "enum": ["insert", "execute"],
                                                "description": "Optional initially focused choice; never automatic."
                                            }
                                        },
                                        "required": ["type", "input"]
                                    },
                                    {
                                        "type": "object",
                                        "additionalProperties": false,
                                        "properties": common_open_properties.as_object().unwrap().iter()
                                            .map(|(key, value)| (key.clone(), value.clone()))
                                            .chain(std::iter::once(("type".to_string(), serde_json::json!({ "const": "open" }))))
                                            .collect::<serde_json::Map<_, _>>(),
                                        "required": ["type", "target"]
                                    },
                                    {
                                        "type": "object",
                                        "additionalProperties": false,
                                        "properties": common_open_properties.as_object().unwrap().iter()
                                            .map(|(key, value)| (key.clone(), value.clone()))
                                            .chain([
                                                ("type".to_string(), serde_json::json!({ "const": "open_and_send" })),
                                                ("destination".to_string(), serde_json::json!({
                                                    "type": "string",
                                                    "enum": ["shell", "delegate"]
                                                })),
                                                ("input".to_string(), serde_json::json!({
                                                    "type": "string",
                                                    "minLength": 1,
                                                    "maxLength": MAX_INPUT_CHARS
                                                }))
                                            ])
                                            .collect::<serde_json::Map<_, _>>(),
                                        "required": ["type", "target", "destination", "input"]
                                    }
                                ]
                            }
                        },
                        "required": ["title", "action"]
                    }
                }
            },
            "required": ["choices"]
        })
    }

    async fn call(
        &self,
        context: &ToolContext<'_>,
        args: &serde_json::Value,
    ) -> Result<String, String> {
        let route_id = context
            .route_id
            .ok_or("propose_terminal_actions requires a session-bound MCP route")?;
        let proposal: TerminalActionsProposal =
            serde_json::from_value(args.clone()).map_err(|err| format!("invalid arguments: {err}"))?;
        proposal.validate()?;

        let route = self.routes.route(route_id).await?;
        let params = TerminalActionsProposalParams {
            session_id: route.session_id.expect("route() requires a bound session"),
            proposal,
        };
        let request = build_terminal_actions_proposal_request(&params);
        let response = route
            .forwarder
            .expect("route() requires a connected helper")
            .ext_method(request)
            .await
            .map_err(|err| format!("helper rejected terminal action proposal request: {err}"))?;
        let response = parse_terminal_actions_proposal_response(&response.0)
            .map_err(|err| format!("invalid helper proposal response: {err}"))?;

        match response.disposition {
            ProposalDisposition::Accepted => {
                Ok("Proposal surfaced for user review. Do not add assistant text.".to_string())
            }
            ProposalDisposition::Stale => {
                Err("The Terminal Agent turn has ended; do not retry the tool.".to_string())
            }
            ProposalDisposition::ContextUnavailable => {
                Err("Trusted prompt context is not ready; explain in Markdown instead.".to_string())
            }
            ProposalDisposition::TargetUnavailable => Err(
                "The proposed action requires an active working pane, but none is available; \
                 explain in Markdown or propose a tab action instead."
                    .to_string(),
            ),
            ProposalDisposition::DelegateUnavailable => {
                Err("No delegate agent is configured; explain in Markdown instead.".to_string())
            }
            ProposalDisposition::Duplicate => {
                Err("A terminal action proposal was already accepted for this turn.".to_string())
            }
            ProposalDisposition::Invalid => {
                Err("The helper rejected this action shape for the current turn; explain in Markdown.".to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn send_proposal() -> TerminalActionsProposal {
        TerminalActionsProposal {
            recommended_choice: Some(1),
            choices: vec![ProposedTerminalChoice {
                title: "Run tests".to_string(),
                rationale: "Verify the change.".to_string(),
                action: ProposedTerminalAction::SendInput {
                    input: "cargo test".to_string(),
                    preferred_action: None,
                },
            }],
        }
    }

    #[test]
    fn proposal_rejects_model_routing_and_invalid_recommended_choice() {
        let model_routing = serde_json::from_value::<TerminalActionsProposal>(
            serde_json::json!({
                "choices": [{
                    "title": "Run tests",
                    "action": {
                        "type": "send_input",
                        "input": "cargo test",
                        "pane_id": "model-controlled"
                    }
                }]
            }),
        );
        assert!(model_routing.is_err());

        let mut invalid = send_proposal();
        invalid.recommended_choice = Some(2);
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn proposal_rejects_tab_direction_and_nul() {
        let tab_direction = TerminalActionsProposal {
            recommended_choice: None,
            choices: vec![ProposedTerminalChoice {
                title: "Open tab".to_string(),
                rationale: String::new(),
                action: ProposedTerminalAction::Open {
                    target: ProposedOpenTarget::Tab,
                    cwd: None,
                    title: None,
                    direction: Some(ProposedSplitDirection::Right),
                    profile: None,
                },
            }],
        };
        assert!(tab_direction.validate().is_err());

        let mut nul = send_proposal();
        nul.choices[0].title = "bad\0title".to_string();
        assert!(nul.validate().is_err());
    }

    #[test]
    fn proposal_rejects_multiline_send_input() {
        for line_break in ['\n', '\r'] {
            let mut proposal = send_proposal();
            let ProposedTerminalAction::SendInput {
                input: proposed_input,
                ..
            } = &mut proposal.choices[0].action
            else {
                unreachable!("send_proposal must contain send_input");
            };
            *proposed_input = format!("echo first{line_break}echo second");
            assert_eq!(
                proposal.validate(),
                Err("input must be a single line".to_string())
            );
        }
    }

    #[test]
    fn proposal_rejects_multiline_open_fields() {
        let line_break = "\n";
        for (name, cwd, title, profile) in [
            ("cwd", Some(format!("C:\\repo{line_break}next")), None, None),
            ("title", None, Some(format!("Build{line_break}output")), None),
            (
                "profile",
                None,
                None,
                Some(format!("PowerShell{line_break}Admin")),
            ),
        ] {
            assert_eq!(
                validate_open_fields(
                    ProposedOpenTarget::Tab,
                    &cwd,
                    &title,
                    None,
                    &profile
                ),
                Err(format!("{name} must be a single line"))
            );
        }
    }

    #[test]
    fn proposal_wire_round_trips() {
        let params = TerminalActionsProposalParams {
            session_id: acp::schema::v1::SessionId::new("session-a"),
            proposal: send_proposal(),
        };
        let request = build_terminal_actions_proposal_request(&params);
        assert_eq!(
            request.method.as_ref(),
            INTELLTERM_METHOD_PROPOSE_TERMINAL_ACTIONS
        );
        assert_eq!(
            parse_terminal_actions_proposal_params(&request.params).unwrap(),
            params
        );
    }
}
