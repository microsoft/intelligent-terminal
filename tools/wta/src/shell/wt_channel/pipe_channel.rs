// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// PipeChannel — direct WT↔WTA transport over an inherited duplex anonymous
// pipe pair. Replaces the wtcli subprocess + COM round-trip for "critical"
// methods (initially: send_input).
//
// Handles are inherited from the parent WT process via STARTUPINFOEX
// PROC_THREAD_ATTRIBUTE_HANDLE_LIST and exposed to wta as decimal HANDLE
// values in two env vars:
//   WT_PROTOCOL_PIPE_R — handle to read responses from WT
//   WT_PROTOCOL_PIPE_W — handle to write requests to WT
//
// Wire format: 4-byte little-endian body length, then UTF-8 JSON-RPC 2.0 body.

use std::io::{BufReader, Read, Write};
use std::os::windows::io::{FromRawHandle, OwnedHandle, RawHandle};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context};

use super::WtChannel;

const MAX_FRAME_BYTES: u32 = 64 * 1024;

pub struct PipeChannel {
    inner: Arc<std::sync::Mutex<Inner>>,
    available: Arc<AtomicBool>,
}

struct Inner {
    writer: std::fs::File,
    reader: BufReader<std::fs::File>,
    next_id: u64,
}

impl PipeChannel {
    /// Look for `WT_PROTOCOL_PIPE_R` / `WT_PROTOCOL_PIPE_W` and, if both are
    /// present, claim the inherited handles. Strips `HANDLE_FLAG_INHERIT`
    /// immediately so the handles cannot be re-inherited by any future child
    /// process this wta itself spawns (e.g. the agent CLI).
    ///
    /// Returns `Ok(None)` (no error) if the env vars are not set — the caller
    /// falls back to CliChannel for backward compatibility.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let r_env = std::env::var("WT_PROTOCOL_PIPE_R").ok();
        let w_env = std::env::var("WT_PROTOCOL_PIPE_W").ok();
        let (r_str, w_str) = match (r_env, w_env) {
            (Some(r), Some(w)) => (r, w),
            _ => return Ok(None),
        };

        // Remove from the environment so they don't accidentally leak into
        // grandchildren (the agent CLI we spawn).
        std::env::remove_var("WT_PROTOCOL_PIPE_R");
        std::env::remove_var("WT_PROTOCOL_PIPE_W");

        let r_raw: usize = r_str
            .parse()
            .with_context(|| format!("Invalid WT_PROTOCOL_PIPE_R: {r_str}"))?;
        let w_raw: usize = w_str
            .parse()
            .with_context(|| format!("Invalid WT_PROTOCOL_PIPE_W: {w_str}"))?;

        // SAFETY: parent WT inherited these handles into our process via
        // PROC_THREAD_ATTRIBUTE_HANDLE_LIST. Taking ownership is correct;
        // OwnedHandle will close them on drop.
        let r_handle = unsafe { OwnedHandle::from_raw_handle(r_raw as RawHandle) };
        let w_handle = unsafe { OwnedHandle::from_raw_handle(w_raw as RawHandle) };

        // Strip HANDLE_FLAG_INHERIT: belt-and-braces against any future
        // bInheritHandles=TRUE spawn path leaking the privileged channel.
        clear_inherit_flag(&r_handle).context("clear inherit on read end")?;
        clear_inherit_flag(&w_handle).context("clear inherit on write end")?;

        let reader_file: std::fs::File = r_handle.into();
        let writer_file: std::fs::File = w_handle.into();

        let inner = Inner {
            writer: writer_file,
            reader: BufReader::new(reader_file),
            next_id: 0,
        };
        Ok(Some(Self {
            inner: Arc::new(std::sync::Mutex::new(inner)),
            available: Arc::new(AtomicBool::new(true)),
        }))
    }

    /// Send the initial `hello` frame and verify the server responds.
    pub async fn handshake(&self) -> anyhow::Result<()> {
        let pid = std::process::id();
        let params = serde_json::json!({
            "client": "wta",
            "version": env!("CARGO_PKG_VERSION"),
            "pid": pid,
        });
        let result = self.request("hello", params).await?;
        let server = result.get("server").and_then(|v| v.as_str()).unwrap_or("");
        if server != "wt" {
            bail!("PipeChannel handshake rejected: server={server:?}");
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl WtChannel for PipeChannel {
    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        if !self.available.load(Ordering::Relaxed) {
            bail!("PipeChannel is not available");
        }
        let inner = self.inner.clone();
        let available = self.available.clone();
        let method = method.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<serde_json::Value> {
            let mut guard = inner
                .lock()
                .map_err(|_| anyhow!("PipeChannel mutex poisoned"))?;
            guard.next_id = guard.next_id.wrapping_add(1);
            let id = guard.next_id;
            let req = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            });
            let result = (|| -> anyhow::Result<serde_json::Value> {
                write_frame(&mut guard.writer, &req)?;
                let resp = read_frame(&mut guard.reader)?;
                if let Some(err) = resp.get("error") {
                    bail!(
                        "JSON-RPC error: {}",
                        serde_json::to_string(err).unwrap_or_default()
                    );
                }
                Ok(resp
                    .get("result")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null))
            })();
            if result.is_err() {
                // Pipe IO failed — mark the channel dead so subsequent calls
                // surface the same error rather than re-trying a broken pipe.
                available.store(false, Ordering::Relaxed);
            }
            result
        })
        .await
        .context("PipeChannel blocking task join failed")?
    }

    fn is_available(&self) -> bool {
        self.available.load(Ordering::Relaxed)
    }
}

fn write_frame<W: Write>(w: &mut W, body: &serde_json::Value) -> anyhow::Result<()> {
    let body_bytes = serde_json::to_vec(body)?;
    if body_bytes.len() > MAX_FRAME_BYTES as usize {
        bail!("Outgoing frame too large: {} bytes", body_bytes.len());
    }
    let len = body_bytes.len() as u32;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&body_bytes)?;
    w.flush()?;
    Ok(())
}

fn read_frame<R: Read>(r: &mut R) -> anyhow::Result<serde_json::Value> {
    let mut len_bytes = [0u8; 4];
    r.read_exact(&mut len_bytes)
        .context("PipeChannel read frame length failed (peer closed?)")?;
    let len = u32::from_le_bytes(len_bytes);
    if len == 0 || len > MAX_FRAME_BYTES {
        bail!("Invalid frame length: {len}");
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body)
        .context("PipeChannel read frame body failed")?;
    let v: serde_json::Value =
        serde_json::from_slice(&body).context("PipeChannel JSON parse failed")?;
    Ok(v)
}

fn clear_inherit_flag(h: &OwnedHandle) -> anyhow::Result<()> {
    use std::os::windows::io::AsRawHandle;
    extern "system" {
        fn SetHandleInformation(
            h: *mut std::ffi::c_void,
            mask: u32,
            flags: u32,
        ) -> i32;
    }
    const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;

    let raw = h.as_raw_handle() as *mut std::ffi::c_void;
    let ok = unsafe { SetHandleInformation(raw, HANDLE_FLAG_INHERIT, 0) };
    if ok == 0 {
        bail!(
            "SetHandleInformation failed: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}
