//! Lightweight ACP model-list probe.
//!
//! Used by both the Settings `probe-models` command and wta-master's clean
//! cloud discovery so both paths share the same handshake and retry policy.
//! Does the minimum work — `initialize` + `new_session` — reads the
//! agent-advertised model list off the `NewSessionResponse`, prints
//! it as a single JSON object to stdout, then drops the child. The
//! child runs with provider overrides scrubbed so this catalog remains
//! the agent's native cloud catalog rather than the selected shared provider.
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
use std::future::Future;
use std::time::Duration;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::app_contracts::AcpModelInfo;
use crate::protocol::acp::conn;
use crate::protocol::acp::spawn::{spawn_agent_process, AgentStderrLog, ChildEnvironmentPolicy};

const MODEL_PROBE_ENVIRONMENT_POLICY: ChildEnvironmentPolicy =
    ChildEnvironmentPolicy::CleanCloudDiscovery;
const MODEL_PROBE_MAX_ATTEMPTS: usize = 3;
const MODEL_PROBE_RETRY_DELAY: Duration = Duration::from_millis(500);

#[derive(Serialize)]
pub struct ProbeResult {
    pub available_models: Vec<AcpModelInfo>,
    pub current_model_id: Option<String>,
}

/// `agent_cmd` is the full cmdline as passed to `--agent` in the agent
/// pane (e.g. `"copilot --acp --stdio"`,
/// `"npx -y @zed-industries/claude-code-acp"`).
pub async fn probe_models(agent_cmd: &str) -> Result<ProbeResult> {
    retry_model_probe(
        || probe_models_impl(agent_cmd, MODEL_PROBE_ENVIRONMENT_POLICY),
        MODEL_PROBE_RETRY_DELAY,
    )
    .await
}

async fn retry_model_probe<F, Fut>(mut probe: F, retry_delay: Duration) -> Result<ProbeResult>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<ProbeResult>>,
{
    for attempt in 1..=MODEL_PROBE_MAX_ATTEMPTS {
        match probe().await {
            Ok(result)
                if !result.available_models.is_empty() || attempt == MODEL_PROBE_MAX_ATTEMPTS =>
            {
                return Ok(result);
            }
            Ok(_) => {
                tracing::warn!(
                    attempt,
                    max_attempts = MODEL_PROBE_MAX_ATTEMPTS,
                    "model probe returned an empty catalog; retrying"
                );
            }
            Err(error) if attempt == MODEL_PROBE_MAX_ATTEMPTS => return Err(error),
            Err(error) => {
                tracing::warn!(
                    attempt,
                    max_attempts = MODEL_PROBE_MAX_ATTEMPTS,
                    %error,
                    "model probe failed; retrying"
                );
            }
        }
        tokio::time::sleep(retry_delay).await;
    }
    unreachable!("the bounded model probe loop always returns on its final attempt")
}

