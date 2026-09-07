//! Explicit SSH history browsing, isolated from the helper's chat agent and
//! the master's host/WSL registry.

use super::*;
use crate::agent_sessions::{AgentSession, CliSource, SessionLocation};
use crate::ssh_sessions::SshTarget;

const USAGE: &str = "/sessions [ssh <destination> [-p <port>] [--cli <agent>]]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SshSessionsSource {
    pub target: SshTarget,
    pub agent_id: String,
}

fn parse_source(arguments: &str, current_agent: &str) -> Result<Option<SshSessionsSource>> {
    let mut words = arguments.split_whitespace();
    let Some(kind) = words.next() else {
        return Ok(None);
    };
    anyhow::ensure!(kind == "ssh", "{USAGE}");
    let destination = words.next().ok_or_else(|| anyhow::anyhow!(USAGE))?;
    let mut port = None;
    let mut agent_id = None;
    while let Some(option) = words.next() {
        match option {
            "-p" | "--port" if port.is_none() => {
                port = Some(
                    words
                        .next()
                        .ok_or_else(|| anyhow::anyhow!(USAGE))?
                        .parse::<u16>()?,
                );
            }
            "--cli" if agent_id.is_none() => {
                agent_id = Some(
                    words
                        .next()
                        .ok_or_else(|| anyhow::anyhow!(USAGE))?
                        .to_ascii_lowercase(),
                );
            }
            _ => anyhow::bail!("{USAGE}"),
        }
    }
    let agent_id = agent_id.unwrap_or_else(|| current_agent.to_ascii_lowercase());
    anyhow::ensure!(
        crate::agent_registry::is_known_id(&agent_id),
        "SSH sessions require a built-in agent; select one with --cli."
    );
    Ok(Some(SshSessionsSource {
        target: SshTarget::new(destination, port)?,
        agent_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_sessions::{AgentStatus, SessionEvent, SessionOrigin};
    use crate::app::tests::{test_app, test_app_with_master_rx};
    use agent_client_protocol::schema::v1::{SessionId, SessionInfo};

    fn source(destination: &str) -> SshSessionsSource {
        SshSessionsSource {
            target: SshTarget::new(destination, None).unwrap(),
            agent_id: "copilot".to_string(),
        }
    }

    fn row(source: &SshSessionsSource, key: &str) -> AgentSession {
        let mut info = SessionInfo::new(SessionId::new(key), "/home/test/project");
        info.title = Some(format!("Session {key}"));
        crate::session_history::acp_session_to_agent_session(
            &info,
            SessionLocation::Ssh {
                target: source.target.clone(),
            },
            &CliSource::Copilot,
        )
    }

    fn prepare(app: &mut App, tab_id: &str, source: &SshSessionsSource, request_id: u64) {
        let tab = app.tab_mut(tab_id);
        tab.current_view = View::Agents;
        tab.agents_view.ssh_source = Some(source.clone());
        tab.agents_view.snapshot = Some(Vec::new());
        tab.agents_view.refetch_in_flight = true;
        tab.agents_view.latest_request_id = Some(request_id);
    }

    fn loaded(
        app: &mut App,
        tab_id: &str,
        source: &SshSessionsSource,
        request_id: u64,
        rows: Vec<AgentSession>,
    ) {
        app.handle_event(AppEvent::SshSessionsLoaded {
            tab_id: tab_id.to_string(),
            request_id,
            target: source.target.clone(),
            agent_id: source.agent_id.clone(),
            result: Ok(rows),
        });
    }

    #[test]
    fn ssh_source_parser_keeps_host_port_and_agent_separate() {
        let parsed = parse_source("ssh user@host -p 2222 --cli Claude", "copilot")
            .unwrap()
            .unwrap();
        assert_eq!(parsed.target.destination(), "user@host");
        assert_eq!(parsed.target.port(), Some(2222));
        assert_eq!(parsed.agent_id, "claude");
        assert_eq!(
            parse_source("ssh work", "copilot").unwrap(),
            Some(source("work"))
        );
        assert_eq!(parse_source("", "copilot").unwrap(), None);
    }

    #[test]
    fn ssh_source_parser_rejects_ambiguous_or_unsafe_arguments() {
        for arguments in [
            "ssh",
            "host",
            "ssh host command",
            "ssh host -p",
            "ssh host -p 0",
            "ssh host -p 65536",
            "ssh host -p 22 -p 23",
            "ssh host --cli unknown",
            "ssh host --cli copilot --cli claude",
            "ssh -oProxyCommand=whoami",
        ] {
            assert!(parse_source(arguments, "copilot").is_err(), "{arguments}");
        }
        assert!(parse_source("ssh host", "custom:private").is_err());
    }

    #[test]
    fn ssh_source_selection_does_not_rebind_the_chat_agent() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut master_rx) = test_app_with_master_rx();
        app.current_agent_id = "copilot".to_string();
        app.current_agent_source = crate::agent_source::AgentSource::Wsl {
            distro: "Ubuntu".to_string(),
        };
        assert!(app.select_sessions_source("ssh remote --cli claude"));
        assert_eq!(app.current_cli_filter(), Some(CliSource::Claude));
        assert_eq!(app.current_agent_id, "copilot");
        assert!(matches!(
            app.current_agent_source,
            crate::agent_source::AgentSource::Wsl { .. }
        ));
        assert!(master_rx.try_recv().is_err());

        assert!(app.select_sessions_source(""));
        assert_eq!(app.current_cli_filter(), Some(CliSource::Copilot));
        assert_eq!(
            app.current_location_filter(),
            SessionLocation::Wsl {
                distro: "Ubuntu".to_string()
            }
        );
    }

    #[test]
    fn ssh_source_selection_preserves_agent_policy() {
        let _locale = crate::test_support::lock_locale();
        let mut app = test_app();
        app.current_agent_id = "copilot".to_string();
        app.host_agent_allowlist_present = true;
        app.allowed_agent_ids = vec!["claude".to_string()];
        assert!(!app.select_sessions_source("ssh remote"));
        assert!(app.current_tab().agents_view.ssh_source.is_none());
        assert!(!app.current_tab().messages.is_empty());
    }

    #[test]
    fn ssh_slash_command_preserves_arguments_through_enter_dispatch() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut master_rx) = test_app_with_master_rx();
        app.current_agent_id = "copilot".to_string();
        app.state = ConnectionState::Connected;
        app.current_tab_mut()
            .replace_input("/sessions ssh test-host -p 2222 --cli claude".to_string());
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let source = app.current_tab().agents_view.ssh_source.as_ref().unwrap();
        assert_eq!(source.target.destination(), "test-host");
        assert_eq!(source.target.port(), Some(2222));
        assert_eq!(source.agent_id, "claude");
        assert_eq!(app.current_tab().current_view, View::Agents);
        assert!(master_rx.try_recv().is_err());
    }

    #[test]
    fn ssh_refresh_coalesces_without_sending_a_host_rpc() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut master_rx) = test_app_with_master_rx();
        prepare(&mut app, DEFAULT_TAB_ID, &source("remote"), 4);
        for mode in [AppMode::Chat, AppMode::Setup, AppMode::Auth] {
            app.mode = mode;
            app.handle_key(KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE));
            assert!(app.current_tab().agents_view.refetch_in_flight);
            assert!(app.current_tab().agents_view.pending_rescan);
            assert!(app.current_tab().agents_view.dirty);
            assert_eq!(app.current_tab().agents_view.latest_request_id, Some(4));
            assert!(master_rx.try_recv().is_err());
        }
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.current_tab().current_view, View::Chat);
        assert!(!app.current_tab().agents_view.refetch_in_flight);
        assert_eq!(app.mode, AppMode::Auth);
    }

    #[test]
    fn ssh_snapshots_never_enter_the_host_registry() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut master_rx) = test_app_with_master_rx();
        let source = source("remote");
        prepare(&mut app, DEFAULT_TAB_ID, &source, 7);
        loaded(
            &mut app,
            DEFAULT_TAB_ID,
            &source,
            7,
            vec![row(&source, "same-id")],
        );

        assert_eq!(app.agents_rows_for_tab(DEFAULT_TAB_ID).len(), 1);
        assert!(app.agent_sessions.get(&"same-id".to_string()).is_none());
        assert!(!app.current_tab().agents_view.refetch_in_flight);
        app.handle_event(AppEvent::SessionsChanged);
        assert!(master_rx.try_recv().is_err());
        assert!(!app.current_tab().agents_view.refetch_in_flight);
    }

    #[test]
    fn ssh_snapshots_ignore_colliding_host_request_ids() {
        let _locale = crate::test_support::lock_locale();
        let mut app = test_app();
        let source = source("remote");
        prepare(&mut app, DEFAULT_TAB_ID, &source, 7);
        let host_row = crate::session_registry::SessionInfo::new(
            SessionId::new("host-only"),
            r"C:\host".into(),
        );
        app.handle_event(AppEvent::AgentsSnapshotLoaded {
            request_id: 7,
            sessions: vec![host_row],
        });
        app.handle_event(AppEvent::AgentsSnapshotFailed { request_id: 7 });
        assert!(app.agents_rows_for_tab(DEFAULT_TAB_ID).is_empty());
        assert!(app.current_tab().agents_view.refetch_in_flight);
        loaded(
            &mut app,
            DEFAULT_TAB_ID,
            &source,
            7,
            vec![row(&source, "remote")],
        );
        assert_eq!(app.agents_rows_for_tab(DEFAULT_TAB_ID)[0].key, "remote");
    }

    #[test]
    fn ssh_snapshot_identity_includes_tab_target_agent_and_request() {
        let _locale = crate::test_support::lock_locale();
        let mut app = test_app();
        let first = source("first");
        let second = source("second");
        prepare(&mut app, DEFAULT_TAB_ID, &first, 9);
        prepare(&mut app, "other-tab", &second, 9);
        loaded(
            &mut app,
            DEFAULT_TAB_ID,
            &second,
            9,
            vec![row(&second, "wrong-host")],
        );
        loaded(
            &mut app,
            DEFAULT_TAB_ID,
            &first,
            8,
            vec![row(&first, "stale")],
        );
        let mut wrong_agent = first.clone();
        wrong_agent.agent_id = "claude".to_string();
        loaded(&mut app, DEFAULT_TAB_ID, &wrong_agent, 9, Vec::new());
        assert!(app.agents_rows_for_tab(DEFAULT_TAB_ID).is_empty());

        loaded(
            &mut app,
            "other-tab",
            &second,
            9,
            vec![row(&second, "other")],
        );
        assert!(app.agents_rows_for_tab(DEFAULT_TAB_ID).is_empty());
        assert_eq!(app.agents_rows_for_tab("other-tab")[0].key, "other");
        loaded(
            &mut app,
            DEFAULT_TAB_ID,
            &first,
            9,
            vec![row(&first, "own")],
        );
        assert_eq!(app.agents_rows_for_tab(DEFAULT_TAB_ID)[0].key, "own");
    }

    #[test]
    fn ssh_response_with_foreign_rows_is_an_error_not_an_empty_success() {
        let _locale = crate::test_support::lock_locale();
        let mut app = test_app();
        let own = source("own");
        let other = source("other");
        prepare(&mut app, DEFAULT_TAB_ID, &own, 1);
        loaded(
            &mut app,
            DEFAULT_TAB_ID,
            &own,
            1,
            vec![row(&other, "foreign")],
        );
        assert!(app.current_tab().agents_view.ssh_error.is_some());
        assert!(app.agents_rows_for_tab(DEFAULT_TAB_ID).is_empty());
        assert!(!app.current_tab().agents_view.refetch_in_flight);
    }

    #[test]
    fn ssh_refresh_failure_keeps_last_good_rows_and_surfaces_the_error() {
        let _locale = crate::test_support::lock_locale();
        let mut app = test_app();
        let source = source("remote");
        prepare(&mut app, DEFAULT_TAB_ID, &source, 1);
        loaded(
            &mut app,
            DEFAULT_TAB_ID,
            &source,
            1,
            vec![row(&source, "saved")],
        );
        app.current_tab_mut().agents_view.refetch_in_flight = true;
        app.current_tab_mut().agents_view.latest_request_id = Some(2);
        app.handle_ssh_sessions_loaded(
            DEFAULT_TAB_ID,
            2,
            &source.target,
            &source.agent_id,
            Err("connection refused".to_string()),
        );
        assert_eq!(app.agents_rows_for_tab(DEFAULT_TAB_ID)[0].key, "saved");
        assert_eq!(
            app.current_tab().agents_view.ssh_error.as_deref(),
            Some("connection refused")
        );
        assert!(!app.current_tab().agents_view.refetch_in_flight);
    }

    #[test]
    fn closing_ssh_view_rejects_late_results() {
        let _locale = crate::test_support::lock_locale();
        let mut app = test_app();
        let source = source("remote");
        prepare(&mut app, DEFAULT_TAB_ID, &source, 1);
        app.close_agents_view_for_tab(DEFAULT_TAB_ID);
        loaded(
            &mut app,
            DEFAULT_TAB_ID,
            &source,
            1,
            vec![row(&source, "late")],
        );
        assert!(app.current_tab().agents_view.snapshot.is_none());
        assert_eq!(app.current_tab().current_view, View::Chat);
    }

    #[test]
    fn ssh_resume_uses_remote_cli_without_mutating_same_id_host_session() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut master_rx) = test_app_with_master_rx();
        let source = source("remote");
        prepare(&mut app, DEFAULT_TAB_ID, &source, 1);
        let key = "same-id".to_string();
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: key.clone(),
            cli_source: CliSource::Copilot,
            pane_session_id: "host-pane".to_string(),
            cwd: r"C:\host".into(),
            title: "Host session".to_string(),
        });
        let mut remote = row(&source, &key);
        remote.origin = SessionOrigin::AgentPane;
        app.activate_agent_session_routed(&remote);

        let dispatched = app.last_dispatched_command.as_ref().unwrap();
        assert!(matches!(
            dispatched.kind,
            DispatchedCommandKind::NewTabResume
        ));
        assert_eq!(dispatched.argv[0], "new-tab");
        assert!(!dispatched.argv.iter().any(|arg| arg == "-d"));
        assert!(!dispatched.argv[2].contains("cmd /c"));
        assert!(dispatched.argv[2].contains("ssh"));
        assert!(dispatched.argv[2].contains("remote"));
        assert!(dispatched.argv[2].contains("/home/test/project"));
        assert!(master_rx.try_recv().is_err());
        let host = app.agent_sessions.get(&key).unwrap();
        assert_eq!(host.location, SessionLocation::Host);
        assert_eq!(host.status, AgentStatus::Idle);
        assert_eq!(host.pane_session_id.as_deref(), Some("host-pane"));
    }

    #[test]
    fn ssh_resume_rejects_a_session_from_another_target() {
        let _locale = crate::test_support::lock_locale();
        let mut app = test_app();
        prepare(&mut app, DEFAULT_TAB_ID, &source("own"), 1);
        app.activate_agent_session_routed(&row(&source("other"), "foreign"));
        assert!(app.last_dispatched_command.is_none());
        assert!(app.current_tab().agents_view.ssh_error.is_some());
    }

    #[test]
    fn ssh_location_survives_snapshot_serialization() {
        let source = source("remote");
        let info = crate::session_registry::agent_session_to_session_info(&row(&source, "session"));
        let json = serde_json::to_string(&info).unwrap();
        let restored: crate::session_registry::SessionInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.location, info.location);
        assert!(!restored.location.is_wsl());
        assert_eq!(restored.location.distro(), None);
    }

    #[test]
    fn ssh_view_displays_source_and_errors_even_without_rows() {
        let _locale = crate::test_support::lock_locale();
        let mut app = test_app();
        prepare(&mut app, DEFAULT_TAB_ID, &source("test-host"), 1);
        app.current_tab_mut().agents_view.refetch_in_flight = false;
        app.current_tab_mut().agents_view.ssh_error = Some("connection refused".to_string());
        let backend = ratatui::backend::TestBackend::new(72, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        for mode in [AppMode::Chat, AppMode::Setup, AppMode::Auth] {
            app.mode = mode;
            terminal
                .draw(|frame| crate::ui::render(frame, &mut app))
                .unwrap();
            let rendered: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(rendered.contains("SSH: test-host (copilot)"));
            assert!(rendered.contains("connection refused"));
        }
    }
}

