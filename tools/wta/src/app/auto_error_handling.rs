//! Per-tab Auto error handling bottom-bar state machine.
//!
//! Owns the bar-snapshot data types and the `impl App` methods that drive
//! the Detected -> Analyzing -> Review lifecycle (trigger / emit / execute /
//! clear). Split out of app.rs to keep that file focused; the methods stay
//! on `App` (via `impl App` here) so they share its per-tab state directly.
//!
//! See app/turn_state.rs for the sibling per-tab turn lifecycle.

use super::*;

/// Per-tab Auto error handling state machine. Each tab tracks its own detected,
/// pending, and review state independently so a failure in a background tab
/// doesn't clobber the active tab's state and vice versa.
/// The bottom-bar projection is per-tab too: WTA only emits
/// `auto_error_handling_state` events to C++ when the target tab is currently
/// active, and re-emits the active tab's snapshot on tab_changed.
#[derive(Debug, Clone, Default)]
pub struct AutoErrorHandlingState {
    /// Failing pane for an active analysis or actionable result. Cleared when
    /// the user dismisses the flow, the error resolves, or the result runs.
    pub pane_id: Option<String>,
    /// Monotonic timestamp captured when handling starts, used for
    /// AutoErrorHandlingResolved telemetry elapsed time.
    pub started_at: Option<std::time::Instant>,
    /// Failing pane whose completed result is waiting for review in chat.
    /// Kept separate from `pane_id` so active analysis and pending review can
    /// be reasoned about independently.
    pub result_pane_id: Option<String>,
    /// Bumped on every new trigger / cancel. Snapshotted into
    /// `AutoErrorHandlingContext.generation` at submit time; chunks whose
    /// snapshot diverges are dropped as stale.
    pub generation: u64,
    /// Last bottom-bar state we emitted (or would have emitted, if the
    /// tab wasn't active). Used to re-emit on tab_changed so the bar
    /// shows the right state when the user comes back to this tab.
    pub bar_snapshot: AutoErrorHandlingBarSnapshot,
    /// PaneID where the most recent D-synchronous state set happened
    /// (Detected or Pending — both fire ~1ms before PowerShell emits the
    /// next `osc:133;A`). The next prompt-start in that pane is consumed
    /// as the trigger's echo rather than as a "user moved on" dismiss,
    /// otherwise the state we just set would be cleared before reaching
    /// the user. Cleared when the echo A arrives, or when the state
    /// transitions out (set_bar_snapshot → Idle).
    pub trigger_echo_pane: Option<String>,
}

/// Snapshot of the bottom-bar Auto error handling state for one tab. Mirrors
/// the `state` field of the `auto_error_handling_state` protocol event so we
/// can rebuild the payload from the cached snapshot when the tab becomes active.
#[derive(Debug, Clone, Default)]
pub enum AutoErrorHandlingBarSnapshot {
    #[default]
    Idle,
    /// Detect-only mode: an error was detected but the agent has not been
    /// invoked. The bar shows a hint inviting the user to press the
    /// hotkey / click the pill to request a fix. Carries enough
    /// context to replay the LLM trigger when the user activates it.
    Detected {
        pane_id: String,
        summary: String,
        hotkey_hint: String,
    },
    /// Analysis in progress ("Analyzing…"). Non-interactive.
    Pending { pane_id: String, summary: String },
    /// Analysis finished; a result (a fix or an explanation) is waiting in
    /// the agent pane chat. Surfaced ONLY when the pane is not open — the
    /// bar invites the user to open the pane and review. Once the pane
    /// opens, the snapshot flips to `Idle` (the result is already visible
    /// there, so the bar goes quiet). Replaces the old dual result states:
    /// Auto error handling no longer auto-executes, so a fix and an
    /// explanation surface identically (open pane → review → act manually).
    Review {
        pane_id: String,
        hotkey_hint: String,
    },
}

impl App {
    /// Auto error handling: when a command fails in another pane, ask the coordinator
    /// agent to produce a result. The user confirms before execution.
    pub(super) fn maybe_trigger_auto_error_handling(&mut self, notification: &WtNotification) {
        self.trigger_auto_error_handling_inner(notification, false);
    }

