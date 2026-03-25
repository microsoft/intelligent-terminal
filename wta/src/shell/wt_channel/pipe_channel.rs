use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::ClientOptions;
use tokio::sync::{mpsc, Mutex};

use super::types::{WireRequest, WireResponse};
use super::WtChannel;
use crate::app::DebugMessage;

/// Named-pipe channel to the Windows Terminal protocol server.
///
/// Connects to `\\.\pipe\WindowsTerminal-<PID>` using env var `WT_PIPE_NAME`.
/// `WT_MCP_TOKEN` is optional — if missing, sends empty string (dev bypass).
/// Debug logging to `wta-pipe-debug.log` is opt-in via `WTA_DEBUG_LOG=1`.
pub struct PipeChannel {
    pipe: Mutex<tokio::net::windows::named_pipe::NamedPipeClient>,
    next_id: AtomicU64,
    available: AtomicBool,
    debug_log: Option<Mutex<std::fs::File>>,
    debug_tx: Option<mpsc::UnboundedSender<DebugMessage>>,
    debug_enabled: Option<Arc<AtomicBool>>,
}

impl PipeChannel {
    /// Connect to the WT protocol server and authenticate.
    ///
    /// Reads `WT_PIPE_NAME` from environment (required).
    /// `WT_MCP_TOKEN` is optional — defaults to empty string for dev bypass.
    /// Debug log is written to `wta-pipe-debug.log` only when `WTA_DEBUG_LOG=1`.
    pub async fn connect() -> anyhow::Result<Self> {
        let pipe_name = std::env::var("WT_PIPE_NAME").context(
            "WT_PIPE_NAME not set. Must run inside a Windows Terminal pane with protocol access.",
        )?;
        // Token is optional for dev — empty string triggers the dev bypass in WT.
        let token = std::env::var("WT_MCP_TOKEN").unwrap_or_default();

        Self::connect_with(&pipe_name, &token).await
    }

    /// Connect to a specific pipe with an explicit name and token.
    /// This avoids needing environment variables (e.g. after VT discovery).
    pub async fn connect_with(pipe_name: &str, token: &str) -> anyhow::Result<Self> {
        let pipe = ClientOptions::new()
            .open(pipe_name)
            .context(format!("Failed to connect to pipe: {}", pipe_name))?;

        let debug_log = if std::env::var("WTA_DEBUG_LOG").as_deref() == Ok("1") {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("wta-pipe-debug.log")
                .ok();
            file.map(Mutex::new)
        } else {
            None
        };

        let channel = Self {
            pipe: Mutex::new(pipe),
            next_id: AtomicU64::new(1),
            available: AtomicBool::new(false),
            debug_log,
            debug_tx: None,
            debug_enabled: None,
        };

        channel
            .log(&format!("Connecting to {} ...", pipe_name))
            .await;

        // Authenticate (empty token triggers dev bypass on WT side)
        channel.log("Authenticating...").await;
        let result = channel
            .request_inner("authenticate", serde_json::json!({ "token": token }))
            .await
            .context("Authentication failed")?;

        let authenticated = result
            .get("authenticated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !authenticated {
            bail!("Authentication rejected by Windows Terminal");
        }

        channel.available.store(true, Ordering::Relaxed);
        channel.log("Authenticated successfully").await;
        Ok(channel)
    }

    /// Attach a debug message sender for the TUI debug panel.
    pub fn with_debug_sender(
        mut self,
        tx: mpsc::UnboundedSender<DebugMessage>,
        enabled: Arc<AtomicBool>,
    ) -> Self {
        self.debug_tx = Some(tx);
        self.debug_enabled = Some(enabled);
        self
    }

    fn emit_debug(&self, direction: crate::app::DebugDir, content: String) {
        let Some(enabled) = &self.debug_enabled else {
            return;
        };
        if !enabled.load(Ordering::Relaxed) {
            return;
        }
        if let Some(ref tx) = self.debug_tx {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
            let _ = tx.send(DebugMessage {
                timestamp: ts,
                direction,
                content,
            });
        }
    }

    async fn log(&self, msg: &str) {
        if let Some(ref log_file) = self.debug_log {
            use std::io::Write;
            let mut f = log_file.lock().await;
            let elapsed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            let _ = writeln!(f, "[{:.3}] {}", elapsed.as_secs_f64(), msg);
        }
    }

    /// Core request implementation with full logging.
    async fn request_inner(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).to_string();

        let wire_req = WireRequest {
            msg_type: "request",
            id,
            method,
            params,
        };

        let mut json = serde_json::to_string(&wire_req)?;
        self.log(&format!(">>> {}", json)).await;
        self.emit_debug(crate::app::DebugDir::Sent, json.clone());
        json.push('\n');

        let mut pipe = self.pipe.lock().await;

        // Write request
        pipe.write_all(json.as_bytes()).await?;

        // Read response line (byte-by-byte until \n)
        let mut buf = Vec::with_capacity(4096);
        loop {
            let byte = pipe.read_u8().await?;
            if byte == b'\n' {
                break;
            }
            buf.push(byte);
        }

        let resp_str = String::from_utf8_lossy(&buf);
        self.log(&format!("<<< {}", resp_str)).await;
        self.emit_debug(crate::app::DebugDir::Received, resp_str.to_string());

        let resp: WireResponse = serde_json::from_slice(&buf)
            .context("Failed to parse response from Windows Terminal")?;

        if let Some(err) = resp.error {
            bail!("WT protocol error [{}]: {}", err.code, err.message);
        }

        Ok(resp.result.unwrap_or(serde_json::Value::Null))
    }
}

#[async_trait::async_trait]
impl WtChannel for PipeChannel {
    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        self.request_inner(method, params).await
    }

    fn is_available(&self) -> bool {
        self.available.load(Ordering::Relaxed)
    }
}