impl App {
    fn ssh_sessions_agent_allowed(&self, agent_id: &str) -> bool {
        crate::agent_registry::is_known_id(agent_id)
            && (!self.host_agent_allowlist_present
                || self
                    .allowed_agent_ids
                    .iter()
                    .any(|allowed| allowed.eq_ignore_ascii_case(agent_id)))
    }

    pub(super) fn select_sessions_source(&mut self, arguments: &str) -> bool {
        let source = match parse_source(arguments, &self.current_agent_id) {
            Ok(source) => source,
            Err(error) => {
                tracing::warn!(target: "ssh_sessions", %error, "invalid session source");
                let message = format!("{}: {error}", t!("agents.status.error"));
                self.current_tab_mut()
                    .messages
                    .push(ChatMessage::warning(message));
                self.current_tab_mut().scroll_to_bottom();
                return false;
            }
        };
        if let Some(source) = &source {
            if !self.ssh_sessions_agent_allowed(&source.agent_id) {
                tracing::warn!(
                    target: "ssh_sessions",
                    agent_id = %source.agent_id,
                    "SSH session agent is blocked by policy"
                );
                let message =
                    t!("system.agent_unknown", agent = source.agent_id.as_str()).into_owned();
                self.current_tab_mut()
                    .messages
                    .push(ChatMessage::warning(message));
                return false;
            }
        }
        let tab_id = self.active_tab_key().to_string();
        self.cancel_ssh_sessions_fetch(&tab_id);
        let tab = self.tab_mut(&tab_id);
        tab.agents_view.ssh_source = source;
        tab.agents_view.ssh_error = None;
        tab.agents_view.snapshot = None;
        tab.agents_view.focused_sid = None;
        tab.agents_list_state.select(None);
        true
    }

