//! Relay envelopes and the transport boundary for the Remote Agent Host MVP.
//!
//! Canonical host state stores domain records, never AHP wire structs. This
//! module is the explicit conversion boundary between those records and the
//! official `ahp-types` envelope used on the wire.

use ahp_types::actions::{
    ActionEnvelope, ActionOrigin, RootActiveSessionsChangedAction, SessionTitleChangedAction,
    StateAction,
};
use ahp_types::messages::JsonRpcMessage;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{tcp::OwnedReadHalf, tcp::OwnedWriteHalf, TcpStream};

use super::state::HostId;

/// Largest accepted loopback frame. This protects the diagnostic host from an
/// accidental or malicious unbounded allocation.
const MAX_FRAME_BYTES: usize = 1024 * 1024;
const LOCAL_AUTH_PREFIX: &[u8] = b"wta-local-auth-v1\0";

/// The only domain mutations surfaced by the MVP.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum RelayKind {
    /// The root's session count changed. Individual catalogue rows are
    /// deliberately refetched with `listSessions` after reconnect.
    RootActiveSessionsChanged { active_sessions: i64 },
    /// A legacy summary or an AHP client changed a session title.
    SessionTitleChanged { session_id: String, title: String },
}

/// Client identity stamped on an accepted or rejected dispatch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RelayOrigin {
    pub(crate) client_id: String,
    pub(crate) client_seq: i64,
}

/// Host-authoritative event stored in the bounded replay buffer.
///
/// This is intentionally not `ahp_types::ActionEnvelope`: it records domain
/// event kinds and converts to the official wire envelope only at the relay
/// edge. That keeps SDK wire evolution out of persistent canonical state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RelayEnvelope {
    pub(crate) host_id: HostId,
    pub(crate) channel: String,
    pub(crate) server_seq: u64,
    pub(crate) kind: RelayKind,
    pub(crate) origin: Option<RelayOrigin>,
    pub(crate) rejection_reason: Option<String>,
}

impl RelayEnvelope {
    pub(crate) fn into_action_envelope(self) -> ActionEnvelope {
        let action = match self.kind {
            RelayKind::RootActiveSessionsChanged { active_sessions } => {
                StateAction::RootActiveSessionsChanged(RootActiveSessionsChangedAction {
                    active_sessions,
                })
            }
            RelayKind::SessionTitleChanged { title, .. } => {
                StateAction::SessionTitleChanged(SessionTitleChangedAction { title })
            }
        };

        ActionEnvelope {
            channel: self.channel,
            action,
            server_seq: self.server_seq,
            origin: self.origin.map(|origin| ActionOrigin {
                client_id: origin.client_id,
                client_seq: origin.client_seq,
            }),
            rejection_reason: self.rejection_reason,
        }
    }
}

/// Transport abstraction for server-side relay connections.
///
/// A future Azure Web PubSub adapter implements this trait and feeds the same
/// JSON-RPC messages into the host reducer. The current TCP framing is strictly
/// a loopback diagnostic transport, not a cloud relay.
#[async_trait]
pub(crate) trait RelayTransport: Send {
    /// Send one complete JSON-RPC message.
    async fn send_json(&mut self, message: JsonRpcMessage) -> Result<()>;

    /// Receive one complete JSON-RPC message, or `None` after clean close.
    async fn recv_json(&mut self) -> Result<Option<JsonRpcMessage>>;
}

/// Length-prefixed JSON-RPC transport used only by the local MVP.
pub(crate) struct LocalFramedTransport {
    reader: OwnedReadHalf,
    writer: OwnedWriteHalf,
}

