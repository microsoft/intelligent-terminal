use super::tests::{complete_autofix_capture, test_app};
use super::*;
use crate::protocol::acp::soft_stop::SoftStopReason;

fn connected_app() -> (App, mpsc::UnboundedReceiver<PromptSubmission>) {
    let mut app = test_app();
    let (tx, rx) = mpsc::unbounded_channel();
    app.prompt_tx = tx;
    app.state = ConnectionState::Connected;
    app.tab_id = Some("queue-tab".into());
    app.tab_mut("queue-tab").session_id = Some("queue-session".into());
    app.session_to_tab
        .insert("queue-session".into(), "queue-tab".into());
    (app, rx)
}

fn enter(app: &mut App, text: &str) {
    app.current_tab_mut().input = text.into();
    app.current_tab_mut().cursor_pos = text.len();
    app.current_tab_mut().refresh_command_popup();
    app.handle_event(AppEvent::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )));
}

fn shell_event(app: &mut App, sequence: &str) {
    app.handle_event(AppEvent::WtEvent {
        method: "vt_sequence".into(),
        pane_id: "source".into(),
        tab_id: Some("queue-tab".into()),
        params: serde_json::json!({ "sequence": sequence }),
    });
}

#[test]
fn recall_restores_image_placement_without_copying_payload_and_refuses_busy_drafts() {
    let _locale = crate::test_support::lock_locale();
    for prefix in ["before ", "/fix inspect "] {
        let (mut app, mut rx) = connected_app();
        app.current_tab_mut().config_pending_id = Some("hold".into());
        let tab = app.current_tab_mut();
        tab.input = format!("{prefix}after");
        tab.cursor_pos = prefix.len();
        let image = crate::clipboard_image::PastedImage {
            data_base64: "aW1hZ2U=".into(),
            mime_type: "image/png".into(),
            label: "evidence.png".into(),
        };
        let payload = image.data_base64.as_ptr();
        tab.attachments
            .insert_image(&mut tab.input, &mut tab.cursor_pos, image);
        let draft = tab.input.clone();
        app.enqueue_input(prefix.starts_with("/fix").then(|| "inspect after".into()));
        let old = &app.current_tab().prompt_queue.entries[0];
        let old_id = old.submission.id;
        let token = old.submission.cancellation_token();
        app.current_tab_mut().input = "do not overwrite".into();
        app.recall_last_pending_input();
        assert_eq!(app.current_tab().input, "do not overwrite");
        assert_eq!(app.current_tab().prompt_queue.entries.len(), 1);
        assert!(!token.is_cancelled());
        app.current_tab_mut().clear_input();
        app.recall_last_pending_input();
        assert!(token.is_cancelled());
        assert_eq!(app.current_tab().input, draft);
        assert_eq!(
            app.current_tab()
                .attachments
                .images()
                .next()
                .unwrap()
                .data_base64
                .as_ptr(),
            payload
        );
        assert!(app.current_tab().prompt_queue.entries.is_empty());
        app.enqueue_input(prefix.starts_with("/fix").then(|| "inspect after".into()));
        let fresh = &app.current_tab().prompt_queue.entries[0];
        assert_ne!(fresh.submission.id, old_id);
        assert!(!fresh.submission.cancellation_token().is_cancelled());
        assert_eq!(fresh.capturing, prefix.starts_with("/fix"));
        assert!(rx.try_recv().is_err());
    }
}

#[test]
fn recall_skips_automatic_work_and_unblocked_preparation() {
    let _locale = crate::test_support::lock_locale();
    let (mut app, mut rx) = connected_app();
    enter(&mut app, "/fix preparing");
    assert!(!app.pending_queue_can_recall());
    app.recall_last_pending_input();
    assert!(app.current_tab().input.is_empty());
    enter(&mut app, "last explicit");
    app.enqueue_autofix("queue-tab", "source", "automatic", false);
    app.recall_last_pending_input();
    assert_eq!(app.current_tab().input, "last explicit");
    assert_eq!(app.current_tab().prompt_queue.entries.len(), 2);
    assert!(rx.try_recv().is_err());
}

