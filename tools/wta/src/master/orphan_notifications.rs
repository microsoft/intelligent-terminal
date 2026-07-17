// tools/wta/src/master/orphan_notifications.rs
//
// Buffer for `session/update` notifications that arrive from the agent
// CLI *before* master has installed the `session_to_helper` route for
// their session.
//
// Why this exists: for a brand-new session the `SessionId` is only known
// once the agent CLI answers `session/new`, so master cannot pre-register
// the route the way `load_session` does (there the id is a request
// input). Agents commonly emit `session/update` notifications —
// `available_commands_update`, and occasionally an opening message chunk
// — *synchronously while `session/new` is still returning*. Those land in
// `MasterClient::session_notification` a few milliseconds before
// `new_session` records the route, so without buffering they hit the
// "unknown SessionId" path and are dropped (a WARN on every session
// create, and the slash-command list silently lost).
//
// This buffer holds such notifications briefly, keyed by session id, so
// `new_session` can replay them in arrival order once the route is
// installed. Anything still unbound after `ORPHAN_TTL` is a genuine drop
// (agent bug / vanished helper) and is surfaced to the caller for a WARN.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use agent_client_protocol as acp;

/// How long a buffered notification is retained waiting for its session
/// to bind to a helper. The pre-binding race resolves in single-digit
/// milliseconds, so this window is generous; anything still unbound after
/// it is treated as a real dropped notification.
pub(super) const ORPHAN_TTL: Duration = Duration::from_secs(30);

/// Max buffered notifications retained per not-yet-bound session. Bounds
/// memory if an agent streams into a session that never binds. Oldest
/// entries are dropped first once the cap is exceeded.
pub(super) const PER_SESSION_CAP: usize = 256;

/// Max number of distinct not-yet-bound sessions buffered at once. A hard
/// safety cap; steady state is 0–2 (each cleared within ~1 s by binding).
pub(super) const MAX_SESSIONS: usize = 256;

/// Why a session's buffered notifications were evicted without ever being
/// replayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EvictReason {
    /// The session stayed unbound past [`ORPHAN_TTL`].
    Ttl,
    /// The distinct-session count exceeded [`MAX_SESSIONS`] and this was
    /// the oldest victim.
    MaxSessions,
}

/// A session whose buffered notifications were dropped without replay — a
/// genuine lost-notification event the caller surfaces at WARN.
#[derive(Debug, Clone)]
pub(super) struct Evicted {
    pub session_id: acp::schema::v1::SessionId,
    pub dropped: usize,
    pub reason: EvictReason,
}

struct Entry {
    received_at: Instant,
    notification: acp::schema::v1::SessionNotification,
}

/// Bounded, TTL-pruned store of pre-binding session notifications, keyed
/// by session id. Not thread-safe on its own — the master wraps it in a
/// `Mutex` and only ever touches it while holding the `session_to_helper`
/// lock, which serializes buffering against replay (see the module doc
/// and `MasterClient::session_notification`).
#[derive(Default)]
pub(super) struct OrphanNotificationBuffer {
    by_session: HashMap<acp::schema::v1::SessionId, VecDeque<Entry>>,
}

impl OrphanNotificationBuffer {
    /// Buffer `notification` for a session that has no route yet. Prunes
    /// TTL-expired sessions and enforces the session-count cap first.
    /// Returns any sessions evicted in the process so the caller can log
    /// them at WARN.
    pub(super) fn buffer(
        &mut self,
        session_id: acp::schema::v1::SessionId,
        notification: acp::schema::v1::SessionNotification,
        now: Instant,
    ) -> Vec<Evicted> {
        let mut evicted = self.prune_expired(now);

        let queue = self.by_session.entry(session_id.clone()).or_default();
        queue.push_back(Entry {
            received_at: now,
            notification,
        });
        while queue.len() > PER_SESSION_CAP {
            queue.pop_front();
        }

        // Session-count safety net. If we're tracking too many distinct
        // unbound sessions, evict the one with the oldest head entry
        // (closest to TTL expiry anyway). Never evict the session we just
        // buffered into.
        while self.by_session.len() > MAX_SESSIONS {
            let Some(victim) = self.oldest_session_except(&session_id) else {
                break;
            };
            if let Some(queue) = self.by_session.remove(&victim) {
                evicted.push(Evicted {
                    session_id: victim,
                    dropped: queue.len(),
                    reason: EvictReason::MaxSessions,
                });
            }
        }
        evicted
    }

    /// Remove and return every notification buffered for `session_id`, in
    /// arrival order. Called right after the route is installed so the
    /// caller can replay them to the newly-bound helper.
    pub(super) fn take(
        &mut self,
        session_id: &acp::schema::v1::SessionId,
    ) -> Vec<acp::schema::v1::SessionNotification> {
        self.by_session
            .remove(session_id)
            .map(|queue| queue.into_iter().map(|entry| entry.notification).collect())
            .unwrap_or_default()
    }

