//! Behavior tests for the agent-pane slash-command system, split out of the
//! large `app.rs` / `commands.rs` test modules so all of it lives in one
//! place: the pure `commands::classify` mapping and the `App` dispatch path.
//!
//! This is a child module of `app` (declared with `#[path]` in app.rs), not
//! of the crate root, so it can reach `App`'s private dispatch methods —
//! exactly like the inline `app::tests` module it borrows `test_app` from.

use super::tests::{test_app, test_app_with_master_rx, test_app_with_new_session_rx};
use super::*;
use crate::protocol::acp::client::MasterExtRequest;

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

fn custom_model(selection_id: &str, model_id: &str) -> CustomModelCatalogEntry {
    CustomModelCatalogEntry {
        selection_id: selection_id.into(),
        api_contract: crate::custom_model_provider::CANONICAL_API_CONTRACT.into(),
        model_id: model_id.into(),
        ..Default::default()
    }
}

fn last_notice(app: &App) -> (NoticeKind, &str) {
    match app.current_tab().messages.last() {
        Some(ChatMessage::Notice { kind, text }) => (*kind, text),
        other => panic!("expected an inline notice, got {other:?}"),
    }
}

fn session_command(
    name: &str,
    description: &str,
    input_hint: Option<&str>,
) -> crate::app_contracts::AcpSessionCommand {
    let completion_behavior = if input_hint.is_some() {
        crate::app_contracts::CompletionBehavior::OptionalFreeText
    } else {
        crate::app_contracts::CompletionBehavior::ExecuteImmediately
    };
    session_command_with_behavior(name, description, input_hint, completion_behavior)
}

fn session_command_with_behavior(
    name: &str,
    description: &str,
    input_hint: Option<&str>,
    completion_behavior: crate::app_contracts::CompletionBehavior,
) -> crate::app_contracts::AcpSessionCommand {
    crate::app_contracts::AcpSessionCommand {
        name: name.into(),
        description: description.into(),
        input_hint: input_hint.map(str::to_string),
        completion_behavior,
    }
}

fn popup_command_names(app: &App) -> Vec<String> {
    match app.command_popup_state().expect("command popup").candidates {
        crate::ui::PopupCandidates::Commands(candidates) => candidates
            .into_iter()
            .map(|candidate| match candidate {
                crate::ui::CommandCandidate::Client(spec) => spec.name.to_string(),
                crate::ui::CommandCandidate::Agent(command) => command.name.clone(),
            })
            .collect(),
        _ => panic!("expected slash-command candidates"),
    }
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
    assert_eq!(
        commands::classify("run cmd /flag"),
        ParseOutcome::NotCommand
    );
}

#[test]
fn agent_commands_merge_after_reserved_commands_and_replace_by_session() {
    let mut app = test_app();
    app.current_tab_mut().session_id = Some("session-1".into());
    app.handle_event(AppEvent::SessionCommandsUpdated {
        session_id: "session-1".into(),
        commands: vec![
            session_command("plan", "Build a plan", None),
            session_command("clear", "Agent collision", None),
        ],
    });
    app.handle_event(AppEvent::SessionCommandsUpdated {
        session_id: "session-2".into(),
        commands: vec![session_command("research", "Research", None)],
    });
    type_input(&mut app, "/");

    let names = popup_command_names(&app);
    assert_eq!(&names[..commands::REGISTRY.len()], {
        &commands::REGISTRY
            .iter()
            .map(|spec| spec.name.to_string())
            .collect::<Vec<_>>()[..]
    });
    assert_eq!(
        names.iter().filter(|name| name.as_str() == "clear").count(),
        1,
        "an Agent command must not shadow a reserved WTA command"
    );
    assert!(names.contains(&"plan".to_string()));
    assert!(!names.contains(&"research".to_string()));

    app.current_tab_mut().clear_input();
    app.handle_event(AppEvent::SessionCommandsUpdated {
        session_id: "session-1".into(),
        commands: vec![session_command("review", "Review changes", None)],
    });
    type_input(&mut app, "/");
    let names = popup_command_names(&app);
    assert!(!names.contains(&"plan".to_string()));
    assert!(names.contains(&"review".to_string()));
}

#[test]
fn agent_command_without_input_submits_as_a_normal_prompt() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.current_tab_mut().session_id = Some("session-1".into());
    app.handle_event(AppEvent::SessionCommandsUpdated {
        session_id: "session-1".into(),
        commands: vec![session_command("plan", "Build a plan", None)],
    });
    type_input(&mut app, "/plan");

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(app.current_tab().input.is_empty());
    assert!(app.current_tab().turn.is_in_flight());
    assert!(!app.current_tab().messages.iter().any(|message| matches!(
        message,
        ChatMessage::Notice {
            kind: NoticeKind::Warning,
            ..
        }
    )));
}

