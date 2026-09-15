//! Shared ACP `session/list` plumbing.
//!
//! Drives the minimal client side of an ACP connection — `initialize`
//! then `session/list` — over an already-spawned agent process's piped
//! stdio. It lives here, separate from its caller, so the exchange stays
//! reusable for any diagnostic or scan that needs a one-shot session list:
//!
//! * the `probe-sessions` diagnostic ([`super::probe`]) spawns a
//!   Windows-side agent and dumps the raw result.
//!
//! Callers must drive this inside a tokio `LocalSet`: the stderr drain and the
//! ACP connection I/O are spawned via [`tokio::task::spawn_local`] (the
//! `agent-client-protocol` 1.0 connection itself is `Send`).

use agent_client_protocol as acp;
use anyhow::{anyhow, Result};
use std::collections::HashSet;
use std::time::Duration;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tokio_util::task::AbortOnDropHandle;

use crate::protocol::acp::conn;
use crate::protocol::acp::spawn::AgentStderrLog;

const MAX_LIST_PAGES: usize = 100;
const MAX_LIST_ITEMS: usize = 10_000;

/// The successful list outcome, or a human-readable reason it failed.
///
/// `session/list` is an UNSTABLE ACP capability: an agent that doesn't
/// implement it answers `Method not found`. That is a normal,
/// non-fatal outcome (distinct from a transport/`initialize` failure,
/// which surfaces as the outer `Err`), so it is captured as a `String`
/// rather than collapsing the whole call.
pub(crate) type ListOutcome = std::result::Result<Vec<acp::schema::v1::SessionInfo>, String>;

// The SDK's ListSessionsResponse deliberately defaults malformed arrays/cursors
// and skips invalid rows. A complete history scan must fail instead of silently
// reporting an empty or truncated success.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionListPage {
    sessions: Vec<acp::schema::v1::SessionInfo>,
    next_cursor: Option<String>,
}

fn readonly_client_builder() -> acp::Builder<acp::Client, impl acp::HandleDispatchFrom<acp::Agent>>
{
    acp::Client
        .builder()
        .name("wta-session-list")
        .on_receive_dispatch(
            |message: acp::Dispatch, _connection| async move {
                match message {
                    // The SDK otherwise queues session-scoped messages until a
                    // session handler appears. A history reader never creates
                    // one, so reject requests explicitly rather than waiting.
                    acp::Dispatch::Request(_, responder) => {
                        responder.respond_with_error(acp::Error::method_not_found())
                    }
                    acp::Dispatch::Notification(_) => Ok(()),
                    acp::Dispatch::Response(result, router) => router.respond_with_result(result),
                }
            },
            acp::on_receive_dispatch!(),
        )
}

/// Run read-only ACP `initialize` then paginated `session/list` over piped stdio.
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
    let stderr_log = AgentStderrLog::new(client_label.to_string());
    let mut stderr_task = child
        .stderr
        .take()
        .map(|stderr| AbortOnDropHandle::new(stderr_log.drain(stderr)));

    let (conn, handle_io) = conn::spawn_client(
        readonly_client_builder(),
        conn::byte_streams(outgoing, incoming),
    );
    let io_label = client_label.to_string();
    let _io_task = AbortOnDropHandle::new(tokio::task::spawn_local(async move {
        if let Err(e) = handle_io.await {
            tracing::warn!(target: "acp_session_list", agent = %io_label, "handle_io failed: {:#}", e);
        }
    }));

    let init_result =
        tokio::time::timeout(init_timeout, conn.initialize(initialize_request())).await;
    let startup_stderr = if matches!(init_result, Ok(Ok(_))) {
        stderr_log.mark_initialized();
        Vec::new()
    } else {
        stderr_log
            .finish_failed_startup(child, stderr_task.take().map(AbortOnDropHandle::detach))
            .await
    };
    let startup_context = if startup_stderr.is_empty() {
        String::new()
    } else {
        format!("; stderr: {}", startup_stderr.join("\n"))
    };
    let init_resp = init_result
        .map_err(|_| {
            anyhow!(
                "ACP initialize timed out after {:?} (agent={}){}",
                init_timeout,
                client_label,
                startup_context
            )
        })?
        .map_err(|e| {
            anyhow!(
                "initialize failed (agent={}): {}{}",
                client_label,
                e,
                startup_context
            )
        })?;

    let list = fetch_all_pages(&conn, client_label, list_timeout).await;
    Ok((init_resp, list))
}

