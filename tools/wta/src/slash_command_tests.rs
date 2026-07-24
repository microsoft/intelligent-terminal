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
fn agent_and_model_pickers_are_mutually_exclusive() {
    let mut app = test_app();
    app.available_models =
        vec![AcpModelInfo { id: "fast".into(), name: "Fast".into(), description: None }];

    app.open_model_picker();
    assert!(app.current_tab().model_picker_open);
    assert!(!app.current_tab().agent_picker_open);

    app.open_agent_picker(0);
    assert!(app.current_tab().agent_picker_open);
    assert!(!app.current_tab().model_picker_open);

    app.open_model_picker();
    assert!(app.current_tab().model_picker_open);
    assert!(!app.current_tab().agent_picker_open);
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

#[test]
fn slash_move_changes_only_the_active_tab() {
    let mut app = test_app();
    app.tab_sessions
        .insert("other-tab".to_string(), TabSession::default());

    run_slash_args(&mut app, "move", "l");

    assert_eq!(
        app.current_tab().agent_pane_position,
        Some("left"),
        "/move l must normalize to the canonical left position"
    );
    assert_eq!(
        app.tab_sessions["other-tab"].agent_pane_position,
        None,
        "/move must not alter another tab's pane position"
    );
}

#[test]
fn slash_move_down_uses_bottom_pane_position() {
    let mut app = test_app();

    run_slash_args(&mut app, "move", "down");

    assert_eq!(
        app.current_tab().agent_pane_position,
        Some("bottom"),
        "/move down must map to the Terminal pane position named bottom"
    );
}

#[test]
fn slash_move_invalid_argument_reopens_position_completion() {
    let mut app = test_app();

    run_slash_args(&mut app, "move", "sideways");

    assert_eq!(app.current_tab().input, "/move ");
    assert_eq!(
        app.current_tab().move_position_candidates.len(),
        commands::MOVE_POSITIONS.len()
    );
    assert!(app.command_popup_state().is_some());
}

#[test]
fn move_position_popup_completes_alias_and_dispatches() {
    let mut app = test_app();
    type_input(&mut app, "/move r");

    assert_eq!(app.current_tab().move_position_candidates.len(), 1);
    assert_eq!(
        app.current_tab().selected_move_position().unwrap().name,
        "right"
    );
    assert!(app.try_handle_slash_on_enter());
    assert_eq!(app.current_tab().agent_pane_position, Some("right"));
    assert!(app.current_tab().input.is_empty());
}

#[test]
fn explicit_empty_agent_allowlist_is_fail_closed() {
    let mut app = test_app();
    app.set_allowed_agent_ids(vec![String::new()]);
    assert!(app.available_agents.is_empty());
}

#[test]
fn switch_agent_event_is_scoped_to_window_and_tab() {
    let payload = build_switch_agent_event("42", "{tab-guid}", "claude");
    let event: serde_json::Value = serde_json::from_str(&payload).expect("valid event json");
    assert_eq!(event["method"], "switch_agent");
    assert_eq!(event["params"]["window_id"], "42");
    assert_eq!(event["params"]["tab_id"], "{tab-guid}");
    assert_eq!(event["params"]["agent_id"], "claude");
}

fn seed_completion_agents(app: &mut App) {
    app.available_agents = vec![
        AvailableAgent {
            id: "copilot".into(),
            display_name: "GitHub Copilot".into(),
        },
        AvailableAgent {
            id: "codex".into(),
            display_name: "Codex".into(),
        },
        AvailableAgent {
            id: "gemini".into(),
            display_name: "Gemini".into(),
        },
    ];
}

#[test]
fn agent_argument_completion_uses_available_agents_in_registry_order() {
    let mut app = test_app();
    seed_completion_agents(&mut app);
    type_input(&mut app, "/AGENT CO");

    let state = app.command_popup_state().expect("agent candidates");
    let crate::ui::PopupCandidates::Agents(candidates) = state.candidates else {
        panic!("expected agent candidates");
    };
    assert_eq!(
        candidates
            .iter()
            .map(|agent| agent.id.as_str())
            .collect::<Vec<_>>(),
        vec!["copilot", "codex"]
    );
    assert_eq!(app.command_ghost_suffix(), Some("pilot"));
}

#[test]
fn agent_trailing_space_opens_completion_with_all_agents() {
    let mut app = test_app();
    seed_completion_agents(&mut app);
    type_input(&mut app, "/agent ");

    let state = app.command_popup_state().expect("all agent candidates");
    let crate::ui::PopupCandidates::Agents(candidates) = state.candidates else {
        panic!("expected agent candidates");
    };
    assert_eq!(
        candidates
            .iter()
            .map(|agent| agent.id.as_str())
            .collect::<Vec<_>>(),
        vec!["copilot", "codex", "gemini"]
    );
}

#[test]
fn agent_argument_arrow_changes_ghost_and_tab_completes() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = test_app();
    seed_completion_agents(&mut app);
    type_input(&mut app, "/agent co");

    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(app.command_ghost_suffix(), Some("dex"));

    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(app.current_tab().input, "/agent codex");
    assert_eq!(app.command_ghost_suffix(), None);
}

#[test]
fn agent_ghost_requires_cursor_at_end() {
    let mut app = test_app();
    seed_completion_agents(&mut app);
    type_input(&mut app, "/agent co");
    app.current_tab_mut().move_cursor_left();

    assert_eq!(app.command_ghost_suffix(), None);
}

