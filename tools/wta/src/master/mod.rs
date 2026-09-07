// tools/wta/src/master/mod.rs
//
// `wta-master` mode — the singleton ACP multiplexer half of the
// helper+master architecture (see doc/specs/Multi-window-agent-pane.md).
//
// Responsibilities:
//   1. Spawn the agent CLI subprocess (claude / copilot / gemini)
//      and wrap its stdio in a `ConnectionTo<Agent>` (master is the
//      *client* of the agent CLI — same role that legacy wta plays
//      today). Built via the `conn.rs` shim (`ClientLink` /
//      `spawn_client`) so call sites keep the old `conn.method().await`
//      shape.
//   2. Listen on a named pipe (path supplied by the C++ side via
//      `--master <pipe-name>`). Accept one wta-helper per connect.
//   3. For each helper, run a `ConnectionTo<Client>` in which master
//      plays the *agent* role (via the shim: `AgentLink` /
//      `spawn_agent`). Forward helper requests to the agent CLI; route
//      inbound `session_notification`s from the agent CLI back to the
//      helper that owns the session.
//
// Forwarding paths:
//   * `helper → master → agent CLI`: every helper request runs
//     through `HelperHandler`'s dispatch (inherent fns on the
//     agent-side builder), a thin pass-through to the agent CLI's
//     `ClientLink`.
//   * `agent CLI → master → helper` (notifications): inbound
//     `session_notification`s land in `MasterClient::session_notification`
//     and are fanned out to the owning helper's notification channel
//     via the `session_to_helper` map (populated in `new_session` /
//     `load_session`).
//   * `agent CLI → master → helper` (requests — request_permission,
//     terminal/*, fs/*): same map carries each helper's `AgentLink`.
//     `MasterClient` looks up the helper by `args.session_id` and calls
//     the matching `AgentLink` method, which re-issues each call as an
//     RPC request over the helper's pipe. The helper-side `WtaClient`
//     then runs the same code path it ran pre-helper-split (TUI
//     permission UI, `ShellManager`, etc.).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, Weak};

/// Per-helper notification channel capacity. Sized for bursty chunk
/// streaming during a single agent turn; well above what a healthy
/// helper pipe needs to drain. If it fills up, the helper's pipe is
/// genuinely stuck and we'd rather drop chunks (with a warning) than
/// back-pressure the agent CLI's I/O loop and freeze every other
/// helper sharing this master.
const NOTIF_CHANNEL_CAPACITY: usize = 1024;
const SESSION_NEW_TIMEOUT_SECS: u64 = 120;
const SESSION_LOAD_TIMEOUT_SECS: u64 = 55;
const SESSION_ROLLBACK_CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const MASTER_PIPE_DISCOVERY_FILE: &str = "master-pipe.txt";

use agent_client_protocol as acp;
use anyhow::{anyhow, Context, Result};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::{mpsc, watch, Mutex, OnceCell};
use tokio::task::LocalSet;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::protocol::acp::conn;
use crate::protocol::acp::spawn::{
    spawn_agent_process_for_source_with_provider, AgentStderrLog, ChildEnvironmentPolicy,
    SharedProviderSelection,
};

pub(crate) mod config;
mod session_mcp;

use config::MasterConfig;

/// Opaque identifier for a helper connection. Used in logs only;
/// routing keys off `acp::schema::v1::SessionId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct HelperId(u64);

type AgentCmdKey = String;
type AgentInstanceId = uuid::Uuid;
type AgentCell = Arc<OnceCell<Arc<AgentCli>>>;
type WeakAgentCell = Weak<OnceCell<Arc<AgentCli>>>;

struct CustomModelGeneration {
    config: crate::custom_model_provider::Config,
    generation: u64,
}

#[derive(Clone)]
enum ProviderBinding {
    LegacyEnvironment,
    Native,
    Custom {
        selection_id: String,
        generation: u64,
        config: crate::custom_model_provider::Config,
    },
}

impl ProviderBinding {
    fn pool_key(&self) -> String {
        match self {
            Self::LegacyEnvironment => "legacy".to_string(),
            Self::Native => "native".to_string(),
            Self::Custom {
                selection_id,
                generation,
                ..
            } => format!("{selection_id}@{generation}"),
        }
    }

    fn spawn_selection(&self) -> SharedProviderSelection<'_> {
        match self {
            Self::LegacyEnvironment => SharedProviderSelection::Inherit,
            Self::Native => SharedProviderSelection::Disabled,
            Self::Custom { config, .. } => SharedProviderSelection::Custom(config),
        }
    }

    fn is_model_scoped(&self) -> bool {
        matches!(self, Self::Custom { .. })
    }

    fn has_active_custom_provider(&self) -> bool {
        match self {
            Self::LegacyEnvironment => crate::custom_model_provider::shared_provider_is_complete(),
            Self::Native => false,
            Self::Custom { .. } => true,
        }
    }
}

