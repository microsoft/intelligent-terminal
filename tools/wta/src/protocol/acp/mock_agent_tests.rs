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

use super::{
    dispatch_cancel, dispatch_drop_session, dispatch_load_session, dispatch_master_ext_request,
    dispatch_master_ext_request_with_yolo_timeout, dispatch_new_session, dispatch_prompt,
    dispatch_rename_session, publish_current_native_config_options, take_retired_session_result,
    AutofixTextKind, CancelRequest, DropSessionRequest, LoadSessionForTab, MasterExtRequest,
    NewSessionForTab, PromptSubmission, RenameSessionRequest,
};
use super::{ClientState, PromptUsageIdentity, ProviderProbeCapture, WtaClient};
use crate::app_contracts::{AppEvent, PlanEntry, PlanEntryStatus};
use crate::protocol::acp::conn;
use crate::protocol::acp::prompt_builder::TemplateMemo;
use crate::protocol::acp::turn_metrics::PromptTimingState;
use crate::shell::ShellManager;
use agent_client_protocol as acp;
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
#[derive(Clone)]
struct MockAgent {
    conn: Arc<OnceCell<conn::AgentLink>>,
    behavior: MockBehavior,
    /// Side-channel: every prompt's user text.
    seen_prompts: Arc<Mutex<Vec<String>>>,
    /// Side-channel: every image content block (mime, base64 data) that reached
    /// the agent on the wire — used by the Alt+V image-paste integration test.
    seen_images: Arc<Mutex<Vec<(String, String)>>>,
    /// Side-channel: the permission option id the client selected (or
    /// "cancelled"), for `AskPermission` runs.
    permission_outcome: Arc<Mutex<Option<String>>>,
    /// Side-channel: sessions released through `session/close`.
    closed_sessions: Arc<Mutex<Vec<String>>>,
    /// Side-channel: config option writes received from the client.
    seen_config_updates: Arc<Mutex<Vec<(String, String)>>>,
    /// Side-channel: stable tab ids sent through master's close-by-tab
    /// extension.
    close_tab_requests: Arc<Mutex<Vec<String>>>,
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
    new_session_advertises_native_yolo: Arc<AtomicBool>,
    new_session_starts_in_native_yolo: Arc<AtomicBool>,
    fail_native_updates: Arc<AtomicBool>,
    fail_next_native_update_after_barrier: Arc<AtomicBool>,
    hang_native_updates: Arc<AtomicBool>,
    native_update_started: Arc<tokio::sync::Notify>,
    native_update_release: Arc<tokio::sync::Notify>,
}

