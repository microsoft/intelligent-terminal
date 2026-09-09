use super::tests::test_app;
use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn test_app_with_prompt_rx() -> (
    App,
    tokio::sync::mpsc::UnboundedReceiver<crate::protocol::acp::client::PromptSubmission>,
) {
    let mut app = test_app();
    let (prompt_tx, prompt_rx) = tokio::sync::mpsc::unbounded_channel();
    app.prompt_tx = prompt_tx;
    (app, prompt_rx)
}

fn install_app_event_queue(app: &mut App) -> tokio::sync::mpsc::UnboundedReceiver<AppEvent> {
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
    app.set_event_tx(event_tx);
    event_rx
}

fn handle_scheduled_drain(
    app: &mut App,
    event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    session_id: &str,
) {
    let expected_tab = app
        .bound_tab_for_session(session_id)
        .expect("session is bound to a tab");
    handle_scheduled_drain_for_tab(app, event_rx, &expected_tab);
}

fn handle_scheduled_drain_for_tab(
    app: &mut App,
    event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    expected_tab: &str,
) {
    let event = event_rx.try_recv().expect("input drain event");
    assert!(matches!(
        &event,
        AppEvent::DrainInputQueue {
            tab_id: queued_tab
        } if queued_tab == expected_tab
    ));
    app.handle_event(event);
}

fn bind_tab(app: &mut App, tab_id: &str, session_id: &str) {
    app.state = ConnectionState::Connected;
    app.tab_id = Some(tab_id.to_string());
    app.tab_sessions.entry(tab_id.to_string()).or_default();
    app.tab_mut(tab_id).session_id = Some(session_id.to_string());
    app.session_to_tab
        .insert(session_id.to_string(), tab_id.to_string());
}

fn enter_text(app: &mut App, text: &str) {
    assert!(app.current_tab().input.is_empty());
    app.current_tab_mut().insert_input_str(text);
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
}

fn failure_notification(pane_id: &str, tab_id: &str) -> WtNotification {
    WtNotification {
        severity: WtEventSeverity::Actionable,
        pane_id: pane_id.to_string(),
        tab_id: Some(tab_id.to_string()),
        summary: format!("{pane_id} failed"),
        acknowledged: false,
        age_ticks: 0,
    }
}

fn queued_texts(app: &App, tab_id: &str) -> Vec<String> {
    app.tab_sessions[tab_id]
        .pending_inputs
        .iter()
        .map(|input| input.text.clone())
        .collect()
}

fn actionable_recommendation() -> crate::coordinator::RecommendationSet {
    crate::coordinator::RecommendationSet {
        recommended_choice: Some(0),
        choices: vec![crate::coordinator::RecommendationChoice {
            choice: 1,
            title: "Run fix".into(),
            rationale: "Resolve the failure".into(),
            actions: vec![crate::coordinator::RecommendedAction::Send {
                parent: "pane-1".into(),
                input: "run-fix".into(),
            }],
        }],
    }
}

fn surface_actionable_recommendation(app: &mut App, end_pending: bool) {
    let prompt = app
        .current_tab()
        .turn
        .prompt()
        .cloned()
        .expect("active prompt");
    app.current_tab_mut().turn = TurnState::Surfaced {
        prompt,
        outcome: TurnOutcome::Recommendation(actionable_recommendation()),
        end_pending,
    };
}

#[test]
fn fifo_drains_exactly_one_input_per_terminal_completion() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    let mut event_rx = install_app_event_queue(&mut app);

    enter_text(&mut app, "first");
    assert_eq!(prompt_rx.try_recv().unwrap().text, "first");
    enter_text(&mut app, "second");
    enter_text(&mut app, "third");
    assert_eq!(queued_texts(&app, DEFAULT_TAB_ID), ["second", "third"]);

    app.handle_event(AppEvent::Tick);
    assert!(
        prompt_rx.try_recv().is_err(),
        "non-terminal events must not drain input"
    );

    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: "session-1".into(),
    });
    assert!(prompt_rx.try_recv().is_err());
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");
    assert_eq!(prompt_rx.try_recv().unwrap().text, "second");
    assert_eq!(queued_texts(&app, DEFAULT_TAB_ID), ["third"]);

    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: "session-1".into(),
    });
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");
    assert_eq!(prompt_rx.try_recv().unwrap().text, "third");
    assert!(app.current_tab().pending_inputs.is_empty());
}

#[test]
fn actionable_direct_proposal_blocks_queue_until_execution_resolves_waiter() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    let mut event_rx = install_app_event_queue(&mut app);
    let (recommendation_tx, mut recommendation_rx) = tokio::sync::mpsc::unbounded_channel();
    app.recommendation_tx = recommendation_tx;

    enter_text(&mut app, "A");
    prompt_rx.try_recv().expect("A dispatched");
    enter_text(&mut app, "B");
    surface_actionable_recommendation(&mut app, false);

    let manager = std::sync::Arc::new(
        crate::agent_tools::action_proposal::channel::ProposalChannelManager::new(),
    );
    let prompt_id = app.current_tab().turn.prompt().unwrap().id;
    let channel = manager
        .issue("session-1".into(), prompt_id, Some("pane-1".into()), false)
        .unwrap();
    let context = manager.begin_validation(&channel).unwrap();
    let proposal_id = context.proposal_id;
    let (final_tx, mut final_rx) = tokio::sync::oneshot::channel();
    assert!(manager.accept_validation(&proposal_id, final_tx));
    app.set_proposal_channels(manager);
    app.current_tab_mut().active_direct_proposal_id = Some(proposal_id.clone());

    app.handle_event(AppEvent::DrainInputQueue {
        tab_id: DEFAULT_TAB_ID.into(),
    });

    assert!(prompt_rx.try_recv().is_err());
    assert_eq!(queued_texts(&app, DEFAULT_TAB_ID), ["B"]);
    assert_eq!(
        app.current_tab().active_direct_proposal_id.as_deref(),
        Some(proposal_id.as_str())
    );
    assert!(matches!(
        final_rx.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));

    app.turn_execute_card("session-1");

    assert_eq!(
        final_rx.blocking_recv().unwrap(),
        crate::agent_tools::action_proposal::channel::ProposalFinalStatus::Confirmed
    );
    recommendation_rx.try_recv().expect("choice execution");
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");
    assert_eq!(prompt_rx.try_recv().unwrap().text, "B");
    assert!(app.current_tab().pending_inputs.is_empty());
}