    pub(super) fn sessions_filters_for_tab(
        &self,
        tab_id: &str,
    ) -> (Option<CliSource>, SessionLocation) {
        match self
            .tab_sessions
            .get(tab_id)
            .and_then(|tab| tab.agents_view.ssh_source.as_ref())
        {
            Some(source) => (
                CliSource::from_agent_id(&source.agent_id),
                SessionLocation::Ssh {
                    target: source.target.clone(),
                },
            ),
            None => (
                CliSource::from_agent_id(&self.current_agent_id),
                self.current_agent_source.session_location(),
            ),
        }
    }

    pub(super) fn cancel_ssh_sessions_fetch(&mut self, tab_id: &str) {
        if let Some(tab) = self.tab_sessions.get_mut(tab_id) {
            if let Some(fetch) = tab.agents_view.ssh_fetch.take() {
                fetch.abort();
            }
            tab.agents_view.latest_request_id = None;
            tab.agents_view.refetch_in_flight = false;
            tab.agents_view.rescan_in_flight = false;
            tab.agents_view.pending_rescan = false;
            tab.agents_view.dirty = false;
        }
    }

    pub(super) fn schedule_ssh_sessions_refetch(&mut self, tab_id: &str) {
        let (source, request_id) = {
            let tab = self.tab_mut(tab_id);
            let Some(source) = tab.agents_view.ssh_source.clone() else {
                return;
            };
            if tab.agents_view.snapshot.is_none() {
                return;
            }
            if tab.agents_view.refetch_in_flight {
                tab.agents_view.dirty = true;
                return;
            }
            tab.agents_view.next_request_id = tab.agents_view.next_request_id.wrapping_add(1);
            let request_id = tab.agents_view.next_request_id;
            tab.agents_view.latest_request_id = Some(request_id);
            tab.agents_view.refetch_in_flight = true;
            tab.agents_view.dirty = false;
            tab.agents_view.ssh_error = None;
            tab.agents_view.rescan_in_flight = std::mem::take(&mut tab.agents_view.pending_rescan);
            (source, request_id)
        };
        let unavailable = if !self.ssh_sessions_agent_allowed(&source.agent_id) {
            Some("SSH session agent is unavailable or blocked by policy.")
        } else if self.event_tx.is_none() || tokio::runtime::Handle::try_current().is_err() {
            Some("SSH session listing requires the helper event loop.")
        } else {
            None
        };
        if let Some(error) = unavailable {
            self.handle_ssh_sessions_loaded(
                tab_id,
                request_id,
                &source.target,
                &source.agent_id,
                Err(error.to_string()),
            );
            return;
        }
        let Some(event_tx) = self.event_tx.clone() else {
            return;
        };
        let owner_tab = tab_id.to_string();
        let task = tokio::task::spawn_local(async move {
            let result = crate::ssh_sessions::list_sessions(&source.target, &source.agent_id)
                .await
                .map_err(|error| format!("{error:#}"));
            let _ = event_tx.send(AppEvent::SshSessionsLoaded {
                tab_id: owner_tab,
                request_id,
                target: source.target,
                agent_id: source.agent_id,
                result,
            });
        });
        self.tab_mut(tab_id).agents_view.ssh_fetch = Some(task.abort_handle());
    }

