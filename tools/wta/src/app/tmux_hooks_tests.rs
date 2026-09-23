use super::*;
use crate::agent_sessions::{
    AgentSessionRegistry, AgentStatus, SessionEvent, SessionLocation, SessionOrigin,
};
use crate::tmux_hooks::tests::{hook, key, PANE_A, PANE_B, PANE_C};

fn route(reg: &mut AgentSessionRegistry, params: &serde_json::Value) -> bool {
    route_agent_event_to_registry(reg, params["pane_id"].as_str().unwrap(), params)
}

#[test]
fn tmux_helper_detach_clears_binding_without_ending_remote_activity() {
    let mut reg = AgentSessionRegistry::new();
    route(
        &mut reg,
        &hook("agent.prompt.submit", PANE_A, "copilot", "live"),
    );
    let sid = key(PANE_A, "copilot", "live");
    let before = reg.get(&sid).unwrap().clone();
    reg.apply(SessionEvent::PaneDetached {
        pane_session_id: format!("{{{}}}", PANE_A.to_uppercase()),
    });
    let detached = reg.get(&sid).unwrap();
    assert_eq!(detached.status, AgentStatus::Working);
    assert_eq!(detached.current_tool, before.current_tool);
    assert_eq!(detached.last_activity_at, before.last_activity_at);
    assert!(detached.pane_session_id.is_none());
    assert!(reg.key_for_pane(PANE_A).is_none());
    reg.apply(SessionEvent::PaneClosed {
        pane_session_id: PANE_A.into(),
    });
    assert_eq!(reg.get(&sid).unwrap().status, AgentStatus::Working);
    reg.apply(SessionEvent::SessionStopped {
        key: sid.clone(),
        reason: "agent exited".into(),
    });
    assert_eq!(reg.get(&sid).unwrap().status, AgentStatus::Ended);
}

#[test]
fn tmux_ssh_hooks_do_not_create_helper_local_host_duplicates() {
    let target = crate::ssh_sessions::SshTarget::new("wsl-ubuntu", None).unwrap();
    let mut reg = AgentSessionRegistry::new();
    for event in [
        "agent.session.start",
        "agent.prompt.submit",
        "agent.notification",
        "agent.stop",
        "agent.session.end",
    ] {
        let params = crate::tmux_hooks::tests::ssh_hook(event, PANE_A, "copilot", "same", &target);
        assert!(!route(&mut reg, &params));
    }
    assert!(reg.iter_sorted().is_empty());
    assert!(route(
        &mut reg,
        &hook("agent.session.start", PANE_B, "copilot", "same")
    ));
    assert!(matches!(
        reg.get(&key(PANE_B, "copilot", "same")).unwrap().location,
        SessionLocation::Tmux { .. }
    ));
}

#[test]
fn tmux_helper_hooks_keep_local_wsl_panes_and_providers_distinct() {
    let mut reg = AgentSessionRegistry::new();
    let mut local = hook("agent.session.start", PANE_C, "copilot", "shared");
    local.as_object_mut().unwrap().remove("tmux");
    assert!(route(&mut reg, &local));
    for (pane, cli) in [(PANE_A, "copilot"), (PANE_B, "copilot"), (PANE_A, "claude")] {
        assert!(route(
            &mut reg,
            &hook("agent.session.start", pane, cli, "shared")
        ));
        let remote = reg.get(&key(pane, cli, "shared")).unwrap();
        assert!(matches!(remote.location, SessionLocation::Tmux { .. }));
        assert_eq!(remote.pane_session_id.as_deref(), Some(pane));
    }
    assert_eq!(reg.iter_sorted().len(), 4);
    assert_eq!(
        reg.get(&"shared".to_string()).unwrap().location,
        SessionLocation::Host
    );
    assert_eq!(
        reg.get(&"shared".to_string()).unwrap().status,
        AgentStatus::Idle
    );
    reg.set_location(
        "shared",
        SessionLocation::Wsl {
            distro: "Ubuntu".into(),
        },
    );
    route(
        &mut reg,
        &hook("agent.prompt.submit", PANE_B, "copilot", "shared"),
    );
    assert!(reg.get(&"shared".to_string()).unwrap().location.is_wsl());
    assert_eq!(
        reg.get(&"shared".to_string()).unwrap().status,
        AgentStatus::Idle
    );
}

