use std::collections::{hash_map::DefaultHasher, HashMap};
use std::hash::{Hash, Hasher};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use windows_sys::Win32::Foundation::ERROR_PIPE_BUSY;

use crate::app::{
    AppEvent, ChatMessage, CompletedTurn, ConnectionState, DebugDir, DebugMessage, PermOption,
};
use crate::coordinator::{
    parse_recommendation_set, validate_recommendation_set_for_coordinator_target,
    RecommendationChoice, RecommendationSet,
};
use crate::protocol::acp::client::{prompt_timing_log, run_acp_client, PromptSubmission};
use crate::shell::wt_channel::ConnectionInfo;
use crate::shell::ShellManager;
use crate::ui_trace;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PaneContext {
    pub pane_id: Option<String>,
    pub tab_id: Option<String>,
    pub window_id: Option<String>,
    pub cwd: Option<String>,
    pub source_pane_id: Option<String>,
}

impl PaneContext {
    pub fn effective_source_pane_id(&self) -> Option<&str> {
        self.source_pane_id.as_deref().or(self.pane_id.as_deref())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PermissionPrompt {
    pub description: String,
    pub options: Vec<PermOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SharedStateSnapshot {
    pub version: u64,
    pub state: ConnectionState,
    pub agent_name: String,
    #[serde(default)]
    pub agent_model: Option<String>,
    #[serde(default)]
    pub prompt_name: Option<String>,
    #[serde(default)]
    pub progress_status: Option<String>,
    pub session_id: String,
    pub wt_connected: bool,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub completed_turns: Vec<CompletedTurn>,
    pub recommendations: Option<RecommendationSet>,
    pub agent_streaming: bool,
    #[serde(default)]
    pub pending_thought_response: String,
    #[serde(default)]
    pub pending_agent_response: String,
    pub prompt_in_flight: bool,
    #[serde(default)]
    pub timing_note: Option<String>,
    pub permission: Option<PermissionPrompt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostClientRequest {
    Attach {
        pane_context: PaneContext,
    },
    GetSnapshot,
    SubmitPrompt {
        prompt_id: u64,
        submitted_at_unix_s: f64,
        text: String,
        pane_context: Option<PaneContext>,
        #[serde(default)]
        is_autofix: bool,
    },
    SelectRecommendation {
        choice: usize,
    },
    /// Host-internal only (client_id == u64::MAX). Used when the user clicks
    /// the bottom-bar autofix icon / presses Ctrl+. while no attach TUI is
    /// running. Host picks the recommended choice from its state, auto-fills
    /// `parent` on Send actions with `source_pane_id`, runs it, and emits
    /// autofix_state:cleared.
    ExecuteArmedAutofix {
        source_pane_id: String,
    },
    RespondPermission {
        option_id: String,
    },
    PaneContextUpdate {
        pane_context: PaneContext,
    },
    Detach,
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostServerMessage {
    Attached {
        client_id: u64,
        snapshot: SharedStateSnapshot,
    },
    SharedStateSnapshot {
        snapshot: SharedStateSnapshot,
    },
    Event {
        event: SharedUiEvent,
    },
    Error {
        message: String,
    },
    Pong,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SharedUiEvent {
    ConnectionStage {
        stage: String,
    },
    ProgressStatus {
        message: String,
    },
    AgentConnected {
        name: String,
        #[serde(default)]
        model: Option<String>,
        session_id: String,
    },
    PromptTemplateLoaded {
        name: String,
    },
    AgentError {
        message: String,
    },
    ExecutionInfo {
        message: String,
    },
    UserMessage {
        text: String,
    },
    AgentThoughtChunk {
        text: String,
    },
    AgentMessageChunk {
        text: String,
    },
    AgentMessageEnd,
    TimingMetric {
        message: String,
    },
    ToolCall {
        id: String,
        title: String,
        status: String,
    },
    ToolCallUpdate {
        id: String,
        status: String,
    },
    Plan {
        entries: Vec<crate::app::PlanEntry>,
    },
    PermissionRequest {
        description: String,
        options: Vec<PermOption>,
    },
    PermissionCleared,
    SystemMessage {
        message: String,
    },
    WtEvent {
        method: String,
        pane_id: String,
        params: serde_json::Value,
    },
}

impl SharedUiEvent {
    fn from_app_event(event: &AppEvent) -> Option<Self> {
        match event {
            AppEvent::ConnectionStage(stage) => Some(Self::ConnectionStage {
                stage: stage.clone(),
            }),
            AppEvent::ProgressStatus(message) => Some(Self::ProgressStatus {
                message: message.clone(),
            }),
            AppEvent::AgentConnected {
                name,
                model,
                session_id,
            } => Some(Self::AgentConnected {
                name: name.clone(),
                model: model.clone(),
                session_id: session_id.clone(),
            }),
            AppEvent::PromptTemplateLoaded { name } => {
                Some(Self::PromptTemplateLoaded { name: name.clone() })
            }
            AppEvent::AgentError(message) => Some(Self::AgentError {
                message: message.clone(),
            }),
            AppEvent::ExecutionInfo(message) => Some(Self::ExecutionInfo {
                message: message.clone(),
            }),
            AppEvent::AgentThoughtChunk(text) => {
                Some(Self::AgentThoughtChunk { text: text.clone() })
            }
            AppEvent::AgentMessageChunk(text) => {
                Some(Self::AgentMessageChunk { text: text.clone() })
            }
            AppEvent::AgentMessageEnd => Some(Self::AgentMessageEnd),
            AppEvent::TimingMetric(message) => Some(Self::TimingMetric {
                message: message.clone(),
            }),
            AppEvent::ToolCall { id, title, status } => Some(Self::ToolCall {
                id: id.clone(),
                title: title.clone(),
                status: status.clone(),
            }),
            AppEvent::ToolCallUpdate { id, status } => Some(Self::ToolCallUpdate {
                id: id.clone(),
                status: status.clone(),
            }),
            AppEvent::Plan(entries) => Some(Self::Plan {
                entries: entries.clone(),
            }),
            AppEvent::PermissionRequest {
                description,
                options,
                ..
            } => Some(Self::PermissionRequest {
                description: description.clone(),
                options: options.clone(),
            }),
            AppEvent::SystemMessage(message) => Some(Self::SystemMessage {
                message: message.clone(),
            }),
            AppEvent::WtEvent {
                method,
                pane_id,
                params,
            } => Some(Self::WtEvent {
                method: method.clone(),
                pane_id: pane_id.clone(),
                params: params.clone(),
            }),
            AppEvent::Tick
            | AppEvent::Key(_)
            | AppEvent::Resize(_, _)
            | AppEvent::DebugPipeMessage(_)
            | AppEvent::SharedStateSnapshot(_)
            | AppEvent::SharedPermissionRequest { .. }
            | AppEvent::PermissionCleared => None,
            AppEvent::UserMessage(_) => None,
        }
    }

    fn into_app_event(self) -> AppEvent {
        match self {
            Self::ConnectionStage { stage } => AppEvent::ConnectionStage(stage),
            Self::ProgressStatus { message } => AppEvent::ProgressStatus(message),
            Self::AgentConnected {
                name,
                model,
                session_id,
            } => AppEvent::AgentConnected {
                name,
                model,
                session_id,
            },
            Self::PromptTemplateLoaded { name } => AppEvent::PromptTemplateLoaded { name },
            Self::AgentError { message } => AppEvent::AgentError(message),
            Self::ExecutionInfo { message } => AppEvent::ExecutionInfo(message),
            Self::UserMessage { text } => AppEvent::UserMessage(text),
            Self::AgentThoughtChunk { text } => AppEvent::AgentThoughtChunk(text),
            Self::AgentMessageChunk { text } => AppEvent::AgentMessageChunk(text),
            Self::AgentMessageEnd => AppEvent::AgentMessageEnd,
            Self::TimingMetric { message } => AppEvent::TimingMetric(message),
            Self::ToolCall { id, title, status } => AppEvent::ToolCall { id, title, status },
            Self::ToolCallUpdate { id, status } => AppEvent::ToolCallUpdate { id, status },
            Self::Plan { entries } => AppEvent::Plan(entries),
            Self::PermissionRequest {
                description,
                options,
            } => AppEvent::SharedPermissionRequest {
                description,
                options,
            },
            Self::PermissionCleared => AppEvent::PermissionCleared,
            Self::SystemMessage { message } => AppEvent::SystemMessage(message),
            Self::WtEvent {
                method,
                pane_id,
                params,
            } => AppEvent::WtEvent {
                method,
                pane_id,
                params,
            },
        }
    }
}

fn normalize_command(command: Option<&str>) -> String {
    command
        .map(|cmd| cmd.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
}

pub fn pipe_name_for(
    pipe_info: Option<&ConnectionInfo>,
    agent_cmd: Option<&str>,
    delegate_agent_cmd: Option<&str>,
) -> String {
    let mut hasher = DefaultHasher::new();
    pipe_info
        .map(|info| info.pipe_name.as_str())
        .unwrap_or("local-only")
        .hash(&mut hasher);
    normalize_command(agent_cmd).hash(&mut hasher);
    normalize_command(delegate_agent_cmd).hash(&mut hasher);
    format!(r"\\.\pipe\wta-shared-host-{:016x}", hasher.finish())
}

pub fn host_session_is_ready(snapshot: &SharedStateSnapshot) -> bool {
    matches!(snapshot.state, ConnectionState::Connected) && !snapshot.session_id.trim().is_empty()
}

pub async fn wait_for_host(pipe_name: &str, timeout: Duration) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match try_connect_client_once(pipe_name) {
            Ok(client) => {
                let (reader, mut writer) = tokio::io::split(client);
                let mut lines = BufReader::new(reader).lines();

                send_line(&mut writer, &HostClientRequest::Ping).await?;
                if let Ok(Ok(Some(line))) =
                    tokio::time::timeout(Duration::from_secs(1), lines.next_line()).await
                {
                    let message: HostServerMessage =
                        serde_json::from_str(&line).context("invalid host ping response")?;
                    if matches!(message, HostServerMessage::Pong) {
                        return Ok(());
                    }
                }
            }
            Err(_) => {}
        }

        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for shared host pipe {}", pipe_name);
        }

        sleep(Duration::from_millis(75)).await;
    }
}

pub async fn probe_host_snapshot(
    pipe_name: &str,
    timeout: Duration,
) -> Result<SharedStateSnapshot> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(snapshot) = try_probe_host_snapshot_once(pipe_name).await? {
            return Ok(snapshot);
        }

        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for shared host snapshot {}", pipe_name);
        }

        sleep(Duration::from_millis(75)).await;
    }
}

pub async fn wait_for_host_ready(
    pipe_name: &str,
    timeout: Duration,
) -> Result<SharedStateSnapshot> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(snapshot) = try_probe_host_snapshot_once(pipe_name).await? {
            if host_session_is_ready(&snapshot) {
                return Ok(snapshot);
            }

            if let ConnectionState::Failed(message) = &snapshot.state {
                anyhow::bail!("shared host failed to initialize: {}", message);
            }
        }

        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for shared host session {}", pipe_name);
        }

        sleep(Duration::from_millis(75)).await;
    }
}

