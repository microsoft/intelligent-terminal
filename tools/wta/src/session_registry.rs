//! In-memory registry of currently-alive ACP sessions.
//!
//! Used by both the master (truth source) and each helper (a push-updated
//! mirror). Master maintains it as the authoritative view of "which sessions
//! are connected right now"; helpers receive `intellterm.wta/session_added`
//! and `session_removed` ext-notifications and apply them locally so the
//! session-manager Enter routing can decide focus vs. resume with zero
//! IPC round-trip.
//!
//! The trait surface is intentionally tiny and async (matching the master's
//! existing `tokio::sync::Mutex` convention on `session_to_helper`). The
//! interior of `InMemoryRegistry` is a plain HashMap behind a tokio mutex —
//! operations are sub-µs CPU work, no awaits held across the lock. Switching
//! to a sync lock model is tracked as a follow-up PR; it stays out of scope
//! here to avoid mixing a lock refactor into the routing change.

use agent_client_protocol as acp;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use crate::agent_sessions::{
    AgentSession, AgentStatus, CliSource, SessionEvent, SessionLocation, SessionOrigin,
};
use tokio::sync::Mutex;

/// Top-level key under `_meta` reserved for our extension. ACP lets
/// vendors pile arbitrary keys into `_meta`; we sit under exactly one
/// namespace so anyone else's `_meta` payload survives a round-trip
/// through master untouched.
pub const WTA_META_NAMESPACE: &str = "wta";

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct HistoryRowKey {
    pub provider_id: String,
    pub location: SessionLocation,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_universe: Option<String>,
}

impl HistoryRowKey {
    pub fn new(
        provider_id: impl AsRef<str>,
        location: SessionLocation,
        session_id: impl AsRef<str>,
        session_universe: Option<String>,
    ) -> Option<Self> {
        let provider_id = provider_id.as_ref().trim().to_ascii_lowercase();
        let session_id = session_id.as_ref().trim().to_string();
        if provider_id.is_empty() || session_id.is_empty() || !location.is_actionable() {
            return None;
        }
        Some(Self {
            provider_id,
            location,
            session_id,
            session_universe: session_universe.filter(|value| !value.trim().is_empty()),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SessionIdentity {
    pub session_id: acp::schema::v1::SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_key: Option<HistoryRowKey>,
}

#[derive(Debug, Default, Clone)]
struct CwdEvidence {
    hook: Option<PathBuf>,
    acp: Option<PathBuf>,
    acp_agent: Option<uuid::Uuid>,
    hook_after_acp: bool,
    revision: u64,
}

impl SessionIdentity {
    pub fn from_info(info: &SessionInfo) -> Self {
        Self {
            session_id: info.session_id.clone(),
            history_key: info.history_row_key(),
        }
    }

    pub fn legacy(session_id: acp::schema::v1::SessionId) -> Self {
        Self {
            session_id,
            history_key: None,
        }
    }
}

impl From<&acp::schema::v1::SessionId> for SessionIdentity {
    fn from(session_id: &acp::schema::v1::SessionId) -> Self {
        Self::legacy(session_id.clone())
    }
}

impl From<&SessionInfo> for SessionIdentity {
    fn from(info: &SessionInfo) -> Self {
        Self::from_info(info)
    }
}

impl From<&SessionIdentity> for SessionIdentity {
    fn from(identity: &SessionIdentity) -> Self {
        identity.clone()
    }
}

/// The subset of `_meta.wta` we read/write today. A struct (rather than
/// just shipping `pane_session_id: Option<String>` directly) so that
/// future fields (titles, owner_tab_id, etc.) can join without
/// touching every call site.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WtaMeta {
    pub pane_session_id: Option<String>,
    /// Declares a master-local control client that must not bind or spawn an
    /// agent CLI. The only supported value is `sidebar-history-v1`.
    pub control_client: Option<String>,
    /// Legacy/advisory full command line. **The master no longer spawns
    /// this** — it is a security hazard to execute an arbitrary string
    /// arriving over the pipe (any same-user process could connect and
    /// drive process creation). The master selects the agent from
    /// `agent_id` and reconstructs the command itself. Kept on the wire
    /// only for diagnostics / back-compat; helpers no longer set it.
    pub agent_cmd: Option<String>,
    /// Canonical agent id (`copilot` / `claude` / `gemini` / …) the
    /// helper's tab requests. The master validates this selection and
    /// reconstructs the agent command internally from this id
    /// (`agent_registry::build_acp_command`) and never executes a string
    /// supplied over the pipe. Also stamps the per-session `cli_source`
    /// so the F2 view labels each row with its real CLI. `None` on older
    /// helpers — master then falls back to its own `--agent` default. This
    /// request is not trusted for agent-specific permission behavior; the
    /// helper uses `resolved_agent_id` returned by the master.
    pub agent_id: Option<String>,
    /// Canonical agent id selected by the master after allowlist validation
    /// and fallback resolution. Returned only in the initialize response so
    /// helper-side agent-specific behavior never trusts its own request.
    pub resolved_agent_id: Option<String>,
    /// Master-resolved process binding category, never a model identifier.
    /// Returned on initialize; absent when talking to an older master.
    pub resolved_model_source: Option<String>,
    /// Model override the tab wants (e.g. `gpt-5`). Folded into the
    /// reconstructed command by `build_acp_command` for agents that
    /// take a `--model` flag (adapter agents ignore it and receive the
    /// model later via `setSessionModel`). Carried as its own field
    /// because the master no longer trusts `agent_cmd` to carry it.
    pub model: Option<String>,
    /// Authoritative custom-provider binding for this helper generation.
    /// `"default"` explicitly disables the master's inherited provider;
    /// `custom:<provider>:<model>` asks the master to resolve that selection
    /// from Terminal settings. The helper never receives credential metadata.
    pub provider_binding: Option<String>,
    /// Execution environment selected for this tab (`host` or `wsl`).
    pub agent_source: Option<String>,
    /// WSL distribution paired with `agent_source=wsl`.
    pub wsl_distro: Option<String>,
    /// The WT tab StableId (`--owner-tab-id`) of the agent pane that
    /// owns this session. Carried so master can resolve close-by-tab
    /// ownership. `None` for non-agent-pane helpers / legacy callers.
    pub owner_tab_id: Option<String>,
    /// JSON-encoded native cloud model catalog. Helpers may supply the host's
    /// last successful snapshot on initialize; master may return a clean-probed
    /// replacement in response metadata. Never forwarded to a third-party agent.
    pub cloud_models: Option<String>,
    /// Provenance for `cloud_models` (`helper` or `clean_probe`).
    pub cloud_models_source: Option<String>,
    /// Requests the master-owned session MCP endpoint. The value is a
    /// versioned contract marker, currently `http-v1`.
    pub proposal_mcp: Option<String>,
    /// Master-to-helper disposition for a completed session transaction.
    /// `retired` means the owning tab was reset or closed while the agent
    /// request was in flight, so the helper must not bind the returned ID.
    pub session_result: Option<String>,
    /// Explicit user retry; never inferred from refresh or a new helper.
    pub startup_intent: Option<String>,
}

impl WtaMeta {
    /// `true` when no field carries a meaningful value. A field counts as
    /// empty when it is `None` **or** holds a whitespace-only string — the
    /// same trim-based notion of emptiness that `extract_wta_meta` /
    /// `inject_wta_meta` use when dropping fields on the wire. Without the
    /// trim check a `Some("   ")` would make `is_empty()` return `false`
    /// while `inject_wta_meta` still serialized nothing, leaving the two
    /// views of "empty" inconsistent (and `inject_wta_meta`'s early-out
    /// keyed off a value the wire never reflects).
    pub fn is_empty(&self) -> bool {
        fn blank(field: &Option<String>) -> bool {
            field.as_deref().map_or(true, |s| s.trim().is_empty())
        }
        blank(&self.pane_session_id)
            && blank(&self.control_client)
            && blank(&self.agent_cmd)
            && blank(&self.agent_id)
            && blank(&self.resolved_agent_id)
            && blank(&self.resolved_model_source)
            && blank(&self.model)
            && blank(&self.provider_binding)
            && blank(&self.agent_source)
            && blank(&self.wsl_distro)
            && blank(&self.owner_tab_id)
            && blank(&self.cloud_models)
            && blank(&self.cloud_models_source)
            && blank(&self.proposal_mcp)
            && blank(&self.session_result)
            && blank(&self.startup_intent)
    }
}

/// Strip the `wta` key out of an ACP `_meta` map and parse what was
/// there into a [`WtaMeta`]. The caller-owned `meta` is mutated in
/// place: the `wta` key is gone afterwards, and if that was the only
/// key the whole `_meta` is collapsed back to `None` so we don't ship
/// `"_meta": {}` to the downstream agent (which a strict implementer
/// might reject).
///
/// This is the master's inbound hook: helpers attach `_meta.wta` on
/// `session/new` / `session/load` requests; master pulls it off,
/// records the binding in `SessionRegistry`, and forwards the
/// request to the agent CLI with `_meta.wta` removed so third-party
/// agents never see our private namespace.
pub fn extract_wta_meta(meta: &mut Option<acp::schema::v1::Meta>) -> WtaMeta {
    let Some(map) = meta.as_mut() else {
        return WtaMeta::default();
    };
    let wta_val = map.remove(WTA_META_NAMESPACE);
    if map.is_empty() {
        *meta = None;
    }
    let Some(serde_json::Value::Object(obj)) = wta_val else {
        return WtaMeta::default();
    };
    // Treat empty / whitespace-only values as absent (`None`) rather than
    // `Some("")`: an empty string on the wire would otherwise make
    // `WtaMeta::is_empty()` return false and keep `_meta.wta` alive for a
    // semantically-empty field. Mirrors the helper's injection-side filter
    // (`.filter(|s| !s.trim().is_empty())`) so values round-trip the same
    // way in both directions.
    let str_field = |key: &str| {
        obj.get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(String::from)
    };
    WtaMeta {
        pane_session_id: str_field("pane_session_id"),
        control_client: str_field("control_client"),
        agent_cmd: str_field("agent_cmd"),
        agent_id: str_field("agent_id"),
        resolved_agent_id: str_field("resolved_agent_id"),
        resolved_model_source: str_field("resolved_model_source"),
        model: str_field("model"),
        provider_binding: str_field("provider_binding"),
        agent_source: str_field("agent_source"),
        wsl_distro: str_field("wsl_distro"),
        owner_tab_id: str_field("owner_tab_id"),
        cloud_models: str_field("cloud_models"),
        cloud_models_source: str_field("cloud_models_source"),
        proposal_mcp: str_field("proposal_mcp"),
        session_result: str_field("session_result"),
        startup_intent: str_field("startup_intent"),
    }
}

/// Inverse of [`extract_wta_meta`]: write our namespace into an ACP
/// `_meta` map, creating the map if it didn't exist. No-op when
/// `wta.is_empty()` — we don't want to litter the wire with empty
/// `_meta.wta` objects when there's nothing to communicate.
///
/// Used by both helpers (when sending `session/new` / `session/load`
/// requests carrying `pane_session_id`) and master (when answering
/// `session/list` with rows whose `pane_session_id` came from the
/// registry).
pub fn inject_wta_meta(meta: &mut Option<acp::schema::v1::Meta>, wta: &WtaMeta) {
    if wta.is_empty() {
        return;
    }
    // Mirror `extract_wta_meta`'s filter: only serialize fields whose value
    // is non-empty after trimming. A whitespace-only `Some(" ")` (which any
    // caller could construct) would otherwise be written as a real field —
    // reintroducing the "semantically empty but present `_meta.wta`" problem
    // that `extract_wta_meta` exists to avoid, and flipping
    // `WtaMeta::is_empty()` off on the wire. Values then round-trip the same
    // way in both directions.
    let mut wta_obj = serde_json::Map::new();
    let mut put = |key: &str, val: &Option<String>| {
        if let Some(s) = val.as_ref().filter(|s| !s.trim().is_empty()) {
            wta_obj.insert(key.to_string(), serde_json::Value::String(s.clone()));
        }
    };
    put("pane_session_id", &wta.pane_session_id);
    put("control_client", &wta.control_client);
    put("agent_cmd", &wta.agent_cmd);
    put("agent_id", &wta.agent_id);
    put("resolved_agent_id", &wta.resolved_agent_id);
    put("resolved_model_source", &wta.resolved_model_source);
    put("model", &wta.model);
    put("provider_binding", &wta.provider_binding);
    put("agent_source", &wta.agent_source);
    put("wsl_distro", &wta.wsl_distro);
    put("owner_tab_id", &wta.owner_tab_id);
    put("cloud_models", &wta.cloud_models);
    put("cloud_models_source", &wta.cloud_models_source);
    put("proposal_mcp", &wta.proposal_mcp);
    put("session_result", &wta.session_result);
    put("startup_intent", &wta.startup_intent);
    // Every field was absent/whitespace-only after filtering — nothing
    // meaningful to attach, so don't litter the wire with an empty
    // `_meta.wta` object (a strict downstream implementer might reject it).
    if wta_obj.is_empty() {
        return;
    }
    let map = meta.get_or_insert_with(serde_json::Map::new);
    map.insert(
        WTA_META_NAMESPACE.to_string(),
        serde_json::Value::Object(wta_obj),
    );
}

/// Project a registry [`SessionInfo`] onto the ACP wire shape that
/// `session/list` answers expect, with our `pane_session_id` stashed
/// inside the standard `_meta.wta` namespace.
///
/// Kept in this module (rather than in `master/mod.rs`) so the
/// `_meta.wta` shape lives in exactly one place — symmetric with
/// [`extract_wta_meta`] / [`inject_wta_meta`].
pub fn to_acp_session_info(info: &SessionInfo) -> acp::schema::v1::SessionInfo {
    let mut out = acp::schema::v1::SessionInfo::new(info.session_id.clone(), info.cwd.clone());
    out.title = info.title.clone();
    out.updated_at = info.updated_at.clone();
    inject_wta_meta(
        &mut out.meta,
        &WtaMeta {
            pane_session_id: info.pane_session_id.clone(),
            ..Default::default()
        },
    );
    out
}

// =============================================================
// ACP ExtNotification protocol — master ⇄ helper live-set sync.
// =============================================================
//
// We send live-set deltas as standard ACP `ExtNotification`s under our
// own `intellterm.wta/...` method namespace. Wire shape is JSON-RPC
// `{ method: "_intellterm.wta/...", params: { ... } }` (the crate
// prepends the `_` itself; see `AgentSideConnection::ext_notification`
// — we pass the bare method here).
//
// The param payload is `to_acp_session_info(info)` serialized — the
// helper just deserializes it back into an `acp::schema::v1::SessionInfo`, lifts
// the `_meta.wta.pane_session_id` out via `extract_wta_meta`, and
// upserts into its mirror. Using `acp::schema::v1::SessionInfo` (not our own
// `SessionInfo`) gives the helper a free `cwd`/`title`/`updated_at`
// schema in exchange for the round-trip through wire types.

/// ExtNotification method for "a new session was just bound to a
/// helper inside this master".
pub const INTELLTERM_METHOD_SESSION_ADDED: &str = "_intellterm.wta/session_added";

/// ExtNotification method for "a session previously announced via
/// `session_added` is gone" (helper disconnect, load_session rollback,
/// future explicit close).
pub const INTELLTERM_METHOD_SESSION_REMOVED: &str = "_intellterm.wta/session_removed";

/// ExtNotification method for "master's session registry changed; refetch if interested".
pub const INTELLTERM_METHOD_SESSIONS_CHANGED: &str = "_intellterm.wta/sessions/changed";

/// ExtRequest method for fetching the master's full session registry snapshot.
pub const INTELLTERM_METHOD_SESSIONS_LIST: &str = "_intellterm.wta/sessions/list";
pub const INTELLTERM_METHOD_SESSION_ACTIVATE: &str = "_intellterm.wta/session/activate";
pub const INTELLTERM_METHOD_SESSION_ACTIVATION_STATUS: &str =
    "_intellterm.wta/session/activation_status";

/// ExtRequest method for physically closing the ACP session owned by a
/// destroyed WT tab. Any surviving helper may send this because master owns
/// the authoritative tab → helper → session route.
pub const INTELLTERM_METHOD_CLOSE_TAB_SESSION: &str = "_intellterm.wta/session/close_tab";

/// ExtRequest method for resolving the live ACP session currently bound to a
/// WT source pane. Master answers from its in-memory registry only.
pub const INTELLTERM_METHOD_SOURCE_PANE_SESSION_BY_PANE: &str = "_intellterm.wta/session/by_pane";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct CloseTabSessionParams {
    pub tab_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SourcePaneSessionByPaneParams {
    pub pane_session_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SourcePaneSessionByPaneResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<acp::schema::v1::SessionId>,
}

/// Wire payload for [`INTELLTERM_METHOD_SESSION_REMOVED`].
///
/// New senders include the qualified history key so helpers can remove the
/// exact row when two providers or locations reuse the same raw ACP id.
/// `history_key` remains optional for compatibility with older masters.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SessionRemovedParams {
    pub session_id: acp::schema::v1::SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_key: Option<HistoryRowKey>,
}

impl SessionRemovedParams {
    pub fn identity(&self) -> SessionIdentity {
        SessionIdentity {
            session_id: self.session_id.clone(),
            history_key: self.history_key.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SessionsChangedParams {}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SessionsListParams {
    /// Explicit refresh: synchronize a bound helper's agent, or schedule host
    /// discovery for an unbound control client. Ordinary snapshots are read-only.
    #[serde(default)]
    pub rescan: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SessionsListResponse {
    pub sessions: Vec<SessionInfo>,
    #[serde(default)]
    pub pane_statuses: Vec<PaneAgentStatus>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native_pane_observations: Vec<NativePaneObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_observation_receiver_generation: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_observation_listener_ready: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_status: Option<HistoryLoadStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_error_kind: Option<HistoryErrorKind>,
}

/// Effective live-pane metadata, independent of whether an SDK conversation exists.
/// Native transport fences are absent for canonical ON/helper pane owners.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct PaneAgentStatus {
    pub pane_id: String,
    pub provider_id: String,
    pub status: AgentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    /// Presentation-only correlation; never a confirmed binding or action route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inferred_history_key: Option<HistoryRowKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<SessionLocation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode_epoch: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
}

/// Terminal-authored observations, not ACP sessions. Only reset establishes an
/// epoch; ON and removal retain a fence against queued OFF observations.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct NativeAgentObservation {
    pub pane_id: String,
    pub provider_id: String,
    pub window_id: u64,
    pub tab_id: String,
    pub mode_epoch: u64,
    #[serde(default)]
    pub receiver_generation: Option<u64>,
    #[serde(default)]
    pub agent_session_id: Option<String>,
    #[serde(default)]
    pub location: Option<SessionLocation>,
    #[serde(default)]
    pub pane_created_at_ms: Option<u64>,
    #[serde(default)]
    pub pane_creation_sequence: Option<u64>,
    #[serde(flatten)]
    pub event: NativeObservationKind,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NativeObservationKind {
    Reset { tracking_enabled: bool },
    Progress { progress_state: u8 },
    Bell,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NativePaneObservation {
    pub pane_id: String,
    pub provider_id: String,
    pub window_id: u64,
    pub tab_id: String,
    pub mode_epoch: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<SessionLocation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_created_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_creation_sequence: Option<u64>,
    /// Advisory list correlation only; never a live binding or routing identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inferred_agent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inferred_history_key: Option<HistoryRowKey>,
    pub status: AgentStatus,
    pub last_activity_at_ms: u64,
}

struct NativePaneState {
    observation: NativePaneObservation,
    bound_identity: Option<SessionIdentity>,
    inference_blocked: bool,
}

struct NativeListingEvidence {
    first_observed_at_ms: u64,
    provider_activity_at_ms: Option<u64>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct NativePaneBindingPredecessor {
    pub agent_session_id: String,
    pub mode_epoch: u64,
}

pub struct BornBoundEventResult {
    pub applied: bool,
    pub native_pane_owned: bool,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HistoryLoadStatus {
    Loading,
    Ready,
    Error,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HistoryErrorKind {
    Timeout,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SessionActivateParams {
    pub identity: SessionIdentity,
    pub window_id: u64,
    pub activation_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SessionActivateResponse {
    pub activation_id: String,
    #[serde(default)]
    pub state: SessionActivationState,
    pub action: String,
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionActivationState {
    Pending,
    #[default]
    Complete,
    Unknown,
}

/// Build a `session_added` ExtNotification from a registry row.
///
/// Panics only if the serializer fails on `acp::schema::v1::SessionInfo`, which
/// would itself be a bug in the schema crate.
pub fn build_session_added_notification(info: &SessionInfo) -> acp::schema::v1::ExtNotification {
    let wire = to_acp_session_info(info);
    let json = serde_json::to_string(&wire)
        .expect("acp::schema::v1::SessionInfo serialization is infallible for owned data");
    let raw = serde_json::value::RawValue::from_string(json)
        .expect("serde_json::to_string always produces valid JSON");
    acp::schema::v1::ExtNotification::new(INTELLTERM_METHOD_SESSION_ADDED, Arc::from(raw))
}

/// Build a `session_removed` ExtNotification.
pub fn build_session_removed_notification(
    identity: impl Into<SessionIdentity>,
) -> acp::schema::v1::ExtNotification {
    let identity = identity.into();
    let params = SessionRemovedParams {
        session_id: identity.session_id.clone(),
        history_key: identity.history_key.clone(),
    };
    let json =
        serde_json::to_string(&params).expect("SessionRemovedParams is trivially serializable");
    let raw = serde_json::value::RawValue::from_string(json)
        .expect("serde_json::to_string always produces valid JSON");
    acp::schema::v1::ExtNotification::new(INTELLTERM_METHOD_SESSION_REMOVED, Arc::from(raw))
}

/// Build a `sessions/changed` ExtNotification with an intentionally empty payload.
pub fn build_sessions_changed_notification() -> acp::schema::v1::ExtNotification {
    let json = serde_json::to_string(&SessionsChangedParams::default())
        .expect("SessionsChangedParams is trivially serializable");
    let raw = serde_json::value::RawValue::from_string(json)
        .expect("serde_json::to_string always produces valid JSON");
    acp::schema::v1::ExtNotification::new(INTELLTERM_METHOD_SESSIONS_CHANGED, Arc::from(raw))
}

/// Build an `ExtRequest` for `intellterm.wta/sessions/list`. `rescan` refreshes
/// the bound agent, or requests background host discovery for a control client.
pub fn build_sessions_list_request(rescan: bool) -> acp::schema::v1::ExtRequest {
    let json = serde_json::to_string(&SessionsListParams { rescan })
        .expect("SessionsListParams is trivially serializable");
    let raw = serde_json::value::RawValue::from_string(json)
        .expect("serde_json::to_string always produces valid JSON");
    acp::schema::v1::ExtRequest::new(INTELLTERM_METHOD_SESSIONS_LIST, Arc::from(raw))
}

pub fn build_session_activate_request(
    identity: SessionIdentity,
    window_id: u64,
    activation_id: String,
    status_only: bool,
) -> acp::schema::v1::ExtRequest {
    let json = serde_json::to_string(&SessionActivateParams {
        identity,
        window_id,
        activation_id,
    })
    .expect("SessionActivateParams is trivially serializable");
    let raw = serde_json::value::RawValue::from_string(json)
        .expect("serde_json::to_string always produces valid JSON");
    acp::schema::v1::ExtRequest::new(
        if status_only {
            INTELLTERM_METHOD_SESSION_ACTIVATION_STATUS
        } else {
            INTELLTERM_METHOD_SESSION_ACTIVATE
        },
        Arc::from(raw),
    )
}

pub fn build_close_tab_session_request(tab_id: &str) -> acp::schema::v1::ExtRequest {
    let json = serde_json::to_string(&CloseTabSessionParams {
        tab_id: tab_id.to_string(),
    })
    .expect("CloseTabSessionParams is trivially serializable");
    let raw = serde_json::value::RawValue::from_string(json)
        .expect("serde_json::to_string always produces valid JSON");
    acp::schema::v1::ExtRequest::new(INTELLTERM_METHOD_CLOSE_TAB_SESSION, Arc::from(raw))
}

pub fn build_source_pane_session_by_pane_request(
    pane_session_id: &str,
) -> acp::schema::v1::ExtRequest {
    let json = serde_json::to_string(&SourcePaneSessionByPaneParams {
        pane_session_id: pane_session_id.to_string(),
    })
    .expect("SourcePaneSessionByPaneParams is trivially serializable");
    let raw = serde_json::value::RawValue::from_string(json)
        .expect("serde_json::to_string always produces valid JSON");
    acp::schema::v1::ExtRequest::new(
        INTELLTERM_METHOD_SOURCE_PANE_SESSION_BY_PANE,
        Arc::from(raw),
    )
}

pub fn parse_close_tab_session_params(
    raw: &serde_json::value::RawValue,
) -> Result<CloseTabSessionParams, serde_json::Error> {
    serde_json::from_str::<CloseTabSessionParams>(raw.get())
}

pub fn parse_source_pane_session_by_pane_params(
    raw: &serde_json::value::RawValue,
) -> Result<SourcePaneSessionByPaneParams, serde_json::Error> {
    serde_json::from_str::<SourcePaneSessionByPaneParams>(raw.get())
}

pub fn build_source_pane_session_by_pane_response(
    session_id: Option<acp::schema::v1::SessionId>,
) -> Box<serde_json::value::RawValue> {
    let response = SourcePaneSessionByPaneResponse { session_id };
    serde_json::value::to_raw_value(&response)
        .expect("SourcePaneSessionByPaneResponse serialization is infallible")
}

pub fn parse_source_pane_session_by_pane_response(
    raw: &serde_json::value::RawValue,
) -> Result<SourcePaneSessionByPaneResponse, serde_json::Error> {
    serde_json::from_str::<SourcePaneSessionByPaneResponse>(raw.get())
}

pub fn parse_sessions_list_params(
    raw: &serde_json::value::RawValue,
) -> Result<SessionsListParams, serde_json::Error> {
    serde_json::from_str::<SessionsListParams>(raw.get())
}

pub fn parse_session_activate_params(
    raw: &serde_json::value::RawValue,
) -> Result<SessionActivateParams, serde_json::Error> {
    serde_json::from_str::<SessionActivateParams>(raw.get())
}

pub fn build_sessions_list_response(
    sessions: Vec<SessionInfo>,
    history_status: Option<HistoryLoadStatus>,
    history_error_kind: Option<HistoryErrorKind>,
    native_pane_observations: Vec<NativePaneObservation>,
    native_observation_receiver_generation: Option<u64>,
    native_observation_listener_ready: Option<bool>,
    pane_statuses: Vec<PaneAgentStatus>,
) -> Box<serde_json::value::RawValue> {
    let response = SessionsListResponse {
        sessions,
        pane_statuses,
        native_pane_observations,
        native_observation_receiver_generation,
        native_observation_listener_ready,
        history_status,
        history_error_kind,
    };
    serde_json::value::to_raw_value(&response)
        .expect("SessionsListResponse serialization is infallible for owned data")
}

pub fn parse_sessions_list_response(
    raw: &serde_json::value::RawValue,
) -> Result<SessionsListResponse, serde_json::Error> {
    serde_json::from_str::<SessionsListResponse>(raw.get())
}

pub fn parse_session_activate_response(
    raw: &serde_json::value::RawValue,
) -> Result<SessionActivateResponse, serde_json::Error> {
    serde_json::from_str::<SessionActivateResponse>(raw.get())
}

/// Parsed view of an inbound ACP `ExtNotification` from master, as
/// recognized by the helper's live-set mirror.
///
/// We deliberately don't fail-loud on unknown methods: extension
/// notifications from a future master version (or a different vendor
/// sharing the channel) must be ignored, not crashed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WtaExtNotification {
    SessionAdded(SessionInfo),
    SessionRemoved(SessionRemovedParams),
    SessionsChanged,
    /// Not one of ours. Caller should silently ignore.
    Unknown,
    /// Method matched but params failed to parse. Caller should log
    /// and skip rather than tear down the connection — a malformed
    /// notification is a master-side bug, but the helper must remain
    /// usable.
    MalformedParams {
        method: String,
        error: String,
    },
}

/// Identify whether an `ExtNotification` is one of ours and, if so,
/// extract the typed payload.
pub fn parse_ext_notification(n: &acp::schema::v1::ExtNotification) -> WtaExtNotification {
    let method: &str = &n.method;
    let raw: &serde_json::value::RawValue = &n.params;
    // ACP 1.0 strips the leading `_` from extension method names on receive,
    // so compare through `ext_method_matches` rather than matching the
    // `_`-prefixed constants literally — otherwise every over-the-wire
    // notification would fall through to `Unknown` and the alive-mirror would
    // never update.
    if ext_method_matches(method, INTELLTERM_METHOD_SESSION_ADDED) {
        match serde_json::from_str::<acp::schema::v1::SessionInfo>(raw.get()) {
            Ok(mut wire) => {
                let wta = extract_wta_meta(&mut wire.meta);
                let mut info = SessionInfo::new(wire.session_id, wire.cwd);
                info.title = wire.title;
                info.updated_at = wire.updated_at;
                info.pane_session_id = wta.pane_session_id;
                WtaExtNotification::SessionAdded(info)
            }
            Err(err) => WtaExtNotification::MalformedParams {
                method: method.to_string(),
                error: err.to_string(),
            },
        }
    } else if ext_method_matches(method, INTELLTERM_METHOD_SESSION_REMOVED) {
        match serde_json::from_str::<SessionRemovedParams>(raw.get()) {
            Ok(p) => WtaExtNotification::SessionRemoved(p),
            Err(err) => WtaExtNotification::MalformedParams {
                method: method.to_string(),
                error: err.to_string(),
            },
        }
    } else if ext_method_matches(method, INTELLTERM_METHOD_SESSIONS_CHANGED) {
        match serde_json::from_str::<SessionsChangedParams>(raw.get()) {
            Ok(_) => WtaExtNotification::SessionsChanged,
            Err(err) => WtaExtNotification::MalformedParams {
                method: method.to_string(),
                error: err.to_string(),
            },
        }
    } else {
        WtaExtNotification::Unknown
    }
}

/// Compare an inbound extension method name against one of our `_`-prefixed
/// `INTELLTERM_METHOD_*` constants.
///
/// ACP 1.0's receive path (`agent-client-protocol` schema `parse_message`)
/// strips the single leading `_` from unrecognized method names before it
/// hands us the `ExtRequest` / `ExtNotification`, while the send side and our
/// constants keep it. Match both forms so a value works whether it arrived
/// over the wire (stripped) or was constructed locally — e.g. in a unit test
/// via a `build_*` helper (still `_`-prefixed).
#[inline]
#[must_use]
pub(crate) fn ext_method_matches(inbound: &str, const_with_underscore: &str) -> bool {
    inbound == const_with_underscore || Some(inbound) == const_with_underscore.strip_prefix('_')
}

/// A typed, inbound helper→master `ExtRequest` that the master answers
/// locally, or a passthrough for anything else. Request-side analogue of
/// [`WtaExtNotification`]: [`parse_ext_request`] collapses the six method-name
/// comparisons, the ACP-1.0 leading-`_` normalization, and per-method param
/// decoding into one place, so `MasterAgent::ext_method` can `match` on a type
/// (with typed params already in hand) instead of chaining `if method == …`
/// string compares — which silently fell through to the agent CLI once ACP 1.0
/// began stripping the `_`.
#[derive(Debug)]
pub enum WtaExtRequest {
    /// `_intellterm.wta/focus_session` — focus the WT pane hosting a session.
    FocusSession(FocusSessionParams),
    /// `_intellterm.wta/sessions/list` — full registry snapshot.
    SessionsList(SessionsListParams),
    /// `_intellterm.wta/session/activate` — qualified Sidebar History activation.
    SessionActivate(SessionActivateParams),
    /// Read an activation receipt without dispatching any terminal operation.
    SessionActivationStatus(SessionActivateParams),
    /// `_intellterm.wta/session_hook` — a helper-originated session event
    /// (resume bookkeeping, pane lifecycle). Agent CLI hooks reach master over
    /// the COM broadcast instead.
    SessionHook(crate::agent_sessions::SessionEvent),
    /// `_intellterm.wta/session_born_bound` — a #266 binding-only registration
    /// (same body as `SessionHook` plus an optional `wsl_distro` → binding-only
    /// semantics). The second field is the WSL distro when the born-bound
    /// session runs inside a distro (delegate `?<prompt>` from a WSL pane), so
    /// the master can stamp the row `Wsl { distro }` for the distro suffix.
    SessionBornBound(crate::agent_sessions::SessionEvent, Option<String>),
    /// `_intellterm.wta/session_resume_dispatched` — optimistic resume flip.
    SessionResumeDispatched(SessionResumeDispatchedParams),
    /// `_intellterm.wta/session_focus` — focus + typed focus result.
    SessionFocus(SessionFocusParams),
    /// `_intellterm.wta/session/close_tab` — close the session belonging to a
    /// destroyed stable tab id, regardless of which surviving helper observed
    /// the terminal event.
    CloseTabSession(CloseTabSessionParams),
    /// `_intellterm.wta/session/by_pane` — read-only source pane → live ACP
    /// session lookup from the master's current registry binding.
    SourcePaneSessionByPane(SourcePaneSessionByPaneParams),
    /// Not one of ours (or a future agent-native extension); forward it
    /// verbatim to the agent CLI so unknown extension methods still work.
    ForwardToAgent(acp::schema::v1::ExtRequest),
    /// Method matched one of ours but the params failed to decode. The master
    /// answers `invalid_params` rather than acting on a half-parsed payload.
    Malformed { method: String, error: String },
}

/// Classify and decode an inbound helper→master `ExtRequest`.
///
/// Normalizes the ACP-1.0 leading-`_` strip in exactly one place (via
/// `ext_method_matches`) and decodes the params for recognized methods, so
/// every caller downstream matches exhaustively on [`WtaExtRequest`]. None of
/// the six method names is a prefix of another, so equality-after-strip cannot
/// collide. Consumes `req` so an unrecognized method can be forwarded verbatim.
#[must_use]
pub fn parse_ext_request(req: acp::schema::v1::ExtRequest) -> WtaExtRequest {
    // Decode `req.params` with `$parser` into the `$variant` payload, mapping a
    // decode failure to `Malformed` (never a silent forward-to-agent).
    macro_rules! decode {
        ($variant:ident, $parser:path) => {
            match $parser(&req.params) {
                Ok(v) => WtaExtRequest::$variant(v),
                Err(err) => WtaExtRequest::Malformed {
                    method: req.method.to_string(),
                    error: err.to_string(),
                },
            }
        };
    }
    if ext_method_matches(&req.method, INTELLTERM_METHOD_FOCUS_SESSION) {
        decode!(FocusSession, parse_focus_session_params)
    } else if ext_method_matches(&req.method, INTELLTERM_METHOD_SESSIONS_LIST) {
        decode!(SessionsList, parse_sessions_list_params)
    } else if ext_method_matches(&req.method, INTELLTERM_METHOD_SESSION_ACTIVATE) {
        decode!(SessionActivate, parse_session_activate_params)
    } else if ext_method_matches(&req.method, INTELLTERM_METHOD_SESSION_ACTIVATION_STATUS) {
        decode!(SessionActivationStatus, parse_session_activate_params)
    } else if ext_method_matches(&req.method, INTELLTERM_METHOD_SESSION_HOOK) {
        decode!(SessionHook, parse_session_hook_params)
    } else if ext_method_matches(&req.method, INTELLTERM_METHOD_SESSION_BORN_BOUND) {
        match parse_born_bound_params(&req.params) {
            Ok((ev, wsl_distro)) => WtaExtRequest::SessionBornBound(ev, wsl_distro),
            Err(err) => WtaExtRequest::Malformed {
                method: req.method.to_string(),
                error: err.to_string(),
            },
        }
    } else if ext_method_matches(&req.method, INTELLTERM_METHOD_SESSION_RESUME_DISPATCHED) {
        decode!(
            SessionResumeDispatched,
            parse_session_resume_dispatched_params
        )
    } else if ext_method_matches(&req.method, INTELLTERM_METHOD_SESSION_FOCUS) {
        decode!(SessionFocus, parse_session_focus_params)
    } else if ext_method_matches(&req.method, INTELLTERM_METHOD_CLOSE_TAB_SESSION) {
        decode!(CloseTabSession, parse_close_tab_session_params)
    } else if ext_method_matches(&req.method, INTELLTERM_METHOD_SOURCE_PANE_SESSION_BY_PANE) {
        decode!(
            SourcePaneSessionByPane,
            parse_source_pane_session_by_pane_params
        )
    } else {
        WtaExtRequest::ForwardToAgent(req)
    }
}

// ─── intellterm.wta/session_resume_dispatched ────────────────────────────────

pub const INTELLTERM_METHOD_SESSION_RESUME_DISPATCHED: &str =
    "_intellterm.wta/session_resume_dispatched";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SessionResumeDispatchedParams {
    pub sid: acp::schema::v1::SessionId,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SessionResumeDispatchedResponse {
    pub flipped: bool,
    pub current_status: String,
}

pub fn build_session_resume_dispatched_request(
    sid: &acp::schema::v1::SessionId,
) -> acp::schema::v1::ExtRequest {
    let params = SessionResumeDispatchedParams { sid: sid.clone() };
    let json = serde_json::to_string(&params)
        .expect("SessionResumeDispatchedParams is trivially serializable");
    let raw = serde_json::value::RawValue::from_string(json)
        .expect("serde_json::to_string always produces valid JSON");
    acp::schema::v1::ExtRequest::new(INTELLTERM_METHOD_SESSION_RESUME_DISPATCHED, Arc::from(raw))
}

pub fn parse_session_resume_dispatched_params(
    raw: &serde_json::value::RawValue,
) -> Result<SessionResumeDispatchedParams, serde_json::Error> {
    serde_json::from_str::<SessionResumeDispatchedParams>(raw.get())
}

pub fn parse_session_resume_dispatched_response(
    raw: &serde_json::value::RawValue,
) -> Result<SessionResumeDispatchedResponse, serde_json::Error> {
    serde_json::from_str::<SessionResumeDispatchedResponse>(raw.get())
}

// ─── intellterm.wta/session_focus ────────────────────────────────────────────

pub const INTELLTERM_METHOD_SESSION_FOCUS: &str = "_intellterm.wta/session_focus";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SessionFocusParams {
    pub sid: acp::schema::v1::SessionId,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SessionFocusResponse {
    pub focused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

pub fn build_session_focus_request(
    sid: &acp::schema::v1::SessionId,
) -> acp::schema::v1::ExtRequest {
    let params = SessionFocusParams { sid: sid.clone() };
    let json =
        serde_json::to_string(&params).expect("SessionFocusParams is trivially serializable");
    let raw = serde_json::value::RawValue::from_string(json)
        .expect("serde_json::to_string always produces valid JSON");
    acp::schema::v1::ExtRequest::new(INTELLTERM_METHOD_SESSION_FOCUS, Arc::from(raw))
}

pub fn parse_session_focus_params(
    raw: &serde_json::value::RawValue,
) -> Result<SessionFocusParams, serde_json::Error> {
    serde_json::from_str::<SessionFocusParams>(raw.get())
}

pub fn parse_session_focus_response(
    raw: &serde_json::value::RawValue,
) -> Result<SessionFocusResponse, serde_json::Error> {
    serde_json::from_str::<SessionFocusResponse>(raw.get())
}

// ─── intellterm.wta/focus_session ────────────────────────────────────────────

/// `ExtRequest` method for "helper asks master to focus the WT pane
/// hosting a given ACP session".
///
/// Helper → master only. Master resolves the SessionId to a
/// `pane_session_id` via its `SessionRegistry`, then dispatches via the
/// shared `WtChannel` (`focus_pane`). Helper never touches wtcli for
/// focus operations directly — all focus traffic funnels through
/// master so a single in-memory map (the master's registry) is the
/// authority on "which pane owns which sid".
pub const INTELLTERM_METHOD_FOCUS_SESSION: &str = "_intellterm.wta/focus_session";

/// Wire payload for [`INTELLTERM_METHOD_FOCUS_SESSION`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct FocusSessionParams {
    pub session_id: acp::schema::v1::SessionId,
}

/// Build an `ExtRequest` for focus_session. Helper side.
pub fn build_focus_session_request(
    sid: &acp::schema::v1::SessionId,
) -> acp::schema::v1::ExtRequest {
    let params = FocusSessionParams {
        session_id: sid.clone(),
    };
    let json =
        serde_json::to_string(&params).expect("FocusSessionParams is trivially serializable");
    let raw = serde_json::value::RawValue::from_string(json)
        .expect("serde_json::to_string always produces valid JSON");
    acp::schema::v1::ExtRequest::new(INTELLTERM_METHOD_FOCUS_SESSION, Arc::from(raw))
}

/// Parse `FocusSessionParams` from an inbound `ExtRequest.params`. Master side.
pub fn parse_focus_session_params(
    raw: &serde_json::value::RawValue,
) -> Result<FocusSessionParams, serde_json::Error> {
    serde_json::from_str::<FocusSessionParams>(raw.get())
}

// ─── intellterm.wta/session_hook ─────────────────────────────────────────────

/// ExtRequest method for "helper observed a SessionEvent; master should apply it".
pub const INTELLTERM_METHOD_SESSION_HOOK: &str = "_intellterm.wta/session_hook";

/// ExtRequest method for a #266 *born-bound* registration — a WTA-launched CLI
/// session (delegate `?<prompt>` / resume) that IT bound to its pane at launch.
/// Same body as `session_hook`, distinct method so the master records it as
/// *binding-only* (`born_bound`) rather than hook-owned: with no real hook
/// installed the file watcher may still supply activity/status for it (never
/// re-binding). A later real `session_hook` event moves it to hook-owned.
pub const INTELLTERM_METHOD_SESSION_BORN_BOUND: &str = "_intellterm.wta/session_born_bound";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum SessionHookCliSource {
    Known(String),
    Unknown {
        #[serde(rename = "Unknown")]
        value: String,
    },
}

impl From<&crate::agent_sessions::CliSource> for SessionHookCliSource {
    fn from(value: &crate::agent_sessions::CliSource) -> Self {
        match value {
            crate::agent_sessions::CliSource::Claude => Self::Known("Claude".to_string()),
            crate::agent_sessions::CliSource::Codex => Self::Known("Codex".to_string()),
            crate::agent_sessions::CliSource::Copilot => Self::Known("Copilot".to_string()),
            crate::agent_sessions::CliSource::Gemini => Self::Known("Gemini".to_string()),
            crate::agent_sessions::CliSource::OpenCode => Self::Known("OpenCode".to_string()),
            crate::agent_sessions::CliSource::Antigravity => Self::Known("Antigravity".to_string()),
            crate::agent_sessions::CliSource::Unknown(value) => Self::Unknown {
                value: value.clone(),
            },
        }
    }
}

impl From<SessionHookCliSource> for crate::agent_sessions::CliSource {
    fn from(value: SessionHookCliSource) -> Self {
        match value {
            SessionHookCliSource::Known(value) => match value.as_str() {
                "Claude" | "claude" => Self::Claude,
                "Codex" | "codex" => Self::Codex,
                "Copilot" | "copilot" => Self::Copilot,
                "Gemini" | "gemini" => Self::Gemini,
                "OpenCode" | "opencode" => Self::OpenCode,
                "Antigravity" | "antigravity" => Self::Antigravity,
                other => Self::Unknown(other.to_string()),
            },
            SessionHookCliSource::Unknown { value } => Self::Unknown(value),
        }
    }
}

/// Wire payload for [`INTELLTERM_METHOD_SESSION_HOOK`]. Mirrors every current
/// [`crate::agent_sessions::SessionEvent`] variant.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum SessionHookParams {
    SessionStarted {
        key: crate::agent_sessions::AgentKey,
        cli_source: SessionHookCliSource,
        pane_session_id: String,
        cwd: PathBuf,
        title: String,
    },
    SessionRenamed {
        key: crate::agent_sessions::AgentKey,
        pane_session_id: String,
        title: String,
    },
    WorkingDirectoryChanged {
        key: crate::agent_sessions::AgentKey,
        pane_session_id: String,
        cwd: PathBuf,
    },
    ToolStarting {
        key: crate::agent_sessions::AgentKey,
        tool_name: String,
    },
    ToolCompleted {
        key: crate::agent_sessions::AgentKey,
    },
    ToolReturned {
        key: crate::agent_sessions::AgentKey,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timestamp_ms: Option<u64>,
    },
    Notification {
        key: crate::agent_sessions::AgentKey,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timestamp_ms: Option<u64>,
    },
    SessionStopped {
        key: crate::agent_sessions::AgentKey,
        reason: String,
    },
    SessionUnbound {
        key: crate::agent_sessions::AgentKey,
        pane_session_id: String,
    },
    SessionStoppedIfOwner {
        key: crate::agent_sessions::AgentKey,
        pane_session_id: Option<String>,
        reason: String,
    },
    ConnectionFailed {
        pane_session_id: String,
        reason: String,
    },
    PaneClosed {
        pane_session_id: String,
    },
    ResumeDispatched {
        key: crate::agent_sessions::AgentKey,
    },
    ResumeFailed {
        key: crate::agent_sessions::AgentKey,
        reason: String,
    },
    ResumePaneAssigned {
        key: crate::agent_sessions::AgentKey,
        pane_session_id: String,
    },
}

impl From<&crate::agent_sessions::SessionEvent> for SessionHookParams {
    fn from(value: &crate::agent_sessions::SessionEvent) -> Self {
        use crate::agent_sessions::SessionEvent;
        match value {
            SessionEvent::SessionStarted {
                key,
                cli_source,
                pane_session_id,
                cwd,
                title,
            } => Self::SessionStarted {
                key: key.clone(),
                cli_source: SessionHookCliSource::from(cli_source),
                pane_session_id: pane_session_id.clone(),
                cwd: cwd.clone(),
                title: title.clone(),
            },
            SessionEvent::SessionRenamed {
                key,
                pane_session_id,
                title,
            } => Self::SessionRenamed {
                key: key.clone(),
                pane_session_id: pane_session_id.clone(),
                title: title.clone(),
            },
            SessionEvent::WorkingDirectoryChanged {
                key,
                pane_session_id,
                cwd,
            } => Self::WorkingDirectoryChanged {
                key: key.clone(),
                pane_session_id: pane_session_id.clone(),
                cwd: cwd.clone(),
            },
            SessionEvent::ToolStarting { key, tool_name } => Self::ToolStarting {
                key: key.clone(),
                tool_name: tool_name.clone(),
            },
            SessionEvent::ToolCompleted { key } => Self::ToolCompleted { key: key.clone() },
            SessionEvent::ToolReturned { key, timestamp_ms } => Self::ToolReturned {
                key: key.clone(),
                timestamp_ms: *timestamp_ms,
            },
            SessionEvent::Notification {
                key,
                message,
                timestamp_ms,
            } => Self::Notification {
                key: key.clone(),
                message: message.clone(),
                timestamp_ms: *timestamp_ms,
            },
            SessionEvent::SessionStopped { key, reason } => Self::SessionStopped {
                key: key.clone(),
                reason: reason.clone(),
            },
            SessionEvent::SessionUnbound {
                key,
                pane_session_id,
            } => Self::SessionUnbound {
                key: key.clone(),
                pane_session_id: pane_session_id.clone(),
            },
            SessionEvent::SessionStoppedIfOwner {
                key,
                pane_session_id,
                reason,
            } => Self::SessionStoppedIfOwner {
                key: key.clone(),
                pane_session_id: pane_session_id.clone(),
                reason: reason.clone(),
            },
            SessionEvent::ConnectionFailed {
                pane_session_id,
                reason,
            } => Self::ConnectionFailed {
                pane_session_id: pane_session_id.clone(),
                reason: reason.clone(),
            },
            SessionEvent::PaneClosed { pane_session_id } => Self::PaneClosed {
                pane_session_id: pane_session_id.clone(),
            },
            SessionEvent::ResumeDispatched { key } => Self::ResumeDispatched { key: key.clone() },
            SessionEvent::ResumeFailed { key, reason } => Self::ResumeFailed {
                key: key.clone(),
                reason: reason.clone(),
            },
            SessionEvent::ResumePaneAssigned {
                key,
                pane_session_id,
            } => Self::ResumePaneAssigned {
                key: key.clone(),
                pane_session_id: pane_session_id.clone(),
            },
        }
    }
}

impl From<SessionHookParams> for crate::agent_sessions::SessionEvent {
    fn from(value: SessionHookParams) -> Self {
        match value {
            SessionHookParams::SessionStarted {
                key,
                cli_source,
                pane_session_id,
                cwd,
                title,
            } => Self::SessionStarted {
                key,
                cli_source: cli_source.into(),
                pane_session_id,
                cwd,
                title,
            },
            SessionHookParams::SessionRenamed {
                key,
                pane_session_id,
                title,
            } => Self::SessionRenamed {
                key,
                pane_session_id,
                title,
            },
            SessionHookParams::WorkingDirectoryChanged {
                key,
                pane_session_id,
                cwd,
            } => Self::WorkingDirectoryChanged {
                key,
                pane_session_id,
                cwd,
            },
            SessionHookParams::ToolStarting { key, tool_name } => {
                Self::ToolStarting { key, tool_name }
            }
            SessionHookParams::ToolCompleted { key } => Self::ToolCompleted { key },
            SessionHookParams::ToolReturned { key, timestamp_ms } => {
                Self::ToolReturned { key, timestamp_ms }
            }
            SessionHookParams::Notification {
                key,
                message,
                timestamp_ms,
            } => Self::Notification {
                key,
                message,
                timestamp_ms,
            },
            SessionHookParams::SessionStopped { key, reason } => {
                Self::SessionStopped { key, reason }
            }
            SessionHookParams::SessionUnbound {
                key,
                pane_session_id,
            } => Self::SessionUnbound {
                key,
                pane_session_id,
            },
            SessionHookParams::SessionStoppedIfOwner {
                key,
                pane_session_id,
                reason,
            } => Self::SessionStoppedIfOwner {
                key,
                pane_session_id,
                reason,
            },
            SessionHookParams::ConnectionFailed {
                pane_session_id,
                reason,
            } => Self::ConnectionFailed {
                pane_session_id,
                reason,
            },
            SessionHookParams::PaneClosed { pane_session_id } => {
                Self::PaneClosed { pane_session_id }
            }
            SessionHookParams::ResumeDispatched { key } => Self::ResumeDispatched { key },
            SessionHookParams::ResumeFailed { key, reason } => Self::ResumeFailed { key, reason },
            SessionHookParams::ResumePaneAssigned {
                key,
                pane_session_id,
            } => Self::ResumePaneAssigned {
                key,
                pane_session_id,
            },
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SessionHookResponse {
    pub applied: bool,
}

/// Build a fire-and-forget helper → master `session_hook` ExtRequest.
///
/// Still used for events the helper itself originates (resume bookkeeping,
/// born-bound registration). Agent CLI hooks do NOT travel this path: master
/// subscribes to the COM broadcast and routes them itself.
pub fn build_session_hook_request(
    event: &crate::agent_sessions::SessionEvent,
) -> acp::schema::v1::ExtRequest {
    let params = SessionHookParams::from(event);
    let json =
        serde_json::to_string(&params).expect("SessionHookParams serialization is infallible");
    let raw = serde_json::value::RawValue::from_string(json)
        .expect("serde_json::to_string always produces valid JSON");
    acp::schema::v1::ExtRequest::new(INTELLTERM_METHOD_SESSION_HOOK, Arc::from(raw))
}

/// Build a #266 *born-bound* registration ExtRequest. Identical body to
/// [`build_session_hook_request`] but a distinct method
/// ([`INTELLTERM_METHOD_SESSION_BORN_BOUND`]) so the master treats it as
/// binding-only (watcher may still supply status), not hook-owned.
pub fn build_born_bound_request(
    event: &crate::agent_sessions::SessionEvent,
) -> acp::schema::v1::ExtRequest {
    build_born_bound_request_inner(event, None)
}

/// Like [`build_born_bound_request`] but tags the session as running inside a
/// WSL distro. The master stamps the created row `SessionLocation::Wsl {
/// distro }` (the reducer defaults `SessionStarted` to `Host`) so the session
/// view names the distro in the row suffix — used by the `?<prompt>` delegate
/// path when the active pane is a WSL pane.
pub fn build_born_bound_request_wsl(
    event: &crate::agent_sessions::SessionEvent,
    distro: &str,
) -> acp::schema::v1::ExtRequest {
    build_born_bound_request_inner(event, Some(distro.to_string()))
}

/// Serialize the born-bound body: the shared `SessionHookParams` object with an
/// optional `wsl_distro` key merged in (omitted when `None`). Routing this
/// through `serde_json::Value` may reorder keys relative to a direct
/// `SessionHookParams` serialization, so the host body is not byte-identical to
/// a `session_hook` body — but it is structurally the same object, which is all
/// `parse_session_hook_params` (field-based, order-independent) relies on.
fn build_born_bound_request_inner(
    event: &crate::agent_sessions::SessionEvent,
    wsl_distro: Option<String>,
) -> acp::schema::v1::ExtRequest {
    let params = SessionHookParams::from(event);
    let mut value =
        serde_json::to_value(&params).expect("SessionHookParams serialization is infallible");
    if let (Some(obj), Some(distro)) = (value.as_object_mut(), wsl_distro) {
        obj.insert("wsl_distro".to_string(), serde_json::Value::String(distro));
    }
    let json =
        serde_json::to_string(&value).expect("serde_json::Value serialization is infallible");
    let raw = serde_json::value::RawValue::from_string(json)
        .expect("serde_json::to_string always produces valid JSON");
    acp::schema::v1::ExtRequest::new(INTELLTERM_METHOD_SESSION_BORN_BOUND, Arc::from(raw))
}

/// Parse a master-bound `session_hook` body into the canonical reducer event.
pub fn parse_session_hook_params(
    raw: &serde_json::value::RawValue,
) -> Result<crate::agent_sessions::SessionEvent, serde_json::Error> {
    serde_json::from_str::<SessionHookParams>(raw.get()).map(Into::into)
}
/// Parse a born-bound body into `(event, wsl_distro)`. Reuses
/// [`parse_session_hook_params`] for the event (it ignores the extra
/// `wsl_distro` key) and separately extracts the optional distro.
pub fn parse_born_bound_params(
    raw: &serde_json::value::RawValue,
) -> Result<(crate::agent_sessions::SessionEvent, Option<String>), serde_json::Error> {
    #[derive(serde::Deserialize)]
    struct WslDistroField {
        #[serde(default)]
        wsl_distro: Option<String>,
    }
    let event = parse_session_hook_params(raw)?;

    // Propagate a malformed `wsl_distro` (present but the wrong JSON type) as a
    // parse error so the master answers `invalid_params`, rather than silently
    // dropping the distro and mislabelling the row as a host session. A missing
    // field deserializes to `None` via `#[serde(default)]`, so the plain host
    // body (no `wsl_distro`) still parses cleanly.
    let distro = serde_json::from_str::<WslDistroField>(raw.get())?.wsl_distro;
    Ok((event, distro))
}

/// Build a master response for `session_hook`.
pub fn build_session_hook_response(applied: bool) -> acp::schema::v1::ExtResponse {
    let response = SessionHookResponse { applied };
    let raw = serde_json::value::to_raw_value(&response)
        .expect("SessionHookResponse serialization is infallible");
    acp::schema::v1::ExtResponse::new(raw.into())
}

/// One row in the registry. Mirrors the fields the session management view needs:
///
/// * `session_id` — the ACP session GUID (truth-source key).
/// * `cwd`        — required by ACP `SessionInfo` for `session/list`
///                  responses; populated from `NewSessionRequest.cwd` at
///                  insertion time.
/// * `title`      — optional human-friendly label; `None` until we wire a
///                  title source (e.g. derived from the first user prompt).
/// * `updated_at` — optional ISO-8601 timestamp of the last activity, kept
///                  here so `session/list` responses match agents that
///                  populate it; we leave it `None` for now (history sort
///                  uses local `agent-pane-sessions.jsonl` provenance).
/// * `pane_session_id` — the WT pane GUID (`WT_SESSION`) that owns this
///                  ACP session. Some sessions have no pane attached
///                  (e.g. legacy entries replayed from history before the
///                  field was introduced) so this is `Option`. Serialized
///                  into `acp::schema::v1::SessionInfo._meta.wta.pane_session_id` on
///                  the wire so we don't pollute the standard ACP schema.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionInfo {
    pub session_id: acp::schema::v1::SessionId,
    #[serde(default)]
    pub provider_id: Option<String>,
    #[serde(default)]
    pub session_universe: Option<String>,
    pub cwd: PathBuf,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub pane_session_id: Option<String>,
    /// Response-only attribution, resolved from the bound pane by `sessions/list`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_window_id: Option<u64>,
    /// Response-only whole-tab membership; absent on older or unavailable hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_tab: Option<bool>,
    #[serde(default)]
    pub status: Option<AgentStatus>,
    #[serde(default)]
    pub cli_source: Option<CliSource>,
    #[serde(default)]
    pub current_tool: Option<String>,
    #[serde(default)]
    pub attention_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention_timestamp_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention_cooldown_until_ms: Option<u64>,
    #[serde(default)]
    pub last_activity_at_ms: Option<u64>,
    #[serde(default)]
    pub origin: Option<SessionOrigin>,
    #[serde(default)]
    pub last_error: Option<String>,
    /// Where this session's artefacts live (host vs a WSL distro).
    /// `#[serde(default)]` so older peers / responses without the field
    /// deserialize as `Host`. This is the cross-process carrier that
    /// makes the distro tag + WSL-resume work in the agent-pane
    /// `/sessions` view (which renders from master's `SessionInfo`
    /// snapshot, not the helper's `AgentSession` registry).
    #[serde(default)]
    pub location: SessionLocation,
    /// PID of the process that owns this Class-B session (Copilot worker /
    /// Codex rollout holder / cwd-matched Claude), captured at bind time.
    /// Master's liveness poll checks it to demote shell-pane sessions whose
    /// CLI exited without writing a "session ended" record (e.g. `Ctrl+C`).
    /// `None` for agent-pane sessions and any session we couldn't bind to a
    /// pid. Master-internal — never serialized (`#[serde(skip)]`), so it can
    /// never leak into a `sessions/list` response, which serializes
    /// `SessionInfo` directly.
    #[serde(skip)]
    pub bound_pid: Option<u32>,
    /// True while this row's pane binding was established by WTA itself
    /// (`/sessions` resume → `ResumePaneAssigned`) rather than by a hook.
    ///
    /// WTA creates the resume pane and binds it *before* the agent CLI even
    /// starts, so the binding is authoritative: for that pane, only this
    /// session id may claim ownership. The flag exists because Copilot's
    /// `--resume` boots a throwaway bootstrap session first and only switches
    /// to the requested one seconds later; its `SessionStart` hook therefore
    /// arrives carrying the *bootstrap* id together with the resumed pane's
    /// GUID. Honouring that handoff ends the resumed row, and terminal-state
    /// rows refuse resurrection, so the row stays `Ended` for the rest of its
    /// life while the CLI keeps running. See
    /// [`apply_event_locked`]'s `SessionStarted` arm.
    ///
    /// Cleared whenever the row gives up the pane (ended, pane closed,
    /// rebound), so the protection releases itself without a timer.
    /// Master-internal — never serialized, exactly like [`Self::bound_pid`].
    #[serde(skip)]
    pub born_bound_pane: bool,
    #[serde(skip)]
    cwd_evidence: CwdEvidence,
}

// Cwd evidence is internal; preserve equality of the existing session fields.
impl PartialEq for SessionInfo {
    fn eq(&self, other: &Self) -> bool {
        self.session_id == other.session_id
            && self.provider_id == other.provider_id
            && self.session_universe == other.session_universe
            && self.cwd == other.cwd
            && self.title == other.title
            && self.updated_at == other.updated_at
            && self.pane_session_id == other.pane_session_id
            && self.owner_window_id == other.owner_window_id
            && self.background_tab == other.background_tab
            && self.status == other.status
            && self.cli_source == other.cli_source
            && self.current_tool == other.current_tool
            && self.attention_reason == other.attention_reason
            && self.attention_timestamp_ms == other.attention_timestamp_ms
            && self.attention_cooldown_until_ms == other.attention_cooldown_until_ms
            && self.last_activity_at_ms == other.last_activity_at_ms
            && self.origin == other.origin
            && self.last_error == other.last_error
            && self.location == other.location
            && self.bound_pid == other.bound_pid
            && self.born_bound_pane == other.born_bound_pane
    }
}

impl Eq for SessionInfo {}

impl SessionInfo {
    pub(crate) fn cwd_revision(&self) -> u64 {
        self.cwd_evidence.revision
    }

    /// Convenience constructor for tests and call sites that only have the
    /// mandatory fields. Optional fields default to `None`.
    pub fn new(session_id: acp::schema::v1::SessionId, cwd: PathBuf) -> Self {
        Self {
            session_id,
            provider_id: None,
            session_universe: None,
            cwd,
            title: None,
            updated_at: None,
            pane_session_id: None,
            owner_window_id: None,
            background_tab: None,
            status: None,
            cli_source: None,
            current_tool: None,
            attention_reason: None,
            attention_timestamp_ms: None,
            attention_cooldown_until_ms: None,
            last_activity_at_ms: None,
            origin: None,
            last_error: None,
            location: SessionLocation::Unknown,
            bound_pid: None,
            born_bound_pane: false,
            cwd_evidence: CwdEvidence::default(),
        }
    }

    /// Builder-style setter for `pane_session_id`, useful in tests and at
    /// `new_session` time when the helper hands us a `_meta.wta` payload.
    pub fn with_pane_session_id(mut self, pane_session_id: impl Into<String>) -> Self {
        self.pane_session_id = Some(pane_session_id.into());
        self
    }

    pub fn history_row_key(&self) -> Option<HistoryRowKey> {
        HistoryRowKey::new(
            self.provider_id.as_deref()?,
            self.location.clone(),
            self.session_id.to_string(),
            self.session_universe.clone(),
        )
    }
}

/// Convert an `AgentSession` (the helper-side representation, also produced
/// by `session_history`'s mapping of ACP `session/list` rows) into a
/// `SessionInfo` for upsert into master's registry.
///
/// Used by master at startup to seed the registry with historical rows
/// returned by the agent's own ACP `session/list` so `wta sessions list`
/// and session management viewers see the full set, not just live
/// sessions created via `session/new` after master booted.
pub fn agent_session_to_session_info(s: &AgentSession) -> SessionInfo {
    let last_activity_at_ms = s
        .last_activity_at
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64);
    SessionInfo {
        session_id: acp::schema::v1::SessionId::new(s.key.clone()),
        provider_id: s.cli_source.canonical_provider_id(),
        session_universe: None,
        cwd: s.cwd.clone(),
        title: if s.title.is_empty() {
            None
        } else {
            Some(s.title.clone())
        },
        updated_at: None,
        pane_session_id: s.pane_session_id.clone(),
        owner_window_id: None,
        background_tab: None,
        status: Some(s.status.clone()),
        cli_source: Some(s.cli_source.clone()),
        current_tool: s.current_tool.clone(),
        attention_reason: s.attention_reason.clone(),
        attention_timestamp_ms: s.attention_timestamp_ms,
        attention_cooldown_until_ms: s.attention_cooldown_until_ms,
        last_activity_at_ms,
        origin: Some(s.origin.clone()),
        last_error: s.last_error.clone(),
        location: s.location.clone(),
        // History-scan / helper-sourced rows have no bound pid; only the
        // file watcher's bind step populates it.
        bound_pid: None,
        // Master-internal: only master's own `ResumePaneAssigned` reducer
        // marks a pane binding as WTA-owned.
        born_bound_pane: false,
        cwd_evidence: CwdEvidence::default(),
    }
}

/// Read/write surface over the live-session set. Both master and helper
/// hold an `Arc<dyn SessionRegistry>` so unit tests can swap in mocks
/// without spinning up a real pipe. In production both sides use
/// `InMemoryRegistry`.
#[allow(dead_code)] // Task B wires hook RPCs into these reducer methods.
#[async_trait::async_trait]
pub trait SessionRegistry: Send + Sync {
    /// Some means an active native OFF owner claims this id, including rejected
    /// provider/backend or non-activity events. None leaves legacy gates intact.
    async fn apply_native_watcher_activity(
        &self,
        _cli: &CliSource,
        _key: &str,
        _event: SessionEvent,
    ) -> Option<bool> {
        None
    }

    async fn apply_native_observation(&self, _event: NativeAgentObservation) -> bool {
        false
    }

    async fn refine_native_launch_source(
        &self,
        _pane: &str,
        _provider: &str,
        _location: SessionLocation,
    ) -> bool {
        false
    }

    async fn register_native_born_binding(
        &self,
        _identity: &SessionIdentity,
        _pane: &str,
        _cwd: &std::path::Path,
        _mode_epoch: Option<u64>,
        _predecessor: Option<&NativePaneBindingPredecessor>,
    ) -> bool {
        false
    }

    async fn native_pane_observations(&self) -> Vec<NativePaneObservation> {
        Vec::new()
    }

    async fn has_pending_native_inference(
        &self,
        _provider: &str,
        _location: &SessionLocation,
    ) -> bool {
        false
    }

    /// Read effective pane metadata and its legacy native diagnostic view under
    /// the same registry lock, so OFF precedence cannot race a mode transition.
    async fn pane_status_snapshot(&self) -> (Vec<PaneAgentStatus>, Vec<NativePaneObservation>);

    /// Successful SDK list page, before inserting its historical rows. Partial
    /// pages retain first-observation evidence but can never infer a binding.
    async fn enrich_native_from_listing(
        &self,
        _provider: &str,
        _location: &SessionLocation,
        _rows: &[SessionInfo],
        _previously_listed: &HashSet<String>,
        _started_at_ms: u64,
        _completed_at_ms: u64,
        _listing_complete: bool,
    ) -> bool {
        false
    }

    /// Insert-or-replace the row for `info.session_id`. Idempotent — calling
    /// twice with the same `session_id` keeps only the latest copy.
    async fn upsert(&self, info: SessionInfo);

    /// Insert `info` only if no row already exists for `info.session_id`.
    /// Unlike [`upsert`](Self::upsert), this never replaces an existing row —
    /// used by the history (re)scan so a reloaded **Historical** disk row can't
    /// clobber a live session's status / pane binding (the row a live session
    /// already occupies). The default impl is a (non-atomic) lookup-then-upsert;
    /// `InMemoryRegistry` overrides it with an atomic check under one lock.
    async fn upsert_if_absent(&self, info: SessionInfo) {
        if self
            .lookup_identity(&SessionIdentity::from_info(&info))
            .await
            .is_none()
        {
            self.upsert(info).await;
        }
    }

    /// Remove the only row for `sid`. Ambiguous raw ids fail closed.
    async fn remove(&self, sid: &acp::schema::v1::SessionId) -> Option<SessionInfo>;

    /// Atomically remove the only row for `sid` satisfying `predicate`.
    /// Multiple matching rows fail closed.
    async fn remove_if(
        &self,
        sid: &acp::schema::v1::SessionId,
        predicate: &(dyn for<'a> Fn(&'a SessionInfo) -> bool + Sync),
    ) -> Option<SessionInfo>;

    /// Fetch the only row for `sid`. Ambiguous raw ids fail closed.
    async fn lookup(&self, sid: &acp::schema::v1::SessionId) -> Option<SessionInfo>;

    /// Resolve the currently active live session bound to `pane_session_id`.
    /// Unknown / historical / ended statuses, synthetic pane-derived ids, and
    /// Copilot's internal sidekick sessions are intentionally omitted.
    async fn lookup_active_by_pane(&self, pane_session_id: &str) -> Option<SessionInfo>;

    async fn remove_identity(&self, identity: &SessionIdentity) -> Option<SessionInfo>;

    async fn remove_identity_if(
        &self,
        identity: &SessionIdentity,
        predicate: &(dyn for<'a> Fn(&'a SessionInfo) -> bool + Sync),
    ) -> Option<SessionInfo>;

    async fn lookup_identity(&self, identity: &SessionIdentity) -> Option<SessionInfo>;

    /// Snapshot the full set. Order is unspecified — callers that need a
    /// stable order should sort by `session_id` themselves. The clone is
    /// cheap because `SessionInfo` is small (`Arc<str>` for the id).
    async fn snapshot(&self) -> Vec<SessionInfo>;

    /// Apply a helper-observed session event to the master-side reducer state.
    async fn apply_event(&self, ev: SessionEvent) -> bool;
    /// Record lifecycle evidence only from master's ordered WT event stream.
    async fn record_pane_connected(&self, pane_session_id: &str);
    async fn record_pane_connection_failure(&self, pane_session_id: &str, reason: &str);
    async fn apply_born_bound_event(
        &self,
        ev: SessionEvent,
        location: SessionLocation,
        preserve_live_binding: bool,
    ) -> BornBoundEventResult;

    /// Update origin metadata on an existing row.
    async fn set_origin(&self, sid: &acp::schema::v1::SessionId, origin: SessionOrigin) -> bool;

    /// Update the `location` (Host / WSL distro) of an existing row. Returns
    /// `true` iff the value actually changed. Used to stamp a born-bound WSL
    /// delegate row as `Wsl { distro }` after the reducer creates it (the
    /// reducer defaults every `SessionStarted` to `Host`), so the session view
    /// names the distro in the row suffix.
    async fn set_location(
        &self,
        sid: &acp::schema::v1::SessionId,
        location: crate::agent_sessions::SessionLocation,
    ) -> bool;

    /// Atomically flip a Historical row to Idle for resume dispatch (Task C).
    /// Returns Some((flipped, current_status_label)) where `flipped` is true
    /// only when the row was Historical and was transitioned this call.
    async fn mark_resume_dispatched(
        &self,
        sid: &acp::schema::v1::SessionId,
    ) -> Option<(bool, String)>;

    /// Atomically flip the exact qualified row for resume dispatch.
    async fn mark_resume_dispatched_identity(
        &self,
        identity: &SessionIdentity,
    ) -> Option<(bool, String)>;

    /// Bind a newly created resume pane to the exact qualified row.
    async fn assign_resume_pane_identity(
        &self,
        identity: &SessionIdentity,
        pane_session_id: String,
    ) -> bool;

    /// Atomically replace `title` for `sid` only if the current title is
    /// "synthetic" (`None`, empty, or equal to the cwd basename). Returns
    /// `true` iff the title was actually changed. The candidate must be
    /// non-empty.
    ///
    /// Mirrors the helper-side `AgentSessionRegistry::upgrade_title_if_synthetic`
    /// (see `agent_sessions.rs`). Master needs the same surface so it can
    /// upgrade titles from disk after a `session_hook` ExtRequest applies an
    /// event — without it, the session management view (which renders master's snapshot) keeps showing
    /// the synthetic cwd-basename title even after the CLI writes the real
    /// chat title to disk.
    ///
    /// The check + mutate happen under one lock so a concurrent `apply_event`
    /// or `upsert` can't race the disk-read-and-write that an alternative
    /// `lookup → mutate clone → upsert` flow would produce.
    async fn upgrade_title_if_synthetic(
        &self,
        sid: &acp::schema::v1::SessionId,
        candidate: &str,
    ) -> bool;

    async fn upgrade_title_if_synthetic_identity(
        &self,
        identity: &SessionIdentity,
        candidate: &str,
    ) -> bool;

    /// Atomically replace `title` for `sid` with the listing agent's current
    /// `session/list` title, whatever the row already holds. Returns `true`
    /// iff the title actually changed. The candidate must be non-empty.
    ///
    /// The CLI owns a session's display name and keeps rewriting it: Copilot
    /// reports the session's *first user message* until it generates a real
    /// summary, then swaps in the summary. [`upgrade_title_if_synthetic`]
    /// cannot recover from that, because the transient first-message echo is
    /// an ordinary non-synthetic title — adopting it once would freeze the row
    /// on it forever. This is the counterpart primitive that lets the poll
    /// re-adopt the agent's current answer.
    ///
    /// Callers must first establish that the listing agent owns this row (in
    /// master that is `row_refreshable_by_connected_agent` plus a session-id
    /// match against that agent's own `session/list`) and must filter
    /// placeholder and injected-context candidates with
    /// [`title_is_displayable`], since this method applies whatever it is
    /// given.
    ///
    /// [`upgrade_title_if_synthetic`]: SessionRegistry::upgrade_title_if_synthetic
    async fn adopt_agent_title(&self, sid: &acp::schema::v1::SessionId, candidate: &str) -> bool;

    async fn adopt_agent_title_identity(&self, identity: &SessionIdentity, candidate: &str)
        -> bool;

    /// Returns true only if the completed ACP listing changed the effective cwd.
    async fn adopt_acp_cwd_identity(
        &self,
        identity: &SessionIdentity,
        revision: u64,
        agent_instance: uuid::Uuid,
        cwd: PathBuf,
    ) -> bool;

    /// A complete listing lacked this row; a newer trusted hook may now win.
    async fn clear_acp_cwd_identity(&self, identity: &SessionIdentity, revision: u64) -> bool;
}

enum PaneConnectionState {
    Connected,
    Failed { was_connected: bool, reason: String },
}

/// Production implementation. Uses `tokio::sync::Mutex` for parity with the
/// existing master state; the critical sections are all sync HashMap ops
/// so a future sync-lock conversion is a mechanical swap.
#[derive(Default)]
struct RegistryState {
    sessions: HashMap<SessionIdentity, SessionInfo>,
    identities_by_raw: HashMap<acp::schema::v1::SessionId, HashSet<SessionIdentity>>,
    active_by_pane: HashMap<String, SessionIdentity>,
    // Startup failures can arrive before create_tab returns the pane to bind.
    pane_connections: HashMap<String, PaneConnectionState>,
    // Full observations are active OFF panes only. Retired GUIDs retain only
    // their epoch fence; forgetting it would admit delayed reset replays.
    native_panes: HashMap<String, NativePaneState>,
    native_retired_epochs: HashMap<uuid::Uuid, u64>,
    next_cwd_revision: u64,
    known_raw_ids: HashSet<String>,
    native_listing_completed_at: HashMap<(String, SessionLocation), u64>,
    native_listing_evidence: HashMap<HistoryRowKey, NativeListingEvidence>,
}

#[derive(Default)]
pub struct InMemoryRegistry {
    inner: Mutex<RegistryState>,
}

impl InMemoryRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn shared() -> Arc<dyn SessionRegistry> {
        Arc::new(Self::new())
    }
}

/// A title is "synthetic" (not the CLI's real chat title) when it's missing,
/// empty, or just the cwd basename — the placeholder a born-bound session
/// starts with before the CLI writes its generated name.
pub(crate) fn title_is_synthetic(info: &SessionInfo) -> bool {
    let cwd_leaf = info.cwd.file_name().and_then(|n| n.to_str()).unwrap_or("");
    match info.title.as_deref() {
        None | Some("") => true,
        Some(t) => {
            t == cwd_leaf
                || info
                    .cli_source
                    .as_ref()
                    .is_some_and(|cli| crate::agent_sessions::title_is_placeholder(cli, t))
        }
    }
}

/// The literal heading the delegate `?<prompt>` flow injects into the prompt it
/// bakes into a freshly launched agent CLI: `"<prompt>\n\n## Terminal Context
/// (pane <id>)\n```…```"` (built in `cli/delegate.rs`, which references this
/// same constant so the two can't drift).
///
/// Agent CLIs (e.g. Copilot) briefly surface a session's *first user message* as
/// its `session/list` title before they generate their real chat summary. For a
/// delegate session that first message is the baked prompt above, so the echoed
/// title contains this marker; a genuine CLI-generated summary never does. That
/// lets [`title_is_injected_context_echo`] tell the two apart.
pub(crate) const TERMINAL_CONTEXT_TITLE_MARKER: &str = "## Terminal Context (pane ";

/// Whether `title` is the delegate's injected first-message echo (contains
/// [`TERMINAL_CONTEXT_TITLE_MARKER`]) rather than a real CLI-generated summary.
///
/// Such a title must not be adopted as a session's display title: it leaks the
/// injected terminal context (pane GUID and the captured pane output included)
/// into the session list for the whole window before the CLI generates its real
/// summary. Callers drop it so the row keeps its own title and the next poll
/// adopts the summary instead.
///
/// This filter is load-bearing for
/// [`SessionRegistry::adopt_agent_title`], which overwrites whatever it is
/// handed: an unfiltered echo would not merely stick (the pre-`adopt` failure
/// mode, where a non-synthetic echo locked the row out of every later upgrade)
/// but actively replace a good title, and be re-adopted on every 5 s poll until
/// the summary lands.
pub(crate) fn title_is_injected_context_echo(title: &str) -> bool {
    title.contains(TERMINAL_CONTEXT_TITLE_MARKER)
}

/// Whether a `session/list` title reported for `cli` is fit to become a
/// session's display name.
///
/// The single source of truth for "is this candidate usable", shared by the
/// producer (`master::titles_from_listing`, which builds the title map) and the
/// destructive consumer (`master::refresh_titles_from_listing`, which feeds
/// [`SessionRegistry::adopt_agent_title`]). An agent's *latest* answer is not
/// automatically a *displayable* one: it can be empty, the delegate's injected
/// first-message echo, or a CLI's own untouched-session placeholder. Keeping
/// both ends on one predicate means a future caller can't reintroduce a
/// clobbering candidate by rebuilding the map by hand.
pub(crate) fn title_is_displayable(cli: Option<&CliSource>, title: &str) -> bool {
    !title.is_empty()
        && !title_is_injected_context_echo(title)
        && !cli.is_some_and(|cli| crate::agent_sessions::title_is_placeholder(cli, title))
}

#[async_trait::async_trait]
impl SessionRegistry for InMemoryRegistry {
    async fn has_pending_native_inference(
        &self,
        provider: &str,
        location: &SessionLocation,
    ) -> bool {
        let state = self.inner.lock().await;
        let now = now_ms();
        state.native_panes.values().any(|pane| {
            let row = &pane.observation;
            row.provider_id.eq_ignore_ascii_case(provider)
                && row.location.as_ref() == Some(location)
                && row.agent_session_id.is_none()
                && pane.bound_identity.is_none()
                && row.inferred_history_key.is_none()
                && !pane.inference_blocked
                && row
                    .pane_created_at_ms
                    .is_some_and(|birth| now.saturating_sub(birth) <= 5 * 60 * 1000)
        })
    }

    async fn refine_native_launch_source(
        &self,
        pane: &str,
        provider: &str,
        location: SessionLocation,
    ) -> bool {
        if !location.is_actionable() {
            return false;
        }
        let mut state = self.inner.lock().await;
        let Some(native) = state.native_panes.get_mut(&pane_key(pane)) else {
            return false;
        };
        if native.observation.provider_id != provider.trim().to_ascii_lowercase()
            || native.observation.agent_session_id.is_some()
            || native.bound_identity.is_some()
            || native
                .observation
                .location
                .as_ref()
                .is_some_and(SessionLocation::is_actionable)
        {
            return false;
        }
        native.observation.location = Some(location);
        true
    }

    async fn enrich_native_from_listing(
        &self,
        provider: &str,
        location: &SessionLocation,
        rows: &[SessionInfo],
        previously_listed: &HashSet<String>,
        started_at_ms: u64,
        completed_at_ms: u64,
        listing_complete: bool,
    ) -> bool {
        // Bound query start and provider activity, not delivery of a retained
        // slow response. SDK updated_at is activity, not session birth.
        const STARTUP_WINDOW_MS: u64 = 5 * 60 * 1000;
        let mut state = self.inner.lock().await;
        let scope = (provider.to_ascii_lowercase(), location.clone());
        if !location.is_actionable()
            || completed_at_ms < started_at_ms
            || state
                .native_listing_completed_at
                .get(&scope)
                .is_some_and(|last| started_at_ms <= *last)
        {
            return false;
        }
        state
            .native_listing_completed_at
            .insert(scope.clone(), completed_at_ms);
        state.native_listing_evidence.retain(|_, evidence| {
            completed_at_ms.saturating_sub(evidence.first_observed_at_ms) <= STARTUP_WINDOW_MS
        });
        let mut panes: Vec<_> = state
            .native_panes
            .iter()
            .filter_map(|(id, pane)| {
                let row = &pane.observation;
                let birth = row.pane_created_at_ms?;
                let sequence = row.pane_creation_sequence?;
                (row.provider_id == scope.0
                    && row.location.as_ref() == Some(location)
                    && row.agent_session_id.is_none()
                    && pane.bound_identity.is_none()
                    && row.inferred_agent_session_id.is_none()
                    && !pane.inference_blocked
                    && birth <= started_at_ms
                    && started_at_ms.saturating_sub(birth) <= STARTUP_WINDOW_MS)
                    .then(|| (birth, sequence, id.clone()))
            })
            .collect();
        panes.sort();
        let first_birth = panes.first().map(|p| p.0);
        let mut candidates = Vec::new();
        let mut ambiguous = false;
        for row in rows {
            let id = row.session_id.to_string();
            let Some(key) = row.history_row_key() else {
                ambiguous |= !state.known_raw_ids.contains(&id);
                continue;
            };
            if key.provider_id != scope.0
                || key.location != *location
                || key.session_universe.is_some()
            {
                ambiguous = true;
                continue;
            }
            if !state.native_listing_evidence.contains_key(&key) {
                if previously_listed.contains(&id) || state.known_raw_ids.contains(&id) {
                    continue;
                }
                state.native_listing_evidence.insert(
                    key.clone(),
                    NativeListingEvidence {
                        first_observed_at_ms: completed_at_ms,
                        provider_activity_at_ms: row.last_activity_at_ms,
                    },
                );
            }
            let evidence = &state.native_listing_evidence[&key];
            if first_birth.is_none_or(|birth| evidence.first_observed_at_ms < birth) {
                continue;
            }
            let Some(activity) = evidence.provider_activity_at_ms else {
                ambiguous = true;
                continue;
            };
            if activity == 0 {
                ambiguous = true;
                continue;
            }
            if first_birth.is_none_or(|birth| activity < birth) {
                continue;
            }
            if activity > evidence.first_observed_at_ms {
                ambiguous = true;
                continue;
            }
            if row.origin == Some(SessionOrigin::AgentPane)
                || row.pane_session_id.is_some()
                || state.native_panes.values().any(|pane| {
                    pane.observation.inferred_agent_session_id.as_deref() == Some(&id)
                        || pane.observation.agent_session_id.as_deref() == Some(&id)
                })
                || rows
                    .iter()
                    .filter(|other| other.session_id == row.session_id)
                    .count()
                    != 1
                || state
                    .identities_by_raw
                    .get(&row.session_id)
                    .is_some_and(|identities| {
                        identities
                            .iter()
                            .any(|identity| identity.history_key.as_ref() != Some(&key))
                    })
            {
                ambiguous = true;
                continue;
            }
            candidates.push((activity, key));
        }
        // Remember every raw ID, including rejected/ambiguous candidates. A later
        // update (or a provider/backend collision) is never evidence of a new ID.
        state
            .known_raw_ids
            .extend(rows.iter().map(|row| row.session_id.to_string()));
        if !listing_complete || panes.is_empty() {
            if !listing_complete && !panes.is_empty() {
                tracing::debug!(
                    target: "session_registry",
                    provider,
                    pending_panes = panes.len(),
                    candidates = candidates.len(),
                    "native identity correlation waits for complete session inventory"
                );
            }
            return false;
        }
        candidates.sort_by_key(|(activity, _)| *activity);
        if ambiguous
            || candidates.len() > panes.len()
            || candidates.windows(2).any(|pair| pair[0].0 == pair[1].0)
            || panes
                .windows(2)
                .any(|pair| pair[0].0 == pair[1].0 && pair[0].1 == pair[1].1)
            || panes
                .iter()
                .zip(&candidates)
                .any(|((birth, _, _), (activity, _))| {
                    activity < birth || activity - birth > STARTUP_WINDOW_MS
                })
        {
            tracing::debug!(
                target: "session_registry",
                provider,
                pending_panes = panes.len(),
                candidates = candidates.len(),
                ambiguous,
                "native identity correlation rejected ambiguous inventory or ordering"
            );
            for (_, _, pane) in &panes {
                if let Some(native) = state.native_panes.get_mut(pane) {
                    native.inference_blocked = true;
                }
            }
            return false;
        }
        // Fewer rows are a staggered launch, not proof of ambiguity. Keep the
        // immutable first-list evidence for the next existing discovery result.
        if candidates.len() < panes.len() {
            return false;
        }
        for ((_, _, pane), (_, key)) in panes.into_iter().zip(candidates) {
            if let Some(native) = state.native_panes.get_mut(&pane) {
                native.observation.inferred_agent_session_id = Some(key.session_id.clone());
                native.observation.inferred_history_key = Some(key);
            }
        }
        true
    }

    async fn register_native_born_binding(
        &self,
        identity: &SessionIdentity,
        pane: &str,
        cwd: &std::path::Path,
        mode_epoch: Option<u64>,
        predecessor: Option<&NativePaneBindingPredecessor>,
    ) -> bool {
        let mut state = self.inner.lock().await;
        register_native_born_binding_locked(
            &mut state,
            identity,
            pane,
            cwd,
            mode_epoch,
            predecessor,
        )
    }

    async fn apply_native_watcher_activity(
        &self,
        cli: &CliSource,
        key: &str,
        event: SessionEvent,
    ) -> Option<bool> {
        let mut state = self.inner.lock().await;
        let owners: Vec<_> = state
            .native_panes
            .values()
            .filter(|native| {
                native
                    .bound_identity
                    .as_ref()
                    .is_some_and(|identity| identity.session_id.to_string() == key)
            })
            .collect();
        if owners.is_empty() {
            return None;
        }
        let mut matches = owners.into_iter().filter_map(|native| {
            let identity = native.bound_identity.as_ref()?;
            let row = state.sessions.get(identity)?;
            (cli.canonical_provider_id().as_deref()
                == Some(native.observation.provider_id.as_str())
                && row.location == SessionLocation::Host
                && row.session_universe.is_none()
                && native_bound_identity(&state, &native.observation).as_ref() == Some(identity))
            .then(|| identity.clone())
        });
        let Some(identity) = matches.next() else {
            return Some(false);
        };
        if matches.next().is_some() {
            return Some(false);
        }
        let event_key = match &event {
            SessionEvent::ToolStarting { key, .. }
            | SessionEvent::ToolCompleted { key }
            | SessionEvent::Notification { key, .. } => key,
            _ => return Some(false),
        };
        if event_key != key {
            return Some(false);
        }
        Some(apply_event_locked(&mut state, event, Some(&identity)))
    }

    async fn apply_native_observation(&self, event: NativeAgentObservation) -> bool {
        let pane_id = uuid::Uuid::parse_str(&event.pane_id).ok();
        let window_id = event.window_id;
        let mode_epoch = event.mode_epoch;
        let mut state = self.inner.lock().await;
        let applied = apply_native_observation_locked(&mut state, event);
        if !applied {
            tracing::debug!(
                target: "session_registry",
                ?pane_id, window_id, mode_epoch,
                "rejected native agent observation (invalid, stale, or unadmitted epoch)"
            );
        }
        applied
    }

    async fn native_pane_observations(&self) -> Vec<NativePaneObservation> {
        let state = self.inner.lock().await;
        native_pane_observations_locked(&state)
    }

    async fn pane_status_snapshot(&self) -> (Vec<PaneAgentStatus>, Vec<NativePaneObservation>) {
        let state = self.inner.lock().await;
        let native = native_pane_observations_locked(&state);
        let mut panes: HashMap<String, PaneAgentStatus> = native
            .iter()
            .map(|row| {
                (
                    pane_key(&row.pane_id),
                    PaneAgentStatus {
                        pane_id: pane_key(&row.pane_id),
                        provider_id: row.provider_id.clone(),
                        status: row.status.clone(),
                        agent_session_id: row.agent_session_id.clone(),
                        inferred_history_key: row.inferred_history_key.clone(),
                        last_activity_at_ms: Some(row.last_activity_at_ms),
                        cwd: pane_status_cwd(&row.provider_id, None, row.cwd.as_ref()),
                        location: row.location.clone(),
                        mode_epoch: Some(row.mode_epoch),
                        window_id: Some(row.window_id),
                        tab_id: Some(row.tab_id.clone()),
                    },
                )
            })
            .collect();
        let mut claims = HashMap::<String, usize>::new();
        for row in state
            .sessions
            .values()
            .filter(|row| source_pane_session_identity(row) && row.status.is_some())
        {
            if let Some(pane) = row.pane_session_id.as_deref() {
                *claims.entry(pane_key(pane)).or_default() += 1;
            }
        }
        for (pane, identity) in &state.active_by_pane {
            // The owner index, not recency or history visibility, grants association.
            // An OFF owner always wins, even before it has a confirmed conversation.
            if panes.contains_key(pane) || pane.trim().is_empty() || claims.get(pane) != Some(&1) {
                continue;
            }
            let Some(row) = state.sessions.get(identity).filter(|row| {
                source_pane_session_identity(row)
                    && row
                        .pane_session_id
                        .as_deref()
                        .is_some_and(|id| pane_key(id) == *pane)
            }) else {
                continue;
            };
            let Some(provider_id) = row
                .provider_id
                .clone()
                .filter(|id| !id.trim().is_empty())
                .or_else(|| {
                    row.cli_source
                        .as_ref()
                        .filter(|cli| !matches!(cli, CliSource::Unknown(_)))
                        .and_then(CliSource::canonical_provider_id)
                })
            else {
                continue;
            };
            let Some(status) = row.status.clone() else {
                continue;
            };
            let cwd = pane_status_cwd(&provider_id, row.origin.as_ref(), Some(&row.cwd));
            panes.insert(
                pane.clone(),
                PaneAgentStatus {
                    pane_id: pane.clone(),
                    provider_id,
                    status,
                    agent_session_id: Some(row.session_id.to_string()),
                    inferred_history_key: None,
                    last_activity_at_ms: row.last_activity_at_ms,
                    cwd,
                    location: row.location.is_actionable().then(|| row.location.clone()),
                    mode_epoch: None,
                    window_id: None,
                    tab_id: None,
                },
            );
        }
        let mut panes: Vec<_> = panes.into_values().collect();
        panes.sort_by(|left, right| left.pane_id.cmp(&right.pane_id));
        (panes, native)
    }

    async fn upsert(&self, info: SessionInfo) {
        let mut guard = self.inner.lock().await;
        upsert_locked(&mut guard, info);
    }

    async fn upsert_if_absent(&self, info: SessionInfo) {
        let mut guard = self.inner.lock().await;
        if !guard
            .sessions
            .contains_key(&SessionIdentity::from_info(&info))
        {
            upsert_locked(&mut guard, info);
        }
    }

    async fn remove(&self, sid: &acp::schema::v1::SessionId) -> Option<SessionInfo> {
        let mut guard = self.inner.lock().await;
        let identity = unique_identity_for_raw(&guard, sid)?;
        remove_identity_locked(&mut guard, &identity)
    }

    async fn remove_if(
        &self,
        sid: &acp::schema::v1::SessionId,
        predicate: &(dyn for<'a> Fn(&'a SessionInfo) -> bool + Sync),
    ) -> Option<SessionInfo> {
        let mut guard = self.inner.lock().await;
        let identity = unique_matching_identity_for_raw(&guard, sid, predicate)?;
        remove_identity_locked(&mut guard, &identity)
    }

    async fn lookup(&self, sid: &acp::schema::v1::SessionId) -> Option<SessionInfo> {
        let guard = self.inner.lock().await;
        let identity = unique_identity_for_raw(&guard, sid)?;
        guard.sessions.get(&identity).cloned()
    }

    async fn remove_identity(&self, identity: &SessionIdentity) -> Option<SessionInfo> {
        let mut guard = self.inner.lock().await;
        remove_identity_locked(&mut guard, identity)
    }

    async fn remove_identity_if(
        &self,
        identity: &SessionIdentity,
        predicate: &(dyn for<'a> Fn(&'a SessionInfo) -> bool + Sync),
    ) -> Option<SessionInfo> {
        let mut guard = self.inner.lock().await;
        if guard
            .sessions
            .get(identity)
            .is_some_and(|current| predicate(current))
        {
            remove_identity_locked(&mut guard, identity)
        } else {
            None
        }
    }

    async fn lookup_identity(&self, identity: &SessionIdentity) -> Option<SessionInfo> {
        let guard = self.inner.lock().await;
        guard.sessions.get(identity).cloned()
    }

    async fn lookup_active_by_pane(&self, pane_session_id: &str) -> Option<SessionInfo> {
        let guard = self.inner.lock().await;
        let identity = guard.active_by_pane.get(&pane_key(pane_session_id))?;
        let info = guard.sessions.get(identity)?;
        active_source_pane_session(info).then(|| info.clone())
    }

    async fn snapshot(&self) -> Vec<SessionInfo> {
        let guard = self.inner.lock().await;
        guard.sessions.values().cloned().collect()
    }

    async fn apply_event(&self, ev: SessionEvent) -> bool {
        let mut guard = self.inner.lock().await;
        apply_event_locked(&mut guard, ev, None)
    }

    async fn record_pane_connected(&self, pane_session_id: &str) {
        self.inner
            .lock()
            .await
            .pane_connections
            .insert(pane_key(pane_session_id), PaneConnectionState::Connected);
    }

    async fn record_pane_connection_failure(&self, pane_session_id: &str, reason: &str) {
        let mut state = self.inner.lock().await;
        let pane = pane_key(pane_session_id);
        let was_connected = matches!(
            state.pane_connections.get(&pane),
            Some(
                PaneConnectionState::Connected
                    | PaneConnectionState::Failed {
                        was_connected: true,
                        ..
                    }
            )
        );
        state.pane_connections.insert(
            pane,
            PaneConnectionState::Failed {
                was_connected,
                reason: reason.to_string(),
            },
        );
    }

    async fn apply_born_bound_event(
        &self,
        ev: SessionEvent,
        location: SessionLocation,
        preserve_live_binding: bool,
    ) -> BornBoundEventResult {
        let mut state = self.inner.lock().await;
        // Reset and registration choose their ordering under the same lock.
        if let SessionEvent::SessionStarted {
            key,
            cli_source,
            pane_session_id,
            cwd,
            ..
        } = &ev
        {
            if state.native_panes.contains_key(&pane_key(pane_session_id)) {
                let applied = if let Some(provider) = cli_source.canonical_provider_id() {
                    let identity = SessionIdentity {
                        session_id: acp::schema::v1::SessionId::new(key.clone()),
                        history_key: HistoryRowKey::new(&provider, location, key, None),
                    };
                    register_native_born_binding_locked(
                        &mut state,
                        &identity,
                        pane_session_id,
                        cwd,
                        None,
                        None,
                    )
                } else {
                    tracing::warn!(target: "session_registry", "native birth has no qualified provider");
                    false
                };
                return BornBoundEventResult {
                    applied,
                    native_pane_owned: true,
                };
            }
        }
        let sid = match &ev {
            SessionEvent::SessionStarted { key, .. } => {
                Some(acp::schema::v1::SessionId::new(key.clone()))
            }
            _ => None,
        };
        let mut applied = !preserve_live_binding && apply_event_locked(&mut state, ev, None);
        if matches!(location, SessionLocation::Wsl { .. }) {
            if let Some(sid) = sid {
                applied |= set_location_locked(&mut state, &sid, location);
            }
        }
        BornBoundEventResult {
            applied,
            native_pane_owned: false,
        }
    }

    async fn set_origin(&self, sid: &acp::schema::v1::SessionId, origin: SessionOrigin) -> bool {
        let mut guard = self.inner.lock().await;
        let Some(identity) = unique_identity_for_raw(&guard, sid) else {
            return false;
        };
        let Some(entry) = guard.sessions.get_mut(&identity) else {
            return false;
        };
        if entry.origin.as_ref() == Some(&origin) {
            return false;
        }
        entry.origin = Some(origin);
        true
    }

    async fn set_location(
        &self,
        sid: &acp::schema::v1::SessionId,
        location: crate::agent_sessions::SessionLocation,
    ) -> bool {
        let mut guard = self.inner.lock().await;
        set_location_locked(&mut guard, sid, location)
    }

    async fn mark_resume_dispatched(
        &self,
        sid: &acp::schema::v1::SessionId,
    ) -> Option<(bool, String)> {
        let mut guard = self.inner.lock().await;
        let identity = unique_identity_for_raw(&guard, sid)?;
        mark_resume_dispatched_identity_locked(&mut guard, &identity)
    }

    async fn mark_resume_dispatched_identity(
        &self,
        identity: &SessionIdentity,
    ) -> Option<(bool, String)> {
        let mut guard = self.inner.lock().await;
        mark_resume_dispatched_identity_locked(&mut guard, identity)
    }

    async fn assign_resume_pane_identity(
        &self,
        identity: &SessionIdentity,
        pane_session_id: String,
    ) -> bool {
        let mut guard = self.inner.lock().await;
        assign_resume_pane_identity_locked(&mut guard, identity, pane_session_id, now_ms())
    }

    async fn upgrade_title_if_synthetic(
        &self,
        sid: &acp::schema::v1::SessionId,
        candidate: &str,
    ) -> bool {
        if candidate.is_empty() {
            return false;
        }
        let mut guard = self.inner.lock().await;
        let Some(identity) = unique_identity_for_raw(&guard, sid) else {
            return false;
        };
        let Some(entry) = guard.sessions.get_mut(&identity) else {
            return false;
        };
        if !title_is_synthetic(entry) {
            return false;
        }
        if entry.title.as_deref() == Some(candidate) {
            return false;
        }
        entry.title = Some(candidate.to_string());
        true
    }

    async fn upgrade_title_if_synthetic_identity(
        &self,
        identity: &SessionIdentity,
        candidate: &str,
    ) -> bool {
        if candidate.is_empty() {
            return false;
        }
        let mut guard = self.inner.lock().await;
        let Some(entry) = guard.sessions.get_mut(identity) else {
            return false;
        };
        if !title_is_synthetic(entry) || entry.title.as_deref() == Some(candidate) {
            return false;
        }
        entry.title = Some(candidate.to_string());
        true
    }

    async fn adopt_agent_title(&self, sid: &acp::schema::v1::SessionId, candidate: &str) -> bool {
        if candidate.is_empty() {
            return false;
        }
        let mut guard = self.inner.lock().await;
        let Some(identity) = unique_identity_for_raw(&guard, sid) else {
            return false;
        };
        let Some(entry) = guard.sessions.get_mut(&identity) else {
            return false;
        };
        if entry.title.as_deref() == Some(candidate) {
            return false;
        }
        entry.title = Some(candidate.to_string());
        true
    }

    async fn adopt_agent_title_identity(
        &self,
        identity: &SessionIdentity,
        candidate: &str,
    ) -> bool {
        if candidate.is_empty() {
            return false;
        }
        let mut guard = self.inner.lock().await;
        let Some(entry) = guard.sessions.get_mut(identity) else {
            return false;
        };
        if entry.title.as_deref() == Some(candidate) {
            return false;
        }
        entry.title = Some(candidate.to_string());
        true
    }

    async fn adopt_acp_cwd_identity(
        &self,
        identity: &SessionIdentity,
        revision: u64,
        agent_instance: uuid::Uuid,
        cwd: PathBuf,
    ) -> bool {
        if !cwd.is_absolute() {
            return false;
        }
        let mut guard = self.inner.lock().await;
        let Some(row) = guard.sessions.get(identity) else {
            return false;
        };
        if row.cwd_evidence.revision != revision
            || !matches!(
                row.provider_id.as_deref(),
                Some("copilot" | "claude" | "opencode")
            )
            || row.location != SessionLocation::Host
            || row.session_universe.is_some()
            || row
                .pane_session_id
                .as_deref()
                .is_some_and(|pane| guard.active_by_pane.get(&pane_key(pane)) != Some(identity))
        {
            return false;
        }
        let changed = row.cwd != cwd;
        if !changed
            && row.cwd_evidence.acp.as_ref() == Some(&cwd)
            && row.cwd_evidence.acp_agent == Some(agent_instance)
            && !row.cwd_evidence.hook_after_acp
        {
            return false;
        }
        let next_revision = advance_cwd_revision(&mut guard);
        let row = guard.sessions.get_mut(identity).expect("checked above");
        row.cwd_evidence.acp = Some(cwd.clone());
        row.cwd_evidence.acp_agent = Some(agent_instance);
        row.cwd_evidence.hook_after_acp = false;
        row.cwd_evidence.revision = next_revision;
        row.cwd = cwd;
        changed
    }

    async fn clear_acp_cwd_identity(&self, identity: &SessionIdentity, revision: u64) -> bool {
        let mut guard = self.inner.lock().await;
        let Some(row) = guard.sessions.get(identity) else {
            return false;
        };
        if row.cwd_evidence.revision != revision || row.cwd_evidence.acp.is_none() {
            return false;
        }
        let next_revision = advance_cwd_revision(&mut guard);
        let row = guard.sessions.get_mut(identity).expect("checked above");
        row.cwd_evidence.acp = None;
        row.cwd_evidence.acp_agent = None;
        row.cwd_evidence.revision = next_revision;
        let newer_hook = row
            .cwd_evidence
            .hook_after_acp
            .then(|| row.cwd_evidence.hook.clone())
            .flatten();
        row.cwd_evidence.hook_after_acp = false;
        if let Some(hook) = newer_hook {
            if row.cwd != hook {
                row.cwd = hook;
                return true;
            }
        }
        false
    }
}

#[allow(dead_code)] // Used through apply_event once Task B forwards hook events.
fn now_ms() -> u64 {
    crate::agent_sessions::system_time_ms(SystemTime::now())
}

fn pane_key(pane_session_id: &str) -> String {
    crate::agent_sessions::pane_key(pane_session_id)
}

fn pane_status_cwd(
    provider: &str,
    origin: Option<&SessionOrigin>,
    cwd: Option<&PathBuf>,
) -> Option<PathBuf> {
    if provider.eq_ignore_ascii_case("antigravity") && origin != Some(&SessionOrigin::AgentPane) {
        return None;
    }
    cwd.filter(|cwd| !cwd.as_os_str().is_empty()).cloned()
}

fn native_pane_observations_locked(state: &RegistryState) -> Vec<NativePaneObservation> {
    state
        .native_panes
        .values()
        .map(|pane| {
            let mut observation = pane.observation.clone();
            let row = pane
                .bound_identity
                .as_ref()
                .filter(|identity| {
                    state.active_by_pane.get(&observation.pane_id) == Some(*identity)
                })
                .and_then(|identity| state.sessions.get(identity))
                .filter(|row| active_source_pane_session(row));
            observation.agent_session_id = row.map(|row| row.session_id.to_string());
            observation.cwd = row
                .map(|row| row.cwd.clone())
                .filter(|cwd| !cwd.as_os_str().is_empty());
            if let Some(row) = row {
                observation.location = row.location.is_actionable().then(|| row.location.clone());
                if let Some(status) = &row.status {
                    observation.status = status.clone();
                    observation.last_activity_at_ms = row
                        .last_activity_at_ms
                        .unwrap_or(observation.last_activity_at_ms);
                }
            }
            observation
        })
        .collect()
}

fn register_native_born_binding_locked(
    state: &mut RegistryState,
    identity: &SessionIdentity,
    pane: &str,
    cwd: &std::path::Path,
    mode_epoch: Option<u64>,
    predecessor: Option<&NativePaneBindingPredecessor>,
) -> bool {
    let Some(key) = identity.history_key.as_ref() else {
        return false;
    };
    if key.session_id != identity.session_id.to_string()
        || !key.location.is_actionable()
        || pane.is_empty()
    {
        return false;
    }
    let pane = pane_key(pane);
    if let Some(row) = state.sessions.get(identity) {
        if row.origin == Some(SessionOrigin::AgentPane)
            || (row
                .pane_session_id
                .as_deref()
                .is_some_and(|owner| pane_key(owner) != pane)
                && !matches!(
                    row.status,
                    Some(AgentStatus::Ended | AgentStatus::Historical)
                ))
        {
            return false;
        }
    }
    let previous = state
        .active_by_pane
        .get(&pane)
        .filter(|owner| *owner != identity)
        .cloned();
    if previous.is_none()
        && state.native_panes.get(&pane).is_some_and(|native| {
            native.observation.provider_id != key.provider_id
                || native
                    .observation
                    .location
                    .as_ref()
                    .is_some_and(|location| location != &key.location)
                || native
                    .observation
                    .agent_session_id
                    .as_ref()
                    .is_some_and(|id| id != key.session_id.as_str())
        })
    {
        tracing::debug!(target: "session_registry", "native birth conflicts with admitted pane identity");
        return false;
    }
    if let Some(previous) = previous.as_ref() {
        let authorized = state.native_panes.get(&pane).is_some_and(|native| {
            native.bound_identity.as_ref() == Some(previous)
                && native_bound_identity(state, &native.observation).as_ref() == Some(previous)
                && predecessor.is_some_and(|expected| {
                    expected.agent_session_id == previous.session_id.to_string()
                        && expected.mode_epoch == native.observation.mode_epoch
                })
                && mode_epoch.is_some_and(|epoch| epoch > native.observation.mode_epoch)
        });
        if !authorized {
            return false;
        }
        // Explicit foreground replacement is not evidence of process exit.
        // Detach before assignment, whose normal resume path ends competitors.
        state.active_by_pane.remove(&pane);
        if let Some(row) = state.sessions.get_mut(previous) {
            row.pane_session_id = None;
            row.status = Some(AgentStatus::Historical);
            row.born_bound_pane = false;
        }
    }
    let created = !state.sessions.contains_key(identity);
    if created {
        let mut row = SessionInfo::new(identity.session_id.clone(), cwd.to_path_buf());
        row.provider_id = Some(key.provider_id.clone());
        row.cli_source = Some(SessionHookCliSource::Known(key.provider_id.clone()).into());
        row.location = key.location.clone();
        row.session_universe = key.session_universe.clone();
        row.status = Some(AgentStatus::Idle);
        row.last_activity_at_ms = Some(now_ms());
        upsert_locked(state, row);
    }
    let revived =
        mark_resume_dispatched_identity_locked(state, identity).is_some_and(|(changed, _)| changed);
    if previous.is_some() {
        let row = state.sessions.get(identity).cloned();
        if let (Some(native), Some(row)) = (state.native_panes.get_mut(&pane), row) {
            // Keep the old epoch until the ordered Reset admits the new one,
            // but never attribute the old owner's activity to its successor.
            native.bound_identity = Some(identity.clone());
            native.observation.agent_session_id = Some(identity.session_id.to_string());
            native.observation.inferred_agent_session_id = None;
            native.observation.inferred_history_key = None;
            native.observation.provider_id = key.provider_id.clone();
            native.observation.location = Some(key.location.clone());
            native.observation.status = row.status.unwrap_or(AgentStatus::Idle);
            native.observation.last_activity_at_ms = row.last_activity_at_ms.unwrap_or_else(now_ms);
        }
    }
    let assigned = assign_resume_pane_identity_locked(state, identity, pane.clone(), now_ms());
    let confirmed = state.sessions.get_mut(identity).is_some_and(|row| {
        if row.pane_session_id.as_deref() != Some(pane.as_str()) {
            return false;
        }
        let changed = !row.born_bound_pane;
        row.born_bound_pane = true;
        changed
    });
    let bind_native = state.native_panes.get(&pane).is_some_and(|native| {
        native.bound_identity.is_none()
            && native_bound_identity(state, &native.observation).as_ref() == Some(identity)
    });
    if let Some(native) = state.native_panes.get_mut(&pane).filter(|_| bind_native) {
        native.bound_identity = Some(identity.clone());
        native.observation.inferred_agent_session_id = None;
        native.observation.inferred_history_key = None;
    }
    created || revived || assigned || confirmed || bind_native
}

fn set_location_locked(
    state: &mut RegistryState,
    sid: &acp::schema::v1::SessionId,
    location: SessionLocation,
) -> bool {
    let Some(identity) = unique_identity_for_raw(state, sid) else {
        return false;
    };
    let Some(mut entry) = remove_identity_locked(state, &identity) else {
        return false;
    };
    let changed = entry.location != location;
    entry.location = location;
    upsert_locked(state, entry);
    changed
}

fn native_bound_identity(
    state: &RegistryState,
    observation: &NativePaneObservation,
) -> Option<SessionIdentity> {
    let mut matches = state.sessions.iter().filter(|(_, row)| {
        active_source_pane_session(row)
            && row
                .pane_session_id
                .as_deref()
                .is_some_and(|pane| pane_key(pane) == observation.pane_id)
    });
    let (identity, row) = matches.next()?;
    if matches.next().is_some()
        || row.origin == Some(SessionOrigin::AgentPane)
        || !(row.born_bound_pane
            || (observation.agent_session_id.is_some()
                && observation
                    .location
                    .as_ref()
                    .is_some_and(SessionLocation::is_actionable)))
        || !row
            .provider_id
            .clone()
            .or_else(|| {
                row.cli_source
                    .as_ref()
                    .and_then(CliSource::canonical_provider_id)
            })
            .is_some_and(|provider| provider.eq_ignore_ascii_case(&observation.provider_id))
        || !observation
            .agent_session_id
            .as_ref()
            .is_none_or(|id| row.session_id.to_string() == *id)
        || observation
            .location
            .as_ref()
            .is_some_and(|location| row.location != *location)
    {
        return None;
    }
    let identity = identity.clone();
    Some(identity)
}

fn apply_native_observation_locked(
    state: &mut RegistryState,
    event: NativeAgentObservation,
) -> bool {
    let Ok(pane_id) = uuid::Uuid::parse_str(&event.pane_id) else {
        return false;
    };
    let provider = event.provider_id.trim().to_ascii_lowercase();
    if pane_id.is_nil()
        || event.tab_id.trim().is_empty()
        || !(crate::agent_registry::is_known_id(&provider)
            || provider.strip_prefix("custom:").is_some_and(|name| {
                !name.trim().is_empty() && !provider.contains(['\r', '\n', '\t'])
            }))
        || event
            .agent_session_id
            .as_ref()
            .is_some_and(|id| id.trim().is_empty() || id.starts_with("pane:"))
    {
        return false;
    }
    let pane = pane_id.to_string();
    let old = state.native_panes.get(&pane);
    let reset = matches!(event.event, NativeObservationKind::Reset { .. });
    if reset {
        if old.is_some_and(|old| event.mode_epoch <= old.observation.mode_epoch)
            || state
                .native_retired_epochs
                .get(&pane_id)
                .is_some_and(|epoch| event.mode_epoch <= *epoch)
        {
            return false;
        }
    } else if old.is_none_or(|old| {
        old.observation.mode_epoch != event.mode_epoch
            || old.observation.provider_id != provider
            || old.observation.window_id != event.window_id
            || old.observation.tab_id != event.tab_id
            || event
                .agent_session_id
                .as_ref()
                .is_some_and(|id| old.observation.agent_session_id.as_ref() != Some(id))
            || event.location.as_ref().is_some_and(|location| {
                old.observation
                    .location
                    .as_ref()
                    .is_some_and(|old| old != location)
            })
    }) {
        return false;
    }
    let generic_activity = matches!(
        event.event,
        NativeObservationKind::Bell
            | NativeObservationKind::Progress {
                progress_state: 0 | 1 | 3 | 4
            }
    );
    let (status, active, removed) = match event.event {
        NativeObservationKind::Reset { tracking_enabled } => {
            (AgentStatus::Idle, !tracking_enabled, false)
        }
        NativeObservationKind::Progress {
            progress_state: 0 | 4,
        } => (AgentStatus::Idle, true, false),
        NativeObservationKind::Progress {
            progress_state: 1 | 3,
        } => (AgentStatus::Working, true, false),
        NativeObservationKind::Progress { progress_state: 2 } => (AgentStatus::Error, true, false),
        NativeObservationKind::Progress { .. } => return false,
        NativeObservationKind::Bell => (AgentStatus::Attention, true, false),
        NativeObservationKind::Removed => (AgentStatus::Ended, false, true),
    };
    let mut observation = NativePaneObservation {
        pane_id: pane.clone(),
        provider_id: provider,
        window_id: event.window_id,
        tab_id: event.tab_id,
        mode_epoch: event.mode_epoch,
        cwd: None,
        agent_session_id: event.agent_session_id.or_else(|| {
            (!reset)
                .then(|| old.and_then(|old| old.observation.agent_session_id.clone()))
                .flatten()
        }),
        location: event
            .location
            .or_else(|| old.and_then(|old| old.observation.location.clone())),
        pane_created_at_ms: old
            .and_then(|old| old.observation.pane_created_at_ms)
            .or(event.pane_created_at_ms),
        pane_creation_sequence: old
            .and_then(|old| old.observation.pane_creation_sequence)
            .or(event.pane_creation_sequence),
        inferred_agent_session_id: None,
        inferred_history_key: None,
        status: status.clone(),
        last_activity_at_ms: now_ms(),
    };
    let mut bound_identity = if reset {
        native_bound_identity(state, &observation)
    } else {
        old.and_then(|old| old.bound_identity.clone())
            .filter(|identity| {
                native_bound_identity(state, &observation).as_ref() == Some(identity)
            })
            .or_else(|| {
                observation
                    .agent_session_id
                    .as_ref()
                    .and_then(|_| native_bound_identity(state, &observation))
            })
    };
    // A different foreground identity needs its authorized birth first. In
    // particular, a lost birth must not consume A's epoch and prevent retry.
    if reset
        && active
        && observation.agent_session_id.is_some()
        && old.is_some_and(|old| {
            old.bound_identity.as_ref().is_some_and(|owner| {
                state.active_by_pane.get(&pane) == Some(owner)
                    && bound_identity.as_ref() != Some(owner)
            })
        })
    {
        return false;
    }
    // A higher epoch can re-admit the same OFF owner after a transfer or
    // reconnect. This is a context update, not a fresh OFF activity baseline.
    let continuation = reset
        && active
        && old.is_some_and(|old| {
            old.observation.provider_id == observation.provider_id
                && old.observation.agent_session_id == observation.agent_session_id
                && (old.observation.location == observation.location
                    || old.observation.location.is_none())
                && old.bound_identity == bound_identity
        });
    if continuation {
        if let Some(old) = old {
            let row = bound_identity
                .as_ref()
                .and_then(|identity| state.sessions.get(identity));
            observation.status = row
                .and_then(|row| row.status.clone())
                .unwrap_or_else(|| old.observation.status.clone());
            observation.last_activity_at_ms = row
                .and_then(|row| row.last_activity_at_ms)
                .unwrap_or(old.observation.last_activity_at_ms);
        }
    }
    if !reset || continuation {
        if let Some(old) = old {
            observation.inferred_agent_session_id =
                old.observation.inferred_agent_session_id.clone();
            observation.inferred_history_key = old.observation.inferred_history_key.clone();
        }
    }
    let inference_blocked =
        (!reset || continuation) && old.is_some_and(|old| old.inference_blocked);
    // Binding an unbound history row requires Terminal's exact resume identity
    // and backend. A provider-only new launch must never guess a history row.
    if reset
        && active
        && bound_identity.is_none()
        && !state.sessions.values().any(|row| {
            active_source_pane_session(row)
                && row
                    .pane_session_id
                    .as_deref()
                    .is_some_and(|value| pane_key(value) == pane)
        })
    {
        if let (Some(id), Some(location)) = (&observation.agent_session_id, &observation.location) {
            if location.is_actionable() {
                let mut candidates = state.sessions.iter().filter(|(_, row)| {
                    row.session_id.to_string() == *id
                        && row.provider_id.as_deref() == Some(observation.provider_id.as_str())
                        && row.location == *location
                        && row.origin != Some(SessionOrigin::AgentPane)
                        && row.pane_session_id.is_none()
                });
                let candidate = candidates.next().map(|(identity, _)| identity.clone());
                if candidates.next().is_none() {
                    if let Some(identity) = candidate {
                        mark_resume_dispatched_identity_locked(state, &identity);
                        assign_resume_pane_identity_locked(
                            state,
                            &identity,
                            pane.clone(),
                            observation.last_activity_at_ms,
                        );
                        bound_identity = Some(identity);
                    }
                }
            }
        }
    }
    if let Some(identity) = bound_identity.as_ref() {
        observation.inferred_agent_session_id = None;
        observation.inferred_history_key = None;
        if removed {
            end_entry(state, identity, observation.last_activity_at_ms);
        } else if let Some(row) = state
            .sessions
            .get_mut(identity)
            .filter(|_| active && !continuation)
        {
            // Generic terminal progress does not prove that a canonical
            // request for input was answered. Native-only BEL has no such context.
            let canonical_attention = row.status == Some(AgentStatus::Attention)
                && (row.attention_reason.is_some()
                    || row
                        .current_tool
                        .as_deref()
                        .is_some_and(crate::agent_sessions::is_user_input_tool));
            if !(generic_activity && canonical_attention) {
                row.status = Some(status);
                row.current_tool = None;
                row.attention_reason = None;
                row.last_error = None;
                row.last_activity_at_ms = Some(observation.last_activity_at_ms);
            }
        }
    }
    if active {
        state.native_retired_epochs.remove(&pane_id);
        state.native_panes.insert(
            pane,
            NativePaneState {
                observation,
                bound_identity,
                inference_blocked,
            },
        );
    } else {
        state.native_panes.remove(&pane);
        state
            .native_retired_epochs
            .entry(pane_id)
            .and_modify(|epoch| *epoch = (*epoch).max(observation.mode_epoch))
            .or_insert(observation.mode_epoch);
        // HashMap iteration visits capacity, not just live entries. Shrink
        // geometrically after retirement rather than rehashing every removal.
        if state.native_panes.capacity() > state.native_panes.len().saturating_mul(4).max(64) {
            state.native_panes.shrink_to_fit();
        }
    }
    true
}

fn advance_cwd_revision(state: &mut RegistryState) -> u64 {
    state.next_cwd_revision = state.next_cwd_revision.saturating_add(1);
    state.next_cwd_revision
}

fn active_source_pane_session(info: &SessionInfo) -> bool {
    matches!(
        info.status,
        Some(
            AgentStatus::Idle | AgentStatus::Working | AgentStatus::Attention | AgentStatus::Error
        )
    ) && source_pane_session_identity(info)
}

fn source_pane_session_identity(info: &SessionInfo) -> bool {
    let id = info.session_id.0.as_ref();
    // Qualified provider identity takes precedence over legacy CLI stamps.
    let is_copilot = info.provider_id.as_deref().map_or_else(
        || info.cli_source.as_ref() == Some(&CliSource::Copilot),
        |provider| provider.trim().eq_ignore_ascii_case("copilot"),
    );
    !id.trim().is_empty()
        && !id.starts_with("pane:")
        && !(is_copilot && id.starts_with("sidekick-"))
}

fn upsert_locked(state: &mut RegistryState, mut info: SessionInfo) {
    info.cwd_evidence.revision = advance_cwd_revision(state);
    state.known_raw_ids.insert(info.session_id.to_string());
    let identity = SessionIdentity::from_info(&info);
    if let Some(old) = state.sessions.get(&identity) {
        if let Some(native) = old
            .pane_session_id
            .as_deref()
            .and_then(|pane| state.native_panes.get(&pane_key(pane)))
        {
            if native.bound_identity.as_ref() == Some(&identity) {
                info.pane_session_id = old.pane_session_id.clone();
                info.born_bound_pane = old.born_bound_pane;
                if !active_source_pane_session(&info) {
                    info.status = old.status.clone();
                    info.current_tool = old.current_tool.clone();
                    info.attention_reason = old.attention_reason.clone();
                    info.last_activity_at_ms = old.last_activity_at_ms;
                }
            }
        }
    }
    if let Some(old) = state.sessions.get(&identity) {
        if let Some(old_pane) = old.pane_session_id.as_deref() {
            let old_pane = pane_key(old_pane);
            if state.active_by_pane.get(&old_pane) == Some(&identity) {
                state.active_by_pane.remove(&old_pane);
            }
        }
    }
    if let Some(pane) = info.pane_session_id.as_deref() {
        if !pane.is_empty() {
            state
                .active_by_pane
                .insert(pane_key(pane), identity.clone());
        }
    }
    state
        .identities_by_raw
        .entry(info.session_id.clone())
        .or_default()
        .insert(identity.clone());
    state.sessions.insert(identity, info);
}

fn remove_identity_locked(
    state: &mut RegistryState,
    identity: &SessionIdentity,
) -> Option<SessionInfo> {
    let removed = state.sessions.remove(identity);
    if let Some(info) = &removed {
        if let Some(pane) = info.pane_session_id.as_deref() {
            let pane = pane_key(pane);
            if state.active_by_pane.get(&pane) == Some(identity) {
                state.active_by_pane.remove(&pane);
            }
        }
        if let Some(identities) = state.identities_by_raw.get_mut(&info.session_id) {
            identities.remove(identity);
            if identities.is_empty() {
                state.identities_by_raw.remove(&info.session_id);
            }
        }
    }
    removed
}

fn unique_identity_for_raw(
    state: &RegistryState,
    sid: &acp::schema::v1::SessionId,
) -> Option<SessionIdentity> {
    let mut identities = state.identities_by_raw.get(sid)?.iter();
    let identity = identities.next()?.clone();
    identities.next().is_none().then_some(identity)
}

fn mark_resume_dispatched_identity_locked(
    state: &mut RegistryState,
    identity: &SessionIdentity,
) -> Option<(bool, String)> {
    let revision = advance_cwd_revision(state);
    let row = state.sessions.get_mut(identity)?;
    let current_label = match &row.status {
        Some(status) => format!("{status:?}"),
        None => "Idle".to_string(),
    };
    if matches!(
        row.status,
        Some(AgentStatus::Historical | AgentStatus::Ended)
    ) {
        if let Some(pane) = row.pane_session_id.take() {
            let pane = pane_key(&pane);
            if state.active_by_pane.get(&pane) == Some(identity) {
                state.active_by_pane.remove(&pane);
            }
        }
        row.born_bound_pane = false;
        row.bound_pid = None;
        row.status = Some(AgentStatus::Idle);
        row.cwd_evidence.revision = revision;
        row.last_activity_at_ms = Some(now_ms());
        Some((true, "Idle".to_string()))
    } else {
        Some((false, current_label))
    }
}

fn assign_resume_pane_identity_locked(
    state: &mut RegistryState,
    identity: &SessionIdentity,
    pane_session_id: String,
    now: u64,
) -> bool {
    let pane_session_id = pane_key(&pane_session_id);
    if let Some(PaneConnectionState::Failed {
        was_connected: false,
        reason,
    }) = state.pane_connections.get(&pane_session_id)
    {
        let Some(entry) = state.sessions.get(identity) else {
            return false;
        };
        if entry.pane_session_id.as_deref().is_some_and(|pane| {
            pane != pane_session_id
                || !entry.born_bound_pane
                || matches!(
                    entry.status,
                    Some(AgentStatus::Working | AgentStatus::Attention)
                )
        }) && matches!(
            entry.status,
            Some(
                AgentStatus::Idle
                    | AgentStatus::Working
                    | AgentStatus::Attention
                    | AgentStatus::Error
            )
        ) {
            return false;
        }
        let reason = reason.clone();
        return restore_failed_resume_identity_locked(state, identity, reason, now);
    }
    if let Some(previous) = state.active_by_pane.get(&pane_session_id).cloned() {
        if &previous != identity {
            let _ = end_entry(state, &previous, now);
        }
    }
    let revision = advance_cwd_revision(state);
    let Some(entry) = state.sessions.get_mut(identity) else {
        return false;
    };
    if entry.pane_session_id.as_deref() == Some(pane_session_id.as_str()) {
        return false;
    }
    if let Some(old_pane) = entry.pane_session_id.take() {
        let old_pane = pane_key(&old_pane);
        if old_pane != pane_session_id && state.active_by_pane.get(&old_pane) == Some(identity) {
            state.active_by_pane.remove(&old_pane);
        }
    }
    entry.pane_session_id = Some(pane_session_id.clone());
    let previous_activity_at_ms = entry.last_activity_at_ms;
    entry.cwd_evidence.acp = None;
    entry.cwd_evidence.acp_agent = None;
    entry.cwd_evidence.hook_after_acp = false;
    entry.cwd_evidence.revision = revision;
    entry.last_activity_at_ms = Some(now);
    entry.born_bound_pane = true;
    state
        .active_by_pane
        .insert(pane_session_id.clone(), identity.clone());
    for native in state.native_panes.values_mut() {
        if native.observation.inferred_agent_session_id.as_deref()
            == Some(identity.session_id.0.as_ref())
        {
            native.observation.inferred_agent_session_id = None;
            native.observation.inferred_history_key = None;
        }
    }
    // Explicit resume can finish after OFF admission. Link only at this
    // authoritative born-bound assignment, never from a later history upsert.
    if let Some(native) = state.native_panes.get(&pane_session_id) {
        if native_bound_identity(state, &native.observation).as_ref() == Some(identity) {
            let activity = native.bound_identity.is_none().then(|| {
                (
                    native.observation.status.clone(),
                    native.observation.last_activity_at_ms,
                )
            });
            if let Some((status, activity_at_ms)) = activity {
                if let Some(row) = state.sessions.get_mut(identity) {
                    // Idle's resume timestamp is bookkeeping, not evidence
                    // newer than activity already observed in this OFF pane.
                    let canonical_activity = matches!(
                        row.status,
                        Some(AgentStatus::Working | AgentStatus::Attention | AgentStatus::Error)
                    ) || row.current_tool.is_some()
                        || row.attention_reason.is_some()
                        || row.last_error.is_some();
                    if canonical_activity
                        && previous_activity_at_ms.is_some_and(|time| time >= activity_at_ms)
                    {
                        row.last_activity_at_ms = previous_activity_at_ms;
                    } else {
                        row.status = Some(status);
                        row.last_activity_at_ms = Some(activity_at_ms);
                        row.current_tool = None;
                        row.attention_reason = None;
                        row.last_error = None;
                    }
                }
            }
            if let Some(native) = state.native_panes.get_mut(&pane_session_id) {
                native.bound_identity = Some(identity.clone());
                native.observation.inferred_agent_session_id = None;
                native.observation.inferred_history_key = None;
            }
        }
    }
    if let Some(PaneConnectionState::Failed { reason, .. }) =
        state.pane_connections.get(&pane_session_id)
    {
        let event = SessionEvent::ConnectionFailed {
            pane_session_id,
            reason: reason.clone(),
        };
        apply_event_locked(state, event, None);
    }
    true
}

fn unique_matching_identity_for_raw(
    state: &RegistryState,
    sid: &acp::schema::v1::SessionId,
    predicate: &(dyn for<'a> Fn(&'a SessionInfo) -> bool + Sync),
) -> Option<SessionIdentity> {
    let mut matches = state
        .identities_by_raw
        .get(sid)?
        .iter()
        .filter(|identity| state.sessions.get(*identity).is_some_and(predicate));
    let identity = matches.next()?.clone();
    matches.next().is_none().then_some(identity)
}

#[allow(dead_code)] // Used through apply_event once Task B forwards hook events.
fn end_entry(state: &mut RegistryState, identity: &SessionIdentity, now: u64) -> bool {
    let revision = advance_cwd_revision(state);
    let Some(entry) = state.sessions.get_mut(identity) else {
        return false;
    };
    entry.cwd_evidence.revision = revision;
    entry.status = Some(AgentStatus::Ended);
    if let Some(pane) = entry.pane_session_id.take() {
        let pane = pane_key(&pane);
        if state.active_by_pane.get(&pane) == Some(identity) {
            state.active_by_pane.remove(&pane);
        }
    }
    entry.born_bound_pane = false;
    entry.current_tool = None;
    entry.attention_reason = None;
    entry.attention_timestamp_ms = None;
    entry.attention_cooldown_until_ms = None;
    entry.last_activity_at_ms = Some(now);
    true
}

fn restore_failed_resume_identity_locked(
    state: &mut RegistryState,
    identity: &SessionIdentity,
    reason: String,
    now: u64,
) -> bool {
    if state.sessions.get(identity).is_none_or(|entry| {
        entry.status == Some(AgentStatus::Historical)
            && entry.pane_session_id.is_none()
            && entry.last_error.as_deref() == Some(reason.as_str())
    }) {
        return false;
    }
    end_entry(state, identity, now);
    let entry = state
        .sessions
        .get_mut(identity)
        .expect("entry was just ended");
    entry.status = Some(AgentStatus::Historical);
    entry.last_error = Some(reason);
    entry.bound_pid = None;
    true
}

#[allow(dead_code)] // Task B calls this via SessionRegistry::apply_event.
fn apply_event_locked(
    state: &mut RegistryState,
    ev: SessionEvent,
    qualified_activity: Option<&SessionIdentity>,
) -> bool {
    apply_event_qualified_at_ms(state, ev, qualified_activity, now_ms())
}

#[cfg(test)]
fn apply_event_at_ms(state: &mut RegistryState, ev: SessionEvent, now: u64) -> bool {
    apply_event_qualified_at_ms(state, ev, None, now)
}

fn apply_event_qualified_at_ms(
    state: &mut RegistryState,
    ev: SessionEvent,
    qualified_activity: Option<&SessionIdentity>,
    now: u64,
) -> bool {
    let unqualified_activity_key = match &ev {
        SessionEvent::ToolStarting { key, .. }
        | SessionEvent::ToolReturned { key, .. }
        | SessionEvent::ToolCompleted { key }
        | SessionEvent::Notification { key, .. }
            if qualified_activity.is_none() =>
        {
            Some(key)
        }
        _ => None,
    };
    // OFF activity belongs to OSC or the explicitly qualified watcher path,
    // never an unqualified hook sharing the same raw SDK session ID.
    if unqualified_activity_key.is_some_and(|key| {
        state.native_panes.values().any(|native| {
            native.bound_identity.as_ref().is_some_and(|identity| {
                identity.session_id.0.as_ref() == key.as_str()
                    && state.active_by_pane.get(&native.observation.pane_id) == Some(identity)
                    && native_bound_identity(state, &native.observation).as_ref() == Some(identity)
            })
        })
    }) {
        return false;
    }
    let protected_pane = match &ev {
        SessionEvent::SessionStarted {
            pane_session_id, ..
        }
        | SessionEvent::ConnectionFailed {
            pane_session_id, ..
        }
        | SessionEvent::SessionUnbound {
            pane_session_id, ..
        }
        | SessionEvent::PaneClosed { pane_session_id } => Some(pane_key(pane_session_id)),
        SessionEvent::SessionStopped { key, .. } => {
            unique_identity_for_raw(state, &acp::schema::v1::SessionId::new(key.clone()))
                .and_then(|identity| state.sessions.get(&identity))
                .and_then(|row| row.pane_session_id.as_deref())
                .map(pane_key)
        }
        _ => None,
    };
    if protected_pane
        .and_then(|pane| state.native_panes.get(&pane))
        .is_some()
    {
        return false;
    }
    let ev = match ev {
        SessionEvent::SessionStarted {
            key,
            cli_source,
            pane_session_id,
            cwd,
            title,
        } => {
            let title = if crate::agent_sessions::title_is_placeholder(&cli_source, &title) {
                String::new()
            } else {
                title
            };
            SessionEvent::SessionStarted {
                key,
                cli_source,
                pane_session_id: pane_key(&pane_session_id),
                cwd,
                title,
            }
        }
        SessionEvent::WorkingDirectoryChanged {
            key,
            pane_session_id,
            cwd,
        } => SessionEvent::WorkingDirectoryChanged {
            key,
            pane_session_id: pane_key(&pane_session_id),
            cwd,
        },
        SessionEvent::SessionRenamed {
            key,
            pane_session_id,
            title,
        } => SessionEvent::SessionRenamed {
            key,
            pane_session_id: pane_key(&pane_session_id),
            title,
        },
        SessionEvent::SessionUnbound {
            key,
            pane_session_id,
        } => SessionEvent::SessionUnbound {
            key,
            pane_session_id: pane_key(&pane_session_id),
        },
        SessionEvent::SessionStoppedIfOwner {
            key,
            pane_session_id,
            reason,
        } => SessionEvent::SessionStoppedIfOwner {
            key,
            pane_session_id: pane_session_id.map(|pane| pane_key(&pane)),
            reason,
        },
        SessionEvent::ConnectionFailed {
            pane_session_id,
            reason,
        } => SessionEvent::ConnectionFailed {
            pane_session_id: pane_key(&pane_session_id),
            reason,
        },
        SessionEvent::PaneClosed { pane_session_id } => SessionEvent::PaneClosed {
            pane_session_id: pane_key(&pane_session_id),
        },
        SessionEvent::ResumePaneAssigned {
            key,
            pane_session_id,
        } => SessionEvent::ResumePaneAssigned {
            key,
            pane_session_id: pane_key(&pane_session_id),
        },
        other => other,
    };

    match ev {
        SessionEvent::SessionStarted {
            key,
            cli_source,
            pane_session_id,
            cwd,
            title,
        } => {
            let sid = acp::schema::v1::SessionId::new(key.clone());
            let pane_known = !pane_session_id.is_empty();
            let existing_identity = unique_identity_for_raw(state, &sid);

            // GUARD: PowerShell shell-integration hooks fire from wherever
            // an agent ran a tool, NOT from the agent's home pane. For
            // agent panes (origin=AgentPane, where the wta-helper TUI
            // owns the pane) master already set the authoritative
            // pane_session_id at new_session/load_session time from
            // _meta.wta.pane_session_id. A SessionStarted hook arriving
            // later with a DIFFERENT pane (e.g. the workspace shell where
            // a Get-ChildItem ran) must NOT overwrite the helper's pane,
            // because doing so:
            //   1. Breaks focus: session management Enter on the agent-pane row sends the
            //      shell-pane GUID to wtcli, which focuses the wrong pane.
            //   2. Cross-contaminates: multiple agents running tools in
            //      the same shell all claim that shell's pane, so master's
            //      active_by_pane handoff ends each other in turn.
            //
            // For Class B (origin=Unknown, e.g. user typed `gemini` in
            // pwsh) the shell pane IS the agent pane, so the hook is
            // authoritative. The guard only triggers when the existing
            // row is firmly an agent pane AND already has a pane bound.
            let is_protected_agent_pane = existing_identity
                .as_ref()
                .and_then(|identity| state.sessions.get(identity))
                .map(|s| s.origin == Some(SessionOrigin::AgentPane) && s.pane_session_id.is_some())
                .unwrap_or(false);
            if is_protected_agent_pane {
                // Skip the pane mutation entirely. Update only the
                // activity heartbeat so master still knows the session
                // is alive.
                let entry = state
                    .sessions
                    .get_mut(
                        existing_identity
                            .as_ref()
                            .expect("protected row has a unique identity"),
                    )
                    .expect("just verified by lookup");
                entry.last_activity_at_ms = Some(now);
                // Refresh title if the hook brought a non-empty one and
                // we don't already have a better one (existing title
                // wins on non-empty — agent-pane sessions normally have
                // their title set from the chat content, not from cwd).
                if entry.title.is_none() && !title.is_empty() {
                    entry.title = Some(title);
                }
                return true;
            }

            // A pane that WTA bound itself during `/sessions` resume belongs to
            // exactly one session id: the one the user asked to resume. Copilot's
            // `--resume` boots a throwaway bootstrap session first and only
            // switches to the requested one seconds later, so its (deferred)
            // SessionStart hook reports the *bootstrap* id against the resumed
            // pane's GUID. Taking the handoff would end the resumed row, and
            // terminal-state rows refuse resurrection, so the row would sit at
            // Ended for the rest of the CLI's life while the watcher's status
            // fallback is silently dropped. Require the hook's session id to
            // match the pane's born-bound owner; a mismatch still records the
            // session, it just gets no pane binding.
            let pane_owned_by_other_born_bound = state
                .active_by_pane
                .get(&pane_session_id)
                .filter(|owner| Some(*owner) != existing_identity.as_ref())
                .and_then(|owner| state.sessions.get(owner))
                .is_some_and(|owner| {
                    owner.born_bound_pane
                        && matches!(
                            owner.status,
                            Some(AgentStatus::Idle | AgentStatus::Working | AgentStatus::Attention)
                        )
                });
            let pane_known = pane_known && !pane_owned_by_other_born_bound;

            if pane_known {
                if let Some(previous) = state.active_by_pane.get(&pane_session_id).cloned() {
                    if Some(&previous) != existing_identity.as_ref() {
                        let _ = end_entry(state, &previous, now);
                    }
                }
            }

            let is_new_entry = existing_identity.is_none();
            let mut entry = existing_identity
                .as_ref()
                .and_then(|identity| remove_identity_locked(state, identity))
                .unwrap_or_else(|| SessionInfo::new(sid.clone(), cwd.clone()));
            // remove_identity_locked already released only this row's pane binding.
            let resumed = matches!(
                entry.status,
                Some(AgentStatus::Historical | AgentStatus::Ended)
            );
            let new_pane = if pane_known {
                Some(pane_session_id.as_str())
            } else {
                None
            };
            if resumed || entry.pane_session_id.as_deref() != new_pane {
                entry.cwd_evidence.acp = None;
                entry.cwd_evidence.acp_agent = None;
                entry.cwd_evidence.hook_after_acp = false;
            }
            entry.cwd_evidence.hook = Some(cwd.clone());
            entry.cwd_evidence.hook_after_acp = entry.cwd_evidence.acp.is_some();
            entry.cwd = entry.cwd_evidence.acp.clone().unwrap_or(cwd);
            if !title.is_empty() {
                entry.title = Some(title);
            }
            entry.provider_id = cli_source.canonical_provider_id();
            entry.cli_source = Some(cli_source);
            if entry.location == SessionLocation::Unknown {
                entry.location = SessionLocation::Host;
            }
            // Status baseline. Preserve a live status on an EXISTING row instead
            // of clobbering it to Idle: some CLIs fire activity hooks before
            // `session.start` — e.g. Copilot sends `prompt.submit` (→ Working)
            // ~2 s before its `session.start`, so an unconditional reset here
            // blanks the row to Idle for the rest of the turn (until the next
            // `tool.starting`). This is the authoritative reducer behind
            // `sessions/list`, so the clobber is what the user actually saw.
            // Only (re)baseline to Idle for a new row or one being revived from a
            // non-live terminal state (Ended/Error/Historical, e.g. resume /
            // reconnect); Working/Attention/Idle are preserved. Mirrors the
            // resurrection guards on ToolStarting/ToolCompleted below and the
            // helper-side reducer in `agent_sessions.rs`.
            if is_new_entry
                || matches!(
                    entry.status,
                    Some(AgentStatus::Ended | AgentStatus::Error | AgentStatus::Historical)
                )
            {
                entry.status = Some(AgentStatus::Idle);
                entry.last_error = None;
                entry.attention_reason = None;
                entry.attention_timestamp_ms = None;
                entry.attention_cooldown_until_ms = None;
                entry.current_tool = None;
            }
            entry.last_activity_at_ms = Some(now);
            if pane_known {
                entry.pane_session_id = Some(pane_session_id.clone());
            } else {
                entry.pane_session_id = None;
            }
            // The CLI's own hook has now claimed (or disclaimed) this pane, so
            // the WTA-owned resume binding no longer needs protecting: a later
            // session started in the same pane may take over normally.
            entry.born_bound_pane = false;
            upsert_locked(state, entry);
            true
        }
        SessionEvent::SessionRenamed {
            key,
            pane_session_id,
            title,
        } => {
            if pane_session_id.is_empty() || title.trim().is_empty() {
                return false;
            }
            let sid = acp::schema::v1::SessionId::new(key);
            let Some(identity) = unique_identity_for_raw(state, &sid) else {
                return false;
            };
            if state.active_by_pane.get(&pane_session_id) != Some(&identity) {
                return false;
            }
            let Some(entry) = state.sessions.get_mut(&identity) else {
                return false;
            };
            if entry.cli_source != Some(crate::agent_sessions::CliSource::OpenCode)
                || entry.pane_session_id.as_deref() != Some(pane_session_id.as_str())
                || !matches!(
                    entry.status,
                    Some(
                        AgentStatus::Idle
                            | AgentStatus::Working
                            | AgentStatus::Attention
                            | AgentStatus::Error
                    )
                )
                || entry.title.as_deref() == Some(title.as_str())
            {
                return false;
            }
            entry.title = Some(title);
            entry.last_activity_at_ms = Some(now);
            true
        }
        SessionEvent::WorkingDirectoryChanged {
            key,
            pane_session_id,
            cwd,
        } => {
            if cwd.as_os_str().is_empty() || pane_session_id.is_empty() {
                return false;
            }
            let sid = acp::schema::v1::SessionId::new(key);
            let Some(identity) = unique_identity_for_raw(state, &sid) else {
                return false;
            };
            if state.active_by_pane.get(&pane_session_id) != Some(&identity) {
                return false;
            }
            let Some(entry) = state.sessions.get_mut(&identity) else {
                return false;
            };
            if !matches!(
                entry.status,
                Some(
                    AgentStatus::Idle
                        | AgentStatus::Working
                        | AgentStatus::Attention
                        | AgentStatus::Error
                )
            ) {
                return false;
            }
            if entry.cwd_evidence.hook.as_ref() == Some(&cwd)
                && entry.cwd_evidence.acp.is_none()
                && entry.cwd == cwd
            {
                return false;
            }
            let revision = advance_cwd_revision(state);
            let entry = state.sessions.get_mut(&identity).expect("checked above");
            entry.cwd_evidence.hook = Some(cwd.clone());
            entry.cwd_evidence.hook_after_acp = entry.cwd_evidence.acp.is_some();
            entry.cwd_evidence.revision = revision;
            if entry.cwd_evidence.acp.is_some() || entry.cwd == cwd {
                return false;
            }
            entry.cwd = cwd;
            entry.last_activity_at_ms = Some(now);
            true
        }
        SessionEvent::ToolStarting { key, tool_name } => {
            let sid = acp::schema::v1::SessionId::new(key);
            let Some(identity) = qualified_activity
                .cloned()
                .or_else(|| unique_identity_for_raw(state, &sid))
            else {
                return false;
            };
            let Some(entry) = state.sessions.get_mut(&identity) else {
                return false;
            };
            // Refuse to resurrect terminal-state rows. If a prior
            // SessionStarted at the same pane ended this row (master's
            // active_by_pane handoff), a straggling ToolStarting hook
            // would re-promote status to Working while pane_session_id
            // stays None — the row would appear as "Working with no
            // pane" in session management view, fail decide_enter_action's LiveWithoutPane
            // guard, and visually duplicate the synthetic pane:<guid>
            // row that took over the binding. Reject the resurrection
            // so the demotion stays sticky and session management view shows a single Live
            // row at the pane.
            if matches!(
                entry.status,
                Some(AgentStatus::Ended | AgentStatus::Historical)
            ) {
                return false;
            }
            entry.status = Some(AgentStatus::Working);
            entry.attention_cooldown_until_ms = None;
            entry.current_tool = Some(tool_name);
            entry.last_activity_at_ms = Some(now);
            true
        }
        SessionEvent::ToolReturned { key, timestamp_ms } => {
            let sid = acp::schema::v1::SessionId::new(key);
            let Some(identity) = qualified_activity
                .cloned()
                .or_else(|| unique_identity_for_raw(state, &sid))
            else {
                return false;
            };
            let Some(entry) = state.sessions.get_mut(&identity) else {
                return false;
            };
            if entry.cli_source != Some(CliSource::Copilot)
                || !matches!(
                    entry.status,
                    Some(AgentStatus::Working | AgentStatus::Attention)
                )
            {
                return false;
            }
            if entry.status == Some(AgentStatus::Attention)
                && entry
                    .attention_cooldown_until_ms
                    .is_some_and(|until| now < until)
            {
                tracing::debug!(target: "session_registry", session_id = %sid, attention_cooldown_until_ms = entry.attention_cooldown_until_ms, "ignored tool return during attention cooldown");
                return false;
            }
            if entry.status == Some(AgentStatus::Attention)
                && timestamp_ms
                    .zip(entry.attention_timestamp_ms)
                    .is_some_and(|(returned, attention)| returned < attention)
            {
                tracing::debug!(target: "session_registry", session_id = %sid, timestamp_ms, attention_timestamp_ms = entry.attention_timestamp_ms, "ignored tool return predating waiting notification");
                return false;
            }
            entry.status = Some(AgentStatus::Working);
            entry.current_tool = None;
            entry.attention_reason = None;
            entry.attention_timestamp_ms = None;
            entry.attention_cooldown_until_ms = None;
            entry.last_activity_at_ms = Some(now);
            true
        }
        SessionEvent::ToolCompleted { key } => {
            let sid = acp::schema::v1::SessionId::new(key);
            let Some(identity) = qualified_activity
                .cloned()
                .or_else(|| unique_identity_for_raw(state, &sid))
            else {
                return false;
            };
            let Some(entry) = state.sessions.get_mut(&identity) else {
                return false;
            };
            // Same resurrection guard as ToolStarting — a stale
            // ToolCompleted on an Ended row would either be a no-op or
            // (worse) drop current_tool that some other resurrected
            // state depended on. Treat it as a no-op for terminal rows.
            if matches!(
                entry.status,
                Some(AgentStatus::Ended | AgentStatus::Historical)
            ) {
                return false;
            }
            if matches!(
                entry.status,
                Some(AgentStatus::Working | AgentStatus::Attention)
            ) {
                entry.status = Some(AgentStatus::Idle);
                entry.attention_reason = None;
                entry.attention_timestamp_ms = None;
                entry.attention_cooldown_until_ms = None;
            }
            entry.current_tool = None;
            entry.last_activity_at_ms = Some(now);
            true
        }
        SessionEvent::Notification {
            key,
            message,
            timestamp_ms,
        } => {
            let sid = acp::schema::v1::SessionId::new(key);
            let Some(identity) = qualified_activity
                .cloned()
                .or_else(|| unique_identity_for_raw(state, &sid))
            else {
                return false;
            };
            let Some(entry) = state.sessions.get_mut(&identity) else {
                return false;
            };
            // Same resurrection guard — a stale Notification on an
            // Ended row would flip it to Attention and surface a
            // spurious "agent needs input" prompt for a session that
            // has already been ended.
            if matches!(
                entry.status,
                Some(AgentStatus::Ended | AgentStatus::Historical)
            ) {
                return false;
            }
            entry.attention_timestamp_ms = if entry.status == Some(AgentStatus::Attention) {
                entry.attention_timestamp_ms.max(timestamp_ms)
            } else {
                timestamp_ms
            };
            entry.status = Some(AgentStatus::Attention);
            entry.attention_cooldown_until_ms = (entry.cli_source == Some(CliSource::Copilot))
                .then_some(
                    now.saturating_add(crate::agent_sessions::COPILOT_ATTENTION_COOLDOWN_MS),
                );
            entry.attention_reason = Some(message);
            entry.last_activity_at_ms = Some(now);
            true
        }
        SessionEvent::SessionUnbound {
            key,
            pane_session_id,
        } => {
            if pane_session_id.is_empty() {
                return false;
            }
            let sid = acp::schema::v1::SessionId::new(key);
            let Some(identity) = unique_identity_for_raw(state, &sid) else {
                return false;
            };
            if state.active_by_pane.get(&pane_session_id) != Some(&identity)
                || !state.sessions.get(&identity).is_some_and(|row| {
                    row.cli_source == Some(crate::agent_sessions::CliSource::OpenCode)
                        && row.pane_session_id.as_deref() == Some(pane_session_id.as_str())
                        && matches!(
                            row.status,
                            Some(
                                AgentStatus::Idle
                                    | AgentStatus::Working
                                    | AgentStatus::Attention
                                    | AgentStatus::Error
                            )
                        )
                })
            {
                return false;
            }
            state.active_by_pane.remove(&pane_session_id);
            let revision = advance_cwd_revision(state);
            let row = state.sessions.get_mut(&identity).expect("checked above");
            row.pane_session_id = None;
            row.born_bound_pane = false;
            row.cwd_evidence.revision = revision;
            true
        }
        SessionEvent::SessionStoppedIfOwner {
            key,
            pane_session_id,
            reason,
        } => {
            let sid = acp::schema::v1::SessionId::new(key.clone());
            let Some(identity) = unique_identity_for_raw(state, &sid) else {
                return false;
            };
            if !state.sessions.get(&identity).is_some_and(|row| {
                row.cli_source == Some(crate::agent_sessions::CliSource::OpenCode)
                    && matches!(
                        row.status,
                        Some(
                            AgentStatus::Idle
                                | AgentStatus::Working
                                | AgentStatus::Attention
                                | AgentStatus::Error
                        )
                    )
                    && row.pane_session_id == pane_session_id
                    && pane_session_id
                        .as_deref()
                        .is_none_or(|pane| state.active_by_pane.get(pane) == Some(&identity))
            }) {
                return false;
            }
            apply_event_qualified_at_ms(
                state,
                SessionEvent::SessionStopped { key, reason },
                qualified_activity,
                now,
            )
        }
        SessionEvent::SessionStopped { key, reason } => {
            let sid = acp::schema::v1::SessionId::new(key);
            let Some(identity) = unique_identity_for_raw(state, &sid) else {
                return false;
            };
            let reason_keeps_session_alive = reason == "complete";
            let pane_still_live = state
                .sessions
                .get(&identity)
                .and_then(|s| s.pane_session_id.as_deref())
                .map(|p| state.active_by_pane.get(&pane_key(p)) == Some(&identity))
                .unwrap_or(false);
            let is_agent_pane_session = state
                .sessions
                .get(&identity)
                .map(|s| s.origin == Some(SessionOrigin::AgentPane))
                .unwrap_or(false);
            let revision = advance_cwd_revision(state);
            let Some(entry) = state.sessions.get_mut(&identity) else {
                return false;
            };
            if is_agent_pane_session && pane_still_live && reason_keeps_session_alive {
                entry.status = Some(AgentStatus::Idle);
            } else {
                entry.status = Some(AgentStatus::Ended);
                entry.cwd_evidence.revision = revision;
                if let Some(pane) = entry.pane_session_id.take() {
                    let pane = pane_key(&pane);
                    if state.active_by_pane.get(&pane) == Some(&identity) {
                        state.active_by_pane.remove(&pane);
                    }
                }
                entry.born_bound_pane = false;
            }
            entry.current_tool = None;
            entry.attention_reason = None;
            entry.attention_timestamp_ms = None;
            entry.attention_cooldown_until_ms = None;
            entry.last_activity_at_ms = Some(now);
            true
        }
        SessionEvent::PaneClosed { pane_session_id } => {
            if matches!(
                state.pane_connections.get(&pane_session_id),
                Some(PaneConnectionState::Connected)
            ) {
                state.pane_connections.remove(&pane_session_id);
            }
            let Some(sid) = state.active_by_pane.remove(&pane_session_id) else {
                return false;
            };
            let revision = advance_cwd_revision(state);
            let Some(entry) = state.sessions.get_mut(&sid) else {
                return false;
            };
            entry.status = Some(AgentStatus::Ended);
            entry.cwd_evidence.revision = revision;
            entry.pane_session_id = None;
            entry.born_bound_pane = false;
            entry.current_tool = None;
            entry.attention_reason = None;
            entry.attention_timestamp_ms = None;
            entry.attention_cooldown_until_ms = None;
            entry.last_activity_at_ms = Some(now);
            true
        }
        SessionEvent::ConnectionFailed {
            pane_session_id,
            reason,
        } => {
            let Some(sid) = state.active_by_pane.get(&pane_session_id).cloned() else {
                return false;
            };
            if state.sessions.get(&sid).is_some_and(|entry| {
                entry.born_bound_pane
                    && matches!(entry.status, Some(AgentStatus::Idle | AgentStatus::Error))
            }) && matches!(
                state.pane_connections.get(&pane_session_id),
                Some(PaneConnectionState::Failed {
                    was_connected: false,
                    ..
                })
            ) {
                return restore_failed_resume_identity_locked(state, &sid, reason, now);
            }
            let Some(entry) = state.sessions.get_mut(&sid) else {
                return false;
            };
            // Idempotent, unlike the `remove`-based PaneClosed arm above: the
            // pane binding survives a failure, so a repeat would otherwise
            // re-report a change every time. Every helper in the window
            // observes the same WT `connection_state: failed` and publishes
            // its own copy, so without this each one re-broadcasts
            // `sessions/changed` for a row that already says exactly this.
            if entry.status == Some(AgentStatus::Error)
                && entry.last_error.as_deref() == Some(reason.as_str())
            {
                return false;
            }
            entry.status = Some(AgentStatus::Error);
            entry.attention_cooldown_until_ms = None;
            entry.last_error = Some(reason);
            entry.last_activity_at_ms = Some(now);
            true
        }
        SessionEvent::ResumeDispatched { key } => {
            let sid = acp::schema::v1::SessionId::new(key);
            let Some(identity) = unique_identity_for_raw(state, &sid) else {
                return false;
            };
            mark_resume_dispatched_identity_locked(state, &identity)
                .is_some_and(|(flipped, _)| flipped)
        }
        SessionEvent::ResumeFailed { key, reason } => {
            let sid = acp::schema::v1::SessionId::new(key);
            let Some(identity) = unique_identity_for_raw(state, &sid) else {
                return false;
            };
            let Some(entry) = state.sessions.get_mut(&identity) else {
                return false;
            };
            if entry.status == Some(AgentStatus::Idle) && entry.pane_session_id.is_none() {
                entry.status = Some(AgentStatus::Historical);
                entry.last_error = Some(reason);
                entry.last_activity_at_ms = Some(now);
                return true;
            }
            false
        }
        SessionEvent::ResumePaneAssigned {
            key,
            pane_session_id,
        } => {
            let sid = acp::schema::v1::SessionId::new(key);
            let Some(identity) = unique_identity_for_raw(state, &sid) else {
                return false;
            };
            assign_resume_pane_identity_locked(state, &identity, pane_session_id, now)
        }
    }
}

/// Bulk-load the result of an ACP `session/list` response into a registry
/// and mark the helper as having seen its first authoritative snapshot.
///
/// Semantics: the snapshot is *authoritative* — any row not present in
/// `items` is removed. We achieve this by issuing per-key removes against
/// the current snapshot (so we honor the registry's existing locking
/// surface without adding a `clear()` method just for one bootstrap call
/// site) and then upserting each item from `items`.
///
/// Setting `loaded` to `true` flips the helper from "we haven't heard
/// from master yet, fall back to legacy behavior" to "registry is
/// authoritative". The session management routing layer reads this flag to avoid
/// misclassifying an actually-Live row as Ended during the startup
/// window between helper boot and the first `session/list` response.
///
/// This is intentionally a free function rather than a method on
/// `SessionRegistry`: bootstrap-vs-incremental is a *caller* concern,
/// not a property of the storage, and keeping the trait minimal keeps
/// the mock surface small for unit tests of higher layers.
pub async fn apply_snapshot(
    reg: &dyn SessionRegistry,
    loaded: &AtomicBool,
    items: impl IntoIterator<Item = SessionInfo>,
) {
    // Drop every row currently in the registry. We snapshot first and
    // then remove by id rather than holding a write lock across the
    // whole reload, because (a) the trait surface only offers per-key
    // mutations, (b) bootstrap snapshots are tiny (<<100 rows) so the
    // double-pass is cheap, and (c) the only concurrent writer at
    // bootstrap is the ext-notification listener, which we *want* to
    // win against this routine — see comment on `alive_loaded` for
    // why we tolerate the small race window.
    for old in reg.snapshot().await {
        reg.remove_identity(&SessionIdentity::from_info(&old)).await;
    }
    for item in items {
        reg.upsert(item).await;
    }
    loaded.store(true, Ordering::Release);
}

/// Apply a single `intellterm.wta/session_*` ext-notification to the
/// helper's local registry mirror.
///
/// Splitting this out of `WtaClient::ext_notification` lets the helper
/// trait impl stay a one-liner and keeps the interesting logic — what
/// counts as ours, what we do with the payload — purely synchronous
/// (well, async-fn-shaped) and unit-testable without an ACP transport.
///
/// Returns the parsed classification so callers can log/trace by kind;
/// the registry side-effect has already happened by the time the value
/// is returned.
pub async fn apply_ext_notification(
    reg: &dyn SessionRegistry,
    n: &acp::schema::v1::ExtNotification,
) -> WtaExtNotification {
    let parsed = parse_ext_notification(n);
    match &parsed {
        WtaExtNotification::SessionAdded(info) => {
            reg.upsert(info.clone()).await;
        }
        WtaExtNotification::SessionRemoved(params) => {
            if params.history_key.is_some() {
                reg.remove_identity(&params.identity()).await;
            } else {
                reg.remove(&params.session_id).await;
            }
        }
        WtaExtNotification::SessionsChanged => {}
        // Unknown / MalformedParams: caller's job to log; never panic
        // and never mutate the registry. A future master may broadcast
        // notifications we don't recognise — silently ignoring them
        // keeps the helper forward-compatible.
        WtaExtNotification::Unknown | WtaExtNotification::MalformedParams { .. } => {}
    }
    parsed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cooldown_state() -> RegistryState {
        let mut state = RegistryState::default();
        apply_event_at_ms(
            &mut state,
            SessionEvent::SessionStarted {
                key: "cooldown".into(),
                cli_source: CliSource::Copilot,
                pane_session_id: "owner".into(),
                cwd: PathBuf::from("/repo"),
                title: "copilot".into(),
            },
            0,
        );
        apply_event_at_ms(
            &mut state,
            SessionEvent::ToolStarting {
                key: "cooldown".into(),
                tool_name: "prompt".into(),
            },
            0,
        );
        state
    }

    #[test]
    fn copilot_attention_cooldown_preserves_master_snapshot_and_expiry_boundary() {
        for (attention_timestamp, return_timestamp) in [
            (Some(100), Some(200)),
            (Some(100), None),
            (None, Some(200)),
            (None, None),
        ] {
            let mut state = cooldown_state();
            assert!(apply_event_at_ms(
                &mut state,
                SessionEvent::Notification {
                    key: "cooldown".into(),
                    message: "approve command".into(),
                    timestamp_ms: attention_timestamp,
                },
                10_000,
            ));
            apply_event_at_ms(
                &mut state,
                SessionEvent::WorkingDirectoryChanged {
                    key: "cooldown".into(),
                    pane_session_id: "owner".into(),
                    cwd: PathBuf::from("/other"),
                },
                20_000,
            );
            let before = state.sessions.values().next().unwrap().clone();
            let wire = serde_json::to_value(&before).unwrap();
            let decoded: SessionInfo = serde_json::from_value(wire).unwrap();
            let helper = crate::app::session_info_to_agent_session(&decoded);
            let round_trip = agent_session_to_session_info(&helper);
            assert_eq!(round_trip.attention_cooldown_until_ms, Some(40_000));
            assert_eq!(round_trip.attention_timestamp_ms, attention_timestamp);
            assert!(!apply_event_at_ms(
                &mut state,
                SessionEvent::ToolReturned {
                    key: "cooldown".into(),
                    timestamp_ms: return_timestamp,
                },
                39_999,
            ));
            assert_eq!(state.sessions.values().next().unwrap(), &before);
            apply_event_at_ms(
                &mut state,
                SessionEvent::WorkingDirectoryChanged {
                    key: "cooldown".into(),
                    pane_session_id: "owner".into(),
                    cwd: PathBuf::from("/at-expiry"),
                },
                40_000,
            );
            assert_eq!(
                state.sessions.values().next().unwrap().status,
                Some(AgentStatus::Attention)
            );
            assert!(apply_event_at_ms(
                &mut state,
                SessionEvent::ToolReturned {
                    key: "cooldown".into(),
                    timestamp_ms: return_timestamp,
                },
                40_000,
            ));
            let recovered = state.sessions.values().next().unwrap();
            assert_eq!(recovered.status, Some(AgentStatus::Working));
            assert!(recovered.attention_reason.is_none());
            assert!(recovered.attention_cooldown_until_ms.is_none());
        }
    }

    #[test]
    fn copilot_attention_cooldown_resets_without_delaying_master_stop_or_prompt() {
        let mut state = cooldown_state();
        for (received, source) in [(10_000, Some(100)), (40_000, Some(90))] {
            apply_event_at_ms(
                &mut state,
                SessionEvent::Notification {
                    key: "cooldown".into(),
                    message: "waiting".into(),
                    timestamp_ms: source,
                },
                received,
            );
        }
        assert_eq!(
            state
                .sessions
                .values()
                .next()
                .unwrap()
                .attention_cooldown_until_ms,
            Some(70_000)
        );
        assert!(!apply_event_at_ms(
            &mut state,
            SessionEvent::ToolReturned {
                key: "cooldown".into(),
                timestamp_ms: Some(200),
            },
            40_000,
        ));
        assert!(apply_event_at_ms(
            &mut state,
            SessionEvent::ToolCompleted {
                key: "cooldown".into()
            },
            40_001
        ));
        let stopped = state.sessions.values().next().unwrap();
        assert_eq!(stopped.status, Some(AgentStatus::Idle));
        assert!(stopped.attention_cooldown_until_ms.is_none());
        assert!(!apply_event_at_ms(
            &mut state,
            SessionEvent::ToolReturned {
                key: "cooldown".into(),
                timestamp_ms: Some(200),
            },
            40_002,
        ));
        apply_event_at_ms(
            &mut state,
            SessionEvent::Notification {
                key: "cooldown".into(),
                message: "another wait".into(),
                timestamp_ms: None,
            },
            80_000,
        );
        apply_event_at_ms(
            &mut state,
            SessionEvent::ToolStarting {
                key: "cooldown".into(),
                tool_name: "prompt".into(),
            },
            80_001,
        );
        let active = state.sessions.values().next().unwrap();
        assert_eq!(active.status, Some(AgentStatus::Working));
        assert!(active.attention_cooldown_until_ms.is_none());
        apply_event_at_ms(
            &mut state,
            SessionEvent::Notification {
                key: "cooldown".into(),
                message: "waiting before error".into(),
                timestamp_ms: None,
            },
            90_000,
        );
        apply_event_at_ms(
            &mut state,
            SessionEvent::ConnectionFailed {
                pane_session_id: "owner".into(),
                reason: "connection failed".into(),
            },
            90_001,
        );
        assert!(!apply_event_at_ms(
            &mut state,
            SessionEvent::ToolReturned {
                key: "cooldown".into(),
                timestamp_ms: None,
            },
            90_002,
        ));
        assert_eq!(
            state.sessions.values().next().unwrap().status,
            Some(AgentStatus::Error)
        );
    }

    const NATIVE_PANE: &str = "00112233-4455-6677-8899-aabbccddeeff";

    #[tokio::test]
    async fn pane_status_snapshot_keeps_unknown_conversations_distinct_and_inference_advisory() {
        let registry = InMemoryRegistry::new();
        for pane in [NATIVE_PANE, "11112233-4455-6677-8899-aabbccddeeff"] {
            let mut event = native_event(
                serde_json::json!({"kind":"reset", "tracking_enabled":false}),
                1,
            );
            event.pane_id = pane.into();
            assert!(registry.apply_native_observation(event).await);
        }
        // List correlation is deliberately not a confirmed binding.
        {
            let mut state = registry.inner.lock().await;
            let observation = &mut state.native_panes.get_mut(NATIVE_PANE).unwrap().observation;
            observation.inferred_agent_session_id = Some("advisory-only".into());
            observation.inferred_history_key =
                HistoryRowKey::new("copilot", SessionLocation::Host, "advisory-only", None);
        }
        let (panes, native) = registry.pane_status_snapshot().await;
        assert_eq!(panes.len(), 2);
        assert_ne!(panes[0].pane_id, panes[1].pane_id);
        for pane in &panes {
            assert!(pane.agent_session_id.is_none());
            assert!(pane.cwd.is_none());
            assert_eq!(pane.mode_epoch, Some(1));
            assert_eq!(pane.window_id, Some(7));
            assert_eq!(pane.tab_id.as_deref(), Some("tab-7"));
        }
        let wire = build_sessions_list_response(
            Vec::new(),
            None,
            None,
            native,
            Some(4),
            Some(true),
            panes.clone(),
        );
        let response = parse_sessions_list_response(&wire).unwrap();
        assert_eq!(response.pane_statuses, panes);
        assert_eq!(response.native_pane_observations.len(), 2);
        let value: serde_json::Value = serde_json::from_str(wire.get()).unwrap();
        let advisory = value["pane_statuses"]
            .as_array()
            .unwrap()
            .iter()
            .find(|pane| pane["pane_id"] == NATIVE_PANE)
            .unwrap();
        assert_eq!(
            advisory["inferred_history_key"]["session_id"],
            "advisory-only"
        );
        assert!(advisory.get("agent_session_id").is_none());
        assert_eq!(response.native_observation_receiver_generation, Some(4));
        assert_eq!(response.native_observation_listener_ready, Some(true));
    }

    #[tokio::test]
    async fn pane_status_snapshot_projects_qualified_owners_and_not_history_visibility() {
        let registry = InMemoryRegistry::new();
        let mut rows = [
            qualified_info("same-id", "copilot", SessionLocation::Host, "host"),
            qualified_info("same-id", "claude", SessionLocation::Host, "claude"),
            qualified_info(
                "same-id",
                "copilot",
                SessionLocation::Wsl {
                    distro: "Ubuntu".into(),
                },
                "wsl",
            ),
        ];
        for (index, row) in rows.iter_mut().enumerate() {
            row.status = Some(AgentStatus::Working);
            row.pane_session_id = Some(format!("pane-{index}"));
            row.last_activity_at_ms = Some(42 + index as u64);
            row.origin = Some(SessionOrigin::AgentPane);
            registry.upsert(row.clone()).await;
        }
        let mut legacy_helper = info("legacy-helper", Some("helper-pane"));
        legacy_helper.status = Some(AgentStatus::Idle);
        legacy_helper.cli_source = Some(CliSource::Codex);
        registry.upsert(legacy_helper).await;
        let mut history = rows[0].clone();
        history.session_id = acp::schema::v1::SessionId::new("history-only");
        history.status = Some(AgentStatus::Historical);
        history.pane_session_id = None;
        registry.upsert(history).await;
        let mut unlocated = rows[0].clone();
        unlocated.session_id = acp::schema::v1::SessionId::new("no-pane");
        unlocated.pane_session_id = None;
        registry.upsert(unlocated).await;
        let mut unknown_provider = info("unknown-provider", Some("unknown-provider-pane"));
        unknown_provider.status = Some(AgentStatus::Idle);
        unknown_provider.cli_source = Some(CliSource::Unknown("unrecognized-cli".into()));
        registry.upsert(unknown_provider).await;
        assert!(registry.lookup(&rows[0].session_id).await.is_none());
        let (panes, native) = registry.pane_status_snapshot().await;
        assert!(native.is_empty());
        assert_eq!(panes.len(), 4);
        for row in rows {
            let pane = panes
                .iter()
                .find(|pane| Some(&pane.pane_id) == row.pane_session_id.as_ref())
                .unwrap();
            assert_eq!(pane.provider_id, row.provider_id.unwrap());
            assert_eq!(pane.agent_session_id, Some(row.session_id.to_string()));
            assert_eq!(pane.cwd, Some(row.cwd));
            assert_eq!(pane.location, Some(row.location));
            assert_eq!(pane.last_activity_at_ms, row.last_activity_at_ms);
            let json = serde_json::to_value(pane).unwrap();
            for field in ["mode_epoch", "window_id", "tab_id"] {
                assert!(json.get(field).is_none());
            }
        }
        assert_eq!(panes[0].provider_id, "codex");
    }

    #[tokio::test]
    async fn pane_status_snapshot_native_owner_is_one_effective_canonical_bound_row() {
        let (registry, identity) = native_bound_registry().await;
        let mut row = registry.lookup_identity(&identity).await.unwrap();
        row.status = Some(AgentStatus::Attention);
        row.last_activity_at_ms = Some(123);
        assert!(
            registry
                .apply_native_observation(native_event(
                    serde_json::json!({"kind":"reset", "tracking_enabled":false}),
                    1,
                ))
                .await
        );
        registry.upsert(row.clone()).await;
        let (panes, native) = registry.pane_status_snapshot().await;
        assert_eq!(panes.len(), 1);
        assert_eq!(native.len(), 1);
        assert_eq!(panes[0].pane_id, NATIVE_PANE);
        assert_eq!(panes[0].agent_session_id, Some(row.session_id.to_string()));
        assert_eq!(panes[0].status, row.status.unwrap());
        assert_eq!(panes[0].cwd, Some(row.cwd));
        assert_eq!(panes[0].last_activity_at_ms, Some(123));
        assert_eq!(panes[0].mode_epoch, Some(1));
        assert_eq!(panes[0].cwd, native[0].cwd);
        assert_eq!(panes[0].status, native[0].status);
        assert!(
            registry
                .apply_native_observation(native_event(
                    serde_json::json!({"kind":"reset", "tracking_enabled":true}),
                    2,
                ))
                .await
        );
        let (panes, native) = registry.pane_status_snapshot().await;
        assert!(native.is_empty());
        assert_eq!(panes.len(), 1);
        assert!(panes[0].mode_epoch.is_none());
    }

    #[tokio::test]
    async fn pane_status_snapshot_rejects_normalized_canonical_collisions() {
        let registry = InMemoryRegistry::new();
        for (id, pane) in [
            ("first", format!("{{{}}}", NATIVE_PANE.to_uppercase())),
            ("second", NATIVE_PANE.into()),
        ] {
            let mut row = qualified_info(id, "copilot", SessionLocation::Host, id);
            row.status = Some(AgentStatus::Idle);
            row.pane_session_id = Some(pane);
            registry.upsert(row).await;
        }
        assert!(registry.pane_status_snapshot().await.0.is_empty());
        // OFF ownership still supplies pane-only status when canonical ownership is ambiguous.
        assert!(
            registry
                .apply_native_observation(native_event(
                    serde_json::json!({"kind":"reset", "tracking_enabled":false}),
                    1,
                ))
                .await
        );
        let (panes, _) = registry.pane_status_snapshot().await;
        assert_eq!(panes.len(), 1);
        assert!(panes[0].agent_session_id.is_none());
        assert!(panes[0].cwd.is_none());
    }

    #[tokio::test]
    async fn pane_status_snapshot_retains_explicitly_bound_status_transitions() {
        let registry = InMemoryRegistry::new();
        let mut row = qualified_info("helper", "copilot", SessionLocation::Host, "helper");
        row.pane_session_id = Some(NATIVE_PANE.into());
        row.origin = Some(SessionOrigin::AgentPane);
        for status in [
            AgentStatus::Idle,
            AgentStatus::Working,
            AgentStatus::Attention,
            AgentStatus::Error,
            AgentStatus::Ended,
            AgentStatus::Historical,
            AgentStatus::InUse,
        ] {
            row.status = Some(status.clone());
            registry.upsert(row.clone()).await;
            let (panes, _) = registry.pane_status_snapshot().await;
            assert_eq!(panes.len(), 1);
            assert_eq!(panes[0].status, status);
            assert_eq!(panes[0].agent_session_id.as_deref(), Some("helper"));
            assert!(panes[0].mode_epoch.is_none());
        }
        registry
            .apply_event(SessionEvent::PaneClosed {
                pane_session_id: NATIVE_PANE.into(),
            })
            .await;
        assert!(registry.pane_status_snapshot().await.0.is_empty());
    }

    #[tokio::test]
    async fn pane_status_snapshot_withholds_antigravity_shell_cwd_without_changing_diagnostics() {
        for agent_pane in [false, true] {
            let registry = InMemoryRegistry::new();
            let mut row = qualified_info("anti", "antigravity", SessionLocation::Host, "anti");
            row.status = Some(AgentStatus::Idle);
            row.pane_session_id = Some(NATIVE_PANE.into());
            row.origin = Some(if agent_pane {
                SessionOrigin::AgentPane
            } else {
                SessionOrigin::Unknown
            });
            row.born_bound_pane = !agent_pane;
            registry.upsert(row.clone()).await;
            let (panes, _) = registry.pane_status_snapshot().await;
            assert_eq!(panes[0].cwd, agent_pane.then(|| row.cwd.clone()));
            if !agent_pane {
                let mut event = native_event(
                    serde_json::json!({"kind":"reset", "tracking_enabled":false}),
                    1,
                );
                event.provider_id = "antigravity".into();
                assert!(registry.apply_native_observation(event).await);
                let (panes, native) = registry.pane_status_snapshot().await;
                assert_eq!(panes[0].agent_session_id.as_deref(), Some("anti"));
                assert!(panes[0].cwd.is_none());
                assert_eq!(native[0].cwd, Some(row.cwd));
            }
        }
    }

    #[tokio::test]
    async fn native_birth_wire_snapshot_precedes_osc() {
        let registry = InMemoryRegistry::new();
        let event = native_event(
            serde_json::json!({
                "kind":"reset", "tracking_enabled":false,
                "pane_created_at_ms":1000, "pane_creation_sequence":42
            }),
            1,
        );
        assert!(registry.apply_native_observation(event).await);
        let value = serde_json::to_value(registry.native_pane_observations().await).unwrap();
        assert_eq!(value[0]["pane_created_at_ms"], 1000);
        assert_eq!(value[0]["pane_creation_sequence"], 42);
        assert!(value[0].get("agent_session_id").is_none());
    }

    fn native_event(kind: serde_json::Value, epoch: u64) -> NativeAgentObservation {
        let mut value = serde_json::json!({
            "pane_id": NATIVE_PANE, "provider_id": "copilot",
            "window_id": 7, "tab_id": "tab-7", "mode_epoch": epoch,
        });
        value
            .as_object_mut()
            .unwrap()
            .extend(kind.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }

    async fn pending_native(registry: &InMemoryRegistry, pane: &str, birth: u64, sequence: u64) {
        let mut event = native_event(
            serde_json::json!({
                "kind":"reset", "tracking_enabled":false, "location":"Host",
                "pane_created_at_ms":birth, "pane_creation_sequence":sequence
            }),
            1,
        );
        event.pane_id = pane.into();
        assert!(registry.apply_native_observation(event).await);
    }

    fn listed_native(id: &str, activity: u64) -> SessionInfo {
        let mut row = SessionInfo::new(id.to_string().into(), PathBuf::from(r"C:\repo"));
        row.provider_id = Some("copilot".into());
        row.location = SessionLocation::Host;
        row.last_activity_at_ms = Some(activity);
        row.status = Some(AgentStatus::Historical);
        row
    }

    async fn enrich_native(registry: &InMemoryRegistry, rows: &[SessionInfo]) -> bool {
        registry
            .enrich_native_from_listing(
                "copilot",
                &SessionLocation::Host,
                rows,
                &HashSet::new(),
                2000,
                3000,
                true,
            )
            .await
    }

    async fn ingest_native_listing_page(
        registry: &InMemoryRegistry,
        rows: &[SessionInfo],
        complete: bool,
        start: u64,
        end: u64,
    ) {
        registry
            .enrich_native_from_listing(
                "copilot",
                &SessionLocation::Host,
                rows,
                &HashSet::new(),
                start,
                end,
                complete,
            )
            .await;
        for row in rows {
            registry.upsert_if_absent(row.clone()).await;
        }
    }

    #[tokio::test]
    async fn native_list_evidence_survives_delayed_pane_admission_and_slow_launch() {
        let registry = InMemoryRegistry::new();
        let birth = 1_000_000;
        let discovered = birth + 180_000;
        let row = listed_native("slow-native", discovered - 1_000);
        assert!(
            !registry
                .enrich_native_from_listing(
                    "copilot",
                    &SessionLocation::Host,
                    &[row.clone()],
                    &HashSet::new(),
                    birth - 1_000,
                    discovered,
                    false,
                )
                .await
        );
        registry.upsert_if_absent(row.clone()).await;
        pending_native(&registry, NATIVE_PANE, birth, 42).await;
        assert!(
            registry
                .enrich_native_from_listing(
                    "copilot",
                    &SessionLocation::Host,
                    &[row],
                    &HashSet::from(["slow-native".to_string()]),
                    discovered + 1_000,
                    discovered + 180_000,
                    true,
                )
                .await
        );
        let (panes, _) = registry.pane_status_snapshot().await;
        assert_eq!(panes.len(), 1);
        assert!(panes[0].agent_session_id.is_none());
        assert_eq!(
            panes[0].inferred_history_key.as_ref().unwrap().session_id,
            "slow-native"
        );
        assert!(registry.inner.lock().await.active_by_pane.is_empty());
        let history = registry.snapshot().await;
        assert_eq!(history.len(), 1);
        assert!(history[0].pane_session_id.is_none());
        assert_eq!(history[0].status, Some(AgentStatus::Historical));
        assert!(
            registry
                .apply_native_observation(native_event(serde_json::json!({"kind":"removed"}), 1))
                .await
        );
        assert!(registry.pane_status_snapshot().await.0.is_empty());
        assert_eq!(registry.snapshot().await.len(), 1);
    }

    #[tokio::test]
    async fn native_pending_inventory_request_is_limited_to_eligible_pane_lifetime() {
        let registry = InMemoryRegistry::new();
        pending_native(&registry, NATIVE_PANE, now_ms() - 180_000, 42).await;
        assert!(
            registry
                .has_pending_native_inference("copilot", &SessionLocation::Host)
                .await
        );
        assert!(
            !registry
                .has_pending_native_inference("claude", &SessionLocation::Host)
                .await
        );
        assert!(
            !registry
                .has_pending_native_inference(
                    "copilot",
                    &SessionLocation::Wsl {
                        distro: "Ubuntu".into()
                    },
                )
                .await
        );
        assert!(
            registry
                .apply_native_observation(native_event(
                    serde_json::json!({"kind":"reset", "tracking_enabled":true}),
                    2,
                ))
                .await
        );
        assert!(
            !registry
                .has_pending_native_inference("copilot", &SessionLocation::Host)
                .await
        );
    }

    #[tokio::test]
    async fn native_list_cohort_combines_staggered_discovery_and_freezes_first_activity() {
        let registry = InMemoryRegistry::new();
        pending_native(&registry, NATIVE_PANE, 1000, 42).await;
        let second = "00112233-4455-6677-8899-aabbccddee00";
        pending_native(&registry, second, 1000, 43).await;
        ingest_native_listing_page(&registry, &[listed_native("A", 1500)], true, 2000, 3000).await;
        assert!(registry
            .native_pane_observations()
            .await
            .iter()
            .all(|row| row.inferred_agent_session_id.is_none()));
        ingest_native_listing_page(
            &registry,
            &[listed_native("B", 1700), listed_native("A", 9000)],
            true,
            4000,
            5000,
        )
        .await;
        let rows = registry.native_pane_observations().await;
        assert_eq!(
            rows.iter()
                .find(|row| row.pane_id == NATIVE_PANE)
                .unwrap()
                .inferred_agent_session_id
                .as_deref(),
            Some("A")
        );
        assert_eq!(
            rows.iter()
                .find(|row| row.pane_id == second)
                .unwrap()
                .inferred_agent_session_id
                .as_deref(),
            Some("B")
        );
    }

    #[tokio::test]
    async fn native_list_cohort_complete_ambiguity_is_permanent_for_admission() {
        let registry = InMemoryRegistry::new();
        pending_native(&registry, NATIVE_PANE, 1000, 42).await;
        ingest_native_listing_page(
            &registry,
            &[listed_native("A", 1500), listed_native("B", 1700)],
            true,
            2000,
            3000,
        )
        .await;
        ingest_native_listing_page(
            &registry,
            &[
                listed_native("A", 1500),
                listed_native("B", 1700),
                listed_native("C", 2500),
            ],
            true,
            4000,
            5000,
        )
        .await;
        assert!(registry.native_pane_observations().await[0]
            .inferred_agent_session_id
            .is_none());
    }

    #[tokio::test]
    async fn native_list_cohort_partial_evidence_retains_competitors_until_complete() {
        let registry = InMemoryRegistry::new();
        pending_native(&registry, NATIVE_PANE, 1000, 42).await;
        ingest_native_listing_page(&registry, &[listed_native("A", 1500)], false, 2000, 3000).await;
        assert!(registry.native_pane_observations().await[0]
            .inferred_agent_session_id
            .is_none());
        ingest_native_listing_page(
            &registry,
            &[listed_native("A", 1500), listed_native("B", 1700)],
            true,
            4000,
            5000,
        )
        .await;
        assert!(registry.native_pane_observations().await[0]
            .inferred_agent_session_id
            .is_none());
    }

    #[tokio::test]
    async fn native_list_inference_is_advisory_and_preserves_immutable_birth_and_progress() {
        let registry = InMemoryRegistry::new();
        pending_native(&registry, NATIVE_PANE, 1000, 42).await;
        assert!(
            registry
                .apply_native_observation(native_event(
                    serde_json::json!({"kind":"progress","progress_state":1}),
                    1
                ))
                .await
        );
        assert!(enrich_native(&registry, &[listed_native("new", 1500)]).await);
        let row = &registry.native_pane_observations().await[0];
        assert_eq!(row.inferred_agent_session_id.as_deref(), Some("new"));
        assert!(row.agent_session_id.is_none());
        assert_eq!(row.status, AgentStatus::Working);
        assert!(registry.snapshot().await.is_empty());
        assert!(registry.inner.lock().await.active_by_pane.is_empty());
        assert!(
            registry
                .apply_native_observation(native_event(
                    serde_json::json!({
                        "kind":"reset","tracking_enabled":false,"location":"Host",
                        "pane_created_at_ms":9999,"pane_creation_sequence":999
                    }),
                    2
                ))
                .await
        );
        let row = &registry.native_pane_observations().await[0];
        assert_eq!(
            (row.pane_created_at_ms, row.pane_creation_sequence),
            (Some(1000), Some(42))
        );
        assert_eq!(row.status, AgentStatus::Working);
        assert_eq!(row.inferred_agent_session_id.as_deref(), Some("new"));
        assert!(
            !registry
                .enrich_native_from_listing(
                    "copilot",
                    &SessionLocation::Host,
                    &[listed_native("replacement", 3500)],
                    &HashSet::new(),
                    4000,
                    5000,
                    true,
                )
                .await
        );
        assert_eq!(
            registry.native_pane_observations().await[0]
                .inferred_agent_session_id
                .as_deref(),
            Some("new")
        );
    }

    #[tokio::test]
    async fn native_list_inference_orders_panes_and_results_not_input_order() {
        let registry = InMemoryRegistry::new();
        let second = "00112233-4455-6677-8899-aabbccddee00";
        pending_native(&registry, second, 1000, 43).await;
        pending_native(&registry, NATIVE_PANE, 1000, 42).await;
        assert!(
            enrich_native(
                &registry,
                &[listed_native("second", 1700), listed_native("first", 1500)]
            )
            .await
        );
        let rows = registry.native_pane_observations().await;
        assert_eq!(
            rows.iter()
                .find(|row| row.pane_id == NATIVE_PANE)
                .unwrap()
                .inferred_agent_session_id
                .as_deref(),
            Some("first")
        );
        assert_eq!(
            rows.iter()
                .find(|row| row.pane_id == second)
                .unwrap()
                .inferred_agent_session_id
                .as_deref(),
            Some("second")
        );
    }

    #[tokio::test]
    async fn native_birth_late_source_metadata_preserves_osc_and_does_not_admit_shells() {
        let registry = InMemoryRegistry::new();
        let mut birth = native_event(
            serde_json::json!({
                "kind":"reset","tracking_enabled":false,
                "pane_created_at_ms":1000,"pane_creation_sequence":42
            }),
            1,
        );
        assert!(registry.apply_native_observation(birth.clone()).await);
        assert!(
            registry
                .apply_native_observation(native_event(
                    serde_json::json!({"kind":"progress","progress_state":1}),
                    1
                ))
                .await
        );
        birth.mode_epoch = 2;
        birth.location = Some(SessionLocation::Host);
        assert!(registry.apply_native_observation(birth).await);
        assert_eq!(
            registry.native_pane_observations().await[0].status,
            AgentStatus::Working
        );
        assert!(
            registry
                .apply_native_observation(native_event(
                    serde_json::json!({"kind":"bell","pane_created_at_ms":9999}),
                    2
                ))
                .await
        );
        let row = &registry.native_pane_observations().await[0];
        assert_eq!(row.location, Some(SessionLocation::Host));
        assert_eq!(row.pane_created_at_ms, Some(1000));
        assert_eq!(row.status, AgentStatus::Attention);
        assert!(enrich_native(&registry, &[listed_native("new", 1500)]).await);
        let manual = InMemoryRegistry::new();
        assert!(
            !manual
                .apply_native_observation(native_event(
                    serde_json::json!({"kind":"progress","progress_state":1,"location":"Host"}),
                    1
                ))
                .await
        );
        assert!(manual.native_pane_observations().await.is_empty());
    }

    #[tokio::test]
    async fn native_list_inference_rejects_ambiguity_backend_ownership_and_missing_metadata() {
        for case in 0..13 {
            let registry = InMemoryRegistry::new();
            pending_native(&registry, NATIVE_PANE, 1000, 42).await;
            let mut rows = vec![listed_native("new", 1500)];
            match case {
                0 => rows[0].provider_id = Some("claude".into()),
                1 => {
                    rows[0].location = SessionLocation::Wsl {
                        distro: "Ubuntu".into(),
                    }
                }
                2 => rows[0].session_universe = Some("other".into()),
                3 => rows[0].pane_session_id = Some("another-pane".into()),
                4 => rows[0].origin = Some(SessionOrigin::AgentPane),
                5 => rows.push(listed_native("external", 1600)),
                6 => rows[0].last_activity_at_ms = None,
                7 => rows[0].last_activity_at_ms = Some(0),
                8 => rows[0].last_activity_at_ms = Some(999),
                9 => {
                    let mut old = listed_native("new", 1);
                    old.provider_id = Some("claude".into());
                    registry.upsert(old).await;
                }
                10 => {
                    registry.upsert(listed_native("new", 1)).await;
                    registry.remove(&"new".into()).await;
                }
                11 => {
                    registry
                        .inner
                        .lock()
                        .await
                        .native_panes
                        .get_mut(NATIVE_PANE)
                        .unwrap()
                        .observation
                        .location = None
                }
                12 => {
                    registry
                        .inner
                        .lock()
                        .await
                        .native_panes
                        .get_mut(NATIVE_PANE)
                        .unwrap()
                        .observation
                        .pane_created_at_ms = None
                }
                _ => unreachable!(),
            }
            assert!(!enrich_native(&registry, &rows).await, "case {case}");
            assert!(registry.native_pane_observations().await[0]
                .inferred_agent_session_id
                .is_none());
        }
        let registry = InMemoryRegistry::new();
        pending_native(&registry, NATIVE_PANE, 1000, 42).await;
        pending_native(&registry, "00112233-4455-6677-8899-aabbccddee00", 1000, 43).await;
        assert!(
            !enrich_native(
                &registry,
                &[listed_native("a", 1500), listed_native("b", 1500)]
            )
            .await
        );
    }

    #[tokio::test]
    async fn native_list_inference_rejects_known_delayed_and_out_of_order_discovery() {
        for (started, completed, known) in [
            (999, 3000, false),
            (301001, 302001, false),
            (2000, 3000, true),
        ] {
            let registry = InMemoryRegistry::new();
            pending_native(&registry, NATIVE_PANE, 1000, 42).await;
            let known = if known {
                HashSet::from(["new".into()])
            } else {
                HashSet::new()
            };
            assert!(
                !registry
                    .enrich_native_from_listing(
                        "copilot",
                        &SessionLocation::Host,
                        &[listed_native("new", 1500)],
                        &known,
                        started,
                        completed,
                        true,
                    )
                    .await
            );
        }
        let registry = InMemoryRegistry::new();
        pending_native(&registry, NATIVE_PANE, 1000, 42).await;
        assert!(!enrich_native(&registry, &[]).await);
        assert!(
            !registry
                .enrich_native_from_listing(
                    "copilot",
                    &SessionLocation::Host,
                    &[listed_native("new", 1500)],
                    &HashSet::new(),
                    1999,
                    2999,
                    true,
                )
                .await
        );
    }

    #[tokio::test]
    async fn native_list_inference_clears_on_exact_resume_mode_change_and_removal() {
        for kind in ["resume", "mode", "removed"] {
            let registry = InMemoryRegistry::new();
            pending_native(&registry, NATIVE_PANE, 1000, 42).await;
            assert!(enrich_native(&registry, &[listed_native("guess", 1500)]).await);
            if kind == "resume" {
                let row = listed_native("exact", 1600);
                let identity = SessionIdentity::from_info(&row);
                registry.upsert(row).await;
                assert!(
                    registry
                        .register_native_born_binding(
                            &identity,
                            NATIVE_PANE,
                            &PathBuf::from(r"C:\repo"),
                            Some(2),
                            None
                        )
                        .await
                );
                let native = &registry.native_pane_observations().await[0];
                assert_eq!(native.agent_session_id.as_deref(), Some("exact"));
                assert!(native.inferred_agent_session_id.is_none());
                assert_eq!(
                    registry.inner.lock().await.active_by_pane.get(NATIVE_PANE),
                    Some(&identity)
                );
            } else {
                let event = if kind == "mode" {
                    native_event(
                        serde_json::json!({"kind":"reset","tracking_enabled":true}),
                        2,
                    )
                } else {
                    native_event(serde_json::json!({"kind":"removed"}), 1)
                };
                assert!(registry.apply_native_observation(event).await);
                assert!(registry.native_pane_observations().await.is_empty());
            }
        }
    }

    async fn native_bound_registry() -> (InMemoryRegistry, SessionIdentity) {
        let registry = InMemoryRegistry::new();
        let mut row = SessionInfo::new(
            acp::schema::v1::SessionId::new("real-session"),
            PathBuf::from(r"C:\repo"),
        )
        .with_pane_session_id(format!("{{{}}}", NATIVE_PANE.to_uppercase()));
        row.provider_id = Some("copilot".into());
        row.location = SessionLocation::Host;
        row.status = Some(AgentStatus::Idle);
        row.born_bound_pane = true;
        let identity = SessionIdentity::from_info(&row);
        registry.upsert(row).await;
        (registry, identity)
    }

    #[tokio::test]
    async fn native_cwd_projection_requires_exact_active_qualified_binding() {
        for (location, cwd, other_location) in [
            (
                SessionLocation::Host,
                PathBuf::from(r"C:\canonical"),
                SessionLocation::Wsl {
                    distro: "Ubuntu".into(),
                },
            ),
            (
                SessionLocation::Wsl {
                    distro: "Ubuntu".into(),
                },
                PathBuf::from("/home/canonical"),
                SessionLocation::Host,
            ),
        ] {
            let registry = InMemoryRegistry::new();
            let mut row =
                SessionInfo::new("same-id".into(), cwd.clone()).with_pane_session_id(NATIVE_PANE);
            row.provider_id = Some("copilot".into());
            row.location = location.clone();
            row.status = Some(AgentStatus::Idle);
            row.born_bound_pane = true;
            registry.upsert(row).await;
            let mut other = SessionInfo::new("same-id".into(), PathBuf::from(r"C:\wrong-backend"));
            other.provider_id = Some("copilot".into());
            other.location = other_location;
            other.status = Some(AgentStatus::Historical);
            registry.upsert(other).await;
            let mut event = native_event(
                serde_json::json!({
                    "kind":"reset","tracking_enabled":false
                }),
                1,
            );
            event.agent_session_id = Some("same-id".into());
            event.location = Some(location);
            assert!(registry.apply_native_observation(event).await);
            let native = &registry.native_pane_observations().await[0];
            assert_eq!(native.cwd.as_ref(), Some(&cwd));
            assert_eq!(native.agent_session_id.as_deref(), Some("same-id"));
            assert!(registry.inner.lock().await.native_panes[NATIVE_PANE]
                .observation
                .cwd
                .is_none());
        }
        let registry = InMemoryRegistry::new();
        pending_native(&registry, NATIVE_PANE, 1000, 42).await;
        assert!(registry.native_pane_observations().await[0].cwd.is_none());
        assert!(enrich_native(&registry, &[listed_native("inferred", 1500)]).await);
        let native = &registry.native_pane_observations().await[0];
        assert_eq!(
            native.inferred_agent_session_id.as_deref(),
            Some("inferred")
        );
        assert!(native.agent_session_id.is_none());
        assert!(native.cwd.is_none());
    }

    #[tokio::test]
    async fn native_observation_updates_shared_bound_rows_and_progress_mapping() {
        let (registry, identity) = native_bound_registry().await;
        assert!(
            registry
                .apply_native_observation(native_event(
                    serde_json::json!({"kind":"reset", "tracking_enabled":false}),
                    1
                ))
                .await
        );
        for (progress, status) in [
            (1, AgentStatus::Working),
            (3, AgentStatus::Working),
            (2, AgentStatus::Error),
            (0, AgentStatus::Idle),
            (4, AgentStatus::Idle),
        ] {
            assert!(
                registry
                    .apply_native_observation(native_event(
                        serde_json::json!({"kind":"progress", "progress_state":progress}),
                        1
                    ))
                    .await
            );
            let row = registry.lookup_identity(&identity).await.unwrap();
            assert_eq!(row.status, Some(status.clone()));
            assert!(row.born_bound_pane);
            assert_eq!(registry.native_pane_observations().await[0].status, status);
        }
        registry
            .apply_native_observation(native_event(serde_json::json!({"kind":"bell"}), 1))
            .await;
        assert_eq!(
            registry.snapshot().await[0].status,
            Some(AgentStatus::Attention)
        );
        assert_eq!(
            registry.native_pane_observations().await[0]
                .agent_session_id
                .as_deref(),
            Some("real-session")
        );
    }

    #[tokio::test]
    async fn native_observation_unknown_panes_never_create_or_claim_history_rows() {
        let registry = InMemoryRegistry::new();
        registry
            .apply_native_observation(native_event(
                serde_json::json!({"kind":"reset", "tracking_enabled":false}),
                1,
            ))
            .await;
        registry
            .apply_native_observation(native_event(serde_json::json!({"kind":"bell"}), 1))
            .await;
        assert!(registry.snapshot().await.is_empty());
        assert!(registry.lookup_active_by_pane(NATIVE_PANE).await.is_none());
        let (other, _) = native_bound_registry().await;
        registry.upsert(other.snapshot().await.remove(0)).await;
        registry
            .apply_native_observation(native_event(
                serde_json::json!({"kind":"progress", "progress_state":1}),
                1,
            ))
            .await;
        assert_eq!(registry.snapshot().await[0].status, Some(AgentStatus::Idle));
        let native = registry.native_pane_observations().await;
        assert!(native[0].agent_session_id.is_none());
        let wire = build_sessions_list_response(
            registry.snapshot().await,
            None,
            None,
            native,
            None,
            None,
            Vec::new(),
        );
        let parsed = parse_sessions_list_response(&wire).unwrap();
        assert_eq!(parsed.sessions.len(), 1);
        assert_eq!(
            parsed.native_pane_observations[0].status,
            AgentStatus::Working
        );
    }

    #[tokio::test]
    async fn native_observation_mode_epochs_fence_replay_and_require_reset() {
        let (registry, _) = native_bound_registry().await;
        assert!(
            !registry
                .apply_native_observation(native_event(serde_json::json!({"kind":"bell"}), 1))
                .await
        );
        registry
            .apply_native_observation(native_event(
                serde_json::json!({"kind":"reset", "tracking_enabled":false}),
                1,
            ))
            .await;
        assert!(
            !registry
                .apply_native_observation(native_event(
                    serde_json::json!({"kind":"progress", "progress_state":9}),
                    1
                ))
                .await
        );
        registry
            .apply_native_observation(native_event(
                serde_json::json!({"kind":"reset", "tracking_enabled":true}),
                2,
            ))
            .await;
        assert!(registry.native_pane_observations().await.is_empty());
        for epoch in [1, 2] {
            assert!(
                !registry
                    .apply_native_observation(native_event(
                        serde_json::json!({"kind":"bell"}),
                        epoch
                    ))
                    .await
            );
        }
        assert!(
            !registry
                .apply_native_observation(native_event(
                    serde_json::json!({"kind":"reset", "tracking_enabled":false}),
                    1
                ))
                .await
        );
        registry
            .apply_native_observation(native_event(
                serde_json::json!({"kind":"reset", "tracking_enabled":false}),
                3,
            ))
            .await;
        assert_eq!(
            registry.native_pane_observations().await[0].status,
            AgentStatus::Idle
        );
    }

    #[tokio::test]
    async fn native_observation_only_actual_removal_ends_off_binding() {
        let (registry, identity) = native_bound_registry().await;
        registry
            .apply_native_observation(native_event(
                serde_json::json!({"kind":"reset", "tracking_enabled":false}),
                1,
            ))
            .await;
        for event in [
            SessionEvent::PaneClosed {
                pane_session_id: NATIVE_PANE.into(),
            },
            SessionEvent::ConnectionFailed {
                pane_session_id: NATIVE_PANE.into(),
                reason: "retained".into(),
            },
            SessionEvent::SessionStopped {
                key: "real-session".into(),
                reason: "poll evidence".into(),
            },
        ] {
            assert!(!registry.apply_event(event).await);
        }
        assert!(
            registry
                .lookup_identity(&identity)
                .await
                .unwrap()
                .born_bound_pane
        );
        assert!(
            registry
                .apply_native_observation(native_event(serde_json::json!({"kind":"removed"}), 1))
                .await
        );
        let row = registry.lookup_identity(&identity).await.unwrap();
        assert_eq!(row.status, Some(AgentStatus::Ended));
        assert!(row.pane_session_id.is_none());
        assert!(registry.native_pane_observations().await.is_empty());
        assert!(
            !registry
                .apply_native_observation(native_event(serde_json::json!({"kind":"bell"}), 1))
                .await
        );
    }

    #[tokio::test]
    async fn native_observation_rejects_shell_provider_mismatch_and_ambiguous_binding() {
        let (registry, identity) = native_bound_registry().await;
        let mut event = native_event(
            serde_json::json!({"kind":"reset", "tracking_enabled":false}),
            1,
        );
        event.provider_id = "powershell".into();
        assert!(!registry.apply_native_observation(event).await);
        let mut event = native_event(
            serde_json::json!({"kind":"reset", "tracking_enabled":false}),
            1,
        );
        event.agent_session_id = Some("other-session".into());
        registry.apply_native_observation(event).await;
        assert!(registry.native_pane_observations().await[0]
            .agent_session_id
            .is_none());
        assert_eq!(
            registry.lookup_identity(&identity).await.unwrap().status,
            Some(AgentStatus::Idle)
        );
        let mut other = registry.lookup_identity(&identity).await.unwrap();
        other.provider_id = Some("claude".into());
        registry.upsert(other).await;
        registry
            .apply_native_observation(native_event(
                serde_json::json!({"kind":"reset", "tracking_enabled":false}),
                2,
            ))
            .await;
        registry
            .apply_native_observation(native_event(serde_json::json!({"kind":"bell"}), 2))
            .await;
        assert!(registry.native_pane_observations().await[0]
            .agent_session_id
            .is_none());
        assert!(registry
            .snapshot()
            .await
            .iter()
            .all(|row| row.status == Some(AgentStatus::Idle)));
    }

    #[tokio::test]
    async fn native_observation_exact_resume_binds_only_qualified_unowned_history() {
        let registry = InMemoryRegistry::new();
        let row = qualified_info("resume-id", "copilot", SessionLocation::Host, "history");
        let identity = SessionIdentity::from_info(&row);
        registry.upsert(row).await;
        let mut event = native_event(
            serde_json::json!({"kind":"reset", "tracking_enabled":false}),
            1,
        );
        event.agent_session_id = Some("resume-id".into());
        // Raw id alone is not enough to bind an unowned row.
        registry.apply_native_observation(event.clone()).await;
        assert!(registry
            .lookup_identity(&identity)
            .await
            .unwrap()
            .pane_session_id
            .is_none());
        event.mode_epoch = 2;
        event.location = Some(SessionLocation::Host);
        registry.apply_native_observation(event.clone()).await;
        assert_eq!(
            registry
                .lookup_active_by_pane(NATIVE_PANE)
                .await
                .unwrap()
                .session_id
                .to_string(),
            "resume-id"
        );
        assert!(
            registry
                .lookup_identity(&identity)
                .await
                .unwrap()
                .born_bound_pane
        );
        event.pane_id = "11112233-4455-6677-8899-aabbccddeeff".into();
        registry.apply_native_observation(event).await;
        assert!(registry
            .lookup_active_by_pane("11112233-4455-6677-8899-aabbccddeeff")
            .await
            .is_none());
        assert_eq!(registry.snapshot().await.len(), 1);
    }

    #[tokio::test]
    async fn native_observation_accepts_window_zero_and_catalog_providers() {
        for provider in ["copilot", "antigravity", "custom:test"] {
            let registry = InMemoryRegistry::new();
            let mut event = native_event(
                serde_json::json!({"kind":"reset", "tracking_enabled":false}),
                1,
            );
            event.window_id = 0;
            event.provider_id = provider.into();
            assert!(registry.apply_native_observation(event).await);
            let observations = registry.native_pane_observations().await;
            assert_eq!(observations[0].window_id, 0);
            assert_eq!(observations[0].provider_id, provider);
            assert!(registry.snapshot().await.is_empty());
        }
        for provider in [
            "custom:",
            "custom:   ",
            concat!("custom:test", "\t", "bad"),
            "powershell",
        ] {
            let registry = InMemoryRegistry::new();
            let mut event = native_event(
                serde_json::json!({"kind":"reset", "tracking_enabled":false}),
                1,
            );
            event.provider_id = provider.into();
            assert!(!registry.apply_native_observation(event).await);
        }
    }

    #[tokio::test]
    async fn native_observation_on_reset_preserves_canonical_activity_and_fields() {
        for status in [AgentStatus::Working, AgentStatus::Attention] {
            let (registry, identity) = native_bound_registry().await;
            registry
                .apply_native_observation(native_event(
                    serde_json::json!({"kind":"reset", "tracking_enabled":false}),
                    1,
                ))
                .await;
            let mut row = registry.lookup_identity(&identity).await.unwrap();
            row.status = Some(status);
            row.current_tool = Some("canonical-tool".into());
            row.attention_reason = Some("canonical-reason".into());
            row.last_error = Some("canonical-error".into());
            row.last_activity_at_ms = Some(42);
            registry.upsert(row.clone()).await;
            assert!(
                registry
                    .apply_native_observation(native_event(
                        serde_json::json!({"kind":"reset", "tracking_enabled":true}),
                        2,
                    ))
                    .await
            );
            assert_eq!(registry.lookup_identity(&identity).await, Some(row));
            assert!(registry.native_pane_observations().await.is_empty());
            assert!(
                !registry
                    .apply_native_observation(native_event(serde_json::json!({"kind":"bell"}), 1,))
                    .await
            );
        }
    }

    #[tokio::test]
    async fn native_observation_late_explicit_resume_assignment_reconciles_without_progress() {
        let registry = InMemoryRegistry::new();
        registry
            .apply_native_observation(native_event(
                serde_json::json!({"kind":"reset", "tracking_enabled":false}),
                1,
            ))
            .await;
        assert!(registry.native_pane_observations().await[0]
            .agent_session_id
            .is_none());

        let row = qualified_info("late-resume", "copilot", SessionLocation::Host, "history");
        let identity = SessionIdentity::from_info(&row);
        registry.upsert(row).await;
        registry.mark_resume_dispatched_identity(&identity).await;
        assert!(
            registry
                .assign_resume_pane_identity(&identity, NATIVE_PANE.into())
                .await
        );
        assert!(
            registry
                .lookup_identity(&identity)
                .await
                .unwrap()
                .born_bound_pane
        );
        registry
            .apply_native_watcher_activity(
                &CliSource::Copilot,
                "late-resume",
                SessionEvent::ToolStarting {
                    key: "late-resume".into(),
                    tool_name: "canonical-tool".into(),
                },
            )
            .await;
        // No additional native observation is required after the assignment.
        let observation = registry.native_pane_observations().await.remove(0);
        assert_eq!(observation.agent_session_id.as_deref(), Some("late-resume"));
        assert_eq!(observation.status, AgentStatus::Working);
        for event in [
            SessionEvent::PaneClosed {
                pane_session_id: NATIVE_PANE.into(),
            },
            SessionEvent::ConnectionFailed {
                pane_session_id: NATIVE_PANE.into(),
                reason: "retained".into(),
            },
        ] {
            assert!(!registry.apply_event(event).await);
        }
        assert_eq!(
            registry
                .lookup_active_by_pane(NATIVE_PANE)
                .await
                .unwrap()
                .status,
            Some(AgentStatus::Working)
        );
        registry
            .apply_native_observation(native_event(serde_json::json!({"kind":"bell"}), 1))
            .await;
        assert_eq!(
            registry.lookup_identity(&identity).await.unwrap().status,
            Some(AgentStatus::Attention)
        );
        registry
            .apply_native_observation(native_event(serde_json::json!({"kind":"removed"}), 1))
            .await;
        assert_eq!(
            registry.lookup_identity(&identity).await.unwrap().status,
            Some(AgentStatus::Ended)
        );
        assert!(registry.native_pane_observations().await.is_empty());
        assert_eq!(registry.snapshot().await.len(), 1);
    }

    #[tokio::test]
    async fn native_observation_off_transfer_preserves_owner_activity_and_fences_old_epoch() {
        for known in [true, false] {
            let registry = if known {
                native_bound_registry().await.0
            } else {
                InMemoryRegistry::new()
            };
            let mut event = native_event(
                serde_json::json!({"kind":"reset", "tracking_enabled":false}),
                1,
            );
            if known {
                event.agent_session_id = Some("real-session".into());
                event.location = Some(SessionLocation::Host);
            }
            registry.apply_native_observation(event.clone()).await;
            event.event = NativeObservationKind::Bell;
            registry.apply_native_observation(event.clone()).await;
            if known {
                let mut row = registry.snapshot().await.remove(0);
                row.last_activity_at_ms = Some(42);
                row.attention_reason = Some("canonical attention".into());
                registry.upsert(row).await;
            } else {
                // A later hook/history-like pane match is not explicit resume
                // provenance and must not promote the unknown owner on reset.
                let mut row = qualified_info(
                    "untrusted-match",
                    "copilot",
                    SessionLocation::Host,
                    "history",
                );
                row.status = Some(AgentStatus::Idle);
                row.pane_session_id = Some(NATIVE_PANE.into());
                registry.upsert(row).await;
            }
            let before = registry.native_pane_observations().await.remove(0);
            let rows_before = registry.snapshot().await;
            let mut transferred = event.clone();
            transferred.event = NativeObservationKind::Reset {
                tracking_enabled: false,
            };
            transferred.mode_epoch = 2;
            transferred.window_id = 8;
            transferred.tab_id = "transferred-tab".into();
            assert!(registry.apply_native_observation(transferred.clone()).await);
            let after = registry.native_pane_observations().await.remove(0);
            assert_eq!(after.status, AgentStatus::Attention);
            assert_eq!(after.last_activity_at_ms, before.last_activity_at_ms);
            assert_eq!(after.agent_session_id, before.agent_session_id);
            assert_eq!(after.window_id, 8);
            assert_eq!(after.tab_id, "transferred-tab");
            assert_eq!(registry.snapshot().await, rows_before);
            event.event = NativeObservationKind::Progress { progress_state: 1 };
            assert!(!registry.apply_native_observation(event).await);
            transferred.event = NativeObservationKind::Progress { progress_state: 1 };
            assert!(registry.apply_native_observation(transferred.clone()).await);
            if known {
                assert_eq!(
                    registry.native_pane_observations().await[0].status,
                    AgentStatus::Attention
                );
                registry
                    .apply_native_watcher_activity(
                        &CliSource::Copilot,
                        "real-session",
                        SessionEvent::ToolCompleted {
                            key: "real-session".into(),
                        },
                    )
                    .await;
                registry.apply_native_observation(transferred.clone()).await;
            }
            assert_eq!(
                registry.native_pane_observations().await[0].status,
                AgentStatus::Working
            );
            assert_eq!(registry.snapshot().await.len(), 1);
            if !known {
                assert!(registry.native_pane_observations().await[0]
                    .agent_session_id
                    .is_none());
                assert_eq!(registry.snapshot().await[0].status, Some(AgentStatus::Idle));
            }
            transferred.event = NativeObservationKind::Reset {
                tracking_enabled: true,
            };
            transferred.mode_epoch = 3;
            registry.apply_native_observation(transferred.clone()).await;
            transferred.event = NativeObservationKind::Reset {
                tracking_enabled: false,
            };
            transferred.mode_epoch = 4;
            registry.apply_native_observation(transferred.clone()).await;
            assert_eq!(
                registry.native_pane_observations().await[0].status,
                AgentStatus::Idle
            );
            transferred.event = NativeObservationKind::Bell;
            registry.apply_native_observation(transferred.clone()).await;
            transferred.event = NativeObservationKind::Reset {
                tracking_enabled: false,
            };
            transferred.agent_session_id = Some("different-owner".into());
            transferred.mode_epoch = 5;
            assert_eq!(registry.apply_native_observation(transferred).await, !known);
            let new_owner = registry.native_pane_observations().await.remove(0);
            if known {
                assert_eq!(new_owner.status, AgentStatus::Attention);
                assert_eq!(new_owner.agent_session_id.as_deref(), Some("real-session"));
                assert_eq!(new_owner.mode_epoch, 4);
            } else {
                assert_eq!(new_owner.status, AgentStatus::Idle);
                assert!(new_owner.agent_session_id.is_none());
            }
        }
    }

    #[tokio::test]
    async fn native_observation_on_clear_and_provider_only_reset_do_not_require_birth() {
        for (tracking_enabled, incoming_id) in
            [(true, Some("different-owner")), (true, None), (false, None)]
        {
            let (registry, identity) = native_bound_registry().await;
            assert!(
                registry
                    .apply_native_observation(native_event(
                        serde_json::json!({"kind":"reset", "tracking_enabled":false}),
                        1,
                    ))
                    .await
            );
            let owner = registry.lookup_identity(&identity).await.unwrap();
            let reset = native_event(
                serde_json::json!({
                    "kind":"reset", "tracking_enabled":tracking_enabled,
                    "provider_id":"claude", "agent_session_id":incoming_id
                }),
                2,
            );
            assert!(registry.apply_native_observation(reset.clone()).await);
            assert_eq!(registry.lookup_identity(&identity).await.unwrap(), owner);
            let observations = registry.native_pane_observations().await;
            if tracking_enabled {
                assert!(observations.is_empty());
                let mut stale_off = reset;
                stale_off.event = NativeObservationKind::Reset {
                    tracking_enabled: false,
                };
                assert!(!registry.apply_native_observation(stale_off).await);
                assert!(registry.native_pane_observations().await.is_empty());
            } else {
                assert_eq!(observations.len(), 1);
                assert_eq!(observations[0].provider_id, "claude");
                assert!(observations[0].agent_session_id.is_none());
                assert_eq!(observations[0].mode_epoch, 2);
            }
        }
    }

    #[tokio::test]
    async fn native_observation_late_binding_preserves_admitted_activity() {
        for activity in [
            NativeObservationKind::Bell,
            NativeObservationKind::Progress { progress_state: 1 },
            NativeObservationKind::Progress { progress_state: 2 },
        ] {
            let registry = InMemoryRegistry::new();
            let mut event = native_event(
                serde_json::json!({"kind":"reset", "tracking_enabled":false}),
                1,
            );
            registry.apply_native_observation(event.clone()).await;
            event.event = activity;
            registry.apply_native_observation(event).await;
            let observed = registry.native_pane_observations().await.remove(0);
            let row = qualified_info("late-activity", "copilot", SessionLocation::Host, "history");
            let identity = SessionIdentity::from_info(&row);
            registry.upsert(row).await;
            registry.mark_resume_dispatched_identity(&identity).await;
            registry
                .assign_resume_pane_identity(&identity, NATIVE_PANE.into())
                .await;
            let canonical = registry.lookup_identity(&identity).await.unwrap();
            assert_eq!(canonical.status, Some(observed.status.clone()));
            assert_eq!(
                canonical.last_activity_at_ms,
                Some(observed.last_activity_at_ms)
            );
            let native = registry.native_pane_observations().await.remove(0);
            assert_eq!(native.status, observed.status);
            assert_eq!(native.last_activity_at_ms, observed.last_activity_at_ms);
            assert_eq!(native.agent_session_id.as_deref(), Some("late-activity"));
        }
    }

    #[tokio::test]
    async fn native_observation_late_binding_preserves_newer_canonical_activity() {
        for status in [AgentStatus::Working, AgentStatus::Attention] {
            let registry = InMemoryRegistry::new();
            let mut event = native_event(
                serde_json::json!({"kind":"reset", "tracking_enabled":false}),
                1,
            );
            registry.apply_native_observation(event.clone()).await;
            event.event = NativeObservationKind::Progress { progress_state: 2 };
            registry.apply_native_observation(event).await;
            let observed = registry.native_pane_observations().await.remove(0);
            let mut row = qualified_info(
                "newer-activity",
                "copilot",
                SessionLocation::Host,
                "history",
            );
            row.status = Some(status.clone());
            row.last_activity_at_ms = Some(observed.last_activity_at_ms + 1);
            row.current_tool = Some("watcher-tool".into());
            row.attention_reason = Some("watcher attention".into());
            let identity = SessionIdentity::from_info(&row);
            registry.upsert(row.clone()).await;
            registry.mark_resume_dispatched_identity(&identity).await;
            registry
                .assign_resume_pane_identity(&identity, NATIVE_PANE.into())
                .await;
            let canonical = registry.lookup_identity(&identity).await.unwrap();
            assert_eq!(canonical.status, row.status);
            assert_eq!(canonical.last_activity_at_ms, row.last_activity_at_ms);
            assert_eq!(canonical.current_tool, row.current_tool);
            assert_eq!(canonical.attention_reason, row.attention_reason);
            let native = registry.native_pane_observations().await.remove(0);
            assert_eq!(native.status, status);
            assert_eq!(native.last_activity_at_ms, observed.last_activity_at_ms + 1);
        }
    }

    #[tokio::test]
    async fn native_observation_generic_progress_preserves_canonical_attention() {
        let (registry, identity) = native_bound_registry().await;
        registry
            .apply_native_observation(native_event(
                serde_json::json!({"kind":"reset", "tracking_enabled":false}),
                1,
            ))
            .await;
        registry
            .apply_native_watcher_activity(
                &CliSource::Copilot,
                "real-session",
                SessionEvent::ToolStarting {
                    key: "real-session".into(),
                    tool_name: "prompt".into(),
                },
            )
            .await;
        registry
            .apply_native_watcher_activity(
                &CliSource::Copilot,
                "real-session",
                SessionEvent::Notification {
                    key: "real-session".into(),
                    message: "input requested".into(),
                    timestamp_ms: None,
                },
            )
            .await;
        let waiting = registry.lookup_identity(&identity).await.unwrap();
        for progress in [1, 3, 0, 4] {
            registry
                .apply_native_observation(native_event(
                    serde_json::json!({"kind":"progress", "progress_state":progress}),
                    1,
                ))
                .await;
            assert_eq!(
                registry.lookup_identity(&identity).await,
                Some(waiting.clone())
            );
            assert_eq!(
                registry.native_pane_observations().await[0].status,
                AgentStatus::Attention
            );
        }
        registry
            .apply_native_observation(native_event(serde_json::json!({"kind":"bell"}), 1))
            .await;
        assert_eq!(registry.lookup_identity(&identity).await, Some(waiting));
        registry
            .apply_native_watcher_activity(
                &CliSource::Copilot,
                "real-session",
                SessionEvent::ToolCompleted {
                    key: "real-session".into(),
                },
            )
            .await;
        assert_eq!(
            registry.lookup_identity(&identity).await.unwrap().status,
            Some(AgentStatus::Idle)
        );
        registry
            .apply_native_observation(native_event(
                serde_json::json!({"kind":"progress", "progress_state":1}),
                1,
            ))
            .await;
        assert_eq!(
            registry.lookup_identity(&identity).await.unwrap().status,
            Some(AgentStatus::Working)
        );
        registry
            .apply_native_watcher_activity(
                &CliSource::Copilot,
                "real-session",
                SessionEvent::Notification {
                    key: "real-session".into(),
                    message: "ready for next request".into(),
                    timestamp_ms: None,
                },
            )
            .await;
        registry
            .apply_native_watcher_activity(
                &CliSource::Copilot,
                "real-session",
                SessionEvent::ToolStarting {
                    key: "real-session".into(),
                    tool_name: "prompt".into(),
                },
            )
            .await;
        registry
            .apply_native_observation(native_event(
                serde_json::json!({"kind":"progress", "progress_state":3}),
                1,
            ))
            .await;
        let working = registry.lookup_identity(&identity).await.unwrap();
        assert_eq!(working.status, Some(AgentStatus::Working));
        assert!(working.attention_reason.is_none());
        let mut tool_wait = working;
        tool_wait.status = Some(AgentStatus::Attention);
        tool_wait.current_tool = Some("ask_user".into());
        registry.upsert(tool_wait.clone()).await;
        registry
            .apply_native_observation(native_event(
                serde_json::json!({"kind":"progress", "progress_state":1}),
                1,
            ))
            .await;
        assert_eq!(registry.lookup_identity(&identity).await, Some(tool_wait));
    }

    #[tokio::test]
    async fn native_observation_bell_fallback_clears_and_explicit_error_overrides_attention() {
        let (registry, identity) = native_bound_registry().await;
        registry
            .apply_native_observation(native_event(
                serde_json::json!({"kind":"reset", "tracking_enabled":false}),
                1,
            ))
            .await;
        for (progress, status) in [
            (1, AgentStatus::Working),
            (3, AgentStatus::Working),
            (0, AgentStatus::Idle),
            (4, AgentStatus::Idle),
        ] {
            registry
                .apply_native_observation(native_event(serde_json::json!({"kind":"bell"}), 1))
                .await;
            let attention = registry.lookup_identity(&identity).await.unwrap();
            assert_eq!(attention.status, Some(AgentStatus::Attention));
            assert!(attention.current_tool.is_none());
            assert!(attention.attention_reason.is_none());
            registry
                .apply_native_observation(native_event(
                    serde_json::json!({"kind":"progress", "progress_state":progress}),
                    1,
                ))
                .await;
            assert_eq!(
                registry.lookup_identity(&identity).await.unwrap().status,
                Some(status)
            );
        }
        registry
            .apply_event(SessionEvent::Notification {
                key: "real-session".into(),
                message: "input requested".into(),
                timestamp_ms: None,
            })
            .await;
        registry
            .apply_native_observation(native_event(
                serde_json::json!({"kind":"progress", "progress_state":2}),
                1,
            ))
            .await;
        let error = registry.lookup_identity(&identity).await.unwrap();
        assert_eq!(error.status, Some(AgentStatus::Error));
        assert!(error.attention_reason.is_none());
        assert_eq!(
            registry.native_pane_observations().await[0].status,
            AgentStatus::Error
        );
        assert!(
            registry
                .apply_native_observation(native_event(
                    serde_json::json!({"kind":"progress", "progress_state":0}),
                    1,
                ))
                .await
        );
        let idle = registry.lookup_identity(&identity).await.unwrap();
        assert_eq!(idle.status, Some(AgentStatus::Idle));
        assert!(idle.attention_reason.is_none());
        assert!(idle.current_tool.is_none());
        assert_eq!(
            registry.native_pane_observations().await[0].status,
            AgentStatus::Idle
        );
    }

    #[tokio::test]
    async fn native_birth_registration_preserves_activity_and_requires_exact_ownership() {
        let registry = InMemoryRegistry::new();
        let cwd = std::path::Path::new("C:\\repo");
        let mut owner =
            qualified_info("confirmed-birth", "copilot", SessionLocation::Host, "owner");
        owner.pane_session_id = Some(NATIVE_PANE.into());
        owner.status = Some(AgentStatus::Attention);
        owner.current_tool = Some("ask_user".into());
        owner.attention_reason = Some("ask_user".into());
        owner.last_activity_at_ms = Some(123);
        let identity = SessionIdentity::from_info(&owner);
        registry.upsert(owner.clone()).await;
        for (provider, location) in [
            ("claude", SessionLocation::Host),
            (
                "copilot",
                SessionLocation::Wsl {
                    distro: "Ubuntu".into(),
                },
            ),
        ] {
            let other = qualified_info("confirmed-birth", provider, location, "other");
            let other_identity = SessionIdentity::from_info(&other);
            registry.upsert(other).await;
            assert!(
                !registry
                    .register_native_born_binding(&other_identity, NATIVE_PANE, cwd, None, None)
                    .await
            );
        }
        assert!(
            !registry
                .register_native_born_binding(&identity, "another-pane", cwd, None, None)
                .await
        );
        assert_eq!(registry.lookup_identity(&identity).await.unwrap(), owner);
        assert!(
            registry
                .register_native_born_binding(&identity, NATIVE_PANE, cwd, None, None)
                .await
        );
        owner.born_bound_pane = true;
        assert_eq!(registry.lookup_identity(&identity).await.unwrap(), owner);
        assert!(
            !registry
                .register_native_born_binding(&identity, NATIVE_PANE, cwd, None, None)
                .await
        );
        assert_eq!(registry.lookup_identity(&identity).await.unwrap(), owner);

        let helper_registry = InMemoryRegistry::new();
        owner.origin = Some(SessionOrigin::AgentPane);
        owner.born_bound_pane = false;
        helper_registry.upsert(owner.clone()).await;
        assert!(
            !helper_registry
                .register_native_born_binding(&identity, NATIVE_PANE, cwd, None, None)
                .await
        );
        assert_eq!(
            helper_registry.lookup_identity(&identity).await.unwrap(),
            owner
        );
    }

    #[tokio::test]
    async fn native_observation_compact_retirement_releases_churn_records_and_scan_capacity() {
        let registry = InMemoryRegistry::new();
        registry
            .apply_native_observation(native_event(
                serde_json::json!({"kind":"reset", "tracking_enabled":false}),
                1,
            ))
            .await;
        registry
            .apply_native_observation(native_event(serde_json::json!({"kind":"bell"}), 1))
            .await;
        for index in 1..=10_000 {
            let mut event = native_event(
                serde_json::json!({"kind":"reset", "tracking_enabled":false}),
                1,
            );
            event.pane_id = uuid::Uuid::from_u128(index).to_string();
            assert!(registry.apply_native_observation(event.clone()).await);
            event.event = NativeObservationKind::Bell;
            assert!(registry.apply_native_observation(event).await);
        }
        {
            let state = registry.inner.lock().await;
            assert_eq!(state.native_panes.len(), 10_001);
            assert!(state.native_panes.capacity() > 64);
        }
        for index in 1..=10_000 {
            let mut event = native_event(serde_json::json!({"kind":"removed"}), 1);
            event.pane_id = uuid::Uuid::from_u128(index).to_string();
            assert!(registry.apply_native_observation(event).await);
        }
        let observations = registry.native_pane_observations().await;
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].pane_id, NATIVE_PANE);
        assert_eq!(observations[0].status, AgentStatus::Attention);
        assert!(registry.snapshot().await.is_empty());
        let state = registry.inner.lock().await;
        assert_eq!(state.native_panes.len(), 1);
        assert!(state.native_panes.capacity() <= 64);
        assert_eq!(state.native_retired_epochs.len(), 10_000);
        for index in 1..=10_000 {
            assert_eq!(
                state
                    .native_retired_epochs
                    .get(&uuid::Uuid::from_u128(index)),
                Some(&1)
            );
        }
    }

    #[tokio::test]
    async fn native_observation_compact_retirement_fences_replay_and_on_cycles() {
        let registry = InMemoryRegistry::new();
        let mut event = native_event(
            serde_json::json!({"kind":"reset", "tracking_enabled":false}),
            1,
        );
        registry.apply_native_observation(event.clone()).await;
        event.event = NativeObservationKind::Removed;
        registry.apply_native_observation(event.clone()).await;
        for epoch in [0, 1] {
            let mut replay = event.clone();
            replay.mode_epoch = epoch;
            replay.event = NativeObservationKind::Reset {
                tracking_enabled: false,
            };
            assert!(!registry.apply_native_observation(replay).await);
        }
        for kind in [
            NativeObservationKind::Bell,
            NativeObservationKind::Progress { progress_state: 1 },
            NativeObservationKind::Removed,
        ] {
            let mut replay = event.clone();
            replay.event = kind;
            assert!(!registry.apply_native_observation(replay).await);
        }
        event.mode_epoch = 2;
        event.event = NativeObservationKind::Reset {
            tracking_enabled: true,
        };
        assert!(registry.apply_native_observation(event.clone()).await);
        event.event = NativeObservationKind::Reset {
            tracking_enabled: false,
        };
        assert!(!registry.apply_native_observation(event.clone()).await);
        event.mode_epoch = 3;
        assert!(registry.apply_native_observation(event.clone()).await);
        assert_eq!(
            registry.native_pane_observations().await[0].status,
            AgentStatus::Idle
        );
        {
            let state = registry.inner.lock().await;
            assert!(!state
                .native_retired_epochs
                .contains_key(&uuid::Uuid::parse_str(NATIVE_PANE).unwrap()));
            assert_eq!(state.native_panes.len(), 1);
        }
        event.event = NativeObservationKind::Bell;
        registry.apply_native_observation(event.clone()).await;
        let before = registry.native_pane_observations().await.remove(0);
        event.mode_epoch = 4;
        event.tab_id = "transferred".into();
        event.window_id = 8;
        event.event = NativeObservationKind::Reset {
            tracking_enabled: false,
        };
        assert!(registry.apply_native_observation(event.clone()).await);
        let transferred = registry.native_pane_observations().await.remove(0);
        assert_eq!(transferred.status, AgentStatus::Attention);
        assert_eq!(transferred.last_activity_at_ms, before.last_activity_at_ms);
        assert_eq!(transferred.tab_id, "transferred");
        event.event = NativeObservationKind::Removed;
        registry.apply_native_observation(event).await;
        let state = registry.inner.lock().await;
        assert!(state.native_panes.is_empty());
        assert_eq!(
            state
                .native_retired_epochs
                .get(&uuid::Uuid::parse_str(NATIVE_PANE).unwrap()),
            Some(&4)
        );
    }

    #[tokio::test]
    async fn native_observation_compact_retirement_keeps_epochs_per_guid_not_global() {
        let registry = InMemoryRegistry::new();
        let mut first = native_event(
            serde_json::json!({"kind":"reset", "tracking_enabled":false}),
            10,
        );
        registry.apply_native_observation(first.clone()).await;
        first.event = NativeObservationKind::Removed;
        registry.apply_native_observation(first.clone()).await;
        let mut second = first.clone();
        second.pane_id = uuid::Uuid::from_u128(2).to_string();
        second.mode_epoch = 11;
        second.event = NativeObservationKind::Reset {
            tracking_enabled: true,
        };
        registry.apply_native_observation(second.clone()).await;
        // Another pane's higher generation cannot invalidate a delayed but
        // newer reset for this pane, or a fresh producer's first admission.
        first.mode_epoch = 11;
        first.event = NativeObservationKind::Reset {
            tracking_enabled: false,
        };
        assert!(registry.apply_native_observation(first).await);
        second.pane_id = uuid::Uuid::from_u128(3).to_string();
        second.mode_epoch = 1;
        second.event = NativeObservationKind::Reset {
            tracking_enabled: false,
        };
        assert!(registry.apply_native_observation(second).await);
        assert_eq!(registry.native_pane_observations().await.len(), 2);
    }

    #[tokio::test]
    async fn native_off_generic_opencode_lifecycle_cannot_detach_owner() {
        for event in [
            SessionEvent::SessionUnbound {
                key: "real-session".into(),
                pane_session_id: NATIVE_PANE.into(),
            },
            SessionEvent::SessionStoppedIfOwner {
                key: "real-session".into(),
                pane_session_id: Some(NATIVE_PANE.into()),
                reason: "exit".into(),
            },
        ] {
            let registry = InMemoryRegistry::new();
            let mut row = SessionInfo::new("real-session".into(), PathBuf::from(r"C:\repo"))
                .with_pane_session_id(NATIVE_PANE);
            row.provider_id = Some("opencode".into());
            row.cli_source = Some(CliSource::OpenCode);
            row.location = SessionLocation::Host;
            row.status = Some(AgentStatus::Working);
            row.born_bound_pane = true;
            let identity = SessionIdentity::from_info(&row);
            registry.upsert(row).await;
            let mut reset = native_event(
                serde_json::json!({"kind":"reset","tracking_enabled":false}),
                1,
            );
            reset.provider_id = "opencode".into();
            assert!(registry.apply_native_observation(reset.clone()).await);
            let before = registry.lookup_identity(&identity).await.unwrap();
            assert!(!registry.apply_event(event.clone()).await);
            assert_eq!(registry.lookup_identity(&identity).await.unwrap(), before);
            assert_eq!(
                registry.inner.lock().await.active_by_pane.get(NATIVE_PANE),
                Some(&identity)
            );
            reset.mode_epoch = 2;
            reset.event = NativeObservationKind::Reset {
                tracking_enabled: true,
            };
            assert!(registry.apply_native_observation(reset).await);
            assert!(registry.apply_event(event).await);
        }
    }

    #[tokio::test]
    async fn native_off_generic_tool_start_preserves_osc() {
        assert_native_off_generic_activity_rejected(SessionEvent::ToolStarting {
            key: "real-session".into(),
            tool_name: "ReadFile".into(),
        })
        .await;
    }

    #[tokio::test]
    async fn native_off_generic_tool_complete_preserves_osc() {
        assert_native_off_generic_activity_rejected(SessionEvent::ToolCompleted {
            key: "real-session".into(),
        })
        .await;
    }

    #[tokio::test]
    async fn native_off_generic_tool_return_preserves_osc() {
        assert_native_off_generic_activity_rejected(SessionEvent::ToolReturned {
            key: "real-session".into(),
            timestamp_ms: None,
        })
        .await;
    }

    #[tokio::test]
    async fn native_off_generic_notification_preserves_osc() {
        assert_native_off_generic_activity_rejected(SessionEvent::Notification {
            key: "real-session".into(),
            message: "ask_user".into(),
            timestamp_ms: None,
        })
        .await;
    }

    async fn assert_native_off_generic_activity_rejected(event: SessionEvent) {
        let (registry, identity) = native_bound_registry().await;
        let mut row = registry.lookup_identity(&identity).await.unwrap();
        row.cli_source = Some(CliSource::Copilot);
        registry.upsert(row).await;
        assert!(
            registry
                .apply_native_observation(native_event(
                    serde_json::json!({"kind":"reset","tracking_enabled":false}),
                    1
                ))
                .await
        );
        assert!(
            registry
                .apply_native_observation(native_event(
                    serde_json::json!({"kind":"progress","progress_state":1}),
                    1
                ))
                .await
        );
        let before = registry.lookup_identity(&identity).await.unwrap();
        let observation = registry.native_pane_observations().await;
        assert!(!registry.apply_event(event).await);
        assert_eq!(registry.lookup_identity(&identity).await.unwrap(), before);
        assert_eq!(registry.native_pane_observations().await, observation);
    }

    fn native_activity_variants() -> Vec<(SessionEvent, AgentStatus)> {
        vec![
            (
                SessionEvent::ToolStarting {
                    key: "real-session".into(),
                    tool_name: "ReadFile".into(),
                },
                AgentStatus::Working,
            ),
            (
                SessionEvent::ToolCompleted {
                    key: "real-session".into(),
                },
                AgentStatus::Idle,
            ),
            (
                SessionEvent::Notification {
                    key: "real-session".into(),
                    message: "ask_user".into(),
                    timestamp_ms: None,
                },
                AgentStatus::Attention,
            ),
        ]
    }

    #[tokio::test]
    async fn native_off_generic_guard_preserves_qualified_watcher_activity() {
        for (event, status) in native_activity_variants() {
            let (registry, identity) = native_bound_registry().await;
            assert!(
                registry
                    .apply_native_observation(native_event(
                        serde_json::json!({"kind":"reset","tracking_enabled":false}),
                        1
                    ))
                    .await
            );
            assert!(
                registry
                    .apply_native_observation(native_event(
                        serde_json::json!({"kind":"progress","progress_state":1}),
                        1
                    ))
                    .await
            );
            assert_eq!(
                registry
                    .apply_native_watcher_activity(&CliSource::Copilot, "real-session", event)
                    .await,
                Some(true)
            );
            assert_eq!(
                registry.lookup_identity(&identity).await.unwrap().status,
                Some(status.clone())
            );
            assert_eq!(registry.native_pane_observations().await[0].status, status);
        }
    }

    #[tokio::test]
    async fn native_off_generic_guard_leaves_on_and_helper_activity_enabled() {
        for helper in [false, true] {
            for (event, status) in native_activity_variants() {
                let (registry, identity) = native_bound_registry().await;
                if helper {
                    let mut row = registry.lookup_identity(&identity).await.unwrap();
                    row.origin = Some(SessionOrigin::AgentPane);
                    registry.upsert(row).await;
                } else {
                    assert!(
                        registry
                            .apply_native_observation(native_event(
                                serde_json::json!({"kind":"reset","tracking_enabled":false}),
                                1
                            ))
                            .await
                    );
                    assert!(
                        registry
                            .apply_native_observation(native_event(
                                serde_json::json!({"kind":"progress","progress_state":1}),
                                1
                            ))
                            .await
                    );
                    assert!(
                        registry
                            .apply_native_observation(native_event(
                                serde_json::json!({"kind":"reset","tracking_enabled":true}),
                                2
                            ))
                            .await
                    );
                }
                assert!(registry.native_pane_observations().await.is_empty());
                assert!(registry.apply_event(event).await);
                assert_eq!(
                    registry.lookup_identity(&identity).await.unwrap().status,
                    Some(status)
                );
            }
        }
    }

    #[tokio::test]
    async fn native_observation_compact_retirement_revokes_watcher_ownership() {
        let (registry, identity) = native_bound_registry().await;
        registry
            .apply_native_observation(native_event(
                serde_json::json!({"kind":"reset", "tracking_enabled":false}),
                1,
            ))
            .await;
        let activity = SessionEvent::Notification {
            key: "real-session".into(),
            message: "ask_user".into(),
            timestamp_ms: None,
        };
        assert_eq!(
            registry
                .apply_native_watcher_activity(
                    &CliSource::Copilot,
                    "real-session",
                    activity.clone(),
                )
                .await,
            Some(true)
        );
        registry
            .apply_native_observation(native_event(serde_json::json!({"kind":"removed"}), 1))
            .await;
        assert_eq!(
            registry
                .apply_native_watcher_activity(
                    &CliSource::Copilot,
                    "real-session",
                    activity.clone(),
                )
                .await,
            None
        );
        assert!(!registry.apply_event(activity).await);
        assert_eq!(
            registry.lookup_identity(&identity).await.unwrap().status,
            Some(AgentStatus::Ended)
        );
        assert!(registry.native_pane_observations().await.is_empty());
        assert!(registry.inner.lock().await.native_panes.is_empty());
    }

    #[tokio::test]
    async fn native_observation_late_binding_projects_canonical_location() {
        for location in [
            SessionLocation::Host,
            SessionLocation::Wsl {
                distro: "Ubuntu".into(),
            },
        ] {
            let registry = InMemoryRegistry::new();
            registry
                .apply_native_observation(native_event(
                    serde_json::json!({"kind":"reset", "tracking_enabled":false}),
                    1,
                ))
                .await;
            let unknown = registry.native_pane_observations().await.remove(0);
            assert!(unknown.agent_session_id.is_none());
            assert!(unknown.location.is_none());
            let row = qualified_info("source-resume", "copilot", location.clone(), "history");
            let identity = SessionIdentity::from_info(&row);
            registry.upsert(row).await;
            let other = if location == SessionLocation::Host {
                SessionLocation::Wsl {
                    distro: "Ubuntu".into(),
                }
            } else {
                SessionLocation::Host
            };
            registry
                .upsert(qualified_info(
                    "source-resume",
                    "copilot",
                    other,
                    "other history",
                ))
                .await;
            registry.mark_resume_dispatched_identity(&identity).await;
            registry
                .assign_resume_pane_identity(&identity, NATIVE_PANE.into())
                .await;
            let bound = registry.native_pane_observations().await.remove(0);
            assert_eq!(bound.agent_session_id.as_deref(), Some("source-resume"));
            assert_eq!(bound.location, Some(location));
            assert_eq!(bound.status, AgentStatus::Idle);
            registry
                .assign_resume_pane_identity(&identity, uuid::Uuid::from_u128(2).to_string())
                .await;
            let rebound = registry.native_pane_observations().await.remove(0);
            assert!(rebound.agent_session_id.is_none());
            assert!(rebound.location.is_none());
        }
    }

    #[tokio::test]
    async fn native_observation_wrong_source_cannot_project_an_unmatched_history_row() {
        let registry = InMemoryRegistry::new();
        let mut event = native_event(
            serde_json::json!({"kind":"reset", "tracking_enabled":false}),
            1,
        );
        event.agent_session_id = Some("source-mismatch".into());
        event.location = Some(SessionLocation::Host);
        registry.apply_native_observation(event).await;
        let row = qualified_info(
            "source-mismatch",
            "copilot",
            SessionLocation::Wsl {
                distro: "Ubuntu".into(),
            },
            "history",
        );
        let identity = SessionIdentity::from_info(&row);
        registry.upsert(row).await;
        registry.mark_resume_dispatched_identity(&identity).await;
        registry
            .assign_resume_pane_identity(&identity, NATIVE_PANE.into())
            .await;
        let unmatched = registry.native_pane_observations().await.remove(0);
        assert!(unmatched.agent_session_id.is_none());
        assert_eq!(unmatched.location, Some(SessionLocation::Host));
        assert_eq!(
            registry.lookup_identity(&identity).await.unwrap().location,
            SessionLocation::Wsl {
                distro: "Ubuntu".into()
            }
        );
    }

    #[test]
    fn native_receiver_metadata_round_trips_and_legacy_fields_remain_optional() {
        let raw = build_sessions_list_response(
            Vec::new(),
            None,
            None,
            Vec::new(),
            Some(7),
            Some(false),
            Vec::new(),
        );
        let parsed = parse_sessions_list_response(&raw).unwrap();
        assert_eq!(parsed.native_observation_receiver_generation, Some(7));
        assert_eq!(parsed.native_observation_listener_ready, Some(false));
        let legacy = build_sessions_list_response(
            Vec::new(),
            None,
            None,
            Vec::new(),
            None,
            None,
            Vec::new(),
        );
        let value: serde_json::Value = serde_json::from_str(legacy.get()).unwrap();
        assert!(value
            .get("native_observation_receiver_generation")
            .is_none());
        assert!(value.get("native_observation_listener_ready").is_none());
        let parsed = parse_sessions_list_response(&legacy).unwrap();
        assert!(parsed.native_observation_receiver_generation.is_none());
        assert!(parsed.native_observation_listener_ready.is_none());
    }

    fn info(id: &str, pane: Option<&str>) -> SessionInfo {
        let mut s = SessionInfo::new(
            acp::schema::v1::SessionId::new(id.to_string()),
            PathBuf::from("/tmp"),
        );
        if let Some(p) = pane {
            s = s.with_pane_session_id(p.to_string());
        }
        s
    }

    fn qualified_info(
        id: &str,
        provider: &str,
        location: SessionLocation,
        title: &str,
    ) -> SessionInfo {
        let mut row = info(id, None);
        row.provider_id = Some(provider.to_string());
        row.location = location;
        row.title = Some(title.to_string());
        row.status = Some(AgentStatus::Historical);
        row
    }

    #[tokio::test]
    async fn upsert_then_lookup_returns_clone() {
        let reg = InMemoryRegistry::new();
        let original = info("sess-1", Some("pane-A"));
        reg.upsert(original.clone()).await;
        let found = reg
            .lookup(&acp::schema::v1::SessionId::new("sess-1".to_string()))
            .await
            .expect("session present");
        assert_eq!(found, original);
    }

    #[tokio::test]
    async fn lookup_miss_returns_none() {
        let reg = InMemoryRegistry::new();
        assert!(reg
            .lookup(&acp::schema::v1::SessionId::new("missing".to_string()))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn lookup_active_by_pane_normalizes_guid_and_tracks_current_binding() {
        let reg = InMemoryRegistry::new();
        let mut first = info("sess-first", Some("{ABCDEFAB-1234-5678-9ABC-DEFABCDEFABC}"));
        first.status = Some(AgentStatus::Idle);
        reg.upsert(first).await;
        let mut second = info("sess-second", Some("abcdefab-1234-5678-9abc-defabcdefabc"));
        second.status = Some(AgentStatus::Working);
        reg.upsert(second.clone()).await;

        let found = reg
            .lookup_active_by_pane("{ABCDEFAB-1234-5678-9ABC-DEFABCDEFABC}")
            .await
            .expect("normalized pane key should resolve current owner");
        assert_eq!(found.session_id, second.session_id);
    }

    #[tokio::test]
    async fn lookup_active_by_pane_preserves_qualified_collisions_without_mutation() {
        let reg = InMemoryRegistry::new();
        let mut rows = [
            qualified_info("same-id", "copilot", SessionLocation::Host, "host"),
            qualified_info("same-id", "claude", SessionLocation::Host, "claude"),
            qualified_info(
                "same-id",
                "copilot",
                SessionLocation::Wsl {
                    distro: "Ubuntu".into(),
                },
                "wsl",
            ),
        ];
        for (index, row) in rows.iter_mut().enumerate() {
            row.status = Some(AgentStatus::Working);
            row.pane_session_id = Some(format!("pane-{index}"));
            reg.upsert(row.clone()).await;
        }
        assert!(reg.lookup(&rows[0].session_id).await.is_none());
        for row in &rows {
            assert_eq!(
                reg.lookup_active_by_pane(row.pane_session_id.as_deref().unwrap())
                    .await
                    .as_ref(),
                Some(row)
            );
            assert_eq!(
                reg.lookup_identity(&SessionIdentity::from_info(row))
                    .await
                    .as_ref(),
                Some(row)
            );
        }
        assert_eq!(reg.snapshot().await.len(), rows.len());
    }

    #[tokio::test]
    async fn lookup_active_by_pane_tracks_qualified_resume_rebind_close_and_remove() {
        let reg = InMemoryRegistry::new();
        let row = qualified_info("resumed-id", "copilot", SessionLocation::Host, "resumed");
        let identity = SessionIdentity::from_info(&row);
        reg.upsert(row.clone()).await;
        assert!(reg
            .mark_resume_dispatched_identity(&identity)
            .await
            .is_some());
        assert!(
            reg.assign_resume_pane_identity(&identity, "pane-first".into())
                .await
        );
        assert_eq!(
            reg.lookup_active_by_pane("pane-first")
                .await
                .map(|info| info.session_id),
            Some(row.session_id.clone())
        );
        assert!(
            reg.assign_resume_pane_identity(&identity, "pane-second".into())
                .await
        );
        assert!(reg.lookup_active_by_pane("pane-first").await.is_none());
        assert!(reg.lookup_active_by_pane("pane-second").await.is_some());
        assert!(
            reg.apply_event(SessionEvent::PaneClosed {
                pane_session_id: "pane-second".into(),
            })
            .await
        );
        assert!(reg.lookup_active_by_pane("pane-second").await.is_none());
        assert!(reg
            .mark_resume_dispatched_identity(&identity)
            .await
            .is_some());
        assert!(
            reg.assign_resume_pane_identity(&identity, "pane-third".into())
                .await
        );
        assert!(reg.lookup_active_by_pane("pane-third").await.is_some());
        assert!(reg.remove_identity(&identity).await.is_some());
        assert!(reg.lookup_active_by_pane("pane-third").await.is_none());
    }

    #[tokio::test]
    async fn lookup_active_by_pane_omits_missing_unknown_ended_historical_and_synthetic() {
        let reg = InMemoryRegistry::new();
        assert!(reg.lookup_active_by_pane("missing-pane").await.is_none());

        reg.upsert(info("unknown-status", Some("pane-unknown")))
            .await;
        assert!(reg.lookup_active_by_pane("pane-unknown").await.is_none());

        let mut ended = info("ended", Some("pane-ended"));
        ended.status = Some(AgentStatus::Ended);
        reg.upsert(ended).await;
        assert!(reg.lookup_active_by_pane("pane-ended").await.is_none());

        let mut historical = info("historical", Some("pane-historical"));
        historical.status = Some(AgentStatus::Historical);
        reg.upsert(historical).await;
        assert!(reg.lookup_active_by_pane("pane-historical").await.is_none());

        for sid in ["", "   ", "pane:synthetic"] {
            let mut synthetic = info(sid, Some(&format!("pane-{sid}")));
            synthetic.status = Some(AgentStatus::Idle);
            reg.upsert(synthetic).await;
            assert!(reg
                .lookup_active_by_pane(&format!("pane-{sid}"))
                .await
                .is_none());
        }
    }

    #[tokio::test]
    async fn lookup_active_by_pane_accepts_all_known_live_statuses() {
        let reg = InMemoryRegistry::new();
        for status in [
            AgentStatus::Idle,
            AgentStatus::Working,
            AgentStatus::Attention,
            AgentStatus::Error,
        ] {
            let mut row = info("live-session", Some("pane-live"));
            row.status = Some(status);
            reg.upsert(row.clone()).await;
            assert_eq!(reg.lookup_active_by_pane("pane-live").await, Some(row));
        }
    }

    #[tokio::test]
    async fn lookup_active_by_pane_omits_only_copilot_sidekicks() {
        let reg = InMemoryRegistry::new();
        for (provider, cli_source, accepted) in [
            (Some("copilot"), None, false),
            (Some(" COPILOT "), Some(CliSource::Claude), false),
            (None, Some(CliSource::Copilot), false),
            (Some("claude"), Some(CliSource::Copilot), true),
            (Some("custom:local"), Some(CliSource::Copilot), true),
            (Some("claude"), Some(CliSource::Claude), true),
            (Some("custom:local"), None, true),
            (None, Some(CliSource::Claude), true),
            (None, None, true),
        ] {
            let mut row = info("sidekick-opaque-id", Some("pane-sidekick"));
            row.provider_id = provider.map(str::to_string);
            row.cli_source = cli_source;
            row.location = SessionLocation::Host;
            row.status = Some(AgentStatus::Idle);
            reg.upsert(row.clone()).await;
            assert_eq!(
                reg.lookup_active_by_pane("pane-sidekick").await,
                accepted.then_some(row),
                "provider: {provider:?}"
            );
        }
    }

    #[tokio::test]
    async fn lookup_active_by_pane_preserves_owner_after_stale_row_mutation() {
        let mut other_universe = qualified_info(
            "same-id",
            "copilot",
            SessionLocation::Host,
            "other universe",
        );
        other_universe.session_universe = Some("other".into());
        for mut stale in [
            info("different-id", None),
            qualified_info("same-id", "claude", SessionLocation::Host, "other provider"),
            qualified_info(
                "same-id",
                "copilot",
                SessionLocation::Wsl {
                    distro: "Ubuntu".into(),
                },
                "other location",
            ),
            other_universe,
        ] {
            stale.status = Some(AgentStatus::Idle);
            stale.pane_session_id = Some("{ABCDEFAB-1234-5678-9ABC-DEFABCDEFABC}".into());
            let stale_identity = SessionIdentity::from_info(&stale);
            for mutation in ["remove", "unbind", "rebind", "resume", "end"] {
                let reg = InMemoryRegistry::new();
                let mut current =
                    qualified_info("same-id", "copilot", SessionLocation::Host, "current");
                current.status = Some(AgentStatus::Working);
                current.pane_session_id = Some("abcdefab-1234-5678-9abc-defabcdefabc".into());
                reg.upsert(stale.clone()).await;
                reg.upsert(current.clone()).await;

                if mutation == "remove" {
                    assert_eq!(
                        reg.remove_identity(&stale_identity).await,
                        Some(stale.clone())
                    );
                    assert!(reg.lookup_identity(&stale_identity).await.is_none());
                } else if mutation == "resume" {
                    assert!(
                        reg.assign_resume_pane_identity(&stale_identity, "pane-new".into())
                            .await
                    );
                    let resumed = reg.lookup_identity(&stale_identity).await.unwrap();
                    assert_eq!(resumed.pane_session_id.as_deref(), Some("pane-new"));
                    assert!(resumed.born_bound_pane);
                    assert_eq!(reg.lookup_active_by_pane("pane-new").await, Some(resumed));
                } else if mutation == "end" {
                    assert!(end_entry(&mut *reg.inner.lock().await, &stale_identity, 42));
                    let ended = reg.lookup_identity(&stale_identity).await.unwrap();
                    assert_eq!(ended.status, Some(AgentStatus::Ended));
                    assert!(ended.pane_session_id.is_none());
                } else {
                    let mut updated = stale.clone();
                    updated.pane_session_id = (mutation == "rebind").then(|| "pane-new".into());
                    reg.upsert(updated.clone()).await;
                    assert_eq!(
                        reg.lookup_identity(&stale_identity).await,
                        Some(updated.clone())
                    );
                    assert_eq!(
                        reg.lookup_active_by_pane("pane-new").await,
                        (mutation == "rebind").then_some(updated)
                    );
                }
                assert_eq!(
                    reg.lookup_active_by_pane("{ABCDEFAB-1234-5678-9ABC-DEFABCDEFABC}")
                        .await,
                    Some(current.clone()),
                    "{mutation} of {stale_identity:?} must preserve the current pane owner"
                );
                assert!(
                    reg.apply_event(SessionEvent::PaneClosed {
                        pane_session_id: "{ABCDEFAB-1234-5678-9ABC-DEFABCDEFABC}".into(),
                    })
                    .await
                );
                assert!(reg
                    .lookup_active_by_pane("abcdefab-1234-5678-9abc-defabcdefabc")
                    .await
                    .is_none());
                let closed = reg
                    .lookup_identity(&SessionIdentity::from_info(&current))
                    .await
                    .unwrap();
                assert_eq!(closed.status, Some(AgentStatus::Ended));
                assert!(closed.pane_session_id.is_none());
                if matches!(mutation, "resume" | "rebind") {
                    assert!(reg.lookup_active_by_pane("pane-new").await.is_some());
                }
            }
        }
    }

    #[tokio::test]
    async fn lookup_active_by_pane_preserves_owner_after_stale_row_lifecycle_event() {
        for mutation in ["stop", "complete", "start"] {
            let reg = InMemoryRegistry::new();
            // Hook events require unique raw IDs, but both rows remain qualified.
            let mut stale = qualified_info("stale-id", "copilot", SessionLocation::Host, "stale");
            stale.status = Some(AgentStatus::Working);
            stale.pane_session_id = Some("{ABCDEFAB-1234-5678-9ABC-DEFABCDEFABC}".into());
            stale.born_bound_pane = true;
            stale.origin = (mutation != "start").then_some(SessionOrigin::AgentPane);
            let identity = SessionIdentity::from_info(&stale);
            let mut current =
                qualified_info("current-id", "copilot", SessionLocation::Host, "current");
            current.status = Some(AgentStatus::Working);
            current.pane_session_id = Some("abcdefab-1234-5678-9abc-defabcdefabc".into());
            reg.upsert(stale.clone()).await;
            reg.upsert(current.clone()).await;

            let event = if mutation == "start" {
                SessionEvent::SessionStarted {
                    key: stale.session_id.to_string(),
                    cli_source: CliSource::Copilot,
                    pane_session_id: "pane-new".into(),
                    cwd: stale.cwd.clone(),
                    title: "restarted".into(),
                }
            } else {
                SessionEvent::SessionStopped {
                    key: stale.session_id.to_string(),
                    reason: mutation.into(),
                }
            };
            assert!(reg.apply_event(event).await);
            let updated = reg.lookup_identity(&identity).await.unwrap();
            assert!(!updated.born_bound_pane);
            if mutation == "start" {
                assert_eq!(updated.pane_session_id.as_deref(), Some("pane-new"));
                assert_eq!(reg.lookup_active_by_pane("pane-new").await, Some(updated));
            } else {
                assert_eq!(updated.status, Some(AgentStatus::Ended));
                assert!(updated.pane_session_id.is_none());
            }
            assert_eq!(
                reg.lookup_active_by_pane("{ABCDEFAB-1234-5678-9ABC-DEFABCDEFABC}")
                    .await,
                Some(current.clone()),
                "{mutation} must preserve the current pane owner"
            );
            assert!(
                reg.apply_event(SessionEvent::PaneClosed {
                    pane_session_id: "{ABCDEFAB-1234-5678-9ABC-DEFABCDEFABC}".into(),
                })
                .await
            );
            assert!(reg
                .lookup_active_by_pane("abcdefab-1234-5678-9abc-defabcdefabc")
                .await
                .is_none());
            let closed = reg
                .lookup_identity(&SessionIdentity::from_info(&current))
                .await
                .unwrap();
            assert_eq!(closed.status, Some(AgentStatus::Ended));
            assert!(closed.pane_session_id.is_none());
            if mutation == "start" {
                assert!(reg.lookup_active_by_pane("pane-new").await.is_some());
            }
        }
    }

    #[tokio::test]
    async fn upsert_is_idempotent_and_replaces() {
        let reg = InMemoryRegistry::new();
        reg.upsert(info("sess-1", Some("pane-A"))).await;
        reg.upsert(info("sess-1", Some("pane-B"))).await;
        let found = reg
            .lookup(&acp::schema::v1::SessionId::new("sess-1".to_string()))
            .await
            .unwrap();
        assert_eq!(found.pane_session_id.as_deref(), Some("pane-B"));
        assert_eq!(reg.snapshot().await.len(), 1, "no duplicate rows");
    }

    #[tokio::test]
    async fn qualified_rows_with_equal_raw_ids_coexist_and_raw_lookup_fails_closed() {
        let reg = InMemoryRegistry::new();
        let copilot = qualified_info("same-id", "copilot", SessionLocation::Host, "copilot");
        let claude = qualified_info("same-id", "claude", SessionLocation::Host, "claude");
        let ubuntu = qualified_info(
            "same-id",
            "copilot",
            SessionLocation::Wsl {
                distro: "Ubuntu".to_string(),
            },
            "ubuntu",
        );
        let copilot_identity = SessionIdentity::from_info(&copilot);
        let claude_identity = SessionIdentity::from_info(&claude);
        let ubuntu_identity = SessionIdentity::from_info(&ubuntu);

        reg.upsert(copilot).await;
        reg.upsert(claude).await;
        reg.upsert(ubuntu).await;

        assert_eq!(reg.snapshot().await.len(), 3);
        assert!(
            reg.lookup(&acp::schema::v1::SessionId::new("same-id".to_string()))
                .await
                .is_none(),
            "ambiguous raw lookup must not select an arbitrary provider or location"
        );
        assert_eq!(
            reg.lookup_identity(&copilot_identity)
                .await
                .and_then(|row| row.title),
            Some("copilot".to_string())
        );
        assert_eq!(
            reg.lookup_identity(&claude_identity)
                .await
                .and_then(|row| row.title),
            Some("claude".to_string())
        );
        assert_eq!(
            reg.lookup_identity(&ubuntu_identity)
                .await
                .and_then(|row| row.title),
            Some("ubuntu".to_string())
        );
    }

    #[tokio::test]
    async fn qualified_remove_and_notification_remove_only_the_exact_collision() {
        let reg = InMemoryRegistry::new();
        let host = qualified_info("same-id", "copilot", SessionLocation::Host, "host");
        let wsl = qualified_info(
            "same-id",
            "copilot",
            SessionLocation::Wsl {
                distro: "Debian".to_string(),
            },
            "wsl",
        );
        let host_identity = SessionIdentity::from_info(&host);
        let wsl_identity = SessionIdentity::from_info(&wsl);
        reg.upsert(host).await;
        reg.upsert(wsl).await;

        let ext = build_session_removed_notification(&host_identity);
        apply_ext_notification(&reg, &ext).await;

        assert!(reg.lookup_identity(&host_identity).await.is_none());
        assert!(reg.lookup_identity(&wsl_identity).await.is_some());
        assert_eq!(reg.snapshot().await.len(), 1);
    }

    #[tokio::test]
    async fn upsert_if_absent_preserves_existing_row_and_adds_new() {
        let reg = InMemoryRegistry::new();

        // A live row: bound pane + a non-historical status.
        let mut live = info("sess-1", Some("pane-A"));
        live.status = Some(AgentStatus::Idle);
        reg.upsert(live).await;

        // A history-scan row for the SAME id (Historical, no pane) — exactly
        // what an F5 rescan reloads from disk. It must NOT replace the live row.
        let mut historical = info("sess-1", None);
        historical.status = Some(AgentStatus::Historical);
        reg.upsert_if_absent(historical).await;

        let got = reg
            .lookup(&acp::schema::v1::SessionId::new("sess-1".to_string()))
            .await
            .expect("row present");
        assert_eq!(
            got.status,
            Some(AgentStatus::Idle),
            "rescan must not downgrade a live session to Historical"
        );
        assert_eq!(
            got.pane_session_id.as_deref(),
            Some("pane-A"),
            "rescan must not drop the live pane binding"
        );

        // A genuinely new session (absent from the registry) IS added.
        reg.upsert_if_absent(info("sess-2", None)).await;
        assert!(
            reg.lookup(&acp::schema::v1::SessionId::new("sess-2".to_string()))
                .await
                .is_some(),
            "a new disk session is surfaced"
        );
    }

    #[tokio::test]
    async fn remove_returns_prior_and_subsequent_lookup_is_none() {
        let reg = InMemoryRegistry::new();
        reg.upsert(info("sess-1", Some("pane-A"))).await;
        let removed = reg
            .remove(&acp::schema::v1::SessionId::new("sess-1".to_string()))
            .await
            .expect("entry removed");
        assert_eq!(removed.pane_session_id.as_deref(), Some("pane-A"));
        assert!(reg
            .lookup(&acp::schema::v1::SessionId::new("sess-1".to_string()))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn remove_miss_returns_none() {
        let reg = InMemoryRegistry::new();
        assert!(reg
            .remove(&acp::schema::v1::SessionId::new("nope".to_string()))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn remove_if_respects_predicate() {
        let reg = InMemoryRegistry::new();
        reg.upsert(info("keep", None)).await;
        reg.upsert(info("drop", None)).await;

        // Predicate rejects -> row stays, returns None.
        let kept = reg
            .remove_if(
                &acp::schema::v1::SessionId::new("keep".to_string()),
                &|_| false,
            )
            .await;
        assert!(
            kept.is_none(),
            "remove_if must not remove when predicate is false"
        );
        assert!(
            reg.lookup(&acp::schema::v1::SessionId::new("keep".to_string()))
                .await
                .is_some(),
            "rejected row must still be present"
        );

        // Predicate accepts -> row removed, returns the prior value.
        let dropped = reg
            .remove_if(
                &acp::schema::v1::SessionId::new("drop".to_string()),
                &|_| true,
            )
            .await;
        assert!(
            dropped.is_some(),
            "remove_if must remove when predicate is true"
        );
        assert!(
            reg.lookup(&acp::schema::v1::SessionId::new("drop".to_string()))
                .await
                .is_none(),
            "accepted row must be gone"
        );

        // Missing id -> None even with an always-true predicate.
        assert!(
            reg.remove_if(
                &acp::schema::v1::SessionId::new("missing".to_string()),
                &|_| true
            )
            .await
            .is_none(),
            "remove_if on an absent id returns None"
        );
    }

    #[tokio::test]
    async fn snapshot_contains_all_inserted_rows_in_any_order() {
        let reg = InMemoryRegistry::new();
        reg.upsert(info("a", Some("pa"))).await;
        reg.upsert(info("b", None)).await;
        reg.upsert(info("c", Some("pc"))).await;
        let mut snap = reg.snapshot().await;
        snap.sort_by(|l, r| l.session_id.0.cmp(&r.session_id.0));
        let ids: Vec<&str> = snap.iter().map(|s| &*s.session_id.0).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[tokio::test]
    async fn shared_constructor_returns_trait_object_that_works() {
        let reg: Arc<dyn SessionRegistry> = InMemoryRegistry::shared();
        reg.upsert(info("sess-1", None)).await;
        assert_eq!(reg.snapshot().await.len(), 1);
    }

    // ── apply_snapshot ──────────────────────────────────────────────

    #[tokio::test]
    async fn apply_snapshot_seeds_empty_registry() {
        let reg = InMemoryRegistry::new();
        let loaded = AtomicBool::new(false);
        apply_snapshot(&reg, &loaded, vec![info("a", Some("pa")), info("b", None)]).await;
        let mut snap = reg.snapshot().await;
        snap.sort_by(|l, r| l.session_id.0.cmp(&r.session_id.0));
        let ids: Vec<&str> = snap.iter().map(|s| &*s.session_id.0).collect();
        assert_eq!(ids, vec!["a", "b"]);
        assert!(loaded.load(Ordering::Acquire), "loaded flag flipped");
    }

    #[tokio::test]
    async fn apply_snapshot_drops_rows_absent_from_new_snapshot() {
        let reg = InMemoryRegistry::new();
        let loaded = AtomicBool::new(false);
        reg.upsert(info("stale", Some("pa"))).await;
        reg.upsert(info("keep", Some("pb"))).await;
        apply_snapshot(
            &reg,
            &loaded,
            vec![info("keep", Some("pb")), info("fresh", None)],
        )
        .await;
        let mut snap = reg.snapshot().await;
        snap.sort_by(|l, r| l.session_id.0.cmp(&r.session_id.0));
        let ids: Vec<&str> = snap.iter().map(|s| &*s.session_id.0).collect();
        assert_eq!(ids, vec!["fresh", "keep"], "stale row evicted");
    }

    #[tokio::test]
    async fn apply_snapshot_replaces_existing_row_contents() {
        let reg = InMemoryRegistry::new();
        let loaded = AtomicBool::new(false);
        reg.upsert(info("sess-1", Some("old-pane"))).await;
        apply_snapshot(&reg, &loaded, vec![info("sess-1", Some("new-pane"))]).await;
        let found = reg
            .lookup(&acp::schema::v1::SessionId::new("sess-1".to_string()))
            .await
            .unwrap();
        assert_eq!(found.pane_session_id.as_deref(), Some("new-pane"));
        assert_eq!(reg.snapshot().await.len(), 1, "no duplicates");
    }

    #[tokio::test]
    async fn apply_snapshot_with_empty_iter_clears_registry() {
        let reg = InMemoryRegistry::new();
        let loaded = AtomicBool::new(false);
        reg.upsert(info("a", None)).await;
        reg.upsert(info("b", None)).await;
        apply_snapshot(&reg, &loaded, std::iter::empty()).await;
        assert!(reg.snapshot().await.is_empty(), "registry cleared");
        assert!(
            loaded.load(Ordering::Acquire),
            "loaded still flips on empty snapshot"
        );
    }

    #[tokio::test]
    async fn apply_snapshot_is_idempotent() {
        let reg = InMemoryRegistry::new();
        let loaded = AtomicBool::new(false);
        let items = vec![info("a", Some("pa")), info("b", None)];
        apply_snapshot(&reg, &loaded, items.clone()).await;
        apply_snapshot(&reg, &loaded, items).await;
        assert_eq!(reg.snapshot().await.len(), 2, "second apply matches first");
    }

    // ── upgrade_title_if_synthetic ──────────────────────────────────

    fn info_with(id: &str, cwd: &str, title: Option<&str>) -> SessionInfo {
        let mut s = SessionInfo::new(
            acp::schema::v1::SessionId::new(id.to_string()),
            PathBuf::from(cwd),
        );
        s.title = title.map(str::to_owned);
        s
    }

    #[test]
    fn title_is_synthetic_detects_missing_empty_cwd_and_opencode_placeholder() {
        assert!(title_is_synthetic(&info_with("s-none", "/repo/proj", None)));
        assert!(title_is_synthetic(&info_with(
            "s-empty",
            "/repo/proj",
            Some("")
        )));
        assert!(title_is_synthetic(&info_with(
            "s-leaf",
            "/repo/proj",
            Some("proj")
        )));
        assert!(!title_is_synthetic(&info_with(
            "s-real",
            "/repo/proj",
            Some("Real Title")
        )));

        let mut opencode = info_with(
            "s-opencode",
            "/repo/proj",
            Some("New session - 2026-07-23T01:14:00.422Z"),
        );
        opencode.cli_source = Some(CliSource::OpenCode);
        assert!(title_is_synthetic(&opencode));

        opencode.cli_source = Some(CliSource::Copilot);
        assert!(
            !title_is_synthetic(&opencode),
            "provider-specific placeholders must not hide a real title from another CLI"
        );
    }

    #[test]
    fn title_is_injected_context_echo_detects_delegate_marker_only() {
        // Mirrors the `?<prompt>` prompt built in `cli/delegate.rs` from
        // `TERMINAL_CONTEXT_TITLE_MARKER`: an agent CLI can echo this whole first
        // user message back as a `session/list` title before it generates a real
        // summary. Such an echo must be dropped (see `titles_from_listing`), or
        // `adopt_agent_title` would overwrite the row's title with the injected
        // pane GUID and captured pane output on every poll until the summary
        // lands.
        let echo = format!(
            "hi test\n\n{}8A9B4ABA-BEB4-4F94-B0D3-55569420B902)\n```\nPowerShell 7.6.3\n```",
            TERMINAL_CONTEXT_TITLE_MARKER
        );
        assert!(title_is_injected_context_echo(&echo));
        // A real CLI-generated summary, the bare user prompt, and empty never
        // contain the injected marker.
        assert!(!title_is_injected_context_echo(
            "PowerShell Terminal Session"
        ));
        assert!(!title_is_injected_context_echo("hi test"));
        assert!(!title_is_injected_context_echo(""));
    }

    #[test]
    fn title_is_displayable_rejects_empty_echo_and_provider_placeholder() {
        assert!(title_is_displayable(
            Some(&CliSource::Copilot),
            "Check Copilot Resume Hooks"
        ));
        assert!(title_is_displayable(None, "Check Copilot Resume Hooks"));
        assert!(!title_is_displayable(Some(&CliSource::Copilot), ""));
        assert!(!title_is_displayable(
            Some(&CliSource::Copilot),
            &format!("hi\n\n{TERMINAL_CONTEXT_TITLE_MARKER}pane-1)\n```\nx\n```")
        ));
        // An unknown cli can't recognize a provider placeholder, but the echo
        // and empty checks still apply.
        assert!(!title_is_displayable(
            None,
            &format!("hi\n\n{TERMINAL_CONTEXT_TITLE_MARKER}pane-1)\n```\nx\n```")
        ));
        let placeholder = "New session - 2026-07-23T01:14:00.422Z";
        assert!(!title_is_displayable(
            Some(&CliSource::OpenCode),
            placeholder
        ));
        assert!(
            title_is_displayable(Some(&CliSource::Copilot), placeholder),
            "another CLI's placeholder shape is a real title here"
        );
    }

    #[tokio::test]
    async fn upgrade_title_replaces_none_title() {
        let reg = InMemoryRegistry::new();
        reg.upsert(info_with("s1", "/repo/proj", None)).await;
        let sid = acp::schema::v1::SessionId::new("s1".to_string());
        assert!(reg.upgrade_title_if_synthetic(&sid, "Real Title").await);
        assert_eq!(
            reg.lookup(&sid).await.unwrap().title.as_deref(),
            Some("Real Title")
        );
    }

    #[tokio::test]
    async fn upgrade_title_replaces_empty_title() {
        let reg = InMemoryRegistry::new();
        reg.upsert(info_with("s1", "/repo/proj", Some(""))).await;
        let sid = acp::schema::v1::SessionId::new("s1".to_string());
        assert!(reg.upgrade_title_if_synthetic(&sid, "Real Title").await);
        assert_eq!(
            reg.lookup(&sid).await.unwrap().title.as_deref(),
            Some("Real Title")
        );
    }

    #[tokio::test]
    async fn upgrade_title_replaces_cwd_basename_title() {
        // The exact bug from the helper logs: cwd=C:\Users\<user>,
        // title="<user>" (cwd basename). Helper-local upgrade ran but
        // never reached master; master keeps "<user>" forever. This
        // method is the atomic primitive that lets handle_session_hook
        // upgrade master's row when it observes the same condition.
        let reg = InMemoryRegistry::new();
        reg.upsert(info_with("s1", "C:\\Users\\alice", Some("alice")))
            .await;
        let sid = acp::schema::v1::SessionId::new("s1".to_string());
        assert!(
            reg.upgrade_title_if_synthetic(&sid, "No Coding Task Identified")
                .await
        );
        assert_eq!(
            reg.lookup(&sid).await.unwrap().title.as_deref(),
            Some("No Coding Task Identified")
        );
    }

    #[tokio::test]
    async fn upgrade_title_leaves_real_title_untouched() {
        let reg = InMemoryRegistry::new();
        reg.upsert(info_with("s1", "/repo/proj", Some("Real Existing Title")))
            .await;
        let sid = acp::schema::v1::SessionId::new("s1".to_string());
        // "proj" (cwd basename) ≠ "Real Existing Title", so the row is
        // NOT synthetic — even an attempted upgrade must not clobber it.
        assert!(
            !reg.upgrade_title_if_synthetic(&sid, "Different Title")
                .await
        );
        assert_eq!(
            reg.lookup(&sid).await.unwrap().title.as_deref(),
            Some("Real Existing Title")
        );
    }

    #[tokio::test]
    async fn upgrade_title_rejects_empty_candidate() {
        let reg = InMemoryRegistry::new();
        reg.upsert(info_with("s1", "/repo/proj", None)).await;
        let sid = acp::schema::v1::SessionId::new("s1".to_string());
        assert!(!reg.upgrade_title_if_synthetic(&sid, "").await);
        assert!(reg.lookup(&sid).await.unwrap().title.is_none());
    }

    #[tokio::test]
    async fn upgrade_title_idempotent_when_candidate_matches_existing() {
        let reg = InMemoryRegistry::new();
        reg.upsert(info_with("s1", "/repo/proj", Some("Same")))
            .await;
        let sid = acp::schema::v1::SessionId::new("s1".to_string());
        // Same != "proj" basename, so the row isn't synthetic — and
        // even if it were, returning false on no-op keeps the
        // broadcast budget low.
        assert!(!reg.upgrade_title_if_synthetic(&sid, "Same").await);
    }

    #[tokio::test]
    async fn upgrade_title_returns_false_for_missing_session() {
        let reg = InMemoryRegistry::new();
        let sid = acp::schema::v1::SessionId::new("nope".to_string());
        assert!(!reg.upgrade_title_if_synthetic(&sid, "Real Title").await);
    }

    #[tokio::test]
    async fn upgrade_title_preserves_other_fields() {
        // Regression guard for the rubber-duck concern: a naïve
        // lookup → clone → mutate title → upsert flow would clobber
        // status / pane_session_id / current_tool / last_activity_at
        // if another writer raced between lookup and upsert. The
        // atomic method must touch *only* `title`.
        let reg = InMemoryRegistry::new();
        let mut row = info_with("s1", "C:\\Users\\alice", Some("alice"));
        row.pane_session_id = Some("pane-abc".to_string());
        row.status = Some(AgentStatus::Working);
        row.cli_source = Some(CliSource::Copilot);
        row.current_tool = Some("write".to_string());
        row.last_activity_at_ms = Some(123_456_789);
        row.origin = Some(SessionOrigin::Unknown);
        reg.upsert(row.clone()).await;

        let sid = acp::schema::v1::SessionId::new("s1".to_string());
        assert!(reg.upgrade_title_if_synthetic(&sid, "Real Title").await);

        let found = reg.lookup(&sid).await.unwrap();
        assert_eq!(found.title.as_deref(), Some("Real Title"));
        // Everything else is preserved.
        assert_eq!(found.pane_session_id.as_deref(), Some("pane-abc"));
        assert_eq!(found.status, Some(AgentStatus::Working));
        assert_eq!(found.cli_source, Some(CliSource::Copilot));
        assert_eq!(found.current_tool.as_deref(), Some("write"));
        assert_eq!(found.last_activity_at_ms, Some(123_456_789));
        assert_eq!(found.origin, Some(SessionOrigin::Unknown));
        assert_eq!(found.cwd, PathBuf::from("C:\\Users\\alice"));
    }

    // ── adopt_agent_title ───────────────────────────────────────────

    #[tokio::test]
    async fn adopt_agent_title_replaces_a_stale_real_title() {
        // Copilot surfaces the first user message as a session's
        // `session/list` title until it generates a summary. That echo is
        // non-synthetic, so `upgrade_title_if_synthetic` can never correct it.
        let reg = InMemoryRegistry::new();
        reg.upsert(info_with(
            "s1",
            "/repo/proj",
            Some("first user message echoed as a title"),
        ))
        .await;
        let sid = acp::schema::v1::SessionId::new("s1".to_string());
        assert!(
            !reg.upgrade_title_if_synthetic(&sid, "Check Copilot Resume Hooks")
                .await
        );
        assert!(
            reg.adopt_agent_title(&sid, "Check Copilot Resume Hooks")
                .await
        );
        assert_eq!(
            reg.lookup(&sid).await.unwrap().title.as_deref(),
            Some("Check Copilot Resume Hooks")
        );
    }

    #[tokio::test]
    async fn adopt_agent_title_is_a_no_op_for_empty_same_or_missing() {
        let reg = InMemoryRegistry::new();
        reg.upsert(info_with("s1", "/repo/proj", Some("Same")))
            .await;
        let sid = acp::schema::v1::SessionId::new("s1".to_string());
        // Empty candidate must never blank out a title.
        assert!(!reg.adopt_agent_title(&sid, "").await);
        // Steady state returns false so the poll doesn't broadcast every tick.
        assert!(!reg.adopt_agent_title(&sid, "Same").await);
        assert_eq!(
            reg.lookup(&sid).await.unwrap().title.as_deref(),
            Some("Same")
        );
        assert!(
            !reg.adopt_agent_title(
                &acp::schema::v1::SessionId::new("nope".to_string()),
                "Real Title"
            )
            .await
        );
    }

    #[tokio::test]
    async fn adopt_agent_title_preserves_other_fields() {
        let reg = InMemoryRegistry::new();
        let mut row = info_with("s1", "/repo/proj", Some("old"));
        row.pane_session_id = Some("pane-abc".to_string());
        row.status = Some(AgentStatus::Working);
        row.cli_source = Some(CliSource::Copilot);
        row.last_activity_at_ms = Some(123_456_789);
        reg.upsert(row).await;

        let sid = acp::schema::v1::SessionId::new("s1".to_string());
        assert!(reg.adopt_agent_title(&sid, "new").await);

        let found = reg.lookup(&sid).await.unwrap();
        assert_eq!(found.title.as_deref(), Some("new"));
        assert_eq!(found.pane_session_id.as_deref(), Some("pane-abc"));
        assert_eq!(found.status, Some(AgentStatus::Working));
        assert_eq!(found.cli_source, Some(CliSource::Copilot));
        assert_eq!(found.last_activity_at_ms, Some(123_456_789));
    }

    // ── _meta.wta extract / inject ──────────────────────────────────

    fn meta_with(json: serde_json::Value) -> Option<acp::schema::v1::Meta> {
        match json {
            serde_json::Value::Object(map) => Some(map),
            _ => panic!("test bug: meta_with expects a JSON object"),
        }
    }

    #[test]
    fn extract_returns_default_when_meta_is_none() {
        let mut meta: Option<acp::schema::v1::Meta> = None;
        let wta = extract_wta_meta(&mut meta);
        assert_eq!(wta, WtaMeta::default());
        assert!(meta.is_none(), "meta unchanged");
    }

    #[test]
    fn extract_returns_default_when_wta_key_absent() {
        let mut meta = meta_with(serde_json::json!({ "other": "keep-me" }));
        let wta = extract_wta_meta(&mut meta);
        assert_eq!(wta, WtaMeta::default());
        // Other vendors' meta must survive untouched.
        assert_eq!(
            meta.as_ref().and_then(|m| m.get("other")),
            Some(&serde_json::Value::String("keep-me".to_string()))
        );
    }

    #[test]
    fn extract_pulls_pane_session_id_and_removes_wta_key() {
        let mut meta = meta_with(serde_json::json!({
            "wta": { "pane_session_id": "pane-A" },
            "other": "keep-me",
        }));
        let wta = extract_wta_meta(&mut meta);
        assert_eq!(wta.pane_session_id.as_deref(), Some("pane-A"));
        let leftover = meta.expect("`other` survives");
        assert!(!leftover.contains_key("wta"), "wta key stripped");
        assert!(leftover.contains_key("other"), "other key preserved");
    }

    #[test]
    fn extract_collapses_meta_to_none_when_wta_was_only_key() {
        let mut meta = meta_with(serde_json::json!({
            "wta": { "pane_session_id": "pane-A" },
        }));
        let wta = extract_wta_meta(&mut meta);
        assert_eq!(wta.pane_session_id.as_deref(), Some("pane-A"));
        assert!(
            meta.is_none(),
            "downstream agents must not see an empty _meta object"
        );
    }

    #[test]
    fn extract_tolerates_non_object_wta_value() {
        // Malformed wire data: `_meta.wta` is a string instead of an
        // object. We should not panic; just treat it as "no extension
        // data" while still stripping the bad key so we don't forward
        // it to the agent.
        let mut meta = meta_with(serde_json::json!({
            "wta": "not-an-object",
        }));
        let wta = extract_wta_meta(&mut meta);
        assert_eq!(wta, WtaMeta::default());
        assert!(meta.is_none(), "bad wta key still stripped");
    }

    #[test]
    fn inject_is_noop_when_wta_is_empty() {
        let mut meta: Option<acp::schema::v1::Meta> = None;
        inject_wta_meta(&mut meta, &WtaMeta::default());
        assert!(meta.is_none(), "no spurious _meta created");
    }

    #[test]
    fn to_acp_session_info_carries_pane_session_id_in_meta() {
        let mut row = SessionInfo::new(
            acp::schema::v1::SessionId::new("sess-1".to_string()),
            PathBuf::from("/repo/a"),
        );
        row.title = Some("hello".into());
        row.updated_at = Some("2025-01-01T00:00:00Z".into());
        row.pane_session_id = Some("pane-X".into());
        let acp = to_acp_session_info(&row);
        assert_eq!(acp.session_id, row.session_id);
        assert_eq!(acp.cwd, row.cwd);
        assert_eq!(acp.title.as_deref(), Some("hello"));
        assert_eq!(acp.updated_at.as_deref(), Some("2025-01-01T00:00:00Z"));
        let mut meta = acp.meta.clone();
        let wta = extract_wta_meta(&mut meta);
        assert_eq!(wta.pane_session_id.as_deref(), Some("pane-X"));
    }

    #[test]
    fn to_acp_session_info_omits_meta_when_no_pane_session_id() {
        let row = SessionInfo::new(
            acp::schema::v1::SessionId::new("sess-1".to_string()),
            PathBuf::from("/repo/a"),
        );
        let acp = to_acp_session_info(&row);
        assert!(
            acp.meta.is_none(),
            "no _meta when there's nothing to communicate"
        );
    }

    // ---------------- ExtNotification round-trips ----------------

    #[test]
    fn build_then_parse_session_added_is_round_trip() {
        let mut row = SessionInfo::new(
            acp::schema::v1::SessionId::new("sess-77".to_string()),
            PathBuf::from("/repo/x"),
        );
        row.title = Some("hello".into());
        row.updated_at = Some("2025-01-02T03:04:05Z".into());
        row.pane_session_id = Some("pane-ZZ".into());
        let ext = build_session_added_notification(&row);
        assert_eq!(&*ext.method, INTELLTERM_METHOD_SESSION_ADDED);
        match parse_ext_notification(&ext) {
            WtaExtNotification::SessionAdded(parsed) => assert_eq!(parsed, row),
            other => panic!("expected SessionAdded, got {other:?}"),
        }
    }

    #[test]
    fn build_session_added_with_no_pane_session_id_still_round_trips() {
        let row = SessionInfo::new(
            acp::schema::v1::SessionId::new("sess-99".to_string()),
            PathBuf::from("/repo/y"),
        );
        let ext = build_session_added_notification(&row);
        match parse_ext_notification(&ext) {
            WtaExtNotification::SessionAdded(parsed) => {
                assert_eq!(parsed, row);
                assert!(parsed.pane_session_id.is_none());
            }
            other => panic!("expected SessionAdded, got {other:?}"),
        }
    }

    #[test]
    fn build_then_parse_session_removed_is_round_trip() {
        let sid = acp::schema::v1::SessionId::new("sess-dead".to_string());
        let ext = build_session_removed_notification(&sid);
        assert_eq!(&*ext.method, INTELLTERM_METHOD_SESSION_REMOVED);
        match parse_ext_notification(&ext) {
            WtaExtNotification::SessionRemoved(parsed) => {
                assert_eq!(parsed.session_id, sid);
                assert!(parsed.history_key.is_none());
            }
            other => panic!("expected SessionRemoved, got {other:?}"),
        }
    }

    #[test]
    fn parse_unknown_method_returns_unknown() {
        let raw = serde_json::value::RawValue::from_string("{}".into()).unwrap();
        let ext = acp::schema::v1::ExtNotification::new("somebody.else/event", Arc::from(raw));
        assert!(matches!(
            parse_ext_notification(&ext),
            WtaExtNotification::Unknown
        ));
    }

    #[test]
    fn parse_session_added_with_garbage_params_is_malformed_not_panic() {
        let raw =
            serde_json::value::RawValue::from_string(r#"{"not":"a session"}"#.into()).unwrap();
        let ext =
            acp::schema::v1::ExtNotification::new(INTELLTERM_METHOD_SESSION_ADDED, Arc::from(raw));
        assert!(matches!(
            parse_ext_notification(&ext),
            WtaExtNotification::MalformedParams { .. }
        ));
    }

    // ─── Task A: expanded SessionInfo + sessions/list + reducers ───────────

    #[test]
    fn session_info_json_round_trips_all_master_fields() {
        let row = SessionInfo {
            session_id: acp::schema::v1::SessionId::new("sess-full".to_string()),
            provider_id: None,
            session_universe: None,
            cwd: PathBuf::from("C:\\repo"),
            title: Some("fix the build".into()),
            updated_at: Some("2026-05-27T12:34:56Z".into()),
            pane_session_id: Some("pane-1".into()),
            owner_window_id: Some(42),
            background_tab: Some(false),
            status: Some(crate::agent_sessions::AgentStatus::Attention),
            cli_source: Some(crate::agent_sessions::CliSource::Copilot),
            current_tool: Some("ask_user".into()),
            attention_reason: Some("Need approval".into()),
            attention_timestamp_ms: None,
            attention_cooldown_until_ms: None,
            last_activity_at_ms: Some(1717012345678),
            origin: Some(crate::agent_sessions::SessionOrigin::AgentPane),
            last_error: Some("previous failure".into()),
            location: crate::agent_sessions::SessionLocation::Host,
            bound_pid: None,
            born_bound_pane: false,
            cwd_evidence: CwdEvidence::default(),
        };

        let json = serde_json::to_string(&row).expect("serialize SessionInfo");
        let parsed: SessionInfo = serde_json::from_str(&json).expect("deserialize SessionInfo");

        assert_eq!(parsed, row);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["status"], "Attention");
        assert_eq!(value["cli_source"], "Copilot");
        assert_eq!(value["origin"], "AgentPane");
        assert_eq!(value["last_activity_at_ms"], 1717012345678u64);
        assert_eq!(value["owner_window_id"], 42u64);
        assert_eq!(value["background_tab"], false);
        let mut legacy = value;
        legacy.as_object_mut().unwrap().remove("owner_window_id");
        legacy.as_object_mut().unwrap().remove("background_tab");
        let parsed: SessionInfo = serde_json::from_value(legacy).unwrap();
        assert_eq!(parsed.owner_window_id, None);
        assert_eq!(parsed.background_tab, None);
        for background in [Some(true), Some(false), None] {
            let mut row = parsed.clone();
            row.background_tab = background;
            let value = serde_json::to_value(&row).unwrap();
            assert_eq!(value.get("background_tab").is_some(), background.is_some());
            let round_trip: SessionInfo = serde_json::from_value(value).unwrap();
            assert_eq!(round_trip.background_tab, background);
        }
    }

    #[test]
    fn history_row_key_qualifies_provider_location_and_raw_session_id() {
        let key = HistoryRowKey::new(
            " CoPilot ",
            SessionLocation::Wsl {
                distro: "Ubuntu".to_string(),
            },
            "raw-session-id",
            Some("tenant-a".to_string()),
        )
        .expect("qualified identity");

        assert_eq!(key.provider_id, "copilot");
        assert_eq!(key.session_id, "raw-session-id");
        assert_eq!(key.session_universe.as_deref(), Some("tenant-a"));
    }

    #[test]
    fn history_row_key_rejects_missing_provenance() {
        assert!(HistoryRowKey::new("copilot", SessionLocation::Unknown, "sid", None).is_none());
        assert!(HistoryRowKey::new("", SessionLocation::Host, "sid", None).is_none());
        assert!(HistoryRowKey::new("copilot", SessionLocation::Host, "", None).is_none());
    }

    #[test]
    fn build_sessions_list_request_round_trips_rescan() {
        let req = build_sessions_list_request(false);
        assert_eq!(&*req.method, INTELLTERM_METHOD_SESSIONS_LIST);
        assert!(
            !parse_sessions_list_params(&req.params)
                .expect("params are valid")
                .rescan
        );

        let req_rescan = build_sessions_list_request(true);
        assert!(
            parse_sessions_list_params(&req_rescan.params)
                .expect("params are valid")
                .rescan
        );

        // Backward compat: a legacy empty `{}` params object (older helper /
        // master that predates the flag) deserializes as rescan=false.
        let empty = serde_json::value::RawValue::from_string("{}".to_string()).unwrap();
        assert!(
            !parse_sessions_list_params(&empty)
                .expect("empty is valid")
                .rescan
        );
    }

    #[test]
    fn session_activate_request_round_trips_qualified_identity() {
        let identity = SessionIdentity {
            session_id: acp::schema::v1::SessionId::new("same-raw-id"),
            history_key: HistoryRowKey::new(
                "copilot",
                SessionLocation::Wsl {
                    distro: "Ubuntu".to_string(),
                },
                "same-raw-id",
                Some("tenant-a".to_string()),
            ),
        };
        let request =
            build_session_activate_request(identity.clone(), 42, "activation-1".to_string(), false);
        assert_eq!(&*request.method, INTELLTERM_METHOD_SESSION_ACTIVATE);
        let parsed = parse_session_activate_params(&request.params).expect("params parse");
        assert_eq!(parsed.identity, identity);
        assert_eq!(parsed.window_id, 42);
        assert_eq!(parsed.activation_id, "activation-1");

        let request =
            build_session_activate_request(identity, 42, "activation-1".to_string(), true);
        assert_eq!(
            &*request.method,
            INTELLTERM_METHOD_SESSION_ACTIVATION_STATUS
        );
        assert_eq!(
            parse_session_activate_params(&request.params).unwrap(),
            parsed
        );
        assert!(matches!(
            parse_ext_request(request),
            WtaExtRequest::SessionActivationStatus(_)
        ));
    }

    #[test]
    fn session_activation_response_distinguishes_pending_complete_and_unknown() {
        for state in [
            SessionActivationState::Pending,
            SessionActivationState::Complete,
            SessionActivationState::Unknown,
        ] {
            let response = SessionActivateResponse {
                activation_id: "activation-1".to_string(),
                state,
                action: String::new(),
                accepted: false,
                detail: None,
            };
            let raw = serde_json::value::to_raw_value(&response).unwrap();
            assert_eq!(parse_session_activate_response(&raw).unwrap(), response);
        }
        let legacy = serde_json::value::RawValue::from_string(
            r#"{"activation_id":"legacy","action":"focus","accepted":true}"#.to_string(),
        )
        .unwrap();
        assert_eq!(
            parse_session_activate_response(&legacy).unwrap().state,
            SessionActivationState::Complete
        );
    }

    #[test]
    fn build_sessions_changed_notification_has_empty_params() {
        let ext = build_sessions_changed_notification();
        assert_eq!(&*ext.method, INTELLTERM_METHOD_SESSIONS_CHANGED);
        assert_eq!(ext.params.get(), "{}");
        assert!(matches!(
            parse_ext_notification(&ext),
            WtaExtNotification::SessionsChanged
        ));
    }

    #[test]
    fn sessions_list_response_round_trips_rows() {
        let row = SessionInfo {
            session_id: acp::schema::v1::SessionId::new("sess-list".to_string()),
            provider_id: None,
            session_universe: None,
            cwd: PathBuf::from("C:\\repo"),
            title: Some("title".into()),
            updated_at: Some("2026-05-27T12:34:56Z".into()),
            pane_session_id: Some("pane-list".into()),
            owner_window_id: Some(42),
            background_tab: Some(false),
            status: Some(crate::agent_sessions::AgentStatus::Idle),
            cli_source: Some(crate::agent_sessions::CliSource::Claude),
            current_tool: None,
            attention_reason: None,
            attention_timestamp_ms: None,
            attention_cooldown_until_ms: None,
            last_activity_at_ms: Some(123),
            origin: Some(crate::agent_sessions::SessionOrigin::AgentPane),
            last_error: None,
            location: crate::agent_sessions::SessionLocation::Host,
            bound_pid: None,
            born_bound_pane: false,
            cwd_evidence: CwdEvidence::default(),
        };
        let raw = build_sessions_list_response(
            vec![row.clone()],
            None,
            None,
            Vec::new(),
            None,
            None,
            Vec::new(),
        );
        let parsed = parse_sessions_list_response(&raw).expect("response parses");
        assert_eq!(parsed.sessions, vec![row]);
    }

    #[tokio::test]
    async fn master_reducer_session_started_creates_idle_entry_bound_to_pane() {
        let reg = InMemoryRegistry::new();
        let changed = reg
            .apply_event(crate::agent_sessions::SessionEvent::SessionStarted {
                key: "sid-1".into(),
                cli_source: crate::agent_sessions::CliSource::Claude,
                pane_session_id: "Pane-A".into(),
                cwd: PathBuf::from("C:\\work"),
                title: "claude — work".into(),
            })
            .await;

        assert!(changed);
        let row = reg
            .lookup(&acp::schema::v1::SessionId::new("sid-1"))
            .await
            .unwrap();
        assert_eq!(row.status, Some(crate::agent_sessions::AgentStatus::Idle));
        assert_eq!(
            row.cli_source,
            Some(crate::agent_sessions::CliSource::Claude)
        );
        assert_eq!(row.pane_session_id.as_deref(), Some("pane-a"));
        assert_eq!(row.title.as_deref(), Some("claude — work"));
    }

    #[tokio::test]
    async fn master_reducer_tool_lifecycle_and_notification_update_activity_fields() {
        let reg = InMemoryRegistry::new();
        reg.apply_event(crate::agent_sessions::SessionEvent::SessionStarted {
            key: "sid".into(),
            cli_source: crate::agent_sessions::CliSource::Copilot,
            pane_session_id: "p".into(),
            cwd: PathBuf::from("C:\\x"),
            title: "t".into(),
        })
        .await;
        reg.apply_event(crate::agent_sessions::SessionEvent::ToolStarting {
            key: "sid".into(),
            tool_name: "bash".into(),
        })
        .await;
        let row = reg
            .lookup(&acp::schema::v1::SessionId::new("sid"))
            .await
            .unwrap();
        assert_eq!(
            row.status,
            Some(crate::agent_sessions::AgentStatus::Working)
        );
        assert_eq!(row.current_tool.as_deref(), Some("bash"));

        reg.apply_event(crate::agent_sessions::SessionEvent::Notification {
            key: "sid".into(),
            message: "approve?".into(),
            timestamp_ms: None,
        })
        .await;
        let row = reg
            .lookup(&acp::schema::v1::SessionId::new("sid"))
            .await
            .unwrap();
        assert_eq!(
            row.status,
            Some(crate::agent_sessions::AgentStatus::Attention)
        );
        assert_eq!(row.attention_reason.as_deref(), Some("approve?"));

        reg.apply_event(crate::agent_sessions::SessionEvent::ToolCompleted { key: "sid".into() })
            .await;
        let row = reg
            .lookup(&acp::schema::v1::SessionId::new("sid"))
            .await
            .unwrap();
        assert_eq!(row.status, Some(crate::agent_sessions::AgentStatus::Idle));
        assert!(row.current_tool.is_none());
        assert!(row.attention_reason.is_none());
    }

    #[tokio::test]
    async fn master_reducer_session_started_preserves_live_working_status() {
        // Prompt-before-start ordering bug, authoritative reducer: Copilot fires
        // `prompt.submit` (→ Working) ~2 s BEFORE its `session.start` for the
        // same session id. A late SessionStarted must NOT reset the row to Idle —
        // this is the reducer behind `sessions/list`, so the clobber is exactly
        // what the user saw on screen.
        use crate::agent_sessions::{AgentStatus, CliSource, SessionEvent};
        let reg = InMemoryRegistry::new();
        // prompt.submit path: synthetic SessionStarted, then ToolStarting.
        reg.apply_event(SessionEvent::SessionStarted {
            key: "sid".into(),
            cli_source: CliSource::Copilot,
            pane_session_id: "p".into(),
            cwd: PathBuf::from("C:\\x"),
            title: "t".into(),
        })
        .await;
        reg.apply_event(SessionEvent::ToolStarting {
            key: "sid".into(),
            tool_name: "prompt".into(),
        })
        .await;
        assert_eq!(
            reg.lookup(&acp::schema::v1::SessionId::new("sid"))
                .await
                .unwrap()
                .status,
            Some(AgentStatus::Working),
        );

        // The real session.start arrives later for the SAME key.
        reg.apply_event(SessionEvent::SessionStarted {
            key: "sid".into(),
            cli_source: CliSource::Copilot,
            pane_session_id: "p".into(),
            cwd: PathBuf::from("C:\\x"),
            title: "t".into(),
        })
        .await;
        assert_eq!(
            reg.lookup(&acp::schema::v1::SessionId::new("sid"))
                .await
                .unwrap()
                .status,
            Some(AgentStatus::Working),
            "a late SessionStarted must preserve the live Working status",
        );
    }

    #[tokio::test]
    async fn master_reducer_session_started_revives_ended_row_to_idle() {
        // Counterpart: a SessionStarted for a row in a terminal state (Ended)
        // must still re-baseline it to Idle (resume / reconnect).
        use crate::agent_sessions::{AgentStatus, CliSource, SessionEvent};
        let reg = InMemoryRegistry::new();
        reg.apply_event(SessionEvent::SessionStarted {
            key: "sid".into(),
            cli_source: CliSource::Copilot,
            pane_session_id: "p".into(),
            cwd: PathBuf::from("C:\\x"),
            title: "t".into(),
        })
        .await;
        reg.apply_event(SessionEvent::PaneClosed {
            pane_session_id: "p".into(),
        })
        .await;
        assert_eq!(
            reg.lookup(&acp::schema::v1::SessionId::new("sid"))
                .await
                .unwrap()
                .status,
            Some(AgentStatus::Ended),
        );
        reg.apply_event(SessionEvent::SessionStarted {
            key: "sid".into(),
            cli_source: CliSource::Copilot,
            pane_session_id: "p2".into(),
            cwd: PathBuf::from("C:\\x"),
            title: "t".into(),
        })
        .await;
        assert_eq!(
            reg.lookup(&acp::schema::v1::SessionId::new("sid"))
                .await
                .unwrap()
                .status,
            Some(AgentStatus::Idle),
            "a SessionStarted reviving an Ended row must reset it to Idle",
        );
    }

    #[tokio::test]
    async fn master_reducer_session_stopped_and_pane_closed_end_sessions() {
        let reg = InMemoryRegistry::new();
        reg.apply_event(crate::agent_sessions::SessionEvent::SessionStarted {
            key: "sid".into(),
            cli_source: crate::agent_sessions::CliSource::Gemini,
            pane_session_id: "p".into(),
            cwd: PathBuf::from("C:\\x"),
            title: "t".into(),
        })
        .await;
        reg.set_origin(
            &acp::schema::v1::SessionId::new("sid"),
            crate::agent_sessions::SessionOrigin::AgentPane,
        )
        .await;
        reg.apply_event(crate::agent_sessions::SessionEvent::SessionStopped {
            key: "sid".into(),
            reason: "user_exit".into(),
        })
        .await;
        let row = reg
            .lookup(&acp::schema::v1::SessionId::new("sid"))
            .await
            .unwrap();
        assert_eq!(row.status, Some(crate::agent_sessions::AgentStatus::Ended));
        assert!(row.pane_session_id.is_none());

        reg.apply_event(crate::agent_sessions::SessionEvent::SessionStarted {
            key: "sid2".into(),
            cli_source: crate::agent_sessions::CliSource::Gemini,
            pane_session_id: "p2".into(),
            cwd: PathBuf::from("C:\\x"),
            title: "t".into(),
        })
        .await;
        reg.apply_event(crate::agent_sessions::SessionEvent::PaneClosed {
            pane_session_id: "P2".into(),
        })
        .await;
        let row = reg
            .lookup(&acp::schema::v1::SessionId::new("sid2"))
            .await
            .unwrap();
        assert_eq!(row.status, Some(crate::agent_sessions::AgentStatus::Ended));
        assert!(row.pane_session_id.is_none());
    }

    #[tokio::test]
    async fn master_reducer_connection_failed_sets_error_on_bound_session() {
        let reg = InMemoryRegistry::new();
        reg.apply_event(crate::agent_sessions::SessionEvent::SessionStarted {
            key: "sid".into(),
            cli_source: crate::agent_sessions::CliSource::Claude,
            pane_session_id: "p".into(),
            cwd: PathBuf::from("C:\\x"),
            title: "t".into(),
        })
        .await;
        reg.apply_event(crate::agent_sessions::SessionEvent::ConnectionFailed {
            pane_session_id: "P".into(),
            reason: "ECONNRESET".into(),
        })
        .await;
        let row = reg
            .lookup(&acp::schema::v1::SessionId::new("sid"))
            .await
            .unwrap();
        assert_eq!(row.status, Some(crate::agent_sessions::AgentStatus::Error));
        assert_eq!(row.last_error.as_deref(), Some("ECONNRESET"));
        assert_eq!(row.pane_session_id.as_deref(), Some("p"));
    }

    /// Master's counterpart to the helper-side idempotency test. Every helper
    /// publishes its own copy of the same WT `connection_state: failed`, and
    /// `apply_event`'s return value gates the `sessions/changed` broadcast — so
    /// a repeat that reports `true` would push one notification per helper for
    /// a row that already says exactly this.
    #[tokio::test]
    async fn master_reducer_repeated_connection_failure_reports_no_change() {
        let reg = InMemoryRegistry::new();
        reg.apply_event(crate::agent_sessions::SessionEvent::SessionStarted {
            key: "sid".into(),
            cli_source: crate::agent_sessions::CliSource::Claude,
            pane_session_id: "p".into(),
            cwd: PathBuf::from("C:\\x"),
            title: "t".into(),
        })
        .await;

        let first = reg
            .apply_event(crate::agent_sessions::SessionEvent::ConnectionFailed {
                pane_session_id: "p".into(),
                reason: "ECONNRESET".into(),
            })
            .await;
        assert!(first, "the first failure must land and broadcast");

        let repeat = reg
            .apply_event(crate::agent_sessions::SessionEvent::ConnectionFailed {
                pane_session_id: "p".into(),
                reason: "ECONNRESET".into(),
            })
            .await;
        assert!(
            !repeat,
            "a sibling helper's identical copy must not re-broadcast"
        );

        let changed = reg
            .apply_event(crate::agent_sessions::SessionEvent::ConnectionFailed {
                pane_session_id: "p".into(),
                reason: "pipe closed".into(),
            })
            .await;
        assert!(changed, "a different reason is new information");
        let row = reg
            .lookup(&acp::schema::v1::SessionId::new("sid"))
            .await
            .unwrap();
        assert_eq!(row.last_error.as_deref(), Some("pipe closed"));
    }

    #[tokio::test]
    async fn master_reducer_resume_dispatched_promotes_ended_without_binding_pane() {
        let reg = InMemoryRegistry::new();
        reg.apply_event(crate::agent_sessions::SessionEvent::SessionStarted {
            key: "sid".into(),
            cli_source: crate::agent_sessions::CliSource::Gemini,
            pane_session_id: "p".into(),
            cwd: PathBuf::from("C:\\x"),
            title: "t".into(),
        })
        .await;
        reg.apply_event(crate::agent_sessions::SessionEvent::PaneClosed {
            pane_session_id: "p".into(),
        })
        .await;

        let changed = reg
            .apply_event(crate::agent_sessions::SessionEvent::ResumeDispatched {
                key: "sid".into(),
            })
            .await;
        let row = reg
            .lookup(&acp::schema::v1::SessionId::new("sid"))
            .await
            .unwrap();

        assert!(changed);
        assert_eq!(row.status, Some(crate::agent_sessions::AgentStatus::Idle));
        assert!(row.pane_session_id.is_none());
    }

    #[tokio::test]
    async fn master_reducer_resume_pane_assigned_binds_new_pane() {
        let reg = InMemoryRegistry::new();
        reg.upsert(SessionInfo {
            session_id: acp::schema::v1::SessionId::new("sid".to_string()),
            provider_id: None,
            session_universe: None,
            cwd: PathBuf::from("C:\\x"),
            title: Some("historical".into()),
            updated_at: None,
            pane_session_id: None,
            status: Some(crate::agent_sessions::AgentStatus::Historical),
            owner_window_id: None,
            background_tab: None,
            cli_source: Some(crate::agent_sessions::CliSource::Gemini),
            current_tool: None,
            attention_reason: None,
            attention_timestamp_ms: None,
            attention_cooldown_until_ms: None,
            last_activity_at_ms: Some(1),
            origin: Some(crate::agent_sessions::SessionOrigin::AgentPane),
            last_error: None,
            location: crate::agent_sessions::SessionLocation::Host,
            bound_pid: None,
            born_bound_pane: false,
            cwd_evidence: CwdEvidence::default(),
        })
        .await;
        reg.apply_event(crate::agent_sessions::SessionEvent::ResumeDispatched {
            key: "sid".into(),
        })
        .await;

        let changed = reg
            .apply_event(crate::agent_sessions::SessionEvent::ResumePaneAssigned {
                key: "sid".into(),
                pane_session_id: "New-Pane".into(),
            })
            .await;
        let row = reg
            .lookup(&acp::schema::v1::SessionId::new("sid"))
            .await
            .unwrap();

        assert!(changed);
        assert_eq!(row.status, Some(crate::agent_sessions::AgentStatus::Idle));
        assert_eq!(row.pane_session_id.as_deref(), Some("new-pane"));
    }

    /// Seed a `/sessions` resume: a historical row promoted to Idle and bound
    /// to a freshly-spawned pane by WTA itself, before the CLI has started.
    async fn seed_resumed_pane(reg: &InMemoryRegistry, key: &str, pane: &str) {
        let mut info = SessionInfo::new(
            acp::schema::v1::SessionId::new(key.to_string()),
            PathBuf::from("C:\\x"),
        );
        info.status = Some(crate::agent_sessions::AgentStatus::Historical);
        info.cli_source = Some(crate::agent_sessions::CliSource::Copilot);
        reg.upsert(info).await;
        reg.apply_event(crate::agent_sessions::SessionEvent::ResumeDispatched { key: key.into() })
            .await;
        reg.apply_event(crate::agent_sessions::SessionEvent::ResumePaneAssigned {
            key: key.into(),
            pane_session_id: pane.into(),
        })
        .await;
    }

    #[tokio::test]
    async fn resume_startup_failure_is_historical_before_or_after_pane_assignment() {
        for failure_first in [false, true] {
            let reg = InMemoryRegistry::new();
            let failure = SessionEvent::ConnectionFailed {
                pane_session_id: "RESUME-PANE".into(),
                reason: "The system cannot find the path specified.".into(),
            };
            if failure_first {
                reg.record_pane_connection_failure(
                    "RESUME-PANE",
                    "The system cannot find the path specified.",
                )
                .await;
                assert!(!reg.apply_event(failure.clone()).await);
            }
            let sid = acp::schema::v1::SessionId::new("resumed");
            let mut history = SessionInfo::new(sid.clone(), "C:\\deleted-worktree".into());
            history.status = Some(AgentStatus::Historical);
            history.cli_source = Some(CliSource::Copilot);
            history.pane_session_id = Some("stale-pane".into());
            reg.upsert(history).await;
            reg.mark_resume_dispatched(&sid).await.unwrap();
            assert!(reg.lookup_active_by_pane("stale-pane").await.is_none());
            reg.apply_event(SessionEvent::ResumePaneAssigned {
                key: "resumed".into(),
                pane_session_id: "resume-pane".into(),
            })
            .await;
            if !failure_first {
                assert!(reg.apply_event(failure.clone()).await);
                reg.record_pane_connection_failure(
                    "RESUME-PANE",
                    "The system cannot find the path specified.",
                )
                .await;
                assert!(reg.apply_event(failure.clone()).await);
            }
            let row = reg.lookup(&sid).await.unwrap();
            assert_eq!(row.status, Some(AgentStatus::Historical));
            assert!(row.pane_session_id.is_none());
            assert!(!row.born_bound_pane);
            assert_eq!(
                row.last_error.as_deref(),
                Some("The system cannot find the path specified.")
            );
            assert!(reg.lookup_active_by_pane("resume-pane").await.is_none());

            assert!(!reg.apply_event(failure).await);
            assert!(
                !reg.apply_event(SessionEvent::ResumePaneAssigned {
                    key: "resumed".into(),
                    pane_session_id: "resume-pane".into(),
                })
                .await
            );
            assert!(
                !reg.apply_event(SessionEvent::PaneClosed {
                    pane_session_id: "resume-pane".into(),
                })
                .await
            );
            assert_eq!(reg.lookup(&sid).await.unwrap(), row);

            reg.mark_resume_dispatched(&sid).await.unwrap();
            assert!(
                reg.apply_event(SessionEvent::ResumePaneAssigned {
                    key: "resumed".into(),
                    pane_session_id: "retry-pane".into(),
                })
                .await
            );
            let retried = reg.lookup(&sid).await.unwrap();
            assert_eq!(retried.status, Some(AgentStatus::Idle));
            assert_eq!(retried.pane_session_id.as_deref(), Some("retry-pane"));
            assert!(
                !reg.apply_event(SessionEvent::ResumePaneAssigned {
                    key: "resumed".into(),
                    pane_session_id: "resume-pane".into(),
                })
                .await
            );
            assert_eq!(reg.lookup(&sid).await.unwrap(), retried);

            reg.apply_event(SessionEvent::SessionStarted {
                key: "resumed".into(),
                cli_source: CliSource::Copilot,
                pane_session_id: "retry-pane".into(),
                cwd: "C:\\repo".into(),
                title: "Resumed session".into(),
            })
            .await;
            let started = reg.lookup(&sid).await.unwrap();
            assert!(
                !reg.apply_event(SessionEvent::ResumePaneAssigned {
                    key: "resumed".into(),
                    pane_session_id: "resume-pane".into(),
                })
                .await
            );
            assert_eq!(reg.lookup(&sid).await.unwrap(), started);
        }
    }

    #[tokio::test]
    async fn connected_resume_failure_remains_error_before_or_after_pane_assignment() {
        for failure_first in [false, true] {
            let reg = InMemoryRegistry::new();
            reg.record_pane_connected("RESUME-PANE").await;
            let failure = SessionEvent::ConnectionFailed {
                pane_session_id: "resume-pane".into(),
                reason: "process exited with code 1".into(),
            };
            if failure_first {
                reg.record_pane_connection_failure("resume-pane", "process exited with code 1")
                    .await;
                assert!(!reg.apply_event(failure.clone()).await);
            }
            seed_resumed_pane(&reg, "resumed", "resume-pane").await;
            if !failure_first {
                reg.record_pane_connection_failure("resume-pane", "process exited with code 1")
                    .await;
                assert!(reg.apply_event(failure).await);
            }
            let sid = acp::schema::v1::SessionId::new("resumed");
            let row = reg.lookup(&sid).await.unwrap();
            assert_eq!(row.status, Some(AgentStatus::Error));
            assert_eq!(row.pane_session_id.as_deref(), Some("resume-pane"));
            assert_eq!(
                row.last_error.as_deref(),
                Some("process exited with code 1")
            );
            assert!(
                reg.apply_event(SessionEvent::PaneClosed {
                    pane_session_id: "resume-pane".into(),
                })
                .await
            );
            assert_eq!(
                reg.lookup(&sid).await.unwrap().status,
                Some(AgentStatus::Ended)
            );
        }
    }

    /// Regression: Copilot's `--resume` boots a throwaway bootstrap session and
    /// only switches to the requested one seconds later, so the SessionStart
    /// hook reports the bootstrap id against the resumed pane's GUID. That must
    /// not evict the resumed row — doing so ended it permanently, because
    /// terminal-state rows refuse resurrection.
    #[tokio::test]
    async fn master_reducer_foreign_session_start_cannot_steal_a_resumed_pane() {
        let reg = InMemoryRegistry::new();
        seed_resumed_pane(&reg, "resumed", "pane-1").await;

        reg.apply_event(crate::agent_sessions::SessionEvent::SessionStarted {
            key: "bootstrap".into(),
            cli_source: crate::agent_sessions::CliSource::Copilot,
            pane_session_id: "pane-1".into(),
            cwd: PathBuf::from("C:\\x"),
            title: "t".into(),
        })
        .await;

        let resumed = reg
            .lookup(&acp::schema::v1::SessionId::new("resumed"))
            .await
            .unwrap();
        assert_eq!(
            resumed.status,
            Some(crate::agent_sessions::AgentStatus::Idle),
            "the resumed row must survive a foreign session start on its pane"
        );
        assert_eq!(resumed.pane_session_id.as_deref(), Some("pane-1"));

        let bootstrap = reg
            .lookup(&acp::schema::v1::SessionId::new("bootstrap"))
            .await
            .unwrap();
        assert!(
            bootstrap.pane_session_id.is_none(),
            "the bootstrap session is still recorded, just never bound to the pane"
        );
    }

    /// The point of keeping the row live: the hookless watcher's status
    /// fallback still reaches it. An Ended row would drop the event.
    #[tokio::test]
    async fn master_reducer_resumed_row_still_takes_status_after_foreign_session_start() {
        let reg = InMemoryRegistry::new();
        seed_resumed_pane(&reg, "resumed", "pane-1").await;
        reg.apply_event(crate::agent_sessions::SessionEvent::SessionStarted {
            key: "bootstrap".into(),
            cli_source: crate::agent_sessions::CliSource::Copilot,
            pane_session_id: "pane-1".into(),
            cwd: PathBuf::from("C:\\x"),
            title: "t".into(),
        })
        .await;

        let changed = reg
            .apply_event(crate::agent_sessions::SessionEvent::ToolStarting {
                key: "resumed".into(),
                tool_name: "shell".into(),
            })
            .await;
        let resumed = reg
            .lookup(&acp::schema::v1::SessionId::new("resumed"))
            .await
            .unwrap();

        assert!(changed);
        assert_eq!(
            resumed.status,
            Some(crate::agent_sessions::AgentStatus::Working)
        );
    }

    #[tokio::test]
    async fn opencode_unbind_and_server_end_are_conditional_on_current_owner() {
        use crate::agent_sessions::{AgentStatus, CliSource, SessionEvent};
        let reg = InMemoryRegistry::new();
        let sid = acp::schema::v1::SessionId::new("opencode-unbind");
        let start = |pane: &str| SessionEvent::SessionStarted {
            key: sid.to_string(),
            cli_source: CliSource::OpenCode,
            pane_session_id: pane.into(),
            cwd: PathBuf::from("C:\\repo"),
            title: "title".into(),
        };
        reg.apply_event(start("pane-a")).await;
        reg.apply_event(start("pane-b")).await;
        assert!(
            !reg.apply_event(SessionEvent::SessionUnbound {
                key: sid.to_string(),
                pane_session_id: "pane-a".into(),
            })
            .await
        );
        assert!(
            !reg.apply_event(SessionEvent::SessionStoppedIfOwner {
                key: sid.to_string(),
                pane_session_id: Some("pane-a".into()),
                reason: "deleted".into(),
            })
            .await
        );
        assert!(
            reg.apply_event(SessionEvent::SessionUnbound {
                key: sid.to_string(),
                pane_session_id: "pane-b".into(),
            })
            .await
        );
        let row = reg.lookup(&sid).await.unwrap();
        assert_eq!(row.status, Some(AgentStatus::Idle));
        assert_eq!(row.pane_session_id, None);
        assert!(
            !reg.apply_event(SessionEvent::SessionStoppedIfOwner {
                key: sid.to_string(),
                pane_session_id: Some("pane-b".into()),
                reason: "deleted".into(),
            })
            .await
        );
        assert!(
            reg.apply_event(SessionEvent::SessionStoppedIfOwner {
                key: sid.to_string(),
                pane_session_id: None,
                reason: "deleted".into(),
            })
            .await
        );
        assert_eq!(
            reg.lookup(&sid).await.unwrap().status,
            Some(AgentStatus::Ended)
        );
    }

    /// The guard releases itself: once the CLI's own hook claims the pane for
    /// the resumed id, a genuinely new session in that pane takes over as
    /// before.
    #[tokio::test]
    async fn master_reducer_own_session_start_releases_the_resumed_pane_guard() {
        let reg = InMemoryRegistry::new();
        seed_resumed_pane(&reg, "resumed", "pane-1").await;
        reg.apply_event(crate::agent_sessions::SessionEvent::SessionStarted {
            key: "resumed".into(),
            cli_source: crate::agent_sessions::CliSource::Copilot,
            pane_session_id: "pane-1".into(),
            cwd: PathBuf::from("C:\\x"),
            title: "t".into(),
        })
        .await;

        reg.apply_event(crate::agent_sessions::SessionEvent::SessionStarted {
            key: "typed-later".into(),
            cli_source: crate::agent_sessions::CliSource::Copilot,
            pane_session_id: "pane-1".into(),
            cwd: PathBuf::from("C:\\x"),
            title: "t".into(),
        })
        .await;

        let resumed = reg
            .lookup(&acp::schema::v1::SessionId::new("resumed"))
            .await
            .unwrap();
        let typed = reg
            .lookup(&acp::schema::v1::SessionId::new("typed-later"))
            .await
            .unwrap();
        assert_eq!(
            resumed.status,
            Some(crate::agent_sessions::AgentStatus::Ended)
        );
        assert!(resumed.pane_session_id.is_none());
        assert_eq!(typed.pane_session_id.as_deref(), Some("pane-1"));
    }

    /// The other release path: the resumed session exits (`/exit` → SessionEnd
    /// hook), so the next session started in that pane binds normally.
    #[tokio::test]
    async fn master_reducer_session_stopped_releases_the_resumed_pane_guard() {
        let reg = InMemoryRegistry::new();
        seed_resumed_pane(&reg, "resumed", "pane-1").await;
        reg.apply_event(crate::agent_sessions::SessionEvent::SessionStopped {
            key: "resumed".into(),
            reason: "user_exit".into(),
        })
        .await;

        reg.apply_event(crate::agent_sessions::SessionEvent::SessionStarted {
            key: "typed-later".into(),
            cli_source: crate::agent_sessions::CliSource::Copilot,
            pane_session_id: "pane-1".into(),
            cwd: PathBuf::from("C:\\x"),
            title: "t".into(),
        })
        .await;

        let typed = reg
            .lookup(&acp::schema::v1::SessionId::new("typed-later"))
            .await
            .unwrap();
        assert_eq!(typed.pane_session_id.as_deref(), Some("pane-1"));
    }

    // ─── Task C sessions/list + sessions/changed schemas ───────────

    #[tokio::test]
    async fn cwd_change_updates_only_the_live_owning_session() {
        use crate::agent_sessions::{AgentStatus, CliSource, SessionEvent};

        let reg = InMemoryRegistry::new();
        reg.apply_event(SessionEvent::SessionStarted {
            key: "cwd-session".into(),
            cli_source: CliSource::Claude,
            pane_session_id: "{DD7141E2-A8D7-4766-B7EE-77C286CAFE83}".into(),
            cwd: PathBuf::from("C:\\first"),
            title: String::new(),
        })
        .await;
        let sid = acp::schema::v1::SessionId::new("cwd-session");
        let update = |pane: &str, cwd: &str| SessionEvent::WorkingDirectoryChanged {
            key: "cwd-session".into(),
            pane_session_id: pane.into(),
            cwd: PathBuf::from(cwd),
        };

        let pane = "dd7141e2-a8d7-4766-b7ee-77c286cafe83";
        assert!(!reg.apply_event(update("pane-2", "C:\\wrong")).await);
        assert!(!reg.apply_event(update(pane, "")).await);
        assert!(reg.apply_event(update(pane, "C:\\second")).await);
        let row = reg.lookup(&sid).await.unwrap();
        assert_eq!(row.cwd, PathBuf::from("C:\\second"));
        assert_eq!(row.status, Some(AgentStatus::Idle));
        assert_eq!(row.pane_session_id.as_deref(), Some(pane));
        assert!(!reg.apply_event(update(pane, "C:\\second")).await);

        reg.apply_event(SessionEvent::SessionStopped {
            key: "cwd-session".into(),
            reason: "exit".into(),
        })
        .await;
        assert!(!reg.apply_event(update(pane, "C:\\stale")).await);
        assert_eq!(
            reg.lookup(&sid).await.unwrap().cwd,
            PathBuf::from("C:\\second")
        );
    }

    #[tokio::test]
    async fn acp_cwd_wins_over_hook_and_rejects_in_flight_old_listing() {
        use crate::agent_sessions::{CliSource, SessionEvent};

        let reg = InMemoryRegistry::new();
        let sid = acp::schema::v1::SessionId::new("copilot-cwd");
        reg.apply_event(SessionEvent::SessionStarted {
            key: sid.to_string(),
            cli_source: CliSource::Copilot,
            pane_session_id: "pane-cwd".into(),
            cwd: PathBuf::from("C:\\initial"),
            title: String::new(),
        })
        .await;
        let original = reg.lookup(&sid).await.unwrap();
        let identity = SessionIdentity::from_info(&original);
        let agent = uuid::Uuid::new_v4();
        assert!(
            reg.adopt_acp_cwd_identity(
                &identity,
                original.cwd_revision(),
                agent,
                PathBuf::from("C:\\acp"),
            )
            .await
        );
        let acp_row = reg.lookup(&sid).await.unwrap();
        let activity_at = acp_row.last_activity_at_ms;
        assert!(
            !reg.apply_event(SessionEvent::WorkingDirectoryChanged {
                key: sid.to_string(),
                pane_session_id: "pane-cwd".into(),
                cwd: PathBuf::from("C:\\new-hook"),
            })
            .await
        );
        let after_hook = reg.lookup(&sid).await.unwrap();
        assert_eq!(after_hook.cwd, PathBuf::from("C:\\acp"));
        assert_eq!(
            after_hook.cwd_evidence.hook,
            Some(PathBuf::from("C:\\new-hook"))
        );
        assert!(
            !reg.adopt_acp_cwd_identity(
                &identity,
                acp_row.cwd_revision(),
                agent,
                PathBuf::from("C:\\old-list"),
            )
            .await
        );
        assert!(
            reg.adopt_acp_cwd_identity(
                &identity,
                after_hook.cwd_revision(),
                agent,
                PathBuf::from("C:\\new-acp"),
            )
            .await
        );
        let current = reg.lookup(&sid).await.unwrap();
        assert_eq!(current.cwd, PathBuf::from("C:\\new-acp"));
        assert_eq!(current.last_activity_at_ms, activity_at);
        assert_eq!(reg.snapshot().await[0].cwd, current.cwd);
    }

    #[tokio::test]
    async fn acp_cwd_rejects_rebound_or_recreated_session() {
        use crate::agent_sessions::{CliSource, SessionEvent};

        let reg = InMemoryRegistry::new();
        let sid = acp::schema::v1::SessionId::new("rebound");
        let start = |pane: &str| SessionEvent::SessionStarted {
            key: sid.to_string(),
            cli_source: CliSource::Copilot,
            pane_session_id: pane.into(),
            cwd: PathBuf::from("C:\\hook"),
            title: String::new(),
        };
        reg.apply_event(start("pane-one")).await;
        let old = reg.lookup(&sid).await.unwrap();
        let identity = SessionIdentity::from_info(&old);
        reg.adopt_acp_cwd_identity(
            &identity,
            old.cwd_revision(),
            uuid::Uuid::new_v4(),
            PathBuf::from("C:\\acp"),
        )
        .await;
        reg.apply_event(start("pane-two")).await;
        assert!(
            !reg.adopt_acp_cwd_identity(
                &identity,
                old.cwd_revision(),
                uuid::Uuid::new_v4(),
                PathBuf::from("C:\\stale"),
            )
            .await
        );
        let rebound = reg.lookup(&sid).await.unwrap();
        assert_eq!(rebound.cwd, PathBuf::from("C:\\hook"));
        assert!(rebound.cwd_evidence.acp.is_none());
        reg.remove_identity(&identity).await;
        reg.apply_event(start("pane-three")).await;
        assert!(
            !reg.adopt_acp_cwd_identity(
                &identity,
                rebound.cwd_revision(),
                uuid::Uuid::new_v4(),
                PathBuf::from("C:\\stale"),
            )
            .await
        );
        assert_eq!(
            reg.lookup(&sid).await.unwrap().cwd,
            PathBuf::from("C:\\hook")
        );
    }

    #[tokio::test]
    async fn acp_cwd_updates_historical_row_without_inventing_pane() {
        use crate::agent_sessions::{AgentStatus, SessionLocation};
        let reg = InMemoryRegistry::new();
        let sid = acp::schema::v1::SessionId::new("saved");
        let mut row = SessionInfo::new(sid.clone(), PathBuf::from("C:\\old"));
        row.provider_id = Some("copilot".into());
        row.location = SessionLocation::Host;
        row.status = Some(AgentStatus::Historical);
        reg.upsert(row).await;
        let old = reg.lookup(&sid).await.unwrap();
        assert!(
            reg.adopt_acp_cwd_identity(
                &SessionIdentity::from_info(&old),
                old.cwd_revision(),
                uuid::Uuid::new_v4(),
                PathBuf::from("C:\\new"),
            )
            .await
        );
        let current = reg.lookup(&sid).await.unwrap();
        assert_eq!(current.cwd, PathBuf::from("C:\\new"));
        assert!(current.pane_session_id.is_none());
        assert_eq!(current.status, Some(AgentStatus::Historical));
    }

    #[tokio::test]
    async fn acp_cwd_requires_exact_supported_host_identity() {
        use crate::agent_sessions::{AgentStatus, SessionLocation};
        let reg = InMemoryRegistry::new();
        let sid = acp::schema::v1::SessionId::new("same-raw-id");
        let mut host = SessionInfo::new(sid.clone(), PathBuf::from("C:\\host"));
        host.provider_id = Some("copilot".into());
        host.location = SessionLocation::Host;
        host.status = Some(AgentStatus::Historical);
        let mut wsl = host.clone();
        wsl.location = SessionLocation::Wsl {
            distro: "Debian".into(),
        };
        wsl.cwd = PathBuf::from("/wsl");
        reg.upsert(host.clone()).await;
        reg.upsert(wsl.clone()).await;
        let host_identity = SessionIdentity::from_info(&host);
        let wsl_identity = SessionIdentity::from_info(&wsl);
        let host_revision = reg
            .lookup_identity(&host_identity)
            .await
            .unwrap()
            .cwd_revision();
        let wsl_revision = reg
            .lookup_identity(&wsl_identity)
            .await
            .unwrap()
            .cwd_revision();
        assert!(
            !reg.adopt_acp_cwd_identity(
                &wsl_identity,
                wsl_revision,
                uuid::Uuid::new_v4(),
                PathBuf::from("C:\\wrong"),
            )
            .await
        );
        assert!(
            reg.adopt_acp_cwd_identity(
                &host_identity,
                host_revision,
                uuid::Uuid::new_v4(),
                PathBuf::from("C:\\updated"),
            )
            .await
        );
        assert!(reg.lookup(&sid).await.is_none(), "raw id remains ambiguous");
        assert_eq!(
            reg.lookup_identity(&wsl_identity).await.unwrap().cwd,
            PathBuf::from("/wsl")
        );
        let mut claude = host.clone();
        claude.provider_id = Some("claude".into());
        claude.cwd = PathBuf::from("C:\\claude");
        reg.upsert(claude.clone()).await;
        let claude_identity = SessionIdentity::from_info(&claude);
        let claude_revision = reg
            .lookup_identity(&claude_identity)
            .await
            .unwrap()
            .cwd_revision();
        assert!(
            reg.adopt_acp_cwd_identity(
                &claude_identity,
                claude_revision,
                uuid::Uuid::new_v4(),
                PathBuf::from("C:\\claude-updated"),
            )
            .await
        );
        assert_eq!(
            reg.lookup_identity(&host_identity).await.unwrap().cwd,
            PathBuf::from("C:\\updated"),
            "Claude's list must not change a colliding Copilot session"
        );
        let mut opencode = host.clone();
        opencode.provider_id = Some("opencode".into());
        reg.upsert(opencode.clone()).await;
        let opencode_identity = SessionIdentity::from_info(&opencode);
        assert!(
            reg.adopt_acp_cwd_identity(
                &opencode_identity,
                reg.lookup_identity(&opencode_identity)
                    .await
                    .unwrap()
                    .cwd_revision(),
                uuid::Uuid::new_v4(),
                PathBuf::from("C:\\opencode-updated"),
            )
            .await
        );
        assert_eq!(
            reg.lookup_identity(&host_identity).await.unwrap().cwd,
            PathBuf::from("C:\\updated"),
            "OpenCode's list must not change a colliding Copilot session"
        );
        let mut codex = host.clone();
        codex.provider_id = Some("codex".into());
        reg.upsert(codex.clone()).await;
        let codex_identity = SessionIdentity::from_info(&codex);
        assert!(
            !reg.adopt_acp_cwd_identity(
                &codex_identity,
                reg.lookup_identity(&codex_identity)
                    .await
                    .unwrap()
                    .cwd_revision(),
                uuid::Uuid::new_v4(),
                PathBuf::from("C:\\wrong-provider"),
            )
            .await
        );
    }

    #[tokio::test]
    async fn complete_acp_absence_uses_only_hook_observed_after_acp() {
        use crate::agent_sessions::{CliSource, SessionEvent};
        let reg = InMemoryRegistry::new();
        let sid = acp::schema::v1::SessionId::new("absent");
        reg.apply_event(SessionEvent::SessionStarted {
            key: sid.to_string(),
            cli_source: CliSource::Copilot,
            pane_session_id: "pane".into(),
            cwd: PathBuf::from("C:\\old-hook"),
            title: String::new(),
        })
        .await;
        let initial = reg.lookup(&sid).await.unwrap();
        let identity = SessionIdentity::from_info(&initial);
        let agent = uuid::Uuid::new_v4();
        reg.adopt_acp_cwd_identity(
            &identity,
            initial.cwd_revision(),
            agent,
            PathBuf::from("C:\\acp"),
        )
        .await;
        let acp_row = reg.lookup(&sid).await.unwrap();
        assert!(
            !reg.clear_acp_cwd_identity(&identity, initial.cwd_revision())
                .await
        );
        assert!(
            !reg.clear_acp_cwd_identity(&identity, acp_row.cwd_revision())
                .await
        );
        assert_eq!(
            reg.lookup(&sid).await.unwrap().cwd,
            PathBuf::from("C:\\acp")
        );

        reg.adopt_acp_cwd_identity(
            &identity,
            reg.lookup(&sid).await.unwrap().cwd_revision(),
            agent,
            PathBuf::from("C:\\acp"),
        )
        .await;
        reg.apply_event(SessionEvent::WorkingDirectoryChanged {
            key: sid.to_string(),
            pane_session_id: "pane".into(),
            cwd: PathBuf::from("C:\\fresh-hook"),
        })
        .await;
        assert!(
            reg.clear_acp_cwd_identity(&identity, reg.lookup(&sid).await.unwrap().cwd_revision(),)
                .await
        );
        assert_eq!(
            reg.lookup(&sid).await.unwrap().cwd,
            PathBuf::from("C:\\fresh-hook")
        );

        reg.adopt_acp_cwd_identity(
            &identity,
            reg.lookup(&sid).await.unwrap().cwd_revision(),
            agent,
            PathBuf::from("C:\\acp-again"),
        )
        .await;
        assert!(
            !reg.apply_event(SessionEvent::WorkingDirectoryChanged {
                key: sid.to_string(),
                pane_session_id: "pane".into(),
                cwd: PathBuf::from("C:\\fresh-hook"),
            })
            .await
        );
        assert!(
            reg.clear_acp_cwd_identity(&identity, reg.lookup(&sid).await.unwrap().cwd_revision())
                .await
        );
        assert_eq!(
            reg.lookup(&sid).await.unwrap().cwd,
            PathBuf::from("C:\\fresh-hook")
        );

        reg.adopt_acp_cwd_identity(
            &identity,
            reg.lookup(&sid).await.unwrap().cwd_revision(),
            agent,
            PathBuf::from("C:\\authoritative"),
        )
        .await;
        reg.apply_event(SessionEvent::WorkingDirectoryChanged {
            key: sid.to_string(),
            pane_session_id: "pane".into(),
            cwd: PathBuf::from("C:\\older-hook"),
        })
        .await;
        let before_confirmation = reg.lookup(&sid).await.unwrap();
        assert!(
            !reg.adopt_acp_cwd_identity(
                &identity,
                before_confirmation.cwd_revision(),
                agent,
                PathBuf::from("C:\\authoritative"),
            )
            .await
        );
        assert!(
            !reg.clear_acp_cwd_identity(&identity, reg.lookup(&sid).await.unwrap().cwd_revision())
                .await
        );
        assert_eq!(
            reg.lookup(&sid).await.unwrap().cwd,
            PathBuf::from("C:\\authoritative"),
            "a hook superseded by a later successful ACP poll must not resurface"
        );
    }

    #[test]
    fn build_sessions_changed_notification_decodes_to_changed() {
        let ext = build_sessions_changed_notification();
        assert_eq!(&*ext.method, INTELLTERM_METHOD_SESSIONS_CHANGED);
        assert_eq!(
            parse_ext_notification(&ext),
            WtaExtNotification::SessionsChanged
        );
    }

    #[tokio::test]
    async fn session_started_hook_does_not_clobber_agent_pane_binding() {
        // The user-visible "focus goes to wrong pane" bug:
        // 1. Master creates an agent-pane session at new_session time
        //    with pane_session_id from _meta.wta (helper's WT_SESSION).
        // 2. The agent runs a tool in a DIFFERENT workspace shell pane.
        // 3. Native CLI hooks in that shell pane fire SessionStarted
        //    with the SHELL pane's GUID, not the helper's.
        // 4. Before this fix: master's reducer clobbered the row's
        //    pane_session_id with the shell GUID. session management Enter on the row
        //    then focused the shell pane instead of the helper pane.
        // 5. With multiple agents sharing a shell, EVERY hook claimed
        //    that shell pane, so sessions thrashed each other off it.
        use crate::agent_sessions::{AgentStatus, CliSource, SessionEvent, SessionOrigin};
        let reg = InMemoryRegistry::new();
        // Seed master's authoritative state: agent-pane session at
        // HELPER_PANE (where the wta-helper TUI lives).
        let sid = acp::schema::v1::SessionId::new("agent-sid");
        let mut info = SessionInfo::new(sid.clone(), PathBuf::from("/repo"));
        info.pane_session_id = Some("helper-pane".to_string());
        info.origin = Some(SessionOrigin::AgentPane);
        info.status = Some(AgentStatus::Idle);
        info.cli_source = Some(CliSource::Copilot);
        reg.upsert(info).await;

        // Now a native CLI hook fires from a SHELL pane (where the
        // agent ran Get-ChildItem), publishing SessionStarted with
        // the SHELL pane's GUID.
        let applied = reg
            .apply_event(SessionEvent::SessionStarted {
                key: "agent-sid".to_string(),
                cli_source: CliSource::Copilot,
                pane_session_id: "shell-pane".to_string(),
                cwd: PathBuf::from("/repo"),
                title: "system32".to_string(),
            })
            .await;
        assert!(applied, "still applied (activity heartbeat update)");

        let row = reg.lookup(&sid).await.unwrap();
        assert_eq!(
            row.pane_session_id.as_deref(),
            Some("helper-pane"),
            "agent-pane row's pane_session_id must NOT be clobbered by \
             a hook from a different pane; got {:?}",
            row.pane_session_id
        );
        assert_eq!(
            row.origin,
            Some(SessionOrigin::AgentPane),
            "origin preserved"
        );
        assert_eq!(row.status, Some(AgentStatus::Idle), "status preserved");
    }

    #[tokio::test]
    async fn session_started_hook_DOES_set_pane_on_class_b_unknown_origin() {
        // Defense-against-overcorrection: the guard above must only
        // protect AgentPane rows. For Class B (origin=Unknown, e.g.
        // user typed `gemini` in pwsh) the shell pane IS the agent
        // pane, so the hook is authoritative and must take effect.
        use crate::agent_sessions::{AgentStatus, CliSource, SessionEvent, SessionOrigin};
        let reg = InMemoryRegistry::new();
        // Don't pre-seed — Class B sessions are born from the hook
        // itself.
        let applied = reg
            .apply_event(SessionEvent::SessionStarted {
                key: "shell-agent-sid".to_string(),
                cli_source: CliSource::Gemini,
                pane_session_id: "shell-pane".to_string(),
                cwd: PathBuf::from("/repo"),
                title: "ask me".to_string(),
            })
            .await;
        assert!(applied);
        let row = reg
            .lookup(&acp::schema::v1::SessionId::new("shell-agent-sid"))
            .await
            .unwrap();
        assert_eq!(row.pane_session_id.as_deref(), Some("shell-pane"));
        assert_eq!(row.status, Some(AgentStatus::Idle));
        // origin defaults to None at creation; would be set to Unknown
        // explicitly by the caller if needed. Test only what we set.
        assert!(matches!(row.origin, None | Some(SessionOrigin::Unknown)));
    }

    // ─── activity-event resurrection guards ─────────────────────────

    #[tokio::test]
    async fn tool_starting_does_not_resurrect_ended_row() {
        // Regression: a straggling ToolStarting hook arriving after a
        // SessionStarted-at-same-pane handoff ended the row used to
        // re-promote status to Working while leaving pane_session_id
        // None, producing the "Working with no pane" zombie the user
        // sees as a duplicate row in session management view.
        use crate::agent_sessions::{AgentStatus, SessionEvent};
        let reg = InMemoryRegistry::new();
        let mut info = SessionInfo::new(
            acp::schema::v1::SessionId::new("ended-sid"),
            PathBuf::from("/repo"),
        );
        info.status = Some(AgentStatus::Ended);
        reg.upsert(info).await;
        let applied = reg
            .apply_event(SessionEvent::ToolStarting {
                key: "ended-sid".to_string(),
                tool_name: "edit".to_string(),
            })
            .await;
        assert!(!applied, "ToolStarting on Ended row must be a no-op");
        let row = reg
            .lookup(&acp::schema::v1::SessionId::new("ended-sid"))
            .await
            .unwrap();
        assert_eq!(
            row.status,
            Some(AgentStatus::Ended),
            "status must stay Ended"
        );
        assert_eq!(
            row.current_tool, None,
            "current_tool must not be set on a zombie"
        );
    }

    #[tokio::test]
    async fn tool_starting_still_promotes_live_idle_row_to_working() {
        // Defense-against-overcorrection: the guard above must not
        // accidentally block legitimate Idle -> Working transitions.
        use crate::agent_sessions::{AgentStatus, SessionEvent};
        let reg = InMemoryRegistry::new();
        let mut info = SessionInfo::new(
            acp::schema::v1::SessionId::new("idle-sid"),
            PathBuf::from("/repo"),
        );
        info.status = Some(AgentStatus::Idle);
        reg.upsert(info).await;
        let applied = reg
            .apply_event(SessionEvent::ToolStarting {
                key: "idle-sid".to_string(),
                tool_name: "edit".to_string(),
            })
            .await;
        assert!(applied);
        let row = reg
            .lookup(&acp::schema::v1::SessionId::new("idle-sid"))
            .await
            .unwrap();
        assert_eq!(row.status, Some(AgentStatus::Working));
        assert_eq!(row.current_tool.as_deref(), Some("edit"));
    }

    #[tokio::test]
    async fn notification_does_not_resurrect_ended_row() {
        use crate::agent_sessions::{AgentStatus, SessionEvent};
        let reg = InMemoryRegistry::new();
        let mut info = SessionInfo::new(
            acp::schema::v1::SessionId::new("ended-sid"),
            PathBuf::from("/repo"),
        );
        info.status = Some(AgentStatus::Ended);
        reg.upsert(info).await;
        let applied = reg
            .apply_event(SessionEvent::Notification {
                key: "ended-sid".to_string(),
                message: "needs input".to_string(),
                timestamp_ms: None,
            })
            .await;
        assert!(!applied);
        let row = reg
            .lookup(&acp::schema::v1::SessionId::new("ended-sid"))
            .await
            .unwrap();
        assert_eq!(row.status, Some(AgentStatus::Ended));
        assert_eq!(row.attention_reason, None);
    }

    #[tokio::test]
    async fn tool_completed_does_not_resurrect_ended_row() {
        use crate::agent_sessions::{AgentStatus, SessionEvent};
        let reg = InMemoryRegistry::new();
        let mut info = SessionInfo::new(
            acp::schema::v1::SessionId::new("ended-sid"),
            PathBuf::from("/repo"),
        );
        info.status = Some(AgentStatus::Ended);
        info.current_tool = Some("edit".to_string()); // pretend it had a tool when ended
        reg.upsert(info).await;
        let applied = reg
            .apply_event(SessionEvent::ToolCompleted {
                key: "ended-sid".to_string(),
            })
            .await;
        assert!(!applied);
        let row = reg
            .lookup(&acp::schema::v1::SessionId::new("ended-sid"))
            .await
            .unwrap();
        assert_eq!(row.status, Some(AgentStatus::Ended));
    }

    #[tokio::test]
    async fn session_started_at_same_pane_ends_prev_and_tool_event_cannot_resurrect_it() {
        // End-to-end repro of the user-visible scenario: real session
        // A runs at pane X, synthetic SessionStarted (or new session)
        // arrives at pane X, then a straggling ToolStarting for A
        // tries to re-promote it. The combined behavior across both
        // reducers must leave A in a stable Ended state with no pane.
        use crate::agent_sessions::{AgentStatus, CliSource, SessionEvent};
        let reg = InMemoryRegistry::new();
        // Seed A as Live at pane X.
        reg.apply_event(SessionEvent::SessionStarted {
            key: "sid-a".to_string(),
            cli_source: CliSource::Copilot,
            pane_session_id: "pane-x".to_string(),
            cwd: PathBuf::from("/repo"),
            title: String::new(),
        })
        .await;
        // Another SessionStarted arrives at pane X (e.g., synthetic
        // pane:<guid> placeholder created from a tool event without
        // agent_session_id). This handoff must end A.
        reg.apply_event(SessionEvent::SessionStarted {
            key: "sid-b".to_string(),
            cli_source: CliSource::Copilot,
            pane_session_id: "pane-x".to_string(),
            cwd: PathBuf::from("/repo"),
            title: String::new(),
        })
        .await;
        let a_after_handoff = reg
            .lookup(&acp::schema::v1::SessionId::new("sid-a"))
            .await
            .unwrap();
        assert_eq!(a_after_handoff.status, Some(AgentStatus::Ended));
        assert_eq!(a_after_handoff.pane_session_id, None);
        // A straggling ToolStarting for A must not resurrect it.
        reg.apply_event(SessionEvent::ToolStarting {
            key: "sid-a".to_string(),
            tool_name: "edit".to_string(),
        })
        .await;
        let a_final = reg
            .lookup(&acp::schema::v1::SessionId::new("sid-a"))
            .await
            .unwrap();
        assert_eq!(
            a_final.status,
            Some(AgentStatus::Ended),
            "ToolStarting after pane handoff must not flip Ended -> Working (zombie)"
        );
        assert_eq!(
            a_final.pane_session_id, None,
            "pane binding must stay None after handoff"
        );
    }

    #[tokio::test]
    async fn registry_assigns_codex_cli_source_when_session_started_via_agent_id() {
        use crate::agent_sessions::{CliSource, SessionEvent};
        let reg = InMemoryRegistry::new();
        let cli = CliSource::from_agent_id("codex")
            .expect("from_agent_id('codex') must yield Some(Codex)");
        let event = SessionEvent::SessionStarted {
            key: "codex-fan-in-test".to_string(),
            cli_source: cli.clone(),
            pane_session_id: "p1".to_string(),
            cwd: PathBuf::from(r#"C:\x"#),
            title: "fan-in test".to_string(),
        };
        reg.apply_event(event).await;
        let row = reg
            .lookup(&acp::schema::v1::SessionId::new("codex-fan-in-test"))
            .await
            .expect("row inserted");
        assert_eq!(row.cli_source, Some(CliSource::Codex));
    }

    #[test]
    fn sessions_list_response_round_trips_session_info_with_typed_fields() {
        let mut info = SessionInfo::new(
            acp::schema::v1::SessionId::new("sid-1"),
            PathBuf::from("/repo"),
        );
        info.title = Some("title".to_string());
        info.status = Some(crate::agent_sessions::AgentStatus::Idle);
        info.cli_source = Some(crate::agent_sessions::CliSource::Copilot);
        info.last_activity_at_ms = Some(42);
        let resp = SessionsListResponse {
            pane_statuses: Vec::new(),
            native_observation_receiver_generation: None,
            native_observation_listener_ready: None,
            sessions: vec![info.clone()],
            native_pane_observations: Vec::new(),
            history_status: Some(HistoryLoadStatus::Ready),
            history_error_kind: None,
        };
        let raw = serde_json::value::to_raw_value(&resp).unwrap();
        let parsed = parse_sessions_list_response(&raw).unwrap();
        assert_eq!(parsed.sessions, vec![info]);
        assert_eq!(parsed.history_status, Some(HistoryLoadStatus::Ready));
    }

    #[test]
    fn sessions_list_response_distinguishes_loading_empty_and_failed_history() {
        for (status, wire) in [
            (HistoryLoadStatus::Loading, "loading"),
            (HistoryLoadStatus::Ready, "ready"),
            (HistoryLoadStatus::Error, "error"),
        ] {
            let raw = build_sessions_list_response(
                Vec::new(),
                Some(status),
                None,
                Vec::new(),
                None,
                None,
                Vec::new(),
            );
            let json: serde_json::Value = serde_json::from_str(raw.get()).unwrap();
            assert_eq!(json["history_status"], wire);
            let parsed = parse_sessions_list_response(&raw).unwrap();
            assert!(parsed.sessions.is_empty());
            assert_eq!(parsed.history_status, Some(status));
        }

        let legacy = serde_json::value::RawValue::from_string(r#"{"sessions":[]}"#.into()).unwrap();
        assert_eq!(
            parse_sessions_list_response(&legacy)
                .unwrap()
                .history_status,
            None
        );
    }

    #[test]
    fn sessions_list_response_timeout_kind_is_optional_and_backward_compatible() {
        let raw = build_sessions_list_response(
            Vec::new(),
            Some(HistoryLoadStatus::Error),
            Some(HistoryErrorKind::Timeout),
            Vec::new(),
            None,
            None,
            Vec::new(),
        );
        let json: serde_json::Value = serde_json::from_str(raw.get()).unwrap();
        assert_eq!(json["history_status"], "error");
        assert_eq!(json["history_error_kind"], "timeout");
        let parsed = parse_sessions_list_response(&raw).unwrap();
        assert_eq!(parsed.history_error_kind, Some(HistoryErrorKind::Timeout));

        #[derive(serde::Deserialize)]
        struct LegacyResponse {
            sessions: Vec<SessionInfo>,
            history_status: Option<HistoryLoadStatus>,
        }
        let legacy: LegacyResponse = serde_json::from_str(raw.get()).unwrap();
        assert!(legacy.sessions.is_empty());
        assert_eq!(legacy.history_status, Some(HistoryLoadStatus::Error));

        for wire in [
            r#"{"sessions":[]}"#,
            r#"{"sessions":[],"history_status":"error"}"#,
        ] {
            let raw = serde_json::value::RawValue::from_string(wire.into()).unwrap();
            assert_eq!(
                parse_sessions_list_response(&raw)
                    .unwrap()
                    .history_error_kind,
                None
            );
        }
        let raw = build_sessions_list_response(
            Vec::new(),
            Some(HistoryLoadStatus::Ready),
            None,
            Vec::new(),
            None,
            None,
            Vec::new(),
        );
        let json: serde_json::Value = serde_json::from_str(raw.get()).unwrap();
        assert!(json.get("history_error_kind").is_none());
        let unknown = serde_json::value::RawValue::from_string(
            r#"{"sessions":[],"history_status":"error","history_error_kind":"future_error"}"#
                .into(),
        )
        .unwrap();
        assert_eq!(
            parse_sessions_list_response(&unknown)
                .unwrap()
                .history_error_kind,
            Some(HistoryErrorKind::Other)
        );
    }

    #[test]
    fn session_resume_dispatched_request_carries_sid() {
        let sid = acp::schema::v1::SessionId::new("resume-me");
        let req = build_session_resume_dispatched_request(&sid);
        assert_eq!(&*req.method, INTELLTERM_METHOD_SESSION_RESUME_DISPATCHED);
        let parsed = parse_session_resume_dispatched_params(&req.params).unwrap();
        assert_eq!(parsed.sid, sid);
    }

    #[test]
    fn session_focus_request_carries_sid() {
        let sid = acp::schema::v1::SessionId::new("focus-me");
        let req = build_session_focus_request(&sid);
        assert_eq!(&*req.method, INTELLTERM_METHOD_SESSION_FOCUS);
        let parsed = parse_session_focus_params(&req.params).unwrap();
        assert_eq!(parsed.sid, sid);
    }

    #[test]
    fn source_pane_session_by_pane_request_round_trips() {
        let req = build_source_pane_session_by_pane_request("{PANE-GUID}");
        assert_eq!(&*req.method, INTELLTERM_METHOD_SOURCE_PANE_SESSION_BY_PANE);
        let parsed = parse_source_pane_session_by_pane_params(&req.params).unwrap();
        assert_eq!(parsed.pane_session_id, "{PANE-GUID}");

        let sid = acp::schema::v1::SessionId::new("agent-session");
        let raw = build_source_pane_session_by_pane_response(Some(sid.clone()));
        let response = parse_source_pane_session_by_pane_response(&raw).unwrap();
        assert_eq!(response.session_id, Some(sid));
    }

    // ─── focus_session ──────────────────────────────────────────────

    #[test]
    fn build_focus_session_request_carries_method_and_session_id() {
        let sid = acp::schema::v1::SessionId::new("focus-target".to_string());
        let req = build_focus_session_request(&sid);
        assert_eq!(&*req.method, INTELLTERM_METHOD_FOCUS_SESSION);
        let parsed = parse_focus_session_params(&req.params)
            .expect("round-trip of FocusSessionParams must succeed");
        assert_eq!(parsed.session_id, sid);
    }

    #[test]
    fn parse_focus_session_params_rejects_garbage() {
        let raw = serde_json::value::RawValue::from_string(r#"{"wrong":"shape"}"#.into()).unwrap();
        assert!(parse_focus_session_params(&raw).is_err());
    }

    #[test]
    fn build_then_parse_sessions_changed_is_empty_notification() {
        let notification = build_sessions_changed_notification();
        assert_eq!(&*notification.method, INTELLTERM_METHOD_SESSIONS_CHANGED);
        assert_eq!(notification.params.get(), "{}");
    }

    // ─── ext-method classification (ACP-1.0 leading-`_` strip) ──────────

    #[test]
    fn ext_method_matches_accepts_prefixed_and_wire_stripped() {
        // Local/test construction keeps the `_`; the ACP-1.0 wire path strips it.
        assert!(ext_method_matches(
            INTELLTERM_METHOD_SESSIONS_LIST,
            INTELLTERM_METHOD_SESSIONS_LIST
        ));
        assert!(ext_method_matches(
            "intellterm.wta/sessions/list",
            INTELLTERM_METHOD_SESSIONS_LIST
        ));
        assert!(!ext_method_matches(
            "intellterm.wta/something_else",
            INTELLTERM_METHOD_SESSIONS_LIST
        ));
    }

    /// Emulate the ACP-1.0 receive path: drop the single leading `_` from an
    /// `ExtRequest`'s method, keeping the params, so a test can exercise what
    /// the master actually sees over the wire.
    fn strip_leading_underscore(req: acp::schema::v1::ExtRequest) -> acp::schema::v1::ExtRequest {
        let stripped = req
            .method
            .strip_prefix('_')
            .expect("our INTELLTERM_METHOD_* constants are `_`-prefixed")
            .to_string();
        acp::schema::v1::ExtRequest::new(stripped, req.params.clone())
    }

    /// A prefixed (locally-constructed) request decodes to its typed variant,
    /// and params flow through (checked on `sessions/list.rescan`).
    #[test]
    fn parse_ext_request_decodes_every_method_prefixed() {
        let sid = acp::schema::v1::SessionId::new("sid-p".to_string());
        let ev = crate::agent_sessions::SessionEvent::ToolCompleted {
            key: "k".to_string(),
        };
        assert!(matches!(
            parse_ext_request(build_focus_session_request(&sid)),
            WtaExtRequest::FocusSession(_)
        ));
        assert!(
            matches!(parse_ext_request(build_sessions_list_request(true)), WtaExtRequest::SessionsList(p) if p.rescan)
        );
        assert!(matches!(
            parse_ext_request(build_session_hook_request(&ev)),
            WtaExtRequest::SessionHook(_)
        ));
        assert!(matches!(
            parse_ext_request(build_born_bound_request(&ev)),
            WtaExtRequest::SessionBornBound(..)
        ));
        assert!(matches!(
            parse_ext_request(build_session_resume_dispatched_request(&sid)),
            WtaExtRequest::SessionResumeDispatched(_)
        ));
        assert!(matches!(
            parse_ext_request(build_session_focus_request(&sid)),
            WtaExtRequest::SessionFocus(_)
        ));
        assert!(matches!(
            parse_ext_request(build_source_pane_session_by_pane_request("pane")),
            WtaExtRequest::SourcePaneSessionByPane(_)
        ));
    }

    /// Regression: ACP 1.0 delivers the method name with the leading `_`
    /// already stripped. Each must still decode to its handler variant, not
    /// `ForwardToAgent` (which forwarded to the agent CLI → -32601).
    #[test]
    fn parse_ext_request_decodes_every_method_wire_stripped() {
        let sid = acp::schema::v1::SessionId::new("sid-w".to_string());
        let ev = crate::agent_sessions::SessionEvent::ToolCompleted {
            key: "k".to_string(),
        };
        assert!(matches!(
            parse_ext_request(strip_leading_underscore(build_focus_session_request(&sid))),
            WtaExtRequest::FocusSession(_)
        ));
        assert!(matches!(
            parse_ext_request(strip_leading_underscore(build_sessions_list_request(false))),
            WtaExtRequest::SessionsList(_)
        ));
        assert!(matches!(
            parse_ext_request(strip_leading_underscore(build_session_hook_request(&ev))),
            WtaExtRequest::SessionHook(_)
        ));
        assert!(matches!(
            parse_ext_request(strip_leading_underscore(build_born_bound_request(&ev))),
            WtaExtRequest::SessionBornBound(..)
        ));
        assert!(matches!(
            parse_ext_request(strip_leading_underscore(
                build_session_resume_dispatched_request(&sid)
            )),
            WtaExtRequest::SessionResumeDispatched(_)
        ));
        assert!(matches!(
            parse_ext_request(strip_leading_underscore(build_session_focus_request(&sid))),
            WtaExtRequest::SessionFocus(_)
        ));
        assert!(matches!(
            parse_ext_request(strip_leading_underscore(
                build_source_pane_session_by_pane_request("pane")
            )),
            WtaExtRequest::SourcePaneSessionByPane(_)
        ));
    }

    #[test]
    fn parse_ext_request_forwards_foreign_methods() {
        let raw = serde_json::value::RawValue::from_string("{}".into()).unwrap();
        let foreign = acp::schema::v1::ExtRequest::new("somebody.else/method", Arc::from(raw));
        assert!(matches!(
            parse_ext_request(foreign),
            WtaExtRequest::ForwardToAgent(_)
        ));
    }

    /// A recognized method carrying the wrong param shape → `Malformed`, never a
    /// silent forward-to-agent (which would surface to the user as -32601).
    #[test]
    fn parse_ext_request_reports_malformed_params_for_known_method() {
        let raw = serde_json::value::RawValue::from_string(r#"{"wrong":"shape"}"#.into()).unwrap();
        let bad = acp::schema::v1::ExtRequest::new(INTELLTERM_METHOD_FOCUS_SESSION, Arc::from(raw));
        match parse_ext_request(bad) {
            WtaExtRequest::Malformed { method, .. } => assert!(method.contains("focus_session")),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    /// Regression for the notification side of the same ACP-1.0 strip: a
    /// `session_added` arriving over the wire (no leading `_`) must parse to
    /// `SessionAdded`, not `Unknown` — otherwise the helper's alive-mirror
    /// never updates.
    #[test]
    fn parse_ext_notification_accepts_wire_stripped_method() {
        let row = SessionInfo::new(
            acp::schema::v1::SessionId::new("sess-strip".to_string()),
            PathBuf::from("/repo/strip"),
        );
        let built = build_session_added_notification(&row);
        // Emulate the ACP-1.0 receive path: drop the single leading `_`.
        let stripped_method = built
            .method
            .strip_prefix('_')
            .expect("our INTELLTERM_METHOD_* constants are `_`-prefixed")
            .to_string();
        let wire = acp::schema::v1::ExtNotification::new(stripped_method, built.params.clone());
        match parse_ext_notification(&wire) {
            WtaExtNotification::SessionAdded(parsed) => assert_eq!(parsed, row),
            other => panic!("expected SessionAdded from wire-stripped method, got {other:?}"),
        }
    }

    // ─── agent_session_to_session_info (master history seeding) ─────────

    #[test]
    fn agent_session_to_session_info_preserves_fields_for_historical_row() {
        use crate::agent_sessions::{
            AgentSession, AgentStatus, CliSource, SessionLocation, SessionOrigin,
        };
        let s = AgentSession {
            key: "hist-sid".to_string(),
            cli_source: CliSource::Copilot,
            pane_session_id: None,
            window_id: None,
            tab_id: None,
            title: "fix build".to_string(),
            cwd: PathBuf::from(r#"C:\repo"#),
            started_at: std::time::UNIX_EPOCH + std::time::Duration::from_millis(1_000_000),
            last_activity_at: std::time::UNIX_EPOCH + std::time::Duration::from_millis(2_000_000),
            status: AgentStatus::Historical,
            last_error: None,
            current_tool: None,
            attention_reason: None,
            attention_timestamp_ms: None,
            attention_cooldown_until_ms: None,
            log_path: None,
            origin: SessionOrigin::AgentPane,
            location: SessionLocation::Host,
        };
        let info = agent_session_to_session_info(&s);
        assert_eq!(info.session_id.0.as_ref(), "hist-sid");
        assert_eq!(info.cwd, PathBuf::from(r#"C:\repo"#));
        assert_eq!(info.title.as_deref(), Some("fix build"));
        assert_eq!(info.status, Some(AgentStatus::Historical));
        assert_eq!(info.cli_source, Some(CliSource::Copilot));
        assert_eq!(info.origin, Some(SessionOrigin::AgentPane));
        assert_eq!(info.last_activity_at_ms, Some(2_000_000));
        assert_eq!(info.pane_session_id, None);
    }

    #[test]
    fn agent_session_to_session_info_drops_empty_title() {
        use crate::agent_sessions::{
            AgentSession, AgentStatus, CliSource, SessionLocation, SessionOrigin,
        };
        let s = AgentSession {
            key: "x".to_string(),
            cli_source: CliSource::Claude,
            pane_session_id: Some("pane-X".to_string()),
            window_id: None,
            tab_id: None,
            title: String::new(),
            cwd: PathBuf::from("/repo"),
            started_at: std::time::SystemTime::now(),
            last_activity_at: std::time::SystemTime::now(),
            status: AgentStatus::Idle,
            last_error: None,
            current_tool: None,
            attention_reason: None,
            attention_timestamp_ms: None,
            attention_cooldown_until_ms: None,
            log_path: None,
            origin: SessionOrigin::Unknown,
            location: SessionLocation::Host,
        };
        let info = agent_session_to_session_info(&s);
        assert_eq!(
            info.title, None,
            "empty title should map to None, not Some(\"\")"
        );
        assert_eq!(info.pane_session_id.as_deref(), Some("pane-X"));
    }

    #[test]
    fn build_then_parse_session_hook_round_trips_every_session_event_variant() {
        use crate::agent_sessions::{CliSource, SessionEvent};

        let cases = vec![
            SessionEvent::SessionStarted {
                key: "session-started".to_string(),
                cli_source: CliSource::Copilot,
                pane_session_id: "pane-1".to_string(),
                cwd: PathBuf::from(r#"C:\repo\project"#),
                title: "fix build".to_string(),
            },
            SessionEvent::ToolStarting {
                key: "tool-starting".to_string(),
                tool_name: "edit".to_string(),
            },
            SessionEvent::ToolCompleted {
                key: "tool-completed".to_string(),
            },
            SessionEvent::ToolReturned {
                key: "tool-returned".to_string(),
                timestamp_ms: Some(1792473545580),
            },
            SessionEvent::Notification {
                key: "notification".to_string(),
                message: "waiting for input".to_string(),
                timestamp_ms: Some(1792473545608),
            },
            SessionEvent::SessionStopped {
                key: "session-stopped".to_string(),
                reason: "done".to_string(),
            },
            SessionEvent::ConnectionFailed {
                pane_session_id: "pane-failed".to_string(),
                reason: "pipe closed".to_string(),
            },
            SessionEvent::PaneClosed {
                pane_session_id: "pane-closed".to_string(),
            },
            SessionEvent::ResumeDispatched {
                key: "resume-dispatched".to_string(),
            },
            SessionEvent::ResumePaneAssigned {
                key: "resume-pane-assigned".to_string(),
                pane_session_id: "pane-resumed".to_string(),
            },
        ];

        for event in cases {
            let request = build_session_hook_request(&event);
            assert_eq!(&*request.method, INTELLTERM_METHOD_SESSION_HOOK);
            let parsed = parse_session_hook_params(&request.params)
                .expect("session_hook request params must decode");
            assert_eq!(parsed, event);
        }
    }

    #[test]
    fn session_hook_timestamps_remain_optional_for_older_peers() {
        use crate::agent_sessions::SessionEvent;

        for (params, expected) in [
            (
                serde_json::json!({"kind": "ToolReturned", "key": "sid"}),
                SessionEvent::ToolReturned {
                    key: "sid".into(),
                    timestamp_ms: None,
                },
            ),
            (
                serde_json::json!({"kind": "Notification", "key": "sid", "message": "approve"}),
                SessionEvent::Notification {
                    key: "sid".into(),
                    message: "approve".into(),
                    timestamp_ms: None,
                },
            ),
        ] {
            let wire: SessionHookParams = serde_json::from_value(params).unwrap();
            assert_eq!(SessionEvent::from(wire), expected);
        }
    }

    #[test]
    fn session_hook_cli_source_unknown_round_trips_without_lowercasing() {
        use crate::agent_sessions::{CliSource, SessionEvent};

        let event = SessionEvent::SessionStarted {
            key: "unknown-cli".to_string(),
            cli_source: CliSource::Unknown("MyCustomCLI".to_string()),
            pane_session_id: "pane-unknown".to_string(),
            cwd: PathBuf::from(r#"C:\repo\custom"#),
            title: "custom".to_string(),
        };

        let request = build_session_hook_request(&event);
        let parsed =
            parse_session_hook_params(&request.params).expect("unknown cli_source must round-trip");
        assert_eq!(parsed, event);
    }

    #[test]
    fn build_born_bound_request_uses_distinct_method_and_round_trips() {
        use crate::agent_sessions::{CliSource, SessionEvent};
        let event = SessionEvent::SessionStarted {
            key: "bb-1".to_string(),
            cli_source: CliSource::Claude,
            pane_session_id: "pane-bb".to_string(),
            cwd: PathBuf::from(r#"C:\repo"#),
            title: String::new(),
        };
        let request = build_born_bound_request(&event);
        assert_eq!(
            &*request.method, INTELLTERM_METHOD_SESSION_BORN_BOUND,
            "born-bound must use its own method, not session_hook"
        );
        // Body is the same wire shape, so the master parses it identically.
        let parsed =
            parse_session_hook_params(&request.params).expect("born-bound body must round-trip");
        assert_eq!(parsed, event);
    }

    #[test]
    fn born_bound_request_carries_optional_wsl_distro() {
        use crate::agent_sessions::{CliSource, SessionEvent};
        let event = SessionEvent::SessionStarted {
            key: "bb-wsl".to_string(),
            cli_source: CliSource::Copilot,
            pane_session_id: "pane-wsl".to_string(),
            cwd: PathBuf::from("/mnt/c/Users/dev"),
            title: String::new(),
        };

        // Host (no distro): parses back to the event with no distro, and the
        // event still round-trips through the plain hook parser.
        let host = build_born_bound_request(&event);
        let (ev_host, distro_host) =
            parse_born_bound_params(&host.params).expect("host born-bound parses");
        assert_eq!(ev_host, event);
        assert_eq!(distro_host, None);
        assert_eq!(
            parse_session_hook_params(&host.params).expect("host body is hook-shaped"),
            event,
            "host born-bound body must stay compatible with the plain hook parser",
        );

        // WSL: the distro rides alongside, and the event still parses cleanly.
        let wsl = build_born_bound_request_wsl(&event, "Ubuntu");
        let (ev_wsl, distro_wsl) =
            parse_born_bound_params(&wsl.params).expect("wsl born-bound parses");
        assert_eq!(ev_wsl, event);
        assert_eq!(distro_wsl.as_deref(), Some("Ubuntu"));
        assert_eq!(
            parse_session_hook_params(&wsl.params)
                .expect("wsl body ignores the extra wsl_distro key"),
            event,
            "the extra wsl_distro key must not break the plain hook parser",
        );
    }

    #[test]
    fn parse_born_bound_params_rejects_malformed_wsl_distro() {
        use crate::agent_sessions::{CliSource, SessionEvent};
        let event = SessionEvent::SessionStarted {
            key: "bb-bad".to_string(),
            cli_source: CliSource::Copilot,
            pane_session_id: "pane".to_string(),
            cwd: PathBuf::from("/mnt/c"),
            title: String::new(),
        };
        // Build a valid WSL body, then corrupt `wsl_distro` to the wrong type.
        let good = build_born_bound_request_wsl(&event, "Ubuntu");
        let mut value: serde_json::Value = serde_json::from_str(good.params.get()).unwrap();
        value["wsl_distro"] = serde_json::json!(123);
        let raw = serde_json::value::RawValue::from_string(value.to_string()).unwrap();

        // The event still parses, but the malformed distro must surface as an
        // error rather than being silently dropped (which would mislabel the
        // WSL row as a host session).
        assert!(parse_session_hook_params(&raw).is_ok());
        assert!(
            parse_born_bound_params(&raw).is_err(),
            "a wrong-typed wsl_distro must be rejected, not silently ignored"
        );
    }

    #[tokio::test]
    async fn set_location_updates_only_on_change() {
        use crate::agent_sessions::SessionLocation;
        let reg = InMemoryRegistry::new();
        let sid = acp::schema::v1::SessionId::new("loc-1".to_string());
        reg.upsert(SessionInfo::new(sid.clone(), PathBuf::from("/mnt/c/proj")))
            .await;
        // Default row is Host → stamping Wsl changes it.
        assert!(
            reg.set_location(
                &sid,
                SessionLocation::Wsl {
                    distro: "Ubuntu".to_string()
                }
            )
            .await
        );
        assert_eq!(
            reg.lookup(&sid).await.unwrap().location,
            SessionLocation::Wsl {
                distro: "Ubuntu".to_string()
            }
        );
        // Idempotent: same value → no change reported.
        assert!(
            !reg.set_location(
                &sid,
                SessionLocation::Wsl {
                    distro: "Ubuntu".to_string()
                }
            )
            .await
        );
        // Absent id → no change.
        assert!(
            !reg.set_location(
                &acp::schema::v1::SessionId::new("missing".to_string()),
                SessionLocation::Host
            )
            .await
        );
    }

    #[test]
    fn session_hook_cli_source_round_trips_codex() {
        use crate::agent_sessions::CliSource;
        let typed = CliSource::Codex;
        let wire: SessionHookCliSource = (&typed).into();
        assert!(
            matches!(wire, SessionHookCliSource::Known(ref s) if s == "Codex"),
            "Codex must serialize to Known(\"Codex\"), got {:?}",
            wire
        );
        let back: CliSource = wire.into();
        assert_eq!(back, CliSource::Codex);
    }

    #[test]
    fn session_hook_cli_source_accepts_lowercase_codex() {
        use crate::agent_sessions::CliSource;
        let wire = SessionHookCliSource::Known("codex".to_string());
        let typed: CliSource = wire.into();
        assert_eq!(typed, CliSource::Codex);
    }

    #[test]
    fn session_hook_cli_source_round_trips_opencode() {
        use crate::agent_sessions::CliSource;
        let wire: SessionHookCliSource = (&CliSource::OpenCode).into();
        assert!(matches!(wire, SessionHookCliSource::Known(ref s) if s == "OpenCode"));
        let typed: CliSource = wire.into();
        assert_eq!(typed, CliSource::OpenCode);
    }

    #[test]
    fn antigravity_session_source_survives_the_helper_master_boundary() {
        use crate::agent_sessions::CliSource;
        let source = CliSource::from_agent_id("antigravity")
            .expect("the built-in provider must have a typed session source");
        assert_eq!(CliSource::parse(Some("ANTIGRAVITY")), source);
        let wire: SessionHookCliSource = (&source).into();
        assert_eq!(wire, SessionHookCliSource::Known("Antigravity".to_string()));
        let restored: CliSource = wire.into();
        assert_eq!(restored, source);
        let lowercase: CliSource = SessionHookCliSource::Known("antigravity".to_string()).into();
        assert_eq!(lowercase, source);
    }

    #[test]
    fn parse_session_hook_params_rejects_garbage() {
        let raw = serde_json::value::RawValue::from_string(r#"{"wrong":"shape"}"#.into()).unwrap();
        assert!(parse_session_hook_params(&raw).is_err());
    }

    #[test]
    fn build_session_hook_response_serializes_applied_flag() {
        let response = build_session_hook_response(true);
        assert_eq!(response.0.get(), r#"{"applied":true}"#);
    }

    #[test]
    fn inject_creates_meta_when_missing_and_writes_pane_session_id() {
        let mut meta: Option<acp::schema::v1::Meta> = None;
        inject_wta_meta(
            &mut meta,
            &WtaMeta {
                pane_session_id: Some("pane-A".to_string()),
                ..Default::default()
            },
        );
        let map = meta.expect("meta created");
        let wta = map.get("wta").and_then(|v| v.as_object()).unwrap();
        assert_eq!(
            wta.get("pane_session_id").and_then(|v| v.as_str()),
            Some("pane-A")
        );
    }

    #[test]
    fn inject_preserves_other_vendor_meta_keys() {
        let mut meta = meta_with(serde_json::json!({ "other": "keep-me" }));
        inject_wta_meta(
            &mut meta,
            &WtaMeta {
                pane_session_id: Some("pane-A".to_string()),
                ..Default::default()
            },
        );
        let map = meta.unwrap();
        assert_eq!(
            map.get("other"),
            Some(&serde_json::Value::String("keep-me".to_string())),
            "other vendor's meta survives"
        );
        assert!(map.contains_key("wta"), "wta inserted");
    }

    #[test]
    fn inject_then_extract_is_identity() {
        let original = WtaMeta {
            pane_session_id: Some("pane-X".to_string()),
            owner_tab_id: Some("{tab-owner-X}".to_string()),
            ..Default::default()
        };
        let mut meta: Option<acp::schema::v1::Meta> = None;
        inject_wta_meta(&mut meta, &original);
        let parsed = extract_wta_meta(&mut meta);
        assert_eq!(parsed, original, "round-trip preserves data");
        assert!(meta.is_none(), "round-trip ends with empty meta");
    }

    #[test]
    fn inject_then_extract_round_trips_agent_identity() {
        // The multi-agent master selects + reconstructs the CLI from
        // `agent_id` (+ `model`) carried on the helper's `initialize`
        // handshake. Guard the wire round-trip of all three identity
        // fields, including `model` (which used to ride inside
        // `agent_cmd` and now travels on its own).
        let original = WtaMeta {
            agent_cmd: Some("npx -y @agentclientprotocol/claude-agent-acp@0.65.0".to_string()),
            agent_id: Some("gemini".to_string()),
            resolved_agent_id: Some("copilot".to_string()),
            resolved_model_source: Some("byok".to_string()),
            model: Some("gemini-2.5-pro".to_string()),
            provider_binding: Some("custom:provider-openrouter:qwen/qwen3.5-9b".to_string()),
            agent_source: Some("wsl".to_string()),
            wsl_distro: Some("Ubuntu".to_string()),
            cloud_models: Some(r#"[{"id":"cloud-native","name":"Cloud Native"}]"#.to_string()),
            cloud_models_source: Some("helper".to_string()),
            ..Default::default()
        };
        let mut meta: Option<acp::schema::v1::Meta> = None;
        inject_wta_meta(&mut meta, &original);
        let parsed = extract_wta_meta(&mut meta);
        assert_eq!(parsed, original, "agent identity survives the wire");
    }

    #[test]
    fn explicit_startup_retry_intent_survives_the_private_metadata_boundary() {
        let mut meta = Some(
            serde_json::json!({"wta": {"agent_id": "copilot", "startup_intent": "retry"}})
                .as_object()
                .unwrap()
                .clone(),
        );
        let parsed = extract_wta_meta(&mut meta);
        assert!(meta.is_none(), "private retry intent must not reach the agent");
        inject_wta_meta(&mut meta, &parsed);
        assert_eq!(
            meta.as_ref().unwrap()["wta"]["startup_intent"],
            "retry",
            "explicit retry intent must reach the lifecycle owner"
        );
    }

    #[test]
    fn inject_then_extract_round_trips_control_client_role() {
        let original = WtaMeta {
            control_client: Some("sidebar-history-v1".to_string()),
            ..Default::default()
        };
        let mut meta: Option<acp::schema::v1::Meta> = None;
        inject_wta_meta(&mut meta, &original);
        assert_eq!(extract_wta_meta(&mut meta), original);
        assert!(meta.is_none());
    }

    #[test]
    fn extract_drops_empty_and_whitespace_string_fields_to_none() {
        // An empty / whitespace-only value on the wire must parse back to
        // `None`, not `Some("")` — otherwise `WtaMeta::is_empty()` stays
        // false and a semantically-empty `_meta.wta` is kept alive on the
        // wire and in registry rows.
        let mut map = serde_json::Map::new();
        map.insert(
            WTA_META_NAMESPACE.to_string(),
            serde_json::json!({
                "pane_session_id": "",
                "agent_cmd": "",
                "agent_id": "   ",
                "model": "\t",
                "owner_tab_id": " ",
            }),
        );
        let mut meta: Option<acp::schema::v1::Meta> = Some(map);
        let parsed = extract_wta_meta(&mut meta);
        assert_eq!(parsed, WtaMeta::default(), "all-blank fields ⇒ default");
        assert!(parsed.is_empty(), "is_empty() true for all-blank input");
        assert!(
            meta.is_none(),
            "_meta with only-blank wta collapses to None"
        );
    }

    #[test]
    fn inject_drops_whitespace_only_string_fields() {
        // `inject_wta_meta` must mirror `extract_wta_meta`: a whitespace-only
        // `Some(" ")` is semantically empty and must not be serialized as a
        // real field (that would flip `is_empty()` off on the wire and keep an
        // empty `_meta.wta` alive). When *every* field is blank, no `_meta.wta`
        // is attached at all.
        let mut meta: Option<acp::schema::v1::Meta> = None;
        inject_wta_meta(
            &mut meta,
            &WtaMeta {
                pane_session_id: Some("  ".to_string()),
                control_client: Some(" ".to_string()),
                agent_cmd: Some(String::new()),
                agent_id: Some("\t".to_string()),
                resolved_agent_id: Some(" ".to_string()),
                resolved_model_source: Some(" ".to_string()),
                model: Some(" ".to_string()),
                provider_binding: Some(" ".to_string()),
                agent_source: Some(" ".to_string()),
                wsl_distro: Some("\t".to_string()),
                owner_tab_id: Some("\n".to_string()),
                cloud_models: Some(" ".to_string()),
                cloud_models_source: Some("\t".to_string()),
                proposal_mcp: Some(" ".to_string()),
                session_result: Some(" ".to_string()),
                startup_intent: Some(" ".to_string()),
            },
        );
        assert!(meta.is_none(), "all-blank meta ⇒ no _meta.wta on the wire");

        // A blank field alongside a real one drops only the blank; the real
        // value survives and round-trips.
        let mut meta2: Option<acp::schema::v1::Meta> = None;
        inject_wta_meta(
            &mut meta2,
            &WtaMeta {
                agent_id: Some("claude".to_string()),
                model: Some("   ".to_string()),
                ..Default::default()
            },
        );
        let parsed = extract_wta_meta(&mut meta2);
        assert_eq!(parsed.agent_id.as_deref(), Some("claude"), "real id kept");
        assert_eq!(parsed.model, None, "blank model dropped on inject");
    }

    #[test]
    fn is_empty_treats_whitespace_only_fields_as_empty() {
        // `is_empty()` must use the same trim-based notion of emptiness as
        // `extract_wta_meta` / `inject_wta_meta`. A `Some("   ")` is dropped on
        // the wire, so it must also count as empty here — otherwise the two
        // views of "empty" disagree and `inject_wta_meta`'s `is_empty()`
        // early-out keys off a value the wire never reflects.
        assert!(WtaMeta::default().is_empty(), "all-None is empty");
        assert!(
            WtaMeta {
                pane_session_id: Some("  ".to_string()),
                control_client: Some(" ".to_string()),
                agent_cmd: Some(String::new()),
                agent_id: Some("\t".to_string()),
                resolved_agent_id: Some(" ".to_string()),
                resolved_model_source: Some(" ".to_string()),
                model: Some(" ".to_string()),
                provider_binding: Some(" ".to_string()),
                agent_source: Some(" ".to_string()),
                wsl_distro: Some("\t".to_string()),
                owner_tab_id: Some("\n".to_string()),
                cloud_models: Some(" ".to_string()),
                cloud_models_source: Some("\t".to_string()),
                proposal_mcp: Some(" ".to_string()),
                session_result: Some(" ".to_string()),
                startup_intent: Some("\t".to_string()),
            }
            .is_empty(),
            "all-whitespace fields ⇒ empty"
        );
        // A single real value flips it back to non-empty.
        assert!(
            !WtaMeta {
                agent_id: Some("claude".to_string()),
                model: Some("   ".to_string()),
                ..Default::default()
            }
            .is_empty(),
            "one real field ⇒ not empty"
        );
    }

    // ── apply_ext_notification ──────────────────────────────────────

    #[tokio::test]
    async fn apply_ext_notification_upserts_on_session_added() {
        let reg = InMemoryRegistry::new();
        let info = SessionInfo::new(
            acp::schema::v1::SessionId::new("sess-1".to_string()),
            std::path::PathBuf::from("/tmp/x"),
        )
        .with_pane_session_id("pane-1".to_string());
        let ext = build_session_added_notification(&info);
        let classified = apply_ext_notification(&reg, &ext).await;
        assert!(matches!(classified, WtaExtNotification::SessionAdded(_)));
        let row = reg.lookup(&info.session_id).await.expect("upserted");
        assert_eq!(row.pane_session_id.as_deref(), Some("pane-1"));
    }

    #[tokio::test]
    async fn apply_ext_notification_removes_on_session_removed() {
        let reg = InMemoryRegistry::new();
        let info = SessionInfo::new(
            acp::schema::v1::SessionId::new("dies".to_string()),
            std::path::PathBuf::from("/tmp/y"),
        );
        reg.upsert(info.clone()).await;
        let ext = build_session_removed_notification(&info.session_id);
        let classified = apply_ext_notification(&reg, &ext).await;
        assert!(matches!(classified, WtaExtNotification::SessionRemoved(_)));
        assert!(reg.lookup(&info.session_id).await.is_none());
    }

    #[tokio::test]
    async fn apply_ext_notification_is_noop_on_unknown_method() {
        let reg = InMemoryRegistry::new();
        let pre = SessionInfo::new(
            acp::schema::v1::SessionId::new("keep".to_string()),
            std::path::PathBuf::from("/tmp/z"),
        );
        reg.upsert(pre.clone()).await;
        let raw = serde_json::value::RawValue::from_string("{}".into()).unwrap();
        let ext = acp::schema::v1::ExtNotification::new(
            std::sync::Arc::<str>::from("some.other.vendor/event"),
            std::sync::Arc::from(raw),
        );
        let classified = apply_ext_notification(&reg, &ext).await;
        assert!(matches!(classified, WtaExtNotification::Unknown));
        assert!(
            reg.lookup(&pre.session_id).await.is_some(),
            "registry not touched"
        );
    }

    #[tokio::test]
    async fn apply_ext_notification_is_noop_on_malformed_params() {
        let reg = InMemoryRegistry::new();
        // Right method, wrong shape (missing session_id).
        let raw =
            serde_json::value::RawValue::from_string(r#"{"not_session_id":"x"}"#.into()).unwrap();
        let ext = acp::schema::v1::ExtNotification::new(
            std::sync::Arc::<str>::from(INTELLTERM_METHOD_SESSION_REMOVED),
            std::sync::Arc::from(raw),
        );
        let classified = apply_ext_notification(&reg, &ext).await;
        assert!(
            matches!(classified, WtaExtNotification::MalformedParams { .. }),
            "got {classified:?}"
        );
        assert!(
            reg.snapshot().await.is_empty(),
            "registry untouched on malformed input"
        );
    }

    // ─── WSL SessionLocation serde + wire round-trip ─────────────────────────

    /// `SessionInfo` must carry a `Wsl` location through JSON serde so that
    /// when master serializes the `sessions/list` response and the helper
    /// deserializes it, the distro stamp survives the boundary.
    #[test]
    fn session_info_carries_wsl_location_through_serde() {
        use crate::agent_sessions::{
            AgentSession, AgentStatus, CliSource, SessionLocation, SessionOrigin,
        };

        let s = AgentSession {
            key: "k".into(),
            cli_source: CliSource::Copilot,
            pane_session_id: None,
            window_id: None,
            tab_id: None,
            title: "t".into(),
            cwd: PathBuf::from("/home/u"),
            started_at: std::time::SystemTime::UNIX_EPOCH,
            last_activity_at: std::time::SystemTime::UNIX_EPOCH,
            status: AgentStatus::Historical,
            last_error: None,
            current_tool: None,
            attention_reason: None,
            attention_timestamp_ms: None,
            attention_cooldown_until_ms: None,
            log_path: None,
            origin: SessionOrigin::Unknown,
            location: SessionLocation::Wsl {
                distro: "Ubuntu".into(),
            },
        };

        // Round-trip through agent_session_to_session_info.
        let info = agent_session_to_session_info(&s);
        assert_eq!(
            info.location,
            SessionLocation::Wsl {
                distro: "Ubuntu".into()
            },
            "agent_session_to_session_info must carry the WSL location"
        );

        // Round-trip through JSON serde (the wire boundary).
        let json = serde_json::to_string(&info).unwrap();
        let back: SessionInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.location,
            SessionLocation::Wsl {
                distro: "Ubuntu".into()
            },
            "SessionInfo must survive JSON round-trip with Wsl location intact"
        );
    }

    /// A `SessionInfo` JSON object that has no `location` key (e.g. produced
    /// by an older master that predates this field) must deserialize as
    /// `Unknown`, so activation cannot guess where its artefacts live.
    #[test]
    fn session_info_without_location_defaults_to_unknown() {
        use crate::agent_sessions::SessionLocation;

        // Build a valid SessionInfo, serialise it, then strip the location key.
        let info = SessionInfo::new(acp::schema::v1::SessionId::new("x"), PathBuf::from("/tmp"));
        let mut value: serde_json::Value = serde_json::to_value(&info).unwrap();
        value.as_object_mut().unwrap().remove("location");

        let json = serde_json::to_string(&value).unwrap();
        let parsed: SessionInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed.location,
            SessionLocation::Unknown,
            "missing location key must fail closed"
        );
    }
}
