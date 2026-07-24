//! Core App unit tests, split out of the large app.rs file so it lives
//! in its own file. This is a child module of `app` (declared with `#[path]`
//! in app.rs), not of the crate root, so it can reach App's private
//! dispatch methods and state directly, the same way the file used to when
//! this was an inline `mod tests { ... }` block.

use super::*;
use serde_json::json;

/// Custom-agent preflight regression: when the user's `acpAgent` is a
/// `custom:*` id, the preflight must NOT gate the TUI into Setup mode.
/// Previously `check_agent("custom:foo")` walked PATH for a literal
/// `custom:foo.exe`, always failed, and dropped the TUI into Setup with
/// the misleading `DEFAULT_PROFILE` "Agent" display name — blocking
/// `/restart` and other chat input until a re-save lifecycle-raced the
/// preflight failure.
#[test]
fn passed_for_custom_agent_never_triggers_setup_mode() {
    let r = PreflightResult::passed_for_custom_agent("custom:foo");
    // Identity preserved on the canonical id (downstream retry/auth
    // paths still see `custom:foo`, not the bare exe name).
    assert_eq!(r.agent_id, "custom:foo");
    // Display name comes from the canonical id stripped of the
    // `custom:` prefix — never the generic `DEFAULT_PROFILE` "Agent".
    assert_eq!(r.display_name, "foo");
    // `all_passed()` must return true so the PreflightComplete handler
    // does NOT enter `AppMode::Setup` ("Agent not installed" banner).
    assert!(r.all_passed());
    assert_eq!(r.cli_status, CheckStatus::Passed);
    assert!(matches!(r.auth_status, CheckStatus::Skipped));
}

/// Defensive: a bare `custom:` (empty name) or a non-`custom:` unknown id
/// must not produce an empty display name. Falls back to the canonical id.
#[test]
fn passed_for_custom_agent_falls_back_when_no_custom_suffix() {
    let r = PreflightResult::passed_for_custom_agent("custom:");
    assert_eq!(r.display_name, "custom:");
    assert!(r.all_passed());

    let r2 = PreflightResult::passed_for_custom_agent("some-unknown-id");
    assert_eq!(r2.display_name, "some-unknown-id");
    assert!(r2.all_passed());
}

// Helper to create an App for testing (avoids needing real channels for simple state tests).
// `pub(super)` so the sibling `slash_command_tests` module (see the
// `#[path]` mod in app.rs) can reuse it instead of duplicating App::new.
pub(super) fn test_app() -> App {
    let (prompt_tx, _prompt_rx) = tokio::sync::mpsc::unbounded_channel();
    let (recommendation_tx, _recommendation_rx) = tokio::sync::mpsc::unbounded_channel();
    let (permission_tx, _permission_rx) = tokio::sync::mpsc::unbounded_channel();
    let (cancel_tx, _cancel_rx) = tokio::sync::mpsc::unbounded_channel();
    let (new_session_tx, _new_session_rx) = tokio::sync::mpsc::unbounded_channel();
    let (load_session_tx, _load_session_rx) = tokio::sync::mpsc::unbounded_channel();
    let (drop_session_tx, _drop_session_rx) = tokio::sync::mpsc::unbounded_channel();
    let (rename_session_tx, _rename_session_rx) = tokio::sync::mpsc::unbounded_channel();
    let (restart_tx, _restart_rx) = tokio::sync::mpsc::unbounded_channel();
    let debug_capture = Arc::new(AtomicBool::new(false));
    let (master_tx, _master_rx) = tokio::sync::mpsc::unbounded_channel();
    App::new(
        prompt_tx,
        recommendation_tx,
        permission_tx,
        cancel_tx,
        new_session_tx,
        load_session_tx,
        drop_session_tx,
        rename_session_tx,
        restart_tx,
        master_tx,
        debug_capture,
        true,
        false,
        Arc::new(crate::shell::ShellManager::new()),
    )
}

fn agent_paste_params(window_id: &str, tab_id: &str) -> serde_json::Value {
    json!({
        "window_id": window_id,
        "tab_id": tab_id,
    })
}

#[test]
fn agent_paste_text_normalizes_and_filters_control_chars() {
    assert_eq!(
        normalize_agent_paste_text("a\r\nb\rc\n\u{0085}d\u{2028}e\u{2029}f"),
        "a\nb\nc\n\nd\ne\nf"
    );
    assert_eq!(
        normalize_agent_paste_text("ok\u{0000}\u{001b}\u{0007}\tΩ\u{202E}x"),
        "ok\tΩx",
        "paste sanitizer must preserve tabs/text but strip controls and bidi overrides"
    );
}

#[test]
fn agent_paste_text_inserts_into_owner_chat_input_without_submitting() {
    let mut app = test_app();
    app.window_id = Some("w1".into());
    app.owner_tab_id = Some("tab-a".into());
    app.tab_id = Some("tab-a".into());
    app.tab_mut("tab-a");
    let pasted = format!("{}\r\n{}", "alpha", "beta");
    let expected = ["alpha", "beta"].join("\n");

    app.insert_agent_paste_text("tab-a", 0, &pasted);

    let tab = app.tab_sessions.get("tab-a").expect("target tab exists");
    assert_eq!(tab.input, expected);
    assert_eq!(tab.cursor_pos, tab.input.len());
    assert!(tab.messages.iter().all(|m| !matches!(m, ChatMessage::User(_))));
}

#[test]
fn agent_paste_text_inserts_at_cursor() {
    let mut app = test_app();
    app.window_id = Some("w1".into());
    app.owner_tab_id = Some("tab-a".into());
    app.tab_id = Some("tab-a".into());
    {
        let tab = app.tab_mut("tab-a");
        tab.input = "ab".into();
        tab.cursor_pos = 1;
        tab.paste_pending = true;
    }

    app.insert_agent_paste_text("tab-a", 0, "X\nY");

    let tab = app.tab_sessions.get("tab-a").expect("target tab exists");
    assert_eq!(tab.input, "aX\nYb");
    assert_eq!(tab.cursor_pos, "aX\nY".len());
    assert!(!tab.paste_pending);
}

#[test]
fn agent_paste_text_ignores_wrong_window_and_non_owner_helpers() {
    let mut app = test_app();
    app.window_id = Some("w1".into());
    app.owner_tab_id = Some("tab-a".into());
    app.tab_id = Some("tab-a".into());
    app.tab_mut("tab-a");

    assert_eq!(app.agent_paste_target_tab(&agent_paste_params("w2", "tab-a")), None);
    assert_eq!(app.agent_paste_target_tab(&agent_paste_params("w1", "tab-b")), None);

    assert!(app.tab_sessions.get("tab-a").unwrap().input.is_empty());
    assert!(
        app.tab_sessions
            .get("tab-b")
            .map(|t| t.input.is_empty())
            .unwrap_or(true),
        "non-owner helper must not create a phantom draft for another tab"
    );
}

#[test]
fn agent_paste_text_ignores_missing_owner_or_window() {
    let mut app = test_app();
    app.window_id = Some("w1".into());
    app.owner_tab_id = None;
    assert_eq!(app.agent_paste_target_tab(&agent_paste_params("w1", "tab-a")), None);
    assert!(app.tab_sessions.get("tab-a").map(|t| t.input.is_empty()).unwrap_or(true));

    app.owner_tab_id = Some("tab-a".into());
    let missing_window = json!({ "tab_id": "tab-a" });
    assert_eq!(app.agent_paste_target_tab(&missing_window), None);
}

#[test]
fn agent_paste_text_allows_unknown_helper_window_when_owner_matches() {
    let mut app = test_app();
    app.owner_tab_id = Some("tab-a".into());
    app.window_id = None;
    assert_eq!(
        app.agent_paste_target_tab(&agent_paste_params("w1", "tab-a")),
        Some("tab-a")
    );
}

#[test]
fn agent_paste_text_ignores_non_chat_or_non_live_input() {
    let mut app = test_app();
    app.window_id = Some("w1".into());
    app.owner_tab_id = Some("tab-a".into());
    app.tab_id = Some("tab-a".into());
    app.tab_mut("tab-a").current_view = View::Agents;

    app.insert_agent_paste_text("tab-a", 0, "hidden");
    assert!(app.tab_sessions.get("tab-a").unwrap().input.is_empty());

    app.tab_mut("tab-a").current_view = View::Chat;
    app.tab_mut("tab-a").completed_turns.push(CompletedTurn {
        prompt: "old".into(),
        details: Vec::new(),
        expanded: false,
        trailing_marker: None,
    });
    app.tab_mut("tab-a").selected_completed_turn_idx = Some(0);

    app.insert_agent_paste_text("tab-a", 0, "locked");
    assert!(app.tab_sessions.get("tab-a").unwrap().input.is_empty());
}

#[test]
fn agent_paste_input_live_requires_existing_chat_input_focus() {
    let mut app = test_app();
    assert!(!app.agent_paste_input_is_live("tab-a"));

    app.tab_mut("tab-a");
    assert!(app.agent_paste_input_is_live("tab-a"));

    app.tab_mut("tab-a").paste_pending = true;
    assert!(!app.agent_paste_input_is_live("tab-a"));
    app.tab_mut("tab-a").paste_pending = false;

    app.tab_mut("tab-a").current_view = View::Agents;
    assert!(!app.agent_paste_input_is_live("tab-a"));

    app.tab_mut("tab-a").current_view = View::Chat;
    app.tab_mut("tab-a").completed_turns.push(CompletedTurn {
        prompt: "old".into(),
        details: Vec::new(),
        expanded: false,
        trailing_marker: None,
    });
    app.tab_mut("tab-a").selected_completed_turn_idx = Some(0);
    assert!(!app.agent_paste_input_is_live("tab-a"));

    app.tab_mut("tab-a").selected_completed_turn_idx = None;
    app.tab_mut("tab-a").model_picker_open = true;
    assert!(!app.agent_paste_input_is_live("tab-a"));
    app.tab_mut("tab-a").model_picker_open = false;

    app.tab_mut("tab-a").agent_picker_open = true;
    assert!(!app.agent_paste_input_is_live("tab-a"));
}

#[test]
fn agent_paste_failure_clears_pending_state() {
    let mut app = test_app();
    app.tab_mut("tab-a").paste_pending = true;
    app.tab_mut("tab-a").paste_generation = 1;

    app.handle_event(AppEvent::AgentPasteTextFailed {
        tab_id: "tab-a".into(),
        generation: 1,
        error: "clipboard busy".into(),
    });

    assert!(!app.tab_sessions.get("tab-a").unwrap().paste_pending);
}

#[test]
fn stale_agent_paste_completion_is_ignored() {
    let mut app = test_app();
    app.mode = AppMode::Chat;
    app.tab_mut("tab-a").paste_pending = true;
    app.tab_mut("tab-a").paste_generation = 2;

    app.handle_event(AppEvent::AgentPasteTextReady {
        tab_id: "tab-a".into(),
        generation: 1,
        text: "stale".into(),
    });

    let tab = app.tab_sessions.get("tab-a").unwrap();
    assert!(tab.input.is_empty());
    assert!(tab.paste_pending, "stale completion must not clear a newer pending paste");
}

#[test]
fn agent_paste_text_ignores_auth_and_setup_modes_before_reading_clipboard() {
    let mut app = test_app();
    app.window_id = Some("w1".into());
    app.owner_tab_id = Some("tab-a".into());
    app.tab_id = Some("tab-a".into());

    app.mode = AppMode::Auth;
    app.handle_event(AppEvent::WtEvent {
        method: "agent_paste_text".into(),
        pane_id: String::new(),
        tab_id: Some("tab-a".into()),
        params: agent_paste_params("w1", "tab-a"),
    });
    assert!(app.tab_sessions.get("tab-a").map(|t| t.input.is_empty()).unwrap_or(true));

    app.mode = AppMode::Setup;
    app.handle_event(AppEvent::WtEvent {
        method: "agent_paste_text".into(),
        pane_id: String::new(),
        tab_id: Some("tab-a".into()),
        params: agent_paste_params("w1", "tab-a"),
    });
    assert!(app.tab_sessions.get("tab-a").map(|t| t.input.is_empty()).unwrap_or(true));
}

