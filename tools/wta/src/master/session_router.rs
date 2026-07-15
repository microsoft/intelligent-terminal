// tools/wta/src/master/session_router.rs
//
// The N:1 session multiplexer that sits on the **helper (client) side** of
// `wta-master`. It owns the `SessionId -> helper` map and every routing
// decision that funnels N helper pipes onto the single shared agent-CLI
// connection, so that everything *downstream* of it (`agent_conn`, a
// `conn::ClientLink` = `ConnectionTo<Agent>`) is a plain, standard ACP client
// connection. Keeping this aggregation cohesive here is what makes the
// master->agent boundary a clean seam for future in-process transforms
// (autofix / context / delegate proxies) to hook into — see
// doc/specs/acp-1.0-conductor-migration.md (Phase 1).
//
// This module is a pure routing table: it never touches the session registry,
// the ext-notification broadcast, or crash-recovery metadata. Callers layer
// those concerns on top (e.g. `drop_sessions_for_helper` in `mod.rs` calls
// [`SessionRouter::drop_helper`] and then does `registry.remove` + broadcast).

use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use agent_client_protocol as acp;
use tokio::sync::{mpsc, Mutex};

use crate::protocol::acp::conn;

/// Per-helper notification channel capacity. Sized for bursty chunk
/// streaming during a single agent turn; well above what a healthy
/// helper pipe needs to drain. If it fills up, the helper's pipe is
/// genuinely stuck and we'd rather drop chunks (with a warning) than
/// back-pressure the agent CLI's I/O loop and freeze every other
/// helper sharing this master.
pub(crate) const NOTIF_CHANNEL_CAPACITY: usize = 1024;

/// Opaque identifier for a helper connection. Used in logs only;
/// routing keys off `acp::schema::v1::SessionId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct HelperId(pub(crate) u64);

/// Per-session routing entry. Owned by [`SessionRouter`] and keyed by
/// `acp::schema::v1::SessionId`.
///
/// Two reverse paths share this entry:
///   * `notif_tx`: master's `Client::session_notification` posts here;
///     the helper's `serve_helper` loop drains it and writes back
///     across the pipe.
///   * `forwarder`: master's `Client::request_permission` / `create_terminal`
///     / `terminal_*` / `read_text_file` / `write_text_file` calls
///     directly on this connection. `AgentLink` re-issues each call as an
///     RPC request to the helper.
///
/// `forwarder` is `Option<_>` for one reason only: unit tests below
/// construct routing entries without a real connection. The
/// production path (`new_session` / `load_session`) always sets it
/// to `Some(_)`, and [`SessionRouter::route_for`] treats `None` as a
/// routing bug.
#[derive(Clone)]
pub(crate) struct HelperRoute {
    pub(crate) helper_id: HelperId,
    pub(crate) notif_tx: mpsc::Sender<acp::schema::v1::SessionNotification>,
    pub(crate) forwarder: Option<conn::AgentLink>,
    /// Per-route counter for back-pressure log rate-limiting.
    ///
    /// Chunk-streaming during a single agent turn is high-rate, so if
    /// a helper's pipe stalls and we drop notifications, naively
    /// `warn!`-ing on every drop would flood the log (and add I/O
    /// load right when the system is already strained). Instead the
    /// notification delivery path:
    ///
    ///   * On the FIRST `Full` (`fetch_add` returns 0): emits one
    ///     `warn!` announcing that the helper's queue is backed up.
    ///   * On subsequent `Full`s: silently bumps the counter — the
    ///     summary on recovery covers them.
    ///   * On the first `Ok` after at least one drop (`swap` returns
    ///     >0): emits one `info!` reporting the total dropped chunks
    ///     and that backpressure has cleared.
    ///
    /// This gives operators exactly one log line per stall start and
    /// one per stall end, with the count in between, regardless of
    /// how many chunks were dropped.
    pub(crate) consecutive_drops: Arc<AtomicU64>,
}

/// The N:1 helper-session routing table. All access goes through one async
/// `Mutex`, so callers must never hold it across an `await` that re-enters the
/// router.
///
/// Lock ordering: the `MasterStateInner::registry` doc requires taking this
/// router's lock *before* touching the registry. Every method here releases
/// the lock before returning, and no method awaits anything but the lock, so
/// the router never re-enters itself and can't deadlock against the registry.
#[derive(Default)]
pub(crate) struct SessionRouter {
    map: Mutex<HashMap<acp::schema::v1::SessionId, HelperRoute>>,
}

