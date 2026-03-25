use acp::Agent as _;
use agent_client_protocol as acp;
use anyhow::Result;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader, ReadBuf};
use tokio::sync::mpsc;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::app::{AppEvent, PermOption, PlanEntry, PlanEntryStatus};
use crate::coordinator::default_supported_delegate_agents;
use crate::shell::{ActivePaneSnapshot, ShellManager, TerminalConfig};

const ACTIVE_PANE_CONTEXT_MAX_LINES: u32 = 80;
const ACTIVE_PANE_CONTEXT_MAX_CHARS: usize = 4000;

fn truncate_for_prompt(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let truncated: String = text.chars().take(max_chars).collect();
        format!("{truncated}\n...<truncated>")
    }
}

fn format_active_pane_context(snapshot: &ActivePaneSnapshot) -> String {
    let mut context = format!(
        "Active pane context:\n\
         - window_id={}\n\
         - tab_id={}\n\
         - pane_id={}\n",
        snapshot.window_id, snapshot.tab_id, snapshot.pane_id
    );

    if let Some(title) = &snapshot.title {
        context.push_str(&format!("         - title={}\n", title));
    }
    if let Some(profile) = &snapshot.profile {
        context.push_str(&format!("         - profile={}\n", profile));
    }
    if let Some(line_count) = snapshot.line_count {
        context.push_str(&format!("         - captured_line_count={}\n", line_count));
    }
    if snapshot.truncated {
        context.push_str("         - captured_output_truncated=true\n");
    }

    if let Some(content) = &snapshot.content {
        context.push_str("         - recent pane content:\n");
        context.push_str("```text\n");
        context.push_str(&truncate_for_prompt(content, ACTIVE_PANE_CONTEXT_MAX_CHARS));
        if !content.ends_with('\n') {
            context.push('\n');
        }
        context.push_str("```\n");
    }

    context
}

async fn live_active_pane_context(shell_mgr: &ShellManager) -> Option<String> {
    let snapshot = shell_mgr
        .wt_active_pane_snapshot(Some(ACTIVE_PANE_CONTEXT_MAX_LINES))
        .await
        .ok()?;
    Some(format_active_pane_context(&snapshot))
}

fn source_pane_values() -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    (
        std::env::var("WTA_SOURCE_PANE_ID").ok(),
        std::env::var("WTA_SOURCE_TAB_ID").ok(),
        std::env::var("WTA_SOURCE_WINDOW_ID").ok(),
        std::env::var("WTA_SOURCE_CWD").ok(),
    )
}

