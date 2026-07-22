//! Minimal MCP (Model Context Protocol) tool server for WTA, served over
//! localhost Streamable HTTP by `wta-master`.
//!
//! Why hand-rolled (no axum/hyper/rmcp): the agent only needs a single
//! request/response JSON-RPC endpoint, so a tokio-only HTTP/1.1 handler keeps
//! the dependency graph (and `+crt-static` build) clean — no third-party-notice
//! churn. Responses are single `application/json` objects, which the MCP
//! Streamable-HTTP transport explicitly allows (no SSE needed for stateless
//! tools).
//!
//! Extensibility: add a tool by implementing [`Tool`] and registering it in
//! [`default_registry`]. The dispatch (`initialize` / `tools/list` /
//! `tools/call`) is tool-agnostic.

mod resolve_command;
mod server;
pub(crate) mod terminal_actions;

pub const SERVER_NAME: &str = "wta";
pub const CODEX_PROPOSAL_TOOL_CALL_TITLE: &str = "mcp.wta.propose_terminal_actions";

pub fn is_proposal_tool_call_title(title: &str) -> bool {
    matches!(
        title,
        terminal_actions::ACP_TOOL_CALL_TITLE | CODEX_PROPOSAL_TOOL_CALL_TITLE
    )
}

use server::serve;
pub use terminal_actions::{
    build_terminal_actions_proposal_response, parse_terminal_actions_proposal_params,
    PreferredInputAction, ProposalDisposition, ProposedDestination, ProposedOpenTarget,
    ProposedTerminalAction, TerminalActionsProposal, TerminalActionsProposalResponse,
    INTELLTERM_METHOD_PROPOSE_TERMINAL_ACTIONS,
};

use async_trait::async_trait;
use std::{collections::HashMap, sync::Arc};

use agent_client_protocol as acp;
use tokio::sync::Mutex;

use crate::{master::HelperId, protocol::acp::conn};

/// Per-request transport context derived from the MCP URL. The route id is
/// generated and injected by master; it is never model-authored tool input.
pub struct ToolContext<'a> {
    pub route_id: Option<&'a str>,
}

/// A single MCP tool. Stateless and `Send + Sync` so one registry instance can
/// serve concurrent requests from every agent session over the shared endpoint.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique tool name the agent calls (`tools/call` `name`).
    fn name(&self) -> &'static str;
    /// One-line human/LLM-facing description.
    fn description(&self) -> &'static str;
    /// JSON Schema for the tool's arguments (object schema).
    fn input_schema(&self) -> serde_json::Value;
    /// Execute. `args` is the `arguments` object; return the result payload to
    /// embed as text content, or an `Err` message surfaced as an error result.
    async fn call(&self, context: &ToolContext<'_>, args: &serde_json::Value) -> Result<String, String>;
}

/// Ordered tool registry. Add a tool here and it appears in `tools/list` and is
/// dispatchable by `tools/call` — no other wiring needed.
pub fn default_registry(routes: RouteRegistry) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(resolve_command::ResolveCommand),
        Arc::new(terminal_actions::ProposeTerminalActions::new(routes)),
    ]
}

#[derive(Clone)]
struct ProposalRoute {
    helper_id: HelperId,
    forwarder: Option<conn::AgentLink>,
    session_id: Option<acp::schema::v1::SessionId>,
}

/// Transport-only correlation from an opaque MCP URL route to the existing
/// helper connection. Turn state remains helper-owned.
#[derive(Clone, Default)]
pub struct RouteRegistry {
    routes: Arc<Mutex<HashMap<String, ProposalRoute>>>,
}

pub(crate) enum LoadSessionRoute {
    Registered(String),
    Rebound(String),
    Active,
    Missing,
}

impl RouteRegistry {
    pub async fn register(&self, helper_id: HelperId, forwarder: conn::AgentLink) -> String {
        let route_id = uuid::Uuid::new_v4().simple().to_string();
        self.routes.lock().await.insert(
            route_id.clone(),
            ProposalRoute {
                helper_id,
                forwarder: Some(forwarder),
                session_id: None,
            },
        );
        route_id
    }