#[derive(Clone)]
enum RetirementOperationState {
    InFlight,
    Completed {
        event: serde_json::Value,
        completed_at: tokio::time::Instant,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TabRetirementPhase {
    Fencing,
    CompletedAwaitingDisconnect,
}

struct TabRetirementFence {
    phase: TabRetirementPhase,
    active_operations: usize,
    /// Helpers connected before the fence was established belong to the
    /// outgoing generation. A helper connected after Terminal observes
    /// completion is a replacement and may claim the tab.
    outgoing_helpers: HashSet<HelperId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TabRetirementTarget {
    helper_id: HelperId,
    requires_future_disconnect: bool,
}

#[derive(Default)]
struct AllRetirementFence {
    active_operations: usize,
    outgoing_helpers: HashSet<HelperId>,
}

#[derive(Debug)]
enum OwnerlessRetirementSafety {
    Targets(HashSet<String>),
    DenyNextOwner,
}

impl OwnerlessRetirementSafety {
    fn record(&mut self, tab_id: &str) {
        let Self::Targets(targets) = self else {
            return;
        };
        if targets.contains(tab_id) {
            return;
        }
        if targets.len() == OWNERLESS_RETIREMENT_TARGET_CAP {
            *self = Self::DenyNextOwner;
        } else {
            targets.insert(tab_id.to_string());
        }
    }

    fn rejects(&self, tab_id: &str) -> bool {
        match self {
            Self::Targets(targets) => targets.contains(tab_id),
            Self::DenyNextOwner => true,
        }
    }

    fn rekey(&mut self, old_tab_id: &str, new_tab_id: &str) {
        if let Self::Targets(targets) = self {
            if targets.remove(old_tab_id) {
                targets.insert(new_tab_id.to_string());
            }
        }
    }
}

/// Per-session routing entry. Owned by `session_to_helper` and
/// keyed by `acp::schema::v1::SessionId`.
///
/// Two reverse paths share this entry:
///   * `notif_tx`: master's `Client::session_notification` posts here;
///     the helper's `serve_helper` loop drains it and writes back
///     across the pipe.
///   * `forwarder`: master's `Client::request_permission` / `create_terminal`
///     / `terminal_*` / `read_text_file` / `write_text_file` calls
///     directly on this connection. `AgentSideConnection` itself
///     implements `acp::Client` and re-issues each call as an RPC
///     request to the helper.
///
/// `forwarder` is `Option<_>` for one reason only: unit tests below
/// construct routing entries without a real connection. The
/// production path (`new_session` / `load_session`) always sets it
/// to `Some(_)`, and `MasterClient` treats `None` as a routing bug.
#[derive(Clone)]
struct HelperRoute {
    helper_id: HelperId,
    agent_instance_id: AgentInstanceId,
    notif_tx: mpsc::Sender<acp::schema::v1::SessionNotification>,
    forwarder: Option<conn::AgentLink>,
    /// Per-route counter for back-pressure log rate-limiting.
    ///
    /// Chunk-streaming during a single agent turn is high-rate, so if
    /// a helper's pipe stalls and we drop notifications, naively
    /// `warn!`-ing on every drop would flood the log (and add I/O
    /// load right when the system is already strained). Instead the
    /// `session_notification` handler:
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
    consecutive_drops: Arc<std::sync::atomic::AtomicU64>,
}

/// State shared between the master's `acp::Client` impl (receives
/// notifications from the agent CLI) and each helper's `acp::Agent`
/// impl (receives requests from one helper).
struct MasterStateInner {
    /// Weak cache of per-SessionId lifecycle gates. Route mutations take the
    /// corresponding gate before `session_to_helper`; physical close holds it
    /// across the ACP round-trip without blocking unrelated SessionIds.
    session_lifecycle_gates:
        Mutex<HashMap<acp::schema::v1::SessionId, Weak<tokio::sync::Mutex<()>>>>,
    /// Routes inbound traffic from the agent CLI back to the helper
    /// that owns the session. Inserted by the helper's `new_session`
    /// / `load_session` handlers atomically (before responding to
    /// the helper), so no race window.
    ///
    /// `HelperRoute.helper_id` lets `drop_sessions_for_helper` reap
    /// every session belonging to a disconnecting helper without a
    /// secondary index. Without that cleanup the map would grow
    /// unboundedly across the master's lifetime — each closed pane
    /// leaves a dead `SessionId` behind, and every future
    /// notification for it lights up a "helper notification channel
    /// closed" warning.
    ///
    /// `HelperRoute.notif_tx` is a **bounded** mpsc with capacity
    /// `NOTIF_CHANNEL_CAPACITY`. Chunk-streaming notifications are
    /// high-rate, so an unbounded channel would let memory grow without
    /// bound if a helper's pipe write stalls. On a full channel we
    /// drop the notification + log a warning (see
    /// `MasterClient::session_notification`) rather than
    /// `await`-blocking the agent CLI's I/O loop — head-of-line
    /// blocking would freeze notification delivery for every other
    /// helper sharing this master.
    session_to_helper: Mutex<HashMap<acp::schema::v1::SessionId, HelperRoute>>,
    session_mcp_endpoints: session_mcp::Endpoints,
    session_mcp_capabilities: session_mcp::CapabilityRegistry,
    /// Latest Usage waiting for its owning helper. Context is replaced by
    /// SessionId while an omitted optional cost is retained from an
    /// undelivered prior update.
    pending_usage: Mutex<
        HashMap<acp::schema::v1::SessionId, (HelperId, acp::schema::v1::SessionNotification)>,
    >,
    usage_generation: watch::Sender<u64>,
    /// Authoritative live-session set, owned by master. Mirrors what
    /// helpers learn via ext-notifications and what the session management view sees
    /// via the standard ACP `session/list` request. Kept beside
    /// `session_to_helper` (rather than fused with it) so the
    /// per-row metadata that `SessionInfo` carries — cwd, future
    /// title/updated_at — has a typed home that isn't intertwined
    /// with notification-channel plumbing.
    ///
    /// Route-mutation lock ordering is per-session lifecycle gate, then
    /// `session_to_helper`, then subordinate state such as `registry`.
    /// Route reads do not need the lifecycle gate.
    pub(crate) registry: Arc<dyn crate::session_registry::SessionRegistry>,
    /// Per-helper subscribers for `intellterm.wta/*` ExtNotifications
    /// fanned out from master. Populated by `serve_helper` on connect
    /// and removed on disconnect (or whenever a send fails). Keyed by
    /// `HelperId` rather than `SessionId` because the deltas being
    /// broadcast are *about* SessionIds (added/removed) and every
    /// helper learns the full live set.
    ///
    /// Independent lock from `session_to_helper` and `registry`: the
    /// broadcast path (`broadcast_ext_to_helpers`) only takes this
    /// one, so it never blocks per-session routing or per-row reads
    /// of the registry.
    pub(crate) helper_ext_subscribers:
        Mutex<HashMap<HelperId, mpsc::UnboundedSender<acp::schema::v1::ExtNotification>>>,
    /// Shared `WtChannel` for outbound wtcli/COM calls — currently
    /// used only for `intellterm.wta/focus_session` (resolves a
    /// SessionId → pane_session_id via `registry`, then issues
    /// `request("focus_pane", { session_id: <pane_guid> })`).
    ///
    /// `Option` so unit tests can construct a `MasterStateInner`
    /// without spinning up a real wtcli channel; production sets
    /// `Some(Arc::new(CliChannel::connect().await?))` in
    /// `run_master_mode`. When `None`, `handle_focus_session` returns
    /// a structured `acp::Error` so the helper can fall back to its
    /// legacy resume path.
    pub(crate) wt: Option<Arc<dyn crate::shell::wt_channel::WtChannel>>,
    /// The pool of agent CLI subprocesses master is multiplexing,
    /// keyed by agent identity, execution source, and command line
    /// (`AgentCmdKey`). Lazily
    /// populated: a helper declares its agent *id* in the `initialize`
    /// handshake (`_meta.wta.agent_id`), the master reconstructs the
    /// command from that id (`agent_registry::build_acp_command`), and
    /// `get_or_spawn_agent` spawns the CLI on first use and reuses it for
    /// every later helper that resolves to the same identity, source, and
    /// command line. The key is always master-derived, never a string off
    /// the pipe.
    /// This is what lets one tab run Gemini while another runs Claude in
    /// the same window.
    ///
    /// Each value is an `Arc<OnceCell<…>>` so two helpers racing the
    /// *same* new agent serialize on that key's init (one spawns, the
    /// other awaits the same `AgentCli`), while helpers for *different*
    /// agents spawn in parallel — we hold the outer `Mutex` only long
    /// enough to get/insert the `OnceCell`, never across the spawn.
    ///
    /// **Pool eviction policy:** agents are kept warm for the lifetime of
    /// the master process (no idle-timeout eviction). The expected pool
    /// cardinality is small — one entry per distinct agent-id selected by
    /// any tab in the window — so the memory/process overhead is bounded
    /// by the number of GPO-allowed agents (typically 1–3). An agent that
    /// crashes is reaped and removed by `reap_agent`; its slot is refilled
    /// lazily on the next helper request. Idle-timeout eviction would save
    /// a background process at the cost of cold-start latency for the next
    /// tab switch; that trade-off favors warm agents for a terminal app.
    pub(crate) agents: Mutex<HashMap<AgentCmdKey, AgentCell>>,
    /// Master-only BYOK configurations keyed by the credential-free selection
    /// ID. A changed endpoint/model/credential reference advances the
    /// generation and therefore gets a new agent-pool entry.
    custom_model_generations: Mutex<HashMap<String, CustomModelGeneration>>,
    /// Fallback agent command line + id for helpers that don't declare
    /// their own in `_meta.wta` (older helper builds, or the rare
    /// manual launch). Comes from the master's own `--agent` / `--agent-id`,
    /// which the C++ side still passes as the global default. This command
    /// is **trusted** (it came from the master's own argv, not the pipe),
    /// so a rejected/unknown helper request safely falls back to it.
    pub(crate) default_agent_cmd: String,
    pub(crate) default_agent_id: Option<String>,
    /// Allowlist of agent ids a helper may select over the pipe, from the
    /// host's GPO-filtered set (`--allowed-agent-ids`). `None` = the flag was
    /// absent (manual runs / older hosts): any *known* agent id is accepted.
    /// `Some(set)` = the flag was supplied, honored fail-closed: only ids in
    /// `set` are honored; any other id (and *every* id when `set` is empty)
    /// falls back to the trusted default. Either way the master reconstructs
    /// the command from the id and never spawns a string taken off the pipe.
    pub(crate) allowed_agent_ids: Option<std::collections::HashSet<String>>,
    /// Per-helper tab/session ownership metadata, keyed by `HelperId`.
    ///
    /// Populated/refreshed by the `new_session` + `load_session`
    /// handlers, which see the helper-supplied `_meta.wta.owner_tab_id`
    /// and the resulting `SessionId`. Close-by-tab uses it to find the
    /// exact helper/session pair even while a session transaction is in
    /// flight. One entry per helper; `last_session_id` is the most
    /// recently created or loaded session.
    ///
    /// Independent lock from `session_to_helper` so the per-session
    /// routing hot path never contends on it.
    pub(crate) helper_meta: Mutex<HashMap<HelperId, HelperRecoveryMeta>>,
    /// Serializes publication and rename of helper/tab ownership across the
    /// pending transaction map, recovery metadata, and retirement fences.
    tab_ownership_gate: Mutex<()>,
    /// Helpers whose pipe connected before a tab retirement fence. Membership
    /// gives retirement an explicit helper generation boundary even when the
    /// helper has not published its tab owner yet.
    connected_helpers: Mutex<HashSet<HelperId>>,
    /// Active process-wide retirement generation. Helpers admitted while this
    /// fence is active join the outgoing generation before they can publish an
    /// owner or start a session transaction.
    all_retirement_fence: Mutex<AllRetirementFence>,
    /// Destructive retirement fences keyed by stable tab id. A fence blocks
    /// the outgoing helper generation through completion and is consumed by
    /// its disconnect or by the first post-completion replacement helper.
    tab_retirement_fences: Mutex<HashMap<String, TabRetirementFence>>,
    /// Stable-id moves for active tab retirement transactions. Entries exist
    /// only while an operation still needs to follow a tab across a drag.
    tab_retirement_rekeys: Mutex<HashMap<String, String>>,
    /// Retired tab ids that an ownerless connected helper could still claim.
    /// State is bounded per HelperId; overflow conservatively denies that
    /// connection's first owner publication instead of evicting safety.
    unresolved_owner_retirements: Mutex<HashMap<HelperId, OwnerlessRetirementSafety>>,
    /// Helpers with a session/new or session/load transaction in flight.
    /// Tab-close keeps their recovery metadata until the response arrives so
    /// the newly created/loaded session can be closed before it is exposed.
    pending_session_helpers: Mutex<HashMap<HelperId, Option<String>>>,
    /// Unbound session MCP capability owned by each in-flight session
    /// transaction. Retirement can revoke it before the provider responds.
    pending_session_mcp: Mutex<HashMap<HelperId, session_mcp::PendingCapability>>,
    /// Helpers whose owning tab was destroyed while a session transaction was
    /// in flight. The transaction checks this before committing its response.
    closing_session_helpers: Mutex<HashSet<HelperId>>,
    /// Closing helpers participating in a destructive retirement. For
    /// `scope=all`, every helper connected at operation start is captured here
    /// even if it has not published owner metadata yet. HelperIds are unique
    /// for the master lifetime, so later replacement helpers are not blocked.
    /// Late session results use logical fallback instead of preserving routes.
    destructive_session_helpers: Mutex<HashSet<HelperId>>,
    /// Destructive helpers whose retirement transaction is still collecting
    /// late session cleanup outcomes. Unlike the destructive tombstone, this
    /// entry is removed by the forced cleanup epilogue.
    active_retirement_helpers: Mutex<HashSet<HelperId>>,
    /// Physical/logical outcome produced by a late session transaction.
    closing_session_results: Mutex<HashMap<HelperId, ReplacedSessionCleanup>>,
    /// Wakes destructive retirement transactions waiting for an in-flight
    /// session/new or session/load to consume its closing marker.
    session_transaction_changed: tokio::sync::Notify,
    /// Process-wide idempotency state for Terminal retirement transactions.
    retirement_operations: Mutex<HashMap<String, RetirementOperationState>>,
    #[cfg(test)]
    retirement_completion_tx: Mutex<Option<mpsc::UnboundedSender<serde_json::Value>>>,
    #[cfg(test)]
    retirement_pending_timeout: std::time::Duration,
    #[cfg(test)]
    disconnect_orphan_publication_pause: Mutex<Option<Arc<DisconnectOrphanPublicationPause>>>,
    #[cfg(test)]
    reap_agent_orphan_cleanup_pause: Mutex<Option<Arc<ReapAgentOrphanCleanupPause>>>,
    #[cfg(test)]
    deferred_retirement_cleanup_complete: tokio::sync::Notify,
    /// Session ids claimed by an *authoritative* producer — a native agent hook
    /// (arrives via `intellterm.wta/session_hook`) or an ACP agent-pane
    /// session (driven by ACP `session/*`), both of which fully own binding and
    /// activity. The hookless file watcher is a **fallback** only: once a session
    /// id appears here, its watcher-emitted events are dropped in
    /// [`apply_watcher_event`] so hooks and the watcher never double-track the
    /// same session.
    /// double-track the same session. This is what lets a CLI that ships hooks
    /// (and the WTA-launched born-bound sessions) keep their exact, hook-sourced
    /// pane binding while the watcher still covers user-typed CLIs that have no
    /// hook installed (notably Codex's Restart-Manager fallback).
    ///
    /// Grow-only for the master's lifetime: a dead session id costs a few bytes
    /// and re-adding is idempotent, so no eviction is needed. Independent lock —
    /// touched only on the session_hook ingest path and the watcher apply path.
    hook_owned: Mutex<HashSet<acp::schema::v1::SessionId>>,
    /// Sessions loaded on a shared agent CLI whose owning helper has
    /// disconnected (its tab/pane closed) — the CLI keeps them loaded as
    /// "orphans". Keyed by `AgentCmdKey` so orphans belong to a specific
    /// agent CLI, never a global pool: a window can run Copilot in one tab
    /// and Gemini in another, and reaping one must not affect the other.
    ///
    /// When a helper resumes such a session (`--initial-load-session-id`
    /// re-warm or `/restart`), `load_session` re-binds routing to the new
    /// helper *directly* — no fresh `session/load` — because the CLI already
    /// has it (a re-load would be rejected "already loaded", or, if the
    /// orphan turn is still running, wedge behind it and hang the pane on
    /// "Resuming…"). Only recorded while the owning CLI *instance* is still
    /// the live pool entry (checked under the `agents` lock), and `reap_agent`
    /// drops just that agent's set on CLI death, so a crashed-and-respawned CLI
    /// under the same command line never re-binds to a session it never had —
    /// such a resume falls back to a real `session/load` from disk.
    orphaned_sessions: Mutex<HashMap<AgentCmdKey, HashSet<acp::schema::v1::SessionId>>>,
    /// Generations whose initializer was cancelled before its `OnceCell`
    /// published a value. This uses a synchronous mutex so the initializer's
    /// drop guard can tombstone the generation before `get_or_try_init` wakes
    /// queued waiters.
    retired_agent_cells: std::sync::Mutex<Vec<WeakAgentCell>>,
    /// Stable tab identity retained when a helper disconnect wins the race
    /// against the terminal's close-by-tab request. This lets a surviving
    /// helper physically close the now-orphaned ACP session milliseconds later.
    orphaned_tabs: Mutex<HashMap<String, (AgentCmdKey, HelperId, acp::schema::v1::SessionId)>>,
    /// #266 born-bound sessions (WTA-launched delegate/resume — copilot/claude/
    /// gemini). **Binding-only**: unlike `hook_owned`, the file watcher may
    /// still supply STATUS for these when no real hook is installed
    /// (activity-only, never re-binding the pane). A subsequent real hook moves
    /// the session into `hook_owned` and out of here, after which the watcher
    /// fully backs off.
    born_bound: Mutex<HashSet<acp::schema::v1::SessionId>>,
}

#[cfg(test)]
#[derive(Default)]
struct DisconnectOrphanPublicationPause {
    routes_dropped: tokio::sync::Notify,
    resume_publication: tokio::sync::Notify,
}

#[cfg(test)]
#[derive(Default)]
struct ReapAgentOrphanCleanupPause {
    agent_removed: tokio::sync::Notify,
    resume_cleanup: tokio::sync::Notify,
}

async fn session_lifecycle_gate(
    state: &MasterStateInner,
    session_id: &acp::schema::v1::SessionId,
) -> Arc<tokio::sync::Mutex<()>> {
    let mut gates = state.session_lifecycle_gates.lock().await;
    gates.retain(|_, gate| gate.strong_count() > 0);
    if let Some(gate) = gates.get(session_id).and_then(Weak::upgrade) {
        return gate;
    }
    let gate = Arc::new(tokio::sync::Mutex::new(()));
    gates.insert(session_id.clone(), Arc::downgrade(&gate));
    gate
}

async fn bind_session_route(
    state: &MasterStateInner,
    session_id: acp::schema::v1::SessionId,
    route: HelperRoute,
) -> usize {
    let gate = session_lifecycle_gate(state, &session_id).await;
    let _guard = gate.lock().await;
    let mut routes = state.session_to_helper.lock().await;
    let mut pending_usage = state.pending_usage.lock().await;
    pending_usage.remove(&session_id);
    routes.insert(session_id, route);
    routes.len()
}

async fn swap_session_route(
    state: &MasterStateInner,
    session_id: acp::schema::v1::SessionId,
    route: HelperRoute,
) -> Option<HelperRoute> {
    let gate = session_lifecycle_gate(state, &session_id).await;
    let _guard = gate.lock().await;
    let mut routes = state.session_to_helper.lock().await;
    state.pending_usage.lock().await.remove(&session_id);
    routes.insert(session_id, route)
}

async fn rollback_swapped_session_route(
    state: &MasterStateInner,
    helper_id: HelperId,
    session_id: &acp::schema::v1::SessionId,
    previous: Option<HelperRoute>,
) -> SwappedSessionRouteRollback {
    let gate = session_lifecycle_gate(state, session_id).await;
    let _guard = gate.lock().await;
    let mut routes = state.session_to_helper.lock().await;
    rollback_swapped_session_route_locked(&mut routes, helper_id, session_id, previous)
}

async fn rollback_orphan_rebind(
    state: &MasterStateInner,
    helper_id: HelperId,
    agent_key: &AgentCmdKey,
    expected_agent_instance_id: AgentInstanceId,
    session_id: &acp::schema::v1::SessionId,
    previous: Option<HelperRoute>,
) -> SwappedSessionRouteRollback {
    let gate = session_lifecycle_gate(state, session_id).await;
    let _guard = gate.lock().await;
    let (rollback, route_absent) = {
        let mut routes = state.session_to_helper.lock().await;
        let rollback =
            rollback_swapped_session_route_locked(&mut routes, helper_id, session_id, previous);
        (rollback, !routes.contains_key(session_id))
    };
    if rollback == SwappedSessionRouteRollback::Restored && route_absent {
        let agents = state.agents.lock().await;
        let expected_instance_is_current = agents
            .get(agent_key)
            .and_then(|cell| cell.get())
            .is_some_and(|agent| agent.instance_id == expected_agent_instance_id);
        if expected_instance_is_current {
            state
                .orphaned_sessions
                .lock()
                .await
                .entry(agent_key.clone())
                .or_default()
                .insert(session_id.clone());
        }
    }
    rollback
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SwappedSessionRouteRollback {
    Restored,
    OwnershipChanged {
        current_agent_instance_id: Option<AgentInstanceId>,
    },
}

fn rollback_swapped_session_route_locked(
    routes: &mut HashMap<acp::schema::v1::SessionId, HelperRoute>,
    helper_id: HelperId,
    session_id: &acp::schema::v1::SessionId,
    previous: Option<HelperRoute>,
) -> SwappedSessionRouteRollback {
    if !routes
        .get(session_id)
        .is_some_and(|route| route.helper_id == helper_id)
    {
        return SwappedSessionRouteRollback::OwnershipChanged {
            current_agent_instance_id: routes.get(session_id).map(|route| route.agent_instance_id),
        };
    }
    if let Some(previous) = previous {
        routes.insert(session_id.clone(), previous);
    } else {
        routes.remove(session_id);
    }
    SwappedSessionRouteRollback::Restored
}

// Copilot occasionally needs several seconds to unwind a cancelled turn or
// per-session MCP process before acknowledging session/close. Keep this below
// the E2E/user-visible 20s teardown budget while avoiding a false orphan leak
// on transient 5s stalls observed in live runs. SharedWta.cpp keeps pane-driven
// master teardown alive for 16s; keep that grace strictly above this timeout.
const SESSION_CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const RETIREMENT_COMPLETION_TTL: std::time::Duration = std::time::Duration::from_secs(5 * 60);
const RETIREMENT_COMPLETION_CAP: usize = 256;
const OWNERLESS_RETIREMENT_TARGET_CAP: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReplacedSessionCleanup {
    NotOwned,
    PhysicallyClosed,
    LogicalFallback,
}

/// Close and retire one session owned by a helper.
///
/// The per-SessionId lifecycle gate stays held across ownership validation,
/// the bounded `session/close` RPC (or unsupported-agent cancel fallback), all
/// WTA cleanup, and removal broadcasts. The global route lock is held only for
/// short map operations, so agent callbacks can still perform route lookups
/// while the close response is pending.
async fn close_and_retire_replaced_session(
    state: &MasterStateInner,
    helper_id: HelperId,
    agent: &AgentCli,
    session_id: &acp::schema::v1::SessionId,
    timeout: std::time::Duration,
) -> acp::Result<ReplacedSessionCleanup> {
    close_and_retire_owned_session(
        state,
        helper_id,
        agent,
        session_id,
        tokio::time::Instant::now() + timeout,
        false,
    )
    .await
}

async fn close_and_retire_owned_session(
    state: &MasterStateInner,
    helper_id: HelperId,
    agent: &AgentCli,
    session_id: &acp::schema::v1::SessionId,
    deadline: tokio::time::Instant,
    retire_on_close_failure: bool,
) -> acp::Result<ReplacedSessionCleanup> {
    let gate = session_lifecycle_gate(state, session_id).await;
    let _guard = tokio::time::timeout_at(deadline, gate.lock())
        .await
        .map_err(|_| retirement_deadline_error(session_id, "lifecycle_gate"))?;
    {
        let routes = state.session_to_helper.lock().await;
        if !routes.get(session_id).is_some_and(|route| {
            route.helper_id == helper_id && route.agent_instance_id == agent.instance_id
        }) {
            return Ok(ReplacedSessionCleanup::NotOwned);
        }
    }

    let cleanup = if !agent_supports_session_close(agent) {
        tracing::warn!(
            target: "master",
            step = "helper→agent",
            op = "close_replaced_session",
            helper_id = ?helper_id,
            old_session_id = %session_id,
            outcome = "unsupported_logical_fallback",
            "agent does not advertise session/close; cancelling best-effort and retiring only WTA state"
        );
        let remaining = retirement_remaining(deadline);
        if remaining.is_zero() {
            return Err(retirement_deadline_error(session_id, "cancel"));
        }
        match tokio::time::timeout(
            remaining,
            agent
                .conn
                .cancel(acp::schema::v1::CancelNotification::new(session_id.clone())),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(
                    target: "master",
                    step = "helper→agent",
                    op = "cancel_replaced_session",
                    helper_id = ?helper_id,
                    old_session_id = %session_id,
                    error = %error,
                    "legacy session/cancel fallback failed"
                );
            }
            Err(_) => return Err(retirement_deadline_error(session_id, "cancel")),
        }
        ReplacedSessionCleanup::LogicalFallback
    } else {
        let remaining = retirement_remaining(deadline);
        if remaining.is_zero() {
            return Err(retirement_deadline_error(session_id, "cancel"));
        }
        match tokio::time::timeout(
            remaining,
            agent
                .conn
                .cancel(acp::schema::v1::CancelNotification::new(session_id.clone())),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(
                    target: "master",
                    step = "helper→agent",
                    op = "cancel_replaced_session",
                    helper_id = ?helper_id,
                    old_session_id = %session_id,
                    error = %error,
                    "failed to cancel active turn before session/close"
                );
            }
            Err(_) => return Err(retirement_deadline_error(session_id, "cancel")),
        }
        let started = std::time::Instant::now();
        let remaining = retirement_remaining(deadline);
        if remaining.is_zero() {
            return Err(retirement_deadline_error(session_id, "session_close"));
        }
        match tokio::time::timeout(
            remaining,
            agent
                .conn
                .close_session(acp::schema::v1::CloseSessionRequest::new(
                    session_id.clone(),
                )),
        )
        .await
        {
            Ok(Ok(_)) => {
                tracing::info!(
                    target: "master",
                    step = "helper→agent",
                    op = "close_replaced_session",
                    helper_id = ?helper_id,
                    old_session_id = %session_id,
                    outcome = "closed",
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "physically closed replaced ACP session"
                );
                ReplacedSessionCleanup::PhysicallyClosed
            }
            Ok(Err(error)) => {
                if error.code == acp::ErrorCode::MethodNotFound {
                    tracing::warn!(
                        target: "master",
                        step = "helper→agent",
                        op = "close_replaced_session",
                        helper_id = ?helper_id,
                        old_session_id = %session_id,
                        outcome = "unsupported_logical_fallback",
                        error = %error,
                        "agent advertised session/close but rejected it; retiring only WTA state"
                    );
                    ReplacedSessionCleanup::LogicalFallback
                } else {
                    tracing::error!(
                        target: "master",
                        step = "helper→agent",
                        op = "close_replaced_session",
                        helper_id = ?helper_id,
                        old_session_id = %session_id,
                        outcome = "acp_error",
                        error = %error,
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "failed to physically close replaced ACP session"
                    );
                    if retire_on_close_failure {
                        ReplacedSessionCleanup::LogicalFallback
                    } else {
                        return Err(error);
                    }
                }
            }
            Err(_) => {
                let message =
                    format!("agent CLI session/close timed out for replaced session {session_id}");
                tracing::error!(
                    target: "master",
                    step = "helper→agent",
                    op = "close_replaced_session",
                    helper_id = ?helper_id,
                    old_session_id = %session_id,
                    outcome = "timeout",
                    timeout_ms = remaining.as_millis() as u64,
                    "timed out physically closing replaced ACP session"
                );
                if retire_on_close_failure {
                    ReplacedSessionCleanup::LogicalFallback
                } else {
                    return Err(
                        acp::Error::new(-32603, message.clone()).data(serde_json::json!({
                            "message": message
                        })),
                    );
                }
            }
        }
    };

    {
        let mut routes = state.session_to_helper.lock().await;
        if !routes.get(session_id).is_some_and(|route| {
            route.helper_id == helper_id && route.agent_instance_id == agent.instance_id
        }) {
            unreachable!("session route cannot change while its lifecycle gate is held");
        }
        routes.remove(session_id);
    }

    state.pending_usage.lock().await.remove(session_id);
    state
        .session_mcp_capabilities
        .remove_session(session_id)
        .await;
    state.registry.remove(session_id).await;
    broadcast_ext_to_helpers(
        state,
        crate::session_registry::build_session_removed_notification(session_id),
    )
    .await;
    broadcast_ext_to_helpers(
        state,
        crate::session_registry::build_sessions_changed_notification(),
    )
    .await;
    Ok(cleanup)
}

fn retirement_deadline_error(session_id: &acp::schema::v1::SessionId, phase: &str) -> acp::Error {
    let message = format!("retirement deadline expired during {phase} for session {session_id}");
    acp::Error::new(-32603, message.clone()).data(serde_json::json!({
        "message": message
    }))
}

async fn retire_unbound_session_state(
    state: &MasterStateInner,
    session_id: &acp::schema::v1::SessionId,
) {
    let gate = session_lifecycle_gate(state, session_id).await;
    let _guard = gate.lock().await;
    if state
        .session_to_helper
        .lock()
        .await
        .contains_key(session_id)
    {
        return;
    }
    retire_unbound_session_state_gate_held(state, session_id).await;
}

async fn retire_unbound_session_state_gate_held(
    state: &MasterStateInner,
    session_id: &acp::schema::v1::SessionId,
) {
    state.pending_usage.lock().await.remove(session_id);
    state
        .session_mcp_capabilities
        .remove_session(session_id)
        .await;
    state.registry.remove(session_id).await;
    broadcast_ext_to_helpers(
        state,
        crate::session_registry::build_session_removed_notification(session_id),
    )
    .await;
    broadcast_ext_to_helpers(
        state,
        crate::session_registry::build_sessions_changed_notification(),
    )
    .await;
}

async fn force_retire_owned_session_state(
    state: &MasterStateInner,
    helper_id: HelperId,
    session_id: &acp::schema::v1::SessionId,
) -> ReplacedSessionCleanup {
    let gate = session_lifecycle_gate(state, session_id).await;
    let _guard = gate.lock().await;
    let removed = {
        let mut routes = state.session_to_helper.lock().await;
        if routes
            .get(session_id)
            .is_some_and(|route| route.helper_id == helper_id)
        {
            routes.remove(session_id);
            true
        } else {
            false
        }
    };
    if !removed {
        return ReplacedSessionCleanup::NotOwned;
    }
    // Keep the SessionId gate across ownership validation, route removal,
    // registry/MCP cleanup, and broadcasts. A rebound route cannot appear
    // between the absence check and destructive cleanup.
    retire_unbound_session_state_gate_held(state, session_id).await;
    ReplacedSessionCleanup::LogicalFallback
}

fn agent_supports_session_close(agent: &AgentCli) -> bool {
    agent
        .cached_init_resp
        .agent_capabilities
        .session_capabilities
        .close
        .is_some()
}

async fn handle_close_tab_session(
    state: &Arc<MasterStateInner>,
    params: &crate::session_registry::CloseTabSessionParams,
    reset_only: bool,
) -> acp::Result<acp::schema::v1::ExtResponse> {
    let deadline = tokio::time::Instant::now() + SESSION_CLOSE_TIMEOUT;
    retire_tab_session(state, params, reset_only, false, deadline).await?;
    let raw = serde_json::value::RawValue::from_string("{}".to_string())
        .expect("empty object is valid JSON");
    Ok(acp::schema::v1::ExtResponse::new(raw.into()))
}

async fn retire_tab_session(
    state: &Arc<MasterStateInner>,
    params: &crate::session_registry::CloseTabSessionParams,
    reset_only: bool,
    destructive: bool,
    deadline: tokio::time::Instant,
) -> acp::Result<ReplacedSessionCleanup> {
    let (tab_id, target, newly_marked_close, deferred_pending) = {
        let _ownership_guard = state.tab_ownership_gate.lock().await;
        let rekeys = state.tab_retirement_rekeys.lock().await;
        let tab_id = resolve_tab_retirement_id(&rekeys, &params.tab_id);
        drop(rekeys);
        let mut target = {
            let meta = state.helper_meta.lock().await;
            meta.iter().find_map(|(helper_id, recovery)| {
                (recovery.owner_tab_id.as_deref() == Some(tab_id.as_str()))
                    .then(|| (*helper_id, recovery.last_session_id.clone()))
            })
        };
        if target.is_none() {
            target = state.pending_session_helpers.lock().await.iter().find_map(
                |(helper_id, owner_tab_id)| {
                    (owner_tab_id.as_deref() == Some(tab_id.as_str())).then_some((*helper_id, None))
                },
            );
        }
        let newly_marked_close = if let Some((owner_helper_id, _)) = &target {
            state
                .closing_session_helpers
                .lock()
                .await
                .insert(*owner_helper_id)
        } else {
            false
        };
        let deferred_pending = if let Some((owner_helper_id, _)) = &target {
            state
                .pending_session_helpers
                .lock()
                .await
                .contains_key(owner_helper_id)
        } else {
            false
        };
        (tab_id, target, newly_marked_close, deferred_pending)
    };
    let matched_helper_id = target.as_ref().map(|(helper_id, _)| *helper_id);

    let live_target = match target {
        Some((owner_helper_id, Some(session_id))) => {
            let agent_instance_id = {
                let routes = state.session_to_helper.lock().await;
                routes.get(&session_id).and_then(|route| {
                    (route.helper_id == owner_helper_id).then_some(route.agent_instance_id)
                })
            };
            agent_instance_id
                .map(|agent_instance_id| (owner_helper_id, session_id, agent_instance_id))
        }
        Some((owner_helper_id, None)) => {
            let route = {
                let routes = state.session_to_helper.lock().await;
                routes.iter().find_map(|(session_id, route)| {
                    (route.helper_id == owner_helper_id)
                        .then_some((session_id.clone(), route.agent_instance_id))
                })
            };
            route.map(|(session_id, agent_instance_id)| {
                (owner_helper_id, session_id, agent_instance_id)
            })
        }
        None => None,
    };

    let Some((owner_helper_id, session_id, agent_instance_id)) = live_target else {
        let orphan = { state.orphaned_tabs.lock().await.get(&tab_id).cloned() };
        if let Some((agent_key, orphan_helper_id, orphan_session_id)) = orphan {
            let gate = session_lifecycle_gate(state, &orphan_session_id).await;
            let _guard = match tokio::time::timeout_at(deadline, gate.lock()).await {
                Ok(guard) => guard,
                Err(_) if destructive => {
                    tracing::error!(
                        target: "master_retirement",
                        tab_id,
                        helper_id = ?orphan_helper_id,
                        session_id = %orphan_session_id,
                        "retirement deadline expired waiting for orphan lifecycle gate; deferring exact orphan cleanup"
                    );
                    schedule_deferred_tab_orphan_cleanup(
                        state,
                        agent_key,
                        orphan_helper_id,
                        orphan_session_id,
                    );
                    return Ok(ReplacedSessionCleanup::LogicalFallback);
                }
                Err(_) => {
                    return Err(retirement_deadline_error(
                        &orphan_session_id,
                        "lifecycle_gate",
                    ));
                }
            };
            let orphan_is_current =
                state
                    .orphaned_tabs
                    .lock()
                    .await
                    .get(&tab_id)
                    .is_some_and(|current| {
                        current
                            == &(
                                agent_key.clone(),
                                orphan_helper_id,
                                orphan_session_id.clone(),
                            )
                    });
            if !orphan_is_current {
                return Ok(ReplacedSessionCleanup::NotOwned);
            }
            if state
                .session_to_helper
                .lock()
                .await
                .contains_key(&orphan_session_id)
            {
                state.orphaned_tabs.lock().await.remove(&tab_id);
                return Ok(ReplacedSessionCleanup::NotOwned);
            }
            let agent = {
                let agents = state.agents.lock().await;
                agents.get(&agent_key).and_then(|cell| cell.get()).cloned()
            };

            let cleanup = if let Some(agent) = agent {
                let cancel = tokio::time::timeout_at(
                    deadline,
                    agent.conn.cancel(acp::schema::v1::CancelNotification::new(
                        orphan_session_id.clone(),
                    )),
                )
                .await;
                let cancel_timed_out = match cancel {
                    Ok(Ok(())) => false,
                    Ok(Err(error)) => {
                        tracing::warn!(
                            target: "master",
                            tab_id,
                            session_id = %orphan_session_id,
                            error = %error,
                                "failed to cancel orphaned turn before retirement"
                        );
                        false
                    }
                    Err(_) if destructive => {
                        tracing::error!(
                            target: "master_retirement",
                            tab_id,
                            session_id = %orphan_session_id,
                            "retirement deadline expired cancelling orphaned turn; retiring WTA state"
                        );
                        true
                    }
                    Err(_) => {
                        return Err(retirement_deadline_error(&orphan_session_id, "cancel"));
                    }
                };
                if cancel_timed_out {
                    ReplacedSessionCleanup::LogicalFallback
                } else if agent_supports_session_close(&agent) {
                    match tokio::time::timeout_at(
                        deadline,
                        agent
                            .conn
                            .close_session(acp::schema::v1::CloseSessionRequest::new(
                                orphan_session_id.clone(),
                            )),
                    )
                    .await
                    {
                        Ok(Ok(_)) => ReplacedSessionCleanup::PhysicallyClosed,
                        Ok(Err(error)) if error.code == acp::ErrorCode::MethodNotFound => {
                            tracing::warn!(
                                target: "master",
                                tab_id,
                                session_id = %orphan_session_id,
                                error = %error,
                                "agent advertised session/close but rejected it; retiring orphaned WTA state"
                            );
                            ReplacedSessionCleanup::LogicalFallback
                        }
                        Ok(Err(error)) if destructive => {
                            tracing::error!(
                                target: "master",
                                tab_id,
                                session_id = %orphan_session_id,
                                error = %error,
                                "failed to physically close orphan; retiring WTA state"
                            );
                            ReplacedSessionCleanup::LogicalFallback
                        }
                        Ok(Err(error)) => return Err(error),
                        Err(_) if destructive => {
                            tracing::error!(
                                target: "master",
                                tab_id,
                                session_id = %orphan_session_id,
                                "session/close timed out for orphan; retiring WTA state"
                            );
                            ReplacedSessionCleanup::LogicalFallback
                        }
                        Err(_) => {
                            return Err(acp::Error::internal_error().data(serde_json::json!({
                                "message": format!(
                                    "session/close timed out for orphaned tab {}",
                                    tab_id
                                )
                            })));
                        }
                    }
                } else {
                    ReplacedSessionCleanup::LogicalFallback
                }
            } else if destructive {
                tracing::warn!(
                    target: "master",
                    tab_id,
                    session_id = %orphan_session_id,
                    "orphan agent is unavailable; retiring WTA state"
                );
                ReplacedSessionCleanup::LogicalFallback
            } else {
                return Err(acp::Error::internal_error().data(serde_json::json!({
                    "message": format!(
                        "agent for orphaned tab {} is no longer available",
                        tab_id
                    )
                })));
            };

            {
                let mut orphaned_sessions = state.orphaned_sessions.lock().await;
                if let Some(sessions) = orphaned_sessions.get_mut(&agent_key) {
                    sessions.remove(&orphan_session_id);
                    if sessions.is_empty() {
                        orphaned_sessions.remove(&agent_key);
                    }
                }
            }
            state.orphaned_tabs.lock().await.remove(&tab_id);
            state.pending_usage.lock().await.remove(&orphan_session_id);
            state
                .session_mcp_capabilities
                .remove_session(&orphan_session_id)
                .await;
            state.registry.remove(&orphan_session_id).await;
            state.helper_meta.lock().await.remove(&orphan_helper_id);
            state
                .pending_session_helpers
                .lock()
                .await
                .remove(&orphan_helper_id);
            // Disconnect consumes the closing tombstone after its orphan
            // publication phase has observed this physical retirement.
            broadcast_ext_to_helpers(
                state,
                crate::session_registry::build_session_removed_notification(&orphan_session_id),
            )
            .await;
            broadcast_ext_to_helpers(
                state,
                crate::session_registry::build_sessions_changed_notification(),
            )
            .await;
            tracing::info!(
                target: "master",
                tab_id,
                helper_id = ?orphan_helper_id,
                session_id = %orphan_session_id,
                cleanup = ?cleanup,
                "closed ACP session resolved from destroyed tab"
            );
            return Ok(cleanup);
        }

        if !destructive && !deferred_pending && newly_marked_close {
            if let Some(helper_id) = matched_helper_id {
                state.helper_meta.lock().await.remove(&helper_id);
                state
                    .closing_session_helpers
                    .lock()
                    .await
                    .remove(&helper_id);
            }
        }
        tracing::debug!(
            target: "master",
            tab_id,
            deferred = deferred_pending,
            "close-by-tab found no live session; treating duplicate, late, or in-flight request as success"
        );
        return Ok(ReplacedSessionCleanup::NotOwned);
    };

    let agent = {
        let agents = state.agents.lock().await;
        agents
            .values()
            .filter_map(|cell| cell.get())
            .find(|agent| agent.instance_id == agent_instance_id)
            .cloned()
    };

    let cleanup = if let Some(agent) = agent {
        close_and_retire_owned_session(
            state,
            owner_helper_id,
            &agent,
            &session_id,
            deadline,
            destructive,
        )
        .await?
    } else if destructive {
        tracing::warn!(
            target: "master",
            tab_id,
            helper_id = ?owner_helper_id,
            session_id = %session_id,
            agent_instance_id = %agent_instance_id,
            "owning agent is unavailable; retiring WTA state"
        );
        force_retire_owned_session_state(state, owner_helper_id, &session_id).await
    } else {
        return Err(acp::Error::internal_error().data(serde_json::json!({
            "message": format!(
                "agent instance {} for tab {} is no longer available",
                agent_instance_id, tab_id
            )
        })));
    };
    if cleanup != ReplacedSessionCleanup::NotOwned {
        // This is intentional tab destruction, not a helper crash. Remove the
        // recovery record only when the transaction consumes the closing
        // marker or the helper disconnects. Keeping the marker here closes the
        // race where a committing transaction has just removed its pending
        // flag but has not yet checked whether tab close retired its session.
        state.orphaned_tabs.lock().await.remove(&tab_id);
        if reset_only && !deferred_pending {
            state
                .closing_session_helpers
                .lock()
                .await
                .remove(&owner_helper_id);
            if let Some(meta) = state.helper_meta.lock().await.get_mut(&owner_helper_id) {
                meta.last_session_id = None;
            }
        }
        if destructive {
            state.orphaned_tabs.lock().await.remove(&tab_id);
            {
                let mut orphaned_sessions = state.orphaned_sessions.lock().await;
                for sessions in orphaned_sessions.values_mut() {
                    sessions.remove(&session_id);
                }
            }
            if !deferred_pending {
                state.helper_meta.lock().await.remove(&owner_helper_id);
                state
                    .pending_session_helpers
                    .lock()
                    .await
                    .remove(&owner_helper_id);
                state.session_transaction_changed.notify_waiters();
            }
        }
    }
    tracing::info!(
        target: "master",
        tab_id,
        helper_id = ?owner_helper_id,
        session_id = %session_id,
        cleanup = ?cleanup,
        "closed ACP session resolved from destroyed tab"
    );

    Ok(cleanup)
}

/// Canonical key for the agent-CLI pool: authoritative agent identity,
/// execution source, and full command line. Two tabs with the same identity,
/// source, and command share one CLI; custom and built-in agents never share
/// merely because their commands happen to match.
fn agent_cmd_key(
    command: &str,
    agent_id: Option<&str>,
    source: &crate::agent_source::AgentSource,
) -> AgentCmdKey {
    agent_cmd_key_with_provider(command, agent_id, source, &ProviderBinding::Native)
}

fn agent_cmd_key_with_provider(
    command: &str,
    agent_id: Option<&str>,
    source: &crate::agent_source::AgentSource,
    provider_binding: &ProviderBinding,
) -> AgentCmdKey {
    let lifecycle =
        if requested_model_is_explicit(command, agent_id) || provider_binding.is_model_scoped() {
            "model:"
        } else {
            "warm:"
        };
    format!(
        "{lifecycle}{:?}",
        (source, agent_id, command, provider_binding.pool_key())
    )
}

/// One spawned agent CLI subprocess and everything a helper needs to
/// talk to it. Shared (`Arc`) across every helper currently bound to
/// this agent.
struct AgentCli {
    instance_id: AgentInstanceId,
    resolved_agent_id: String,
    /// Master is the ACP *client* of this CLI. Every helper request for
    /// a session owned by this agent forwards onto this connection.
    conn: conn::ClientLink,
    /// This CLI's `initialize` response, replayed verbatim to every
    /// helper that binds to it (re-forwarding `initialize` to the CLI
    /// returns empty `agent_info` on most backends, which blanks the
    /// XAML agent bar). Per-agent so each tab's bar shows ITS agent.
    cached_init_resp: acp::schema::v1::InitializeResponse,
    /// The CLI provider, resolved from this agent's id/command line.
    /// Stamped on every SessionInfo this agent's sessions upsert so the
    /// F2 view labels each row with its real CLI (Gemini vs Claude),
    /// not one process-wide value.
    cli_source: Option<crate::agent_sessions::CliSource>,
    /// Short-TTL cache of THIS CLI's raw `session/list` response.
    /// `Some(Some(sessions))` = the agent listed (possibly empty);
    /// `Some(None)` = the last fetch failed / timed out / is unsupported —
    /// negative-cached so a burst of hook/watcher events and the 5 s poll share
    /// one round-trip and don't hammer a hung agent. Both the host-history
    /// reconcile and the synthetic-title refresh derive from this one fetch.
    ///
    /// Per-agent (not per-master) because an agent enumerates only its OWN
    /// sessions: a shared cache would serve one CLI's rows to another and, once
    /// the user switches agents in Settings, permanently answer for the wrong
    /// one. Dies with the `AgentCli` when the pool reaps it.
    host_list_cache: Mutex<
        Option<(
            std::time::Instant,
            Option<std::sync::Arc<[acp::schema::v1::SessionInfo]>>,
        )>,
    >,
    /// Session ids THIS agent's `session/list` has returned at least once.
    ///
    /// Reconcile may only drop rows from this set. `cli_source` does not
    /// identify a session universe: host Copilot, Copilot in WSL Debian, and
    /// Copilot in WSL Ubuntu all stamp `Some(Copilot)` yet list disjoint
    /// sessions. Keying the prune on "ids I previously listed and no longer
    /// list" is what stops two such agents from deleting each other's rows on
    /// every 5 s poll — which otherwise thrashes forever, since each one
    /// re-adds what the other just dropped.
    listed_ever: Mutex<HashSet<String>>,
    source: crate::agent_source::AgentSource,
    /// The pool key (agent command line) this CLI was spawned under —
    /// the same `AgentCmdKey` used in [`MasterStateInner::agents`]. Lets
    /// helper-disconnect cleanup and `load_session` scope orphan tracking
    /// to THIS agent (and this exact instance, via `Arc::ptr_eq`), so a
    /// crashed-and-respawned CLI under the same command line never inherits
    /// another instance's stale orphan sessions.
    cmd_key: AgentCmdKey,
    /// Native Host cloud catalog retained independently from the BYOK session's
    /// own ACP selector. A clean discovery probe may still be pending after the
    /// real CLI has initialized; helpers receive the eventual result through a
    /// private WTA notification.
    cloud_catalog: Mutex<NativeCloudCatalogState>,
    /// Helpers currently bound to this exact CLI instance. Used to target the
    /// eventual clean-probe result without leaking one agent/source catalog to
    /// unrelated helpers in the same master.
    bound_helpers: Mutex<HashSet<HelperId>>,
}

fn update_model_switch_channel_from_load(
    session_id: &acp::schema::v1::SessionId,
    response: &acp::schema::v1::LoadSessionResponse,
) -> (Vec<crate::app::AcpModelInfo>, Option<String>) {
    crate::protocol::acp::model_select::models_from_load_session(session_id.0.as_ref(), response)
}

#[derive(Clone, Debug)]
struct NativeCloudCatalog {
    models: Vec<crate::app::AcpModelInfo>,
    source: CloudCatalogSource,
}

#[derive(Debug, Default)]
enum NativeCloudCatalogState {
    #[default]
    Unavailable,
    Pending,
    Ready(NativeCloudCatalog),
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloudCatalogSource {
    Helper,
    CleanProbe,
}

impl CloudCatalogSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Helper => "helper",
            Self::CleanProbe => "clean_probe",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloudCatalogPlan {
    None,
    Supplied,
    CleanProbe,
}

fn cloud_catalog_plan(
    source: &crate::agent_source::AgentSource,
    byok_mode: crate::agent_registry::ByokMode,
    shared_provider_complete: bool,
    supplied_models_empty: bool,
) -> CloudCatalogPlan {
    if !matches!(source, crate::agent_source::AgentSource::Host) {
        return CloudCatalogPlan::None;
    }
    if !supplied_models_empty {
        return CloudCatalogPlan::Supplied;
    }
    if shared_provider_complete && byok_mode != crate::agent_registry::ByokMode::Unsupported {
        return CloudCatalogPlan::CleanProbe;
    }
    CloudCatalogPlan::None
}

fn prepare_native_cloud_catalog(
    resolved_agent_id: &str,
    source: &crate::agent_source::AgentSource,
    provider_binding: &ProviderBinding,
    supplied_models: Vec<crate::app::AcpModelInfo>,
) -> (NativeCloudCatalogState, bool) {
    let profile = crate::agent_registry::lookup_profile_by_id(resolved_agent_id);
    match cloud_catalog_plan(
        source,
        profile.byok_mode,
        provider_binding.has_active_custom_provider(),
        supplied_models.is_empty(),
    ) {
        CloudCatalogPlan::None => (NativeCloudCatalogState::Unavailable, false),
        CloudCatalogPlan::Supplied => (
            NativeCloudCatalogState::Ready(NativeCloudCatalog {
                models: supplied_models,
                source: CloudCatalogSource::Helper,
            }),
            false,
        ),
        CloudCatalogPlan::CleanProbe => (NativeCloudCatalogState::Pending, true),
    }
}

async fn add_ready_cloud_catalog(
    agent: &AgentCli,
    wta_meta: &mut crate::session_registry::WtaMeta,
) -> Result<(), serde_json::Error> {
    let catalog = agent.cloud_catalog.lock().await;
    let NativeCloudCatalogState::Ready(catalog) = &*catalog else {
        return Ok(());
    };
    wta_meta.cloud_models = Some(serde_json::to_string(&catalog.models)?);
    wta_meta.cloud_models_source = Some(catalog.source.as_str().to_string());
    Ok(())
}

async fn initialize_response_for_agent(
    agent: &AgentCli,
    session_mcp_available: bool,
) -> Result<acp::schema::v1::InitializeResponse, serde_json::Error> {
    let mut response = agent.cached_init_resp.clone();
    let mut wta_meta = crate::session_registry::WtaMeta {
        resolved_agent_id: Some(agent.resolved_agent_id.clone()),
        proposal_mcp: session_mcp_available.then(|| "http-v1".to_string()),
        ..Default::default()
    };
    add_ready_cloud_catalog(agent, &mut wta_meta).await?;
    crate::session_registry::inject_wta_meta(&mut response.meta, &wta_meta);
    Ok(response)
}

async fn notify_bound_helpers(
    state: &MasterStateInner,
    agent: &AgentCli,
    notification: acp::schema::v1::ExtNotification,
) {
    let helper_ids: Vec<_> = agent.bound_helpers.lock().await.iter().copied().collect();
    let mut subscribers = state.helper_ext_subscribers.lock().await;
    for helper_id in helper_ids {
        let Some(tx) = subscribers.get(&helper_id) else {
            continue;
        };
        if let Err(error) = tx.send(notification.clone()) {
            tracing::warn!(
                target: "master",
                helper_id = ?helper_id,
                method = %notification.method,
                %error,
                "cloud catalog notification channel closed; pruning subscriber"
            );
            subscribers.remove(&helper_id);
        }
    }
}

fn start_clean_cloud_catalog_probe<F>(
    state: Arc<MasterStateInner>,
    agent: Arc<AgentCli>,
    resolved_agent_id: String,
    probe: F,
) where
    F: Future<Output = Result<crate::protocol::acp::probe::ProbeResult>> + 'static,
{
    tokio::task::spawn_local(async move {
        match probe.await {
            Ok(result) => {
                let catalog = NativeCloudCatalog {
                    models: result.available_models,
                    source: CloudCatalogSource::CleanProbe,
                };
                tracing::info!(
                    target: "master",
                    agent_id = %resolved_agent_id,
                    model_count = catalog.models.len(),
                    "clean native cloud model probe completed for BYOK agent"
                );
                {
                    let mut state = agent.cloud_catalog.lock().await;
                    *state = NativeCloudCatalogState::Ready(catalog.clone());
                }
                if !catalog.models.is_empty() {
                    notify_bound_helpers(
                        &state,
                        &agent,
                        crate::protocol::acp::model_select::build_wta_cloud_catalog_notification(
                            &catalog.models,
                            catalog.source.as_str(),
                        ),
                    )
                    .await;
                }
            }
            Err(error) => {
                {
                    let mut state = agent.cloud_catalog.lock().await;
                    *state = NativeCloudCatalogState::Failed;
                }
                tracing::warn!(
                    target: "master",
                    agent_id = %resolved_agent_id,
                    %error,
                    "clean native cloud model probe failed; Host helper will continue with host cache and agent-advertised models"
                );
            }
        }
    });
}

/// Per-helper ownership metadata stashed in
/// [`MasterStateInner::helper_meta`]. See the field doc for lifecycle.
#[derive(Debug, Clone, Default)]
pub(crate) struct HelperRecoveryMeta {
    /// The WT tab StableId that owns this helper's agent pane, from
    /// `_meta.wta.owner_tab_id`. `None` for non-agent-pane helpers.
    pub(crate) owner_tab_id: Option<String>,
    /// The most recently created or loaded session for this helper.
    pub(crate) last_session_id: Option<acp::schema::v1::SessionId>,
}

/// Master's `acp::Client` impl: handles inbound from the agent CLI.
///
/// `session_notification` fans out to the owning helper via its
/// notification channel. The request-shaped Client methods
/// (`request_permission`, `create_terminal`, `terminal_*`,
/// `read_text_file`, `write_text_file`) look up the owning helper by
/// `args.session_id` in `session_to_helper` and forward the call on
/// that helper's `AgentSideConnection` — the helper's `WtaClient`
/// then runs the same handler it ran pre-helper-split (TUI permission
/// UI, `ShellManager`, etc.). The agent CLI sees the helper's
/// response as if master had answered directly.
#[derive(Clone)]
struct MasterClient {
    state: Arc<MasterStateInner>,
}

impl MasterClient {
    /// Look up the helper owning `sid` and clone the forwarder + id.
    ///
    /// Returns `Err(internal_error)` if either (a) no helper is bound
    /// to this session — typically means the agent CLI emitted a
    /// stale request after the owning helper disconnected — or
    /// (b) the routing entry has no forwarder (production code never
    /// reaches this branch; see `HelperRoute::forwarder`).
    async fn route_for(
        &self,
        sid: &acp::schema::v1::SessionId,
        op: &'static str,
    ) -> acp::Result<(HelperId, conn::AgentLink)> {
        let entry = {
            let map = self.state.session_to_helper.lock().await;
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
}

/// True when an agent CLI's `session/load` error means the session id is
/// already live inside the CLI (not missing). Copilot reports this as a
/// "… is already loaded" message under `-32602`; we match that stable
/// substring (in message or data) rather than the code. `load_session`
/// uses it to re-bind an orphan session instead of failing the resume.
fn is_already_loaded_error(err: &acp::Error) -> bool {
    let msg = err.message.to_ascii_lowercase();
    if msg.contains("already loaded") {
        return true;
    }
    err.data
        .as_ref()
        .and_then(|d| d.as_str())
        .map(|s| s.to_ascii_lowercase().contains("already loaded"))
        .unwrap_or(false)
}

impl MasterClient {
    async fn request_permission(
        &self,
        args: acp::schema::v1::RequestPermissionRequest,
    ) -> acp::Result<acp::schema::v1::RequestPermissionResponse> {
        let sid = args.session_id.clone();
        // The shared agent CLI can ask permission for an orphan session
        // (its owning tab closed mid-turn). With no helper to ask, answer
        // `Cancelled` — a well-formed "user dismissed it" the agent handles
        // by aborting the tool call. Never return an error to the shared CLI
        // here: a hard failure can make it drop the whole connection and take
        // every other tab down with it.
        let (helper_id, forwarder) = match self.route_for(&sid, "request_permission").await {
            Ok(route) => route,
            Err(_) => {
                tracing::info!(
                    target: "master",
                    op = "request_permission",
                    session_id = ?sid,
                    "orphan session permission request answered with Cancelled"
                );
                return Ok(acp::schema::v1::RequestPermissionResponse::new(
                    acp::schema::v1::RequestPermissionOutcome::Cancelled,
                ));
            }
        };
        tracing::info!(
            target: "master",
            step = "agent→helper",
            op = "request_permission",
            helper_id = ?helper_id,
            session_id = ?sid,
            "forwarding permission request to helper"
        );
        let resp = forwarder.request_permission(args).await;
        if let Err(ref e) = resp {
            tracing::warn!(
                target: "master",
                op = "request_permission",
                helper_id = ?helper_id,
                session_id = ?sid,
                error = %e,
                "helper returned error for permission request"
            );
        }
        resp
    }

    async fn session_notification(
        &self,
        args: acp::schema::v1::SessionNotification,
    ) -> acp::Result<()> {
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
            let map = self.state.session_to_helper.lock().await;
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
                if kind == "usage_update" {
                    let mut args = args;
                    let mut pending = self.state.pending_usage.lock().await;
                    if let Some((pending_owner, pending_notification)) = pending.get(&sid) {
                        if *pending_owner == snap_helper_id {
                            if let (
                                acp::schema::v1::SessionUpdate::UsageUpdate(previous),
                                acp::schema::v1::SessionUpdate::UsageUpdate(incoming),
                            ) = (&pending_notification.update, &mut args.update)
                            {
                                if incoming.cost.is_none() {
                                    incoming.cost = previous.cost.clone();
                                }
                            }
                        }
                    }
                    pending.insert(sid.clone(), (snap_helper_id, args));
                    drop(pending);
                    self.state
                        .usage_generation
                        .send_modify(|generation| *generation = generation.wrapping_add(1));
                    return Ok(());
                }
                // `try_send` rather than `send().await`: a slow helper
                // pipe must not back-pressure this trait method, which
                // is driven by the agent CLI's I/O loop and is shared
                // across every helper. Blocking here would freeze
                // notification delivery for everyone.
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
                        //
                        // CRITICAL: only remove if the entry STILL
                        // is the exact route we snapshotted. A
                        // freshly-issued `load_session` can have
                        // rebound the same SessionId to a new channel,
                        // even for the same helper, between our snapshot and now —
                        // clobbering that new entry would silently
                        // break notification delivery for the new route.
                        let gate = session_lifecycle_gate(&self.state, &sid).await;
                        let _guard = gate.lock().await;
                        let mut map = self.state.session_to_helper.lock().await;
                        match map.get(&sid) {
                            Some(current)
                                if current.helper_id == snap_helper_id
                                    && current.notif_tx.same_channel(&tx) =>
                            {
                                map.remove(&sid);
                                tracing::warn!(
                                    target: "master",
                                    session_id = ?sid,
                                    kind = %kind,
                                    helper_id = ?snap_helper_id,
                                    "helper notification channel closed — helper likely disconnected; dropping update and routing entry"
                                );
                            }
                            Some(current) => {
                                tracing::info!(
                                    target: "master",
                                    session_id = ?sid,
                                    kind = %kind,
                                    stale_helper_id = ?snap_helper_id,
                                    current_helper_id = ?current.helper_id,
                                    "helper notification channel closed but SessionId has been rebound — dropping update, leaving new route intact"
                                );
                            }
                            None => {
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
        Ok(())
    }

    async fn write_text_file(
        &self,
        args: acp::schema::v1::WriteTextFileRequest,
    ) -> acp::Result<acp::schema::v1::WriteTextFileResponse> {
        let sid = args.session_id.clone();
        let (helper_id, forwarder) = self.route_for(&sid, "write_text_file").await?;
        tracing::info!(
            target: "master",
            step = "agent→helper",
            op = "write_text_file",
            helper_id = ?helper_id,
            session_id = ?sid,
            "forwarding fs/write_text_file to helper"
        );
        forwarder.write_text_file(args).await
    }

    async fn read_text_file(
        &self,
        args: acp::schema::v1::ReadTextFileRequest,
    ) -> acp::Result<acp::schema::v1::ReadTextFileResponse> {
        let sid = args.session_id.clone();
        let (helper_id, forwarder) = self.route_for(&sid, "read_text_file").await?;
        tracing::info!(
            target: "master",
            step = "agent→helper",
            op = "read_text_file",
            helper_id = ?helper_id,
            session_id = ?sid,
            "forwarding fs/read_text_file to helper"
        );
        forwarder.read_text_file(args).await
    }

    async fn create_terminal(
        &self,
        args: acp::schema::v1::CreateTerminalRequest,
    ) -> acp::Result<acp::schema::v1::CreateTerminalResponse> {
        let sid = args.session_id.clone();
        let (helper_id, forwarder) = self.route_for(&sid, "create_terminal").await?;
        tracing::info!(
            target: "master",
            step = "agent→helper",
            op = "create_terminal",
            helper_id = ?helper_id,
            session_id = ?sid,
            args_len = args.args.len(),
            "forwarding terminal/create to helper"
        );
        forwarder.create_terminal(args).await
    }

    async fn terminal_output(
        &self,
        args: acp::schema::v1::TerminalOutputRequest,
    ) -> acp::Result<acp::schema::v1::TerminalOutputResponse> {
        let sid = args.session_id.clone();
        let (helper_id, forwarder) = self.route_for(&sid, "terminal_output").await?;
        tracing::debug!(
            target: "master",
            step = "agent→helper",
            op = "terminal_output",
            helper_id = ?helper_id,
            session_id = ?sid,
            terminal_id = ?args.terminal_id,
            "forwarding terminal/output to helper"
        );
        forwarder.terminal_output(args).await
    }

    async fn release_terminal(
        &self,
        args: acp::schema::v1::ReleaseTerminalRequest,
    ) -> acp::Result<acp::schema::v1::ReleaseTerminalResponse> {
        let sid = args.session_id.clone();
        let (helper_id, forwarder) = self.route_for(&sid, "release_terminal").await?;
        tracing::info!(
            target: "master",
            step = "agent→helper",
            op = "release_terminal",
            helper_id = ?helper_id,
            session_id = ?sid,
            terminal_id = ?args.terminal_id,
            "forwarding terminal/release to helper"
        );
        forwarder.release_terminal(args).await
    }

    async fn wait_for_terminal_exit(
        &self,
        args: acp::schema::v1::WaitForTerminalExitRequest,
    ) -> acp::Result<acp::schema::v1::WaitForTerminalExitResponse> {
        let sid = args.session_id.clone();
        let (helper_id, forwarder) = self.route_for(&sid, "wait_for_terminal_exit").await?;
        tracing::info!(
            target: "master",
            step = "agent→helper",
            op = "wait_for_terminal_exit",
            helper_id = ?helper_id,
            session_id = ?sid,
            terminal_id = ?args.terminal_id,
            "forwarding terminal/wait_for_exit to helper"
        );
        forwarder.wait_for_terminal_exit(args).await
    }

    async fn kill_terminal(
        &self,
        args: acp::schema::v1::KillTerminalRequest,
    ) -> acp::Result<acp::schema::v1::KillTerminalResponse> {
        let sid = args.session_id.clone();
        let (helper_id, forwarder) = self.route_for(&sid, "kill_terminal").await?;
        tracing::info!(
            target: "master",
            step = "agent→helper",
            op = "kill_terminal",
            helper_id = ?helper_id,
            session_id = ?sid,
            terminal_id = ?args.terminal_id,
            "forwarding terminal/kill to helper"
        );
        forwarder.kill_terminal(args).await
    }
}

/// Short, log-friendly tag for a `SessionNotification`'s update
/// variant. Just enough to grep — "this turn started chunking",
/// "this turn called a tool", "this turn ended".
fn notification_kind(notif: &acp::schema::v1::SessionNotification) -> &'static str {
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
        UsageUpdate(_) => "usage_update",
        _ => "other",
    }
}

/// `acp::Agent` impl wired into one helper's `AgentSideConnection`.
/// Each helper gets its own `HelperHandler` instance.
#[derive(Clone)]
struct HelperHandler {
    helper_id: HelperId,
    /// The agent CLI this helper is bound to. Resolved lazily during
    /// `initialize` from the helper's declared `_meta.wta.agent_id`
    /// (+ `model`): the master reconstructs the command from that id and
    /// never executes a command string off the pipe (falling back to the
    /// master default when no / unknown id is declared). Reused by every
    /// later request on this connection. The async `OnceCell` serializes
    /// concurrent `initialize` requests so only the published binding can
    /// acquire an agent and register this helper.
    agent: AgentCell,
    state: Arc<MasterStateInner>,
    /// Serializes complete session replacement transactions for this helper.
    /// Shared by every cloned request handler, while unrelated helpers retain
    /// their own independent gate. This is always acquired before state locks.
    replacement_gate: Arc<Mutex<()>>,
    /// Notification fan-in for this helper. `new_session` /
    /// `load_session` writes `(SessionId → this sender)` into
    /// `state.session_to_helper` so future agent-CLI notifications
    /// land here. The helper's serve loop drains the matching
    /// receiver and writes notifications back over the
    /// `AgentSideConnection`.
    notif_tx: mpsc::Sender<acp::schema::v1::SessionNotification>,
    /// The same helper's outbound connection back to its pipe, held
    /// as a `Weak` to break a reference cycle.
    ///
    /// `HelperHandler` is moved INTO `AgentSideConnection::new`, so
    /// the conn owns the handler. If we then stored a strong `Arc`
    /// back to that same conn here, the conn would never drop after
    /// helper disconnect (its own internally-held handler keeps a
    /// strong ref to itself), leaking one conn + helper state per
    /// disconnect across the master's lifetime. `Weak` lets the
    /// conn die when all its external strong refs go away
    /// (`serve_helper`'s local + every `HelperRoute.forwarder`),
    /// after which `upgrade()` returns `None` and the handler can't
    /// fire any more outbound requests — which is the right behaviour
    /// since the conn is being torn down.
    ///
    /// Shared with `serve_helper` via `OnceLock`: the conn doesn't
    /// exist until `AgentSideConnection::new()` returns, but
    /// `serve_helper` populates this slot strictly before `handle_io`
    /// starts polling, so any inbound request observed by a handler
    /// sees a populated slot.
    agent_side_slot: Arc<OnceLock<conn::AgentLink>>,
}

impl HelperHandler {
    async fn publish_pending_owner(&self, owner_tab_id: Option<String>) -> acp::Result<()> {
        let _guard = self.state.tab_ownership_gate.lock().await;
        let helper_is_retired = self
            .state
            .closing_session_helpers
            .lock()
            .await
            .contains(&self.helper_id)
            || self
                .state
                .destructive_session_helpers
                .lock()
                .await
                .contains(&self.helper_id);
        let blocked_by_fence = if let Some(owner_tab_id) = owner_tab_id.as_deref() {
            let blocked_by_unresolved = self
                .state
                .unresolved_owner_retirements
                .lock()
                .await
                .remove(&self.helper_id)
                .is_some_and(|safety| safety.rejects(owner_tab_id));
            let mut fences = self.state.tab_retirement_fences.lock().await;
            fences.retain(|fence_tab_id, fence| {
                if fence_tab_id != owner_tab_id
                    && fence.phase == TabRetirementPhase::CompletedAwaitingDisconnect
                {
                    fence.outgoing_helpers.remove(&self.helper_id);
                }
                fence.phase == TabRetirementPhase::Fencing || !fence.outgoing_helpers.is_empty()
            });
            let blocked_by_tab = match fences.get_mut(owner_tab_id) {
                Some(fence)
                    if fence.phase == TabRetirementPhase::Fencing
                        || fence.outgoing_helpers.contains(&self.helper_id) =>
                {
                    fence.outgoing_helpers.insert(self.helper_id);
                    true
                }
                Some(_) => {
                    // Terminal only creates a replacement helper after it has
                    // received completion. This helper is outside the captured
                    // outgoing generation, so consuming the completed fence is
                    // safe and prevents it from blocking the replacement.
                    fences.remove(owner_tab_id);
                    false
                }
                None => false,
            };
            blocked_by_tab || blocked_by_unresolved
        } else {
            false
        };
        if helper_is_retired || blocked_by_fence {
            self.state
                .closing_session_helpers
                .lock()
                .await
                .insert(self.helper_id);
            self.state
                .destructive_session_helpers
                .lock()
                .await
                .insert(self.helper_id);
            return Err(acp::Error::invalid_params().data(serde_json::json!({
                "message": "the owning tab's outgoing helper generation has been retired"
            })));
        }
        self.state
            .pending_session_helpers
            .lock()
            .await
            .insert(self.helper_id, owner_tab_id.clone());
        if let Some(owner_tab_id) = owner_tab_id {
            self.state
                .helper_meta
                .lock()
                .await
                .entry(self.helper_id)
                .or_default()
                .owner_tab_id = Some(owner_tab_id);
        }
        Ok(())
    }

    async fn commit_pending_session(&self, session_id: &acp::schema::v1::SessionId) -> bool {
        let _guard = self.state.tab_ownership_gate.lock().await;
        if self
            .state
            .closing_session_helpers
            .lock()
            .await
            .contains(&self.helper_id)
            || self
                .state
                .destructive_session_helpers
                .lock()
                .await
                .contains(&self.helper_id)
        {
            return false;
        }
        let owner_tab_id = self
            .state
            .pending_session_helpers
            .lock()
            .await
            .get(&self.helper_id)
            .cloned()
            .flatten();
        if let Some(owner_tab_id) = owner_tab_id.as_deref() {
            let fences = self.state.tab_retirement_fences.lock().await;
            if fences.get(owner_tab_id).is_some_and(|fence| {
                fence.phase == TabRetirementPhase::Fencing
                    || fence.outgoing_helpers.contains(&self.helper_id)
            }) {
                return false;
            }
        }
        let mut meta = self.state.helper_meta.lock().await;
        let entry = meta.entry(self.helper_id).or_default();
        if let Some(owner_tab_id) = owner_tab_id {
            entry.owner_tab_id = Some(owner_tab_id);
        }
        entry.last_session_id = Some(session_id.clone());
        self.state
            .pending_session_helpers
            .lock()
            .await
            .remove(&self.helper_id);
        true
    }

    async fn finish_failed_pending_session(&self) {
        let pending_mcp = self
            .state
            .pending_session_mcp
            .lock()
            .await
            .remove(&self.helper_id);
        let _guard = self.state.tab_ownership_gate.lock().await;
        let destructive = self
            .state
            .destructive_session_helpers
            .lock()
            .await
            .contains(&self.helper_id);
        self.state
            .pending_session_helpers
            .lock()
            .await
            .remove(&self.helper_id);
        let closing = if destructive {
            self.state
                .closing_session_helpers
                .lock()
                .await
                .contains(&self.helper_id)
        } else {
            self.state
                .closing_session_helpers
                .lock()
                .await
                .remove(&self.helper_id)
        };
        if closing {
            self.state.helper_meta.lock().await.remove(&self.helper_id);
        }
        self.state.session_transaction_changed.notify_waiters();
        drop(_guard);
        if let Some(pending_mcp) = pending_mcp {
            self.state
                .session_mcp_capabilities
                .cancel(&pending_mcp)
                .await;
        }
    }

    async fn close_session_for_destroyed_tab(
        &self,
        agent: &AgentCli,
        session_id: &acp::schema::v1::SessionId,
    ) -> acp::Result<ReplacedSessionCleanup> {
        let destructive = self
            .state
            .destructive_session_helpers
            .lock()
            .await
            .contains(&self.helper_id);
        let result = close_and_retire_owned_session(
            &self.state,
            self.helper_id,
            agent,
            session_id,
            tokio::time::Instant::now() + SESSION_CLOSE_TIMEOUT,
            destructive,
        )
        .await;
        if destructive
            && self
                .state
                .active_retirement_helpers
                .lock()
                .await
                .contains(&self.helper_id)
        {
            let outcome = result
                .as_ref()
                .copied()
                .unwrap_or(ReplacedSessionCleanup::LogicalFallback);
            let mut outcomes = self.state.closing_session_results.lock().await;
            outcomes
                .entry(self.helper_id)
                .and_modify(|current| {
                    if outcome == ReplacedSessionCleanup::LogicalFallback
                        || *current == ReplacedSessionCleanup::NotOwned
                    {
                        *current = outcome;
                    }
                })
                .or_insert(outcome);
        }
        result
    }

    /// Snapshot the populated `AgentSideConnection` for this helper.
    /// Must only be called from request handlers driven by
    /// `handle_io` (which `serve_helper` polls strictly after the
    /// slot is set).
    ///
    /// Two failure modes, both returning `internal_error`:
    ///   * Slot not yet set — a real bug (shouldn't happen given the
    ///     ordering above).
    ///   * `Weak::upgrade` returns `None` — the conn has already been
    ///     dropped (helper disconnect path); we have no way to route
    ///     a fresh request anyway.
    fn forwarder_for_route(&self, op: &'static str) -> acp::Result<conn::AgentLink> {
        let link = self.agent_side_slot.get().ok_or_else(|| {
            tracing::error!(
                target: "master",
                op = op,
                helper_id = ?self.helper_id,
                "agent_side_slot empty inside helper request handler — bug; serve_helper must populate it before handle_io polls"
            );
            acp::Error::internal_error()
                .data(serde_json::json!("agent_side_slot not yet set"))
        })?;
        Ok(link.clone())
    }

    /// The agent CLI this helper bound to during `initialize`. Returns
    /// `internal_error` if called before `initialize` resolved the
    /// binding — a protocol violation by the helper, never expected in
    /// the normal handshake order.
    fn resolved_agent(&self, op: &'static str) -> acp::Result<Arc<AgentCli>> {
        self.agent.get().cloned().ok_or_else(|| {
            tracing::error!(
                target: "master",
                op = op,
                helper_id = ?self.helper_id,
                "helper request arrived before initialize bound an agent — protocol violation"
            );
            acp::Error::internal_error().data(serde_json::json!(
                "no agent bound; initialize must come first"
            ))
        })
    }

    /// Forward `session/new` to this helper's bound agent CLI with a
    /// timeout (moved to the master per #268) plus ACP telemetry. The
    /// timeout breaks an ACP cancellation-safety deadlock so a hung
    /// agent surfaces as an error instead of wedging the helper.
    async fn forward_new_session_to_agent(
        &self,
        args: acp::schema::v1::NewSessionRequest,
        timeout: std::time::Duration,
    ) -> acp::Result<acp::schema::v1::NewSessionResponse> {
        let timeout_secs = timeout.as_secs();
        let started = std::time::Instant::now();
        let agent = self.resolved_agent("new_session")?;
        let result = tokio::time::timeout(timeout, agent.conn.new_session(args)).await;
        let session_id = result
            .as_ref()
            .ok()
            .and_then(|inner| inner.as_ref().ok())
            .map(|resp| resp.session_id.to_string());
        let (failure_kind, acp_error_code) = match &result {
            Ok(Ok(_)) => ("", 0),
            Ok(Err(e)) => ("AcpError", e.code.into()),
            Err(_) => ("Timeout", 0),
        };
        crate::telemetry::log_acp_new_session_complete(
            session_id.as_deref(),
            started.elapsed().as_secs_f64() * 1000.0,
            matches!(result, Ok(Ok(_))),
            "MasterForward",
            failure_kind,
            acp_error_code,
        );
        result.map_err(|_| {
            let message = format!("agent CLI session/new timed out after {timeout_secs}s");
            tracing::error!(
                target: "master",
                step = "helper→agent",
                op = "new_session",
                helper_id = ?self.helper_id,
                timeout_secs,
                "agent CLI session/new timed out"
            );
            acp::Error::new(-32603, message.clone()).data(serde_json::json!({
                "message": message
            }))
        })?
    }

    fn load_session_timeout_error(
        &self,
        timeout: std::time::Duration,
        phase: &'static str,
    ) -> acp::Error {
        let timeout_secs = timeout.as_secs();
        let message = format!("agent CLI session/load timed out after {timeout_secs}s");
        tracing::error!(
            target: "master",
            step = "helper→agent",
            op = "load_session",
            helper_id = ?self.helper_id,
            phase,
            timeout_secs,
            "agent CLI session/load timed out"
        );
        acp::Error::new(-32603, message.clone()).data(serde_json::json!({
            "message": message
        }))
    }

    /// Forward `session/load` to this helper's bound agent CLI under the
    /// master's deadline. This deadline must expire before the helper's
    /// outer 60-second timeout so the replacement transaction can roll back
    /// routing and MCP state before the helper falls back to `session/new`.
    async fn forward_load_session_to_agent(
        &self,
        args: acp::schema::v1::LoadSessionRequest,
        rpc_timeout: std::time::Duration,
        total_timeout: std::time::Duration,
    ) -> acp::Result<acp::schema::v1::LoadSessionResponse> {
        let agent = self.resolved_agent("load_session")?;
        tokio::time::timeout(rpc_timeout, agent.conn.load_session(args))
            .await
            .map_err(|_| self.load_session_timeout_error(total_timeout, "agent_rpc"))?
    }

    async fn rollback_and_maybe_close_loaded_target(
        &self,
        agent: &AgentCli,
        session_id: &acp::schema::v1::SessionId,
        previous: Option<HelperRoute>,
        previous_used_same_agent: bool,
        timeout: std::time::Duration,
    ) -> SwappedSessionRouteRollback {
        let gate = session_lifecycle_gate(&self.state, session_id).await;
        let _guard = gate.lock().await;
        let rollback = {
            let mut routes = self.state.session_to_helper.lock().await;
            rollback_swapped_session_route_locked(&mut routes, self.helper_id, session_id, previous)
        };
        let should_close = match rollback {
            SwappedSessionRouteRollback::Restored => !previous_used_same_agent,
            SwappedSessionRouteRollback::OwnershipChanged {
                current_agent_instance_id,
            } => current_agent_instance_id != Some(agent.instance_id),
        };
        if should_close {
            // Keep the SessionId lifecycle gate held through the send so a
            // same-agent rebind cannot start after the safety check.
            self.best_effort_close_loaded_target(agent, session_id, timeout)
                .await;
        }
        rollback
    }

    async fn best_effort_close_loaded_target(
        &self,
        agent: &AgentCli,
        session_id: &acp::schema::v1::SessionId,
        timeout: std::time::Duration,
    ) {
        if !agent_supports_session_close(agent) || timeout.is_zero() {
            return;
        }
        match tokio::time::timeout(
            timeout,
            agent
                .conn
                .close_session(acp::schema::v1::CloseSessionRequest::new(
                    session_id.clone(),
                )),
        )
        .await
        {
            Ok(Ok(_)) => tracing::info!(
                target: "master",
                step = "helper→agent",
                op = "rollback_close_loaded_target",
                helper_id = ?self.helper_id,
                session_id = %session_id,
                outcome = "closed",
                "closed newly loaded target after predecessor close failed"
            ),
            Ok(Err(error)) => tracing::warn!(
                target: "master",
                step = "helper→agent",
                op = "rollback_close_loaded_target",
                helper_id = ?self.helper_id,
                session_id = %session_id,
                outcome = "acp_error",
                error = %error,
                "failed to close newly loaded target during rollback"
            ),
            Err(_) => tracing::warn!(
                target: "master",
                step = "helper→agent",
                op = "rollback_close_loaded_target",
                helper_id = ?self.helper_id,
                session_id = %session_id,
                outcome = "timeout",
                timeout_ms = timeout.as_millis() as u64,
                "timed out closing newly loaded target during rollback"
            ),
        }
    }
}

impl HelperHandler {
    async fn get_or_initialize_agent<F, Fut>(&self, acquire: F) -> Result<Arc<AgentCli>>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<Arc<AgentCli>>>,
    {
        let agent = self
            .agent
            .get_or_try_init(|| acquire_and_bind_agent(&self.state, self.helper_id, acquire))
            .await?;
        Ok(Arc::clone(agent))
    }

    async fn session_mcp_endpoint_for_session(
        &self,
        agent: &AgentCli,
        wta_meta: &crate::session_registry::WtaMeta,
        operation: &str,
    ) -> acp::Result<Option<String>> {
        if wta_meta.proposal_mcp.as_deref() != Some("http-v1") {
            return Ok(None);
        }
        if !agent
            .cached_init_resp
            .agent_capabilities
            .mcp_capabilities
            .http
        {
            tracing::error!(
                target: "session_mcp",
                helper_id = ?self.helper_id,
                op = operation,
                source = %agent.source,
                "helper requested session MCP after agent HTTP support disappeared"
            );
            return Err(acp::Error::internal_error().data(serde_json::json!(
                "session MCP is unavailable because the agent does not support HTTP MCP"
            )));
        }
        session_mcp::endpoint_for(&self.state, &agent.source)
            .await
            .map(Some)
            .map_err(|error| {
                let error_chain = format!("{error:#}");
                tracing::error!(
                    target: "session_mcp",
                    helper_id = ?self.helper_id,
                    op = operation,
                    source = %agent.source,
                    error = %error_chain,
                    "session MCP endpoint became unavailable after initialize"
                );
                acp::Error::internal_error().data(serde_json::json!(format!(
                    "session MCP endpoint unavailable: {error_chain}"
                )))
            })
    }

    async fn initialize(
        &self,
        mut args: acp::schema::v1::InitializeRequest,
    ) -> acp::Result<acp::schema::v1::InitializeResponse> {
        // The helper declares which agent this tab wants in `_meta.wta`
        // by *identity* (id + model). Strip the namespace so it can never
        // reach an agent CLI, then resolve the command the master will
        // actually spawn. Crucially we NEVER execute a command string off
        // the pipe: `resolve_agent_selection` reconstructs the command
        // from the declared id (only for known, GPO-allowed ids) and
        // otherwise falls back to the trusted `--agent` default. See
        // `resolve_agent_selection` for the full policy.
        let wta_meta = crate::session_registry::extract_wta_meta(&mut args.meta);
        let supplied_cloud_models: Vec<crate::app::AcpModelInfo> = wta_meta
            .cloud_models
            .as_deref()
            .and_then(|raw| match serde_json::from_str(raw) {
                Ok(models) => Some(models),
                Err(error) => {
                    tracing::warn!(
                        target: "master",
                        helper_id = ?self.helper_id,
                        %error,
                        "helper supplied invalid cloud model catalog metadata"
                    );
                    None
                }
            })
            .unwrap_or_default();
        let ResolvedAgentSelection {
            command: agent_cmd,
            agent_id,
            source: agent_source,
            explicit_selection,
        } = resolve_agent_selection(
            &self.state.default_agent_cmd,
            self.state.default_agent_id.as_deref(),
            self.state.allowed_agent_ids.as_ref(),
            wta_meta.agent_id.as_deref(),
            wta_meta.model.as_deref(),
            wta_meta.agent_source.as_deref(),
            wta_meta.wsl_distro.as_deref(),
            self.helper_id,
        );
        tracing::info!(
            target: "master",
            step = "helper→agent",
            op = "initialize",
            helper_id = ?self.helper_id,
            protocol_version = ?args.protocol_version,
            requested_agent_id = ?wta_meta.agent_id,
            resolved_agent_cmd = %agent_cmd,
            resolved_agent_id = ?agent_id,
            resolved_agent_source = %agent_source,
            "resolving agent CLI for helper"
        );
        let supplied_cloud_models =
            if matches!(&agent_source, crate::agent_source::AgentSource::Host) {
                supplied_cloud_models
            } else {
                if !supplied_cloud_models.is_empty() {
                    tracing::warn!(
                        target: "master",
                        helper_id = ?self.helper_id,
                        resolved_agent_source = %agent_source,
                        "ignoring Host cloud model catalog metadata from WSL helper"
                    );
                }
                Vec::new()
            };
        let provider_binding = resolve_provider_binding(
            &self.state,
            agent_id.as_deref(),
            &agent_source,
            wta_meta.provider_binding.as_deref(),
            explicit_selection,
        )
        .await
        .map_err(|error| {
            tracing::error!(
                target: "master",
                op = "initialize",
                helper_id = ?self.helper_id,
                agent_id = ?agent_id,
                "failed to resolve custom model binding: {error:#}"
            );
            helper_initialize_error(HelperInitializeFailure::ProviderResolution, &error)
        })?;

        let agent = self
            .get_or_initialize_agent(|| {
                get_or_spawn_agent(
                    &self.state,
                    &agent_cmd,
                    agent_id.as_deref(),
                    &agent_source,
                    provider_binding.clone(),
                    supplied_cloud_models.clone(),
                )
            })
            .await
            .map_err(|e| {
                let error_chain = format!("{e:#}");
                tracing::error!(
                    target: "master",
                    op = "initialize",
                    helper_id = ?self.helper_id,
                    agent_cmd = %agent_cmd,
                    error = %error_chain,
                    "failed to spawn/resolve agent CLI for helper"
                );
                helper_initialize_error(HelperInitializeFailure::AgentStartup, &e)
            })?;
        let session_mcp_available = if agent
            .cached_init_resp
            .agent_capabilities
            .mcp_capabilities
            .http
        {
            match session_mcp::endpoint_for(&self.state, &agent.source).await {
                Ok(_) => true,
                Err(error) => {
                    tracing::warn!(
                        target: "session_mcp",
                        helper_id = ?self.helper_id,
                        source = %agent.source,
                        error = %format!("{error:#}"),
                        "session MCP is unavailable for helper source"
                    );
                    false
                }
            }
        } else {
            false
        };

        // Replay the CLI's own initialize response (re-forwarding returns
        // empty `agent_info` on most backends, blanking the agent bar), adding
        // only our private helper-facing cloud catalog metadata. The original
        // third-party response capabilities remain untouched.
        match initialize_response_for_agent(&agent, session_mcp_available).await {
            Ok(response) => Ok(response),
            Err(error) => {
                tracing::warn!(
                    target: "master",
                    helper_id = ?self.helper_id,
                    %error,
                    "failed to serialize private cloud model catalog metadata"
                );
                Ok(agent.cached_init_resp.clone())
            }
        }
    }

    async fn authenticate(
        &self,
        args: acp::schema::v1::AuthenticateRequest,
    ) -> acp::Result<acp::schema::v1::AuthenticateResponse> {
        tracing::info!(
            target: "master",
            step = "helper→agent",
            op = "authenticate",
            helper_id = ?self.helper_id,
            "forwarding authenticate"
        );
        self.resolved_agent("authenticate")?
            .conn
            .authenticate(args)
            .await
    }

    async fn close_session(
        &self,
        args: acp::schema::v1::CloseSessionRequest,
    ) -> acp::Result<acp::schema::v1::CloseSessionResponse> {
        let _replacement_guard = self.replacement_gate.lock().await;
        let agent = self.resolved_agent("close_session")?;
        let session_id = args.session_id;
        let cleanup = close_and_retire_replaced_session(
            &self.state,
            self.helper_id,
            &agent,
            &session_id,
            SESSION_CLOSE_TIMEOUT,
        )
        .await?;
        if cleanup == ReplacedSessionCleanup::NotOwned {
            return Err(acp::Error::invalid_params().data(serde_json::json!({
                "message": format!("session {session_id} is not owned by this helper")
            })));
        }
        self.state.helper_meta.lock().await.remove(&self.helper_id);
        tracing::info!(
            target: "master",
            helper_id = ?self.helper_id,
            session_id = %session_id,
            cleanup = ?cleanup,
            "closed helper-owned ACP session"
        );
        Ok(acp::schema::v1::CloseSessionResponse::new())
    }

    async fn new_session(
        &self,
        args: acp::schema::v1::NewSessionRequest,
    ) -> acp::Result<acp::schema::v1::NewSessionResponse> {
        let _replacement_guard = self.replacement_gate.lock().await;
        // Pull our `_meta.wta` payload off the request before forwarding
        // to the agent CLI. Two reasons we strip here and not after the
        // RPC: (1) the spec lets third-party agents reject unknown
        // top-level meta keys, so anything not under their own
        // namespace must not leak through master; (2) we record the
        // helper-supplied `pane_session_id` against the session id in
        // B-4 — keeping the extract here means the binding is captured
        // in the same place as the routing entry.
        let mut args = args;
        let wta_meta = crate::session_registry::extract_wta_meta(&mut args.meta);
        self.publish_pending_owner(wta_meta.owner_tab_id.clone())
            .await?;
        let previous_session_id = self
            .state
            .helper_meta
            .lock()
            .await
            .get(&self.helper_id)
            .and_then(|meta| meta.last_session_id.clone());
        let cwd_for_registry = args.cwd.clone();
        let agent = match self.resolved_agent("new_session") {
            Ok(agent) => agent,
            Err(error) => {
                self.finish_failed_pending_session().await;
                return Err(error);
            }
        };
        if let Some(previous_session_id) = previous_session_id.as_ref() {
            let cleanup = match close_and_retire_replaced_session(
                &self.state,
                self.helper_id,
                &agent,
                previous_session_id,
                SESSION_CLOSE_TIMEOUT,
            )
            .await
            {
                Ok(cleanup) => cleanup,
                Err(error) => {
                    self.finish_failed_pending_session().await;
                    return Err(error);
                }
            };
            {
                let mut meta = self.state.helper_meta.lock().await;
                if meta
                    .get(&self.helper_id)
                    .and_then(|meta| meta.last_session_id.as_ref())
                    == Some(previous_session_id)
                {
                    meta.entry(self.helper_id).or_default().last_session_id = None;
                }
            }
            tracing::info!(
                target: "master",
                helper_id = ?self.helper_id,
                old_session_id = %previous_session_id,
                cleanup = ?cleanup,
                "finished predecessor cleanup before session/new"
            );
        }
        let session_mcp_endpoint = match self
            .session_mcp_endpoint_for_session(&agent, &wta_meta, "new_session")
            .await
        {
            Ok(endpoint) => endpoint,
            Err(error) => {
                self.finish_failed_pending_session().await;
                return Err(error);
            }
        };
        let session_mcp = if let Some(endpoint) = session_mcp_endpoint {
            let pending = self
                .state
                .session_mcp_capabilities
                .prepare(agent.instance_id, None)
                .await;
            self.state
                .pending_session_mcp
                .lock()
                .await
                .insert(self.helper_id, pending.clone());
            args.mcp_servers
                .push(session_mcp::server_config(&endpoint, &pending));
            Some(pending)
        } else {
            None
        };
        tracing::info!(
            target: "master",
            step = "helper→agent",
            op = "new_session",
            helper_id = ?self.helper_id,
            mcp_servers = args.mcp_servers.len(),
            pane_session_id = ?wta_meta.pane_session_id,
            "forwarding new_session"
        );
        let mut resp = match self
            .forward_new_session_to_agent(
                args,
                std::time::Duration::from_secs(SESSION_NEW_TIMEOUT_SECS),
            )
            .await
        {
            Ok(response) => response,
            Err(error) => {
                self.finish_failed_pending_session().await;
                if let Some(pending) = session_mcp.as_ref() {
                    self.state.session_mcp_capabilities.cancel(pending).await;
                }
                return Err(error);
            }
        };
        if let Some(pending) = session_mcp.as_ref() {
            let bound = self
                .state
                .session_mcp_capabilities
                .bind(pending, resp.session_id.clone())
                .await;
            self.state
                .pending_session_mcp
                .lock()
                .await
                .remove(&self.helper_id);
            if !bound {
                tracing::warn!(
                    target: "session_mcp",
                    session_id = %resp.session_id,
                    "session MCP capability disappeared before session binding"
                );
            }
        }
        let (available_models, current_model_id) =
            crate::protocol::acp::model_select::models_from_new_session(&resp);
        let forwarder = match self.forwarder_for_route("new_session") {
            Ok(forwarder) => forwarder,
            Err(error) => {
                if let Some(pending) = session_mcp.as_ref() {
                    self.state.session_mcp_capabilities.cancel(pending).await;
                }
                // The agent has already created this session, but the helper
                // forwarder disappeared before normal route installation.
                // Establish temporary ownership so the standard gated close
                // path can physically retire the otherwise-unreachable session.
                bind_session_route(
                    &self.state,
                    resp.session_id.clone(),
                    HelperRoute {
                        helper_id: self.helper_id,
                        agent_instance_id: agent.instance_id,
                        notif_tx: self.notif_tx.clone(),
                        forwarder: None,
                        consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                    },
                )
                .await;
                if let Err(cleanup_error) = close_and_retire_replaced_session(
                    &self.state,
                    self.helper_id,
                    &agent,
                    &resp.session_id,
                    SESSION_CLOSE_TIMEOUT,
                )
                .await
                {
                    tracing::warn!(
                        target: "master",
                        helper_id = ?self.helper_id,
                        session_id = %resp.session_id,
                        error = ?cleanup_error,
                        "failed to retire session/new result after helper forwarder disappeared"
                    );
                }
                self.finish_failed_pending_session().await;
                return Err(error);
            }
        };
        // Record routing entry BEFORE returning so the helper can't
        // race a session/update notification.
        let registry_size = bind_session_route(
            &self.state,
            resp.session_id.clone(),
            HelperRoute {
                helper_id: self.helper_id,
                agent_instance_id: agent.instance_id,
                notif_tx: self.notif_tx.clone(),
                forwarder: Some(forwarder),
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        )
        .await;
        if self
            .state
            .closing_session_helpers
            .lock()
            .await
            .contains(&self.helper_id)
        {
            let cleanup = self
                .close_session_for_destroyed_tab(&agent, &resp.session_id)
                .await;
            self.finish_failed_pending_session().await;
            let cleanup = cleanup?;
            if cleanup == ReplacedSessionCleanup::NotOwned {
                retire_unbound_session_state(&self.state, &resp.session_id).await;
            }
            tracing::info!(
                target: "master",
                helper_id = ?self.helper_id,
                session_id = %resp.session_id,
                cleanup = ?cleanup,
                "closed ACP session created after its owning tab was destroyed"
            );
            crate::session_registry::inject_wta_meta(
                &mut resp.meta,
                &crate::session_registry::WtaMeta {
                    session_result: Some("retired".to_string()),
                    ..Default::default()
                },
            );
            return Ok(resp);
        }
        // Mirror the binding into the live-session registry. Lock
        // ordering matches the doc on `MasterStateInner::registry`:
        // `session_to_helper` is no longer held here, so the upsert
        // can't deadlock against `drop_sessions_for_helper`.
        let mut info =
            crate::session_registry::SessionInfo::new(resp.session_id.clone(), cwd_for_registry);
        info.pane_session_id = wta_meta.pane_session_id;
        // Stamp the row as a Live agent-pane session. Without this, the
        // row lands in master's registry with status=cli_source=origin=None,
        // and helper-side session management routing treats it as Historical (the default
        // fallback in session_info_to_agent_session). Enter on it then
        // tries to resume and fails with "unknown CLI" since cli_source
        // is None. Agent-pane sessions never get a SessionStarted hook
        // (those fire for shell-pane agents through native CLI hooks
        // only), so master is the only one that can fill these fields.
        info.status = Some(crate::agent_sessions::AgentStatus::Idle);
        info.cli_source = agent.cli_source.clone();
        info.location = agent.source.session_location();
        info.origin = Some(crate::agent_sessions::SessionOrigin::AgentPane);
        info.last_activity_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_millis() as u64);
        self.state.registry.upsert(info.clone()).await;
        // Commit ownership only if the tab's outgoing helper generation has
        // not been retired concurrently.
        if !self.commit_pending_session(&resp.session_id).await {
            let cleanup = self
                .close_session_for_destroyed_tab(&agent, &resp.session_id)
                .await;
            self.finish_failed_pending_session().await;
            let cleanup = cleanup?;
            if cleanup == ReplacedSessionCleanup::NotOwned {
                retire_unbound_session_state(&self.state, &resp.session_id).await;
            }
            tracing::info!(
                target: "master",
                helper_id = ?self.helper_id,
                session_id = %resp.session_id,
                cleanup = ?cleanup,
                "closed ACP session committed concurrently with tab destruction"
            );
            crate::session_registry::inject_wta_meta(
                &mut resp.meta,
                &crate::session_registry::WtaMeta {
                    session_result: Some("retired".to_string()),
                    ..Default::default()
                },
            );
            return Ok(resp);
        }
        // helper so their mirrors learn about this new row without
        // having to re-run `session/list`. The disconnecting-helper
        // race is benign: if a peer disconnects between us picking it
        // up here and the actual write, the prune path in
        // `broadcast_ext_to_helpers` cleans up its subscriber slot.
        crate::master::broadcast_ext_to_helpers(
            &self.state,
            crate::session_registry::build_session_added_notification(&info),
        )
        .await;
        crate::master::broadcast_ext_to_helpers(
            &self.state,
            crate::session_registry::build_sessions_changed_notification(),
        )
        .await;
        // Trace the model the agent actually selected for this session at
        // INFO. When the WT `acpModel` setting is empty (the "agent default"
        // case) we forward no setSessionModel, so this current_model_id from
        // the agent's NewSessionResponse is the only INFO-level record of
        // which model is really in effect — the acp-client current_model_id
        // line is debug-only. The explicit case is already covered by the
        // "forwarding set_session_model" log.
        let agent_model_count = available_models.len();
        tracing::info!(
            target: "master",
            step = "helper→agent",
            op = "new_session",
            helper_id = ?self.helper_id,
            session_id = ?resp.session_id,
            registry_size = registry_size,
            current_model_id = ?current_model_id,
            available_models = agent_model_count,
            "session bound to helper"
        );
        Ok(resp)
    }

    async fn load_session(
        &self,
        args: acp::schema::v1::LoadSessionRequest,
    ) -> acp::Result<acp::schema::v1::LoadSessionResponse> {
        self.load_session_with_timeout(
            args,
            std::time::Duration::from_secs(SESSION_LOAD_TIMEOUT_SECS),
        )
        .await
    }

    async fn load_session_with_timeout(
        &self,
        args: acp::schema::v1::LoadSessionRequest,
        timeout: std::time::Duration,
    ) -> acp::Result<acp::schema::v1::LoadSessionResponse> {
        let deadline = tokio::time::Instant::now() + timeout;
        let _replacement_guard = tokio::time::timeout_at(deadline, self.replacement_gate.lock())
            .await
            .map_err(|_| self.load_session_timeout_error(timeout, "replacement_gate"))?;
        let mut args = args;
        let wta_meta = crate::session_registry::extract_wta_meta(&mut args.meta);
        self.publish_pending_owner(wta_meta.owner_tab_id.clone())
            .await?;
        let session_id = args.session_id.clone();
        let previous_session_id = self
            .state
            .helper_meta
            .lock()
            .await
            .get(&self.helper_id)
            .and_then(|meta| meta.last_session_id.clone());
        let rollback_reserve = previous_session_id
            .as_ref()
            .is_some_and(|sid| sid != &session_id)
            .then(|| SESSION_ROLLBACK_CLOSE_TIMEOUT.min(timeout / 2))
            .unwrap_or_default();
        let cwd_for_registry = args.cwd.clone();
        tracing::info!(
            target: "master",
            step = "helper→agent",
            op = "load_session",
            helper_id = ?self.helper_id,
            session_id = ?session_id,
            pane_session_id = ?wta_meta.pane_session_id,
            "forwarding load_session"
        );
        // Pre-register routing BEFORE awaiting the agent CLI.
        //
        // Unlike `new_session`, the SessionId for `load_session` is a
        // request input (the resume target) so we already know it.
        // Agents commonly replay the session's history as a burst of
        // `session/update` notifications *while* `load_session` is
        // still executing on their side. If we waited for the response
        // to install the routing entry, those early notifications hit
        // `MasterClient::session_notification` with an unknown sid and
        // get dropped — the user-visible symptom is "I see no scroll-
        // back when I resume". Pre-registration closes that window.
        //
        // We do NOT pre-upsert into the live-session registry: peer
        // helpers shouldn't observe a row that the load could still
        // fail on. On success we upsert + broadcast `session_added`
        // atomically; on failure we just unregister routing without
        // any peer-visible flicker.
        let agent = match self.resolved_agent("load_session") {
            Ok(agent) => agent,
            Err(error) => {
                self.finish_failed_pending_session().await;
                return Err(error);
            }
        };
        let forwarder = match self.forwarder_for_route("load_session") {
            Ok(forwarder) => forwarder,
            Err(error) => {
                self.finish_failed_pending_session().await;
                return Err(error);
            }
        };
        let previous_target_route = swap_session_route(
            &self.state,
            session_id.clone(),
            HelperRoute {
                helper_id: self.helper_id,
                agent_instance_id: agent.instance_id,
                notif_tx: self.notif_tx.clone(),
                forwarder: Some(forwarder),
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        )
        .await;
        let previous_target_used_same_agent = previous_target_route
            .as_ref()
            .is_some_and(|route| route.agent_instance_id == agent.instance_id);
        // Orphan re-bind fast path: this session's previous helper
        // disconnected but the shared CLI still has it loaded (tracked in
        // `orphaned_sessions` under this agent's key). Re-attach onto the
        // routing pre-registered above WITHOUT a `session/load` round-trip —
        // the CLI already has the session, and forwarding a load would be
        // rejected "already loaded", or (if the orphan turn is still running)
        // wedge behind it and hang the pane on "Resuming…". Any in-flight
        // turn now streams its `session/update`s to this new helper. Scoped
        // to `agent.cmd_key` so we only re-bind sessions this exact CLI still
        // holds (a crashed+respawned CLI's set was dropped by `reap_agent`).
        let is_orphan_rebind = {
            let mut orphans = self.state.orphaned_sessions.lock().await;
            orphans
                .get_mut(&agent.cmd_key)
                .is_some_and(|set| set.remove(&session_id))
        };
        // Both a re-bind and a real `session/load` resume the session; only a
        // genuine load failure rolls back. Resolve the response, then register
        // the resumed row once for either success path.
        let mut session_mcp = None;
        let mut loaded_target_physically = false;
        let mut rebound_existing_session = false;
        let mut resp = if is_orphan_rebind {
            tracing::info!(
                target: "master",
                step = "helper→agent",
                op = "load_session",
                helper_id = ?self.helper_id,
                session_id = ?session_id,
                "re-binding orphan session without a session/load round-trip"
            );
            acp::schema::v1::LoadSessionResponse::new()
        } else {
            let session_mcp_endpoint = match self
                .session_mcp_endpoint_for_session(&agent, &wta_meta, "load_session")
                .await
            {
                Ok(endpoint) => endpoint,
                Err(error) => {
                    self.finish_failed_pending_session().await;
                    rollback_swapped_session_route(
                        &self.state,
                        self.helper_id,
                        &session_id,
                        previous_target_route,
                    )
                    .await;
                    return Err(error);
                }
            };
            session_mcp = if let Some(endpoint) = session_mcp_endpoint {
                let pending = self
                    .state
                    .session_mcp_capabilities
                    .prepare(agent.instance_id, Some(session_id.clone()))
                    .await;
                self.state
                    .pending_session_mcp
                    .lock()
                    .await
                    .insert(self.helper_id, pending.clone());
                args.mcp_servers
                    .push(session_mcp::server_config(&endpoint, &pending));
                Some(pending)
            } else {
                None
            };
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let load_rpc_timeout = remaining.saturating_sub(rollback_reserve);
            let load_result = if load_rpc_timeout.is_zero() {
                Err(self.load_session_timeout_error(timeout, "agent_rpc"))
            } else {
                self.forward_load_session_to_agent(args, load_rpc_timeout, timeout)
                    .await
            };
            match load_result {
                Ok(resp) => {
                    loaded_target_physically = true;
                    resp
                }
                // Fallback for an orphan we didn't track (e.g. it predates
                // this master): the CLI reports "already loaded", so re-bind
                // onto the pre-registered routing just like the fast path.
                Err(err) if is_already_loaded_error(&err) => {
                    rebound_existing_session = true;
                    if let Some(pending) = session_mcp.take() {
                        self.state.session_mcp_capabilities.cancel(&pending).await;
                        self.state
                            .pending_session_mcp
                            .lock()
                            .await
                            .remove(&self.helper_id);
                    }
                    tracing::info!(
                        target: "master",
                        step = "helper→agent",
                        op = "load_session",
                        helper_id = ?self.helper_id,
                        session_id = ?session_id,
                        "re-binding session already loaded in the shared CLI"
                    );
                    acp::schema::v1::LoadSessionResponse::new()
                }
                Err(err) => {
                    self.finish_failed_pending_session().await;
                    if let Some(pending) = session_mcp.as_ref() {
                        self.state.session_mcp_capabilities.cancel(pending).await;
                    }
                    // Roll back the pre-registration. Only `session_to_helper`
                    // needs touching — we never wrote to `registry` and we
                    // never broadcast `session_added`, so peers never saw
                    // this row.
                    let route_rollback = self
                        .rollback_and_maybe_close_loaded_target(
                            &agent,
                            &session_id,
                            previous_target_route,
                            previous_target_used_same_agent,
                            deadline
                                .saturating_duration_since(tokio::time::Instant::now())
                                .min(rollback_reserve),
                        )
                        .await;
                    tracing::warn!(
                        target: "master",
                        helper_id = ?self.helper_id,
                        session_id = ?session_id,
                        route_rollback = ?route_rollback,
                        error = %err,
                        "load_session failed; rolled back routing entry"
                    );
                    return Err(err);
                }
            }
        };

        if self
            .state
            .closing_session_helpers
            .lock()
            .await
            .contains(&self.helper_id)
        {
            if let Some(pending) = session_mcp.as_ref() {
                self.state.session_mcp_capabilities.cancel(pending).await;
            }
            let cleanup_result = self
                .close_session_for_destroyed_tab(&agent, &session_id)
                .await;
            let cleanup_result = match cleanup_result {
                Ok(mut cleanup) => {
                    let mut predecessor_error = None;
                    if let Some(previous_session_id) = previous_session_id
                        .as_ref()
                        .filter(|sid| *sid != &session_id)
                    {
                        match self
                            .close_session_for_destroyed_tab(&agent, previous_session_id)
                            .await
                        {
                            Ok(predecessor_cleanup) => {
                                if cleanup == ReplacedSessionCleanup::NotOwned {
                                    cleanup = predecessor_cleanup;
                                }
                            }
                            Err(error) => predecessor_error = Some(error),
                        }
                    }
                    predecessor_error.map_or(Ok(cleanup), Err)
                }
                Err(error) => Err(error),
            };
            self.finish_failed_pending_session().await;
            let cleanup = cleanup_result?;
            tracing::info!(
                target: "master",
                helper_id = ?self.helper_id,
                session_id = %session_id,
                cleanup = ?cleanup,
                "closed ACP session loaded after its owning tab was destroyed"
            );
            crate::session_registry::inject_wta_meta(
                &mut resp.meta,
                &crate::session_registry::WtaMeta {
                    session_result: Some("retired".to_string()),
                    ..Default::default()
                },
            );
            return Ok(resp);
        }

        if let Some(previous_session_id) = previous_session_id
            .as_ref()
            .filter(|sid| *sid != &session_id)
        {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let close_timeout = remaining
                .saturating_sub(rollback_reserve)
                .min(SESSION_CLOSE_TIMEOUT);
            let close_result = if close_timeout.is_zero() {
                Err(self.load_session_timeout_error(timeout, "predecessor_close"))
            } else {
                close_and_retire_replaced_session(
                    &self.state,
                    self.helper_id,
                    &agent,
                    previous_session_id,
                    close_timeout,
                )
                .await
            };
            let cleanup = match close_result {
                Ok(cleanup) => cleanup,
                Err(error) => {
                    self.finish_failed_pending_session().await;
                    if let Some(pending) = session_mcp.as_ref() {
                        self.state.session_mcp_capabilities.cancel(pending).await;
                    }
                    let route_rollback = if loaded_target_physically {
                        self.rollback_and_maybe_close_loaded_target(
                            &agent,
                            &session_id,
                            previous_target_route,
                            previous_target_used_same_agent,
                            deadline
                                .saturating_duration_since(tokio::time::Instant::now())
                                .min(rollback_reserve),
                        )
                        .await
                    } else if is_orphan_rebind || rebound_existing_session {
                        rollback_orphan_rebind(
                            &self.state,
                            self.helper_id,
                            &agent.cmd_key,
                            agent.instance_id,
                            &session_id,
                            previous_target_route,
                        )
                        .await
                    } else {
                        rollback_swapped_session_route(
                            &self.state,
                            self.helper_id,
                            &session_id,
                            previous_target_route,
                        )
                        .await
                    };
                    tracing::error!(
                        target: "master",
                        helper_id = ?self.helper_id,
                        old_session_id = %previous_session_id,
                        target_session_id = %session_id,
                        route_rollback = ?route_rollback,
                        "predecessor close failed after session/load; target transaction rolled back"
                    );
                    return Err(error);
                }
            };
            tracing::info!(
                target: "master",
                helper_id = ?self.helper_id,
                old_session_id = %previous_session_id,
                new_session_id = %session_id,
                cleanup = ?cleanup,
                "finished predecessor cleanup after successful session/load"
            );
        }

        if is_orphan_rebind || rebound_existing_session {
            self.state
                .orphaned_tabs
                .lock()
                .await
                .retain(|_, (key, _, orphan_session_id)| {
                    key != &agent.cmd_key || orphan_session_id != &session_id
                });
        }

        if let Some(pending) = session_mcp.as_ref() {
            let bound = self
                .state
                .session_mcp_capabilities
                .bind(pending, session_id.clone())
                .await;
            self.state
                .pending_session_mcp
                .lock()
                .await
                .remove(&self.helper_id);
            if !bound {
                tracing::warn!(
                    target: "session_mcp",
                    session_id = %session_id,
                    "session MCP capability disappeared before load binding"
                );
            }
        }

        let (available_models, current_model_id) =
            update_model_switch_channel_from_load(&session_id, &resp);
        if !available_models.is_empty() || current_model_id.is_some() {
            tracing::info!(
                target: "master",
                step = "helper→agent",
                op = "load_session",
                helper_id = ?self.helper_id,
                session_id = ?session_id,
                current_model_id = ?current_model_id,
                available_models = available_models.len(),
                "updated model selector from load_session response"
            );
        }

        // Register the resumed row (Live + tagged) — shared by the real-load
        // and orphan-re-bind paths.
        let mut info =
            crate::session_registry::SessionInfo::new(session_id.clone(), cwd_for_registry);
        info.pane_session_id = wta_meta.pane_session_id;
        info.status = Some(crate::agent_sessions::AgentStatus::Idle);
        info.cli_source = agent.cli_source.clone();
        info.location = agent.source.session_location();
        info.origin = Some(crate::agent_sessions::SessionOrigin::AgentPane);
        info.last_activity_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_millis() as u64);
        // Carry the title (and updated_at) forward from the row that already
        // exists for this sid so the resumed Live row stays identifiable
        // (master seeds the registry at startup with disk-derived chat titles;
        // a naked `SessionInfo::new` upsert would blank them to "—" in the
        // session-management view).
        if let Some(existing) = self.state.registry.lookup(&session_id).await {
            if info.title.is_none() {
                info.title = existing.title;
            }
            if info.updated_at.is_none() {
                info.updated_at = existing.updated_at;
            }
        }
        self.state.registry.upsert(info.clone()).await;
        // Commit ownership only if the tab's outgoing helper generation has
        // not been retired concurrently.
        if !self.commit_pending_session(&session_id).await {
            if let Some(pending) = session_mcp.as_ref() {
                self.state.session_mcp_capabilities.cancel(pending).await;
            }
            let cleanup = self
                .close_session_for_destroyed_tab(&agent, &session_id)
                .await;
            self.finish_failed_pending_session().await;
            let cleanup = cleanup?;
            if cleanup == ReplacedSessionCleanup::NotOwned {
                retire_unbound_session_state(&self.state, &session_id).await;
            }
            tracing::info!(
                target: "master",
                helper_id = ?self.helper_id,
                session_id = %session_id,
                cleanup = ?cleanup,
                "closed ACP session committed concurrently with tab destruction"
            );
            crate::session_registry::inject_wta_meta(
                &mut resp.meta,
                &crate::session_registry::WtaMeta {
                    session_result: Some("retired".to_string()),
                    ..Default::default()
                },
            );
            return Ok(resp);
        }
        crate::master::broadcast_ext_to_helpers(
            &self.state,
            crate::session_registry::build_session_added_notification(&info),
        )
        .await;
        crate::master::broadcast_ext_to_helpers(
            &self.state,
            crate::session_registry::build_sessions_changed_notification(),
        )
        .await;
        Ok(resp)
    }

    async fn set_session_mode(
        &self,
        args: acp::schema::v1::SetSessionModeRequest,
    ) -> acp::Result<acp::schema::v1::SetSessionModeResponse> {
        self.resolved_agent("set_session_mode")?
            .conn
            .set_session_mode(args)
            .await
    }

    // Forward config-option changes (incl. model selection) — the
    // `set_session_config_option` capability (driven by the ACP
    // `ConfigOptionUpdate` notifications the helper already handles)
    // and the trait default returns method_not_found, so anything
    // that flows through this path would also silently fail.
    async fn set_session_config_option(
        &self,
        args: acp::schema::v1::SetSessionConfigOptionRequest,
    ) -> acp::Result<acp::schema::v1::SetSessionConfigOptionResponse> {
        tracing::info!(
            target: "master",
            step = "helper→agent",
            op = "set_session_config_option",
            helper_id = ?self.helper_id,
            session_id = ?args.session_id,
            "forwarding set_session_config_option"
        );
        self.resolved_agent("set_session_config_option")?
            .conn
            .set_session_config_option(args)
            .await
    }

    /// Route the schema-1.1-dropped `session/set_model` request through this
    /// helper's bound AgentCli. The AgentCli owns the selector metadata and its
    /// MethodNotFound downgrade, so another pooled agent cannot overwrite it.
    async fn set_session_model(
        &self,
        args: conn::SetSessionModelRequest,
    ) -> acp::Result<conn::SetSessionModelResponse> {
        tracing::info!(
            target: "master",
            step = "helper→agent",
            op = "set_session_model",
            helper_id = ?self.helper_id,
            session_id = ?args.session_id,
            model = %args.model_id,
            "routing set_session_model for bound agent"
        );
        let agent = self.resolved_agent("set_session_model")?;
        crate::protocol::acp::model_select::apply_session_model(
            &agent.conn,
            args.session_id,
            args.model_id,
        )
        .await?;
        Ok(conn::SetSessionModelResponse::default())
    }

    /// Answer `session/list` from our own registry (NOT by proxying the
    /// helper's call to the agent CLI). The registry holds both live
    /// sessions and the historical rows seeded at startup / rescan from
    /// the agent's own `session/list`, Class-A-filtered by the
    /// `agent_pane_origin` index. Proxying the
    /// helper's call directly would bypass that merge + filter.
    ///
    /// The response carries our `pane_session_id` inside the standard
    /// `_meta.wta` namespace so the helper can join it with WT pane
    /// state for routing decisions in B-10/B-11.
    async fn list_sessions(
        &self,
        _args: acp::schema::v1::ListSessionsRequest,
    ) -> acp::Result<acp::schema::v1::ListSessionsResponse> {
        // Lock-order safety: this call only takes the registry mutex
        // (sub-µs hashmap snapshot, no awaits inside the critical
        // section). `drop_sessions_for_helper` mutates the registry
        // by calling `registry.remove(sid)` *after* releasing
        // `session_to_helper`'s mutex (see lock-order comment on
        // `MasterStateInner::registry`). Both operations are
        // serialized by the registry's own internal mutex, so any
        // ordering between a concurrent helper-drop and this
        // snapshot is acceptable:
        //   - snapshot first  → caller sees the about-to-drop sid;
        //                       the subsequent `session_removed`
        //                       broadcast reconciles it on the
        //                       caller's mirror.
        //   - drop first      → snapshot omits the sid; caller never
        //                       saw it as live, so nothing to clean up.
        // No torn-state window because the registry holds a
        // tokio::sync::Mutex<HashMap<...>> internally; each
        // upsert/remove/snapshot is one full hashmap op.
        let snapshot = self.state.registry.snapshot().await;
        tracing::info!(
            target: "master",
            op = "list_sessions",
            helper_id = ?self.helper_id,
            count = snapshot.len(),
            "answering session/list from master registry"
        );
        let sessions: Vec<acp::schema::v1::SessionInfo> = snapshot
            .into_iter()
            .map(|s| crate::session_registry::to_acp_session_info(&s))
            .collect();
        Ok(acp::schema::v1::ListSessionsResponse::new(sessions))
    }

    async fn prompt(
        &self,
        args: acp::schema::v1::PromptRequest,
        responder: acp::Responder<serde_json::Value>,
    ) -> acp::Result<()> {
        let helper_id = self.helper_id;
        tracing::info!(
            target: "master",
            step = "helper→agent",
            op = "prompt",
            helper_id = ?helper_id,
            session_id = ?args.session_id,
            content_chunks = args.prompt.len(),
            "forwarding prompt to agent CLI (non-blocking)"
        );
        let started = std::time::Instant::now();
        // Forward WITHOUT awaiting the turn: awaiting here would block this
        // helper's dispatch loop for the whole turn, so a reentrant
        // request_permission / create_terminal the agent issues mid-turn could
        // never be read back off the same loop — a cross-loop deadlock that
        // wedges the shared agent CLI. Register a continuation instead so the
        // loop stays free; the response is delivered to `responder` when the
        // agent replies. See ClientLink::prompt_forwarding.
        self.resolved_agent("prompt")?
            .conn
            .prompt_forwarding(args, move |resp| async move {
                let elapsed_ms = started.elapsed().as_millis() as u64;
                match &resp {
                    Ok(ok) => tracing::info!(
                        target: "master",
                        step = "helper→agent",
                        op = "prompt",
                        helper_id = ?helper_id,
                        stop_reason = ?ok.stop_reason,
                        elapsed_ms,
                        "prompt completed"
                    ),
                    Err(err) => tracing::warn!(
                        target: "master",
                        step = "helper→agent",
                        op = "prompt",
                        helper_id = ?helper_id,
                        error = %err,
                        elapsed_ms,
                        "prompt failed"
                    ),
                }
                // Deliver the turn result to the helper that issued the prompt.
                // This callback runs on the SHARED agent-CLI connection's
                // dispatch loop, so a delivery error must NEVER propagate: if
                // the helper's tab closed mid-turn its channel is gone, and
                // returning that error would tear the shared CLI connection
                // down and every other tab with it. Swallow it — the orphan
                // turn's result just has nowhere to go.
                if let Err(err) = conn::respond_enum(
                    responder,
                    resp.map(acp::schema::v1::AgentResponse::PromptResponse),
                ) {
                    tracing::info!(
                        target: "master",
                        op = "prompt",
                        helper_id = ?helper_id,
                        error = %err,
                        "dropping orphan prompt result (helper gone)"
                    );
                }
                Ok(())
            })
            .await
    }

    async fn cancel(&self, args: acp::schema::v1::CancelNotification) -> acp::Result<()> {
        tracing::info!(
            target: "master",
            step = "helper→agent",
            op = "cancel",
            helper_id = ?self.helper_id,
            session_id = ?args.session_id,
            "forwarding cancel"
        );
        self.resolved_agent("cancel")?.conn.cancel(args).await
    }

    /// Master answers our own `_intellterm.wta/*` ext methods locally
    /// (without round-tripping to the agent CLI); anything we don't
    /// recognize is forwarded so future agent-native extension methods
    /// still work. Routing + param decoding go through
    /// [`parse_ext_request`](crate::session_registry::parse_ext_request) so the
    /// ACP-1.0 leading-`_` normalization lives in one place and the match below
    /// is exhaustive (a new method is a compile error until it is handled,
    /// instead of silently falling through to the agent CLI).
    async fn ext_method(
        &self,
        args: acp::schema::v1::ExtRequest,
    ) -> acp::Result<acp::schema::v1::ExtResponse> {
        use crate::session_registry::WtaExtRequest as Req;
        tracing::debug!(
            target: "master",
            op = "ext_method",
            method = %args.method,
            helper_id = ?self.helper_id,
            "routing ext_method"
        );
        match crate::session_registry::parse_ext_request(args) {
            Req::FocusSession(p) => handle_focus_session(&self.state, &p).await,
            Req::SessionsList(p) => {
                // Not `resolved_agent`: `wta sessions list` reaches this method
                // without binding an agent, and that is a valid read-only
                // caller — not a protocol violation worth erroring on.
                let agent = self.agent.get().cloned();
                handle_sessions_list(&self.state, agent.as_deref(), &p).await
            }
            Req::SessionHook(ev) => handle_session_hook(&self.state, ev, false).await,
            Req::SessionBornBound(ev, wsl_distro) => {
                handle_session_born_bound(&self.state, ev, wsl_distro).await
            }
            Req::SessionResumeDispatched(p) => {
                handle_session_resume_dispatched(&self.state, &p).await
            }
            Req::SessionFocus(p) => handle_session_focus(&self.state, &p).await,
            Req::CloseTabSession(p) => handle_close_tab_session(&self.state, &p, false).await,
            Req::ForwardToAgent(raw) => {
                self.resolved_agent("ext_method")?
                    .conn
                    .ext_method(raw)
                    .await
            }
            Req::Malformed { method, error } => {
                tracing::warn!(
                    target: "master",
                    op = "ext_method",
                    %method,
                    %error,
                    helper_id = ?self.helper_id,
                    "rejecting malformed ext_method params"
                );
                Err(acp::Error::invalid_params().data(serde_json::json!({ "message": error })))
            }
        }
    }
}

/// Master mode entry point.
pub async fn run_master_mode(config: MasterConfig, pipe_name: String) -> Result<()> {
    // Logging is initialized once in `main()`; the WorkerGuard lives there for
    // the whole process so the non-blocking appender flushes on the graceful
    // shutdown path (see the `run_master_loop` shutdown notes below).
    tracing::info!(
        target: "master",
        pipe_name = %pipe_name,
        agent_cmd = %config.agent,
        "=== wta-master starting ==="
    );

    if config.agent.is_empty() {
        return Err(anyhow!(
            "wta-master requires --agent <cmd>; nothing to multiplex onto"
        ));
    }

    if config.session_management_enabled {
        // Reconciliation is fire-and-forget so slow third-party plugin
        // managers never delay ACP startup. Missing hooks are installed and
        // stale hooks are upgraded; failures remain isolated to session status.
        tokio::task::spawn_blocking(|| {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::agent_hooks_installer::reconcile_agent_hooks(
                    crate::agent_hooks_installer::CliScope::All,
                )
            }));
            match result {
                Ok(outcome) if !outcome.succeeded() => {
                    for failure in outcome.spawn_failures {
                        tracing::warn!(
                            target: "agent_hooks",
                            cli = failure.cli,
                            reason = %failure.reason,
                            "hook reconciliation command failed",
                        );
                    }
                    for cli in outcome.missing {
                        tracing::warn!(
                            target: "agent_hooks",
                            cli,
                            "hooks remain incomplete or stale after reconciliation",
                        );
                    }
                }
                Ok(_) => {}
                Err(panic) => {
                    let msg = panic
                        .downcast_ref::<&'static str>()
                        .copied()
                        .or_else(|| panic.downcast_ref::<String>().map(|s| s.as_str()))
                        .unwrap_or("<non-string panic payload>");
                    tracing::error!(
                        target: "agent_hooks",
                        panic = %msg,
                        "reconcile_agent_hooks panicked; master continues",
                    );
                }
            }
        });
    }

    let local_set = LocalSet::new();
    let result = local_set
        .run_until(async move { run_master_loop(config, pipe_name).await })
        .await;

    // Every master-side failure (named-pipe create/connect, agent CLI spawn,
    // ACP initialize timeout/failure, accept-loop shutdown) funnels through
    // here. Log with target=master so connection failures are always present
    // in wta-main_master.log, greppable alongside the success-path traces.
    if let Err(err) = &result {
        tracing::error!(target: "master", error = ?err, "wta-master exiting with error");
    }
    result
}

struct MasterPipeDiscoveryGuard {
    path: Option<PathBuf>,
    pipe_name: String,
}

impl MasterPipeDiscoveryGuard {
    fn write(pipe_name: &str) -> Self {
        let path = crate::runtime_paths::master_pipe_file_path();
        if let Some(path) = &path {
            if let Some(parent) = path.parent() {
                if let Err(err) = std::fs::create_dir_all(parent) {
                    tracing::warn!(
                        target: "master",
                        discovery_file = MASTER_PIPE_DISCOVERY_FILE,
                        pipe_name = %pipe_name,
                        error = %err,
                        "failed to create master pipe discovery directory"
                    );
                    return Self {
                        path: None,
                        pipe_name: pipe_name.to_string(),
                    };
                }
            }
            match std::fs::write(path, pipe_name) {
                Ok(()) => tracing::info!(
                    target: "master",
                    discovery_file = MASTER_PIPE_DISCOVERY_FILE,
                    pipe_name = %pipe_name,
                    "master pipe discovery file written"
                ),
                Err(err) => {
                    tracing::warn!(
                        target: "master",
                        discovery_file = MASTER_PIPE_DISCOVERY_FILE,
                        pipe_name = %pipe_name,
                        error = %err,
                        "failed to write master pipe discovery file"
                    );
                    return Self {
                        path: None,
                        pipe_name: pipe_name.to_string(),
                    };
                }
            }
        }
        Self {
            path,
            pipe_name: pipe_name.to_string(),
        }
    }
}

impl Drop for MasterPipeDiscoveryGuard {
    fn drop(&mut self) {
        let Some(path) = &self.path else {
            return;
        };
        let should_remove = std::fs::read_to_string(path)
            .map(|current| current.trim() == self.pipe_name)
            .unwrap_or(false);
        if should_remove {
            if let Err(err) = std::fs::remove_file(path) {
                tracing::warn!(
                    target: "master",
                    discovery_file = MASTER_PIPE_DISCOVERY_FILE,
                    pipe_name = %self.pipe_name,
                    error = %err,
                    "failed to remove master pipe discovery file"
                );
            }
        }
    }
}

/// Owns a self-relative security descriptor (built from an SDDL string)
/// and the `SECURITY_ATTRIBUTES` that points at it, so the named pipe can
/// be created with a tightened ACL. Frees the descriptor on drop.
///
/// Must outlive every `create_*` call that consumes its `sa_ptr()` — in
/// practice it lives for the whole accept loop (each follow-up pipe
/// instance is created with the same attributes). Do not move it after
/// taking `sa_ptr()`.
struct PipeSecurity {
    sa: windows_sys::Win32::Security::SECURITY_ATTRIBUTES,
    /// The descriptor `sa.lpSecurityDescriptor` aliases. Kept so `Drop`
    /// can `LocalFree` exactly the allocation Windows handed us.
    psd: *mut std::ffi::c_void,
}

impl PipeSecurity {
    fn sa_ptr(&self) -> *mut std::ffi::c_void {
        &self.sa as *const _ as *mut std::ffi::c_void
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        if !self.psd.is_null() {
            // LocalFree takes/returns HLOCAL (= *mut c_void); ignore the
            // (null on success) return.
            unsafe {
                windows_sys::Win32::Foundation::LocalFree(self.psd);
            }
        }
    }
}

/// Resolve the current process user's SID as an SDDL string (e.g.
/// `"S-1-5-21-…"`). Returns `None` on any failure so the caller can fall
/// back to the default pipe ACL rather than refuse to start.
fn current_user_sid_string() -> Option<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return None;
        }
        // Size probe (fails with ERROR_INSUFFICIENT_BUFFER, fills `len`).
        let mut len: u32 = 0;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
        if len == 0 {
            CloseHandle(token);
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        let ok = GetTokenInformation(
            token,
            TokenUser,
            buf.as_mut_ptr() as *mut std::ffi::c_void,
            len,
            &mut len,
        );
        CloseHandle(token);
        if ok == 0 {
            return None;
        }
        // `buf` is a `Vec<u8>` (alignment 1), but `TOKEN_USER` contains a
        // pointer and so needs pointer alignment — forming
        // `&*(buf.as_ptr() as *const TOKEN_USER)` would create a reference to
        // a potentially-misaligned address, which is UB in Rust. Copy the
        // header out with an unaligned read into a properly-aligned local
        // instead. `token_user.User.Sid` still points *into* `buf` (kept
        // alive until after the conversion below), which is what
        // `ConvertSidToStringSidW` dereferences.
        let token_user = std::ptr::read_unaligned(buf.as_ptr() as *const TOKEN_USER);
        let mut sid_str: *mut u16 = std::ptr::null_mut();
        if ConvertSidToStringSidW(token_user.User.Sid, &mut sid_str) == 0 || sid_str.is_null() {
            return None;
        }
        // Copy out the wide string, then free Windows' allocation.
        let mut n = 0usize;
        while *sid_str.add(n) != 0 {
            n += 1;
        }
        let slice = std::slice::from_raw_parts(sid_str, n);
        let s = String::from_utf16_lossy(slice);
        LocalFree(sid_str as *mut std::ffi::c_void);
        Some(s)
    }
}

