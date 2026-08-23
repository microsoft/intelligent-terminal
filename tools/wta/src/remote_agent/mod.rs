//! Executable local Remote Agent Host MVP.
//!
//! The subsystem is deliberately isolated from the established WTA
//! master/helper ACP path. It implements a small server-side AHP JSON-RPC
//! subset because the official `ahp` 0.7.0 crate supplies a client, reducers,
//! and transport abstraction but no server runtime.

pub(crate) mod legacy;

mod acp_backend;
pub(crate) mod client;
mod host;
mod mirror;
mod persistence;
mod relay;
mod replay;
mod snapshot;
mod state;
mod terminal_bridge;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};

use persistence::FileHostStateStore;

pub(crate) use terminal_bridge::{
    apply_terminal_client_event, terminal_client_config_from_env,
};

/// Start the standalone loopback host. This path is never entered by existing
/// master/helper launches, so their behavior remains unchanged.
pub(crate) async fn run_host(
    listen: std::net::SocketAddr,
    replay_capacity: usize,
    state_path: Option<PathBuf>,
    auth_token_path: Option<PathBuf>,
) -> Result<()> {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(run_host_local(
            listen,
            replay_capacity,
            state_path,
            auth_token_path,
        ))
        .await
}

async fn run_host_local(
    listen: std::net::SocketAddr,
    replay_capacity: usize,
    state_path: Option<PathBuf>,
    auth_token_path: Option<PathBuf>,
) -> Result<()> {
    let state_path = match state_path {
        Some(path) => path,
        None => crate::runtime_paths::remote_agent_host_state_path()
            .context("resolve Remote Agent Host runtime state path")?,
    };
    let auth_token_path = auth_token_path.unwrap_or_else(|| state_path.with_extension("token"));
    let auth_token = load_or_create_auth_token(&auth_token_path)?;
    let store = Arc::new(FileHostStateStore::new(state_path.clone())?);
    let (backend, mut backend_events) = acp_backend::HostAcpBackend::start();
    let host =
        host::RemoteAgentHost::load_with_backend(store, replay_capacity, &auth_token, backend)?;
    let event_host = Arc::clone(&host);
    tokio::task::spawn_local(async move {
        while let Some(event) = backend_events.recv().await {
            if let Err(error) = event_host.apply_backend_event(event) {
                tracing::warn!(
                    target = "remote_agent_host",
                    %error,
                    "failed to apply Host ACP event"
                );
            }
        }
    });
    tracing::info!(
        target = "remote_agent_host",
        host_id = %host.host_id(),
        state_path = %state_path.display(),
        auth_token_path = %auth_token_path.display(),
        "starting standalone Remote Agent Host MVP"
    );
    host::serve(host, listen).await
}

