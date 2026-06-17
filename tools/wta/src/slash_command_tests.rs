//! Behavior tests for the agent-pane slash-command system, split out of the
//! large `app.rs` / `commands.rs` test modules so all of it lives in one
//! place: the pure `commands::classify` mapping and the `App` dispatch path.
//!
//! This is a child module of `app` (declared with `#[path]` in app.rs), not
//! of the crate root, so it can reach `App`'s private dispatch methods —
//! exactly like the inline `app::tests` module it borrows `test_app` from.

use super::tests::test_app;
use super::*;

/// Dispatch a zero-arg slash command by name through the real
/// `handle_slash_command` path, the way the Enter handler does.
fn run_slash(app: &mut App, name: &str) {
    let spec = commands::lookup(name).expect("name is a registered command");
    app.handle_slash_command(ParsedCommand {
        kind: spec.kind,
        spec,
        rest: String::new(),
    });
}

// ---- commands::classify — the pure input → intent mapping ----

#[test]
fn classify_known_command() {
    match commands::classify("/stop") {
        ParseOutcome::Command(c) => assert_eq!(c.kind, CommandKind::Stop),
        other => panic!("expected Command, got {other:?}"),
    }
    match commands::classify("/help me please") {
        ParseOutcome::Command(c) => {
            assert_eq!(c.kind, CommandKind::Help);
            assert_eq!(c.rest, "me please");
        }
        other => panic!("expected Command, got {other:?}"),
    }
}

#[test]
fn classify_unknown_keeps_attempted_token() {
    // Token carries its leading `/`, and trailing args are dropped from it.
    assert_eq!(
        commands::classify("/nope"),
        ParseOutcome::Unknown("/nope".to_string())
    );
    assert_eq!(
        commands::classify("/nope foo bar"),
        ParseOutcome::Unknown("/nope".to_string())
    );
    // Leading whitespace is trimmed before the token is taken.
    assert_eq!(
        commands::classify("   /missing"),
        ParseOutcome::Unknown("/missing".to_string())
    );
}

#[test]
fn classify_not_a_command() {
    assert_eq!(commands::classify("hello"), ParseOutcome::NotCommand);
    // `//literal` escape is a prompt, not an unknown-command warning.
    assert_eq!(commands::classify("//etc/hosts"), ParseOutcome::NotCommand);
    // Bare slash / whitespace-only slash have no token to name.
    assert_eq!(commands::classify("/"), ParseOutcome::NotCommand);
    assert_eq!(commands::classify("/  "), ParseOutcome::NotCommand);
    // A `/` in the middle of a prompt is not an attempt.
    assert_eq!(commands::classify("run cmd /flag"), ParseOutcome::NotCommand);
}

// ---- App dispatch — state effects via handle_slash_command ----

#[test]
fn slash_help_toggles_overlay() {
    let mut app = test_app();
    assert!(!app.help_overlay_visible);
    run_slash(&mut app, "help");
    assert!(app.help_overlay_visible);
    run_slash(&mut app, "help");
    assert!(!app.help_overlay_visible);
}

#[test]
fn slash_clear_wipes_active_tab_history() {
    let mut app = test_app();
    app.current_tab_mut()
        .messages
        .push(ChatMessage::System("stale".into()));
    app.current_tab_mut().selected_completed_turn_idx = Some(0);

    run_slash(&mut app, "clear");

    assert!(app.current_tab().messages.is_empty());
    assert_eq!(app.current_tab().selected_completed_turn_idx, None);
}

#[test]
fn slash_stop_when_idle_notes_nothing_to_stop() {
    let mut app = test_app();
    // Fresh tab: turn is Idle, so /stop only emits the advisory message.
    assert!(!app.current_tab().turn.is_in_flight());

    run_slash(&mut app, "stop");

    assert_eq!(app.current_tab().messages.len(), 1);
    assert!(matches!(
        app.current_tab().messages.last(),
        Some(ChatMessage::System(_))
    ));
}