#[test]
fn tmux_helper_sessionless_hooks_require_matching_live_pane_and_provider() {
    let mut reg = AgentSessionRegistry::new();
    route(
        &mut reg,
        &hook("agent.session.start", PANE_A, "copilot", "sid"),
    );
    let mut local = hook("agent.session.start", PANE_C, "copilot", "local");
    local.as_object_mut().unwrap().remove("tmux");
    route(&mut reg, &local);
    for (pane, cli) in [(PANE_B, "copilot"), (PANE_C, "copilot"), (PANE_A, "claude")] {
        assert!(!route(&mut reg, &hook("agent.notification", pane, cli, "")));
    }
    assert_eq!(reg.iter_sorted().len(), 2);
    assert_eq!(reg.get(&"local".into()).unwrap().status, AgentStatus::Idle);
    let remote_key = key(PANE_A, "copilot", "sid");
    assert_eq!(reg.get(&remote_key).unwrap().status, AgentStatus::Idle);
    assert!(route(
        &mut reg,
        &hook("agent.notification", PANE_A, "copilot", "")
    ));
    assert_eq!(reg.get(&remote_key).unwrap().status, AgentStatus::Attention);
    let mut missing = hook("agent.stop", PANE_A, "copilot", "");
    missing.as_object_mut().unwrap().remove("agent_session_id");
    missing["tmux"]["session_id"] = "$7".into();
    missing["tmux"]["session_name"] = "moved session".into();
    assert!(route(&mut reg, &missing));
    assert_eq!(reg.get(&remote_key).unwrap().status, AgentStatus::Idle);
    assert!(matches!(
        &reg.get(&remote_key).unwrap().location,
        SessionLocation::Tmux { session_id, .. } if session_id == "$7"
    ));
    route(
        &mut reg,
        &hook("agent.prompt.submit", PANE_A, "copilot", "sid"),
    );
    let mut local_missing = hook("agent.notification", PANE_B, "copilot", "");
    local_missing.as_object_mut().unwrap().remove("tmux");
    assert!(route(&mut reg, &local_missing));
    assert_eq!(
        reg.get(&"local".into()).unwrap().status,
        AgentStatus::Attention
    );
    assert_eq!(reg.get(&remote_key).unwrap().status, AgentStatus::Working);
    route(
        &mut reg,
        &hook("agent.session.end", PANE_A, "copilot", "sid"),
    );
    assert!(!route(
        &mut reg,
        &hook("agent.notification", PANE_A, "copilot", "")
    ));
}

#[test]
fn tmux_helper_lifecycle_focuses_native_pane_and_never_resumes_locally() {
    let _locale = crate::test_support::lock_locale();
    let mut app = super::tests::test_app();
    let remote_key = key(PANE_A, "copilot", "sid");
    for (event, status) in [
        ("agent.session.start", AgentStatus::Idle),
        ("agent.prompt.submit", AgentStatus::Working),
        ("agent.notification", AgentStatus::Attention),
        ("agent.stop", AgentStatus::Idle),
        ("agent.error", AgentStatus::Error),
        ("agent.session.end", AgentStatus::Ended),
    ] {
        route(
            &mut app.agent_sessions,
            &hook(event, PANE_A, "copilot", "sid"),
        );
        let row = app.agent_sessions.get(&remote_key).unwrap().clone();
        assert_eq!(row.status, status);
        assert!(matches!(row.location, SessionLocation::Tmux { .. }));
        if status != AgentStatus::Ended {
            app.activate_agent_session_routed(&row);
            let dispatch = app.last_dispatched_command.as_ref().unwrap();
            assert_eq!(dispatch.kind, DispatchedCommandKind::FocusPane);
            assert_eq!(dispatch.session_id.as_deref(), Some(remote_key.as_str()));
            assert_eq!(row.pane_session_id.as_deref(), Some(PANE_A));
            assert!(!dispatch.argv.iter().any(|arg| arg == "%1"));
        }
    }
    let ended = app.agent_sessions.get(&remote_key).unwrap().clone();
    assert!(ended.pane_session_id.is_none());
    for status in [AgentStatus::Ended, AgentStatus::Historical] {
        for origin in [SessionOrigin::Unknown, SessionOrigin::AgentPane] {
            let mut row = ended.clone();
            row.status = status.clone();
            row.origin = origin;
            app.agent_supports_load_session = true;
            app.activate_agent_session_routed(&row);
            let dispatch = app.last_dispatched_command.as_ref().unwrap();
            assert_eq!(dispatch.kind, DispatchedCommandKind::NotResumable);
            assert_eq!(dispatch.argv[1], "CliHasNoResumeFlag");
            assert!(crate::wt_protocol_events::resumed_pane_binding_event(
                "copilot",
                &row.key,
                PANE_A,
                &row.location
            )
            .is_none());
            app.last_dispatched_command = None;
            app.dispatch_resume(&row);
            assert!(app.last_dispatched_command.is_none());
        }
    }
}

