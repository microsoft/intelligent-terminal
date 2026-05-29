//! Per-tab autofix bottom-bar state machine.
//!
//! Owns the bar-snapshot data types and the `impl App` methods that drive
//! the Detected -> Analyzing -> Review lifecycle (trigger / emit / execute /
//! clear). Split out of app.rs to keep that file focused; the methods stay
//! on `App` (via `impl App` here) so they share its per-tab state directly.
//!
//! See app/turn_state.rs for the sibling per-tab turn lifecycle.

use super::*;

/// Per-tab autofix state machine. Each tab tracks its own pending /
/// armed / suggested autofix independently so a failure in a background
/// tab doesn't clobber an armed fix in the active tab and vice versa.
/// The bottom-bar projection is per-tab too: WTA only emits
/// `autofix_state` events to C++ when the target tab is currently
/// active, and re-emits the active tab's snapshot on tab_changed.
#[derive(Debug, Clone, Default)]
pub struct TabAutofixState {
    /// Failing pane for Pending/Armed. Cleared when the user dismisses
    /// (Esc), the error resolves (exit 0 on the same pane), or the fix
    /// is executed.
    pub pane_id: Option<String>,
    /// Monotonic timestamp captured when `pane_id` is armed, used for
    /// ErrorFixResolved telemetry elapsed time.
    pub armed_at: Option<std::time::Instant>,
    /// Failing pane for the Suggested terminal state (a non-actionable
    /// explanation in chat — distinct from `pane_id` so the two
    /// kinds of "the bar is showing something" can be reasoned about
    /// independently).
    pub suggested_pane_id: Option<String>,
    /// Bumped on every new trigger / cancel. Snapshotted into
    /// `AutofixContext.generation` at submit time; chunks whose
    /// snapshot diverges are dropped as stale.
    pub generation: u64,
    /// Last bottom-bar state we emitted (or would have emitted, if the
    /// tab wasn't active). Used to re-emit on tab_changed so the bar
    /// shows the right state when the user comes back to this tab.
    pub bar_snapshot: AutofixBarSnapshot,
    /// PaneID where the most recent D-synchronous state set happened
    /// (Detected or Pending — both fire ~1ms before PowerShell emits the
    /// next `osc:133;A`). The next prompt-start in that pane is consumed
    /// as the trigger's echo rather than as a "user moved on" dismiss,
    /// otherwise the state we just set would be cleared before reaching
    /// the user. Cleared when the echo A arrives, or when the state
    /// transitions out (set_bar_snapshot → Idle).
    pub trigger_echo_pane: Option<String>,
}

/// Snapshot of the bottom-bar autofix state for one tab. Mirrors the
/// `state` field of the `autofix_state` protocol event so we can rebuild
/// the payload from the cached snapshot when the tab becomes active.
#[derive(Debug, Clone, Default)]
pub enum AutofixBarSnapshot {
    #[default]
    Idle,
    /// Suggest mode: an error was detected but the LLM has not been
    /// invoked. The bar shows a hint inviting the user to press the
    /// hotkey / click the pill to request a fix. Carries enough
    /// context to replay the LLM trigger when the user activates it.
    Detected {
        pane_id: String,
        summary: String,
        hotkey_hint: String,
    },
    /// Analysis in progress ("Analyzing…"). Non-interactive.
    Pending {
        pane_id: String,
        summary: String,
    },
    /// Analysis finished; a result (a fix or an explanation) is waiting in
    /// the agent pane chat. Surfaced ONLY when the pane is not open — the
    /// bar invites the user to open the pane and review. Once the pane
    /// opens, the snapshot flips to `Idle` (the result is already visible
    /// there, so the bar goes quiet). Replaces the old Armed/Suggested
    /// split: autofix no longer auto-executes, so a fix and an explanation
    /// surface identically (open pane → review → act manually).
    Review {
        pane_id: String,
        hotkey_hint: String,
    },
}

impl App {
    /// Auto-fix: when a command fails in another pane, ask the coordinator
    /// agent to suggest a fix. The user confirms before execution.
    pub(super) fn maybe_trigger_autofix(&mut self, notification: &WtNotification) {
        self.trigger_autofix_inner(notification, false);
    }