#[test]
fn copilot_sidekick_hook_session_is_ignored() {
    use crate::agent_sessions::{AgentSessionRegistry, SessionEvent};

    let mut reg = AgentSessionRegistry::new();
    let params = json!({
        "event": "agent.prompt.submit",
        "cli_source": "copilot",
        "agent_session_id": "sidekick-github-context-memory-1783651400639",
        "payload": { "cwd": r#"C:\Users\user"# }
    });
    let mut published = Vec::<SessionEvent>::new();

    let dirty = route_agent_event_to_registry_with_hook_sink(
        &mut reg,
        "11111111-1111-1111-1111-111111111111",
        &params,
        |event| published.push(event),
    );

    assert!(!dirty, "an internal sidekick event must not dirty the registry");
    assert!(
        reg.iter_sorted().is_empty(),
        "an internal sidekick must not create a session row"
    );
    assert!(published.is_empty(), "an internal sidekick event must not reach master");
}

/// Bug-1 fix (PR #73 follow-up): an `agent.notification` hook event
/// arrives with neither `agent_session_id` nor a `pane_session_id`
/// resolving to a live session — exactly the shape Copilot CLI's
/// `Notification` hook emits (no `session_id` field in the JSON
/// payload AND no `WT_SESSION` inherited by the hook subprocess).
///
/// Before the fix, `resolve_or_synthesize_key` produces `pane:<x>`,
/// the reducer no-ops (synthetic session unknown) AND the synthetic
/// key gates the event out of the master publish path, so the row
/// stays at `Working` from the prior `tool.starting`.
///
/// After the fix, the routing layer falls back to the most-recently-
/// active live session for the same `cli_source` — the row flips to
/// `Attention` locally AND a real-key event is published to master.
#[test]
fn sessionless_notification_falls_back_to_recent_live_cli_session() {
    use crate::agent_sessions::{
        AgentSessionRegistry, AgentStatus, CliSource, SessionEvent,
    };
    let mut reg = AgentSessionRegistry::new();
    // One live Copilot session bound to a known pane.
    reg.apply(SessionEvent::SessionStarted {
        key: "real-copilot-sid".into(),
        cli_source: CliSource::Copilot,
        pane_session_id: "11111111-1111-1111-1111-111111111111".into(),
        cwd: std::path::PathBuf::from("/work"),
        title: "live copilot".into(),
    });
    reg.take_dirty();

    // Notification arrives with an UNRELATED active-pane GUID
    // (user focused on a different pane) and no agent_session_id —
    // mirrors the WT_SESSION-less Copilot hook trace.
    let unrelated_pane = "99999999-9999-9999-9999-999999999999";
    let params = json!({
        "event": "agent.notification",
        "cli_source": "copilot",
        "agent_session_id": "",  // missing — the bug shape
        "payload": { "message": "approve: rm -rf foo" }
    });

    let mut published: Vec<SessionEvent> = Vec::new();
    route_agent_event_to_registry_with_hook_sink(
        &mut reg,
        unrelated_pane,
        &params,
        |ev| published.push(ev),
    );

    // Local reducer flipped the real row to Attention.
    let s = reg.get(&"real-copilot-sid".to_string()).expect("row preserved");
    assert_eq!(
        s.status,
        AgentStatus::Attention,
        "fallback must route the Notification to the live Copilot row",
    );
    assert_eq!(s.attention_reason.as_deref(), Some("approve: rm -rf foo"));

    // Master got a real-key (not synthetic `pane:`) Notification.
    let notif_to_master = published.iter().find_map(|ev| match ev {
        SessionEvent::Notification { key, .. } => Some(key.clone()),
        _ => None,
    });
    assert_eq!(
        notif_to_master.as_deref(),
        Some("real-copilot-sid"),
        "Notification must be published to master keyed by the real session id; \
         synthetic `pane:` keys are dropped from the publish path",
    );
    assert!(
        !published.iter().any(|ev| matches!(
            ev,
            SessionEvent::Notification { key, .. } if key.starts_with("pane:")
        )),
        "no synthetic-key Notification should leak to master",
    );
}

/// Turn-based hook status (multi-tool turn bug): Copilot/Gemini fire a
/// `tool.finished` per tool — several per turn, in parallel batches — but
/// the agent keeps working until `agent.stop`. A `tool.finished` must NOT
/// demote the row to Idle (only `agent.stop` ends the turn); otherwise a
/// multi-tool turn flickers to (and sits at) Idle while the agent is busy.
#[test]
fn copilot_tool_finished_keeps_working_only_agent_stop_idles() {
    use crate::agent_sessions::{
        AgentSessionRegistry, AgentStatus, CliSource, SessionEvent,
    };
    let mut reg = AgentSessionRegistry::new();
    let pane = "11111111-1111-1111-1111-111111111111";
    let sid = "copilot-sid";
    reg.apply(SessionEvent::SessionStarted {
        key: sid.into(),
        cli_source: CliSource::Copilot,
        pane_session_id: pane.into(),
        cwd: std::path::PathBuf::from("/work"),
        title: "copilot".into(),
    });
    reg.take_dirty();

    let route = |reg: &mut AgentSessionRegistry, event: &str| {
        let params = json!({
            "event": event,
            "cli_source": "copilot",
            "agent_session_id": sid,
            "payload": { "tool_name": "read_file" }
        });
        route_agent_event_to_registry_with_hook_sink(reg, pane, &params, |_| {});
    };

    // User prompt → Working (turn start).
    route(&mut reg, "agent.prompt.submit");
    assert_eq!(reg.get(&sid.to_string()).unwrap().status, AgentStatus::Working);

    // A parallel batch: three starts, then three finishes.
    route(&mut reg, "agent.tool.starting");
    route(&mut reg, "agent.tool.starting");
    route(&mut reg, "agent.tool.starting");
    assert_eq!(reg.get(&sid.to_string()).unwrap().status, AgentStatus::Working);
    route(&mut reg, "agent.tool.finished");
    assert_eq!(
        reg.get(&sid.to_string()).unwrap().status,
        AgentStatus::Working,
        "first tool.finished must NOT demote while siblings run / the turn continues",
    );
    route(&mut reg, "agent.tool.finished");
    route(&mut reg, "agent.tool.finished");
    assert_eq!(
        reg.get(&sid.to_string()).unwrap().status,
        AgentStatus::Working,
        "tool completions never end the turn",
    );

    // Only agent.stop ends the turn → Idle.
    route(&mut reg, "agent.stop");
    assert_eq!(
        reg.get(&sid.to_string()).unwrap().status,
        AgentStatus::Idle,
        "agent.stop owns the turn-end → Idle",
    );
}

/// Counterpart guard: when the event carries a real `agent_session_id`,
/// the fallback must NOT replace it — the explicit session id always
/// wins over the heuristic.
#[test]
fn notification_with_real_session_id_skips_fallback() {
    use crate::agent_sessions::{
        AgentSessionRegistry, AgentStatus, CliSource, SessionEvent,
    };
    let mut reg = AgentSessionRegistry::new();
    // Two Copilot sessions; `target` is the explicit one in the hook,
    // `other` is the most-recently-active and would win the fallback.
    reg.apply(SessionEvent::SessionStarted {
        key: "target".into(),
        cli_source: CliSource::Copilot,
        pane_session_id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".into(),
        cwd: std::path::PathBuf::from("/work"),
        title: "target".into(),
    });
    std::thread::sleep(std::time::Duration::from_millis(5));
    reg.apply(SessionEvent::SessionStarted {
        key: "other".into(),
        cli_source: CliSource::Copilot,
        pane_session_id: "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".into(),
        cwd: std::path::PathBuf::from("/work"),
        title: "other".into(),
    });
    reg.take_dirty();

    let params = json!({
        "event": "agent.notification",
        "cli_source": "copilot",
        "agent_session_id": "target",
        "payload": { "message": "explicit" }
    });
    let unrelated_pane = "99999999-9999-9999-9999-999999999999";
    route_agent_event_to_registry_with_hook_sink(
        &mut reg, unrelated_pane, &params, |_| {},
    );

    assert_eq!(
        reg.get(&"target".to_string()).unwrap().status,
        AgentStatus::Attention,
        "explicit session id must win over the fallback heuristic",
    );
    assert_ne!(
        reg.get(&"other".to_string()).unwrap().status,
        AgentStatus::Attention,
        "fallback target must NOT be touched when explicit sid was supplied",
    );
}

/// The fallback must refuse to act when `cli_source` is `Unknown`
/// (no trustworthy CLI hint); otherwise a sessionless event from an
/// unknown source could land on whichever live session happened to be
/// the most recent across ALL CLIs.
#[test]
fn sessionless_notification_with_unknown_cli_does_not_fall_back() {
    use crate::agent_sessions::{
        AgentSessionRegistry, AgentStatus, CliSource, SessionEvent,
    };
    let mut reg = AgentSessionRegistry::new();
    reg.apply(SessionEvent::SessionStarted {
        key: "copilot".into(),
        cli_source: CliSource::Copilot,
        pane_session_id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".into(),
        cwd: std::path::PathBuf::from("/work"),
        title: "live".into(),
    });

    // No cli_source field at all → CliSource::Unknown — fallback
    // must NOT pick the only live row.
    let params = json!({
        "event": "agent.notification",
        "agent_session_id": "",
        "payload": { "message": "approve?" }
    });
    let _ = route_agent_event_to_registry_with_hook_sink(
        &mut reg,
        "99999999-9999-9999-9999-999999999999",
        &params,
        |_| {},
    );

    assert_ne!(
        reg.get(&"copilot".to_string()).unwrap().status,
        AgentStatus::Attention,
        "fallback must require a trustworthy cli_source hint to avoid \
         routing sessionless events into unrelated CLIs",
    );
}

#[test]
fn session_info_to_agent_session_preserves_live_agent_pane_session_fields() {
    // Regression: master's new_session/load_session handlers stamp
    // status=Idle, cli_source=<resolved>, origin=AgentPane on the
    // SessionInfo so helper-side session management routing sees a Live row. Without
    // this stamping the row would land with all fields None, the
    // converter would map status=None -> AgentStatus::Historical (its
    // documented default), and Enter would fall through to the resume
    // path and fail with "unknown CLI" since cli_source is also None.
    let mut info = crate::session_registry::SessionInfo::new(
        agent_client_protocol::schema::v1::SessionId::new("sid-live"),
        std::path::PathBuf::from("/repo"),
    );
    info.pane_session_id = Some("pane-live".to_string());
    info.status = Some(crate::agent_sessions::AgentStatus::Idle);
    info.cli_source = Some(crate::agent_sessions::CliSource::Copilot);
    info.origin = Some(crate::agent_sessions::SessionOrigin::AgentPane);
    let s = crate::app::session_info_to_agent_session(&info);
    assert_eq!(s.status, crate::agent_sessions::AgentStatus::Idle);
    assert_eq!(s.cli_source, crate::agent_sessions::CliSource::Copilot);
    assert_eq!(s.origin, crate::agent_sessions::SessionOrigin::AgentPane);
    assert_eq!(s.pane_session_id.as_deref(), Some("pane-live"));
}

#[test]
fn session_info_to_agent_session_uses_cwd_fallback_for_opencode_placeholder() {
    let mut info = crate::session_registry::SessionInfo::new(
        agent_client_protocol::schema::v1::SessionId::new("sid-opencode"),
        std::path::PathBuf::from(r"C:\repo\project"),
    );
    info.cli_source = Some(crate::agent_sessions::CliSource::OpenCode);
    info.title = Some("New session - 2026-07-23T01:14:00.422Z".to_string());

    let session = crate::app::session_info_to_agent_session(&info);

    assert!(session.title.is_empty());
    assert_eq!(
        session.cwd.file_name().and_then(|name| name.to_str()),
        Some("project")
    );
}

#[test]
fn session_info_to_agent_session_unstamped_row_falls_to_historical() {
    // Defensive: SessionInfo with all metadata None (the master-side
    // bug we're guarding against) deliberately maps status -> Historical
    // and cli_source -> Unknown(""). This is the WRONG end-state for a
    // Live row but matches the documented fallback. If we ever change
    // the fallback (e.g. to Idle/None) update the docstring on
    // session_info_to_agent_session AND on the master handler
    // comments — silently flipping defaults will mask future bugs.
    let info = crate::session_registry::SessionInfo::new(
        agent_client_protocol::schema::v1::SessionId::new("sid-bare"),
        std::path::PathBuf::from("/repo"),
    );
    let s = crate::app::session_info_to_agent_session(&info);
    assert_eq!(s.status, crate::agent_sessions::AgentStatus::Historical);
    assert!(matches!(
        s.cli_source,
        crate::agent_sessions::CliSource::Unknown(ref v) if v.is_empty()
    ));
}

#[test]
fn helper_agent_event_queues_session_hook_while_updating_local_registry() {
    let mut app = test_app();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.set_session_hook_tx(tx);

    app.handle_event(AppEvent::WtEvent {
        method: "agent_event".to_string(),
        pane_id: "pane-hook".to_string(),
        tab_id: Some("tab-1".to_string()),
        params: json!({
            "event": "agent.session.started",
            "cli_source": "copilot",
            "agent_session_id": "sid-hook",
            "payload": {
                "cwd": r#"C:\repo\hook"#,
            }
        }),
    });

    let queued = rx.try_recv().expect("session_hook event queued");
    assert_eq!(
        queued,
        crate::agent_sessions::SessionEvent::SessionStarted {
            key: "sid-hook".to_string(),
            cli_source: crate::agent_sessions::CliSource::Copilot,
            pane_session_id: "pane-hook".to_string(),
            cwd: std::path::PathBuf::from(r#"C:\repo\hook"#),
            title: "hook".to_string(),
        }
    );
    assert!(
        app.agent_sessions.has_session(&"sid-hook".to_string()),
        "local registry mutation remains in place"
    );
}

#[test]
fn helper_agent_event_queues_synthetic_start_and_followup_hook() {
    let mut app = test_app();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.set_session_hook_tx(tx);

    app.handle_event(AppEvent::WtEvent {
        method: "agent_event".to_string(),
        pane_id: "pane-tool".to_string(),
        tab_id: Some("tab-1".to_string()),
        params: json!({
            "event": "agent.tool.starting",
            "cli_source": "copilot",
            "agent_session_id": "sid-tool",
            "payload": {
                "cwd": r#"C:\repo\tool"#,
                "tool_name": "edit"
            }
        }),
    });

    assert!(matches!(
        rx.try_recv().expect("synthetic SessionStarted queued"),
        crate::agent_sessions::SessionEvent::SessionStarted { ref key, .. } if key == "sid-tool"
    ));
    assert_eq!(
        rx.try_recv().expect("ToolStarting queued"),
        crate::agent_sessions::SessionEvent::ToolStarting {
            key: "sid-tool".to_string(),
            tool_name: "edit".to_string(),
        }
    );
}

#[test]
fn helper_agent_event_without_agent_session_id_does_not_publish_synthetic_to_master() {
    // Regression for the user-reported duplicate session management row:
    //   "system32  Error                          29 minutes ago"
    //   "Agent pane session b832a8d3: system32  Active · copilot"
    //
    // When an agent_event arrives with no agent_session_id (broken
    // hook, race, or hook from a workspace shell pane that doesn't
    // own an ACP session), the helper used to synthesize a
    // `pane:<guid>` placeholder, apply it locally, AND publish it to
    // master. Master then surfaced the placeholder as a separate
    // session management row alongside the real session, both pointing
    // at the same
    // underlying pane — hence the duplicate.
    //
    // Fix: keep the synthetic placeholder local for helper
    // bookkeeping (is_agent_pane / OSC handler), but DO NOT publish
    // events with `pane:<guid>` keys to master.
    let mut app = test_app();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.set_session_hook_tx(tx);

    // Tool event with NO agent_session_id, NO existing pane binding
    // → resolve_or_synthesize_key returns "pane:<guid>", synthetic
    // placeholder created locally, but nothing published to master.
    app.handle_event(AppEvent::WtEvent {
        method: "agent_event".to_string(),
        pane_id: "pane-orphan".to_string(),
        tab_id: Some("tab-1".to_string()),
        params: json!({
            "event": "agent.tool.starting",
            "cli_source": "copilot",
            "payload": {
                "cwd": r#"C:\repo\hook"#,
                "tool_name": "edit"
            }
        }),
    });

    assert!(
        rx.try_recv().is_err(),
        "synthetic pane:<guid> events must NOT be published to master"
    );
    // Local registry still has the placeholder for helper-side
    // is_agent_pane / OSC handler bookkeeping.
    assert!(app.agent_sessions.is_agent_pane("pane-orphan"));
}

#[test]
fn helper_agent_event_with_real_agent_session_id_still_publishes_to_master() {
    // Defense against overcorrection: the synthetic-key gate above
    // must not block legitimate events with real agent_session_ids.
    let mut app = test_app();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    app.set_session_hook_tx(tx);

    app.handle_event(AppEvent::WtEvent {
        method: "agent_event".to_string(),
        pane_id: "pane-real".to_string(),
        tab_id: Some("tab-1".to_string()),
        params: json!({
            "event": "agent.tool.starting",
            "cli_source": "copilot",
            "agent_session_id": "real-sid-deadbeef",
            "payload": {
                "cwd": r#"C:\repo\hook"#,
                "tool_name": "edit"
            }
        }),
    });

    // Should publish at least one event (likely synthetic
    // SessionStarted + ToolStarting). Both must have the REAL key.
    let mut count = 0;
    while let Ok(evt) = rx.try_recv() {
        match evt {
            crate::agent_sessions::SessionEvent::SessionStarted { key, .. } => {
                assert_eq!(key, "real-sid-deadbeef", "real session id preserved");
                count += 1;
            }
            crate::agent_sessions::SessionEvent::ToolStarting { key, .. } => {
                assert_eq!(key, "real-sid-deadbeef", "real session id preserved");
                count += 1;
            }
            other => panic!("unexpected event: {:?}", other),
        }
    }
    assert!(count >= 1, "at least one real-keyed event must reach master");
}

fn test_app_with_master_rx() -> (
    App,
    tokio::sync::mpsc::UnboundedReceiver<crate::protocol::acp::client::MasterExtRequest>,
) {
    let (prompt_tx, _prompt_rx) = tokio::sync::mpsc::unbounded_channel();
    let (recommendation_tx, _recommendation_rx) = tokio::sync::mpsc::unbounded_channel();
    let (permission_tx, _permission_rx) = tokio::sync::mpsc::unbounded_channel();
    let (cancel_tx, _cancel_rx) = tokio::sync::mpsc::unbounded_channel();
    let (new_session_tx, _new_session_rx) = tokio::sync::mpsc::unbounded_channel();
    let (load_session_tx, _load_session_rx) = tokio::sync::mpsc::unbounded_channel();
    let (drop_session_tx, _drop_session_rx) = tokio::sync::mpsc::unbounded_channel();
    let (rename_session_tx, _rename_session_rx) = tokio::sync::mpsc::unbounded_channel();
    let (restart_tx, _restart_rx) = tokio::sync::mpsc::unbounded_channel();
    let (master_tx, master_rx) = tokio::sync::mpsc::unbounded_channel();
    let debug_capture = Arc::new(AtomicBool::new(false));
    let app = App::new(
        prompt_tx,
        recommendation_tx,
        permission_tx,
        cancel_tx,
        new_session_tx,
        load_session_tx,
        drop_session_tx,
        rename_session_tx,
        restart_tx,
        master_tx,
        debug_capture,
        true,
        false,
        Arc::new(crate::shell::ShellManager::new()),
    );
    (app, master_rx)
}

// ─── word boundary helpers ──────────────────────────────────────────────

#[test]
fn next_word_jumps_to_end_of_current_then_next_word() {
    let s = "hello world";
    // Start of input → end of "hello".
    assert_eq!(next_word_boundary(s, 0), 5);
    // Inside "hello" → end of "hello".
    assert_eq!(next_word_boundary(s, 2), 5);
    // On the space → end of "world".
    assert_eq!(next_word_boundary(s, 5), 11);
    // End of input → stays.
    assert_eq!(next_word_boundary(s, 11), 11);
}

#[test]
fn prev_word_jumps_to_start_of_current_then_previous_word() {
    let s = "hello world";
    // End of input → start of "world".
    assert_eq!(prev_word_boundary(s, 11), 6);
    // On 'w' → start of "hello".
    assert_eq!(prev_word_boundary(s, 6), 0);
    // Inside "world" → start of "world".
    assert_eq!(prev_word_boundary(s, 9), 6);
    // Start of input → stays.
    assert_eq!(prev_word_boundary(s, 0), 0);
}

#[test]
fn word_boundary_skips_punctuation_runs() {
    let s = "foo --bar baz";
    // After "foo" → skip space + "--", land at end of "bar".
    assert_eq!(next_word_boundary(s, 3), 9);
    // From end of "bar" backwards → start of "bar".
    assert_eq!(prev_word_boundary(s, 9), 6);
}

#[test]
fn word_boundary_handles_multibyte_chars() {
    // "你好 world" — each Chinese char is 3 bytes in UTF-8.
    let s = "你好 world";
    assert_eq!(s.len(), 12);
    // Start → end of "你好" (after 2 CJK chars = byte 6).
    assert_eq!(next_word_boundary(s, 0), 6);
    // From end → start of "world" at byte 7.
    assert_eq!(prev_word_boundary(s, 12), 7);
    // From byte 7 (start of "world") → start of "你好" at byte 0.
    assert_eq!(prev_word_boundary(s, 7), 0);
}

#[test]
fn word_boundary_handles_newlines() {
    let s = "foo\nbar";
    // From start → end of "foo".
    assert_eq!(next_word_boundary(s, 0), 3);
    // On '\n' → end of "bar".
    assert_eq!(next_word_boundary(s, 3), 7);
    // From end → start of "bar".
    assert_eq!(prev_word_boundary(s, 7), 4);
}

// ─── classify_wt_event ──────────────────────────────────────────────────

#[test]
fn classify_connection_failed_is_critical() {
    let params = json!({"session_id": "3", "state": "failed"});
    let n = classify_wt_event("connection_state", "3", None, &params);
    assert_eq!(n.severity, WtEventSeverity::Critical);
    assert!(n.summary.contains("failed"));
    assert!(!n.acknowledged);
}

#[test]
fn classify_connection_closed_is_actionable() {
    let params = json!({"session_id": "5", "state": "closed"});
    let n = classify_wt_event("connection_state", "5", None, &params);
    assert_eq!(n.severity, WtEventSeverity::Actionable);
    assert!(n.summary.contains("exited"));
}

#[test]
fn classify_connection_connected_is_informational() {
    let params = json!({"session_id": "1", "state": "connected"});
    let n = classify_wt_event("connection_state", "1", None, &params);
    assert_eq!(n.severity, WtEventSeverity::Informational);
    assert!(n.summary.contains("connected"));
}

#[test]
fn classify_osc133_command_failed_is_actionable() {
    let params = json!({"session_id": "2", "sequence": "osc:133;D;1"});
    let n = classify_wt_event("vt_sequence", "2", None, &params);
    assert_eq!(n.severity, WtEventSeverity::Actionable);
    assert!(n.summary.contains("Command failed"));
    assert!(n.summary.contains("exit 1"));
}

#[test]
fn classify_osc133_command_success_is_silent() {
    let params = json!({"session_id": "2", "sequence": "osc:133;D;0"});
    let n = classify_wt_event("vt_sequence", "2", None, &params);
    assert!(n.acknowledged); // auto-dismissed
}

#[test]
fn classify_osc133_high_exit_code() {
    let params = json!({"session_id": "2", "sequence": "osc:133;D;127"});
    let n = classify_wt_event("vt_sequence", "2", None, &params);
    assert_eq!(n.severity, WtEventSeverity::Actionable);
    assert!(n.summary.contains("exit 127"));
}

#[test]
fn classify_osc133_prompt_marker_is_silent() {
    // OSC 133;A is a prompt marker, not a command finish
    let params = json!({"session_id": "2", "sequence": "osc:133;A"});
    let n = classify_wt_event("vt_sequence", "2", None, &params);
    assert!(n.acknowledged); // silenced
}

#[test]
fn classify_normal_vt_sequence_is_silent() {
    let params = json!({"session_id": "7", "sequence": "osc:0;title"});
    let n = classify_wt_event("vt_sequence", "7", None, &params);
    assert!(n.acknowledged); // silenced
}

#[test]
fn classify_unknown_method_is_informational() {
    let params = json!({"session_id": "1"});
    let n = classify_wt_event("something_new", "1", None, &params);
    assert_eq!(n.severity, WtEventSeverity::Informational);
}

// ─── tab_renamed (tab-drag rekeying) ────────────────────────────────────

#[test]
fn tab_renamed_rekeys_active_tab_and_session_map() {
    let mut app = test_app();
    // Seed: active tab is AAAA with a bound ACP session.
    app.tab_id = Some("AAAA".to_string());
    app.tab_sessions
        .insert("AAAA".to_string(), TabSession::default());
    app.session_to_tab
        .insert("sess-1".to_string(), "AAAA".to_string());

    // Drive the rename via the WtEvent dispatch path — same code path
    // a real broadcast from the COM server takes.
    app.handle_event(AppEvent::WtEvent {
        method: "tab_renamed".to_string(),
        pane_id: String::new(),
        tab_id: None,
        params: json!({"old_tab_id": "AAAA", "new_tab_id": "BBBB"}),
    });

    assert_eq!(
        app.tab_id.as_deref(),
        Some("BBBB"),
        "active tab id must follow the rename"
    );
    assert!(
        app.tab_sessions.contains_key("BBBB"),
        "tab_sessions must contain the new key after rename"
    );
    assert!(
        !app.tab_sessions.contains_key("AAAA"),
        "tab_sessions must no longer contain the old key"
    );
    assert_eq!(
        app.session_to_tab.get("sess-1").map(String::as_str),
        Some("BBBB"),
        "session_to_tab values pointing at the old id must be rewritten"
    );
}

#[test]
fn tab_renamed_appevent_variant_drives_same_handler() {
    // Direct AppEvent::TabRenamed dispatch — used by callers that
    // already deserialized the params (mirrors the WtEvent inline
    // path).
    let mut app = test_app();
    app.tab_id = Some("AAAA".to_string());
    app.tab_sessions
        .insert("AAAA".to_string(), TabSession::default());

    app.handle_event(AppEvent::TabRenamed {
        old_tab_id: "AAAA".to_string(),
        new_tab_id: "CCCC".to_string(),
        new_window_id: None,
    });

    assert_eq!(app.tab_id.as_deref(), Some("CCCC"));
    assert!(app.tab_sessions.contains_key("CCCC"));
    assert!(!app.tab_sessions.contains_key("AAAA"));
}

#[test]
fn tab_renamed_sends_rename_session_request_to_acp_client() {
    // The chat-history side rekeys in-process, but tab_to_session
    // lives in the ACP client task — it has to be told to rekey via
    // the rename_session_tx channel. Without this signal, the next
    // prompt on the dragged tab can't find the old SessionId.
    let (prompt_tx, _prompt_rx) = tokio::sync::mpsc::unbounded_channel();
    let (recommendation_tx, _recommendation_rx) = tokio::sync::mpsc::unbounded_channel();
    let (permission_tx, _permission_rx) = tokio::sync::mpsc::unbounded_channel();
    let (cancel_tx, _cancel_rx) = tokio::sync::mpsc::unbounded_channel();
    let (new_session_tx, _new_session_rx) = tokio::sync::mpsc::unbounded_channel();
    let (load_session_tx, _load_session_rx) = tokio::sync::mpsc::unbounded_channel();
    let (drop_session_tx, _drop_session_rx) = tokio::sync::mpsc::unbounded_channel();
    let (rename_session_tx, mut rename_session_rx) = tokio::sync::mpsc::unbounded_channel();
    let (restart_tx, _restart_rx) = tokio::sync::mpsc::unbounded_channel();
    let debug_capture = Arc::new(AtomicBool::new(false));
    let (master_tx, _master_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        prompt_tx,
        recommendation_tx,
        permission_tx,
        cancel_tx,
        new_session_tx,
        load_session_tx,
        drop_session_tx,
        rename_session_tx,
        restart_tx,
        master_tx,
        debug_capture,
        true,
        false,
        Arc::new(crate::shell::ShellManager::new()),
    );

    app.tab_id = Some("AAAA".to_string());
    app.tab_sessions
        .insert("AAAA".to_string(), TabSession::default());

    app.handle_event(AppEvent::TabRenamed {
        old_tab_id: "AAAA".to_string(),
        new_tab_id: "BBBB".to_string(),
        new_window_id: None,
    });

    // The ACP client task should have received exactly one
    // RenameSessionRequest with the old/new ids — that's what makes
    // the dragged tab's chat history line up with the agent's turn
    // context after the drag.
    let req = rename_session_rx
        .try_recv()
        .expect("rename_session_tx must have received a request");
    assert_eq!(req.old_tab_id, "AAAA");
    assert_eq!(req.new_tab_id, "BBBB");
    assert!(
        rename_session_rx.try_recv().is_err(),
        "exactly one request should have been sent"
    );
}

#[test]
fn tab_renamed_noop_does_not_send_rename_session_request() {
    // A no-op rename (old == new) must not bother the ACP client —
    // there's nothing to rekey, and a spurious request would
    // needlessly grab the tab_to_session lock.
    let (prompt_tx, _prompt_rx) = tokio::sync::mpsc::unbounded_channel();
    let (recommendation_tx, _recommendation_rx) = tokio::sync::mpsc::unbounded_channel();
    let (permission_tx, _permission_rx) = tokio::sync::mpsc::unbounded_channel();
    let (cancel_tx, _cancel_rx) = tokio::sync::mpsc::unbounded_channel();
    let (new_session_tx, _new_session_rx) = tokio::sync::mpsc::unbounded_channel();
    let (load_session_tx, _load_session_rx) = tokio::sync::mpsc::unbounded_channel();
    let (drop_session_tx, _drop_session_rx) = tokio::sync::mpsc::unbounded_channel();
    let (rename_session_tx, mut rename_session_rx) = tokio::sync::mpsc::unbounded_channel();
    let (restart_tx, _restart_rx) = tokio::sync::mpsc::unbounded_channel();
    let debug_capture = Arc::new(AtomicBool::new(false));
    let (master_tx, _master_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        prompt_tx,
        recommendation_tx,
        permission_tx,
        cancel_tx,
        new_session_tx,
        load_session_tx,
        drop_session_tx,
        rename_session_tx,
        restart_tx,
        master_tx,
        debug_capture,
        true,
        false,
        Arc::new(crate::shell::ShellManager::new()),
    );

    app.tab_id = Some("AAAA".to_string());
    app.tab_sessions
        .insert("AAAA".to_string(), TabSession::default());

    app.handle_event(AppEvent::TabRenamed {
        old_tab_id: "AAAA".to_string(),
        new_tab_id: "AAAA".to_string(),
        new_window_id: None,
    });

    assert!(
        rename_session_rx.try_recv().is_err(),
        "no-op rename must not send a RenameSessionRequest"
    );
}

#[test]
fn tab_renamed_with_missing_fields_is_dropped() {
    let mut app = test_app();
    app.tab_id = Some("AAAA".to_string());
    app.tab_sessions
        .insert("AAAA".to_string(), TabSession::default());

    // Empty new_tab_id — must not corrupt state.
    app.handle_event(AppEvent::WtEvent {
        method: "tab_renamed".to_string(),
        pane_id: String::new(),
        tab_id: None,
        params: json!({"old_tab_id": "AAAA", "new_tab_id": ""}),
    });
    assert_eq!(
        app.tab_id.as_deref(),
        Some("AAAA"),
        "rename with empty new_tab_id must be dropped, leaving state untouched"
    );
    assert!(app.tab_sessions.contains_key("AAAA"));

    // Missing field entirely — must not corrupt state.
    app.handle_event(AppEvent::WtEvent {
        method: "tab_renamed".to_string(),
        pane_id: String::new(),
        tab_id: None,
        params: json!({"old_tab_id": "AAAA"}),
    });
    assert_eq!(app.tab_id.as_deref(), Some("AAAA"));
    assert!(app.tab_sessions.contains_key("AAAA"));
}

// ─── load_session owner_tab_id filter ───────────────────────────────────
//
// WT broadcasts `load_session` over shared COM, so every helper in every
// window receives it. Pre-PR-B, every helper would respond regardless of
// the target tab — the misroute at the heart of bug #1 (resume into a
// newly-spawned agent pane landed in the wrong helper). The filter
// ensures a helper only acts on a `load_session` whose `tab_id` matches
// its `owner_tab_id`. The legacy single-helper flow (no owner_tab_id)
// still works as before.

fn make_app_with_load_session_channel() -> (
    App,
    tokio::sync::mpsc::UnboundedReceiver<crate::protocol::acp::client::LoadSessionForTab>,
) {
    let (prompt_tx, _prompt_rx) = tokio::sync::mpsc::unbounded_channel();
    let (recommendation_tx, _recommendation_rx) = tokio::sync::mpsc::unbounded_channel();
    let (permission_tx, _permission_rx) = tokio::sync::mpsc::unbounded_channel();
    let (cancel_tx, _cancel_rx) = tokio::sync::mpsc::unbounded_channel();
    let (new_session_tx, _new_session_rx) = tokio::sync::mpsc::unbounded_channel();
    let (load_session_tx, load_session_rx) = tokio::sync::mpsc::unbounded_channel();
    let (drop_session_tx, _drop_session_rx) = tokio::sync::mpsc::unbounded_channel();
    let (rename_session_tx, _rename_session_rx) = tokio::sync::mpsc::unbounded_channel();
    let (restart_tx, _restart_rx) = tokio::sync::mpsc::unbounded_channel();
    let debug_capture = Arc::new(AtomicBool::new(false));
    let (master_tx, _master_rx) = tokio::sync::mpsc::unbounded_channel();
    let app = App::new(
        prompt_tx,
        recommendation_tx,
        permission_tx,
        cancel_tx,
        new_session_tx,
        load_session_tx,
        drop_session_tx,
        rename_session_tx,
        restart_tx,
        master_tx,
        debug_capture,
        true,
        false,
        Arc::new(crate::shell::ShellManager::new()),
    );
    (app, load_session_rx)
}

#[test]
fn load_session_ignored_when_target_tab_differs_from_owner() {
    let (mut app, mut load_session_rx) = make_app_with_load_session_channel();
    app.owner_tab_id = Some("OWNER-TAB".to_string());

    // Broadcast targeting a different tab — must NOT be forwarded
    // through the load_session_tx channel (otherwise the ACP client
    // would call session/load and bind the wrong tab).
    app.handle_event(AppEvent::WtEvent {
        method: "load_session".to_string(),
        pane_id: String::new(),
        tab_id: None,
        params: json!({
            "tab_id": "OTHER-TAB",
            "session_id": "sess-xyz",
            "cwd": "C:/foo",
        }),
    });

    assert!(
        load_session_rx.try_recv().is_err(),
        "load_session for non-owner tab must be silently dropped"
    );
}

#[test]
fn load_session_applied_when_target_tab_matches_owner() {
    let (mut app, mut load_session_rx) = make_app_with_load_session_channel();
    app.owner_tab_id = Some("OWNER-TAB".to_string());
    app.tab_sessions
        .insert("OWNER-TAB".to_string(), TabSession::default());

    app.handle_event(AppEvent::WtEvent {
        method: "load_session".to_string(),
        pane_id: String::new(),
        tab_id: None,
        params: json!({
            "tab_id": "OWNER-TAB",
            "session_id": "sess-abc",
            "cwd": "C:/foo",
        }),
    });

    let req = load_session_rx
        .try_recv()
        .expect("matching tab id must enqueue a LoadSessionForTab");
    assert_eq!(req.tab_id, "OWNER-TAB");
    assert_eq!(req.session_id, "sess-abc");
    assert_eq!(req.cwd.as_deref(), Some("C:/foo"));
}

#[test]
fn load_session_passes_through_when_owner_tab_id_unset() {
    // Legacy mode: helper spawned without `--owner-tab-id` (the
    // pre-multi-window code path). Filter must be transparent.
    let (mut app, mut load_session_rx) = make_app_with_load_session_channel();
    assert!(app.owner_tab_id.is_none());
    app.tab_sessions
        .insert("ANY-TAB".to_string(), TabSession::default());

    app.handle_event(AppEvent::WtEvent {
        method: "load_session".to_string(),
        pane_id: String::new(),
        tab_id: None,
        params: json!({
            "tab_id": "ANY-TAB",
            "session_id": "sess-legacy",
            "cwd": "",
        }),
    });

    let req = load_session_rx
        .try_recv()
        .expect("legacy mode must still forward load_session");
    assert_eq!(req.session_id, "sess-legacy");
}

// ─── SessionAttached load-target gating (Plan-C race fix) ───────────────

/// After a load_session sets the replay window open, an unrelated
/// `SessionAttached` (e.g. the bootstrap `session/new` that the helper
/// always runs at startup) MUST NOT close the window — otherwise
/// subsequent replay chunks for the real load target get dropped at
/// the chunk handlers' `if !loading_session { return; }` gate.
/// This is the exact race the Plan-C
/// `--initial-load-session-id` boot path was hitting (helper queued
/// the load_session via AppEvent before bootstrap completed, then
/// bootstrap SessionAttached arrived and prematurely closed the
/// window).
#[test]
fn session_attached_for_bootstrap_does_not_close_load_replay_window() {
    let (mut app, _load_session_rx) = make_app_with_load_session_channel();
    app.owner_tab_id = Some("OWNER-TAB".to_string());
    app.tab_sessions
        .insert("OWNER-TAB".to_string(), TabSession::default());

    // Open the replay window targeting "sess-target".
    app.handle_event(AppEvent::WtEvent {
        method: "load_session".to_string(),
        pane_id: String::new(),
        tab_id: None,
        params: json!({
            "tab_id": "OWNER-TAB",
            "session_id": "sess-target",
            "cwd": "",
        }),
    });
    assert!(app.tab_sessions["OWNER-TAB"].loading_session);
    assert_eq!(
        app.tab_sessions["OWNER-TAB"]
            .loading_target_session_id
            .as_deref(),
        Some("sess-target")
    );

    // Bootstrap `session/new` completes — SessionAttached for a
    // DIFFERENT session id arrives.
    app.handle_event(AppEvent::SessionAttached {
        tab_id: "OWNER-TAB".to_string(),
        session_id: "sess-bootstrap".to_string(),
        available_models: vec![],
        current_model_id: None,
    });

    // Window MUST still be open so replay chunks for sess-target
    // (which arrive after `session/load` actually runs) are accepted.
    assert!(
        app.tab_sessions["OWNER-TAB"].loading_session,
        "unrelated SessionAttached must not close the load_session replay window"
    );
    assert_eq!(
        app.tab_sessions["OWNER-TAB"]
            .loading_target_session_id
            .as_deref(),
        Some("sess-target"),
        "load target must persist across unrelated SessionAttached"
    );
}

/// SessionAttached for the actual load target DOES close the window
/// (the normal happy path — keep working).
#[test]
fn session_attached_for_load_target_closes_replay_window() {
    let (mut app, _load_session_rx) = make_app_with_load_session_channel();
    app.owner_tab_id = Some("OWNER-TAB".to_string());
    app.tab_sessions
        .insert("OWNER-TAB".to_string(), TabSession::default());

    app.handle_event(AppEvent::WtEvent {
        method: "load_session".to_string(),
        pane_id: String::new(),
        tab_id: None,
        params: json!({
            "tab_id": "OWNER-TAB",
            "session_id": "sess-target",
            "cwd": "",
        }),
    });
    assert!(app.tab_sessions["OWNER-TAB"].loading_session);

    app.handle_event(AppEvent::SessionAttached {
        tab_id: "OWNER-TAB".to_string(),
        session_id: "sess-target".to_string(),
        available_models: vec![],
        current_model_id: None,
    });

    assert!(
        !app.tab_sessions["OWNER-TAB"].loading_session,
        "SessionAttached for the load target must close the window"
    );
    assert!(
        app.tab_sessions["OWNER-TAB"]
            .loading_target_session_id
            .is_none(),
        "target id must be cleared after window closes"
    );
}

/// TabError must clear both flags so a subsequent load can re-open
/// the window cleanly.
#[test]
fn tab_error_clears_load_target() {
    let (mut app, _load_session_rx) = make_app_with_load_session_channel();
    app.owner_tab_id = Some("OWNER-TAB".to_string());
    app.tab_sessions
        .insert("OWNER-TAB".to_string(), TabSession::default());

    app.handle_event(AppEvent::WtEvent {
        method: "load_session".to_string(),
        pane_id: String::new(),
        tab_id: None,
        params: json!({
            "tab_id": "OWNER-TAB",
            "session_id": "sess-target",
            "cwd": "",
        }),
    });
    assert!(app.tab_sessions["OWNER-TAB"].loading_session);

    app.handle_event(AppEvent::TabError {
        tab_id: "OWNER-TAB".to_string(),
        message: "agent rejected load_session".to_string(),
    });

    assert!(!app.tab_sessions["OWNER-TAB"].loading_session);
    assert!(app.tab_sessions["OWNER-TAB"]
        .loading_target_session_id
        .is_none());
}

/// Replayed history must be packed into collapsed CompletedTurn rows
/// after session/load completes. Each User message opens a new turn;
/// the prompt header is a short preview (the full original User text
/// is kept as the first details entry so expanding shows everything).
/// Subsequent non-User messages become later details. Default
/// `expanded: false` so the resumed transcript doesn't dump as one
/// long wall.
#[test]
fn pack_replayed_messages_groups_into_collapsed_turns() {
    let mut tab = TabSession::default();
    tab.messages = vec![
        ChatMessage::System("Resuming session abc...".to_string()),
        ChatMessage::User("# Terminal Agent\nYou are...".to_string()),
        ChatMessage::Agent("Hello, I am ready.".to_string()),
        ChatMessage::User("list files".to_string()),
        ChatMessage::ToolCall {
            id: "t1".to_string(),
            title: "ls".to_string(),
            status: "done".to_string(),
        },
        ChatMessage::Agent("Here are the files...".to_string()),
    ];

    tab.pack_replayed_messages_into_turns();

    // System marker stays — it's not anchored to a User.
    assert_eq!(tab.messages.len(), 1);
    assert!(matches!(&tab.messages[0], ChatMessage::System(s) if s.starts_with("Resuming")));

    // Two turns: one per User prompt.
    assert_eq!(tab.completed_turns.len(), 2);

    let t0 = &tab.completed_turns[0];
    // Preview shows first non-empty line + ellipsis (extra lines below).
    assert_eq!(t0.prompt, "# Terminal Agent…");
    // details = [original full User, Agent reply].
    assert_eq!(t0.details.len(), 2);
    assert!(matches!(&t0.details[0], ChatMessage::User(s) if s.starts_with("# Terminal Agent\nYou are")));
    assert!(matches!(&t0.details[1], ChatMessage::Agent(_)));
    assert!(!t0.expanded, "replayed turn must default to collapsed");
    assert!(t0.trailing_marker.is_none());

    let t1 = &tab.completed_turns[1];
    // Short single-line prompt — no ellipsis.
    assert_eq!(t1.prompt, "list files");
    // details = [original User, ToolCall, Agent].
    assert_eq!(t1.details.len(), 3);
    assert!(matches!(&t1.details[0], ChatMessage::User(s) if s == "list files"));
    assert!(matches!(&t1.details[1], ChatMessage::ToolCall { .. }));
    assert!(matches!(&t1.details[2], ChatMessage::Agent(_)));
    assert!(!t1.expanded);
}

/// Preview logic: huge single-line prompt must clip to the cap with
/// a trailing ellipsis; short single-line prompts stay verbatim.
#[test]
fn collapsed_prompt_preview_clips_long_single_line() {
    let long = "a".repeat(500);
    let preview = collapsed_prompt_preview(&long);
    // 80 chars + ellipsis.
    assert_eq!(preview.chars().count(), 81);
    assert!(preview.ends_with('…'));

    let short = "hello world";
    assert_eq!(collapsed_prompt_preview(short), "hello world");
    assert!(!collapsed_prompt_preview(short).ends_with('…'));
}

/// Edge: messages that come BEFORE the first User must NOT be lost —
/// they stay in `tab.messages`. Pre-User stray Agent dumps (rare but
/// possible) should remain visible rather than being silently dropped.
#[test]
fn pack_replayed_messages_preserves_pre_user_orphans() {
    let mut tab = TabSession::default();
    tab.messages = vec![
        ChatMessage::System("Resuming...".to_string()),
        ChatMessage::Agent("stray context dump".to_string()),
        ChatMessage::User("hi".to_string()),
        ChatMessage::Agent("hello".to_string()),
    ];

    tab.pack_replayed_messages_into_turns();

    assert_eq!(tab.messages.len(), 2);
    assert!(matches!(&tab.messages[0], ChatMessage::System(_)));
    assert!(matches!(&tab.messages[1], ChatMessage::Agent(s) if s == "stray context dump"));
    assert_eq!(tab.completed_turns.len(), 1);
    assert_eq!(tab.completed_turns[0].prompt, "hi");
    assert!(!tab.completed_turns[0].expanded);
}

/// Empty messages must no-op (no panic, no spurious turn).
#[test]
fn pack_replayed_messages_empty_is_noop() {
    let mut tab = TabSession::default();
    tab.pack_replayed_messages_into_turns();
    assert!(tab.messages.is_empty());
    assert!(tab.completed_turns.is_empty());
}

/// Integration: SessionAttached for the load target must trigger
/// packing — replayed User/Agent rows must end up as collapsed
/// CompletedTurn entries, not loose ChatMessage rows.
#[test]
fn session_attached_for_load_target_packs_replayed_history() {
    let (mut app, _load_session_rx) = make_app_with_load_session_channel();
    app.owner_tab_id = Some("OWNER-TAB".to_string());
    app.tab_sessions
        .insert("OWNER-TAB".to_string(), TabSession::default());

    app.handle_event(AppEvent::WtEvent {
        method: "load_session".to_string(),
        pane_id: String::new(),
        tab_id: None,
        params: json!({
            "tab_id": "OWNER-TAB",
            "session_id": "sess-target",
            "cwd": "",
        }),
    });
    // Simulate replay chunks landing in messages.
    let tab = app.tab_sessions.get_mut("OWNER-TAB").unwrap();
    tab.messages.push(ChatMessage::User("first prompt".to_string()));
    tab.messages.push(ChatMessage::Agent("first reply".to_string()));
    tab.messages.push(ChatMessage::User("second prompt".to_string()));
    tab.messages.push(ChatMessage::Agent("second reply".to_string()));

    app.handle_event(AppEvent::SessionAttached {
        tab_id: "OWNER-TAB".to_string(),
        session_id: "sess-target".to_string(),
        available_models: vec![],
        current_model_id: None,
    });

    let tab = &app.tab_sessions["OWNER-TAB"];
    assert!(!tab.loading_session);
    assert_eq!(
        tab.completed_turns.len(),
        2,
        "both replayed user prompts must become collapsed CompletedTurn rows"
    );
    for turn in &tab.completed_turns {
        assert!(!turn.expanded, "replayed turns default collapsed");
    }
    // Resume is silent now — no "Resuming…" marker is posted, so after
    // packing the replayed User/Agent rows into turns nothing is left in
    // `messages`.
    assert!(
        tab.messages.is_empty(),
        "resume must not leave any loose chat messages, got {:?}",
        tab.messages
    );
}

// ─── WtNotification auto-dismiss ────────────────────────────────────────

#[test]
fn informational_auto_dismisses_after_threshold() {
    let mut n = WtNotification {
        severity: WtEventSeverity::Informational,
        pane_id: "1".to_string(),
        tab_id: None,
        summary: "test".to_string(),
        acknowledged: false,
        age_ticks: 0,
    };
    assert!(!n.should_auto_dismiss());
    n.age_ticks = 42;
    assert!(!n.should_auto_dismiss());
    n.age_ticks = 43;
    assert!(n.should_auto_dismiss());
}

#[test]
fn critical_never_auto_dismisses() {
    let n = WtNotification {
        severity: WtEventSeverity::Critical,
        pane_id: "1".to_string(),
        tab_id: None,
        summary: "crash".to_string(),
        acknowledged: false,
        age_ticks: 1000,
    };
    assert!(!n.should_auto_dismiss());
}

#[test]
fn actionable_never_auto_dismisses() {
    let n = WtNotification {
        severity: WtEventSeverity::Actionable,
        pane_id: "1".to_string(),
        tab_id: None,
        summary: "exited".to_string(),
        acknowledged: false,
        age_ticks: 1000,
    };
    assert!(!n.should_auto_dismiss());
}

// ─── App notification state ─────────────────────────────────────────────

#[test]
fn wt_event_critical_raises_banner_only_no_chat() {
    // WT events route through the bottom bar / `wt_notifications` queue,
    // never the agent's chat history. The chat is for agent dialogue;
    // process-lifecycle noise belongs in the bar.
    let mut app = test_app();
    app.handle_event(AppEvent::WtEvent {
        method: "connection_state".to_string(),
        pane_id: "3".to_string(),
        tab_id: None,
        params: json!({"session_id": "3", "state": "failed"}),
    });
    assert!(app.show_notification_banner);
    assert_eq!(app.wt_notifications.len(), 1);
    assert_eq!(app.wt_notifications[0].severity, WtEventSeverity::Critical);
    assert!(
        app.current_tab().messages.is_empty(),
        "WT events must not pollute chat history with Error messages"
    );
}

#[test]
fn wt_event_actionable_raises_banner_only_no_chat() {
    let mut app = test_app();
    app.handle_event(AppEvent::WtEvent {
        method: "connection_state".to_string(),
        pane_id: "5".to_string(),
        tab_id: None,
        params: json!({"session_id": "5", "state": "closed"}),
    });
    assert!(app.show_notification_banner);
    assert!(
        app.current_tab().messages.is_empty(),
        "WT events must not pollute chat history with System messages"
    );
}

#[test]
fn wt_event_informational_no_banner_no_chat_message() {
    let mut app = test_app();
    app.handle_event(AppEvent::WtEvent {
        method: "connection_state".to_string(),
        pane_id: "1".to_string(),
        tab_id: None,
        params: json!({"session_id": "1", "state": "connected"}),
    });
    assert!(!app.show_notification_banner);
    assert!(app.current_tab().messages.is_empty());
    assert_eq!(app.wt_notifications.len(), 1);
}

#[test]
fn wt_event_from_own_pane_is_ignored() {
    let mut app = test_app();
    app.pane_id = Some("42".to_string());
    app.handle_event(AppEvent::WtEvent {
        method: "connection_state".to_string(),
        pane_id: "42".to_string(),
        tab_id: None,
        params: json!({"session_id": "42", "state": "failed"}),
    });
    // Events from our own pane should be completely ignored
    assert!(!app.show_notification_banner);
    assert!(app.wt_notifications.is_empty());
    assert!(app.current_tab().messages.is_empty());
}

#[test]
fn wt_event_critical_from_other_tab_does_not_surface_in_owner_tab() {
    // Regression for the cross-tab "Pane …: connection failed" leak:
    // helper A owns tab A; tab B's Copilot pane fails; WT broadcasts
    // the `connection_state:failed` event to every helper. Helper A
    // must drop it instead of writing a red Error into tab A's chat.
    let mut app = test_app();
    app.owner_tab_id = Some("{tab-A}".to_string());
    app.handle_event(AppEvent::WtEvent {
        method: "connection_state".to_string(),
        pane_id: "B-PANE".to_string(),
        tab_id: Some("{tab-B}".to_string()),
        params: json!({"pane_id": "B-PANE", "state": "failed", "tab_id": "{tab-B}"}),
    });
    assert!(!app.show_notification_banner);
    assert!(app.wt_notifications.is_empty());
    assert!(app.current_tab().messages.is_empty());
}

#[test]
fn wt_event_critical_from_owner_tab_raises_banner_not_chat() {
    // Same-tab event raises the banner but still does NOT push into chat
    // — the bar is the user-visible surface for connection failures.
    let mut app = test_app();
    app.owner_tab_id = Some("{tab-A}".to_string());
    app.handle_event(AppEvent::WtEvent {
        method: "connection_state".to_string(),
        pane_id: "A-PANE".to_string(),
        tab_id: Some("{tab-A}".to_string()),
        params: json!({"pane_id": "A-PANE", "state": "failed", "tab_id": "{tab-A}"}),
    });
    assert!(app.show_notification_banner);
    assert_eq!(app.wt_notifications.len(), 1);
    assert!(
        app.current_tab().messages.is_empty(),
        "WT events must not pollute chat history"
    );
}

#[test]
fn dismiss_notifications_clears_banner_and_acknowledges() {
    let mut app = test_app();
    app.handle_event(AppEvent::WtEvent {
        method: "connection_state".to_string(),
        pane_id: "3".to_string(),
        tab_id: None,
        params: json!({"session_id": "3", "state": "failed"}),
    });
    assert!(app.show_notification_banner);
    assert_eq!(app.unacknowledged_count(), 1);

    app.dismiss_notifications();
    assert!(!app.show_notification_banner);
    assert_eq!(app.unacknowledged_count(), 0);
    assert!(app.wt_notifications[0].acknowledged);
}

#[test]
fn notification_badge_returns_most_recent_unacknowledged() {
    let mut app = test_app();
    // First event
    app.handle_event(AppEvent::WtEvent {
        method: "connection_state".to_string(),
        pane_id: "1".to_string(),
        tab_id: None,
        params: json!({"session_id": "1", "state": "closed"}),
    });
    // Second event (more recent)
    app.handle_event(AppEvent::WtEvent {
        method: "connection_state".to_string(),
        pane_id: "2".to_string(),
        tab_id: None,
        params: json!({"session_id": "2", "state": "failed"}),
    });

    let (summary, severity) = app.notification_badge().unwrap();
    assert!(summary.contains("Pane 2"));
    assert_eq!(*severity, WtEventSeverity::Critical);
    assert_eq!(app.unacknowledged_count(), 2);
}

#[test]
fn notification_queue_caps_at_20() {
    let mut app = test_app();
    for i in 0..25 {
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: format!("{}", i),
            tab_id: None,
            params: json!({"session_id": format!("{}", i), "state": "connected"}),
        });
    }
    assert_eq!(app.wt_notifications.len(), 20);
}