#[test]
fn tmux_helper_drops_sidekicks_malformed_metadata_and_stale_errors() {
    let mut reg = AgentSessionRegistry::new();
    for event in [
        "agent.session.start",
        "agent.notification",
        "agent.error",
        "agent.session.end",
    ] {
        let params = hook(event, PANE_A, "copilot", "sidekick-internal");
        let mut applied = Vec::new();
        assert!(!route_agent_event_to_registry_with_hook_sink(
            &mut reg,
            PANE_A,
            &params,
            |event| applied.push(event)
        ));
        assert!(applied.is_empty());
        assert!(!reg.has_session(&key(PANE_A, "copilot", "sidekick-internal")));
        let mut invalid = hook(event, PANE_A, "copilot", "invalid");
        invalid["tmux"] = serde_json::Value::Null;
        assert!(!route(&mut reg, &invalid));
    }
    assert!(reg.iter_sorted().is_empty());
    route(
        &mut reg,
        &hook("agent.session.start", PANE_A, "copilot", "old"),
    );
    route(
        &mut reg,
        &hook("agent.session.start", PANE_A, "claude", "new"),
    );
    assert!(!route(
        &mut reg,
        &hook("agent.error", PANE_A, "copilot", "old")
    ));
    assert_eq!(
        reg.get(&key(PANE_A, "claude", "new")).unwrap().status,
        AgentStatus::Idle
    );
    reg.apply(SessionEvent::PaneClosed {
        pane_session_id: PANE_A.into(),
    });
    let row = reg.get(&key(PANE_A, "claude", "new")).unwrap();
    assert_eq!(row.status, AgentStatus::Ended);
    assert!(matches!(row.location, SessionLocation::Tmux { .. }));
}

#[test]
fn tmux_helper_initial_attach_accepts_empty_metadata_and_ignored_hooks_are_no_ops() {
    let mut reg = AgentSessionRegistry::new();
    let mut params = hook("agent.session.start", PANE_A, "copilot", "sid");
    params["tmux"]["session_name"] = "".into();
    params["tmux"]["socket_path"] = "".into();
    assert!(route(&mut reg, &params));
    let remote_key = key(PANE_A, "copilot", "sid");
    let initial = reg.get(&remote_key).unwrap().clone();
    for event in ["agent.unknown", "agent.tool.finished", "agent.notification"] {
        let mut ignored = hook(event, PANE_A, "copilot", "sid");
        ignored["payload"]["notification_type"] = "idle_prompt".into();
        let mut applied = Vec::new();
        assert!(!route_agent_event_to_registry_with_hook_sink(
            &mut reg,
            PANE_A,
            &ignored,
            |event| applied.push(event)
        ));
        assert!(applied.is_empty());
        let row = reg.get(&remote_key).unwrap();
        assert_eq!(row.location, initial.location);
        assert_eq!(row.status, initial.status);
        assert_eq!(row.last_activity_at, initial.last_activity_at);
    }
    for metadata in [
        serde_json::Value::Null,
        serde_json::json!({"session_id": "$0", "pane_id": "%1"}),
    ] {
        params["tmux"] = metadata;
        assert!(!route(&mut reg, &params));
        assert_eq!(reg.get(&remote_key).unwrap().location, initial.location);
        assert_eq!(
            reg.get(&remote_key).unwrap().last_activity_at,
            initial.last_activity_at
        );
    }
}