pub async fn run_attach_client(
    host_pipe_name: String,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    mut prompt_rx: mpsc::UnboundedReceiver<PromptSubmission>,
    mut recommendation_rx: mpsc::UnboundedReceiver<crate::coordinator::ChoiceExecution>,
    mut permission_rx: mpsc::UnboundedReceiver<String>,
    pane_context: PaneContext,
    initial_prompt: Option<String>,
    debug_capture_enabled: Arc<AtomicBool>,
) {
    if let Err(err) = run_attach_client_inner(
        host_pipe_name,
        event_tx.clone(),
        &mut prompt_rx,
        &mut recommendation_rx,
        &mut permission_rx,
        pane_context,
        initial_prompt,
        debug_capture_enabled,
    )
    .await
    {
        let _ = event_tx.send(AppEvent::AgentError(format!(
            "shared host connection failed: {:#}",
            err
        )));
    }
}

/// Host-internal autofix commands from main.rs when no attach TUI is present.
///
/// `Trigger`: run the full flow for a failing pane — submit the diagnose
/// prompt, emit pending, and (via finalize_agent_response) eventually emit
/// armed with the fix preview.
///
/// `Execute`: the user pressed Ctrl+. (or clicked the status bar) — take the
/// currently armed recommendation and run it against the failing pane.
#[derive(Debug, Clone)]
pub enum HostAutofixCommand {
    Trigger {
        pane_id: String,
        summary: String,
        source_cwd: Option<String>,
    },
    Execute {
        pane_id: String,
    },
}

