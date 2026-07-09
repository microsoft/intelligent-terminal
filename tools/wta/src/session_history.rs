//! Shared mapping of ACP `session/list` rows into `AgentSession`s.
//!
//! One source of truth for both the host scan (master's already-running
//! agent) and the WSL scan (`wsl_acp`). Class-A (agent-pane) rows are
//! filtered out by `session_id` against the `agent_pane_origin` index —
//! the picker's MVP filter hides WTA-created sessions; ACP `session/list`
//! returns them, so we subtract them here.

use crate::agent_sessions::{AgentSession, AgentStatus, CliSource, SessionLocation, SessionOrigin};
use std::collections::HashSet;
use std::time::SystemTime;

pub(crate) fn acp_session_to_agent_session(
    info: &agent_client_protocol::schema::v1::SessionInfo,
    location: SessionLocation,
    cli: &CliSource,
) -> AgentSession {
    let key = info.session_id.to_string();
    let last = info
        .updated_at
        .as_deref()
        .and_then(crate::history_loader::parse_iso_to_system_time)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let title = info
        .title
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| crate::history_loader::short_id(&key, cli_label(cli)));
    AgentSession {
        key,
        cli_source: cli.clone(),
        pane_session_id: None,
        window_id: None,
        tab_id: None,
        title,
        cwd: info.cwd.clone(),
        started_at: last,
        last_activity_at: last,
        status: AgentStatus::Historical,
        last_error: None,
        current_tool: None,
        attention_reason: None,
        log_path: None,
        origin: SessionOrigin::default(),
        location,
    }
}

pub(crate) fn classify_and_map(
    sessions: &[agent_client_protocol::schema::v1::SessionInfo],
    agent_pane_index: &HashSet<String>,
    location: SessionLocation,
    cli: &CliSource,
) -> Vec<AgentSession> {
    sessions
        .iter()
        .map(|s| acp_session_to_agent_session(s, location.clone(), cli))
        .filter(|s| !agent_pane_index.contains(&s.key))
        .collect()
}

pub(crate) fn cli_label(cli: &CliSource) -> &'static str {
    match cli {
        CliSource::Copilot => "copilot",
        CliSource::Claude => "claude",
        CliSource::Codex => "codex",
        CliSource::Gemini => "gemini",
        CliSource::Unknown(_) => "agent",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_sessions::{AgentStatus, CliSource, SessionLocation, SessionOrigin};
    use agent_client_protocol as acp;
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn info(id: &str, cwd: &str) -> acp::schema::v1::SessionInfo {
        acp::schema::v1::SessionInfo::new(acp::schema::v1::SessionId::new(id.to_string()), PathBuf::from(cwd))
    }

    #[test]
    fn maps_host_row_with_origin_and_location() {
        let mut s = info("abc-1", "C:/Users/u");
        s.title = Some("Hello".into());
        s.updated_at = Some("2026-06-24T04:42:14.588Z".into());
        let row = acp_session_to_agent_session(&s, SessionLocation::Host, &CliSource::Copilot);
        assert_eq!(row.key, "abc-1");
        assert_eq!(row.location, SessionLocation::Host);
        assert_eq!(row.status, AgentStatus::Historical);
        assert_eq!(row.origin, SessionOrigin::default());
        assert!(row.last_activity_at > std::time::SystemTime::UNIX_EPOCH);
    }

    #[test]
    fn classify_filters_class_a_by_session_id() {
        let rows = vec![info("keep-b", "C:/p"), info("hide-a", "C:/q")];
        let mut idx = HashSet::new();
        idx.insert("hide-a".to_string());
        let out = classify_and_map(&rows, &idx, SessionLocation::Host, &CliSource::Copilot);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].key, "keep-b");
    }
}
