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
