//! Migrate an already-bound conversation only after Terminal confirms its old identity.

use super::*;
use crate::agent_sessions::{pane_key, AgentStatus, CliSource, SessionLocation, SessionOrigin};
use crate::session_registry::{SessionIdentity, SessionInfo};
use crate::session_watcher::copilot_fork::CopilotFork;

pub(super) struct PendingFork {
    operation_id: String,
    source: SessionIdentity,
    target: acp::schema::v1::SessionId,
    dependent: std::collections::VecDeque<CopilotFork>,
    activity: std::collections::VecDeque<(std::time::Instant, crate::session_watcher::Emitted)>,
    pub(super) published_at: std::time::Instant,
}

const MAX_DEPENDENTS: usize = 16;
const MAX_ACTIVITY: usize = 128;
const MAX_ORPHAN_AGE: std::time::Duration = std::time::Duration::from_secs(30);
const MAX_ORPHANS: usize = 256;

#[derive(Default)]
pub(super) struct Orphans {
    records: std::collections::VecDeque<(std::time::Instant, OrphanWork)>,
}

enum OrphanWork {
    Fork(CopilotFork),
    Activity(crate::session_watcher::Emitted),
}

impl Orphans {
    fn prune(&mut self) {
        self.records
            .retain(|(created, _)| created.elapsed() < MAX_ORPHAN_AGE);
    }

    fn push(&mut self, work: OrphanWork) {
        self.prune();
        if self.records.len() == MAX_ORPHANS {
            self.records.pop_front();
        }
        self.records.push_back((std::time::Instant::now(), work));
    }
}

pub(super) fn is_host_identity(identity: &SessionIdentity) -> bool {
    identity.history_key
        == crate::session_registry::HistoryRowKey::new(
            "copilot",
            SessionLocation::Host,
            identity.session_id.0.as_ref(),
            None,
        )
}

pub(super) async fn is_reserved(state: &MasterStateInner, identity: &SessionIdentity) -> bool {
    if !is_host_identity(identity) {
        return false;
    }
    let pending = state.pending_copilot_forks.lock().await;
    pending.values().any(|plan| {
        plan.target == identity.session_id
            || plan
                .dependent
                .iter()
                .any(|fork| fork.target == identity.session_id.0.as_ref())
    })
}

fn owns_live_pane(row: &SessionInfo) -> bool {
    row.origin == Some(SessionOrigin::AgentPane)
        || (row.pane_session_id.is_some()
            && matches!(
                row.status,
                Some(
                    AgentStatus::Idle
                        | AgentStatus::Working
                        | AgentStatus::Attention
                        | AgentStatus::Error
                )
            ))
}

async fn target_owned(state: &MasterStateInner, key: &str) -> bool {
    state
        .registry
        .lookup_identity(&SessionIdentity {
            session_id: acp::schema::v1::SessionId::new(key.to_string()),
            history_key: crate::session_registry::HistoryRowKey::new(
                "copilot",
                SessionLocation::Host,
                key,
                None,
            ),
        })
        .await
        .is_some_and(|row| owns_live_pane(&row))
}

async fn attach_orphans(
    state: &MasterStateInner,
    plan: &mut PendingFork,
    reserved: &HashSet<String>,
) {
    let mut orphans = state.copilot_fork_orphans.lock().await;
    orphans.prune();
    loop {
        let tail = plan
            .dependent
            .back()
            .map_or(plan.target.0.as_ref(), |fork| fork.target.as_str());
        let Some(index) = orphans
            .records
            .iter()
            .position(|(_, work)| matches!(work, OrphanWork::Fork(fork) if fork.source == tail))
        else {
            break;
        };
        let Some((_, OrphanWork::Fork(fork))) = orphans.records.remove(index) else {
            break;
        };
        if plan.dependent.len() == MAX_DEPENDENTS {
            break;
        }
        if fork.target == plan.source.session_id.0.as_ref()
            || fork.target == plan.target.0.as_ref()
            || plan.dependent.iter().any(|f| f.target == fork.target)
            || reserved.contains(&fork.target)
            || target_owned(state, &fork.target).await
        {
            continue;
        }
        plan.dependent.push_back(fork);
    }
    let mut index = 0;
    while index < orphans.records.len() {
        let matches = matches!(&orphans.records[index].1, OrphanWork::Activity(event)
            if event.key == plan.target.0.as_ref()
                || plan.dependent.iter().any(|fork| fork.target == event.key));
        if matches {
            if let Some((created, OrphanWork::Activity(event))) = orphans.records.remove(index) {
                let position = plan
                    .activity
                    .iter()
                    .position(|(received, _)| *received > created)
                    .unwrap_or(plan.activity.len());
                plan.activity.insert(position, (created, event));
                if plan.activity.len() > MAX_ACTIVITY {
                    plan.activity.pop_front();
                }
            }
        } else {
            index += 1;
        }
    }
}

