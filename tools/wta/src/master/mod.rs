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
// Phase 1 (this commit): wires the full forward path (helper →
// master → agent CLI) and the session_notification reverse path
// (agent CLI → master → helper). Client-trait methods that need
// per-session helper routing (request_permission, terminal/*,
// fs/*) return `method_not_found` for now; Phase 2 wires them via
// a `session_to_helper`-style table keyed on AgentRequest.session_id.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

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

/// State shared between the master's `acp::Client` impl (receives
/// notifications from the agent CLI) and each helper's `acp::Agent`
/// impl (receives requests from one helper).
struct MasterStateInner {
    /// Routes inbound `session_notification`s from the agent CLI
    /// back to the helper that owns the session. Inserted by the
    /// helper's `new_session` / `load_session` handlers atomically
    /// (before responding to the helper), so no race window.
    ///
    /// Value is `(HelperId, sender)` so that on helper disconnect we
    /// can `retain` out every session belonging to that helper
    /// without keeping a separate index. Without that cleanup the
    /// map would grow unboundedly across the master's lifetime —
    /// each closed pane leaves a dead `SessionId` behind, and every
    /// future notification for it lights up a "helper notification
    /// channel closed" warning.
    session_to_helper: Mutex<
        HashMap<acp::SessionId, (HelperId, mpsc::UnboundedSender<acp::SessionNotification>)>,
    >,
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
/// `session_notification` fans out to the owning helper. The other
/// Client-trait methods that come from the agent CLI (permission
/// requests, terminal/* calls, fs/* calls) target a specific session
/// — Phase 2 will route them via `session_to_helper` too. For now
/// we let them fall through to the trait's default `method_not_found`
/// (the agent CLI's behaviour is "advertise capability=false, never
/// call these").
struct MasterClient {
    state: Arc<MasterStateInner>,
}

#[async_trait::async_trait(?Send)]
impl acp::Client for MasterClient {
    async fn request_permission(
        &self,
        _args: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        // TODO Phase 2: route to the helper owning args.session_id
        Err(acp::Error::method_not_found())
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
        let tx = {
            let map = self.state.session_to_helper.lock().await;
            map.get(&sid).map(|(_hid, tx)| tx.clone())
        };
        match tx {
            Some(tx) => {
                let send_ok = tx.send(args).is_ok();
                tracing::debug!(
                    target: "master",
                    step = "agent→helper",
                    op = "session_notification",
                    session_id = ?sid,
                    kind = %kind,
                    delivered = send_ok,
                    "routed agent CLI notification to helper"
                );
                if !send_ok {
                    // Helper went away between our lookup and our
                    // send. Drop the routing entry so subsequent
                    // notifications don't repeat the same warning
                    // (and the map doesn't grow forever). The
                    // `serve_helper` cleanup path also retains-out
                    // these entries on graceful disconnect; this
                    // path catches the race where send fails before
                    // that runs.
                    let mut map = self.state.session_to_helper.lock().await;
                    map.remove(&sid);
                    tracing::warn!(
                        target: "master",
                        session_id = ?sid,
                        kind = %kind,
                        "helper notification channel closed — helper likely disconnected; dropping update and routing entry"
                    );
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
    notif_tx: mpsc::UnboundedSender<acp::SessionNotification>,
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
        // Record routing entry BEFORE returning so the helper can't
        // race a session/update notification.
        let registry_size = {
            let mut map = self.state.session_to_helper.lock().await;
            map.insert(
                resp.session_id.clone(),
                (self.helper_id, self.notif_tx.clone()),
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
        {
            let mut map = self.state.session_to_helper.lock().await;
            map.insert(session_id.clone(), (self.helper_id, self.notif_tx.clone()));
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

    // Reap the child so it doesn't zombie if it dies.
    let mut child = spawn_result.child;
    tokio::task::spawn_local(async move {
        match child.wait().await {
            Ok(status) => tracing::error!(target: "master", "agent CLI exited: {status:?}"),
            Err(err) => tracing::error!(target: "master", "agent CLI wait failed: {err}"),
        }
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

    tokio::task::spawn_local(async move {
        match handle_io.await {
            Ok(()) => tracing::info!(target: "master", "agent CLI I/O loop ended cleanly"),
            Err(err) => {
                tracing::error!(target: "master", error = %err, "agent CLI I/O loop ended with error")
            }
        }
    });

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
        mpsc::unbounded_channel::<acp::SessionNotification>();

    let handler = HelperHandler {
        helper_id,
        agent_conn,
        state: Arc::clone(&state),
        notif_tx,
    };

    let (read_half, write_half) = tokio::io::split(pipe);
    let outgoing = write_half.compat_write();
    let incoming = read_half.compat();

    let (agent_side_conn, handle_io) =
        acp::AgentSideConnection::new(handler, outgoing, incoming, |fut| {
            tokio::task::spawn_local(fut);
        });

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
    map.retain(|_, (hid, _)| *hid != helper_id);
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
        let (tx1, mut rx1) = mpsc::unbounded_channel();
        let (tx2, mut rx2) = mpsc::unbounded_channel();
        let sid1 = SessionId::new("sess-1");
        let sid2 = SessionId::new("sess-2");

        {
            let mut map = state.session_to_helper.lock().await;
            map.insert(sid1.clone(), (HelperId(1), tx1));
            map.insert(sid2.clone(), (HelperId(2), tx2));
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
        let (tx, rx) = mpsc::unbounded_channel::<SessionNotification>();
        let sid = SessionId::new("dead-session");
        {
            let mut map = state.session_to_helper.lock().await;
            map.insert(sid.clone(), (HelperId(7), tx));
        }
        drop(rx); // simulate helper going away

        route(&state, make_notif(&sid)).await;

        let map = state.session_to_helper.lock().await;
        assert!(
            !map.contains_key(&sid),
            "send failure should have removed the routing entry"
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
        let (tx_a, _rx_a) = mpsc::unbounded_channel();
        let (tx_b, _rx_b) = mpsc::unbounded_channel();
        let (tx_c, _rx_c) = mpsc::unbounded_channel();
        {
            let mut map = state.session_to_helper.lock().await;
            map.insert(SessionId::new("a1"), (HelperId(1), tx_a.clone()));
            map.insert(SessionId::new("a2"), (HelperId(1), tx_a));
            map.insert(SessionId::new("b1"), (HelperId(2), tx_b));
            map.insert(SessionId::new("c1"), (HelperId(3), tx_c));
        }

        let dropped = drop_sessions_for_helper(&state, HelperId(1)).await;
        assert_eq!(dropped, 2);

        let map = state.session_to_helper.lock().await;
        assert!(!map.contains_key(&SessionId::new("a1")));
        assert!(!map.contains_key(&SessionId::new("a2")));
        assert!(map.contains_key(&SessionId::new("b1")));
        assert!(map.contains_key(&SessionId::new("c1")));
    }
}