async fn probe_models_impl(
    agent_cmd: &str,
    environment_policy: ChildEnvironmentPolicy,
) -> Result<ProbeResult> {
    let mut spawned = spawn_agent_process(agent_cmd, None, None, environment_policy)?;
    tracing::debug!(
        "probe spawned: program={} is_npx={} pid={:?}",
        spawned.resolved_program,
        spawned.is_npx,
        spawned.child.id()
    );

    let outgoing = spawned
        .child
        .stdin
        .take()
        .expect("stdin piped")
        .compat_write();
    let incoming = spawned.child.stdout.take().expect("stdout piped").compat();
    let stderr_log = AgentStderrLog::new(spawned.label().to_string());
    let stderr_task = spawned
        .child
        .stderr
        .take()
        .map(|stderr| stderr_log.drain(stderr));

    let (conn, handle_io) = conn::spawn_client(
        acp::Client.builder().name("wta-probe"),
        conn::byte_streams(outgoing, incoming),
    );

    tokio::task::spawn_local(async move {
        if let Err(e) = handle_io.await {
            tracing::warn!("probe handle_io failed: {:#}", e);
        }
    });

    // Tighter than the full client's timeouts so a failed attempt can retry
    // promptly. Cached adapters complete in <2s.
    let init_timeout_secs: u64 = if spawned.is_npx { 25 } else { 10 };

    let init_req = acp::schema::v1::InitializeRequest::new(acp::schema::ProtocolVersion::V1)
        .client_capabilities(acp::schema::v1::ClientCapabilities::new().terminal(true))
        .client_info(
            acp::schema::v1::Implementation::new("wta-probe", env!("CARGO_PKG_VERSION"))
                .title("WTA Model Probe"),
        );
    let init_started = std::time::Instant::now();
    let init_result = tokio::time::timeout(
        Duration::from_secs(init_timeout_secs),
        conn.initialize(init_req),
    )
    .await;
    if matches!(init_result, Ok(Ok(_))) {
        stderr_log.mark_initialized();
    } else {
        stderr_log
            .finish_failed_startup(&mut spawned.child, stderr_task)
            .await;
    }
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
    // Native CLIs may fetch their cloud model catalog during session/new.
    // Copilot can legitimately take longer than 10 seconds under load, while
    // adapter probes already receive a larger initialize budget and must keep
    // the full three-attempt probe within the Settings caller's 120s ceiling.
    let session_timeout_secs = if spawned.is_npx { 10 } else { 20 };
    let session_result = tokio::time::timeout(
        Duration::from_secs(session_timeout_secs),
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
        .map_err(|_| {
            anyhow!(
                "new_session timed out after {}s during probe",
                session_timeout_secs
            )
        })?
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
    let mut spawned = spawn_agent_process(
        agent_cmd,
        None,
        None,
        ChildEnvironmentPolicy::ApplySharedProvider,
    )?;
    tracing::debug!(
        "session probe spawned: program={} is_npx={} pid={:?}",
        spawned.resolved_program,
        spawned.is_npx,
        spawned.child.id()
    );

    let outgoing = spawned
        .child
        .stdin
        .take()
        .expect("stdin piped")
        .compat_write();
    let incoming = spawned.child.stdout.take().expect("stdout piped").compat();
    let stderr_log = AgentStderrLog::new(spawned.label().to_string());
    let stderr_task = spawned
        .child
        .stderr
        .take()
        .map(|stderr| stderr_log.drain(stderr));

    let (conn, handle_io) = conn::spawn_client(
        acp::Client.builder().name("wta-probe-sessions"),
        conn::byte_streams(outgoing, incoming),
    );
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
    let init_result = tokio::time::timeout(
        Duration::from_secs(init_timeout_secs),
        conn.initialize(init_req),
    )
    .await;
    if matches!(init_result, Ok(Ok(_))) {
        stderr_log.mark_initialized();
    } else {
        stderr_log
            .finish_failed_startup(&mut spawned.child, stderr_task)
            .await;
    }
    let init_resp = init_result
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn model_catalog_probe_uses_clean_cloud_environment() {
        assert_eq!(
            MODEL_PROBE_ENVIRONMENT_POLICY,
            ChildEnvironmentPolicy::CleanCloudDiscovery
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn model_probe_retries_failures_until_success() {
        let attempts = Rc::new(Cell::new(0));
        let attempts_for_probe = Rc::clone(&attempts);

        let result = retry_model_probe(
            move || {
                let attempt = attempts_for_probe.get() + 1;
                attempts_for_probe.set(attempt);
                async move {
                    if attempt < MODEL_PROBE_MAX_ATTEMPTS {
                        Err(anyhow!("transient failure"))
                    } else {
                        Ok(ProbeResult {
                            available_models: vec![AcpModelInfo {
                                id: "cloud-model".into(),
                                name: "Cloud Model".into(),
                                description: None,
                            }],
                            current_model_id: None,
                        })
                    }
                }
            },
            Duration::ZERO,
        )
        .await
        .expect("the final probe attempt should succeed");

        assert_eq!(attempts.get(), MODEL_PROBE_MAX_ATTEMPTS);
        assert_eq!(result.available_models[0].id, "cloud-model");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn model_probe_retries_empty_catalogs_without_changing_result_semantics() {
        let attempts = Rc::new(Cell::new(0));
        let attempts_for_probe = Rc::clone(&attempts);

        let result = retry_model_probe(
            move || {
                attempts_for_probe.set(attempts_for_probe.get() + 1);
                async {
                    Ok(ProbeResult {
                        available_models: Vec::new(),
                        current_model_id: None,
                    })
                }
            },
            Duration::ZERO,
        )
        .await
        .expect("an empty catalog remains a successful probe result");

        assert_eq!(attempts.get(), MODEL_PROBE_MAX_ATTEMPTS);
        assert!(result.available_models.is_empty());
    }
}
