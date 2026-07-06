//! ACP 1.0 connection compat shim.
//!
//! `agent-client-protocol` 1.0 replaced the `ClientSideConnection` /
//! `AgentSideConnection` objects (each exposing `conn.method(req).await` plus a
//! separate `handle_io` future) with a builder + dispatch model where the only
//! handle is a `ConnectionTo<Counterpart>` delivered *inside* `connect_with`'s
//! `main_fn`. The N:1 master/helper multiplexer is bespoke and wants the old
//! "stash a connection, drive I/O on the side, call typed methods later" shape,
//! so this module re-exposes exactly that:
//!
//! * [`ClientLink`] wraps `ConnectionTo<Agent>` — the agent-CLI / master client
//!   side. Methods mirror the old `ClientSideConnection` (`initialize`,
//!   `new_session`, `prompt`, …).
//! * [`AgentLink`] wraps `ConnectionTo<Client>` — the master's per-helper agent
//!   side. Methods mirror the old `AgentSideConnection` server→client requests
//!   (`request_permission`, `create_terminal`, …) plus the two outbound
//!   notifications.
//! * [`spawn_client`] / [`spawn_agent`] run a pre-wired builder, hand back the
//!   link, and return a `handle_io` future that resolves when the connection
//!   ends (clean EOF → `Ok`, transport error → `Err`) — same liveness contract
//!   the multiplexer relied on.
//!
//! The legacy `session/set_model` method was dropped from schema 1.1; it is
//! reintroduced here as a local typed request so Copilot/Gemini model switching
//! keeps working.

use std::future::Future;

use agent_client_protocol as acp;
use acp::schema::v1::{
    self, AuthenticateRequest, AuthenticateResponse, CancelNotification, CreateTerminalRequest,
    CreateTerminalResponse, ExtNotification, ExtRequest, ExtResponse, InitializeRequest,
    InitializeResponse,
    KillTerminalRequest, KillTerminalResponse, ListSessionsRequest, ListSessionsResponse,
    LoadSessionRequest, LoadSessionResponse, NewSessionRequest, NewSessionResponse,
    PromptRequest, PromptResponse, ReadTextFileRequest, ReadTextFileResponse,
    ReleaseTerminalRequest, ReleaseTerminalResponse, RequestPermissionRequest,
    RequestPermissionResponse, SessionId, SessionNotification, SetSessionConfigOptionRequest,
    SetSessionConfigOptionResponse, SetSessionModeRequest, SetSessionModeResponse,
    TerminalOutputRequest, TerminalOutputResponse, WaitForTerminalExitRequest,
    WaitForTerminalExitResponse, WriteTextFileRequest, WriteTextFileResponse,
};
use serde::{Deserialize, Serialize};

/// Legacy `session/set_model` request, removed from schema 1.1 but still spoken
/// by Copilot/Gemini. Re-declared locally as a typed JSON-RPC request so the
/// model-switch path keeps the exact wire shape (`sessionId` / `modelId`).
#[derive(Debug, Clone, Serialize, Deserialize, acp::JsonRpcRequest)]
#[request(method = "session/set_model", response = SetSessionModelResponse)]
#[serde(rename_all = "camelCase")]
pub struct SetSessionModelRequest {
    pub session_id: SessionId,
    pub model_id: String,
}

impl SetSessionModelRequest {
    pub fn new(session_id: impl Into<SessionId>, model_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            model_id: model_id.into(),
        }
    }
}

/// Empty response for [`SetSessionModelRequest`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, acp::JsonRpcResponse)]
pub struct SetSessionModelResponse {}

/// Shared readiness cell: `connect_with` fills `slot` then notifies; if the
/// connection task ends before filling it (handshake/transport failure), `failed`
/// is set + notified so waiters surface an error instead of spinning forever.
#[derive(Debug)]
struct Ready<T> {
    slot: std::sync::OnceLock<T>,
    failed: std::sync::atomic::AtomicBool,
    notify: tokio::sync::Notify,
}

