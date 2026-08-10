//! Standalone loopback AHP server and authoritative host coordinator.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ahp_types::commands::{
    DispatchActionParams, Implementation, InitializeParams, InitializeResult, ListSessionsParams,
    ListSessionsResult, ReconnectParams, ReconnectReplayResult, ReconnectResult,
    ReconnectSnapshotResult, SubscribeParams, SubscribeResult, UnsubscribeParams,
};
use ahp_types::messages::{
    JsonRpcError, JsonRpcErrorResponse, JsonRpcMessage, JsonRpcNotification,
    JsonRpcSuccessResponse, JsonRpcVersion,
};
use ahp_types::notifications::{SessionAddedParams, SessionSummaryChangedParams};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use super::legacy::{LegacySessionSummary, LegacySessionSummarySink};
use super::persistence::HostStateStore;
use super::relay::{LocalFramedTransport, RelayEnvelope, RelayTransport};
use super::replay::ReplayBuffer;
use super::snapshot::{list_session_summaries, session_summary, snapshot_for, snapshots_for};
use super::state::{DispatchOutcome, HostState, LegacyIngestOutcome};

const METHOD_NOT_FOUND: i32 = -32601;
const INVALID_PARAMS: i32 = -32602;
const UNSUPPORTED_PROTOCOL_VERSION: i32 = -32005;
const OUTBOUND_QUEUE_CAPACITY: usize = 64;
const MAX_CONNECTIONS: usize = 64;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone)]
struct Subscriber {
    subscriptions: BTreeSet<String>,
    sender: mpsc::Sender<JsonRpcMessage>,
}

/// Runtime coordinator around the pure [`HostState`] reducer.
pub(crate) struct RemoteAgentHost {
    state: Mutex<HostState>,
    replay: Mutex<ReplayBuffer>,
    commit: Mutex<()>,
    store: Arc<dyn HostStateStore>,
    subscribers: Mutex<HashMap<u64, Subscriber>>,
    next_connection_id: AtomicU64,
    auth_token_hash: [u8; 32],
}

impl RemoteAgentHost {
    pub(crate) fn load(
        store: Arc<dyn HostStateStore>,
        replay_capacity: usize,
        auth_token: &str,
    ) -> Result<Arc<Self>> {
        anyhow::ensure!(!auth_token.is_empty(), "Remote Agent Host token is empty");
        let state = match store.load()? {
            Some(state) => HostState::from_persisted(state),
            None => HostState::new(),
        };
        // Persist a generated host ID before listening, so an immediate
        // restart retains its identity even before a client mutates state.
        store.save(&state.persisted())?;
        Ok(Arc::new(Self {
            state: Mutex::new(state),
            replay: Mutex::new(ReplayBuffer::new(replay_capacity)),
            commit: Mutex::new(()),
            store,
            subscribers: Mutex::new(HashMap::new()),
            next_connection_id: AtomicU64::new(1),
            auth_token_hash: Sha256::digest(auth_token.as_bytes()).into(),
        }))
    }

    pub(crate) fn host_id(&self) -> String {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .host_id()
            .0
            .clone()
    }

