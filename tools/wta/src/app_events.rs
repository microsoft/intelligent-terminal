//! `App::handle_event` — the central `AppEvent` dispatch match, split out of
//! the large `app.rs` file into its own child module of `app` (via
//! `#[path]`) so it can reach `App`'s private fields and helper methods just
//! like the rest of `app.rs` does. `handle_event` is `pub(super)` because
//! `App::run` (in `app.rs`) and the test helpers in sibling test modules
//! (`app_tests.rs`, `autofix_tests.rs`) all call it directly on an `App`.

use super::*;

pub(super) const AUTH_RECOVERY_CONNECTION_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(8);
// SharedWta grants retirement 16 seconds before replacing the master. Allow
// one second for replacement readiness plus one second of scheduling margin.
pub(super) const AUTH_RECOVERY_MASTER_READY_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(18);

#[derive(serde::Deserialize)]
struct AgentReconnectWire {
    operation_id: String,
    window_id: String,
    tab_id: String,
    generation: u64,
    agent_id: String,
    acp_model: Option<String>,
    custom_model_selection: Option<String>,
    agent_source: String,
    wsl_distro: Option<String>,
}

impl App {
    fn arm_auth_recovery_timeout(
        &self,
        agent_id: String,
        generation: u64,
        timeout: std::time::Duration,
    ) {
        let Some(tx) = self.event_tx.clone() else {
            return;
        };
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        tokio::task::spawn_local(async move {
            tokio::time::sleep(timeout).await;
            let _ = tx.send(AppEvent::AuthRecoveryTimedOut {
                agent_id,
                generation,
            });
        });
    }

    fn handle_agent_rebind(&mut self, params: serde_json::Value) {
        let Ok(wire) = serde_json::from_value::<AgentReconnectWire>(params.clone()) else {
            tracing::warn!(
                target: "agent_rebind",
                payload = %params,
                "ignoring malformed agent rebind event"
            );
            return;
        };
        if self.owner_tab_id.as_deref() != Some(wire.tab_id.as_str())
            || wire.window_id.trim().is_empty()
            || self
                .window_id
                .as_deref()
                .is_some_and(|window_id| !window_id.is_empty() && window_id != wire.window_id)
        {
            return;
        }

        let agent_source = if wire
            .agent_source
            .eq_ignore_ascii_case(crate::agent_source::AgentSource::HOST_KIND)
        {
            crate::agent_source::AgentSource::Host
        } else if wire
            .agent_source
            .eq_ignore_ascii_case(crate::agent_source::AgentSource::WSL_KIND)
        {
            let Some(distro) = wire
                .wsl_distro
                .as_deref()
                .map(str::trim)
                .filter(|distro| !distro.is_empty())
            else {
                tracing::warn!(
                    target: "agent_rebind",
                    "ignoring WSL agent rebind without a distro"
                );
                return;
            };
            crate::agent_source::AgentSource::Wsl {
                distro: distro.to_string(),
            }
        } else {
            tracing::warn!(
                target: "agent_rebind",
                source = %wire.agent_source,
                "ignoring agent rebind with an unknown source"
            );
            return;
        };

        let mut request = AgentReconnectRequest {
            operation_id: wire.operation_id,
            window_id: wire.window_id,
            generation: wire.generation,
            agent_id: wire.agent_id,
            acp_model: wire.acp_model,
            custom_model_selection: wire.custom_model_selection,
            agent_source,
        };
        if request.operation_id.trim().is_empty()
            || (self.last_agent_rebind_window_id.as_deref() == Some(request.window_id.as_str())
                && request.generation <= self.last_agent_rebind_generation)
        {
            return;
        }
        if request.agent_source != self.current_agent_source {
            tracing::warn!(
                target: "agent_rebind",
                operation_id = %request.operation_id,
                generation = request.generation,
                current_source = %self.current_agent_source,
                requested_source = %request.agent_source,
                "ignoring execution-source-changing agent rebind"
            );
            return;
        }
        if !crate::agent_registry::is_known_id(&request.agent_id)
            || (self.host_agent_allowlist_present
                && !self
                    .allowed_agent_ids
                    .iter()
                    .any(|id| id.eq_ignore_ascii_case(&request.agent_id)))
        {
            tracing::warn!(
                target: "agent_rebind",
                operation_id = %request.operation_id,
                generation = request.generation,
                agent_id = %request.agent_id,
                source = %request.agent_source,
                "ignoring unsupported or policy-blocked agent rebind"
            );
            return;
        }

        request.agent_id.make_ascii_lowercase();
        request.acp_model = request
            .acp_model
            .take()
            .filter(|model| !model.trim().is_empty());
        request.custom_model_selection = request
            .custom_model_selection
            .take()
            .filter(|selection| !selection.trim().is_empty());
        if !matches!(request.agent_source, crate::agent_source::AgentSource::Host)
            || crate::agent_registry::lookup_profile_by_id(&request.agent_id).byok_mode
                == crate::agent_registry::ByokMode::Unsupported
        {
            request.custom_model_selection = None;
        }

        self.last_agent_rebind_window_id = Some(request.window_id.clone());
        self.last_agent_rebind_generation = request.generation;
        self.prepare_agent_reconnect(&request);

        let disconnect_in_progress = matches!(
            &self.agent_reconnect_state,
            AgentReconnectState::Disconnecting(_)
        );
        self.agent_reconnect_state = AgentReconnectState::Disconnecting(request.clone());
        if !disconnect_in_progress
            && self
                .restart_tx
                .send(AgentLifecycleRequest::RebindAgent(request))
                .is_err()
        {
            let _ = self.begin_pending_agent_reconnect_preflight();
        }
    }

    fn completed_turn_hit_at(&self, column: u16, row: u16) -> Option<CompletedTurnHitRegion> {
        let tab = self.current_tab();
        if self.mode != AppMode::Chat
            || tab.current_view != View::Chat
            || self.help_overlay_visible
            || tab.model_picker_open
            || tab.agent_picker_open
            || self.command_popup_visible()
        {
            return None;
        }
        self.completed_turn_hits
            .iter()
            .copied()
            .find(|hit| hit.contains(column, row))
    }

    fn active_mouse_tab_id(&self) -> String {
        self.tab_id
            .clone()
            .unwrap_or_else(|| DEFAULT_TAB_ID.to_string())
    }

    fn input_dialog_at(&self, column: u16, row: u16) -> bool {
        let tab = self.current_tab();
        let Some(area) = self.input_dialog_area else {
            return false;
        };
        self.mode == AppMode::Chat
            && tab.current_view == View::Chat
            && tab.input_can_receive_nav_focus()
            && !self.help_overlay_visible
            && !self.command_popup_visible()
            && column >= area.x
            && column < area.x.saturating_add(area.width)
            && row >= area.y
            && row < area.y.saturating_add(area.height)
    }

    fn cancel_completed_turn_click(&mut self) {
        self.pressed_completed_turn = None;
        self.last_completed_turn_click = None;
        self.pressed_input_dialog_tab = None;
    }

