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
    dispatch_cancel, dispatch_drop_session, dispatch_load_session, dispatch_master_ext_request,
    dispatch_new_session, dispatch_prompt, dispatch_rename_session, drain_cancel_signals,
    CancelRequest, DropSessionRequest, LoadSessionForTab, MasterExtRequest, NewSessionForTab,
    PromptSubmission, RenameSessionRequest, TemplateMemo,
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
    /// When set, `load_session` returns an error instead of a response —
    /// simulates the agent not recognizing the session id / `session/load`
    /// being unsupported.
    fail_load_session: Arc<AtomicBool>,
    /// When set, `load_session` sleeps long enough that a short injected
    /// dispatch timeout elapses first — exercises the timeout path.
    slow_load: Arc<AtomicBool>,
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

    async fn load_session(
        &self,
        _args: acp::LoadSessionRequest,
    ) -> acp::Result<acp::LoadSessionResponse> {
        if self.slow_load.load(Ordering::SeqCst) {
            // Outlast any short injected dispatch timeout so the
            // dispatcher takes its `Err(_)` (timeout) branch.
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
        if self.fail_load_session.load(Ordering::SeqCst) {
            return Err(acp::Error::internal_error().data("mock load_session failure".to_string()));
        }
        Ok(acp::LoadSessionResponse::new())
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
        fail_load_session: Arc::new(AtomicBool::new(false)),
        slow_load: Arc::new(AtomicBool::new(false)),
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
    /// Flip to `true` before dispatching to make the mock's `load_session`
    /// return an error, exercising the resume-failure path.
    pub fail_load_session: Arc<AtomicBool>,
    /// Flip to `true` before dispatching to make the mock's `load_session`
    /// sleep past a short injected timeout, exercising the resume-timeout path.
    pub slow_load: Arc<AtomicBool>,
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
    let fail_load_session = Arc::new(AtomicBool::new(false));
    let slow_load = Arc::new(AtomicBool::new(false));
    let conn_cell: Arc<OnceCell<Arc<acp::AgentSideConnection>>> = Arc::new(OnceCell::new());
    let mock = MockAgent {
        conn: conn_cell.clone(),
        behavior,
        seen_prompts: seen_prompts.clone(),
        permission_outcome,
        fail_new_session: fail_new_session.clone(),
        fail_load_session: fail_load_session.clone(),
        slow_load: slow_load.clone(),
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
        fail_load_session,
        slow_load,
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

/// `dispatch_rename_session` must rekey an existing tab binding from the old
/// tab id to the new one (cross-window drag), preserving the SessionId, and
/// be a no-op when the old tab id is absent.
#[tokio::test]
async fn dispatch_rename_session_rekeys_existing_and_noops_on_missing() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let tab_to_session = std::sync::Arc::new(tokio::sync::Mutex::new(HashMap::new()));
            let sid = acp::SessionId::new("sess-rekey");
            tab_to_session
                .lock()
                .await
                .insert("old-tab".to_string(), sid.clone());

            // Rekey old-tab -> new-tab.
            dispatch_rename_session(
                RenameSessionRequest {
                    old_tab_id: "old-tab".to_string(),
                    new_tab_id: "new-tab".to_string(),
                },
                &tab_to_session,
            );

            // The rekey runs on a spawned task; wait for it to land.
            let mut rekeyed = false;
            for _ in 0..200 {
                {
                    let g = tab_to_session.lock().await;
                    if !g.contains_key("old-tab") && g.get("new-tab") == Some(&sid) {
                        rekeyed = true;
                        break;
                    }
                }
                tokio::task::yield_now().await;
            }
            assert!(rekeyed, "old-tab must be rekeyed to new-tab with same SessionId");

            // No-op: renaming a ghost tab leaves the map untouched.
            dispatch_rename_session(
                RenameSessionRequest {
                    old_tab_id: "ghost".to_string(),
                    new_tab_id: "phantom".to_string(),
                },
                &tab_to_session,
            );
            // Give the spawned task a chance to (not) mutate anything.
            for _ in 0..10 {
                tokio::task::yield_now().await;
            }
            let g = tab_to_session.lock().await;
            assert!(!g.contains_key("phantom"), "missing old id must be a no-op");
            assert!(g.contains_key("new-tab"), "existing binding must survive the no-op");
        })
        .await;
}