#[test]
fn tick_ages_and_auto_dismisses_informational() {
    let mut app = test_app();
    app.handle_event(AppEvent::WtEvent {
        method: "connection_state".to_string(),
        pane_id: "1".to_string(),
        tab_id: None,
        params: json!({"session_id": "1", "state": "connected"}),
    });
    assert_eq!(app.wt_notifications.len(), 1);
    assert_eq!(app.wt_notifications[0].age_ticks, 0);

    // Simulate enough ticks to trigger auto-dismiss (43 ticks)
    for _ in 0..43 {
        app.handle_event(AppEvent::Tick);
    }
    // Informational notification should be auto-removed
    assert_eq!(app.wt_notifications.len(), 0);
}

#[test]
fn tick_does_not_dismiss_critical_notifications() {
    let mut app = test_app();
    app.handle_event(AppEvent::WtEvent {
        method: "connection_state".to_string(),
        pane_id: "3".to_string(),
        tab_id: None,
        params: json!({"session_id": "3", "state": "failed"}),
    });
    // Simulate many ticks
    for _ in 0..200 {
        app.handle_event(AppEvent::Tick);
    }
    // Critical notification should persist
    assert_eq!(app.wt_notifications.len(), 1);
    assert!(app.show_notification_banner);
}

#[test]
fn banner_hides_when_all_acknowledged() {
    let mut app = test_app();
    app.handle_event(AppEvent::WtEvent {
        method: "connection_state".to_string(),
        pane_id: "3".to_string(),
        tab_id: None,
        params: json!({"session_id": "3", "state": "failed"}),
    });
    assert!(app.show_notification_banner);

    // Acknowledge all
    app.dismiss_notifications();

    // One more tick to process the banner-hide logic
    app.handle_event(AppEvent::Tick);
    assert!(!app.show_notification_banner);
}

#[test]
fn active_notification_returns_none_when_all_acknowledged() {
    let mut app = test_app();
    app.handle_event(AppEvent::WtEvent {
        method: "connection_state".to_string(),
        pane_id: "3".to_string(),
        tab_id: None,
        params: json!({"session_id": "3", "state": "closed"}),
    });
    assert!(app.active_notification().is_some());

    app.dismiss_notifications();
    assert!(app.active_notification().is_none());
}

#[test]
fn multiple_events_different_panes() {
    let mut app = test_app();
    // Informational from pane 1
    app.handle_event(AppEvent::WtEvent {
        method: "connection_state".to_string(),
        pane_id: "1".to_string(),
        tab_id: None,
        params: json!({"session_id": "1", "state": "connected"}),
    });
    // Critical from pane 2
    app.handle_event(AppEvent::WtEvent {
        method: "connection_state".to_string(),
        pane_id: "2".to_string(),
        tab_id: None,
        params: json!({"session_id": "2", "state": "failed"}),
    });
    // Actionable from pane 3
    app.handle_event(AppEvent::WtEvent {
        method: "connection_state".to_string(),
        pane_id: "3".to_string(),
        tab_id: None,
        params: json!({"session_id": "3", "state": "closed"}),
    });

    assert_eq!(app.wt_notifications.len(), 3);
    // Unacknowledged count only counts actionable + critical
    assert_eq!(app.unacknowledged_count(), 2);
    // Banner should show (due to critical + actionable)
    assert!(app.show_notification_banner);
    // Chat must stay empty — WT events surface in the bar/banner, never
    // in agent dialogue.
    assert!(app.current_tab().messages.is_empty());
}

// ─── Task C: Agents snapshot viewer / master refetch ────────────────────

#[test]
fn agents_view_open_sends_sessions_list_request() {
    let (mut app, mut master_rx) = test_app_with_master_rx();
    app.open_agents_view_for_tab(DEFAULT_TAB_ID.to_string());
    match master_rx
        .try_recv()
        .expect("open must request sessions/list")
    {
        crate::protocol::acp::client::MasterExtRequest::SessionsList { .. } => {}
        other => panic!("expected SessionsList, got {other:?}"),
    }
    assert!(app.current_tab().agents_view.snapshot.is_some());
    assert!(app.current_tab().agents_view.refetch_in_flight);
}

#[test]
fn born_bound_registration_uses_current_master_request_sender() {
    let (mut app, mut old_master_rx) = test_app_with_master_rx();
    let (new_master_tx, mut new_master_rx) = tokio::sync::mpsc::unbounded_channel();
    app.master_request_tx = new_master_tx;
    let event = crate::agent_sessions::SessionEvent::SessionStarted {
        key: "sid".to_string(),
        cli_source: crate::agent_sessions::CliSource::Copilot,
        pane_session_id: "pane".to_string(),
        cwd: std::path::PathBuf::from("C:\\repo"),
        title: String::new(),
    };

    app.handle_event(AppEvent::RegisterBornBoundSession {
        event: event.clone(),
    });

    assert!(old_master_rx.try_recv().is_err());
    match new_master_rx
        .try_recv()
        .expect("registration should use the replacement sender")
    {
        crate::protocol::acp::client::MasterExtRequest::SessionBornBound {
            event: actual,
        } => assert_eq!(actual, event),
        other => panic!("expected SessionBornBound, got {other:?}"),
    }
}

#[test]
fn sessions_changed_with_open_agents_view_schedules_refetch() {
    let (mut app, mut master_rx) = test_app_with_master_rx();
    app.current_tab_mut().current_view = View::Agents;
    app.current_tab_mut().agents_view.snapshot = Some(Vec::new());
    app.handle_event(AppEvent::SessionsChanged);
    match master_rx.try_recv().expect("change must request refetch") {
        crate::protocol::acp::client::MasterExtRequest::SessionsList { .. } => {}
        other => panic!("expected SessionsList, got {other:?}"),
    }
    assert!(app.current_tab().agents_view.refetch_in_flight);
}

#[test]
fn sessions_changed_with_closed_agents_view_is_noop() {
    let (mut app, mut master_rx) = test_app_with_master_rx();
    app.current_tab_mut().current_view = View::Chat;
    app.current_tab_mut().agents_view.snapshot = None;
    app.handle_event(AppEvent::SessionsChanged);
    assert!(master_rx.try_recv().is_err(), "closed UI must not refetch");
}

// ─── /model per-pane override ───────────────────────────────────────────

fn model_info(id: &str) -> AcpModelInfo {
    AcpModelInfo {
        id: id.to_string(),
        name: id.to_uppercase(),
        description: None,
    }
}

/// `/model <id>` records a per-pane override and hot-applies it to *that*
/// tab's live session (a targeted `SetSessionModel`, not a fan-out).
#[test]
fn model_pick_overrides_and_applies_to_live_session() {
    use crate::protocol::acp::client::MasterExtRequest;
    let (mut app, mut master_rx) = test_app_with_master_rx();
    app.available_models = vec![model_info("gpt-5.5"), model_info("gpt-5.4")];
    app.current_tab_mut().session_id = Some("sid-1".into());

    app.cmd_model("gpt-5.4".into());

    assert_eq!(
        app.current_tab().model_override.as_deref(),
        Some("gpt-5.4"),
        "the pane records its per-pane override"
    );
    match master_rx
        .try_recv()
        .expect("a live session gets set_session_model")
    {
        MasterExtRequest::SetSessionModel { session_id, model } => {
            assert_eq!(model, "gpt-5.4");
            assert_eq!(
                session_id.expect("targets just this session").0.to_string(),
                "sid-1"
            );
        }
        other => panic!("expected SetSessionModel, got {other:?}"),
    }
}

/// A global `acpModel` settings change is authoritative: it overrides a
/// pane's local `/model` pick — clearing the override, redirecting the
/// shared current model, and pushing the new model to the pane's session.
#[test]
fn global_settings_change_overrides_local_pick() {
    use crate::protocol::acp::client::MasterExtRequest;
    let (mut app, mut master_rx) = test_app_with_master_rx();
    app.available_models = vec![model_info("local"), model_info("globalv2")];
    app.current_tab_mut().session_id = Some("sid-1".into());

    // Pane pins a local model first.
    app.cmd_model("local".into());
    let _ = master_rx.try_recv(); // drain the pick's own apply
    assert_eq!(app.current_tab().model_override.as_deref(), Some("local"));

    // Global settings change to a different model — authoritative.
    app.apply_global_acp_model(Some("globalv2".into()));

    assert_eq!(
        app.current_tab().model_override,
        None,
        "a global change clears the per-pane override"
    );
    assert_eq!(
        app.current_model_id.as_deref(),
        Some("globalv2"),
        "the shared current model follows the new global value"
    );
    match master_rx
        .try_recv()
        .expect("the previously-overridden pane still gets the new global model")
    {
        MasterExtRequest::SetSessionModel { session_id, model } => {
            assert_eq!(model, "globalv2");
            assert_eq!(session_id.unwrap().0.to_string(), "sid-1");
        }
        other => panic!("expected SetSessionModel, got {other:?}"),
    }
}

/// A pane with no local pick follows the global `acpModel` on hot-reload.
#[test]
fn non_overridden_pane_follows_global_model() {
    use crate::protocol::acp::client::MasterExtRequest;
    let (mut app, mut master_rx) = test_app_with_master_rx();
    app.current_tab_mut().session_id = Some("sid-1".into());
    app.acp_model = Some("global".into());

    app.send_acp_model_update();

    match master_rx
        .try_recv()
        .expect("non-overridden pane follows global")
    {
        MasterExtRequest::SetSessionModel { session_id, model } => {
            assert_eq!(model, "global");
            assert_eq!(session_id.unwrap().0.to_string(), "sid-1");
        }
        other => panic!("expected SetSessionModel, got {other:?}"),
    }
}

/// `/model` with an unrecognized argument warns and changes nothing.
#[test]
fn model_pick_rejects_unknown_model() {
    let (mut app, mut master_rx) = test_app_with_master_rx();
    app.available_models = vec![model_info("known")];
    app.current_tab_mut().session_id = Some("sid-1".into());

    app.cmd_model("nope".into());

    assert!(
        app.current_tab().model_override.is_none(),
        "an unknown model must not set an override"
    );
    assert!(
        master_rx.try_recv().is_err(),
        "an unknown model must not emit a set_session_model"
    );
}

/// MVP sessions origin filter: with `ShellOnly`, agent-pane rows must
/// be hidden from `agents_rows_for_tab` (the cursor / Enter
/// dispatch source of truth) — *not just* from `agents_view::render`.
/// A bug where render filtered but `agents_rows_for_tab` didn't
/// would let Enter on visible row N activate hidden row M.
#[test]
fn shell_only_filter_hides_agent_pane_rows_from_cursor_model() {
    use crate::agent_sessions::{OriginFilter, SessionOrigin};
    let mut app = test_app();
    app.sessions_origin_filter = OriginFilter::ShellOnly;
    // Snapshot path: master pushed two rows — one tagged
    // AgentPane (Class A, hidden under ShellOnly), one tagged
    // Unknown (Class B, visible).
    let mut pane = session_info_for_test("class-a");
    pane.origin = Some(SessionOrigin::AgentPane);
    pane.last_activity_at_ms = Some(200);
    let mut shell = session_info_for_test("class-b");
    shell.origin = Some(SessionOrigin::Unknown);
    shell.last_activity_at_ms = Some(100);
    app.current_tab_mut().agents_view.snapshot = Some(vec![pane, shell]);

    let rows = app.agents_rows_for_tab(DEFAULT_TAB_ID);
    assert_eq!(rows.len(), 1, "only the Class B row is visible: {rows:?}");
    assert_eq!(rows[0].key, "class-b");

    // Flip to All — both rows must reappear so the un-MVP toggle
    // brings agent-pane rows back without any other code change.
    app.sessions_origin_filter = OriginFilter::All;
    let rows = app.agents_rows_for_tab(DEFAULT_TAB_ID);
    assert_eq!(rows.len(), 2);

    // AgentPaneOnly is the inverse — only Class A surfaces.
    app.sessions_origin_filter = OriginFilter::AgentPaneOnly;
    let rows = app.agents_rows_for_tab(DEFAULT_TAB_ID);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].key, "class-a");
}

/// Registry path (no snapshot): the same filter must apply when
/// `agents_rows_for_tab` falls back to `agent_sessions` directly.
/// Without this, helpers that haven't received a master snapshot
/// yet would show every row regardless of the MVP filter.
#[test]
fn shell_only_filter_applies_to_registry_fallback_path() {
    use crate::agent_sessions::{CliSource, OriginFilter, SessionEvent, SessionOrigin};
    use std::path::PathBuf;
    let mut app = test_app();
    app.sessions_origin_filter = OriginFilter::ShellOnly;
    // No snapshot primed — `agents_rows_for_tab` goes through
    // `iter_sorted_with_filters` on the registry.
    app.agent_sessions.apply(SessionEvent::SessionStarted {
        key: "shell-key".into(),
        cli_source: CliSource::Claude,
        pane_session_id: "00000000-0000-0000-0000-00000000aaaa".into(),
        cwd: PathBuf::from("/x"),
        title: "shell".into(),
    });
    app.agent_sessions.apply(SessionEvent::SessionStarted {
        key: "pane-key".into(),
        cli_source: CliSource::Claude,
        pane_session_id: "00000000-0000-0000-0000-00000000bbbb".into(),
        cwd: PathBuf::from("/x"),
        title: "pane".into(),
    });
    app.agent_sessions.set_origin("pane-key", SessionOrigin::AgentPane);

    let rows = app.agents_rows_for_tab(DEFAULT_TAB_ID);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].key, "shell-key");
}

/// The PRODUCTION snapshot path (master pushed `sessions/list` response
/// into `agents_view.snapshot`) must preserve the `Wsl` location in every
/// `AgentSession` produced by `agents_rows_for_tab`.
///
/// This is the regression test that would have caught the original bug:
/// `session_info_to_agent_session` hardcoded `location: Host`, so WSL
/// rows crossing the master→helper boundary silently lost their distro
/// stamp.  The fix carries `location` through `SessionInfo`; this test
/// guards that fix forever.
#[test]
fn agents_rows_snapshot_preserves_wsl_location() {
    use crate::agent_sessions::{OriginFilter, SessionLocation};

    let mut app = test_app();
    // Use `All` to bypass the MVP ShellOnly filter — we want to confirm
    // location preservation regardless of origin filtering.
    app.sessions_origin_filter = OriginFilter::All;

    let mut info = session_info_for_test("wsl-1");
    info.origin = Some(crate::agent_sessions::SessionOrigin::Unknown);
    info.location = SessionLocation::Wsl { distro: "Ubuntu".into() };

    app.current_tab_mut().current_view = View::Agents;
    app.current_tab_mut().agents_view.snapshot = Some(vec![info]);

    let rows = app.agents_rows_for_tab(DEFAULT_TAB_ID);
    assert_eq!(rows.len(), 1, "expected one row; got: {rows:?}");
    assert!(
        rows[0].location.is_wsl(),
        "snapshot path must preserve WSL location; got: {:?}",
        rows[0].location
    );
    assert_eq!(
        rows[0].location,
        SessionLocation::Wsl { distro: "Ubuntu".into() },
        "distro name must round-trip through session_info_to_agent_session"
    );
}

/// End-to-end render proof: a WSL `SessionInfo` in the `/sessions`
/// snapshot must actually paint its bracketed distro tag (`[WSL-Ubuntu]`)
/// on screen. `agents_rows_snapshot_preserves_wsl_location` proves the
/// data path and `origin_prefix_shows_distro_for_wsl_rows` proves the
/// prefix builder; this closes the loop through `crate::ui::render` so a
/// regression in `agents_view::render`'s own `session_info_to_agent_session`
/// conversion (a *second* call site, separate from `agents_rows_for_tab`)
/// can't silently drop the tag.
#[test]
fn render_sessions_view_paints_wsl_distro_tag() {
    use crate::agent_sessions::{OriginFilter, SessionLocation};

    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.sessions_origin_filter = OriginFilter::All;

    let mut info = session_info_for_test("wsl-render-1");
    info.title = Some("hack on wsl".into());
    info.origin = Some(crate::agent_sessions::SessionOrigin::Unknown);
    info.location = SessionLocation::Wsl { distro: "Ubuntu".into() };

    app.current_tab_mut().current_view = View::Agents;
    app.current_tab_mut().agents_view.snapshot = Some(vec![info]);

    let text = render_to_text(&mut app, 80, 24);
    assert!(
        text.contains("[WSL-Ubuntu]"),
        "the /sessions view must paint the bracketed WSL distro tag; rendered:\n{text}"
    );
}

/// `resolve_sessions_origin_filter` reads the `WTA_SESSIONS_SHOW_AGENT_PANE`
/// env var. With it unset (or 0/false) the MVP default
/// (`ShellOnly`) wins; with it set to a truthy value we flip to
/// `All` so a single debug helper can see everything without a
/// rebuild.
///
/// Env vars are process-global, so this test serializes via the
/// `WTA_SESSIONS_SHOW_AGENT_PANE_TEST_LOCK` mutex shared with any other
/// future test that touches the same var.
#[test]
fn resolve_sessions_origin_filter_respects_env_override() {
    use crate::agent_sessions::OriginFilter;
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    std::env::remove_var("WTA_SESSIONS_SHOW_AGENT_PANE");
    assert_eq!(crate::app::resolve_sessions_origin_filter(), MVP_SESSIONS_ORIGIN_FILTER);
    assert_eq!(MVP_SESSIONS_ORIGIN_FILTER, OriginFilter::ShellOnly);

    std::env::set_var("WTA_SESSIONS_SHOW_AGENT_PANE", "1");
    assert_eq!(crate::app::resolve_sessions_origin_filter(), OriginFilter::All);

    std::env::set_var("WTA_SESSIONS_SHOW_AGENT_PANE", "true");
    assert_eq!(crate::app::resolve_sessions_origin_filter(), OriginFilter::All);

    std::env::set_var("WTA_SESSIONS_SHOW_AGENT_PANE", "0");
    assert_eq!(crate::app::resolve_sessions_origin_filter(), MVP_SESSIONS_ORIGIN_FILTER);

    std::env::remove_var("WTA_SESSIONS_SHOW_AGENT_PANE");
}

#[test]
fn snapshot_refetch_preserves_focused_sid() {
    let (mut app, mut master_rx) = test_app_with_master_rx();
    app.open_agents_view_for_tab(DEFAULT_TAB_ID.to_string());
    let first_req = match master_rx.try_recv().unwrap() {
        crate::protocol::acp::client::MasterExtRequest::SessionsList { request_id, .. } => {
            request_id
        }
        other => panic!("expected SessionsList, got {other:?}"),
    };
    app.handle_event(AppEvent::AgentsSnapshotLoaded {
        request_id: first_req,
        sessions: vec![
            session_info_for_test("a"),
            session_info_for_test("b"),
            session_info_for_test("c"),
        ],
    });
    app.current_tab_mut().agents_list_state.select(Some(1));
    app.current_tab_mut().agents_view.focused_sid =
        Some(agent_client_protocol::schema::v1::SessionId::new("b"));
    app.handle_event(AppEvent::SessionsChanged);
    let second_req = match master_rx.try_recv().unwrap() {
        crate::protocol::acp::client::MasterExtRequest::SessionsList { request_id, .. } => {
            request_id
        }
        other => panic!("expected SessionsList, got {other:?}"),
    };
    app.handle_event(AppEvent::AgentsSnapshotLoaded {
        request_id: second_req,
        sessions: vec![
            session_info_for_test("c"),
            session_info_for_test("a"),
            session_info_for_test("b"),
        ],
    });
    assert_eq!(app.current_tab().agents_list_state.selected(), Some(2));
    assert_eq!(
        app.current_tab()
            .agents_view
            .focused_sid
            .as_ref()
            .map(|s| s.0.as_ref()),
        Some("b")
    );
}

#[test]
fn sessions_changed_coalesces_rapid_pushes() {
    let (mut app, mut master_rx) = test_app_with_master_rx();
    app.current_tab_mut().current_view = View::Agents;
    app.current_tab_mut().agents_view.snapshot = Some(Vec::new());
    for _ in 0..100 {
        app.handle_event(AppEvent::SessionsChanged);
    }
    let first_req = match master_rx.try_recv().expect("one in-flight refetch") {
        crate::protocol::acp::client::MasterExtRequest::SessionsList { request_id, .. } => {
            request_id
        }
        other => panic!("expected SessionsList, got {other:?}"),
    };
    assert!(
        master_rx.try_recv().is_err(),
        "rapid pushes coalesce while in flight"
    );
    assert!(app.current_tab().agents_view.refetch_in_flight);
    assert!(app.current_tab().agents_view.dirty);
    app.handle_event(AppEvent::AgentsSnapshotLoaded {
        request_id: first_req,
        sessions: Vec::new(),
    });
    match master_rx.try_recv().expect("dirty trailing refetch") {
        crate::protocol::acp::client::MasterExtRequest::SessionsList { .. } => {}
        other => panic!("expected SessionsList, got {other:?}"),
    }
    assert!(
        master_rx.try_recv().is_err(),
        "at most one trailing refetch"
    );
}

/// Failure / timeout path must unblock `refetch_in_flight` so the
/// next `SessionsChanged` (from a broadcast or the 5s tick) can
/// retry, while keeping the existing snapshot rendered. Without
/// this, an `ext_method` future that never resolves (the ACP-0.10
/// cancellation-safety bug) would freeze the view forever.
#[test]
fn agents_snapshot_failed_unblocks_refetch_without_dropping_snapshot() {
    let (mut app, mut master_rx) = test_app_with_master_rx();
    app.open_agents_view_for_tab(DEFAULT_TAB_ID.to_string());
    let first_req = match master_rx.try_recv().unwrap() {
        crate::protocol::acp::client::MasterExtRequest::SessionsList { request_id, .. } => {
            request_id
        }
        other => panic!("expected SessionsList, got {other:?}"),
    };
    // Land a real snapshot first so we can assert it is preserved
    // across the subsequent failure.
    app.handle_event(AppEvent::AgentsSnapshotLoaded {
        request_id: first_req,
        sessions: vec![session_info_for_test("a"), session_info_for_test("b")],
    });
    assert!(!app.current_tab().agents_view.refetch_in_flight);
    let before_len = app
        .current_tab()
        .agents_view
        .snapshot
        .as_ref()
        .map(|v| v.len())
        .unwrap_or(0);
    assert_eq!(before_len, 2);

    // Kick a second refetch and report it as failed.
    app.handle_event(AppEvent::SessionsChanged);
    let second_req = match master_rx.try_recv().expect("second refetch sent") {
        crate::protocol::acp::client::MasterExtRequest::SessionsList { request_id, .. } => {
            request_id
        }
        other => panic!("expected SessionsList, got {other:?}"),
    };
    assert!(app.current_tab().agents_view.refetch_in_flight);
    app.handle_event(AppEvent::AgentsSnapshotFailed {
        request_id: second_req,
    });

    // refetch_in_flight must clear; snapshot must NOT be wiped.
    assert!(
        !app.current_tab().agents_view.refetch_in_flight,
        "failure path must unblock the gate"
    );
    let after_len = app
        .current_tab()
        .agents_view
        .snapshot
        .as_ref()
        .map(|v| v.len())
        .unwrap_or(0);
    assert_eq!(
        after_len, 2,
        "failure path must not overwrite the existing snapshot"
    );
    assert!(
        master_rx.try_recv().is_err(),
        "no spurious immediate retry without dirty coalescing"
    );
}

/// If pushes arrive while the in-flight `sessions/list` is doomed
/// to fail, the trailing-refetch behaviour must still fire on
/// `AgentsSnapshotFailed` — otherwise the user would have to wait
/// for the next 5s tick after every failure even when state has
/// already changed since the request went out.
#[test]
fn agents_snapshot_failed_fires_dirty_trailing_refetch() {
    let (mut app, mut master_rx) = test_app_with_master_rx();
    app.open_agents_view_for_tab(DEFAULT_TAB_ID.to_string());
    let req_id = match master_rx.try_recv().unwrap() {
        crate::protocol::acp::client::MasterExtRequest::SessionsList { request_id, .. } => {
            request_id
        }
        other => panic!("expected SessionsList, got {other:?}"),
    };
    // While the request is in-flight, more pushes arrive and
    // coalesce into `dirty=true`.
    for _ in 0..5 {
        app.handle_event(AppEvent::SessionsChanged);
    }
    assert!(app.current_tab().agents_view.dirty);
    assert!(
        master_rx.try_recv().is_err(),
        "additional pushes must coalesce while in flight"
    );

    app.handle_event(AppEvent::AgentsSnapshotFailed { request_id: req_id });
    match master_rx
        .try_recv()
        .expect("dirty trailing refetch after failure")
    {
        crate::protocol::acp::client::MasterExtRequest::SessionsList { .. } => {}
        other => panic!("expected SessionsList, got {other:?}"),
    }
    assert!(app.current_tab().agents_view.refetch_in_flight);
    assert!(!app.current_tab().agents_view.dirty);
}

/// `AgentsSnapshotFailed` for a stale `request_id` (e.g. arrives
/// after the tab was closed and reopened) must be a no-op — it
/// must not clobber a fresh in-flight refetch's
/// `refetch_in_flight=true` flag.
#[test]
fn agents_snapshot_failed_ignores_stale_request_id() {
    let (mut app, mut master_rx) = test_app_with_master_rx();
    app.open_agents_view_for_tab(DEFAULT_TAB_ID.to_string());
    let _stale = match master_rx.try_recv().unwrap() {
        crate::protocol::acp::client::MasterExtRequest::SessionsList { request_id, .. } => {
            request_id
        }
        other => panic!("expected SessionsList, got {other:?}"),
    };
    // Resolve the first request, then kick another so latest_request_id
    // moves on.
    app.handle_event(AppEvent::AgentsSnapshotLoaded {
        request_id: _stale,
        sessions: vec![session_info_for_test("a")],
    });
    app.handle_event(AppEvent::SessionsChanged);
    let _fresh = match master_rx.try_recv().unwrap() {
        crate::protocol::acp::client::MasterExtRequest::SessionsList { request_id, .. } => {
            request_id
        }
        other => panic!("expected SessionsList, got {other:?}"),
    };
    assert!(app.current_tab().agents_view.refetch_in_flight);

    // A stale failure must NOT touch the fresh in-flight state.
    app.handle_event(AppEvent::AgentsSnapshotFailed { request_id: _stale });
    assert!(
        app.current_tab().agents_view.refetch_in_flight,
        "stale failure must not clear the fresh in-flight gate"
    );
}

/// The loading-shimmer signal: true only while the agents view is open
/// and waiting on its first `session/list` reply (empty placeholder
/// snapshot + in-flight refetch). Replaces the removed on-disk-scan
/// `HistoryLoadState::Loading` signal.
#[test]
fn agents_view_awaiting_snapshot_tracks_first_session_list() {
    let (mut app, _master_rx) = test_app_with_master_rx();
    // Chat view → never awaiting (the shimmer is agents-view only).
    assert!(!app.agents_view_awaiting_snapshot());

    // Opening the agents view primes an empty placeholder snapshot and an
    // in-flight refetch — exactly the loading-shimmer window.
    app.open_agents_view_for_tab(DEFAULT_TAB_ID.to_string());
    assert!(
        app.agents_view_awaiting_snapshot(),
        "awaiting the first session/list snapshot right after open"
    );

    // A non-empty snapshot (master replied with rows) ends the awaiting
    // state even while a follow-up refetch is in flight.
    app.current_tab_mut().agents_view.snapshot = Some(vec![session_info_for_test("a")]);
    assert!(!app.agents_view_awaiting_snapshot());

    // An empty reply with the refetch finished is the genuine empty
    // state, not loading.
    app.current_tab_mut().agents_view.snapshot = Some(Vec::new());
    app.current_tab_mut().agents_view.refetch_in_flight = false;
    assert!(!app.agents_view_awaiting_snapshot());
}

#[test]
fn agents_view_loading_shows_during_f5_rescan() {
    let (mut app, _master_rx) = test_app_with_master_rx();
    app.open_agents_view_for_tab(DEFAULT_TAB_ID.to_string());
    // First snapshot landed: rows present, fetch settled — not loading.
    app.current_tab_mut().agents_view.snapshot = Some(vec![session_info_for_test("a")]);
    app.current_tab_mut().agents_view.refetch_in_flight = false;
    assert!(!app.agents_view_awaiting_snapshot(), "a settled list is not loading");

    // F5 dispatches a rescan: the loading shimmer must show even though the
    // list already has rows, so the refresh is visible.
    app.current_tab_mut().agents_view.pending_rescan = true;
    app.schedule_agents_refetch_for_tab(DEFAULT_TAB_ID);
    assert!(
        app.agents_view_awaiting_snapshot(),
        "F5 rescan must show the loading shimmer even with rows present"
    );

    // The rescan response clears it back to the settled list.
    let rid = app
        .current_tab()
        .agents_view
        .latest_request_id
        .expect("a request was dispatched");
    app.handle_agents_snapshot_loaded(rid, vec![session_info_for_test("a")]);
    assert!(
        !app.agents_view_awaiting_snapshot(),
        "loading clears once the rescan response lands"
    );
}