fn planner_prompt_rules() -> String {
    String::from(
        "You are Terminal Agent, a Windows Terminal co-ordinator.\n\
         Your job is to plan the best next steps for the user and return executable action JSON for WTA.\n\
         Do not read files, run commands, use tools, inspect the repo, or fix issues directly.\n\
         Do not use local `wta`, shell commands, or MCP tools yourself in this session.\n\
         Only analyze the provided terminal context and propose ranked actions for WTA to execute after user selection.\n\
         \n\
         Action types you may emit:\n\
         - `run_command`: send a shell command plus Enter to an existing pane.\n\
         - `send_prompt`: send a prompt plus Enter to an existing agent pane.\n\
         - `create_shell_tab`: open a new WT tab, optionally with a commandline.\n\
         - `create_shell_panel`: split a new WT pane from a parent pane, optionally with a commandline.\n\
         - `delegate_tab`: open a new WT tab, optionally set its cwd/title, start the chosen delegate agent command there, then send the prompt plus Enter.\n\
         \n\
         Rules:\n\
         - Always return exactly 3 ranked choices.\n\
         - At least one choice should reuse an existing relevant pane when practical.\n\
         - At least one choice should delegate a hard or long-running task to a supported agent when appropriate.\n\
         - Use only `parent` pane IDs that appear in the terminal context JSON.\n\
         - Use only `agent` IDs that appear in the supported delegate agent JSON.\n\
         - For delegate actions, make the `prompt` fully self-contained. WTA will only launch the delegate CLI and paste exactly that prompt.\n\
         - Delegate prompts should include the user goal, the relevant context from the terminal snapshot, and the concrete next task.\n\
         - Delegation is tab-only. Do not propose delegate pane splits.\n\
         - Prefer `delegate_tab` for Copilot when the work is hard, long-running, or should stay isolated from the current pane.\n\
         - Prefer the source pane when the user refers to the terminal they were working in before opening this assistant.\n\
         - Do not invent capabilities that are not in the action list.\n\
         - Make titles concise and rationales short.\n\
         \n\
         Response format:\n\
         1. Three short numbered suggestions for the user.\n\
         2. One fenced JSON block with this shape and no additional JSON blocks:\n\
         ```json\n\
         {\n\
           \"recommended_choice\": 1,\n\
           \"choices\": [\n\
             {\n\
               \"choice\": 1,\n\
               \"title\": \"Delegate to Copilot in a new tab\",\n\
               \"rationale\": \"Best for a hard coding task that should run separately.\",\n\
               \"actions\": [\n\
                 {\n\
                   \"type\": \"delegate_tab\",\n\
                   \"parent\": \"12\",\n\
                   \"agent\": \"copilot\",\n\
                   \"cwd\": \"D:\\\\repo\",\n\
                   \"prompt\": \"You are working in D:\\\\repo. Investigate the failing test path shown in the terminal context, identify the root cause, make the smallest safe fix, and summarize what changed.\",\n\
                   \"title\": \"Copilot delegate\"\n\
                 }\n\
               ]\n\
             },\n\
             {\n\
               \"choice\": 2,\n\
               \"title\": \"Run a command in the source pane\",\n\
               \"rationale\": \"Fastest local verification path.\",\n\
               \"actions\": [\n\
                 {\n\
                   \"type\": \"run_command\",\n\
                   \"parent\": \"10\",\n\
                   \"command\": \"dotnet test\"\n\
                 }\n\
               ]\n\
             },\n\
             {\n\
               \"choice\": 3,\n\
               \"title\": \"Prompt the current agent pane\",\n\
               \"rationale\": \"Keeps work in the current assistant session.\",\n\
               \"actions\": [\n\
                 {\n\
                   \"type\": \"send_prompt\",\n\
                   \"parent\": \"14\",\n\
                   \"prompt\": \"Take the smaller next step...\"\n\
                 }\n\
               ]\n\
             }\n\
           ]\n\
         }\n\
         ```",
    )
}

