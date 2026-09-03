//! Per-tab pending-prompt queue transitions.
//!
//! Prompts are owned by [`TabSession`] rather than the ACP transport. This
//! keeps input, cancellation, reset, and rendering scoped to the Terminal tab
//! that accepted the prompt, while `PromptSubmission::pane_context.tab_id`
//! makes the helper/master transport route the eventual dispatch correctly.

use super::tab_state::{QueuedPrompt, PENDING_PROMPT_QUEUE_CAP};
use super::*;

const QUEUE_HINT_DURATION: std::time::Duration = std::time::Duration::from_millis(2500);

impl App {
    /// Move the active tab's draft into its pending queue. Returns false when
    /// the queue is full, leaving the complete draft (including attachments)
    /// available for the user to edit or submit later.
    pub(super) fn enqueue_current_prompt(&mut self) -> bool {
        if self.current_tab().pending_prompts.len() >= PENDING_PROMPT_QUEUE_CAP {
            let now = std::time::Instant::now();
            self.transient_hint = Some((
                t!("input.queue.full", cap = PENDING_PROMPT_QUEUE_CAP).into_owned(),
                now + QUEUE_HINT_DURATION,
            ));
            return false;
        }

        let (text, display_text, images) = {
            let tab = self.current_tab_mut();
            let display_text = std::mem::take(&mut tab.input);
            let (text, images) = tab.attachments.take_for_submission(display_text.clone());
            tab.record_input_history(&text);
            tab.cursor_pos = 0;
            tab.refresh_command_popup();
            (text, display_text, images)
        };
        self.current_tab_mut()
            .pending_prompts
            .push_back(QueuedPrompt::new(text, display_text, images));

        let resume = {
            let tab = self.current_tab();
            tab.queue_paused && tab.accepts_queued_dispatch()
        };
        if resume {
            self.current_tab_mut().queue_paused = false;
            let tab_id = self.active_tab_key().to_string();
            self.dispatch_next_queued_prompt(&tab_id);
        }

        if self.show_welcome_hint {
            self.show_welcome_hint = false;
            set_welcome_shown_in_state();
        }
        true
    }

    /// Remove the newest queued prompt from the active tab. Queue dispatch is
    /// FIFO, while Esc behaves as an undo stack for the user's most recent
    /// enqueue action.
    pub(super) fn undo_latest_queued_prompt(&mut self) -> bool {
        let Some(queued) = self.current_tab_mut().pop_latest_pending_prompt() else {
            return false;
        };
        let now = std::time::Instant::now();
        self.transient_hint = Some((
            t!("input.queue.removed", preview = queued.collapsed_text()).into_owned(),
            now + QUEUE_HINT_DURATION,
        ));
        true
    }

    /// Drop pending user work for the active tab and return how many prompts
    /// were discarded. Ctrl+C and `/stop` use this intentionally stronger
    /// semantic than Esc, which only undoes one queued prompt at a time.
    pub(super) fn discard_current_pending_prompts(&mut self) -> usize {
        self.current_tab_mut().clear_pending_prompts()
    }

    /// Dispatch exactly one queued prompt for this helper's owner tab.
    ///
    /// Helpers are per-tab in the current architecture. Never scan every
    /// `TabSession`: doing so revives the retired multi-tab helper routing and
    /// lets unrelated state transitions start work in another helper's tab.
    fn dispatch_next_queued_prompt(&mut self, tab_id: &str) -> bool {
        if self.state != ConnectionState::Connected {
            return false;
        }
        if let Some(owner) = self.owner_tab_id.as_deref() {
            if owner != tab_id {
                return false;
            }
        } else if self.active_tab_key() != tab_id {
            return false;
        }

        let queued = {
            let Some(tab) = self.tab_sessions.get_mut(tab_id) else {
                return false;
            };
            if tab.queue_paused || !tab.accepts_queued_dispatch() {
                return false;
            }
            let Some(queued) = tab.pending_prompts.pop_front() else {
                return false;
            };
            queued
        };
        let (text, display_text, images) = queued.into_parts();
        // Provider routing and command classification are resolved at
        // dispatch, not at enqueue: the session's advertised commands, model
        // and agent can all change while the prompt waits. Classify from
        // `display_text` for parity with the direct Enter path, which reads
        // the raw input before attachment tokens are expanded.
        let is_agent_command = self.agent_command_for_input(&display_text).is_some();
        let is_byok = self.current_model_is_byok();
        let agent_id = self.current_agent_id.clone();
        let prompt = {
            let pane_context = PaneContext {
                pane_id: self.pane_id.clone(),
                tab_id: Some(tab_id.to_string()),
                window_id: self.window_id.clone(),
                cwd: self.source_cwd.clone(),
                source_pane_id: self.source_session_id.clone(),
            };
            if is_agent_command {
                PromptSubmission::new_agent_command(text, Some(pane_context))
            } else {
                PromptSubmission::new(text, Some(pane_context))
            }
            .with_images(images)
            .with_byok(is_byok)
            .with_agent_id(agent_id)
        };
        prompt_timing_log(
            prompt.id,
            prompt.submitted_at_unix_s,
            "queue_dispatch",
            &format!("preview={:?}", prompt.preview()),
        );
        let submitted = SubmittedPrompt {
            id: prompt.id,
            text: display_text,
            submitted_at_unix_s: prompt.submitted_at_unix_s,
            context: TurnContext::default(),
            autofix: None,
        };
        let queued = QueuedPrompt::new(
            prompt.text.clone(),
            submitted.text.clone(),
            prompt.images.clone(),
        );
        self.tab_mut(tab_id).queued_dispatch = Some(super::tab_state::QueuedDispatch {
            prompt_id: prompt.id,
            prompt: queued,
        });
        self.turn_submit_prompt_for_tab(tab_id, submitted);
        let _ = self.prompt_tx.send(prompt);
        true
    }

    /// A completed, successful agent turn is the only automatic queue-drain
    /// point. It runs after terminal metadata has been applied and only for
    /// the helper's owning tab.
    pub(super) fn dispatch_after_successful_turn(&mut self, session_id: &str, prompt_id: u64) {
        let tab_id = self.tab_for_session(session_id);
        self.clear_dispatched_prompt(&tab_id, prompt_id);
        self.dispatch_next_queued_prompt(&tab_id);
    }