fn initialize_request() -> acp::schema::v1::InitializeRequest {
    // A history reader has no filesystem, terminal, or permission handlers.
    acp::schema::v1::InitializeRequest::new(acp::schema::ProtocolVersion::V1)
        .client_capabilities(acp::schema::v1::ClientCapabilities::new())
        .client_info(
            acp::schema::v1::Implementation::new("wta-session-list", env!("CARGO_PKG_VERSION"))
                .title("WTA Session List"),
        )
}

async fn fetch_all_pages(
    conn: &conn::ClientLink,
    client_label: &str,
    list_timeout: Duration,
) -> ListOutcome {
    // One deadline for the complete list, not one fresh timeout per page.
    match tokio::time::timeout(list_timeout, async {
        let mut sessions = Vec::new();
        let mut cursors = HashSet::new();
        let mut cursor = None;
        for _ in 0..MAX_LIST_PAGES {
            let mut request = acp::schema::v1::ListSessionsRequest::new();
            request.cursor = cursor;
            let params = serde_json::value::to_raw_value(&request)
                .map_err(|e| format!("serialize session/list request: {e}"))?;
            // Reuse the connection's raw response path; this is still the
            // standard session/list wire method, not an extension RPC.
            let response = conn
                .ext_method(acp::schema::v1::ExtRequest::new(
                    "session/list",
                    params.into(),
                ))
                .await
                .map_err(|e| format!("session/list failed (agent={client_label}): {e}"))?;
            let response: SessionListPage =
                serde_json::from_str(response.0.get()).map_err(|e| {
                    format!("invalid session/list response (agent={client_label}): {e}")
                })?;
            if response.sessions.len() > MAX_LIST_ITEMS - sessions.len() {
                return Err(format!("session/list exceeded {MAX_LIST_ITEMS} items"));
            }
            sessions.extend(response.sessions);
            match response.next_cursor {
                None => return Ok(sessions),
                Some(next) => {
                    if !cursors.insert(next.clone()) {
                        return Err("session/list returned a repeated pagination cursor".into());
                    }
                    cursor = Some(next);
                }
            }
        }
        Err(format!("session/list exceeded {MAX_LIST_PAGES} pages"))
    })
    .await
    {
        Err(_) => Err(format!(
            "session/list timed out after {list_timeout:?} (agent={client_label})"
        )),
        Ok(result) => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acp::schema::v1::{
        InitializeRequest, InitializeResponse, ListSessionsRequest, ListSessionsResponse,
        SessionId, SessionInfo,
    };
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    struct MockListingAgent {
        client: conn::ClientLink,
        agent: conn::AgentLink,
        cursors: Arc<Mutex<Vec<Option<String>>>>,
        _client_io: AbortOnDropHandle<()>,
        _agent_io: AbortOnDropHandle<()>,
    }

    fn mock_agent(pages: Vec<acp::Result<ListSessionsResponse>>) -> MockListingAgent {
        let (client_pipe, agent_pipe) = tokio::io::duplex(8192);
        let (client_read, client_write) = tokio::io::split(client_pipe);
        let (agent_read, agent_write) = tokio::io::split(agent_pipe);
        let pages = Arc::new(Mutex::new(VecDeque::from(pages)));
        let cursors = Arc::new(Mutex::new(Vec::new()));
        let received_cursors = cursors.clone();
        let builder =
            acp::Agent
                .builder()
                .name("read-only-listing-agent")
                .on_receive_request(
                    |request: InitializeRequest,
                     responder: acp::Responder<InitializeResponse>,
                     _cx| async move {
                        let request = serde_json::to_value(request).unwrap();
                        let capabilities = &request["clientCapabilities"];
                        assert_ne!(capabilities["terminal"], true);
                        assert_ne!(capabilities["fs"]["readTextFile"], true);
                        assert_ne!(capabilities["fs"]["writeTextFile"], true);
                        responder.respond(InitializeResponse::new(acp::schema::ProtocolVersion::V1))
                    },
                    acp::on_receive_request!(),
                )
                .on_receive_request(
                    move |request: ListSessionsRequest,
                          responder: acp::Responder<ListSessionsResponse>,
                          _cx| {
                        received_cursors.lock().unwrap().push(request.cursor);
                        let response = pages.lock().unwrap().pop_front();
                        async move {
                            match response {
                                Some(Ok(response)) => responder.respond(response),
                                Some(Err(error)) => responder.respond_with_error(error),
                                None => std::future::pending().await,
                            }
                        }
                    },
                    acp::on_receive_request!(),
                );
        let (agent, agent_io) = conn::spawn_agent(
            builder,
            conn::byte_streams(agent_write.compat_write(), agent_read.compat()),
        );
        let (client, client_io) = conn::spawn_client(
            readonly_client_builder(),
            conn::byte_streams(client_write.compat_write(), client_read.compat()),
        );
        MockListingAgent {
            client,
            agent,
            cursors,
            _client_io: AbortOnDropHandle::new(tokio::task::spawn_local(async move {
                let _ = client_io.await;
            })),
            _agent_io: AbortOnDropHandle::new(tokio::task::spawn_local(async move {
                let _ = agent_io.await;
            })),
        }
    }

    fn page(ids: &[&str], next: Option<&str>) -> ListSessionsResponse {
        let mut response = ListSessionsResponse::new(
            ids.iter()
                .map(|id| {
                    SessionInfo::new(SessionId::new((*id).to_string()), PathBuf::from("/repo"))
                })
                .collect(),
        );
        response.next_cursor = next.map(str::to_string);
        response
    }

    #[test]
    fn malformed_list_pages_are_errors_not_empty_or_truncated_successes() {
        for json in [
            "{}",
            r#"{"sessions":null}"#,
            r#"{"sessions":"invalid"}"#,
            r#"{"sessions":[{"sessionId":"missing-cwd"}]}"#,
            r#"{"sessions":[{"cwd":"/repo"}]}"#,
            r#"{"sessions":[],"nextCursor":123}"#,
        ] {
            assert!(
                serde_json::from_str::<SessionListPage>(json).is_err(),
                "{json}"
            );
        }
        assert!(serde_json::from_str::<SessionListPage>(r#"{"sessions":[]}"#).is_ok());
    }

    #[tokio::test]
    async fn readonly_list_initializes_without_tools_and_fetches_every_page() {
        tokio::task::LocalSet::new()
            .run_until(tokio::time::timeout(Duration::from_secs(5), async {
                let mock = mock_agent(vec![
                    Ok(page(&["first"], Some("cursor-1"))),
                    Ok(page(&[], Some("cursor-2"))),
                    Ok(page(&["last"], None)),
                ]);
                mock.client.initialize(initialize_request()).await.unwrap();
                let rows = fetch_all_pages(&mock.client, "mock", Duration::from_secs(5))
                    .await
                    .unwrap();
                assert_eq!(
                    rows.iter()
                        .map(|row| row.session_id.to_string())
                        .collect::<Vec<_>>(),
                    ["first", "last"]
                );
                assert_eq!(
                    *mock.cursors.lock().unwrap(),
                    [None, Some("cursor-1".into()), Some("cursor-2".into())]
                );

                let terminal = serde_json::from_value(serde_json::json!({
                    "sessionId": "no-session", "command": "never-run"
                }))
                .unwrap();
                assert!(mock.agent.create_terminal(terminal).await.is_err());
                let read = serde_json::from_value(serde_json::json!({
                    "sessionId": "no-session", "path": "C:\\never-read"
                }))
                .unwrap();
                assert!(mock.agent.read_text_file(read).await.is_err());
                let write = serde_json::from_value(serde_json::json!({
                    "sessionId": "no-session", "path": "C:\\never-write", "content": "forbidden"
                }))
                .unwrap();
                assert!(mock.agent.write_text_file(write).await.is_err());
            }))
            .await
            .expect("read-only client must reject tools without waiting for session registration");
    }

    #[tokio::test]
    async fn readonly_list_rejects_repeated_and_cyclic_cursors_without_partial_success() {
        tokio::task::LocalSet::new()
            .run_until(async {
                for next in [vec!["a", "a"], vec!["a", "b", "a"]] {
                    let pages = next
                        .iter()
                        .map(|cursor| Ok(page(&["row"], Some(cursor))))
                        .collect();
                    let mock = mock_agent(pages);
                    let error = fetch_all_pages(&mock.client, "mock", Duration::from_secs(5))
                        .await
                        .unwrap_err();
                    assert!(error.contains("repeated pagination cursor"), "{error}");
                    assert_eq!(mock.cursors.lock().unwrap().len(), next.len());
                }
            })
            .await;
    }

    #[tokio::test]
    async fn readonly_list_surfaces_unsupported_and_later_page_errors() {
        tokio::task::LocalSet::new()
            .run_until(async {
                for mut pages in [vec![], vec![Ok(page(&["partial"], Some("next")))]] {
                    pages.push(Err(acp::Error::method_not_found()));
                    let mock = mock_agent(pages);
                    let error = fetch_all_pages(&mock.client, "mock", Duration::from_secs(5))
                        .await
                        .unwrap_err();
                    assert!(error.contains("session/list failed"), "{error}");
                }
            })
            .await;
    }

    #[tokio::test]
    async fn readonly_list_times_out_instead_of_returning_partial_rows() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mock = mock_agent(vec![Ok(page(&["partial"], Some("never-answers")))]);
                let error = fetch_all_pages(&mock.client, "mock", Duration::from_millis(10))
                    .await
                    .unwrap_err();
                assert!(error.contains("timed out"), "{error}");
            })
            .await;
    }

    #[tokio::test]
    async fn readonly_list_enforces_page_and_item_limits() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let pages = (0..MAX_LIST_PAGES)
                    .map(|index| Ok(page(&[], Some(&index.to_string()))))
                    .collect();
                let mock = mock_agent(pages);
                let error = fetch_all_pages(&mock.client, "mock", Duration::from_secs(5))
                    .await
                    .unwrap_err();
                assert!(error.contains("pages"), "{error}");
                assert_eq!(mock.cursors.lock().unwrap().len(), MAX_LIST_PAGES);

                let mut response = page(&["row"], Some("one-too-many"));
                response.sessions = vec![response.sessions[0].clone(); MAX_LIST_ITEMS];
                let mock = mock_agent(vec![Ok(response), Ok(page(&["extra"], None))]);
                let error = fetch_all_pages(&mock.client, "mock", Duration::from_secs(5))
                    .await
                    .unwrap_err();
                assert!(error.contains("items"), "{error}");
            })
            .await;
    }

    #[tokio::test]
    async fn readonly_list_accepts_empty_success_and_exact_limits() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mock = mock_agent(vec![Ok(page(&[], None))]);
                assert!(
                    fetch_all_pages(&mock.client, "mock", Duration::from_secs(5))
                        .await
                        .unwrap()
                        .is_empty()
                );
                let mut response = page(&["row"], None);
                response.sessions = vec![response.sessions[0].clone(); MAX_LIST_ITEMS];
                let mock = mock_agent(vec![Ok(response)]);
                assert_eq!(
                    fetch_all_pages(&mock.client, "mock", Duration::from_secs(5))
                        .await
                        .unwrap()
                        .len(),
                    MAX_LIST_ITEMS
                );
            })
            .await;
    }
}