    fn register_connection(&self, sender: mpsc::Sender<JsonRpcMessage>) -> u64 {
        let connection_id = self.next_connection_id.fetch_add(1, Ordering::Relaxed);
        self.subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                connection_id,
                Subscriber {
                    subscriptions: BTreeSet::new(),
                    sender,
                },
            );
        connection_id
    }

    fn unregister_connection(&self, connection_id: u64) {
        self.subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&connection_id);
    }

    fn set_subscriptions(&self, connection_id: u64, subscriptions: &BTreeSet<String>) {
        if let Some(subscriber) = self
            .subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(&connection_id)
        {
            subscriber.subscriptions = subscriptions.clone();
        }
    }

    pub(super) fn initialize(
        &self,
        client_id: &str,
        subscriptions: &[String],
        connection_id: Option<u64>,
    ) -> Result<(u64, InitializeResult)> {
        let _commit = self
            .commit
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let epoch = state.connect_client(client_id);
        let (snapshots, _) = snapshots_for(&state, subscriptions);
        let server_seq = i64::try_from(state.server_seq()).unwrap_or(i64::MAX);
        self.store.save(&state.persisted())?;
        if let Some(connection_id) = connection_id {
            self.set_subscriptions(connection_id, &subscriptions.iter().cloned().collect());
        }
        drop(state);

        Ok((
            epoch,
            InitializeResult {
                protocol_version: ahp_types::PROTOCOL_VERSION.to_string(),
                server_seq,
                server_info: Some(Implementation {
                    name: "Intelligent Terminal Remote Agent Host".to_string(),
                    version: Some(env!("CARGO_PKG_VERSION").to_string()),
                    title: Some("Intelligent Terminal".to_string()),
                }),
                snapshots,
                default_directory: None,
                completion_trigger_characters: None,
                terminal_command_prefix: None,
                telemetry: None,
            },
        ))
    }

    pub(super) fn reconnect(
        &self,
        client_id: &str,
        last_seen_server_seq: i64,
        subscriptions: &[String],
        connection_id: Option<u64>,
    ) -> Result<(u64, ReconnectResult)> {
        let _commit = self
            .commit
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let epoch = state.connect_client(client_id);
        let current_server_seq = state.server_seq();
        let (_, missing) = snapshots_for(&state, subscriptions);
        let snapshots = snapshots_for(&state, subscriptions).0;
        self.store.save(&state.persisted())?;
        if let Some(connection_id) = connection_id {
            self.set_subscriptions(connection_id, &subscriptions.iter().cloned().collect());
        }
        drop(state);

        let last_seen_server_seq = u64::try_from(last_seen_server_seq).unwrap_or(0);
        let replay = self
            .replay
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .replay_since(last_seen_server_seq, current_server_seq, subscriptions);

        let result = match replay {
            Some(envelopes) => ReconnectResult::Replay(ReconnectReplayResult {
                actions: envelopes
                    .into_iter()
                    .map(RelayEnvelope::into_action_envelope)
                    .collect(),
                missing,
            }),
            None => ReconnectResult::Snapshot(ReconnectSnapshotResult { snapshots }),
        };
        Ok((epoch, result))
    }

    fn subscribe(&self, connection_id: u64, resource: &str) -> Option<SubscribeResult> {
        let _commit = self
            .commit
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        snapshot_for(&state, resource).map(|snapshot| {
            let mut subscriptions = self
                .subscribers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(subscriber) = subscriptions.get_mut(&connection_id) {
                subscriber.subscriptions.insert(resource.to_string());
            }
            SubscribeResult {
                snapshot: Some(snapshot),
            }
        })
    }

    pub(super) fn list_sessions(&self, params: ListSessionsParams) -> Result<ListSessionsResult> {
        anyhow::ensure!(
            params.channel == ahp_types::ROOT_RESOURCE_URI,
            "listSessions must target the root resource"
        );
        let offset = match params.cursor {
            Some(cursor) => cursor
                .parse::<usize>()
                .with_context(|| "invalid listSessions cursor")?,
            None => 0,
        };
        let limit = params.limit.unwrap_or(100).clamp(1, 100) as usize;
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let summaries = list_session_summaries(&state);
        anyhow::ensure!(
            offset <= summaries.len(),
            "listSessions cursor is out of range"
        );
        let end = offset.saturating_add(limit).min(summaries.len());
        Ok(ListSessionsResult {
            next_cursor: (end < summaries.len()).then(|| end.to_string()),
            items: summaries[offset..end].to_vec(),
        })
    }

    pub(super) fn dispatch(
        &self,
        client_id: &str,
        connection_epoch: u64,
        params: DispatchActionParams,
    ) -> Result<DispatchOutcome> {
        let _commit = self
            .commit
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let outcome = state
            .dispatch(client_id, connection_epoch, params)
            .map_err(anyhow::Error::msg)?;
        self.store.save(&state.persisted())?;
        let envelope = match &outcome {
            DispatchOutcome::Accepted(envelope) | DispatchOutcome::Rejected(envelope) => {
                envelope.clone()
            }
        };
        let changed_summary = matches!(outcome, DispatchOutcome::Accepted(_))
            .then(|| state.session(&envelope.channel).map(session_summary))
            .flatten();
        let action_message = action_message(envelope.clone())?;
        let catalogue_message = changed_summary
            .as_ref()
            .map(session_summary_changed_message)
            .transpose()?;
        self.replay
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(envelope.clone());
        self.publish_to_subscribers(&envelope.channel, action_message);
        if let Some(message) = catalogue_message {
            self.publish_to_subscribers(ahp_types::ROOT_RESOURCE_URI, message);
        }
        Ok(outcome)
    }

    fn legacy_catalogue_message(&self, outcome: &LegacyIngestOutcome) -> Result<JsonRpcMessage> {
        if outcome.added {
            Ok(JsonRpcMessage::Notification(JsonRpcNotification {
                jsonrpc: JsonRpcVersion::V2,
                method: "root/sessionAdded".to_string(),
                params: Some(
                    serde_json::to_value(SessionAddedParams {
                        channel: ahp_types::ROOT_RESOURCE_URI.to_string(),
                        summary: session_summary(&outcome.session),
                    })
                    .context("serialize root/sessionAdded notification")?,
                ),
            }))
        } else {
            Ok(JsonRpcMessage::Notification(JsonRpcNotification {
                jsonrpc: JsonRpcVersion::V2,
                method: "root/sessionSummaryChanged".to_string(),
                params: Some(
                    serde_json::to_value(SessionSummaryChangedParams {
                        channel: ahp_types::ROOT_RESOURCE_URI.to_string(),
                        session: outcome.session.resource_uri(),
                        changes: ahp_types::notifications::PartialSessionSummary {
                            provider: None,
                            title: Some(outcome.session.title.clone()),
                            status: Some(outcome.session.status),
                            activity: outcome.session.activity.clone(),
                            project: None,
                            working_directories: None,
                            annotations: None,
                            resource: None,
                            created_at: None,
                            modified_at: Some(outcome.session.modified_at.clone()),
                            changes: None,
                            meta: None,
                        },
                    })
                    .context("serialize root/sessionSummaryChanged notification")?,
                ),
            }))
        }
    }

    fn publish_to_subscribers(&self, channel: &str, message: JsonRpcMessage) {
        let mut subscribers = self
            .subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let slow_connections: Vec<_> = subscribers
            .iter()
            .filter(|(_, subscriber)| subscriber.subscriptions.contains(channel))
            .filter_map(|(connection_id, subscriber)| {
                subscriber
                    .sender
                    .try_send(message.clone())
                    .err()
                    .map(|_| *connection_id)
            })
            .collect();
        for connection_id in slow_connections {
            subscribers.remove(&connection_id);
        }
    }

    fn authenticate(&self, token: &str) -> bool {
        let candidate: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        self.auth_token_hash
            .iter()
            .zip(candidate)
            .fold(0_u8, |difference, (expected, actual)| {
                difference | (expected ^ actual)
            })
            == 0
    }
}

