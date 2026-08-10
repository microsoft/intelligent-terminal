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
}

impl DomainSession {
    pub(crate) fn resource_uri(&self) -> String {
        format!("ahp-session:/{}", self.id)
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

impl HostState {
    pub(crate) fn new() -> Self {
        Self {
            persisted: PersistedHostState {
                format_version: 1,
                host_id: HostId::new(),
                next_server_seq: 0,
                next_connection_epoch: 0,
                sessions: BTreeMap::new(),
                client_sequences: BTreeMap::new(),
            },
            live_clients: BTreeMap::new(),
        }
    }

    pub(crate) fn from_persisted(persisted: PersistedHostState) -> Self {
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
