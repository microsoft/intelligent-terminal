use super::*;
use clap::Parser;

// Plan-C boot-time initial-load flags: WT bundles a session resume
// with helper spawn by passing `--initial-load-session-id` (and
// optionally `--initial-load-cwd`) on the helper's command line.
// Replaces the race-prone "spawn helper, then broadcast a separate
// `load_session` VT event" path that often misrouted.

#[test]
fn cli_parses_initial_load_session_id() {
    let cli = Cli::try_parse_from([
        "wta",
        "--initial-load-session-id",
        "abc-123",
        "--initial-load-cwd",
        "C:/foo/bar",
    ])
    .expect("flags must parse");
    assert_eq!(cli.initial_load_session_id.as_deref(), Some("abc-123"));
    assert_eq!(cli.initial_load_cwd.as_deref(), Some("C:/foo/bar"));
}

#[test]
fn cli_initial_load_session_id_defaults_to_none() {
    let cli = Cli::try_parse_from(["wta"]).expect("no flags must parse");
    assert!(cli.initial_load_session_id.is_none());
    assert!(cli.initial_load_cwd.is_none());
}

#[test]
fn cli_initial_load_session_id_without_cwd_is_allowed() {
    // cwd is optional — the helper falls back to its process cwd when
    // omitted (matches the runtime `load_session` arm's behavior).
    let cli = Cli::try_parse_from(["wta", "--initial-load-session-id", "sid-only"])
        .expect("session id alone must parse");
    assert_eq!(cli.initial_load_session_id.as_deref(), Some("sid-only"));
    assert!(cli.initial_load_cwd.is_none());
}

#[test]
fn sessions_list_cli_parses_json_and_master_override() {
    let cli = Cli::try_parse_from([
        "wta",
        "sessions",
        "list",
        "--json",
        "--master",
        r"\\.\pipe\wta-master-test",
    ])
    .expect("sessions list parses");

    assert!(cli.json);
    match cli.command {
        Some(Command::Sessions { action: SessionsAction::List { master, origin } }) => {
            assert_eq!(master.as_deref(), Some(r"\\.\pipe\wta-master-test"));
            // Default keeps the historical debug behavior — show
            // every origin. MVP sessions picker has its own default in
            // `app::resolve_sessions_origin_filter`; this CLI default is
            // intentionally divergent so `wta sessions list` is
            // the "see everything" debug tool.
            assert_eq!(origin, SessionsOriginArg::All);
        }
        other => panic!("expected sessions list command, got {other:?}"),
    }
}

#[test]
fn sessions_list_cli_parses_origin_shell() {
    let cli = Cli::try_parse_from(["wta", "sessions", "list", "--origin", "shell"])
        .expect("sessions list --origin shell parses");
    match cli.command {
        Some(Command::Sessions { action: SessionsAction::List { origin, .. } }) => {
            assert_eq!(origin, SessionsOriginArg::Shell);
            assert_eq!(
                origin.to_filter(),
                agent_sessions::OriginFilter::ShellOnly,
            );
        }
        other => panic!("expected sessions list command, got {other:?}"),
    }
}

#[test]
fn sessions_list_cli_parses_origin_agent_pane() {
    let cli = Cli::try_parse_from(["wta", "sessions", "list", "--origin", "agent-pane"])
        .expect("sessions list --origin agent-pane parses");
    match cli.command {
        Some(Command::Sessions { action: SessionsAction::List { origin, .. } }) => {
            assert_eq!(origin, SessionsOriginArg::AgentPane);
            assert_eq!(
                origin.to_filter(),
                agent_sessions::OriginFilter::AgentPaneOnly,
            );
        }
        other => panic!("expected sessions list command, got {other:?}"),
    }
}

#[test]
fn sessions_json_lines_prints_one_session_info_per_line() {
    let mut row = session_registry::SessionInfo::new(
        agent_client_protocol::SessionId::new("sid-json"),
        std::path::PathBuf::from("C:\\repo"),
    );
    row.status = Some(agent_sessions::AgentStatus::Working);
    row.cli_source = Some(agent_sessions::CliSource::Copilot);
    row.current_tool = Some("shell".into());

    let out = format_sessions_json_lines(&[row]).expect("format jsonl");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 1);
    let value: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(value["session_id"], "sid-json");
    assert_eq!(value["status"], "Working");
    assert_eq!(value["cli_source"], "Copilot");
    assert_eq!(value["current_tool"], "shell");
}

#[test]
fn sessions_table_prints_header_and_rows() {
    let mut row = session_registry::SessionInfo::new(
        agent_client_protocol::SessionId::new("sid-table"),
        std::path::PathBuf::from("C:\\repo"),
    );
    row.title = Some("fix build".into());
    row.status = Some(agent_sessions::AgentStatus::Idle);
    row.cli_source = Some(agent_sessions::CliSource::Claude);
    row.pane_session_id = Some("pane-table".into());

    let out = format_sessions_table(&[row]);
    assert!(out.contains("SESSION"));
    assert!(out.contains("sid-table"));
    assert!(out.contains("Idle"));
    assert!(out.contains("Claude"));
    assert!(out.contains("pane-table"));
    // ORIGIN column exists and untagged rows render as "-" so the
    // operator can tell "legacy / unclassified" from "shell".
    assert!(out.contains("ORIGIN"));
    let body = out.lines().nth(1).expect("body row present");
    assert!(body.contains(" - "), "untagged origin renders as '-' got: {body}");
}

#[test]
fn sessions_table_renders_origin_labels() {
    let mut shell = session_registry::SessionInfo::new(
        agent_client_protocol::SessionId::new("sid-shell"),
        std::path::PathBuf::from("C:\\repo"),
    );
    shell.origin = Some(agent_sessions::SessionOrigin::Unknown);
    let mut pane = session_registry::SessionInfo::new(
        agent_client_protocol::SessionId::new("sid-pane"),
        std::path::PathBuf::from("C:\\repo"),
    );
    pane.origin = Some(agent_sessions::SessionOrigin::AgentPane);

    let out = format_sessions_table(&[shell, pane]);
    assert!(out.contains("Shell"), "shell origin label present: {out}");
    assert!(out.contains("AgentPane"), "agent-pane origin label present: {out}");
}
