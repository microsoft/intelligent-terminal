use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use crossterm::queue;
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

struct DeferredAcpParams {
    agent_cmd: String,
    acp_model: Option<String>,
    prompt_rx: Option<mpsc::UnboundedReceiver<crate::protocol::acp::client::PromptSubmission>>,
    cancel_rx: Option<mpsc::UnboundedReceiver<crate::protocol::acp::client::CancelRequest>>,
    new_session_rx: Option<mpsc::UnboundedReceiver<crate::protocol::acp::client::NewSessionForTab>>,
    load_session_rx: Option<mpsc::UnboundedReceiver<crate::protocol::acp::client::LoadSessionForTab>>,
    drop_session_rx: Option<mpsc::UnboundedReceiver<crate::protocol::acp::client::DropSessionRequest>>,
    restart_rx: Option<mpsc::UnboundedReceiver<crate::protocol::acp::client::RestartRequest>>,
    shell_mgr: Arc<crate::shell::ShellManager>,
    wt_connected: bool,
}

mod turn_state;

pub use turn_state::{AutofixContext, ChunkKind, SubmittedPrompt, TurnOutcome, TurnState};

use crate::commands::{self, CommandKind, CommandSpec, ParsedCommand};
use crate::coordinator::{
    parse_autofix_response, parse_recommendation_set, recommended_choice_index,
    validate_recommendation_set_for_coordinator_target, AutofixDecision, RecommendationChoice,
    RecommendationSet,
};
use crate::pane_context::PaneContext;

use crate::protocol::acp::client::{
    prompt_timing_log, CancelRequest, DropSessionRequest, LoadSessionForTab, NewSessionForTab,
    PromptSubmission, RestartRequest,
};
use crate::ui;
use crate::ui_trace;

// --- Debug types ---

#[derive(Debug, Clone)]
pub enum DebugDir {
    Sent,
    Received,
}

#[derive(Debug, Clone)]
pub struct DebugMessage {
    pub timestamp: f64,
    pub direction: DebugDir,
    pub content: String,
}

// --- Application mode ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    /// Normal agent chat (default).
    Chat,
    /// Setup / getting-started screen.
    Setup,
    /// Auth screen — agent selected but needs sign-in.
    Auth,
}

#[derive(Debug, Clone)]
pub struct AuthState {
    pub agent_id: String,
    pub agent_name: String,
    pub auth_hint: String,
    pub login_command: String,
    pub checking: bool,
    pub status_message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupReason {
    FirstRun,
    AgentMissing,
    AgentError,
    SwitchAgent,
}

impl SetupReason {
    pub fn from_str(s: &str) -> Self {
        match s {
            "first-run" => Self::FirstRun,
            "agent-missing" => Self::AgentMissing,
            "agent-error" => Self::AgentError,
            "switch-agent" => Self::SwitchAgent,
            _ => Self::FirstRun,
        }
    }

    pub fn title(&self) -> String {
        match self {
            Self::FirstRun => t!("setup.title.first_run").into_owned(),
            Self::AgentMissing => t!("setup.title.agent_missing").into_owned(),
            Self::AgentError => t!("setup.title.agent_error").into_owned(),
            Self::SwitchAgent => t!("setup.title.switch_agent").into_owned(),
        }
    }
}

/// A single option in the unified setup list.
#[derive(Debug, Clone)]
pub enum SetupOption {
    /// FRE: select this agent to use
    SelectAgent { agent: crate::agent_check::AgentStatus },
    /// Preflight: reinstall via winget (automatic)
    Reinstall { agent_id: String, display_name: String },
    /// Preflight: sign in to fix auth
    SignIn { agent_id: String, display_name: String },
    /// Preflight: switch to a different agent
    SwitchAgent { agent: crate::agent_check::AgentStatus },
    /// Preflight: retry connection (custom agent)
    Retry,
}

#[derive(Debug, Clone)]
pub struct SetupState {
    pub reason: SetupReason,
    pub selected_index: usize,
    /// Preflight result populated from `preflight::check_agent`.
    pub preflight: PreflightResult,
    /// True while a `winget install` task is running.
    pub install_in_progress: bool,
    /// Tail of the install command's output (last ~6 lines).
    pub install_log: Vec<String>,
    /// Error message from the most recent install attempt (cleared on retry).
    pub install_error: Option<String>,
    /// Unified options list for the setup screen.
    pub options: Vec<SetupOption>,
    /// Dynamic title for the setup screen.
    pub title: String,
    /// Dynamic subtitle for the setup screen.
    pub subtitle: String,
}

/// Status of a single preflight check.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckStatus {
    Checking,
    Passed,
    Failed(String),
    Skipped,
}

/// Result of all preflight checks for an agent.
#[derive(Debug, Clone)]
pub struct PreflightResult {
    pub agent_id: String,
    pub display_name: String,
    pub cli_status: CheckStatus,
    pub cli_path: Option<String>,
    pub auth_status: CheckStatus,
    pub install_hint: String,
    pub install_url: String,
    pub auth_hint: String,
}

impl PreflightResult {
    pub fn all_passed(&self) -> bool {
        self.cli_status == CheckStatus::Passed
            && matches!(self.auth_status, CheckStatus::Passed | CheckStatus::Skipped)
    }
}

/// Build the unified setup options list based on the setup reason.
///
/// - `FirstRun` / `SwitchAgent`: one `SelectAgent` per known agent.
/// - `AgentMissing` / `AgentError`: diagnostic options for the current agent
///   (reinstall, install manually, sign in, switch) depending on what failed.
pub fn build_setup_options(
    reason: &SetupReason,
    current_agent_status: Option<&crate::agent_check::AgentStatus>,
    all_agents: &[crate::agent_check::AgentStatus],
) -> Vec<SetupOption> {
    match reason {
        SetupReason::FirstRun | SetupReason::SwitchAgent => {
            // Show Copilot (always) + any detected agents
            all_agents
                .iter()
                .filter(|a| a.id == "copilot" || a.cli_found)
                .map(|a| SetupOption::SelectAgent { agent: a.clone() })
                .collect()
        }
        SetupReason::AgentMissing | SetupReason::AgentError => {
            let mut opts = Vec::new();
            if let Some(status) = current_agent_status {
                if !status.cli_found {
                    // CLI not found — offer install options
                    if status.can_auto_install() {
                        opts.push(SetupOption::Reinstall {
                            agent_id: status.id.clone(),
                            display_name: status.display_name.clone(),
                        });
                    }
                } else if !status.has_credential || *reason == SetupReason::AgentError {
                    // CLI found but auth missing or known to have failed
                    if status.id == "copilot" {
                        // Copilot: we can drive the device-flow sign-in
                        opts.push(SetupOption::SignIn {
                            agent_id: status.id.clone(),
                            display_name: status.display_name.clone(),
                        });
                    } else {
                        // Other agents: user must sign in externally, then retry
                        opts.push(SetupOption::Retry);
                    }
                }
                // If custom/unknown agent, offer retry
                if status.id == "unknown" || (!status.can_auto_install() && !status.cli_found) {
                    opts.push(SetupOption::Retry);
                }
            } else {
                opts.push(SetupOption::Retry);
            }
            opts
        }
    }
}