impl LegacySessionSummarySink for RemoteAgentHost {
    fn ingest_legacy_summary(&self, summary: LegacySessionSummary) -> Result<LegacyIngestOutcome> {
        summary.validate()?;
        let _commit = self
            .commit
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let outcome = state.ingest_legacy(summary).map_err(anyhow::Error::msg)?;
        self.store.save(&state.persisted())?;
        let action_message = action_message(outcome.envelope.clone())?;
        let catalogue_message = self.legacy_catalogue_message(&outcome)?;
        self.replay
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(outcome.envelope.clone());
        self.publish_to_subscribers(&outcome.envelope.channel, action_message);
        self.publish_to_subscribers(ahp_types::ROOT_RESOURCE_URI, catalogue_message);
        Ok(outcome)
    }
}

fn action_message(envelope: RelayEnvelope) -> Result<JsonRpcMessage> {
    Ok(JsonRpcMessage::Notification(JsonRpcNotification {
        jsonrpc: JsonRpcVersion::V2,
        method: "action".to_string(),
        params: Some(
            serde_json::to_value(envelope.into_action_envelope())
                .context("serialize AHP action notification")?,
        ),
    }))
}

fn session_summary_changed_message(
    summary: &ahp_types::state::SessionSummary,
) -> Result<JsonRpcMessage> {
    Ok(JsonRpcMessage::Notification(JsonRpcNotification {
        jsonrpc: JsonRpcVersion::V2,
        method: "root/sessionSummaryChanged".to_string(),
        params: Some(
            serde_json::to_value(SessionSummaryChangedParams {
                channel: ahp_types::ROOT_RESOURCE_URI.to_string(),
                session: summary.resource.clone(),
                changes: ahp_types::notifications::PartialSessionSummary {
                    provider: None,
                    title: Some(summary.title.clone()),
                    status: Some(summary.status),
                    activity: summary.activity.clone(),
                    project: None,
                    working_directories: None,
                    annotations: None,
                    resource: None,
                    created_at: None,
                    modified_at: Some(summary.modified_at.clone()),
                    changes: None,
                    meta: None,
                },
            })
            .context("serialize root/sessionSummaryChanged notification")?,
        ),
    }))
}