/// `dispatch_cancel` must fire the local per-session cancel oneshot (so an
/// in-flight prompt task drops out of `conn.prompt().await`) and remove the
/// signal from the registry. The agent-side `session/cancel` is best-effort.
#[tokio::test]
async fn dispatch_cancel_fires_local_signal_and_removes_registry_entry() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let h = connect_for_dispatch(MockBehavior::Reply);
            let cancel_signals: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>> =
                Arc::new(Mutex::new(HashMap::new()));
            let (tx, rx) = oneshot::channel::<()>();
            cancel_signals
                .lock()
                .unwrap()
                .insert("sess-cancel".to_string(), tx);

            dispatch_cancel(
                CancelRequest {
                    session_id: "sess-cancel".to_string(),
                },
                &h.conn,
                &cancel_signals,
            );

            // The local oneshot is fired synchronously inside dispatch_cancel.
            assert!(rx.await.is_ok(), "local cancel signal must be fired");
            assert!(
                !cancel_signals.lock().unwrap().contains_key("sess-cancel"),
                "the fired signal must be removed from the registry"
            );

            // Cancelling an unknown session is a harmless no-op (no panic).
            dispatch_cancel(
                CancelRequest {
                    session_id: "ghost".to_string(),
                },
                &h.conn,
                &cancel_signals,
            );
            // Let the best-effort agent-notify subtask run.
            for _ in 0..10 {
                tokio::task::yield_now().await;
            }
        })
        .await;
}

/// `drain_cancel_signals` must fire every registered oneshot and empty the
/// registry — the `/restart` precondition that drops all in-flight prompt
/// tasks before the connection is torn down.
#[test]
fn drain_cancel_signals_fires_all_and_clears() {
    let cancel_signals: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let (tx_a, mut rx_a) = oneshot::channel::<()>();
    let (tx_b, mut rx_b) = oneshot::channel::<()>();
    {
        let mut g = cancel_signals.lock().unwrap();
        g.insert("a".to_string(), tx_a);
        g.insert("b".to_string(), tx_b);
    }

    drain_cancel_signals(&cancel_signals);

    assert!(rx_a.try_recv().is_ok(), "signal a must be fired");
    assert!(rx_b.try_recv().is_ok(), "signal b must be fired");
    assert!(
        cancel_signals.lock().unwrap().is_empty(),
        "registry must be emptied after drain"
    );
}

/// `dispatch_drop_session` must unbind the tab's session, fire its in-flight
/// cancel signal, and be a no-op for a tab that holds no session.
#[tokio::test]
async fn dispatch_drop_session_unbinds_and_fires_cancel_then_noops() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let h = connect_for_dispatch(MockBehavior::Reply);
            let tab_to_session = std::sync::Arc::new(tokio::sync::Mutex::new(HashMap::new()));
            let cancel_signals: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>> =
                Arc::new(Mutex::new(HashMap::new()));
            let memo = TemplateMemo::default();

            let sid = acp::SessionId::new("sess-drop");
            tab_to_session
                .lock()
                .await
                .insert("t1".to_string(), sid.clone());
            let (tx, rx) = oneshot::channel::<()>();
            cancel_signals
                .lock()
                .unwrap()
                .insert(sid.to_string(), tx);

            dispatch_drop_session(
                DropSessionRequest {
                    tab_id: "t1".to_string(),
                },
                &h.conn,
                &tab_to_session,
                &memo,
                &cancel_signals,
            );

            // The in-flight cancel oneshot fires when the spawned task runs.
            assert!(
                tokio::time::timeout(std::time::Duration::from_secs(5), rx)
                    .await
                    .is_ok(),
                "drop must fire the in-flight cancel signal"
            );
            assert!(
                !tab_to_session.lock().await.contains_key("t1"),
                "drop must unbind the tab's session"
            );
            assert!(
                !cancel_signals.lock().unwrap().contains_key("sess-drop"),
                "drop must remove the cancel signal from the registry"
            );

            // No-op: dropping an unbound tab leaves the map empty, no panic.
            dispatch_drop_session(
                DropSessionRequest {
                    tab_id: "unbound".to_string(),
                },
                &h.conn,
                &tab_to_session,
                &memo,
                &cancel_signals,
            );
            for _ in 0..10 {
                tokio::task::yield_now().await;
            }
            assert!(tab_to_session.lock().await.is_empty());
        })
        .await;
}