fn session_info_for_test(id: &str) -> crate::session_registry::SessionInfo {
    let mut info = crate::session_registry::SessionInfo::new(
        agent_client_protocol::schema::v1::SessionId::new(id),
        std::path::PathBuf::from(format!("/repo/{id}")),
    );
    info.title = Some(id.to_string());
    info.status = Some(crate::agent_sessions::AgentStatus::Idle);
    info.cli_source = Some(crate::agent_sessions::CliSource::Claude);
    info.last_activity_at_ms = Some(1);
    info
}

// ─── agent session view: Enter dispatch ────────────────────────────────────

#[test]
fn enter_on_live_row_dispatches_focus_command() {
    use crate::agent_sessions::{CliSource, SessionEvent};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::path::PathBuf;
    let mut app = test_app();
    app.agent_sessions.apply(SessionEvent::SessionStarted {
        key: "a".into(),
        cli_source: CliSource::Claude,
        pane_session_id: "00000000-0000-0000-0000-0000000000aa".into(),
        cwd: PathBuf::from("/x"),
        title: "t".into(),
    });
    app.current_tab_mut().current_view = View::Agents;
    app.current_tab_mut().agents_list_state.select(Some(0));

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let cmd = app
        .last_dispatched_command_for_test()
        .expect("a command was dispatched");
    assert_eq!(cmd.kind, DispatchedCommandKind::FocusPane);
    assert_eq!(cmd.session_id.as_deref(), Some("a"));
}

// F5 in the session-management view refetches the session list (footer
// hint: "F5 to refresh"). When no fetch is in flight it dispatches a
// fresh sessions/list request to master.
#[test]
fn f5_in_session_view_refetches_sessions() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let (mut app, mut master_rx) = test_app_with_master_rx();
    let tab_id = app.active_tab_key().to_string();
    app.open_agents_view_for_tab(tab_id);

    // The open-time refetch must be snapshot-only (no disk rescan).
    match master_rx.try_recv().expect("open requests sessions/list") {
        crate::protocol::acp::client::MasterExtRequest::SessionsList { rescan, .. } => {
            assert!(!rescan, "view-open refetch must not rescan");
        }
        other => panic!("expected SessionsList, got {other:?}"),
    }
    // Clear the in-flight flag so the F5 refetch dispatches fresh.
    app.current_tab_mut().agents_view.refetch_in_flight = false;
    app.current_tab_mut().agents_view.search_query = "active search".into();
    app.current_tab_mut().agents_view.search_focused = true;

    app.handle_key(KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE));

    match master_rx.try_recv().expect("F5 must request sessions/list") {
        crate::protocol::acp::client::MasterExtRequest::SessionsList { rescan, .. } => {
            assert!(rescan, "F5 must request a master-side disk rescan");
        }
        other => panic!("expected SessionsList, got {other:?}"),
    }
    assert_eq!(app.current_tab().agents_view.search_query, "active search");
    assert!(app.current_tab().agents_view.search_focused);
}

#[test]
fn session_search_filters_navigation_and_enter_dispatch() {
    use crate::agent_sessions::SessionOrigin;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = test_app();
    app.current_tab_mut().current_view = View::Agents;

    let mut title_match = session_info_for_test("title-match");
    title_match.title = Some("PowerShell repair".into());
    title_match.cwd = std::path::PathBuf::from(r"C:\Windows");
    title_match.pane_session_id =
        Some("00000000-0000-0000-0000-0000000000a1".into());
    title_match.origin = Some(SessionOrigin::Unknown);
    title_match.last_activity_at_ms = Some(300);

    let mut unrelated = session_info_for_test("unrelated");
    unrelated.title = Some("fix the build".into());
    unrelated.cwd = std::path::PathBuf::from(r"C:\Windows");
    unrelated.pane_session_id =
        Some("00000000-0000-0000-0000-0000000000b2".into());
    unrelated.origin = Some(SessionOrigin::Unknown);
    unrelated.last_activity_at_ms = Some(200);

    let mut second_title_match = session_info_for_test("second-title-match");
    second_title_match.title = Some("portal review".into());
    second_title_match.cwd = std::path::PathBuf::from(r"C:\repos\portal");
    second_title_match.pane_session_id =
        Some("00000000-0000-0000-0000-0000000000c3".into());
    second_title_match.origin = Some(SessionOrigin::Unknown);
    second_title_match.last_activity_at_ms = Some(100);

    app.current_tab_mut().agents_view.snapshot =
        Some(vec![title_match, unrelated, second_title_match]);
    app.current_tab_mut().agents_list_state.select(Some(0));

    app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
    assert!(app.current_tab().agents_view.search_focused);
    app.handle_key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT));
    app.handle_key(KeyEvent::new(KeyCode::Char('O'), KeyModifiers::SHIFT));

    assert_eq!(app.current_tab().agents_view.search_query, "PO");
    assert_eq!(
        app.agents_rows_for_tab(DEFAULT_TAB_ID)
            .iter()
            .map(|row| row.key.as_str())
            .collect::<Vec<_>>(),
        vec!["title-match", "second-title-match"]
    );

    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(app.current_tab().agents_list_state.selected(), Some(1));
    assert!(
        app.current_tab().agents_view.search_focused,
        "arrow navigation must keep the search input active"
    );
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let cmd = app
        .last_dispatched_command_for_test()
        .expect("the selected filtered row must dispatch");
    assert_eq!(cmd.kind, DispatchedCommandKind::FocusPane);
    assert_eq!(cmd.session_id.as_deref(), Some("second-title-match"));
}

#[test]
fn session_search_is_hidden_until_slash_and_escape_clears_it() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = test_app();
    app.current_tab_mut().current_view = View::Agents;
    app.current_tab_mut().agents_view.snapshot =
        Some(vec![session_info_for_test("visible-session")]);

    let before = render_to_text(&mut app, 80, 24);
    assert!(
        !before.contains('▏'),
        "the search cursor must be hidden before / is pressed; rendered:\n{before}"
    );

    app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
    let active = render_to_text(&mut app, 80, 24);
    assert!(
        active.contains('▏'),
        "pressing / must reveal the search input; rendered:\n{active}"
    );

    app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.current_tab().agents_view.search_query.is_empty());
    assert!(!app.current_tab().agents_view.search_focused);
    assert_eq!(
        app.current_tab().current_view,
        View::Agents,
        "the first Esc dismisses search instead of closing session management"
    );

}

// Esc out of the session-management (Agents) view restores the pane
// visibility the user had *before* they entered it, rather than always
// leaving an open chat pane behind. Two cases mirror the two ways the
// view is reached (see `open_agents_view_for_tab` + the Esc handler).

#[test]
fn esc_from_session_view_refolds_when_entered_from_folded_pane() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = test_app();
    let tab_id = app.active_tab_key().to_string();

    // Pane starts folded (stashed): pane_open == false.
    app.tab_mut(&tab_id).pane_open = false;

    // Reproduce the C++ "unstash into sessions" request, which applies
    // `view` before `pane_open`: the view switch snapshots the pre-message
    // `pane_open=false`, then the pane is marked open while sessions show.
    app.open_agents_view_for_tab(tab_id.clone());
    app.tab_mut(&tab_id).pane_open = true;
    assert_eq!(app.current_tab().current_view, View::Agents);

    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    // Re-folds: pane hidden. The view is intentionally left on Agents
    // (not switched to Chat) so the pane stashes straight from the
    // session list without flashing the chat view for a frame first.
    assert!(
        !app.current_tab().pane_open,
        "Esc from a pane that was folded before session management must re-fold it"
    );
    assert_eq!(
        app.current_tab().current_view,
        View::Agents,
        "fold-restore must not switch to chat (would flash before stashing)"
    );
    assert_eq!(
        app.current_tab().agents_view_prev_pane_open, None,
        "the snapshot must be cleared after Esc so a re-entry re-captures"
    );
}

#[test]
fn esc_from_session_view_keeps_pane_open_when_entered_from_chat() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = test_app();
    let tab_id = app.active_tab_key().to_string();

    // Pane is already an expanded chat pane: pane_open == true. The
    // chat->sessions request keeps pane_open=true, so the snapshot is
    // Some(true) and Esc must leave the pane open.
    app.tab_mut(&tab_id).pane_open = true;
    app.open_agents_view_for_tab(tab_id.clone());
    assert_eq!(app.current_tab().current_view, View::Agents);

    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert_eq!(app.current_tab().current_view, View::Chat);
    assert!(
        app.current_tab().pane_open,
        "Esc from an expanded chat pane must return to it (stay open)"
    );
}

// Checklist C085 "View switch preserves input": a typed-but-unsubmitted chat draft must
// survive a round-trip through the session (Agents) view. This is the deterministic coverage
// for the item whose E2E form is not harness-reliable (opening the session view input-free and
// reading it back races the per-tab pre-warm's extra pane; the slash `/sessions` trigger would
// itself type into the draft; Esc is overloaded chat-clear vs view-exit). Here we drive the
// REAL Esc key handler — the exact path where an accidental input-clear on view exit would
// live — not just the open/close_agents_view helpers.
#[test]
fn view_switch_preserves_chat_draft_input() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = test_app();
    let tab_id = app.active_tab_key().to_string();

    // A user is composing a prompt in the chat view (pane open, draft typed, not submitted).
    app.tab_mut(&tab_id).pane_open = true;
    let draft = "unsubmitted draft prompt";
    app.current_tab_mut().input = draft.into();
    app.current_tab_mut().cursor_pos = draft.len();

    // Switch chat -> sessions view (the chat->sessions request keeps pane_open=true).
    app.open_agents_view_for_tab(tab_id.clone());
    assert_eq!(app.current_tab().current_view, View::Agents);
    assert_eq!(
        app.current_tab().input,
        draft,
        "the draft must be untouched while the session view is shown"
    );

    // Esc back to chat (the round-trip return path).
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.current_tab().current_view, View::Chat);

    // The draft AND the cursor position must still be there after the round-trip.
    assert_eq!(
        app.current_tab().input,
        draft,
        "returning to chat after a view switch must preserve the unsubmitted draft"
    );
    assert_eq!(
        app.current_tab().cursor_pos,
        draft.len(),
        "the cursor position in the draft must be preserved across the view round-trip"
    );
}

// A pane folded *from within* the sessions view (fold keeps current_view ==
// Agents) and then reopened must re-snapshot the now-folded state, so a
// later Esc re-folds instead of using a stale "was open" snapshot.
#[test]
fn esc_reuses_latest_snapshot_after_fold_from_session_view() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = test_app();
    let tab_id = app.active_tab_key().to_string();

    // 1. Enter sessions from an open chat pane -> snapshot Some(true).
    app.tab_mut(&tab_id).pane_open = true;
    app.open_agents_view_for_tab(tab_id.clone());

    // 2. Fold while staying in the sessions view (current_view unchanged).
    app.tab_mut(&tab_id).pane_open = false;

    // 3. Reopen sessions (C++ unstash echo) -> must re-snapshot Some(false).
    app.open_agents_view_for_tab(tab_id.clone());
    app.tab_mut(&tab_id).pane_open = true;

    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert!(
        !app.current_tab().pane_open,
        "the second entry must capture the folded state, so Esc re-folds"
    );
}

#[test]
fn enter_on_history_row_dispatches_new_tab_with_resume() {
    use crate::agent_sessions::{CliSource, SessionEvent};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    // Use a real existing directory so cwd_util::validate_starting_directory
    // accepts it. A missing path would (correctly) be dropped from
    // the argv — that behaviour is covered by
    // `enter_on_history_row_with_missing_cwd_omits_d_flag` below.
    let real_cwd = std::env::temp_dir();
    let real_cwd_str = real_cwd.to_string_lossy().to_string();
    let mut app = test_app();
    app.agent_sessions.apply(SessionEvent::SessionStarted {
        key: "abc-123".into(),
        cli_source: CliSource::Claude,
        pane_session_id: "p".into(),
        cwd: real_cwd.clone(),
        title: "Fix the build".into(),
    });
    app.agent_sessions.apply(SessionEvent::SessionStopped {
        key: "abc-123".into(),
        reason: "user_exit".into(),
    });

    app.current_tab_mut().current_view = View::Agents;
    app.current_tab_mut().agents_list_state.select(Some(0));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let cmd = app
        .last_dispatched_command_for_test()
        .expect("a command was dispatched");
    assert_eq!(cmd.kind, DispatchedCommandKind::NewTabResume);
    let argv = cmd.argv.join(" ");
    // The dispatch must use `wtcli new-tab` (not `split-pane`) so the
    // resumed CLI lands in its own WT tab instead of carving up the
    // originating tab.
    assert!(argv.contains("new-tab"), "argv: {}", argv);
    assert!(
        !argv.contains("split-pane"),
        "argv must NOT use split-pane: {}",
        argv
    );
    assert!(
        cmd.argv.windows(2).any(|args| args == ["--title", "Fix the build"]),
        "resume tab must use the session title: {:?}",
        cmd.argv
    );
    // The CLI invocation is still wrapped in `cmd /c` so .cmd shims
    // resolve via PATHEXT, but the legacy `cd /d` prefix is gone —
    // cwd is threaded through wtcli's `-d` flag now. Issue #135:
    // a muted "Resuming … session …" banner is prepended so the
    // user sees immediate feedback while the CLI cold-starts; the
    // CLI's alt-screen TUI overwrites it on success. (Previously
    // SGR 1;36;5 — bold + cyan + slow-blink — was used, but the
    // blink + bold were too noisy. Now SGR 2;37 = dim + white, a
    // low-contrast tone similar to the cwd line in a typical
    // Copilot-CLI shell prompt.)
    assert!(
        argv.contains(
            "cmd /c echo \x1b[2;37mResuming claude session abc-123...\x1b[0m"
        ),
        "expected dim-white Resuming banner echo; argv: {:?}",
        argv
    );
    assert!(
        argv.contains("&& claude --resume abc-123"),
        "expected resume command chained after banner; argv: {}",
        argv
    );
    assert!(
        !argv.contains("cd /d"),
        "argv must NOT contain cd /d (cwd is now passed via -d): {}",
        argv
    );
    // Resume is keyed off the session's project cwd — the new tab's
    // primary pane must start in that directory so the CLI's session
    // store lookup (`~/.claude/projects/<encoded-cwd>/...`) succeeds.
    let expected = format!("-d {}", real_cwd_str);
    assert!(
        argv.contains(&expected),
        "expected `{}` in argv: {}",
        expected,
        argv
    );
}

/// When the stored cwd no longer exists on disk (e.g. user deleted
/// the project), `dispatch_resume` must omit `-d <cwd>` entirely so
/// wtcli falls back to the profile's startingDirectory. Without
/// this guard, `CreateProcessW` would fail with `ERROR_DIRECTORY`
/// and produce a visibly-broken pane.
#[test]
fn enter_on_history_row_with_missing_cwd_omits_d_flag() {
    use crate::agent_sessions::{CliSource, SessionEvent};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::path::PathBuf;
    let missing = {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "wta-missing-cwd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        p
    };
    assert!(!missing.exists());
    let mut app = test_app();
    app.agent_sessions.apply(SessionEvent::SessionStarted {
        key: "abc-stale".into(),
        cli_source: CliSource::Claude,
        pane_session_id: "p".into(),
        cwd: PathBuf::from(&missing),
        title: "t".into(),
    });
    app.agent_sessions.apply(SessionEvent::SessionStopped {
        key: "abc-stale".into(),
        reason: "user_exit".into(),
    });
    app.current_tab_mut().current_view = View::Agents;
    app.current_tab_mut().agents_list_state.select(Some(0));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let cmd = app
        .last_dispatched_command_for_test()
        .expect("a command was dispatched");
    assert_eq!(cmd.kind, DispatchedCommandKind::NewTabResume);
    let argv = cmd.argv.join(" ");
    assert!(argv.contains("new-tab"), "argv: {}", argv);
    // The stale cwd must NOT have leaked through as `-d`.
    assert!(
        !argv.contains("-d "),
        "argv must omit -d when cwd is missing: {}",
        argv
    );
    assert!(
        !argv.contains(&missing.to_string_lossy().to_string()),
        "argv must not embed the stale cwd: {}",
        argv
    );
}

#[test]
fn shift_enter_on_history_row_dispatches_resume_in_agent_pane() {
    // Shift+Enter on a terminal-state row should route to the
    // ResumeInAgentPane path, NOT the legacy NewTabResume — it
    // emits `resume_in_new_agent_tab` to WT instead of spawning a
    // normal terminal tab locally. The dispatched-command tape
    // captures the shape so downstream wiring can be
    // regression-checked.
    use crate::agent_sessions::{CliSource, SessionEvent};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    // Use a real existing directory so cwd_util::validate_starting_directory
    // accepts it. A missing cwd would (correctly) be omitted —
    // covered by `shift_enter_on_history_row_with_missing_cwd_omits_cwd`.
    let real_cwd = std::env::temp_dir();
    let real_cwd_str = real_cwd.to_string_lossy().to_string();
    let mut app = test_app();
    // Capability gate: dispatch is only attempted when the agent
    // advertised loadSession. Without this, the handler
    // short-circuits with a system message instead.
    app.agent_supports_load_session = true;
    app.agent_sessions.apply(SessionEvent::SessionStarted {
        key: "abc-123".into(),
        cli_source: CliSource::Claude,
        pane_session_id: "p".into(),
        cwd: real_cwd.clone(),
        title: "t".into(),
    });
    app.agent_sessions.apply(SessionEvent::SessionStopped {
        key: "abc-123".into(),
        reason: "user_exit".into(),
    });

    app.current_tab_mut().current_view = View::Agents;
    app.current_tab_mut().agents_list_state.select(Some(0));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));

    let cmd = app
        .last_dispatched_command_for_test()
        .expect("a command was dispatched");
    assert_eq!(cmd.kind, DispatchedCommandKind::ResumeInAgentPane);
    assert_eq!(cmd.session_id.as_deref(), Some("abc-123"));
    let argv = cmd.argv.join(" ");
    assert!(argv.contains("resume_in_new_agent_tab"), "argv: {}", argv);
    assert!(argv.contains("--session-id abc-123"), "argv: {}", argv);
    let expected = format!("--cwd {}", real_cwd_str);
    assert!(
        argv.contains(&expected),
        "expected `{}` in argv: {}",
        expected,
        argv
    );
}

/// Shift+Enter mirror of `enter_on_history_row_with_missing_cwd_omits_d_flag`:
/// when the stored cwd no longer exists, the resume-in-agent-pane
/// path must omit the `cwd` field from the emitted
/// `resume_in_new_agent_tab` event so WT's `_OpenNewTab` falls back
/// to the profile's startingDirectory (otherwise the new tab opens
/// with a broken connection).
#[test]
fn shift_enter_on_history_row_with_missing_cwd_omits_cwd() {
    use crate::agent_sessions::{CliSource, SessionEvent};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::path::PathBuf;
    let missing = {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "wta-missing-shift-cwd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        p
    };
    assert!(!missing.exists());
    let mut app = test_app();
    app.agent_supports_load_session = true;
    app.agent_sessions.apply(SessionEvent::SessionStarted {
        key: "abc-stale".into(),
        cli_source: CliSource::Claude,
        pane_session_id: "p".into(),
        cwd: PathBuf::from(&missing),
        title: "t".into(),
    });
    app.agent_sessions.apply(SessionEvent::SessionStopped {
        key: "abc-stale".into(),
        reason: "user_exit".into(),
    });
    app.current_tab_mut().current_view = View::Agents;
    app.current_tab_mut().agents_list_state.select(Some(0));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));

    let cmd = app
        .last_dispatched_command_for_test()
        .expect("a command was dispatched");
    assert_eq!(cmd.kind, DispatchedCommandKind::ResumeInAgentPane);
    let argv = cmd.argv.join(" ");
    assert!(argv.contains("resume_in_new_agent_tab"), "argv: {}", argv);
    // Fallback contract: the --cwd flag (and any value) must be
    // omitted entirely so the consumer uses its default. A
    // regression that sent `--cwd ""` would slip past a
    // string-contains check, hence the explicit flag assertion.
    assert!(
        !cmd.argv.iter().any(|a| a == "--cwd"),
        "argv must omit --cwd when cwd is missing: {:?}",
        cmd.argv
    );
    assert!(
        !argv.contains(&missing.to_string_lossy().to_string()),
        "argv must not embed the stale cwd: {}",
        argv
    );
}

#[test]
fn shift_enter_history_row_without_load_session_capability_shows_hint() {
    // Capability gate: when the agent doesn't advertise loadSession,
    // Shift+Enter must not open a new tab. Instead it pushes a
    // system message in the session management view explaining the
    // fallback (plain Enter). The dispatched-command tape captures
    // the gated path so the regression is observable.
    use crate::agent_sessions::{CliSource, SessionEvent};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::path::PathBuf;
    let mut app = test_app();
    // No `agent_supports_load_session = true` — default is false.
    app.agent_sessions.apply(SessionEvent::SessionStarted {
        key: "abc-123".into(),
        cli_source: CliSource::Claude,
        pane_session_id: "p".into(),
        cwd: PathBuf::from("/work/proj"),
        title: "t".into(),
    });
    app.agent_sessions.apply(SessionEvent::SessionStopped {
        key: "abc-123".into(),
        reason: "user_exit".into(),
    });

    app.current_tab_mut().current_view = View::Agents;
    app.current_tab_mut().agents_list_state.select(Some(0));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));

    let cmd = app
        .last_dispatched_command_for_test()
        .expect("a command was dispatched");
    // B-10: `decide_enter_action` short-circuits to NotResumable
    // before any side-effect dispatch when the agent doesn't
    // advertise loadSession. Previously this routed all the way
    // through `dispatch_resume_in_agent_pane`'s internal gate;
    // now the gate is hoisted into the pure state machine so
    // there's one canonical path. The system hint message is
    // unchanged.
    assert_eq!(cmd.kind, DispatchedCommandKind::NotResumable);
    let argv = cmd.argv.join(" ");
    assert!(argv.contains("LoadSessionNotSupported"), "argv: {}", argv);
    // The current tab gets a System hint message.
    let has_hint = app.current_tab().messages.iter().any(|m| {
        matches!(m, ChatMessage::System(text)
            if text.contains("loadSession")
                && text.contains("Press Enter"))
    });
    assert!(has_hint, "expected system hint message in the current tab");
}

#[test]
fn shift_enter_on_live_row_falls_back_to_focus() {
    // Live rows have no historical state to "load" — Shift+Enter on
    // them must NOT trigger the resume-in-agent-pane flow. It falls
    // through to the same FocusPane dispatch as plain Enter.
    use crate::agent_sessions::{CliSource, SessionEvent};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::path::PathBuf;
    let mut app = test_app();
    app.agent_sessions.apply(SessionEvent::SessionStarted {
        key: "a".into(),
        cli_source: CliSource::Claude,
        pane_session_id: "00000000-0000-0000-0000-0000000000aa".into(),
        cwd: PathBuf::from("/x"),
        title: "t".into(),
    });
    app.current_tab_mut().current_view = View::Agents;
    app.current_tab_mut().agents_list_state.select(Some(0));

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    let cmd = app
        .last_dispatched_command_for_test()
        .expect("a command was dispatched");
    assert_eq!(cmd.kind, DispatchedCommandKind::FocusPane);
}

// -------- B-10: state-machine-driven Enter / Shift+Enter dispatch --------
//
// Pure routing rules are exhaustively tested in
// `session_mgmt::tests`. Here we verify the *integration* — that the
// key-handler path actually constructs a RowSnapshot from the
// selected AgentSession, hands it to `decide_enter_action`, and
// dispatches each EnterAction variant through the correct side
// effect (or NotResumable hint). One or two representative cases
// per variant is enough; B-1 holds the truth table.

/// Class A (AgentPane origin) dead row + plain Enter:
/// new state machine routes to ResumeInAgentPane (ACP load).
/// This is the headline behavior change from B-10 — previously
/// Class A dead + Enter ran the CLI --resume flag path.
#[test]
fn enter_on_class_a_dead_row_dispatches_resume_in_agent_pane() {
    use crate::agent_sessions::{CliSource, OriginFilter, SessionEvent, SessionOrigin};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::path::PathBuf;
    let mut app = test_app();
    // This test exercises the Class A (AgentPane) Enter routing,
    // which the MVP sessions filter hides. Opt out so the row is
    // visible to the cursor; the dispatch logic under test is
    // unchanged by the filter.
    app.sessions_origin_filter = OriginFilter::All;
    app.agent_supports_load_session = true;
    app.agent_sessions.apply(SessionEvent::SessionStarted {
        key: "abc-class-a".into(),
        cli_source: CliSource::Claude,
        pane_session_id: "p".into(),
        cwd: PathBuf::from("/work/cls-a"),
        title: "t".into(),
    });
    app.agent_sessions.apply(SessionEvent::SessionStopped {
        key: "abc-class-a".into(),
        reason: "user_exit".into(),
    });
    app.agent_sessions
        .set_origin("abc-class-a", SessionOrigin::AgentPane);

    app.current_tab_mut().current_view = View::Agents;
    app.current_tab_mut().agents_list_state.select(Some(0));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let cmd = app
        .last_dispatched_command_for_test()
        .expect("a command was dispatched");
    assert_eq!(cmd.kind, DispatchedCommandKind::ResumeInAgentPane);
    let argv = cmd.argv.join(" ");
    assert!(argv.contains("resume_in_new_agent_tab"), "argv: {}", argv);
    assert!(argv.contains("--session-id abc-class-a"), "argv: {}", argv);
}

/// Class A (AgentPane origin) dead row + Shift+Enter:
/// Shift flips the default → ResumeCliFlag (new tab CLI --resume).
#[test]
fn shift_enter_on_class_a_dead_row_dispatches_cli_resume() {
    use crate::agent_sessions::{CliSource, OriginFilter, SessionEvent, SessionOrigin};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::path::PathBuf;
    let mut app = test_app();
    // See enter_on_class_a_dead_row_dispatches_resume_in_agent_pane
    // for the OriginFilter::All rationale — the MVP filter hides
    // Class A rows from the cursor model; this test exercises the
    // routing logic that fires when they ARE visible.
    app.sessions_origin_filter = OriginFilter::All;
    app.agent_supports_load_session = true;
    app.agent_sessions.apply(SessionEvent::SessionStarted {
        key: "abc-class-a-shift".into(),
        cli_source: CliSource::Claude,
        pane_session_id: "p".into(),
        cwd: PathBuf::from("/work/cls-a"),
        title: "t".into(),
    });
    app.agent_sessions.apply(SessionEvent::SessionStopped {
        key: "abc-class-a-shift".into(),
        reason: "user_exit".into(),
    });
    app.agent_sessions
        .set_origin("abc-class-a-shift", SessionOrigin::AgentPane);

    app.current_tab_mut().current_view = View::Agents;
    app.current_tab_mut().agents_list_state.select(Some(0));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));

    // What matters here is that Shift+Enter on
    // Class A dead routed through dispatch_resume (the CLI flag
    // path), NOT dispatch_resume_in_agent_pane.
    let cmd = app
        .last_dispatched_command_for_test()
        .expect("a command was dispatched");
    assert_eq!(cmd.kind, DispatchedCommandKind::NewTabResume);
}

/// Live row + Shift+Enter: identical to Enter (Shift is a no-op on
/// live rows because agents forbid two clients on one session).
/// This is implicitly the case for `shift_enter_on_live_row_falls_
/// back_to_focus` above; here we additionally assert with a Class
/// A origin to confirm origin doesn't matter for Live rows.
#[test]
fn shift_enter_on_class_a_live_row_focuses() {
    use crate::agent_sessions::{CliSource, OriginFilter, SessionEvent, SessionOrigin};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::path::PathBuf;
    let mut app = test_app();
    // Same rationale as the Class A dead-row tests above:
    // MVP sessions filter hides AgentPane rows, this test verifies the
    // dispatch logic for when they are visible.
    app.sessions_origin_filter = OriginFilter::All;
    app.agent_sessions.apply(SessionEvent::SessionStarted {
        key: "live-class-a".into(),
        cli_source: CliSource::Claude,
        pane_session_id: "00000000-0000-0000-0000-0000000000bb".into(),
        cwd: PathBuf::from("/x"),
        title: "t".into(),
    });
    app.agent_sessions
        .set_origin("live-class-a", SessionOrigin::AgentPane);

    app.current_tab_mut().current_view = View::Agents;
    app.current_tab_mut().agents_list_state.select(Some(0));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));

    let cmd = app
        .last_dispatched_command_for_test()
        .expect("a command was dispatched");
    assert_eq!(cmd.kind, DispatchedCommandKind::FocusPane);
    assert_eq!(cmd.session_id.as_deref(), Some("live-class-a"));
}

/// Class B (Unknown origin) + plain Enter on a Live row preserves
/// the legacy focus behavior — this exercises the most common
/// session management path (user-started `copilot` in a normal pane via hooks).
#[test]
fn enter_on_class_b_live_row_focuses() {
    use crate::agent_sessions::{CliSource, SessionEvent};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::path::PathBuf;
    let mut app = test_app();
    // SessionStarted defaults origin to Unknown (Class B).
    app.agent_sessions.apply(SessionEvent::SessionStarted {
        key: "live-class-b".into(),
        cli_source: CliSource::Copilot,
        pane_session_id: "00000000-0000-0000-0000-0000000000cc".into(),
        cwd: PathBuf::from("/x"),
        title: "t".into(),
    });

    app.current_tab_mut().current_view = View::Agents;
    app.current_tab_mut().agents_list_state.select(Some(0));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let cmd = app
        .last_dispatched_command_for_test()
        .expect("a command was dispatched");
    assert_eq!(cmd.kind, DispatchedCommandKind::FocusPane);
}

// ─── Phantom-session prune ───────────────────────────────────────

#[test]
fn agents_view_state_is_isolated_per_tab() {
    // Regression: opening the Agents picker in tab A should not show
    // up as opened (or with the same selection) when the user switches
    // to tab B. `current_view` and `agents_list_state` live on
    // TabSession exactly to keep these states independent.
    use crate::agent_sessions::{CliSource, SessionEvent};
    use std::path::PathBuf;
    let mut app = test_app();
    for k in ["a", "b", "c"] {
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: k.into(),
            cli_source: CliSource::Claude,
            pane_session_id: format!("p-{}", k),
            cwd: PathBuf::from("/x"),
            title: format!("t-{}", k),
        });
    }

    // Tab "0" (the seeded default): open picker, select row 2.
    app.tab_id = Some("0".into());
    app.current_tab_mut().current_view = View::Agents;
    app.current_tab_mut().agents_list_state.select(Some(2));

    // Switch to tab "1" — its TabSession is lazily created with
    // defaults: View::Chat and no selection.
    app.tab_id = Some("1".into());
    let tab1 = app.current_tab_mut();
    assert_eq!(tab1.current_view, View::Chat, "new tab must start in Chat");
    assert_eq!(tab1.agents_list_state.selected(), None);

    // Mutating tab 1 must not bleed back into tab 0.
    tab1.current_view = View::Agents;
    tab1.agents_list_state.select(Some(0));

    app.tab_id = Some("0".into());
    let tab0 = app.current_tab();
    assert_eq!(tab0.current_view, View::Agents);
    assert_eq!(tab0.agents_list_state.selected(), Some(2));
}