#[derive(Default)]
struct ConnectionContext {
    connection_id: u64,
    client_id: Option<String>,
    connection_epoch: Option<u64>,
    subscriptions: BTreeSet<String>,
}

/// Run the loopback server until its process is stopped.
pub(crate) async fn serve(host: Arc<RemoteAgentHost>, listen: std::net::SocketAddr) -> Result<()> {
    anyhow::ensure!(
        listen.ip().is_loopback(),
        "Remote Agent Host must bind to loopback"
    );
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("bind Remote Agent Host at {listen}"))?;
    serve_listener(host, listener).await
}

pub(crate) async fn serve_listener(
    host: Arc<RemoteAgentHost>,
    listener: TcpListener,
) -> Result<()> {
    let listen = listener
        .local_addr()
        .context("get Remote Agent Host listener address")?;
    tracing::info!(
        target = "remote_agent_host",
        host_id = %host.host_id(),
        %listen,
        "Remote Agent Host loopback server listening"
    );
    let permits = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));

    loop {
        let permit = permits
            .clone()
            .acquire_owned()
            .await
            .context("Remote Agent Host connection limiter closed")?;
        let (stream, peer) = listener
            .accept()
            .await
            .context("accept remote host client")?;
        let host = host.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let mut transport = LocalFramedTransport::from_stream(stream);
            let result = async {
                let token = tokio::time::timeout(HANDSHAKE_TIMEOUT, transport.receive_auth_token())
                    .await
                    .context("Remote Agent Host authentication timed out")??;
                anyhow::ensure!(
                    host.authenticate(&token),
                    "Remote Agent Host authentication failed"
                );
                serve_connection(host, transport).await
            }
            .await;
            if let Err(error) = result {
                tracing::debug!(target = "remote_agent_host", %peer, ?error, "remote client closed with error");
            }
        });
    }
}

