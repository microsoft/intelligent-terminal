//! Form A of the mock-ACP-agent plan (see `doc/specs/mock-acp-agent.md`):
//! an in-process, deterministic `acp::Agent` wired to WTA's real
//! `ClientSideConnection` over an in-memory `tokio::io::duplex`, so a whole
//! agent-pane interaction can be exercised in `cargo test` with no real WT,
//! no network, and no LLM.
//!
//! The wiring mirrors `agent-client-protocol`'s own
//! `rpc_tests::create_connection_pair` but substitutes the real [`WtaClient`]
//! for the crate's test client, so the ACP serialization round-trip and the
//! real `WtaClient` handling are both under test.
//!
//! The constructors are `pub(crate)` so app-module scenarios can borrow the
//! harness and assert on real `App` state (see the spec, "option 2").

use super::{ClientState, PromptTimingState, WtaClient};
use super::{
    dispatch_master_ext_request, dispatch_prompt, MasterExtRequest, PromptSubmission, TemplateMemo,
};
use crate::app::AppEvent;
use crate::shell::ShellManager;
use agent_client_protocol as acp;
use agent_client_protocol::{Agent as _, Client as _};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot, OnceCell};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// What the mock does when it receives a `prompt`.
#[derive(Clone, Copy)]
enum MockBehavior {
    /// Stream a deterministic `MOCK_OK:<echo>` reply, then end the turn.
    Reply,
    /// Request permission (allow-once / reject-once) and record the outcome the
    /// client sent back, then end the turn.
    AskPermission,
    /// Stream a `ToolCall` notification (a proposed command), then end the turn.
    ProposeToolCall,
    /// Stream a `ToolCall` then a `ToolCallUpdate(Completed)`, then end the turn.
    ToolThenComplete,
    /// Stream a `Plan` notification with two entries, then end the turn.
    ProposePlan,
    /// Stream the reply in two `AgentMessageChunk`s (`MOCK_` + `OK`), then end
    /// the turn — exercises streaming coalescing.
    StreamTwoChunks,
}

/// Deterministic ACP agent. Implements only what the scenarios need; the rest
/// of `acp::Agent` keeps its trait defaults.
///
/// `conn` is set after the connection is built (chicken-and-egg: the agent is
/// moved into `AgentSideConnection::new`, so it gets its own connection handle
/// via a `OnceCell` populated immediately afterwards). `prompt` uses it to
/// stream replies / request permission, exactly like a real agent does.
struct MockAgent {
    conn: Arc<OnceCell<Arc<acp::AgentSideConnection>>>,
    behavior: MockBehavior,
    /// Side-channel: every prompt's user text.
    seen_prompts: Arc<Mutex<Vec<String>>>,
    /// Side-channel: the permission option id the client selected (or
    /// "cancelled"), for `AskPermission` runs.
    permission_outcome: Arc<Mutex<Option<String>>>,
    /// When set, `new_session` returns an error instead of a session id —
    /// simulates the agent/transport dropping during session establishment.
    fail_new_session: Arc<AtomicBool>,
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
        if self.fail_new_session.load(Ordering::SeqCst) {
            return Err(acp::Error::internal_error().data("mock new_session failure".to_string()));
        }
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
        let sid = args.session_id.clone();