    fn restore_completed_turn_click(&mut self, column: u16, row: u16) {
        let active_tab_id = self.active_mouse_tab_id();
        let Some(click) = self.last_completed_turn_click.take().filter(|click| {
            click.tab_id == active_tab_id && click.column == column && click.row == row
        }) else {
            return;
        };
        let tab = self.current_tab_mut();
        let Some(turn) = tab.completed_turns.get(click.turn_index) else {
            return;
        };
        if turn.expanded != click.previous_expanded {
            tab.toggle_completed_turn(click.turn_index);
        }
        tab.selected_completed_turn_idx = click.previous_selected_index;
        tab.completed_turn_selection_visible_pending = click.previous_selection_pending;
    }

    fn copy_text_selection(&mut self) -> bool {
        let Some(text) = self.text_selection.selected_text() else {
            return false;
        };
        match crate::win32::copy_text_to_clipboard(&text) {
            Ok(()) => {
                self.text_selection.clear();
                self.close_pane_armed_at = None;
                self.transient_hint = Some((
                    t!("system.selection_copied").into_owned(),
                    std::time::Instant::now() + SELECTION_COPIED_HINT_WINDOW,
                ));
            }
            Err(error) => {
                self.transient_hint = None;
                tracing::warn!(
                    target: "clipboard",
                    error = %error,
                    "failed to copy mouse-selected text"
                );
            }
        }
        true
    }

    pub(super) fn default_paste_request_for_current_tab(&self) -> Option<String> {
        let tab = self.current_tab();
        if self.mode != AppMode::Chat
            || tab.current_view != View::Chat
            || !tab.pane_open
            || !tab.input_can_receive_nav_focus()
            || self.help_overlay_visible
            || self.command_popup_visible()
        {
            return None;
        }
        Some(
            serde_json::json!({
                "type": "event",
                "method": "request_default_paste",
                "params": {
                    "window_id": self.window_id.as_deref()?,
                    "tab_id": self.tab_id.as_deref()?,
                    "pane_id": self.pane_id.as_deref()?,
                }
            })
            .to_string(),
        )
    }

    pub(super) fn handle_right_click(&mut self) -> Option<String> {
        self.cancel_completed_turn_click();
        if self.copy_text_selection() {
            return None;
        }
        let Some(request) = self.default_paste_request_for_current_tab() else {
            let tab = self.current_tab();
            tracing::debug!(
                target: "agent_paste",
                mode = ?self.mode,
                view = ?tab.current_view,
                pane_open = tab.pane_open,
                input_can_receive_focus = tab.input_can_receive_nav_focus(),
                help_overlay_visible = self.help_overlay_visible,
                command_popup_visible = self.command_popup_visible(),
                window_id = ?self.window_id,
                tab_id = ?self.tab_id,
                pane_id = ?self.pane_id,
                "right-click Default Paste request was gated"
            );
            return None;
        };
        self.current_tab_mut().clear_completed_turn_selection();
        tracing::debug!(target: "agent_paste", "publishing right-click Default Paste request");
        Some(request)
    }

