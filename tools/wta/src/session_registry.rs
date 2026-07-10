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
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::agent_sessions::{AgentSession, AgentStatus, CliSource, SessionEvent, SessionOrigin};
use tokio::sync::Mutex;

/// Top-level key under `_meta` reserved for our extension. ACP lets
/// vendors pile arbitrary keys into `_meta`; we sit under exactly one
/// namespace so anyone else's `_meta` payload survives a round-trip
/// through master untouched.
pub const WTA_META_NAMESPACE: &str = "wta";

/// The subset of `_meta.wta` we read/write today. A struct (rather than
/// just shipping `pane_session_id: Option<String>` directly) so that
/// future fields (titles, owner_tab_id, etc.) can join without
/// touching every call site.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WtaMeta {
    pub pane_session_id: Option<String>,
    /// The WT tab StableId (`--owner-tab-id`) of the agent pane that
    /// owns this session. Carried so master can address per-tab events
    /// (notably `restart_agent_pane` on helper crash recovery) by the
    /// same StableId C++ routes every other per-tab event with. `None`
    /// for non-agent-pane helpers / legacy callers.
    pub owner_tab_id: Option<String>,
}

impl WtaMeta {
    pub fn is_empty(&self) -> bool {
        self.pane_session_id.is_none() && self.owner_tab_id.is_none()
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
    WtaMeta {
        pane_session_id: obj
            .get("pane_session_id")
            .and_then(|v| v.as_str())
            .map(String::from),
        owner_tab_id: obj
            .get("owner_tab_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from),
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
    let map = meta.get_or_insert_with(serde_json::Map::new);
    let mut wta_obj = serde_json::Map::new();
    if let Some(pid) = &wta.pane_session_id {
        wta_obj.insert(
            "pane_session_id".to_string(),
            serde_json::Value::String(pid.clone()),
        );
    }
    if let Some(tab) = &wta.owner_tab_id {
        wta_obj.insert(
            "owner_tab_id".to_string(),
            serde_json::Value::String(tab.clone()),
        );
    }
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

/// Wire payload for [`INTELLTERM_METHOD_SESSION_REMOVED`].
///
/// We only need the session id — helpers look the row up locally to
/// retrieve cwd / pane_session_id before dropping it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SessionRemovedParams {
    pub session_id: acp::schema::v1::SessionId,
}


#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SessionsChangedParams {}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SessionsListParams {
    /// When true, master re-scans the on-disk historical session logs
    /// (`load_for_cli`) and upserts them into the registry before answering —
    /// the F5 refresh path. `#[serde(default)]` keeps old empty `{}` params
    /// deserializing as false, so the periodic 5s poll and view-open stay on
    /// the cheap snapshot-only path.
    #[serde(default)]
    pub rescan: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SessionsListResponse {
    pub sessions: Vec<SessionInfo>,
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
pub fn build_session_removed_notification(sid: &acp::schema::v1::SessionId) -> acp::schema::v1::ExtNotification {
    let params = SessionRemovedParams {
        session_id: sid.clone(),
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

/// Build an `ExtRequest` for `intellterm.wta/sessions/list`. `rescan` asks
/// master to re-load the on-disk historical session logs before answering.
pub fn build_sessions_list_request(rescan: bool) -> acp::schema::v1::ExtRequest {
    let json = serde_json::to_string(&SessionsListParams { rescan })
        .expect("SessionsListParams is trivially serializable");
    let raw = serde_json::value::RawValue::from_string(json)
        .expect("serde_json::to_string always produces valid JSON");
    acp::schema::v1::ExtRequest::new(INTELLTERM_METHOD_SESSIONS_LIST, Arc::from(raw))
}

pub fn parse_sessions_list_params(
    raw: &serde_json::value::RawValue,
) -> Result<SessionsListParams, serde_json::Error> {
    serde_json::from_str::<SessionsListParams>(raw.get())
}

pub fn build_sessions_list_response(
    sessions: Vec<SessionInfo>,
) -> Box<serde_json::value::RawValue> {
    let response = SessionsListResponse { sessions };
    serde_json::value::to_raw_value(&response)
        .expect("SessionsListResponse serialization is infallible for owned data")
}

pub fn parse_sessions_list_response(
    raw: &serde_json::value::RawValue,
) -> Result<SessionsListResponse, serde_json::Error> {
    serde_json::from_str::<SessionsListResponse>(raw.get())
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
    SessionRemoved(acp::schema::v1::SessionId),
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
            Ok(p) => WtaExtNotification::SessionRemoved(p.session_id),
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
fn ext_method_matches(inbound: &str, const_with_underscore: &str) -> bool {
    inbound == const_with_underscore
        || Some(inbound) == const_with_underscore.strip_prefix('_')
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
    /// `_intellterm.wta/session_hook` — a real per-session hook event.
    SessionHook(crate::agent_sessions::SessionEvent),
    /// `_intellterm.wta/session_born_bound` — a #266 binding-only registration
    /// (same body as `SessionHook`, distinct method → binding-only semantics).
    SessionBornBound(crate::agent_sessions::SessionEvent),
    /// `_intellterm.wta/session_resume_dispatched` — optimistic resume flip.
    SessionResumeDispatched(SessionResumeDispatchedParams),
    /// `_intellterm.wta/session_focus` — focus + typed focus result.
    SessionFocus(SessionFocusParams),
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
    } else if ext_method_matches(&req.method, INTELLTERM_METHOD_SESSION_HOOK) {
        decode!(SessionHook, parse_session_hook_params)
    } else if ext_method_matches(&req.method, INTELLTERM_METHOD_SESSION_BORN_BOUND) {
        decode!(SessionBornBound, parse_session_hook_params)
    } else if ext_method_matches(&req.method, INTELLTERM_METHOD_SESSION_RESUME_DISPATCHED) {
        decode!(SessionResumeDispatched, parse_session_resume_dispatched_params)
    } else if ext_method_matches(&req.method, INTELLTERM_METHOD_SESSION_FOCUS) {
        decode!(SessionFocus, parse_session_focus_params)
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

pub fn build_session_resume_dispatched_request(sid: &acp::schema::v1::SessionId) -> acp::schema::v1::ExtRequest {
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

pub fn build_session_focus_request(sid: &acp::schema::v1::SessionId) -> acp::schema::v1::ExtRequest {
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
pub fn build_focus_session_request(sid: &acp::schema::v1::SessionId) -> acp::schema::v1::ExtRequest {
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
                "Codex"  | "codex"  => Self::Codex,
                "Copilot" | "copilot" => Self::Copilot,
                "Gemini" | "gemini" => Self::Gemini,
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
    ToolStarting {
        key: crate::agent_sessions::AgentKey,
        tool_name: String,
    },
    ToolCompleted {
        key: crate::agent_sessions::AgentKey,
    },
    Notification {
        key: crate::agent_sessions::AgentKey,
        message: String,
    },
    SessionStopped {
        key: crate::agent_sessions::AgentKey,
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
            SessionEvent::ToolStarting { key, tool_name } => Self::ToolStarting {
                key: key.clone(),
                tool_name: tool_name.clone(),
            },
            SessionEvent::ToolCompleted { key } => Self::ToolCompleted { key: key.clone() },
            SessionEvent::Notification { key, message } => Self::Notification {
                key: key.clone(),
                message: message.clone(),
            },
            SessionEvent::SessionStopped { key, reason } => Self::SessionStopped {
                key: key.clone(),
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
            SessionHookParams::ToolStarting { key, tool_name } => {
                Self::ToolStarting { key, tool_name }
            }
            SessionHookParams::ToolCompleted { key } => Self::ToolCompleted { key },
            SessionHookParams::Notification { key, message } => {
                Self::Notification { key, message }
            }
            SessionHookParams::SessionStopped { key, reason } => {
                Self::SessionStopped { key, reason }
            }
            SessionHookParams::ConnectionFailed {
                pane_session_id,
                reason,
            } => Self::ConnectionFailed {
                pane_session_id,
                reason,
            },
            SessionHookParams::PaneClosed { pane_session_id } => Self::PaneClosed { pane_session_id },
            SessionHookParams::ResumeDispatched { key } => Self::ResumeDispatched { key },
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
pub fn build_session_hook_request(event: &crate::agent_sessions::SessionEvent) -> acp::schema::v1::ExtRequest {
    let params = SessionHookParams::from(event);
    let json = serde_json::to_string(&params).expect("SessionHookParams serialization is infallible");
    let raw = serde_json::value::RawValue::from_string(json)
        .expect("serde_json::to_string always produces valid JSON");
    acp::schema::v1::ExtRequest::new(INTELLTERM_METHOD_SESSION_HOOK, Arc::from(raw))
}

/// Build a #266 *born-bound* registration ExtRequest. Identical body to
/// [`build_session_hook_request`] but a distinct method
/// ([`INTELLTERM_METHOD_SESSION_BORN_BOUND`]) so the master treats it as
/// binding-only (watcher may still supply status), not hook-owned.
pub fn build_born_bound_request(event: &crate::agent_sessions::SessionEvent) -> acp::schema::v1::ExtRequest {
    let params = SessionHookParams::from(event);
    let json = serde_json::to_string(&params).expect("SessionHookParams serialization is infallible");
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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SessionInfo {
    pub session_id: acp::schema::v1::SessionId,
    pub cwd: PathBuf,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub pane_session_id: Option<String>,
    #[serde(default)]
    pub status: Option<AgentStatus>,
    #[serde(default)]
    pub cli_source: Option<CliSource>,
    #[serde(default)]
    pub current_tool: Option<String>,
    #[serde(default)]
    pub attention_reason: Option<String>,
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
    pub location: crate::agent_sessions::SessionLocation,
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
}

impl SessionInfo {
    /// Convenience constructor for tests and call sites that only have the
    /// mandatory fields. Optional fields default to `None`.
    pub fn new(session_id: acp::schema::v1::SessionId, cwd: PathBuf) -> Self {
        Self {
            session_id,
            cwd,
            title: None,
            updated_at: None,
            pane_session_id: None,
            status: None,
            cli_source: None,
            current_tool: None,
            attention_reason: None,
            last_activity_at_ms: None,
            origin: None,
            last_error: None,
            location: crate::agent_sessions::SessionLocation::Host,
            bound_pid: None,
        }
    }

    /// Builder-style setter for `pane_session_id`, useful in tests and at
    /// `new_session` time when the helper hands us a `_meta.wta` payload.
    pub fn with_pane_session_id(mut self, pane_session_id: impl Into<String>) -> Self {
        self.pane_session_id = Some(pane_session_id.into());
        self
    }
}

/// Convert an `AgentSession` (the helper-side representation, also used
/// by the disk scanner `history_loader::load_all`) into a `SessionInfo`
/// for upsert into master's registry.
///
/// Used by master at startup to seed the registry with historical
/// rows scanned from `~/.copilot/`, `~/.claude/`, `~/.gemini/` so
/// `wta sessions list` and session management viewers see the full set, not just live
/// sessions created via `session/new` after master booted.
pub fn agent_session_to_session_info(s: &AgentSession) -> SessionInfo {
    let last_activity_at_ms = s
        .last_activity_at
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64);
    SessionInfo {
        session_id: acp::schema::v1::SessionId::new(s.key.clone()),
        cwd: s.cwd.clone(),
        title: if s.title.is_empty() { None } else { Some(s.title.clone()) },
        updated_at: None,
        pane_session_id: s.pane_session_id.clone(),
        status: Some(s.status.clone()),
        cli_source: Some(s.cli_source.clone()),
        current_tool: s.current_tool.clone(),
        attention_reason: s.attention_reason.clone(),
        last_activity_at_ms,
        origin: Some(s.origin.clone()),
        last_error: s.last_error.clone(),
        location: s.location.clone(),
        // History-scan / helper-sourced rows have no bound pid; only the
        // file watcher's bind step populates it.
        bound_pid: None,
    }
}

/// Read/write surface over the live-session set. Both master and helper
/// hold an `Arc<dyn SessionRegistry>` so unit tests can swap in mocks
/// without spinning up a real pipe. In production both sides use
/// `InMemoryRegistry`.
#[allow(dead_code)] // Task B wires hook RPCs into these reducer methods.
#[async_trait::async_trait]
pub trait SessionRegistry: Send + Sync {
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
        if self.lookup(&info.session_id).await.is_none() {
            self.upsert(info).await;
        }
    }

    /// Remove the row for `sid`. Returns the prior value if any (the master
    /// uses this both for routing teardown and to know what to broadcast
    /// in `session_removed` ext-notifications).
    async fn remove(&self, sid: &acp::schema::v1::SessionId) -> Option<SessionInfo>;

    /// Atomically remove the row for `sid` **only if** the current entry still
    /// satisfies `predicate`. The check + remove happen under one lock, so a
    /// concurrent `apply_event` that flips the row live between a caller's
    /// snapshot and this call cannot be clobbered by a stale-snapshot remove.
    /// Returns the removed row, or `None` if absent or rejected by `predicate`.
    async fn remove_if(
        &self,
        sid: &acp::schema::v1::SessionId,
        predicate: &(dyn for<'a> Fn(&'a SessionInfo) -> bool + Sync),
    ) -> Option<SessionInfo>;

    /// Fetch a clone of the current entry for `sid`. Returns `None` if the
    /// session isn't alive (or hasn't been mirrored yet on the helper side).
    async fn lookup(&self, sid: &acp::schema::v1::SessionId) -> Option<SessionInfo>;

    /// Snapshot the full set. Order is unspecified — callers that need a
    /// stable order should sort by `session_id` themselves. The clone is
    /// cheap because `SessionInfo` is small (`Arc<str>` for the id).
    async fn snapshot(&self) -> Vec<SessionInfo>;

    /// Apply a helper-observed session event to the master-side reducer state.
    async fn apply_event(&self, ev: SessionEvent) -> bool;

    /// Update origin metadata on an existing row.
    async fn set_origin(&self, sid: &acp::schema::v1::SessionId, origin: SessionOrigin) -> bool;

    /// Atomically flip a Historical row to Idle for resume dispatch (Task C).
    /// Returns Some((flipped, current_status_label)) where `flipped` is true
    /// only when the row was Historical and was transitioned this call.
    async fn mark_resume_dispatched(&self, sid: &acp::schema::v1::SessionId) -> Option<(bool, String)>;

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
}

/// Production implementation. Uses `tokio::sync::Mutex` for parity with the
/// existing master state; the critical sections are all sync HashMap ops
/// so a future sync-lock conversion is a mechanical swap.
#[derive(Default)]
struct RegistryState {
    sessions: HashMap<acp::schema::v1::SessionId, SessionInfo>,
    active_by_pane: HashMap<String, acp::schema::v1::SessionId>,
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
    let cwd_leaf = info
        .cwd
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    match info.title.as_deref() {
        None | Some("") => true,
        Some(t) => t == cwd_leaf,
    }
}

/// The literal heading the delegate `?<prompt>` flow injects into the prompt it
/// bakes into a freshly launched agent CLI: `"<prompt>\n\n## Terminal Context
/// (pane <id>)\n```…```"` (built in `main.rs`, which references this same
/// constant so the two can't drift).
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
/// injected terminal context (pane GUID included), and adopting it would lock
/// the row out of the later upgrade to the CLI's real summary name (an echoed
/// title is non-synthetic, so `refresh_synthetic_titles_from` would skip it
/// forever). Callers drop it so the row stays synthetic and keeps upgrading.
pub(crate) fn title_is_injected_context_echo(title: &str) -> bool {
    title.contains(TERMINAL_CONTEXT_TITLE_MARKER)
}

#[async_trait::async_trait]
impl SessionRegistry for InMemoryRegistry {
    async fn upsert(&self, info: SessionInfo) {
        let mut guard = self.inner.lock().await;
        upsert_locked(&mut guard, info);
    }

    async fn upsert_if_absent(&self, info: SessionInfo) {
        let mut guard = self.inner.lock().await;
        if !guard.sessions.contains_key(&info.session_id) {
            upsert_locked(&mut guard, info);
        }
    }

    async fn remove(&self, sid: &acp::schema::v1::SessionId) -> Option<SessionInfo> {
        let mut guard = self.inner.lock().await;
        remove_locked(&mut guard, sid)
    }

    async fn remove_if(
        &self,
        sid: &acp::schema::v1::SessionId,
        predicate: &(dyn for<'a> Fn(&'a SessionInfo) -> bool + Sync),
    ) -> Option<SessionInfo> {
        let mut guard = self.inner.lock().await;
        let matches = guard
            .sessions
            .get(sid)
            .map(|cur| predicate(cur))
            .unwrap_or(false);
        if matches {
            remove_locked(&mut guard, sid)
        } else {
            None
        }
    }

    async fn lookup(&self, sid: &acp::schema::v1::SessionId) -> Option<SessionInfo> {
        let guard = self.inner.lock().await;
        guard.sessions.get(sid).cloned()
    }

    async fn snapshot(&self) -> Vec<SessionInfo> {
        let guard = self.inner.lock().await;
        guard.sessions.values().cloned().collect()
    }

    async fn apply_event(&self, ev: SessionEvent) -> bool {
        let mut guard = self.inner.lock().await;
        apply_event_locked(&mut guard, ev)
    }

    async fn set_origin(&self, sid: &acp::schema::v1::SessionId, origin: SessionOrigin) -> bool {
        let mut guard = self.inner.lock().await;
        let Some(entry) = guard.sessions.get_mut(sid) else {
            return false;
        };
        if entry.origin.as_ref() == Some(&origin) {
            return false;
        }
        entry.origin = Some(origin);
        true
    }

    async fn mark_resume_dispatched(&self, sid: &acp::schema::v1::SessionId) -> Option<(bool, String)> {
        let mut guard = self.inner.lock().await;
        let row = guard.sessions.get_mut(sid)?;
        let current_label = match &row.status {
            Some(s) => format!("{:?}", s),
            None => "Idle".to_string(),
        };
        if matches!(row.status, Some(AgentStatus::Historical)) {
            row.status = Some(AgentStatus::Idle);
            row.last_activity_at_ms = Some(now_ms());
            Some((true, "Idle".to_string()))
        } else {
            Some((false, current_label))
        }
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
        let Some(entry) = guard.sessions.get_mut(sid) else {
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
}

#[allow(dead_code)] // Used through apply_event once Task B forwards hook events.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn pane_key(pane_session_id: &str) -> String {
    pane_session_id.to_ascii_lowercase()
}

fn upsert_locked(state: &mut RegistryState, info: SessionInfo) {
    if let Some(old) = state.sessions.get(&info.session_id) {
        if let Some(old_pane) = old.pane_session_id.as_deref() {
            state.active_by_pane.remove(&pane_key(old_pane));
        }
    }
    if let Some(pane) = info.pane_session_id.as_deref() {
        if !pane.is_empty() {
            state.active_by_pane.insert(pane_key(pane), info.session_id.clone());
        }
    }
    state.sessions.insert(info.session_id.clone(), info);
}

fn remove_locked(state: &mut RegistryState, sid: &acp::schema::v1::SessionId) -> Option<SessionInfo> {
    let removed = state.sessions.remove(sid);
    if let Some(info) = &removed {
        if let Some(pane) = info.pane_session_id.as_deref() {
            state.active_by_pane.remove(&pane_key(pane));
        }
    }
    removed
}

#[allow(dead_code)] // Used through apply_event once Task B forwards hook events.
fn end_entry(state: &mut RegistryState, sid: &acp::schema::v1::SessionId, now: u64) -> bool {
    let Some(entry) = state.sessions.get_mut(sid) else {
        return false;
    };
    entry.status = Some(AgentStatus::Ended);
    if let Some(pane) = entry.pane_session_id.take() {
        state.active_by_pane.remove(&pane_key(&pane));
    }
    entry.current_tool = None;
    entry.attention_reason = None;
    entry.last_activity_at_ms = Some(now);
    true
}

#[allow(dead_code)] // Task B calls this via SessionRegistry::apply_event.
fn apply_event_locked(state: &mut RegistryState, ev: SessionEvent) -> bool {
    let now = now_ms();
    let ev = match ev {
        SessionEvent::SessionStarted {
            key,
            cli_source,
            pane_session_id,
            cwd,
            title,
        } => SessionEvent::SessionStarted {
            key,
            cli_source,
            pane_session_id: pane_key(&pane_session_id),
            cwd,
            title,
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
        SessionEvent::SessionStarted { key, cli_source, pane_session_id, cwd, title } => {
            let sid = acp::schema::v1::SessionId::new(key.clone());
            let pane_known = !pane_session_id.is_empty();

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
            let is_protected_agent_pane = state
                .sessions
                .get(&sid)
                .map(|s| {
                    s.origin == Some(SessionOrigin::AgentPane)
                        && s.pane_session_id.is_some()
                })
                .unwrap_or(false);
            if is_protected_agent_pane {
                // Skip the pane mutation entirely. Update only the
                // activity heartbeat so master still knows the session
                // is alive.
                let entry = state.sessions.get_mut(&sid).expect("just verified by lookup");
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

            if pane_known {
                if let Some(prev_sid) = state.active_by_pane.get(&pane_session_id).cloned() {
                    if prev_sid != sid {
                        let _ = end_entry(state, &prev_sid, now);
                    }
                }
            }

            let is_new_entry = !state.sessions.contains_key(&sid);
            let entry = state
                .sessions
                .entry(sid.clone())
                .or_insert_with(|| SessionInfo::new(sid.clone(), cwd.clone()));
            if let Some(old_pane) = entry.pane_session_id.take() {
                if old_pane != pane_session_id {
                    state.active_by_pane.remove(&pane_key(&old_pane));
                }
            }
            entry.cwd = cwd;
            if !title.is_empty() {
                entry.title = Some(title);
            }
            entry.cli_source = Some(cli_source);
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
                entry.current_tool = None;
            }
            entry.last_activity_at_ms = Some(now);
            if pane_known {
                entry.pane_session_id = Some(pane_session_id.clone());
                state.active_by_pane.insert(pane_session_id, sid);
            } else {
                entry.pane_session_id = None;
            }
            true
        }
        SessionEvent::ToolStarting { key, tool_name } => {
            let sid = acp::schema::v1::SessionId::new(key);
            let Some(entry) = state.sessions.get_mut(&sid) else { return false; };
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
            if matches!(entry.status, Some(AgentStatus::Ended | AgentStatus::Historical)) {
                return false;
            }
            entry.status = Some(AgentStatus::Working);
            entry.current_tool = Some(tool_name);
            entry.last_activity_at_ms = Some(now);
            true
        }
        SessionEvent::ToolCompleted { key } => {
            let sid = acp::schema::v1::SessionId::new(key);
            let Some(entry) = state.sessions.get_mut(&sid) else { return false; };
            // Same resurrection guard as ToolStarting — a stale
            // ToolCompleted on an Ended row would either be a no-op or
            // (worse) drop current_tool that some other resurrected
            // state depended on. Treat it as a no-op for terminal rows.
            if matches!(entry.status, Some(AgentStatus::Ended | AgentStatus::Historical)) {
                return false;
            }
            if matches!(entry.status, Some(AgentStatus::Working | AgentStatus::Attention)) {
                entry.status = Some(AgentStatus::Idle);
                entry.attention_reason = None;
            }
            entry.current_tool = None;
            entry.last_activity_at_ms = Some(now);
            true
        }
        SessionEvent::Notification { key, message } => {
            let sid = acp::schema::v1::SessionId::new(key);
            let Some(entry) = state.sessions.get_mut(&sid) else { return false; };
            // Same resurrection guard — a stale Notification on an
            // Ended row would flip it to Attention and surface a
            // spurious "agent needs input" prompt for a session that
            // has already been ended.
            if matches!(entry.status, Some(AgentStatus::Ended | AgentStatus::Historical)) {
                return false;
            }
            entry.status = Some(AgentStatus::Attention);
            entry.attention_reason = Some(message);
            entry.last_activity_at_ms = Some(now);
            true
        }
        SessionEvent::SessionStopped { key, reason } => {
            let sid = acp::schema::v1::SessionId::new(key);
            let reason_keeps_session_alive = reason == "complete";
            let pane_still_live = state.sessions.get(&sid)
                .and_then(|s| s.pane_session_id.as_deref())
                .map(|p| state.active_by_pane.get(&pane_key(p)) == Some(&sid))
                .unwrap_or(false);
            let is_agent_pane_session = state.sessions.get(&sid)
                .map(|s| s.origin == Some(SessionOrigin::AgentPane))
                .unwrap_or(false);
            let Some(entry) = state.sessions.get_mut(&sid) else { return false; };
            if is_agent_pane_session && pane_still_live && reason_keeps_session_alive {
                entry.status = Some(AgentStatus::Idle);
            } else {
                entry.status = Some(AgentStatus::Ended);
                if let Some(pane) = entry.pane_session_id.take() {
                    state.active_by_pane.remove(&pane_key(&pane));
                }
            }
            entry.current_tool = None;
            entry.attention_reason = None;
            entry.last_activity_at_ms = Some(now);
            true
        }
        SessionEvent::PaneClosed { pane_session_id } => {
            let Some(sid) = state.active_by_pane.remove(&pane_session_id) else { return false; };
            let Some(entry) = state.sessions.get_mut(&sid) else { return false; };
            entry.status = Some(AgentStatus::Ended);
            entry.pane_session_id = None;
            entry.current_tool = None;
            entry.attention_reason = None;
            entry.last_activity_at_ms = Some(now);
            true
        }
        SessionEvent::ConnectionFailed { pane_session_id, reason } => {
            let Some(sid) = state.active_by_pane.get(&pane_session_id).cloned() else { return false; };
            let Some(entry) = state.sessions.get_mut(&sid) else { return false; };
            entry.status = Some(AgentStatus::Error);
            entry.last_error = Some(reason);
            entry.last_activity_at_ms = Some(now);
            true
        }
        SessionEvent::ResumeDispatched { key } => {
            let sid = acp::schema::v1::SessionId::new(key);
            let Some(entry) = state.sessions.get_mut(&sid) else { return false; };
            if matches!(entry.status, Some(AgentStatus::Historical | AgentStatus::Ended)) {
                entry.status = Some(AgentStatus::Idle);
                entry.last_activity_at_ms = Some(now);
                return true;
            }
            false
        }
        SessionEvent::ResumePaneAssigned { key, pane_session_id } => {
            let sid = acp::schema::v1::SessionId::new(key);
            if let Some(prev_sid) = state.active_by_pane.get(&pane_session_id).cloned() {
                if prev_sid != sid {
                    let _ = end_entry(state, &prev_sid, now);
                }
            }
            let Some(entry) = state.sessions.get_mut(&sid) else { return false; };
            if entry.pane_session_id.as_deref() == Some(pane_session_id.as_str()) {
                return false;
            }
            if let Some(old_pane) = entry.pane_session_id.take() {
                if old_pane != pane_session_id {
                    state.active_by_pane.remove(&pane_key(&old_pane));
                }
            }
            entry.pane_session_id = Some(pane_session_id.clone());
            entry.last_activity_at_ms = Some(now);
            state.active_by_pane.insert(pane_session_id, sid);
            true
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
        reg.remove(&old.session_id).await;
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
        WtaExtNotification::SessionRemoved(sid) => {
            reg.remove(sid).await;
        }
        WtaExtNotification::SessionsChanged => {}
        // Unknown / MalformedParams: caller's job to log; never panic
        // and never mutate the registry. A future master may broadcast
        // notifications we don't recognise — silently ignoring them
        // keeps the helper forward-compatible.
        WtaExtNotification::SessionsChanged
        | WtaExtNotification::Unknown
        | WtaExtNotification::MalformedParams { .. } => {}
    }
    parsed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str, pane: Option<&str>) -> SessionInfo {
        let mut s = SessionInfo::new(acp::schema::v1::SessionId::new(id.to_string()), PathBuf::from("/tmp"));
        if let Some(p) = pane {
            s = s.with_pane_session_id(p.to_string());
        }
        s
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
            .remove_if(&acp::schema::v1::SessionId::new("keep".to_string()), &|_| false)
            .await;
        assert!(kept.is_none(), "remove_if must not remove when predicate is false");
        assert!(
            reg.lookup(&acp::schema::v1::SessionId::new("keep".to_string()))
                .await
                .is_some(),
            "rejected row must still be present"
        );

        // Predicate accepts -> row removed, returns the prior value.
        let dropped = reg
            .remove_if(&acp::schema::v1::SessionId::new("drop".to_string()), &|_| true)
            .await;
        assert!(dropped.is_some(), "remove_if must remove when predicate is true");
        assert!(
            reg.lookup(&acp::schema::v1::SessionId::new("drop".to_string()))
                .await
                .is_none(),
            "accepted row must be gone"
        );

        // Missing id -> None even with an always-true predicate.
        assert!(
            reg.remove_if(&acp::schema::v1::SessionId::new("missing".to_string()), &|_| true)
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
        let mut s = SessionInfo::new(acp::schema::v1::SessionId::new(id.to_string()), PathBuf::from(cwd));
        s.title = title.map(str::to_owned);
        s
    }

    #[test]
    fn title_is_synthetic_detects_missing_empty_and_cwd_basename() {
        assert!(title_is_synthetic(&info_with("s-none", "/repo/proj", None)));
        assert!(title_is_synthetic(&info_with("s-empty", "/repo/proj", Some(""))));
        assert!(title_is_synthetic(&info_with("s-leaf", "/repo/proj", Some("proj"))));
        assert!(!title_is_synthetic(&info_with("s-real", "/repo/proj", Some("Real Title"))));
    }

    #[test]
    fn title_is_injected_context_echo_detects_delegate_marker_only() {
        // Mirrors the `?<prompt>` prompt built in `main.rs` from
        // `TERMINAL_CONTEXT_TITLE_MARKER`: an agent CLI can echo this whole first
        // user message back as a `session/list` title before it generates a real
        // summary. Such an echo must be dropped (see `host_titles_via_acp`) so the
        // born-bound row stays synthetic and keeps upgrading — the counterpart
        // behaviour is covered by `refresh_synthetic_titles_from_skips_when_id_absent`.
        let echo = format!(
            "hi test\n\n{}8A9B4ABA-BEB4-4F94-B0D3-55569420B902)\n```\nPowerShell 7.6.3\n```",
            TERMINAL_CONTEXT_TITLE_MARKER
        );
        assert!(title_is_injected_context_echo(&echo));
        // A real CLI-generated summary, the bare user prompt, and empty never
        // contain the injected marker.
        assert!(!title_is_injected_context_echo("PowerShell Terminal Session"));
        assert!(!title_is_injected_context_echo("hi test"));
        assert!(!title_is_injected_context_echo(""));
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
            !reg.upgrade_title_if_synthetic(&sid, "Different Title").await
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
        reg.upsert(info_with("s1", "/repo/proj", Some("Same"))).await;
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
            WtaExtNotification::SessionRemoved(parsed) => assert_eq!(parsed, sid),
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
        let ext = acp::schema::v1::ExtNotification::new(INTELLTERM_METHOD_SESSION_ADDED, Arc::from(raw));
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
            cwd: PathBuf::from("C:\\repo"),
            title: Some("fix the build".into()),
            updated_at: Some("2026-05-27T12:34:56Z".into()),
            pane_session_id: Some("pane-1".into()),
            status: Some(crate::agent_sessions::AgentStatus::Attention),
            cli_source: Some(crate::agent_sessions::CliSource::Copilot),
            current_tool: Some("ask_user".into()),
            attention_reason: Some("Need approval".into()),
            last_activity_at_ms: Some(1717012345678),
            origin: Some(crate::agent_sessions::SessionOrigin::AgentPane),
            last_error: Some("previous failure".into()),
            location: crate::agent_sessions::SessionLocation::Host,
            bound_pid: None,
        };

        let json = serde_json::to_string(&row).expect("serialize SessionInfo");
        let parsed: SessionInfo = serde_json::from_str(&json).expect("deserialize SessionInfo");

        assert_eq!(parsed, row);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["status"], "Attention");
        assert_eq!(value["cli_source"], "Copilot");
        assert_eq!(value["origin"], "AgentPane");
        assert_eq!(value["last_activity_at_ms"], 1717012345678u64);
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
    fn build_sessions_changed_notification_has_empty_params() {
        let ext = build_sessions_changed_notification();
        assert_eq!(&*ext.method, INTELLTERM_METHOD_SESSIONS_CHANGED);
        assert_eq!(ext.params.get(), "{}");
        assert!(matches!(parse_ext_notification(&ext), WtaExtNotification::SessionsChanged));
    }

    #[test]
    fn sessions_list_response_round_trips_rows() {
        let row = SessionInfo {
            session_id: acp::schema::v1::SessionId::new("sess-list".to_string()),
            cwd: PathBuf::from("C:\\repo"),
            title: Some("title".into()),
            updated_at: Some("2026-05-27T12:34:56Z".into()),
            pane_session_id: Some("pane-list".into()),
            status: Some(crate::agent_sessions::AgentStatus::Idle),
            cli_source: Some(crate::agent_sessions::CliSource::Claude),
            current_tool: None,
            attention_reason: None,
            last_activity_at_ms: Some(123),
            origin: Some(crate::agent_sessions::SessionOrigin::AgentPane),
            last_error: None,
            location: crate::agent_sessions::SessionLocation::Host,
            bound_pid: None,
        };
        let raw = build_sessions_list_response(vec![row.clone()]);
        let parsed = parse_sessions_list_response(&raw).expect("response parses");
        assert_eq!(parsed.sessions, vec![row]);
    }

    #[tokio::test]
    async fn master_reducer_session_started_creates_idle_entry_bound_to_pane() {
        let reg = InMemoryRegistry::new();
        let changed = reg.apply_event(crate::agent_sessions::SessionEvent::SessionStarted {
            key: "sid-1".into(),
            cli_source: crate::agent_sessions::CliSource::Claude,
            pane_session_id: "Pane-A".into(),
            cwd: PathBuf::from("C:\\work"),
            title: "claude — work".into(),
        }).await;

        assert!(changed);
        let row = reg.lookup(&acp::schema::v1::SessionId::new("sid-1")).await.unwrap();
        assert_eq!(row.status, Some(crate::agent_sessions::AgentStatus::Idle));
        assert_eq!(row.cli_source, Some(crate::agent_sessions::CliSource::Claude));
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
        }).await;
        reg.apply_event(crate::agent_sessions::SessionEvent::ToolStarting { key: "sid".into(), tool_name: "bash".into() }).await;
        let row = reg.lookup(&acp::schema::v1::SessionId::new("sid")).await.unwrap();
        assert_eq!(row.status, Some(crate::agent_sessions::AgentStatus::Working));
        assert_eq!(row.current_tool.as_deref(), Some("bash"));

        reg.apply_event(crate::agent_sessions::SessionEvent::Notification { key: "sid".into(), message: "approve?".into() }).await;
        let row = reg.lookup(&acp::schema::v1::SessionId::new("sid")).await.unwrap();
        assert_eq!(row.status, Some(crate::agent_sessions::AgentStatus::Attention));
        assert_eq!(row.attention_reason.as_deref(), Some("approve?"));

        reg.apply_event(crate::agent_sessions::SessionEvent::ToolCompleted { key: "sid".into() }).await;
        let row = reg.lookup(&acp::schema::v1::SessionId::new("sid")).await.unwrap();
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
        }).await;
        reg.apply_event(SessionEvent::ToolStarting { key: "sid".into(), tool_name: "prompt".into() }).await;
        assert_eq!(
            reg.lookup(&acp::schema::v1::SessionId::new("sid")).await.unwrap().status,
            Some(AgentStatus::Working),
        );

        // The real session.start arrives later for the SAME key.
        reg.apply_event(SessionEvent::SessionStarted {
            key: "sid".into(),
            cli_source: CliSource::Copilot,
            pane_session_id: "p".into(),
            cwd: PathBuf::from("C:\\x"),
            title: "t".into(),
        }).await;
        assert_eq!(
            reg.lookup(&acp::schema::v1::SessionId::new("sid")).await.unwrap().status,
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
        }).await;
        reg.apply_event(SessionEvent::PaneClosed { pane_session_id: "p".into() }).await;
        assert_eq!(
            reg.lookup(&acp::schema::v1::SessionId::new("sid")).await.unwrap().status,
            Some(AgentStatus::Ended),
        );
        reg.apply_event(SessionEvent::SessionStarted {
            key: "sid".into(),
            cli_source: CliSource::Copilot,
            pane_session_id: "p2".into(),
            cwd: PathBuf::from("C:\\x"),
            title: "t".into(),
        }).await;
        assert_eq!(
            reg.lookup(&acp::schema::v1::SessionId::new("sid")).await.unwrap().status,
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
        }).await;
        reg.set_origin(&acp::schema::v1::SessionId::new("sid"), crate::agent_sessions::SessionOrigin::AgentPane).await;
        reg.apply_event(crate::agent_sessions::SessionEvent::SessionStopped { key: "sid".into(), reason: "user_exit".into() }).await;
        let row = reg.lookup(&acp::schema::v1::SessionId::new("sid")).await.unwrap();
        assert_eq!(row.status, Some(crate::agent_sessions::AgentStatus::Ended));
        assert!(row.pane_session_id.is_none());

        reg.apply_event(crate::agent_sessions::SessionEvent::SessionStarted {
            key: "sid2".into(),
            cli_source: crate::agent_sessions::CliSource::Gemini,
            pane_session_id: "p2".into(),
            cwd: PathBuf::from("C:\\x"),
            title: "t".into(),
        }).await;
        reg.apply_event(crate::agent_sessions::SessionEvent::PaneClosed { pane_session_id: "P2".into() }).await;
        let row = reg.lookup(&acp::schema::v1::SessionId::new("sid2")).await.unwrap();
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
        }).await;
        reg.apply_event(crate::agent_sessions::SessionEvent::ConnectionFailed { pane_session_id: "P".into(), reason: "ECONNRESET".into() }).await;
        let row = reg.lookup(&acp::schema::v1::SessionId::new("sid")).await.unwrap();
        assert_eq!(row.status, Some(crate::agent_sessions::AgentStatus::Error));
        assert_eq!(row.last_error.as_deref(), Some("ECONNRESET"));
        assert_eq!(row.pane_session_id.as_deref(), Some("p"));
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
        }).await;
        reg.apply_event(crate::agent_sessions::SessionEvent::PaneClosed { pane_session_id: "p".into() }).await;

        let changed = reg.apply_event(crate::agent_sessions::SessionEvent::ResumeDispatched { key: "sid".into() }).await;
        let row = reg.lookup(&acp::schema::v1::SessionId::new("sid")).await.unwrap();

        assert!(changed);
        assert_eq!(row.status, Some(crate::agent_sessions::AgentStatus::Idle));
        assert!(row.pane_session_id.is_none());
    }

    #[tokio::test]
    async fn master_reducer_resume_pane_assigned_binds_new_pane() {
        let reg = InMemoryRegistry::new();
        reg.upsert(SessionInfo {
            session_id: acp::schema::v1::SessionId::new("sid".to_string()),
            cwd: PathBuf::from("C:\\x"),
            title: Some("historical".into()),
            updated_at: None,
            pane_session_id: None,
            status: Some(crate::agent_sessions::AgentStatus::Historical),
            cli_source: Some(crate::agent_sessions::CliSource::Gemini),
            current_tool: None,
            attention_reason: None,
            last_activity_at_ms: Some(1),
            origin: Some(crate::agent_sessions::SessionOrigin::AgentPane),
            last_error: None,
            location: crate::agent_sessions::SessionLocation::Host,
            bound_pid: None,
        }).await;
        reg.apply_event(crate::agent_sessions::SessionEvent::ResumeDispatched { key: "sid".into() }).await;

        let changed = reg.apply_event(crate::agent_sessions::SessionEvent::ResumePaneAssigned {
            key: "sid".into(),
            pane_session_id: "New-Pane".into(),
        }).await;
        let row = reg.lookup(&acp::schema::v1::SessionId::new("sid")).await.unwrap();

        assert!(changed);
        assert_eq!(row.status, Some(crate::agent_sessions::AgentStatus::Idle));
        assert_eq!(row.pane_session_id.as_deref(), Some("new-pane"));
    }

    // ─── Task C sessions/list + sessions/changed schemas ───────────

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
        // 3. PowerShell hooks in that shell pane fire SessionStarted
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

        // Now a PowerShell hook fires from a SHELL pane (where the
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
        assert_eq!(row.origin, Some(SessionOrigin::AgentPane), "origin preserved");
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
        let mut info = SessionInfo::new(acp::schema::v1::SessionId::new("ended-sid"), PathBuf::from("/repo"));
        info.status = Some(AgentStatus::Ended);
        reg.upsert(info).await;
        let applied = reg
            .apply_event(SessionEvent::ToolStarting {
                key: "ended-sid".to_string(),
                tool_name: "edit".to_string(),
            })
            .await;
        assert!(!applied, "ToolStarting on Ended row must be a no-op");
        let row = reg.lookup(&acp::schema::v1::SessionId::new("ended-sid")).await.unwrap();
        assert_eq!(row.status, Some(AgentStatus::Ended), "status must stay Ended");
        assert_eq!(row.current_tool, None, "current_tool must not be set on a zombie");
    }

    #[tokio::test]
    async fn tool_starting_still_promotes_live_idle_row_to_working() {
        // Defense-against-overcorrection: the guard above must not
        // accidentally block legitimate Idle -> Working transitions.
        use crate::agent_sessions::{AgentStatus, SessionEvent};
        let reg = InMemoryRegistry::new();
        let mut info = SessionInfo::new(acp::schema::v1::SessionId::new("idle-sid"), PathBuf::from("/repo"));
        info.status = Some(AgentStatus::Idle);
        reg.upsert(info).await;
        let applied = reg
            .apply_event(SessionEvent::ToolStarting {
                key: "idle-sid".to_string(),
                tool_name: "edit".to_string(),
            })
            .await;
        assert!(applied);
        let row = reg.lookup(&acp::schema::v1::SessionId::new("idle-sid")).await.unwrap();
        assert_eq!(row.status, Some(AgentStatus::Working));
        assert_eq!(row.current_tool.as_deref(), Some("edit"));
    }

    #[tokio::test]
    async fn notification_does_not_resurrect_ended_row() {
        use crate::agent_sessions::{AgentStatus, SessionEvent};
        let reg = InMemoryRegistry::new();
        let mut info = SessionInfo::new(acp::schema::v1::SessionId::new("ended-sid"), PathBuf::from("/repo"));
        info.status = Some(AgentStatus::Ended);
        reg.upsert(info).await;
        let applied = reg
            .apply_event(SessionEvent::Notification {
                key: "ended-sid".to_string(),
                message: "needs input".to_string(),
            })
            .await;
        assert!(!applied);
        let row = reg.lookup(&acp::schema::v1::SessionId::new("ended-sid")).await.unwrap();
        assert_eq!(row.status, Some(AgentStatus::Ended));
        assert_eq!(row.attention_reason, None);
    }

    #[tokio::test]
    async fn tool_completed_does_not_resurrect_ended_row() {
        use crate::agent_sessions::{AgentStatus, SessionEvent};
        let reg = InMemoryRegistry::new();
        let mut info = SessionInfo::new(acp::schema::v1::SessionId::new("ended-sid"), PathBuf::from("/repo"));
        info.status = Some(AgentStatus::Ended);
        info.current_tool = Some("edit".to_string()); // pretend it had a tool when ended
        reg.upsert(info).await;
        let applied = reg
            .apply_event(SessionEvent::ToolCompleted {
                key: "ended-sid".to_string(),
            })
            .await;
        assert!(!applied);
        let row = reg.lookup(&acp::schema::v1::SessionId::new("ended-sid")).await.unwrap();
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
        let a_after_handoff = reg.lookup(&acp::schema::v1::SessionId::new("sid-a")).await.unwrap();
        assert_eq!(a_after_handoff.status, Some(AgentStatus::Ended));
        assert_eq!(a_after_handoff.pane_session_id, None);
        // A straggling ToolStarting for A must not resurrect it.
        reg.apply_event(SessionEvent::ToolStarting {
            key: "sid-a".to_string(),
            tool_name: "edit".to_string(),
        })
        .await;
        let a_final = reg.lookup(&acp::schema::v1::SessionId::new("sid-a")).await.unwrap();
        assert_eq!(a_final.status, Some(AgentStatus::Ended),
            "ToolStarting after pane handoff must not flip Ended -> Working (zombie)");
        assert_eq!(a_final.pane_session_id, None,
            "pane binding must stay None after handoff");
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
        let row = reg.lookup(&acp::schema::v1::SessionId::new("codex-fan-in-test")).await.expect("row inserted");
        assert_eq!(row.cli_source, Some(CliSource::Codex));
    }

    #[test]
    fn sessions_list_response_round_trips_session_info_with_typed_fields() {
        let mut info = SessionInfo::new(acp::schema::v1::SessionId::new("sid-1"), PathBuf::from("/repo"));
        info.title = Some("title".to_string());
        info.status = Some(crate::agent_sessions::AgentStatus::Idle);
        info.cli_source = Some(crate::agent_sessions::CliSource::Copilot);
        info.last_activity_at_ms = Some(42);
        let resp = SessionsListResponse {
            sessions: vec![info.clone()],
        };
        let raw = serde_json::value::to_raw_value(&resp).unwrap();
        let parsed = parse_sessions_list_response(&raw).unwrap();
        assert_eq!(parsed.sessions, vec![info]);
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
        let ev = crate::agent_sessions::SessionEvent::ToolCompleted { key: "k".to_string() };
        assert!(matches!(parse_ext_request(build_focus_session_request(&sid)), WtaExtRequest::FocusSession(_)));
        assert!(matches!(parse_ext_request(build_sessions_list_request(true)), WtaExtRequest::SessionsList(p) if p.rescan));
        assert!(matches!(parse_ext_request(build_session_hook_request(&ev)), WtaExtRequest::SessionHook(_)));
        assert!(matches!(parse_ext_request(build_born_bound_request(&ev)), WtaExtRequest::SessionBornBound(_)));
        assert!(matches!(parse_ext_request(build_session_resume_dispatched_request(&sid)), WtaExtRequest::SessionResumeDispatched(_)));
        assert!(matches!(parse_ext_request(build_session_focus_request(&sid)), WtaExtRequest::SessionFocus(_)));
    }

    /// Regression: ACP 1.0 delivers the method name with the leading `_`
    /// already stripped. Each must still decode to its handler variant, not
    /// `ForwardToAgent` (which forwarded to the agent CLI → -32601).
    #[test]
    fn parse_ext_request_decodes_every_method_wire_stripped() {
        let sid = acp::schema::v1::SessionId::new("sid-w".to_string());
        let ev = crate::agent_sessions::SessionEvent::ToolCompleted { key: "k".to_string() };
        assert!(matches!(parse_ext_request(strip_leading_underscore(build_focus_session_request(&sid))), WtaExtRequest::FocusSession(_)));
        assert!(matches!(parse_ext_request(strip_leading_underscore(build_sessions_list_request(false))), WtaExtRequest::SessionsList(_)));
        assert!(matches!(parse_ext_request(strip_leading_underscore(build_session_hook_request(&ev))), WtaExtRequest::SessionHook(_)));
        assert!(matches!(parse_ext_request(strip_leading_underscore(build_born_bound_request(&ev))), WtaExtRequest::SessionBornBound(_)));
        assert!(matches!(parse_ext_request(strip_leading_underscore(build_session_resume_dispatched_request(&sid))), WtaExtRequest::SessionResumeDispatched(_)));
        assert!(matches!(parse_ext_request(strip_leading_underscore(build_session_focus_request(&sid))), WtaExtRequest::SessionFocus(_)));
    }

    #[test]
    fn parse_ext_request_forwards_foreign_methods() {
        let raw = serde_json::value::RawValue::from_string("{}".into()).unwrap();
        let foreign = acp::schema::v1::ExtRequest::new("somebody.else/method", Arc::from(raw));
        assert!(matches!(parse_ext_request(foreign), WtaExtRequest::ForwardToAgent(_)));
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
        use crate::agent_sessions::{AgentSession, AgentStatus, CliSource, SessionLocation, SessionOrigin};
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
        use crate::agent_sessions::{AgentSession, AgentStatus, CliSource, SessionLocation, SessionOrigin};
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
            log_path: None,
            origin: SessionOrigin::Unknown,
            location: SessionLocation::Host,
        };
        let info = agent_session_to_session_info(&s);
        assert_eq!(info.title, None, "empty title should map to None, not Some(\"\")");
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
            SessionEvent::Notification {
                key: "notification".to_string(),
                message: "waiting for input".to_string(),
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
        let parsed = parse_session_hook_params(&request.params)
            .expect("unknown cli_source must round-trip");
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
            &*request.method,
            INTELLTERM_METHOD_SESSION_BORN_BOUND,
            "born-bound must use its own method, not session_hook"
        );
        // Body is the same wire shape, so the master parses it identically.
        let parsed =
            parse_session_hook_params(&request.params).expect("born-bound body must round-trip");
        assert_eq!(parsed, event);
    }