    /// Core error-handling trigger logic. `forced=true` bypasses automatic
    /// sending to the agent (used when the user explicitly activates a
    /// Detected pill via click or hotkey). When `forced=false` and automatic
    /// sending is off, this just emits the Detected
    /// snapshot — the LLM is not invoked.
    pub(super) fn trigger_auto_error_handling_inner(
        &mut self,
        notification: &WtNotification,
        forced: bool,
    ) {
        if self.state != ConnectionState::Connected {
            return;
        }

        // No `is_agent_pane` suppression here. This path is reached only
        // for `vt_sequence` notifications (see the dispatcher in
        // `handle_event`), and `vt_sequence` Actionable events come from
        // shell integration's `osc:133;D;<exit>` markers — the agent CLI
        // doesn't emit OSC 133, so a D arriving implies the shell is the
        // current foreground process and there's no agent teardown to
        // filter. The two genuine "agent exited" paths are handled
        // elsewhere: `osc:133;A` triggers a `PaneClosed` demotion above
        // `classify_wt_event`, and pane-process exit surfaces as
        // `connection_state: closed/failed`, which the dispatcher routes
        // to the banner only — not here. A stale agent binding sitting in
        // the registry (e.g. left by a hook that misreported `pane_id`)
        // must not be allowed to eat a real shell command failure.

        // Resolve the target tab: the tab that owns the failing pane.
        // Without it we can't route Auto error handling to the right ACP session
        // (the prior code fell back to `self.tab_id` and would land the
        // fix in whichever tab WTA happened to be focused on — see
        // comment block at `maybe_trigger_auto_error_handling` head). In release
        // builds we drop the event with a warn instead of panicking,
        // per Step 2 decision #4.
        let target_tab_id = match notification.tab_id.clone() {
            Some(t) => t,
            None => {
                tracing::warn!(
                    target: "auto_error_handling",
                    pane_id = %notification.pane_id,
                    "dropping Auto error handling request: notification missing tab_id (older WT build?)",
                );
                return;
            }
        };

        // Detect-only mode: when automatic sending is off and this isn't a user-
        // forced activation, just surface the Detected pill and let the
        // user decide whether to call the LLM. Skips the busy / generation
        // / submit logic below — none of that machinery applies until the
        // user activates the pill.
        if !self.auto_error_handling_with_agent_enabled && !forced {
            tracing::info!(
                target: "auto_error_handling",
                pane_id = %notification.pane_id,
                tab_id = %target_tab_id,
                "detect-only mode — surfacing Detected pill, no agent call",
            );
            // D-driven: PowerShell will emit an immediate echo A within
            // ~1ms. Arm the gate so it gets consumed rather than
            // dismissing the pill we just set.
            self.tab_mut(&target_tab_id)
                .auto_error_handling
                .trigger_echo_pane = Some(notification.pane_id.clone());
            self.emit_auto_error_handling_state_detected(
                &target_tab_id,
                &notification.pane_id,
                &notification.summary,
            );
            return;
        }

        // Latest event always wins — but only if we can actually act on it.
        // The ACP transport single-flights at the tab level, so if the
        // target tab already has a prompt in flight, submitting another
        // one results in `tab.turn = Submitted(new)` + ACP `AgentBusy`
        // rejection — the buffer and the wire diverge, and old chunks
        // corrupt the new turn's state. Defer instead.
        let (same_pane, already_busy, active_pane_dbg) = {
            let tab = self.tab_mut(&target_tab_id);
            let same =
                tab.auto_error_handling.pane_id.as_deref() == Some(notification.pane_id.as_str());
            let busy = !tab.turn.is_idle()
                && !matches!(
                    tab.turn,
                    TurnState::Surfaced {
                        end_pending: false,
                        ..
                    }
                );
            (same, busy, tab.auto_error_handling.pane_id.clone())
        };

        if already_busy {
            if same_pane {
                // Same pane re-trigger: refresh the bar's summary text but
                // don't re-submit — the agent is already working on it.
                tracing::info!(
                    target: "auto_error_handling",
                    pane_id = %notification.pane_id,
                    tab_id = %target_tab_id,
                    "Auto error handling re-triggered for same pane while pending; re-emitting only",
                );
                // This branch is only reached on a fresh D event (the
                // dispatcher routes vt_sequence here); arm the echo gate.
                self.tab_mut(&target_tab_id)
                    .auto_error_handling
                    .trigger_echo_pane = Some(notification.pane_id.clone());
                self.emit_auto_error_handling_state_pending(
                    &target_tab_id,
                    &notification.pane_id,
                    &notification.summary,
                );
            } else {
                // Different pane while busy: drop. The user can Esc the
                // current Auto error handling turn to free the slot.
                tracing::info!(
                    target: "auto_error_handling",
                    pane_id = %notification.pane_id,
                    tab_id = %target_tab_id,
                    active_pane = ?active_pane_dbg,
                    "skipping Auto error handling request: previous turn still in flight",
                );
            }
            return;
        }

        // For all other cases (different pane, completed result, or Idle):
        // bump the target tab's generation to stale any in-flight response,
        // then submit a new Auto error handling turn via the state machine.
        let new_gen = {
            let tab = self.tab_mut(&target_tab_id);
            tab.auto_error_handling.generation = tab.auto_error_handling.generation.wrapping_add(1);
            // A new analysis supersedes any result awaiting review. The C++ side
            // will swap to Pending on the new pending event below; emitting an
            // explicit cleared first would create a flicker.
            tab.auto_error_handling.result_pane_id = None;
            tab.auto_error_handling.generation
        };

        // Route through the target tab's ACP session. `tab_id` carries the
        // failing tab's StableId so the ACP layer's `tab_to_session` map
        // routes (or lazy-creates) to the right session even when the
        // failing tab isn't currently focused. `source_pane_id` points at
        // the failing pane so the agent can read its buffer.
        let pane_context = PaneContext {
            pane_id: self.pane_id.clone(),
            tab_id: Some(target_tab_id.clone()),
            window_id: self.window_id.clone(),
            cwd: None,
            source_pane_id: Some(notification.pane_id.clone()),
        };

        // Store the failing pane ID on the target tab so the Esc dismiss
        // path can find it (legacy; the new state machine carries it via
        // AutoErrorHandlingContext), and start telemetry timing for resolution.
        {
            let tab = self.tab_mut(&target_tab_id);
            tab.auto_error_handling.pane_id = Some(notification.pane_id.clone());
            tab.auto_error_handling.started_at = Some(std::time::Instant::now());
        }

        let prompt = PromptSubmission::new_auto_error_handling_failure(
            notification.summary.clone(),
            Some(pane_context),
        )
        .with_byok(self.current_model_is_byok())
        .with_agent_id(self.current_agent_id.clone());
        let submitted = SubmittedPrompt {
            id: prompt.id,
            text: prompt.text.clone(),
            submitted_at_unix_s: prompt.submitted_at_unix_s,
            context: TurnContext::with_target_pane(notification.pane_id.clone()),
            auto_error_handling: Some(AutoErrorHandlingContext {
                generation: new_gen,
            }),
        };
        // Install the turn on the target tab — bypasses session_to_tab
        // lookup so a tab with no ACP session yet still gets the prompt
        // queued correctly (the ACP layer creates the session lazily when
        // it processes the prompt).
        self.turn_submit_prompt_for_tab(&target_tab_id, submitted);
        tracing::info!(target: "auto_error_handling", pane_id = %notification.pane_id, tab_id = %target_tab_id, generation = new_gen, "sending Auto error handling prompt");
        let _ = self.prompt_tx.send(prompt);

        // Light up the bottom-bar diagnostic icon in "Pending" state — the
        // user knows something went wrong even before the agent responds.
        // Arm the echo gate ONLY for D-driven entries (forced=false).
        // The `request_analysis` path (forced=true) fires this on a
        // stable prompt — no echo A is in flight, and arming would eat
        // the user's first Enter as a fake echo. Bug repro: typo →
        // Detected pill → click pill → Pending → Review → press Enter
        // (consumed as echo) → press Enter again (finally dismisses).
        if !forced {
            self.tab_mut(&target_tab_id)
                .auto_error_handling
                .trigger_echo_pane = Some(notification.pane_id.clone());
        }
        self.emit_auto_error_handling_state_pending(
            &target_tab_id,
            &notification.pane_id,
            &notification.summary,
        );
    }

