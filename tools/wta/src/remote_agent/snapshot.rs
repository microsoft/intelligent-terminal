//! Conversion of canonical host state into official AHP snapshot/list types.

use ahp_types::state::{
    RootState, SessionLifecycle, SessionState, SessionSummary, Snapshot, SnapshotState,
};

use super::state::{DomainSession, HostState};

/// Build one fresh snapshot without storing any SDK wire object in host state.
pub(crate) fn snapshot_for(state: &HostState, resource: &str) -> Option<Snapshot> {
    let from_seq = i64::try_from(state.server_seq()).unwrap_or(i64::MAX);
    if resource == ahp_types::ROOT_RESOURCE_URI {
        let mut meta = serde_json::Map::new();
        meta.insert(
            "hostId".to_string(),
            serde_json::Value::String(state.host_id().0.clone()),
        );
        meta.insert(
            "hostResource".to_string(),
            serde_json::Value::String(state.host_id().resource_uri()),
        );
        return Some(Snapshot {
            resource: resource.to_string(),
            state: SnapshotState::Root(Box::new(RootState {
                agents: Vec::new(),
                active_sessions: Some(state.sessions().count() as i64),
                terminals: None,
                config: None,
                meta: Some(meta),
            })),
            from_seq,
        });
    }

    state.session(resource).map(|session| Snapshot {
        resource: resource.to_string(),
        state: SnapshotState::Session(Box::new(session_state(session))),
        from_seq,
    })
}

/// Builds all valid subscription snapshots and reports missing resources
/// separately, matching AHP reconnect semantics.
pub(crate) fn snapshots_for(
    state: &HostState,
    resources: &[String],
) -> (Vec<Snapshot>, Vec<String>) {
    let mut snapshots = Vec::new();
    let mut missing = Vec::new();
    for resource in resources {
        match snapshot_for(state, resource) {
            Some(snapshot) => snapshots.push(snapshot),
            None => missing.push(resource.clone()),
        }
    }
    (snapshots, missing)
}

pub(crate) fn list_session_summaries(state: &HostState) -> Vec<SessionSummary> {
    let mut summaries: Vec<_> = state.sessions().map(session_summary).collect();
    summaries.sort_by(|left, right| right.modified_at.cmp(&left.modified_at));
    summaries
}

pub(crate) fn session_summary(session: &DomainSession) -> SessionSummary {
    SessionSummary {
        provider: session.provider.clone(),
        title: session.title.clone(),
        status: session.status,
        activity: session.activity.clone(),
        project: None,
        working_directories: None,
        annotations: None,
        resource: session.resource_uri(),
        created_at: session.created_at.clone(),
        modified_at: session.modified_at.clone(),
        changes: None,
        meta: None,
    }
}

fn session_state(session: &DomainSession) -> SessionState {
    SessionState {
        provider: session.provider.clone(),
        title: session.title.clone(),
        status: session.status,
        activity: session.activity.clone(),
        project: None,
        working_directories: None,
        annotations: None,
        lifecycle: SessionLifecycle::Ready,
        creation_error: None,
        server_tools: None,
        active_clients: Vec::new(),
        chats: Vec::new(),
        default_chat: None,
        config: None,
        customizations: None,
        changesets: None,
        input_needed: None,
        meta: None,
    }
}