#[test]
fn agent_argument_enter_dispatches_highlighted_agent() {
    let mut app = test_app();
    seed_completion_agents(&mut app);
    app.current_agent_id = "copilot".into();
    type_input(&mut app, "/agent co");

    assert!(app.try_handle_slash_on_enter());
    assert!(app.current_tab().input.is_empty());
    assert!(
        app.current_tab().messages.is_empty(),
        "the selected exact agent must dispatch; the raw prefix must not reach unknown-agent handling"
    );
}

#[test]
fn unknown_agent_prefix_does_not_open_completion() {
    let mut app = test_app();
    seed_completion_agents(&mut app);
    type_input(&mut app, "/agent zzz");

    assert!(app.command_popup_state().is_none());
    assert_eq!(app.command_ghost_suffix(), None);
}

#[test]
fn agent_argument_completion_is_hidden_when_transport_is_lost() {
    let mut app = test_app();
    seed_completion_agents(&mut app);
    type_input(&mut app, "/agent co");
    app.transport_lost = true;

    assert!(app.command_popup_state().is_none());
    assert_eq!(app.command_ghost_suffix(), None);
}

// ---- Degraded (transport-lost) gating: only /restart runs ----

#[test]
fn degraded_blocks_non_restart_command() {
    let mut app = test_app();
    app.transport_lost = true;
    app.current_tab_mut().session_id = Some("sid-1".into());

    run_slash(&mut app, "new");

    // /new must NOT have reset the session — it was refused before dispatch
    // because every command but /restart would hit the dead master pipe.
    assert_eq!(
        app.current_tab().session_id,
        Some("sid-1".into()),
        "while the transport is lost, /new must be refused, not run"
    );
    // ...and the user is steered to /restart (the locked token is present in
    // every locale, so this holds regardless of the active language).
    match app.current_tab().messages.last() {
        Some(ChatMessage::System(msg)) => assert!(
            msg.contains("/restart"),
            "the degraded hint must point the user at /restart, got: {msg}"
        ),
        other => panic!("expected a System hint, got {other:?}"),
    }
}

#[test]
fn degraded_blocks_model_command_too() {
    let mut app = test_app();
    app.transport_lost = true;
    app.available_models = vec![AcpModelInfo {
        id: "fast".into(),
        name: "Fast".into(),
        description: None,
    }];

    run_slash(&mut app, "model");

    assert!(
        !app.current_tab().model_picker_open,
        "/model must be refused while the transport is lost"
    );
    assert!(matches!(
        app.current_tab().messages.last(),
        Some(ChatMessage::System(_))
    ));
}

#[test]
fn degraded_still_allows_restart() {
    let mut app = test_app();
    app.transport_lost = true;
    app.state = ConnectionState::Connected;
    app.session_id = "live-sid".to_string();
    app.current_tab_mut().session_id = Some("tab-sid".into());

    run_slash(&mut app, "restart");

    // /restart is the one command exempt from the degraded guard — it ran and
    // moved the connection into Connecting while the stack respawns.
    assert!(
        matches!(app.state, ConnectionState::Connecting(_)),
        "/restart must run even while degraded — it recovers the dead transport"
    );
    assert!(
        app.session_id.is_empty(),
        "/restart must clear the process-level session id even while degraded"
    );
}

// ---- Degraded popup effective-visibility (key-swallow regression) ----

/// Type `text` char-by-char through the real input path so the command popup
/// candidates refresh exactly as they do live.
fn type_input(app: &mut App, text: &str) {
    for ch in text.chars() {
        app.current_tab_mut().insert_input_char(ch);
    }
}

#[test]
fn degraded_popup_hidden_when_prefix_excludes_restart() {
    // Regression: in degraded mode the popup is filtered to /restart only.
    // When the typed prefix can't match /restart (e.g. "/ne"), nothing is
    // drawn — and command_popup_visible() must report false so Up/Down/Tab
    // fall through to their normal handling instead of being swallowed against
    // an invisible popup.
    let mut app = test_app();
    app.transport_lost = true;
    type_input(&mut app, "/ne"); // matches /new, NOT /restart

    assert!(
        app.command_popup_state().is_none(),
        "degraded popup must not render when the prefix excludes /restart"
    );
    assert!(
        !app.command_popup_visible(),
        "command_popup_visible() must be false when the degraded popup isn't drawn, \
         so arrow/Tab keys aren't swallowed"
    );
}

#[test]
fn degraded_popup_visible_when_prefix_matches_restart() {
    let mut app = test_app();
    app.transport_lost = true;
    type_input(&mut app, "/r"); // matches /restart

    assert!(
        app.command_popup_state().is_some(),
        "degraded popup must render when /restart is a prefix match"
    );
    assert!(
        app.command_popup_visible(),
        "command_popup_visible() must be true when /restart is shown"
    );
}

#[test]
fn connected_popup_visible_for_any_prefix() {
    // Sanity: when connected the popup behaves normally — "/ne" shows /new.
    let mut app = test_app();
    assert!(!app.transport_lost);
    type_input(&mut app, "/ne");

    assert!(
        app.command_popup_visible(),
        "a healthy connection must keep the normal popup behavior"
    );
}