#[test]
fn stopping_preserves_captured_evidence_but_expires_unfinished_capture() {
    let _locale = crate::test_support::lock_locale();
    for captured in [false, true] {
        let (mut app, mut rx) = connected_app();
        app.current_tab_mut().config_pending_id = Some("hold".into());
        app.enqueue_autofix("queue-tab", "source", "explicit diagnostic", true);
        let id = app.current_tab().prompt_queue.entries[0].submission.id;
        let token = app.current_tab().prompt_queue.entries[0]
            .submission
            .cancellation_token();
        if captured {
            complete_autofix_capture(&mut app, "queue-tab");
        }
        app.enqueue_autofix("queue-tab", "other-source", "automatic", false);
        let automatic = app.current_tab().prompt_queue.entries[1]
            .submission
            .cancellation_token();
        app.current_tab_mut().pause_pending_prompts();
        assert!(automatic.is_cancelled());
        assert_eq!(token.is_cancelled(), !captured);
        assert_eq!(app.current_tab().prompt_queue.entries.len(), 1);
        assert_eq!(
            app.current_tab().prompt_queue.entries[0].needs_resubmission,
            !captured
        );
        if !captured {
            app.autofix_snapshot_ready(
                id,
                Ok(crate::protocol::acp::client::AutofixSnapshot::for_test(
                    "source",
                )),
            );
            assert!(app.current_tab().prompt_queue.entries[0]
                .submission
                .autofix_snapshot
                .is_none());
        }
        app.current_tab_mut().config_pending_id = None;
        assert_eq!(app.pending_queue_can_resume(), captured);
        app.resume_pending_inputs();
        if captured {
            let sent = rx.try_recv().unwrap();
            assert_eq!(sent.id, id);
            assert!(sent.autofix_snapshot.is_some());
        } else {
            assert!(rx.try_recv().is_err());
            app.recall_last_pending_input();
            assert_eq!(app.current_tab().input, "/fix explicit diagnostic");
            assert!(!app.pending_queue_paused());
        }
    }
}

#[test]
fn paused_automatic_failures_show_diagnostics_without_queuing_work() {
    let _locale = crate::test_support::lock_locale();
    let (mut app, mut rx) = connected_app();
    app.autofix_enabled = true;
    app.current_tab_mut().config_pending_id = Some("hold".into());
    enter(&mut app, "pending explicit");
    app.current_tab_mut().pause_pending_prompts();
    shell_event(&mut app, "osc:133;D;1");
    assert_eq!(app.current_tab().prompt_queue.entries.len(), 1);
    assert!(matches!(
        &app.current_tab().autofix.bar_snapshot,
        AutofixBarSnapshot::Detected { pane_id, .. } if pane_id == "source"
    ));
    assert!(rx.try_recv().is_err());
}

#[test]
fn source_close_expires_completed_manual_evidence_without_falling_back_to_focus() {
    let _locale = crate::test_support::lock_locale();
    let (mut app, mut rx) = connected_app();
    app.current_tab_mut().config_pending_id = Some("hold".into());
    app.enqueue_autofix("queue-tab", "source", "explicit diagnostic", true);
    complete_autofix_capture(&mut app, "queue-tab");
    shell_event(&mut app, "osc:133;C");
    assert!(!app.current_tab().prompt_queue.entries[0].needs_resubmission);
    app.handle_autofix_pane_closed(Some("queue-tab"), "source");
    app.source_session_id = Some("unrelated-focus".into());
    app.current_tab_mut().config_pending_id = None;
    assert!(app.current_tab().prompt_queue.entries[0].needs_resubmission);
    assert!(!app.pending_queue_can_resume());
    app.resume_pending_inputs();
    assert!(rx.try_recv().is_err());
}

#[test]
fn resume_obeys_configuration_interaction_and_connection_barriers() {
    let _locale = crate::test_support::lock_locale();
    let (mut app, mut rx) = connected_app();
    app.current_tab_mut().config_pending_id = Some("hold".into());
    enter(&mut app, "pending");
    app.current_tab_mut().pause_pending_prompts();
    assert!(!app.pending_queue_can_resume());
    app.current_tab_mut().config_pending_id = None;
    app.current_tab_mut().pending_queue_action = Some(42);
    assert!(!app.pending_queue_can_resume());
    app.current_tab_mut().pending_queue_action = None;
    app.current_tab_mut().loading_session = true;
    assert!(!app.pending_queue_can_resume());
    app.current_tab_mut().loading_session = false;
    app.state = ConnectionState::Disconnected;
    assert!(!app.pending_queue_can_resume());
    app.state = ConnectionState::Connected;
    assert!(app.pending_queue_can_resume());
    app.resume_pending_inputs();
    assert_eq!(rx.try_recv().unwrap().text, "pending");
    assert!(!app.pending_queue_paused());
}