pub(super) async fn buffer_activity(
    state: &Arc<MasterStateInner>,
    emitted: &crate::session_watcher::Emitted,
) -> bool {
    if emitted.cli != CliSource::Copilot || !session_event_is_activity_only(&emitted.event) {
        return false;
    }
    let mut pending = state.pending_copilot_forks.lock().await;
    for plan in pending.values_mut() {
        if plan.target.0.as_ref() == emitted.key
            || plan.dependent.iter().any(|fork| fork.target == emitted.key)
        {
            if plan.activity.len() == MAX_ACTIVITY {
                plan.activity.pop_front();
            }
            plan.activity
                .push_back((std::time::Instant::now(), emitted.clone()));
            return true;
        }
    }
    // Only the watcher's fresh suffix reaches this boundary. Unowned activity
    // is evidence, never authority to create or move a registry binding.
    let sid = acp::schema::v1::SessionId::new(emitted.key.clone());
    let identity = SessionIdentity {
        session_id: sid,
        history_key: crate::session_registry::HistoryRowKey::new(
            "copilot",
            SessionLocation::Host,
            &emitted.key,
            None,
        ),
    };
    let activity_owned = state.hook_owned.lock().await.contains(&identity)
        || state.born_bound.lock().await.contains(&identity)
        || state
            .registry
            .lookup_identity(&identity)
            .await
            .is_some_and(|row| {
                row.origin == Some(SessionOrigin::AgentPane)
                    || matches!(
                        row.status,
                        Some(
                            AgentStatus::Idle
                                | AgentStatus::Working
                                | AgentStatus::Attention
                                | AgentStatus::Error
                        )
                    )
            });
    if !activity_owned {
        state
            .copilot_fork_orphans
            .lock()
            .await
            .push(OrphanWork::Activity(emitted.clone()));
        return true;
    }
    false
}

fn eligible(row: &SessionInfo) -> bool {
    row.provider_id.as_deref() == Some("copilot")
        && row.cli_source == Some(CliSource::Copilot)
        && row.location == SessionLocation::Host
        && row.session_universe.is_none()
        && row.origin != Some(SessionOrigin::AgentPane)
        && row
            .pane_session_id
            .as_ref()
            .is_some_and(|pane| !pane.is_empty())
        && matches!(
            row.status,
            Some(
                AgentStatus::Idle
                    | AgentStatus::Working
                    | AgentStatus::Attention
                    | AgentStatus::Error
            )
        )
}

pub(super) async fn observe(state: Arc<MasterStateInner>, fork: CopilotFork) {
    match request(&state, &fork).await {
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(target: "copilot_fork", %error, "could not publish fork binding");
            return;
        }
    }
    let pending = state.pending_copilot_forks.lock().await;
    if pending.values().any(|plan| {
        plan.source.session_id.0.as_ref() == fork.source
            || plan.target.0.as_ref() == fork.source
            || plan.dependent.iter().any(|f| f.target == fork.source)
    }) {
        return;
    }
    if !target_owned(&state, &fork.source).await {
        state
            .copilot_fork_orphans
            .lock()
            .await
            .push(OrphanWork::Fork(fork));
    }
}