        // Spawn the turn's work on the LocalSet so the prompt response returns
        // promptly and the streamed notification / permission round-trip flushes
        // concurrently (a real agent works during the turn; decoupling here also
        // avoids any in-flight-request reentrancy).
        if let Some(conn) = self.conn.get() {
            let conn = conn.clone();
            match self.behavior {
                MockBehavior::Reply => {
                    let reply = format!("MOCK_OK:{text}");
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
                MockBehavior::AskPermission => {
                    let outcome_slot = self.permission_outcome.clone();
                    tokio::task::spawn_local(async move {
                        let req = acp::RequestPermissionRequest::new(
                            sid,
                            acp::ToolCallUpdate::new(
                                acp::ToolCallId::new("mock-tool-1"),
                                acp::ToolCallUpdateFields::new().title("Run: echo hi"),
                            ),
                            // Allow first so a default-selected (index 0) Enter
                            // means "allow"; reject is index 1.
                            vec![
                                acp::PermissionOption::new(
                                    acp::PermissionOptionId::new("allow-once"),
                                    "Allow once",
                                    acp::PermissionOptionKind::AllowOnce,
                                ),
                                acp::PermissionOption::new(
                                    acp::PermissionOptionId::new("reject-once"),
                                    "Reject",
                                    acp::PermissionOptionKind::RejectOnce,
                                ),
                            ],
                        );
                        if let Ok(resp) = conn.request_permission(req).await {
                            let chosen = match resp.outcome {
                                acp::RequestPermissionOutcome::Selected(sel) => {
                                    sel.option_id.to_string()
                                }
                                acp::RequestPermissionOutcome::Cancelled => "cancelled".to_string(),
                                _ => "unknown".to_string(),
                            };
                            *outcome_slot.lock().unwrap() = Some(chosen);
                        }
                    });
                }
                MockBehavior::ProposeToolCall => {
                    tokio::task::spawn_local(async move {
                        let _ = conn
                            .session_notification(acp::SessionNotification::new(
                                sid,
                                acp::SessionUpdate::ToolCall(acp::ToolCall::new(
                                    acp::ToolCallId::new("mock-tool-1"),
                                    "Run: echo hi",
                                )),
                            ))
                            .await;
                    });
                }
                MockBehavior::ToolThenComplete => {
                    tokio::task::spawn_local(async move {
                        let _ = conn
                            .session_notification(acp::SessionNotification::new(
                                sid.clone(),
                                acp::SessionUpdate::ToolCall(acp::ToolCall::new(
                                    acp::ToolCallId::new("mock-tool-1"),
                                    "Run: echo hi",
                                )),
                            ))
                            .await;
                        let _ = conn
                            .session_notification(acp::SessionNotification::new(
                                sid,
                                acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
                                    acp::ToolCallId::new("mock-tool-1"),
                                    acp::ToolCallUpdateFields::new()
                                        .status(acp::ToolCallStatus::Completed),
                                )),
                            ))
                            .await;
                    });
                }
                MockBehavior::ProposePlan => {
                    tokio::task::spawn_local(async move {
                        let _ = conn
                            .session_notification(acp::SessionNotification::new(
                                sid,
                                acp::SessionUpdate::Plan(acp::Plan::new(vec![
                                    acp::PlanEntry::new(
                                        "Step one",
                                        acp::PlanEntryPriority::Medium,
                                        acp::PlanEntryStatus::InProgress,
                                    ),
                                    acp::PlanEntry::new(
                                        "Step two",
                                        acp::PlanEntryPriority::Low,
                                        acp::PlanEntryStatus::Pending,
                                    ),
                                ])),
                            ))
                            .await;
                    });
                }
                MockBehavior::StreamTwoChunks => {
                    tokio::task::spawn_local(async move {
                        for part in ["MOCK_", "OK"] {
                            let _ = conn
                                .session_notification(acp::SessionNotification::new(
                                    sid.clone(),
                                    acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(
                                        part.into(),
                                    )),
                                ))
                                .await;
                        }
                    });
                }
            }
        }

        Ok(acp::PromptResponse::new(acp::StopReason::EndTurn))
    }

    async fn cancel(&self, _args: acp::CancelNotification) -> acp::Result<()> {
        Ok(())
    }
}