pub async fn run_host_server(
    host_pipe_name: String,
    agent_cmd: String,
    delegate_agent_cmd: Option<String>,
    shell_mgr: Arc<ShellManager>,
    wt_connected: bool,
    // If Some, caller injects autofix commands here. run_host_server forwards
    // them to host_command_tx as synthetic requests with a reserved client_id
    // so the existing state/prompt machinery handles them identically to
    // pipe-client requests.
    autofix_cmd_rx: Option<mpsc::UnboundedReceiver<HostAutofixCommand>>,
) -> Result<()> {
    host_log(&format!(
        "starting shared host pipe={} wt_connected={}",
        host_pipe_name, wt_connected
    ));

    let (host_command_tx, host_command_rx) = mpsc::unbounded_channel();
    tokio::spawn(run_accept_loop(
        host_pipe_name.clone(),
        host_command_tx.clone(),
    ));

    // Shared attach-client count. Host-side autofix trigger skips when > 0 so
    // the attach TUI (which has its own maybe_trigger_autofix) stays the
    // single autofix initiator in that mode. Host only takes over when no
    // attach is connected (agent pane closed or never opened).
    let attach_count = Arc::new(AtomicUsize::new(0));

    // Host-internal autofix bridge. Routes main.rs-originated commands
    // through host_command_tx so they go through the same state machine
    // as pipe-client requests.
    if let Some(mut cmd_rx) = autofix_cmd_rx {
        let command_tx = host_command_tx.clone();
        let attach_count_trigger = attach_count.clone();
        tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                let active_attaches = attach_count_trigger.load(Ordering::Relaxed);
                if active_attaches > 0 {
                    host_log(&format!(
                        "autofix cmd skipped (attach_count={}): attach TUI will handle: {:?}",
                        active_attaches, cmd
                    ));
                    continue;
                }
                match cmd {
                    HostAutofixCommand::Trigger {
                        pane_id,
                        summary,
                        source_cwd,
                    } => {
                        host_log(&format!(
                            "autofix_trigger received: pane={} summary={}",
                            pane_id, summary
                        ));
                        // Publish pending UI state right away.
                        let pending_evt = serde_json::json!({
                            "type": "event",
                            "method": "autofix_state",
                            "params": {
                                "state": "pending",
                                "pane_id": pane_id,
                                "summary": summary,
                            }
                        });
                        crate::app::send_wt_protocol_event(pending_evt.to_string());

                        let pane_context = PaneContext {
                            pane_id: None,
                            tab_id: None,
                            window_id: None,
                            cwd: source_cwd,
                            source_pane_id: Some(pane_id.clone()),
                        };
                        let prompt_text =
                            format!("{}\nDiagnose the error and suggest a fix.", summary);
                        let sub = PromptSubmission::new_autofix(
                            prompt_text.clone(),
                            Some(pane_context.clone()),
                        );
                        let _ = command_tx.send(HostCommand::ClientRequest {
                            client_id: u64::MAX,
                            request: HostClientRequest::SubmitPrompt {
                                prompt_id: sub.id,
                                submitted_at_unix_s: sub.submitted_at_unix_s,
                                text: prompt_text,
                                pane_context: Some(pane_context),
                                is_autofix: true,
                            },
                        });
                    }
                    HostAutofixCommand::Execute { pane_id } => {
                        host_log(&format!("autofix_execute received: pane={}", pane_id));
                        // run_host_service will read state.recommendations,
                        // pick the recommended choice, run it against the
                        // failing pane, and emit autofix_state:cleared.
                        let _ = command_tx.send(HostCommand::ClientRequest {
                            client_id: u64::MAX,
                            request: HostClientRequest::ExecuteArmedAutofix {
                                source_pane_id: pane_id,
                            },
                        });
                    }
                }
            }
        });
    }

    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (prompt_tx, prompt_rx) = mpsc::unbounded_channel::<PromptSubmission>();
    let (recommendation_tx, recommendation_rx) = mpsc::unbounded_channel();

    let delegate_agent_runtimes = crate::coordinator::default_delegate_agent_runtimes(
        delegate_agent_cmd.as_deref(),
        Some(agent_cmd.as_str()),
        None, // shared host doesn't have delegate model
    );
    tokio::spawn(crate::coordinator::run_recommendation_executor(
        recommendation_rx,
        event_tx.clone(),
        shell_mgr.clone(),
        delegate_agent_runtimes,
    ));

    tokio::task::spawn_local(run_acp_client(
        agent_cmd,
        event_tx.clone(),
        prompt_rx,
        shell_mgr,
        wt_connected,
    ));

    run_host_service(
        host_command_rx,
        event_rx,
        prompt_tx,
        recommendation_tx,
        wt_connected,
        attach_count,
    )
    .await
}

async fn run_attach_client_inner(
    host_pipe_name: String,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    prompt_rx: &mut mpsc::UnboundedReceiver<PromptSubmission>,
    recommendation_rx: &mut mpsc::UnboundedReceiver<crate::coordinator::ChoiceExecution>,
    permission_rx: &mut mpsc::UnboundedReceiver<String>,
    pane_context: PaneContext,
    initial_prompt: Option<String>,
    debug_capture_enabled: Arc<AtomicBool>,
) -> Result<()> {
    let client = connect_client(&host_pipe_name)
        .await
        .with_context(|| format!("failed to connect to shared host {}", host_pipe_name))?;
    let (reader, mut writer) = tokio::io::split(client);
    let mut lines = BufReader::new(reader).lines();

    send_host_request(
        &event_tx,
        &debug_capture_enabled,
        &mut writer,
        &HostClientRequest::Attach {
            pane_context: pane_context.clone(),
        },
    )
    .await?;

    if let Some(text) = initial_prompt {
        let prompt = PromptSubmission::new(text, Some(pane_context.clone()));
        prompt_timing_log(
            prompt.id,
            prompt.submitted_at_unix_s,
            "attach_initial_prompt",
            &format!("preview={:?}", prompt.preview()),
        );
        send_host_request(
            &event_tx,
            &debug_capture_enabled,
            &mut writer,
            &HostClientRequest::SubmitPrompt {
                prompt_id: prompt.id,
                submitted_at_unix_s: prompt.submitted_at_unix_s,
                text: prompt.text,
                pane_context: Some(pane_context.clone()),
                is_autofix: prompt.is_autofix,
            },
        )
        .await?;
    }

    loop {
        tokio::select! {
            read = lines.next_line() => {
                match read? {
                    Some(line) => {
                        let line_len = line.len();
                        emit_debug_message(
                            &event_tx,
                            &debug_capture_enabled,
                            DebugDir::Received,
                            line.clone(),
                        );
                        let parse_started = std::time::Instant::now();
                        let message: HostServerMessage = serde_json::from_str(&line)
                            .context("failed to parse shared host message")?;
                        ui_trace::log_slow("attach_host_message_parse", parse_started.elapsed(), || {
                            format!(
                                "bytes={} message_type={}",
                                line_len,
                                host_server_message_name(&message)
                            )
                        });
                        match message {
                            HostServerMessage::Attached { snapshot, .. }
                            | HostServerMessage::SharedStateSnapshot { snapshot } => {
                                let _ = event_tx.send(AppEvent::SharedStateSnapshot(snapshot));
                            }
                            HostServerMessage::Event { event } => {
                                let _ = event_tx.send(event.into_app_event());
                            }
                            HostServerMessage::Error { message } => {
                                let _ = event_tx.send(AppEvent::SystemMessage(format!(
                                    "[host] {}",
                                    message
                                )));
                            }
                            HostServerMessage::Pong => {}
                        }
                    }
                    None => {
                        anyhow::bail!("shared host closed the connection");
                    }
                }
            }

            Some(prompt) = prompt_rx.recv() => {
                prompt_timing_log(
                    prompt.id,
                    prompt.submitted_at_unix_s,
                    "attach_client_send",
                    &format!("preview={:?}", prompt.preview()),
                );
                // Use the prompt's own pane_context (e.g. autofix sets
                // source_pane_id), falling back to the attach context.
                let effective_context = prompt.pane_context.unwrap_or_else(|| pane_context.clone());
                send_host_request(
                    &event_tx,
                    &debug_capture_enabled,
                    &mut writer,
                    &HostClientRequest::SubmitPrompt {
                        prompt_id: prompt.id,
                        submitted_at_unix_s: prompt.submitted_at_unix_s,
                        text: prompt.text,
                        pane_context: Some(effective_context),
                        is_autofix: prompt.is_autofix,
                    },
                ).await?;
            }

            Some(exec) = recommendation_rx.recv() => {
                send_host_request(
                    &event_tx,
                    &debug_capture_enabled,
                    &mut writer,
                    &HostClientRequest::SelectRecommendation {
                        choice: exec.choice.choice,
                    },
                ).await?;
            }

            Some(option_id) = permission_rx.recv() => {
                send_host_request(
                    &event_tx,
                    &debug_capture_enabled,
                    &mut writer,
                    &HostClientRequest::RespondPermission { option_id },
                ).await?;
            }

            else => {
                send_host_request(
                    &event_tx,
                    &debug_capture_enabled,
                    &mut writer,
                    &HostClientRequest::Detach,
                ).await.ok();
                break;
            }
        }
    }

    Ok(())
}