fn json_str_or_num(value: Option<&serde_json::Value>) -> Option<String> {
    match value {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

async fn build_terminal_context_json(
    shell_mgr: &ShellManager,
    pane_identity: Option<&(String, String, String)>,
) -> Option<String> {
    let (source_pane_id, source_tab_id, source_window_id, source_cwd) = source_pane_values();
    let active = shell_mgr.wt_get_active_pane().await.ok();
    let active_pane_id = active
        .as_ref()
        .and_then(|v| json_str_or_num(v.get("pane_id")));

    let mut highlighted_panes = std::collections::BTreeSet::new();
    if let Some(pane_id) = &source_pane_id {
        highlighted_panes.insert(pane_id.clone());
    }
    if let Some(pane_id) = &active_pane_id {
        highlighted_panes.insert(pane_id.clone());
    }
    if let Some((pane_id, _, _)) = pane_identity {
        highlighted_panes.insert(pane_id.clone());
    }

    let windows = shell_mgr.wt_list_windows().await.ok()?;
    let windows_arr = windows.get("windows")?.as_array()?;
    let mut tabs_json = Vec::new();

    for win in windows_arr {
        let window_id = json_str_or_num(win.get("window_id"))?;
        let window_title = win
            .get("title")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let tabs = shell_mgr.wt_list_tabs(&window_id).await.ok()?;
        let tabs_arr = tabs.get("tabs")?.as_array()?;

        for tab in tabs_arr {
            let tab_id = json_str_or_num(tab.get("tab_id"))?;
            let panes = shell_mgr.wt_list_panes(&tab_id).await.ok()?;
            let panes_arr = panes.get("panes")?.as_array()?;
            let mut panels_json = Vec::new();

            for pane in panes_arr {
                let pane_id = json_str_or_num(pane.get("pane_id"))?;
                let pid = pane.get("pid").and_then(|value| value.as_u64());
                let is_active = pane
                    .get("is_active")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);

                let buffer = if highlighted_panes.contains(&pane_id) {
                    shell_mgr
                        .wt_read_pane_output(&pane_id, Some(24))
                        .await
                        .ok()
                        .and_then(|value| {
                            value
                                .get("content")
                                .and_then(|content| content.as_str())
                                .map(|content| {
                                    truncate_for_prompt(content, ACTIVE_PANE_CONTEXT_MAX_CHARS / 2)
                                })
                        })
                } else {
                    None
                };

                let pane_role = if source_pane_id.as_deref() == Some(pane_id.as_str()) {
                    Some("source")
                } else if pane_identity.map(|identity| identity.0.as_str())
                    == Some(pane_id.as_str())
                {
                    Some("assistant")
                } else if active_pane_id.as_deref() == Some(pane_id.as_str()) {
                    Some("active")
                } else {
                    None
                };

                panels_json.push(serde_json::json!({
                    "id": pane_id.clone(),
                    "pane_id": pane_id.clone(),
                    "window_id": window_id.clone(),
                    "tab_id": tab_id.clone(),
                    "window_title": window_title.clone(),
                    "is_active": is_active,
                    "pid": pid,
                    "role": pane_role,
                    "cwd": if source_pane_id.as_deref() == Some(pane_id.as_str()) { source_cwd.clone() } else { None },
                    "buffer": buffer,
                }));
            }

            tabs_json.push(serde_json::json!({
                "id": tab_id.clone(),
                "tab_id": tab_id.clone(),
                "window_id": window_id.clone(),
                "label": tab.get("title").and_then(|value| value.as_str()).unwrap_or(""),
                "is_active": tab.get("is_active").and_then(|value| value.as_bool()).unwrap_or(false),
                "panels": panels_json,
            }));
        }
    }

    serde_json::to_string_pretty(&serde_json::json!({
        "activeTarget": active_pane_id,
        "sourceTarget": source_pane_id,
        "sourceTabId": source_tab_id,
        "sourceWindowId": source_window_id,
        "assistantPaneId": pane_identity.map(|identity| identity.0.clone()),
        "assistantTabId": pane_identity.map(|identity| identity.1.clone()),
        "assistantWindowId": pane_identity.map(|identity| identity.2.clone()),
        "tabs": tabs_json,
    }))
    .ok()
}

async fn build_prompt_text(
    user_text: &str,
    shell_mgr: &ShellManager,
    wt_connected: bool,
    pane_identity: Option<&(String, String, String)>,
) -> String {
    let mut context_parts = Vec::new();
    context_parts.push(planner_prompt_rules());

    let supported_agents_json = serde_json::to_string_pretty(&default_supported_delegate_agents())
        .unwrap_or_else(|_| "[]".to_string());
    context_parts.push(format!(
        "Supported delegate agents:\n```json\n{}\n```",
        supported_agents_json
    ));

    if wt_connected {
        if let Some(terminal_context_json) =
            build_terminal_context_json(shell_mgr, pane_identity).await
        {
            context_parts.push(format!(
                "Terminal context JSON:\n```json\n{}\n```",
                terminal_context_json
            ));
        }
    }

    if let Some(active_pane) = live_active_pane_context(shell_mgr).await {
        context_parts.push(active_pane.trim_end().to_string());
    }

    format!(
        "{}\n\nUser request:\n{}",
        context_parts.join("\n\n"),
        user_text
    )
}

/// Write a line to wta-acp-debug.log when `WTA_DEBUG_LOG=1`.
fn acp_log_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("WTA_DEBUG_LOG").as_deref() == Ok("1"))
}

