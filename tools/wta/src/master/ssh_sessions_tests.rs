use super::super::{tests::make_state, HelperHandler, HelperId};
use super::*;
use crate::session_registry::{SessionInfo, WtaExtNotification, WtaExtRequest};
use crate::ssh_session_registry::{CHANGED_METHOD, METHOD};
use std::path::PathBuf;
use std::sync::OnceLock;
use tokio::sync::{mpsc, oneshot, OnceCell};

struct NativeCall {
    method: String,
    params: serde_json::Value,
    reply: oneshot::Sender<anyhow::Result<serde_json::Value>>,
}

struct NativeChannel(mpsc::UnboundedSender<NativeCall>);

#[async_trait::async_trait]
impl crate::shell::wt_channel::WtChannel for NativeChannel {
    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let (reply, response) = oneshot::channel();
        self.0
            .send(NativeCall {
                method: method.to_owned(),
                params,
                reply,
            })
            .map_err(|_| anyhow!("native test receiver closed"))?;
        response.await.context("native test response cancelled")?
    }

    fn is_available(&self) -> bool {
        true
    }
}

fn harness() -> (Arc<MasterStateInner>, mpsc::UnboundedReceiver<NativeCall>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut state = make_state();
    Arc::get_mut(&mut state).unwrap().wt = Some(Arc::new(NativeChannel(tx)));
    (state, rx)
}

fn caller(state: &Arc<MasterStateInner>, id: u64) -> HelperHandler {
    HelperHandler {
        helper_id: HelperId(id),
        agent: Arc::new(OnceCell::new()),
        state: Arc::clone(state),
        replacement_gate: Arc::new(Mutex::new(())),
        notif_tx: mpsc::channel(super::super::NOTIF_CHANNEL_CAPACITY).0,
        agent_side_slot: Arc::new(OnceLock::new()),
    }
}

fn source(destination: &str, port: Option<u16>, agent_id: &str) -> Source {
    Source {
        target: crate::ssh_sessions::SshTarget::new(destination, port).unwrap(),
        agent_id: agent_id.to_owned(),
    }
}

fn history(source: &Source, id: &str, title: &str) -> AgentSession {
    let now = std::time::UNIX_EPOCH + std::time::Duration::from_secs(100);
    AgentSession {
        key: id.to_owned(),
        cli_source: CliSource::parse(Some(&source.agent_id)),
        pane_session_id: None,
        window_id: None,
        tab_id: None,
        title: title.to_owned(),
        cwd: PathBuf::from("/home/user/project 'quoted'"),
        started_at: now,
        last_activity_at: now,
        status: AgentStatus::Historical,
        last_error: None,
        current_tool: None,
        attention_reason: None,
        log_path: None,
        origin: SessionOrigin::Unknown,
        location: SessionLocation::Ssh {
            target: source.target.clone(),
        },
    }
}

async fn seed(state: &MasterStateInner, source: &Source, ids: &[&str]) {
    let scoped = state.ssh_sessions.source(source).await;
    merge_history(
        state,
        source,
        &scoped,
        ids.iter()
            .map(|id| history(source, id, "Original title"))
            .collect(),
    )
    .await;
}

async fn call(handler: &HelperHandler, request: Request) -> acp::Result<Snapshot> {
    let params = serde_json::value::to_raw_value(&request).unwrap();
    let response = handler
        .ext_method(acp::schema::v1::ExtRequest::new(METHOD, params.into()))
        .await?;
    Ok(serde_json::from_str(response.0.get()).unwrap())
}

fn activation(
    handler: &HelperHandler,
    source: &Source,
    id: &str,
) -> tokio::task::JoinHandle<acp::Result<Snapshot>> {
    let handler = handler.clone();
    let request = Request::Activate {
        source: source.clone(),
        session_id: id.to_owned(),
    };
    tokio::task::spawn_local(async move { call(&handler, request).await })
}

async fn cached(handler: &HelperHandler, source: &Source) -> Snapshot {
    call(
        handler,
        Request::List {
            source: source.clone(),
            refresh_history: false,
        },
    )
    .await
    .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn ssh_source_snapshot_is_read_only_even_before_first_history_list() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (state, mut native) = harness();
            let handler = caller(&state, 1);
            let source = source("must-not-be-probed.invalid", None, "copilot");
            let first = call(
                &handler,
                Request::Snapshot {
                    source: source.clone(),
                },
            )
            .await
            .unwrap();
            assert!(first.sessions.is_empty());
            assert_eq!(first.revision, 0);
            let scoped = state.ssh_sessions.source(&source).await;
            assert!(!scoped.operations.lock().await.history_loaded);
            assert!(native.try_recv().is_err());

            let pane = uuid::Uuid::new_v4();
            hook_event(
                &state,
                &source.target,
                crate::ssh_hook_protocol::RouteId::new(),
                &pane.to_string(),
                hook("copilot", "agent.prompt.submit", "live"),
            )
            .await
            .unwrap();
            let second = call(
                &handler,
                Request::Snapshot {
                    source: source.clone(),
                },
            )
            .await
            .unwrap();
            assert_eq!(second.source, source);
            assert_eq!(second.sessions.len(), 1);
            assert_eq!(second.sessions[0].status, Some(AgentStatus::Working));
            assert_eq!(
                second.sessions[0].pane_session_id.as_deref(),
                Some(pane.to_string().as_str())
            );
            assert!(!scoped.operations.lock().await.history_loaded);
            assert!(native.try_recv().is_err());
        })
        .await;
}

async fn created(rx: &mut mpsc::UnboundedReceiver<NativeCall>, pane: uuid::Uuid) {
    let call = rx.recv().await.unwrap();
    assert_eq!(call.method, "create_tab");
    assert!(
        call.params.get("cwd").is_none(),
        "POSIX cwd is never a native cwd"
    );
    assert_eq!(call.params["title"], "Original title");
    call.reply
        .send(Ok(serde_json::json!({ "session_id": pane.to_string() })))
        .unwrap();
    focused(rx, pane).await;
}

async fn focused(rx: &mut mpsc::UnboundedReceiver<NativeCall>, pane: uuid::Uuid) {
    let call = rx.recv().await.unwrap();
    assert_eq!(call.method, "focus_pane");
    assert_eq!(call.params["session_id"], pane.to_string());
    call.reply.send(Ok(serde_json::json!({}))).unwrap();
}

async fn close(state: &Arc<MasterStateInner>, pane: uuid::Uuid, native_state: &str) {
    super::super::handle_master_wt_event(
        state,
        serde_json::json!({
            "method": "connection_state",
            "params": {
                "pane_id": format!("{{{}}}", pane.to_string().to_uppercase()),
                "state": native_state
            }
        }),
    )
    .await;
}

fn hook(cli: &str, event: &str, sid: &str) -> crate::ssh_hook_protocol::HookEvent {
    crate::ssh_hook_protocol::HookEvent {
        cli_source: cli.into(),
        event: event.into(),
        raw_session_id: sid.into(),
        payload: serde_json::json!({"cwd": "/home/alice/project", "message": "approve", "tool_name": "edit"}),
    }
}

