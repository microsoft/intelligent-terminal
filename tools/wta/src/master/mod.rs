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

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

/// Per-helper notification channel capacity. Sized for bursty chunk
/// streaming during a single agent turn; well above what a healthy
/// helper pipe needs to drain. If it fills up, the helper's pipe is
/// genuinely stuck and we'd rather drop chunks (with a warning) than
/// back-pressure the agent CLI's I/O loop and freeze every other
/// helper sharing this master.
const NOTIF_CHANNEL_CAPACITY: usize = 1024;
const SESSION_NEW_TIMEOUT_SECS: u64 = 120;
const MASTER_PIPE_DISCOVERY_FILE: &str = "master-pipe.txt";

use agent_client_protocol as acp;
use anyhow::{anyhow, Context, Result};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::{mpsc, Mutex};
use tokio::task::LocalSet;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::protocol::acp::conn;
use crate::protocol::acp::spawn::spawn_agent_process;
use crate::Cli;

/// Opaque identifier for a helper connection. Used in logs only;
/// routing keys off `acp::schema::v1::SessionId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct HelperId(u64);

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
    /// The agent CLI's response to the master's startup initialize.
    /// Replayed verbatim to every helper that calls `initialize` over
    /// its pipe — re-forwarding to the agent CLI returns a stale or
    /// empty `agent_info`, which clears the XAML agent bar
    /// (`AgentLabelText` goes blank, logo hides) because the helper
    /// publishes the empty name out via `agent_status`. Caching here
    /// is also a small perf win — initialize is otherwise a no-op
    /// round trip on every pane open.
    ///
    /// `OnceLock` so we can construct the shared state *before* the
    /// initialize round trip (the `MasterClient` inside
    /// `ClientSideConnection` needs an `Arc<MasterStateInner>` first),
    /// and fill the slot once initialize returns. Every helper
    /// connection happens strictly after that, so the `get()` in
    /// `HelperHandler::initialize` always sees `Some(_)`.
    cached_init_resp: OnceLock<acp::schema::v1::InitializeResponse>,
    /// The agent CLI connection, set once after startup `initialize`.
    /// Used to source HOST session history via `session/list` instead of
    /// reading the CLI's on-disk files.
    agent_conn: OnceLock<conn::ClientLink>,
    /// The CLI provider master is multiplexing. Resolved once at
    /// startup from `cli.agent` via `agent_registry::resolve_agent_id_from_cmd`.
    /// Used to stamp `cli_source` on every SessionInfo upserted from
    /// `session/new` and `session/load` so agent-pane sessions are not
    /// reported with cli_source=None (which would make session management Enter on a
    /// Live row fall through to the resume path and fail with
    /// "unknown CLI"). `None` only when running with an agent CLI we
    /// don't recognize (e.g. `--agent codex` — tracked in CliSource::Unknown
    /// but not surfaced as a known session management filter).
    pub(crate) cli_source: Option<crate::agent_sessions::CliSource>,
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
    hook_owned: Mutex<HashSet<acp::schema::v1::SessionId>>,
    /// #266 born-bound sessions (WTA-launched delegate/resume — copilot/claude/
    /// gemini). **Binding-only**: unlike `hook_owned`, the file watcher may
    /// still supply STATUS for these when no real hook is installed
    /// (activity-only, never re-binding the pane). A subsequent real hook moves
    /// the session into `hook_owned` and out of here, after which the watcher
    /// fully backs off.
    born_bound: Mutex<HashSet<acp::schema::v1::SessionId>>,
    /// Short-TTL cache of the connected agent's raw `session/list` response.
    /// `Some(Some(sessions))` = the agent listed (possibly empty);
    /// `Some(None)` = the last fetch failed / timed out / is unsupported —
    /// negative-cached so a burst of hook/watcher events and the 5s poll share
    /// one round-trip and don't hammer a hung agent. Both the host-history
    /// reconcile and the synthetic-title refresh derive from this one fetch.
    host_list_cache:
        Mutex<Option<(std::time::Instant, Option<std::sync::Arc<[acp::schema::v1::SessionInfo]>>)>>,
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

impl MasterClient {
    async fn request_permission(
        &self,
        args: acp::schema::v1::RequestPermissionRequest,
    ) -> acp::Result<acp::schema::v1::RequestPermissionResponse> {
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

    async fn session_notification(&self, args: acp::schema::v1::SessionNotification) -> acp::Result<()> {
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
        _ => "other",
    }
}

/// `acp::Agent` impl wired into one helper's `AgentSideConnection`.
/// Each helper gets its own `HelperHandler` instance.
#[derive(Clone)]
struct HelperHandler {
    helper_id: HelperId,
    agent_conn: conn::ClientLink,
    state: Arc<MasterStateInner>,
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