fn first_text(blocks: &[acp::schema::v1::ContentBlock]) -> String {
    blocks
        .iter()
        .find_map(|b| match b {
            acp::schema::v1::ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

impl MockAgent {
    async fn initialize(
        &self,
        args: acp::schema::v1::InitializeRequest,
    ) -> acp::Result<acp::schema::v1::InitializeResponse> {
        Ok(
            acp::schema::v1::InitializeResponse::new(args.protocol_version).agent_info(
                acp::schema::v1::Implementation::new("mock-acp-agent", "0.0.0")
                    .title("Mock ACP Agent"),
            ),
        )
    }

    async fn new_session(
        &self,
        _args: acp::schema::v1::NewSessionRequest,
    ) -> acp::Result<acp::schema::v1::NewSessionResponse> {
        if self.fail_new_session.load(Ordering::SeqCst) {
            return Err(acp::Error::internal_error().data("mock new_session failure".to_string()));
        }
        let mut response = acp::schema::v1::NewSessionResponse::new(
            acp::schema::v1::SessionId::new("mock-session-1"),
        );
        let starts_in_native_yolo = self
            .new_session_starts_in_native_yolo
            .load(Ordering::SeqCst);
        if starts_in_native_yolo
            || self
                .new_session_advertises_native_yolo
                .load(Ordering::SeqCst)
        {
            let option = serde_json::json!({
                "id": "mode",
                "name": "Mode",
                "category": "mode",
                "type": "select",
                "currentValue": if starts_in_native_yolo { "bypassPermissions" } else { "default" },
                "options": [
                    {"value": "default", "name": "Default"},
                    {"value": "bypassPermissions", "name": "Bypass Permissions"}
                ]
            });
            response.config_options = Some(vec![serde_json::from_value(option)
                .map_err(|error| acp::Error::internal_error().data(error.to_string()))?]);
        }
        Ok(response)
    }

    async fn authenticate(
        &self,
        _args: acp::schema::v1::AuthenticateRequest,
    ) -> acp::Result<acp::schema::v1::AuthenticateResponse> {
        Ok(acp::schema::v1::AuthenticateResponse::default())
    }

    async fn close_session(
        &self,
        args: acp::schema::v1::CloseSessionRequest,
    ) -> acp::Result<acp::schema::v1::CloseSessionResponse> {
        self.closed_sessions
            .lock()
            .unwrap()
            .push(args.session_id.to_string());
        Ok(acp::schema::v1::CloseSessionResponse::new())
    }

    async fn set_session_config_option(
        &self,
        args: acp::schema::v1::SetSessionConfigOptionRequest,
    ) -> acp::Result<acp::schema::v1::SetSessionConfigOptionResponse> {
        let config_id = args.config_id.0.to_string();
        let value = args
            .value
            .as_value_id()
            .map(|value| value.0.to_string())
            .ok_or_else(|| acp::Error::invalid_params().data("expected select value"))?;
        if self.fail_native_updates.load(Ordering::SeqCst) {
            return Err(acp::Error::internal_error().data("mock native update failure"));
        }
        if self.hang_native_updates.load(Ordering::SeqCst) {
            self.native_update_started.notify_one();
            self.native_update_release.notified().await;
        }
        if self
            .fail_next_native_update_after_barrier
            .swap(false, Ordering::SeqCst)
        {
            return Err(acp::Error::internal_error().data("mock deferred native update failure"));
        }
        self.seen_config_updates
            .lock()
            .unwrap()
            .push((config_id.clone(), value.clone()));
        let option = serde_json::from_value(serde_json::json!({
            "id": config_id,
            "name": "Mode",
            "category": "mode",
            "type": "select",
            "currentValue": value,
            "options": [
                {"value": "default", "name": "Default"},
                {"value": "plan", "name": "Plan"},
                {"value": "bypassPermissions", "name": "Bypass Permissions"}
            ]
        }))
        .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
        Ok(acp::schema::v1::SetSessionConfigOptionResponse::new(vec![
            option,
        ]))
    }

    async fn set_session_mode(
        &self,
        _args: acp::schema::v1::SetSessionModeRequest,
    ) -> acp::Result<acp::schema::v1::SetSessionModeResponse> {
        if self.hang_native_updates.load(Ordering::SeqCst) {
            self.native_update_started.notify_one();
            self.native_update_release.notified().await;
        }
        Ok(acp::schema::v1::SetSessionModeResponse::new())
    }

    async fn prompt(
        &self,
        args: acp::schema::v1::PromptRequest,
    ) -> acp::Result<acp::schema::v1::PromptResponse> {
        let text = first_text(&args.prompt);
        self.seen_prompts.lock().unwrap().push(text.clone());
        let images: Vec<(String, String)> = args
            .prompt
            .iter()
            .filter_map(|b| match b {
                acp::schema::v1::ContentBlock::Image(img) => {
                    Some((img.mime_type.clone(), img.data.clone()))
                }
                _ => None,
            })
            .collect();
        self.seen_images.lock().unwrap().extend(images);
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
                            .session_notification(acp::schema::v1::SessionNotification::new(
                                sid,
                                acp::schema::v1::SessionUpdate::AgentMessageChunk(
                                    acp::schema::v1::ContentChunk::new(reply.as_str().into()),
                                ),
                            ))
                            .await;
                    });
                }
                MockBehavior::AskPermission => {
                    let outcome_slot = self.permission_outcome.clone();
                    tokio::task::spawn_local(async move {
                        let req = acp::schema::v1::RequestPermissionRequest::new(
                            sid,
                            acp::schema::v1::ToolCallUpdate::new(
                                acp::schema::v1::ToolCallId::new("mock-tool-1"),
                                acp::schema::v1::ToolCallUpdateFields::new().title("Run: echo hi"),
                            ),
                            // Allow first so a default-selected (index 0) Enter
                            // means "allow"; reject is index 1.
                            vec![
                                acp::schema::v1::PermissionOption::new(
                                    acp::schema::v1::PermissionOptionId::new("allow-once"),
                                    "Allow once",
                                    acp::schema::v1::PermissionOptionKind::AllowOnce,
                                ),
                                acp::schema::v1::PermissionOption::new(
                                    acp::schema::v1::PermissionOptionId::new("reject-once"),
                                    "Reject",
                                    acp::schema::v1::PermissionOptionKind::RejectOnce,
                                ),
                            ],
                        );
                        if let Ok(resp) = conn.request_permission(req).await {
                            let chosen = match resp.outcome {
                                acp::schema::v1::RequestPermissionOutcome::Selected(sel) => {
                                    sel.option_id.to_string()
                                }
                                acp::schema::v1::RequestPermissionOutcome::Cancelled => {
                                    "cancelled".to_string()
                                }
                                _ => "unknown".to_string(),
                            };
                            *outcome_slot.lock().unwrap() = Some(chosen);
                        }
                    });
                }
                MockBehavior::ProposeToolCall => {
                    tokio::task::spawn_local(async move {
                        let _ = conn
                            .session_notification(acp::schema::v1::SessionNotification::new(
                                sid,
                                acp::schema::v1::SessionUpdate::ToolCall(
                                    acp::schema::v1::ToolCall::new(
                                        acp::schema::v1::ToolCallId::new("mock-tool-1"),
                                        "Run: echo hi",
                                    ),
                                ),
                            ))
                            .await;
                    });
                }
                MockBehavior::ToolThenComplete => {
                    tokio::task::spawn_local(async move {
                        let _ = conn
                            .session_notification(acp::schema::v1::SessionNotification::new(
                                sid.clone(),
                                acp::schema::v1::SessionUpdate::ToolCall(
                                    acp::schema::v1::ToolCall::new(
                                        acp::schema::v1::ToolCallId::new("mock-tool-1"),
                                        "Run: echo hi",
                                    ),
                                ),
                            ))
                            .await;
                        let _ = conn
                            .session_notification(acp::schema::v1::SessionNotification::new(
                                sid,
                                acp::schema::v1::SessionUpdate::ToolCallUpdate(
                                    acp::schema::v1::ToolCallUpdate::new(
                                        acp::schema::v1::ToolCallId::new("mock-tool-1"),
                                        acp::schema::v1::ToolCallUpdateFields::new()
                                            .status(acp::schema::v1::ToolCallStatus::Completed),
                                    ),
                                ),
                            ))
                            .await;
                    });
                }
                MockBehavior::ProposePlan => {
                    tokio::task::spawn_local(async move {
                        let _ = conn
                            .session_notification(acp::schema::v1::SessionNotification::new(
                                sid,
                                acp::schema::v1::SessionUpdate::Plan(acp::schema::v1::Plan::new(
                                    vec![
                                        acp::schema::v1::PlanEntry::new(
                                            "Step one",
                                            acp::schema::v1::PlanEntryPriority::Medium,
                                            acp::schema::v1::PlanEntryStatus::InProgress,
                                        ),
                                        acp::schema::v1::PlanEntry::new(
                                            "Step two",
                                            acp::schema::v1::PlanEntryPriority::Low,
                                            acp::schema::v1::PlanEntryStatus::Pending,
                                        ),
                                    ],
                                )),
                            ))
                            .await;
                    });
                }
                MockBehavior::StreamTwoChunks => {
                    tokio::task::spawn_local(async move {
                        for part in ["MOCK_", "OK"] {
                            let _ = conn
                                .session_notification(acp::schema::v1::SessionNotification::new(
                                    sid.clone(),
                                    acp::schema::v1::SessionUpdate::AgentMessageChunk(
                                        acp::schema::v1::ContentChunk::new(part.into()),
                                    ),
                                ))
                                .await;
                        }
                    });
                }
            }
        }

        Ok(acp::schema::v1::PromptResponse::new(
            acp::schema::v1::StopReason::EndTurn,
        ))
    }

    async fn cancel(&self, _args: acp::schema::v1::CancelNotification) -> acp::Result<()> {
        Ok(())
    }

    async fn load_session(
        &self,
        _args: acp::schema::v1::LoadSessionRequest,
    ) -> acp::Result<acp::schema::v1::LoadSessionResponse> {
        if self.slow_load.load(Ordering::SeqCst) {
            // Outlast any short injected dispatch timeout so the
            // dispatcher takes its `Err(_)` (timeout) branch, but stay
            // bounded so the task doesn't linger after the test returns.
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        if self.fail_load_session.load(Ordering::SeqCst) {
            return Err(acp::Error::internal_error().data("mock load_session failure".to_string()));
        }
        Ok(acp::schema::v1::LoadSessionResponse::new())
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
    conn::ClientLink,
    mpsc::UnboundedReceiver<AppEvent>,
    Arc<Mutex<Vec<String>>>,
    Arc<Mutex<Option<String>>>,
) {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let state = Arc::new(ClientState {
        event_tx,
        shell_mgr: Arc::new(ShellManager::new()),
        prompt_timing: Arc::new(PromptTimingState::default()),
        native_yolo: Arc::new(crate::protocol::acp::native_yolo::NativeYoloState::new()),
        yolo_state: Arc::new(Mutex::new(crate::app_contracts::YoloState::new(
            false, false,
        ))),
        provider_probe_capture: ProviderProbeCapture::default(),
        standard_usage_sessions: Mutex::new(HashSet::new()),
        proposal_channels: Arc::new(
            crate::agent_tools::action_proposal::channel::ProposalChannelManager::new(),
        ),
        hidden_tool_calls: Mutex::new(HashMap::new()),
    });
    let wta = WtaClient { state };

    let seen_prompts = Arc::new(Mutex::new(Vec::new()));
    let permission_outcome = Arc::new(Mutex::new(None));
    let conn_cell: Arc<OnceCell<conn::AgentLink>> = Arc::new(OnceCell::new());
    let mock = MockAgent {
        conn: conn_cell.clone(),
        behavior,
        seen_prompts: seen_prompts.clone(),
        seen_images: Arc::new(Mutex::new(Vec::new())),
        permission_outcome: permission_outcome.clone(),
        closed_sessions: Arc::new(Mutex::new(Vec::new())),
        seen_config_updates: Arc::new(Mutex::new(Vec::new())),
        close_tab_requests: Arc::new(Mutex::new(Vec::new())),
        fail_new_session: Arc::new(AtomicBool::new(false)),
        fail_load_session: Arc::new(AtomicBool::new(false)),
        slow_load: Arc::new(AtomicBool::new(false)),
        new_session_advertises_native_yolo: Arc::new(AtomicBool::new(false)),
        new_session_starts_in_native_yolo: Arc::new(AtomicBool::new(false)),
        fail_native_updates: Arc::new(AtomicBool::new(false)),
        fail_next_native_update_after_barrier: Arc::new(AtomicBool::new(false)),
        hang_native_updates: Arc::new(AtomicBool::new(false)),
        native_update_started: Arc::new(tokio::sync::Notify::new()),
        native_update_release: Arc::new(tokio::sync::Notify::new()),
    };

    let client_conn = spawn_mock_pair(wta, mock, &conn_cell);
    (client_conn, event_rx, seen_prompts, permission_outcome)
}

/// Wire a `WtaClient` (client) to a `MockAgent` (agent) over an in-memory duplex
/// using the 1.0 builder model, spawn both I/O loops, hand the mock its
/// `AgentLink`, and return the client `ClientLink`. Must run inside a `LocalSet`.
fn spawn_mock_pair(
    wta: WtaClient,
    mock: MockAgent,
    conn_cell: &Arc<OnceCell<conn::AgentLink>>,
) -> conn::ClientLink {
    let (wta_io, mock_io) = tokio::io::duplex(64 * 1024);
    let (wta_r, wta_w) = tokio::io::split(wta_io);
    let (mock_r, mock_w) = tokio::io::split(mock_io);

    let client_builder = acp::Client
        .builder()
        .name("mock-wta")
        .on_receive_request(
            {
                let c = wta.clone();
                move |req: acp::schema::v1::AgentRequest, responder, _cx| {
                    let c = c.clone();
                    async move {
                        use acp::schema::v1::{AgentRequest as Q, ClientResponse as R};
                        match req {
                            Q::RequestPermissionRequest(a) => conn::respond_enum(
                                responder,
                                c.request_permission(a)
                                    .await
                                    .map(R::RequestPermissionResponse),
                            ),
                            Q::CreateTerminalRequest(a) => conn::respond_enum(
                                responder,
                                c.create_terminal(a).await.map(R::CreateTerminalResponse),
                            ),
                            Q::TerminalOutputRequest(a) => conn::respond_enum(
                                responder,
                                c.terminal_output(a).await.map(R::TerminalOutputResponse),
                            ),
                            Q::WaitForTerminalExitRequest(a) => conn::respond_enum(
                                responder,
                                c.wait_for_terminal_exit(a)
                                    .await
                                    .map(R::WaitForTerminalExitResponse),
                            ),
                            Q::ReleaseTerminalRequest(a) => conn::respond_enum(
                                responder,
                                c.release_terminal(a).await.map(R::ReleaseTerminalResponse),
                            ),
                            Q::KillTerminalRequest(a) => conn::respond_enum(
                                responder,
                                c.kill_terminal(a).await.map(R::KillTerminalResponse),
                            ),
                            _ => responder.respond_with_error(acp::Error::method_not_found()),
                        }
                    }
                }
            },
            acp::on_receive_request!(),
        )
        .on_receive_notification(
            {
                let c = wta.clone();
                move |notif: acp::schema::v1::AgentNotification, _cx| {
                    let c = c.clone();
                    async move {
                        if let acp::schema::v1::AgentNotification::SessionNotification(n) = notif {
                            let _ = c.session_notification(n).await;
                        }
                        Ok(())
                    }
                }
            },
            acp::on_receive_notification!(),
        );
    let (client_conn, client_io) = conn::spawn_client(
        client_builder,
        conn::byte_streams(wta_w.compat_write(), wta_r.compat()),
    );

    let agent_builder = acp::Agent
        .builder()
        .name("mock-agent")
        .on_receive_request(
            {
                let m = mock.clone();
                move |req: acp::schema::v1::ClientRequest, responder, _cx| {
                    let m = m.clone();
                    async move {
                        use acp::schema::v1::{AgentResponse as R, ClientRequest as Q};
                        match req {
                            Q::InitializeRequest(a) => conn::respond_enum(
                                responder,
                                m.initialize(a).await.map(R::InitializeResponse),
                            ),
                            Q::AuthenticateRequest(a) => conn::respond_enum(
                                responder,
                                m.authenticate(a).await.map(R::AuthenticateResponse),
                            ),
                            Q::NewSessionRequest(a) => conn::respond_enum(
                                responder,
                                m.new_session(a).await.map(R::NewSessionResponse),
                            ),
                            Q::LoadSessionRequest(a) => conn::respond_enum(
                                responder,
                                m.load_session(a).await.map(R::LoadSessionResponse),
                            ),
                            Q::CloseSessionRequest(a) => conn::respond_enum(
                                responder,
                                m.close_session(a).await.map(R::CloseSessionResponse),
                            ),
                            Q::SetSessionConfigOptionRequest(a) => conn::respond_enum(
                                responder,
                                m.set_session_config_option(a)
                                    .await
                                    .map(R::SetSessionConfigOptionResponse),
                            ),
                            Q::SetSessionModeRequest(a) => conn::respond_enum(
                                responder,
                                m.set_session_mode(a).await.map(R::SetSessionModeResponse),
                            ),
                            Q::PromptRequest(a) => conn::respond_enum(
                                responder,
                                m.prompt(a).await.map(R::PromptResponse),
                            ),
                            Q::ExtMethodRequest(request) => {
                                if let crate::session_registry::WtaExtRequest::CloseTabSession(
                                    params,
                                ) = crate::session_registry::parse_ext_request(request)
                                {
                                    m.close_tab_requests.lock().unwrap().push(params.tab_id);
                                }
                                conn::respond_enum(
                                    responder,
                                    Ok(R::ExtMethodResponse(acp::schema::v1::ExtResponse::new(
                                        serde_json::value::to_raw_value(&serde_json::Value::Null)
                                            .unwrap()
                                            .into(),
                                    ))),
                                )
                            }
                            _ => responder.respond_with_error(acp::Error::method_not_found()),
                        }
                    }
                }
            },
            acp::on_receive_request!(),
        )
        .on_receive_notification(
            {
                let m = mock.clone();
                move |notif: acp::schema::v1::ClientNotification, _cx| {
                    let m = m.clone();
                    async move {
                        if let acp::schema::v1::ClientNotification::CancelNotification(n) = notif {
                            let _ = m.cancel(n).await;
                        }
                        Ok(())
                    }
                }
            },
            acp::on_receive_notification!(),
        );
    let (agent_conn, agent_io) = conn::spawn_agent(
        agent_builder,
        conn::byte_streams(mock_w.compat_write(), mock_r.compat()),
    );

    assert!(
        conn_cell.set(agent_conn).is_ok(),
        "mock agent connection cell must be set exactly once"
    );
    tokio::task::spawn_local(async move {
        let _ = client_io.await;
    });
    tokio::task::spawn_local(async move {
        let _ = agent_io.await;
    });
    client_conn
}

/// Happy-path harness: the mock streams a deterministic reply on each prompt.
/// Returns the client connection, the `AppEvent` receiver, and the seen-prompts
/// side-channel.
pub(crate) fn connect_mock_agent() -> (
    conn::ClientLink,
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
    conn::ClientLink,
    mpsc::UnboundedReceiver<AppEvent>,
    Arc<Mutex<Option<String>>>,
) {
    let (conn, event_rx, _seen, permission_outcome) = connect_with(MockBehavior::AskPermission);
    (conn, event_rx, permission_outcome)
}

/// Tool-call harness: the mock streams a `ToolCall` (a proposed command) on each
/// prompt. Returns the client connection and the `AppEvent` receiver.
pub(crate) fn connect_mock_agent_proposing_tool(
) -> (conn::ClientLink, mpsc::UnboundedReceiver<AppEvent>) {
    let (conn, event_rx, _seen, _outcome) = connect_with(MockBehavior::ProposeToolCall);
    (conn, event_rx)
}

/// Tool-call lifecycle harness: streams a `ToolCall` then a
/// `ToolCallUpdate(Completed)`.
pub(crate) fn connect_mock_agent_completing_tool(
) -> (conn::ClientLink, mpsc::UnboundedReceiver<AppEvent>) {
    let (conn, event_rx, _seen, _outcome) = connect_with(MockBehavior::ToolThenComplete);
    (conn, event_rx)
}

/// Plan harness: the mock streams a `Plan` with two entries.
pub(crate) fn connect_mock_agent_proposing_plan(
) -> (conn::ClientLink, mpsc::UnboundedReceiver<AppEvent>) {
    let (conn, event_rx, _seen, _outcome) = connect_with(MockBehavior::ProposePlan);
    (conn, event_rx)
}

/// Streaming harness: the mock streams the reply in two chunks.
pub(crate) fn connect_mock_agent_streaming_two_chunks(
) -> (conn::ClientLink, mpsc::UnboundedReceiver<AppEvent>) {
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

async fn next_yolo_reconcile_completion(
    event_rx: &mut mpsc::UnboundedReceiver<AppEvent>,
    timeout: std::time::Duration,
) -> (u64, bool, bool, Result<(), String>) {
    tokio::time::timeout(timeout, async {
        loop {
            match event_rx.recv().await {
                Some(AppEvent::RuntimeYoloReconcileCompleted {
                    reconcile_id,
                    fail_closed,
                    restart_required,
                    result,
                }) => break (reconcile_id, fail_closed, restart_required, result),
                Some(_) => continue,
                None => panic!("event channel closed before Yolo reconciliation completed"),
            }
        }
    })
    .await
    .expect("timed out waiting for Yolo reconciliation completion")
}

#[tokio::test]
async fn happy_path_chat_round_trip_surfaces_mock_reply() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (client_conn, mut event_rx, seen_prompts) = connect_mock_agent();

            client_conn
                .initialize(acp::schema::v1::InitializeRequest::new(
                    acp::schema::ProtocolVersion::LATEST,
                ))
                .await
                .expect("initialize failed");
            let session = client_conn
                .new_session(acp::schema::v1::NewSessionRequest::new("/test"))
                .await
                .expect("new_session failed");
            client_conn
                .prompt(acp::schema::v1::PromptRequest::new(
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
// orchestration — the per-prompt arm of the `run_acp_client_over_pipe`
// select loop — against the same mock agent, so the dispatcher
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
    client: WtaClient,
    pub conn: conn::ClientLink,
    pub event_tx: mpsc::UnboundedSender<AppEvent>,
    pub event_rx: mpsc::UnboundedReceiver<AppEvent>,
    pub shell_mgr: Arc<ShellManager>,
    pub prompt_timing: Arc<PromptTimingState>,
    pub proposal_channels:
        Arc<crate::agent_tools::action_proposal::channel::ProposalChannelManager>,
    pub seen_prompts: Arc<Mutex<Vec<String>>>,
    pub permission_outcome: Arc<Mutex<Option<String>>>,
    /// Agent-side record of every image content block (mime, base64) assembled
    /// onto the wire — the Alt+V image-paste assertion target.
    pub seen_images: Arc<Mutex<Vec<(String, String)>>>,
    pub closed_sessions: Arc<Mutex<Vec<String>>>,
    pub seen_config_updates: Arc<Mutex<Vec<(String, String)>>>,
    pub close_tab_requests: Arc<Mutex<Vec<String>>>,
    /// Flip to `true` before dispatching to make the mock's `new_session`
    /// fail, exercising the dispatcher's session-establishment error path.
    pub fail_new_session: Arc<AtomicBool>,
    /// Flip to `true` before dispatching to make the mock's `load_session`
    /// return an error, exercising the resume-failure path.
    pub fail_load_session: Arc<AtomicBool>,
    /// Flip to `true` before dispatching to make the mock's `load_session`
    /// sleep past a short injected timeout, exercising the resume-timeout path.
    pub slow_load: Arc<AtomicBool>,
    pub new_session_advertises_native_yolo: Arc<AtomicBool>,
    pub new_session_starts_in_native_yolo: Arc<AtomicBool>,
    pub fail_native_updates: Arc<AtomicBool>,
    pub fail_next_native_update_after_barrier: Arc<AtomicBool>,
    pub hang_native_updates: Arc<AtomicBool>,
    pub native_update_started: Arc<tokio::sync::Notify>,
    pub native_update_release: Arc<tokio::sync::Notify>,
}

/// Wire a real `WtaClient` to a `MockAgent` like [`connect_with`], but expose
/// the shared `event_tx` / `shell_mgr` / `prompt_timing` so the dispatcher can
/// be invoked with the same handles the production loop would.
fn connect_for_dispatch(behavior: MockBehavior) -> DispatchHarness {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let shell_mgr = Arc::new(ShellManager::new());
    let prompt_timing = Arc::new(PromptTimingState::default());
    let proposal_channels =
        Arc::new(crate::agent_tools::action_proposal::channel::ProposalChannelManager::new());
    let state = Arc::new(ClientState {
        event_tx: event_tx.clone(),
        shell_mgr: shell_mgr.clone(),
        prompt_timing: prompt_timing.clone(),
        native_yolo: Arc::new(crate::protocol::acp::native_yolo::NativeYoloState::new()),
        yolo_state: Arc::new(Mutex::new(crate::app_contracts::YoloState::new(
            false, false,
        ))),
        provider_probe_capture: ProviderProbeCapture::default(),
        standard_usage_sessions: Mutex::new(HashSet::new()),
        proposal_channels: Arc::clone(&proposal_channels),
        hidden_tool_calls: Mutex::new(HashMap::new()),
    });
    let wta = WtaClient { state };

    let seen_prompts = Arc::new(Mutex::new(Vec::new()));
    let seen_images = Arc::new(Mutex::new(Vec::new()));
    let permission_outcome = Arc::new(Mutex::new(None));
    let closed_sessions = Arc::new(Mutex::new(Vec::new()));
    let seen_config_updates = Arc::new(Mutex::new(Vec::new()));
    let close_tab_requests = Arc::new(Mutex::new(Vec::new()));
    let fail_new_session = Arc::new(AtomicBool::new(false));
    let fail_load_session = Arc::new(AtomicBool::new(false));
    let slow_load = Arc::new(AtomicBool::new(false));
    let new_session_advertises_native_yolo = Arc::new(AtomicBool::new(false));
    let new_session_starts_in_native_yolo = Arc::new(AtomicBool::new(false));
    let fail_native_updates = Arc::new(AtomicBool::new(false));
    let fail_next_native_update_after_barrier = Arc::new(AtomicBool::new(false));
    let hang_native_updates = Arc::new(AtomicBool::new(false));
    let native_update_started = Arc::new(tokio::sync::Notify::new());
    let native_update_release = Arc::new(tokio::sync::Notify::new());
    let conn_cell: Arc<OnceCell<conn::AgentLink>> = Arc::new(OnceCell::new());
    let mock = MockAgent {
        conn: conn_cell.clone(),
        behavior,
        seen_prompts: seen_prompts.clone(),
        seen_images: seen_images.clone(),
        permission_outcome: permission_outcome.clone(),
        closed_sessions: closed_sessions.clone(),
        seen_config_updates: seen_config_updates.clone(),
        close_tab_requests: close_tab_requests.clone(),
        fail_new_session: fail_new_session.clone(),
        fail_load_session: fail_load_session.clone(),
        slow_load: slow_load.clone(),
        new_session_advertises_native_yolo: new_session_advertises_native_yolo.clone(),
        new_session_starts_in_native_yolo: new_session_starts_in_native_yolo.clone(),
        fail_native_updates: fail_native_updates.clone(),
        fail_next_native_update_after_barrier: fail_next_native_update_after_barrier.clone(),
        hang_native_updates: hang_native_updates.clone(),
        native_update_started: native_update_started.clone(),
        native_update_release: native_update_release.clone(),
    };

    let client_conn = spawn_mock_pair(wta.clone(), mock, &conn_cell);

    DispatchHarness {
        client: wta,
        conn: client_conn,
        event_tx,
        event_rx,
        shell_mgr,
        prompt_timing,
        proposal_channels,
        seen_prompts,
        permission_outcome,
        seen_images,
        closed_sessions,
        seen_config_updates,
        close_tab_requests,
        fail_new_session,
        fail_load_session,
        slow_load,
        new_session_advertises_native_yolo,
        new_session_starts_in_native_yolo,
        fail_native_updates,
        fail_next_native_update_after_barrier,
        hang_native_updates,
        native_update_started,
        native_update_release,
    }
}

/// Fresh, empty per-tab dispatcher state (session map, single-flight set,
/// cancel registry, template memo) for one `dispatch_prompt` invocation.
#[allow(clippy::type_complexity)]
fn fresh_dispatch_state() -> (
    Arc<tokio::sync::Mutex<HashMap<String, acp::schema::v1::SessionId>>>,
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
        autofix_text_kind: is_autofix.then_some(AutofixTextKind::UserRequest),
        agent_command: false,
        images: Vec::new(),
        is_byok: false,
        agent_id: "copilot".to_string(),
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
                &h.client,
                &PromptUsageIdentity::default(),
                false, // wt_connected
                false, // is_agent_pane
                true,  // proposal_commands_supported
                &h.proposal_channels,
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
                .initialize(acp::schema::v1::InitializeRequest::new(
                    acp::schema::ProtocolVersion::LATEST,
                ))
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
                &h.client,
                &PromptUsageIdentity::default(),
                false,
                false,
                true,
                &h.proposal_channels,
            );

            // Pump until the agent's streamed reply surfaces — implies lazy
            // new_session, prompt assembly + send, and response routing all ran.
            // The mock echoes the *assembled* prompt, which the dispatcher wraps
            // in the terminal template, so we assert structure rather than exact
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
            // text (build_prompt_text wraps it with the terminal template).
            let seen = h.seen_prompts.lock().unwrap().clone();
            assert_eq!(seen.len(), 1, "exactly one prompt reached the agent");
            assert!(
                seen[0].contains("hello"),
                "the agent must receive the user's text inside the assembled prompt"
            );
            assert!(
                seen[0].contains("Working in Windows Terminal"),
                "a non-autofix prompt must carry the terminal template"
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

#[tokio::test]
async fn lazy_session_disables_native_yolo_before_first_prompt() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let h = connect_for_dispatch(MockBehavior::Reply);
            h.client
                .state
                .native_yolo
                .set_resolved_agent_id(Some(crate::agent_registry::CLAUDE_AGENT_ID));
            h.new_session_starts_in_native_yolo
                .store(true, Ordering::SeqCst);
            h.hang_native_updates.store(true, Ordering::SeqCst);
            h.conn
                .initialize(acp::schema::v1::InitializeRequest::new(
                    acp::schema::ProtocolVersion::LATEST,
                ))
                .await
                .expect("initialize failed");
            let (tab_to_session, in_flight, cancel_signals, memo) = fresh_dispatch_state();

            dispatch_prompt(
                test_prompt(1, "must wait", false),
                &h.conn,
                &tab_to_session,
                &memo,
                &in_flight,
                &cancel_signals,
                &h.event_tx,
                &h.shell_mgr,
                &h.prompt_timing,
                &h.client,
                &PromptUsageIdentity::default(),
                false,
                false,
                true,
                &h.proposal_channels,
            );

            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                h.native_update_started.notified(),
            )
            .await
            .expect("native disable must start before the first prompt");
            assert!(
                h.seen_prompts.lock().unwrap().is_empty(),
                "the first prompt must remain gated until native disable acknowledges"
            );

            h.native_update_release.notify_one();
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    if !h.seen_prompts.lock().unwrap().is_empty() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("first prompt did not resume after native disable");
            assert_eq!(
                *h.seen_config_updates.lock().unwrap(),
                vec![("mode".to_string(), "default".to_string())],
                "lazy first-prompt reconciliation must issue exactly one native update"
            );
        })
        .await;
}

