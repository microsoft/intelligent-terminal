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
//! The ACP 0.10 connection is `!Send`, so callers must drive this inside a
//! tokio `LocalSet`.

use acp::Agent as _;
use agent_client_protocol as acp;
use anyhow::{anyhow, Result};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// No-op ACP client. `initialize` + `session/list` never trigger
/// server→client calls, so every method here is a fail-fast safety net
/// rather than a real implementation.
pub(crate) struct StubClient;

#[async_trait::async_trait(?Send)]
impl acp::Client for StubClient {
    async fn request_permission(
        &self,
        _: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        Err(acp::Error::internal_error()
            .data("session-list client does not handle permissions".to_string()))
    }

    async fn session_notification(&self, _: acp::SessionNotification) -> acp::Result<()> {
        Ok(())
    }

    async fn create_terminal(
        &self,
        _: acp::CreateTerminalRequest,
    ) -> acp::Result<acp::CreateTerminalResponse> {
        Err(acp::Error::internal_error()
            .data("session-list client does not create terminals".to_string()))
    }

    async fn terminal_output(
        &self,
        _: acp::TerminalOutputRequest,
    ) -> acp::Result<acp::TerminalOutputResponse> {
        Err(acp::Error::internal_error()
            .data("session-list client does not run terminals".to_string()))
    }

    async fn wait_for_terminal_exit(
        &self,
        _: acp::WaitForTerminalExitRequest,
    ) -> acp::Result<acp::WaitForTerminalExitResponse> {
        Err(acp::Error::internal_error()
            .data("session-list client does not run terminals".to_string()))
    }

    async fn release_terminal(
        &self,
        _: acp::ReleaseTerminalRequest,
    ) -> acp::Result<acp::ReleaseTerminalResponse> {
        Err(acp::Error::internal_error()
            .data("session-list client does not run terminals".to_string()))
    }

    async fn kill_terminal(
        &self,
        _: acp::KillTerminalRequest,
    ) -> acp::Result<acp::KillTerminalResponse> {
        Err(acp::Error::internal_error()
            .data("session-list client does not run terminals".to_string()))
    }
}

/// The successful list outcome, or a human-readable reason it failed.
///
/// `session/list` is an UNSTABLE ACP capability: an agent that doesn't
/// implement it answers `Method not found`. That is a normal,
/// non-fatal outcome (distinct from a transport/`initialize` failure,
/// which surfaces as the outer `Err`), so it is captured as a `String`
/// rather than collapsing the whole call.
pub(crate) type ListOutcome = std::result::Result<Vec<acp::SessionInfo>, String>;

/// Run ACP `initialize` then `session/list` over `child`'s piped stdio.
///
/// Returns the `initialize` response (so the diagnostic caller can dump
/// the agent's advertised capabilities) alongside the `session/list`
/// [`ListOutcome`]. `child` must have `stdin`/`stdout` piped; `stderr`,
/// when piped, is drained so a chatty agent can't deadlock the pipe.
///
/// The ACP connection is `!Send`; call this inside a tokio `LocalSet`.
pub(crate) async fn fetch_session_list(
    child: &mut tokio::process::Child,
    client_label: &str,
    init_timeout: Duration,
    list_timeout: Duration,
) -> Result<(acp::InitializeResponse, ListOutcome)> {
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
        tokio::task::spawn_local(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "acp_session_list", "agent stderr: {}", line);
            }
        });
    }

    let (conn, handle_io) = acp::ClientSideConnection::new(StubClient, outgoing, incoming, |fut| {
        tokio::task::spawn_local(fut);
    });
    tokio::task::spawn_local(async move {
        if let Err(e) = handle_io.await {
            tracing::warn!(target: "acp_session_list", "handle_io failed: {:#}", e);
        }
    });

    let init_req = acp::InitializeRequest::new(acp::ProtocolVersion::V1)
        .client_capabilities(acp::ClientCapabilities::new().terminal(true))
        .client_info(
            acp::Implementation::new("wta-session-list", env!("CARGO_PKG_VERSION"))
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
        conn.list_sessions(acp::ListSessionsRequest::new()),
    )
    .await
    {
        Err(_) => Err(format!("session/list timed out after {list_timeout:?}")),
        Ok(Err(e)) => Err(format!("{e}")),
        Ok(Ok(resp)) => Ok(resp.sessions),
    };

    Ok((init_resp, list))
}
