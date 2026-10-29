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
use anyhow::{Context, Result};
use serde::Serialize;
use std::future::Future;
use std::time::Duration;

use crate::app_contracts::AcpModelInfo;
use crate::protocol::acp::spawn::ChildEnvironmentPolicy;

const MODEL_PROBE_ENVIRONMENT_POLICY: ChildEnvironmentPolicy =
    ChildEnvironmentPolicy::CleanCloudDiscovery;
const MODEL_PROBE_MAX_ATTEMPTS: usize = crate::agent_lifecycle::MAX_STARTUP_ATTEMPTS;
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
    probe_models_for_agent(agent_cmd, None).await
}

pub async fn probe_models_for_agent(
    agent_cmd: &str,
    agent_id: Option<&str>,
) -> Result<ProbeResult> {
    probe_models_with_intent(agent_cmd, agent_id, false).await
}

pub(crate) async fn probe_models_with_intent(
    agent_cmd: &str,
    agent_id: Option<&str>,
    retry_startup: bool,
) -> Result<ProbeResult> {
    let agent_id = agent_id.or_else(|| {
        let inferred = crate::agent_registry::resolve_agent_id_from_cmd(agent_cmd);
        crate::agent_registry::is_known_id(inferred).then_some(inferred)
    });
    let registry = crate::agent_lifecycle::StartupRegistry::runtime()?;
    let key = crate::agent_lifecycle::agent_key(
        agent_cmd,
        agent_id,
        &crate::agent_source::AgentSource::Host,
        "native",
        false,
    );
    if retry_startup {
        registry.retry(&key).await?;
    }
    retry_model_probe(
        || async {
            let mut startup = registry.lock(&key).await?;
            startup.begin()?;
            let result = probe_models_impl(
                agent_cmd,
                agent_id,
                MODEL_PROBE_ENVIRONMENT_POLICY,
                &mut startup,
            )
            .await;
            result.map_err(|error| {
                let error = if startup.dependency_present() && !startup.verified() {
                    error.context(crate::agent_lifecycle::InstallationStartupCheckFailed)
                } else {
                    error
                };
                startup.failed(error)
            })
        },
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
            Err(error)
                if attempt == MODEL_PROBE_MAX_ATTEMPTS
                    || error.is::<crate::agent_lifecycle::StartupLimitReached>()
                    || error.is::<crate::agent_lifecycle::InstallationStartupCheckFailed>()
                    || error.is::<crate::agent_lifecycle::StartupCleanupUnconfirmed>()
                    || error.downcast_ref::<crate::agent_lifecycle::StartupPhase>()
                        == Some(&crate::agent_lifecycle::StartupPhase::Cleanup)
                    || error.downcast_ref::<crate::agent_lifecycle::StartupPhase>()
                        == Some(&crate::agent_lifecycle::StartupPhase::Preparation) =>
            {
                return Err(error);
            }
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
    agent_id: Option<&str>,
    environment_policy: ChildEnvironmentPolicy,
    startup: &mut crate::agent_lifecycle::StartupLease,
) -> Result<ProbeResult> {
    let mut probe = super::startup::start_probe(
        agent_cmd,
        agent_id,
        &crate::agent_source::AgentSource::Host,
        environment_policy,
        startup,
    )
    .await?;
    let result = async {
        let cwd = std::env::current_dir().context("read model probe working directory")?;
        let session_started = std::time::Instant::now();
        let session_result = tokio::time::timeout(
            Duration::from_secs(20),
            probe.connection.new_session(acp::schema::v1::NewSessionRequest::new(cwd)),
        )
        .await;
        crate::telemetry::log_acp_new_session_complete(
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
            .context(crate::agent_lifecycle::StartupPhase::Session)?
            .map_err(anyhow::Error::new)
            .context(crate::agent_lifecycle::StartupPhase::Session)?;

        let (available_models, current_model_id) =
            crate::protocol::acp::model_select::models_from_new_session(&session_resp);

        Ok(ProbeResult {
            available_models,
            current_model_id,
        })
    }
    .await;
    probe.close().await?;
    result
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
/// and capture the outcome through the shared startup/cleanup lifecycle.
pub async fn probe_sessions(agent_cmd: &str, retry_startup: bool) -> Result<SessionProbeResult> {
    let (initialize, listed) =
        super::session_list::fetch_session_list(agent_cmd, retry_startup).await?;
    let (list_sessions_ok, list_sessions_error, sessions) = match listed {
        Ok(sessions) => (
            true,
            None,
            sessions
                .into_iter()
                .map(|session| ProbedSession {
                    session_id: session.session_id.to_string(),
                    cwd: format!("{:?}", session.cwd),
                    title: session.title,
                    updated_at: session.updated_at,
                })
                .collect(),
        ),
        Err(error) => (false, Some(error), Vec::new()),
    };
    Ok(SessionProbeResult {
        initialize_dump: format!("{initialize:#?}"),
        list_sessions_ok,
        list_sessions_error,
        sessions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
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