#[tokio::test]
async fn superseded_lazy_yolo_operation_reports_retryable_error_instead_of_silent_turn_end() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut h = connect_for_dispatch(MockBehavior::Reply);
            h.client
                .state
                .native_yolo
                .set_resolved_agent_id(Some(crate::agent_registry::CLAUDE_AGENT_ID));
            h.new_session_starts_in_native_yolo
                .store(true, Ordering::SeqCst);
            h.hang_native_updates.store(true, Ordering::SeqCst);
            h.conn
                .initialize(acp::schema::v1::InitializeRequest::new(
                    acp::schema::ProtocolVersion::LATEST,
                ))
                .await
                .expect("initialize failed");
            let (tab_to_session, in_flight, cancel_signals, memo) = fresh_dispatch_state();

            dispatch_prompt(
                test_prompt(1, "must receive a retryable result", false),
                &h.conn,
                &tab_to_session,
                &memo,
                &in_flight,
                &cancel_signals,
                &h.event_tx,
                &h.shell_mgr,
                &h.prompt_timing,
                &h.client,
                &PromptUsageIdentity::default(),
                false,
                false,
                true,
                &h.proposal_channels,
            );

            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                h.native_update_started.notified(),
            )
            .await
            .expect("lazy native update must start before supersession");
            let session_id = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    if let Some(session_id) = tab_to_session.lock().await.get("0").cloned() {
                        break session_id;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("lazy session was not bound");

            h.hang_native_updates.store(false, Ordering::SeqCst);
            dispatch_master_ext_request(
                MasterExtRequest::ReconcileSessionYolo {
                    reconcile_id: 97,
                    sessions: vec![(session_id.clone(), true)],
                    fail_closed: false,
                },
                &h.conn,
                &h.event_tx,
                &tab_to_session,
                Arc::clone(&h.client.state),
            );
            h.native_update_release.notify_one();

            let message = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    match h.event_rx.recv().await {
                        Some(AppEvent::AgentError {
                            session_id: Some(completed_session),
                            failure: crate::protocol::acp::failure::AgentFailure::Protocol { .. },
                            message,
                        }) => {
                            assert_eq!(completed_session, session_id.to_string());
                            break message;
                        }
                        Some(AppEvent::AgentMessageEnd { .. }) => {
                            panic!("superseded lazy prompt ended without a retryable error")
                        }
                        Some(_) => continue,
                        None => panic!("event channel closed before retryable error"),
                    }
                }
            })
            .await
            .expect("timed out waiting for retryable lazy-prompt error");

            assert!(message.contains("Yolo"));
            assert!(h.seen_prompts.lock().unwrap().is_empty());
            let (_, fail_closed, restart_required, result) =
                next_yolo_reconcile_completion(&mut h.event_rx, std::time::Duration::from_secs(5))
                    .await;
            assert!(!fail_closed);
            assert!(!restart_required);
            assert!(result.is_ok());
            assert_eq!(
                *h.seen_config_updates.lock().unwrap(),
                vec![
                    ("mode".to_string(), "default".to_string()),
                    ("mode".to_string(), "bypassPermissions".to_string()),
                ],
                "the newer reconcile must establish its final mode after the retryable prompt error"
            );
        })
        .await;
}

#[tokio::test]
async fn policy_blocked_lazy_yolo_operation_reports_retryable_error_instead_of_silent_turn_end() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut h = connect_for_dispatch(MockBehavior::Reply);
            let blocker_h = connect_for_dispatch(MockBehavior::Reply);
            h.client
                .state
                .native_yolo
                .set_resolved_agent_id(Some(crate::agent_registry::CLAUDE_AGENT_ID));
            h.client
                .state
                .yolo_state
                .lock()
                .unwrap()
                .update_runtime(true, false);
            h.new_session_advertises_native_yolo
                .store(true, Ordering::SeqCst);
            h.conn
                .initialize(acp::schema::v1::InitializeRequest::new(
                    acp::schema::ProtocolVersion::LATEST,
                ))
                .await
                .expect("initialize failed");
            blocker_h
                .conn
                .initialize(acp::schema::v1::InitializeRequest::new(
                    acp::schema::ProtocolVersion::LATEST,
                ))
                .await
                .expect("blocker initialize failed");

            let preexisting = h
                .conn
                .new_session(acp::schema::v1::NewSessionRequest::new("/test"))
                .await
                .expect("preexisting session failed");
            h.client
                .state
                .native_yolo
                .record_from_new_session(&preexisting);
            blocker_h.hang_native_updates.store(true, Ordering::SeqCst);
            let blocker_state = Arc::clone(&h.client.state.native_yolo);
            let blocker_conn = blocker_h.conn.clone();
            let blocker_session = preexisting.session_id.clone();
            let blocker = tokio::task::spawn_local(async move {
                blocker_state
                    .apply(&blocker_conn, blocker_session, true)
                    .await
            });
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                blocker_h.native_update_started.notified(),
            )
            .await
            .expect("blocking native update did not acquire the session gate");

            let (tab_to_session, in_flight, cancel_signals, memo) = fresh_dispatch_state();
            dispatch_prompt(
                test_prompt(1, "must receive a policy error", false),
                &h.conn,
                &tab_to_session,
                &memo,
                &in_flight,
                &cancel_signals,
                &h.event_tx,
                &h.shell_mgr,
                &h.prompt_timing,
                &h.client,
                &PromptUsageIdentity::default(),
                false,
                false,
                true,
                &h.proposal_channels,
            );

            let session_id = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    if let Some(session_id) = tab_to_session.lock().await.get("0").cloned() {
                        break session_id;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("lazy session was not bound");
            h.client
                .state
                .yolo_state
                .lock()
                .unwrap()
                .update_runtime(true, true);
            blocker_h.hang_native_updates.store(false, Ordering::SeqCst);
            blocker_h.native_update_release.notify_one();

            let mut saw_reconcile_error = false;
            let mut retryable_error = None;
            let mut terminal_event_order = Vec::new();
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    match h.event_rx.recv().await {
                        Some(AppEvent::RuntimeYoloReconcileCompleted {
                            reconcile_id: 0,
                            result: Err(_),
                            ..
                        }) => {
                            saw_reconcile_error = true;
                            terminal_event_order.push("reconcile");
                        }
                        Some(AppEvent::AgentError {
                            session_id: Some(completed_session),
                            failure: crate::protocol::acp::failure::AgentFailure::Protocol { .. },
                            message,
                        }) => {
                            assert_eq!(completed_session, session_id.to_string());
                            retryable_error = Some(message);
                            terminal_event_order.push("error");
                        }
                        Some(AppEvent::AgentMessageEnd { .. }) => {
                            panic!("policy-blocked lazy prompt reached an unexpected turn end")
                        }
                        Some(_) => continue,
                        None => panic!("event channel closed before retryable policy error"),
                    }
                    if saw_reconcile_error && retryable_error.is_some() {
                        break;
                    }
                }
            })
            .await
            .expect("policy-blocked lazy prompt ended without a retryable error");

            assert!(saw_reconcile_error);
            assert!(retryable_error.unwrap().contains("Yolo"));
            assert_eq!(terminal_event_order, ["error", "reconcile"]);
            assert!(h.seen_prompts.lock().unwrap().is_empty());
            assert!(in_flight.lock().unwrap().is_empty());
            blocker
                .await
                .expect("blocking operation task panicked")
                .unwrap();
        })
        .await;
}

#[tokio::test]
async fn native_yolo_active_permission_request_remains_pending_for_user() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut h = connect_for_dispatch(MockBehavior::AskPermission);
            h.client
                .state
                .native_yolo
                .set_resolved_agent_id(Some(crate::agent_registry::CLAUDE_AGENT_ID));
            h.client
                .state
                .yolo_state
                .lock()
                .unwrap()
                .update_runtime(true, false);
            h.new_session_advertises_native_yolo
                .store(true, Ordering::SeqCst);
            h.conn
                .initialize(acp::schema::v1::InitializeRequest::new(
                    acp::schema::ProtocolVersion::LATEST,
                ))
                .await
                .expect("initialize failed");
            let (tab_to_session, in_flight, cancel_signals, memo) = fresh_dispatch_state();

            dispatch_prompt(
                test_prompt(1, "permission after native Yolo", false),
                &h.conn,
                &tab_to_session,
                &memo,
                &in_flight,
                &cancel_signals,
                &h.event_tx,
                &h.shell_mgr,
                &h.prompt_timing,
                &h.client,
                &PromptUsageIdentity::default(),
                false,
                false,
                true,
                &h.proposal_channels,
            );

            let mut published_config_value = None;
            let responder = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    match h.event_rx.recv().await {
                        Some(AppEvent::PermissionRequest { responder, .. }) => break responder,
                        Some(AppEvent::SessionConfigUpdated { options, .. }) => {
                            published_config_value = options
                                .iter()
                                .find(|option| option.id == "mode")
                                .map(|option| option.current_value.clone());
                        }
                        Some(_) => continue,
                        None => panic!("event channel closed before permission request"),
                    }
                }
            })
            .await
            .expect("timed out waiting for permission request");

            assert_eq!(
                *h.seen_config_updates.lock().unwrap(),
                vec![("mode".to_string(), "bypassPermissions".to_string())],
                "the supported provider must acknowledge native Yolo before prompting"
            );
            assert_eq!(
                published_config_value.as_deref(),
                Some("bypassPermissions"),
                "lazy startup must publish the provider-acknowledged config value before prompting"
            );
            assert!(
                h.permission_outcome.lock().unwrap().is_none(),
                "WTA must not select a permission while native Yolo is active"
            );
            tokio::task::yield_now().await;
            assert!(h.permission_outcome.lock().unwrap().is_none());

            responder.send("allow-once".to_string()).unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    if h.permission_outcome.lock().unwrap().is_some() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("permission outcome did not follow explicit user selection");
            assert_eq!(
                h.permission_outcome.lock().unwrap().as_deref(),
                Some("allow-once")
            );
        })
        .await;
}

#[tokio::test]
async fn lazy_session_native_disable_failure_keeps_first_prompt_blocked() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut h = connect_for_dispatch(MockBehavior::Reply);
            h.client
                .state
                .native_yolo
                .set_resolved_agent_id(Some(crate::agent_registry::CLAUDE_AGENT_ID));
            h.new_session_starts_in_native_yolo
                .store(true, Ordering::SeqCst);
            h.fail_native_updates.store(true, Ordering::SeqCst);
            h.conn
                .initialize(acp::schema::v1::InitializeRequest::new(
                    acp::schema::ProtocolVersion::LATEST,
                ))
                .await
                .expect("initialize failed");
            let (tab_to_session, in_flight, cancel_signals, memo) = fresh_dispatch_state();

            dispatch_prompt(
                test_prompt(1, "must never send", false),
                &h.conn,
                &tab_to_session,
                &memo,
                &in_flight,
                &cancel_signals,
                &h.event_tx,
                &h.shell_mgr,
                &h.prompt_timing,
                &h.client,
                &PromptUsageIdentity::default(),
                false,
                false,
                true,
                &h.proposal_channels,
            );

            let event = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    match h.event_rx.recv().await {
                        Some(AppEvent::RuntimeYoloReconcileCompleted {
                            reconcile_id,
                            fail_closed,
                            restart_required,
                            result,
                        }) => break (reconcile_id, fail_closed, restart_required, result),
                        Some(_) => continue,
                        None => panic!("event channel closed before fail-closed result"),
                    }
                }
            })
            .await
            .expect("timed out waiting for fail-closed result");
            assert_eq!(event.0, 0);
            assert!(event.1);
            assert!(event.2);
            assert!(event.3.is_err());
            assert!(h.seen_prompts.lock().unwrap().is_empty());
        })
        .await;
}

#[tokio::test]
async fn dispatch_agent_command_reaches_agent_verbatim() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut h = connect_for_dispatch(MockBehavior::Reply);
            h.conn
                .initialize(acp::schema::v1::InitializeRequest::new(
                    acp::schema::ProtocolVersion::LATEST,
                ))
                .await
                .expect("initialize failed");

            let (tab_to_session, in_flight, cancel_signals, memo) = fresh_dispatch_state();
            let mut prompt = test_prompt(1, "/usage", false);
            prompt.agent_command = true;

            dispatch_prompt(
                prompt,
                &h.conn,
                &tab_to_session,
                &memo,
                &in_flight,
                &cancel_signals,
                &h.event_tx,
                &h.shell_mgr,
                &h.prompt_timing,
                &h.client,
                &PromptUsageIdentity::default(),
                false,
                false,
                true,
                &h.proposal_channels,
            );

            let _ = next_agent_chunk(&mut h.event_rx).await;
            assert_eq!(
                h.seen_prompts.lock().unwrap().as_slice(),
                ["/usage"],
                "Agent commands must bypass terminal templates and runtime context"
            );
        })
        .await;
}

#[tokio::test]
async fn prompt_guard_evaluates_policy_when_the_request_is_sent() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let h = connect_for_dispatch(MockBehavior::Reply);
            h.conn
                .initialize(acp::schema::v1::InitializeRequest::new(
                    acp::schema::ProtocolVersion::LATEST,
                ))
                .await
                .expect("initialize failed");
            let allowed = Arc::new(AtomicBool::new(true));
            let request = acp::schema::v1::PromptRequest::new(
                acp::schema::v1::SessionId::new("guarded-command-session"),
                vec![acp::schema::v1::ContentBlock::Text(
                    acp::schema::v1::TextContent::new("/allow_all"),
                )],
            );
            let prompt = h.conn.prompt_if(request, {
                let allowed = Arc::clone(&allowed);
                move || allowed.load(Ordering::SeqCst)
            });

            allowed.store(false, Ordering::SeqCst);
            assert!(prompt.await.expect("prompt guard failed").is_none());
            assert!(h.seen_prompts.lock().unwrap().is_empty());
        })
        .await;
}

#[tokio::test]
async fn policy_block_allows_nonprivileged_or_non_copilot_agent_commands() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            for (agent_id, command) in [
                (crate::agent_registry::COPILOT_AGENT_ID, "/usage"),
                ("custom:copilot-look-alike", "/allow_all"),
            ] {
                let mut h = connect_for_dispatch(MockBehavior::Reply);
                h.conn
                    .initialize(acp::schema::v1::InitializeRequest::new(
                        acp::schema::ProtocolVersion::LATEST,
                    ))
                    .await
                    .expect("initialize failed");
                h.client
                    .state
                    .native_yolo
                    .set_resolved_agent_id(Some(agent_id));
                h.client
                    .state
                    .yolo_state
                    .lock()
                    .unwrap()
                    .update_runtime(false, true);

                let (tab_to_session, in_flight, cancel_signals, memo) = fresh_dispatch_state();
                let mut prompt = test_prompt(1, command, false);
                prompt.agent_command = true;

                dispatch_prompt(
                    prompt,
                    &h.conn,
                    &tab_to_session,
                    &memo,
                    &in_flight,
                    &cancel_signals,
                    &h.event_tx,
                    &h.shell_mgr,
                    &h.prompt_timing,
                    &h.client,
                    &PromptUsageIdentity::default(),
                    false,
                    false,
                    true,
                    &h.proposal_channels,
                );

                let _ = next_agent_chunk(&mut h.event_rx).await;
                assert_eq!(
                    h.seen_prompts.lock().unwrap().as_slice(),
                    [command],
                    "provider={agent_id}"
                );
            }
        })
        .await;
}

#[tokio::test]
async fn policy_block_rejects_copilot_allow_all_agent_command_before_acp() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut h = connect_for_dispatch(MockBehavior::Reply);
            h.conn
                .initialize(acp::schema::v1::InitializeRequest::new(
                    acp::schema::ProtocolVersion::LATEST,
                ))
                .await
                .expect("initialize failed");
            h.client
                .state
                .native_yolo
                .set_resolved_agent_id(Some(crate::agent_registry::COPILOT_AGENT_ID));
            h.client
                .state
                .yolo_state
                .lock()
                .unwrap()
                .update_runtime(false, true);

            let (tab_to_session, in_flight, cancel_signals, memo) = fresh_dispatch_state();
            let mut prompt = test_prompt(1, "/allow_all", false);
            prompt.agent_command = true;

            dispatch_prompt(
                prompt,
                &h.conn,
                &tab_to_session,
                &memo,
                &in_flight,
                &cancel_signals,
                &h.event_tx,
                &h.shell_mgr,
                &h.prompt_timing,
                &h.client,
                &PromptUsageIdentity::default(),
                false,
                false,
                true,
                &h.proposal_channels,
            );

            let message = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    match h.event_rx.recv().await {
                        Some(AppEvent::AgentError { message, .. }) => break message,
                        Some(AppEvent::AgentMessageChunk { .. }) => {
                            panic!("policy-blocked /allow_all reached the ACP agent")
                        }
                        Some(_) => continue,
                        None => panic!("event channel closed before policy rejection"),
                    }
                }
            })
            .await
            .expect("timed out waiting for policy rejection");
            assert!(message.contains("/allow_all"));
            assert!(message.contains("Yolo mode is disabled"));
            assert!(h.seen_prompts.lock().unwrap().is_empty());
        })
        .await;
}

