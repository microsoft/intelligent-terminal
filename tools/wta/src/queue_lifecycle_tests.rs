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
        enter(&mut app, "discard on stop");
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
        assert_eq!(app.current_tab().prompt_queue.entries.len(), 1);
        assert!(app.current_tab().turn.is_cancelling());
        assert!(rx.try_recv().is_err());
        app.handle_event(AppEvent::AgentMessageEnd {
            session_id: "queue-session".into(),
        });
        assert_eq!(rx.try_recv().unwrap().text, "fresh after stop");
        assert!(rx.try_recv().is_err());
    }
}

#[test]
fn uncancelled_soft_stop_discards_queued_requests_without_disconnecting() {
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
    assert!(pending.is_cancelled());
    assert!(app.current_tab().prompt_queue.entries.is_empty());
    assert_eq!(app.state, ConnectionState::Connected);
    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: "queue-session".into(),
    });
    assert!(rx.try_recv().is_err());
    enter(&mut app, "independent retry");
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
            assert!(app.current_tab().prompt_queue.entries.is_empty());
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