#[test]
fn dismissing_completed_actionable_recommendation_resumes_queue_immediately() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    let mut event_rx = install_app_event_queue(&mut app);

    enter_text(&mut app, "A");
    prompt_rx.try_recv().expect("A dispatched");
    enter_text(&mut app, "B");
    surface_actionable_recommendation(&mut app, false);

    app.turn_cancel("session-1");

    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");
    assert_eq!(prompt_rx.try_recv().unwrap().text, "B");
    assert!(app.current_tab().pending_inputs.is_empty());
}

#[test]
fn dismissing_end_pending_recommendation_waits_for_terminal_completion() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    let mut event_rx = install_app_event_queue(&mut app);

    enter_text(&mut app, "A");
    prompt_rx.try_recv().expect("A dispatched");
    enter_text(&mut app, "B");
    surface_actionable_recommendation(&mut app, true);

    app.turn_cancel("session-1");

    assert!(
        event_rx.try_recv().is_err(),
        "cancel must not schedule queued work before AgentMessageEnd"
    );
    assert!(prompt_rx.try_recv().is_err());
    assert_eq!(queued_texts(&app, DEFAULT_TAB_ID), ["B"]);

    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: "session-1".into(),
    });
    assert!(prompt_rx.try_recv().is_err());
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");
    assert_eq!(prompt_rx.try_recv().unwrap().text, "B");
    assert!(app.current_tab().pending_inputs.is_empty());
}

#[test]
fn slash_clear_completed_recommendation_resumes_preserved_queue() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    let mut event_rx = install_app_event_queue(&mut app);

    enter_text(&mut app, "A");
    prompt_rx.try_recv().expect("A dispatched");
    enter_text(&mut app, "B");
    surface_actionable_recommendation(&mut app, true);
    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: "session-1".into(),
    });
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");
    assert!(prompt_rx.try_recv().is_err());
    assert_eq!(queued_texts(&app, DEFAULT_TAB_ID), ["B"]);

    let spec = commands::lookup("clear").expect("clear command");
    app.handle_slash_command(ParsedCommand {
        kind: spec.kind,
        spec,
        rest: String::new(),
    });

    assert_eq!(queued_texts(&app, DEFAULT_TAB_ID), ["B"]);
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");
    assert_eq!(prompt_rx.try_recv().unwrap().text, "B");
    assert!(app.current_tab().pending_inputs.is_empty());
}

#[test]
fn slash_clear_end_pending_recommendation_waits_for_terminal_completion() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    let mut event_rx = install_app_event_queue(&mut app);

    enter_text(&mut app, "A");
    prompt_rx.try_recv().expect("A dispatched");
    enter_text(&mut app, "B");
    surface_actionable_recommendation(&mut app, true);

    let spec = commands::lookup("clear").expect("clear command");
    app.handle_slash_command(ParsedCommand {
        kind: spec.kind,
        spec,
        rest: String::new(),
    });

    assert!(
        event_rx.try_recv().is_err(),
        "clear must not schedule queued work before AgentMessageEnd"
    );
    assert!(prompt_rx.try_recv().is_err());
    assert_eq!(queued_texts(&app, DEFAULT_TAB_ID), ["B"]);

    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: "session-1".into(),
    });
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");
    assert_eq!(prompt_rx.try_recv().unwrap().text, "B");
}

#[test]
fn cancellation_settlement_releases_exactly_one_queued_input() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    let mut event_rx = install_app_event_queue(&mut app);

    enter_text(&mut app, "A");
    prompt_rx.try_recv().expect("A dispatched");
    enter_text(&mut app, "B");
    enter_text(&mut app, "C");

    app.turn_cancel("session-1");
    let prompt_id = match app.current_tab().turn {
        TurnState::Cancelling { prompt_id } => prompt_id,
        ref state => panic!("expected cancellation barrier, got {state:?}"),
    };
    assert!(event_rx.try_recv().is_err());

    app.handle_event(AppEvent::PromptCancellationSettled {
        prompt_id,
        started: false,
    });
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");

    assert_eq!(prompt_rx.try_recv().unwrap().text, "B");
    assert!(prompt_rx.try_recv().is_err());
    assert_eq!(queued_texts(&app, DEFAULT_TAB_ID), ["C"]);
}

#[test]
fn focused_recommendation_draft_joins_existing_queue_in_fifo_order() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    let mut event_rx = install_app_event_queue(&mut app);

    enter_text(&mut app, "A");
    prompt_rx.try_recv().expect("A dispatched");
    enter_text(&mut app, "B");
    surface_actionable_recommendation(&mut app, false);
    app.current_tab_mut().recommendation_focus = RecommendationFocus::Input;
    app.current_tab_mut().insert_input_str("C");

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(prompt_rx.try_recv().is_err());
    assert_eq!(queued_texts(&app, DEFAULT_TAB_ID), ["B", "C"]);
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");
    assert_eq!(prompt_rx.try_recv().unwrap().text, "B");

    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: "session-1".into(),
    });
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");
    assert_eq!(prompt_rx.try_recv().unwrap().text, "C");
    assert!(app.current_tab().pending_inputs.is_empty());
}

#[test]
fn soft_stop_warning_stays_with_completed_turn_before_next_input_starts() {
    use crate::protocol::acp::soft_stop::SoftStopReason;

    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    let mut event_rx = install_app_event_queue(&mut app);
    let event_tx = app.event_tx.clone().unwrap();

    enter_text(&mut app, "A");
    prompt_rx.try_recv().expect("A dispatched");
    enter_text(&mut app, "B");
    event_tx
        .send(AppEvent::AgentSoftStop {
            session_id: "session-1".into(),
            reason: SoftStopReason::MaxTokens,
        })
        .unwrap();

    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: "session-1".into(),
    });
    assert!(prompt_rx.try_recv().is_err());

    let soft_stop = event_rx.try_recv().expect("soft-stop event");
    assert!(matches!(soft_stop, AppEvent::AgentSoftStop { .. }));
    app.handle_event(soft_stop);
    assert!(prompt_rx.try_recv().is_err());

    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");
    assert_eq!(prompt_rx.try_recv().unwrap().text, "B");
    let completed = app.current_tab().completed_turns.last().unwrap();
    assert_eq!(completed.prompt, "A");
    let expected_warning = t!("system.stopped_max_tokens").into_owned();
    assert!(completed.details.iter().any(|message| matches!(
        message,
        ChatMessage::Notice {
            kind: NoticeKind::Warning,
            text,
        } if text == &expected_warning
    )));
}

