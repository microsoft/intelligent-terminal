//! Master-owned SSH history and native pane bindings.
//!
//! Each execution source reuses the host/WSL registry and its event reducer,
//! without inserting remote IDs into the host registry or owning an ACP agent.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;

use agent_client_protocol as acp;
use anyhow::{anyhow, Context};
use tokio::sync::Mutex;

use super::{broadcast_ext_to_helpers, MasterStateInner};
use crate::agent_sessions::{
    AgentSession, AgentStatus, CliSource, SessionEvent, SessionLocation, SessionOrigin,
};
use crate::session_registry::{
    agent_session_to_session_info, title_is_displayable, InMemoryRegistry, SessionRegistry,
};
use crate::ssh_session_registry::{build_changed_notification, Request, Snapshot, Source};

const MAX_EARLY_CLOSES: usize = 1024;

pub(super) struct Service {
    epoch: uuid::Uuid,
    sources: Mutex<HashMap<Source, Arc<SourceState>>>,
}

impl Default for Service {
    fn default() -> Self {
        Self {
            epoch: uuid::Uuid::new_v4(),
            sources: Mutex::new(HashMap::new()),
        }
    }
}

impl Service {
    async fn source(&self, source: &Source) -> Arc<SourceState> {
        self.sources
            .lock()
            .await
            .entry(source.clone())
            .or_default()
            .clone()
    }
}

#[derive(Default)]
struct SourceState {
    registry: InMemoryRegistry,
    /// Only history readers wait on SSH; snapshots and mutations do not.
    refresh: Mutex<()>,
    operations: Mutex<SourceMetadata>,
}

#[derive(Default)]
struct SourceMetadata {
    revision: u64,
    history_loaded: bool,
    listed_ids: HashSet<acp::schema::v1::SessionId>,
    outstanding_creates: usize,
    early_closes: HashSet<uuid::Uuid>,
}

impl SourceMetadata {
    fn finished_create(&mut self) {
        self.outstanding_creates -= 1;
        if self.outstanding_creates == 0 {
            self.early_closes.clear();
        }
    }
}

fn validate_source(state: &MasterStateInner, source: &Source) -> acp::Result<()> {
    if !crate::agent_registry::is_known_id(&source.agent_id) {
        return Err(acp::Error::invalid_params().data(serde_json::json!({
            "message": "SSH sessions require a known built-in agent CLI id"
        })));
    }
    if state
        .allowed_agent_ids
        .as_ref()
        .is_some_and(|allowed| !allowed.contains(&source.agent_id))
    {
        return Err(acp::Error::invalid_request().data(serde_json::json!({
            "message": "SSH session agent is blocked by policy"
        })));
    }
    Ok(())
}

fn request_error(error: anyhow::Error) -> acp::Error {
    acp::Error::internal_error().data(serde_json::json!({ "message": format!("{error:#}") }))
}

pub(super) async fn handle(
    state: &Arc<MasterStateInner>,
    request: Request,
) -> acp::Result<acp::schema::v1::ExtResponse> {
    let source = match &request {
        Request::List { source, .. } | Request::Activate { source, .. } => source,
    };
    validate_source(state, source)?;
    let state = Arc::clone(state);
    // The task owns its mutations and completion. Dropping the requesting
    // helper's RPC/JoinHandle must not abandon a native create in another tab,
    // or cancel a history merge between its first row and revision publication.
    let snapshot = tokio::task::spawn_local(async move {
        match request {
            Request::List {
                source,
                refresh_history,
            } => {
                list(
                    &state,
                    &source,
                    refresh_history,
                    crate::ssh_sessions::list_sessions(&source.target, &source.agent_id),
                )
                .await
            }
            Request::Activate { source, session_id } => {
                activate(&state, &source, &session_id).await
            }
        }
    })
    .await
    .map_err(|error| request_error(anyhow!("SSH registry task failed: {error}")))??;
    let raw =
        serde_json::value::to_raw_value(&snapshot).map_err(|error| request_error(error.into()))?;
    Ok(acp::schema::v1::ExtResponse::new(raw.into()))
}

async fn snapshot(state: &MasterStateInner, source: &Source, scoped: &SourceState) -> Snapshot {
    let metadata = scoped.operations.lock().await;
    snapshot_locked(state, source, scoped, &metadata).await
}

async fn snapshot_locked(
    state: &MasterStateInner,
    source: &Source,
    scoped: &SourceState,
    metadata: &SourceMetadata,
) -> Snapshot {
    let mut sessions = scoped.registry.snapshot().await;
    sessions.sort_by(|a, b| a.session_id.0.cmp(&b.session_id.0));
    Snapshot {
        source: source.clone(),
        epoch: state.ssh_sessions.epoch,
        revision: metadata.revision,
        sessions,
    }
}