    // ── Auto error handling state signalling ───────────────────────────────
    //
    // Notifies TerminalPage about Auto error handling progress via a JSON event on
    // the SendEvent bus. The COM server special-cases method=="auto_error_handling_state"
    // and dispatches to TerminalPage.OnAutoErrorHandlingStateChanged (UI thread).
    //
    // Per-tab projection: the bar shows the active tab's Auto error handling state. Each
    // emit_auto_error_handling_state_* stores the new snapshot on the target tab AND
    // only forwards to WT when the target tab is currently active. On
    // tab_changed, `project_active_tab_state` re-emits the new active
    // tab's snapshot so the bar matches.

    pub(super) fn emit_auto_error_handling_state_pending(
        &mut self,
        target_tab_id: &str,
        pane_id: &str,
        summary: &str,
    ) {
        let snapshot = AutoErrorHandlingBarSnapshot::Pending {
            pane_id: pane_id.to_string(),
            summary: summary.to_string(),
        };
        // NOTE: `trigger_echo_pane` is armed by the *caller*, not here —
        // only D-driven calls expect an immediate echo A. The
        // `request_analysis` path also funnels through Pending but
        // runs on a stable prompt (no D), so arming inside this helper
        // would consume the user's first real Enter as a fake echo.
        self.set_bar_snapshot(target_tab_id, snapshot);
    }