#[tokio::test]
async fn dispatch_prompt_does_not_advertise_unavailable_proposals() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let h = connect_for_dispatch(MockBehavior::Reply);
            h.conn
                .initialize(acp::schema::v1::InitializeRequest::new(
                    acp::schema::ProtocolVersion::LATEST,
                ))
                .await
                .expect("initialize failed");

            let (tab_to_session, in_flight, cancel_signals, memo) = fresh_dispatch_state();
            let mut event_rx = h.event_rx;
            dispatch_prompt(
                test_prompt(1, "hello from WSL", false),
                &h.conn,
                &tab_to_session,
                &memo,
                &in_flight,
                &cancel_signals,
                &h.event_tx,
                &h.shell_mgr,
                &h.prompt_timing,
                &h.client,
                &PromptUsageIdentity::default(),
                false,
                false,
                false,
                &h.proposal_channels,
            );

            let _ = next_agent_chunk(&mut event_rx).await;
            let seen = h.seen_prompts.lock().unwrap();
            assert_eq!(seen.len(), 1);
            assert!(!seen[0].contains("WTA_CLI_PATH"));
            assert!(!seen[0].contains("--channel v1."));
        })
        .await;
}

/// Full **Alt+V image paste** integration: a screenshot-shaped DIB on the live
/// OS clipboard is captured via `read_clipboard_image` (the paste), attached to
/// a `PromptSubmission`, and dispatched through the real `dispatch_prompt` →
/// real ACP serialization → mock agent. Proves the captured image survives,
/// byte-for-byte, as a `ContentBlock::Image` on the wire alongside the user's
/// text. Skips where the session has no clipboard.
#[cfg(windows)]
#[tokio::test]
async fn dispatch_prompt_sends_clipboard_image_to_agent() {
    use crate::clipboard_image::{
        read_clipboard_image, sample_screenshot_dib, set_clipboard_dib, CLIPBOARD_TEST_LOCK,
    };

    // Capture the screenshot off the live clipboard up front (all sync). The
    // clipboard lock is held only for set+read, never across an `.await`.
    let pasted = {
        let _guard = CLIPBOARD_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dib = sample_screenshot_dib();
        if !unsafe { set_clipboard_dib(&dib) } {
            eprintln!(
                "skipping dispatch_prompt_sends_clipboard_image_to_agent: clipboard unavailable"
            );
            return;
        }
        read_clipboard_image().expect("Alt+V must capture the DIB on the clipboard")
    };
    assert_eq!(pasted.mime_type, "image/png");

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let h = connect_for_dispatch(MockBehavior::Reply);
            h.conn
                .initialize(acp::schema::v1::InitializeRequest::new(
                    acp::schema::ProtocolVersion::LATEST,
                ))
                .await
                .expect("initialize failed");

            let (tab_to_session, in_flight, cancel_signals, memo) = fresh_dispatch_state();
            let mut event_rx = h.event_rx;

            // The user typed text *and* pasted an image, then pressed Enter.
            let mut submission = test_prompt(1, "what is in this screenshot?", false);
            submission.images = vec![pasted.clone()];

            dispatch_prompt(
                submission,
                &h.conn,
                &tab_to_session,
                &memo,
                &in_flight,
                &cancel_signals,
                &h.event_tx,
                &h.shell_mgr,
                &h.prompt_timing,
                &h.client,
                &PromptUsageIdentity::default(),
                false, // wt_connected
                true,  // is_agent_pane
                true,  // proposal_commands_supported
                &h.proposal_channels,
            );

            // Pump until the turn ends so the prompt has fully reached the agent.
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

            // The agent received exactly the captured image as a ContentBlock::Image.
            let images = h.seen_images.lock().unwrap().clone();
            assert_eq!(images.len(), 1, "exactly one image must reach the agent");
            assert_eq!(images[0].0, "image/png", "mime type must survive the wire");
            assert_eq!(
                images[0].1, pasted.data_base64,
                "the exact captured image bytes must reach the agent unmodified"
            );

            // ...and the user's text rode along in the same prompt.
            let seen = h.seen_prompts.lock().unwrap().clone();
            assert_eq!(seen.len(), 1);
            assert!(
                seen[0].contains("what is in this screenshot?"),
                "the user's text must accompany the image"
            );
        })
        .await;
}
#[tokio::test]
async fn dispatch_prompt_new_session_failure_emits_error_and_releases_slot() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let h = connect_for_dispatch(MockBehavior::Reply);
            h.conn
                .initialize(acp::schema::v1::InitializeRequest::new(
                    acp::schema::ProtocolVersion::LATEST,
                ))
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
                &h.client,
                &PromptUsageIdentity::default(),
                false,
                false,
                true,
                &h.proposal_channels,
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

/// First-turn autofix installs the base terminal-agent prompt and adds the
/// autofix instruction overlay.
#[tokio::test]
async fn dispatch_prompt_first_autofix_includes_base_and_overlay() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let h = connect_for_dispatch(MockBehavior::Reply);
            h.conn
                .initialize(acp::schema::v1::InitializeRequest::new(
                    acp::schema::ProtocolVersion::LATEST,
                ))
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
                &h.client,
                &PromptUsageIdentity::default(),
                false,
                false,
                true,
                &h.proposal_channels,
            );

            let _ = next_agent_chunk(&mut event_rx).await; // wait for the round-trip
            let seen = h.seen_prompts.lock().unwrap().clone();
            assert_eq!(seen.len(), 1);
            assert!(
                seen[0].contains("Auto-Fix Instructions"),
                "autofix prompt must carry the auto-fix instruction overlay"
            );
            assert!(
                seen[0].contains("# Working in Windows Terminal"),
                "first-turn autofix must install the base terminal-agent prompt"
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
async fn dispatch_rename_session_rekeys_existing_and_ignores_missing() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let tab_to_session = std::sync::Arc::new(tokio::sync::Mutex::new(HashMap::new()));
            let sid = acp::schema::v1::SessionId::new("sess-rekey");
            super::set_helper_owner_tab_id(Some("old-tab"));
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
            )
            .await;
            {
                let g = tab_to_session.lock().await;
                assert!(!g.contains_key("old-tab"));
                assert_eq!(g.get("new-tab"), Some(&sid));
            }
            assert_eq!(
                super::helper_owner_tab_id().as_deref(),
                Some("new-tab"),
                "future session metadata must use the dragged tab id"
            );

            // No-op: renaming a ghost tab leaves the map untouched.
            dispatch_rename_session(
                RenameSessionRequest {
                    old_tab_id: "ghost".to_string(),
                    new_tab_id: "phantom".to_string(),
                },
                &tab_to_session,
            )
            .await;
            let g = tab_to_session.lock().await;
            assert!(!g.contains_key("phantom"), "missing old id must be a no-op");
            assert!(
                g.contains_key("new-tab"),
                "existing binding must survive the no-op"
            );
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

/// `dispatch_drop_session` must close and unbind the tab's session, fire its
/// in-flight cancel signal, and be a no-op for a tab that holds no session.
#[tokio::test]
async fn dispatch_drop_session_closes_unbinds_and_fires_cancel_then_ignores_missing() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let h = connect_for_dispatch(MockBehavior::Reply);
            let tab_to_session = std::sync::Arc::new(tokio::sync::Mutex::new(HashMap::new()));
            let cancel_signals: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>> =
                Arc::new(Mutex::new(HashMap::new()));
            let memo = TemplateMemo::default();

            let sid = acp::schema::v1::SessionId::new("sess-drop");
            tab_to_session
                .lock()
                .await
                .insert("t1".to_string(), sid.clone());
            let (tx, rx) = oneshot::channel::<()>();
            cancel_signals.lock().unwrap().insert(sid.to_string(), tx);

            dispatch_drop_session(
                DropSessionRequest {
                    tab_id: "t1".to_string(),
                    notify_master: true,
                },
                &h.conn,
                &tab_to_session,
                &memo,
                &cancel_signals,
                &h.client.state,
            )
            .await;

            // The in-flight cancel oneshot fires before the close request completes.
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
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    if h.close_tab_requests.lock().unwrap().as_slice() == ["t1".to_string()] {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("drop must ask master to resolve and close the destroyed tab");

            // No-op: dropping an unbound tab leaves the map empty, no panic.
            dispatch_drop_session(
                DropSessionRequest {
                    tab_id: "unbound".to_string(),
                    notify_master: true,
                },
                &h.conn,
                &tab_to_session,
                &memo,
                &cancel_signals,
                &h.client.state,
            )
            .await;
            assert!(tab_to_session.lock().await.is_empty());
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    if h.close_tab_requests.lock().unwrap().as_slice()
                        == ["t1".to_string(), "unbound".to_string()]
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("even a helper without a local binding must notify master");

            let reset_sid = acp::schema::v1::SessionId::new("sess-reset");
            tab_to_session
                .lock()
                .await
                .insert("reset-tab".to_string(), reset_sid.clone());
            let (reset_tx, reset_rx) = oneshot::channel::<()>();
            cancel_signals
                .lock()
                .unwrap()
                .insert(reset_sid.to_string(), reset_tx);

            dispatch_drop_session(
                DropSessionRequest {
                    tab_id: "reset-tab".to_string(),
                    notify_master: false,
                },
                &h.conn,
                &tab_to_session,
                &memo,
                &cancel_signals,
                &h.client.state,
            )
            .await;

            assert!(
                tokio::time::timeout(std::time::Duration::from_secs(5), reset_rx)
                    .await
                    .is_ok(),
                "WT reset must still cancel the helper-local in-flight prompt"
            );
            assert!(!tab_to_session.lock().await.contains_key("reset-tab"));
            for _ in 0..10 {
                tokio::task::yield_now().await;
            }
            assert_eq!(
                h.close_tab_requests.lock().unwrap().as_slice(),
                ["t1".to_string(), "unbound".to_string()],
                "WT reset physical close is master-owned and must not be duplicated by a helper"
            );
        })
        .await;
}

#[test]
fn retired_session_result_is_consumed_without_leaving_private_metadata() {
    let mut meta = None;
    crate::session_registry::inject_wta_meta(
        &mut meta,
        &crate::session_registry::WtaMeta {
            session_result: Some("retired".to_string()),
            ..Default::default()
        },
    );

    assert!(take_retired_session_result(&mut meta));
    assert!(meta.is_none());
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
                Arc::clone(&h.client.state),
                false,
                false,
                "Test",
                &h.proposal_channels,
                false,
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

/// `dispatch_new_session` failure path: when `new_session` errors, a
/// tab-scoped error is surfaced and the tab is left unbound.
#[tokio::test]
async fn dispatch_new_session_failure_emits_tab_error_and_leaves_unbound() {
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
                Arc::clone(&h.client.state),
                false,
                false,
                "Test",
                &h.proposal_channels,
                false,
            );

            match tokio::time::timeout(std::time::Duration::from_secs(5), event_rx.recv()).await {
                Ok(Some(AppEvent::TabError { tab_id, message })) => {
                    assert_eq!(tab_id, "t1");
                    assert!(
                        message.contains("/new failed for tab t1"),
                        "unexpected error message: {message}"
                    );
                }
                _ => panic!("expected TabError"),
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

            let old = acp::schema::v1::SessionId::new("old-sess");
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
                Arc::clone(&h.client.state),
                false,
                false,
                "Test",
                &h.proposal_channels,
                false,
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
/// to the tab and emits `SessionAttached`. Resume is silent — no
/// confirmation note — so a resumed pane looks like a normal connection.
#[tokio::test]
async fn dispatch_load_session_binds_and_emits_attached() {
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
                Arc::clone(&h.client.state),
                false,
                false,
                std::time::Duration::from_secs(5),
                &h.proposal_channels,
                false,
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
                Arc::clone(&h.client.state),
                false,
                false,
                std::time::Duration::from_secs(5),
                &h.proposal_channels,
                false,
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

            let old = acp::schema::v1::SessionId::new("old-sess");
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
                Arc::clone(&h.client.state),
                false,
                true,
                std::time::Duration::from_secs(5),
                &h.proposal_channels,
                false,
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
                Arc::clone(&h.client.state),
                false,
                false,
                std::time::Duration::from_millis(50),
                &h.proposal_channels,
                false,
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
                MasterExtRequest::SessionsList {
                    request_id: 7,
                    rescan: false,
                },
                &h.conn,
                &h.event_tx,
                &tab_to_session,
                Arc::clone(&h.client.state),
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
                    sid: acp::schema::v1::SessionId::new("sess-focus"),
                },
                &h.conn,
                &h.event_tx,
                &tab_to_session,
                Arc::clone(&h.client.state),
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

#[tokio::test]
async fn generic_config_response_refreshes_native_yolo_restore_value() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut h = connect_for_dispatch(MockBehavior::Reply);
            h.client
                .state
                .native_yolo
                .set_resolved_agent_id(Some(crate::agent_registry::CLAUDE_AGENT_ID));
            let response: acp::schema::v1::NewSessionResponse =
                serde_json::from_value(serde_json::json!({
                    "sessionId": "config-refresh-session",
                    "configOptions": [{
                        "id": "mode",
                        "name": "Mode",
                        "category": "mode",
                        "type": "select",
                        "currentValue": "default",
                        "options": [
                            {"value": "default", "name": "Default"},
                            {"value": "plan", "name": "Plan"},
                            {"value": "bypassPermissions", "name": "Bypass Permissions"}
                        ]
                    }]
                }))
                .unwrap();
            let session_id = response.session_id.clone();
            h.client
                .state
                .native_yolo
                .record_from_new_session(&response);
            let tab_to_session = Arc::new(tokio::sync::Mutex::new(HashMap::from([(
                "tab-1".to_string(),
                session_id.clone(),
            )])));

            dispatch_master_ext_request(
                MasterExtRequest::SetSessionConfigOption {
                    session_id: session_id.clone(),
                    config_id: "mode".to_string(),
                    value: "plan".to_string(),
                },
                &h.conn,
                &h.event_tx,
                &tab_to_session,
                Arc::clone(&h.client.state),
            );

            loop {
                match tokio::time::timeout(std::time::Duration::from_secs(5), h.event_rx.recv())
                    .await
                {
                    Ok(Some(AppEvent::SessionConfigSetCompleted { .. })) => break,
                    Ok(Some(_)) => {}
                    _ => panic!("expected SessionConfigSetCompleted"),
                }
            }

            h.client
                .state
                .native_yolo
                .apply(&h.conn, session_id, false)
                .await
                .unwrap();

            assert_eq!(
                *h.seen_config_updates.lock().unwrap(),
                vec![
                    ("mode".to_string(), "plan".to_string()),
                    ("mode".to_string(), "plan".to_string()),
                ]
            );
        })
        .await;
}

#[tokio::test]
async fn policy_block_rejects_privileged_generic_config_before_acp() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            for (agent_id, config_id, category, restore_value, enable_value) in [
                (
                    crate::agent_registry::COPILOT_AGENT_ID,
                    "allow_all",
                    "permissions",
                    "off",
                    "on",
                ),
                (
                    crate::agent_registry::CLAUDE_AGENT_ID,
                    "mode",
                    "mode",
                    "default",
                    "bypassPermissions",
                ),
                (
                    crate::agent_registry::CODEX_AGENT_ID,
                    "mode",
                    "mode",
                    "agent",
                    "agent-full-access",
                ),
            ] {
                let mut h = connect_for_dispatch(MockBehavior::Reply);
                h.client
                    .state
                    .native_yolo
                    .set_resolved_agent_id(Some(agent_id));
                let response: acp::schema::v1::NewSessionResponse =
                    serde_json::from_value(serde_json::json!({
                        "sessionId": format!("{agent_id}-policy-config-session"),
                        "configOptions": [{
                            "id": config_id,
                            "name": "Native Yolo",
                            "category": category,
                            "type": "select",
                            "currentValue": restore_value,
                            "options": [
                                {"value": restore_value, "name": "Restore"},
                                {"value": enable_value, "name": "Enable"}
                            ]
                        }]
                    }))
                    .unwrap();
                let session_id = response.session_id.clone();
                h.client
                    .state
                    .native_yolo
                    .record_from_new_session(&response);
                h.client
                    .state
                    .yolo_state
                    .lock()
                    .unwrap()
                    .update_runtime(false, true);
                let tab_to_session = Arc::new(tokio::sync::Mutex::new(HashMap::from([(
                    "tab-1".to_string(),
                    session_id.clone(),
                )])));

                dispatch_master_ext_request(
                    MasterExtRequest::SetSessionConfigOption {
                        session_id,
                        config_id: config_id.to_string(),
                        value: enable_value.to_string(),
                    },
                    &h.conn,
                    &h.event_tx,
                    &tab_to_session,
                    Arc::clone(&h.client.state),
                );

                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    loop {
                        match h.event_rx.recv().await {
                            Some(AppEvent::SessionConfigSetFailed { message, .. }) => {
                                assert!(message.contains("policy"), "provider={agent_id}");
                                break;
                            }
                            Some(_) => continue,
                            None => {
                                panic!("expected SessionConfigSetFailed, provider={agent_id}")
                            }
                        }
                    }
                })
                .await
                .expect("expected SessionConfigSetFailed, got nothing");
                assert!(
                    h.seen_config_updates.lock().unwrap().is_empty(),
                    "blocked privileged config must never reach ACP: provider={agent_id}"
                );
            }
        })
        .await;
}

