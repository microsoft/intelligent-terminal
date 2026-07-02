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
        agent_client_protocol::schema::v1::SessionId::new("sid-json"),
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
        agent_client_protocol::schema::v1::SessionId::new("sid-table"),
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
    // Leading 1-based index column.
    assert!(out.lines().next().expect("header").starts_with("#"), "header has # column");
    assert!(body.starts_with("1"), "first row is numbered 1, got: {body}");
}

#[test]
fn sessions_table_renders_origin_labels() {
    let mut shell = session_registry::SessionInfo::new(
        agent_client_protocol::schema::v1::SessionId::new("sid-shell"),
        std::path::PathBuf::from("C:\\repo"),
    );
    shell.origin = Some(agent_sessions::SessionOrigin::Unknown);
    let mut pane = session_registry::SessionInfo::new(
        agent_client_protocol::schema::v1::SessionId::new("sid-pane"),
        std::path::PathBuf::from("C:\\repo"),
    );
    pane.origin = Some(agent_sessions::SessionOrigin::AgentPane);

    let out = format_sessions_table(&[shell, pane]);
    assert!(out.contains("Shell"), "shell origin label present: {out}");
    assert!(out.contains("AgentPane"), "agent-pane origin label present: {out}");
}

#[test]
fn sessions_table_renders_location_labels() {
    let mut host = session_registry::SessionInfo::new(
        agent_client_protocol::schema::v1::SessionId::new("sid-host"),
        std::path::PathBuf::from("C:\\repo"),
    );
    host.location = agent_sessions::SessionLocation::Host;
    let mut wsl = session_registry::SessionInfo::new(
        agent_client_protocol::schema::v1::SessionId::new("sid-wsl"),
        std::path::PathBuf::from("/home/u"),
    );
    wsl.location = agent_sessions::SessionLocation::Wsl { distro: "Ubuntu".into() };

    let out = format_sessions_table(&[host, wsl]);
    assert!(out.contains("LOCATION"), "LOCATION header present: {out}");
    assert!(out.contains("host"), "host location label present: {out}");
    assert!(out.contains("wsl:Ubuntu"), "wsl distro label present: {out}");
}

#[test]
fn format_epoch_ms_utc_known_values() {
    assert_eq!(format_epoch_ms_utc(0), "1970-01-01 00:00");
    // 2021-01-01 00:00:00 UTC
    assert_eq!(format_epoch_ms_utc(1_609_459_200_000), "2021-01-01 00:00");
    // 2021-03-01 (just past a non-leap February) sanity-checks the month math.
    assert_eq!(format_epoch_ms_utc(1_614_556_800_000), "2021-03-01 00:00");
}

#[test]
fn updated_label_falls_back_to_last_activity_ms() {
    let mut s = session_registry::SessionInfo::new(
        agent_client_protocol::schema::v1::SessionId::new("sid-u"),
        std::path::PathBuf::from("/home/u"),
    );
    // No updated_at, but an epoch-ms activity stamp -> formatted, not "-".
    s.updated_at = None;
    s.last_activity_at_ms = Some(1_609_459_200_000);
    assert_eq!(updated_label(&s), "2021-01-01 00:00");
    // updated_at, when present, wins verbatim.
    s.updated_at = Some("2026-06-22T03:33:46Z".into());
    assert_eq!(updated_label(&s), "2026-06-22T03:33:46Z");
}

// ── normalize_locale: OS-locale → bundled-locale affinity matching ──────────

#[test]
fn normalize_locale_exact_match_is_passthrough() {
    // A locale we ship verbatim is returned unchanged (the input casing is
    // preserved — step 1 returns the caller's string, not the file stem).
    assert_eq!(normalize_locale("en-US"), "en-US");
    assert_eq!(normalize_locale("zh-CN"), "zh-CN");
    // Canadian French is shipped, so affinity must NOT rewrite it to fr-FR.
    assert_eq!(normalize_locale("fr-CA"), "fr-CA");
}

#[test]
fn normalize_locale_script_and_region_affinity() {
    // Chinese: script-based split.
    assert_eq!(normalize_locale("zh-HK"), "zh-TW");
    assert_eq!(normalize_locale("zh-Hant-HK"), "zh-TW");
    assert_eq!(normalize_locale("zh-SG"), "zh-CN");
    assert_eq!(normalize_locale("zh-Hans"), "zh-CN");
    // English: Commonwealth regions → en-GB.
    assert_eq!(normalize_locale("en-AU"), "en-GB");
    assert_eq!(normalize_locale("en-IN"), "en-GB");
    // Spanish: Latin-American regions → es-MX.
    assert_eq!(normalize_locale("es-AR"), "es-MX");
    assert_eq!(normalize_locale("es-419"), "es-MX");
    // French: non-Canadian → fr-FR.
    assert_eq!(normalize_locale("fr-BE"), "fr-FR");
    // Portuguese: non-Brazilian → pt-PT.
    assert_eq!(normalize_locale("pt-MZ"), "pt-PT");
    // Serbian: script-based split.
    assert_eq!(normalize_locale("sr-Latn-BA"), "sr-Latn-RS");
    assert_eq!(normalize_locale("sr-Cyrl-ME"), "sr-Cyrl-RS");
}

