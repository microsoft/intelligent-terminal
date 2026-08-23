//! Conversion of canonical host state into official AHP snapshot/list types.

use ahp_types::state::{
    ActiveTurn, ChatInteractivity, ChatOrigin, ChatState, ChatSummary, ErrorInfo,
    MarkdownResponsePart, Message, MessageKind, MessageOrigin, ResponsePart, RootState,
    SessionLifecycle, SessionState, SessionSummary, Snapshot, SnapshotState, Turn, TurnState,
};

use super::state::{
    DomainActiveTurn, DomainChat, DomainMessage, DomainResponsePart, DomainSession,
    DomainSessionLifecycle, DomainTurn, DomainTurnState, HostState,
};

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

    if let Some(session) = state.session(resource) {
        return Some(Snapshot {
            resource: resource.to_string(),
            state: SnapshotState::Session(Box::new(session_state(state, session))),
            from_seq,
        });
    }
    state.chat(resource).map(|chat| Snapshot {
        resource: resource.to_string(),
        state: SnapshotState::Chat(Box::new(chat_state(chat))),
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
        working_directories: session.working_directories.clone(),
        annotations: None,
        resource: session.resource_uri(),
        created_at: session.created_at.clone(),
        modified_at: session.modified_at.clone(),
        changes: None,
        meta: None,
    }
}

fn session_state(state: &HostState, session: &DomainSession) -> SessionState {
    SessionState {
        provider: session.provider.clone(),
        title: session.title.clone(),
        status: session.status,
        activity: session.activity.clone(),
        project: None,
        working_directories: session.working_directories.clone(),
        annotations: None,
        lifecycle: match session.lifecycle {
            DomainSessionLifecycle::Creating => SessionLifecycle::Creating,
            DomainSessionLifecycle::Ready => SessionLifecycle::Ready,
            DomainSessionLifecycle::CreationFailed => SessionLifecycle::CreationFailed,
        },
        creation_error: session.creation_error.as_ref().map(|error| ErrorInfo {
            error_type: error.error_type.clone(),
            message: error.message.clone(),
            stack: None,
            meta: None,
        }),
        server_tools: None,
        active_clients: Vec::new(),
        chats: state
            .chats_for_session(&session.id)
            .map(chat_summary)
            .collect(),
        default_chat: session.default_chat.clone(),
        config: None,
        customizations: None,
        changesets: None,
        input_needed: None,
        meta: None,
    }
}

fn chat_summary(chat: &DomainChat) -> ChatSummary {
    ChatSummary {
        resource: chat.resource_uri(),
        title: chat.title.clone(),
        status: chat.status,
        activity: chat.activity.clone(),
        modified_at: chat.modified_at.clone(),
        origin: Some(ChatOrigin::User),
        interactivity: Some(ChatInteractivity::Full),
        working_directories: None,
    }
}

fn chat_state(chat: &DomainChat) -> ChatState {
    ChatState {
        resource: chat.resource_uri(),
        title: chat.title.clone(),
        status: chat.status,
        activity: chat.activity.clone(),
        modified_at: chat.modified_at.clone(),
        origin: Some(ChatOrigin::User),
        interactivity: Some(ChatInteractivity::Full),
        working_directories: None,
        turns: chat.turns.iter().map(turn).collect(),
        turns_next_cursor: None,
        active_turn: chat.active_turn.as_ref().map(active_turn),
        steering_message: None,
        queued_messages: None,
        draft: None,
        meta: None,
    }
}

fn active_turn(turn: &DomainActiveTurn) -> ActiveTurn {
    ActiveTurn {
        id: turn.id.clone(),
        started_at: turn.started_at.clone(),
        message: message(&turn.message),
        response_parts: turn.response_parts.iter().map(response_part).collect(),
        usage: None,
    }
}

fn turn(turn: &DomainTurn) -> Turn {
    Turn {
        id: turn.id.clone(),
        started_at: Some(turn.started_at.clone()),
        duration: Some(turn.duration),
        message: message(&turn.message),
        response_parts: turn.response_parts.iter().map(response_part).collect(),
        usage: None,
        state: match turn.state {
            DomainTurnState::Complete => TurnState::Complete,
            DomainTurnState::Cancelled => TurnState::Cancelled,
            DomainTurnState::Error => TurnState::Error,
        },
        error: turn.error.as_ref().map(|error| ErrorInfo {
            error_type: error.error_type.clone(),
            message: error.message.clone(),
            stack: None,
            meta: None,
        }),
    }
}

fn message(message: &DomainMessage) -> Message {
    Message {
        text: message.text.clone(),
        origin: MessageOrigin {
            kind: MessageKind::User,
        },
        attachments: None,
        model: None,
        agent: None,
        meta: None,
    }
}

fn response_part(part: &DomainResponsePart) -> ResponsePart {
    ResponsePart::Markdown(MarkdownResponsePart {
        id: part.id.clone(),
        content: part.content.clone(),
    })
}