async fn send_host_request<W: AsyncWrite + Unpin>(
    event_tx: &mpsc::UnboundedSender<AppEvent>,
    debug_capture_enabled: &Arc<AtomicBool>,
    writer: &mut W,
    request: &HostClientRequest,
) -> Result<()> {
    let json = serde_json::to_string(request)?;
    emit_debug_message(
        event_tx,
        debug_capture_enabled,
        DebugDir::Sent,
        json.clone(),
    );
    writer.write_all(json.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

async fn run_accept_loop(pipe_name: String, host_command_tx: mpsc::UnboundedSender<HostCommand>) {
    let mut server = match ServerOptions::new()
        .first_pipe_instance(true)
        .create(&pipe_name)
    {
        Ok(server) => server,
        Err(err) => {
            host_log(&format!(
                "failed to create shared host pipe {}: {}",
                pipe_name, err
            ));
            return;
        }
    };

    let mut next_client_id = 1u64;
    loop {
        if let Err(err) = server.connect().await {
            host_log(&format!("shared host accept failed: {}", err));
            return;
        }

        let connected = server;
        server = match ServerOptions::new().create(&pipe_name) {
            Ok(server) => server,
            Err(err) => {
                host_log(&format!(
                    "failed to replenish shared host pipe {}: {}",
                    pipe_name, err
                ));
                return;
            }
        };

        let client_id = next_client_id;
        next_client_id += 1;
        let tx = host_command_tx.clone();
        tokio::spawn(async move {
            if let Err(err) = run_client_connection(connected, client_id, tx).await {
                host_log(&format!(
                    "client {} disconnected with error: {:#}",
                    client_id, err
                ));
            }
        });
    }
}

async fn run_client_connection(
    pipe: tokio::net::windows::named_pipe::NamedPipeServer,
    client_id: u64,
    host_command_tx: mpsc::UnboundedSender<HostCommand>,
) -> Result<()> {
    let (reader, mut writer) = tokio::io::split(pipe);
    let mut lines = BufReader::new(reader).lines();
    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();
    let mut attached = false;

    loop {
        tokio::select! {
            read = lines.next_line() => {
                match read? {
                    Some(line) => {
                        let request: HostClientRequest = serde_json::from_str(&line)
                            .context("failed to parse client host request")?;

                        match request {
                            HostClientRequest::Attach { pane_context } => {
                                attached = true;
                                let _ = host_command_tx.send(HostCommand::AttachClient {
                                    client_id,
                                    pane_context,
                                    updates: updates_tx.clone(),
                                });
                            }
                            HostClientRequest::Detach => {
                                let _ = host_command_tx.send(HostCommand::DetachClient { client_id });
                                break;
                            }
                            HostClientRequest::Ping => {
                                send_line(&mut writer, &HostServerMessage::Pong).await?;
                            }
                            other => {
                                if attached {
                                    let _ = host_command_tx.send(HostCommand::ClientRequest {
                                        client_id,
                                        request: other,
                                    });
                                } else {
                                    send_line(
                                        &mut writer,
                                        &HostServerMessage::Error {
                                            message: "attach must be sent before other host requests".to_string(),
                                        },
                                    )
                                    .await?;
                                }
                            }
                        }
                    }
                    None => {
                        break;
                    }
                }
            }

            Some(message) = updates_rx.recv() => {
                send_line(&mut writer, &message).await?;
            }

            else => break,
        }
    }

    let _ = host_command_tx.send(HostCommand::DetachClient { client_id });
    Ok(())
}

async fn run_host_service(
    mut host_command_rx: mpsc::UnboundedReceiver<HostCommand>,
    mut event_rx: mpsc::UnboundedReceiver<AppEvent>,
    prompt_tx: mpsc::UnboundedSender<PromptSubmission>,
    recommendation_tx: mpsc::UnboundedSender<crate::coordinator::ChoiceExecution>,
    wt_connected: bool,
    attach_count: Arc<AtomicUsize>,
) -> Result<()> {
    let mut clients: HashMap<u64, AttachedClient> = HashMap::new();
    let mut state = HostSessionState::new(wt_connected);

    loop {
        tokio::select! {
            Some(command) = host_command_rx.recv() => {
                handle_host_command(
                    command,
                    &mut clients,
                    &mut state,
                    &prompt_tx,
                    &recommendation_tx,
                );
                // Keep attach_count in sync with the clients map so the
                // host-side autofix trigger knows whether an attach TUI is
                // currently responsible for initiating autofix.
                attach_count.store(clients.len(), Ordering::Relaxed);
            }

            Some(event) = event_rx.recv() => {
                let snapshot_after_event = matches!(event, AppEvent::AgentMessageEnd);
                let shared_event = if snapshot_after_event {
                    None
                } else {
                    SharedUiEvent::from_app_event(&event)
                };
                state.apply_agent_event(event);
                if let Some(event) = shared_event {
                    broadcast_event(&mut clients, &event);
                } else {
                    broadcast_snapshot(&mut clients, &state.snapshot());
                }
            }

            else => break,
        }
    }

    Ok(())
}

fn handle_host_command(
    command: HostCommand,
    clients: &mut HashMap<u64, AttachedClient>,
    state: &mut HostSessionState,
    prompt_tx: &mpsc::UnboundedSender<PromptSubmission>,
    recommendation_tx: &mpsc::UnboundedSender<crate::coordinator::ChoiceExecution>,
) {
    match command {
        HostCommand::AttachClient {
            client_id,
            pane_context,
            updates,
        } => {
            clients.insert(
                client_id,
                AttachedClient {
                    pane_context,
                    updates,
                },
            );
            send_to_client(
                clients,
                client_id,
                HostServerMessage::Attached {
                    client_id,
                    snapshot: state.snapshot(),
                },
            );
        }
        HostCommand::DetachClient { client_id } => {
            clients.remove(&client_id);
        }
        HostCommand::ClientRequest { client_id, request } => match request {
            HostClientRequest::GetSnapshot => {
                send_to_client(
                    clients,
                    client_id,
                    HostServerMessage::SharedStateSnapshot {
                        snapshot: state.snapshot(),
                    },
                );
            }
            HostClientRequest::PaneContextUpdate { pane_context } => {
                if let Some(client) = clients.get_mut(&client_id) {
                    client.pane_context = pane_context;
                }
            }
            HostClientRequest::SubmitPrompt {
                prompt_id,
                submitted_at_unix_s,
                text,
                pane_context,
                is_autofix,
            } => {
                if text.trim().is_empty() {
                    return;
                }

                let effective_context = if let Some(context) = pane_context {
                    if let Some(client) = clients.get_mut(&client_id) {
                        client.pane_context = context.clone();
                    }
                    Some(context)
                } else {
                    clients
                        .get(&client_id)
                        .map(|client| client.pane_context.clone())
                };

                state.record_prompt_submission(
                    text.clone(),
                    effective_context.clone(),
                    submitted_at_unix_s,
                    is_autofix,
                );
                // Auto-fix prompts are synthesized by the client when a
                // command fails — the client already renders its own
                // ChatMessage::Error with a red dot, so don't broadcast
                // them as a User message (avoids the "> ... Diagnose the
                // error ..." header line).
                if !is_autofix {
                    broadcast_event(clients, &SharedUiEvent::UserMessage { text: text.clone() });
                }
                prompt_timing_log(
                    prompt_id,
                    submitted_at_unix_s,
                    "shared_host_received",
                    &format!(
                        "preview={:?}",
                        crate::protocol::acp::client::PromptSubmission::from_parts(
                            prompt_id,
                            text.clone(),
                            None,
                            submitted_at_unix_s,
                            is_autofix,
                        )
                        .preview()
                    ),
                );
                if prompt_tx
                    .send(PromptSubmission::from_parts(
                        prompt_id,
                        text,
                        effective_context,
                        submitted_at_unix_s,
                        is_autofix,
                    ))
                    .is_err()
                {
                    state.push_error("agent prompt loop is unavailable".to_string());
                    broadcast_event(
                        clients,
                        &SharedUiEvent::AgentError {
                            message: "agent prompt loop is unavailable".to_string(),
                        },
                    );
                }
            }
            HostClientRequest::SelectRecommendation { choice } => {
                let maybe_choice = state
                    .recommendations
                    .as_ref()
                    .and_then(|set| set.choices.iter().find(|item| item.choice == choice))
                    .cloned();

                if let Some(mut selected) = maybe_choice {
                    // Auto-fill empty `parent` on Send actions from the
                    // submitting client's pane context (needed for auto-fix
                    // where the failing pane ID is only known client-side).
                    let source_pane = clients
                        .get(&client_id)
                        .and_then(|c| c.pane_context.source_pane_id.clone())
                        .or_else(|| {
                            clients
                                .get(&client_id)
                                .and_then(|c| c.pane_context.pane_id.clone())
                        });
                    if let Some(ref pane_id) = source_pane {
                        for action in &mut selected.actions {
                            if let crate::coordinator::RecommendedAction::Send {
                                ref mut parent, ..
                            } = action
                            {
                                if parent.is_empty() {
                                    *parent = pane_id.clone();
                                }
                            }
                        }
                    }

                    state.commit_pending_completed_turn();
                    state.clear_recommendations();
                    state.push_execution_info(format!("Executing choice {}.", selected.choice));
                    broadcast_snapshot(clients, &state.snapshot());
                    if recommendation_tx.send(crate::coordinator::ChoiceExecution {
                        choice: selected,
                        insert_only: false,
                    }).is_err() {
                        state.push_system_message(
                            "recommendation executor is unavailable".to_string(),
                        );
                        broadcast_snapshot(clients, &state.snapshot());
                    }
                } else {
                    send_to_client(
                        clients,
                        client_id,
                        HostServerMessage::Error {
                            message: format!("recommendation {} is no longer available", choice),
                        },
                    );
                }
            }
            HostClientRequest::ExecuteArmedAutofix { source_pane_id } => {
                // Emit cleared unconditionally — whatever happens below, the
                // bottom-bar state should return to Idle so a stale Armed
                // badge doesn't linger.
                let emit_cleared = || {
                    let evt = serde_json::json!({
                        "type": "event",
                        "method": "autofix_state",
                        "params": {
                            "state": "cleared",
                            "pane_id": source_pane_id,
                        }
                    });
                    crate::app::send_wt_protocol_event(evt.to_string());
                };

                let maybe_choice = state
                    .recommendations
                    .as_ref()
                    .and_then(|set| {
                        let idx = set
                            .recommended_choice
                            .unwrap_or(0)
                            .min(set.choices.len().saturating_sub(1));
                        set.choices.get(idx)
                    })
                    .cloned();

                let Some(mut selected) = maybe_choice else {
                    host_log(&format!(
                        "execute_armed_autofix: no recommendation available for pane {}",
                        source_pane_id
                    ));
                    emit_cleared();
                    return;
                };

                // Fill `parent` on Send actions with the failing pane id so
                // the fix runs in the right place.
                for action in &mut selected.actions {
                    if let crate::coordinator::RecommendedAction::Send {
                        ref mut parent, ..
                    } = action
                    {
                        if parent.is_empty() {
                            *parent = source_pane_id.clone();
                        }
                    }
                }

                state.commit_pending_completed_turn();
                state.clear_recommendations();
                state.push_execution_info(format!(
                    "Auto-executing choice {} for pane {}.",
                    selected.choice, source_pane_id
                ));
                broadcast_snapshot(clients, &state.snapshot());
                if recommendation_tx
                    .send(crate::coordinator::ChoiceExecution {
                        choice: selected,
                        insert_only: false,
                    })
                    .is_err()
                {
                    state.push_system_message(
                        "recommendation executor is unavailable".to_string(),
                    );
                    broadcast_snapshot(clients, &state.snapshot());
                }
                emit_cleared();
            }
            HostClientRequest::RespondPermission { option_id } => {
                if let Some(responder) = state.permission_responder.take() {
                    let _ = responder.send(option_id);
                    state.permission = None;
                    state.bump();
                    broadcast_event(clients, &SharedUiEvent::PermissionCleared);
                } else {
                    send_to_client(
                        clients,
                        client_id,
                        HostServerMessage::Error {
                            message: "no pending permission request".to_string(),
                        },
                    );
                }
            }
            HostClientRequest::Ping => {
                send_to_client(clients, client_id, HostServerMessage::Pong);
            }
            HostClientRequest::Attach { .. } | HostClientRequest::Detach => {}
        },
    }
}

async fn connect_client(
    pipe_name: &str,
) -> io::Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    loop {
        match try_connect_client_once(pipe_name) {
            Ok(client) => return Ok(client),
            Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                sleep(Duration::from_millis(50)).await;
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                sleep(Duration::from_millis(50)).await;
            }
            Err(err) => return Err(err),
        }
    }
}