#[test]
fn normalize_locale_affinity_is_case_insensitive() {
    assert_eq!(normalize_locale("ZH-hk"), "zh-TW");
    assert_eq!(normalize_locale("EN-au"), "en-GB");
}

#[test]
fn normalize_locale_strips_territory_for_single_variant_languages() {
    // We ship exactly one German / Japanese variant, so an unknown region
    // falls back to it via the language-prefix match (step 3).
    assert_eq!(normalize_locale("de-AT"), "de-DE");
    assert_eq!(normalize_locale("ja-XX"), "ja-JP");
}

#[test]
fn normalize_locale_unknown_language_falls_back_to_en_us() {
    assert_eq!(normalize_locale("xx-YY"), "en-US");
    assert_eq!(normalize_locale(""), "en-US");
}

// ── process_label: per-process log-file label derived from the CLI shape ─────

#[test]
fn process_label_default_no_subcommand_is_main() {
    let cli = Cli::try_parse_from(["wta"]).unwrap();
    assert_eq!(process_label(&cli), "main");
}

#[test]
fn process_label_master_and_helper_modes() {
    let master = Cli::try_parse_from(["wta", "--master", "\\\\.\\pipe\\m"]).unwrap();
    assert_eq!(process_label(&master), "main_master");

    let helper = Cli::try_parse_from(["wta", "--connect-master", "\\\\.\\pipe\\h"]).unwrap();
    assert!(
        process_label(&helper).starts_with("main_helper-"),
        "helper label is per-PID"
    );
}

#[test]
fn process_label_short_lived_diagnostic_flags_are_cli() {
    let info = Cli::try_parse_from(["wta", "--info"]).unwrap();
    assert_eq!(process_label(&info), "cli");
    let test_pipe = Cli::try_parse_from(["wta", "--test-pipe"]).unwrap();
    assert_eq!(process_label(&test_pipe), "cli");
}

#[test]
fn process_label_subcommands() {
    let delegate = Cli::try_parse_from(["wta", "delegate", "do a thing"]).unwrap();
    assert_eq!(process_label(&delegate), "delegate");

    let probe = Cli::try_parse_from(["wta", "probe-models", "--agent", "copilot"]).unwrap();
    assert_eq!(process_label(&probe), "probe");

    let probe_sessions =
        Cli::try_parse_from(["wta", "probe-sessions", "--agent", "copilot"]).unwrap();
    assert_eq!(process_label(&probe_sessions), "probe");

    let probe_host =
        Cli::try_parse_from(["wta", "probe-host-sessions", "--agent", "copilot"]).unwrap();
    assert_eq!(process_label(&probe_host), "probe");

    let probe_wsl = Cli::try_parse_from(["wta", "probe-wsl-sessions"]).unwrap();
    assert_eq!(process_label(&probe_wsl), "probe");

    // Any other subcommand is a short-lived wtcli-style client.
    let sessions = Cli::try_parse_from(["wta", "sessions", "list"]).unwrap();
    assert_eq!(process_label(&sessions), "cli");
}

// ── HooksCliFilter::into_scope: CLI filter → installer scope ─────────────────

#[test]
fn hooks_cli_filter_into_scope_maps_each_variant() {
    use agent_hooks_installer::{CliKind, CliScope};
    assert!(matches!(HooksCliFilter::All.into_scope(), CliScope::All));
    assert!(matches!(
        HooksCliFilter::Copilot.into_scope(),
        CliScope::One(CliKind::Copilot)
    ));
    assert!(matches!(
        HooksCliFilter::Claude.into_scope(),
        CliScope::One(CliKind::Claude)
    ));
    assert!(matches!(
        HooksCliFilter::Gemini.into_scope(),
        CliScope::One(CliKind::Gemini)
    ));
    assert!(matches!(
        HooksCliFilter::Codex.into_scope(),
        CliScope::One(CliKind::Codex)
    ));
}

// ── json_str_or_num: tolerant scalar extraction for human table rows ─────────

#[test]
fn json_str_or_num_reads_strings_and_numbers_else_dash() {
    let v = serde_json::json!({ "s": "hi", "n": 42, "b": true, "nl": null });
    assert_eq!(json_str_or_num(&v, "s"), "hi");
    assert_eq!(json_str_or_num(&v, "n"), "42");
    // Non-scalar / wrong-type / missing keys all degrade to "-".
    assert_eq!(json_str_or_num(&v, "b"), "-");
    assert_eq!(json_str_or_num(&v, "nl"), "-");
    assert_eq!(json_str_or_num(&v, "missing"), "-");
}