#[test]
fn missing_event_sender_keeps_input_until_internal_drain_is_processed() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    enter_text(&mut app, "A");
    prompt_rx.try_recv().expect("A dispatched");
    enter_text(&mut app, "B");

    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: "session-1".into(),
    });
    assert!(prompt_rx.try_recv().is_err());
    assert_eq!(queued_texts(&app, DEFAULT_TAB_ID), ["B"]);

    app.handle_event(AppEvent::DrainInputQueue {
        tab_id: DEFAULT_TAB_ID.into(),
    });
    assert_eq!(prompt_rx.try_recv().unwrap().text, "B");
}

#[test]
fn queued_input_preserves_text_display_images_and_acp_flags() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    let mut event_rx = install_app_event_queue(&mut app);
    app.handle_event(AppEvent::SessionCommandsUpdated {
        session_id: "session-1".into(),
        commands: vec![crate::app_contracts::AcpSessionCommand {
            name: "plan".into(),
            description: "Build a plan".into(),
            input_hint: Some("focus".into()),
            completion_behavior: crate::app_contracts::CompletionBehavior::OptionalFreeText,
        }],
    });
    enter_text(&mut app, "busy");
    prompt_rx.try_recv().unwrap();

    let image = crate::clipboard_image::PastedImage {
        data_base64: "QQ==".into(),
        mime_type: "image/png".into(),
        label: "diagram.png".into(),
    };
    app.current_tab_mut().insert_input_str("/plan inspect ");
    app.current_tab_mut().insert_image_attachment(image.clone());
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let queued = app.current_tab().pending_inputs.front().unwrap();
    assert_eq!(queued.text, "/plan inspect ");
    assert!(queued.display_text.contains("[image: diagram.png]"));
    assert!(queued.agent_command);
    assert!(queued.autofix.is_none());
    assert_eq!(queued.images.len(), 1);
    assert_eq!(queued.images[0].data_base64, image.data_base64);
    let expected_display = queued.display_text.clone();

    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: "session-1".into(),
    });
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");
    let dispatched = prompt_rx.try_recv().unwrap();
    assert!(dispatched.is_agent_command());
    assert_eq!(dispatched.text, "/plan inspect ");
    assert_eq!(dispatched.images.len(), 1);
    assert_eq!(
        app.current_tab().turn.prompt().unwrap().text,
        expected_display
    );
}

#[test]
fn image_only_input_remains_valid_when_queued() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    let mut event_rx = install_app_event_queue(&mut app);
    enter_text(&mut app, "busy");
    prompt_rx.try_recv().unwrap();

    let image = crate::clipboard_image::PastedImage {
        data_base64: "Qg==".into(),
        mime_type: "image/png".into(),
        label: "image.png".into(),
    };
    app.current_tab_mut().insert_image_attachment(image);
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.current_tab().pending_inputs.len(), 1);
    assert!(app.current_tab().pending_inputs[0].text.is_empty());
    assert_eq!(app.current_tab().pending_inputs[0].images.len(), 1);

    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: "session-1".into(),
    });
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");
    let dispatched = prompt_rx.try_recv().unwrap();
    assert!(dispatched.text.is_empty());
    assert_eq!(dispatched.images.len(), 1);
}

#[test]
fn slash_fix_queues_while_help_remains_local() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    enter_text(&mut app, "busy");
    prompt_rx.try_recv().unwrap();

    enter_text(&mut app, "/help");
    assert!(app.help_overlay_visible);
    assert!(app.current_tab().pending_inputs.is_empty());
    assert!(prompt_rx.try_recv().is_err());
    app.help_overlay_visible = false;

    enter_text(&mut app, "/fix explain this");
    assert_eq!(app.current_tab().pending_inputs.len(), 1);
    assert_eq!(
        app.current_tab().pending_inputs[0]
            .autofix
            .as_ref()
            .map(|metadata| metadata.text_kind),
        Some(crate::protocol::acp::client::AutofixTextKind::UserRequest)
    );
    assert_eq!(app.current_tab().pending_inputs[0].text, "explain this");
    assert!(prompt_rx.try_recv().is_err());
}

#[test]
fn mixed_input_producers_share_one_source_agnostic_fifo() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    let mut event_rx = install_app_event_queue(&mut app);
    app.autofix_enabled = false;
    app.handle_event(AppEvent::SessionCommandsUpdated {
        session_id: "session-1".into(),
        commands: vec![crate::app_contracts::AcpSessionCommand {
            name: "plan".into(),
            description: "Build a plan".into(),
            input_hint: Some("focus".into()),
            completion_behavior: crate::app_contracts::CompletionBehavior::OptionalFreeText,
        }],
    });

    enter_text(&mut app, "active");
    prompt_rx.try_recv().unwrap();
    enter_text(&mut app, "/plan first");
    enter_text(&mut app, "/fix manual");
    let generation_before = app.current_tab().autofix.generation;
    app.maybe_trigger_autofix(&failure_notification("pane-a", DEFAULT_TAB_ID));
    assert!(matches!(
        app.current_tab().autofix.bar_snapshot,
        AutofixBarSnapshot::Detected { .. }
    ));
    app.handle_autofix_execute_from_detected("pane-a", Some(DEFAULT_TAB_ID));
    enter_text(&mut app, "normal last");

    assert_eq!(
        queued_texts(&app, DEFAULT_TAB_ID),
        ["/plan first", "manual", "pane-a failed", "normal last"]
    );
    assert!(app.current_tab().autofix.pane_id.is_none());
    assert_eq!(
        app.current_tab().autofix.generation,
        generation_before,
        "enqueue must not mutate the active autofix singleton"
    );
    let queued_generation = app.current_tab().pending_inputs[2]
        .autofix_generation()
        .unwrap();

    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: "session-1".into(),
    });
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");
    assert!(prompt_rx.try_recv().unwrap().is_agent_command());

    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: "session-1".into(),
    });
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");
    assert_eq!(
        prompt_rx.try_recv().unwrap().autofix_text_kind,
        Some(crate::protocol::acp::client::AutofixTextKind::UserRequest)
    );

    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: "session-1".into(),
    });
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");
    assert_eq!(
        prompt_rx.try_recv().unwrap().autofix_text_kind,
        Some(crate::protocol::acp::client::AutofixTextKind::FailureSummary)
    );
    assert_eq!(app.current_tab().autofix.pane_id.as_deref(), Some("pane-a"));
    assert_eq!(app.current_tab().autofix.generation, queued_generation);

    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: "session-1".into(),
    });
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");
    let normal = prompt_rx.try_recv().unwrap();
    assert_eq!(normal.text, "normal last");
    assert!(!normal.is_agent_command());
    assert!(!normal.is_autofix());
    assert!(app.current_tab().pending_inputs.is_empty());
}