pub(super) async fn request(state: &Arc<MasterStateInner>, fork: &CopilotFork) -> Result<bool> {
    let _admission = state.copilot_fork_admission.lock().await;
    let mut pending = state.pending_copilot_forks.lock().await;
    let reserved: HashSet<_> = pending
        .values()
        .flat_map(|plan| {
            std::iter::once(plan.target.to_string())
                .chain(plan.dependent.iter().map(|fork| fork.target.clone()))
        })
        .collect();
    // Only an outstanding, verified chain can admit an otherwise historical source.
    for plan in pending.values_mut() {
        let tail = plan
            .dependent
            .back()
            .map_or(plan.target.0.as_ref(), |f| f.target.as_str());
        if tail == fork.source {
            if plan.dependent.len() < MAX_DEPENDENTS
                && fork.target != plan.source.session_id.0.as_ref()
                && fork.target != plan.target.0.as_ref()
                && !plan.dependent.iter().any(|f| f.target == fork.target)
                && !reserved.contains(&fork.target)
                && !target_owned(state, &fork.target).await
            {
                plan.dependent.push_back(fork.clone());
                attach_orphans(state, plan, &reserved).await;
            }
            return Ok(true);
        }
    }
    let sid = acp::schema::v1::SessionId::new(fork.source.clone());
    let source_identity = SessionIdentity {
        session_id: sid,
        history_key: crate::session_registry::HistoryRowKey::new(
            "copilot",
            SessionLocation::Host,
            &fork.source,
            None,
        ),
    };
    let Some(row) = state.registry.lookup_identity(&source_identity).await else {
        return Ok(false);
    };
    if !eligible(&row) {
        return Ok(true);
    }
    let target = acp::schema::v1::SessionId::new(fork.target.clone());
    let target_identity = SessionIdentity {
        session_id: target.clone(),
        history_key: crate::session_registry::HistoryRowKey::new(
            "copilot",
            SessionLocation::Host,
            &fork.target,
            None,
        ),
    };
    if state
        .registry
        .lookup_identity(&target_identity)
        .await
        .is_some_and(|target| {
            target.origin == Some(SessionOrigin::AgentPane)
                || (target.pane_session_id.is_some()
                    && matches!(
                        target.status,
                        Some(
                            AgentStatus::Idle
                                | AgentStatus::Working
                                | AgentStatus::Attention
                                | AgentStatus::Error
                        )
                    ))
        })
    {
        tracing::debug!(target: "copilot_fork", "fork target already has a live owner");
        return Ok(true);
    }
    let pane = pane_key(
        row.pane_session_id
            .as_deref()
            .context("fork source pane missing")?,
    );
    if !state.wt.as_ref().is_some_and(|wt| wt.is_available()) {
        return Ok(true);
    }
    let source = SessionIdentity::from_info(&row);
    // Never replace an outstanding operation: its target may already be
    // published to Terminal and must stay reserved until that exact ACK.
    if pending.contains_key(&pane) {
        return Ok(true);
    }
    if pending
        .values()
        .any(|plan| plan.target == target || plan.dependent.iter().any(|f| f.target == fork.target))
    {
        return Ok(true);
    }
    let operation_id = uuid::Uuid::new_v4().to_string();
    let mut plan = PendingFork {
        operation_id: operation_id.clone(),
        source,
        target,
        dependent: Default::default(),
        activity: Default::default(),
        published_at: std::time::Instant::now(),
    };
    attach_orphans(state, &mut plan, &reserved).await;
    pending.insert(pane.clone(), plan);
    drop(pending);
    drop(_admission);
    publish(state, &pane, &operation_id, fork).await?;
    Ok(true)
}

async fn publish(
    state: &Arc<MasterStateInner>,
    pane: &str,
    operation_id: &str,
    fork: &CopilotFork,
) -> Result<()> {
    let result = if let Some(wt) = state.wt.as_ref().filter(|wt| wt.is_available()) {
        wt.request(
            "publish",
            serde_json::json!({
                "type": "event",
                "method": "pane_agent_session_changed",
                "params": {
                    "event": "agent.session.fork",
                    "agent": "copilot",
                    "pane_id": pane,
                    "previous_agent_session_id": fork.source,
                    "agent_session_id": fork.target,
                    "operation_id": operation_id,
                },
            }),
        )
        .await
    } else {
        Err(anyhow::anyhow!(
            "Terminal fork binding service is unavailable"
        ))
    };
    if let Err(error) = result {
        if !crate::shell::wt_channel::request_outcome_unknown(&error) {
            let mut pending = state.pending_copilot_forks.lock().await;
            if pending
                .get(pane)
                .is_some_and(|plan| plan.operation_id == operation_id)
            {
                pending.remove(pane);
            }
        }
        return Err(error).context("publish verified fork binding");
    }
    Ok(())
}

pub(super) async fn reject(state: &Arc<MasterStateInner>, params: &serde_json::Value) {
    let pane = pane_key(params["pane_id"].as_str().unwrap_or(""));
    let mut pending = state.pending_copilot_forks.lock().await;
    if pending.get(&pane).is_some_and(|plan| {
        params["operation_id"].as_str() == Some(plan.operation_id.as_str())
            && params["previous_agent_session_id"].as_str()
                == Some(plan.source.session_id.0.as_ref())
            && params["agent_session_id"].as_str() == Some(plan.target.0.as_ref())
    }) {
        pending.remove(&pane);
        tracing::debug!(target: "copilot_fork", pane, "Terminal rejected foreground Copilot fork binding");
    }
}