fn acp_log(msg: &str) {
    if !acp_log_enabled() {
        return;
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("wta-acp-debug.log")
    {
        let elapsed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let _ = writeln!(f, "[{:.3}] {}", elapsed.as_secs_f64(), msg);
    }
}

#[derive(Clone)]
struct StartupProbe {
    begin: std::time::Instant,
}

impl StartupProbe {
    fn new() -> Self {
        Self {
            begin: std::time::Instant::now(),
        }
    }

    fn log(&self, msg: &str) {
        if acp_log_enabled() {
            acp_log(&format!(
                "{} (t+{:.3}s)",
                msg,
                self.begin.elapsed().as_secs_f64()
            ));
        }
    }
}

struct StartupInstrumentedReader<R> {
    inner: R,
    probe: StartupProbe,
    label: &'static str,
    saw_data: bool,
}

impl<R> StartupInstrumentedReader<R> {
    fn new(inner: R, probe: StartupProbe, label: &'static str) -> Self {
        Self {
            inner,
            probe,
            label,
            saw_data: false,
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for StartupInstrumentedReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let filled_before = buf.filled().len();
        match Pin::new(&mut self.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                let read_len = buf.filled().len().saturating_sub(filled_before);
                if read_len > 0 && !self.saw_data {
                    self.saw_data = true;
                    self.probe.log(&format!(
                        "first data received on agent {}: {} byte(s)",
                        self.label, read_len
                    ));
                }
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

/// Shared state accessible from the Client trait impl.
struct ClientState {
    event_tx: mpsc::UnboundedSender<AppEvent>,
    shell_mgr: Arc<ShellManager>,
}

/// Our Client trait implementation — handles incoming agent requests and notifications.
struct WtaClient {
    state: Arc<ClientState>,
}

#[async_trait::async_trait(?Send)]
impl acp::Client for WtaClient {
    async fn request_permission(
        &self,
        args: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        if acp_log_enabled() {
            acp_log(&format!(
                "request_permission: {:?}",
                args.tool_call.fields.title
            ));
        }
        let description = args
            .tool_call
            .fields
            .title
            .clone()
            .unwrap_or_else(|| "Permission requested".to_string());

        let options: Vec<PermOption> = args
            .options
            .iter()
            .map(|o| PermOption {
                id: o.option_id.to_string(),
                name: o.name.clone(),
                kind: format!("{:?}", o.kind),
            })
            .collect();

        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();

        let _ = self.state.event_tx.send(AppEvent::PermissionRequest {
            description,
            options,
            responder: resp_tx,
        });

        // Wait for user to choose
        match resp_rx.await {
            Ok(option_id) => Ok(acp::RequestPermissionResponse::new(
                acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome::new(
                    option_id,
                )),
            )),
            Err(_) => Ok(acp::RequestPermissionResponse::new(
                acp::RequestPermissionOutcome::Cancelled,
            )),
        }
    }

    async fn session_notification(&self, args: acp::SessionNotification) -> acp::Result<()> {
        if acp_log_enabled() {
            acp_log(&format!("session_notification: {:?}", args.update));
        }
        match args.update {
            acp::SessionUpdate::AgentMessageChunk(chunk) => {
                if let acp::ContentBlock::Text(text_content) = chunk.content {
                    let _ = self
                        .state
                        .event_tx
                        .send(AppEvent::AgentMessageChunk(text_content.text));
                }
            }
            acp::SessionUpdate::ToolCall(tool_call) => {
                let _ = self.state.event_tx.send(AppEvent::ToolCall {
                    id: tool_call.tool_call_id.to_string(),
                    title: tool_call.title.clone(),
                    status: format!("{:?}", tool_call.status),
                });
            }
            acp::SessionUpdate::ToolCallUpdate(update) => {
                if let Some(status) = &update.fields.status {
                    let _ = self.state.event_tx.send(AppEvent::ToolCallUpdate {
                        id: update.tool_call_id.to_string(),
                        status: format!("{:?}", status),
                    });
                }
            }
            acp::SessionUpdate::Plan(plan) => {
                let entries = plan
                    .entries
                    .iter()
                    .map(|e| PlanEntry {
                        content: e.content.clone(),
                        status: match e.status {
                            acp::PlanEntryStatus::Completed => PlanEntryStatus::Completed,
                            acp::PlanEntryStatus::InProgress => PlanEntryStatus::InProgress,
                            _ => PlanEntryStatus::Pending,
                        },
                    })
                    .collect();
                let _ = self.state.event_tx.send(AppEvent::Plan(entries));
            }
            _ => {} // Ignore other update types for now
        }
        Ok(())
    }

    async fn create_terminal(
        &self,
        args: acp::CreateTerminalRequest,
    ) -> acp::Result<acp::CreateTerminalResponse> {
        if acp_log_enabled() {
            acp_log(&format!(
                "create_terminal called: cmd={} args={:?}",
                args.command, args.args
            ));
        }
        let env: Vec<(String, String)> = args
            .env
            .iter()
            .map(|e| (e.name.clone(), e.value.clone()))
            .collect();
        let cwd = args.cwd.as_ref().map(|p| p.to_string_lossy().to_string());

        let config = TerminalConfig {
            command: args.command.clone(),
            args: args.args.clone(),
            cwd,
            env,
        };

        match self.state.shell_mgr.create_terminal(config).await {
            Ok(id) => {
                // Show tool-call-like feedback
                let _ = self.state.event_tx.send(AppEvent::ToolCall {
                    id: id.clone(),
                    title: format!("{} {}", args.command, args.args.join(" ")),
                    status: "running".to_string(),
                });
                Ok(acp::CreateTerminalResponse::new(id))
            }
            Err(e) => Err(acp::Error::internal_error().data(e.to_string())),
        }
    }

    async fn terminal_output(
        &self,
        args: acp::TerminalOutputRequest,
    ) -> acp::Result<acp::TerminalOutputResponse> {
        match self
            .state
            .shell_mgr
            .get_output(&args.terminal_id.to_string())
            .await
        {
            Ok(output) => {
                let mut resp = acp::TerminalOutputResponse::new(output.data, false);
                if let Some(code) = output.exit_status {
                    resp = resp.exit_status(acp::TerminalExitStatus::new().exit_code(code));
                }
                Ok(resp)
            }
            Err(e) => Err(acp::Error::internal_error().data(e.to_string())),
        }
    }

    async fn wait_for_terminal_exit(
        &self,
        args: acp::WaitForTerminalExitRequest,
    ) -> acp::Result<acp::WaitForTerminalExitResponse> {
        let tid = args.terminal_id.to_string();

        match self.state.shell_mgr.wait_for_exit(&tid).await {
            Ok(code) => {
                // Update tool call status
                let _ = self.state.event_tx.send(AppEvent::ToolCallUpdate {
                    id: tid,
                    status: format!("exited ({})", code),
                });
                Ok(acp::WaitForTerminalExitResponse::new(
                    acp::TerminalExitStatus::new().exit_code(code),
                ))
            }
            Err(e) => Err(acp::Error::internal_error().data(e.to_string())),
        }
    }

    async fn release_terminal(
        &self,
        args: acp::ReleaseTerminalRequest,
    ) -> acp::Result<acp::ReleaseTerminalResponse> {
        let _ = self
            .state
            .shell_mgr
            .release(&args.terminal_id.to_string())
            .await;
        Ok(acp::ReleaseTerminalResponse::new())
    }

    async fn kill_terminal(
        &self,
        args: acp::KillTerminalRequest,
    ) -> acp::Result<acp::KillTerminalResponse> {
        let _ = self
            .state
            .shell_mgr
            .kill(&args.terminal_id.to_string())
            .await;
        Ok(acp::KillTerminalResponse::new())
    }
}

/// Top-level ACP client task: spawn agent, handshake, prompt loop.
pub async fn run_acp_client(
    agent_cmd: String,
    initial_prompt: Option<String>,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    mut prompt_rx: mpsc::UnboundedReceiver<String>,
    shell_mgr: Arc<ShellManager>,
    wt_connected: bool,
    pane_identity: Option<(String, String, String)>,
) {
    let startup_probe = StartupProbe::new();
    startup_probe.log(&format!(
        "run_acp_client task start agent_cmd={} wt_connected={} pane_identity={:?}",
        agent_cmd, wt_connected, pane_identity
    ));
    startup_probe.log("run_acp_client entering run_inner");
    if let Err(e) = run_inner(
        agent_cmd,
        initial_prompt,
        event_tx.clone(),
        &mut prompt_rx,
        shell_mgr,
        wt_connected,
        pane_identity,
    )
    .await
    {
        startup_probe.log(&format!("run_acp_client failed: {:#}", e));
        let _ = event_tx.send(AppEvent::AgentError(format!("{:#}", e)));
    } else {
        startup_probe.log("run_acp_client completed");
    }
}

async fn run_inner(
    agent_cmd: String,
    initial_prompt: Option<String>,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    prompt_rx: &mut mpsc::UnboundedReceiver<String>,
    shell_mgr: Arc<ShellManager>,
    wt_connected: bool,
    pane_identity: Option<(String, String, String)>,
) -> Result<()> {
    let startup_probe = StartupProbe::new();

    // Parse agent command into program + args
    let parts: Vec<&str> = agent_cmd.split_whitespace().collect();
    let program = parts
        .first()
        .ok_or_else(|| anyhow::anyhow!("empty agent command"))?;
    let args = &parts[1..];

    // Spawn agent subprocess
    let spawn_stage = format!("Spawning {}...", program);
    let _ = event_tx.send(AppEvent::ConnectionStage(spawn_stage.clone()));
    startup_probe.log(&format!("{} cmd={}", spawn_stage, agent_cmd));

    let mut child = tokio::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn agent '{}': {}", agent_cmd, e))?;

    let child_pid = child.id();
    startup_probe.log(&format!("Spawned {} pid={:?}", program, child_pid));

    let outgoing = child.stdin.take().unwrap().compat_write();
    startup_probe.log("Agent stdin pipe attached");

    let stdout = child.stdout.take().unwrap();
    startup_probe.log("Agent stdout pipe attached");
    let incoming = StartupInstrumentedReader::new(stdout, startup_probe.clone(), "stdout").compat();

    if let Some(stderr) = child.stderr.take() {
        let stderr_probe = startup_probe.clone();
        tokio::task::spawn_local(async move {
            stderr_probe.log("Agent stderr pipe attached");
            let mut lines = BufReader::new(stderr).lines();
            let mut line_no = 0usize;
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        line_no += 1;
                        stderr_probe.log(&format!("agent stderr[{line_no}]: {}", line));
                    }
                    Ok(None) => {
                        stderr_probe.log("Agent stderr closed");
                        break;
                    }
                    Err(e) => {
                        stderr_probe.log(&format!("Agent stderr read error: {}", e));
                        break;
                    }
                }
            }
        });
    }

    let child_probe = startup_probe.clone();
    tokio::task::spawn_local(async move {
        match child.wait().await {
            Ok(status) => child_probe.log(&format!("Agent process exited: {}", status)),
            Err(e) => child_probe.log(&format!("Agent wait failed: {}", e)),
        }
    });

    let state = Arc::new(ClientState {
        event_tx: event_tx.clone(),
        shell_mgr: shell_mgr.clone(),
    });

    let client = WtaClient {
        state: state.clone(),
    };

    let (conn, handle_io) = acp::ClientSideConnection::new(client, outgoing, incoming, |fut| {
        tokio::task::spawn_local(fut);
    });
    startup_probe.log("ACP client connection created");

    let io_probe = startup_probe.clone();
    tokio::task::spawn_local(async move {
        io_probe.log("ACP handle_io task started");
        if let Err(e) = handle_io.await {
            io_probe.log(&format!("ACP handle_io failed: {:#}", e));
            eprintln!("ACP I/O error: {:#}", e);
        } else {
            io_probe.log("ACP handle_io completed");
        }
    });

    // Initialize
    let _ = event_tx.send(AppEvent::ConnectionStage("Initializing ACP...".to_string()));
    startup_probe.log("Initializing ACP");
    let init_resp = conn
        .initialize(
            acp::InitializeRequest::new(acp::ProtocolVersion::V1)
                .client_capabilities(acp::ClientCapabilities::new().terminal(true))
                .client_info(
                    acp::Implementation::new("wta", env!("CARGO_PKG_VERSION"))
                        .title("Windows Terminal Agent"),
                ),
        )
        .await
        .map_err(|e| anyhow::anyhow!("initialize failed: {}", e))?;

    // Log the agent's initialize response for debugging
    startup_probe.log(&format!("Agent init response received: {:?}", init_resp));

    // Create session
    let _ = event_tx.send(AppEvent::ConnectionStage("Creating session...".to_string()));
    startup_probe.log("Creating session");
    let cwd = std::env::current_dir().unwrap_or_default();
    startup_probe.log(&format!("Using session cwd={}", cwd.display()));
    let session = conn
        .new_session(acp::NewSessionRequest::new(cwd))
        .await
        .map_err(|e| anyhow::anyhow!("new_session failed: {}", e))?;

    let session_id = session.session_id.clone();
    startup_probe.log(&format!("Session created: {}", session_id));

    // Notify app of connection
    let agent_name = program.to_string();
    let _ = event_tx.send(AppEvent::AgentConnected {
        name: agent_name,
        session_id: session_id.to_string(),
    });

    // Send initial prompt if provided
    if let Some(prompt_text) = initial_prompt {
        let _ = event_tx.send(AppEvent::AgentMessageChunk(String::new())); // trigger streaming state
        let prompt_text = build_prompt_text(
            &prompt_text,
            &shell_mgr,
            wt_connected,
            pane_identity.as_ref(),
        )
        .await;
        let result = conn
            .prompt(acp::PromptRequest::new(
                session_id.clone(),
                vec![prompt_text.into()],
            ))
            .await;
        let _ = event_tx.send(AppEvent::AgentMessageEnd);
        if let Err(e) = result {
            let _ = event_tx.send(AppEvent::AgentError(format!("prompt error: {}", e)));
        }
    }

    // Prompt loop: wait for user input, send to agent
    while let Some(text) = prompt_rx.recv().await {
        let text = build_prompt_text(&text, &shell_mgr, wt_connected, pane_identity.as_ref()).await;
        let result = conn
            .prompt(acp::PromptRequest::new(
                session_id.clone(),
                vec![text.into()],
            ))
            .await;
        let _ = event_tx.send(AppEvent::AgentMessageEnd);
        if let Err(e) = result {
            let _ = event_tx.send(AppEvent::AgentError(format!("prompt error: {}", e)));
        }
    }

    Ok(())
}