#[test]
fn same_pane_failure_after_review_stays_queued_until_end_releases_fifo() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    let mut event_rx = install_app_event_queue(&mut app);
    app.autofix_enabled = true;
    let pane = "pane-a";

    app.maybe_trigger_autofix(&failure_notification(pane, DEFAULT_TAB_ID));
    let first = prompt_rx.try_recv().expect("first autofix submitted");
    assert_eq!(
        first.autofix_text_kind,
        Some(crate::protocol::acp::client::AutofixTextKind::FailureSummary)
    );
    let first_generation = app.current_tab().autofix.generation;

    surface_actionable_recommendation(&mut app, true);
    {
        let autofix = &mut app.current_tab_mut().autofix;
        autofix.pane_id = None;
        autofix.armed_at = None;
        autofix.suggested_pane_id = Some(pane.to_string());
    }
    app.emit_autofix_state_result(DEFAULT_TAB_ID, pane);

    app.maybe_trigger_autofix(&failure_notification(pane, DEFAULT_TAB_ID));

    assert!(matches!(
        app.current_tab().autofix.bar_snapshot,
        AutofixBarSnapshot::Review { .. }
    ));
    assert!(app.current_tab().autofix.pane_id.is_none());
    assert_eq!(
        app.current_tab().autofix.suggested_pane_id.as_deref(),
        Some(pane)
    );
    assert_eq!(
        queued_texts(&app, DEFAULT_TAB_ID),
        [format!("{pane} failed")]
    );
    let queued_generation = app.current_tab().pending_inputs[0]
        .autofix_generation()
        .expect("queued autofix generation");
    assert_eq!(queued_generation, first_generation.wrapping_add(1));
    assert_eq!(
        app.current_tab().autofix.generation,
        first_generation,
        "queueing must not overwrite the surfaced autofix result state"
    );

    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: "session-1".into(),
    });
    assert!(prompt_rx.try_recv().is_err());
    assert_eq!(
        queued_texts(&app, DEFAULT_TAB_ID),
        [format!("{pane} failed")],
        "the surfaced review result must keep the queued failure blocked until the user clears it"
    );

    app.turn_cancel("session-1");
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");

    let second = prompt_rx.try_recv().expect("queued autofix submitted");
    assert_eq!(
        second.autofix_text_kind,
        Some(crate::protocol::acp::client::AutofixTextKind::FailureSummary)
    );
    assert_eq!(app.current_tab().autofix.generation, queued_generation);
    assert_eq!(app.current_tab().autofix.pane_id.as_deref(), Some(pane));
    assert!(app.current_tab().autofix.suggested_pane_id.is_none());
    assert!(matches!(
        app.current_tab().autofix.bar_snapshot,
        AutofixBarSnapshot::Pending { .. }
    ));
    assert!(app.current_tab().pending_inputs.is_empty());
}

#[test]
fn escape_clears_draft_then_newest_queue_item_then_active_autofix() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    app.autofix_enabled = true;
    app.maybe_trigger_autofix(&failure_notification("pane-a", DEFAULT_TAB_ID));
    assert!(prompt_rx.try_recv().unwrap().is_autofix());

    enter_text(&mut app, "first queued");
    enter_text(&mut app, "second queued");
    app.current_tab_mut().insert_input_str("draft");

    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.current_tab().input.is_empty());
    assert_eq!(
        queued_texts(&app, DEFAULT_TAB_ID),
        ["first queued", "second queued"]
    );
    assert!(app.current_tab().turn.is_in_flight());

    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(queued_texts(&app, DEFAULT_TAB_ID), ["first queued"]);
    assert!(app.current_tab().turn.is_in_flight());

    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.current_tab().pending_inputs.is_empty());
    assert!(app.current_tab().turn.is_in_flight());

    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    let prompt_id = match app.current_tab().turn {
        TurnState::Cancelling { prompt_id } => prompt_id,
        ref state => panic!("expected cancellation barrier, got {state:?}"),
    };
    app.handle_event(AppEvent::PromptCancellationSettled {
        prompt_id,
        started: true,
    });
    assert!(app.current_tab().turn.is_idle());
}

#[test]
fn full_queue_preserves_complete_draft_and_attachments() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    enter_text(&mut app, "active");
    prompt_rx.try_recv().unwrap();
    for index in 0..INPUT_QUEUE_CAPACITY {
        enter_text(&mut app, &format!("queued {index}"));
    }
    assert_eq!(app.current_tab().pending_inputs.len(), INPUT_QUEUE_CAPACITY);

    let image = crate::clipboard_image::PastedImage {
        data_base64: "Qw==".into(),
        mime_type: "image/png".into(),
        label: "preserve.png".into(),
    };
    app.current_tab_mut().insert_input_str("/fix diagnose ");
    app.current_tab_mut().insert_image_attachment(image.clone());
    let draft = app.current_tab().input.clone();
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(app.current_tab().pending_inputs.len(), INPUT_QUEUE_CAPACITY);
    assert_eq!(app.current_tab().input, draft);
    let preserved = app.current_tab().attachments.images().next().unwrap();
    assert_eq!(preserved.data_base64, image.data_base64);
    assert!(app.transient_hint.is_some());
}