/// Wire WTA's real `WtaClient` to a `MockAgent` over an in-memory duplex, spawn
/// both I/O loops on the current `LocalSet`, and return the client connection,
/// the `AppEvent` receiver fed by `WtaClient`, and both side-channels.
///
/// Must be called inside a `tokio::task::LocalSet` (the connections spawn their
/// I/O via `spawn_local`).
fn connect_with(
    behavior: MockBehavior,
) -> (
    acp::ClientSideConnection,
    mpsc::UnboundedReceiver<AppEvent>,
    Arc<Mutex<Vec<String>>>,
    Arc<Mutex<Option<String>>>,
) {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let state = Arc::new(ClientState {
        event_tx,
        shell_mgr: Arc::new(ShellManager::new()),
        prompt_timing: Arc::new(PromptTimingState::default()),
    });
    let wta = WtaClient { state };

    let seen_prompts = Arc::new(Mutex::new(Vec::new()));
    let permission_outcome = Arc::new(Mutex::new(None));
    let conn_cell: Arc<OnceCell<Arc<acp::AgentSideConnection>>> = Arc::new(OnceCell::new());
    let mock = MockAgent {
        conn: conn_cell.clone(),
        behavior,
        seen_prompts: seen_prompts.clone(),
        permission_outcome: permission_outcome.clone(),
        fail_new_session: Arc::new(AtomicBool::new(false)),
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

    // Hand the mock its own connection so `prompt` can stream / request permission.
    assert!(
        conn_cell.set(Arc::new(agent_conn)).is_ok(),
        "mock agent connection cell must be set exactly once"
    );

    tokio::task::spawn_local(async move {
        let _ = client_io.await;
    });
    tokio::task::spawn_local(async move {
        let _ = agent_io.await;
    });

    (client_conn, event_rx, seen_prompts, permission_outcome)
}

/// Happy-path harness: the mock streams a deterministic reply on each prompt.
/// Returns the client connection, the `AppEvent` receiver, and the seen-prompts
/// side-channel.
pub(crate) fn connect_mock_agent() -> (
    acp::ClientSideConnection,
    mpsc::UnboundedReceiver<AppEvent>,
    Arc<Mutex<Vec<String>>>,
) {
    let (conn, event_rx, seen_prompts, _outcome) = connect_with(MockBehavior::Reply);
    (conn, event_rx, seen_prompts)
}

/// Permission harness: the mock requests permission (allow-once / reject-once)
/// on each prompt and records the selected outcome. Returns the client
/// connection, the `AppEvent` receiver, and the permission-outcome side-channel.
pub(crate) fn connect_mock_agent_asking_permission() -> (
    acp::ClientSideConnection,
    mpsc::UnboundedReceiver<AppEvent>,
    Arc<Mutex<Option<String>>>,
) {
    let (conn, event_rx, _seen, permission_outcome) = connect_with(MockBehavior::AskPermission);
    (conn, event_rx, permission_outcome)
}

/// Tool-call harness: the mock streams a `ToolCall` (a proposed command) on each
/// prompt. Returns the client connection and the `AppEvent` receiver.
pub(crate) fn connect_mock_agent_proposing_tool() -> (
    acp::ClientSideConnection,
    mpsc::UnboundedReceiver<AppEvent>,
) {
    let (conn, event_rx, _seen, _outcome) = connect_with(MockBehavior::ProposeToolCall);
    (conn, event_rx)
}

/// Tool-call lifecycle harness: streams a `ToolCall` then a
/// `ToolCallUpdate(Completed)`.
pub(crate) fn connect_mock_agent_completing_tool() -> (
    acp::ClientSideConnection,
    mpsc::UnboundedReceiver<AppEvent>,
) {
    let (conn, event_rx, _seen, _outcome) = connect_with(MockBehavior::ToolThenComplete);
    (conn, event_rx)
}

/// Plan harness: the mock streams a `Plan` with two entries.
pub(crate) fn connect_mock_agent_proposing_plan() -> (
    acp::ClientSideConnection,
    mpsc::UnboundedReceiver<AppEvent>,
) {
    let (conn, event_rx, _seen, _outcome) = connect_with(MockBehavior::ProposePlan);
    (conn, event_rx)
}

/// Streaming harness: the mock streams the reply in two chunks.
pub(crate) fn connect_mock_agent_streaming_two_chunks() -> (
    acp::ClientSideConnection,
    mpsc::UnboundedReceiver<AppEvent>,
) {
    let (conn, event_rx, _seen, _outcome) = connect_with(MockBehavior::StreamTwoChunks);
    (conn, event_rx)
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

// ─── A2.1: dispatch_* orchestration harness + tests ─────────────────────────
//
// The tests above act AS the orchestrator (they call `client_conn.prompt`
// directly). The tests below instead drive WTA's real `dispatch_prompt`
// orchestration — the per-prompt arm of the `run_acp_client_over_pipe` /
// `run_inner` select loops — against the same mock agent, so the dispatcher
// ("driver") logic
// (single-flight gating, lazy session create, prompt assembly, response
// routing) is itself under test, not just `WtaClient`'s ACP↔AppEvent
// translation.

/// Everything a `dispatch_prompt` call needs that the harness owns: the
/// client connection (as the `Arc` the dispatcher takes), a *shared* event
/// channel (so chunks emitted by `WtaClient` and lifecycle events emitted by
/// the dispatcher land on one stream), and the `shell_mgr` / `prompt_timing`
/// the dispatcher threads into prompt assembly. `seen_prompts` is the
/// agent-side record of every assembled prompt that reached the wire.
pub(crate) struct DispatchHarness {
    pub conn: Arc<acp::ClientSideConnection>,
    pub event_tx: mpsc::UnboundedSender<AppEvent>,
    pub event_rx: mpsc::UnboundedReceiver<AppEvent>,
    pub shell_mgr: Arc<ShellManager>,
    pub prompt_timing: Arc<PromptTimingState>,
    pub seen_prompts: Arc<Mutex<Vec<String>>>,
    /// Flip to `true` before dispatching to make the mock's `new_session`
    /// fail, exercising the dispatcher's session-establishment error path.
    pub fail_new_session: Arc<AtomicBool>,
}

/// Wire a real `WtaClient` to a `MockAgent` like [`connect_with`], but expose
/// the shared `event_tx` / `shell_mgr` / `prompt_timing` so the dispatcher can
/// be invoked with the same handles the production loop would.
fn connect_for_dispatch(behavior: MockBehavior) -> DispatchHarness {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let shell_mgr = Arc::new(ShellManager::new());
    let prompt_timing = Arc::new(PromptTimingState::default());
    let state = Arc::new(ClientState {
        event_tx: event_tx.clone(),
        shell_mgr: shell_mgr.clone(),
        prompt_timing: prompt_timing.clone(),
    });
    let wta = WtaClient { state };

    let seen_prompts = Arc::new(Mutex::new(Vec::new()));
    let permission_outcome = Arc::new(Mutex::new(None));
    let fail_new_session = Arc::new(AtomicBool::new(false));
    let conn_cell: Arc<OnceCell<Arc<acp::AgentSideConnection>>> = Arc::new(OnceCell::new());
    let mock = MockAgent {
        conn: conn_cell.clone(),
        behavior,
        seen_prompts: seen_prompts.clone(),
        permission_outcome,
        fail_new_session: fail_new_session.clone(),
    };

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
    assert!(
        conn_cell.set(Arc::new(agent_conn)).is_ok(),
        "mock agent connection cell must be set exactly once"
    );
    tokio::task::spawn_local(async move {
        let _ = client_io.await;
    });
    tokio::task::spawn_local(async move {
        let _ = agent_io.await;
    });

    DispatchHarness {
        conn: Arc::new(client_conn),
        event_tx,
        event_rx,
        shell_mgr,
        prompt_timing,
        seen_prompts,
        fail_new_session,
    }
}

/// Fresh, empty per-tab dispatcher state (session map, single-flight set,
/// cancel registry, template memo) for one `dispatch_prompt` invocation.
#[allow(clippy::type_complexity)]
fn fresh_dispatch_state() -> (
    Arc<tokio::sync::Mutex<HashMap<String, acp::SessionId>>>,
    Arc<std::sync::Mutex<HashSet<String>>>,
    Arc<std::sync::Mutex<HashMap<String, oneshot::Sender<()>>>>,
    TemplateMemo,
) {
    (
        Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        Arc::new(std::sync::Mutex::new(HashSet::new())),
        Arc::new(std::sync::Mutex::new(HashMap::new())),
        TemplateMemo::default(),
    )
}

fn test_prompt(id: u64, text: &str, is_autofix: bool) -> PromptSubmission {
    PromptSubmission {
        id,
        text: text.to_string(),
        pane_context: None,
        submitted_at_unix_s: 0.0,
        is_autofix,
    }
}

/// Single-flight: a prompt for a tab that already has a turn in flight must
/// emit `AgentBusy` and NOT start a second turn (no `conn.prompt`, the agent
/// never sees the text).
#[tokio::test]
async fn dispatch_prompt_busy_tab_emits_agent_busy_and_drops() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let h = connect_for_dispatch(MockBehavior::Reply);
            let (tab_to_session, in_flight, cancel_signals, memo) = fresh_dispatch_state();
            // A turn is already running for the default tab ("0").
            in_flight.lock().unwrap().insert("0".to_string());
            let mut event_rx = h.event_rx;

            dispatch_prompt(
                test_prompt(1, "hi", false),
                &h.conn,
                &tab_to_session,
                &memo,
                &in_flight,
                &cancel_signals,
                &h.event_tx,
                &h.shell_mgr,
                &h.prompt_timing,
                false, // wt_connected
                false, // is_agent_pane
            );

            match tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv()).await {
                Ok(Some(AppEvent::AgentBusy { tab_id })) => assert_eq!(tab_id, "0"),
                Ok(_) => panic!("expected AgentBusy, got a different event"),
                _ => panic!("expected AgentBusy, got nothing"),
            }
            // The in-flight set is unchanged (the busy prompt did not remove or
            // duplicate the owner), and the agent never received a prompt.
            assert_eq!(in_flight.lock().unwrap().len(), 1);
            assert!(
                h.seen_prompts.lock().unwrap().is_empty(),
                "a busy-dropped prompt must never reach the agent"
            );
        })
        .await;
}