// --- State types ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ConnectionState {
    Disconnected,
    Connecting(String),
    Connected,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ChatMessage {
    User(String),
    Agent(String),
    System(String),
    ToolCall {
        id: String,
        title: String,
        status: String,
    },
    Plan(Vec<PlanEntry>),
    Error(String),
    /// Informational WT event surfaced inline in the chat (e.g. shell exit
    /// codes, OSC sequences). Distinct from `Error` so we can theme it
    /// differently and skip autofix wiring.
    AgentEvent(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompletedTurn {
    pub prompt: String,
    #[serde(default)]
    pub details: Vec<ChatMessage>,
    /// Whether the turn's `details` are visible in the UI. Tab to select +
    /// Enter to toggle. Default false (collapsed) so history stays compact.
    #[serde(default)]
    pub expanded: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanEntry {
    pub content: String,
    pub status: PlanEntryStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PlanEntryStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermOption {
    pub id: String,
    pub name: String,
    pub kind: String,
}

pub struct PermissionState {
    pub description: String,
    pub options: Vec<PermOption>,
    pub selected: usize,
    pub responder: Option<tokio::sync::oneshot::Sender<String>>,
}

// --- WT Event Notification ---

#[derive(Debug, Clone, PartialEq)]
pub enum WtEventSeverity {
    Critical,
    Actionable,
    Informational,
}

#[derive(Debug, Clone)]
pub struct WtNotification {
    pub severity: WtEventSeverity,
    pub pane_id: String,
    pub summary: String,
    pub acknowledged: bool,
    pub age_ticks: u32,
}

impl WtNotification {
    /// Auto-collapse informational notifications after ~5s (42 ticks at 120ms).
    /// Actionable/critical persist until dismissed.
    pub fn should_auto_dismiss(&self) -> bool {
        self.severity == WtEventSeverity::Informational && self.age_ticks > 42
    }
}

/// Open a URL in the user's default browser. Used by Setup mode's
/// "press O to open install URL" key handler.
fn open_url_in_browser(url: &str) -> std::io::Result<()> {
    std::process::Command::new("cmd")
        .args(["/c", "start", "", url])
        .spawn()?;
    Ok(())
}

/// Route a parsed `agent_event` payload into the AgentSessionRegistry.
///
/// `pane_session_id` is the **WT pane GUID** ($env:WT_SESSION in the
/// originating pane), carried in the COM broadcast as
/// `params.session_id`. It is NOT the CLI agent's own session id.
/// The agent's session id arrives as `params.agent_session_id` (the
/// `asid` local) and is what we use as the registry key when known —
/// see the module-level docs in `agent_sessions.rs` for the
/// distinction.
///
/// Returns `true` if the registry was updated and the UI should redraw.
pub fn route_agent_event_to_registry(
    reg: &mut crate::agent_sessions::AgentSessionRegistry,
    pane_session_id: &str,
    params: &serde_json::Value,
) -> bool {
    use crate::agent_sessions::{CliSource, SessionEvent};
    use std::path::PathBuf;

    let event = params.get("event").and_then(|v| v.as_str()).unwrap_or("");
    if !event.starts_with("agent.") {
        tracing::debug!(target: "agent_route", event = %event, "skipped: not agent.*");
        return false;
    }

    let cli_source = CliSource::parse(params.get("cli_source").and_then(|v| v.as_str()));
    let asid       = params.get("agent_session_id").and_then(|v| v.as_str()).unwrap_or("");
    let key        = reg.resolve_or_synthesize_key(asid, pane_session_id);
    let key_for_refresh = key.clone();
    tracing::info!(
        target: "agent_route",
        event = %event,
        asid = %asid,
        key = %key,
        pane_session_id = %pane_session_id,
        cli_source = ?cli_source,
        "routing"
    );

    let payload = params.get("payload").cloned().unwrap_or(serde_json::Value::Null);
    let cwd = payload.get("cwd")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_default();
    let cwd_label = cwd.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();

    let session_known = reg.has_session(&key);
    let synth_title: String = if session_known {
        String::new()
    } else {
        cwd_label.clone()
    };
    let needs_synthetic_start = event != "agent.session.started" && !session_known;
    if needs_synthetic_start {
        reg.apply(SessionEvent::SessionStarted {
            key: key.clone(),
            cli_source: cli_source.clone(),
            pane_session_id: pane_session_id.to_string(),
            cwd: cwd.clone(),
            title: synth_title.clone(),
        });
    }

    if event == "agent.session.started" && !asid.is_empty() {
        reg.drop_synthetic_for_pane(pane_session_id);
    }

    let ev = match event {
        "agent.session.started" | "agent.session.start" => SessionEvent::SessionStarted {
            key,
            cli_source,
            pane_session_id: pane_session_id.to_string(),
            cwd,
            title: synth_title,
        },
        "agent.tool.starting" => {
            let tool_name = payload.get("tool_name").or_else(|| payload.get("toolName"))
                .and_then(|v| v.as_str()).unwrap_or("").to_string();
            if crate::agent_sessions::is_user_input_tool(&tool_name) {
                reg.apply(SessionEvent::ToolStarting { key: key.clone(), tool_name });
                let message = payload.get("tool_input")
                    .and_then(|ti| ti.get("question")
                        .or_else(|| ti.get("prompt"))
                        .or_else(|| ti.get("message")))
                    .and_then(|v| v.as_str())
                    .unwrap_or("waiting for user input")
                    .to_string();
                SessionEvent::Notification { key, message }
            } else {
                SessionEvent::ToolStarting { key, tool_name }
            }
        },
        "agent.prompt.submit" => SessionEvent::ToolStarting {
            key,
            tool_name: "prompt".to_string(),
        },
        "agent.tool.completed" | "agent.tool.finished" | "agent.tool.failed"
        | "agent.stop" | "agent.subagent.stop" => SessionEvent::ToolCompleted { key },
        "agent.notification"   => SessionEvent::Notification {
            key,
            message: payload.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        },
        "agent.session.stopped" | "agent.session.end" => SessionEvent::SessionStopped {
            key,
            reason: payload.get("reason").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        },
        "agent.error" => SessionEvent::ConnectionFailed {
            pane_session_id: pane_session_id.to_string(),
            reason: payload.get("error").and_then(|v| v.as_str())
                .or_else(|| payload.get("message").and_then(|v| v.as_str()))
                .unwrap_or("agent error").to_string(),
        },
        _ => return reg.take_dirty(),
    };

    reg.apply(ev);

    // Upgrade synthetic title from disk if the CLI has now written one.
    if reg.title_is_synthetic(&key_for_refresh) {
        if let Some(cli) = reg.cli_source_for(&key_for_refresh) {
            if let Some(disk_title) = crate::history_loader::lookup_title_for_session(cli, &key_for_refresh) {
                reg.upgrade_title_if_synthetic(&key_for_refresh, &disk_title);
            }
        }
    }

    let dirty = reg.take_dirty();
    tracing::info!(
        target: "agent_route",
        event = %event,
        dirty = dirty,
        session_count = reg.iter_sorted().len(),
        "applied"
    );
    dirty
}

/// Classify a WT protocol event into a notification.
pub fn classify_wt_event(method: &str, pane_id: &str, params: &serde_json::Value) -> WtNotification {
    match method {
        "connection_state" => {
            let state = params
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            match state {
                "failed" => WtNotification {
                    severity: WtEventSeverity::Critical,
                    pane_id: pane_id.to_string(),
                    summary: format!("Pane {}: connection failed", pane_id),
                    acknowledged: false,
                    age_ticks: 0,
                },
                "closed" => WtNotification {
                    severity: WtEventSeverity::Actionable,
                    pane_id: pane_id.to_string(),
                    summary: format!("Pane {}: process exited", pane_id),
                    acknowledged: false,
                    age_ticks: 0,
                },
                "connected" => WtNotification {
                    severity: WtEventSeverity::Informational,
                    pane_id: pane_id.to_string(),
                    summary: format!("Pane {}: connected", pane_id),
                    acknowledged: false,
                    age_ticks: 0,
                },
                // "unknown" is sent when the C++ try_as cast fails — ignore it.
                "unknown" => return WtNotification {
                    severity: WtEventSeverity::Informational,
                    pane_id: pane_id.to_string(),
                    summary: String::new(),
                    acknowledged: true, // auto-acknowledge so it never shows
                    age_ticks: 100,     // will be auto-dismissed immediately
                },
                _ => WtNotification {
                    severity: WtEventSeverity::Informational,
                    pane_id: pane_id.to_string(),
                    summary: format!("Pane {}: {}", pane_id, state),
                    acknowledged: false,
                    age_ticks: 0,
                },
            }
        }
        "vt_sequence" => {
            let seq = params
                .get("sequence")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // OSC 133;D;<exit_code> — FinalTerm "command finished" marker.
            // Emitted by PowerShell/bash shell integration after every command.
            // Format: "osc:133;D;0" (success) or "osc:133;D;1" (failure)
            if let Some(rest) = seq.strip_prefix("osc:133;") {
                let parts: Vec<&str> = rest.splitn(2, ';').collect();
                if parts.first() == Some(&"D") {
                    let exit_code = parts.get(1)
                        .and_then(|s| s.trim().parse::<i32>().ok())
                        .unwrap_or(-1);
                    if exit_code != 0 {
                        // TODO: fetch the actual command text via
                        // wt_read_pane_output(pane_id) and include it here
                        // (e.g. "`ls /nope` failed (exit 1)"). That requires
                        // an async hop; for now surface just the exit code.
                        return WtNotification {
                            severity: WtEventSeverity::Actionable,
                            pane_id: pane_id.to_string(),
                            summary: format!("Command failed (exit {})", exit_code),
                            acknowledged: false,
                            age_ticks: 0,
                        };
                    } else {
                        // exit code 0 = success, not interesting
                        return WtNotification {
                            severity: WtEventSeverity::Informational,
                            pane_id: pane_id.to_string(),
                            summary: String::new(),
                            acknowledged: true,
                            age_ticks: 100,
                        };
                    }
                }
            }

            // All other VT sequences — not interesting, suppress.
            WtNotification {
                severity: WtEventSeverity::Informational,
                pane_id: pane_id.to_string(),
                summary: String::new(),
                acknowledged: true,
                age_ticks: 100,
            }
        }
        "agent_prompt" => {
            let prompt = params
                .get("prompt")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            WtNotification {
                severity: WtEventSeverity::Actionable,
                pane_id: pane_id.to_string(),
                summary: format!("agent_prompt:{}", prompt),
                acknowledged: false,
                age_ticks: 0,
            }
        }
        "set_view" => {
            // handle_event consumes set_view at the top of WtEvent before
            // classification runs, so classify normally never sees it.
            // Add an explicit arm anyway so a future refactor that drops
            // the early return doesn't surface a stray "Pane: set_view"
            // banner via the default catch-all.
            WtNotification {
                severity: WtEventSeverity::Informational,
                pane_id: pane_id.to_string(),
                summary: String::new(),
                acknowledged: true,
                age_ticks: 100,
            }
        }
        _ => WtNotification {
            severity: WtEventSeverity::Informational,
            pane_id: pane_id.to_string(),
            summary: format!("Pane {}: {}", pane_id, method),
            acknowledged: false,
            age_ticks: 0,
        },
    }
}

// --- Events ---

/// One entry of an ACP agent's advertised model list, mirrored into the
/// `agent_status` event so the XAML settings page can populate a real
/// dropdown instead of asking the user to type a free-form string.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AcpModelInfo {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Test-visible record of a wtcli command the App fired through the
/// `wt_channel::spawn_*` helpers. Captured under `cfg(test)` so we can
/// assert the F2 Agents view dispatches the right shape of command
/// without needing a live wtcli to verify against.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatchedCommandKind {
    FocusPane,
    /// Plain Enter on a terminal-state row in the session management
    /// view: open a new WT tab whose primary pane runs
    /// `<cli> --resume <key>`. (Previously this was a split-pane in the
    /// current tab — see commit history; the new-tab variant keeps the
    /// originating tab clean and matches user expectation that
    /// resuming a historical session is a "go open my session" action,
    /// not a "split my workspace" action.)
    NewTabResume,
    /// Shift+Enter in the session management view — resume a historical
    /// session in the agent pane of a new tab via WT-side coordination +
    /// ACP session/load.
    ResumeInAgentPane,
}

#[cfg(test)]
#[derive(Clone, Debug)]
pub struct DispatchedCommand {
    pub kind: DispatchedCommandKind,
    pub session_id: Option<String>,
    pub argv: Vec<String>,
}

pub enum AppEvent {
    Key(KeyEvent),
    /// Mouse wheel scroll: delta<0 = scroll up, delta>0 = scroll down, row = terminal row of event
    MouseScroll { delta: i32, row: u16 },
    Tick,
    Resize(u16, u16), // terminal resize (handled by ratatui)
    ConnectionStage(String),
    /// `session_id` lets us route the status update to the originating tab
    /// once an ACP session is bound to it. Pre-session statuses (startup
    /// stages) carry None and fall through to the active tab.
    ProgressStatus {
        session_id: Option<String>,
        status: String,
    },
    AgentConnected {
        name: String,
        model: Option<String>,
        version: Option<String>,
        /// Session id for the implicitly-created `DEFAULT_TAB_ID` ("0")
        /// session at startup. Wires into App.session_to_tab. Other tabs
        /// get their own sessions lazily on first prompt — see
        /// `SessionAttached`.
        session_id: String,
        /// ACP-advertised models (NewSessionResponse.models.available_models).
        /// Empty when the agent didn't fill the field.
        available_models: Vec<AcpModelInfo>,
        /// ACP-advertised current model id (NewSessionResponse.models.current_model_id).
        current_model_id: Option<String>,
        /// Whether the agent advertised the `loadSession` capability in
        /// the initialize response. Used by the session-management
        /// view's Shift+Enter handler to short-circuit with a clear
        /// error before opening a new tab when the agent can't
        /// rehydrate ACP sessions.
        load_session_supported: bool,
    },
    /// A new ACP session has been created and bound to a tab. Carries the
    /// per-tab model list (each ACP session can advertise its own).
    SessionAttached {
        tab_id: String,
        session_id: String,
        available_models: Vec<AcpModelInfo>,
        current_model_id: Option<String>,
    },
    /// Error scoped to a specific tab. Used by paths that know the tab
    /// (e.g. ACP `session/load` failure) but have no session_id yet
    /// because the session never came up. Routes into that tab's chat as
    /// a normal Error message; does NOT bounce through the auth/global
    /// disconnect fallback that `AgentError` triggers.
    TabError {
        tab_id: String,
        message: String,
    },
    /// Informational system message scoped to a specific tab. Used for
    /// session/load progress notes ("Resuming...", "Session loaded.")
    /// where we want the user to see something before the agent's
    /// session/update replay (if any) arrives.
    TabSystemMessage {
        tab_id: String,
        message: String,
    },
    PromptTemplateLoaded {
        name: String,
    },
    /// Errors raised before a session exists carry None for `session_id`
    /// and route to the active tab; in-flight failures route to the
    /// session's tab.
    AgentError {
        session_id: Option<String>,
        message: String,
    },
    /// Same-tab single-flight guard rejection. The user submitted a new
    /// prompt while the previous one is still in flight on the same tab.
    /// The ACP client side enforces this for safety; the front-end Enter
    /// handler also has its own guard so the bounce is rare.
    AgentBusy {
        tab_id: String,
    },
    ExecutionInfo(String),
    AgentThoughtChunk {
        session_id: String,
        text: String,
    },
    AgentMessageChunk {
        session_id: String,
        text: String,
    },
    /// A `user_message_chunk` SessionUpdate received from the agent
    /// during an ACP `session/load` replay. Carries the historical
    /// user prompt that opens the next replayed turn. Accumulated into
    /// `pending_user_replay` and flushed as a `ChatMessage::User` when
    /// the next agent/tool/plan chunk lands or the load completes.
    /// Outside of `loading_session` mode, dropped — copilot uses these
    /// only during load.
    UserMessageReplayChunk {
        session_id: String,
        text: String,
    },
    AgentMessageEnd {
        session_id: String,
    },
    TimingMetric {
        session_id: String,
        note: String,
    },
    ToolCall {
        session_id: String,
        id: String,
        title: String,
        status: String,
    },
    ToolCallUpdate {
        session_id: String,
        id: String,
        status: String,
    },
    Plan {
        session_id: String,
        entries: Vec<PlanEntry>,
    },
    PermissionRequest {
        session_id: String,
        description: String,
        options: Vec<PermOption>,
        responder: tokio::sync::oneshot::Sender<String>,
    },
    SystemMessage(String),
    DebugPipeMessage(DebugMessage),
    /// Push event from Windows Terminal protocol (VT sequence or connection state).
    WtEvent {
        method: String,
        pane_id: String,
        params: serde_json::Value,
    },
    /// Background agent install completed — refresh the detected agents list.
    AgentInstallComplete,
    /// Login progress — device code received, display to user.
    LoginProgress { device_code: String, verify_url: String },
    /// Login flow completed.
    LoginComplete { agent_id: String, success: bool },
    /// Result of `preflight::check_agent` run by main.rs before the TUI
    /// loop starts. If `all_passed()` is false the App switches into
    /// `AppMode::Setup` so the user can install / authenticate the CLI.
    PreflightComplete(PreflightResult),
    /// Background-thread callback from `wt_channel::spawn_wtcli_split_then_focus_with_callback`
    /// (used by `dispatch_resume`) reaches the registry through this variant.
    /// Posting via the main loop keeps `agent_sessions` access single-threaded
    /// and lets `tracing::*` calls emit on a stable thread.
    AgentSessionEvent(crate::agent_sessions::SessionEvent),
    /// Historical agent sessions scanned off the main thread by a
    /// `spawn_blocking` task wrapping `history_loader::load_all()`. Posted
    /// instead of running the scan inline so a large `~/.copilot/session-state`
    /// (hundreds of dirs, each with an `events.jsonl` to sniff) doesn't block
    /// the LocalSet — which would otherwise stall `run_acp_client`,
    /// the first frame, and therefore the user-visible "connecting" state.
    HistoricalSessionsLoaded(Vec<crate::agent_sessions::AgentSession>),
}

// --- Per-tab session storage ---

pub(crate) const DEFAULT_TAB_ID: &str = "0";

/// Single-axis scroll cursor. All mutations go through methods so callers
/// don't reinvent saturating-math; the upper bound `max` is established by
/// the layout/render pass once total content height is known and re-clamps
/// on every frame.
///
/// `by` deliberately does NOT clamp to `max` — the bound may be stale at
/// input time (the lazy chat build only learns `max` after exhausting
/// history). Clamping happens on the next `set_max`.
#[derive(Debug, Default, Clone, Copy)]
pub struct Scroll {
    pub offset: usize,
    pub max: usize,
}

impl Scroll {
    pub fn by(&mut self, delta: isize) {
        self.offset = if delta >= 0 {
            self.offset.saturating_add(delta as usize)
        } else {
            self.offset.saturating_sub(delta.unsigned_abs())
        };
    }

    /// Jump to an absolute offset, clamped to current `max`. Only meaningful
    /// after `max` has been set this frame.
    pub fn set(&mut self, offset: usize) {
        self.offset = offset.min(self.max);
    }

    pub fn set_max(&mut self, max: usize) {
        self.max = max;
        if self.offset > max {
            self.offset = max;
        }
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Everything that conceptually belongs to one tab's conversation: the
/// message history, the streaming buffer of the in-flight prompt, the
/// pending tool calls, the recommendations panel state, etc.
///
/// `App` holds a `HashMap<TabId, TabSession>` and a `tab_id` pointing at
/// the currently focused entry. Renderers read via `app.current_tab()`;
/// event handlers route updates to the relevant `TabSession` rather than
/// mutating shared `App` fields.
#[derive(Default)]
pub struct TabSession {
    // Conversation history
    pub messages: Vec<ChatMessage>,
    pub completed_turns: Vec<CompletedTurn>,
    /// Tab/Shift+Tab selects a past turn (most recent first). Enter then
    /// toggles `CompletedTurn.expanded`. None means no selection — Enter
    /// goes to the input/prompt path as before.
    pub selected_completed_turn_idx: Option<usize>,
    pub chat_scroll: Scroll,

    // Streaming state
    pub pending_agent_response: String,
    /// Accumulator for `session/update` user_message_chunk events
    /// arriving during an ACP `session/load` replay (the historical
    /// user prompt for the next replayed turn). Flushed as a
    /// `ChatMessage::User` whenever a turn boundary is detected — an
    /// agent message / thought / tool call starts, OR the load
    /// completes (SessionAttached for the loading tab).
    pub pending_user_replay: String,
    /// True between the inbound `load_session` event and the
    /// `SessionAttached` event that closes out the ACP `session/load`
    /// call. While set, session/update chunk handlers accept chunks
    /// even though no `TurnState::Submitted` was created for the
    /// replay — `turn` stays Idle through the load.
    pub loading_session: bool,
    // Explicit per-turn lifecycle. Source of truth in the new state machine
    // (see `doc/specs/turn-state-refactor.md`).
    pub turn: TurnState,

    // Agent-supplied progress message (e.g. "Reading file foo.rs"). Falls
    // back to the spinner label derived from `turn` when None.
    pub progress_status: Option<String>,
    pub activity_frame: usize,
    pub timing_note: Option<String>,
    pub selection_visible_pending: bool,

    // Tool calls / permission
    pub tool_calls: HashMap<String, (String, String)>,
    pub permission: Option<PermissionState>,
    // Recommendation card UI focus (the set itself lives on
    // `turn.recommendations()`).
    pub selected_recommendation: usize,
    pub selected_button: usize,
    pub rec_scroll: Scroll,


    // Input editor state — per-tab so each tab keeps its own draft text,
    // cursor, and slash-command popup across switches.
    pub input: String,
    pub cursor_pos: usize,
    /// Recomputed on every input mutation. Empty when not in
    /// command-prefix mode. The popup renderer treats an empty Vec as
    /// "do not render".
    pub command_popup_candidates: Vec<&'static CommandSpec>,
    /// Index into [`Self::command_popup_candidates`]. Clamped on every
    /// mutation that could shrink the list.
    pub command_popup_selected: usize,

    // Filled in Milestone 2 once each tab has its own ACP SessionId.
    #[allow(dead_code)]
    pub session_id: Option<String>,

    // Agents picker view (F2 / `/sessions`) — per-tab so each WT tab keeps
    // its own open/closed state and selected row across tab switches.
    pub current_view: View,
    pub agents_list_state: ratatui::widgets::ListState,
}

impl TabSession {
    pub fn scroll_to_bottom(&mut self) {
        self.chat_scroll.offset = 0;
    }

    pub fn clear_recommendations(&mut self) {
        self.selected_recommendation = 0;
        self.selected_button = 0;
        self.rec_scroll.reset();
    }

    pub fn clear_chat_history(&mut self) {
        self.messages.clear();
        self.tool_calls.clear();
        self.permission = None;
        self.progress_status = None;
        self.activity_frame = 0;
        self.pending_agent_response.clear();
        self.pending_user_replay.clear();
        self.chat_scroll.reset();
        self.timing_note = None;
        self.selection_visible_pending = false;
        self.turn = TurnState::Idle;
        self.clear_recommendations();
    }

    /// Flush pending user/agent replay buffers at a turn boundary during
    /// an ACP `session/load`. Called when a new user_message_chunk
    /// arrives (the previous agent turn is complete) and again at end
    /// of load to drain whatever remains. Empty buffers no-op.
    pub fn flush_load_replay_pending(&mut self) {
        if !self.pending_user_replay.is_empty() {
            let text = std::mem::take(&mut self.pending_user_replay);
            self.messages.push(ChatMessage::User(text));
        }
        if !self.pending_agent_response.is_empty() {
            let text = std::mem::take(&mut self.pending_agent_response);
            self.messages.push(ChatMessage::Agent(text));
        }
    }

    /// Cycle the past-turn selection toward older entries.
    /// `None → last (most recent) → ... → 0 → None`. No-op when there are
    /// no completed turns.
    pub fn select_older_completed_turn(&mut self) {
        let len = self.completed_turns.len();
        if len == 0 {
            self.selected_completed_turn_idx = None;
            return;
        }
        self.selected_completed_turn_idx = match self.selected_completed_turn_idx {
            None => Some(len - 1),
            Some(0) => None,
            Some(i) => Some(i - 1),
        };
    }

    /// Cycle the past-turn selection toward newer entries.
    /// `None → 0 (oldest) → ... → last → None`.
    pub fn select_newer_completed_turn(&mut self) {
        let len = self.completed_turns.len();
        if len == 0 {
            self.selected_completed_turn_idx = None;
            return;
        }
        self.selected_completed_turn_idx = match self.selected_completed_turn_idx {
            None => Some(0),
            Some(i) if i + 1 >= len => None,
            Some(i) => Some(i + 1),
        };
    }

    /// Flip `expanded` on the currently selected past turn. No-op if nothing
    /// is selected or the index is out of range (defensive — selection
    /// should track turn count, but a stale index shouldn't panic).
    pub fn toggle_selected_completed_turn(&mut self) {
        let Some(idx) = self.selected_completed_turn_idx else {
            return;
        };
        if let Some(turn) = self.completed_turns.get_mut(idx) {
            turn.expanded = !turn.expanded;
        }
    }

    pub fn current_turn_details(&self) -> Vec<ChatMessage> {
        self.messages
            .iter()
            .filter(|message| !matches!(message, ChatMessage::User(_)))
            .cloned()
            .collect()
    }

    pub fn clear_input(&mut self) {
        self.input.clear();
        self.cursor_pos = 0;
        self.refresh_command_popup();
    }

    pub fn insert_input_char(&mut self, ch: char) {
        self.cursor_pos = clamp_cursor_to_boundary(&self.input, self.cursor_pos);
        self.input.insert(self.cursor_pos, ch);
        self.cursor_pos += ch.len_utf8();
        self.refresh_command_popup();
    }

    pub fn delete_before_cursor(&mut self) {
        self.cursor_pos = clamp_cursor_to_boundary(&self.input, self.cursor_pos);
        if self.cursor_pos == 0 {
            return;
        }

        let previous = prev_char_boundary(&self.input, self.cursor_pos);
        self.input.replace_range(previous..self.cursor_pos, "");
        self.cursor_pos = previous;
        self.refresh_command_popup();
    }

    pub fn delete_at_cursor(&mut self) {
        self.cursor_pos = clamp_cursor_to_boundary(&self.input, self.cursor_pos);
        if self.cursor_pos >= self.input.len() {
            return;
        }

        let next = next_char_boundary(&self.input, self.cursor_pos);
        self.input.replace_range(self.cursor_pos..next, "");
        self.refresh_command_popup();
    }

    pub fn move_cursor_left(&mut self) {
        self.cursor_pos = prev_char_boundary(&self.input, self.cursor_pos);
    }

    pub fn move_cursor_right(&mut self) {
        self.cursor_pos = next_char_boundary(&self.input, self.cursor_pos);
    }

    pub fn move_cursor_word_left(&mut self) {
        self.cursor_pos = prev_word_boundary(&self.input, self.cursor_pos);
    }

    pub fn move_cursor_word_right(&mut self) {
        self.cursor_pos = next_word_boundary(&self.input, self.cursor_pos);
    }

    pub fn move_cursor_home(&mut self) {
        self.cursor_pos = 0;
    }

    pub fn move_cursor_end(&mut self) {
        self.cursor_pos = self.input.len();
    }

    /// Recompute the slash-command popup candidates from the current
    /// input. Called after every input mutation. Clamps the selected
    /// index so it stays valid when the candidate list shrinks.
    pub fn refresh_command_popup(&mut self) {
        if commands::is_command_prefix(&self.input) {
            // Strip leading whitespace + the `/` to get the user's
            // partial name. `is_command_prefix` already guarantees the
            // shape, so the unwrap is safe.
            let trimmed = self.input.trim_start();
            let name = trimmed.strip_prefix('/').unwrap_or("");
            self.command_popup_candidates = commands::matches(name);
        } else {
            self.command_popup_candidates.clear();
        }
        if self.command_popup_candidates.is_empty() {
            self.command_popup_selected = 0;
        } else if self.command_popup_selected >= self.command_popup_candidates.len() {
            self.command_popup_selected = self.command_popup_candidates.len() - 1;
        }
    }

    pub fn command_popup_visible(&self) -> bool {
        !self.command_popup_candidates.is_empty()
    }

    pub fn command_popup_up(&mut self) {
        if self.command_popup_selected > 0 {
            self.command_popup_selected -= 1;
        }
    }

    pub fn command_popup_down(&mut self) {
        if self.command_popup_selected + 1 < self.command_popup_candidates.len() {
            self.command_popup_selected += 1;
        }
    }

    pub fn selected_command_spec(&self) -> Option<&'static CommandSpec> {
        self.command_popup_candidates
            .get(self.command_popup_selected)
            .copied()
    }

    /// Tab-completion: replace the input buffer with `/<name> ` (with a
    /// trailing space if the command takes args, otherwise just the
    /// name) and reset the cursor to the end. Triggered by Tab when the
    /// popup is visible.
    pub fn accept_command_popup_completion(&mut self) {
        if let Some(spec) = self.selected_command_spec() {
            self.input = if spec.takes_args {
                format!("/{} ", spec.name)
            } else {
                format!("/{}", spec.name)
            };
            self.cursor_pos = self.input.len();
            self.refresh_command_popup();
        }
    }
}

// --- App ---

pub struct App {
    pub mode: AppMode,
    pub setup: Option<SetupState>,
    pub auth: Option<AuthState>,
    /// Channel for spawning background tasks from event handlers.
    event_tx: Option<mpsc::UnboundedSender<AppEvent>>,
    /// Set after login completes — consumed by main loop to spawn ACP client.
    pub pending_acp_start: bool,
    /// Agent ID selected by user (FRE/preflight) — sent to C++ once connected.
    pending_agent_selection: Option<String>,
    /// Show first-run welcome hint until user sends first message.
    pub show_welcome_hint: bool,
    deferred_acp: Option<DeferredAcpParams>,
    pub state: ConnectionState,
    /// The agent ID we're trying to connect to (set at preflight/FRE time).
    pub current_agent_id: String,
    /// True when preflight detected an issue and is showing Setup screen.
    /// Prevents AgentError from overriding the preflight Setup.
    preflight_setup_active: bool,
    pub agent_name: String,
    pub agent_model: Option<String>,
    pub agent_version: Option<String>,
    /// Models the ACP agent advertised at session start. Empty until the
    /// first AgentConnected event with non-empty data; published into the
    /// `agent_status` event so the settings UI can render a dropdown.
    pub available_models: Vec<AcpModelInfo>,
    pub current_model_id: Option<String>,
    pub prompt_name: Option<String>,
    pub session_id: String,
    #[allow(dead_code)]
    pub wt_connected: bool,
    pub terminal_rows: u16,
    pub terminal_cols: u16,
    pub should_quit: bool,
    prompt_tx: mpsc::UnboundedSender<PromptSubmission>,
    recommendation_tx: mpsc::UnboundedSender<crate::coordinator::ChoiceExecution>,
    permission_tx: mpsc::UnboundedSender<String>,
    cancel_tx: mpsc::UnboundedSender<CancelRequest>,
    new_session_tx: mpsc::UnboundedSender<NewSessionForTab>,
    load_session_tx: mpsc::UnboundedSender<LoadSessionForTab>,
    drop_session_tx: mpsc::UnboundedSender<DropSessionRequest>,
    restart_tx: mpsc::UnboundedSender<RestartRequest>,
    debug_capture_enabled: Arc<AtomicBool>,
    // Slash-command UI state. The /help overlay is global — it covers
    // the chat area regardless of which tab is active. Per-tab popup
    // state (the command-completion candidates as the user types `/he…`)
    // lives on `TabSession`.
    pub help_overlay_visible: bool,
    // Debug panel
    pub debug_messages: Vec<DebugMessage>,
    pub show_debug_panel: bool,
    pub debug_scroll: usize,
    // Pane identity (populated via VT channel)
    pub pane_id: Option<String>,
    pub tab_id: Option<String>,
    pub window_id: Option<String>,
    // WT event notifications (global — affects bottom-bar / banner across tabs)
    pub wt_notifications: std::collections::VecDeque<WtNotification>,
    pub show_notification_banner: bool,
    // Auto-fix: the pane ID where the error occurred (used to auto-fill Send parent)
    pub autofix_pane_id: Option<String>,
    // Auto-fix Suggested state: pane ID with a non-actionable suggestion shown on
    // the bottom bar. Cleared when the user runs a successful command in the
    // same pane (signal that they've moved on) or when a new autofix triggers.
    pub suggested_pane_id: Option<String>,
    pub autofix_enabled: bool,
    // Generation counter: incremented on every new trigger or cancel.
    // Snapshotted into `AutofixContext.generation` at submit time; chunks /
    // close events whose snapshot doesn't match the current value are
    // discarded by the state machine.
    autofix_generation: u64,
    // Per-tab conversation sessions. Keyed by the stable tab GUID WT mints
    // at tab construction. The active tab is `tab_id` — seeded from the
    // `--owner-tab-id` CLI arg before ACP init in the WT-spawned path, or
    // None (falling back to `DEFAULT_TAB_ID`) for manual `wta` runs.
    // Lazily extended on each new `tab_changed` event.
    pub(crate) tab_sessions: HashMap<String, TabSession>,
    // Reverse lookup: ACP `SessionId` → tab id. Populated from
    // `AgentConnected` (the startup session, bound to whichever tab the
    // process owns) and `SessionAttached` (lazily-created sessions for
    // other tabs the user has visited). All ACP-emitted events route via
    // this map: chunks, tool calls, end notifications all carry a
    // `session_id`, the App looks up the owning tab and writes to that
    // `TabSession`.
    session_to_tab: HashMap<String, String>,
    // ── Agent management view state (re-applied on top of theirs) ──
    /// Live & historical CLI agent sessions. Populated from `agent_event`
    /// hook payloads via `route_agent_event_to_registry`. Cross-tab — the
    /// session list itself is global; only the *picker view* (open state
    /// + selected row) lives per-tab on `TabSession`.
    pub agent_sessions: crate::agent_sessions::AgentSessionRegistry,
    /// Tracks the lazy load of historical sessions. Flipped to Loading
    /// on first session-management-view open; flipped to Loaded when
    /// `HistoricalSessionsLoaded` arrives. The agents_view reads this to
    /// render a "Loading..." row instead of an empty list during the
    /// scan.
    pub history_load_state: HistoryLoadState,
    /// Whether the connected ACP agent advertised the `loadSession`
    /// capability in its initialize response. Used by the
    /// session-management view's Shift+Enter handler to short-circuit
    /// with a clear error before opening a new tab when the agent
    /// can't rehydrate ACP sessions. Set on `AgentConnected`.
    pub agent_supports_load_session: bool,
    // Onboarding: signals main.rs to install agent hook plugins on demand.
    install_request_tx: Option<mpsc::UnboundedSender<()>>,
    /// Posts `AppEvent::AgentSessionEvent` from background callbacks
    /// (split-pane callback in `dispatch_resume`) back into the main
    /// event loop so they can apply to `agent_sessions` on the UI thread.
    /// Set by `set_agent_event_tx` from main.rs after the event channel
    /// is constructed; remains None in tests so dispatch_resume is a
    /// no-op outside the integration loop.
    agent_event_tx: Option<mpsc::UnboundedSender<AppEvent>>,
    /// Test-only: last command issued via the F2 Agents view's Enter
    /// dispatch (`dispatch_resume` / focus). Used by unit tests in
    /// place of a live wtcli; not compiled into release builds.
    #[cfg(test)]
    pub last_dispatched_command: Option<DispatchedCommand>,
    /// Source pane GUID (set from `WTA_SOURCE_SESSION_ID` env var by the
    /// launching pane). Used by autofix to attribute which pane originated
    /// the failing command we're about to fix.
    pub source_session_id: Option<String>,
    /// Source pane working directory (set from `WTA_SOURCE_CWD`).
    pub source_cwd: Option<String>,
    /// When true, surface raw `agent_event` payloads in the chat as
    /// `ChatMessage::AgentEvent` for diagnostics. Controlled by the
    /// `WTA_LOG_AGENT_EVENT` env var (1/true/yes).
    pub log_agent_events: bool,
    /// Spinner tick counter used by Setup mode (per-tab `activity_frame`
    /// drives chat-mode spinners; this one is for the wizard view which
    /// has no tab context). Bumped from the Tick handler when in Setup.
    pub activity_frame: u8,

    /// First-press timestamp of the double-Ctrl+C "close pane" sequence. Set
    /// when the user presses Ctrl+C while input is empty and nothing is in
    /// flight. A second Ctrl+C within `CLOSE_PANE_ARM_WINDOW` closes the
    /// pane (we ask WT to do it; ConPty then SIGKILLs us). Cleared on any
    /// other key, on prompt activity, or after the window elapses.
    pub close_pane_armed_at: Option<std::time::Instant>,
    /// Transient one-line hint rendered at the bottom of the chat area
    /// (e.g. "Press Ctrl+C again to close pane"). Auto-clears at the
    /// recorded deadline.
    pub transient_hint: Option<(String, std::time::Instant)>,
}

/// How long the "Press Ctrl+C again to close pane" arm stays live. Long
/// enough that the user can react after seeing the hint; short enough that
/// a stale arm doesn't bite the next time they want to clear input.
pub const CLOSE_PANE_ARM_WINDOW: std::time::Duration = std::time::Duration::from_millis(1500);

/// Top-level UI view selector. Toggled with F2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Chat,
    Agents,
}

impl Default for View {
    fn default() -> Self {
        View::Chat
    }
}

/// Lazy-load tracking for the historical `agent_sessions` registry.
///
/// `history_loader::load_all()` scans `~/.copilot/session-state`,
/// `~/.claude/projects`, `~/.gemini/tmp` and reads `events.jsonl`
/// from each Copilot session to sniff the wta-internal autofix
/// fingerprint. On a populated machine that's hundreds of file
/// opens — observed ~10 seconds.
///
/// Doing that eagerly on every wta spawn (including every model
/// switch, which kills the old wta and starts a new one) is wasted
/// work — the data is only consumed by the Agents view (F2). We
/// defer the scan to the first F2 press and cache the result for
/// the rest of this wta's lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryLoadState {
    NotStarted,
    Loading,
    Loaded,
}

impl Default for HistoryLoadState {
    fn default() -> Self {
        HistoryLoadState::NotStarted
    }
}

impl App {
    pub fn new(
        prompt_tx: mpsc::UnboundedSender<PromptSubmission>,
        recommendation_tx: mpsc::UnboundedSender<crate::coordinator::ChoiceExecution>,
        permission_tx: mpsc::UnboundedSender<String>,
        cancel_tx: mpsc::UnboundedSender<CancelRequest>,
        new_session_tx: mpsc::UnboundedSender<NewSessionForTab>,
        load_session_tx: mpsc::UnboundedSender<LoadSessionForTab>,
        drop_session_tx: mpsc::UnboundedSender<DropSessionRequest>,
        restart_tx: mpsc::UnboundedSender<RestartRequest>,
        debug_capture_enabled: Arc<AtomicBool>,
        wt_connected: bool,
        autofix_enabled: bool,
    ) -> Self {
        let mut tab_sessions = HashMap::new();
        tab_sessions.insert(DEFAULT_TAB_ID.to_string(), TabSession::default());
        Self {
            mode: AppMode::Chat,
            setup: None,
            auth: None,
            event_tx: None,
            pending_acp_start: false,
            pending_agent_selection: None,
            show_welcome_hint: false,
            deferred_acp: None,
            state: ConnectionState::Connecting(t!("connection.starting").into_owned()),
            current_agent_id: String::new(),
            preflight_setup_active: false,
            agent_name: String::new(),
            agent_model: None,
            agent_version: None,
            available_models: Vec::new(),
            current_model_id: None,
            prompt_name: None,
            session_id: String::new(),
            wt_connected,
            terminal_rows: 24,
            terminal_cols: 80,
            should_quit: false,
            prompt_tx,
            recommendation_tx,
            permission_tx,
            cancel_tx,
            new_session_tx,
            load_session_tx,
            drop_session_tx,
            restart_tx,
            debug_capture_enabled,
            help_overlay_visible: false,
            debug_messages: Vec::new(),
            show_debug_panel: false,
            debug_scroll: 0,
            pane_id: None,
            tab_id: None,
            window_id: None,
            wt_notifications: VecDeque::new(),
            show_notification_banner: false,
            autofix_pane_id: None,
            suggested_pane_id: None,
            autofix_enabled,
            autofix_generation: 0,
            tab_sessions,
            session_to_tab: HashMap::new(),
            agent_sessions: crate::agent_sessions::AgentSessionRegistry::new(),
            history_load_state: HistoryLoadState::NotStarted,
            agent_supports_load_session: false,
            install_request_tx: None,
            agent_event_tx: None,
            #[cfg(test)]
            last_dispatched_command: None,
            source_session_id: None,
            source_cwd: None,
            log_agent_events: false,
            activity_frame: 0,
            close_pane_armed_at: None,
            transient_hint: None,
        }
    }

    /// Store ACP launch parameters for deferred start (after login).
    pub fn set_acp_params(
        &mut self,
        agent_cmd: String,
        acp_model: Option<String>,
        prompt_rx: mpsc::UnboundedReceiver<crate::protocol::acp::client::PromptSubmission>,
        cancel_rx: mpsc::UnboundedReceiver<crate::protocol::acp::client::CancelRequest>,
        new_session_rx: mpsc::UnboundedReceiver<crate::protocol::acp::client::NewSessionForTab>,
        load_session_rx: mpsc::UnboundedReceiver<crate::protocol::acp::client::LoadSessionForTab>,
        drop_session_rx: mpsc::UnboundedReceiver<crate::protocol::acp::client::DropSessionRequest>,
        restart_rx: mpsc::UnboundedReceiver<crate::protocol::acp::client::RestartRequest>,
        shell_mgr: Arc<crate::shell::ShellManager>,
        wt_connected: bool,
    ) {
        self.deferred_acp = Some(DeferredAcpParams {
            agent_cmd,
            acp_model,
            prompt_rx: Some(prompt_rx),
            cancel_rx: Some(cancel_rx),
            new_session_rx: Some(new_session_rx),
            load_session_rx: Some(load_session_rx),
            drop_session_rx: Some(drop_session_rx),
            restart_rx: Some(restart_rx),
            shell_mgr,
            wt_connected,
        });
    }

    /// Try to start the ACP client if login just completed.
    /// Creates fresh channels if previous ones were consumed by a failed attempt.
    pub fn try_start_acp(&mut self) {
        if !self.pending_acp_start {
            return;
        }
        self.pending_acp_start = false;
        tracing::info!(target: "acp", has_event_tx = self.event_tx.is_some(), has_deferred = self.deferred_acp.is_some(), "try_start_acp triggered");

        if let (Some(ref tx), Some(ref mut params)) = (&self.event_tx, &mut self.deferred_acp) {
            // If channels were consumed by a previous (failed) attempt, create fresh ones.
            // Also update self.prompt_tx so the App sends prompts to the new ACP client.
            if params.prompt_rx.is_none() {
                let (ptx, prx) = mpsc::unbounded_channel();
                let (_ctx, crx) = mpsc::unbounded_channel();
                let (_ntx, nrx) = mpsc::unbounded_channel();
                let (_ltx, lrx) = mpsc::unbounded_channel();
                let (_dtx, drx) = mpsc::unbounded_channel();
                let (_rtx, rrx) = mpsc::unbounded_channel();
                self.prompt_tx = ptx;
                params.prompt_rx = Some(prx);
                params.cancel_rx = Some(crx);
                params.new_session_rx = Some(nrx);
                params.load_session_rx = Some(lrx);
                params.drop_session_rx = Some(drx);
                params.restart_rx = Some(rrx);
            }

            if let (
                Some(prompt_rx),
                Some(cancel_rx),
                Some(new_session_rx),
                Some(load_session_rx),
                Some(drop_session_rx),
                Some(restart_rx),
            ) = (
                params.prompt_rx.take(),
                params.cancel_rx.take(),
                params.new_session_rx.take(),
                params.load_session_rx.take(),
                params.drop_session_rx.take(),
                params.restart_rx.take(),
            ) {
                // Resolve the agent executable path (bare "copilot" may not
                // be on PATH in packaged apps — use WinGet Links fallback).
                let agent_cmd = resolve_agent_cmd(&params.agent_cmd);
                let acp_model = params.acp_model.clone();
                let owner_tab_id = self.tab_id.clone();
                let event_tx = tx.clone();
                let shell_mgr = Arc::clone(&params.shell_mgr);
                let wt_connected = params.wt_connected;

                tokio::task::spawn_local(crate::protocol::acp::client::run_acp_client(
                    agent_cmd,
                    acp_model,
                    owner_tab_id,
                    event_tx,
                    prompt_rx,
                    cancel_rx,
                    new_session_rx,
                    load_session_rx,
                    drop_session_rx,
                    restart_rx,
                    shell_mgr,
                    wt_connected,
                ));
            }
        }
    }

    /// Wire a sender that signals main.rs to run the agent-hooks installer
    /// (Settings UI -> Install button -> main.rs spawns
    /// `agent_hooks_installer::ensure_installed`).
    pub fn set_install_request_tx(&mut self, tx: mpsc::UnboundedSender<()>) {
        self.install_request_tx = Some(tx);
    }

    /// Wire the main loop's `AppEvent` sender so background callbacks
    /// (e.g. `dispatch_resume`'s split-pane completion) can post
    /// `AgentSessionEvent`s back into the event loop instead of needing
    /// shared mutable access to `agent_sessions`.
    pub fn set_agent_event_tx(&mut self, tx: mpsc::UnboundedSender<AppEvent>) {
        self.agent_event_tx = Some(tx);
    }

    /// Trigger an install-hooks request. No-op if no channel is wired
    /// (e.g. running outside the packaged app).
    #[allow(dead_code)]
    pub fn request_install_hooks(&self) {
        if let Some(tx) = &self.install_request_tx {
            let _ = tx.send(());
        }
    }

    /// Filter to apply to the F2 session-management view based on which
    /// agent CLI the WTA agent pane is currently driving. Returns
    /// `Some(CliSource::*)` when `current_agent_id` resolves to a tracked
    /// CLI (copilot / claude / gemini) so only matching rows are listed.
    /// Returns `None` when no agent has been selected yet or the agent is
    /// not one the session registry tracks (codex / unknown) — in that
    /// case the view falls back to showing every row so the user can still
    /// see and resume their history.
    pub fn current_cli_filter(&self) -> Option<crate::agent_sessions::CliSource> {
        crate::agent_sessions::CliSource::from_agent_id(&self.current_agent_id)
    }

    /// Enter handler for the F2 Agents view. For live rows (Idle / Working
    /// / Attention / Error), focus the underlying WT pane. For terminal-
    /// state rows (Ended / Historical), spawn a new pane that runs the
    /// CLI's `--resume <session_id>` flow via [`Self::dispatch_resume`].
    fn activate_agent_session(&mut self, s: &crate::agent_sessions::AgentSession) {
        use crate::agent_sessions::AgentStatus::*;
        tracing::info!(
            target: "agents_view",
            key = %s.key,
            status = ?s.status,
            pane_session_id = ?s.pane_session_id,
            cli = ?s.cli_source,
            "activate_agent_session: Enter on row",
        );
        match s.status {
            Idle | Working | Attention | Error => {
                if let Some(pane) = &s.pane_session_id {
                    crate::shell::wt_channel::spawn_wtcli_focus_pane(pane);
                    #[cfg(test)]
                    {
                        self.last_dispatched_command = Some(DispatchedCommand {
                            kind: DispatchedCommandKind::FocusPane,
                            session_id: Some(pane.clone()),
                            argv: vec![
                                "focus-pane".to_string(),
                                "-t".to_string(),
                                pane.clone(),
                            ],
                        });
                    }
                } else {
                    tracing::warn!(
                        target: "agents_view",
                        key = %s.key,
                        "live row has no pane_session_id; Enter is a no-op",
                    );
                }
            }
            Ended | Historical => {
                self.dispatch_resume(s);
            }
        }
    }

    /// Open a new WT tab whose primary pane runs `<cli> <resume_flag>
    /// <session_key>` to rehydrate a Historical/Ended agent session from
    /// the CLI's on-disk session store. Silent no-op for CLIs without a
    /// resume flag (Codex today) or unknown CLI sources.
    ///
    /// Flow:
    ///   1. Apply `ResumeDispatched` synchronously so a rapid second Enter
    ///      on the same row no-ops while this resume is in flight.
    ///   2. Issue `wtcli --json new-tab -c "<cli> <flag> <key>" -d "<cwd>"`
    ///      on a background thread via
    ///      `spawn_wtcli_split_then_focus_with_callback` — the helper is
    ///      generic (parses `session_id` from JSON and focuses the new
    ///      pane), so it works equally well for new-tab and split-pane.
    ///      Routing through `new-tab` keeps the originating tab clean
    ///      and matches user expectation that resuming a historical
    ///      session is a "go open my session" action, not a "split my
    ///      workspace" action.
    ///   3. The callback posts `AgentSessionEvent(ResumePaneAssigned{...})`
    ///      through `agent_event_tx` so the registry can bind the new
    ///      tab's primary pane GUID to the row even for hook-less CLIs
    ///      (Gemini), allowing a later `PaneClosed` to transition the
    ///      row back to Ended.
    fn dispatch_resume(&mut self, s: &crate::agent_sessions::AgentSession) {
        let cli_id = match s.cli_source {
            crate::agent_sessions::CliSource::Claude  => "claude",
            crate::agent_sessions::CliSource::Copilot => "copilot",
            crate::agent_sessions::CliSource::Gemini  => "gemini",
            crate::agent_sessions::CliSource::Unknown(_) => {
                tracing::debug!(
                    target: "agents_view",
                    key = %s.key,
                    "dispatch_resume: unknown cli_source, skipping",
                );
                return;
            }
        };
        let profile = crate::agent_registry::lookup_profile_by_id(cli_id);
        if profile.resume_flag.is_empty() {
            tracing::debug!(
                target: "agents_view",
                key = %s.key,
                cli = %cli_id,
                "dispatch_resume: CLI does not advertise a resume flag, skipping",
            );
            return;
        }

        let key = s.key.clone();
        let commandline = format!("{} {} {}", cli_id, profile.resume_flag, key);

        // Per-CLI session stores are keyed by an encoding of the *current*
        // working directory (e.g. Claude looks under
        // `~/.claude/projects/<encoded-cwd>/<id>.jsonl`; Copilot and
        // Gemini behave similarly). Without the right cwd the CLI
        // reports `No conversation found with session ID: <id>` even
        // though the JSONL exists on disk.
        //
        // `wtcli new-tab` exposes `-d <cwd>` (see
        // `src/tools/wtcli/main.cpp:326-353` → COM `CreateTab(...,
        // startingDirectory, ...)`) so the new tab's primary pane
        // launches in the historical session's project root directly,
        // without needing a `cd /d` shell prefix.
        //
        // We still wrap the CLI invocation in `cmd /c` because
        // npm-installed CLIs (`copilot.cmd`, `claude.cmd`, `gemini.cmd`)
        // need cmd.exe's PATHEXT resolution to launch from a bare name
        // (`CreateProcess` returns 0x80070002 for `.cmd` shims).
        let cwd_string = s.cwd.to_string_lossy().to_string();
        let launch_commandline = format!("cmd /c {}", commandline);
        let mut argv = vec![
            "new-tab".to_string(),
            "-c".to_string(),
            launch_commandline.clone(),
        ];
        if !cwd_string.is_empty() {
            argv.push("-d".to_string());
            argv.push(cwd_string.clone());
        }

        // Optimistic state flip: bump Historical/Ended -> Idle so a rapid
        // second Enter on the same row sees a non-terminal status and
        // skips this branch (idempotent: ResumeDispatched no-ops on live
        // rows). See `agent_sessions::SessionEvent::ResumeDispatched`.
        self.agent_sessions
            .apply(crate::agent_sessions::SessionEvent::ResumeDispatched { key: key.clone() });

        // Bind the freshly-spawned pane GUID back to the row. Required
        // for hook-less CLIs (Gemini) so a future `PaneClosed` can
        // transition the row to Ended; harmless duplicate work for
        // Claude/Copilot whose hooks beat us to the same binding.
        // `wtcli new-tab --json` emits a `session_id` field on the new
        // tab's primary pane in the same shape as `split-pane --json`,
        // so the existing helper handles both.
        let cb_key = key.clone();
        let event_tx = self.agent_event_tx.clone();
        let on_pane_id: Option<Box<dyn FnOnce(String) + Send + 'static>> = match event_tx {
            Some(tx) => Some(Box::new(move |pane_session_id| {
                let _ = tx.send(AppEvent::AgentSessionEvent(
                    crate::agent_sessions::SessionEvent::ResumePaneAssigned {
                        key: cb_key,
                        pane_session_id,
                    },
                ));
            })),
            None => None,
        };
        crate::shell::wt_channel::spawn_wtcli_split_then_focus_with_callback(&argv, on_pane_id);

        tracing::info!(
            target: "agents_view",
            key = %key,
            cli = %cli_id,
            commandline = %commandline,
            launch_commandline = %launch_commandline,
            cwd = %cwd_string,
            "dispatch_resume: new-tab scheduled",
        );

        #[cfg(test)]
        {
            self.last_dispatched_command = Some(DispatchedCommand {
                kind: DispatchedCommandKind::NewTabResume,
                session_id: None,
                argv,
            });
        }
    }

    /// Shift+Enter handler for terminal-state rows (Ended/Historical) in
    /// the session management view. Rather than splitting a normal pane
    /// (which `dispatch_resume` does for plain Enter), this resumes the
    /// session **inside the agent pane of a new WT tab** via ACP
    /// `session/load`.
    ///
    /// Flow:
    ///   1. Short-circuit with a system message in the current view when
    ///      the connected agent didn't advertise the `loadSession`
    ///      capability — opening a new tab would just dead-end on a
    ///      `JSON-RPC method not found` from the agent.
    ///   2. Optimistically apply `ResumeDispatched` to bump
    ///      Historical/Ended -> Idle so a rapid second Shift+Enter on the
    ///      same row no-ops (shared with `dispatch_resume`).
    ///   3. Emit a `resume_in_new_agent_tab` event to WT carrying the
    ///      session key + cwd. WT is responsible for:
    ///        - Creating a new tab (default profile, optionally honoring
    ///          cwd as the starting directory).
    ///        - Reconciling the shared agent pane onto the new tab.
    ///        - Publishing a `load_session` event BACK to WTA with the
    ///          new tab's StableId + the same session key + cwd.
    ///   4. The inbound `load_session` event handler in
    ///      `handle_wt_protocol_event` then forwards a `LoadSessionForTab`
    ///      request to the ACP client, which calls `conn.load_session`.
    ///
    /// Silent no-op for CLIs whose `cli_source` doesn't have a recognized
    /// id (unknown adapters); the inflight check is best-effort because
    /// only the agent-side knows whether the session id is recognizable.
    fn dispatch_resume_in_agent_pane(&mut self, s: &crate::agent_sessions::AgentSession) {
        tracing::info!(
            target: "agents_view",
            key = %s.key,
            status = ?s.status,
            cli = ?s.cli_source,
            supports_load = self.agent_supports_load_session,
            "dispatch_resume_in_agent_pane: Shift+Enter on row",
        );

        // Capability gate. ACP's `session/load` is opt-in (initialize
        // advertises `agentCapabilities.loadSession: bool`). Without it
        // the agent will reject the call — and we'd burn a new WT tab
        // to land on an error message. Short-circuit here instead and
        // keep the session management view focused so the user can
        // press plain Enter to fall back to the split-pane resume path.
        if !self.agent_supports_load_session {
            let agent = if self.agent_name.is_empty() {
                "the connected agent"
            } else {
                self.agent_name.as_str()
            };
            let msg = format!(
                "Cannot resume in agent pane: {} did not advertise the ACP \
                 `loadSession` capability. Press Enter (without Shift) to \
                 resume in a new terminal pane instead.",
                agent
            );
            tracing::warn!(
                target: "agents_view",
                key = %s.key,
                agent = %self.agent_name,
                "dispatch_resume_in_agent_pane: agent does not support loadSession",
            );
            let tab = self.current_tab_mut();
            tab.messages.push(ChatMessage::System(msg));
            tab.scroll_to_bottom();
            #[cfg(test)]
            {
                self.last_dispatched_command = Some(DispatchedCommand {
                    kind: DispatchedCommandKind::ResumeInAgentPane,
                    session_id: Some(s.key.clone()),
                    argv: vec!["resume_in_new_agent_tab".to_string(), "--unsupported".to_string()],
                });
            }
            return;
        }

        let key = s.key.clone();
        let cwd_string = s.cwd.to_string_lossy().to_string();

        // Mirror dispatch_resume's optimistic state flip so a rapid
        // double press doesn't double-dispatch.
        self.agent_sessions
            .apply(crate::agent_sessions::SessionEvent::ResumeDispatched { key: key.clone() });

        let evt = serde_json::json!({
            "type": "event",
            "method": "resume_in_new_agent_tab",
            "params": {
                "session_id": key,
                "cwd": cwd_string,
            }
        });
        send_wt_protocol_event(evt.to_string());

        tracing::info!(
            target: "agents_view",
            key = %s.key,
            cwd = %cwd_string,
            "dispatch_resume_in_agent_pane: resume_in_new_agent_tab event published",
        );

        #[cfg(test)]
        {
            self.last_dispatched_command = Some(DispatchedCommand {
                kind: DispatchedCommandKind::ResumeInAgentPane,
                session_id: Some(s.key.clone()),
                argv: vec![
                    "resume_in_new_agent_tab".to_string(),
                    "--session-id".to_string(),
                    s.key.clone(),
                    "--cwd".to_string(),
                    cwd_string,
                ],
            });
        }
    }

    /// Test-only accessor for the most recent F2 Agents-view dispatch.
    #[cfg(test)]
    pub fn last_dispatched_command_for_test(&self) -> Option<DispatchedCommand> {
        self.last_dispatched_command.clone()
    }

    /// Build the resolved ACP command string for an agent (e.g. "C:\...\claude.exe --acp").
    fn build_agent_cmd(&self, agent_id: &str) -> String {
        let profile = crate::agent_registry::lookup_profile_by_id(agent_id);
        let cmd = if !profile.acp_launch_command.is_empty() {
            profile.acp_launch_command.to_string()
        } else {
            let exe = crate::agent_check::find_exe(agent_id)
                .unwrap_or_else(|| agent_id.to_string());
            let mut cmd = exe;
            for flag in profile.acp_flags {
                cmd.push(' ');
                cmd.push_str(flag);
            }
            cmd
        };
        resolve_agent_cmd(&cmd)
    }

    /// Update the deferred ACP params to use the selected agent's command.
    fn update_deferred_acp_agent(&mut self, agent_id: &str) {
        if agent_id.is_empty() {
            return;
        }
        let profile = crate::agent_registry::lookup_profile_by_id(agent_id);
        let new_cmd = if !profile.acp_launch_command.is_empty() {
            profile.acp_launch_command.to_string()
        } else {
            let exe = crate::agent_check::find_exe(agent_id)
                .unwrap_or_else(|| agent_id.to_string());
            let mut cmd = exe;
            for flag in profile.acp_flags {
                cmd.push(' ');
                cmd.push_str(flag);
            }
            cmd
        };
        // Resolve to full path
        let resolved = resolve_agent_cmd(&new_cmd);
        if let Some(ref mut params) = self.deferred_acp {
            tracing::info!("Updating ACP agent command: {} -> {}", params.agent_cmd, resolved);
            params.agent_cmd = resolved;
        }
        // Remember the selected agent so we can notify C++ after connection succeeds.
        // We don't notify now because mid-FRE WriteSettingsToDisk triggers
        // _RebuildAgentStack which tears down the in-progress agent pane.
        self.pending_agent_selection = Some(agent_id.to_string());
    }

    pub fn set_event_tx(&mut self, tx: mpsc::UnboundedSender<AppEvent>) {
        self.event_tx = Some(tx);
    }

    /// First-call: spawn a blocking task to scan `~/.copilot`, `~/.claude`,
    /// `~/.gemini` for historical agent sessions and merge the result into
    /// `agent_sessions` via `AppEvent::HistoricalSessionsLoaded`. Subsequent
    /// calls are no-ops — the registry is cached for this wta's lifetime.
    ///
    /// Called eagerly from `run_acp_app` right after `set_event_tx` so the
    /// scan starts overlapping with ACP startup and is usually done by the
    /// time the user first opens the Agents view. Also called defensively
    /// from the F2 / `/sessions` toggle in case startup raced ahead of
    /// `set_event_tx` (Setup/FRE mode — `event_tx` not yet wired, so the
    /// eager call early-returns and the F2 press picks it up).
    ///
    /// Pre-eager-load this was strictly lazy because each wta restart
    /// (model switch, new agent pane) re-pays the ~10s scan. The eager
    /// kick is gated to the ACP TUI mode for the same reason — short-lived
    /// modes (`delegate`, `mcp`, CLI helpers) never call this.
    pub fn ensure_history_loaded(&mut self) {
        if self.history_load_state != HistoryLoadState::NotStarted {
            return;
        }
        let Some(tx) = self.event_tx.clone() else {
            // No event channel yet — Setup mode at startup. The first F2
            // press post-FRE will retry. Safe to leave state as NotStarted.
            return;
        };
        self.history_load_state = HistoryLoadState::Loading;
        tokio::task::spawn_blocking(move || {
            let scan_started = std::time::Instant::now();
            let sessions = crate::history_loader::load_all();
            tracing::info!(
                target: "history_loader",
                count = sessions.len(),
                elapsed_ms = scan_started.elapsed().as_millis() as u64,
                "background history scan complete (lazy)"
            );
            let _ = tx.send(AppEvent::HistoricalSessionsLoaded(sessions));
        });
    }

    fn spawn_login(&self, agent_id: &str, login_command: &str) {
        if let Some(ref tx) = self.event_tx {
            let tx = tx.clone();
            let cmd = login_command.to_string();
            let id = agent_id.to_string();
            tokio::task::spawn_local(async move {
                let progress_tx = tx.clone();
                let result = tokio::task::spawn_blocking(move || {
                    use std::io::BufRead;

                    // Parse command into exe + args (e.g. "C:\path\copilot.exe login")
                    // Handle quoted paths: "C:\path with spaces\copilot.exe" login
                    let (exe, args) = if cmd.starts_with('"') {
                        // Quoted path: find closing quote
                        if let Some(end) = cmd[1..].find('"') {
                            let exe = &cmd[1..end + 1];
                            let rest = cmd[end + 2..].trim();
                            (exe.to_string(), rest.split_whitespace().map(String::from).collect::<Vec<_>>())
                        } else {
                            (cmd.clone(), vec![])
                        }
                    } else {
                        let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
                        (parts[0].to_string(), parts.get(1).map(|s| s.split_whitespace().map(String::from).collect()).unwrap_or_default())
                    };

                    let mut child = match std::process::Command::new(&exe)
                        .args(&args)
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::piped())
                        .stdin(std::process::Stdio::null())
                        .spawn()
                    {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::warn!("spawn_login: failed to spawn '{}': {}", exe, e);
                            return false;
                        }
                    };

                    // Read both stdout and stderr — copilot login may
                    // write to either depending on buffering/version.
                    let stdout = child.stdout.take();
                    let stderr = child.stderr.take();

                    let progress_tx2 = progress_tx.clone();
                    let stderr_handle = std::thread::spawn(move || {
                        let mut found_success = false;
                        if let Some(stderr) = stderr {
                            let reader = std::io::BufReader::new(stderr);
                            for line in reader.lines().map_while(Result::ok) {
                                tracing::debug!("login stderr: {}", line);
                                if line.contains("enter code") {
                                    if let Some(code) = line.split("enter code ").nth(1) {
                                        let code = code.trim_end_matches('.');
                                        let _ = progress_tx2.send(AppEvent::LoginProgress {
                                            device_code: code.to_string(),
                                            verify_url: "https://github.com/login/device".to_string(),
                                        });
                                    }
                                }
                                if line.contains("Signed in successfully")
                                    || line.contains("already logged in")
                                {
                                    found_success = true;
                                    break;
                                }
                            }
                        }
                        found_success
                    });

                    let mut found_success = false;
                    if let Some(stdout) = stdout {
                        let reader = std::io::BufReader::new(stdout);
                        for line in reader.lines().map_while(Result::ok) {
                            tracing::debug!("login stdout: {}", line);
                            if line.contains("enter code") {
                                if let Some(code) = line.split("enter code ").nth(1) {
                                    let code = code.trim_end_matches('.');
                                    let _ = progress_tx.send(AppEvent::LoginProgress {
                                        device_code: code.to_string(),
                                        verify_url: "https://github.com/login/device".to_string(),
                                    });
                                }
                            }
                            if line.contains("Signed in successfully")
                                || line.contains("already logged in")
                            {
                                found_success = true;
                                break;
                            }
                        }
                    }

                    let stderr_success = stderr_handle.join().unwrap_or(false);
                    found_success = found_success || stderr_success;

                    if !found_success {
                        // Wait for process and check exit code
                        found_success = child.wait().map(|s| s.success()).unwrap_or(false);
                    } else {
                        let _ = child.wait();
                    }
                    found_success
                })
                .await;

                let success = result.unwrap_or(false);
                let _ = tx.send(AppEvent::LoginComplete { agent_id: id, success });
            });
        }
    }

    /// Unified setup-mode key handler. Covers both FRE agent selection and
    /// preflight diagnostic flows via the `SetupOption` variants.
    fn handle_setup_key(&mut self, key: KeyEvent) {
        // Block all input during install (except Ctrl+C / Esc to quit)
        let is_installing = self.setup.as_ref().map_or(false, |s| s.install_in_progress);
        tracing::debug!(target: "setup_key", code = ?key.code, is_installing, selected = ?self.setup.as_ref().map(|s| s.selected_index), options_count = ?self.setup.as_ref().map(|s| s.options.len()), "handle_setup_key");

        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Esc => {
                self.should_quit = true;
            }
            _ if is_installing => {
                return; // block all other keys during install
            }
            KeyCode::Up => {
                if let Some(ref mut setup) = self.setup {
                    if setup.selected_index > 0 {
                        setup.selected_index -= 1;
                    }
                }
            }
            KeyCode::Down => {
                if let Some(ref mut setup) = self.setup {
                    let max = setup.options.len().saturating_sub(1);
                    if setup.selected_index < max {
                        setup.selected_index += 1;
                    }
                }
            }
            KeyCode::Enter => {
                // Clone the selected option so we can act on it without borrowing setup
                let selected_opt = self
                    .setup
                    .as_ref()
                    .and_then(|s| s.options.get(s.selected_index).cloned());
                if let Some(opt) = selected_opt {
                    self.handle_setup_enter(opt);
                }
            }
            KeyCode::Char('o') | KeyCode::Char('O') => {
                // Open install URL if the selected option is an install-related one
                if let Some(ref setup) = self.setup {
                    if let Some(opt) = setup.options.get(setup.selected_index) {
                        match opt {
                            SetupOption::Reinstall { .. } => {
                                let url = setup.preflight.install_url.clone();
                                if !url.is_empty() {
                                    let _ = open_url_in_browser(&url);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Dispatch Enter on the selected `SetupOption`.
    fn handle_setup_enter(&mut self, opt: SetupOption) {
        tracing::info!(target: "setup_key", option = ?std::mem::discriminant(&opt), "handle_setup_enter");
        match opt {
            SetupOption::SelectAgent { agent } | SetupOption::SwitchAgent { agent } => {
                let agent_id = agent.id.clone();
                let agent_name = agent.display_name.clone();
                let profile = crate::agent_registry::lookup_profile_by_id(&agent_id);
                self.current_agent_id = agent_id.clone();
                tracing::info!(target: "setup_key", agent_id = %agent_id, cli_found = agent.cli_found, has_deferred = self.deferred_acp.is_some(), "SelectAgent/SwitchAgent entered");

                if agent.cli_found {
                    let has_cred = crate::agent_check::has_credential(&agent_id);
                    tracing::info!(target: "setup_key", agent_id = %agent_id, has_cred = has_cred, "credential check");
                    if has_cred {
                        // Credential found → connect directly
                        self.update_deferred_acp_agent(&agent_id);
                        self.mode = AppMode::Chat;
                        self.state = ConnectionState::Connecting(t!("connection.starting").into_owned());
                        // FRE mode uses deferred_acp, preflight mode uses restart_tx
                        if self.deferred_acp.is_some() {
                            self.pending_acp_start = true;
                        } else {
                            let new_cmd = self.build_agent_cmd(&agent_id);
                            let _ = self.restart_tx.send(RestartRequest { agent_cmd: Some(new_cmd) });
                        }
                        self.setup = None;
                        self.auth = Some(AuthState {
                            agent_id: agent_id.clone(),
                            agent_name,
                            auth_hint: profile.auth_hint.to_string(),
                            login_command: crate::agent_check::build_login_cmd(&agent_id),
                            checking: false,
                            status_message: String::new(),
                        });
                    } else {
                        // No credential → auth screen
                        self.update_deferred_acp_agent(&agent_id);
                        self.mode = AppMode::Auth;
                        self.setup = None;
                        self.auth = Some(AuthState {
                            agent_id: agent_id.clone(),
                            agent_name,
                            auth_hint: profile.auth_hint.to_string(),
                            login_command: crate::agent_check::build_login_cmd(&agent_id),
                            checking: false,
                            status_message: String::new(),
                        });
                    }
                } else if agent.can_auto_install() {
                    // Copilot not found → auto-install via winget
                    if let Some(ref mut setup) = self.setup {
                        setup.install_in_progress = true;
                        setup.install_log.clear();
                        setup.install_error = None;
                        setup.preflight.agent_id = agent_id.clone();
                        setup.preflight.display_name = agent_name.clone();
                    }
                    if let Some(ref tx) = self.event_tx {
                        let tx = tx.clone();
                        let id = agent_id.clone();
                        tokio::task::spawn_local(async move {
                            let on_line = |line: String| {
                                tracing::info!(target: "install", "{}", line);
                            };
                            let _ = crate::agent_check::install(&id, on_line).await;
                            let _ = tx.send(AppEvent::AgentInstallComplete);
                        });
                    }
                } else {
                    // CLI not found → rebuild setup as AgentMissing for this agent,
                    // showing install/fix options instead of jumping to auth.
                    let all_agents = crate::agent_check::check_all_agents();
                    let agent_status = crate::agent_check::check_agent(&agent_id);
                    let reason = SetupReason::AgentMissing;
                    let options = build_setup_options(&reason, Some(&agent_status), &all_agents);
                    self.mode = AppMode::Setup;
                    self.setup = Some(SetupState {
                        reason,

                        selected_index: 0,
                        preflight: PreflightResult {
                            agent_id: agent_id.clone(),
                            display_name: agent_name.clone(),
                            cli_status: CheckStatus::Failed("Not found".to_string()),
                            cli_path: None,
                            auth_status: CheckStatus::Skipped,
                            install_hint: profile.install_hint.to_string(),
                            install_url: String::new(),
                            auth_hint: profile.auth_hint.to_string(),
                        },
                        install_in_progress: false,
                        install_log: Vec::new(),
                        install_error: None,
                        options,
                        title: t!("setup.title.not_available").into_owned(),
                        subtitle: t!("setup.subtitle.agent_missing", agent = &agent_name).into_owned(),
                    });
                }
            }
            SetupOption::Reinstall { agent_id, .. } => {
                if let Some(ref setup) = self.setup {
                    if setup.install_in_progress {
                        return;
                    }
                }
                if let Some(ref mut setup) = self.setup {
                    setup.install_in_progress = true;
                    setup.install_error = None;
                    setup.install_log.clear();
                    setup.install_log.push(format!("{} {}", t!("setup.status.installing"), agent_id));
                }
                // Spawn async winget install via agent_check
                if let Some(ref tx) = self.event_tx {
                    let tx = tx.clone();
                    let id = agent_id.clone();
                    tokio::task::spawn_local(async move {
                        let result = crate::agent_check::install(&id, |_line| {
                            // Could send log lines as events, but keep simple for now
                        }).await;
                        match result {
                            Ok(()) => {
                                tracing::info!("Reinstall {} succeeded", id);
                            }
                            Err(e) => {
                                tracing::warn!("Reinstall {} failed: {}", id, e);
                            }
                        }
                        let _ = tx.send(AppEvent::AgentInstallComplete);
                    });
                }
            }
            SetupOption::SignIn { agent_id, display_name } => {
                let profile = crate::agent_registry::lookup_profile_by_id(&agent_id);
                self.mode = AppMode::Auth;
                self.auth = Some(AuthState {
                    agent_id: agent_id.clone(),
                    agent_name: display_name,
                    auth_hint: profile.auth_hint.to_string(),
                    login_command: crate::agent_check::build_login_cmd(&agent_id),
                    checking: false,
                    status_message: String::new(),
                });
            }
            SetupOption::Retry => {
                // Re-run preflight detection and try to reconnect
                if let Some(ref setup) = self.setup {
                    let agent_id = setup.preflight.agent_id.clone();
                    if !agent_id.is_empty() {
                        let status = crate::agent_check::check_agent(&agent_id);
                        if status.cli_found {
                            // CLI found — try to connect (auth will be checked by ACP).
                            // Stay in Setup mode with "Connecting..." to avoid a flash
                            // of red error text in Chat if ACP fails immediately.
                            self.update_deferred_acp_agent(&agent_id);
                            self.state =
                                ConnectionState::Connecting(t!("connection.reconnecting").into_owned());
                            self.preflight_setup_active = false;
                            if self.deferred_acp.is_some() {
                                self.pending_acp_start = true;
                            } else {
                                let new_cmd = self.build_agent_cmd(&agent_id);
                                let _ = self.restart_tx.send(RestartRequest { agent_cmd: Some(new_cmd) });
                            }
                            // Don't clear setup yet — AgentConnected will transition to Chat,
                            // AgentError will update the Setup screen.
                        }
                    }
                }
            }
        }
    }

    /// Key used for lookup into `tab_sessions`. Falls back to
    /// `DEFAULT_TAB_ID` until `tab_changed` from Windows Terminal arrives.
    fn active_tab_key(&self) -> &str {
        self.tab_id.as_deref().unwrap_or(DEFAULT_TAB_ID)
    }

    /// Read-only view of the currently focused tab's per-tab state. Always
    /// non-panicking: `App::new` seeds `DEFAULT_TAB_ID` and
    /// `tab_changed` lazily creates the entry for any new tab via
    /// `current_tab_mut`/`tab_mut`.
    pub fn current_tab(&self) -> &TabSession {
        let key = self.active_tab_key();
        self.tab_sessions
            .get(key)
            .expect("active tab session always materialized")
    }

    /// Mutable view of the currently focused tab's per-tab state.
    /// Lazily inserts a default `TabSession` if the key is missing.
    pub fn current_tab_mut(&mut self) -> &mut TabSession {
        let key = self.tab_id.clone().unwrap_or_else(|| DEFAULT_TAB_ID.to_string());
        self.tab_sessions.entry(key).or_default()
    }

    /// Mutable view of an arbitrary tab's per-tab state, lazily inserting
    /// a default `TabSession` if missing. Used by `tab_changed` and (in
    /// Milestone 2) by chunk routing keyed on `SessionId`.
    #[allow(dead_code)]
    pub fn tab_mut(&mut self, tab_id: &str) -> &mut TabSession {
        self.tab_sessions
            .entry(tab_id.to_string())
            .or_default()
    }

    /// Resolve a `SessionId` to the tab that owns it. Returns the active
    /// tab as a fallback when the session is unknown -- covers events
    /// emitted before a session was attached (rare) or pre-session
    /// startup events.
    fn tab_for_session(&self, session_id: &str) -> String {
        self.session_to_tab
            .get(session_id)
            .cloned()
            .or_else(|| self.tab_id.clone())
            .unwrap_or_else(|| DEFAULT_TAB_ID.to_string())
    }

    /// Mutable view of the tab that owns the given session id. Lazily
    /// creates the `TabSession` if missing.
    pub fn session_tab_mut(&mut self, session_id: &str) -> &mut TabSession {
        let key = self.tab_for_session(session_id);
        self.tab_sessions.entry(key).or_default()
    }

    /// Read-only view of the tab that owns the given session id.
    pub fn session_tab(&self, session_id: &str) -> &TabSession {
        let key = self.tab_for_session(session_id);
        self.tab_sessions
            .get(&key)
            .or_else(|| self.tab_sessions.get(DEFAULT_TAB_ID))
            .expect("active tab session always materialized")
    }

    pub async fn run(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        mut ui_rx: mpsc::UnboundedReceiver<AppEvent>,
        mut event_rx: mpsc::UnboundedReceiver<AppEvent>,
    ) -> Result<()> {
        const MAX_EVENTS_PER_FRAME: usize = 64;

        let initial_draw_started = std::time::Instant::now();
        self.draw_frame(terminal)?;
        ui_trace::log_slow("initial_draw", initial_draw_started.elapsed(), || {
            self.trace_state()
        });

        loop {
            tokio::select! {
                biased;

                Some(event) = ui_rx.recv() => {
                    let event_name = Self::event_name(&event);
                    self.apply_resize_if_needed(terminal, &event)?;
                    let should_redraw = self.event_requires_redraw(&event);
                    let handle_started = std::time::Instant::now();
                    self.handle_event(event);
                    ui_trace::log_slow("ui_event_handle", handle_started.elapsed(), || {
                        format!("event={} {}", event_name, self.trace_state())
                    });
                    if should_redraw {
                        let draw_started = std::time::Instant::now();
                        self.draw_frame(terminal)?;
                        ui_trace::log_slow("ui_event_draw", draw_started.elapsed(), || {
                            format!("event={} {}", event_name, self.trace_state())
                        });
                    }
                }

                Some(event) = event_rx.recv() => {
                    let first_event_name = Self::event_name(&event);
                    self.apply_resize_if_needed(terminal, &event)?;
                    let batch_started = std::time::Instant::now();
                    let mut processed = 0usize;

                    let mut should_redraw_now = self.event_requires_redraw(&event);
                    self.handle_event(event);
                    processed += 1;

                    while processed < MAX_EVENTS_PER_FRAME {
                        match event_rx.try_recv() {
                            Ok(event) => {
                                self.apply_resize_if_needed(terminal, &event)?;
                                if self.event_requires_redraw(&event) {
                                    should_redraw_now = true;
                                }
                                self.handle_event(event);
                                processed += 1;
                            }
                            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
                        }
                    }

                    ui_trace::log_slow("event_batch_handle", batch_started.elapsed(), || {
                        format!(
                            "first_event={} processed={} redraw={} {}",
                            first_event_name,
                            processed,
                            should_redraw_now,
                            self.trace_state()
                        )
                    });

                    if should_redraw_now {
                        let draw_started = std::time::Instant::now();
                        self.draw_frame(terminal)?;
                        ui_trace::log_slow("event_batch_draw", draw_started.elapsed(), || {
                            format!(
                                "first_event={} processed={} {}",
                                first_event_name,
                                processed,
                                self.trace_state()
                            )
                        });
                    }
                }

                else => {
                    break; // All senders dropped
                }
            }

            // Deferred ACP start after login completes
            self.try_start_acp();

            if self.should_quit {
                break;
            }
        }
        Ok(())
    }

    fn apply_resize_if_needed(
        &self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        event: &AppEvent,
    ) -> Result<()> {
        let AppEvent::Resize(width, height) = event else {
            return Ok(());
        };

        let resize_started = std::time::Instant::now();
        terminal.resize(Rect::new(0, 0, *width, *height))?;
        ui_trace::log_slow("terminal_resize", resize_started.elapsed(), || {
            format!("width={} height={}", width, height)
        });
        Ok(())
    }

    fn draw_frame(&mut self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
        let total_started = std::time::Instant::now();

        let mut frame = terminal.get_frame();
        let area = frame.area();

        let render_started = std::time::Instant::now();
        ui::render(&mut frame, self);
        ui_trace::log_slow("ui_render", render_started.elapsed(), || self.trace_state());

        // Wrap the whole frame in a synchronized-update boundary (CSI ? 2026
        // h/l, supported by Windows Terminal). Without it, every cursor
        // call in the ratatui-crossterm backend (`hide_cursor`,
        // `show_cursor`, `set_cursor_position` — all `execute!`-based, see
        // ratatui-crossterm lib.rs:288/292/303) flushes stdout on its own,
        // so WT can render partial states between them — most visibly the
        // brief cursor-hidden window during the shimmer redraw, which the
        // eye reads as the inputbox cursor blinking at ~8Hz. Inside a sync
        // block WT freezes rendering until End and paints the final state
        // in a single frame.
        queue!(terminal.backend_mut(), BeginSynchronizedUpdate)?;

        let flush_started = std::time::Instant::now();
        terminal.flush()?;
        ui_trace::log_slow("terminal_flush", flush_started.elapsed(), || {
            self.trace_state()
        });

        let cursor_started = std::time::Instant::now();
        if let Some(position) = ui::input_cursor_position(self, area) {
            // Order matters: position first, then show. Showing first would
            // briefly reveal the cursor wherever the flush left it (typically
            // the last redrawn cell on the chat side) before the move lands.
            // (Inside the sync block this is academic — WT won't render
            // either intermediate — but the ordering also documents intent.)
            terminal.set_cursor_position(position)?;
            terminal.show_cursor()?;
        } else {
            terminal.hide_cursor()?;
        }
        ui_trace::log_slow("terminal_cursor", cursor_started.elapsed(), || {
            self.trace_state()
        });

        queue!(terminal.backend_mut(), EndSynchronizedUpdate)?;

        terminal.swap_buffers();

        let backend_flush_started = std::time::Instant::now();
        terminal.backend_mut().flush()?;
        ui_trace::log_slow(
            "terminal_backend_flush",
            backend_flush_started.elapsed(),
            || self.trace_state(),
        );

        self.log_selection_visible_if_needed();

        ui_trace::log_slow("draw_frame_total", total_started.elapsed(), || {
            self.trace_state()
        });

        Ok(())
    }

    fn event_name(event: &AppEvent) -> &'static str {
        match event {
            AppEvent::Key(_) => "key",
            AppEvent::MouseScroll { .. } => "mouse_scroll",
            AppEvent::Tick => "tick",
            AppEvent::Resize(_, _) => "resize",
            AppEvent::ConnectionStage(_) => "connection_stage",
            AppEvent::ProgressStatus { .. } => "progress_status",
            AppEvent::AgentConnected { .. } => "agent_connected",
            AppEvent::SessionAttached { .. } => "session_attached",
            AppEvent::TabError { .. } => "tab_error",
            AppEvent::TabSystemMessage { .. } => "tab_system_message",
            AppEvent::PromptTemplateLoaded { .. } => "prompt_template_loaded",
            AppEvent::AgentError { .. } => "agent_error",
            AppEvent::AgentBusy { .. } => "agent_busy",
            AppEvent::ExecutionInfo(_) => "execution_info",
            AppEvent::AgentThoughtChunk { .. } => "agent_thought_chunk",
            AppEvent::AgentMessageChunk { .. } => "agent_message_chunk",
            AppEvent::UserMessageReplayChunk { .. } => "user_message_replay_chunk",
            AppEvent::AgentMessageEnd { .. } => "agent_message_end",
            AppEvent::TimingMetric { .. } => "timing_metric",
            AppEvent::ToolCall { .. } => "tool_call",
            AppEvent::ToolCallUpdate { .. } => "tool_call_update",
            AppEvent::Plan { .. } => "plan",
            AppEvent::PermissionRequest { .. } => "permission_request",
            AppEvent::SystemMessage(_) => "system_message",
            AppEvent::DebugPipeMessage(_) => "debug_pipe_message",
            AppEvent::WtEvent { .. } => "wt_event",
            AppEvent::AgentInstallComplete => "agent_install_complete",
            AppEvent::LoginProgress { .. } => "login_progress",
            AppEvent::LoginComplete { .. } => "login_complete",
            AppEvent::PreflightComplete(_) => "preflight_complete",
            AppEvent::AgentSessionEvent(_) => "agent_session_event",
            AppEvent::HistoricalSessionsLoaded(_) => "historical_sessions_loaded",
        }
    }

    fn trace_state(&self) -> String {
        let tab = self.current_tab();
        format!(
            "state={:?} turn={:?} messages={} completed_turns={} input_chars={} pending_chars={} scroll={} activity_frame={} recommendations={} permission={} timing_note={}",
            self.state,
            std::mem::discriminant(&tab.turn),
            tab.messages.len(),
            tab.completed_turns.len(),
            tab.input.chars().count(),
            tab.turn.buffer().map(|b| b.chars().count()).unwrap_or(0),
            tab.chat_scroll.offset,
            tab.activity_frame,
            tab.turn.recommendations().map(|r| r.choices.len()).unwrap_or(0),
            tab.permission.is_some(),
            tab.timing_note.is_some()
        )
    }

    fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Key(key) => self.handle_key(key),
            AppEvent::MouseScroll { delta, row } => {
                // Recs panel sits just above the input. When the cursor is
                // over it, the wheel drives `rec_scroll` (top-anchored:
                // positive delta = scroll down = increase offset). Otherwise
                // it drives `chat_scroll` (bottom-anchored: wheel up shows
                // higher content = increase offset, so we negate delta).
                let in_recs = self.current_tab().turn.recommendations().is_some() && {
                    // 3 input + 1 nav hint row above the input.
                    let recs_top = self
                        .terminal_rows
                        .saturating_sub(4 + self.rec_panel_height(self.main_area_width()));
                    row >= recs_top
                };
                let d = delta as isize;
                let tab = self.current_tab_mut();
                if in_recs {
                    tab.rec_scroll.by(d);
                } else {
                    tab.chat_scroll.by(-d);
                }
            }
            AppEvent::Tick => {
                // Fan out across all tabs: a background tab with an in-flight
                // prompt should keep its shimmer phase advancing so when the
                // user switches back the animation is in step.
                for tab in self.tab_sessions.values_mut() {
                    if tab.turn.spinner_label().is_some() || tab.progress_status.is_some() {
                        tab.activity_frame =
                            (tab.activity_frame + 1) % crate::ui::ACTIVITY_CYCLE_FRAMES;
                    }
                }
                // Setup-mode spinner: ticks while we're showing the wizard
                // (e.g. spinning during a `winget install` background job).
                // Also advance while the agents-view history scan is in
                // flight so the "Loading" shimmer keeps animating.
                if self.mode == AppMode::Setup
                    || self.mode == AppMode::Auth
                    || self.history_load_state == HistoryLoadState::Loading
                {
                    self.activity_frame = self.activity_frame.wrapping_add(1);
                }
                // Age and auto-dismiss notifications
                for n in self.wt_notifications.iter_mut() {
                    n.age_ticks = n.age_ticks.saturating_add(1);
                }
                self.wt_notifications.retain(|n| !n.should_auto_dismiss());
                if self.wt_notifications.is_empty()
                    || self.wt_notifications.iter().all(|n| n.acknowledged)
                {
                    self.show_notification_banner = false;
                }
            }
            AppEvent::Resize(w, h) => {
                self.terminal_cols = w;
                self.terminal_rows = h;
            }
            AppEvent::ConnectionStage(stage) => {
                self.state = ConnectionState::Connecting(stage);
                self.publish_agent_status();
            }
            AppEvent::ProgressStatus { session_id, status } => {
                let tab = match session_id {
                    Some(sid) => self.session_tab_mut(&sid),
                    None => self.current_tab_mut(),
                };
                tab.progress_status = Some(status);
                tab.scroll_to_bottom();
            }
            AppEvent::AgentConnected {
                name,
                model,
                version,
                session_id,
                available_models,
                current_model_id,
                load_session_supported,
            } => {
                self.agent_name = name;
                self.agent_model = model;
                self.agent_version = version;
                self.session_id = session_id.clone();
                self.available_models = available_models.clone();
                self.current_model_id = current_model_id.clone();
                self.agent_supports_load_session = load_session_supported;
                self.state = ConnectionState::Connected;
                self.preflight_setup_active = false;
                // If we were in Setup (e.g. after Retry), transition to Chat
                if self.mode == AppMode::Setup {
                    self.mode = AppMode::Chat;
                    self.setup = None;
                }
                // Show welcome hint on first-ever connect (persisted in state.json)
                if !welcome_shown_in_state() {
                    self.show_welcome_hint = true;
                }
                // Bind the startup session to whichever tab we own.
                let bind_tab = self
                    .tab_id
                    .clone()
                    .unwrap_or_else(|| DEFAULT_TAB_ID.to_string());
                self.session_to_tab
                    .insert(session_id.clone(), bind_tab.clone());
                let tab = self.tab_mut(&bind_tab);
                tab.session_id = Some(session_id);
                self.publish_agent_status();
            }
            AppEvent::SessionAttached {
                tab_id,
                session_id,
                available_models,
                current_model_id,
            } => {
                self.session_to_tab
                    .insert(session_id.clone(), tab_id.clone());
                let tab = self.tab_mut(&tab_id);
                tab.session_id = Some(session_id);
                // Close the session/load replay window if it was open.
                // The agent has finished returning the load_session
                // RPC; any straggling session/update notifications
                // arriving after this point would have been pushed
                // already (the spec requires they precede the
                // PromptResponse-equivalent). Flush any pending
                // user/agent buffers as a final turn boundary.
                if tab.loading_session {
                    tab.flush_load_replay_pending();
                    tab.loading_session = false;
                    tab.scroll_to_bottom();
                }
                // Per-session model lists could differ — surface the new
                // tab's models when the agent_status event publishes for
                // this session in the future. For now we keep
                // App.available_models pointing at the active session's
                // models so the existing settings UI stays correct.
                if !available_models.is_empty() {
                    self.available_models = available_models;
                }
                if current_model_id.is_some() {
                    self.current_model_id = current_model_id;
                }
                self.publish_agent_status();
            }
            AppEvent::TabError { tab_id, message } => {
                // Scoped error for a specific tab. Bypasses the global
                // auth-fallback / ConnectionState::Failed flip in
                // AgentError because the error is local to one tab's
                // session-load attempt, not the whole connection.
                let tab = self.tab_mut(&tab_id);
                tab.loading_session = false;
                tab.progress_status = None;
                tab.pending_agent_response.clear();
                tab.pending_user_replay.clear();
                tab.timing_note = None;
                tab.turn = TurnState::Idle;
                tab.messages.push(ChatMessage::Error(message));
                tab.scroll_to_bottom();
            }
            AppEvent::TabSystemMessage { tab_id, message } => {
                let tab = self.tab_mut(&tab_id);
                tab.messages.push(ChatMessage::System(message));
                tab.scroll_to_bottom();
            }
            AppEvent::PromptTemplateLoaded { name } => {
                self.prompt_name = Some(name);
            }
            AppEvent::AgentBusy { tab_id } => {
                let tab = self.tab_mut(&tab_id);
                tab.messages.push(ChatMessage::System(
                    "Agent is busy on this tab — wait for the current prompt to finish."
                        .to_string(),
                ));
                tab.scroll_to_bottom();
            }
            AppEvent::AgentError { session_id, message } => {
                // Optimistic-connect fallback: if we have stashed auth info
                // and the error is auth-related, show the auth screen instead
                // of a dead error state.
                let lower = message.to_lowercase();
                let is_auth_error = lower.contains("authentication required")
                    || lower.contains("not logged in")
                    || lower.contains("unauthorized")
                    || lower.contains("401")
                    || lower.contains("apikey is missing")
                    || lower.contains("api key");
                if is_auth_error && !self.preflight_setup_active {
                    tracing::info!("AgentError auth fallback: showing setup screen");
                    // Use current_agent_id — set at preflight or agent selection time.
                    let agent_id = if !self.current_agent_id.is_empty() {
                        self.current_agent_id.clone()
                    } else {
                        "copilot".to_string()
                    };
                    tracing::info!("AgentError: resolved agent_id={}", agent_id);
                    let profile = crate::agent_registry::lookup_profile(&agent_id);
                    let agent_status = crate::agent_check::check_agent(profile.id);
                    let all_agents = crate::agent_check::check_all_agents();
                    let reason = SetupReason::AgentError;
                    let options = build_setup_options(&reason, Some(&agent_status), &all_agents);
                    self.mode = AppMode::Setup;
                    self.state = ConnectionState::Disconnected;
                    self.auth = None;
                    self.setup = Some(SetupState {
                        reason,
                        selected_index: 0,
                        preflight: PreflightResult {
                            agent_id: profile.id.to_string(),
                            display_name: profile.display_name.to_string(),
                            cli_status: CheckStatus::Passed,
                            cli_path: None,
                            auth_status: CheckStatus::Failed(t!("system.authentication_failed").into_owned()),
                            install_hint: profile.install_hint.to_string(),
                            install_url: String::new(),
                            auth_hint: profile.auth_hint.to_string(),
                        },
                        install_in_progress: false,
                        install_log: Vec::new(),
                        install_error: None,
                        options,
                        title: t!("setup.title.sign_in").into_owned(),
                        subtitle: if profile.id == "copilot" {
                            t!("setup.subtitle.copilot_auth", agent = profile.display_name).into_owned()
                        } else {
                            t!("setup.subtitle.agent_auth", agent = profile.display_name).into_owned()
                        },
                    });
                    // Clear error messages
                    let tab = self.current_tab_mut();
                    tab.messages.retain(|m| !matches!(m, ChatMessage::Error(_)));
                } else {
                    self.state = ConnectionState::Failed(message.clone());
                    self.publish_agent_status();
                    let tab = match session_id.as_deref() {
                        Some(sid) => self.session_tab_mut(sid),
                        None => self.current_tab_mut(),
                    };
                    tab.progress_status = None;
                    tab.activity_frame = 0;
                    tab.timing_note = None;
                    tab.turn = TurnState::Idle;
                    tab.messages.push(ChatMessage::Error(message));
                }
            }
            AppEvent::ExecutionInfo(message) => {
                self.push_execution_info(message);
                self.current_tab_mut().scroll_to_bottom();
            }
            AppEvent::AgentThoughtChunk { session_id, text } => {
                // Late chunk after cancel / completion is dropped by
                // `turn_observe_chunk` (state isn't Submitted/Streaming).
                self.turn_observe_chunk(&session_id, ChunkKind::Thought, &text);
            }
            AppEvent::AgentMessageChunk { session_id, text } => {
                let tab = self.session_tab_mut(&session_id);
                // Late chunks after cancel / completion are dropped by
                // `turn_observe_chunk` (state isn't Submitted/Streaming).
                // During session/load replay no Submitted state exists,
                // so we still need to gate on `loading_session` here to
                // accept replayed chunks into `messages`.
                if !tab.turn.is_in_flight() && !tab.loading_session {
                    return;
                }
                // Turn boundary detection during replay: an agent
                // message chunk after a buffered user_message_chunk
                // means the previous user turn is complete — flush it
                // as a ChatMessage::User so the chat stays in turn
                // order.
                if tab.loading_session && !tab.pending_user_replay.is_empty() {
                    let text = std::mem::take(&mut tab.pending_user_replay);
                    tab.messages.push(ChatMessage::User(text));
                }
                tab.progress_status = None;
                tab.pending_agent_response.push_str(&text);

                // Append to the streaming buffer. The state machine drops
                // late chunks and handles the stale-autofix generation check
                // before returning whether the buffer actually grew.
                let advanced =
                    self.turn_observe_chunk(&session_id, ChunkKind::Message, &text);

                // Surface the card the moment the streamed JSON parses,
                // instead of waiting for AgentMessageEnd (gated behind
                // Copilot's Stop/SessionEnd hooks, ~8s on Windows).
                if advanced {
                    self.turn_try_eager_surface(&session_id);
                }
            }
            AppEvent::UserMessageReplayChunk { session_id, text } => {
                // Replayed historical user prompt from a `session/load`
                // SessionUpdate. Only meaningful during the load window;
                // dropped otherwise. A new user_message_chunk after a
                // buffered agent response marks the turn boundary —
                // flush the previous agent message first.
                let tab = self.session_tab_mut(&session_id);
                if !tab.loading_session {
                    return;
                }
                if !tab.pending_agent_response.is_empty() {
                    let prev = std::mem::take(&mut tab.pending_agent_response);
                    tab.messages.push(ChatMessage::Agent(prev));
                }
                tab.pending_user_replay.push_str(&text);
            }
            AppEvent::AgentMessageEnd { session_id } => {
                if let Some(summary) = self.session_completion_latency_summary(&session_id) {
                    self.push_execution_info(summary);
                }
                self.turn_close(&session_id);
                self.session_tab_mut(&session_id).scroll_to_bottom();
            }
            AppEvent::TimingMetric { session_id, note } => {
                self.session_tab_mut(&session_id).timing_note = Some(note);
            }
            AppEvent::ToolCall { session_id, id, title, status } => {
                let tab = self.session_tab_mut(&session_id);
                if !tab.turn.is_in_flight() && !tab.loading_session {
                    return;
                }
                // Turn boundary during replay (see AgentMessageChunk).
                if tab.loading_session {
                    if !tab.pending_user_replay.is_empty() {
                        let text = std::mem::take(&mut tab.pending_user_replay);
                        tab.messages.push(ChatMessage::User(text));
                    }
                    if !tab.pending_agent_response.is_empty() {
                        let text = std::mem::take(&mut tab.pending_agent_response);
                        tab.messages.push(ChatMessage::Agent(text));
                    }
                }
                tab.tool_calls
                    .insert(id.clone(), (title.clone(), status.clone()));
                tab.messages
                    .push(ChatMessage::ToolCall { id, title, status });
                tab.scroll_to_bottom();
            }
            AppEvent::ToolCallUpdate { session_id, id, status } => {
                let tab = self.session_tab_mut(&session_id);
                if !tab.turn.is_in_flight() && !tab.loading_session {
                    return;
                }
                if let Some(entry) = tab.tool_calls.get_mut(&id) {
                    entry.1 = status.clone();
                }
                // Update in-place in messages
                for msg in &mut tab.messages {
                    if let ChatMessage::ToolCall {
                        id: ref mid,
                        status: ref mut s,
                        ..
                    } = msg
                    {
                        if mid == &id {
                            *s = status.clone();
                        }
                    }
                }
            }
            AppEvent::Plan { session_id, entries } => {
                let tab = self.session_tab_mut(&session_id);
                if !tab.turn.is_in_flight() && !tab.loading_session {
                    return;
                }
                if tab.loading_session {
                    if !tab.pending_user_replay.is_empty() {
                        let text = std::mem::take(&mut tab.pending_user_replay);
                        tab.messages.push(ChatMessage::User(text));
                    }
                    if !tab.pending_agent_response.is_empty() {
                        let text = std::mem::take(&mut tab.pending_agent_response);
                        tab.messages.push(ChatMessage::Agent(text));
                    }
                }
                tab.messages.push(ChatMessage::Plan(entries));
                tab.scroll_to_bottom();
            }
            AppEvent::PermissionRequest {
                session_id,
                description,
                options,
                responder,
            } => {
                let tab = self.session_tab_mut(&session_id);
                if !tab.turn.is_in_flight() && !tab.loading_session {
                    // Auto-deny if the user cancelled before the agent
                    // got around to asking. Dropping the responder yields
                    // a Cancelled outcome on the agent side.
                    return;
                }
                tab.permission = Some(PermissionState {
                    description,
                    options,
                    selected: 0,
                    responder: Some(responder),
                });
            }
            AppEvent::SystemMessage(message) => {
                self.current_tab_mut().messages.push(ChatMessage::System(message));
                self.scroll_to_bottom();
            }
            AppEvent::DebugPipeMessage(msg) => {
                self.debug_messages.push(msg);
                // Cap at 500 messages
                if self.debug_messages.len() > 500 {
                    self.debug_messages.remove(0);
                }
            }
            AppEvent::PreflightComplete(result) => {
                tracing::info!(
                    target: "preflight",
                    agent = %result.agent_id,
                    cli_status = ?result.cli_status,
                    auth_status = ?result.auth_status,
                    "preflight result received"
                );
                if !result.all_passed() {
                    let reason = SetupReason::AgentMissing;
                    let current_status = crate::agent_check::check_agent(&result.agent_id);
                    let all_agents = crate::agent_check::check_all_agents();
                    let options = build_setup_options(&reason, Some(&current_status), &all_agents);
                    let title = reason.title().to_string();
                    let subtitle = if current_status.can_auto_install() {
                        t!("setup.subtitle.copilot_missing", agent = &result.display_name).into_owned()
                    } else {
                        t!("setup.subtitle.agent_missing", agent = &result.display_name).into_owned()
                    };
                    self.mode = AppMode::Setup;
                    self.preflight_setup_active = true;
                    self.setup = Some(SetupState {
                        reason,

                        preflight: result,
                        selected_index: 0,
                        install_in_progress: false,
                        install_log: Vec::new(),
                        install_error: None,
                        options,
                        title,
                        subtitle,
                    });
                }
            }
            AppEvent::AgentSessionEvent(ev) => {
                tracing::debug!(
                    target: "agent_session_registry",
                    event = ?ev,
                    "AgentSessionEvent posted from background callback"
                );
                self.agent_sessions.apply(ev);
            }
            AppEvent::HistoricalSessionsLoaded(sessions) => {
                tracing::info!(
                    target: "history_loader",
                    count = sessions.len(),
                    "historical sessions merged from background scan"
                );
                self.agent_sessions.merge_historical(sessions);
                self.history_load_state = HistoryLoadState::Loaded;

                // If the user is already on the Agents view (e.g. they were
                // dropped there by --initial-view sessions, or they pressed
                // F2 / Ctrl+Shift+/ before the scan finished) and nothing
                // is selected yet, seed selection on row 0 so Enter
                // activates immediately. Mirrors the F2 enter-Agents path.
                if self.current_tab().current_view == View::Agents
                    && self.current_tab().agents_list_state.selected().is_none()
                    && !self
                        .agent_sessions
                        .iter_sorted_filtered(self.current_cli_filter().as_ref())
                        .is_empty()
                {
                    self.current_tab_mut().agents_list_state.select(Some(0));
                }
            }
            AppEvent::WtEvent {
                method,
                pane_id,
                params,
            } => {
                tracing::debug!(target: "autofix", method = %method, pane_id = %pane_id, self_pane_id = ?self.pane_id, "WtEvent");

                // Hook bridge events: fire-and-forget into the agent registry
                // so the F2 Agents view stays current. Unrelated to autofix /
                // tab routing; runs before the same-pane skip because we want
                // to record events from our own pane too.
                if method == "agent_event" {
                    let _ = route_agent_event_to_registry(
                        &mut self.agent_sessions,
                        pane_id.as_str(),
                        &params,
                    );
                    // Diagnostics aid: surface the raw event payload in the
                    // active tab's chat so a developer can correlate hook
                    // wire-format with registry behavior. Off by default.
                    if self.log_agent_events {
                        let detail = serde_json::to_string(&params)
                            .unwrap_or_else(|_| "<unserializable>".to_string());
                        self.current_tab_mut()
                            .messages
                            .push(ChatMessage::AgentEvent(detail));
                    }
                    return;
                }

                // autofix_execute is an inbound UI action ("run the armed
                // fix now") from TerminalPage. pane_id is the failing
                // pane — NOT our own — so this check must run before the
                // same-pane skip below. Ignore the event if we don't
                // actually have a cached autofix for that pane.
                if method == "autofix_execute" {
                    self.handle_autofix_execute_request(&pane_id);
                    return;
                }

                if method == "tab_changed" {
                    tracing::info!(
                        target: "tab_session",
                        raw_params = %params,
                        current_tab = ?self.tab_id,
                        "tab_changed event received"
                    );
                    if let Some(new_tab_id) = params.get("tab_id").and_then(|v| v.as_str()) {
                        self.switch_tab_session(new_tab_id.to_string());
                    } else {
                        tracing::warn!(target: "tab_session", "tab_changed: missing tab_id in params");
                    }
                    return;
                }

                if method == "tab_closed" {
                    if let Some(closed_tab_id) =
                        params.get("tab_id").and_then(|v| v.as_str())
                    {
                        self.drop_tab_session(closed_tab_id);
                    } else {
                        tracing::warn!(target: "tab_session", "tab_closed: missing tab_id in params");
                    }
                    return;
                }

                if method == "reset_tab_session" {
                    if let Some(tab_id) = params.get("tab_id").and_then(|v| v.as_str()) {
                        self.reset_tab_session_for(tab_id);
                    } else {
                        tracing::warn!(target: "tab_session", "reset_tab_session: missing tab_id in params");
                    }
                    return;
                }

                // load_session: WT-side replay of WTA's
                // `resume_in_new_agent_tab` request. After WT creates a
                // new tab and reconciles the shared agent pane onto it,
                // it publishes this event with the new tab's StableId,
                // the historical session id, and the cwd. We forward to
                // the ACP client which calls `conn.load_session` and
                // binds the loaded session to the tab via
                // `SessionAttached`. Best-effort: if the agent doesn't
                // recognize the session id (e.g. CLI mismatch), the
                // client emits a `TabError` scoped to this tab. We also
                // pre-switch the target tab back to the Chat view, clear
                // its local chat, and post a "Resuming..." system note
                // so the user sees something even if the agent's
                // session/update replay is delayed or absent.
                if method == "load_session" {
                    let tab_id = params
                        .get("tab_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let session_id = params
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let cwd = params
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .filter(|s| !s.is_empty());
                    tracing::info!(
                        target: "acp_load_session",
                        tab_id,
                        session_id,
                        cwd = ?cwd,
                        "inbound load_session event from WT"
                    );
                    if tab_id.is_empty() || session_id.is_empty() {
                        tracing::warn!(
                            target: "acp_load_session",
                            "load_session: missing tab_id or session_id in params"
                        );
                        return;
                    }
                    {
                        let tab = self.tab_mut(tab_id);
                        tab.current_view = View::Chat;
                        tab.clear_chat_history();
                        tab.completed_turns.clear();
                        tab.selected_completed_turn_idx = None;
                        tab.session_id = None;
                        // Open the replay window: chunk handlers will
                        // now accept session/update events for this
                        // tab even though `turn` stays Idle. Closed by
                        // the SessionAttached handler when
                        // `conn.load_session` returns.
                        tab.loading_session = true;
                        tab.messages.push(ChatMessage::System(format!(
                            "Resuming session {}...",
                            session_id
                        )));
                        tab.scroll_to_bottom();
                    }
                    let _ = self.load_session_tx.send(LoadSessionForTab {
                        tab_id: tab_id.to_string(),
                        session_id: session_id.to_string(),
                        cwd,
                    });
                    return;
                }

                // set_view: WT broadcasts this from Ctrl+Shift+/ (or any
                // future "open agent pane in <view>" action) to switch the
                // active TabSession's TUI view. Absolute (not toggle).
                //
                // Window-scoped: WT includes its own window_id; we ignore
                // the event when our window_id is known and doesn't match,
                // so multi-window setups don't cross-talk. When window_id
                // is unknown on either side we apply (best-effort fallback).
                //
                // Processed BEFORE the own-pane skip below: this is a
                // global UI command, not a per-pane signal.
                if method == "set_view" {
                    let target_window = params
                        .get("window_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let our_window = self.window_id.as_deref().unwrap_or("");
                    if !target_window.is_empty()
                        && !our_window.is_empty()
                        && target_window != our_window
                    {
                        tracing::debug!(
                            target: "set_view",
                            target_window,
                            our_window,
                            "ignoring set_view for different window"
                        );
                        return;
                    }
                    let view_str = params.get("view").and_then(|v| v.as_str()).unwrap_or("");
                    tracing::info!(target: "set_view", view = view_str, "applying set_view");
                    match view_str {
                        "sessions" | "agents" => {
                            // User entered session management (via shortcut or UI) —
                            // permanently dismiss the welcome hint.
                            if self.show_welcome_hint {
                                self.show_welcome_hint = false;
                                set_welcome_shown_in_state();
                            }
                            let entering_agents =
                                self.current_tab().current_view != View::Agents;
                            let has_sessions = !self
                                .agent_sessions
                                .iter_sorted_filtered(self.current_cli_filter().as_ref())
                                .is_empty();
                            {
                                let tab = self.current_tab_mut();
                                tab.current_view = View::Agents;
                                if tab.agents_list_state.selected().is_none()
                                    && has_sessions
                                {
                                    tab.agents_list_state.select(Some(0));
                                }
                            }
                            if entering_agents {
                                self.ensure_history_loaded();
                            }
                        }
                        "chat" => {
                            self.current_tab_mut().current_view = View::Chat;
                        }
                        other => {
                            tracing::warn!(target: "set_view", view = other, "unknown view");
                        }
                    }
                    return;
                }

                // Skip events from our own pane
                if self.pane_id.as_deref() == Some(pane_id.as_str()) {
                    tracing::debug!(target: "autofix", "skipped: own pane");
                    return;
                }

                // Bridge WT-native `connection_state` events into the agent
                // session registry so rows transition out of live states
                // (Idle/Working/...) when the underlying pane dies. The
                // hook-bridge path (`agent.session.end` → `SessionStopped`)
                // handles Claude/Copilot, but Gemini has no end-of-session
                // hook, so without this wire a Gemini row spawned via F2
                // resume stays Idle forever after the user types `/exit`.
                //
                // Both event variants are no-ops in the registry when
                // `pane_id` isn't bound to any agent session, so this is
                // safe to apply unconditionally for non-own panes.
                if method == "connection_state" {
                    let state = params.get("state").and_then(|v| v.as_str()).unwrap_or("");
                    match state {
                        "closed" => {
                            self.agent_sessions.apply(
                                crate::agent_sessions::SessionEvent::PaneClosed {
                                    pane_session_id: pane_id.clone(),
                                },
                            );
                        }
                        "failed" => {
                            let reason = params
                                .get("reason")
                                .and_then(|v| v.as_str())
                                .unwrap_or("connection failed")
                                .to_string();
                            self.agent_sessions.apply(
                                crate::agent_sessions::SessionEvent::ConnectionFailed {
                                    pane_session_id: pane_id.clone(),
                                    reason,
                                },
                            );
                        }
                        _ => {}
                    }
                }

                // Detect agent CLI exit when the pane stays alive (e.g. user
                // typed `gemini` inside their pwsh/cmd shell, then `/exit`):
                // the shell emits `osc:133;A` (FinalTerm prompt-start) when
                // it returns to its own prompt. If the pane is currently
                // bound to an agent session, treat that as the agent's
                // teardown signal and transition the row to Ended.
                //
                // We deliberately do NOT depend on the agent's own SessionEnd
                // hook here because:
                //   * Gemini has no reliable hook on `/exit`
                //     (`agent.session.end` is "Hook cancelled" most of the time)
                //   * Even when the hook fires, it races with our event loop
                //
                // The shell's prompt-start marker is the most reliable
                // cross-CLI signal that the agent process has released the
                // foreground.
                if method == "vt_sequence" {
                    let seq = params
                        .get("sequence")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if seq == "osc:133;A"
                        && self.agent_sessions.is_agent_pane(&pane_id)
                    {
                        tracing::info!(
                            target: "agent_session_registry",
                            pane_id = %pane_id,
                            "shell prompt-start in agent-bound pane: treating as agent exit",
                        );
                        self.agent_sessions.apply(
                            crate::agent_sessions::SessionEvent::PaneClosed {
                                pane_session_id: pane_id.clone(),
                            },
                        );
                    }
                }

                let notification = classify_wt_event(&method, &pane_id, &params);
                tracing::debug!(target: "autofix", severity = ?notification.severity, summary = %notification.summary, "classified");

                // Always log to chat for critical/actionable events
                match notification.severity {
                    WtEventSeverity::Critical => {
                        self.current_tab_mut().messages
                            .push(ChatMessage::Error(notification.summary.clone()));
                        self.show_notification_banner = true;
                        self.scroll_to_bottom();
                    }
                    WtEventSeverity::Actionable => {
                        if method == "agent_prompt" {
                            // Command palette prompt: delegate directly to a new tab agent.
                            let prompt = params
                                .get("prompt")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            tracing::info!(target: "autofix", prompt_len = prompt.len(), "agent_prompt: delegating");
                            if !prompt.is_empty() {
                                self.delegate_to_tab_agent(&prompt);
                            }
                            return;
                        }

                        self.show_notification_banner = true;
                        // Only OSC-133;D vt_sequence events carry enough info
                        // to drive autofix (a per-command exit code from a
                        // shell-integrated pane whose shell is still alive
                        // so we can read its buffer). `connection_state:
                        // closed` is just process termination — no exit
                        // code, no command context, fires for both exit 0
                        // and exit 1, and the pane is *gone* so any
                        // downstream `wt_read_last_prompt(<dead_guid>)`
                        // throws E_FAIL on the C++ side. Surface those as a
                        // System message instead.
                        let is_autofix_candidate = method == "vt_sequence";
                        if self.autofix_enabled && is_autofix_candidate {
                            // maybe_trigger_autofix pushes ChatMessage::Error (red dot)
                            // itself — don't double-push here as a System message.
                            self.maybe_trigger_autofix(&notification);
                        } else {
                            // Autofix disabled OR event isn't an autofix
                            // candidate (e.g. connection_state:closed):
                            // surface the event in chat so the user still
                            // sees it.
                            self.current_tab_mut().messages
                                .push(ChatMessage::System(notification.summary.clone()));
                            self.scroll_to_bottom();
                        }
                    }
                    WtEventSeverity::Informational => {
                        // A successful command (exit 0) in the armed/pending pane
                        // means the error was resolved. Cancel any in-flight fix and dismiss.
                        //
                        // Suggested has weaker semantics: any prompt activity in any
                        // pane (osc:133;A start of a new prompt, OR osc:133;D;0
                        // exit-zero) signals the user is moving on. Suggested is a
                        // global UI state, not pane-local.
                        if method == "vt_sequence" {
                            let seq = params.get("sequence").and_then(|v| v.as_str()).unwrap_or("");
                            let is_exit_zero = seq.strip_prefix("osc:133;")
                                .and_then(|rest| rest.strip_prefix("D;"))
                                .and_then(|code| code.trim().parse::<i32>().ok())
                                .map(|c| c == 0)
                                .unwrap_or(false);
                            let is_prompt_start = seq == "osc:133;A";
                            if is_exit_zero && self.autofix_pane_id.as_deref() == Some(pane_id.as_str()) {
                                // `turn_cancel` owns the full cleanup: bumps
                                // `autofix_generation`, emits autofix_state_cleared
                                // (resolving the pane from the AutofixContext, or
                                // `autofix_pane_id` as a fallback), and resets
                                // `tab.turn` to `Idle`. Avoid duplicating its work.
                                let session_id = self.current_tab().session_id.clone();
                                if let Some(sid) = session_id {
                                    self.turn_cancel(&sid);
                                } else {
                                    // No ACP session bound — replicate the
                                    // minimum cleanup turn_cancel would do.
                                    self.autofix_generation =
                                        self.autofix_generation.wrapping_add(1);
                                    self.clear_recommendations();
                                    if let Some(pane) = self.autofix_pane_id.take() {
                                        self.emit_autofix_state_cleared(&pane);
                                    }
                                }
                            }
                            // Suggested: dismiss on prompt activity (exit-zero or
                            // a fresh prompt-start) in ANY pane. Emit cleared
                            // against the original suggested pane so the bar's
                            // lastErrorPaneId stays consistent.
                            if (is_exit_zero || is_prompt_start)
                                && self.suggested_pane_id.is_some()
                            {
                                let pane = self.suggested_pane_id.take().unwrap();
                                self.emit_autofix_state_cleared(&pane);
                            }
                        }
                    }
                }

                // Queue the notification (cap at 20)
                self.wt_notifications.push_back(notification);
                if self.wt_notifications.len() > 20 {
                    self.wt_notifications.pop_front();
                }
            }
            AppEvent::AgentInstallComplete => {
                // Check if the agent we were trying to install is now available.
                let agent_id = self.setup.as_ref()
                    .map(|s| s.preflight.agent_id.clone())
                    .unwrap_or_default();

                if !agent_id.is_empty() {
                    let status = crate::agent_check::check_agent(&agent_id);
                    if status.cli_found {
                        // Install succeeded → proceed to connect or auth
                        let profile = crate::agent_registry::lookup_profile_by_id(&agent_id);
                        let is_fre = self.setup.as_ref()
                            .map(|s| s.reason == SetupReason::FirstRun)
                            .unwrap_or(false);

                        if crate::agent_check::has_credential(&agent_id) {
                            // Has credential → connect directly
                            if is_fre {
                                self.update_deferred_acp_agent(&agent_id);
                                self.pending_acp_start = true;
                            } else {
                                let new_cmd = self.build_agent_cmd(&agent_id);
                                let _ = self.restart_tx.send(RestartRequest { agent_cmd: Some(new_cmd) });
                            }
                            self.mode = AppMode::Chat;
                            self.state = ConnectionState::Connecting(t!("connection.starting").into_owned());
                            let tab = self.current_tab_mut();
                            tab.messages.retain(|m| !matches!(m, ChatMessage::Error(_)));
                            tab.chat_scroll.reset();
                            self.setup = None;
                            self.auth = Some(AuthState {
                                agent_id: agent_id.clone(),
                                agent_name: status.display_name.clone(),
                                auth_hint: profile.auth_hint.to_string(),
                                login_command: crate::agent_check::build_login_cmd(&agent_id),
                                checking: false,
                                status_message: String::new(),
                            });
                        } else {
                            // No credential → auth screen
                            if is_fre {
                                self.update_deferred_acp_agent(&agent_id);
                            }
                            self.mode = AppMode::Auth;
                            self.setup = None;
                            self.auth = Some(AuthState {
                                agent_id: agent_id.clone(),
                                agent_name: status.display_name.clone(),
                                auth_hint: profile.auth_hint.to_string(),
                                login_command: crate::agent_check::build_login_cmd(&agent_id),
                                checking: false,
                                status_message: String::new(),
                            });
                        }
                        return;
                    }
                }

                // Install didn't resolve the issue — stay on setup, refresh options
                if let Some(ref mut setup) = self.setup {
                    setup.install_in_progress = false;
                    let all_statuses = crate::agent_check::check_all_agents();
                    let current_status = if !agent_id.is_empty() {
                        Some(crate::agent_check::check_agent(&agent_id))
                    } else {
                        None
                    };
                    setup.options = build_setup_options(
                        &setup.reason,
                        current_status.as_ref(),
                        &all_statuses,
                    );
                }
            }
            AppEvent::LoginProgress { device_code, verify_url } => {
                if let Some(ref mut auth) = self.auth {
                    auth.status_message = format!(
                        "Visit {} and enter code: {}",
                        verify_url, device_code
                    );
                }
                // Copy device code to clipboard
                #[cfg(windows)]
                {
                    let _ = std::process::Command::new("cmd")
                        .args(["/C", &format!("echo {}| clip", device_code)])
                        .spawn();
                }
            }
            AppEvent::LoginComplete { success, .. } => {
                if success {
                    // Login succeeded → transition to Chat and start ACP
                    self.mode = AppMode::Chat;
                    self.setup = None;
                    self.state = ConnectionState::Connecting(t!("connection.starting").into_owned());
                    let agent_id = self.auth.as_ref().map(|a| a.agent_id.clone()).unwrap_or_default();
                    self.update_deferred_acp_agent(&agent_id);
                    if self.deferred_acp.is_some() {
                        self.pending_acp_start = true;
                    } else {
                        let new_cmd = self.build_agent_cmd(&agent_id);
                        let _ = self.restart_tx.send(RestartRequest { agent_cmd: Some(new_cmd) });
                    }
                    self.auth = None;
                } else {
                    // Login failed — show auth screen again
                    if let Some(ref mut auth) = self.auth {
                        auth.checking = false;
                        if !auth.login_command.contains("copilot") {
                            auth.status_message = t!("system.command_copied_retry").into_owned();
                        }
                    }
                }
            }
        }
    }

    fn event_requires_redraw(&self, event: &AppEvent) -> bool {
        match event {
            AppEvent::Tick => self.has_activity_indicator() || self.show_notification_banner,
            AppEvent::AgentMessageChunk { .. } => true,
            AppEvent::DebugPipeMessage(_) => self.show_debug_panel,
            // History only affects the Agents view; chat doesn't read it.
            // A redraw is cheap enough that we don't bother gating on which
            // view is showing — pay the one frame.
            AppEvent::HistoricalSessionsLoaded(_) => true,
            _ => true,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        tracing::info!(
            target: "input",
            code = ?key.code,
            modifiers = ?key.modifiers,
            input_empty = self.current_tab().input.is_empty(),
            recs = self.current_tab().turn.recommendations().is_some(),
            turns = self.current_tab().completed_turns.len(),
            selected_turn = ?self.current_tab().selected_completed_turn_idx,
            "key received"
        );

        // Any non-Ctrl+C key disarms the close-pane sequence. We allow plain
        // Ctrl presses (modifier-only events) through so the user can still
        // hold Ctrl while preparing to tap C the second time. The Ctrl+C
        // arm-or-fire transitions itself are handled in the match below.
        let is_ctrl_c = matches!(key.code, KeyCode::Char('c'))
            && key.modifiers.contains(KeyModifiers::CONTROL);
        if !is_ctrl_c {
            self.close_pane_armed_at = None;
            // Don't clear `transient_hint` here — it has its own deadline and
            // ui::render checks expiry on each draw. Clearing on every key
            // would steal too much of the hint's visible lifetime.
        }

        // Setup mode: unified setup wizard (FRE + preflight)
        if self.mode == AppMode::Setup {
            self.handle_setup_key(key);
            return;
        }

        // Auth mode: Enter to sign in, Esc to go back
        if self.mode == AppMode::Auth {
            match key.code {
                KeyCode::Enter => {
                    // Extract values before borrowing self again
                    let login_info = self.auth.as_ref().and_then(|a| {
                        if !a.checking && !a.login_command.is_empty() {
                            Some((a.agent_id.clone(), a.login_command.clone()))
                        } else {
                            None
                        }
                    });
                    if let Some((agent_id, login_cmd)) = login_info {
                        if login_cmd.contains("copilot") {
                            // Copilot: auto device-flow sign-in via piped stdio
                            if let Some(ref mut auth) = self.auth {
                                auth.checking = true;
                            }
                            self.spawn_login(&agent_id, &login_cmd);
                        } else {
                            // Non-Copilot agents: copy command to clipboard, re-check credential
                            #[cfg(windows)]
                            {
                                let _ = std::process::Command::new("powershell")
                                    .args(["-NoProfile", "-Command", &format!("Set-Clipboard '{}'", login_cmd.replace('\'', "''"))])
                                    .stdin(std::process::Stdio::null())
                                    .stdout(std::process::Stdio::null())
                                    .stderr(std::process::Stdio::null())
                                    .spawn();
                            }

                            if let Some(ref mut auth) = self.auth {
                                auth.checking = true;
                                auth.status_message = String::new();
                            }

                            // Re-check credential asynchronously
                            if let Some(ref tx) = self.event_tx {
                                let tx = tx.clone();
                                let id = agent_id.clone();
                                tokio::task::spawn_local(async move {
                                    let result = tokio::task::spawn_blocking(move || {
                                        crate::agent_check::has_credential(&id)
                                    }).await;
                                    let success = result.unwrap_or(false);
                                    let _ = tx.send(AppEvent::LoginComplete { agent_id, success });
                                });
                            }
                        }
                    }
                }
                KeyCode::Esc => {
                    if self.setup.is_some() {
                        // Go back to setup screen
                        self.mode = AppMode::Setup;
                    } else {
                        // No setup to go back to (e.g. preflight auth failure) —
                        // rebuild setup as AgentMissing for this agent
                        let agent_id = self.auth.as_ref()
                            .map(|a| a.agent_id.clone())
                            .unwrap_or_default();
                        if !agent_id.is_empty() {
                            let all_agents = crate::agent_check::check_all_agents();
                            let agent_status = crate::agent_check::check_agent(&agent_id);
                            let profile = crate::agent_registry::lookup_profile_by_id(&agent_id);
                            let reason = SetupReason::AgentError;
                            let options = build_setup_options(&reason, Some(&agent_status), &all_agents);
                            self.mode = AppMode::Setup;
                            self.setup = Some(SetupState {
                                reason,
        
                                selected_index: 0,
                                preflight: PreflightResult {
                                    agent_id: agent_id.clone(),
                                    display_name: profile.display_name.to_string(),
                                    cli_status: CheckStatus::Passed,
                                    cli_path: None,
                                    auth_status: CheckStatus::Failed(t!("system.authentication_failed").into_owned()),
                                    install_hint: profile.install_hint.to_string(),
                                    install_url: String::new(),
                                    auth_hint: profile.auth_hint.to_string(),
                                },
                                install_in_progress: false,
                                install_log: Vec::new(),
                                install_error: None,
                                options,
                                title: t!("setup.title.sign_in").into_owned(),
                                subtitle: if profile.id == "copilot" {
                                    t!("setup.subtitle.copilot_auth", agent = profile.display_name).into_owned()
                                } else {
                                    t!("setup.subtitle.agent_auth", agent = profile.display_name).into_owned()
                                },
                            });
                        } else {
                            self.mode = AppMode::Chat;
                        }
                    }
                    self.auth = None;
                }
                _ => {}
            }
            return;
        }

        // Agents view (F2): list navigation + Enter to focus pane + Delete
        // to evict an Ended/Historical row. Captures all input while open
        // — including Esc which closes the view. View open-state and the
        // selection cursor are per-tab on `TabSession` so each WT tab
        // keeps its own picker state across switches.
        if self.current_tab().current_view == View::Agents {
            let filter = self.current_cli_filter();
            let count = self.agent_sessions.iter_sorted_filtered(filter.as_ref()).len();
            match key.code {
                KeyCode::Down => {
                    let cur = self.current_tab().agents_list_state.selected().unwrap_or(0);
                    let next = if count == 0 { 0 } else { (cur + 1).min(count - 1) };
                    self.current_tab_mut().agents_list_state.select(Some(next));
                }
                KeyCode::Up => {
                    let cur = self.current_tab().agents_list_state.selected().unwrap_or(0);
                    self.current_tab_mut()
                        .agents_list_state
                        .select(Some(cur.saturating_sub(1)));
                }
                KeyCode::Enter => {
                    if let Some(idx) = self.current_tab().agents_list_state.selected() {
                        let selected = self
                            .agent_sessions
                            .iter_sorted_filtered(filter.as_ref())
                            .get(idx)
                            .map(|s| (*s).clone());
                        if let Some(s) = selected {
                            use crate::agent_sessions::AgentStatus::*;
                            // Shift+Enter on a terminal-state row resumes
                            // the session in the agent pane of a new WT
                            // tab via ACP session/load. Plain Enter keeps
                            // the legacy behaviour (split a normal pane
                            // running `<cli> --resume <key>` for terminal
                            // rows, or focus the existing pane for live
                            // rows).
                            if key.modifiers.contains(KeyModifiers::SHIFT)
                                && matches!(s.status, Ended | Historical)
                            {
                                self.dispatch_resume_in_agent_pane(&s);
                            } else {
                                self.activate_agent_session(&s);
                            }
                        }
                    }
                }
                KeyCode::Delete => {
                    if let Some(idx) = self.current_tab().agents_list_state.selected() {
                        let target = self
                            .agent_sessions
                            .iter_sorted_filtered(filter.as_ref())
                            .get(idx)
                            .map(|s| (s.key.clone(), s.status.clone()));
                        if let Some((key, status)) = target {
                            use crate::agent_sessions::AgentStatus::*;
                            // Evicting a live session would orphan its pane,
                            // so restrict Delete to terminal states. Live
                            // rows transition to Ended via SessionStopped.
                            if matches!(status, Ended | Historical) {
                                self.agent_sessions.remove(&key);
                                // Keep the cursor in-bounds after eviction.
                                // Re-query through the same filter so the
                                // selection clamp matches the rendered list.
                                let new_count = self
                                    .agent_sessions
                                    .iter_sorted_filtered(filter.as_ref())
                                    .len();
                                let tab = self.current_tab_mut();
                                if new_count == 0 {
                                    tab.agents_list_state.select(None);
                                } else if idx >= new_count {
                                    tab.agents_list_state.select(Some(new_count - 1));
                                }
                            }
                        }
                    }
                }
                KeyCode::Esc => {
                    self.current_tab_mut().current_view = View::Chat;
                    self.emit_view_changed("chat");
                }
                _ => {}
            }
            return;
        }

        // If permission card is showing, route keys there. Buttons are
        // rendered horizontally inside the embedded card (same chrome as
        // recommendations), so Left/Right move the focus; Up/Down kept as
        // aliases for muscle memory from the prior modal.
        if let Some(ref mut perm) = self.current_tab_mut().permission {
            match key.code {
                KeyCode::Left | KeyCode::Up => {
                    if perm.selected > 0 {
                        perm.selected -= 1;
                    }
                }
                KeyCode::Right | KeyCode::Down => {
                    if perm.selected < perm.options.len().saturating_sub(1) {
                        perm.selected += 1;
                    }
                }
                KeyCode::Enter => {
                    let option_id = perm.options[perm.selected].id.clone();
                    // Take ownership to send
                    if let Some(perm) = self.current_tab_mut().permission.take() {
                        if let Some(responder) = perm.responder {
                            let _ = responder.send(option_id);
                        } else {
                            let _ = self.permission_tx.send(option_id);
                        }
                    }
                }
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    // Quick allow: find first allow option
                    if let Some(idx) = perm.options.iter().position(|o| o.kind.contains("allow")) {
                        let option_id = perm.options[idx].id.clone();
                        if let Some(perm) = self.current_tab_mut().permission.take() {
                            if let Some(responder) = perm.responder {
                                let _ = responder.send(option_id);
                            } else {
                                let _ = self.permission_tx.send(option_id);
                            }
                        }
                    }
                }
                KeyCode::Char('n') | KeyCode::Char('N') => {
                    // Quick deny: find first reject option
                    if let Some(idx) = perm.options.iter().position(|o| o.kind.contains("reject")) {
                        let option_id = perm.options[idx].id.clone();
                        if let Some(perm) = self.current_tab_mut().permission.take() {
                            if let Some(responder) = perm.responder {
                                let _ = responder.send(option_id);
                            } else {
                                let _ = self.permission_tx.send(option_id);
                            }
                        }
                    }
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Up if self.current_tab().input.is_empty() && self.current_tab().turn.recommendations().is_some() => {
                if self.current_tab_mut().selected_recommendation > 0 {
                    self.current_tab_mut().selected_recommendation -= 1;
                    self.current_tab_mut().selected_button = self.default_button_for_selected();
                    self.scroll_rec_to_selected(self.main_area_width());
                }
            }
            KeyCode::Down if self.current_tab().input.is_empty() && self.current_tab().turn.recommendations().is_some() => {
                let choices_len = self
                    .current_tab()
                    .turn
                    .recommendations()
                    .map(|r| r.choices.len())
                    .unwrap_or(0);
                if self.current_tab().selected_recommendation + 1 < choices_len {
                    let default_btn = self.default_button_for_selected();
                    self.current_tab_mut().selected_recommendation += 1;
                    self.current_tab_mut().selected_button = default_btn;
                    self.scroll_rec_to_selected(self.main_area_width());
                }
            }
            KeyCode::Right | KeyCode::Tab
                if self.current_tab().input.is_empty() && self.current_tab().turn.recommendations().is_some() =>
            {
                // Cycle button focus forward within the selected card.
                // Send: 0=Run, 1=Insert. OpenAndSend has only index 0.
                let button_count = self.button_count_for_selected();
                if button_count > 1 {
                    self.current_tab_mut().selected_button = (self.current_tab_mut().selected_button + 1) % button_count;
                }
            }
            KeyCode::Tab
                if self.current_tab().input.is_empty()
                    && self.current_tab().turn.recommendations().is_none()
                    && !self.current_tab().completed_turns.is_empty() =>
            {
                self.current_tab_mut().select_older_completed_turn();
            }
            KeyCode::BackTab
                if self.current_tab().input.is_empty()
                    && self.current_tab().turn.recommendations().is_none()
                    && !self.current_tab().completed_turns.is_empty() =>
            {
                self.current_tab_mut().select_newer_completed_turn();
            }
            KeyCode::Esc
                if self.current_tab().selected_completed_turn_idx.is_some() =>
            {
                // Esc clears the past-turn selection without any other side
                // effect. Lets the user back out of the history nav cleanly.
                self.current_tab_mut().selected_completed_turn_idx = None;
            }
            KeyCode::Left
                if self.current_tab().input.is_empty() && self.current_tab().turn.recommendations().is_some() =>
            {
                // Cycle button focus backward.
                let button_count = self.button_count_for_selected();
                if button_count > 1 {
                    self.current_tab_mut().selected_button = (self.current_tab_mut().selected_button + button_count - 1) % button_count;
                }
            }
            KeyCode::F(12) => {
                self.show_debug_panel = !self.show_debug_panel;
                self.debug_capture_enabled
                    .store(self.show_debug_panel, Ordering::Relaxed);
                self.debug_scroll = 0;
                return;
            }
            KeyCode::PageUp
                if key.modifiers.contains(KeyModifiers::SHIFT) && self.show_debug_panel =>
            {
                self.debug_scroll = self.debug_scroll.saturating_add(10);
                return;
            }
            KeyCode::PageDown
                if key.modifiers.contains(KeyModifiers::SHIFT) && self.show_debug_panel =>
            {
                self.debug_scroll = self.debug_scroll.saturating_sub(10);
                return;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // In-flight: state is Submitted/Streaming or Surfaced{end_pending}.
                let in_flight = !self.current_tab().turn.is_idle()
                    && !matches!(
                        self.current_tab().turn,
                        TurnState::Surfaced { end_pending: false, .. }
                    );
                if in_flight {
                    // Send a session/cancel to the ACP client. The client
                    // will fire the protocol notification and signal the
                    // per-prompt oneshot so the spawned task drops out of
                    // conn.prompt() immediately.
                    let session_id = self.current_tab().session_id.clone();
                    if let Some(sid) = session_id.clone() {
                        let _ = self.cancel_tx.send(CancelRequest { session_id: sid });
                    }
                    if let Some(sid) = session_id {
                        self.turn_cancel(&sid);
                    }
                    let tab = self.current_tab_mut();
                    tab.messages.push(ChatMessage::System(t!("system.cancelled").into_owned()));
                    tab.scroll_to_bottom();
                    self.close_pane_armed_at = None;
                } else if !self.current_tab().input.is_empty() {
                    // Mirror bash readline: Ctrl+C clears the buffer.
                    self.current_tab_mut().clear_input();
                    self.close_pane_armed_at = None;
                } else {
                    // Idle + empty input. First press arms; second press
                    // within CLOSE_PANE_ARM_WINDOW asks WT to close the
                    // pane. We never set should_quit ourselves — the pane
                    // teardown will kill our ConPty, which is the only
                    // path that should terminate wta.
                    let now = std::time::Instant::now();
                    let armed = self
                        .close_pane_armed_at
                        .map(|t| now.duration_since(t) < CLOSE_PANE_ARM_WINDOW)
                        .unwrap_or(false);
                    if armed {
                        self.close_pane_armed_at = None;
                        self.transient_hint = None;
                        self.request_close_agent_pane();
                    } else {
                        self.close_pane_armed_at = Some(now);
                        self.transient_hint = Some((
                            "Press Ctrl+C again to close pane".to_string(),
                            now + CLOSE_PANE_ARM_WINDOW,
                        ));
                    }
                }
            }
            KeyCode::Esc if self.help_overlay_visible => {
                self.help_overlay_visible = false;
            }
            KeyCode::Esc if self.show_notification_banner => {
                self.dismiss_notifications();
            }
            KeyCode::Esc
                if self.current_tab().turn.recommendations().is_some()
                    || (self.autofix_pane_id.is_some()
                        && !self.current_tab().turn.is_idle()) =>
            {
                // Dismiss armed fix card or cancel in-flight autofix request.
                // `turn_cancel` bumps generation, emits autofix_state_cleared,
                // and resets the state machine to Idle.
                let session_id = self.current_tab().session_id.clone();
                if let Some(sid) = session_id {
                    self.turn_cancel(&sid);
                } else {
                    // No session attached yet — fall back to manual cleanup
                    // (no chunks can be in flight in that case).
                    self.autofix_generation = self.autofix_generation.wrapping_add(1);
                    if let Some(p) = self.autofix_pane_id.take() {
                        self.emit_autofix_state_cleared(&p);
                    }
                }
            }
            // Dismiss the bottom-bar Suggested indicator (autofix produced an
            // explanation, not an executable fix). Reachable only when the user
            // is interacting with this TUI — i.e. the agent pane is currently
            // visible. Other dismiss paths: clicking the bar (opens pane), or
            // any prompt activity in any pane (exit-zero or osc:133;A).
            //
            // NOTE: this only handles the default-tui (single-process) mode.
            // In shared-host attach mode `suggested_pane_id` lives on the host;
            // the attach client would need to send a HostCommand::DismissSuggestion.
            // TODO: wire that path when shared-host mode is exercised.
            KeyCode::Esc if self.suggested_pane_id.is_some() => {
                let pane = self.suggested_pane_id.take().unwrap();
                self.emit_autofix_state_cleared(&pane);
            }
            KeyCode::Esc => {
                self.current_tab_mut().clear_input();
            }
            KeyCode::Up if self.command_popup_visible() => {
                self.current_tab_mut().command_popup_up();
            }
            KeyCode::Down if self.command_popup_visible() => {
                self.current_tab_mut().command_popup_down();
            }
            KeyCode::Tab if self.command_popup_visible() => {
                self.current_tab_mut().accept_command_popup_completion();
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.current_tab_mut().insert_input_char('\n');
            }
            KeyCode::Enter if self.command_popup_visible() => {
                // Popup is showing — Enter runs the highlighted command
                // (/, /h, /he etc. → /help) instead of committing the
                // raw text as a prompt. Esc dismisses if the user
                // doesn't want any command.
                if let Some(spec) = self.current_tab().selected_command_spec() {
                    let parsed = ParsedCommand {
                        kind: spec.kind,
                        spec,
                        rest: String::new(),
                    };
                    self.current_tab_mut().clear_input();
                    self.handle_slash_command(parsed);
                }
            }
            KeyCode::Enter
                if self.current_tab().input.is_empty()
                    && self.current_tab().selected_completed_turn_idx.is_some()
                    && self.current_tab().turn.recommendations().is_none() =>
            {
                // A past turn is highlighted via Tab — Enter toggles its
                // expanded state instead of submitting / activating recs.
                self.current_tab_mut().toggle_selected_completed_turn();
            }
            KeyCode::Enter => {
                let _tab = self.current_tab();
                tracing::debug!(target: "autofix", input_empty = _tab.input.is_empty(), state = ?self.state, has_recs = _tab.turn.recommendations().is_some(), autofix_pane = ?self.autofix_pane_id, selected_idx = _tab.selected_recommendation, "Enter");
                // Slash-command intercept. Runs before the prompt path so
                // commands like /stop work even mid-flight, and /help / /clear
                // / /exit work even when the agent isn't Connected.
                //
                // `//literal` falls through to the prompt path (parse() returns
                // None), and the leading `/` is left intact — the agent sees
                // exactly what the user typed.
                if !self.current_tab().input.is_empty() {
                    if let Some(cmd) = commands::parse(&self.current_tab().input) {
                        self.current_tab_mut().clear_input();
                        self.handle_slash_command(cmd);
                        return;
                    } else if self.current_tab().input.trim_start().starts_with('/')
                        && !self.current_tab().input.trim_start().starts_with("//")
                    {
                        // Looks like an attempted command but the name isn't
                        // registered: warn the user but still send the line as
                        // a prompt so they don't lose what they typed.
                        let unknown = self
                            .current_tab()
                            .input
                            .trim_start()
                            .split_whitespace()
                            .next()
                            .unwrap_or("/")
                            .to_string();
                        let tab = self.current_tab_mut();
                        tab.messages.push(ChatMessage::System(format!(
                            "Unknown command \"{}\" — sent as prompt. Type /help for the list.",
                            unknown
                        )));
                        // Fall through to the prompt path below.
                    }
                }
                if self.current_tab().input.is_empty()
                    && self.state == ConnectionState::Connected
                    && self.current_tab().turn.recommendations().is_some()
                {
                    // Card is visible — Enter executes the selected choice.
                    // `turn_execute_card` dispatches the choice to the
                    // coordinator, transitions the state machine to
                    // `Surfaced{Empty, end_pending preserved}`, and emits
                    // the autofix-cleared bottom-bar event when applicable.
                    let session_id = self.current_tab().session_id.clone();
                    if let Some(session_id) = session_id {
                        let label_choice = self
                            .selected_recommendation_choice()
                            .map(|c| c.choice)
                            .unwrap_or(0);
                        let insert_only = self.current_tab().selected_button == 1
                            && self
                                .selected_recommendation_choice()
                                .map(|c| self.is_send_choice(c))
                                .unwrap_or(false);
                        tracing::info!(
                            target: "autofix",
                            choice = label_choice,
                            insert_only,
                            "Executing choice",
                        );
                        let label = if insert_only { "Inserting" } else { "Executing" };
                        self.push_execution_info(format!(
                            "{} choice {}.",
                            label, label_choice
                        ));
                        self.turn_execute_card(&session_id);
                    }
                } else if !self.current_tab().input.is_empty() && self.state == ConnectionState::Connected {
                    // Same-tab single-flight: refuse a new prompt if the
                    // turn isn't accepting one. The ACP transport rejects
                    // too, but bouncing here keeps the user's input intact.
                    if !self.current_tab().turn.accepts_new_prompt() {
                        let tab = self.current_tab_mut();
                        tab.messages.push(ChatMessage::System(
                            "Agent is busy on this tab — wait for the current prompt to finish."
                                .to_string(),
                        ));
                        tab.scroll_to_bottom();
                        return;
                    }
                    let tab = self.current_tab_mut();
                    let text = std::mem::take(&mut tab.input);
                    tab.cursor_pos = 0;
                    tab.refresh_command_popup();
                    // `session_id` may be None on a brand-new tab whose ACP
                    // session is created lazily by `dispatch_prompt_body`.
                    // Fall back to a key that `session_tab_mut`'s
                    // `tab_for_session` resolves to the active tab — same
                    // trick as `maybe_trigger_autofix` — so the state
                    // machine still installs the turn on this tab. When
                    // `SessionAttached` later writes the real session id,
                    // subsequent chunks route here correctly.
                    let session_id = tab
                        .session_id
                        .clone()
                        .unwrap_or_else(|| DEFAULT_TAB_ID.to_string());
                    let pane_context = PaneContext {
                        pane_id: self.pane_id.clone(),
                        tab_id: self.tab_id.clone(),
                        window_id: self.window_id.clone(),
                        cwd: None,
                        source_pane_id: None,
                    };
                    let prompt = PromptSubmission::new(text.clone(), Some(pane_context));
                    prompt_timing_log(
                        prompt.id,
                        prompt.submitted_at_unix_s,
                        "ui_submit",
                        &format!("preview={:?}", prompt.preview()),
                    );
                    if self.show_welcome_hint {
                        self.show_welcome_hint = false;
                        set_welcome_shown_in_state();
                    }
                    let submitted = SubmittedPrompt {
                        id: prompt.id,
                        text: text.clone(),
                        submitted_at_unix_s: prompt.submitted_at_unix_s,
                        autofix: None,
                    };
                    self.turn_submit_prompt(&session_id, submitted);
                    let _ = self.prompt_tx.send(prompt);
                }
            }
            KeyCode::Backspace => {
                self.current_tab_mut().delete_before_cursor();
            }
            KeyCode::Delete => {
                self.current_tab_mut().delete_at_cursor();
            }
            KeyCode::Left if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.current_tab_mut().move_cursor_word_left();
            }
            KeyCode::Right if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.current_tab_mut().move_cursor_word_right();
            }
            KeyCode::Left => {
                self.current_tab_mut().move_cursor_left();
            }
            KeyCode::Right => {
                self.current_tab_mut().move_cursor_right();
            }
            KeyCode::Home => {
                self.current_tab_mut().move_cursor_home();
            }
            KeyCode::End => {
                self.current_tab_mut().move_cursor_end();
            }
            KeyCode::PageUp => {
                self.current_tab_mut().chat_scroll.by(10);
            }
            KeyCode::PageDown => {
                self.current_tab_mut().chat_scroll.by(-10);
            }
            KeyCode::Char(c) => {
                self.current_tab_mut().insert_input_char(c);
            }
            _ => {}
        }
    }

    fn scroll_to_bottom(&mut self) {
        self.current_tab_mut().scroll_to_bottom();
    }

    fn has_activity_indicator(&self) -> bool {
        if self.mode == AppMode::Setup || self.mode == AppMode::Auth {
            return true; // spinner always ticks in setup/auth mode
        }
        if self.history_load_state == HistoryLoadState::Loading {
            return true; // agents-view "Loading" shimmer
        }
        let tab = self.current_tab();
        tab.turn.spinner_label().is_some() || tab.progress_status.is_some()
    }

    /// Get the most recent unacknowledged notification (for the banner).
    #[allow(dead_code)]
    pub fn active_notification(&self) -> Option<&WtNotification> {
        self.wt_notifications
            .iter()
            .rev()
            .find(|n| !n.acknowledged)
    }

    /// Count of unacknowledged actionable/critical notifications.
    #[allow(dead_code)]
    pub fn unacknowledged_count(&self) -> usize {
        self.wt_notifications
            .iter()
            .filter(|n| !n.acknowledged && n.severity != WtEventSeverity::Informational)
            .count()
    }

    /// Dismiss the notification banner and mark all current notifications as acknowledged.
    pub fn dismiss_notifications(&mut self) {
        self.show_notification_banner = false;
        for n in self.wt_notifications.iter_mut() {
            n.acknowledged = true;
        }
    }

    /// Get the latest status-bar badge text (if any unacknowledged notification exists).
    #[allow(dead_code)]
    pub fn notification_badge(&self) -> Option<(&str, &WtEventSeverity)> {
        // Show the most severe unacknowledged notification
        self.wt_notifications
            .iter()
            .rev()
            .find(|n| !n.acknowledged)
            .map(|n| (n.summary.as_str(), &n.severity))
    }

    /// Visible popup state for the renderer. Returns `None` when the
    /// popup should not be drawn this frame. Reads from the active tab.
    pub fn command_popup_state(&self) -> Option<crate::ui::PopupState<'_>> {
        let tab = self.current_tab();
        if tab.command_popup_candidates.is_empty() {
            None
        } else {
            Some(crate::ui::PopupState {
                candidates: &tab.command_popup_candidates,
                selected: tab.command_popup_selected,
            })
        }
    }

    fn command_popup_visible(&self) -> bool {
        self.current_tab().command_popup_visible()
    }

    /// Dispatch a parsed slash-command. The Enter handler is responsible
    /// for clearing the input and cursor before calling this.
    fn handle_slash_command(&mut self, cmd: ParsedCommand) {
        let in_flight = self.current_tab().turn.is_in_flight();
        tracing::info!(
            target: "slash_cmd",
            name = cmd.spec.name,
            in_flight,
            "dispatch"
        );

        match cmd.kind {
            CommandKind::Help => {
                self.help_overlay_visible = !self.help_overlay_visible;
            }
            CommandKind::Clear => {
                let tab = self.current_tab_mut();
                tab.clear_chat_history();
                tab.completed_turns.clear();
                tab.selected_completed_turn_idx = None;
                tab.scroll_to_bottom();
            }
            CommandKind::Stop => {
                if in_flight {
                    let session_id = self.current_tab().session_id.clone();
                    if let Some(sid) = session_id.clone() {
                        let _ = self.cancel_tx.send(CancelRequest { session_id: sid });
                    }
                    if let Some(sid) = session_id {
                        self.turn_cancel(&sid);
                    }
                    let tab = self.current_tab_mut();
                    tab.messages
                        .push(ChatMessage::System(t!("system.cancelled").into_owned()));
                    tab.scroll_to_bottom();
                } else {
                    let tab = self.current_tab_mut();
                    tab.messages
                        .push(ChatMessage::System(t!("system.no_prompt_in_flight").into_owned()));
                    tab.scroll_to_bottom();
                }
            }
            CommandKind::New => {
                if in_flight {
                    let tab = self.current_tab_mut();
                    tab.messages.push(ChatMessage::System(
                        "Wait for the current prompt to finish, or /stop first.".to_string(),
                    ));
                    tab.scroll_to_bottom();
                    return;
                }
                let tab_id = self
                    .tab_id
                    .clone()
                    .unwrap_or_else(|| DEFAULT_TAB_ID.to_string());
                let _ = self.new_session_tx.send(NewSessionForTab {
                    tab_id,
                    cwd: None,
                });
                let tab = self.current_tab_mut();
                tab.clear_chat_history();
                tab.completed_turns.clear();
                tab.selected_completed_turn_idx = None;
                tab.session_id = None;
                tab.scroll_to_bottom();
            }
            CommandKind::Sessions => {
                // Mirror the F2 keybinding's open path: jump straight to
                // the Agents picker and seed a selection so Enter/Up/Down
                // are immediately useful. Esc / F2 still close the view.
                // Per-tab — only flips the active tab's view state.
                let has_sessions = !self
                    .agent_sessions
                    .iter_sorted_filtered(self.current_cli_filter().as_ref())
                    .is_empty();
                {
                    let tab = self.current_tab_mut();
                    if tab.agents_list_state.selected().is_none() && has_sessions {
                        tab.agents_list_state.select(Some(0));
                    }
                    tab.current_view = View::Agents;
                }
                // F2 path also kicks the lazy history scan here. Without this,
                // /sessions left the registry empty and rendered a blank view
                // forever (state stuck at NotStarted, no Loading row, no rows).
                self.ensure_history_loaded();
                self.emit_view_changed("sessions");
            }
            CommandKind::Restart => {
                // Full reconnect. Reset every tab: drop session_id (the
                // old SessionIds are about to become invalid), clear
                // streaming state, wipe scrollback. The ACP client side
                // will kill the agent child and respawn it; subsequent
                // prompts on each tab will lazily get a fresh session.
                self.state = ConnectionState::Connecting("Restarting agent...".to_string());
                self.session_to_tab.clear();
                self.session_id.clear();
                for (_, tab) in self.tab_sessions.iter_mut() {
                    tab.clear_chat_history();
                    tab.completed_turns.clear();
                    tab.selected_completed_turn_idx = None;
                    tab.session_id = None;
                }
                let _ = self.restart_tx.send(RestartRequest { agent_cmd: None });
                self.publish_agent_status();
            }
        }
    }

    /// Width of the main area (chat / recs / perm / input) — matches the
    /// 60/40 horizontal split in `ui::layout::render` when the debug panel is
    /// open. All card/wrap calculations must root here, not `terminal_cols`.
    pub fn main_area_width(&self) -> u16 {
        if self.show_debug_panel {
            self.terminal_cols.saturating_mul(60) / 100
        } else {
            self.terminal_cols
        }
    }

    /// Height of the recommendations panel — grows to fit content, capped so
    /// input and chat still have room, but floored at the tallest card's
    /// height so any card is fully renderable when scrolled to. Using the
    /// tallest (not just the recommended) means Down/Up navigation never
    /// lands on a card too tall for the panel.
    ///
    /// `panel_width` is the actual render width (`main_area.width` after the
    /// debug-panel split), not `terminal_cols` — passing the wrong one
    /// under-counts wrap rows and clips the bottom card when the debug panel
    /// is open.
    pub fn rec_panel_height(&self, panel_width: u16) -> u16 {
        let Some(recs) = self.current_tab().turn.recommendations() else { return 0 };
        let card_heights = recs.choices.iter().map(|c| rec_card_height(c, panel_width) as u16);
        let total = card_heights.clone().sum::<u16>();
        let floor = card_heights.max().unwrap_or(ui::card::CARD_MIN_SIZE);
        // Reserve: input(3) + chat_min(1) + rec_hint(1) = 5.
        let ceiling = self.terminal_rows.saturating_sub(5);
        total.min(ceiling).max(floor)
    }

    /// Height reserved for the embedded permission card. Returns 0 only when
    /// no permission is pending — when one *is* pending, the user must be
    /// able to see it (the agent flow is blocked until they answer), so we
    /// fall back to a 1-row compact strip when the full card can't fit.
    /// `permission::render` reads the actual reserved height and switches
    /// between full and compact rendering.
    ///
    /// `panel_width` is the actual render width (`main_area.width` after the
    /// debug-panel split), not `terminal_cols`.
    pub fn permission_panel_height(&self, panel_width: u16) -> u16 {
        let Some(perm) = self.current_tab().permission.as_ref() else { return 0 };
        let card_h = permission_card_height(perm, panel_width) as u16;
        // Permission is modal — only hard-reserve input(3).
        let ceiling = self.terminal_rows.saturating_sub(3);
        let h = card_h.min(ceiling);
        if h >= ui::card::CARD_MIN_SIZE { h } else { 1 }
    }

    /// Recompute `rec_scroll.max` from the current card heights and the
    /// panel's available cards region. Called from layout.rs before
    /// `recommendations::render` so the renderer stays `&App` and any
    /// wheel-driven over-scroll is clamped before paint.
    pub fn sync_rec_scroll_max(&mut self, panel_width: u16) {
        let panel_cards_h = self.rec_panel_height(panel_width) as usize;
        let Some(recs) = self.current_tab().turn.recommendations() else { return };
        let total: usize = recs.choices.iter().map(|c| rec_card_height(c, panel_width)).sum();
        self.current_tab_mut().rec_scroll.set_max(total.saturating_sub(panel_cards_h));
    }

    fn clear_recommendations(&mut self) {
        self.current_tab_mut().clear_recommendations();
    }

    /// Scroll the rec panel so the selected card's top sits at the panel top.
    fn scroll_rec_to_selected(&mut self, panel_width: u16) {
        let panel_height = self.rec_panel_height(panel_width) as usize;
        let Some(recs) = self.current_tab().turn.recommendations().cloned() else { return };

        let mut line_top = 0usize;
        for (idx, choice) in recs.choices.iter().enumerate() {
            let card_h = rec_card_height(choice, panel_width);
            if idx == self.current_tab().selected_recommendation {
                let tab = self.current_tab_mut();
                if line_top < tab.rec_scroll.offset
                    || line_top + card_h > tab.rec_scroll.offset + panel_height
                {
                    tab.rec_scroll.set(line_top);
                }
                return;
            }
            line_top += card_h;
        }
    }

    /// Switch the active tab. Per-tab state lives in `tab_sessions`, so all
    /// this does is materialize the destination entry (if missing) and
    /// update `tab_id`. No swapping or copying — the previous tab's state
    /// stays exactly where it was.
    fn switch_tab_session(&mut self, new_tab_id: String) {
        let old_tab = self.tab_id.clone();
        let entry = self.tab_sessions.entry(new_tab_id.clone()).or_default();
        tracing::info!(
            target: "tab_session",
            from = ?old_tab,
            to = %new_tab_id,
            target_completed_turns = entry.completed_turns.len(),
            target_messages = entry.messages.len(),
            "switch_tab_session"
        );
        self.tab_id = Some(new_tab_id);
    }

    /// Drop the per-tab state for a tab that WT has just destroyed. Removes
    /// the matching `TabSession` and prunes any `session_to_tab` entries
    /// that pointed at it (so a future SessionId reuse can't route into the
    /// dead tab's slot). Refuses to drop `DEFAULT_TAB_ID` since the App
    /// always needs at least one materialized tab to render.
    fn drop_tab_session(&mut self, closed_tab_id: &str) {
        if closed_tab_id == DEFAULT_TAB_ID {
            tracing::warn!(
                target: "tab_session",
                "tab_closed: refusing to drop default tab"
            );
            return;
        }
        let removed = self.tab_sessions.remove(closed_tab_id);
        self.session_to_tab.retain(|_, tab| tab != closed_tab_id);
        if self.tab_id.as_deref() == Some(closed_tab_id) {
            // Active tab is gone; the next focused tab's tab_changed will
            // arrive imminently, but in the meantime `current_tab()` must
            // not panic. `active_tab_key()` falls back to DEFAULT_TAB_ID
            // when tab_id is None, so re-materialize that slot. The
            // fallback session is empty by design; renders during the gap
            // just show nothing.
            self.tab_id = None;
            self.tab_sessions
                .entry(DEFAULT_TAB_ID.to_string())
                .or_default();
        }
        tracing::info!(
            target: "tab_session",
            tab_id = closed_tab_id,
            had_session = removed.is_some(),
            remaining_tabs = self.tab_sessions.len(),
            "drop_tab_session"
        );
    }

    /// Wipe per-tab state in place while keeping the `TabSession` slot
    /// alive. Called when WT sends `reset_tab_session` (the Ctrl+C×2 hide
    /// path): the WT tab itself isn't going anywhere, but the user asked
    /// for a clean slate on this tab. After this:
    ///   - Conversation history, completed turns, in-flight state are gone.
    ///   - `session_to_tab` entries pointing at this tab are pruned so any
    ///     late ACP events for the old SessionId can't route back in.
    ///   - The ACP client task is asked to drop the binding in
    ///     `tab_to_session` and cancel any in-flight prompt for the old
    ///     SessionId; the next prompt on this tab lazily creates a fresh
    ///     ACP session.
    /// Unlike `drop_tab_session`, this preserves the HashMap key — the
    /// next tab_changed back into this tab finds an empty-but-present
    /// `TabSession` and just renders an empty chat.
    fn reset_tab_session_for(&mut self, tab_id: &str) {
        // Same wipe as the `/clear` slash command: clear in-flight chat state
        // via `clear_chat_history` AND the completed-turn history that
        // `clear_chat_history` deliberately leaves alone.
        if let Some(tab) = self.tab_sessions.get_mut(tab_id) {
            tab.clear_chat_history();
            tab.completed_turns.clear();
            tab.selected_completed_turn_idx = None;
            tab.scroll_to_bottom();
            tab.session_id = None;
        }

        // Prune the reverse SessionId → tab routing so late ACP chunks for
        // the dropped session can't land on this tab's slot.
        self.session_to_tab.retain(|_, t| t != tab_id);

        // Ask the ACP client task to release the binding for this tab.
        let _ = self
            .drop_session_tx
            .send(DropSessionRequest {
                tab_id: tab_id.to_string(),
            });

        tracing::info!(
            target: "tab_session",
            tab_id = tab_id,
            "reset_tab_session_for done"
        );
    }


    fn session_completion_latency_summary(&self, session_id: &str) -> Option<String> {
        let mut parts = Vec::new();
        let tab = self.session_tab(session_id);

        if let Some(prompt) = tab.turn.prompt() {
            let total_s = (now_unix_s() - prompt.submitted_at_unix_s).max(0.0);
            parts.push(format!("total {:.3}s", total_s));
        }

        if let Some(note) = tab.timing_note.as_deref().filter(|note| !note.is_empty()) {
            parts.push(note.to_string());
        }

        if parts.is_empty() {
            None
        } else {
            Some(format!("Latency: {}", parts.join(" | ")))
        }
    }

    /// Delegate a prompt to a new tab agent by spawning `wta delegate` subprocess.
    /// This is the same path used by the command palette — single code path for
    /// context capture, prompt building, and tab creation.
    pub fn delegate_to_tab_agent(&self, prompt: &str) {
        tracing::info!(target: "autofix", prompt_len = prompt.len(), "delegate_to_tab_agent called");
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(_) => return,
        };
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("delegate").arg(prompt);
        // The delegate child inherits WT_COM_CLSID from our env; no explicit pass needed.

        // Fire-and-forget: spawn hidden, don't wait.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        let _ = cmd.spawn();
    }

    /// Auto-fix: when a command fails in another pane, ask the coordinator
    /// agent to suggest a fix. The user confirms before execution.
    fn maybe_trigger_autofix(&mut self, notification: &WtNotification) {
        if !self.autofix_enabled {
            return;
        }
        if self.state != ConnectionState::Connected {
            return;
        }

        // Suppress autofix when the failing/exiting pane is an agent CLI
        // session (Claude/Copilot/Gemini). An agent CLI exiting (whether via
        // `/exit`, Ctrl+C, or the user typing `exit` after a resume) is
        // intentional teardown, not a fixable command failure. Without this
        // guard, the autofix prompt path issues a `wt_read_last_prompt` /
        // `wt_read_pane_output` against the just-closed pane GUID, which
        // makes WT's `TerminalProtocolComServer::ReadPaneOutput` throw
        // `E_FAIL` (pane not found). The error is swallowed by the Rust
        // RPC layer but surfaces as a noisy first-chance exception in the
        // C++ debugger.
        //
        // `is_agent_pane()` was added for exactly this purpose (see its
        // doc comment) but was previously only consulted by the C++
        // get_active_pane handler. Wire it into the Rust trigger path too.
        if self.agent_sessions.is_agent_pane(&notification.pane_id) {
            tracing::info!(
                target: "autofix",
                pane_id = %notification.pane_id,
                "suppressed: pane is bound to an agent CLI session",
            );
            return;
        }

        // Latest event always wins — but only if we can actually act on it.
        // The ACP transport single-flights at the tab level, so if the
        // current tab already has a prompt in flight, submitting another
        // one results in `App.turn = Submitted(new)` + ACP `AgentBusy`
        // rejection — the buffer and the wire diverge, and old chunks
        // corrupt the new turn's state. Defer instead.
        let same_pane = self.autofix_pane_id.as_deref() == Some(notification.pane_id.as_str());
        let already_busy = !self.current_tab().turn.is_idle()
            && !matches!(
                self.current_tab().turn,
                TurnState::Surfaced { end_pending: false, .. }
            );

        if already_busy {
            if same_pane {
                // Same pane re-trigger: refresh the bar's summary text but
                // don't re-submit — the agent is already working on it.
                tracing::info!(
                    target: "autofix",
                    pane_id = %notification.pane_id,
                    "autofix re-trigger same pane while pending — re-emit only",
                );
                self.emit_autofix_state_pending(
                    &notification.pane_id,
                    &notification.summary,
                );
            } else {
                // Different pane while busy: drop. The user can Esc the
                // current autofix to free the slot if they want this one.
                tracing::info!(
                    target: "autofix",
                    pane_id = %notification.pane_id,
                    armed_pane = ?self.autofix_pane_id,
                    "skipping autofix: previous turn still in-flight",
                );
            }
            return;
        }

        // For all other cases (different pane, or Armed state, or Idle):
        // bump generation to stale any in-flight response, then submit a new
        // autofix turn via the state machine.
        self.autofix_generation = self.autofix_generation.wrapping_add(1);
        let new_gen = self.autofix_generation;
        // A new analysis supersedes any leftover suggestion. The C++ side
        // will swap to Pending on the new pending event below; emitting an
        // explicit cleared first would create a flicker.
        self.suggested_pane_id = None;

        // The auto-fix kind is carried by PromptSubmission::is_autofix,
        // so the text doesn't need a marker prefix — just the raw error
        // summary + instruction.
        let prompt_text = format!(
            "{}\nDiagnose the error and suggest a fix.",
            notification.summary
        );

        // Use the failing pane as the source so the agent reads its buffer.
        let pane_context = PaneContext {
            pane_id: self.pane_id.clone(),
            tab_id: self.tab_id.clone(),
            window_id: self.window_id.clone(),
            cwd: None,
            source_pane_id: Some(notification.pane_id.clone()),
        };

        // Store the failing pane ID so the Esc dismiss path can find it
        // (legacy; the new state machine carries it via AutofixContext).
        self.autofix_pane_id = Some(notification.pane_id.clone());

        let prompt = PromptSubmission::new_autofix(prompt_text, Some(pane_context));
        let submitted = SubmittedPrompt {
            id: prompt.id,
            text: prompt.text.clone(),
            submitted_at_unix_s: prompt.submitted_at_unix_s,
            autofix: Some(AutofixContext {
                target_pane_id: notification.pane_id.clone(),
                generation: new_gen,
            }),
        };
        // Route through the state machine. If no ACP session is bound yet
        // (tests / pre-AgentConnected), `turn_submit_prompt` still installs
        // the turn on the default tab so the prompt is queued correctly.
        let session_id = self
            .current_tab()
            .session_id
            .clone()
            .unwrap_or_else(|| DEFAULT_TAB_ID.to_string());
        self.turn_submit_prompt(&session_id, submitted);
        tracing::info!(target: "autofix", pane_id = %notification.pane_id, generation = new_gen, "sending auto-fix prompt");
        let _ = self.prompt_tx.send(prompt);

        // Light up the bottom-bar diagnostic icon in "Pending" state — the
        // user knows something went wrong even before the agent responds.
        self.emit_autofix_state_pending(&notification.pane_id, &notification.summary);
    }

    // ── autofix_state signalling ───────────────────────────────────────────
    //
    // Notifies the TerminalPage about autofix progress via a JSON event on
    // the SendEvent bus. The COM server special-cases method=="autofix_state"
    // and dispatches to TerminalPage.OnAutofixStateChanged (UI thread).

    fn emit_autofix_state_pending(&self, pane_id: &str, summary: &str) {
        let evt = serde_json::json!({
            "type": "event",
            "method": "autofix_state",
            "params": {
                "state": "pending",
                "session_id": pane_id,
                "summary": summary,
            }
        });
        send_wt_protocol_event(evt.to_string());
    }

    fn emit_autofix_state_armed(&self, pane_id: &str, fix_preview: &str) {
        let evt = serde_json::json!({
            "type": "event",
            "method": "autofix_state",
            "params": {
                "state": "armed",
                "session_id": pane_id,
                "fix_preview": fix_preview,
                "hotkey_hint": "Ctrl+Alt+.",
            }
        });
        send_wt_protocol_event(evt.to_string());
    }

    /// Execute the currently armed autofix on behalf of the user (they
    /// clicked the bottom-bar button or pressed Ctrl+. in the terminal
    /// window). Mirrors the Enter-key path in the recommendations handler
    /// but without requiring the agent pane to be focused.
    fn handle_autofix_execute_request(&mut self, requested_pane_id: &str) {
        tracing::info!(target: "autofix", requested_pane = %requested_pane_id, armed_pane = ?self.autofix_pane_id, has_recs = self.current_tab().turn.recommendations().is_some(), "autofix_execute received");
        // Only execute if we have a cached autofix for the requested pane.
        // The pane_id check prevents a stale UI click from running against
        // an unrelated, more recent error.
        let armed_pane = match self.autofix_pane_id.clone() {
            Some(p) if p == requested_pane_id => p,
            _ => {
                tracing::info!(target: "autofix", "autofix_execute: no armed fix for this pane");
                // Tell the UI anyway so it returns to Idle.
                self.emit_autofix_state_cleared(requested_pane_id);
                return;
            }
        };
        let rec = match self.current_tab().turn.recommendations().cloned() {
            Some(r) => r,
            None => {
                self.emit_autofix_state_cleared(&armed_pane);
                self.autofix_pane_id = None;
                return;
            }
        };
        let idx = rec
            .recommended_choice
            .unwrap_or(self.current_tab_mut().selected_recommendation)
            .min(rec.choices.len().saturating_sub(1));
        let Some(mut choice) = rec.choices.get(idx).cloned() else {
            self.emit_autofix_state_cleared(&armed_pane);
            self.autofix_pane_id = None;
            return;
        };
        // Auto-fill parent for Send actions, same as Enter path.
        for action in &mut choice.actions {
            if let crate::coordinator::RecommendedAction::Send { ref mut parent, .. } = action {
                if parent.is_empty() {
                    *parent = armed_pane.clone();
                }
            }
        }
        // Drive the cutover state machine: if the current tab's turn is
        // still in `Surfaced{Recommendation,..}`, route through
        // `turn_execute_card`; otherwise fall back to the lightweight
        // dispatch path (the user may have already cleared the card via
        // some other input).
        let session_id = self.current_tab().session_id.clone();
        let routed = if let Some(sid) = session_id {
            if matches!(
                self.current_tab().turn,
                TurnState::Surfaced { outcome: TurnOutcome::Recommendation(_), .. }
            ) {
                self.turn_execute_card(&sid);
                true
            } else {
                false
            }
        } else {
            false
        };
        let choice_label = choice.choice;
        if !routed {
            self.autofix_pane_id = None;
            self.clear_recommendations();
            let _ = self
                .recommendation_tx
                .send(crate::coordinator::ChoiceExecution {
                    choice,
                    insert_only: false,
                });
        }
        self.push_execution_info(format!("Auto-executing choice {}.", choice_label));
        self.emit_autofix_state_cleared(&armed_pane);
    }

    fn emit_autofix_state_cleared(&self, pane_id: &str) {
        let evt = serde_json::json!({
            "type": "event",
            "method": "autofix_state",
            "params": {
                "state": "cleared",
                "session_id": pane_id,
            }
        });
        send_wt_protocol_event(evt.to_string());
    }

    /// Ask WT to tear down this agent pane. Wired to the second tap of the
    /// double-Ctrl+C close sequence. WT closes the Pane, which causes its
    /// ConPty to SIGKILL us — so the natural side effect of pane teardown
    /// is that wta exits and the in-process `tab_to_session` map dies with
    /// it. The next time the user toggles the agent pane open, WT spawns a
    /// fresh wta whose map is empty: per-tab ACP sessions get re-bound to
    /// the new wta's keyspace, which is the "clean session" semantics we
    /// want without any explicit per-entry cleanup.
    ///
    /// We do NOT set `should_quit` here. If WT's close path is delayed or
    /// the event is dropped, wta keeps running and the user can try again
    /// (or use the WT-side close-pane keybinding). Self-quitting would
    /// race the close request and produce a "process exited" pane that
    /// the next toggle can't recover from cleanly.
    fn request_close_agent_pane(&self) {
        let mut params = serde_json::Map::new();
        if let Some(ref p) = self.pane_id {
            params.insert("pane_id".to_string(), serde_json::Value::String(p.clone()));
        }
        if let Some(ref t) = self.tab_id {
            params.insert("tab_id".to_string(), serde_json::Value::String(t.clone()));
        }
        let evt = serde_json::json!({
            "type": "event",
            "method": "close_agent_pane",
            "params": serde_json::Value::Object(params),
        });
        tracing::info!(target: "close_pane", "double-Ctrl+C → asking WT to close agent pane");
        send_wt_protocol_event(evt.to_string());
    }

    /// Bottom bar shows "Suggestion ready — open agent pane" (blue/info style).
    /// The full explanation lives in the agent pane chat history; the protocol
    /// event only carries the title used as the bar label.
    fn emit_autofix_state_suggested(&self, pane_id: &str, title: &str) {
        let evt = serde_json::json!({
            "type": "event",
            "method": "autofix_state",
            "params": {
                "state": "suggested",
                "session_id": pane_id,
                "suggestion_title": title,
            }
        });
        send_wt_protocol_event(evt.to_string());
    }

    fn armed_fix_preview(rec: &crate::coordinator::RecommendationSet) -> String {
        armed_fix_preview(rec)
    }

    fn push_execution_info(&mut self, _message: String) {}

    fn selected_recommendation_choice(&self) -> Option<&RecommendationChoice> {
        let tab = self.current_tab();
        tab.turn
            .recommendations()
            .and_then(|recs| recs.choices.get(tab.selected_recommendation))
    }

    /// Returns the number of buttons for the currently selected choice card.
    /// Send actions have 2 buttons (Run, Insert); OpenAndSend has 1 button.
    fn button_count_for_selected(&self) -> usize {
        self.selected_recommendation_choice()
            .map(|c| if self.is_send_choice(c) { 2 } else { 1 })
            .unwrap_or(1)
    }

    /// Default focused button index when landing on a card. Always 0 — the
    /// leftmost button (Run for Send cards, the sole button for OpenAndSend).
    fn default_button_for_selected(&self) -> usize {
        0
    }

    /// Returns true if the choice's primary action is Send (shell command).
    fn is_send_choice(&self, choice: &RecommendationChoice) -> bool {
        choice.actions.iter().any(|a| matches!(a, crate::coordinator::RecommendedAction::Send { .. }))
    }

    fn log_selection_phase_for(&self, session_id: &str, phase: &str, details: &str) {
        // log against the in-flight tab so traces stay coherent with where
        // the prompt was submitted, even after the user switches tabs.
        let tab = self.session_tab(session_id);
        if let Some(prompt) = tab.turn.prompt() {
            prompt_timing_log(prompt.id, prompt.submitted_at_unix_s, phase, details);
        }
    }

    fn log_selection_visible_if_needed(&mut self) {
        let tab = self.current_tab();
        if !tab.selection_visible_pending || tab.turn.recommendations().is_none() {
            return;
        }
        let details = format!(
            "choice_count={} selected_index={}",
            tab.turn
                .recommendations()
                .map(|set| set.choices.len())
                .unwrap_or(0),
            tab.selected_recommendation
        );
        let session_id = tab.session_id.clone();
        if let Some(sid) = session_id {
            self.log_selection_phase_for(&sid, "selection_visible", &details);
        }
        self.current_tab_mut().selection_visible_pending = false;
    }
}

// ─────────────────────────────────────────────────────────────────────────
// TurnState transition methods
//
// Source of truth for the per-turn lifecycle (see
// `doc/specs/turn-state-refactor.md`). Every event handler — chunk arrival,
// end-of-turn, Enter on a card, Esc / Ctrl+C cancel, autofix trigger — goes
// through one of these methods.
// ─────────────────────────────────────────────────────────────────────────

impl App {
    /// Transition `tab.turn` into `Submitted` for a new prompt and perform
    /// the side effects: clear stale in-flight chat state (messages, tool
    /// calls, permission, scroll), push the user bubble, log
    /// `prompt_received`. Caller is responsible for actually dispatching the
    /// prompt over ACP (so this method stays free of async / channel
    /// concerns).
    pub fn turn_submit_prompt(&mut self, session_id: &str, prompt: SubmittedPrompt) {
        prompt_timing_log(
            prompt.id,
            prompt.submitted_at_unix_s,
            "prompt_received",
            &format!(
                "autofix={} text_chars={}",
                prompt.autofix.is_some(),
                prompt.text.chars().count()
            ),
        );
        let is_autofix = prompt.autofix.is_some();
        let user_text = prompt.text.clone();
        let tab = self.session_tab_mut(session_id);
        // Per Decision #3, every Idle→Submitted transition explicitly clears
        // these orthogonal fields rather than relying on side effects from a
        // grab-bag helper.
        tab.messages.clear();
        tab.tool_calls.clear();
        tab.permission = None;
        tab.chat_scroll.reset();
        tab.selection_visible_pending = false;
        // Any leftover card from the previous turn's
        // `Surfaced{end_pending:false}` is dismissed by the new submit.
        tab.selected_recommendation = 0;
        tab.selected_button = 0;
        tab.rec_scroll.reset();
        // Autofix prompts are synthesized by the system; they don't render
        // as a User bubble (the user already sees the error line in the
        // failing pane).
        if !is_autofix {
            tab.messages.push(ChatMessage::User(user_text));
        }
        tab.scroll_to_bottom();
        tab.progress_status = None;
        tab.activity_frame = 0;
        tab.timing_note = None;
        tab.turn = TurnState::Submitted(prompt);
    }

    /// Observe a streamed chunk. Thought chunks only advance the state
    /// (Submitted→Streaming with empty buffer); message chunks append to the
    /// streaming buffer. Returns true if the buffer changed (so the caller
    /// can decide whether to attempt an eager surface).
    pub fn turn_observe_chunk(&mut self, session_id: &str, kind: ChunkKind, text: &str) -> bool {
        // Stale-autofix check: if the chunk belongs to an autofix turn whose
        // generation no longer matches the current counter, drop it.
        let current_gen = self.autofix_generation;
        let tab = self.session_tab_mut(session_id);
        if let Some(gen) = tab.turn.autofix_generation() {
            if gen != current_gen {
                tracing::debug!(
                    target: "autofix",
                    inflight_gen = gen,
                    current_gen,
                    "dropping stale autofix chunk",
                );
                return false;
            }
        }

        // `progress_status` (agent-supplied "Reading foo.rs" etc.) is left
        // alone here — its natural lifetime is the whole turn. It's cleared
        // at turn close (`turn_clear_agent_progress`) and overwritten by
        // future `ProgressStatus` events. The old per-chunk wipe erased
        // the value the moment a streaming agent would have it set.
        match (&mut tab.turn, kind) {
            // First message chunk: transition Submitted → Streaming.
            (TurnState::Submitted(_), ChunkKind::Message) => {
                let TurnState::Submitted(prompt) =
                    std::mem::replace(&mut tab.turn, TurnState::Idle)
                else {
                    unreachable!();
                };
                tab.turn = TurnState::Streaming {
                    prompt,
                    buf: text.to_string(),
                };
                true
            }
            // Thought chunk while Submitted: enter Streaming with empty buf.
            (TurnState::Submitted(_), ChunkKind::Thought) => {
                let TurnState::Submitted(prompt) =
                    std::mem::replace(&mut tab.turn, TurnState::Idle)
                else {
                    unreachable!();
                };
                tab.turn = TurnState::Streaming {
                    prompt,
                    buf: String::new(),
                };
                false
            }
            // Streaming → Streaming, append message chunks only.
            (TurnState::Streaming { buf, .. }, ChunkKind::Message) => {
                buf.push_str(text);
                true
            }
            // Thought chunks during Streaming: no buffer change.
            (TurnState::Streaming { .. }, ChunkKind::Thought) => false,
            // Trailing chunks after the card has surfaced: drop them.
            (TurnState::Surfaced { .. }, _) => false,
            // Chunks while Idle: shouldn't happen; defensive drop.
            (TurnState::Idle, _) => false,
        }
    }

    /// Attempt to parse the streaming buffer and surface a card / chat turn
    /// without waiting for `AgentMessageEnd`. No-op if state isn't
    /// `Streaming`, buffer hasn't opened a fence yet, or parsing fails.
    pub fn turn_try_eager_surface(&mut self, session_id: &str) {
        let tab = self.session_tab(session_id);
        let TurnState::Streaming { buf, .. } = &tab.turn else {
            return;
        };
        if !buf.contains("```") {
            return;
        }
        let buf = buf.clone();
        let is_autofix = tab.turn.is_autofix();

        if is_autofix {
            match parse_autofix_response(&buf) {
                AutofixDecision::Fix(recommendations) => {
                    self.turn_surface_fix(session_id, recommendations, "autofix_fix_eager");
                }
                AutofixDecision::Explain { title, explanation } => {
                    self.turn_surface_explain(
                        session_id,
                        title,
                        explanation,
                        "autofix_explain_eager",
                    );
                }
                AutofixDecision::Ignore => {}
            }
        } else {
            let parsed = parse_recommendation_set(&buf).and_then(|r| {
                validate_recommendation_set_for_coordinator_target(&r, self.pane_id.as_deref())
            });
            if let Ok(recommendations) = parsed {
                self.turn_surface_recommendation(
                    session_id,
                    recommendations,
                    "selection_ready_eager",
                );
            }
        }
    }

    /// Close the in-flight turn on `AgentMessageEnd`. Dispatches across
    /// four termination paths:
    ///
    /// 1. Stale-autofix discard (newer trigger or Esc cancelled this turn).
    /// 2. Eager surface already fired — just release the UI gate.
    /// 3. `Submitted` with no chunks — model returned nothing.
    /// 4. `Streaming` with a buffer — final parse via the autofix or
    ///    planner finalize helper.
    pub fn turn_close(&mut self, session_id: &str) {
        // (1) Stale-autofix discard.
        let current_gen = self.autofix_generation;
        if let Some(gen) = self.session_tab(session_id).turn.autofix_generation() {
            if gen != current_gen {
                tracing::info!(
                    target: "autofix",
                    inflight_gen = gen,
                    current_gen,
                    "discarding stale autofix turn at close",
                );
                self.turn_clear_agent_progress(session_id);
                self.session_tab_mut(session_id).turn = TurnState::Idle;
                return;
            }
        }

        // (2) Eager surface already fired.
        if let TurnState::Surfaced {
            end_pending: true, ..
        } = &self.session_tab(session_id).turn
        {
            self.turn_release_end_pending_logged(session_id, "via=eager+end");
            self.turn_clear_agent_progress(session_id);
            return;
        }

        // (3) Submitted, no chunks. For autofix this would leave the bar
        //     stuck in Pending; clear it explicitly.
        let (buf, is_autofix) = match &self.session_tab(session_id).turn {
            TurnState::Streaming { buf, prompt } => (buf.clone(), prompt.autofix.is_some()),
            TurnState::Submitted(_) => {
                self.turn_close_no_chunks(session_id);
                return;
            }
            // Idle / already-surfaced+end_done — nothing to do.
            _ => return,
        };

        // (4) Final parse on the streaming buffer.
        if is_autofix {
            self.turn_close_finalize_autofix(session_id, &buf);
        } else {
            self.turn_close_finalize_planner(session_id, buf);
        }
        self.turn_clear_agent_progress(session_id);
    }

    /// Path (3): close a turn that received `AgentMessageEnd` with no
    /// streamed content. Emits `autofix_state_cleared` if it was an
    /// autofix turn so the bottom bar doesn't stick in Pending.
    fn turn_close_no_chunks(&mut self, session_id: &str) {
        let tab = self.session_tab_mut(session_id);
        let prompt = tab.turn.prompt().cloned().expect("prompt set");
        let autofix_pane = prompt.autofix.as_ref().map(|a| a.target_pane_id.clone());
        tab.turn = TurnState::Surfaced {
            prompt,
            outcome: TurnOutcome::Empty,
            end_pending: true,
        };
        if let Some(pane) = autofix_pane {
            self.emit_autofix_state_cleared(&pane);
            self.autofix_pane_id = None;
        }
        self.turn_release_end_pending(session_id);
        self.turn_clear_agent_progress(session_id);
    }

    /// Path (4a): autofix Streaming buffer reached `AgentMessageEnd` with
    /// no eager surface. Parse and route to Fix / Explain / Ignore.
    fn turn_close_finalize_autofix(&mut self, session_id: &str, buf: &str) {
        match parse_autofix_response(buf) {
            AutofixDecision::Fix(recommendations) => {
                self.turn_surface_fix(session_id, recommendations, "autofix_fix");
                self.turn_release_end_pending(session_id);
            }
            AutofixDecision::Explain { title, explanation } => {
                self.turn_surface_explain(session_id, title, explanation, "autofix_explain");
                self.turn_release_end_pending(session_id);
            }
            AutofixDecision::Ignore => {
                let pane_id = self.autofix_pane_id.clone();
                self.log_selection_phase_for(
                    session_id,
                    "autofix_ignore",
                    &format!("pane={:?}", pane_id),
                );
                if let Some(pane_id) = pane_id {
                    self.emit_autofix_state_cleared(&pane_id);
                }
                self.autofix_pane_id = None;
                let tab = self.session_tab_mut(session_id);
                let prompt = tab.turn.prompt().cloned().expect("prompt set");
                tab.turn = TurnState::Surfaced {
                    prompt,
                    outcome: TurnOutcome::Empty,
                    end_pending: false,
                };
            }
        }
    }

    /// Path (4b): non-autofix Streaming buffer. Try `RecommendationSet`
    /// parse first; on failure, commit as a chat turn (chat-mode answer).
    fn turn_close_finalize_planner(&mut self, session_id: &str, buf: String) {
        let parsed = parse_recommendation_set(&buf).and_then(|r| {
            validate_recommendation_set_for_coordinator_target(&r, self.pane_id.as_deref())
        });
        match parsed {
            Ok(recommendations) => {
                self.turn_surface_recommendation(session_id, recommendations, "selection_ready");
                self.turn_release_end_pending(session_id);
            }
            Err(err) => {
                let chars = buf.chars().count();
                let error_text = format!("{:#}", err).replace('\n', " | ");
                self.log_selection_phase_for(
                    session_id,
                    "selection_parse_failed",
                    &format!("response_chars={} error={:?}", chars, error_text),
                );
                let tab = self.session_tab_mut(session_id);
                let prompt = tab.turn.prompt().cloned().expect("prompt set");
                let mut details = tab.current_turn_details();
                details.push(ChatMessage::Agent(buf));
                tab.completed_turns.push(CompletedTurn {
                    prompt: prompt.text.clone(),
                    details,
                    expanded: true,
                });
                tab.messages.clear();
                tab.tool_calls.clear();
                tab.scroll_to_bottom();
                // Route through `turn_release_end_pending` so
                // `prompt_complete` fires on this terminal path too.
                tab.turn = TurnState::Surfaced {
                    prompt,
                    outcome: TurnOutcome::ChatTurn,
                    end_pending: true,
                };
                self.turn_release_end_pending(session_id);
            }
        }
    }

    /// Variant of `turn_release_end_pending` with a custom `via=` log tag
    /// for the eager-surface path. `turn_release_end_pending` uses
    /// `via=end_only`; `via=eager+end` lets `prompt_timing` consumers
    /// distinguish.
    fn turn_release_end_pending_logged(&mut self, session_id: &str, via: &str) {
        let tab = self.session_tab_mut(session_id);
        if let TurnState::Surfaced {
            end_pending,
            prompt,
            ..
        } = &mut tab.turn
        {
            if *end_pending {
                *end_pending = false;
                let prompt_id = prompt.id;
                let submitted_at = prompt.submitted_at_unix_s;
                prompt_timing_log(prompt_id, submitted_at, "prompt_complete", via);
            }
        }
    }

    /// Helper called at every turn-close path. Clears the agent-supplied
    /// progress override and the shimmer animation phase; the UI spinner
    /// otherwise drives off `tab.turn.spinner_label()`.
    fn turn_clear_agent_progress(&mut self, session_id: &str) {
        let tab = self.session_tab_mut(session_id);
        tab.progress_status = None;
        tab.activity_frame = 0;
    }

    /// User pressed Enter while a card was visible — dispatch the selected
    /// choice to the coordinator and transition to `Surfaced { Empty, .. }`
    /// while preserving the ACP single-flight gate.
    pub fn turn_execute_card(&mut self, session_id: &str) {
        let Some(mut choice) = self.selected_recommendation_choice().cloned() else {
            return;
        };
        let tab = self.session_tab(session_id);
        let TurnState::Surfaced {
            outcome: TurnOutcome::Recommendation(_),
            ..
        } = &tab.turn
        else {
            return;
        };
        let insert_only =
            self.session_tab(session_id).selected_button == 1 && self.is_send_choice(&choice);
        // Autofill parent for Send actions when this is an autofix turn.
        if let Some(pane_id) = self
            .session_tab(session_id)
            .turn
            .prompt()
            .and_then(|p| p.autofix.as_ref())
            .map(|a| a.target_pane_id.clone())
        {
            for action in &mut choice.actions {
                if let crate::coordinator::RecommendedAction::Send { ref mut parent, .. } = action {
                    if parent.is_empty() {
                        *parent = pane_id.clone();
                    }
                }
            }
        }
        let armed_pane = self
            .session_tab(session_id)
            .turn
            .prompt()
            .and_then(|p| p.autofix.as_ref())
            .map(|a| a.target_pane_id.clone());
        let _ = self
            .recommendation_tx
            .send(crate::coordinator::ChoiceExecution { choice, insert_only });
        if let Some(pane_id) = armed_pane {
            self.emit_autofix_state_cleared(&pane_id);
        }
        self.autofix_pane_id = None;
        let tab = self.session_tab_mut(session_id);
        let TurnState::Surfaced { prompt, end_pending, .. } =
            std::mem::replace(&mut tab.turn, TurnState::Idle)
        else {
            unreachable!()
        };
        tab.selected_recommendation = 0;
        tab.selected_button = 0;
        tab.rec_scroll.reset();
        // commit pending turn (in case eager surface staged one).
        tab.turn = TurnState::Surfaced {
            prompt,
            outcome: TurnOutcome::Empty,
            end_pending,
        };
    }

    /// User pressed Esc — cancel the in-flight turn. Bumps
    /// `autofix_generation` so any chunks that arrive after this point are
    /// dropped by the stale-check in `turn_observe_chunk`.
    pub fn turn_cancel(&mut self, session_id: &str) {
        self.autofix_generation = self.autofix_generation.wrapping_add(1);
        let pane_id = self
            .session_tab(session_id)
            .turn
            .prompt()
            .and_then(|p| p.autofix.as_ref())
            .map(|a| a.target_pane_id.clone())
            .or_else(|| self.autofix_pane_id.clone());
        if let Some(pane_id) = pane_id {
            self.emit_autofix_state_cleared(&pane_id);
        }
        self.autofix_pane_id = None;
        let tab = self.session_tab_mut(session_id);
        tab.selected_recommendation = 0;
        tab.selected_button = 0;
        tab.rec_scroll.reset();
        tab.progress_status = None;
        tab.activity_frame = 0;
        tab.turn = TurnState::Idle;
    }

    // ── Internal surface helpers (shared between eager and end-of-turn). ──

    /// Surface a planner-mode recommendation card.
    fn turn_surface_recommendation(
        &mut self,
        session_id: &str,
        recommendations: RecommendationSet,
        phase_name: &str,
    ) {
        let rec_idx = recommended_choice_index(&recommendations);
        let choice_count = recommendations.choices.len();
        let recommended_choice = recommendations.recommended_choice;
        let summary = format_recommendations_for_chat(&recommendations);
        self.log_selection_phase_for(
            session_id,
            phase_name,
            &format!(
                "choice_count={} recommended_choice={:?}",
                choice_count, recommended_choice
            ),
        );
        let tab = self.session_tab_mut(session_id);
        let prompt = tab.turn.prompt().cloned().expect("prompt set");
        let mut details = tab.current_turn_details();
        details.push(ChatMessage::Agent(summary));
        tab.completed_turns.push(CompletedTurn {
            prompt: prompt.text.clone(),
            details,
            expanded: false,
        });
        tab.messages.clear();
        tab.tool_calls.clear();
        tab.scroll_to_bottom();
        tab.selected_recommendation = rec_idx;
        tab.selected_button = 0;
        tab.rec_scroll.reset();
        tab.selection_visible_pending = true;
        tab.selected_completed_turn_idx = None;
        tab.progress_status = None;
        tab.activity_frame = 0;
        tab.turn = TurnState::Surfaced {
            prompt,
            outcome: TurnOutcome::Recommendation(recommendations),
            end_pending: true,
        };
    }

    /// Surface an autofix Fix recommendation as an Armed card.
    fn turn_surface_fix(
        &mut self,
        session_id: &str,
        recommendations: RecommendationSet,
        phase_name: &str,
    ) {
        let pane_id = self
            .session_tab(session_id)
            .turn
            .prompt()
            .and_then(|p| p.autofix.as_ref())
            .map(|a| a.target_pane_id.clone());
        let Some(pane_id) = pane_id else {
            return;
        };
        self.log_selection_phase_for(
            session_id,
            phase_name,
            &format!(
                "pane={pane_id} title={:?}",
                recommendations.choices.first().map(|c| &c.title)
            ),
        );
        let preview = Self::armed_fix_preview(&recommendations);
        self.emit_autofix_state_armed(&pane_id, &preview);
        let rec_idx = recommended_choice_index(&recommendations);
        let tab = self.session_tab_mut(session_id);
        let prompt = tab.turn.prompt().cloned().expect("prompt set");
        tab.selected_recommendation = rec_idx;
        tab.selection_visible_pending = true;
        tab.progress_status = None;
        tab.activity_frame = 0;
        tab.turn = TurnState::Surfaced {
            prompt,
            outcome: TurnOutcome::Recommendation(recommendations),
            end_pending: true,
        };
    }

    /// Surface an autofix Explain answer as a chat turn + bottom-bar
    /// Suggested indicator.
    fn turn_surface_explain(
        &mut self,
        session_id: &str,
        title: String,
        explanation: String,
        phase_name: &str,
    ) {
        let pane_id = self
            .session_tab(session_id)
            .turn
            .prompt()
            .and_then(|p| p.autofix.as_ref())
            .map(|a| a.target_pane_id.clone());
        let Some(pane_id) = pane_id else {
            return;
        };
        self.log_selection_phase_for(
            session_id,
            phase_name,
            &format!(
                "pane={pane_id} title={title:?} chars={}",
                explanation.chars().count()
            ),
        );

        let turn_prompt_label = format!("Auto-diagnosed error in pane {pane_id}");
        {
            let tab = self.session_tab_mut(session_id);
            let mut details = tab.current_turn_details();
            details.push(ChatMessage::Agent(explanation));
            tab.completed_turns.push(CompletedTurn {
                prompt: turn_prompt_label,
                details,
                expanded: false,
            });
            tab.messages.clear();
            tab.tool_calls.clear();
            tab.scroll_to_bottom();
        }

        self.emit_autofix_state_suggested(&pane_id, &title);
        self.suggested_pane_id = Some(pane_id.clone());
        self.autofix_pane_id = None;

        let tab = self.session_tab_mut(session_id);
        let prompt = tab.turn.prompt().cloned().expect("prompt set");
        tab.selected_recommendation = 0;
        tab.selected_button = 0;
        tab.rec_scroll.reset();
        tab.progress_status = None;
        tab.activity_frame = 0;
        tab.turn = TurnState::Surfaced {
            prompt,
            outcome: TurnOutcome::ChatTurn,
            end_pending: true,
        };
    }

    /// Flip `end_pending=false` after a final-path surface. Mirrors the
    /// `prompt_complete` log used by the eager path.
    fn turn_release_end_pending(&mut self, session_id: &str) {
        let tab = self.session_tab_mut(session_id);
        if let TurnState::Surfaced {
            end_pending,
            prompt,
            ..
        } = &mut tab.turn
        {
            if *end_pending {
                *end_pending = false;
                let prompt_id = prompt.id;
                let submitted_at = prompt.submitted_at_unix_s;
                prompt_timing_log(prompt_id, submitted_at, "prompt_complete", "via=end_only");
            }
        }
    }
}

/// Computes the rendered height (in terminal rows) of a recommendation card.
/// Includes one trailing row used as the inter-card gap in the rec panel.
pub(crate) fn rec_card_height(choice: &RecommendationChoice, panel_width: u16) -> usize {
    use crate::coordinator::RecommendedAction;
    let inner_width = ui::card::card_content_width(panel_width);

    let text = choice.actions.iter().find_map(|action| match action {
        RecommendedAction::Send { input, .. } => Some(input.clone()),
        RecommendedAction::OpenAndSend { agent, input, .. } => {
            let label = agent.as_deref().unwrap_or("agent");
            Some(format!("{}: {}", label, input))
        }
        RecommendedAction::Open { target, cwd, title, .. } => {
            use crate::coordinator::OpenTarget;
            let kind = match target {
                OpenTarget::Tab => "tab",
                OpenTarget::Panel => "panel",
            };
            Some(match (title.as_deref(), cwd.as_deref()) {
                (Some(t), Some(c)) if !t.is_empty() && !c.is_empty() => {
                    format!("New {} ({}) in {}", kind, t, c)
                }
                (Some(t), _) if !t.is_empty() => format!("New {} ({})", kind, t),
                (_, Some(c)) if !c.is_empty() => format!("New {} in {}", kind, c),
                _ => format!("New {} (empty)", kind),
            })
        }
    }).unwrap_or_else(|| choice.title.clone());

    let content_lines: usize = text.lines()
        .map(|line| {
            let chars = line.chars().count();
            if chars == 0 { 1 } else { chars.div_ceil(inner_width) }
        })
        .sum::<usize>()
        .max(1);

    // CARD_MIN_SIZE counts 1 content row; add the wrap-extra rows + 1 gap.
    ui::card::CARD_MIN_SIZE as usize + content_lines.saturating_sub(1) + 1
}

/// Computes the rendered height (in terminal rows) of the embedded
/// permission card. No inter-card gap — only one card is ever shown.
pub(crate) fn permission_card_height(perm: &PermissionState, panel_width: u16) -> usize {
    let inner_width = ui::card::card_content_width(panel_width);
    let content_lines: usize = perm
        .description
        .lines()
        .map(|line| {
            let chars = line.chars().count();
            if chars == 0 { 1 } else { chars.div_ceil(inner_width) }
        })
        .sum::<usize>()
        .max(1);
    // CARD_MIN_SIZE counts 1 content row; add the wrap-extra rows.
    ui::card::CARD_MIN_SIZE as usize + content_lines.saturating_sub(1)
}

/// Render a parsed `RecommendationSet` as the agent's "reply" text in chat.
///
/// Recommendation responses arrive as JSON; storing the raw JSON in a completed
/// turn means re-expanding the prompt header reveals raw JSON instead of a
/// CLI-style answer. This builds a single line per choice that mirrors what the
/// recommendation cards show, prefixed with `✓` for the recommended one.
fn format_recommendations_for_chat(set: &RecommendationSet) -> String {
    use crate::coordinator::{OpenTarget, RecommendedAction};

    let header = if set.choices.len() == 1 {
        "Suggested 1 option:".to_string()
    } else {
        format!("Suggested {} options:", set.choices.len())
    };
    let mut out = header;

    for choice in &set.choices {
        let action_text = choice
            .actions
            .iter()
            .find_map(|action| match action {
                RecommendedAction::Send { input, .. } => Some(format!("Run: {}", input)),
                RecommendedAction::OpenAndSend {
                    target, input, agent, ..
                } => {
                    let where_ = match target {
                        OpenTarget::Tab => "new tab",
                        OpenTarget::Panel => "new panel",
                    };
                    let label = agent.as_deref().unwrap_or("agent");
                    Some(format!("Open {} and run {}: {}", where_, label, input))
                }
                RecommendedAction::Open { target, cwd, title, .. } => {
                    let kind = match target {
                        OpenTarget::Tab => "tab",
                        OpenTarget::Panel => "panel",
                    };
                    Some(match (title.as_deref(), cwd.as_deref()) {
                        (Some(t), Some(c)) if !t.is_empty() && !c.is_empty() => {
                            format!("Open new {} ({}) in {}", kind, t, c)
                        }
                        (Some(t), _) if !t.is_empty() => format!("Open new {} ({})", kind, t),
                        (_, Some(c)) if !c.is_empty() => format!("Open new {} in {}", kind, c),
                        _ => format!("Open new empty {}", kind),
                    })
                }
            })
            .unwrap_or_else(|| choice.title.clone());

        let marker = if set.recommended_choice == Some(choice.choice) {
            "✓"
        } else {
            " "
        };
        out.push('\n');
        out.push_str(&format!("  {} {}. {}", marker, choice.choice, action_text));
    }

    out
}

/// Extract a short preview string from the recommended choice's first
/// Send action, for display in the bottom-bar tooltip on Armed state.
pub fn armed_fix_preview(rec: &crate::coordinator::RecommendationSet) -> String {
    let idx = rec
        .recommended_choice
        .unwrap_or(0)
        .min(rec.choices.len().saturating_sub(1));
    let Some(choice) = rec.choices.get(idx).or_else(|| rec.choices.first()) else {
        return String::new();
    };
    for action in &choice.actions {
        use crate::coordinator::RecommendedAction;
        match action {
            RecommendedAction::Send { input, .. } => {
                let cleaned = input.trim().replace(['\r', '\n'], " ");
                return truncate(&cleaned, 80);
            }
            RecommendedAction::OpenAndSend { input, .. } => {
                let cleaned = input.trim().replace(['\r', '\n'], " ");
                return truncate(&cleaned, 80);
            }
            RecommendedAction::Open { .. } => {
                return truncate(&choice.title, 80);
            }
        }
    }
    truncate(&choice.title, 80)
}

impl App {
    /// Push the current agent status (name / version / model / connection state)
    /// to the host so a XAML-rendered agent bar can update itself. The COM
    /// server special-cases `method == "agent_status"` and dispatches it
    /// straight to TerminalPage, parallel to the existing `autofix_state`
    /// path. Cheap to call on every state change — the publisher serializes
    /// `wtcli publish` invocations, and an extra one per state transition is
    /// negligible compared to chat traffic.
    fn publish_agent_status(&mut self) {
        let state_str = match &self.state {
            ConnectionState::Connecting(_) => "connecting",
            ConnectionState::Connected => "connected",
            ConnectionState::Failed(_) => "failed",
            ConnectionState::Disconnected => "disconnected",
        };
        // Include selected_agent only once — when connected after user selection.
        // This avoids triggering _RebuildAgentStack mid-FRE.
        let selected = if self.state == ConnectionState::Connected {
            self.pending_agent_selection.take()
        } else {
            None
        };
        let mut params = serde_json::json!({
            "name": self.agent_name,
            "version": self.agent_version,
            "model": self.agent_model,
            "state": state_str,
            "available_models": self.available_models,
            "current_model_id": self.current_model_id,
        });
        if let Some(agent_id) = selected {
            params["selected_agent"] = serde_json::Value::String(agent_id);
        }
        let evt = serde_json::json!({
            "type": "event",
            "method": "agent_status",
            "params": params,
        });
        send_wt_protocol_event(evt.to_string());
    }

    /// Notify the host that the wta-internal view changed. C++ owns the
    /// agent bar's title + the `_agentSessionsViewActive` flag that drives
    /// the bottom bar; without this push the bar would stay on
    /// "Agent sessions" after Esc / out of sync after `/sessions`.
    ///
    /// Only emit from paths where wta is the sole source of truth (Esc,
    /// `/sessions`). The C++-originated `set_view` path already knows
    /// what it asked for and updates its own state directly.
    fn emit_view_changed(&self, view: &str) {
        let evt = serde_json::json!({
            "type": "event",
            "method": "view_changed",
            "params": { "view": view }
        });
        send_wt_protocol_event(evt.to_string());
    }
}

/// Publish a raw JSON event via `wtcli publish`. The event flows through
/// IProtocolServer::SendEvent; our modified COM server special-cases
/// method=="autofix_state" and dispatches directly to TerminalPage.
///
/// Events are funnelled through a single background thread that waits
/// for each `wtcli publish` subprocess to exit before launching the next.
/// Without this, two rapid emits (e.g. armed → cleared) could race at
/// the OS process-scheduling layer and arrive at WT out of order,
/// leaving the bottom-bar stuck in the earlier state.
pub fn send_wt_protocol_event(json_payload: String) {
    let tx = publisher_sender();
    let _ = tx.send(json_payload);
}

fn publisher_sender() -> &'static std::sync::mpsc::Sender<String> {
    static SENDER: std::sync::OnceLock<std::sync::mpsc::Sender<String>> =
        std::sync::OnceLock::new();
    SENDER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::Builder::new()
            .name("wt-event-publisher".into())
            .spawn(move || {
                while let Ok(payload) = rx.recv() {
                    publish_event_blocking(&payload);
                }
            })
            .expect("spawn wt-event-publisher thread");
        tx
    })
}