#[test]
fn closing_other_tab_preserves_per_tab_view_when_tab_changed_follows() {
    // Reproduces the user-reported bug:
    //   tab1 has the session list (agent session view) open. User opens
    //   tab2, then closes tab2. Focus returns to tab1, the agent
    //   pane is still visible, but the session list has vanished
    //   — the user has to press the shortcut again to bring it
    //   back.
    //
    // Root cause was on the C++ side: `_OnTabSelectionChanged`
    // is suppressed during tab removal, so the
    // `_NotifyAgentTabChanged(tab1)` that normally follows the
    // auto-selection of the previous tab never fired. wta's
    // `tab_id` got nulled by `tab_closed` and never restored, so
    // `current_tab()` silently fell back to the empty
    // `DEFAULT_TAB_ID` slot. After the C++ fix
    // (explicit `_ReconcileAgentPaneForActiveTab` post-removal),
    // wta receives the missing `tab_changed { tab_id: tab1 }`
    // event and `current_tab()` resolves back to tab1's
    // preserved TabSession with `View::Agents` intact.
    //
    // This test simulates the full wta-side event sequence:
    //   1. tab1 active, picker open with selection at row 2.
    //   2. user clicks tab2 → tab_changed { tab_id: tab2 }.
    //   3. user closes tab2 → tab_closed { tab_id: tab2 }.
    //   4. C++ fires the post-removal reconcile →
    //      tab_changed { tab_id: tab1 }.
    // After (4), `current_tab()` must return tab1's TabSession
    // with View::Agents and the row-2 selection preserved.
    use crate::agent_sessions::{CliSource, SessionEvent};
    use std::path::PathBuf;
    let mut app = test_app();
    for k in ["a", "b", "c"] {
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: k.into(),
            cli_source: CliSource::Claude,
            pane_session_id: format!("p-{}", k),
            cwd: PathBuf::from("/x"),
            title: format!("t-{}", k),
        });
    }

    // (1) tab1 active, agent session view, selection at row 2.
    let tab1 = "tab1-stable-id";
    let tab2 = "tab2-stable-id";
    app.tab_id = Some(tab1.into());
    app.current_tab_mut().current_view = View::Agents;
    app.current_tab_mut().agents_list_state.select(Some(2));

    // (2) User clicks tab2: switch_tab_session simulates the
    // arrival of `tab_changed { tab_id: tab2 }`.
    app.switch_tab_session(tab2.into());
    // tab2 starts at defaults; tab1 entry is untouched in the map.
    assert_eq!(app.current_tab().current_view, View::Chat);

    // (3) User closes tab2: drop_tab_session simulates
    // `tab_closed { tab_id: tab2 }`. tab2's entry is removed and
    // tab_id is nulled (DEFAULT_TAB_ID slot lazily created).
    app.drop_tab_session(tab2);
    assert!(
        app.tab_id.is_none(),
        "drop of active tab must null tab_id pending the next tab_changed"
    );

    // Critical: BEFORE the C++ fix, this is where wta is left
    // stranded — no further `tab_changed` ever arrives. The user
    // sees the agent pane stuck on DEFAULT_TAB_ID's empty Chat
    // view even though tab1's state is still in the map.
    // Demonstrate the bug shape:
    assert_eq!(
        app.current_tab().current_view,
        View::Chat,
        "without the follow-up tab_changed, current_tab falls back to DEFAULT_TAB_ID"
    );

    // (4) The C++ fix: post-removal reconcile fires
    // `_NotifyAgentTabChanged(tab1)` which lands here as
    // `switch_tab_session(tab1)`.
    app.switch_tab_session(tab1.into());

    // Now current_tab resolves back to tab1's preserved state.
    assert_eq!(
        app.current_tab().current_view,
        View::Agents,
        "tab1's View::Agents must be preserved across tab2's open/close"
    );
    assert_eq!(
        app.current_tab().agents_list_state.selected(),
        Some(2),
        "tab1's list selection must be preserved"
    );
}

#[test]
fn autofix_still_triggers_for_non_agent_pane() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.autofix_enabled = true;
    // No SessionStarted apply -> pane is not an agent pane.
    let pane = "non-agent-pane-guid";

    let notification = WtNotification {
        severity: WtEventSeverity::Actionable,
        pane_id: pane.to_string(),
        tab_id: Some("test-tab".to_string()),
        summary: "Command failed (exit 1)".to_string(),
        acknowledged: false,
        age_ticks: 0,
    };
    app.maybe_trigger_autofix(&notification);

    assert_eq!(
        app.tab_mut("test-tab").autofix.pane_id.as_deref(),
        Some(pane),
        "autofix must still arm normal panes when a command fails"
    );
    // The target tab's turn (not the active tab's) should be in-flight.
    assert!(
        !app.tab_mut("test-tab").turn.is_idle(),
        "autofix prompt should be in-flight on the target tab"
    );
}

/// F3: a transport death (helper `handle_io` watchdog) moves the UI out of
/// `Connected`, and its connection.lost ("/restart") line must survive even
/// when a different error (e.g. the in-flight prompt failure, "returned as
/// is") is already shown — only identical consecutive errors collapse, so
/// the recovery hint is never hidden.
#[test]
fn transport_loss_surfaces_restart_hint_even_behind_another_error() {
    let lost = t!("connection.lost").into_owned();
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    // In-flight prompt fails first (raw), then the watchdog's connection.lost.
    app.handle_event(AppEvent::AgentError {
        session_id: None,
        failure: crate::protocol::acp::failure::AgentFailure::Protocol {
            code: -32603,
            message: "pipe closed".to_string(),
        },
        message: "prompt error: pipe closed".to_string(),
    });
    app.handle_event(AppEvent::AgentError {
        session_id: None,
        failure: crate::protocol::acp::failure::AgentFailure::TransportLost,
        message: lost.clone(),
    });
    assert!(
        matches!(app.state, ConnectionState::Failed(_)),
        "a transport loss must move the UI out of Connected (F3)"
    );
    assert!(
        app.current_tab()
            .messages
            .iter()
            .any(|m| matches!(m, ChatMessage::Error(s) if *s == lost)),
        "the connection.lost /restart hint must be shown, not hidden behind the raw error"
    );
    // An identical connection.lost arriving again must not stack a duplicate.
    app.handle_event(AppEvent::AgentError {
        session_id: None,
        failure: crate::protocol::acp::failure::AgentFailure::TransportLost,
        message: lost.clone(),
    });
    let n = app
        .current_tab()
        .messages
        .iter()
        .filter(|m| matches!(m, ChatMessage::Error(s) if *s == lost))
        .count();
    assert_eq!(n, 1, "identical connection.lost must not duplicate");
}

/// `is_post_login_auth_failure` must catch BOTH the plain `AuthRequired`
/// and the `HandshakeFailed { NewSession }` the pipe client wraps a
/// still-AuthRequired post-login `new_session` into — `is_auth()` alone
/// would miss the latter and the auth recovery would never fire. It must
/// NOT match `HandshakeFailed { Authenticate }` (a genuine authenticate
/// RPC rejection/timeout) — that routes to sign-in, not a master restart.
#[test]
fn post_login_auth_failure_matches_auth_required_and_handshake_new_session() {
    use crate::protocol::acp::failure::{AgentFailure, HandshakeStage};
    assert!(is_post_login_auth_failure(&AgentFailure::AuthRequired {
        message: "auth".to_string()
    }));
    assert!(is_post_login_auth_failure(&AgentFailure::HandshakeFailed {
        stage: HandshakeStage::NewSession,
        detail: "still auth after authenticate".to_string()
    }));
    // An authenticate-RPC rejection/timeout must NOT trigger auth recovery
    // (a master restart can't fix bad credentials) — it routes to sign-in.
    assert!(!is_post_login_auth_failure(&AgentFailure::HandshakeFailed {
        stage: HandshakeStage::Authenticate,
        detail: "authenticate rejected/timed out".to_string()
    }));
    // A non-auth handshake stage must NOT trigger auth recovery.
    assert!(!is_post_login_auth_failure(&AgentFailure::HandshakeFailed {
        stage: HandshakeStage::Initialize,
        detail: "boom".to_string()
    }));
}

#[test]
fn post_login_master_unavailable_matches_only_pipe_connect() {
    use crate::protocol::acp::failure::{AgentFailure, HandshakeStage};

    assert!(is_post_login_master_unavailable(
        &AgentFailure::HandshakeFailed {
            stage: HandshakeStage::PipeConnect,
            detail: "pipe missing".to_string()
        }
    ));
    assert!(!is_post_login_master_unavailable(
        &AgentFailure::HandshakeFailed {
            stage: HandshakeStage::Initialize,
            detail: "init failed".to_string()
        }
    ));
    assert!(!is_post_login_master_unavailable(
        &AgentFailure::HandshakeFailed {
            stage: HandshakeStage::Authenticate,
            detail: "auth failed".to_string()
        }
    ));
    assert!(!is_post_login_master_unavailable(
        &AgentFailure::HandshakeFailed {
            stage: HandshakeStage::NewSession,
            detail: "session failed".to_string()
        }
    ));
}

#[test]
fn typed_pipe_connect_failure_survives_classify_anyhow() {
    use crate::protocol::acp::failure::{AgentFailure, HandshakeStage};

    let err = anyhow::Error::new(AgentFailure::HandshakeFailed {
        stage: HandshakeStage::PipeConnect,
        detail: "connect to master pipe after 3 attempts: missing".into(),
    });

    assert_eq!(
        crate::protocol::acp::failure::classify_anyhow(&err, HandshakeStage::Initialize),
        AgentFailure::HandshakeFailed {
            stage: HandshakeStage::PipeConnect,
            detail: "connect to master pipe after 3 attempts: missing".into(),
        }
    );
}

#[test]
fn post_login_recovery_route_covers_pipe_connect_without_external_auth_gate() {
    use crate::protocol::acp::failure::{AgentFailure, HandshakeStage};

    let pipe_connect = AgentFailure::HandshakeFailed {
        stage: HandshakeStage::PipeConnect,
        detail: "pipe missing".to_string(),
    };
    assert!(
        should_trigger_post_login_recovery(
            true,
            false,
            &pipe_connect
        ),
        "post-login master-unavailable recovery must not be gated on External auth flow"
    );
    assert!(
        !should_trigger_post_login_recovery(
            false,
            false,
            &pipe_connect
        ),
        "non-post-login pipe failures should surface normally"
    );

    let still_auth = AgentFailure::HandshakeFailed {
        stage: HandshakeStage::NewSession,
        detail: "still auth".to_string(),
    };
    assert!(
        should_trigger_post_login_recovery(true, true, &still_auth),
        "external post-login auth failures still recover via fresh master"
    );
    assert!(
        !should_trigger_post_login_recovery(true, false, &still_auth),
        "non-external auth failures should not use auth-stale recovery"
    );
}

/// `PostLoginAuthRecovery` shows a transient "Reconnecting…" (NOT the
/// sign-in screen, so there is no flash), and the `AuthRecoveryTimedOut`
/// dead-man only falls back to the sign-in screen if the restart never
/// took effect (this helper survived the window).
#[test]
fn post_login_auth_recovery_shows_reconnecting_then_signin_fallback() {
    let mut app = test_app();
    app.handle_event(AppEvent::PostLoginAuthRecovery {
        failure: crate::protocol::acp::failure::AgentFailure::AuthRequired {
            message: "auth".to_string(),
        },
        tab_id: None,
        agent_id: "copilot".to_string(),
    });
    // Common case: transient Reconnecting, NOT the setup screen (no flash).
    assert!(
        !matches!(app.mode, AppMode::Setup),
        "recovery must NOT flash the sign-in screen"
    );
    assert!(
        matches!(app.state, ConnectionState::Connecting(_)),
        "recovery must show a transient Reconnecting state"
    );
    let generation = app.auth_recovery_generation;
    // A STALE timer (older generation) must be ignored — it must not force
    // the sign-in screen onto the current Connecting state.
    app.handle_event(AppEvent::AuthRecoveryTimedOut {
        agent_id: "copilot".to_string(),
        generation: generation.wrapping_sub(1),
    });
    assert!(
        !matches!(app.mode, AppMode::Setup),
        "a stale-generation timeout must be ignored"
    );
    // Dead-man fallback (restart never took effect) → sign-in screen.
    app.handle_event(AppEvent::AuthRecoveryTimedOut {
        agent_id: "copilot".to_string(),
        generation,
    });
    assert!(
        matches!(app.mode, AppMode::Setup),
        "timeout fallback must surface the sign-in screen"
    );
}

/// The degraded latch (`App::transport_lost`) drives the slash-command
/// greying. It must arm on a transport loss and stay armed (the helper has
/// no in-process reconnect), so the popup keeps refusing everything but
/// /restart until recovery.
#[test]
fn transport_lost_latch_arms_on_transport_loss() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    assert!(!app.transport_lost, "fresh app is not degraded");

    app.handle_event(AppEvent::AgentError {
        session_id: None,
        failure: crate::protocol::acp::failure::AgentFailure::TransportLost,
        message: t!("connection.lost").into_owned(),
    });

    assert!(
        app.transport_lost,
        "a transport loss must arm the degraded latch"
    );
}

/// A non-transport failure (a one-off protocol error) must NOT arm the
/// latch — the session is still alive, so commands stay enabled.
#[test]
fn protocol_error_does_not_arm_degraded_latch() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;

    app.handle_event(AppEvent::AgentError {
        session_id: None,
        failure: crate::protocol::acp::failure::AgentFailure::Protocol {
            code: -32603,
            message: "bad params".to_string(),
        },
        message: "protocol error".to_string(),
    });

    assert!(
        !app.transport_lost,
        "a non-transport protocol error must not degrade the pane"
    );
}

/// An auth failure routes to sign-in, not the dead-transport path, so it
/// must not arm the degraded latch (otherwise the post-sign-in pane would
/// wrongly grey out its commands).
#[test]
fn auth_failure_does_not_arm_degraded_latch() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;

    app.handle_event(AppEvent::AgentError {
        session_id: None,
        failure: crate::protocol::acp::failure::AgentFailure::AuthRequired {
            message: "authentication required".to_string(),
        },
        message: "authentication required".to_string(),
    });

    assert!(
        !app.transport_lost,
        "an auth failure must not arm the degraded latch"
    );
}

/// A fresh connection (e.g. the post-sign-in reconnect that goes back
/// through master) must clear the latch so commands re-enable.
#[test]
fn agent_connected_clears_degraded_latch() {
    let mut app = test_app();
    app.transport_lost = true;

    app.handle_event(AppEvent::AgentConnected {
        name: "Copilot".to_string(),
        model: None,
        version: None,
        session_id: "sid-fresh".to_string(),
        available_models: Vec::new(),
        current_model_id: None,
        load_session_supported: true,
        image_supported: false,
    });

    assert!(
        !app.transport_lost,
        "reaching Connected must clear the degraded latch"
    );
}

/// Auth failures must reach the sign-in screen, not get flattened to a dead
/// `connection.lost`. Classification is typed (`AgentFailure::AuthRequired`),
/// done once at the helper boundary, so the handler routes purely on the
/// discriminant — no substring matching of the message text.
#[test]
fn auth_error_routes_to_signin_not_connection_lost() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.handle_event(AppEvent::AgentError {
        session_id: None,
        failure: crate::protocol::acp::failure::AgentFailure::AuthRequired {
            message: "authentication required".to_string(),
        },
        message: "new_session over master pipe failed: authentication required"
            .to_string(),
    });
    assert_eq!(
        app.mode,
        AppMode::Setup,
        "an auth failure must route to the sign-in screen"
    );
    assert!(
        !matches!(app.state, ConnectionState::Failed(_)),
        "an auth failure must not become a Failed connection-lost state"
    );
}

/// A soft stop is an *outcome*, not a connection failure: the handler must
/// append an informational System line carrying the localized reason text,
/// while leaving the connection `Connected` and never routing to the
/// sign-in screen. This is what keeps soft stops off the `AgentFailure`
/// axis — the gap the client-level emit test cannot cover.
#[test]
fn soft_stop_appends_system_line_without_changing_state() {
    use crate::protocol::acp::soft_stop::SoftStopReason;
    let mut app = test_app();
    app.state = ConnectionState::Connected;

    app.handle_event(AppEvent::AgentSoftStop {
        session_id: "0".to_string(),
        reason: SoftStopReason::Refusal,
    });

    let expected = t!("system.stopped_refusal").into_owned();
    assert!(
        app.current_tab()
            .messages
            .iter()
            .any(|m| matches!(m, ChatMessage::System(s) if *s == expected)),
        "a soft stop must append its localized System line"
    );
    assert!(
        matches!(app.state, ConnectionState::Connected),
        "a soft stop must not change the connection state"
    );
    assert_ne!(
        app.mode,
        AppMode::Setup,
        "a soft stop is not a failure — it must never route to sign-in"
    );
    assert!(
        !app.current_tab()
            .messages
            .iter()
            .any(|m| matches!(m, ChatMessage::Error(_))),
        "a soft stop must not surface an Error line"
    );
}

/// Each `SoftStopReason` must resolve to its own distinct localized line so
/// the user can tell truncation from a request-budget stop from a refusal.
#[test]
fn soft_stop_reasons_map_to_distinct_localized_lines() {
    use crate::protocol::acp::soft_stop::SoftStopReason;
    for (reason, key) in [
        (SoftStopReason::MaxTokens, "system.stopped_max_tokens"),
        (
            SoftStopReason::MaxTurnRequests,
            "system.stopped_max_turn_requests",
        ),
        (SoftStopReason::Refusal, "system.stopped_refusal"),
    ] {
        let mut app = test_app();
        app.handle_event(AppEvent::AgentSoftStop {
            session_id: "0".to_string(),
            reason,
        });
        let expected = t!(key).into_owned();
        assert!(
            app.current_tab()
                .messages
                .iter()
                .any(|m| matches!(m, ChatMessage::System(s) if *s == expected)),
            "reason {reason:?} must render the {key} line"
        );
    }
}

/// F7: while `Connecting`, the activity frame must keep advancing on Tick so
/// the indicator animates and a cold start doesn't look frozen.
#[test]
fn connecting_state_advances_activity_frame_on_tick() {
    let mut app = test_app();
    app.state = ConnectionState::Connecting("Initializing ACP...".to_string());
    let before = app.activity_frame;
    app.handle_event(AppEvent::Tick);
    assert_ne!(
        app.activity_frame, before,
        "the connecting indicator must keep animating (F7)"
    );
}

/// `connection_state: closed/failed` is pane-process termination, not
/// a shell command failure — it carries no exit code, no command
/// context, and the pane is gone so any follow-up ReadPaneOutput
/// would trip E_FAIL. The dispatcher in `handle_event` only routes
/// `vt_sequence` events to autofix; this asserts the connection_state
/// path stays banner-only.
#[test]
fn connection_state_closed_does_not_trigger_autofix_even_when_binding_cleared() {
    use crate::agent_sessions::{CliSource, SessionEvent};
    use std::path::PathBuf;
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.autofix_enabled = true;
    let pane = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

    // Bind, then unbind — mirrors the Copilot order: agent.session.end
    // hook arrives and runs SessionStopped before WT emits closed.
    // The session is NOT tagged with `SessionOrigin::AgentPane` (this
    // test sets up state via raw SessionStarted, so origin defaults
    // to Unknown), which means SessionStopped immediately transitions
    // to Ended and releases the pane binding — exactly the precondition
    // this test depends on.
    app.agent_sessions.apply(SessionEvent::SessionStarted {
        key: "copilot-key".into(),
        cli_source: CliSource::Copilot,
        pane_session_id: pane.into(),
        cwd: PathBuf::from("/work"),
        title: "t".into(),
    });
    app.agent_sessions.apply(SessionEvent::SessionStopped {
        key: "copilot-key".into(),
        reason: "user_exit".into(),
    });
    // Sanity: binding is gone, so the inner is_agent_pane guard alone
    // would not catch this.
    assert!(!app.agent_sessions.is_agent_pane(pane));

    app.handle_event(AppEvent::WtEvent {
        method: "connection_state".to_string(),
        pane_id: pane.to_string(),
        tab_id: None,
        params: serde_json::json!({"session_id": pane, "state": "closed"}),
    });

    assert!(
        app.tab_sessions
            .values()
            .all(|t| t.autofix.pane_id.is_none()),
        "connection_state:closed must never arm autofix — no exit code, \
         no command context, pane is dead so subsequent ReadPaneOutput \
         would throw E_FAIL"
    );
    assert!(
        app.current_tab().turn.is_idle(),
        "no autofix prompt should be in-flight"
    );
    // The pane-closed event surfaces via the banner / `wt_notifications`,
    // never in chat. Chat is the agent dialogue surface.
    assert!(
        app.current_tab().messages.is_empty(),
        "WT events must not push into chat history"
    );
    assert!(app.show_notification_banner);
}

/// Regression: a stale agent-CLI binding in the registry must NOT eat a
/// real shell command failure. OSC 133;D is emitted by shell integration
/// (PowerShell/bash), never by an agent CLI, so a D arriving in an
/// "agent-bound" pane implies the binding is a ghost — typically left
/// over from a hook that misreported `pane_id`, or from the previous
/// agent CLI having exited without the registry catching it yet.
/// Real-world repro: autofix runs Copilot, Copilot's hooks emit events
/// with `pane_id` = the source (user's) pane, registry registers the
/// user's PowerShell pane as Copilot-bound, then the next typo there
/// silently dies in the suppression check.
#[test]
fn ghost_agent_binding_does_not_suppress_shell_failure() {
    use crate::agent_sessions::{CliSource, SessionEvent};
    use std::path::PathBuf;
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.autofix_enabled = true;
    let pane = "11111111-2222-3333-4444-555555555555";
    app.agent_sessions.apply(SessionEvent::SessionStarted {
        key: "copilot-key".into(),
        cli_source: CliSource::Copilot,
        pane_session_id: pane.into(),
        cwd: PathBuf::from("/work"),
        title: "t".into(),
    });
    assert!(app.agent_sessions.is_agent_pane(pane), "precondition: pane is registered as agent-bound");

    app.handle_event(AppEvent::WtEvent {
        method: "vt_sequence".to_string(),
        pane_id: pane.to_string(),
        tab_id: Some("test-tab".to_string()),
        params: serde_json::json!({
            "session_id": pane,
            "sequence": "osc:133;D;1",
        }),
    });

    assert_eq!(
        app.tab_mut("test-tab").autofix.pane_id.as_deref(),
        Some(pane),
        "shell failure must arm autofix even when the registry still holds a stale agent binding for the pane"
    );
}

/// Positive coverage: a vt_sequence (osc:133;D;1) in a normal shell pane
/// still fires autofix (the proper command-failure signal). Ensures the
/// new "vt_sequence-only" routing doesn't silently disable autofix.
#[test]
fn vt_sequence_failure_in_normal_pane_still_triggers_autofix() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.autofix_enabled = true;
    let pane = "fedcba98-7654-3210-fedc-ba9876543210";

    app.handle_event(AppEvent::WtEvent {
        method: "vt_sequence".to_string(),
        pane_id: pane.to_string(),
        tab_id: Some("test-tab".to_string()),
        params: serde_json::json!({
            "session_id": pane,
            "sequence": "osc:133;D;1",
        }),
    });

    assert_eq!(
        app.tab_mut("test-tab").autofix.pane_id.as_deref(),
        Some(pane),
        "vt_sequence osc:133;D;<non-zero> in a normal pane must still arm autofix"
    );
}

fn vt_event(pane: &str, tab: &str, seq: &str) -> AppEvent {
    AppEvent::WtEvent {
        method: "vt_sequence".to_string(),
        pane_id: pane.to_string(),
        tab_id: Some(tab.to_string()),
        params: serde_json::json!({ "session_id": pane, "sequence": seq }),
    }
}

/// Detected state must survive the `osc:133;A` that PowerShell emits
/// ~1ms after the triggering `osc:133;D` — that A is the trigger's
/// echo, not the user moving on. The NEXT prompt-start (after the
/// user actually does something) is what dismisses.
#[test]
fn detected_survives_trigger_echo_dismisses_on_next_prompt_start() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.autofix_enabled = false; // suggest-mode → produces Detected
    let pane = "11111111-2222-3333-4444-555555555555";
    let tab = "tab-A";

    // D;1 → Detected pill armed.
    app.handle_event(vt_event(pane, tab, "osc:133;D;1"));
    assert!(
        matches!(
            app.tab_mut(tab).autofix.bar_snapshot,
            AutofixBarSnapshot::Detected { .. }
        ),
        "D;1 must establish Detected"
    );
    assert_eq!(
        app.tab_mut(tab).autofix.trigger_echo_pane.as_deref(),
        Some(pane),
        "trigger_echo_pane must be armed at Detected set so the immediate A is consumed"
    );

    // Immediate A (PowerShell redrawing the prompt) — must NOT dismiss.
    app.handle_event(vt_event(pane, tab, "osc:133;A"));
    assert!(
        matches!(
            app.tab_mut(tab).autofix.bar_snapshot,
            AutofixBarSnapshot::Detected { .. }
        ),
        "the trigger-echo A must not dismiss Detected"
    );
    assert!(
        app.tab_mut(tab).autofix.trigger_echo_pane.is_none(),
        "trigger_echo_pane must be consumed by the echo A"
    );

    // A second A (user actually moved on) — must dismiss.
    app.handle_event(vt_event(pane, tab, "osc:133;A"));
    assert!(
        matches!(
            app.tab_mut(tab).autofix.bar_snapshot,
            AutofixBarSnapshot::Idle
        ),
        "a subsequent A (user moved on) must dismiss Detected"
    );
}

/// Pending state (auto-suggest on path: D arms `autofix.pane_id` and
/// emits Pending) must also survive the trigger-echo A and dismiss on
/// the next user-driven prompt-start. The Pending/Armed dismiss path
/// goes through `turn_cancel` (or its manual fallback when no ACP
/// session is bound).
#[test]
fn pending_survives_trigger_echo_dismisses_on_next_prompt_start() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.autofix_enabled = true; // LLM-call path → produces Pending
    let pane = "22222222-3333-4444-5555-666666666666";
    let tab = "tab-B";

    app.handle_event(vt_event(pane, tab, "osc:133;D;1"));
    assert_eq!(
        app.tab_mut(tab).autofix.pane_id.as_deref(),
        Some(pane),
        "D;1 must arm Pending (autofix.pane_id set)"
    );
    assert_eq!(
        app.tab_mut(tab).autofix.trigger_echo_pane.as_deref(),
        Some(pane),
    );

    // Echo A — Pending stays.
    app.handle_event(vt_event(pane, tab, "osc:133;A"));
    assert_eq!(
        app.tab_mut(tab).autofix.pane_id.as_deref(),
        Some(pane),
        "trigger-echo A must not cancel Pending"
    );

    // Real A — turn_cancel (or manual fallback) clears pane_id and bar.
    app.handle_event(vt_event(pane, tab, "osc:133;A"));
    assert!(
        app.tab_mut(tab).autofix.pane_id.is_none(),
        "subsequent A must cancel Pending"
    );
    assert!(
        matches!(
            app.tab_mut(tab).autofix.bar_snapshot,
            AutofixBarSnapshot::Idle
        ),
        "bar must return to Idle after Pending cancel"
    );
}

/// User clicks the Detected pill on a stable prompt → autofix
/// transitions Detected → Pending → Armed via the LLM call. No D
/// event is in flight during this transition, so no echo A is
/// coming. The next prompt-start the user produces must dismiss on
/// the FIRST Enter, not be eaten as a fake echo.
///
/// Bug repro before this fix: emit_autofix_state_pending used to
/// arm `trigger_echo_pane` unconditionally, so the forced-from-
/// Detected path planted a gate with no echo to consume. The
/// gate then ate the user's first real Enter.
#[test]
fn force_from_detected_does_not_arm_echo_gate() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.autofix_enabled = false; // suggest-mode produces Detected first
    let pane = "44444444-5555-6666-7777-888888888888";
    let tab = "tab-D";

    // D;1 → Detected (gate armed, echo A consumed below).
    app.handle_event(vt_event(pane, tab, "osc:133;D;1"));
    app.handle_event(vt_event(pane, tab, "osc:133;A")); // echo
    assert!(
        app.tab_mut(tab).autofix.trigger_echo_pane.is_none(),
        "echo A must consume the gate"
    );

    // User clicks the pill → forced trigger → Pending. This is on a
    // stable prompt with no D in flight — gate must NOT re-arm.
    let synth = WtNotification {
        severity: WtEventSeverity::Actionable,
        pane_id: pane.to_string(),
        tab_id: Some(tab.to_string()),
        summary: "Command failed (exit 1)".to_string(),
        acknowledged: false,
        age_ticks: 0,
    };
    app.trigger_autofix_inner(&synth, /*forced*/ true);
    assert!(
        app.tab_mut(tab).autofix.trigger_echo_pane.is_none(),
        "force-from-Detected path must not arm trigger_echo_pane — \
         no D is in flight, no echo A is coming, and arming would eat \
         the user's first dismiss Enter"
    );
}

/// Returning to Idle clears the echo guard. Otherwise, a stale
/// `trigger_echo_pane` could swallow a real prompt-start that arrives
/// long after the state has already been cleared by other means
/// (e.g. the user clicked the Suggested pill, then the autofix
/// re-fires later in the same pane).
#[test]
fn trigger_echo_pane_clears_when_state_returns_to_idle() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.autofix_enabled = false;
    let pane = "33333333-4444-5555-6666-777777777777";
    let tab = "tab-C";

    app.handle_event(vt_event(pane, tab, "osc:133;D;1"));
    assert_eq!(
        app.tab_mut(tab).autofix.trigger_echo_pane.as_deref(),
        Some(pane)
    );

    // Externally clear the bar (e.g. user dismissed via Esc / pill).
    let tab_owned = tab.to_string();
    app.emit_autofix_state_cleared(&tab_owned);
    assert!(
        app.tab_mut(tab).autofix.trigger_echo_pane.is_none(),
        "trigger_echo_pane must be released when bar transitions to Idle, \
         otherwise the next real prompt-start would be silently swallowed"
    );
}

/// Gemini "manual launch" scenario: the user opened a normal pwsh/cmd
/// pane and typed `gemini`. The hook bridge fires `agent.session.start`
/// (binding the pane) but `agent.session.end` is unreliable on `/exit`
/// (Gemini cancels its own hook chain), AND the pane stays alive after
/// Gemini exits because pwsh keeps running. So neither
/// `connection_state: closed` nor `SessionStopped` ever arrive.
///
/// The shell's FinalTerm prompt-start marker (`osc:133;A`) fires when
/// pwsh redraws its prompt after Gemini releases the foreground —
/// that's our signal.
#[test]
fn osc133_prompt_start_in_agent_pane_transitions_row_to_ended() {
    use crate::agent_sessions::{CliSource, SessionEvent};
    use std::path::PathBuf;
    let mut app = test_app();
    let pane = "ffffffff-eeee-dddd-cccc-bbbbbbbbbbbb";
    app.agent_sessions.apply(SessionEvent::SessionStarted {
        key: "gemini-key".into(),
        cli_source: CliSource::Gemini,
        pane_session_id: pane.into(),
        cwd: PathBuf::from("/work"),
        title: "t".into(),
    });
    // Sanity: row is live before the prompt-start arrives.
    assert!(app.agent_sessions.is_agent_pane(pane));

    app.handle_event(AppEvent::WtEvent {
        method: "vt_sequence".to_string(),
        pane_id: pane.to_string(),
        tab_id: None,
        params: serde_json::json!({
            "session_id": pane,
            "sequence": "osc:133;A",
        }),
    });

    let row = app
        .agent_sessions
        .iter_sorted()
        .into_iter()
        .find(|s| s.key == "gemini-key")
        .expect("row still exists");
    assert!(
        matches!(row.status, crate::agent_sessions::AgentStatus::Ended),
        "agent-bound pane seeing osc:133;A must transition to Ended",
    );
    // The pane→key binding must be cleared either way.
    assert!(
        !app.agent_sessions.is_agent_pane(pane),
        "pane binding should be cleared after close",
    );
}