/// Outcome of a `remove_if_owned`-style cleanup, surfaced so callers/tests can
/// assert which race branch fired.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ClosedCleanup {
    /// The entry still belonged to the snapshotted helper and was removed.
    Removed,
    /// The `SessionId` was rebound to a different helper; left intact.
    Rebound,
    /// The entry was already gone (cleanup raced ahead).
    AlreadyGone,
}

impl SessionRouter {
    pub(crate) fn new() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
        }
    }

    /// Bind (or rebind) a session to a helper's route. Returns the map size
    /// after the insert (used only for a diagnostic `registry_size` log).
    pub(crate) async fn bind(
        &self,
        sid: acp::schema::v1::SessionId,
        route: HelperRoute,
    ) -> usize {
        let mut map = self.map.lock().await;
        map.insert(sid, route);
        map.len()
    }

    /// Remove a single session binding — used by the `load_session` rollback
    /// path when the agent CLI rejects the resume.
    pub(crate) async fn unbind(&self, sid: &acp::schema::v1::SessionId) {
        self.map.lock().await.remove(sid);
    }

    /// Look up the helper owning `sid` and clone the forwarder + id.
    ///
    /// Returns `Err(internal_error)` if either (a) no helper is bound
    /// to this session — typically means the agent CLI emitted a
    /// stale request after the owning helper disconnected — or
    /// (b) the routing entry has no forwarder (production code never
    /// reaches this branch; see [`HelperRoute::forwarder`]).
    pub(crate) async fn route_for(
        &self,
        sid: &acp::schema::v1::SessionId,
        op: &'static str,
    ) -> acp::Result<(HelperId, conn::AgentLink)> {
        let entry = {
            let map = self.map.lock().await;
            map.get(sid).cloned()
        };
        match entry {
            Some(HelperRoute {
                helper_id,
                forwarder: Some(forwarder),
                ..
            }) => Ok((helper_id, forwarder)),
            Some(HelperRoute {
                forwarder: None,
                helper_id,
                ..
            }) => {
                tracing::error!(
                    target: "master",
                    op = op,
                    session_id = ?sid,
                    helper_id = ?helper_id,
                    "routing entry has no forwarder — bug; routing entry should always carry the helper's AgentSideConnection",
                );
                Err(acp::Error::internal_error()
                    .data(serde_json::json!("master routing entry missing forwarder")))
            }
            None => {
                tracing::warn!(
                    target: "master",
                    op = op,
                    session_id = ?sid,
                    "agent CLI sent request for unknown SessionId — no helper to route to",
                );
                Err(acp::Error::internal_error()
                    .data(serde_json::json!("no helper bound to session_id")))
            }
        }
    }

    /// Fan an inbound agent-CLI `session_notification` out to the owning
    /// helper's bounded channel, applying the drop/back-pressure policy
    /// documented on [`HelperRoute::consecutive_drops`].
    ///
    /// Never blocks: a slow helper pipe must not back-pressure the agent CLI's
    /// shared I/O loop, which would freeze notification delivery for every
    /// other helper on this master.
    pub(crate) async fn deliver_notification(
        &self,
        args: acp::schema::v1::SessionNotification,
    ) {
        let sid = args.session_id.clone();
        // Discriminator for "what KIND of notification this is" — useful
        // when scrolling logs to see prompt/turn lifecycle without
        // tracing the full payload.
        let kind = notification_kind(&args);
        // Snapshot the sender, the per-route drop counter, AND the
        // owning helper_id under one map lock. `helper_id` is the
        // identity key the Closed-cleanup path uses to make sure a
        // rebinding race (helper A disconnects → helper B re-uses the
        // same SessionId via `load_session`) doesn't make us delete
        // the *new* helper's entry. Without that check, the sequence
        //
        //   1. we snapshot A's `notif_tx`
        //   2. helper B rebinds `sid` to its own route via load_session
        //   3. our `try_send` on A's tx returns `Closed` (A's channel
        //      receiver was dropped when A disconnected)
        //   4. `map.remove(&sid)` would clobber B's freshly-installed
        //      route
        //
        // would silently break notification delivery for B.
        let route = {
            let map = self.map.lock().await;
            map.get(&sid).map(|r| {
                (
                    r.helper_id,
                    r.notif_tx.clone(),
                    Arc::clone(&r.consecutive_drops),
                )
            })
        };
        match route {
            Some((snap_helper_id, tx, drops)) => {
                use std::sync::atomic::Ordering;
                // `try_send` rather than `send().await`: a slow helper
                // pipe must not back-pressure this call, which is driven
                // by the agent CLI's I/O loop and is shared across every
                // helper. Blocking here would freeze notification
                // delivery for everyone.
                match tx.try_send(args) {
                    Ok(()) => {
                        // First successful send after one or more drops
                        // is the recovery point — summarize and reset.
                        let dropped = drops.swap(0, Ordering::SeqCst);
                        if dropped > 0 {
                            tracing::info!(
                                target: "master",
                                session_id = ?sid,
                                kind = %kind,
                                dropped = dropped,
                                "helper notification channel drained — backpressure cleared"
                            );
                        }
                        // Per-streamed-chunk; trace-only so default debug logs
                        // stay readable. Turn-level flow is in `prompt_timing`.
                        tracing::trace!(
                            target: "master",
                            step = "agent→helper",
                            op = "session_notification",
                            session_id = ?sid,
                            kind = %kind,
                            delivered = true,
                            "routed agent CLI notification to helper"
                        );
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        // The helper isn't draining fast enough. Drop
                        // this update rather than queue forever — the
                        // user will see a chunk gap, which is the
                        // least-bad option vs. unbounded memory growth
                        // or master-wide stall. Warn ONCE per stall
                        // (first drop); subsequent drops in the same
                        // stall increment silently and are reported in
                        // aggregate on recovery.
                        let prior = drops.fetch_add(1, Ordering::SeqCst);
                        if prior == 0 {
                            tracing::warn!(
                                target: "master",
                                session_id = ?sid,
                                kind = %kind,
                                capacity = NOTIF_CHANNEL_CAPACITY,
                                "helper notification channel full — dropping updates (subsequent drops in this stall will be silent until drain)"
                            );
                        }
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        // Helper went away between our lookup and our
                        // send. Drop the routing entry so subsequent
                        // notifications don't repeat the same warning
                        // (and the map doesn't grow forever). The
                        // `serve_helper` cleanup path also retains-out
                        // these entries on graceful disconnect; this
                        // path catches the race where send fails before
                        // that runs.
                        match self.remove_if_owned(&sid, snap_helper_id).await {
                            ClosedCleanup::Removed => {
                                tracing::warn!(
                                    target: "master",
                                    session_id = ?sid,
                                    kind = %kind,
                                    helper_id = ?snap_helper_id,
                                    "helper notification channel closed — helper likely disconnected; dropping update and routing entry"
                                );
                            }
                            ClosedCleanup::Rebound => {
                                tracing::info!(
                                    target: "master",
                                    session_id = ?sid,
                                    kind = %kind,
                                    stale_helper_id = ?snap_helper_id,
                                    "helper notification channel closed but SessionId has been rebound to a different helper — dropping update, leaving new route intact"
                                );
                            }
                            ClosedCleanup::AlreadyGone => {
                                // Entry already gone (likely the
                                // `serve_helper` cleanup raced ahead
                                // of us). Nothing to do.
                                tracing::debug!(
                                    target: "master",
                                    session_id = ?sid,
                                    kind = %kind,
                                    "helper notification channel closed and routing entry already cleaned up"
                                );
                            }
                        }
                    }
                }
            }
            None => {
                tracing::warn!(
                    target: "master",
                    session_id = ?sid,
                    kind = %kind,
                    "agent CLI emitted session_notification for unknown SessionId — no helper to route to"
                );
            }
        }
    }

    /// Remove `sid`'s route **iff** it still belongs to `expected_helper`.
    ///
    /// CRITICAL: only remove if the entry STILL belongs to the helper the
    /// caller snapshotted. A freshly-issued `load_session` can have rebound
    /// the same SessionId to a different helper between the snapshot and now —
    /// clobbering that new entry would silently break notification delivery
    /// for the new helper. `HelperId` is unique per master lifetime (monotonic
    /// counter), so equality is a sufficient identity check.
    async fn remove_if_owned(
        &self,
        sid: &acp::schema::v1::SessionId,
        expected_helper: HelperId,
    ) -> ClosedCleanup {
        let mut map = self.map.lock().await;
        match map.get(sid) {
            Some(current) if current.helper_id == expected_helper => {
                map.remove(sid);
                ClosedCleanup::Removed
            }
            Some(_) => ClosedCleanup::Rebound,
            None => ClosedCleanup::AlreadyGone,
        }
    }

    /// Remove every routing entry owned by `helper_id`, returning the removed
    /// `SessionId`s so the caller can mirror the removal into the live-session
    /// registry and broadcast `session_removed` for each. Factored out of
    /// `serve_helper` so the cleanup is unit-testable without a real pipe.
    pub(crate) async fn drop_helper(
        &self,
        helper_id: HelperId,
    ) -> Vec<acp::schema::v1::SessionId> {
        let mut map = self.map.lock().await;
        let victims = map
            .iter()
            .filter_map(|(sid, route)| (route.helper_id == helper_id).then(|| sid.clone()))
            .collect::<Vec<_>>();
        map.retain(|_, route| route.helper_id != helper_id);
        victims
    }

    /// Snapshot of all currently-bound session ids. Used by the host
    /// `session/list` reconcile to union in master's authoritative live-pane
    /// set (every agent-pane `session/new` is routed here first).
    pub(crate) async fn session_ids(&self) -> Vec<acp::schema::v1::SessionId> {
        self.map.lock().await.keys().cloned().collect()
    }

    /// Test-only: direct access to the underlying map, so the master's
    /// integration tests can seed routing state without standing up a real
    /// helper pipe. Production code goes through the typed methods above.
    #[cfg(test)]
    pub(crate) async fn map_for_test(
        &self,
    ) -> tokio::sync::MutexGuard<'_, HashMap<acp::schema::v1::SessionId, HelperRoute>> {
        self.map.lock().await
    }

    /// Test-only: number of live routing entries.
    #[cfg(test)]
    pub(crate) async fn len(&self) -> usize {
        self.map.lock().await.len()
    }

    /// Test-only: whether a session is currently bound.
    #[cfg(test)]
    pub(crate) async fn contains(&self, sid: &acp::schema::v1::SessionId) -> bool {
        self.map.lock().await.contains_key(sid)
    }

    /// Test-only: the helper currently owning a session, if any.
    #[cfg(test)]
    pub(crate) async fn owner_of(
        &self,
        sid: &acp::schema::v1::SessionId,
    ) -> Option<HelperId> {
        self.map.lock().await.get(sid).map(|r| r.helper_id)
    }
}