#[test]
fn provider_yolo_command_is_not_reserved_and_submits_as_a_normal_prompt() {
    assert!(
        commands::lookup("yolo").is_none(),
        "WTA must not reserve a provider-owned yolo command"
    );

    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.current_tab_mut().session_id = Some("session-1".into());
    app.handle_event(AppEvent::SessionCommandsUpdated {
        session_id: "session-1".into(),
        commands: vec![session_command("yolo", "Provider Yolo mode", None)],
    });
    type_input(&mut app, "/yolo");

    assert_eq!(popup_command_names(&app), vec!["yolo"]);
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(app.current_tab().input.is_empty());
    assert!(app.current_tab().turn.is_in_flight());
}

#[test]
fn agent_command_with_optional_input_enters_prepared_mode() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.current_tab_mut().session_id = Some("session-1".into());
    app.handle_event(AppEvent::SessionCommandsUpdated {
        session_id: "session-1".into(),
        commands: vec![session_command(
            "review",
            "Review changes",
            Some("focus area"),
        )],
    });
    type_input(&mut app, "/rev");

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(app.current_tab().input, "/review ");
    assert_eq!(app.command_ghost_suffix(), Some("focus area"));
    assert_eq!(app.prepared_command_range(), Some(0..7));
    assert!(app.current_tab().turn.is_idle());
}

#[test]
fn required_free_text_metadata_drives_the_complete_prepared_flow() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.current_tab_mut().session_id = Some("session-1".into());
    app.handle_event(AppEvent::SessionCommandsUpdated {
        session_id: "session-1".into(),
        commands: vec![session_command_with_behavior(
            "intent",
            "Provide an intent",
            None,
            crate::app_contracts::CompletionBehavior::RequireFreeText,
        )],
    });
    type_input(&mut app, "/int");

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(app.current_tab().input, "/intent ");
    assert_eq!(app.prepared_command_range(), Some(0..7));
    assert!(app.current_tab().turn.is_idle());

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(app.current_tab().input, "/intent ");
    assert!(app.current_tab().turn.is_idle());
    assert_eq!(
        last_notice(&app),
        (NoticeKind::Warning, "Provide an intent")
    );

    type_input(&mut app, "describe the task");
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(app.current_tab().input.is_empty());
    assert!(app.current_tab().turn.is_in_flight());
}

#[test]
fn agent_prefix_match_ranks_before_client_substring_match() {
    let mut app = test_app();
    app.current_tab_mut().session_id = Some("session-1".into());
    app.model_picker_models = vec![AcpModelInfo {
        id: "test-model".into(),
        name: "Test model".into(),
        description: None,
    }];
    app.handle_event(AppEvent::SessionCommandsUpdated {
        session_id: "session-1".into(),
        commands: vec![session_command("delta", "Synthetic command", None)],
    });
    type_input(&mut app, "/del");

    let names = popup_command_names(&app);

    assert_eq!(names, vec!["delta", "model"]);

    app.command_popup_down();
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(
        app.current_tab().model_picker_open,
        "the selected Client candidate must dispatch from the combined ranking"
    );
}

#[test]
fn optional_fix_completion_prepares_then_second_enter_runs() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    type_input(&mut app, "/fi");

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(app.current_tab().input, "/fix ");
    assert_eq!(app.prepared_command_range(), Some(0..4));
    assert!(app.current_tab().turn.is_idle());

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(app.current_tab().input.is_empty());
    assert!(!app.current_tab().turn.is_idle());
}

#[test]
fn typed_agent_command_with_arguments_has_no_unknown_warning() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.current_tab_mut().session_id = Some("session-1".into());
    app.handle_event(AppEvent::SessionCommandsUpdated {
        session_id: "session-1".into(),
        commands: vec![session_command(
            "review",
            "Review changes",
            Some("focus area"),
        )],
    });
    type_input(&mut app, "/review parser");

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(app.current_tab().turn.is_in_flight());
    assert!(!app.current_tab().messages.iter().any(|message| matches!(
        message,
        ChatMessage::Notice {
            kind: NoticeKind::Warning,
            ..
        }
    )));
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
fn slash_clear_wipes_active_tab_conversation() {
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
    assert_eq!(last_notice(&app).0, NoticeKind::Info);
}