async fn try_probe_host_snapshot_once(pipe_name: &str) -> Result<Option<SharedStateSnapshot>> {
    let client = match try_connect_client_once(pipe_name) {
        Ok(client) => client,
        Err(err)
            if err.raw_os_error() == Some(ERROR_PIPE_BUSY as i32)
                || err.kind() == io::ErrorKind::NotFound =>
        {
            return Ok(None);
        }
        Err(err) => return Err(err.into()),
    };

    let (reader, mut writer) = tokio::io::split(client);
    let mut lines = BufReader::new(reader).lines();
    send_line(
        &mut writer,
        &HostClientRequest::Attach {
            pane_context: PaneContext::default(),
        },
    )
    .await?;

    let response_deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        let remaining = response_deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }

        match tokio::time::timeout(remaining, lines.next_line()).await {
            Ok(Ok(Some(line))) => {
                let message: HostServerMessage =
                    serde_json::from_str(&line).context("invalid shared host snapshot response")?;
                match message {
                    HostServerMessage::Attached { snapshot, .. }
                    | HostServerMessage::SharedStateSnapshot { snapshot } => {
                        return Ok(Some(snapshot));
                    }
                    HostServerMessage::Error { message } => {
                        anyhow::bail!("shared host snapshot request failed: {}", message);
                    }
                    HostServerMessage::Event { .. } | HostServerMessage::Pong => {}
                }
            }
            Ok(Ok(None)) => break,
            Ok(Err(err)) => return Err(err.into()),
            Err(_) => break,
        }
    }

    Ok(None)
}