fn publish_event_blocking(json_payload: &str) {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("wtcli.exe")))
        .filter(|p| p.exists())
        .unwrap_or_else(|| std::path::PathBuf::from("wtcli.exe"));
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("publish").arg(json_payload);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null());
    match cmd.spawn() {
        Ok(mut child) => {
            // Block the publisher thread until this publish finishes so
            // the next event's subprocess can't overtake it.
            let _ = child.wait();
        }
        Err(_) => {},
    }
}

/// Resolve an agent command like "copilot --acp --stdio" to use the full
/// path if the bare executable isn't on PATH (common in packaged apps).
fn resolve_agent_cmd(cmd: &str) -> String {
    let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
    let exe = parts[0];
    let rest = parts.get(1).copied().unwrap_or("");

    // Already a full path?
    if exe.contains('\\') || exe.contains('/') {
        return cmd.to_string();
    }

    // Use agent_check::find_exe which reads fresh PATH from registry
    let profile = crate::agent_registry::lookup_profile(exe);
    if let Some(full_path) = crate::agent_check::find_exe(profile.id) {
        return if rest.is_empty() {
            full_path
        } else {
            format!("{} {}", full_path, rest)
        };
    }

    // Legacy fallback: check known directories
    let search_dirs: Vec<std::path::PathBuf> = [
        std::env::var("LOCALAPPDATA").ok().map(|l| std::path::PathBuf::from(l).join("Microsoft").join("WinGet").join("Links")),
        std::env::var("APPDATA").ok().map(|a| std::path::PathBuf::from(a).join("npm")),
        std::env::var("USERPROFILE").ok().map(|h| std::path::PathBuf::from(h).join(".claude-cli").join("CurrentVersion")),
    ].into_iter().flatten().collect();

    for dir in &search_dirs {
        for ext in &[".exe", ".cmd"] {
            let full = dir.join(format!("{}{}", exe, ext));
            if full.exists() {
                return if rest.is_empty() {
                    full.to_string_lossy().to_string()
                } else {
                    format!("{} {}", full.to_string_lossy(), rest)
                };
            }
        }
    }

    // Fallback: return as-is
    cmd.to_string()
}