    /// A recommendation action is an explicit successful resolution of the
    /// card that blocked FIFO progression. Only after `turn_execute_card`
    /// has transitioned the card to its terminal outcome may Q2 begin.
    pub(super) fn dispatch_after_recommendation_execution(&mut self, tab_id: &str) {
        self.dispatch_next_queued_prompt(tab_id);
    }

    /// Drop the tab's dispatched-prompt rollback copy when it exactly
    /// matches `prompt_id`. A stale or mismatched id must never evict a
    /// different prompt's copy — that would let a late terminal event
    /// resurrect abandoned work or erase a live successor's own copy.
    pub(super) fn clear_dispatched_prompt(&mut self, tab_id: &str, prompt_id: u64) {
        if self
            .tab_sessions
            .get(tab_id)
            .and_then(|tab| tab.queued_dispatch.as_ref())
            .is_some_and(|dispatch| dispatch.prompt_id == prompt_id)
        {
            self.tab_mut(tab_id).queued_dispatch = None;
        }
    }

    /// Apply one coordinator completion to its exact in-flight recommendation.
    ///
    /// A tab drag can rekey the contextual tab ID while the coordinator runs,
    /// so only the immutable prompt/execution pair may release the gate.
    pub(super) fn complete_recommendation_execution(
        &mut self,
        execution: crate::coordinator::RecommendationExecutionIdentity,
        _choice: usize,
        result: Result<(), String>,
    ) -> bool {
        let target_tab = self.tab_sessions.iter().find_map(|(tab_id, tab)| {
            (tab.pending_execution == Some(execution)).then(|| tab_id.clone())
        });
        let Some(target_tab) = target_tab else {
            return false;
        };
        self.tab_mut(&target_tab).pending_execution = None;

        match result {
            Ok(()) => self.dispatch_after_recommendation_execution(&target_tab),
            Err(_error) => {
                // A recommendation action never resumes ACP work on its own
                // authority: pause the queue but never restore the consumed
                // prompt onto it, or a failed action would resend a turn the
                // agent already executed.
                //
                // The coordinator already surfaced this failure as an
                // `AppEvent::SystemMessage`, matching the pre-queue
                // behavior; this completion path only needs to release the
                // gate and stop FIFO progression.
                self.pause_queue(&target_tab);
            }
        }
        true
    }

    /// Pause the tab's queue without disturbing whatever is already
    /// dispatched. Used when a failure carries no specific prompt to roll
    /// back, or must never resurrect one. The next typed Enter is an
    /// explicit user decision to retry in FIFO order.
    pub(super) fn pause_queue(&mut self, tab_id: &str) {
        let tab = self.tab_mut(tab_id);
        if !tab.pending_prompts.is_empty() {
            tab.queue_paused = true;
        }
    }

    /// Roll back a queued prompt that was handed to the ACP transport but
    /// never accepted. Restores it to the head of the queue and pauses FIFO
    /// progression; a mismatched `prompt_id` is a no-op so a stale event can
    /// never resurrect abandoned or unrelated work.
    pub(super) fn restore_dispatched_prompt(&mut self, tab_id: &str, prompt_id: u64) -> bool {
        let tab = self.tab_mut(tab_id);
        let restored = tab
            .queued_dispatch
            .take_if(|dispatch| dispatch.prompt_id == prompt_id);
        let matched = restored.is_some();
        if let Some(dispatch) = restored {
            tab.pending_prompts.push_front(dispatch.prompt);
        }
        if !tab.pending_prompts.is_empty() {
            tab.queue_paused = true;
        }
        matched
    }

    /// Roll back an ACP-side busy rejection. The rejected queued prompt was
    /// never accepted by the agent, so restore it at the front and clear the
    /// optimistic local Submitted state without draining again.
    pub(super) fn rollback_queued_dispatch(&mut self, tab_id: &str, prompt_id: u64) -> bool {
        if !self.restore_dispatched_prompt(tab_id, prompt_id) {
            return false;
        }
        let tab = self.tab_mut(tab_id);
        if tab
            .turn
            .prompt()
            .is_some_and(|prompt| prompt.id == prompt_id)
        {
            tab.turn = TurnState::Idle;
            tab.messages.clear();
            tab.clear_streaming_thought();
            tab.permission.clear();
            tab.reveal_chars = 0;
            tab.activity_frame = 0;
            tab.timing_note = None;
        }
        true
    }