async fn changed(state: &MasterStateInner, source: &Source, metadata: &mut SourceMetadata) {
    metadata.revision += 1;
    broadcast_ext_to_helpers(state, build_changed_notification(source)).await;
}

async fn list<F>(
    state: &MasterStateInner,
    source: &Source,
    refresh_history: bool,
    history: F,
) -> acp::Result<Snapshot>
where
    F: Future<Output = anyhow::Result<Vec<AgentSession>>>,
{
    let scoped = state.ssh_sessions.source(source).await;
    if !refresh_history && scoped.operations.lock().await.history_loaded {
        return Ok(snapshot(state, source, &scoped).await);
    }
    let _refresh = scoped.refresh.lock().await;
    if refresh_history || !scoped.operations.lock().await.history_loaded {
        let rows = history.await.map_err(request_error)?;
        merge_history(state, source, &scoped, rows).await;
    }
    Ok(snapshot(state, source, &scoped).await)
}

/// Unbound Historical rows follow the latest history metadata. Live and Ended
/// rows retain their authoritative pane lifetime and only adopt displayable
/// titles, using the same registry primitives as host history.
async fn merge_history(
    state: &MasterStateInner,
    source: &Source,
    scoped: &SourceState,
    rows: Vec<AgentSession>,
) {
    let mut metadata = scoped.operations.lock().await;
    let listed_ids: HashSet<_> = rows
        .iter()
        .map(|row| acp::schema::v1::SessionId::new(row.key.clone()))
        .collect();
    let mut updated = !metadata.history_loaded;
    let cli = CliSource::parse(Some(&source.agent_id));
    for row in &rows {
        let mut info = agent_session_to_session_info(row);
        // Remote metadata is history, never a host pane or an origin claim.
        info.location = SessionLocation::Ssh {
            target: source.target.clone(),
        };
        info.cli_source = Some(cli.clone());
        info.origin = Some(SessionOrigin::Unknown);
        info.status = Some(AgentStatus::Historical);
        info.pane_session_id = None;
        if !title_is_displayable(Some(&cli), &row.title) {
            info.title = None;
        }
        match scoped.registry.lookup(&info.session_id).await {
            None => {
                scoped.registry.upsert_if_absent(info).await;
                updated = true;
            }
            Some(previous)
                if previous.status == Some(AgentStatus::Historical)
                    && previous.pane_session_id.is_none() =>
            {
                if info.title.is_none() {
                    info.title = previous.title.clone();
                }
                if info != previous {
                    scoped.registry.upsert(info).await;
                    updated = true;
                }
            }
            Some(_) => {
                if title_is_displayable(Some(&cli), &row.title)
                    && scoped
                        .registry
                        .adopt_agent_title(&info.session_id, &row.title)
                        .await
                {
                    updated = true;
                }
            }
        }
    }
    for sid in metadata.listed_ids.difference(&listed_ids) {
        if scoped
            .registry
            .remove_if(sid, &|row| {
                row.status == Some(AgentStatus::Historical) && row.pane_session_id.is_none()
            })
            .await
            .is_some()
        {
            updated = true;
        }
    }
    metadata.listed_ids = listed_ids;
    metadata.history_loaded = true;
    if updated {
        changed(state, source, &mut metadata).await;
    }
}

