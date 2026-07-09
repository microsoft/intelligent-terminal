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

use agent_client_protocol as acp;
use anyhow::{anyhow, Result};
use serde::Serialize;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::app::AcpModelInfo;
use crate::protocol::acp::conn;
use crate::protocol::acp::spawn::spawn_agent_process;

#[derive(Serialize)]
pub struct ProbeResult {
    pub available_models: Vec<AcpModelInfo>,
    pub current_model_id: Option<String>,
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
        conn::spawn_client(acp::Client.builder().name("wta-probe"), conn::byte_streams(outgoing, incoming));

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

    let init_req = acp::schema::v1::InitializeRequest::new(acp::schema::ProtocolVersion::V1)
        .client_capabilities(acp::schema::v1::ClientCapabilities::new().terminal(true))
        .client_info(
            acp::schema::v1::Implementation::new("wta-probe", env!("CARGO_PKG_VERSION"))
                .title("WTA Model Probe"),
        );
    let init_started = std::time::Instant::now();
    let init_result =
        tokio::time::timeout(Duration::from_secs(init_timeout_secs), conn.initialize(init_req))
            .await;
    crate::telemetry::log_acp_initialize_complete(
        init_started.elapsed().as_secs_f64() * 1000.0,
        matches!(init_result, Ok(Ok(_))),
        "Probe",
        match &init_result {
            Ok(Ok(_)) => "",
            Ok(Err(_)) => "AcpError",
            Err(_) => "Timeout",
        },
        match &init_result {
            Ok(Err(e)) => e.code.into(),
            _ => 0,
        },
    );
    let _init_resp = init_result
        .map_err(|_| {
            anyhow!(
                "ACP initialize timed out after {}s during probe (agent={})",
                init_timeout_secs,
                spawned.label()
            )
        })?
        .map_err(|e| anyhow!("initialize failed: {}", e))?;

    let cwd = std::env::current_dir().unwrap_or_default();
    let session_started = std::time::Instant::now();
    let session_result = tokio::time::timeout(
        Duration::from_secs(10),
        conn.new_session(acp::schema::v1::NewSessionRequest::new(cwd)),
    )
    .await;
    let session_id = session_result
        .as_ref()
        .ok()
        .and_then(|inner| inner.as_ref().ok())
        .map(|resp| resp.session_id.to_string());
    crate::telemetry::log_acp_new_session_complete(
        session_id.as_deref(),
        session_started.elapsed().as_secs_f64() * 1000.0,
        matches!(session_result, Ok(Ok(_))),
        "Probe",
        match &session_result {
            Ok(Ok(_)) => "",
            Ok(Err(_)) => "AcpError",
            Err(_) => "Timeout",
        },
        match &session_result {
            Ok(Err(e)) => e.code.into(),
            _ => 0,
        },
    );
    let session_resp = session_result
        .map_err(|_| anyhow!("new_session timed out after 10s during probe"))?
        .map_err(|e| anyhow!("new_session failed: {}", e))?;

    let (available_models, current_model_id) =
        crate::protocol::acp::model_select::models_from_new_session(&session_resp);

    drop(spawned.child);

    Ok(ProbeResult {
        available_models,
        current_model_id,
    })
}

/// One row from an agent CLI's `session/list` (ACP `list_sessions`) response.
#[derive(Serialize)]
pub struct ProbedSession {
    pub session_id: String,
    /// Debug-formatted to avoid coupling to the wire `cwd` type.
    pub cwd: String,
    pub title: Option<String>,
    pub updated_at: Option<String>,
}

/// Outcome of probing an agent CLI's ACP `session/list` capability.
///
/// Answers the design question "can ACP `session/list` replace reading
/// on-disk transcripts?": it records whether the agent answers the call
/// at all and, if so, what it returns — so live-only can be told apart
/// from full on-disk history.
#[derive(Serialize)]
pub struct SessionProbeResult {
    /// `{:#?}` dump of the `initialize` response, including the agent's
    /// advertised capabilities — reveals whether the agent claims a
    /// session-list capability in the first place.
    pub initialize_dump: String,
    /// `true` when `list_sessions` returned `Ok`.
    pub list_sessions_ok: bool,
    /// ACP error string when `list_sessions` failed (e.g. `method not
    /// found` for an agent that doesn't implement the unstable method).
    pub list_sessions_error: Option<String>,
    /// Sessions the agent reported when the call succeeded.
    pub sessions: Vec<ProbedSession>,
}

/// Spawn `agent_cmd`, run ACP `initialize`, then call `list_sessions`
/// and capture the outcome. Mirrors [`probe_models`]'s spawn/initialize
/// preamble (kept inline rather than shared so the probe stays a
/// self-contained diagnostic).
pub async fn probe_sessions(agent_cmd: &str) -> Result<SessionProbeResult> {
    let mut spawned = spawn_agent_process(agent_cmd, None)?;
    tracing::debug!(
        "session probe spawned: program={} is_npx={} pid={:?}",
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
        conn::spawn_client(acp::Client.builder().name("wta-probe-sessions"), conn::byte_streams(outgoing, incoming));
    tokio::task::spawn_local(async move {
        if let Err(e) = handle_io.await {
            tracing::warn!("session probe handle_io failed: {:#}", e);
        }
    });

    let init_timeout_secs: u64 = if spawned.is_npx { 25 } else { 10 };
    let init_req = acp::schema::v1::InitializeRequest::new(acp::schema::ProtocolVersion::V1)
        .client_capabilities(acp::schema::v1::ClientCapabilities::new().terminal(true))
        .client_info(
            acp::schema::v1::Implementation::new("wta-probe-sessions", env!("CARGO_PKG_VERSION"))
                .title("WTA Session Probe"),
        );
    let init_resp = tokio::time::timeout(
        Duration::from_secs(init_timeout_secs),
        conn.initialize(init_req),
    )
    .await
    .map_err(|_| {
        anyhow!(
            "ACP initialize timed out after {}s during session probe (agent={})",
            init_timeout_secs,
            spawned.label()
        )
    })?
    .map_err(|e| anyhow!("initialize failed: {}", e))?;

    let initialize_dump = format!("{init_resp:#?}");

    let list_result = tokio::time::timeout(
        Duration::from_secs(10),
        conn.list_sessions(acp::schema::v1::ListSessionsRequest::new()),
    )
    .await;

    let (list_sessions_ok, list_sessions_error, sessions) = match list_result {
        Err(_) => (
            false,
            Some("list_sessions timed out after 10s".to_string()),
            Vec::new(),
        ),
        Ok(Err(e)) => (false, Some(format!("{e}")), Vec::new()),
        Ok(Ok(resp)) => {
            let sessions = resp
                .sessions
                .iter()
                .map(|s| ProbedSession {
                    session_id: s.session_id.to_string(),
                    cwd: format!("{:?}", s.cwd),
                    title: s.title.clone(),
                    updated_at: s.updated_at.clone(),
                })
                .collect();
            (true, None, sessions)
        }
    };

    drop(spawned.child);

    Ok(SessionProbeResult {
        initialize_dump,
        list_sessions_ok,
        list_sessions_error,
        sessions,
    })
}