#[tokio::test]
async fn queued_privileged_config_rechecks_policy_after_native_gate() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut h = connect_for_dispatch(MockBehavior::Reply);
            h.client
                .state
                .native_yolo
                .set_resolved_agent_id(Some(crate::agent_registry::CLAUDE_AGENT_ID));
            let response: acp::schema::v1::NewSessionResponse =
                serde_json::from_value(serde_json::json!({
                    "sessionId": "queued-policy-config-session",
                    "configOptions": [{
                        "id": "mode",
                        "name": "Mode",
                        "category": "mode",
                        "type": "select",
                        "currentValue": "default",
                        "options": [
                            {"value": "default", "name": "Default"},
                            {"value": "bypassPermissions", "name": "Bypass Permissions"}
                        ]
                    }]
                }))
                .unwrap();
            let session_id = response.session_id.clone();
            h.client
                .state
                .native_yolo
                .record_from_new_session(&response);
            let tab_to_session = Arc::new(tokio::sync::Mutex::new(HashMap::from([(
                "tab-1".to_string(),
                session_id.clone(),
            )])));
            h.hang_native_updates.store(true, Ordering::SeqCst);

            let native_yolo = Arc::clone(&h.client.state.native_yolo);
            let conn = h.conn.clone();
            let blocking_session = session_id.clone();
            let blocking_update = tokio::task::spawn_local(async move {
                native_yolo
                    .apply(&conn, blocking_session, false)
                    .await
                    .unwrap();
            });
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                h.native_update_started.notified(),
            )
            .await
            .expect("blocking native update must acquire the session gate");

            dispatch_master_ext_request(
                MasterExtRequest::SetSessionConfigOption {
                    session_id,
                    config_id: "mode".to_string(),
                    value: "bypassPermissions".to_string(),
                },
                &h.conn,
                &h.event_tx,
                &tab_to_session,
                Arc::clone(&h.client.state),
            );
            tokio::task::yield_now().await;
            h.client
                .state
                .yolo_state
                .lock()
                .unwrap()
                .update_runtime(false, true);
            h.native_update_release.notify_one();
            blocking_update.await.unwrap();

            let message = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    match h.event_rx.recv().await {
                        Some(AppEvent::SessionConfigSetFailed { message, .. }) => break message,
                        Some(_) => continue,
                        None => panic!("event channel closed before config rejection"),
                    }
                }
            })
            .await
            .expect("timed out waiting for queued config rejection");
            assert!(message.contains("policy"));
            assert_eq!(
                *h.seen_config_updates.lock().unwrap(),
                vec![("mode".to_string(), "default".to_string())],
                "the queued privileged enable must not reach ACP after policy blocks it"
            );
        })
        .await;
}

#[tokio::test]
async fn native_disable_config_timeout_requests_restart() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut h = connect_for_dispatch(MockBehavior::Reply);
            h.client
                .state
                .native_yolo
                .set_resolved_agent_id(Some(crate::agent_registry::CLAUDE_AGENT_ID));
            let response: acp::schema::v1::NewSessionResponse =
                serde_json::from_value(serde_json::json!({
                    "sessionId": "hung-config-disable-session",
                    "configOptions": [{
                        "id": "mode",
                        "name": "Mode",
                        "category": "mode",
                        "type": "select",
                        "currentValue": "bypassPermissions",
                        "options": [
                            {"value": "default", "name": "Default"},
                            {"value": "bypassPermissions", "name": "Bypass Permissions"}
                        ]
                    }]
                }))
                .unwrap();
            let session_id = response.session_id.clone();
            h.client
                .state
                .native_yolo
                .record_from_new_session(&response);
            h.hang_native_updates.store(true, Ordering::SeqCst);
            let tab_to_session = Arc::new(tokio::sync::Mutex::new(HashMap::from([(
                "tab-1".to_string(),
                session_id.clone(),
            )])));

            dispatch_master_ext_request_with_yolo_timeout(
                MasterExtRequest::SetSessionConfigOption {
                    session_id: session_id.clone(),
                    config_id: "mode".to_string(),
                    value: "plan".to_string(),
                },
                &h.conn,
                &h.event_tx,
                &tab_to_session,
                Arc::clone(&h.client.state),
                std::time::Duration::from_millis(20),
            );

            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                h.native_update_started.notified(),
            )
            .await
            .expect("the provider-native disable must start");
            let event =
                tokio::time::timeout(std::time::Duration::from_millis(250), h.event_rx.recv())
                    .await
                    .expect("native config disable timeout must report an unknown outcome")
                    .expect("event channel must remain open");
            let AppEvent::RuntimeYoloReconcileCompleted {
                reconcile_id,
                fail_closed,
                restart_required,
                result,
            } = event
            else {
                panic!("expected RuntimeYoloReconcileCompleted");
            };
            assert_eq!(reconcile_id, 0);
            assert!(fail_closed);
            assert!(restart_required);
            assert!(result.unwrap_err().contains("setting config option 'mode'"));
            h.native_update_release.notify_one();
        })
        .await;
}

#[tokio::test]
async fn rejected_config_yolo_disable_requests_restart() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut h = connect_for_dispatch(MockBehavior::Reply);
            h.client
                .state
                .native_yolo
                .set_resolved_agent_id(Some(crate::agent_registry::CLAUDE_AGENT_ID));
            let response: acp::schema::v1::NewSessionResponse =
                serde_json::from_value(serde_json::json!({
                    "sessionId": "rejected-config-disable-session",
                    "configOptions": [{
                        "id": "mode",
                        "name": "Mode",
                        "category": "mode",
                        "type": "select",
                        "currentValue": "bypassPermissions",
                        "options": [
                            {"value": "default", "name": "Default"},
                            {"value": "plan", "name": "Plan"},
                            {"value": "bypassPermissions", "name": "Bypass Permissions"}
                        ]
                    }]
                }))
                .unwrap();
            let session_id = response.session_id.clone();
            h.client
                .state
                .native_yolo
                .record_from_new_session(&response);
            h.fail_native_updates.store(true, Ordering::SeqCst);
            let tab_to_session = Arc::new(tokio::sync::Mutex::new(HashMap::from([(
                "tab-1".to_string(),
                session_id.clone(),
            )])));

            dispatch_master_ext_request(
                MasterExtRequest::SetSessionConfigOption {
                    session_id,
                    config_id: "mode".to_string(),
                    value: "plan".to_string(),
                },
                &h.conn,
                &h.event_tx,
                &tab_to_session,
                Arc::clone(&h.client.state),
            );

            let restart_required = tokio::time::timeout(std::time::Duration::from_secs(1), async {
                loop {
                    match h.event_rx.recv().await {
                        Some(AppEvent::RuntimeYoloReconcileCompleted {
                            restart_required,
                            result,
                            ..
                        }) => {
                            assert!(result.is_err());
                            break restart_required;
                        }
                        Some(_) => continue,
                        None => panic!("event channel closed before restart request"),
                    }
                }
            })
            .await
            .expect("rejected native config disable must request restart");
            assert!(restart_required);

            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                loop {
                    match h.event_rx.recv().await {
                        Some(AppEvent::SessionConfigSetFailed {
                            session_id,
                            config_id,
                            message,
                            restart_required,
                        }) => {
                            assert_eq!(session_id, "rejected-config-disable-session");
                            assert_eq!(config_id, "mode");
                            assert!(message.contains("mock native update failure"));
                            assert!(restart_required);
                            break;
                        }
                        Some(_) => continue,
                        None => panic!("event channel closed before config failure"),
                    }
                }
            })
            .await
            .expect("expected the paired SessionConfigSetFailed event");
        })
        .await;
}

#[tokio::test]
async fn superseded_config_error_defers_to_the_newer_dispatched_operation() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut h = connect_for_dispatch(MockBehavior::Reply);
            h.client
                .state
                .native_yolo
                .set_resolved_agent_id(Some(crate::agent_registry::CLAUDE_AGENT_ID));
            let response: acp::schema::v1::NewSessionResponse =
                serde_json::from_value(serde_json::json!({
                    "sessionId": "superseded-config-error",
                    "configOptions": [{
                        "id": "mode",
                        "name": "Mode",
                        "category": "mode",
                        "type": "select",
                        "currentValue": "bypassPermissions",
                        "options": [
                            {"value": "default", "name": "Default"},
                            {"value": "bypassPermissions", "name": "Bypass Permissions"}
                        ]
                    }]
                }))
                .unwrap();
            let session_id = response.session_id.clone();
            h.client
                .state
                .native_yolo
                .record_from_new_session(&response);
            let tab_to_session = Arc::new(tokio::sync::Mutex::new(HashMap::from([(
                "tab-1".to_string(),
                session_id.clone(),
            )])));
            h.hang_native_updates.store(true, Ordering::SeqCst);

            dispatch_master_ext_request(
                MasterExtRequest::SetSessionConfigOption {
                    session_id: session_id.clone(),
                    config_id: "mode".to_string(),
                    value: "default".to_string(),
                },
                &h.conn,
                &h.event_tx,
                &tab_to_session,
                Arc::clone(&h.client.state),
            );
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                h.native_update_started.notified(),
            )
            .await
            .expect("the first config RPC must reach the controlled barrier");

            dispatch_master_ext_request(
                MasterExtRequest::SetSessionConfigOption {
                    session_id,
                    config_id: "mode".to_string(),
                    value: "bypassPermissions".to_string(),
                },
                &h.conn,
                &h.event_tx,
                &tab_to_session,
                Arc::clone(&h.client.state),
            );
            // Dispatch reserves the newer sequence synchronously before spawning its
            // gate waiter, while the first request still owns the per-session gate.
            h.fail_next_native_update_after_barrier
                .store(true, Ordering::SeqCst);
            h.hang_native_updates.store(false, Ordering::SeqCst);
            h.native_update_release.notify_one();

            let mut saw_superseded = false;
            let mut saw_newer_completion = false;
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                while !saw_superseded || !saw_newer_completion {
                    match h.event_rx.recv().await {
                        Some(AppEvent::SessionConfigSetFailed {
                            message,
                            restart_required,
                            ..
                        }) => {
                            assert!(message.contains("superseded"));
                            assert!(!restart_required);
                            saw_superseded = true;
                        }
                        Some(AppEvent::SessionConfigSetCompleted { value, .. }) => {
                            assert_eq!(value, "bypassPermissions");
                            saw_newer_completion = true;
                        }
                        Some(AppEvent::RuntimeYoloReconcileCompleted { .. }) => {
                            panic!("the stale failed disable must not restart before the newer operation")
                        }
                        Some(_) => continue,
                        None => panic!("event channel closed before both config results"),
                    }
                }
            })
            .await
            .expect("both serialized config operations must complete");
            assert_eq!(
                *h.seen_config_updates.lock().unwrap(),
                vec![("mode".to_string(), "bypassPermissions".to_string())]
            );
        })
        .await;
}

#[tokio::test]
async fn rejected_reconcile_yolo_disable_requests_restart() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut h = connect_for_dispatch(MockBehavior::Reply);
            h.client
                .state
                .native_yolo
                .set_resolved_agent_id(Some(crate::agent_registry::CLAUDE_AGENT_ID));
            let response: acp::schema::v1::NewSessionResponse =
                serde_json::from_value(serde_json::json!({
                    "sessionId": "rejected-reconcile-disable-session",
                    "configOptions": [{
                        "id": "mode",
                        "name": "Mode",
                        "category": "mode",
                        "type": "select",
                        "currentValue": "bypassPermissions",
                        "options": [
                            {"value": "default", "name": "Default"},
                            {"value": "bypassPermissions", "name": "Bypass Permissions"}
                        ]
                    }]
                }))
                .unwrap();
            let session_id = response.session_id.clone();
            h.client
                .state
                .native_yolo
                .record_from_new_session(&response);
            h.fail_native_updates.store(true, Ordering::SeqCst);
            let tab_to_session = Arc::new(tokio::sync::Mutex::new(HashMap::new()));

            dispatch_master_ext_request(
                MasterExtRequest::ReconcileSessionYolo {
                    reconcile_id: 61,
                    sessions: vec![(session_id, false)],
                    fail_closed: true,
                },
                &h.conn,
                &h.event_tx,
                &tab_to_session,
                Arc::clone(&h.client.state),
            );

            let (reconcile_id, fail_closed, restart_required, result) =
                next_yolo_reconcile_completion(&mut h.event_rx, std::time::Duration::from_secs(1))
                    .await;
            assert_eq!(reconcile_id, 61);
            assert!(fail_closed);
            assert!(result.is_err());
            assert!(
                restart_required,
                "a rejected disable has unknown provider state"
            );
        })
        .await;
}

#[tokio::test]
async fn successful_reconcile_publishes_returned_config_options() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut h = connect_for_dispatch(MockBehavior::Reply);
            h.client
                .state
                .native_yolo
                .set_resolved_agent_id(Some(crate::agent_registry::CLAUDE_AGENT_ID));
            let response: acp::schema::v1::NewSessionResponse =
                serde_json::from_value(serde_json::json!({
                    "sessionId": "reconcile-config-update-session",
                    "configOptions": [{
                        "id": "mode",
                        "name": "Mode",
                        "category": "mode",
                        "type": "select",
                        "currentValue": "default",
                        "options": [
                            {"value": "default", "name": "Default"},
                            {"value": "bypassPermissions", "name": "Bypass Permissions"}
                        ]
                    }]
                }))
                .unwrap();
            let session_id = response.session_id.clone();
            h.client
                .state
                .native_yolo
                .record_from_new_session(&response);
            let tab_to_session = Arc::new(tokio::sync::Mutex::new(HashMap::new()));

            dispatch_master_ext_request(
                MasterExtRequest::ReconcileSessionYolo {
                    reconcile_id: 62,
                    sessions: vec![(session_id.clone(), true)],
                    fail_closed: false,
                },
                &h.conn,
                &h.event_tx,
                &tab_to_session,
                Arc::clone(&h.client.state),
            );

            let mut config_updated = false;
            let mut reconcile_completed = false;
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                while !config_updated || !reconcile_completed {
                    match h.event_rx.recv().await {
                        Some(AppEvent::SessionConfigUpdated {
                            session_id: updated_session,
                            options,
                        }) if updated_session == session_id.to_string() => {
                            assert_eq!(options.len(), 1);
                            assert_eq!(options[0].id, "mode");
                            assert_eq!(options[0].current_value, "bypassPermissions");
                            assert!(options[0].native_yolo);
                            config_updated = true;
                        }
                        Some(AppEvent::RuntimeYoloReconcileCompleted {
                            reconcile_id: 62,
                            result,
                            ..
                        }) => {
                            result.unwrap();
                            reconcile_completed = true;
                        }
                        Some(_) => continue,
                        None => panic!("event channel closed before reconciliation updates"),
                    }
                }
            })
            .await
            .expect("reconciliation must publish config options and completion");
        })
        .await;
}

#[test]
fn superseded_reconcile_does_not_publish_stale_config_options() {
    let native_yolo = crate::protocol::acp::native_yolo::NativeYoloState::new();
    native_yolo.set_resolved_agent_id(Some(crate::agent_registry::CLAUDE_AGENT_ID));
    let response: acp::schema::v1::NewSessionResponse = serde_json::from_value(serde_json::json!({
        "sessionId": "superseded-config-publication",
        "configOptions": [{
            "id": "mode",
            "name": "Mode",
            "category": "mode",
            "type": "select",
            "currentValue": "default",
            "options": [
                {"value": "default", "name": "Default"},
                {"value": "bypassPermissions", "name": "Bypass Permissions"}
            ]
        }]
    }))
    .unwrap();
    let session_id = response.session_id.clone();
    native_yolo.record_from_new_session(&response);
    let stale_operation = native_yolo.reserve_operation(session_id.clone(), true);
    let _newer_operation = native_yolo.reserve_operation(session_id.clone(), false);
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();

    assert!(!publish_current_native_config_options(
        &event_tx,
        &native_yolo,
        &session_id,
        &stale_operation,
        response.config_options.as_deref(),
    ));
    assert!(event_rx.try_recv().is_err());
}