/// `dispatch_new_session` happy path: a fresh tab gets a session created and
/// bound, and a `SessionAttached` event carrying the new session id is emitted.
#[tokio::test]
async fn dispatch_new_session_creates_binds_and_emits_attached() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let h = connect_for_dispatch(MockBehavior::Reply);
            let tab_to_session = std::sync::Arc::new(tokio::sync::Mutex::new(HashMap::new()));
            let cancel_signals: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>> =
                Arc::new(Mutex::new(HashMap::new()));
            let memo = TemplateMemo::default();
            let mut event_rx = h.event_rx;

            dispatch_new_session(
                NewSessionForTab {
                    tab_id: "t1".to_string(),
                    cwd: None,
                },
                &h.conn,
                &tab_to_session,
                &memo,
                &cancel_signals,
                &h.event_tx,
                false,
                false,
                "Test",
            );

            match tokio::time::timeout(std::time::Duration::from_secs(5), event_rx.recv()).await {
                Ok(Some(AppEvent::SessionAttached {
                    tab_id, session_id, ..
                })) => {
                    assert_eq!(tab_id, "t1");
                    assert_eq!(session_id, "mock-session-1");
                }
                _ => panic!("expected SessionAttached"),
            }
            assert_eq!(
                tab_to_session.lock().await.get("t1").map(|s| s.to_string()),
                Some("mock-session-1".to_string()),
                "new session must be bound to the tab"
            );
        })
        .await;
}

/// `dispatch_new_session` failure path: when `new_session` errors, an
/// `AgentError` is surfaced and the tab is left unbound.
#[tokio::test]
async fn dispatch_new_session_failure_emits_agent_error_and_leaves_unbound() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let h = connect_for_dispatch(MockBehavior::Reply);
            h.fail_new_session.store(true, Ordering::SeqCst);
            let tab_to_session = std::sync::Arc::new(tokio::sync::Mutex::new(HashMap::new()));
            let cancel_signals: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>> =
                Arc::new(Mutex::new(HashMap::new()));
            let memo = TemplateMemo::default();
            let mut event_rx = h.event_rx;

            dispatch_new_session(
                NewSessionForTab {
                    tab_id: "t1".to_string(),
                    cwd: None,
                },
                &h.conn,
                &tab_to_session,
                &memo,
                &cancel_signals,
                &h.event_tx,
                false,
                false,
                "Test",
            );

            match tokio::time::timeout(std::time::Duration::from_secs(5), event_rx.recv()).await {
                Ok(Some(AppEvent::AgentError { message, .. })) => {
                    assert!(
                        message.contains("/new failed for tab t1"),
                        "unexpected error message: {message}"
                    );
                }
                _ => panic!("expected AgentError"),
            }
            assert!(
                tab_to_session.lock().await.is_empty(),
                "failed new_session must leave the tab unbound"
            );
        })
        .await;
}