#[test]
fn slash_new_when_idle_resets_session() {
    let mut app = test_app();
    app.current_tab_mut().session_id = Some("sid-1".into());
    app.current_tab_mut()
        .messages
        .push(ChatMessage::System("stale".into()));

    run_slash(&mut app, "new");

    assert_eq!(app.current_tab().session_id, None);
    assert!(app.current_tab().messages.is_empty());
}

/// Dispatch a slash command with free-form args (e.g. `/model gpt-5`) through
/// the same `handle_slash_command` path the Enter handler uses.
fn run_slash_args(app: &mut App, name: &str, rest: &str) {
    let spec = commands::lookup(name).expect("name is a registered command");
    app.handle_slash_command(ParsedCommand {
        kind: spec.kind,
        spec,
        rest: rest.to_string(),
    });
}

#[test]
fn slash_sessions_opens_agents_view() {
    let mut app = test_app();
    assert_eq!(app.current_tab().current_view, View::Chat);

    run_slash(&mut app, "sessions");

    assert_eq!(
        app.current_tab().current_view,
        View::Agents,
        "/sessions must switch the active tab to the session-management view"
    );
}

#[test]
fn slash_restart_resets_connection_and_clears_sessions() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.session_id = "live-sid".to_string();
    app.current_tab_mut().session_id = Some("tab-sid".into());
    app.current_tab_mut()
        .messages
        .push(ChatMessage::System("stale".into()));

    run_slash(&mut app, "restart");

    assert!(
        matches!(app.state, ConnectionState::Connecting(_)),
        "/restart must move the connection into Connecting while the stack respawns"
    );
    assert!(
        app.session_id.is_empty(),
        "/restart must clear the process-level session id"
    );
    assert_eq!(
        app.current_tab().session_id,
        None,
        "/restart must drop each tab's session so the next prompt gets a fresh one"
    );
    assert!(
        app.current_tab().messages.is_empty(),
        "/restart must wipe per-tab chat history"
    );
}

#[test]
fn slash_fix_when_idle_submits_autofix_turn() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    let gen_before = app.current_tab().autofix.generation;
    assert!(app.current_tab().turn.is_idle());

    run_slash(&mut app, "fix");

    assert!(
        !app.current_tab().turn.is_idle(),
        "/fix on an idle tab must submit an autofix turn"
    );
    assert_eq!(
        app.current_tab().autofix.generation,
        gen_before.wrapping_add(1),
        "/fix must bump the autofix generation so stale responses are dropped"
    );
}

#[test]
fn slash_fix_while_busy_does_not_resubmit() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    // First /fix arms an in-flight turn.
    run_slash(&mut app, "fix");
    assert!(!app.current_tab().turn.is_idle());
    let gen_after_first = app.current_tab().autofix.generation;

    // Second /fix while busy must be refused (busy advisory), not resubmitted.
    run_slash(&mut app, "fix");
    assert_eq!(
        app.current_tab().autofix.generation,
        gen_after_first,
        "/fix while a turn is in flight must not bump generation / resubmit"
    );
    assert!(matches!(
        app.current_tab().messages.last(),
        Some(ChatMessage::System(_))
    ));
}

#[test]
fn slash_model_without_models_notes_none() {
    let mut app = test_app();
    assert!(app.available_models.is_empty());

    run_slash(&mut app, "model");

    assert!(
        !app.current_tab().model_picker_open,
        "/model must not open the picker when no models are available"
    );
    assert!(matches!(
        app.current_tab().messages.last(),
        Some(ChatMessage::System(_))
    ));
}

#[test]
fn slash_model_bare_opens_picker_when_models_present() {
    let mut app = test_app();
    app.available_models = vec![
        AcpModelInfo { id: "fast".into(), name: "Fast".into(), description: None },
        AcpModelInfo { id: "smart".into(), name: "Smart".into(), description: None },
    ];

    run_slash(&mut app, "model");

    assert!(
        app.current_tab().model_picker_open,
        "bare /model must open the model picker when models are available"
    );
}