    /// Detect-only entry: error detected but the agent has not been invoked.
    /// The bar shows a clickable hint; the user requests analysis via the
    /// pill or the hotkey, which fires `auto_error_handling_request_analysis`
    /// and replays through `trigger_auto_error_handling_inner` with `force=true`.
    pub(super) fn emit_auto_error_handling_state_detected(
        &mut self,
        target_tab_id: &str,
        pane_id: &str,
        summary: &str,
    ) {
        let snapshot = AutoErrorHandlingBarSnapshot::Detected {
            pane_id: pane_id.to_string(),
            summary: summary.to_string(),
            hotkey_hint: "Ctrl+Alt+.".to_string(),
        };
        // See note in `emit_auto_error_handling_state_pending`: caller arms the echo
        // gate when (and only when) a D-driven trigger is in progress.
        self.set_bar_snapshot(target_tab_id, snapshot);
    }

    /// Surface the terminal "result ready" state after analysis finishes
    /// (a fix or an explanation — both land in the agent pane chat). When
    /// the pane is closed, show a `Review` hint inviting the user to open
    /// it; when it's already open the result is visible there, so the bar
    /// goes quiet (`Idle`). Re-invoked from the `pane_open` handler so the
    /// bar tracks the pane without the C++ side computing anything.
    pub(super) fn update_auto_error_handling_review_state(
        &mut self,
        target_tab_id: &str,
        pane_id: &str,
    ) {
        let pane_open = self
            .tab_sessions
            .get(target_tab_id)
            .map(|t| t.pane_open)
            .unwrap_or(false);
        let snapshot = if pane_open {
            AutoErrorHandlingBarSnapshot::Idle
        } else {
            AutoErrorHandlingBarSnapshot::Review {
                pane_id: pane_id.to_string(),
                hotkey_hint: "Ctrl+Alt+.".to_string(),
            }
        };
        self.set_bar_snapshot(target_tab_id, snapshot);
    }

