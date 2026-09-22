use super::*;
use crate::agent_sessions::{CliSource, SessionLocation};
use crate::app::tests::test_app_with_master_rx;
use serde_json::json;

fn app_with_owner() -> (
    App,
    mpsc::UnboundedReceiver<crate::protocol::acp::client::MasterExtRequest>,
) {
    let (mut app, receiver) = test_app_with_master_rx();
    app.current_agent_id = "copilot".into();
    app.owner_tab_id = Some("owner-tab".into());
    app.tab_id = Some("owner-tab".into());
    app.window_id = Some("owner-window".into());
    app.show_welcome_hint = false;
    app.state = ConnectionState::Connected;
    app.tab_mut("owner-tab");
    (app, receiver)
}

fn open_native(app: &mut App, window: &str, tab: &str, metadata: serde_json::Value) {
    app.handle_event(AppEvent::WtEvent {
        method: "set_agent_state".into(),
        pane_id: String::new(),
        tab_id: Some(tab.into()),
        params: json!({
            "tab_id": tab,
            "window_id": window,
            "view": "sessions",
            "pane_open": true,
            "sessions_ssh": metadata,
        }),
    });
}

fn assert_ssh(app: &App, destination: &str, port: Option<u16>, cli: &str) {
    let source = app.current_tab().agents_view.ssh_source.as_ref().unwrap();
    assert_eq!(source.target.destination(), destination);
    assert_eq!(source.target.port(), port);
    assert_eq!(source.agent_id, cli);
    assert_eq!(
        app.current_location_filter(),
        SessionLocation::Ssh {
            target: SshTarget::new(destination, port).unwrap(),
        }
    );
}

#[test]
fn ssh_profile_startup_and_runtime_metadata_resolve_the_same_target() {
    for port in [None, Some(2222)] {
        let expected = SessionsProfile::Ssh(SshTarget::new("user@Alias", port).unwrap());
        assert_eq!(
            SessionsProfile::from_startup(Some("user@Alias"), port, None),
            expected
        );
        assert_eq!(
            SessionsProfile::from_wire(&json!({ "destination": "user@Alias", "port": port })),
            expected
        );
    }
    assert_eq!(
        SessionsProfile::from_startup(None, None, None),
        SessionsProfile::Agent
    );
    assert_eq!(
        SessionsProfile::from_wire(&serde_json::Value::Null),
        SessionsProfile::Agent
    );
}

#[test]
fn ssh_profile_invalid_metadata_is_not_a_host_source() {
    for value in [
        json!({}),
        json!("alias"),
        json!({ "destination": "-oProxyCommand=bad" }),
        json!({ "destination": "host", "port": 0 }),
        json!({ "destination": "host", "port": "22" }),
        json!({ "destination": "host", "command": "unexpected" }),
        json!({ "error": "unsupported SSH option" }),
        json!({ "error": 12 }),
    ] {
        assert!(
            matches!(
                SessionsProfile::from_wire(&value),
                SessionsProfile::Invalid(_)
            ),
            "{value}"
        );
    }
    assert!(matches!(
        SessionsProfile::from_startup(None, Some(22), None),
        SessionsProfile::Invalid(_)
    ));
    assert!(matches!(
        SessionsProfile::from_startup(Some("bad;target"), None, None),
        SessionsProfile::Invalid(_)
    ));
    assert_eq!(
        SessionsProfile::from_startup(None, None, Some("unsupported SSH option")),
        SessionsProfile::Invalid("unsupported SSH option".into())
    );
}

#[test]
fn ssh_profile_prewarmed_helper_opens_remote_history() {
    let _locale = crate::test_support::lock_locale();
    let (mut app, mut master) = app_with_owner();
    app.set_initial_sessions_ssh_profile(Some("wsl-ssh"), None, None);
    assert_ssh(&app, "wsl-ssh", None, "copilot");
    app.open_agents_view_for_tab("owner-tab".into());
    assert_eq!(app.current_tab().current_view, View::Agents);
    assert_ssh(&app, "wsl-ssh", None, "copilot");
    assert!(
        master.try_recv().is_err(),
        "SSH Profiles must not request host history"
    );
    assert_eq!(
        app.current_agent_source,
        crate::agent_source::AgentSource::Host
    );
}

#[test]
fn ssh_profile_native_button_selects_the_owning_profiles_destination() {
    let _locale = crate::test_support::lock_locale();
    let (mut app, mut master) = app_with_owner();
    open_native(
        &mut app,
        "owner-window",
        "owner-tab",
        json!({ "destination": "user@remote", "port": 2222 }),
    );
    assert_ssh(&app, "user@remote", Some(2222), "copilot");
    assert_eq!(app.current_tab().current_view, View::Agents);
    assert!(app.current_tab().pane_open);
    assert!(master.try_recv().is_err());
}