fn try_connect_client_once(
    pipe_name: &str,
) -> io::Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    ClientOptions::new().open(pipe_name)
}

async fn send_line<W: AsyncWrite + Unpin, T: Serialize>(writer: &mut W, value: &T) -> Result<()> {
    let json = serde_json::to_string(value)?;
    writer.write_all(json.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

fn emit_debug_message(
    event_tx: &mpsc::UnboundedSender<AppEvent>,
    debug_capture_enabled: &Arc<AtomicBool>,
    direction: DebugDir,
    content: String,
) {
    if !debug_capture_enabled.load(Ordering::Relaxed) {
        return;
    }

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let _ = event_tx.send(AppEvent::DebugPipeMessage(DebugMessage {
        timestamp,
        direction,
        content,
    }));
}

fn send_to_client(
    clients: &mut HashMap<u64, AttachedClient>,
    client_id: u64,
    message: HostServerMessage,
) {
    let failed = clients
        .get(&client_id)
        .map(|client| client.updates.send(message).is_err())
        .unwrap_or(false);

    if failed {
        clients.remove(&client_id);
    }
}

fn broadcast_snapshot(clients: &mut HashMap<u64, AttachedClient>, snapshot: &SharedStateSnapshot) {
    let mut dead = Vec::new();
    for (client_id, client) in clients.iter() {
        if client
            .updates
            .send(HostServerMessage::SharedStateSnapshot {
                snapshot: snapshot.clone(),
            })
            .is_err()
        {
            dead.push(*client_id);
        }
    }

    for client_id in dead {
        clients.remove(&client_id);
    }
}

fn broadcast_event(clients: &mut HashMap<u64, AttachedClient>, event: &SharedUiEvent) {
    let mut dead = Vec::new();
    for (client_id, client) in clients.iter() {
        if client
            .updates
            .send(HostServerMessage::Event {
                event: event.clone(),
            })
            .is_err()
        {
            dead.push(*client_id);
        }
    }

    for client_id in dead {
        clients.remove(&client_id);
    }
}

fn host_log(message: &str) {
    use std::io::Write;

    if std::env::var("WTA_DEBUG_LOG").as_deref() != Ok("1") {
        return;
    }

    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(crate::runtime_paths::runtime_log_path("wta-host-debug.log"))
    {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let _ = writeln!(file, "[{:.3}] {}", timestamp, message);
        let _ = file.flush();
    }
}

fn host_server_message_name(message: &HostServerMessage) -> &'static str {
    match message {
        HostServerMessage::Attached { .. } => "attached",
        HostServerMessage::SharedStateSnapshot { .. } => "shared_state_snapshot",
        HostServerMessage::Event { .. } => "event",
        HostServerMessage::Error { .. } => "error",
        HostServerMessage::Pong => "pong",
    }
}

struct AttachedClient {
    pane_context: PaneContext,
    updates: mpsc::UnboundedSender<HostServerMessage>,
}

enum FinalizeOutcome {
    None,
    SelectionReady,
}

enum HostCommand {
    AttachClient {
        client_id: u64,
        pane_context: PaneContext,
        updates: mpsc::UnboundedSender<HostServerMessage>,
    },
    ClientRequest {
        client_id: u64,
        request: HostClientRequest,
    },
    DetachClient {
        client_id: u64,
    },
}

struct HostSessionState {
    version: u64,
    state: ConnectionState,
    agent_name: String,
    agent_model: Option<String>,
    prompt_name: Option<String>,
    progress_status: Option<String>,
    session_id: String,
    wt_connected: bool,
    messages: Vec<ChatMessage>,
    completed_turns: Vec<CompletedTurn>,
    recommendations: Option<RecommendationSet>,
    current_prompt_pane_context: Option<PaneContext>,
    current_prompt_text: Option<String>,
    // Set when the in-flight prompt was synthesized by auto-fix.
    // Used to emit `autofix_state:armed` directly from the host when the
    // attach TUI isn't running (agent pane closed / never opened).
    current_prompt_is_autofix: bool,
    current_prompt_submitted_at_unix_s: Option<f64>,
    pending_completed_turn: Option<CompletedTurn>,
    agent_streaming: bool,
    pending_thought_response: String,
    pending_agent_response: String,
    prompt_in_flight: bool,
    timing_note: Option<String>,
    permission: Option<PermissionPrompt>,
    permission_responder: Option<tokio::sync::oneshot::Sender<String>>,
    tool_calls: HashMap<String, (String, String)>,
}

impl HostSessionState {
    fn new(wt_connected: bool) -> Self {
        Self {
            version: 1,
            state: ConnectionState::Connecting("Starting agent...".to_string()),
            agent_name: String::new(),
            agent_model: None,
            prompt_name: None,
            progress_status: None,
            session_id: String::new(),
            wt_connected,
            messages: Vec::new(),
            completed_turns: Vec::new(),
            recommendations: None,
            current_prompt_pane_context: None,
            current_prompt_text: None,
            current_prompt_is_autofix: false,
            current_prompt_submitted_at_unix_s: None,
            pending_completed_turn: None,
            agent_streaming: false,
            pending_thought_response: String::new(),
            pending_agent_response: String::new(),
            prompt_in_flight: false,
            timing_note: None,
            permission: None,
            permission_responder: None,
            tool_calls: HashMap::new(),
        }
    }

    fn snapshot(&self) -> SharedStateSnapshot {
        SharedStateSnapshot {
            version: self.version,
            state: self.state.clone(),
            agent_name: self.agent_name.clone(),
            agent_model: self.agent_model.clone(),
            prompt_name: self.prompt_name.clone(),
            progress_status: self.progress_status.clone(),
            session_id: self.session_id.clone(),
            wt_connected: self.wt_connected,
            messages: self.messages.clone(),
            completed_turns: self.completed_turns.clone(),
            recommendations: self.recommendations.clone(),
            agent_streaming: self.agent_streaming,
            pending_thought_response: self.pending_thought_response.clone(),
            pending_agent_response: self.pending_agent_response.clone(),
            prompt_in_flight: self.prompt_in_flight,
            timing_note: self.timing_note.clone(),
            permission: self.permission.clone(),
        }
    }

    fn bump(&mut self) {
        self.version += 1;
    }

    fn record_prompt_submission(
        &mut self,
        text: String,
        pane_context: Option<PaneContext>,
        submitted_at_unix_s: f64,
        is_autofix: bool,
    ) {
        self.clear_chat_history();
        self.current_prompt_pane_context = pane_context;
        self.current_prompt_text = Some(text.clone());
        self.current_prompt_submitted_at_unix_s = Some(submitted_at_unix_s);
        self.current_prompt_is_autofix = is_autofix;
        self.prompt_in_flight = true;
        self.agent_streaming = false;
        self.progress_status = Some("Preparing context...".to_string());
        self.messages.push(ChatMessage::User(text));
        self.bump();
    }

    fn push_error(&mut self, message: String) {
        self.state = ConnectionState::Failed(message.clone());
        self.prompt_in_flight = false;
        self.agent_streaming = false;
        self.progress_status = None;
        self.pending_thought_response.clear();
        self.pending_agent_response.clear();
        self.timing_note = None;
        self.current_prompt_submitted_at_unix_s = None;
        self.pending_completed_turn = None;
        self.messages.push(ChatMessage::Error(message));
        self.permission = None;
        self.permission_responder = None;
        self.bump();
    }

    fn push_system_message(&mut self, message: String) {
        self.messages.push(ChatMessage::System(message));
        self.bump();
    }

    fn push_execution_info(&mut self, message: String) {
        if let Some(turn) = self.completed_turns.last_mut() {
            turn.details.push(ChatMessage::System(message));
        } else {
            self.messages.push(ChatMessage::System(message));
        }
        self.bump();
    }

    fn clear_recommendations(&mut self) {
        self.recommendations = None;
    }

    fn clear_chat_history(&mut self) {
        self.messages.clear();
        self.tool_calls.clear();
        self.permission = None;
        self.permission_responder = None;
        self.progress_status = None;
        self.pending_thought_response.clear();
        self.pending_agent_response.clear();
        self.agent_streaming = false;
        self.timing_note = None;
        self.current_prompt_text = None;
        self.current_prompt_submitted_at_unix_s = None;
        self.pending_completed_turn = None;
        self.clear_recommendations();
    }

    fn clear_completed_turn_history(&mut self) {
        self.messages.clear();
        self.tool_calls.clear();
        self.permission = None;
        self.permission_responder = None;
        self.progress_status = None;
        self.pending_thought_response.clear();
        self.pending_agent_response.clear();
        self.agent_streaming = false;
        self.current_prompt_text = None;
        self.current_prompt_submitted_at_unix_s = None;
    }

    fn completion_latency_summary(&self) -> Option<String> {
        let mut parts = Vec::new();

        if let Some(submitted_at) = self.current_prompt_submitted_at_unix_s {
            let total_s = (now_unix_s() - submitted_at).max(0.0);
            parts.push(format!("total {:.3}s", total_s));
        }

        if let Some(note) = self.timing_note.as_deref().filter(|note| !note.is_empty()) {
            parts.push(note.to_string());
        }

        if parts.is_empty() {
            None
        } else {
            Some(format!("Latency: {}", parts.join(" | ")))
        }
    }

    fn current_turn_details(&self) -> Vec<ChatMessage> {
        self.messages
            .iter()
            .filter(|message| !matches!(message, ChatMessage::User(_)))
            .cloned()
            .collect()
    }

    fn stage_completed_turn(&mut self, agent_text: String) {
        let Some(prompt) = self.current_prompt_text.clone() else {
            self.pending_completed_turn = None;
            return;
        };

        let mut details = self.current_turn_details();
        details.push(ChatMessage::Agent(agent_text));
        self.pending_completed_turn = Some(CompletedTurn { prompt, details });
    }

    fn commit_pending_completed_turn(&mut self) {
        let Some(turn) = self.pending_completed_turn.take() else {
            return;
        };

        self.completed_turns.push(turn);
    }

    fn apply_agent_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::ConnectionStage(stage) => {
                self.state = ConnectionState::Connecting(stage);
                self.bump();
            }
            AppEvent::ProgressStatus(message) => {
                self.progress_status = Some(message);
                self.bump();
            }
            AppEvent::AgentConnected {
                name,
                model,
                session_id,
            } => {
                self.agent_name = name;
                self.agent_model = model;
                self.session_id = session_id;
                self.state = ConnectionState::Connected;
                self.bump();
            }
            AppEvent::PromptTemplateLoaded { name } => {
                self.prompt_name = Some(name);
                self.bump();
            }
            AppEvent::AgentError(message) => {
                self.push_error(message);
            }
            AppEvent::ExecutionInfo(message) => {
                self.push_execution_info(message);
            }
            AppEvent::AgentThoughtChunk(text) => {
                self.prompt_in_flight = true;
                if self.progress_status.is_none() {
                    self.progress_status = Some("Thinking...".to_string());
                }
                append_thought_preview(&mut self.pending_thought_response, &text);
                self.bump();
            }
            AppEvent::AgentMessageChunk(text) => {
                self.agent_streaming = true;
                self.prompt_in_flight = true;
                self.progress_status = None;
                self.pending_thought_response.clear();
                self.pending_agent_response.push_str(&text);
                self.bump();
            }
            AppEvent::AgentMessageEnd => {
                self.agent_streaming = false;
                self.prompt_in_flight = false;
                self.progress_status = None;
                self.pending_thought_response.clear();
                if let Some(summary) = self.completion_latency_summary() {
                    self.push_execution_info(summary);
                }
                match self.finalize_agent_response() {
                    FinalizeOutcome::SelectionReady => {
                        self.clear_completed_turn_history();
                    }
                    FinalizeOutcome::None => {}
                }
                self.bump();
            }
            AppEvent::TimingMetric(message) => {
                self.timing_note = Some(message);
                self.bump();
            }
            AppEvent::ToolCall { id, title, status } => {
                self.tool_calls
                    .insert(id.clone(), (title.clone(), status.clone()));
                self.messages
                    .push(ChatMessage::ToolCall { id, title, status });
                self.bump();
            }
            AppEvent::ToolCallUpdate { id, status } => {
                if let Some(entry) = self.tool_calls.get_mut(&id) {
                    entry.1 = status.clone();
                }
                for message in &mut self.messages {
                    if let ChatMessage::ToolCall {
                        id: mid,
                        status: current,
                        ..
                    } = message
                    {
                        if mid == &id {
                            *current = status.clone();
                        }
                    }
                }
                self.bump();
            }
            AppEvent::Plan(entries) => {
                self.messages.push(ChatMessage::Plan(entries));
                self.bump();
            }
            AppEvent::PermissionRequest {
                description,
                options,
                responder,
            } => {
                self.permission = Some(PermissionPrompt {
                    description,
                    options,
                });
                self.permission_responder = Some(responder);
                self.bump();
            }
            AppEvent::SystemMessage(message) => {
                self.push_system_message(message);
            }
            AppEvent::WtEvent { .. }
            | AppEvent::UserMessage(_)
            | AppEvent::SharedPermissionRequest { .. }
            | AppEvent::PermissionCleared
            | AppEvent::Tick
            | AppEvent::Key(_)
            | AppEvent::Resize(_, _)
            | AppEvent::DebugPipeMessage(_)
            | AppEvent::SharedStateSnapshot(_) => {}
        }
    }

    fn finalize_agent_response(&mut self) -> FinalizeOutcome {
        if self.pending_agent_response.trim().is_empty() {
            self.pending_agent_response.clear();
            return FinalizeOutcome::None;
        }

        let text = std::mem::take(&mut self.pending_agent_response);
        let is_autofix = self.current_prompt_is_autofix;
        let autofix_source_pane = self
            .current_prompt_pane_context
            .as_ref()
            .and_then(|ctx| ctx.source_pane_id.clone());
        match parse_recommendation_set(&text) {
            Ok(recommendations) => {
                match validate_recommendation_set_for_coordinator_target(
                    &recommendations,
                    self.current_prompt_pane_context
                        .as_ref()
                        .and_then(|context| context.pane_id.as_deref()),
                ) {
                    Ok(filtered) => {
                        // When no attach TUI is running, this host is the
                        // only process that sees the recommendation become
                        // ready — so it must emit autofix_state:armed.
                        if is_autofix {
                            if let Some(pane_id) = autofix_source_pane.as_ref() {
                                let preview = crate::app::armed_fix_preview(&filtered);
                                let evt = serde_json::json!({
                                    "type": "event",
                                    "method": "autofix_state",
                                    "params": {
                                        "state": "armed",
                                        "pane_id": pane_id,
                                        "fix_preview": preview,
                                        "hotkey_hint": "Ctrl+Alt+.",
                                    }
                                });
                                crate::app::send_wt_protocol_event(evt.to_string());
                            }
                            self.current_prompt_is_autofix = false;
                        }
                        self.stage_completed_turn(text);
                        self.recommendations = Some(filtered);
                        FinalizeOutcome::SelectionReady
                    }
                    Err(_) => {
                        if is_autofix {
                            if let Some(pane_id) = autofix_source_pane.as_ref() {
                                let evt = serde_json::json!({
                                    "type": "event",
                                    "method": "autofix_state",
                                    "params": {
                                        "state": "cleared",
                                        "pane_id": pane_id,
                                    }
                                });
                                crate::app::send_wt_protocol_event(evt.to_string());
                            }
                            self.current_prompt_is_autofix = false;
                        }
                        self.recommendations = None;
                        self.pending_completed_turn = None;
                        self.stage_completed_turn(text);
                        self.commit_pending_completed_turn();
                        self.clear_chat_history();
                        FinalizeOutcome::None
                    }
                }
            }
            Err(_) => {
                if is_autofix {
                    if let Some(pane_id) = autofix_source_pane.as_ref() {
                        let evt = serde_json::json!({
                            "type": "event",
                            "method": "autofix_state",
                            "params": {
                                "state": "cleared",
                                "pane_id": pane_id,
                            }
                        });
                        crate::app::send_wt_protocol_event(evt.to_string());
                    }
                    self.current_prompt_is_autofix = false;
                }
                self.recommendations = None;
                self.pending_completed_turn = None;
                self.stage_completed_turn(text);
                self.commit_pending_completed_turn();
                self.clear_chat_history();
                FinalizeOutcome::None
            }
        }
    }
}

fn now_unix_s() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

const THOUGHT_PREVIEW_MAX_CHARS: usize = 1024;

fn append_thought_preview(buffer: &mut String, chunk: &str) {
    if chunk.is_empty() {
        return;
    }

    buffer.push_str(chunk);
    let char_count = buffer.chars().count();
    if char_count <= THOUGHT_PREVIEW_MAX_CHARS {
        return;
    }

    let tail: String = buffer
        .chars()
        .skip(char_count.saturating_sub(THOUGHT_PREVIEW_MAX_CHARS))
        .collect();
    *buffer = format!("...{tail}");
}
