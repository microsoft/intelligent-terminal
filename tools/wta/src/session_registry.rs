//! In-memory registry of currently-alive ACP sessions.
//!
//! Used by both the master (truth source) and each helper (a push-updated
//! mirror). Master maintains it as the authoritative view of "which sessions
//! are connected right now"; helpers receive `intellterm.wta/session_added`
//! and `session_removed` ext-notifications and apply them locally so the
//! F2 session-manager Enter routing can decide focus vs. resume with zero
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
}

impl WtaMeta {
    pub fn is_empty(&self) -> bool {
        self.pane_session_id.is_none()
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
pub fn extract_wta_meta(meta: &mut Option<acp::Meta>) -> WtaMeta {
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
pub fn inject_wta_meta(meta: &mut Option<acp::Meta>, wta: &WtaMeta) {
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
pub fn to_acp_session_info(info: &SessionInfo) -> acp::SessionInfo {
    let mut out = acp::SessionInfo::new(info.session_id.clone(), info.cwd.clone());
    out.title = info.title.clone();
    out.updated_at = info.updated_at.clone();
    inject_wta_meta(
        &mut out.meta,
        &WtaMeta {
            pane_session_id: info.pane_session_id.clone(),
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
// helper just deserializes it back into an `acp::SessionInfo`, lifts
// the `_meta.wta.pane_session_id` out via `extract_wta_meta`, and
// upserts into its mirror. Using `acp::SessionInfo` (not our own
// `SessionInfo`) gives the helper a free `cwd`/`title`/`updated_at`
// schema in exchange for the round-trip through wire types.

/// ExtNotification method for "a new session was just bound to a
/// helper inside this master".
pub const INTELLTERM_METHOD_SESSION_ADDED: &str = "intellterm.wta/session_added";

/// ExtNotification method for "a session previously announced via
/// `session_added` is gone" (helper disconnect, load_session rollback,
/// future explicit close).
pub const INTELLTERM_METHOD_SESSION_REMOVED: &str = "intellterm.wta/session_removed";

/// Wire payload for [`INTELLTERM_METHOD_SESSION_REMOVED`].
///
/// We only need the session id — helpers look the row up locally to
/// retrieve cwd / pane_session_id before dropping it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SessionRemovedParams {
    pub session_id: acp::SessionId,
}

/// Build a `session_added` ExtNotification from a registry row.
///
/// Panics only if the serializer fails on `acp::SessionInfo`, which
/// would itself be a bug in the schema crate.
pub fn build_session_added_notification(info: &SessionInfo) -> acp::ExtNotification {
    let wire = to_acp_session_info(info);
    let json = serde_json::to_string(&wire)
        .expect("acp::SessionInfo serialization is infallible for owned data");
    let raw = serde_json::value::RawValue::from_string(json)
        .expect("serde_json::to_string always produces valid JSON");
    acp::ExtNotification::new(INTELLTERM_METHOD_SESSION_ADDED, Arc::from(raw))
}

/// Build a `session_removed` ExtNotification.
pub fn build_session_removed_notification(sid: &acp::SessionId) -> acp::ExtNotification {
    let params = SessionRemovedParams {
        session_id: sid.clone(),
    };
    let json = serde_json::to_string(&params).expect("SessionRemovedParams is trivially serializable");
    let raw = serde_json::value::RawValue::from_string(json)
        .expect("serde_json::to_string always produces valid JSON");
    acp::ExtNotification::new(INTELLTERM_METHOD_SESSION_REMOVED, Arc::from(raw))
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
    SessionRemoved(acp::SessionId),
    /// Not one of ours. Caller should silently ignore.
    Unknown,
    /// Method matched but params failed to parse. Caller should log
    /// and skip rather than tear down the connection — a malformed
    /// notification is a master-side bug, but the helper must remain
    /// usable.
    MalformedParams { method: String, error: String },
}

/// Identify whether an `ExtNotification` is one of ours and, if so,
/// extract the typed payload.
pub fn parse_ext_notification(n: &acp::ExtNotification) -> WtaExtNotification {
    let method: &str = &n.method;
    let raw: &serde_json::value::RawValue = &n.params;
    match method {
        INTELLTERM_METHOD_SESSION_ADDED => match serde_json::from_str::<acp::SessionInfo>(raw.get()) {
            Ok(mut wire) => {
                let wta = extract_wta_meta(&mut wire.meta);
                WtaExtNotification::SessionAdded(SessionInfo {
                    session_id: wire.session_id,
                    cwd: wire.cwd,
                    title: wire.title,
                    updated_at: wire.updated_at,
                    pane_session_id: wta.pane_session_id,
                })
            }
            Err(err) => WtaExtNotification::MalformedParams {
                method: method.to_string(),
                error: err.to_string(),
            },
        },
        INTELLTERM_METHOD_SESSION_REMOVED => {
            match serde_json::from_str::<SessionRemovedParams>(raw.get()) {
                Ok(p) => WtaExtNotification::SessionRemoved(p.session_id),
                Err(err) => WtaExtNotification::MalformedParams {
                    method: method.to_string(),
                    error: err.to_string(),
                },
            }
        }
        _ => WtaExtNotification::Unknown,
    }
}

/// One row in the registry. Mirrors the fields the F2 view needs:
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
///                  into `acp::SessionInfo._meta.wta.pane_session_id` on
///                  the wire so we don't pollute the standard ACP schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub session_id: acp::SessionId,
    pub cwd: PathBuf,
    pub title: Option<String>,
    pub updated_at: Option<String>,
    pub pane_session_id: Option<String>,
}

impl SessionInfo {
    /// Convenience constructor for tests and call sites that only have the
    /// mandatory fields. Optional fields default to `None`.
    pub fn new(session_id: acp::SessionId, cwd: PathBuf) -> Self {
        Self {
            session_id,
            cwd,
            title: None,
            updated_at: None,
            pane_session_id: None,
        }
    }

    /// Builder-style setter for `pane_session_id`, useful in tests and at
    /// `new_session` time when the helper hands us a `_meta.wta` payload.
    pub fn with_pane_session_id(mut self, pane_session_id: impl Into<String>) -> Self {
        self.pane_session_id = Some(pane_session_id.into());
        self
    }
}

/// Read/write surface over the live-session set. Both master and helper
/// hold an `Arc<dyn SessionRegistry>` so unit tests can swap in mocks
/// without spinning up a real pipe. In production both sides use
/// `InMemoryRegistry`.
#[async_trait::async_trait]
pub trait SessionRegistry: Send + Sync {
    /// Insert-or-replace the row for `info.session_id`. Idempotent — calling
    /// twice with the same `session_id` keeps only the latest copy.
    async fn upsert(&self, info: SessionInfo);

    /// Remove the row for `sid`. Returns the prior value if any (the master
    /// uses this both for routing teardown and to know what to broadcast
    /// in `session_removed` ext-notifications).
    async fn remove(&self, sid: &acp::SessionId) -> Option<SessionInfo>;

    /// Fetch a clone of the current entry for `sid`. Returns `None` if the
    /// session isn't alive (or hasn't been mirrored yet on the helper side).
    async fn lookup(&self, sid: &acp::SessionId) -> Option<SessionInfo>;

    /// Snapshot the full set. Order is unspecified — callers that need a
    /// stable order should sort by `session_id` themselves. The clone is
    /// cheap because `SessionInfo` is small (`Arc<str>` for the id).
    async fn snapshot(&self) -> Vec<SessionInfo>;
}

/// Production implementation. Uses `tokio::sync::Mutex` for parity with the
/// existing master state; the critical sections are all sync HashMap ops
/// so a future sync-lock conversion is a mechanical swap.
#[derive(Default)]
pub struct InMemoryRegistry {
    inner: Mutex<HashMap<acp::SessionId, SessionInfo>>,
}

impl InMemoryRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn shared() -> Arc<dyn SessionRegistry> {
        Arc::new(Self::new())
    }
}

#[async_trait::async_trait]
impl SessionRegistry for InMemoryRegistry {
    async fn upsert(&self, info: SessionInfo) {
        let mut guard = self.inner.lock().await;
        guard.insert(info.session_id.clone(), info);
    }

    async fn remove(&self, sid: &acp::SessionId) -> Option<SessionInfo> {
        let mut guard = self.inner.lock().await;
        guard.remove(sid)
    }

    async fn lookup(&self, sid: &acp::SessionId) -> Option<SessionInfo> {
        let guard = self.inner.lock().await;
        guard.get(sid).cloned()
    }

    async fn snapshot(&self) -> Vec<SessionInfo> {
        let guard = self.inner.lock().await;
        guard.values().cloned().collect()
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
/// authoritative". The F2 routing layer reads this flag to avoid
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
    n: &acp::ExtNotification,
) -> WtaExtNotification {
    let parsed = parse_ext_notification(n);
    match &parsed {
        WtaExtNotification::SessionAdded(info) => {
            reg.upsert(info.clone()).await;
        }
        WtaExtNotification::SessionRemoved(sid) => {
            reg.remove(sid).await;
        }
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

    fn info(id: &str, pane: Option<&str>) -> SessionInfo {
        let mut s = SessionInfo::new(acp::SessionId::new(id.to_string()), PathBuf::from("/tmp"));
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
            .lookup(&acp::SessionId::new("sess-1".to_string()))
            .await
            .expect("session present");
        assert_eq!(found, original);
    }

    #[tokio::test]
    async fn lookup_miss_returns_none() {
        let reg = InMemoryRegistry::new();
        assert!(reg
            .lookup(&acp::SessionId::new("missing".to_string()))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn upsert_is_idempotent_and_replaces() {
        let reg = InMemoryRegistry::new();
        reg.upsert(info("sess-1", Some("pane-A"))).await;
        reg.upsert(info("sess-1", Some("pane-B"))).await;
        let found = reg
            .lookup(&acp::SessionId::new("sess-1".to_string()))
            .await
            .unwrap();
        assert_eq!(found.pane_session_id.as_deref(), Some("pane-B"));
        assert_eq!(reg.snapshot().await.len(), 1, "no duplicate rows");
    }

    #[tokio::test]
    async fn remove_returns_prior_and_subsequent_lookup_is_none() {
        let reg = InMemoryRegistry::new();
        reg.upsert(info("sess-1", Some("pane-A"))).await;
        let removed = reg
            .remove(&acp::SessionId::new("sess-1".to_string()))
            .await
            .expect("entry removed");
        assert_eq!(removed.pane_session_id.as_deref(), Some("pane-A"));
        assert!(reg
            .lookup(&acp::SessionId::new("sess-1".to_string()))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn remove_miss_returns_none() {
        let reg = InMemoryRegistry::new();
        assert!(reg
            .remove(&acp::SessionId::new("nope".to_string()))
            .await
            .is_none());
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
        apply_snapshot(&reg, &loaded, vec![info("keep", Some("pb")), info("fresh", None)]).await;
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
            .lookup(&acp::SessionId::new("sess-1".to_string()))
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

    // ── _meta.wta extract / inject ──────────────────────────────────

    fn meta_with(json: serde_json::Value) -> Option<acp::Meta> {
        match json {
            serde_json::Value::Object(map) => Some(map),
            _ => panic!("test bug: meta_with expects a JSON object"),
        }
    }

    #[test]
    fn extract_returns_default_when_meta_is_none() {
        let mut meta: Option<acp::Meta> = None;
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
        let mut meta: Option<acp::Meta> = None;
        inject_wta_meta(&mut meta, &WtaMeta::default());
        assert!(meta.is_none(), "no spurious _meta created");
    }

    #[test]
    fn to_acp_session_info_carries_pane_session_id_in_meta() {
        let row = SessionInfo {
            session_id: acp::SessionId::new("sess-1".to_string()),
            cwd: PathBuf::from("/repo/a"),
            title: Some("hello".into()),
            updated_at: Some("2025-01-01T00:00:00Z".into()),
            pane_session_id: Some("pane-X".into()),
        };
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
            acp::SessionId::new("sess-1".to_string()),
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
        let row = SessionInfo {
            session_id: acp::SessionId::new("sess-77".to_string()),
            cwd: PathBuf::from("/repo/x"),
            title: Some("hello".into()),
            updated_at: Some("2025-01-02T03:04:05Z".into()),
            pane_session_id: Some("pane-ZZ".into()),
        };
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
            acp::SessionId::new("sess-99".to_string()),
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
        let sid = acp::SessionId::new("sess-dead".to_string());
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
        let ext = acp::ExtNotification::new("somebody.else/event", Arc::from(raw));
        assert!(matches!(
            parse_ext_notification(&ext),
            WtaExtNotification::Unknown
        ));
    }

    #[test]
    fn parse_session_added_with_garbage_params_is_malformed_not_panic() {
        let raw = serde_json::value::RawValue::from_string(r#"{"not":"a session"}"#.into()).unwrap();
        let ext = acp::ExtNotification::new(INTELLTERM_METHOD_SESSION_ADDED, Arc::from(raw));
        assert!(matches!(
            parse_ext_notification(&ext),
            WtaExtNotification::MalformedParams { .. }
        ));
    }

    #[test]
    fn inject_creates_meta_when_missing_and_writes_pane_session_id() {
        let mut meta: Option<acp::Meta> = None;
        inject_wta_meta(
            &mut meta,
            &WtaMeta {
                pane_session_id: Some("pane-A".to_string()),
            },
        );
        let map = meta.expect("meta created");
        let wta = map.get("wta").and_then(|v| v.as_object()).unwrap();
        assert_eq!(
            wta.get("pane_session_id")
                .and_then(|v| v.as_str()),
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
        };
        let mut meta: Option<acp::Meta> = None;
        inject_wta_meta(&mut meta, &original);
        let parsed = extract_wta_meta(&mut meta);
        assert_eq!(parsed, original, "round-trip preserves data");
        assert!(meta.is_none(), "round-trip ends with empty meta");
    }

    // ── apply_ext_notification ──────────────────────────────────────

    #[tokio::test]
    async fn apply_ext_notification_upserts_on_session_added() {
        let reg = InMemoryRegistry::new();
        let info = SessionInfo {
            session_id: acp::SessionId::new("sess-1".to_string()),
            cwd: std::path::PathBuf::from("/tmp/x"),
            title: None,
            updated_at: None,
            pane_session_id: Some("pane-1".to_string()),
        };
        let ext = build_session_added_notification(&info);
        let classified = apply_ext_notification(&reg, &ext).await;
        assert!(matches!(classified, WtaExtNotification::SessionAdded(_)));
        let row = reg.lookup(&info.session_id).await.expect("upserted");
        assert_eq!(row.pane_session_id.as_deref(), Some("pane-1"));
    }

    #[tokio::test]
    async fn apply_ext_notification_removes_on_session_removed() {
        let reg = InMemoryRegistry::new();
        let info = SessionInfo {
            session_id: acp::SessionId::new("dies".to_string()),
            cwd: std::path::PathBuf::from("/tmp/y"),
            title: None,
            updated_at: None,
            pane_session_id: None,
        };
        reg.upsert(info.clone()).await;
        let ext = build_session_removed_notification(&info.session_id);
        let classified = apply_ext_notification(&reg, &ext).await;
        assert!(matches!(classified, WtaExtNotification::SessionRemoved(_)));
        assert!(reg.lookup(&info.session_id).await.is_none());
    }

    #[tokio::test]
    async fn apply_ext_notification_is_noop_on_unknown_method() {
        let reg = InMemoryRegistry::new();
        let pre = SessionInfo {
            session_id: acp::SessionId::new("keep".to_string()),
            cwd: std::path::PathBuf::from("/tmp/z"),
            title: None,
            updated_at: None,
            pane_session_id: None,
        };
        reg.upsert(pre.clone()).await;
        let raw = serde_json::value::RawValue::from_string("{}".into()).unwrap();
        let ext = acp::ExtNotification::new(
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
        let ext = acp::ExtNotification::new(
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
}