    /// Handle activation of the Detected pill (click or hotkey). Read the
    /// active tab's cached snapshot, synthesize a `WtNotification` from
    /// it, and replay through `trigger_auto_error_handling_inner` with `forced=true`
    /// so detect-only mode is bypassed and the agent call starts.
    pub(super) fn handle_auto_error_handling_request_analysis(&mut self) {
        let active_tab = self.active_tab_key().to_string();
        let snapshot = self.current_tab().auto_error_handling.bar_snapshot.clone();
        let (pane_id, summary) = match snapshot {
            AutoErrorHandlingBarSnapshot::Detected {
                pane_id, summary, ..
            } => (pane_id, summary),
            other => {
                tracing::info!(
                    target: "auto_error_handling",
                    state = ?other,
                    "auto_error_handling_request_analysis: bar not in Detected state — ignoring",
                );
                return;
            }
        };
        let notification = WtNotification {
            severity: WtEventSeverity::Actionable,
            pane_id,
            tab_id: Some(active_tab),
            summary,
            acknowledged: false,
            age_ticks: 0,
        };
        self.trigger_auto_error_handling_inner(&notification, true);
    }

    /// Execute the actionable result selected from the bottom bar without
    /// requiring the agent pane to be focused.
    pub(super) fn handle_auto_error_handling_execute_result_request(
        &mut self,
        requested_pane_id: &str,
    ) {
        let active_tab = self.active_tab_key().to_string();
        let active_pane = self.current_tab().auto_error_handling.pane_id.clone();
        tracing::info!(target: "auto_error_handling", requested_pane = %requested_pane_id, active_pane = ?active_pane, has_recommendations = self.current_tab().turn.recommendations().is_some(), "Auto error handling execute request received");
        // Only execute if the active tab's pane matches the request.
        // The bar always reflects the active tab, so the click must target
        // it. The pane_id check prevents a stale UI click from running
        // against an unrelated, more recent error.
        let result_pane = match active_pane {
            Some(p) if p == requested_pane_id => p,
            _ => {
                tracing::info!(target: "auto_error_handling", "ignoring execute-result request: no actionable result for this pane");
                // Tell the UI anyway so it returns to Idle.
                self.emit_auto_error_handling_state_cleared(&active_tab);
                return;
            }
        };
        let rec = match self.current_tab().turn.recommendations().cloned() {
            Some(r) => r,
            None => {
                self.emit_auto_error_handling_state_cleared(&active_tab);
                let auto_error_handling = &mut self.current_tab_mut().auto_error_handling;
                auto_error_handling.pane_id = None;
                auto_error_handling.started_at = None;
                return;
            }
        };
        let idx = rec
            .recommended_choice
            .unwrap_or(self.current_tab_mut().selected_recommendation)
            .min(rec.choices.len().saturating_sub(1));
        let Some(mut choice) = rec.choices.get(idx).cloned() else {
            self.emit_auto_error_handling_state_cleared(&active_tab);
            let auto_error_handling = &mut self.current_tab_mut().auto_error_handling;
            auto_error_handling.pane_id = None;
            auto_error_handling.started_at = None;
            return;
        };
        // Auto-fill parent for Send actions, same as Enter path.
        for action in &mut choice.actions {
            if let crate::coordinator::RecommendedAction::Send { ref mut parent, .. } = action {
                if parent.is_empty() {
                    *parent = result_pane.clone();
                }
            }
        }
        // Drive the cutover state machine: if the current tab's turn is
        // still in `Surfaced{Recommendation,..}`, route through
        // `turn_execute_card`; otherwise fall back to the lightweight
        // dispatch path (the user may have already cleared the card via
        // some other input).
        let session_id = self.current_tab().session_id.clone();
        let routed = if let Some(sid) = session_id {
            if matches!(
                self.current_tab().turn,
                TurnState::Surfaced {
                    outcome: TurnOutcome::Recommendation(_),
                    ..
                }
            ) {
                self.turn_execute_card(&sid);
                true
            } else {
                false
            }
        } else {
            false
        };
        let choice_label = choice.choice;
        if !routed {
            let auto_error_handling = &mut self.current_tab_mut().auto_error_handling;
            auto_error_handling.pane_id = None;
            auto_error_handling.started_at = None;
            self.clear_recommendations();
            let _ = self
                .recommendation_tx
                .send(crate::coordinator::ChoiceExecution {
                    choice,
                    insert_only: false,
                    context: TurnContext::with_target_pane(result_pane),
                });
        }
        self.push_execution_info(format!("Auto-executing choice {}.", choice_label));
        self.emit_auto_error_handling_state_cleared(&active_tab);
        // Defensive: covers the fall-back path above where we dispatched the
        // choice directly without going through `turn_execute_card`. The
        // matched-path case already recomputes via that callee.
        self.recompute_chip_override(&active_tab);
    }