/// Full round-trip through the dispatcher: a fresh tab lazily creates a
/// session, the assembled prompt reaches the agent, and the streamed reply is
/// surfaced as an `AgentMessageChunk`. Proves the prompt arm wires
/// new_session → prompt assembly → response routing end to end.
#[tokio::test]
async fn dispatch_prompt_round_trips_through_agent() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let h = connect_for_dispatch(MockBehavior::Reply);
            // Handshake so the lazy `new_session` inside the dispatcher succeeds.
            h.conn
                .initialize(acp::InitializeRequest::new(acp::ProtocolVersion::LATEST))
                .await
                .expect("initialize failed");

            let (tab_to_session, in_flight, cancel_signals, memo) = fresh_dispatch_state();
            let mut event_rx = h.event_rx;

            dispatch_prompt(
                test_prompt(1, "hello", false),
                &h.conn,
                &tab_to_session,
                &memo,
                &in_flight,
                &cancel_signals,
                &h.event_tx,
                &h.shell_mgr,
                &h.prompt_timing,
                false,
                false,
            );

            // Pump until the agent's streamed reply surfaces — implies lazy
            // new_session, prompt assembly + send, and response routing all ran.
            // The mock echoes the *assembled* prompt, which the dispatcher wraps
            // in the planner template, so we assert structure rather than exact
            // equality.
            let chunk = next_agent_chunk(&mut event_rx).await;
            assert!(
                chunk.starts_with("MOCK_OK:"),
                "reply must be the mock's echo of the assembled prompt"
            );
            assert!(
                chunk.contains("hello"),
                "the user's text must survive into the assembled prompt"
            );

            // The assembled prompt that reached the agent must contain the user
            // text (build_prompt_text wraps it with the planner template).
            let seen = h.seen_prompts.lock().unwrap().clone();
            assert_eq!(seen.len(), 1, "exactly one prompt reached the agent");
            assert!(
                seen[0].contains("hello"),
                "the agent must receive the user's text inside the assembled prompt"
            );
            assert!(
                seen[0].contains("Terminal Agent"),
                "a non-autofix prompt must carry the planner template"
            );

            // The session is cached before the prompt is sent, so it's already
            // present by the time the reply arrives.
            assert!(tab_to_session.lock().await.contains_key("0"));

            // in-flight is cleared at turn *completion* (AgentMessageEnd), which
            // lands after the first chunk — pump until then before asserting.
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    match event_rx.recv().await {
                        Some(AppEvent::AgentMessageEnd { .. }) => break,
                        Some(_) => continue,
                        None => panic!("event channel closed before turn end"),
                    }
                }
            })
            .await
            .expect("timed out waiting for turn end");
            assert!(
                in_flight.lock().unwrap().is_empty(),
                "single-flight slot must be released when the turn completes"
            );
        })
        .await;
}

