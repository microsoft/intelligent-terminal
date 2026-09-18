use super::*;
use crate::agent_sessions::{AgentStatus, SessionLocation};
use crate::tmux_hooks::tests::{hook, key, PANE_A, PANE_B, PANE_C};

async fn route(state: &Arc<MasterStateInner>, params: serde_json::Value) {
    handle_master_wt_event(
        state,
        serde_json::json!({"method": "agent_event", "params": params}),
    )
    .await;
}

#[tokio::test]
async fn tmux_master_tracks_without_helpers_and_preserves_location_over_wire() {
    let state = make_state();
    let mut local = hook("agent.session.start", PANE_C, "copilot", "same");
    local.as_object_mut().unwrap().remove("tmux");
    route(&state, local).await;
    for pane in [PANE_A, PANE_B] {
        route(&state, hook("agent.session.start", pane, "copilot", "same")).await;
    }
    assert_eq!(state.registry.snapshot().await.len(), 3);
    assert_eq!(
        state
            .registry
            .lookup(&SessionId::new("same"))
            .await
            .unwrap()
            .location,
        SessionLocation::Host
    );
    let (tx, mut notifications) = mpsc::unbounded_channel();
    state
        .helper_ext_subscribers
        .lock()
        .await
        .insert(HelperId(1), tx);
    let sid = SessionId::new(key(PANE_A, "copilot", "same"));
    for (event, status) in [
        ("agent.prompt.submit", AgentStatus::Working),
        ("agent.session.start", AgentStatus::Working),
        ("agent.notification", AgentStatus::Attention),
        ("agent.stop", AgentStatus::Idle),
        ("agent.error", AgentStatus::Error),
        ("agent.session.end", AgentStatus::Ended),
    ] {
        let mut params = hook(
            event,
            &format!("{{{}}}", PANE_A.to_uppercase()),
            "copilot",
            "same",
        );
        params["tab_id"] = 77.into();
        params["window_id"] = "9".into();
        params["tmux"]["session_name"] = "renamed work".into();
        route(&state, params).await;
        let info = state.registry.lookup(&sid).await.unwrap();
        assert_eq!(info.status, Some(status.clone()));
        assert!(
            matches!(&info.location, SessionLocation::Tmux { session_name, pane_id, .. }
            if session_name == "renamed work" && pane_id == "%1")
        );
        let wire = serde_json::to_value(crate::session_registry::SessionsListResponse {
            sessions: vec![info.clone()],
        })
        .unwrap();
        let restored: crate::session_registry::SessionsListResponse =
            serde_json::from_value(wire).unwrap();
        let restored = &restored.sessions[0];
        assert_eq!(restored.location, info.location);
        let helper_row = crate::app::session_info_to_agent_session(restored);
        assert_eq!(helper_row.location, info.location);
        assert_eq!(
            crate::session_registry::agent_session_to_session_info(&helper_row).location,
            info.location
        );
        assert_eq!(
            info.pane_session_id.as_deref(),
            (status != AgentStatus::Ended).then_some(PANE_A)
        );
        assert!(notifications.try_recv().is_ok());
        assert!(
            notifications.try_recv().is_err(),
            "one hook yields one changed notification"
        );
    }
    assert!(state.hook_owned.lock().await.contains(&sid));
    assert_eq!(state.registry.snapshot().await.len(), 3);
}

