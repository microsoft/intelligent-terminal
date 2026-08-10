//! Client mirror and reconnect diagnostics for the Remote Agent Host MVP.

use std::collections::BTreeMap;

use ahp::reducers::apply_action_to_root;
use ahp::{Client, ClientConfig};
use ahp_types::actions::{ActionEnvelope, StateAction};
use ahp_types::commands::{ListSessionsParams, ListSessionsResult, ReconnectResult};
use ahp_types::state::{RootState, SessionSummary, Snapshot, SnapshotState};
use anyhow::{Context, Result};
use serde::Serialize;

use super::relay::LocalFramedTransport;

/// Result of applying a relay event to a local mirror.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MirrorApplyOutcome {
    Applied,
    Duplicate,
}

/// Minimal client-side state mirror. It uses the official AHP reducer for root
/// actions and retains the root catalogue separately because catalogue
/// notifications are ephemeral by AHP design.
#[derive(Clone, Debug)]
pub(crate) struct RemoteClientMirror {
    pub(crate) last_seen_server_seq: u64,
    pub(crate) root: RootState,
    pub(crate) catalogue: BTreeMap<String, SessionSummary>,
    pub(crate) session_titles: BTreeMap<String, String>,
}

impl Default for RemoteClientMirror {
    fn default() -> Self {
        Self {
            last_seen_server_seq: 0,
            root: RootState {
                agents: Vec::new(),
                active_sessions: None,
                terminals: None,
                config: None,
                meta: None,
            },
            catalogue: BTreeMap::new(),
            session_titles: BTreeMap::new(),
        }
    }
}

impl RemoteClientMirror {
    /// Applies one replay/live envelope. Repeated envelopes are ignored,
    /// allowing at-least-once relay delivery without duplicating state.
    pub(crate) fn apply_action(&mut self, envelope: &ActionEnvelope) -> MirrorApplyOutcome {
        if envelope.server_seq <= self.last_seen_server_seq {
            return MirrorApplyOutcome::Duplicate;
        }

        let _ = apply_action_to_root(&mut self.root, &envelope.action);
        if let StateAction::SessionTitleChanged(action) = &envelope.action {
            self.session_titles
                .insert(envelope.channel.clone(), action.title.clone());
        }
        self.last_seen_server_seq = envelope.server_seq;
        MirrorApplyOutcome::Applied
    }

    pub(crate) fn apply_snapshot(&mut self, snapshot: &Snapshot) {
        if let SnapshotState::Root(root) = &snapshot.state {
            self.root = (**root).clone();
        }
        self.last_seen_server_seq = self
            .last_seen_server_seq
            .max(u64::try_from(snapshot.from_seq).unwrap_or(0));
    }

    /// Root session notifications are not replayed. Always replace the
    /// catalogue from `listSessions` after reconnect rather than attempting to
    /// infer it from root actions.
    pub(crate) fn refetch_catalogue(&mut self, sessions: Vec<SessionSummary>) {
        self.catalogue = sessions
            .into_iter()
            .map(|session| (session.resource.clone(), session))
            .collect();
    }
}

/// Human- and machine-readable result from `wta remote-host diagnose`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DiagnosticReport {
    pub(crate) host_address: String,
    pub(crate) client_id: String,
    pub(crate) initialized_server_seq: i64,
    pub(crate) initial_subscription_snapshot: bool,
    pub(crate) initial_session_count: usize,
    pub(crate) reconnect_last_seen_server_seq: i64,
    pub(crate) reconnect_kind: &'static str,
    pub(crate) replayed_actions: usize,
    pub(crate) reconnect_snapshots: usize,
    pub(crate) catalogue_refetched_count: usize,
}