/// Run the local client vertical slice and print its report as JSON.
pub(crate) async fn run_diagnostic(
    address: std::net::SocketAddr,
    client_id: &str,
    last_seen_server_seq: Option<i64>,
    auth_token_path: Option<PathBuf>,
) -> Result<()> {
    let auth_token = load_auth_token(auth_token_path)?;
    let report =
        mirror::run_diagnostic(address, client_id, last_seen_server_seq, &auth_token).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

/// List the current remote catalogue through the reusable client runtime.
pub(crate) async fn run_client_list(
    address: std::net::SocketAddr,
    client_id: &str,
    auth_token_path: Option<PathBuf>,
) -> Result<()> {
    let auth_token = load_auth_token(auth_token_path)?;
    let catalogue = client::list_catalogue(address, client_id, &auth_token).await?;
    println!("{}", serde_json::to_string_pretty(&catalogue)?);
    Ok(())
}

/// Run a long-lived remote client and emit newline-delimited JSON events.
pub(crate) async fn run_client_watch(
    address: std::net::SocketAddr,
    client_id: &str,
    auth_token_path: Option<PathBuf>,
    event_limit: Option<usize>,
) -> Result<()> {
    let auth_token = load_auth_token(auth_token_path)?;
    client::watch(
        client::RemoteClientConfig {
            address,
            client_id: client_id.to_string(),
            auth_token,
            event_limit,
            ..client::RemoteClientConfig::default()
        },
        |event| {
            use std::io::Write;

            let mut value = serde_json::to_value(event)?;
            value
                .as_object_mut()
                .context("serialize Remote Agent client event as an object")?
                .insert("schemaVersion".to_string(), serde_json::json!(1));
            let mut stdout = std::io::stdout().lock();
            writeln!(stdout, "{}", serde_json::to_string(&value)?)?;
            stdout.flush()?;
            Ok(())
        },
    )
    .await
}

/// Submit one compatibility-boundary legacy summary to a running local host.
pub(crate) async fn run_legacy_ingest(
    address: std::net::SocketAddr,
    summary: legacy::LegacySessionSummary,
    auth_token_path: Option<PathBuf>,
) -> Result<()> {
    let auth_token = load_auth_token(auth_token_path)?;
    let result = mirror::run_legacy_ingest(address, summary, &auth_token).await?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn default_auth_token_path() -> Result<PathBuf> {
    Ok(crate::runtime_paths::remote_agent_host_state_path()
        .context("resolve Remote Agent Host runtime state path")?
        .with_extension("token"))
}

fn load_auth_token(path: Option<PathBuf>) -> Result<String> {
    let path = match path {
        Some(path) => path,
        None => default_auth_token_path()?,
    };
    let token = std::fs::read_to_string(&path)
        .with_context(|| format!("read Remote Agent Host token {}", path.display()))?;
    let token = token.trim().to_string();
    anyhow::ensure!(!token.is_empty(), "Remote Agent Host token is empty");
    anyhow::ensure!(
        token.len() <= 4096,
        "Remote Agent Host token exceeds 4096 bytes"
    );
    Ok(token)
}

fn load_or_create_auth_token(path: &std::path::Path) -> Result<String> {
    if path.exists() {
        return load_auth_token(Some(path.to_path_buf()));
    }
    let parent = path
        .parent()
        .context("Remote Agent Host token path has no parent")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create token directory {}", parent.display()))?;
    let token = format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    match std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
    {
        Ok(mut file) => {
            use std::io::Write;
            file.write_all(token.as_bytes())
                .with_context(|| format!("write Remote Agent Host token {}", path.display()))?;
            file.sync_all()
                .with_context(|| format!("flush Remote Agent Host token {}", path.display()))?;
            Ok(token)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            load_auth_token(Some(path.to_path_buf()))
        }
        Err(error) => {
            Err(error).with_context(|| format!("create Remote Agent Host token {}", path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use ahp_types::actions::{
        ChatTurnCancelledAction, ChatTurnStartedAction, SessionTitleChangedAction, StateAction,
    };
    use ahp_types::commands::{DispatchActionParams, ListSessionsParams, ReconnectResult};
    use ahp_types::state::{
        Message, MessageKind, MessageOrigin, SessionLifecycle, SessionStatus, SnapshotState,
        TurnState,
    };
    use tokio::net::TcpListener;

    use super::host::RemoteAgentHost;
    use super::legacy::{LegacySessionSummary, LegacySessionSummarySink};
    use super::mirror::{MirrorApplyOutcome, RemoteClientMirror};
    use super::persistence::{FileHostStateStore, HostStateStore, MemoryHostStateStore};
    use super::relay::RelayKind;
    use super::state::{DispatchOutcome, DomainSessionLifecycle, HostState};
    fn test_host(replay_capacity: usize) -> Arc<RemoteAgentHost> {
        let store: Arc<dyn HostStateStore> = Arc::new(MemoryHostStateStore::empty());
        RemoteAgentHost::load(store, replay_capacity, "test-token").expect("create in-memory host")
    }

    fn seed_session(host: &RemoteAgentHost) -> ahp_types::state::SessionSummary {
        let outcome = host
            .ingest_legacy_summary(LegacySessionSummary::new(
                "legacy-test".to_string(),
                "session-1".to_string(),
                "Initial title".to_string(),
            ))
            .expect("ingest legacy summary");
        super::snapshot::session_summary(&outcome.session)
    }

    fn title_dispatch(resource: String, client_seq: i64, title: &str) -> DispatchActionParams {
        DispatchActionParams {
            channel: resource,
            client_seq,
            action: StateAction::SessionTitleChanged(SessionTitleChangedAction {
                title: title.to_string(),
            }),
        }
    }

    fn turn_dispatch(resource: &str, client_seq: i64, turn_id: &str) -> DispatchActionParams {
        DispatchActionParams {
            channel: resource.to_string(),
            client_seq,
            action: StateAction::ChatTurnStarted(ChatTurnStartedAction {
                turn_id: turn_id.to_string(),
                started_at: "2026-08-23T00:00:00.000Z".to_string(),
                message: Message {
                    text: "Explain this failure".to_string(),
                    origin: MessageOrigin {
                        kind: MessageKind::User,
                    },
                    meta: None,
                    attachments: None,
                    model: None,
                    agent: None,
                },
                queued_message_id: None,
                meta: None,
            }),
        }
    }

    fn cancel_dispatch(
        resource: &str,
        client_seq: i64,
        turn_id: &str,
    ) -> DispatchActionParams {
        DispatchActionParams {
            channel: resource.to_string(),
            client_seq,
            action: StateAction::ChatTurnCancelled(ChatTurnCancelledAction {
                turn_id: turn_id.to_string(),
                duration: 15,
                meta: None,
            }),
        }
    }

    #[test]
    fn host_native_session_lifecycle_is_reduced_and_snapshotted() {
        let resource = "ahp-session:/host-native-1";
        let mut state = HostState::new();
        let created = state
            .begin_session_creation(
                resource,
                "copilot".to_string(),
                Some(vec!["file:///C:/repo".to_string()]),
            )
            .expect("begin session creation");

        assert_eq!(created.session.lifecycle, DomainSessionLifecycle::Creating);
        assert_eq!(created.session.status, SessionStatus::Idle.bits());
        assert!(matches!(
            created.envelope.kind,
            RelayKind::RootActiveSessionsChanged { active_sessions: 1 }
        ));
        let creating_snapshot =
            super::snapshot::snapshot_for(&state, resource).expect("creating snapshot");
        let SnapshotState::Session(creating_state) = creating_snapshot.state else {
            panic!("expected session snapshot");
        };
        assert_eq!(creating_state.lifecycle, SessionLifecycle::Creating);
        assert_eq!(
            creating_state.working_directories,
            Some(vec!["file:///C:/repo".to_string()])
        );

        let ready = state
            .complete_session_creation(resource, "acp-session-1".to_string())
            .expect("complete session creation");
        assert!(matches!(ready.envelope.kind, RelayKind::SessionReady));
        assert_eq!(ready.session.lifecycle, DomainSessionLifecycle::Ready);
        assert_eq!(
            ready.session.agent_session_id.as_deref(),
            Some("acp-session-1")
        );
        let ready_snapshot =
            super::snapshot::snapshot_for(&state, resource).expect("ready snapshot");
        let SnapshotState::Session(ready_state) = ready_snapshot.state else {
            panic!("expected session snapshot");
        };
        assert_eq!(ready_state.lifecycle, SessionLifecycle::Ready);
        assert!(ready_state.creation_error.is_none());
    }

    #[test]
    fn host_native_session_creation_failure_is_durable() {
        let resource = "ahp-session:/host-native-failed";
        let mut state = HostState::new();
        state
            .begin_session_creation(resource, "copilot".to_string(), None)
            .expect("begin session creation");
        let failed = state
            .fail_session_creation(
                resource,
                "AcpInitializeFailed".to_string(),
                "agent did not initialize".to_string(),
            )
            .expect("fail session creation");

        assert!(matches!(
            failed.envelope.kind,
            RelayKind::SessionCreationFailed { .. }
        ));
        assert_eq!(
            failed.session.lifecycle,
            DomainSessionLifecycle::CreationFailed
        );
        assert_eq!(failed.session.status, SessionStatus::Error.bits());

        let restored = HostState::from_persisted(state.persisted());
        let snapshot = super::snapshot::snapshot_for(&restored, resource).expect("failed snapshot");
        let SnapshotState::Session(session) = snapshot.state else {
            panic!("expected session snapshot");
        };
        assert_eq!(session.lifecycle, SessionLifecycle::CreationFailed);
        let error = session.creation_error.expect("creation error");
        assert_eq!(error.error_type, "AcpInitializeFailed");
        assert_eq!(error.message, "agent did not initialize");
    }

    #[test]
    fn host_native_session_resource_is_unique_and_disposable() {
        let resource = "ahp-session:/host-native-dispose";
        let mut state = HostState::new();
        state
            .begin_session_creation(resource, "copilot".to_string(), None)
            .expect("begin session creation");
        let duplicate = state.begin_session_creation(resource, "copilot".to_string(), None);
        assert_eq!(
            duplicate.expect_err("duplicate session must fail"),
            "session resource already exists"
        );

        let disposed = state.dispose_session(resource).expect("dispose session");
        assert_eq!(disposed.resource, resource);
        assert!(matches!(
            disposed.envelope.kind,
            RelayKind::RootActiveSessionsChanged { active_sessions: 0 }
        ));
        assert!(super::snapshot::snapshot_for(&state, resource).is_none());
    }

    #[test]
    fn host_native_chat_turn_stream_is_durable_and_snapshotted() {
        let session_resource = "ahp-session:/chat-session";
        let chat_resource = "ahp-chat:/chat-1";
        let mut state = HostState::new();
        state
            .begin_session_creation(session_resource, "copilot".to_string(), None)
            .expect("begin session");
        state
            .complete_session_creation(session_resource, "acp-chat-session".to_string())
            .expect("complete session");
        let created = state
            .create_chat(session_resource, chat_resource)
            .expect("create chat");
        assert_eq!(created.envelopes.len(), 2);

        let epoch = state.connect_client("client");
        let started = state
            .start_turn(
                "client",
                epoch,
                turn_dispatch(chat_resource, 1, "turn-1"),
            )
            .expect("start turn");
        assert!(matches!(started, DispatchOutcome::Accepted(_)));

        let first = state
            .append_agent_text("acp-chat-session", "Hello ".to_string())
            .expect("append first chunk");
        assert_eq!(first.len(), 2, "first chunk creates a part then appends");
        let second = state
            .append_agent_text("acp-chat-session", "world".to_string())
            .expect("append second chunk");
        assert_eq!(second.len(), 1);
        state
            .finish_turn("acp-chat-session", 42, None)
            .expect("finish turn");

        let restored = HostState::from_persisted(state.persisted());
        let chat =
            super::snapshot::snapshot_for(&restored, chat_resource).expect("chat snapshot");
        let SnapshotState::Chat(chat) = chat.state else {
            panic!("expected chat snapshot");
        };
        assert!(chat.active_turn.is_none());
        assert_eq!(chat.turns.len(), 1);
        assert_eq!(chat.turns[0].state, TurnState::Complete);
        assert_eq!(chat.turns[0].message.text, "Explain this failure");
        assert_eq!(chat.turns[0].response_parts.len(), 1);
        let ahp_types::state::ResponsePart::Markdown(part) =
            &chat.turns[0].response_parts[0]
        else {
            panic!("expected markdown response");
        };
        assert_eq!(part.content, "Hello world");

        let session =
            super::snapshot::snapshot_for(&restored, session_resource).expect("session snapshot");
        let SnapshotState::Session(session) = session.state else {
            panic!("expected session snapshot");
        };
        assert_eq!(session.default_chat.as_deref(), Some(chat_resource));
        assert_eq!(session.chats.len(), 1);
        assert_eq!(session.chats[0].resource, chat_resource);
    }

    #[test]
    fn host_native_chat_rejects_parallel_turns_and_records_errors() {
        let session_resource = "ahp-session:/chat-error-session";
        let chat_resource = "ahp-chat:/chat-error";
        let mut state = HostState::new();
        state
            .begin_session_creation(session_resource, "copilot".to_string(), None)
            .expect("begin session");
        state
            .complete_session_creation(session_resource, "acp-chat-error".to_string())
            .expect("complete session");
        state
            .create_chat(session_resource, chat_resource)
            .expect("create chat");
        let epoch = state.connect_client("client");
        state
            .start_turn(
                "client",
                epoch,
                turn_dispatch(chat_resource, 1, "turn-1"),
            )
            .expect("start first turn");
        let parallel = state
            .start_turn(
                "client",
                epoch,
                turn_dispatch(chat_resource, 2, "turn-2"),
            )
            .expect("reject parallel turn");
        let DispatchOutcome::Rejected(rejected) = parallel else {
            panic!("parallel turn must be rejected");
        };
        assert_eq!(
            rejected.rejection_reason.as_deref(),
            Some("chat already has an active turn")
        );

        state
            .finish_turn(
                "acp-chat-error",
                7,
                Some(super::state::DomainError {
                    error_type: "AcpPromptFailed".to_string(),
                    message: "agent disconnected".to_string(),
                }),
            )
            .expect("record turn error");
        let snapshot =
            super::snapshot::snapshot_for(&state, chat_resource).expect("chat snapshot");
        let SnapshotState::Chat(chat) = snapshot.state else {
            panic!("expected chat snapshot");
        };
        assert_eq!(chat.turns[0].state, TurnState::Error);
        assert_eq!(
            chat.turns[0].error.as_ref().map(|error| error.message.as_str()),
            Some("agent disconnected")
        );
    }

    #[test]
    fn host_native_chat_cancellation_is_durable_and_rejects_stale_turns() {
        let session_resource = "ahp-session:/chat-cancel-session";
        let chat_resource = "ahp-chat:/chat-cancel";
        let mut state = HostState::new();
        state
            .begin_session_creation(session_resource, "copilot".to_string(), None)
            .expect("begin session");
        state
            .complete_session_creation(session_resource, "acp-chat-cancel".to_string())
            .expect("complete session");
        state
            .create_chat(session_resource, chat_resource)
            .expect("create chat");
        let epoch = state.connect_client("client");
        state
            .start_turn(
                "client",
                epoch,
                turn_dispatch(chat_resource, 1, "turn-1"),
            )
            .expect("start turn");

        let wrong_turn = state
            .cancel_turn(
                "client",
                epoch,
                cancel_dispatch(chat_resource, 2, "turn-2"),
            )
            .expect("reject stale cancellation");
        let DispatchOutcome::Rejected(rejected) = wrong_turn else {
            panic!("stale cancellation must be rejected");
        };
        assert_eq!(
            rejected.rejection_reason.as_deref(),
            Some("turn identifier does not match the active turn")
        );

        let cancelled = state
            .cancel_turn(
                "client",
                epoch,
                cancel_dispatch(chat_resource, 2, "turn-1"),
            )
            .expect("cancel turn");
        let DispatchOutcome::Accepted(cancelled) = cancelled else {
            panic!("matching cancellation must be accepted");
        };
        assert!(matches!(
            cancelled.kind,
            RelayKind::ChatTurnCancelled {
                ref turn_id,
                duration: 15
            } if turn_id == "turn-1"
        ));

        let restored = HostState::from_persisted(state.persisted());
        let snapshot =
            super::snapshot::snapshot_for(&restored, chat_resource).expect("chat snapshot");
        let SnapshotState::Chat(chat) = snapshot.state else {
            panic!("expected chat snapshot");
        };
        assert!(chat.active_turn.is_none());
        assert_eq!(chat.turns.len(), 1);
        assert_eq!(chat.turns[0].state, TurnState::Cancelled);
        assert_eq!(chat.turns[0].duration, Some(15));
    }

    #[test]
    fn host_coordinator_persists_and_replays_native_session_lifecycle() {
        let host = test_host(8);
        let resource = "ahp-session:/host-coordinator";
        let (epoch, _) = host
            .initialize(
                "client",
                &[
                    ahp_types::ROOT_RESOURCE_URI.to_string(),
                    resource.to_string(),
                ],
                None,
            )
            .expect("initialize");

        let snapshot = host
            .begin_session_creation(resource, "copilot".to_string(), None)
            .expect("begin session creation");
        let SnapshotState::Session(session) = snapshot.state else {
            panic!("expected session snapshot");
        };
        assert_eq!(session.lifecycle, SessionLifecycle::Creating);
        host.complete_session_creation(resource, "acp-session-1".to_string())
            .expect("complete session creation");

        let (_, reconnect) = host
            .reconnect(
                "client",
                0,
                &[
                    ahp_types::ROOT_RESOURCE_URI.to_string(),
                    resource.to_string(),
                ],
                None,
            )
            .expect("reconnect");
        let ReconnectResult::Replay(replay) = reconnect else {
            panic!("expected lifecycle replay");
        };
        assert_eq!(replay.actions.len(), 2);
        assert!(matches!(
            replay.actions[0].action,
            StateAction::RootActiveSessionsChanged(_)
        ));
        assert!(matches!(
            replay.actions[1].action,
            StateAction::SessionReady(_)
        ));

        host.dispose_session(resource).expect("dispose session");
        assert!(host
            .list_sessions(ListSessionsParams {
                channel: ahp_types::ROOT_RESOURCE_URI.to_string(),
                limit: None,
                cursor: None,
            })
            .expect("list sessions")
            .items
            .is_empty());

        let failed_resource = "ahp-session:/host-coordinator-failed";
        host.begin_session_creation(failed_resource, "copilot".to_string(), None)
            .expect("begin failed session creation");
        host.fail_session_creation(
            failed_resource,
            "AcpInitializeFailed".to_string(),
            "agent did not initialize".to_string(),
        )
        .expect("fail session creation");
        let (_, failed_init) = host
            .initialize("observer", &[failed_resource.to_string()], None)
            .expect("initialize failed-session observer");
        let failed_snapshot = failed_init
            .snapshots
            .into_iter()
            .next()
            .expect("failed snapshot");
        let SnapshotState::Session(failed_session) = failed_snapshot.state else {
            panic!("expected failed session snapshot");
        };
        assert_eq!(failed_session.lifecycle, SessionLifecycle::CreationFailed);
        assert_ne!(epoch, 0);
    }

    #[tokio::test]
    async fn host_acp_backend_failure_drives_creation_failed_lifecycle() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let store: Arc<dyn HostStateStore> =
                    Arc::new(MemoryHostStateStore::empty());
                let (backend, _events) = super::acp_backend::HostAcpBackend::start();
                let host =
                    RemoteAgentHost::load_with_backend(store, 8, "test-token", backend)
                        .expect("create host with ACP backend");
                let resource = "ahp-session:/unknown-provider";

                let snapshot = host
                    .create_session(resource, "not-a-real-provider".to_string(), None)
                    .expect("begin ACP-backed session creation");
                let SnapshotState::Session(session) = snapshot.state else {
                    panic!("expected creating session snapshot");
                };
                assert_eq!(session.lifecycle, SessionLifecycle::Creating);

                tokio::time::timeout(std::time::Duration::from_secs(1), async {
                    loop {
                        let (_, initialized) = host
                            .initialize("observer", &[resource.to_string()], None)
                            .expect("observe ACP-backed session");
                        let lifecycle = initialized
                            .snapshots
                            .into_iter()
                            .next()
                            .and_then(|snapshot| match snapshot.state {
                                SnapshotState::Session(session) => Some(session.lifecycle),
                                _ => None,
                            });
                        if lifecycle == Some(SessionLifecycle::CreationFailed) {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("ACP backend failure should update the lifecycle");
            })
            .await;
    }

    #[test]
    fn two_clients_have_independent_client_sequences() {
        let host = test_host(8);
        let session = seed_session(&host);
        let (first_epoch, _) = host
            .initialize("client-one", &[], None)
            .expect("initialize first");
        let (second_epoch, _) = host
            .initialize("client-two", &[], None)
            .expect("initialize second");

        assert!(matches!(
            host.dispatch(
                "client-one",
                first_epoch,
                title_dispatch(session.resource.clone(), 1, "One")
            )
            .expect("dispatch from first"),
            DispatchOutcome::Accepted(_)
        ));
        assert!(matches!(
            host.dispatch(
                "client-two",
                second_epoch,
                title_dispatch(session.resource.clone(), 1, "Two")
            )
            .expect("dispatch from second"),
            DispatchOutcome::Accepted(_)
        ));

        let sessions = host
            .list_sessions(ListSessionsParams {
                channel: ahp_types::ROOT_RESOURCE_URI.to_string(),
                limit: None,
                cursor: None,
            })
            .expect("list sessions");
        assert_eq!(sessions.items[0].title, "Two");
    }

    #[test]
    fn concurrent_duplicate_dispatch_accepts_once_and_rejects_once() {
        let host = test_host(8);
        let session = seed_session(&host);
        let (epoch, _) = host.initialize("client", &[], None).expect("initialize");
        let barrier = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for title in ["left", "right"] {
            let host = host.clone();
            let barrier = barrier.clone();
            let resource = session.resource.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                host.dispatch("client", epoch, title_dispatch(resource, 1, title))
                    .expect("concurrent dispatch")
            }));
        }
        barrier.wait();

        let outcomes: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().expect("worker should not panic"))
            .collect();
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, DispatchOutcome::Accepted(_)))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, DispatchOutcome::Rejected(_)))
                .count(),
            1
        );
    }

    #[test]
    fn replay_resumes_events_after_last_seen_sequence() {
        let host = test_host(8);
        let session = seed_session(&host);
        let (epoch, initialized) = host.initialize("client", &[], None).expect("initialize");
        let initial_seq = initialized.server_seq;
        let accepted = host
            .dispatch(
                "client",
                epoch,
                title_dispatch(session.resource.clone(), 1, "Replayed"),
            )
            .expect("dispatch");
        assert!(matches!(accepted, DispatchOutcome::Accepted(_)));

        let (_, reconnect) = host
            .reconnect("client", initial_seq, &[session.resource.clone()], None)
            .expect("reconnect");
        let ReconnectResult::Replay(replay) = reconnect else {
            panic!("expected bounded replay");
        };
        assert_eq!(replay.actions.len(), 1);
        assert_eq!(replay.actions[0].server_seq, initial_seq as u64 + 1);
    }

    #[test]
    fn old_gap_falls_back_to_fresh_snapshots() {
        let host = test_host(1);
        let session = seed_session(&host);
        let (epoch, _) = host.initialize("client", &[], None).expect("initialize");
        host.dispatch(
            "client",
            epoch,
            title_dispatch(session.resource.clone(), 1, "Beyond replay"),
        )
        .expect("dispatch");

        let (_, reconnect) = host
            .reconnect(
                "client",
                0,
                &[ahp_types::ROOT_RESOURCE_URI.to_string(), session.resource],
                None,
            )
            .expect("reconnect");
        let ReconnectResult::Snapshot(snapshot) = reconnect else {
            panic!("expected snapshot fallback");
        };
        assert_eq!(snapshot.snapshots.len(), 2);
    }

    #[test]
    fn duplicate_relay_message_is_ignored_by_client_mirror() {
        let host = test_host(8);
        let session = seed_session(&host);
        let (epoch, _) = host.initialize("client", &[], None).expect("initialize");
        let DispatchOutcome::Accepted(envelope) = host
            .dispatch(
                "client",
                epoch,
                title_dispatch(session.resource.clone(), 1, "Mirrored"),
            )
            .expect("dispatch")
        else {
            panic!("dispatch should be accepted");
        };

        let action = envelope.clone().into_action_envelope();
        let mut mirror = RemoteClientMirror::default();
        assert_eq!(mirror.apply_action(&action), MirrorApplyOutcome::Applied);
        assert_eq!(mirror.apply_action(&action), MirrorApplyOutcome::Duplicate);
        assert_eq!(mirror.session_titles[&session.resource], "Mirrored");
    }

    #[test]
    fn stale_connection_epoch_is_rejected() {
        let host = test_host(8);
        let session = seed_session(&host);
        let (old_epoch, _) = host.initialize("client", &[], None).expect("initialize");
        let (new_epoch, _) = host.initialize("client", &[], None).expect("reinitialize");
        assert!(new_epoch > old_epoch);

        let DispatchOutcome::Rejected(envelope) = host
            .dispatch(
                "client",
                old_epoch,
                title_dispatch(session.resource, 1, "Stale"),
            )
            .expect("dispatch")
        else {
            panic!("old connection must be rejected");
        };
        assert_eq!(
            envelope.rejection_reason.as_deref(),
            Some("stale connection epoch")
        );
    }

    #[test]
    fn reconnect_resets_client_sequence_for_the_new_epoch() {
        let host = test_host(8);
        let session = seed_session(&host);
        let (first_epoch, initialized) = host.initialize("client", &[], None).expect("initialize");
        assert!(matches!(
            host.dispatch(
                "client",
                first_epoch,
                title_dispatch(session.resource.clone(), 1, "First connection")
            )
            .expect("first dispatch"),
            DispatchOutcome::Accepted(_)
        ));

        let (second_epoch, _) = host
            .reconnect("client", initialized.server_seq, &[], None)
            .expect("reconnect");
        assert!(matches!(
            host.dispatch(
                "client",
                second_epoch,
                title_dispatch(session.resource, 1, "Second connection")
            )
            .expect("dispatch after reconnect"),
            DispatchOutcome::Accepted(_)
        ));
    }

    #[cfg(windows)]
    #[test]
    fn file_store_excludes_a_second_host_process() {
        let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("remote-agent-tests");
        let path = directory.join(format!("lock-{}.json", uuid::Uuid::new_v4()));
        let first = FileHostStateStore::new(path.clone()).expect("lock first store");
        let second = FileHostStateStore::new(path.clone());
        assert!(second.is_err());
        drop(first);
        FileHostStateStore::new(path.clone()).expect("lock released store");
        let _ = std::fs::remove_file(path.with_extension("lock"));
    }

    #[test]
    fn process_restart_keeps_identity_catalogue_and_uses_snapshot() {
        let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("remote-agent-tests");
        let path = directory.join(format!("restart-{}.json", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_file(&path);

        let first_store: Arc<dyn HostStateStore> =
            Arc::new(FileHostStateStore::new(path.clone()).expect("lock first store"));
        let first = RemoteAgentHost::load(first_store, 8, "test-token").expect("create first host");
        let host_id = first.host_id();
        let session = seed_session(&first);
        drop(first);

        let second_store: Arc<dyn HostStateStore> =
            Arc::new(FileHostStateStore::new(path.clone()).expect("lock second store"));
        let second = RemoteAgentHost::load(second_store, 8, "test-token").expect("reload host");
        assert_eq!(second.host_id(), host_id);
        assert_eq!(
            second
                .list_sessions(ListSessionsParams {
                    channel: ahp_types::ROOT_RESOURCE_URI.to_string(),
                    limit: None,
                    cursor: None,
                })
                .expect("list reloaded sessions")
                .items
                .len(),
            1
        );
        let (_, reconnect) = second
            .reconnect("client", 0, &[session.resource], None)
            .expect("reconnect after restart");
        assert!(matches!(reconnect, ReconnectResult::Snapshot(_)));
        drop(second);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn catalogue_is_refetched_after_reconnect() {
        let host = test_host(8);
        let first = seed_session(&host);
        let mut mirror = RemoteClientMirror::default();
        let first_page = host
            .list_sessions(ListSessionsParams {
                channel: ahp_types::ROOT_RESOURCE_URI.to_string(),
                limit: None,
                cursor: None,
            })
            .expect("initial catalogue");
        mirror.refetch_catalogue(first_page.items);
        assert!(mirror.catalogue.contains_key(&first.resource));

        let second = host
            .ingest_legacy_summary(LegacySessionSummary::new(
                "legacy-test".to_string(),
                "session-2".to_string(),
                "Second".to_string(),
            ))
            .expect("ingest second");
        assert!(!mirror
            .catalogue
            .contains_key(&second.session.resource_uri()));

        let refreshed = host
            .list_sessions(ListSessionsParams {
                channel: ahp_types::ROOT_RESOURCE_URI.to_string(),
                limit: None,
                cursor: None,
            })
            .expect("refetched catalogue");
        mirror.refetch_catalogue(refreshed.items);
        assert_eq!(mirror.catalogue.len(), 2);
    }

    #[tokio::test]
    async fn loopback_sdk_client_runs_vertical_slice() {
        let host = test_host(8);
        seed_session(&host);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("test listener address");
        let server = tokio::spawn(super::host::serve_listener(host, listener));

        let report =
            super::mirror::run_diagnostic(address, "integration-client", None, "test-token")
                .await
                .expect("run local diagnostic client");
        assert_eq!(report.initial_session_count, 1);
        assert!(report.initial_subscription_snapshot);
        assert_eq!(report.reconnect_kind, "replay");
        server.abort();
    }

    #[tokio::test]
    async fn loopback_transport_rejects_the_wrong_token() {
        let host = test_host(8);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("test listener address");
        let server = tokio::spawn(super::host::serve_listener(host, listener));

        let result =
            super::mirror::run_diagnostic(address, "unauthorized-client", None, "wrong-token")
                .await;
        assert!(result.is_err());
        server.abort();
    }

    #[tokio::test]
    async fn long_lived_client_receives_catalogue_updates() {
        let host = test_host(8);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("test listener address");
        let server = tokio::spawn(super::host::serve_listener(host.clone(), listener));

        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let watcher = tokio::spawn(async move {
            super::client::watch(
                super::client::RemoteClientConfig {
                    address,
                    client_id: "watch-client".to_string(),
                    auth_token: "test-token".to_string(),
                    event_limit: Some(3),
                    ..super::client::RemoteClientConfig::default()
                },
                move |event| {
                    event_tx.send(event.clone()).expect("capture client event");
                    Ok(())
                },
            )
            .await
        });

        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
                .await
                .expect("connected event timeout"),
            Some(super::client::RemoteClientEvent::Connected { .. })
        ));
        host.ingest_legacy_summary(LegacySessionSummary::new(
            "legacy-test".to_string(),
            "watched-session".to_string(),
            "Watched".to_string(),
        ))
        .expect("ingest watched summary");

        let action = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
            .await
            .expect("action event timeout")
            .expect("action event");
        assert!(matches!(
            action,
            super::client::RemoteClientEvent::Action { .. }
        ));
        let update = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
            .await
            .expect("catalogue event timeout")
            .expect("catalogue event");
        assert!(
            matches!(
                &update,
                super::client::RemoteClientEvent::CatalogueChanged { sessions, .. }
                    if sessions.iter().any(|session| session.title == "Watched")
            ),
            "unexpected remote client event: {update:?}"
        );
        watcher
            .await
            .expect("watch task should finish")
            .expect("watch should succeed");
        server.abort();
    }

    #[tokio::test]
    async fn terminal_bridge_reconciles_and_marks_remote_catalogue() {
        let registry = super::session_registry_for_test();
        let summary: ahp_types::state::SessionSummary = serde_json::from_value(serde_json::json!({
            "provider": "copilot",
            "title": "Remote build fix",
            "status": ahp_types::state::SessionStatus::InputNeeded.bits(),
            "activity": "Approve terminal command",
            "workingDirectories": ["file:///C:/remote/repo"],
            "resource": "ahp-session:/remote-1",
            "createdAt": "2026-08-10T00:00:00Z",
            "modifiedAt": "2026-08-10T01:00:00Z"
        }))
        .expect("deserialize remote session summary");
        let event = super::client::RemoteClientEvent::Connected {
            address: "127.0.0.1:8787".to_string(),
            recovery: "initialize",
            server_seq: 1,
            sessions: vec![summary],
        };

        assert!(
            super::apply_terminal_client_event(&*registry, "127.0.0.1:8787", &event).await
        );
        let rows = registry.snapshot().await;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].status,
            Some(crate::agent_sessions::AgentStatus::Attention)
        );
        assert!(matches!(
            &rows[0].location,
            crate::agent_sessions::SessionLocation::Remote { host, resource }
                if host == "127.0.0.1:8787" && resource == "ahp-session:/remote-1"
        ));

        let empty = super::client::RemoteClientEvent::CatalogueChanged {
            reason: "sessionRemoved",
            sessions: Vec::new(),
        };
        assert!(
            super::apply_terminal_client_event(&*registry, "127.0.0.1:8787", &empty).await
        );
        assert!(registry.snapshot().await.is_empty());
    }

    #[tokio::test]
    async fn terminal_bridge_marks_remote_rows_disconnected() {
        let registry = super::session_registry_for_test();
        let summary: ahp_types::state::SessionSummary = serde_json::from_value(serde_json::json!({
            "provider": "copilot",
            "title": "Remote session",
            "status": ahp_types::state::SessionStatus::Idle.bits(),
            "resource": "ahp-session:/remote-2",
            "createdAt": "2026-08-10T00:00:00Z",
            "modifiedAt": "2026-08-10T01:00:00Z"
        }))
        .expect("deserialize remote session summary");
        let connected = super::client::RemoteClientEvent::Connected {
            address: "127.0.0.1:8787".to_string(),
            recovery: "initialize",
            server_seq: 0,
            sessions: vec![summary],
        };
        super::apply_terminal_client_event(&*registry, "127.0.0.1:8787", &connected).await;

        let disconnected = super::client::RemoteClientEvent::Disconnected {
            address: "127.0.0.1:8787".to_string(),
            retry_in_ms: 1000,
            error: "connection lost".to_string(),
        };
        assert!(
            super::apply_terminal_client_event(&*registry, "127.0.0.1:8787", &disconnected).await
        );
        let row = registry.snapshot().await.pop().expect("remote row remains");
        assert_eq!(row.status, Some(crate::agent_sessions::AgentStatus::Error));
        assert_eq!(row.last_error.as_deref(), Some("connection lost"));
    }
}

#[cfg(test)]
fn session_registry_for_test() -> Arc<dyn crate::session_registry::SessionRegistry> {
    crate::session_registry::InMemoryRegistry::shared()
}
