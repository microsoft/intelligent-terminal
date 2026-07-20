//! WTA's session-routing control plane.
//!
//! WTA multiplexes many helper pipes onto a pool of shared proxy-chain
//! runtimes, so instance-scoped routing and orphan/rebind lifetime live outside
//! the canonical ACP conductors.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use agent_client_protocol as acp;
use tokio::sync::{mpsc, Mutex};

use crate::protocol::acp::conn;

const MAX_PENDING_NOTIFICATIONS_PER_AGENT: usize = 1024;
const MAX_PENDING_NOTIFICATIONS_PER_SESSION: usize = 256;

/// Monotonic identity of one spawned agent process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct AgentInstanceId(pub(super) u64);

/// Monotonic identity of one helper pipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct HelperId(pub(crate) u64);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SessionKey {
    agent: AgentInstanceId,
    session_id: acp::schema::v1::SessionId,
}

/// Exact ownership of one route generation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct BindingToken {
    key: SessionKey,
    generation: u64,
}

impl BindingToken {
    pub(super) fn session_id(&self) -> &acp::schema::v1::SessionId {
        &self.key.session_id
    }

    pub(super) fn agent_instance(&self) -> AgentInstanceId {
        self.key.agent
    }

    pub(super) fn generation(&self) -> u64 {
        self.generation
    }
}

/// Reverse path from an agent session to its helper.
#[derive(Clone)]
pub(super) struct HelperRoute {
    pub(super) helper_id: HelperId,
    pub(super) notif_tx: mpsc::Sender<acp::schema::v1::SessionNotification>,
    pub(super) forwarder: Option<conn::AgentLink>,
    pub(super) consecutive_drops: Arc<AtomicU64>,
}

struct Binding {
    token: BindingToken,
    route: HelperRoute,
}

#[derive(Default)]
struct PendingNew {
    attempts: HashMap<u64, HelperId>,
    buffered: HashMap<acp::schema::v1::SessionId, VecDeque<acp::schema::v1::SessionNotification>>,
    total: usize,
}

#[derive(Default)]
struct RouterState {
    routes: HashMap<SessionKey, Binding>,
    orphans: HashSet<SessionKey>,
    published: HashMap<acp::schema::v1::SessionId, BindingToken>,
    publication_candidates:
        HashMap<BindingToken, crate::session_registry::SessionInfo>,
    pending_new: HashMap<AgentInstanceId, PendingNew>,
    reaped_agents: HashSet<AgentInstanceId>,
    next_generation: u64,
}

#[derive(Debug)]
pub(super) enum BindError {
    AgentReaped,
    LiveBinding(BindingToken),
}

pub(super) struct PendingNewToken {
    agent: AgentInstanceId,
    generation: u64,
}

#[derive(Debug)]
pub(super) enum FinishNewError {
    Cancelled,
    LiveBinding,
}

/// Result of installing a load binding.
#[derive(Debug)]
pub(super) struct LoadBinding {
    pub(super) token: BindingToken,
    pub(super) claimed_orphan: bool,
}

pub(super) struct AgentReap {
    pub(super) routes_removed: usize,
    pub(super) registry_changes: Vec<RegistryChange>,
    pub(super) discarded: usize,
}

/// Result of completing a pending `session/new`.
pub(super) struct NewBinding {
    pub(super) token: BindingToken,
    pub(super) buffered_enqueued: usize,
    pub(super) buffered_dropped: usize,
    pub(super) channel_closed: bool,
    pub(super) discarded: usize,
}

pub(super) enum NotificationRoute {
    Deliver {
        token: BindingToken,
        route: HelperRoute,
        notification: acp::schema::v1::SessionNotification,
    },
    Buffered,
    DroppedUnknown,
    DroppedOverflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PublishResult {
    Published,
    Collision,
    Stale,
}

pub(super) struct RegistryChange {
    pub(super) removed: BindingToken,
    pub(super) promoted: Option<crate::session_registry::SessionInfo>,
}

pub(super) enum UnpublishResult {
    CandidateRemoved,
    OwnerRemoved(RegistryChange),
}

/// Routing, ownership, orphan, and pending-new state for all agent instances.
#[derive(Default)]
pub(super) struct SessionRouter {
    state: Mutex<RouterState>,
}

impl SessionRouter {
    pub(super) fn new() -> Self {
        Self::default()
    }