    /// Core autofix-trigger logic. `forced=true` bypasses the
    /// `autofix_enabled` gate (used when the user explicitly activates a
    /// Detected pill via click or hotkey). When `forced=false` and the
    /// auto-suggest setting is off, this just emits the Detected
    /// snapshot — the LLM is not invoked.
    pub(super) fn trigger_autofix_inner(&mut self, notification: &WtNotification, forced: bool) {
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
        // Without it we can't route the autofix to the right ACP session
        // (the prior code fell back to `self.tab_id` and would land the
        // fix in whichever tab WTA happened to be focused on — see
        // comment block at `maybe_trigger_autofix` head). In release
        // builds we drop the event with a warn instead of panicking,
        // per Step 2 decision #4.
        let target_tab_id = match notification.tab_id.clone() {
            Some(t) => t,
            None => {
                tracing::warn!(
                    target: "autofix",
                    pane_id = %notification.pane_id,
                    "dropping autofix: notification missing tab_id (older WT build?)",
                );
                return;
            }
        };

        // Suggest-mode: when auto-suggest is off AND this isn't a user-
        // forced activation, just surface the Detected pill and let the
        // user decide whether to call the LLM. Skips the busy / generation
        // / submit logic below — none of that machinery applies until the
        // user activates the pill.
        if !self.autofix_enabled && !forced {
            tracing::info!(
                target: "autofix",
                pane_id = %notification.pane_id,
                tab_id = %target_tab_id,
                "auto-suggest off — surfacing Detected pill, no LLM call",
            );
            // D-driven: PowerShell will emit an immediate echo A within
            // ~1ms. Arm the gate so it gets consumed rather than
            // dismissing the pill we just set.
            self.tab_mut(&target_tab_id).autofix.trigger_echo_pane =
                Some(notification.pane_id.clone());
            self.emit_autofix_state_detected(
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
        let (same_pane, already_busy, armed_pane_dbg) = {
            let tab = self.tab_mut(&target_tab_id);
            let same = tab.autofix.pane_id.as_deref() == Some(notification.pane_id.as_str());
            let busy = !tab.turn.is_idle()
                && !matches!(
                    tab.turn,
                    TurnState::Surfaced {
                        end_pending: false,
                        ..
                    }
                );
            (same, busy, tab.autofix.pane_id.clone())
        };

        if already_busy {
            if same_pane {
                // Same pane re-trigger: refresh the bar's summary text but
                // don't re-submit — the agent is already working on it.
                tracing::info!(
                    target: "autofix",
                    pane_id = %notification.pane_id,
                    tab_id = %target_tab_id,
                    "autofix re-trigger same pane while pending — re-emit only",
                );
                // This branch is only reached on a fresh D event (the
                // dispatcher routes vt_sequence here); arm the echo gate.
                self.tab_mut(&target_tab_id).autofix.trigger_echo_pane =
                    Some(notification.pane_id.clone());
                self.emit_autofix_state_pending(
                    &target_tab_id,
                    &notification.pane_id,
                    &notification.summary,
                );
            } else {
                // Different pane while busy: drop. The user can Esc the
                // current autofix to free the slot if they want this one.
                tracing::info!(
                    target: "autofix",
                    pane_id = %notification.pane_id,
                    tab_id = %target_tab_id,
                    armed_pane = ?armed_pane_dbg,
                    "skipping autofix: previous turn still in-flight",
                );
            }
            return;
        }

        // For all other cases (different pane, or Armed state, or Idle):
        // bump the target tab's generation to stale any in-flight response,
        // then submit a new autofix turn via the state machine.
        let new_gen = {
            let tab = self.tab_mut(&target_tab_id);
            tab.autofix.generation = tab.autofix.generation.wrapping_add(1);
            // A new analysis supersedes any leftover suggestion. The C++ side
            // will swap to Pending on the new pending event below; emitting an
            // explicit cleared first would create a flicker.
            tab.autofix.suggested_pane_id = None;
            tab.autofix.generation
        };

        // The auto-fix kind is carried by PromptSubmission::is_autofix,
        // so the text doesn't need a marker prefix — just the raw error
        // summary + instruction.
        let prompt_text = format!(
            "{}\nDiagnose the error and suggest a fix.",
            notification.summary
        );

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
        // AutofixContext), and arm telemetry timing for resolution.
        {
            let tab = self.tab_mut(&target_tab_id);
            tab.autofix.pane_id = Some(notification.pane_id.clone());
            tab.autofix.armed_at = Some(std::time::Instant::now());
        }

        let prompt = PromptSubmission::new_autofix(prompt_text, Some(pane_context));
        let submitted = SubmittedPrompt {
            id: prompt.id,
            text: prompt.text.clone(),
            submitted_at_unix_s: prompt.submitted_at_unix_s,
            autofix: Some(AutofixContext {
                target_pane_id: notification.pane_id.clone(),
                generation: new_gen,
            }),
        };
        // Install the turn on the target tab — bypasses session_to_tab
        // lookup so a tab with no ACP session yet still gets the prompt
        // queued correctly (the ACP layer creates the session lazily when
        // it processes the prompt).
        self.turn_submit_prompt_for_tab(&target_tab_id, submitted);
        tracing::info!(target: "autofix", pane_id = %notification.pane_id, tab_id = %target_tab_id, generation = new_gen, "sending auto-fix prompt");
        let _ = self.prompt_tx.send(prompt);

        // Light up the bottom-bar diagnostic icon in "Pending" state — the
        // user knows something went wrong even before the agent responds.
        // Arm the echo gate ONLY for D-driven entries (forced=false).
        // The `execute_from_detected` path (forced=true) fires this on a
        // stable prompt — no echo A is in flight, and arming would eat
        // the user's first Enter as a fake echo. Bug repro: typo →
        // Detected pill → click pill → Pending → Armed → press Enter
        // (consumed as echo) → press Enter again (finally dismisses).
        if !forced {
            self.tab_mut(&target_tab_id).autofix.trigger_echo_pane =
                Some(notification.pane_id.clone());
        }
        self.emit_autofix_state_pending(
            &target_tab_id,
            &notification.pane_id,
            &notification.summary,
        );
    }

    // ── autofix_state signalling ───────────────────────────────────────────
    //
    // Notifies the TerminalPage about autofix progress via a JSON event on
    // the SendEvent bus. The COM server special-cases method=="autofix_state"
    // and dispatches to TerminalPage.OnAutofixStateChanged (UI thread).
    //
    // Per-tab projection: the bar shows the ACTIVE tab's autofix state. Each
    // emit_autofix_state_* stores the new snapshot on the target tab AND
    // only forwards to WT when the target tab is currently active. On
    // tab_changed, `project_active_tab_state` re-emits the new active
    // tab's snapshot so the bar matches.

    pub(super) fn emit_autofix_state_pending(&mut self, target_tab_id: &str, pane_id: &str, summary: &str) {
        let snapshot = AutofixBarSnapshot::Pending {
            pane_id: pane_id.to_string(),
            summary: summary.to_string(),
        };
        // NOTE: `trigger_echo_pane` is armed by the *caller*, not here —
        // only D-driven calls expect an immediate echo A. The
        // `execute_from_detected` path also funnels through Pending but
        // runs on a stable prompt (no D), so arming inside this helper
        // would consume the user's first real Enter as a fake echo.
        self.set_bar_snapshot(target_tab_id, snapshot);
    }

    /// Suggest-mode entry: error detected but LLM not yet invoked. The
    /// bar shows a clickable hint; the user activates the fix via the
    /// pill or the hotkey, which fires `autofix_execute_from_detected`
    /// and replays through `trigger_autofix_inner` with `force=true`.
    pub(super) fn emit_autofix_state_detected(&mut self, target_tab_id: &str, pane_id: &str, summary: &str) {
        let snapshot = AutofixBarSnapshot::Detected {
            pane_id: pane_id.to_string(),
            summary: summary.to_string(),
            hotkey_hint: "Ctrl+Alt+.".to_string(),
        };
        // See note in `emit_autofix_state_pending`: caller arms the echo
        // gate when (and only when) a D-driven trigger is in progress.
        self.set_bar_snapshot(target_tab_id, snapshot);
    }

    /// Surface the terminal "result ready" state after analysis finishes
    /// (a fix or an explanation — both land in the agent pane chat). When
    /// the pane is closed, show a `Review` hint inviting the user to open
    /// it; when it's already open the result is visible there, so the bar
    /// goes quiet (`Idle`). Re-invoked from the `pane_open` handler so the
    /// bar tracks the pane without the C++ side computing anything.
    pub(super) fn emit_autofix_state_result(&mut self, target_tab_id: &str, pane_id: &str) {
        let pane_open = self
            .tab_sessions
            .get(target_tab_id)
            .map(|t| t.pane_open)
            .unwrap_or(false);
        let snapshot = if pane_open {
            AutofixBarSnapshot::Idle
        } else {
            AutofixBarSnapshot::Review {
                pane_id: pane_id.to_string(),
                hotkey_hint: "Ctrl+Alt+.".to_string(),
            }
        };
        self.set_bar_snapshot(target_tab_id, snapshot);
    }

    /// Execute the currently armed autofix on behalf of the user (they
    /// clicked the bottom-bar button or pressed Ctrl+. in the terminal
    /// window). Mirrors the Enter-key path in the recommendations handler
    /// but without requiring the agent pane to be focused.
    /// User activated the Detected pill (click or hotkey). Read the
    /// active tab's cached snapshot, synthesize a `WtNotification` from
    /// it, and replay through `trigger_autofix_inner` with `forced=true`
    /// so the auto-suggest off gate is bypassed and the LLM call fires.
    pub(super) fn handle_autofix_execute_from_detected(&mut self) {
        let active_tab = self.active_tab_key().to_string();
        let snapshot = self.current_tab().autofix.bar_snapshot.clone();
        let (pane_id, summary) = match snapshot {
            AutofixBarSnapshot::Detected {
                pane_id, summary, ..
            } => (pane_id, summary),
            other => {
                tracing::info!(
                    target: "autofix",
                    state = ?other,
                    "autofix_execute_from_detected: bar not in Detected state — ignoring",
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
        self.trigger_autofix_inner(&notification, true);
    }

    pub(super) fn handle_autofix_execute_request(&mut self, requested_pane_id: &str) {
        let active_tab = self.active_tab_key().to_string();
        let active_armed = self.current_tab().autofix.pane_id.clone();
        tracing::info!(target: "autofix", requested_pane = %requested_pane_id, armed_pane = ?active_armed, has_recs = self.current_tab().turn.recommendations().is_some(), "autofix_execute received");
        // Only execute if the active tab's armed pane matches the request.
        // The bar always reflects the active tab, so the click must target
        // it. The pane_id check prevents a stale UI click from running
        // against an unrelated, more recent error.
        let armed_pane = match active_armed {
            Some(p) if p == requested_pane_id => p,
            _ => {
                tracing::info!(target: "autofix", "autofix_execute: no armed fix for this pane");
                // Tell the UI anyway so it returns to Idle.
                self.emit_autofix_state_cleared(&active_tab);
                return;
            }
        };
        let rec = match self.current_tab().turn.recommendations().cloned() {
            Some(r) => r,
            None => {
                self.emit_autofix_state_cleared(&active_tab);
                let autofix = &mut self.current_tab_mut().autofix;
                autofix.pane_id = None;
                autofix.armed_at = None;
                return;
            }
        };
        let idx = rec
            .recommended_choice
            .unwrap_or(self.current_tab_mut().selected_recommendation)
            .min(rec.choices.len().saturating_sub(1));
        let Some(mut choice) = rec.choices.get(idx).cloned() else {
            self.emit_autofix_state_cleared(&active_tab);
            let autofix = &mut self.current_tab_mut().autofix;
            autofix.pane_id = None;
            autofix.armed_at = None;
            return;
        };
        // Auto-fill parent for Send actions, same as Enter path.
        for action in &mut choice.actions {
            if let crate::coordinator::RecommendedAction::Send { ref mut parent, .. } = action {
                if parent.is_empty() {
                    *parent = armed_pane.clone();
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
            let autofix = &mut self.current_tab_mut().autofix;
            autofix.pane_id = None;
            autofix.armed_at = None;
            self.clear_recommendations();
            let _ = self
                .recommendation_tx
                .send(crate::coordinator::ChoiceExecution {
                    choice,
                    insert_only: false,
                });
        }
        self.push_execution_info(format!("Auto-executing choice {}.", choice_label));
        self.emit_autofix_state_cleared(&active_tab);
        // Defensive: covers the fall-back path above where we dispatched the
        // choice directly without going through `turn_execute_card`. The
        // matched-path case already recomputes via that callee.
        self.recompute_chip_override(&active_tab);
    }

    pub(super) fn emit_autofix_state_cleared(&mut self, target_tab_id: &str) {
        // `cleared` carries no pane info — C++ clears its
        // `lastErrorSessionId` based on the state alone. Reusing the
        // `Idle` snapshot means a subsequent tab switch re-emits a
        // clean state rather than something stale.
        // Also drop any pending trigger-echo gate: once we're back to
        // Idle there's no state to protect, and leaving the pane
        // armed would silently swallow the next real prompt-start.
        self.tab_mut(target_tab_id).autofix.trigger_echo_pane = None;
        // Clearing the bar also ends any "pending review" result, so the
        // pane_open handler won't resurrect a Review hint after a dismiss /
        // exit-0 / Esc. (Completion sets `suggested_pane_id` and surfaces
        // via `emit_autofix_state_result`, never through here.)
        self.tab_mut(target_tab_id).autofix.suggested_pane_id = None;
        self.set_bar_snapshot(target_tab_id, AutofixBarSnapshot::Idle);
    }

    /// Store a fresh bar snapshot on the target tab and, if that tab is
    /// currently active, forward it to WT so the bottom bar updates.
    pub(super) fn set_bar_snapshot(&mut self, target_tab_id: &str, snapshot: AutofixBarSnapshot) {
        self.tab_mut(target_tab_id).autofix.bar_snapshot = snapshot.clone();
        if target_tab_id == self.active_tab_key() {
            send_bar_event(&snapshot, Some(target_tab_id));
        }
    }
}

/// Build and send an `autofix_state` protocol event from a cached bar
/// snapshot. Used by both fresh state transitions (active tab) and the
/// tab_changed re-emit path. Field shape mirrors what C++
/// `OnAutofixStateChanged` consumes.
pub(super) fn send_bar_event(snapshot: &AutofixBarSnapshot, tab_id: Option<&str>) {
    let mut evt = match snapshot {
        AutofixBarSnapshot::Idle => serde_json::json!({
            "type": "event",
            "method": "autofix_state",
            "params": { "state": "cleared" }
        }),
        AutofixBarSnapshot::Detected {
            pane_id,
            summary,
            hotkey_hint,
        } => serde_json::json!({
            "type": "event",
            "method": "autofix_state",
            "params": {
                "state": "detected",
                "pane_id": pane_id,
                "summary": summary,
                "hotkey_hint": hotkey_hint,
            }
        }),
        AutofixBarSnapshot::Pending { pane_id, summary } => serde_json::json!({
            "type": "event",
            "method": "autofix_state",
            "params": {
                "state": "pending",
                "pane_id": pane_id,
                "summary": summary,
            }
        }),
        AutofixBarSnapshot::Review {
            pane_id,
            hotkey_hint,
        } => serde_json::json!({
            "type": "event",
            "method": "autofix_state",
            "params": {
                "state": "review",
                "pane_id": pane_id,
                "hotkey_hint": hotkey_hint,
            }
        }),
    };
    // Tag with tab_id so C++ routes the bottom-bar update to the right
    // tab's AgentPaneContent (window-level bar reflects active tab's
    // autofix state). Without this, the event fans out and a non-active
    // tab's autofix would clobber the bar.
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