#[tokio::test]
async fn fail_closed_yolo_reconcile_has_one_deadline_across_sessions() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut h = connect_for_dispatch(MockBehavior::Reply);
            h.client
                .state
                .native_yolo
                .set_resolved_agent_id(Some(crate::agent_registry::CLAUDE_AGENT_ID));
            let mut sessions = Vec::new();
            for name in ["hung-policy-session", "queued-policy-session"] {
                let response: acp::schema::v1::NewSessionResponse =
                    serde_json::from_value(serde_json::json!({
                        "sessionId": name,
                        "configOptions": [{
                            "id": "mode",
                            "name": "Mode",
                            "category": "mode",
                            "type": "select",
                            "currentValue": "bypassPermissions",
                            "options": [
                                {"value": "default", "name": "Default"},
                                {"value": "bypassPermissions", "name": "Bypass Permissions"}
                            ]
                        }]
                    }))
                    .unwrap();
                h.client
                    .state
                    .native_yolo
                    .record_from_new_session(&response);
                sessions.push((response.session_id, false));
            }
            h.hang_native_updates.store(true, Ordering::SeqCst);
            let tab_to_session = Arc::new(tokio::sync::Mutex::new(HashMap::new()));

            dispatch_master_ext_request_with_yolo_timeout(
                MasterExtRequest::ReconcileSessionYolo {
                    reconcile_id: 1,
                    sessions,
                    fail_closed: true,
                },
                &h.conn,
                &h.event_tx,
                &tab_to_session,
                Arc::clone(&h.client.state),
                std::time::Duration::from_millis(50),
            );

            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                h.native_update_started.notified(),
            )
            .await
            .expect("the first provider disable must start");
            let (_, fail_closed, restart_required, result) = next_yolo_reconcile_completion(
                &mut h.event_rx,
                std::time::Duration::from_millis(250),
            )
            .await;
            assert!(fail_closed);
            assert!(restart_required);
            assert!(result.unwrap_err().contains("timed out"));
            assert!(h.seen_config_updates.lock().unwrap().is_empty());
            h.native_update_release.notify_one();
        })
        .await;
}

#[tokio::test]
async fn fail_closed_yolo_reconcile_stops_after_first_error() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut h = connect_for_dispatch(MockBehavior::Reply);
            h.client
                .state
                .native_yolo
                .set_resolved_agent_id(Some(crate::agent_registry::CLAUDE_AGENT_ID));
            let missing_session = acp::schema::v1::SessionId::new("missing-policy-session");
            h.client.state.native_yolo.record_from_load_session(
                &missing_session,
                &acp::schema::v1::LoadSessionResponse::new(),
            );
            let available: acp::schema::v1::NewSessionResponse =
                serde_json::from_value(serde_json::json!({
                    "sessionId": "available-policy-session",
                    "configOptions": [{
                        "id": "mode",
                        "name": "Mode",
                        "category": "mode",
                        "type": "select",
                        "currentValue": "bypassPermissions",
                        "options": [
                            {"value": "default", "name": "Default"},
                            {"value": "bypassPermissions", "name": "Bypass Permissions"}
                        ]
                    }]
                }))
                .unwrap();
            h.client
                .state
                .native_yolo
                .record_from_new_session(&available);
            let tab_to_session = Arc::new(tokio::sync::Mutex::new(HashMap::new()));

            dispatch_master_ext_request_with_yolo_timeout(
                MasterExtRequest::ReconcileSessionYolo {
                    reconcile_id: 2,
                    sessions: vec![(missing_session, false), (available.session_id, false)],
                    fail_closed: true,
                },
                &h.conn,
                &h.event_tx,
                &tab_to_session,
                Arc::clone(&h.client.state),
                std::time::Duration::from_millis(250),
            );

            let (_, fail_closed, restart_required, result) =
                next_yolo_reconcile_completion(&mut h.event_rx, std::time::Duration::from_secs(1))
                    .await;
            assert!(fail_closed);
            assert!(restart_required);
            assert!(result
                .unwrap_err()
                .contains("expected ACP session Yolo capability"));
            assert!(
                h.seen_config_updates.lock().unwrap().is_empty(),
                "later sessions must wait for restart after a fail-closed error"
            );
        })
        .await;
}

#[tokio::test]
async fn timed_out_mode_yolo_change_requests_restart() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut h = connect_for_dispatch(MockBehavior::Reply);
            h.client
                .state
                .native_yolo
                .set_resolved_agent_id(Some(crate::agent_registry::GEMINI_AGENT_ID));
            let response: acp::schema::v1::NewSessionResponse =
                serde_json::from_value(serde_json::json!({
                    "sessionId": "hung-mode-dispatch-session",
                    "modes": {
                        "currentModeId": "default",
                        "availableModes": [
                            {"id": "default", "name": "Default"},
                            {"id": "yolo", "name": "YOLO"}
                        ]
                    }
                }))
                .unwrap();
            let session_id = response.session_id.clone();
            h.client
                .state
                .native_yolo
                .record_from_new_session(&response);
            h.hang_native_updates.store(true, Ordering::SeqCst);
            let tab_to_session = Arc::new(tokio::sync::Mutex::new(HashMap::new()));

            dispatch_master_ext_request_with_yolo_timeout(
                MasterExtRequest::ReconcileSessionYolo {
                    reconcile_id: 41,
                    sessions: vec![(session_id, true)],
                    fail_closed: false,
                },
                &h.conn,
                &h.event_tx,
                &tab_to_session,
                Arc::clone(&h.client.state),
                std::time::Duration::from_millis(20),
            );

            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                h.native_update_started.notified(),
            )
            .await
            .expect("the provider mode mutation must start");
            let event =
                tokio::time::timeout(std::time::Duration::from_millis(250), h.event_rx.recv())
                    .await
                    .expect("the mode timeout must produce a completion event")
                    .expect("event channel must remain open");
            let AppEvent::RuntimeYoloReconcileCompleted {
                reconcile_id,
                fail_closed,
                restart_required,
                result,
            } = event
            else {
                panic!("expected RuntimeYoloReconcileCompleted");
            };
            assert_eq!(reconcile_id, 41);
            assert!(!fail_closed);
            assert!(restart_required);
            assert!(result.unwrap_err().contains("setting mode 'yolo'"));
            h.native_update_release.notify_one();
        })
        .await;
}

#[tokio::test]
async fn non_fail_closed_yolo_reconcile_continues_after_known_error() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut h = connect_for_dispatch(MockBehavior::Reply);
            h.client
                .state
                .native_yolo
                .set_resolved_agent_id(Some(crate::agent_registry::CLAUDE_AGENT_ID));
            let missing_session = acp::schema::v1::SessionId::new("missing-enable-session");
            h.client.state.native_yolo.record_from_load_session(
                &missing_session,
                &acp::schema::v1::LoadSessionResponse::new(),
            );
            let available: acp::schema::v1::NewSessionResponse =
                serde_json::from_value(serde_json::json!({
                    "sessionId": "available-enable-session",
                    "configOptions": [{
                        "id": "mode",
                        "name": "Mode",
                        "category": "mode",
                        "type": "select",
                        "currentValue": "default",
                        "options": [
                            {"value": "default", "name": "Default"},
                            {"value": "bypassPermissions", "name": "Bypass Permissions"}
                        ]
                    }]
                }))
                .unwrap();
            h.client
                .state
                .native_yolo
                .record_from_new_session(&available);
            let tab_to_session = Arc::new(tokio::sync::Mutex::new(HashMap::new()));

            dispatch_master_ext_request_with_yolo_timeout(
                MasterExtRequest::ReconcileSessionYolo {
                    reconcile_id: 3,
                    sessions: vec![(missing_session, true), (available.session_id, true)],
                    fail_closed: false,
                },
                &h.conn,
                &h.event_tx,
                &tab_to_session,
                Arc::clone(&h.client.state),
                std::time::Duration::from_millis(250),
            );

            let (_, fail_closed, restart_required, result) =
                next_yolo_reconcile_completion(&mut h.event_rx, std::time::Duration::from_secs(1))
                    .await;
            assert!(!fail_closed);
            assert!(!restart_required);
            assert!(result
                .unwrap_err()
                .contains("expected ACP session Yolo capability"));
            assert_eq!(
                *h.seen_config_updates.lock().unwrap(),
                vec![("mode".to_string(), "bypassPermissions".to_string())]
            );
        })
        .await;
}

// ── inbound Client-trait routing (session_notification / request_permission) ──

/// Build a bare `WtaClient` (no agent connection) plus the `AppEvent` receiver
/// its handlers write to. Lets us drive the inbound `Client` trait methods
/// directly and assert the `SessionUpdate → AppEvent` translation without
/// spinning up the ACP I/O loop.
fn bare_client() -> (WtaClient, mpsc::UnboundedReceiver<AppEvent>) {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let state = Arc::new(ClientState {
        event_tx,
        shell_mgr: Arc::new(ShellManager::new()),
        prompt_timing: Arc::new(PromptTimingState::default()),
        native_yolo: Arc::new(crate::protocol::acp::native_yolo::NativeYoloState::new()),
        yolo_state: Arc::new(Mutex::new(crate::app_contracts::YoloState::new(
            false, false,
        ))),
        provider_probe_capture: ProviderProbeCapture::default(),
        standard_usage_sessions: Mutex::new(HashSet::new()),
        proposal_channels: Arc::new(
            crate::agent_tools::action_proposal::channel::ProposalChannelManager::new(),
        ),
        hidden_tool_calls: Mutex::new(HashMap::new()),
    });
    (WtaClient { state }, event_rx)
}

fn notif(
    sid: &str,
    update: acp::schema::v1::SessionUpdate,
) -> acp::schema::v1::SessionNotification {
    acp::schema::v1::SessionNotification::new(acp::schema::v1::SessionId::new(sid), update)
}

#[derive(Clone)]
struct SharedLogWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for SharedLogWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// An `AgentThoughtChunk` update becomes an `AgentThoughtChunk` event carrying
/// the session id and the chunk text.
#[tokio::test]
async fn session_notification_routes_agent_thought_chunk() {
    let (client, mut rx) = bare_client();
    client
        .session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::AgentThoughtChunk(acp::schema::v1::ContentChunk::new(
                "thinking".into(),
            )),
        ))
        .await
        .unwrap();
    match rx.try_recv() {
        Ok(AppEvent::AgentThoughtChunk { session_id, text }) => {
            assert_eq!(session_id, "s1");
            assert_eq!(text, "thinking");
        }
        _ => panic!("expected AgentThoughtChunk"),
    }
}

/// A `user_message_chunk` (only emitted during a `session/load` replay) becomes
/// a `UserMessageReplayChunk` event.
#[tokio::test]
async fn session_notification_routes_user_message_replay_chunk() {
    let (client, mut rx) = bare_client();
    client
        .session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::UserMessageChunk(
                acp::schema::v1::ContentChunk::new("prior prompt".into())
                    .message_id("prior-message"),
            ),
        ))
        .await
        .unwrap();
    match rx.try_recv() {
        Ok(AppEvent::UserMessageReplayChunk {
            session_id,
            message_id,
            text,
        }) => {
            assert_eq!(session_id, "s1");
            assert_eq!(message_id.as_deref(), Some("prior-message"));
            assert_eq!(text, "prior prompt");
        }
        _ => panic!("expected UserMessageReplayChunk"),
    }
}

#[tokio::test]
async fn session_notification_routes_usage_update() {
    let (client, mut rx) = bare_client();
    let usage = acp::schema::v1::UsageUpdate::new(1_024, 8_192)
        .cost(acp::schema::v1::Cost::new(0.004, "USD"));
    client
        .session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::UsageUpdate(usage),
        ))
        .await
        .unwrap();

    match rx.try_recv() {
        Ok(AppEvent::UsageReported {
            session_id,
            snapshot,
        }) => {
            assert_eq!(session_id, "s1");
            assert_eq!(
                snapshot.context,
                Some(crate::usage::UsageContext {
                    used: 1_024,
                    size: 8_192,
                })
            );
            assert_eq!(snapshot.cost.expect("cost").currency, "USD");
        }
        _ => panic!("expected UsageReported"),
    }
}

#[tokio::test]
async fn session_notification_routes_model_config_update() {
    let (client, mut rx) = bare_client();
    let update: acp::schema::v1::SessionUpdate = serde_json::from_value(serde_json::json!({
        "sessionUpdate": "config_option_update",
        "configOptions": [{
            "id": "model",
            "name": "Model",
            "category": "model",
            "type": "select",
            "currentValue": "gpt-5.6-sol",
            "options": [
                {"value": "claude-sonnet-5", "name": "Claude Sonnet 5"},
                {"value": "gpt-5.6-sol", "name": "GPT-5.6 Sol"}
            ]
        }]
    }))
    .unwrap();

    client
        .session_notification(notif("s1", update))
        .await
        .unwrap();

    match rx.try_recv() {
        Ok(AppEvent::SessionConfigUpdated {
            session_id,
            options,
        }) => {
            assert_eq!(session_id, "s1");
            assert_eq!(options.len(), 1);
            assert_eq!(options[0].id, "model");
            assert_eq!(options[0].current_value, "gpt-5.6-sol");
        }
        _ => panic!("expected SessionConfigUpdated"),
    }
    match rx.try_recv() {
        Ok(AppEvent::ModelConfigUpdated {
            session_id,
            available_models,
            current_model_id,
        }) => {
            assert_eq!(session_id, "s1");
            assert_eq!(current_model_id.as_deref(), Some("gpt-5.6-sol"));
            assert_eq!(
                available_models
                    .iter()
                    .map(|model| model.id.as_str())
                    .collect::<Vec<_>>(),
                vec!["claude-sonnet-5", "gpt-5.6-sol"]
            );
        }
        _ => panic!("expected ModelConfigUpdated"),
    }
}

#[tokio::test]
async fn session_notification_routes_provider_reported_zero_size() {
    let (client, mut rx) = bare_client();
    client
        .session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::UsageUpdate(acp::schema::v1::UsageUpdate::new(1, 0)),
        ))
        .await
        .expect("provider-owned capacity must not be rejected by the client");

    assert!(matches!(
        rx.try_recv(),
        Ok(AppEvent::UsageReported { session_id, snapshot })
            if session_id == "s1"
                && snapshot.context == Some(crate::usage::UsageContext { used: 1, size: 0 })
    ));
}

#[tokio::test]
async fn notification_dispatch_routes_over_capacity_usage_and_keeps_chat_flow() {
    let (client, mut rx) = bare_client();
    client
        .dispatch_session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::UsageUpdate(acp::schema::v1::UsageUpdate::new(
                101, 100,
            )),
        ))
        .await;

    assert!(matches!(
        rx.try_recv(),
        Ok(AppEvent::UsageReported { session_id, snapshot })
            if session_id == "s1"
                && snapshot.context == Some(crate::usage::UsageContext { used: 101, size: 100 })
    ));

    client
        .dispatch_session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::AgentMessageChunk(acp::schema::v1::ContentChunk::new(
                "still connected".into(),
            )),
        ))
        .await;

    assert!(matches!(
        rx.try_recv(),
        Ok(AppEvent::AgentMessageChunk { session_id, text })
            if session_id == "s1" && text == "still connected"
    ));
}

#[tokio::test]
async fn invalid_optional_cost_preserves_context_without_logging_values() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || SharedLogWriter(writer.clone()))
        .finish();
    let _subscriber_guard = tracing::subscriber::set_default(subscriber);

    let (client, mut rx) = bare_client();
    client
        .dispatch_session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::UsageUpdate(
                acp::schema::v1::UsageUpdate::new(123_456_789, 987_654_321)
                    .cost(acp::schema::v1::Cost::new(-1.0, "USD")),
            ),
        ))
        .await;
    assert!(matches!(
        rx.try_recv(),
        Ok(AppEvent::UsageReported { snapshot, .. })
            if snapshot.context == Some(crate::usage::UsageContext {
                used: 123_456_789,
                size: 987_654_321,
            }) && snapshot.cost.is_none()
    ));

    let logs = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
    assert!(!logs.contains("987654321"));
    assert!(!logs.contains("123456789"));
}

#[tokio::test]
async fn session_notification_marks_master_attested_native_yolo_config() {
    let (client, mut rx) = bare_client();
    client
        .state
        .native_yolo
        .set_resolved_agent_id(Some(crate::agent_registry::CLAUDE_AGENT_ID));
    let response: acp::schema::v1::NewSessionResponse = serde_json::from_value(serde_json::json!({
        "sessionId": "native-config-session",
        "configOptions": [{
            "id": "mode",
            "name": "Mode",
            "category": "mode",
            "type": "select",
            "currentValue": "default",
            "options": [
                {"value": "default", "name": "Default"},
                {"value": "bypassPermissions", "name": "Bypass Permissions"}
            ]
        }]
    }))
    .unwrap();
    client.state.native_yolo.record_from_new_session(&response);
    let update: acp::schema::v1::SessionUpdate = serde_json::from_value(serde_json::json!({
        "sessionUpdate": "config_option_update",
        "configOptions": [{
            "id": "mode",
            "name": "Mode",
            "category": "mode",
            "type": "select",
            "currentValue": "default",
            "options": [
                {"value": "default", "name": "Default"},
                {"value": "bypassPermissions", "name": "Bypass Permissions"}
            ]
        }]
    }))
    .unwrap();

    client
        .session_notification(notif("native-config-session", update))
        .await
        .unwrap();

    match rx.try_recv() {
        Ok(AppEvent::SessionConfigUpdated { options, .. }) => {
            assert_eq!(options.len(), 1);
            assert!(options[0].native_yolo);
        }
        _ => panic!("expected SessionConfigUpdated"),
    }
}