#[tokio::test]
async fn tmux_master_sessionless_binding_never_crosses_panes_or_providers() {
    let state = make_state();
    route(
        &state,
        hook("agent.session.start", PANE_A, "copilot", "same"),
    )
    .await;
    let mut local = hook("agent.session.start", PANE_C, "copilot", "local");
    local.as_object_mut().unwrap().remove("tmux");
    route(&state, local).await;
    for (pane, cli) in [(PANE_B, "copilot"), (PANE_C, "copilot"), (PANE_A, "claude")] {
        route(&state, hook("agent.notification", pane, cli, "")).await;
    }
    let sid = SessionId::new(key(PANE_A, "copilot", "same"));
    assert_eq!(state.registry.snapshot().await.len(), 2);
    assert_eq!(
        state.registry.lookup(&sid).await.unwrap().status,
        Some(AgentStatus::Idle)
    );
    assert_eq!(
        state
            .registry
            .lookup(&SessionId::new("local"))
            .await
            .unwrap()
            .status,
        Some(AgentStatus::Idle)
    );
    route(&state, hook("agent.notification", PANE_A, "copilot", "")).await;
    assert_eq!(
        state.registry.lookup(&sid).await.unwrap().status,
        Some(AgentStatus::Attention)
    );
    let mut missing = hook("agent.stop", PANE_A, "copilot", "");
    missing.as_object_mut().unwrap().remove("agent_session_id");
    missing["tmux"]["session_id"] = "$7".into();
    missing["tmux"]["session_name"] = "moved session".into();
    route(&state, missing).await;
    assert_eq!(
        state.registry.lookup(&sid).await.unwrap().status,
        Some(AgentStatus::Idle)
    );
    assert!(matches!(
        state.registry.lookup(&sid).await.unwrap().location,
        SessionLocation::Tmux { session_id, .. } if session_id == "$7"
    ));
    route(
        &state,
        hook("agent.prompt.submit", PANE_A, "copilot", "same"),
    )
    .await;
    let mut local_missing = hook("agent.notification", PANE_B, "copilot", "");
    local_missing.as_object_mut().unwrap().remove("tmux");
    route(&state, local_missing).await;
    assert_eq!(
        state
            .registry
            .lookup(&SessionId::new("local"))
            .await
            .unwrap()
            .status,
        Some(AgentStatus::Attention)
    );
    assert_eq!(
        state.registry.lookup(&sid).await.unwrap().status,
        Some(AgentStatus::Working)
    );
    route(
        &state,
        hook("agent.session.start", PANE_A, "claude", "same"),
    )
    .await;
    route(&state, hook("agent.error", PANE_A, "copilot", "same")).await;
    let replacement = SessionId::new(key(PANE_A, "claude", "same"));
    assert_ne!(sid, replacement);
    assert_eq!(
        state.registry.lookup(&sid).await.unwrap().status,
        Some(AgentStatus::Ended)
    );
    assert_eq!(
        state.registry.lookup(&replacement).await.unwrap().status,
        Some(AgentStatus::Idle)
    );
    handle_master_wt_event(
        &state,
        serde_json::json!({
            "method": "connection_state",
            "params": {"pane_id": PANE_A, "state": "closed"}
        }),
    )
    .await;
    route(&state, hook("agent.notification", PANE_A, "claude", "")).await;
    let row = state.registry.lookup(&replacement).await.unwrap();
    assert_eq!(row.status, Some(AgentStatus::Ended));
    assert_eq!(row.pane_session_id, None);
    assert!(matches!(row.location, SessionLocation::Tmux { .. }));
}

#[tokio::test]
async fn tmux_master_drops_sidekicks_and_malformed_metadata_without_host_fallback() {
    let state = make_state();
    for event in [
        "agent.session.start",
        "agent.notification",
        "agent.error",
        "agent.session.end",
    ] {
        route(&state, hook(event, PANE_A, "copilot", "sidekick-worker")).await;
        let mut malformed = hook(event, PANE_A, "copilot", "sid");
        malformed["tmux"]["pane_id"] = 1.into();
        route(&state, malformed).await;
    }
    assert!(state.registry.snapshot().await.is_empty());
    assert!(state.hook_owned.lock().await.is_empty());
}

#[tokio::test]
async fn tmux_master_focus_uses_controller_pane_not_remote_metadata() {
    let mock = Arc::new(MockWtChannel::ok());
    let state = make_state_with_wt(mock.clone());
    route(
        &state,
        hook("agent.session.start", PANE_A, "copilot", "sid"),
    )
    .await;
    let sid = SessionId::new(key(PANE_A, "copilot", "sid"));
    handle_focus_session(&state, &focus_params_for(&sid))
        .await
        .unwrap();
    assert_eq!(
        mock.calls(),
        vec![(
            "focus_pane".into(),
            serde_json::json!({"session_id": PANE_A})
        )]
    );
    handle_master_wt_event(
        &state,
        serde_json::json!({
            "method": "connection_state", "params": {"pane_id": PANE_A, "state": "closed"}
        }),
    )
    .await;
    assert!(handle_focus_session(&state, &focus_params_for(&sid))
        .await
        .is_err());
    assert_eq!(mock.calls().len(), 1);
}

#[tokio::test]
async fn tmux_master_initial_attach_accepts_empty_metadata_and_ignored_hooks_are_no_ops() {
    let state = make_state();
    let mut params = hook("agent.session.start", PANE_A, "copilot", "sid");
    params["tmux"]["session_name"] = "".into();
    params["tmux"]["socket_path"] = "".into();
    route(&state, params.clone()).await;
    let sid = SessionId::new(key(PANE_A, "copilot", "sid"));
    let initial = state.registry.lookup(&sid).await.unwrap();
    for event in ["agent.unknown", "agent.tool.finished", "agent.notification"] {
        let mut ignored = hook(event, PANE_A, "copilot", "sid");
        ignored["payload"]["notification_type"] = "idle_prompt".into();
        route(&state, ignored).await;
        assert_eq!(state.registry.lookup(&sid).await.unwrap(), initial);
    }
    for metadata in [
        serde_json::Value::Null,
        serde_json::json!({"session_id": "$0", "pane_id": "%1"}),
    ] {
        params["tmux"] = metadata;
        route(&state, params.clone()).await;
        assert_eq!(state.registry.lookup(&sid).await.unwrap(), initial);
    }
}