    /// Drop every session whose oldest buffered entry has aged past the
    /// TTL, returning them for logging.
    fn prune_expired(&mut self, now: Instant) -> Vec<Evicted> {
        let expired: Vec<acp::schema::v1::SessionId> = self
            .by_session
            .iter()
            .filter(|(_, queue)| {
                queue
                    .front()
                    .map_or(true, |entry| now.duration_since(entry.received_at) >= ORPHAN_TTL)
            })
            .map(|(sid, _)| sid.clone())
            .collect();
        expired
            .into_iter()
            .filter_map(|sid| {
                self.by_session.remove(&sid).map(|queue| Evicted {
                    session_id: sid,
                    dropped: queue.len(),
                    reason: EvictReason::Ttl,
                })
            })
            .collect()
    }

    /// The session (other than `except`) whose oldest buffered entry is
    /// the earliest — the best MaxSessions eviction victim.
    fn oldest_session_except(
        &self,
        except: &acp::schema::v1::SessionId,
    ) -> Option<acp::schema::v1::SessionId> {
        self.by_session
            .iter()
            .filter(|(sid, _)| *sid != except)
            .min_by_key(|(_, queue)| queue.front().map(|entry| entry.received_at))
            .map(|(sid, _)| sid.clone())
    }

    #[cfg(test)]
    pub(super) fn session_count(&self) -> usize {
        self.by_session.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(s: &str) -> acp::schema::v1::SessionId {
        acp::schema::v1::SessionId::new(s.to_string())
    }

    fn notif(sid_str: &str) -> acp::schema::v1::SessionNotification {
        acp::schema::v1::SessionNotification::new(
            sid(sid_str),
            acp::schema::v1::SessionUpdate::AvailableCommandsUpdate(
                acp::schema::v1::AvailableCommandsUpdate::new(Vec::new()),
            ),
        )
    }

    #[test]
    fn buffers_and_takes_in_arrival_order() {
        let mut buf = OrphanNotificationBuffer::default();
        let now = Instant::now();
        assert!(buf.buffer(sid("a"), notif("a"), now).is_empty());
        assert!(buf.buffer(sid("a"), notif("a"), now).is_empty());
        assert_eq!(buf.session_count(), 1);

        let taken = buf.take(&sid("a"));
        assert_eq!(taken.len(), 2, "both notifications replayed");
        assert_eq!(buf.session_count(), 0, "queue removed on take");
    }

    #[test]
    fn take_of_unknown_session_is_empty() {
        let mut buf = OrphanNotificationBuffer::default();
        assert!(buf.take(&sid("missing")).is_empty());
    }

    #[test]
    fn take_leaves_other_sessions_intact() {
        let mut buf = OrphanNotificationBuffer::default();
        let now = Instant::now();
        buf.buffer(sid("a"), notif("a"), now);
        buf.buffer(sid("b"), notif("b"), now);

        assert_eq!(buf.take(&sid("a")).len(), 1);
        assert_eq!(buf.session_count(), 1, "b still buffered");
        assert_eq!(buf.take(&sid("b")).len(), 1);
    }

    #[test]
    fn expired_session_is_evicted_on_next_buffer() {
        let mut buf = OrphanNotificationBuffer::default();
        let start = Instant::now();
        buf.buffer(sid("stale"), notif("stale"), start);

        // A later buffer for a different session past the TTL evicts the
        // stale one and reports it.
        let later = start + ORPHAN_TTL + Duration::from_secs(1);
        let evicted = buf.buffer(sid("fresh"), notif("fresh"), later);

        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].session_id, sid("stale"));
        assert_eq!(evicted[0].dropped, 1);
        assert_eq!(evicted[0].reason, EvictReason::Ttl);
        // The stale queue is gone; only the fresh one remains.
        assert!(buf.take(&sid("stale")).is_empty());
        assert_eq!(buf.take(&sid("fresh")).len(), 1);
    }

    #[test]
    fn per_session_cap_drops_oldest() {
        let mut buf = OrphanNotificationBuffer::default();
        let now = Instant::now();
        for _ in 0..(PER_SESSION_CAP + 5) {
            buf.buffer(sid("a"), notif("a"), now);
        }
        assert_eq!(
            buf.take(&sid("a")).len(),
            PER_SESSION_CAP,
            "queue capped at PER_SESSION_CAP"
        );
    }

    #[test]
    fn max_sessions_evicts_oldest_head() {
        let mut buf = OrphanNotificationBuffer::default();
        let base = Instant::now();
        // Fill exactly to the cap, each session with a distinct (older →
        // newer) head timestamp.
        for i in 0..MAX_SESSIONS {
            let when = base + Duration::from_millis(i as u64);
            buf.buffer(sid(&format!("s{i}")), notif("x"), when);
        }
        assert_eq!(buf.session_count(), MAX_SESSIONS);

        // One more distinct session tips us over; the oldest head (s0) is
        // evicted as MaxSessions.
        let when = base + Duration::from_millis(MAX_SESSIONS as u64);
        let evicted = buf.buffer(sid("overflow"), notif("x"), when);

        let max_evictions: Vec<_> = evicted
            .iter()
            .filter(|e| e.reason == EvictReason::MaxSessions)
            .collect();
        assert_eq!(max_evictions.len(), 1);
        assert_eq!(max_evictions[0].session_id, sid("s0"));
        assert_eq!(buf.session_count(), MAX_SESSIONS);
        assert!(buf.take(&sid("s0")).is_empty(), "oldest evicted");
        assert_eq!(buf.take(&sid("overflow")).len(), 1, "newcomer kept");
    }
}
