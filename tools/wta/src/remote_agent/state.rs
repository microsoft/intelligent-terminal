//! Authoritative, SDK-independent Remote Agent Host state and reducer.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use ahp_types::actions::StateAction;
use ahp_types::commands::DispatchActionParams;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::legacy::LegacySessionSummary;
use super::relay::{RelayEnvelope, RelayKind, RelayOrigin};

const MAX_SESSIONS: usize = 1_000;
const MAX_SESSION_TITLE_BYTES: usize = 512;
const MAX_SESSION_ID_BYTES: usize = 256;
const MAX_CHAT_ID_BYTES: usize = 256;
const MAX_TURN_ID_BYTES: usize = 256;
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_PROVIDER_BYTES: usize = 128;
const CURRENT_FORMAT_VERSION: u32 = 3;

/// Stable host identifier. It is generated only once and stored with the
/// canonical host state, so a restarted process represents the same host.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct HostId(pub(crate) String);

impl HostId {
    pub(crate) fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    pub(crate) fn resource_uri(&self) -> String {
        format!("ahp-host:/{}", self.0)
    }
}

/// Canonical session row; it deliberately contains no AHP wire type.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DomainSession {
    pub(crate) id: String,
    pub(crate) provider: String,
    pub(crate) title: String,
    pub(crate) status: u32,
    pub(crate) activity: Option<String>,
    pub(crate) created_at: String,
    pub(crate) modified_at: String,
    #[serde(default)]
    pub(crate) lifecycle: DomainSessionLifecycle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) creation_error: Option<DomainError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) working_directories: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) agent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) default_chat: Option<String>,
}

impl DomainSession {
    pub(crate) fn resource_uri(&self) -> String {
        format!("ahp-session:/{}", self.id)
    }
}

/// Durable lifecycle state independent from the AHP wire enum.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum DomainSessionLifecycle {
    Creating,
    #[default]
    Ready,
    CreationFailed,
}