#[test]
fn tmux_normal_sessions_view_shows_remote_live_rows_without_crossing_explicit_sources() {
    let _locale = crate::test_support::lock_locale();
    let mut app = super::tests::test_app();
    app.current_agent_id = "copilot".into();
    app.sessions_origin_filter = MVP_SESSIONS_ORIGIN_FILTER;
    assert_eq!(
        app.current_agent_source,
        crate::agent_source::AgentSource::Host
    );
    let mut host = hook("agent.session.start", PANE_C, "copilot", "shared");
    host.as_object_mut().unwrap().remove("tmux");
    route(&mut app.agent_sessions, &host);
    route(
        &mut app.agent_sessions,
        &hook("agent.prompt.submit", PANE_A, "copilot", "shared"),
    );
    let remote_key = key(PANE_A, "copilot", "shared");
    let rows = app.agents_rows_for_tab(DEFAULT_TAB_ID);
    assert_eq!(
        rows.len(),
        2,
        "the helper-local view includes host and tmux"
    );
    assert!(rows
        .iter()
        .any(|row| row.key == "shared" && row.location == SessionLocation::Host));
    assert!(rows
        .iter()
        .any(|row| row.key == remote_key && row.status == AgentStatus::Working));

    let mut snapshot: Vec<_> = rows
        .iter()
        .map(crate::session_registry::agent_session_to_session_info)
        .collect();
    let local_history = snapshot
        .iter_mut()
        .find(|row| row.session_id.0.as_ref() == "shared")
        .unwrap();
    local_history.status = Some(AgentStatus::Historical);
    local_history.pane_session_id = None;
    let mut wsl = crate::session_registry::SessionInfo::new(
        agent_client_protocol::schema::v1::SessionId::new("wsl-only"),
        "/home/me".into(),
    );
    wsl.cli_source = Some(crate::agent_sessions::CliSource::Copilot);
    wsl.location = SessionLocation::Wsl {
        distro: "Ubuntu".into(),
    };
    snapshot.push(wsl);
    let target = crate::ssh_sessions::SshTarget::new("remote-host", None).unwrap();
    let mut ssh = crate::session_registry::SessionInfo::new(
        agent_client_protocol::schema::v1::SessionId::new("ssh-only"),
        "/home/me".into(),
    );
    ssh.cli_source = Some(crate::agent_sessions::CliSource::Copilot);
    ssh.location = SessionLocation::Ssh {
        target: target.clone(),
    };
    snapshot.push(ssh);
    app.current_tab_mut().agents_view.snapshot = Some(snapshot);

    let rows = app.agents_rows_for_tab(DEFAULT_TAB_ID);
    assert_eq!(
        rows.len(),
        2,
        "normal master snapshot includes host history and live tmux"
    );
    assert!(rows
        .iter()
        .any(|row| row.key == "shared" && row.status == AgentStatus::Historical));
    let remote = rows.iter().find(|row| row.key == remote_key).unwrap();
    assert_eq!(remote.status, AgentStatus::Working);
    assert_eq!(remote.pane_session_id.as_deref(), Some(PANE_A));
    app.activate_agent_session_routed(remote);
    let command = app.last_dispatched_command.as_ref().unwrap();
    assert_eq!(command.kind, DispatchedCommandKind::FocusPane);
    assert_eq!(command.session_id.as_deref(), Some(remote_key.as_str()));

    app.current_agent_source = crate::agent_source::AgentSource::Wsl {
        distro: "Ubuntu".into(),
    };
    let rows = app.agents_rows_for_tab(DEFAULT_TAB_ID);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].key, "wsl-only");
    app.current_tab_mut().agents_view.ssh_source = Some(crate::ssh_session_registry::Source {
        target,
        agent_id: "copilot".into(),
    });
    let rows = app.agents_rows_for_tab(DEFAULT_TAB_ID);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].key, "ssh-only");
    assert_eq!(
        app.agent_sessions.get(&"shared".into()).unwrap().location,
        SessionLocation::Host
    );
}