    /// Cancel the active tab's in-flight head. The ACP cancellation is
    /// only possible once the tab has an ACP session id, but the local turn
    /// state always returns to Idle so the queue gate is deterministic.
    pub(super) fn cancel_active_in_flight_turn(&mut self) -> bool {
        if !self.current_tab().turn.is_in_flight() {
            return false;
        }
        let session_id = self.current_tab().session_id.clone();
        // A lazily created session may not have answered yet.
        if let Some(session_id) = session_id.clone() {
            let _ = self.cancel_tx.send(CancelRequest { session_id });
        }
        self.turn_cancel(session_id.as_deref().unwrap_or(DEFAULT_TAB_ID));
        let tab = self.current_tab_mut();
        tab.messages
            .push(ChatMessage::success(t!("system.cancelled").into_owned()));
        tab.scroll_to_bottom();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{test_app_with_prompt_and_cancel_rx, test_app_with_prompt_rx};
    use crate::clipboard_image::PastedImage;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn connected_app() -> (
        App,
        tokio::sync::mpsc::UnboundedReceiver<crate::protocol::acp::client::PromptSubmission>,
    ) {
        let (mut app, prompt_rx) = test_app_with_prompt_rx();
        app.state = ConnectionState::Connected;
        (app, prompt_rx)
    }

    fn enter(app: &mut App, text: &str) {
        app.current_tab_mut().insert_input_str(text);
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    }

    fn submitted(id: u64, text: &str) -> SubmittedPrompt {
        SubmittedPrompt {
            id,
            text: text.to_string(),
            submitted_at_unix_s: 0.0,
            context: TurnContext::default(),
            autofix: None,
        }
    }

    /// Put a rollback copy on the tab for `prompt_id`, as every dispatch
    /// does, so a reset path can be observed clearing it.
    fn stub_queued_dispatch(app: &mut App, prompt_id: u64, text: &str) {
        app.current_tab_mut().queued_dispatch = Some(super::tab_state::QueuedDispatch {
            prompt_id,
            prompt: QueuedPrompt::new(text.into(), text.into(), vec![]),
        });
    }

    fn execution(
        prompt_id: u64,
        execution_id: u64,
    ) -> crate::coordinator::RecommendationExecutionIdentity {
        crate::coordinator::RecommendationExecutionIdentity {
            prompt_id: Some(prompt_id),
            execution_id,
        }
    }

    /// Put `tab_id` in the state `turn_execute_card` leaves behind: the ACP
    /// turn has surfaced, but the coordinator action is still outstanding.
    fn gate_on_execution(
        app: &mut App,
        tab_id: &str,
        prompt_id: u64,
        execution_id: u64,
    ) -> crate::coordinator::RecommendationExecutionIdentity {
        let identity = execution(prompt_id, execution_id);
        let tab = app.tab_mut(tab_id);
        tab.turn = TurnState::Surfaced {
            prompt: submitted(prompt_id, "Q1"),
            outcome: TurnOutcome::Empty,
            end_pending: false,
        };
        tab.pending_execution = Some(identity);
        identity
    }

    #[test]
    fn busy_enter_queues_and_completion_drains_fifo() {
        let (mut app, mut prompt_rx) = connected_app();
        enter(&mut app, "first");
        assert_eq!(prompt_rx.try_recv().unwrap().text, "first");

        enter(&mut app, "second");
        enter(&mut app, "third");
        let tab = app.current_tab();
        assert_eq!(tab.pending_prompts.len(), 2);
        assert_eq!(
            tab.messages
                .iter()
                .filter(|message| matches!(message, ChatMessage::User(_)))
                .count(),
            1,
            "queued work must not render a premature user bubble"
        );

        app.handle_event(AppEvent::AgentTurnCompleted {
            session_id: DEFAULT_TAB_ID.to_string(),
            prompt_id: app.current_tab().turn.prompt().unwrap().id,
            soft_stop: None,
        });
        let second = prompt_rx
            .try_recv()
            .expect("first queued prompt dispatched");
        assert_eq!(second.text, "second");
        assert_eq!(
            second
                .pane_context
                .as_ref()
                .and_then(|context| context.tab_id.as_deref()),
            Some(DEFAULT_TAB_ID)
        );
        assert_eq!(app.current_tab().pending_prompts.len(), 1);

        app.handle_event(AppEvent::AgentTurnCompleted {
            session_id: DEFAULT_TAB_ID.to_string(),
            prompt_id: app.current_tab().turn.prompt().unwrap().id,
            soft_stop: None,
        });
        assert_eq!(
            prompt_rx
                .try_recv()
                .expect("second queued prompt dispatched")
                .text,
            "third"
        );
    }

    #[test]
    fn session_load_permission_and_card_hold_queue_until_ready() {
        let (mut app, mut prompt_rx) = connected_app();
        app.current_tab_mut().loading_session = true;
        enter(&mut app, "after load");
        assert_eq!(app.current_tab().pending_prompts.len(), 1);
        assert!(prompt_rx.try_recv().is_err());

        {
            let tab = app.current_tab_mut();
            tab.loading_session = false;
            tab.permission.push_back(PermissionState {
                tool_call_id: "tool-1".into(),
                description: "permission".into(),
                title: "permission".into(),
                kind_label: None,
                target: None,
                target_is_command: false,
                options: vec![],
                selected: 0,
                responder: None,
            });
        }
        assert!(prompt_rx.try_recv().is_err());

        {
            let tab = app.current_tab_mut();
            tab.permission.clear();
            tab.turn = TurnState::Surfaced {
                prompt: submitted(1, "card"),
                outcome: TurnOutcome::Recommendation(crate::coordinator::RecommendationSet {
                    recommended_choice: None,
                    choices: vec![],
                }),
                end_pending: false,
            };
        }
        assert!(prompt_rx.try_recv().is_err());
        assert_eq!(app.current_tab().pending_prompts.len(), 1);

        app.current_tab_mut().turn = TurnState::Idle;
        app.current_tab_mut().queue_paused = true;
        enter(&mut app, "explicit resume");
        assert_eq!(
            prompt_rx
                .try_recv()
                .expect("queue drains after all gates release")
                .text,
            "after load"
        );
    }

    /// The first prompt on a pane with no session owns the lazy `session/new`,
    /// and the pane stays in flight for that whole round trip. A queued prompt
    /// must therefore wait for the head turn's normal completion gate — never
    /// dequeue mid-creation, which is what would make a second concurrent
    /// `session/new` (and an orphaned session) possible for one pane.
    #[test]
    fn queue_holds_while_the_first_prompt_creates_the_session() {
        let (mut app, mut prompt_rx) = connected_app();
        assert!(app.current_tab().session_id.is_none());

        enter(&mut app, "first");
        let head = prompt_rx
            .try_recv()
            .expect("the head prompt owns the lazy session/new");
        assert!(
            app.current_tab().session_id.is_none(),
            "session/new has not answered yet"
        );

        enter(&mut app, "second");
        assert_eq!(app.current_tab().pending_prompts.len(), 1);
        assert!(
            prompt_rx.try_recv().is_err(),
            "no queued prompt may dispatch while session/new is pending"
        );

        // The lazy session lands. Attachment binds the pane but is not a
        // drain point: the head turn still owns it.
        app.handle_event(AppEvent::SessionAttached {
            tab_id: DEFAULT_TAB_ID.to_string(),
            session_id: "lazy-session".into(),
            available_models: vec![],
            current_model_id: None,
        });
        assert_eq!(
            app.current_tab().session_id.as_deref(),
            Some("lazy-session")
        );
        assert_eq!(app.current_tab().pending_prompts.len(), 1);
        assert!(
            prompt_rx.try_recv().is_err(),
            "session attachment alone must not drain the queue"
        );

        app.handle_event(AppEvent::AgentTurnCompleted {
            session_id: "lazy-session".into(),
            prompt_id: head.id,
            soft_stop: None,
        });
        assert_eq!(
            prompt_rx
                .try_recv()
                .expect("the queue drains on the head turn's completion gate")
                .text,
            "second"
        );
    }

    #[test]
    fn esc_undoes_queued_prompts_lifo_then_is_a_no_op_on_the_head() {
        let (mut app, _prompt_rx) = connected_app();
        enter(&mut app, "head");
        enter(&mut app, "first queued");
        enter(&mut app, "second queued");

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.current_tab().pending_prompts.len(), 1);
        assert_eq!(
            app.current_tab()
                .pending_prompts
                .back()
                .unwrap()
                .collapsed_text(),
            "first queued"
        );
        assert!(app.current_tab().turn.is_in_flight());

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.current_tab().pending_prompts.is_empty());
        assert!(app.current_tab().turn.is_in_flight());