#[test]
fn automatic_autofix_dedupes_by_pane_and_pane_close_removes_pending() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    app.autofix_enabled = true;
    enter_text(&mut app, "active");
    prompt_rx.try_recv().unwrap();

    app.maybe_trigger_autofix(&failure_notification("pane-a", DEFAULT_TAB_ID));
    app.maybe_trigger_autofix(&failure_notification("pane-b", DEFAULT_TAB_ID));
    app.maybe_trigger_autofix(&failure_notification("pane-a", DEFAULT_TAB_ID));
    assert_eq!(app.current_tab().pending_inputs.len(), 2);
    assert_eq!(
        app.current_tab()
            .pending_inputs
            .iter()
            .filter_map(InputEnvelope::autofix_target_pane)
            .collect::<Vec<_>>(),
        ["pane-a", "pane-b"]
    );
    assert!(app.current_tab().autofix.pane_id.is_none());

    app.handle_autofix_pane_closed(None, "pane-a");
    assert_eq!(
        app.current_tab()
            .pending_inputs
            .iter()
            .filter_map(InputEnvelope::autofix_target_pane)
            .collect::<Vec<_>>(),
        ["pane-b"]
    );
    assert!(app.current_tab().turn.is_in_flight());
}

#[test]
fn queued_manual_fix_tracks_its_source_pane_and_is_dropped_on_close() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    app.source_session_id = Some("pane-a".into());
    enter_text(&mut app, "active");
    prompt_rx.try_recv().unwrap();

    enter_text(&mut app, "/fix explain this");

    assert_eq!(app.current_tab().pending_inputs.len(), 1);
    assert_eq!(
        app.current_tab().pending_inputs[0].autofix_target_pane(),
        Some("pane-a")
    );

    app.handle_autofix_pane_closed(None, "pane-a");

    assert!(app.current_tab().pending_inputs.is_empty());
    assert!(app.current_tab().turn.is_in_flight());
}

#[test]
fn queues_are_isolated_by_tab_and_follow_tab_rekey() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, "tab-a", "session-a");
    let mut event_rx = install_app_event_queue(&mut app);
    enter_text(&mut app, "active a");
    prompt_rx.try_recv().unwrap();
    enter_text(&mut app, "queued a");

    bind_tab(&mut app, "tab-b", "session-b");
    enter_text(&mut app, "active b");
    prompt_rx.try_recv().unwrap();
    enter_text(&mut app, "queued b");

    app.rename_tab_session("tab-a", "tab-a-renamed", Some("window-2"));
    assert!(!app.tab_sessions.contains_key("tab-a"));
    assert_eq!(queued_texts(&app, "tab-a-renamed"), ["queued a"]);
    assert_eq!(queued_texts(&app, "tab-b"), ["queued b"]);

    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: "session-a".into(),
    });
    handle_scheduled_drain(&mut app, &mut event_rx, "session-a");
    let dispatched = prompt_rx.try_recv().unwrap();
    assert_eq!(dispatched.text, "queued a");
    assert_eq!(
        dispatched
            .pane_context
            .as_ref()
            .and_then(|context| context.tab_id.as_deref()),
        Some("tab-a-renamed")
    );
    assert!(app.tab_sessions["tab-a-renamed"].pending_inputs.is_empty());
    assert_eq!(queued_texts(&app, "tab-b"), ["queued b"]);
}

#[test]
fn local_clear_preserves_queue_while_tab_reset_clears_it() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    enter_text(&mut app, "active");
    prompt_rx.try_recv().unwrap();
    enter_text(&mut app, "queued");

    enter_text(&mut app, "/clear");
    assert_eq!(queued_texts(&app, DEFAULT_TAB_ID), ["queued"]);

    app.reset_tab_session_for(DEFAULT_TAB_ID);
    assert!(app.current_tab().pending_inputs.is_empty());
}

fn app_with_pending_input() -> App {
    let mut app = test_app();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    enter_text(&mut app, "active");
    enter_text(&mut app, "queued");
    assert_eq!(app.current_tab().pending_inputs.len(), 1);
    app
}

#[test]
fn existing_session_reset_paths_clear_pending_input() {
    let mut new_app = app_with_pending_input();
    let (new_session_tx, mut new_session_rx) = tokio::sync::mpsc::unbounded_channel();
    new_app.new_session_tx = new_session_tx;
    new_app.current_tab_mut().turn = TurnState::Idle;
    new_app.cmd_new(false);
    new_session_rx.try_recv().expect("/new request");
    assert!(new_app.current_tab().pending_inputs.is_empty());

    let mut restart_app = app_with_pending_input();
    let (restart_tx, mut restart_rx) = tokio::sync::mpsc::unbounded_channel();
    restart_app.restart_tx = restart_tx;
    restart_app.cmd_restart();
    restart_rx.try_recv().expect("/restart request");
    assert!(restart_app.current_tab().pending_inputs.is_empty());

    let mut load_app = app_with_pending_input();
    let (load_session_tx, mut load_session_rx) = tokio::sync::mpsc::unbounded_channel();
    load_app.load_session_tx = load_session_tx;
    load_app.owner_tab_id = Some(DEFAULT_TAB_ID.into());
    load_app.handle_event(AppEvent::WtEvent {
        method: "load_session".into(),
        pane_id: String::new(),
        tab_id: None,
        params: serde_json::json!({
            "tab_id": DEFAULT_TAB_ID,
            "session_id": "loaded-session",
            "cwd": "",
        }),
    });
    load_session_rx.try_recv().expect("load request");
    assert!(load_app.current_tab().pending_inputs.is_empty());

    let mut reset_app = app_with_pending_input();
    reset_app.reset_tab_session_for(DEFAULT_TAB_ID);
    assert!(reset_app.current_tab().pending_inputs.is_empty());

    let mut agent_reset_app = app_with_pending_input();
    agent_reset_app.reset_agent_scoped_state();
    assert!(agent_reset_app.current_tab().pending_inputs.is_empty());
}

