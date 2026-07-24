//! `App`'s state-projection methods (echoing tab/agent state back to the
//! C++/XAML host), split out of the large `app.rs` file. Declared as a
//! regular (non-test) child module of `app` via `#[path]` so it can reach
//! `App`'s private fields and helper methods just like the rest of
//! `app.rs` does.

use super::*;

impl App {
    /// Push the current agent status (name / version / model / connection state)
    /// to the host so a XAML-rendered agent bar can update itself. The COM
    /// server special-cases `method == "agent_status"` and dispatches it
    /// straight to TerminalPage, parallel to the existing `autofix_state`
    /// path. Cheap to call on every state change — the publisher serializes
    /// `wtcli publish` invocations, and an extra one per state transition is
    /// negligible compared to chat traffic.
    pub(super) fn publish_agent_status(&mut self) {
        let state_str = match &self.state {
            ConnectionState::Connecting(_) => "connecting",
            ConnectionState::Connected => "connected",
            ConnectionState::Failed(_) => "failed",
            ConnectionState::Disconnected => "disconnected",
        };
        // Include selected_agent only once — when connected after user selection.
        // This avoids triggering _RebuildAgentStack mid-FRE.
        let selected = if self.state == ConnectionState::Connected {
            self.pending_agent_selection.take()
        } else {
            None
        };
        let mut params = serde_json::json!({
            "name": self.agent_name,
            "version": self.agent_version,
            "model": self.agent_model,
            "backend": self.current_agent_source.display_suffix(),
            "state": state_str,
            "available_models": self.available_models,
            "current_model_id": self.current_model_id,
        });
        if let Some(agent_id) = selected {
            params["selected_agent"] = serde_json::Value::String(agent_id);
        }
        // Tag with the helper's owned tab so C++ routes the title-bar
        // update to the right AgentPaneContent. Without this, OnAgentStatusChanged
        // fans the event out to every agent pane in every window — fine
        // for single-pane setups, broken once multiple helpers each
        // publish their own status (cross-tab title-bar clobber).
        if let Some(ref tab) = self.owner_tab_id {
            params["tab_id"] = serde_json::Value::String(tab.clone());
        }
        let evt = serde_json::json!({
            "type": "event",
            "method": "agent_status",
            "params": params,
        });
        send_wt_protocol_event(evt.to_string());
    }

    /// Single outbound projection of the active tab's agent-pane UI state.
    ///
    /// **Architecture contract**: per-tab agent-pane UI state lives in wta.
    /// C++ has one shared agent pane and one set of XAML flags per window,
    /// so anything that varies across WT tabs must be re-asserted on every
    /// tab switch or local mutation. Emits one unified `agent_state_changed`
    /// snapshot — adding a new piece of per-tab UI state in the future is
    /// a matter of putting another field in the payload, no new IDL route
    /// or new C++ handler.
    ///
    /// Payload shape (mirror of the inbound `set_agent_state` request):
    /// ```json
    /// {
    ///   "type": "event",
    ///   "method": "agent_state_changed",
    ///   "params": {
    ///     "view":      "chat" | "sessions",
    ///     "pane_open": true | false,
    ///     "pane_position": "left" | "right" | "up" | "bottom" | null
    ///   }
    /// }
    /// ```
    ///
    /// On the C++ side this lands in `TerminalPage::OnAgentStateChanged`,
    /// which is the single writer of `_agentSessionsViewActive` and
    /// `Tab.AgentPaneOpen` for the active tab.
    ///
    /// Also re-emits the autofix bar snapshot (orthogonal domain — bottom
    /// bar autofix indicator — kept on its own `autofix_state` route).
    ///
    /// Call sites:
    ///   - `switch_tab_session` end — covers WT `tab_changed`.
    ///   - `set_agent_state` handler end — echoes C++'s request back so C++
    ///     mirrors it (the round-trip the new architecture is built on).
    ///   - `load_session` after the per-tab mutation.
    ///   - Esc out of agent session view, `/sessions` slash command, Ctrl+C×2
    ///     multi-tab reset.
    ///   - Once at startup (after `--initial-view` has been applied) so
    ///     the bar and the agent-pane-open flag both pick up the spawn
    ///     intent.
    ///
    /// Idempotent — safe to call multiple times in a row.
    pub fn project_active_tab_state(&self) {
        let active = self.active_tab_key().to_string();
        self.project_tab_state(&active);
    }

    /// Project the given tab's state to C++ regardless of whether it is the
    /// active tab. Used by `set_agent_state` so a mutation targeting a
    /// non-active tab still echoes back — under per-tab routing C++ can
    /// apply state changes to any tab, not just the focused one, so
    /// the old "defer until next tab_changed" gate was wrong.
    pub fn project_tab_state(&self, target_tab: &str) {
        let Some(tab) = self.tab_sessions.get(target_tab) else {
            tracing::warn!(
                target: "project_tab_state",
                tab_id = %target_tab,
                "no tab_session for target — skipping echo"
            );
            return;
        };
        let view = match tab.current_view {
            View::Agents => "sessions",
            View::Chat => "chat",
        };
        let evt = serde_json::json!({
            "type": "event",
            "method": "agent_state_changed",
            "params": {
                "tab_id":    target_tab,
                "view":      view,
                "pane_open": tab.pane_open,
                "pane_position": tab.agent_pane_position,
            }
        });
        send_wt_protocol_event(evt.to_string());

        // Autofix bar is window-level (single bottom bar reflecting the
        // active tab), so only re-emit when we're projecting the active
        // tab. A non-active mutation does not change the visible bar.
        if target_tab == self.active_tab_key() {
            send_bar_event(&tab.autofix.bar_snapshot, Some(target_tab));
        }
    }
}