impl<T> Default for Ready<T> {
    fn default() -> Self {
        Self {
            slot: std::sync::OnceLock::new(),
            failed: std::sync::atomic::AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
        }
    }
}

async fn await_ready<T: Clone>(ready: &Ready<T>) -> acp::Result<T> {
    loop {
        // Register interest before checking so a fill/fail between the check and
        // the await can't be missed (Notify drops un-awaited permits otherwise).
        let notified = ready.notify.notified();
        if let Some(v) = ready.slot.get() {
            return Ok(v.clone());
        }
        if ready.failed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(acp::Error::internal_error().data("ACP connection setup failed before ready"));
        }
        notified.await;
    }
}

/// Client-side connection handle (talks to an agent CLI or to master). The
/// connection is delivered asynchronously from `connect_with`'s main closure, so
/// it lives behind a shared cell that `spawn_client` fills before the handshake.
#[derive(Clone, Debug)]
pub struct ClientLink {
    cell: std::sync::Arc<Ready<acp::ConnectionTo<acp::Agent>>>,
}

impl ClientLink {
    async fn cx(&self) -> acp::Result<acp::ConnectionTo<acp::Agent>> {
        await_ready(&self.cell).await
    }

    pub async fn initialize(&self, req: InitializeRequest) -> acp::Result<InitializeResponse> {
        self.cx().await?.send_request(req).block_task().await
    }

    pub async fn authenticate(&self, req: AuthenticateRequest) -> acp::Result<AuthenticateResponse> {
        self.cx().await?.send_request(req).block_task().await
    }

    pub async fn new_session(&self, req: NewSessionRequest) -> acp::Result<NewSessionResponse> {
        self.cx().await?.send_request(req).block_task().await
    }

    pub async fn load_session(&self, req: LoadSessionRequest) -> acp::Result<LoadSessionResponse> {
        self.cx().await?.send_request(req).block_task().await
    }

    pub async fn prompt(&self, req: PromptRequest) -> acp::Result<PromptResponse> {
        self.cx().await?.send_request(req).block_task().await
    }

    /// Non-blocking counterpart of [`ClientLink::prompt`], for use **from
    /// inside an ACP `on_receive_request` dispatch handler**.
    ///
    /// Every other method here `block_task().await`s the agent round-trip. That
    /// is correct from a spawned task, but **deadlocks when called from a
    /// dispatch handler**: awaiting freezes that connection's single dispatch
    /// loop, so a reentrant `request_permission` / `create_terminal` the agent
    /// issues *during* the prompt can never have its response read — the exact
    /// hazard the ACP `ordering` docs call out. Instead of awaiting, this
    /// registers `on_response` (run by the SDK when the agent finally replies)
    /// and returns as soon as the request is on the wire, keeping the loop free.
    pub async fn prompt_forwarding<Fut>(
        &self,
        req: PromptRequest,
        on_response: impl FnOnce(acp::Result<PromptResponse>) -> Fut + 'static + Send,
    ) -> acp::Result<()>
    where
        Fut: Future<Output = acp::Result<()>> + 'static + Send,
    {
        self.cx()
            .await?
            .send_request(req)
            .on_receiving_result(on_response)
    }

    pub async fn cancel(&self, notif: CancelNotification) -> acp::Result<()> {
        self.cx().await?.send_notification(notif)
    }

    pub async fn set_session_mode(
        &self,
        req: SetSessionModeRequest,
    ) -> acp::Result<SetSessionModeResponse> {
        self.cx().await?.send_request(req).block_task().await
    }

    pub async fn set_session_model(
        &self,
        req: SetSessionModelRequest,
    ) -> acp::Result<SetSessionModelResponse> {
        self.cx().await?.send_request(req).block_task().await
    }

    pub async fn set_session_config_option(
        &self,
        req: SetSessionConfigOptionRequest,
    ) -> acp::Result<SetSessionConfigOptionResponse> {
        self.cx().await?.send_request(req).block_task().await
    }