    #[test]
    fn session_hook_cli_source_round_trips_codex() {
        use crate::agent_sessions::CliSource;
        let typed = CliSource::Codex;
        let wire: SessionHookCliSource = (&typed).into();
        assert!(matches!(wire, SessionHookCliSource::Known(ref s) if s == "Codex"),
                "Codex must serialize to Known(\"Codex\"), got {:?}", wire);
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
        };
        let mut meta: Option<acp::schema::v1::Meta> = None;
        inject_wta_meta(&mut meta, &original);
        let parsed = extract_wta_meta(&mut meta);
        assert_eq!(parsed, original, "round-trip preserves data");
        assert!(meta.is_none(), "round-trip ends with empty meta");
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
        use crate::agent_sessions::{AgentSession, AgentStatus, CliSource, SessionLocation, SessionOrigin};

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
            log_path: None,
            origin: SessionOrigin::Unknown,
            location: SessionLocation::Wsl { distro: "Ubuntu".into() },
        };

        // Round-trip through agent_session_to_session_info.
        let info = agent_session_to_session_info(&s);
        assert_eq!(
            info.location,
            SessionLocation::Wsl { distro: "Ubuntu".into() },
            "agent_session_to_session_info must carry the WSL location"
        );

        // Round-trip through JSON serde (the wire boundary).
        let json = serde_json::to_string(&info).unwrap();
        let back: SessionInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.location,
            SessionLocation::Wsl { distro: "Ubuntu".into() },
            "SessionInfo must survive JSON round-trip with Wsl location intact"
        );
    }

    /// A `SessionInfo` JSON object that has no `location` key (e.g. produced
    /// by an older master that predates this field) must deserialize as `Host`
    /// thanks to `#[serde(default)]`.
    #[test]
    fn session_info_without_location_defaults_to_host() {
        use crate::agent_sessions::SessionLocation;

        // Build a valid SessionInfo, serialise it, then strip the location key.
        let info = SessionInfo::new(
            acp::schema::v1::SessionId::new("x"),
            PathBuf::from("/tmp"),
        );
        let mut value: serde_json::Value = serde_json::to_value(&info).unwrap();
        value.as_object_mut().unwrap().remove("location");

        let json = serde_json::to_string(&value).unwrap();
        let parsed: SessionInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed.location,
            SessionLocation::Host,
            "missing location key must default to Host"
        );
    }
}