/// Durable backend error without coupling the host store to AHP wire types.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DomainError {
    pub(crate) error_type: String,
    pub(crate) message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DomainMessage {
    pub(crate) text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DomainResponsePart {
    pub(crate) id: String,
    pub(crate) content: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum DomainTurnState {
    Complete,
    Cancelled,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DomainActiveTurn {
    pub(crate) id: String,
    pub(crate) started_at: String,
    pub(crate) message: DomainMessage,
    pub(crate) response_parts: Vec<DomainResponsePart>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DomainTurn {
    pub(crate) id: String,
    pub(crate) started_at: String,
    pub(crate) duration: i64,
    pub(crate) message: DomainMessage,
    pub(crate) response_parts: Vec<DomainResponsePart>,
    pub(crate) state: DomainTurnState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<DomainError>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DomainChat {
    pub(crate) id: String,
    pub(crate) session_id: String,
    pub(crate) title: String,
    pub(crate) status: u32,
    pub(crate) activity: Option<String>,
    pub(crate) modified_at: String,
    pub(crate) turns: Vec<DomainTurn>,
    pub(crate) active_turn: Option<DomainActiveTurn>,
}

impl DomainChat {
    pub(crate) fn resource_uri(&self) -> String {
        format!("ahp-chat:/{}", self.id)
    }

    pub(crate) fn session_resource_uri(&self) -> String {
        format!("ahp-session:/{}", self.session_id)
    }
}

/// Persisted portion of host state. Active connections are intentionally not
/// persisted: all reconnecting clients receive a fresh connection epoch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PersistedHostState {
    pub(crate) format_version: u32,
    pub(crate) host_id: HostId,
    pub(crate) next_server_seq: u64,
    pub(crate) next_connection_epoch: u64,
    pub(crate) sessions: BTreeMap<String, DomainSession>,
    #[serde(default)]
    pub(crate) chats: BTreeMap<String, DomainChat>,
    pub(crate) client_sequences: BTreeMap<String, i64>,
}

/// Authoritative in-memory host state.
#[derive(Clone, Debug)]
pub(crate) struct HostState {
    persisted: PersistedHostState,
    live_clients: BTreeMap<String, u64>,
}

/// Outcome of an action dispatch. Rejections are envelopes too, so an AHP
/// client can reconcile an optimistic write with the server's decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DispatchOutcome {
    Accepted(RelayEnvelope),
    Rejected(RelayEnvelope),
}

/// Result of applying a legacy session summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LegacyIngestOutcome {
    pub(crate) envelope: RelayEnvelope,
    pub(crate) session: DomainSession,
    pub(crate) added: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentUnavailableOutcome {
    pub(crate) session: DomainSession,
    pub(crate) envelopes: Vec<RelayEnvelope>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionMutationOutcome {
    pub(crate) envelope: RelayEnvelope,
    pub(crate) session: DomainSession,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionDisposedOutcome {
    pub(crate) envelope: RelayEnvelope,
    pub(crate) resource: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChatCreatedOutcome {
    pub(crate) envelopes: Vec<RelayEnvelope>,
    pub(crate) chat: DomainChat,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChatMutationOutcome {
    pub(crate) envelope: RelayEnvelope,
    pub(crate) chat: DomainChat,
}

impl HostState {
    pub(crate) fn new() -> Self {
        Self {
            persisted: PersistedHostState {
                format_version: CURRENT_FORMAT_VERSION,
                host_id: HostId::new(),
                next_server_seq: 0,
                next_connection_epoch: 0,
                sessions: BTreeMap::new(),
                chats: BTreeMap::new(),
                client_sequences: BTreeMap::new(),
            },
            live_clients: BTreeMap::new(),
        }
    }

    pub(crate) fn from_persisted(mut persisted: PersistedHostState) -> Self {
        persisted.format_version = CURRENT_FORMAT_VERSION;
        Self {
            persisted,
            live_clients: BTreeMap::new(),
        }
    }

    pub(crate) fn persisted(&self) -> PersistedHostState {
        self.persisted.clone()
    }

    pub(crate) fn host_id(&self) -> &HostId {
        &self.persisted.host_id
    }

    pub(crate) fn server_seq(&self) -> u64 {
        self.persisted.next_server_seq
    }

    /// Starts a new connection epoch for a client identifier. A newer
    /// connection makes every message from an older socket stale.
    pub(crate) fn connect_client(&mut self, client_id: &str) -> u64 {
        self.persisted.next_connection_epoch =
            self.persisted.next_connection_epoch.saturating_add(1);
        let epoch = self.persisted.next_connection_epoch;
        self.live_clients.insert(client_id.to_string(), epoch);
        // AHP clientSeq is scoped to one live connection. A reconnect creates
        // a new epoch and the official client starts again at sequence one.
        self.persisted
            .client_sequences
            .insert(client_id.to_string(), 0);
        epoch
    }

    pub(crate) fn sessions(&self) -> impl Iterator<Item = &DomainSession> {
        self.persisted.sessions.values()
    }

    pub(crate) fn session(&self, resource: &str) -> Option<&DomainSession> {
        self.persisted
            .sessions
            .values()
            .find(|session| session.resource_uri() == resource)
    }

    pub(crate) fn chats_for_session<'a>(
        &'a self,
        session_id: &'a str,
    ) -> impl Iterator<Item = &'a DomainChat> {
        self.persisted
            .chats
            .values()
            .filter(move |chat| chat.session_id == session_id)
    }

    pub(crate) fn chat(&self, resource: &str) -> Option<&DomainChat> {
        self.persisted
            .chats
            .values()
            .find(|chat| chat.resource_uri() == resource)
    }

    pub(crate) fn sync_chat_summary(
        &mut self,
        chat_resource: &str,
    ) -> Result<RelayEnvelope, String> {
        let chat_id = parse_chat_resource(chat_resource)?;
        let chat = self
            .persisted
            .chats
            .get(chat_id)
            .cloned()
            .ok_or_else(|| "unknown chat resource".to_string())?;
        let session = self
            .persisted
            .sessions
            .get_mut(&chat.session_id)
            .ok_or_else(|| "chat session is unavailable".to_string())?;
        session.status = chat.status;
        session.activity = chat.activity.clone();
        session.modified_at = chat.modified_at.clone();
        let session_resource = session.resource_uri();
        Ok(self.next_envelope(
            session_resource,
            RelayKind::SessionChatUpdated {
                chat: chat.resource_uri(),
                status: chat.status,
                activity: chat.activity,
                modified_at: chat.modified_at,
            },
            None,
            None,
        ))
    }

    pub(crate) fn begin_session_creation(
        &mut self,
        resource: &str,
        provider: String,
        working_directories: Option<Vec<String>>,
    ) -> Result<SessionMutationOutcome, String> {
        let session_id = parse_session_resource(resource)?;
        let provider = provider.trim();
        if provider.is_empty() {
            return Err("session provider must not be empty".to_string());
        }
        if provider.len() > MAX_PROVIDER_BYTES {
            return Err(format!(
                "session provider exceeds {MAX_PROVIDER_BYTES} bytes"
            ));
        }
        if self.persisted.sessions.contains_key(session_id) {
            return Err("session resource already exists".to_string());
        }
        if self.persisted.sessions.len() >= MAX_SESSIONS {
            return Err(format!(
                "Remote Agent Host session limit of {MAX_SESSIONS} reached"
            ));
        }

        let now = current_timestamp();
        let session = DomainSession {
            id: session_id.to_string(),
            provider: provider.to_string(),
            title: "New Session".to_string(),
            status: ahp_types::state::SessionStatus::Idle.bits(),
            activity: Some("Starting agent".to_string()),
            created_at: now.clone(),
            modified_at: now,
            lifecycle: DomainSessionLifecycle::Creating,
            creation_error: None,
            working_directories,
            agent_session_id: None,
            default_chat: None,
        };
        self.persisted
            .sessions
            .insert(session.id.clone(), session.clone());
        let envelope = self.next_envelope(
            ahp_types::ROOT_RESOURCE_URI.to_string(),
            RelayKind::RootActiveSessionsChanged {
                active_sessions: self.persisted.sessions.len() as i64,
            },
            None,
            None,
        );
        Ok(SessionMutationOutcome { envelope, session })
    }

    pub(crate) fn complete_session_creation(
        &mut self,
        resource: &str,
        agent_session_id: String,
    ) -> Result<SessionMutationOutcome, String> {
        if agent_session_id.trim().is_empty() {
            return Err("agent session id must not be empty".to_string());
        }
        let session = self
            .persisted
            .sessions
            .values_mut()
            .find(|session| session.resource_uri() == resource)
            .ok_or_else(|| "unknown session resource".to_string())?;
        if session.lifecycle != DomainSessionLifecycle::Creating {
            return Err("session is not being created".to_string());
        }
        session.lifecycle = DomainSessionLifecycle::Ready;
        session.creation_error = None;
        session.agent_session_id = Some(agent_session_id);
        session.status = ahp_types::state::SessionStatus::Idle.bits();
        session.activity = None;
        session.modified_at = current_timestamp();
        let session = session.clone();
        let envelope =
            self.next_envelope(resource.to_string(), RelayKind::SessionReady, None, None);
        Ok(SessionMutationOutcome { envelope, session })
    }

    pub(crate) fn fail_session_creation(
        &mut self,
        resource: &str,
        error_type: String,
        message: String,
    ) -> Result<SessionMutationOutcome, String> {
        if error_type.trim().is_empty() || message.trim().is_empty() {
            return Err("session creation error type and message must not be empty".to_string());
        }
        let session = self
            .persisted
            .sessions
            .values_mut()
            .find(|session| session.resource_uri() == resource)
            .ok_or_else(|| "unknown session resource".to_string())?;
        if session.lifecycle != DomainSessionLifecycle::Creating {
            return Err("session is not being created".to_string());
        }
        let error = DomainError {
            error_type,
            message,
        };
        session.lifecycle = DomainSessionLifecycle::CreationFailed;
        session.creation_error = Some(error.clone());
        session.status = ahp_types::state::SessionStatus::Error.bits();
        session.activity = None;
        session.modified_at = current_timestamp();
        let session = session.clone();
        let envelope = self.next_envelope(
            resource.to_string(),
            RelayKind::SessionCreationFailed { error },
            None,
            None,
        );
        Ok(SessionMutationOutcome { envelope, session })
    }

    pub(crate) fn dispose_session(
        &mut self,
        resource: &str,
    ) -> Result<SessionDisposedOutcome, String> {
        let session_id = parse_session_resource(resource)?;
        self.persisted
            .sessions
            .remove(session_id)
            .ok_or_else(|| "unknown session resource".to_string())?;
        self.persisted
            .chats
            .retain(|_, chat| chat.session_id != session_id);
        let envelope = self.next_envelope(
            ahp_types::ROOT_RESOURCE_URI.to_string(),
            RelayKind::RootActiveSessionsChanged {
                active_sessions: self.persisted.sessions.len() as i64,
            },
            None,
            None,
        );
        Ok(SessionDisposedOutcome {
            envelope,
            resource: resource.to_string(),
        })
    }

    pub(crate) fn create_chat(
        &mut self,
        session_resource: &str,
        chat_resource: &str,
    ) -> Result<ChatCreatedOutcome, String> {
        let session_id = parse_session_resource(session_resource)?;
        let chat_id = parse_chat_resource(chat_resource)?;
        let session = self
            .persisted
            .sessions
            .get(session_id)
            .ok_or_else(|| "unknown session resource".to_string())?;
        if session.lifecycle != DomainSessionLifecycle::Ready {
            return Err("session is not ready".to_string());
        }
        if self.persisted.chats.contains_key(chat_id) {
            return Err("chat resource already exists".to_string());
        }
        if self
            .persisted
            .chats
            .values()
            .any(|chat| chat.session_id == session_id)
        {
            return Err("multiple chats are not supported yet".to_string());
        }

        let now = current_timestamp();
        let chat = DomainChat {
            id: chat_id.to_string(),
            session_id: session_id.to_string(),
            title: "New Chat".to_string(),
            status: ahp_types::state::SessionStatus::Idle.bits(),
            activity: None,
            modified_at: now,
            turns: Vec::new(),
            active_turn: None,
        };
        self.persisted.chats.insert(chat.id.clone(), chat.clone());

        let set_default = self
            .persisted
            .sessions
            .get(session_id)
            .is_some_and(|session| session.default_chat.is_none());
        if set_default {
            let session = self
                .persisted
                .sessions
                .get_mut(session_id)
                .expect("validated session exists");
            session.default_chat = Some(chat_resource.to_string());
            session.modified_at = current_timestamp();
        }

        let mut envelopes = vec![self.next_envelope(
            session_resource.to_string(),
            RelayKind::SessionChatAdded {
                chat_id: chat.id.clone(),
                title: chat.title.clone(),
                status: chat.status,
                activity: chat.activity.clone(),
                modified_at: chat.modified_at.clone(),
            },
            None,
            None,
        )];
        if set_default {
            envelopes.push(self.next_envelope(
                session_resource.to_string(),
                RelayKind::SessionDefaultChatChanged {
                    default_chat: Some(chat_resource.to_string()),
                },
                None,
                None,
            ));
        }
        Ok(ChatCreatedOutcome { envelopes, chat })
    }

    pub(crate) fn start_turn(
        &mut self,
        client_id: &str,
        connection_epoch: u64,
        params: DispatchActionParams,
    ) -> Result<DispatchOutcome, String> {
        let StateAction::ChatTurnStarted(action) = &params.action else {
            return Err("expected chat/turnStarted action".to_string());
        };
        let chat_id = parse_chat_resource(&params.channel)?;
        validate_turn_id(&action.turn_id)?;
        if action.message.origin.kind != ahp_types::state::MessageKind::User {
            return Err("chat/turnStarted requires a user message".to_string());
        }
        if action.message.text.trim().is_empty() {
            return Err("chat/turnStarted message must not be empty".to_string());
        }
        if action.message.text.len() > MAX_MESSAGE_BYTES {
            return Err(format!("message exceeds {MAX_MESSAGE_BYTES} bytes"));
        }
        if action.message.attachments.is_some()
            || action.message.model.is_some()
            || action.message.agent.is_some()
            || action.message.meta.is_some()
            || action.queued_message_id.is_some()
            || action.meta.is_some()
        {
            return Err(
                "chat/turnStarted advanced message fields are not supported yet".to_string(),
            );
        }

        let origin = RelayOrigin {
            client_id: client_id.to_string(),
            client_seq: params.client_seq,
        };
        if self.live_clients.get(client_id).copied() != Some(connection_epoch) {
            return Ok(DispatchOutcome::Rejected(self.next_envelope(
                params.channel,
                RelayKind::ChatTurnStarted {
                    turn_id: action.turn_id.clone(),
                    started_at: action.started_at.clone(),
                    message: DomainMessage {
                        text: action.message.text.clone(),
                    },
                },
                Some(origin),
                Some("stale connection epoch".to_string()),
            )));
        }
        let expected = self
            .persisted
            .client_sequences
            .get(client_id)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        if params.client_seq != expected {
            return Ok(DispatchOutcome::Rejected(self.next_envelope(
                params.channel,
                RelayKind::ChatTurnStarted {
                    turn_id: action.turn_id.clone(),
                    started_at: action.started_at.clone(),
                    message: DomainMessage {
                        text: action.message.text.clone(),
                    },
                },
                Some(origin),
                Some(format!(
                    "invalid clientSeq {}; expected {expected}",
                    params.client_seq
                )),
            )));
        }

        let chat = self
            .persisted
            .chats
            .get_mut(chat_id)
            .ok_or_else(|| "unknown chat resource".to_string())?;
        if chat.active_turn.is_some() {
            return Ok(DispatchOutcome::Rejected(self.next_envelope(
                params.channel,
                RelayKind::ChatTurnStarted {
                    turn_id: action.turn_id.clone(),
                    started_at: action.started_at.clone(),
                    message: DomainMessage {
                        text: action.message.text.clone(),
                    },
                },
                Some(origin),
                Some("chat already has an active turn".to_string()),
            )));
        }
        if chat.turns.iter().any(|turn| turn.id == action.turn_id) {
            return Ok(DispatchOutcome::Rejected(self.next_envelope(
                params.channel,
                RelayKind::ChatTurnStarted {
                    turn_id: action.turn_id.clone(),
                    started_at: action.started_at.clone(),
                    message: DomainMessage {
                        text: action.message.text.clone(),
                    },
                },
                Some(origin),
                Some("turn identifier already exists".to_string()),
            )));
        }

        let message = DomainMessage {
            text: action.message.text.clone(),
        };
        chat.active_turn = Some(DomainActiveTurn {
            id: action.turn_id.clone(),
            started_at: action.started_at.clone(),
            message: message.clone(),
            response_parts: Vec::new(),
        });
        chat.status = ahp_types::state::SessionStatus::InProgress.bits();
        chat.activity = Some("Waiting for agent".to_string());
        chat.modified_at = current_timestamp();
        self.persisted
            .client_sequences
            .insert(client_id.to_string(), params.client_seq);
        Ok(DispatchOutcome::Accepted(self.next_envelope(
            params.channel,
            RelayKind::ChatTurnStarted {
                turn_id: action.turn_id.clone(),
                started_at: action.started_at.clone(),
                message,
            },
            Some(origin),
            None,
        )))
    }

    pub(crate) fn append_agent_text(
        &mut self,
        agent_session_id: &str,
        content: String,
    ) -> Result<Vec<ChatMutationOutcome>, String> {
        let session_id = self
            .persisted
            .sessions
            .values()
            .find(|session| session.agent_session_id.as_deref() == Some(agent_session_id))
            .map(|session| session.id.clone())
            .ok_or_else(|| "unknown ACP session".to_string())?;
        let chat = self
            .persisted
            .chats
            .values_mut()
            .find(|chat| chat.session_id == session_id && chat.active_turn.is_some())
            .ok_or_else(|| "ACP session has no active chat turn".to_string())?;
        let active = chat.active_turn.as_mut().expect("active chat was selected");
        let turn_id = active.id.clone();
        let mut kinds = Vec::new();
        if active.response_parts.is_empty() {
            let part_id = format!("{turn_id}-response");
            active.response_parts.push(DomainResponsePart {
                id: part_id.clone(),
                content: String::new(),
            });
            kinds.push(RelayKind::ChatResponsePart {
                turn_id: turn_id.clone(),
                part_id,
            });
        }
        let part = active
            .response_parts
            .last_mut()
            .expect("response part was created");
        part.content.push_str(&content);
        let part_id = part.id.clone();
        chat.activity = Some("Agent is responding".to_string());
        chat.modified_at = current_timestamp();
        let chat = chat.clone();
        kinds.push(RelayKind::ChatDelta {
            turn_id,
            part_id,
            content,
        });
        Ok(kinds
            .into_iter()
            .map(|kind| ChatMutationOutcome {
                envelope: self.next_envelope(chat.resource_uri(), kind, None, None),
                chat: chat.clone(),
            })
            .collect())
    }

    pub(crate) fn cancel_turn(
        &mut self,
        client_id: &str,
        connection_epoch: u64,
        params: DispatchActionParams,
    ) -> Result<DispatchOutcome, String> {
        let StateAction::ChatTurnCancelled(action) = &params.action else {
            return Err("expected chat/turnCancelled action".to_string());
        };
        let chat_id = parse_chat_resource(&params.channel)?;
        validate_turn_id(&action.turn_id)?;
        if action.duration < 0 {
            return Err("chat/turnCancelled duration must not be negative".to_string());
        }
        if action.meta.is_some() {
            return Err("chat/turnCancelled metadata is not supported yet".to_string());
        }

        let origin = RelayOrigin {
            client_id: client_id.to_string(),
            client_seq: params.client_seq,
        };
        let kind = RelayKind::ChatTurnCancelled {
            turn_id: action.turn_id.clone(),
            duration: action.duration,
        };
        if self.live_clients.get(client_id).copied() != Some(connection_epoch) {
            return Ok(DispatchOutcome::Rejected(self.next_envelope(
                params.channel,
                kind,
                Some(origin),
                Some("stale connection epoch".to_string()),
            )));
        }
        let expected = self
            .persisted
            .client_sequences
            .get(client_id)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        if params.client_seq != expected {
            return Ok(DispatchOutcome::Rejected(self.next_envelope(
                params.channel,
                kind,
                Some(origin),
                Some(format!(
                    "invalid clientSeq {}; expected {expected}",
                    params.client_seq
                )),
            )));
        }

        let chat = self
            .persisted
            .chats
            .get_mut(chat_id)
            .ok_or_else(|| "unknown chat resource".to_string())?;
        let Some(active) = chat.active_turn.take() else {
            return Ok(DispatchOutcome::Rejected(self.next_envelope(
                params.channel,
                kind,
                Some(origin),
                Some("chat has no active turn".to_string()),
            )));
        };
        if active.id != action.turn_id {
            chat.active_turn = Some(active);
            return Ok(DispatchOutcome::Rejected(self.next_envelope(
                params.channel,
                kind,
                Some(origin),
                Some("turn identifier does not match the active turn".to_string()),
            )));
        }
        chat.turns.push(DomainTurn {
            id: active.id,
            started_at: active.started_at,
            duration: action.duration,
            message: active.message,
            response_parts: active.response_parts,
            state: DomainTurnState::Cancelled,
            error: None,
        });
        chat.status = ahp_types::state::SessionStatus::Idle.bits();
        chat.activity = None;
        chat.modified_at = current_timestamp();
        self.persisted
            .client_sequences
            .insert(client_id.to_string(), params.client_seq);
        Ok(DispatchOutcome::Accepted(self.next_envelope(
            params.channel,
            kind,
            Some(origin),
            None,
        )))
    }

    pub(crate) fn finish_turn(
        &mut self,
        agent_session_id: &str,
        duration: i64,
        error: Option<DomainError>,
    ) -> Result<ChatMutationOutcome, String> {
        let state = if error.is_some() {
            DomainTurnState::Error
        } else {
            DomainTurnState::Complete
        };
        self.finish_agent_turn(agent_session_id, duration, state, error)
    }

    pub(crate) fn cancel_agent_turn(
        &mut self,
        agent_session_id: &str,
        duration: i64,
    ) -> Result<ChatMutationOutcome, String> {
        self.finish_agent_turn(agent_session_id, duration, DomainTurnState::Cancelled, None)
    }

    pub(crate) fn fail_agent_session(
        &mut self,
        agent_session_id: &str,
        error: DomainError,
    ) -> Result<AgentUnavailableOutcome, String> {
        let session_id = self
            .persisted
            .sessions
            .values()
            .find(|session| session.agent_session_id.as_deref() == Some(agent_session_id))
            .map(|session| session.id.clone())
            .ok_or_else(|| "unknown ACP session".to_string())?;
        let active_chat = self
            .persisted
            .chats
            .values()
            .find(|chat| chat.session_id == session_id && chat.active_turn.is_some())
            .map(|chat| chat.resource_uri());
        let mut envelopes = Vec::new();
        if active_chat.is_some() {
            let turn = self.finish_agent_turn(
                agent_session_id,
                0,
                DomainTurnState::Error,
                Some(error.clone()),
            )?;
            envelopes.push(turn.envelope);
            envelopes.push(
                self.sync_chat_summary(
                    active_chat
                        .as_deref()
                        .expect("active chat resource was selected"),
                )?,
            );
        } else {
            let session = self
                .persisted
                .sessions
                .get_mut(&session_id)
                .expect("session id was selected from the same map");
            session.status = ahp_types::state::SessionStatus::Error.bits();
            session.activity = Some("Agent disconnected".to_string());
            session.modified_at = current_timestamp();
        }
        let session = self
            .persisted
            .sessions
            .get(&session_id)
            .expect("session id was selected from the same map")
            .clone();
        Ok(AgentUnavailableOutcome { session, envelopes })
    }

    fn finish_agent_turn(
        &mut self,
        agent_session_id: &str,
        duration: i64,
        state: DomainTurnState,
        error: Option<DomainError>,
    ) -> Result<ChatMutationOutcome, String> {
        let session_id = self
            .persisted
            .sessions
            .values()
            .find(|session| session.agent_session_id.as_deref() == Some(agent_session_id))
            .map(|session| session.id.clone())
            .ok_or_else(|| "unknown ACP session".to_string())?;
        let chat = self
            .persisted
            .chats
            .values_mut()
            .find(|chat| chat.session_id == session_id && chat.active_turn.is_some())
            .ok_or_else(|| "ACP session has no active chat turn".to_string())?;
        let active = chat.active_turn.take().expect("active chat was selected");
        let turn_id = active.id.clone();
        let kind = match state {
            DomainTurnState::Error => RelayKind::ChatError {
                turn_id: turn_id.clone(),
                duration,
                error: error
                    .clone()
                    .ok_or_else(|| "errored turn requires error details".to_string())?,
            },
            DomainTurnState::Cancelled => RelayKind::ChatTurnCancelled {
                turn_id: turn_id.clone(),
                duration,
            },
            DomainTurnState::Complete => RelayKind::ChatTurnComplete {
                turn_id: turn_id.clone(),
                duration,
            },
        };
        chat.turns.push(DomainTurn {
            id: active.id,
            started_at: active.started_at,
            duration,
            message: active.message,
            response_parts: active.response_parts,
            state,
            error,
        });
        chat.status = if chat
            .turns
            .last()
            .is_some_and(|turn| turn.state == DomainTurnState::Error)
        {
            ahp_types::state::SessionStatus::Error.bits()
        } else {
            ahp_types::state::SessionStatus::Idle.bits()
        };
        chat.activity = None;
        chat.modified_at = current_timestamp();
        let chat = chat.clone();
        let envelope = self.next_envelope(chat.resource_uri(), kind, None, None);
        Ok(ChatMutationOutcome { envelope, chat })
    }

    /// Applies the narrow, deliberate MVP dispatch surface.
    pub(crate) fn dispatch(
        &mut self,
        client_id: &str,
        connection_epoch: u64,
        params: DispatchActionParams,
    ) -> std::result::Result<DispatchOutcome, String> {
        // Reject unsupported action kinds as JSON-RPC invalid-params rather
        // than fabricating an unrelated AHP action envelope.
        let kind = Self::relay_kind_from_action(&params.channel, &params.action)?;

        let origin = RelayOrigin {
            client_id: client_id.to_string(),
            client_seq: params.client_seq,
        };

        if self.live_clients.get(client_id).copied() != Some(connection_epoch) {
            return Ok(DispatchOutcome::Rejected(self.next_envelope(
                params.channel,
                kind,
                Some(origin),
                Some("stale connection epoch".to_string()),
            )));
        }

        let expected = self
            .persisted
            .client_sequences
            .get(client_id)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        if params.client_seq != expected {
            return Ok(DispatchOutcome::Rejected(self.next_envelope(
                params.channel,
                kind,
                Some(origin),
                Some(format!(
                    "invalid clientSeq {}; expected {expected}",
                    params.client_seq
                )),
            )));
        }

        if let RelayKind::SessionTitleChanged { session_id, title } = &kind {
            let Some(session) = self.persisted.sessions.get_mut(session_id) else {
                return Ok(DispatchOutcome::Rejected(self.next_envelope(
                    params.channel,
                    kind,
                    Some(origin),
                    Some("unknown session resource".to_string()),
                )));
            };
            session.title = title.clone();
            session.modified_at = current_timestamp();
        }

        self.persisted
            .client_sequences
            .insert(client_id.to_string(), params.client_seq);
        Ok(DispatchOutcome::Accepted(self.next_envelope(
            params.channel,
            kind,
            Some(origin),
            None,
        )))
    }

    /// Upserts a legacy hook/session summary through a single compatibility
    /// boundary. Existing master/helper hooks do not call this yet.
    pub(crate) fn ingest_legacy(
        &mut self,
        summary: LegacySessionSummary,
    ) -> Result<LegacyIngestOutcome, String> {
        let session_id = legacy_session_id(&summary.source, &summary.external_id);
        let now = current_timestamp();
        let added = !self.persisted.sessions.contains_key(&session_id);
        if added && self.persisted.sessions.len() >= MAX_SESSIONS {
            return Err(format!(
                "Remote Agent Host session limit of {MAX_SESSIONS} reached"
            ));
        }
        let session = self
            .persisted
            .sessions
            .entry(session_id.clone())
            .and_modify(|existing| {
                existing.title = summary.title.clone();
                existing.status = summary.status;
                existing.activity = summary.activity.clone();
                existing.modified_at = now.clone();
            })
            .or_insert_with(|| DomainSession {
                id: session_id,
                provider: summary.source,
                title: summary.title,
                status: summary.status,
                activity: summary.activity,
                created_at: now.clone(),
                modified_at: now,
                lifecycle: DomainSessionLifecycle::Ready,
                creation_error: None,
                working_directories: None,
                agent_session_id: None,
                default_chat: None,
            })
            .clone();

        let envelope = self.next_envelope(
            ahp_types::ROOT_RESOURCE_URI.to_string(),
            RelayKind::RootActiveSessionsChanged {
                active_sessions: self.persisted.sessions.len() as i64,
            },
            None,
            None,
        );
        Ok(LegacyIngestOutcome {
            envelope,
            session,
            added,
        })
    }

    fn relay_kind_from_action(channel: &str, action: &StateAction) -> Result<RelayKind, String> {
        let session_id = channel
            .strip_prefix("ahp-session:/")
            .filter(|id| !id.is_empty())
            .ok_or_else(|| "MVP accepts dispatches only to ahp-session resources".to_string())?;
        match action {
            StateAction::SessionTitleChanged(action) => {
                if action.title.trim().is_empty() {
                    return Err("session title must not be empty".to_string());
                }
                if action.title.len() > MAX_SESSION_TITLE_BYTES {
                    return Err(format!(
                        "session title exceeds {MAX_SESSION_TITLE_BYTES} bytes"
                    ));
                }
                Ok(RelayKind::SessionTitleChanged {
                    session_id: session_id.to_string(),
                    title: action.title.clone(),
                })
            }
            _ => Err("MVP supports only session/titleChanged dispatches".to_string()),
        }
    }

    fn next_envelope(
        &mut self,
        channel: String,
        kind: RelayKind,
        origin: Option<RelayOrigin>,
        rejection_reason: Option<String>,
    ) -> RelayEnvelope {
        self.persisted.next_server_seq = self.persisted.next_server_seq.saturating_add(1);
        RelayEnvelope {
            host_id: self.persisted.host_id.clone(),
            channel,
            server_seq: self.persisted.next_server_seq,
            kind,
            origin,
            rejection_reason,
        }
    }
}

fn parse_session_resource(resource: &str) -> Result<&str, String> {
    let session_id = resource
        .strip_prefix("ahp-session:/")
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "invalid session resource".to_string())?;
    if session_id.len() > MAX_SESSION_ID_BYTES {
        return Err(format!(
            "session identifier exceeds {MAX_SESSION_ID_BYTES} bytes"
        ));
    }

    if session_id.contains('/') {
        return Err("session identifier must be one path segment".to_string());
    }
    Ok(session_id)
}

fn parse_chat_resource(resource: &str) -> Result<&str, String> {
    let chat_id = resource
        .strip_prefix("ahp-chat:/")
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "invalid chat resource".to_string())?;
    if chat_id.len() > MAX_CHAT_ID_BYTES {
        return Err(format!("chat identifier exceeds {MAX_CHAT_ID_BYTES} bytes"));
    }
    if chat_id.contains('/') {
        return Err("chat identifier must be one path segment".to_string());
    }
    Ok(chat_id)
}

fn validate_turn_id(turn_id: &str) -> Result<(), String> {
    if turn_id.trim().is_empty() {
        return Err("turn identifier must not be empty".to_string());
    }
    if turn_id.len() > MAX_TURN_ID_BYTES {
        return Err(format!("turn identifier exceeds {MAX_TURN_ID_BYTES} bytes"));
    }
    Ok(())
}

fn legacy_session_id(source: &str, external_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(source.as_bytes());
    hasher.update([0]);
    hasher.update(external_id.as_bytes());
    let digest = hasher.finalize();
    let prefix: String = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("legacy-{prefix}")
}

fn current_timestamp() -> String {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    iso8601_from_unix_millis(milliseconds as i64)
}

/// Format a Unix timestamp without adding a date/time dependency to the
/// packaged WTA binary. AHP session summaries use RFC 3339 timestamps.
fn iso8601_from_unix_millis(milliseconds: i64) -> String {
    let seconds = milliseconds.div_euclid(1_000);
    let millis = milliseconds.rem_euclid(1_000);
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 }.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year_of_era = yoe + era * 400;
    let day_of_year = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = month_part + if month_part < 10 { 3 } else { -9 };
    let year = year_of_era + if month <= 2 { 1 } else { 0 };

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}
