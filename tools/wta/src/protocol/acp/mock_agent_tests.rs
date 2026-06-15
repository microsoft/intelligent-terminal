//! Form A of the mock-ACP-agent plan (see `doc/specs/mock-acp-agent.md`):
//! an in-process, deterministic `acp::Agent` wired to WTA's real
//! `ClientSideConnection` over an in-memory `tokio::io::duplex`, so a whole
//! agent-pane interaction can be exercised in `cargo test` with no real WT,
//! no network, and no LLM.
//!
//! This is the harness + first scenario (happy-path chat round-trip). The
//! wiring mirrors `agent-client-protocol`'s own `rpc_tests::create_connection_pair`
//! but substitutes the real [`WtaClient`] for the crate's test client, so the
//! ACP serialization round-trip and the real `WtaClient` notification handling
//! are both under test.

use super::{ClientState, PromptTimingState, WtaClient};
use crate::app::AppEvent;
use crate::shell::ShellManager;
use agent_client_protocol as acp;
use agent_client_protocol::{Agent as _, Client as _};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, OnceCell};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// Deterministic ACP agent. Implements only what the scenarios need; the rest
/// of `acp::Agent` keeps its trait defaults.
///
/// `conn` is set after the connection is built (chicken-and-egg: the agent is
/// moved into `AgentSideConnection::new`, so it gets its own connection handle
/// via a `OnceCell` populated immediately afterwards). `prompt` uses it to
/// stream the reply, exactly like a real agent does.
struct MockAgent {
    conn: Arc<OnceCell<Arc<acp::AgentSideConnection>>>,
    /// Side-channel: every prompt's user text, for the test to assert that WTA
    /// actually put the right thing on the wire.
    seen_prompts: Arc<Mutex<Vec<String>>>,
}