#[test]
fn stale_unbound_completion_does_not_drain_active_tab() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "current-session");
    let mut event_rx = install_app_event_queue(&mut app);
    enter_text(&mut app, "active");
    prompt_rx.try_recv().expect("active prompt");
    enter_text(&mut app, "queued");

    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: "retired-session".into(),
    });

    assert!(prompt_rx.try_recv().is_err());
    assert!(event_rx.try_recv().is_err());
    assert_eq!(queued_texts(&app, DEFAULT_TAB_ID), ["queued"]);
    assert!(
        app.current_tab().turn.is_in_flight(),
        "stale completion must not close the active turn"
    );

    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: "current-session".into(),
    });
    handle_scheduled_drain(&mut app, &mut event_rx, "current-session");
    assert_eq!(prompt_rx.try_recv().unwrap().text, "queued");
    assert!(app.current_tab().pending_inputs.is_empty());
}

#[test]
fn stale_unbound_soft_stop_does_not_mutate_active_tab() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "current-session");
    enter_text(&mut app, "active");
    prompt_rx.try_recv().expect("active prompt");
    let messages_before = app.current_tab().messages.clone();

    app.handle_event(AppEvent::AgentSoftStop {
        session_id: "retired-session".into(),
        reason: crate::protocol::acp::soft_stop::SoftStopReason::MaxTokens,
    });

    assert_eq!(app.current_tab().messages, messages_before);
    assert!(app.current_tab().turn.is_in_flight());
}

#[test]
fn stale_event_does_not_fall_back_when_only_background_tab_is_attached() {
    let mut app = test_app();
    bind_tab(&mut app, "background-tab", "background-session");
    app.tab_id = Some("active-tab".into());
    app.tab_mut("active-tab")
        .messages
        .push(ChatMessage::Agent("keep active".into()));

    app.handle_event(AppEvent::AgentError {
        session_id: Some("retired-session".into()),
        failure: crate::protocol::acp::failure::AgentFailure::Protocol {
            code: -32603,
            message: "stale".into(),
        },
        message: "stale".into(),
    });

    assert_eq!(
        app.current_tab().messages,
        [ChatMessage::Agent("keep active".into())]
    );
}

#[test]
fn recoverable_prompt_error_drains_exactly_one_queued_input() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    let mut event_rx = install_app_event_queue(&mut app);
    enter_text(&mut app, "A");
    prompt_rx.try_recv().expect("A dispatched");
    enter_text(&mut app, "B");
    enter_text(&mut app, "C");

    app.handle_event(AppEvent::AgentError {
        session_id: Some("session-1".into()),
        failure: crate::protocol::acp::failure::AgentFailure::Protocol {
            code: -32603,
            message: "recoverable prompt failure".into(),
        },
        message: "recoverable prompt failure".into(),
    });

    assert!(prompt_rx.try_recv().is_err());
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");
    assert_eq!(prompt_rx.try_recv().unwrap().text, "B");
    assert!(prompt_rx.try_recv().is_err());
    assert_eq!(queued_texts(&app, DEFAULT_TAB_ID), ["C"]);
    let failed_turn = app
        .current_tab()
        .completed_turns
        .last()
        .expect("A preserved");
    assert_eq!(failed_turn.prompt, "A");
    assert!(failed_turn.details.iter().any(|message| matches!(
        message,
        ChatMessage::Error(text) if text == "recoverable prompt failure"
    )));
}

#[test]
fn queued_input_waits_for_prompt_reconfiguration() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    let mut event_rx = install_app_event_queue(&mut app);
    enter_text(&mut app, "A");
    prompt_rx.try_recv().expect("A dispatched");
    enter_text(&mut app, "B");
    {
        let tab = app.current_tab_mut();
        tab.config_pending_id = Some("mode".into());
        tab.native_yolo_config_pending = true;
    }

    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: "session-1".into(),
    });
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");
    assert!(prompt_rx.try_recv().is_err());
    assert_eq!(queued_texts(&app, DEFAULT_TAB_ID), ["B"]);

    app.handle_event(AppEvent::SessionConfigSetCompleted {
        session_id: "session-1".into(),
        config_id: "mode".into(),
        value: "code".into(),
        model_compat: false,
    });
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");
    assert_eq!(prompt_rx.try_recv().unwrap().text, "B");
    assert!(app.current_tab().pending_inputs.is_empty());
}

#[test]
fn input_arriving_during_reconfiguration_waits_in_fifo() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    let mut event_rx = install_app_event_queue(&mut app);
    {
        let tab = app.current_tab_mut();
        tab.config_pending_id = Some("mode".into());
        tab.native_yolo_config_pending = true;
    }

    enter_text(&mut app, "queued during reconfiguration");
    assert!(prompt_rx.try_recv().is_err());
    assert_eq!(
        queued_texts(&app, DEFAULT_TAB_ID),
        ["queued during reconfiguration"]
    );

    app.handle_event(AppEvent::SessionConfigSetCompleted {
        session_id: "session-1".into(),
        config_id: "mode".into(),
        value: "code".into(),
        model_compat: false,
    });
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");

    assert_eq!(
        prompt_rx.try_recv().unwrap().text,
        "queued during reconfiguration"
    );
    assert!(app.current_tab().pending_inputs.is_empty());
}

#[test]
fn queued_input_waits_for_yolo_queue_drain_after_reconcile_completion() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    let mut event_rx = install_app_event_queue(&mut app);

    enter_text(&mut app, "A");
    prompt_rx.try_recv().expect("A dispatched");
    enter_text(&mut app, "B");
    app.pending_yolo_reconciles.insert(
        17,
        (
            std::collections::HashSet::from(["session-1".to_string()]),
            true,
        ),
    );

    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: "session-1".into(),
    });
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");
    assert!(prompt_rx.try_recv().is_err());
    assert_eq!(queued_texts(&app, DEFAULT_TAB_ID), ["B"]);

    app.handle_event(AppEvent::RuntimeYoloReconcileCompleted {
        reconcile_id: 17,
        fail_closed: true,
        restart_required: false,
        result: Ok(()),
    });
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");

    assert_eq!(prompt_rx.try_recv().unwrap().text, "B");
    assert!(app.current_tab().pending_inputs.is_empty());
}