/// Build a `PipeSecurity` granting full control only to SYSTEM and the
/// current user (protected DACL → denies other users and, with
/// `reject_remote_clients`, remote connectors), plus a medium-integrity
/// no-write-up mandatory label (blocks lower-integrity / AppContainer
/// same-user code). This is **defense in depth**: it does not separate a
/// same-user, medium-integrity, full-trust process — which is exactly why
/// the master never executes a command string off the pipe
/// (`resolve_agent_selection`) and that, not this ACL, is the real fix.
///
/// Returns `None` (caller falls back to the default ACL) on any failure;
/// hardening should never be the reason the master can't start.
fn build_pipe_security_attributes() -> Option<PipeSecurity> {
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;

    let user_sid = current_user_sid_string()?;
    // D:P → protected DACL (no inheritance). GA = GENERIC_ALL.
    //   (A;;GA;;;SY)        SYSTEM
    //   (A;;GA;;;<user>)    the current user
    // S:(ML;;NW;;;ME)       mandatory label: Medium IL, no-write-up.
    let sddl = format!("D:P(A;;GA;;;SY)(A;;GA;;;{user_sid})S:(ML;;NW;;;ME)");
    let sddl_w: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();

    let mut psd: *mut std::ffi::c_void = std::ptr::null_mut();
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl_w.as_ptr(),
            SDDL_REVISION_1 as u32,
            &mut psd,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 || psd.is_null() {
        tracing::warn!(
            target: "master",
            "failed to build pipe security descriptor from SDDL; using default ACL"
        );
        return None;
    }

    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: psd,
        bInheritHandle: 0,
    };
    Some(PipeSecurity { sa, psd })
}

