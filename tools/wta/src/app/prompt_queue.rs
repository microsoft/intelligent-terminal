//! Helper-owned pending work. A queue entry never installs a turn or a responder.
use super::*;

const MAX_REQUESTS: usize = 32;
const MAX_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
// Reserve context space before capture, then account for the actual snapshot payload.
const SNAPSHOT_RESERVATION: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RequestKind {
    Prompt,
    AgentCommand,
    ManualFix,
    AutomaticFix,
}

pub(super) struct QueuedRequest {
    pub submission: PromptSubmission,
    pub display_text: String,
    pub kind: RequestKind,
    pub queued_at: std::time::Instant,
    pub capturing: bool,
}

impl QueuedRequest {
    fn preview(&self, index: usize) -> String {
        let safe: String = self
            .display_text
            .chars()
            .map(|c| {
                if c.is_control()
                    || c.is_whitespace()
                    || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
                {
                    ' '
                } else {
                    c
                }
            })
            .take(120)
            .collect();
        let marker = if self.kind == RequestKind::AutomaticFix {
            format!("{} · ", t!("queue.auto"))
        } else {
            String::new()
        };
        format!("{}. {marker}{safe}", index + 1)
    }

    fn bytes(&self) -> usize {
        self.submission.text.len()
            + self.display_text.len()
            + self
                .submission
                .pane_context
                .as_ref()
                .map(|context| {
                    [
                        &context.pane_id,
                        &context.tab_id,
                        &context.window_id,
                        &context.cwd,
                        &context.source_pane_id,
                    ]
                    .into_iter()
                    .filter_map(|value| value.as_ref())
                    .map(String::len)
                    .sum::<usize>()
                })
                .unwrap_or(0)
            + self
                .submission
                .images
                .iter()
                .map(|image| image.data_base64.len() + image.label.len() + image.mime_type.len())
                .sum::<usize>()
            + self
                .submission
                .autofix_snapshot
                .as_ref()
                .map(|snapshot| snapshot.payload_bytes())
                .unwrap_or(if self.capturing {
                    SNAPSHOT_RESERVATION
                } else {
                    0
                })
    }

    fn source(&self) -> Option<&str> {
        self.submission
            .pane_context
            .as_ref()?
            .source_pane_id
            .as_deref()
    }

    /// Admission and insertion exclude exactly the same automatic request.
    /// A diagnostic activation promotes it; a typed /fix remains independent FIFO work.
    fn replaces_automatic(&self, entry: &Self) -> bool {
        entry.kind == RequestKind::AutomaticFix
            && entry.source() == self.source()
            && (self.kind == RequestKind::AutomaticFix
                || (self.kind == RequestKind::ManualFix
                    && self.submission.autofix_text_kind
                        == Some(crate::protocol::acp::client::AutofixTextKind::FailureSummary)))
    }
}

#[derive(Default)]
pub(super) struct PromptQueue {
    pub entries: VecDeque<QueuedRequest>,
    pub echoes: HashSet<String>,
    pub active_automatic_id: Option<u64>,
}

impl PromptQueue {
    pub fn rename(&mut self, tab_id: &str, window_id: Option<&str>) {
        for context in self
            .entries
            .iter_mut()
            .filter_map(|item| item.submission.pane_context.as_mut())
        {
            context.tab_id = Some(tab_id.to_owned());
            if let Some(window) = window_id {
                context.window_id = Some(window.to_owned());
            }
        }
    }

    fn cancel_pending(&mut self) -> usize {
        let discarded = self.entries.len();
        for entry in self.entries.drain(..) {
            entry.submission.cancellation_token().cancel();
        }
        self.echoes.clear();
        discarded
    }

    fn invalidate_automatic(&mut self, pane: &str) {
        self.entries.retain(|entry| {
            if entry.kind == RequestKind::AutomaticFix && entry.source() == Some(pane) {
                entry.submission.cancellation_token().cancel();
                false
            } else {
                true
            }
        });
        self.echoes.remove(pane);
    }

    fn can_fit(&self, item: &QueuedRequest, pending_image_bytes: usize) -> bool {
        let retained = |entry: &&QueuedRequest| !item.replaces_automatic(entry);
        let count = self.entries.iter().filter(retained).count();
        let bytes: usize = self
            .entries
            .iter()
            .filter(retained)
            .map(QueuedRequest::bytes)
            .sum();
        count < MAX_REQUESTS
            && bytes
                .saturating_add(item.bytes())
                .saturating_add(pending_image_bytes)
                <= MAX_PAYLOAD_BYTES
    }

    fn insert(&mut self, item: QueuedRequest) {
        if item.kind == RequestKind::AutomaticFix {
            let old_index = self
                .entries
                .iter()
                .position(|entry| item.replaces_automatic(entry));
            if let Some(index) = old_index {
                self.entries[index].submission.cancellation_token().cancel();
                self.entries[index] = item;
            } else {
                self.entries.push_back(item);
            }
        } else {
            if self
                .entries
                .iter()
                .any(|entry| item.replaces_automatic(entry))
            {
                if let Some(source) = item.source() {
                    self.echoes.remove(source);
                }
            }
            self.entries.retain(|entry| {
                if item.replaces_automatic(entry) {
                    entry.submission.cancellation_token().cancel();
                    false
                } else {
                    true
                }
            });
            let index = self
                .entries
                .iter()
                .position(|entry| entry.kind == RequestKind::AutomaticFix)
                .unwrap_or(self.entries.len());
            self.entries.insert(index, item);
        }
    }
}

impl Drop for PromptQueue {
    fn drop(&mut self) {
        self.cancel_pending();
    }
}

impl TabSession {
    /// Discard only pending requests. The active turn, action barrier, and draft
    /// belong to their existing lifecycle owners and remain untouched.
    pub(super) fn cancel_pending_prompts(&mut self) {
        let discarded = self.prompt_queue.cancel_pending();
        if discarded > 0 {
            self.messages.push(ChatMessage::info(
                t!("queue.cancelled", count = discarded).into_owned(),
            ));
            self.scroll_to_bottom();
        }
    }
}