    pub(super) fn emit_auto_error_handling_state_cleared(&mut self, target_tab_id: &str) {
        // `cleared` carries no pane info — C++ clears its
        // `lastErrorSessionId` based on the state alone. Reusing the
        // `Idle` snapshot means a subsequent tab switch re-emits a
        // clean state rather than something stale.
        // Also drop any pending trigger-echo gate: once we're back to
        // Idle there's no state to protect, and leaving the pane
        // protected would silently swallow the next real prompt-start.
        self.tab_mut(target_tab_id)
            .auto_error_handling
            .trigger_echo_pane = None;
        // Clearing the bar also ends any "pending review" result, so the
        // pane_open handler won't resurrect a Review hint after a dismiss /
        // exit-0 / Esc. (Completion sets `result_pane_id` and surfaces
        // via `update_auto_error_handling_review_state`, never through here.)
        self.tab_mut(target_tab_id)
            .auto_error_handling
            .result_pane_id = None;
        self.set_bar_snapshot(target_tab_id, AutoErrorHandlingBarSnapshot::Idle);
    }

    /// Store a fresh bar snapshot on the target tab and, if that tab is
    /// currently active, forward it to WT so the bottom bar updates.
    pub(super) fn set_bar_snapshot(
        &mut self,
        target_tab_id: &str,
        snapshot: AutoErrorHandlingBarSnapshot,
    ) {
        self.tab_mut(target_tab_id).auto_error_handling.bar_snapshot = snapshot.clone();
        if target_tab_id == self.active_tab_key() {
            send_bar_event(&snapshot, Some(target_tab_id));
        }
    }
}

/// Build and send an `auto_error_handling_state` protocol event from a cached bar
/// snapshot. Used by both fresh state transitions (active tab) and the
/// tab_changed re-emit path. Field shape mirrors what C++
/// `OnAutoErrorHandlingStateChanged` consumes.
pub(super) fn send_bar_event(snapshot: &AutoErrorHandlingBarSnapshot, tab_id: Option<&str>) {
    let mut evt = match snapshot {
        AutoErrorHandlingBarSnapshot::Idle => serde_json::json!({
            "type": "event",
            "method": "auto_error_handling_state",
            "params": { "state": "cleared" }
        }),
        AutoErrorHandlingBarSnapshot::Detected {
            pane_id,
            summary,
            hotkey_hint,
        } => serde_json::json!({
            "type": "event",
            "method": "auto_error_handling_state",
            "params": {
                "state": "detected",
                "pane_id": pane_id,
                "summary": summary,
                "hotkey_hint": hotkey_hint,
            }
        }),
        AutoErrorHandlingBarSnapshot::Pending { pane_id, summary } => serde_json::json!({
            "type": "event",
            "method": "auto_error_handling_state",
            "params": {
                "state": "pending",
                "pane_id": pane_id,
                "summary": summary,
            }
        }),
        AutoErrorHandlingBarSnapshot::Review {
            pane_id,
            hotkey_hint,
        } => serde_json::json!({
            "type": "event",
            "method": "auto_error_handling_state",
            "params": {
                "state": "review",
                "pane_id": pane_id,
                "hotkey_hint": hotkey_hint,
            }
        }),
    };
    // Tag with tab_id so C++ routes the bottom-bar update to the right
    // tab's AgentPaneContent (the window-level bar reflects the active tab's
    // Auto error handling state). Without this, a non-active tab could
    // clobber the bar.
    if let Some(t) = tab_id {
        if let Some(params) = evt.get_mut("params").and_then(|v| v.as_object_mut()) {
            params.insert(
                "tab_id".to_string(),
                serde_json::Value::String(t.to_string()),
            );
        }
    }
    send_wt_protocol_event(evt.to_string());
}
