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
    assert!(!cli.follows_global_acp_model);
}

#[test]
fn cli_parses_global_model_follow_scope() {
    let cli = Cli::try_parse_from(["wta", "--follows-global-acp-model"])
        .expect("global model scope flag must parse");
    assert!(cli.follows_global_acp_model);
}

#[test]
fn cli_parses_per_tab_wsl_agent_source() {
    let cli = Cli::try_parse_from([
        "wta",
        "--agent-source",
        "wsl",
        "--agent-wsl-distro",
        "Ubuntu",
        "--agent-source-cwd",
        "/home/user/project",
    ])
    .expect("WSL source flags must parse");
    assert_eq!(cli.agent_source.as_deref(), Some("wsl"));
    assert_eq!(cli.agent_wsl_distro.as_deref(), Some("Ubuntu"));
    assert_eq!(cli.agent_source_cwd.as_deref(), Some("/home/user/project"));
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
fn cli_parses_owner_tab_and_window_identity() {
    let cli = Cli::try_parse_from([
        "wta",
        "--owner-tab-id",
        "{tab-guid}",
        "--owner-window-id",
        "42",
    ])
    .expect("owner identity flags must parse");

    assert_eq!(cli.owner_tab_id.as_deref(), Some("{tab-guid}"));
    assert_eq!(cli.owner_window_id.as_deref(), Some("42"));
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
        Some(Command::Sessions {
            action: SessionsAction::List { master, origin },
        }) => {
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
        Some(Command::Sessions {
            action: SessionsAction::List { origin, .. },
        }) => {
            assert_eq!(origin, SessionsOriginArg::Shell);
            assert_eq!(origin.to_filter(), agent_sessions::OriginFilter::ShellOnly,);
        }
        other => panic!("expected sessions list command, got {other:?}"),
    }
}

#[test]
fn sessions_list_cli_parses_origin_agent_pane() {
    let cli = Cli::try_parse_from(["wta", "sessions", "list", "--origin", "agent-pane"])
        .expect("sessions list --origin agent-pane parses");
    match cli.command {
        Some(Command::Sessions {
            action: SessionsAction::List { origin, .. },
        }) => {
            assert_eq!(origin, SessionsOriginArg::AgentPane);
            assert_eq!(
                origin.to_filter(),
                agent_sessions::OriginFilter::AgentPaneOnly,
            );
        }
        other => panic!("expected sessions list command, got {other:?}"),
    }
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

    let probe_sources =
        Cli::try_parse_from(["wta", "probe-agent-sources", "--wsl-distro", "Ubuntu-24.04"])
            .unwrap();
    assert_eq!(process_label(&probe_sources), "probe");
    assert!(Cli::try_parse_from(["wta", "probe-agent-sources"]).is_err());

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

// ── Command::Delegate: --delegate-source / --delegate-wsl-distro parsing ────

#[test]
fn delegate_command_parses_explicit_source_and_distro() {
    let cli = Cli::try_parse_from([
        "wta",
        "delegate",
        "--delegate-agent",
        "codex",
        "--delegate-source",
        "wsl",
        "--delegate-wsl-distro",
        "Ubuntu",
        "do a thing",
    ])
    .expect("flags must parse");
    match cli.command {
        Some(Command::Delegate {
            delegate_source,
            delegate_wsl_distro,
            ..
        }) => {
            assert_eq!(delegate_source.as_deref(), Some("wsl"));
            assert_eq!(delegate_wsl_distro.as_deref(), Some("Ubuntu"));
        }
        other => panic!("expected Command::Delegate, got {other:?}"),
    }
}

#[test]
fn delegate_command_source_and_distro_default_to_none() {
    let cli = Cli::try_parse_from(["wta", "delegate", "do a thing"]).expect("flags must parse");
    match cli.command {
        Some(Command::Delegate {
            delegate_source,
            delegate_wsl_distro,
            ..
        }) => {
            assert!(delegate_source.is_none());
            assert!(delegate_wsl_distro.is_none());
        }
        other => panic!("expected Command::Delegate, got {other:?}"),
    }
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
    assert!(matches!(
        HooksCliFilter::OpenCode.into_scope(),
        CliScope::One(CliKind::OpenCode)
    ));
}

/// `--only-missing` is the Settings "Install hooks" button's contract with
/// wta. It must stay opt-in: a bare `wta hooks install` remains the full
/// (re)install a user reaches for when something is broken.
#[test]
fn hooks_install_only_missing_is_opt_in() {
    use crate::cli::args::HooksAction;

    let default = Cli::try_parse_from(["wta", "hooks", "install"]).expect("flags must parse");
    match default.command {
        Some(Command::Hooks {
            action: HooksAction::Install { only_missing, .. },
        }) => assert!(!only_missing),
        other => panic!("expected Command::Hooks/Install, got {other:?}"),
    }

    let opted = Cli::try_parse_from(["wta", "hooks", "install", "--only-missing"])
        .expect("flags must parse");
    match opted.command {
        Some(Command::Hooks {
            action: HooksAction::Install { only_missing, .. },
        }) => assert!(only_missing),
        other => panic!("expected Command::Hooks/Install, got {other:?}"),
    }
}

// ── json_str_or_num: tolerant scalar extraction for human table rows ─────────

#[test]
fn json_str_or_num_reads_strings_and_numbers_else_dash() {
    let v = serde_json::json!({ "s": "hi", "n": 42, "b": true, "nl": null });
    assert_eq!(cli::wt::json_str_or_num(&v, "s"), "hi");
    assert_eq!(cli::wt::json_str_or_num(&v, "n"), "42");
    // Non-scalar / wrong-type / missing keys all degrade to "-".
    assert_eq!(cli::wt::json_str_or_num(&v, "b"), "-");
    assert_eq!(cli::wt::json_str_or_num(&v, "nl"), "-");
    assert_eq!(cli::wt::json_str_or_num(&v, "missing"), "-");
}

// ── `/agent` source picker: WSL pane detection ──────────────────────────────
//
// `active_pane_wsl_distro` detects whether the active pane is a WSL shell so
// the `/agent` command-palette source picker can offer that distro's ACP
// agents alongside the host ones (see `App::request_agent_source_picker` in
// `app.rs`). `wta delegate` no longer uses this helper for source selection —
// its explicit `--delegate-source`/`--delegate-wsl-distro` flags own that
// decision (see `cli::delegate::parse_delegate_source` and its tests in
// `cli/delegate.rs`); this detector remains covered here because the `/agent`
// picker still depends on it.

/// Build a minimal active-pane JSON value with the given `shell` field, as
/// reported by WT's `get_active_pane` / `OSC 9001;ShellType`.
fn pane_with_shell(shell: &str) -> serde_json::Value {
    serde_json::json!({ "shell": shell })
}

#[test]
fn active_pane_wsl_distro_extracts_distro_name() {
    // `wsl:<distro>` → the distro name (drives `wsl -d <distro>`).
    assert_eq!(
        crate::agent_source::active_pane_wsl_distro(Some(&pane_with_shell("wsl:Ubuntu"))),
        Some("Ubuntu")
    );
    assert_eq!(
        crate::agent_source::active_pane_wsl_distro(Some(&pane_with_shell("wsl:Ubuntu-22.04"))),
        Some("Ubuntu-22.04")
    );
}

#[test]
fn active_pane_wsl_distro_rejects_non_wsl_shells() {
    // Non-WSL shells → None (host path).
    assert_eq!(
        crate::agent_source::active_pane_wsl_distro(Some(&pane_with_shell("pwsh"))),
        None
    );
    assert_eq!(
        crate::agent_source::active_pane_wsl_distro(Some(&pane_with_shell("cmd"))),
        None
    );
    // A pane name that merely contains "wsl" is not the `wsl:` prefix.
    assert_eq!(
        crate::agent_source::active_pane_wsl_distro(Some(&pane_with_shell("my-wsl"))),
        None
    );
    // Bare `wsl:` with an empty distro name is not a valid WSL pane — shell
    // integration only emits `wsl:<distro>` when `$WSL_DISTRO_NAME` is set —
    // and would otherwise build an invalid `wsl -d "" …` command.
    assert_eq!(
        crate::agent_source::active_pane_wsl_distro(Some(&pane_with_shell("wsl:"))),
        None
    );
    // `shell` field absent.
    let no_shell = serde_json::json!({ "cwd": "/home/u" });
    assert_eq!(
        crate::agent_source::active_pane_wsl_distro(Some(&no_shell)),
        None
    );
    // `shell` present but not a string.
    let numeric_shell = serde_json::json!({ "shell": 42 });
    assert_eq!(
        crate::agent_source::active_pane_wsl_distro(Some(&numeric_shell)),
        None
    );
    // No active pane at all.
    assert_eq!(crate::agent_source::active_pane_wsl_distro(None), None);
}

#[test]
fn wsl_agent_probe_script_prints_command_v_resolution() {
    // Emits `command -v <exe>` straight to stdout (the caller captures it and
    // rejects empty or /mnt results). Deliberately NOT wrapped in `$(…)`, which
    // returns empty for snap apps. sh_quote single-quotes the exe.
    assert_eq!(
        crate::agent_check::wsl_agent_probe_script("copilot"),
        "printf '__WTA_PROBE_BEGIN__\\n'; command -v 'copilot' 2>/dev/null; \
         printf '__WTA_PROBE_END__\\n'"
    );
    // An agent identity with shell metacharacters stays contained in the quotes.
    assert_eq!(
        crate::agent_check::wsl_agent_probe_script("my agent; rm -rf /"),
        "printf '__WTA_PROBE_BEGIN__\\n'; command -v 'my agent; rm -rf /' 2>/dev/null; \
         printf '__WTA_PROBE_END__\\n'"
    );
}

#[test]
fn propose_terminal_actions_cli_parses_channel_and_inline_payload() {
    let cli = Cli::try_parse_from([
        "wta",
        "propose-terminal-actions",
        "--channel",
        "v1.0123456789abcdef0123456789abcdef.abcdef0123456789abcdef0123456789",
        "--payload-json",
        r#"{"schema_version":1}"#,
    ])
    .expect("propose-terminal-actions flags must parse");

    match cli.command {
        Some(Command::ProposeTerminalActions {
            channel,
            payload_json,
        }) => {
            assert_eq!(
                channel,
                "v1.0123456789abcdef0123456789abcdef.abcdef0123456789abcdef0123456789"
            );
            assert_eq!(payload_json, r#"{"schema_version":1}"#);
        }
        other => panic!("expected ProposeTerminalActions command, got {other:?}"),
    }
}

#[test]
fn propose_terminal_actions_cli_requires_channel_and_payload() {
    Cli::try_parse_from(["wta", "propose-terminal-actions"])
        .expect_err("channel and payload are required");
    Cli::try_parse_from([
        "wta",
        "propose-terminal-actions",
        "--channel",
        "channel-only",
    ])
    .expect_err("payload is required");
}

// `wta delegate`'s own launch checks (explicit `--delegate-source`, never
// auto-routed) are covered by the module-private tests in `cli/delegate.rs`,
// alongside its `parse_delegate_source` / `select_wsl_delegate_cwd` coverage.