        // With the queue empty, Esc no longer cancels a bare in-flight head —
        // that generic cancellation was unrelated scope. Only Ctrl+C, /stop,
        // or an explicit card/autofix dismissal end an in-flight turn.
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.current_tab().turn.is_in_flight());
    }

    #[test]
    fn esc_unwinds_draft_queue_then_cancels_active_autofix_transport() {
        let (mut app, mut prompt_rx, mut cancel_rx) = test_app_with_prompt_and_cancel_rx();
        app.state = ConnectionState::Connected;
        app.current_tab_mut().session_id = Some("autofix-session".into());
        app.current_tab_mut().autofix.pane_id = Some("failing-pane".into());

        let mut autofix_prompt = submitted(7, "diagnose failure");
        autofix_prompt.context = TurnContext::with_target_pane("failing-pane");
        autofix_prompt.autofix = Some(AutofixContext { generation: 1 });
        app.turn_submit_prompt_for_tab(DEFAULT_TAB_ID, autofix_prompt);

        app.current_tab_mut().insert_input_str("draft");
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.current_tab().input.is_empty());
        assert!(app.current_tab().turn.is_in_flight());
        assert!(cancel_rx.try_recv().is_err());

        enter(&mut app, "queued");
        assert_eq!(app.current_tab().pending_prompts.len(), 1);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.current_tab().pending_prompts.is_empty());
        assert!(app.current_tab().turn.is_in_flight());
        assert!(cancel_rx.try_recv().is_err());

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.current_tab().turn.is_idle());
        assert_eq!(cancel_rx.try_recv().unwrap().session_id, "autofix-session");

        enter(&mut app, "after cancel");
        assert_eq!(prompt_rx.try_recv().unwrap().text, "after cancel");
    }

    /// Complete the tab's in-flight turn, which is the only automatic drain
    /// point.
    fn complete_turn(app: &mut App) {
        let prompt_id = app
            .current_tab()
            .turn
            .prompt()
            .expect("a turn must be in flight")
            .id;
        app.handle_event(AppEvent::AgentTurnCompleted {
            session_id: DEFAULT_TAB_ID.to_string(),
            prompt_id,
            soft_stop: None,
        });
    }

    /// A pause defers the queue that exists. Emptying that queue by hand must
    /// release it, or every later queue on the tab is silently dead: a direct
    /// submission never runs the resume path in `enqueue_current_prompt`, so
    /// nothing would ever clear the flag again.
    #[test]
    fn esc_undo_emptying_a_paused_queue_re_enables_a_later_drain() {
        let (mut app, mut prompt_rx) = connected_app();
        enter(&mut app, "A");
        assert_eq!(prompt_rx.try_recv().unwrap().text, "A");
        enter(&mut app, "B");
        // Any cancel or card dismissal pauses the queue rather than draining it.
        app.turn_cancel(DEFAULT_TAB_ID);
        assert!(app.current_tab().queue_paused);

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.current_tab().pending_prompts.is_empty());
        assert!(
            !app.current_tab().queue_paused,
            "an empty queue must never stay paused"
        );

        enter(&mut app, "C");
        assert_eq!(prompt_rx.try_recv().unwrap().text, "C");
        enter(&mut app, "D");
        complete_turn(&mut app);
        assert_eq!(
            prompt_rx
                .try_recv()
                .expect("a queue built after the pause was released must still drain")
                .text,
            "D"
        );
    }

    /// Ctrl+C and `/stop` empty the queue wholesale through the same helper,
    /// so they carry the same invariant as Esc's undo.
    #[test]
    fn discarding_a_paused_queue_re_enables_a_later_drain() {
        for discard_with_ctrl_c in [true, false] {
            let (mut app, mut prompt_rx) = connected_app();
            enter(&mut app, "A");
            assert_eq!(prompt_rx.try_recv().unwrap().text, "A");
            enter(&mut app, "B");
            app.turn_cancel(DEFAULT_TAB_ID);
            assert!(app.current_tab().queue_paused);

            if discard_with_ctrl_c {
                app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
            } else {
                app.current_tab_mut().replace_input("/stop".into());
                app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            }
            assert!(app.current_tab().pending_prompts.is_empty());
            assert!(
                !app.current_tab().queue_paused,
                "discarding the queue must release its pause (ctrl_c={discard_with_ctrl_c})"
            );

            enter(&mut app, "C");
            assert_eq!(prompt_rx.try_recv().unwrap().text, "C");
            enter(&mut app, "D");
            complete_turn(&mut app);
            assert_eq!(
                prompt_rx
                    .try_recv()
                    .expect("a later queue must still drain")
                    .text,
                "D",
                "case ctrl_c={discard_with_ctrl_c}"
            );
        }
    }

    /// An advertised agent command must stay an agent command through the
    /// queue. Re-sending it as an ordinary prompt would wrap `/usage` in the
    /// terminal planner template and the agent would never run it.
    #[test]
    fn queued_agent_command_is_dispatched_verbatim_not_templated() {
        let (mut app, mut prompt_rx) = connected_app();
        app.current_tab_mut().session_id = Some(DEFAULT_TAB_ID.to_string());
        app.session_commands.insert(
            DEFAULT_TAB_ID.to_string(),
            vec![crate::app_contracts::AcpSessionCommand {
                name: "usage".into(),
                description: "Show token usage".into(),
                input_hint: None,
                completion_behavior: crate::app_contracts::CompletionBehavior::ExecuteImmediately,
            }],
        );

        enter(&mut app, "first");
        let direct = prompt_rx.try_recv().expect("head dispatched");
        assert!(!direct.is_agent_command());

        enter(&mut app, "/usage");
        assert_eq!(app.current_tab().pending_prompts.len(), 1);

        complete_turn(&mut app);
        let queued = prompt_rx.try_recv().expect("queued command dispatched");
        assert_eq!(queued.text, "/usage");
        assert!(
            queued.is_agent_command(),
            "a queued advertised command must keep its agent-command classification"
        );
    }

    /// The queue holds ACP-bound input only. A client-side command is not
    /// conversation input, so it runs on the keystroke even while the pane is
    /// mid-turn with work already queued behind it.
    #[test]
    fn local_slash_commands_run_immediately_and_never_queue() {
        let (mut app, mut prompt_rx) = connected_app();
        enter(&mut app, "head");
        prompt_rx.try_recv().expect("head dispatched");
        enter(&mut app, "queued");
        assert_eq!(app.current_tab().pending_prompts.len(), 1);
        assert!(!app.help_overlay_visible);

        app.current_tab_mut().replace_input("/help".into());
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(
            app.help_overlay_visible,
            "a local command must execute while the pane is busy"
        );
        assert!(app.current_tab().input.is_empty());
        assert_eq!(
            app.current_tab().pending_prompts.len(),
            1,
            "a local command must not enter the ACP prompt queue"
        );
        assert!(
            prompt_rx.try_recv().is_err(),
            "a local command must never reach the ACP hop"
        );

        // The queue it stepped over is intact and still drains in order.
        complete_turn(&mut app);
        assert_eq!(
            prompt_rx
                .try_recv()
                .expect("the held queue still drains")
                .text,
            "queued"
        );
    }

    #[test]
    fn stop_and_ctrl_c_clear_all_queued_work() {
        let (mut app, _prompt_rx) = connected_app();
        enter(&mut app, "head");
        enter(&mut app, "queued");
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.current_tab().pending_prompts.is_empty());
        assert!(app.current_tab().turn.is_idle());

        enter(&mut app, "new head");
        enter(&mut app, "new queued");
        app.current_tab_mut().replace_input("/stop".into());
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.current_tab().pending_prompts.is_empty());
        assert!(app.current_tab().turn.is_idle());
    }

    /// Every reset path that abandons the active turn must drop its rollback
    /// copy too. `queued_dispatch` is the queue member already handed to the
    /// ACP transport; left behind, the next `restore_dispatched_prompt` pushes an
    /// abandoned prompt back onto the queue.
    #[test]
    fn reset_paths_drop_the_dispatched_prompt_copy() {
        // 1. turn_cancel — Esc on a card, a cancelled proposal, or Ctrl+C.
        let (mut app, _prompt_rx) = connected_app();
        app.current_tab_mut().turn = TurnState::Submitted(submitted(11, "Q1"));
        stub_queued_dispatch(&mut app, 11, "Q1");
        app.turn_cancel(DEFAULT_TAB_ID);
        assert!(app.current_tab().queued_dispatch.is_none());

        // A later connection-level failure carries no prompt id and takes
        // `queued_dispatch` unconditionally, so an uncleared copy would
        // resurrect the cancelled prompt here.
        app.handle_event(AppEvent::AgentError {
            session_id: None,
            prompt_id: None,
            failure: crate::protocol::acp::failure::AgentFailure::Protocol {
                code: -32603,
                message: "transport lost".into(),
            },
            message: "transport lost".into(),
        });
        assert!(
            app.current_tab().pending_prompts.is_empty(),
            "a cancelled prompt must never be pushed back onto the queue"
        );

        // 2. clear_pending_prompts — Ctrl+C and /stop with no in-flight head.
        let (mut app, _prompt_rx) = connected_app();
        stub_queued_dispatch(&mut app, 12, "Q1");
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.current_tab().queued_dispatch.is_none());

        // 3. clear_chat_history — /clear, /new, /restart, tab reset, rebind.
        let (mut app, _prompt_rx) = connected_app();
        stub_queued_dispatch(&mut app, 13, "Q1");
        app.current_tab_mut().clear_chat_history();
        assert!(app.current_tab().queued_dispatch.is_none());
        assert!(app.current_tab().pending_prompts.is_empty());
        assert!(!app.current_tab().queue_paused);
    }

    /// A tab-scoped load failure rejects the prompt without the agent ever
    /// accepting it, so it returns to the head of the queue and pauses —
    /// the same contract `AgentError` already honors.
    #[test]
    fn tab_error_restores_the_rejected_prompt_to_the_queue_head() {
        let (mut app, mut prompt_rx) = connected_app();
        app.current_tab_mut().turn = TurnState::Submitted(submitted(21, "rejected"));
        stub_queued_dispatch(&mut app, 21, "rejected");
        app.current_tab_mut()
            .pending_prompts
            .push_back(QueuedPrompt::new("later".into(), "later".into(), vec![]));

        app.handle_event(AppEvent::TabError {
            tab_id: DEFAULT_TAB_ID.to_string(),
            message: "load failed".into(),
        });

        let tab = app.current_tab();
        assert!(tab.queued_dispatch.is_none());
        assert!(tab.queue_paused);
        assert_eq!(
            tab.pending_prompts
                .iter()
                .map(|prompt| prompt.collapsed_text())
                .collect::<Vec<_>>(),
            vec!["rejected".to_string(), "later".to_string()],
            "the rejected prompt returns ahead of the work queued behind it"
        );
        assert!(prompt_rx.try_recv().is_err());
    }

    /// Enter with nothing to say is a no-op on both paths. Whitespace-only
    /// text carries no instruction, so it must not burn a queue slot while
    /// the pane is busy, nor reach the agent while it is idle.
    #[test]
    fn empty_and_whitespace_only_enter_never_queue_or_dispatch() {
        for text in ["", "   ", "\t", " \t  "] {
            // Idle pane: the direct path must not dispatch.
            let (mut app, mut prompt_rx) = connected_app();
            enter(&mut app, text);
            assert!(
                prompt_rx.try_recv().is_err(),
                "idle Enter on {text:?} must not reach the agent"
            );
            assert!(app.current_tab().turn.is_idle(), "case {text:?}");
            assert!(
                app.current_tab().pending_prompts.is_empty(),
                "case {text:?}"
            );

            // Busy pane: the queue path must not consume capacity.
            let (mut app, mut prompt_rx) = connected_app();
            enter(&mut app, "head");
            prompt_rx.try_recv().expect("head dispatched");
            enter(&mut app, text);
            assert!(
                app.current_tab().pending_prompts.is_empty(),
                "busy Enter on {text:?} must not consume queue capacity"
            );
            assert_eq!(
                app.current_tab().input,
                text,
                "a non-submission leaves the draft untouched, case {text:?}"
            );
        }
    }

    /// An attachment is submittable on its own, so image-only input stays
    /// valid even when its caption is empty or whitespace.
    #[test]
    fn image_only_input_still_queues_without_text() {
        for caption in ["", "   "] {
            let (mut app, mut prompt_rx) = connected_app();
            enter(&mut app, "head");
            prompt_rx.try_recv().expect("head dispatched");

            let image = PastedImage {
                data_base64: "QQ==".into(),
                mime_type: "image/png".into(),
                label: "image.png".into(),
            };
            app.current_tab_mut().insert_image_attachment(image.clone());
            app.current_tab_mut().insert_input_str(caption);
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

            assert_eq!(
                app.current_tab().pending_prompts.len(),
                1,
                "image-only input must queue, case {caption:?}"
            );
            complete_turn(&mut app);
            assert_eq!(
                prompt_rx
                    .try_recv()
                    .expect("the queued image dispatches")
                    .images,
                vec![image],
                "case {caption:?}"
            );
        }
    }

    #[test]
    fn queue_cap_preserves_the_draft() {
        let (mut app, _prompt_rx) = connected_app();
        enter(&mut app, "head");
        for index in 0..PENDING_PROMPT_QUEUE_CAP {
            enter(&mut app, &format!("queued {index}"));
        }
        assert_eq!(
            app.current_tab().pending_prompts.len(),
            PENDING_PROMPT_QUEUE_CAP
        );

        app.current_tab_mut().insert_input_str("overflow");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            app.current_tab().pending_prompts.len(),
            PENDING_PROMPT_QUEUE_CAP
        );
        assert_eq!(app.current_tab().input, "overflow");
        assert!(app.transient_hint.is_some());
    }

    #[test]
    fn owner_helper_never_dispatches_another_tabs_queue() {
        let (mut app, mut prompt_rx) = connected_app();
        app.owner_tab_id = Some("tab-a".into());
        app.tab_id = Some("tab-a".into());
        app.tab_mut("tab-a");
        app.tab_mut("tab-b")
            .pending_prompts
            .push_back(QueuedPrompt::new(
                "background".into(),
                "background".into(),
                vec![],
            ));

        assert!(!app.dispatch_next_queued_prompt("tab-b"));

        assert!(app.tab_sessions["tab-a"].turn.is_idle());
        assert!(app.tab_sessions["tab-b"].turn.is_idle());
        assert!(prompt_rx.try_recv().is_err());
    }

    #[test]
    fn queued_images_survive_until_dispatch() {
        let (mut app, mut prompt_rx) = connected_app();
        let image = PastedImage {
            data_base64: "QQ==".into(),
            mime_type: "image/png".into(),
            label: "image.png".into(),
        };
        app.current_tab_mut()
            .pending_prompts
            .push_back(QueuedPrompt::new(
                "describe this".into(),
                "[image: image.png] describe this".into(),
                vec![image.clone()],
            ));

        assert!(app.dispatch_next_queued_prompt(DEFAULT_TAB_ID));
        assert_eq!(
            prompt_rx
                .try_recv()
                .expect("queued prompt dispatched")
                .images,
            vec![image]
        );
    }

    #[test]
    fn clearing_history_drops_queued_prompts() {
        let (mut app, _prompt_rx) = connected_app();
        app.current_tab_mut()
            .pending_prompts
            .push_back(QueuedPrompt::new("stale".into(), "stale".into(), vec![]));
        app.current_tab_mut().queue_paused = true;
        app.current_tab_mut().clear_chat_history();
        assert!(app.current_tab().pending_prompts.is_empty());
        assert!(!app.current_tab().queue_paused);
    }

    #[test]
    fn tab_reset_and_rename_do_not_leak_queued_prompts() {
        let (mut app, _prompt_rx) = connected_app();
        app.tab_id = Some("old-tab".into());
        app.tab_mut("old-tab")
            .pending_prompts
            .push_back(QueuedPrompt::new(
                "queued before drag".into(),
                "queued before drag".into(),
                vec![],
            ));

        app.rename_tab_session("old-tab", "new-tab", Some("window-2"));
        assert_eq!(app.tab_sessions["new-tab"].pending_prompts.len(), 1);
        assert!(!app.tab_sessions.contains_key("old-tab"));

        app.reset_tab_session_for("new-tab");
        assert!(app.tab_sessions["new-tab"].pending_prompts.is_empty());
    }

    #[test]
    fn preview_collapses_whitespace_and_is_bounded() {
        let prompt =
            QueuedPrompt::new("unused".into(), "  first\n second\t third  ".into(), vec![]);
        assert_eq!(prompt.collapsed_text(), "first second third");

        let long = QueuedPrompt::new("unused".into(), "x ".repeat(300), vec![]);
        assert_eq!(long.collapsed_text().chars().count(), 256);
    }

    #[test]
    fn recoverable_error_pauses_queue_without_dispatching_on_later_events() {
        let (mut app, mut prompt_rx) = connected_app();
        app.current_tab_mut()
            .pending_prompts
            .push_back(QueuedPrompt::new(
                "retry me".into(),
                "retry me".into(),
                vec![],
            ));
        assert!(app.dispatch_next_queued_prompt(DEFAULT_TAB_ID));
        assert_eq!(prompt_rx.try_recv().unwrap().text, "retry me");

        app.handle_event(AppEvent::AgentError {
            session_id: None,
            prompt_id: None,
            failure: crate::protocol::acp::failure::AgentFailure::Protocol {
                code: -32603,
                message: "temporary failure".into(),
            },
            message: "temporary failure".into(),
        });
        app.handle_event(AppEvent::Tick);

        assert!(app.current_tab().queue_paused);
        assert_eq!(app.current_tab().pending_prompts.len(), 1);
        assert!(prompt_rx.try_recv().is_err());
    }

    #[test]
    fn load_success_and_failure_pause_until_explicit_fifo_resume() {
        for load_succeeds in [true, false] {
            let (mut app, mut prompt_rx) = connected_app();
            {
                let tab = app.current_tab_mut();
                tab.loading_session = true;
                tab.loading_target_session_id = Some("loaded-session".into());
                tab.pending_prompts.push_back(QueuedPrompt::new(
                    "queued during load".into(),
                    "queued during load".into(),
                    vec![],
                ));
            }
            if load_succeeds {
                app.handle_event(AppEvent::SessionAttached {
                    tab_id: DEFAULT_TAB_ID.to_string(),
                    session_id: "loaded-session".into(),
                    available_models: vec![],
                    current_model_id: None,
                });
            } else {
                app.handle_event(AppEvent::TabError {
                    tab_id: DEFAULT_TAB_ID.to_string(),
                    message: "load failed".into(),
                });
            }
            assert!(app.current_tab().queue_paused);
            assert!(prompt_rx.try_recv().is_err());

            enter(&mut app, "explicit resume");
            assert_eq!(
                prompt_rx.try_recv().unwrap().text,
                "queued during load",
                "case load_succeeds={load_succeeds}"
            );
        }
    }

    #[test]
    fn queued_input_never_bypasses_fifo_head() {
        let (mut app, mut prompt_rx) = connected_app();
        app.current_tab_mut()
            .pending_prompts
            .push_back(QueuedPrompt::new("Q1".into(), "Q1".into(), vec![]));

        enter(&mut app, "Q2");

        assert_eq!(app.current_tab().pending_prompts.len(), 2);
        assert!(prompt_rx.try_recv().is_err());
        assert!(app.dispatch_next_queued_prompt(DEFAULT_TAB_ID));
        assert_eq!(prompt_rx.try_recv().unwrap().text, "Q1");
    }

    #[test]
    fn direct_agent_busy_rolls_back_text_and_attachments_for_fifo_retry() {
        let (mut app, mut prompt_rx) = connected_app();
        let image = PastedImage {
            data_base64: "QQ==".into(),
            mime_type: "image/png".into(),
            label: "image.png".into(),
        };
        app.current_tab_mut().insert_image_attachment(image.clone());
        enter(&mut app, "describe");
        let first = prompt_rx.try_recv().expect("direct prompt dispatched");
        let prompt_id = app.current_tab().turn.prompt().unwrap().id;
        assert_eq!(first.images, vec![image.clone()]);

        app.handle_event(AppEvent::AgentBusy {
            tab_id: DEFAULT_TAB_ID.to_string(),
            prompt_id,
        });
        assert!(app.current_tab().queue_paused);
        assert_eq!(app.current_tab().pending_prompts.len(), 1);

        enter(&mut app, "explicit resume");
        let retried = prompt_rx
            .try_recv()
            .expect("rolled-back prompt retried first");
        assert_eq!(retried.text, "describe");
        assert_eq!(retried.images, vec![image]);
    }

    #[test]
    fn recommendation_execution_acknowledgement_gates_queue_drain() {
        let (mut app, mut prompt_rx) = connected_app();
        app.current_tab_mut()
            .pending_prompts
            .push_back(QueuedPrompt::new("Q2".into(), "Q2".into(), vec![]));
        gate_on_execution(&mut app, DEFAULT_TAB_ID, 1, 1);

        app.handle_event(AppEvent::Tick);
        assert!(
            prompt_rx.try_recv().is_err(),
            "pending coordinator work blocks FIFO"
        );
        app.handle_event(AppEvent::RecommendationExecutionCompleted {
            tab_id: DEFAULT_TAB_ID.to_string(),
            execution: execution(1, 1),
            choice: 1,
            result: Ok(()),
        });

        assert_eq!(prompt_rx.try_recv().unwrap().text, "Q2");
    }

    #[test]
    fn failed_recommendation_execution_pauses_queue() {
        let (mut app, mut prompt_rx) = connected_app();
        app.current_tab_mut()
            .pending_prompts
            .push_back(QueuedPrompt::new("Q2".into(), "Q2".into(), vec![]));
        gate_on_execution(&mut app, DEFAULT_TAB_ID, 1, 2);

        app.handle_event(AppEvent::RecommendationExecutionCompleted {
            tab_id: DEFAULT_TAB_ID.to_string(),
            execution: execution(1, 2),
            choice: 1,
            result: Err("executor failed".into()),
        });
        app.handle_event(AppEvent::Tick);

        assert!(app.current_tab().queue_paused);
        assert_eq!(app.current_tab().pending_prompts.len(), 1);
        assert!(prompt_rx.try_recv().is_err());
    }

    /// A failed recommendation action must never resurrect a dispatched ACP
    /// prompt. The eliminated wildcard `pause_queued_dispatch(tab, None)`
    /// restored whatever `queued_dispatch` happened to be set regardless of
    /// which prompt it belonged to; a failed action must now only pause.
    #[test]
    fn failed_recommendation_execution_never_resends_a_dispatched_prompt() {
        let (mut app, mut prompt_rx) = connected_app();
        gate_on_execution(&mut app, DEFAULT_TAB_ID, 1, 5);
        // A prompt the ACP transport already owns, exactly as
        // `dispatch_next_queued_prompt` leaves behind for whatever is
        // currently in flight on this tab.
        stub_queued_dispatch(&mut app, 9, "in flight");

        app.handle_event(AppEvent::RecommendationExecutionCompleted {
            tab_id: DEFAULT_TAB_ID.to_string(),
            execution: execution(1, 5),
            choice: 1,
            result: Err("executor failed".into()),
        });

        assert!(
            app.current_tab().queued_dispatch.is_some(),
            "a failed recommendation action must never evict an in-flight prompt's dispatch copy"
        );
        assert!(
            app.current_tab().pending_prompts.is_empty(),
            "the dispatched prompt must not be resurrected onto the queue"
        );
        assert!(prompt_rx.try_recv().is_err());
    }

    #[test]
    fn recommendation_acknowledgements_follow_execution_not_renamed_tab_id() {
        let (mut app, mut prompt_rx) = connected_app();
        app.tab_id = Some("old-tab".into());
        app.owner_tab_id = Some("old-tab".into());
        app.tab_mut("old-tab")
            .pending_prompts
            .push_back(QueuedPrompt::new("Q2".into(), "Q2".into(), vec![]));
        gate_on_execution(&mut app, "old-tab", 41, 81);

        app.handle_event(AppEvent::TabRenamed {
            old_tab_id: "old-tab".into(),
            new_tab_id: "new-tab".into(),
            new_window_id: Some("window-2".into()),
        });
        app.handle_event(AppEvent::RecommendationExecutionCompleted {
            tab_id: "old-tab".into(),
            execution: execution(41, 81),
            choice: 1,
            result: Ok(()),
        });

        assert!(!app.tab_sessions.contains_key("old-tab"));
        assert_eq!(prompt_rx.try_recv().unwrap().text, "Q2");
        assert!(matches!(
            app.tab_sessions["new-tab"].turn,
            TurnState::Submitted(_)
        ));
    }

    #[test]
    fn failed_recommendation_acknowledgement_after_rename_pauses_only_owner() {
        let (mut app, _prompt_rx) = connected_app();
        app.tab_id = Some("old-tab".into());
        app.tab_mut("old-tab")
            .pending_prompts
            .push_back(QueuedPrompt::new("Q2".into(), "Q2".into(), vec![]));
        gate_on_execution(&mut app, "old-tab", 42, 82);
        let unrelated = gate_on_execution(&mut app, "unrelated-tab", 99, 199);

        app.handle_event(AppEvent::TabRenamed {
            old_tab_id: "old-tab".into(),
            new_tab_id: "new-tab".into(),
            new_window_id: Some("window-2".into()),
        });
        app.handle_event(AppEvent::RecommendationExecutionCompleted {
            tab_id: "old-tab".into(),
            execution: execution(42, 82),
            choice: 1,
            result: Err("executor failed".into()),
        });

        assert!(app.tab_sessions["new-tab"].queue_paused);
        assert_eq!(
            app.tab_sessions["unrelated-tab"].pending_execution,
            Some(unrelated),
            "an acknowledgement must only release the gate it identifies"
        );
    }

    #[test]
    fn stale_recommendation_acknowledgement_cannot_affect_newer_turn() {
        let (mut app, mut prompt_rx) = connected_app();
        app.current_tab_mut()
            .pending_prompts
            .push_back(QueuedPrompt::new("Q3".into(), "Q3".into(), vec![]));
        let active = gate_on_execution(&mut app, DEFAULT_TAB_ID, 52, 102);

        app.handle_event(AppEvent::RecommendationExecutionCompleted {
            tab_id: DEFAULT_TAB_ID.into(),
            execution: execution(51, 101),
            choice: 1,
            result: Err("old execution failed".into()),
        });
        app.handle_event(AppEvent::RecommendationExecutionCompleted {
            tab_id: DEFAULT_TAB_ID.into(),
            execution: execution(52, 103),
            choice: 1,
            result: Err("wrong execution failed".into()),
        });

        assert_eq!(
            app.current_tab().pending_execution,
            Some(active),
            "neither a stale nor a mismatched acknowledgement may release the gate"
        );
        assert!(!app.current_tab().queue_paused);
        assert!(prompt_rx.try_recv().is_err());
    }

    #[test]
    fn delayed_acp_completion_after_tab_rename_closes_its_prompt_only() {
        let (mut app, mut prompt_rx) = connected_app();
        app.tab_id = Some("old-tab".into());
        app.owner_tab_id = Some("old-tab".into());
        {
            let tab = app.tab_mut("old-tab");
            tab.session_id = Some("stable-session".into());
            tab.turn = TurnState::Submitted(submitted(71, "Q1"));
            tab.pending_prompts
                .push_back(QueuedPrompt::new("Q2".into(), "Q2".into(), vec![]));
        }
        app.session_to_tab
            .insert("stable-session".into(), "old-tab".into());

        app.handle_event(AppEvent::TabRenamed {
            old_tab_id: "old-tab".into(),
            new_tab_id: "new-tab".into(),
            new_window_id: Some("window-2".into()),
        });
        app.session_to_tab.remove("stable-session");
        app.tab_id = Some("other-tab".into());
        app.tab_mut("other-tab");
        app.handle_event(AppEvent::AgentTurnCompleted {
            session_id: "stable-session".into(),
            prompt_id: 71,
            soft_stop: None,
        });

        assert!(!app.tab_sessions.contains_key("old-tab"));
        assert_eq!(
            app.session_to_tab.get("stable-session").map(String::as_str),
            Some("new-tab")
        );
        assert_eq!(prompt_rx.try_recv().unwrap().text, "Q2");
        assert!(matches!(
            app.tab_sessions["new-tab"].turn,
            TurnState::Submitted(_)
        ));
    }

    #[test]
    fn stale_acp_failure_cannot_fail_a_replacement_prompt() {
        let (mut app, _prompt_rx) = connected_app();
        app.current_tab_mut().turn = TurnState::Submitted(submitted(74, "replacement"));

        app.handle_event(AppEvent::AgentError {
            session_id: Some("stable-session".into()),
            prompt_id: Some(73),
            failure: crate::protocol::acp::failure::AgentFailure::Protocol {
                code: -32603,
                message: "old prompt failed".into(),
            },
            message: "old prompt failed".into(),
        });

        assert!(matches!(
            app.current_tab().turn,
            TurnState::Submitted(SubmittedPrompt { id: 74, .. })
        ));
        assert!(app.current_tab().messages.is_empty());
    }

    #[test]
    fn delayed_acp_cancellation_end_cannot_close_a_replacement_prompt() {
        let (mut app, _prompt_rx) = connected_app();
        app.current_tab_mut().session_id = Some("stable-session".into());
        app.current_tab_mut().turn = TurnState::Submitted(submitted(75, "cancelled"));
        app.turn_cancel(DEFAULT_TAB_ID);
        app.current_tab_mut().turn = TurnState::Submitted(submitted(76, "replacement"));

        app.handle_event(AppEvent::AgentMessageEnd {
            session_id: "stable-session".into(),
            prompt_id: 75,
        });

        assert!(matches!(
            app.current_tab().turn,
            TurnState::Submitted(SubmittedPrompt { id: 76, .. })
        ));
    }

    #[test]
    fn soft_stop_metadata_is_committed_before_the_next_queue_item() {
        let (mut app, mut prompt_rx) = connected_app();
        enter(&mut app, "Q1");
        let _ = prompt_rx.try_recv();
        enter(&mut app, "Q2");

        app.handle_event(AppEvent::AgentTurnCompleted {
            session_id: DEFAULT_TAB_ID.to_string(),
            prompt_id: app.current_tab().turn.prompt().unwrap().id,
            soft_stop: Some(crate::protocol::acp::soft_stop::SoftStopReason::MaxTokens),
        });

        assert_eq!(prompt_rx.try_recv().unwrap().text, "Q2");
        assert!(matches!(
            app.current_tab()
                .completed_turns
                .last()
                .and_then(|turn| turn.details.last()),
            Some(ChatMessage::Notice {
                kind: super::tab_state::NoticeKind::Warning,
                ..
            })
        ));
    }
}
