//! `App`'s state-projection methods (echoing tab/agent state back to the
//! C++/XAML host), split out of the large `app.rs` file. Declared as a
//! regular (non-test) child module of `app` via `#[path]` so it can reach
//! `App`'s private fields and helper methods just like the rest of
//! `app.rs` does.

use super::*;

impl App {
    pub(super) fn publish_session_started(&mut self, target_tab: &str, loaded: bool) {
        let Some(tab) = self.tab_sessions.get(target_tab) else {
            return;
        };
        let Some(session_id) = tab.session_id.as_deref().filter(|id| !id.is_empty()) else {
            return;
        };
        if tab.loading_session
            || tab
                .telemetry_model_pending
                .as_ref()
                .is_some_and(|(pending_session, _)| pending_session == session_id)
            || (!loaded && tab.last_telemetry_session_id.as_deref() == Some(session_id))
        {
            return;
        }

        let model_source = match self.telemetry_byok_binding {
            Some(true) => "byok",
            Some(false) => "provider",
            None => "unknown",
        };
        let (automatic_yolo, yolo_policy_blocked, yolo_control_owner) = {
            let state = self.yolo_state.lock().unwrap();
            (
                state.automatic_directive(session_id).target(),
                state.policy_blocked(),
                state.owner(session_id),
            )
        };
        let delegate_agent_id = self
            .delegate_agents
            .as_ref()
            .and_then(|agents| agents.lock().unwrap().first().map(|agent| agent.id.clone()));
        let mut event = build_agent_state_changed_event(target_tab, tab, yolo_control_owner);
        event["params"]["session_started"] = serde_json::json!({
            "start_id": uuid::Uuid::new_v4().to_string(),
            "session_id": session_id,
            "start_kind": if loaded { "Load" } else { "New" },
            "agent_id": self.current_agent_id,
            "agent_source": self.current_agent_source.kind(),
            "delegate_agent_id": delegate_agent_id,
            "model_source": model_source,
            "autofix_enabled": self.autofix_enabled,
            "automatic_yolo": automatic_yolo,
            "yolo_policy_blocked": yolo_policy_blocked,
            "yolo_control_owner": yolo_control_owner.map(crate::app_contracts::YoloControlOwner::as_wire),
        });
        let session_id = session_id.to_string();
        self.tab_mut(target_tab).last_telemetry_session_id = Some(session_id);
        send_wt_protocol_event(event.to_string());
    }

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
        // Include selected_agent only once, after the selected Agent connects,
        // so C++ persists only a completed first-run selection.
        let selected = if self.state == ConnectionState::Connected {
            self.pending_agent_selection.take()
        } else {
            None
        };
        let display_model = self
            .confirmed_model_display()
            .or_else(|| self.agent_model.clone());
        let mut params = serde_json::json!({
            "agent_id": self.current_agent_id,
            "name": self.agent_name,
            "version": self.agent_version,
            "model": display_model,
            "backend": self.current_agent_source.display_suffix(),
            "agent_source": self.current_agent_source.kind(),
            "state": state_str,
            "available_models": self.available_models,
            "current_model_id": self.current_model_id,
            "host_catalog_ready": self.host_catalog_ready,
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
        let projected_session_id = tab.resumable_session_id();
        let yolo_control_owner = projected_session_id
            .as_deref()
            .and_then(|session_id| self.yolo_state.lock().unwrap().owner(session_id));
        let evt = build_agent_state_changed_event(target_tab, tab, yolo_control_owner);
        send_wt_protocol_event(evt.to_string());

        // Autofix bar is window-level (single bottom bar reflecting the
        // active tab), so only re-emit when we're projecting the active
        // tab. A non-active mutation does not change the visible bar.
        if target_tab == self.active_tab_key() {
            send_bar_event(&tab.autofix.bar_snapshot, Some(target_tab));
        }
    }
}

pub(super) fn build_agent_state_changed_event(
    target_tab: &str,
    tab: &TabSession,
    yolo_control_owner: Option<crate::app_contracts::YoloControlOwner>,
) -> serde_json::Value {
    let view = match tab.current_view {
        View::Agents => "sessions",
        View::Chat => "chat",
    };
    let usage = tab.usage.as_ref().map(|snapshot| {
        crate::usage::UsageProjection::with_staleness(snapshot, tab.usage_staleness)
    });
    let projected_session_id = tab.resumable_session_id();
    serde_json::json!({
        "type": "event",
        "method": "agent_state_changed",
        "params": {
            "tab_id": target_tab,
            "agent_session_id": projected_session_id,
            "yolo_control_owner": yolo_control_owner.map(crate::app_contracts::YoloControlOwner::as_wire),
            "view": view,
            "pane_open": tab.pane_open,
            "pane_position": tab.agent_pane_position,
            "usage": usage,
        }
    })
}

#[cfg(test)]
mod session_telemetry_tests {
    use super::*;
    use crate::app::tests::{test_app, test_app_with_master_rx};