#[test]
fn slash_model_direct_switch_sets_override() {
    let mut app = test_app();
    app.available_models = vec![
        AcpModelInfo { id: "fast".into(), name: "Fast".into(), description: None },
        AcpModelInfo { id: "smart".into(), name: "Smart".into(), description: None },
    ];

    run_slash_args(&mut app, "model", "smart");

    assert_eq!(
        app.current_tab().model_override.as_deref(),
        Some("smart"),
        "/model <id> must pin the active tab's per-pane model override"
    );
    assert!(
        !app.current_tab().model_picker_open,
        "a direct /model <id> switch must not leave the picker open"
    );
}

// ---- /switch-agent ----

#[test]
fn slash_switch_agent_no_arg_lists_agents() {
    let mut app = test_app();
    // Bare /switch-agent: no agent name provided. Should emit a system message
    // listing available agents and NOT trigger a restart. We verify no restart
    // by checking session_id is unchanged (restart clears it).
    app.session_id = "pre-existing-sid".to_string();
    run_slash(&mut app, "switch-agent");

    assert_eq!(
        app.current_tab().messages.len(),
        1,
        "bare /switch-agent must emit exactly one system message"
    );
    assert!(
        matches!(app.current_tab().messages.last(), Some(ChatMessage::System(_))),
        "bare /switch-agent must emit a System message"
    );
    // session_id must be unchanged — no restart was triggered.
    assert_eq!(
        app.session_id, "pre-existing-sid",
        "bare /switch-agent must not clear the session id (no restart)"
    );
}

#[test]
fn slash_switch_agent_unknown_agent_emits_error() {
    let mut app = test_app();
    app.session_id = "pre-existing-sid".to_string();
    run_slash_args(&mut app, "switch-agent", "does-not-exist");

    assert_eq!(
        app.current_tab().messages.len(),
        1,
        "/switch-agent <unknown> must emit exactly one error message"
    );
    assert!(
        matches!(app.current_tab().messages.last(), Some(ChatMessage::System(_))),
        "/switch-agent <unknown> must emit a System message"
    );
    // session_id must be unchanged — no restart was triggered for unknown agent.
    assert_eq!(
        app.session_id, "pre-existing-sid",
        "/switch-agent <unknown> must not clear session id (no restart)"
    );
}

#[test]
fn slash_switch_agent_known_agent_triggers_restart() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;

    // "claude" is a known agent in the registry.
    run_slash_args(&mut app, "switch-agent", "claude");

    assert!(
        matches!(app.state, ConnectionState::Connecting(_)),
        "/switch-agent claude must transition to Connecting state"
    );
    assert!(
        app.session_id.is_empty(),
        "/switch-agent must clear the process-level session id"
    );
    assert_eq!(
        app.current_tab().session_id,
        None,
        "/switch-agent must drop the tab session so a fresh one is created"
    );
    // A confirmation message must appear in chat.
    assert!(
        !app.current_tab().messages.is_empty(),
        "/switch-agent must emit a confirmation message"
    );
    assert!(
        matches!(app.current_tab().messages.last(), Some(ChatMessage::System(_))),
        "/switch-agent must emit a System confirmation message"
    );
}

#[test]
fn slash_switch_agent_case_insensitive() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;

    run_slash_args(&mut app, "switch-agent", "CLAUDE");

    assert!(
        matches!(app.state, ConnectionState::Connecting(_)),
        "/switch-agent CLAUDE (uppercase) must be accepted case-insensitively"
    );
}

#[test]
fn slash_switch_agent_all_known_agents_accepted() {
    // Every agent in the registry must be accepted by /switch-agent.
    for profile in crate::agent_registry::KNOWN_AGENTS {
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        run_slash_args(&mut app, "switch-agent", profile.id);
        assert!(
            matches!(app.state, ConnectionState::Connecting(_)),
            "/switch-agent {} must be accepted (id is in KNOWN_AGENTS)",
            profile.id
        );
    }
}
