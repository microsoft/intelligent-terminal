//! Master-owned SSH history and native pane bindings.
//!
//! Each execution source reuses the host/WSL registry and its event reducer,
//! without inserting remote IDs into the host registry or owning an ACP agent.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_client_protocol as acp;
use anyhow::{anyhow, Context};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use super::{broadcast_ext_to_helpers, MasterStateInner};
use crate::agent_sessions::{
    AgentSession, AgentStatus, CliSource, SessionEvent, SessionLocation, SessionOrigin,
};
use crate::session_registry::{
    agent_session_to_session_info, title_is_displayable, InMemoryRegistry, SessionRegistry,
};
use crate::ssh_hook_protocol::{HookEvent, RouteId};
use crate::ssh_session_registry::{build_changed_notification, Request, Snapshot, Source};

const MAX_EARLY_CLOSES: usize = 1024;
const HISTORY_POLL_INTERVAL: Duration = Duration::from_secs(5);

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
    refresh: Arc<Mutex<Option<Instant>>>,
    operations: Mutex<SourceMetadata>,
}

#[derive(Default)]
struct SourceMetadata {
    revision: u64,
    history_loaded: bool,
    listed_ids: HashSet<acp::schema::v1::SessionId>,
    outstanding_creates: usize,
    early_closes: HashSet<uuid::Uuid>,
    hook_keys: HashMap<(HookRoute, String), acp::schema::v1::SessionId>,
    raw_ids: HashMap<acp::schema::v1::SessionId, String>,
    row_routes: HashMap<acp::schema::v1::SessionId, HookRoute>,
    detached_native_panes: HashSet<uuid::Uuid>,
    pending_hooks: VecDeque<PendingHook>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum HookRoute {
    ManagedSsh(RouteId),
    NativeTmux(uuid::Uuid),
}

struct PendingHook {
    route: HookRoute,
    pane: String,
    event: HookEvent,
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
        Request::Snapshot { source }
        | Request::Poll { source }
        | Request::List { source, .. }
        | Request::Activate { source, .. } => source,
    };
    validate_source(state, source)?;
    let state = Arc::clone(state);
    // The task owns its mutations and completion. Dropping the requesting
    // helper's RPC/JoinHandle must not abandon a native create in another tab,
    // or cancel a history merge between its first row and revision publication.
    let snapshot = tokio::task::spawn_local(async move {
        match request {
            Request::Snapshot { source } => {
                let scoped = state.ssh_sessions.source(&source).await;
                Ok(snapshot(&state, &source, &scoped).await)
            }
            Request::Poll { source } => {
                let history_source = source.clone();
                Ok(poll(&state, &source, async move {
                    crate::ssh_sessions::list_sessions(
                        &history_source.target,
                        &history_source.agent_id,
                    )
                    .await
                })
                .await)
            }
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

async fn poll<F>(state: &Arc<MasterStateInner>, source: &Source, history: F) -> Snapshot
where
    F: Future<Output = anyhow::Result<Vec<AgentSession>>> + 'static,
{
    let scoped = state.ssh_sessions.source(source).await;
    let snapshot = snapshot(state, source, &scoped).await;
    let Ok(mut last_refresh) = Arc::clone(&scoped.refresh).try_lock_owned() else {
        return snapshot;
    };
    if last_refresh.is_some_and(|last| last.elapsed() < HISTORY_POLL_INTERVAL) {
        return snapshot;
    }
    let state = Arc::clone(state);
    let source = source.clone();
    tokio::task::spawn_local(async move {
        if let Err(error) =
            refresh_history(&state, &source, &scoped, &mut last_refresh, history).await
        {
            tracing::warn!(
                target: "ssh_sessions",
                destination = %source.target.display_name(),
                agent_id = %source.agent_id,
                ?error,
                "Background SSH session history refresh failed; retaining cached state"
            );
        }
    });
    snapshot
}

async fn refresh_history<F>(
    state: &MasterStateInner,
    source: &Source,
    scoped: &SourceState,
    last_refresh: &mut Option<Instant>,
    history: F,
) -> acp::Result<()>
where
    F: Future<Output = anyhow::Result<Vec<AgentSession>>>,
{
    let rows = history.await;
    // Throttle from completion, including failures. Hook-triggered cache
    // reads remain independent of this potentially slow remote operation.
    *last_refresh = Some(Instant::now());
    merge_history(state, source, scoped, rows.map_err(request_error)?).await;
    Ok(())
}

async fn list<F>(
    state: &MasterStateInner,
    source: &Source,
    force_refresh: bool,
    history: F,
) -> acp::Result<Snapshot>
where
    F: Future<Output = anyhow::Result<Vec<AgentSession>>>,
{
    let scoped = state.ssh_sessions.source(source).await;
    if !force_refresh
        && (scoped.operations.lock().await.history_loaded
            || !scoped.registry.snapshot().await.is_empty())
    {
        return Ok(snapshot(state, source, &scoped).await);
    }
    let mut last_refresh = scoped.refresh.lock().await;
    if force_refresh || !scoped.operations.lock().await.history_loaded {
        refresh_history(state, source, &scoped, &mut last_refresh, history).await?;
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
            if matches!(metadata.row_routes.get(&sid), Some(HookRoute::NativeTmux(pane)) if metadata.detached_native_panes.contains(pane))
            {
                return Err(request_error(anyhow!(
                    "The remote tmux session is still live but detached; attach its tmux session before focusing it"
                )));
            }
            // ResumeDispatched has already reserved this session. A second
            // helper sees pending Idle, and never starts a second native RPC.
            return Ok(snapshot_locked(state, source, &scoped, &metadata).await);
        };
        drop(metadata);
        focus(state, source, &scoped, &pane).await?;
        return Ok(snapshot(state, source, &scoped).await);
    }

    let raw_sid = metadata
        .raw_ids
        .get(&sid)
        .map(String::as_str)
        .unwrap_or(session_id);
    let cwd = info
        .cwd
        .to_str()
        .ok_or_else(|| request_error(anyhow!("SSH session cwd is not UTF-8")))?;
    let commandline = crate::ssh_sessions::resume_commandline(
        &source.target,
        &source.agent_id,
        raw_sid,
        cwd,
        state.ssh_hooks.enabled(),
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
            flush_pending_hooks(state, source, &scoped, &mut metadata, None).await;
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
    if closed_before_ack {
        metadata.pending_hooks.retain(|hook| hook.pane != pane);
    }
    flush_pending_hooks(state, source, &scoped, &mut metadata, Some(&pane)).await;
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
            let detached = uuid::Uuid::parse_str(pane).ok().filter(|pane_id| {
                metadata
                    .row_routes
                    .values()
                    .any(|route| *route == HookRoute::NativeTmux(*pane_id))
            });
            let event = if let Some(pane_id) = detached {
                metadata.detached_native_panes.insert(pane_id);
                SessionEvent::PaneDetached {
                    pane_session_id: pane.to_owned(),
                }
            } else {
                SessionEvent::PaneClosed {
                    pane_session_id: pane.to_owned(),
                }
            };
            if scoped.registry.apply_event(event).await {
                changed(state, source, &mut metadata).await;
            }
        }
        return Err(request_error(error.context("Focus SSH resume pane")));
    }
    Ok(())
}

pub(super) async fn pane_closed(state: &MasterStateInner, pane: &str) {
    pane_unavailable(state, pane, false).await;
}

pub(super) async fn pane_detached(state: &MasterStateInner, pane: &str) {
    pane_unavailable(state, pane, true).await;
}

async fn pane_unavailable(state: &MasterStateInner, pane: &str, detached: bool) {
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
        let canonical = pane_id.hyphenated().to_string();
        if detached {
            let native = HookRoute::NativeTmux(pane_id);
            if !metadata.row_routes.values().any(|route| *route == native)
                && !metadata
                    .pending_hooks
                    .iter()
                    .any(|hook| hook.route == native)
            {
                continue;
            }
            metadata.detached_native_panes.insert(pane_id);
        }
        metadata.pending_hooks.retain(|hook| hook.pane != canonical);
        if !detached
            && metadata.outstanding_creates > 0
            && metadata.early_closes.len() < MAX_EARLY_CLOSES
        {
            metadata.early_closes.insert(pane_id);
        }
        let event = if detached {
            SessionEvent::PaneDetached {
                pane_session_id: canonical,
            }
        } else {
            SessionEvent::PaneClosed {
                pane_session_id: canonical,
            }
        };
        if scoped.registry.apply_event(event).await {
            changed(state, &source, &mut metadata).await;
        }
    }
}

/// Apply a v3 hook only after the control reader has resolved its live local
/// route. The remote payload cannot select a host pane or an SSH source.
pub(super) async fn hook_event(
    state: &MasterStateInner,
    target: &crate::ssh_sessions::SshTarget,
    route: RouteId,
    pane: &str,
    event: HookEvent,
) -> acp::Result<()> {
    routed_hook_event(state, target, HookRoute::ManagedSsh(route), pane, event).await
}

/// The native controller supplies both the SSH target and pane identity after
/// validating a v2 frame. Its pane lifetime is separate from managed SSH routes.
pub(super) async fn tmux_hook_event(
    state: &MasterStateInner,
    target: &crate::ssh_sessions::SshTarget,
    pane: &str,
    event: HookEvent,
) -> acp::Result<()> {
    if !state.ssh_hooks.enabled() {
        return Ok(());
    }
    let pane_id = uuid::Uuid::parse_str(pane)
        .context("Native tmux hook has an invalid pane UUID")
        .map_err(request_error)?;
    if pane_id.is_nil() {
        return Err(request_error(anyhow!(
            "Native tmux hook has a nil pane UUID"
        )));
    }
    routed_hook_event(state, target, HookRoute::NativeTmux(pane_id), pane, event).await
}

async fn routed_hook_event(
    state: &MasterStateInner,
    target: &crate::ssh_sessions::SshTarget,
    route: HookRoute,
    pane: &str,
    event: HookEvent,
) -> acp::Result<()> {
    let source = Source {
        target: target.clone(),
        agent_id: event.cli_source.clone(),
    };
    validate_source(state, &source)?;
    if event.cli_source == "copilot" && event.raw_session_id.starts_with("sidekick-") {
        return Ok(());
    }
    let scoped = state.ssh_sessions.source(&source).await;
    let mut metadata = scoped.operations.lock().await;
    if matches!(route, HookRoute::NativeTmux(pane) if metadata.detached_native_panes.contains(&pane))
    {
        tracing::debug!(target: "ssh_sessions", "ignoring hook from a detached native attachment");
        return Ok(());
    }
    let pane = crate::agent_sessions::pane_key(pane);
    let pending = PendingHook { route, pane, event };
    let bound = scoped
        .registry
        .snapshot()
        .await
        .iter()
        .any(|row| row.pane_session_id.as_deref() == Some(pending.pane.as_str()));
    if !bound && metadata.outstanding_creates > 0 {
        // The native create acknowledgement identifies the reserved session's
        // pane. Do not let a CLI's bootstrap id steal that pending binding.
        if metadata.pending_hooks.len() >= 32 {
            return Err(request_error(anyhow!("SSH hook create-race queue is full")));
        }
        metadata.pending_hooks.push_back(pending);
        return Ok(());
    }
    if apply_hook_locked(&scoped, &source, &mut metadata, pending).await {
        changed(state, &source, &mut metadata).await;
    }
    Ok(())
}

async fn flush_pending_hooks(
    state: &MasterStateInner,
    source: &Source,
    scoped: &SourceState,
    metadata: &mut SourceMetadata,
    pane: Option<&str>,
) {
    let pending = std::mem::take(&mut metadata.pending_hooks);
    let mut updated = false;
    for hook in pending {
        if metadata.outstanding_creates == 0 || pane == Some(hook.pane.as_str()) {
            updated |= apply_hook_locked(scoped, source, metadata, hook).await;
        } else {
            metadata.pending_hooks.push_back(hook);
        }
    }
    if updated {
        changed(state, source, metadata).await;
    }
}

async fn apply_hook_locked(
    scoped: &SourceState,
    source: &Source,
    metadata: &mut SourceMetadata,
    hook: PendingHook,
) -> bool {
    let rows = scoped.registry.snapshot().await;
    let owner = rows
        .iter()
        .find(|row| row.pane_session_id.as_deref() == Some(hook.pane.as_str()));
    let raw_id = |row: &crate::session_registry::SessionInfo| {
        metadata
            .raw_ids
            .get(&row.session_id)
            .cloned()
            .unwrap_or_else(|| row.session_id.0.to_string())
    };
    let raw = if hook.event.raw_session_id.is_empty() {
        let Some(owner) = owner.filter(|row| {
            row.born_bound_pane || metadata.row_routes.get(&row.session_id) == Some(&hook.route)
        }) else {
            return false;
        };
        raw_id(owner)
    } else {
        hook.event.raw_session_id.clone()
    };
    if owner.is_some_and(|row| row.born_bound_pane && raw_id(row) != raw) {
        return false;
    }
    let pair = (hook.route, raw.clone());
    let cached = metadata.hook_keys.get(&pair);
    let cached_owner = cached.and_then(|key| rows.iter().find(|row| &row.session_id == key));
    let stale_cached_binding = cached_owner.is_some_and(|row| {
        row.pane_session_id
            .as_deref()
            .is_some_and(|pane| pane != hook.pane)
            || metadata
                .row_routes
                .get(&row.session_id)
                .is_some_and(|route| *route != hook.route)
    });
    if stale_cached_binding
        && !matches!(
            hook.event.event.as_str(),
            "agent.session.start" | "agent.prompt.submit"
        )
    {
        return false;
    }
    let sid = if let Some(key) = cached.filter(|_| !stale_cached_binding) {
        key.clone()
    } else if let Some(owner) = owner.filter(|row| raw_id(row) == raw) {
        owner.session_id.clone()
    } else {
        let canonical = acp::schema::v1::SessionId::new(raw.clone());
        if rows.iter().any(|row| {
            row.session_id == canonical
                && row
                    .pane_session_id
                    .as_deref()
                    .is_some_and(|pane| pane != hook.pane)
        }) {
            // Registry IDs may appear in diagnostics. Do not embed the route
            // token itself in a collision key that leaves this ownership map.
            let route_identity = match hook.route {
                HookRoute::ManagedSsh(route) => format!("ssh:{route}"),
                HookRoute::NativeTmux(pane) => format!("tmux:{pane}"),
            };
            let identity = format!("{route_identity}:{}:{}:{}", hook.pane, raw.len(), raw);
            acp::schema::v1::SessionId::new(format!(
                "ssh-hook:{:x}",
                Sha256::digest(identity.as_bytes())
            ))
        } else {
            canonical
        }
    };
    let known = rows.iter().find(|row| row.session_id == sid);
    if hook.event.event == "agent.error"
        && known.is_some()
        && owner.map(|row| &row.session_id) != Some(&sid)
    {
        return false;
    }
    let same_generation = known.is_some_and(|row| {
        (!matches!(
            row.status,
            Some(AgentStatus::Ended | AgentStatus::Historical)
        ) && row.pane_session_id.as_deref() == Some(hook.pane.as_str()))
            || metadata.row_routes.get(&sid) == Some(&hook.route)
    });
    let facts = crate::app::AgentEventFacts {
        key: sid.0.to_string(),
        session_known: same_generation,
    };
    let plan = crate::app::plan_agent_event(
        &hook.event.event,
        &hook.event.payload,
        &hook.pane,
        &hook.event.cli(),
        &facts,
    );
    if plan.events.is_empty() {
        return false;
    }
    let mut updated = false;
    for mut event in plan.events {
        if let SessionEvent::SessionStarted { title, .. } = &mut event {
            if known.is_some_and(|row| !crate::session_registry::title_is_synthetic(row)) {
                title.clear();
            }
        }
        updated |= scoped.registry.apply_event(event).await;
    }
    if scoped.registry.lookup(&sid).await.is_some() {
        updated |= scoped
            .registry
            .set_location(
                &sid,
                SessionLocation::Ssh {
                    target: source.target.clone(),
                },
            )
            .await;
        metadata.hook_keys.insert(pair, sid.clone());
        metadata.raw_ids.insert(sid.clone(), raw);
        metadata.row_routes.insert(sid, hook.route);
    }
    updated
}

#[cfg(test)]
#[path = "ssh_sessions_tests.rs"]
mod tests;