async fn serve_connection<T: RelayTransport>(
    host: Arc<RemoteAgentHost>,
    mut transport: T,
) -> Result<()> {
    let (sender, mut receiver) = mpsc::channel(OUTBOUND_QUEUE_CAPACITY);
    let connection_id = host.register_connection(sender);
    let mut context = ConnectionContext {
        connection_id,
        ..ConnectionContext::default()
    };

    let result = async {
        loop {
            tokio::select! {
                outbound = receiver.recv() => {
                    match outbound {
                        Some(message) => transport.send_json(message).await?,
                        None => break,
                    }
                }
                inbound = tokio::time::timeout(IDLE_TIMEOUT, transport.recv_json()) => {
                    let Some(message) = inbound.context("Remote Agent Host connection timed out")?? else {
                        break;
                    };
                    if let Some(response) = handle_message(&host, &mut context, message).await {
                        transport.send_json(response).await?;
                    }
                }
            }
        }
        Ok(())
    }
    .await;

    host.unregister_connection(connection_id);
    result
}

async fn handle_message(
    host: &Arc<RemoteAgentHost>,
    context: &mut ConnectionContext,
    message: JsonRpcMessage,
) -> Option<JsonRpcMessage> {
    match message {
        JsonRpcMessage::Request(request) => {
            let id = request.id;
            match handle_command(host, context, &request.method, request.params).await {
                Ok(result) => Some(success_response(id, result)),
                Err(error) => Some(error_response(id, error.code, error.message, error.data)),
            }
        }
        JsonRpcMessage::Notification(notification) => {
            if let Err(error) =
                handle_notification(host, context, &notification.method, notification.params).await
            {
                tracing::debug!(target = "remote_agent_host", method = %notification.method, ?error, "ignored invalid remote host notification");
            }
            None
        }
        JsonRpcMessage::SuccessResponse(_) | JsonRpcMessage::ErrorResponse(_) => None,
    }
}

async fn handle_command(
    host: &Arc<RemoteAgentHost>,
    context: &mut ConnectionContext,
    method: &str,
    params: Option<serde_json::Value>,
) -> std::result::Result<serde_json::Value, RpcFailure> {
    let params = params.unwrap_or(serde_json::Value::Null);
    match method {
        "initialize" => {
            require_new_connection(context)?;
            let params: InitializeParams =
                serde_json::from_value(params).context("parse initialize")?;
            if params.channel != ahp_types::ROOT_RESOURCE_URI {
                return Err(RpcFailure::invalid_params(
                    "initialize must target the root resource",
                ));
            }
            if !params
                .protocol_versions
                .iter()
                .any(|version| version == ahp_types::PROTOCOL_VERSION)
            {
                return Err(RpcFailure {
                    code: UNSUPPORTED_PROTOCOL_VERSION,
                    message: "unsupported AHP protocol version".to_string(),
                    data: Some(serde_json::json!({
                        "supportedVersions": [ahp_types::PROTOCOL_VERSION],
                    })),
                });
            }
            let subscriptions = params.initial_subscriptions.unwrap_or_default();
            let (epoch, result) = host.initialize(
                &params.client_id,
                &subscriptions,
                Some(context.connection_id),
            )?;
            context.client_id = Some(params.client_id);
            context.connection_epoch = Some(epoch);
            context.subscriptions = subscriptions.into_iter().collect();
            Ok(serde_json::to_value(result).context("serialize initialize result")?)
        }
        "reconnect" => {
            require_new_connection(context)?;
            let params: ReconnectParams =
                serde_json::from_value(params).context("parse reconnect")?;
            let (epoch, result) = host.reconnect(
                &params.client_id,
                params.last_seen_server_seq,
                &params.subscriptions,
                Some(context.connection_id),
            )?;
            context.client_id = Some(params.client_id);
            context.connection_epoch = Some(epoch);
            context.subscriptions = params.subscriptions.into_iter().collect();
            Ok(serde_json::to_value(result).context("serialize reconnect result")?)
        }
        "subscribe" => {
            require_connection(context)?;
            let params: SubscribeParams =
                serde_json::from_value(params).context("parse subscribe")?;
            let result = host
                .subscribe(context.connection_id, &params.channel)
                .context("unknown subscription resource")?;
            context.subscriptions.insert(params.channel);
            Ok(serde_json::to_value(result).context("serialize subscribe result")?)
        }
        "listSessions" => {
            require_connection(context)?;
            let params: ListSessionsParams =
                serde_json::from_value(params).context("parse listSessions")?;
            Ok(serde_json::to_value(host.list_sessions(params)?)
                .context("serialize listSessions result")?)
        }
        "dispatchAction" => {
            require_connection(context)?;
            let params: DispatchActionParams =
                serde_json::from_value(params).context("parse dispatchAction")?;
            dispatch(host, context, params)?;
            Ok(serde_json::json!({}))
        }
        "wta/ingestLegacySession" => {
            require_connection(context)?;
            let summary: LegacySessionSummary =
                serde_json::from_value(params).context("parse legacy session summary")?;
            let outcome = host.ingest_legacy_summary(summary)?;
            Ok(serde_json::to_value(session_summary(&outcome.session))
                .context("serialize ingested session summary")?)
        }
        "ping" => {
            require_connection(context)?;
            Ok(serde_json::json!({}))
        }
        _ => Err(RpcFailure {
            code: METHOD_NOT_FOUND,
            message: format!("method not found: {method}"),
            data: None,
        }),
    }
}