    pub(super) fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Key(key) => {
                self.cancel_completed_turn_click();
                let is_copy = matches!(key.code, KeyCode::Char('c'))
                    && key.modifiers.contains(KeyModifiers::CONTROL);
                if is_copy && self.copy_text_selection() {
                    return;
                }
                self.text_selection.clear();
                self.handle_key(key);
            }
            AppEvent::Mouse(mouse) => match mouse.kind {
                crossterm::event::MouseEventKind::ScrollUp
                | crossterm::event::MouseEventKind::ScrollDown
                    if self.current_tab().current_view == View::Agents =>
                {
                    self.cancel_completed_turn_click();
                    self.text_selection.clear();
                    let code = if matches!(mouse.kind, crossterm::event::MouseEventKind::ScrollUp) {
                        KeyCode::Up
                    } else {
                        KeyCode::Down
                    };
                    self.handle_key(KeyEvent::new(code, mouse.modifiers));
                }
                crossterm::event::MouseEventKind::ScrollUp
                | crossterm::event::MouseEventKind::ScrollDown
                    if self.mode == AppMode::Chat
                        && self.current_tab().current_view == View::Chat =>
                {
                    self.cancel_completed_turn_click();
                    self.text_selection.clear();
                    let lines = if mouse.modifiers.contains(KeyModifiers::ALT) {
                        1
                    } else {
                        3
                    };
                    match mouse.kind {
                        crossterm::event::MouseEventKind::ScrollUp => {
                            self.current_tab_mut().chat_scroll.by(lines);
                        }
                        crossterm::event::MouseEventKind::ScrollDown => {
                            self.current_tab_mut().chat_scroll.by(-lines);
                        }
                        _ => {}
                    }
                }
                crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Right) => {
                    if let Some(request) = self.handle_right_click() {
                        send_wt_protocol_event(request);
                    }
                }
                crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                    self.text_selection.handle_mouse(mouse);
                    let click_count = self.text_selection.click_count().unwrap_or(1);
                    if click_count > 1 {
                        self.pressed_completed_turn = None;
                        if click_count == 2 {
                            self.restore_completed_turn_click(mouse.column, mouse.row);
                        }
                        return;
                    }
                    self.last_completed_turn_click = None;
                    if self.input_dialog_at(mouse.column, mouse.row) {
                        self.pressed_input_dialog_tab = Some(self.active_mouse_tab_id());
                        self.pressed_completed_turn = None;
                        return;
                    }
                    self.pressed_completed_turn = self
                        .completed_turn_hit_at(mouse.column, mouse.row)
                        .map(|hit| PressedCompletedTurn {
                            tab_id: self.active_mouse_tab_id(),
                            hit,
                        });
                }
                crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left) => {
                    self.cancel_completed_turn_click();
                    self.text_selection.handle_mouse(mouse);
                }
                crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left) => {
                    let active_tab_id = self.active_mouse_tab_id();
                    let input_pressed = self.pressed_input_dialog_tab.take();
                    if input_pressed.as_deref() == Some(active_tab_id.as_str())
                        && self.input_dialog_at(mouse.column, mouse.row)
                    {
                        self.text_selection.handle_mouse(mouse);
                        self.pressed_completed_turn = None;
                        self.last_completed_turn_click = None;
                        self.current_tab_mut().clear_completed_turn_selection();
                        return;
                    }
                    let pressed = self.pressed_completed_turn.take();
                    let released = self.completed_turn_hit_at(mouse.column, mouse.row);
                    self.text_selection.handle_mouse(mouse);
                    if let Some(pressed) = pressed.filter(|pressed| {
                        pressed.tab_id == active_tab_id
                            && released.is_some_and(|hit| {
                                hit.turn_index == pressed.hit.turn_index
                                    && hit.kind == pressed.hit.kind
                            })
                    }) {
                        let tab = self.current_tab_mut();
                        match pressed.hit.kind {
                            CompletedTurnHitKind::ToolCall { detail_index } => {
                                tab.toggle_completed_tool_call(
                                    pressed.hit.turn_index,
                                    detail_index,
                                );
                                return;
                            }
                            CompletedTurnHitKind::ToolGroup {
                                first_detail_index,
                                detail_count,
                            } => {
                                tab.toggle_completed_tool_group(
                                    pressed.hit.turn_index,
                                    first_detail_index,
                                    detail_count,
                                );
                                return;
                            }
                            _ => {}
                        }
                        let previous_selected_index = tab.selected_completed_turn_idx;
                        let previous_selection_pending =
                            tab.completed_turn_selection_visible_pending;
                        let previous_expanded =
                            tab.completed_turns[pressed.hit.turn_index].expanded;
                        if tab.select_completed_turn(pressed.hit.turn_index)
                            && tab.toggle_completed_turn(pressed.hit.turn_index)
                            && pressed.hit.kind == CompletedTurnHitKind::UserInput
                        {
                            self.last_completed_turn_click = Some(CompletedTurnClickRecord {
                                tab_id: active_tab_id,
                                column: mouse.column,
                                row: mouse.row,
                                turn_index: pressed.hit.turn_index,
                                previous_selected_index,
                                previous_selection_pending,
                                previous_expanded,
                            });
                        }
                    }
                }
                _ => {
                    self.text_selection.handle_mouse(mouse);
                }
            },
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
                    if tab.turn.is_in_flight() {
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
                self.cancel_completed_turn_click();
                self.text_selection.clear();
                self.terminal_cols = w;
                self.terminal_rows = h;
            }
            AppEvent::RevealTick => {
                self.advance_reveal();
            }
            AppEvent::FocusChanged(focused) => {
                self.cancel_completed_turn_click();
                self.pane_focused = focused;
            }
            AppEvent::ConnectionStage(stage) => {
                self.state = ConnectionState::Connecting(stage);
                self.publish_agent_status();
            }
            AppEvent::CloudModelsAvailable(models) => {
                if matches!(
                    &self.current_agent_source,
                    crate::agent_source::AgentSource::Host
                ) {
                    self.set_cloud_models(models);
                    self.publish_agent_status();
                } else {
                    tracing::warn!(
                        target: "cloud_models",
                        agent_source = %self.current_agent_source,
                        "ignoring Host cloud catalog delivery for WSL helper"
                    );
                }
            }
            AppEvent::AgentReconnectReady(completed) => {
                let Some(latest) = self.begin_pending_agent_reconnect_preflight() else {
                    tracing::debug!(
                        target: "agent_rebind",
                        operation_id = %completed.operation_id,
                        generation = completed.generation,
                        "ignoring reconnect-ready event with no disconnect pending"
                    );
                    return;
                };
                tracing::info!(
                    target: "agent_rebind",
                    completed_operation_id = %completed.operation_id,
                    completed_generation = completed.generation,
                    target_operation_id = %latest.operation_id,
                    target_generation = latest.generation,
                    target_agent_id = %latest.agent_id,
                    "old ACP transport retired; preflighting latest target"
                );
            }
            AppEvent::AgentClientFailed => {
                if let Some(latest) = self.begin_pending_agent_reconnect_preflight() {
                    self.suppress_next_failed_client_error = true;
                    tracing::info!(
                        target: "agent_rebind",
                        operation_id = %latest.operation_id,
                        generation = latest.generation,
                        agent_id = %latest.agent_id,
                        "outgoing ACP client failed during startup; preflighting latest target"
                    );
                }
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
                session_capabilities_ready,
            } => {
                self.pending_yolo_reconciles.clear();
                self.agent_name = name;
                self.agent_model = model;
                self.agent_version = version;
                self.session_id = session_id.clone();
                self.auth_recovery_state = AuthRecoveryState::Idle;
                let (available_models, current_model_id) = self
                    .session_model_configs
                    .entry(session_id.clone())
                    .or_insert((available_models, current_model_id))
                    .clone();
                self.agent_models = available_models;
                self.agent_current_model_id = current_model_id;
                self.rebuild_model_catalog_from_agent_state();
                self.agent_supports_load_session = load_session_supported;
                self.agent_supports_image = image_supported;
                self.state = ConnectionState::Connected;
                // A successful connect resolves any in-flight auth recovery:
                // bump the generation so a still-pending dead-man timer becomes
                // stale and can't later force the sign-in screen.
                self.auth_recovery_generation = self.auth_recovery_generation.wrapping_add(1);
                self.proposal_channels.set_agent_transport_available(true);
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
                    .owner_tab_id
                    .clone()
                    .or_else(|| self.tab_id.clone())
                    .unwrap_or_else(|| DEFAULT_TAB_ID.to_string());
                if session_capabilities_ready {
                    self.pending_yolo_session_tabs.remove(&bind_tab);
                } else {
                    self.pending_yolo_session_tabs.insert(bind_tab.clone());
                }
                self.session_to_tab
                    .insert(session_id.clone(), bind_tab.clone());
                let tab = self.tab_mut(&bind_tab);
                if tab.session_id.as_deref() != Some(session_id.as_str()) {
                    tab.usage = None;
                    tab.usage_staleness = crate::usage::UsageStaleness::default();
                }
                tab.session_id = Some(session_id.clone());
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
                if session_capabilities_ready {
                    self.reconcile_session_yolo(&session_id);
                }
                self.publish_agent_status();
            }
            AppEvent::SessionAttached {
                tab_id,
                session_id,
                available_models,
                current_model_id,
            } => {
                let is_active_tab = self.active_tab_key() == tab_id;
                let replaced_session_ids: Vec<String> = self
                    .session_to_tab
                    .iter()
                    .filter(|(known_session_id, known_tab_id)| {
                        *known_tab_id == &tab_id && *known_session_id != &session_id
                    })
                    .map(|(known_session_id, _)| known_session_id.clone())
                    .collect();
                for replaced_session_id in replaced_session_ids {
                    self.session_to_tab.remove(&replaced_session_id);
                    self.session_model_configs.remove(&replaced_session_id);
                    self.clear_yolo_session_state(&replaced_session_id);
                    self.session_config_options.remove(&replaced_session_id);
                    self.session_commands.remove(&replaced_session_id);
                }
                self.session_to_tab
                    .insert(session_id.clone(), tab_id.clone());
                self.pending_yolo_session_tabs.remove(&tab_id);
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
                let (available_models, current_model_id) = self
                    .session_model_configs
                    .entry(session_id.clone())
                    .or_insert((available_models, current_model_id))
                    .clone();
                if is_active_tab {
                    self.agent_models = available_models;
                    self.agent_current_model_id = current_model_id;
                    self.rebuild_model_catalog_from_agent_state();
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
                        self.send_session_model(Some(session_id.clone()), model, false);
                    }
                }
                let reconciled_by_client = self
                    .yolo_state
                    .lock()
                    .unwrap()
                    .take_client_reconciled(&session_id);
                if !reconciled_by_client {
                    self.reconcile_session_yolo(&session_id);
                }
                self.publish_agent_status();
            }
            AppEvent::UsageReported {
                session_id,
                snapshot,
            } => {
                let target_tab = self.tab_for_session(&session_id);
                let tab = self.tab_mut(&target_tab);
                tab.usage_staleness.mark_reported(&snapshot);
                if let Some(current) = tab.usage.as_mut() {
                    current.merge(snapshot);
                } else {
                    tab.usage = Some(snapshot);
                }
                self.project_tab_state(&target_tab);
            }
            AppEvent::UsageCleared { session_id } => {
                let target_tab = self.tab_for_session(&session_id);
                let tab = self.tab_mut(&target_tab);
                tab.usage = None;
                tab.usage_staleness = crate::usage::UsageStaleness::default();
                self.project_tab_state(&target_tab);
            }
            AppEvent::ModelConfigUpdated {
                session_id,
                available_models,
                current_model_id,
            } => {
                self.session_model_configs.insert(
                    session_id.clone(),
                    (available_models.clone(), current_model_id.clone()),
                );
                if self.current_tab().session_id.as_deref() == Some(session_id.as_str()) {
                    self.agent_models = available_models;
                    self.agent_current_model_id = current_model_id;
                    self.rebuild_model_catalog_from_agent_state();
                    self.publish_agent_status();
                }
            }
            AppEvent::YoloModeChangeCompleted {
                transaction_id,
                session_id,
                enabled,
                restart_required,
                result,
            } => {
                if restart_required {
                    tracing::error!(
                        target: "yolo",
                        error = %result.as_ref().err().map(String::as_str).unwrap_or("unknown provider state"),
                        "provider-native Yolo outcome is unknown; restarting agent stack fail-closed"
                    );
                    let _ = self
                        .restart_tx
                        .send(crate::protocol::acp::client::AgentLifecycleRequest::RestartMaster);
                }
                self.complete_yolo_change(
                    transaction_id,
                    session_id,
                    enabled,
                    restart_required,
                    result,
                );
            }
            AppEvent::RuntimeYoloReconcileCompleted {
                reconcile_id,
                fail_closed,
                restart_required,
                result,
            } => {
                if reconcile_id != 0 && !self.pending_yolo_reconciles.contains_key(&reconcile_id) {
                    tracing::debug!(
                        target: "yolo",
                        reconcile_id,
                        "ignoring stale provider-native Yolo reconciliation completion"
                    );
                    return;
                }
                let settled = self.pending_yolo_reconciles.get(&reconcile_id).is_some_and(
                    |(_, pending_fail_closed)| {
                        result.is_ok() || (!*pending_fail_closed && !restart_required)
                    },
                );
                if settled {
                    self.pending_yolo_reconciles.remove(&reconcile_id);
                }
                if result.is_err() && (fail_closed || restart_required) {
                    tracing::error!(
                        target: "yolo",
                        error = %result.as_ref().unwrap_err(),
                        unknown_outcome = restart_required,
                        "provider-native Yolo reconciliation failed; restarting agent stack fail-closed"
                    );
                    let _ = self
                        .restart_tx
                        .send(crate::protocol::acp::client::AgentLifecycleRequest::RestartMaster);
                }
            }
            AppEvent::ModelSetCompleted {
                session_id,
                model,
                pane_override,
            } => {
                let target_tab = self.bound_tab_for_session(&session_id);
                let Some(target_tab) = target_tab else {
                    return;
                };
                if let Some((_, current_model_id)) = self.session_model_configs.get_mut(&session_id)
                {
                    *current_model_id = Some(model.clone());
                }
                if pane_override {
                    let name = self.model_display_name(&model);
                    let tab = self.tab_mut(&target_tab);
                    tab.model_override = Some(model.clone());
                    tab.messages.push(ChatMessage::success(
                        t!("system.model_set", model = name.as_str()).into_owned(),
                    ));
                    tab.scroll_to_bottom();
                }
                if self.current_tab().session_id.as_deref() == Some(session_id.as_str()) {
                    self.agent_current_model_id = Some(model);
                    self.rebuild_model_catalog_from_agent_state();
                    self.publish_agent_status();
                }
            }
            AppEvent::ModelSetFailed {
                session_id,
                model,
                pane_override,
                message,
            } => {
                let target_tab = self.bound_tab_for_session(&session_id);
                let Some(target_tab) = target_tab else {
                    return;
                };
                if pane_override {
                    let name = self.model_display_name(&model);
                    let tab = self.tab_mut(&target_tab);
                    tab.messages.push(ChatMessage::error(
                        t!(
                            "system.config_update_failed",
                            option = name.as_str(),
                            error = message.as_str()
                        )
                        .into_owned(),
                    ));
                    tab.scroll_to_bottom();
                }
            }
            AppEvent::SessionConfigUpdated {
                session_id,
                options,
            } => {
                self.session_config_options
                    .insert(session_id.clone(), options);
                let target_tab = self.bound_tab_for_session(&session_id);
                let Some(target_tab) = target_tab else {
                    return;
                };
                let options = self
                    .session_config_options
                    .get(&session_id)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                let mut picker = self
                    .tab_sessions
                    .get(&target_tab)
                    .map(|tab| tab.config_picker.clone())
                    .unwrap_or_default();
                picker.reconcile(options);
                self.tab_mut(&target_tab).config_picker = picker;
            }
            AppEvent::SessionCommandsUpdated {
                session_id,
                commands,
            } => {
                self.session_commands.insert(session_id, commands);
            }
            AppEvent::SessionConfigSetCompleted {
                session_id,
                config_id,
                value,
                model_compat,
            } => {
                let (option_name, value_name) = self
                    .session_config_options
                    .get_mut(&session_id)
                    .and_then(|options| options.iter_mut().find(|option| option.id == config_id))
                    .map(|option| {
                        option.current_value = value.clone();
                        (option.name.clone(), option.current_value_name().to_string())
                    })
                    .unwrap_or_else(|| (config_id.clone(), value.clone()));
                let target_tab = self.bound_tab_for_session(&session_id);
                let Some(target_tab) = target_tab else {
                    return;
                };
                {
                    let tab = self.tab_mut(&target_tab);
                    if tab.config_pending_id.as_deref() == Some(config_id.as_str()) {
                        tab.config_pending_id = None;
                    }
                    let message = if model_compat {
                        tab.model_override = Some(value.clone());
                        t!("system.model_set", model = value_name.as_str()).into_owned()
                    } else {
                        format!("{option_name}: {value_name}")
                    };
                    tab.messages.push(ChatMessage::success(message));
                    tab.scroll_to_bottom();
                }
                if model_compat {
                    if let Some((_, current_model_id)) =
                        self.session_model_configs.get_mut(&session_id)
                    {
                        *current_model_id = Some(value.clone());
                    }
                    if self.current_tab().session_id.as_deref() == Some(session_id.as_str()) {
                        self.agent_current_model_id = Some(value);
                        self.rebuild_model_catalog_from_agent_state();
                        self.publish_agent_status();
                    }
                }
            }
            AppEvent::SessionConfigSetFailed {
                session_id,
                config_id,
                message,
            } => {
                let option_name = self
                    .session_config_options
                    .get(&session_id)
                    .and_then(|options| options.iter().find(|option| option.id == config_id))
                    .map(|option| option.name.clone())
                    .unwrap_or(config_id.clone());
                let target_tab = self.bound_tab_for_session(&session_id);
                let Some(target_tab) = target_tab else {
                    return;
                };
                let tab = self.tab_mut(&target_tab);
                if tab.config_pending_id.as_deref() == Some(config_id.as_str()) {
                    tab.config_pending_id = None;
                }
                tab.messages.push(ChatMessage::error(
                    t!(
                        "system.config_update_failed",
                        option = option_name.as_str(),
                        error = message.as_str()
                    )
                    .into_owned(),
                ));
                tab.scroll_to_bottom();
            }
            AppEvent::TabError { tab_id, message } => {
                self.pending_yolo_session_tabs.remove(&tab_id);
                // Scoped error for a specific tab. Bypasses the global
                // auth-fallback / ConnectionState::Failed flip in
                // AgentError because the error is local to one tab's
                // session-load attempt, not the whole connection.
                let tab = self.tab_mut(&tab_id);
                tab.loading_session = false;
                tab.loading_target_session_id = None;
                tab.replay_agent_buffer.clear();
                tab.replay_user_buffer.clear();
                tab.timing_note = None;
                tab.turn = TurnState::Idle;
                tab.active_direct_proposal_id = None;
                tab.messages.push(ChatMessage::Error(message));
                tab.scroll_to_bottom();
            }
            AppEvent::TabSystemMessage { tab_id, message } => {
                let tab = self.tab_mut(&tab_id);
                tab.messages.push(ChatMessage::info(message));
                tab.scroll_to_bottom();
            }
            AppEvent::PromptTemplateLoaded { name } => {
                self.prompt_name = Some(name);
            }
            AppEvent::PromptTargetResolved {
                tab_id,
                prompt_id,
                pane_id,
            } => {
                self.apply_prompt_target_resolved(tab_id, prompt_id, pane_id);
            }
            AppEvent::AgentBusy { tab_id } => {
                let tab = self.tab_mut(&tab_id);
                tab.messages
                    .push(ChatMessage::warning(t!("system.agent_busy").into_owned()));
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
                if session_id.is_none()
                    && std::mem::take(&mut self.suppress_next_failed_client_error)
                {
                    return;
                }
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

                let session_survives = session_id.is_some()
                    && matches!(
                        &failure,
                        crate::protocol::acp::failure::AgentFailure::Protocol { .. }
                    );

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
                    if !session_survives {
                        self.state = ConnectionState::Failed(message.clone());
                        self.publish_agent_status();
                    }
                    let tab = match session_id.as_deref() {
                        Some(sid) => self.session_tab_mut(sid),
                        None => self.current_tab_mut(),
                    };
                    tab.activity_frame = 0;
                    tab.timing_note = None;
                    tab.turn = TurnState::Idle;
                    // Suppress only an identical consecutive error so repeated
                    // provider failures do not stack duplicate messages.
                    let is_duplicate = matches!(
                        tab.messages.last(),
                        Some(ChatMessage::Error(prev)) if prev == &message
                    );
                    if !is_duplicate {
                        tab.messages.push(ChatMessage::Error(message));
                    }
                }
            }
            AppEvent::MasterDisconnected => {
                self.agent_reconnect_state = AgentReconnectState::Idle;
                if self.deferred_acp.is_some() {
                    if !matches!(&self.auth_recovery_state, AuthRecoveryState::Idle) {
                        tracing::warn!(
                            target: "helper",
                            "master disconnected during auth recovery; explicit restart barrier owns reconnect"
                        );
                        return;
                    }
                    tracing::warn!(
                        target: "helper",
                        agent_id = %self.current_agent_id,
                        source = %self.current_agent_source,
                        "master disconnected; reconnecting retained helper over stable pipe"
                    );
                    self.reset_agent_scoped_state();
                    self.pending_acp_start = true;
                } else {
                    tracing::warn!(
                        target: "helper",
                        "master disconnected without deferred binding; terminating helper"
                    );
                    self.should_quit = true;
                }
            }
            AppEvent::PostLoginAuthRecovery {
                failure,
                tab_id,
                agent_id,
            } => {
                if std::mem::take(&mut self.suppress_next_failed_client_error) {
                    return;
                }
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
                let restart_request_id = uuid::Uuid::new_v4().to_string();
                self.auth_recovery_state = AuthRecoveryState::WaitingForMaster {
                    request_id: restart_request_id.clone(),
                };
                // (i) Transient "Reconnecting…" — NOT the sign-in screen. The
                // master restart below reconnects this retained helper, so the
                // common path never flashes the setup screen. Only a dropped
                // or slow restart leaves us in this state long enough for the
                // `AuthRecoveryTimedOut` handler to fall back to the sign-in screen.
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
                // `/restart` machinery. C++ publishes agent_master_restarted
                // only after the replacement has claimed the stable pipe.
                send_wt_protocol_event(
                    crate::wt_protocol_events::restart_agent_stack_event_with_id(
                        &restart_request_id,
                    ),
                );
                // (iii) Dead-man fallback for replacement-master readiness.
                // This phase must cover SharedWta's retirement budget. Once
                // readiness arrives, a fresh generation gets the usual,
                // shorter connection deadline.
                self.arm_auth_recovery_timeout(
                    resolved,
                    recovery_generation,
                    AUTH_RECOVERY_MASTER_READY_TIMEOUT,
                );
            }
            AppEvent::AuthRecoveryTimedOut {
                agent_id,
                generation,
            } => {
                // Only takes effect when the retained helper did not reconnect
                // within the window. Surface the sign-in fallback so the user
                // can retry instead of remaining on "Reconnecting…".
                //
                // The generation guard drops a stale timer: if a newer recovery
                // started, or the reconnect already succeeded (AgentConnected
                // bumps the generation), this no longer matches the current
                // recovery and must not force the sign-in screen.
                let timed_out_phase = if generation == self.auth_recovery_generation
                    && self.mode != AppMode::Setup
                    && matches!(self.state, ConnectionState::Connecting(_))
                {
                    match &self.auth_recovery_state {
                        AuthRecoveryState::WaitingForMaster { .. } => Some("master readiness"),
                        AuthRecoveryState::Connecting => Some("retained-helper connection"),
                        AuthRecoveryState::Idle => None,
                    }
                } else {
                    None
                };
                if let Some(phase) = timed_out_phase {
                    self.auth_recovery_state = AuthRecoveryState::Idle;
                    tracing::warn!(
                        target: "auth_recovery",
                        agent_id = %agent_id,
                        %phase,
                        "auth recovery phase did not complete within its deadline; \
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
                tab.messages.push(ChatMessage::warning(msg.into_owned()));
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
                if tab.loading_session {
                    if !tab.replay_user_buffer.is_empty() {
                        let text = std::mem::take(&mut tab.replay_user_buffer);
                        tab.messages.push(ChatMessage::User(text));
                    }
                    tab.replay_agent_buffer.push_str(&text);
                    return;
                }

                // Append directly to the ordered active transcript. The state
                // machine drops late chunks and stale autofix generations.
                self.turn_observe_chunk(&session_id, ChunkKind::Message, &text);
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
                if !tab.replay_agent_buffer.is_empty() {
                    let prev = std::mem::take(&mut tab.replay_agent_buffer);
                    tab.messages.push(ChatMessage::Agent(prev));
                }
                tab.replay_user_buffer.push_str(&text);
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
                kind,
                location,
                location_is_command,
                cwd,
                output,
                exit_code,
                content,
                locations,
            } => {
                let loading_session = self.session_tab(&session_id).loading_session;
                if !self.session_tab(&session_id).turn.is_in_flight() && !loading_session {
                    return;
                }
                if !loading_session {
                    self.turn_observe_chunk(&session_id, ChunkKind::Thought, "");
                }
                let tab = self.session_tab_mut(&session_id);
                // Commit streamed prose before the tool so the transcript
                // follows ACP event order instead of drawing the streaming
                // buffer after every eagerly inserted tool card.
                if tab.loading_session {
                    if !tab.replay_user_buffer.is_empty() {
                        let text = std::mem::take(&mut tab.replay_user_buffer);
                        tab.messages.push(ChatMessage::User(text));
                    }
                    if !tab.replay_agent_buffer.is_empty() {
                        let text = std::mem::take(&mut tab.replay_agent_buffer);
                        tab.messages.push(ChatMessage::Agent(text));
                    }
                }
                tab.messages.push(ChatMessage::ToolCall {
                    id,
                    title,
                    status,
                    kind,
                    location,
                    location_is_command,
                    cwd,
                    output,
                    exit_code,
                    content,
                    locations,
                });
                tab.scroll_to_bottom();
            }
            AppEvent::ToolCallUpdate {
                session_id,
                id,
                title,
                status,
                kind,
                location,
                location_is_command,
                output,
                content,
                locations,
                cwd,
                exit_code,
            } => {
                let tab = self.session_tab_mut(&session_id);
                if !tab.turn.is_in_flight() && !tab.loading_session {
                    return;
                }
                // Update in-place in messages
                for msg in &mut tab.messages {
                    if let ChatMessage::ToolCall {
                        id: ref mid,
                        title: ref mut current_title,
                        status: ref mut s,
                        kind: ref mut current_kind,
                        location: ref mut loc,
                        location_is_command: ref mut loc_is_cmd,
                        cwd: ref mut current_cwd,
                        output: ref mut current_output,
                        exit_code: ref mut current_exit_code,
                        content: ref mut current_content,
                        locations: ref mut current_locations,
                        ..
                    } = msg
                    {
                        if mid == &id {
                            if let Some(title) = &title {
                                *current_title = title.clone();
                            }
                            if let Some(status) = &status {
                                *s = status.clone();
                            }
                            if let Some(kind) = kind {
                                *current_kind = kind;
                            }
                            // Only overwrite when the update actually carried
                            // a fresh location — `None` means "unchanged",
                            // not "clear it" (see `AppEvent::ToolCallUpdate`).
                            if location.is_some() || locations.is_some() {
                                *loc = location.clone();
                                *loc_is_cmd = location_is_command;
                            }
                            if let Some(output) = &output {
                                *current_output = (!output.text.is_empty()).then(|| output.clone());
                            }
                            if let Some(content) = &content {
                                *current_content = content.clone();
                            }
                            if let Some(locations) = &locations {
                                *current_locations = locations.clone();
                            }
                            if let Some(cwd) = &cwd {
                                *current_cwd = (!cwd.is_empty()).then(|| cwd.clone());
                            }
                            if let Some(exit_code) = exit_code {
                                *current_exit_code = Some(exit_code);
                            }
                        }
                    }
                }
            }
            AppEvent::ToolTerminalOutput {
                session_id,
                terminal_id,
                output,
                exit_code,
            } => {
                let tab = self.session_tab_mut(&session_id);
                let update_content = |message: &mut ChatMessage| -> bool {
                    let ChatMessage::ToolCall {
                        output: card_output,
                        exit_code: card_exit_code,
                        content,
                        ..
                    } = message
                    else {
                        return false;
                    };
                    let mut contains_terminal = false;
                    for item in content {
                        if let crate::app::ToolCallContent::Terminal {
                            id,
                            output: current_output,
                            exit_code: current_exit_code,
                        } = item
                        {
                            if id == &terminal_id {
                                contains_terminal = true;
                                *current_output = Some(output.clone());
                                if exit_code.is_some() {
                                    *current_exit_code = exit_code;
                                }
                            }
                        }
                    }
                    if contains_terminal {
                        *card_output = Some(output.clone());
                        if exit_code.is_some() {
                            *card_exit_code = exit_code;
                        }
                    }
                    contains_terminal
                };
                for message in &mut tab.messages {
                    update_content(message);
                }
                let mut invalidated_turns = Vec::new();
                for (index, turn) in tab.completed_turns.iter_mut().enumerate() {
                    let mut changed = false;
                    for message in &mut turn.details {
                        changed |= update_content(message);
                    }
                    if changed {
                        invalidated_turns.push(index);
                    }
                }
                for index in invalidated_turns {
                    tab.invalidate_completed_turn_height(index);
                }
            }
            AppEvent::HideToolCall { session_id, id } => {
                let tab = self.session_tab_mut(&session_id);
                tab.messages.retain(
                    |message| !matches!(message, ChatMessage::ToolCall { id: message_id, .. } if message_id == &id),
                );
            }
            AppEvent::Plan {
                session_id,
                entries,
            } => {
                let loading_session = self.session_tab(&session_id).loading_session;
                if !self.session_tab(&session_id).turn.is_in_flight() && !loading_session {
                    return;
                }
                if !loading_session {
                    self.turn_observe_chunk(&session_id, ChunkKind::Thought, "");
                }
                let tab = self.session_tab_mut(&session_id);
                if tab.loading_session {
                    if !tab.replay_user_buffer.is_empty() {
                        let text = std::mem::take(&mut tab.replay_user_buffer);
                        tab.messages.push(ChatMessage::User(text));
                    }
                    if !tab.replay_agent_buffer.is_empty() {
                        let text = std::mem::take(&mut tab.replay_agent_buffer);
                        tab.messages.push(ChatMessage::Agent(text));
                    }
                }
                tab.messages.push(ChatMessage::Plan(entries));
                tab.scroll_to_bottom();
            }
            AppEvent::PermissionRequest {
                session_id,
                tool_call_id,
                description,
                title,
                kind_label,
                target,
                target_is_command,
                options,
                responder,
            } => {
                let tab = self.session_tab_mut(&session_id);
                if !tab.turn.can_service_agent_request() && !tab.loading_session {
                    // Auto-deny only when no turn remains to own the request.
                    // A surfaced turn may have released its UI busy gate while
                    // the Agent continues a multi-step flow.
                    return;
                }
                // FIFO push — never overwrite an in-flight request. The
                // user sees them one at a time (front of the queue is the
                // one rendered + key-handled); resolving the front pops
                // it and exposes the next.
                tab.permission.push_back(PermissionState {
                    tool_call_id,
                    description,
                    title,
                    kind_label,
                    target,
                    target_is_command,
                    options,
                    selected: 0,
                    responder: Some(responder),
                });
            }
            AppEvent::UserInputRequest {
                request_id,
                session_id,
                request,
                responder,
            } => {
                let tab = self.session_tab_mut(&session_id);
                if !tab.turn.can_service_agent_request() && !tab.loading_session {
                    return;
                }
                tab.user_input.push_back(UserInputState {
                    request_id,
                    request,
                    selected: 0,
                    input: String::new(),
                    responder: Some(responder),
                });
            }
            AppEvent::CancelUserInputRequest {
                request_id,
                session_id,
            } => {
                let tab = self.session_tab_mut(&session_id);
                if let Some(index) = tab
                    .user_input
                    .iter()
                    .position(|pending| pending.request_id == request_id)
                {
                    tab.user_input.remove(index);
                }
            }
            AppEvent::SystemMessage(message) => {
                self.current_tab_mut()
                    .messages
                    .push(ChatMessage::error(message));
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
                if !matches!(&self.agent_reconnect_state, AgentReconnectState::Idle) {
                    tracing::debug!(
                        target: "preflight",
                        stale_agent = %result.agent_id,
                        current_agent = %self.current_agent_id,
                        "ignoring startup preflight result during agent rebind"
                    );
                    return;
                }
                if !self.current_agent_id.is_empty()
                    && !result.agent_id.eq_ignore_ascii_case(&self.current_agent_id)
                {
                    tracing::debug!(
                        target: "preflight",
                        stale_agent = %result.agent_id,
                        current_agent = %self.current_agent_id,
                        "ignoring stale preflight result after agent rebind"
                    );
                    return;
                }
                tracing::info!(
                    target: "preflight",
                    agent = %result.agent_id,
                    cli_status = ?result.cli_status,
                    auth_status = ?result.auth_status,
                    "preflight result received"
                );
                if !result.all_passed() {
                    self.show_preflight_setup(result);
                }
            }
            AppEvent::AgentReconnectPreflightComplete {
                operation_id,
                generation,
                result,
            } => {
                let matches_pending = match &self.agent_reconnect_state {
                    AgentReconnectState::Preflighting(pending) => {
                        pending.operation_id == operation_id
                            && pending.generation == generation
                            && pending.agent_id.eq_ignore_ascii_case(&result.agent_id)
                    }
                    _ => false,
                };
                if !matches_pending {
                    tracing::debug!(
                        target: "agent_rebind",
                        operation_id = %operation_id,
                        generation,
                        agent_id = %result.agent_id,
                        "ignoring stale target preflight result"
                    );
                    return;
                }

                self.agent_reconnect_state = AgentReconnectState::Idle;
                if result.all_passed() {
                    self.pending_acp_start = true;
                    tracing::info!(
                        target: "agent_rebind",
                        operation_id = %operation_id,
                        generation,
                        agent_id = %result.agent_id,
                        "target preflight passed; reconnecting helper"
                    );
                } else {
                    self.show_preflight_setup(result);
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
                            .push(ChatMessage::warning(t!("system.no_agents").into_owned()));
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
            AppEvent::DirectTerminalActionProposal {
                context,
                payload,
                source,
                responder,
            } => {
                let decision =
                    self.evaluate_direct_terminal_action_proposal(&context, &payload, source);
                let _ = responder.send(decision);
            }
            AppEvent::DirectTerminalActionProposalCommit {
                proposal_id,
                responder,
            } => {
                let committed = self.commit_terminal_action_proposal(&proposal_id);
                if !committed {
                    self.proposal_channels.resolve_final(
                        &proposal_id,
                        crate::agent_tools::action_proposal::channel::ProposalFinalStatus::Cancelled,
                    );
                }
                let _ = responder.send(committed);
            }
            AppEvent::DirectTerminalActionProposalInvalidate {
                proposal_id,
                session_id,
            } => {
                self.invalidate_terminal_action_proposal(&proposal_id, &session_id);
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
                        crate::protocol::acp::client::MasterExtRequest::SessionBornBound { event },
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
                    let prompt = params.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
                    tracing::info!(target: "autofix", prompt_len = prompt.len(), "agent_prompt: delegating");
                    if !prompt.is_empty() {
                        self.delegate_to_tab_agent(prompt);
                    }
                    return;
                }

                if method == "agent_master_restarted" {
                    let request_id = params
                        .get("operation_id")
                        .and_then(|value| value.as_str())
                        .unwrap_or("");
                    if request_id.is_empty() {
                        return;
                    }
                    let completes_auth_recovery = matches!(
                        &self.auth_recovery_state,
                        AuthRecoveryState::WaitingForMaster {
                            request_id: expected
                        } if expected == request_id
                    );
                    // Session retirement can fail an in-flight startup before
                    // the old master exits, so no later transport-loss event
                    // remains to trigger the retained helper's reconnect.
                    let recovers_retirement_failure =
                        matches!(&self.auth_recovery_state, AuthRecoveryState::Idle)
                            && matches!(&self.state, ConnectionState::Failed(_));
                    if !completes_auth_recovery && !recovers_retirement_failure {
                        return;
                    }
                    if self.deferred_acp.is_none() {
                        tracing::warn!(
                            target: "helper",
                            %request_id,
                            "replacement master is ready but helper binding is unavailable"
                        );
                        return;
                    }

                    tracing::info!(
                        target: "helper",
                        %request_id,
                        "replacement master is ready; reconnecting retained helper"
                    );
                    if completes_auth_recovery {
                        // Invalidate the longer master-readiness timer and arm
                        // the normal connection deadline for this new phase.
                        self.auth_recovery_generation =
                            self.auth_recovery_generation.wrapping_add(1);
                        self.auth_recovery_state = AuthRecoveryState::Connecting;
                        let agent_id = self
                            .deferred_acp
                            .as_ref()
                            .and_then(|params| params.agent_id.clone())
                            .filter(|agent_id| !agent_id.is_empty())
                            .unwrap_or_else(|| {
                                if self.current_agent_id.is_empty() {
                                    "copilot".to_string()
                                } else {
                                    self.current_agent_id.clone()
                                }
                            });
                        self.arm_auth_recovery_timeout(
                            agent_id,
                            self.auth_recovery_generation,
                            AUTH_RECOVERY_CONNECTION_TIMEOUT,
                        );
                    }
                    self.reset_agent_scoped_state();
                    self.pending_acp_start = true;
                    return;
                }

                if method == "rebind_agent" {
                    self.handle_agent_rebind(params);
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
                    // Agent identity and launch-time model changes use the
                    // separate same-helper rebind event.
                    let target_window = params
                        .get("window_id")
                        .and_then(|value| value.as_str())
                        .unwrap_or("");
                    let owner_window = self.window_id.as_deref().unwrap_or("");
                    if !target_window.is_empty()
                        && !owner_window.is_empty()
                        && target_window != owner_window
                    {
                        return;
                    }
                    let target_tab = params
                        .get("tab_id")
                        .and_then(|value| value.as_str())
                        .unwrap_or("");
                    let owner_tab = self.owner_tab_id.as_deref().unwrap_or("");
                    if !target_tab.is_empty() && !owner_tab.is_empty() && target_tab != owner_tab {
                        return;
                    }

                    if let Some(enabled) = params.get("autofix_enabled").and_then(|v| v.as_bool()) {
                        tracing::info!(
                            target: "autofix",
                            old = self.autofix_enabled,
                            new = enabled,
                            "autofix_enabled hot-reloaded from settings change",
                        );
                        self.autofix_enabled = enabled;
                    }

                    self.apply_runtime_yolo_config(
                        params.get("yolo_enabled").and_then(|v| v.as_bool()),
                        params.get("yolo_command_blocked").and_then(|v| v.as_bool()),
                    );

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

                    // acp-model is scoped by both the authoritative global agent
                    // id and this helper's spawn-time follow mode. Helpers pinned
                    // to another agent/profile, and panes with a local `/model`
                    // override, keep their existing model.
                    if let Some(raw) = params.get("acp_model").and_then(|v| v.as_str()) {
                        if let Some(target_agent_id) =
                            params.get("target_agent_id").and_then(|v| v.as_str())
                        {
                            tracing::info!(
                            target: "autofix",
                            model = raw,
                                target_agent_id,
                                "scoped acp-model hot-update requested from settings change",
                            );
                            self.apply_global_acp_model(target_agent_id, Some(raw.to_string()));
                        } else {
                            tracing::warn!(
                                target: "autofix",
                                "ignoring acp-model hot-update without target_agent_id"
                            );
                        }
                    }
                    let host_catalog_fields_present = params.get("cloud_models").is_some()
                        || params.get("custom_models").is_some()
                        || params.get("custom_model_selection").is_some();
                    let accepts_host_catalog = matches!(
                        &self.current_agent_source,
                        crate::agent_source::AgentSource::Host
                    );
                    let catalog_delivery = accepts_host_catalog
                        && (params.get("cloud_models").is_some()
                            || params.get("custom_models").is_some());
                    if accepts_host_catalog {
                        if let Some(value) = params.get("cloud_models") {
                            match serde_json::from_value::<Vec<AcpModelInfo>>(value.clone()) {
                                Ok(models) => self.set_cloud_models(models),
                                Err(error) => {
                                    tracing::error!(
                                                    target: "cloud_models",
                                                    %error,
                                                    "invalid cloud model catalog in agent_config_changed"
                                    );
                                    return;
                                }
                            }
                        }
                        if params.get("custom_models").is_some()
                            || params.get("custom_model_selection").is_some()
                        {
                            if crate::agent_registry::lookup_profile_by_id(&self.current_agent_id)
                                .byok_mode
                                == crate::agent_registry::ByokMode::Unsupported
                            {
                                self.set_custom_model_config(Vec::new(), None);
                            } else {
                                let models = match params.get("custom_models") {
                                    Some(value) => {
                                        match serde_json::from_value::<Vec<CustomModelCatalogEntry>>(
                                            value.clone(),
                                        ) {
                                            Ok(models) => models,
                                            Err(error) => {
                                                tracing::error!(
                                                    target: "custom_models",
                                                    %error,
                                                    "invalid custom model catalog in agent_config_changed"
                                                );
                                                return;
                                            }
                                        }
                                    }
                                    None => self.custom_model_catalog.clone(),
                                };
                                let selection = params
                                    .get("custom_model_selection")
                                    .and_then(|value| value.as_str())
                                    .map(str::to_string)
                                    .or_else(|| self.custom_model_selection.clone());
                                self.set_custom_model_config(models, selection);
                            }
                        }
                    } else if host_catalog_fields_present {
                        tracing::info!(
                            target: "cloud_models",
                            agent_source = %self.current_agent_source,
                            "ignoring Host cloud/custom catalog hot update for WSL helper"
                        );
                    }
                    if catalog_delivery {
                        self.host_catalog_ready = true;
                    }
                    if catalog_delivery
                        || (accepts_host_catalog && params.get("custom_model_selection").is_some())
                    {
                        self.publish_agent_status();
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
                        // Do not mutate this helper's per-window tab state,
                        // but still notify master. If every helper in the
                        // owning window exits during teardown, a helper in a
                        // surviving window is the only process left that can
                        // deliver the stable tab id needed to close the ACP
                        // session. Master de-duplicates these requests.
                        if let Some(closed_tab_id) = params.get("tab_id").and_then(|v| v.as_str()) {
                            self.request_tab_session_close(closed_tab_id);
                        }
                        tracing::debug!(
                            target: "tab_session",
                            target_window,
                            our_window,
                            "forwarded cross-window tab_closed without mutating local state"
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
                        tab.usage = None;
                        tab.usage_staleness = crate::usage::UsageStaleness::default();
                        tab.clear_completed_turns();
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
                    self.pending_yolo_session_tabs.insert(tab_id.to_string());
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
                    if self
                        .load_session_tx
                        .send(LoadSessionForTab {
                            tab_id: tab_id.to_string(),
                            session_id: session_id.to_string(),
                            cwd,
                        })
                        .is_err()
                    {
                        self.pending_yolo_session_tabs.remove(tab_id);
                        let tab = self.tab_mut(tab_id);
                        tab.loading_session = false;
                        tab.loading_target_session_id = None;
                        tab.messages
                            .push(ChatMessage::Error(t!("connection.lost").into_owned()));
                        tab.scroll_to_bottom();
                    }
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
                // Processed BEFORE the own-pane skip below: this command
                // is routed by the target tab rather than the source pane.
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

                    if let Some(owner) = self.owner_tab_id.as_deref() {
                        if owner != target_tab {
                            tracing::debug!(
                                target: "set_agent_state",
                                owner,
                                tab = %target_tab,
                                "ignoring set_agent_state for non-owner tab"
                            );
                            return;
                        }
                    }

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
                        let tab = self.tab_mut(&target_tab);
                        if !open {
                            tab.invalidate_pending_paste();
                        }
                        tab.pane_open = open;
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
                            let key_before = self.agent_sessions.key_for_pane(&pane_id);
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
                    let is_shell_agent =
                        matches!(origin, Some(crate::agent_sessions::SessionOrigin::Unknown));
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
                if let (Some(event_tab), Some(self_tab)) =
                    (notification.tab_id.as_deref(), self.owner_tab_id.as_deref())
                {
                    if !event_tab.is_empty() && !self_tab.is_empty() && event_tab != self_tab {
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
                        crate::telemetry::log_error_detected(severity_str, &method, &pane_id);
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
                                        self.tab_mut(&t.to_string()).autofix.trigger_echo_pane =
                                            None;
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
                                if let Some(armed) =
                                    self.tab_mut(&target_tab).autofix.armed_at.take()
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
            AppEvent::LoginComplete {
                success,
                error,
                agent_id,
            } => {
                tracing::info!(
                    "LoginComplete received: success={} deferred_acp={}",
                    success,
                    self.deferred_acp.is_some()
                );
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
                        tracing::info!(
                            "LoginComplete: creating deferred_acp for reconnect cmd={}",
                            new_cmd
                        );
                        self.deferred_acp = Some(DeferredAcpParams {
                            agent_cmd: new_cmd,
                            agent_id: Some(agent_id),
                            acp_model: None,
                            custom_model_selection: self.custom_model_selection.clone(),
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
