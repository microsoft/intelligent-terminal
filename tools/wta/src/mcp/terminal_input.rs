use std::sync::Arc;

use agent_client_protocol as acp;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{RouteRegistry, Tool, ToolContext};

pub const INTELLTERM_METHOD_PROPOSE_TERMINAL_INPUT: &str =
    "_intellterm.wta/propose_terminal_input";

pub(super) const MAX_INPUT_CHARS: usize = 16 * 1024;
pub(super) const MAX_TITLE_CHARS: usize = 160;
pub(super) const MAX_RATIONALE_CHARS: usize = 2 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreferredTerminalInputAction {
    Insert,
    Execute,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalInputProposal {
    pub input: String,
    pub preferred_action: PreferredTerminalInputAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

impl TerminalInputProposal {
    fn validate(&self) -> Result<(), String> {
        validate_text("input", &self.input, MAX_INPUT_CHARS, false)?;
        if let Some(title) = &self.title {
            validate_text("title", title, MAX_TITLE_CHARS, true)?;
        }
        if let Some(rationale) = &self.rationale {
            validate_text("rationale", rationale, MAX_RATIONALE_CHARS, true)?;
        }
        Ok(())
    }
}

pub(super) fn validate_text(
    name: &str,
    text: &str,
    max_chars: usize,
    allow_blank: bool,
) -> Result<(), String> {
    if text.contains('\0') {
        return Err(format!("{name} must not contain NUL characters"));
    }
    if !allow_blank && text.trim().is_empty() {
        return Err(format!("{name} must not be empty"));
    }
    if text.chars().count() > max_chars {
        return Err(format!("{name} exceeds the {max_chars}-character limit"));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalInputProposalParams {
    pub session_id: acp::schema::v1::SessionId,
    pub proposal: TerminalInputProposal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalDisposition {
    Accepted,
    NotAutofix,
    Stale,
    TargetUnavailable,
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalInputProposalResponse {
    pub disposition: ProposalDisposition,
}

pub fn build_terminal_input_proposal_request(
    params: &TerminalInputProposalParams,
) -> acp::schema::v1::ExtRequest {
    let raw = serde_json::value::to_raw_value(params)
        .expect("TerminalInputProposalParams serialization is infallible");
    acp::schema::v1::ExtRequest::new(
        INTELLTERM_METHOD_PROPOSE_TERMINAL_INPUT,
        Arc::from(raw),
    )
}

pub fn parse_terminal_input_proposal_params(
    raw: &serde_json::value::RawValue,
) -> Result<TerminalInputProposalParams, serde_json::Error> {
    serde_json::from_str(raw.get())
}

pub fn build_terminal_input_proposal_response(
    response: &TerminalInputProposalResponse,
) -> acp::schema::v1::ExtResponse {
    let raw = serde_json::value::to_raw_value(response)
        .expect("TerminalInputProposalResponse serialization is infallible");
    acp::schema::v1::ExtResponse::new(Arc::from(raw))
}

pub fn parse_terminal_input_proposal_response(
    raw: &serde_json::value::RawValue,
) -> Result<TerminalInputProposalResponse, serde_json::Error> {
    serde_json::from_str(raw.get())
}

pub struct ProposeTerminalInput {
    routes: RouteRegistry,
}

impl ProposeTerminalInput {
    pub fn new(routes: RouteRegistry) -> Self {
        Self { routes }
    }
}

#[async_trait]
impl Tool for ProposeTerminalInput {
    fn name(&self) -> &'static str {
        "propose_terminal_input"
    }

    fn description(&self) -> &'static str {
        "Propose one deterministic command for the current Intelligent Terminal \
         Autofix turn. This surfaces Run and Insert choices to the user; it never \
         executes automatically. Use only for a safe, certain, single-command fix. \
         For ambiguous, destructive, privileged, install, authentication, or \
         multi-step cases, do not call this tool and explain in Markdown instead."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "input": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_INPUT_CHARS,
                    "description": "One command valid in the failing pane's current shell."
                },
                "preferred_action": {
                    "type": "string",
                    "enum": ["insert", "execute"],
                    "description": "Initially focused user choice; never automatic."
                },
                "title": {
                    "type": "string",
                    "maxLength": MAX_TITLE_CHARS
                },
                "rationale": {
                    "type": "string",
                    "maxLength": MAX_RATIONALE_CHARS
                }
            },
            "required": ["input", "preferred_action"]
        })
    }

    async fn call(
        &self,
        context: &ToolContext<'_>,
        args: &serde_json::Value,
    ) -> Result<String, String> {
        let route_id = context
            .route_id
            .ok_or("propose_terminal_input requires a session-bound MCP route")?;
        let proposal: TerminalInputProposal =
            serde_json::from_value(args.clone()).map_err(|err| format!("invalid arguments: {err}"))?;
        proposal.validate()?;

        let route = self.routes.route(route_id).await?;
        let params = TerminalInputProposalParams {
            session_id: route.session_id.expect("route() requires a bound session"),
            proposal,
        };
        let request = build_terminal_input_proposal_request(&params);
        let response = route
            .forwarder
            .expect("route() requires a connected helper")
            .ext_method(request)
            .await
            .map_err(|err| format!("helper rejected proposal request: {err}"))?;
        let response = parse_terminal_input_proposal_response(&response.0)
            .map_err(|err| format!("invalid helper proposal response: {err}"))?;

        match response.disposition {
            ProposalDisposition::Accepted => {
                Ok("Proposal surfaced for user review. Do not repeat the command in assistant text.".to_string())
            }
            ProposalDisposition::NotAutofix => {
                Err("This tool is only available during an Autofix turn; explain in Markdown instead.".to_string())
            }
            ProposalDisposition::Stale => {
                Err("The Autofix turn has ended; do not retry the tool.".to_string())
            }
            ProposalDisposition::TargetUnavailable => {
                Err("No target pane is available; explain in Markdown instead.".to_string())
            }
            ProposalDisposition::Duplicate => {
                Err("A terminal-input proposal was already accepted for this turn.".to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::acp::conn;
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    #[test]
    fn proposal_rejects_unknown_fields_and_nul() {
        let unknown = serde_json::from_value::<TerminalInputProposal>(serde_json::json!({
            "input": "cargo test",
            "preferred_action": "execute",
            "pane_id": "model-controlled"
        }));
        assert!(unknown.is_err());

        let proposal = TerminalInputProposal {
            input: "echo \0 bad".to_string(),
            preferred_action: PreferredTerminalInputAction::Insert,
            title: None,
            rationale: None,
        };
        assert!(proposal.validate().is_err());
    }

    #[test]
    fn proposal_wire_round_trips() {
        let params = TerminalInputProposalParams {
            session_id: acp::schema::v1::SessionId::new("session-a"),
            proposal: TerminalInputProposal {
                input: "cargo test".to_string(),
                preferred_action: PreferredTerminalInputAction::Execute,
                title: Some("Run tests".to_string()),
                rationale: None,
            },
        };
        let request = build_terminal_input_proposal_request(&params);
        assert_eq!(request.method.as_ref(), INTELLTERM_METHOD_PROPOSE_TERMINAL_INPUT);
        assert_eq!(
            parse_terminal_input_proposal_params(&request.params).unwrap(),
            params
        );
    }

    fn mock_helper_forwarder(
        observed: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> conn::AgentLink {
        let (master_pipe, helper_pipe) = tokio::io::duplex(16 * 1024);
        let (master_read, master_write) = tokio::io::split(master_pipe);
        let master_builder = acp::Agent.builder().name("mcp-route-test-master");
        let (forwarder, master_io) = conn::spawn_agent(
            master_builder,
            conn::byte_streams(master_write.compat_write(), master_read.compat()),
        );
        tokio::task::spawn_local(async move {
            let _ = master_io.await;
        });

        let (helper_read, helper_write) = tokio::io::split(helper_pipe);
        let helper_builder = acp::Client
            .builder()
            .name("mcp-route-test-helper")
            .on_receive_request(
                move |request: acp::schema::v1::AgentRequest, responder, _cx| {
                    let observed = observed.clone();
                    async move {
                        use acp::schema::v1::{
                            AgentRequest as Request, ClientResponse as Response,
                        };
                        match request {
                            Request::ExtMethodRequest(request)
                                if crate::session_registry::ext_method_matches(
                                    &request.method,
                                    INTELLTERM_METHOD_PROPOSE_TERMINAL_INPUT,
                                ) =>
                            {
                                let params =
                                    parse_terminal_input_proposal_params(&request.params).unwrap();
                                let _ = observed.send(params.session_id.0.to_string());
                                conn::respond_enum(
                                    responder,
                                    Ok(Response::ExtMethodResponse(
                                        build_terminal_input_proposal_response(
                                            &TerminalInputProposalResponse {
                                                disposition: ProposalDisposition::Accepted,
                                            },
                                        ),
                                    )),
                                )
                            }
                            _ => responder.respond_with_error(acp::Error::method_not_found()),
                        }
                    }
                },
                acp::on_receive_request!(),
            );
        let (_helper_link, helper_io) = conn::spawn_client(
            helper_builder,
            conn::byte_streams(helper_write.compat_write(), helper_read.compat()),
        );
        tokio::task::spawn_local(async move {
            let _ = helper_io.await;
        });
        forwarder
    }

    #[tokio::test(flavor = "current_thread")]
    async fn proposal_routes_only_to_the_bound_session_helper() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let routes = RouteRegistry::default();
                let tool = ProposeTerminalInput::new(routes.clone());
                let (observed_a_tx, mut observed_a_rx) = tokio::sync::mpsc::unbounded_channel();
                let (observed_b_tx, mut observed_b_rx) = tokio::sync::mpsc::unbounded_channel();
                let route_a = routes
                    .register(
                        crate::master::HelperId::for_test(1),
                        mock_helper_forwarder(observed_a_tx),
                    )
                    .await;
                routes
                    .bind_session(&route_a, acp::schema::v1::SessionId::new("session-a"))
                    .await;
                let route_b = routes
                    .register(
                        crate::master::HelperId::for_test(2),
                        mock_helper_forwarder(observed_b_tx),
                    )
                    .await;
                routes
                    .bind_session(&route_b, acp::schema::v1::SessionId::new("session-b"))
                    .await;
                assert!(matches!(
                    routes
                        .bind_load_session(
                            &acp::schema::v1::SessionId::new("session-b"),
                            crate::master::HelperId::for_test(3),
                            mock_helper_forwarder(tokio::sync::mpsc::unbounded_channel().0),
                            true,
                        )
                        .await,
                    crate::mcp::LoadSessionRoute::Active
                ));

                let result = tool
                    .call(
                        &ToolContext {
                            route_id: Some(&route_b),
                        },
                        &serde_json::json!({
                            "input": "dotnet test",
                            "preferred_action": "execute"
                        }),
                    )
                    .await;

                assert!(result.is_ok(), "tool call failed: {result:?}");
                assert_eq!(observed_b_rx.recv().await.as_deref(), Some("session-b"));
                assert!(observed_a_rx.try_recv().is_err());

                // Cleanup for a disconnected helper must not delete a route
                // that another helper rebound between deactivate and remove.
                routes
                    .deactivate_helper(crate::master::HelperId::for_test(2))
                    .await;
                let (observed_c_tx, mut observed_c_rx) =
                    tokio::sync::mpsc::unbounded_channel();
                assert_eq!(
                    match routes
                        .bind_load_session(
                            &acp::schema::v1::SessionId::new("session-b"),
                            crate::master::HelperId::for_test(3),
                            mock_helper_forwarder(observed_c_tx),
                            true,
                        )
                        .await
                    {
                        crate::mcp::LoadSessionRoute::Rebound(route_id) => Some(route_id),
                        _ => None,
                    }
                    .as_deref(),
                    Some(route_b.as_str())
                );
                routes
                    .remove_sessions(&[acp::schema::v1::SessionId::new("session-b")])
                    .await;
                let result = tool
                    .call(
                        &ToolContext {
                            route_id: Some(&route_b),
                        },
                        &serde_json::json!({
                            "input": "dotnet test",
                            "preferred_action": "execute"
                        }),
                    )
                    .await;
                assert!(result.is_ok(), "rebound route was removed: {result:?}");
                assert_eq!(observed_c_rx.recv().await.as_deref(), Some("session-b"));
            })
            .await;
    }
}