    pub async fn bind_session(&self, route_id: &str, session_id: acp::schema::v1::SessionId) {
        if let Some(route) = self.routes.lock().await.get_mut(route_id) {
            route.session_id = Some(session_id);
        }
    }

    /// Atomically claim the route for `session/load`. An active route belongs
    /// to another helper and must not be stolen; an inactive route is an orphan
    /// we can rebind. Historical sessions register a fresh route for injection.
    pub(crate) async fn bind_load_session(
        &self,
        session_id: &acp::schema::v1::SessionId,
        helper_id: HelperId,
        forwarder: conn::AgentLink,
        register_if_missing: bool,
    ) -> LoadSessionRoute {
        let mut routes = self.routes.lock().await;
        if let Some((route_id, route)) = routes
            .iter_mut()
            .find(|(_, route)| route.session_id.as_ref() == Some(session_id))
        {
            if route.forwarder.is_some() {
                return LoadSessionRoute::Active;
            }
            route.helper_id = helper_id;
            route.forwarder = Some(forwarder);
            return LoadSessionRoute::Rebound(route_id.clone());
        }
        if !register_if_missing {
            return LoadSessionRoute::Missing;
        }
        let route_id = uuid::Uuid::new_v4().simple().to_string();
        routes.insert(
            route_id.clone(),
            ProposalRoute {
                helper_id,
                forwarder: Some(forwarder),
                session_id: Some(session_id.clone()),
            },
        );
        LoadSessionRoute::Registered(route_id)
    }

    pub async fn deactivate(&self, route_id: &str) {
        if let Some(route) = self.routes.lock().await.get_mut(route_id) {
            route.forwarder = None;
        }
    }

    pub async fn remove(&self, route_id: &str) {
        self.routes.lock().await.remove(route_id);
    }

    pub async fn remove_sessions(&self, session_ids: &[acp::schema::v1::SessionId]) {
        if session_ids.is_empty() {
            return;
        }
        self.routes.lock().await.retain(|_, route| {
            route.forwarder.is_some()
                || !route
                    .session_id
                    .as_ref()
                    .is_some_and(|session_id| session_ids.contains(session_id))
        });
    }

    /// Retain session correlation for same-master orphan rebind, but make the
    /// dead helper unreachable until a new helper reattaches it.
    pub async fn deactivate_helper(&self, helper_id: HelperId) {
        let mut routes = self.routes.lock().await;
        routes.retain(|_, route| {
            if route.helper_id == helper_id {
                route.forwarder = None;
            }
            route.helper_id != helper_id || route.session_id.is_some()
        });
    }

    async fn route(&self, route_id: &str) -> Result<ProposalRoute, String> {
        let route = self
            .routes
            .lock()
            .await
            .get(route_id)
            .cloned()
            .ok_or("unknown or expired MCP session route")?;
        if route.session_id.is_none() {
            return Err("MCP session route is not bound to an ACP session yet".to_string());
        }
        if route.forwarder.is_none() {
            return Err("MCP session route has no connected helper".to_string());
        }
        Ok(route)
    }
}

/// Running master-owned MCP host. Clones share one route registry.
#[derive(Clone)]
pub struct McpHost {
    endpoint: Arc<str>,
    pub routes: RouteRegistry,
}

impl McpHost {
    pub fn route_url(&self, route_id: &str) -> String {
        format!("{}/{}", self.endpoint, route_id)
    }
}

/// Start the MCP server. Failure is non-fatal; callers omit MCP servers from
/// ACP session creation and typed proposal flows degrade to Markdown.
pub async fn start() -> Option<McpHost> {
    let routes = RouteRegistry::default();
    let ep = serve(default_registry(routes.clone())).await?;
    Some(McpHost {
        endpoint: Arc::from(ep.url),
        routes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_url_appends_opaque_route() {
        let host = McpHost {
            endpoint: Arc::from("http://127.0.0.1:51234/mcp"),
            routes: RouteRegistry::default(),
        };
        assert_eq!(
            host.route_url("abc"),
            "http://127.0.0.1:51234/mcp/abc"
        );
    }
}