#[test]
fn slash_new_when_idle_resets_session() {
    let (mut app, _new_session_rx) = test_app_with_new_session_rx();
    app.current_tab_mut().session_id = Some("sid-1".into());
    app.current_tab_mut()
        .messages
        .push(ChatMessage::System("stale".into()));

    run_slash(&mut app, "new");

    assert_eq!(app.current_tab().session_id, None);
    assert!(app.current_tab().messages.is_empty());
}

#[test]
fn runtime_global_change_reconciles_all_live_sessions() {
    let (mut app, mut master_rx) = test_app_with_master_rx();
    app.session_to_tab
        .insert("session-a".into(), DEFAULT_TAB_ID.into());
    app.session_to_tab
        .insert("session-b".into(), DEFAULT_TAB_ID.into());

    app.apply_runtime_yolo_config(Some(true), Some(false));

    let request = master_rx.try_recv().expect("reconcile request");
    let MasterExtRequest::ReconcileSessionYolo {
        sessions,
        fail_closed,
        ..
    } = request
    else {
        panic!("expected ReconcileSessionYolo");
    };
    assert!(!fail_closed);
    assert!(sessions
        .iter()
        .any(|(session, enabled)| session.0.as_ref() == "session-a" && *enabled));
    assert!(sessions
        .iter()
        .any(|(session, enabled)| session.0.as_ref() == "session-b" && *enabled));
}

#[test]
fn runtime_policy_block_forces_off_and_clears_session_override() {
    let (mut app, mut master_rx) = test_app_with_master_rx();
    app.session_to_tab
        .insert("session".into(), DEFAULT_TAB_ID.into());
    app.yolo_state.lock().unwrap().update_runtime(true, false);

    app.apply_runtime_yolo_config(Some(false), Some(true));

    assert!(!app.yolo_state.lock().unwrap().effective("session"));
    let request = master_rx.try_recv().expect("reconcile request");
    let MasterExtRequest::ReconcileSessionYolo {
        sessions,
        fail_closed,
        ..
    } = request
    else {
        panic!("expected ReconcileSessionYolo");
    };
    assert!(fail_closed);
    assert_eq!(sessions.len(), 1);
    assert!(!sessions[0].1);

    app.apply_runtime_yolo_config(Some(false), Some(false));
    assert!(
        !app.yolo_state.lock().unwrap().effective("session"),
        "policy removal must leave the current global default off"
    );
}

#[test]
fn session_attach_reconciles_latest_yolo_state() {
    let (mut app, mut master_rx) = test_app_with_master_rx();
    app.yolo_state.lock().unwrap().update_runtime(true, false);

    app.handle_event(AppEvent::SessionAttached {
        tab_id: DEFAULT_TAB_ID.into(),
        session_id: "new-yolo-session".into(),
        available_models: Vec::new(),
        current_model_id: None,
    });

    let request = master_rx.try_recv().expect("attach reconcile request");
    let MasterExtRequest::ReconcileSessionYolo {
        sessions,
        fail_closed,
        ..
    } = request
    else {
        panic!("expected ReconcileSessionYolo");
    };
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].0 .0.as_ref(), "new-yolo-session");
    assert!(sessions[0].1);
    assert!(!fail_closed);
}

#[test]
fn client_reconciled_session_attach_does_not_duplicate_native_yolo_rpc() {
    let (mut app, mut master_rx) = super::tests::test_app_with_master_rx();
    let session_id = "client-reconciled-lazy-session";
    app.yolo_state
        .lock()
        .unwrap()
        .mark_client_reconciled(session_id.to_string());

    app.handle_event(AppEvent::SessionAttached {
        tab_id: DEFAULT_TAB_ID.into(),
        session_id: session_id.into(),
        available_models: Vec::new(),
        current_model_id: None,
    });

    assert!(
        master_rx.try_recv().is_err(),
        "the client-owned lazy reconciliation must suppress the App duplicate"
    );
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
    assert_eq!(last_notice(&app).0, NoticeKind::Warning);
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
    assert_eq!(last_notice(&app).0, NoticeKind::Info);
}

