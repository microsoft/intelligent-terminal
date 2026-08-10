//! Bridge from the reusable AHP client into wta-master's session registry.

use std::collections::HashSet;
use std::path::PathBuf;

use ahp_types::state::{SessionStatus, SessionSummary};
use anyhow::{Context, Result};

use super::client::{RemoteClientConfig, RemoteClientEvent};

const REMOTE_HOST_ADDRESS_ENV: &str = "WTA_REMOTE_HOST_ADDRESS";
const REMOTE_HOST_TOKEN_PATH_ENV: &str = "WTA_REMOTE_HOST_AUTH_TOKEN_PATH";
const REMOTE_CLIENT_ID_ENV: &str = "WTA_REMOTE_CLIENT_ID";

pub(crate) fn terminal_client_config_from_env() -> Result<Option<RemoteClientConfig>> {
    let Some(address) = std::env::var_os(REMOTE_HOST_ADDRESS_ENV) else {
        return Ok(None);
    };
    let address = address
        .to_string_lossy()
        .parse()
        .with_context(|| format!("parse {REMOTE_HOST_ADDRESS_ENV} as a socket address"))?;
    let token_path = std::env::var_os(REMOTE_HOST_TOKEN_PATH_ENV).map(PathBuf::from);
    let auth_token = super::load_auth_token(token_path)?;
    let client_id = std::env::var(REMOTE_CLIENT_ID_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("wta-terminal-{}", std::process::id()));
    Ok(Some(RemoteClientConfig {
        address,
        client_id,
        auth_token,
        ..RemoteClientConfig::default()
    }))
}

pub(crate) async fn apply_terminal_client_event(
    registry: &dyn crate::session_registry::SessionRegistry,
    host: &str,
    event: &RemoteClientEvent,
) -> bool {
    match event {
        RemoteClientEvent::Connected { sessions, .. }
        | RemoteClientEvent::CatalogueChanged { sessions, .. } => {
            reconcile_catalogue(registry, host, sessions).await
        }
        RemoteClientEvent::Disconnected { error, .. } => {
            update_remote_status(
                registry,
                host,
                crate::agent_sessions::AgentStatus::Error,
                Some(error.clone()),
                None,
            )
            .await
        }
        RemoteClientEvent::AuthRequired { params, .. } => {
            update_remote_status(
                registry,
                host,
                crate::agent_sessions::AgentStatus::Attention,
                None,
                Some(format!("Remote authentication {:?}", params.reason)),
            )
            .await
        }
        RemoteClientEvent::Action { .. } => false,
    }
}

async fn reconcile_catalogue(
    registry: &dyn crate::session_registry::SessionRegistry,
    host: &str,
    sessions: &[SessionSummary],
) -> bool {
    let projected: Vec<_> = sessions
        .iter()
        .map(|summary| project_session(host, summary))
        .collect();
    let projected_ids: HashSet<_> = projected
        .iter()
        .map(|row| row.session_id.clone())
        .collect();
    let existing: Vec<_> = registry
        .snapshot()
        .await
        .into_iter()
        .filter(|row| {
            matches!(
                &row.location,
                crate::agent_sessions::SessionLocation::Remote {
                    host: row_host,
                    ..
                } if row_host == host
            )
        })
        .collect();

    let mut changed = existing.len() != projected.len();
    for row in existing {
        if !projected_ids.contains(&row.session_id) {
            changed |= registry.remove(&row.session_id).await.is_some();
        }
    }
    for row in projected {
        changed |= registry.lookup(&row.session_id).await.as_ref() != Some(&row);
        registry.upsert(row).await;
    }
    changed
}

async fn update_remote_status(
    registry: &dyn crate::session_registry::SessionRegistry,
    host: &str,
    status: crate::agent_sessions::AgentStatus,
    last_error: Option<String>,
    attention_reason: Option<String>,
) -> bool {
    let rows: Vec<_> = registry
        .snapshot()
        .await
        .into_iter()
        .filter(|row| {
            matches!(
                &row.location,
                crate::agent_sessions::SessionLocation::Remote {
                    host: row_host,
                    ..
                } if row_host == host
            )
        })
        .collect();
    let mut changed = false;
    for mut row in rows {
        if row.status.as_ref() != Some(&status)
            || row.last_error != last_error
            || row.attention_reason != attention_reason
        {
            changed = true;
            row.status = Some(status.clone());
            row.last_error = last_error.clone();
            row.attention_reason = attention_reason.clone();
            registry.upsert(row).await;
        }
    }
    changed
}

fn project_session(host: &str, summary: &SessionSummary) -> crate::session_registry::SessionInfo {
    let resource = summary.resource.clone();
    let session_id =
        agent_client_protocol::schema::v1::SessionId::new(format!("remote:{host}:{resource}"));
    let cwd = summary
        .working_directories
        .as_ref()
        .and_then(|directories| directories.first())
        .or_else(|| summary.project.as_ref().map(|project| &project.uri))
        .map(PathBuf::from)
        .unwrap_or_default();
    let mut row = crate::session_registry::SessionInfo::new(session_id, cwd);
    row.title = Some(summary.title.clone());
    row.updated_at = Some(summary.modified_at.clone());
    row.status = Some(map_status(summary.status));
    row.cli_source = crate::agent_sessions::CliSource::from_agent_id(&summary.provider)
        .or_else(|| Some(crate::agent_sessions::CliSource::Unknown(summary.provider.clone())));
    row.current_tool = summary.activity.clone();
    row.attention_reason = SessionStatus::from_bits(summary.status)
        .contains(SessionStatus::InputNeeded)
        .then(|| summary.activity.clone())
        .flatten();
    row.origin = Some(crate::agent_sessions::SessionOrigin::Unknown);
    row.location = crate::agent_sessions::SessionLocation::Remote {
        host: host.to_string(),
        resource,
    };
    row
}

fn map_status(status: u32) -> crate::agent_sessions::AgentStatus {
    let status = SessionStatus::from_bits(status);
    if status.contains(SessionStatus::InputNeeded) {
        crate::agent_sessions::AgentStatus::Attention
    } else if status.contains(SessionStatus::InProgress) {
        crate::agent_sessions::AgentStatus::Working
    } else if status.contains(SessionStatus::Error) {
        crate::agent_sessions::AgentStatus::Error
    } else {
        crate::agent_sessions::AgentStatus::Idle
    }
}