async fn native_tmux_hook(
    state: &Arc<MasterStateInner>,
    source: &Source,
    pane: uuid::Uuid,
    event: &str,
    sid: &str,
) {
    super::super::handle_master_wt_event(
        state,
        serde_json::json!({
            "method": "agent_event",
            "params": crate::tmux_hooks::tests::ssh_hook(
                event, &pane.to_string(), &source.agent_id, sid, &source.target,
            )
        }),
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn ssh_native_tmux_merges_history_and_supports_shared_focus_titles_close_and_resume() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut state, mut native) = harness();
            Arc::get_mut(&mut state).unwrap().ssh_hooks =
                super::super::ssh_hooks::Service::new(true);
            let source = source("wsl-ubuntu", Some(2222), "copilot");
            seed(&state, &source, &["same-id"]).await;
            let scoped = state.ssh_sessions.source(&source).await;
            let a = caller(&state, 1);
            let b = caller(&state, 2);
            let pane = uuid::Uuid::new_v4();
            let (tx, mut notifications) = mpsc::unbounded_channel();
            state
                .helper_ext_subscribers
                .lock()
                .await
                .insert(HelperId(2), tx);

            for (event, status) in [
                ("agent.session.start", AgentStatus::Idle),
                ("agent.prompt.submit", AgentStatus::Working),
                ("agent.session.start", AgentStatus::Working),
                ("agent.notification", AgentStatus::Attention),
                ("agent.stop", AgentStatus::Idle),
            ] {
                native_tmux_hook(&state, &source, pane, event, "same-id").await;
                let snapshot = cached(&b, &source).await;
                assert_eq!(snapshot.sessions.len(), 1);
                let row = &snapshot.sessions[0];
                assert_eq!(row.session_id.0.as_ref(), "same-id");
                assert_eq!(row.title.as_deref(), Some("Original title"));
                assert_eq!(row.status, Some(status));
                assert_eq!(row.pane_session_id, Some(pane.to_string()));
                assert_eq!(
                    row.location,
                    SessionLocation::Ssh {
                        target: source.target.clone()
                    }
                );
                assert_eq!(
                    crate::session_registry::parse_ext_notification(
                        &notifications.try_recv().unwrap()
                    ),
                    WtaExtNotification::SshSessionsChanged(source.clone())
                );
                assert!(notifications.try_recv().is_err());
                assert!(
                    state.registry.snapshot().await.is_empty(),
                    "no duplicate Host/tmux row"
                );
            }
            let focus = activation(&a, &source, "same-id");
            focused(&mut native, pane).await;
            focus.await.unwrap().unwrap();
            merge_history(
                &state,
                &source,
                &scoped,
                vec![history(&source, "same-id", "Updated title")],
            )
            .await;
            let refreshed = cached(&b, &source).await;
            assert_eq!(refreshed.sessions.len(), 1);
            assert_eq!(
                refreshed.sessions[0].title.as_deref(),
                Some("Updated title")
            );
            assert_eq!(refreshed.sessions[0].status, Some(AgentStatus::Idle));
            assert_eq!(
                refreshed.sessions[0].pane_session_id,
                Some(pane.to_string())
            );

            close(&state, pane, "closed").await;
            assert_eq!(
                cached(&b, &source).await.sessions[0].status,
                Some(AgentStatus::Ended)
            );
            let resume = activation(&a, &source, "same-id");
            let create = native.recv().await.unwrap();
            assert_eq!(create.method, "create_tab");
            let argv = crate::coordinator::split_windows_commandline(
                create.params["commandline"].as_str().unwrap(),
            );
            assert_eq!(argv[1], "ssh");
            assert!(argv
                .windows(2)
                .any(|pair| pair == ["--destination", "wsl-ubuntu"]));
            assert!(argv.windows(2).any(|pair| pair == ["--port", "2222"]));
            let script = argv
                .windows(2)
                .find(|pair| pair[0] == "--remote-command")
                .unwrap();
            assert_eq!(
                script[1],
                crate::ssh_sessions::resume_script("copilot", "same-id", "/home/me/project")
                    .unwrap()
            );
            let resumed_pane = uuid::Uuid::new_v4();
            create
                .reply
                .send(Ok(
                    serde_json::json!({"session_id": resumed_pane.to_string()}),
                ))
                .unwrap();
            focused(&mut native, resumed_pane).await;
            resume.await.unwrap().unwrap();
            hook_event(
                &state,
                &source.target,
                RouteId::new(),
                &resumed_pane.to_string(),
                hook("copilot", "agent.prompt.submit", "same-id"),
            )
            .await
            .unwrap();
            native_tmux_hook(&state, &source, pane, "agent.stop", "same-id").await;
            native_tmux_hook(&state, &source, pane, "agent.session.end", "same-id").await;
            let resumed = cached(&b, &source).await;
            assert_eq!(resumed.sessions.len(), 1);
            assert_eq!(resumed.sessions[0].status, Some(AgentStatus::Working));
            assert_eq!(
                resumed.sessions[0].pane_session_id,
                Some(resumed_pane.to_string())
            );
            assert!(native.try_recv().is_err());
        })
        .await;
}

#[tokio::test]
async fn ssh_native_tmux_and_managed_hooks_share_a_source_without_crossing_pane_lifetimes() {
    let mut state = make_state();
    Arc::get_mut(&mut state).unwrap().ssh_hooks = super::super::ssh_hooks::Service::new(true);
    let source = source("wsl-ubuntu", None, "copilot");
    let scoped = state.ssh_sessions.source(&source).await;
    let tmux_pane = uuid::Uuid::new_v4();
    let ssh_pane = uuid::Uuid::new_v4();
    hook_event(
        &state,
        &source.target,
        RouteId::new(),
        &ssh_pane.to_string(),
        hook("copilot", "agent.prompt.submit", "ssh-session"),
    )
    .await
    .unwrap();
    native_tmux_hook(
        &state,
        &source,
        tmux_pane,
        "agent.prompt.submit",
        "tmux-session",
    )
    .await;
    native_tmux_hook(&state, &source, tmux_pane, "agent.notification", "").await;
    merge_history(
        &state,
        &source,
        &scoped,
        vec![
            history(&source, "ssh-session", "SSH conversation"),
            history(&source, "tmux-session", "Tmux conversation"),
        ],
    )
    .await;
    let snapshot = snapshot(&state, &source, &scoped).await;
    assert_eq!(snapshot.sessions.len(), 2);
    let tmux = snapshot
        .sessions
        .iter()
        .find(|row| row.session_id.0.as_ref() == "tmux-session")
        .unwrap();
    assert_eq!(tmux.status, Some(AgentStatus::Attention));
    assert_eq!(tmux.title.as_deref(), Some("Tmux conversation"));
    close(&state, tmux_pane, "closed").await;
    assert_eq!(
        scoped
            .registry
            .lookup(&"tmux-session".into())
            .await
            .unwrap()
            .status,
        Some(AgentStatus::Ended)
    );
    assert_eq!(
        scoped
            .registry
            .lookup(&"ssh-session".into())
            .await
            .unwrap()
            .status,
        Some(AgentStatus::Working)
    );
    assert!(state.registry.snapshot().await.is_empty());
}