fn config_option(
    id: &str,
    name: &str,
    category: &str,
    current_value: &str,
) -> crate::app_contracts::AcpSessionConfigOption {
    crate::app_contracts::AcpSessionConfigOption {
        id: id.into(),
        name: name.into(),
        description: Some(format!("Configure {name}")),
        category: Some(category.into()),
        current_value: current_value.into(),
        values: vec![
            crate::app_contracts::AcpSessionConfigValue {
                id: "ask".into(),
                name: "Ask".into(),
                description: None,
            },
            crate::app_contracts::AcpSessionConfigValue {
                id: "code".into(),
                name: "Code".into(),
                description: Some("Write code".into()),
            },
        ],
        native_yolo: false,
    }
}

#[test]
fn slash_config_without_options_notes_none() {
    let mut app = test_app();

    run_slash(&mut app, "config");

    assert!(!app.current_tab().config_picker.is_open());
    assert_eq!(last_notice(&app).0, NoticeKind::Info);
}

#[test]
fn slash_config_opens_ordered_session_options() {
    let mut app = test_app();
    app.current_tab_mut().session_id = Some("session-1".into());
    app.handle_event(AppEvent::SessionConfigUpdated {
        session_id: "session-1".into(),
        options: vec![
            config_option("mode", "Mode", "mode", "ask"),
            config_option("reasoning", "Reasoning", "thought_level", "code"),
        ],
    });

    run_slash(&mut app, "config");

    let state = app.config_popup_state().expect("config picker state");
    assert_eq!(state.options[0].name, "Mode");
    assert_eq!(state.options[1].name, "Reasoning");
    assert!(state.value_option.is_none());
}

#[test]
fn slash_model_opens_the_standard_model_config_option() {
    let mut app = test_app();
    app.current_tab_mut().session_id = Some("session-1".into());
    app.handle_event(AppEvent::SessionConfigUpdated {
        session_id: "session-1".into(),
        options: vec![config_option("agent-model", "Model", "model", "ask")],
    });

    run_slash(&mut app, "model");

    assert!(!app.current_tab().model_picker_open);
    assert!(app.current_tab().config_picker.is_open());
    assert_eq!(
        app.current_tab().config_picker.option_id(),
        Some("agent-model")
    );
}

#[test]
fn escape_from_slash_model_closes_the_config_picker() {
    let mut app = test_app();
    app.current_tab_mut().session_id = Some("session-1".into());
    app.handle_event(AppEvent::SessionConfigUpdated {
        session_id: "session-1".into(),
        options: vec![config_option("agent-model", "Model", "model", "ask")],
    });

    run_slash(&mut app, "model");
    app.config_picker_escape();

    assert!(!app.current_tab().config_picker.is_open());
}

#[test]
fn escape_from_config_value_picker_returns_to_option_list() {
    let mut app = test_app();
    app.current_tab_mut().session_id = Some("session-1".into());
    app.handle_event(AppEvent::SessionConfigUpdated {
        session_id: "session-1".into(),
        options: vec![config_option("agent-model", "Model", "model", "ask")],
    });

    run_slash(&mut app, "config");
    app.config_picker_enter();
    app.config_picker_escape();

    assert!(app.current_tab().config_picker.is_open());
    assert!(app.current_tab().config_picker.option_id().is_none());
}

#[test]
fn slash_model_selection_uses_the_generic_config_request_lifecycle() {
    let (mut app, mut master_rx) = super::tests::test_app_with_master_rx();
    app.current_tab_mut().session_id = Some("session-1".into());
    app.handle_event(AppEvent::SessionConfigUpdated {
        session_id: "session-1".into(),
        options: vec![config_option("agent-model", "Model", "model", "ask")],
    });

    run_slash(&mut app, "model");
    app.config_picker_down();
    app.config_picker_enter();

    match master_rx.try_recv().expect("model config update request") {
        crate::protocol::acp::client::MasterExtRequest::SetSessionConfigOption {
            session_id,
            config_id,
            value,
        } => {
            assert_eq!(session_id.to_string(), "session-1");
            assert_eq!(config_id, "agent-model");
            assert_eq!(value, "code");
        }
        other => panic!("expected SetSessionConfigOption, got {other:?}"),
    }
    assert!(!app.current_tab().config_picker.is_open());
    assert_eq!(
        app.current_tab().config_pending_id.as_deref(),
        Some("agent-model")
    );
    assert!(app.current_tab().model_override.is_none());

    app.handle_event(AppEvent::SessionConfigSetCompleted {
        session_id: "session-1".into(),
        config_id: "agent-model".into(),
        value: "code".into(),
        model_compat: true,
    });

    assert!(app.current_tab().config_pending_id.is_none());
    assert_eq!(app.current_tab().model_override.as_deref(), Some("code"));
    assert!(
        app.config_popup_state().is_none(),
        "the /model deep link remains closed after completion"
    );
}

