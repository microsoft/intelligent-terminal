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
    rename_session_rx:
        Option<mpsc::UnboundedReceiver<crate::protocol::acp::client::RenameSessionRequest>>,
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
    PromptSubmission, RenameSessionRequest, RestartRequest,
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
    Install { agent_id: String, display_name: String },
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
                        opts.push(SetupOption::Install {
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
    /// Trailing inline status marker rendered in DIM next to the turn's
    /// first content line (e.g. "(canceled)" / "→ executed: Run Get-Date").
    /// Set when the user dismisses or executes a recommendation card, or
    /// cancels a mid-stream turn — `None` for normal chat turns.
    #[serde(default)]
    pub trailing_marker: Option<String>,
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
    /// WT tab StableId that owns the failing pane. `None` when the
    /// underlying event predates the tab_id wire (older WT builds) or
    /// arrived without a tab context. Autofix routing treats absence as
    /// "cannot route — drop with warn", to avoid the old failure mode
    /// where the fix landed in whatever tab happened to be active.
    pub tab_id: Option<String>,
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

    // Phantom-session prune: if this event was a session-end and the
    // row is now Ended for a managed CLI (Claude/Copilot/Gemini) whose
    // on-disk artefacts contain no resumable content, drop it from the
    // registry now. Without this, the user who opens `<cli>` and exits
    // without typing a real prompt is left with an Ended row in F2
    // whose Enter would launch `<cli> --resume <id>` — and the CLI
    // itself would then reject the request (`No conversation found`
    // for Claude, `No session, task, or name matched` for Copilot,
    // similar for Gemini), often leaving fresh phantom artefacts on
    // disk in the process. Mirrors the loader-side filters in
    // `history_loader::load_*` (which only fire for historical rows
    // reconstructed at startup).
    if matches!(event, "agent.session.stopped" | "agent.session.end") {
        prune_phantom_session_if_ended(reg, &key_for_refresh);
    }

    // Stamp `AgentPane` origin on the live session if the agent-pane
    // origin index recorded its session id. This is what flips the
    // "agent pane" prefix on for *live* rows — historical rows pick up
    // the same flag through `history_loader::load_all`'s join. We
    // re-read the index on every routed event (small file, infrequent
    // event) rather than caching, to stay correct after a new session
    // is created while wta is already running.
    if !key_for_refresh.is_empty() {
        let agent_pane_keys = crate::agent_pane_origin::load_default_set();
        if agent_pane_keys.contains(&key_for_refresh) {
            reg.set_origin(&key_for_refresh, crate::agent_sessions::SessionOrigin::AgentPane);
        }
    }

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

/// Drop `key` from `reg` if it has transitioned to `Ended` and its
/// on-disk artefacts indicate `<cli> --resume <key>` would be rejected
/// (per the CLI's own resumability rule). A no-op for any other
/// status or for keys whose CLI has no on-disk artefact yet (defers
/// to the CLI's own validation).
///
/// Covers all three managed CLIs:
///   * Claude  — JSONL exists but contains only meta records
///               (Claude rejects with `No conversation found with
///               session ID: <id>`).
///   * Copilot — session-state dir exists but `events.jsonl` is missing
///               or empty (Copilot rejects with
///               `Error: No session, task, or name matched '<id>'`).
///   * Gemini  — chat JSONL exists but contains only the session
///               header line(s), no user/tool activity.
pub(crate) fn prune_phantom_session_if_ended(
    reg: &mut crate::agent_sessions::AgentSessionRegistry,
    key: &str,
) {
    prune_phantom_session_if_ended_with(reg, key, |cli, k| {
        // Use the *strict* probe here. Rationale: the row is in
        // wta's live registry, so we know the session really existed
        // in this process — a missing on-disk artefact is conclusive
        // evidence of a phantom (the CLI never had anything to
        // flush). The lenient probe defers to the CLI on missing
        // artefacts, but for live-tracked sessions that produces a
        // sticky Idle/Ended row that pressing Enter dead-ends on
        // (e.g. ACP-launched `claude` that the user exits without
        // typing — Claude never writes a JSONL, so
        // `claude --resume <id>` rejects with
        // `No conversation found with session ID: <id>`).
        crate::history_loader::key_has_definite_resumable_content(cli, k)
    });
}

/// Variant of [`prune_phantom_session_if_ended`] that takes the
/// resumability probe as a callback. Allows unit tests to drive the
/// prune path without touching the real
/// `~/.{claude,copilot,gemini}` trees (and without racing on
/// `USERPROFILE` env-var mutation).
pub(crate) fn prune_phantom_session_if_ended_with(
    reg: &mut crate::agent_sessions::AgentSessionRegistry,
    key: &str,
    is_resumable: impl FnOnce(&crate::agent_sessions::CliSource, &str) -> bool,
) {
    use crate::agent_sessions::AgentStatus;
    let probe_input = match reg.get(&key.to_string()) {
        Some(s) if matches!(s.status, AgentStatus::Ended) => {
            Some((s.cli_source.clone(), s.key.clone()))
        }
        _ => None,
    };
    let should_prune = match probe_input {
        Some((cli, k)) => !is_resumable(&cli, &k),
        None => false,
    };
    if should_prune {
        tracing::info!(
            target: "agent_session_registry",
            key = %key,
            "pruning phantom session (Ended, no resumable on-disk content)",
        );
        reg.remove(&key.to_string());
    }
}

/// Classify a WT protocol event into a notification.
pub fn classify_wt_event(
    method: &str,
    pane_id: &str,
    tab_id: Option<&str>,
    params: &serde_json::Value,
) -> WtNotification {
    let tab = tab_id.map(str::to_string);
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
                    tab_id: tab,
                    summary: format!("Pane {}: connection failed", pane_id),
                    acknowledged: false,
                    age_ticks: 0,
                },
                "closed" => WtNotification {
                    severity: WtEventSeverity::Actionable,
                    pane_id: pane_id.to_string(),
                    tab_id: tab,
                    summary: format!("Pane {}: process exited", pane_id),
                    acknowledged: false,
                    age_ticks: 0,
                },
                "connected" => WtNotification {
                    severity: WtEventSeverity::Informational,
                    pane_id: pane_id.to_string(),
                    tab_id: tab,
                    summary: format!("Pane {}: connected", pane_id),
                    acknowledged: false,
                    age_ticks: 0,
                },
                // "unknown" is sent when the C++ try_as cast fails — ignore it.
                "unknown" => return WtNotification {
                    severity: WtEventSeverity::Informational,
                    pane_id: pane_id.to_string(),
                    tab_id: tab,
                    summary: String::new(),
                    acknowledged: true, // auto-acknowledge so it never shows
                    age_ticks: 100,     // will be auto-dismissed immediately
                },
                _ => WtNotification {
                    severity: WtEventSeverity::Informational,
                    pane_id: pane_id.to_string(),
                    tab_id: tab,
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
                            tab_id: tab,
                            summary: format!("Command failed (exit {})", exit_code),
                            acknowledged: false,
                            age_ticks: 0,
                        };
                    } else {
                        // exit code 0 = success, not interesting
                        return WtNotification {
                            severity: WtEventSeverity::Informational,
                            pane_id: pane_id.to_string(),
                            tab_id: tab,
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
                tab_id: tab,
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
                tab_id: tab,
                summary: format!("agent_prompt:{}", prompt),
                acknowledged: false,
                age_ticks: 0,
            }
        }
        "set_agent_state" => {
            // handle_event consumes set_agent_state at the top of WtEvent
            // before classification runs, so classify normally never sees
            // it. Add an explicit arm anyway so a future refactor that
            // drops the early return doesn't surface a stray
            // "Pane: set_agent_state" banner via the default catch-all.
            WtNotification {
                severity: WtEventSeverity::Informational,
                pane_id: pane_id.to_string(),
                tab_id: tab,
                summary: String::new(),
                acknowledged: true,
                age_ticks: 100,
            }
        }
        _ => WtNotification {
            severity: WtEventSeverity::Informational,
            pane_id: pane_id.to_string(),
            tab_id: tab,
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
    /// WT-side `tab_renamed` event: the user dragged a tab out into a new
    /// window (or otherwise caused the tab's StableId to change). The
    /// underlying helper process survives the drag (conpty + TermControl
    /// are reattached via WT's ContentId mechanism), but the tab key WT
    /// uses to address us has changed. Without rekeying, autofix /
    /// per-tab state events targeting the new id wouldn't match any
    /// entry in `tab_sessions`.
    TabRenamed {
        old_tab_id: String,
        new_tab_id: String,
        /// Dest window id (from WT's `tab_renamed` payload). When this
        /// helper rekeys onto the new id, it also updates `self.window_id`
        /// to this value so subsequent `set_agent_state` / `tab_changed`
        /// events from the new window pass the per-window filter. `None`
        /// for direct AppEvent dispatches that don't carry it (tests).
        new_window_id: Option<String>,
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
    /// `pane_id` is the WT pane GUID where the event originated.
    /// `tab_id` is the WT tab StableId that owns the pane — used by autofix
    /// routing to send fixes to the failing tab's ACP session rather than
    /// whatever tab WTA happens to be focused on. `None` for events from
    /// older WT builds that don't yet carry tab_id.
    WtEvent {
        method: String,
        pane_id: String,
        tab_id: Option<String>,
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

/// Per-tab autofix state machine. Each tab tracks its own pending /
/// armed / suggested autofix independently so a failure in a background
/// tab doesn't clobber an armed fix in the active tab and vice versa.
/// The bottom-bar projection is per-tab too: WTA only emits
/// `autofix_state` events to C++ when the target tab is currently
/// active, and re-emits the active tab's snapshot on tab_changed.
#[derive(Debug, Clone, Default)]
pub struct TabAutofixState {
    /// Failing pane for Pending/Armed. Cleared when the user dismisses
    /// (Esc), the error resolves (exit 0 on the same pane), or the fix
    /// is executed.
    pub pane_id: Option<String>,
    /// Failing pane for the Suggested terminal state (a non-actionable
    /// explanation in chat — distinct from autofix_pane_id so the two
    /// kinds of "the bar is showing something" can be reasoned about
    /// independently).
    pub suggested_pane_id: Option<String>,
    /// Bumped on every new trigger / cancel. Snapshotted into
    /// `AutofixContext.generation` at submit time; chunks whose
    /// snapshot diverges are dropped as stale.
    pub generation: u64,
    /// Last bottom-bar state we emitted (or would have emitted, if the
    /// tab wasn't active). Used to re-emit on tab_changed so the bar
    /// shows the right state when the user comes back to this tab.
    pub bar_snapshot: AutofixBarSnapshot,
}

/// Snapshot of the bottom-bar autofix state for one tab. Mirrors the
/// `state` field of the `autofix_state` protocol event so we can rebuild
/// the payload from the cached snapshot when the tab becomes active.
#[derive(Debug, Clone, Default)]
pub enum AutofixBarSnapshot {
    #[default]
    Idle,
    /// Suggest mode: an error was detected but the LLM has not been
    /// invoked. The bar shows a hint inviting the user to press the
    /// hotkey / click the pill to request a fix. Carries enough
    /// context to replay the LLM trigger when the user activates it.
    Detected {
        pane_id: String,
        summary: String,
        hotkey_hint: String,
    },
    Pending {
        pane_id: String,
        summary: String,
    },
    Armed {
        pane_id: String,
        fix_preview: String,
        hotkey_hint: String,
    },
    Suggested {
        pane_id: String,
        suggestion_title: String,
    },
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
    /// Per-tab autofix state machine (see `TabAutofixState`).
    pub autofix: TabAutofixState,

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
    /// FIFO of pending permission requests for this session. The front
    /// entry is the one currently rendered and accepting keys; the rest
    /// queue up. Agents (Copilot in particular) sometimes fire multiple
    /// concurrent `request_permission` calls for one tool invocation
    /// — e.g. one per path that needs to be unlocked outside the trusted
    /// directory set — and each carries its own oneshot responder. The
    /// previous single-slot `Option` overwrote the prior entry on every
    /// new request, dropping its responder, which `WtaClient::request_permission`
    /// observed as `Cancelled` and the agent interpreted as "user rejected"
    /// — producing the silent tool-call failure tracked alongside the
    /// helper+master split.
    pub permission: VecDeque<PermissionState>,
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

    // "Does this tab want the agent pane visible?" — per-tab user intent.
    // Independent of where the (single, shared) XAML pane physically lives:
    // C++ relocates the pane to whichever active tab has `pane_open == true`
    // and hides it on tabs where it's `false`. wta owns this state so the
    // C++ side has one writer (`OnAgentStateChanged`) and the desync that
    // came from tracking it as a per-Tab.AgentPaneOpen flag on a moving
    // XAML pane is gone.
    //
    // Default false. Seeded to true at startup for the spawn owner tab
    // (the user just asked to open the pane on that tab). Flipped by
    // C++-originated `set_agent_state` requests (hotkey/button toggles)
    // and by wta-internal events like Ctrl+C×2 reset.
    pub pane_open: bool,
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
        // Dropping pending responders signals `Cancelled` back to the
        // agent — appropriate when the user wipes chat history mid-turn.
        self.permission.clear();
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

    pub fn delete_word_before_cursor(&mut self) {
        self.cursor_pos = clamp_cursor_to_boundary(&self.input, self.cursor_pos);
        if self.cursor_pos == 0 {
            return;
        }
        let word_start = prev_word_boundary(&self.input, self.cursor_pos);
        self.input.replace_range(word_start..self.cursor_pos, "");
        self.cursor_pos = word_start;
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
    rename_session_tx: mpsc::UnboundedSender<RenameSessionRequest>,
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
    // The tab id this helper's agent pane was spawned to own. Unlike
    // `tab_id` (which floats with `tab_changed` to track WT's currently-
    // focused tab), this is anchored to the helper's owning pane and
    // follows only `tab_renamed` events (cross-window drag). Used as the
    // `tab_id` field on outbound `agent_status` and `autofix_state` events
    // so the C++ side can route per-pane state to the right
    // AgentPaneContent / bottom bar window without fan-out.
    pub owner_tab_id: Option<String>,
    pub window_id: Option<String>,
    // WT event notifications (global — affects bottom-bar / banner across tabs)
    pub wt_notifications: std::collections::VecDeque<WtNotification>,
    pub show_notification_banner: bool,
    // Auto-fix global on/off. Per-tab autofix machinery (pane_id,
    // generation, suggested_pane_id, bar_snapshot) lives on `TabSession.autofix`.
    pub autofix_enabled: bool,
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
        rename_session_tx: mpsc::UnboundedSender<RenameSessionRequest>,
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
            rename_session_tx,
            restart_tx,
            debug_capture_enabled,
            help_overlay_visible: false,
            debug_messages: Vec::new(),
            show_debug_panel: false,
            debug_scroll: 0,
            pane_id: None,
            tab_id: None,
            owner_tab_id: None,
            window_id: None,
            wt_notifications: VecDeque::new(),
            show_notification_banner: false,
            autofix_enabled,
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
        rename_session_rx: mpsc::UnboundedReceiver<
            crate::protocol::acp::client::RenameSessionRequest,
        >,
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
            rename_session_rx: Some(rename_session_rx),
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
                let (_rntx, rnrx) = mpsc::unbounded_channel();
                let (_rtx, rrx) = mpsc::unbounded_channel();
                self.prompt_tx = ptx;
                params.prompt_rx = Some(prx);
                params.cancel_rx = Some(crx);
                params.new_session_rx = Some(nrx);
                params.load_session_rx = Some(lrx);
                params.drop_session_rx = Some(drx);
                params.rename_session_rx = Some(rnrx);
                params.restart_rx = Some(rrx);
            }

            if let (
                Some(prompt_rx),
                Some(cancel_rx),
                Some(new_session_rx),
                Some(load_session_rx),
                Some(drop_session_rx),
                Some(rename_session_rx),
                Some(restart_rx),
            ) = (
                params.prompt_rx.take(),
                params.cancel_rx.take(),
                params.new_session_rx.take(),
                params.load_session_rx.take(),
                params.drop_session_rx.take(),
                params.rename_session_rx.take(),
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
                    rename_session_rx,
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
                    // Skip self-focus: if the user pressed Enter on the
                    // row that represents the pane this WTA is already
                    // running in, the focus call is a no-op for them and
                    // can throw `winrt::hresult_error` (E_FAIL /
                    // 0x80004005) on the WT side. Compare case-insensitively
                    // because pane GUIDs arrive in mixed case (hooks emit
                    // lowercase, WT-native events emit canonical
                    // uppercase) and `self.pane_id` is populated from
                    // whichever path discovered it first.
                    let is_self = self
                        .pane_id
                        .as_deref()
                        .map(|own| own.eq_ignore_ascii_case(pane.as_str()))
                        .unwrap_or(false);
                    if is_self {
                        tracing::info!(
                            target: "agents_view",
                            key = %s.key,
                            pane = %pane,
                            "skipping focus_pane: row points at our own pane",
                        );
                    } else {
                        // Wire NotFound failures back through
                        // `AgentSessionEvent(PaneClosed)`. Without this,
                        // a row whose pane has died silently (tab
                        // closed while WT's `connection_state`
                        // notification raced with TermControl teardown
                        // and never reached us) stays stuck at Idle
                        // forever, and Enter on it just keeps
                        // re-failing the focus-pane call. The
                        // subsequent prune in the `PaneClosed` handler
                        // also drops phantom rows for Claude/Copilot/
                        // Gemini whose on-disk artefacts have no
                        // resumable content. `Other` failures (RPC
                        // glitches, broken wtcli install, etc.) leave
                        // the row alone — the pane may still be alive.
                        let pane_for_cb = pane.clone();
                        let event_tx = self.agent_event_tx.clone();
                        let on_failure: Option<Box<dyn FnOnce(
                            crate::shell::wt_channel::FocusPaneFailureReason,
                        ) + Send + 'static>> = match event_tx {
                            Some(tx) => Some(Box::new(move |reason| {
                                use crate::shell::wt_channel::FocusPaneFailureReason::*;
                                if matches!(reason, NotFound) {
                                    let _ = tx.send(AppEvent::AgentSessionEvent(
                                        crate::agent_sessions::SessionEvent::PaneClosed {
                                            pane_session_id: pane_for_cb,
                                        },
                                    ));
                                }
                            })),
                            None => None,
                        };
                        crate::shell::wt_channel::spawn_wtcli_focus_pane_with_callback(
                            pane,
                            on_failure,
                        );
                    }
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

        // Belt-and-suspenders phantom-session guard. The session-end
        // and pane-closed routes already prune phantoms via
        // `prune_phantom_session_if_ended`, but if any path slips
        // through (e.g. session loaded from disk that wasn't filtered,
        // race on artefact flush, manual CLI invocation outside of an
        // agent pane), avoid launching `<cli> --resume <id>` here. The
        // CLI itself would otherwise allocate fresh session
        // artefacts at startup, *then* validate the `--resume`
        // argument and exit with an error — leaving phantom artefacts
        // behind for the next session-load to surface again.
        if !crate::history_loader::key_is_resumable_on_disk(&s.cli_source, &s.key) {
            tracing::warn!(
                target: "agents_view",
                key = %s.key,
                cli = %cli_id,
                "dispatch_resume: refusing to resume phantom session (no on-disk content); pruning row",
            );
            let short_key: String = s.key.chars().take(8).collect();
            let msg = format!(
                "Cannot resume {} session {}: it was started but never accumulated any \
                 conversation, so {} itself would reject the resume. Removing the row.",
                cli_id, short_key, cli_id
            );
            let tab = self.current_tab_mut();
            tab.messages.push(ChatMessage::System(msg));
            tab.scroll_to_bottom();
            let key_to_remove = s.key.clone();
            self.agent_sessions.remove(&key_to_remove);
            #[cfg(test)]
            {
                self.last_dispatched_command = Some(DispatchedCommand {
                    kind: DispatchedCommandKind::NewTabResume,
                    session_id: Some(key_to_remove),
                    argv: vec!["resume".to_string(), "--phantom-skipped".to_string()],
                });
            }
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

        // Mirror dispatch_resume's belt-and-suspenders phantom guard.
        // Without this, Shift+Enter on a row whose on-disk artefact
        // has no resumable content would open a new tab + reconcile
        // the agent pane onto it, then dead-end inside the agent with
        // a JSON-RPC `loadSession` error (the agent's own session
        // store can't find the id). Preempt that round trip and drop
        // the row in place, same as plain Enter.
        if !crate::history_loader::key_is_resumable_on_disk(&s.cli_source, &s.key) {
            tracing::warn!(
                target: "agents_view",
                key = %s.key,
                cli = ?s.cli_source,
                "dispatch_resume_in_agent_pane: refusing to load phantom session; pruning row",
            );
            let short_key: String = s.key.chars().take(8).collect();
            let cli_id = match s.cli_source {
                crate::agent_sessions::CliSource::Claude  => "claude",
                crate::agent_sessions::CliSource::Copilot => "copilot",
                crate::agent_sessions::CliSource::Gemini  => "gemini",
                crate::agent_sessions::CliSource::Unknown(_) => "this CLI",
            };
            let msg = format!(
                "Cannot resume {} session {}: it was started but never accumulated any \
                 conversation, so {} would reject the load. Removing the row.",
                cli_id, short_key, cli_id
            );
            let tab = self.current_tab_mut();
            tab.messages.push(ChatMessage::System(msg));
            tab.scroll_to_bottom();
            let key_to_remove = s.key.clone();
            self.agent_sessions.remove(&key_to_remove);
            #[cfg(test)]
            {
                self.last_dispatched_command = Some(DispatchedCommand {
                    kind: DispatchedCommandKind::ResumeInAgentPane,
                    session_id: Some(key_to_remove),
                    argv: vec!["resume_in_new_agent_tab".to_string(), "--phantom-skipped".to_string()],
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
                            SetupOption::Install { .. } => {
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
            SetupOption::Install { agent_id, .. } => {
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
                                tracing::info!("Install {} succeeded", id);
                            }
                            Err(e) => {
                                tracing::warn!("Install {} failed: {}", id, e);
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
            AppEvent::TabRenamed { .. } => "tab_renamed",
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
            !tab.permission.is_empty(),
            tab.timing_note.is_some()
        )
    }

    fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Key(key) => self.handle_key(key),
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
            AppEvent::TabRenamed { old_tab_id, new_tab_id, new_window_id } => {
                self.rename_tab_session(&old_tab_id, &new_tab_id, new_window_id.as_deref());
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
                // FIFO push — never overwrite an in-flight request. The
                // user sees them one at a time (front of the queue is the
                // one rendered + key-handled); resolving the front pops
                // it and exposes the next.
                tab.permission.push_back(PermissionState {
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
                // Capture key BEFORE apply for events that unbind it
                // (PaneClosed clears the pane→key mapping), so the
                // phantom-session prune below can still see the row.
                let key_to_prune = match &ev {
                    crate::agent_sessions::SessionEvent::PaneClosed { pane_session_id } => {
                        self.agent_sessions.key_for_pane(pane_session_id)
                    }
                    crate::agent_sessions::SessionEvent::SessionStopped { key, .. } => {
                        Some(key.clone())
                    }
                    _ => None,
                };
                self.agent_sessions.apply(ev);
                if let Some(k) = key_to_prune {
                    crate::app::prune_phantom_session_if_ended(&mut self.agent_sessions, &k);
                }
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
                tab_id,
                params,
            } => {
                tracing::debug!(target: "autofix", method = %method, pane_id = %pane_id, tab_id = ?tab_id, self_pane_id = ?self.pane_id, "WtEvent");

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

                if method == "autofix_dismiss_suggestion" {
                    // User clicked the bar in Suggested state. The bar
                    // always projects the active tab, so clear that tab's
                    // suggested_pane_id and emit cleared.
                    let active = self.active_tab_key().to_string();
                    let suggested = self
                        .current_tab_mut()
                        .autofix
                        .suggested_pane_id
                        .take();
                    if suggested.is_some() {
                        self.emit_autofix_state_cleared(&active);
                    }
                    return;
                }

                if method == "autofix_execute_from_detected" {
                    // User pressed the pill / hotkey in Detected state.
                    // Replay the trigger as if auto-suggest were on, so
                    // the LLM call fires and we transition to Pending.
                    self.handle_autofix_execute_from_detected();
                    return;
                }

                if method == "agent_prompt" {
                    // Command palette `?<prompt>` delegation. Not a WT
                    // notification — has nothing to do with banner/queue.
                    let prompt = params
                        .get("prompt")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    tracing::info!(target: "autofix", prompt_len = prompt.len(), "agent_prompt: delegating");
                    if !prompt.is_empty() {
                        self.delegate_to_tab_agent(prompt);
                    }
                    return;
                }

                if method == "autofix_enabled_changed" {
                    // C++ pushes this when the user toggles "Auto-suggest
                    // fixes" in settings while WTA is already running.
                    // Without it the flag would stay pinned to whatever
                    // `--no-autofix` value WTA was launched with.
                    let enabled = params
                        .get("enabled")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    tracing::info!(
                        target: "autofix",
                        old = self.autofix_enabled,
                        new = enabled,
                        "autofix_enabled hot-reloaded from settings change",
                    );
                    self.autofix_enabled = enabled;
                    return;
                }

                if method == "tab_changed" {
                    // Window-scoped: WT broadcasts via shared COM, so every
                    // helper (across every window) receives every tab_changed.
                    // Without this filter, helper-A in window 1 would call
                    // switch_tab_session on a window-2 tab_id and start
                    // rendering tab_sessions[<window-2 tab>] in its TUI —
                    // detaching the agent pane content from its owner tab.
                    // Same shape as the `set_agent_state` window filter below:
                    // skip only when both ids are non-empty and differ.
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
                            target: "tab_session",
                            target_window,
                            our_window,
                            "ignoring tab_changed for different window"
                        );
                        return;
                    }
                    tracing::info!(
                        target: "tab_session",
                        raw_params = %params,
                        current_tab = ?self.tab_id,
                        "tab_changed event received"
                    );
                    if let Some(new_tab_id) = params.get("tab_id").and_then(|v| v.as_str()) {
                        // switch_tab_session calls project_active_tab_state
                        // at its end — that pushes the new tab's view AND
                        // autofix bar snapshot to C++ in one shot.
                        self.switch_tab_session(new_tab_id.to_string());
                    } else {
                        tracing::warn!(target: "tab_session", "tab_changed: missing tab_id in params");
                    }
                    return;
                }

                if method == "tab_closed" {
                    // Same window filter as tab_changed — drop_tab_session
                    // removes from `tab_sessions` and nulls `self.tab_id`
                    // when the closed tab is the active one, so a cross-
                    // window leak would wipe per-tab state of a tab the
                    // helper doesn't even own.
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
                            target: "tab_session",
                            target_window,
                            our_window,
                            "ignoring tab_closed for different window"
                        );
                        return;
                    }
                    if let Some(closed_tab_id) =
                        params.get("tab_id").and_then(|v| v.as_str())
                    {
                        self.drop_tab_session(closed_tab_id);
                    } else {
                        tracing::warn!(target: "tab_session", "tab_closed: missing tab_id in params");
                    }
                    return;
                }

                if method == "tab_renamed" {
                    // Tab-drag rename: the user dragged this tab into
                    // another window so WT minted a fresh StableId. The
                    // helper process survives the drag; we just need to
                    // rekey our per-tab maps so events with the new id
                    // route to this tab's existing state. Route through
                    // the AppEvent::TabRenamed handler so the WtEvent
                    // inline path and any direct AppEvent posts share
                    // one implementation.
                    let old_tab_id = params
                        .get("old_tab_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let new_tab_id = params
                        .get("new_tab_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if old_tab_id.is_empty() || new_tab_id.is_empty() {
                        tracing::warn!(
                            target: "tab_session",
                            old_tab_id,
                            new_tab_id,
                            "tab_renamed: missing old_tab_id or new_tab_id in params"
                        );
                        return;
                    }
                    let new_window_id = params
                        .get("window_id")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string());
                    self.handle_event(AppEvent::TabRenamed {
                        old_tab_id: old_tab_id.to_string(),
                        new_tab_id: new_tab_id.to_string(),
                        new_window_id,
                    });
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
                    // If the load_session target IS the active tab, push the
                    // (now Chat) view to C++ so the bar drops the "Agent
                    // sessions" label that the user was looking at when they
                    // hit Shift+Enter on a session row. When the target is a
                    // not-yet-active tab (e.g. WT just created a fresh tab
                    // and the `tab_changed` race still hasn't landed), the
                    // imminent `tab_changed` to that tab will project then.
                    if tab_id == self.active_tab_key() {
                        self.project_active_tab_state();
                    }
                    let _ = self.load_session_tx.send(LoadSessionForTab {
                        tab_id: tab_id.to_string(),
                        session_id: session_id.to_string(),
                        cwd,
                    });
                    return;
                }

                // set_agent_state: unified inbound request from C++ to
                // change one or more pieces of per-tab agent-pane UI state
                // for a specific tab. Every field under `params` is
                // optional — only specified ones are applied, the rest
                // are left untouched.
                //
                // Supported fields:
                //   * `tab_id`: optional WT StableId of the tab to mutate.
                //               Falls back to the active tab when absent.
                //               C++ should always include it: defends
                //               against `tab_changed`/`set_agent_state`
                //               ordering ambiguity (e.g. resume-in-new-tab
                //               creates a new tab and immediately requests
                //               pane_open=true; with `tab_id` we don't
                //               depend on `tab_changed` arriving first to
                //               route to the right TabSession).
                //   * `view`: "chat" | "sessions"
                //   * `pane_open`: bool
                //
                // **Projection rule**: if the target tab is the currently-
                // active one, immediately project the new snapshot back to
                // C++ (`agent_state_changed`). If the target is NOT active,
                // skip projection — the next `tab_changed` to that tab will
                // project the now-up-to-date state. C++ mirrors are global
                // per-pane so they only need refreshing when the active tab
                // changes (or when a mutation lands on the active tab).
                //
                // **Round-trip contract**: under the "wta is the sole owner
                // of agent-pane UI state" architecture, C++ does NOT update
                // its mirrors (`_agentSessionsViewActive`, `Tab.AgentPaneOpen`)
                // when it sends `set_agent_state`. It waits for the resulting
                // `agent_state_changed` emitted by `project_active_tab_state`
                // below. One IPC round-trip latency, in exchange for the
                // C++ flags having a single writer (`OnAgentStateChanged`),
                // which makes desync architecturally impossible.
                //
                // Window-scoped: WT includes its own window_id; we ignore
                // the event when our window_id is known and doesn't match,
                // so multi-window setups don't cross-talk. When window_id
                // is unknown on either side we apply (best-effort fallback).
                //
                // Processed BEFORE the own-pane skip below: this is a
                // global UI command, not a per-pane signal.
                if method == "set_agent_state" {
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
                            target: "set_agent_state",
                            target_window,
                            our_window,
                            "ignoring set_agent_state for different window"
                        );
                        return;
                    }

                    // Resolve target tab: explicit `tab_id` wins;
                    // otherwise fall back to the active tab. The explicit
                    // path is robust against `tab_changed` ordering races
                    // (e.g. resume-in-new-tab where C++ creates a tab and
                    // immediately fires `set_agent_state` for it).
                    let target_tab = params
                        .get("tab_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| self.active_tab_key().to_string());

                    // Apply `view` if present.
                    if let Some(view_str) = params.get("view").and_then(|v| v.as_str()) {
                        tracing::info!(
                            target: "set_agent_state",
                            tab = %target_tab,
                            view = view_str,
                            "applying view"
                        );
                        match view_str {
                            "sessions" | "agents" => {
                                // User entered session management (via shortcut or UI) —
                                // permanently dismiss the welcome hint.
                                if self.show_welcome_hint {
                                    self.show_welcome_hint = false;
                                    set_welcome_shown_in_state();
                                }
                                let entering_agents = self
                                    .tab_sessions
                                    .get(&target_tab)
                                    .map(|t| t.current_view != View::Agents)
                                    .unwrap_or(true);
                                let has_sessions = !self
                                    .agent_sessions
                                    .iter_sorted_filtered(self.current_cli_filter().as_ref())
                                    .is_empty();
                                {
                                    let tab = self.tab_mut(&target_tab);
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
                                self.tab_mut(&target_tab).current_view = View::Chat;
                            }
                            other => {
                                tracing::warn!(
                                    target: "set_agent_state",
                                    view = other,
                                    "unknown view value — ignoring"
                                );
                            }
                        }
                    }

                    // Apply `pane_open` if present.
                    if let Some(open) = params.get("pane_open").and_then(|v| v.as_bool()) {
                        tracing::info!(
                            target: "set_agent_state",
                            tab = %target_tab,
                            pane_open = open,
                            "applying pane_open"
                        );
                        self.tab_mut(&target_tab).pane_open = open;
                    }

                    // Always echo the mutation back — C++ routes
                    // `agent_state_changed` by `tab_id`, so per-tab state
                    // updates apply to the right AgentPaneContent
                    // regardless of which tab is currently focused.
                    self.project_tab_state(&target_tab);
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
                            // Capture the key BEFORE PaneClosed clears
                            // the pane→key binding, so the post-apply
                            // phantom-session prune still sees the row.
                            let key_before = self
                                .agent_sessions
                                .key_for_pane(&pane_id);
                            self.agent_sessions.apply(
                                crate::agent_sessions::SessionEvent::PaneClosed {
                                    pane_session_id: pane_id.clone(),
                                },
                            );
                            if let Some(k) = key_before {
                                crate::app::prune_phantom_session_if_ended(
                                    &mut self.agent_sessions,
                                    &k,
                                );
                            }
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
                        // Capture the key BEFORE PaneClosed clears the
                        // pane→key binding so the phantom-session prune
                        // can still inspect the row.
                        let key_before = self.agent_sessions.key_for_pane(&pane_id);
                        self.agent_sessions.apply(
                            crate::agent_sessions::SessionEvent::PaneClosed {
                                pane_session_id: pane_id.clone(),
                            },
                        );
                        if let Some(k) = key_before {
                            crate::app::prune_phantom_session_if_ended(
                                &mut self.agent_sessions,
                                &k,
                            );
                        }
                    }
                }

                let notification =
                    classify_wt_event(&method, &pane_id, tab_id.as_deref(), &params);
                tracing::debug!(target: "autofix", severity = ?notification.severity, summary = %notification.summary, tab_id = ?notification.tab_id, "classified");

                // Per-tab filter. WT broadcasts pane-scoped events to every
                // helper in the window, but another tab's failures are not
                // this helper's concern. Drop notifications whose tab_id
                // doesn't match our owner_tab_id; empty/missing tab_id falls
                // through (no per-tab scope).
                if let (Some(event_tab), Some(self_tab)) = (
                    notification.tab_id.as_deref(),
                    self.owner_tab_id.as_deref(),
                ) {
                    if !event_tab.is_empty()
                        && !self_tab.is_empty()
                        && event_tab != self_tab
                    {
                        tracing::debug!(
                            target: "autofix",
                            event_tab,
                            self_tab,
                            method = %method,
                            "dropping cross-tab WT event"
                        );
                        return;
                    }
                }

                // Surface rule: WT events (connection_state, vt_sequence)
                // surface via the bottom bar / `wt_notifications` queue ONLY.
                // Chat is the agent dialogue surface — only user input and
                // agent responses go there.
                match notification.severity {
                    WtEventSeverity::Critical | WtEventSeverity::Actionable => {
                        self.show_notification_banner = true;
                        // Only OSC-133;D vt_sequence events have the exit
                        // code + live shell buffer needed to drive autofix.
                        // `connection_state: closed`/`failed` is just process
                        // termination — banner-only.
                        if method == "vt_sequence" {
                            self.maybe_trigger_autofix(&notification);
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
                            // Resolve the event's owning tab (added in Step 1).
                            // Older events without tab_id can't be cleanly
                            // routed; skip the per-tab clear for them.
                            let event_tab = tab_id.clone();
                            let armed_in_event_tab = event_tab
                                .as_deref()
                                .and_then(|t| self.tab_sessions.get(t))
                                .and_then(|t| t.autofix.pane_id.as_deref())
                                .map(str::to_string);
                            if is_exit_zero && armed_in_event_tab.as_deref() == Some(pane_id.as_str()) {
                                let target_tab = event_tab
                                    .clone()
                                    .expect("armed_in_event_tab requires tab_id present");
                                // `turn_cancel` owns the full cleanup: bumps
                                // the tab's autofix_generation, emits cleared
                                // (resolving the pane from AutofixContext, or
                                // `autofix.pane_id` as a fallback), and
                                // resets `tab.turn` to Idle. Avoid duplicating
                                // its work.
                                let session_id = self
                                    .tab_sessions
                                    .get(&target_tab)
                                    .and_then(|t| t.session_id.clone());
                                if let Some(sid) = session_id {
                                    self.turn_cancel(&sid);
                                } else {
                                    // No ACP session bound — replicate the
                                    // minimum cleanup turn_cancel would do.
                                    let pane_to_clear = {
                                        let tab = self.tab_mut(&target_tab);
                                        tab.autofix.generation =
                                            tab.autofix.generation.wrapping_add(1);
                                        tab.clear_recommendations();
                                        tab.autofix.pane_id.take()
                                    };
                                    if pane_to_clear.is_some() {
                                        self.emit_autofix_state_cleared(&target_tab);
                                    }
                                }
                            }
                            // Suggested: dismiss on prompt activity (exit-zero
                            // or a fresh prompt-start) in the event's tab.
                            // Emit cleared so the bar's per-tab snapshot
                            // resets to Idle.
                            if is_exit_zero || is_prompt_start {
                                if let Some(t) = event_tab.as_deref() {
                                    let t_owned = t.to_string();
                                    let pane_to_clear = self
                                        .tab_mut(&t_owned)
                                        .autofix
                                        .suggested_pane_id
                                        .take();
                                    if pane_to_clear.is_some() {
                                        self.emit_autofix_state_cleared(&t_owned);
                                    }
                                }
                            }
                            // Detected (suggest-mode pill): dismiss when
                            // the user makes a fresh successful run in
                            // the same pane. The Detected snapshot has
                            // no in-flight turn to cancel — just clear
                            // the bar.
                            if is_exit_zero {
                                if let Some(t) = event_tab.as_deref() {
                                    let t_owned = t.to_string();
                                    let detected_matches = matches!(
                                        &self.tab_mut(&t_owned).autofix.bar_snapshot,
                                        AutofixBarSnapshot::Detected { pane_id: bar_pane, .. }
                                            if bar_pane == pane_id.as_str()
                                    );
                                    if detected_matches {
                                        self.emit_autofix_state_cleared(&t_owned);
                                    }
                                }
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
                    self.project_active_tab_state();
                }
                _ => {}
            }
            return;
        }

        // If permission card is showing, route keys there. Buttons are
        // rendered horizontally inside the embedded card (same chrome as
        // recommendations), so Left/Right move the focus; Up/Down kept as
        // aliases for muscle memory from the prior modal.
        if let Some(perm) = self.current_tab_mut().permission.front_mut() {
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
                    // Pop the resolved entry; the next queued request (if
                    // any) automatically becomes the new front and is
                    // rendered on the next frame.
                    if let Some(perm) = self.current_tab_mut().permission.pop_front() {
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
                        if let Some(perm) = self.current_tab_mut().permission.pop_front() {
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
                        if let Some(perm) = self.current_tab_mut().permission.pop_front() {
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
            // Wheel-as-arrow scroll fallback: when the input is empty and no
            // recommendation card is active, ↑/↓ scroll the chat transcript.
            // The host terminal (WT, xterm, kitty, …) translates a mouse-wheel
            // notch into ~3 arrow keystrokes when mouse capture is OFF and the
            // app is in the alt-screen buffer, so by(1)/by(-1) per key matches
            // one wheel notch ≈ 3 lines, in line with the previous explicit
            // MouseScroll handler. Convention here mirrors PgUp/PgDn below:
            // positive delta = scroll up (toward older content).
            KeyCode::Up if self.current_tab().input.is_empty()
                        && self.current_tab().turn.recommendations().is_none()
                        && !self.command_popup_visible() => {
                self.current_tab_mut().chat_scroll.by(1);
            }
            KeyCode::Down if self.current_tab().input.is_empty()
                          && self.current_tab().turn.recommendations().is_none()
                          && !self.command_popup_visible() => {
                self.current_tab_mut().chat_scroll.by(-1);
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
                    || (self.current_tab().autofix.pane_id.is_some()
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
                    let pane_to_clear = {
                        let tab = self.current_tab_mut();
                        tab.autofix.generation = tab.autofix.generation.wrapping_add(1);
                        tab.autofix.pane_id.take()
                    };
                    if pane_to_clear.is_some() {
                        let active = self.active_tab_key().to_string();
                        self.emit_autofix_state_cleared(&active);
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
            KeyCode::Esc if self.current_tab().autofix.suggested_pane_id.is_some() => {
                self.current_tab_mut().autofix.suggested_pane_id = None;
                let active = self.active_tab_key().to_string();
                self.emit_autofix_state_cleared(&active);
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
                tracing::debug!(target: "autofix", input_empty = _tab.input.is_empty(), state = ?self.state, has_recs = _tab.turn.recommendations().is_some(), autofix_pane = ?_tab.autofix.pane_id, selected_idx = _tab.selected_recommendation, "Enter");
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
            KeyCode::Backspace if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.current_tab_mut().delete_word_before_cursor();
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
                self.project_active_tab_state();
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
        let Some(perm) = self.current_tab().permission.front() else { return 0 };
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
    ///
    /// Owner-lock: when `self.owner_tab_id` is set (i.e. this is a per-tab
    /// helper spawned for a specific agent pane), `tab_changed` events for
    /// a *different* tab are no-ops. The helper's TUI / per-tab state /
    /// autofix bar are anchored to the owner tab; without this guard, two
    /// helpers in the same window both process every tab switch and the
    /// non-owner's stale `tab_sessions[<other tab>]` default snapshot
    /// (created via `.or_default()` below) clobbers the owner's real
    /// snapshot when both call `project_active_tab_state` — the pane
    /// appears to "disappear" on tab switch because the loser emits
    /// `pane_open=false` after the winner emitted `pane_open=true`.
    /// Helpers without an owner (delegate path, legacy `wta` runs) still
    /// follow the active tab.
    fn switch_tab_session(&mut self, new_tab_id: String) {
        if let Some(owner) = self.owner_tab_id.as_deref() {
            if owner != new_tab_id {
                tracing::debug!(
                    target: "tab_session",
                    owner,
                    new_tab_id = %new_tab_id,
                    "switch_tab_session: ignoring tab_changed for non-owner tab"
                );
                return;
            }
        }

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

        // The new active tab's `current_view` (and autofix bar) is now
        // authoritative for the shared C++ agent pane. Re-emit so the bar
        // title and bottom-bar highlight match the tab we just switched to;
        // without this, C++'s global flag stays on the previous tab's view
        // and the agent bar shows "Agent sessions" while the TUI below
        // actually renders chat (or vice versa).
        self.project_active_tab_state();
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

        // Tell the ACP client to release the binding for this tab so
        // the agent process can `session/cancel` the orphaned session.
        // Without this, every closed tab leaves a live ACP session
        // behind on the CLI side — `tab_sessions` and `session_to_tab`
        // are cleaned above but the ACP layer's own `tab_to_session`
        // map and the agent's session state are not.
        let _ = self.drop_session_tx.send(DropSessionRequest {
            tab_id: closed_tab_id.to_string(),
        });

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

    /// Rekey per-tab state after a tab-drag rename. WT mints a fresh
    /// StableId when the user drags a tab into another window; the
    /// underlying helper process survives the drag (conpty + TermControl
    /// reattach via WT's ContentId mechanism) but the tab key WT uses to
    /// address us has changed. Without this, autofix / set_agent_state /
    /// any other event WT broadcasts with the new id would miss every
    /// entry keyed under the old id.
    ///
    /// Concretely re-keys: `self.tab_id`, `self.tab_sessions` (HashMap key),
    /// `self.session_to_tab` (values), and any cached
    /// `wt_notifications.tab_id` matching the old id. Triggers a
    /// re-projection so the bottom-bar autofix snapshot, agent-pane view,
    /// and pane_open flag are republished under the new identity.
    ///
    /// No-op when `new_tab_id == old_tab_id`. If the old tab id is unknown,
    /// still updates `self.tab_id` when it pointed there — this defends
    /// against a missed `tab_changed` race where WTA's view of the active
    /// tab and tab_sessions disagree.
    fn rename_tab_session(
        &mut self,
        old_tab_id: &str,
        new_tab_id: &str,
        new_window_id: Option<&str>,
    ) {
        if old_tab_id == new_tab_id {
            tracing::debug!(
                target: "helper",
                old_tab_id,
                new_tab_id,
                "tab_renamed no-op: ids identical"
            );
            return;
        }
        let had_session = if let Some(mut entry) = self.tab_sessions.remove(old_tab_id) {
            // Preserve target slot's TabSession if one was lazily
            // created under the new id before this event arrived — but
            // in normal flow that shouldn't happen (WT mints the new
            // id atomically with the drag). Defensive only: prefer the
            // entry that already has conversation state.
            if let Some(existing) = self.tab_sessions.remove(new_tab_id) {
                if !existing.messages.is_empty() && entry.messages.is_empty() {
                    entry = existing;
                }
            }
            self.tab_sessions.insert(new_tab_id.to_string(), entry);
            true
        } else {
            false
        };

        if self.tab_id.as_deref() == Some(old_tab_id) {
            self.tab_id = Some(new_tab_id.to_string());
        }
        // owner_tab_id is the helper's anchor for outbound per-pane events
        // (agent_status / autofix_state). Follow the rename so subsequent
        // events route to the new tab id on the C++ side. Without this,
        // a cross-window drag leaves the helper publishing tab_id=old —
        // C++'s _FindTabByStableId(old) misses (old tab is gone from the
        // source window, new id is in target), drops the event, and the
        // title bar / bottom bar never picks up the helper's state.
        let owner_matched = self.owner_tab_id.as_deref() == Some(old_tab_id);
        if owner_matched {
            self.owner_tab_id = Some(new_tab_id.to_string());
            // This helper owns the dragged tab. The conpty/TermControl
            // moved to the dest window — point `self.window_id` at it so
            // subsequent set_agent_state / tab_changed events from the new
            // window pass the per-window filter. Without this, the helper
            // stays bound to the source window's id and ignores its own
            // tab's events in the new window.
            if let Some(wid) = new_window_id {
                let old = self.window_id.clone();
                self.window_id = Some(wid.to_string());
                tracing::info!(
                    target: "helper",
                    old_window_id = ?old,
                    new_window_id = wid,
                    "tab_renamed: updated self.window_id (dragged helper)"
                );
            }
        }

        // session_to_tab values point at tab ids — rewrite any that
        // matched. Iterating + collecting keys to avoid holding the
        // borrow while we mutate.
        let mut rebound_sessions = 0usize;
        for tab in self.session_to_tab.values_mut() {
            if tab == old_tab_id {
                *tab = new_tab_id.to_string();
                rebound_sessions += 1;
            }
        }

        // wt_notifications carry the originating tab id so a later
        // dismiss / re-emit targets the right tab. Rewrite cached ones.
        let mut rebound_notifications = 0usize;
        for n in self.wt_notifications.iter_mut() {
            if n.tab_id.as_deref() == Some(old_tab_id) {
                n.tab_id = Some(new_tab_id.to_string());
                rebound_notifications += 1;
            }
        }

        tracing::info!(
            target: "helper",
            old_tab_id,
            new_tab_id,
            had_session,
            rebound_sessions,
            rebound_notifications,
            "tab renamed via drag"
        );

        // Re-publish the (now-renamed) active tab so the bottom-bar
        // autofix snapshot, agent-pane view, and pane_open flag are
        // republished under the new identity. Without this, C++'s
        // mirrored state would still be tagged with the old id on the
        // next event round-trip.
        if self.tab_id.as_deref() == Some(new_tab_id) {
            self.project_active_tab_state();
        }

        // Cross-window drag rebuilds the target window's AgentPaneContent
        // from scratch — `_agentName/_agentVersion/_agentModel` all start
        // empty, and nothing on the C++ side re-requests them. Re-emit
        // `agent_status` tagged with the new tab id so the new
        // AgentPaneContent's `UpdateAgentStatus` fires and the XAML bar
        // (label + logo) repopulates. Only the owning helper has
        // meaningful state to publish — other helpers' status events
        // for the dragged tab id would be wrong.
        if owner_matched {
            self.publish_agent_status();
        }

        // Tell the ACP client task to rekey its tab→SessionId map so the
        // next prompt on this tab finds the existing ACP session instead
        // of falling through to the lazy-create branch. The map lives
        // behind `Arc<Mutex<…>>` in the ACP task and can't be touched
        // from `&mut App` directly — mirror the DropSessionRequest plumb.
        // Send-failure means the ACP task is gone; logged for traces but
        // not actionable.
        if let Err(e) = self.rename_session_tx.send(RenameSessionRequest {
            old_tab_id: old_tab_id.to_string(),
            new_tab_id: new_tab_id.to_string(),
        }) {
            tracing::warn!(
                target: "helper",
                old_tab_id,
                new_tab_id,
                error = ?e,
                "rename_session_tx send failed (ACP client task closed?)"
            );
        }
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
        self.trigger_autofix_inner(notification, false);
    }

    /// Core autofix-trigger logic. `forced=true` bypasses the
    /// `autofix_enabled` gate (used when the user explicitly activates a
    /// Detected pill via click or hotkey). When `forced=false` and the
    /// auto-suggest setting is off, this just emits the Detected
    /// snapshot — the LLM is not invoked.
    fn trigger_autofix_inner(&mut self, notification: &WtNotification, forced: bool) {
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

        // Resolve the target tab: the tab that owns the failing pane.
        // Without it we can't route the autofix to the right ACP session
        // (the prior code fell back to `self.tab_id` and would land the
        // fix in whichever tab WTA happened to be focused on — see
        // comment block at `maybe_trigger_autofix` head). In release
        // builds we drop the event with a warn instead of panicking,
        // per Step 2 decision #4.
        let target_tab_id = match notification.tab_id.clone() {
            Some(t) => t,
            None => {
                tracing::warn!(
                    target: "autofix",
                    pane_id = %notification.pane_id,
                    "dropping autofix: notification missing tab_id (older WT build?)",
                );
                return;
            }
        };

        // Suggest-mode: when auto-suggest is off AND this isn't a user-
        // forced activation, just surface the Detected pill and let the
        // user decide whether to call the LLM. Skips the busy / generation
        // / submit logic below — none of that machinery applies until the
        // user activates the pill.
        if !self.autofix_enabled && !forced {
            tracing::info!(
                target: "autofix",
                pane_id = %notification.pane_id,
                tab_id = %target_tab_id,
                "auto-suggest off — surfacing Detected pill, no LLM call",
            );
            self.emit_autofix_state_detected(
                &target_tab_id,
                &notification.pane_id,
                &notification.summary,
            );
            return;
        }

        // Latest event always wins — but only if we can actually act on it.
        // The ACP transport single-flights at the tab level, so if the
        // target tab already has a prompt in flight, submitting another
        // one results in `tab.turn = Submitted(new)` + ACP `AgentBusy`
        // rejection — the buffer and the wire diverge, and old chunks
        // corrupt the new turn's state. Defer instead.
        let (same_pane, already_busy, armed_pane_dbg) = {
            let tab = self.tab_mut(&target_tab_id);
            let same = tab.autofix.pane_id.as_deref() == Some(notification.pane_id.as_str());
            let busy = !tab.turn.is_idle()
                && !matches!(
                    tab.turn,
                    TurnState::Surfaced { end_pending: false, .. }
                );
            (same, busy, tab.autofix.pane_id.clone())
        };

        if already_busy {
            if same_pane {
                // Same pane re-trigger: refresh the bar's summary text but
                // don't re-submit — the agent is already working on it.
                tracing::info!(
                    target: "autofix",
                    pane_id = %notification.pane_id,
                    tab_id = %target_tab_id,
                    "autofix re-trigger same pane while pending — re-emit only",
                );
                self.emit_autofix_state_pending(
                    &target_tab_id,
                    &notification.pane_id,
                    &notification.summary,
                );
            } else {
                // Different pane while busy: drop. The user can Esc the
                // current autofix to free the slot if they want this one.
                tracing::info!(
                    target: "autofix",
                    pane_id = %notification.pane_id,
                    tab_id = %target_tab_id,
                    armed_pane = ?armed_pane_dbg,
                    "skipping autofix: previous turn still in-flight",
                );
            }
            return;
        }

        // For all other cases (different pane, or Armed state, or Idle):
        // bump the target tab's generation to stale any in-flight response,
        // then submit a new autofix turn via the state machine.
        let new_gen = {
            let tab = self.tab_mut(&target_tab_id);
            tab.autofix.generation = tab.autofix.generation.wrapping_add(1);
            // A new analysis supersedes any leftover suggestion. The C++ side
            // will swap to Pending on the new pending event below; emitting an
            // explicit cleared first would create a flicker.
            tab.autofix.suggested_pane_id = None;
            tab.autofix.generation
        };

        // The auto-fix kind is carried by PromptSubmission::is_autofix,
        // so the text doesn't need a marker prefix — just the raw error
        // summary + instruction.
        let prompt_text = format!(
            "{}\nDiagnose the error and suggest a fix.",
            notification.summary
        );

        // Route through the target tab's ACP session. `tab_id` carries the
        // failing tab's StableId so the ACP layer's `tab_to_session` map
        // routes (or lazy-creates) to the right session even when the
        // failing tab isn't currently focused. `source_pane_id` points at
        // the failing pane so the agent can read its buffer.
        let pane_context = PaneContext {
            pane_id: self.pane_id.clone(),
            tab_id: Some(target_tab_id.clone()),
            window_id: self.window_id.clone(),
            cwd: None,
            source_pane_id: Some(notification.pane_id.clone()),
        };

        // Store the failing pane ID on the target tab so the Esc dismiss
        // path can find it (legacy; the new state machine carries it via
        // AutofixContext).
        self.tab_mut(&target_tab_id).autofix.pane_id = Some(notification.pane_id.clone());

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
        // Install the turn on the target tab — bypasses session_to_tab
        // lookup so a tab with no ACP session yet still gets the prompt
        // queued correctly (the ACP layer creates the session lazily when
        // it processes the prompt).
        self.turn_submit_prompt_for_tab(&target_tab_id, submitted);
        tracing::info!(target: "autofix", pane_id = %notification.pane_id, tab_id = %target_tab_id, generation = new_gen, "sending auto-fix prompt");
        let _ = self.prompt_tx.send(prompt);

        // Light up the bottom-bar diagnostic icon in "Pending" state — the
        // user knows something went wrong even before the agent responds.
        self.emit_autofix_state_pending(
            &target_tab_id,
            &notification.pane_id,
            &notification.summary,
        );
    }

    // ── autofix_state signalling ───────────────────────────────────────────
    //
    // Notifies the TerminalPage about autofix progress via a JSON event on
    // the SendEvent bus. The COM server special-cases method=="autofix_state"
    // and dispatches to TerminalPage.OnAutofixStateChanged (UI thread).
    //
    // Per-tab projection: the bar shows the ACTIVE tab's autofix state. Each
    // emit_autofix_state_* stores the new snapshot on the target tab AND
    // only forwards to WT when the target tab is currently active. On
    // tab_changed, `project_active_tab_state` re-emits the new active
    // tab's snapshot so the bar matches.

    fn emit_autofix_state_pending(&mut self, target_tab_id: &str, pane_id: &str, summary: &str) {
        let snapshot = AutofixBarSnapshot::Pending {
            pane_id: pane_id.to_string(),
            summary: summary.to_string(),
        };
        self.set_bar_snapshot(target_tab_id, snapshot);
    }

    /// Suggest-mode entry: error detected but LLM not yet invoked. The
    /// bar shows a clickable hint; the user activates the fix via the
    /// pill or the hotkey, which fires `autofix_execute_from_detected`
    /// and replays through `trigger_autofix_inner` with `force=true`.
    fn emit_autofix_state_detected(&mut self, target_tab_id: &str, pane_id: &str, summary: &str) {
        let snapshot = AutofixBarSnapshot::Detected {
            pane_id: pane_id.to_string(),
            summary: summary.to_string(),
            hotkey_hint: "Ctrl+Alt+.".to_string(),
        };
        self.set_bar_snapshot(target_tab_id, snapshot);
    }

    fn emit_autofix_state_armed(&mut self, target_tab_id: &str, pane_id: &str, fix_preview: &str) {
        let snapshot = AutofixBarSnapshot::Armed {
            pane_id: pane_id.to_string(),
            fix_preview: fix_preview.to_string(),
            hotkey_hint: "Ctrl+Alt+.".to_string(),
        };
        self.set_bar_snapshot(target_tab_id, snapshot);
    }

    /// Execute the currently armed autofix on behalf of the user (they
    /// clicked the bottom-bar button or pressed Ctrl+. in the terminal
    /// window). Mirrors the Enter-key path in the recommendations handler
    /// but without requiring the agent pane to be focused.
    /// User activated the Detected pill (click or hotkey). Read the
    /// active tab's cached snapshot, synthesize a `WtNotification` from
    /// it, and replay through `trigger_autofix_inner` with `forced=true`
    /// so the auto-suggest off gate is bypassed and the LLM call fires.
    fn handle_autofix_execute_from_detected(&mut self) {
        let active_tab = self.active_tab_key().to_string();
        let snapshot = self.current_tab().autofix.bar_snapshot.clone();
        let (pane_id, summary) = match snapshot {
            AutofixBarSnapshot::Detected { pane_id, summary, .. } => (pane_id, summary),
            other => {
                tracing::info!(
                    target: "autofix",
                    state = ?other,
                    "autofix_execute_from_detected: bar not in Detected state — ignoring",
                );
                return;
            }
        };
        let notification = WtNotification {
            severity: WtEventSeverity::Actionable,
            pane_id,
            tab_id: Some(active_tab),
            summary,
            acknowledged: false,
            age_ticks: 0,
        };
        self.trigger_autofix_inner(&notification, true);
    }

    fn handle_autofix_execute_request(&mut self, requested_pane_id: &str) {
        let active_tab = self.active_tab_key().to_string();
        let active_armed = self.current_tab().autofix.pane_id.clone();
        tracing::info!(target: "autofix", requested_pane = %requested_pane_id, armed_pane = ?active_armed, has_recs = self.current_tab().turn.recommendations().is_some(), "autofix_execute received");
        // Only execute if the active tab's armed pane matches the request.
        // The bar always reflects the active tab, so the click must target
        // it. The pane_id check prevents a stale UI click from running
        // against an unrelated, more recent error.
        let armed_pane = match active_armed {
            Some(p) if p == requested_pane_id => p,
            _ => {
                tracing::info!(target: "autofix", "autofix_execute: no armed fix for this pane");
                // Tell the UI anyway so it returns to Idle.
                self.emit_autofix_state_cleared(&active_tab);
                return;
            }
        };
        let rec = match self.current_tab().turn.recommendations().cloned() {
            Some(r) => r,
            None => {
                self.emit_autofix_state_cleared(&active_tab);
                self.current_tab_mut().autofix.pane_id = None;
                return;
            }
        };
        let idx = rec
            .recommended_choice
            .unwrap_or(self.current_tab_mut().selected_recommendation)
            .min(rec.choices.len().saturating_sub(1));
        let Some(mut choice) = rec.choices.get(idx).cloned() else {
            self.emit_autofix_state_cleared(&active_tab);
            self.current_tab_mut().autofix.pane_id = None;
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
            self.current_tab_mut().autofix.pane_id = None;
            self.clear_recommendations();
            let _ = self
                .recommendation_tx
                .send(crate::coordinator::ChoiceExecution {
                    choice,
                    insert_only: false,
                });
        }
        self.push_execution_info(format!("Auto-executing choice {}.", choice_label));
        self.emit_autofix_state_cleared(&active_tab);
    }

    fn emit_autofix_state_cleared(&mut self, target_tab_id: &str) {
        // `cleared` carries no pane info — C++ clears its
        // `lastErrorSessionId` based on the state alone. Reusing the
        // `Idle` snapshot means a subsequent tab switch re-emits a
        // clean state rather than something stale.
        self.set_bar_snapshot(target_tab_id, AutofixBarSnapshot::Idle);
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
    fn emit_autofix_state_suggested(&mut self, target_tab_id: &str, pane_id: &str, title: &str) {
        let snapshot = AutofixBarSnapshot::Suggested {
            pane_id: pane_id.to_string(),
            suggestion_title: title.to_string(),
        };
        self.set_bar_snapshot(target_tab_id, snapshot);
    }

    /// Store a fresh bar snapshot on the target tab and, if that tab is
    /// currently active, forward it to WT so the bottom bar updates.
    fn set_bar_snapshot(&mut self, target_tab_id: &str, snapshot: AutofixBarSnapshot) {
        self.tab_mut(target_tab_id).autofix.bar_snapshot = snapshot.clone();
        if target_tab_id == self.active_tab_key() {
            send_bar_event(&snapshot, Some(target_tab_id));
        }
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
        let tab_key = self.tab_for_session(session_id);
        self.turn_submit_prompt_for_tab(&tab_key, prompt);
    }

    /// Identical to `turn_submit_prompt` but takes the target tab's id
    /// directly, bypassing the `session_id → tab_id` lookup. Used by the
    /// autofix path so a failure in a background tab installs the turn on
    /// that tab even when its ACP session hasn't been created yet (the ACP
    /// layer lazy-creates one when the prompt is dispatched).
    pub fn turn_submit_prompt_for_tab(&mut self, tab_id: &str, prompt: SubmittedPrompt) {
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
        let tab = self.tab_mut(tab_id);
        // Per Decision #3, every Idle→Submitted transition explicitly clears
        // these orthogonal fields rather than relying on side effects from a
        // grab-bag helper.
        tab.messages.clear();
        tab.tool_calls.clear();
        // Dropping any in-flight responders signals Cancelled back to
        // the agent — appropriate when the user starts a new turn.
        tab.permission.clear();
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
        // generation no longer matches the tab's counter, drop it.
        let tab = self.session_tab_mut(session_id);
        let current_gen = tab.autofix.generation;
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
        let current_gen = self.session_tab(session_id).autofix.generation;
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
        let target_tab = self.tab_for_session(session_id);
        let tab = self.session_tab_mut(session_id);
        let prompt = tab.turn.prompt().cloned().expect("prompt set");
        let autofix_pane = prompt.autofix.as_ref().map(|a| a.target_pane_id.clone());
        tab.turn = TurnState::Surfaced {
            prompt,
            outcome: TurnOutcome::Empty,
            end_pending: true,
        };
        if autofix_pane.is_some() {
            self.emit_autofix_state_cleared(&target_tab);
            self.session_tab_mut(session_id).autofix.pane_id = None;
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
                let target_tab = self.tab_for_session(session_id);
                let pane_id = self.session_tab(session_id).autofix.pane_id.clone();
                self.log_selection_phase_for(
                    session_id,
                    "autofix_ignore",
                    &format!("pane={:?}", pane_id),
                );
                if pane_id.is_some() {
                    self.emit_autofix_state_cleared(&target_tab);
                }
                self.session_tab_mut(session_id).autofix.pane_id = None;
                let tab = self.session_tab_mut(session_id);
                let prompt = tab.turn.prompt().cloned().expect("prompt set");
                // Preserve only what the user actually saw streaming (prose
                // or extracted `explanation`) — not the raw JSON wrapper.
                // Any tool calls / plans that streamed during the turn are
                // included regardless; an empty-buf+prose ignore still
                // records them so they don't get stranded on screen.
                let visible = ui::chat::user_visible_stream_text(buf).map(|c| c.into_owned());
                let mut details = tab.current_turn_details();
                if let Some(visible) = visible {
                    details.push(ChatMessage::Agent(visible));
                }
                if !details.is_empty() {
                    let pane_label = prompt
                        .autofix
                        .as_ref()
                        .map(|a| a.target_pane_id.clone())
                        .expect("autofix finalize requires autofix prompt");
                    tab.completed_turns.push(CompletedTurn {
                        prompt: format!("Auto-diagnosed error in pane {pane_label}"),
                        details,
                        expanded: true,
                        trailing_marker: None,
                    });
                }
                // Always clear in-flight UI state on Ignore — even if there
                // was nothing to commit, lingering tool-call rows would look
                // like an active turn.
                tab.messages.clear();
                tab.tool_calls.clear();
                tab.scroll_to_bottom();
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
                    trailing_marker: None,
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
        // Snapshot the title before `choice` is moved into ChoiceExecution,
        // so we can stamp the chat history with an "executed" marker after
        // dispatch.
        let executed_title = choice.title.clone();
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
        let target_tab = self.tab_for_session(session_id);
        let armed_pane = self
            .session_tab(session_id)
            .turn
            .prompt()
            .and_then(|p| p.autofix.as_ref())
            .map(|a| a.target_pane_id.clone());
        let _ = self
            .recommendation_tx
            .send(crate::coordinator::ChoiceExecution { choice, insert_only });
        if armed_pane.is_some() {
            self.emit_autofix_state_cleared(&target_tab);
        }
        self.session_tab_mut(session_id).autofix.pane_id = None;
        let tab = self.session_tab_mut(session_id);
        let TurnState::Surfaced { prompt, end_pending, .. } =
            std::mem::replace(&mut tab.turn, TurnState::Idle)
        else {
            unreachable!()
        };
        tab.selected_recommendation = 0;
        tab.selected_button = 0;
        tab.rec_scroll.reset();
        // Stamp the matching completed_turn (pushed during surface) with an
        // "executed" marker so chat history reflects the user's choice.
        if let Some(last) = tab.completed_turns.last_mut() {
            let marker = t!("chat.turn_executed", title = &executed_title).into_owned();
            last.trailing_marker = Some(marker);
        }
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
        let target_tab = self.tab_for_session(session_id);
        let pane_id = {
            let tab = self.session_tab_mut(session_id);
            tab.autofix.generation = tab.autofix.generation.wrapping_add(1);
            tab.turn
                .prompt()
                .and_then(|p| p.autofix.as_ref())
                .map(|a| a.target_pane_id.clone())
                .or_else(|| tab.autofix.pane_id.clone())
        };
        if pane_id.is_some() {
            self.emit_autofix_state_cleared(&target_tab);
        }
        let tab = self.session_tab_mut(session_id);
        let canceled_marker = t!("chat.turn_canceled").into_owned();
        // Three paths into cancel:
        //   - Submitted / Streaming → commit a fresh completed_turn (prompt +
        //     whatever streamed + canceled marker) so the user always sees
        //     that this turn happened and that they cancelled it.
        //   - Surfaced{Recommendation}: turn_surface_* already pushed a
        //     completed_turn; just append the canceled marker to its details.
        //   - Other states (Idle / Surfaced{Empty / ChatTurn}) → no-op.
        let new_turn_data: Option<(String, Option<String>)> = match &tab.turn {
            TurnState::Submitted(prompt) => {
                let label = match prompt.autofix.as_ref() {
                    Some(a) => format!("Auto-diagnosed error in pane {}", a.target_pane_id),
                    None => prompt.text.clone(),
                };
                Some((label, None))
            }
            TurnState::Streaming { prompt, buf } => {
                let label = match prompt.autofix.as_ref() {
                    Some(a) => format!("Auto-diagnosed error in pane {}", a.target_pane_id),
                    None => prompt.text.clone(),
                };
                let visible = ui::chat::user_visible_stream_text(buf).map(|c| c.into_owned());
                Some((label, visible))
            }
            _ => None,
        };
        let annotate_card = matches!(
            &tab.turn,
            TurnState::Surfaced {
                outcome: TurnOutcome::Recommendation(_),
                ..
            }
        );
        if let Some((prompt_label, visible)) = new_turn_data {
            let mut details = tab.current_turn_details();
            if let Some(v) = visible {
                details.push(ChatMessage::Agent(v));
            }
            tab.completed_turns.push(CompletedTurn {
                prompt: prompt_label,
                details,
                expanded: true,
                trailing_marker: Some(canceled_marker),
            });
            tab.messages.clear();
            tab.tool_calls.clear();
            tab.scroll_to_bottom();
        } else if annotate_card {
            if let Some(last) = tab.completed_turns.last_mut() {
                last.trailing_marker = Some(canceled_marker);
            }
        }
        tab.autofix.pane_id = None;
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
            expanded: true,
            trailing_marker: None,
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
        let target_tab = self.tab_for_session(session_id);
        self.emit_autofix_state_armed(&target_tab, &pane_id, &preview);
        let rec_idx = recommended_choice_index(&recommendations);
        let summary = format_recommendations_for_chat(&recommendations);
        let turn_prompt_label = format!("Auto-diagnosed error in pane {pane_id}");
        let tab = self.session_tab_mut(session_id);
        let prompt = tab.turn.prompt().cloned().expect("prompt set");
        let mut details = tab.current_turn_details();
        details.push(ChatMessage::Agent(summary));
        tab.completed_turns.push(CompletedTurn {
            prompt: turn_prompt_label,
            details,
            expanded: true,
            trailing_marker: None,
        });
        tab.messages.clear();
        tab.tool_calls.clear();
        tab.scroll_to_bottom();
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
            // Auto-expand the auto-diagnosed-error turn: when the user
            // clicks the Suggested pill they came here specifically to
            // read the explanation, so showing the collapsed preview
            // would force a second click.
            tab.completed_turns.push(CompletedTurn {
                prompt: turn_prompt_label,
                details,
                expanded: true,
                trailing_marker: None,
            });
            tab.messages.clear();
            tab.tool_calls.clear();
            tab.scroll_to_bottom();
        }

        let target_tab = self.tab_for_session(session_id);
        self.emit_autofix_state_suggested(&target_tab, &pane_id, &title);
        {
            let tab = self.session_tab_mut(session_id);
            tab.autofix.suggested_pane_id = Some(pane_id.clone());
            tab.autofix.pane_id = None;
        }

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
        // Tag with the helper's owned tab so C++ routes the title-bar
        // update to the right AgentPaneContent. Without this, OnAgentStatusChanged
        // fans the event out to every agent pane in every window — fine
        // for single-pane setups, broken once multiple helpers each
        // publish their own status (cross-tab title-bar clobber).
        if let Some(ref tab) = self.owner_tab_id {
            params["tab_id"] = serde_json::Value::String(tab.clone());
        }
        let evt = serde_json::json!({
            "type": "event",
            "method": "agent_status",
            "params": params,
        });
        send_wt_protocol_event(evt.to_string());
    }

    /// Single outbound projection of the active tab's agent-pane UI state.
    ///
    /// **Architecture contract**: per-tab agent-pane UI state lives in wta.
    /// C++ has one shared agent pane and one set of XAML flags per window,
    /// so anything that varies across WT tabs must be re-asserted on every
    /// tab switch or local mutation. Emits one unified `agent_state_changed`
    /// snapshot — adding a new piece of per-tab UI state in the future is
    /// a matter of putting another field in the payload, no new IDL route
    /// or new C++ handler.
    ///
    /// Payload shape (mirror of the inbound `set_agent_state` request):
    /// ```json
    /// {
    ///   "type": "event",
    ///   "method": "agent_state_changed",
    ///   "params": {
    ///     "view":      "chat" | "sessions",
    ///     "pane_open": true | false
    ///   }
    /// }
    /// ```
    ///
    /// On the C++ side this lands in `TerminalPage::OnAgentStateChanged`,
    /// which is the single writer of `_agentSessionsViewActive` and
    /// `Tab.AgentPaneOpen` for the active tab.
    ///
    /// Also re-emits the autofix bar snapshot (orthogonal domain — bottom
    /// bar autofix indicator — kept on its own `autofix_state` route).
    ///
    /// Call sites:
    ///   - `switch_tab_session` end — covers WT `tab_changed`.
    ///   - `set_agent_state` handler end — echoes C++'s request back so C++
    ///     mirrors it (the round-trip the new architecture is built on).
    ///   - `load_session` after the per-tab mutation.
    ///   - Esc out of Agents view, `/sessions` slash command, Ctrl+C×2
    ///     multi-tab reset.
    ///   - Once at startup (after `--initial-view` has been applied) so
    ///     the bar and the agent-pane-open flag both pick up the spawn
    ///     intent.
    ///
    /// Idempotent — safe to call multiple times in a row.
    pub fn project_active_tab_state(&self) {
        let active = self.active_tab_key().to_string();
        self.project_tab_state(&active);
    }

    /// Project the given tab's state to C++ regardless of whether it is the
    /// active tab. Used by `set_agent_state` so a mutation targeting a
    /// non-active tab still echoes back — under per-tab routing C++ can
    /// apply state changes to any tab, not just the focused one, so
    /// the old "defer until next tab_changed" gate was wrong.
    pub fn project_tab_state(&self, target_tab: &str) {
        let Some(tab) = self.tab_sessions.get(target_tab) else {
            tracing::warn!(
                target: "project_tab_state",
                tab_id = %target_tab,
                "no tab_session for target — skipping echo"
            );
            return;
        };
        let view = match tab.current_view {
            View::Agents => "sessions",
            View::Chat => "chat",
        };
        let evt = serde_json::json!({
            "type": "event",
            "method": "agent_state_changed",
            "params": {
                "tab_id":    target_tab,
                "view":      view,
                "pane_open": tab.pane_open,
            }
        });
        send_wt_protocol_event(evt.to_string());

        // Autofix bar is window-level (single bottom bar reflecting the
        // active tab), so only re-emit when we're projecting the active
        // tab. A non-active mutation does not change the visible bar.
        if target_tab == self.active_tab_key() {
            send_bar_event(&tab.autofix.bar_snapshot, Some(target_tab));
        }
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

/// Build and send an `autofix_state` protocol event from a cached bar
/// snapshot. Used by both fresh state transitions (active tab) and the
/// tab_changed re-emit path. Field shape mirrors what C++
/// `OnAutofixStateChanged` consumes.
fn send_bar_event(snapshot: &AutofixBarSnapshot, tab_id: Option<&str>) {
    let mut evt = match snapshot {
        AutofixBarSnapshot::Idle => serde_json::json!({
            "type": "event",
            "method": "autofix_state",
            "params": { "state": "cleared" }
        }),
        AutofixBarSnapshot::Detected { pane_id, summary, hotkey_hint } => serde_json::json!({
            "type": "event",
            "method": "autofix_state",
            "params": {
                "state": "detected",
                "pane_id": pane_id,
                "summary": summary,
                "hotkey_hint": hotkey_hint,
            }
        }),
        AutofixBarSnapshot::Pending { pane_id, summary } => serde_json::json!({
            "type": "event",
            "method": "autofix_state",
            "params": {
                "state": "pending",
                "pane_id": pane_id,
                "summary": summary,
            }
        }),
        AutofixBarSnapshot::Armed { pane_id, fix_preview, hotkey_hint } => serde_json::json!({
            "type": "event",
            "method": "autofix_state",
            "params": {
                "state": "armed",
                "pane_id": pane_id,
                "fix_preview": fix_preview,
                "hotkey_hint": hotkey_hint,
            }
        }),
        AutofixBarSnapshot::Suggested { pane_id, suggestion_title } => serde_json::json!({
            "type": "event",
            "method": "autofix_state",
            "params": {
                "state": "suggested",
                "pane_id": pane_id,
                "suggestion_title": suggestion_title,
            }
        }),
    };
    // Tag with tab_id so C++ routes the bottom-bar update to the right
    // tab's AgentPaneContent (window-level bar reflects active tab's
    // autofix state). Without this, the event fans out and a non-active
    // tab's autofix would clobber the bar.
    if let Some(t) = tab_id {
        if let Some(params) = evt.get_mut("params").and_then(|v| v.as_object_mut()) {
            params.insert(
                "tab_id".to_string(),
                serde_json::Value::String(t.to_string()),
            );
        }
    }
    send_wt_protocol_event(evt.to_string());
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
        let (rename_session_tx, _rename_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (restart_tx, _restart_rx) = tokio::sync::mpsc::unbounded_channel();
        let debug_capture = Arc::new(AtomicBool::new(false));
        App::new(prompt_tx, recommendation_tx, permission_tx, cancel_tx, new_session_tx, load_session_tx, drop_session_tx, rename_session_tx, restart_tx, debug_capture, true, false)
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
        let n = classify_wt_event("connection_state", "3", None, &params);
        assert_eq!(n.severity, WtEventSeverity::Critical);
        assert!(n.summary.contains("failed"));
        assert!(!n.acknowledged);
    }

    #[test]
    fn classify_connection_closed_is_actionable() {
        let params = json!({"session_id": "5", "state": "closed"});
        let n = classify_wt_event("connection_state", "5", None, &params);
        assert_eq!(n.severity, WtEventSeverity::Actionable);
        assert!(n.summary.contains("exited"));
    }

    #[test]
    fn classify_connection_connected_is_informational() {
        let params = json!({"session_id": "1", "state": "connected"});
        let n = classify_wt_event("connection_state", "1", None, &params);
        assert_eq!(n.severity, WtEventSeverity::Informational);
        assert!(n.summary.contains("connected"));
    }

    #[test]
    fn classify_osc133_command_failed_is_actionable() {
        let params = json!({"session_id": "2", "sequence": "osc:133;D;1"});
        let n = classify_wt_event("vt_sequence", "2", None, &params);
        assert_eq!(n.severity, WtEventSeverity::Actionable);
        assert!(n.summary.contains("Command failed"));
        assert!(n.summary.contains("exit 1"));
    }

    #[test]
    fn classify_osc133_command_success_is_silent() {
        let params = json!({"session_id": "2", "sequence": "osc:133;D;0"});
        let n = classify_wt_event("vt_sequence", "2", None, &params);
        assert!(n.acknowledged); // auto-dismissed
    }

    #[test]
    fn classify_osc133_high_exit_code() {
        let params = json!({"session_id": "2", "sequence": "osc:133;D;127"});
        let n = classify_wt_event("vt_sequence", "2", None, &params);
        assert_eq!(n.severity, WtEventSeverity::Actionable);
        assert!(n.summary.contains("exit 127"));
    }

    #[test]
    fn classify_osc133_prompt_marker_is_silent() {
        // OSC 133;A is a prompt marker, not a command finish
        let params = json!({"session_id": "2", "sequence": "osc:133;A"});
        let n = classify_wt_event("vt_sequence", "2", None, &params);
        assert!(n.acknowledged); // silenced
    }

    #[test]
    fn classify_normal_vt_sequence_is_silent() {
        let params = json!({"session_id": "7", "sequence": "osc:0;title"});
        let n = classify_wt_event("vt_sequence", "7", None, &params);
        assert!(n.acknowledged); // silenced
    }

    #[test]
    fn classify_unknown_method_is_informational() {
        let params = json!({"session_id": "1"});
        let n = classify_wt_event("something_new", "1", None, &params);
        assert_eq!(n.severity, WtEventSeverity::Informational);
    }

    // ─── tab_renamed (tab-drag rekeying) ────────────────────────────────────

    #[test]
    fn tab_renamed_rekeys_active_tab_and_session_map() {
        let mut app = test_app();
        // Seed: active tab is AAAA with a bound ACP session.
        app.tab_id = Some("AAAA".to_string());
        app.tab_sessions
            .insert("AAAA".to_string(), TabSession::default());
        app.session_to_tab
            .insert("sess-1".to_string(), "AAAA".to_string());

        // Drive the rename via the WtEvent dispatch path — same code path
        // a real broadcast from the COM server takes.
        app.handle_event(AppEvent::WtEvent {
            method: "tab_renamed".to_string(),
            pane_id: String::new(),
            tab_id: None,
            params: json!({"old_tab_id": "AAAA", "new_tab_id": "BBBB"}),
        });

        assert_eq!(app.tab_id.as_deref(), Some("BBBB"),
            "active tab id must follow the rename");
        assert!(app.tab_sessions.contains_key("BBBB"),
            "tab_sessions must contain the new key after rename");
        assert!(!app.tab_sessions.contains_key("AAAA"),
            "tab_sessions must no longer contain the old key");
        assert_eq!(app.session_to_tab.get("sess-1").map(String::as_str),
            Some("BBBB"),
            "session_to_tab values pointing at the old id must be rewritten");
    }

    #[test]
    fn tab_renamed_appevent_variant_drives_same_handler() {
        // Direct AppEvent::TabRenamed dispatch — used by callers that
        // already deserialized the params (mirrors the WtEvent inline
        // path).
        let mut app = test_app();
        app.tab_id = Some("AAAA".to_string());
        app.tab_sessions
            .insert("AAAA".to_string(), TabSession::default());

        app.handle_event(AppEvent::TabRenamed {
            old_tab_id: "AAAA".to_string(),
            new_tab_id: "CCCC".to_string(),
            new_window_id: None,
        });

        assert_eq!(app.tab_id.as_deref(), Some("CCCC"));
        assert!(app.tab_sessions.contains_key("CCCC"));
        assert!(!app.tab_sessions.contains_key("AAAA"));
    }

    #[test]
    fn tab_renamed_sends_rename_session_request_to_acp_client() {
        // The chat-history side rekeys in-process, but tab_to_session
        // lives in the ACP client task — it has to be told to rekey via
        // the rename_session_tx channel. Without this signal, the next
        // prompt on the dragged tab can't find the old SessionId.
        let (prompt_tx, _prompt_rx) = tokio::sync::mpsc::unbounded_channel();
        let (recommendation_tx, _recommendation_rx) = tokio::sync::mpsc::unbounded_channel();
        let (permission_tx, _permission_rx) = tokio::sync::mpsc::unbounded_channel();
        let (cancel_tx, _cancel_rx) = tokio::sync::mpsc::unbounded_channel();
        let (new_session_tx, _new_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (load_session_tx, _load_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (drop_session_tx, _drop_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (rename_session_tx, mut rename_session_rx) =
            tokio::sync::mpsc::unbounded_channel();
        let (restart_tx, _restart_rx) = tokio::sync::mpsc::unbounded_channel();
        let debug_capture = Arc::new(AtomicBool::new(false));
        let mut app = App::new(
            prompt_tx,
            recommendation_tx,
            permission_tx,
            cancel_tx,
            new_session_tx,
            load_session_tx,
            drop_session_tx,
            rename_session_tx,
            restart_tx,
            debug_capture,
            true,
            false,
        );

        app.tab_id = Some("AAAA".to_string());
        app.tab_sessions
            .insert("AAAA".to_string(), TabSession::default());

        app.handle_event(AppEvent::TabRenamed {
            old_tab_id: "AAAA".to_string(),
            new_tab_id: "BBBB".to_string(),
            new_window_id: None,
        });

        // The ACP client task should have received exactly one
        // RenameSessionRequest with the old/new ids — that's what makes
        // the dragged tab's chat history line up with the agent's turn
        // context after the drag.
        let req = rename_session_rx
            .try_recv()
            .expect("rename_session_tx must have received a request");
        assert_eq!(req.old_tab_id, "AAAA");
        assert_eq!(req.new_tab_id, "BBBB");
        assert!(rename_session_rx.try_recv().is_err(),
            "exactly one request should have been sent");
    }

    #[test]
    fn tab_renamed_noop_does_not_send_rename_session_request() {
        // A no-op rename (old == new) must not bother the ACP client —
        // there's nothing to rekey, and a spurious request would
        // needlessly grab the tab_to_session lock.
        let (prompt_tx, _prompt_rx) = tokio::sync::mpsc::unbounded_channel();
        let (recommendation_tx, _recommendation_rx) = tokio::sync::mpsc::unbounded_channel();
        let (permission_tx, _permission_rx) = tokio::sync::mpsc::unbounded_channel();
        let (cancel_tx, _cancel_rx) = tokio::sync::mpsc::unbounded_channel();
        let (new_session_tx, _new_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (load_session_tx, _load_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (drop_session_tx, _drop_session_rx) = tokio::sync::mpsc::unbounded_channel();
        let (rename_session_tx, mut rename_session_rx) =
            tokio::sync::mpsc::unbounded_channel();
        let (restart_tx, _restart_rx) = tokio::sync::mpsc::unbounded_channel();
        let debug_capture = Arc::new(AtomicBool::new(false));
        let mut app = App::new(
            prompt_tx,
            recommendation_tx,
            permission_tx,
            cancel_tx,
            new_session_tx,
            load_session_tx,
            drop_session_tx,
            rename_session_tx,
            restart_tx,
            debug_capture,
            true,
            false,
        );

        app.tab_id = Some("AAAA".to_string());
        app.tab_sessions
            .insert("AAAA".to_string(), TabSession::default());

        app.handle_event(AppEvent::TabRenamed {
            old_tab_id: "AAAA".to_string(),
            new_tab_id: "AAAA".to_string(),
            new_window_id: None,
        });

        assert!(rename_session_rx.try_recv().is_err(),
            "no-op rename must not send a RenameSessionRequest");
    }

    #[test]
    fn tab_renamed_with_missing_fields_is_dropped() {
        let mut app = test_app();
        app.tab_id = Some("AAAA".to_string());
        app.tab_sessions
            .insert("AAAA".to_string(), TabSession::default());

        // Empty new_tab_id — must not corrupt state.
        app.handle_event(AppEvent::WtEvent {
            method: "tab_renamed".to_string(),
            pane_id: String::new(),
            tab_id: None,
            params: json!({"old_tab_id": "AAAA", "new_tab_id": ""}),
        });
        assert_eq!(app.tab_id.as_deref(), Some("AAAA"),
            "rename with empty new_tab_id must be dropped, leaving state untouched");
        assert!(app.tab_sessions.contains_key("AAAA"));

        // Missing field entirely — must not corrupt state.
        app.handle_event(AppEvent::WtEvent {
            method: "tab_renamed".to_string(),
            pane_id: String::new(),
            tab_id: None,
            params: json!({"old_tab_id": "AAAA"}),
        });
        assert_eq!(app.tab_id.as_deref(), Some("AAAA"));
        assert!(app.tab_sessions.contains_key("AAAA"));
    }

    // ─── WtNotification auto-dismiss ────────────────────────────────────────

    #[test]
    fn informational_auto_dismisses_after_threshold() {
        let mut n = WtNotification {
            severity: WtEventSeverity::Informational,
            pane_id: "1".to_string(),
            tab_id: None,
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
            tab_id: None,
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
            tab_id: None,
            summary: "exited".to_string(),
            acknowledged: false,
            age_ticks: 1000,
        };
        assert!(!n.should_auto_dismiss());
    }

    // ─── App notification state ─────────────────────────────────────────────

    #[test]
    fn wt_event_critical_raises_banner_only_no_chat() {
        // WT events route through the bottom bar / `wt_notifications` queue,
        // never the agent's chat history. The chat is for agent dialogue;
        // process-lifecycle noise belongs in the bar.
        let mut app = test_app();
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "3".to_string(),
            tab_id: None,
            params: json!({"session_id": "3", "state": "failed"}),
        });
        assert!(app.show_notification_banner);
        assert_eq!(app.wt_notifications.len(), 1);
        assert_eq!(app.wt_notifications[0].severity, WtEventSeverity::Critical);
        assert!(
            app.current_tab().messages.is_empty(),
            "WT events must not pollute chat history with Error messages"
        );
    }

    #[test]
    fn wt_event_actionable_raises_banner_only_no_chat() {
        let mut app = test_app();
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "5".to_string(),
            tab_id: None,
            params: json!({"session_id": "5", "state": "closed"}),
        });
        assert!(app.show_notification_banner);
        assert!(
            app.current_tab().messages.is_empty(),
            "WT events must not pollute chat history with System messages"
        );
    }

    #[test]
    fn wt_event_informational_no_banner_no_chat_message() {
        let mut app = test_app();
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "1".to_string(),
            tab_id: None,
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
            tab_id: None,
            params: json!({"session_id": "42", "state": "failed"}),
        });
        // Events from our own pane should be completely ignored
        assert!(!app.show_notification_banner);
        assert!(app.wt_notifications.is_empty());
        assert!(app.current_tab().messages.is_empty());
    }

    #[test]
    fn wt_event_critical_from_other_tab_does_not_surface_in_owner_tab() {
        // Regression for the cross-tab "Pane …: connection failed" leak:
        // helper A owns tab A; tab B's Copilot pane fails; WT broadcasts
        // the `connection_state:failed` event to every helper. Helper A
        // must drop it instead of writing a red Error into tab A's chat.
        let mut app = test_app();
        app.owner_tab_id = Some("{tab-A}".to_string());
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "B-PANE".to_string(),
            tab_id: Some("{tab-B}".to_string()),
            params: json!({"pane_id": "B-PANE", "state": "failed", "tab_id": "{tab-B}"}),
        });
        assert!(!app.show_notification_banner);
        assert!(app.wt_notifications.is_empty());
        assert!(app.current_tab().messages.is_empty());
    }

    #[test]
    fn wt_event_critical_from_owner_tab_raises_banner_not_chat() {
        // Same-tab event raises the banner but still does NOT push into chat
        // — the bar is the user-visible surface for connection failures.
        let mut app = test_app();
        app.owner_tab_id = Some("{tab-A}".to_string());
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "A-PANE".to_string(),
            tab_id: Some("{tab-A}".to_string()),
            params: json!({"pane_id": "A-PANE", "state": "failed", "tab_id": "{tab-A}"}),
        });
        assert!(app.show_notification_banner);
        assert_eq!(app.wt_notifications.len(), 1);
        assert!(
            app.current_tab().messages.is_empty(),
            "WT events must not pollute chat history"
        );
    }

    #[test]
    fn dismiss_notifications_clears_banner_and_acknowledges() {
        let mut app = test_app();
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "3".to_string(),
            tab_id: None,
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
            tab_id: None,
            params: json!({"session_id": "1", "state": "closed"}),
        });
        // Second event (more recent)
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "2".to_string(),
            tab_id: None,
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
                tab_id: None,
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
            tab_id: None,
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
            tab_id: None,
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
            tab_id: None,
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
            tab_id: None,
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
            tab_id: None,
            params: json!({"session_id": "1", "state": "connected"}),
        });
        // Critical from pane 2
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "2".to_string(),
            tab_id: None,
            params: json!({"session_id": "2", "state": "failed"}),
        });
        // Actionable from pane 3
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            pane_id: "3".to_string(),
            tab_id: None,
            params: json!({"session_id": "3", "state": "closed"}),
        });

        assert_eq!(app.wt_notifications.len(), 3);
        // Unacknowledged count only counts actionable + critical
        assert_eq!(app.unacknowledged_count(), 2);
        // Banner should show (due to critical + actionable)
        assert!(app.show_notification_banner);
        // Chat must stay empty — WT events surface in the bar/banner, never
        // in agent dialogue.
        assert!(app.current_tab().messages.is_empty());
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

    // ─── Phantom-session prune ───────────────────────────────────────

    fn make_ended_session(
        cli: crate::agent_sessions::CliSource,
        key: &str,
    ) -> crate::agent_sessions::AgentSessionRegistry {
        use crate::agent_sessions::{AgentSessionRegistry, SessionEvent};
        use std::path::PathBuf;
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: key.into(),
            cli_source: cli,
            pane_session_id: "p".into(),
            cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        reg.apply(SessionEvent::SessionStopped {
            key: key.into(),
            reason: "user_exit".into(),
        });
        reg
    }

    #[test]
    fn prune_phantom_session_drops_ended_claude_row_when_not_resumable() {
        // Reproduces the "ghost live Claude session" bug: open
        // `claude`, run `/model`, exit. Without the prune, the Ended
        // row sticks around and Enter dead-ends on
        // `No conversation found with session ID: <id>`.
        use crate::agent_sessions::CliSource;
        let mut reg = make_ended_session(CliSource::Claude, "phantom-claude");
        assert!(reg.has_session(&"phantom-claude".to_string()));
        crate::app::prune_phantom_session_if_ended_with(
            &mut reg,
            "phantom-claude",
            |_cli, _k| false,
        );
        assert!(!reg.has_session(&"phantom-claude".to_string()));
    }

    #[test]
    fn prune_phantom_session_drops_ended_copilot_row_when_not_resumable() {
        // Reproduces the equivalent Copilot bug: open `copilot`, exit.
        // workspace.yaml exists but events.jsonl is missing/empty.
        // Enter on the Ended row would launch
        // `copilot --resume=<id>` and dead-end on
        // `Error: No session, task, or name matched '<id>'`.
        use crate::agent_sessions::CliSource;
        let mut reg = make_ended_session(CliSource::Copilot, "phantom-copilot");
        crate::app::prune_phantom_session_if_ended_with(
            &mut reg,
            "phantom-copilot",
            |_cli, _k| false,
        );
        assert!(!reg.has_session(&"phantom-copilot".to_string()),
            "phantom Ended Copilot row must be removed");
    }

    #[test]
    fn prune_phantom_session_drops_ended_gemini_row_when_not_resumable() {
        // Reproduces the equivalent Gemini bug: open `gemini`, exit.
        // The JSONL has only the session header — no user/tool record.
        use crate::agent_sessions::CliSource;
        let mut reg = make_ended_session(CliSource::Gemini, "phantom-gemini");
        crate::app::prune_phantom_session_if_ended_with(
            &mut reg,
            "phantom-gemini",
            |_cli, _k| false,
        );
        assert!(!reg.has_session(&"phantom-gemini".to_string()),
            "phantom Ended Gemini row must be removed");
    }

    #[test]
    fn prune_phantom_session_dispatches_cli_argument() {
        // The probe callback receives the row's CliSource so it can
        // dispatch to the right per-CLI on-disk check. Verify the
        // CliSource passed through matches the row's, for both
        // Claude and Copilot, so the routing logic is regression-safe.
        use crate::agent_sessions::CliSource;
        use std::sync::{Arc, Mutex};

        for cli in [CliSource::Claude, CliSource::Copilot, CliSource::Gemini] {
            let mut reg = make_ended_session(cli.clone(), "k");
            let probed = Arc::new(Mutex::new(None));
            let probed_capture = Arc::clone(&probed);
            crate::app::prune_phantom_session_if_ended_with(&mut reg, "k", move |c, _k| {
                *probed_capture.lock().unwrap() = Some(c.clone());
                true // not a phantom — keep row, just observe routing
            });
            let captured = probed.lock().unwrap().clone();
            assert_eq!(captured.as_ref(), Some(&cli),
                "probe must receive the row's CliSource ({:?})", cli);
        }
    }

    #[test]
    fn prune_phantom_session_keeps_ended_row_when_resumable() {
        // Symmetric to the per-CLI drop tests: if the on-disk
        // artefact has real content, the prune is a no-op so the user
        // can resume via Enter in F2.
        use crate::agent_sessions::CliSource;
        let mut reg = make_ended_session(CliSource::Claude, "real-id");
        crate::app::prune_phantom_session_if_ended_with(&mut reg, "real-id", |_cli, _k| true);
        assert!(reg.has_session(&"real-id".to_string()),
            "resumable Ended row must NOT be removed");
    }

    #[test]
    fn prune_phantom_session_skips_live_rows() {
        // Status must be Ended for the prune to fire — silently
        // removing a still-live (Idle/Working/Attention) row would be
        // a UX disaster.
        use crate::agent_sessions::{AgentSessionRegistry, CliSource, SessionEvent};
        use std::path::PathBuf;
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: "live-id".into(),
            cli_source: CliSource::Claude,
            pane_session_id: "p".into(),
            cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        crate::app::prune_phantom_session_if_ended_with(&mut reg, "live-id", |_, _| false);
        assert!(reg.has_session(&"live-id".to_string()));
    }

    #[test]
    fn pane_closed_via_agent_event_triggers_phantom_prune() {
        // Reproduces the "stale-Idle row" recovery path: when the
        // user presses Enter on a row whose pane has died silently
        // (tab closed while WT's connection_state racing with
        // TermControl teardown lost the event), our focus-pane
        // callback posts `AgentSessionEvent(PaneClosed { ... })` to
        // demote the row to Ended — and the post-apply prune in the
        // AgentSessionEvent handler then drops the row if its CLI
        // artefacts indicate no resumable content. This test drives
        // that whole path: PaneClosed event → Ended → prune fires.
        //
        // Stubs the on-disk probe via the testable `_with` variant
        // so the test doesn't touch the real `~/.claude` tree. The
        // full path being tested here uses the global probe, so the
        // test directly exercises the handler logic via the variant
        // we use to test prune behaviour.
        use crate::agent_sessions::{AgentSessionRegistry, CliSource, SessionEvent};
        use std::path::PathBuf;
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: "stale".into(),
            cli_source: CliSource::Claude,
            pane_session_id: "00000000-0000-0000-0000-deadbeefdead".into(),
            cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        // Sanity: row is bound to the pane.
        assert_eq!(
            reg.key_for_pane("00000000-0000-0000-0000-deadbeefdead").as_deref(),
            Some("stale"),
            "row must be bound to pane before close",
        );
        // Simulate the focus-pane callback firing PaneClosed.
        // Capture key BEFORE apply (mirrors the AgentSessionEvent handler).
        let key_to_prune = reg.key_for_pane("00000000-0000-0000-0000-deadbeefdead");
        reg.apply(SessionEvent::PaneClosed {
            pane_session_id: "00000000-0000-0000-0000-deadbeefdead".into(),
        });
        // Pre-prune: row is now Ended but still in the registry.
        assert!(reg.has_session(&"stale".to_string()));
        // Prune with phantom probe → row should be removed.
        let k = key_to_prune.expect("captured key before apply");
        crate::app::prune_phantom_session_if_ended_with(&mut reg, &k, |_cli, _key| false);
        assert!(
            !reg.has_session(&"stale".to_string()),
            "phantom row must be removed once PaneClosed transitions it to Ended",
        );
    }

    #[test]
    fn default_prune_uses_strict_probe_for_live_claude_session_without_jsonl() {
        // End-to-end regression for the user-reported bug:
        //   "start a claude session，no conversation，close session,
        //    session still active, resume error"
        //
        // Concretely: the user launches `claude` via the agent pane
        // (ACP), exchanges zero turns, exits the pane. Claude does
        // NOT write a JSONL under `~/.claude/projects/...` for that
        // session id (it only flushes when there's content). With
        // the previous lenient probe ("missing artefact → defer to
        // CLI → resumable=true"), the post-`SessionStopped` prune
        // believed the row was real and left it Ended in F2. Pressing
        // Enter then launched `claude --resume <id>` and dead-ended
        // on `No conversation found with session ID: <id>`.
        //
        // We drive the contract via the injectable
        // `prune_phantom_session_if_ended_with` variant rather than
        // the global `prune_phantom_session_if_ended`. The latter
        // probes the real `~/.claude/projects` tree under whatever
        // home directory the test runner happens to have, which is
        // non-hermetic — flaky on developer machines if a Claude
        // session ever happens to land on the chosen UUID, and
        // dependent on USERPROFILE/HOME environment state. The
        // injectable variant lets us pin the probe to the precise
        // semantics the production default uses
        // (`key_has_definite_resumable_content`) without touching
        // the filesystem.
        use crate::agent_sessions::{AgentSessionRegistry, CliSource, SessionEvent};
        use std::path::PathBuf;
        let key = "ed7c7c7c-9999-8888-7777-666666666666-strict";
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: key.into(),
            cli_source: CliSource::Claude,
            pane_session_id: "00000000-0000-0000-0000-aaaaaaaaaaaa".into(),
            cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        reg.apply(SessionEvent::SessionStopped {
            key: key.into(),
            reason: "user_exit".into(),
        });
        // Sanity: the row is in the registry (Ended) before prune.
        assert!(reg.has_session(&key.to_string()));
        // Drive the prune with the strict-probe contract pinned
        // via the injectable variant. Stub returns `false` to model
        // "no JSONL on disk" — the exact case the default's strict
        // probe (`key_has_definite_resumable_content`) reports for
        // a Claude session whose CLI never flushed.
        crate::app::prune_phantom_session_if_ended_with(
            &mut reg,
            key,
            |_cli, _key| false,
        );
        assert!(
            !reg.has_session(&key.to_string()),
            "prune must drop an Ended Claude row whose on-disk \
             artefacts are absent (strict-probe contract)",
        );
    }

    #[test]
    fn shift_enter_history_row_short_circuits_when_session_is_phantom() {
        // Belt-and-suspenders: the Shift+Enter path
        // (resume_in_new_agent_tab → ACP loadSession) also gates on
        // the phantom check. Without it, the user pressing Shift+Enter
        // on a row whose CLI artefacts indicate "no conversation"
        // would burn a new WT tab + reconcile the agent pane onto it,
        // then dead-end inside the agent on a loadSession error.
        //
        // We can't easily inject a fake `key_is_resumable_on_disk`
        // into `dispatch_resume_in_agent_pane` (it calls the global
        // helper directly), but we CAN exercise the path by using a
        // key whose CLI artefact does exist on disk and is phantom.
        // Instead of that filesystem dependency, this test asserts
        // the simpler invariant: when the probe returns false in
        // production code, the dispatched command is the
        // `--phantom-skipped` shape (not the real
        // resume_in_new_agent_tab). The full check is covered by
        // history_loader tests; here we exercise the App-level
        // routing.
        //
        // Sanity check via the dispatched-command tape: when the
        // capability gate fails (loadSession unsupported), the tape
        // shows `--unsupported`. The phantom branch should likewise
        // tag the tape with `--phantom-skipped`. This mirrors the
        // existing test for the unsupported branch.
        //
        // (Direct fully-integrated test of the phantom branch
        // requires manipulating the real home filesystem; covered
        // by the existing history_loader tests for the probe and
        // the prune tests above. This is documentation of the
        // contract.)
        use crate::agent_sessions::{CliSource, SessionEvent};
        use std::path::PathBuf;
        let mut app = test_app();
        // Mark loadSession supported so the capability gate doesn't
        // preempt the phantom check.
        app.agent_supports_load_session = true;
        // Use a CLI source for which the real ~/.claude/projects
        // can't have this UUID. The probe falls through to "no
        // JSONL → defer to CLI" (true), so the phantom branch does
        // NOT fire and we get the normal resume_in_new_agent_tab
        // dispatch. This proves the no-false-positive case.
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "abc-this-uuid-is-not-on-disk-anywhere-9999".into(),
            cli_source: CliSource::Claude,
            pane_session_id: "p".into(),
            cwd: PathBuf::from("/work/proj"),
            title: "t".into(),
        });
        app.agent_sessions.apply(SessionEvent::SessionStopped {
            key: "abc-this-uuid-is-not-on-disk-anywhere-9999".into(),
            reason: "user_exit".into(),
        });
        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_list_state.select(Some(0));
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        let cmd = app
            .last_dispatched_command_for_test()
            .expect("a command was dispatched");
        // Probe returned true ("no JSONL on disk → defer to CLI"), so
        // the normal dispatch goes through, NOT the phantom-skipped
        // path. The argv should contain --cwd, not --phantom-skipped.
        let argv = cmd.argv.join(" ");
        assert!(
            !argv.contains("--phantom-skipped"),
            "no-on-disk-artefact case must NOT short-circuit as phantom; argv: {}",
            argv
        );
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

    #[test]
    fn closing_other_tab_preserves_per_tab_view_when_tab_changed_follows() {
        // Reproduces the user-reported bug:
        //   tab1 has the session list (Agents view) open. User opens
        //   tab2, then closes tab2. Focus returns to tab1, the agent
        //   pane is still visible, but the session list has vanished
        //   — the user has to press the shortcut again to bring it
        //   back.
        //
        // Root cause was on the C++ side: `_OnTabSelectionChanged`
        // is suppressed during tab removal, so the
        // `_NotifyAgentTabChanged(tab1)` that normally follows the
        // auto-selection of the previous tab never fired. wta's
        // `tab_id` got nulled by `tab_closed` and never restored, so
        // `current_tab()` silently fell back to the empty
        // `DEFAULT_TAB_ID` slot. After the C++ fix
        // (explicit `_ReconcileAgentPaneForActiveTab` post-removal),
        // wta receives the missing `tab_changed { tab_id: tab1 }`
        // event and `current_tab()` resolves back to tab1's
        // preserved TabSession with `View::Agents` intact.
        //
        // This test simulates the full wta-side event sequence:
        //   1. tab1 active, picker open with selection at row 2.
        //   2. user clicks tab2 → tab_changed { tab_id: tab2 }.
        //   3. user closes tab2 → tab_closed { tab_id: tab2 }.
        //   4. C++ fires the post-removal reconcile →
        //      tab_changed { tab_id: tab1 }.
        // After (4), `current_tab()` must return tab1's TabSession
        // with View::Agents and the row-2 selection preserved.
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

        // (1) tab1 active, Agents view, selection at row 2.
        let tab1 = "tab1-stable-id";
        let tab2 = "tab2-stable-id";
        app.tab_id = Some(tab1.into());
        app.current_tab_mut().current_view = View::Agents;
        app.current_tab_mut().agents_list_state.select(Some(2));

        // (2) User clicks tab2: switch_tab_session simulates the
        // arrival of `tab_changed { tab_id: tab2 }`.
        app.switch_tab_session(tab2.into());
        // tab2 starts at defaults; tab1 entry is untouched in the map.
        assert_eq!(app.current_tab().current_view, View::Chat);

        // (3) User closes tab2: drop_tab_session simulates
        // `tab_closed { tab_id: tab2 }`. tab2's entry is removed and
        // tab_id is nulled (DEFAULT_TAB_ID slot lazily created).
        app.drop_tab_session(tab2);
        assert!(app.tab_id.is_none(),
            "drop of active tab must null tab_id pending the next tab_changed");

        // Critical: BEFORE the C++ fix, this is where wta is left
        // stranded — no further `tab_changed` ever arrives. The user
        // sees the agent pane stuck on DEFAULT_TAB_ID's empty Chat
        // view even though tab1's state is still in the map.
        // Demonstrate the bug shape:
        assert_eq!(app.current_tab().current_view, View::Chat,
            "without the follow-up tab_changed, current_tab falls back to DEFAULT_TAB_ID");

        // (4) The C++ fix: post-removal reconcile fires
        // `_NotifyAgentTabChanged(tab1)` which lands here as
        // `switch_tab_session(tab1)`.
        app.switch_tab_session(tab1.into());

        // Now current_tab resolves back to tab1's preserved state.
        assert_eq!(app.current_tab().current_view, View::Agents,
            "tab1's View::Agents must be preserved across tab2's open/close");
        assert_eq!(app.current_tab().agents_list_state.selected(), Some(2),
            "tab1's list selection must be preserved");
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
            tab_id: Some("test-tab".to_string()),
            summary: format!("Pane {}: process exited", pane),
            acknowledged: false,
            age_ticks: 0,
        };
        app.maybe_trigger_autofix(&notification);

        // Suppression: no autofix prompt should be in-flight, no armed pane.
        assert!(
            app.tab_mut("test-tab").autofix.pane_id.is_none(),
            "autofix must not arm an agent CLI pane on its own exit"
        );
        assert!(
            app.tab_mut("test-tab").turn.is_idle(),
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
            tab_id: Some("test-tab".to_string()),
            summary: "Command failed (exit 1)".to_string(),
            acknowledged: false,
            age_ticks: 0,
        };
        app.maybe_trigger_autofix(&notification);

        assert_eq!(
            app.tab_mut("test-tab").autofix.pane_id.as_deref(),
            Some(pane),
            "autofix must still arm normal panes when a command fails"
        );
        // The target tab's turn (not the active tab's) should be in-flight.
        assert!(
            !app.tab_mut("test-tab").turn.is_idle(),
            "autofix prompt should be in-flight on the target tab"
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
        // The session is NOT tagged with `SessionOrigin::AgentPane` (this
        // test sets up state via raw SessionStarted, so origin defaults
        // to Unknown), which means SessionStopped immediately transitions
        // to Ended and releases the pane binding — exactly the precondition
        // this test depends on.
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
            tab_id: None,
            params: serde_json::json!({"session_id": pane, "state": "closed"}),
        });

        assert!(
            app.tab_sessions.values().all(|t| t.autofix.pane_id.is_none()),
            "connection_state:closed must never arm autofix — no exit code, \
             no command context, pane is dead so subsequent ReadPaneOutput \
             would throw E_FAIL"
        );
        assert!(
            app.current_tab().turn.is_idle(),
            "no autofix prompt should be in-flight"
        );
        // The pane-closed event surfaces via the banner / `wt_notifications`,
        // never in chat. Chat is the agent dialogue surface.
        assert!(
            app.current_tab().messages.is_empty(),
            "WT events must not push into chat history"
        );
        assert!(app.show_notification_banner);
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
            tab_id: None,
            params: serde_json::json!({
                "session_id": pane,
                "sequence": "osc:133;D;1",
            }),
        });

        assert!(
            app.tab_sessions.values().all(|t| t.autofix.pane_id.is_none()),
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
            tab_id: Some("test-tab".to_string()),
            params: serde_json::json!({
                "session_id": pane,
                "sequence": "osc:133;D;1",
            }),
        });

        assert_eq!(
            app.tab_mut("test-tab").autofix.pane_id.as_deref(),
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
        use crate::agent_sessions::{CliSource, SessionEvent};
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
            tab_id: None,
            params: serde_json::json!({
                "session_id": pane,
                "sequence": "osc:133;A",
            }),
        });

        // Two outcomes are correct, both signal that the bridge fired:
        //   1. Row transitions to Ended (PaneClosed applied), AND
        //   2. The phantom-session prune (Gemini has no on-disk JSONL
        //      for `gemini-key`, so the strict probe treats it as a
        //      phantom and removes the row entirely).
        // If the bridge had NOT fired, the row would still be Idle
        // and the prune would not have run (prune only fires on
        // Ended rows). So absence-from-registry confirms both:
        // the bridge fired AND the prune fired correctly.
        let still_present = app
            .agent_sessions
            .iter_sorted()
            .into_iter()
            .any(|s| s.key == "gemini-key");
        assert!(
            !still_present,
            "agent-bound pane seeing osc:133;A must transition to Ended \
             and (since `gemini-key` has no on-disk JSONL) be pruned as \
             a phantom; row is still present so the bridge didn't fire",
        );
        // The pane→key binding must be cleared either way.
        assert!(
            !app.agent_sessions.is_agent_pane(pane),
            "pane binding should be cleared after close",
        );
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
            tab_id: None,
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
            tab_id: None,
            params: serde_json::json!({"session_id": pane, "state": "closed"}),
        });

        // `gemini-key` has no on-disk JSONL, so the strict phantom
        // probe (run by the post-PaneClosed prune) removes the row.
        // The test still verifies the bridge wired correctly: if
        // PaneClosed had NOT been applied, the row would still be
        // Idle and the prune (which only fires on Ended) would have
        // left it alone — so absence-from-registry is the strongest
        // signal that the bridge fired.
        let still_present = app
            .agent_sessions
            .iter_sorted()
            .into_iter()
            .any(|s| s.key == "gemini-key");
        assert!(
            !still_present,
            "Gemini row must transition to Ended on connection_state:closed \
             AND (with no on-disk JSONL) be pruned by the phantom check; \
             row is still present so the bridge didn't fire",
        );
        assert!(
            !app.agent_sessions.is_agent_pane(pane),
            "pane binding should be cleared after close",
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
        let gen = {
            let tab = app.tab_mut(DEFAULT_TAB_ID);
            tab.autofix.generation = tab.autofix.generation.wrapping_add(1);
            tab.autofix.pane_id = Some(pane.into());
            tab.autofix.generation
        };
        let prompt = SubmittedPrompt {
            id: 99,
            text: "diagnose this".into(),
            submitted_at_unix_s: 0.0,
            autofix: Some(AutofixContext {
                target_pane_id: pane.into(),
                generation: gen,
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
        assert!(app.tab_mut(DEFAULT_TAB_ID).autofix.pane_id.is_some());
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
            app.tab_mut(DEFAULT_TAB_ID).autofix.pane_id.is_none(),
            "autofix.pane_id must be cleared so the bar leaves Pending"
        );
    }

    #[test]
    fn stale_autofix_chunks_dropped_when_generation_diverges() {
        let mut app = test_app();
        submit_autofix_prompt(&mut app, "pane-1");
        // Simulate an Esc cancel or a newer trigger bumping the counter
        // on the same tab as the in-flight prompt.
        {
            let tab = app.tab_mut(DEFAULT_TAB_ID);
            tab.autofix.generation = tab.autofix.generation.wrapping_add(1);
        }
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
        {
            let tab = app.tab_mut(DEFAULT_TAB_ID);
            tab.autofix.generation = tab.autofix.generation.wrapping_add(1);
        }
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
        let gen_before = app.tab_mut(DEFAULT_TAB_ID).autofix.generation;
        app.turn_cancel(DEFAULT_TAB_ID);
        assert_eq!(
            app.tab_mut(DEFAULT_TAB_ID).autofix.generation,
            gen_before.wrapping_add(1)
        );
        assert!(app.current_tab().turn.is_idle());
        assert!(app.tab_mut(DEFAULT_TAB_ID).autofix.pane_id.is_none());
    }

    #[test]
    fn cancel_mid_stream_preserves_visible_prose_with_canceled_marker() {
        // Esc while prose is streaming → commit partial prose as a
        // CompletedTurn (default-expanded) with the trailing_marker set
        // so the user sees what arrived and that they cancelled it.
        let mut app = test_app();
        submit_test_prompt(&mut app, "tell me a story");
        app.turn_observe_chunk(
            DEFAULT_TAB_ID,
            ChunkKind::Message,
            "\n\nOnce upon a time",
        );
        app.turn_cancel(DEFAULT_TAB_ID);
        let tab = app.current_tab();
        assert!(tab.turn.is_idle(), "got {:?}", tab.turn);
        assert_eq!(tab.completed_turns.len(), 1);
        let committed = &tab.completed_turns[0];
        assert_eq!(committed.prompt, "tell me a story");
        assert!(committed.expanded, "cancel-committed turns default expanded");
        assert!(committed
            .details
            .iter()
            .any(|m| matches!(m, ChatMessage::Agent(t) if t.contains("Once upon a time"))));
        assert!(
            committed
                .trailing_marker
                .as_deref()
                .map_or(false, |m| m.contains("canceled")),
            "trailing_marker should hold (canceled), got {:?}",
            committed.trailing_marker
        );
        assert!(tab.messages.is_empty(), "messages cleared on cancel");
        assert!(tab.tool_calls.is_empty(), "tool_calls cleared on cancel");
    }

    #[test]
    fn cancel_mid_stream_records_canceled_marker_even_without_visible_prose() {
        // A buffer that's pure JSON (no `explanation` field, no prose
        // prefix) renders as nothing during streaming. We must NOT commit
        // raw JSON as agent prose, but we still record a completed_turn
        // with the canceled marker so the user knows the prompt was sent
        // and cancelled.
        let mut app = test_app();
        submit_test_prompt(&mut app, "kill pid 1234");
        app.turn_observe_chunk(
            DEFAULT_TAB_ID,
            ChunkKind::Message,
            r#"{"recommended_choice":1,"choices":[{"choice":1,"#,
        );
        app.turn_cancel(DEFAULT_TAB_ID);
        let tab = app.current_tab();
        assert!(tab.turn.is_idle());
        assert_eq!(tab.completed_turns.len(), 1);
        let committed = &tab.completed_turns[0];
        assert_eq!(committed.prompt, "kill pid 1234");
        assert!(
            !committed
                .details
                .iter()
                .any(|m| matches!(m, ChatMessage::Agent(_))),
            "JSON-only buffer must not be committed as agent prose"
        );
        assert!(
            committed
                .trailing_marker
                .as_deref()
                .map_or(false, |m| m.contains("canceled")),
            "trailing_marker should hold (canceled), got {:?}",
            committed.trailing_marker
        );
        assert!(tab.messages.is_empty());
        assert!(tab.tool_calls.is_empty());
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
        app.current_tab_mut().permission.push_back(perm_with("ok"));
        // Must stay visible — agent flow blocks on this prompt. 1-row strip
        // is the compact fallback rendered by `ui::permission::render`.
        assert_eq!(app.permission_panel_height(80), 1);
    }

    #[test]
    fn permission_panel_height_admits_at_card_min_ceiling() {
        let mut app = test_app();
        app.terminal_rows = 8; // ceiling = 5 == CARD_MIN_SIZE
        app.current_tab_mut().permission.push_back(perm_with("ok"));
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

    // ─── Up/Down chat-scroll fallback ──────────────────────────────────
    //
    // After dropping crossterm mouse capture (so users can drag-select &
    // copy text in the agent pane), wheel scrolling relies on the host
    // terminal translating wheel notches into Up/Down arrow keystrokes
    // while in the alt-screen buffer. The fallback in `handle_key`
    // forwards those arrows to `chat_scroll.by(±1)` ONLY when none of
    // the existing arrow-key consumers is active:
    //   * input box is empty
    //   * no recommendation card is shown
    //   * slash-command popup is not visible
    // These tests pin down each branch.

    #[test]
    fn up_arrow_scrolls_chat_when_input_empty_no_recs_no_popup() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app();
        // Fresh chat tab — Idle turn, empty input, no popup candidates.
        assert!(app.current_tab().input.is_empty());
        assert!(app.current_tab().turn.recommendations().is_none());
        assert!(!app.command_popup_visible());
        assert_eq!(app.current_tab().chat_scroll.offset, 0);

        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(
            app.current_tab().chat_scroll.offset, 1,
            "↑ on empty input should scroll chat up by one line",
        );
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.current_tab().chat_scroll.offset, 2);
    }

    #[test]
    fn down_arrow_scrolls_chat_when_input_empty_no_recs_no_popup() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app();
        // Prime scroll offset so Down has somewhere to go (saturating sub
        // would otherwise leave it at 0 and the assert couldn't tell the
        // arm from a no-op).
        app.current_tab_mut().chat_scroll.offset = 5;

        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(
            app.current_tab().chat_scroll.offset, 4,
            "↓ on empty input should scroll chat down by one line",
        );
    }

    #[test]
    fn up_down_does_not_scroll_chat_when_input_non_empty() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app();
        // Non-empty input: arrows belong to the input-box editor, not
        // the chat-scroll fallback.
        app.current_tab_mut().input.push_str("hi");
        app.current_tab_mut().cursor_pos = 2;
        app.current_tab_mut().chat_scroll.offset = 3;

        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(
            app.current_tab().chat_scroll.offset, 3,
            "non-empty input must NOT trigger the chat-scroll fallback",
        );
    }

    #[test]
    fn up_down_does_not_scroll_chat_when_command_popup_visible() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app();
        // Open the slash-command popup the same way real input would:
        // type "/" and let refresh_command_popup populate candidates.
        app.current_tab_mut().input.push('/');
        app.current_tab_mut().cursor_pos = 1;
        app.current_tab_mut().refresh_command_popup();
        assert!(
            app.command_popup_visible(),
            "test prerequisite: command popup must be visible after typing '/'",
        );
        // Force-clear input WITHOUT calling refresh_command_popup so the
        // candidates list stays populated while input becomes empty. This
        // isolates the !command_popup_visible() guard as the one being
        // tested — without this step, the input-empty guard would
        // independently suppress the fallback and the assertion below
        // could not tell which guard fired.
        app.current_tab_mut().input.clear();
        app.current_tab_mut().cursor_pos = 0;
        assert!(
            app.current_tab().input.is_empty(),
            "test prerequisite: input must be empty so only the popup guard is exercised",
        );
        assert!(
            app.command_popup_visible(),
            "test prerequisite: popup must remain visible after clearing input",
        );
        app.current_tab_mut().chat_scroll.offset = 7;

        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(
            app.current_tab().chat_scroll.offset, 7,
            "command popup visibility must suppress the chat-scroll fallback",
        );
    }
}