    pub async fn list_sessions(&self, req: ListSessionsRequest) -> acp::Result<ListSessionsResponse> {
        self.cx().await?.send_request(req).block_task().await
    }

    pub async fn ext_method(&self, req: ExtRequest) -> acp::Result<ExtResponse> {
        let value = self.cx().await?.send_request(v1::ClientRequest::ExtMethodRequest(req))
            .block_task()
            .await?;
        serde_json::from_value(value)
            .map_err(|e| acp::Error::internal_error().data(format!("ext response decode: {e}")))
    }
}

/// Agent-side connection handle (master → one helper). Forwards server→client
/// requests and the two outbound notifications.
#[derive(Clone, Debug)]
pub struct AgentLink {
    cell: std::sync::Arc<Ready<acp::ConnectionTo<acp::Client>>>,
}

impl AgentLink {
    async fn cx(&self) -> acp::Result<acp::ConnectionTo<acp::Client>> {
        await_ready(&self.cell).await
    }

    pub async fn request_permission(
        &self,
        req: RequestPermissionRequest,
    ) -> acp::Result<RequestPermissionResponse> {
        self.cx().await?.send_request(req).block_task().await
    }

    pub async fn write_text_file(
        &self,
        req: WriteTextFileRequest,
    ) -> acp::Result<WriteTextFileResponse> {
        self.cx().await?.send_request(req).block_task().await
    }

    pub async fn read_text_file(
        &self,
        req: ReadTextFileRequest,
    ) -> acp::Result<ReadTextFileResponse> {
        self.cx().await?.send_request(req).block_task().await
    }

    pub async fn create_terminal(
        &self,
        req: CreateTerminalRequest,
    ) -> acp::Result<CreateTerminalResponse> {
        self.cx().await?.send_request(req).block_task().await
    }

    pub async fn terminal_output(
        &self,
        req: TerminalOutputRequest,
    ) -> acp::Result<TerminalOutputResponse> {
        self.cx().await?.send_request(req).block_task().await
    }

    pub async fn release_terminal(
        &self,
        req: ReleaseTerminalRequest,
    ) -> acp::Result<ReleaseTerminalResponse> {
        self.cx().await?.send_request(req).block_task().await
    }

    pub async fn wait_for_terminal_exit(
        &self,
        req: WaitForTerminalExitRequest,
    ) -> acp::Result<WaitForTerminalExitResponse> {
        self.cx().await?.send_request(req).block_task().await
    }

    pub async fn kill_terminal(&self, req: KillTerminalRequest) -> acp::Result<KillTerminalResponse> {
        self.cx().await?.send_request(req).block_task().await
    }

    pub async fn session_notification(&self, notif: SessionNotification) -> acp::Result<()> {
        self.cx().await?.send_notification(notif)
    }

    pub async fn ext_notification(&self, notif: ExtNotification) -> acp::Result<()> {
        self.cx()
            .await?
            .send_notification(v1::AgentNotification::ExtNotification(notif))
    }
}

/// One-shot "the transport peer died" latch. Set + notified the first time the
/// incoming byte stream hits EOF or a read error — i.e. when wta-master (helper
/// side) or the agent CLI (master side) goes away.
///
/// This exists because ACP 1.0's `connect_with` only surfaces peer death by
/// *returning* — and it returns early only when the internal background I/O
/// **errors**. A *clean* EOF (exactly what `taskkill`-ing wta-master produces)
/// does not error, so with a `pending` `main_fn` `connect_with` would hang
/// forever and `handle_io` would never resolve, leaving the pane stuck on
/// `Connected`. We instead watch the reader directly and complete `main_fn` on
/// death so `connect_with` returns and `handle_io` fires.
#[derive(Debug, Default)]
struct TransportDeath {
    dead: std::sync::atomic::AtomicBool,
    notify: tokio::sync::Notify,
}

impl TransportDeath {
    fn signal(&self) {
        self.dead.store(true, std::sync::atomic::Ordering::Release);
        self.notify.notify_waiters();
    }