#[tokio::test]
async fn session_notification_clears_removed_model_config() {
    let (client, mut rx) = bare_client();
    let update: acp::schema::v1::SessionUpdate = serde_json::from_value(serde_json::json!({
        "sessionUpdate": "config_option_update",
        "configOptions": [{
            "id": "mode",
            "name": "Mode",
            "category": "mode",
            "type": "select",
            "currentValue": "auto",
            "options": [{"value": "auto", "name": "Auto"}]
        }]
    }))
    .unwrap();

    client
        .session_notification(notif("s1", update))
        .await
        .unwrap();

    match rx.try_recv() {
        Ok(AppEvent::SessionConfigUpdated {
            session_id,
            options,
        }) => {
            assert_eq!(session_id, "s1");
            assert_eq!(options.len(), 1);
            assert_eq!(options[0].id, "mode");
        }
        _ => panic!("expected SessionConfigUpdated"),
    }
    match rx.try_recv() {
        Ok(AppEvent::ModelConfigUpdated {
            session_id,
            available_models,
            current_model_id,
        }) => {
            assert_eq!(session_id, "s1");
            assert!(available_models.is_empty());
            assert_eq!(current_model_id, None);
        }
        _ => panic!("expected ModelConfigUpdated"),
    }
}

/// A `ToolCall` update becomes a `ToolCall` event with the tool id and title.
#[tokio::test]
async fn session_notification_routes_tool_call() {
    let (client, mut rx) = bare_client();
    client
        .session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::ToolCall(acp::schema::v1::ToolCall::new(
                acp::schema::v1::ToolCallId::new("tc-1"),
                "Run: echo hi",
            )),
        ))
        .await
        .unwrap();
    match rx.try_recv() {
        Ok(AppEvent::ToolCall {
            session_id,
            id,
            title,
            status,
            ..
        }) => {
            assert_eq!(session_id, "s1");
            assert_eq!(id, "tc-1");
            assert_eq!(title, "Run: echo hi");
            assert!(!status.is_empty(), "status should be a rendered enum name");
        }
        _ => panic!("expected ToolCall"),
    }
}

#[tokio::test]
async fn session_notification_hides_proposal_tool_call_before_permission() {
    let (client, mut rx) = bare_client();
    let command = r#"& "$env:WTA_CLI_PATH" propose-terminal-actions --channel v1.helper.turn --payload-json '{}'"#;
    client
        .session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::ToolCall(
                acp::schema::v1::ToolCall::new(
                    acp::schema::v1::ToolCallId::new("proposal-tool"),
                    "Propose terminal action",
                )
                .raw_input(Some(serde_json::json!({
                    "command": command,
                }))),
            ),
        ))
        .await
        .unwrap();

    assert!(matches!(
        rx.try_recv(),
        Ok(AppEvent::HideToolCall { session_id, id })
            if session_id == "s1" && id == "proposal-tool"
    ));
    assert!(
        rx.try_recv().is_err(),
        "proposal ToolCall must not reach the chat UI"
    );

    client
        .session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::ToolCallUpdate(acp::schema::v1::ToolCallUpdate::new(
                acp::schema::v1::ToolCallId::new("proposal-tool"),
                acp::schema::v1::ToolCallUpdateFields::new()
                    .status(acp::schema::v1::ToolCallStatus::Completed),
            )),
        ))
        .await
        .unwrap();
    assert!(
        rx.try_recv().is_err(),
        "updates for a hidden proposal ToolCall must remain hidden"
    );
}

#[tokio::test]
async fn session_notification_hides_proposal_mcp_tool_call() {
    let (client, mut rx) = bare_client();
    client
        .session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::ToolCall(acp::schema::v1::ToolCall::new(
                acp::schema::v1::ToolCallId::new("proposal-mcp-tool"),
                "intellterm_0123456789abcdef/run_command_in_current_shell",
            )),
        ))
        .await
        .unwrap();

    assert!(matches!(
        rx.try_recv(),
        Ok(AppEvent::HideToolCall { session_id, id })
            if session_id == "s1" && id == "proposal-mcp-tool"
    ));
    assert!(
        rx.try_recv().is_err(),
        "session MCP ToolCall must not reach the chat UI"
    );
}

/// When the agent's own `title` already embeds the location text (common
/// for read/view tool calls, e.g. title "Viewing C:\...\rust-app" whose
/// `locations` names that exact same path), the hint must be suppressed —
/// otherwise the card renders the path twice on one line: "Viewing X (X)".
#[tokio::test]
async fn session_notification_tool_call_omits_location_already_in_title() {
    let (client, mut rx) = bare_client();
    client
        .session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::ToolCall(
                acp::schema::v1::ToolCall::new(
                    acp::schema::v1::ToolCallId::new("tc-1"),
                    r"Viewing C:\src\rust-app",
                )
                .locations(vec![acp::schema::v1::ToolCallLocation::new(
                    r"C:\src\rust-app",
                )]),
            ),
        ))
        .await
        .unwrap();
    match rx.try_recv() {
        Ok(AppEvent::ToolCall { location, .. }) => {
            assert_eq!(location, None);
        }
        _ => panic!("expected ToolCall"),
    }
}

/// A `ToolCall` whose `locations` names a file surfaces that path as the
/// event's `location` — this is what lets the chat card show *what* a
/// generically-titled permission/read tool call actually touched (the bug
/// this test guards: `client.rs` used to drop `locations`/`raw_input`
/// entirely and only forward `title`/`status`).
#[tokio::test]
async fn session_notification_tool_call_surfaces_location_from_locations() {
    let (client, mut rx) = bare_client();
    client
        .session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::ToolCall(
                acp::schema::v1::ToolCall::new(
                    acp::schema::v1::ToolCallId::new("tc-1"),
                    "Access paths outside trusted directories",
                )
                .locations(vec![acp::schema::v1::ToolCallLocation::new(
                    r"C:\src\rust-app",
                )]),
            ),
        ))
        .await
        .unwrap();
    match rx.try_recv() {
        Ok(AppEvent::ToolCall { location, .. }) => {
            assert_eq!(location.as_deref(), Some(r"C:\src\rust-app"));
        }
        _ => panic!("expected ToolCall"),
    }
}

#[tokio::test]
async fn session_notification_preserves_standard_tool_content_and_location_lines() {
    let (client, mut rx) = bare_client();
    let content = vec![
        acp::schema::v1::Diff::new(r"C:\src\main.rs", "new line")
            .old_text("old line")
            .into(),
        acp::schema::v1::ToolCallContent::Terminal(acp::schema::v1::Terminal::new("term-1")),
        acp::schema::v1::ToolCallContent::Content(acp::schema::v1::Content::new(
            acp::schema::v1::ContentBlock::Image(acp::schema::v1::ImageContent::new(
                "base64",
                "image/png",
            )),
        )),
    ];
    client
        .session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::ToolCall(
                acp::schema::v1::ToolCall::new(
                    acp::schema::v1::ToolCallId::new("tc-typed"),
                    "Edit source",
                )
                .content(content)
                .locations(vec![acp::schema::v1::ToolCallLocation::new(
                    r"C:\src\main.rs",
                )
                .line(42)]),
            ),
        ))
        .await
        .unwrap();

    match rx.try_recv() {
        Ok(AppEvent::ToolCall {
            location,
            content,
            locations,
            ..
        }) => {
            assert_eq!(location.as_deref(), Some(r"C:\src\main.rs:42"));
            assert_eq!(locations[0].line, Some(42));
            assert!(matches!(
                &content[0],
                crate::app::ToolCallContent::Diff {
                    path,
                    old_text: Some(old_text),
                    new_text,
                } if path == r"C:\src\main.rs"
                    && old_text.text == "old line"
                    && new_text.text == "new line"
            ));
            assert!(matches!(
                &content[1],
                crate::app::ToolCallContent::Terminal { id, .. } if id == "term-1"
            ));
            assert!(matches!(
                &content[2],
                crate::app::ToolCallContent::Attachment { label, .. }
                    if label == "image/png"
            ));
        }
        _ => panic!("expected typed ToolCall"),
    }
}

/// When `locations` is empty (typical for `execute` tool calls), the
/// `location` hint falls back to `raw_input.command` so the card can still
/// show what's actually being run.
#[tokio::test]
async fn session_notification_tool_call_surfaces_location_from_raw_input_command() {
    let (client, mut rx) = bare_client();
    client
        .session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::ToolCall(
                acp::schema::v1::ToolCall::new(
                    acp::schema::v1::ToolCallId::new("tc-1"),
                    "Read main.rs",
                )
                .raw_input(Some(serde_json::json!({
                    "command": "Get-Content 'C:\\rust-app\\src\\main.rs'",
                }))),
            ),
        ))
        .await
        .unwrap();
    match rx.try_recv() {
        Ok(AppEvent::ToolCall { location, .. }) => {
            assert_eq!(
                location.as_deref(),
                Some("Get-Content 'C:\\rust-app\\src\\main.rs'")
            );
        }
        _ => panic!("expected ToolCall"),
    }
}

#[tokio::test]
async fn session_notification_tool_call_ignores_empty_raw_input_target() {
    let (client, mut rx) = bare_client();
    client
        .session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::ToolCall(
                acp::schema::v1::ToolCall::new(
                    acp::schema::v1::ToolCallId::new("tc-1"),
                    "Run command",
                )
                .raw_input(Some(serde_json::json!({
                    "command": "",
                    "path": "   ",
                }))),
            ),
        ))
        .await
        .unwrap();

    match rx.try_recv() {
        Ok(AppEvent::ToolCall { location, .. }) => assert_eq!(location, None),
        _ => panic!("expected ToolCall"),
    }
}

#[tokio::test]
async fn session_notification_tool_call_truncates_long_target_on_char_boundary() {
    let (client, mut rx) = bare_client();
    client
        .session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::ToolCall(
                acp::schema::v1::ToolCall::new(
                    acp::schema::v1::ToolCallId::new("tc-1"),
                    "Run command",
                )
                .raw_input(Some(serde_json::json!({
                    "command": "界".repeat(10_000),
                }))),
            ),
        ))
        .await
        .unwrap();

    match rx.try_recv() {
        Ok(AppEvent::ToolCall {
            location: Some(location),
            ..
        }) => {
            assert_eq!(location.chars().count(), 201);
            assert!(location.ends_with('…'));
        }
        _ => panic!("expected ToolCall with location"),
    }
}

#[tokio::test]
async fn session_notification_tool_call_dedupes_long_target_by_visible_prefix() {
    let (client, mut rx) = bare_client();
    let visible_prefix = "A".repeat(200);
    client
        .session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::ToolCall(
                acp::schema::v1::ToolCall::new(
                    acp::schema::v1::ToolCallId::new("tc-1"),
                    format!("Run {visible_prefix}"),
                )
                .raw_input(Some(serde_json::json!({
                    "command": format!("{visible_prefix}{}", "B".repeat(10_000)),
                }))),
            ),
        ))
        .await
        .unwrap();

    match rx.try_recv() {
        Ok(AppEvent::ToolCall { location, .. }) => assert_eq!(location, None),
        _ => panic!("expected ToolCall"),
    }
}

/// A `ToolCallUpdate` carrying only a status becomes a `ToolCallUpdate` event
/// whose status string is the rendered enum name.
#[tokio::test]
async fn session_notification_routes_tool_call_update_status_only() {
    let (client, mut rx) = bare_client();
    client
        .session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::ToolCallUpdate(acp::schema::v1::ToolCallUpdate::new(
                acp::schema::v1::ToolCallId::new("tc-1"),
                acp::schema::v1::ToolCallUpdateFields::new()
                    .status(acp::schema::v1::ToolCallStatus::Completed),
            )),
        ))
        .await
        .unwrap();
    match rx.try_recv() {
        Ok(AppEvent::ToolCallUpdate {
            session_id,
            id,
            status,
            ..
        }) => {
            assert_eq!(session_id, "s1");
            assert_eq!(id, "tc-1");
            assert_eq!(status.as_deref(), Some("Completed"));
        }
        _ => panic!("expected ToolCallUpdate"),
    }
}

#[tokio::test]
async fn session_notification_routes_tool_call_content_without_status() {
    let (client, mut rx) = bare_client();
    client
        .session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::ToolCallUpdate(acp::schema::v1::ToolCallUpdate::new(
                acp::schema::v1::ToolCallId::new("tc-1"),
                acp::schema::v1::ToolCallUpdateFields::new().content(vec!["step 1 of 3".into()]),
            )),
        ))
        .await
        .unwrap();

    match rx.try_recv() {
        Ok(AppEvent::ToolCallUpdate { status, output, .. }) => {
            assert_eq!(status, None);
            assert_eq!(output.expect("expected text content").text, "step 1 of 3");
        }
        _ => panic!("expected content-only ToolCallUpdate"),
    }
}

#[tokio::test]
async fn session_notification_preserves_empty_tool_content_replacement() {
    let (client, mut rx) = bare_client();
    client
        .session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::ToolCallUpdate(acp::schema::v1::ToolCallUpdate::new(
                acp::schema::v1::ToolCallId::new("tc-1"),
                acp::schema::v1::ToolCallUpdateFields::new().content(Vec::new()),
            )),
        ))
        .await
        .unwrap();

    match rx.try_recv() {
        Ok(AppEvent::ToolCallUpdate {
            content: Some(content),
            output: Some(output),
            ..
        }) => {
            assert!(content.is_empty());
            assert!(output.text.is_empty());
        }
        _ => panic!("expected empty content replacement"),
    }
}

#[tokio::test]
async fn session_notification_bounds_tool_call_output_to_latest_text() {
    let (client, mut rx) = bare_client();
    let reported = format!("{}TAIL", "x".repeat(5000));
    client
        .session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::ToolCallUpdate(acp::schema::v1::ToolCallUpdate::new(
                acp::schema::v1::ToolCallId::new("tc-1"),
                acp::schema::v1::ToolCallUpdateFields::new().content(vec![reported.into()]),
            )),
        ))
        .await
        .unwrap();

    match rx.try_recv() {
        Ok(AppEvent::ToolCallUpdate {
            output: Some(output),
            ..
        }) => {
            assert!(output.truncated);
            assert_eq!(output.text.chars().count(), 4000);
            assert!(output.text.ends_with("TAIL"));
        }
        _ => panic!("expected bounded ToolCallUpdate output"),
    }
}

#[tokio::test]
async fn session_notification_surfaces_execute_metadata_and_raw_output() {
    let (client, mut rx) = bare_client();
    let expected_cwd = concat!("C:", "\\", "repo");
    client
        .session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::ToolCall(
                acp::schema::v1::ToolCall::new(acp::schema::v1::ToolCallId::new("tc-1"), "bash")
                    .kind(acp::schema::v1::ToolKind::Execute)
                    .status(acp::schema::v1::ToolCallStatus::Completed)
                    .raw_input(Some(serde_json::json!({
                        "command": "cargo test",
                        "cwd": expected_cwd
                    })))
                    .raw_output(Some(serde_json::json!({
                        "stdout": "12 tests passed",
                        "stderr": "one warning",
                        "exitCode": 0
                    }))),
            ),
        ))
        .await
        .unwrap();

    match rx.try_recv() {
        Ok(AppEvent::ToolCall {
            kind,
            location,
            location_is_command,
            cwd,
            output,
            exit_code,
            ..
        }) => {
            assert_eq!(kind, crate::app::ToolCallKind::Execute);
            assert_eq!(location.as_deref(), Some("cargo test"));
            assert!(location_is_command);
            assert_eq!(cwd.as_deref(), Some(expected_cwd));
            assert_eq!(
                output.expect("expected reported output").text,
                "12 tests passed\none warning"
            );
            assert_eq!(exit_code, Some(0));
        }
        _ => panic!("expected execute ToolCall"),
    }
}

/// A failed `ToolCallUpdate` that carries a `raw_output.message` surfaces that
/// reason appended to the status, so the chat shows *why* a tool call failed
/// instead of a bare "Failed".
#[tokio::test]
async fn session_notification_tool_call_update_surfaces_raw_output_message() {
    let (client, mut rx) = bare_client();
    client
        .session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::ToolCallUpdate(acp::schema::v1::ToolCallUpdate::new(
                acp::schema::v1::ToolCallId::new("tc-1"),
                acp::schema::v1::ToolCallUpdateFields::new()
                    .status(acp::schema::v1::ToolCallStatus::Failed)
                    .raw_output(serde_json::json!({
                        "message": "The user rejected this tool call."
                    })),
            )),
        ))
        .await
        .unwrap();
    match rx.try_recv() {
        Ok(AppEvent::ToolCallUpdate { status, .. }) => {
            let status = status.as_deref().expect("expected status update");
            assert!(status.contains("Failed"), "got: {status}");
            assert!(
                status.contains("The user rejected this tool call."),
                "the raw_output reason must be surfaced; got: {status}"
            );
        }
        _ => panic!("expected ToolCallUpdate"),
    }
}