#[tokio::test]
async fn ssh_native_tmux_respects_source_isolation_policy_and_disabled_tracking() {
    let mut state = make_state();
    let first = source("alice@ubuntu", None, "copilot");
    let pane = uuid::Uuid::new_v4();
    native_tmux_hook(&state, &first, pane, "agent.session.start", "same").await;
    assert!(state.ssh_sessions.sources.lock().await.is_empty());
    Arc::get_mut(&mut state).unwrap().ssh_hooks = super::super::ssh_hooks::Service::new(true);
    for source in [
        first.clone(),
        source("bob@ubuntu", None, "copilot"),
        source("alice@ubuntu", Some(2222), "copilot"),
        source("alice@other", None, "copilot"),
        source("alice@ubuntu", None, "claude"),
    ] {
        native_tmux_hook(&state, &source, pane, "agent.session.start", "same").await;
        let scoped = state.ssh_sessions.source(&source).await;
        let rows = scoped.registry.snapshot().await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, Some(AgentStatus::Idle));
    }
    native_tmux_hook(&state, &first, pane, "agent.prompt.submit", "same").await;
    native_tmux_hook(&state, &first, pane, "agent.session.start", "sidekick-test").await;
    let unrelated = source("alice@other", None, "copilot");
    assert_eq!(
        state
            .ssh_sessions
            .source(&unrelated)
            .await
            .registry
            .snapshot()
            .await[0]
            .status,
        Some(AgentStatus::Idle),
    );
    Arc::get_mut(&mut state).unwrap().allowed_agent_ids = Some(HashSet::from(["claude".into()]));
    native_tmux_hook(&state, &first, pane, "agent.stop", "same").await;
    let scoped = state.ssh_sessions.source(&first).await;
    let rows = scoped.registry.snapshot().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, Some(AgentStatus::Working));
    assert!(state.registry.snapshot().await.is_empty());
}

#[tokio::test]
async fn ssh_hook_sessions_use_existing_source_registry_and_preserve_connection_collisions() {
    let state = make_state();
    let first = source("alice@remote", Some(2222), "copilot");
    let route_a = crate::ssh_hook_protocol::RouteId::new();
    let route_b = crate::ssh_hook_protocol::RouteId::new();
    let pane_a = uuid::Uuid::new_v4().to_string();
    let pane_b = uuid::Uuid::new_v4().to_string();
    hook_event(
        &state,
        &first.target,
        route_a,
        &pane_a,
        hook("copilot", "agent.session.start", "same"),
    )
    .await
    .unwrap();
    hook_event(
        &state,
        &first.target,
        route_b,
        &pane_b,
        hook("copilot", "agent.session.start", "same"),
    )
    .await
    .unwrap();
    let scoped = state.ssh_sessions.source(&first).await;
    let rows = scoped.registry.snapshot().await;
    assert_eq!(rows.len(), 2);
    assert_ne!(rows[0].session_id, rows[1].session_id);
    for other in [
        source("bob@remote", Some(2222), "copilot"),
        source("alice@remote", Some(22), "copilot"),
        source("alice@other", Some(2222), "copilot"),
        source("alice@remote", Some(2222), "claude"),
    ] {
        hook_event(
            &state,
            &other.target,
            route_a,
            &pane_a,
            hook(&other.agent_id, "agent.session.start", "same"),
        )
        .await
        .unwrap();
        assert_eq!(
            state
                .ssh_sessions
                .source(&other)
                .await
                .registry
                .snapshot()
                .await
                .len(),
            1
        );
    }
    for (event, status) in [
        ("agent.prompt.submit", AgentStatus::Working),
        ("agent.notification", AgentStatus::Attention),
        ("agent.stop", AgentStatus::Idle),
        ("agent.error", AgentStatus::Error),
        ("agent.session.end", AgentStatus::Ended),
    ] {
        hook_event(
            &state,
            &first.target,
            route_a,
            &pane_a,
            hook("copilot", event, "same"),
        )
        .await
        .unwrap();
        let row = scoped
            .registry
            .lookup(&acp::schema::v1::SessionId::new("same"))
            .await
            .unwrap();
        assert_eq!(row.status, Some(status));
        assert_eq!(
            row.location,
            SessionLocation::Ssh {
                target: first.target.clone()
            }
        );
    }
    let untouched = scoped
        .registry
        .snapshot()
        .await
        .into_iter()
        .find(|row| row.pane_session_id.as_deref() == Some(pane_b.as_str()))
        .unwrap();
    assert_eq!(untouched.status, Some(AgentStatus::Idle));
    assert!(state.registry.snapshot().await.is_empty());
}

#[tokio::test]
async fn ssh_hook_cached_route_cannot_steal_or_end_a_resumed_panes_binding() {
    let state = make_state();
    let source = source("alice@remote", None, "copilot");
    let route_a = crate::ssh_hook_protocol::RouteId::new();
    let route_b = crate::ssh_hook_protocol::RouteId::new();
    let pane_a = uuid::Uuid::new_v4().to_string();
    let pane_b = uuid::Uuid::new_v4().to_string();
    let sid = acp::schema::v1::SessionId::new("same");
    for event in ["agent.session.start", "agent.session.end"] {
        hook_event(
            &state, &source.target, route_a, &pane_a,
            hook("copilot", event, "same"),
        )
        .await
        .unwrap();
    }
    let scoped = state.ssh_sessions.source(&source).await;
    scoped.registry.apply_event(SessionEvent::ResumeDispatched {
        key: "same".into(),
    }).await;
    scoped.registry.apply_event(SessionEvent::ResumePaneAssigned {
        key: "same".into(),
        pane_session_id: pane_b.clone(),
    }).await;
    for recorded_route_b in [false, true] {
        if recorded_route_b {
            hook_event(
                &state, &source.target, route_b, &pane_b,
                hook("copilot", "agent.prompt.submit", "same"),
            )
            .await
            .unwrap();
        }
        for event in ["agent.stop", "agent.notification", "agent.session.end", "agent.error"] {
            let before = scoped.registry.lookup(&sid).await.unwrap();
            hook_event(
                &state, &source.target, route_a, &pane_a,
                hook("copilot", event, "same"),
            )
            .await
            .unwrap();
            assert_eq!(scoped.registry.lookup(&sid).await.unwrap(), before);
        }
    }
    hook_event(
        &state, &source.target, route_a, &pane_a,
        hook("copilot", "agent.session.start", "same"),
    )
    .await
    .unwrap();
    let original = scoped.registry.lookup(&sid).await.unwrap();
    assert_eq!(original.pane_session_id.as_deref(), Some(pane_b.as_str()));
    assert_eq!(original.status, Some(AgentStatus::Working));
    let rows = scoped.registry.snapshot().await;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|row|
        row.session_id != sid && row.pane_session_id.as_deref() == Some(pane_a.as_str())
    ));
}