    fn next_token(state: &mut RouterState, key: SessionKey) -> BindingToken {
        state.next_generation = state.next_generation.wrapping_add(1);
        if state.next_generation == 0 {
            state.next_generation = 1;
        }
        BindingToken {
            key,
            generation: state.next_generation,
        }
    }

    fn install(
        state: &mut RouterState,
        agent: AgentInstanceId,
        session_id: acp::schema::v1::SessionId,
        route: HelperRoute,
    ) -> BindingToken {
        let key = SessionKey { agent, session_id };
        let token = Self::next_token(state, key.clone());
        state.routes.insert(
            key,
            Binding {
                token: token.clone(),
                route,
            },
        );
        token
    }

    pub(super) async fn begin_new(
        &self,
        agent: AgentInstanceId,
        helper: HelperId,
    ) -> Option<PendingNewToken> {
        let mut state = self.state.lock().await;
        if state.reaped_agents.contains(&agent) {
            return None;
        }
        state.next_generation = state.next_generation.wrapping_add(1);
        if state.next_generation == 0 {
            state.next_generation = 1;
        }
        let generation = state.next_generation;
        state
            .pending_new
            .entry(agent)
            .or_default()
            .attempts
            .insert(generation, helper);
        Some(PendingNewToken { agent, generation })
    }

    pub(super) async fn finish_new_success(
        &self,
        pending_token: &PendingNewToken,
        session_id: acp::schema::v1::SessionId,
        route: HelperRoute,
    ) -> Result<NewBinding, FinishNewError> {
        let mut state = self.state.lock().await;
        let key = SessionKey {
            agent: pending_token.agent,
            session_id: session_id.clone(),
        };
        let live_binding = state.routes.contains_key(&key);
        let Some(pending) = state.pending_new.get_mut(&pending_token.agent) else {
            return Err(FinishNewError::Cancelled);
        };
        if pending
            .attempts
            .remove(&pending_token.generation)
            .is_none()
        {
            return Err(FinishNewError::Cancelled);
        }
        let notif_tx = route.notif_tx.clone();
        let mut buffered = Vec::new();
        let mut discarded = 0;
        if !live_binding {
            if let Some(queue) = pending.buffered.remove(&session_id) {
                pending.total = pending.total.saturating_sub(queue.len());
                buffered.extend(queue);
            }
        }
        if pending.attempts.is_empty() {
            discarded = pending.total;
        }
        if state
            .pending_new
            .get(&pending_token.agent)
            .is_some_and(|pending| pending.attempts.is_empty())
        {
            state.pending_new.remove(&pending_token.agent);
        }
        if live_binding {
            return Err(FinishNewError::LiveBinding);
        }
        let token = Self::install(
            &mut state,
            pending_token.agent,
            session_id.clone(),
            route,
        );
        let mut buffered_enqueued = 0;
        let mut buffered_dropped = 0;
        let mut channel_closed = notif_tx.is_closed();
        // Keep the router lock until every early notification is enqueued.
        // A later notification therefore cannot observe the new route and
        // overtake a buffered one.
        for notification in buffered {
            match notif_tx.try_send(notification) {
                Ok(()) => buffered_enqueued += 1,
                Err(mpsc::error::TrySendError::Full(_)) => buffered_dropped += 1,
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    channel_closed = true;
                    buffered_dropped += 1;
                }
            }
        }
        if channel_closed {
            state.routes.remove(&token.key);
            state.orphans.insert(token.key.clone());
        }
        Ok(NewBinding {
            token,
            buffered_enqueued,
            buffered_dropped,
            channel_closed,
            discarded,
        })
    }

    /// Finish one failed `session/new`; returns buffered notifications discarded
    /// when this was the final pending request for the agent.
    pub(super) async fn finish_new_failure(&self, token: &PendingNewToken) -> usize {
        let mut state = self.state.lock().await;
        let mut discarded = 0;
        if let Some(pending) = state.pending_new.get_mut(&token.agent) {
            pending.attempts.remove(&token.generation);
            if pending.attempts.is_empty() {
                discarded = pending.total;
            }
        }
        if state
            .pending_new
            .get(&token.agent)
            .is_some_and(|pending| pending.attempts.is_empty())
        {
            state.pending_new.remove(&token.agent);
        }
        discarded
    }