async fn handle_notification(
    host: &Arc<RemoteAgentHost>,
    context: &mut ConnectionContext,
    method: &str,
    params: Option<serde_json::Value>,
) -> Result<()> {
    let params = params.unwrap_or(serde_json::Value::Null);
    match method {
        "dispatchAction" => {
            require_connection(context)?;
            let params: DispatchActionParams =
                serde_json::from_value(params).context("parse dispatchAction")?;
            dispatch(host, context, params)?;
            Ok(())
        }
        "unsubscribe" => {
            require_connection(context)?;
            let params: UnsubscribeParams =
                serde_json::from_value(params).context("parse unsubscribe")?;
            context.subscriptions.remove(&params.channel);
            host.set_subscriptions(context.connection_id, &context.subscriptions);
            Ok(())
        }
        _ => anyhow::bail!("method not found: {method}"),
    }
}

fn require_new_connection(context: &ConnectionContext) -> Result<()> {
    anyhow::ensure!(
        context.client_id.is_none(),
        "connection is already initialized"
    );
    Ok(())
}

fn dispatch(
    host: &RemoteAgentHost,
    context: &ConnectionContext,
    params: DispatchActionParams,
) -> Result<()> {
    let (client_id, epoch) = require_connection(context)?;
    let _ = host.dispatch(client_id, epoch, params)?;
    Ok(())
}

fn require_connection(context: &ConnectionContext) -> Result<(&str, u64)> {
    let client_id = context
        .client_id
        .as_deref()
        .context("initialize or reconnect is required first")?;
    let epoch = context
        .connection_epoch
        .context("connection epoch is unavailable")?;
    Ok((client_id, epoch))
}

fn success_response(id: u64, result: serde_json::Value) -> JsonRpcMessage {
    JsonRpcMessage::SuccessResponse(JsonRpcSuccessResponse {
        jsonrpc: JsonRpcVersion::V2,
        id,
        result,
    })
}

fn error_response(
    id: u64,
    code: i32,
    message: String,
    data: Option<serde_json::Value>,
) -> JsonRpcMessage {
    JsonRpcMessage::ErrorResponse(JsonRpcErrorResponse {
        jsonrpc: JsonRpcVersion::V2,
        id,
        error: JsonRpcError {
            code,
            message,
            data,
        },
    })
}

struct RpcFailure {
    code: i32,
    message: String,
    data: Option<serde_json::Value>,
}

impl RpcFailure {
    fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: INVALID_PARAMS,
            message: message.into(),
            data: None,
        }
    }
}

impl From<anyhow::Error> for RpcFailure {
    fn from(error: anyhow::Error) -> Self {
        Self::invalid_params(error.to_string())
    }
}