#[test]
fn transport_recovery_retains_requests_but_requires_user_resume() {
    let _locale = crate::test_support::lock_locale();
    let (mut app, mut rx) = connected_app();
    app.current_tab_mut().config_pending_id = Some("hold".into());
    enter(&mut app, "retained across reconnect");
    app.handle_event(AppEvent::AgentClientFailed);
    assert!(app.pending_queue_paused());
    app.state = ConnectionState::Failed("transport failed".into());
    app.cmd_restart();
    assert!(matches!(app.state, ConnectionState::Connecting(_)));
    assert_eq!(app.current_tab().prompt_queue.entries.len(), 1);
    app.state = ConnectionState::Connected;
    app.current_tab_mut().config_pending_id = None;
    app.dispatch_prompt_queues();
    assert!(rx.try_recv().is_err());
    app.resume_pending_inputs();
    assert_eq!(rx.try_recv().unwrap().text, "retained across reconnect");
}

#[test]
fn late_prompt_error_after_stop_and_rename_does_not_invalidate_fresh_capture() {
    let _locale = crate::test_support::lock_locale();
    let (mut app, mut rx) = connected_app();
    enter(&mut app, "active");
    let active = rx.try_recv().unwrap();
    app.request_turn_cancel_for_tab("queue-tab");
    enter(&mut app, "/fix fresh investigation");
    let id = app.current_tab().prompt_queue.entries[0].submission.id;
    let token = app.current_tab().prompt_queue.entries[0]
        .submission
        .cancellation_token();
    app.rename_tab_session("queue-tab", "renamed", Some("new-window"));
    app.handle_event(AppEvent::PromptError {
        tab_id: "queue-tab".into(),
        prompt_id: active.id,
        message: "late cancellation error".into(),
    });
    assert!(app.current_tab().turn.is_idle());
    assert!(!token.is_cancelled());
    assert!(app.current_tab().prompt_queue.entries[0].capturing);
    assert!(!app.pending_queue_paused());
    app.handle_event(AppEvent::AutofixSnapshotReady {
        request_id: id,
        result: Ok(crate::protocol::acp::client::AutofixSnapshot::for_test(
            "source",
        )),
    });
    assert_eq!(rx.try_recv().unwrap().id, id);
}

#[test]
fn ordinary_protocol_failure_retains_explicit_input_and_drops_automatic_capture() {
    let _locale = crate::test_support::lock_locale();
    let (mut app, mut rx) = connected_app();
    enter(&mut app, "active");
    rx.try_recv().unwrap();
    enter(&mut app, "unsent user request");
    app.enqueue_autofix("queue-tab", "source", "automatic", false);
    let automatic = app.current_tab().prompt_queue.entries[1]
        .submission
        .cancellation_token();
    app.handle_event(AppEvent::AgentError {
        session_id: Some("queue-session".into()),
        failure: crate::protocol::acp::failure::AgentFailure::Protocol {
            code: -32603,
            message: "ordinary failure".into(),
        },
        message: "ordinary failure".into(),
    });
    assert!(automatic.is_cancelled());
    assert_eq!(app.state, ConnectionState::Connected);
    assert!(app.pending_queue_paused());
    assert_eq!(app.current_tab().prompt_queue.entries.len(), 1);
    assert!(rx.try_recv().is_err());
    app.resume_pending_inputs();
    assert_eq!(rx.try_recv().unwrap().text, "unsent user request");
}

#[test]
fn stopping_only_automatic_work_does_not_pause_future_explicit_input() {
    let _locale = crate::test_support::lock_locale();
    let (mut app, mut rx) = connected_app();
    app.enqueue_autofix("queue-tab", "source", "automatic", false);
    app.current_tab_mut().pause_pending_prompts();
    assert!(!app.pending_queue_paused());
    assert!(app.current_tab().prompt_queue.entries.is_empty());
    enter(&mut app, "fresh explicit request");
    assert_eq!(rx.try_recv().unwrap().text, "fresh explicit request");
}

#[test]
fn external_agent_and_model_rebinds_do_not_carry_paused_requests_to_another_conversation() {
    let _locale = crate::test_support::lock_locale();
    for agent_changed in [false, true] {
        let (mut app, mut rx) = connected_app();
        app.current_tab_mut().config_pending_id = Some("hold".into());
        enter(&mut app, "belongs to old conversation");
        let token = app.current_tab().prompt_queue.entries[0]
            .submission
            .cancellation_token();
        app.current_tab_mut().pause_pending_prompts();
        assert!(app.queue_blocks_session_change());
        if agent_changed {
            app.reset_agent_scoped_state();
        } else {
            app.follows_global_acp_model = true;
            app.current_agent_id = "copilot".into();
            assert!(app.apply_global_acp_model("copilot", Some("replacement-model".into())));
        }
        assert!(token.is_cancelled());
        assert!(app.current_tab().prompt_queue.entries.is_empty());
        assert!(!app.pending_queue_paused());
        app.state = ConnectionState::Connected;
        app.current_tab_mut().config_pending_id = None;
        app.dispatch_prompt_queues();
        assert!(rx.try_recv().is_err());
    }
}

