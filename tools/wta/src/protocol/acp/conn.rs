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

/// Client-side connection handle (talks to an agent CLI or to master). The
/// connection is delivered asynchronously from `connect_with`'s main closure, so
/// it lives behind a shared cell that `spawn_client` fills before the handshake.
#[derive(Clone, Debug)]
pub struct ClientLink {
    cell: std::sync::Arc<std::sync::OnceLock<acp::ConnectionTo<acp::Agent>>>,
}

impl ClientLink {
    fn cx(&self) -> impl std::future::Future<Output = acp::ConnectionTo<acp::Agent>> + '_ {
        async move {
            loop {
                if let Some(c) = self.cell.get() {
                    return c.clone();
                }
                tokio::task::yield_now().await;
            }
        }
    }

    pub async fn initialize(&self, req: InitializeRequest) -> acp::Result<InitializeResponse> {
        self.cx().await.send_request(req).block_task().await
    }

    pub async fn authenticate(&self, req: AuthenticateRequest) -> acp::Result<AuthenticateResponse> {
        self.cx().await.send_request(req).block_task().await
    }

    pub async fn new_session(&self, req: NewSessionRequest) -> acp::Result<NewSessionResponse> {
        self.cx().await.send_request(req).block_task().await
    }

    pub async fn load_session(&self, req: LoadSessionRequest) -> acp::Result<LoadSessionResponse> {
        self.cx().await.send_request(req).block_task().await
    }

    pub async fn prompt(&self, req: PromptRequest) -> acp::Result<PromptResponse> {
        self.cx().await.send_request(req).block_task().await
    }

    pub async fn cancel(&self, notif: CancelNotification) -> acp::Result<()> {
        self.cx().await.send_notification(notif)
    }

    pub async fn set_session_mode(
        &self,
        req: SetSessionModeRequest,
    ) -> acp::Result<SetSessionModeResponse> {
        self.cx().await.send_request(req).block_task().await
    }

    pub async fn set_session_model(
        &self,
        req: SetSessionModelRequest,
    ) -> acp::Result<SetSessionModelResponse> {
        self.cx().await.send_request(req).block_task().await
    }

    pub async fn set_session_config_option(
        &self,
        req: SetSessionConfigOptionRequest,
    ) -> acp::Result<SetSessionConfigOptionResponse> {
        self.cx().await.send_request(req).block_task().await
    }

    pub async fn list_sessions(&self, req: ListSessionsRequest) -> acp::Result<ListSessionsResponse> {
        self.cx().await.send_request(req).block_task().await
    }

    pub async fn ext_method(&self, req: ExtRequest) -> acp::Result<ExtResponse> {
        let value = self
            .cx()
            .await
            .send_request(v1::ClientRequest::ExtMethodRequest(req))
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
    cell: std::sync::Arc<std::sync::OnceLock<acp::ConnectionTo<acp::Client>>>,
}

impl AgentLink {
    fn cx(&self) -> impl std::future::Future<Output = acp::ConnectionTo<acp::Client>> + '_ {
        async move {
            loop {
                if let Some(c) = self.cell.get() {
                    return c.clone();
                }
                tokio::task::yield_now().await;
            }
        }
    }

    pub async fn request_permission(
        &self,
        req: RequestPermissionRequest,
    ) -> acp::Result<RequestPermissionResponse> {
        self.cx().await.send_request(req).block_task().await
    }

    pub async fn write_text_file(
        &self,
        req: WriteTextFileRequest,
    ) -> acp::Result<WriteTextFileResponse> {
        self.cx().await.send_request(req).block_task().await
    }

    pub async fn read_text_file(
        &self,
        req: ReadTextFileRequest,
    ) -> acp::Result<ReadTextFileResponse> {
        self.cx().await.send_request(req).block_task().await
    }

    pub async fn create_terminal(
        &self,
        req: CreateTerminalRequest,
    ) -> acp::Result<CreateTerminalResponse> {
        self.cx().await.send_request(req).block_task().await
    }

    pub async fn terminal_output(
        &self,
        req: TerminalOutputRequest,
    ) -> acp::Result<TerminalOutputResponse> {
        self.cx().await.send_request(req).block_task().await
    }

    pub async fn release_terminal(
        &self,
        req: ReleaseTerminalRequest,
    ) -> acp::Result<ReleaseTerminalResponse> {
        self.cx().await.send_request(req).block_task().await
    }

    pub async fn wait_for_terminal_exit(
        &self,
        req: WaitForTerminalExitRequest,
    ) -> acp::Result<WaitForTerminalExitResponse> {
        self.cx().await.send_request(req).block_task().await
    }

    pub async fn kill_terminal(&self, req: KillTerminalRequest) -> acp::Result<KillTerminalResponse> {
        self.cx().await.send_request(req).block_task().await
    }

    pub async fn session_notification(&self, notif: SessionNotification) -> acp::Result<()> {
        self.cx().await.send_notification(notif)
    }

    pub async fn ext_notification(&self, notif: ExtNotification) -> acp::Result<()> {
        self.cx()
            .await
            .send_notification(v1::AgentNotification::ExtNotification(notif))
    }
}

/// Drive a pre-wired client builder over `transport`, returning a [`ClientLink`]
/// for sending requests plus a `handle_io` future. The future resolves when the
/// connection ends: clean EOF → `Ok(())`, transport error → `Err`.
pub fn spawn_client<H, Run>(
    builder: acp::Builder<acp::Client, H, Run>,
    transport: impl acp::ConnectTo<acp::Client> + 'static,
) -> (ClientLink, impl Future<Output = acp::Result<()>>)
where
    H: acp::HandleDispatchFrom<acp::Agent> + 'static,
    Run: acp::RunWithConnectionTo<acp::Agent> + 'static,
{
    let cell = std::sync::Arc::new(std::sync::OnceLock::new());
    let fill = cell.clone();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    tokio::task::spawn_local(async move {
        let result = builder
            .connect_with(transport, async move |cx| {
                let _ = fill.set(cx.clone());
                std::future::pending::<acp::Result<()>>().await
            })
            .await;
        let _ = done_tx.send(result);
    });
    let handle_io = async move { done_rx.await.unwrap_or(Ok(())) };
    (ClientLink { cell }, handle_io)
}

/// Drive a pre-wired agent builder over `transport`, returning an [`AgentLink`]
/// plus a `handle_io` future with the same liveness contract as [`spawn_client`].
pub fn spawn_agent<H, Run>(
    builder: acp::Builder<acp::Agent, H, Run>,
    transport: impl acp::ConnectTo<acp::Agent> + 'static,
) -> (AgentLink, impl Future<Output = acp::Result<()>>)
where
    H: acp::HandleDispatchFrom<acp::Client> + 'static,
    Run: acp::RunWithConnectionTo<acp::Client> + 'static,
{
    let cell = std::sync::Arc::new(std::sync::OnceLock::new());
    let fill = cell.clone();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    tokio::task::spawn_local(async move {
        let result = builder
            .connect_with(transport, async move |cx| {
                let _ = fill.set(cx.clone());
                std::future::pending::<acp::Result<()>>().await
            })
            .await;
        let _ = done_tx.send(result);
    });
    let handle_io = async move { done_rx.await.unwrap_or(Ok(())) };
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