/// Negative coverage: `osc:133;A` in a normal (non-agent) pane must
/// never apply PaneClosed (defensive — the registry would treat it as
/// a no-op anyway, but verify the guard short-circuits the call).
#[test]
fn osc133_prompt_start_in_normal_pane_is_inert() {
    let mut app = test_app();
    let pane = "00000000-1111-2222-3333-444444444444";
    // No SessionStarted apply -> not an agent pane.
    app.handle_event(AppEvent::WtEvent {
        method: "vt_sequence".to_string(),
        pane_id: pane.to_string(),
        tab_id: None,
        params: serde_json::json!({
            "session_id": pane,
            "sequence": "osc:133;A",
        }),
    });
    // Nothing to assert positively — the registry just doesn't grow.
    assert_eq!(app.agent_sessions.iter_sorted().len(), 0);
}

/// Gemini scenario: no `agent.session.end` hook bridge, so the only
/// signal we get when the user `/exit`s a resumed Gemini pane is
/// WT-native `connection_state: closed`. Without bridging that into a
/// `SessionEvent::PaneClosed`, the row stays stuck at Idle/Working
/// forever in the session management list.
#[test]
fn connection_state_closed_transitions_agent_row_to_ended() {
    use crate::agent_sessions::{AgentStatus, CliSource, SessionEvent};
    use std::path::PathBuf;
    let mut app = test_app();
    let pane = "deadbeef-1111-2222-3333-444455556666";
    // Gemini-style: the pane was bound (via ResumePaneAssigned in real
    // life; SessionStarted is a stand-in here) but no session.end hook
    // ever fires.
    app.agent_sessions.apply(SessionEvent::SessionStarted {
        key: "gemini-key".into(),
        cli_source: CliSource::Gemini,
        pane_session_id: pane.into(),
        cwd: PathBuf::from("/work"),
        title: "t".into(),
    });
    // Sanity: the row is live before close.
    let s = app
        .agent_sessions
        .iter_sorted()
        .into_iter()
        .find(|s| s.key == "gemini-key")
        .expect("row exists");
    assert!(matches!(s.status, AgentStatus::Idle | AgentStatus::Working));

    app.handle_event(AppEvent::WtEvent {
        method: "connection_state".to_string(),
        pane_id: pane.to_string(),
        tab_id: None,
        params: serde_json::json!({"session_id": pane, "state": "closed"}),
    });

    let row = app
        .agent_sessions
        .iter_sorted()
        .into_iter()
        .find(|s| s.key == "gemini-key")
        .expect("row still exists");
    assert!(
        matches!(row.status, AgentStatus::Ended),
        "Gemini row must transition to Ended on connection_state:closed",
    );
    assert!(
        !app.agent_sessions.is_agent_pane(pane),
        "pane binding should be cleared after close",
    );
}

/// Regression: OSC 133;A in an AGENT-PANE-origin session must NOT
/// trigger PaneClosed. The previous gate (`is_agent_pane(pane_id)`)
/// fired on any pane with a bound session, demoting agent panes
/// when WT itself emitted a stray OSC 133;A around focus events.
/// Fix at app.rs ~4717 restricts the bridge to origin=Unknown
/// (shell-pane agents like `gemini` typed in pwsh).
#[test]
fn osc133_prompt_start_in_agent_pane_origin_is_ignored() {
    use crate::agent_sessions::{CliSource, SessionEvent, SessionOrigin};
    use std::path::PathBuf;
    let mut app = test_app();
    let pane = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    let key = "copilot-agent-pane-key";
    app.agent_sessions.apply(SessionEvent::SessionStarted {
        key: key.into(),
        cli_source: CliSource::Copilot,
        pane_session_id: pane.into(),
        cwd: PathBuf::from("/work"),
        title: "t".into(),
    });
    // Stamp this row as agent-pane origin (the wta-managed kind).
    app.agent_sessions.set_origin(key, SessionOrigin::AgentPane);

    // Sanity: row is Live before the stray OSC arrives.
    let before = app
        .agent_sessions
        .iter_sorted()
        .into_iter()
        .find(|s| s.key == key)
        .expect("row exists");
    assert!(matches!(
        before.status,
        crate::agent_sessions::AgentStatus::Idle
            | crate::agent_sessions::AgentStatus::Working
    ));
    assert_eq!(before.origin, SessionOrigin::AgentPane);

    // Fire OSC 133;A — this is the event WT spuriously emits
    // around focus_pane on agent panes. The handler must IGNORE
    // it for agent-pane origin and leave the row Live.
    app.handle_event(AppEvent::WtEvent {
        method: "vt_sequence".to_string(),
        pane_id: pane.to_string(),
        tab_id: None,
        params: serde_json::json!({
            "session_id": pane,
            "sequence": "osc:133;A",
        }),
    });

    let after = app
        .agent_sessions
        .iter_sorted()
        .into_iter()
        .find(|s| s.key == key)
        .expect("row must still exist (must NOT be pruned by spurious PaneClosed)");
    assert!(
        matches!(
            after.status,
            crate::agent_sessions::AgentStatus::Idle
                | crate::agent_sessions::AgentStatus::Working
        ),
        "agent-pane row must stay Live on OSC 133;A; got {:?}",
        after.status,
    );
    assert!(
        app.agent_sessions.is_agent_pane(pane),
        "pane binding must NOT be cleared by a spurious shell-prompt OSC",
    );
}

// ─── turn-state integration tests ──────────────────────────────────────
//
// Drive `App` directly through the turn-state transitions in
// `doc/specs/turn-state-refactor.md`'s table. We use the active tab's
// `DEFAULT_TAB_ID` as the session key — `session_tab_mut` falls back to
// the active tab when the id is unknown, which keeps these tests free
// of ACP wiring.

fn submit_test_prompt(app: &mut App, text: &str) {
    let prompt = SubmittedPrompt {
        id: 42,
        text: text.into(),
        submitted_at_unix_s: 0.0,
        autofix: None,
    };
    app.turn_submit_prompt(DEFAULT_TAB_ID, prompt);
}

/// Form A end-to-end (mock-acp-agent spec, "option 2"): the mock + real
/// `WtaClient` harness lives in the acp module (it needs the private
/// `WtaClient`), but this App-state assertion lives here where `App`
/// internals are reachable. We drive a prompt through the **real** ACP
/// client against the deterministic mock, pump the resulting `AppEvent`s
/// into a **real** `App`, and assert the streamed reply is what the chat
/// view would show — i.e. what the chat should display is covered without a
/// real terminal, real WT, or an LLM.
#[tokio::test]
async fn mock_agent_reply_streams_into_app_chat() {
    use crate::protocol::acp::client::mock_agent_tests::connect_mock_agent;
    use agent_client_protocol as acp;
    

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            // Borrow the acp-module harness: deterministic mock wired to a
            // real WtaClient over an in-memory duplex.
            let (conn, mut event_rx, _seen) = connect_mock_agent();
            conn.initialize(acp::schema::v1::InitializeRequest::new(acp::schema::ProtocolVersion::LATEST))
                .await
                .expect("initialize failed");
            let session = conn
                .new_session(acp::schema::v1::NewSessionRequest::new("/test"))
                .await
                .expect("new_session failed");
            conn.prompt(acp::schema::v1::PromptRequest::new(
                session.session_id.clone(),
                vec!["hello".into()],
            ))
            .await
            .expect("prompt failed");

            // Real App with an in-flight turn so streamed chunks are accepted
            // (the AgentMessageChunk handler drops chunks on an idle turn).
            let mut app = test_app();
            submit_test_prompt(&mut app, "hello");

            // Pump the AppEvents the real WtaClient produced into the real
            // App until the agent message chunk has been applied (bounded so
            // a wiring bug fails fast instead of hanging).
            let pumped = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    match event_rx.recv().await {
                        Some(ev) => {
                            let is_chunk = matches!(ev, AppEvent::AgentMessageChunk { .. });
                            app.handle_event(ev);
                            if is_chunk {
                                break;
                            }
                        }
                        None => break,
                    }
                }
            })
            .await;
            assert!(pumped.is_ok(), "timed out waiting for the agent message chunk");

            // "What the chat shows" while streaming: the mock's reply is in
            // the active tab's streaming buffer.
            assert!(
                app.current_tab()
                    .pending_agent_response
                    .contains("MOCK_OK:hello"),
                "mock reply must stream into the App chat buffer; got {:?}",
                app.current_tab().pending_agent_response
            );
        })
        .await;
}

/// Drive a prompt through the real ACP client against a mock that requests
/// permission, pump the `PermissionRequest` into a real `App`, then simulate
/// the user's key choice and assert the chosen option round-trips back to
/// the agent. `expected_keys` is the key sequence the user presses; `want`
/// is the option id the mock must end up recording.
async fn run_permission_scenario(expected_keys: &[KeyCode], want: &str) {
    use crate::protocol::acp::client::mock_agent_tests::connect_mock_agent_asking_permission;
    use agent_client_protocol as acp;
    

    let (conn, mut event_rx, outcome) = connect_mock_agent_asking_permission();
    conn.initialize(acp::schema::v1::InitializeRequest::new(acp::schema::ProtocolVersion::LATEST))
        .await
        .expect("initialize failed");
    let session = conn
        .new_session(acp::schema::v1::NewSessionRequest::new("/test"))
        .await
        .expect("new_session failed");
    conn.prompt(acp::schema::v1::PromptRequest::new(
        session.session_id.clone(),
        vec!["do it".into()],
    ))
    .await
    .expect("prompt failed");

    // Real App with an in-flight turn so the permission request is accepted.
    let mut app = test_app();
    submit_test_prompt(&mut app, "do it");

    // Pump events until the PermissionRequest is applied to the App.
    let pumped = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match event_rx.recv().await {
                Some(ev) => {
                    let is_perm = matches!(ev, AppEvent::PermissionRequest { .. });
                    app.handle_event(ev);
                    if is_perm {
                        break;
                    }
                }
                None => break,
            }
        }
    })
    .await;
    assert!(pumped.is_ok(), "timed out waiting for the permission request");

    // Display assertion: the permission card is queued with allow/reject,
    // allow selected by default.
    {
        let perm = app
            .current_tab()
            .permission
            .front()
            .expect("a permission request must be queued for display");
        assert_eq!(perm.options.len(), 2, "expected allow + reject options");
        assert_eq!(perm.options[0].id, "allow-once");
        assert_eq!(perm.options[1].id, "reject-once");
        assert_eq!(perm.selected, 0, "allow must be selected by default");
    }

    // Simulate the user's key choice (e.g. Enter = allow, Right then Enter = reject).
    for key in expected_keys {
        app.handle_key(KeyEvent::from(*key));
    }

    // The choice must round-trip back to the agent.
    let resolved = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(v) = outcome.lock().unwrap().clone() {
                break v;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("timed out waiting for the permission outcome to reach the agent");
    assert_eq!(resolved, want, "the agent must receive the user's choice");

    // The card is cleared once resolved.
    assert!(
        app.current_tab().permission.is_empty(),
        "the permission card must clear after the user resolves it"
    );
}

/// Permission allow round-trip: Enter on the default-selected option (allow)
/// surfaces the card, then sends `allow-once` back to the agent.
#[tokio::test]
async fn permission_allow_round_trips_to_agent() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(run_permission_scenario(&[KeyCode::Enter], "allow-once"))
        .await;
}

/// Permission reject round-trip: Right moves selection to reject, Enter
/// sends `reject-once` back to the agent.
#[tokio::test]
async fn permission_reject_round_trips_to_agent() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(run_permission_scenario(
            &[KeyCode::Right, KeyCode::Enter],
            "reject-once",
        ))
        .await;
}

/// Regression (#permission-quick-keys): the `y` quick-key must resolve to
/// the allow option even though the wire `kind` is PascalCase (`AllowOnce`)
/// while the matcher searches for the lowercase substring `allow`. Before
/// the case-insensitive fix this keypress was a silent no-op and the agent
/// never received a response — this scenario would time out.
#[tokio::test]
async fn permission_quick_allow_key_round_trips_to_agent() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(run_permission_scenario(
            &[KeyCode::Char('y')],
            "allow-once",
        ))
        .await;
}

/// Regression (#permission-quick-keys): the `n` quick-key must resolve to
/// the reject option. See [`permission_quick_allow_key_round_trips_to_agent`].
#[tokio::test]
async fn permission_quick_reject_key_round_trips_to_agent() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(run_permission_scenario(
            &[KeyCode::Char('n')],
            "reject-once",
        ))
        .await;
}

/// The `kind` string is the ACP `PermissionOptionKind` rendered via
/// `format!("{:?}", …)`, i.e. PascalCase (`AllowOnce`, `RejectAlways`).
/// `PermOption::is_allow`/`is_reject` must match those case-insensitively
/// so the `y`/`n` quick-keys and the `[Y]`/`[N]` button labels both fire.
#[test]
fn perm_option_kind_matching_is_case_insensitive() {
    let opt = |kind: &str| PermOption {
        id: "id".into(),
        name: "name".into(),
        kind: kind.into(),
    };
    for k in ["AllowOnce", "AllowAlways", "allow_once"] {
        assert!(opt(k).is_allow(), "{k:?} must be recognized as allow");
        assert!(!opt(k).is_reject(), "{k:?} must not be reject");
    }
    for k in ["RejectOnce", "RejectAlways", "reject_once"] {
        assert!(opt(k).is_reject(), "{k:?} must be recognized as reject");
        assert!(!opt(k).is_allow(), "{k:?} must not be allow");
    }

    // PermissionState index helpers pick the first matching option.
    let perm = PermissionState {
        description: String::new(),
        options: vec![opt("AllowOnce"), opt("RejectOnce")],
        selected: 0,
        responder: None,
    };
    assert_eq!(perm.allow_index(), Some(0));
    assert_eq!(perm.reject_index(), Some(1));
}

/// Regression (issue #189): while the agent has queued a permission request
/// but `AgentMessageEnd` has not yet arrived (turn is
/// `Surfaced{end_pending:true}`), the thinking/activity indicator must
/// remain visible. Previously `spinner_label()` returned `None` for any
/// `Surfaced` variant, making the pane look frozen between the eager surface
/// and the permission card appearing.
#[test]
fn thinking_indicator_visible_while_permission_pending_and_end_pending() {
    let mut app = test_app();

    // Put the tab in `Surfaced{end_pending:true}` — the state that exists
    // between an eager surface (recommendation / chat turn visible) and the
    // `AgentMessageEnd` event that releases the UI gate. A permission
    // request can arrive in this window.
    let prompt = SubmittedPrompt {
        id: 1,
        text: "test".into(),
        submitted_at_unix_s: 0.0,
        autofix: None,
    };
    app.tab_mut(DEFAULT_TAB_ID).turn = TurnState::Surfaced {
        prompt,
        outcome: TurnOutcome::Empty,
        end_pending: true,
    };

    // The spinner must be active while end_pending=true.
    assert!(
        app.current_tab().turn.spinner_label().is_some(),
        "spinner_label must be Some while Surfaced{{end_pending:true}} (issue #189)"
    );
    assert!(
        app.has_activity_indicator(),
        "has_activity_indicator must be true while Surfaced{{end_pending:true}} (issue #189)"
    );

    // Simulate the PermissionRequest arriving in this window.
    app.tab_mut(DEFAULT_TAB_ID)
        .permission
        .push_back(PermissionState {
            description: "Allow tool X?".into(),
            options: vec![
                PermOption { id: "allow-once".into(), name: "Allow".into(), kind: "AllowOnce".into() },
                PermOption { id: "reject-once".into(), name: "Deny".into(), kind: "RejectOnce".into() },
            ],
            selected: 0,
            responder: None,
        });

    // With a queued permission AND end_pending=true the spinner must still be on.
    assert!(
        app.current_tab().turn.spinner_label().is_some(),
        "spinner_label must remain Some after PermissionRequest queued while end_pending=true"
    );
    assert!(
        app.has_activity_indicator(),
        "has_activity_indicator must remain true after PermissionRequest queued"
    );
}

/// Tool-call card: when the mock proposes a command (a `ToolCall`
/// notification), the real `WtaClient` turns it into `AppEvent::ToolCall`
/// and the real `App` surfaces a tool-call card in the chat — the display
/// state the insert/run affordance hangs off.
#[tokio::test]
async fn tool_call_surfaces_card_in_chat() {
    use crate::protocol::acp::client::mock_agent_tests::connect_mock_agent_proposing_tool;
    use agent_client_protocol as acp;
    

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (conn, mut event_rx) = connect_mock_agent_proposing_tool();
            conn.initialize(acp::schema::v1::InitializeRequest::new(acp::schema::ProtocolVersion::LATEST))
                .await
                .expect("initialize failed");
            let session = conn
                .new_session(acp::schema::v1::NewSessionRequest::new("/test"))
                .await
                .expect("new_session failed");
            conn.prompt(acp::schema::v1::PromptRequest::new(
                session.session_id.clone(),
                vec!["run it".into()],
            ))
            .await
            .expect("prompt failed");

            let mut app = test_app();
            submit_test_prompt(&mut app, "run it");

            let pumped = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    match event_rx.recv().await {
                        Some(ev) => {
                            let is_tool = matches!(ev, AppEvent::ToolCall { .. });
                            app.handle_event(ev);
                            if is_tool {
                                break;
                            }
                        }
                        None => break,
                    }
                }
            })
            .await;
            assert!(pumped.is_ok(), "timed out waiting for the tool call");

            // Display assertion: the proposed command shows as a tool-call card.
            let has_card = app.current_tab().messages.iter().any(|m| {
                matches!(m, ChatMessage::ToolCall { title, .. } if title == "Run: echo hi")
            });
            assert!(
                has_card,
                "a tool-call card must surface in the chat; got {:?}",
                app.current_tab().messages
            );
        })
        .await;
}

/// Pump `AppEvent`s into a real `App` until `pred` matches (inclusive), with
/// a timeout so a wiring bug fails fast instead of hanging.
async fn pump_until(
    app: &mut App,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    pred: impl Fn(&AppEvent) -> bool,
) {
    let r = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match rx.recv().await {
                Some(ev) => {
                    let stop = pred(&ev);
                    app.handle_event(ev);
                    if stop {
                        break;
                    }
                }
                None => break,
            }
        }
    })
    .await;
    assert!(r.is_ok(), "timed out pumping events");
}

/// Drive initialize → new_session → prompt against the harness connection,
/// leaving an in-flight turn whose streamed notifications the caller pumps
/// into a real `App`. Returns `()` — it only drives ACP traffic; the caller
/// owns the `App`.
async fn app_after_prompt(
    conn: &crate::protocol::acp::conn::ClientLink,
) {
    use agent_client_protocol as acp;
    
    conn.initialize(acp::schema::v1::InitializeRequest::new(acp::schema::ProtocolVersion::LATEST))
        .await
        .expect("initialize failed");
    let session = conn
        .new_session(acp::schema::v1::NewSessionRequest::new("/test"))
        .await
        .expect("new_session failed");
    conn.prompt(acp::schema::v1::PromptRequest::new(
        session.session_id.clone(),
        vec!["go".into()],
    ))
    .await
    .expect("prompt failed");
}

/// Streaming: a reply split across two `AgentMessageChunk`s must coalesce
/// into one contiguous streaming buffer in the chat.
#[tokio::test]
async fn streaming_two_chunks_coalesce_in_app_chat() {
    use crate::protocol::acp::client::mock_agent_tests::connect_mock_agent_streaming_two_chunks;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (conn, mut event_rx) = connect_mock_agent_streaming_two_chunks();
            app_after_prompt(&conn).await;

            let mut app = test_app();
            submit_test_prompt(&mut app, "go");

            // Two chunks arrive; pump each.
            pump_until(&mut app, &mut event_rx, |ev| {
                matches!(ev, AppEvent::AgentMessageChunk { .. })
            })
            .await;
            pump_until(&mut app, &mut event_rx, |ev| {
                matches!(ev, AppEvent::AgentMessageChunk { .. })
            })
            .await;

            assert_eq!(
                app.current_tab().pending_agent_response,
                "MOCK_OK",
                "streamed chunks must coalesce into one contiguous reply"
            );
        })
        .await;
}

/// Tool-call lifecycle: a `ToolCallUpdate(Completed)` after the initial
/// `ToolCall` must update the card's status in-place (not duplicate it).
#[tokio::test]
async fn tool_call_completion_updates_card_status() {
    use crate::protocol::acp::client::mock_agent_tests::connect_mock_agent_completing_tool;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (conn, mut event_rx) = connect_mock_agent_completing_tool();
            app_after_prompt(&conn).await;

            let mut app = test_app();
            submit_test_prompt(&mut app, "go");

            pump_until(&mut app, &mut event_rx, |ev| {
                matches!(ev, AppEvent::ToolCallUpdate { .. })
            })
            .await;

            let cards: Vec<_> = app
                .current_tab()
                .messages
                .iter()
                .filter_map(|m| match m {
                    ChatMessage::ToolCall { id, status, .. } => Some((id.clone(), status.clone())),
                    _ => None,
                })
                .collect();
            assert_eq!(cards.len(), 1, "the update must edit in place, not add a card");
            assert_eq!(cards[0].0, "mock-tool-1");
            assert_eq!(cards[0].1, "Completed", "card status must reflect the update");
        })
        .await;
}

/// Plan: a `Plan` notification must surface as a plan card with its entries.
#[tokio::test]
async fn plan_surfaces_card_in_chat() {
    use crate::protocol::acp::client::mock_agent_tests::connect_mock_agent_proposing_plan;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (conn, mut event_rx) = connect_mock_agent_proposing_plan();
            app_after_prompt(&conn).await;

            let mut app = test_app();
            submit_test_prompt(&mut app, "go");

            pump_until(&mut app, &mut event_rx, |ev| matches!(ev, AppEvent::Plan { .. })).await;

            let plan = app.current_tab().messages.iter().find_map(|m| match m {
                ChatMessage::Plan(entries) => Some(entries.clone()),
                _ => None,
            });
            let entries = plan.expect("a plan card must surface in the chat");
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].content, "Step one");
            assert_eq!(entries[0].status, PlanEntryStatus::InProgress);
            assert_eq!(entries[1].content, "Step two");
        })
        .await;
}

/// Render a driven `App` to a ratatui `TestBackend` and return the visible
/// buffer as text (rows joined by `\n`). Lets scenarios assert on what is
/// actually painted, not just on `App` state.
fn render_to_text(app: &mut App, width: u16, height: u16) -> String {
    use ratatui::{backend::TestBackend, Terminal};
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| crate::ui::render(frame, app))
        .expect("render must not panic");
    let buf = terminal.backend().buffer();
    let w = buf.area.width as usize;
    let mut out = String::new();
    for (i, cell) in buf.content.iter().enumerate() {
        if i > 0 && i % w == 0 {
            out.push('\n');
        }
        out.push_str(cell.symbol());
    }
    out
}

/// Render (C063 "prompt out-of-focus appearance"): when keyboard focus leaves the agent pane
/// (`pane_focused = false`) the input box must still look correct — the prompt marker and the
/// connection placeholder still paint, the box is not blanked or broken. Only the caret styling
/// changes (a solid REVERSED block when focused → DIM when not; input.rs:69/90), which is the
/// intended out-of-focus appearance.
#[test]
fn render_input_box_intact_when_pane_unfocused() {
    let _g = crate::test_support::lock_locale();
    rust_i18n::set_locale("en-US");
    let mut app = test_app();
    app.state = ConnectionState::Connected;

    // Focused baseline: the input box paints the prompt + connected placeholder.
    app.pane_focused = true;
    let focused = render_to_text(&mut app, 80, 24);
    let placeholder = rust_i18n::t!("input.placeholder.connected").into_owned();
    assert!(
        focused.contains('>') && focused.contains(&placeholder),
        "sanity: the focused input must paint the prompt + placeholder; rendered:\n{focused}"
    );

    // Focus leaves the pane: the input box must remain intact (prompt + placeholder still there),
    // i.e. losing focus does not blank or corrupt the input surface.
    app.pane_focused = false;
    let unfocused = render_to_text(&mut app, 80, 24);
    assert!(
        unfocused.contains('>'),
        "the out-of-focus input must still paint the prompt marker; rendered:\n{unfocused}"
    );
    assert!(
        unfocused.contains(&placeholder),
        "the out-of-focus input must still paint the connection placeholder (box intact); rendered:\n{unfocused}"
    );
}

/// Render (C067 "non-ASCII input"): non-ASCII characters typed into the agent-pane input must be
/// accepted and painted correctly (multi-byte UTF-8: accented Latin, Greek, CJK). Drives the real
/// key handler with `KeyCode::Char` events (a Rust `char` is a full Unicode scalar, exactly what a
/// keyboard/IME commit produces) and asserts they render. The E2E send path (wtcli send-keys)
/// cannot carry non-ASCII, so this unit test is the deterministic coverage for the product side;
/// the IME-composition half stays MANUAL. `insert_input_char` advances the caret by
/// `ch.len_utf8()` (app.rs:1842), so multi-byte chars must round-trip.
#[test]
fn render_agent_input_accepts_non_ascii() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let _g = crate::test_support::lock_locale();
    rust_i18n::set_locale("en-US");
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    let sample = "café Ω 你好";
    for c in sample.chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    // The input buffer holds the exact non-ASCII string (the product contract: non-ASCII input
    // is accepted verbatim, multi-byte caret advance included)...
    assert_eq!(
        app.current_tab().input,
        sample,
        "non-ASCII characters must be accepted verbatim into the input buffer"
    );
    // ...and the painted input line shows the multi-byte glyphs. (CJK are double-width; the
    // ratatui TestBackend splits a wide glyph across two cells so the raw cell-join may not
    // reconstruct the CJK codepoint — assert the single-width non-ASCII glyphs render, and rely
    // on the input-buffer assertion above for the wide-char acceptance contract.)
    let text = render_to_text(&mut app, 80, 24);
    for needle in ["café", "Ω"] {
        assert!(
            text.contains(needle),
            "the agent input must paint the non-ASCII text {needle:?}; rendered:\n{text}"
        );
    }
}

/// Render: a committed agent message must actually appear in the painted
/// chat view (not just in `App` state). Lifts `ui/chat.rs` coverage.
#[test]
fn render_chat_shows_agent_message() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.current_tab_mut()
        .messages
        .push(ChatMessage::Agent("VISIBLE_REPLY_XYZ".into()));

    let text = render_to_text(&mut app, 80, 24);
    assert!(
        text.contains("VISIBLE_REPLY_XYZ"),
        "the chat view must paint the agent message; rendered:\n{text}"
    );
}

/// Render (C134 "Hooks off behavior is safe"): with session management OFF — no tracked
/// sessions, exactly as when wt-agent-hooks are not installed — the session-management (Agents)
/// view must still paint a STABLE empty state (the draw does not panic and the navigation footer
/// hint is drawn) rather than a broken/blank surface.
#[test]
fn render_agents_view_empty_when_no_sessions_is_stable() {
    let mut app = test_app();
    let key = app.active_tab_key().to_string();
    // No SessionStarted events applied => the registry is empty, exactly as when session
    // management is off (no wt-agent-hooks tracking any sessions).
    assert!(
        app.agents_rows_for_tab(&key).is_empty(),
        "precondition: no tracked sessions (hooks off)"
    );
    app.current_tab_mut().current_view = View::Agents;

    // render_to_text asserts the draw does not panic.
    let text = render_to_text(&mut app, 80, 24);

    // The navigation footer hint (agents.footer_hint) is drawn in the empty state too; its
    // leading "↑ ↓" arrows are invariant across every bundled locale, so assert on those.
    assert!(
        text.contains('↑') && text.contains('↓'),
        "the empty session view must paint the stable navigation footer hint; rendered:\n{text}"
    );
}

/// Render: a queued permission request must paint its description and the
/// allow/reject option labels. Lifts `ui/permission.rs` coverage.
#[test]
fn render_permission_card_shows_options() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.current_tab_mut().permission.push_back(PermissionState {
        description: "Run: echo PERM_XYZ".into(),
        options: vec![
            PermOption {
                id: "allow-once".into(),
                name: "Allow once".into(),
                kind: "AllowOnce".into(),
            },
            PermOption {
                id: "reject-once".into(),
                name: "Reject".into(),
                kind: "RejectOnce".into(),
            },
        ],
        selected: 0,
        responder: None,
    });

    let text = render_to_text(&mut app, 80, 24);
    assert!(
        text.contains("PERM_XYZ"),
        "the permission card must paint its description; rendered:\n{text}"
    );
    assert!(
        text.contains("Allow once"),
        "the permission card must paint the allow option; rendered:\n{text}"
    );
}

/// Render: a tool-call card must paint its title in the chat. Lifts the
/// tool-call branch of `ui/chat.rs`.
#[test]
fn render_tool_call_card_in_chat() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.current_tab_mut().messages.push(ChatMessage::ToolCall {
        id: "mock-tool-1".into(),
        title: "Run: echo TOOL_XYZ".into(),
        status: "Pending".into(),
    });

    let text = render_to_text(&mut app, 80, 24);
    assert!(
        text.contains("TOOL_XYZ"),
        "the tool-call card must paint its title; rendered:\n{text}"
    );
}

/// Render: the `/help` overlay must list the slash commands. Lifts
/// `ui/command_popup.rs`.
#[test]
fn render_help_overlay_lists_commands() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.help_overlay_visible = true;

    let text = render_to_text(&mut app, 80, 24);
    assert!(
        text.contains("/restart"),
        "the help overlay must list slash commands; rendered:\n{text}"
    );
}

/// Render: the `/model` picker must list the advertised models. Lifts
/// `ui/model_popup.rs`.
#[test]
fn render_model_picker_lists_models() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.available_models = vec![
        AcpModelInfo {
            id: "pick-1".into(),
            name: "PickModelXYZ".into(),
            description: None,
        },
        AcpModelInfo {
            id: "pick-2".into(),
            name: "OtherModel".into(),
            description: None,
        },
    ];
    app.current_tab_mut().model_picker_open = true;

    let text = render_to_text(&mut app, 80, 24);
    assert!(
        text.contains("PickModelXYZ"),
        "the model picker must list the advertised models; rendered:\n{text}"
    );
}

#[test]
fn render_agent_picker_lists_available_agents() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.current_agent_id = "copilot".into();
    app.available_agents = vec![
        AvailableAgent {
            id: "copilot".into(),
            display_name: "GitHub Copilot".into(),
            source: crate::agent_source::AgentSource::Host,
        },
        AvailableAgent {
            id: "claude".into(),
            display_name: "Claude Test Agent".into(),
            source: crate::agent_source::AgentSource::Host,
        },
    ];
    app.current_tab_mut().agent_picker_open = true;

    let text = render_to_text(&mut app, 80, 24);
    assert!(
        text.contains("Claude Test Agent"),
        "the agent picker must list available agents; rendered:\n{text}"
    );
}

#[test]
fn slash_agent_accepts_base_display_name_with_source_suffix() {
    let available_agents = vec![AvailableAgent {
        id: "copilot".into(),
        display_name: "GitHub Copilot — Windows".into(),
        source: crate::agent_source::AgentSource::Host,
    }];

    let selected = App::find_host_agent_for_command(
        &available_agents,
        crate::agent_registry::lookup_profile_by_id("copilot").display_name,
    )
    .expect("base built-in display name should select the suffixed host entry");

    assert_eq!(selected.id, "copilot");
}

/// Render: the setup diagnostic screen must paint its title and subtitle.
/// Lifts `ui/setup.rs` (reached only via `AppMode::Setup`).
#[test]
fn render_setup_screen_shows_title() {
    let mut app = test_app();
    app.mode = AppMode::Setup;
    app.setup = Some(SetupState {
        reason: SetupReason::AgentError,
        selected_index: 0,
        preflight: PreflightResult::passed_for_custom_agent("custom:qwen"),
        install_in_progress: false,
        install_log: Vec::new(),
        install_error: None,
        options: Vec::new(),
        title: "SETUP_TITLE_XYZ".into(),
        subtitle: "SETUP_SUBTITLE_XYZ".into(),
    });

    let text = render_to_text(&mut app, 80, 24);
    assert!(
        text.contains("SETUP_TITLE_XYZ"),
        "the setup screen must paint its title; rendered:\n{text}"
    );
    assert!(
        text.contains("SETUP_SUBTITLE_XYZ"),
        "the setup screen must paint its subtitle; rendered:\n{text}"
    );
}