impl LocalFramedTransport {
    /// Connect and complete the transport-level local authentication handshake
    /// before the AHP client sends its required first `initialize` message.
    pub(crate) async fn connect(address: std::net::SocketAddr, auth_token: &str) -> Result<Self> {
        let stream = TcpStream::connect(address)
            .await
            .with_context(|| format!("connect Remote Agent Host at {address}"))?;
        let mut transport = Self::from_stream(stream);
        let mut handshake = Vec::with_capacity(LOCAL_AUTH_PREFIX.len() + auth_token.len());
        handshake.extend_from_slice(LOCAL_AUTH_PREFIX);
        handshake.extend_from_slice(auth_token.as_bytes());
        transport.write_frame(&handshake).await?;
        Ok(transport)
    }

    /// Wrap an accepted local TCP stream.
    pub(crate) fn from_stream(stream: TcpStream) -> Self {
        let (reader, writer) = stream.into_split();
        Self { reader, writer }
    }

    pub(crate) async fn receive_auth_token(&mut self) -> Result<String> {
        let bytes = self
            .read_frame()
            .await?
            .context("local relay closed before authentication")?;
        let token = bytes
            .strip_prefix(LOCAL_AUTH_PREFIX)
            .context("invalid local relay authentication handshake")?;
        let token = std::str::from_utf8(token).context("local relay token is not UTF-8")?;
        anyhow::ensure!(!token.is_empty(), "local relay token is empty");
        anyhow::ensure!(token.len() <= 4096, "local relay token exceeds 4096 bytes");
        Ok(token.to_string())
    }

    async fn write_message(&mut self, message: &JsonRpcMessage) -> Result<()> {
        let bytes = serde_json::to_vec(message).context("serialize local relay message")?;
        self.write_frame(&bytes).await
    }

    async fn write_frame(&mut self, bytes: &[u8]) -> Result<()> {
        anyhow::ensure!(
            bytes.len() <= MAX_FRAME_BYTES,
            "local relay message exceeds {MAX_FRAME_BYTES} byte limit"
        );
        self.writer
            .write_u32_le(bytes.len() as u32)
            .await
            .context("write local relay frame length")?;
        self.writer
            .write_all(&bytes)
            .await
            .context("write local relay frame")?;
        self.writer.flush().await.context("flush local relay frame")
    }

    async fn read_message(&mut self) -> Result<Option<JsonRpcMessage>> {
        let Some(bytes) = self.read_frame().await? else {
            return Ok(None);
        };
        serde_json::from_slice(&bytes)
            .context("decode local relay JSON-RPC message")
            .map(Some)
    }

    async fn read_frame(&mut self) -> Result<Option<Vec<u8>>> {
        let length = match self.reader.read_u32_le().await {
            Ok(length) => length as usize,
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(error) => return Err(error).context("read local relay frame length"),
        };
        anyhow::ensure!(
            length <= MAX_FRAME_BYTES,
            "local relay frame exceeds {MAX_FRAME_BYTES} byte limit"
        );

        let mut bytes = vec![0; length];
        match self.reader.read_exact(&mut bytes).await {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Ok(None);
            }
            Err(error) => return Err(error).context("read local relay frame"),
        }
        Ok(Some(bytes))
    }
}

#[async_trait]
impl RelayTransport for LocalFramedTransport {
    async fn send_json(&mut self, message: JsonRpcMessage) -> Result<()> {
        self.write_message(&message).await
    }

    async fn recv_json(&mut self) -> Result<Option<JsonRpcMessage>> {
        self.read_message().await
    }
}

/// Let the official SDK client use the local transport unchanged.
impl ahp::Transport for LocalFramedTransport {
    async fn send(&mut self, message: ahp::TransportMessage) -> Result<(), ahp::TransportError> {
        let parsed = message.into_parsed()?;
        self.send_json(parsed)
            .await
            .map_err(|error| ahp::TransportError::Io(error.to_string()))
    }

    async fn recv(&mut self) -> Result<Option<ahp::TransportMessage>, ahp::TransportError> {
        self.recv_json()
            .await
            .map(|message| message.map(ahp::TransportMessage::Parsed))
            .map_err(|error| ahp::TransportError::Io(error.to_string()))
    }
}