#[test]
fn ssh_profile_bare_sessions_uses_profile_not_windows() {
    let _locale = crate::test_support::lock_locale();
    let (mut app, mut master) = app_with_owner();
    app.set_initial_sessions_ssh_profile(Some("wsl-ssh"), Some(2222), None);
    app.current_tab_mut().replace_input("/sessions".into());
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_ssh(&app, "wsl-ssh", Some(2222), "copilot");
    assert_eq!(app.current_tab().current_view, View::Agents);
    assert!(master.try_recv().is_err());
}

#[test]
fn ssh_profile_tracks_the_selected_agent() {
    let _locale = crate::test_support::lock_locale();
    let (mut app, _) = app_with_owner();
    app.set_initial_sessions_ssh_profile(Some("wsl-ssh"), None, None);
    app.current_agent_id = "claude".into();
    app.open_agents_view_for_tab("owner-tab".into());
    assert_ssh(&app, "wsl-ssh", None, "claude");
    assert_eq!(app.current_cli_filter(), Some(CliSource::Claude));
    app.current_agent_id = "codex".into();
    app.open_agents_view_for_tab("owner-tab".into());
    assert_ssh(&app, "wsl-ssh", None, "codex");
}

#[test]
fn ssh_profile_rejects_removed_slash_source_arguments() {
    let _locale = crate::test_support::lock_locale();
    for target in [None, Some("profile-host")] {
        for command in [
            "/sessions ssh yuazha@127.0.0.1 --cli copilot",
            "/sessions ssh other -p 2222 --cli claude",
            "/sessions --cli claude",
        ] {
            let (mut app, mut master) = app_with_owner();
            app.set_initial_sessions_ssh_profile(target, None, None);
            let location = app.current_location_filter();
            app.current_tab_mut().replace_input(command.to_string());
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            assert_eq!(app.current_location_filter(), location);
            assert_eq!(app.current_tab().current_view, View::Chat);
            assert!(!app.current_tab().messages.is_empty());
            assert!(master.try_recv().is_err());
            assert_eq!(app.current_agent_id, "copilot");
        }
    }
}

#[test]
fn ssh_profile_keeps_windows_and_wsl_default_history_behavior() {
    let _locale = crate::test_support::lock_locale();
    for agent_source in [
        crate::agent_source::AgentSource::Host,
        crate::agent_source::AgentSource::Wsl {
            distro: "Ubuntu".into(),
        },
    ] {
        let (mut app, mut master) = app_with_owner();
        app.current_agent_source = agent_source.clone();
        app.set_initial_sessions_ssh_profile(None, None, None);
        open_native(
            &mut app,
            "owner-window",
            "owner-tab",
            serde_json::Value::Null,
        );
        assert_eq!(
            app.current_location_filter(),
            agent_source.session_location()
        );
        assert!(app.current_tab().agents_view.ssh_source.is_none());
        assert!(matches!(
            master.try_recv().unwrap(),
            crate::protocol::acp::client::MasterExtRequest::SessionsList { .. }
        ));
    }
}

#[test]
fn ssh_profile_agent_rebind_updates_an_open_view_without_using_host_history() {
    let _locale = crate::test_support::lock_locale();
    let (mut app, mut master) = app_with_owner();
    app.set_initial_sessions_ssh_profile(Some("wsl-ssh"), None, None);
    app.open_agents_view_for_tab("owner-tab".into());
    app.current_agent_id = "claude".into();
    app.reset_agent_scoped_state();
    assert_ssh(&app, "wsl-ssh", None, "claude");
    assert_eq!(app.current_tab().current_view, View::Agents);
    assert!(master.try_recv().is_err());
}

#[test]
fn ssh_profile_updates_cannot_cross_window_or_tab_boundaries() {
    let _locale = crate::test_support::lock_locale();
    let (mut app, mut master) = app_with_owner();
    app.set_initial_sessions_ssh_profile(Some("own"), None, None);
    for (window, tab) in [
        ("foreign-window", "owner-tab"),
        ("owner-window", "foreign-tab"),
    ] {
        open_native(
            &mut app,
            window,
            tab,
            json!({ "destination": "foreign", "port": null }),
        );
        assert_ssh(&app, "own", None, "copilot");
        app.handle_event(AppEvent::WtEvent {
            method: "tab_changed".into(),
            pane_id: String::new(),
            tab_id: Some(tab.into()),
            params: json!({
                "tab_id": tab, "window_id": window,
                "sessions_ssh": { "destination": "foreign", "port": null }
            }),
        });
        assert_ssh(&app, "own", None, "copilot");
    }
    assert!(master.try_recv().is_err());
}