    pub(super) fn handle_ssh_sessions_loaded(
        &mut self,
        tab_id: &str,
        request_id: u64,
        target: &SshTarget,
        agent_id: &str,
        result: std::result::Result<Vec<AgentSession>, String>,
    ) {
        let Some(tab) = self.tab_sessions.get_mut(tab_id) else {
            return;
        };
        if tab.agents_view.snapshot.is_none()
            || !tab.agents_view.refetch_in_flight
            || tab.agents_view.latest_request_id != Some(request_id)
            || !tab
                .agents_view
                .ssh_source
                .as_ref()
                .is_some_and(|source| &source.target == target && source.agent_id == agent_id)
        {
            return;
        }
        let old_selected = tab.agents_list_state.selected().unwrap_or(0);
        let location = SessionLocation::Ssh {
            target: target.clone(),
        };
        let cli = CliSource::from_agent_id(agent_id);
        let result = result.and_then(|rows| {
            if rows
                .iter()
                .any(|row| row.location != location || Some(&row.cli_source) != cli.as_ref())
            {
                Err("SSH session response did not match the requested source.".to_string())
            } else {
                Ok(rows)
            }
        });
        match result {
            Ok(rows) => {
                tab.agents_view.snapshot = Some(
                    rows.iter()
                        .map(crate::session_registry::agent_session_to_session_info)
                        .collect(),
                );
                tab.agents_view.ssh_error = None;
            }
            Err(error) => {
                tracing::warn!(target: "ssh_sessions", agent_id, %error, "SSH session listing failed");
                tab.agents_view.ssh_error = Some(error);
            }
        }
        tab.agents_view.ssh_fetch = None;
        tab.agents_view.refetch_in_flight = false;
        tab.agents_view.rescan_in_flight = false;
        let trailing = std::mem::take(&mut tab.agents_view.dirty);
        self.restore_agents_selection(tab_id, old_selected);
        if trailing {
            self.schedule_ssh_sessions_refetch(tab_id);
        }
    }