/// Create one named-pipe server instance, applying `security` when
/// available. Always rejects remote clients. Shared by the first-instance
/// and the follow-up-instance create sites so neither can silently regress
/// to the default ACL.
fn create_master_pipe_instance(
    pipe_name: &str,
    first_instance: bool,
    security: Option<&PipeSecurity>,
) -> std::io::Result<NamedPipeServer> {
    let mut opts = ServerOptions::new();
    opts.first_pipe_instance(first_instance);
    opts.reject_remote_clients(true);
    match security {
        // SAFETY: `sa_ptr()` points at a `SECURITY_ATTRIBUTES` whose
        // descriptor stays valid for the lifetime of `security` (the
        // caller holds it across the whole accept loop).
        Some(sec) => unsafe { opts.create_with_security_attributes_raw(pipe_name, sec.sa_ptr()) },
        None => opts.create(pipe_name),
    }
}

async fn run_master_loop(config: MasterConfig, pipe_name: String) -> Result<()> {
    // Best-effort wtcli/COM channel for intellterm.wta/focus_session AND
    // the WT connection_state -> PaneClosed bridge: master demotes F2 rows
    // to Ended on pane-close even when no helper publishes a `PaneClosed`
    // hook (notably Gemini's hard-close, whose SessionEnd hook doesn't run
    // reliably). Event subscription needs the concrete `CliChannel` (the
    // `WtChannel` trait surface doesn't expose it), so bind `wt_cli` first,
    // subscribe, then wrap as `dyn WtChannel`. On the rare boot path with
    // no WT (`WT_COM_CLSID` unset) we degrade to `None`.
    let wt_cli: Option<Arc<crate::shell::wt_channel::CliChannel>> =
        match crate::shell::wt_channel::CliChannel::connect().await {
            Ok(ch) => Some(Arc::new(ch)),
            Err(err) => {
                tracing::warn!(
                    target: "master",
                    error = %err,
                    "CliChannel unavailable; intellterm.wta/focus_session will error, \
                     and master will not bridge WT connection_state -> PaneClosed"
                );
                None
            }
        };
    // Subscribe to WT events + start the reader BEFORE wrapping as
    // `dyn WtChannel` (the trait surface doesn't expose subscription).
    // Single-consumer: focus_session uses the same channel via request/
    // response, which doesn't touch the event sender.
    let wt_event_rx = wt_cli.as_ref().map(|c| c.subscribe_events());
    if let Some(ref c) = wt_cli {
        c.start_reader().await;
    }
    let wt: Option<Arc<dyn crate::shell::wt_channel::WtChannel>> = wt_cli
        .clone()
        .map(|c| c as Arc<dyn crate::shell::wt_channel::WtChannel>);

    // Agent CLIs are spawned LAZILY by `get_or_spawn_agent` the first time
    // a helper declares an agent in its `initialize` handshake — the master
    // no longer owns a single eager agent CLI. `config.agent` / `config.agent_id`
    // become the fallback default for helpers that don't declare one.
    // Host-supplied allowlist (GPO-filtered) of agent ids a helper may
    // select. An *absent* flag means "no allowlist; accept any known id"
    // (`None`); a *present* flag is honored fail-closed even when it filters
    // down to nothing (`Some(empty_set)` ⇒ block all) — see
    // `normalize_allowed_agent_ids` for the absent-vs-present-empty split.
    let allowed_agent_ids = normalize_allowed_agent_ids(&config.allowed_agent_ids);
    tracing::info!(
        target: "master",
        allowed_agent_ids = ?allowed_agent_ids,
        default_agent_id = ?config.agent_id,
        "agent allowlist resolved"
    );

    let session_mcp_listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .context("bind master session MCP HTTP endpoint")?;
    let session_mcp_endpoint = format!(
        "http://{}/mcp",
        session_mcp_listener
            .local_addr()
            .context("read master session MCP HTTP endpoint")?
    );
    let inner = Arc::new(MasterStateInner {
        session_lifecycle_gates: Mutex::new(HashMap::new()),
        session_to_helper: Mutex::new(HashMap::new()),
        session_mcp_endpoints: session_mcp::Endpoints::new(session_mcp_endpoint),
        session_mcp_capabilities: session_mcp::CapabilityRegistry::default(),
        pending_usage: Mutex::new(HashMap::new()),
        usage_generation: watch::channel(0u64).0,
        registry: crate::session_registry::InMemoryRegistry::shared(),
        helper_ext_subscribers: Mutex::new(HashMap::new()),
        wt,
        agents: Mutex::new(HashMap::new()),
        custom_model_generations: Mutex::new(HashMap::new()),
        default_agent_cmd: config.agent.clone(),
        default_agent_id: config.agent_id.clone(),
        allowed_agent_ids,
        helper_meta: Mutex::new(HashMap::new()),
        tab_ownership_gate: Mutex::new(()),
        connected_helpers: Mutex::new(HashSet::new()),
        all_retirement_fence: Mutex::new(AllRetirementFence::default()),
        tab_retirement_fences: Mutex::new(HashMap::new()),
        tab_retirement_rekeys: Mutex::new(HashMap::new()),
        unresolved_owner_retirements: Mutex::new(HashMap::new()),
        pending_session_helpers: Mutex::new(HashMap::new()),
        pending_session_mcp: Mutex::new(HashMap::new()),
        closing_session_helpers: Mutex::new(HashSet::new()),
        destructive_session_helpers: Mutex::new(HashSet::new()),
        active_retirement_helpers: Mutex::new(HashSet::new()),
        closing_session_results: Mutex::new(HashMap::new()),
        session_transaction_changed: tokio::sync::Notify::new(),
        retirement_operations: Mutex::new(HashMap::new()),
        #[cfg(test)]
        retirement_completion_tx: Mutex::new(None),
        #[cfg(test)]
        retirement_pending_timeout: SESSION_CLOSE_TIMEOUT,
        #[cfg(test)]
        disconnect_orphan_publication_pause: Mutex::new(None),
        #[cfg(test)]
        reap_agent_orphan_cleanup_pause: Mutex::new(None),
        #[cfg(test)]
        deferred_retirement_cleanup_complete: tokio::sync::Notify::new(),
        hook_owned: Mutex::new(HashSet::new()),
        born_bound: Mutex::new(HashSet::new()),
        orphaned_sessions: Mutex::new(HashMap::new()),
        retired_agent_cells: std::sync::Mutex::new(Vec::new()),
        orphaned_tabs: Mutex::new(HashMap::new()),
    });
    {
        let session_mcp_state = Arc::clone(&inner);
        tokio::task::spawn_local(async move {
            if let Err(error) = session_mcp::run(session_mcp_listener, session_mcp_state).await {
                tracing::error!(
                    target: "session_mcp",
                    error = %format!("{error:#}"),
                    "master session MCP HTTP endpoint stopped"
                );
            }
        });
    }

    // ── Hookless Class-B session watcher ──────────────────────────────
    // A blocking `notify` watcher runs on its own OS thread; a bridge thread
    // forwards emitted events into this LocalSet via a tokio channel, where
    // they're applied to master's registry (same reducer as session_hook).
    {
        let (sync_tx, sync_rx) = std::sync::mpsc::channel::<crate::session_watcher::Emitted>();
        if let Err(err) = std::thread::Builder::new()
            .name("wta-session-watch".into())
            .spawn(move || {
                if let Err(err) = crate::session_watcher::watch(sync_tx) {
                    tracing::warn!(target: "session_watcher", error = %err, "watcher exited");
                }
            })
        {
            tracing::warn!(
                target: "session_watcher",
                error = %err,
                "failed to spawn session-watch thread; hookless fallback disabled"
            );
        }

        let (async_tx, mut async_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::session_watcher::Emitted>();
        if let Err(err) = std::thread::Builder::new()
            .name("wta-session-watch-bridge".into())
            .spawn(move || {
                for emitted in sync_rx {
                    if async_tx.send(emitted).is_err() {
                        break;
                    }
                }
            })
        {
            tracing::warn!(
                target: "session_watcher",
                error = %err,
                "failed to spawn session-watch bridge thread; watcher events will not reach master"
            );
        }

        let inner_for_watch = Arc::clone(&inner);
        tokio::task::spawn_local(async move {
            while let Some(emitted) = async_rx.recv().await {
                apply_watcher_event(&inner_for_watch, emitted).await;
            }
        });
    }

    // WT event subscriber: drive PaneClosed / ConnectionFailed into the
    // master registry directly off WT's `connection_state` events. This
    // is the fallback for cases where no helper publishes the event —
    // see the `wt_cli` setup above for the Gemini hard-close motivation.
    if let Some(mut rx) = wt_event_rx {
        let inner_for_wt = Arc::clone(&inner);
        tokio::task::spawn_local(async move {
            tracing::info!(
                target: "master_wt_event",
                "master WT event subscriber task started"
            );
            while let Some(event_json) = rx.recv().await {
                handle_master_wt_event(&inner_for_wt, event_json).await;
            }
            tracing::warn!(
                target: "master_wt_event",
                "master WT event subscriber channel closed"
            );
        });
    }

    // Open the named pipe and accept helper connections. Agent CLIs are
    // spawned lazily per-helper (see `get_or_spawn_agent`), and an
    // individual agent CLI dying is handled per-CLI by its reaper
    // (`spawn_one_agent`) — it removes that agent from the pool but the
    // master stays alive so sibling tabs on OTHER agents keep working.
    // Only a fatal pipe error returns from this loop. SharedWta on the
    // C++ side still owns the master's process lifetime (job object +
    // pane refcount).
    // Tighten the pipe ACL (defense in depth — see
    // `build_pipe_security_attributes`). Held for the whole accept loop so
    // every follow-up instance inherits the same attributes; `None` means
    // we couldn't build it and fall back to the default ACL.
    let pipe_security = build_pipe_security_attributes();
    if pipe_security.is_none() {
        tracing::warn!(
            target: "master",
            "named pipe uses default ACL (hardened SD unavailable)"
        );
    }
    let mut server = create_master_pipe_instance(&pipe_name, true, pipe_security.as_ref())
        .with_context(|| format!("failed to create named pipe '{pipe_name}'"))?;
    tracing::info!(
        target: "master",
        pipe_name = %pipe_name,
        secured = pipe_security.is_some(),
        "named pipe listening; awaiting helper connections"
    );
    let _pipe_discovery_guard = MasterPipeDiscoveryGuard::write(&pipe_name);

    let mut next_helper_id: u64 = 1;
    // Cheap monotonic counter for tracking concurrent helper count.
    // Both connect and disconnect log it, so a single grep on
    // "live_helpers=" reconstructs the timeline.
    let live_helpers = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    loop {
        server
            .connect()
            .await
            .with_context(|| format!("named pipe connect on '{pipe_name}'"))?;

        let helper_id = HelperId(next_helper_id);
        next_helper_id = next_helper_id.wrapping_add(1);
        let live = live_helpers.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        tracing::info!(
            target: "master",
            helper_id = ?helper_id,
            live_helpers = live,
            "helper pipe connected, dispatching to serve_helper"
        );

        // Replace the connected instance with a fresh one so the next
        // helper can connect concurrently.
        let connected = std::mem::replace(
            &mut server,
            create_master_pipe_instance(&pipe_name, false, pipe_security.as_ref()).with_context(
                || format!("failed to create follow-up pipe instance for '{pipe_name}'"),
            )?,
        );

        let inner = Arc::clone(&inner);
        let live_helpers = Arc::clone(&live_helpers);
        tokio::task::spawn_local(async move {
            let result = serve_helper(helper_id, connected, inner).await;
            let live = live_helpers.fetch_sub(1, std::sync::atomic::Ordering::SeqCst) - 1;
            match result {
                Err(err) => tracing::warn!(
                    target: "master",
                    helper_id = ?helper_id,
                    live_helpers = live,
                    error = %err,
                    "helper connection task exited with error"
                ),
                Ok(()) => tracing::info!(
                    target: "master",
                    helper_id = ?helper_id,
                    live_helpers = live,
                    "helper connection task exited cleanly"
                ),
            }
        });
    }
}

/// Normalize the host-supplied `--allowed-agent-ids` argv into the
/// allowlist [`resolve_agent_selection`] consumes, keying the result on
/// whether the host supplied the flag **at all**:
///
/// * **Flag absent** (clap produced an empty argv) ⇒ `None`: "no host
///   policy" — manual runs / older hosts. [`resolve_agent_selection`]
///   then accepts any *known* agent id.
/// * **Flag present** (any argv, even `--allowed-agent-ids ""`) ⇒
///   `Some(set)`: the host expressed a policy, so honor it **fail-closed**.
///   Each entry is trimmed + lowercased; blanks and unknown/custom ids are
///   dropped (the allowlist is "known ids only" — [`resolve_agent_selection`]
///   additionally requires [`agent_registry::is_known_id`], so keeping inert
///   entries would just mislead policy debugging). The surviving set may be
///   **empty**, which blocks every helper-selected id (all tabs fall back to
///   the trusted default) — *not* a silent widening back to "accept any
///   known id".
///
/// Distinguishing absence from a present-but-empty value matters because the
/// safe default for a policy boundary is fail-closed: a host that supplies an
/// empty/all-filtered list (e.g. GPO filtered every built-in agent out) should
/// block, not implicitly allow. This is reached in real launches: when an
/// `AllowedAgents` policy filters the built-in ACP set to empty, Terminal
/// (`TerminalPage::_BuildSharedWtaExtraArgs`) intentionally emits the combined
/// token `--allowed-agent-ids=` (clap parses it to `[""]`) so the master stays
/// fail-closed instead of reading an absent flag as "no policy". It is also
/// reachable from an explicit manual invocation. (Terminal sends the value
/// attached via `=` rather than as its own argv token because the command-line
/// builder drops empty args.)
fn normalize_allowed_agent_ids(raw: &[String]) -> Option<std::collections::HashSet<String>> {
    // Flag entirely absent ⇒ no host policy. (clap's `Vec<String>` is empty
    // when `--allowed-agent-ids` was not passed; `--allowed-agent-ids ""`
    // instead yields `[""]`, a non-empty argv, which is treated as "present".)
    if raw.is_empty() {
        return None;
    }
    let set: std::collections::HashSet<String> = raw
        .iter()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .filter(|s| crate::agent_registry::is_known_id(s))
        .collect();
    // The flag WAS supplied — return `Some` even when the set is empty, so the
    // policy is honored fail-closed (block all) rather than collapsing back to
    // the no-policy `None`.
    Some(set)
}

#[derive(Clone, Copy)]
enum HelperInitializeFailure {
    ProviderResolution,
    AgentStartup,
}

fn helper_initialize_error(
    failure: HelperInitializeFailure,
    _internal_error: &anyhow::Error,
) -> acp::Error {
    let detail = match failure {
        HelperInitializeFailure::ProviderResolution => {
            "Unable to load the selected model provider. Review it in Settings and try again."
        }
        HelperInitializeFailure::AgentStartup => {
            "Unable to start the selected AI agent. Verify the agent installation and model provider settings, then try again."
        }
    };
    acp::Error::internal_error().data(serde_json::json!(detail))
}

/// Decide which agent command the master will spawn for a helper, given
/// what the helper declared in `_meta.wta` and the master's trusted
/// defaults / GPO allowlist.
///
/// **Security invariant:** the returned command is always master-derived
/// — either reconstructed from a *known, allowed* agent id via
/// [`agent_registry::build_acp_command`], or the trusted `--agent`
/// default. A command string arriving over the pipe (`wta_meta.agent_cmd`)
/// is never returned and never executed; any same-user process that
/// connects to the pipe therefore cannot drive arbitrary process
/// creation by choosing the command line — only by selecting among the
/// host-approved agent ids.
///
/// The returned id is passed on to `spawn_one_agent` so the per-session
/// `cli_source` is stamped correctly; `None` lets it be inferred from the
/// command line. The explicit-selection status keeps rejected helper metadata
/// from influencing provider resolution after fallback.
///
/// Fallback to the default happens when the helper declared no id, an
/// *unknown* id (not in [`agent_registry::KNOWN_AGENTS`] — e.g. a
/// `custom:` agent, which the global default already covers), or an id
/// the host's GPO allowlist excludes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExplicitAgentSelection {
    ImplicitDefault,
    Accepted,
    Rejected,
}

