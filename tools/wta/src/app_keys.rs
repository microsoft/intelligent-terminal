//! `App::handle_key` — the top-level keyboard-event dispatch, split out of
//! the large `app.rs` file into its own child module of `app` (via
//! `#[path]`) so it can reach `App`'s private fields and helper methods just
//! like the rest of `app.rs` does. `handle_key` is `pub(super)` because
//! `App::handle_event` (in the sibling `app_events.rs` module) and the test
//! helpers in `app_tests.rs` call it directly on an `App`.

use super::*;

impl App {
    pub(super) fn handle_key(&mut self, key: KeyEvent) {
        // Per-keystroke and carries the raw `KeyCode` (the typed character for
        // `Char` keys) — the user's prompt can be reconstructed from this
        // stream. Trace only so it never persists in shipping (info) or
        // default-debug logs.
        tracing::trace!(
            target: "input",
            code = ?key.code,
            modifiers = ?key.modifiers,
            input_empty = self.current_tab().input.is_empty(),
            recs = self.current_tab().turn.recommendations().is_some(),
            turns = self.current_tab().completed_turns.len(),
            selected_turn = ?self.current_tab().selected_completed_turn_idx,
            "key received"
        );

        // Any non-Ctrl+C key disarms the close-pane sequence. We allow plain
        // Ctrl presses (modifier-only events) through so the user can still
        // hold Ctrl while preparing to tap C the second time. The Ctrl+C
        // arm-or-fire transitions itself are handled in the match below.
        let is_ctrl_c =
            matches!(key.code, KeyCode::Char('c')) && key.modifiers.contains(KeyModifiers::CONTROL);
        if !is_ctrl_c {
            self.close_pane_armed_at = None;
            // Don't clear `transient_hint` here — it has its own deadline and
            // ui::render checks expiry on each draw. Clearing on every key
            // would steal too much of the hint's visible lifetime.
        }

        // The source picker is also reachable from Setup when the configured
        // Windows agent is missing, so it must receive keys before the
        // mode-specific setup handler.
        if self.agent_picker_visible() {
            match key.code {
                KeyCode::Up => self.agent_picker_up(),
                KeyCode::Down => self.agent_picker_down(),
                KeyCode::Enter => self.commit_agent_pick(),
                KeyCode::Esc => self.close_agent_picker(),
                _ => {}
            }
            return;
        }

        // Setup mode: diagnostic install/sign-in/retry flow.
        if self.mode == AppMode::Setup {
            self.handle_setup_key(key);
            return;
        }

        // Auth mode: Enter to sign in, Esc to go back
        if self.mode == AppMode::Auth {
            match key.code {
                // GitHub Enterprise sign-in (Copilot): [E] reveals a domain
                // input; while it's open, typed chars edit the domain and
                // Backspace deletes. (Esc collapses it — handled below.)
                KeyCode::Char('e') | KeyCode::Char('E')
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT)
                        && self
                            .auth
                            .as_ref()
                            .map(|a| !a.checking && a.agent_id == "copilot" && !a.enterprise_mode)
                            .unwrap_or(false) =>
                {
                    if let Some(ref mut auth) = self.auth {
                        auth.enterprise_mode = true;
                        // Starting a fresh enterprise attempt: drop any prior
                        // failure/progress text so it doesn't show in the domain
                        // input (e.g. a leftover github.com "Login failed").
                        auth.status_message.clear();
                    }
                }
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT)
                        && self
                            .auth
                            .as_ref()
                            .map(|a| !a.checking && a.enterprise_mode)
                            .unwrap_or(false) =>
                {
                    if !c.is_whitespace() {
                        if let Some(ref mut auth) = self.auth {
                            auth.enterprise_host.push(c);
                        }
                    }
                }
                KeyCode::Backspace
                    if self
                        .auth
                        .as_ref()
                        .map(|a| !a.checking && a.enterprise_mode)
                        .unwrap_or(false) =>
                {
                    if let Some(ref mut auth) = self.auth {
                        auth.enterprise_host.pop();
                    }
                }
                KeyCode::Enter => {
                    // Extract values before borrowing self again
                    let login_info = self.auth.as_ref().and_then(|a| {
                        if !a.checking && !a.login_command.is_empty() {
                            // In enterprise mode, a non-empty domain drives a
                            // `--host` sign-in; otherwise the default github.com.
                            let host = if a.enterprise_mode {
                                let h = a.enterprise_host.trim();
                                if h.is_empty() {
                                    None
                                } else {
                                    Some(h.to_string())
                                }
                            } else {
                                None
                            };
                            Some((a.agent_id.clone(), a.login_command.clone(), host))
                        } else {
                            None
                        }
                    });
                    if let Some((agent_id, _login_cmd, host)) = login_info {
                        if agent_id != "copilot" {
                            tracing::warn!(
                                target: "login",
                                agent = %agent_id,
                                "ignoring auth Enter for non-Copilot agent"
                            );
                            return;
                        }
                        // Copilot: auto device-flow sign-in via piped stdio.
                        // Rebuild the command with the (optional) GitHub
                        // Enterprise host and remember it for next time.
                        let login_cmd =
                            crate::agent_check::build_login_cmd(&agent_id, host.as_deref());
                        // Remember the last-used host for next time. Persist
                        // the *normalized* bare domain (or "" for github.com /
                        // empty) so a returning user is prefilled only for a
                        // real GHE domain — not stuck in the expanded
                        // enterprise input after a github.com fallback.
                        let normalized_host = host
                            .as_deref()
                            .and_then(crate::agent_check::normalize_enterprise_host);
                        crate::agent_check::save_copilot_enterprise_host(
                            normalized_host.as_deref().unwrap_or(""),
                        );
                        self.begin_auth_checking();
                        tracing::info!(
                            target: "login",
                            agent = %agent_id,
                            enterprise = host.is_some(),
                            host = host.as_deref().unwrap_or("github.com"),
                            cmd = %login_cmd,
                            "starting copilot device-flow login"
                        );
                        self.spawn_login(&agent_id, &login_cmd);
                    }
                }
                KeyCode::Esc => {
                    // In the GHE domain input, Esc collapses back to the
                    // github.com sign-in choice rather than leaving the screen.
                    if self
                        .auth
                        .as_ref()
                        .map(|a| a.enterprise_mode && !a.checking)
                        .unwrap_or(false)
                    {
                        if let Some(ref mut auth) = self.auth {
                            auth.enterprise_mode = false;
                            // Collapsing back to the github.com choice abandons
                            // the enterprise attempt — clear its failure/progress
                            // text so it doesn't linger on the collapsed screen.
                            auth.status_message.clear();
                        }
                        return;
                    }
                    if self.setup.is_some() {
                        // Go back to setup screen
                        self.mode = AppMode::Setup;
                    } else {
                        // No setup state to go back to (e.g. preflight auth failure) —
                        // rebuild setup as AgentMissing for this agent
                        let agent_id = self
                            .auth
                            .as_ref()
                            .map(|a| a.agent_id.clone())
                            .unwrap_or_default();
                        if !agent_id.is_empty() {
                            let agent_status = crate::agent_check::check_agent(&agent_id);
                            let profile = crate::agent_registry::lookup_profile_by_id(&agent_id);
                            let reason = SetupReason::AgentError;
                            let options = build_setup_options(&reason, Some(&agent_status));
                            self.mode = AppMode::Setup;
                            self.setup = Some(SetupState {
                                reason,

                                selected_index: 0,
                                preflight: PreflightResult {
                                    agent_id: agent_id.clone(),
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
                        } else {
                            self.mode = AppMode::Chat;
                        }
                    }
                    self.auth = None;
                }
                _ => {}
            }
            return;
        }

        // Agent session view: list navigation, Enter activation, search,
        // refresh, and Esc handling. Captures all input while open. View
        // open-state and the selection cursor are per-tab on `TabSession`
        // so each WT tab keeps its own picker state across switches.
        if self.current_tab().current_view == View::Agents {
            let tab_id = self.active_tab_key().to_string();

            if self.current_tab().agents_view.search_focused {
                match &key.code {
                    KeyCode::Esc => {
                        let tab = self.current_tab_mut();
                        tab.agents_view.search_query.clear();
                        tab.agents_view.search_focused = false;
                        self.reset_agents_search_selection(&tab_id);
                        return;
                    }
                    KeyCode::Backspace => {
                        self.current_tab_mut().agents_view.search_query.pop();
                        self.reset_agents_search_selection(&tab_id);
                        return;
                    }
                    KeyCode::Char(character)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        self.current_tab_mut().agents_view.search_query.push(*character);
                        self.reset_agents_search_selection(&tab_id);
                        return;
                    }
                    KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::Enter
                    | KeyCode::F(5) => {}
                    _ => return,
                }
            }

            let rows = self.agents_rows_for_tab(&tab_id);
            let count = rows.len();
            match key.code {
                KeyCode::Down => {
                    let cur = self.current_tab().agents_list_state.selected().unwrap_or(0);
                    let next = if count == 0 {
                        0
                    } else {
                        (cur + 1).min(count - 1)
                    };
                    self.current_tab_mut().agents_list_state.select(Some(next));
                    self.update_agents_focus_for_tab(&tab_id);
                }
                KeyCode::Up => {
                    let cur = self.current_tab().agents_list_state.selected().unwrap_or(0);
                    self.current_tab_mut()
                        .agents_list_state
                        .select(Some(cur.saturating_sub(1)));
                    self.update_agents_focus_for_tab(&tab_id);
                }
                KeyCode::Enter => {
                    if let Some(idx) = self.current_tab().agents_list_state.selected() {
                        let selected = rows.get(idx).cloned();
                        if let Some(s) = selected {
                            // B-10: route through the unified
                            // state-machine dispatcher. Shift flips
                            // the default per-origin (see
                            // session_mgmt::decide_enter_action) —
                            // Live rows ignore Shift; dead rows use
                            // it as an escape hatch to the *other*
                            // resume style.
                            let shift = key.modifiers.contains(KeyModifiers::SHIFT);
                            self.activate_agent_session_with_shift(&s, shift);
                        }
                    }
                }
                KeyCode::Esc if !self.current_tab().agents_view.search_query.is_empty() => {
                    self.current_tab_mut().agents_view.search_query.clear();
                    self.reset_agents_search_selection(&tab_id);
                }
                KeyCode::Esc => {
                    let tab_id = self.active_tab_key().to_string();
                    // Restore the pane visibility the user had *before* they
                    // entered session management. Read before any mutation.
                    // Falls back to "stay open" (the legacy Esc behaviour) if
                    // nothing was captured.
                    let restore_open = self
                        .current_tab()
                        .agents_view_prev_pane_open
                        .unwrap_or(true);
                    if restore_open {
                        // Entered from an expanded chat pane → return to it:
                        // switch the TUI back to chat, leave the pane visible.
                        self.close_agents_view_for_tab(&tab_id);
                        self.tab_mut(&tab_id).pane_open = true;
                    } else {
                        // Entered from a folded (stashed) pane → re-fold.
                        // Deliberately do NOT switch to chat here: if we did,
                        // the helper would re-render the chat view for a frame
                        // while the pane is still on screen, so the user sees
                        // the agent pane flash before C++ stashes it. Keeping
                        // the session list rendered lets the pane stash
                        // straight from it. Clear the snapshot so a later
                        // re-entry re-captures; the lingering Agents view
                        // self-heals to chat on the next chat-toggle open.
                        let tab = self.tab_mut(&tab_id);
                        tab.pane_open = false;
                        tab.agents_view.search_query.clear();
                        tab.agents_view.search_focused = false;
                        tab.agents_view_prev_pane_open = None;
                    }
                    self.project_active_tab_state();
                }
                KeyCode::Char('/')
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.current_tab_mut().agents_view.search_focused = true;
                }
                KeyCode::F(5) => {
                    // Refresh: ask master to re-scan the on-disk historical
                    // session logs (load_for_cli) like the startup seed, then
                    // re-list. The sticky pending_rescan flag is consumed when
                    // schedule actually dispatches, so it survives in-flight
                    // coalescing.
                    let tab_id = self.active_tab_key().to_string();
                    self.tab_mut(&tab_id).agents_view.pending_rescan = true;
                    self.schedule_agents_refetch_for_tab(&tab_id);
                }
                _ => {}
            }
            return;
        }