/// Render: the auth/sign-in screen must paint the selected agent name.
/// Lifts `ui/auth.rs` (reached only via `AppMode::Auth`).
#[test]
fn render_auth_screen_shows_agent_name() {
    let mut app = test_app();
    app.mode = AppMode::Auth;
    app.auth = Some(AuthState {
        agent_id: "copilot".into(),
        agent_name: "SELECTED_AGENT_NAME_XYZ".into(),
        login_command: String::new(),
        checking: true,
        status_message: String::new(),
        enterprise_mode: false,
        enterprise_host: String::new(),
    });

    let text = render_to_text(&mut app, 80, 24);
    assert!(
        text.contains("SELECTED_AGENT_NAME_XYZ"),
        "the auth screen must paint the selected agent name; rendered:\n{text}"
    );
}

/// Render: the sessions (agents) view must paint its footer keybinding
/// hint. Lifts `ui/agents_view.rs` (reached via `View::Agents`).
#[test]
fn render_sessions_view_shows_footer_hint() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.current_tab_mut().current_view = View::Agents;

    let text = render_to_text(&mut app, 80, 24);
    let expected = t!("agents.footer_hint").into_owned();
    // Assert on a stable leading token of the localized hint so the test
    // doesn't break on translation wording while still proving the view
    // painted its chrome.
    let probe: String = expected.chars().take(6).collect();
    assert!(
        !probe.trim().is_empty() && text.contains(&probe),
        "the sessions view must paint its footer hint ({expected:?}); rendered:\n{text}"
    );
}

/// Render: the auth screen's sign-in card branch (`checking == false`)
/// must paint the connect prompt and, for Copilot, the GitHub Enterprise
/// sign-in footer. Covers the `else` arm of `ui/auth.rs`.
#[test]
fn render_auth_sign_in_card() {
    let mut app = test_app();
    app.mode = AppMode::Auth;
    app.auth = Some(AuthState {
        agent_id: "copilot".into(),
        agent_name: "GitHub Copilot".into(),
        login_command: String::new(),
        checking: false,
        status_message: String::new(),
        enterprise_mode: false,
        enterprise_host: String::new(),
    });

    let text = render_to_text(&mut app, 80, 24);
    let connect = t!("auth.card_connect", name = "GitHub Copilot").into_owned();
    let probe: String = connect.chars().take(6).collect();
    assert!(
        !probe.trim().is_empty() && text.contains(&probe),
        "the auth sign-in card must paint the connect prompt ({connect:?}); rendered:\n{text}"
    );
    let footer = t!("auth.enterprise_prompt").into_owned();
    let footer_probe: String = footer.trim_start().chars().take(13).collect();
    assert!(
        !footer_probe.trim().is_empty() && text.contains(&footer_probe),
        "the auth sign-in card must paint the Copilot enterprise footer ({footer:?}); rendered:\n{text}"
    );
}

/// `device_verify_url` derives the device-code verification URL from the
/// login command: github.com by default, but the GitHub Enterprise host
/// when the command carries `--host https://<host>` (bug B).
#[test]
fn device_verify_url_follows_enterprise_host() {
    assert_eq!(
        device_verify_url("copilot login"),
        "https://github.com/login/device"
    );
    assert_eq!(
        device_verify_url("copilot login --host https://mycorp.ghe.com"),
        "https://mycorp.ghe.com/login/device"
    );
    // Trailing slash is trimmed.
    assert_eq!(
        device_verify_url("copilot login --host https://mycorp.ghe.com/"),
        "https://mycorp.ghe.com/login/device"
    );
    // A quoted exe path doesn't confuse the --host parse.
    assert_eq!(
        device_verify_url("\"C:\\Program Files\\copilot.exe\" login --host https://x.ghe.com"),
        "https://x.ghe.com/login/device"
    );
}

/// A failed Copilot device-flow login (e.g. an unreachable GitHub
/// Enterprise host) must surface the captured reason on the auth screen
/// instead of silently returning to the form with no feedback (bug C).
#[test]
fn copilot_login_failure_surfaces_reason() {
    let mut app = test_app();
    app.mode = AppMode::Auth;
    app.auth = Some(AuthState {
        agent_id: "copilot".into(),
        agent_name: "GitHub Copilot".into(),
        login_command: "copilot login --host https://nope.invalid".into(),
        checking: true,
        status_message: String::new(),
        enterprise_mode: true,
        enterprise_host: "nope.invalid".into(),
    });

    app.handle_event(AppEvent::LoginComplete {
        agent_id: "copilot".into(),
        success: false,
        error: Some("Login failed: TypeError: fetch failed".into()),
    });

    let auth = app.auth.as_ref().expect("auth screen stays after failure");
    assert!(!auth.checking, "failure clears the checking spinner");
    assert_eq!(
        auth.status_message, "Login failed: TypeError: fetch failed",
        "the copilot login failure reason must be surfaced"
    );
}

/// When no specific reason is captured, a Copilot login failure still shows
/// a generic localized message rather than nothing.
#[test]
fn copilot_login_failure_without_reason_shows_generic_message() {
    let mut app = test_app();
    app.mode = AppMode::Auth;
    app.auth = Some(AuthState {
        agent_id: "copilot".into(),
        agent_name: "GitHub Copilot".into(),
        login_command: "copilot login".into(),
        checking: true,
        status_message: String::new(),
        enterprise_mode: false,
        enterprise_host: String::new(),
    });

    app.handle_event(AppEvent::LoginComplete {
        agent_id: "copilot".into(),
        success: false,
        error: None,
    });

    let auth = app.auth.as_ref().expect("auth screen stays after failure");
    assert_eq!(
        auth.status_message,
        t!("system.authentication_failed").into_owned(),
        "a reasonless copilot failure falls back to a generic message"
    );
}

/// Render: a Copilot login failure shows the reason at the *bottom* of the
/// screen (not appended to the header) followed by situation-specific
/// guidance. Regression guard for the "error on the first line" report.
#[test]
fn render_auth_copilot_failure_shows_reason_and_guidance_at_bottom() {
    let mut app = test_app();
    app.mode = AppMode::Auth;
    app.auth = Some(AuthState {
        agent_id: "copilot".into(),
        agent_name: "GitHub Copilot".into(),
        login_command: "copilot login --host https://nope.invalid".into(),
        checking: false,
        status_message: "Login failed: boom".into(),
        enterprise_mode: true,
        enterprise_host: "nope.invalid".into(),
    });

    let text = render_to_text(&mut app, 100, 24);
    assert!(
        text.contains("Login failed: boom"),
        "the failure reason must render; rendered:\n{text}"
    );
    // Situation-specific guidance is shown (stable leading probe).
    let help = t!("auth.login_failed_help_enterprise").into_owned();
    let help_probe: String = help.trim_start().chars().take(16).collect();
    assert!(
        text.contains(&help_probe),
        "enterprise failure guidance must render ({help:?}); rendered:\n{text}"
    );
    // The reason must NOT be on the header (card_connect) line — it now
    // belongs at the bottom.
    let header = text
        .lines()
        .find(|l| l.contains("Connect GitHub Copilot"))
        .expect("header line present");
    assert!(
        !header.contains("Login failed"),
        "the failure reason must not be in the header; rendered:\n{text}"
    );
}

/// Review fix ①: a stale `LoginComplete` after the user escaped the auth
/// screen (auth = None) must be ignored — it must not force Chat mode or
/// start ACP for an empty agent.
#[test]
fn login_complete_ignored_when_no_active_auth_attempt() {
    let mut app = test_app();
    app.mode = AppMode::Setup;
    app.auth = None;

    app.handle_event(AppEvent::LoginComplete {
        agent_id: "copilot".into(),
        success: true,
        error: None,
    });

    assert_eq!(
        app.mode,
        AppMode::Setup,
        "a stale success must not force Chat mode after the user left auth"
    );
    assert!(
        !app.pending_acp_start,
        "a stale success must not start an ACP client"
    );
}

/// Review fix ①: a `LoginComplete` whose agent doesn't match the active
/// auth attempt (user switched agents) must be ignored.
#[test]
fn login_complete_ignored_on_agent_mismatch() {
    let mut app = test_app();
    app.mode = AppMode::Auth;
    app.auth = Some(AuthState {
        agent_id: "claude".into(),
        agent_name: "Claude".into(),
        login_command: "claude /login".into(),
        checking: true,
        status_message: String::new(),
        enterprise_mode: false,
        enterprise_host: String::new(),
    });

    app.handle_event(AppEvent::LoginComplete {
        agent_id: "copilot".into(),
        success: true,
        error: None,
    });

    assert_eq!(
        app.mode,
        AppMode::Auth,
        "a completion for a different agent must not transition to Chat"
    );
    assert!(
        app.auth.is_some(),
        "a mismatched completion must not tear down the active auth screen"
    );
}

/// Regression: a Copilot retry must clear any prior failure status so the
/// checking view shows "Checking…" — not a stale "Login failed…" plus a
/// phantom "code copied" from the previous attempt. `begin_auth_checking`
/// is the shared entry point both login paths use.
#[test]
fn begin_auth_checking_clears_stale_status() {
    let mut app = test_app();
    app.mode = AppMode::Auth;
    app.auth = Some(AuthState {
        agent_id: "copilot".into(),
        agent_name: "GitHub Copilot".into(),
        login_command: "copilot login --host https://nope.invalid".into(),
        checking: false,
        status_message: "Login failed: TypeError: fetch failed".into(),
        enterprise_mode: true,
        enterprise_host: "nope.invalid".into(),
    });

    app.begin_auth_checking();

    let auth = app.auth.as_ref().expect("auth screen present");
    assert!(auth.checking, "begin_auth_checking must enter the checking state");
    assert!(
        auth.status_message.is_empty(),
        "a retry must clear the stale failure status so the checking view \
         does not render a phantom 'code copied'"
    );
}

/// Regression: after a GHE failure, the first Esc collapses the enterprise
/// input AND clears the failure status, so it does not linger on the
/// collapsed github.com sign-in screen ("failed/copied message carried back").
#[test]
fn esc_collapse_clears_enterprise_failure_status() {
    let mut app = test_app();
    app.mode = AppMode::Auth;
    app.auth = Some(AuthState {
        agent_id: "copilot".into(),
        agent_name: "GitHub Copilot".into(),
        login_command: "copilot login --host https://nope.invalid".into(),
        checking: false,
        status_message: "Login failed: TypeError: fetch failed".into(),
        enterprise_mode: true,
        enterprise_host: "nope.invalid".into(),
    });

    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert_eq!(app.mode, AppMode::Auth, "collapse stays on the sign-in screen");
    let auth = app.auth.as_ref().expect("collapse keeps the auth screen");
    assert!(!auth.enterprise_mode, "first Esc collapses the enterprise input");
    assert!(
        auth.status_message.is_empty(),
        "collapsing must clear the enterprise failure status so it does not linger"
    );
}

/// Render: the auth screen while checking with a non-empty status message
/// must paint that message (the `waiting_for_authorization` branch). Covers
/// `ui/auth.rs` lines 44-60.
#[test]
fn render_auth_checking_with_status_message() {
    let mut app = test_app();
    app.mode = AppMode::Auth;
    app.auth = Some(AuthState {
        agent_id: "copilot".into(),
        agent_name: "GitHub Copilot".into(),
        login_command: String::new(),
        checking: true,
        status_message: "AUTH_STATUS_XYZ".into(),
        enterprise_mode: false,
        enterprise_host: String::new(),
    });

    let text = render_to_text(&mut app, 80, 24);
    assert!(
        text.contains("AUTH_STATUS_XYZ"),
        "the auth screen must paint the status message while waiting; rendered:\n{text}"
    );
}

/// The GHE sign-in affordance: [E] reveals the domain input, typed chars
/// edit it (Ctrl-modified keys and whitespace are ignored), Backspace
/// deletes, and Esc collapses back to the github.com choice WITHOUT leaving
/// the sign-in screen.
#[test]
fn auth_enterprise_domain_entry_via_keys() {
    let mut app = test_app();
    app.mode = AppMode::Auth;
    app.auth = Some(AuthState {
        agent_id: "copilot".into(),
        agent_name: "GitHub Copilot".into(),
        login_command: "copilot login".into(),
        checking: false,
        status_message: String::new(),
        enterprise_mode: false,
        enterprise_host: String::new(),
    });

    // [E] opens the enterprise domain input (it is not typed into the field).
    app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
    assert!(
        app.auth.as_ref().unwrap().enterprise_mode,
        "E must reveal the domain input"
    );

    // Typed characters edit the domain.
    for c in ['c', 'o', 'r', 'p', '.', 'g', 'h', 'e', '.', 'c', 'o', 'm'] {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    // Ctrl-combinations and whitespace must NOT be typed into the field.
    app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
    assert_eq!(app.auth.as_ref().unwrap().enterprise_host, "corp.ghe.com");

    // Backspace deletes one character.
    app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(app.auth.as_ref().unwrap().enterprise_host, "corp.ghe.co");

    // Esc collapses the input but stays on the sign-in screen.
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    let auth = app
        .auth
        .as_ref()
        .expect("Esc collapse must not leave the sign-in screen");
    assert!(!auth.enterprise_mode, "Esc must collapse the enterprise input");
    assert_eq!(app.mode, AppMode::Auth, "Esc collapse must stay in Auth mode");
}

fn agent_status_for_test(id: &str, display: &str, cli_found: bool) -> crate::agent_check::AgentStatus {
    crate::agent_check::AgentStatus {
        id: id.into(),
        display_name: display.into(),
        cli_found,
        cli_path: None,
        install_hint: String::new(),
        auth_hint: String::new(),
        auto_installable: id == "copilot",
    }
}

#[test]
fn diagnostic_setup_options_route_auth_by_agent() {
    let copilot = agent_status_for_test("copilot", "GitHub Copilot", true);
    let copilot_options = build_setup_options(&SetupReason::AgentError, Some(&copilot));
    assert!(
        matches!(
            copilot_options.as_slice(),
            [SetupOption::SignIn { agent_id, .. }, SetupOption::ChooseAgentSource]
                if agent_id == "copilot"
        ),
        "Copilot auth failures must offer the in-app SignIn flow"
    );

    let codex = agent_status_for_test("codex", "Codex", true);
    let codex_options = build_setup_options(&SetupReason::AgentError, Some(&codex));
    assert!(
        matches!(
            codex_options.as_slice(),
            [SetupOption::Retry, SetupOption::ChooseAgentSource]
        ),
        "external-auth agents stay on the diagnostic Retry flow"
    );
}

#[test]
fn show_copilot_auth_screen_sets_expected_state() {
    let mut app = test_app();
    app.mode = AppMode::Setup;
    app.setup = Some(SetupState {
        reason: SetupReason::AgentError,
        selected_index: 0,
        preflight: PreflightResult::passed_for_custom_agent("copilot"),
        install_in_progress: false,
        install_log: Vec::new(),
        install_error: None,
        options: vec![SetupOption::Retry],
        title: "setup".into(),
        subtitle: "sub".into(),
    });

    app.show_copilot_auth_screen();

    assert_eq!(app.mode, AppMode::Auth);
    assert!(app.setup.is_none(), "auth screen should replace setup state");
    assert_eq!(app.current_agent_id, "copilot");
    let auth = app.auth.as_ref().expect("copilot auth state");
    assert_eq!(auth.agent_id, "copilot");
    assert_eq!(auth.agent_name, "GitHub Copilot");
    assert!(auth.login_command.contains("copilot"));
    assert!(!auth.checking);
    assert!(auth.status_message.is_empty());
}

/// Render: a setup screen with a full options list while a winget install
/// is in progress must paint each option label and the install spinner row.
/// Covers the `SetupOption` match arms + the install-progress block in
/// `ui/setup.rs`.
#[test]
fn render_setup_options_while_installing() {
    let mut app = test_app();
    app.mode = AppMode::Setup;
    app.setup = Some(SetupState {
        reason: SetupReason::AgentMissing,
        selected_index: 0,
        preflight: PreflightResult::passed_for_custom_agent("custom:x"),
        install_in_progress: true,
        install_log: vec!["WINGET_LOG_XYZ".into()],
        install_error: None,
        options: vec![
            SetupOption::Install {
                agent_id: "copilot".into(),
                display_name: "GitHub Copilot".into(),
            },
            SetupOption::SignIn {
                agent_id: "copilot".into(),
                display_name: "GitHub Copilot".into(),
            },
            SetupOption::Retry,
        ],
        title: "INSTALLING_TITLE_XYZ".into(),
        subtitle: "sub".into(),
    });

    let text = render_to_text(&mut app, 80, 30);
    assert!(
        text.contains("INSTALLING_TITLE_XYZ"),
        "the setup screen must paint its title; rendered:\n{text}"
    );
    assert!(
        text.contains("WINGET_LOG_XYZ"),
        "the install-in-progress block must paint the winget log tail; rendered:\n{text}"
    );
}

/// Render: a setup screen carrying an install error must paint the error
/// message. Covers the `install_error` branch in `ui/setup.rs` (line 186+).
#[test]
fn render_setup_install_error() {
    let mut app = test_app();
    app.mode = AppMode::Setup;
    app.setup = Some(SetupState {
        reason: SetupReason::AgentError,
        selected_index: 0,
        preflight: PreflightResult::passed_for_custom_agent("custom:x"),
        install_in_progress: false,
        install_log: vec!["log-a".into(), "log-b".into()],
        install_error: Some("INSTALL_ERR_XYZ".into()),
        options: vec![SetupOption::Retry],
        title: "err".into(),
        subtitle: "sub".into(),
    });

    let text = render_to_text(&mut app, 80, 30);
    assert!(
        text.contains("INSTALL_ERR_XYZ"),
        "the setup screen must paint the install error; rendered:\n{text}"
    );
}

/// Render: a setup screen with a completed-info log (no install running,
/// no error) must paint the info line. Covers the info-log block in
/// `ui/setup.rs` (lines 75-85).
#[test]
fn render_setup_info_log() {
    let mut app = test_app();
    app.mode = AppMode::Setup;
    app.setup = Some(SetupState {
        reason: SetupReason::AgentError,
        selected_index: 0,
        preflight: PreflightResult::passed_for_custom_agent("custom:x"),
        install_in_progress: false,
        install_log: vec!["INFO_LOG_XYZ".into()],
        install_error: None,
        options: vec![SetupOption::Retry],
        title: "info".into(),
        subtitle: "sub".into(),
    });

    let text = render_to_text(&mut app, 80, 30);
    assert!(
        text.contains("INFO_LOG_XYZ"),
        "the setup screen must paint the completed-info log line; rendered:\n{text}"
    );
}

/// Alt+V when the agent did not advertise the `image` prompt capability
/// must no-op the paste and surface a clear system message rather than
/// queueing an image the agent would reject.
#[test]
fn alt_v_without_image_capability_shows_not_supported_message() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.agent_supports_image = false;

    app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::ALT));

    let want = t!("system.image_not_supported").into_owned();
    let tab = app.current_tab();
    assert!(
        tab.messages
            .iter()
            .any(|m| matches!(m, ChatMessage::System(s) if *s == want)),
        "Alt+V without image capability must push the not-supported message"
    );
    assert!(
        tab.pending_images.is_empty(),
        "no image should be queued when the capability is missing"
    );
}

/// Render: queued Alt+V images surface as the input-box title so the user
/// can see what will be sent.
#[test]
fn input_box_titles_queued_images() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.current_tab_mut()
        .pending_images
        .push(crate::clipboard_image::PastedImage {
            data_base64: "AAA=".into(),
            mime_type: "image/png".into(),
            label: "screenshot".into(),
        });

    let text = render_to_text(&mut app, 80, 30);
    assert!(
        text.contains("screenshot"),
        "the input box must title queued images; rendered:\n{text}"
    );
}


/// the action's command body (the card shows the command, not the choice
/// `title` field, which only surfaces for action-less choices) plus the
/// run-command button. Lifts `ui/recommendations.rs` (reached only when
/// `turn.recommendations()` is Some).
#[test]
fn render_recommendation_card_shows_command() {
    use crate::coordinator::{RecommendationChoice, RecommendationSet, RecommendedAction};
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.current_tab_mut().turn = TurnState::Surfaced {
        prompt: SubmittedPrompt {
            id: 1,
            text: "fix it".into(),
            submitted_at_unix_s: 0.0,
            autofix: None,
        },
        outcome: TurnOutcome::Recommendation(RecommendationSet {
            recommended_choice: Some(0),
            choices: vec![RecommendationChoice {
                choice: 0,
                title: "Run the fix".into(),
                rationale: "because reasons".into(),
                actions: vec![RecommendedAction::Send {
                    parent: String::new(),
                    input: "echo REC_CMD_XYZ".into(),
                }],
            }],
        }),
        end_pending: false,
    };

    let text = render_to_text(&mut app, 80, 40);
    assert!(
        text.contains("REC_CMD_XYZ"),
        "the recommendation card must paint its command body; rendered:\n{text}"
    );
    let run_btn = t!("recommendations.button_run_command").into_owned();
    let probe: String = run_btn.chars().take(4).collect();
    assert!(
        !probe.trim().is_empty() && text.contains(&probe),
        "the recommendation card must paint the run-command button ({run_btn:?}); rendered:\n{text}"
    );
}

/// Render: every `ChatMessage` variant must paint without panicking and
/// surface its distinguishing text. Lifts the `build_message_lines` /
/// `message_height` match arms in `ui/chat.rs` (User/System/Plan/Error/
/// AgentEvent/Disclaimer were previously unexercised).
#[test]
fn render_chat_all_message_variants() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    {
        let tab = app.current_tab_mut();
        tab.messages.push(ChatMessage::User("USER_MSG_XYZ".into()));
        tab.messages.push(ChatMessage::Agent("AGENT_MSG_XYZ".into()));
        tab.messages.push(ChatMessage::System("SYSTEM_MSG_XYZ".into()));
        tab.messages.push(ChatMessage::Error("ERROR_MSG_XYZ".into()));
        tab.messages
            .push(ChatMessage::AgentEvent("AGENT_EVENT_MSG_XYZ".into()));
        tab.messages.push(ChatMessage::Plan(vec![
            PlanEntry {
                content: "PLAN_DONE_XYZ".into(),
                status: PlanEntryStatus::Completed,
            },
            PlanEntry {
                content: "PLAN_DOING_XYZ".into(),
                status: PlanEntryStatus::InProgress,
            },
            PlanEntry {
                content: "PLAN_TODO_XYZ".into(),
                status: PlanEntryStatus::Pending,
            },
        ]));
        tab.messages.push(ChatMessage::Disclaimer);
    }

    let text = render_to_text(&mut app, 80, 40);
    for needle in [
        "USER_MSG_XYZ",
        "AGENT_MSG_XYZ",
        "SYSTEM_MSG_XYZ",
        "ERROR_MSG_XYZ",
        "AGENT_EVENT_MSG_XYZ",
        "PLAN_DONE_XYZ",
        "PLAN_DOING_XYZ",
        "PLAN_TODO_XYZ",
    ] {
        assert!(
            text.contains(needle),
            "chat must paint {needle:?}; rendered:\n{text}"
        );
    }
}

/// Render: an expanded completed turn with a trailing marker must paint
/// its prompt header, its detail rows, and the marker. Lifts
/// `build_completed_turn_lines` in `ui/chat.rs`.
#[test]
fn render_chat_completed_turn_expanded_with_marker() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.current_tab_mut().completed_turns.push(CompletedTurn {
        prompt: "TURN_PROMPT_XYZ".into(),
        details: vec![ChatMessage::Agent("TURN_DETAIL_XYZ".into())],
        expanded: true,
        trailing_marker: Some("TURN_MARKER_XYZ".into()),
    });

    let text = render_to_text(&mut app, 80, 40);
    for needle in ["TURN_PROMPT_XYZ", "TURN_DETAIL_XYZ", "TURN_MARKER_XYZ"] {
        assert!(
            text.contains(needle),
            "expanded completed turn must paint {needle:?}; rendered:\n{text}"
        );
    }
}

/// Render: while the helper is still connecting, the chat must paint the
/// animated "Connecting…" activity line. Lifts the `Connecting` branch of
/// `build_activity_line` in `ui/chat.rs`.
#[test]
fn render_chat_connecting_activity_line() {
    let mut app = test_app();
    app.state = ConnectionState::Connecting("starting".into());

    let text = render_to_text(&mut app, 80, 24);
    let label = t!("connection.connecting_activity").into_owned();
    let probe: String = label.chars().take(6).collect();
    assert!(
        !probe.trim().is_empty() && text.contains(&probe),
        "chat must paint the connecting activity line ({label:?}); rendered:\n{text}"
    );
}

/// Render: the first-run welcome hint must paint its title when connected
/// and `show_welcome_hint` is set. Lifts the welcome branch of
/// `ui/chat.rs` + `ui/layout.rs`.
#[test]
fn render_chat_welcome_hint() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.show_welcome_hint = true;

    let text = render_to_text(&mut app, 80, 24);
    let title = t!("chat.welcome_title").into_owned();
    let probe: String = title.chars().take(6).collect();
    assert!(
        !probe.trim().is_empty() && text.contains(&probe),
        "chat must paint the welcome title ({title:?}); rendered:\n{text}"
    );
}

/// Render: when the pane is too short for a full permission card, the
/// compact one-row fallback must paint the description and the `[Y/N]`
/// hint. Lifts `render_compact` in `ui/permission.rs`. The compact path
/// is gated on `terminal_rows - 3 < CARD_MIN_SIZE`.
#[test]
fn render_permission_compact_shows_hint() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.terminal_rows = 7; // ceiling = 4 < CARD_MIN_SIZE(5) → compact fallback
    app.current_tab_mut().permission.push_back(PermissionState {
        description: "Run: echo PERM_COMPACT_XYZ".into(),
        options: vec![
            PermOption {
                id: "allow-once".into(),
                name: "Allow once".into(),
                kind: "AllowOnce".into(),
            },
            PermOption {
                id: "reject-once".into(),
                name: "Reject".into(),
                kind: "RejectOnce".into(),
            },
        ],
        selected: 0,
        responder: None,
    });

    let text = render_to_text(&mut app, 80, 24);
    assert!(
        text.contains("PERM_COMPACT_XYZ"),
        "the compact permission row must paint its description; rendered:\n{text}"
    );
    assert!(
        text.contains("Y/N"),
        "the compact permission row must paint the [Y/N] hint; rendered:\n{text}"
    );
}

fn submit_autofix_prompt(app: &mut App, pane: &str) {
    let gen = {
        let tab = app.tab_mut(DEFAULT_TAB_ID);
        tab.autofix.generation = tab.autofix.generation.wrapping_add(1);
        tab.autofix.pane_id = Some(pane.into());
        tab.autofix.generation
    };
    let prompt = SubmittedPrompt {
        id: 99,
        text: "diagnose this".into(),
        submitted_at_unix_s: 0.0,
        autofix: Some(AutofixContext {
            target_pane_id: pane.into(),
            generation: gen,
        }),
    };
    app.turn_submit_prompt(DEFAULT_TAB_ID, prompt);
}

/// Submit a manual-`/fix`-style autofix turn: an autofix context whose
/// `target_pane_id` is empty (the App doesn't know the working pane until
/// the client task resolves it and plumbs it back).
fn submit_fix_prompt(app: &mut App, id: u64) {
    let gen = {
        let tab = app.tab_mut(DEFAULT_TAB_ID);
        tab.autofix.generation = tab.autofix.generation.wrapping_add(1);
        tab.autofix.generation
    };
    let prompt = SubmittedPrompt {
        id,
        text: String::new(),
        submitted_at_unix_s: 0.0,
        autofix: Some(AutofixContext {
            target_pane_id: String::new(),
            generation: gen,
        }),
    };
    app.turn_submit_prompt(DEFAULT_TAB_ID, prompt);
}

fn fix_target_pane(app: &App) -> String {
    app.current_tab()
        .turn
        .prompt()
        .unwrap()
        .autofix
        .as_ref()
        .unwrap()
        .target_pane_id
        .clone()
}

#[test]
fn fix_target_pane_is_late_bound_by_prompt_id() {
    let mut app = test_app();
    submit_fix_prompt(&mut app, 42);
    assert_eq!(fix_target_pane(&app), "", "starts unbound");

    // A resolution for a different prompt id (a superseded /fix) is ignored.
    app.apply_autofix_target_resolved(Some(DEFAULT_TAB_ID.into()), 7, "pane-X".into());
    assert_eq!(fix_target_pane(&app), "", "stale prompt_id must not patch");

    // An empty pane id is a no-op.
    app.apply_autofix_target_resolved(Some(DEFAULT_TAB_ID.into()), 42, String::new());
    assert_eq!(fix_target_pane(&app), "", "empty pane id is ignored");

    // The matching prompt id binds the resolved working pane.
    app.apply_autofix_target_resolved(Some(DEFAULT_TAB_ID.into()), 42, "pane-7".into());
    assert_eq!(fix_target_pane(&app), "pane-7", "matching id binds the pane");
}

#[test]
fn submit_clears_messages_and_pushes_user_bubble() {
    let mut app = test_app();
    app.current_tab_mut()
        .messages
        .push(ChatMessage::System("stale".into()));
    submit_test_prompt(&mut app, "hello");
    let tab = app.current_tab();
    assert!(matches!(tab.turn, TurnState::Submitted(_)));
    assert!(
        !tab.turn.accepts_new_prompt(),
        "Submitted blocks new prompts"
    );
    assert_eq!(tab.messages.len(), 1, "stale System bubble was cleared");
    assert!(matches!(tab.messages[0], ChatMessage::User(ref t) if t == "hello"));
}

#[test]
fn first_message_chunk_transitions_to_streaming_with_buf() {
    let mut app = test_app();
    submit_test_prompt(&mut app, "hi");
    let advanced = app.turn_observe_chunk(DEFAULT_TAB_ID, ChunkKind::Message, "partial");
    assert!(advanced, "first message chunk must advance the buffer");
    let tab = app.current_tab();
    assert_eq!(tab.turn.buffer(), Some("partial"));
    assert!(tab.turn.is_streaming());
}

#[test]
fn thought_chunk_first_transitions_with_empty_buf() {
    let mut app = test_app();
    submit_test_prompt(&mut app, "hi");
    let advanced = app.turn_observe_chunk(DEFAULT_TAB_ID, ChunkKind::Thought, "thinking…");
    assert!(!advanced, "thought chunks never advance the buffer");
    let tab = app.current_tab();
    assert!(tab.turn.is_streaming());
    assert_eq!(tab.turn.buffer(), Some(""));
}

#[test]
fn end_with_no_eager_chat_fallback_commits_completed_turn() {
    let mut app = test_app();
    submit_test_prompt(&mut app, "why blue?");
    // Pure prose — won't parse as a RecommendationSet, falls to chat.
    app.turn_observe_chunk(
        DEFAULT_TAB_ID,
        ChunkKind::Message,
        "Light scatters in the atmosphere.",
    );
    app.turn_close(DEFAULT_TAB_ID);
    let tab = app.current_tab();
    assert!(
        matches!(
            tab.turn,
            TurnState::Surfaced {
                outcome: TurnOutcome::ChatTurn,
                end_pending: false,
                ..
            }
        ),
        "got {:?}",
        tab.turn
    );
    assert!(
        tab.turn.accepts_new_prompt(),
        "chat fallback unblocks input"
    );
    assert_eq!(tab.completed_turns.len(), 1);
    assert_eq!(tab.completed_turns[0].prompt, "why blue?");
}