struct ResolvedAgentSelection {
    command: String,
    agent_id: Option<String>,
    source: crate::agent_source::AgentSource,
    explicit_selection: ExplicitAgentSelection,
}

fn resolve_agent_selection(
    default_cmd: &str,
    default_id: Option<&str>,
    allowed_ids: Option<&std::collections::HashSet<String>>,
    requested_id: Option<&str>,
    requested_model: Option<&str>,
    requested_source: Option<&str>,
    requested_wsl_distro: Option<&str>,
    helper_id: HelperId,
) -> ResolvedAgentSelection {
    let requested = requested_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase);

    if let Some(id) = requested.as_deref() {
        // Membership test against KNOWN_AGENTS — NOT a comparison against
        // DEFAULT_PROFILE.id, which would treat the default agent as
        // "unknown" (and drop model folding) the day the default profile's
        // id becomes a real, selectable agent id.
        let known = crate::agent_registry::is_known_id(id);
        // `None` allowlist = no host policy supplied (manual run / older
        // host) → trust any known id. `Some(set)` = honor only listed ids.
        let allowed = allowed_ids.map_or(true, |set| set.contains(id));

        if known && allowed {
            let model = requested_model.map(str::trim).filter(|s| !s.is_empty());
            let launch_model =
                model.filter(|_| !crate::agent_registry::supports_live_model_switch(id));
            let cmd = crate::agent_registry::build_acp_command(id, launch_model);
            let source =
                crate::agent_source::AgentSource::from_wire(requested_source, requested_wsl_distro);
            return ResolvedAgentSelection {
                command: cmd,
                agent_id: Some(id.to_string()),
                source,
                explicit_selection: ExplicitAgentSelection::Accepted,
            };
        }

        // A real selection we refused — surface why, then fall back.
        tracing::warn!(
            target: "master",
            helper_id = ?helper_id,
            requested_agent_id = %id,
            known,
            allowed,
            "helper requested an unknown or GPO-blocked agent id; \
             falling back to the trusted default agent"
        );
    }

    ResolvedAgentSelection {
        command: default_cmd.to_string(),
        agent_id: default_id.map(str::to_string),
        source: crate::agent_source::AgentSource::Host,
        explicit_selection: if requested.is_some() {
            ExplicitAgentSelection::Rejected
        } else {
            ExplicitAgentSelection::ImplicitDefault
        },
    }
}

async fn resolve_provider_binding(
    state: &MasterStateInner,
    agent_id: Option<&str>,
    source: &crate::agent_source::AgentSource,
    requested_binding: Option<&str>,
    explicit_selection: ExplicitAgentSelection,
) -> Result<ProviderBinding> {
    if explicit_selection == ExplicitAgentSelection::Rejected {
        return Ok(ProviderBinding::Native);
    }

    let Some(requested_binding) = requested_binding
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(ProviderBinding::LegacyEnvironment);
    };
    if !matches!(source, crate::agent_source::AgentSource::Host)
        || requested_binding.eq_ignore_ascii_case("default")
    {
        return Ok(ProviderBinding::Native);
    }

    let agent_id = agent_id.context("custom model binding requires a known agent")?;
    if crate::agent_registry::lookup_profile_by_id(agent_id).byok_mode
        == crate::agent_registry::ByokMode::Unsupported
    {
        tracing::warn!(
            target: "master",
            agent_id,
            "ignoring custom model binding for an agent without BYOK support"
        );
        return Ok(ProviderBinding::Native);
    }

    let wt = state
        .wt
        .as_ref()
        .context("Terminal settings are unavailable for custom model resolution")?;
    let settings = wt
        .request("get_settings", serde_json::Value::Null)
        .await
        .context("failed to read Terminal settings for custom model resolution")?;
    let config = crate::custom_model_provider::Config::from_settings(&settings, requested_binding)?;

    let mut generations = state.custom_model_generations.lock().await;
    let generation =
        update_custom_model_generation(&mut generations, requested_binding, config.clone())?;

    Ok(ProviderBinding::Custom {
        selection_id: requested_binding.to_string(),
        generation,
        config,
    })
}

fn update_custom_model_generation(
    generations: &mut HashMap<String, CustomModelGeneration>,
    selection_id: &str,
    config: crate::custom_model_provider::Config,
) -> Result<u64> {
    let generation = match generations.get_mut(selection_id) {
        Some(current) if current.config == config => current.generation,
        Some(current) => {
            current.generation = current
                .generation
                .checked_add(1)
                .context("custom model provider generation exhausted")?;
            current.config = config.clone();
            current.generation
        }
        None => {
            generations.insert(
                selection_id.to_string(),
                CustomModelGeneration {
                    config,
                    generation: 1,
                },
            );
            1
        }
    };
    Ok(generation)
}

fn requested_model_is_explicit(agent_cmd: &str, agent_id: Option<&str>) -> bool {
    let resolved_agent_id =
        agent_id.unwrap_or_else(|| crate::agent_registry::resolve_agent_id_from_cmd(agent_cmd));
    if !crate::agent_registry::is_known_id(resolved_agent_id)
        || crate::agent_registry::supports_live_model_switch(resolved_agent_id)
    {
        return false;
    }
    let profile = crate::agent_registry::lookup_profile_by_id(resolved_agent_id);
    let tokens = crate::coordinator::split_windows_commandline(agent_cmd);
    let args: Vec<&str> = tokens.iter().skip(1).map(String::as_str).collect();
    crate::agent_registry::extract_model_from_args(&args, profile).is_some()
}

/// Get the agent CLI for `agent_cmd`, spawning + initializing it on
/// first use and reusing it thereafter. Two helpers racing the same
/// new agent serialize on the per-key `OnceCell`; helpers for different
/// agents spawn in parallel because the outer map lock is held only
/// long enough to get/insert the cell, never across the spawn.
#[derive(Debug)]
struct RetiredAgentCell;

impl std::fmt::Display for RetiredAgentCell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("agent-pool generation was retired")
    }
}

impl std::error::Error for RetiredAgentCell {}

fn retired_agent_cells(state: &MasterStateInner) -> std::sync::MutexGuard<'_, Vec<WeakAgentCell>> {
    state
        .retired_agent_cells
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn agent_cell_is_retired(state: &MasterStateInner, cell: &AgentCell) -> bool {
    let mut retired = retired_agent_cells(state);
    retired.retain(|candidate| candidate.strong_count() > 0);
    let cell = Arc::downgrade(cell);
    retired
        .iter()
        .any(|candidate| Weak::ptr_eq(candidate, &cell))
}

fn retire_agent_cell(state: &MasterStateInner, cell: &AgentCell) {
    let mut retired = retired_agent_cells(state);
    retired.retain(|candidate| candidate.strong_count() > 0);
    let cell = Arc::downgrade(cell);
    if !retired
        .iter()
        .any(|candidate| Weak::ptr_eq(candidate, &cell))
    {
        retired.push(cell);
    }
}

struct AgentInitializationRetirement<'a> {
    state: &'a MasterStateInner,
    cell: AgentCell,
    armed: bool,
}

impl<'a> AgentInitializationRetirement<'a> {
    fn new(state: &'a MasterStateInner, cell: AgentCell) -> Self {
        Self {
            state,
            cell,
            armed: true,
        }
    }

    fn retire(&mut self) {
        if self.armed {
            retire_agent_cell(self.state, &self.cell);
            self.armed = false;
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for AgentInitializationRetirement<'_> {
    fn drop(&mut self) {
        self.retire();
    }
}

async fn agent_cell_is_current(
    state: &MasterStateInner,
    key: &AgentCmdKey,
    cell: &AgentCell,
) -> bool {
    !agent_cell_is_retired(state, cell)
        && state
            .agents
            .lock()
            .await
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, cell))
}

async fn remove_agent_cell_if_current(
    state: &MasterStateInner,
    key: &AgentCmdKey,
    cell: &AgentCell,
) -> bool {
    let mut agents = state.agents.lock().await;
    if agents
        .get(key)
        .is_some_and(|current| Arc::ptr_eq(current, cell))
    {
        agents.remove(key);
        true
    } else {
        false
    }
}

async fn acquire_agent_from_pool<F, Fut>(
    state: &MasterStateInner,
    key: &AgentCmdKey,
    mut initialize: F,
) -> Result<Arc<AgentCli>>
where
    F: FnMut(AgentCell) -> Fut,
    Fut: Future<Output = Result<Arc<AgentCli>>>,
{
    loop {
        let cell = {
            let mut agents = state.agents.lock().await;
            if agents
                .get(key)
                .is_some_and(|cell| agent_cell_is_retired(state, cell))
            {
                agents.remove(key);
            }
            Arc::clone(
                agents
                    .entry(key.clone())
                    .or_insert_with(|| Arc::new(tokio::sync::OnceCell::new())),
            )
        };

        let initialized = cell
            .get_or_try_init(|| async {
                // A failed initializer retires its cell before waking waiters.
                // Waiters that captured that cell must join the replacement
                // generation instead of starting another process in the stale
                // cell where the old process's reaper can still reach it.
                if !agent_cell_is_current(state, key, &cell).await {
                    return Err(anyhow!(RetiredAgentCell));
                }

                let mut retirement = AgentInitializationRetirement::new(state, Arc::clone(&cell));
                match initialize(Arc::clone(&cell)).await {
                    Ok(agent) => {
                        retirement.disarm();
                        Ok(agent)
                    }
                    Err(error) => {
                        retirement.retire();
                        remove_agent_cell_if_current(state, key, &cell).await;
                        Err(error)
                    }
                }
            })
            .await;

        match initialized {
            Ok(agent) if agent_cell_is_current(state, key, &cell).await => {
                return Ok(Arc::clone(agent));
            }
            Ok(agent) => {
                // The process initialized after its generation was retired.
                // Do not leave an untracked provider running outside the pool.
                agent.conn.shutdown();
            }
            Err(error) if error.is::<RetiredAgentCell>() => {}
            Err(error) => return Err(error),
        }
    }
}

async fn get_or_spawn_agent(
    state: &Arc<MasterStateInner>,
    agent_cmd: &str,
    agent_id: Option<&str>,
    source: &crate::agent_source::AgentSource,
    provider_binding: ProviderBinding,
    supplied_cloud_models: Vec<crate::app::AcpModelInfo>,
) -> Result<Arc<AgentCli>> {
    let key = agent_cmd_key_with_provider(agent_cmd, agent_id, source, &provider_binding);
    let initialization_key = key.clone();
    acquire_agent_from_pool(state, &key, move |cell| {
        let key = initialization_key.clone();
        let provider_binding = provider_binding.clone();
        let supplied_cloud_models = supplied_cloud_models.clone();
        async move {
            spawn_one_agent(
                state,
                &cell,
                &key,
                agent_cmd,
                agent_id,
                source,
                &provider_binding,
                supplied_cloud_models,
            )
            .await
        }
    })
    .await
}

/// Spawn one agent CLI subprocess, wire master as its ACP client, run
/// the startup `initialize` round trip, and install per-CLI reapers.
/// Unlike the old single-agent master, an agent CLI death here only
/// removes that agent from the pool — the master process survives so
/// other tabs' agents keep running.
/// How many captured stderr lines to fold into a startup-failure error.
/// Bounded so a chatty CLI can't turn a pane's error banner into a wall of
/// text; the full capture is always in the log.
const STARTUP_STDERR_IN_ERROR: usize = 4;

/// Name the agent in a startup-failure message, including WHERE it runs.
///
/// The command alone is ambiguous: `copilot --acp --stdio` is the spelling for
/// the host CLI *and* for the CLI inside every WSL distro, so a bare command
/// sends the user debugging the wrong machine.
fn describe_agent_target(agent_cmd: &str, source: &crate::agent_source::AgentSource) -> String {
    match source {
        crate::agent_source::AgentSource::Host => format!("'{agent_cmd}'"),
        crate::agent_source::AgentSource::Wsl { distro } => {
            format!("'{agent_cmd}' (WSL {distro})")
        }
    }
}

/// Fold captured startup stderr into an error message, or return empty when
/// the CLI died silently.
///
/// This is the difference between a user seeing the transport symptom
/// ("response to `initialize` never received: oneshot canceled") and the
/// actual cause ("cannot preserve mount namespace ... Invalid argument").
fn format_startup_stderr(lines: &[String]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let shown = lines.len().min(STARTUP_STDERR_IN_ERROR);
    let mut out = String::new();
    for line in &lines[lines.len() - shown..] {
        out.push_str("\n  agent stderr: ");
        out.push_str(line);
    }
    if lines.len() > shown {
        out.push_str(&format!(
            "\n  ({} earlier stderr line(s) in the master log)",
            lines.len() - shown
        ));
    }
    out
}

async fn spawn_one_agent(
    state: &Arc<MasterStateInner>,
    cell: &AgentCell,
    key: &AgentCmdKey,
    agent_cmd: &str,
    agent_id: Option<&str>,
    source: &crate::agent_source::AgentSource,
    provider_binding: &ProviderBinding,
    supplied_cloud_models: Vec<crate::app::AcpModelInfo>,
) -> Result<Arc<AgentCli>> {
    let cold_start_started = std::time::Instant::now();
    let instance_id = AgentInstanceId::new_v4();
    let resolved_agent_id = agent_id
        .map(str::to_string)
        .unwrap_or_else(|| crate::agent_registry::resolve_agent_id_from_cmd(agent_cmd).to_string());
    let source_kind = match source {
        crate::agent_source::AgentSource::Host => "Host",
        crate::agent_source::AgentSource::Wsl { .. } => "Wsl",
    };
    let mut spawn_result = match spawn_agent_process_for_source_with_provider(
        agent_cmd,
        None,
        agent_id,
        source,
        ChildEnvironmentPolicy::ApplySharedProvider,
        provider_binding.spawn_selection(),
    ) {
        Ok(result) => result,
        Err(error) => {
            crate::telemetry::log_agent_cold_start_complete(
                &resolved_agent_id,
                source_kind,
                cold_start_started.elapsed().as_secs_f64() * 1000.0,
                false,
                "SpawnFailed",
            );
            return Err(error).with_context(|| format!("failed to spawn agent CLI: {agent_cmd}"));
        }
    };
    tracing::info!(
        target: "master",
        program = %spawn_result.resolved_program,
        agent_cmd = %agent_cmd,
        agent_source = %source,
        "agent CLI spawned"
    );

    let stdin = spawn_result
        .child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("agent CLI child has no stdin"))?;
    let stdout = spawn_result
        .child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("agent CLI child has no stdout"))?;
    let is_npx = spawn_result.is_npx;

    // Routine stderr stays at debug because it may contain prompt / file
    // content. Pre-initialize stderr is buffered and promoted to warning only
    // if startup fails, so release logs retain package-manager diagnostics.
    let stderr_log = AgentStderrLog::new(key.to_string());
    let stderr_task = spawn_result
        .child
        .stderr
        .take()
        .map(|stderr| stderr_log.drain(stderr));

    let client = MasterClient {
        state: Arc::clone(state),
    };
    let builder = acp::Client
        .builder()
        .name("wta-master")
        .on_receive_request(
            {
                let client = client.clone();
                move |req: acp::schema::v1::AgentRequest, responder, _cx| {
                    let client = client.clone();
                    async move {
                        use acp::schema::v1::{AgentRequest as Q, ClientResponse as R};
                        match req {
                            Q::RequestPermissionRequest(args) => conn::respond_enum(
                                responder,
                                client
                                    .request_permission(args)
                                    .await
                                    .map(R::RequestPermissionResponse),
                            ),
                            Q::WriteTextFileRequest(args) => conn::respond_enum(
                                responder,
                                client
                                    .write_text_file(args)
                                    .await
                                    .map(R::WriteTextFileResponse),
                            ),
                            Q::ReadTextFileRequest(args) => conn::respond_enum(
                                responder,
                                client
                                    .read_text_file(args)
                                    .await
                                    .map(R::ReadTextFileResponse),
                            ),
                            Q::CreateTerminalRequest(args) => conn::respond_enum(
                                responder,
                                client
                                    .create_terminal(args)
                                    .await
                                    .map(R::CreateTerminalResponse),
                            ),
                            Q::TerminalOutputRequest(args) => conn::respond_enum(
                                responder,
                                client
                                    .terminal_output(args)
                                    .await
                                    .map(R::TerminalOutputResponse),
                            ),
                            Q::ReleaseTerminalRequest(args) => conn::respond_enum(
                                responder,
                                client
                                    .release_terminal(args)
                                    .await
                                    .map(R::ReleaseTerminalResponse),
                            ),
                            Q::WaitForTerminalExitRequest(args) => conn::respond_enum(
                                responder,
                                client
                                    .wait_for_terminal_exit(args)
                                    .await
                                    .map(R::WaitForTerminalExitResponse),
                            ),
                            Q::KillTerminalRequest(args) => conn::respond_enum(
                                responder,
                                client
                                    .kill_terminal(args)
                                    .await
                                    .map(R::KillTerminalResponse),
                            ),
                            _ => responder.respond_with_error(acp::Error::method_not_found()),
                        }
                    }
                }
            },
            acp::on_receive_request!(),
        )
        .on_receive_notification(
            {
                let client = client.clone();
                move |notif: acp::schema::v1::AgentNotification, _cx| {
                    let client = client.clone();
                    async move {
                        if let acp::schema::v1::AgentNotification::SessionNotification(notif) =
                            notif
                        {
                            let _ = client.session_notification(notif).await;
                        }
                        Ok(())
                    }
                }
            },
            acp::on_receive_notification!(),
        );
    let (conn, handle_io) = conn::spawn_client(
        builder,
        conn::byte_streams(stdin.compat_write(), stdout.compat()),
    );

    // I/O-loop driver + reaper. This task drives the ACP connection's
    // I/O, so it MUST run before `initialize` (below) — initialize can't
    // make progress otherwise. When the loop ends (clean shutdown, pipe
    // error, or because we killed the child on an init failure) master can
    // no longer talk to this CLI, so the agent is dropped from the pool.
    // On the init-failure path that removes the empty `OnceCell` entry so
    // the next helper retries cleanly instead of reusing a dead slot.
    {
        let state = Arc::clone(state);
        let cell = Arc::clone(cell);
        let key = key.clone();
        tokio::task::spawn_local(async move {
            match handle_io.await {
                Ok(()) => tracing::info!(
                    target: "master",
                    agent = %key,
                    "agent CLI ACP I/O loop ended cleanly — removing from pool"
                ),
                Err(e) => tracing::error!(
                    target: "master",
                    agent = %key,
                    error = %e,
                    "agent CLI ACP I/O loop ended with error — removing from pool"
                ),
            }
            reap_agent(&state, &key, &cell, instance_id).await;
        });
    }

    // Keep the child locally-owned ACROSS `initialize`. The child reaper
    // (which moves `child`) is installed only AFTER init succeeds. If init
    // fails/times out we kill the child here and return `Err` without a
    // detached task left holding a live subprocess — previously the reaper
    // was spawned first, so a failed init leaked the agent process, its
    // I/O task, and (via the empty `OnceCell`) triggered repeated respawns.
    let mut child = spawn_result.child;

    // Initialize this CLI. npx adapter cold starts can be slow, so keep
    // the same generous timeout the single-agent master used.
    let init_timeout_secs = if is_npx { 60 } else { 15 };
    let init_outcome = tokio::time::timeout(
        std::time::Duration::from_secs(init_timeout_secs),
        conn.initialize(
            acp::schema::v1::InitializeRequest::new(acp::schema::ProtocolVersion::V1)
                .client_capabilities(acp::schema::v1::ClientCapabilities::new().terminal(true))
                .client_info(
                    acp::schema::v1::Implementation::new("wta-master", env!("CARGO_PKG_VERSION"))
                        .title("Windows Terminal Agent (master)"),
                ),
        ),
    )
    .await;

    let init_resp = match init_outcome {
        Ok(Ok(resp)) => {
            stderr_log.mark_initialized();
            resp
        }
        Ok(Err(e)) => {
            // Kill the child so its stdio closes → the I/O task above ends
            // → `reap_agent` clears the pool slot. `kill_on_drop` is a
            // backstop when `child` drops at return.
            let stderr = stderr_log
                .finish_failed_startup(&mut child, stderr_task)
                .await;
            crate::telemetry::log_agent_cold_start_complete(
                &resolved_agent_id,
                source_kind,
                cold_start_started.elapsed().as_secs_f64() * 1000.0,
                false,
                "InitializeFailed",
            );
            return Err(anyhow!(
                "ACP initialize failed for {}: {e}{}",
                describe_agent_target(agent_cmd, source),
                format_startup_stderr(&stderr)
            ));
        }
        Err(_) => {
            let stderr = stderr_log
                .finish_failed_startup(&mut child, stderr_task)
                .await;
            crate::telemetry::log_agent_cold_start_complete(
                &resolved_agent_id,
                source_kind,
                cold_start_started.elapsed().as_secs_f64() * 1000.0,
                false,
                "Timeout",
            );
            return Err(anyhow!(
                "ACP initialize timed out after {init_timeout_secs}s — agent CLI {} did not respond{}",
                describe_agent_target(agent_cmd, source),
                format_startup_stderr(&stderr)
            ));
        }
    };
    crate::telemetry::log_agent_cold_start_complete(
        &resolved_agent_id,
        source_kind,
        cold_start_started.elapsed().as_secs_f64() * 1000.0,
        true,
        "",
    );

    // Init succeeded — install the child reaper now (takes ownership of
    // `child`). A later CLI exit drops just this agent from the pool so
    // the next helper respawns it; the master stays up for other agents.
    {
        let state = Arc::clone(state);
        let cell = Arc::clone(cell);
        let key = key.clone();
        tokio::task::spawn_local(async move {
            let status = child.wait().await;
            tracing::error!(
                target: "master",
                agent = %key,
                ?status,
                "agent CLI exited — removing from pool (master stays up for other agents)"
            );
            reap_agent(&state, &key, &cell, instance_id).await;
        });
    }

    // Prefer the host-supplied agent id (authoritative); fall back to
    // parsing the command line. Stamps each session's `cli_source`.
    let cli_source = crate::agent_sessions::CliSource::from_agent_id(&resolved_agent_id);
    tracing::info!(
        target: "master",
        agent_cmd = %agent_cmd,
        resolved_agent_id = %resolved_agent_id,
        cli_source = ?cli_source,
        "agent CLI initialize OK; cli_source resolved"
    );

    let (cloud_catalog, start_clean_probe) = prepare_native_cloud_catalog(
        &resolved_agent_id,
        source,
        provider_binding,
        supplied_cloud_models,
    );
    let agent = Arc::new(AgentCli {
        instance_id,
        resolved_agent_id: resolved_agent_id.clone(),
        conn,
        cached_init_resp: init_resp,
        cli_source,
        source: source.clone(),
        cmd_key: key.clone(),
        cloud_catalog: Mutex::new(cloud_catalog),
        bound_helpers: Mutex::new(HashSet::new()),
        host_list_cache: Mutex::new(None),
        listed_ever: Mutex::new(HashSet::new()),
    });

    // Seed THIS CLI's history. Every agent entering the pool seeds, not just
    // the first: master outlives a Settings agent switch (the helper
    // reconnects and the pool spawns the new CLI without a master restart), so
    // gating this on "first agent wins" left the registry holding only the
    // launch agent's rows. The session view filters by the helper's current
    // CLI, so every switched-to agent then rendered an empty list until the
    // user restarted Terminal.
    {
        let state = Arc::clone(state);
        let agent = Arc::clone(&agent);
        tokio::task::spawn_local(async move {
            let count = seed_host_and_broadcast(&state, &agent).await;
            tracing::info!(
                target: "master_history",
                cli = ?agent.cli_source,
                count,
                "agent ACP history seed complete"
            );
        });
    }

    if start_clean_probe {
        let command = agent_cmd.to_string();
        start_clean_cloud_catalog_probe(
            Arc::clone(state),
            Arc::clone(&agent),
            resolved_agent_id,
            async move { crate::protocol::acp::probe::probe_models(&command).await },
        );
    }

    Ok(agent)
}

/// Remove a dead agent CLI from the pool. Helpers still holding an
/// `Arc<AgentCli>` for it will error on their next request (and the
/// pane gets rebuilt); a fresh helper requesting the same `agent_cmd`
/// re-runs `spawn_one_agent`. Sessions owned by the dead agent are left
/// for the owning helper's disconnect cleanup (`drop_sessions_for_helper`).
async fn reap_agent(
    state: &Arc<MasterStateInner>,
    key: &AgentCmdKey,
    cell: &AgentCell,
    instance_id: AgentInstanceId,
) {
    #[cfg(test)]
    let cleanup_pause = state.reap_agent_orphan_cleanup_pause.lock().await.clone();
    let removed = {
        let mut agents = state.agents.lock().await;
        let owns_generation = agents
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, cell));
        let owns_instance = cell
            .get()
            .is_none_or(|agent| agent.instance_id == instance_id);
        if owns_generation && owns_instance {
            agents.remove(key);
            #[cfg(test)]
            if let Some(pause) = cleanup_pause {
                pause.agent_removed.notify_one();
                pause.resume_cleanup.notified().await;
            }
            // Keep the key unavailable until all generation-owned state is
            // gone. Pool publication takes `agents` first, so the consistent
            // order is agents -> orphaned_tabs -> orphaned_sessions.
            state
                .orphaned_tabs
                .lock()
                .await
                .retain(|_, (orphan_key, _, _)| orphan_key != key);
            state.orphaned_sessions.lock().await.remove(key);
            true
        } else {
            false
        }
    };
    let capabilities_removed = state
        .session_mcp_capabilities
        .remove_owner(instance_id)
        .await;
    tracing::info!(
        target: "master",
        agent = %key,
        pool_entry_removed = removed,
        capabilities_removed,
        "dead agent reaped; replacement pool entry preserved when present"
    );
}