    async fn wait(&self) {
        loop {
            // Register interest before checking so a signal racing between the
            // check and the await can't be missed (Notify keeps no permit).
            let notified = self.notify.notified();
            if self.dead.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}

/// `AsyncRead` adapter that fires a [`TransportDeath`] the first time the wrapped
/// reader reports EOF (`Ok(0)` for a non-empty buffer) or an error.
struct DeathWatchRead<I> {
    inner: I,
    death: std::sync::Arc<TransportDeath>,
}

impl<I> futures::AsyncRead for DeathWatchRead<I>
where
    I: futures::AsyncRead + Unpin,
{
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let this = &mut *self;
        let poll = std::pin::Pin::new(&mut this.inner).poll_read(cx, buf);
        if let std::task::Poll::Ready(result) = &poll {
            match result {
                // A real EOF (peer closed the pipe): a 0-byte read into a
                // non-empty buffer. Guard on `!buf.is_empty()` so a benign empty
                // read isn't mistaken for death.
                Ok(0) if !buf.is_empty() => this.death.signal(),
                Err(_) => this.death.signal(),
                _ => {}
            }
        }
        poll
    }
}

/// A byte-stream transport plus the [`TransportDeath`] latch that fires when its
/// incoming half dies. Produced by [`byte_streams`], consumed by
/// [`spawn_client`] / [`spawn_agent`].
pub struct WatchedTransport<O, I> {
    inner: acp::ByteStreams<O, DeathWatchRead<I>>,
    death: std::sync::Arc<TransportDeath>,
}

/// Drive a pre-wired client builder over `transport`, returning a [`ClientLink`]
/// for sending requests plus a `handle_io` future. The future resolves when the
/// connection ends: peer death (EOF/error) → `Ok(())`, transport error surfaced
/// by the SDK → `Err`.
///
/// **Must be called inside a `tokio::task::LocalSet`** — it drives the connection
/// I/O via [`tokio::task::spawn_local`] and will panic on a runtime without one
/// (the WTA helper/master/probe/CLI entry points all establish a `LocalSet`).
pub fn spawn_client<H, Run, O, I>(
    builder: acp::Builder<acp::Client, H, Run>,
    transport: WatchedTransport<O, I>,
) -> (ClientLink, impl Future<Output = acp::Result<()>>)
where
    H: acp::HandleDispatchFrom<acp::Agent> + 'static,
    Run: acp::RunWithConnectionTo<acp::Agent> + 'static,
    O: futures::AsyncWrite + Send + Unpin + 'static,
    I: futures::AsyncRead + Send + Unpin + 'static,
{
    let WatchedTransport { inner, death } = transport;
    let cell = std::sync::Arc::new(Ready::default());
    let fill = cell.clone();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    tokio::task::spawn_local(async move {
        let result = builder
            .connect_with(inner, async move |cx| {
                let _ = fill.slot.set(cx.clone());
                fill.notify.notify_waiters();
                // Stay alive until the transport peer dies. On EOF/read-error the
                // `DeathWatchRead` fires `death`, this `main_fn` completes, and
                // `connect_with` returns (via `run_until`'s foreground branch) so
                // `handle_io` can resolve. A bare `pending` here would hang
                // `connect_with` forever on a clean EOF.
                death.wait().await;
                Ok(())
            })
            .await;
        let _ = done_tx.send(result);
    });
    let handle_io = {
        let cell = cell.clone();
        async move {
            // The connection task always reports its result on `done_tx`. A
            // receive error means it was dropped/panicked before reporting — a
            // real failure, so surface it as an error rather than masking it as a
            // clean `Ok(())` shutdown.
            let r = done_rx.await.unwrap_or_else(|_| {
                Err(acp::Error::internal_error()
                    .data("ACP connection task ended without reporting a result"))
            });
            // Connection ended; if it never became ready, wake waiters so they
            // surface an error instead of spinning/blocking forever.
            cell.failed.store(true, std::sync::atomic::Ordering::Release);
            cell.notify.notify_waiters();
            r
        }
    };
    (ClientLink { cell }, handle_io)
}

/// Drive a pre-wired agent builder over `transport`, returning an [`AgentLink`]
/// plus a `handle_io` future with the same liveness contract as [`spawn_client`].
///
/// **Must be called inside a `tokio::task::LocalSet`** (see [`spawn_client`]).
pub fn spawn_agent<H, Run, O, I>(
    builder: acp::Builder<acp::Agent, H, Run>,
    transport: WatchedTransport<O, I>,
) -> (AgentLink, impl Future<Output = acp::Result<()>>)
where
    H: acp::HandleDispatchFrom<acp::Client> + 'static,
    Run: acp::RunWithConnectionTo<acp::Client> + 'static,
    O: futures::AsyncWrite + Send + Unpin + 'static,
    I: futures::AsyncRead + Send + Unpin + 'static,
{
    let WatchedTransport { inner, death } = transport;
    let cell = std::sync::Arc::new(Ready::default());
    let fill = cell.clone();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    tokio::task::spawn_local(async move {
        let result = builder
            .connect_with(inner, async move |cx| {
                let _ = fill.slot.set(cx.clone());
                fill.notify.notify_waiters();
                // See `spawn_client`: complete on peer death so `connect_with`
                // returns and `handle_io` resolves instead of hanging on EOF.
                death.wait().await;
                Ok(())
            })
            .await;
        let _ = done_tx.send(result);
    });
    let handle_io = {
        let cell = cell.clone();
        async move {
            // See `spawn_client`: a receive error means the connection task ended
            // without reporting — surface it as an error, don't mask it as a
            // clean `Ok(())` shutdown.
            let r = done_rx.await.unwrap_or_else(|_| {
                Err(acp::Error::internal_error()
                    .data("ACP connection task ended without reporting a result"))
            });
            cell.failed.store(true, std::sync::atomic::Ordering::Release);
            cell.notify.notify_waiters();
            r
        }
    };
    (AgentLink { cell }, handle_io)
}

/// Build a peer-death-watching `ByteStreams` transport from compat read/write
/// halves. The incoming half is wrapped so EOF / read errors trip a
/// [`TransportDeath`] latch that lets `handle_io` resolve when the peer dies.
pub fn byte_streams<O, I>(outgoing: O, incoming: I) -> WatchedTransport<O, I>
where
    O: futures::AsyncWrite + Send + Unpin + 'static,
    I: futures::AsyncRead + Send + Unpin + 'static,
{
    let death = std::sync::Arc::new(TransportDeath::default());
    let incoming = DeathWatchRead {
        inner: incoming,
        death: death.clone(),
    };
    WatchedTransport {
        inner: acp::ByteStreams::new(outgoing, incoming),
        death,
    }
}

/// Bridge an enum-typed `acp::Result<T>` into a builder request handler
/// (`AgentResponse`/`ClientResponse` serialize to `serde_json::Value`); the value
/// is already wrapped in its variant. Reply with the value or forward the error.
pub fn respond_enum<T: serde::Serialize>(
    responder: acp::Responder<serde_json::Value>,
    result: acp::Result<T>,
) -> acp::Result<()> {
    match result {
        Ok(value) => match serde_json::to_value(value) {
            Ok(v) => responder.respond(v),
            Err(e) => responder.respond_with_error(acp::Error::into_internal_error(e)),
        },
        Err(err) => responder.respond_with_error(err),
    }
}

#[allow(unused_imports)]
use v1 as _schema_marker;

#[cfg(test)]
mod transport_death_tests {
    //! Regression guard for AgentMasterDeath (#329): `spawn_client` /
    //! `spawn_agent`'s `handle_io` must resolve when the transport peer dies, so
    //! the helper can leave `Connected` and show the `/restart` degraded state
    //! (`client.rs`: `handle_io.await` -> `AgentFailure::TransportLost`).
    //!
    //! Under ACP 1.0 a `pending` `main_fn` left `connect_with` hung on a *clean*
    //! EOF (exactly what killing wta-master produces), so `handle_io` never fired
    //! and the pane stuck on `Connected`. The [`DeathWatchRead`] +
    //! [`TransportDeath`] wiring fixes that; these tests would hang (3s timeout)
    //! without it.
    use super::*;
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    /// Drops the transport far end (clean EOF, as a real `taskkill` on
    /// wta-master produces) and asserts the client `handle_io` resolves promptly.
    #[test]
    fn client_handle_io_resolves_when_peer_far_end_dies() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async {
            let (near, far) = tokio::io::duplex(64 * 1024);
            let (near_r, near_w) = tokio::io::split(near);

            let builder = acp::Client
                .builder()
                .name("test-client")
                .on_receive_request(
                    |_req: v1::AgentRequest, responder: acp::Responder<serde_json::Value>, _cx| async move {
                        responder.respond_with_error(acp::Error::method_not_found())
                    },
                    acp::on_receive_request!(),
                )
                .on_receive_notification(
                    |_notif: v1::AgentNotification, _cx| async move { Ok(()) },
                    acp::on_receive_notification!(),
                );

            let (_link, handle_io) =
                spawn_client(builder, byte_streams(near_w.compat_write(), near_r.compat()));

            // Simulate wta-master death: closing the far end makes `near` read EOF.
            drop(far);

            let res =
                tokio::time::timeout(std::time::Duration::from_secs(3), handle_io).await;
            assert!(
                res.is_ok(),
                "client handle_io must resolve when the master (transport far end) \
                 dies, but it hung — the helper would stay Connected forever"
            );
        });
    }

    /// Symmetric guard for the master side: when a helper (or the agent CLI) dies,
    /// the `spawn_agent` `handle_io` must resolve too.
    #[test]
    fn agent_handle_io_resolves_when_peer_far_end_dies() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async {
            let (near, far) = tokio::io::duplex(64 * 1024);
            let (near_r, near_w) = tokio::io::split(near);

            let builder = acp::Agent
                .builder()
                .name("test-agent")
                .on_receive_request(
                    |_req: v1::ClientRequest, responder: acp::Responder<serde_json::Value>, _cx| async move {
                        responder.respond_with_error(acp::Error::method_not_found())
                    },
                    acp::on_receive_request!(),
                )
                .on_receive_notification(
                    |_notif: v1::ClientNotification, _cx| async move { Ok(()) },
                    acp::on_receive_notification!(),
                );

            let (_link, handle_io) =
                spawn_agent(builder, byte_streams(near_w.compat_write(), near_r.compat()));

            drop(far);

            let res =
                tokio::time::timeout(std::time::Duration::from_secs(3), handle_io).await;
            assert!(
                res.is_ok(),
                "agent handle_io must resolve when the peer (transport far end) dies"
            );
        });
    }

    // ── Unit guards for the two primitives the fix is built on ──────────────
    //
    // The integration tests above prove the end-to-end contract; the ones below
    // pin the individual pieces so a future refactor of either primitive can't
    // silently reintroduce the hang (or, for `DeathWatchRead`, over-eagerly tear
    // down a live connection).

    /// A `futures::AsyncRead` that yields a single scripted `poll_read` result.
    struct ScriptedRead(Option<std::io::Result<usize>>);

    impl futures::AsyncRead for ScriptedRead {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: &mut [u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Ready(self.0.take().expect("ScriptedRead polled twice"))
        }
    }

    /// A no-op `Waker` so `DeathWatchRead::poll_read` can be driven synchronously
    /// without a runtime (uses `std::task::Wake`, so no `futures` test helpers).
    fn noop_waker() -> std::task::Waker {
        struct NoopWake;
        impl std::task::Wake for NoopWake {
            fn wake(self: std::sync::Arc<Self>) {}
            fn wake_by_ref(self: &std::sync::Arc<Self>) {}
        }
        std::sync::Arc::new(NoopWake).into()
    }

    /// Poll a `DeathWatchRead<ScriptedRead>` exactly once with the given buffer.
    fn poll_once(
        read: &mut DeathWatchRead<ScriptedRead>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let waker = noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        futures::AsyncRead::poll_read(std::pin::Pin::new(read), &mut cx, buf)
    }

    fn is_dead(d: &TransportDeath) -> bool {
        d.dead.load(std::sync::atomic::Ordering::Acquire)
    }

    #[test]
    fn death_watch_read_signals_on_eof() {
        let death = std::sync::Arc::new(TransportDeath::default());
        let mut reader = DeathWatchRead {
            inner: ScriptedRead(Some(Ok(0))),
            death: death.clone(),
        };
        let mut buf = [0u8; 8];
        let poll = poll_once(&mut reader, &mut buf);
        assert!(matches!(poll, std::task::Poll::Ready(Ok(0))));
        assert!(
            is_dead(&death),
            "a 0-byte read into a non-empty buffer is a real EOF and must signal death"
        );
    }

    #[test]
    fn death_watch_read_ignores_benign_empty_read() {
        // A 0-byte read into a *zero-length* buffer is NOT EOF — it must not be
        // mistaken for peer death (the `!buf.is_empty()` guard). Getting this
        // wrong would tear a live connection down on a spurious empty read.
        let death = std::sync::Arc::new(TransportDeath::default());
        let mut reader = DeathWatchRead {
            inner: ScriptedRead(Some(Ok(0))),
            death: death.clone(),
        };
        let mut empty: [u8; 0] = [];
        let poll = poll_once(&mut reader, &mut empty);
        assert!(matches!(poll, std::task::Poll::Ready(Ok(0))));
        assert!(
            !is_dead(&death),
            "an empty-buffer 0-byte read must not be treated as peer death"
        );
    }

    #[test]
    fn death_watch_read_signals_on_error() {
        let death = std::sync::Arc::new(TransportDeath::default());
        let err = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe gone");
        let mut reader = DeathWatchRead {
            inner: ScriptedRead(Some(Err(err))),
            death: death.clone(),
        };
        let mut buf = [0u8; 8];
        let poll = poll_once(&mut reader, &mut buf);
        assert!(matches!(poll, std::task::Poll::Ready(Err(_))));
        assert!(is_dead(&death), "a read error must signal death");
    }

    #[test]
    fn death_watch_read_passes_through_normal_read() {
        let death = std::sync::Arc::new(TransportDeath::default());
        let mut reader = DeathWatchRead {
            inner: ScriptedRead(Some(Ok(4))),
            death: death.clone(),
        };
        let mut buf = [0u8; 8];
        let poll = poll_once(&mut reader, &mut buf);
        assert!(matches!(poll, std::task::Poll::Ready(Ok(4))));
        assert!(
            !is_dead(&death),
            "a normal non-empty read must not signal death"
        );
    }

    #[test]
    fn transport_death_wait_returns_when_already_signaled() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let d = TransportDeath::default();
            d.signal();
            // Must return immediately; bound it so a regression fails fast
            // instead of hanging the whole test binary.
            tokio::time::timeout(std::time::Duration::from_secs(1), d.wait())
                .await
                .expect("wait() must return immediately when death was already signaled");
        });
    }

    #[test]
    fn transport_death_wait_wakes_on_later_signal() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let d = TransportDeath::default();
            // The signal races in *after* wait() has registered its Notify
            // interest — the exact case the `notified()`-before-`load` ordering
            // in `wait()` exists to handle.
            let guarded = tokio::time::timeout(std::time::Duration::from_secs(1), async {
                tokio::join!(d.wait(), async {
                    tokio::task::yield_now().await;
                    d.signal();
                });
            });
            guarded
                .await
                .expect("wait() must be woken by a signal that arrives after it starts waiting");
        });
    }

    #[test]
    fn transport_death_signal_is_idempotent() {
        let d = TransportDeath::default();
        assert!(!is_dead(&d));
        d.signal();
        d.signal(); // a second signal must be a harmless no-op
        assert!(is_dead(&d));
    }
}