/// Error path: if the lazy `new_session` fails (agent/transport dropped during
/// session establishment), the dispatcher must surface `AgentError`, release
/// the single-flight slot, and never reach the prompt — not hang the turn.
#[tokio::test]
async fn dispatch_prompt_new_session_failure_emits_error_and_releases_slot() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let h = connect_for_dispatch(MockBehavior::Reply);
            h.conn
                .initialize(acp::InitializeRequest::new(acp::ProtocolVersion::LATEST))
                .await
                .expect("initialize failed");
            // Make the mock reject session establishment.
            h.fail_new_session.store(true, Ordering::SeqCst);

            let (tab_to_session, in_flight, cancel_signals, memo) = fresh_dispatch_state();
            let mut event_rx = h.event_rx;

            dispatch_prompt(
                test_prompt(1, "hello", false),
                &h.conn,
                &tab_to_session,
                &memo,
                &in_flight,
                &cancel_signals,
                &h.event_tx,
                &h.shell_mgr,
                &h.prompt_timing,
                false,
                false,
            );

            match tokio::time::timeout(std::time::Duration::from_secs(5), event_rx.recv()).await {
                Ok(Some(AppEvent::AgentError { message, .. })) => {
                    assert!(
                        message.contains("new_session failed"),
                        "error must name the failed step; got {message:?}"
                    );
                }
                Ok(_) => panic!("expected AgentError"),
                _ => panic!("expected AgentError, got nothing"),
            }
            // The slot is released so a retry isn't permanently blocked, no
            // session was cached, and the agent never saw the prompt.
            assert!(
                in_flight.lock().unwrap().is_empty(),
                "single-flight slot must be released on new_session failure"
            );
            assert!(tab_to_session.lock().await.is_empty());
            assert!(h.seen_prompts.lock().unwrap().is_empty());
        })
        .await;
}