/// Per-helper-connection task. Wraps the named pipe in an
/// `AgentSideConnection`, runs both its I/O loop and a notification
/// forwarder until the helper disconnects.
async fn serve_helper(
    helper_id: HelperId,
    pipe: NamedPipeServer,
    state: Arc<MasterStateInner>,
) -> Result<()> {
    tracing::info!(target: "master", helper_id = ?helper_id, "helper connected");
    register_connected_helper(&state, helper_id).await;

    let (notif_tx, mut notif_rx) =
        mpsc::channel::<acp::schema::v1::SessionNotification>(NOTIF_CHANNEL_CAPACITY);
    let mut usage_generation_rx = state.usage_generation.subscribe();

    // Second channel: master-originated ExtNotifications fanned out by
    // `broadcast_ext_to_helpers`. Kept separate from `notif_tx` so the
    // per-session and live-set fan-out paths don't collide on the
    // wire-write loop below; the `tokio::select!` can dispatch each to
    // the appropriate `AgentSideConnection` method without an enum
    // discriminator at every write site.
    let (ext_tx, mut ext_rx) = mpsc::unbounded_channel::<acp::schema::v1::ExtNotification>();
    {
        let mut subs = state.helper_ext_subscribers.lock().await;
        subs.insert(helper_id, ext_tx);
    }

    // Shared with `HelperHandler` so it can stash the helper's
    // outbound `AgentSideConnection` into `HelperRoute.forwarder` at
    // `new_session` / `load_session` time. `OnceLock` because the
    // conn doesn't exist until `AgentSideConnection::new` returns,
    // but we populate it strictly before `handle_io` is polled below.
    //
    // Stored as `Weak` (not `Arc`) to avoid a reference cycle: the
    // conn owns the handler, the handler owns this slot — if the
    // slot held a strong `Arc` back to the conn, the conn could
    // never drop after helper disconnect.
    let agent_side_slot: Arc<OnceLock<conn::AgentLink>> = Arc::new(OnceLock::new());

    let handler = HelperHandler {
        helper_id,
        // Resolved lazily during this helper's `initialize` (see
        // HelperHandler::initialize → get_or_spawn_agent).
        agent: Arc::new(OnceCell::new()),
        state: Arc::clone(&state),
        replacement_gate: Arc::new(Mutex::new(())),
        notif_tx,
        agent_side_slot: Arc::clone(&agent_side_slot),
    };

    let (read_half, write_half) = tokio::io::split(pipe);
    let outgoing = write_half.compat_write();
    let incoming = read_half.compat();

    let builder = acp::Agent
        .builder()
        .name("wta-master-helper")
        .on_receive_request(
            {
                let h = handler.clone();
                move |req: conn::SetSessionModelRequest,
                      responder: acp::Responder<conn::SetSessionModelResponse>,
                      _cx| {
                    let h = h.clone();
                    async move {
                        match h.set_session_model(req).await {
                            Ok(response) => responder.respond(response),
                            Err(error) => responder.respond_with_error(error),
                        }
                    }
                }
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            {
                let h = handler.clone();
                move |req: acp::schema::v1::ClientRequest, responder, _cx| {
                    let h = h.clone();
                    async move {
                        use acp::schema::v1::{AgentResponse as R, ClientRequest as Q};
                        match req {
                            Q::InitializeRequest(a) => conn::respond_enum(
                                responder,
                                h.initialize(a).await.map(R::InitializeResponse),
                            ),
                            Q::AuthenticateRequest(a) => conn::respond_enum(
                                responder,
                                h.authenticate(a).await.map(R::AuthenticateResponse),
                            ),
                            Q::NewSessionRequest(a) => conn::respond_enum(
                                responder,
                                h.new_session(a).await.map(R::NewSessionResponse),
                            ),
                            Q::LoadSessionRequest(a) => conn::respond_enum(
                                responder,
                                h.load_session(a).await.map(R::LoadSessionResponse),
                            ),
                            Q::CloseSessionRequest(a) => conn::respond_enum(
                                responder,
                                h.close_session(a).await.map(R::CloseSessionResponse),
                            ),
                            Q::SetSessionModeRequest(a) => conn::respond_enum(
                                responder,
                                h.set_session_mode(a).await.map(R::SetSessionModeResponse),
                            ),
                            Q::SetSessionConfigOptionRequest(a) => conn::respond_enum(
                                responder,
                                h.set_session_config_option(a)
                                    .await
                                    .map(R::SetSessionConfigOptionResponse),
                            ),
                            Q::ListSessionsRequest(a) => conn::respond_enum(
                                responder,
                                h.list_sessions(a).await.map(R::ListSessionsResponse),
                            ),
                            Q::PromptRequest(a) => h.prompt(a, responder).await,
                            Q::ExtMethodRequest(a) => conn::respond_enum(
                                responder,
                                h.ext_method(a).await.map(R::ExtMethodResponse),
                            ),
                            _ => responder.respond_with_error(acp::Error::method_not_found()),
                        }
                    }
                }
            },
            acp::on_receive_request!(),
        )
        .on_receive_notification(
            {
                let h = handler.clone();
                move |notif: acp::schema::v1::ClientNotification, _cx| {
                    let h = h.clone();
                    async move {
                        if let acp::schema::v1::ClientNotification::CancelNotification(n) = notif {
                            let _ = h.cancel(n).await;
                        }
                        Ok(())
                    }
                }
            },
            acp::on_receive_notification!(),
        );

    let (agent_side_conn, handle_io) =
        conn::spawn_agent(builder, conn::byte_streams(outgoing, incoming));
    // Populate BEFORE the I/O loop drives any inbound request so handlers see a
    // ready forwarder. The link is cheap-Clone (`ConnectionTo` handle), so no
    // Arc/Weak cycle worry like the old object connection.
    let _ = agent_side_slot.set(agent_side_conn.clone());

    tokio::pin!(handle_io);
    let result = loop {
        tokio::select! {
            io_result = &mut handle_io => {
                break io_result.map_err(|e| anyhow!(e));
            }
            Some(notif) = notif_rx.recv() => {
                let sid = notif.session_id.clone();
                let kind = notification_kind(&notif);
                // Per-streamed-chunk; trace-only to keep default debug logs
                // readable (this line alone dominated the master log volume).
                tracing::trace!(
                    target: "master",
                    step = "master→helper",
                    op = "session_notification",
                    helper_id = ?helper_id,
                    session_id = ?sid,
                    kind = %kind,
                    "writing agent CLI notification to helper pipe"
                );
                if let Err(err) = agent_side_conn.session_notification(notif).await {
                    tracing::warn!(
                        target: "master",
                        helper_id = ?helper_id,
                        session_id = ?sid,
                        kind = %kind,
                        error = %err,
                        "forwarding session_notification to helper failed"
                    );
                }
            }
            changed = usage_generation_rx.changed() => {
                if changed.is_err() {
                    continue;
                }
                let pending = {
                    let mut pending = state.pending_usage.lock().await;
                    let session_ids = pending
                        .iter()
                        .filter_map(|(session_id, (owner, _))| (*owner == helper_id).then(|| session_id.clone()))
                        .collect::<Vec<_>>();
                    session_ids
                        .into_iter()
                        .filter_map(|session_id| pending.remove(&session_id).map(|(_, notification)| notification))
                        .collect::<Vec<_>>()
                };
                for notification in pending {
                    let session_id = notification.session_id.clone();
                    if let Err(error) = agent_side_conn.session_notification(notification).await {
                        tracing::warn!(
                            target: "master",
                            helper_id = ?helper_id,
                            session_id = ?session_id,
                            error = %error,
                            "forwarding coalesced usage update to helper failed"
                        );
                    }
                }
            }
            Some(ext) = ext_rx.recv() => {
                let method = ext.method.clone();
                tracing::debug!(
                    target: "master",
                    step = "master→helper",
                    op = "ext_notification",
                    helper_id = ?helper_id,
                    method = %method,
                    "writing live-set ext-notification to helper pipe"
                );
                if let Err(err) = agent_side_conn.ext_notification(ext).await {
                    tracing::warn!(
                        target: "master",
                        helper_id = ?helper_id,
                        method = %method,
                        error = %err,
                        "forwarding ext_notification to helper failed"
                    );
                }
            }
            else => {
                break Ok(());
            }
        }
    };

    // Unregister BEFORE dropping sessions: prevents a race where
    // `drop_sessions_for_helper` would broadcast `session_removed`
    // to ourselves (harmless but pointless, and our `ext_rx` is
    // already gone). After this point peers fan-out skips us.
    {
        let mut subs = state.helper_ext_subscribers.lock().await;
        subs.remove(&helper_id);
    }
    if let Some(agent) = handler.agent.get() {
        agent.bound_helpers.lock().await.remove(&helper_id);
    }

    let cleanup = cleanup_disconnected_helper(&handler).await;
    if let Some(agent) = handler.agent.get() {
        retire_unbound_model_agent(&state, agent).await;
    }

    tracing::info!(
        target: "master",
        helper_id = ?helper_id,
        sessions_owned = cleanup.sessions_owned,
        sessions_fallback_retired = cleanup.sessions_fallback_retired,
        intentional_close = cleanup.intentional_close,
        "helper ownership retired; automatic recovery disabled"
    );

    result
}

async fn retire_unbound_model_agent(state: &MasterStateInner, agent: &Arc<AgentCli>) {
    if !agent.cmd_key.starts_with("model:") {
        return;
    }

    let removed = {
        let mut agents = state.agents.lock().await;
        let matches_instance = agents
            .get(&agent.cmd_key)
            .and_then(|cell| cell.get())
            .is_some_and(|current| Arc::ptr_eq(current, agent));
        if !matches_instance || !agent.bound_helpers.lock().await.is_empty() {
            false
        } else {
            agents.remove(&agent.cmd_key).is_some()
        }
    };
    if removed {
        tracing::info!(
            target: "master",
            agent = %agent.cmd_key,
            "retiring unbound model-specific agent CLI"
        );
        agent.conn.shutdown();
    }
}

async fn bind_helper_to_agent(
    state: &MasterStateInner,
    agent: &Arc<AgentCli>,
    helper_id: HelperId,
) -> bool {
    let agents = state.agents.lock().await;
    let matches_instance = agents
        .get(&agent.cmd_key)
        .and_then(|cell| cell.get())
        .is_some_and(|current| Arc::ptr_eq(current, agent));
    if !matches_instance {
        return false;
    }
    agent.bound_helpers.lock().await.insert(helper_id);
    true
}

async fn acquire_and_bind_agent<F, Fut>(
    state: &MasterStateInner,
    helper_id: HelperId,
    mut acquire: F,
) -> Result<Arc<AgentCli>>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Arc<AgentCli>>>,
{
    loop {
        let agent = acquire().await?;
        if bind_helper_to_agent(state, &agent, helper_id).await {
            return Ok(agent);
        }
    }
}

struct DisconnectedHelperCleanup {
    sessions_owned: usize,
    sessions_fallback_retired: usize,
    intentional_close: bool,
}

async fn cleanup_disconnected_helper(handler: &HelperHandler) -> DisconnectedHelperCleanup {
    let state = &handler.state;
    let helper_id = handler.helper_id;

    // Fence the helper before waiting for an in-flight replacement. The
    // destructive tombstone keeps finish_failed_pending_session from clearing
    // the closing marker when that transaction fails. FIFO-queued replacements
    // therefore reject at owner publication before reaching the provider, and
    // holding the gate through cleanup prevents later state installation.
    let disconnect_added_closing_marker = {
        let _ownership_guard = state.tab_ownership_gate.lock().await;
        let added = state.closing_session_helpers.lock().await.insert(helper_id);
        state
            .destructive_session_helpers
            .lock()
            .await
            .insert(helper_id);
        added
    };
    let _replacement_guard = handler.replacement_gate.lock().await;

    // A disconnected helper is terminal: do not preserve or automatically
    // resume its live ACP sessions. Cancel and close each session while the
    // route still proves ownership, then retire any route whose provider-side
    // cleanup failed or is unsupported. Historical provider data remains
    // available for an explicit user-initiated session/load later.
    let owned_sessions = {
        let routes = state.session_to_helper.lock().await;
        routes
            .iter()
            .filter_map(|(session_id, route)| {
                (route.helper_id == helper_id).then(|| session_id.clone())
            })
            .collect::<Vec<_>>()
    };
    if let Some(agent) = handler.agent.get() {
        for session_id in &owned_sessions {
            if let Err(error) = close_and_retire_replaced_session(
                &state,
                helper_id,
                agent,
                session_id,
                SESSION_CLOSE_TIMEOUT,
            )
            .await
            {
                tracing::error!(
                    target: "master",
                    helper_id = ?helper_id,
                    session_id = %session_id,
                    error = %error,
                    "failed to close ACP session after helper disconnect; retiring local route"
                );
            }
        }
    }
    let fallback_retired = drop_sessions_for_helper(state, helper_id).await;

    let pending_mcp = state.pending_session_mcp.lock().await.remove(&helper_id);
    if let Some(pending_mcp) = pending_mcp {
        state.session_mcp_capabilities.cancel(&pending_mcp).await;
    }

    let (closing_marker_present, _) =
        consume_disconnected_helper_retirement_state(&state, helper_id).await;
    DisconnectedHelperCleanup {
        sessions_owned: owned_sessions.len(),
        sessions_fallback_retired: fallback_retired.len(),
        intentional_close: closing_marker_present && !disconnect_added_closing_marker,
    }
}

async fn consume_disconnected_helper_retirement_state(
    state: &MasterStateInner,
    helper_id: HelperId,
) -> (bool, Option<HelperRecoveryMeta>) {
    let _ownership_guard = state.tab_ownership_gate.lock().await;
    let pending_removed = state
        .pending_session_helpers
        .lock()
        .await
        .remove(&helper_id)
        .is_some();
    let intentional_close = state
        .closing_session_helpers
        .lock()
        .await
        .remove(&helper_id);
    state
        .destructive_session_helpers
        .lock()
        .await
        .remove(&helper_id);
    state
        .active_retirement_helpers
        .lock()
        .await
        .remove(&helper_id);
    state
        .closing_session_results
        .lock()
        .await
        .remove(&helper_id);
    state.connected_helpers.lock().await.remove(&helper_id);
    state
        .unresolved_owner_retirements
        .lock()
        .await
        .remove(&helper_id);
    state.tab_retirement_fences.lock().await.retain(|_, fence| {
        fence.outgoing_helpers.remove(&helper_id);
        fence.phase == TabRetirementPhase::Fencing || !fence.outgoing_helpers.is_empty()
    });
    let recovery = state.helper_meta.lock().await.remove(&helper_id);
    if pending_removed {
        state.session_transaction_changed.notify_waiters();
    }

    (intentional_close, recovery)
}

/// Remove every `session_to_helper` entry owned by `helper_id` and return
/// the dropped `SessionId`s (used for disconnect diagnostics). Factored out
/// of `serve_helper` so the cleanup is
/// unit-testable without a real named pipe.
async fn drop_sessions_for_helper(
    state: &MasterStateInner,
    helper_id: HelperId,
) -> Vec<acp::schema::v1::SessionId> {
    let candidates = {
        let map = state.session_to_helper.lock().await;
        map.iter()
            .filter_map(|(sid, route)| (route.helper_id == helper_id).then(|| sid.clone()))
            .collect::<Vec<_>>()
    };
    let mut victims = Vec::with_capacity(candidates.len());
    for session_id in candidates {
        let gate = session_lifecycle_gate(state, &session_id).await;
        let _guard = gate.lock().await;
        let removed = {
            let mut map = state.session_to_helper.lock().await;
            if map
                .get(&session_id)
                .is_some_and(|route| route.helper_id == helper_id)
            {
                map.remove(&session_id);
                true
            } else {
                false
            }
        };
        if !removed {
            continue;
        }
        state.pending_usage.lock().await.remove(&session_id);
        state
            .session_mcp_capabilities
            .remove_session(&session_id)
            .await;
        state.registry.remove(&session_id).await;
        // Broadcast removal so every still-attached helper drops the
        // row from its mirror. The disconnecting helper itself has
        // (almost always) already been removed from
        // `helper_ext_subscribers` by `serve_helper`'s cleanup path
        // before this is called, so the broadcast only reaches the
        // peers it should reach.
        broadcast_ext_to_helpers(
            state,
            crate::session_registry::build_session_removed_notification(&session_id),
        )
        .await;
        broadcast_ext_to_helpers(
            state,
            crate::session_registry::build_sessions_changed_notification(),
        )
        .await;
        victims.push(session_id);
    }
    victims
}

/// Fan an ACP `ExtNotification` out to every currently-attached helper.
///
/// Sends are non-blocking (`UnboundedSender::send` is a sync call that
/// returns immediately); any `SendError` here means the helper's
/// `serve_helper` loop has dropped its receiver, so we prune that
/// helper from the subscriber map. The loop is `O(N_helpers)` under a
/// single lock; we expect N to be tiny (one per WT window/agent pane)
/// so a lock-while-iterate is fine.
pub(crate) async fn broadcast_ext_to_helpers(
    state: &MasterStateInner,
    notification: acp::schema::v1::ExtNotification,
) {
    let mut subs = state.helper_ext_subscribers.lock().await;
    let mut dead: Vec<HelperId> = Vec::new();
    for (helper_id, tx) in subs.iter() {
        if let Err(err) = tx.send(notification.clone()) {
            tracing::warn!(
                target: "master",
                helper_id = ?helper_id,
                method = %notification.method,
                error = %err,
                "helper ext-notification channel closed; pruning subscriber"
            );
            dead.push(*helper_id);
        }
    }
    for helper_id in dead {
        subs.remove(&helper_id);
    }
}

/// Cached raw host `session/list`. `Some(sessions)` = the agent listed (possibly
/// empty); `None` = unsupported (Gemini / non-ACP custom), not connected yet, or
/// the call failed / timed out. Callers MUST treat `None` as "unknown", never as
/// "no sessions" — the reconcile skips it so a transient error can't wipe the
/// view. 2s TTL so the 5s poll, the title refresh, and a burst of hook events
/// share one round-trip.
async fn host_session_list_raw(
    agent: &AgentCli,
) -> Option<std::sync::Arc<[acp::schema::v1::SessionInfo]>> {
    if agent
        .cached_init_resp
        .agent_capabilities
        .session_capabilities
        .list
        .is_none()
    {
        return None;
    }

    const TTL: std::time::Duration = std::time::Duration::from_secs(2);
    {
        let cache = agent.host_list_cache.lock().await;
        if let Some((at, outcome)) = cache.as_ref() {
            if at.elapsed() < TTL {
                return outcome.clone();
            }
        }
    }

    // Captured before the await so the write-back can detect a result another
    // caller published while we were in-flight.
    let fetch_started = std::time::Instant::now();
    let outcome = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        agent
            .conn
            .list_sessions(acp::schema::v1::ListSessionsRequest::new()),
    )
    .await
    {
        Ok(Ok(resp)) => Some(resp.sessions.into()),
        Ok(Err(e)) => {
            tracing::debug!(
                target: "master_history",
                cli = ?agent.cli_source,
                "host session/list error: {e}"
            );
            None
        }
        Err(_) => {
            tracing::warn!(
                target: "master_history",
                cli = ?agent.cli_source,
                "host session/list timed out"
            );
            None
        }
    };
    // Single-flight write-back: if a concurrent caller already published a
    // result while we were awaiting `list_sessions`, adopt it instead of
    // clobbering — so a slow failure can't overwrite a fast success (or
    // vice-versa) and poison the 2 s cache with a transient None.
    let mut cache = agent.host_list_cache.lock().await;
    if let Some((at, cached)) = cache.as_ref() {
        if *at >= fetch_started {
            return cached.clone();
        }
    }
    *cache = Some((std::time::Instant::now(), outcome.clone()));
    outcome
}

/// The `CliSource` an agent's history rows are stamped with.
///
/// An agent we don't recognize (a `custom:<name>` provider) has
/// `cli_source: None`, but [`host_history_via_acp`] stamps its rows
/// `Unknown("custom")`. Reconcile authority and row-driven routing must
/// collapse `None` the same way, or an unrecognized agent can never match its
/// own rows: reconcile silently no-ops and title refresh never finds the
/// owning CLI. Every caller that compares an agent against a stamped row goes
/// through here so the two can't drift apart again.
///
/// This does bucket all custom providers together — with two pooled, either
/// may reconcile the other's rows. That is no worse than the pre-pool behavior
/// (one listing reconciled every row regardless of CLI) and stays bounded by
/// the host / Class-B / terminal gates in [`is_stale_host_history_row`].
fn stamped_cli(cli: Option<&crate::agent_sessions::CliSource>) -> crate::agent_sessions::CliSource {
    cli.cloned()
        .unwrap_or_else(|| crate::agent_sessions::CliSource::Unknown("custom".into()))
}

/// Host history from `agent`'s `session/list`, gated on the
/// `sessionCapabilities.list` capability. `None` when unsupported (Gemini,
/// non-ACP custom) / failed — distinct from `Some(vec![])` (listed, but empty),
/// which the reconcile needs to authoritatively drop stale rows. No on-disk
/// fallback by design.
///
/// Rows are stamped with **`agent`'s** CLI and execution source, never the
/// master's launch CLI or a blanket `Host`: an agent enumerates only its own
/// sessions, and master multiplexes several across host and WSL at once.
async fn host_history_via_acp(
    state: &MasterStateInner,
    agent: &AgentCli,
) -> Option<Vec<crate::agent_sessions::AgentSession>> {
    let sessions = host_session_list_raw(agent).await?;
    let cli = stamped_cli(agent.cli_source.as_ref());
    // Class-A (agent-pane) exclusion. The on-disk index is written by the helper
    // *after* session/new lands, so a just-created pane session can be returned by
    // session/list before its index line exists, leaking a phantom historical row.
    // Master routes every session/new, so its live `session_to_helper` keys are the
    // authoritative live-pane set — union them in to close that race.
    let mut idx = crate::agent_pane_origin::load_default_set();
    for sid in state.session_to_helper.lock().await.keys() {
        idx.insert(sid.0.to_string());
    }
    Some(crate::session_history::classify_and_map(
        &sessions,
        &idx,
        // Where this agent's sessions actually live. Blanket-stamping `Host`
        // collapsed host Copilot, Copilot in WSL Debian, and Copilot in WSL
        // Ubuntu into one indistinguishable set — they share a `CliSource`,
        // so `location` is the only thing that separates them. The live
        // `session/new` path already stamps the bound agent's source; this is
        // the historical path catching up.
        agent.source.session_location(),
        &cli,
    ))
}

/// Raw host `session/list` as session_id → title, UNFILTERED (includes Class-A
/// agent-pane rows, whose live registry entries still need synthetic-title
/// upgrades). Empty when `agent` can't list.
async fn host_titles_via_acp(agent: &AgentCli) -> std::collections::HashMap<String, String> {
    let Some(sessions) = host_session_list_raw(agent).await else {
        return std::collections::HashMap::new();
    };
    sessions
        .iter()
        .filter_map(|row| {
            row.title
                .clone()
                // Drop candidates that must never become a display name — most
                // notably the delegate's injected first-message echo, which an
                // agent CLI (e.g. Copilot) briefly reports as a session's
                // `session/list` title before it generates its real summary and
                // which embeds the `## Terminal Context (pane …)` block.
                // `refresh_titles_from_listing` applies the same predicate at
                // the point of mutation.
                .filter(|title| {
                    crate::session_registry::title_is_displayable(agent.cli_source.as_ref(), title)
                })
                .map(|title| (row.session_id.to_string(), title))
        })
        .collect()
}

/// Sync master's host-history rows for `agent` to its `session/list` (the
/// single source of truth for THAT CLI): add newly-listed sessions and drop
/// terminal Class-B host rows it no longer lists (phantoms, CLI-side deletes).
/// No-op when the agent can't list (unsupported / failed / timed out) so a
/// transient error never wipes the view. Returns `(changed, listed_count)`, or
/// `None` when the agent couldn't be listed.
async fn sync_host_history(state: &MasterStateInner, agent: &AgentCli) -> Option<(bool, usize)> {
    let rows = host_history_via_acp(state, agent).await?;
    let listed_ids: std::collections::HashSet<String> =
        rows.iter().map(|r| r.key.clone()).collect();
    // Must match how `host_history_via_acp` stamped those same rows, or an
    // unrecognized (custom) agent could never prove authority over its own
    // rows and reconcile would silently never prune.
    let listing_cli = stamped_cli(agent.cli_source.as_ref());
    let listing_cli = Some(&listing_cli);
    // Ids THIS agent listed before and no longer lists — the only rows it may
    // drop. `cli_source` alone is not a session universe: host Copilot and an
    // in-distro Copilot both stamp `Some(Copilot)` while listing disjoint
    // sessions, so a set-difference against the whole registry would have each
    // one delete the other's rows on every poll, forever.
    let prunable_ids: std::collections::HashSet<String> = {
        let mut ever = agent.listed_ever.lock().await;
        let prunable = ever.difference(&listed_ids).cloned().collect();
        ever.extend(listed_ids.iter().cloned());
        prunable
    };

    // Snapshot once; compute existing ids for the add pass and reconcile the
    // terminal Class-B host rows in the same pass.
    let snapshot = state.registry.snapshot().await;
    let existing: std::collections::HashSet<String> = snapshot
        .iter()
        .map(|s| s.session_id.0.to_string())
        .collect();

    let mut changed = false;

    // Add: newly-listed sessions not already in the registry.
    for s in &rows {
        if !existing.contains(&s.key) {
            let info = crate::session_registry::agent_session_to_session_info(s);
            state.registry.upsert_if_absent(info).await;
            changed = true;
        }
    }

    // Reconcile: drop terminal Class-B host rows the agent no longer lists.
    // `remove_if` re-checks staleness on the *current* row under the registry
    // lock, so a row a hook/watcher flips live between the snapshot above and
    // the remove below is never deleted out from under that update.
    for row in &snapshot {
        if !is_stale_host_history_row(row, &prunable_ids, listing_cli) {
            continue;
        }
        let removed = state
            .registry
            .remove_if(&row.session_id, &|cur| {
                is_stale_host_history_row(cur, &prunable_ids, listing_cli)
            })
            .await;
        if removed.is_some() {
            tracing::info!(
                target: "master_history",
                key = %row.session_id.0,
                cli = ?listing_cli,
                "reconcile: dropped host row no longer in session/list"
            );
            changed = true;
        }
    }

    // Title refresh: the agent CLI owns a session's display name and keeps
    // rewriting it (Copilot reports the first user message until it generates
    // a summary), so re-adopt the current listing title for every row this
    // agent owns — not just the still-synthetic ones, which is all
    // `refresh_synthetic_titles_from` can do. A row that latched the transient
    // first-message echo is non-synthetic and would otherwise display it
    // forever. Uses the UNFILTERED title map so live Class-A agent-pane rows,
    // which `host_history_via_acp` subtracts, are refreshed too, and reuses the
    // same 2 s-cached `session/list` fetch this function already made.
    if refresh_titles_from_listing(
        &*state.registry,
        &host_titles_via_acp(agent).await,
        listing_cli,
    )
    .await
    {
        changed = true;
    }

    Some((changed, rows.len()))
}

/// Adopt `titles` (session_id → the listing agent's own `session/list` title)
/// for every registry row that agent owns, replacing a stale real title rather
/// than only filling a synthetic one. Returns true if any row changed.
///
/// Authority is `row_refreshable_by_connected_agent` plus the session id
/// itself, and deliberately NOT `SessionInfo::location`. Host Copilot and an
/// in-distro Copilot share a `CliSource` while enumerating disjoint stores, but
/// the id is what separates them: a host agent's listing simply never contains
/// an in-distro session id (`doc/specs/wsl-session-management.md` calls a
/// host/WSL key collision astronomically unlikely). Gating on `location` would
/// instead *lose* refreshes, because only the born-bound path stamps it —
/// an ordinary `session_hook` row for a CLI running inside WSL keeps the
/// reducer's default `Host`, so the in-distro agent that actually holds its
/// title would be skipped.
///
/// [`crate::session_registry::SessionRegistry::adopt_agent_title`] overwrites
/// whatever it is handed, so every candidate is re-checked with
/// [`crate::session_registry::title_is_displayable`] here, at the point of
/// mutation, and not only where [`host_titles_via_acp`] builds the map. The
/// row's own `cli_source` is the stamp to check against when it has one — it
/// is the CLI that actually produced the session — falling back to
/// `listing_cli` for a row `row_refreshable_by_connected_agent` admitted while
/// unstamped. Without that fallback an unstamped row would skip the
/// provider-specific placeholder check entirely and adopt, say, OpenCode's
/// `New session - <timestamp>`; the candidate came from the listing agent, so
/// that agent's provider is the right rule to judge it by.
async fn refresh_titles_from_listing(
    reg: &dyn crate::session_registry::SessionRegistry,
    titles: &std::collections::HashMap<String, String>,
    listing_cli: Option<&crate::agent_sessions::CliSource>,
) -> bool {
    if titles.is_empty() {
        return false;
    }
    let mut changed = false;
    for row in reg.snapshot().await {
        if !row_refreshable_by_connected_agent(&row, listing_cli) {
            continue;
        }
        let Some(title) = titles.get(row.session_id.0.as_ref()) else {
            continue;
        };
        let judging_cli = row.cli_source.as_ref().or(listing_cli);
        if !crate::session_registry::title_is_displayable(judging_cli, title) {
            continue;
        }
        if reg.adopt_agent_title(&row.session_id, title).await {
            // The title itself is user content (a chat summary), so log only
            // the identity of the row that changed.
            tracing::info!(
                target: "master_history",
                key = %row.session_id.0,
                cli = ?listing_cli,
                "title refresh: adopted updated session/list title"
            );
            changed = true;
        }
    }
    changed
}

/// Whether a registry row is a stale host-history row to drop during reconcile:
/// a terminal (Historical / Ended) Class-B **host** row belonging to
/// `listing_cli` whose id is in `prunable_ids` — the set the listing agent
/// previously returned from `session/list` and no longer returns. Live rows
/// (Working / Idle), agent panes (ACP-driven), and WSL rows are never
/// reconciled away. Pure for unit testing.
///
/// Two guards, and both are load-bearing:
///
/// * `listing_cli` keeps a multi-agent master (and the machine-wide,
///   cross-CLI file watcher) honest — an agent enumerates only its OWN
///   sessions, so its listing is no authority over another CLI's rows.
///   Deliberately stricter than [`row_refreshable_by_connected_agent`], which
///   governs a *non-destructive* title upgrade: pruning requires both sides
///   known and equal, so an unstamped row is kept rather than deleted by
///   whichever agent polls first.
/// * `prunable_ids` is scoped to what the listing agent itself has seen,
///   because `CliSource` is NOT a session universe: host Copilot and an
///   in-distro (WSL) Copilot both stamp `Some(Copilot)` while listing disjoint
///   sessions. A plain "not in the current listing" test would have each drop
///   the other's rows on every 5 s poll while the other re-added them.
fn is_stale_host_history_row(
    row: &crate::session_registry::SessionInfo,
    prunable_ids: &std::collections::HashSet<String>,
    listing_cli: Option<&crate::agent_sessions::CliSource>,
) -> bool {
    use crate::agent_sessions::{AgentStatus, SessionLocation, SessionOrigin};
    if !matches!(row.location, SessionLocation::Host) {
        return false;
    }
    if row.origin == Some(SessionOrigin::AgentPane) {
        return false;
    }
    let terminal = matches!(
        row.status,
        Some(AgentStatus::Historical) | Some(AgentStatus::Ended)
    );
    if !terminal {
        return false;
    }
    if listing_cli.is_none() || row.cli_source.as_ref() != listing_cli {
        return false;
    }
    prunable_ids.contains(row.session_id.0.as_ref())
}

/// Seed + reconcile `agent`'s history against its own `session/list`,
/// broadcasting when anything changed. Returns the listed count.
async fn seed_host_and_broadcast(
    state: &std::sync::Arc<MasterStateInner>,
    agent: &AgentCli,
) -> usize {
    let Some((changed, count)) = sync_host_history(state, agent).await else {
        return 0;
    };
    if changed {
        broadcast_ext_to_helpers(
            state,
            crate::session_registry::build_sessions_changed_notification(),
        )
        .await;
    }
    count
}

/// Before returning the snapshot, opportunistically upgrade any row whose title
/// is still synthetic (empty / cwd-basename) from the agent's raw ACP
/// `session/list` titles.
/// This is what gets a title onto **born-bound** rows — e.g. `?<prompt>`
/// delegate sessions, which register with an empty title before the CLI has
/// generated its real one.
///
/// `agent` is the caller's bound CLI. It is `None` only for a client that never
/// bound one (`wta sessions list`), in which case the ACP re-pull is skipped
/// and the current registry snapshot is returned as-is — master must not guess
/// an agent, since asking the wrong one would stamp and reconcile foreign rows.
async fn handle_sessions_list(
    state: &std::sync::Arc<MasterStateInner>,
    agent: Option<&AgentCli>,
    parsed: &crate::session_registry::SessionsListParams,
) -> acp::Result<acp::schema::v1::ExtResponse> {
    if let Some(agent) = agent {
        if parsed.rescan {
            // Re-pull this agent's own `session/list` and broadcast. Each pooled
            // agent — host or in-distro — enumerates its own sessions, so an
            // F5 in a WSL pane refreshes that distro through its own CLI.
            let count = seed_host_and_broadcast(state, agent).await;
            tracing::info!(
                target: "master_history",
                cli = ?agent.cli_source,
                count,
                "sessions/list rescan: reloaded history via ACP"
            );
        } else {
            // Periodic poll: reconcile host rows against `session/list` (the source
            // of truth) so phantom / CLI-deleted host rows are pruned and newly-listed
            // ones appear. Reuses the 2s-cached fetch. No-op (and no broadcast) when
            // nothing changed or the agent can't list — so a transient error never
            // wipes the view and steady state causes no push storm.
            if let Some((true, _)) = sync_host_history(state, agent).await {
                broadcast_ext_to_helpers(
                    state,
                    crate::session_registry::build_sessions_changed_notification(),
                )
                .await;
            }
        }
    }

    let mut sessions = state.registry.snapshot().await;
    if let Some(agent) = agent {
        if sessions
            .iter()
            .any(crate::session_registry::title_is_synthetic)
        {
            let titles = host_titles_via_acp(agent).await;
            // Re-snapshot only when a title actually changed; the common steady-state
            // (no synthetic rows, or nothing to upgrade) reuses the first snapshot.
            if refresh_synthetic_titles_from(&*state.registry, &titles).await {
                sessions = state.registry.snapshot().await;
            }
        }
    }

    sessions.sort_by(|l, r| l.session_id.0.cmp(&r.session_id.0));
    let raw = crate::session_registry::build_sessions_list_response(sessions);
    Ok(acp::schema::v1::ExtResponse::new(raw.into()))
}