#[test]
fn failed_model_config_selection_keeps_the_previous_model() {
    let (mut app, _master_rx) = super::tests::test_app_with_master_rx();
    app.current_tab_mut().session_id = Some("session-1".into());
    app.handle_event(AppEvent::SessionConfigUpdated {
        session_id: "session-1".into(),
        options: vec![config_option("agent-model", "Model", "model", "ask")],
    });

    run_slash(&mut app, "model");
    app.config_picker_down();
    app.config_picker_enter();
    app.handle_event(AppEvent::SessionConfigSetFailed {
        session_id: "session-1".into(),
        config_id: "agent-model".into(),
        message: "rejected".into(),
        restart_required: false,
    });

    assert!(app.current_tab().config_pending_id.is_none());
    assert!(app.current_tab().model_override.is_none());
    assert_eq!(last_notice(&app).0, NoticeKind::Error);
}

#[test]
fn config_picker_select_sends_session_scoped_option_request() {
    let (mut app, mut master_rx) = super::tests::test_app_with_master_rx();
    app.current_tab_mut().session_id = Some("session-1".into());
    app.handle_event(AppEvent::SessionConfigUpdated {
        session_id: "session-1".into(),
        options: vec![config_option("mode", "Mode", "mode", "ask")],
    });
    run_slash(&mut app, "config");

    app.config_picker_enter();
    assert_eq!(app.current_tab().config_picker.option_id(), Some("mode"));
    app.config_picker_down();
    app.config_picker_enter();

    match master_rx.try_recv().expect("config update request") {
        crate::protocol::acp::client::MasterExtRequest::SetSessionConfigOption {
            session_id,
            config_id,
            value,
        } => {
            assert_eq!(session_id.to_string(), "session-1");
            assert_eq!(config_id, "mode");
            assert_eq!(value, "code");
        }
        other => panic!("expected SetSessionConfigOption, got {other:?}"),
    }
    assert_eq!(app.current_tab().config_pending_id.as_deref(), Some("mode"));

    app.handle_event(AppEvent::SessionConfigUpdated {
        session_id: "session-1".into(),
        options: vec![config_option("mode", "Mode", "mode", "code")],
    });
    app.handle_event(AppEvent::SessionConfigSetCompleted {
        session_id: "session-1".into(),
        config_id: "mode".into(),
        value: "code".into(),
        model_compat: false,
    });

    assert_eq!(
        app.config_popup_state().unwrap().options[0].current_value,
        "code"
    );
    assert!(app.current_tab().config_pending_id.is_none());
    assert_eq!(last_notice(&app).0, NoticeKind::Success);
    assert!(last_notice(&app).1.contains("Mode: Code"));
}

#[test]
fn failed_native_config_dispatch_clears_prompt_gate() {
    let (mut app, master_rx) = super::tests::test_app_with_master_rx();
    drop(master_rx);
    app.current_tab_mut().session_id = Some("session-1".into());
    let mut option = config_option("mode", "Mode", "mode", "ask");
    option.native_yolo = true;
    app.handle_event(AppEvent::SessionConfigUpdated {
        session_id: "session-1".into(),
        options: vec![option],
    });
    run_slash(&mut app, "config");
    app.config_picker_enter();
    app.config_picker_down();

    app.config_picker_enter();

    assert!(app.current_tab().config_pending_id.is_none());
    assert!(!app.current_tab().native_yolo_config_pending);
    assert!(matches!(
        app.current_tab().messages.last(),
        Some(ChatMessage::Error(message)) if message.contains("/restart")
    ));
}

#[test]
fn restart_required_native_config_failure_keeps_prompt_gate_until_reset() {
    let mut app = test_app();
    app.current_tab_mut().session_id = Some("session-1".into());
    app.current_tab_mut().config_pending_id = Some("mode".into());
    app.current_tab_mut().native_yolo_config_pending = true;
    app.session_to_tab
        .insert("session-1".into(), DEFAULT_TAB_ID.into());

    app.handle_event(AppEvent::SessionConfigSetFailed {
        session_id: "session-1".into(),
        config_id: "mode".into(),
        message: "provider rejected disable".into(),
        restart_required: true,
    });

    assert_eq!(app.current_tab().config_pending_id.as_deref(), Some("mode"));
    assert!(app.current_tab().native_yolo_config_pending);
    assert!(app.prompt_reconfiguration_pending_for_tab(DEFAULT_TAB_ID));

    app.reset_agent_scoped_state();
    assert!(app.current_tab().config_pending_id.is_none());
    assert!(!app.current_tab().native_yolo_config_pending);
}