    fn starts() -> Vec<serde_json::Value> {
        crate::wt_protocol_events::take_test_published_events()
            .iter()
            .map(|event| serde_json::from_str::<serde_json::Value>(event).unwrap())
            .filter_map(|event| event.pointer("/params/session_started").cloned())
            .collect()
    }

    fn connected(id: &str, ready: bool) -> AppEvent {
        connected_with_binding(id, ready, Some(false))
    }

    fn connected_with_binding(id: &str, ready: bool, binding: Option<bool>) -> AppEvent {
        AppEvent::AgentConnected {
            name: "Copilot".into(),
            model: None,
            version: None,
            session_id: id.into(),
            available_models: vec![],
            current_model_id: Some("provider-model".into()),
            load_session_supported: true,
            image_supported: false,
            session_capabilities_ready: ready,
            telemetry_byok_binding: binding,
        }
    }

    #[test]
    fn session_telemetry_initial_byok_does_not_wait_for_host_catalog() {
        let _locale = crate::test_support::lock_locale();
        let _capture = crate::wt_protocol_events::capture_test_published_events();
        let mut app = test_app();
        app.current_agent_id = "copilot".into();
        app.set_custom_model_config(vec![], Some("custom:private:model".into()));
        app.set_host_catalog_ready(false);
        assert!(app.selected_custom_model_id().is_none());

        app.handle_event(connected_with_binding("created", true, Some(true)));
        let snapshots = starts();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0]["model_source"], "byok");
        assert!(!snapshots[0].to_string().contains("private"));