/// `dispatch_new_session` replacing an existing session: the old session's
/// in-flight cancel signal fires, and the tab is rebound to the new session.
#[tokio::test]
async fn dispatch_new_session_replaces_old_and_fires_its_cancel() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let h = connect_for_dispatch(MockBehavior::Reply);
            let tab_to_session = std::sync::Arc::new(tokio::sync::Mutex::new(HashMap::new()));
            let cancel_signals: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>> =
                Arc::new(Mutex::new(HashMap::new()));
            let memo = TemplateMemo::default();
            let mut event_rx = h.event_rx;

            let old = acp::SessionId::new("old-sess");
            tab_to_session
                .lock()
                .await
                .insert("t1".to_string(), old.clone());
            let (tx_old, rx_old) = oneshot::channel::<()>();
            cancel_signals
                .lock()
                .unwrap()
                .insert(old.to_string(), tx_old);

            dispatch_new_session(
                NewSessionForTab {
                    tab_id: "t1".to_string(),
                    cwd: None,
                },
                &h.conn,
                &tab_to_session,
                &memo,
                &cancel_signals,
                &h.event_tx,
                false,
                false,
                "Test",
            );

            assert!(
                tokio::time::timeout(std::time::Duration::from_secs(5), rx_old)
                    .await
                    .is_ok(),
                "replacing a session must fire the old session's cancel signal"
            );
            match tokio::time::timeout(std::time::Duration::from_secs(5), event_rx.recv()).await {
                Ok(Some(AppEvent::SessionAttached { session_id, .. })) => {
                    assert_eq!(session_id, "mock-session-1");
                }
                _ => panic!("expected SessionAttached"),
            }
            assert_eq!(
                tab_to_session.lock().await.get("t1").map(|s| s.to_string()),
                Some("mock-session-1".to_string()),
                "tab must be rebound to the replacement session"
            );
        })
        .await;
}

/// `dispatch_load_session` happy path: resuming a historical session binds it
/// to the tab and emits `SessionAttached` followed by a `TabSystemMessage`
/// confirmation note.
#[tokio::test]
async fn dispatch_load_session_binds_and_emits_attached_then_note() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let h = connect_for_dispatch(MockBehavior::Reply);
            let tab_to_session = std::sync::Arc::new(tokio::sync::Mutex::new(HashMap::new()));
            let cancel_signals: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>> =
                Arc::new(Mutex::new(HashMap::new()));
            let mut event_rx = h.event_rx;

            dispatch_load_session(
                LoadSessionForTab {
                    tab_id: "t1".to_string(),
                    session_id: "hist-sess-7".to_string(),
                    cwd: None,
                },
                &h.conn,
                &tab_to_session,
                &cancel_signals,
                &h.event_tx,
                false,
                false,
                std::time::Duration::from_secs(5),
            );

            match tokio::time::timeout(std::time::Duration::from_secs(5), event_rx.recv()).await {
                Ok(Some(AppEvent::SessionAttached {
                    tab_id, session_id, ..
                })) => {
                    assert_eq!(tab_id, "t1");
                    assert_eq!(session_id, "hist-sess-7");
                }
                _ => panic!("expected SessionAttached"),
            }
            match tokio::time::timeout(std::time::Duration::from_secs(5), event_rx.recv()).await {
                Ok(Some(AppEvent::TabSystemMessage { tab_id, message })) => {
                    assert_eq!(tab_id, "t1");
                    assert!(
                        message.contains("Session loaded"),
                        "unexpected system message: {message}"
                    );
                }
                _ => panic!("expected TabSystemMessage"),
            }
            assert_eq!(
                tab_to_session.lock().await.get("t1").map(|s| s.to_string()),
                Some("hist-sess-7".to_string()),
                "loaded session must be bound to the tab"
            );
        })
        .await;
}