#[test]
fn unbound_background_config_update_does_not_close_current_picker() {
    let mut app = test_app();
    app.current_tab_mut().session_id = Some("current-session".into());
    app.handle_event(AppEvent::SessionConfigUpdated {
        session_id: "current-session".into(),
        options: vec![config_option("mode", "Mode", "mode", "ask")],
    });
    run_slash(&mut app, "config");

    app.handle_event(AppEvent::SessionConfigUpdated {
        session_id: "background-session".into(),
        options: Vec::new(),
    });

    assert!(app.current_tab().config_picker.is_open());
    assert_eq!(app.config_popup_state().unwrap().options[0].id, "mode");
}

#[test]
fn unbound_background_config_failure_does_not_pollute_current_tab() {
    let mut app = test_app();
    app.current_tab_mut().session_id = Some("current-session".into());
    let message_count = app.current_tab().messages.len();

    app.handle_event(AppEvent::SessionConfigSetFailed {
        session_id: "closed-session".into(),
        config_id: "mode".into(),
        message: "the session is no longer active".into(),
        restart_required: false,
    });

    assert_eq!(app.current_tab().messages.len(), message_count);
}

#[test]
fn slash_model_bare_opens_picker_when_models_present() {
    let mut app = test_app();
    let selected = "custom:provider:local";
    app.set_custom_model_config(vec![custom_model(selected, "local")], Some(selected.into()));

    run_slash(&mut app, "model");

    assert!(
        app.current_tab().model_picker_open,
        "bare /model must open the model picker when models are available"
    );
}

#[test]
fn slash_model_shows_cloud_models() {
    let mut app = test_app();
    app.set_cloud_models(vec![AcpModelInfo {
        id: "cloud".into(),
        name: "Cloud".into(),
        description: None,
    }]);

    run_slash(&mut app, "model");

    assert!(app.current_tab().model_picker_open);
    assert_eq!(app.model_picker_models[0].id, "cloud");
}

#[test]
fn custom_provider_models_replace_agent_duplicates_and_use_byok_labels() {
    let mut app = test_app();
    app.set_custom_model_config(
        vec![
            custom_model("custom:provider-one:qwen/qwen3.5-9b", "qwen/qwen3.5-9b"),
            custom_model(
                "custom:provider-two:deepseek/deepseek-v4-flash",
                "deepseek/deepseek-v4-flash",
            ),
        ],
        Some("custom:provider-two:deepseek/deepseek-v4-flash".into()),
    );

    let merged = app.merge_custom_models(vec![
        AcpModelInfo {
            id: "intelligent-terminal/deepseek/deepseek-v4-flash".into(),
            name: "deepseek/deepseek-v4-flash".into(),
            description: None,
        },
        AcpModelInfo {
            id: "native".into(),
            name: "Native".into(),
            description: None,
        },
    ]);

    assert_eq!(merged.len(), 3);
    assert!(merged.iter().any(|model| model.id == "native"));
    assert!(merged.iter().any(|model| {
        model.id == "custom:provider-one:qwen/qwen3.5-9b" && model.name == "qwen/qwen3.5-9b (BYOK)"
    }));
    assert!(merged.iter().any(|model| {
        model.id == "custom:provider-two:deepseek/deepseek-v4-flash"
            && model.name == "deepseek/deepseek-v4-flash (BYOK)"
    }));
    assert_eq!(
        app.current_model_id.as_deref(),
        Some("custom:provider-two:deepseek/deepseek-v4-flash")
    );
}

#[test]
fn custom_provider_models_normalize_metadata_and_drop_empty_entries() {
    let mut app = test_app();
    app.set_custom_model_config(
        vec![
            custom_model("  custom:provider:model  ", "  provider/model  "),
            custom_model("   ", "  ignored/model  "),
            custom_model("  custom:provider:ignored  ", "   "),
        ],
        Some("  custom:provider:model  ".into()),
    );

    assert_eq!(
        app.custom_model_catalog,
        vec![custom_model("custom:provider:model", "provider/model")]
    );
    assert_eq!(app.available_models.len(), 1);
    assert_eq!(app.available_models[0].id, "custom:provider:model");
    assert_eq!(app.available_models[0].name, "provider/model (BYOK)");
    assert_eq!(
        app.current_model_id.as_deref(),
        Some("custom:provider:model")
    );
}

