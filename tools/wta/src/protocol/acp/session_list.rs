//! Shared ACP `session/list` plumbing.
//!
//! Both host diagnostics use shared startup admission and confirmed cleanup.
//! Callers must drive this inside a tokio `LocalSet`.

use crate::protocol::acp::spawn::{ChildEnvironmentPolicy, SharedProviderSelection};
use agent_client_protocol as acp;
use anyhow::Result;
use std::time::Duration;

/// The successful list outcome, or a human-readable reason it failed.
///
/// `session/list` is an UNSTABLE ACP capability: an agent that doesn't
/// implement it answers `Method not found`. That is a normal,
/// non-fatal outcome (distinct from a transport/`initialize` failure,
/// which surfaces as the outer `Err`), so it is captured as a `String`
/// rather than collapsing the whole call.
pub(crate) type ListOutcome = std::result::Result<Vec<acp::schema::v1::SessionInfo>, String>;

/// Initialize once, inspect session-list support, and close the owned process
/// while retaining the same admission lease used by inherited-provider masters.
pub(crate) async fn fetch_session_list(
    agent_cmd: &str,
    retry_startup: bool,
) -> Result<(acp::schema::v1::InitializeResponse, ListOutcome)> {
    let shared = crate::custom_model_provider::Config::shared_from_env();
    let inferred = crate::agent_registry::resolve_agent_id_from_cmd(agent_cmd);
    let agent_id = crate::agent_registry::is_known_id(inferred).then_some(inferred);
    let source = crate::agent_source::AgentSource::Host;
    let binding = crate::agent_lifecycle::provider_binding_key("legacy", &shared);
    let key = crate::agent_lifecycle::agent_key(agent_cmd, agent_id, &source, &binding, false);
    let registry = crate::agent_lifecycle::StartupRegistry::runtime()?;
    if retry_startup {
        registry.retry(&key).await?;
    }
    let mut startup = registry.lock(&key).await?;
    startup.begin()?;
    let result = async {
        let mut probe = super::startup::start_probe_with_provider(
            agent_cmd,
            agent_id,
            &source,
            ChildEnvironmentPolicy::ApplySharedProvider,
            SharedProviderSelection::Custom(&shared),
            &mut startup,
        )
        .await?;

        let list = match tokio::time::timeout(
            Duration::from_secs(10),
            probe
                .connection
                .list_sessions(acp::schema::v1::ListSessionsRequest::new()),
        )
        .await
        {
            Err(_) => Err("list_sessions timed out after 10s".to_string()),
            Ok(Err(error)) => Err(error.to_string()),
            Ok(Ok(response)) => Ok(response.sessions),
        };
        probe.close().await?;
        Ok((probe.response, list))
    }
    .await;
    result.map_err(|error| startup.failed(error))
}
