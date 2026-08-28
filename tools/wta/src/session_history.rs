//! Shared mapping of ACP `session/list` rows into `AgentSession`s.
//!
//! One source of truth for master's host scan (of its already-running agent)
//! and for the `probe-host-sessions` diagnostic in [`crate::cli::probes`].
//! Class-A (agent-pane) rows are
//! filtered out by `session_id` against the `agent_pane_origin` index —
//! the picker's MVP filter hides WTA-created sessions; ACP `session/list`
//! returns them, so we subtract them here.

use crate::agent_sessions::{AgentSession, AgentStatus, CliSource, SessionLocation, SessionOrigin};
use std::collections::HashSet;
use std::time::SystemTime;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

pub(crate) fn acp_session_to_agent_session(
    info: &agent_client_protocol::schema::v1::SessionInfo,
    location: SessionLocation,
    cli: &CliSource,
) -> AgentSession {
    let key = info.session_id.to_string();
    let last = info
        .updated_at
        .as_deref()
        .and_then(parse_iso_to_system_time)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let title = info
        .title
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| short_id(&key, cli_label(cli)));
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
        .filter(|s| {
            !s.title
                .as_deref()
                .is_some_and(|title| crate::agent_sessions::title_is_placeholder(cli, title))
        })
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
        CliSource::OpenCode => "opencode",
        CliSource::Unknown(_) => "agent",
    }
}

/// Parse an RFC 3339 timestamp into `SystemTime`.
///
/// Covers what agents put in ACP `SessionInfo::updated_at` — `Z` and
/// numeric-offset forms, with or without fractional seconds — and, as a
/// legacy concession, an offset-less `YYYY-MM-DDTHH:MM:SS`, which is read
/// as UTC. Returns `None` for malformed input and for anything before the
/// Unix epoch, so callers can keep treating `SystemTime` as an offset from
/// `UNIX_EPOCH` (never panics).
pub(crate) fn parse_iso_to_system_time(s: &str) -> Option<SystemTime> {
    let s = s.trim();
    let parsed = OffsetDateTime::parse(s, &Rfc3339)
        // No `Z` and no `±HH:MM` — assume UTC rather than dropping the value.
        .or_else(|_| OffsetDateTime::parse(&format!("{s}Z"), &Rfc3339))
        .ok()?;
    if parsed < OffsetDateTime::UNIX_EPOCH {
        return None;
    }
    Some(SystemTime::from(parsed))
}

/// Fallback display label for a session the agent listed without a title:
/// the CLI name plus the first 8 characters of its session id.
pub(crate) fn short_id(id: &str, cli: &str) -> String {
    let head: String = id.chars().take(8).collect();
    format!("{} {}", cli, head)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_sessions::{AgentStatus, CliSource, SessionLocation, SessionOrigin};
    use agent_client_protocol as acp;
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn info(id: &str, cwd: &str) -> acp::schema::v1::SessionInfo {
        acp::schema::v1::SessionInfo::new(
            acp::schema::v1::SessionId::new(id.to_string()),
            PathBuf::from(cwd),
        )
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

    #[test]
    fn classify_filters_only_opencode_timestamp_placeholders() {
        let mut placeholder = info("placeholder", "C:/p");
        placeholder.title = Some("New session - 2026-07-23T01:14:00.422Z".into());
        let mut real = info("real", "C:/p");
        real.title = Some("Project overview".into());
        let rows = vec![placeholder.clone(), real];

        let opencode = classify_and_map(
            &rows,
            &HashSet::new(),
            SessionLocation::Host,
            &CliSource::OpenCode,
        );
        assert_eq!(opencode.len(), 1);
        assert_eq!(opencode[0].key, "real");

        let copilot = classify_and_map(
            &[placeholder],
            &HashSet::new(),
            SessionLocation::Host,
            &CliSource::Copilot,
        );
        assert_eq!(
            copilot.len(),
            1,
            "the placeholder shape is OpenCode-specific"
        );
    }

    #[test]
    fn parse_iso_handles_positive_offset() {
        // 2026-05-27T10:53:09+08:00 is 2026-05-27T02:53:09Z
        let t1 = parse_iso_to_system_time("2026-05-27T10:53:09+08:00").unwrap();
        let t2 = parse_iso_to_system_time("2026-05-27T02:53:09Z").unwrap();
        assert_eq!(t1, t2);
    }

    #[test]
    fn parse_iso_handles_negative_offset() {
        // 2026-05-27T02:53:09-05:00 is 2026-05-27T07:53:09Z
        let t1 = parse_iso_to_system_time("2026-05-27T02:53:09-05:00").unwrap();
        let t2 = parse_iso_to_system_time("2026-05-27T07:53:09Z").unwrap();
        assert_eq!(t1, t2);
    }

    #[test]
    fn parse_iso_rejects_pre_1970_years() {
        assert!(parse_iso_to_system_time("1969-12-31T23:59:59Z").is_none());
    }

    #[test]
    fn parse_iso_rejects_invalid_month() {
        assert!(parse_iso_to_system_time("2026-13-01T00:00:00Z").is_none());
        assert!(parse_iso_to_system_time("2026-00-01T00:00:00Z").is_none());
    }

    #[test]
    fn parse_iso_rejects_invalid_day_for_month() {
        assert!(parse_iso_to_system_time("2026-02-30T00:00:00Z").is_none());
        assert!(parse_iso_to_system_time("2026-05-32T00:00:00Z").is_none());
        assert!(parse_iso_to_system_time("2026-04-31T00:00:00Z").is_none()); // April has 30
    }

    #[test]
    fn parse_iso_rejects_invalid_time_components() {
        assert!(parse_iso_to_system_time("2026-05-28T25:30:00Z").is_none());
        assert!(parse_iso_to_system_time("2026-05-28T10:60:00Z").is_none());
        assert!(parse_iso_to_system_time("2026-05-28T10:30:60Z").is_none());
    }

    #[test]
    fn parse_iso_accepts_february_29_leap_year() {
        // 2024 IS a leap year; 2023 is not.
        assert!(parse_iso_to_system_time("2024-02-29T00:00:00Z").is_some());
        assert!(parse_iso_to_system_time("2023-02-29T00:00:00Z").is_none());
    }

    #[test]
    fn parse_iso_reads_offsetless_timestamps_as_utc() {
        // Not RFC 3339 (no `Z`, no offset), but accepted by the legacy
        // hand-rolled parser, so the fallback keeps reading it as UTC.
        let naive = parse_iso_to_system_time("2026-05-27T10:53:09").unwrap();
        let utc = parse_iso_to_system_time("2026-05-27T10:53:09Z").unwrap();
        assert_eq!(naive, utc);
    }

    #[test]
    fn parse_iso_rejects_malformed_input() {
        assert!(parse_iso_to_system_time("").is_none());
        assert!(parse_iso_to_system_time("not a timestamp").is_none());
        assert!(parse_iso_to_system_time("2026-05-27").is_none());
    }
}