/// Template selection: an autofix prompt (`is_autofix=true`) must be assembled
/// with the *autofix* template ("A command failed. Diagnose…"), NOT the planner
/// persona. Picking the wrong template would make autofix behave like the
/// general planner and fail to diagnose the failure.
#[tokio::test]
async fn dispatch_prompt_autofix_uses_autofix_template() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let h = connect_for_dispatch(MockBehavior::Reply);
            h.conn
                .initialize(acp::InitializeRequest::new(acp::ProtocolVersion::LATEST))
                .await
                .expect("initialize failed");

            let (tab_to_session, in_flight, cancel_signals, memo) = fresh_dispatch_state();
            let mut event_rx = h.event_rx;

            dispatch_prompt(
                test_prompt(1, "fix the build", true), // is_autofix = true
                &h.conn,
                &tab_to_session,
                &memo,
                &in_flight,
                &cancel_signals,
                &h.event_tx,
                &h.shell_mgr,
                &h.prompt_timing,
                false,
                false,
            );

            let _ = next_agent_chunk(&mut event_rx).await; // wait for the round-trip
            let seen = h.seen_prompts.lock().unwrap().clone();
            assert_eq!(seen.len(), 1);
            assert!(
                seen[0].contains("A command failed. Diagnose the error"),
                "autofix prompt must carry the auto-fix template"
            );
            assert!(
                !seen[0].contains("You are Terminal Agent"),
                "autofix prompt must NOT carry the planner persona template"
            );
            assert!(
                seen[0].contains("fix the build"),
                "autofix prompt must still carry the user's text"
            );
        })
        .await;
}

/// `dispatch_master_ext_request(SessionsList)` must call `ext_method` and turn
/// the response into an `AgentsSnapshotLoaded` carrying the same `request_id`.
/// Against a mock that returns an empty/null ext response, the snapshot is an
/// empty session list (the graceful "view opened, nothing live yet" state).
#[tokio::test]
async fn dispatch_master_ext_sessions_list_loads_snapshot() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let h = connect_for_dispatch(MockBehavior::Reply);
            let tab_to_session = std::sync::Arc::new(tokio::sync::Mutex::new(HashMap::new()));
            let mut event_rx = h.event_rx;

            dispatch_master_ext_request(
                MasterExtRequest::SessionsList { request_id: 7 },
                &h.conn,
                &h.event_tx,
                &tab_to_session,
            );

            match tokio::time::timeout(std::time::Duration::from_secs(5), event_rx.recv()).await {
                Ok(Some(AppEvent::AgentsSnapshotLoaded {
                    request_id,
                    sessions,
                })) => {
                    assert_eq!(request_id, 7, "request_id must round-trip");
                    assert!(sessions.is_empty(), "null ext response -> empty snapshot");
                }
                Ok(_) => panic!("expected AgentsSnapshotLoaded"),
                _ => panic!("expected AgentsSnapshotLoaded, got nothing"),
            }
        })
        .await;
}

/// `dispatch_master_ext_request(SessionFocus)` always emits
/// `MasterMutationCompleted` with the request_id once the ext-method call
/// returns, so the App can clear its pending-mutation state.
#[tokio::test]
async fn dispatch_master_ext_session_focus_completes() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let h = connect_for_dispatch(MockBehavior::Reply);
            let tab_to_session = std::sync::Arc::new(tokio::sync::Mutex::new(HashMap::new()));
            let mut event_rx = h.event_rx;

            dispatch_master_ext_request(
                MasterExtRequest::SessionFocus {
                    request_id: 9,
                    sid: acp::SessionId::new("sess-focus"),
                },
                &h.conn,
                &h.event_tx,
                &tab_to_session,
            );

            match tokio::time::timeout(std::time::Duration::from_secs(5), event_rx.recv()).await {
                Ok(Some(AppEvent::MasterMutationCompleted { request_id })) => {
                    assert_eq!(request_id, 9)
                }
                Ok(_) => panic!("expected MasterMutationCompleted"),
                _ => panic!("expected MasterMutationCompleted, got nothing"),
            }
        })
        .await;
}