        app.handle_event(AppEvent::WtEvent {
            method: "agent_config_changed".into(),
            pane_id: String::new(),
            tab_id: None,
            params: serde_json::json!({
                "custom_models": [{
                    "selection_id": "custom:private:model",
                    "model_id": "private-model"
                }],
                "custom_model_selection": "custom:private:model"
            }),
        });
        assert!(app.host_catalog_ready);
        assert!(app.selected_custom_model_id().is_some());
        app.publish_session_started(DEFAULT_TAB_ID, false);
        assert!(starts().is_empty());
    }

    #[test]
    fn session_telemetry_binding_wins_over_catalog_and_pane_model_names() {
        let _locale = crate::test_support::lock_locale();
        let _capture = crate::wt_protocol_events::capture_test_published_events();
        for (binding, expected) in [
            (Some(false), "provider"),
            (Some(true), "byok"),
            (None, "unknown"),
        ] {
            let mut app = test_app();
            app.set_custom_model_config(
                vec![CustomModelCatalogEntry {
                    selection_id: "custom:selected".into(),
                    model_id: "provider-model".into(),
                    ..Default::default()
                }],
                Some("custom:selected".into()),
            );
            app.tab_mut(DEFAULT_TAB_ID).model_override = Some("provider-model".into());
            app.handle_event(connected_with_binding("created", true, binding));
            let snapshots = starts();
            assert_eq!(snapshots.len(), 1);
            assert_eq!(snapshots[0]["model_source"], expected);
        }
    }

    fn attached(tab: &str, id: &str) -> AppEvent {
        AppEvent::SessionAttached {
            tab_id: tab.into(),
            session_id: id.into(),
            prompt_id: None,
            available_models: vec![],
            current_model_id: Some("provider-model".into()),
        }
    }

    #[test]
    fn session_telemetry_bootstrap_emits_once_not_on_projection_or_handshake_only() {
        let _locale = crate::test_support::lock_locale();
        let _capture = crate::wt_protocol_events::capture_test_published_events();
        let mut app = test_app();
        app.handle_event(connected("pending-load", false));
        assert!(starts().is_empty());
        app.handle_event(connected("created", true));
        let snapshots = starts();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0]["start_kind"], "New");
        assert_eq!(snapshots[0]["session_id"], "created");
        assert!(uuid::Uuid::parse_str(snapshots[0]["start_id"].as_str().unwrap()).is_ok());
        app.project_active_tab_state();
        app.handle_event(connected("created", true));
        assert!(starts().is_empty());
    }

    #[test]
    fn session_telemetry_load_only_emits_for_successful_target_and_counts_reload() {
        let _locale = crate::test_support::lock_locale();
        let _capture = crate::wt_protocol_events::capture_test_published_events();
        let mut app = test_app();
        for _ in 0..2 {
            let tab = app.tab_mut(DEFAULT_TAB_ID);
            tab.loading_session = true;
            tab.loading_target_session_id = Some("saved".into());
            app.handle_event(attached(DEFAULT_TAB_ID, "unrelated"));
            app.publish_session_started(DEFAULT_TAB_ID, true);
            assert!(starts().is_empty());
            app.handle_event(attached(DEFAULT_TAB_ID, "saved"));
            let snapshots = starts();
            assert_eq!(snapshots.len(), 1);
            assert_eq!(snapshots[0]["start_kind"], "Load");
            assert_eq!(snapshots[0]["session_id"], "saved");
            app.handle_event(attached(DEFAULT_TAB_ID, "saved"));
            assert!(starts().is_empty());
        }
    }

    #[test]
    fn session_telemetry_waits_for_tab_model_selection_and_uses_confirmed_value() {
        let _locale = crate::test_support::lock_locale();
        let _capture = crate::wt_protocol_events::capture_test_published_events();
        let (mut app, mut requests) = test_app_with_master_rx();
        app.tab_mut("background").model_override = Some("custom:chosen".into());
        app.telemetry_byok_binding = Some(true);
        app.custom_model_catalog = vec![CustomModelCatalogEntry {
            selection_id: "custom:chosen".into(),
            model_id: "private-model".into(),
            ..Default::default()
        }];
        app.handle_event(attached("background", "new-session"));
        assert!(starts().is_empty());
        let crate::protocol::acp::client::MasterExtRequest::SetSessionModel {
            request_id,
            session_id,
            ..
        } = requests.try_recv().unwrap()
        else {
            panic!("expected the initial model request");
        };
        assert_eq!(session_id.unwrap().to_string(), "new-session");
        // Duplicate-value requests still have independent completion identities.
        app.handle_event(AppEvent::ModelSetCompleted {
            request_id: uuid::Uuid::new_v4(),
            session_id: "new-session".into(),
            model: "custom:chosen".into(),
            pane_override: false,
        });
        app.handle_event(AppEvent::ModelSetFailed {
            request_id: uuid::Uuid::new_v4(),
            session_id: "new-session".into(),
            model: "custom:chosen".into(),
            pane_override: false,
            message: "not supported".into(),
        });
        assert!(starts().is_empty());
        app.handle_event(AppEvent::ModelSetCompleted {
            request_id,
            session_id: "new-session".into(),
            model: "custom:chosen".into(),
            pane_override: false,
        });
        let snapshots = starts();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0]["model_source"], "byok");
        assert!(!snapshots[0].to_string().contains("private-model"));
        assert!(!snapshots[0].to_string().contains("custom:chosen"));
        app.handle_event(AppEvent::ModelSetCompleted {
            request_id,
            session_id: "new-session".into(),
            model: "custom:chosen".into(),
            pane_override: false,
        });
        assert!(starts().is_empty());
    }

    #[test]
    fn session_telemetry_failed_model_selection_reports_previous_provider_model() {
        let _locale = crate::test_support::lock_locale();
        let _capture = crate::wt_protocol_events::capture_test_published_events();
        let (mut app, _requests) = test_app_with_master_rx();
        app.telemetry_byok_binding = Some(false);
        app.acp_model = Some("custom:unavailable".into());
        app.handle_event(attached(DEFAULT_TAB_ID, "created"));
        assert!(starts().is_empty());
        let request_id = app
            .tab_mut(DEFAULT_TAB_ID)
            .telemetry_model_pending
            .as_ref()
            .unwrap()
            .1;
        app.handle_event(AppEvent::ModelSetFailed {
            request_id,
            session_id: "created".into(),
            model: "custom:unavailable".into(),
            pane_override: false,
            message: "not supported".into(),
        });
        let snapshots = starts();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0]["model_source"], "provider");
    }

    #[test]
    fn session_telemetry_closed_model_channel_does_not_leave_snapshot_pending() {
        let _locale = crate::test_support::lock_locale();
        let _capture = crate::wt_protocol_events::capture_test_published_events();
        let mut app = test_app();
        app.telemetry_byok_binding = Some(false);
        app.acp_model = Some("unavailable-model".into());
        app.handle_event(attached(DEFAULT_TAB_ID, "created"));
        let snapshots = starts();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0]["model_source"], "provider");
        assert!(app
            .tab_mut(DEFAULT_TAB_ID)
            .telemetry_model_pending
            .is_none());
    }

    #[test]
    fn session_telemetry_load_clears_pending_model_and_preserves_byok_binding() {
        let _locale = crate::test_support::lock_locale();
        let _capture = crate::wt_protocol_events::capture_test_published_events();
        let (mut app, _requests) = test_app_with_master_rx();
        app.telemetry_byok_binding = Some(true);
        app.acp_model = Some("custom:chosen".into());
        app.handle_event(attached(DEFAULT_TAB_ID, "created"));
        assert!(starts().is_empty());
        let tab = app.tab_mut(DEFAULT_TAB_ID);
        tab.loading_session = true;
        tab.loading_target_session_id = Some("saved".into());
        app.handle_event(attached(DEFAULT_TAB_ID, "saved"));
        let snapshots = starts();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0]["start_kind"], "Load");
        assert_eq!(snapshots[0]["model_source"], "byok");
        app.handle_event(AppEvent::ModelSetCompleted {
            request_id: uuid::Uuid::new_v4(),
            session_id: "created".into(),
            model: "custom:chosen".into(),
            pane_override: false,
        });
        assert!(starts().is_empty());
    }

    #[test]
    fn session_telemetry_rebind_clears_binding_and_ignores_old_model_completion() {
        let _locale = crate::test_support::lock_locale();
        let _capture = crate::wt_protocol_events::capture_test_published_events();
        let (mut app, _requests) = test_app_with_master_rx();
        app.telemetry_byok_binding = Some(true);
        app.acp_model = Some("provider-model".into());
        app.handle_event(attached(DEFAULT_TAB_ID, "reused-id"));
        let old_request = app
            .current_tab()
            .telemetry_model_pending
            .as_ref()
            .unwrap()
            .1;
        app.reset_agent_scoped_state();
        assert!(app.telemetry_byok_binding.is_none());
        assert!(app.current_tab().telemetry_model_pending.is_none());
        assert!(app.current_tab().last_telemetry_session_id.is_none());

        app.handle_event(connected_with_binding("bootstrap", false, Some(false)));
        app.handle_event(attached(DEFAULT_TAB_ID, "reused-id"));
        let new_request = app
            .current_tab()
            .telemetry_model_pending
            .as_ref()
            .unwrap()
            .1;
        assert_ne!(old_request, new_request);
        app.handle_event(AppEvent::ModelSetCompleted {
            request_id: old_request,
            session_id: "reused-id".into(),
            model: "old-model".into(),
            pane_override: false,
        });
        assert!(starts().is_empty());
        app.handle_event(AppEvent::ModelSetFailed {
            request_id: old_request,
            session_id: "reused-id".into(),
            model: "old-model".into(),
            pane_override: false,
            message: "retired".into(),
        });
        assert!(starts().is_empty());
        app.handle_event(AppEvent::ModelSetCompleted {
            request_id: new_request,
            session_id: "reused-id".into(),
            model: "provider-model".into(),
            pane_override: false,
        });
        let snapshots = starts();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0]["model_source"], "provider");
    }

    #[test]
    fn session_telemetry_policy_and_restored_owner_are_not_global_yolo_defaults() {
        let _locale = crate::test_support::lock_locale();
        let _capture = crate::wt_protocol_events::capture_test_published_events();
        let mut app = test_app();
        app.current_agent_source = crate::agent_source::AgentSource::Wsl {
            distro: "private-distro".into(),
        };
        app.autofix_enabled = true;
        app.tab_mut(DEFAULT_TAB_ID).session_id = Some("session".into());
        {
            let mut state = app.yolo_state.lock().unwrap();
            state.update_runtime(true, true);
            state.mark_manual("session");
        }
        app.publish_session_started(DEFAULT_TAB_ID, false);
        let snapshots = starts();
        assert_eq!(snapshots[0]["automatic_yolo"], false);
        assert_eq!(snapshots[0]["yolo_policy_blocked"], true);
        assert_eq!(snapshots[0]["agent_source"], "wsl");
        assert_eq!(snapshots[0]["autofix_enabled"], true);
        assert!(!snapshots[0].to_string().contains("private-distro"));

        {
            let mut state = app.yolo_state.lock().unwrap();
            state.update_runtime(true, false);
            state.mark_provider_restored("session");
        }
        app.publish_session_started(DEFAULT_TAB_ID, true);
        let snapshots = starts();
        assert!(snapshots[0]["automatic_yolo"].is_null());
        assert_eq!(snapshots[0]["yolo_control_owner"], "provider-restored");
    }

    #[test]
    fn session_telemetry_deduplication_moves_with_tab_state() {
        let _locale = crate::test_support::lock_locale();
        let _capture = crate::wt_protocol_events::capture_test_published_events();
        let mut app = test_app();
        app.tab_mut("old").session_id = Some("session".into());
        app.publish_session_started("old", false);
        assert_eq!(starts().len(), 1);
        let tab = app.tab_sessions.remove("old").unwrap();
        app.tab_sessions.insert("new".into(), tab);
        app.publish_session_started("new", false);
        assert!(starts().is_empty());
    }
}
