//! Lightweight ACP model-list probe.
//!
//! Spawned by the Settings UI when the user picks a new ACP agent so
//! the model dropdown can populate before any agent pane rebuild.
//! Does the minimum work — `initialize` + `new_session` — reads the
//! agent-advertised model list off the `NewSessionResponse`, prints
//! it as a single JSON object to stdout, then drops the child.
//!
//! Output shape (stdout, one JSON object — caller reads the whole
//! stream and `serde_json::from_str`s it):
//!
//! ```json
//! { "available_models": [{"id":"...","name":"...","description":"..."}],
//!   "current_model_id": "..." }
//! ```
//!
//! On error: non-zero exit, message on stderr, no JSON on stdout.

use acp::Agent as _;
use agent_client_protocol as acp;
use anyhow::{anyhow, Result};
use serde::Serialize;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::app::AcpModelInfo;
use crate::protocol::acp::spawn::spawn_agent_process;

#[derive(Serialize)]
pub struct ProbeResult {
    pub available_models: Vec<AcpModelInfo>,
    pub current_model_id: Option<String>,
}

/// Stub `acp::Client`. We only drive `initialize` + `new_session`,
/// which don't trigger server→client calls — every method here is a
/// fail-fast safety net rather than a real implementation.
struct ProbeClient;

#[async_trait::async_trait(?Send)]
impl acp::Client for ProbeClient {
    async fn request_permission(
        &self,
        _: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        Err(acp::Error::internal_error().data("probe-models does not handle permissions".to_string()))
    }

    async fn session_notification(&self, _: acp::SessionNotification) -> acp::Result<()> {
        Ok(())
    }

    async fn create_terminal(
        &self,
        _: acp::CreateTerminalRequest,
    ) -> acp::Result<acp::CreateTerminalResponse> {
        Err(acp::Error::internal_error().data("probe-models does not create terminals".to_string()))
    }

    async fn terminal_output(
        &self,
        _: acp::TerminalOutputRequest,
    ) -> acp::Result<acp::TerminalOutputResponse> {
        Err(acp::Error::internal_error().data("probe-models does not run terminals".to_string()))
    }

    async fn wait_for_terminal_exit(
        &self,
        _: acp::WaitForTerminalExitRequest,
    ) -> acp::Result<acp::WaitForTerminalExitResponse> {
        Err(acp::Error::internal_error().data("probe-models does not run terminals".to_string()))
    }

    async fn release_terminal(
        &self,
        _: acp::ReleaseTerminalRequest,
    ) -> acp::Result<acp::ReleaseTerminalResponse> {
        Err(acp::Error::internal_error().data("probe-models does not run terminals".to_string()))
    }

    async fn kill_terminal(
        &self,
        _: acp::KillTerminalRequest,
    ) -> acp::Result<acp::KillTerminalResponse> {
        Err(acp::Error::internal_error().data("probe-models does not run terminals".to_string()))
    }
}

/// `agent_cmd` is the full cmdline as passed to `--agent` in the agent
/// pane (e.g. `"copilot --acp --stdio"`,
/// `"npx -y @zed-industries/claude-code-acp"`).
pub async fn probe_models(agent_cmd: &str) -> Result<ProbeResult> {
    let mut spawned = spawn_agent_process(agent_cmd, None)?;
    tracing::debug!(
        "probe spawned: program={} is_npx={} pid={:?}",
        spawned.resolved_program,
        spawned.is_npx,
        spawned.child.id()
    );

    let outgoing = spawned.child.stdin.take().expect("stdin piped").compat_write();
    let incoming = spawned.child.stdout.take().expect("stdout piped").compat();
    if let Some(stderr) = spawned.child.stderr.take() {
        // Drain stderr so npx startup banners don't fill the pipe and
        // block the adapter.
        tokio::task::spawn_local(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!("agent stderr: {}", line);
            }
        });
    }

    let (conn, handle_io) =
        acp::ClientSideConnection::new(ProbeClient, outgoing, incoming, |fut| {
            tokio::task::spawn_local(fut);
        });

    tokio::task::spawn_local(async move {
        if let Err(e) = handle_io.await {
            tracing::warn!("probe handle_io failed: {:#}", e);
        }
    });

    // Tighter than the full client's timeouts: the probe is
    // user-blocking, so we'd rather fail fast and let the user retry
    // than make them stare at the dropdown placeholder for 60s+.
    // Cached adapters complete in <2s.
    let init_timeout_secs: u64 = if spawned.is_npx { 25 } else { 10 };

    let init_req = acp::InitializeRequest::new(acp::ProtocolVersion::V1)
        .client_capabilities(acp::ClientCapabilities::new().terminal(true))
        .client_info(
            acp::Implementation::new("wta-probe", env!("CARGO_PKG_VERSION"))
                .title("WTA Model Probe"),
        );
    let _init_resp = tokio::time::timeout(
        Duration::from_secs(init_timeout_secs),
        conn.initialize(init_req),
    )
    .await
    .map_err(|_| {
        anyhow!(
            "ACP initialize timed out after {}s during probe (agent={})",
            init_timeout_secs,
            spawned.label()
        )
    })?
    .map_err(|e| anyhow!("initialize failed: {}", e))?;

    let cwd = std::env::current_dir().unwrap_or_default();
    let session_resp = tokio::time::timeout(
        Duration::from_secs(10),
        conn.new_session(acp::NewSessionRequest::new(cwd)),
    )
    .await
    .map_err(|_| anyhow!("new_session timed out after 10s during probe"))?
    .map_err(|e| anyhow!("new_session failed: {}", e))?;

    let (available_models, current_model_id) = match &session_resp.models {
        Some(state) => {
            let models: Vec<AcpModelInfo> = state
                .available_models
                .iter()
                .map(|m| AcpModelInfo {
                    id: m.model_id.0.to_string(),
                    name: m.name.clone(),
                    description: m.description.clone(),
                })
                .collect();
            (models, Some(state.current_model_id.0.to_string()))
        }
        None => (Vec::new(), None),
    };

    drop(spawned.child);

    Ok(ProbeResult {
        available_models,
        current_model_id,
    })
}