#[test]
fn helper_status_catalog_combines_cloud_agent_and_byok_models() {
    let mut app = test_app();
    app.set_cloud_models(vec![AcpModelInfo {
        id: "shared-model".into(),
        name: "Shared cloud model".into(),
        description: None,
    }]);
    app.set_custom_model_config(
        vec![custom_model(
            "custom:provider-one:shared-model",
            "shared-model",
        )],
        None,
    );
    app.handle_event(AppEvent::AgentConnected {
        name: "Test Agent".into(),
        model: None,
        version: None,
        session_id: "session-1".into(),
        available_models: vec![AcpModelInfo {
            id: "agent-only".into(),
            name: "Agent model".into(),
            description: None,
        }],
        current_model_id: Some("agent-only".into()),
        load_session_supported: false,
        image_supported: false,
        session_capabilities_ready: true,
    });

    assert_eq!(app.available_models.len(), 3);
    assert!(app
        .available_models
        .iter()
        .any(|model| model.id == "shared-model"));
    assert!(app
        .available_models
        .iter()
        .any(|model| model.id == "agent-only"));
    assert!(app
        .available_models
        .iter()
        .any(|model| model.id == "custom:provider-one:shared-model"
            && model.name == "shared-model (BYOK)"));
    assert_eq!(app.model_picker_models.len(), 2);
    assert!(app
        .model_picker_models
        .iter()
        .all(|model| !model.id.starts_with("custom:")));
}

#[test]
fn private_cloud_catalog_survives_bare_agent_model_response() {
    let mut app = test_app();
    app.set_custom_model_config(vec![custom_model("custom:provider:byok", "byok")], None);
    app.handle_event(AppEvent::CloudModelsAvailable(vec![AcpModelInfo {
        id: "cloud-native".into(),
        name: "Cloud Native".into(),
        description: None,
    }]));
    app.handle_event(AppEvent::AgentConnected {
        name: "Test Agent".into(),
        model: None,
        version: None,
        session_id: "session-1".into(),
        available_models: Vec::new(),
        current_model_id: None,
        load_session_supported: false,
        image_supported: false,
        session_capabilities_ready: true,
    });

    assert_eq!(app.cloud_models.len(), 1);
    assert_eq!(app.cloud_models[0].id, "cloud-native");
    assert!(
        app.agent_models.is_empty(),
        "private cloud metadata must not be reclassified as an ACP selector"
    );
    assert!(app
        .available_models
        .iter()
        .any(|model| model.id == "cloud-native"));
    assert!(app
        .available_models
        .iter()
        .any(|model| model.id == "custom:provider:byok"));
}