    pub(super) fn dispatch_ssh_session_resume(
        &mut self,
        session: &AgentSession,
        target: &SshTarget,
    ) {
        let result = (|| -> Result<Vec<String>> {
            let agent_id = known_cli_id(&session.cli_source)
                .ok_or_else(|| anyhow::anyhow!("Unknown SSH session agent."))?;
            anyhow::ensure!(
                self.ssh_sessions_agent_allowed(agent_id),
                "SSH session agent is unavailable or blocked by policy."
            );
            anyhow::ensure!(
                self.current_tab()
                    .agents_view
                    .ssh_source
                    .as_ref()
                    .is_some_and(|source| &source.target == target && source.agent_id == agent_id),
                "SSH session does not belong to the selected source."
            );
            let commandline = crate::ssh_sessions::resume_commandline(
                target,
                agent_id,
                &session.key,
                &session.cwd.to_string_lossy(),
            )?;
            let mut argv = vec!["new-tab".to_string(), "-c".to_string(), commandline];
            if !session.title.is_empty() {
                argv.extend(["--title".to_string(), session.title.clone()]);
            }
            Ok(argv)
        })();
        match result {
            Ok(argv) => {
                // Remote history is not a liveness report. In particular, never
                // publish its raw session id into the host registry.
                #[cfg(not(test))]
                crate::shell::wt_channel::spawn_wtcli_split_then_focus_with_callback(&argv, None);
                #[cfg(test)]
                {
                    self.last_dispatched_command = Some(DispatchedCommand {
                        kind: DispatchedCommandKind::NewTabResume,
                        session_id: None,
                        argv,
                    });
                }
            }
            Err(error) => {
                tracing::warn!(target: "ssh_sessions", %error, "SSH session resume failed");
                self.current_tab_mut().agents_view.ssh_error = Some(format!("{error:#}"));
            }
        }
    }
}
