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

/// Drive a pre-wired client builder over `transport`, returning a [`ClientLink`]
/// for sending requests plus a `handle_io` future. The future resolves when the
/// connection ends: clean EOF → `Ok(())`, transport error → `Err`.
///
/// **Must be called inside a `tokio::task::LocalSet`** — it drives the connection
/// I/O via [`tokio::task::spawn_local`] and will panic on a runtime without one
/// (the WTA helper/master/probe/CLI entry points all establish a `LocalSet`).
pub fn spawn_client<H, Run>(
    builder: acp::Builder<acp::Client, H, Run>,
    transport: impl acp::ConnectTo<acp::Client> + 'static,
) -> (ClientLink, impl Future<Output = acp::Result<()>>)
where
    H: acp::HandleDispatchFrom<acp::Agent> + 'static,
    Run: acp::RunWithConnectionTo<acp::Agent> + 'static,
{
    let cell = std::sync::Arc::new(Ready::default());
    let fill = cell.clone();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    tokio::task::spawn_local(async move {
        let result = builder
            .connect_with(transport, async move |cx| {
                let _ = fill.slot.set(cx.clone());
                fill.notify.notify_waiters();
                std::future::pending::<acp::Result<()>>().await
            })
            .await;
        let _ = done_tx.send(result);
    });
    let handle_io = {
        let cell = cell.clone();
        async move {
            let r = done_rx.await.unwrap_or(Ok(()));
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
pub fn spawn_agent<H, Run>(
    builder: acp::Builder<acp::Agent, H, Run>,
    transport: impl acp::ConnectTo<acp::Agent> + 'static,
) -> (AgentLink, impl Future<Output = acp::Result<()>>)
where
    H: acp::HandleDispatchFrom<acp::Client> + 'static,
    Run: acp::RunWithConnectionTo<acp::Client> + 'static,
{
    let cell = std::sync::Arc::new(Ready::default());
    let fill = cell.clone();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    tokio::task::spawn_local(async move {
        let result = builder
            .connect_with(transport, async move |cx| {
                let _ = fill.slot.set(cx.clone());
                fill.notify.notify_waiters();
                std::future::pending::<acp::Result<()>>().await
            })
            .await;
        let _ = done_tx.send(result);
    });
    let handle_io = {
        let cell = cell.clone();
        async move {
            let r = done_rx.await.unwrap_or(Ok(()));
            cell.failed.store(true, std::sync::atomic::Ordering::Release);
            cell.notify.notify_waiters();
            r
        }
    };
    (AgentLink { cell }, handle_io)
}

/// Build a `ByteStreams` transport from compat read/write halves.
pub fn byte_streams<O, I>(outgoing: O, incoming: I) -> acp::ByteStreams<O, I>
where
    O: futures::AsyncWrite + Send + Unpin + 'static,
    I: futures::AsyncRead + Send + Unpin + 'static,
{
    acp::ByteStreams::new(outgoing, incoming)
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