/// Short, log-friendly tag for a `SessionNotification`'s update
/// variant. Just enough to grep — "this turn started chunking",
/// "this turn called a tool", "this turn ended".
pub(crate) fn notification_kind(
    notif: &acp::schema::v1::SessionNotification,
) -> &'static str {
    use acp::schema::v1::SessionUpdate::*;
    match &notif.update {
        AgentMessageChunk { .. } => "agent_message_chunk",
        AgentThoughtChunk { .. } => "agent_thought_chunk",
        UserMessageChunk { .. } => "user_message_chunk",
        ToolCall(_) => "tool_call",
        ToolCallUpdate(_) => "tool_call_update",
        Plan(_) => "plan",
        CurrentModeUpdate { .. } => "current_mode_update",
        AvailableCommandsUpdate { .. } => "available_commands_update",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(s: &str) -> acp::schema::v1::SessionId {
        acp::schema::v1::SessionId::new(s)
    }

    /// A routing entry with a live notification channel but no forwarder —
    /// enough to test the map operations and notification delivery without a
    /// real helper pipe. Returns the entry plus the receiver so the test can
    /// observe delivered notifications.
    fn route_with_channel(
        helper_id: HelperId,
    ) -> (
        HelperRoute,
        mpsc::Receiver<acp::schema::v1::SessionNotification>,
    ) {
        let (tx, rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
        (
            HelperRoute {
                helper_id,
                notif_tx: tx,
                forwarder: None,
                consecutive_drops: Arc::new(AtomicU64::new(0)),
            },
            rx,
        )
    }

    fn notif(session: &str) -> acp::schema::v1::SessionNotification {
        acp::schema::v1::SessionNotification::new(
            sid(session),
            acp::schema::v1::SessionUpdate::AgentMessageChunk(
                acp::schema::v1::ContentChunk::new("hi".into()),
            ),
        )
    }

    #[tokio::test]
    async fn route_for_unknown_session_id_returns_internal_error() {
        let router = SessionRouter::new();
        let err = router
            .route_for(&sid("nope"), "request_permission")
            .await
            .unwrap_err();
        assert_eq!(err.code, acp::Error::internal_error().code);
    }

    #[tokio::test]
    async fn route_for_none_forwarder_returns_internal_error() {
        let router = SessionRouter::new();
        let (route, _rx) = route_with_channel(HelperId(1));
        router.bind(sid("s1"), route).await;
        // forwarder is None, so route_for must reject rather than hand back a
        // half-built entry.
        let err = router.route_for(&sid("s1"), "create_terminal").await.unwrap_err();
        assert_eq!(err.code, acp::Error::internal_error().code);
    }

    #[tokio::test]
    async fn bind_then_deliver_reaches_owning_helper() {
        let router = SessionRouter::new();
        let (route, mut rx) = route_with_channel(HelperId(7));
        router.bind(sid("s1"), route).await;
        router.deliver_notification(notif("s1")).await;
        let got = rx.try_recv().expect("notification should have been delivered");
        assert_eq!(got.session_id, sid("s1"));
    }

    #[tokio::test]
    async fn deliver_to_unknown_session_is_dropped_silently() {
        let router = SessionRouter::new();
        // No panic / no route: just a warn-log and a no-op.
        router.deliver_notification(notif("ghost")).await;
        assert_eq!(router.len().await, 0);
    }

    #[tokio::test]
    async fn closed_channel_removes_owning_route() {
        let router = SessionRouter::new();
        let (route, rx) = route_with_channel(HelperId(3));
        router.bind(sid("s1"), route).await;
        // Drop the receiver so try_send sees `Closed`.
        drop(rx);
        assert!(router.contains(&sid("s1")).await);
        router.deliver_notification(notif("s1")).await;
        assert!(
            !router.contains(&sid("s1")).await,
            "a closed channel must evict its own routing entry"
        );
    }

    #[tokio::test]
    async fn closed_channel_does_not_clobber_rebound_route() {
        let router = SessionRouter::new();
        // Helper A binds, then its receiver dies.
        let (route_a, rx_a) = route_with_channel(HelperId(1));
        router.bind(sid("s1"), route_a).await;
        drop(rx_a);
        // Helper B rebinds the SAME SessionId (load_session resume race).
        let (route_b, _rx_b) = route_with_channel(HelperId(2));
        router.bind(sid("s1"), route_b).await;
        // A late notification finds B's live channel, so it delivers — the
        // Closed path never fires and B's route survives.
        router.deliver_notification(notif("s1")).await;
        assert_eq!(router.owner_of(&sid("s1")).await, Some(HelperId(2)));
    }

    #[tokio::test]
    async fn remove_if_owned_leaves_rebound_entry_intact() {
        let router = SessionRouter::new();
        let (route_a, _rx_a) = route_with_channel(HelperId(1));
        router.bind(sid("s1"), route_a).await;
        // Simulate a rebind to helper 2, then a stale cleanup from helper 1.
        let (route_b, _rx_b) = route_with_channel(HelperId(2));
        router.bind(sid("s1"), route_b).await;
        assert_eq!(
            router.remove_if_owned(&sid("s1"), HelperId(1)).await,
            ClosedCleanup::Rebound
        );
        assert_eq!(router.owner_of(&sid("s1")).await, Some(HelperId(2)));
    }

    #[tokio::test]
    async fn drop_helper_removes_only_its_sessions() {
        let router = SessionRouter::new();
        let (r1, _a) = route_with_channel(HelperId(1));
        let (r2, _b) = route_with_channel(HelperId(1));
        let (r3, _c) = route_with_channel(HelperId(2));
        router.bind(sid("s1"), r1).await;
        router.bind(sid("s2"), r2).await;
        router.bind(sid("s3"), r3).await;
        let mut victims = router.drop_helper(HelperId(1)).await;
        victims.sort_by_key(|s| s.to_string());
        assert_eq!(victims, vec![sid("s1"), sid("s2")]);
        assert_eq!(router.len().await, 1);
        assert_eq!(router.owner_of(&sid("s3")).await, Some(HelperId(2)));
    }

    #[tokio::test]
    async fn unbind_removes_single_session() {
        let router = SessionRouter::new();
        let (route, _rx) = route_with_channel(HelperId(1));
        router.bind(sid("s1"), route).await;
        router.unbind(&sid("s1")).await;
        assert!(!router.contains(&sid("s1")).await);
    }
}