    pub(super) async fn begin_load(
        &self,
        agent: AgentInstanceId,
        session_id: acp::schema::v1::SessionId,
        route: HelperRoute,
    ) -> Result<LoadBinding, BindError> {
        let mut state = self.state.lock().await;
        if state.reaped_agents.contains(&agent) {
            return Err(BindError::AgentReaped);
        }
        let key = SessionKey {
            agent,
            session_id: session_id.clone(),
        };
        if let Some(current) = state.routes.get(&key) {
            return Err(BindError::LiveBinding(current.token.clone()));
        }
        let claimed_orphan = state.orphans.remove(&key);
        let token = Self::install(&mut state, agent, session_id, route);
        Ok(LoadBinding {
            token,
            claimed_orphan,
        })
    }

    /// Roll back only the exact failed load generation.
    pub(super) async fn rollback_load(&self, binding: &LoadBinding) -> bool {
        let mut state = self.state.lock().await;
        let is_current = state
            .routes
            .get(&binding.token.key)
            .is_some_and(|current| current.token == binding.token);
        if !is_current {
            return false;
        }
        state.routes.remove(&binding.token.key);
        if binding.claimed_orphan {
            state.orphans.insert(binding.token.key.clone());
        }
        true
    }

    pub(super) async fn route_for(
        &self,
        agent: AgentInstanceId,
        session_id: &acp::schema::v1::SessionId,
    ) -> Option<(BindingToken, HelperRoute)> {
        let state = self.state.lock().await;
        let key = SessionKey {
            agent,
            session_id: session_id.clone(),
        };
        state
            .routes
            .get(&key)
            .map(|binding| (binding.token.clone(), binding.route.clone()))
    }

    pub(super) async fn route_notification(
        &self,
        agent: AgentInstanceId,
        notification: acp::schema::v1::SessionNotification,
    ) -> NotificationRoute {
        let mut state = self.state.lock().await;
        let key = SessionKey {
            agent,
            session_id: notification.session_id.clone(),
        };
        if let Some(binding) = state.routes.get(&key) {
            return NotificationRoute::Deliver {
                token: binding.token.clone(),
                route: binding.route.clone(),
                notification,
            };
        }
        if state.orphans.contains(&key) {
            return NotificationRoute::DroppedUnknown;
        }

        let Some(pending) = state.pending_new.get_mut(&agent) else {
            return NotificationRoute::DroppedUnknown;
        };
        let session_len = pending
            .buffered
            .get(&notification.session_id)
            .map_or(0, VecDeque::len);
        if pending.total >= MAX_PENDING_NOTIFICATIONS_PER_AGENT
            || session_len >= MAX_PENDING_NOTIFICATIONS_PER_SESSION
        {
            return NotificationRoute::DroppedOverflow;
        }
        pending
            .buffered
            .entry(notification.session_id.clone())
            .or_default()
            .push_back(notification);
        pending.total += 1;
        NotificationRoute::Buffered
    }

    /// Remove an exact route after a closed channel. The caller decides whether
    /// the still-live agent process means it should become an orphan.
    pub(super) async fn remove_current(&self, token: &BindingToken, make_orphan: bool) -> bool {
        let mut state = self.state.lock().await;
        let is_current = state
            .routes
            .get(&token.key)
            .is_some_and(|binding| binding.token == *token);
        if !is_current {
            return false;
        }
        state.routes.remove(&token.key);
        if make_orphan {
            state.orphans.insert(token.key.clone());
        }
        true
    }

    /// Atomically detach this helper's current bindings and orphan only those
    /// owned by the supplied still-live agent instance.
    pub(super) async fn detach_helper(
        &self,
        helper: HelperId,
        live_agent: Option<AgentInstanceId>,
    ) -> Vec<BindingToken> {
        let mut state = self.state.lock().await;
        let victims = state
            .routes
            .values()
            .filter(|binding| binding.route.helper_id == helper)
            .map(|binding| binding.token.clone())
            .collect::<Vec<_>>();
        for token in &victims {
            state.routes.remove(&token.key);
            if live_agent == Some(token.key.agent) {
                state.orphans.insert(token.key.clone());
            }
        }
        let pending_agents = state.pending_new.keys().copied().collect::<Vec<_>>();
        for agent in pending_agents {
            let remove_agent = if let Some(pending) = state.pending_new.get_mut(&agent) {
                pending.attempts.retain(|_, owner| *owner != helper);
                pending.attempts.is_empty()
            } else {
                false
            };
            if remove_agent {
                state.pending_new.remove(&agent);
            }
        }
        victims
    }