/// Pure async handler for the `intellterm.wta/session_hook` ExtRequest.
///
/// Decodes the hook event, dispatches it to the master-side registry reducer
/// (added in Task A), and broadcasts `sessions/changed` to every connected
/// helper when the reducer actually mutated state. Idempotent / no-op events
/// (reducer returned `false`) skip the broadcast to avoid push storms.
///
/// Title refresh: after the reducer applies, we re-check master's row for a
/// "synthetic" title (cwd basename / empty) and try to upgrade it from the
/// agent's raw ACP `session/list` titles. Session management view renders from
/// master's snapshot, so the upgrade must happen here.
async fn handle_session_hook(
    state: &MasterStateInner,
    event: crate::agent_sessions::SessionEvent,
    is_born_bound: bool,
) -> acp::Result<acp::schema::v1::ExtResponse> {
    // Split by event kind so field diagnosis of session-state bugs survives at
    // the default release level: terminal/lifecycle transitions (session
    // start/stop, pane closed, connection failed) stay at info; the
    // high-frequency routine events (tool start/stop, notifications, resume
    // bookkeeping) go to debug. Keeps the load-bearing transitions visible
    // without the per-tool flood that dominated the info logs.
    {
        use crate::agent_sessions::SessionEvent;
        // Match on a reference so the level decision borrows rather than
        // consumes `event` (it's used again below for the reducer).
        let lifecycle = matches!(
            &event,
            SessionEvent::SessionStarted { .. }
                | SessionEvent::SessionStopped { .. }
                | SessionEvent::ConnectionFailed { .. }
                | SessionEvent::PaneClosed { .. }
        );
        if lifecycle {
            tracing::info!(target: "session_hook", event = ?event, "received helper session hook");
        } else {
            tracing::debug!(target: "session_hook", event = ?event, "received helper session hook");
        }
    }

    // Capture the session key BEFORE moving `event` into the reducer so
    // we can dispatch the post-apply title refresh against the right
    // row. Pane-keyed variants (PaneClosed, ConnectionFailed) don't
    // carry a session key — they only transition the row to Ended /
    // Error, where the title is whatever it already was, so skipping
    // the refresh is fine.
    let refresh_key = session_event_key(&event).map(str::to_owned);

    // Resume binding events (`ResumeDispatched` / `ResumePaneAssigned`) are the
    // hook-free born-bound binding for `/sessions` resume (published over the
    // generic `session_hook` method by the helper). Treat them as binding-only —
    // same as a #266 delegate registration — so the watcher can still supply
    // status for a resumed session when no real hook is installed. Without this
    // they'd mark the session `hook_owned` and the resumed row would sit at Idle
    // forever (the delegate path already works because it uses the dedicated
    // born-bound method).
    let binding_only = is_born_bound
        || matches!(
            &event,
            crate::agent_sessions::SessionEvent::ResumeDispatched { .. }
                | crate::agent_sessions::SessionEvent::ResumePaneAssigned { .. }
        );

    // Record ownership so the file watcher (the fallback producer) coordinates
    // with this authoritative event. Keyed variants only (PaneClosed /
    // ConnectionFailed carry no session key — pane-keyed terminal transitions,
    // not an ownership claim).
    //
    //  * binding-only (#266 delegate born-bound + resume binding events): record
    //    in `born_bound` so the watcher may still supply STATUS when no real hook
    //    is installed — without re-binding the pane. Also drop any **stale**
    //    `hook_owned` claim: the two sets are disjoint by contract, and a
    //    born-bound event means WTA has just (re)launched this session id, so an
    //    ownership claim left by a previous generation of the same session is
    //    over. Without this a `/sessions` resume of a session that ran earlier
    //    in the same master process stayed `hook_owned` forever, and because
    //    `apply_watcher_event` checks `hook_owned` first, every watcher status
    //    event for the resumed row was dropped — the row sat at Idle for the
    //    whole session. A real hook re-claims ownership on its very next event.
    //  * real hook / ACP agent-pane event: authoritative for binding AND
    //    activity. Record in `hook_owned` (full watcher suppression) and, if the
    //    session was previously born-bound, drop it from `born_bound` — the real
    //    hook now owns it.
    if let Some(key) = &refresh_key {
        let sid = acp::schema::v1::SessionId::new(key.clone());
        if binding_only {
            state.hook_owned.lock().await.remove(&sid);
            state.born_bound.lock().await.insert(sid);
        } else {
            state.hook_owned.lock().await.insert(sid.clone());
            state.born_bound.lock().await.remove(&sid);
        }
    }

    let applied = state.registry.apply_event(event).await;

    let title_upgraded = if let Some(key) = refresh_key {
        try_refresh_title_via_acp(state, &acp::schema::v1::SessionId::new(key)).await
    } else {
        false
    };

    if applied || title_upgraded {
        broadcast_ext_to_helpers(
            state,
            crate::session_registry::build_sessions_changed_notification(),
        )
        .await;
    }

    Ok(crate::session_registry::build_session_hook_response(
        applied,
    ))
}

/// Handle a #266 *born-bound* registration (delegate `?<prompt>` / resume).
///
/// Applies the event exactly like [`handle_session_hook`] (binding-only), then —
/// for a WSL delegate — stamps the freshly-created row `SessionLocation::Wsl {
/// distro }`. The `SessionStarted` reducer defaults every row to `Host`, so
/// without this a born-bound WSL delegate row would render without the distro
/// suffix the session view already shows for in-distro rows.
/// Re-broadcasts `sessions/changed` only when the location actually changed, so
/// the host path (no distro) adds no extra push.
async fn handle_session_born_bound(
    state: &MasterStateInner,
    event: crate::agent_sessions::SessionEvent,
    wsl_distro: Option<String>,
) -> acp::Result<acp::schema::v1::ExtResponse> {
    // Capture the key before `event` is moved into the reducer.
    let key = session_event_key(&event).map(str::to_owned);
    let response = handle_session_hook(state, event, true).await?;
    if let (Some(distro), Some(key)) = (wsl_distro, key) {
        let sid = acp::schema::v1::SessionId::new(key);
        let changed = state
            .registry
            .set_location(&sid, crate::agent_sessions::SessionLocation::Wsl { distro })
            .await;
        if changed {
            broadcast_ext_to_helpers(
                state,
                crate::session_registry::build_sessions_changed_notification(),
            )
            .await;
        }
    }
    Ok(response)
}

/// Apply one watcher-emitted session event to master's registry and, if it
/// changed state, broadcast `sessions/changed` so helpers refetch.
///
/// The file watcher is a **status-only fallback for #266 born-bound sessions**
/// (delegate `?<prompt>` / `/sessions` resume). It no longer discovers or
/// pane-binds user-typed shell-pane sessions — that path relied on reading a
/// foreign process's PEB (`proc_bind`) to map a pid to its pane, which was
/// removed. Events are routed as:
///   1. `hook_owned` (a real hook / ACP agent-pane event owns binding AND
///      activity) → drop; or
///   2. `born_bound` (WTA-launched, already pane-bound) → apply STATUS only,
///      without touching the pane binding; or
///   3. anything else (a user-typed CLI, or a machine-wide copilot/claude in
///      VS Code / another terminal) → drop — we can't bind it to an IT pane.
async fn apply_watcher_event(state: &MasterStateInner, emitted: crate::session_watcher::Emitted) {
    let sid = acp::schema::v1::SessionId::new(emitted.key.clone());

    // Hybrid dedup — the watcher is a *fallback*. Coordinate with authoritative
    // producers:
    //   1. a real hook / ACP agent-pane event recorded the session in
    //      `hook_owned` → drop (the hook owns binding AND activity); or
    //   2. it's a #266 born-bound row (`born_bound`) → the watcher owns no
    //      binding here, but with no real hook it supplies STATUS only (handled
    //      just below); or
    //   3. anything else (a user-typed CLI, or a machine-wide copilot/claude in
    //      VS Code / another terminal) → drop below; we can't bind it to a pane.
    if state.hook_owned.lock().await.contains(&sid) {
        return;
    }

    // Born-bound activity-only fallback: the row already exists and is bound to
    // its pane by #266 born-bound. Born-bound emits no activity, so when no real
    // hook is installed the watcher supplies STATUS. `emitted.event` is always a
    // keyed status event (ToolStarting/ToolCompleted/Notification), so applying
    // it updates the row's status without touching the pane binding / origin.
    // Born-bound owns the (live, vetted) pane binding; we only move the status.
    if state.born_bound.lock().await.contains(&sid) {
        let key = emitted.key.clone();
        let applied = state.registry.apply_event(emitted.event).await;
        let title_upgraded =
            try_refresh_title_via_acp(state, &acp::schema::v1::SessionId::new(key)).await;
        if applied || title_upgraded {
            broadcast_ext_to_helpers(
                state,
                crate::session_registry::build_sessions_changed_notification(),
            )
            .await;
        }
        return;
    }

    // Neither hook-owned nor born-bound: a user-typed shell-pane session, or a
    // machine-wide CLI running in VS Code / another terminal. Surfacing it once
    // required pane-binding via the removed PEB reader (`proc_bind`), so there
    // is nothing left to do — drop it.
}

fn build_agent_sessions_retired_event(
    operation_id: &str,
    reason: &str,
    failed_tabs: &[String],
    unattributed_failures: &[String],
) -> serde_json::Value {
    let mut event = serde_json::json!({
        "type": "event",
        "method": "agent_sessions_retired",
        "params": {
            "operation_id": operation_id,
            "success": failed_tabs.is_empty() && unattributed_failures.is_empty(),
            "reason": reason,
            "failed_tabs": failed_tabs,
        }
    });
    if !unattributed_failures.is_empty() {
        event["params"]["unattributed_failures"] = serde_json::json!({
            "count": unattributed_failures.len(),
            "helpers": unattributed_failures,
        });
    }
    event
}

fn publish_agent_sessions_retired(_state: &MasterStateInner, event: serde_json::Value) {
    #[cfg(test)]
    {
        if let Ok(mut completion_tx) = _state.retirement_completion_tx.try_lock() {
            if let Some(completion_tx) = completion_tx.as_mut() {
                let _ = completion_tx.send(event);
                return;
            }
        }
    }
    crate::wt_protocol_events::send(event.to_string());
}

fn resolve_tab_retirement_id(rekeys: &HashMap<String, String>, tab_id: &str) -> String {
    let mut current = tab_id;
    for _ in 0..=rekeys.len() {
        let Some(next) = rekeys.get(current) else {
            return current.to_string();
        };
        current = next;
    }
    current.to_string()
}

async fn current_tab_retirement_id(state: &MasterStateInner, tab_id: &str) -> String {
    let _ownership_guard = state.tab_ownership_gate.lock().await;
    let rekeys = state.tab_retirement_rekeys.lock().await;
    resolve_tab_retirement_id(&rekeys, tab_id)
}

async fn begin_tab_retirement(
    state: &MasterStateInner,
    tab_id: &str,
) -> Option<TabRetirementTarget> {
    let _ownership_guard = state.tab_ownership_gate.lock().await;
    let connected_helpers = state.connected_helpers.lock().await.clone();
    let helper_meta = state.helper_meta.lock().await;
    let pending_helpers = state.pending_session_helpers.lock().await;
    let mut outgoing_helpers = HashSet::new();
    let mut ownerless_helpers = Vec::new();
    for helper_id in &connected_helpers {
        let published_owner = pending_helpers
            .get(helper_id)
            .and_then(Option::as_deref)
            .or_else(|| {
                helper_meta
                    .get(helper_id)
                    .and_then(|recovery| recovery.owner_tab_id.as_deref())
            });
        match published_owner {
            Some(owner_tab_id) if owner_tab_id == tab_id => {
                outgoing_helpers.insert(*helper_id);
            }
            Some(_) => {}
            None => ownerless_helpers.push(*helper_id),
        }
    }
    drop(pending_helpers);
    drop(helper_meta);
    {
        let mut unresolved = state.unresolved_owner_retirements.lock().await;
        for helper_id in ownerless_helpers {
            unresolved
                .entry(helper_id)
                .or_insert_with(|| OwnerlessRetirementSafety::Targets(HashSet::new()))
                .record(tab_id);
        }
    }
    {
        let mut fences = state.tab_retirement_fences.lock().await;
        let fence = fences
            .entry(tab_id.to_string())
            .or_insert_with(|| TabRetirementFence {
                phase: TabRetirementPhase::Fencing,
                active_operations: 0,
                outgoing_helpers: HashSet::new(),
            });
        fence.phase = TabRetirementPhase::Fencing;
        fence.active_operations += 1;
        fence.outgoing_helpers.extend(outgoing_helpers);
    }

    let helper_id = state
        .helper_meta
        .lock()
        .await
        .iter()
        .find_map(|(helper_id, recovery)| {
            (recovery.owner_tab_id.as_deref() == Some(tab_id)).then_some(*helper_id)
        });
    let helper_id =
        if helper_id.is_some() {
            helper_id
        } else {
            state.pending_session_helpers.lock().await.iter().find_map(
                |(helper_id, owner_tab_id)| {
                    (owner_tab_id.as_deref() == Some(tab_id)).then_some(*helper_id)
                },
            )
        };
    let (helper_id, resolved_from_orphan) = if helper_id.is_some() {
        (helper_id, false)
    } else {
        (
            state
                .orphaned_tabs
                .lock()
                .await
                .get(tab_id)
                .map(|(_, helper_id, _)| *helper_id),
            true,
        )
    };
    if let Some(helper_id) = helper_id {
        let requires_future_disconnect =
            !resolved_from_orphan || connected_helpers.contains(&helper_id);
        let mut fences = state.tab_retirement_fences.lock().await;
        let fence = fences
            .get_mut(tab_id)
            .expect("retirement fence was inserted under the ownership gate");
        // Ownership is now authoritative; unrelated helpers that happened to
        // be connected at the generation boundary must not keep this tab's
        // completed fence alive after its actual helper disconnects.
        fence.outgoing_helpers.clear();
        if requires_future_disconnect {
            fence.outgoing_helpers.insert(helper_id);
            state.closing_session_helpers.lock().await.insert(helper_id);
            state
                .destructive_session_helpers
                .lock()
                .await
                .insert(helper_id);
        }
        state
            .active_retirement_helpers
            .lock()
            .await
            .insert(helper_id);
        Some(TabRetirementTarget {
            helper_id,
            requires_future_disconnect,
        })
    } else {
        None
    }
}

async fn complete_tab_retirement(state: &MasterStateInner, tab_id: &str) {
    let _ownership_guard = state.tab_ownership_gate.lock().await;
    let current_tab_id = {
        let rekeys = state.tab_retirement_rekeys.lock().await;
        resolve_tab_retirement_id(&rekeys, tab_id)
    };
    let mut fences = state.tab_retirement_fences.lock().await;
    let (remove, operation_complete) = if let Some(fence) = fences.get_mut(&current_tab_id) {
        fence.active_operations = fence.active_operations.saturating_sub(1);
        if fence.active_operations == 0 {
            fence.phase = TabRetirementPhase::CompletedAwaitingDisconnect;
        }
        (
            fence.active_operations == 0 && fence.outgoing_helpers.is_empty(),
            fence.active_operations == 0,
        )
    } else {
        (false, true)
    };
    if remove {
        fences.remove(&current_tab_id);
    }
    drop(fences);
    if operation_complete {
        let mut rekeys = state.tab_retirement_rekeys.lock().await;
        let completed_aliases = rekeys
            .keys()
            .filter(|alias| resolve_tab_retirement_id(&rekeys, alias) == current_tab_id)
            .cloned()
            .collect::<Vec<_>>();
        for alias in completed_aliases {
            rekeys.remove(&alias);
        }
    }
}

struct CapturedRetirementSession {
    session_id: acp::schema::v1::SessionId,
    agent: Arc<AgentCli>,
    source: CapturedRetirementSessionSource,
}

enum CapturedRetirementSessionSource {
    Route(HelperRoute),
    Orphan {
        tab_id: String,
        agent_key: AgentCmdKey,
    },
}

struct CapturedHelperRetirement {
    helper_id: HelperId,
    owner_tab_id: Option<String>,
    sessions: Vec<CapturedRetirementSession>,
    logical_fallback_required: bool,
}

struct AllRetirementTargets {
    helpers: Vec<CapturedHelperRetirement>,
    orphaned_tabs: Vec<String>,
    ownerless_orphans: Vec<(AgentCmdKey, acp::schema::v1::SessionId)>,
}

async fn capture_helper_retirements(
    state: &MasterStateInner,
    helper_ids: &HashSet<HelperId>,
) -> (Vec<CapturedHelperRetirement>, HashSet<String>) {
    let agents_by_instance = state
        .agents
        .lock()
        .await
        .values()
        .filter_map(|cell| cell.get().cloned())
        .map(|agent| (agent.instance_id, agent))
        .collect::<HashMap<_, _>>();
    let agents_by_key = agents_by_instance
        .values()
        .map(|agent| (agent.cmd_key.clone(), Arc::clone(agent)))
        .collect::<HashMap<_, _>>();
    let mut sessions_by_helper: HashMap<HelperId, Vec<CapturedRetirementSession>> = HashMap::new();
    for (session_id, route) in state.session_to_helper.lock().await.iter() {
        if helper_ids.contains(&route.helper_id) {
            if let Some(agent) = agents_by_instance.get(&route.agent_instance_id) {
                sessions_by_helper.entry(route.helper_id).or_default().push(
                    CapturedRetirementSession {
                        session_id: session_id.clone(),
                        agent: Arc::clone(agent),
                        source: CapturedRetirementSessionSource::Route(route.clone()),
                    },
                );
            }
        }
    }
    let helper_meta = state.helper_meta.lock().await;
    let pending_helpers = state.pending_session_helpers.lock().await;
    let orphaned_tabs = state.orphaned_tabs.lock().await;
    let mut captured_orphaned_tabs = HashSet::new();
    let mut unclosable_helpers = HashSet::new();
    for (tab_id, (agent_key, helper_id, session_id)) in orphaned_tabs.iter() {
        if !helper_ids.contains(helper_id) {
            continue;
        }
        let Some(agent) = agents_by_key.get(agent_key) else {
            unclosable_helpers.insert(*helper_id);
            continue;
        };
        let sessions = sessions_by_helper.entry(*helper_id).or_default();
        let already_captured = sessions.iter().any(|session| {
            session.session_id == *session_id && session.agent.instance_id == agent.instance_id
        });
        if !already_captured {
            sessions.push(CapturedRetirementSession {
                session_id: session_id.clone(),
                agent: Arc::clone(agent),
                source: CapturedRetirementSessionSource::Orphan {
                    tab_id: tab_id.clone(),
                    agent_key: agent_key.clone(),
                },
            });
        }
        captured_orphaned_tabs.insert(tab_id.clone());
    }
    let helpers = helper_ids
        .iter()
        .copied()
        .map(|helper_id| {
            let owner_tab_id = pending_helpers
                .get(&helper_id)
                .and_then(Clone::clone)
                .or_else(|| {
                    helper_meta
                        .get(&helper_id)
                        .and_then(|meta| meta.owner_tab_id.clone())
                })
                .or_else(|| {
                    orphaned_tabs.iter().find_map(|(tab_id, (_, owner, _))| {
                        (*owner == helper_id).then_some(tab_id.clone())
                    })
                });
            CapturedHelperRetirement {
                helper_id,
                owner_tab_id,
                sessions: sessions_by_helper.remove(&helper_id).unwrap_or_default(),
                logical_fallback_required: unclosable_helpers.contains(&helper_id),
            }
        })
        .collect();
    (helpers, captured_orphaned_tabs)
}

async fn begin_all_retirement(state: &MasterStateInner) -> AllRetirementTargets {
    let _ownership_guard = state.tab_ownership_gate.lock().await;
    let outgoing_helpers = state.connected_helpers.lock().await.clone();
    {
        let mut fence = state.all_retirement_fence.lock().await;
        fence.active_operations += 1;
        fence
            .outgoing_helpers
            .extend(outgoing_helpers.iter().copied());
    }
    state
        .closing_session_helpers
        .lock()
        .await
        .extend(outgoing_helpers.iter().copied());
    state
        .destructive_session_helpers
        .lock()
        .await
        .extend(outgoing_helpers.iter().copied());
    state
        .active_retirement_helpers
        .lock()
        .await
        .extend(outgoing_helpers.iter().copied());
    let (helpers, captured_orphaned_tabs) =
        capture_helper_retirements(state, &outgoing_helpers).await;

    let orphaned_tabs_guard = state.orphaned_tabs.lock().await;
    let owned_orphans = orphaned_tabs_guard
        .values()
        .map(|(agent_key, _, session_id)| (agent_key.clone(), session_id.clone()))
        .collect::<HashSet<_>>();
    let orphaned_tabs = orphaned_tabs_guard
        .keys()
        .filter(|tab_id| !captured_orphaned_tabs.contains(*tab_id))
        .cloned()
        .collect();
    let routed_sessions = state
        .session_to_helper
        .lock()
        .await
        .keys()
        .cloned()
        .collect::<HashSet<_>>();
    let ownerless_orphans = state
        .orphaned_sessions
        .lock()
        .await
        .iter()
        .flat_map(|(agent_key, sessions)| {
            sessions.iter().filter_map(|session_id| {
                (!owned_orphans.contains(&(agent_key.clone(), session_id.clone()))
                    && !routed_sessions.contains(session_id))
                .then_some((agent_key.clone(), session_id.clone()))
            })
        })
        .collect();
    AllRetirementTargets {
        helpers,
        orphaned_tabs,
        ownerless_orphans,
    }
}

async fn finish_all_retirement_batch(
    state: &MasterStateInner,
    processed: &HashSet<HelperId>,
) -> Option<HashSet<HelperId>> {
    let _ownership_guard = state.tab_ownership_gate.lock().await;
    let mut fence = state.all_retirement_fence.lock().await;
    let remaining = fence
        .outgoing_helpers
        .difference(processed)
        .copied()
        .collect::<HashSet<_>>();
    if !remaining.is_empty() {
        return Some(remaining);
    }
    fence.active_operations = fence.active_operations.saturating_sub(1);
    if fence.active_operations == 0 {
        fence.outgoing_helpers.clear();
    }
    None
}

async fn register_connected_helper(state: &MasterStateInner, helper_id: HelperId) {
    let _ownership_guard = state.tab_ownership_gate.lock().await;
    state.connected_helpers.lock().await.insert(helper_id);
    let outgoing = {
        let mut fence = state.all_retirement_fence.lock().await;
        if fence.active_operations == 0 {
            false
        } else {
            fence.outgoing_helpers.insert(helper_id);
            true
        }
    };
    if outgoing {
        state.closing_session_helpers.lock().await.insert(helper_id);
        state
            .destructive_session_helpers
            .lock()
            .await
            .insert(helper_id);
        state
            .active_retirement_helpers
            .lock()
            .await
            .insert(helper_id);
    }
}

fn merge_retirement_cleanup(
    current: ReplacedSessionCleanup,
    next: ReplacedSessionCleanup,
) -> ReplacedSessionCleanup {
    if current == ReplacedSessionCleanup::LogicalFallback
        || next == ReplacedSessionCleanup::LogicalFallback
    {
        ReplacedSessionCleanup::LogicalFallback
    } else if current == ReplacedSessionCleanup::PhysicallyClosed
        || next == ReplacedSessionCleanup::PhysicallyClosed
    {
        ReplacedSessionCleanup::PhysicallyClosed
    } else {
        ReplacedSessionCleanup::NotOwned
    }
}

fn retirement_pending_timeout(state: &MasterStateInner) -> std::time::Duration {
    #[cfg(test)]
    {
        state.retirement_pending_timeout
    }
    #[cfg(not(test))]
    {
        let _ = state;
        SESSION_CLOSE_TIMEOUT
    }
}

fn retirement_remaining(deadline: tokio::time::Instant) -> std::time::Duration {
    deadline.saturating_duration_since(tokio::time::Instant::now())
}

fn schedule_deferred_tab_orphan_cleanup(
    state: &Arc<MasterStateInner>,
    agent_key: AgentCmdKey,
    helper_id: HelperId,
    session_id: acp::schema::v1::SessionId,
) {
    let state = Arc::clone(state);
    tokio::task::spawn_local(async move {
        let gate = session_lifecycle_gate(&state, &session_id).await;
        let gate_result = tokio::time::timeout(SESSION_CLOSE_TIMEOUT, gate.lock()).await;
        if let Ok(_guard) = gate_result {
            let orphan_tab_id =
                state
                    .orphaned_tabs
                    .lock()
                    .await
                    .iter()
                    .find_map(|(tab_id, current)| {
                        (current == &(agent_key.clone(), helper_id, session_id.clone()))
                            .then_some(tab_id.clone())
                    });
            let orphan_session_is_current = state
                .orphaned_sessions
                .lock()
                .await
                .get(&agent_key)
                .is_some_and(|sessions| sessions.contains(&session_id));
            if let Some(orphan_tab_id) = orphan_tab_id.filter(|_| orphan_session_is_current) {
                let rebound = state
                    .session_to_helper
                    .lock()
                    .await
                    .contains_key(&session_id);
                state.orphaned_tabs.lock().await.remove(&orphan_tab_id);
                let mut orphaned_sessions = state.orphaned_sessions.lock().await;
                if let Some(sessions) = orphaned_sessions.get_mut(&agent_key) {
                    sessions.remove(&session_id);
                    if sessions.is_empty() {
                        orphaned_sessions.remove(&agent_key);
                    }
                }
                drop(orphaned_sessions);
                if !rebound {
                    retire_unbound_session_state_gate_held(&state, &session_id).await;
                    let _ownership_guard = state.tab_ownership_gate.lock().await;
                    let remove_meta =
                        state
                            .helper_meta
                            .lock()
                            .await
                            .get(&helper_id)
                            .is_some_and(|meta| {
                                meta.last_session_id.as_ref() == Some(&session_id)
                                    && meta.owner_tab_id.as_deref() == Some(orphan_tab_id.as_str())
                            });
                    if remove_meta {
                        state.helper_meta.lock().await.remove(&helper_id);
                    }
                    let remove_pending = state
                        .pending_session_helpers
                        .lock()
                        .await
                        .get(&helper_id)
                        .is_some_and(|owner| owner.as_deref() == Some(orphan_tab_id.as_str()));
                    if remove_pending {
                        state
                            .pending_session_helpers
                            .lock()
                            .await
                            .remove(&helper_id);
                        state.session_transaction_changed.notify_waiters();
                    }
                }
            }
        } else {
            tracing::warn!(
                target: "master_retirement",
                helper_id = ?helper_id,
                session_id = %session_id,
                "deferred tab orphan cleanup expired waiting for lifecycle gate"
            );
        };
        #[cfg(test)]
        state.deferred_retirement_cleanup_complete.notify_one();
    });
}

fn schedule_deferred_ownerless_orphan_cleanup(
    state: &Arc<MasterStateInner>,
    agent_key: AgentCmdKey,
    session_id: acp::schema::v1::SessionId,
) {
    let state = Arc::clone(state);
    tokio::task::spawn_local(async move {
        let gate = session_lifecycle_gate(&state, &session_id).await;
        let gate_result = tokio::time::timeout(SESSION_CLOSE_TIMEOUT, gate.lock()).await;
        if let Ok(_guard) = gate_result {
            let orphan_is_current = state
                .orphaned_sessions
                .lock()
                .await
                .get(&agent_key)
                .is_some_and(|sessions| sessions.contains(&session_id));
            if orphan_is_current {
                let rebound = state
                    .session_to_helper
                    .lock()
                    .await
                    .contains_key(&session_id);
                let mut orphaned_sessions = state.orphaned_sessions.lock().await;
                if let Some(sessions) = orphaned_sessions.get_mut(&agent_key) {
                    sessions.remove(&session_id);
                    if sessions.is_empty() {
                        orphaned_sessions.remove(&agent_key);
                    }
                }
                drop(orphaned_sessions);
                if !rebound {
                    retire_unbound_session_state_gate_held(&state, &session_id).await;
                }
            }
        } else {
            tracing::warn!(
                target: "master_retirement",
                session_id = %session_id,
                "deferred ownerless orphan cleanup expired waiting for lifecycle gate"
            );
        };
        #[cfg(test)]
        state.deferred_retirement_cleanup_complete.notify_one();
    });
}

fn prune_retirement_operations(
    operations: &mut HashMap<String, RetirementOperationState>,
    now: tokio::time::Instant,
) {
    operations.retain(|_, state| match state {
        RetirementOperationState::InFlight => true,
        RetirementOperationState::Completed { completed_at, .. } => {
            now.saturating_duration_since(*completed_at) < RETIREMENT_COMPLETION_TTL
        }
    });

    let mut completed = operations
        .iter()
        .filter_map(|(operation_id, state)| match state {
            RetirementOperationState::InFlight => None,
            RetirementOperationState::Completed { completed_at, .. } => {
                Some((operation_id.clone(), *completed_at))
            }
        })
        .collect::<Vec<_>>();
    if completed.len() <= RETIREMENT_COMPLETION_CAP {
        return;
    }
    let excess = completed.len() - RETIREMENT_COMPLETION_CAP;
    completed.sort_unstable_by_key(|(_, completed_at)| *completed_at);
    for (operation_id, _) in completed.into_iter().take(excess) {
        operations.remove(&operation_id);
    }
}

async fn record_retirement_completion(
    state: &MasterStateInner,
    operation_id: String,
    event: serde_json::Value,
) {
    let now = tokio::time::Instant::now();
    let mut operations = state.retirement_operations.lock().await;
    operations.insert(
        operation_id,
        RetirementOperationState::Completed {
            event,
            completed_at: now,
        },
    );
    prune_retirement_operations(&mut operations, now);
}

async fn force_cleanup_retirement_helper(
    state: &MasterStateInner,
    helper_id: HelperId,
    tab_id: &str,
    mut cleanup: ReplacedSessionCleanup,
    deadline: tokio::time::Instant,
    requires_future_disconnect: bool,
) -> ReplacedSessionCleanup {
    if let Some(late_cleanup) = state
        .closing_session_results
        .lock()
        .await
        .remove(&helper_id)
    {
        cleanup = merge_retirement_cleanup(cleanup, late_cleanup);
    }

    let mut session_ids = {
        let routes = state.session_to_helper.lock().await;
        routes
            .iter()
            .filter_map(|(session_id, route)| {
                (route.helper_id == helper_id).then_some(session_id.clone())
            })
            .collect::<HashSet<_>>()
    };
    if let Some(session_id) = state
        .helper_meta
        .lock()
        .await
        .get(&helper_id)
        .and_then(|meta| meta.last_session_id.clone())
    {
        session_ids.insert(session_id);
    }
    session_ids.extend(state.orphaned_tabs.lock().await.iter().filter_map(
        |(orphan_tab_id, (_, orphan_helper_id, session_id))| {
            (orphan_tab_id == tab_id || *orphan_helper_id == helper_id)
                .then_some(session_id.clone())
        },
    ));

    let mut session_cleanup_timed_out = false;
    for session_id in &session_ids {
        let forced = match tokio::time::timeout_at(
            deadline,
            force_retire_owned_session_state(state, helper_id, session_id),
        )
        .await
        {
            Ok(forced) => forced,
            Err(_) => {
                tracing::error!(
                    target: "master_retirement",
                    tab_id,
                    helper_id = ?helper_id,
                    session_id = %session_id,
                    "retirement deadline expired during forced session cleanup"
                );
                cleanup =
                    merge_retirement_cleanup(cleanup, ReplacedSessionCleanup::LogicalFallback);
                session_cleanup_timed_out = true;
                break;
            }
        };
        if forced == ReplacedSessionCleanup::NotOwned {
            if tokio::time::timeout_at(deadline, retire_unbound_session_state(state, session_id))
                .await
                .is_err()
            {
                tracing::error!(
                    target: "master_retirement",
                    tab_id,
                    helper_id = ?helper_id,
                    session_id = %session_id,
                    "retirement deadline expired during forced unbound-session cleanup"
                );
                cleanup =
                    merge_retirement_cleanup(cleanup, ReplacedSessionCleanup::LogicalFallback);
                session_cleanup_timed_out = true;
                break;
            }
        }
        cleanup = merge_retirement_cleanup(cleanup, forced);
    }

    if !session_cleanup_timed_out && !session_ids.is_empty() {
        let mut orphaned_sessions = state.orphaned_sessions.lock().await;
        for sessions in orphaned_sessions.values_mut() {
            for session_id in &session_ids {
                sessions.remove(session_id);
            }
        }
        orphaned_sessions.retain(|_, sessions| !sessions.is_empty());
    }
    if !session_cleanup_timed_out {
        state
            .orphaned_tabs
            .lock()
            .await
            .retain(|orphan_tab_id, (_, orphan_helper_id, _)| {
                orphan_tab_id != tab_id && *orphan_helper_id != helper_id
            });
    }

    let pending_removed = {
        let _ownership_guard = state.tab_ownership_gate.lock().await;
        let pending_removed = if session_cleanup_timed_out {
            false
        } else {
            let pending_removed = state
                .pending_session_helpers
                .lock()
                .await
                .remove(&helper_id)
                .is_some();
            state.helper_meta.lock().await.remove(&helper_id);
            state
                .unresolved_owner_retirements
                .lock()
                .await
                .remove(&helper_id);
            pending_removed
        };
        if !requires_future_disconnect && !session_cleanup_timed_out {
            state
                .closing_session_helpers
                .lock()
                .await
                .remove(&helper_id);
            state
                .destructive_session_helpers
                .lock()
                .await
                .remove(&helper_id);
            state.tab_retirement_fences.lock().await.retain(|_, fence| {
                fence.outgoing_helpers.remove(&helper_id);
                fence.phase == TabRetirementPhase::Fencing || !fence.outgoing_helpers.is_empty()
            });
        }
        pending_removed
    };
    if !session_cleanup_timed_out {
        if let Some(pending_mcp) = state.pending_session_mcp.lock().await.remove(&helper_id) {
            state.session_mcp_capabilities.cancel(&pending_mcp).await;
        }
    }
    if pending_removed {
        state.session_transaction_changed.notify_waiters();
    }
    state
        .active_retirement_helpers
        .lock()
        .await
        .remove(&helper_id);
    cleanup
}