    async fn forward_new_session_to_agent(
        &self,
        args: acp::schema::v1::NewSessionRequest,
        timeout: std::time::Duration,
    ) -> acp::Result<acp::schema::v1::NewSessionResponse> {
        let timeout_secs = timeout.as_secs();
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(timeout, self.agent_conn.new_session(args)).await;
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

impl HelperHandler {
    async fn initialize(
        &self,
        args: acp::schema::v1::InitializeRequest,
    ) -> acp::Result<acp::schema::v1::InitializeResponse> {
        tracing::info!(
            target: "master",
            step = "helper→agent",
            op = "initialize",
            helper_id = ?self.helper_id,
            protocol_version = ?args.protocol_version,
            "replaying cached agent initialize to helper"
        );
        // Replay the master-startup initialize response. Re-forwarding
        // to the agent CLI produced empty `agent_info` on most agent
        // backends (they only fill name/version on the FIRST initialize),
        // which propagated as an empty `agent_status` to C++ and blanked
        // the XAML agent label/logo. The cached response is the one
        // ground truth — every helper sees the same agent_info the
        // master saw at boot.
        match self.state.cached_init_resp.get() {
            Some(resp) => Ok(resp.clone()),
            None => {
                // Shouldn't happen — `run_master_loop` always sets the
                // cache before opening the pipe — but degrade gracefully
                // rather than blanking the bar again.
                tracing::error!(
                    target: "master",
                    helper_id = ?self.helper_id,
                    "cached_init_resp missing; falling back to live agent initialize"
                );
                let started = std::time::Instant::now();
                let result = self.agent_conn.initialize(args).await;
                crate::telemetry::log_acp_initialize_complete(
                    started.elapsed().as_secs_f64() * 1000.0,
                    result.is_ok(),
                    "MasterFallback",
                    if result.is_ok() { "" } else { "AcpError" },
                    result.as_ref().err().map(|e| e.code.into()).unwrap_or(0),
                );
                result
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
        self.agent_conn.authenticate(args).await
    }

    async fn new_session(
        &self,
        args: acp::schema::v1::NewSessionRequest,
    ) -> acp::Result<acp::schema::v1::NewSessionResponse> {
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
        info.cli_source = self.state.cli_source.clone();
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
        let (agent_models, agent_current_model) =
            crate::protocol::acp::model_select::models_from_new_session(&resp);
        let agent_model_count = agent_models.len();
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
        args: acp::schema::v1::LoadSessionRequest,
    ) -> acp::Result<acp::schema::v1::LoadSessionResponse> {
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
        match self.agent_conn.load_session(args).await {
            Ok(resp) => {
                let mut info = crate::session_registry::SessionInfo::new(
                    session_id.clone(),
                    cwd_for_registry,
                );
                info.pane_session_id = wta_meta.pane_session_id;
                // See new_session above for rationale — load_session is the
                // resume path and the resumed row must also be Live + tagged.
                info.status = Some(crate::agent_sessions::AgentStatus::Idle);
                info.cli_source = self.state.cli_source.clone();
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
        args: acp::schema::v1::SetSessionModeRequest,
    ) -> acp::Result<acp::schema::v1::SetSessionModeResponse> {
        self.agent_conn.set_session_mode(args).await
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
        self.agent_conn.set_session_config_option(args).await
    }

    /// Answer `session/list` from our own registry (NOT by proxying the
    /// helper's call to the agent CLI). The registry holds both live
    /// sessions and the historical rows seeded at startup / rescan from
    /// the agent's own `session/list` (host) and `wsl_acp` (WSL),
    /// Class-A-filtered by the `agent_pane_origin` index. Proxying the
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
        self.agent_conn
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
                conn::respond_enum(
                    responder,
                    resp.map(acp::schema::v1::AgentResponse::PromptResponse),
                )
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
        self.agent_conn.cancel(args).await
    }

    /// Master answers our own `_intellterm.wta/*` ext methods locally
    /// (without round-tripping to the agent CLI); anything we don't
    /// recognize is forwarded so future agent-native extension methods
    /// still work. Routing + param decoding go through
    /// [`parse_ext_request`](crate::session_registry::parse_ext_request) so the
    /// ACP-1.0 leading-`_` normalization lives in one place and the match below
    /// is exhaustive (a new method is a compile error until it is handled,
    /// instead of silently falling through to the agent CLI).
    async fn ext_method(&self, args: acp::schema::v1::ExtRequest) -> acp::Result<acp::schema::v1::ExtResponse> {
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
            Req::SessionsList(p) => handle_sessions_list(&self.state, &p).await,
            Req::SessionHook(ev) => handle_session_hook(&self.state, ev, false).await,
            Req::SessionBornBound(ev) => handle_session_hook(&self.state, ev, true).await,
            Req::SessionResumeDispatched(p) => handle_session_resume_dispatched(&self.state, &p).await,
            Req::SessionFocus(p) => handle_session_focus(&self.state, &p).await,
            Req::ForwardToAgent(raw) => self.agent_conn.ext_method(raw).await,
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

async fn run_master_loop(cli: Cli, pipe_name: String) -> Result<()> {
    // 0. Start the shared localhost MCP tool server (resolve_command, …) and
    //    publish its URL for helpers to inject into session/new. Best-effort —
    //    if it can't bind, agents simply don't get MCP tools.
    match crate::mcp::start_and_publish().await {
        Some(ep) => tracing::info!(target: "master", mcp_url = %ep.url, "MCP server started"),
        None => tracing::warn!(target: "master", "MCP server not started (bind failed)"),
    }

    // 1. Spawn the agent CLI subprocess. cwd=None: master inherits
    //    Terminal's cwd, which is fine because per-session cwd is
    //    supplied by helpers via `new_session` params.
    let mut spawn_result = spawn_agent_process(&cli.agent, None)
        .with_context(|| format!("failed to spawn agent CLI: {}", cli.agent))?;
    tracing::info!(
        target: "master",
        program = %spawn_result.resolved_program,
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

    // Drain agent stderr to logs so failures are diagnosable. At debug, not
    // warn: most lines are routine adapter chatter (and can echo prompt/file
    // content), so they shouldn't pollute shipping logs or fire as warnings.
    // The agent's actual exit/crash is logged separately at error.
    if let Some(stderr) = spawn_result.child.stderr.take() {
        tokio::task::spawn_local(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "agent_stderr", "{line}");
            }
        });
    }

    // Shutdown channel — when either the agent CLI subprocess exits or
    // the ACP I/O loop ends, the responsible reaper task posts a reason
    // string here, the accept loop wakes from `recv()`, and
    // `run_master_loop` returns `Err`. Returning (rather than
    // `process::exit`) is critical:
    //
    //   * The `tokio::process::Child` (`spawn_agent_process` configures
    //     `kill_on_drop(true)`) is owned by the child reaper task. When
    //     `LocalSet::run_until` returns, the LocalSet drops, cancels
    //     remaining tasks, and the child handle drops — `kill_on_drop`
    //     then reaps surviving descendants. `process::exit` would skip
    //     that path and could orphan agent grandchildren.
    //   * The `WorkerGuard` from `crate::logging::init` is held by
    //     `main()` for the whole process; it only flushes the
    //     non-blocking tracing appender on Drop. `process::exit` skips
    //     that Drop and the final error lines silently vanish. The
    //     graceful path here lets `main()` return so the guard drops in
    //     normal stack unwinding and the "agent CLI exited" diagnostic
    //     actually lands on disk.
    //
    // Capacity 2: at most one child-exit reason + one I/O-loop reason
    // will ever be sent, and both `try_send`s are non-blocking.
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<&'static str>(2);

    // Reap the child so it doesn't zombie if it dies, and signal
    // shutdown when it does. Without this, helpers would stay
    // connected to a master whose backing agent CLI is gone — every
    // prompt would hang waiting on a dead ACP peer, and SharedWta on
    // the C++ side wouldn't respawn the master (its process handle is
    // still alive). Signalling here lets `run_master_loop` return
    // cleanly so SharedWta can spawn a fresh master + agent CLI pair
    // on the next `AcquirePane`.
    let mut child = spawn_result.child;
    let shutdown_tx_child = shutdown_tx.clone();
    tokio::task::spawn_local(async move {
        let reason = match child.wait().await {
            Ok(status) => {
                tracing::error!(
                    target: "master",
                    ?status,
                    "agent CLI exited — initiating master shutdown"
                );
                "agent CLI exited"
            }
            Err(err) => {
                tracing::error!(
                    target: "master",
                    error = %err,
                    "agent CLI wait failed — initiating master shutdown"
                );
                "agent CLI wait failed"
            }
        };
        let _ = shutdown_tx_child.try_send(reason);
        // `child` drops as this task body ends, firing kill_on_drop on
        // any descendants that survived.
    });

    let outgoing = stdin.compat_write();
    let incoming = stdout.compat();

    // 2. Build the shared state + ClientSideConnection. `cached_init_resp`
    //    starts empty and is filled below once the initialize round
    //    trip with the agent CLI completes; helpers can only connect
    //    after that, so they always see the populated cache.
    //
    //    `wt` is best-effort: master usually runs inside a WT pane
    //    (so `WT_COM_CLSID` is set and `CliChannel::connect` succeeds),
    //    but on the rare boot path where it isn't we degrade to
    //    `None` and `handle_focus_session` returns a structured
    //    "focus channel unavailable" error instead of crashing the
    //    helper's ext_method call.
    //
    //    We also take this opportunity to subscribe to WT events so
    //    master can demote rows to Ended on pane-close even when no
    //    wta-helper publishes a `PaneClosed` session_hook. Two
    //    real-world cases this protects against:
    //
    //      * Gemini shell-pane sessions on Ctrl+Shift+W / close-tab:
    //        Gemini's `SessionEnd` hook does not run reliably on hard
    //        kill (confirmed via `hook-trace.log`), and the helper in
    //        the closing pane (if any) dies before its connection_state
    //        handler runs. Without master subscribing directly, the F2
    //        row stays stuck at Idle indefinitely.
    //      * Helper crash / kill: any path that prevents the helper
    //        from observing-then-publishing the event.
    //
    //    Copilot / Claude work today because their Stop / SessionEnd
    //    hooks fire fast enough during the CTRL_CLOSE grace window;
    //    Gemini does not. Subscribing here makes the demotion path
    //    agnostic to hook behavior across all three CLIs.
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
    // Subscribe + start_reader BEFORE wrapping as `dyn WtChannel` (the
    // trait surface doesn't expose event subscription). Single-consumer
    // model — focus_session uses the same channel via `run_wtcli`
    // request/response, which doesn't touch the event sender, so there
    // is no contention.
    let wt_event_rx = wt_cli.as_ref().map(|c| c.subscribe_events());
    if let Some(ref cli) = wt_cli {
        cli.start_reader().await;
    }
    let wt: Option<Arc<dyn crate::shell::wt_channel::WtChannel>> = wt_cli
        .clone()
        .map(|c| c as Arc<dyn crate::shell::wt_channel::WtChannel>);
    let resolved_agent_id = crate::agent_registry::resolve_agent_id_from_cmd(&cli.agent);
    let cli_source = crate::agent_sessions::CliSource::from_agent_id(resolved_agent_id);
    tracing::info!(
        target: "master",
        agent_cmd = %cli.agent,
        resolved_agent_id = %resolved_agent_id,
        cli_source = ?cli_source,
        "master cli_source resolved for session-row stamping"
    );

    let inner = Arc::new(MasterStateInner {
        session_to_helper: Mutex::new(HashMap::new()),
        registry: crate::session_registry::InMemoryRegistry::shared(),
        helper_ext_subscribers: Mutex::new(HashMap::new()),
        wt,
        cached_init_resp: OnceLock::new(),
        agent_conn: OnceLock::new(),
        cli_source,
        helper_meta: Mutex::new(HashMap::new()),
        hook_owned: Mutex::new(HashSet::new()),
        born_bound: Mutex::new(HashSet::new()),
        host_list_cache: Mutex::new(None),
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

    let client = MasterClient {
        state: Arc::clone(&inner),
    };
    let builder = acp::Client
        .builder()
        .name("wta-master")
        .on_receive_request({ let c = client.clone(); move |req: acp::schema::v1::AgentRequest, responder, _cx| { let c = c.clone(); async move {
            use acp::schema::v1::{AgentRequest as Q, ClientResponse as R};
            match req {
                Q::RequestPermissionRequest(a) => conn::respond_enum(responder, c.request_permission(a).await.map(R::RequestPermissionResponse)),
                Q::WriteTextFileRequest(a) => conn::respond_enum(responder, c.write_text_file(a).await.map(R::WriteTextFileResponse)),
                Q::ReadTextFileRequest(a) => conn::respond_enum(responder, c.read_text_file(a).await.map(R::ReadTextFileResponse)),
                Q::CreateTerminalRequest(a) => conn::respond_enum(responder, c.create_terminal(a).await.map(R::CreateTerminalResponse)),
                Q::TerminalOutputRequest(a) => conn::respond_enum(responder, c.terminal_output(a).await.map(R::TerminalOutputResponse)),
                Q::ReleaseTerminalRequest(a) => conn::respond_enum(responder, c.release_terminal(a).await.map(R::ReleaseTerminalResponse)),
                Q::WaitForTerminalExitRequest(a) => conn::respond_enum(responder, c.wait_for_terminal_exit(a).await.map(R::WaitForTerminalExitResponse)),
                Q::KillTerminalRequest(a) => conn::respond_enum(responder, c.kill_terminal(a).await.map(R::KillTerminalResponse)),
                _ => responder.respond_with_error(acp::Error::method_not_found()),
            }
        } } }, acp::on_receive_request!())
        .on_receive_notification({ let c = client.clone(); move |notif: acp::schema::v1::AgentNotification, _cx| { let c = c.clone(); async move {
            if let acp::schema::v1::AgentNotification::SessionNotification(n) = notif { let _ = c.session_notification(n).await; }
            Ok(())
        } } }, acp::on_receive_notification!());
    let (conn, handle_io) =
        conn::spawn_client(builder, conn::byte_streams(outgoing, incoming));
    let agent_conn = conn;

    // The ACP I/O loop ending (clean or error) means the master can no
    // longer talk to the agent CLI — same liveness problem as a child
    // exit. Signal shutdown through the same channel so the accept
    // loop can return cleanly and SharedWta can rebuild a fresh
    // master on the next AcquirePane.
    let shutdown_tx_io = shutdown_tx.clone();
    tokio::task::spawn_local(async move {
        let reason = match handle_io.await {
            Ok(()) => {
                tracing::error!(
                    target: "master",
                    "agent CLI I/O loop ended cleanly — initiating master shutdown"
                );
                "ACP I/O loop ended cleanly"
            }
            Err(err) => {
                tracing::error!(
                    target: "master",
                    error = %err,
                    "agent CLI I/O loop ended with error — initiating master shutdown"
                );
                "ACP I/O loop ended with error"
            }
        };
        let _ = shutdown_tx_io.try_send(reason);
    });
    // Drop our original sender so the channel closes naturally when
    // both reaper tasks exit. The receiver in the accept loop will
    // still observe sends from `shutdown_tx_{child,io}`.
    drop(shutdown_tx);

    // 3. Initialize the agent CLI once at master startup.
    let init_timeout_secs = if is_npx { 60 } else { 15 };
    let init_started = std::time::Instant::now();
    let init_result = tokio::time::timeout(
        std::time::Duration::from_secs(init_timeout_secs),
        agent_conn.initialize(
            acp::schema::v1::InitializeRequest::new(acp::schema::ProtocolVersion::V1)
                .client_capabilities(acp::schema::v1::ClientCapabilities::new().terminal(true))
                .client_info(
                    acp::schema::v1::Implementation::new("wta-master", env!("CARGO_PKG_VERSION"))
                        .title("Windows Terminal Agent (master)"),
                ),
        ),
    )
    .await;
    crate::telemetry::log_acp_initialize_complete(
        init_started.elapsed().as_secs_f64() * 1000.0,
        matches!(init_result, Ok(Ok(_))),
        "MasterStartup",
        match &init_result {
            Ok(Ok(_)) => "",
            Ok(Err(_)) => "AcpError",
            Err(_) => "Timeout",
        },
        match &init_result {
            Ok(Err(e)) => e.code.into(),
            _ => 0,
        },
    );
    let init_resp = init_result
        .map_err(|_| {
            tracing::error!(
                target: "master",
                timeout_secs = init_timeout_secs,
                "ACP initialize timed out — agent CLI did not respond"
            );
            anyhow!(
                "ACP initialize timed out after {}s — agent CLI did not respond",
                init_timeout_secs
            )
        })?
        .map_err(|e| {
            tracing::error!(target: "master", error = %e, "ACP initialize failed");
            anyhow!("ACP initialize failed: {e}")
        })?;
    tracing::info!(
        target: "master",
        ?init_resp,
        "agent CLI initialize OK"
    );

    // Lock in the cached response BEFORE opening the pipe so the
    // first helper's `initialize` request always sees a populated
    // cache. (Subsequent helpers can race the OnceLock, but `set`
    // is idempotent on already-populated cells — we ignore the
    // returned Err.)
    let _ = inner.cached_init_resp.set(init_resp.clone());
    let _ = inner.agent_conn.set(agent_conn.clone());

    // Seed the registry with historical sessions sourced from ACP
    // `session/list`. Host (the already-running agent) is fast — seed +
    // broadcast it immediately. WSL (per-distro spawn) can be slow / wedged, so
    // it runs decoupled on the LocalSet and broadcasts when it lands. No on-disk
    // CLI parsing. Runs after init so the capability + connection are ready.
    {
        let inner_for_history = std::sync::Arc::clone(&inner);
        tokio::task::spawn_local(async move {
            let scan_started = std::time::Instant::now();
            let count = seed_host_and_broadcast(&inner_for_history).await;
            tracing::info!(
                target: "master_history",
                count,
                elapsed_ms = scan_started.elapsed().as_millis() as u64,
                "master ACP host history seed complete"
            );
            spawn_wsl_seed(&inner_for_history);
        });
    }

    // 4. Open the named pipe and accept helper connections.
    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&pipe_name)
        .with_context(|| format!("failed to create named pipe '{pipe_name}'"))?;
    tracing::info!(
        target: "master",
        pipe_name = %pipe_name,
        "named pipe listening; awaiting helper connections"
    );
    let _pipe_discovery_guard = MasterPipeDiscoveryGuard::write(&pipe_name);

    let mut next_helper_id: u64 = 1;
    // Cheap monotonic counter for tracking concurrent helper count.
    // Both connect and disconnect log it, so a single grep on
    // "live_helpers=" reconstructs the timeline.
    let live_helpers = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    loop {
        // Race the next helper connect against the shutdown channel:
        // when either reaper task posts a reason, we return early so
        // the LocalSet unwinds and drops the Child (kill_on_drop) +
        // WorkerGuard (flush).
        tokio::select! {
            connect_result = server.connect() => {
                connect_result
                    .with_context(|| format!("named pipe connect on '{pipe_name}'"))?;
            }
            shutdown_reason = shutdown_rx.recv() => {
                let reason = shutdown_reason.unwrap_or("shutdown channel closed");
                tracing::error!(
                    target: "master",
                    reason,
                    "master accept loop exiting"
                );
                return Err(anyhow!(
                    "wta-master shutting down: {reason} — SharedWta will respawn a fresh master on the next AcquirePane"
                ));
            }
        }

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
            ServerOptions::new().create(&pipe_name).with_context(|| {
                format!("failed to create follow-up pipe instance for '{pipe_name}'")
            })?,
        );

        let agent_conn = agent_conn.clone();
        let inner = Arc::clone(&inner);
        let live_helpers = Arc::clone(&live_helpers);
        tokio::task::spawn_local(async move {
            let result = serve_helper(helper_id, connected, agent_conn, inner).await;
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

/// Per-helper-connection task. Wraps the named pipe in an
/// `AgentSideConnection`, runs both its I/O loop and a notification
/// forwarder until the helper disconnects.
async fn serve_helper(
    helper_id: HelperId,
    pipe: NamedPipeServer,
    agent_conn: conn::ClientLink,
    state: Arc<MasterStateInner>,
) -> Result<()> {
    tracing::info!(target: "master", helper_id = ?helper_id, "helper connected");

    let (notif_tx, mut notif_rx) =
        mpsc::channel::<acp::schema::v1::SessionNotification>(NOTIF_CHANNEL_CAPACITY);

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
        agent_conn,
        state: Arc::clone(&state),
        notif_tx,
        agent_side_slot: Arc::clone(&agent_side_slot),
    };

    let (read_half, write_half) = tokio::io::split(pipe);
    let outgoing = write_half.compat_write();
    let incoming = read_half.compat();

    let builder = acp::Agent
        .builder()
        .name("wta-master-helper")
        .on_receive_request({ let h = handler.clone(); move |req: acp::schema::v1::ClientRequest, responder, _cx| { let h = h.clone(); async move {
            use acp::schema::v1::{ClientRequest as Q, AgentResponse as R};
            match req {
                Q::InitializeRequest(a) => conn::respond_enum(responder, h.initialize(a).await.map(R::InitializeResponse)),
                Q::AuthenticateRequest(a) => conn::respond_enum(responder, h.authenticate(a).await.map(R::AuthenticateResponse)),
                Q::NewSessionRequest(a) => conn::respond_enum(responder, h.new_session(a).await.map(R::NewSessionResponse)),
                Q::LoadSessionRequest(a) => conn::respond_enum(responder, h.load_session(a).await.map(R::LoadSessionResponse)),
                Q::SetSessionModeRequest(a) => conn::respond_enum(responder, h.set_session_mode(a).await.map(R::SetSessionModeResponse)),
                Q::SetSessionConfigOptionRequest(a) => conn::respond_enum(responder, h.set_session_config_option(a).await.map(R::SetSessionConfigOptionResponse)),
                Q::ListSessionsRequest(a) => conn::respond_enum(responder, h.list_sessions(a).await.map(R::ListSessionsResponse)),
                Q::PromptRequest(a) => h.prompt(a, responder).await,
                Q::ExtMethodRequest(a) => conn::respond_enum(responder, h.ext_method(a).await.map(R::ExtMethodResponse)),
                _ => responder.respond_with_error(acp::Error::method_not_found()),
            }
        } } }, acp::on_receive_request!())
        .on_receive_notification({ let h = handler.clone(); move |notif: acp::schema::v1::ClientNotification, _cx| { let h = h.clone(); async move {
            if let acp::schema::v1::ClientNotification::CancelNotification(n) = notif { let _ = h.cancel(n).await; }
            Ok(())
        } } }, acp::on_receive_notification!());

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
fn emit_restart_agent_pane(tab_id: &str, session_id: Option<&acp::schema::v1::SessionId>) {
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
    session_id: Option<&acp::schema::v1::SessionId>,
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
    let victims: Vec<acp::schema::v1::SessionId> = {
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
async fn host_session_list_raw(state: &MasterStateInner) -> Option<std::sync::Arc<[acp::schema::v1::SessionInfo]>> {
    let Some(init) = state.cached_init_resp.get() else {
        return None;
    };
    if init.agent_capabilities.session_capabilities.list.is_none() {
        return None;
    }
    let Some(conn) = state.agent_conn.get() else {
        return None;
    };

    const TTL: std::time::Duration = std::time::Duration::from_secs(2);
    {
        let cache = state.host_list_cache.lock().await;
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
        conn.list_sessions(acp::schema::v1::ListSessionsRequest::new()),
    )
    .await
    {
        Ok(Ok(resp)) => Some(resp.sessions.into()),
        Ok(Err(e)) => {
            tracing::debug!(target: "master_history", "host session/list error: {e}");
            None
        }
        Err(_) => {
            tracing::warn!(target: "master_history", "host session/list timed out");
            None
        }
    };
    // Single-flight write-back: if a concurrent caller already published a
    // result while we were awaiting `list_sessions`, adopt it instead of
    // clobbering — so a slow failure can't overwrite a fast success (or
    // vice-versa) and poison the 2 s cache with a transient None.
    let mut cache = state.host_list_cache.lock().await;
    if let Some((at, cached)) = cache.as_ref() {
        if *at >= fetch_started {
            return cached.clone();
        }
    }
    *cache = Some((std::time::Instant::now(), outcome.clone()));
    outcome
}

/// Host history from the already-running agent's `session/list`, gated on the
/// `sessionCapabilities.list` capability. `None` when unsupported (Gemini,
/// non-ACP custom) / not connected / failed — distinct from `Some(vec![])`
/// (listed, but empty), which the reconcile needs to authoritatively drop stale
/// rows. No on-disk fallback by design.
async fn host_history_via_acp(
    state: &MasterStateInner,
) -> Option<Vec<crate::agent_sessions::AgentSession>> {
    let sessions = host_session_list_raw(state).await?;
    let cli = state
        .cli_source
        .clone()
        .unwrap_or_else(|| crate::agent_sessions::CliSource::Unknown("custom".into()));
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
        crate::agent_sessions::SessionLocation::Host,
        &cli,
    ))
}

/// Raw host `session/list` as session_id → title, UNFILTERED (includes Class-A
/// agent-pane rows, whose live registry entries still need synthetic-title
/// upgrades). Empty when session/list is unsupported or the agent isn't
/// connected yet.
async fn host_titles_via_acp(
    state: &MasterStateInner,
) -> std::collections::HashMap<String, String> {
    let Some(sessions) = host_session_list_raw(state).await else {
        return std::collections::HashMap::new();
    };
    sessions
        .iter()
        .filter_map(|row| {
            row.title
                .clone()
                .filter(|title| {
                    // Drop the delegate's injected first-message echo. An agent CLI
                    // (e.g. Copilot) can briefly report the baked `?<prompt>` — which
                    // embeds the `## Terminal Context (pane …)` block — as a session's
                    // `session/list` title before it generates its real summary.
                    // Adopting it would leak the injected context (pane GUID included)
                    // and, being non-synthetic, lock the row out of the later upgrade
                    // to the CLI's real name. Skipping it leaves the born-bound row
                    // synthetic so a subsequent poll adopts the real summary instead.
                    !title.is_empty()
                        && !crate::session_registry::title_is_injected_context_echo(title)
                })
                .map(|title| (row.session_id.to_string(), title))
        })
        .collect()
}

/// Sync master's host-history rows to the agent's `session/list` (the single
/// source of truth): add newly-listed sessions and drop terminal Class-B host
/// rows the agent no longer lists (phantoms, CLI-side deletes). No-op when the
/// agent can't list (unsupported / failed / timed out) so a transient error
/// never wipes the view. Returns `(changed, listed_count)`, or `None` when the
/// agent couldn't be listed.
async fn sync_host_history(state: &MasterStateInner) -> Option<(bool, usize)> {
    let rows = host_history_via_acp(state).await?;
    let listed_ids: std::collections::HashSet<String> =
        rows.iter().map(|r| r.key.clone()).collect();

    // Snapshot once; compute existing ids for the add pass and reconcile the
    // terminal Class-B host rows in the same pass.
    let snapshot = state.registry.snapshot().await;
    let existing: std::collections::HashSet<String> =
        snapshot.iter().map(|s| s.session_id.0.to_string()).collect();

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
        if !is_stale_host_history_row(row, &listed_ids) {
            continue;
        }
        let removed = state
            .registry
            .remove_if(&row.session_id, &|cur| {
                is_stale_host_history_row(cur, &listed_ids)
            })
            .await;
        if removed.is_some() {
            tracing::info!(
                target: "master_history",
                key = %row.session_id.0,
                "reconcile: dropped host row no longer in session/list"
            );
            changed = true;
        }
    }

    Some((changed, rows.len()))
}

/// Whether a registry row is a stale host-history row to drop during reconcile:
/// a terminal (Historical / Ended) Class-B **host** row whose id is NOT in the
/// authoritative `session/list` set. Live rows (Working / Idle), agent panes
/// (ACP-driven), and WSL rows are never reconciled away. Pure for unit testing.
fn is_stale_host_history_row(
    row: &crate::session_registry::SessionInfo,
    listed_ids: &std::collections::HashSet<String>,
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
    !listed_ids.contains(row.session_id.0.as_ref())
}

/// Seed + reconcile host history against the agent's `session/list`, broadcasting
/// when anything changed. WSL is seeded separately ([`spawn_wsl_seed`]) so a
/// slow/wedged distro never blocks host rows. Returns the listed host count.
async fn seed_host_and_broadcast(state: &std::sync::Arc<MasterStateInner>) -> usize {
    let Some((changed, count)) = sync_host_history(state).await else {
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

/// Fire-and-forget the WSL history scan on the master's LocalSet so a 40s distro
/// timeout can't stall host rows. Upserts + broadcasts when it lands. No-op when
/// WSL sessions are disabled.
fn spawn_wsl_seed(state: &std::sync::Arc<MasterStateInner>) {
    if !crate::history_loader::wsl_sessions_enabled() {
        return;
    }
    let inner = std::sync::Arc::clone(state);
    tokio::task::spawn_local(async move {
        let started = std::time::Instant::now();
        let wsl = crate::wsl_acp::scan_running_distros_acp(inner.cli_source.as_ref()).await;
        let count = wsl.len();
        for s in &wsl {
            let info = crate::session_registry::agent_session_to_session_info(s);
            inner.registry.upsert_if_absent(info).await;
        }
        tracing::info!(
            target: "master_history",
            count,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "WSL ACP history seed complete"
        );
        if count > 0 {
            broadcast_ext_to_helpers(
                &inner,
                crate::session_registry::build_sessions_changed_notification(),
            )
            .await;
        }
    });
}

/// Before returning the snapshot, opportunistically upgrade any row whose title
/// is still synthetic (empty / cwd-basename) from the agent's raw ACP
/// `session/list` titles.
/// This is what gets a title onto **born-bound** rows — e.g. `?<prompt>`
/// delegate sessions, which register with an empty title before the CLI has
/// generated its real one.
async fn handle_sessions_list(
    state: &std::sync::Arc<MasterStateInner>,
    parsed: &crate::session_registry::SessionsListParams,
) -> acp::Result<acp::schema::v1::ExtResponse> {
    if parsed.rescan {
        // Host is fast: re-pull + broadcast inline. WSL can be slow / wedged
        // (40s distro timeout), so fire it asynchronously — it broadcasts again
        // when it lands rather than blocking this response on it.
        let count = seed_host_and_broadcast(state).await;
        tracing::info!(
            target: "master_history",
            count,
            "sessions/list rescan: reloaded host history via ACP (WSL async)"
        );
        spawn_wsl_seed(state);
    } else {
        // Periodic poll: reconcile host rows against `session/list` (the source
        // of truth) so phantom / CLI-deleted host rows are pruned and newly-listed
        // ones appear. Reuses the 2s-cached fetch. No-op (and no broadcast) when
        // nothing changed or the agent can't list — so a transient error never
        // wipes the view and steady state causes no push storm.
        if let Some((true, _)) = sync_host_history(state).await {
            broadcast_ext_to_helpers(
                state,
                crate::session_registry::build_sessions_changed_notification(),
            )
            .await;
        }
    }

    let mut sessions = state.registry.snapshot().await;
    if sessions.iter().any(crate::session_registry::title_is_synthetic) {
        let titles = host_titles_via_acp(state).await;
        // Re-snapshot only when a title actually changed; the common steady-state
        // (no synthetic rows, or nothing to upgrade) reuses the first snapshot.
        if refresh_synthetic_titles_from(&*state.registry, &titles).await {
            sessions = state.registry.snapshot().await;
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
    //    is installed — without re-binding the pane.
    //  * real hook / ACP agent-pane event: authoritative for binding AND
    //    activity. Record in `hook_owned` (full watcher suppression) and, if the
    //    session was previously born-bound, drop it from `born_bound` — the real
    //    hook now owns it.
    if let Some(key) = &refresh_key {
        let sid = acp::schema::v1::SessionId::new(key.clone());
        if binding_only {
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

    Ok(crate::session_registry::build_session_hook_response(applied))
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
async fn apply_watcher_event(
    state: &MasterStateInner,
    emitted: crate::session_watcher::Emitted,
) {
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

/// Whether `info`'s row can be title-refreshed from the connected agent's
/// `session/list`. The agent enumerates only ITS OWN cli's sessions, so a row
/// stamped with a *different* known cli (e.g. a machine-wide watched claude
/// session while master multiplexes copilot) can never appear in it — skip it
/// rather than issue a per-event round-trip that can't match. Such cross-cli
/// titles are no longer upgraded — an accepted consequence of dropping the
/// per-cli on-disk title reads. A `None` cli on either side is treated as
/// "attempt" (the lookup simply no-ops when the id is absent).
fn row_refreshable_by_connected_agent(
    info: &crate::session_registry::SessionInfo,
    conn_cli: Option<&crate::agent_sessions::CliSource>,
) -> bool {
    match (info.cli_source.as_ref(), conn_cli) {
        (Some(row_cli), Some(conn_cli)) => row_cli == conn_cli,
        _ => true,
    }
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
    if !row_refreshable_by_connected_agent(&info, state.cli_source.as_ref()) {
        return false;
    }
    let titles = host_titles_via_acp(state).await;
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
mod tests {
    use super::*;
    use acp::schema::v1::{ContentChunk, SessionId, SessionNotification, SessionUpdate};
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    #[derive(Clone)]
    struct PendingNewSessionAgent;

    impl PendingNewSessionAgent {
        async fn initialize(
            &self,
            _args: acp::schema::v1::InitializeRequest,
        ) -> acp::Result<acp::schema::v1::InitializeResponse> {
            Ok(acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1))
        }
        async fn authenticate(
            &self,
            _args: acp::schema::v1::AuthenticateRequest,
        ) -> acp::Result<acp::schema::v1::AuthenticateResponse> {
            Ok(acp::schema::v1::AuthenticateResponse::new())
        }
        async fn new_session(
            &self,
            _args: acp::schema::v1::NewSessionRequest,
        ) -> acp::Result<acp::schema::v1::NewSessionResponse> {
            futures::future::pending().await
        }
    }

    fn make_state() -> Arc<MasterStateInner> {
        Arc::new(MasterStateInner {
            session_to_helper: Mutex::new(HashMap::new()),
            registry: crate::session_registry::InMemoryRegistry::shared(),
            helper_ext_subscribers: Mutex::new(HashMap::new()),
            wt: None,
            cached_init_resp: OnceLock::new(),
            agent_conn: OnceLock::new(),
            cli_source: Some(crate::agent_sessions::CliSource::Copilot),
            helper_meta: Mutex::new(HashMap::new()),
            hook_owned: Mutex::new(HashSet::new()),
            born_bound: Mutex::new(HashSet::new()),
            host_list_cache: Mutex::new(None),
        })
    }

    fn client_connection_to_pending_new_session_agent() -> conn::ClientLink {
        let (client_pipe, agent_pipe) = tokio::io::duplex(4096);
        let (client_read, client_write) = tokio::io::split(client_pipe);
        let (agent_read, agent_write) = tokio::io::split(agent_pipe);

        let mock = PendingNewSessionAgent;
        let agent_builder = acp::Agent
            .builder()
            .name("pending-agent")
            .on_receive_request({ let m = mock.clone(); move |req: acp::schema::v1::ClientRequest, responder, _cx| { let m = m.clone(); async move {
                use acp::schema::v1::{ClientRequest as Q, AgentResponse as R};
                match req {
                    Q::InitializeRequest(a) => conn::respond_enum(responder, m.initialize(a).await.map(R::InitializeResponse)),
                    Q::AuthenticateRequest(a) => conn::respond_enum(responder, m.authenticate(a).await.map(R::AuthenticateResponse)),
                    Q::NewSessionRequest(a) => conn::respond_enum(responder, m.new_session(a).await.map(R::NewSessionResponse)),
                    _ => responder.respond_with_error(acp::Error::method_not_found()),
                }
            } } }, acp::on_receive_request!());
        let (_agent_conn, agent_io) =
            conn::spawn_agent(agent_builder, conn::byte_streams(agent_write.compat_write(), agent_read.compat()));
        tokio::task::spawn_local(async move { let _ = agent_io.await; });

        let (client_conn, client_io) = conn::spawn_client(
            acp::Client.builder().name("noop-client"),
            conn::byte_streams(client_write.compat_write(), client_read.compat()),
        );
        tokio::task::spawn_local(async move { let _ = client_io.await; });

        client_conn
    }

    #[tokio::test(flavor = "current_thread")]
    async fn new_session_timeout_is_enforced_by_master_forwarder() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
                let handler = HelperHandler {
                    helper_id: HelperId(1),
                    agent_conn: client_connection_to_pending_new_session_agent(),
                    state: make_state(),
                    notif_tx,
                    agent_side_slot: Arc::new(OnceLock::new()),
                };

                let err = handler
                    .forward_new_session_to_agent(
                        acp::schema::v1::NewSessionRequest::new(PathBuf::from(r"C:\repo")),
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

    /// Regression for the reentrant-permission deadlock: a `prompt` in flight
    /// must NOT block the master's helper-side ACP dispatch loop. If it does, a
    /// `request_permission` the agent issues *mid-turn* deadlocks the shared
    /// agent CLI — the helper answers the permission, but the blocked loop can
    /// never read that answer, so the turn (and every later `session/new`)
    /// hangs. Wire the full two hops the incident exercised:
    ///
    /// ```text
    ///   mock helper --prompt--> master --prompt--> mock agent
    ///        ^                                          |
    ///        +---- request_permission (reentrant) <-----+   (answered "allow")
    /// ```
    ///
    /// With the old inline `agent_conn.prompt(a).await` the prompt never
    /// returns (the timeout below fires); with `prompt_forwarding` the loop
    /// stays free, the permission round-trips, and the turn ends with `EndTurn`.
    #[tokio::test(flavor = "current_thread")]
    async fn prompt_forward_survives_reentrant_permission() {
        use acp::schema::v1::{
            AgentRequest, AgentResponse, ClientRequest, ClientResponse, PermissionOption,
            PermissionOptionId, PermissionOptionKind, PromptRequest, PromptResponse,
            RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
            SelectedPermissionOutcome, StopReason, ToolCallId, ToolCallUpdate,
            ToolCallUpdateFields,
        };

        tokio::task::LocalSet::new()
            .run_until(async {
                let state = make_state();
                let sid = SessionId::new("reentrant-sess");

                // ---- hop 1: master (agent-side client) <-> mock reentrant agent ----
                let (master_agent_pipe, mock_agent_pipe) = tokio::io::duplex(64 * 1024);

                // mock agent: on prompt, ask permission (reentrant, from a spawned
                // task so the mock's own dispatch loop stays free), then EndTurn.
                {
                    let (ar, aw) = tokio::io::split(mock_agent_pipe);
                    let builder = acp::Agent
                        .builder()
                        .name("mock-reentrant-agent")
                        .on_receive_request(
                            move |req: ClientRequest,
                                  responder,
                                  cx: acp::ConnectionTo<acp::Client>| async move {
                                match req {
                                    ClientRequest::PromptRequest(a) => {
                                        let sid = a.session_id.clone();
                                        tokio::task::spawn_local(async move {
                                            let perm = RequestPermissionRequest::new(
                                                sid,
                                                ToolCallUpdate::new(
                                                    ToolCallId::new("tool-1"),
                                                    ToolCallUpdateFields::new()
                                                        .title("Run: echo hi"),
                                                ),
                                                vec![PermissionOption::new(
                                                    PermissionOptionId::new("allow-once"),
                                                    "Allow once",
                                                    PermissionOptionKind::AllowOnce,
                                                )],
                                            );
                                            // block_task from a spawned task is safe.
                                            let _ = cx.send_request(perm).block_task().await;
                                            let _ = conn::respond_enum(
                                                responder,
                                                Ok(AgentResponse::PromptResponse(
                                                    PromptResponse::new(StopReason::EndTurn),
                                                )),
                                            );
                                        });
                                        Ok(())
                                    }
                                    _ => responder
                                        .respond_with_error(acp::Error::method_not_found()),
                                }
                            },
                            acp::on_receive_request!(),
                        );
                    let (_agent_link, agent_io) = conn::spawn_agent(
                        builder,
                        conn::byte_streams(aw.compat_write(), ar.compat()),
                    );
                    tokio::task::spawn_local(async move {
                        let _ = agent_io.await;
                    });
                }

                // master's client side of hop 1: MasterClient routes the agent's
                // reentrant request_permission back out to the owning helper.
                let master_client = MasterClient {
                    state: Arc::clone(&state),
                };
                let agent_conn = {
                    let (cr, cw) = tokio::io::split(master_agent_pipe);
                    let builder = acp::Client
                        .builder()
                        .name("master-agent-side")
                        .on_receive_request(
                            {
                                let c = master_client.clone();
                                move |req: AgentRequest, responder, _cx| {
                                    let c = c.clone();
                                    async move {
                                        match req {
                                            AgentRequest::RequestPermissionRequest(a) => {
                                                conn::respond_enum(
                                                    responder,
                                                    c.request_permission(a).await.map(
                                                        ClientResponse::RequestPermissionResponse,
                                                    ),
                                                )
                                            }
                                            _ => responder.respond_with_error(
                                                acp::Error::method_not_found(),
                                            ),
                                        }
                                    }
                                }
                            },
                            acp::on_receive_request!(),
                        );
                    let (link, io) = conn::spawn_client(
                        builder,
                        conn::byte_streams(cw.compat_write(), cr.compat()),
                    );
                    tokio::task::spawn_local(async move {
                        let _ = io.await;
                    });
                    link
                };

                // ---- hop 2: master (helper-side agent) <-> mock helper client ----
                let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
                let handler = HelperHandler {
                    helper_id: HelperId(1),
                    agent_conn,
                    state: Arc::clone(&state),
                    notif_tx: notif_tx.clone(),
                    agent_side_slot: Arc::new(OnceLock::new()),
                };
                let (mock_helper_pipe, master_helper_pipe) = tokio::io::duplex(64 * 1024);
                let master_to_helper = {
                    let (mr, mw) = tokio::io::split(master_helper_pipe);
                    let builder = acp::Agent
                        .builder()
                        .name("master-helper-side")
                        .on_receive_request(
                            {
                                let h = handler.clone();
                                move |req: ClientRequest, responder, _cx| {
                                    let h = h.clone();
                                    async move {
                                        match req {
                                            ClientRequest::PromptRequest(a) => {
                                                h.prompt(a, responder).await
                                            }
                                            _ => responder.respond_with_error(
                                                acp::Error::method_not_found(),
                                            ),
                                        }
                                    }
                                }
                            },
                            acp::on_receive_request!(),
                        );
                    let (link, io) = conn::spawn_agent(
                        builder,
                        conn::byte_streams(mw.compat_write(), mr.compat()),
                    );
                    tokio::task::spawn_local(async move {
                        let _ = io.await;
                    });
                    link
                };

                // Route the session so the agent's reentrant request_permission
                // reaches the mock helper.
                state.session_to_helper.lock().await.insert(
                    sid.clone(),
                    HelperRoute {
                        helper_id: HelperId(1),
                        notif_tx,
                        forwarder: Some(master_to_helper),
                        consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                    },
                );

                // mock helper: approves any permission with "allow-once".
                let helper_link = {
                    let (hr, hw) = tokio::io::split(mock_helper_pipe);
                    let builder = acp::Client
                        .builder()
                        .name("mock-helper")
                        .on_receive_request(
                            move |req: AgentRequest, responder, _cx| async move {
                                match req {
                                    AgentRequest::RequestPermissionRequest(_a) => {
                                        conn::respond_enum(
                                            responder,
                                            Ok(ClientResponse::RequestPermissionResponse(
                                                RequestPermissionResponse::new(
                                                    RequestPermissionOutcome::Selected(
                                                        SelectedPermissionOutcome::new(
                                                            PermissionOptionId::new("allow-once"),
                                                        ),
                                                    ),
                                                ),
                                            )),
                                        )
                                    }
                                    _ => responder
                                        .respond_with_error(acp::Error::method_not_found()),
                                }
                            },
                            acp::on_receive_request!(),
                        );
                    let (link, io) = conn::spawn_client(
                        builder,
                        conn::byte_streams(hw.compat_write(), hr.compat()),
                    );
                    tokio::task::spawn_local(async move {
                        let _ = io.await;
                    });
                    link
                };

                // The helper's prompt must complete despite the reentrant
                // permission — no deadlock, no timeout.
                let resp = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    helper_link.prompt(PromptRequest::new(sid.clone(), vec!["hi".into()])),
                )
                .await
                .expect("prompt deadlocked: helper dispatch loop blocked during in-flight prompt")
                .expect("prompt should succeed");

                assert!(
                    matches!(resp.stop_reason, StopReason::EndTurn),
                    "expected EndTurn, got {:?}",
                    resp.stop_reason
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
        let (tx1, mut rx1) = mpsc::unbounded_channel::<acp::schema::v1::ExtNotification>();
        let (tx2, mut rx2) = mpsc::unbounded_channel::<acp::schema::v1::ExtNotification>();
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
        let (tx_dead, rx_dead) = mpsc::unbounded_channel::<acp::schema::v1::ExtNotification>();
        let (tx_live, _rx_live) = mpsc::unbounded_channel::<acp::schema::v1::ExtNotification>();
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
        let (ext_tx2, mut ext_rx2) = mpsc::unbounded_channel::<acp::schema::v1::ExtNotification>();
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
        let mut got: Vec<acp::schema::v1::SessionId> = Vec::new();
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
        
        let state = make_state();
        let client = MasterClient {
            state: Arc::clone(&state),
        };
        let req =
            acp::schema::v1::CreateTerminalRequest::new(SessionId::new("nobody-home"), "echo".to_string());
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

        let resp = handle_sessions_list(&state, &session_registry::SessionsListParams { rescan: false })
            .await
            .expect("sessions/list succeeds");
        let parsed = session_registry::parse_sessions_list_response(&resp.0)
            .expect("response parses");

        assert_eq!(parsed.sessions, vec![row]);
    }

    #[tokio::test]
    async fn drop_sessions_for_helper_broadcasts_sessions_changed() {
        use crate::session_registry::{self, SessionInfo};
        use std::path::PathBuf;

        let state = make_state();
        let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
        let (ext_tx, mut ext_rx) = mpsc::unbounded_channel::<acp::schema::v1::ExtNotification>();
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
        let sid = acp::schema::v1::SessionId::new("hist-sid");
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
        let sid = acp::schema::v1::SessionId::new("live-sid");
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
        let sid = acp::schema::v1::SessionId::new("focus-sid");
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
        let sid = acp::schema::v1::SessionId::new("orphan-sid");
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

    fn session_resume_params_for(sid: &acp::schema::v1::SessionId) -> crate::session_registry::SessionResumeDispatchedParams {
        crate::session_registry::SessionResumeDispatchedParams { sid: sid.clone() }
    }

    fn session_focus_params_for(sid: &acp::schema::v1::SessionId) -> crate::session_registry::SessionFocusParams {
        crate::session_registry::SessionFocusParams { sid: sid.clone() }
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
            cached_init_resp: OnceLock::new(),
            agent_conn: OnceLock::new(),
            cli_source: Some(crate::agent_sessions::CliSource::Copilot),
            helper_meta: Mutex::new(HashMap::new()),
            hook_owned: Mutex::new(HashSet::new()),
            born_bound: Mutex::new(HashSet::new()),
            host_list_cache: Mutex::new(None),
        })
    }

    fn focus_params_for(sid: &acp::schema::v1::SessionId) -> crate::session_registry::FocusSessionParams {
        crate::session_registry::FocusSessionParams { session_id: sid.clone() }
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
        let sid = acp::schema::v1::SessionId::new("alive-sess");
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
        let sid = acp::schema::v1::SessionId::new("nobody-here");

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
        let sid = acp::schema::v1::SessionId::new("orphan-sess");
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
        let sid = acp::schema::v1::SessionId::new("alive-but-no-wt");
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
        let sid = acp::schema::v1::SessionId::new("alive-but-pane-gone");
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

    /// Malformed params for a recognized method are rejected as `invalid_params`
    /// by `parse_ext_request` (unit-tested in `session_registry`), so the
    /// handlers below always receive already-decoded, well-typed params.
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
        let response = handle_session_hook(&state, event, false)
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

    // ── refresh_synthetic_titles_from ───────────────────────────────

    #[tokio::test]
    async fn refresh_synthetic_titles_from_upgrades_empty_and_basename_titles_only() {
        use std::collections::HashMap;

        let state = make_state();
        let mut empty = crate::session_registry::SessionInfo::new(
            acp::schema::v1::SessionId::new("sid-empty".to_string()),
            std::path::PathBuf::from("/repo/empty"),
        );
        empty.title = Some(String::new());
        state.registry.upsert(empty).await;

        let mut basename = crate::session_registry::SessionInfo::new(
            acp::schema::v1::SessionId::new("sid-base".to_string()),
            std::path::PathBuf::from("/repo/project"),
        );
        basename.title = Some("project".to_string());
        state.registry.upsert(basename).await;

        let mut real = crate::session_registry::SessionInfo::new(
            acp::schema::v1::SessionId::new("sid-real".to_string()),
            std::path::PathBuf::from("/repo/real"),
        );
        real.title = Some("Existing Real Title".to_string());
        state.registry.upsert(real).await;

        let titles = HashMap::from([
            ("sid-empty".to_string(), "Empty Real Title".to_string()),
            ("sid-base".to_string(), "Basename Real Title".to_string()),
            ("sid-real".to_string(), "Should Not Overwrite".to_string()),
        ]);

        assert!(refresh_synthetic_titles_from(&*state.registry, &titles).await);
        assert_eq!(
            state
                .registry
                .lookup(&acp::schema::v1::SessionId::new("sid-empty".to_string()))
                .await
                .unwrap()
                .title
                .as_deref(),
            Some("Empty Real Title")
        );
        assert_eq!(
            state
                .registry
                .lookup(&acp::schema::v1::SessionId::new("sid-base".to_string()))
                .await
                .unwrap()
                .title
                .as_deref(),
            Some("Basename Real Title")
        );
        assert_eq!(
            state
                .registry
                .lookup(&acp::schema::v1::SessionId::new("sid-real".to_string()))
                .await
                .unwrap()
                .title
                .as_deref(),
            Some("Existing Real Title")
        );
    }

    #[tokio::test]
    async fn refresh_synthetic_titles_from_skips_when_id_absent() {
        let state = make_state();
        let mut row = crate::session_registry::SessionInfo::new(
            acp::schema::v1::SessionId::new("sid-missing".to_string()),
            std::path::PathBuf::from("/repo/project"),
        );
        row.title = Some("project".to_string());
        state.registry.upsert(row).await;

        assert!(
            !refresh_synthetic_titles_from(&*state.registry, &std::collections::HashMap::new())
                .await
        );
        assert_eq!(
            state
                .registry
                .lookup(&acp::schema::v1::SessionId::new("sid-missing".to_string()))
                .await
                .unwrap()
                .title
                .as_deref(),
            Some("project")
        );
    }

    #[test]
    fn row_refreshable_skips_only_definitively_cross_cli() {
        use crate::agent_sessions::CliSource;
        let mut row = crate::session_registry::SessionInfo::new(
            acp::schema::v1::SessionId::new("s".to_string()),
            std::path::PathBuf::from("/x"),
        );
        // Same known cli → refreshable.
        row.cli_source = Some(CliSource::Copilot);
        assert!(row_refreshable_by_connected_agent(&row, Some(&CliSource::Copilot)));
        // Different known cli → skipped (the connected agent can't enumerate it).
        assert!(!row_refreshable_by_connected_agent(&row, Some(&CliSource::Claude)));
        // Unknown cli on either side → attempt (never skip).
        row.cli_source = None;
        assert!(row_refreshable_by_connected_agent(&row, Some(&CliSource::Copilot)));
        row.cli_source = Some(CliSource::Copilot);
        assert!(row_refreshable_by_connected_agent(&row, None));
    }

    #[test]
    fn is_stale_host_history_row_reconcile_rules() {
        use crate::agent_sessions::{AgentStatus, SessionLocation, SessionOrigin};
        use std::collections::HashSet;
        let listed: HashSet<String> = ["kept".to_string()].into_iter().collect();
        let mk = |id: &str| {
            let mut r = crate::session_registry::SessionInfo::new(
                acp::schema::v1::SessionId::new(id.to_string()),
                std::path::PathBuf::from("C:\\Users\\dev"),
            );
            r.status = Some(AgentStatus::Historical);
            r.origin = Some(SessionOrigin::Unknown);
            r
        };
        // Terminal Class-B host row NOT in session/list → stale (drop).
        assert!(is_stale_host_history_row(&mk("gone"), &listed));
        // Still listed → keep.
        assert!(!is_stale_host_history_row(&mk("kept"), &listed));
        // Live (Idle/Working) → keep even if not listed.
        let mut live = mk("gone");
        live.status = Some(AgentStatus::Idle);
        assert!(!is_stale_host_history_row(&live, &listed));
        // Agent pane → never reconciled.
        let mut pane = mk("gone");
        pane.origin = Some(SessionOrigin::AgentPane);
        assert!(!is_stale_host_history_row(&pane, &listed));
        // WSL row → host can't authoritatively list distro sessions.
        let mut wsl = mk("gone");
        wsl.location = SessionLocation::Wsl { distro: "Ubuntu".to_string() };
        assert!(!is_stale_host_history_row(&wsl, &listed));
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
    async fn seed_session_row(
        state: &MasterStateInner,
        key: &str,
        origin: crate::agent_sessions::SessionOrigin,
        status: crate::agent_sessions::AgentStatus,
    ) {
        let mut info = crate::session_registry::SessionInfo::new(
            acp::schema::v1::SessionId::new(key.to_string()),
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
            event: crate::agent_sessions::SessionEvent::ToolStarting {
                key: key.to_string(),
                tool_name: String::new(),
            },
        }
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
            .insert(acp::schema::v1::SessionId::new("sid-hooked".to_string()));

        apply_watcher_event(&state, codex_emitted("sid-hooked")).await;

        assert!(
            state
                .registry
                .lookup(&acp::schema::v1::SessionId::new("sid-hooked".to_string()))
                .await
                .is_none(),
            "watcher must not create a row for a hook-owned session"
        );
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
            .lookup(&acp::schema::v1::SessionId::new("sid-agent-pane".to_string()))
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
        handle_session_hook(&state, event, false)
            .await
            .expect("valid session_hook accepted");

        assert!(
            state
                .hook_owned
                .lock()
                .await
                .contains(&acp::schema::v1::SessionId::new("sid-claimed".to_string())),
            "a keyed session_hook event must mark the session hook-owned"
        );

        // A subsequent watcher event must not disturb the hook-bound row.
        apply_watcher_event(&state, codex_emitted("sid-claimed")).await;
        let row = state
            .registry
            .lookup(&acp::schema::v1::SessionId::new("sid-claimed".to_string()))
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
        handle_session_hook(&state, event, true)
            .await
            .expect("valid born-bound accepted");

        let sid = acp::schema::v1::SessionId::new("bb-mark".to_string());
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
        let sid = acp::schema::v1::SessionId::new("bb-activity".to_string());

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
        let sid = acp::schema::v1::SessionId::new("bb-takeover".to_string());

        let bb = crate::agent_sessions::SessionEvent::SessionStarted {
            key: "bb-takeover".to_string(),
            cli_source: crate::agent_sessions::CliSource::Claude,
            pane_session_id: "pane-bb".to_string(),
            cwd: std::path::PathBuf::from("C:\\repo"),
            title: String::new(),
        };
        handle_session_hook(&state, bb, true)
            .await
            .expect("born-bound accepted");
        assert!(state.born_bound.lock().await.contains(&sid));

        // A real hook event arrives via session_hook (is_born_bound = false).
        let hook = crate::agent_sessions::SessionEvent::ToolStarting {
            key: "bb-takeover".to_string(),
            tool_name: "Bash".to_string(),
        };
        handle_session_hook(&state, hook, false)
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
        let sid = acp::schema::v1::SessionId::new("sid-resume".to_string());

        let dispatched = crate::agent_sessions::SessionEvent::ResumeDispatched {
            key: "sid-resume".to_string(),
        };
        handle_session_hook(&state, dispatched, false)
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
        handle_session_hook(&state, assigned, false)
            .await
            .expect("resume pane assigned accepted");
        assert!(
            state.born_bound.lock().await.contains(&sid),
            "ResumePaneAssigned must be born_bound"
        );
        assert!(!state.hook_owned.lock().await.contains(&sid));
    }

}
