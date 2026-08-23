//! Standalone loopback AHP server and authoritative host coordinator.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ahp_types::commands::{
    CreateChatParams, CreateSessionParams, DispatchActionParams, DisposeSessionParams,
    Implementation, InitializeParams, InitializeResult, ListSessionsParams, ListSessionsResult,
    ReconnectParams, ReconnectReplayResult, ReconnectResult, ReconnectSnapshotResult,
    SubscribeParams, SubscribeResult, UnsubscribeParams,
};
use ahp_types::messages::{
    JsonRpcError, JsonRpcErrorResponse, JsonRpcMessage, JsonRpcNotification,
    JsonRpcSuccessResponse, JsonRpcVersion,
};
use ahp_types::notifications::{
    SessionAddedParams, SessionRemovedParams, SessionSummaryChangedParams,
};
use ahp_types::state::Snapshot;
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use super::acp_backend::{BackendEvent, HostAcpBackend};
use super::legacy::{LegacySessionSummary, LegacySessionSummarySink};
use super::persistence::HostStateStore;
use super::relay::{LocalFramedTransport, RelayEnvelope, RelayTransport};
use super::replay::ReplayBuffer;
use super::snapshot::{list_session_summaries, session_summary, snapshot_for, snapshots_for};
use super::state::{DispatchOutcome, DomainError, HostState, LegacyIngestOutcome};

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
    acp_backend: Option<HostAcpBackend>,
}

impl RemoteAgentHost {
    pub(crate) fn load(
        store: Arc<dyn HostStateStore>,
        replay_capacity: usize,
        auth_token: &str,
    ) -> Result<Arc<Self>> {
        Self::load_inner(store, replay_capacity, auth_token, None)
    }

    pub(crate) fn load_with_backend(
        store: Arc<dyn HostStateStore>,
        replay_capacity: usize,
        auth_token: &str,
        acp_backend: HostAcpBackend,
    ) -> Result<Arc<Self>> {
        Self::load_inner(store, replay_capacity, auth_token, Some(acp_backend))
    }