#[tokio::test]
async fn ssh_hook_sessionless_notifications_are_pane_bound_and_history_cannot_overwrite_activity() {
    let state = make_state();
    let source = source("alice@remote", None, "copilot");
    let route = crate::ssh_hook_protocol::RouteId::new();
    let pane = uuid::Uuid::new_v4().to_string();
    hook_event(
        &state,
        &source.target,
        route,
        &pane,
        hook("copilot", "agent.prompt.submit", "live"),
    )
    .await
    .unwrap();
    let scoped = state.ssh_sessions.source(&source).await;
    hook_event(
        &state,
        &source.target,
        route,
        &uuid::Uuid::new_v4().to_string(),
        hook("copilot", "agent.notification", ""),
    )
    .await
    .unwrap();
    hook_event(
        &state,
        &source.target,
        route,
        &pane,
        hook("copilot", "agent.notification", "sidekick-memory"),
    )
    .await
    .unwrap();
    assert_eq!(scoped.registry.snapshot().await.len(), 1);
    merge_history(
        &state,
        &source,
        &scoped,
        vec![history(&source, "live", "New history title")],
    )
    .await;
    let live = scoped
        .registry
        .lookup(&acp::schema::v1::SessionId::new("live"))
        .await
        .unwrap();
    assert_eq!(live.status, Some(AgentStatus::Working));
    assert_eq!(live.pane_session_id.as_deref(), Some(pane.as_str()));
    hook_event(
        &state,
        &source.target,
        route,
        &pane,
        hook("copilot", "agent.notification", ""),
    )
    .await
    .unwrap();
    assert_eq!(
        scoped
            .registry
            .lookup(&live.session_id)
            .await
            .unwrap()
            .status,
        Some(AgentStatus::Attention)
    );
    pane_closed(&state, &pane).await;
    hook_event(
        &state,
        &source.target,
        route,
        &pane,
        hook("copilot", "agent.notification", ""),
    )
    .await
    .unwrap();
    assert_eq!(
        scoped
            .registry
            .lookup(&live.session_id)
            .await
            .unwrap()
            .status,
        Some(AgentStatus::Ended)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn ssh_hook_managed_resume_preserves_reserved_sid_and_pre_ack_hook_activity() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut state, mut native) = harness();
            Arc::get_mut(&mut state).unwrap().ssh_hooks =
                super::super::ssh_hooks::Service::new(true);
            let source = source("alice@remote", Some(2222), "copilot");
            seed(&state, &source, &["requested"]).await;
            let caller = caller(&state, 1);
            let task = activation(&caller, &source, "requested");
            let create = native.recv().await.unwrap();
            assert_eq!(create.method, "create_tab");
            let args = crate::coordinator::split_windows_commandline(
                create.params["commandline"].as_str().unwrap(),
            );
            assert_eq!(args[1], "ssh");
            assert!(args.iter().any(|arg| arg == "--remote-command"));
            let pane = uuid::Uuid::new_v4();
            let route = crate::ssh_hook_protocol::RouteId::new();
            hook_event(
                &state,
                &source.target,
                route,
                &pane.to_string(),
                hook("copilot", "agent.session.start", "bootstrap"),
            )
            .await
            .unwrap();
            hook_event(
                &state,
                &source.target,
                route,
                &pane.to_string(),
                hook("copilot", "agent.prompt.submit", "requested"),
            )
            .await
            .unwrap();
            hook_event(
                &state,
                &source.target,
                route,
                &pane.to_string(),
                hook("copilot", "agent.session.start", "requested"),
            )
            .await
            .unwrap();
            create
                .reply
                .send(Ok(serde_json::json!({"session_id": pane.to_string()})))
                .unwrap();
            focused(&mut native, pane).await;
            let snapshot = task.await.unwrap().unwrap();
            assert_eq!(snapshot.sessions.len(), 1);
            let row = &snapshot.sessions[0];
            assert_eq!(row.session_id.0.as_ref(), "requested");
            assert_eq!(row.status, Some(AgentStatus::Working));
            assert_eq!(
                row.pane_session_id.as_deref(),
                Some(pane.to_string().as_str())
            );
            let focused_task = activation(&caller, &source, "requested");
            focused(&mut native, pane).await;
            assert_eq!(
                focused_task.await.unwrap().unwrap().sessions[0].status,
                Some(AgentStatus::Working)
            );
            assert!(native.try_recv().is_err());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn ssh_resume_in_one_helper_is_idle_and_focusable_in_another() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (state, mut native) = harness();
            let source = source("alice@ubuntu", None, "copilot");
            seed(&state, &source, &["same-id"]).await;
            let a = caller(&state, 1);
            let b = caller(&state, 2);
            let pane = uuid::Uuid::new_v4();
            let resume = activation(&a, &source, "same-id");
            let request = native.recv().await.unwrap();
            assert_eq!(request.method, "create_tab");
            assert_eq!(
                request.params["commandline"],
                crate::ssh_sessions::resume_commandline(
                    &source.target,
                    &source.agent_id,
                    "same-id",
                    "/home/user/project 'quoted'"
                )
                .unwrap()
            );
            assert!(request.params.get("cwd").is_none());
            request
                .reply
                .send(Ok(serde_json::json!({ "session_id": pane.to_string() })))
                .unwrap();
            focused(&mut native, pane).await;
            let resumed = resume.await.unwrap().unwrap();
            let peer = cached(&b, &source).await;
            assert_eq!(resumed.epoch, peer.epoch);
            assert_eq!(resumed.revision, peer.revision);
            assert_eq!(peer.sessions[0].status, Some(AgentStatus::Idle));
            assert_eq!(peer.sessions[0].pane_session_id, Some(pane.to_string()));
            assert_eq!(peer.sessions[0].origin, Some(SessionOrigin::Unknown));
            assert_eq!(peer.sessions[0].cli_source, Some(CliSource::Copilot));
            assert_eq!(
                peer.sessions[0].location,
                SessionLocation::Ssh {
                    target: source.target.clone()
                }
            );
            let focus = activation(&b, &source, "same-id");
            let request = native.recv().await.unwrap();
            assert_eq!(request.method, "focus_pane");
            assert_eq!(request.params["session_id"], pane.to_string());
            request.reply.send(Ok(serde_json::json!({}))).unwrap();
            assert_eq!(focus.await.unwrap().unwrap().revision, peer.revision);
            assert!(native.try_recv().is_err());
            assert!(state.registry.snapshot().await.is_empty());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn ssh_simultaneous_helpers_share_one_pending_create() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (state, mut native) = harness();
            let source = source("ubuntu", None, "copilot");
            seed(&state, &source, &["same-id"]).await;
            let a = caller(&state, 1);
            let b = caller(&state, 2);
            let resume = activation(&a, &source, "same-id");
            let request = native.recv().await.unwrap();
            assert_eq!(request.method, "create_tab");
            let second = call(
                &b,
                Request::Activate {
                    source: source.clone(),
                    session_id: "same-id".to_owned(),
                },
            )
            .await
            .unwrap();
            assert_eq!(second.sessions[0].status, Some(AgentStatus::Idle));
            assert_eq!(second.sessions[0].pane_session_id, None);
            assert!(native.try_recv().is_err());
            let pane = uuid::Uuid::new_v4();
            request
                .reply
                .send(Ok(serde_json::json!({ "session_id": pane.to_string() })))
                .unwrap();
            focused(&mut native, pane).await;
            resume.await.unwrap().unwrap();
            assert_eq!(
                cached(&b, &source).await.sessions[0].pane_session_id,
                Some(pane.to_string())
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn ssh_native_close_and_failure_end_all_views_and_notify_only_affected_source() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for native_state in ["closed", "failed"] {
                let (state, mut native) = harness();
                let source = source("ubuntu", None, "copilot");
                let unrelated = Source {
                    agent_id: "claude".to_owned(),
                    ..source.clone()
                };
                seed(&state, &source, &["same-id"]).await;
                seed(&state, &unrelated, &["same-id"]).await;
                let a = caller(&state, 1);
                let b = caller(&state, 2);
                let pane = uuid::Uuid::new_v4();
                let resume = activation(&a, &source, "same-id");
                created(&mut native, pane).await;
                let before = resume.await.unwrap().unwrap();
                let (tx, mut rx) = mpsc::unbounded_channel();
                state
                    .helper_ext_subscribers
                    .lock()
                    .await
                    .insert(HelperId(2), tx);
                close(&state, pane, native_state).await;
                let after = cached(&a, &source).await;
                assert!(after.revision > before.revision);
                assert_eq!(after.sessions[0].status, Some(AgentStatus::Ended));
                assert_eq!(after.sessions[0].pane_session_id, None);
                assert_eq!(after.sessions, cached(&b, &source).await.sessions);
                assert_eq!(
                    crate::session_registry::parse_ext_notification(&rx.try_recv().unwrap()),
                    WtaExtNotification::SshSessionsChanged(source.clone())
                );
                close(&state, pane, native_state).await;
                assert!(rx.try_recv().is_err(), "duplicate close is a no-op");
                assert_eq!(
                    cached(&b, &unrelated).await.sessions[0].status,
                    Some(AgentStatus::Historical)
                );
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn ssh_raw_ids_are_isolated_by_host_user_port_and_cli() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (state, mut native) = harness();
            let sources = [
                source("alice@ubuntu", None, "copilot"),
                source("bob@ubuntu", None, "copilot"),
                source("alice@ubuntu", Some(2222), "copilot"),
                source("alice@debian", None, "copilot"),
                source("alice@ubuntu", None, "claude"),
            ];
            let host = SessionInfo::new(
                acp::schema::v1::SessionId::new("same-id"),
                PathBuf::from(r"C:\host"),
            );
            state.registry.upsert(host.clone()).await;
            let a = caller(&state, 1);
            for source in &sources {
                seed(&state, source, &["same-id"]).await;
            }
            let mut panes = Vec::new();
            for source in &sources {
                let pane = uuid::Uuid::new_v4();
                let resume = activation(&a, source, "same-id");
                created(&mut native, pane).await;
                let snapshot = resume.await.unwrap().unwrap();
                assert_eq!(snapshot.sessions[0].pane_session_id, Some(pane.to_string()));
                panes.push(pane);
            }
            close(&state, panes[0], "closed").await;
            for (index, source) in sources.iter().enumerate() {
                let snapshot = cached(&a, source).await;
                assert_eq!(
                    snapshot.sessions[0].status,
                    Some(if index == 0 {
                        AgentStatus::Ended
                    } else {
                        AgentStatus::Idle
                    })
                );
            }
            assert_eq!(state.registry.snapshot().await, vec![host]);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn ssh_history_merge_preserves_pending_live_and_ended_bindings_and_refreshes_titles() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (state, mut native) = harness();
            let source = source("ubuntu", None, "copilot");
            seed(&state, &source, &["same-id", "stale"]).await;
            let scoped = state.ssh_sessions.source(&source).await;
            let a = caller(&state, 1);
            let pane = uuid::Uuid::new_v4();
            let resume = activation(&a, &source, "same-id");
            let native_request = native.recv().await.unwrap();
            merge_history(&state, &source, &scoped, Vec::new()).await;
            let pending = cached(&a, &source).await;
            assert_eq!(pending.sessions.len(), 1);
            assert_eq!(pending.sessions[0].status, Some(AgentStatus::Idle));
            native_request
                .reply
                .send(Ok(serde_json::json!({ "session_id": pane.to_string() })))
                .unwrap();
            focused(&mut native, pane).await;
            let live = resume.await.unwrap().unwrap();
            merge_history(
                &state,
                &source,
                &scoped,
                vec![history(&source, "same-id", "Updated title")],
            )
            .await;
            let refreshed = cached(&a, &source).await;
            assert!(refreshed.revision > live.revision);
            assert_eq!(
                refreshed.sessions[0].title.as_deref(),
                Some("Updated title")
            );
            assert_eq!(refreshed.sessions[0].status, Some(AgentStatus::Idle));
            assert_eq!(
                refreshed.sessions[0].pane_session_id,
                Some(pane.to_string())
            );
            close(&state, pane, "closed").await;
            merge_history(&state, &source, &scoped, Vec::new()).await;
            merge_history(
                &state,
                &source,
                &scoped,
                vec![history(&source, "same-id", "Ended title")],
            )
            .await;
            let ended = cached(&a, &source).await;
            assert_eq!(ended.sessions[0].status, Some(AgentStatus::Ended));
            assert_eq!(ended.sessions[0].title.as_deref(), Some("Ended title"));
            assert!(native.try_recv().is_err());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn ssh_failed_create_and_invalid_native_ids_release_the_reservation_for_retry() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for response in [
                Err(anyhow!("COM unavailable")),
                Ok(serde_json::json!({})),
                Ok(serde_json::json!({ "session_id": "not-a-guid" })),
                Ok(serde_json::json!({ "session_id": uuid::Uuid::nil().to_string() })),
            ] {
                let (state, mut native) = harness();
                let source = source("ubuntu", None, "copilot");
                seed(&state, &source, &["same-id"]).await;
                let a = caller(&state, 1);
                let resume = activation(&a, &source, "same-id");
                native.recv().await.unwrap().reply.send(response).unwrap();
                assert!(resume.await.unwrap().is_err());
                let ended = cached(&a, &source).await;
                assert_eq!(ended.sessions[0].status, Some(AgentStatus::Ended));
                assert_eq!(ended.sessions[0].pane_session_id, None);
                let retry = activation(&caller(&state, 2), &source, "same-id");
                let pane = uuid::Uuid::new_v4();
                created(&mut native, pane).await;
                assert_eq!(
                    retry.await.unwrap().unwrap().sessions[0].pane_session_id,
                    Some(pane.to_string())
                );
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn ssh_close_before_create_ack_is_not_resurrected_and_tombstones_are_released() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (state, mut native) = harness();
            let source = source("ubuntu", None, "copilot");
            seed(&state, &source, &["same-id"]).await;
            let a = caller(&state, 1);
            let pane = uuid::Uuid::new_v4();
            let resume = activation(&a, &source, "same-id");
            let request = native.recv().await.unwrap();
            close(&state, pane, "closed").await;
            request
                .reply
                .send(Ok(serde_json::json!({ "session_id": pane.to_string() })))
                .unwrap();
            let ended = resume.await.unwrap().unwrap();
            assert_eq!(ended.sessions[0].status, Some(AgentStatus::Ended));
            assert_eq!(ended.sessions[0].pane_session_id, None);
            assert!(
                native.try_recv().is_err(),
                "never focus a pane closed before its create acknowledgement"
            );
            let scoped = state.ssh_sessions.source(&source).await;
            let metadata = scoped.operations.lock().await;
            assert_eq!(metadata.outstanding_creates, 0);
            assert!(metadata.early_closes.is_empty());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn ssh_activation_completion_outlives_request_and_helper_disconnect() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (state, mut native) = harness();
            let source = source("ubuntu", None, "copilot");
            seed(&state, &source, &["same-id"]).await;
            let a = caller(&state, 1);
            let b = caller(&state, 2);
            let resume = activation(&a, &source, "same-id");
            let request = native.recv().await.unwrap();
            resume.abort();
            assert!(resume.await.unwrap_err().is_cancelled());
            drop(a);
            super::super::drop_sessions_for_helper(&state, HelperId(1)).await;
            let (tx, mut changes) = mpsc::unbounded_channel();
            state
                .helper_ext_subscribers
                .lock()
                .await
                .insert(HelperId(2), tx);
            let pane = uuid::Uuid::new_v4();
            request
                .reply
                .send(Ok(serde_json::json!({ "session_id": pane.to_string() })))
                .unwrap();
            focused(&mut native, pane).await;
            let changed = changes.recv().await.unwrap();
            assert_eq!(
                crate::session_registry::parse_ext_notification(&changed),
                WtaExtNotification::SshSessionsChanged(source.clone())
            );
            let peer = cached(&b, &source).await;
            assert_eq!(peer.sessions[0].status, Some(AgentStatus::Idle));
            assert_eq!(peer.sessions[0].pane_session_id, Some(pane.to_string()));
            super::super::drop_sessions_for_helper(&state, HelperId(1)).await;
            assert_eq!(cached(&b, &source).await.sessions, peer.sessions);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn ssh_focus_infrastructure_error_preserves_binding_but_native_not_found_ends_it() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (state, mut native) = harness();
            let source = source("ubuntu", None, "copilot");
            seed(&state, &source, &["same-id"]).await;
            let a = caller(&state, 1);
            let pane = uuid::Uuid::new_v4();
            let resume = activation(&a, &source, "same-id");
            created(&mut native, pane).await;
            let before = resume.await.unwrap().unwrap();
            for (message, ended) in [
                ("wtcli.exe not found", false),
                ("FocusPane failed: 0x80070490", true),
            ] {
                let focus = activation(&caller(&state, 2), &source, "same-id");
                let request = native.recv().await.unwrap();
                assert_eq!(request.method, "focus_pane");
                request.reply.send(Err(anyhow!("{message}"))).unwrap();
                assert!(focus.await.unwrap().is_err());
                let after = cached(&a, &source).await;
                if ended {
                    assert_eq!(after.sessions[0].status, Some(AgentStatus::Ended));
                    assert_eq!(after.sessions[0].pane_session_id, None);
                    assert!(after.revision > before.revision);
                } else {
                    assert_eq!(after.revision, before.revision);
                    assert_eq!(after.sessions[0].pane_session_id, Some(pane.to_string()));
                }
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn ssh_registry_initialize_and_policy_do_not_require_a_local_agent() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut state, mut native) = harness();
            Arc::get_mut(&mut state).unwrap().allowed_agent_ids = Some(HashSet::new());
            let a = caller(&state, 1);
            let response = a
                .initialize(
                    acp::schema::v1::InitializeRequest::new(acp::schema::ProtocolVersion::V1)
                        .client_info(acp::schema::v1::Implementation::new(
                            "wta-session-registry",
                            "test",
                        )),
                )
                .await
                .unwrap();
            assert_eq!(response.protocol_version, acp::schema::ProtocolVersion::V1);
            assert!(a.agent.get().is_none());
            assert!(state.agents.lock().await.is_empty());
            for source in [
                source("ubuntu", None, "copilot"),
                source("ubuntu", None, "custom:untrusted"),
            ] {
                for request in [
                    Request::Poll {
                        source: source.clone(),
                    },
                    Request::List {
                        source: source.clone(),
                        refresh_history: true,
                    },
                    Request::Activate {
                        source: source.clone(),
                        session_id: "same-id".to_owned(),
                    },
                ] {
                    assert!(call(&a, request).await.is_err());
                }
            }
            assert!(state.ssh_sessions.sources.lock().await.is_empty());
            assert!(native.try_recv().is_err());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn ssh_cached_snapshot_is_available_while_its_history_refresh_gate_is_busy() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (state, mut native) = harness();
            let source = source("ubuntu", None, "copilot");
            seed(&state, &source, &["same-id"]).await;
            let scoped = state.ssh_sessions.source(&source).await;
            let _refresh = scoped.refresh.lock().await;
            let a = caller(&state, 1);
            let b = caller(&state, 2);
            assert_eq!(cached(&b, &source).await.sessions.len(), 1);
            let pane = uuid::Uuid::new_v4();
            let resume = activation(&a, &source, "same-id");
            created(&mut native, pane).await;
            assert_eq!(
                resume.await.unwrap().unwrap().sessions[0].status,
                Some(AgentStatus::Idle)
            );
            assert_eq!(
                cached(&b, &source).await.sessions[0].pane_session_id,
                Some(pane.to_string())
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn ssh_poll_refreshes_hook_titles_without_blocking_status_or_changing_bindings() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let source = source("alice@ubuntu", None, "copilot");
            let scoped = state.ssh_sessions.source(&source).await;
            let route = RouteId::new();
            let pane = uuid::Uuid::new_v4().to_string();
            let mut start = hook("copilot", "agent.session.start", "live");
            start.payload["cwd"] = "/home/yuazha".into();
            hook_event(&state, &source.target, route, &pane, start)
                .await
                .unwrap();
            let (notifications, mut receiver) = mpsc::unbounded_channel();
            state
                .helper_ext_subscribers
                .lock()
                .await
                .insert(HelperId(1), notifications);
            let (started, waiting) = oneshot::channel();
            let (complete, history_result) = oneshot::channel();
            let initial = poll(&state, &source, async move {
                started.send(()).unwrap();
                history_result.await.unwrap()
            })
            .await;
            assert_eq!(initial.sessions[0].title.as_deref(), Some("yuazha"));
            waiting.await.unwrap();

            hook_event(
                &state,
                &source.target,
                route,
                &pane,
                hook("copilot", "agent.prompt.submit", "live"),
            )
            .await
            .unwrap();
            let working = list(&state, &source, false, async {
                panic!("hook notifications must read cache without fetching SSH history")
            })
            .await
            .unwrap();
            assert_eq!(working.sessions[0].status, Some(AgentStatus::Working));
            assert_eq!(
                crate::session_registry::parse_ext_notification(&receiver.try_recv().unwrap()),
                WtaExtNotification::SshSessionsChanged(source.clone())
            );
            assert_eq!(
                poll(&state, &source, async {
                    panic!("a second viewer must share the in-flight refresh")
                })
                .await
                .sessions,
                working.sessions
            );

            complete
                .send(Ok(vec![history(&source, "live", "Generated title")]))
                .unwrap();
            let mut last_refresh = scoped.refresh.lock().await;
            assert!(last_refresh.is_some());
            let updated = snapshot(&state, &source, &scoped).await;
            assert!(updated.revision > working.revision);
            let mut expected = working.sessions[0].clone();
            expected.title = Some("Generated title".into());
            assert_eq!(updated.sessions, vec![expected]);
            assert_eq!(
                crate::session_registry::parse_ext_notification(&receiver.try_recv().unwrap()),
                WtaExtNotification::SshSessionsChanged(source.clone())
            );
            *last_refresh = Some(Instant::now() - HISTORY_POLL_INTERVAL);
            drop(last_refresh);

            let renamed = history(&source, "live", "Renamed title");
            poll(&state, &source, async move { Ok(vec![renamed]) }).await;
            let _refresh = scoped.refresh.lock().await;
            let updated = snapshot(&state, &source, &scoped).await;
            assert_eq!(updated.sessions[0].title.as_deref(), Some("Renamed title"));
            assert_eq!(updated.sessions[0].status, Some(AgentStatus::Working));
            assert_eq!(
                updated.sessions[0].pane_session_id.as_deref(),
                Some(pane.as_str())
            );
            assert!(state.registry.snapshot().await.is_empty());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn ssh_poll_throttles_success_and_failure_but_explicit_refresh_remains_available() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let source = source("ubuntu", None, "copilot");
            let scoped = state.ssh_sessions.source(&source).await;
            let initial = list(&state, &source, true, async {
                Ok(vec![history(&source, "same-id", "Original title")])
            })
            .await
            .unwrap();
            let cached = poll(&state, &source, async {
                panic!("automatic polling must respect a recent explicit refresh")
            })
            .await;
            assert_eq!(cached.sessions, initial.sessions);

            *scoped.refresh.lock().await = Some(Instant::now() - HISTORY_POLL_INTERVAL);
            poll(&state, &source, async {
                Err(anyhow!("Remote ACP listing unavailable"))
            })
            .await;
            {
                let last_refresh = scoped.refresh.lock().await;
                assert!(last_refresh.unwrap().elapsed() < HISTORY_POLL_INTERVAL);
            }
            let after_failure = poll(&state, &source, async {
                panic!("a failed background query must not cause a retry storm")
            })
            .await;
            assert_eq!(after_failure.sessions, initial.sessions);
            assert_eq!(after_failure.revision, initial.revision);

            let refreshed = list(&state, &source, true, async {
                Ok(vec![history(&source, "same-id", "Recovered title")])
            })
            .await
            .unwrap();
            assert_eq!(
                refreshed.sessions[0].title.as_deref(),
                Some("Recovered title")
            );
            assert!(refreshed.revision > initial.revision);
            *scoped.refresh.lock().await = Some(Instant::now() - HISTORY_POLL_INTERVAL);
            let renamed = history(&source, "same-id", "Later title");
            poll(&state, &source, async move { Ok(vec![renamed]) }).await;
            let _refresh = scoped.refresh.lock().await;
            assert_eq!(
                snapshot(&state, &source, &scoped).await.sessions[0]
                    .title
                    .as_deref(),
                Some("Later title")
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn ssh_poll_gates_are_source_scoped_and_wire_poll_returns_without_waiting_for_history() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let first = source("alice@ubuntu", None, "copilot");
            seed(&state, &first, &["same-id"]).await;
            let scoped = state.ssh_sessions.source(&first).await;
            let _refresh = scoped.refresh.lock().await;
            let handler = caller(&state, 1);
            let cached = call(
                &handler,
                Request::Poll {
                    source: first.clone(),
                },
            )
            .await
            .unwrap();
            assert_eq!(cached.sessions[0].title.as_deref(), Some("Original title"));

            for other in [
                source("bob@ubuntu", None, "copilot"),
                source("alice@ubuntu", Some(2222), "copilot"),
                source("alice@other", None, "copilot"),
                source("alice@ubuntu", None, "claude"),
            ] {
                let row = history(&other, "same-id", "Independent title");
                let initial = poll(&state, &other, async move { Ok(vec![row]) }).await;
                assert!(initial.sessions.is_empty());
                let other_scoped = state.ssh_sessions.source(&other).await;
                let _other_refresh = other_scoped.refresh.lock().await;
                assert_eq!(
                    snapshot(&state, &other, &other_scoped).await.sessions[0]
                        .title
                        .as_deref(),
                    Some("Independent title")
                );
            }
            assert_eq!(
                snapshot(&state, &first, &scoped).await.sessions,
                cached.sessions
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn ssh_early_close_tracking_is_bounded_and_overflow_checks_native_liveness() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (state, mut native) = harness();
            let source = source("ubuntu", None, "copilot");
            seed(&state, &source, &["same-id"]).await;
            let a = caller(&state, 1);
            let resume = activation(&a, &source, "same-id");
            let request = native.recv().await.unwrap();
            for _ in 0..MAX_EARLY_CLOSES {
                pane_closed(&state, &uuid::Uuid::new_v4().to_string()).await;
            }
            let pane = uuid::Uuid::new_v4();
            close(&state, pane, "closed").await;
            let scoped = state.ssh_sessions.source(&source).await;
            assert_eq!(
                scoped.operations.lock().await.early_closes.len(),
                MAX_EARLY_CLOSES
            );
            request
                .reply
                .send(Ok(serde_json::json!({ "session_id": pane.to_string() })))
                .unwrap();
            let verify = native.recv().await.unwrap();
            assert_eq!(verify.method, "focus_pane");
            verify
                .reply
                .send(Err(anyhow!("FocusPane failed: 0x80070490")))
                .unwrap();
            assert!(resume.await.unwrap().is_err());
            assert_eq!(
                cached(&a, &source).await.sessions[0].status,
                Some(AgentStatus::Ended)
            );
            assert!(scoped.operations.lock().await.early_closes.is_empty());
        })
        .await;
}

#[tokio::test]
async fn ssh_wire_dispatch_matches_both_acp_forms_and_never_forwards_malformed_requests() {
    let state = make_state();
    let handler = caller(&state, 1);
    let source = source("alice@ubuntu", Some(2222), "copilot");
    let host = SessionInfo::new(
        acp::schema::v1::SessionId::new("host-session"),
        PathBuf::from(r"C:\host"),
    );
    state.registry.upsert(host.clone()).await;
    for method in [METHOD, METHOD.strip_prefix('_').unwrap()] {
        let raw = serde_json::value::to_raw_value(&Request::List {
            source: source.clone(),
            refresh_history: false,
        })
        .unwrap();
        assert!(matches!(
            crate::session_registry::parse_ext_request(acp::schema::v1::ExtRequest::new(
                method,
                raw.into()
            )),
            WtaExtRequest::SshSessions(Request::List { .. })
        ));
        for body in [
            serde_json::json!({}),
            serde_json::json!({ "op": "activate", "source": source }),
            serde_json::json!({ "op": "activate", "source": source, "session_id": "id", "commandline": "untrusted" }),
            serde_json::json!({ "op": "list", "source": { "target": { "destination": "-oProxyCommand=bad" }, "agent_id": "copilot" }, "refresh_history": false }),
        ] {
            let raw = serde_json::value::to_raw_value(&body).unwrap();
            let request = acp::schema::v1::ExtRequest::new(method, raw.into());
            assert!(matches!(
                crate::session_registry::parse_ext_request(request.clone()),
                WtaExtRequest::Malformed { .. }
            ));
            assert!(handler.ext_method(request).await.is_err());
        }
    }
    for method in [CHANGED_METHOD, CHANGED_METHOD.strip_prefix('_').unwrap()] {
        let notification = acp::schema::v1::ExtNotification::new(
            method,
            serde_json::value::to_raw_value(&source).unwrap().into(),
        );
        assert_eq!(
            crate::session_registry::apply_ext_notification(&*state.registry, &notification).await,
            WtaExtNotification::SshSessionsChanged(source.clone())
        );
    }
    assert_eq!(state.registry.snapshot().await, vec![host]);
    assert!(handler.agent.get().is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn ssh_snapshot_epochs_are_service_scoped_and_revisions_only_change_with_state() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let other_master = make_state();
            let source = source("ubuntu", None, "copilot");
            seed(&state, &source, &["same-id"]).await;
            seed(&other_master, &source, &["same-id"]).await;
            let a = caller(&state, 1);
            let first = cached(&a, &source).await;
            let scoped = state.ssh_sessions.source(&source).await;
            merge_history(
                &state,
                &source,
                &scoped,
                vec![history(&source, "same-id", "Original title")],
            )
            .await;
            assert_eq!(cached(&a, &source).await.revision, first.revision);
            assert_ne!(
                cached(&caller(&other_master, 1), &source).await.epoch,
                first.epoch
            );
            let raw = serde_json::to_value(&first).unwrap();
            assert_eq!(raw["epoch"], first.epoch.to_string());
            let roundtrip: Snapshot = serde_json::from_value(raw).unwrap();
            assert_eq!(roundtrip.source, first.source);
            assert_eq!(roundtrip.epoch, first.epoch);
            assert_eq!(roundtrip.revision, first.revision);
            assert_eq!(roundtrip.sessions, first.sessions);
        })
        .await;
}

#[tokio::test]
async fn ssh_history_fetch_errors_are_explicit_and_never_replace_a_cached_snapshot() {
    let state = make_state();
    let source = source("ubuntu", None, "copilot");
    let failure = list(&state, &source, false, async {
        Err(anyhow!("SSH authentication failed"))
    })
    .await;
    assert!(failure.is_err());
    let scoped = state.ssh_sessions.source(&source).await;
    assert!(!scoped.operations.lock().await.history_loaded);
    let initial = list(&state, &source, false, async {
        Ok(vec![history(&source, "same-id", "Original title")])
    })
    .await
    .unwrap();
    let cached = list(&state, &source, false, async {
        panic!("cached list must not fetch remote history")
    })
    .await
    .unwrap();
    assert_eq!(cached.revision, initial.revision);
    assert_eq!(cached.sessions, initial.sessions);
    assert!(list(&state, &source, true, async {
        Err(anyhow!("SSH refresh failed"))
    })
    .await
    .is_err());
    let after_failure = snapshot(&state, &source, &scoped).await;
    assert_eq!(after_failure.revision, initial.revision);
    assert_eq!(after_failure.sessions, initial.sessions);
    let refreshed = list(&state, &source, true, async { Ok(Vec::new()) })
        .await
        .unwrap();
    assert!(refreshed.revision > initial.revision);
    assert!(refreshed.sessions.is_empty());
}

#[tokio::test]
async fn ssh_historical_refresh_updates_metadata_without_losing_a_displayable_title() {
    let state = make_state();
    let source = source("ubuntu", None, "copilot");
    seed(&state, &source, &["same-id"]).await;
    let scoped = state.ssh_sessions.source(&source).await;
    let before = snapshot(&state, &source, &scoped).await;
    let mut refreshed = history(&source, "same-id", "Updated title");
    refreshed.cwd = PathBuf::from("/home/user/new-project");
    refreshed.last_activity_at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(200);
    merge_history(&state, &source, &scoped, vec![refreshed.clone()]).await;
    let after = snapshot(&state, &source, &scoped).await;
    assert!(after.revision > before.revision);
    assert_eq!(after.sessions[0].cwd, refreshed.cwd);
    assert_eq!(after.sessions[0].last_activity_at_ms, Some(200_000));
    assert_eq!(after.sessions[0].title.as_deref(), Some("Updated title"));
    assert_eq!(after.sessions[0].status, Some(AgentStatus::Historical));
    assert_eq!(after.sessions[0].pane_session_id, None);
    refreshed.title.clear();
    merge_history(&state, &source, &scoped, vec![refreshed]).await;
    let without_title = snapshot(&state, &source, &scoped).await;
    assert_eq!(without_title.revision, after.revision);
    assert_eq!(without_title.sessions, after.sessions);
}

#[tokio::test(flavor = "current_thread")]
async fn ssh_concurrent_first_lists_share_history_without_blocking_another_source() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let source = source("ubuntu", None, "copilot");
            let other = Source {
                agent_id: "claude".to_owned(),
                ..source.clone()
            };
            let (started_tx, started_rx) = oneshot::channel();
            let (release_tx, release_rx) = oneshot::channel();
            let a_state = Arc::clone(&state);
            let a_source = source.clone();
            let first = tokio::task::spawn_local(async move {
                list(&a_state, &a_source, false, async {
                    started_tx.send(()).unwrap();
                    release_rx.await.unwrap();
                    Ok(vec![history(&a_source, "same-id", "Original title")])
                })
                .await
                .unwrap()
            });
            started_rx.await.unwrap();
            let b_state = Arc::clone(&state);
            let b_source = source.clone();
            let second = tokio::task::spawn_local(async move {
                list(&b_state, &b_source, false, async {
                    panic!("another first-list caller must reuse the loaded history")
                })
                .await
                .unwrap()
            });
            let independent = list(&state, &other, false, async {
                Ok(vec![history(&other, "same-id", "Other title")])
            })
            .await
            .unwrap();
            assert_eq!(
                independent.sessions[0].title.as_deref(),
                Some("Other title")
            );
            release_tx.send(()).unwrap();
            let a = first.await.unwrap();
            let b = second.await.unwrap();
            assert_eq!(a.revision, b.revision);
            assert_eq!(a.sessions, b.sessions);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn ssh_activation_requires_a_listed_row_and_valid_stored_resume_metadata() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (state, mut native) = harness();
            let source = source("ubuntu", None, "copilot");
            let a = caller(&state, 1);
            assert!(call(
                &a,
                Request::Activate {
                    source: source.clone(),
                    session_id: "not-listed".to_owned()
                }
            )
            .await
            .is_err());
            let scoped = state.ssh_sessions.source(&source).await;
            let mut invalid = history(&source, "bad-cwd", "Original title");
            invalid.cwd = PathBuf::from(r"C:\not-a-remote-directory");
            merge_history(
                &state,
                &source,
                &scoped,
                vec![invalid, history(&source, "--option", "Original title")],
            )
            .await;
            for session_id in ["bad-cwd", "--option"] {
                assert!(call(
                    &a,
                    Request::Activate {
                        source: source.clone(),
                        session_id: session_id.to_owned()
                    }
                )
                .await
                .is_err());
            }
            assert!(native.try_recv().is_err());
            assert!(cached(&a, &source)
                .await
                .sessions
                .iter()
                .all(|row| row.status == Some(AgentStatus::Historical)));
            assert_eq!(scoped.operations.lock().await.outstanding_creates, 0);
        })
        .await;
}