pub(super) async fn confirm(state: &Arc<MasterStateInner>, params: &serde_json::Value) {
    let _admission = state.copilot_fork_admission.lock().await;
    let pane = pane_key(params["pane_id"].as_str().unwrap_or(""));
    let mut pending = state.pending_copilot_forks.lock().await;
    let plan = {
        let Some(plan) = pending.get(&pane) else {
            return;
        };
        if params["operation_id"].as_str() != Some(plan.operation_id.as_str())
            || params["previous_agent_session_id"].as_str()
                != Some(plan.source.session_id.0.as_ref())
            || params["agent_session_id"].as_str() != Some(plan.target.0.as_ref())
        {
            return;
        }

        pending.remove(&pane)
    };
    let Some(mut plan) = plan else { return };
    let (first, second) = if plan.source.session_id.0 < plan.target.0 {
        (&plan.source.session_id, &plan.target)
    } else {
        (&plan.target, &plan.source.session_id)
    };
    let first_gate = session_lifecycle_gate(state, first).await;
    let second_gate = session_lifecycle_gate(state, second).await;
    let first_guard = first_gate.lock().await;
    let second_guard = second_gate.lock().await;
    let applied = state
        .registry
        .rebind_copilot_fork(&plan.source, &pane, &plan.target)
        .await;
    if applied {
        let mut hooked = state.hook_owned.lock().await;
        hooked.remove(&plan.source);
        let mut born = state.born_bound.lock().await;
        born.remove(&plan.source);
        let target_identity = SessionIdentity {
            session_id: plan.target.clone(),
            history_key: crate::session_registry::HistoryRowKey::new(
                "copilot",
                SessionLocation::Host,
                plan.target.0.as_ref(),
                None,
            ),
        };
        if !hooked.contains(&target_identity) {
            born.insert(target_identity);
        }
    }
    drop(second_guard);
    drop(first_guard);
    if applied {
        // Keep the pending lock through replay so subsequent watcher activity
        // cannot overtake the records consumed before Terminal's acknowledgement.
        let mut remaining = std::collections::VecDeque::new();
        for (created, event) in plan.activity {
            if event.key == plan.target.0.as_ref() {
                apply_watcher_event(state, event).await;
            } else {
                remaining.push_back((created, event));
            }
        }
        let successor = if let Some(fork) = plan.dependent.pop_front() {
            let row = state
                .registry
                .lookup_identity(&SessionIdentity {
                    session_id: plan.target.clone(),
                    history_key: crate::session_registry::HistoryRowKey::new(
                        "copilot",
                        SessionLocation::Host,
                        plan.target.0.as_ref(),
                        None,
                    ),
                })
                .await;
            let target = acp::schema::v1::SessionId::new(fork.target.clone());
            let target_row = state
                .registry
                .lookup_identity(&SessionIdentity {
                    session_id: target.clone(),
                    history_key: crate::session_registry::HistoryRowKey::new(
                        "copilot",
                        SessionLocation::Host,
                        &fork.target,
                        None,
                    ),
                })
                .await;
            let source = row
                .as_ref()
                .filter(|row| {
                    eligible(row)
                        && row.pane_session_id.as_deref().map(pane_key).as_deref()
                            == Some(pane.as_str())
                })
                .map(|row| SessionIdentity::from_info(row));
            if let Some(source) = source.filter(|_| {
                !target_row.as_ref().is_some_and(|row| {
                    row.origin == Some(SessionOrigin::AgentPane)
                        || (row.pane_session_id.is_some()
                            && matches!(
                                row.status,
                                Some(
                                    AgentStatus::Idle
                                        | AgentStatus::Working
                                        | AgentStatus::Attention
                                        | AgentStatus::Error
                                )
                            ))
                })
            }) {
                let operation_id = uuid::Uuid::new_v4().to_string();
                pending.insert(
                    pane.clone(),
                    PendingFork {
                        operation_id: operation_id.clone(),
                        source,
                        target,
                        dependent: plan.dependent,
                        activity: remaining,
                        published_at: std::time::Instant::now(),
                    },
                );
                Some((operation_id, fork))
            } else {
                None
            }
        } else {
            None
        };
        drop(pending);
        drop(_admission);
        if let Some((operation_id, fork)) = successor {
            if let Err(error) = publish(state, &pane, &operation_id, &fork).await {
                tracing::warn!(target: "copilot_fork", %error, "could not publish dependent fork binding");
            }
        }
        tracing::info!(target: "copilot_fork", pane, acknowledgment_delay_ms = plan.published_at.elapsed().as_millis() as u64, "confirmed foreground Copilot fork binding");
        broadcast_session_status_change(state, None).await;
        try_refresh_title_via_acp(state, &plan.target).await;
    } else {
        tracing::debug!(target: "copilot_fork", pane, "fork source binding was superseded before confirmation");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copilot_fork_orphans_are_bounded_and_keep_original_expiry() {
        let mut orphans = Orphans::default();
        for index in 0..MAX_ORPHANS + 1 {
            orphans.push(OrphanWork::Fork(CopilotFork {
                source: index.to_string(),
                target: (index + 1).to_string(),
            }));
        }
        assert_eq!(orphans.records.len(), MAX_ORPHANS);
        orphans.records.front_mut().unwrap().0 = std::time::Instant::now() - MAX_ORPHAN_AGE;
        orphans.prune();
        assert_eq!(orphans.records.len(), MAX_ORPHANS - 1);
    }
}
