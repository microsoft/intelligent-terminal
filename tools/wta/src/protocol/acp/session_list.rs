//! Shared ACP `session/list` plumbing.
//!
//! Drives the minimal client side of an ACP connection — `initialize`
//! then `session/list` — over an already-spawned agent process's piped
//! stdio. Two callers need exactly this exchange, so it lives here once:
//!
//! * the `probe-sessions` diagnostic ([`super::probe`]), which spawns a
//!   Windows-side agent and dumps the raw result; and
//! * the production WSL history scan ([`crate::wsl_acp`]), which spawns the
//!   distro's CLI through `wsl.exe` and maps the rows into `AgentSession`s.
//!
//! Callers must drive this inside a tokio `LocalSet`: the stderr drain and the
//! ACP connection I/O are spawned via [`tokio::task::spawn_local`] (the
//! `agent-client-protocol` 1.0 connection itself is `Send`).

use agent_client_protocol as acp;
use anyhow::{anyhow, Result};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::protocol::acp::conn;

/// The successful list outcome, or a human-readable reason it failed.
///
/// `session/list` is an UNSTABLE ACP capability: an agent that doesn't
/// implement it answers `Method not found`. That is a normal,
/// non-fatal outcome (distinct from a transport/`initialize` failure,
/// which surfaces as the outer `Err`), so it is captured as a `String`
/// rather than collapsing the whole call.
pub(crate) type ListOutcome = std::result::Result<Vec<acp::schema::v1::SessionInfo>, String>;

/// Run ACP `initialize` then `session/list` over `child`'s piped stdio.
///
/// Returns the `initialize` response (so the diagnostic caller can dump
/// the agent's advertised capabilities) alongside the `session/list`
/// [`ListOutcome`]. `child` must have `stdin`/`stdout` piped; `stderr`,
/// when piped, is drained so a chatty agent can't deadlock the pipe.
///
/// Runs ACP I/O and the stderr drain via [`tokio::task::spawn_local`], so call
/// this inside a tokio `LocalSet`.
pub(crate) async fn fetch_session_list(
    child: &mut tokio::process::Child,
    client_label: &str,
    init_timeout: Duration,
    list_timeout: Duration,
) -> Result<(acp::schema::v1::InitializeResponse, ListOutcome)> {
    let outgoing = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("agent stdin not piped"))?
        .compat_write();
    let incoming = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("agent stdout not piped"))?
        .compat();
    if let Some(stderr) = child.stderr.take() {
        let label = client_label.to_string();
        tokio::task::spawn_local(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "acp_session_list", agent = %label, "agent stderr: {}", line);
            }
        });
    }

    let (conn, handle_io) =
        conn::spawn_client(acp::Client.builder().name("wta-session-list"), conn::byte_streams(outgoing, incoming));
    let io_label = client_label.to_string();
    tokio::task::spawn_local(async move {
        if let Err(e) = handle_io.await {
            tracing::warn!(target: "acp_session_list", agent = %io_label, "handle_io failed: {:#}", e);
        }
    });

    let init_req = acp::schema::v1::InitializeRequest::new(acp::schema::ProtocolVersion::V1)
        .client_capabilities(acp::schema::v1::ClientCapabilities::new().terminal(true))
        .client_info(
            acp::schema::v1::Implementation::new("wta-session-list", env!("CARGO_PKG_VERSION"))
                .title("WTA Session List"),
        );
    let init_resp = tokio::time::timeout(init_timeout, conn.initialize(init_req))
        .await
        .map_err(|_| {
            anyhow!(
                "ACP initialize timed out after {:?} (agent={})",
                init_timeout,
                client_label
            )
        })?
        .map_err(|e| anyhow!("initialize failed (agent={}): {}", client_label, e))?;

    let list = match tokio::time::timeout(
        list_timeout,
        conn.list_sessions(acp::schema::v1::ListSessionsRequest::new()),
    )
    .await
    {
        Err(_) => Err(format!(
            "session/list timed out after {list_timeout:?} (agent={client_label})"
        )),
        Ok(Err(e)) => Err(format!("session/list failed (agent={client_label}): {e}")),
        Ok(Ok(resp)) => Ok(resp.sessions),
    };

    Ok((init_resp, list))
}
