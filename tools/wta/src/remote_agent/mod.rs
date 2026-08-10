//! Executable local Remote Agent Host MVP.
//!
//! The subsystem is deliberately isolated from the established WTA
//! master/helper ACP path. It implements a small server-side AHP JSON-RPC
//! subset because the official `ahp` 0.7.0 crate supplies a client, reducers,
//! and transport abstraction but no server runtime.

pub(crate) mod legacy;

mod host;
mod mirror;
mod persistence;
mod relay;
mod replay;
mod snapshot;
mod state;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};

use persistence::FileHostStateStore;

/// Start the standalone loopback host. This path is never entered by existing
/// master/helper launches, so their behavior remains unchanged.
pub(crate) async fn run_host(
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
    let host = host::RemoteAgentHost::load(store, replay_capacity, &auth_token)?;
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

    use ahp_types::actions::{SessionTitleChangedAction, StateAction};
    use ahp_types::commands::{DispatchActionParams, ListSessionsParams, ReconnectResult};
    use tokio::net::TcpListener;

    use super::host::RemoteAgentHost;
    use super::legacy::{LegacySessionSummary, LegacySessionSummarySink};
    use super::mirror::{MirrorApplyOutcome, RemoteClientMirror};
    use super::persistence::{FileHostStateStore, HostStateStore, MemoryHostStateStore};
    use super::state::DispatchOutcome;
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
}