pub(crate) async fn run_diagnostic(
    address: std::net::SocketAddr,
    client_id: &str,
    requested_last_seen: Option<i64>,
    auth_token: &str,
) -> Result<DiagnosticReport> {
    let root = ahp_types::ROOT_RESOURCE_URI.to_string();
    let transport = LocalFramedTransport::connect(address, auth_token).await?;
    let client = Client::connect(transport, ClientConfig::default())
        .await
        .context("start AHP diagnostic client")?;
    let initialize = client
        .initialize(
            client_id.to_string(),
            vec![ahp_types::PROTOCOL_VERSION.to_string()],
            vec![root.clone()],
        )
        .await
        .context("initialize Remote Agent Host")?;
    let mut mirror = RemoteClientMirror::default();
    for snapshot in &initialize.snapshots {
        mirror.apply_snapshot(snapshot);
    }
    let (subscription, _) = client
        .subscribe(root.clone())
        .await
        .context("subscribe diagnostic client to root")?;
    if let Some(snapshot) = &subscription.snapshot {
        mirror.apply_snapshot(snapshot);
    }
    let initial_sessions: ListSessionsResult = client
        .request(
            "listSessions",
            ListSessionsParams {
                channel: root.clone(),
                limit: None,
                cursor: None,
            },
        )
        .await
        .context("list initial remote sessions")?;
    mirror.refetch_catalogue(initial_sessions.items.clone());

    let last_seen_server_seq = requested_last_seen
        .unwrap_or_else(|| i64::try_from(mirror.last_seen_server_seq).unwrap_or(i64::MAX));
    client.shutdown().await;
    drop(client);

    let transport = LocalFramedTransport::connect(address, auth_token).await?;
    let reconnected = Client::connect(transport, ClientConfig::default())
        .await
        .context("start reconnect diagnostic client")?;
    let reconnect = reconnected
        .reconnect(
            client_id.to_string(),
            last_seen_server_seq,
            vec![root.clone()],
        )
        .await
        .context("reconnect Remote Agent Host")?;

    let (reconnect_kind, replayed_actions, reconnect_snapshots) = match &reconnect {
        ReconnectResult::Replay(result) => {
            for action in &result.actions {
                let _ = mirror.apply_action(action);
            }
            ("replay", result.actions.len(), 0)
        }
        ReconnectResult::Snapshot(result) => {
            for snapshot in &result.snapshots {
                mirror.apply_snapshot(snapshot);
            }
            ("snapshot", 0, result.snapshots.len())
        }
    };

    // This refetch is mandatory, even after a replay: root/sessionAdded and
    // root/sessionSummaryChanged are intentionally ephemeral notifications.
    let refreshed_sessions: ListSessionsResult = reconnected
        .request(
            "listSessions",
            ListSessionsParams {
                channel: root,
                limit: None,
                cursor: None,
            },
        )
        .await
        .context("refetch remote session catalogue after reconnect")?;
    let catalogue_refetched_count = refreshed_sessions.items.len();
    mirror.refetch_catalogue(refreshed_sessions.items);
    reconnected.shutdown().await;

    Ok(DiagnosticReport {
        host_address: address.to_string(),
        client_id: client_id.to_string(),
        initialized_server_seq: initialize.server_seq,
        initial_subscription_snapshot: subscription.snapshot.is_some(),
        initial_session_count: initial_sessions.items.len(),
        reconnect_last_seen_server_seq: last_seen_server_seq,
        reconnect_kind,
        replayed_actions,
        reconnect_snapshots,
        catalogue_refetched_count,
    })
}

pub(crate) async fn run_legacy_ingest(
    address: std::net::SocketAddr,
    summary: super::legacy::LegacySessionSummary,
    auth_token: &str,
) -> Result<SessionSummary> {
    let transport = LocalFramedTransport::connect(address, auth_token).await?;
    let client = Client::connect(transport, ClientConfig::default())
        .await
        .context("start legacy ingestion client")?;
    client
        .initialize(
            format!("wta-legacy-ingest-{}", uuid::Uuid::new_v4()),
            vec![ahp_types::PROTOCOL_VERSION.to_string()],
            Vec::new(),
        )
        .await
        .context("initialize legacy ingestion client")?;
    let result: SessionSummary = client
        .request("wta/ingestLegacySession", summary)
        .await
        .context("ingest legacy session summary")?;
    client.shutdown().await;
    Ok(result)
}