impl App {
    pub(crate) fn pending_input_previews(&self) -> impl Iterator<Item = String> + '_ {
        self.current_tab()
            .prompt_queue
            .entries
            .iter()
            .enumerate()
            .map(|(index, item)| item.preview(index))
    }

    pub(super) fn queue_notice(&mut self, message: String) {
        let tab = self.current_tab_mut();
        tab.messages.push(ChatMessage::warning(message));
        tab.scroll_to_bottom();
    }

    fn queue_dispatch_ready(&self, tab_id: &str) -> bool {
        let Some(tab) = self.tab_sessions.get(tab_id) else {
            return false;
        };
        // This allowlist governs built-in selection, not the trusted custom/default
        // command already resolved by the host and master during initialization.
        let agent_allowed = !crate::agent_registry::is_known_id(&self.current_agent_id)
            || !self.host_agent_allowlist_present
            || self
                .allowed_agent_ids
                .iter()
                .any(|id| id.eq_ignore_ascii_case(&self.current_agent_id));
        self.state == ConnectionState::Connected
            && self.mode == AppMode::Chat
            && agent_allowed
            && matches!(self.agent_reconnect_state, AgentReconnectState::Idle)
            && !tab.loading_session
            && !self
                .pending_session_load
                .as_ref()
                .is_some_and(|load| load.tab_id == tab_id)
            && tab.config_pending_id.is_none()
            && !self.prompt_reconfiguration_pending_for_tab(tab_id)
            && tab.turn.accepts_new_prompt()
            && tab.turn.recommendations().is_none()
            && tab.permission.is_empty()
            && tab.user_input.is_empty()
            && tab.pending_terminal_action_proposal.is_none()
            && tab.pending_queue_action.is_none()
    }

    pub(super) fn dispatch_prompt_queues(&mut self) {
        for tab_id in self.tab_sessions.keys().cloned().collect::<Vec<_>>() {
            if !self.queue_dispatch_ready(&tab_id) {
                continue;
            }
            let queue = &mut self.tab_mut(&tab_id).prompt_queue;
            if queue.entries.front().is_none_or(|item| item.capturing) {
                continue;
            }
            let Some(mut item) = queue.entries.pop_front() else {
                continue;
            };
            item.submission = item
                .submission
                .with_byok(self.current_model_is_byok())
                .with_agent_id(self.current_agent_id.clone());
            item.submission.submitted_at_unix_s = now_unix_s();
            if let Some(context) = item.submission.pane_context.as_mut() {
                context.tab_id = Some(tab_id.clone());
            }
            let source = item.source().map(str::to_owned);
            let prompt_id = item.submission.id;
            let submitted_at_unix_s = item.submission.submitted_at_unix_s;
            let cancellation = item.submission.cancellation_token();
            let failure_summary = item.submission.autofix_text_kind
                == Some(crate::protocol::acp::client::AutofixTextKind::FailureSummary);
            if let Err(error) = self.prompt_tx.send(item.submission) {
                // Definitely unsent: include this request in the discard count,
                // without installing a turn or replacing the previous transcript.
                item.submission = error.0;
                let tab = self.tab_mut(&tab_id);
                tab.prompt_queue.entries.push_front(item);
                tab.messages
                    .push(ChatMessage::Error(t!("connection.lost").into_owned()));
                tab.cancel_pending_prompts();
                tracing::warn!(target: "prompt_queue", prompt_id,
                    "prompt channel closed; discarding pending requests");
                continue;
            }
            self.tab_mut(&tab_id).prompt_queue.active_automatic_id =
                (item.kind == RequestKind::AutomaticFix).then_some(prompt_id);
            let autofix = if matches!(
                item.kind,
                RequestKind::ManualFix | RequestKind::AutomaticFix
            ) {
                let tab = self.tab_mut(&tab_id);
                tab.autofix.generation = tab.autofix.generation.wrapping_add(1);
                tab.autofix.suggested_pane_id = None;
                Some(AutofixContext {
                    generation: tab.autofix.generation,
                })
            } else {
                None
            };
            tracing::info!(target: "prompt_queue", request_id = prompt_id,
                queued_ms = item.queued_at.elapsed().as_millis(), "dispatching queued request");
            let submitted = SubmittedPrompt {
                id: prompt_id,
                text: item.display_text.clone(),
                submitted_at_unix_s,
                context: TurnContext {
                    target_pane_id: source.clone(),
                },
                autofix,
            };
            self.turn_submit_prompt_for_tab_with_cancellation(&tab_id, submitted, cancellation);
            if failure_summary {
                if let Some(pane) = source {
                    let tab = self.tab_mut(&tab_id);
                    tab.autofix.pane_id = Some(pane.clone());
                    tab.autofix.armed_at = Some(std::time::Instant::now());
                    self.emit_autofix_state_pending(&tab_id, &pane, &item.display_text);
                }
            }
        }
    }

    pub(super) fn ensure_prompt_connection(&mut self) -> bool {
        let tab_id = self.active_tab_key().to_owned();
        self.ensure_prompt_connection_for_tab(&tab_id)
    }

    fn ensure_prompt_connection_for_tab(&mut self, tab_id: &str) -> bool {
        if matches!(
            self.state,
            ConnectionState::Connected | ConnectionState::Connecting(_)
        ) {
            true
        } else {
            let tab = self.tab_mut(tab_id);
            tab.messages
                .push(ChatMessage::Error(t!("connection.lost").into_owned()));
            tab.scroll_to_bottom();
            false
        }
    }

    pub(super) fn enqueue_input(&mut self, manual_fix: Option<String>) {
        if !self.ensure_prompt_connection() {
            return;
        }
        let tab_id = self.active_tab_key().to_owned();
        let tab = self.current_tab();
        let display = tab.input.clone();
        let history_text = tab.attachments.submission_text(display.clone());
        let pending_image_bytes = tab.attachments.payload_bytes();
        let kind = if manual_fix.is_some() {
            RequestKind::ManualFix
        } else if self.agent_command_for_input(&history_text).is_some() {
            RequestKind::AgentCommand
        } else {
            RequestKind::Prompt
        };
        let context = PaneContext {
            pane_id: self.pane_id.clone(),
            tab_id: Some(tab_id.clone()),
            window_id: self.window_id.clone(),
            cwd: self.source_cwd.clone(),
            source_pane_id: self.source_session_id.clone(),
        };
        // Attachment tokens belong to the editor, not the /fix user intent.
        let text = if let Some(hint) = manual_fix {
            commands::parse(&history_text)
                .filter(|command| command.kind == CommandKind::Fix)
                .map(|command| command.rest)
                .unwrap_or(hint)
        } else {
            history_text.clone()
        };
        let submission = match kind {
            RequestKind::AgentCommand => {
                PromptSubmission::new_agent_command(text, Some(context.clone()))
            }
            RequestKind::ManualFix => PromptSubmission::new_autofix(text, Some(context.clone())),
            _ => PromptSubmission::new(text, Some(context.clone())),
        };
        let display_text = if kind == RequestKind::ManualFix {
            format!("/fix {}", submission.text)
        } else {
            display
        };
        let mut item = QueuedRequest {
            submission,
            display_text,
            kind,
            queued_at: std::time::Instant::now(),
            capturing: kind == RequestKind::ManualFix,
        };
        if !self
            .current_tab()
            .prompt_queue
            .can_fit(&item, pending_image_bytes)
        {
            self.queue_notice(t!("queue.full").into_owned());
            return;
        }
        let tab = self.current_tab_mut();
        item.submission = item.submission.with_images(tab.attachments.take_images());
        tab.record_input_history(&history_text);
        let request_id = item.submission.id;
        let cancellation = item.submission.cancellation_token();
        tab.prompt_queue.insert(item);
        tab.clear_input();
        if kind == RequestKind::ManualFix {
            self.launch_autofix_capture(
                request_id,
                cancellation,
                context,
                crate::protocol::acp::client::AutofixTextKind::UserRequest,
            );
        }
        if self.show_welcome_hint {
            self.show_welcome_hint = false;
            set_welcome_shown_in_state();
        }
        self.dispatch_prompt_queues();
        let tab = self.current_tab_mut();
        if tab
            .prompt_queue
            .entries
            .iter()
            .any(|entry| entry.submission.id == request_id)
        {
            tab.messages
                .push(ChatMessage::info(t!("queue.enqueued").into_owned()));
            tab.scroll_to_bottom();
        }
    }

    pub(super) fn queue_blocks_session_change(&mut self) -> bool {
        if self
            .tab_sessions
            .values()
            .any(|tab| !tab.prompt_queue.entries.is_empty() || tab.pending_queue_action.is_some())
        {
            self.queue_notice(t!("queue.session_busy").into_owned());
            true
        } else {
            false
        }
    }

    pub(super) fn invalidate_prompt_queue_sessions(&mut self) {
        for tab in self.tab_sessions.values_mut() {
            tab.cancel_pending_prompts();
            tab.pending_queue_action = None;
        }
    }

    pub(super) fn enqueue_autofix(
        &mut self,
        tab_id: &str,
        pane: &str,
        summary: &str,
        forced: bool,
    ) {
        if !self.ensure_prompt_connection_for_tab(tab_id) {
            return;
        }
        let context = PaneContext {
            pane_id: self.pane_id.clone(),
            tab_id: Some(tab_id.to_owned()),
            window_id: self.window_id.clone(),
            cwd: None,
            source_pane_id: Some(pane.to_owned()),
        };
        let submission =
            PromptSubmission::new_autofix_failure(summary.to_owned(), Some(context.clone()));
        let request_id = submission.id;
        let cancellation = submission.cancellation_token();
        let item = QueuedRequest {
            submission,
            display_text: summary.to_owned(),
            kind: if forced {
                RequestKind::ManualFix
            } else {
                RequestKind::AutomaticFix
            },
            queued_at: std::time::Instant::now(),
            capturing: true,
        };
        if !self.tab_mut(tab_id).prompt_queue.can_fit(&item, 0) {
            self.tab_mut(tab_id)
                .messages
                .push(ChatMessage::warning(t!("queue.full").into_owned()));
            return;
        }
        if forced {
            self.tab_mut(tab_id).autofix.detected_request_id = Some(request_id);
        }
        self.tab_mut(tab_id).prompt_queue.insert(item);
        if !forced {
            self.tab_mut(tab_id).autofix.detected_request_id = None;
            self.tab_mut(tab_id)
                .prompt_queue
                .echoes
                .insert(pane.to_owned());
            self.tab_mut(tab_id).autofix.trigger_echo_pane = Some(pane.to_owned());
            self.emit_autofix_state_detected(tab_id, pane, summary);
        }
        self.launch_autofix_capture(
            request_id,
            cancellation,
            context,
            crate::protocol::acp::client::AutofixTextKind::FailureSummary,
        );
    }

    fn launch_autofix_capture(
        &self,
        request_id: u64,
        cancellation: tokio_util::sync::CancellationToken,
        context: PaneContext,
        text_kind: crate::protocol::acp::client::AutofixTextKind,
    ) {
        if let (Some(tx), Ok(runtime)) =
            (self.event_tx.clone(), tokio::runtime::Handle::try_current())
        {
            let shell = self.shell_mgr.clone();
            runtime.spawn(async move {
                let result = tokio::select! {
                    _ = cancellation.cancelled() => return,
                    result = crate::protocol::acp::client::capture_autofix_snapshot(&shell, &context, text_kind) => result,
                };
                let _ = tx.send(AppEvent::AutofixSnapshotReady { request_id, result });
            });
        } else {
            tracing::warn!(target: "prompt_queue", request_id, "snapshot capture unavailable without runtime/event channel");
        }
    }

    pub(super) fn autofix_snapshot_ready(
        &mut self,
        request_id: u64,
        result: Result<crate::protocol::acp::client::AutofixSnapshot, String>,
    ) {
        let Some((tab_id, index)) = self.tab_sessions.iter().find_map(|(tab_id, tab)| {
            tab.prompt_queue
                .entries
                .iter()
                .position(|entry| entry.submission.id == request_id && entry.capturing)
                .map(|index| (tab_id.clone(), index))
        }) else {
            tracing::debug!(target: "prompt_queue", request_id, "ignoring stale context capture");
            return;
        };
        let result = result.and_then(|snapshot| {
            let queue = &self.tab_sessions[&tab_id].prompt_queue;
            let source_binding_bytes = if queue.entries[index].source().is_none() {
                snapshot.source_pane_id().len()
            } else {
                0
            };
            let bytes = queue
                .entries
                .iter()
                .map(QueuedRequest::bytes)
                .sum::<usize>()
                .saturating_sub(SNAPSHOT_RESERVATION)
                .saturating_add(snapshot.payload_bytes())
                .saturating_add(source_binding_bytes);
            if bytes > MAX_PAYLOAD_BYTES {
                Err(t!("queue.full").into_owned())
            } else {
                Ok(snapshot)
            }
        });
        match result {
            Ok(snapshot) => {
                let item = &mut self.tab_mut(&tab_id).prompt_queue.entries[index];
                if let Some(context) = item.submission.pane_context.as_mut() {
                    context.source_pane_id = Some(snapshot.source_pane_id().to_owned());
                }
                item.submission.autofix_snapshot = Some(snapshot);
                item.capturing = false;
                tracing::info!(
                    target: "prompt_queue",
                    request_id,
                    source_pane_id = item.source().unwrap_or_default(),
                    "autofix snapshot ready"
                );
            }
            Err(error) => {
                tracing::warn!(target: "prompt_queue", request_id, %error, "context capture failed");
                let tab = self.tab_mut(&tab_id);
                tab.messages.push(ChatMessage::warning(
                    t!("queue.capture_failed", error = error).into_owned(),
                ));
                if tab.prompt_queue.entries[index].kind == RequestKind::AutomaticFix {
                    if let Some(item) = tab.prompt_queue.entries.remove(index) {
                        item.submission.cancellation_token().cancel();
                        if let Some(source) = item.source() {
                            tab.prompt_queue.echoes.remove(source);
                        }
                    }
                    tab.scroll_to_bottom();
                } else {
                    tab.cancel_pending_prompts();
                }
            }
        }
    }

    pub(super) fn invalidate_pending_autofix(&mut self, tab_id: &str, pane: &str) {
        if let Some(tab) = self.tab_sessions.get_mut(tab_id) {
            let explicit_capture_invalidated = tab.prompt_queue.entries.iter().any(|entry| {
                entry.source().is_none_or(|source| source == pane)
                    && entry.capturing
                    && entry.kind != RequestKind::AutomaticFix
            });
            if explicit_capture_invalidated {
                tab.messages.push(ChatMessage::warning(
                    t!(
                        "queue.capture_failed",
                        error = t!("queue.snapshot_context_changed", pane = pane)
                    )
                    .into_owned(),
                ));
                tab.cancel_pending_prompts();
                return;
            }
            tab.prompt_queue.invalidate_automatic(pane);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> (App, mpsc::UnboundedReceiver<PromptSubmission>) {
        let mut app = super::super::tests::test_app();
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
        let tab = app.current_tab_mut();
        tab.input = text.into();
        tab.cursor_pos = text.len();
        tab.refresh_command_popup();
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
    }

    fn end(app: &mut App) {
        app.handle_event(AppEvent::AgentMessageEnd {
            session_id: "queue-session".into(),
        });
    }

    fn hold(app: &mut App) {
        app.current_tab_mut().config_pending_id = Some("mode".into());
    }

    fn release(app: &mut App) {
        app.current_tab_mut().config_pending_id = None;
        app.dispatch_prompt_queues();
    }

    fn queued_info_count(app: &App) -> usize {
        app.current_tab()
            .messages
            .iter()
            .filter(|message| {
                matches!(message, ChatMessage::Notice { kind: NoticeKind::Info, text }
                    if text == t!("queue.enqueued").as_ref())
            })
            .count()
    }

    fn has_cancelled_notice(app: &App, count: usize) -> bool {
        let tab = app.current_tab();
        tab.messages
            .iter()
            .chain(
                tab.completed_turns
                    .iter()
                    .filter(|turn| turn.expanded)
                    .flat_map(|turn| &turn.details),
            )
            .any(|message| {
                matches!(message, ChatMessage::Notice { kind: NoticeKind::Info, text }
                if text == t!("queue.cancelled", count = count).as_ref())
            })
    }

    fn pending_tokens(app: &App) -> Vec<tokio_util::sync::CancellationToken> {
        app.current_tab()
            .prompt_queue
            .entries
            .iter()
            .map(|entry| entry.submission.cancellation_token())
            .collect()
    }

    #[test]
    fn trusted_custom_and_legacy_defaults_are_not_filtered_by_builtin_queue_policy() {
        let _locale = crate::test_support::lock_locale();
        for agent_id in ["custom:queue-fixture", "unknown"] {
            let (mut app, mut rx) = app();
            app.current_agent_id = agent_id.into();
            app.host_agent_allowlist_present = true;
            app.allowed_agent_ids = vec!["copilot".into()];
            enter(&mut app, "first");
            assert_eq!(rx.try_recv().unwrap().agent_id(), agent_id);
            enter(&mut app, "second");
            assert!(rx.try_recv().is_err());
            end(&mut app);
            assert_eq!(rx.try_recv().unwrap().text, "second");
            assert!(app.current_tab().prompt_queue.entries.is_empty());
        }
    }

    #[test]
    fn builtin_queue_policy_still_blocks_a_disallowed_agent() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        app.current_agent_id = "claude".into();
        app.host_agent_allowlist_present = true;
        app.allowed_agent_ids = vec!["copilot".into()];
        enter(&mut app, "waiting");
        assert!(rx.try_recv().is_err());
        assert_eq!(app.current_tab().prompt_queue.entries.len(), 1);
        app.allowed_agent_ids.push("claude".into());
        app.dispatch_prompt_queues();
        assert_eq!(rx.try_recv().unwrap().text, "waiting");
    }

    #[test]
    fn waiting_messages_receive_one_info_notice_each_but_immediate_sends_do_not() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        enter(&mut app, "immediate");
        rx.try_recv().unwrap();
        assert_eq!(queued_info_count(&app), 0);
        enter(&mut app, "queued second");
        enter(&mut app, "queued third");
        assert_eq!(queued_info_count(&app), 2);
        app.handle_event(AppEvent::Tick);
        assert_eq!(queued_info_count(&app), 2);
        assert_eq!(app.current_tab().prompt_queue.entries.len(), 2);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn connecting_and_configuration_gates_notify_when_input_is_accepted() {
        let _locale = crate::test_support::lock_locale();
        for connecting in [true, false] {
            let (mut app, mut rx) = app();
            if connecting {
                app.state = ConnectionState::Connecting("startup".into());
            } else {
                hold(&mut app);
            }
            enter(&mut app, "waiting");
            assert_eq!(queued_info_count(&app), 1);
            assert!(app.current_tab().input.is_empty());
            assert!(rx.try_recv().is_err());
        }
    }

    #[test]
    fn user_input_connection_admission_preserves_or_moves_attachments() {
        let _locale = crate::test_support::lock_locale();
        for state in [
            ConnectionState::Disconnected,
            ConnectionState::Failed("startup failed".into()),
            ConnectionState::Connecting("startup".into()),
        ] {
            for (text, kind) in [
                ("explain ", RequestKind::Prompt),
                ("/fix investigate ", RequestKind::ManualFix),
                ("/review changes ", RequestKind::AgentCommand),
            ] {
                let (mut app, mut rx) = app();
                app.state = state.clone();
                app.session_commands.insert(
                    "queue-session".into(),
                    vec![crate::app_contracts::AcpSessionCommand {
                        name: "review".into(),
                        description: "Review changes".into(),
                        input_hint: Some("focus".into()),
                        completion_behavior:
                            crate::app_contracts::CompletionBehavior::OptionalFreeText,
                    }],
                );
                let tab = app.current_tab_mut();
                tab.input = text.into();
                tab.cursor_pos = tab.input.len();
                let image = crate::clipboard_image::PastedImage {
                    data_base64: "aW1hZ2U=".into(),
                    mime_type: "image/png".into(),
                    label: "draft.png".into(),
                };
                let payload = image.data_base64.as_ptr();
                tab.attachments
                    .insert_image(&mut tab.input, &mut tab.cursor_pos, image);
                let draft = tab.input.clone();
                let cursor = tab.cursor_pos;
                let ranges = tab.attachments.token_ranges().collect::<Vec<_>>();
                tab.refresh_command_popup();
                app.handle_event(AppEvent::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                )));
                let tab = app.current_tab();
                assert!(rx.try_recv().is_err());
                assert!(tab.turn.is_idle());
                if matches!(state, ConnectionState::Connecting(_)) {
                    assert!(tab.input.is_empty());
                    assert!(tab.attachments.is_empty());
                    assert_eq!(tab.prompt_queue.entries.len(), 1);
                    let entry = &tab.prompt_queue.entries[0];
                    assert_eq!(entry.kind, kind);
                    assert_eq!(entry.submission.images[0].data_base64.as_ptr(), payload);
                    assert_eq!(entry.capturing, kind == RequestKind::ManualFix);
                    if entry.capturing {
                        let id = entry.submission.id;
                        app.autofix_snapshot_ready(
                            id,
                            Ok(crate::protocol::acp::client::AutofixSnapshot::for_test(
                                "source",
                            )),
                        );
                    }
                    app.state = ConnectionState::Connected;
                    app.dispatch_prompt_queues();
                    assert_eq!(
                        rx.try_recv().unwrap().images[0].data_base64.as_ptr(),
                        payload
                    );
                } else {
                    assert_eq!(tab.input, draft);
                    assert_eq!(tab.cursor_pos, cursor);
                    assert_eq!(tab.attachments.token_ranges().collect::<Vec<_>>(), ranges);
                    assert_eq!(
                        tab.attachments
                            .images()
                            .next()
                            .unwrap()
                            .data_base64
                            .as_ptr(),
                        payload
                    );
                    assert!(tab.prompt_queue.entries.is_empty());
                    assert!(matches!(tab.messages.last(), Some(ChatMessage::Error(text))
                        if text == t!("connection.lost").as_ref()));
                }
            }
        }
    }

    #[test]
    fn unavailable_agent_command_popup_preserves_draft_and_local_commands() {
        let _locale = crate::test_support::lock_locale();
        for state in [
            ConnectionState::Disconnected,
            ConnectionState::Failed("startup failed".into()),
        ] {
            for input in ["/sta", "/status"] {
                let (mut app, mut rx) = app();
                app.state = state.clone();
                app.session_commands.insert(
                    "queue-session".into(),
                    vec![crate::app_contracts::AcpSessionCommand {
                        name: "status".into(),
                        description: "Session status".into(),
                        input_hint: None,
                        completion_behavior:
                            crate::app_contracts::CompletionBehavior::ExecuteImmediately,
                    }],
                );
                app.current_tab_mut().replace_input(input.into());
                app.current_tab_mut().refresh_command_popup();
                assert!(app.command_popup_visible());
                enter(&mut app, input);
                assert_eq!(app.current_tab().input, input);
                assert!(app.current_tab().prompt_queue.entries.is_empty());
                assert!(rx.try_recv().is_err());
                assert!(matches!(app.current_tab().messages.last(),
                    Some(ChatMessage::Error(text)) if text == t!("connection.lost").as_ref()));
                enter(&mut app, "/help");
                assert!(app.help_overlay_visible);
            }
        }
    }

    #[test]
    fn unavailable_autofix_rejects_requests_without_blocking_restart_or_retry() {
        let _locale = crate::test_support::lock_locale();
        for state in [
            ConnectionState::Disconnected,
            ConnectionState::Failed("startup failed".into()),
        ] {
            for automatic in [false, true] {
                let (mut app, mut rx) = app();
                app.show_welcome_hint = false;
                app.autofix_enabled = automatic;
                let notification = WtNotification {
                    severity: WtEventSeverity::Actionable,
                    pane_id: "failed-source".into(),
                    tab_id: Some("queue-tab".into()),
                    summary: "Command failed (exit 1)".into(),
                    acknowledged: false,
                    age_ticks: 0,
                };
                if !automatic {
                    app.maybe_trigger_autofix(&notification);
                }
                app.state = state.clone();
                let (restart_tx, mut restart_rx) = mpsc::unbounded_channel();
                app.restart_tx = restart_tx;
                if automatic {
                    app.maybe_trigger_autofix(&notification);
                } else {
                    app.handle_autofix_execute_from_detected("failed-source", Some("queue-tab"));
                }
                let tab = app.current_tab();
                assert!(tab.prompt_queue.entries.is_empty());
                assert!(tab.prompt_queue.echoes.is_empty());
                assert!(tab.autofix.detected_request_id.is_none());
                assert!(tab.turn.is_idle());
                assert!(matches!(tab.messages.last(), Some(ChatMessage::Error(text))
                    if text == t!("connection.lost").as_ref()));
                assert!(rx.try_recv().is_err());

                enter(&mut app, "/restart");
                assert!(matches!(
                    restart_rx.try_recv(),
                    Ok(AgentLifecycleRequest::RestartMaster)
                ));
                assert!(matches!(app.state, ConnectionState::Connecting(_)));
                if automatic {
                    app.maybe_trigger_autofix(&notification);
                } else {
                    app.handle_autofix_execute_from_detected("failed-source", Some("queue-tab"));
                }
                assert_eq!(app.current_tab().prompt_queue.entries.len(), 1);
                super::super::tests::complete_autofix_capture(&mut app, "queue-tab");
                assert!(rx.try_recv().is_err());
                app.state = ConnectionState::Connected;
                app.dispatch_prompt_queues();
                assert_eq!(rx.try_recv().unwrap().text, notification.summary);
                assert!(rx.try_recv().is_err());
            }
        }
    }

    #[test]
    fn unavailable_autofix_reports_connection_error_only_to_owning_tab() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        app.state = ConnectionState::Failed("startup failed".into());
        app.autofix_enabled = true;
        app.current_tab_mut().replace_input("keep my draft".into());
        let messages_before = app.current_tab().messages.len();
        app.maybe_trigger_autofix(&WtNotification {
            severity: WtEventSeverity::Actionable,
            pane_id: "background-source".into(),
            tab_id: Some("background-tab".into()),
            summary: "Command failed (exit 1)".into(),
            acknowledged: false,
            age_ticks: 0,
        });
        let target = &app.tab_sessions["background-tab"];
        assert!(target.prompt_queue.entries.is_empty());
        assert!(
            matches!(target.messages.last(), Some(ChatMessage::Error(text))
            if text == t!("connection.lost").as_ref())
        );
        assert_eq!(app.current_tab().messages.len(), messages_before);
        assert_eq!(app.current_tab().input, "keep my draft");
        assert!(!app.queue_blocks_session_change());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn typed_fix_history_replays_command_with_fresh_capture_without_image_tokens() {
        let _locale = crate::test_support::lock_locale();
        for input in [
            "/fix ",
            "/fix explain why this failed",
            "/fix first\nsecond ",
        ] {
            for with_image in [false, true] {
                for completed in [false, true] {
                    let (mut app, mut rx) = app();
                    app.show_welcome_hint = false;
                    app.source_session_id = Some("original-source".into());
                    let tab = app.current_tab_mut();
                    tab.replace_input(input.into());
                    if with_image {
                        tab.attachments.insert_image(
                            &mut tab.input,
                            &mut tab.cursor_pos,
                            crate::clipboard_image::PastedImage {
                                data_base64: "aW1hZ2U=".into(),
                                mime_type: "image/png".into(),
                                label: "failure.png".into(),
                            },
                        );
                    }
                    app.handle_event(AppEvent::Key(KeyEvent::new(
                        KeyCode::Enter,
                        KeyModifiers::NONE,
                    )));
                    assert_eq!(
                        app.current_tab().prompt_queue.entries[0].kind,
                        RequestKind::ManualFix
                    );
                    if completed {
                        super::super::tests::complete_autofix_capture(&mut app, "queue-tab");
                        assert_eq!(rx.try_recv().unwrap().images.len(), usize::from(with_image));
                        end(&mut app);
                    } else {
                        enter(&mut app, "/stop");
                    }
                    app.source_session_id = Some("retry-source".into());
                    app.handle_event(AppEvent::Key(KeyEvent::new(
                        KeyCode::Up,
                        KeyModifiers::NONE,
                    )));
                    assert_eq!(app.current_tab().input, input);
                    assert!(app.current_tab().attachments.is_empty());
                    app.handle_event(AppEvent::Key(KeyEvent::new(
                        KeyCode::Enter,
                        KeyModifiers::NONE,
                    )));
                    let entry = &app.current_tab().prompt_queue.entries[0];
                    assert_eq!(entry.kind, RequestKind::ManualFix);
                    assert!(entry.capturing);
                    assert!(rx.try_recv().is_err());
                    super::super::tests::complete_autofix_capture(&mut app, "queue-tab");
                    let replay = rx.try_recv().unwrap();
                    assert_eq!(replay.text, commands::parse(input).unwrap().rest);
                    assert_eq!(
                        replay.autofix_text_kind,
                        Some(crate::protocol::acp::client::AutofixTextKind::UserRequest)
                    );
                    assert_eq!(
                        replay.autofix_snapshot.unwrap().source_pane_id(),
                        "retry-source"
                    );
                    assert!(replay.images.is_empty());
                }
            }
        }
    }

    #[test]
    fn fifo_keeps_active_stream_contiguous_and_drains_only_at_terminal_boundary() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        enter(&mut app, "first");
        let first = rx.try_recv().unwrap();
        app.handle_event(AppEvent::AgentMessageChunk {
            session_id: "queue-session".into(),
            text: "active answer".into(),
        });
        enter(&mut app, "second");
        enter(&mut app, "third");
        assert_eq!(app.current_tab().turn.prompt_id(), Some(first.id));
        assert!(rx.try_recv().is_err());
        app.handle_event(AppEvent::Tick);
        app.handle_event(AppEvent::AgentMessageChunk {
            session_id: "queue-session".into(),
            text: " continues".into(),
        });
        assert_eq!(
            app.current_tab().active_agent_text(),
            "active answer continues"
        );
        assert_eq!(queued_info_count(&app), 2);
        assert!(rx.try_recv().is_err());
        end(&mut app);
        assert_eq!(rx.try_recv().unwrap().text, "second");
        assert_eq!(app.current_tab().completed_turns.len(), 1);
        end(&mut app);
        assert_eq!(rx.try_recv().unwrap().text, "third");
    }

    #[test]
    fn startup_queue_waits_for_connected_and_configuration() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        app.state = ConnectionState::Connecting("startup".into());
        enter(&mut app, "before connected");
        assert!(app.current_tab().turn.is_idle());
        app.state = ConnectionState::Connected;
        hold(&mut app);
        app.handle_event(AppEvent::Tick);
        assert!(rx.try_recv().is_err());
        release(&mut app);
        assert_eq!(rx.try_recv().unwrap().text, "before connected");
    }

    #[test]
    fn pending_previews_include_automatic_requests_without_mutating_chat_or_queue() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        hold(&mut app);
        for n in 1..=5 {
            enter(&mut app, &format!("request {n}"));
        }
        app.enqueue_autofix(
            "queue-tab",
            "source",
            "error\n\u{1b}[31m\u{202e} context",
            false,
        );
        let ids: Vec<_> = app
            .current_tab()
            .prompt_queue
            .entries
            .iter()
            .map(|entry| entry.submission.id)
            .collect();
        let tokens = pending_tokens(&app);
        app.current_tab_mut().input = "preserved draft".into();
        let messages = app.current_tab().messages.clone();
        let text = app.pending_input_previews().collect::<Vec<_>>().join("\n");
        assert!(text.contains("5. request 5"));
        assert!(text.contains(t!("queue.auto").as_ref()));
        assert!(!text.contains('\u{1b}') && !text.contains('\u{202e}'));
        assert_eq!(text.lines().count(), 6);
        assert_eq!(app.current_tab().messages, messages);
        assert_eq!(app.current_tab().input, "preserved draft");
        assert_eq!(
            ids,
            app.current_tab()
                .prompt_queue
                .entries
                .iter()
                .map(|entry| entry.submission.id)
                .collect::<Vec<_>>()
        );
        assert!(tokens.iter().all(|token| !token.is_cancelled()));
        assert!(rx.try_recv().is_err());
        assert_eq!(
            app.current_tab().prompt_queue.entries[0].preview(0),
            "1. request 1"
        );
        let item = &mut app.current_tab_mut().prompt_queue.entries[0];
        item.display_text = "x".repeat(1000);
        assert_eq!(item.preview(0).chars().count(), 123);
    }

    #[test]
    fn empty_queue_has_no_previews_and_empty_discard_is_silent() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        assert_eq!(app.pending_input_previews().count(), 0);
        let messages = app.current_tab().messages.clone();
        app.current_tab_mut().cancel_pending_prompts();
        assert_eq!(app.current_tab().messages, messages);
        enter(&mut app, "fresh input");
        assert_eq!(rx.try_recv().unwrap().text, "fresh input");
    }

    #[test]
    fn discard_cancels_only_pending_tokens_and_preserves_active_turn_and_draft() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        enter(&mut app, "active");
        let active = rx.try_recv().unwrap();
        enter(&mut app, "pending manual");
        app.enqueue_autofix("queue-tab", "source", "pending error", false);
        let tokens = pending_tokens(&app);
        let tab = app.current_tab_mut();
        tab.input = "draft ".into();
        tab.cursor_pos = tab.input.len();
        let image = crate::clipboard_image::PastedImage {
            data_base64: "aW1hZ2U=".into(),
            mime_type: "image/png".into(),
            label: "draft.png".into(),
        };
        tab.attachments
            .insert_image(&mut tab.input, &mut tab.cursor_pos, image.clone());
        let draft = tab.input.clone();
        tab.pending_queue_action = Some(active.id);
        tab.cancel_pending_prompts();
        assert!(tokens.iter().all(|token| token.is_cancelled()));
        assert!(!active.cancellation_token().is_cancelled());
        assert_eq!(app.current_tab().turn.prompt_id(), Some(active.id));
        assert_eq!(app.current_tab().pending_queue_action, Some(active.id));
        assert_eq!(app.current_tab().input, draft);
        assert_eq!(app.current_tab().attachments.images().next(), Some(&image));
        assert!(app.current_tab().prompt_queue.entries.is_empty());
        assert!(app.current_tab().prompt_queue.echoes.is_empty());
        assert!(has_cancelled_notice(&app, 2));
        let messages = app.current_tab().messages.clone();
        app.current_tab_mut().cancel_pending_prompts();
        assert_eq!(app.current_tab().messages, messages);
    }

    #[test]
    fn stop_discards_pending_requests_and_future_input_works_without_recovery_command() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        enter(&mut app, "active");
        rx.try_recv().unwrap();
        enter(&mut app, "discard me");
        let token = app.current_tab().prompt_queue.entries[0]
            .submission
            .cancellation_token();
        enter(&mut app, "/stop");
        assert!(token.is_cancelled());
        assert!(app.current_tab().prompt_queue.entries.is_empty());
        assert!(has_cancelled_notice(&app, 1));
        end(&mut app);
        assert!(rx.try_recv().is_err());
        enter(&mut app, "new explicit request");
        assert_eq!(rx.try_recv().unwrap().text, "new explicit request");
    }

    #[test]
    fn empty_stop_does_not_block_future_input() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        enter(&mut app, "/stop");
        assert!(!has_cancelled_notice(&app, 0));
        enter(&mut app, "next");
        assert_eq!(rx.try_recv().unwrap().text, "next");
    }

    #[test]
    fn ctrl_c_cancels_waiting_input_without_arming_pane_close() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        hold(&mut app);
        enter(&mut app, "waiting for configuration");
        let token = app.current_tab().prompt_queue.entries[0]
            .submission
            .cancellation_token();
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert!(token.is_cancelled());
        assert!(app.current_tab().prompt_queue.entries.is_empty());
        assert!(has_cancelled_notice(&app, 1));
        assert!(app.close_pane_armed_at.is_none());
        assert!(!app.should_quit);
        release(&mut app);
        assert!(rx.try_recv().is_err());
        enter(&mut app, "new request");
        assert_eq!(rx.try_recv().unwrap().text, "new request");
    }

    #[test]
    fn automatic_requests_coalesce_behind_manual_without_changing_active_generation() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        app.enqueue_autofix("queue-tab", "a", "old error", false);
        let old_id = app.current_tab().prompt_queue.entries[0].submission.id;
        let old_token = app.current_tab().prompt_queue.entries[0]
            .submission
            .cancellation_token();
        app.enqueue_autofix("queue-tab", "b", "other error", false);
        app.enqueue_autofix("queue-tab", "a", "new error", false);
        assert_eq!(queued_info_count(&app), 0);
        assert_eq!(
            app.pending_input_previews().collect::<Vec<_>>(),
            [
                format!("1. {} · new error", t!("queue.auto")),
                format!("2. {} · other error", t!("queue.auto")),
            ]
        );
        assert!(old_token.is_cancelled());
        assert_eq!(app.current_tab().prompt_queue.entries.len(), 2);
        assert_eq!(app.current_tab().autofix.generation, 0);
        assert_eq!(
            app.current_tab().prompt_queue.entries[0].submission.text,
            "new error"
        );
        app.autofix_snapshot_ready(old_id, Err("stale failure".into()));
        assert_eq!(app.current_tab().prompt_queue.entries.len(), 2);
        enter(&mut app, "human first");
        assert_eq!(rx.try_recv().unwrap().text, "human first");
        app.invalidate_pending_autofix("queue-tab", "a");
        assert_eq!(app.current_tab().prompt_queue.entries.len(), 1);
        assert!(app.current_tab().turn.is_in_flight());
    }

    #[test]
    fn forced_fix_supersedes_only_pending_automatic_work() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, _) = app();
        hold(&mut app);
        enter(&mut app, "manual request");
        app.enqueue_autofix("queue-tab", "source", "automatic failure", false);
        let automatic = app.current_tab().prompt_queue.entries[1]
            .submission
            .cancellation_token();
        app.enqueue_autofix("queue-tab", "source", "explicit failure", true);
        assert!(automatic.is_cancelled());
        assert_eq!(app.current_tab().prompt_queue.entries.len(), 2);
        let tokens = pending_tokens(&app);
        app.enqueue_autofix("queue-tab", "source", "another explicit request", true);
        assert_eq!(app.current_tab().prompt_queue.entries.len(), 3);
        assert!(tokens.iter().all(|token| !token.is_cancelled()));
        assert!(!has_cancelled_notice(&app, 2));
    }

    #[test]
    fn queued_manual_fix_preserves_images_and_user_intent() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        hold(&mut app);
        app.source_session_id = Some("original-source".into());
        app.source_cwd = Some(r"C:\original".into());
        let image = crate::clipboard_image::PastedImage {
            data_base64: "aW1hZ2U=".into(),
            mime_type: "image/png".into(),
            label: "sample.png".into(),
        };
        let tab = app.current_tab_mut();
        tab.input = "/fix investigate ".into();
        tab.cursor_pos = tab.input.len();
        tab.attachments
            .insert_image(&mut tab.input, &mut tab.cursor_pos, image.clone());
        app.enqueue_input(Some("investigate".into()));
        assert!(rx.try_recv().is_err());
        let entry = &app.current_tab().prompt_queue.entries[0];
        assert!(entry.capturing);
        let request_id = entry.submission.id;
        let snapshot = crate::protocol::acp::client::AutofixSnapshot::for_test("original-source");
        let frozen = format!("{snapshot:?}");
        app.autofix_snapshot_ready(request_id, Ok(snapshot));
        app.source_session_id = Some("different-focused-pane".into());
        app.source_cwd = Some(r"C:\later".into());
        release(&mut app);
        let request = rx.try_recv().unwrap();
        assert_eq!(format!("{:?}", request.autofix_snapshot.unwrap()), frozen);
        let context = request.pane_context.unwrap();
        assert_eq!(context.source_pane_id.as_deref(), Some("original-source"));
        assert_eq!(context.cwd.as_deref(), Some(r"C:\original"));
        assert_eq!(
            request.autofix_text_kind,
            Some(crate::protocol::acp::client::AutofixTextKind::UserRequest)
        );
        assert_eq!(request.images, vec![image]);
        assert_eq!(request.text.trim(), "investigate");
        assert!(!request.text.contains("[image:"));
    }

    #[test]
    fn typed_fixes_capture_before_dispatch_and_preserve_fifo_without_text_deduplication() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        app.source_session_id = Some("source".into());
        enter(&mut app, "/fix investigate");
        enter(&mut app, "/fix investigate");
        let ids: Vec<_> = app
            .current_tab()
            .prompt_queue
            .entries
            .iter()
            .map(|entry| entry.submission.id)
            .collect();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
        assert!(app.current_tab().turn.is_idle());
        assert!(rx.try_recv().is_err());
        app.handle_event(AppEvent::AutofixSnapshotReady {
            request_id: ids[1],
            result: Ok(crate::protocol::acp::client::AutofixSnapshot::for_test(
                "source",
            )),
        });
        assert!(
            rx.try_recv().is_err(),
            "a later capture cannot overtake the head"
        );
        app.handle_event(AppEvent::AutofixSnapshotReady {
            request_id: ids[0],
            result: Ok(crate::protocol::acp::client::AutofixSnapshot::for_test(
                "source",
            )),
        });
        assert_eq!(rx.try_recv().unwrap().id, ids[0]);
        end(&mut app);
        assert_eq!(rx.try_recv().unwrap().id, ids[1]);
    }

    #[test]
    fn typed_fix_reserves_snapshot_budget_and_preserves_rejected_draft_and_images() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        hold(&mut app);
        enter(&mut app, "existing");
        let existing = &mut app.current_tab_mut().prompt_queue.entries[0];
        let padding = MAX_PAYLOAD_BYTES - SNAPSHOT_RESERVATION / 2 - existing.bytes();
        existing.display_text.push_str(&"x".repeat(padding));
        app.source_session_id = Some("source".into());
        let image = crate::clipboard_image::PastedImage {
            data_base64: "aW1hZ2U=".into(),
            mime_type: "image/png".into(),
            label: "sample.png".into(),
        };
        let tab = app.current_tab_mut();
        tab.input = "/fix investigate ".into();
        tab.cursor_pos = tab.input.len();
        tab.attachments
            .insert_image(&mut tab.input, &mut tab.cursor_pos, image.clone());
        let draft = tab.input.clone();
        let cursor = tab.cursor_pos;
        app.enqueue_input(Some("investigate".into()));
        assert_eq!(app.current_tab().input, draft);
        assert_eq!(app.current_tab().cursor_pos, cursor);
        assert_eq!(app.current_tab().attachments.images().next(), Some(&image));
        assert_eq!(app.current_tab().prompt_queue.entries.len(), 1);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn forced_promotion_admission_excludes_replaced_automatic_at_item_and_byte_limits() {
        let _locale = crate::test_support::lock_locale();
        for byte_limit in [false, true] {
            let (mut app, mut rx) = app();
            hold(&mut app);
            let manual_count = if byte_limit { 1 } else { MAX_REQUESTS - 1 };
            for n in 0..manual_count {
                enter(&mut app, &format!("manual {n}"));
            }
            app.enqueue_autofix("queue-tab", "source", "failure", false);
            let automatic = &app.current_tab().prompt_queue.entries[manual_count];
            let stale_id = automatic.submission.id;
            let cancelled = automatic.submission.cancellation_token();
            if byte_limit {
                let bytes: usize = app
                    .current_tab()
                    .prompt_queue
                    .entries
                    .iter()
                    .map(QueuedRequest::bytes)
                    .sum();
                app.current_tab_mut().prompt_queue.entries[0]
                    .display_text
                    .push_str(&"x".repeat(MAX_PAYLOAD_BYTES - bytes));
            }
            app.handle_autofix_execute_from_detected("source", Some("queue-tab"));
            assert!(cancelled.is_cancelled());
            assert_eq!(
                app.current_tab().prompt_queue.entries.len(),
                manual_count + 1
            );
            let promoted = &app.current_tab().prompt_queue.entries[manual_count];
            assert_eq!(promoted.kind, RequestKind::ManualFix);
            assert_ne!(promoted.submission.id, stale_id);
            let promoted_id = promoted.submission.id;
            app.handle_autofix_execute_from_detected("source", Some("queue-tab"));
            assert_eq!(
                app.current_tab().prompt_queue.entries.len(),
                manual_count + 1
            );
            app.autofix_snapshot_ready(stale_id, Err("obsolete capture".into()));
            assert_eq!(
                app.current_tab().prompt_queue.entries[manual_count]
                    .submission
                    .id,
                promoted_id
            );
            assert!(!app.current_tab().prompt_queue.entries[manual_count]
                .submission
                .cancellation_token()
                .is_cancelled());
            assert!(rx.try_recv().is_err());
        }
    }

    #[test]
    fn rejected_promotion_preserves_automatic_and_remains_retryable() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, _) = app();
        hold(&mut app);
        enter(&mut app, "manual");
        app.enqueue_autofix("queue-tab", "source", "failure", false);
        let automatic = &app.current_tab().prompt_queue.entries[1];
        let automatic_id = automatic.submission.id;
        let token = automatic.submission.cancellation_token();
        let bytes: usize = app
            .current_tab()
            .prompt_queue
            .entries
            .iter()
            .map(QueuedRequest::bytes)
            .sum();
        app.current_tab_mut().prompt_queue.entries[0]
            .display_text
            .push_str(&"x".repeat(MAX_PAYLOAD_BYTES - bytes));
        app.emit_autofix_state_detected("queue-tab", "source", "a larger failure diagnostic");
        app.handle_autofix_execute_from_detected("source", Some("queue-tab"));
        assert_eq!(
            app.current_tab().prompt_queue.entries[1].submission.id,
            automatic_id
        );
        assert!(!token.is_cancelled());
        assert!(app.current_tab().autofix.detected_request_id.is_none());
        app.current_tab_mut().prompt_queue.entries[0].display_text = "manual".into();
        app.handle_autofix_execute_from_detected("source", Some("queue-tab"));
        assert!(token.is_cancelled());
        assert_eq!(app.current_tab().prompt_queue.entries.len(), 2);
        assert_eq!(
            app.current_tab().prompt_queue.entries[1].kind,
            RequestKind::ManualFix
        );
    }

    #[test]
    fn typed_fix_never_promotes_automatic_work_to_bypass_the_item_limit() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, _) = app();
        hold(&mut app);
        for _ in 0..MAX_REQUESTS - 1 {
            enter(&mut app, "manual");
        }
        app.source_session_id = Some("source".into());
        app.enqueue_autofix("queue-tab", "source", "failure", false);
        let token = app.current_tab().prompt_queue.entries[MAX_REQUESTS - 1]
            .submission
            .cancellation_token();
        enter(&mut app, "/fix investigate");
        assert_eq!(app.current_tab().input, "/fix investigate");
        assert_eq!(app.current_tab().prompt_queue.entries.len(), MAX_REQUESTS);
        assert_eq!(
            app.current_tab().prompt_queue.entries[MAX_REQUESTS - 1].kind,
            RequestKind::AutomaticFix
        );
        assert!(!token.is_cancelled());
    }

    #[test]
    fn typed_fix_late_capture_cannot_restore_cancelled_invalidated_or_reset_requests() {
        let _locale = crate::test_support::lock_locale();
        for invalidation in ["cancel", "source", "reset", "failure"] {
            let (mut app, mut rx) = app();
            app.source_session_id = Some("source".into());
            enter(&mut app, "/fix investigate");
            let entry = &app.current_tab().prompt_queue.entries[0];
            let id = entry.submission.id;
            let token = entry.submission.cancellation_token();
            match invalidation {
                "cancel" => app.current_tab_mut().cancel_pending_prompts(),
                "source" => app.invalidate_pending_autofix("queue-tab", "source"),
                "reset" => app.invalidate_prompt_queue_sessions(),
                _ => app.autofix_snapshot_ready(id, Err("capture failed".into())),
            }
            assert!(token.is_cancelled());
            for result in [
                Ok(crate::protocol::acp::client::AutofixSnapshot::for_test(
                    "source",
                )),
                Err("late failure".into()),
            ] {
                app.handle_event(AppEvent::AutofixSnapshotReady {
                    request_id: id,
                    result,
                });
            }
            assert!(app.current_tab().prompt_queue.entries.is_empty());
            assert!(app.current_tab().turn.is_idle());
            assert!(rx.try_recv().is_err());
            enter(&mut app, "fresh");
            assert_eq!(rx.try_recv().unwrap().text, "fresh");
        }
    }

    #[test]
    fn typed_fix_capture_survives_tab_rename_without_rebinding_source() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        app.owner_tab_id = Some("queue-tab".into());
        app.source_session_id = Some("original-source".into());
        enter(&mut app, "/fix investigate");
        let id = app.current_tab().prompt_queue.entries[0].submission.id;
        app.rename_tab_session("queue-tab", "renamed", Some("new-window"));
        app.source_session_id = Some("other-source".into());
        app.handle_event(AppEvent::AutofixSnapshotReady {
            request_id: id,
            result: Ok(crate::protocol::acp::client::AutofixSnapshot::for_test(
                "original-source",
            )),
        });
        let request = rx.try_recv().unwrap();
        assert_eq!(request.id, id);
        assert!(request.autofix_snapshot.is_some());
        let context = request.pane_context.unwrap();
        assert_eq!(context.tab_id.as_deref(), Some("renamed"));
        assert_eq!(context.window_id.as_deref(), Some("new-window"));
        assert_eq!(context.source_pane_id.as_deref(), Some("original-source"));
    }

    #[test]
    fn bounds_reject_without_losing_text_or_attachments() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, _) = app();
        hold(&mut app);
        for n in 0..MAX_REQUESTS {
            enter(&mut app, &format!("request {n}"));
        }
        enter(&mut app, "keep this");
        assert_eq!(queued_info_count(&app), MAX_REQUESTS);
        assert_eq!(app.current_tab().input, "keep this");
        assert_eq!(app.current_tab().prompt_queue.entries.len(), MAX_REQUESTS);
        app.current_tab_mut().cancel_pending_prompts();
        let tab = app.current_tab_mut();
        tab.clear_input();
        let image = crate::clipboard_image::PastedImage {
            data_base64: "x".repeat(MAX_PAYLOAD_BYTES),
            mime_type: "image/png".into(),
            label: "large.png".into(),
        };
        let payload = image.data_base64.as_ptr();
        tab.attachments
            .insert_image(&mut tab.input, &mut tab.cursor_pos, image);
        let draft = tab.input.clone();
        let cursor = tab.cursor_pos;
        let ranges = tab.attachments.token_ranges().collect::<Vec<_>>();
        app.enqueue_input(None);
        assert_eq!(app.current_tab().input, draft);
        assert_eq!(app.current_tab().cursor_pos, cursor);
        assert_eq!(
            app.current_tab()
                .attachments
                .token_ranges()
                .collect::<Vec<_>>(),
            ranges
        );
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
    }

    #[test]
    fn borrowed_attachment_preflight_enforces_exact_byte_limit() {
        let _locale = crate::test_support::lock_locale();
        for overflow in [0, 1] {
            let (mut app, _) = app();
            hold(&mut app);
            let set_draft = |app: &mut App, size: usize| {
                let tab = app.current_tab_mut();
                tab.clear_input();
                tab.input = "inspect ".into();
                tab.cursor_pos = tab.input.len();
                let image = crate::clipboard_image::PastedImage {
                    data_base64: "x".repeat(size),
                    mime_type: "image/png".into(),
                    label: "sample.png".into(),
                };
                tab.attachments
                    .insert_image(&mut tab.input, &mut tab.cursor_pos, image);
            };
            set_draft(&mut app, 1);
            app.enqueue_input(None);
            let overhead = app.current_tab().prompt_queue.entries[0].bytes() - 1;
            app.current_tab_mut().cancel_pending_prompts();
            set_draft(&mut app, MAX_PAYLOAD_BYTES - overhead + overflow);
            let draft = app.current_tab().input.clone();
            let payload = app
                .current_tab()
                .attachments
                .images()
                .next()
                .unwrap()
                .data_base64
                .as_ptr();
            app.enqueue_input(None);
            let tab = app.current_tab();
            if overflow == 0 {
                assert_eq!(tab.prompt_queue.entries[0].bytes(), MAX_PAYLOAD_BYTES);
                assert_eq!(
                    tab.prompt_queue.entries[0].submission.images[0]
                        .data_base64
                        .as_ptr(),
                    payload
                );
                assert!(tab.input.is_empty());
                assert!(tab.attachments.is_empty());
            } else {
                assert!(tab.prompt_queue.entries.is_empty());
                assert_eq!(tab.input, draft);
                assert_eq!(
                    tab.attachments
                        .images()
                        .next()
                        .unwrap()
                        .data_base64
                        .as_ptr(),
                    payload
                );
            }
        }
    }

    #[test]
    fn session_changes_block_and_session_invalidation_discards_pending_work() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        hold(&mut app);
        enter(&mut app, "manual");
        app.enqueue_autofix("queue-tab", "source", "error", false);
        assert!(app.queue_blocks_session_change());
        app.cmd_clear();
        assert_eq!(app.current_tab().prompt_queue.entries.len(), 2);
        let tokens = pending_tokens(&app);
        app.current_tab_mut().pending_queue_action = Some(71);
        app.invalidate_prompt_queue_sessions();
        assert!(app.current_tab().prompt_queue.entries.is_empty());
        assert!(app.current_tab().pending_queue_action.is_none());
        assert!(tokens.iter().all(|token| token.is_cancelled()));
        assert!(has_cancelled_notice(&app, 2));
        assert!(!app.queue_blocks_session_change());
        release(&mut app);
        assert!(rx.try_recv().is_err());
        enter(&mut app, "new session request");
        assert_eq!(rx.try_recv().unwrap().text, "new session request");
    }

    #[test]
    fn action_handoff_still_blocks_dispatch_and_explicit_session_changes() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        app.current_tab_mut().pending_queue_action = Some(72);
        enter(&mut app, "after action");
        assert!(app.queue_blocks_session_change());
        assert!(rx.try_recv().is_err());
        app.current_tab_mut().pending_queue_action = None;
        app.dispatch_prompt_queues();
        assert_eq!(rx.try_recv().unwrap().text, "after action");
    }

    #[test]
    fn rename_updates_routing_but_never_changes_pinned_source() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        app.owner_tab_id = Some("queue-tab".into());
        app.source_session_id = Some("source-original".into());
        hold(&mut app);
        enter(&mut app, "queued");
        app.rename_tab_session("queue-tab", "renamed", Some("new-window"));
        app.source_session_id = Some("different-focus".into());
        release(&mut app);
        let context = rx.try_recv().unwrap().pane_context.unwrap();
        assert_eq!(context.tab_id.as_deref(), Some("renamed"));
        assert_eq!(context.window_id.as_deref(), Some("new-window"));
        assert_eq!(context.source_pane_id.as_deref(), Some("source-original"));
    }

    #[test]
    fn capture_ready_follows_request_identity_across_rename_and_replacement() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        app.owner_tab_id = Some("queue-tab".into());
        app.enqueue_autofix("queue-tab", "source", "old", false);
        let stale_id = app.current_tab().prompt_queue.entries[0].submission.id;
        app.enqueue_autofix("queue-tab", "source", "latest", false);
        let id = app.current_tab().prompt_queue.entries[0].submission.id;
        app.rename_tab_session("queue-tab", "renamed", Some("window"));
        app.handle_event(AppEvent::AutofixSnapshotReady {
            request_id: stale_id,
            result: Ok(crate::protocol::acp::client::AutofixSnapshot::for_test(
                "source",
            )),
        });
        assert!(rx.try_recv().is_err());
        app.handle_event(AppEvent::AutofixSnapshotReady {
            request_id: id,
            result: Ok(crate::protocol::acp::client::AutofixSnapshot::for_test(
                "source",
            )),
        });
        let request = rx.try_recv().unwrap();
        assert_eq!(request.id, id);
        assert_eq!(request.text, "latest");
        assert!(request.autofix_snapshot.is_some());
        assert_eq!(
            request.pane_context.unwrap().tab_id.as_deref(),
            Some("renamed")
        );
    }

    #[test]
    fn automatic_capture_failure_does_not_discard_unrelated_user_work() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        hold(&mut app);
        enter(&mut app, "manual");
        app.enqueue_autofix("queue-tab", "source", "error", false);
        let id = app.current_tab().prompt_queue.entries[1].submission.id;
        app.handle_event(AppEvent::AutofixSnapshotReady {
            request_id: id,
            result: Err("capture failed".into()),
        });
        assert_eq!(app.current_tab().prompt_queue.entries.len(), 1);
        assert!(!has_cancelled_notice(&app, 1));
        assert!(matches!(
            app.current_tab().messages.last(),
            Some(ChatMessage::Notice {
                kind: NoticeKind::Warning,
                ..
            })
        ));
        release(&mut app);
        assert_eq!(rx.try_recv().unwrap().text, "manual");
    }

    #[test]
    fn explicit_capture_failure_discards_pending_work_and_allows_fresh_input() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        hold(&mut app);
        app.enqueue_autofix("queue-tab", "source", "explicit error", true);
        let id = app.current_tab().prompt_queue.entries[0].submission.id;
        enter(&mut app, "queued next");
        let tokens = pending_tokens(&app);
        app.handle_event(AppEvent::AutofixSnapshotReady {
            request_id: id,
            result: Err("capture failed".into()),
        });
        assert!(tokens.iter().all(|token| token.is_cancelled()));
        assert!(app.current_tab().prompt_queue.entries.is_empty());
        assert!(has_cancelled_notice(&app, 2));
        release(&mut app);
        assert!(rx.try_recv().is_err());
        enter(&mut app, "fresh");
        assert_eq!(rx.try_recv().unwrap().text, "fresh");
    }

    #[test]
    fn explicit_capture_context_expiry_discards_pending_and_ignores_late_snapshot() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        hold(&mut app);
        app.enqueue_autofix("queue-tab", "source", "old failure", true);
        let id = app.current_tab().prompt_queue.entries[0].submission.id;
        enter(&mut app, "queued next");
        app.invalidate_pending_autofix("queue-tab", "source");
        assert!(has_cancelled_notice(&app, 2));
        app.handle_event(AppEvent::AutofixSnapshotReady {
            request_id: id,
            result: Ok(crate::protocol::acp::client::AutofixSnapshot::for_test(
                "source",
            )),
        });
        assert!(app.current_tab().prompt_queue.entries.is_empty());
        let changed = t!("queue.snapshot_context_changed", pane = "source");
        assert!(app.current_tab().messages.iter().any(|message| {
            matches!(message, ChatMessage::Notice { text, .. } if text.contains(changed.as_ref()))
        }));
        release(&mut app);
        assert!(rx.try_recv().is_err());
        enter(&mut app, "fresh request");
        assert_eq!(rx.try_recv().unwrap().text, "fresh request");
    }

    #[test]
    fn per_pane_echoes_and_prompt_end_preserve_pending_failures() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, _) = app();
        app.autofix_enabled = true;
        hold(&mut app);
        enter(&mut app, "unrelated manual");
        let event = |pane: &str, sequence: &str| AppEvent::WtEvent {
            method: "vt_sequence".into(),
            pane_id: pane.into(),
            tab_id: Some("queue-tab".into()),
            params: serde_json::json!({ "session_id": pane, "sequence": sequence }),
        };
        app.handle_event(event("pane-a", "osc:133;D;1"));
        app.handle_event(event("pane-b", "osc:133;D;2"));
        app.handle_event(event("pane-a", "osc:133;A"));
        app.handle_event(event("pane-b", "osc:133;A"));
        app.handle_event(event("pane-a", "osc:133;B"));
        app.handle_event(event("pane-b", "osc:133;B"));
        assert_eq!(app.current_tab().prompt_queue.entries.len(), 3);
        app.handle_event(event("pane-a", "osc:133;C"));
        assert_eq!(app.current_tab().prompt_queue.entries.len(), 2);
        app.handle_event(event("pane-b", "osc:133;D;0"));
        assert_eq!(app.current_tab().prompt_queue.entries.len(), 1);
        assert_eq!(
            app.current_tab().prompt_queue.entries[0].submission.text,
            "unrelated manual"
        );
        assert!(!has_cancelled_notice(&app, 1));
    }

    #[test]
    fn shell_prompt_end_preserves_active_autofix_but_command_start_cancels_it() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        app.autofix_enabled = true;
        let event = |sequence: &str| AppEvent::WtEvent {
            method: "vt_sequence".into(),
            pane_id: "source".into(),
            tab_id: Some("queue-tab".into()),
            params: serde_json::json!({"sequence": sequence}),
        };
        app.handle_event(event("osc:133;D;1"));
        super::super::tests::complete_autofix_capture(&mut app, "queue-tab");
        let request = rx.try_recv().unwrap();
        app.handle_event(event("osc:133;A"));
        app.handle_event(event("osc:133;B"));
        assert_eq!(app.current_tab().turn.prompt_id(), Some(request.id));
        assert!(!request.cancellation_token().is_cancelled());
        app.handle_event(event("osc:133;C"));
        assert!(request.cancellation_token().is_cancelled());
        assert!(app.current_tab().turn.is_cancelling());
    }

    #[test]
    fn permission_responder_is_preserved_while_input_waits() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        let (sender, mut receiver) = tokio::sync::oneshot::channel();
        app.current_tab_mut().permission.push_back(PermissionState {
            tool_call_id: "permission".into(),
            description: "allow?".into(),
            title: "tool".into(),
            kind_label: None,
            target: None,
            target_is_command: false,
            options: Vec::new(),
            selected: 0,
            responder: Some(sender),
        });
        app.current_tab_mut().input = "waiting input".into();
        app.enqueue_input(None);
        assert!(rx.try_recv().is_err());
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        let mut permission = app.current_tab_mut().permission.pop_front().unwrap();
        permission
            .responder
            .take()
            .unwrap()
            .send("allow".into())
            .unwrap();
        app.handle_event(AppEvent::Tick);
        assert_eq!(rx.try_recv().unwrap().text, "waiting input");
        assert_eq!(receiver.try_recv().unwrap(), "allow");
    }

    #[test]
    fn manual_fix_is_never_cancelled_by_background_auto_invalidation() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        app.source_session_id = Some("source".into());
        app.cmd_fix(false, "manual investigation".into());
        super::super::tests::complete_autofix_capture(&mut app, "queue-tab");
        let manual = rx.try_recv().unwrap();
        app.enqueue_autofix("queue-tab", "source", "background failure", false);
        app.handle_autofix_pane_closed(Some("queue-tab"), "source");
        assert_eq!(app.current_tab().turn.prompt_id(), Some(manual.id));
        assert!(!manual.cancellation_token().is_cancelled());
    }

    #[test]
    fn errors_and_soft_stops_discard_pending_without_replaying_active_work() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        enter(&mut app, "first");
        let first = rx.try_recv().unwrap();
        app.handle_event(AppEvent::AgentMessageChunk {
            session_id: "queue-session".into(),
            text: "partial answer".into(),
        });
        enter(&mut app, "discard after soft stop");
        app.handle_event(AppEvent::AgentSoftStop {
            session_id: "queue-session".into(),
            reason: crate::protocol::acp::soft_stop::SoftStopReason::MaxTokens,
        });
        assert!(has_cancelled_notice(&app, 1));
        end(&mut app);
        assert!(rx.try_recv().is_err());
        assert!(app.current_tab().prompt_queue.entries.is_empty());
        assert!(app.current_tab().completed_turns[0]
            .details
            .iter()
            .any(|message| {
                matches!(message, ChatMessage::Notice { kind: NoticeKind::Warning, text }
                if text == t!("system.stopped_max_tokens").as_ref())
            }));
        enter(&mut app, "fresh after soft stop");
        let second = rx.try_recv().unwrap();
        assert_ne!(first.id, second.id);
        enter(&mut app, "discard after error");
        app.handle_event(AppEvent::PromptError {
            tab_id: "queue-tab".into(),
            prompt_id: second.id,
            message: "transport failed".into(),
        });
        assert!(app.current_tab().prompt_queue.entries.is_empty());
        assert!(rx.try_recv().is_err());
        enter(&mut app, "fresh after failure");
        assert_eq!(rx.try_recv().unwrap().text, "fresh after failure");
    }

    #[test]
    fn closed_local_channel_discards_unsent_work_and_preserves_history_and_draft() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = app();
        enter(&mut app, "completed request");
        rx.try_recv().unwrap();
        app.handle_event(AppEvent::AgentMessageChunk {
            session_id: "queue-session".into(),
            text: "completed answer".into(),
        });
        end(&mut app);
        let history = app.current_tab().completed_turns.clone();
        let turn_id = app.current_tab().turn.prompt_id();
        app.current_tab_mut()
            .messages
            .push(ChatMessage::Agent("prior transcript".into()));
        hold(&mut app);
        enter(&mut app, "unsent first");
        enter(&mut app, "unsent second");
        let tokens = pending_tokens(&app);
        app.current_tab_mut().input = "preserved draft".into();
        drop(rx);
        release(&mut app);
        assert!(tokens.iter().all(|token| token.is_cancelled()));
        assert_eq!(app.current_tab().completed_turns, history);
        assert_eq!(app.current_tab().turn.prompt_id(), turn_id);
        assert!(app.current_tab().turn.accepts_new_prompt());
        assert!(app.current_tab().messages.iter().any(|message| {
            matches!(message, ChatMessage::Agent(text) if text == "prior transcript")
        }));
        assert_eq!(app.current_tab().input, "preserved draft");
        assert!(app.current_tab().prompt_queue.entries.is_empty());
        assert!(has_cancelled_notice(&app, 2));
        let notices = queued_info_count(&app);
        enter(&mut app, "also unsent");
        assert_eq!(queued_info_count(&app), notices);
        assert!(app.current_tab().turn.accepts_new_prompt());
        assert!(app.current_tab().prompt_queue.entries.is_empty());
        let (tx, mut rx) = mpsc::unbounded_channel();
        app.prompt_tx = tx;
        enter(&mut app, "fresh after reconnect");
        let request = rx.try_recv().unwrap();
        assert_eq!(request.text, "fresh after reconnect");
        assert!(
            rx.try_recv().is_err(),
            "discarded requests must not be retried"
        );
    }
}