#[test]
fn agent_and_model_pickers_are_mutually_exclusive() {
    let mut app = test_app();
    let selected = "custom:provider:local";
    app.set_custom_model_config(vec![custom_model(selected, "local")], Some(selected.into()));

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
fn slash_model_direct_current_byok_is_a_noop() {
    let mut app = test_app();
    let selected = "custom:provider:smart";
    app.set_custom_model_config(vec![custom_model(selected, "smart")], Some(selected.into()));

    run_slash_args(&mut app, "model", selected);

    assert_eq!(
        app.current_tab().model_override.as_deref(),
        None,
        "confirming the current BYOK row must not create a pane override"
    );
    assert!(
        !app.current_tab().model_picker_open,
        "confirming the current BYOK model must not leave the picker open"
    );
}

#[test]
fn slash_model_only_shows_cloud_choices_while_cloud_is_active() {
    let mut app = test_app();
    app.set_cloud_models(vec![AcpModelInfo {
        id: "cloud".into(),
        name: "Cloud".into(),
        description: None,
    }]);
    app.set_custom_model_config(vec![custom_model("custom:provider:local", "local")], None);
    app.current_model_id = Some("cloud".into());

    let state = {
        app.open_model_picker();
        app.model_popup_state().expect("picker state")
    };
    assert_eq!(state.models.len(), 1);
    assert_eq!(state.models[0].id, "cloud");
    assert_eq!(state.disabled, vec![false]);

    app.close_model_picker();
    run_slash_args(&mut app, "model", "custom:provider:local");
    assert_eq!(app.current_tab().model_override, None);
    assert!(!app.current_tab().model_picker_open);
    assert_eq!(last_notice(&app).0, NoticeKind::Error);
}

#[test]
fn slash_model_only_shows_selected_byok_while_byok_is_active() {
    let mut app = test_app();
    let selected = "custom:provider:local";
    app.set_cloud_models(vec![AcpModelInfo {
        id: "cloud".into(),
        name: "Cloud".into(),
        description: None,
    }]);
    app.set_custom_model_config(
        vec![
            custom_model(selected, "local"),
            custom_model("custom:provider:other", "other"),
        ],
        Some(selected.into()),
    );

    app.open_model_picker();
    let state = app.model_popup_state().expect("picker state");
    assert_eq!(state.models.len(), 1);
    assert_eq!(state.models[0].id, selected);
    assert_eq!(state.disabled, vec![false]);

    app.close_model_picker();
    run_slash_args(&mut app, "model", "custom:provider:other");
    assert_eq!(app.current_tab().model_override, None);
    assert!(!app.current_tab().model_picker_open);
    assert_eq!(last_notice(&app).0, NoticeKind::Error);

    run_slash_args(&mut app, "model", "cloud");
    assert_eq!(app.current_tab().model_override, None);
    assert!(!app.current_tab().model_picker_open);
    assert_eq!(last_notice(&app).0, NoticeKind::Error);
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
        app.tab_sessions["other-tab"].agent_pane_position, None,
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
    let payload = build_switch_agent_event(
        "42",
        "{tab-guid}",
        "claude",
        &crate::agent_source::AgentSource::Wsl {
            distro: "Ubuntu".to_string(),
        },
    );
    let event: serde_json::Value = serde_json::from_str(&payload).expect("valid event json");
    assert_eq!(event["method"], "switch_agent");
    assert_eq!(event["params"]["window_id"], "42");
    assert_eq!(event["params"]["tab_id"], "{tab-guid}");
    assert_eq!(event["params"]["agent_id"], "claude");
    assert_eq!(event["params"]["agent_source"], "wsl");
    assert_eq!(event["params"]["wsl_distro"], "Ubuntu");
}

fn seed_completion_agents(app: &mut App) {
    app.available_agents = vec![
        AvailableAgent {
            id: "copilot".into(),
            display_name: "GitHub Copilot".into(),
            source: crate::agent_source::AgentSource::Host,
        },
        AvailableAgent {
            id: "codex".into(),
            display_name: "Codex".into(),
            source: crate::agent_source::AgentSource::Host,
        },
        AvailableAgent {
            id: "gemini".into(),
            display_name: "Gemini".into(),
            source: crate::agent_source::AgentSource::Host,
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

    let highlighted = app.selected_agent_command_candidate();
    assert_eq!(highlighted.map(|agent| agent.id.as_str()), Some("copilot"));
    let command =
        agent_command_on_enter(&app.current_tab().input, highlighted).expect("agent command");
    assert_eq!(command.kind, CommandKind::Agent);
    assert_eq!(
        command.rest, "copilot",
        "Enter must dispatch the highlighted agent once the completion list is visible"
    );
}

#[test]
fn agent_argument_arrow_changes_ghost_but_tab_does_not_complete() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = test_app();
    seed_completion_agents(&mut app);
    type_input(&mut app, "/agent co");

    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(app.command_ghost_suffix(), Some("dex"));

    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(app.current_tab().input, "/agent co");
    assert_eq!(app.command_ghost_suffix(), Some("dex"));
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
    type_input(&mut app, "/agent co");

    let highlighted = app.selected_agent_command_candidate();
    let command =
        agent_command_on_enter(&app.current_tab().input, highlighted).expect("agent command");
    assert_eq!(command.kind, CommandKind::Agent);
    assert_eq!(command.rest, "copilot");
}

#[test]
fn unknown_agent_prefix_does_not_open_completion() {
    let mut app = test_app();
    seed_completion_agents(&mut app);
    type_input(&mut app, "/agent zzz");

    assert!(app.command_popup_state().is_none());
    assert_eq!(app.command_ghost_suffix(), None);
}

/// Type `text` char-by-char through the real input path so the command popup
/// candidates refresh exactly as they do live.
fn type_input(app: &mut App, text: &str) {
    for ch in text.chars() {
        app.current_tab_mut().insert_input_char(ch);
    }
}

#[test]
fn connected_popup_visible_for_any_prefix() {
    let mut app = test_app();
    type_input(&mut app, "/ne");

    assert!(
        app.command_popup_visible(),
        "a matching command prefix must keep the popup visible"
    );
}

#[test]
fn connected_popup_matches_command_name_substrings() {
    let mut app = test_app();
    type_input(&mut app, "/lear");

    assert!(
        app.command_popup_visible(),
        "typing a substring of /clear must show the command popup"
    );
}
