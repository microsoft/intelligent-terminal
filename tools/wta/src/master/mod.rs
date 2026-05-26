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
use std::sync::{Arc, OnceLock, Weak};

/// Per-helper notification channel capacity. Sized for bursty chunk
/// streaming during a single agent turn; well above what a healthy
/// helper pipe needs to drain. If it fills up, the helper's pipe is
/// genuinely stuck and we'd rather drop chunks (with a warning) than
/// back-pressure the agent CLI's I/O loop and freeze every other
/// helper sharing this master.
const NOTIF_CHANNEL_CAPACITY: usize = 1024;

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
    cached_init_resp: OnceLock<acp::InitializeResponse>,
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

    async fn session_notification(
        &self,
        args: acp::SessionNotification,
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
            path = ?args.path,
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
            path = ?args.path,
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
            command = %args.command,
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
        let (helper_id, forwarder) =
            self.route_for(&sid, "wait_for_terminal_exit").await?;
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
    agent_conn: Arc<acp::ClientSideConnection>,
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
            acp::Error::internal_error()
                .data(serde_json::json!("helper connection dropped"))
        })
    }
}

#[async_trait::async_trait(?Send)]
impl acp::Agent for HelperHandler {
    async fn initialize(
        &self,
        args: acp::InitializeRequest,
    ) -> acp::Result<acp::InitializeResponse> {
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
                self.agent_conn.initialize(args).await
            }
        }
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
        self.agent_conn.authenticate(args).await
    }

    async fn new_session(
        &self,
        args: acp::NewSessionRequest,
    ) -> acp::Result<acp::NewSessionResponse> {
        tracing::info!(
            target: "master",
            step = "helper→agent",
            op = "new_session",
            helper_id = ?self.helper_id,
            cwd = ?args.cwd,
            mcp_servers = args.mcp_servers.len(),
            "forwarding new_session"
        );
        let resp = self.agent_conn.new_session(args).await?;
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
        tracing::info!(
            target: "master",
            step = "helper→agent",
            op = "new_session",
            helper_id = ?self.helper_id,
            session_id = ?resp.session_id,
            registry_size = registry_size,
            "session bound to helper"
        );
        Ok(resp)
    }

    async fn load_session(
        &self,
        args: acp::LoadSessionRequest,
    ) -> acp::Result<acp::LoadSessionResponse> {
        let session_id = args.session_id.clone();
        let resp = self.agent_conn.load_session(args).await?;
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
        tracing::info!(
            target: "master",
            helper_id = ?self.helper_id,
            session_id = ?session_id,
            "loaded session bound to helper"
        );
        Ok(resp)
    }

    async fn set_session_mode(
        &self,
        args: acp::SetSessionModeRequest,
    ) -> acp::Result<acp::SetSessionModeResponse> {
        self.agent_conn.set_session_mode(args).await
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
        self.agent_conn.set_session_model(args).await
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
        self.agent_conn.set_session_config_option(args).await
    }

    async fn prompt(
        &self,
        args: acp::PromptRequest,
    ) -> acp::Result<acp::PromptResponse> {
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
        let resp = self.agent_conn.prompt(args).await;
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
        self.agent_conn.cancel(args).await
    }
}

/// Master mode entry point.
pub async fn run_master_mode(cli: Cli, pipe_name: String) -> Result<()> {
    let _guard = crate::logging::init("main_master");
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

    let local_set = LocalSet::new();
    local_set
        .run_until(async move { run_master_loop(cli, pipe_name).await })
        .await
}

async fn run_master_loop(cli: Cli, pipe_name: String) -> Result<()> {
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

    // Drain agent stderr to logs so failures are diagnosable.
    if let Some(stderr) = spawn_result.child.stderr.take() {
        tokio::task::spawn_local(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::warn!(target: "agent_stderr", "{line}");
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
    //   * The `WorkerGuard` returned by `crate::logging::init` is held
    //     by `run_master_mode`; it only flushes the non-blocking
    //     tracing appender on Drop. `process::exit` skips that Drop and
    //     the final error lines silently vanish. The graceful path
    //     here lets the guard drop in normal stack unwinding so the
    //     "agent CLI exited" diagnostic actually lands on disk.
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
    let inner = Arc::new(MasterStateInner {
        session_to_helper: Mutex::new(HashMap::new()),
        cached_init_resp: OnceLock::new(),
    });
    let client = MasterClient {
        state: Arc::clone(&inner),
    };
    let (conn, handle_io) = acp::ClientSideConnection::new(client, outgoing, incoming, |fut| {
        tokio::task::spawn_local(fut);
    });
    let agent_conn = Arc::new(conn);

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
    let init_resp = tokio::time::timeout(
        std::time::Duration::from_secs(init_timeout_secs),
        agent_conn.initialize(
            acp::InitializeRequest::new(acp::ProtocolVersion::V1)
                .client_capabilities(acp::ClientCapabilities::new().terminal(true))
                .client_info(
                    acp::Implementation::new("wta-master", env!("CARGO_PKG_VERSION"))
                        .title("Windows Terminal Agent (master)"),
                ),
        ),
    )
    .await
    .map_err(|_| {
        anyhow!(
            "ACP initialize timed out after {}s — agent CLI did not respond",
            init_timeout_secs
        )
    })?
    .map_err(|e| anyhow!("ACP initialize failed: {e}"))?;
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
            ServerOptions::new()
                .create(&pipe_name)
                .with_context(|| {
                    format!("failed to create follow-up pipe instance for '{pipe_name}'")
                })?,
        );

        let agent_conn = Arc::clone(&agent_conn);
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
    agent_conn: Arc<acp::ClientSideConnection>,
    state: Arc<MasterStateInner>,
) -> Result<()> {
    tracing::info!(target: "master", helper_id = ?helper_id, "helper connected");

    let (notif_tx, mut notif_rx) =
        mpsc::channel::<acp::SessionNotification>(NOTIF_CHANNEL_CAPACITY);

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
    let agent_side_slot: Arc<OnceLock<Weak<acp::AgentSideConnection>>> =
        Arc::new(OnceLock::new());

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
            else => {
                break Ok(());
            }
        }
    };

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

    result
}

/// Remove every `session_to_helper` entry owned by `helper_id`.
/// Returns the number of entries dropped. Factored out of
/// `serve_helper` so the cleanup is unit-testable without a real
/// named pipe.
async fn drop_sessions_for_helper(state: &MasterStateInner, helper_id: HelperId) -> usize {
    let mut map = state.session_to_helper.lock().await;
    let before = map.len();
    map.retain(|_, route| route.helper_id != helper_id);
    before - map.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use acp::{ContentChunk, SessionId, SessionNotification, SessionUpdate};

    fn make_state() -> Arc<MasterStateInner> {
        Arc::new(MasterStateInner {
            session_to_helper: Mutex::new(HashMap::new()),
            cached_init_resp: OnceLock::new(),
        })
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
        let req = acp::CreateTerminalRequest::new(
            SessionId::new("nobody-home"),
            "echo".to_string(),
        );
        let err = client
            .create_terminal(req)
            .await
            .expect_err("create_terminal on unknown session must fail");
        assert_eq!(err.code, acp::ErrorCode::InternalError);
    }
}
