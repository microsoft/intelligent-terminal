//! `App::handle_event` — the central `AppEvent` dispatch match, split out of
//! the large `app.rs` file into its own child module of `app` (via
//! `#[path]`) so it can reach `App`'s private fields and helper methods just
//! like the rest of `app.rs` does. `handle_event` is `pub(super)` because
//! `App::run` (in `app.rs`) and the test helpers in sibling test modules
//! (`app_tests.rs`, `autofix_tests.rs`) all call it directly on an `App`.

use super::*;

impl App {
    pub(super) fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Key(key) => self.handle_key(key),
            AppEvent::AgentPasteTextReady {
                tab_id,
                generation,
                text,
            } => {
                self.insert_agent_paste_text(&tab_id, generation, &text);
            }
            AppEvent::AgentPasteTextFailed {
                tab_id,
                generation,
                error,
            } => {
                if let Some(tab) = self.tab_sessions.get_mut(&tab_id) {
                    if tab.paste_generation != generation {
                        return;
                    }
                    tab.paste_pending = false;
                }
                tracing::warn!(
                    target: "agent_paste",
                    tab_id = %tab_id,
                    error = %error,
                    "failed to read text from clipboard"
                );
            }
            AppEvent::Tick => {
                // Fan out across all tabs: a background tab with an in-flight
                // prompt should keep its shimmer phase advancing so when the
                // user switches back the animation is in step.
                for tab in self.tab_sessions.values_mut() {
                    if tab.turn.spinner_label().is_some() || tab.progress_status.is_some() {
                        tab.activity_frame =
                            (tab.activity_frame + 1) % crate::ui::ACTIVITY_CYCLE_FRAMES;
                    }
                }
                // Setup-mode spinner: ticks while we're showing the wizard
                // (e.g. spinning during a `winget install` background job).
                // Also advance while the agents view waits on its first
                // session/list snapshot so the "Loading" shimmer keeps animating.
                if self.mode == AppMode::Setup
                    || self.mode == AppMode::Auth
                    || self.agents_view_awaiting_snapshot()
                    // Keep the connecting indicator animating during the
                    // pipe-connect → ACP init → session/new handshake so a cold
                    // start (which can run tens of seconds) doesn't look frozen
                    // (F7). Without this the chat sat static with no progress.
                    || matches!(self.state, ConnectionState::Connecting(_))
                {
                    self.activity_frame = self.activity_frame.wrapping_add(1);
                }
                // Age and auto-dismiss notifications
                for n in self.wt_notifications.iter_mut() {
                    n.age_ticks = n.age_ticks.saturating_add(1);
                }
                self.wt_notifications.retain(|n| !n.should_auto_dismiss());
                if self.wt_notifications.is_empty()
                    || self.wt_notifications.iter().all(|n| n.acknowledged)
                {
                    self.show_notification_banner = false;
                }
            }
            AppEvent::Resize(w, h) => {
                self.terminal_cols = w;
                self.terminal_rows = h;
            }
            AppEvent::RevealTick => {
                self.advance_reveal();
            }
            AppEvent::FocusChanged(focused) => {
                self.pane_focused = focused;
            }
            AppEvent::ConnectionStage(stage) => {
                self.state = ConnectionState::Connecting(stage);
                self.publish_agent_status();
            }
            AppEvent::ProgressStatus { session_id, status } => {
                let tab = match session_id {
                    Some(sid) => self.session_tab_mut(&sid),
                    None => self.current_tab_mut(),
                };
                tab.progress_status = Some(status);
                tab.scroll_to_bottom();
            }
            AppEvent::AgentConnected {
                name,
                model,
                version,
                session_id,
                available_models,
                current_model_id,
                load_session_supported,
                image_supported,
            } => {
                self.agent_name = name;
                self.agent_model = model;
                self.agent_version = version;
                self.session_id = session_id.clone();
                self.available_models = available_models.clone();
                self.current_model_id = current_model_id.clone();
                self.agent_supports_load_session = load_session_supported;
                self.agent_supports_image = image_supported;
                self.state = ConnectionState::Connected;
                // A successful connect resolves any in-flight auth recovery:
                // bump the generation so a still-pending dead-man timer becomes
                // stale and can't later force the sign-in screen.
                self.auth_recovery_generation = self.auth_recovery_generation.wrapping_add(1);
                // A live connection cancels the degraded latch (e.g. the
                // post-sign-in reconnect that goes back through master).
                self.transport_lost = false;
                self.preflight_setup_active = false;
                // If we were in Setup (e.g. after Retry), transition to Chat
                if self.mode == AppMode::Setup {
                    self.mode = AppMode::Chat;
                    self.setup = None;
                }
                // Show welcome hint on first-ever connect (persisted in state.json).
                // The disclaimer card is pushed as a `ChatMessage::Disclaimer`
                // on every agent-pane startup that lands on an empty chat (no
                // prior completed turns and no other in-flight messages), so
                // a session restored with history doesn't get a disclaimer
                // injected on top. Once shown it's allowed to be cleared by
                // a subsequent turn — the next startup re-pushes it.
                if !welcome_shown_in_state() {
                    self.show_welcome_hint = true;
                }
                // Bind the startup session to whichever tab we own.
                let bind_tab = self
                    .tab_id
                    .clone()
                    .unwrap_or_else(|| DEFAULT_TAB_ID.to_string());
                self.session_to_tab
                    .insert(session_id.clone(), bind_tab.clone());
                let tab = self.tab_mut(&bind_tab);
                tab.session_id = Some(session_id);
                let has_real_content = !tab.completed_turns.is_empty()
                    || tab
                        .messages
                        .iter()
                        .any(|m| !matches!(m, ChatMessage::Disclaimer));
                if !has_real_content
                    && !tab
                        .messages
                        .iter()
                        .any(|m| matches!(m, ChatMessage::Disclaimer))
                {
                    tab.messages.insert(0, ChatMessage::Disclaimer);
                }
                self.publish_agent_status();
            }
            AppEvent::SessionAttached {
                tab_id,
                session_id,
                available_models,
                current_model_id,
            } => {
                self.session_to_tab
                    .insert(session_id.clone(), tab_id.clone());
                let tab = self.tab_mut(&tab_id);
                tab.session_id = Some(session_id.clone());
                // Close the session/load replay window only when this
                // attach is for the session we asked to load. An
                // unrelated `SessionAttached` (e.g. the bootstrap
                // `session/new` that runs once at helper startup, which
                // can race against a Plan-C `--initial-load-session-id`
                // load_session queued at boot) would otherwise flip
                // `loading_session` off prematurely and the agent's
                // replay chunks would hit the chunk handlers'
                // `if !loading_session { return; }` gate and be
                // dropped.
                let is_load_target = tab
                    .loading_target_session_id
                    .as_deref()
                    .map(|t| t == session_id.as_str())
                    .unwrap_or(false);
                if tab.loading_session && is_load_target {
                    tab.flush_load_replay_pending();
                    tab.pack_replayed_messages_into_turns();
                    tab.loading_session = false;
                    tab.loading_target_session_id = None;
                    tab.scroll_to_bottom();
                }
                // Per-session model lists could differ — surface the new
                // tab's models when the agent_status event publishes for
                // this session in the future. For now we keep
                // App.available_models pointing at the active session's
                // models so the existing settings UI stays correct.
                if !available_models.is_empty() {
                    self.available_models = available_models;
                }
                if current_model_id.is_some() {
                    self.current_model_id = current_model_id;
                }
                // Keep freshly-created sessions on the effective model for
                // this tab — its per-pane `/model` override if set, else the
                // global acp-model. A resumed (loaded) session keeps whatever
                // model it was saved with; only fresh `/new` and lazy-first-
                // prompt sessions adopt the override. This is what makes a
                // local `/model` pick survive `/new`. The bootstrap session is
                // already model-applied by the client at startup.
                if !is_load_target {
                    if let Some(model) = self.effective_model_for_tab(&tab_id) {
                        self.send_session_model(Some(session_id.clone()), model);
                    }
                }
                self.publish_agent_status();
            }
            AppEvent::TabError { tab_id, message } => {
                // Scoped error for a specific tab. Bypasses the global
                // auth-fallback / ConnectionState::Failed flip in
                // AgentError because the error is local to one tab's
                // session-load attempt, not the whole connection.
                let tab = self.tab_mut(&tab_id);
                tab.loading_session = false;
                tab.loading_target_session_id = None;
                tab.progress_status = None;
                tab.pending_agent_response.clear();
                tab.pending_user_replay.clear();
                tab.timing_note = None;
                tab.turn = TurnState::Idle;
                tab.messages.push(ChatMessage::Error(message));
                tab.scroll_to_bottom();
            }
            AppEvent::TabSystemMessage { tab_id, message } => {
                let tab = self.tab_mut(&tab_id);
                tab.messages.push(ChatMessage::System(message));
                tab.scroll_to_bottom();
            }
            AppEvent::PromptTemplateLoaded { name } => {
                self.prompt_name = Some(name);
            }
            AppEvent::AutofixTargetResolved {
                tab_id,
                prompt_id,
                pane_id,
            } => {
                self.apply_autofix_target_resolved(tab_id, prompt_id, pane_id);
            }
            AppEvent::AgentBusy { tab_id } => {
                let tab = self.tab_mut(&tab_id);
                tab.messages
                    .push(ChatMessage::System(t!("system.agent_busy").into_owned()));
                tab.scroll_to_bottom();
            }
            AppEvent::TabRenamed {
                old_tab_id,
                new_tab_id,
                new_window_id,
            } => {
                self.rename_tab_session(&old_tab_id, &new_tab_id, new_window_id.as_deref());
            }
            AppEvent::AgentError {
                session_id,
                failure,
                message,
            } => {
                // Classification is typed (`AgentFailure`), done once at the
                // helper boundary where the `acp::Error` code / transport
                // signal is still available. No substring matching here — the
                // discriminant decides the recovery path. `message` is only the
                // human-readable line to display.
                tracing::info!(
                    target: "failure",
                    class = failure.class(),
                    session_id = ?session_id,
                    "agent failure"
                );

                // A user-initiated cancel surfaced as an error is not a
                // failure — the turn already ended via the cancel path, so
                // show nothing and leave the state untouched.
                if failure.is_cancelled() {
                    return;
                }

                // The transport to master is gone — latch the degraded state
                // so the slash-command popup greys out everything but
                // /restart (the only command that can recover without the
                // dead pipe). Cleared on the next Connected.
                if matches!(
                    failure,
                    crate::protocol::acp::failure::AgentFailure::TransportLost
                ) {
                    self.transport_lost = true;
                }

                let is_auth_error = failure.is_auth();
                if is_auth_error && !self.preflight_setup_active {
                    tracing::info!("AgentError auth fallback: showing setup screen");
                    // Use current_agent_id — set at preflight or agent selection time.
                    let agent_id = if !self.current_agent_id.is_empty() {
                        self.current_agent_id.clone()
                    } else {
                        "copilot".to_string()
                    };
                    tracing::info!("AgentError: resolved agent_id={}", agent_id);
                    let profile = crate::agent_registry::lookup_profile(&agent_id);
                    let reason = SetupReason::AgentError;
                    let options = if matches!(
                        self.current_agent_source,
                        crate::agent_source::AgentSource::Wsl { .. }
                    ) {
                        build_setup_options(&reason, None)
                    } else {
                        let agent_status = crate::agent_check::check_agent(profile.id);
                        build_setup_options(&reason, Some(&agent_status))
                    };
                    self.mode = AppMode::Setup;
                    self.state = ConnectionState::Disconnected;
                    self.auth = None;
                    self.setup = Some(SetupState {
                        reason,
                        selected_index: 0,
                        preflight: PreflightResult {
                            agent_id: profile.id.to_string(),
                            display_name: profile.display_name.to_string(),
                            cli_status: CheckStatus::Passed,
                            cli_path: None,
                            auth_status: CheckStatus::Failed(
                                t!("system.authentication_failed").into_owned(),
                            ),
                            install_hint: profile.install_hint.to_string(),
                            install_url: String::new(),
                            auth_hint: profile.auth_hint.to_string(),
                        },
                        install_in_progress: false,
                        install_log: Vec::new(),
                        install_error: None,
                        options,
                        title: t!("setup.title.sign_in").into_owned(),
                        subtitle: if profile.id == "copilot" {
                            t!("setup.subtitle.copilot_auth", agent = profile.display_name)
                                .into_owned()
                        } else {
                            t!("setup.subtitle.agent_auth", agent = profile.display_name)
                                .into_owned()
                        },
                    });
                    // Clear error messages
                    let tab = self.current_tab_mut();
                    tab.messages.retain(|m| !matches!(m, ChatMessage::Error(_)));
                } else {
                    self.state = ConnectionState::Failed(message.clone());
                    self.publish_agent_status();
                    let tab = match session_id.as_deref() {
                        Some(sid) => self.session_tab_mut(sid),
                        None => self.current_tab_mut(),
                    };
                    tab.progress_status = None;
                    tab.activity_frame = 0;
                    tab.timing_note = None;
                    tab.turn = TurnState::Idle;
                    // Suppress only an *identical* consecutive error, not any
                    // trailing error. When the master/agent dies, two errors can
                    // arrive: the raw transport error (returned as-is) and the
                    // `handle_io` watchdog's connection.lost ("/restart") line.
                    // Those are different messages and BOTH should show — the raw
                    // one says what broke, the connection.lost one says how to
                    // recover. Collapsing every consecutive error (the previous
                    // behavior) could hide the /restart hint behind an unrelated
                    // or in-flight error. Dedup only true duplicates so the same
                    // line never stacks.
                    let is_duplicate = matches!(
                        tab.messages.last(),
                        Some(ChatMessage::Error(prev)) if prev == &message
                    );
                    if !is_duplicate {
                        tab.messages.push(ChatMessage::Error(message));
                    }
                }
            }
            AppEvent::PostLoginAuthRecovery {
                failure,
                tab_id,
                agent_id,
            } => {
                tracing::warn!(
                    target: "auth_recovery",
                    failure_class = failure.class(),
                    tab_id = ?tab_id,
                    agent_id = %agent_id,
                    "post-login recovery: reconnecting via a fresh master \
                     (restart_agent_stack)"
                );
                let resolved = if !agent_id.is_empty() {
                    agent_id.clone()
                } else {
                    "copilot".to_string()
                };
                // Pin this recovery to a fresh generation so a stale dead-man
                // timer (from an earlier recovery, or one whose reconnect later
                // succeeds — see AgentConnected) can't fire onto an unrelated
                // Connecting state.
                self.auth_recovery_generation = self.auth_recovery_generation.wrapping_add(1);
                let recovery_generation = self.auth_recovery_generation;
                // (i) Transient "Reconnecting…" — NOT the sign-in screen. The
                // restart below tears this pane down + respawns it, so the
                // common (successful) case never flashes the setup screen
                // between login and the fresh pane connecting. Only a dropped/
                // slow restart leaves us alive long enough for the
                // `AuthRecoveryTimedOut` fallback path to surface the sign-in screen.
                self.mode = AppMode::Chat;
                self.setup = None;
                self.auth = None;
                self.state =
                    ConnectionState::Connecting(t!("connection.reconnecting").into_owned());
                {
                    let tab = self.current_tab_mut();
                    tab.messages.retain(|m| !matches!(m, ChatMessage::Error(_)));
                }
                // (ii) Request a fresh master CLI. The long-lived shared CLI
                // cached its unauthenticated state at spawn and `authenticate`
                // does not refresh it; only a respawn (which re-reads the now
                // valid on-disk credential) recovers. Reuse the tested
                // `/restart` machinery; `tab_id` lets C++ reopen the failing
                // tab rather than the active one.
                let evt = serde_json::json!({
                    "type": "event",
                    "method": "restart_agent_stack",
                    "params": { "reason": "auth_recovery", "tab_id": tab_id },
                });
                send_wt_protocol_event(evt.to_string());
                // (iii) Dead-man fallback: if the restart actually respawned
                // this pane, this helper process is gone before the timer
                // fires. If it survives (dropped/slow restart), surface the
                // sign-in screen so the user isn't stranded on "Reconnecting…".
                // Guarded on a live async runtime so unit tests (no LocalSet)
                // don't panic in `spawn_local`.
                if let Some(ref tx) = self.event_tx {
                    if tokio::runtime::Handle::try_current().is_ok() {
                        let tx = tx.clone();
                        tokio::task::spawn_local(async move {
                            tokio::time::sleep(std::time::Duration::from_secs(8)).await;
                            let _ = tx.send(AppEvent::AuthRecoveryTimedOut {
                                agent_id: resolved,
                                generation: recovery_generation,
                            });
                        });
                    }
                }
            }
            AppEvent::AuthRecoveryTimedOut {
                agent_id,
                generation,
            } => {
                // Only reached when the auth-recovery restart did NOT tear this
                // pane down within the window (dropped/slow delivery) — a
                // successful restart kills this helper process first. Surface
                // the sign-in fallback so the user can retry instead of being
                // stranded on a perpetual "Reconnecting…".
                //
                // The generation guard drops a stale timer: if a newer recovery
                // started, or the reconnect already succeeded (AgentConnected
                // bumps the generation), this no longer matches the current
                // recovery and must not force the sign-in screen.
                if generation == self.auth_recovery_generation
                    && self.mode != AppMode::Setup
                    && matches!(self.state, ConnectionState::Connecting(_))
                {
                    tracing::warn!(
                        target: "auth_recovery",
                        agent_id = %agent_id,
                        "auth-recovery restart did not take effect within the window; \
                         falling back to the sign-in screen"
                    );
                    let resolved = if !agent_id.is_empty() {
                        agent_id
                    } else {
                        "copilot".to_string()
                    };
                    self.show_signin_setup_screen(resolved);
                }
            }
            AppEvent::AgentSoftStop { session_id, reason } => {
                use crate::protocol::acp::soft_stop::SoftStopReason;
                // A soft stop is an *outcome*, not a connection failure — the
                // session stays Connected and the turn already closed via
                // AgentMessageEnd. We only append an informational line so the
                // user knows why the reply ended (truncation / budget / refusal)
                // instead of silently trailing off.
                tracing::info!(
                    target: "soft_stop",
                    class = reason.class(),
                    session_id = %session_id,
                    "agent turn ended on a soft stop"
                );
                let msg = match reason {
                    SoftStopReason::MaxTokens => t!("system.stopped_max_tokens"),
                    SoftStopReason::MaxTurnRequests => t!("system.stopped_max_turn_requests"),
                    SoftStopReason::Refusal => t!("system.stopped_refusal"),
                };
                let tab = self.session_tab_mut(&session_id);
                tab.messages.push(ChatMessage::System(msg.into_owned()));
                tab.scroll_to_bottom();
            }
            AppEvent::ExecutionInfo(message) => {
                self.push_execution_info(message);
                self.current_tab_mut().scroll_to_bottom();
            }
            AppEvent::AgentThoughtChunk { session_id, text } => {
                // Late chunk after cancel / completion is dropped by
                // `turn_observe_chunk` (state isn't Submitted/Streaming).
                self.turn_observe_chunk(&session_id, ChunkKind::Thought, &text);
            }
            AppEvent::AgentMessageChunk { session_id, text } => {
                let tab = self.session_tab_mut(&session_id);
                // Late chunks after cancel / completion are dropped by
                // `turn_observe_chunk` (state isn't Submitted/Streaming).
                // During session/load replay no Submitted state exists,
                // so we still need to gate on `loading_session` here to
                // accept replayed chunks into `messages`.
                if !tab.turn.is_in_flight() && !tab.loading_session {
                    return;
                }
                // Turn boundary detection during replay: an agent
                // message chunk after a buffered user_message_chunk
                // means the previous user turn is complete — flush it
                // as a ChatMessage::User so the chat stays in turn
                // order.
                if tab.loading_session && !tab.pending_user_replay.is_empty() {
                    let text = std::mem::take(&mut tab.pending_user_replay);
                    tab.messages.push(ChatMessage::User(text));
                }
                tab.progress_status = None;
                tab.pending_agent_response.push_str(&text);

                // Append to the streaming buffer. The state machine drops
                // late chunks and handles the stale-autofix generation check
                // before returning whether the buffer actually grew.
                let advanced = self.turn_observe_chunk(&session_id, ChunkKind::Message, &text);

                // Surface the card the moment the streamed JSON parses,
                // instead of waiting for AgentMessageEnd (gated behind
                // Copilot's Stop/SessionEnd hooks, ~8s on Windows).
                if advanced {
                    self.turn_try_eager_surface(&session_id);
                }
            }
            AppEvent::UserMessageReplayChunk { session_id, text } => {
                // Replayed historical user prompt from a `session/load`
                // SessionUpdate. Only meaningful during the load window;
                // dropped otherwise. A new user_message_chunk after a
                // buffered agent response marks the turn boundary —
                // flush the previous agent message first.
                let tab = self.session_tab_mut(&session_id);
                if !tab.loading_session {
                    return;
                }
                if !tab.pending_agent_response.is_empty() {
                    let prev = std::mem::take(&mut tab.pending_agent_response);
                    tab.messages.push(ChatMessage::Agent(prev));
                }
                tab.pending_user_replay.push_str(&text);
            }
            AppEvent::AgentMessageEnd { session_id } => {
                if let Some(summary) = self.session_completion_latency_summary(&session_id) {
                    self.push_execution_info(summary);
                }
                self.turn_close(&session_id);
                self.session_tab_mut(&session_id).scroll_to_bottom();
            }
            AppEvent::TimingMetric { session_id, note } => {
                self.session_tab_mut(&session_id).timing_note = Some(note);
            }
            AppEvent::ToolCall {
                session_id,
                id,
                title,
                status,
            } => {
                let tab = self.session_tab_mut(&session_id);
                if !tab.turn.is_in_flight() && !tab.loading_session {
                    return;
                }
                // Turn boundary during replay (see AgentMessageChunk).
                if tab.loading_session {
                    if !tab.pending_user_replay.is_empty() {
                        let text = std::mem::take(&mut tab.pending_user_replay);
                        tab.messages.push(ChatMessage::User(text));
                    }
                    if !tab.pending_agent_response.is_empty() {
                        let text = std::mem::take(&mut tab.pending_agent_response);
                        tab.messages.push(ChatMessage::Agent(text));
                    }
                }
                tab.tool_calls
                    .insert(id.clone(), (title.clone(), status.clone()));
                tab.messages
                    .push(ChatMessage::ToolCall { id, title, status });
                tab.scroll_to_bottom();
            }
            AppEvent::ToolCallUpdate {
                session_id,
                id,
                status,
            } => {
                let tab = self.session_tab_mut(&session_id);
                if !tab.turn.is_in_flight() && !tab.loading_session {
                    return;
                }
                if let Some(entry) = tab.tool_calls.get_mut(&id) {
                    entry.1 = status.clone();
                }
                // Update in-place in messages
                for msg in &mut tab.messages {
                    if let ChatMessage::ToolCall {
                        id: ref mid,
                        status: ref mut s,
                        ..
                    } = msg
                    {
                        if mid == &id {
                            *s = status.clone();
                        }
                    }
                }
            }
            AppEvent::Plan {
                session_id,
                entries,
            } => {
                let tab = self.session_tab_mut(&session_id);
                if !tab.turn.is_in_flight() && !tab.loading_session {
                    return;
                }
                if tab.loading_session {
                    if !tab.pending_user_replay.is_empty() {
                        let text = std::mem::take(&mut tab.pending_user_replay);
                        tab.messages.push(ChatMessage::User(text));
                    }
                    if !tab.pending_agent_response.is_empty() {
                        let text = std::mem::take(&mut tab.pending_agent_response);
                        tab.messages.push(ChatMessage::Agent(text));
                    }
                }
                tab.messages.push(ChatMessage::Plan(entries));
                tab.scroll_to_bottom();
            }
            AppEvent::PermissionRequest {
                session_id,
                description,
                options,
                responder,
            } => {
                let tab = self.session_tab_mut(&session_id);
                if !tab.turn.is_in_flight() && !tab.loading_session {
                    // Auto-deny if the user cancelled before the agent
                    // got around to asking. Dropping the responder yields
                    // a Cancelled outcome on the agent side.
                    return;
                }
                // FIFO push — never overwrite an in-flight request. The
                // user sees them one at a time (front of the queue is the
                // one rendered + key-handled); resolving the front pops
                // it and exposes the next.
                tab.permission.push_back(PermissionState {
                    description,
                    options,
                    selected: 0,
                    responder: Some(responder),
                });
            }
            AppEvent::SystemMessage(message) => {
                self.current_tab_mut()
                    .messages
                    .push(ChatMessage::System(message));
                self.scroll_to_bottom();
            }
            AppEvent::DebugPipeMessage(msg) => {
                self.debug_messages.push(msg);
                // Cap at 500 messages
                if self.debug_messages.len() > 500 {
                    self.debug_messages.remove(0);
                }
            }
            AppEvent::PreflightComplete(result) => {
                tracing::info!(
                    target: "preflight",
                    agent = %result.agent_id,
                    cli_status = ?result.cli_status,
                    auth_status = ?result.auth_status,
                    "preflight result received"
                );
                if !result.all_passed() {
                    let reason = SetupReason::AgentMissing;
                    let current_status = if matches!(
                        self.current_agent_source,
                        crate::agent_source::AgentSource::Wsl { .. }
                    ) {
                        None
                    } else {
                        Some(crate::agent_check::check_agent(&result.agent_id))
                    };
                    let options = build_setup_options(&reason, current_status.as_ref());
                    let title = reason.title().to_string();
                    let subtitle = if current_status
                        .as_ref()
                        .is_some_and(crate::agent_check::AgentStatus::can_auto_install)
                    {
                        t!(
                            "setup.subtitle.copilot_missing",
                            agent = &result.display_name
                        )
                        .into_owned()
                    } else {
                        t!("setup.subtitle.agent_missing", agent = &result.display_name)
                            .into_owned()
                    };
                    self.mode = AppMode::Setup;
                    self.preflight_setup_active = true;
                    self.setup = Some(SetupState {
                        reason,

                        preflight: result,
                        selected_index: 0,
                        install_in_progress: false,
                        install_log: Vec::new(),
                        install_error: None,
                        options,
                        title,
                        subtitle,
                    });
                }
            }
            AppEvent::AgentSourcesDiscovered {
                generation,
                mut wsl_sources,
            } => {
                if generation != self.agent_source_probe_generation {
                    return;
                }
                self.refresh_available_agents();
                self.available_agents.append(&mut wsl_sources);
                if self.available_agents.is_empty() {
                    self.close_agent_picker();
                    if self.mode == AppMode::Chat {
                        self.current_tab_mut()
                            .messages
                            .push(ChatMessage::System(t!("system.no_agents").into_owned()));
                        self.scroll_to_bottom();
                    }
                } else {
                    let selected = self
                        .available_agents
                        .iter()
                        .position(|agent| {
                            agent.id == self.current_agent_id
                                && agent.source == self.current_agent_source
                        })
                        .unwrap_or(0);
                    self.open_agent_picker(selected);
                }
            }
            AppEvent::AgentSessionEvent(ev) => {
                tracing::debug!(
                    target: "agent_session_registry",
                    event = ?ev,
                    "AgentSessionEvent posted from background callback"
                );
                let hook_event = ev.clone();
                self.agent_sessions.apply(ev);
                self.publish_session_hook(hook_event);
            }
            AppEvent::AliveSnapshotLoaded(items) => {
                let count = items.len();
                tracing::info!(
                    target: "alive_mirror",
                    count,
                    "applied master alive-session bootstrap snapshot"
                );

                // B-9: eagerly snapshot `(sid, pane)` tuples and post
                // `AliveJoinUpgrade` so any already-loaded Historical rows
                // get upgraded to Live. Done before the async registry
                // write so we don't depend on the spawned task finishing
                // before the next event handler runs.
                if let Some(tx) = self.event_tx.clone() {
                    let tuples: Vec<(String, Option<String>)> = items
                        .iter()
                        .map(|i| (i.session_id.0.to_string(), i.pane_session_id.clone()))
                        .collect();
                    let _ = tx.send(AppEvent::AliveJoinUpgrade(tuples));
                }

                let reg = std::sync::Arc::clone(&self.alive);
                let loaded = std::sync::Arc::clone(&self.alive_loaded);
                // The registry is async; we cannot await here (sync
                // event-handler context). spawn_local matches the rest
                // of the helper's tokio LocalSet — the registry mutation
                // races nothing else because AliveSession{Added,Removed}
                // events are also serialized through this loop and the
                // bootstrap snapshot is invoked at most once.
                tokio::task::spawn_local(async move {
                    crate::session_registry::apply_snapshot(&*reg, &loaded, items).await;
                });
            }
            AppEvent::AliveSessionAdded(info) => {
                let sid = info.session_id.clone();
                tracing::debug!(
                    target: "alive_mirror",
                    session_id = %sid.0,
                    pane = ?info.pane_session_id,
                    "alive session added by master"
                );
                // Run the incremental join synchronously so a Historical
                // row (loaded from disk) becomes Live the moment master
                // tells us it's alive. Without this, only the bootstrap
                // `AliveSnapshotLoaded` join would upgrade rows — every
                // subsequent `session_added` broadcast would land only
                // in the mirror and the session management row would stay Historical.
                self.agent_sessions
                    .apply_alive_session_join([(sid.0.as_ref(), info.pane_session_id.as_deref())]);
                let reg = std::sync::Arc::clone(&self.alive);
                tokio::task::spawn_local(async move {
                    reg.upsert(info).await;
                });
            }
            AppEvent::AliveSessionRemoved(sid) => {
                tracing::debug!(
                    target: "alive_mirror",
                    session_id = %sid.0,
                    "alive session removed by master"
                );
                // Mirror PaneClosed's reducer for this sid synchronously,
                // before the async mirror update lands. Otherwise, the
                // session management row stays stuck on Live until the next
                // bootstrap, since
                // `apply_alive_pane_snapshot` is only called at startup
                // and `AliveSessionRemoved` had no path into the reducer
                // (the bug rubber-duck Finding 2 surfaced post-B-12).
                self.agent_sessions
                    .apply_master_session_ended(sid.0.as_ref());
                let reg = std::sync::Arc::clone(&self.alive);
                tokio::task::spawn_local(async move {
                    reg.remove(&sid).await;
                });
            }
            AppEvent::AliveJoinUpgrade(tuples) => {
                tracing::debug!(
                    target: "alive_mirror",
                    count = tuples.len(),
                    "running alive×history join (B-9)"
                );
                let pairs: Vec<(&str, Option<&str>)> = tuples
                    .iter()
                    .map(|(s, p)| (s.as_str(), p.as_deref()))
                    .collect();
                self.agent_sessions.apply_alive_session_join(pairs);
            }
            AppEvent::SessionsChanged => {
                self.schedule_agents_refetch_for_open_views();
            }
            AppEvent::AgentsSnapshotLoaded {
                request_id,
                sessions,
            } => {
                self.handle_agents_snapshot_loaded(request_id, sessions);
            }
            AppEvent::AgentsSnapshotFailed { request_id } => {
                self.handle_agents_snapshot_failed(request_id);
            }
            AppEvent::RegisterBornBoundSession { event } => {
                if self
                    .master_request_tx
                    .send(
                        crate::protocol::acp::client::MasterExtRequest::SessionBornBound {
                            event,
                        },
                    )
                    .is_err()
                {
                    tracing::warn!(
                        target: "coordinator",
                        "born-bound registration queue is unavailable",
                    );
                }
            }
            AppEvent::MasterMutationCompleted { request_id } => {
                tracing::debug!(target: "agents_view", request_id, "master mutation completed; refetching open views");
                self.schedule_agents_refetch_for_open_views();
            }
            AppEvent::WtEvent {
                method,
                pane_id,
                tab_id,
                params,
            } => {
                // Per-WT-event (every vt_sequence included) — trace-only; the
                // single per-event breadcrumb stays at debug in main.rs
                // (`wt_event_rx: received event`).
                tracing::trace!(target: "autofix", method = %method, pane_id = %pane_id, tab_id = ?tab_id, self_pane_id = ?self.pane_id, "WtEvent");

                // Hook bridge events: fire-and-forget into the agent registry
                // so the agent session view stays current. Unrelated to autofix /
                // tab routing; runs before the same-pane skip because we want
                // to record events from our own pane too.
                if method == "agent_event" {
                    let mut hook_events = Vec::new();
                    let _ = route_agent_event_to_registry_with_hook_sink(
                        &mut self.agent_sessions,
                        pane_id.as_str(),
                        &params,
                        |event| hook_events.push(event),
                    );
                    for event in hook_events {
                        self.publish_session_hook(event);
                    }
                    // Diagnostics aid: surface the raw event payload in the
                    // active tab's chat so a developer can correlate hook
                    // wire-format with registry behavior. Off by default.
                    if self.log_agent_events {
                        let detail = serde_json::to_string(&params)
                            .unwrap_or_else(|_| "<unserializable>".to_string());
                        self.current_tab_mut()
                            .messages
                            .push(ChatMessage::AgentEvent(detail));
                    }
                    return;
                }

                // autofix_execute is an inbound UI action ("run the armed
                // fix now") from TerminalPage. pane_id is the failing
                // pane — NOT our own — so this check must run before the
                // same-pane skip below. Ignore the event if we don't
                // actually have a cached autofix for that pane.
                if method == "autofix_execute" {
                    self.handle_autofix_execute_request(&pane_id);
                    return;
                }

                if method == "autofix_dismiss_suggestion" {
                    // User clicked the bar in Suggested state. The bar
                    // always projects the active tab, so clear that tab's
                    // suggested_pane_id and emit cleared.
                    let active = self.active_tab_key().to_string();
                    let suggested = self.current_tab_mut().autofix.suggested_pane_id.take();
                    if suggested.is_some() {
                        self.emit_autofix_state_cleared(&active);
                    }
                    return;
                }

                if method == "autofix_execute_from_detected" {
                    // User pressed the pill / hotkey in Detected state.
                    // Replay the trigger as if auto-suggest were on, so
                    // the LLM call fires and we transition to Pending.
                    self.handle_autofix_execute_from_detected();
                    return;
                }

                if method == "agent_prompt" {
                    // Command palette `?<prompt>` delegation. Not a WT
                    // notification — has nothing to do with banner/queue.
                    let prompt = params
                        .get("prompt")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    tracing::info!(target: "autofix", prompt_len = prompt.len(), "agent_prompt: delegating");
                    if !prompt.is_empty() {
                        self.delegate_to_tab_agent(prompt);
                    }
                    return;
                }

                if method == "agent_paste_text" {
                    self.handle_agent_paste_text(&params);
                    return;
                }

                if method == "agent_config_changed" {
                    // C++ pushes this when the user changes a hot-updatable
                    // agent setting (auto-suggest gate, acp-model, delegate
                    // agent/model) while WTA is already running. Unified
                    // dispatch: each field is optional and only present when
                    // it actually changed, so we apply exactly what's set
                    // — all in place, with NO agent-pane teardown/restart.
                    // (Agent *identity* changes go through a master respawn
                    // on the C++ side, not this event.)
                    if let Some(enabled) =
                        params.get("autofix_enabled").and_then(|v| v.as_bool())
                    {
                        tracing::info!(
                            target: "autofix",
                            old = self.autofix_enabled,
                            new = enabled,
                            "autofix_enabled hot-reloaded from settings change",
                        );
                        self.autofix_enabled = enabled;
                    }

                    // delegate_agent + delegate_model travel together so the
                    // delegate runtime table can be rebuilt in one shot.
                    if params.get("delegate_agent").is_some()
                        || params.get("delegate_model").is_some()
                    {
                        let delegate_agent = params
                            .get("delegate_agent")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let delegate_model = params
                            .get("delegate_model")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        self.apply_delegate_config(delegate_agent, delegate_model);
                    }

                    // acp-model: a global settings change is authoritative. It
                    // overrides every pane's local `/model` pick, redirects the
                    // shared current-model display, hot-swaps the model on all
                    // live sessions, and republishes status — so every pane
                    // visibly follows the new model (see apply_global_acp_model).
                    // Storing it also keeps future sessions (/new, lazy-first-
                    // prompt) on the new model via the SessionAttached re-apply.
                    if let Some(raw) = params.get("acp_model").and_then(|v| v.as_str()) {
                        tracing::info!(
                            target: "autofix",
                            model = raw,
                            "acp-model hot-update requested from settings change",
                        );
                        self.apply_global_acp_model(Some(raw.to_string()));
                    }
                    return;
                }

                if method == "tab_changed" {
                    // Window-scoped: WT broadcasts via shared COM, so every
                    // helper (across every window) receives every tab_changed.
                    // Without this filter, helper-A in window 1 would call
                    // switch_tab_session on a window-2 tab_id and start
                    // rendering tab_sessions[<window-2 tab>] in its TUI —
                    // detaching the agent pane content from its owner tab.
                    // Same shape as the `set_agent_state` window filter below:
                    // skip only when both ids are non-empty and differ.
                    let target_window = params
                        .get("window_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let our_window = self.window_id.as_deref().unwrap_or("");
                    if !target_window.is_empty()
                        && !our_window.is_empty()
                        && target_window != our_window
                    {
                        tracing::debug!(
                            target: "tab_session",
                            target_window,
                            our_window,
                            "ignoring tab_changed for different window"
                        );
                        return;
                    }
                    tracing::info!(
                        target: "tab_session",
                        raw_params = %params,
                        current_tab = ?self.tab_id,
                        "tab_changed event received"
                    );
                    if let Some(new_tab_id) = params.get("tab_id").and_then(|v| v.as_str()) {
                        // switch_tab_session calls project_active_tab_state
                        // at its end — that pushes the new tab's view AND
                        // autofix bar snapshot to C++ in one shot.
                        self.switch_tab_session(new_tab_id.to_string());
                    } else {
                        tracing::warn!(target: "tab_session", "tab_changed: missing tab_id in params");
                    }
                    return;
                }

                if method == "tab_closed" {
                    // Same window filter as tab_changed — drop_tab_session
                    // removes from `tab_sessions` and nulls `self.tab_id`
                    // when the closed tab is the active one, so a cross-
                    // window leak would wipe per-tab state of a tab the
                    // helper doesn't even own.
                    let target_window = params
                        .get("window_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let our_window = self.window_id.as_deref().unwrap_or("");
                    if !target_window.is_empty()
                        && !our_window.is_empty()
                        && target_window != our_window
                    {
                        tracing::debug!(
                            target: "tab_session",
                            target_window,
                            our_window,
                            "ignoring tab_closed for different window"
                        );
                        return;
                    }
                    if let Some(closed_tab_id) = params.get("tab_id").and_then(|v| v.as_str()) {
                        self.drop_tab_session(closed_tab_id);
                    } else {
                        tracing::warn!(target: "tab_session", "tab_closed: missing tab_id in params");
                    }
                    return;
                }

                if method == "tab_renamed" {
                    // Tab-drag rename: the user dragged this tab into
                    // another window so WT minted a fresh StableId. The
                    // helper process survives the drag; we just need to
                    // rekey our per-tab maps so events with the new id
                    // route to this tab's existing state. Route through
                    // the AppEvent::TabRenamed handler so the WtEvent
                    // inline path and any direct AppEvent posts share
                    // one implementation.
                    let old_tab_id = params
                        .get("old_tab_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let new_tab_id = params
                        .get("new_tab_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if old_tab_id.is_empty() || new_tab_id.is_empty() {
                        tracing::warn!(
                            target: "tab_session",
                            old_tab_id,
                            new_tab_id,
                            "tab_renamed: missing old_tab_id or new_tab_id in params"
                        );
                        return;
                    }
                    let new_window_id = params
                        .get("window_id")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string());
                    self.handle_event(AppEvent::TabRenamed {
                        old_tab_id: old_tab_id.to_string(),
                        new_tab_id: new_tab_id.to_string(),
                        new_window_id,
                    });
                    return;
                }

                if method == "reset_tab_session" {
                    if let Some(tab_id) = params.get("tab_id").and_then(|v| v.as_str()) {
                        self.reset_tab_session_for(tab_id);
                    } else {
                        tracing::warn!(target: "tab_session", "reset_tab_session: missing tab_id in params");
                    }
                    return;
                }

                // load_session: WT-side replay of WTA's
                // `resume_in_new_agent_tab` request. After WT creates a
                // new tab and reconciles the shared agent pane onto it,
                // it publishes this event with the new tab's StableId,
                // the historical session id, and the cwd. We forward to
                // the ACP client which calls `conn.load_session` and
                // binds the loaded session to the tab via
                // `SessionAttached`. Best-effort: if the agent doesn't
                // recognize the session id (e.g. CLI mismatch), the
                // client emits a `TabError` scoped to this tab. We also
                // pre-switch the target tab back to the Chat view, clear
                // its local chat, and post a "Resuming..." system note
                // so the user sees something even if the agent's
                // session/update replay is delayed or absent.
                if method == "load_session" {
                    let tab_id = params.get("tab_id").and_then(|v| v.as_str()).unwrap_or("");
                    let session_id = params
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let cwd = params
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .filter(|s| !s.is_empty());
                    tracing::info!(
                        target: "acp_load_session",
                        tab_id,
                        session_id,
                        "inbound load_session event from WT"
                    );
                    if tab_id.is_empty() || session_id.is_empty() {
                        tracing::warn!(
                            target: "acp_load_session",
                            "load_session: missing tab_id or session_id in params"
                        );
                        return;
                    }
                    // Defensive owner_tab_id filter: WT broadcasts
                    // `load_session` over shared COM, so every helper in
                    // every window receives it. Without this filter,
                    // helpers owning a different tab would respond to a
                    // load_session targeted at someone else's pane — the
                    // misroute that bug #1 was about (the legacy resume
                    // flow used to rely on this not filtering, but the
                    // boot-time `--initial-load-session-id` path
                    // (main.rs) is now the canonical way to drive
                    // resumes into a freshly-spawned helper, so a
                    // belt-and-suspenders filter here is safe).
                    if let Some(owner) = self.owner_tab_id.as_deref() {
                        if owner != tab_id {
                            tracing::debug!(
                                target: "acp_load_session",
                                owner,
                                tab_id,
                                "ignoring load_session for non-owner tab"
                            );
                            return;
                        }
                    }
                    {
                        let tab = self.tab_mut(tab_id);
                        tab.current_view = View::Chat;
                        tab.clear_chat_history();
                        tab.completed_turns.clear();
                        tab.selected_completed_turn_idx = None;
                        tab.session_id = None;
                        // Open the replay window: chunk handlers will
                        // now accept session/update events for this
                        // tab even though `turn` stays Idle. Closed by
                        // the SessionAttached handler when the attach
                        // event arrives for THIS specific session id
                        // (unrelated SessionAttached events — e.g. the
                        // bootstrap `session/new` racing with a
                        // boot-time Plan-C initial-load — must not
                        // close it).
                        tab.loading_session = true;
                        tab.loading_target_session_id = Some(session_id.to_string());
                        // Resume is intentionally silent — no "Resuming…"
                        // marker — so a resumed pane presents exactly like a
                        // normal connection. `loading_session` still opens the
                        // replay window; any past content just streams in above.
                    }
                    // If the load_session target IS the active tab, push the
                    // (now Chat) view to C++ so the bar drops the "Agent
                    // sessions" label that the user was looking at when they
                    // hit Shift+Enter on a session row. When the target is a
                    // not-yet-active tab (e.g. WT just created a fresh tab
                    // and the `tab_changed` race still hasn't landed), the
                    // imminent `tab_changed` to that tab will project then.
                    if tab_id == self.active_tab_key() {
                        self.project_active_tab_state();
                    }
                    let _ = self.load_session_tx.send(LoadSessionForTab {
                        tab_id: tab_id.to_string(),
                        session_id: session_id.to_string(),
                        cwd,
                    });
                    return;
                }

                // set_agent_state: unified inbound request from C++ to
                // change one or more pieces of per-tab agent-pane UI state
                // for a specific tab. Every field under `params` is
                // optional — only specified ones are applied, the rest
                // are left untouched.
                //
                // Supported fields:
                //   * `tab_id`: optional WT StableId of the tab to mutate.
                //               Falls back to the active tab when absent.
                //               C++ should always include it: defends
                //               against `tab_changed`/`set_agent_state`
                //               ordering ambiguity (e.g. resume-in-new-tab
                //               creates a new tab and immediately requests
                //               pane_open=true; with `tab_id` we don't
                //               depend on `tab_changed` arriving first to
                //               route to the right TabSession).
                //   * `view`: "chat" | "sessions"
                //   * `pane_open`: bool
                //
                // **Projection rule**: if the target tab is the currently-
                // active one, immediately project the new snapshot back to
                // C++ (`agent_state_changed`). If the target is NOT active,
                // skip projection — the next `tab_changed` to that tab will
                // project the now-up-to-date state. C++ mirrors are global
                // per-pane so they only need refreshing when the active tab
                // changes (or when a mutation lands on the active tab).
                //
                // **Round-trip contract**: under the "wta is the sole owner
                // of agent-pane UI state" architecture, C++ does NOT update
                // its mirrors (`_agentSessionsViewActive`, `Tab.AgentPaneOpen`)
                // when it sends `set_agent_state`. It waits for the resulting
                // `agent_state_changed` emitted by `project_active_tab_state`
                // below. One IPC round-trip latency, in exchange for the
                // C++ flags having a single writer (`OnAgentStateChanged`),
                // which makes desync architecturally impossible.
                //
                // Window-scoped: WT includes its own window_id; we ignore
                // the event when our window_id is known and doesn't match,
                // so multi-window setups don't cross-talk. When window_id
                // is unknown on either side we apply (best-effort fallback).
                //
                // Processed BEFORE the own-pane skip below: this is a
                // global UI command, not a per-pane signal.
                if method == "set_agent_state" {
                    let target_window = params
                        .get("window_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let our_window = self.window_id.as_deref().unwrap_or("");
                    if !target_window.is_empty()
                        && !our_window.is_empty()
                        && target_window != our_window
                    {
                        tracing::debug!(
                            target: "set_agent_state",
                            target_window,
                            our_window,
                            "ignoring set_agent_state for different window"
                        );
                        return;
                    }

                    // Resolve target tab: explicit `tab_id` wins;
                    // otherwise fall back to the active tab. The explicit
                    // path is robust against `tab_changed` ordering races
                    // (e.g. resume-in-new-tab where C++ creates a tab and
                    // immediately fires `set_agent_state` for it).
                    let target_tab = params
                        .get("tab_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| self.active_tab_key().to_string());

                    // Apply `view` if present.
                    if let Some(view_str) = params.get("view").and_then(|v| v.as_str()) {
                        tracing::info!(
                            target: "set_agent_state",
                            tab = %target_tab,
                            view = view_str,
                            "applying view"
                        );
                        match view_str {
                            "sessions" | "agents" => {
                                // User entered session management (via shortcut or UI) —
                                // permanently dismiss the welcome hint.
                                if self.show_welcome_hint {
                                    self.show_welcome_hint = false;
                                    set_welcome_shown_in_state();
                                }
                                self.open_agents_view_for_tab(target_tab.clone());
                            }
                            "chat" => {
                                self.close_agents_view_for_tab(&target_tab);
                            }
                            other => {
                                tracing::warn!(
                                    target: "set_agent_state",
                                    view = other,
                                    "unknown view value — ignoring"
                                );
                            }
                        }
                    }

                    // Apply `pane_open` if present.
                    if let Some(open) = params.get("pane_open").and_then(|v| v.as_bool()) {
                        tracing::info!(
                            target: "set_agent_state",
                            tab = %target_tab,
                            pane_open = open,
                            "applying pane_open"
                        );
                        self.tab_mut(&target_tab).pane_open = open;
                        // If a result is waiting for review on this tab,
                        // re-project the bar: opening the pane makes the
                        // result visible (→ Idle, bar goes quiet), closing
                        // it brings the Review hint back. The open/closed →
                        // Idle/Review decision lives entirely here in the
                        // helper, not in C++.
                        if let Some(review_pane) = self
                            .tab_sessions
                            .get(&target_tab)
                            .and_then(|t| t.autofix.suggested_pane_id.clone())
                        {
                            self.emit_autofix_state_result(&target_tab, &review_pane);
                        }
                    }

                    // Always echo the mutation back — C++ routes
                    // `agent_state_changed` by `tab_id`, so per-tab state
                    // updates apply to the right AgentPaneContent
                    // regardless of which tab is currently focused.
                    self.project_tab_state(&target_tab);
                    return;
                }

                // Skip events from our own pane
                if self.pane_id.as_deref() == Some(pane_id.as_str()) {
                    tracing::debug!(target: "autofix", "skipped: own pane");
                    return;
                }

                // Bridge WT-native `connection_state` events into the agent
                // session registry so rows transition out of live states
                // (Idle/Working/...) when the underlying pane dies. The
                // hook-bridge path (`agent.session.end` → `SessionStopped`)
                // handles Claude/Copilot, but Gemini has no end-of-session
                // hook, so without this wire a Gemini row spawned via session management view
                // resume stays Idle forever after the user types `/exit`.
                //
                // Both event variants are no-ops in the registry when
                // `pane_id` isn't bound to any agent session, so this is
                // safe to apply unconditionally for non-own panes.
                if method == "connection_state" {
                    let state = params.get("state").and_then(|v| v.as_str()).unwrap_or("");
                    tracing::info!(
                        target: "helper_wt_event",
                        pane_id = %pane_id,
                        state,
                        self_pane = ?self.pane_id,
                        "helper observed WT connection_state event"
                    );
                    match state {
                        "closed" => {
                            // Capture the key BEFORE PaneClosed clears
                            // the pane→key binding, so the log can report
                            // which row was demoted.
                            let key_before = self
                                .agent_sessions
                                .key_for_pane(&pane_id);
                            let event = crate::agent_sessions::SessionEvent::PaneClosed {
                                pane_session_id: pane_id.clone(),
                            };
                            self.agent_sessions.apply(event.clone());
                            self.publish_session_hook(event);
                            tracing::info!(
                                target: "helper_wt_event",
                                pane_id = %pane_id,
                                key_before = ?key_before,
                                "helper applied PaneClosed locally + published to master"
                            );
                        }
                        "failed" => {
                            let reason = params
                                .get("reason")
                                .and_then(|v| v.as_str())
                                .unwrap_or("connection failed")
                                .to_string();
                            let event = crate::agent_sessions::SessionEvent::ConnectionFailed {
                                pane_session_id: pane_id.clone(),
                                reason,
                            };
                            self.agent_sessions.apply(event.clone());
                            self.publish_session_hook(event);
                        }
                        _ => {}
                    }
                }

                // Detect agent CLI exit when the pane stays alive (e.g. user
                // typed `gemini` inside their pwsh/cmd shell, then `/exit`):
                // the shell emits `osc:133;A` (FinalTerm prompt-start) when
                // it returns to its own prompt. If the pane is currently
                // bound to an agent session, treat that as the agent's
                // teardown signal and transition the row to Ended.
                //
                // We deliberately do NOT depend on the agent's own SessionEnd
                // hook here because:
                //   * Gemini has no reliable hook on `/exit`
                //     (`agent.session.end` is "Hook cancelled" most of the time)
                //   * Even when the hook fires, it races with our event loop
                //
                // The shell's prompt-start marker is the most reliable
                // cross-CLI signal that the agent process has released the
                // foreground.
                if method == "vt_sequence" {
                    let seq = params
                        .get("sequence")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    // Gate OSC 133;A → PaneClosed on the bound
                    // session's ORIGIN, not just "is_agent_pane".
                    //
                    // Background: this handler exists to detect agent
                    // exit in SHELL panes (user typed `gemini` in pwsh,
                    // agent ran, user `/exit`'d, shell returns to its
                    // prompt → OSC 133;A fires → we treat that as the
                    // agent's teardown signal). For those sessions
                    // origin is `Unknown`.
                    //
                    // For agent panes proper (origin AgentPane) there
                    // is NO shell underneath the conpty — the helper
                    // TUI is the direct child. Yet WT itself can
                    // emit OSC 133;A around focus/window-switch on
                    // arbitrary panes (observed in `wtcli focus-pane`
                    // round trips). The previous gate
                    // `is_agent_pane(pane_id)` matched any pane with
                    // ANY bound session and demoted the row to Ended
                    // even though the agent CLI was happily still
                    // streaming notifications — the user sees this as
                    // "session management Enter on a Live row spawned a new pane
                    // instead of focusing the existing one" because
                    // the row demoted between snapshot and Enter.
                    //
                    // Restrict to origin=Unknown so the heuristic
                    // keeps working for its original shell-pane use
                    // case without nuking agent panes.
                    let origin = self.agent_sessions.origin_for_pane(&pane_id);
                    let is_shell_agent = matches!(origin, Some(crate::agent_sessions::SessionOrigin::Unknown));
                    if seq == "osc:133;A" && is_shell_agent {
                        tracing::info!(
                            target: "agent_session_registry",
                            pane_id = %pane_id,
                            "shell prompt-start in agent-bound pane: treating as agent exit",
                        );
                        let event = crate::agent_sessions::SessionEvent::PaneClosed {
                            pane_session_id: pane_id.clone(),
                        };
                        self.agent_sessions.apply(event.clone());
                        self.publish_session_hook(event);
                    }
                }

                let notification = classify_wt_event(&method, &pane_id, tab_id.as_deref(), &params);
                // Per-WT-event classification — trace-only (vt_sequence volume).
                tracing::trace!(target: "autofix", severity = ?notification.severity, summary = %notification.summary, tab_id = ?notification.tab_id, "classified");

                // Per-tab filter. WT broadcasts pane-scoped events to every
                // helper in the window, but another tab's failures are not
                // this helper's concern. Drop notifications whose tab_id
                // doesn't match our owner_tab_id; empty/missing tab_id falls
                // through (no per-tab scope).
                if let (Some(event_tab), Some(self_tab)) = (
                    notification.tab_id.as_deref(),
                    self.owner_tab_id.as_deref(),
                ) {
                    if !event_tab.is_empty()
                        && !self_tab.is_empty()
                        && event_tab != self_tab
                    {
                        // Per-cross-tab-event (very high volume in multi-tab
                        // windows) — trace-only.
                        tracing::trace!(
                            target: "autofix",
                            event_tab,
                            self_tab,
                            method = %method,
                            "dropping cross-tab WT event"
                        );
                        return;
                    }
                }

                // Telemetry: emit ErrorDetected for any non-acknowledged
                // critical/actionable classification. Acknowledged events are
                // the auto-silenced "unknown"/"connected"/success cases.
                if !notification.acknowledged {
                    let severity_str = match notification.severity {
                        WtEventSeverity::Critical => Some("Critical"),
                        WtEventSeverity::Actionable => Some("Actionable"),
                        WtEventSeverity::Informational => None,
                    };
                    if let Some(severity_str) = severity_str {
                        crate::telemetry::log_error_detected(
                            severity_str,
                            &method,
                            &pane_id,
                        );
                    }
                }

                // Surface rule: WT events (connection_state, vt_sequence)
                // surface via the bottom bar / `wt_notifications` queue ONLY.
                // Chat is the agent dialogue surface — only user input and
                // agent responses go there.
                match notification.severity {
                    WtEventSeverity::Critical | WtEventSeverity::Actionable => {
                        self.show_notification_banner = true;
                        // Only OSC-133;D vt_sequence events have the exit
                        // code + live shell buffer needed to drive autofix.
                        // `connection_state: closed`/`failed` is just process
                        // termination — banner-only.
                        if method == "vt_sequence" {
                            self.maybe_trigger_autofix(&notification);
                        }
                    }
                    WtEventSeverity::Informational => {
                        // "User moved past this prompt" = dismiss. Two signals
                        // both count as "moved on":
                        //   * exit-zero (D;0): the user ran any successful
                        //     command in the failing pane.
                        //   * prompt-start (A): the shell drew a fresh prompt
                        //     line (user pressed Enter, switched away, etc.).
                        // For Pending/Armed/Detected we gate prompt-start on
                        // `trigger_echo_pane` so the immediate A that
                        // PowerShell emits ~1ms after every D doesn't
                        // dismiss the state we just established. Suggested
                        // fires asynchronously (after the LLM returns), so
                        // it has no echo to skip and dismisses on any A.
                        if method == "vt_sequence" {
                            let seq = params
                                .get("sequence")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let is_exit_zero = seq
                                .strip_prefix("osc:133;")
                                .and_then(|rest| rest.strip_prefix("D;"))
                                .and_then(|code| code.trim().parse::<i32>().ok())
                                .map(|c| c == 0)
                                .unwrap_or(false);
                            let is_prompt_start = seq == "osc:133;A";
                            // Resolve the event's owning tab (added in Step 1).
                            // Older events without tab_id can't be cleanly
                            // routed; skip the per-tab clear for them.
                            let event_tab = tab_id.clone();
                            // Consume the trigger-echo flag if this A is the
                            // one PowerShell emits immediately after the
                            // triggering D. `effective_prompt_start` is the
                            // "user actually moved on" signal for D-synchronous
                            // states (Pending / Detected). Suggested uses raw
                            // `is_prompt_start` since it fires post-LLM.
                            let effective_prompt_start = if is_prompt_start {
                                if let Some(t) = event_tab.as_deref() {
                                    let echo = self
                                        .tab_mut(&t.to_string())
                                        .autofix
                                        .trigger_echo_pane
                                        .clone();
                                    if echo.as_deref() == Some(pane_id.as_str()) {
                                        self.tab_mut(&t.to_string())
                                            .autofix
                                            .trigger_echo_pane = None;
                                        false
                                    } else {
                                        true
                                    }
                                } else {
                                    true
                                }
                            } else {
                                false
                            };
                            let armed_in_event_tab = event_tab
                                .as_deref()
                                .and_then(|t| self.tab_sessions.get(t))
                                .and_then(|t| t.autofix.pane_id.as_deref())
                                .map(str::to_string);
                            if (is_exit_zero || effective_prompt_start)
                                && armed_in_event_tab.as_deref() == Some(pane_id.as_str())
                            {
                                let target_tab = event_tab
                                    .clone()
                                    .expect("armed_in_event_tab requires tab_id present");
                                // Telemetry: a fix was armed for this pane and the next
                                // command exited cleanly — the user's problem resolved.
                                // Elapsed is monotonic (`Instant::elapsed`) from arm to
                                // clean exit, not wall-clock.
                                if let Some(armed) = self
                                    .tab_mut(&target_tab)
                                    .autofix
                                    .armed_at
                                    .take()
                                {
                                    let elapsed_ms = armed.elapsed().as_secs_f64() * 1000.0;
                                    crate::telemetry::log_error_fix_resolved(
                                        pane_id.as_str(),
                                        elapsed_ms,
                                    );
                                }
                                // `turn_cancel` owns the full cleanup: bumps
                                // the tab's autofix_generation, emits cleared
                                // (resolving the pane from AutofixContext, or
                                // `autofix.pane_id` as a fallback), and
                                // resets `tab.turn` to Idle. Avoid duplicating
                                // its work.
                                let session_id = self
                                    .tab_sessions
                                    .get(&target_tab)
                                    .and_then(|t| t.session_id.clone());
                                if let Some(sid) = session_id {
                                    self.turn_cancel(&sid);
                                } else {
                                    // No ACP session bound — replicate the
                                    // minimum cleanup turn_cancel would do.
                                    let pane_to_clear = {
                                        let tab = self.tab_mut(&target_tab);
                                        tab.autofix.generation =
                                            tab.autofix.generation.wrapping_add(1);
                                        tab.clear_recommendations();
                                        tab.autofix.pane_id.take()
                                    };
                                    if pane_to_clear.is_some() {
                                        self.emit_autofix_state_cleared(&target_tab);
                                    }
                                }
                            }
                            // Suggested: dismiss on prompt activity (exit-zero
                            // or a fresh prompt-start) in the event's tab.
                            // Emit cleared so the bar's per-tab snapshot
                            // resets to Idle.
                            if is_exit_zero || is_prompt_start {
                                if let Some(t) = event_tab.as_deref() {
                                    let t_owned = t.to_string();
                                    let pane_to_clear =
                                        self.tab_mut(&t_owned).autofix.suggested_pane_id.take();
                                    if pane_to_clear.is_some() {
                                        self.emit_autofix_state_cleared(&t_owned);
                                    }
                                }
                            }
                            // Detected (suggest-mode pill): dismiss when the
                            // user moves on in the same pane — either a
                            // successful command (exit-zero) or a fresh
                            // prompt-start that isn't the trigger's echo.
                            // The Detected snapshot has no in-flight turn
                            // to cancel — just clear the bar.
                            if is_exit_zero || effective_prompt_start {
                                if let Some(t) = event_tab.as_deref() {
                                    let t_owned = t.to_string();
                                    let detected_matches = matches!(
                                        &self.tab_mut(&t_owned).autofix.bar_snapshot,
                                        AutofixBarSnapshot::Detected { pane_id: bar_pane, .. }
                                            if bar_pane == pane_id.as_str()
                                    );
                                    if detected_matches {
                                        self.emit_autofix_state_cleared(&t_owned);
                                    }
                                }
                            }
                        }
                    }
                }

                // Queue the notification (cap at 20)
                self.wt_notifications.push_back(notification);
                if self.wt_notifications.len() > 20 {
                    self.wt_notifications.pop_front();
                }
            }
            AppEvent::AgentInstallComplete => {
                // Check if the agent we were trying to install is now available.
                let agent_id = self
                    .setup
                    .as_ref()
                    .map(|s| s.preflight.agent_id.clone())
                    .unwrap_or_default();

                if !agent_id.is_empty() {
                    let status = crate::agent_check::check_agent(&agent_id);
                    if status.cli_found {
                        // Install succeeded → proceed to connect or auth
                        let profile = crate::agent_registry::lookup_profile_by_id(&agent_id);

                        if agent_id == "copilot" {
                            // Copilot was just installed by IT. Route directly
                            // to sign-in instead of probing local credentials or
                            // paying for a doomed ACP auth roundtrip.
                            self.show_copilot_auth_screen();
                        } else {
                            // Future-proofing: only Copilot has an in-app auth
                            // screen. If another agent ever becomes
                            // auto-installable, keep it on the diagnostic setup
                            // retry path instead of entering Auth mode.
                            let reason = SetupReason::AgentError;
                            let options = build_setup_options(&reason, Some(&status));
                            self.mode = AppMode::Setup;
                            self.setup = Some(SetupState {
                                reason,
                                selected_index: 0,
                                preflight: PreflightResult {
                                    agent_id: agent_id.clone(),
                                    display_name: status.display_name.clone(),
                                    cli_status: CheckStatus::Passed,
                                    cli_path: status.cli_path.clone(),
                                    auth_status: CheckStatus::Failed(
                                        t!("system.authentication_failed").into_owned(),
                                    ),
                                    install_hint: profile.install_hint.to_string(),
                                    install_url: String::new(),
                                    auth_hint: profile.auth_hint.to_string(),
                                },
                                install_in_progress: false,
                                install_log: Vec::new(),
                                install_error: None,
                                options,
                                title: t!("setup.title.sign_in").into_owned(),
                                subtitle: t!(
                                    "setup.subtitle.agent_auth",
                                    agent = status.display_name.as_str()
                                )
                                .into_owned(),
                            });
                        }
                        return;
                    }
                }

                // Install didn't resolve the issue — stay on setup, refresh options
                if let Some(ref mut setup) = self.setup {
                    setup.install_in_progress = false;
                    let current_status = if !agent_id.is_empty() {
                        Some(crate::agent_check::check_agent(&agent_id))
                    } else {
                        None
                    };
                    setup.options = build_setup_options(&setup.reason, current_status.as_ref());
                }
            }
            AppEvent::LoginProgress {
                device_code,
                verify_url,
            } => {
                // Only reflect device-flow progress while an auth attempt is
                // actively checking. A late event after the user left the
                // screen (auth = None) must not write status or copy a device
                // code to the clipboard.
                if let Some(ref mut auth) = self.auth {
                    if auth.checking {
                        auth.status_message = t!(
                            "auth.device_code_prompt",
                            url = verify_url.as_str(),
                            code = device_code.as_str()
                        )
                        .into_owned();
                        // Copy device code to clipboard
                        let code_to_copy = device_code.clone();
                        tokio::task::spawn_blocking(move || {
                            if let Err(e) = crate::win32::copy_text_to_clipboard(&code_to_copy) {
                                tracing::warn!(
                                    target: "clipboard",
                                    error = %e,
                                    "failed to copy Copilot device code to clipboard"
                                );
                            }
                        });
                    }
                }
            }
            AppEvent::LoginComplete { success, error, agent_id } => {
                tracing::info!("LoginComplete received: success={} deferred_acp={}", success, self.deferred_acp.is_some());
                // Ignore stale/late completions: only act on a completion that
                // matches the currently active auth attempt. After the user
                // escapes the auth screen (auth = None) or switches agents, a
                // late background login must not force Chat mode, start ACP for
                // the wrong/empty agent, or rewrite another screen's status.
                let active = self
                    .auth
                    .as_ref()
                    .map(|a| a.agent_id == agent_id)
                    .unwrap_or(false);
                if !active {
                    tracing::info!(
                        "LoginComplete ignored (no matching active auth attempt) agent={}",
                        agent_id
                    );
                    return;
                }
                if success {
                    // Login succeeded → transition to Chat and start ACP
                    self.mode = AppMode::Chat;
                    self.setup = None;
                    self.state =
                        ConnectionState::Connecting(t!("connection.starting").into_owned());
                    self.update_deferred_acp_agent(&agent_id);
                    // If deferred_acp is None (helper mode — the initial
                    // ACP client already exited with auth error and dropped
                    // its channels), create a fresh DeferredAcpParams so
                    // try_start_acp can spawn a new ACP client.
                    if self.deferred_acp.is_none() {
                        let new_cmd = self.build_agent_cmd(&agent_id);
                        tracing::info!("LoginComplete: creating deferred_acp for reconnect cmd={}", new_cmd);
                        self.deferred_acp = Some(DeferredAcpParams {
                            agent_cmd: new_cmd,
                            acp_model: None,
                            agent_source: self.current_agent_source.clone(),
                            source_cwd: self.source_cwd.clone(),
                            prompt_rx: None, // try_start_acp will create fresh channels
                            cancel_rx: None,
                            new_session_rx: None,
                            load_session_rx: None,
                            drop_session_rx: None,
                            rename_session_rx: None,
                            restart_rx: None,
                            master_ext_rx: None,
                            shell_mgr: Arc::clone(&self.shell_mgr),
                            wt_connected: self.wt_connected,
                            master_pipe_name: None,
                            owner_tab_id: None,
                        });
                    }
                    self.pending_acp_start = true;
                    self.needs_post_login_authenticate = true;
                    self.auth = None;
                } else {
                    // Login failed — show auth screen again with feedback.
                    if let Some(ref mut auth) = self.auth {
                        auth.checking = false;
                        // Copilot device-flow failed (e.g. an unreachable
                        // GitHub Enterprise host) — surface the reason instead
                        // of silently returning to the form.
                        auth.status_message = error
                            .filter(|e| !e.trim().is_empty())
                            .unwrap_or_else(|| t!("system.authentication_failed").into_owned());
                    }
                }
            }
        }
    }
}