#[test]
fn ssh_profile_change_retires_old_requests_and_null_clears_the_default() {
    let _locale = crate::test_support::lock_locale();
    let (mut app, mut master) = app_with_owner();
    app.set_initial_sessions_ssh_profile(Some("old"), None, None);
    let tab = app.current_tab_mut();
    tab.agents_view.snapshot = Some(Vec::new());
    tab.agents_view.refetch_in_flight = true;
    tab.agents_view.latest_request_id = Some(19);
    open_native(
        &mut app,
        "owner-window",
        "owner-tab",
        json!({ "destination": "new", "port": null }),
    );
    assert_ssh(&app, "new", None, "copilot");
    app.handle_ssh_sessions_loaded(
        "owner-tab",
        19,
        &SshTarget::new("old", None).unwrap(),
        "copilot",
        Err("stale old-host failure".into()),
    );
    assert_ne!(
        app.current_tab().agents_view.ssh_error.as_deref(),
        Some("stale old-host failure")
    );
    open_native(
        &mut app,
        "owner-window",
        "owner-tab",
        serde_json::Value::Null,
    );
    assert_eq!(app.current_location_filter(), SessionLocation::Host);
    assert!(app.current_tab().agents_view.ssh_source.is_none());
    assert!(app.current_tab().agents_view.ssh_error.is_none());
    assert!(matches!(
        master.try_recv().unwrap(),
        crate::protocol::acp::client::MasterExtRequest::SessionsList { .. }
    ));
}

#[test]
fn ssh_profile_invalid_source_displays_error_instead_of_local_history() {
    let _locale = crate::test_support::lock_locale();
    let (mut app, mut master) = app_with_owner();
    open_native(
        &mut app,
        "owner-window",
        "owner-tab",
        json!({ "error": "Use an OpenSSH Host alias for this profile." }),
    );
    assert!(app.current_tab().agents_view.is_ssh_source());
    assert!(app.current_tab().agents_view.ssh_source.is_none());
    assert!(app
        .current_tab()
        .agents_view
        .snapshot
        .as_ref()
        .unwrap()
        .is_empty());
    assert!(!app.current_tab().agents_view.refetch_in_flight);
    app.handle_key(KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE));
    assert!(master.try_recv().is_err());
    app.mode = AppMode::Setup;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 14)).unwrap();
    terminal
        .draw(|frame| crate::ui::render(frame, &mut app))
        .unwrap();
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(text.contains("SSH"));
    assert!(text.contains("Use an OpenSSH Host alias"));

    assert!(app.block_invalid_ssh_profile_refetch("owner-tab"));
    assert!(master.try_recv().is_err());
}

#[test]
fn ssh_profile_policy_blocks_remote_listing_without_falling_back_to_host() {
    let _locale = crate::test_support::lock_locale();
    let (mut app, mut master) = app_with_owner();
    app.host_agent_allowlist_present = true;
    app.allowed_agent_ids = vec!["claude".into()];
    app.set_initial_sessions_ssh_profile(Some("wsl-ssh"), None, None);
    app.open_agents_view_for_tab("owner-tab".into());
    assert_ssh(&app, "wsl-ssh", None, "copilot");
    assert!(app
        .current_tab()
        .agents_view
        .ssh_error
        .as_deref()
        .unwrap()
        .contains("blocked by policy"));
    assert!(master.try_recv().is_err());
}

#[test]
fn ssh_profile_tab_changed_refreshes_the_cached_default_without_rebinding_chat() {
    let _locale = crate::test_support::lock_locale();
    let (mut app, _) = app_with_owner();
    app.handle_event(AppEvent::WtEvent {
        method: "tab_changed".into(),
        pane_id: String::new(),
        tab_id: Some("owner-tab".into()),
        params: json!({
            "tab_id": "owner-tab", "window_id": "owner-window",
            "sessions_ssh": { "destination": "new-profile", "port": 2200 }
        }),
    });
    assert_ssh(&app, "new-profile", Some(2200), "copilot");
    assert_eq!(app.current_agent_id, "copilot");
    assert_eq!(
        app.current_agent_source,
        crate::agent_source::AgentSource::Host
    );
    assert_eq!(app.current_tab().current_view, View::Chat);
}