        // If permission card is showing, route keys there. Buttons are
        // rendered horizontally inside the embedded card (same chrome as
        // recommendations), so Left/Right move the focus; Up/Down kept as
        // aliases for muscle memory from the prior modal.
        if let Some(perm) = self.current_tab_mut().permission.front_mut() {
            match key.code {
                KeyCode::Left | KeyCode::Up => {
                    if perm.selected > 0 {
                        perm.selected -= 1;
                    }
                }
                KeyCode::Right | KeyCode::Down => {
                    if perm.selected < perm.options.len().saturating_sub(1) {
                        perm.selected += 1;
                    }
                }
                KeyCode::Enter => {
                    let option_id = perm.options[perm.selected].id.clone();
                    // Pop the resolved entry; the next queued request (if
                    // any) automatically becomes the new front and is
                    // rendered on the next frame.
                    if let Some(perm) = self.current_tab_mut().permission.pop_front() {
                        if let Some(responder) = perm.responder {
                            let _ = responder.send(option_id);
                        } else {
                            let _ = self.permission_tx.send(option_id);
                        }
                    }
                }
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    // Quick allow: find first allow option
                    if let Some(idx) = perm.allow_index() {
                        let option_id = perm.options[idx].id.clone();
                        if let Some(perm) = self.current_tab_mut().permission.pop_front() {
                            if let Some(responder) = perm.responder {
                                let _ = responder.send(option_id);
                            } else {
                                let _ = self.permission_tx.send(option_id);
                            }
                        }
                    }
                }
                KeyCode::Char('n') | KeyCode::Char('N') => {
                    // Quick deny: find first reject option
                    if let Some(idx) = perm.reject_index() {
                        let option_id = perm.options[idx].id.clone();
                        if let Some(perm) = self.current_tab_mut().permission.pop_front() {
                            if let Some(responder) = perm.responder {
                                let _ = responder.send(option_id);
                            } else {
                                let _ = self.permission_tx.send(option_id);
                            }
                        }
                    }
                }
                _ => {}
            }
            return;
        }

        // Model picker modal (`/model`): while it's up, arrows move the
        // highlight, Enter commits the pick, Esc dismisses. Swallow every
        // other key so nothing leaks into the input box behind the modal.
        if self.model_picker_visible() {
            match key.code {
                KeyCode::Up => self.model_picker_up(),
                KeyCode::Down => self.model_picker_down(),
                KeyCode::Enter => self.commit_model_pick(),
                KeyCode::Esc => self.close_model_picker(),
                _ => {}
            }
            return;
        }

        if self.current_tab().paste_pending {
            tracing::debug!(target: "agent_paste", "ignoring key while paste is pending");
            return;
        }

        match key.code {
            KeyCode::Up if self.current_tab().turn.recommendations().is_some() =>
            {
                if self.current_tab_mut().selected_recommendation > 0 {
                    self.current_tab_mut().selected_recommendation -= 1;
                    self.current_tab_mut().selected_button = self.default_button_for_selected();
                    self.scroll_rec_to_selected(self.main_area_width());
                    // Selection moved — the new card may target a different
                    // pane (or have no Send action), so re-pin the chip.
                    let tab_id = self.active_tab_key().to_string();
                    self.recompute_chip_override(&tab_id);
                }
            }
            KeyCode::Down if self.current_tab().turn.recommendations().is_some() =>
            {
                let choices_len = self
                    .current_tab()
                    .turn
                    .recommendations()
                    .map(|r| r.choices.len())
                    .unwrap_or(0);
                if self.current_tab().selected_recommendation + 1 < choices_len {
                    let default_btn = self.default_button_for_selected();
                    self.current_tab_mut().selected_recommendation += 1;
                    self.current_tab_mut().selected_button = default_btn;
                    self.scroll_rec_to_selected(self.main_area_width());
                    let tab_id = self.active_tab_key().to_string();
                    self.recompute_chip_override(&tab_id);
                }
            }
            KeyCode::Right | KeyCode::Tab
                if self.current_tab().turn.recommendations().is_some() =>
            {
                // Cycle button focus forward within the selected card.
                // Send: 0=Run, 1=Insert. OpenAndSend has only index 0.
                let button_count = self.button_count_for_selected();
                if button_count > 1 {
                    self.current_tab_mut().selected_button =
                        (self.current_tab_mut().selected_button + 1) % button_count;
                }
            }
            KeyCode::Tab
                if self.current_tab().input.is_empty()
                    && self.current_tab().turn.recommendations().is_none()
                    && !self.current_tab().completed_turns.is_empty() =>
            {
                self.current_tab_mut().select_older_completed_turn();
            }
            KeyCode::BackTab
                if self.current_tab().input.is_empty()
                    && self.current_tab().turn.recommendations().is_none()
                    && !self.current_tab().completed_turns.is_empty() =>
            {
                self.current_tab_mut().select_newer_completed_turn();
            }
            KeyCode::Esc if self.current_tab().selected_completed_turn_idx.is_some() => {
                // Esc clears the past-turn selection without any other side
                // effect. Lets the user back out of the history nav cleanly.
                self.current_tab_mut().selected_completed_turn_idx = None;
            }
            KeyCode::Up if self.current_tab().selected_completed_turn_idx.is_some() => {
                self.current_tab_mut().select_older_completed_turn();
            }
            KeyCode::Down if self.current_tab().selected_completed_turn_idx.is_some() => {
                self.current_tab_mut().select_newer_completed_turn();
            }
            KeyCode::Left | KeyCode::BackTab
                if self.current_tab().turn.recommendations().is_some() =>
            {
                // Cycle button focus backward.
                let button_count = self.button_count_for_selected();
                if button_count > 1 {
                    self.current_tab_mut().selected_button =
                        (self.current_tab_mut().selected_button + button_count - 1) % button_count;
                }
            }
            KeyCode::F(12) => {
                self.show_debug_panel = !self.show_debug_panel;
                self.debug_capture_enabled
                    .store(self.show_debug_panel, Ordering::Relaxed);
                self.debug_scroll = 0;
                return;
            }
            KeyCode::PageUp
                if key.modifiers.contains(KeyModifiers::SHIFT) && self.show_debug_panel =>
            {
                self.debug_scroll = self.debug_scroll.saturating_add(10);
                return;
            }
            KeyCode::PageDown
                if key.modifiers.contains(KeyModifiers::SHIFT) && self.show_debug_panel =>
            {
                self.debug_scroll = self.debug_scroll.saturating_sub(10);
                return;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // In-flight: state is Submitted/Streaming or Surfaced{end_pending}.
                let in_flight = !self.current_tab().turn.is_idle()
                    && !matches!(
                        self.current_tab().turn,
                        TurnState::Surfaced {
                            end_pending: false,
                            ..
                        }
                    );
                if in_flight {
                    // Send a session/cancel to the ACP client. The client
                    // will fire the protocol notification and signal the
                    // per-prompt oneshot so the spawned task drops out of
                    // conn.prompt() immediately.
                    let session_id = self.current_tab().session_id.clone();
                    if let Some(sid) = session_id.clone() {
                        let _ = self.cancel_tx.send(CancelRequest { session_id: sid });
                    }
                    if let Some(sid) = session_id {
                        self.turn_cancel(&sid);
                    }
                    let tab = self.current_tab_mut();
                    tab.messages
                        .push(ChatMessage::System(t!("system.cancelled").into_owned()));
                    tab.scroll_to_bottom();
                    self.close_pane_armed_at = None;
                } else if !self.current_tab().input.is_empty() {
                    // Mirror bash readline: Ctrl+C clears the buffer.
                    self.current_tab_mut().clear_input();
                    self.close_pane_armed_at = None;
                } else {
                    // Idle + empty input. First press arms; second press
                    // within CLOSE_PANE_ARM_WINDOW asks WT to close the
                    // pane. We never set should_quit ourselves — the pane
                    // teardown will kill our ConPty, which is the only
                    // path that should terminate wta.
                    let now = std::time::Instant::now();
                    let armed = self
                        .close_pane_armed_at
                        .map(|t| now.duration_since(t) < CLOSE_PANE_ARM_WINDOW)
                        .unwrap_or(false);
                    if armed {
                        self.close_pane_armed_at = None;
                        self.transient_hint = None;
                        self.request_close_agent_pane();
                    } else {
                        self.close_pane_armed_at = Some(now);
                        self.transient_hint = Some((
                            "Press Ctrl+C again to close pane".to_string(),
                            now + CLOSE_PANE_ARM_WINDOW,
                        ));
                    }
                }
            }
            KeyCode::Esc if self.help_overlay_visible => {
                self.help_overlay_visible = false;
            }
            KeyCode::Esc if self.show_notification_banner => {
                self.dismiss_notifications();
            }
            KeyCode::Esc
                if self.current_tab().turn.recommendations().is_some()
                    || (self.current_tab().autofix.pane_id.is_some()
                        && !self.current_tab().turn.is_idle()) =>
            {
                // Dismiss armed fix card or cancel in-flight autofix request.
                // `turn_cancel` bumps generation, emits autofix_state_cleared,
                // and resets the state machine to Idle.
                let session_id = self.current_tab().session_id.clone();
                if let Some(sid) = session_id {
                    self.turn_cancel(&sid);
                } else {
                    // No session attached yet — fall back to manual cleanup
                    // (no chunks can be in flight in that case).
                    let pane_to_clear = {
                        let tab = self.current_tab_mut();
                        tab.autofix.generation = tab.autofix.generation.wrapping_add(1);
                        tab.autofix.armed_at = None;
                        tab.autofix.pane_id.take()
                    };
                    if pane_to_clear.is_some() {
                        let active = self.active_tab_key().to_string();
                        self.emit_autofix_state_cleared(&active);
                    }
                }
            }
            // Dismiss the bottom-bar Suggested indicator (autofix produced an
            // explanation, not an executable fix). Reachable only when the user
            // is interacting with this TUI — i.e. the agent pane is currently
            // visible. Other dismiss paths: clicking the bar (opens pane), or
            // any prompt activity in any pane (exit-zero or osc:133;A).
            //
            // NOTE: this only handles the default-tui (single-process) mode.
            // In shared-host attach mode `suggested_pane_id` lives on the host;
            // the attach client would need to send a HostCommand::DismissSuggestion.
            // TODO: wire that path when shared-host mode is exercised.
            KeyCode::Esc if self.current_tab().autofix.suggested_pane_id.is_some() => {
                self.current_tab_mut().autofix.suggested_pane_id = None;
                let active = self.active_tab_key().to_string();
                self.emit_autofix_state_cleared(&active);
            }
            KeyCode::Esc => {
                self.current_tab_mut().clear_input();
            }
            KeyCode::Up if self.command_popup_visible() => {
                self.command_popup_up();
            }
            KeyCode::Down if self.command_popup_visible() => {
                self.command_popup_down();
            }
            KeyCode::Tab if self.command_popup_visible() => {
                self.accept_command_popup_completion();
            }
            KeyCode::Up
                if self.current_tab().input_has_nav_focus()
                    && self.current_tab().input_history_is_browsing() =>
            {
                self.current_tab_mut().navigate_input_history_older();
            }
            KeyCode::Down
                if self.current_tab().input_has_nav_focus()
                    && self.current_tab().input_history_is_browsing() =>
            {
                self.current_tab_mut().navigate_input_history_newer();
            }
            KeyCode::Up
                if self.current_tab().input_has_nav_focus()
                    && self.current_tab().has_input_history() =>
            {
                self.current_tab_mut().navigate_input_history_older();
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                // Input editing only acts when the input is the live caret
                // target. While a recommendation/permission card or a past
                // turn is highlighted the input is locked (see Char below).
                if self.current_tab().input_has_nav_focus() {
                    self.current_tab_mut().insert_input_char('\n');
                }
            }
            KeyCode::Enter
                if self.current_tab().input.is_empty()
                    && self.current_tab().selected_completed_turn_idx.is_some()
                    && self.current_tab().turn.recommendations().is_none() =>
            {
                // A past turn is highlighted via Tab — Enter toggles its
                // expanded state instead of submitting / activating recs.
                self.current_tab_mut().toggle_selected_completed_turn();
            }
            KeyCode::Enter => {
                if self.current_tab().turn.recommendations().is_some() {
                    // Card is visible — it owns focus even when the input box
                    // already has draft text. Keep the draft intact and route
                    // Enter to the selected card action instead of submitting
                    // or slash-parsing the input.
                    if self.state == ConnectionState::Connected {
                        let session_id = self.current_tab().session_id.clone();
                        if let Some(session_id) = session_id {
                            let label_choice = self
                                .selected_recommendation_choice()
                                .map(|c| c.choice)
                                .unwrap_or(0);
                            let insert_only = self.current_tab().selected_button == 1
                                && self
                                    .selected_recommendation_choice()
                                    .map(|c| self.is_send_choice(c))
                                    .unwrap_or(false);
                            tracing::info!(
                                target: "autofix",
                                choice = label_choice,
                                insert_only,
                                "Executing choice",
                            );
                            let label = if insert_only {
                                "Inserting"
                            } else {
                                "Executing"
                            };
                            self.push_execution_info(format!("{} choice {}.", label, label_choice));
                            self.turn_execute_card(&session_id);
                        }
                    }
                    return;
                }

                // Slash-command intercept (popup selection, known command, or
                // unknown-command warning). Runs before the prompt path so
                // commands like /stop work even mid-flight, and /help / /clear
                // work even when the agent isn't Connected. Returns true when
                // the keystroke was consumed; an unknown command only warns and
                // falls through so the raw line still goes to the agent.
                if self.try_handle_slash_on_enter() {
                    return;
                }
                let _tab = self.current_tab();
                tracing::debug!(target: "autofix", input_empty = _tab.input.is_empty(), state = ?self.state, has_recs = _tab.turn.recommendations().is_some(), autofix_pane = ?_tab.autofix.pane_id, selected_idx = _tab.selected_recommendation, "Enter");
                if (!self.current_tab().input.is_empty()
                    || !self.current_tab().pending_images.is_empty())
                    && self.state == ConnectionState::Connected
                {
                    // Same-tab single-flight: refuse a new prompt if the
                    // turn isn't accepting one. The ACP transport rejects
                    // too, but bouncing here keeps the user's input intact.
                    if !self.current_tab().turn.accepts_new_prompt() {
                        let tab = self.current_tab_mut();
                        tab.messages
                            .push(ChatMessage::System(t!("system.agent_busy").into_owned()));
                        tab.scroll_to_bottom();
                        return;
                    }
                    let tab = self.current_tab_mut();
                    let text = std::mem::take(&mut tab.input);
                    tab.record_input_history(&text);
                    // Drain any Alt+V images queued for this prompt.
                    let images = std::mem::take(&mut tab.pending_images);
                    tab.cursor_pos = 0;
                    tab.refresh_command_popup();
                    // `session_id` may be None on a brand-new tab whose ACP
                    // session is created lazily by `dispatch_prompt_body`.
                    // Fall back to a key that `session_tab_mut`'s
                    // `tab_for_session` resolves to the active tab — same
                    // trick as `maybe_trigger_autofix` — so the state
                    // machine still installs the turn on this tab. When
                    // `SessionAttached` later writes the real session id,
                    // subsequent chunks route here correctly.
                    let session_id = tab
                        .session_id
                        .clone()
                        .unwrap_or_else(|| DEFAULT_TAB_ID.to_string());
                    let pane_context = PaneContext {
                        pane_id: self.pane_id.clone(),
                        tab_id: self.tab_id.clone(),
                        window_id: self.window_id.clone(),
                        cwd: self.source_cwd.clone(),
                        source_pane_id: None,
                    };
                    // The echoed user message shows a marker for each queued
                    // image; the ACP text block stays raw (the image rides as a
                    // separate ContentBlock::Image).
                    let display_text = if images.is_empty() {
                        text.clone()
                    } else {
                        let items = images
                            .iter()
                            .enumerate()
                            .map(|(i, im)| format!("[{}] {}", i + 1, im.label))
                            .collect::<Vec<_>>()
                            .join(", ");
                        let marker = t!("input.image_attachments", items = items).into_owned();
                        if text.is_empty() {
                            marker
                        } else {
                            format!("{text}\n{marker}")
                        }
                    };
                    let prompt =
                        PromptSubmission::new(text.clone(), Some(pane_context)).with_images(images);
                    prompt_timing_log(
                        prompt.id,
                        prompt.submitted_at_unix_s,
                        "ui_submit",
                        &format!("preview={:?}", prompt.preview()),
                    );
                    if self.show_welcome_hint {
                        self.show_welcome_hint = false;
                        set_welcome_shown_in_state();
                    }
                    let submitted = SubmittedPrompt {
                        id: prompt.id,
                        text: display_text,
                        submitted_at_unix_s: prompt.submitted_at_unix_s,
                        autofix: None,
                    };
                    self.turn_submit_prompt(&session_id, submitted);
                    let _ = self.prompt_tx.send(prompt);
                }
            }
            KeyCode::Backspace if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.current_tab().input_has_nav_focus() {
                    self.current_tab_mut().delete_word_before_cursor();
                }
            }
            KeyCode::Backspace => {
                if self.current_tab().input_has_nav_focus() {
                    self.current_tab_mut().delete_before_cursor();
                }
            }
            KeyCode::Delete => {
                if self.current_tab().input_has_nav_focus() {
                    self.current_tab_mut().delete_at_cursor();
                }
            }
            KeyCode::Left if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.current_tab_mut().move_cursor_word_left();
            }
            KeyCode::Right if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.current_tab_mut().move_cursor_word_right();
            }
            KeyCode::Left => {
                self.current_tab_mut().move_cursor_left();
            }
            KeyCode::Right => {
                self.current_tab_mut().move_cursor_right();
            }
            KeyCode::Home => {
                self.current_tab_mut().move_cursor_home();
            }
            KeyCode::End => {
                self.current_tab_mut().move_cursor_end();
            }
            KeyCode::PageUp => {
                self.current_tab_mut().chat_scroll.by(10);
            }
            KeyCode::PageDown => {
                self.current_tab_mut().chat_scroll.by(-10);
            }
            KeyCode::Char('v') | KeyCode::Char('V')
                if key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.handle_paste_image();
            }
            KeyCode::Char(c) => {
                // Only type into the input when it is the live caret target.
                // When a recommendation/permission card or a past turn is
                // highlighted the input is locked: keystrokes are ignored so
                // the buffer can't fill invisibly (no caret) and strand the
                // user (a non-empty buffer disables Tab/↑ history nav). Press
                // Esc, or Tab/Shift+Tab back past the ends, to return focus.
                if self.current_tab().input_has_nav_focus() {
                    self.current_tab_mut().insert_input_char(c);
                }
            }
            _ => {}
        }
    }
}
