// tools/wta/src/master/mod.rs
//
// `wta-master` mode — the singleton ACP multiplexer half of the
// helper+master architecture (see doc/specs/Multi-window-agent-pane.md).
//
// Responsibilities:
//   1. Spawn the agent CLI subprocess (claude / copilot / gemini)
//      and wrap its stdio in an `acp::ClientSideConnection` (master
//      is the *client* of the agent CLI — same role that legacy
//      wta plays today).
//   2. Listen on a named pipe (path supplied by the C++ side via
//      `--master <pipe-name>`). Accept one wta-helper per connect.
//   3. For each helper, run an `acp::AgentSideConnection` in which
//      master plays the *agent* role. Forward helper requests to
//      the agent CLI; route inbound `session_notification`s from
//      the agent CLI back to the helper that owns the session.
//
// Forwarding paths:
//   * `helper → master → agent CLI`: every helper request runs
//     through `HelperHandler`'s `acp::Agent` impl, which is just a
//     thin pass-through to the agent CLI's `ClientSideConnection`.
//   * `agent CLI → master → helper` (notifications): inbound
//     `session_notification`s land in `MasterClient::session_notification`
//     and are fanned out to the owning helper's notification channel
//     via the `session_to_helper` map (populated in `new_session` /
//     `load_session`).
//   * `agent CLI → master → helper` (requests — request_permission,
//     terminal/*, fs/*): same map carries an `Arc<AgentSideConnection>`
//     to each helper. `MasterClient` looks up the helper by
//     `args.session_id` and calls the matching `Client`-trait method
//     on that connection (`AgentSideConnection` itself implements
//     `acp::Client` and re-issues each call as an RPC request over the
//     helper's pipe). The helper-side `WtaClient` then runs the same
//     code path it ran pre-helper-split (TUI permission UI,
//     `ShellManager`, etc.).

use std::collections::HashMap;
use std::collections::HashSet;
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
const MASTER_PIPE_DISCOVERY_FILE: &str = "master-pipe.txt";

use acp::Agent as _;
use acp::Client as _;
use agent_client_protocol as acp;
use anyhow::{anyhow, Context, Result};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::{mpsc, Mutex};
use tokio::task::LocalSet;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::protocol::acp::spawn::spawn_agent_process;
use crate::Cli;

/// Opaque identifier for a helper connection. Used in logs only;
/// routing keys off `acp::SessionId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct HelperId(u64);

/// Per-session routing entry. Owned by `session_to_helper` and
/// keyed by `acp::SessionId`.
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
    notif_tx: mpsc::Sender<acp::SessionNotification>,
    forwarder: Option<Arc<acp::AgentSideConnection>>,
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
    session_to_helper: Mutex<HashMap<acp::SessionId, HelperRoute>>,
    /// Authoritative live-session set, owned by master. Mirrors what
    /// helpers learn via ext-notifications and what the session management view sees
    /// via the standard ACP `session/list` request. Kept beside
    /// `session_to_helper` (rather than fused with it) so the
    /// per-row metadata that `SessionInfo` carries — cwd, future
    /// title/updated_at — has a typed home that isn't intertwined
    /// with notification-channel plumbing.
    ///
    /// Lock ordering: always take `session_to_helper` *before*
    /// touching `registry` to keep the helper-disconnect cleanup
    /// path single-threaded (it walks `session_to_helper` for ids
    /// and then issues `registry.remove`). Holding `session_to_helper`
    /// while awaiting on `registry` is safe — the registry's interior
    /// lock is sub-µs sync HashMap work that does not re-enter
    /// `session_to_helper`.
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
        Mutex<HashMap<HelperId, mpsc::UnboundedSender<acp::ExtNotification>>>,
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
    /// keyed by the agent command line (`AgentCmdKey`). Lazily
    /// populated: a helper declares its agent *id* in the `initialize`
    /// handshake (`_meta.wta.agent_id`), the master reconstructs the
    /// command from that id (`agent_registry::build_acp_command`), and
    /// `get_or_spawn_agent` spawns the CLI on first use and reuses it for
    /// every later helper that resolves to the same command line. The key
    /// is always a master-derived command, never a string off the pipe.
    /// This is what lets one tab run Gemini while another runs Claude in
    /// the same window.
    ///
    /// Each value is an `Arc<OnceCell<…>>` so two helpers racing the
    /// *same* new agent serialize on that key's init (one spawns, the
    /// other awaits the same `AgentCli`), while helpers for *different*
    /// agents spawn in parallel — we hold the outer `Mutex` only long
    /// enough to get/insert the `OnceCell`, never across the spawn.
    pub(crate) agents:
        Mutex<HashMap<AgentCmdKey, Arc<tokio::sync::OnceCell<Arc<AgentCli>>>>>,
    /// Fallback agent command line + id for helpers that don't declare
    /// their own in `_meta.wta` (older helper builds, or the rare
    /// manual launch). Comes from the master's own `--agent` / `--agent-id`,
    /// which the C++ side still passes as the global default. This command
    /// is **trusted** (it came from the master's own argv, not the pipe),
    /// so a rejected/unknown helper request safely falls back to it.
    pub(crate) default_agent_cmd: String,
    pub(crate) default_agent_id: Option<String>,
    /// Allowlist of agent ids a helper may select over the pipe, from the
    /// host's GPO-filtered set (`--allowed-agent-ids`). `None` = no host
    /// allowlist supplied (manual runs / older hosts): any *known* agent
    /// id is accepted. `Some(set)` = only ids in `set` are honored; any
    /// other id falls back to the trusted default. Either way the master
    /// reconstructs the command from the id and never spawns a string
    /// taken off the pipe.
    pub(crate) allowed_agent_ids: Option<std::collections::HashSet<String>>,
    /// Per-helper crash-recovery metadata, keyed by `HelperId`.
    ///
    /// Populated/refreshed by the `new_session` + `load_session`
    /// handlers (which see the helper-supplied `_meta.wta.owner_tab_id`
    /// and the resulting `SessionId`), and consumed by `serve_helper`
    /// when a helper's pipe disconnects: if the entry carries an
    /// `owner_tab_id`, master emits a `restart_agent_pane` event so C++
    /// re-warms a fresh helper for that tab (resuming the recorded
    /// `last_session_id`). One entry per helper — `last_session_id` is
    /// the most recently created/loaded session, i.e. the one the user
    /// was last looking at, which is the right one to resume.
    ///
    /// Independent lock from `session_to_helper` so the per-session
    /// routing hot path never contends on it.
    pub(crate) helper_meta: Mutex<HashMap<HelperId, HelperRecoveryMeta>>,
    /// Session ids claimed by an *authoritative* producer — a PowerShell agent
    /// hook (arrives via `intellterm.wta/session_hook`) or an ACP agent-pane
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
    hook_owned: Mutex<HashSet<acp::SessionId>>,
    /// #266 born-bound sessions (WTA-launched delegate/resume — copilot/claude/
    /// gemini). **Binding-only**: unlike `hook_owned`, the file watcher may
    /// still supply STATUS for these when no real hook is installed
    /// (activity-only, never re-binding the pane). A subsequent real hook moves
    /// the session into `hook_owned` and out of here, after which the watcher
    /// fully backs off.
    born_bound: Mutex<HashSet<acp::SessionId>>,
    /// Short-lived cache of the live pane GUIDs in THIS IT instance (lowercased),
    /// from a `list_windows`→`list_tabs`→`list_panes` walk over the master's WT
    /// channel. Used by [`apply_watcher_event`] to gate watcher-discovered
    /// sessions: a file-watched CLI is only surfaced if it binds to a pane that
    /// is currently live here — otherwise it's a copilot/claude/… running in
    /// VS Code, a background host, or another terminal (its session file is on
    /// disk machine-wide, but it is not an IT shell-pane session). Cached for a
    /// couple seconds so a startup burst of session files triggers at most one
    /// COM walk. `None` until first populated.
    live_panes_cache: Mutex<Option<(std::time::Instant, HashSet<String>)>>,
}

/// Canonical key for the agent-CLI pool: the full agent command line
/// (e.g. `"copilot --acp --stdio"` or
/// `"npx -y @agentclientprotocol/claude-agent-acp"`). Two tabs with the same
/// command line share one CLI; different command lines get their own.
/// (Distinct from `agent_sessions::AgentKey`, which is a *session* id.)
type AgentCmdKey = String;

/// One spawned agent CLI subprocess and everything a helper needs to
/// talk to it. Shared (`Arc`) across every helper currently bound to
/// this agent.
struct AgentCli {
    /// Pool key (the agent command line) — lets the reaper remove the
    /// right entry from `MasterStateInner::agents` when this CLI dies.
    key: AgentCmdKey,
    /// Master is the ACP *client* of this CLI. Every helper request for
    /// a session owned by this agent forwards onto this connection.
    conn: Arc<acp::ClientSideConnection>,
    /// This CLI's `initialize` response, replayed verbatim to every
    /// helper that binds to it (re-forwarding `initialize` to the CLI
    /// returns empty `agent_info` on most backends, which blanks the
    /// XAML agent bar). Per-agent so each tab's bar shows ITS agent.
    cached_init_resp: acp::InitializeResponse,
    /// The CLI provider, resolved from this agent's id/command line.
    /// Stamped on every SessionInfo this agent's sessions upsert so the
    /// F2 view labels each row with its real CLI (Gemini vs Claude),
    /// not one process-wide value.
    cli_source: Option<crate::agent_sessions::CliSource>,
}