    /// Mark a registry row as owned by this exact live binding.
    pub(super) async fn publish(
        &self,
        token: &BindingToken,
        info: crate::session_registry::SessionInfo,
    ) -> PublishResult {
        let mut state = self.state.lock().await;
        let is_current = state
            .routes
            .get(&token.key)
            .is_some_and(|binding| binding.token == *token);
        if !is_current {
            return PublishResult::Stale;
        }
        state.publication_candidates.insert(token.clone(), info);
        match state.published.get(&token.key.session_id) {
            Some(owner) if owner != token => PublishResult::Collision,
            Some(_) => PublishResult::Published,
            None => {
                state
                    .published
                    .insert(token.key.session_id.clone(), token.clone());
                PublishResult::Published
            }
        }
    }

    fn unpublish_locked(state: &mut RouterState, token: &BindingToken) -> UnpublishResult {
        state.publication_candidates.remove(token);
        let owns = state
            .published
            .get(&token.key.session_id)
            .is_some_and(|current| current == token);
        if !owns {
            return UnpublishResult::CandidateRemoved;
        }
        state.published.remove(&token.key.session_id);
        let promoted = state
            .publication_candidates
            .iter()
            .filter(|(candidate, _)| {
                candidate.key.session_id == token.key.session_id
                    && state
                        .routes
                        .get(&candidate.key)
                        .is_some_and(|binding| binding.token == **candidate)
            })
            .min_by_key(|(candidate, _)| candidate.generation)
            .map(|(candidate, info)| (candidate.clone(), info.clone()));
        let promoted_info = promoted.map(|(candidate, info)| {
            state
                .published
                .insert(candidate.key.session_id.clone(), candidate);
            info
        });
        UnpublishResult::OwnerRemoved(RegistryChange {
            removed: token.clone(),
            promoted: promoted_info,
        })
    }

    /// Release a registry row only if this exact binding still owns it, promoting
    /// another live colliding binding when one exists.
    pub(super) async fn unpublish(&self, token: &BindingToken) -> UnpublishResult {
        let mut state = self.state.lock().await;
        Self::unpublish_locked(&mut state, token)
    }

    pub(super) async fn reap_agent(&self, agent: AgentInstanceId) -> AgentReap {
        let mut state = self.state.lock().await;
        state.reaped_agents.insert(agent);
        let route_tokens = state
            .routes
            .values()
            .filter(|binding| binding.token.key.agent == agent)
            .map(|binding| binding.token.clone())
            .collect::<Vec<_>>();
        for token in &route_tokens {
            state.routes.remove(&token.key);
        }
        state.orphans.retain(|key| key.agent != agent);
        let discarded = state
            .pending_new
            .remove(&agent)
            .map_or(0, |pending| pending.total);
        let registry_changes = route_tokens
            .iter()
            .filter_map(|token| match Self::unpublish_locked(&mut state, token) {
                UnpublishResult::CandidateRemoved => None,
                UnpublishResult::OwnerRemoved(change) => Some(change),
            })
            .collect();
        AgentReap {
            routes_removed: route_tokens.len(),
            registry_changes,
            discarded,
        }
    }

    pub(super) async fn routed_session_ids(&self) -> HashSet<acp::schema::v1::SessionId> {
        self.state
            .lock()
            .await
            .routes
            .keys()
            .map(|key| key.session_id.clone())
            .collect()
    }