async fn retire_captured_orphan_session(
    state: &MasterStateInner,
    helper_id: HelperId,
    tab_id: &str,
    agent_key: &AgentCmdKey,
    agent: &AgentCli,
    session_id: &acp::schema::v1::SessionId,
    deadline: tokio::time::Instant,
) -> ReplacedSessionCleanup {
    let gate = session_lifecycle_gate(state, session_id).await;
    let Ok(_guard) = tokio::time::timeout_at(deadline, gate.lock()).await else {
        return ReplacedSessionCleanup::LogicalFallback;
    };
    let orphan_is_current = state
        .orphaned_tabs
        .lock()
        .await
        .get(tab_id)
        .is_some_and(|current| current == &(agent_key.clone(), helper_id, session_id.clone()));
    if !orphan_is_current
        || state
            .session_to_helper
            .lock()
            .await
            .contains_key(session_id)
    {
        return ReplacedSessionCleanup::LogicalFallback;
    }

    let cancel = tokio::time::timeout_at(
        deadline,
        agent
            .conn
            .cancel(acp::schema::v1::CancelNotification::new(session_id.clone())),
    )
    .await;
    match cancel {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(
                target: "master_retirement",
                tab_id,
                helper_id = ?helper_id,
                session_id = %session_id,
                agent_instance_id = %agent.instance_id,
                error = %error,
                "failed to cancel captured orphan before retirement"
            );
        }
        Err(_) => return ReplacedSessionCleanup::LogicalFallback,
    }

    let cleanup = if agent_supports_session_close(agent) {
        match tokio::time::timeout_at(
            deadline,
            agent
                .conn
                .close_session(acp::schema::v1::CloseSessionRequest::new(
                    session_id.clone(),
                )),
        )
        .await
        {
            Ok(Ok(_)) => ReplacedSessionCleanup::PhysicallyClosed,
            Ok(Err(error)) => {
                tracing::error!(
                    target: "master_retirement",
                    tab_id,
                    helper_id = ?helper_id,
                    session_id = %session_id,
                    agent_instance_id = %agent.instance_id,
                    error = %error,
                    "failed to physically close captured orphan; retiring WTA state"
                );
                ReplacedSessionCleanup::LogicalFallback
            }
            Err(_) => {
                tracing::error!(
                    target: "master_retirement",
                    tab_id,
                    helper_id = ?helper_id,
                    session_id = %session_id,
                    agent_instance_id = %agent.instance_id,
                    "timed out physically closing captured orphan; retiring WTA state"
                );
                ReplacedSessionCleanup::LogicalFallback
            }
        }
    } else {
        ReplacedSessionCleanup::LogicalFallback
    };

    {
        let mut orphaned_tabs = state.orphaned_tabs.lock().await;
        if orphaned_tabs
            .get(tab_id)
            .is_some_and(|current| current == &(agent_key.clone(), helper_id, session_id.clone()))
        {
            orphaned_tabs.remove(tab_id);
        }
    }
    {
        let mut orphaned_sessions = state.orphaned_sessions.lock().await;
        if let Some(sessions) = orphaned_sessions.get_mut(agent_key) {
            sessions.remove(session_id);
            if sessions.is_empty() {
                orphaned_sessions.remove(agent_key);
            }
        }
    }
    retire_unbound_session_state_gate_held(state, session_id).await;
    cleanup
}

async fn retire_ownerless_orphan_session(
    state: &Arc<MasterStateInner>,
    agent_key: &AgentCmdKey,
    session_id: &acp::schema::v1::SessionId,
    deadline: tokio::time::Instant,
) -> ReplacedSessionCleanup {
    let gate = session_lifecycle_gate(state, session_id).await;
    let Ok(_guard) = tokio::time::timeout_at(deadline, gate.lock()).await else {
        tracing::error!(
            target: "master_retirement",
            session_id = %session_id,
            "retirement deadline expired waiting for ownerless orphan lifecycle gate; deferring exact orphan cleanup"
        );
        schedule_deferred_ownerless_orphan_cleanup(state, agent_key.clone(), session_id.clone());
        return ReplacedSessionCleanup::LogicalFallback;
    };

    let orphan_is_current = state
        .orphaned_sessions
        .lock()
        .await
        .get(agent_key)
        .is_some_and(|sessions| sessions.contains(session_id));
    if !orphan_is_current {
        return ReplacedSessionCleanup::NotOwned;
    }
    if state
        .session_to_helper
        .lock()
        .await
        .contains_key(session_id)
    {
        let mut orphaned_sessions = state.orphaned_sessions.lock().await;
        if let Some(sessions) = orphaned_sessions.get_mut(agent_key) {
            sessions.remove(session_id);
            if sessions.is_empty() {
                orphaned_sessions.remove(agent_key);
            }
        }
        return ReplacedSessionCleanup::NotOwned;
    }

    let agent = {
        let agents = state.agents.lock().await;
        agents.get(agent_key).and_then(|cell| cell.get()).cloned()
    };
    let cleanup = if let Some(agent) = agent {
        let cancel_timed_out = match tokio::time::timeout_at(
            deadline,
            agent
                .conn
                .cancel(acp::schema::v1::CancelNotification::new(session_id.clone())),
        )
        .await
        {
            Ok(Ok(())) => false,
            Ok(Err(error)) => {
                tracing::warn!(
                    target: "master_retirement",
                    session_id = %session_id,
                    agent_instance_id = %agent.instance_id,
                    error = %error,
                    "failed to cancel ownerless orphan before retirement"
                );
                false
            }
            Err(_) => {
                tracing::error!(
                    target: "master_retirement",
                    session_id = %session_id,
                    agent_instance_id = %agent.instance_id,
                    "timed out cancelling ownerless orphan; retiring WTA state"
                );
                true
            }
        };
        if cancel_timed_out || !agent_supports_session_close(&agent) {
            ReplacedSessionCleanup::LogicalFallback
        } else {
            match tokio::time::timeout_at(
                deadline,
                agent
                    .conn
                    .close_session(acp::schema::v1::CloseSessionRequest::new(
                        session_id.clone(),
                    )),
            )
            .await
            {
                Ok(Ok(_)) => ReplacedSessionCleanup::PhysicallyClosed,
                Ok(Err(error)) => {
                    tracing::error!(
                        target: "master_retirement",
                        session_id = %session_id,
                        agent_instance_id = %agent.instance_id,
                        error = %error,
                        "failed to physically close ownerless orphan; retiring WTA state"
                    );
                    ReplacedSessionCleanup::LogicalFallback
                }
                Err(_) => {
                    tracing::error!(
                        target: "master_retirement",
                        session_id = %session_id,
                        agent_instance_id = %agent.instance_id,
                        "timed out physically closing ownerless orphan; retiring WTA state"
                    );
                    ReplacedSessionCleanup::LogicalFallback
                }
            }
        }
    } else {
        tracing::warn!(
            target: "master_retirement",
            session_id = %session_id,
            "ownerless orphan agent is unavailable; retiring WTA state"
        );
        ReplacedSessionCleanup::LogicalFallback
    };

    {
        let mut orphaned_sessions = state.orphaned_sessions.lock().await;
        if let Some(sessions) = orphaned_sessions.get_mut(agent_key) {
            sessions.remove(session_id);
            if sessions.is_empty() {
                orphaned_sessions.remove(agent_key);
            }
        }
    }
    retire_unbound_session_state_gate_held(state, session_id).await;
    cleanup
}

async fn retire_helper_transaction(
    state: &MasterStateInner,
    captured: CapturedHelperRetirement,
    deadline: tokio::time::Instant,
) -> (HelperId, Option<String>, ReplacedSessionCleanup) {
    let CapturedHelperRetirement {
        helper_id,
        owner_tab_id,
        sessions,
        logical_fallback_required,
    } = captured;
    let results = futures::future::join_all(sessions.into_iter().map(|session| async move {
        let CapturedRetirementSession {
            session_id,
            agent,
            source,
        } = session;
        let route = match source {
            CapturedRetirementSessionSource::Route(route) => route,
            CapturedRetirementSessionSource::Orphan { tab_id, agent_key } => {
                return retire_captured_orphan_session(
                    state,
                    helper_id,
                    &tab_id,
                    &agent_key,
                    &agent,
                    &session_id,
                    deadline,
                )
                .await;
            }
        };
        let gate = session_lifecycle_gate(state, &session_id).await;
        let restored = match tokio::time::timeout_at(deadline, gate.lock()).await {
            Ok(_guard) => {
                let mut routes = state.session_to_helper.lock().await;
                match routes.get(&session_id) {
                    Some(route)
                        if route.helper_id == helper_id
                            && route.agent_instance_id == agent.instance_id =>
                    {
                        true
                    }
                    Some(_) => false,
                    None => {
                        routes.insert(session_id.clone(), route);
                        true
                    }
                }
            }
            Err(_) => false,
        };
        if !restored {
            return ReplacedSessionCleanup::LogicalFallback;
        }
        close_and_retire_owned_session(state, helper_id, &agent, &session_id, deadline, true)
            .await
            .map(|cleanup| {
                if cleanup == ReplacedSessionCleanup::NotOwned {
                    ReplacedSessionCleanup::LogicalFallback
                } else {
                    cleanup
                }
            })
            .unwrap_or(ReplacedSessionCleanup::LogicalFallback)
    }))
    .await;
    let initial_cleanup = if logical_fallback_required {
        ReplacedSessionCleanup::LogicalFallback
    } else {
        ReplacedSessionCleanup::NotOwned
    };
    let mut cleanup = results
        .into_iter()
        .fold(initial_cleanup, merge_retirement_cleanup);

    loop {
        let notified = state.session_transaction_changed.notified();
        if !state
            .pending_session_helpers
            .lock()
            .await
            .contains_key(&helper_id)
        {
            break;
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            tracing::error!(
                target: "master_retirement",
                helper_id = ?helper_id,
                "timed out waiting for captured helper session transaction retirement"
            );
            cleanup = merge_retirement_cleanup(cleanup, ReplacedSessionCleanup::LogicalFallback);
            break;
        }
    }

    cleanup = force_cleanup_retirement_helper(state, helper_id, "", cleanup, deadline, true).await;
    (helper_id, owner_tab_id, cleanup)
}

async fn retire_tab_transaction(
    state: &Arc<MasterStateInner>,
    tab_id: String,
    deadline: tokio::time::Instant,
) -> ReplacedSessionCleanup {
    // Establish the destructive fence before resolving ownership. This makes
    // a concurrent owner publication serialize either wholly before the fence
    // (and become the bound outgoing helper) or wholly after it (and fail).
    let retirement_target = begin_tab_retirement(state, &tab_id).await;
    let current_tab_id = current_tab_retirement_id(state, &tab_id).await;

    let mut cleanup = match retire_tab_session(
        state,
        &crate::session_registry::CloseTabSessionParams {
            tab_id: current_tab_id,
        },
        false,
        true,
        deadline,
    )
    .await
    {
        Ok(cleanup) => cleanup,
        Err(error) => {
            tracing::error!(
                target: "master_retirement",
                tab_id,
                error = %error,
                "destructive tab retirement failed; forcing logical cleanup"
            );
            ReplacedSessionCleanup::LogicalFallback
        }
    };
    if let Some(retirement_target) = retirement_target {
        let helper_id = retirement_target.helper_id;
        loop {
            let notified = state.session_transaction_changed.notified();
            if !state
                .pending_session_helpers
                .lock()
                .await
                .contains_key(&helper_id)
            {
                break;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                tracing::error!(
                    target: "master_retirement",
                    tab_id,
                    helper_id = ?helper_id,
                    "timed out waiting for pending session transaction retirement"
                );
                cleanup =
                    merge_retirement_cleanup(cleanup, ReplacedSessionCleanup::LogicalFallback);
                break;
            }
        }
        let current_tab_id = current_tab_retirement_id(state, &tab_id).await;
        cleanup = force_cleanup_retirement_helper(
            state,
            helper_id,
            &current_tab_id,
            cleanup,
            deadline,
            retirement_target.requires_future_disconnect,
        )
        .await;
    }
    complete_tab_retirement(state, &tab_id).await;
    cleanup
}

async fn run_retirement_operation(
    state: Arc<MasterStateInner>,
    operation_id: String,
    scope: String,
    requested_tabs: Vec<String>,
    reason: String,
) {
    if scope == "all" {
        let targets = begin_all_retirement(&state).await;
        let deadline = tokio::time::Instant::now() + retirement_pending_timeout(&state);
        let mut processed_helpers = HashSet::new();
        let orphaned_tabs = targets.orphaned_tabs;
        let ownerless_orphans = targets.ownerless_orphans;
        let helper_results = futures::future::join_all(
            targets
                .helpers
                .into_iter()
                .map(|captured| retire_helper_transaction(&state, captured, deadline)),
        );
        let orphan_results = futures::future::join_all(
            orphaned_tabs
                .iter()
                .cloned()
                .map(|tab_id| retire_tab_transaction(&state, tab_id, deadline)),
        );
        let ownerless_results =
            futures::future::join_all(ownerless_orphans.iter().map(|(agent_key, session_id)| {
                retire_ownerless_orphan_session(&state, agent_key, session_id, deadline)
            }));
        let (helper_results, orphan_results, ownerless_results) =
            tokio::join!(helper_results, orphan_results, ownerless_results);
        let mut failed_tabs = Vec::new();
        let mut unattributed_failures = Vec::new();
        for (helper_id, owner_tab_id, cleanup) in helper_results {
            processed_helpers.insert(helper_id);
            if cleanup == ReplacedSessionCleanup::LogicalFallback {
                if let Some(tab_id) = owner_tab_id {
                    failed_tabs.push(tab_id);
                } else {
                    unattributed_failures.push(format!("{helper_id:?}"));
                }
            }
        }
        unattributed_failures.extend(ownerless_orphans.iter().zip(ownerless_results).filter_map(
            |((_, session_id), cleanup)| {
                (cleanup == ReplacedSessionCleanup::LogicalFallback)
                    .then(|| format!("orphan:{session_id}"))
            },
        ));
        while let Some(next_batch) = finish_all_retirement_batch(&state, &processed_helpers).await {
            let (captured, captured_orphaned_tabs) =
                capture_helper_retirements(&state, &next_batch).await;
            let orphaned_tabs = state
                .orphaned_tabs
                .lock()
                .await
                .iter()
                .filter_map(|(tab_id, (_, helper_id, _))| {
                    (next_batch.contains(helper_id) && !captured_orphaned_tabs.contains(tab_id))
                        .then_some(tab_id.clone())
                })
                .collect::<Vec<_>>();
            let helper_results = futures::future::join_all(
                captured
                    .into_iter()
                    .map(|captured| retire_helper_transaction(&state, captured, deadline)),
            );
            let orphan_results = futures::future::join_all(
                orphaned_tabs
                    .iter()
                    .cloned()
                    .map(|tab_id| retire_tab_transaction(&state, tab_id, deadline)),
            );
            let (helper_results, orphan_results) = tokio::join!(helper_results, orphan_results);
            for (helper_id, owner_tab_id, cleanup) in helper_results {
                processed_helpers.insert(helper_id);
                if cleanup == ReplacedSessionCleanup::LogicalFallback {
                    if let Some(tab_id) = owner_tab_id {
                        failed_tabs.push(tab_id);
                    } else {
                        unattributed_failures.push(format!("{helper_id:?}"));
                    }
                }
            }
            failed_tabs.extend(orphaned_tabs.into_iter().zip(orphan_results).filter_map(
                |(tab_id, cleanup)| {
                    (cleanup == ReplacedSessionCleanup::LogicalFallback).then_some(tab_id)
                },
            ));
        }
        failed_tabs.extend(orphaned_tabs.into_iter().zip(orphan_results).filter_map(
            |(tab_id, cleanup)| {
                (cleanup == ReplacedSessionCleanup::LogicalFallback).then_some(tab_id)
            },
        ));
        failed_tabs.sort();
        failed_tabs.dedup();
        unattributed_failures.sort();
        unattributed_failures.dedup();
        let event = build_agent_sessions_retired_event(
            &operation_id,
            &reason,
            &failed_tabs,
            &unattributed_failures,
        );
        record_retirement_completion(&state, operation_id, event.clone()).await;
        publish_agent_sessions_retired(&state, event);
        return;
    }

    let targets: Vec<String> = if scope == "tabs" {
        requested_tabs
            .into_iter()
            .filter(|tab_id| !tab_id.is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    } else {
        let event =
            build_agent_sessions_retired_event(&operation_id, &reason, &requested_tabs, &[]);
        record_retirement_completion(&state, operation_id, event.clone()).await;
        publish_agent_sessions_retired(&state, event);
        return;
    };

    let deadline = tokio::time::Instant::now() + retirement_pending_timeout(&state);
    let results = futures::future::join_all(
        targets
            .iter()
            .cloned()
            .map(|tab_id| retire_tab_transaction(&state, tab_id, deadline)),
    )
    .await;
    let failed_tabs = targets
        .into_iter()
        .zip(results)
        .filter_map(|(tab_id, cleanup)| {
            (cleanup == ReplacedSessionCleanup::LogicalFallback).then_some(tab_id)
        })
        .collect::<Vec<_>>();
    let event = build_agent_sessions_retired_event(&operation_id, &reason, &failed_tabs, &[]);
    record_retirement_completion(&state, operation_id, event.clone()).await;
    publish_agent_sessions_retired(&state, event);
}

async fn handle_retire_agent_sessions_event(
    state: &Arc<MasterStateInner>,
    params: serde_json::Value,
) {
    let operation_id = params
        .get("operation_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if operation_id.is_empty() {
        tracing::warn!(
            target: "master_retirement",
            "retire_agent_sessions missing nonempty operation_id"
        );
        return;
    }
    let scope = params
        .get("scope")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let reason = params
        .get("reason")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let tab_ids = params
        .get("tab_ids")
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let replay = {
        let mut operations = state.retirement_operations.lock().await;
        prune_retirement_operations(&mut operations, tokio::time::Instant::now());
        match operations.get(&operation_id) {
            Some(RetirementOperationState::InFlight) => return,
            Some(RetirementOperationState::Completed { event, .. }) => Some(event.clone()),
            None => {
                operations.insert(operation_id.clone(), RetirementOperationState::InFlight);
                None
            }
        }
    };
    if let Some(event) = replay {
        publish_agent_sessions_retired(state, event);
        return;
    }

    let operation_state = Arc::clone(state);
    tokio::task::spawn_local(async move {
        run_retirement_operation(operation_state, operation_id, scope, tab_ids, reason).await;
    });
}

/// Master-side WT event subscriber. Bridges `connection_state`
/// notifications from the COM channel into the master's session
/// registry so that closing a pane (Ctrl+Shift+W, close-tab, hard kill)
/// reliably demotes any session bound to that pane — even when no
/// `wta-helper` publishes a `session_hook` for it. Two cases this
/// covers in practice:
///
///   * Helper in the closing pane dies before its
///     `connection_state` handler runs.
///   * Shell-pane Gemini sessions on hard close: Gemini's `SessionEnd`
///     hook is unreliable on `CTRL_CLOSE_EVENT`, and the helper observation
///     path may not
///     publish for reasons we have not finished isolating.
///
/// Copilot / Claude's Stop / SessionEnd hooks fire fast enough that
/// the publish-from-helper path works for them today; this subscriber
/// makes the behavior uniform across CLIs and resilient to helper
/// teardown order.
async fn handle_master_wt_event(state: &Arc<MasterStateInner>, event_json: serde_json::Value) {
    let method = event_json
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let params = event_json
        .get("params")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    if method == "retire_agent_sessions" {
        handle_retire_agent_sessions_event(state, params).await;
        return;
    }

    if method == "tab_renamed" {
        let old_tab_id = params
            .get("old_tab_id")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let new_tab_id = params
            .get("new_tab_id")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if old_tab_id.is_empty() || new_tab_id.is_empty() {
            tracing::warn!(
                target: "master_wt_event",
                "tab_renamed missing old_tab_id or new_tab_id"
            );
            return;
        }
        if old_tab_id == new_tab_id {
            return;
        }
        let mut renamed_helpers = 0usize;
        let _ownership_guard = state.tab_ownership_gate.lock().await;
        {
            let mut helper_meta = state.helper_meta.lock().await;
            for recovery in helper_meta.values_mut() {
                if recovery.owner_tab_id.as_deref() == Some(old_tab_id) {
                    recovery.owner_tab_id = Some(new_tab_id.to_string());
                    renamed_helpers += 1;
                }
            }
        }
        {
            let mut pending = state.pending_session_helpers.lock().await;
            for owner_tab_id in pending.values_mut() {
                if owner_tab_id.as_deref() == Some(old_tab_id) {
                    *owner_tab_id = Some(new_tab_id.to_string());
                }
            }
        }
        {
            let mut orphaned_tabs = state.orphaned_tabs.lock().await;
            if let Some(orphan) = orphaned_tabs.remove(old_tab_id) {
                orphaned_tabs.insert(new_tab_id.to_string(), orphan);
            }
        }
        for safety in state.unresolved_owner_retirements.lock().await.values_mut() {
            safety.rekey(old_tab_id, new_tab_id);
        }
        let retirement_rekeyed = {
            let mut fences = state.tab_retirement_fences.lock().await;
            if let Some(old_fence) = fences.remove(old_tab_id) {
                let active = old_fence.active_operations > 0;
                if let Some(new_fence) = fences.get_mut(new_tab_id) {
                    if old_fence.phase == TabRetirementPhase::Fencing {
                        new_fence.phase = TabRetirementPhase::Fencing;
                    }
                    new_fence.active_operations += old_fence.active_operations;
                    new_fence
                        .outgoing_helpers
                        .extend(old_fence.outgoing_helpers);
                } else {
                    fences.insert(new_tab_id.to_string(), old_fence);
                }
                active
            } else {
                false
            }
        };
        if retirement_rekeyed {
            let mut rekeys = state.tab_retirement_rekeys.lock().await;
            let aliases = rekeys
                .keys()
                .filter(|alias| resolve_tab_retirement_id(&rekeys, alias) == old_tab_id)
                .cloned()
                .chain(std::iter::once(old_tab_id.to_string()))
                .collect::<HashSet<_>>();
            rekeys.remove(new_tab_id);
            for alias in aliases {
                if alias == new_tab_id {
                    rekeys.remove(&alias);
                } else {
                    rekeys.insert(alias, new_tab_id.to_string());
                }
            }
        }
        tracing::info!(
            target: "master_wt_event",
            old_tab_id,
            new_tab_id,
            renamed_helpers,
            retirement_rekeyed,
            "rekeyed master tab ownership after drag"
        );
        return;
    }

    if method == "tab_closed" || method == "reset_tab_session" {
        let Some(tab_id) = params.get("tab_id").and_then(|value| value.as_str()) else {
            tracing::warn!(
                target: "master_wt_event",
                method,
                "tab session close event missing tab_id"
            );
            return;
        };
        if let Err(error) = handle_close_tab_session(
            state,
            &crate::session_registry::CloseTabSessionParams {
                tab_id: tab_id.to_string(),
            },
            method == "reset_tab_session",
        )
        .await
        {
            tracing::warn!(
                target: "master_wt_event",
                method,
                tab_id,
                error = %error,
                "master-owned tab session close failed"
            );
        }
        return;
    }

    if method != "connection_state" {
        return;
    }
    // Match the helper-side fallback in `main.rs` (line ~2048): prefer
    // `pane_id`; fall back to legacy `session_id` so a hypothetical
    // older WT build still works.
    let pane_id = params
        .get("pane_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| params.get("session_id").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();
    if pane_id.is_empty() {
        return;
    }
    let pane_state = params.get("state").and_then(|v| v.as_str()).unwrap_or("");
    let event = match pane_state {
        "closed" => crate::agent_sessions::SessionEvent::PaneClosed {
            pane_session_id: pane_id.clone(),
        },
        "failed" => {
            let reason = params
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("connection failed")
                .to_string();
            crate::agent_sessions::SessionEvent::ConnectionFailed {
                pane_session_id: pane_id.clone(),
                reason,
            }
        }
        _ => return,
    };
    tracing::info!(
        target: "master_wt_event",
        pane_id = %pane_id,
        state = %pane_state,
        event = ?event,
        "applying WT connection_state event to master registry"
    );
    let applied = state.registry.apply_event(event).await;
    if applied {
        tracing::info!(
            target: "master_wt_event",
            pane_id = %pane_id,
            "broadcasting sessions/changed after WT-driven demotion"
        );
        broadcast_ext_to_helpers(
            state,
            crate::session_registry::build_sessions_changed_notification(),
        )
        .await;
    } else {
        tracing::debug!(
            target: "master_wt_event",
            pane_id = %pane_id,
            "WT connection_state event was a no-op (pane not bound to any session)"
        );
    }
}

/// Extract the session key from event variants that carry one. Returns
/// `None` for pane-only variants (PaneClosed, ConnectionFailed) — those
/// don't have a stable session id without a reverse lookup, and they
/// transition the row to a terminal state where the title doesn't need
/// refreshing anyway.
fn session_event_key(event: &crate::agent_sessions::SessionEvent) -> Option<&str> {
    use crate::agent_sessions::SessionEvent;
    match event {
        SessionEvent::SessionStarted { key, .. }
        | SessionEvent::ToolStarting { key, .. }
        | SessionEvent::ToolCompleted { key }
        | SessionEvent::Notification { key, .. }
        | SessionEvent::SessionStopped { key, .. }
        | SessionEvent::ResumeDispatched { key }
        | SessionEvent::ResumePaneAssigned { key, .. } => Some(key.as_str()),
        SessionEvent::PaneClosed { .. } | SessionEvent::ConnectionFailed { .. } => None,
    }
}

/// Upgrade every still-synthetic registry row's title from `titles`
/// (session_id → CLI title). Returns true if any row changed.
async fn refresh_synthetic_titles_from(
    reg: &dyn crate::session_registry::SessionRegistry,
    titles: &std::collections::HashMap<String, String>,
) -> bool {
    let mut changed = false;
    for row in reg.snapshot().await {
        if !crate::session_registry::title_is_synthetic(&row) {
            continue;
        }
        if let Some(title) = titles.get(row.session_id.0.as_ref()) {
            if reg.upgrade_title_if_synthetic(&row.session_id, title).await {
                changed = true;
            }
        }
    }
    changed
}

/// Whether `info`'s row can be title-refreshed from `conn_cli`'s
/// `session/list`. The agent enumerates only ITS OWN cli's sessions, so a row
/// stamped with a *different* known cli (e.g. a machine-wide watched claude
/// session while this agent is copilot) can never appear in it — skip it
/// rather than issue a per-event round-trip that can't match. A `None` cli on
/// either side is treated as "attempt" (the lookup simply no-ops when the id is
/// absent).
///
/// That leniency stays safe for the destructive caller
/// ([`refresh_titles_from_listing`], which overwrites rather than fills) because
/// the session id is the real authority: an unstamped row only gets a title when
/// the listing agent actually returned that exact id, which makes it that
/// agent's own session. Reconcile is deliberately stricter — see
/// [`is_stale_host_history_row`] — because deletion cannot be justified that way.
fn row_refreshable_by_connected_agent(
    info: &crate::session_registry::SessionInfo,
    conn_cli: Option<&crate::agent_sessions::CliSource>,
) -> bool {
    match (info.cli_source.as_ref(), conn_cli) {
        (Some(row_cli), Some(conn_cli)) => row_cli == conn_cli,
        _ => true,
    }
}

/// The pooled, already-initialized agent CLI that owns a row's sessions: same
/// provider AND same execution source. Lets row-driven paths (hooks, the
/// watcher) ask the agent that can actually answer for a row instead of
/// whichever CLI master happened to launch with — master multiplexes several at
/// once and the launch one may not even be the user's current selection.
///
/// Both halves are load-bearing. Matching the provider alone would route a
/// `Wsl { Debian }` Copilot row to the *host* Copilot agent, which enumerates a
/// different `$HOME` and can never see it — the lookup would "succeed" and then
/// silently fail to find the session. Both sides pass the provider through
/// [`stamped_cli`], so a `custom:<name>` agent is reachable by the
/// `Unknown("custom")` stamp its own rows carry.
async fn agent_for_row(
    state: &MasterStateInner,
    cli: Option<&crate::agent_sessions::CliSource>,
    location: &crate::agent_sessions::SessionLocation,
) -> Option<Arc<AgentCli>> {
    let want = stamped_cli(cli);
    let agents = state.agents.lock().await;
    agents
        .values()
        .filter_map(|cell| cell.get().cloned())
        .find(|agent| {
            stamped_cli(agent.cli_source.as_ref()) == want
                && &agent.source.session_location() == location
        })
}

/// ACP replacement for the former on-disk single-session title refresh. Cheap
/// early-out: only fetch the agent's session/list when this row is synthetic.
async fn try_refresh_title_via_acp(
    state: &MasterStateInner,
    sid: &acp::schema::v1::SessionId,
) -> bool {
    let Some(info) = state.registry.lookup(sid).await else {
        return false;
    };
    if !crate::session_registry::title_is_synthetic(&info) {
        return false;
    }
    // Ask the agent that owns this row's provider AND source. Hooks and the
    // file watcher report machine-wide across CLIs and distros, so the
    // responder is chosen per row, not per master. No pooled agent for that
    // pair means nobody can title the row right now; a later poll retries once
    // one is up.
    let Some(agent) = agent_for_row(state, info.cli_source.as_ref(), &info.location).await else {
        return false;
    };
    if !row_refreshable_by_connected_agent(&info, Some(&stamped_cli(agent.cli_source.as_ref()))) {
        return false;
    }
    let titles = host_titles_via_acp(&agent).await;
    match titles.get(sid.0.as_ref()) {
        Some(title) => state.registry.upgrade_title_if_synthetic(sid, title).await,
        None => false,
    }
}

/// Pure async handler for the `intellterm.wta/focus_session` ExtRequest.
///
/// 1. Parses `FocusSessionParams` from `params`.
/// 2. Looks the SessionId up in `state.registry`. Miss → `NotFound`.
/// 3. Requires the row to carry a `pane_session_id` (registry rows
///    created before B-3 may not). Missing → `InvalidRequest` so the
///    caller knows the row is unfocusable rather than "doesn't exist".
/// 4. Requires `state.wt` to be `Some` (CliChannel available). None →
///    a structured error; helper falls back to legacy focus path.
/// 5. Dispatches `wt.request("focus_pane", { session_id: <pane_guid> })`.
///    Wraps any wtcli failure in `internal_error` with the underlying
///    stderr-style message so the helper can log it.
///
/// Returned `ExtResponse` is `{ "ok": true, "pane_session_id": "..." }`
/// on success — the helper doesn't strictly need the echo today but it
/// makes the wire trace self-documenting and gives us room to add
/// e.g. `restored_from_stash: true` later without changing the method
/// signature.
///
/// Factored out so unit tests can exercise it with a mock `WtChannel`
/// + an `InMemoryRegistry` without standing up a `HelperHandler` /
/// agent CLI / pipe pair.
pub(crate) async fn handle_focus_session(
    state: &MasterStateInner,
    parsed: &crate::session_registry::FocusSessionParams,
) -> acp::Result<acp::schema::v1::ExtResponse> {
    let info = state
        .registry
        .lookup(&parsed.session_id)
        .await
        .ok_or_else(|| {
            tracing::info!(
                target: "master",
                op = "focus_session",
                session_id = ?parsed.session_id,
                "session not in registry; nothing to focus"
            );
            acp::Error::resource_not_found(None).data(serde_json::json!({
                "session_id": parsed.session_id,
                "reason": "session_id not in master registry"
            }))
        })?;

    let pane_session_id = info.pane_session_id.clone().ok_or_else(|| {
        tracing::warn!(
            target: "master",
            op = "focus_session",
            session_id = ?parsed.session_id,
            "registry row has no pane_session_id; cannot focus"
        );
        acp::Error::invalid_request().data(serde_json::json!({
            "session_id": parsed.session_id,
            "reason": "session has no associated WT pane"
        }))
    })?;

    let wt = state.wt.as_ref().ok_or_else(|| {
        tracing::warn!(
            target: "master",
            op = "focus_session",
            session_id = ?parsed.session_id,
            "WtChannel unavailable; helper must fall back to legacy focus"
        );
        acp::Error::internal_error().data(serde_json::json!({
            "reason": "focus channel unavailable"
        }))
    })?;

    match wt
        .request(
            "focus_pane",
            serde_json::json!({ "session_id": pane_session_id }),
        )
        .await
    {
        Ok(_) => {
            tracing::info!(
                target: "master",
                op = "focus_session",
                session_id = ?parsed.session_id,
                pane_session_id = %pane_session_id,
                "focus dispatched"
            );
            let resp_json = serde_json::json!({
                "ok": true,
                "pane_session_id": pane_session_id,
            });
            let raw = serde_json::value::to_raw_value(&resp_json)
                .expect("trivial JSON value always serializes");
            Ok(acp::schema::v1::ExtResponse::new(raw.into()))
        }
        Err(err) => {
            tracing::warn!(
                target: "master",
                op = "focus_session",
                session_id = ?parsed.session_id,
                pane_session_id = %pane_session_id,
                error = %err,
                "wtcli focus_pane failed"
            );
            Err(acp::Error::internal_error().data(serde_json::json!({
                "reason": "wtcli focus_pane failed",
                "message": err.to_string(),
            })))
        }
    }
}

async fn handle_session_resume_dispatched(
    state: &MasterStateInner,
    parsed: &crate::session_registry::SessionResumeDispatchedParams,
) -> acp::Result<acp::schema::v1::ExtResponse> {
    // TODO(Task A merge): keep this check-and-flip on the expanded reducer-owned status field.
    let (flipped, current_status) = state
        .registry
        .mark_resume_dispatched(&parsed.sid)
        .await
        .unwrap_or((false, "Idle".to_string()));
    if flipped {
        broadcast_ext_to_helpers(
            state,
            crate::session_registry::build_sessions_changed_notification(),
        )
        .await;
    }
    let body = crate::session_registry::SessionResumeDispatchedResponse {
        flipped,
        current_status,
    };
    let raw = serde_json::value::to_raw_value(&body).expect("resume response serializes");
    Ok(acp::schema::v1::ExtResponse::new(raw.into()))
}

async fn handle_session_focus(
    state: &MasterStateInner,
    parsed: &crate::session_registry::SessionFocusParams,
) -> acp::Result<acp::schema::v1::ExtResponse> {
    let Some(info) = state.registry.lookup(&parsed.sid).await else {
        let body = crate::session_registry::SessionFocusResponse {
            focused: false,
            pane_session_id: None,
            reason: Some("no_pane".to_string()),
            detail: Some("session id is not in the master registry".to_string()),
        };
        let raw = serde_json::value::to_raw_value(&body).expect("focus response serializes");
        return Ok(acp::schema::v1::ExtResponse::new(raw.into()));
    };
    let Some(pane_session_id) = info.pane_session_id.clone() else {
        let body = crate::session_registry::SessionFocusResponse {
            focused: false,
            pane_session_id: None,
            reason: Some("no_pane".to_string()),
            detail: None,
        };
        let raw = serde_json::value::to_raw_value(&body).expect("focus response serializes");
        return Ok(acp::schema::v1::ExtResponse::new(raw.into()));
    };
    let Some(wt) = state.wt.as_ref() else {
        let body = crate::session_registry::SessionFocusResponse {
            focused: false,
            pane_session_id: Some(pane_session_id),
            reason: Some("wtcli_error".to_string()),
            detail: Some("focus channel unavailable".to_string()),
        };
        let raw = serde_json::value::to_raw_value(&body).expect("focus response serializes");
        return Ok(acp::schema::v1::ExtResponse::new(raw.into()));
    };
    match wt
        .request(
            "focus_pane",
            serde_json::json!({ "session_id": pane_session_id }),
        )
        .await
    {
        Ok(_) => {
            let body = crate::session_registry::SessionFocusResponse {
                focused: true,
                pane_session_id: Some(pane_session_id),
                reason: None,
                detail: None,
            };
            let raw = serde_json::value::to_raw_value(&body).expect("focus response serializes");
            Ok(acp::schema::v1::ExtResponse::new(raw.into()))
        }
        Err(err) => {
            let detail = err.to_string();
            let not_found =
                detail.to_ascii_lowercase().contains("not found") || detail.contains("0x80070490");
            if not_found {
                let mut demoted = info;
                demoted.status = Some(crate::agent_sessions::AgentStatus::Ended);
                demoted.pane_session_id = None;
                state.registry.upsert(demoted).await;
                broadcast_ext_to_helpers(
                    state,
                    crate::session_registry::build_sessions_changed_notification(),
                )
                .await;
            }
            let body = crate::session_registry::SessionFocusResponse {
                focused: false,
                pane_session_id: None,
                reason: Some(
                    if not_found {
                        "not_found"
                    } else {
                        "wtcli_error"
                    }
                    .to_string(),
                ),
                detail: Some(detail),
            };
            let raw = serde_json::value::to_raw_value(&body).expect("focus response serializes");
            Ok(acp::schema::v1::ExtResponse::new(raw.into()))
        }
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