fn first_text(blocks: &[acp::ContentBlock]) -> String {
    blocks
        .iter()
        .find_map(|b| match b {
            acp::ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

#[async_trait::async_trait(?Send)]
impl acp::Agent for MockAgent {
    async fn initialize(
        &self,
        args: acp::InitializeRequest,
    ) -> acp::Result<acp::InitializeResponse> {
        Ok(acp::InitializeResponse::new(args.protocol_version)
            .agent_info(acp::Implementation::new("mock-acp-agent", "0.0.0").title("Mock ACP Agent")))
    }

    async fn new_session(
        &self,
        _args: acp::NewSessionRequest,
    ) -> acp::Result<acp::NewSessionResponse> {
        Ok(acp::NewSessionResponse::new(acp::SessionId::new("mock-session-1")))
    }

    async fn authenticate(
        &self,
        _args: acp::AuthenticateRequest,
    ) -> acp::Result<acp::AuthenticateResponse> {
        Ok(acp::AuthenticateResponse::default())
    }

    async fn prompt(&self, args: acp::PromptRequest) -> acp::Result<acp::PromptResponse> {
        let text = first_text(&args.prompt);
        self.seen_prompts.lock().unwrap().push(text.clone());

        // Stream a deterministic reply, then end the turn. Spawned on the
        // LocalSet so the prompt response returns promptly and the streamed
        // notification flushes concurrently (a real agent streams during the
        // turn; decoupling here also avoids any in-flight-request reentrancy).
        let reply = format!("MOCK_OK:{text}");
        let sid = args.session_id.clone();
        if let Some(conn) = self.conn.get() {
            let conn = conn.clone();
            tokio::task::spawn_local(async move {
                let _ = conn
                    .session_notification(acp::SessionNotification::new(
                        sid,
                        acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(
                            reply.as_str().into(),
                        )),
                    ))
                    .await;
            });
        }

        Ok(acp::PromptResponse::new(acp::StopReason::EndTurn))
    }

    async fn cancel(&self, _args: acp::CancelNotification) -> acp::Result<()> {
        Ok(())
    }
}

/// Wire WTA's real `WtaClient` to a `MockAgent` over an in-memory duplex, spawn
/// both I/O loops on the current `LocalSet`, and return:
/// - the client-side connection (call `initialize` / `new_session` / `prompt`),
/// - the `AppEvent` receiver fed by `WtaClient`,
/// - the mock's seen-prompts side-channel.
///
/// `pub(crate)` so app-module scenarios can borrow it and assert on real `App`
/// state (the harness must live here to build the private `WtaClient`).
///
/// Must be called inside a `tokio::task::LocalSet` (the connections spawn their
/// I/O via `spawn_local`).
pub(crate) fn connect_mock_agent() -> (
    acp::ClientSideConnection,
    mpsc::UnboundedReceiver<AppEvent>,
    Arc<Mutex<Vec<String>>>,
) {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let state = Arc::new(ClientState {
        event_tx,
        shell_mgr: Arc::new(ShellManager::new()),
        prompt_timing: Arc::new(PromptTimingState::default()),
    });
    let wta = WtaClient { state };

    let seen_prompts = Arc::new(Mutex::new(Vec::new()));
    let conn_cell: Arc<OnceCell<Arc<acp::AgentSideConnection>>> = Arc::new(OnceCell::new());
    let mock = MockAgent {
        conn: conn_cell.clone(),
        seen_prompts: seen_prompts.clone(),
    };

    // Bidirectional in-memory pipe. Each half is split into read/write and
    // adapted from tokio to futures I/O (same shape as the production pipe path
    // in `run_acp_client_over_pipe`).
    let (wta_io, mock_io) = tokio::io::duplex(64 * 1024);
    let (wta_r, wta_w) = tokio::io::split(wta_io);
    let (mock_r, mock_w) = tokio::io::split(mock_io);

    let (client_conn, client_io) = acp::ClientSideConnection::new(
        wta,
        wta_w.compat_write(),
        wta_r.compat(),
        |fut| {
            tokio::task::spawn_local(fut);
        },
    );

    let (agent_conn, agent_io) = acp::AgentSideConnection::new(
        mock,
        mock_w.compat_write(),
        mock_r.compat(),
        |fut| {
            tokio::task::spawn_local(fut);
        },
    );

    // Hand the mock its own connection so `prompt` can stream replies.
    let _ = conn_cell.set(Arc::new(agent_conn));

    tokio::task::spawn_local(async move {
        let _ = client_io.await;
    });
    tokio::task::spawn_local(async move {
        let _ = agent_io.await;
    });

    (client_conn, event_rx, seen_prompts)
}

/// Drain `event_rx` until the first `AgentMessageChunk`, with a timeout so a
/// wiring bug fails fast instead of hanging the suite.
async fn next_agent_chunk(event_rx: &mut mpsc::UnboundedReceiver<AppEvent>) -> String {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match event_rx.recv().await {
                Some(AppEvent::AgentMessageChunk { text, .. }) => break text,
                Some(_) => continue,
                None => panic!("event channel closed before an agent message chunk arrived"),
            }
        }
    })
    .await
    .expect("timed out waiting for an agent message chunk")
}

#[tokio::test]
async fn happy_path_chat_round_trip_surfaces_mock_reply() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (client_conn, mut event_rx, seen_prompts) = connect_mock_agent();

            client_conn
                .initialize(acp::InitializeRequest::new(acp::ProtocolVersion::LATEST))
                .await
                .expect("initialize failed");
            let session = client_conn
                .new_session(acp::NewSessionRequest::new("/test"))
                .await
                .expect("new_session failed");
            client_conn
                .prompt(acp::PromptRequest::new(
                    session.session_id.clone(),
                    vec!["hello".into()],
                ))
                .await
                .expect("prompt failed");

            // WTA must surface the mock's streamed reply as an AgentMessageChunk.
            let text = next_agent_chunk(&mut event_rx).await;
            assert_eq!(text, "MOCK_OK:hello");

            // And the prompt text must have reached the agent over the wire.
            assert_eq!(
                seen_prompts.lock().unwrap().as_slice(),
                &["hello".to_string()],
                "mock must have received the prompt text on the ACP wire"
            );
        })
        .await;
}