async fn activate(
    state: &MasterStateInner,
    source: &Source,
    session_id: &str,
) -> acp::Result<Snapshot> {
    let scoped = state.ssh_sessions.source(source).await;
    let sid = acp::schema::v1::SessionId::new(session_id.to_owned());
    let mut metadata = scoped.operations.lock().await;
    let info = scoped.registry.lookup(&sid).await.ok_or_else(|| {
        acp::Error::invalid_params().data(serde_json::json!({
            "message": "SSH session must first be listed in this source"
        }))
    })?;
    if !matches!(
        info.status,
        Some(AgentStatus::Historical | AgentStatus::Ended)
    ) {
        let Some(pane) = info.pane_session_id else {
            // ResumeDispatched has already reserved this session. A second
            // helper sees pending Idle, and never starts a second native RPC.
            return Ok(snapshot_locked(state, source, &scoped, &metadata).await);
        };
        drop(metadata);
        focus(state, source, &scoped, &pane).await?;
        return Ok(snapshot(state, source, &scoped).await);
    }

    let commandline = crate::ssh_sessions::resume_commandline(
        &source.target,
        &source.agent_id,
        session_id,
        info.cwd
            .to_str()
            .ok_or_else(|| request_error(anyhow!("SSH session cwd is not UTF-8")))?,
    )
    .map_err(request_error)?;
    let wt = state
        .wt
        .as_ref()
        .ok_or_else(|| request_error(anyhow!("SSH resume channel unavailable")))?;
    if scoped
        .registry
        .apply_event(SessionEvent::ResumeDispatched {
            key: session_id.to_owned(),
        })
        .await
    {
        metadata.outstanding_creates += 1;
        changed(state, source, &mut metadata).await;
    } else {
        return Ok(snapshot_locked(state, source, &scoped, &metadata).await);
    }
    drop(metadata);

    // POSIX cwd belongs inside the quoted SSH command, not WT's Windows cwd.
    let created = wt
        .request(
            "create_tab",
            serde_json::json!({ "commandline": commandline, "title": info.title }),
        )
        .await
        .and_then(|result| {
            let pane = crate::coordinator::resolve_created_pane_id(&result, "create_tab")?;
            let pane = uuid::Uuid::parse_str(&pane)
                .context("SSH create_tab returned invalid pane UUID")?;
            if pane.is_nil() {
                return Err(anyhow!("SSH create_tab returned nil pane UUID"));
            }
            Ok(pane)
        });

    let mut metadata = scoped.operations.lock().await;
    let pane = match created {
        Ok(pane) => pane,
        Err(error) => {
            metadata.finished_create();
            if scoped
                .registry
                .apply_event(SessionEvent::SessionStopped {
                    key: session_id.to_owned(),
                    reason: "SSH pane creation failed".to_owned(),
                })
                .await
            {
                changed(state, source, &mut metadata).await;
            }
            return Err(request_error(error.context("Create SSH resume pane")));
        }
    };
    let closed_before_ack = metadata.early_closes.contains(&pane);
    metadata.finished_create();
    let pane = pane.hyphenated().to_string();
    if scoped
        .registry
        .apply_event(SessionEvent::ResumePaneAssigned {
            key: session_id.to_owned(),
            pane_session_id: pane.clone(),
        })
        .await
    {
        changed(state, source, &mut metadata).await;
    }
    if closed_before_ack
        && scoped
            .registry
            .apply_event(SessionEvent::PaneClosed {
                pane_session_id: pane.clone(),
            })
            .await
    {
        changed(state, source, &mut metadata).await;
    }
    drop(metadata);
    if !closed_before_ack {
        // Preserve create-then-focus UX. Focus also supplies fresh liveness
        // evidence if a flood of unrelated events filled the bounded early
        // close set; infrastructure errors preserve the published binding.
        focus(state, source, &scoped, &pane).await?;
    }
    Ok(snapshot(state, source, &scoped).await)
}

async fn focus(
    state: &MasterStateInner,
    source: &Source,
    scoped: &SourceState,
    pane: &str,
) -> acp::Result<()> {
    let wt = state
        .wt
        .as_ref()
        .ok_or_else(|| request_error(anyhow!("SSH focus channel unavailable")))?;
    if let Err(error) = wt
        .request("focus_pane", serde_json::json!({ "session_id": pane }))
        .await
    {
        // Do not mistake missing wtcli.exe / COM infrastructure for a missing
        // pane. Only the native ERROR_NOT_FOUND HRESULT confirms closure.
        if format!("{error:#}")
            .to_ascii_lowercase()
            .contains("0x80070490")
        {
            let mut metadata = scoped.operations.lock().await;
            if scoped
                .registry
                .apply_event(SessionEvent::PaneClosed {
                    pane_session_id: pane.to_owned(),
                })
                .await
            {
                changed(state, source, &mut metadata).await;
            }
        }
        return Err(request_error(error.context("Focus SSH resume pane")));
    }
    Ok(())
}

pub(super) async fn pane_closed(state: &MasterStateInner, pane: &str) {
    let Ok(pane_id) = uuid::Uuid::parse_str(pane) else {
        return;
    };
    if pane_id.is_nil() {
        return;
    }
    let sources: Vec<_> = state
        .ssh_sessions
        .sources
        .lock()
        .await
        .iter()
        .map(|(source, scoped)| (source.clone(), Arc::clone(scoped)))
        .collect();
    for (source, scoped) in sources {
        let mut metadata = scoped.operations.lock().await;
        if metadata.outstanding_creates > 0 && metadata.early_closes.len() < MAX_EARLY_CLOSES {
            metadata.early_closes.insert(pane_id);
        }
        if scoped
            .registry
            .apply_event(SessionEvent::PaneClosed {
                pane_session_id: pane_id.hyphenated().to_string(),
            })
            .await
        {
            changed(state, &source, &mut metadata).await;
        }
    }
}

#[cfg(test)]
#[path = "ssh_sessions_tests.rs"]
mod tests;