    fn load_inner(
        store: Arc<dyn HostStateStore>,
        replay_capacity: usize,
        auth_token: &str,
        acp_backend: Option<HostAcpBackend>,
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
            acp_backend,
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

    pub(super) fn begin_session_creation(
        &self,
        resource: &str,
        provider: String,
        working_directories: Option<Vec<String>>,
    ) -> Result<Snapshot> {
        let _commit = self
            .commit
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let outcome = state
            .begin_session_creation(resource, provider, working_directories)
            .map_err(anyhow::Error::msg)?;
        self.store.save(&state.persisted())?;
        let snapshot =
            snapshot_for(&state, resource).context("created session snapshot is unavailable")?;
        let action_message = action_message(outcome.envelope.clone())?;
        let catalogue_message = session_added_message(&outcome.session)?;
        self.replay
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(outcome.envelope.clone());
        self.publish_to_subscribers(&outcome.envelope.channel, action_message);
        self.publish_to_subscribers(ahp_types::ROOT_RESOURCE_URI, catalogue_message);
        Ok(snapshot)
    }

    pub(super) fn create_session(
        self: &Arc<Self>,
        resource: &str,
        provider: String,
        working_directories: Option<Vec<String>>,
    ) -> Result<Snapshot> {
        let snapshot =
            self.begin_session_creation(resource, provider.clone(), working_directories.clone())?;
        if let Some(backend) = self.acp_backend.clone() {
            let host = Arc::clone(self);
            let resource = resource.to_string();
            tokio::spawn(async move {
                match backend
                    .create_session(resource.clone(), provider, working_directories)
                    .await
                {
                    Ok(agent_session_id) => {
                        if let Err(error) =
                            host.complete_session_creation(&resource, agent_session_id)
                        {
                            tracing::error!(
                                target = "remote_agent_host",
                                %resource,
                                %error,
                                "failed to commit ACP session creation"
                            );
                        }
                    }
                    Err(error) => {
                        if let Err(commit_error) = host.fail_session_creation(
                            &resource,
                            "AcpSessionCreationFailed".to_string(),
                            error.to_string(),
                        ) {
                            tracing::error!(
                                target = "remote_agent_host",
                                %resource,
                                %commit_error,
                                "failed to commit ACP session creation failure"
                            );
                        }
                    }
                }
            });
        }
        Ok(snapshot)
    }

    pub(super) fn complete_session_creation(
        &self,
        resource: &str,
        agent_session_id: String,
    ) -> Result<()> {
        self.finish_session_creation(resource, Ok(agent_session_id))
    }

    pub(super) fn fail_session_creation(
        &self,
        resource: &str,
        error_type: String,
        message: String,
    ) -> Result<()> {
        self.finish_session_creation(resource, Err((error_type, message)))
    }

    pub(super) fn create_chat(
        &self,
        session_resource: &str,
        chat_resource: &str,
    ) -> Result<Snapshot> {
        let _commit = self
            .commit
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let outcome = state
            .create_chat(session_resource, chat_resource)
            .map_err(anyhow::Error::msg)?;
        self.store.save(&state.persisted())?;
        let snapshot =
            snapshot_for(&state, chat_resource).context("created chat snapshot is unavailable")?;
        let messages = outcome
            .envelopes
            .iter()
            .cloned()
            .map(action_message)
            .collect::<Result<Vec<_>>>()?;
        {
            let mut replay = self
                .replay
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for envelope in &outcome.envelopes {
                replay.push(envelope.clone());
            }
        }
        for message in messages {
            self.publish_to_subscribers(session_resource, message);
        }
        Ok(snapshot)
    }

    fn finish_session_creation(
        &self,
        resource: &str,
        result: std::result::Result<String, (String, String)>,
    ) -> Result<()> {
        let _commit = self
            .commit
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let outcome = match result {
            Ok(agent_session_id) => state.complete_session_creation(resource, agent_session_id),
            Err((error_type, message)) => {
                state.fail_session_creation(resource, error_type, message)
            }
        }
        .map_err(anyhow::Error::msg)?;
        self.store.save(&state.persisted())?;
        let action_message = action_message(outcome.envelope.clone())?;
        let catalogue_message =
            session_summary_changed_message(&session_summary(&outcome.session))?;
        self.replay
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(outcome.envelope.clone());
        self.publish_to_subscribers(&outcome.envelope.channel, action_message);
        self.publish_to_subscribers(ahp_types::ROOT_RESOURCE_URI, catalogue_message);
        Ok(())
    }

    pub(super) fn dispose_session(self: &Arc<Self>, resource: &str) -> Result<()> {
        let _commit = self
            .commit
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let outcome = state
            .dispose_session(resource)
            .map_err(anyhow::Error::msg)?;
        self.store.save(&state.persisted())?;
        let action_message = action_message(outcome.envelope.clone())?;
        let removed_message = session_removed_message(&outcome.resource)?;
        self.replay
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(outcome.envelope.clone());
        self.publish_to_subscribers(&outcome.envelope.channel, action_message);
        self.publish_to_subscribers(ahp_types::ROOT_RESOURCE_URI, removed_message);
        if let Some(backend) = self.acp_backend.clone() {
            let resource = resource.to_string();
            tokio::spawn(async move {
                if let Err(error) = backend.dispose_session(resource.clone()).await {
                    tracing::warn!(
                        target = "remote_agent_host",
                        %resource,
                        %error,
                        "failed to close disposed ACP session"
                    );
                }
            });
        }
        Ok(())
    }

    pub(super) fn dispatch(
        self: &Arc<Self>,
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
        let is_turn_started = matches!(
            params.action,
            ahp_types::actions::StateAction::ChatTurnStarted(_)
        );
        let is_turn_cancelled = matches!(
            params.action,
            ahp_types::actions::StateAction::ChatTurnCancelled(_)
        );
        let outcome = if is_turn_started {
            state.start_turn(client_id, connection_epoch, params)
        } else if is_turn_cancelled {
            state.cancel_turn(client_id, connection_epoch, params)
        } else {
            state.dispatch(client_id, connection_epoch, params)
        }
        .map_err(anyhow::Error::msg)?;
        let envelope = match &outcome {
            DispatchOutcome::Accepted(envelope) | DispatchOutcome::Rejected(envelope) => {
                envelope.clone()
            }
        };
        let chat_summary_envelope = if matches!(outcome, DispatchOutcome::Accepted(_))
            && (is_turn_started || is_turn_cancelled)
        {
            Some(
                state
                    .sync_chat_summary(&envelope.channel)
                    .map_err(anyhow::Error::msg)?,
            )
        } else {
            None
        };
        self.store.save(&state.persisted())?;
        let changed_summary = if let Some(summary) = &chat_summary_envelope {
            state.session(&summary.channel).map(session_summary)
        } else {
            state.session(&envelope.channel).map(session_summary)
        };
        let chat_summary_message = chat_summary_envelope
            .clone()
            .map(action_message)
            .transpose()?;
        let action_message = action_message(envelope.clone())?;
        let catalogue_message = changed_summary
            .as_ref()
            .map(session_summary_changed_message)
            .transpose()?;
        let prompt = if matches!(outcome, DispatchOutcome::Accepted(_)) && is_turn_started {
            state.chat(&envelope.channel).and_then(|chat| {
                chat.active_turn.as_ref().map(|turn| {
                    (
                        chat.session_resource_uri(),
                        turn.message.text.clone(),
                        chat.resource_uri(),
                    )
                })
            })
        } else {
            None
        };
        let cancelled_session =
            if matches!(outcome, DispatchOutcome::Accepted(_)) && is_turn_cancelled {
                state
                    .chat(&envelope.channel)
                    .map(|chat| chat.session_resource_uri())
            } else {
                None
            };
        self.replay
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(envelope.clone());
        self.publish_to_subscribers(&envelope.channel, action_message);
        if let Some(summary) = chat_summary_envelope {
            self.replay
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(summary.clone());
            if let Some(message) = chat_summary_message {
                self.publish_to_subscribers(&summary.channel, message);
            }
        }
        if let Some(message) = catalogue_message {
            self.publish_to_subscribers(ahp_types::ROOT_RESOURCE_URI, message);
        }
        if let (Some(backend), Some((session_resource, text, chat_resource))) =
            (self.acp_backend.clone(), prompt)
        {
            let host = Arc::clone(self);
            tokio::spawn(async move {
                if let Err(error) = backend.prompt(session_resource, text).await {
                    let _ = host.finish_active_turn_with_error(&chat_resource, error.to_string());
                }
            });
        }
        if let (Some(backend), Some(session_resource)) =
            (self.acp_backend.clone(), cancelled_session)
        {
            tokio::spawn(async move {
                if let Err(error) = backend.cancel(session_resource.clone()).await {
                    tracing::warn!(
                        target = "remote_agent_host",
                        %session_resource,
                        %error,
                        "failed to cancel Host ACP prompt"
                    );
                }
            });
        }
        Ok(outcome)
    }

    pub(crate) fn apply_backend_event(&self, event: BackendEvent) -> Result<()> {
        match event {
            BackendEvent::AgentText {
                agent_session_id,
                content,
            } => self.append_agent_text(&agent_session_id, content),
            BackendEvent::PromptFinished {
                agent_session_id,
                duration_ms,
                cancelled,
                error,
            } => {
                if cancelled {
                    self.cancel_agent_turn(&agent_session_id, duration_ms)
                } else {
                    self.finish_agent_turn(&agent_session_id, duration_ms, error)
                }
            }
            BackendEvent::ProviderExited {
                agent_session_ids,
                error,
            } => self.fail_agent_sessions(agent_session_ids, error),
        }
    }

    fn fail_agent_sessions(&self, agent_session_ids: Vec<String>, message: String) -> Result<()> {
        let _commit = self
            .commit
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut outcomes = Vec::new();
        for agent_session_id in agent_session_ids {
            match state.fail_agent_session(
                &agent_session_id,
                DomainError {
                    error_type: "AcpProviderExited".to_string(),
                    message: message.clone(),
                },
            ) {
                Ok(outcome) => outcomes.push(outcome),
                Err(error) if error == "unknown ACP session" => {
                    tracing::debug!(
                        target = "remote_agent_host",
                        %agent_session_id,
                        "ignored provider exit for a disposed ACP session"
                    );
                }
                Err(error) => return Err(anyhow::Error::msg(error)),
            }
        }
        if outcomes.is_empty() {
            return Ok(());
        }
        self.store.save(&state.persisted())?;
        let envelope_messages = outcomes
            .iter()
            .flat_map(|outcome| outcome.envelopes.iter())
            .map(|envelope| action_message(envelope.clone()).map(|message| (envelope, message)))
            .collect::<Result<Vec<_>>>()?;
        let catalogue_messages = outcomes
            .iter()
            .map(|outcome| session_summary_changed_message(&session_summary(&outcome.session)))
            .collect::<Result<Vec<_>>>()?;
        {
            let mut replay = self
                .replay
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for outcome in &outcomes {
                for envelope in &outcome.envelopes {
                    replay.push(envelope.clone());
                }
            }
        }
        for (envelope, message) in envelope_messages {
            self.publish_to_subscribers(&envelope.channel, message);
        }
        for message in catalogue_messages {
            self.publish_to_subscribers(ahp_types::ROOT_RESOURCE_URI, message);
        }
        Ok(())
    }

    fn append_agent_text(&self, agent_session_id: &str, content: String) -> Result<()> {
        let _commit = self
            .commit
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let outcomes = state
            .append_agent_text(agent_session_id, content)
            .map_err(anyhow::Error::msg)?;
        let summary = state
            .sync_chat_summary(&outcomes[0].envelope.channel)
            .map_err(anyhow::Error::msg)?;
        self.store.save(&state.persisted())?;
        let messages = outcomes
            .iter()
            .map(|outcome| action_message(outcome.envelope.clone()))
            .collect::<Result<Vec<_>>>()?;
        let summary_message = action_message(summary.clone())?;
        let catalogue_message = state
            .session(&summary.channel)
            .map(session_summary)
            .as_ref()
            .map(session_summary_changed_message)
            .transpose()?;
        {
            let mut replay = self
                .replay
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for outcome in &outcomes {
                replay.push(outcome.envelope.clone());
            }
            replay.push(summary.clone());
        }
        for (outcome, message) in outcomes.iter().zip(messages) {
            self.publish_to_subscribers(&outcome.envelope.channel, message);
        }
        self.publish_to_subscribers(&summary.channel, summary_message);
        if let Some(message) = catalogue_message {
            self.publish_to_subscribers(ahp_types::ROOT_RESOURCE_URI, message);
        }
        Ok(())
    }

    fn finish_agent_turn(
        &self,
        agent_session_id: &str,
        duration_ms: i64,
        error: Option<String>,
    ) -> Result<()> {
        let error = error.map(|message| DomainError {
            error_type: "AcpPromptFailed".to_string(),
            message,
        });
        let _commit = self
            .commit
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let outcome = state
            .finish_turn(agent_session_id, duration_ms, error)
            .map_err(anyhow::Error::msg)?;
        let summary = state
            .sync_chat_summary(&outcome.envelope.channel)
            .map_err(anyhow::Error::msg)?;
        self.store.save(&state.persisted())?;
        let message = action_message(outcome.envelope.clone())?;
        let summary_message = action_message(summary.clone())?;
        let catalogue_message = state
            .session(&summary.channel)
            .map(session_summary)
            .as_ref()
            .map(session_summary_changed_message)
            .transpose()?;
        self.replay
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(outcome.envelope.clone());
        self.replay
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(summary.clone());
        self.publish_to_subscribers(&outcome.envelope.channel, message);
        self.publish_to_subscribers(&summary.channel, summary_message);
        if let Some(message) = catalogue_message {
            self.publish_to_subscribers(ahp_types::ROOT_RESOURCE_URI, message);
        }
        Ok(())
    }

    fn cancel_agent_turn(&self, agent_session_id: &str, duration_ms: i64) -> Result<()> {
        let _commit = self
            .commit
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let outcome = state
            .cancel_agent_turn(agent_session_id, duration_ms)
            .map_err(anyhow::Error::msg)?;
        let summary = state
            .sync_chat_summary(&outcome.envelope.channel)
            .map_err(anyhow::Error::msg)?;
        self.store.save(&state.persisted())?;
        let message = action_message(outcome.envelope.clone())?;
        let summary_message = action_message(summary.clone())?;
        let catalogue_message = state
            .session(&summary.channel)
            .map(session_summary)
            .as_ref()
            .map(session_summary_changed_message)
            .transpose()?;
        self.replay
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(outcome.envelope.clone());
        self.replay
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(summary.clone());
        self.publish_to_subscribers(&outcome.envelope.channel, message);
        self.publish_to_subscribers(&summary.channel, summary_message);
        if let Some(message) = catalogue_message {
            self.publish_to_subscribers(ahp_types::ROOT_RESOURCE_URI, message);
        }
        Ok(())
    }

    fn finish_active_turn_with_error(&self, chat_resource: &str, message: String) -> Result<()> {
        let agent_session_id = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let chat = state
                .chat(chat_resource)
                .context("active turn is unavailable")?;
            anyhow::ensure!(chat.active_turn.is_some(), "active turn is unavailable");
            state
                .session(&chat.session_resource_uri())
                .and_then(|session| session.agent_session_id.clone())
                .context("active turn ACP session is unavailable")?
        };
        self.finish_agent_turn(&agent_session_id, 0, Some(message))
    }