#[test]
fn late_soft_stop_preserves_post_stop_input_until_cancellation_settles() {
    let _locale = crate::test_support::lock_locale();
    for reason in [
        SoftStopReason::MaxTokens,
        SoftStopReason::MaxTurnRequests,
        SoftStopReason::Refusal,
    ] {
        let (mut app, mut rx) = connected_app();
        enter(&mut app, "active");
        let active = rx.try_recv().unwrap();
        app.handle_event(AppEvent::AgentMessageChunk {
            session_id: "queue-session".into(),
            text: "partial answer".into(),
        });
        enter(&mut app, "retain on stop");
        enter(&mut app, "/stop");
        assert!(active.cancellation_token().is_cancelled());
        assert!(app.current_tab().turn.is_cancelling());
        enter(&mut app, "fresh after stop");
        let pending = app.current_tab().prompt_queue.entries[0]
            .submission
            .cancellation_token();
        let messages_before = app.current_tab().messages.len();
        app.handle_event(AppEvent::AgentSoftStop {
            session_id: "queue-session".into(),
            reason,
        });
        assert!(!pending.is_cancelled());
        assert_eq!(app.current_tab().messages.len(), messages_before);
        assert_eq!(app.current_tab().prompt_queue.entries.len(), 2);
        assert!(app.current_tab().turn.is_cancelling());
        assert!(rx.try_recv().is_err());
        app.handle_event(AppEvent::AgentMessageEnd {
            session_id: "queue-session".into(),
        });
        assert!(rx.try_recv().is_err());
        app.resume_pending_inputs();
        assert_eq!(rx.try_recv().unwrap().text, "retain on stop");
        app.handle_event(AppEvent::AgentMessageEnd {
            session_id: "queue-session".into(),
        });
        assert_eq!(rx.try_recv().unwrap().text, "fresh after stop");
        assert!(rx.try_recv().is_err());
    }
}

#[test]
fn uncancelled_soft_stop_pauses_queued_requests_without_disconnecting() {
    let _locale = crate::test_support::lock_locale();
    let (mut app, mut rx) = connected_app();
    enter(&mut app, "active");
    rx.try_recv().unwrap();
    enter(&mut app, "dependent followup");
    let pending = app.current_tab().prompt_queue.entries[0]
        .submission
        .cancellation_token();
    app.handle_event(AppEvent::AgentSoftStop {
        session_id: "queue-session".into(),
        reason: SoftStopReason::MaxTokens,
    });
    assert!(!pending.is_cancelled());
    assert!(app.pending_queue_paused());
    assert_eq!(app.current_tab().prompt_queue.entries.len(), 1);
    assert_eq!(app.state, ConnectionState::Connected);
    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: "queue-session".into(),
    });
    assert!(rx.try_recv().is_err());
    enter(&mut app, "independent retry");
    assert!(rx.try_recv().is_err());
    app.resume_pending_inputs();
    assert_eq!(rx.try_recv().unwrap().text, "dependent followup");
    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: "queue-session".into(),
    });
    assert_eq!(rx.try_recv().unwrap().text, "independent retry");
}

#[test]
fn prompt_redraw_preserves_queued_automatic_and_explicit_captures() {
    let _locale = crate::test_support::lock_locale();
    for explicit in [false, true] {
        let (mut app, mut rx) = connected_app();
        app.autofix_enabled = !explicit;
        enter(&mut app, "active");
        rx.try_recv().unwrap();
        shell_event(&mut app, "osc:133;D;1");
        if explicit {
            app.handle_autofix_execute_from_detected("source", Some("queue-tab"));
        }
        enter(&mut app, "another user request");
        let captures: Vec<_> = app
            .current_tab()
            .prompt_queue
            .entries
            .iter()
            .map(|item| (item.submission.id, item.submission.cancellation_token()))
            .collect();
        assert_eq!(captures.len(), 2);
        for sequence in ["osc:133;A", "osc:133;B", "osc:133;A", "osc:133;B"] {
            shell_event(&mut app, sequence);
        }
        assert_eq!(
            app.current_tab()
                .prompt_queue
                .entries
                .iter()
                .map(|item| item.submission.id)
                .collect::<Vec<_>>(),
            captures.iter().map(|(id, _)| *id).collect::<Vec<_>>()
        );
        assert!(captures.iter().all(|(_, token)| !token.is_cancelled()));
        complete_autofix_capture(&mut app, "queue-tab");
        assert!(rx.try_recv().is_err());
        app.handle_event(AppEvent::AgentMessageEnd {
            session_id: "queue-session".into(),
        });
        let next = rx.try_recv().unwrap();
        assert_eq!(next.is_autofix(), explicit);
    }
}

