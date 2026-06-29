//! Minimal MCP (Model Context Protocol) tool server for WTA, served over a
//! shared localhost Streamable-HTTP endpoint by `wta-master`.
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

pub use server::{serve, McpEndpoint};

use async_trait::async_trait;
use std::sync::Arc;

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
    async fn call(&self, args: &serde_json::Value) -> Result<String, String>;
}

/// Ordered tool registry. Add a tool here and it appears in `tools/list` and is
/// dispatchable by `tools/call` — no other wiring needed.
pub fn default_registry() -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(resolve_command::ResolveCommand)]
}

/// File where master publishes the live MCP endpoint URL for helpers to read.
fn endpoint_file_path() -> Option<std::path::PathBuf> {
    crate::runtime_paths::intelligent_terminal_root().map(|d| d.join("mcp-endpoint.txt"))
}

/// Start the shared MCP server and publish its URL for helpers. Best-effort:
/// returns `None` if binding fails (callers just don't offer MCP).
pub async fn start_and_publish() -> Option<McpEndpoint> {
    let ep = serve(default_registry()).await?;
    if let Some(p) = endpoint_file_path() {
        let _ = std::fs::create_dir_all(p.parent()?);
        let _ = std::fs::write(&p, &ep.url);
    }
    Some(ep)
}

/// Read the published MCP endpoint URL (helper side). `None` when master hasn't
/// started one (MCP unavailable → degrade to in-process autofix only).
pub fn published_url() -> Option<String> {
    let p = endpoint_file_path()?;
    std::fs::read_to_string(p).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}