#[test]
fn overlapping_yolo_queue_drain_requires_all_reconciles_to_complete() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    let mut event_rx = install_app_event_queue(&mut app);

    enter_text(&mut app, "A");
    prompt_rx.try_recv().expect("A dispatched");
    enter_text(&mut app, "B");
    app.pending_yolo_reconciles.insert(
        17,
        (
            std::collections::HashSet::from(["session-1".to_string()]),
            true,
        ),
    );
    app.pending_yolo_reconciles.insert(
        18,
        (
            std::collections::HashSet::from(["session-1".to_string()]),
            true,
        ),
    );

    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: "session-1".into(),
    });
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");
    assert!(prompt_rx.try_recv().is_err());
    assert_eq!(queued_texts(&app, DEFAULT_TAB_ID), ["B"]);

    app.handle_event(AppEvent::RuntimeYoloReconcileCompleted {
        reconcile_id: 17,
        fail_closed: true,
        restart_required: false,
        result: Ok(()),
    });
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");
    assert!(prompt_rx.try_recv().is_err());
    assert_eq!(queued_texts(&app, DEFAULT_TAB_ID), ["B"]);

    app.handle_event(AppEvent::RuntimeYoloReconcileCompleted {
        reconcile_id: 18,
        fail_closed: true,
        restart_required: false,
        result: Ok(()),
    });
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");

    assert_eq!(prompt_rx.try_recv().unwrap().text, "B");
    assert!(app.current_tab().pending_inputs.is_empty());
}

#[test]
fn fatal_and_unbound_prompt_errors_do_not_drain() {
    let (mut fatal_app, mut fatal_rx) = test_app_with_prompt_rx();
    bind_tab(&mut fatal_app, DEFAULT_TAB_ID, "fatal-session");
    let mut fatal_event_rx = install_app_event_queue(&mut fatal_app);
    enter_text(&mut fatal_app, "A");
    fatal_rx.try_recv().expect("A dispatched");
    enter_text(&mut fatal_app, "B");

    fatal_app.handle_event(AppEvent::AgentError {
        session_id: Some("fatal-session".into()),
        failure: crate::protocol::acp::failure::AgentFailure::ResourceGone {
            message: "session gone".into(),
        },
        message: "session gone".into(),
    });

    assert!(fatal_rx.try_recv().is_err());
    assert!(fatal_event_rx.try_recv().is_err());
    assert_eq!(queued_texts(&fatal_app, DEFAULT_TAB_ID), ["B"]);

    let (mut unbound_app, mut unbound_rx) = test_app_with_prompt_rx();
    bind_tab(&mut unbound_app, DEFAULT_TAB_ID, "current-session");
    let mut unbound_event_rx = install_app_event_queue(&mut unbound_app);
    enter_text(&mut unbound_app, "A");
    unbound_rx.try_recv().expect("A dispatched");
    enter_text(&mut unbound_app, "B");

    unbound_app.handle_event(AppEvent::AgentError {
        session_id: Some("retired-session".into()),
        failure: crate::protocol::acp::failure::AgentFailure::Protocol {
            code: -32603,
            message: "late prompt failure".into(),
        },
        message: "late prompt failure".into(),
    });

    assert!(unbound_rx.try_recv().is_err());
    assert!(unbound_event_rx.try_recv().is_err());
    assert_eq!(queued_texts(&unbound_app, DEFAULT_TAB_ID), ["B"]);
    assert!(
        unbound_app.current_tab().turn.is_in_flight(),
        "stale failure must not close the active turn"
    );
    assert!(
        !unbound_app.current_tab().messages.iter().any(
            |message| matches!(message, ChatMessage::Error(text) if text == "late prompt failure")
        ),
        "stale failure must not surface on the active tab"
    );
}

#[test]
fn tab_error_wakes_fifo_drain_for_the_bound_tab() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    let mut event_rx = install_app_event_queue(&mut app);
    {
        let tab = app.current_tab_mut();
        tab.loading_session = true;
        tab.loading_target_session_id = Some("loaded-session".into());
    }
    app.pending_yolo_session_tabs.insert(DEFAULT_TAB_ID.into());

    enter_text(&mut app, "queued during failed load");
    assert_eq!(
        queued_texts(&app, DEFAULT_TAB_ID),
        ["queued during failed load"]
    );

    app.handle_event(AppEvent::TabError {
        tab_id: DEFAULT_TAB_ID.into(),
        message: "load failed".into(),
    });

    assert!(app.current_tab().input.is_empty());
    assert!(
        prompt_rx.try_recv().is_err(),
        "drain is deferred through AppEvent"
    );
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");
    assert_eq!(
        prompt_rx.try_recv().unwrap().text,
        "queued during failed load"
    );
    assert!(app.current_tab().pending_inputs.is_empty());
}