/// Read `agentWelcomeShown` from the packaged app's state.json.
fn welcome_shown_in_state() -> bool {
    find_state_json()
        .and_then(|path| std::fs::read_to_string(&path).ok())
        .map(|content| content.contains("\"agentWelcomeShown\" : true") || content.contains("\"agentWelcomeShown\":true"))
        .unwrap_or(false)
}

/// Set `agentWelcomeShown` to true in state.json using string replacement
/// to preserve formatting and other fields.
fn set_welcome_shown_in_state() {
    let Some(path) = find_state_json() else { return };
    let Ok(content) = std::fs::read_to_string(&path) else { return };

    let updated = if content.contains("\"agentWelcomeShown\"") {
        // Replace existing value
        content
            .replace("\"agentWelcomeShown\" : false", "\"agentWelcomeShown\" : true")
            .replace("\"agentWelcomeShown\":false", "\"agentWelcomeShown\" : true")
    } else if let Some(pos) = content.find('{') {
        // Insert after opening brace
        let (before, after) = content.split_at(pos + 1);
        format!("{}\n\t\"agentWelcomeShown\" : true,{}", before, after)
    } else {
        return;
    };
    let _ = std::fs::write(&path, &updated);
}

/// Find the packaged app's state.json.
fn find_state_json() -> Option<std::path::PathBuf> {
    let local_app_data = std::env::var("LOCALAPPDATA").ok()?;
    let packages_dir = std::path::Path::new(&local_app_data).join("Packages");
    if let Ok(entries) = std::fs::read_dir(&packages_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with("IntelligentTerminal_") {
                let candidate = entry.path().join("LocalState").join("state.json");
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { format!("{}…", &s[..max]) }
}


fn now_unix_s() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn clamp_cursor_to_boundary(input: &str, cursor_pos: usize) -> usize {
    let mut clamped = cursor_pos.min(input.len());
    while clamped > 0 && !input.is_char_boundary(clamped) {
        clamped -= 1;
    }
    clamped
}

fn prev_char_boundary(input: &str, cursor_pos: usize) -> usize {
    let cursor_pos = clamp_cursor_to_boundary(input, cursor_pos);
    if cursor_pos == 0 {
        return 0;
    }

    input[..cursor_pos]
        .char_indices()
        .last()
        .map(|(idx, _)| idx)
        .unwrap_or(0)
}

fn next_char_boundary(input: &str, cursor_pos: usize) -> usize {
    let cursor_pos = clamp_cursor_to_boundary(input, cursor_pos);
    if cursor_pos >= input.len() {
        return input.len();
    }

    input[cursor_pos..]
        .chars()
        .next()
        .map(|ch| cursor_pos + ch.len_utf8())
        .unwrap_or(input.len())
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn next_word_boundary(input: &str, cursor_pos: usize) -> usize {
    let cursor_pos = clamp_cursor_to_boundary(input, cursor_pos);
    if cursor_pos >= input.len() {
        return input.len();
    }

    let mut i = cursor_pos;
    while i < input.len() {
        let ch = input[i..].chars().next().unwrap();
        if is_word_char(ch) {
            break;
        }
        i += ch.len_utf8();
    }
    while i < input.len() {
        let ch = input[i..].chars().next().unwrap();
        if !is_word_char(ch) {
            break;
        }
        i += ch.len_utf8();
    }
    i
}

fn prev_word_boundary(input: &str, cursor_pos: usize) -> usize {
    let cursor_pos = clamp_cursor_to_boundary(input, cursor_pos);
    if cursor_pos == 0 {
        return 0;
    }

    let mut i = cursor_pos;
    while i > 0 {
        let prev = prev_char_boundary(input, i);
        let ch = input[prev..].chars().next().unwrap();
        if is_word_char(ch) {
            break;
        }
        i = prev;
    }
    while i > 0 {
        let prev = prev_char_boundary(input, i);
        let ch = input[prev..].chars().next().unwrap();
        if !is_word_char(ch) {
            break;
        }
        i = prev;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Helper to create an App for testing (avoids needing real channels for simple state tests).
    fn test_app() -> App {
        let (prompt_tx, _prompt_rx) = tokio::sync::mpsc::unbounded_channel();
        let (recommendation_tx, _recommendation_rx) = tokio::sync::mpsc::unbounded_channel();
        let (permission_tx, _permission_rx) = tokio::sync::mpsc::unbounded_channel();
        let (cancel_tx, _cancel_rx) = tokio::sync::mpsc::unbounded_channel();
        let (new_session_tx, _new_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (load_session_tx, _load_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (drop_session_tx, _drop_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (restart_tx, _restart_rx) = tokio::sync::mpsc::unbounded_channel();
        let debug_capture = Arc::new(AtomicBool::new(false));
        App::new(prompt_tx, recommendation_tx, permission_tx, cancel_tx, new_session_tx, load_session_tx, drop_session_tx, restart_tx, debug_capture, true, false)
    }

    // ─── word boundary helpers ──────────────────────────────────────────────

    #[test]
    fn next_word_jumps_to_end_of_current_then_next_word() {
        let s = "hello world";
        // Start of input → end of "hello".
        assert_eq!(next_word_boundary(s, 0), 5);
        // Inside "hello" → end of "hello".
        assert_eq!(next_word_boundary(s, 2), 5);
        // On the space → end of "world".
        assert_eq!(next_word_boundary(s, 5), 11);
        // End of input → stays.
        assert_eq!(next_word_boundary(s, 11), 11);
    }

    #[test]
    fn prev_word_jumps_to_start_of_current_then_previous_word() {
        let s = "hello world";
        // End of input → start of "world".
        assert_eq!(prev_word_boundary(s, 11), 6);
        // On 'w' → start of "hello".
        assert_eq!(prev_word_boundary(s, 6), 0);
        // Inside "world" → start of "world".
        assert_eq!(prev_word_boundary(s, 9), 6);
        // Start of input → stays.
        assert_eq!(prev_word_boundary(s, 0), 0);
    }

    #[test]
    fn word_boundary_skips_punctuation_runs() {
        let s = "foo --bar baz";
        // After "foo" → skip space + "--", land at end of "bar".
        assert_eq!(next_word_boundary(s, 3), 9);
        // From end of "bar" backwards → start of "bar".
        assert_eq!(prev_word_boundary(s, 9), 6);
    }

    #[test]
    fn word_boundary_handles_multibyte_chars() {
        // "你好 world" — each Chinese char is 3 bytes in UTF-8.
        let s = "你好 world";
        assert_eq!(s.len(), 12);
        // Start → end of "你好" (after 2 CJK chars = byte 6).
        assert_eq!(next_word_boundary(s, 0), 6);
        // From end → start of "world" at byte 7.
        assert_eq!(prev_word_boundary(s, 12), 7);
        // From byte 7 (start of "world") → start of "你好" at byte 0.
        assert_eq!(prev_word_boundary(s, 7), 0);
    }

    #[test]
    fn word_boundary_handles_newlines() {
        let s = "foo\nbar";
        // From start → end of "foo".
        assert_eq!(next_word_boundary(s, 0), 3);
        // On '\n' → end of "bar".
        assert_eq!(next_word_boundary(s, 3), 7);
        // From end → start of "bar".
        assert_eq!(prev_word_boundary(s, 7), 4);
    }

    // ─── classify_wt_event ──────────────────────────────────────────────────

    #[test]
    fn classify_connection_failed_is_critical() {
        let params = json!({"session_id": "3", "state": "failed"});
        let n = classify_wt_event("connection_state", "3", &params);
        assert_eq!(n.severity, WtEventSeverity::Critical);
        assert!(n.summary.contains("failed"));
        assert!(!n.acknowledged);
    }

    #[test]
    fn classify_connection_closed_is_actionable() {
        let params = json!({"session_id": "5", "state": "closed"});
        let n = classify_wt_event("connection_state", "5", &params);
        assert_eq!(n.severity, WtEventSeverity::Actionable);
        assert!(n.summary.contains("exited"));
    }

    #[test]
    fn classify_connection_connected_is_informational() {
        let params = json!({"session_id": "1", "state": "connected"});
        let n = classify_wt_event("connection_state", "1", &params);
        assert_eq!(n.severity, WtEventSeverity::Informational);
        assert!(n.summary.contains("connected"));
    }

    #[test]
    fn classify_osc133_command_failed_is_actionable() {
        let params = json!({"session_id": "2", "sequence": "osc:133;D;1"});
        let n = classify_wt_event("vt_sequence", "2", &params);
        assert_eq!(n.severity, WtEventSeverity::Actionable);
        assert!(n.summary.contains("Command failed"));
        assert!(n.summary.contains("exit 1"));
    }

    #[test]
    fn classify_osc133_command_success_is_silent() {
        let params = json!({"session_id": "2", "sequence": "osc:133;D;0"});
        let n = classify_wt_event("vt_sequence", "2", &params);
        assert!(n.acknowledged); // auto-dismissed
    }

    #[test]
    fn classify_osc133_high_exit_code() {
        let params = json!({"session_id": "2", "sequence": "osc:133;D;127"});
        let n = classify_wt_event("vt_sequence", "2", &params);
        assert_eq!(n.severity, WtEventSeverity::Actionable);
        assert!(n.summary.contains("exit 127"));
    }

    #[test]
    fn classify_osc133_prompt_marker_is_silent() {
        // OSC 133;A is a prompt marker, not a command finish
        let params = json!({"session_id": "2", "sequence": "osc:133;A"});
        let n = classify_wt_event("vt_sequence", "2", &params);
        assert!(n.acknowledged); // silenced
    }

    #[test]
    fn classify_normal_vt_sequence_is_silent() {
        let params = json!({"session_id": "7", "sequence": "osc:0;title"});
        let n = classify_wt_event("vt_sequence", "7", &params);
        assert!(n.acknowledged); // silenced
    }

    #[test]
    fn classify_unknown_method_is_informational() {
        let params = json!({"session_id": "1"});
        let n = classify_wt_event("something_new", "1", &params);
        assert_eq!(n.severity, WtEventSeverity::Informational);
    }

    // ─── WtNotification auto-dismiss ────────────────────────────────────────

    #[test]
    fn informational_auto_dismisses_after_threshold() {
        let mut n = WtNotification {
            severity: WtEventSeverity::Informational,
            pane_id: "1".to_string(),
            summary: "test".to_string(),
            acknowledged: false,
            age_ticks: 0,
        };
        assert!(!n.should_auto_dismiss());
        n.age_ticks = 42;
        assert!(!n.should_auto_dismiss());
        n.age_ticks = 43;
        assert!(n.should_auto_dismiss());
    }

    #[test]
    fn critical_never_auto_dismisses() {
        let n = WtNotification {
            severity: WtEventSeverity::Critical,
            pane_id: "1".to_string(),
            summary: "crash".to_string(),
            acknowledged: false,
            age_ticks: 1000,
        };
        assert!(!n.should_auto_dismiss());
    }

    #[test]
    fn actionable_never_auto_dismisses() {
        let n = WtNotification {
            severity: WtEventSeverity::Actionable,
            pane_id: "1".to_string(),
            summary: "exited".to_string(),
            acknowledged: false,
            age_ticks: 1000,
        };
        assert!(!n.should_auto_dismiss());
    }

    // ─── App notification state ─────────────────────────────────────────────

    #[test]
    fn wt_event_critical_shows_banner_and_error_message() {
        let mut app = test_app();
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "3".to_string(),
            params: json!({"session_id": "3", "state": "failed"}),
        });
        assert!(app.show_notification_banner);
        assert_eq!(app.wt_notifications.len(), 1);
        assert_eq!(app.wt_notifications[0].severity, WtEventSeverity::Critical);
        // Should have an Error message in chat
        assert!(app.current_tab().messages.iter().any(|m| matches!(m, ChatMessage::Error(_))));
    }

    #[test]
    fn wt_event_actionable_shows_banner_and_system_message() {
        let mut app = test_app();
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "5".to_string(),
            params: json!({"session_id": "5", "state": "closed"}),
        });
        assert!(app.show_notification_banner);
        assert!(app.current_tab().messages.iter().any(|m| matches!(m, ChatMessage::System(_))));
    }

    #[test]
    fn wt_event_informational_no_banner_no_chat_message() {
        let mut app = test_app();
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "1".to_string(),
            params: json!({"session_id": "1", "state": "connected"}),
        });
        assert!(!app.show_notification_banner);
        assert!(app.current_tab().messages.is_empty());
        assert_eq!(app.wt_notifications.len(), 1);
    }

    #[test]
    fn wt_event_from_own_pane_is_ignored() {
        let mut app = test_app();
        app.pane_id = Some("42".to_string());
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "42".to_string(),
            params: json!({"session_id": "42", "state": "failed"}),
        });
        // Events from our own pane should be completely ignored
        assert!(!app.show_notification_banner);
        assert!(app.wt_notifications.is_empty());
        assert!(app.current_tab().messages.is_empty());
    }

    #[test]
    fn dismiss_notifications_clears_banner_and_acknowledges() {
        let mut app = test_app();
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "3".to_string(),
            params: json!({"session_id": "3", "state": "failed"}),
        });
        assert!(app.show_notification_banner);
        assert_eq!(app.unacknowledged_count(), 1);

        app.dismiss_notifications();
        assert!(!app.show_notification_banner);
        assert_eq!(app.unacknowledged_count(), 0);
        assert!(app.wt_notifications[0].acknowledged);
    }

    #[test]
    fn notification_badge_returns_most_recent_unacknowledged() {
        let mut app = test_app();
        // First event
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "1".to_string(),
            params: json!({"session_id": "1", "state": "closed"}),
        });
        // Second event (more recent)
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "2".to_string(),
            params: json!({"session_id": "2", "state": "failed"}),
        });

        let (summary, severity) = app.notification_badge().unwrap();
        assert!(summary.contains("Pane 2"));
        assert_eq!(*severity, WtEventSeverity::Critical);
        assert_eq!(app.unacknowledged_count(), 2);
    }

    #[test]
    fn notification_queue_caps_at_20() {
        let mut app = test_app();
        for i in 0..25 {
            app.handle_event(AppEvent::WtEvent {
                method: "connection_state".to_string(),
                pane_id: format!("{}", i),
                params: json!({"session_id": format!("{}", i), "state": "connected"}),
            });
        }
        assert_eq!(app.wt_notifications.len(), 20);
    }

    #[test]
    fn tick_ages_and_auto_dismisses_informational() {
        let mut app = test_app();
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "1".to_string(),
            params: json!({"session_id": "1", "state": "connected"}),
        });
        assert_eq!(app.wt_notifications.len(), 1);
        assert_eq!(app.wt_notifications[0].age_ticks, 0);

        // Simulate enough ticks to trigger auto-dismiss (43 ticks)
        for _ in 0..43 {
            app.handle_event(AppEvent::Tick);
        }
        // Informational notification should be auto-removed
        assert_eq!(app.wt_notifications.len(), 0);
    }

    #[test]
    fn tick_does_not_dismiss_critical_notifications() {
        let mut app = test_app();
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "3".to_string(),
            params: json!({"session_id": "3", "state": "failed"}),
        });
        // Simulate many ticks
        for _ in 0..200 {
            app.handle_event(AppEvent::Tick);
        }
        // Critical notification should persist
        assert_eq!(app.wt_notifications.len(), 1);
        assert!(app.show_notification_banner);
    }

    #[test]
    fn banner_hides_when_all_acknowledged() {
        let mut app = test_app();
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "3".to_string(),
            params: json!({"session_id": "3", "state": "failed"}),
        });
        assert!(app.show_notification_banner);

        // Acknowledge all
        app.dismiss_notifications();

        // One more tick to process the banner-hide logic
        app.handle_event(AppEvent::Tick);
        assert!(!app.show_notification_banner);
    }

    #[test]
    fn active_notification_returns_none_when_all_acknowledged() {
        let mut app = test_app();
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "3".to_string(),
            params: json!({"session_id": "3", "state": "closed"}),
        });
        assert!(app.active_notification().is_some());

        app.dismiss_notifications();
        assert!(app.active_notification().is_none());
    }

    #[test]
    fn multiple_events_different_panes() {
        let mut app = test_app();
        // Informational from pane 1
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "1".to_string(),
            params: json!({"session_id": "1", "state": "connected"}),
        });
        // Critical from pane 2
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "2".to_string(),
            params: json!({"session_id": "2", "state": "failed"}),
        });
        // Actionable from pane 3
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "3".to_string(),
            params: json!({"session_id": "3", "state": "closed"}),
        });

        assert_eq!(app.wt_notifications.len(), 3);
        // Unacknowledged count only counts actionable + critical
        assert_eq!(app.unacknowledged_count(), 2);
        // Banner should show (due to critical + actionable)
        assert!(app.show_notification_banner);
        // Chat should have 2 messages (critical error + actionable system msg)
        assert_eq!(app.current_tab().messages.len(), 2);
    }

    // ─── F2 Agents view: Enter / Delete dispatch ───────────────────────────
    //
    // Originally added in commit `e4723510e` ("Enter/Delete actions on Agents
    // view (M4.4-M4.6)") and lost in the post-#29 merge that stubbed out
    // dispatch_resume. Re-added on top of the new
    // `spawn_wtcli_split_then_focus_with_callback` helper.

    #[test]
    fn enter_on_live_row_dispatches_focus_command() {
        use crate::agent_sessions::{CliSource, SessionEvent};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use std::path::PathBuf;
        let mut app = test_app();
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "a".into(),
            cli_source: CliSource::Claude,
            pane_session_id: "00000000-0000-0000-0000-0000000000aa".into(),
            cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_list_state.select(Some(0));

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let cmd = app
            .last_dispatched_command_for_test()
            .expect("a command was dispatched");
        assert_eq!(cmd.kind, DispatchedCommandKind::FocusPane);
        assert_eq!(
            cmd.session_id.as_deref(),
            Some("00000000-0000-0000-0000-0000000000aa")
        );
    }

    #[test]
    fn enter_on_history_row_dispatches_new_tab_with_resume() {
        use crate::agent_sessions::{CliSource, SessionEvent};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use std::path::PathBuf;
        let mut app = test_app();
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "abc-123".into(),
            cli_source: CliSource::Claude,
            pane_session_id: "p".into(),
            cwd: PathBuf::from("/work/proj"),
            title: "t".into(),
        });
        app.agent_sessions.apply(SessionEvent::SessionStopped {
            key: "abc-123".into(),
            reason: "user_exit".into(),
        });

        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_list_state.select(Some(0));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let cmd = app
            .last_dispatched_command_for_test()
            .expect("a command was dispatched");
        assert_eq!(cmd.kind, DispatchedCommandKind::NewTabResume);
        let argv = cmd.argv.join(" ");
        // The dispatch must use `wtcli new-tab` (not `split-pane`) so the
        // resumed CLI lands in its own WT tab instead of carving up the
        // originating tab.
        assert!(argv.contains("new-tab"), "argv: {}", argv);
        assert!(
            !argv.contains("split-pane"),
            "argv must NOT use split-pane: {}",
            argv
        );
        // The CLI invocation is still wrapped in `cmd /c` so .cmd shims
        // resolve via PATHEXT, but the legacy `cd /d` prefix is gone —
        // cwd is threaded through wtcli's `-d` flag now.
        assert!(
            argv.contains("cmd /c claude --resume abc-123"),
            "argv: {}",
            argv
        );
        assert!(
            !argv.contains("cd /d"),
            "argv must NOT contain cd /d (cwd is now passed via -d): {}",
            argv
        );
        // Resume is keyed off the session's project cwd — the new tab's
        // primary pane must start in that directory so the CLI's session
        // store lookup (`~/.claude/projects/<encoded-cwd>/...`) succeeds.
        assert!(
            argv.contains("-d /work/proj"),
            "expected -d <cwd>; argv: {}",
            argv
        );
    }

    #[test]
    fn shift_enter_on_history_row_dispatches_resume_in_agent_pane() {
        // Shift+Enter on a terminal-state row should route to the
        // ResumeInAgentPane path, NOT the legacy NewTabResume — it
        // emits `resume_in_new_agent_tab` to WT instead of spawning a
        // normal terminal tab locally. The dispatched-command tape
        // captures the shape so downstream wiring can be
        // regression-checked.
        use crate::agent_sessions::{CliSource, SessionEvent};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use std::path::PathBuf;
        let mut app = test_app();
        // Capability gate: dispatch is only attempted when the agent
        // advertised loadSession. Without this, the handler
        // short-circuits with a system message instead.
        app.agent_supports_load_session = true;
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "abc-123".into(),
            cli_source: CliSource::Claude,
            pane_session_id: "p".into(),
            cwd: PathBuf::from("/work/proj"),
            title: "t".into(),
        });
        app.agent_sessions.apply(SessionEvent::SessionStopped {
            key: "abc-123".into(),
            reason: "user_exit".into(),
        });

        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_list_state.select(Some(0));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));

        let cmd = app
            .last_dispatched_command_for_test()
            .expect("a command was dispatched");
        assert_eq!(cmd.kind, DispatchedCommandKind::ResumeInAgentPane);
        assert_eq!(cmd.session_id.as_deref(), Some("abc-123"));
        let argv = cmd.argv.join(" ");
        assert!(
            argv.contains("resume_in_new_agent_tab"),
            "argv: {}",
            argv
        );
        assert!(argv.contains("--session-id abc-123"), "argv: {}", argv);
        assert!(argv.contains("--cwd /work/proj"), "argv: {}", argv);
    }

    #[test]
    fn shift_enter_history_row_without_load_session_capability_shows_hint() {
        // Capability gate: when the agent doesn't advertise loadSession,
        // Shift+Enter must not open a new tab. Instead it pushes a
        // system message in the session management view explaining the
        // fallback (plain Enter). The dispatched-command tape captures
        // the gated path so the regression is observable.
        use crate::agent_sessions::{CliSource, SessionEvent};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use std::path::PathBuf;
        let mut app = test_app();
        // No `agent_supports_load_session = true` — default is false.
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "abc-123".into(),
            cli_source: CliSource::Claude,
            pane_session_id: "p".into(),
            cwd: PathBuf::from("/work/proj"),
            title: "t".into(),
        });
        app.agent_sessions.apply(SessionEvent::SessionStopped {
            key: "abc-123".into(),
            reason: "user_exit".into(),
        });

        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_list_state.select(Some(0));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));

        let cmd = app
            .last_dispatched_command_for_test()
            .expect("a command was dispatched");
        assert_eq!(cmd.kind, DispatchedCommandKind::ResumeInAgentPane);
        let argv = cmd.argv.join(" ");
        assert!(argv.contains("--unsupported"), "argv: {}", argv);
        // The current tab gets a System hint message.
        let has_hint = app.current_tab().messages.iter().any(|m| {
            matches!(m, ChatMessage::System(text)
                if text.contains("loadSession")
                    && text.contains("Press Enter"))
        });
        assert!(has_hint, "expected system hint message in the current tab");
    }

    #[test]
    fn shift_enter_on_live_row_falls_back_to_focus() {
        // Live rows have no historical state to "load" — Shift+Enter on
        // them must NOT trigger the resume-in-agent-pane flow. It falls
        // through to the same FocusPane dispatch as plain Enter.
        use crate::agent_sessions::{CliSource, SessionEvent};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use std::path::PathBuf;
        let mut app = test_app();
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "a".into(),
            cli_source: CliSource::Claude,
            pane_session_id: "00000000-0000-0000-0000-0000000000aa".into(),
            cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_list_state.select(Some(0));

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        let cmd = app
            .last_dispatched_command_for_test()
            .expect("a command was dispatched");
        assert_eq!(cmd.kind, DispatchedCommandKind::FocusPane);
    }

    #[test]
    fn delete_on_history_row_removes_session_from_registry() {
        use crate::agent_sessions::{CliSource, SessionEvent};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use std::path::PathBuf;
        let mut app = test_app();
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "k".into(),
            cli_source: CliSource::Claude,
            pane_session_id: "p".into(),
            cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        app.agent_sessions.apply(SessionEvent::SessionStopped {
            key: "k".into(),
            reason: "".into(),
        });
        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_list_state.select(Some(0));

        app.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert!(!app.agent_sessions.has_session(&"k".to_string()));
    }

    #[test]
    fn delete_on_live_row_is_noop() {
        use crate::agent_sessions::{CliSource, SessionEvent};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use std::path::PathBuf;
        let mut app = test_app();
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "k".into(),
            cli_source: CliSource::Claude,
            pane_session_id: "p".into(),
            cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_list_state.select(Some(0));

        app.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert!(app.agent_sessions.has_session(&"k".to_string()));
    }

    #[test]
    fn agents_view_state_is_isolated_per_tab() {
        // Regression: opening the Agents picker in tab A should not show
        // up as opened (or with the same selection) when the user switches
        // to tab B. `current_view` and `agents_list_state` live on
        // TabSession exactly to keep these states independent.
        use crate::agent_sessions::{CliSource, SessionEvent};
        use std::path::PathBuf;
        let mut app = test_app();
        for k in ["a", "b", "c"] {
            app.agent_sessions.apply(SessionEvent::SessionStarted {
                key: k.into(),
                cli_source: CliSource::Claude,
                pane_session_id: format!("p-{}", k),
                cwd: PathBuf::from("/x"),
                title: format!("t-{}", k),
            });
        }

        // Tab "0" (the seeded default): open picker, select row 2.
        app.tab_id = Some("0".into());
        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_list_state.select(Some(2));

        // Switch to tab "1" — its TabSession is lazily created with
        // defaults: View::Chat and no selection.
        app.tab_id = Some("1".into());
        let tab1 = app.current_tab_mut();
        assert_eq!(tab1.current_view, View::Chat, "new tab must start in Chat");
        assert_eq!(tab1.agents_list_state.selected(), None);

        // Mutating tab 1 must not bleed back into tab 0.
        tab1.current_view = View::Agents;
        tab1.agents_list_state.select(Some(0));

        app.tab_id = Some("0".into());
        let tab0 = app.current_tab();
        assert_eq!(tab0.current_view, View::Agents);
        assert_eq!(tab0.agents_list_state.selected(), Some(2));
    }

    // ─── Autofix suppression for agent CLI panes ───────────────────────────
    //
    // Regression test: typing `exit` in a resumed Claude/Gemini/Copilot pane
    // emits `connection_state: closed` for that pane GUID. Without this
    // guard, autofix fires with the dead pane GUID as `source_pane_id`, and
    // the ACP client's prompt-context read trips
    // `TerminalProtocolComServer::ReadPaneOutput` -> `winrt::throw_hresult(E_FAIL)`
    // on the C++ side. `is_agent_pane()` was added precisely to suppress
    // this, but until this fix nothing in the Rust autofix path consulted it.

    #[test]
    fn autofix_suppressed_when_pane_is_agent_session() {
        use crate::agent_sessions::{CliSource, SessionEvent};
        use std::path::PathBuf;
        let mut app = test_app();
        // Autofix needs Connected state to consider triggering at all.
        app.state = ConnectionState::Connected;
        app.autofix_enabled = true;
        // Bind a pane GUID to a Claude agent session.
        let pane = "11111111-2222-3333-4444-555555555555";
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "claude-key".into(),
            cli_source: CliSource::Claude,
            pane_session_id: pane.into(),
            cwd: PathBuf::from("/work/proj"),
            title: "t".into(),
        });

        // Simulate the closure event that fires when the user types `exit`
        // in the resumed Claude pane.
        let notification = WtNotification {
            severity: WtEventSeverity::Actionable,
            pane_id: pane.to_string(),
            summary: format!("Pane {}: process exited", pane),
            acknowledged: false,
            age_ticks: 0,
        };
        app.maybe_trigger_autofix(&notification);

        // Suppression: no autofix prompt should be in-flight, no armed pane.
        assert!(
            app.autofix_pane_id.is_none(),
            "autofix must not arm an agent CLI pane on its own exit"
        );
        assert!(
            app.current_tab().turn.is_idle(),
            "no autofix prompt should have been sent"
        );
    }

    #[test]
    fn autofix_still_triggers_for_non_agent_pane() {
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.autofix_enabled = true;
        // No SessionStarted apply -> pane is not an agent pane.
        let pane = "non-agent-pane-guid";

        let notification = WtNotification {
            severity: WtEventSeverity::Actionable,
            pane_id: pane.to_string(),
            summary: "Command failed (exit 1)".to_string(),
            acknowledged: false,
            age_ticks: 0,
        };
        app.maybe_trigger_autofix(&notification);

        assert_eq!(
            app.autofix_pane_id.as_deref(),
            Some(pane),
            "autofix must still arm normal panes when a command fails"
        );
        assert!(
            !app.current_tab().turn.is_idle(),
            "autofix prompt should be in-flight"
        );
    }

    /// Copilot scenario: agent CLI's SessionStopped hook runs before the
    /// pane's connection_state:closed event arrives, so by the time
    /// `is_agent_pane` would be queried inside `maybe_trigger_autofix`,
    /// `active_by_pane` has already been cleared. The deeper guard at the
    /// `handle_event` layer — only routing `vt_sequence` events to autofix
    /// — covers this case because pane closure (`connection_state:closed`)
    /// no longer dispatches to autofix at all.
    #[test]
    fn connection_state_closed_does_not_trigger_autofix_even_when_binding_cleared() {
        use crate::agent_sessions::{CliSource, SessionEvent};
        use std::path::PathBuf;
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.autofix_enabled = true;
        let pane = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

        // Bind, then unbind — mirrors the Copilot order: agent.session.end
        // hook arrives and runs SessionStopped before WT emits closed.
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "copilot-key".into(),
            cli_source: CliSource::Copilot,
            pane_session_id: pane.into(),
            cwd: PathBuf::from("/work"),
            title: "t".into(),
        });
        app.agent_sessions.apply(SessionEvent::SessionStopped {
            key: "copilot-key".into(),
            reason: "user_exit".into(),
        });
        // Sanity: binding is gone, so the inner is_agent_pane guard alone
        // would not catch this.
        assert!(!app.agent_sessions.is_agent_pane(pane));

        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: pane.to_string(),
            params: serde_json::json!({"session_id": pane, "state": "closed"}),
        });

        assert!(
            app.autofix_pane_id.is_none(),
            "connection_state:closed must never arm autofix — no exit code, \
             no command context, pane is dead so subsequent ReadPaneOutput \
             would throw E_FAIL"
        );
        assert!(
            app.current_tab().turn.is_idle(),
            "no autofix prompt should be in-flight"
        );
        // The user still gets a system message about the pane closing.
        assert!(
            app.current_tab()
                .messages
                .iter()
                .any(|m| matches!(m, ChatMessage::System(_))),
            "the user still gets a system message about the pane closing"
        );
    }

    /// Defense-in-depth: a vt_sequence (osc:133;D non-zero) inside an agent
    /// pane is unusual but possible. The original `is_agent_pane` guard
    /// inside `maybe_trigger_autofix` covers it.
    #[test]
    fn vt_sequence_failure_in_agent_pane_is_suppressed() {
        use crate::agent_sessions::{CliSource, SessionEvent};
        use std::path::PathBuf;
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.autofix_enabled = true;
        let pane = "11111111-2222-3333-4444-555555555555";
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "claude-key".into(),
            cli_source: CliSource::Claude,
            pane_session_id: pane.into(),
            cwd: PathBuf::from("/work"),
            title: "t".into(),
        });

        // Synthesize an osc:133;D;1 from inside the agent pane.
        app.handle_event(AppEvent::WtEvent {
            method: "vt_sequence".to_string(),
            pane_id: pane.to_string(),
            params: serde_json::json!({
                "session_id": pane,
                "sequence": "osc:133;D;1",
            }),
        });

        assert!(
            app.autofix_pane_id.is_none(),
            "agent CLI panes must not arm autofix even on osc:133;D failures"
        );
    }

    /// Positive coverage: a vt_sequence (osc:133;D;1) in a normal shell pane
    /// still fires autofix (the proper command-failure signal). Ensures the
    /// new "vt_sequence-only" routing doesn't silently disable autofix.
    #[test]
    fn vt_sequence_failure_in_normal_pane_still_triggers_autofix() {
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.autofix_enabled = true;
        let pane = "fedcba98-7654-3210-fedc-ba9876543210";

        app.handle_event(AppEvent::WtEvent {
            method: "vt_sequence".to_string(),
            pane_id: pane.to_string(),
            params: serde_json::json!({
                "session_id": pane,
                "sequence": "osc:133;D;1",
            }),
        });

        assert_eq!(
            app.autofix_pane_id.as_deref(),
            Some(pane),
            "vt_sequence osc:133;D;<non-zero> in a normal pane must still arm autofix"
        );
    }

    /// Gemini "manual launch" scenario: the user opened a normal pwsh/cmd
    /// pane and typed `gemini`. The hook bridge fires `agent.session.start`
    /// (binding the pane) but `agent.session.end` is unreliable on `/exit`
    /// (Gemini cancels its own hook chain), AND the pane stays alive after
    /// Gemini exits because pwsh keeps running. So neither
    /// `connection_state: closed` nor `SessionStopped` ever arrive.
    ///
    /// The shell's FinalTerm prompt-start marker (`osc:133;A`) fires when
    /// pwsh redraws its prompt after Gemini releases the foreground —
    /// that's our signal.
    #[test]
    fn osc133_prompt_start_in_agent_pane_transitions_row_to_ended() {
        use crate::agent_sessions::{AgentStatus, CliSource, SessionEvent};
        use std::path::PathBuf;
        let mut app = test_app();
        let pane = "ffffffff-eeee-dddd-cccc-bbbbbbbbbbbb";
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "gemini-key".into(),
            cli_source: CliSource::Gemini,
            pane_session_id: pane.into(),
            cwd: PathBuf::from("/work"),
            title: "t".into(),
        });
        // Sanity: row is live before the prompt-start arrives.
        assert!(app.agent_sessions.is_agent_pane(pane));

        app.handle_event(AppEvent::WtEvent {
            method: "vt_sequence".to_string(),
            pane_id: pane.to_string(),
            params: serde_json::json!({
                "session_id": pane,
                "sequence": "osc:133;A",
            }),
        });

        let s = app
            .agent_sessions
            .iter_sorted()
            .into_iter()
            .find(|s| s.key == "gemini-key")
            .expect("row still exists");
        assert!(
            matches!(s.status, AgentStatus::Ended),
            "agent-bound pane seeing osc:133;A must transition to Ended; got {:?}",
            s.status
        );
        assert!(s.pane_session_id.is_none());
    }

    /// Negative coverage: `osc:133;A` in a normal (non-agent) pane must
    /// never apply PaneClosed (defensive — the registry would treat it as
    /// a no-op anyway, but verify the guard short-circuits the call).
    #[test]
    fn osc133_prompt_start_in_normal_pane_is_inert() {
        let mut app = test_app();
        let pane = "00000000-1111-2222-3333-444444444444";
        // No SessionStarted apply -> not an agent pane.
        app.handle_event(AppEvent::WtEvent {
            method: "vt_sequence".to_string(),
            pane_id: pane.to_string(),
            params: serde_json::json!({
                "session_id": pane,
                "sequence": "osc:133;A",
            }),
        });
        // Nothing to assert positively — the registry just doesn't grow.
        assert_eq!(app.agent_sessions.iter_sorted().len(), 0);
    }

    /// Gemini scenario: no `agent.session.end` hook bridge, so the only
    /// signal we get when the user `/exit`s a resumed Gemini pane is
    /// WT-native `connection_state: closed`. Without bridging that into a
    /// `SessionEvent::PaneClosed`, the row stays stuck at Idle/Working
    /// forever in the F2 list.
    #[test]
    fn connection_state_closed_transitions_agent_row_to_ended() {
        use crate::agent_sessions::{AgentStatus, CliSource, SessionEvent};
        use std::path::PathBuf;
        let mut app = test_app();
        let pane = "deadbeef-1111-2222-3333-444455556666";
        // Gemini-style: the pane was bound (via ResumePaneAssigned in real
        // life; SessionStarted is a stand-in here) but no session.end hook
        // ever fires.
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "gemini-key".into(),
            cli_source: CliSource::Gemini,
            pane_session_id: pane.into(),
            cwd: PathBuf::from("/work"),
            title: "t".into(),
        });
        // Sanity: the row is live before close.
        let s = app
            .agent_sessions
            .iter_sorted()
            .into_iter()
            .find(|s| s.key == "gemini-key")
            .expect("row exists");
        assert!(matches!(s.status, AgentStatus::Idle | AgentStatus::Working));

        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: pane.to_string(),
            params: serde_json::json!({"session_id": pane, "state": "closed"}),
        });

        let s = app
            .agent_sessions
            .iter_sorted()
            .into_iter()
            .find(|s| s.key == "gemini-key")
            .expect("row still exists");
        assert!(
            matches!(s.status, AgentStatus::Ended),
            "Gemini row must transition to Ended on connection_state:closed; got {:?}",
            s.status
        );
        assert!(
            s.pane_session_id.is_none(),
            "pane binding should be cleared after close"
        );
    }

    // ─── turn-state integration tests ──────────────────────────────────────
    //
    // Drive `App` directly through the turn-state transitions in
    // `doc/specs/turn-state-refactor.md`'s table. We use the active tab's
    // `DEFAULT_TAB_ID` as the session key — `session_tab_mut` falls back to
    // the active tab when the id is unknown, which keeps these tests free
    // of ACP wiring.

    fn submit_test_prompt(app: &mut App, text: &str) {
        let prompt = SubmittedPrompt {
            id: 42,
            text: text.into(),
            submitted_at_unix_s: 0.0,
            autofix: None,
        };
        app.turn_submit_prompt(DEFAULT_TAB_ID, prompt);
    }

    fn submit_autofix_prompt(app: &mut App, pane: &str) {
        app.autofix_generation = app.autofix_generation.wrapping_add(1);
        app.autofix_pane_id = Some(pane.into());
        let prompt = SubmittedPrompt {
            id: 99,
            text: "diagnose this".into(),
            submitted_at_unix_s: 0.0,
            autofix: Some(AutofixContext {
                target_pane_id: pane.into(),
                generation: app.autofix_generation,
            }),
        };
        app.turn_submit_prompt(DEFAULT_TAB_ID, prompt);
    }

    #[test]
    fn submit_clears_messages_and_pushes_user_bubble() {
        let mut app = test_app();
        app.current_tab_mut()
            .messages
            .push(ChatMessage::System("stale".into()));
        submit_test_prompt(&mut app, "hello");
        let tab = app.current_tab();
        assert!(matches!(tab.turn, TurnState::Submitted(_)));
        assert!(
            !tab.turn.accepts_new_prompt(),
            "Submitted blocks new prompts"
        );
        assert_eq!(tab.messages.len(), 1, "stale System bubble was cleared");
        assert!(matches!(tab.messages[0], ChatMessage::User(ref t) if t == "hello"));
    }

    #[test]
    fn first_message_chunk_transitions_to_streaming_with_buf() {
        let mut app = test_app();
        submit_test_prompt(&mut app, "hi");
        let advanced =
            app.turn_observe_chunk(DEFAULT_TAB_ID, ChunkKind::Message, "partial");
        assert!(advanced, "first message chunk must advance the buffer");
        let tab = app.current_tab();
        assert_eq!(tab.turn.buffer(), Some("partial"));
        assert!(tab.turn.is_streaming());
    }

    #[test]
    fn thought_chunk_first_transitions_with_empty_buf() {
        let mut app = test_app();
        submit_test_prompt(&mut app, "hi");
        let advanced =
            app.turn_observe_chunk(DEFAULT_TAB_ID, ChunkKind::Thought, "thinking…");
        assert!(!advanced, "thought chunks never advance the buffer");
        let tab = app.current_tab();
        assert!(tab.turn.is_streaming());
        assert_eq!(tab.turn.buffer(), Some(""));
    }

    #[test]
    fn end_with_no_eager_chat_fallback_commits_completed_turn() {
        let mut app = test_app();
        submit_test_prompt(&mut app, "why blue?");
        // Pure prose — won't parse as a RecommendationSet, falls to chat.
        app.turn_observe_chunk(
            DEFAULT_TAB_ID,
            ChunkKind::Message,
            "Light scatters in the atmosphere.",
        );
        app.turn_close(DEFAULT_TAB_ID);
        let tab = app.current_tab();
        assert!(
            matches!(
                tab.turn,
                TurnState::Surfaced {
                    outcome: TurnOutcome::ChatTurn,
                    end_pending: false,
                    ..
                }
            ),
            "got {:?}",
            tab.turn
        );
        assert!(tab.turn.accepts_new_prompt(), "chat fallback unblocks input");
        assert_eq!(tab.completed_turns.len(), 1);
        assert_eq!(tab.completed_turns[0].prompt, "why blue?");
    }

    #[test]
    fn end_with_no_chunks_clears_autofix_bottom_bar() {
        let mut app = test_app();
        submit_autofix_prompt(&mut app, "pane-7");
        assert!(app.autofix_pane_id.is_some());
        // No chunks arrived; AgentMessageEnd fires.
        app.turn_close(DEFAULT_TAB_ID);
        let tab = app.current_tab();
        assert!(
            matches!(
                tab.turn,
                TurnState::Surfaced {
                    outcome: TurnOutcome::Empty,
                    end_pending: false,
                    ..
                }
            ),
            "got {:?}",
            tab.turn
        );
        assert!(
            app.autofix_pane_id.is_none(),
            "autofix_pane_id must be cleared so the bar leaves Pending"
        );
    }

    #[test]
    fn stale_autofix_chunks_dropped_when_generation_diverges() {
        let mut app = test_app();
        submit_autofix_prompt(&mut app, "pane-1");
        // Simulate an Esc cancel or a newer trigger bumping the counter.
        app.autofix_generation = app.autofix_generation.wrapping_add(1);
        let advanced =
            app.turn_observe_chunk(DEFAULT_TAB_ID, ChunkKind::Message, "stale");
        assert!(!advanced, "stale-gen chunks must be dropped");
        let tab = app.current_tab();
        assert!(
            matches!(tab.turn, TurnState::Submitted(_)),
            "state unchanged on stale drop, got {:?}",
            tab.turn
        );
        assert_eq!(tab.turn.buffer(), None);
    }

    #[test]
    fn stale_autofix_at_close_resets_to_idle() {
        let mut app = test_app();
        submit_autofix_prompt(&mut app, "pane-1");
        // A chunk advances state to Streaming.
        app.turn_observe_chunk(DEFAULT_TAB_ID, ChunkKind::Message, "partial");
        // Generation diverges (newer trigger / Esc).
        app.autofix_generation = app.autofix_generation.wrapping_add(1);
        app.turn_close(DEFAULT_TAB_ID);
        assert!(
            app.current_tab().turn.is_idle(),
            "stale-close must reset to Idle, got {:?}",
            app.current_tab().turn
        );
    }

    #[test]
    fn cancel_bumps_generation_and_returns_to_idle() {
        let mut app = test_app();
        submit_autofix_prompt(&mut app, "pane-1");
        let gen_before = app.autofix_generation;
        app.turn_cancel(DEFAULT_TAB_ID);
        assert_eq!(app.autofix_generation, gen_before.wrapping_add(1));
        assert!(app.current_tab().turn.is_idle());
        assert!(app.autofix_pane_id.is_none());
    }

    #[test]
    fn end_pending_blocks_new_prompts_until_message_end() {
        // Eager-surface path: user submits → JSON streams → recommendation
        // surfaces before AgentMessageEnd. While end_pending=true the UI
        // gate must hold. AgentMessageEnd then releases it.
        let mut app = test_app();
        submit_test_prompt(&mut app, "first");
        // RecommendationSet shape that survives `validate_recommendation_set`.
        let json = r#"```json
{"recommended_choice":1,"choices":[{"choice":1,"title":"do it","rationale":"r","actions":[{"type":"send","parent":"pane-X","input":"ls"}]}]}
```"#;
        app.turn_observe_chunk(DEFAULT_TAB_ID, ChunkKind::Message, json);
        app.turn_try_eager_surface(DEFAULT_TAB_ID);
        let tab = app.current_tab();
        assert!(
            matches!(
                tab.turn,
                TurnState::Surfaced {
                    outcome: TurnOutcome::Recommendation(_),
                    end_pending: true,
                    ..
                }
            ),
            "expected eager surface, got {:?}",
            tab.turn
        );
        assert!(
            !tab.turn.accepts_new_prompt(),
            "end_pending=true must hold the UI gate"
        );
        // AgentMessageEnd flips end_pending=false.
        app.turn_close(DEFAULT_TAB_ID);
        assert!(app.current_tab().turn.accepts_new_prompt());
    }

    // ─── card / panel height math ───────────────────────────────────────────

    use crate::app::turn_state::{SubmittedPrompt, TurnOutcome, TurnState};
    use crate::coordinator::{
        OpenTarget, RecommendationChoice, RecommendationSet, RecommendedAction,
    };
    use crate::ui::card::{card_content_width, CARD_H_CHROME, CARD_MIN_SIZE};

    fn perm_with(desc: &str) -> PermissionState {
        PermissionState {
            description: desc.to_string(),
            options: vec![PermOption {
                id: "allow_once".into(),
                name: "Allow".into(),
                kind: "allow_once".into(),
            }],
            selected: 0,
            responder: None,
        }
    }

    fn rec_send(input: &str) -> RecommendationChoice {
        RecommendationChoice {
            choice: 0,
            title: "t".into(),
            rationale: String::new(),
            actions: vec![RecommendedAction::Send {
                parent: String::new(),
                input: input.into(),
            }],
        }
    }

    fn install_recs(app: &mut App, choices: Vec<RecommendationChoice>) {
        let tab = app.current_tab_mut();
        tab.turn = TurnState::Surfaced {
            prompt: SubmittedPrompt {
                id: 1,
                text: "p".into(),
                submitted_at_unix_s: 0.0,
                autofix: None,
            },
            outcome: TurnOutcome::Recommendation(RecommendationSet {
                recommended_choice: Some(0),
                choices,
            }),
            end_pending: false,
        };
    }

    #[test]
    fn card_content_width_subtracts_chrome_and_floors_at_1() {
        assert_eq!(card_content_width(80), 80 - CARD_H_CHROME as usize);
        assert_eq!(card_content_width(CARD_H_CHROME + 1), 1);
        assert_eq!(card_content_width(CARD_H_CHROME), 1);
        assert_eq!(card_content_width(0), 1);
    }

    #[test]
    fn permission_card_height_single_line_is_card_min() {
        let perm = perm_with("ok");
        assert_eq!(
            permission_card_height(&perm, 80) as u16,
            CARD_MIN_SIZE
        );
    }

    #[test]
    fn permission_card_height_counts_wrap_at_actual_panel_width() {
        let perm = perm_with(&"a".repeat(200));
        // Full-width terminal: wrap at 80 - 8 = 72.
        let inner_full = 80 - CARD_H_CHROME as usize;
        assert_eq!(
            permission_card_height(&perm, 80),
            CARD_MIN_SIZE as usize + 200_usize.div_ceil(inner_full) - 1
        );
        // Debug panel open: 60% of 80 = 48 → wrap at 40.
        let inner_split = 48 - CARD_H_CHROME as usize;
        assert_eq!(
            permission_card_height(&perm, 48),
            CARD_MIN_SIZE as usize + 200_usize.div_ceil(inner_split) - 1
        );
        // The two should differ — proves the panel_width input matters
        // (the PR #20 reviewer-3 bug).
        assert_ne!(
            permission_card_height(&perm, 80),
            permission_card_height(&perm, 48)
        );
    }

    #[test]
    fn permission_card_height_treats_blank_lines_as_one_row() {
        let perm = perm_with("line1\n\nline2");
        // 3 logical lines (blank counts as 1).
        assert_eq!(permission_card_height(&perm, 80), CARD_MIN_SIZE as usize + 2);
    }

    #[test]
    fn rec_card_height_includes_inter_card_gap() {
        let h = rec_card_height(&rec_send("ls"), 80);
        assert_eq!(h as u16, CARD_MIN_SIZE + 1);
    }

    #[test]
    fn rec_card_height_handles_open_action_synthesis() {
        let choice = RecommendationChoice {
            choice: 0,
            title: "t".into(),
            rationale: String::new(),
            actions: vec![RecommendedAction::Open {
                target: OpenTarget::Tab,
                parent: None,
                cwd: Some("C:/repo".into()),
                title: Some("logs".into()),
                direction: None,
            }],
        };
        let h = rec_card_height(&choice, 80);
        // "New tab (logs) in C:/repo" fits on one row at width 72.
        assert_eq!(h as u16, CARD_MIN_SIZE + 1);
    }

    #[test]
    fn permission_panel_height_zero_when_no_permission() {
        let mut app = test_app();
        app.terminal_rows = 30;
        assert_eq!(app.permission_panel_height(80), 0);
    }

    #[test]
    fn permission_panel_height_falls_back_to_compact_below_card_min() {
        let mut app = test_app();
        app.terminal_rows = 7; // ceiling = 7-3 = 4 < CARD_MIN_SIZE
        app.current_tab_mut().permission = Some(perm_with("ok"));
        // Must stay visible — agent flow blocks on this prompt. 1-row strip
        // is the compact fallback rendered by `ui::permission::render`.
        assert_eq!(app.permission_panel_height(80), 1);
    }

    #[test]
    fn permission_panel_height_admits_at_card_min_ceiling() {
        let mut app = test_app();
        app.terminal_rows = 8; // ceiling = 5 == CARD_MIN_SIZE
        app.current_tab_mut().permission = Some(perm_with("ok"));
        assert_eq!(app.permission_panel_height(80), CARD_MIN_SIZE);
    }

    #[test]
    fn rec_panel_height_floor_lets_tallest_card_render() {
        let mut app = test_app();
        app.terminal_rows = 20;
        let tall = "x".repeat(500);
        install_recs(&mut app, vec![rec_send(&tall)]);
        let tall_h = rec_card_height(&app.current_tab().turn.recommendations()
            .unwrap().choices[0], 80) as u16;
        // ceiling = 20 - 5 = 15; tall card is much larger; floor wins.
        assert_eq!(app.rec_panel_height(80), tall_h);
    }

    #[test]
    fn rec_panel_height_caps_at_ceiling_when_total_exceeds() {
        let mut app = test_app();
        app.terminal_rows = 30;
        // Three short cards, each h=6 → total 18; ceiling 30-5=25.
        install_recs(&mut app, vec![rec_send("a"), rec_send("b"), rec_send("c")]);
        assert_eq!(app.rec_panel_height(80), 18);
    }

    #[test]
    fn rec_panel_height_zero_when_no_recs() {
        let app = test_app();
        assert_eq!(app.rec_panel_height(80), 0);
    }

    #[test]
    fn main_area_width_reflects_debug_panel_split() {
        let mut app = test_app();
        app.terminal_cols = 100;
        assert_eq!(app.main_area_width(), 100);
        app.show_debug_panel = true;
        assert_eq!(app.main_area_width(), 60);
    }

    /// Regression: `ui::recommendations::render` used `area.width` (= `h_rec[1]`
    /// = `main_area.width - 2`) when calling `rec_card_height`, while
    /// `rec_panel_height` / `sync_rec_scroll_max` used `main_area.width`. The
    /// 2-cell desync clipped the bottom card and undercounted scroll bounds
    /// whenever a card's wrap row count differed between the two widths.
    ///
    /// This test pins both code paths to `main_area.width`, and picks a
    /// text length that lies in the critical window `(W-10, W-8]` so the
    /// old buggy width (`W-2`, content `W-10`) would wrap to a different
    /// row count than the correct width (`W`, content `W-8`).
    #[test]
    fn rec_card_height_matches_predict_and_render_paths() {
        let w: u16 = 50;
        // text length 42 sits exactly at the boundary: fits on 1 row at
        // inner_width 42 (W=50, chrome=8), but spills to 2 rows at
        // inner_width 40 (the old buggy basis).
        let text = "a".repeat(42);
        let choice = rec_send(&text);
        let mut app = test_app();
        app.terminal_cols = w;
        app.terminal_rows = 30;
        install_recs(&mut app, vec![choice.clone()]);

        let predict = app.rec_panel_height(app.main_area_width()) as usize;
        // Same width the renderer now uses (`app.main_area_width()`).
        let render = rec_card_height(&choice, app.main_area_width());
        assert_eq!(predict, render);

        // Sanity: confirm the chosen text *is* a sensitive input — i.e. the
        // old buggy basis (h_rec[1] width = W-2) would have produced a
        // different height. If this ever fails the test no longer guards
        // the regression.
        let buggy = rec_card_height(&choice, app.main_area_width() - 2);
        assert_ne!(render, buggy,
            "text length 42 should wrap differently at width 50 vs 48 — \
             pick a different critical input");
    }
}