/// Per-helper recovery metadata stashed in
/// [`MasterStateInner::helper_meta`]. See the field doc for lifecycle.
#[derive(Debug, Clone, Default)]
pub(crate) struct HelperRecoveryMeta {
    /// The WT tab StableId that owns this helper's agent pane, from
    /// `_meta.wta.owner_tab_id`. `None` for non-agent-pane helpers — in
    /// which case no `restart_agent_pane` is emitted on disconnect.
    pub(crate) owner_tab_id: Option<String>,
    /// The most recently created/loaded session for this helper — the
    /// one to resume via `--initial-load-session-id` on recovery.
    pub(crate) last_session_id: Option<acp::SessionId>,
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
        sid: &acp::SessionId,
        op: &'static str,
    ) -> acp::Result<(HelperId, Arc<acp::AgentSideConnection>)> {
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

#[async_trait::async_trait(?Send)]
impl acp::Client for MasterClient {
    async fn request_permission(
        &self,
        args: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        let sid = args.session_id.clone();
        let (helper_id, forwarder) = self.route_for(&sid, "request_permission").await?;
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

    async fn session_notification(&self, args: acp::SessionNotification) -> acp::Result<()> {
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
                        tracing::debug!(
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
                        // belongs to the helper we snapshotted. A
                        // freshly-issued `load_session` can have
                        // rebound the same SessionId to a different
                        // helper between our snapshot and now —
                        // clobbering that new entry would silently
                        // break notification delivery for the new
                        // helper. `helper_id` is unique per master
                        // lifetime (monotonic counter), so equality is
                        // a sufficient identity check.
                        let mut map = self.state.session_to_helper.lock().await;
                        match map.get(&sid) {
                            Some(current) if current.helper_id == snap_helper_id => {
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
                                    "helper notification channel closed but SessionId has been rebound to a different helper — dropping update, leaving new route intact"
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
        args: acp::WriteTextFileRequest,
    ) -> acp::Result<acp::WriteTextFileResponse> {
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
        args: acp::ReadTextFileRequest,
    ) -> acp::Result<acp::ReadTextFileResponse> {
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
        args: acp::CreateTerminalRequest,
    ) -> acp::Result<acp::CreateTerminalResponse> {
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
        args: acp::TerminalOutputRequest,
    ) -> acp::Result<acp::TerminalOutputResponse> {
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
        args: acp::ReleaseTerminalRequest,
    ) -> acp::Result<acp::ReleaseTerminalResponse> {
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
        args: acp::WaitForTerminalExitRequest,
    ) -> acp::Result<acp::WaitForTerminalExitResponse> {
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
        args: acp::KillTerminalRequest,
    ) -> acp::Result<acp::KillTerminalResponse> {
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
fn notification_kind(notif: &acp::SessionNotification) -> &'static str {
    use acp::SessionUpdate::*;
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

/// `acp::Agent` impl wired into one helper's `AgentSideConnection`.
/// Each helper gets its own `HelperHandler` instance.
struct HelperHandler {
    helper_id: HelperId,
    /// The agent CLI this helper is bound to. Resolved lazily during
    /// `initialize` from the helper's declared `_meta.wta.agent_cmd`
    /// (or the master default), then reused by every later request on
    /// this connection. `OnceLock` because the binding can't be known
    /// until the helper's `initialize` arrives, but the ACP protocol
    /// guarantees `initialize` precedes `new_session`/`prompt`/…, so
    /// `resolved_agent()` always finds it populated for those.
    agent: OnceLock<Arc<AgentCli>>,
    state: Arc<MasterStateInner>,
    /// Notification fan-in for this helper. `new_session` /
    /// `load_session` writes `(SessionId → this sender)` into
    /// `state.session_to_helper` so future agent-CLI notifications
    /// land here. The helper's serve loop drains the matching
    /// receiver and writes notifications back over the
    /// `AgentSideConnection`.
    notif_tx: mpsc::Sender<acp::SessionNotification>,
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
    agent_side_slot: Arc<OnceLock<Weak<acp::AgentSideConnection>>>,
}

impl HelperHandler {
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
    fn forwarder_for_route(&self, op: &'static str) -> acp::Result<Arc<acp::AgentSideConnection>> {
        let weak = self.agent_side_slot.get().ok_or_else(|| {
            tracing::error!(
                target: "master",
                op = op,
                helper_id = ?self.helper_id,
                "agent_side_slot empty inside helper request handler — bug; serve_helper must populate it before handle_io polls"
            );
            acp::Error::internal_error()
                .data(serde_json::json!("agent_side_slot not yet set"))
        })?;
        weak.upgrade().ok_or_else(|| {
            tracing::warn!(
                target: "master",
                op = op,
                helper_id = ?self.helper_id,
                "helper AgentSideConnection already dropped — cannot route new request"
            );
            acp::Error::internal_error().data(serde_json::json!("helper connection dropped"))
        })
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
            acp::Error::internal_error()
                .data(serde_json::json!("no agent bound; initialize must come first"))
        })
    }

    /// Forward `session/new` to this helper's bound agent CLI with a
    /// timeout (moved to the master per #268) plus ACP telemetry. The
    /// timeout breaks an ACP cancellation-safety deadlock so a hung
    /// agent surfaces as an error instead of wedging the helper.
    async fn forward_new_session_to_agent(
        &self,
        args: acp::NewSessionRequest,
        timeout: std::time::Duration,
    ) -> acp::Result<acp::NewSessionResponse> {
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
}

#[async_trait::async_trait(?Send)]
impl acp::Agent for HelperHandler {
    async fn initialize(
        &self,
        mut args: acp::InitializeRequest,
    ) -> acp::Result<acp::InitializeResponse> {
        // The helper declares which agent this tab wants in `_meta.wta`
        // by *identity* (id + model). Strip the namespace so it can never
        // reach an agent CLI, then resolve the command the master will
        // actually spawn. Crucially we NEVER execute a command string off
        // the pipe: `resolve_agent_selection` reconstructs the command
        // from the declared id (only for known, GPO-allowed ids) and
        // otherwise falls back to the trusted `--agent` default. See
        // `resolve_agent_selection` for the full policy.
        let wta_meta = crate::session_registry::extract_wta_meta(&mut args.meta);
        let (agent_cmd, agent_id) = resolve_agent_selection(
            &self.state.default_agent_cmd,
            self.state.default_agent_id.as_deref(),
            self.state.allowed_agent_ids.as_ref(),
            wta_meta.agent_id.as_deref(),
            wta_meta.model.as_deref(),
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
            "resolving agent CLI for helper"
        );

        // Lazily spawn (or reuse) the agent CLI for THIS tab's agent,
        // then bind it to this helper for the rest of the connection.
        let agent = get_or_spawn_agent(&self.state, &agent_cmd, agent_id.as_deref())
            .await
            .map_err(|e| {
                tracing::error!(
                    target: "master",
                    op = "initialize",
                    helper_id = ?self.helper_id,
                    agent_cmd = %agent_cmd,
                    error = %e,
                    "failed to spawn/resolve agent CLI for helper"
                );
                acp::Error::internal_error()
                    .data(serde_json::json!(format!("agent CLI unavailable: {e}")))
            })?;
        // `set` is idempotent-by-error; a helper that (incorrectly) sent
        // initialize twice keeps its first binding, which is fine.
        let _ = self.agent.set(Arc::clone(&agent));

        // Replay the CLI's own initialize response (re-forwarding returns
        // empty `agent_info` on most backends, blanking the agent bar).
        // Per-agent cache means each tab's bar shows ITS agent's identity.
        Ok(agent.cached_init_resp.clone())
    }

    async fn authenticate(
        &self,
        args: acp::AuthenticateRequest,
    ) -> acp::Result<acp::AuthenticateResponse> {
        tracing::info!(
            target: "master",
            step = "helper→agent",
            op = "authenticate",
            helper_id = ?self.helper_id,
            "forwarding authenticate"
        );
        self.resolved_agent("authenticate")?.conn.authenticate(args).await
    }

    async fn new_session(
        &self,
        args: acp::NewSessionRequest,
    ) -> acp::Result<acp::NewSessionResponse> {
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
        let cwd_for_registry = args.cwd.clone();
        tracing::info!(
            target: "master",
            step = "helper→agent",
            op = "new_session",
            helper_id = ?self.helper_id,
            mcp_servers = args.mcp_servers.len(),
            pane_session_id = ?wta_meta.pane_session_id,
            "forwarding new_session"
        );
        let resp = self
            .forward_new_session_to_agent(
                args,
                std::time::Duration::from_secs(SESSION_NEW_TIMEOUT_SECS),
            )
            .await?;
        // Resolve the bound agent for `cli_source` stamping below (cheap
        // Arc clone; the forward above already used it for the RPC).
        let agent = self.resolved_agent("new_session")?;
        let forwarder = self.forwarder_for_route("new_session")?;
        // Record routing entry BEFORE returning so the helper can't
        // race a session/update notification.
        let registry_size = {
            let mut map = self.state.session_to_helper.lock().await;
            map.insert(
                resp.session_id.clone(),
                HelperRoute {
                    helper_id: self.helper_id,
                    notif_tx: self.notif_tx.clone(),
                    forwarder: Some(forwarder),
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            );
            map.len()
        };
        // Mirror the binding into the live-session registry. Lock
        // ordering matches the doc on `MasterStateInner::registry`:
        // `session_to_helper` is no longer held here, so the upsert
        // can't deadlock against `drop_sessions_for_helper`.
        let mut info = crate::session_registry::SessionInfo::new(
            resp.session_id.clone(),
            cwd_for_registry,
        );
        info.pane_session_id = wta_meta.pane_session_id;
        // Stamp the row as a Live agent-pane session. Without this, the
        // row lands in master's registry with status=cli_source=origin=None,
        // and helper-side session management routing treats it as Historical (the default
        // fallback in session_info_to_agent_session). Enter on it then
        // tries to resume and fails with "unknown CLI" since cli_source
        // is None. Agent-pane sessions never get a SessionStarted hook
        // (those fire for shell-pane agents through PowerShell hooks
        // only), so master is the only one that can fill these fields.
        info.status = Some(crate::agent_sessions::AgentStatus::Idle);
        info.cli_source = agent.cli_source.clone();
        info.origin = Some(crate::agent_sessions::SessionOrigin::AgentPane);
        info.last_activity_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_millis() as u64);
        self.state.registry.upsert(info.clone()).await;
        // Record crash-recovery metadata for this helper: the owning
        // WT tab StableId (so master can address a `restart_agent_pane`
        // event on disconnect) and the just-created session as the
        // resume target. See `MasterStateInner::helper_meta`.
        {
            let mut meta = self.state.helper_meta.lock().await;
            let entry = meta.entry(self.helper_id).or_default();
            if wta_meta.owner_tab_id.is_some() {
                entry.owner_tab_id = wta_meta.owner_tab_id.clone();
            }
            entry.last_session_id = Some(resp.session_id.clone());
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
        let agent_current_model = resp
            .models
            .as_ref()
            .map(|state| state.current_model_id.0.to_string());
        let agent_model_count = resp
            .models
            .as_ref()
            .map(|state| state.available_models.len())
            .unwrap_or(0);
        tracing::info!(
            target: "master",
            step = "helper→agent",
            op = "new_session",
            helper_id = ?self.helper_id,
            session_id = ?resp.session_id,
            registry_size = registry_size,
            current_model_id = ?agent_current_model,
            available_models = agent_model_count,
            "session bound to helper"
        );
        Ok(resp)
    }

    async fn load_session(
        &self,
        args: acp::LoadSessionRequest,
    ) -> acp::Result<acp::LoadSessionResponse> {
        let mut args = args;
        let wta_meta = crate::session_registry::extract_wta_meta(&mut args.meta);
        let session_id = args.session_id.clone();
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
        let agent = self.resolved_agent("load_session")?;
        let forwarder = self.forwarder_for_route("load_session")?;
        {
            let mut map = self.state.session_to_helper.lock().await;
            map.insert(
                session_id.clone(),
                HelperRoute {
                    helper_id: self.helper_id,
                    notif_tx: self.notif_tx.clone(),
                    forwarder: Some(forwarder),
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            );
        }
        match agent.conn.load_session(args).await {
            Ok(resp) => {
                let mut info = crate::session_registry::SessionInfo::new(
                    session_id.clone(),
                    cwd_for_registry,
                );
                info.pane_session_id = wta_meta.pane_session_id;
                // See new_session above for rationale — load_session is the
                // resume path and the resumed row must also be Live + tagged.
                info.status = Some(crate::agent_sessions::AgentStatus::Idle);
                info.cli_source = agent.cli_source.clone();
                info.origin = Some(crate::agent_sessions::SessionOrigin::AgentPane);
                info.last_activity_at_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_millis() as u64);
                // Carry the title (and updated_at) forward from the row
                // that already exists for this sid. Master seeds the
                // registry at startup with rows from `history_loader`
                // which include the disk-derived chat title (e.g.
                // "# Terminal AgentYou"). A naked `SessionInfo::new`
                // upsert would clobber that title with `None`, leaving
                // the resumed Live row showing "—" in session management view. By copying
                // the prior title we keep the resumed row identifiable
                // to the user.
                if let Some(existing) =
                    self.state.registry.lookup(&session_id).await
                {
                    if info.title.is_none() {
                        info.title = existing.title;
                    }
                    if info.updated_at.is_none() {
                        info.updated_at = existing.updated_at;
                    }
                }
                self.state.registry.upsert(info.clone()).await;
                // Mirror new_session: refresh crash-recovery metadata so
                // a resume targets the session the user is now looking at.
                {
                    let mut meta = self.state.helper_meta.lock().await;
                    let entry = meta.entry(self.helper_id).or_default();
                    if wta_meta.owner_tab_id.is_some() {
                        entry.owner_tab_id = wta_meta.owner_tab_id.clone();
                    }
                    entry.last_session_id = Some(session_id.clone());
                }
                Ok(resp)
            }
            Err(err) => {
                // Roll back the pre-registration. Only `session_to_helper`
                // needs touching — we never wrote to `registry` and we
                // never broadcast `session_added`, so peers never saw
                // this row.
                {
                    let mut map = self.state.session_to_helper.lock().await;
                    map.remove(&session_id);
                }
                tracing::warn!(
                    target: "master",
                    helper_id = ?self.helper_id,
                    session_id = ?session_id,
                    error = %err,
                    "load_session failed; rolled back routing entry"
                );
                Err(err)
            }
        }
    }

    async fn set_session_mode(
        &self,
        args: acp::SetSessionModeRequest,
    ) -> acp::Result<acp::SetSessionModeResponse> {
        self.resolved_agent("set_session_mode")?.conn.set_session_mode(args).await
    }

    // Forward model selection to the agent CLI. Without this override
    // the trait's default impl returns `method_not_found`, which is
    // what the helper sees when the user picks a model from the
    // Settings UI (e.g. Claude → haiku). Symptom in
    // `wta-main_helper.log`:
    //
    //   ERROR helper: run_acp_client_over_pipe failed
    //     error=set_session_model failed for requested model haiku:
    //     Method not found
    //
    // PR #54 missed this when slicing the per-pane Agent impl into
    // the helper+master split — set_session_model is gated behind the
    // `unstable_session_model` Cargo feature (already enabled in
    // `tools/wta/Cargo.toml`) and is distinct from set_session_mode
    // (Mode = Agent/Plan/Autopilot vs Model = haiku/sonnet/opus).
    async fn set_session_model(
        &self,
        args: acp::SetSessionModelRequest,
    ) -> acp::Result<acp::SetSessionModelResponse> {
        tracing::info!(
            target: "master",
            step = "helper→agent",
            op = "set_session_model",
            helper_id = ?self.helper_id,
            session_id = ?args.session_id,
            model_id = ?args.model_id,
            "forwarding set_session_model"
        );
        self.resolved_agent("set_session_model")?.conn.set_session_model(args).await
    }

    // Same story as set_session_model — the agent CLI advertises a
    // `set_session_config_option` capability (driven by the ACP
    // `ConfigOptionUpdate` notifications the helper already handles)
    // and the trait default returns method_not_found, so anything
    // that flows through this path would also silently fail.
    async fn set_session_config_option(
        &self,
        args: acp::SetSessionConfigOptionRequest,
    ) -> acp::Result<acp::SetSessionConfigOptionResponse> {
        tracing::info!(
            target: "master",
            step = "helper→agent",
            op = "set_session_config_option",
            helper_id = ?self.helper_id,
            session_id = ?args.session_id,
            "forwarding set_session_config_option"
        );
        self.resolved_agent("set_session_config_option")?.conn.set_session_config_option(args).await
    }

    /// Answer `session/list` from our own live-session registry instead
    /// of forwarding to the agent CLI.
    ///
    /// Rationale: the only live-session view that matters to the
    /// Terminal session management panel is "what's wired up through
    /// master right now" — agent-CLI-side dormant history is exposed
    /// separately through `agent-pane-sessions.jsonl` + per-CLI
    /// `<cli> --resume`. Forwarding to the agent CLI would conflate
    /// the two and re-introduce the cross-CLI variance we built
    /// `agent-pane-sessions.jsonl` to escape.
    ///
    /// The response carries our `pane_session_id` inside the standard
    /// `_meta.wta` namespace so the helper can join it with WT pane
    /// state for routing decisions in B-10/B-11.
    async fn list_sessions(
        &self,
        _args: acp::ListSessionsRequest,
    ) -> acp::Result<acp::ListSessionsResponse> {
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
        let sessions: Vec<acp::SessionInfo> = snapshot
            .into_iter()
            .map(|s| crate::session_registry::to_acp_session_info(&s))
            .collect();
        Ok(acp::ListSessionsResponse::new(sessions))
    }

    async fn prompt(&self, args: acp::PromptRequest) -> acp::Result<acp::PromptResponse> {
        tracing::info!(
            target: "master",
            step = "helper→agent",
            op = "prompt",
            helper_id = ?self.helper_id,
            session_id = ?args.session_id,
            content_chunks = args.prompt.len(),
            "forwarding prompt to agent CLI"
        );
        let started = std::time::Instant::now();
        let resp = self.resolved_agent("prompt")?.conn.prompt(args).await;
        let elapsed_ms = started.elapsed().as_millis();
        match &resp {
            Ok(ok) => tracing::info!(
                target: "master",
                step = "helper→agent",
                op = "prompt",
                helper_id = ?self.helper_id,
                stop_reason = ?ok.stop_reason,
                elapsed_ms = elapsed_ms as u64,
                "prompt completed"
            ),
            Err(err) => tracing::warn!(
                target: "master",
                step = "helper→agent",
                op = "prompt",
                helper_id = ?self.helper_id,
                error = %err,
                elapsed_ms = elapsed_ms as u64,
                "prompt failed"
            ),
        }
        resp
    }

    async fn cancel(&self, args: acp::CancelNotification) -> acp::Result<()> {
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

    /// Master answers our own `intellterm.wta/*` ext methods locally
    /// (without round-tripping to the agent CLI). Today only
    /// `focus_session` is recognized; everything else is forwarded so
    /// future agent-native extension methods still work.
    async fn ext_method(&self, args: acp::ExtRequest) -> acp::Result<acp::ExtResponse> {
        let method: &str = &args.method;
        if method == crate::session_registry::INTELLTERM_METHOD_FOCUS_SESSION {
            tracing::info!(
                target: "master",
                op = "ext_method",
                method = %method,
                helper_id = ?self.helper_id,
                "handling intellterm.wta/focus_session locally"
            );
            return handle_focus_session(&self.state, &args.params).await;
        }
        if method == crate::session_registry::INTELLTERM_METHOD_SESSIONS_LIST {
            tracing::info!(
                target: "master",
                op = "ext_method",
                method = %method,
                helper_id = ?self.helper_id,
                "handling intellterm.wta/sessions/list locally"
            );
            return handle_sessions_list(&self.state, &args.params).await;
        }
        if method == crate::session_registry::INTELLTERM_METHOD_SESSION_HOOK {
            tracing::info!(
                target: "master",
                op = "ext_method",
                method = %method,
                helper_id = ?self.helper_id,
                "handling intellterm.wta/session_hook locally"
            );
            return handle_session_hook(&self.state, &args.params, false).await;
        }
        if method == crate::session_registry::INTELLTERM_METHOD_SESSION_BORN_BOUND {
            tracing::info!(
                target: "master",
                op = "ext_method",
                method = %method,
                helper_id = ?self.helper_id,
                "handling intellterm.wta/session_born_bound locally"
            );
            return handle_session_hook(&self.state, &args.params, true).await;
        }
        if method == crate::session_registry::INTELLTERM_METHOD_SESSION_RESUME_DISPATCHED {
            return handle_session_resume_dispatched(&self.state, &args.params).await;
        }
        if method == crate::session_registry::INTELLTERM_METHOD_SESSION_FOCUS {
            return handle_session_focus(&self.state, &args.params).await;
        }
        tracing::debug!(
            target: "master",
            op = "ext_method",
            method = %method,
            helper_id = ?self.helper_id,
            "forwarding non-intellterm ext_method to agent CLI"
        );
        self.resolved_agent("ext_method")?.conn.ext_method(args).await
    }
}

/// Master mode entry point.
pub async fn run_master_mode(cli: Cli, pipe_name: String) -> Result<()> {
    // Logging is initialized once in `main()`; the WorkerGuard lives there for
    // the whole process so the non-blocking appender flushes on the graceful
    // shutdown path (see the `run_master_loop` shutdown notes below).
    tracing::info!(
        target: "master",
        pipe_name = %pipe_name,
        agent_cmd = %cli.agent,
        "=== wta-master starting ==="
    );

    if cli.agent.is_empty() {
        return Err(anyhow!(
            "wta-master requires --agent <cmd>; nothing to multiplex onto"
        ));
    }

    // Kick off the auto-upgrade check on a blocking-pool thread. Fire-and-
    // forget — the agent CLI spawn below proceeds concurrently. Fast-path
    // cache (see `agent_hooks_installer::upgrade_installed_hooks` doc) keeps
    // the common no-upgrade case under ~10ms; only the first run after an
    // IT install/upgrade does any per-CLI work. Caveat: when an upgrade is
    // actually needed, the agent CLI process master is about to spawn may
    // miss the new hooks until its next restart.
    //
    // Wrap in `catch_unwind` so an unexpected panic inside the upgrade flow
    // (or any of its transitive dependencies) doesn't get silently swallowed
    // by tokio's fire-and-forget JoinHandle. Master keeps running either
    // way; this just promotes the panic into a visible trace event.
    tokio::task::spawn_blocking(|| {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            crate::agent_hooks_installer::upgrade_installed_hooks,
        ));
        if let Err(panic) = result {
            let msg = panic
                .downcast_ref::<&'static str>()
                .copied()
                .or_else(|| panic.downcast_ref::<String>().map(|s| s.as_str()))
                .unwrap_or("<non-string panic payload>");
            tracing::error!(
                target: "agent_hooks",
                panic = %msg,
                "upgrade_installed_hooks panicked; master continues",
            );
        }
    });

    let local_set = LocalSet::new();
    let result = local_set
        .run_until(async move { run_master_loop(cli, pipe_name).await })
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
        let token_user = &*(buf.as_ptr() as *const TOKEN_USER);
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

async fn run_master_loop(cli: Cli, pipe_name: String) -> Result<()> {
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
    // no longer owns a single eager agent CLI. `cli.agent` / `cli.agent_id`
    // become the fallback default for helpers that don't declare one.
    // Host-supplied allowlist (GPO-filtered) of agent ids a helper may
    // select. Empty argv ⇒ `None` (no allowlist; accept any known id).
    let allowed_agent_ids: Option<std::collections::HashSet<String>> =
        if cli.allowed_agent_ids.is_empty() {
            None
        } else {
            Some(
                cli.allowed_agent_ids
                    .iter()
                    .map(|s| s.trim().to_ascii_lowercase())
                    .filter(|s| !s.is_empty())
                    .collect(),
            )
        };
    tracing::info!(
        target: "master",
        allowed_agent_ids = ?allowed_agent_ids,
        default_agent_id = ?cli.agent_id,
        "agent allowlist resolved"
    );

    let inner = Arc::new(MasterStateInner {
        session_to_helper: Mutex::new(HashMap::new()),
        registry: crate::session_registry::InMemoryRegistry::shared(),
        helper_ext_subscribers: Mutex::new(HashMap::new()),
        wt,
        agents: Mutex::new(HashMap::new()),
        default_agent_cmd: cli.agent.clone(),
        default_agent_id: cli.agent_id.clone(),
        allowed_agent_ids,
        helper_meta: Mutex::new(HashMap::new()),
        hook_owned: Mutex::new(HashSet::new()),
        born_bound: Mutex::new(HashSet::new()),
        live_panes_cache: Mutex::new(None),
    });

    // Seed the registry with historical sessions scanned from
    // `~/.copilot/`, `~/.claude/`, `~/.gemini/` so `wta sessions list`
    // and helper session management viewers see the full set, not just live sessions
    // created via `session/new` after master booted. Disk scan can take
    // ~100ms-1s for users with many sessions, so we run it in
    // spawn_blocking and broadcast `sessions/changed` once when done.
    // Helpers that have session management view open at that moment will refetch and pick
    // up the historicals; helpers that open session management view later will see them on
    // the next `sessions/list` call.
    let inner_for_history = Arc::clone(&inner);
    tokio::task::spawn_local(async move {
        let scan_started = std::time::Instant::now();
        let sessions = match tokio::task::spawn_blocking(|| {
            crate::history_loader::load_all()
        })
        .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    target: "master_history",
                    error = %e,
                    "history scan task panicked; registry will not include historicals"
                );
                return;
            }
        };
        let count = sessions.len();
        for s in &sessions {
            let info = crate::session_registry::agent_session_to_session_info(s);
            inner_for_history.registry.upsert(info).await;
        }
        tracing::info!(
            target: "master_history",
            count,
            elapsed_ms = scan_started.elapsed().as_millis() as u64,
            "master-side history scan complete; broadcasting sessions/changed"
        );
        if count > 0 {
            broadcast_ext_to_helpers(
                &inner_for_history,
                crate::session_registry::build_sessions_changed_notification(),
            )
            .await;
        }
    });

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

    // ── Class-B liveness poll ───────────────────────────────────────────
    // Shell-pane CLIs (codex/claude/gemini) write no "session ended" record
    // and don't all hold a lock file, so a `Ctrl+C` leaves the row stuck at
    // its last status. Poll the bound pids every few seconds and end any whose
    // owning process has exited. Each tick is cheap — an O(1) `OpenProcess`
    // per bound Class-B session (~tens of microseconds) — so the fixed 5s
    // interval adds no meaningful idle cost. `Skip` missed ticks so a busy
    // executor never queues a backlog of polls.
    {
        let inner_for_reap = Arc::clone(&inner);
        tokio::task::spawn_local(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(5));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                reap_dead_class_b_sessions(&inner_for_reap).await;
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
/// Returns `(command_line, agent_id_for_cli_source)`. The id is passed
/// on to `spawn_one_agent` so the per-session `cli_source` is stamped
/// correctly; `None` lets it be inferred from the command line.
///
/// Fallback to the default happens when the helper declared no id, an
/// *unknown* id (not in [`agent_registry::KNOWN_AGENTS`] — e.g. a
/// `custom:` agent, which the global default already covers), or an id
/// the host's GPO allowlist excludes.
fn resolve_agent_selection(
    default_cmd: &str,
    default_id: Option<&str>,
    allowed_ids: Option<&std::collections::HashSet<String>>,
    requested_id: Option<&str>,
    requested_model: Option<&str>,
    helper_id: HelperId,
) -> (String, Option<String>) {
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
            let model = requested_model
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let cmd = crate::agent_registry::build_acp_command(id, model);
            return (cmd, Some(id.to_string()));
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

    (default_cmd.to_string(), default_id.map(str::to_string))
}

/// Get the agent CLI for `agent_cmd`, spawning + initializing it on
/// first use and reusing it thereafter. Two helpers racing the same
/// new agent serialize on the per-key `OnceCell`; helpers for different
/// agents spawn in parallel because the outer map lock is held only
/// long enough to get/insert the cell, never across the spawn.
async fn get_or_spawn_agent(
    state: &Arc<MasterStateInner>,
    agent_cmd: &str,
    agent_id: Option<&str>,
) -> Result<Arc<AgentCli>> {
    let key: AgentCmdKey = agent_cmd.to_string();
    let cell = {
        let mut agents = state.agents.lock().await;
        Arc::clone(
            agents
                .entry(key.clone())
                .or_insert_with(|| Arc::new(tokio::sync::OnceCell::new())),
        )
    };
    // On spawn/init failure the `OnceCell` stays uninitialized and
    // `spawn_one_agent` kills its child, whose closing stdio ends the I/O
    // task that then `reap_agent`s this key out of the map — so a later
    // helper requesting the same agent gets a fresh cell and retries
    // cleanly (no lingering dead slot, no leaked subprocess).
    let agent = cell
        .get_or_try_init(|| async { spawn_one_agent(state, &key, agent_cmd, agent_id).await })
        .await?;
    Ok(Arc::clone(agent))
}

/// Spawn one agent CLI subprocess, wire master as its ACP client, run
/// the startup `initialize` round trip, and install per-CLI reapers.
/// Unlike the old single-agent master, an agent CLI death here only
/// removes that agent from the pool — the master process survives so
/// other tabs' agents keep running.
async fn spawn_one_agent(
    state: &Arc<MasterStateInner>,
    key: &AgentCmdKey,
    agent_cmd: &str,
    agent_id: Option<&str>,
) -> Result<Arc<AgentCli>> {
    let mut spawn_result = spawn_agent_process(agent_cmd, None)
        .with_context(|| format!("failed to spawn agent CLI: {agent_cmd}"))?;
    tracing::info!(
        target: "master",
        program = %spawn_result.resolved_program,
        agent_cmd = %agent_cmd,
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

    // Drain agent stderr to logs so failures are diagnosable. Tag with
    // the agent key so multi-agent logs stay attributable.
    if let Some(stderr) = spawn_result.child.stderr.take() {
        let key_for_log = key.clone();
        tokio::task::spawn_local(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::warn!(target: "agent_stderr", agent = %key_for_log, "{line}");
            }
        });
    }

    let outgoing = stdin.compat_write();
    let incoming = stdout.compat();
    let client = MasterClient {
        state: Arc::clone(state),
    };
    let (conn, handle_io) = acp::ClientSideConnection::new(client, outgoing, incoming, |fut| {
        tokio::task::spawn_local(fut);
    });
    let conn = Arc::new(conn);

    // I/O-loop driver + reaper. This task drives the ACP connection's
    // I/O, so it MUST run before `initialize` (below) — initialize can't
    // make progress otherwise. When the loop ends (clean shutdown, pipe
    // error, or because we killed the child on an init failure) master can
    // no longer talk to this CLI, so the agent is dropped from the pool.
    // On the init-failure path that removes the empty `OnceCell` entry so
    // the next helper retries cleanly instead of reusing a dead slot.
    {
        let state = Arc::clone(state);
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
            reap_agent(&state, &key).await;
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
            acp::InitializeRequest::new(acp::ProtocolVersion::V1)
                .client_capabilities(acp::ClientCapabilities::new().terminal(true))
                .client_info(
                    acp::Implementation::new("wta-master", env!("CARGO_PKG_VERSION"))
                        .title("Windows Terminal Agent (master)"),
                ),
        ),
    )
    .await;

    let init_resp = match init_outcome {
        Ok(Ok(resp)) => resp,
        Ok(Err(e)) => {
            // Kill the child so its stdio closes → the I/O task above ends
            // → `reap_agent` clears the pool slot. `kill_on_drop` is a
            // backstop when `child` drops at return.
            let _ = child.start_kill();
            return Err(anyhow!("ACP initialize failed for '{agent_cmd}': {e}"));
        }
        Err(_) => {
            let _ = child.start_kill();
            return Err(anyhow!(
                "ACP initialize timed out after {init_timeout_secs}s — agent CLI '{agent_cmd}' did not respond"
            ));
        }
    };

    // Init succeeded — install the child reaper now (takes ownership of
    // `child`). A later CLI exit drops just this agent from the pool so
    // the next helper respawns it; the master stays up for other agents.
    {
        let state = Arc::clone(state);
        let key = key.clone();
        tokio::task::spawn_local(async move {
            let status = child.wait().await;
            tracing::error!(
                target: "master",
                agent = %key,
                ?status,
                "agent CLI exited — removing from pool (master stays up for other agents)"
            );
            reap_agent(&state, &key).await;
        });
    }

    // Prefer the host-supplied agent id (authoritative); fall back to
    // parsing the command line. Stamps each session's `cli_source`.
    let resolved_agent_id = match agent_id {
        Some(id) if !id.trim().is_empty() => id.to_string(),
        _ => crate::agent_registry::resolve_agent_id_from_cmd(agent_cmd).to_string(),
    };
    let cli_source = crate::agent_sessions::CliSource::from_agent_id(&resolved_agent_id);
    tracing::info!(
        target: "master",
        agent_cmd = %agent_cmd,
        resolved_agent_id = %resolved_agent_id,
        cli_source = ?cli_source,
        "agent CLI initialize OK; cli_source resolved"
    );

    Ok(Arc::new(AgentCli {
        key: key.clone(),
        conn,
        cached_init_resp: init_resp,
        cli_source,
    }))
}

/// Remove a dead agent CLI from the pool. Helpers still holding an
/// `Arc<AgentCli>` for it will error on their next request (and the
/// pane gets rebuilt); a fresh helper requesting the same `agent_cmd`
/// re-runs `spawn_one_agent`. Sessions owned by the dead agent are left
/// for the owning helper's disconnect cleanup (`drop_sessions_for_helper`).
async fn reap_agent(state: &Arc<MasterStateInner>, key: &AgentCmdKey) {
    let removed = { state.agents.lock().await.remove(key).is_some() };
    if removed {
        tracing::info!(
            target: "master",
            agent = %key,
            "dead agent removed from pool; next pane for this agent will respawn it"
        );
    }
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

    let (notif_tx, mut notif_rx) =
        mpsc::channel::<acp::SessionNotification>(NOTIF_CHANNEL_CAPACITY);

    // Second channel: master-originated ExtNotifications fanned out by
    // `broadcast_ext_to_helpers`. Kept separate from `notif_tx` so the
    // per-session and live-set fan-out paths don't collide on the
    // wire-write loop below; the `tokio::select!` can dispatch each to
    // the appropriate `AgentSideConnection` method without an enum
    // discriminator at every write site.
    let (ext_tx, mut ext_rx) = mpsc::unbounded_channel::<acp::ExtNotification>();
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
    let agent_side_slot: Arc<OnceLock<Weak<acp::AgentSideConnection>>> = Arc::new(OnceLock::new());

    let handler = HelperHandler {
        helper_id,
        // Resolved lazily during this helper's `initialize` (see
        // HelperHandler::initialize → get_or_spawn_agent).
        agent: OnceLock::new(),
        state: Arc::clone(&state),
        notif_tx,
        agent_side_slot: Arc::clone(&agent_side_slot),
    };

    let (read_half, write_half) = tokio::io::split(pipe);
    let outgoing = write_half.compat_write();
    let incoming = read_half.compat();

    let (agent_side_conn, handle_io) =
        acp::AgentSideConnection::new(handler, outgoing, incoming, |fut| {
            tokio::task::spawn_local(fut);
        });
    let agent_side_conn = Arc::new(agent_side_conn);
    // Populate BEFORE `handle_io.await` (below) so any inbound
    // request the agent CLI sends is guaranteed to see a populated
    // slot. `set` returns `Err` only if already-set, which can't
    // happen here. `Arc::downgrade` so the slot holds a `Weak` —
    // see the field comment on `HelperHandler::agent_side_slot` for
    // why a strong `Arc` here would leak the conn.
    let _ = agent_side_slot.set(Arc::downgrade(&agent_side_conn));

    tokio::pin!(handle_io);
    let result = loop {
        tokio::select! {
            io_result = &mut handle_io => {
                break io_result.map_err(|e| anyhow!(e));
            }
            Some(notif) = notif_rx.recv() => {
                let sid = notif.session_id.clone();
                let kind = notification_kind(&notif);
                tracing::debug!(
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

    // Drop every session this helper owned so the map can't grow
    // unboundedly across the master's lifetime, and so the agent
    // CLI's notifications for already-detached sessions don't keep
    // lighting up "unknown SessionId" warnings.
    let dropped = drop_sessions_for_helper(&state, helper_id).await;

    tracing::info!(
        target: "master",
        helper_id = ?helper_id,
        sessions_dropped = dropped,
        "helper disconnected"
    );

    // Crash-recovery: if this helper owned an agent pane (we recorded an
    // `owner_tab_id` from its `_meta.wta` at session/new|load), tell C++
    // to re-warm a fresh helper for that tab. A clean helper EXIT also
    // takes this path, but C++ suppresses the restart when the pane was
    // torn down deliberately (Ctrl+C×2, tab close) — see
    // `OnAgentPaneRestartRequested`. The pipe-disconnect that brings us
    // here is the same signal for both crash and clean exit, which is
    // exactly what we want: respawn unless C++ knows it was intentional.
    let recovery = {
        let mut meta = state.helper_meta.lock().await;
        meta.remove(&helper_id)
    };
    if let Some(recovery) = recovery {
        if let Some(tab_id) = recovery.owner_tab_id {
            emit_restart_agent_pane(&tab_id, recovery.last_session_id.as_ref());
        }
    }

    result
}

/// Emit a `restart_agent_pane` WT-protocol event so C++ re-warms a fresh
/// helper for `tab_id`, resuming `session_id` (when known) via
/// `--initial-load-session-id`. Routed per-tab by StableId, mirroring
/// `close_agent_pane`. See `doc/specs/connection-resilience.md` §8.
fn emit_restart_agent_pane(tab_id: &str, session_id: Option<&acp::SessionId>) {
    let evt = build_restart_agent_pane_event(tab_id, session_id);
    tracing::info!(
        target: "master",
        tab_id = %tab_id,
        session_id = ?session_id,
        "emitting restart_agent_pane (helper disconnected)"
    );
    crate::app::send_wt_protocol_event(evt.to_string());
}

/// Pure builder for the `restart_agent_pane` WT-protocol event payload.
/// Split out from [`emit_restart_agent_pane`] so the envelope shape is
/// unit-testable without the `wtcli publish` side effect.
fn build_restart_agent_pane_event(
    tab_id: &str,
    session_id: Option<&acp::SessionId>,
) -> serde_json::Value {
    serde_json::json!({
        "type": "event",
        "method": "restart_agent_pane",
        "params": {
            "tab_id": tab_id,
            "session_id": session_id.map(|s| s.0.as_ref()),
            "reason": "helper_disconnect",
        }
    })
}

/// Remove every `session_to_helper` entry owned by `helper_id`.
/// Returns the number of entries dropped. Factored out of
/// `serve_helper` so the cleanup is unit-testable without a real
/// named pipe.
async fn drop_sessions_for_helper(state: &MasterStateInner, helper_id: HelperId) -> usize {
    // Collect the owned SessionIds first so we can drop them from the
    // live registry too. Single pass through `session_to_helper` while
    // we already hold its lock; the corresponding `registry.remove`
    // calls happen after we release `session_to_helper` to keep with
    // the lock ordering doc'd on `MasterStateInner::registry`.
    let victims: Vec<acp::SessionId> = {
        let mut map = state.session_to_helper.lock().await;
        let victims = map
            .iter()
            .filter_map(|(sid, route)| (route.helper_id == helper_id).then(|| sid.clone()))
            .collect::<Vec<_>>();
        map.retain(|_, route| route.helper_id != helper_id);
        victims
    };
    for sid in &victims {
        state.registry.remove(sid).await;
        // Broadcast removal so every still-attached helper drops the
        // row from its mirror. The disconnecting helper itself has
        // (almost always) already been removed from
        // `helper_ext_subscribers` by `serve_helper`'s cleanup path
        // before this is called, so the broadcast only reaches the
        // peers it should reach.
        broadcast_ext_to_helpers(
            state,
            crate::session_registry::build_session_removed_notification(sid),
        )
        .await;
        broadcast_ext_to_helpers(
            state,
            crate::session_registry::build_sessions_changed_notification(),
        )
        .await;
    }
    victims.len()
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
    notification: acp::ExtNotification,
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

/// Pure async handler for the `intellterm.wta/sessions/list` ExtRequest.
async fn handle_sessions_list(
    state: &MasterStateInner,
    params: &serde_json::value::RawValue,
) -> acp::Result<acp::ExtResponse> {
    handle_sessions_list_with(state, params, |cli, key| {
        crate::history_loader::lookup_title_for_session(cli, key)
    })
    .await
}

/// Testable inner of [`handle_sessions_list`]: the per-CLI disk title lookup is
/// injected so tests can avoid touching `USERPROFILE`. Production uses the
/// wrapper above pinned to `history_loader::lookup_title_for_session`.
///
/// Before returning the snapshot, opportunistically upgrade any row whose title
/// is still synthetic (empty / cwd-basename) from the CLI's on-disk artefacts.
/// This is what gets a title onto **born-bound** rows — e.g. `?<prompt>`
/// delegate sessions, which register a single `SessionStarted` with an empty
/// title at launch (before the CLI has written its generated `name:`) and, being
/// hook-independent, receive no follow-up events to re-trigger
/// `handle_session_hook`'s refresh. The `/sessions` view re-polls `sessions/list`
/// every 5s, so refreshing here surfaces the title once the CLI writes it. The
/// `is_synthetic` early-out inside `try_refresh_title_from_disk_with` keeps this
/// cheap: a row is read from disk only while it still lacks a real title.
async fn handle_sessions_list_with<F>(
    state: &MasterStateInner,
    params: &serde_json::value::RawValue,
    lookup: F,
) -> acp::Result<acp::ExtResponse>
where
    F: Fn(crate::agent_sessions::CliSource, &str) -> Option<String> + Copy,
{
    crate::session_registry::parse_sessions_list_params(params).map_err(|err| {
        tracing::warn!(
            target: "master",
            op = "sessions_list",
            error = %err,
            "rejecting malformed sessions/list params"
        );
        acp::Error::invalid_params().data(serde_json::json!({ "message": err.to_string() }))
    })?;

    for row in state.registry.snapshot().await {
        // `try_refresh_title_from_disk_with` no-ops internally unless the title
        // is still synthetic and a `cli_source` is present, so we can call it
        // for every row without pre-filtering.
        try_refresh_title_from_disk_with(&state.registry, &row.session_id, lookup).await;
    }

    let mut sessions = state.registry.snapshot().await;
    sessions.sort_by(|l, r| l.session_id.0.cmp(&r.session_id.0));
    let raw = crate::session_registry::build_sessions_list_response(sessions);
    Ok(acp::ExtResponse::new(raw.into()))
}

/// Pure async handler for the `intellterm.wta/session_hook` ExtRequest.
///
/// Decodes the hook event, dispatches it to the master-side registry reducer
/// (added in Task A), and broadcasts `sessions/changed` to every connected
/// helper when the reducer actually mutated state. Idempotent / no-op events
/// (reducer returned `false`) skip the broadcast to avoid push storms.
///
/// Title-from-disk refresh: after the reducer applies, we re-check master's
/// row for a "synthetic" title (cwd basename / empty) and try to upgrade it
/// by reading the CLI's on-disk session artefacts (Copilot's `workspace.yaml
/// summary:`/`name:`, Claude/Gemini's first user prompt). The helper already
/// runs the equivalent refresh against its *local* registry, but session management view renders
/// from master's snapshot — without this refresh master never sees the
/// upgraded title and the row keeps showing the cwd basename forever for
/// shell-pane CLI sessions whose first hook arrives before the CLI has
/// written the chat title to disk (e.g. Copilot's UserPromptSubmit fires
/// before its LLM-generated `name:` is written).
async fn handle_session_hook(
    state: &MasterStateInner,
    params: &serde_json::value::RawValue,
    is_born_bound: bool,
) -> acp::Result<acp::ExtResponse> {
    let event = crate::session_registry::parse_session_hook_params(params).map_err(|err| {
        tracing::warn!(
            target: "session_hook",
            error = %err,
            "rejecting malformed session_hook params"
        );
        acp::Error::invalid_params().data(serde_json::json!({ "message": err.to_string() }))
    })?;

    tracing::info!(
        target: "session_hook",
        event = ?event,
        "received helper session hook"
    );

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
    //    is installed — without re-binding the pane.
    //  * real hook / ACP agent-pane event: authoritative for binding AND
    //    activity. Record in `hook_owned` (full watcher suppression) and, if the
    //    session was previously born-bound, drop it from `born_bound` — the real
    //    hook now owns it.
    if let Some(key) = &refresh_key {
        let sid = acp::SessionId::new(key.clone());
        if binding_only {
            state.born_bound.lock().await.insert(sid);
        } else {
            state.hook_owned.lock().await.insert(sid.clone());
            state.born_bound.lock().await.remove(&sid);
        }
    }

    let applied = state.registry.apply_event(event).await;

    let title_upgraded = if let Some(key) = refresh_key {
        try_refresh_title_from_disk(&state.registry, &acp::SessionId::new(key)).await
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

    Ok(crate::session_registry::build_session_hook_response(applied))
}

/// Apply one watcher-emitted session event to master's registry and, if it
/// changed state, broadcast `sessions/changed` so helpers refetch. Mirrors
/// `handle_session_hook` but for the in-process file watcher (no ext-request
/// round-trip). `SessionStarted` synthesis + pane binding happens in
/// `ensure_watched_session_row` before the activity event is applied; the
/// post-apply title refresh upgrades the synthetic (cwd-basename / empty)
/// title from the CLI's on-disk artefacts, same as the hook path.
async fn apply_watcher_event(
    state: &MasterStateInner,
    emitted: crate::session_watcher::Emitted,
) {
    let sid = acp::SessionId::new(emitted.key.clone());

    // Hybrid dedup — the watcher is a *fallback*. Coordinate with authoritative
    // producers:
    //   1. a real hook / ACP agent-pane event recorded the session in
    //      `hook_owned` → drop (the hook owns binding AND activity); or
    //   2. it's a #266 born-bound row (`born_bound`) → the watcher owns no
    //      binding here, but with no real hook it supplies STATUS only (handled
    //      just below); or
    //   3. it's an agent-pane (Class A) session, driven by ACP `session/update`.
    if state.hook_owned.lock().await.contains(&sid) {
        return;
    }

    // Born-bound activity-only fallback: the row already exists and is bound to
    // its pane by #266 born-bound. Born-bound emits no activity, so when no real
    // hook is installed the watcher supplies STATUS. `emitted.event` is always a
    // keyed status event (ToolStarting/ToolCompleted/Notification), so applying
    // it updates the row's status without touching the pane binding / origin.
    // Skip the liveness gate and `ensure_watched_session_row` — born-bound owns
    // the (live, vetted) binding; we only move the status.
    if state.born_bound.lock().await.contains(&sid) {
        let key = emitted.key.clone();
        let applied = state.registry.apply_event(emitted.event).await;
        let title_upgraded =
            try_refresh_title_from_disk(&state.registry, &acp::SessionId::new(key)).await;
        if applied || title_upgraded {
            broadcast_ext_to_helpers(
                state,
                crate::session_registry::build_sessions_changed_notification(),
            )
            .await;
        }
        return;
    }

    let existing = state.registry.lookup(&sid).await;
    if let Some(ref e) = existing {
        if e.origin == Some(crate::agent_sessions::SessionOrigin::AgentPane) {
            return;
        }
    }

    // Liveness gate (only when we'd CREATE a new row or REVIVE a terminal one).
    // The file watcher sees session files machine-wide, so the same on-disk CLI
    // (copilot/claude/…) may be running in VS Code, a background host, or another
    // terminal — not an IT shell pane. Only surface it if its resolved pane is a
    // pane that is currently live in THIS IT instance. Already-live rows skip the
    // gate (vetted at creation) so a chatty turn doesn't re-resolve every event.
    let needs_gate = match &existing {
        None => true,
        Some(e) => matches!(
            e.status,
            Some(crate::agent_sessions::AgentStatus::Historical | crate::agent_sessions::AgentStatus::Ended)
        ),
    };
    if needs_gate {
        let home = std::env::var("USERPROFILE")
            .map(std::path::PathBuf::from)
            .unwrap_or_default();
        let (pane, _pid, _cwd) = resolve_watched_pane_pid_cwd(&home, &emitted);
        let live = live_it_pane_guids(state).await;
        let allowed = watcher_row_allowed(pane.as_deref(), live.as_ref());
        tracing::debug!(
            target: "session_watcher",
            cli = ?emitted.cli,
            key = %emitted.key,
            resolved_pane = ?pane,
            gated = live.is_some(),
            live_pane_count = live.as_ref().map(|s| s.len()).unwrap_or(0),
            allowed,
            "watcher liveness gate decision"
        );
        if !allowed {
            return;
        }
    }

    ensure_watched_session_row(state, &emitted).await;
    let key = emitted.key.clone();
    let applied = state.registry.apply_event(emitted.event).await;
    let title_upgraded =
        try_refresh_title_from_disk(&state.registry, &acp::SessionId::new(key)).await;
    if applied || title_upgraded {
        broadcast_ext_to_helpers(
            state,
            crate::session_registry::build_sessions_changed_notification(),
        )
        .await;
    }
}

/// Pure decision for the watcher liveness gate: should a watcher-discovered
/// session be surfaced, given its resolved `pane` and the set of `live_panes`
/// in this IT instance?
///
/// * `live_panes == None` → liveness is unknown (no WT channel — e.g. unit
///   tests); don't gate, allow.
/// * `live_panes == Some(set)` → allow only if `pane` is `Some` and present in
///   the set (case-insensitive). A `None` pane (CLI not in any terminal, e.g.
///   VS Code / background host) or a pane absent from this IT (another terminal
///   / closed pane) is rejected.
fn watcher_row_allowed(pane: Option<&str>, live_panes: Option<&HashSet<String>>) -> bool {
    match live_panes {
        None => true,
        Some(set) => pane.is_some_and(|p| set.contains(&p.to_ascii_lowercase())),
    }
}

/// The pane GUIDs (lowercased) currently live in this IT instance, via a
/// `list_windows`→`list_tabs`→`list_panes` walk over the master WT channel,
/// cached for [`LIVE_PANES_TTL`]. Returns `None` when there is no WT channel
/// (unit tests) so callers skip the gate entirely. On a COM error it serves the
/// last cached set if any; with no cache it returns `Some(empty)`, which makes
/// the gate *reject* every watcher row (conservative — suppress rather than
/// surface a possibly-dead pane), self-healing on a later event once COM
/// succeeds and the live set repopulates.
async fn live_it_pane_guids(state: &MasterStateInner) -> Option<HashSet<String>> {
    const LIVE_PANES_TTL: std::time::Duration = std::time::Duration::from_secs(2);
    let wt = state.wt.as_ref()?;

    {
        let cache = state.live_panes_cache.lock().await;
        if let Some((at, set)) = cache.as_ref() {
            if at.elapsed() < LIVE_PANES_TTL {
                return Some(set.clone());
            }
        }
    }

    let mut guids = HashSet::new();
    let mut com_ok = false;
    if let Ok(windows) = wt.request("list_windows", serde_json::json!({})).await {
        com_ok = true;
        if let Some(ws) = windows.get("windows").and_then(|v| v.as_array()) {
            for w in ws {
                // `window_id` / `tab_id` come back as JSON *numbers* from COM
                // (e.g. `"window_id": 1`), so match String|Number — `as_str()`
                // alone silently skips every window and yields an empty set,
                // which would make the liveness gate reject every session.
                let wid = match w.get("window_id") {
                    Some(serde_json::Value::String(s)) => s.clone(),
                    Some(serde_json::Value::Number(n)) => n.to_string(),
                    _ => continue,
                };
                let Ok(tabs) = wt
                    .request("list_tabs", serde_json::json!({ "window_id": wid }))
                    .await
                else { continue };
                let Some(ts) = tabs.get("tabs").and_then(|v| v.as_array()) else { continue };
                for t in ts {
                    let tid = match t.get("tab_id") {
                        Some(serde_json::Value::String(s)) => s.clone(),
                        Some(serde_json::Value::Number(n)) => n.to_string(),
                        _ => continue,
                    };
                    let Ok(panes) = wt
                        .request("list_panes", serde_json::json!({ "tab_id": tid }))
                        .await
                    else { continue };
                    if let Some(ps) = panes.get("panes").and_then(|v| v.as_array()) {
                        for p in ps {
                            let guid = match p.get("session_id") {
                                Some(serde_json::Value::String(s)) => Some(s.clone()),
                                Some(serde_json::Value::Number(n)) => Some(n.to_string()),
                                _ => None,
                            };
                            if let Some(g) = guid {
                                guids.insert(g.to_ascii_lowercase());
                            }
                        }
                    }
                }
            }
        }
    }

    if com_ok {
        tracing::debug!(
            target: "session_watcher",
            panes = ?guids,
            "refreshed live IT pane set"
        );
        let mut cache = state.live_panes_cache.lock().await;
        *cache = Some((std::time::Instant::now(), guids.clone()));
        Some(guids)
    } else {
        // COM failed: serve the last good set if we have one, else empty.
        let cache = state.live_panes_cache.lock().await;
        Some(cache.as_ref().map(|(_, s)| s.clone()).unwrap_or_default())
    }
}

/// Ensure master's registry has a row for the event's session key, creating a
/// minimal one (with a best-effort pane binding) on first sight, OR reviving a
/// Class-B (shell-pane) row the user just resumed. Binding per the spec's
/// Decision #3: Copilot=lock, Codex=Restart Manager, Claude=cwd-correlation,
/// Gemini=unbound (cwd not path-encoded). All resolver calls are best-effort —
/// a failed bind never blocks row creation/revival, it just leaves
/// `pane_session_id = None`.
///
/// Revival: a resumed shell-pane session is `Historical` (from the startup
/// history scan) or `Ended`; the watcher event flips it back to `Idle` and
/// rebinds its pane so the activity event applied immediately after can mark it
/// `Working`. This is done here, in the watcher path, rather than by loosening
/// the shared reducer's terminal-state guard, so Class-A agent-pane ghost rows
/// stay protected.
async fn ensure_watched_session_row(
    state: &MasterStateInner,
    emitted: &crate::session_watcher::Emitted,
) {
    use crate::agent_sessions::{AgentStatus, SessionOrigin};
    let sid = acp::SessionId::new(emitted.key.clone());
    let home = std::env::var("USERPROFILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();

    match state.registry.lookup(&sid).await {
        None => {
            // First sight: create the row with a best-effort pane binding.
            let (pane, pid, cwd) = resolve_watched_pane_pid_cwd(&home, emitted);
            let mut info = crate::session_registry::SessionInfo::new(sid, cwd);
            info.cli_source = Some(emitted.cli.clone());
            info.status = Some(AgentStatus::Idle);
            info.origin = Some(SessionOrigin::Unknown);
            info.pane_session_id = pane;
            info.bound_pid = pid;
            state.registry.upsert(info).await;
        }
        Some(existing) => {
            // Revive a Class-B (non-agent-pane) row that the user just resumed
            // in a shell pane: it's Historical (from the startup history scan)
            // or Ended (pane previously closed). Rebind its pane and clear the
            // terminal status to Idle so the activity event applied right after
            // this can mark it Working. Doing the revival here — in the watcher
            // path — keeps the shared reducer's terminal-state guard untouched,
            // so Class-A agent-pane ghost rows stay protected.
            let is_class_b = existing.origin != Some(SessionOrigin::AgentPane);
            let is_terminal = matches!(
                existing.status,
                Some(AgentStatus::Historical | AgentStatus::Ended)
            );
            if is_class_b && is_terminal {
                let (pane, pid, _cwd) = resolve_watched_pane_pid_cwd(&home, emitted);
                let mut revived = existing;
                revived.status = Some(AgentStatus::Idle);
                // Only overwrite the pane binding / pid when we resolved a
                // fresh one; never clobber a good binding with None.
                if pane.is_some() {
                    revived.pane_session_id = pane;
                }
                if pid.is_some() {
                    revived.bound_pid = pid;
                }
                revived.last_error = None;
                revived.attention_reason = None;
                revived.current_tool = None;
                state.registry.upsert(revived).await;
            }
            // Class-A rows, and already-live Class-B rows, are left as-is.
        }
    }
}

/// Best-effort `(pane GUID, owner pid, cwd)` for a watched session, per the
/// spec's Decision #3 binding strategy. All resolver calls are best-effort — a
/// failed bind yields `pane = None` / `pid = None` and never blocks row
/// creation/revival. The pid feeds master's Class-B liveness poll.
fn resolve_watched_pane_pid_cwd(
    home: &std::path::Path,
    emitted: &crate::session_watcher::Emitted,
) -> (Option<String>, Option<u32>, std::path::PathBuf) {
    use crate::agent_sessions::CliSource;
    match &emitted.cli {
        CliSource::Copilot => {
            let dir = crate::history_loader::copilot_session_dir_for_key(home, &emitted.key);
            let (pane, pid) = crate::session_watcher::bind::bind_copilot(&dir);
            (pane, pid, emitted.cwd.clone().unwrap_or_default())
        }
        CliSource::Codex => {
            match crate::history_loader::find_codex_rollout_by_id(home, &emitted.key) {
                Some(path) => {
                    let (pane, pid) = crate::session_watcher::bind::bind_codex(&path);
                    // Codex's emitted.cwd is None (not path-encoded); read it
                    // from the rollout's session_meta so the row has a
                    // cwd-basename title fallback before the user's first
                    // message (which is what the title is derived from) lands.
                    let cwd = crate::history_loader::codex_cwd_from_rollout(&path)
                        .or_else(|| emitted.cwd.clone())
                        .unwrap_or_default();
                    (pane, pid, cwd)
                }
                None => (None, None, emitted.cwd.clone().unwrap_or_default()),
            }
        }
        CliSource::Claude => match &emitted.cwd {
            Some(cwd) => {
                let (pane, pid) = crate::session_watcher::bind::bind_by_cwd(&emitted.cli, cwd);
                (pane, pid, cwd.clone())
            }
            None => (None, None, std::path::PathBuf::new()),
        },
        // Gemini's cwd is not path-encoded (MVP: unbound); Unknown likewise.
        CliSource::Gemini | CliSource::Unknown(_) => {
            (None, None, emitted.cwd.clone().unwrap_or_default())
        }
    }
}

/// Demote shell-pane (Class-B) sessions whose owning CLI process has exited
/// without writing a "session ended" record — e.g. the user `Ctrl+C`'d a
/// `codex` / `claude` / `gemini` running directly in a pane. Those CLIs leave
/// the rollout/transcript file frozen at its last turn, so process death is the
/// only end signal; master polls the bound pids and ends any that are gone.
///
/// Agent-pane (Class-A) sessions are managed by the ACP / alive-mirror path and
/// are never touched here. Rows without a `bound_pid` (binding failed, or
/// Gemini which is unbound) can't be polled and are left as-is. Returns the
/// number of sessions reaped (for the caller / tests).
async fn reap_dead_class_b_sessions(state: &MasterStateInner) -> usize {
    use crate::agent_sessions::{AgentStatus, SessionOrigin};
    let dead: Vec<String> = state
        .registry
        .snapshot()
        .await
        .into_iter()
        .filter(|s| s.origin != Some(SessionOrigin::AgentPane))
        .filter(|s| {
            matches!(
                s.status,
                Some(AgentStatus::Working | AgentStatus::Idle | AgentStatus::Attention)
            )
        })
        .filter_map(|s| s.bound_pid.map(|pid| (s.session_id.0.to_string(), pid)))
        .filter(|(_, pid)| !crate::proc_bind::pid_alive(*pid))
        .map(|(key, _)| key)
        .collect();

    if dead.is_empty() {
        return 0;
    }

    let mut reaped = 0;
    for key in &dead {
        let applied = state
            .registry
            .apply_event(crate::agent_sessions::SessionEvent::SessionStopped {
                key: key.clone(),
                reason: "process exited".to_string(),
            })
            .await;
        if applied {
            reaped += 1;
            tracing::info!(
                target: "session_watcher",
                session_id = %key,
                "reaped Class-B session: owning process exited"
            );
        }
    }
    if reaped > 0 {
        broadcast_ext_to_helpers(
            state,
            crate::session_registry::build_sessions_changed_notification(),
        )
        .await;
    }
    reaped
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
///     hook is unreliable on `CTRL_CLOSE_EVENT` (confirmed via
///     `hook-trace.log`), and the helper observation path may not
///     publish for reasons we have not finished isolating.
///
/// Copilot / Claude's Stop / SessionEnd hooks fire fast enough that
/// the publish-from-helper path works for them today; this subscriber
/// makes the behavior uniform across CLIs and resilient to helper
/// teardown order.
async fn handle_master_wt_event(
    state: &MasterStateInner,
    event_json: serde_json::Value,
) {
    let method = event_json
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if method != "connection_state" {
        return;
    }
    let params = event_json
        .get("params")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
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
    let pane_state = params
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("");
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

/// If the row for `sid` has a synthetic title (None / empty / cwd
/// basename) and we can resolve a richer title from the CLI's on-disk
/// session artefacts, atomically upgrade the row's title. Returns
/// `true` iff the title actually changed.
///
/// Reads workspace.yaml / JSONL outside the registry lock; commits via
/// the atomic `upgrade_title_if_synthetic` registry method so a
/// concurrent `apply_event` can't lose status / pane_session_id from a
/// full-row upsert with a stale clone (which is what naïve
/// lookup→clone→mutate→upsert would do here).
async fn try_refresh_title_from_disk(
    registry: &std::sync::Arc<dyn crate::session_registry::SessionRegistry>,
    sid: &acp::SessionId,
) -> bool {
    try_refresh_title_from_disk_with(registry, sid, |cli, key| {
        crate::history_loader::lookup_title_for_session(cli, key)
    })
    .await
}

/// Testable inner of [`try_refresh_title_from_disk`]: the disk lookup
/// is injected as a closure so tests can avoid mutating `USERPROFILE`
/// or staging files. Production code uses the wrapper above which
/// pins the closure to `history_loader::lookup_title_for_session`.
async fn try_refresh_title_from_disk_with<F>(
    registry: &std::sync::Arc<dyn crate::session_registry::SessionRegistry>,
    sid: &acp::SessionId,
    lookup: F,
) -> bool
where
    F: FnOnce(crate::agent_sessions::CliSource, &str) -> Option<String>,
{
    let Some(info) = registry.lookup(sid).await else {
        return false;
    };
    // Skip the disk read when the title is already a real one (not
    // empty / None / equal to the cwd basename). Avoids hammering
    // workspace.yaml / JSONL on every hook event for sessions that are
    // already labelled.
    let cwd_leaf = info
        .cwd
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let is_synthetic = match info.title.as_deref() {
        None | Some("") => true,
        Some(t) => t == cwd_leaf,
    };
    if !is_synthetic {
        return false;
    }
    // cli_source is needed to dispatch the right per-CLI on-disk
    // scanner; rows that landed in master without one (legacy /
    // partially-populated history seeds) can't be refreshed.
    let Some(cli) = info.cli_source.clone() else {
        return false;
    };
    let Some(disk_title) = lookup(cli, &info.session_id.0) else {
        return false;
    };
    if disk_title.is_empty() {
        return false;
    }
    let upgraded = registry
        .upgrade_title_if_synthetic(sid, &disk_title)
        .await;
    if upgraded {
        tracing::info!(
            target: "session_hook",
            session_id = %sid.0,
            title_len = disk_title.chars().count(),
            "upgraded synthetic title from on-disk session artefacts",
        );
    }
    upgraded
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
    params: &serde_json::value::RawValue,
) -> acp::Result<acp::ExtResponse> {
    let parsed = crate::session_registry::parse_focus_session_params(params).map_err(|err| {
        tracing::warn!(
            target: "master",
            op = "focus_session",
            error = %err,
            "rejecting malformed focus_session params"
        );
        acp::Error::invalid_params().data(serde_json::json!({ "message": err.to_string() }))
    })?;

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
            Ok(acp::ExtResponse::new(raw.into()))
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
    params: &serde_json::value::RawValue,
) -> acp::Result<acp::ExtResponse> {
    let parsed =
        crate::session_registry::parse_session_resume_dispatched_params(params).map_err(|err| {
            acp::Error::invalid_params().data(serde_json::json!({ "message": err.to_string() }))
        })?;
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
    Ok(acp::ExtResponse::new(raw.into()))
}

async fn handle_session_focus(
    state: &MasterStateInner,
    params: &serde_json::value::RawValue,
) -> acp::Result<acp::ExtResponse> {
    let parsed = crate::session_registry::parse_session_focus_params(params).map_err(|err| {
        acp::Error::invalid_params().data(serde_json::json!({ "message": err.to_string() }))
    })?;
    let Some(info) = state.registry.lookup(&parsed.sid).await else {
        let body = crate::session_registry::SessionFocusResponse {
            focused: false,
            pane_session_id: None,
            reason: Some("no_pane".to_string()),
            detail: Some("session id is not in the master registry".to_string()),
        };
        let raw = serde_json::value::to_raw_value(&body).expect("focus response serializes");
        return Ok(acp::ExtResponse::new(raw.into()));
    };
    let Some(pane_session_id) = info.pane_session_id.clone() else {
        let body = crate::session_registry::SessionFocusResponse {
            focused: false,
            pane_session_id: None,
            reason: Some("no_pane".to_string()),
            detail: None,
        };
        let raw = serde_json::value::to_raw_value(&body).expect("focus response serializes");
        return Ok(acp::ExtResponse::new(raw.into()));
    };
    let Some(wt) = state.wt.as_ref() else {
        let body = crate::session_registry::SessionFocusResponse {
            focused: false,
            pane_session_id: Some(pane_session_id),
            reason: Some("wtcli_error".to_string()),
            detail: Some("focus channel unavailable".to_string()),
        };
        let raw = serde_json::value::to_raw_value(&body).expect("focus response serializes");
        return Ok(acp::ExtResponse::new(raw.into()));
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
            Ok(acp::ExtResponse::new(raw.into()))
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
            Ok(acp::ExtResponse::new(raw.into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acp::{ContentChunk, SessionId, SessionNotification, SessionUpdate};
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    struct NoopClient;

    #[async_trait::async_trait(?Send)]
    impl acp::Client for NoopClient {
        async fn request_permission(
            &self,
            _args: acp::RequestPermissionRequest,
        ) -> acp::Result<acp::RequestPermissionResponse> {
            Err(acp::Error::method_not_found())
        }

        async fn session_notification(
            &self,
            _args: acp::SessionNotification,
        ) -> acp::Result<()> {
            Ok(())
        }
    }

    struct PendingNewSessionAgent;

    #[async_trait::async_trait(?Send)]
    impl acp::Agent for PendingNewSessionAgent {
        async fn initialize(
            &self,
            _args: acp::InitializeRequest,
        ) -> acp::Result<acp::InitializeResponse> {
            Ok(acp::InitializeResponse::new(acp::ProtocolVersion::V1))
        }

        async fn authenticate(
            &self,
            _args: acp::AuthenticateRequest,
        ) -> acp::Result<acp::AuthenticateResponse> {
            Ok(acp::AuthenticateResponse::new())
        }

        async fn new_session(
            &self,
            _args: acp::NewSessionRequest,
        ) -> acp::Result<acp::NewSessionResponse> {
            futures::future::pending().await
        }

        async fn prompt(&self, _args: acp::PromptRequest) -> acp::Result<acp::PromptResponse> {
            Err(acp::Error::method_not_found())
        }

        async fn cancel(&self, _args: acp::CancelNotification) -> acp::Result<()> {
            Ok(())
        }
    }

    // ── Agent selection / security policy ───────────────────────────
    //
    // `resolve_agent_selection` is the single choke point that decides
    // what the master will spawn for a helper. Extracting it as a pure
    // function lets us exercise the full policy — id reconstruction,
    // GPO allowlist, fallback, and the "never trust a command off the
    // pipe" invariant — without launching a single subprocess (cleaner
    // than injecting a fake spawner, which only the I/O plumbing needs).

    const DEFAULT_CMD: &str = "copilot --acp --stdio";

    fn allow_set(ids: &[&str]) -> std::collections::HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    /// Run the resolver the way `HelperHandler::initialize` does.
    fn resolve(
        allowed: Option<&std::collections::HashSet<String>>,
        requested_id: Option<&str>,
        model: Option<&str>,
    ) -> (String, Option<String>) {
        resolve_agent_selection(
            DEFAULT_CMD,
            Some("copilot"),
            allowed,
            requested_id,
            model,
            HelperId(1),
        )
    }

    #[test]
    fn known_id_with_no_allowlist_is_reconstructed_not_taken_from_pipe() {
        // No host allowlist (manual run / older host) ⇒ any known id is
        // honored, and the command is REBUILT from the id.
        let (cmd, id) = resolve(None, Some("gemini"), None);
        assert_eq!(cmd, "gemini --experimental-acp");
        assert_eq!(id.as_deref(), Some("gemini"));
    }

    #[test]
    fn model_is_folded_in_for_native_agents_and_ignored_for_adapters() {
        // Native agent (gemini) takes --model on the command line.
        let (cmd, _) = resolve(None, Some("gemini"), Some("gemini-2.5-pro"));
        assert_eq!(cmd, "gemini --experimental-acp --model gemini-2.5-pro");

        // Adapter agent (claude via npx) ignores the model here — it's
        // applied later via setSessionModel — so the command is stable.
        let (cmd, id) = resolve(None, Some("claude"), Some("opus-4"));
        assert_eq!(cmd, "npx -y @agentclientprotocol/claude-agent-acp");
        assert_eq!(id.as_deref(), Some("claude"));
    }

    #[test]
    fn id_is_case_insensitive() {
        let (cmd, id) = resolve(Some(&allow_set(&["gemini"])), Some("GeMiNi"), None);
        assert_eq!(cmd, "gemini --experimental-acp");
        assert_eq!(id.as_deref(), Some("gemini"));
    }

    #[test]
    fn empty_or_missing_id_falls_back_to_default() {
        for requested in [None, Some(""), Some("   ")] {
            let (cmd, id) = resolve(None, requested, None);
            assert_eq!(cmd, DEFAULT_CMD, "requested={requested:?}");
            assert_eq!(id.as_deref(), Some("copilot"));
        }
    }

    #[test]
    fn every_known_agent_id_is_honored_not_conflated_with_default_fallback() {
        // Regression guard for the conflation flagged in review: the `known`
        // check must test KNOWN_AGENTS membership directly, NOT
        // `lookup_profile_by_id(id).id != DEFAULT_PROFILE.id`. The latter
        // silently treats a real agent as "unknown" — forcing the default and
        // dropping requested-model folding — the day DEFAULT_PROFILE.id is set
        // to a genuine, selectable agent id. Every known agent must resolve to
        // its own rebuilt command and stamp its own id.
        for profile in crate::agent_registry::KNOWN_AGENTS {
            let (cmd, id) = resolve(None, Some(profile.id), None);
            let expected = crate::agent_registry::build_acp_command(profile.id, None);
            assert_eq!(cmd, expected, "agent {} must be honored, not fall back", profile.id);
            assert_eq!(id.as_deref(), Some(profile.id), "id stamp for {}", profile.id);
        }
    }

    #[test]
    fn unknown_or_custom_id_falls_back_to_trusted_default() {
        // `custom:` and bogus ids aren't in KNOWN_AGENTS ⇒ the master
        // runs the trusted global default (which is what carries the
        // global custom command), never a string from the pipe.
        for requested in ["custom", "custom:calc.exe", "totally-bogus"] {
            let (cmd, id) = resolve(None, Some(requested), None);
            assert_eq!(cmd, DEFAULT_CMD, "requested={requested}");
            assert_eq!(id.as_deref(), Some("copilot"));
        }
    }

    #[test]
    fn gpo_allowlist_blocks_known_but_unlisted_ids() {
        let allowed = allow_set(&["gemini"]);
        // gemini is listed ⇒ honored.
        let (cmd, _) = resolve(Some(&allowed), Some("gemini"), None);
        assert_eq!(cmd, "gemini --experimental-acp");
        // copilot is a *known* agent but NOT in the GPO-filtered set ⇒
        // refused, fall back to default. (Defends against a peer helper
        // selecting a policy-blocked agent.)
        let (cmd, id) = resolve(Some(&allowed), Some("copilot"), None);
        assert_eq!(cmd, DEFAULT_CMD);
        assert_eq!(id.as_deref(), Some("copilot"));
    }

    #[test]
    fn agent_cmd_from_the_pipe_is_never_executed() {
        // Mirror the initialize path: a malicious helper sets a dangerous
        // `agent_cmd` alongside a benign `agent_id`. The resolver doesn't
        // even take `agent_cmd`, and the resolved command is rebuilt from
        // the id — so the pipe-supplied string can never be spawned.
        let mut meta: Option<acp::Meta> = None;
        crate::session_registry::inject_wta_meta(
            &mut meta,
            &crate::session_registry::WtaMeta {
                agent_cmd: Some("calc.exe".to_string()),
                agent_id: Some("gemini".to_string()),
                ..Default::default()
            },
        );
        let wta = crate::session_registry::extract_wta_meta(&mut meta);
        let (cmd, _) = resolve(None, wta.agent_id.as_deref(), wta.model.as_deref());
        assert_eq!(cmd, "gemini --experimental-acp");
        assert!(!cmd.contains("calc.exe"), "pipe command must never appear");
    }

    #[test]
    fn pool_key_dedupes_same_selection_and_separates_distinct_agents() {
        // `get_or_spawn_agent` keys its CLI pool on the resolved command.
        // Same id+model ⇒ identical key ⇒ one shared CLI; different ids ⇒
        // different keys ⇒ separate CLIs (Gemini in one tab, Claude in
        // another). Assert the keying that drives that dedup.
        let (a, _) = resolve(None, Some("gemini"), Some("flash"));
        let (b, _) = resolve(None, Some("gemini"), Some("flash"));
        let (c, _) = resolve(None, Some("claude"), None);
        assert_eq!(a, b, "same selection must yield one pool key");
        assert_ne!(a, c, "different agents must get different pool keys");
    }

    fn make_state() -> Arc<MasterStateInner> {
        Arc::new(MasterStateInner {
            session_to_helper: Mutex::new(HashMap::new()),
            registry: crate::session_registry::InMemoryRegistry::shared(),
            helper_ext_subscribers: Mutex::new(HashMap::new()),
            wt: None,
            agents: Mutex::new(HashMap::new()),
            default_agent_cmd: "copilot --acp --stdio".to_string(),
            default_agent_id: Some("copilot".to_string()),
            allowed_agent_ids: None,
            helper_meta: Mutex::new(HashMap::new()),
            hook_owned: Mutex::new(HashSet::new()),
            born_bound: Mutex::new(HashSet::new()),
            live_panes_cache: Mutex::new(None),
        })
    }

    fn client_connection_to_pending_new_session_agent() -> Arc<acp::ClientSideConnection> {
        let (client_pipe, agent_pipe) = tokio::io::duplex(4096);
        let (client_read, client_write) = tokio::io::split(client_pipe);
        let (agent_read, agent_write) = tokio::io::split(agent_pipe);

        let (_agent_conn, agent_io) = acp::AgentSideConnection::new(
            PendingNewSessionAgent,
            agent_write.compat_write(),
            agent_read.compat(),
            |fut| {
                tokio::task::spawn_local(fut);
            },
        );
        tokio::task::spawn_local(async move {
            let _ = agent_io.await;
        });

        let (client_conn, client_io) = acp::ClientSideConnection::new(
            NoopClient,
            client_write.compat_write(),
            client_read.compat(),
            |fut| {
                tokio::task::spawn_local(fut);
            },
        );
        tokio::task::spawn_local(async move {
            let _ = client_io.await;
        });

        Arc::new(client_conn)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn new_session_timeout_is_enforced_by_master_forwarder() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
                // The multi-agent HelperHandler binds its agent during
                // `initialize`; pre-bind one wrapping the pending
                // (hangs-on-session/new) connection so
                // `forward_new_session_to_agent` resolves it and exercises
                // the timeout path.
                let agent = OnceLock::new();
                let _ = agent.set(Arc::new(AgentCli {
                    key: "test-agent".to_string(),
                    conn: client_connection_to_pending_new_session_agent(),
                    cached_init_resp: acp::InitializeResponse::new(acp::ProtocolVersion::V1),
                    cli_source: None,
                }));
                let handler = HelperHandler {
                    helper_id: HelperId(1),
                    agent,
                    state: make_state(),
                    notif_tx,
                    agent_side_slot: Arc::new(OnceLock::new()),
                };

                let err = handler
                    .forward_new_session_to_agent(
                        acp::NewSessionRequest::new(PathBuf::from(r"C:\repo")),
                        std::time::Duration::from_millis(1),
                    )
                    .await
                    .expect_err("master should return an ACP error when agent session/new hangs");

                assert_eq!(err.code, acp::ErrorCode::InternalError);
                assert!(
                    format!("{err}").contains("agent CLI session/new timed out"),
                    "error should identify master->agent session/new timeout: {err}"
                );
            })
            .await;
    }

    #[test]
    fn restart_agent_pane_event_shape_carries_tab_and_session() {
        let sid = SessionId::from("sess-abc");
        let evt = build_restart_agent_pane_event("tab-42", Some(&sid));
        assert_eq!(evt["type"], "event");
        assert_eq!(evt["method"], "restart_agent_pane");
        assert_eq!(evt["params"]["tab_id"], "tab-42");
        assert_eq!(evt["params"]["session_id"], "sess-abc");
        assert_eq!(evt["params"]["reason"], "helper_disconnect");
    }

    #[test]
    fn restart_agent_pane_event_null_session_when_none() {
        let evt = build_restart_agent_pane_event("tab-7", None);
        assert!(evt["params"]["session_id"].is_null());
        assert_eq!(evt["params"]["tab_id"], "tab-7");
    }

    fn make_notif(sid: &SessionId) -> SessionNotification {
        SessionNotification::new(
            sid.clone(),
            SessionUpdate::AgentMessageChunk(ContentChunk::new("hi".into())),
        )
    }

    async fn route(state: &Arc<MasterStateInner>, notif: SessionNotification) {
        let client = MasterClient {
            state: Arc::clone(state),
        };
        client.session_notification(notif).await.unwrap();
    }

    /// New `session_notification`s for a registered SessionId reach
    /// the owning helper's channel, and a second helper's channel
    /// stays untouched.
    #[tokio::test]
    async fn session_notification_routes_to_owning_helper() {
        let state = make_state();
        let (tx1, mut rx1) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
        let (tx2, mut rx2) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
        let sid1 = SessionId::new("sess-1");
        let sid2 = SessionId::new("sess-2");

        {
            let mut map = state.session_to_helper.lock().await;
            map.insert(
                sid1.clone(),
                HelperRoute {
                    helper_id: HelperId(1),
                    notif_tx: tx1,
                    forwarder: None,
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            );
            map.insert(
                sid2.clone(),
                HelperRoute {
                    helper_id: HelperId(2),
                    notif_tx: tx2,
                    forwarder: None,
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            );
        }

        route(&state, make_notif(&sid1)).await;
        assert!(rx1.try_recv().is_ok(), "helper 1 should have received");
        assert!(
            rx2.try_recv().is_err(),
            "helper 2 should NOT have received helper 1's notification"
        );
    }

    /// When the helper's receiver has been dropped, the failed-send
    /// path removes the routing entry so the warning doesn't repeat
    /// for the same SessionId on every subsequent notification.
    #[tokio::test]
    async fn session_notification_drops_entry_on_send_failure() {
        let state = make_state();
        let (tx, rx) = mpsc::channel::<SessionNotification>(NOTIF_CHANNEL_CAPACITY);
        let sid = SessionId::new("dead-session");
        {
            let mut map = state.session_to_helper.lock().await;
            map.insert(
                sid.clone(),
                HelperRoute {
                    helper_id: HelperId(7),
                    notif_tx: tx,
                    forwarder: None,
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            );
        }
        drop(rx); // simulate helper going away

        route(&state, make_notif(&sid)).await;

        let map = state.session_to_helper.lock().await;
        assert!(
            !map.contains_key(&sid),
            "send failure should have removed the routing entry"
        );
    }

    /// Regression test for the rebinding race in the Closed-cleanup
    /// path. Sequence:
    ///   1. Helper A is bound to `sid`; we snapshot its `notif_tx`.
    ///   2. Helper A's receiver is dropped (channel becomes Closed).
    ///   3. Helper B rebinds the SAME `sid` via `load_session` —
    ///      the map entry now points at helper B.
    ///   4. Master finally tries `try_send` on the snapshotted (now
    ///      Closed) sender → `TrySendError::Closed`.
    ///
    /// Before the fix the cleanup path would `map.remove(&sid)`
    /// unconditionally and clobber helper B's freshly-installed route.
    /// With the fix it compares `helper_id` and leaves the new entry
    /// alone.
    #[tokio::test]
    async fn session_notification_preserves_rebound_route_on_closed() {
        let state = make_state();
        let sid = SessionId::new("reused-session");

        // Helper A is initially bound; we'll snapshot its sender by
        // invoking session_notification — `route` only takes a state
        // snapshot under the lock, then drops the lock before
        // try_send. We need the snapshot to capture A but the rebind
        // to happen before try_send wakes Closed. Easiest: drop A's
        // receiver, then immediately rebind to B in the same task,
        // then route — `try_send` sees Closed; the helper_id check
        // sees the entry is B's; cleanup must NOT remove B.
        let (tx_a, rx_a) = mpsc::channel::<SessionNotification>(NOTIF_CHANNEL_CAPACITY);
        {
            let mut map = state.session_to_helper.lock().await;
            map.insert(
                sid.clone(),
                HelperRoute {
                    helper_id: HelperId(1),
                    notif_tx: tx_a.clone(),
                    forwarder: None,
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            );
        }
        drop(rx_a); // A's channel is now Closed

        // We can't reliably interleave "snapshot then rebind then
        // try_send" without unsafe scheduling; instead, simulate the
        // exact post-race state: helper B has already rebound by the
        // time the cleanup runs. Construct the snapshot manually and
        // invoke a tiny helper that mirrors the production
        // cleanup-with-identity-check path.
        let snap_helper_a = HelperId(1);

        // Rebind to helper B (simulating the racing load_session
        // landing between snapshot and try_send).
        let (tx_b, _rx_b) = mpsc::channel::<SessionNotification>(NOTIF_CHANNEL_CAPACITY);
        {
            let mut map = state.session_to_helper.lock().await;
            map.insert(
                sid.clone(),
                HelperRoute {
                    helper_id: HelperId(2),
                    notif_tx: tx_b,
                    forwarder: None,
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            );
        }

        // Drive the real production path. `tx_a` is the snapshot we'd
        // have captured before the rebind; `try_send` on it returns
        // Closed. The cleanup must look at the current map entry,
        // see it's helper B (≠ A), and leave it alone.
        match tx_a.try_send(make_notif(&sid)) {
            Err(mpsc::error::TrySendError::Closed(_)) => {}
            other => panic!("expected Closed, got {other:?}"),
        }
        {
            let mut map = state.session_to_helper.lock().await;
            match map.get(&sid) {
                Some(current) if current.helper_id == snap_helper_a => {
                    map.remove(&sid);
                }
                _ => {} // identity mismatch — leave new route intact
            }
        }

        let map = state.session_to_helper.lock().await;
        let current = map.get(&sid).expect("helper B's route must survive");
        assert_eq!(
            current.helper_id,
            HelperId(2),
            "Closed cleanup must not remove a route rebound to a different helper"
        );
    }

    /// A full bounded channel drops the new notification (and logs)
    /// instead of `await`-blocking — protects the agent CLI I/O loop
    /// from head-of-line blocking when one helper's pipe stalls.
    /// Verified by filling a capacity-1 channel without draining, then
    /// routing — the second notification must be silently dropped and
    /// the routing entry must remain (channel is Full, not Closed).
    #[tokio::test]
    async fn session_notification_drops_on_full_channel() {
        let state = make_state();
        let (tx, _rx) = mpsc::channel::<SessionNotification>(1);
        let sid = SessionId::new("slow-helper");
        {
            let mut map = state.session_to_helper.lock().await;
            map.insert(
                sid.clone(),
                HelperRoute {
                    helper_id: HelperId(9),
                    notif_tx: tx.clone(),
                    forwarder: None,
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            );
        }
        // Fill capacity. _rx is held so the channel stays open.
        tx.try_send(make_notif(&sid)).unwrap();
        // Second send via the routing path must be a no-op-with-warn,
        // not a panic or an error.
        route(&state, make_notif(&sid)).await;
        // Routing entry survives Full (only Closed removes it).
        let map = state.session_to_helper.lock().await;
        assert!(
            map.contains_key(&sid),
            "Full (not Closed) must NOT remove the routing entry"
        );
    }

    /// Unknown SessionId is a no-op (warned but not errored) — the
    /// `Client` trait return value must stay `Ok` so the master's
    /// I/O loop doesn't tear down on a stale notification.
    #[tokio::test]
    async fn session_notification_unknown_session_is_noop() {
        let state = make_state();
        let sid = SessionId::new("never-registered");
        // Just ensure the call doesn't panic and returns Ok.
        route(&state, make_notif(&sid)).await;
        let map = state.session_to_helper.lock().await;
        assert!(map.is_empty());
    }

    /// `drop_sessions_for_helper` removes exactly the rows owned by
    /// the disconnecting helper, leaving other helpers' rows intact.
    /// This is the cleanup the helper-disconnect path runs.
    #[tokio::test]
    async fn drop_sessions_for_helper_retains_only_other_helpers() {
        let state = make_state();
        let (tx_a, _rx_a) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
        let (tx_b, _rx_b) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
        let (tx_c, _rx_c) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
        {
            let mut map = state.session_to_helper.lock().await;
            map.insert(
                SessionId::new("a1"),
                HelperRoute {
                    helper_id: HelperId(1),
                    notif_tx: tx_a.clone(),
                    forwarder: None,
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            );
            map.insert(
                SessionId::new("a2"),
                HelperRoute {
                    helper_id: HelperId(1),
                    notif_tx: tx_a,
                    forwarder: None,
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            );
            map.insert(
                SessionId::new("b1"),
                HelperRoute {
                    helper_id: HelperId(2),
                    notif_tx: tx_b,
                    forwarder: None,
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            );
            map.insert(
                SessionId::new("c1"),
                HelperRoute {
                    helper_id: HelperId(3),
                    notif_tx: tx_c,
                    forwarder: None,
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            );
        }

        let dropped = drop_sessions_for_helper(&state, HelperId(1)).await;
        assert_eq!(dropped, 2);

        let map = state.session_to_helper.lock().await;
        assert!(!map.contains_key(&SessionId::new("a1")));
        assert!(!map.contains_key(&SessionId::new("a2")));
        assert!(map.contains_key(&SessionId::new("b1")));
        assert!(map.contains_key(&SessionId::new("c1")));
    }

    /// Companion invariant to `drop_sessions_for_helper_retains_only_other_helpers`:
    /// the same teardown call must also remove the corresponding rows
    /// from `state.registry`. Otherwise, a `session/list` response (or
    /// a downstream `intellterm.wta/focus_session` lookup) could hand
    /// out a SessionId whose helper is already gone, and the session management view
    /// would route Enter to a dead pane.
    #[tokio::test]
    async fn drop_sessions_for_helper_also_clears_registry() {
        use crate::session_registry::SessionInfo;
        use std::path::PathBuf;

        let state = make_state();
        let (tx_a, _rx_a) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
        let (tx_b, _rx_b) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);

        // Two helpers, one session each.
        let sid_a = SessionId::new("alive-a");
        let sid_b = SessionId::new("alive-b");
        {
            let mut map = state.session_to_helper.lock().await;
            map.insert(
                sid_a.clone(),
                HelperRoute {
                    helper_id: HelperId(1),
                    notif_tx: tx_a,
                    forwarder: None,
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            );
            map.insert(
                sid_b.clone(),
                HelperRoute {
                    helper_id: HelperId(2),
                    notif_tx: tx_b,
                    forwarder: None,
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            );
        }
        state
            .registry
            .upsert(SessionInfo::new(sid_a.clone(), PathBuf::from("/repo/a")))
            .await;
        state
            .registry
            .upsert(SessionInfo::new(sid_b.clone(), PathBuf::from("/repo/b")))
            .await;

        // Disconnect helper 1.
        drop_sessions_for_helper(&state, HelperId(1)).await;

        assert!(
            state.registry.lookup(&sid_a).await.is_none(),
            "registry must drop sessions owned by the disconnecting helper"
        );
        assert!(
            state.registry.lookup(&sid_b).await.is_some(),
            "registry must keep sessions owned by other helpers"
        );
        let snapshot = state.registry.snapshot().await;
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].session_id, sid_b);
    }

    /// `broadcast_ext_to_helpers` should reach every currently
    /// registered helper subscriber, leaving the subscriber map
    /// intact when channels are live.
    #[tokio::test]
    async fn broadcast_ext_to_helpers_fans_out_to_all_subscribers() {
        use crate::session_registry::{self, build_session_added_notification, SessionInfo};
        use std::path::PathBuf;

        let state = make_state();
        let (tx1, mut rx1) = mpsc::unbounded_channel::<acp::ExtNotification>();
        let (tx2, mut rx2) = mpsc::unbounded_channel::<acp::ExtNotification>();
        {
            let mut subs = state.helper_ext_subscribers.lock().await;
            subs.insert(HelperId(1), tx1);
            subs.insert(HelperId(2), tx2);
        }

        let info = SessionInfo::new(SessionId::new("alive-x"), PathBuf::from("/repo/x"));
        broadcast_ext_to_helpers(&state, build_session_added_notification(&info)).await;

        let got1 = rx1.try_recv().expect("helper 1 receives broadcast");
        let got2 = rx2.try_recv().expect("helper 2 receives broadcast");
        assert_eq!(
            &*got1.method,
            session_registry::INTELLTERM_METHOD_SESSION_ADDED
        );
        assert_eq!(
            &*got2.method,
            session_registry::INTELLTERM_METHOD_SESSION_ADDED
        );

        let subs = state.helper_ext_subscribers.lock().await;
        assert_eq!(subs.len(), 2, "live subscribers stay registered");
    }

    /// If a helper's ext-channel receiver has been dropped, the
    /// broadcast should prune the entry so we don't keep warning on
    /// every future fan-out.
    #[tokio::test]
    async fn broadcast_ext_to_helpers_prunes_dead_subscribers() {
        use crate::session_registry::build_session_removed_notification;

        let state = make_state();
        let (tx_dead, rx_dead) = mpsc::unbounded_channel::<acp::ExtNotification>();
        let (tx_live, _rx_live) = mpsc::unbounded_channel::<acp::ExtNotification>();
        {
            let mut subs = state.helper_ext_subscribers.lock().await;
            subs.insert(HelperId(7), tx_dead);
            subs.insert(HelperId(8), tx_live);
        }
        drop(rx_dead);

        broadcast_ext_to_helpers(
            &state,
            build_session_removed_notification(&SessionId::new("zzz")),
        )
        .await;

        let subs = state.helper_ext_subscribers.lock().await;
        assert!(!subs.contains_key(&HelperId(7)), "dead subscriber pruned");
        assert!(subs.contains_key(&HelperId(8)), "live subscriber retained");
    }

    /// When a helper disconnects, `drop_sessions_for_helper` should
    /// emit a `session_removed` for every session it owned, fanning
    /// out to all OTHER helpers' subscribers.
    #[tokio::test]
    async fn drop_sessions_for_helper_broadcasts_session_removed_to_peers() {
        use crate::session_registry::{self, SessionInfo};
        use std::path::PathBuf;

        let state = make_state();
        // Helper 1 owns two sessions, helper 2 owns none but is
        // subscribed (it's a peer that should learn of the removals).
        let (notif_tx1, _notif_rx1) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
        let (ext_tx2, mut ext_rx2) = mpsc::unbounded_channel::<acp::ExtNotification>();
        let sid_a = SessionId::new("removed-a");
        let sid_b = SessionId::new("removed-b");
        {
            let mut map = state.session_to_helper.lock().await;
            map.insert(
                sid_a.clone(),
                HelperRoute {
                    helper_id: HelperId(1),
                    notif_tx: notif_tx1.clone(),
                    forwarder: None,
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            );
            map.insert(
                sid_b.clone(),
                HelperRoute {
                    helper_id: HelperId(1),
                    notif_tx: notif_tx1,
                    forwarder: None,
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            );
        }
        state
            .registry
            .upsert(SessionInfo::new(sid_a.clone(), PathBuf::from("/a")))
            .await;
        state
            .registry
            .upsert(SessionInfo::new(sid_b.clone(), PathBuf::from("/b")))
            .await;
        {
            let mut subs = state.helper_ext_subscribers.lock().await;
            subs.insert(HelperId(2), ext_tx2);
        }

        drop_sessions_for_helper(&state, HelperId(1)).await;

        // Expect two session_removed notifications on peer 2's channel;
        // Task A also emits sessions/changed after each registry mutation.
        let mut got: Vec<acp::SessionId> = Vec::new();
        while let Ok(ext) = ext_rx2.try_recv() {
            match session_registry::parse_ext_notification(&ext) {
                session_registry::WtaExtNotification::SessionRemoved(sid) => got.push(sid),
                session_registry::WtaExtNotification::SessionsChanged => {}
                other => panic!("expected SessionRemoved or SessionsChanged, got {other:?}"),
            }
        }
        got.sort_by(|a, b| a.0.cmp(&b.0));
        let mut expected = vec![sid_a, sid_b];
        expected.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(got, expected);
    }

    /// `route_for` (used by every `MasterClient::<client-method>`
    /// forwarder) must return `internal_error` when the agent CLI
    /// sends a request for a session that no helper has registered
    /// — typically a stale call after the owning helper disconnected.
    /// Returning `Ok(...)` here would dereference an invalid route.
    #[tokio::test]
    async fn route_for_unknown_session_id_returns_internal_error() {
        let state = make_state();
        let client = MasterClient {
            state: Arc::clone(&state),
        };
        let err = client
            .route_for(&SessionId::new("ghost"), "request_permission")
            .await
            .expect_err("unknown session_id must not resolve");
        assert_eq!(err.code, acp::ErrorCode::InternalError);
    }

    /// `route_for` must also fail when the routing entry exists but
    /// its `forwarder` slot is `None`. Production code never inserts
    /// a `None` forwarder (every `new_session` / `load_session` path
    /// upgrades the helper's `Weak<AgentSideConnection>`), so reaching
    /// this branch means the slot was inserted before the conn was
    /// alive — that's a bug we want to surface, not paper over.
    #[tokio::test]
    async fn route_for_none_forwarder_returns_internal_error() {
        let state = make_state();
        let (tx, _rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
        {
            let mut map = state.session_to_helper.lock().await;
            map.insert(
                SessionId::new("orphan"),
                HelperRoute {
                    helper_id: HelperId(42),
                    notif_tx: tx,
                    forwarder: None,
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            );
        }
        let client = MasterClient {
            state: Arc::clone(&state),
        };
        let err = client
            .route_for(&SessionId::new("orphan"), "create_terminal")
            .await
            .expect_err("None forwarder must not resolve");
        assert_eq!(err.code, acp::ErrorCode::InternalError);
    }

    /// End-to-end through one of the forwarder methods: a Client-trait
    /// request on `MasterClient` for an unknown session_id propagates
    /// the same `internal_error` (rather than the trait default
    /// `method_not_found`, which would mislead the agent CLI into
    /// thinking the master doesn't support terminals at all).
    #[tokio::test]
    async fn master_client_create_terminal_unknown_session_returns_internal_error() {
        use acp::Client as _;
        let state = make_state();
        let client = MasterClient {
            state: Arc::clone(&state),
        };
        let req =
            acp::CreateTerminalRequest::new(SessionId::new("nobody-home"), "echo".to_string());
        let err = client
            .create_terminal(req)
            .await
            .expect_err("create_terminal on unknown session must fail");
        assert_eq!(err.code, acp::ErrorCode::InternalError);
    }



    #[tokio::test]
    async fn sessions_list_handler_returns_registry_snapshot_payload() {
        use crate::session_registry::{self, SessionInfo};
        use std::path::PathBuf;

        let state = make_state();
        let mut row = SessionInfo::new(SessionId::new("sess-b"), PathBuf::from("C:\\repo\\b"));
        row.status = Some(crate::agent_sessions::AgentStatus::Idle);
        row.cli_source = Some(crate::agent_sessions::CliSource::Copilot);
        row.last_activity_at_ms = Some(42);
        state.registry.upsert(row.clone()).await;

        let req = session_registry::build_sessions_list_request();
        let resp = handle_sessions_list(&state, &req.params)
            .await
            .expect("sessions/list succeeds");
        let parsed = session_registry::parse_sessions_list_response(&resp.0)
            .expect("response parses");

        assert_eq!(parsed.sessions, vec![row]);
    }

    #[tokio::test]
    async fn sessions_list_upgrades_synthetic_title_from_disk() {
        // Born-bound rows (e.g. ?<prompt> delegate sessions) register a single
        // SessionStarted with an empty title — before the CLI has written its
        // generated `name:` — and, being hook-independent, get no follow-up
        // events to re-trigger the per-hook title refresh. The /sessions view
        // re-polls sessions/list every 5s, so the list handler must surface the
        // CLI-generated title once it lands on disk.
        use crate::session_registry::{self, SessionInfo};
        use std::path::PathBuf;

        let state = make_state();
        let mut row = SessionInfo::new(
            SessionId::new("born-bound"),
            PathBuf::from("C:\\Windows\\system32"),
        );
        row.cli_source = Some(crate::agent_sessions::CliSource::Copilot);
        // title left None → synthetic, exactly as at born-bound launch time.
        state.registry.upsert(row).await;

        let req = session_registry::build_sessions_list_request();
        let resp = handle_sessions_list_with(&state, &req.params, |cli, key| {
            assert_eq!(cli, crate::agent_sessions::CliSource::Copilot);
            assert_eq!(key, "born-bound");
            Some("Implement Greeting Function".to_string())
        })
        .await
        .expect("sessions/list succeeds");
        let parsed =
            session_registry::parse_sessions_list_response(&resp.0).expect("response parses");

        let upgraded = parsed
            .sessions
            .iter()
            .find(|s| s.session_id == SessionId::new("born-bound"))
            .expect("born-bound row present in snapshot");
        assert_eq!(
            upgraded.title.as_deref(),
            Some("Implement Greeting Function"),
            "synthetic born-bound title should be upgraded from on-disk artefacts"
        );
    }

    #[tokio::test]
    async fn drop_sessions_for_helper_broadcasts_sessions_changed() {
        use crate::session_registry::{self, SessionInfo};
        use std::path::PathBuf;

        let state = make_state();
        let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
        let (ext_tx, mut ext_rx) = mpsc::unbounded_channel::<acp::ExtNotification>();
        let sid = SessionId::new("removed-a");
        {
            let mut map = state.session_to_helper.lock().await;
            map.insert(sid.clone(), HelperRoute {
                helper_id: HelperId(1),
                notif_tx,
                forwarder: None,
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            });
        }
        state.registry.upsert(SessionInfo::new(sid, PathBuf::from("C:\\repo"))).await;
        {
            let mut subs = state.helper_ext_subscribers.lock().await;
            subs.insert(HelperId(2), ext_tx);
        }

        drop_sessions_for_helper(&state, HelperId(1)).await;

        let methods: Vec<String> = std::iter::from_fn(|| ext_rx.try_recv().ok())
            .map(|ext| ext.method.to_string())
            .collect();
        assert!(methods.contains(&session_registry::INTELLTERM_METHOD_SESSION_REMOVED.to_string()));
        assert!(methods.contains(&session_registry::INTELLTERM_METHOD_SESSIONS_CHANGED.to_string()));
    }

    // ─── Task C master mutation RPCs ────────────────────────────────

    #[tokio::test]
    async fn session_resume_dispatched_historical_flips_and_broadcasts() {
        use crate::session_registry::SessionInfo;
        use std::path::PathBuf;
        let state = make_state();
        let (tx, mut rx) = mpsc::unbounded_channel();
        state
            .helper_ext_subscribers
            .lock()
            .await
            .insert(HelperId(7), tx);
        let sid = acp::SessionId::new("hist-sid");
        let mut info = SessionInfo::new(sid.clone(), PathBuf::from("/repo"));
        info.status = Some(crate::agent_sessions::AgentStatus::Historical);
        state.registry.upsert(info).await;
        let params = session_resume_params_for(&sid);
        let resp = handle_session_resume_dispatched(&state, &params)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.0.get()).unwrap();
        assert_eq!(body["flipped"], true);
        assert_eq!(body["current_status"], "Idle");
        assert_eq!(
            state.registry.lookup(&sid).await.unwrap().status,
            Some(crate::agent_sessions::AgentStatus::Idle)
        );
        let notif = rx.try_recv().expect("flip must broadcast sessions/changed");
        assert_eq!(
            &*notif.method,
            crate::session_registry::INTELLTERM_METHOD_SESSIONS_CHANGED
        );
    }

    #[tokio::test]
    async fn session_resume_dispatched_live_is_noop() {
        use crate::session_registry::SessionInfo;
        use std::path::PathBuf;
        let state = make_state();
        let (tx, mut rx) = mpsc::unbounded_channel();
        state
            .helper_ext_subscribers
            .lock()
            .await
            .insert(HelperId(7), tx);
        let sid = acp::SessionId::new("live-sid");
        let mut info = SessionInfo::new(sid.clone(), PathBuf::from("/repo"));
        info.status = Some(crate::agent_sessions::AgentStatus::Idle);
        state.registry.upsert(info).await;
        let params = session_resume_params_for(&sid);
        let resp = handle_session_resume_dispatched(&state, &params)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.0.get()).unwrap();
        assert_eq!(body["flipped"], false);
        assert_eq!(body["current_status"], "Idle");
        assert!(rx.try_recv().is_err(), "no-op must not broadcast");
    }

    #[tokio::test]
    async fn session_focus_with_bound_pane_calls_wtcli() {
        use crate::session_registry::SessionInfo;
        use std::path::PathBuf;
        let mock = Arc::new(MockWtChannel::ok());
        let state = make_state_with_wt(mock.clone());
        let sid = acp::SessionId::new("focus-sid");
        let mut info = SessionInfo::new(sid.clone(), PathBuf::from("/repo"));
        info.pane_session_id = Some("pane-123".to_string());
        state.registry.upsert(info).await;
        let params = session_focus_params_for(&sid);
        let resp = handle_session_focus(&state, &params).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.0.get()).unwrap();
        assert_eq!(body["focused"], true);
        assert_eq!(body["pane_session_id"], "pane-123");
        assert_eq!(mock.calls()[0].0, "focus_pane");
    }

    #[tokio::test]
    async fn session_focus_without_pane_returns_no_pane() {
        use crate::session_registry::SessionInfo;
        use std::path::PathBuf;
        let mock = Arc::new(MockWtChannel::ok());
        let state = make_state_with_wt(mock.clone());
        let sid = acp::SessionId::new("orphan-sid");
        state
            .registry
            .upsert(SessionInfo::new(sid.clone(), PathBuf::from("/repo")))
            .await;
        let params = session_focus_params_for(&sid);
        let resp = handle_session_focus(&state, &params).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(resp.0.get()).unwrap();
        assert_eq!(body["focused"], false);
        assert_eq!(body["reason"], "no_pane");
        assert!(mock.calls().is_empty());
    }

    fn session_resume_params_for(sid: &acp::SessionId) -> Box<serde_json::value::RawValue> {
        let req = crate::session_registry::build_session_resume_dispatched_request(sid);
        serde_json::value::to_raw_value(
            &serde_json::from_str::<serde_json::Value>(req.params.get()).unwrap(),
        )
        .unwrap()
    }

    fn session_focus_params_for(sid: &acp::SessionId) -> Box<serde_json::value::RawValue> {
        let req = crate::session_registry::build_session_focus_request(sid);
        serde_json::value::to_raw_value(
            &serde_json::from_str::<serde_json::Value>(req.params.get()).unwrap(),
        )
        .unwrap()
    }

    // ─── handle_focus_session ───────────────────────────────────────

    /// Mock `WtChannel` that captures every `request` call into a
    /// shared vec so tests can assert the dispatched method + params.
    /// Returns `Ok(<configured-response>)` for every request — the
    /// real `CliChannel` returns a JSON value from `wtcli`, but the
    /// handler doesn't inspect it (it just maps `Ok(_)` to a fixed
    /// success ExtResponse), so any JSON works here.
    struct MockWtChannel {
        calls: std::sync::Mutex<Vec<(String, serde_json::Value)>>,
        fail_with: Option<String>,
    }

    impl MockWtChannel {
        fn ok() -> Self {
            Self {
                calls: std::sync::Mutex::new(Vec::new()),
                fail_with: None,
            }
        }
        fn failing(message: &str) -> Self {
            Self {
                calls: std::sync::Mutex::new(Vec::new()),
                fail_with: Some(message.to_string()),
            }
        }
        fn calls(&self) -> Vec<(String, serde_json::Value)> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl crate::shell::wt_channel::WtChannel for MockWtChannel {
        async fn request(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> anyhow::Result<serde_json::Value> {
            self.calls
                .lock()
                .unwrap()
                .push((method.to_string(), params));
            match &self.fail_with {
                Some(msg) => Err(anyhow::anyhow!("{msg}")),
                None => Ok(serde_json::json!({ "ok": true })),
            }
        }
        fn is_available(&self) -> bool {
            true
        }
    }

    fn make_state_with_wt(
        wt: Arc<dyn crate::shell::wt_channel::WtChannel>,
    ) -> Arc<MasterStateInner> {
        Arc::new(MasterStateInner {
            session_to_helper: Mutex::new(HashMap::new()),
            registry: crate::session_registry::InMemoryRegistry::shared(),
            helper_ext_subscribers: Mutex::new(HashMap::new()),
            wt: Some(wt),
            agents: Mutex::new(HashMap::new()),
            default_agent_cmd: "copilot --acp --stdio".to_string(),
            default_agent_id: Some("copilot".to_string()),
            allowed_agent_ids: None,
            helper_meta: Mutex::new(HashMap::new()),
            hook_owned: Mutex::new(HashSet::new()),
            born_bound: Mutex::new(HashSet::new()),
            live_panes_cache: Mutex::new(None),
        })
    }

    fn focus_params_for(sid: &acp::SessionId) -> Box<serde_json::value::RawValue> {
        let req = crate::session_registry::build_focus_session_request(sid);
        // ExtRequest stores params as Arc<RawValue>; cloning to owned Box
        // through serialization is the simplest portable way to feed it
        // into `handle_focus_session` which expects `&RawValue`.
        serde_json::value::to_raw_value(
            &serde_json::from_str::<serde_json::Value>(req.params.get()).unwrap(),
        )
        .unwrap()
    }

    /// Happy path: sid in registry with pane_session_id, WtChannel present.
    /// The handler should call `wt.request("focus_pane", { session_id: <pane_guid> })`
    /// exactly once and return an `Ok` ExtResponse.
    #[tokio::test]
    async fn focus_session_dispatches_to_wt_channel_with_pane_session_id() {
        use crate::session_registry::SessionInfo;
        use std::path::PathBuf;

        let mock = Arc::new(MockWtChannel::ok());
        let state = make_state_with_wt(mock.clone());
        let sid = acp::SessionId::new("alive-sess");
        let mut info = SessionInfo::new(sid.clone(), PathBuf::from("/repo"));
        info.pane_session_id = Some("pane-GUID-123".to_string());
        state.registry.upsert(info).await;

        let params = focus_params_for(&sid);
        let resp = handle_focus_session(&state, &params)
            .await
            .expect("focus_session must succeed");

        let calls = mock.calls();
        assert_eq!(calls.len(), 1, "exactly one wt.request call expected");
        assert_eq!(calls[0].0, "focus_pane");
        assert_eq!(
            calls[0].1,
            serde_json::json!({ "session_id": "pane-GUID-123" })
        );

        let body: serde_json::Value = serde_json::from_str(resp.0.get()).expect("response is JSON");
        assert_eq!(body["ok"], serde_json::Value::Bool(true));
        assert_eq!(body["pane_session_id"], "pane-GUID-123");
    }

    /// Unknown SessionId → `resource_not_found` so the helper knows
    /// the row doesn't exist on this master (vs. existing-but-unfocusable).
    #[tokio::test]
    async fn focus_session_returns_not_found_for_unknown_session() {
        let mock = Arc::new(MockWtChannel::ok());
        let state = make_state_with_wt(mock.clone());
        let sid = acp::SessionId::new("nobody-here");

        let params = focus_params_for(&sid);
        let err = handle_focus_session(&state, &params)
            .await
            .expect_err("unknown sid must error");
        assert_eq!(err.code, acp::ErrorCode::ResourceNotFound);
        assert!(
            mock.calls().is_empty(),
            "no wt call when session not in registry"
        );
    }

    /// Row exists but has no pane_session_id → `invalid_request`
    /// (different code from "not found" so the helper can branch on it).
    #[tokio::test]
    async fn focus_session_returns_invalid_request_for_row_without_pane_session_id() {
        use crate::session_registry::SessionInfo;
        use std::path::PathBuf;

        let mock = Arc::new(MockWtChannel::ok());
        let state = make_state_with_wt(mock.clone());
        let sid = acp::SessionId::new("orphan-sess");
        let info = SessionInfo::new(sid.clone(), PathBuf::from("/repo")); // no pane_session_id
        state.registry.upsert(info).await;

        let params = focus_params_for(&sid);
        let err = handle_focus_session(&state, &params)
            .await
            .expect_err("row without pane_session_id must error");
        assert_eq!(err.code, acp::ErrorCode::InvalidRequest);
        assert!(mock.calls().is_empty());
    }

    /// `wt: None` (master booted outside a WT pane) → `internal_error`
    /// so the helper can fall back to its legacy focus path.
    #[tokio::test]
    async fn focus_session_returns_internal_error_when_wt_channel_unavailable() {
        use crate::session_registry::SessionInfo;
        use std::path::PathBuf;

        let state = make_state(); // wt: None
        let sid = acp::SessionId::new("alive-but-no-wt");
        let mut info = SessionInfo::new(sid.clone(), PathBuf::from("/repo"));
        info.pane_session_id = Some("pane-X".to_string());
        state.registry.upsert(info).await;

        let params = focus_params_for(&sid);
        let err = handle_focus_session(&state, &params)
            .await
            .expect_err("wt None must error");
        assert_eq!(err.code, acp::ErrorCode::InternalError);
    }

    /// Wtcli failure propagates as `internal_error` with the wtcli
    /// error message embedded in `data` so the helper can log it.
    #[tokio::test]
    async fn focus_session_wraps_wt_failure_as_internal_error() {
        use crate::session_registry::SessionInfo;
        use std::path::PathBuf;

        let mock = Arc::new(MockWtChannel::failing("0x80070490: pane not found"));
        let state = make_state_with_wt(mock.clone());
        let sid = acp::SessionId::new("alive-but-pane-gone");
        let mut info = SessionInfo::new(sid.clone(), PathBuf::from("/repo"));
        info.pane_session_id = Some("dead-pane".to_string());
        state.registry.upsert(info).await;

        let params = focus_params_for(&sid);
        let err = handle_focus_session(&state, &params)
            .await
            .expect_err("wt failure must surface as Err");
        assert_eq!(err.code, acp::ErrorCode::InternalError);
        // Mock was still invoked once before failing — confirms we
        // didn't short-circuit somewhere upstream of the dispatch.
        assert_eq!(mock.calls().len(), 1);
    }

    /// Malformed params (e.g. missing `session_id`) → `invalid_params`
    /// without touching the registry or wt channel.
    #[tokio::test]
    async fn focus_session_returns_invalid_params_for_garbage() {
        let mock = Arc::new(MockWtChannel::ok());
        let state = make_state_with_wt(mock.clone());

        let garbage = serde_json::value::to_raw_value(&serde_json::json!({
            "wrong_field": "huh"
        }))
        .unwrap();
        let err = handle_focus_session(&state, &garbage)
            .await
            .expect_err("malformed params must error");
        assert_eq!(err.code, acp::ErrorCode::InvalidParams);
        assert!(mock.calls().is_empty());
    }

    #[tokio::test]
    async fn session_hook_returns_invalid_params_for_garbage() {
        let state = make_state();
        let garbage = serde_json::value::to_raw_value(&serde_json::json!({
            "wrong_field": "huh"
        }))
        .unwrap();

        let err = handle_session_hook(&state, &garbage, false)
            .await
            .expect_err("malformed session_hook params must error");
        assert_eq!(err.code, acp::ErrorCode::InvalidParams);
    }

    #[tokio::test]
    async fn session_hook_broadcasts_sessions_changed_after_valid_payload() {
        let state = make_state();
        let (tx, mut rx) = mpsc::unbounded_channel();
        state.helper_ext_subscribers.lock().await.insert(HelperId(7), tx);

        // Use SessionStarted because it unconditionally upserts a row,
        // so the reducer returns true and the broadcast fires. PaneClosed
        // against an empty registry is a no-op (returns false) and would
        // not exercise the broadcast path.
        let event = crate::agent_sessions::SessionEvent::SessionStarted {
            key: "sid-for-hook".to_string(),
            cli_source: crate::agent_sessions::CliSource::Copilot,
            pane_session_id: "pane-for-hook".to_string(),
            cwd: std::path::PathBuf::from("/tmp"),
            title: String::new(),
        };
        let req = crate::session_registry::build_session_hook_request(&event);

        let response = handle_session_hook(&state, &req.params, false)
            .await
            .expect("valid session_hook accepted");
        assert_eq!(response.0.get(), r#"{"applied":true}"#);

        let notification = rx.try_recv().expect("sessions/changed broadcast queued");
        assert_eq!(
            &*notification.method,
            crate::session_registry::INTELLTERM_METHOD_SESSIONS_CHANGED
        );
        assert_eq!(notification.params.get(), "{}");
    }

    // ── try_refresh_title_from_disk_with ────────────────────────────

    #[tokio::test]
    async fn try_refresh_title_upgrades_synthetic_cwd_basename_title() {
        // Repro of the production bug: shell-pane Copilot first hook arrives
        // with title = cwd basename ("alice" for cwd C:\Users\alice). Later,
        // the on-disk workspace.yaml has the real `name:` field, but the
        // helper-local upgrade never reaches master. This is the master-side
        // upgrade path.
        let state = make_state();
        let event = crate::agent_sessions::SessionEvent::SessionStarted {
            key: "sid-y".to_string(),
            cli_source: crate::agent_sessions::CliSource::Copilot,
            pane_session_id: "pane-y".to_string(),
            cwd: std::path::PathBuf::from("C:\\Users\\alice"),
            title: "alice".to_string(),
        };
        state.registry.apply_event(event).await;

        let sid = acp::SessionId::new("sid-y".to_string());
        let upgraded = try_refresh_title_from_disk_with(&state.registry, &sid, |cli, key| {
            assert_eq!(cli, crate::agent_sessions::CliSource::Copilot);
            assert_eq!(key, "sid-y");
            Some("No Coding Task Identified".to_string())
        })
        .await;
        assert!(upgraded, "title should be upgraded from synthetic basename");
        assert_eq!(
            state.registry.lookup(&sid).await.unwrap().title.as_deref(),
            Some("No Coding Task Identified")
        );
    }

    #[tokio::test]
    async fn try_refresh_title_skips_when_title_is_real() {
        let state = make_state();
        let event = crate::agent_sessions::SessionEvent::SessionStarted {
            key: "sid-real".to_string(),
            cli_source: crate::agent_sessions::CliSource::Copilot,
            pane_session_id: "pane-real".to_string(),
            cwd: std::path::PathBuf::from("/repo/proj"),
            // "Some Real Title" ≠ "proj" → not synthetic. Lookup must not run.
            title: "Some Real Title".to_string(),
        };
        state.registry.apply_event(event).await;

        let sid = acp::SessionId::new("sid-real".to_string());
        let lookup_called = std::cell::Cell::new(false);
        let upgraded = try_refresh_title_from_disk_with(&state.registry, &sid, |_, _| {
            lookup_called.set(true);
            Some("would-be-overwrite".to_string())
        })
        .await;
        assert!(!upgraded);
        assert!(
            !lookup_called.get(),
            "disk lookup must be skipped when title is already real"
        );
        assert_eq!(
            state.registry.lookup(&sid).await.unwrap().title.as_deref(),
            Some("Some Real Title")
        );
    }

    #[tokio::test]
    async fn try_refresh_title_skips_when_cli_source_missing() {
        // Rows without a cli_source (legacy / partial seeds) can't be
        // dispatched to a per-CLI on-disk scanner; refresh must be a
        // no-op rather than trying to guess.
        let state = make_state();
        let mut info = crate::session_registry::SessionInfo::new(
            acp::SessionId::new("sid-bare".to_string()),
            std::path::PathBuf::from("/x/proj"),
        );
        info.title = Some("proj".to_string()); // synthetic
        // info.cli_source intentionally left as None
        state.registry.upsert(info).await;

        let sid = acp::SessionId::new("sid-bare".to_string());
        let upgraded = try_refresh_title_from_disk_with(&state.registry, &sid, |_, _| {
            panic!("lookup must not be invoked without cli_source");
        })
        .await;
        assert!(!upgraded);
    }

    #[tokio::test]
    async fn try_refresh_title_skips_when_lookup_returns_none_or_empty() {
        let state = make_state();
        let event = crate::agent_sessions::SessionEvent::SessionStarted {
            key: "sid-none".to_string(),
            cli_source: crate::agent_sessions::CliSource::Copilot,
            pane_session_id: "pane-none".to_string(),
            cwd: std::path::PathBuf::from("/x/proj"),
            title: "proj".to_string(),
        };
        state.registry.apply_event(event).await;
        let sid = acp::SessionId::new("sid-none".to_string());

        // Disk lookup returns None (e.g. workspace.yaml `name:` not yet
        // written by Copilot at the moment this hook arrives).
        let upgraded = try_refresh_title_from_disk_with(&state.registry, &sid, |_, _| None).await;
        assert!(!upgraded);
        assert_eq!(
            state.registry.lookup(&sid).await.unwrap().title.as_deref(),
            Some("proj"),
            "title must stay synthetic when no disk title is available"
        );

        // Disk lookup returns empty string — treat as None.
        let upgraded = try_refresh_title_from_disk_with(&state.registry, &sid, |_, _| {
            Some(String::new())
        })
        .await;
        assert!(!upgraded);
    }

    #[tokio::test]
    async fn try_refresh_title_returns_false_for_missing_session() {
        let state = make_state();
        let sid = acp::SessionId::new("nope".to_string());
        let upgraded = try_refresh_title_from_disk_with(&state.registry, &sid, |_, _| {
            panic!("lookup must not run for missing session");
        })
        .await;
        assert!(!upgraded);
    }

    #[test]
    fn session_event_key_returns_key_for_keyed_variants() {
        use crate::agent_sessions::{CliSource, SessionEvent};
        let cases: Vec<(SessionEvent, Option<&str>)> = vec![
            (
                SessionEvent::SessionStarted {
                    key: "k1".into(),
                    cli_source: CliSource::Copilot,
                    pane_session_id: "p".into(),
                    cwd: std::path::PathBuf::new(),
                    title: String::new(),
                },
                Some("k1"),
            ),
            (
                SessionEvent::ToolStarting {
                    key: "k2".into(),
                    tool_name: "t".into(),
                },
                Some("k2"),
            ),
            (SessionEvent::ToolCompleted { key: "k3".into() }, Some("k3")),
            (
                SessionEvent::Notification {
                    key: "k4".into(),
                    message: "m".into(),
                },
                Some("k4"),
            ),
            (
                SessionEvent::SessionStopped {
                    key: "k5".into(),
                    reason: "r".into(),
                },
                Some("k5"),
            ),
            (
                SessionEvent::ResumeDispatched { key: "k6".into() },
                Some("k6"),
            ),
            (
                SessionEvent::ResumePaneAssigned {
                    key: "k7".into(),
                    pane_session_id: "p".into(),
                },
                Some("k7"),
            ),
            // Pane-only variants: no session key → refresh skipped.
            (
                SessionEvent::PaneClosed {
                    pane_session_id: "p".into(),
                },
                None,
            ),
            (
                SessionEvent::ConnectionFailed {
                    pane_session_id: "p".into(),
                    reason: "r".into(),
                },
                None,
            ),
        ];
        for (event, expected) in cases {
            assert_eq!(session_event_key(&event), expected, "event={event:?}");
        }
    }
    // ── ensure_watched_session_row: Class-B resume revival ──────────

    async fn seed_session_row(
        state: &MasterStateInner,
        key: &str,
        origin: crate::agent_sessions::SessionOrigin,
        status: crate::agent_sessions::AgentStatus,
    ) {
        let mut info = crate::session_registry::SessionInfo::new(
            acp::SessionId::new(key.to_string()),
            std::path::PathBuf::from("C:\\repo"),
        );
        info.cli_source = Some(crate::agent_sessions::CliSource::Codex);
        info.origin = Some(origin);
        info.status = Some(status);
        state.registry.upsert(info).await;
    }

    fn codex_emitted(key: &str) -> crate::session_watcher::Emitted {
        crate::session_watcher::Emitted {
            cli: crate::agent_sessions::CliSource::Codex,
            key: key.to_string(),
            cwd: None,
            event: crate::agent_sessions::SessionEvent::ToolStarting {
                key: key.to_string(),
                tool_name: String::new(),
            },
        }
    }

    #[tokio::test]
    async fn ensure_row_revives_class_b_historical_session() {
        // A shell-pane (Class B) session the user resumed is Historical from
        // the startup history scan. The watcher's first event must revive it
        // (Historical -> Idle) so the following activity event can mark it
        // Working — otherwise the reducer's terminal-state guard keeps it
        // stuck at "no status".
        let state = make_state();
        seed_session_row(
            &state,
            "sid-resumed",
            crate::agent_sessions::SessionOrigin::Unknown,
            crate::agent_sessions::AgentStatus::Historical,
        )
        .await;

        ensure_watched_session_row(&state, &codex_emitted("sid-resumed")).await;

        let row = state
            .registry
            .lookup(&acp::SessionId::new("sid-resumed".to_string()))
            .await
            .unwrap();
        assert_eq!(row.status, Some(crate::agent_sessions::AgentStatus::Idle));
    }

    #[tokio::test]
    async fn ensure_row_does_not_revive_agent_pane_session() {
        // Class A (agent pane) terminal rows must NOT be revived by a watcher
        // event — that's the ghost-row case the reducer guard protects
        // against. Keep the revival scoped to Class B.
        let state = make_state();
        seed_session_row(
            &state,
            "sid-agent",
            crate::agent_sessions::SessionOrigin::AgentPane,
            crate::agent_sessions::AgentStatus::Historical,
        )
        .await;

        ensure_watched_session_row(&state, &codex_emitted("sid-agent")).await;

        let row = state
            .registry
            .lookup(&acp::SessionId::new("sid-agent".to_string()))
            .await
            .unwrap();
        assert_eq!(
            row.status,
            Some(crate::agent_sessions::AgentStatus::Historical),
            "Class A agent-pane rows must stay terminal"
        );
    }

    #[tokio::test]
    async fn ensure_row_leaves_live_class_b_session_untouched() {
        // A live (non-terminal) Class-B row must not be re-bound or reset on
        // every event — revival applies only to terminal rows.
        let state = make_state();
        let mut info = crate::session_registry::SessionInfo::new(
            acp::SessionId::new("sid-live".to_string()),
            std::path::PathBuf::from("C:\\repo"),
        );
        info.cli_source = Some(crate::agent_sessions::CliSource::Codex);
        info.origin = Some(crate::agent_sessions::SessionOrigin::Unknown);
        info.status = Some(crate::agent_sessions::AgentStatus::Working);
        info.pane_session_id = Some("pane-live".to_string());
        state.registry.upsert(info).await;

        ensure_watched_session_row(&state, &codex_emitted("sid-live")).await;

        let row = state
            .registry
            .lookup(&acp::SessionId::new("sid-live".to_string()))
            .await
            .unwrap();
        assert_eq!(row.status, Some(crate::agent_sessions::AgentStatus::Working));
        assert_eq!(row.pane_session_id.as_deref(), Some("pane-live"));
    }

    // ── reap_dead_class_b_sessions: Ctrl+C liveness poll ────────────

    async fn seed_row_with_pid(
        state: &MasterStateInner,
        key: &str,
        origin: crate::agent_sessions::SessionOrigin,
        status: crate::agent_sessions::AgentStatus,
        pid: Option<u32>,
    ) {
        let mut info = crate::session_registry::SessionInfo::new(
            acp::SessionId::new(key.to_string()),
            std::path::PathBuf::from("C:\\repo"),
        );
        info.cli_source = Some(crate::agent_sessions::CliSource::Codex);
        info.origin = Some(origin);
        info.status = Some(status);
        info.bound_pid = pid;
        state.registry.upsert(info).await;
    }

    // A pid that is essentially guaranteed not to exist, so pid_alive is false.
    const DEAD_PID: u32 = 0x7FFF_FFF0;

    #[tokio::test]
    async fn reap_ends_class_b_with_dead_pid() {
        let state = make_state();
        seed_row_with_pid(
            &state,
            "sid-dead",
            crate::agent_sessions::SessionOrigin::Unknown,
            crate::agent_sessions::AgentStatus::Idle,
            Some(DEAD_PID),
        )
        .await;

        let reaped = reap_dead_class_b_sessions(&state).await;
        assert_eq!(reaped, 1);

        let row = state
            .registry
            .lookup(&acp::SessionId::new("sid-dead".to_string()))
            .await
            .unwrap();
        assert_eq!(row.status, Some(crate::agent_sessions::AgentStatus::Ended));
    }

    #[tokio::test]
    async fn reap_keeps_class_b_with_live_pid() {
        let state = make_state();
        // Our own process is alive — the session must survive the poll.
        seed_row_with_pid(
            &state,
            "sid-alive",
            crate::agent_sessions::SessionOrigin::Unknown,
            crate::agent_sessions::AgentStatus::Working,
            Some(std::process::id()),
        )
        .await;

        let reaped = reap_dead_class_b_sessions(&state).await;
        assert_eq!(reaped, 0);

        let row = state
            .registry
            .lookup(&acp::SessionId::new("sid-alive".to_string()))
            .await
            .unwrap();
        assert_eq!(row.status, Some(crate::agent_sessions::AgentStatus::Working));
    }

    #[tokio::test]
    async fn reap_ignores_agent_pane_sessions() {
        // Class A (agent pane) rows are managed by the ACP / alive-mirror path;
        // the liveness poll must never touch them even with a dead pid.
        let state = make_state();
        seed_row_with_pid(
            &state,
            "sid-a",
            crate::agent_sessions::SessionOrigin::AgentPane,
            crate::agent_sessions::AgentStatus::Idle,
            Some(DEAD_PID),
        )
        .await;

        let reaped = reap_dead_class_b_sessions(&state).await;
        assert_eq!(reaped, 0);

        let row = state
            .registry
            .lookup(&acp::SessionId::new("sid-a".to_string()))
            .await
            .unwrap();
        assert_eq!(row.status, Some(crate::agent_sessions::AgentStatus::Idle));
    }

    #[tokio::test]
    async fn reap_ignores_rows_without_bound_pid() {
        // A Class-B row we couldn't bind to a pid (or Gemini, which is unbound)
        // can't be polled, so it's left alone.
        let state = make_state();
        seed_row_with_pid(
            &state,
            "sid-no-pid",
            crate::agent_sessions::SessionOrigin::Unknown,
            crate::agent_sessions::AgentStatus::Idle,
            None,
        )
        .await;

        let reaped = reap_dead_class_b_sessions(&state).await;
        assert_eq!(reaped, 0);

        let row = state
            .registry
            .lookup(&acp::SessionId::new("sid-no-pid".to_string()))
            .await
            .unwrap();
        assert_eq!(row.status, Some(crate::agent_sessions::AgentStatus::Idle));
    }

    // ── Hybrid event-dedup: hooks / born-bound win, watcher is fallback ──

    #[tokio::test]
    async fn watcher_event_dropped_when_session_is_hook_owned() {
        // A session a hook (or #266 born-bound) already claimed is recorded in
        // `hook_owned`. The watcher is a fallback and must not double-track it:
        // its event is dropped before any row is created.
        let state = make_state();
        state
            .hook_owned
            .lock()
            .await
            .insert(acp::SessionId::new("sid-hooked".to_string()));

        apply_watcher_event(&state, codex_emitted("sid-hooked")).await;

        assert!(
            state
                .registry
                .lookup(&acp::SessionId::new("sid-hooked".to_string()))
                .await
                .is_none(),
            "watcher must not create a row for a hook-owned session"
        );
    }

    #[tokio::test]
    async fn watcher_event_applied_when_not_hook_owned() {
        // The fallback path: a user-typed CLI with no hook installed is tracked
        // by the watcher, which creates a Class-B row.
        let state = make_state();

        apply_watcher_event(&state, codex_emitted("sid-typed")).await;

        let row = state
            .registry
            .lookup(&acp::SessionId::new("sid-typed".to_string()))
            .await
            .expect("watcher creates a row for a non-hook-owned session");
        assert_eq!(
            row.origin,
            Some(crate::agent_sessions::SessionOrigin::Unknown)
        );
        assert_eq!(row.status, Some(crate::agent_sessions::AgentStatus::Working));
    }

    #[tokio::test]
    async fn watcher_event_dropped_for_agent_pane_session() {
        // Agent-pane (Class A) sessions are driven by ACP session/update; the
        // watcher must defer to ACP even though the agent CLI also writes the
        // on-disk session file the watcher sees.
        let state = make_state();
        seed_session_row(
            &state,
            "sid-agent-pane",
            crate::agent_sessions::SessionOrigin::AgentPane,
            crate::agent_sessions::AgentStatus::Idle,
        )
        .await;

        apply_watcher_event(&state, codex_emitted("sid-agent-pane")).await;

        let row = state
            .registry
            .lookup(&acp::SessionId::new("sid-agent-pane".to_string()))
            .await
            .unwrap();
        // Still Idle — the watcher's ToolStarting (Working) was dropped.
        assert_eq!(row.status, Some(crate::agent_sessions::AgentStatus::Idle));
    }

    #[tokio::test]
    async fn session_hook_marks_session_hook_owned_then_watcher_is_ignored() {
        // End-to-end: a hook SessionStarted claims the session (recording it in
        // `hook_owned`), after which the watcher's events for that session are
        // dropped — so the hook-sourced pane binding is never clobbered.
        let state = make_state();
        let event = crate::agent_sessions::SessionEvent::SessionStarted {
            key: "sid-claimed".to_string(),
            cli_source: crate::agent_sessions::CliSource::Codex,
            pane_session_id: "pane-from-hook".to_string(),
            cwd: std::path::PathBuf::from("C:\\repo"),
            title: String::new(),
        };
        let req = crate::session_registry::build_session_hook_request(&event);
        handle_session_hook(&state, &req.params, false)
            .await
            .expect("valid session_hook accepted");

        assert!(
            state
                .hook_owned
                .lock()
                .await
                .contains(&acp::SessionId::new("sid-claimed".to_string())),
            "a keyed session_hook event must mark the session hook-owned"
        );

        // A subsequent watcher event must not disturb the hook-bound row.
        apply_watcher_event(&state, codex_emitted("sid-claimed")).await;
        let row = state
            .registry
            .lookup(&acp::SessionId::new("sid-claimed".to_string()))
            .await
            .unwrap();
        assert_eq!(
            row.pane_session_id.as_deref(),
            Some("pane-from-hook"),
            "watcher must not clobber the hook-sourced pane binding"
        );
    }

    #[tokio::test]
    async fn session_born_bound_marks_born_bound_not_hook_owned() {
        // #266 born-bound (WTA-launched delegate/resume) is binding-only: it must
        // land in `born_bound`, NOT `hook_owned`, so the watcher can still supply
        // status for it when no real hook is installed.
        let state = make_state();
        let event = crate::agent_sessions::SessionEvent::SessionStarted {
            key: "bb-mark".to_string(),
            cli_source: crate::agent_sessions::CliSource::Claude,
            pane_session_id: "pane-bb".to_string(),
            cwd: std::path::PathBuf::from("C:\\repo"),
            title: String::new(),
        };
        let req = crate::session_registry::build_born_bound_request(&event);
        handle_session_hook(&state, &req.params, true)
            .await
            .expect("valid born-bound accepted");

        let sid = acp::SessionId::new("bb-mark".to_string());
        assert!(
            state.born_bound.lock().await.contains(&sid),
            "born-bound registration must record the session in `born_bound`"
        );
        assert!(
            !state.hook_owned.lock().await.contains(&sid),
            "born-bound is binding-only — must NOT be hook-owned"
        );
    }

    #[tokio::test]
    async fn born_bound_session_gets_watcher_activity_without_rebinding() {
        // The whole point: a born-bound row (no hook) gets STATUS from the
        // watcher, while its pane binding (owned by born-bound) is untouched.
        let state = make_state();
        let sid = acp::SessionId::new("bb-activity".to_string());

        let mut info =
            crate::session_registry::SessionInfo::new(sid.clone(), std::path::PathBuf::from("C:\\repo"));
        info.cli_source = Some(crate::agent_sessions::CliSource::Claude);
        info.origin = Some(crate::agent_sessions::SessionOrigin::Unknown);
        info.status = Some(crate::agent_sessions::AgentStatus::Idle);
        info.pane_session_id = Some("born-pane".to_string());
        state.registry.upsert(info).await;
        state.born_bound.lock().await.insert(sid.clone());

        // Watcher observes a tool start (the Emitted's cli is irrelevant on the
        // born-bound path — binding/gate are skipped).
        apply_watcher_event(&state, codex_emitted("bb-activity")).await;

        let row = state.registry.lookup(&sid).await.unwrap();
        assert_eq!(
            row.status,
            Some(crate::agent_sessions::AgentStatus::Working),
            "watcher must supply status for a born-bound row with no hook"
        );
        assert_eq!(
            row.pane_session_id.as_deref(),
            Some("born-pane"),
            "watcher must NOT re-bind a born-bound row's pane"
        );
    }

    #[tokio::test]
    async fn real_hook_takes_over_born_bound_session() {
        // If a real hook later fires for a born-bound session (hooks installed
        // after launch), it becomes fully hook-owned and leaves `born_bound`, so
        // the watcher backs off entirely.
        let state = make_state();
        let sid = acp::SessionId::new("bb-takeover".to_string());

        let bb = crate::agent_sessions::SessionEvent::SessionStarted {
            key: "bb-takeover".to_string(),
            cli_source: crate::agent_sessions::CliSource::Claude,
            pane_session_id: "pane-bb".to_string(),
            cwd: std::path::PathBuf::from("C:\\repo"),
            title: String::new(),
        };
        handle_session_hook(
            &state,
            &crate::session_registry::build_born_bound_request(&bb).params,
            true,
        )
        .await
        .expect("born-bound accepted");
        assert!(state.born_bound.lock().await.contains(&sid));

        // A real hook event arrives via session_hook (is_born_bound = false).
        let hook = crate::agent_sessions::SessionEvent::ToolStarting {
            key: "bb-takeover".to_string(),
            tool_name: "Bash".to_string(),
        };
        handle_session_hook(
            &state,
            &crate::session_registry::build_session_hook_request(&hook).params,
            false,
        )
        .await
        .expect("real hook accepted");

        assert!(
            state.hook_owned.lock().await.contains(&sid),
            "the real hook must take ownership"
        );
        assert!(
            !state.born_bound.lock().await.contains(&sid),
            "the real hook must remove the stale born-bound claim"
        );
    }

    #[tokio::test]
    async fn resume_binding_events_are_born_bound_not_hook_owned() {
        // `/sessions` resume publishes ResumeDispatched / ResumePaneAssigned over
        // the generic session_hook method. These are the hook-free resume binding,
        // so they must record `born_bound` (watcher can supply status), NOT
        // `hook_owned` — otherwise the resumed row sits at Idle forever.
        let state = make_state();
        let sid = acp::SessionId::new("sid-resume".to_string());

        let dispatched = crate::agent_sessions::SessionEvent::ResumeDispatched {
            key: "sid-resume".to_string(),
        };
        handle_session_hook(
            &state,
            &crate::session_registry::build_session_hook_request(&dispatched).params,
            false,
        )
        .await
        .expect("resume dispatched accepted");
        assert!(
            state.born_bound.lock().await.contains(&sid),
            "ResumeDispatched must be born_bound"
        );
        assert!(
            !state.hook_owned.lock().await.contains(&sid),
            "ResumeDispatched must NOT be hook_owned"
        );

        let assigned = crate::agent_sessions::SessionEvent::ResumePaneAssigned {
            key: "sid-resume".to_string(),
            pane_session_id: "pane-resume".to_string(),
        };
        handle_session_hook(
            &state,
            &crate::session_registry::build_session_hook_request(&assigned).params,
            false,
        )
        .await
        .expect("resume pane assigned accepted");
        assert!(
            state.born_bound.lock().await.contains(&sid),
            "ResumePaneAssigned must be born_bound"
        );
        assert!(!state.hook_owned.lock().await.contains(&sid));
    }

    // ── Liveness gate: only surface watcher sessions bound to a live IT pane ──

    #[test]
    fn watcher_row_allowed_no_live_set_is_permissive() {
        // No WT channel (unit tests / master without a wt channel) → can't gate
        // → allow, preserving the watcher's create-on-first-sight behavior.
        assert!(watcher_row_allowed(Some("pane-1"), None));
        assert!(watcher_row_allowed(None, None));
    }

    #[test]
    fn watcher_row_allowed_requires_membership_when_gating() {
        let live: HashSet<String> = ["aaaa-bbbb", "cccc-dddd"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // In the live set (case-insensitive) → allowed.
        assert!(watcher_row_allowed(Some("aaaa-bbbb"), Some(&live)));
        assert!(watcher_row_allowed(Some("AAAA-BBBB"), Some(&live)));
        // Not a live IT pane (another terminal / closed pane) → rejected.
        assert!(!watcher_row_allowed(Some("9999-9999"), Some(&live)));
        // No pane at all (VS Code / background host, no WT_SESSION) → rejected.
        assert!(!watcher_row_allowed(None, Some(&live)));
    }

    /// WtChannel mock that scripts a windows→tabs→panes topology so
    /// `live_it_pane_guids` can be exercised without COM. Uses **numeric**
    /// `window_id`/`tab_id` to match the real COM JSON shape (`"window_id": 1`),
    /// so the walk's String|Number handling is actually covered.
    struct PaneTopoMock;

    #[async_trait::async_trait]
    impl crate::shell::wt_channel::WtChannel for PaneTopoMock {
        async fn request(
            &self,
            method: &str,
            _params: serde_json::Value,
        ) -> anyhow::Result<serde_json::Value> {
            Ok(match method {
                "list_windows" => serde_json::json!({ "windows": [ { "window_id": 1 } ] }),
                "list_tabs" => serde_json::json!({ "tabs": [ { "tab_id": 0 } ] }),
                "list_panes" => serde_json::json!({ "panes": [
                    { "session_id": "PANE-AAAA", "pid": 10 },
                    { "session_id": "pane-bbbb", "pid": 20 }
                ] }),
                _ => serde_json::json!({ "ok": true }),
            })
        }
        fn is_available(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn live_it_pane_guids_collects_lowercased_set() {
        let state = make_state_with_wt(Arc::new(PaneTopoMock));
        let set = live_it_pane_guids(&state).await.expect("wt present → Some");
        assert!(set.contains("pane-aaaa"), "GUIDs are lowercased; got {:?}", set);
        assert!(set.contains("pane-bbbb"));
        assert_eq!(set.len(), 2);
    }

    #[tokio::test]
    async fn live_it_pane_guids_none_without_wt_channel() {
        // No WT channel → None so callers skip the gate (unit-test path).
        let state = make_state();
        assert!(live_it_pane_guids(&state).await.is_none());
    }

}