    #[cfg(test)]
    async fn orphan_contains(
        &self,
        agent: AgentInstanceId,
        session_id: &acp::schema::v1::SessionId,
    ) -> bool {
        self.state.lock().await.orphans.contains(&SessionKey {
            agent,
            session_id: session_id.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(
        helper: u64,
    ) -> (
        HelperRoute,
        mpsc::Receiver<acp::schema::v1::SessionNotification>,
    ) {
        let (tx, rx) = mpsc::channel(2);
        (
            HelperRoute {
                helper_id: HelperId(helper),
                notif_tx: tx,
                forwarder: None,
                consecutive_drops: Arc::new(AtomicU64::new(0)),
            },
            rx,
        )
    }

    fn notification(id: &str) -> acp::schema::v1::SessionNotification {
        acp::schema::v1::SessionNotification::new(
            acp::schema::v1::SessionId::new(id),
            acp::schema::v1::SessionUpdate::AgentMessageChunk(acp::schema::v1::ContentChunk::new(
                "chunk".into(),
            )),
        )
    }

    fn info(id: &str) -> crate::session_registry::SessionInfo {
        crate::session_registry::SessionInfo::new(
            acp::schema::v1::SessionId::new(id),
            std::path::PathBuf::from("C:\\repo"),
        )
    }

    #[tokio::test]
    async fn equal_session_ids_on_two_agents_route_independently() {
        let conductor = SessionRouter::new();
        let sid = acp::schema::v1::SessionId::new("same");
        let (route_a, _) = route(1);
        let (route_b, _) = route(2);
        conductor
            .begin_load(AgentInstanceId(10), sid.clone(), route_a)
            .await
            .expect("agent A binding");
        conductor
            .begin_load(AgentInstanceId(20), sid.clone(), route_b)
            .await
            .expect("agent B binding");

        let (_, got_a) = conductor
            .route_for(AgentInstanceId(10), &sid)
            .await
            .expect("agent A route");
        let (_, got_b) = conductor
            .route_for(AgentInstanceId(20), &sid)
            .await
            .expect("agent B route");
        assert_eq!(got_a.helper_id, HelperId(1));
        assert_eq!(got_b.helper_id, HelperId(2));
    }

    #[tokio::test]
    async fn live_binding_cannot_be_stolen_by_another_helper() {
        let conductor = SessionRouter::new();
        let sid = acp::schema::v1::SessionId::new("sid");
        let (old, _) = route(1);
        let current = conductor
            .begin_load(AgentInstanceId(1), sid.clone(), old)
            .await
            .expect("initial binding");
        let (new, _) = route(2);
        let conflict = conductor
            .begin_load(AgentInstanceId(1), sid.clone(), new)
            .await
            .expect_err("live binding must reject a competing load");

        assert!(matches!(
            conflict,
            BindError::LiveBinding(owner) if owner == current.token
        ));
        let (_, current) = conductor
            .route_for(AgentInstanceId(1), &sid)
            .await
            .expect("original route");
        assert_eq!(current.helper_id, HelperId(1));
    }

    #[tokio::test]
    async fn rollback_restores_claimed_orphan() {
        let conductor = SessionRouter::new();
        let sid = acp::schema::v1::SessionId::new("sid");
        let (original, _) = route(1);
        conductor
            .begin_load(AgentInstanceId(1), sid.clone(), original)
            .await
            .expect("initial binding");
        conductor
            .detach_helper(HelperId(1), Some(AgentInstanceId(1)))
            .await;
        let (replacement, _) = route(2);
        let failed = conductor
            .begin_load(AgentInstanceId(1), sid.clone(), replacement)
            .await
            .expect("orphan claim");
        assert!(failed.claimed_orphan);

        assert!(conductor.rollback_load(&failed).await);
        assert!(conductor.orphan_contains(AgentInstanceId(1), &sid).await);
    }

    #[tokio::test]
    async fn agent_reap_is_isolated_by_instance() {
        let conductor = SessionRouter::new();
        let sid = acp::schema::v1::SessionId::new("same");
        for (agent, helper) in [(AgentInstanceId(1), 1), (AgentInstanceId(2), 2)] {
            let (route, _) = route(helper);
            conductor
                .begin_load(agent, sid.clone(), route)
                .await
                .expect("binding");
            conductor.detach_helper(HelperId(helper), Some(agent)).await;
        }

        let reaped = conductor.reap_agent(AgentInstanceId(1)).await;
        assert_eq!(reaped.routes_removed, 0);
        assert!(!conductor.orphan_contains(AgentInstanceId(1), &sid).await);
        assert!(conductor.orphan_contains(AgentInstanceId(2), &sid).await);
    }

    #[tokio::test]
    async fn early_new_notification_is_deliverable_before_response_completion() {
        let conductor = SessionRouter::new();
        let agent = AgentInstanceId(1);
        let pending = conductor
            .begin_new(agent, HelperId(1))
            .await
            .expect("pending new");
        assert!(matches!(
            conductor
                .route_notification(agent, notification("new-sid"))
                .await,
            NotificationRoute::Buffered
        ));
        let (route, mut receiver) = route(1);
        let completed = conductor
            .finish_new_success(
                &pending,
                acp::schema::v1::SessionId::new("new-sid"),
                route,
            )
            .await
            .expect("live agent completion");
        assert_eq!(completed.buffered_enqueued, 1);
        assert_eq!(completed.buffered_dropped, 0);
        assert_eq!(
            receiver
                .try_recv()
                .expect("early notification delivered")
                .session_id
                .0
                .as_ref(),
            "new-sid"
        );
    }

    #[tokio::test]
    async fn pending_buffer_is_bounded_and_cleared_on_final_failure() {
        let conductor = SessionRouter::new();
        let agent = AgentInstanceId(1);
        let pending = conductor
            .begin_new(agent, HelperId(1))
            .await
            .expect("pending new");
        for _ in 0..MAX_PENDING_NOTIFICATIONS_PER_SESSION {
            assert!(matches!(
                conductor
                    .route_notification(agent, notification("sid"))
                    .await,
                NotificationRoute::Buffered
            ));
        }
        assert!(matches!(
            conductor
                .route_notification(agent, notification("sid"))
                .await,
            NotificationRoute::DroppedOverflow
        ));
        assert_eq!(
            conductor.finish_new_failure(&pending).await,
            MAX_PENDING_NOTIFICATIONS_PER_SESSION
        );
        assert!(matches!(
            conductor
                .route_notification(agent, notification("other"))
                .await,
            NotificationRoute::DroppedUnknown
        ));
    }

    #[tokio::test]
    async fn full_channel_does_not_remove_binding() {
        let conductor = SessionRouter::new();
        let agent = AgentInstanceId(1);
        let sid = acp::schema::v1::SessionId::new("sid");
        let (route, _receiver) = route(1);
        route
            .notif_tx
            .try_send(notification("sid"))
            .expect("fill route channel");
        route
            .notif_tx
            .try_send(notification("sid"))
            .expect("fill route channel");
        conductor
            .begin_load(agent, sid.clone(), route)
            .await
            .expect("binding");

        let NotificationRoute::Deliver {
            token: _,
            route,
            notification,
        } = conductor
            .route_notification(agent, notification("sid"))
            .await
        else {
            panic!("expected delivery snapshot");
        };
        assert!(matches!(
            route.notif_tx.try_send(notification),
            Err(mpsc::error::TrySendError::Full(_))
        ));
        assert!(conductor.route_for(agent, &sid).await.is_some());
    }

    #[tokio::test]
    async fn closed_channel_cleanup_cannot_remove_rebound_binding() {
        let conductor = SessionRouter::new();
        let agent = AgentInstanceId(1);
        let sid = acp::schema::v1::SessionId::new("sid");
        let (old_route, old_receiver) = route(1);
        let old = conductor
            .begin_load(agent, sid.clone(), old_route)
            .await
            .expect("old binding");
        drop(old_receiver);
        assert!(conductor.remove_current(&old.token, true).await);
        let (new_route, _new_receiver) = route(2);
        conductor
            .begin_load(agent, sid.clone(), new_route)
            .await
            .expect("new binding");

        assert!(!conductor.remove_current(&old.token, true).await);
        let (_, current) = conductor
            .route_for(agent, &sid)
            .await
            .expect("replacement binding");
        assert_eq!(current.helper_id, HelperId(2));
    }

    #[tokio::test]
    async fn publication_collision_keeps_first_agent_owner() {
        let conductor = SessionRouter::new();
        let sid = acp::schema::v1::SessionId::new("same");
        let (route_a, _) = route(1);
        let binding_a = conductor
            .begin_load(AgentInstanceId(1), sid.clone(), route_a)
            .await
            .expect("agent A binding");
        let (route_b, _) = route(2);
        let binding_b = conductor
            .begin_load(AgentInstanceId(2), sid, route_b)
            .await
            .expect("agent B binding");

        assert_eq!(
            conductor.publish(&binding_a.token, info("same")).await,
            PublishResult::Published
        );
        assert_eq!(
            conductor.publish(&binding_b.token, info("same")).await,
            PublishResult::Collision
        );
        assert!(matches!(
            conductor.unpublish(&binding_a.token).await,
            UnpublishResult::OwnerRemoved(RegistryChange {
                promoted: Some(_),
                ..
            })
        ));
        assert!(matches!(
            conductor.unpublish(&binding_b.token).await,
            UnpublishResult::OwnerRemoved(RegistryChange { promoted: None, .. })
        ));
    }

    #[tokio::test]
    async fn agent_reap_removes_routes_and_exact_publications() {
        let conductor = SessionRouter::new();
        let sid = acp::schema::v1::SessionId::new("sid");
        let (route, _) = route(1);
        let binding = conductor
            .begin_load(AgentInstanceId(1), sid.clone(), route)
            .await
            .expect("binding");
        assert_eq!(
            conductor.publish(&binding.token, info("sid")).await,
            PublishResult::Published
        );

        let reaped = conductor.reap_agent(AgentInstanceId(1)).await;

        assert_eq!(reaped.routes_removed, 1);
        assert_eq!(reaped.registry_changes.len(), 1);
        assert_eq!(reaped.registry_changes[0].removed, binding.token);
        assert!(conductor
            .route_for(AgentInstanceId(1), &sid)
            .await
            .is_none());
    }

    #[tokio::test]
    async fn agent_reap_blocks_late_new_and_load_commits() {
        let conductor = SessionRouter::new();
        let agent = AgentInstanceId(1);
        let pending = conductor
            .begin_new(agent, HelperId(1))
            .await
            .expect("pending new");
        conductor.reap_agent(agent).await;

        let (new_route, _) = route(1);
        assert!(conductor
            .finish_new_success(
                &pending,
                acp::schema::v1::SessionId::new("new"),
                new_route,
            )
            .await
            .is_err());
        let (load_route, _) = route(1);
        assert!(matches!(
            conductor
                .begin_load(agent, acp::schema::v1::SessionId::new("load"), load_route,)
                .await,
            Err(BindError::AgentReaped)
        ));
        assert!(conductor.begin_new(agent, HelperId(1)).await.is_none());
    }

    #[tokio::test]
    async fn duplicate_new_session_id_preserves_live_binding() {
        let conductor = SessionRouter::new();
        let agent = AgentInstanceId(1);
        let sid = acp::schema::v1::SessionId::new("same");
        let (existing_route, _) = route(1);
        conductor
            .begin_load(agent, sid.clone(), existing_route)
            .await
            .expect("existing binding");
        let pending = conductor
            .begin_new(agent, HelperId(2))
            .await
            .expect("pending new");
        let (duplicate_route, _) = route(2);

        assert!(matches!(
            conductor
                .finish_new_success(&pending, sid.clone(), duplicate_route)
                .await,
            Err(FinishNewError::LiveBinding)
        ));
        let (_, current) = conductor
            .route_for(agent, &sid)
            .await
            .expect("original binding");
        assert_eq!(current.helper_id, HelperId(1));
    }

    #[tokio::test]
    async fn helper_disconnect_cancels_pending_new_completion() {
        let conductor = SessionRouter::new();
        let agent = AgentInstanceId(1);
        let pending = conductor
            .begin_new(agent, HelperId(1))
            .await
            .expect("pending new");
        conductor.detach_helper(HelperId(1), Some(agent)).await;
        let (late_route, _) = route(1);

        assert!(matches!(
            conductor
                .finish_new_success(
                    &pending,
                    acp::schema::v1::SessionId::new("late"),
                    late_route,
                )
                .await,
            Err(FinishNewError::Cancelled)
        ));
    }

    #[tokio::test]
    async fn orphan_notifications_do_not_consume_pending_new_buffer() {
        let conductor = SessionRouter::new();
        let agent = AgentInstanceId(1);
        let orphan_sid = acp::schema::v1::SessionId::new("orphan");
        let (orphan_route, _) = route(1);
        conductor
            .begin_load(agent, orphan_sid.clone(), orphan_route)
            .await
            .expect("orphan binding");
        conductor.detach_helper(HelperId(1), Some(agent)).await;
        let pending = conductor
            .begin_new(agent, HelperId(2))
            .await
            .expect("pending new");

        assert!(matches!(
            conductor
                .route_notification(agent, notification("orphan"))
                .await,
            NotificationRoute::DroppedUnknown
        ));
        assert_eq!(conductor.finish_new_failure(&pending).await, 0);
    }
}