#[test]
fn prompt_error_before_binding_advances_fifo_and_preserves_the_draft() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    app.state = ConnectionState::Connected;
    let mut event_rx = install_app_event_queue(&mut app);

    enter_text(&mut app, "A");
    let first = prompt_rx.try_recv().expect("A dispatched");
    enter_text(&mut app, "B");
    enter_text(&mut app, "C");
    app.current_tab_mut().insert_input_str("draft");

    app.handle_event(AppEvent::PromptError {
        tab_id: DEFAULT_TAB_ID.into(),
        prompt_id: first.id,
        message: "new_session failed".into(),
    });

    assert_eq!(app.current_tab().input, "draft");
    assert_eq!(queued_texts(&app, DEFAULT_TAB_ID), ["B", "C"]);
    assert_eq!(app.current_tab().completed_turns.len(), 1);
    assert_eq!(app.current_tab().completed_turns[0].prompt, "A");
    assert!(matches!(
        app.current_tab().completed_turns[0].details.last(),
        Some(ChatMessage::Error(text)) if text == "new_session failed"
    ));

    handle_scheduled_drain_for_tab(&mut app, &mut event_rx, DEFAULT_TAB_ID);
    let second = prompt_rx.try_recv().expect("B dispatched");
    assert_eq!(second.text, "B");
    assert!(prompt_rx.try_recv().is_err(), "failure must not retry A");
    assert_eq!(queued_texts(&app, DEFAULT_TAB_ID), ["C"]);
    assert_eq!(app.current_tab().input, "draft");

    app.handle_event(AppEvent::PromptError {
        tab_id: DEFAULT_TAB_ID.into(),
        prompt_id: second.id,
        message: "new_session failed again".into(),
    });

    assert_eq!(app.current_tab().input, "draft");
    assert_eq!(queued_texts(&app, DEFAULT_TAB_ID), ["C"]);
    assert_eq!(app.current_tab().completed_turns.len(), 2);
    assert_eq!(app.current_tab().completed_turns[1].prompt, "B");
    assert!(matches!(
        app.current_tab().completed_turns[1].details.last(),
        Some(ChatMessage::Error(text)) if text == "new_session failed again"
    ));

    handle_scheduled_drain_for_tab(&mut app, &mut event_rx, DEFAULT_TAB_ID);
    assert_eq!(prompt_rx.try_recv().unwrap().text, "C");
    assert!(app.current_tab().pending_inputs.is_empty());
}

#[test]
fn stale_prompt_error_is_ignored_without_waking_the_queue() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    app.state = ConnectionState::Connected;
    let mut event_rx = install_app_event_queue(&mut app);

    enter_text(&mut app, "A");
    let active = prompt_rx.try_recv().expect("A dispatched");
    enter_text(&mut app, "B");

    app.handle_event(AppEvent::PromptError {
        tab_id: DEFAULT_TAB_ID.into(),
        prompt_id: active.id + 1,
        message: "stale prompt failure".into(),
    });

    assert!(event_rx.try_recv().is_err());
    assert!(prompt_rx.try_recv().is_err());
    assert_eq!(queued_texts(&app, DEFAULT_TAB_ID), ["B"]);
    assert!(
        app.current_tab().turn.is_in_flight(),
        "stale failure must not close the active turn"
    );
    assert!(
        !app.current_tab().messages.iter().any(
            |message| matches!(message, ChatMessage::Error(text) if text == "stale prompt failure")
        ),
        "stale failure must not surface on the active tab"
    );
}

#[test]
fn failed_connection_ignores_deferred_yolo_drain_without_popping_input() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    let mut event_rx = install_app_event_queue(&mut app);
    enter_text(&mut app, "A");
    prompt_rx.try_recv().expect("A dispatched");
    enter_text(&mut app, "B");
    app.pending_yolo_reconciles.insert(
        17,
        (
            std::collections::HashSet::from(["session-1".to_string()]),
            true,
        ),
    );

    app.handle_event(AppEvent::AgentError {
        session_id: Some("session-1".into()),
        failure: crate::protocol::acp::failure::AgentFailure::ResourceGone {
            message: "session gone".into(),
        },
        message: "session gone".into(),
    });
    assert!(matches!(app.state, ConnectionState::Failed(_)));

    app.handle_event(AppEvent::RuntimeYoloReconcileCompleted {
        reconcile_id: 17,
        fail_closed: true,
        restart_required: false,
        result: Ok(()),
    });
    handle_scheduled_drain(&mut app, &mut event_rx, "session-1");

    assert!(prompt_rx.try_recv().is_err());
    assert_eq!(queued_texts(&app, DEFAULT_TAB_ID), ["B"]);
}

#[test]
fn recoverable_autofix_error_clears_pending_state() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");
    app.autofix_enabled = false;
    app.maybe_trigger_autofix(&failure_notification("pane-a", DEFAULT_TAB_ID));
    app.handle_autofix_execute_from_detected("pane-a", Some(DEFAULT_TAB_ID));
    prompt_rx.try_recv().expect("autofix prompt");

    assert_eq!(app.current_tab().autofix.pane_id.as_deref(), Some("pane-a"));
    assert!(matches!(
        app.current_tab().autofix.bar_snapshot,
        AutofixBarSnapshot::Pending { .. }
    ));

    app.handle_event(AppEvent::AgentError {
        session_id: Some("session-1".into()),
        failure: crate::protocol::acp::failure::AgentFailure::Protocol {
            code: -32603,
            message: "recoverable autofix failure".into(),
        },
        message: "recoverable autofix failure".into(),
    });

    assert!(app.current_tab().autofix.pane_id.is_none());
    assert!(app.current_tab().autofix.armed_at.is_none());
    assert!(app.current_tab().autofix.trigger_echo_pane.is_none());
    assert!(matches!(
        app.current_tab().autofix.bar_snapshot,
        AutofixBarSnapshot::Idle
    ));
    assert!(app.current_tab().messages.iter().any(|message| matches!(
        message,
        ChatMessage::Error(text) if text == "recoverable autofix failure"
    )));
}

#[test]
fn background_queue_full_does_not_replace_active_tab_hint() {
    let mut app = test_app();
    bind_tab(&mut app, "background-tab", "background-session");
    enter_text(&mut app, "active");
    for index in 0..INPUT_QUEUE_CAPACITY {
        enter_text(&mut app, &format!("queued {index}"));
    }

    bind_tab(&mut app, "active-tab", "active-session");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    app.transient_hint = Some(("keep active hint".into(), deadline));
    app.autofix_enabled = true;

    app.maybe_trigger_autofix(&failure_notification("background-pane", "background-tab"));

    assert_eq!(
        app.transient_hint.as_ref().map(|(text, _)| text.as_str()),
        Some("keep active hint")
    );
    assert_eq!(
        app.tab_sessions["background-tab"].pending_inputs.len(),
        INPUT_QUEUE_CAPACITY
    );
}

#[test]
fn whitespace_only_input_is_ignored() {
    let (mut app, mut prompt_rx) = test_app_with_prompt_rx();
    bind_tab(&mut app, DEFAULT_TAB_ID, "session-1");

    enter_text(&mut app, "   \t ");

    assert!(app.current_tab().pending_inputs.is_empty());
    assert!(app.current_tab().turn.is_idle());
    assert!(prompt_rx.try_recv().is_err());
}