    fn legacy_catalogue_message(&self, outcome: &LegacyIngestOutcome) -> Result<JsonRpcMessage> {
        if outcome.added {
            session_added_message(&outcome.session)
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

fn session_added_message(session: &super::state::DomainSession) -> Result<JsonRpcMessage> {
    Ok(JsonRpcMessage::Notification(JsonRpcNotification {
        jsonrpc: JsonRpcVersion::V2,
        method: "root/sessionAdded".to_string(),
        params: Some(
            serde_json::to_value(SessionAddedParams {
                channel: ahp_types::ROOT_RESOURCE_URI.to_string(),
                summary: session_summary(session),
            })
            .context("serialize root/sessionAdded notification")?,
        ),
    }))
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

fn session_removed_message(resource: &str) -> Result<JsonRpcMessage> {
    Ok(JsonRpcMessage::Notification(JsonRpcNotification {
        jsonrpc: JsonRpcVersion::V2,
        method: "root/sessionRemoved".to_string(),
        params: Some(
            serde_json::to_value(SessionRemovedParams {
                channel: ahp_types::ROOT_RESOURCE_URI.to_string(),
                session: resource.to_string(),
            })
            .context("serialize root/sessionRemoved notification")?,
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
        "createSession" => {
            let (client_id, _) = require_connection(context)?;
            let params: CreateSessionParams =
                serde_json::from_value(params).context("parse createSession")?;
            let provider = params
                .provider
                .context("createSession requires a provider")?;
            if params.fork.is_some() {
                return Err(RpcFailure::invalid_params(
                    "forked sessions are not supported yet",
                ));
            }
            if params.config.is_some() {
                return Err(RpcFailure::invalid_params(
                    "session configuration is not supported yet",
                ));
            }
            if let Some(active_client) = params.active_client {
                if active_client.client_id != client_id {
                    return Err(RpcFailure::invalid_params(
                        "createSession activeClient must match the initialized client",
                    ));
                }
                return Err(RpcFailure::invalid_params(
                    "activeClient is not supported yet",
                ));
            }
            let working_directories = params
                .working_directories
                .map(|directories| {
                    anyhow::ensure!(
                        directories.len() <= 1,
                        "multiple working directories are not supported yet"
                    );
                    Ok(directories)
                })
                .transpose()?
                .map(|directories| directories.into_iter().map(String::from).collect());
            let snapshot = host.create_session(&params.channel, provider, working_directories)?;
            Ok(serde_json::to_value(snapshot).context("serialize createSession snapshot")?)
        }
        "createChat" => {
            require_connection(context)?;
            let params: CreateChatParams =
                serde_json::from_value(params).context("parse createChat")?;
            if params.initial_message.is_some() {
                return Err(RpcFailure::invalid_params(
                    "createChat initialMessage is not supported yet",
                ));
            }
            if params.source.is_some() {
                return Err(RpcFailure::invalid_params(
                    "forked and side chats are not supported yet",
                ));
            }
            if params.working_directories.is_some() {
                return Err(RpcFailure::invalid_params(
                    "chat working directories are not supported yet",
                ));
            }
            let snapshot = host.create_chat(&params.channel, &params.chat)?;
            Ok(serde_json::to_value(snapshot).context("serialize createChat snapshot")?)
        }
        "disposeSession" => {
            require_connection(context)?;
            let params: DisposeSessionParams =
                serde_json::from_value(params).context("parse disposeSession")?;
            host.dispose_session(&params.channel)?;
            Ok(serde_json::json!({}))
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
    host: &Arc<RemoteAgentHost>,
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

#[derive(Debug)]
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use ahp_types::actions::{ChatTurnCancelledAction, ChatTurnStartedAction, StateAction};
    use ahp_types::commands::{
        DispatchActionParams, ListSessionsParams, ListSessionsResult, ReconnectResult,
    };
    use ahp_types::state::{
        Message, MessageKind, MessageOrigin, SessionLifecycle, SessionStatus, Snapshot,
        SnapshotState,
    };

    use super::{handle_command, ConnectionContext, RemoteAgentHost, INVALID_PARAMS};
    use crate::remote_agent::acp_backend::BackendEvent;
    use crate::remote_agent::persistence::{HostStateStore, MemoryHostStateStore};

    fn test_host() -> Arc<RemoteAgentHost> {
        let store: Arc<dyn HostStateStore> = Arc::new(MemoryHostStateStore::empty());
        RemoteAgentHost::load(store, 8, "test-token").expect("create in-memory host")
    }

    fn initialized_context() -> ConnectionContext {
        ConnectionContext {
            connection_id: 1,
            client_id: Some("client".to_string()),
            connection_epoch: Some(1),
            subscriptions: BTreeSet::new(),
        }
    }

    #[tokio::test]
    async fn create_and_dispose_session_commands_update_the_catalogue() {
        let host = test_host();
        let mut context = initialized_context();
        let resource = "ahp-session:/rpc-session";

        let value = handle_command(
            &host,
            &mut context,
            "createSession",
            Some(serde_json::json!({
                "channel": resource,
                "provider": "copilot",
                "workingDirectories": ["file:///C:/repo"],
            })),
        )
        .await
        .expect("create session command");
        let snapshot: Snapshot = serde_json::from_value(value).expect("deserialize snapshot");
        let SnapshotState::Session(session) = snapshot.state else {
            panic!("expected session snapshot");
        };
        assert_eq!(snapshot.resource, resource);
        assert_eq!(session.lifecycle, SessionLifecycle::Creating);
        assert_eq!(session.provider, "copilot");
        assert_eq!(
            session.working_directories,
            Some(vec!["file:///C:/repo".to_string()])
        );

        handle_command(
            &host,
            &mut context,
            "disposeSession",
            Some(serde_json::json!({ "channel": resource })),
        )
        .await
        .expect("dispose session command");
        let sessions: ListSessionsResult = host
            .list_sessions(ListSessionsParams {
                channel: ahp_types::ROOT_RESOURCE_URI.to_string(),
                limit: None,
                cursor: None,
            })
            .expect("list sessions");
        assert!(sessions.items.is_empty());
    }

    #[tokio::test]
    async fn create_session_command_rejects_unsupported_options() {
        let host = test_host();
        let mut context = initialized_context();
        let cases = [
            (
                serde_json::json!({
                    "channel": "ahp-session:/missing-provider",
                }),
                "createSession requires a provider",
            ),
            (
                serde_json::json!({
                    "channel": "ahp-session:/multiple-directories",
                    "provider": "copilot",
                    "workingDirectories": ["file:///C:/one", "file:///C:/two"],
                }),
                "multiple working directories are not supported yet",
            ),
            (
                serde_json::json!({
                    "channel": "ahp-session:/active-client",
                    "provider": "copilot",
                    "activeClient": {
                        "clientId": "other-client",
                        "displayName": "Other client",
                        "tools": [],
                    },
                }),
                "createSession activeClient must match the initialized client",
            ),
        ];

        for (params, expected_message) in cases {
            let error = handle_command(&host, &mut context, "createSession", Some(params))
                .await
                .expect_err("unsupported createSession input must fail");
            assert_eq!(error.code, INVALID_PARAMS);
            assert_eq!(error.message, expected_message);
        }
    }

    #[tokio::test]
    async fn chat_commands_and_backend_events_produce_a_durable_transcript() {
        let host = test_host();
        let session_resource = "ahp-session:/chat-command-session";
        let chat_resource = "ahp-chat:/chat-command";
        host.begin_session_creation(session_resource, "copilot".to_string(), None)
            .expect("begin session");
        host.complete_session_creation(session_resource, "acp-chat-command".to_string())
            .expect("complete session");
        let (epoch, initialized) = host
            .initialize(
                "client",
                &[session_resource.to_string(), chat_resource.to_string()],
                None,
            )
            .expect("initialize client");
        let mut context = ConnectionContext {
            connection_id: 1,
            client_id: Some("client".to_string()),
            connection_epoch: Some(epoch),
            subscriptions: BTreeSet::new(),
        };

        let value = handle_command(
            &host,
            &mut context,
            "createChat",
            Some(serde_json::json!({
                "channel": session_resource,
                "chat": chat_resource,
            })),
        )
        .await
        .expect("create chat command");
        let snapshot: Snapshot = serde_json::from_value(value).expect("deserialize chat snapshot");
        assert!(matches!(snapshot.state, SnapshotState::Chat(_)));

        let turn = DispatchActionParams {
            channel: chat_resource.to_string(),
            client_seq: 1,
            action: StateAction::ChatTurnStarted(ChatTurnStartedAction {
                turn_id: "turn-1".to_string(),
                started_at: "2026-08-23T00:00:00.000Z".to_string(),
                message: Message {
                    text: "Hello".to_string(),
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
        };
        handle_command(
            &host,
            &mut context,
            "dispatchAction",
            Some(serde_json::to_value(turn).expect("serialize turn dispatch")),
        )
        .await
        .expect("start chat turn");
        let (_, replay) = host
            .reconnect(
                "replay-observer",
                initialized.server_seq,
                &[session_resource.to_string(), chat_resource.to_string()],
                None,
            )
            .expect("replay started turn");
        let ReconnectResult::Replay(replay) = replay else {
            panic!("expected chat replay");
        };
        assert_eq!(replay.actions.len(), 4);
        assert!(matches!(
            replay.actions[0].action,
            StateAction::SessionChatAdded(_)
        ));
        assert!(matches!(
            replay.actions[1].action,
            StateAction::SessionDefaultChatChanged(_)
        ));
        assert!(matches!(
            replay.actions[2].action,
            StateAction::ChatTurnStarted(_)
        ));
        assert!(matches!(
            replay.actions[3].action,
            StateAction::SessionChatUpdated(_)
        ));
        let (_, active_session) = host
            .initialize("status-observer", &[session_resource.to_string()], None)
            .expect("observe active session");
        let SnapshotState::Session(active_session) = active_session.snapshots[0].state.clone()
        else {
            panic!("expected session snapshot");
        };
        assert_eq!(active_session.status, SessionStatus::InProgress.bits());
        assert_eq!(
            active_session.activity.as_deref(),
            Some("Waiting for agent")
        );
        host.apply_backend_event(BackendEvent::AgentText {
            agent_session_id: "acp-chat-command".to_string(),
            content: "Hi there".to_string(),
        })
        .expect("append backend text");
        host.apply_backend_event(BackendEvent::PromptFinished {
            agent_session_id: "acp-chat-command".to_string(),
            duration_ms: 12,
            cancelled: false,
            error: None,
        })
        .expect("finish backend prompt");

        let (_, initialized) = host
            .initialize(
                "observer",
                &[chat_resource.to_string(), session_resource.to_string()],
                None,
            )
            .expect("observe chat");
        let SnapshotState::Chat(chat) = initialized.snapshots[0].state.clone() else {
            panic!("expected chat snapshot");
        };
        assert!(chat.active_turn.is_none());
        assert_eq!(chat.turns.len(), 1);
        let ahp_types::state::ResponsePart::Markdown(part) = &chat.turns[0].response_parts[0]
        else {
            panic!("expected markdown response");
        };
        assert_eq!(part.content, "Hi there");
        let SnapshotState::Session(session) = initialized.snapshots[1].state.clone() else {
            panic!("expected session snapshot");
        };
        assert_eq!(session.status, SessionStatus::Idle.bits());
        assert!(session.activity.is_none());

        let cancelled_turn = DispatchActionParams {
            channel: chat_resource.to_string(),
            client_seq: 2,
            action: StateAction::ChatTurnStarted(ChatTurnStartedAction {
                turn_id: "turn-2".to_string(),
                started_at: "2026-08-23T00:00:01.000Z".to_string(),
                message: Message {
                    text: "Stop".to_string(),
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
        };
        host.dispatch("client", epoch, cancelled_turn)
            .expect("start cancelled turn");
        host.apply_backend_event(BackendEvent::PromptFinished {
            agent_session_id: "acp-chat-command".to_string(),
            duration_ms: 5,
            cancelled: true,
            error: None,
        })
        .expect("apply ACP cancellation");
        let (_, initialized) = host
            .initialize("cancel-observer", &[chat_resource.to_string()], None)
            .expect("observe ACP cancellation");
        let SnapshotState::Chat(chat) = initialized.snapshots[0].state.clone() else {
            panic!("expected chat snapshot");
        };
        assert_eq!(chat.turns.len(), 2);
        assert_eq!(chat.turns[1].state, ahp_types::state::TurnState::Cancelled);

        host.dispatch(
            "client",
            epoch,
            DispatchActionParams {
                channel: chat_resource.to_string(),
                client_seq: 3,
                action: StateAction::ChatTurnStarted(ChatTurnStartedAction {
                    turn_id: "turn-3".to_string(),
                    started_at: "2026-08-23T00:00:02.000Z".to_string(),
                    message: Message {
                        text: "Continue".to_string(),
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
            },
        )
        .expect("start interrupted turn");
        host.apply_backend_event(BackendEvent::ProviderExited {
            agent_session_ids: vec!["acp-chat-command".to_string()],
            error: "agent process exited".to_string(),
        })
        .expect("apply provider exit");
        let (_, initialized) = host
            .initialize(
                "failure-observer",
                &[chat_resource.to_string(), session_resource.to_string()],
                None,
            )
            .expect("observe provider failure");
        let SnapshotState::Chat(chat) = initialized.snapshots[0].state.clone() else {
            panic!("expected chat snapshot");
        };
        assert!(chat.active_turn.is_none());
        assert_eq!(chat.turns[2].state, ahp_types::state::TurnState::Error);
        assert_eq!(
            chat.turns[2]
                .error
                .as_ref()
                .map(|error| error.error_type.as_str()),
            Some("AcpProviderExited")
        );
        let SnapshotState::Session(session) = initialized.snapshots[1].state.clone() else {
            panic!("expected session snapshot");
        };
        assert_eq!(session.status, SessionStatus::Error.bits());
    }

    #[test]
    fn prompt_acceptance_failure_is_routed_by_chat_resource() {
        let host = test_host();
        let first_session = "ahp-session:/first-session";
        let second_session = "ahp-session:/second-session";
        let first_chat = "ahp-chat:/first-chat";
        let second_chat = "ahp-chat:/second-chat";
        for (session, agent_session, chat) in [
            (first_session, "acp-first", first_chat),
            (second_session, "acp-second", second_chat),
        ] {
            host.begin_session_creation(session, "copilot".to_string(), None)
                .expect("begin session");
            host.complete_session_creation(session, agent_session.to_string())
                .expect("complete session");
            host.create_chat(session, chat).expect("create chat");
        }
        let (epoch, _) = host
            .initialize("client", &[], None)
            .expect("initialize client");
        for (client_seq, chat) in [(1, first_chat), (2, second_chat)] {
            host.dispatch(
                "client",
                epoch,
                DispatchActionParams {
                    channel: chat.to_string(),
                    client_seq,
                    action: StateAction::ChatTurnStarted(ChatTurnStartedAction {
                        turn_id: "shared-turn-id".to_string(),
                        started_at: "2026-08-23T00:00:00.000Z".to_string(),
                        message: Message {
                            text: "Hello".to_string(),
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
                },
            )
            .expect("start turn");
        }

        host.finish_active_turn_with_error(second_chat, "prompt unavailable".to_string())
            .expect("finish exact chat turn");

        let (_, observed) = host
            .initialize(
                "observer",
                &[first_chat.to_string(), second_chat.to_string()],
                None,
            )
            .expect("observe chats");
        let SnapshotState::Chat(first) = observed.snapshots[0].state.clone() else {
            panic!("expected first chat snapshot");
        };
        assert!(first.active_turn.is_some());
        let SnapshotState::Chat(second) = observed.snapshots[1].state.clone() else {
            panic!("expected second chat snapshot");
        };
        assert!(second.active_turn.is_none());
        assert_eq!(second.turns[0].state, ahp_types::state::TurnState::Error);

        host.dispatch(
            "client",
            epoch,
            DispatchActionParams {
                channel: first_chat.to_string(),
                client_seq: 3,
                action: StateAction::ChatTurnCancelled(ChatTurnCancelledAction {
                    turn_id: "shared-turn-id".to_string(),
                    duration: 21,
                    meta: None,
                }),
            },
        )
        .expect("cancel exact chat turn");
        let (_, observed) = host
            .initialize("cancel-observer", &[first_chat.to_string()], None)
            .expect("observe cancelled chat");
        let SnapshotState::Chat(first) = observed.snapshots[0].state.clone() else {
            panic!("expected first chat snapshot");
        };
        assert!(first.active_turn.is_none());
        assert_eq!(first.turns[0].state, ahp_types::state::TurnState::Cancelled);

        host.apply_backend_event(BackendEvent::ProviderExited {
            agent_session_ids: vec!["acp-first".to_string(), "acp-second".to_string()],
            error: "shared agent process exited".to_string(),
        })
        .expect("apply shared provider exit");
        let (_, observed) = host
            .initialize(
                "provider-observer",
                &[first_session.to_string(), second_session.to_string()],
                None,
            )
            .expect("observe failed sessions");
        for snapshot in observed.snapshots {
            let SnapshotState::Session(session) = snapshot.state else {
                panic!("expected session snapshot");
            };
            assert_eq!(session.status, SessionStatus::Error.bits());
            assert_eq!(session.activity.as_deref(), Some("Agent disconnected"));
        }
    }
}