#[test]
fn command_start_still_invalidates_waiting_automatic_evidence() {
    let _locale = crate::test_support::lock_locale();
    let (mut app, mut rx) = connected_app();
    app.autofix_enabled = true;
    enter(&mut app, "active");
    rx.try_recv().unwrap();
    shell_event(&mut app, "osc:133;D;1");
    let pending = app.current_tab().prompt_queue.entries[0]
        .submission
        .cancellation_token();
    shell_event(&mut app, "osc:133;A");
    shell_event(&mut app, "osc:133;B");
    assert!(!pending.is_cancelled());
    shell_event(&mut app, "osc:133;C");
    assert!(pending.is_cancelled());
    assert!(app.current_tab().prompt_queue.entries.is_empty());
}

#[test]
fn typed_fix_binds_its_discovered_source_and_rejects_activity_during_resolution() {
    let _locale = crate::test_support::lock_locale();
    for source_changes in [false, true] {
        let (mut app, mut rx) = connected_app();
        app.current_tab_mut().config_pending_id = Some("hold".into());
        enter(&mut app, "/fix explain");
        let item = &app.current_tab().prompt_queue.entries[0];
        let request_id = item.submission.id;
        let token = item.submission.cancellation_token();
        assert!(item
            .submission
            .pane_context
            .as_ref()
            .unwrap()
            .source_pane_id
            .is_none());
        if source_changes {
            shell_event(&mut app, "osc:133;C");
            assert!(token.is_cancelled());
        }
        app.handle_event(AppEvent::AutofixSnapshotReady {
            request_id,
            result: Ok(crate::protocol::acp::client::AutofixSnapshot::for_test(
                "source",
            )),
        });
        app.current_tab_mut().config_pending_id = None;
        app.dispatch_prompt_queues();
        if source_changes {
            assert_eq!(app.current_tab().prompt_queue.entries.len(), 1);
            assert!(app.current_tab().prompt_queue.entries[0].needs_resubmission);
            assert!(rx.try_recv().is_err());
        } else {
            let prompt = rx.try_recv().unwrap();
            assert_eq!(
                prompt.pane_context.unwrap().source_pane_id.as_deref(),
                Some("source")
            );
            assert_eq!(
                app.current_tab()
                    .turn
                    .prompt()
                    .unwrap()
                    .context
                    .target_pane_id(),
                Some("source")
            );
        }
    }
}

#[test]
fn new_diagnostic_survives_the_previous_results_prompt_dismissal() {
    let _locale = crate::test_support::lock_locale();
    for pane_open in [false, true] {
        let (mut app, mut rx) = connected_app();
        app.autofix_enabled = false;
        app.current_tab_mut().pane_open = pane_open;
        shell_event(&mut app, "osc:133;D;1");
        app.handle_autofix_execute_from_detected("source", Some("queue-tab"));
        complete_autofix_capture(&mut app, "queue-tab");
        rx.try_recv().unwrap();
        app.handle_event(AppEvent::AgentMessageChunk {
            session_id: "queue-session".into(),
            text: "The command failed because its argument is invalid.".into(),
        });
        app.handle_event(AppEvent::AgentMessageEnd {
            session_id: "queue-session".into(),
        });
        assert_eq!(
            app.current_tab().autofix.suggested_pane_id.as_deref(),
            Some("source")
        );
        // A failure can be reported as D/A/B without a preceding C.
        shell_event(&mut app, "osc:133;D;1");
        shell_event(&mut app, "osc:133;A");
        shell_event(&mut app, "osc:133;B");
        assert!(matches!(
            &app.current_tab().autofix.bar_snapshot,
            AutofixBarSnapshot::Detected { pane_id, .. } if pane_id == "source"
        ));
        assert!(app.current_tab().autofix.suggested_pane_id.is_none());
        assert!(app.current_tab().prompt_queue.entries.is_empty());
        app.handle_autofix_execute_from_detected("source", Some("queue-tab"));
        complete_autofix_capture(&mut app, "queue-tab");
        assert!(rx.try_recv().unwrap().is_autofix());
        assert!(rx.try_recv().is_err());
    }
}