#[tokio::test]
async fn session_notification_tool_call_update_surfaces_stdout_and_nonzero_exit() {
    let (client, mut rx) = bare_client();
    client
        .session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::ToolCallUpdate(acp::schema::v1::ToolCallUpdate::new(
                acp::schema::v1::ToolCallId::new("tc-1"),
                acp::schema::v1::ToolCallUpdateFields::new()
                    .status(acp::schema::v1::ToolCallStatus::Completed)
                    .raw_output(serde_json::json!({
                        "stdout": "TOOL_OUTPUT_MARKER",
                        "exitCode": 7
                    })),
            )),
        ))
        .await
        .unwrap();

    match rx.try_recv() {
        Ok(AppEvent::ToolCallUpdate {
            output, exit_code, ..
        }) => {
            assert_eq!(
                output.expect("expected stdout update").text,
                "TOOL_OUTPUT_MARKER"
            );
            assert_eq!(exit_code, Some(7));
        }
        _ => panic!("expected ToolCallUpdate"),
    }
}

/// A `ToolCallUpdate` with no supported fields is dropped.
#[tokio::test]
async fn session_notification_tool_call_update_without_status_is_dropped() {
    let (client, mut rx) = bare_client();
    client
        .session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::ToolCallUpdate(acp::schema::v1::ToolCallUpdate::new(
                acp::schema::v1::ToolCallId::new("tc-1"),
                acp::schema::v1::ToolCallUpdateFields::new(),
            )),
        ))
        .await
        .unwrap();
    assert!(
        rx.try_recv().is_err(),
        "a status-less ToolCallUpdate must not emit an event"
    );
}

/// A `Plan` update becomes a `Plan` event whose entries preserve content and
/// map each ACP status onto the app's `PlanEntryStatus`.
#[tokio::test]
async fn session_notification_routes_plan_with_status_mapping() {
    let (client, mut rx) = bare_client();
    client
        .session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::Plan(acp::schema::v1::Plan::new(vec![
                acp::schema::v1::PlanEntry::new(
                    "Step one",
                    acp::schema::v1::PlanEntryPriority::Medium,
                    acp::schema::v1::PlanEntryStatus::InProgress,
                ),
                acp::schema::v1::PlanEntry::new(
                    "Step two",
                    acp::schema::v1::PlanEntryPriority::Low,
                    acp::schema::v1::PlanEntryStatus::Completed,
                ),
                acp::schema::v1::PlanEntry::new(
                    "Step three",
                    acp::schema::v1::PlanEntryPriority::Low,
                    acp::schema::v1::PlanEntryStatus::Pending,
                ),
            ])),
        ))
        .await
        .unwrap();
    match rx.try_recv() {
        Ok(AppEvent::Plan {
            session_id,
            entries,
        }) => {
            assert_eq!(session_id, "s1");
            assert_eq!(
                entries,
                vec![
                    PlanEntry {
                        content: "Step one".to_string(),
                        status: PlanEntryStatus::InProgress,
                    },
                    PlanEntry {
                        content: "Step two".to_string(),
                        status: PlanEntryStatus::Completed,
                    },
                    PlanEntry {
                        content: "Step three".to_string(),
                        status: PlanEntryStatus::Pending,
                    },
                ]
            );
        }
        _ => panic!("expected Plan"),
    }
}

#[tokio::test]
async fn session_notification_routes_complete_available_command_snapshot() {
    let (client, mut rx) = bare_client();
    let commands = vec![
        acp::schema::v1::AvailableCommand::new("plan", "Build a plan"),
        acp::schema::v1::AvailableCommand::new("review", "Review changes").input(
            acp::schema::v1::AvailableCommandInput::Unstructured(
                acp::schema::v1::UnstructuredCommandInput::new("focus area"),
            ),
        ),
    ];
    client
        .session_notification(notif(
            "s1",
            acp::schema::v1::SessionUpdate::AvailableCommandsUpdate(
                acp::schema::v1::AvailableCommandsUpdate::new(commands),
            ),
        ))
        .await
        .unwrap();

    match rx.try_recv() {
        Ok(AppEvent::SessionCommandsUpdated {
            session_id,
            commands,
        }) => {
            assert_eq!(session_id, "s1");
            assert_eq!(
                commands,
                vec![
                    crate::app_contracts::AcpSessionCommand {
                        name: "plan".into(),
                        description: "Build a plan".into(),
                        input_hint: None,
                        completion_behavior:
                            crate::app_contracts::CompletionBehavior::ExecuteImmediately,
                    },
                    crate::app_contracts::AcpSessionCommand {
                        name: "review".into(),
                        description: "Review changes".into(),
                        input_hint: Some("focus area".into()),
                        completion_behavior:
                            crate::app_contracts::CompletionBehavior::OptionalFreeText,
                    },
                ]
            );
        }
        _ => panic!("expected SessionCommandsUpdated"),
    }
}

fn permission_request(sid: &str) -> acp::schema::v1::RequestPermissionRequest {
    acp::schema::v1::RequestPermissionRequest::new(
        acp::schema::v1::SessionId::new(sid),
        acp::schema::v1::ToolCallUpdate::new(
            acp::schema::v1::ToolCallId::new("mock-tool-1"),
            acp::schema::v1::ToolCallUpdateFields::new().title("Run: echo hi"),
        ),
        vec![acp::schema::v1::PermissionOption::new(
            acp::schema::v1::PermissionOptionId::new("allow-once"),
            "Allow once",
            acp::schema::v1::PermissionOptionKind::AllowOnce,
        )],
    )
}

/// When the tool call carries a `locations` path, `request_permission`
/// appends it to the description so the dialog is actionable — without
/// this, generic titles like "Access paths outside trusted directories"
/// give the user no idea what path is actually being requested.
#[tokio::test]
async fn request_permission_description_includes_location_from_locations() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (client, mut rx) = bare_client();
            let req = acp::schema::v1::RequestPermissionRequest::new(
                acp::schema::v1::SessionId::new("s1"),
                acp::schema::v1::ToolCallUpdate::new(
                    acp::schema::v1::ToolCallId::new("mock-tool-1"),
                    acp::schema::v1::ToolCallUpdateFields::new()
                        .title("Access paths outside trusted directories")
                        .locations(vec![acp::schema::v1::ToolCallLocation::new(
                            r"C:\src\rust-app",
                        )]),
                ),
                vec![acp::schema::v1::PermissionOption::new(
                    acp::schema::v1::PermissionOptionId::new("allow-once"),
                    "Allow once",
                    acp::schema::v1::PermissionOptionKind::AllowOnce,
                )],
            );
            let handle =
                tokio::task::spawn_local(async move { client.request_permission(req).await });

            match rx.recv().await {
                Some(AppEvent::PermissionRequest {
                    description,
                    responder,
                    ..
                }) => {
                    assert_eq!(
                        description,
                        r"Access paths outside trusted directories (C:\src\rust-app)"
                    );
                    responder.send("allow-once".to_string()).unwrap();
                }
                _ => panic!("expected PermissionRequest"),
            }
            handle.await.unwrap().unwrap();
        })
        .await;
}

/// The full-card `target`/`target_is_command`/`kind_label` fields must
/// surface the concrete path even when it repeats the title verbatim —
/// unlike the chat `ToolCall` card, the permission dialog is a decision
/// point and must never silently drop the target via dedup.
#[tokio::test]
async fn request_permission_target_is_not_deduped_against_title() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (client, mut rx) = bare_client();
            let req = acp::schema::v1::RequestPermissionRequest::new(
                acp::schema::v1::SessionId::new("s1"),
                acp::schema::v1::ToolCallUpdate::new(
                    acp::schema::v1::ToolCallId::new("mock-tool-1"),
                    acp::schema::v1::ToolCallUpdateFields::new()
                        .title(r"Viewing C:\src\rust-app")
                        .kind(acp::schema::v1::ToolKind::Read)
                        .locations(vec![acp::schema::v1::ToolCallLocation::new(
                            r"C:\src\rust-app",
                        )]),
                ),
                vec![acp::schema::v1::PermissionOption::new(
                    acp::schema::v1::PermissionOptionId::new("allow-once"),
                    "Allow once",
                    acp::schema::v1::PermissionOptionKind::AllowOnce,
                )],
            );
            let handle =
                tokio::task::spawn_local(async move { client.request_permission(req).await });

            match rx.recv().await {
                Some(AppEvent::PermissionRequest {
                    target,
                    target_is_command,
                    kind_label,
                    responder,
                    ..
                }) => {
                    assert_eq!(
                        target.as_deref(),
                        Some(r"C:\src\rust-app"),
                        "target must be present even though the title already contains it"
                    );
                    assert!(!target_is_command);
                    assert_eq!(kind_label.as_deref(), Some("→"));
                    responder.send("allow-once".to_string()).unwrap();
                }
                _ => panic!("expected PermissionRequest"),
            }
            handle.await.unwrap().unwrap();
        })
        .await;
}

/// An `execute`-kind tool call's target is flagged as a command (not a
/// path) so the permission card can render it with the `$ ` shell-prompt
/// prefix instead of as a plain path.
#[tokio::test]
async fn request_permission_execute_kind_marks_target_as_command() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (client, mut rx) = bare_client();
            let req = acp::schema::v1::RequestPermissionRequest::new(
                acp::schema::v1::SessionId::new("s1"),
                acp::schema::v1::ToolCallUpdate::new(
                    acp::schema::v1::ToolCallId::new("mock-tool-1"),
                    acp::schema::v1::ToolCallUpdateFields::new()
                        .title("Run command")
                        .kind(acp::schema::v1::ToolKind::Execute)
                        .raw_input(Some(serde_json::json!({ "command": "rm -rf build" }))),
                ),
                vec![acp::schema::v1::PermissionOption::new(
                    acp::schema::v1::PermissionOptionId::new("allow-once"),
                    "Allow once",
                    acp::schema::v1::PermissionOptionKind::AllowOnce,
                )],
            );
            let handle =
                tokio::task::spawn_local(async move { client.request_permission(req).await });

            match rx.recv().await {
                Some(AppEvent::PermissionRequest {
                    target,
                    target_is_command,
                    kind_label,
                    responder,
                    ..
                }) => {
                    assert_eq!(target.as_deref(), Some("rm -rf build"));
                    assert!(target_is_command);
                    assert_eq!(kind_label.as_deref(), Some("$"));
                    responder.send("allow-once".to_string()).unwrap();
                }
                _ => panic!("expected PermissionRequest"),
            }
            handle.await.unwrap().unwrap();
        })
        .await;
}

/// Resolver commands follow the same permission flow as every other execute
/// request; receiving the event proves no built-in bypass selected an option.
#[tokio::test]
async fn request_permission_resolver_uses_normal_permission_flow() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (client, mut rx) = bare_client();
            let req = acp::schema::v1::RequestPermissionRequest::new(
                acp::schema::v1::SessionId::new("s1"),
                acp::schema::v1::ToolCallUpdate::new(
                    acp::schema::v1::ToolCallId::new("resolver-tool"),
                    acp::schema::v1::ToolCallUpdateFields::new()
                        .title("Resolve command")
                        .kind(acp::schema::v1::ToolKind::Execute)
                        .raw_input(Some(serde_json::json!({
                            "command": "wta.exe",
                            "args": [
                                "resolve-command",
                                "git",
                                "--shell",
                                "cmd.exe",
                                "--cwd",
                                r"C:\workspace",
                                "--json"
                            ],
                        }))),
                ),
                vec![acp::schema::v1::PermissionOption::new(
                    acp::schema::v1::PermissionOptionId::new("allow-once"),
                    "Allow once",
                    acp::schema::v1::PermissionOptionKind::AllowOnce,
                )],
            );
            let handle =
                tokio::task::spawn_local(async move { client.request_permission(req).await });

            let responder = match rx.recv().await {
                Some(AppEvent::PermissionRequest { responder, .. }) => responder,
                _ => panic!("expected resolver PermissionRequest"),
            };
            responder.send("allow-once".to_string()).unwrap();

            let response = handle.await.unwrap().unwrap();
            assert!(matches!(
                response.outcome,
                acp::schema::v1::RequestPermissionOutcome::Selected(_)
            ));
        })
        .await;
}

/// `request_permission` surfaces a `PermissionRequest` event and, once the user
/// picks an option through the responder, returns `Selected(option_id)`.
#[tokio::test]
async fn request_permission_returns_selected_option() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (client, mut rx) = bare_client();
            let handle = tokio::task::spawn_local(async move {
                client.request_permission(permission_request("s1")).await
            });

            let responder = match rx.recv().await {
                Some(AppEvent::PermissionRequest {
                    session_id,
                    tool_call_id,
                    description,
                    options,
                    responder,
                    ..
                }) => {
                    assert_eq!(session_id, "s1");
                    assert_eq!(tool_call_id, "mock-tool-1");
                    assert_eq!(description, "Run: echo hi");
                    assert_eq!(options.len(), 1);
                    assert_eq!(options[0].id, "allow-once");
                    responder
                }
                _ => panic!("expected PermissionRequest"),
            };
            responder.send("allow-once".to_string()).unwrap();

            let resp = handle.await.unwrap().unwrap();
            match resp.outcome {
                acp::schema::v1::RequestPermissionOutcome::Selected(sel) => {
                    assert_eq!(sel.option_id.to_string(), "allow-once");
                }
                _ => panic!("expected Selected outcome"),
            }
        })
        .await;
}

/// If the responder is dropped without a choice (e.g. the pane closes), the
/// permission resolves as `Cancelled` rather than hanging.
#[tokio::test]
async fn request_permission_cancelled_when_responder_dropped() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (client, mut rx) = bare_client();
            let handle = tokio::task::spawn_local(async move {
                client.request_permission(permission_request("s1")).await
            });

            let responder = match rx.recv().await {
                Some(AppEvent::PermissionRequest { responder, .. }) => responder,
                _ => panic!("expected PermissionRequest"),
            };
            drop(responder);

            let resp = handle.await.unwrap().unwrap();
            assert!(matches!(
                resp.outcome,
                acp::schema::v1::RequestPermissionOutcome::Cancelled
            ));
        })
        .await;
}
// ── provider-native Yolo mode ────────────────────────────────────────────

#[tokio::test]
async fn request_permission_with_allow_once_and_always_waits_for_user() {
    let (client, mut rx) = bare_client();
    let req = acp::schema::v1::RequestPermissionRequest::new(
        acp::schema::v1::SessionId::new("s1"),
        acp::schema::v1::ToolCallUpdate::new(
            acp::schema::v1::ToolCallId::new("mock-tool-1"),
            acp::schema::v1::ToolCallUpdateFields::new().title("Run: echo hi"),
        ),
        vec![
            acp::schema::v1::PermissionOption::new(
                acp::schema::v1::PermissionOptionId::new("allow-once"),
                "Allow once",
                acp::schema::v1::PermissionOptionKind::AllowOnce,
            ),
            acp::schema::v1::PermissionOption::new(
                acp::schema::v1::PermissionOptionId::new("allow-always"),
                "Allow always",
                acp::schema::v1::PermissionOptionKind::AllowAlways,
            ),
        ],
    );
    tokio::task::LocalSet::new()
        .run_until(async move {
            let handle =
                tokio::task::spawn_local(async move { client.request_permission(req).await });
            match rx.recv().await {
                Some(AppEvent::PermissionRequest { responder, .. }) => {
                    assert!(
                        !handle.is_finished(),
                        "WTA must not choose AllowOnce or AllowAlways before the user responds"
                    );
                    tokio::task::yield_now().await;
                    assert!(!handle.is_finished());
                    responder.send("allow-once".to_string()).unwrap();
                }
                other => panic!(
                    "expected interactive PermissionRequest, got is_some={}",
                    other.is_some()
                ),
            }
            assert!(handle.await.unwrap().is_ok());
        })
        .await;
}

#[tokio::test]
async fn request_permission_with_only_allow_always_waits_for_user() {
    let (client, mut rx) = bare_client();
    let req = acp::schema::v1::RequestPermissionRequest::new(
        acp::schema::v1::SessionId::new("s1"),
        acp::schema::v1::ToolCallUpdate::new(
            acp::schema::v1::ToolCallId::new("mock-tool-1"),
            acp::schema::v1::ToolCallUpdateFields::new().title("Run: echo hi"),
        ),
        vec![acp::schema::v1::PermissionOption::new(
            acp::schema::v1::PermissionOptionId::new("allow-always"),
            "Allow always",
            acp::schema::v1::PermissionOptionKind::AllowAlways,
        )],
    );

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let handle =
                tokio::task::spawn_local(async move { client.request_permission(req).await });
            match rx.recv().await {
                Some(AppEvent::PermissionRequest { responder, .. }) => {
                    assert!(
                        !handle.is_finished(),
                        "WTA must not choose AllowAlways before the user responds"
                    );
                    tokio::task::yield_now().await;
                    assert!(!handle.is_finished());
                    responder.send("allow-always".to_string()).unwrap();
                }
                other => panic!(
                    "expected interactive PermissionRequest, got is_some={}",
                    other.is_some()
                ),
            }
            let response = handle.await.unwrap().unwrap();
            assert!(matches!(
                response.outcome,
                acp::schema::v1::RequestPermissionOutcome::Selected(_)
            ));
        })
        .await;
}