/// `dispatch_load_session` failure with the direct-path strategy
/// (`use_load_failure_handler = false`): a `load_session` error surfaces a
/// `TabError` routed to the target tab and leaves it unbound.
#[tokio::test]
async fn dispatch_load_session_failure_inline_emits_tab_error() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let h = connect_for_dispatch(MockBehavior::Reply);
            h.fail_load_session.store(true, Ordering::SeqCst);
            let tab_to_session = std::sync::Arc::new(tokio::sync::Mutex::new(HashMap::new()));
            let cancel_signals: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>> =
                Arc::new(Mutex::new(HashMap::new()));
            let mut event_rx = h.event_rx;

            dispatch_load_session(
                LoadSessionForTab {
                    tab_id: "t1".to_string(),
                    session_id: "hist-sess-7".to_string(),
                    cwd: None,
                },
                &h.conn,
                &tab_to_session,
                &cancel_signals,
                &h.event_tx,
                false,
                false,
                std::time::Duration::from_secs(5),
            );

            match tokio::time::timeout(std::time::Duration::from_secs(5), event_rx.recv()).await {
                Ok(Some(AppEvent::TabError { tab_id, message })) => {
                    assert_eq!(tab_id, "t1");
                    assert!(
                        message.contains("Failed to resume session"),
                        "unexpected error message: {message}"
                    );
                }
                _ => panic!("expected TabError"),
            }
            assert!(
                tab_to_session.lock().await.is_empty(),
                "failed load must leave the tab unbound"
            );
        })
        .await;
}

/// `dispatch_load_session` failure with the helper-path strategy
/// (`use_load_failure_handler = true`) and a pre-bound prior session: the
/// failure handler restores the prior binding and surfaces a `TabError`.
#[tokio::test]
async fn dispatch_load_session_failure_handler_restores_prior_binding() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let h = connect_for_dispatch(MockBehavior::Reply);
            h.fail_load_session.store(true, Ordering::SeqCst);
            let tab_to_session = std::sync::Arc::new(tokio::sync::Mutex::new(HashMap::new()));
            let cancel_signals: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>> =
                Arc::new(Mutex::new(HashMap::new()));
            let mut event_rx = h.event_rx;

            let old = acp::SessionId::new("old-sess");
            tab_to_session
                .lock()
                .await
                .insert("t1".to_string(), old.clone());

            dispatch_load_session(
                LoadSessionForTab {
                    tab_id: "t1".to_string(),
                    session_id: "hist-sess-7".to_string(),
                    cwd: None,
                },
                &h.conn,
                &tab_to_session,
                &cancel_signals,
                &h.event_tx,
                false,
                true,
                std::time::Duration::from_secs(5),
            );

            match tokio::time::timeout(std::time::Duration::from_secs(5), event_rx.recv()).await {
                Ok(Some(AppEvent::TabError { tab_id, message })) => {
                    assert_eq!(tab_id, "t1");
                    assert!(
                        message.contains("Failed to resume session"),
                        "unexpected error message: {message}"
                    );
                }
                _ => panic!("expected TabError"),
            }
            assert_eq!(
                tab_to_session.lock().await.get("t1").map(|s| s.to_string()),
                Some("old-sess".to_string()),
                "failure handler must restore the prior session binding"
            );
        })
        .await;
}

/// `dispatch_load_session` timeout path: when the agent does not respond
/// within the injected timeout, a `TabError` is surfaced (here via the direct
/// inline strategy).
#[tokio::test]
async fn dispatch_load_session_timeout_emits_tab_error() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let h = connect_for_dispatch(MockBehavior::Reply);
            h.slow_load.store(true, Ordering::SeqCst);
            let tab_to_session = std::sync::Arc::new(tokio::sync::Mutex::new(HashMap::new()));
            let cancel_signals: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>> =
                Arc::new(Mutex::new(HashMap::new()));
            let mut event_rx = h.event_rx;

            dispatch_load_session(
                LoadSessionForTab {
                    tab_id: "t1".to_string(),
                    session_id: "hist-sess-7".to_string(),
                    cwd: None,
                },
                &h.conn,
                &tab_to_session,
                &cancel_signals,
                &h.event_tx,
                false,
                false,
                std::time::Duration::from_millis(50),
            );

            match tokio::time::timeout(std::time::Duration::from_secs(5), event_rx.recv()).await {
                Ok(Some(AppEvent::TabError { tab_id, message })) => {
                    assert_eq!(tab_id, "t1");
                    assert!(
                        message.contains("timed out"),
                        "unexpected error message: {message}"
                    );
                }
                _ => panic!("expected TabError"),
            }
            assert!(
                tab_to_session.lock().await.is_empty(),
                "timed-out load must leave the tab unbound"
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