#[test]
fn end_with_no_chunks_clears_autofix_bottom_bar() {
    let mut app = test_app();
    submit_autofix_prompt(&mut app, "pane-7");
    assert!(app.tab_mut(DEFAULT_TAB_ID).autofix.pane_id.is_some());
    // No chunks arrived; AgentMessageEnd fires.
    app.turn_close(DEFAULT_TAB_ID);
    let tab = app.current_tab();
    assert!(
        matches!(
            tab.turn,
            TurnState::Surfaced {
                outcome: TurnOutcome::Empty,
                end_pending: false,
                ..
            }
        ),
        "got {:?}",
        tab.turn
    );
    assert!(
        app.tab_mut(DEFAULT_TAB_ID).autofix.pane_id.is_none(),
        "autofix.pane_id must be cleared so the bar leaves Pending"
    );
}

#[test]
fn stale_autofix_chunks_dropped_when_generation_diverges() {
    let mut app = test_app();
    submit_autofix_prompt(&mut app, "pane-1");
    // Simulate an Esc cancel or a newer trigger bumping the counter
    // on the same tab as the in-flight prompt.
    {
        let tab = app.tab_mut(DEFAULT_TAB_ID);
        tab.autofix.generation = tab.autofix.generation.wrapping_add(1);
    }
    let advanced = app.turn_observe_chunk(DEFAULT_TAB_ID, ChunkKind::Message, "stale");
    assert!(!advanced, "stale-gen chunks must be dropped");
    let tab = app.current_tab();
    assert!(
        matches!(tab.turn, TurnState::Submitted(_)),
        "state unchanged on stale drop, got {:?}",
        tab.turn
    );
    assert_eq!(tab.turn.buffer(), None);
}

#[test]
fn stale_autofix_at_close_resets_to_idle() {
    let mut app = test_app();
    submit_autofix_prompt(&mut app, "pane-1");
    // A chunk advances state to Streaming.
    app.turn_observe_chunk(DEFAULT_TAB_ID, ChunkKind::Message, "partial");
    // Generation diverges (newer trigger / Esc).
    {
        let tab = app.tab_mut(DEFAULT_TAB_ID);
        tab.autofix.generation = tab.autofix.generation.wrapping_add(1);
    }
    app.turn_close(DEFAULT_TAB_ID);
    assert!(
        app.current_tab().turn.is_idle(),
        "stale-close must reset to Idle, got {:?}",
        app.current_tab().turn
    );
}

#[test]
fn cancel_bumps_generation_and_returns_to_idle() {
    let mut app = test_app();
    submit_autofix_prompt(&mut app, "pane-1");
    let gen_before = app.tab_mut(DEFAULT_TAB_ID).autofix.generation;
    app.turn_cancel(DEFAULT_TAB_ID);
    assert_eq!(
        app.tab_mut(DEFAULT_TAB_ID).autofix.generation,
        gen_before.wrapping_add(1)
    );
    assert!(app.current_tab().turn.is_idle());
    assert!(app.tab_mut(DEFAULT_TAB_ID).autofix.pane_id.is_none());
}

#[test]
fn cancel_mid_stream_preserves_visible_prose_with_canceled_marker() {
    // Esc while prose is streaming → commit partial prose as a
    // CompletedTurn (default-expanded) with the trailing_marker set
    // so the user sees what arrived and that they cancelled it.
    let mut app = test_app();
    submit_test_prompt(&mut app, "tell me a story");
    app.turn_observe_chunk(DEFAULT_TAB_ID, ChunkKind::Message, "\n\nOnce upon a time");
    app.turn_cancel(DEFAULT_TAB_ID);
    let tab = app.current_tab();
    assert!(tab.turn.is_idle(), "got {:?}", tab.turn);
    assert_eq!(tab.completed_turns.len(), 1);
    let committed = &tab.completed_turns[0];
    assert_eq!(committed.prompt, "tell me a story");
    assert!(
        committed.expanded,
        "cancel-committed turns default expanded"
    );
    assert!(committed
        .details
        .iter()
        .any(|m| matches!(m, ChatMessage::Agent(t) if t.contains("Once upon a time"))));
    assert!(
        committed
            .trailing_marker
            .as_deref()
            .map_or(false, |m| m.contains("canceled")),
        "trailing_marker should hold (canceled), got {:?}",
        committed.trailing_marker
    );
    assert!(tab.messages.is_empty(), "messages cleared on cancel");
    assert!(tab.tool_calls.is_empty(), "tool_calls cleared on cancel");
}

#[test]
fn cancel_mid_stream_records_canceled_marker_even_without_visible_prose() {
    // A buffer that's pure JSON (no `explanation` field, no prose
    // prefix) renders as nothing during streaming. We must NOT commit
    // raw JSON as agent prose, but we still record a completed_turn
    // with the canceled marker so the user knows the prompt was sent
    // and cancelled.
    let mut app = test_app();
    submit_test_prompt(&mut app, "kill pid 1234");
    app.turn_observe_chunk(
        DEFAULT_TAB_ID,
        ChunkKind::Message,
        r#"{"recommended_choice":1,"choices":[{"choice":1,"#,
    );
    app.turn_cancel(DEFAULT_TAB_ID);
    let tab = app.current_tab();
    assert!(tab.turn.is_idle());
    assert_eq!(tab.completed_turns.len(), 1);
    let committed = &tab.completed_turns[0];
    assert_eq!(committed.prompt, "kill pid 1234");
    assert!(
        !committed
            .details
            .iter()
            .any(|m| matches!(m, ChatMessage::Agent(_))),
        "JSON-only buffer must not be committed as agent prose"
    );
    assert!(
        committed
            .trailing_marker
            .as_deref()
            .map_or(false, |m| m.contains("canceled")),
        "trailing_marker should hold (canceled), got {:?}",
        committed.trailing_marker
    );
    assert!(tab.messages.is_empty());
    assert!(tab.tool_calls.is_empty());
}

#[test]
fn end_pending_blocks_new_prompts_until_message_end() {
    // Eager-surface path: user submits → JSON streams → recommendation
    // surfaces before AgentMessageEnd. While end_pending=true the UI
    // gate must hold. AgentMessageEnd then releases it.
    let mut app = test_app();
    submit_test_prompt(&mut app, "first");
    // RecommendationSet shape that survives `validate_recommendation_set`.
    let json = r#"```json
{"recommended_choice":1,"choices":[{"choice":1,"title":"do it","rationale":"r","actions":[{"type":"send","parent":"pane-X","input":"ls"}]}]}
```"#;
    app.turn_observe_chunk(DEFAULT_TAB_ID, ChunkKind::Message, json);
    app.turn_try_eager_surface(DEFAULT_TAB_ID);
    let tab = app.current_tab();
    assert!(
        matches!(
            tab.turn,
            TurnState::Surfaced {
                outcome: TurnOutcome::Recommendation(_),
                end_pending: true,
                ..
            }
        ),
        "expected eager surface, got {:?}",
        tab.turn
    );
    assert!(
        !tab.turn.accepts_new_prompt(),
        "end_pending=true must hold the UI gate"
    );
    // AgentMessageEnd flips end_pending=false.
    app.turn_close(DEFAULT_TAB_ID);
    assert!(app.current_tab().turn.accepts_new_prompt());
}

// ─── card / panel height math ───────────────────────────────────────────

use crate::app::turn_state::{SubmittedPrompt, TurnOutcome, TurnState};
use crate::coordinator::{
    OpenTarget, RecommendationChoice, RecommendationSet, RecommendedAction,
};
use crate::ui::card::{card_content_width, CARD_H_CHROME, CARD_MIN_SIZE};

fn perm_with(desc: &str) -> PermissionState {
    PermissionState {
        description: desc.to_string(),
        options: vec![PermOption {
            id: "allow_once".into(),
            name: "Allow".into(),
            kind: "allow_once".into(),
        }],
        selected: 0,
        responder: None,
    }
}

fn rec_send(input: &str) -> RecommendationChoice {
    RecommendationChoice {
        choice: 0,
        title: "t".into(),
        rationale: String::new(),
        actions: vec![RecommendedAction::Send {
            parent: String::new(),
            input: input.into(),
        }],
    }
}

fn install_recs(app: &mut App, choices: Vec<RecommendationChoice>) {
    let tab = app.current_tab_mut();
    tab.turn = TurnState::Surfaced {
        prompt: SubmittedPrompt {
            id: 1,
            text: "p".into(),
            submitted_at_unix_s: 0.0,
            autofix: None,
        },
        outcome: TurnOutcome::Recommendation(RecommendationSet {
            recommended_choice: Some(0),
            choices,
        }),
        end_pending: false,
    };
}

#[test]
fn card_content_width_subtracts_chrome_and_floors_at_1() {
    assert_eq!(card_content_width(80), 80 - CARD_H_CHROME as usize);
    assert_eq!(card_content_width(CARD_H_CHROME + 1), 1);
    assert_eq!(card_content_width(CARD_H_CHROME), 1);
    assert_eq!(card_content_width(0), 1);
}

#[test]
fn permission_card_height_single_line_is_card_min() {
    let perm = perm_with("ok");
    assert_eq!(permission_card_height(&perm, 80) as u16, CARD_MIN_SIZE);
}

#[test]
fn permission_card_height_counts_wrap_at_actual_panel_width() {
    let perm = perm_with(&"a".repeat(200));
    // Full-width terminal: wrap at 80 - 8 = 72.
    let inner_full = 80 - CARD_H_CHROME as usize;
    assert_eq!(
        permission_card_height(&perm, 80),
        CARD_MIN_SIZE as usize + 200_usize.div_ceil(inner_full) - 1
    );
    // Debug panel open: 60% of 80 = 48 → wrap at 40.
    let inner_split = 48 - CARD_H_CHROME as usize;
    assert_eq!(
        permission_card_height(&perm, 48),
        CARD_MIN_SIZE as usize + 200_usize.div_ceil(inner_split) - 1
    );
    // The two should differ — proves the panel_width input matters
    // (the PR #20 reviewer-3 bug).
    assert_ne!(
        permission_card_height(&perm, 80),
        permission_card_height(&perm, 48)
    );
}

#[test]
fn permission_card_height_treats_blank_lines_as_one_row() {
    let perm = perm_with("line1\n\nline2");
    // 3 logical lines (blank counts as 1).
    assert_eq!(
        permission_card_height(&perm, 80),
        CARD_MIN_SIZE as usize + 2
    );
}

#[test]
fn rec_card_height_includes_inter_card_gap() {
    let h = rec_card_height(&rec_send("ls"), 80);
    assert_eq!(h as u16, CARD_MIN_SIZE + 1);
}

#[test]
fn rec_card_height_handles_open_action_synthesis() {
    let choice = RecommendationChoice {
        choice: 0,
        title: "t".into(),
        rationale: String::new(),
        actions: vec![RecommendedAction::Open {
            target: OpenTarget::Tab,
            parent: None,
            cwd: Some("C:/repo".into()),
            title: Some("logs".into()),
            direction: None,
            profile: None,
        }],
    };
    let h = rec_card_height(&choice, 80);
    // "New tab (logs) in C:/repo" fits on one row at width 72.
    assert_eq!(h as u16, CARD_MIN_SIZE + 1);
}

#[test]
fn permission_panel_height_zero_when_no_permission() {
    let mut app = test_app();
    app.terminal_rows = 30;
    assert_eq!(app.permission_panel_height(80), 0);
}

#[test]
fn permission_panel_height_falls_back_to_compact_below_card_min() {
    let mut app = test_app();
    app.terminal_rows = 7; // ceiling = 7-3 = 4 < CARD_MIN_SIZE
    app.current_tab_mut().permission.push_back(perm_with("ok"));
    // Must stay visible — agent flow blocks on this prompt. 1-row strip
    // is the compact fallback rendered by `ui::permission::render`.
    assert_eq!(app.permission_panel_height(80), 1);
}

#[test]
fn permission_panel_height_admits_at_card_min_ceiling() {
    let mut app = test_app();
    app.terminal_rows = 8; // ceiling = 5 == CARD_MIN_SIZE
    app.current_tab_mut().permission.push_back(perm_with("ok"));
    assert_eq!(app.permission_panel_height(80), CARD_MIN_SIZE);
}

#[test]
fn rec_panel_height_floor_lets_tallest_card_render() {
    let mut app = test_app();
    app.terminal_rows = 20;
    let tall = "x".repeat(500);
    install_recs(&mut app, vec![rec_send(&tall)]);
    let tall_h = rec_card_height(
        &app.current_tab().turn.recommendations().unwrap().choices[0],
        80,
    ) as u16;
    // ceiling = 20 - 5 = 15; tall card is much larger; floor wins.
    assert_eq!(app.rec_panel_height(80), tall_h);
}

#[test]
fn rec_panel_height_caps_at_ceiling_when_total_exceeds() {
    let mut app = test_app();
    app.terminal_rows = 30;
    // Three short cards, each h=6 → total 18; ceiling 30-5=25.
    install_recs(&mut app, vec![rec_send("a"), rec_send("b"), rec_send("c")]);
    assert_eq!(app.rec_panel_height(80), 18);
}

#[test]
fn rec_panel_height_zero_when_no_recs() {
    let app = test_app();
    assert_eq!(app.rec_panel_height(80), 0);
}

#[test]
fn main_area_width_reflects_debug_panel_split() {
    let mut app = test_app();
    app.terminal_cols = 100;
    assert_eq!(app.main_area_width(), 100);
    app.show_debug_panel = true;
    assert_eq!(app.main_area_width(), 60);
}

/// Regression: `ui::recommendations::render` used `area.width` (= `h_rec[1]`
/// = `main_area.width - 2`) when calling `rec_card_height`, while
/// `rec_panel_height` / `sync_rec_scroll_max` used `main_area.width`. The
/// 2-cell desync clipped the bottom card and undercounted scroll bounds
/// whenever a card's wrap row count differed between the two widths.
///
/// This test pins both code paths to `main_area.width`, and picks a
/// text length that lies in the critical window `(W-10, W-8]` so the
/// old buggy width (`W-2`, content `W-10`) would wrap to a different
/// row count than the correct width (`W`, content `W-8`).
#[test]
fn rec_card_height_matches_predict_and_render_paths() {
    let w: u16 = 50;
    // text length 42 sits exactly at the boundary: fits on 1 row at
    // inner_width 42 (W=50, chrome=8), but spills to 2 rows at
    // inner_width 40 (the old buggy basis).
    let text = "a".repeat(42);
    let choice = rec_send(&text);
    let mut app = test_app();
    app.terminal_cols = w;
    app.terminal_rows = 30;
    install_recs(&mut app, vec![choice.clone()]);

    let predict = app.rec_panel_height(app.main_area_width()) as usize;
    // Same width the renderer now uses (`app.main_area_width()`).
    let render = rec_card_height(&choice, app.main_area_width());
    assert_eq!(predict, render);

    // Sanity: confirm the chosen text *is* a sensitive input — i.e. the
    // old buggy basis (h_rec[1] width = W-2) would have produced a
    // different height. If this ever fails the test no longer guards
    // the regression.
    let buggy = rec_card_height(&choice, app.main_area_width() - 2);
    assert_ne!(
        render, buggy,
        "text length 42 should wrap differently at width 50 vs 48 — \
         pick a different critical input"
    );
}

// ─── Per-tab input history ──────────────────────────────────────────

#[test]
fn input_history_navigates_newest_first_and_restores_draft() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = test_app();
    let tab = app.current_tab_mut();
    tab.record_input_history("older");
    tab.record_input_history("newer");
    tab.input = "draft".into();
    tab.cursor_pos = 2;

    app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(app.current_tab().input, "newer");
    assert_eq!(app.current_tab().cursor_pos, "newer".len());

    app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(app.current_tab().input, "older");

    // The oldest boundary clamps instead of wrapping.
    app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(app.current_tab().input, "older");

    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(app.current_tab().input, "newer");
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(app.current_tab().input, "draft");
    assert_eq!(app.current_tab().cursor_pos, 2);
    assert!(!app.current_tab().input_history_is_browsing());
}

#[test]
fn message_list_focus_routes_arrows_to_completed_turn_selection() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = test_app();
    let tab = app.current_tab_mut();
    tab.record_input_history("historical prompt");
    tab.completed_turns.push(CompletedTurn {
        prompt: "older prompt".into(),
        details: Vec::new(),
        expanded: false,
        trailing_marker: None,
    });
    tab.completed_turns.push(CompletedTurn {
        prompt: "newer prompt".into(),
        details: Vec::new(),
        expanded: false,
        trailing_marker: None,
    });
    tab.selected_completed_turn_idx = Some(1);

    app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(app.current_tab().selected_completed_turn_idx, Some(0));
    assert!(app.current_tab().input.is_empty());
    assert!(!app.current_tab().input_history_is_browsing());

    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(app.current_tab().selected_completed_turn_idx, Some(1));
    assert!(app.current_tab().input.is_empty());
    assert!(!app.current_tab().input_history_is_browsing());
}

#[test]
fn input_history_deduplicates_and_caps_at_fifty() {
    let mut tab = TabSession::default();
    for index in 0..55 {
        tab.record_input_history(&format!("prompt-{index}"));
    }
    assert_eq!(tab.input_history.entries.len(), INPUT_HISTORY_MAX_ENTRIES);
    assert_eq!(tab.input_history.entries.front().unwrap(), "prompt-54");
    assert_eq!(tab.input_history.entries.back().unwrap(), "prompt-5");

    tab.record_input_history("prompt-20");
    assert_eq!(tab.input_history.entries.len(), INPUT_HISTORY_MAX_ENTRIES);
    assert_eq!(tab.input_history.entries.front().unwrap(), "prompt-20");
    assert_eq!(
        tab.input_history
            .entries
            .iter()
            .filter(|entry| entry.as_str() == "prompt-20")
            .count(),
        1
    );
}

#[test]
fn editing_recalled_input_detaches_without_overwriting_history() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = test_app();
    app.current_tab_mut().record_input_history("original");

    app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE));
    assert_eq!(app.current_tab().input, "original!");
    assert!(!app.current_tab().input_history_is_browsing());
    assert_eq!(app.current_tab().input_history.entries[0], "original");

    // Down is a no-op after editing; the edited buffer is now the live draft.
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(app.current_tab().input, "original!");

    app.current_tab_mut().record_input_history("original!");
    assert_eq!(app.current_tab().input_history.entries[0], "original!");
    assert_eq!(app.current_tab().input_history.entries[1], "original");
}

#[test]
fn input_history_preserves_multiline_entries_atomically() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = test_app();
    app.current_tab_mut()
        .record_input_history("first line\nsecond line");

    app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));

    assert_eq!(app.current_tab().input, "first line\nsecond line");
    assert_eq!(app.current_tab().cursor_pos, app.current_tab().input.len());
}

#[test]
fn submitting_prompt_records_only_that_tab_history() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = test_app();
    app.tab_sessions
        .insert("another-tab".into(), TabSession::default());
    app.state = ConnectionState::Connected;
    app.current_tab_mut().session_id = Some(DEFAULT_TAB_ID.into());
    app.current_tab_mut().input = "remember me".into();
    app.current_tab_mut().cursor_pos = "remember me".len();

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(app.current_tab().input.is_empty());
    assert_eq!(app.current_tab().input_history.entries[0], "remember me");
    assert!(
        app.tab_sessions
            .get("another-tab")
            .is_some_and(|tab| tab.input_history.entries.is_empty())
    );
}

#[test]
fn clearing_chat_keeps_input_history_for_the_tab() {
    let mut tab = TabSession::default();
    tab.record_input_history("keep me");

    tab.clear_chat_history();

    assert_eq!(tab.input_history.entries[0], "keep me");
}

#[test]
fn local_slash_command_is_not_recorded_in_input_history() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = test_app();
    app.current_tab_mut().input = "/help".into();
    app.current_tab_mut().cursor_pos = "/help".len();
    app.current_tab_mut().refresh_command_popup();

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(app.help_overlay_visible);
    assert!(app.current_tab().input_history.entries.is_empty());
}

#[test]
fn recommendation_card_keeps_focus_when_input_has_draft() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = test_app();
    stage_surfaced_recommendation(
        &mut app,
        vec![send_choice("pane-A", "ls"), send_choice("pane-B", "pwd")],
        0,
        None,
    );
    app.current_tab_mut().input = "draft".into();
    app.current_tab_mut().cursor_pos = "draft".len();
    app.current_tab_mut().chat_scroll.offset = 7;

    assert!(
        !app.current_tab().input_has_nav_focus(),
        "a visible card owns focus even when the input keeps draft text",
    );

    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(app.current_tab().selected_recommendation, 1);
    assert_eq!(app.current_tab().input, "draft");
    assert_eq!(
        app.current_tab().chat_scroll.offset,
        7,
        "card navigation must not fall through to chat scrolling",
    );

    app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(app.current_tab().selected_recommendation, 0);

    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(app.current_tab().selected_button, 1);
    app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(app.current_tab().selected_button, 0);

    app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    assert_eq!(
        app.current_tab().input,
        "draft",
        "typing stays locked while the card owns focus",
    );
}

#[test]
fn recommendation_card_enter_wins_over_draft_input() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.current_tab_mut().session_id = Some(DEFAULT_TAB_ID.into());
    stage_surfaced_recommendation(&mut app, vec![send_choice("pane-A", "ls")], 0, None);
    app.current_tab_mut().input = "/help".into();
    app.current_tab_mut().cursor_pos = "/help".len();

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(
        app.current_tab().input,
        "/help",
        "executing the card must preserve the user's draft",
    );
    assert!(
        app.current_tab().turn.recommendations().is_none(),
        "Enter should execute the visible card, not submit or slash-parse the draft",
    );
    assert!(
        !app.help_overlay_visible,
        "draft slash commands must not run while a recommendation card owns focus",
    );
}

#[test]
fn typing_is_ignored_while_a_past_turn_is_selected() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = test_app();
    app.current_tab_mut().completed_turns.push(CompletedTurn {
        prompt: "old prompt".into(),
        details: Vec::new(),
        expanded: false,
        trailing_marker: None,
    });
    // Highlight the past turn, as Tab would.
    app.current_tab_mut().selected_completed_turn_idx = Some(0);
    assert!(!app.current_tab().input_has_nav_focus());

    app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));

    assert!(
        app.current_tab().input.is_empty(),
        "typing must be ignored while a past turn is highlighted (input locked)",
    );
    assert_eq!(
        app.current_tab().selected_completed_turn_idx,
        Some(0),
        "selection must survive the keystroke so Tab/Shift+Tab navigation keeps working",
    );
}

#[test]
fn typing_returns_to_input_after_clearing_selection() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = test_app();
    app.current_tab_mut().completed_turns.push(CompletedTurn {
        prompt: "old prompt".into(),
        details: Vec::new(),
        expanded: false,
        trailing_marker: None,
    });
    app.current_tab_mut().selected_completed_turn_idx = Some(0);

    // Esc backs out of history nav, then typing lands in the input again.
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.current_tab().selected_completed_turn_idx, None);
    app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    assert_eq!(app.current_tab().input, "x");
}

#[test]
fn command_popup_keeps_arrow_priority_over_input_history() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = test_app();
    app.current_tab_mut().record_input_history("historical prompt");
    app.current_tab_mut().input.push('/');
    app.current_tab_mut().cursor_pos = 1;
    app.current_tab_mut().refresh_command_popup();
    assert!(
        app.command_popup_visible(),
        "test prerequisite: command popup must be visible after typing '/'",
    );
    assert!(app.current_tab().command_popup_candidates.len() > 1);
    app.current_tab_mut().command_popup_selected = 1;

    app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(app.current_tab().command_popup_selected, 0);
    assert_eq!(app.current_tab().input, "/");
    assert!(!app.current_tab().input_history_is_browsing());

    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(app.current_tab().command_popup_selected, 1);
    assert_eq!(app.current_tab().input, "/");
}

// ─── compute_chip_card_target ───────────────────────────────────────────

/// Stage a tab into `Surfaced { Recommendation(...) }` with the given
/// choices and selected index. Mirrors the side-effects the real
/// `turn_surface_recommendation` would have but skips all the
/// chat-history / scroll bookkeeping so the resulting state stays
/// minimal for the chip-target calculator.
fn stage_surfaced_recommendation(
    app: &mut App,
    choices: Vec<crate::coordinator::RecommendationChoice>,
    selected: usize,
    autofix_target: Option<&str>,
) {
    let prompt = SubmittedPrompt {
        id: 1,
        text: "p".into(),
        submitted_at_unix_s: 0.0,
        autofix: autofix_target.map(|t| AutofixContext {
            target_pane_id: t.into(),
            generation: 0,
        }),
    };
    let recs = crate::coordinator::RecommendationSet {
        recommended_choice: Some(selected),
        choices,
    };
    let tab = app.tab_mut(DEFAULT_TAB_ID);
    tab.selected_recommendation = selected;
    tab.turn = TurnState::Surfaced {
        prompt,
        outcome: TurnOutcome::Recommendation(recs),
        end_pending: false,
    };
}

fn send_choice(parent: &str, input: &str) -> crate::coordinator::RecommendationChoice {
    crate::coordinator::RecommendationChoice {
        choice: 1,
        title: "Run".into(),
        rationale: String::new(),
        actions: vec![crate::coordinator::RecommendedAction::Send {
            parent: parent.into(),
            input: input.into(),
        }],
    }
}

fn open_choice() -> crate::coordinator::RecommendationChoice {
    crate::coordinator::RecommendationChoice {
        choice: 2,
        title: "Open".into(),
        rationale: String::new(),
        actions: vec![crate::coordinator::RecommendedAction::Open {
            target: crate::coordinator::OpenTarget::Tab,
            parent: None,
            cwd: None,
            title: None,
            direction: None,
            profile: None,
        }],
    }
}

#[test]
fn chip_target_returns_none_when_idle() {
    let app = test_app();
    assert_eq!(app.current_tab().compute_chip_card_target(), None);
}

#[test]
fn chip_target_uses_send_parent_when_set() {
    let mut app = test_app();
    stage_surfaced_recommendation(
        &mut app,
        vec![send_choice("pane-A", "ls")],
        0,
        None,
    );
    assert_eq!(
        app.current_tab().compute_chip_card_target(),
        Some("pane-A".to_string()),
    );
}

#[test]
fn chip_target_falls_back_to_autofix_target_when_send_parent_empty() {
    let mut app = test_app();
    // Planner-emitted Send actions in autofix turns leave `parent`
    // blank — `turn_execute_card` fills it from `target_pane_id` at
    // execute time. The chip should already point there now.
    stage_surfaced_recommendation(
        &mut app,
        vec![send_choice("", "fix --auto")],
        0,
        Some("pane-failing"),
    );
    assert_eq!(
        app.current_tab().compute_chip_card_target(),
        Some("pane-failing".to_string()),
    );
}

#[test]
fn chip_target_filters_empty_autofix_target() {
    // C++ treats `pane_session_id == ""` as "no override", so emitting
    // Some("") would let the helper's dedupe believe it pinned the chip
    // while WT silently ignores the event.
    let mut app = test_app();
    stage_surfaced_recommendation(
        &mut app,
        vec![send_choice("", "fix")],
        0,
        Some(""),
    );
    assert_eq!(app.current_tab().compute_chip_card_target(), None);
}

#[test]
fn chip_target_is_none_for_non_send_card() {
    let mut app = test_app();
    stage_surfaced_recommendation(&mut app, vec![open_choice()], 0, None);
    assert_eq!(app.current_tab().compute_chip_card_target(), None);
}

#[test]
fn chip_target_tracks_selected_index() {
    let mut app = test_app();
    stage_surfaced_recommendation(
        &mut app,
        vec![send_choice("pane-A", "ls"), send_choice("pane-B", "pwd")],
        0,
        None,
    );
    assert_eq!(
        app.current_tab().compute_chip_card_target(),
        Some("pane-A".to_string()),
    );
    app.current_tab_mut().selected_recommendation = 1;
    assert_eq!(
        app.current_tab().compute_chip_card_target(),
        Some("pane-B".to_string()),
    );
}

#[test]
fn chip_recompute_dedupes_and_releases_on_idle() {
    // After surfacing a Send card, recompute should record an override.
    // Transitioning back to Idle (here: clear the recs) should make
    // the next recompute observe a different value and clear the
    // last_emitted slot.
    let mut app = test_app();
    stage_surfaced_recommendation(
        &mut app,
        vec![send_choice("pane-A", "ls")],
        0,
        None,
    );
    app.recompute_chip_override(DEFAULT_TAB_ID);
    assert_eq!(
        app.tab_mut(DEFAULT_TAB_ID).last_emitted_chip_override,
        Some("pane-A".to_string()),
    );

    // Drop the surfaced state — chip target now resolves to None and
    // the dedupe slot must follow so a fresh surface re-emits cleanly.
    app.tab_mut(DEFAULT_TAB_ID).turn = TurnState::Idle;
    app.recompute_chip_override(DEFAULT_TAB_ID);
    assert_eq!(
        app.tab_mut(DEFAULT_TAB_ID).last_emitted_chip_override,
        None,
    );
}

#[test]
fn known_cli_id_returns_some_for_all_first_party_clis() {
    use crate::agent_sessions::CliSource;
    assert_eq!(known_cli_id(&CliSource::Claude),  Some("claude"));
    assert_eq!(known_cli_id(&CliSource::Codex),   Some("codex"));
    assert_eq!(known_cli_id(&CliSource::Copilot), Some("copilot"));
    assert_eq!(known_cli_id(&CliSource::Gemini),  Some("gemini"));
    assert_eq!(known_cli_id(&CliSource::OpenCode), Some("opencode"));
}

#[test]
fn known_cli_id_returns_none_for_unknown_variant() {
    use crate::agent_sessions::CliSource;
    assert_eq!(known_cli_id(&CliSource::Unknown("anything".to_string())), None);
}

#[test]
fn enter_on_wsl_history_row_resumes_inside_distro() {
    use crate::agent_sessions::{AgentStatus, CliSource, SessionLocation, SessionOrigin};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let row = crate::agent_sessions::AgentSession {
        key:              "abc-123".to_string(),
        cli_source:       CliSource::Copilot,
        pane_session_id:  None,
        window_id:        None,
        tab_id:           None,
        title:            "t".to_string(),
        cwd:              std::path::PathBuf::from("/home/u/proj"),
        started_at:       std::time::SystemTime::UNIX_EPOCH,
        last_activity_at: std::time::SystemTime::UNIX_EPOCH,
        status:           AgentStatus::Historical,
        last_error:       None,
        current_tool:     None,
        attention_reason: None,
        log_path:         None,
        origin:           SessionOrigin::Unknown,
        location:         SessionLocation::Wsl { distro: "Ubuntu".to_string() },
    };
    let mut app = test_app();
    app.agent_sessions.merge_historical(vec![row]);
    app.current_tab_mut().current_view = View::Agents;
    app.current_tab_mut().agents_list_state.select(Some(0));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let cmd = app
        .last_dispatched_command_for_test()
        .expect("a command was dispatched");
    assert_eq!(cmd.kind, DispatchedCommandKind::NewTabResume);
    let argv = cmd.argv.join(" ");
    assert!(
        argv.contains("wsl -d Ubuntu --cd \"/home/u/proj\" -- bash -lc \"copilot --resume abc-123\""),
        "expected in-distro resume; argv: {argv}"
    );
    // The loading banner keeps the short session id and also names the
    // distro for WSL rows.
    assert!(
        argv.contains("Resuming copilot session abc-123 in Ubuntu (WSL)"),
        "expected distro-named WSL banner; argv: {argv}"
    );
    // WSL rows must not also pass the Windows `-d <cwd>` flag.
    assert!(!argv.contains(" -d /home"), "WSL row must not pass Windows -d cwd");
}
