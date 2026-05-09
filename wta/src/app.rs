use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::commands::{self, CommandKind, CommandSpec, ParsedCommand};
use crate::coordinator::{
    parse_autofix_response, parse_recommendation_set, recommended_choice_index,
    validate_recommendation_set_for_coordinator_target, AutofixDecision, RecommendationChoice,
    RecommendationSet,
};
use crate::pane_context::PaneContext;
use crate::preflight::{CheckStatus, PreflightResult};
use crate::protocol::acp::client::{
    prompt_timing_log, CancelRequest, NewSessionForTab, PromptSubmission, RestartRequest,
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
        _ => WtNotification {
            severity: WtEventSeverity::Informational,
            pane_id: pane_id.to_string(),
            summary: format!("Pane {}: {}", pane_id, method),
            acknowledged: false,
            age_ticks: 0,
        },
    }
}

enum FinalizeOutcome {
    None,
    SelectionReady,
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
    },
    /// A new ACP session has been created and bound to a tab. Carries the
    /// per-tab model list (each ACP session can advertise its own).
    SessionAttached {
        tab_id: String,
        session_id: String,
        available_models: Vec<AcpModelInfo>,
        current_model_id: Option<String>,
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
    /// Result of `preflight::check_agent` run by main.rs before the TUI
    /// loop starts. If `all_passed()` is false the App switches into
    /// `AppMode::Setup` so the user can install / authenticate the CLI.
    PreflightComplete(PreflightResult),
}

// --- Per-tab session storage ---

const DEFAULT_TAB_ID: &str = "0";

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
    pub scroll_offset: usize,

    // Streaming state
    pub prompt_in_flight: bool,
    pub agent_streaming: bool,
    pub pending_thought_response: String,
    pub pending_agent_response: String,
    pub progress_status: Option<String>,
    pub activity_frame: usize,
    pub timing_note: Option<String>,
    pub selection_visible_pending: bool,

    // Tool calls / permission / recommendations
    pub tool_calls: HashMap<String, (String, String)>,
    pub permission: Option<PermissionState>,
    pub recommendations: Option<RecommendationSet>,
    pub selected_recommendation: usize,
    pub selected_button: usize,
    pub rec_scroll: usize,

    // Prompt identification / completion staging
    pub current_prompt_id: Option<u64>,
    pub current_prompt_submitted_at_unix_s: Option<f64>,
    pub current_prompt_text: Option<String>,
    pub pending_completed_turn: Option<CompletedTurn>,

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
}

impl TabSession {
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    pub fn clear_recommendations(&mut self) {
        self.recommendations = None;
        self.selected_recommendation = 0;
        self.selected_button = 0;
        self.rec_scroll = 0;
    }

    pub fn clear_chat_history(&mut self) {
        self.messages.clear();
        self.tool_calls.clear();
        self.permission = None;
        self.progress_status = None;
        self.pending_thought_response.clear();
        self.activity_frame = 0;
        self.pending_agent_response.clear();
        self.agent_streaming = false;
        self.scroll_offset = 0;
        self.timing_note = None;
        self.selection_visible_pending = false;
        self.current_prompt_text = None;
        self.current_prompt_submitted_at_unix_s = None;
        self.pending_completed_turn = None;
        self.clear_recommendations();
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

    pub fn clear_completed_turn_history(&mut self) {
        self.messages.clear();
        self.tool_calls.clear();
        self.permission = None;
        self.progress_status = None;
        self.pending_thought_response.clear();
        self.activity_frame = 0;
        self.pending_agent_response.clear();
        self.agent_streaming = false;
        self.scroll_offset = 0;
        self.selection_visible_pending = false;
        self.current_prompt_text = None;
        self.current_prompt_submitted_at_unix_s = None;
    }

    pub fn prepare_for_new_prompt(&mut self, prompt_text: &str) {
        self.clear_chat_history();
        self.current_prompt_text = Some(prompt_text.to_string());
        self.prompt_in_flight = true;
        self.progress_status = Some("Preparing context...".to_string());
        self.activity_frame = 0;
    }

    pub fn current_turn_details(&self) -> Vec<ChatMessage> {
        self.messages
            .iter()
            .filter(|message| !matches!(message, ChatMessage::User(_)))
            .cloned()
            .collect()
    }

    pub fn stage_completed_turn(&mut self, agent_text: String) {
        let Some(prompt) = self.current_prompt_text.clone() else {
            self.pending_completed_turn = None;
            return;
        };

        let mut details = self.current_turn_details();
        details.push(ChatMessage::Agent(agent_text));
        self.pending_completed_turn = Some(CompletedTurn {
            prompt,
            details,
            expanded: false,
        });
    }

    pub fn commit_pending_completed_turn(&mut self) {
        let Some(turn) = self.pending_completed_turn.take() else {
            return;
        };

        self.completed_turns.push(turn);
        self.scroll_to_bottom();
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
    pub state: ConnectionState,
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
    // AgentMessageEnd responses whose generation doesn't match are discarded.
    autofix_generation: u64,
    // Generation captured when the current in-flight autofix prompt was sent.
    // None means the in-flight prompt is not an autofix prompt.
    inflight_autofix_generation: Option<u64>,
    // Per-tab conversation sessions. Keyed by tab_id string (0-based index).
    // The active tab is `tab_id`, with `DEFAULT_TAB_ID` ("0") as fallback
    // before the first `tab_changed` event arrives. Always contains at
    // least an entry for the active tab; lazily extended on first
    // `tab_changed` to a new tab.
    tab_sessions: HashMap<String, TabSession>,
    // Reverse lookup: ACP `SessionId` → tab id. Populated from
    // `AgentConnected` (the implicit tab "0" session) and `SessionAttached`
    // (lazily-created sessions for other tabs). All ACP-emitted events
    // route via this map: chunks, tool calls, end notifications all carry
    // a `session_id`, the App looks up the owning tab and writes to that
    // `TabSession`. Replaces M1's `inflight_tab_id` slot.
    session_to_tab: HashMap<String, String>,
    // ── Agent management view state (re-applied on top of theirs) ──
    /// Live & historical CLI agent sessions. Populated from `agent_event`
    /// hook payloads via `route_agent_event_to_registry`.
    pub agent_sessions: crate::agent_sessions::AgentSessionRegistry,
    /// Current top-level view: chat (default) vs the F2 Agents picker.
    pub current_view: View,
    /// Selection state for the Agents view list widget.
    pub agents_list_state: ratatui::widgets::ListState,
    // Onboarding: signals main.rs to install agent hook plugins on demand.
    install_request_tx: Option<mpsc::UnboundedSender<()>>,
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
    /// Top-level mode: Chat (normal) vs Setup (preflight failed).
    pub mode: AppMode,
    /// Active Setup wizard state (Some iff `mode == AppMode::Setup`).
    pub setup: Option<SetupState>,
    /// Spinner tick counter used by Setup mode (per-tab `activity_frame`
    /// drives chat-mode spinners; this one is for the wizard view which
    /// has no tab context). Bumped from the Tick handler when in Setup.
    pub activity_frame: u8,
}

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

/// App-level mode. Setup mode covers the whole pane (no chat/input/agents)
/// while we wait for the user to install / authenticate the agent CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    /// Normal agent chat / Agents view.
    Chat,
    /// Setup wizard (agent CLI missing or not authenticated).
    Setup,
}

impl Default for AppMode {
    fn default() -> Self {
        AppMode::Chat
    }
}

/// State for the setup wizard screen, populated from `preflight::check_agent`.
#[derive(Debug, Clone)]
pub struct SetupState {
    pub preflight: PreflightResult,
    /// Which check row is currently selected (0 = CLI, 1 = Auth).
    pub selected_index: usize,
    /// True while a `winget install` task is running.
    pub install_in_progress: bool,
    /// Tail of the install command's output (last ~6 lines).
    pub install_log: Vec<String>,
    /// Error message from the most recent install attempt (cleared on retry).
    pub install_error: Option<String>,
}

impl App {
    pub fn new(
        prompt_tx: mpsc::UnboundedSender<PromptSubmission>,
        recommendation_tx: mpsc::UnboundedSender<crate::coordinator::ChoiceExecution>,
        permission_tx: mpsc::UnboundedSender<String>,
        cancel_tx: mpsc::UnboundedSender<CancelRequest>,
        new_session_tx: mpsc::UnboundedSender<NewSessionForTab>,
        restart_tx: mpsc::UnboundedSender<RestartRequest>,
        debug_capture_enabled: Arc<AtomicBool>,
        wt_connected: bool,
        autofix_enabled: bool,
    ) -> Self {
        let mut tab_sessions = HashMap::new();
        tab_sessions.insert(DEFAULT_TAB_ID.to_string(), TabSession::default());
        Self {
            state: ConnectionState::Connecting("Starting agent...".to_string()),
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
            inflight_autofix_generation: None,
            tab_sessions,
            session_to_tab: HashMap::new(),
            agent_sessions: crate::agent_sessions::AgentSessionRegistry::new(),
            current_view: View::Chat,
            agents_list_state: ratatui::widgets::ListState::default(),
            install_request_tx: None,
            source_session_id: None,
            source_cwd: None,
            log_agent_events: false,
            mode: AppMode::Chat,
            setup: None,
            activity_frame: 0,
        }
    }

    /// Wire a sender that signals main.rs to run the agent-hooks installer
    /// (Settings UI → Install button → main.rs spawns
    /// `agent_hooks_installer::ensure_installed`).
    pub fn set_install_request_tx(&mut self, tx: mpsc::UnboundedSender<()>) {
        self.install_request_tx = Some(tx);
    }

    /// Trigger an install-hooks request. No-op if no channel is wired
    /// (e.g. running outside the packaged app).
    #[allow(dead_code)]
    pub fn request_install_hooks(&self) {
        if let Some(tx) = &self.install_request_tx {
            let _ = tx.send(());
        }
    }

    /// Enter handler for the F2 Agents view: focus the underlying WT pane
    /// for the selected row. For terminal-state rows (Ended / Historical)
    /// we currently no-op — resume support requires per-CLI flag handling
    /// (see `dispatch_resume` in HEAD) and will be re-applied separately.
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
                } else {
                    tracing::warn!(
                        target: "agents_view",
                        key = %s.key,
                        "live row has no pane_session_id; Enter is a no-op",
                    );
                }
            }
            Ended | Historical => {
                tracing::debug!(
                    target: "agents_view",
                    key = %s.key,
                    "Enter on terminal-state row: resume not yet wired in this build",
                );
            }
        }
    }

    /// Setup-mode key handler. Active when `mode == AppMode::Setup`.
    /// Esc / Ctrl+C quit; Up/Down move between rows; Enter on the CLI
    /// row (when missing) signals install_request_tx; 'O' opens the
    /// install URL in the system browser.
    fn handle_setup_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Esc => {
                self.should_quit = true;
            }
            KeyCode::Enter => {
                let should_install = self
                    .setup
                    .as_ref()
                    .map(|s| {
                        s.selected_index == 0
                            && s.preflight.cli_status != CheckStatus::Passed
                            && !s.install_in_progress
                            && s.preflight.agent_id == "copilot"
                    })
                    .unwrap_or(false);

                if should_install {
                    if let Some(tx) = &self.install_request_tx {
                        let _ = tx.send(());
                        if let Some(ref mut setup) = self.setup {
                            setup.install_in_progress = true;
                            setup.install_error = None;
                            setup.install_log.clear();
                            setup
                                .install_log
                                .push("Starting GitHub Copilot installation...".to_string());
                        }
                    }
                }
            }
            KeyCode::Char('o') | KeyCode::Char('O') => {
                if let Some(ref setup) = self.setup {
                    if setup.selected_index == 0
                        && setup.preflight.cli_status != CheckStatus::Passed
                    {
                        let url = setup.preflight.install_url.clone();
                        if !url.is_empty() {
                            let _ = open_url_in_browser(&url);
                        }
                    }
                }
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
                    if setup.selected_index < 1 {
                        setup.selected_index += 1;
                    }
                }
            }
            _ => {}
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
    /// tab as a fallback when the session is unknown — covers events
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

        let flush_started = std::time::Instant::now();
        terminal.flush()?;
        ui_trace::log_slow("terminal_flush", flush_started.elapsed(), || {
            self.trace_state()
        });

        let cursor_started = std::time::Instant::now();
        match ui::input_cursor_position(self, area) {
            Some(position) => {
                terminal.show_cursor()?;
                terminal.set_cursor_position(position)?;
            }
            None => {
                terminal.hide_cursor()?;
            }
        }
        ui_trace::log_slow("terminal_cursor", cursor_started.elapsed(), || {
            self.trace_state()
        });

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
            AppEvent::PromptTemplateLoaded { .. } => "prompt_template_loaded",
            AppEvent::AgentError { .. } => "agent_error",
            AppEvent::AgentBusy { .. } => "agent_busy",
            AppEvent::ExecutionInfo(_) => "execution_info",
            AppEvent::AgentThoughtChunk { .. } => "agent_thought_chunk",
            AppEvent::AgentMessageChunk { .. } => "agent_message_chunk",
            AppEvent::AgentMessageEnd { .. } => "agent_message_end",
            AppEvent::TimingMetric { .. } => "timing_metric",
            AppEvent::ToolCall { .. } => "tool_call",
            AppEvent::ToolCallUpdate { .. } => "tool_call_update",
            AppEvent::Plan { .. } => "plan",
            AppEvent::PermissionRequest { .. } => "permission_request",
            AppEvent::SystemMessage(_) => "system_message",
            AppEvent::DebugPipeMessage(_) => "debug_pipe_message",
            AppEvent::WtEvent { .. } => "wt_event",
            AppEvent::PreflightComplete(_) => "preflight_complete",
        }
    }

    fn trace_state(&self) -> String {
        let tab = self.current_tab();
        format!(
            "state={:?} messages={} completed_turns={} input_chars={} thought_chars={} pending_chars={} scroll={} streaming={} activity_frame={} recommendations={} permission={} timing_note={}",
            self.state,
            tab.messages.len(),
            tab.completed_turns.len(),
            tab.input.chars().count(),
            tab.pending_thought_response.chars().count(),
            tab.pending_agent_response.chars().count(),
            tab.scroll_offset,
            tab.agent_streaming,
            tab.activity_frame,
            tab.recommendations
                .as_ref()
                .map(|recs| recs.choices.len())
                .unwrap_or(0),
            tab.permission.is_some(),
            tab.timing_note.is_some()
        )
    }

    fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Key(key) => self.handle_key(key),
            AppEvent::MouseScroll { delta, row } => {
                if self.current_tab_mut().recommendations.is_some() {
                    // Route based on where the mouse is.
                    // Recs panel sits just above the input (bottom of screen).
                    let input_h: u16 = 3; // INPUT_MIN_HEIGHT
                    let rec_h = self.rec_panel_height();
                    let recs_top = self.terminal_rows.saturating_sub(input_h + rec_h);
                    if row >= recs_top {
                        // Mouse is in the recs area: scroll the recommendation panel.
                        // Ratatui scroll(n,0) skips n lines from the top, so:
                        //   delta>0 (wheel down) → show lower content → rec_scroll increases
                        //   delta<0 (wheel up)   → show higher content → rec_scroll decreases
                        if delta > 0 {
                            self.current_tab_mut().rec_scroll = self.current_tab_mut().rec_scroll.saturating_add(delta as usize);
                        } else {
                            self.current_tab_mut().rec_scroll = self.current_tab_mut().rec_scroll.saturating_sub((-delta) as usize);
                        }
                    } else {
                        // Mouse is in the chat area: scroll chat history.
                        if delta < 0 {
                            self.current_tab_mut().scroll_offset = self.current_tab_mut().scroll_offset.saturating_add((-delta) as usize);
                        } else {
                            self.current_tab_mut().scroll_offset = self.current_tab_mut().scroll_offset.saturating_sub(delta as usize);
                        }
                    }
                } else {
                    // No recs visible — scroll chat.
                    if delta < 0 {
                        self.current_tab_mut().scroll_offset = self.current_tab_mut().scroll_offset.saturating_add((-delta) as usize);
                    } else {
                        self.current_tab_mut().scroll_offset = self.current_tab_mut().scroll_offset.saturating_sub(delta as usize);
                    }
                }
            }
            AppEvent::Tick => {
                // Fan out across all tabs: a background tab with an in-flight
                // prompt should keep its spinner advancing so when the user
                // switches back the animation is in step. Must match
                // ACTIVITY_HIGHLIGHT_WINDOWS.len() in ui/chat.rs.
                for tab in self.tab_sessions.values_mut() {
                    if tab.prompt_in_flight
                        || tab.agent_streaming
                        || tab.progress_status.is_some()
                    {
                        tab.activity_frame = (tab.activity_frame + 1) % 10;
                    }
                }
                // Setup-mode spinner: ticks while we're showing the wizard
                // (e.g. spinning during a `winget install` background job).
                if self.mode == AppMode::Setup {
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
            } => {
                self.agent_name = name;
                self.agent_model = model;
                self.agent_version = version;
                self.session_id = session_id.clone();
                self.available_models = available_models.clone();
                self.current_model_id = current_model_id.clone();
                self.state = ConnectionState::Connected;
                // Bind the startup session to the implicit tab "0" — the
                // ACP client lazy-creates a session per-tab, but the
                // initial one is for tab "0" by convention.
                self.session_to_tab
                    .insert(session_id.clone(), DEFAULT_TAB_ID.to_string());
                let default_tab = self.tab_mut(DEFAULT_TAB_ID);
                default_tab.session_id = Some(session_id);
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
                self.state = ConnectionState::Failed(message.clone());
                self.publish_agent_status();
                let tab = match session_id.as_deref() {
                    Some(sid) => self.session_tab_mut(sid),
                    None => self.current_tab_mut(),
                };
                tab.prompt_in_flight = false;
                tab.agent_streaming = false;
                tab.progress_status = None;
                tab.pending_thought_response.clear();
                tab.activity_frame = 0;
                tab.pending_agent_response.clear();
                tab.timing_note = None;
                tab.pending_completed_turn = None;
                tab.messages.push(ChatMessage::Error(message));
            }
            AppEvent::ExecutionInfo(message) => {
                self.push_execution_info(message);
                self.current_tab_mut().scroll_to_bottom();
            }
            AppEvent::AgentThoughtChunk { session_id, text } => {
                let tab = self.session_tab_mut(&session_id);
                // If the user cancelled this prompt (or it already
                // completed) we drop the late chunk rather than re-arming
                // the spinner.
                if !tab.prompt_in_flight {
                    return;
                }
                if tab.progress_status.is_none() {
                    tab.progress_status = Some("Thinking...".to_string());
                }
                append_thought_preview(&mut tab.pending_thought_response, &text);
            }
            AppEvent::AgentMessageChunk { session_id, text } => {
                let tab = self.session_tab_mut(&session_id);
                if !tab.prompt_in_flight {
                    return;
                }
                tab.agent_streaming = true;
                tab.progress_status = None;
                tab.pending_thought_response.clear();
                tab.pending_agent_response.push_str(&text);
            }
            AppEvent::AgentMessageEnd { session_id } => {
                // Check if this response is stale (generation bumped since we sent).
                let is_stale_autofix = match self.inflight_autofix_generation {
                    Some(gen) => gen != self.autofix_generation,
                    None => false,
                };

                if is_stale_autofix {
                    // Discard: a newer error or cancel superseded this response.
                    tracing::info!(target: "autofix", inflight_gen = ?self.inflight_autofix_generation, current_gen = self.autofix_generation, "discarding stale autofix response");
                    {
                        let tab = self.session_tab_mut(&session_id);
                        tab.agent_streaming = false;
                        tab.prompt_in_flight = false;
                        tab.progress_status = None;
                        tab.pending_thought_response.clear();
                        tab.pending_agent_response.clear();
                        tab.activity_frame = 0;
                    }
                    self.inflight_autofix_generation = None;
                    return;
                }

                // Always reset streaming flags so autofix guards don't get stuck.
                {
                    let tab = self.session_tab_mut(&session_id);
                    tab.agent_streaming = false;
                    tab.prompt_in_flight = false;
                    tab.progress_status = None;
                    tab.pending_thought_response.clear();
                    tab.activity_frame = 0;
                }
                self.inflight_autofix_generation = None;

                if let Some(summary) = self.session_completion_latency_summary(&session_id) {
                    self.push_execution_info(summary);
                }
                match self.finalize_agent_response_for(&session_id) {
                    FinalizeOutcome::SelectionReady => {
                        self.session_tab_mut(&session_id)
                            .clear_completed_turn_history();
                    }
                    FinalizeOutcome::None => {
                        self.session_tab_mut(&session_id).scroll_to_bottom();
                    }
                }
            }
            AppEvent::TimingMetric { session_id, note } => {
                self.session_tab_mut(&session_id).timing_note = Some(note);
            }
            AppEvent::ToolCall { session_id, id, title, status } => {
                let tab = self.session_tab_mut(&session_id);
                if !tab.prompt_in_flight {
                    return;
                }
                tab.tool_calls
                    .insert(id.clone(), (title.clone(), status.clone()));
                tab.messages
                    .push(ChatMessage::ToolCall { id, title, status });
                tab.scroll_to_bottom();
            }
            AppEvent::ToolCallUpdate { session_id, id, status } => {
                let tab = self.session_tab_mut(&session_id);
                if !tab.prompt_in_flight {
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
                if !tab.prompt_in_flight {
                    return;
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
                if !tab.prompt_in_flight {
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
                    self.mode = AppMode::Setup;
                    self.setup = Some(SetupState {
                        preflight: result,
                        selected_index: 0,
                        install_in_progress: false,
                        install_log: Vec::new(),
                        install_error: None,
                    });
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
                        // If discover_pane_identity failed at startup, self.tab_id is None.
                        // Use from_tab_id (sent by C++) to initialize it before saving.
                        if self.tab_id.is_none() {
                            if let Some(from_id) = params.get("from_tab_id").and_then(|v| v.as_str()) {
                                tracing::info!(target: "tab_session", from_tab_id = from_id, "initializing tab_id from from_tab_id");
                                self.tab_id = Some(from_id.to_string());
                            }
                        }
                        self.switch_tab_session(new_tab_id.to_string());
                    } else {
                        tracing::warn!(target: "tab_session", "tab_changed: missing tab_id in params");
                    }
                    return;
                }

                // Skip events from our own pane
                if self.pane_id.as_deref() == Some(pane_id.as_str()) {
                    tracing::debug!(target: "autofix", "skipped: own pane");
                    return;
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
                        if self.autofix_enabled {
                            // maybe_trigger_autofix pushes ChatMessage::Error (red dot)
                            // itself — don't double-push here as a System message.
                            self.maybe_trigger_autofix(&notification);
                        } else {
                            // Autofix disabled: surface the event in chat so the
                            // user still sees it.
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
                                self.autofix_generation = self.autofix_generation.wrapping_add(1);
                                // Do NOT clear inflight_autofix_generation: the stale
                                // check in AgentMessageEnd relies on Some(old) != new_gen.
                                let pane = self.autofix_pane_id.take().unwrap();
                                self.clear_recommendations();
                                self.current_tab_mut().prompt_in_flight = false;
                                self.current_tab_mut().agent_streaming = false;
                                self.current_tab_mut().progress_status = None;
                                self.emit_autofix_state_cleared(&pane);
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
        }
    }

    fn event_requires_redraw(&self, event: &AppEvent) -> bool {
        match event {
            AppEvent::Tick => self.has_activity_indicator() || self.show_notification_banner,
            AppEvent::AgentMessageChunk { .. } => true,
            AppEvent::DebugPipeMessage(_) => self.show_debug_panel,
            _ => true,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        tracing::info!(
            target: "input",
            code = ?key.code,
            modifiers = ?key.modifiers,
            input_empty = self.current_tab().input.is_empty(),
            recs = self.current_tab().recommendations.is_some(),
            turns = self.current_tab().completed_turns.len(),
            selected_turn = ?self.current_tab().selected_completed_turn_idx,
            "key received"
        );

        // Setup mode (preflight failed): route everything to the setup key
        // handler. No agent / chat keybindings apply in this mode.
        if self.mode == AppMode::Setup {
            self.handle_setup_key(key);
            return;
        }

        // Agents view (F2): list navigation + Enter to focus pane + Delete
        // to evict an Ended/Historical row. Captures all input while open
        // — including Esc which closes the view.
        if self.current_view == View::Agents {
            let count = self.agent_sessions.iter_sorted().len();
            match key.code {
                KeyCode::Down => {
                    let cur = self.agents_list_state.selected().unwrap_or(0);
                    let next = if count == 0 { 0 } else { (cur + 1).min(count - 1) };
                    self.agents_list_state.select(Some(next));
                }
                KeyCode::Up => {
                    let cur = self.agents_list_state.selected().unwrap_or(0);
                    self.agents_list_state.select(Some(cur.saturating_sub(1)));
                }
                KeyCode::Enter => {
                    if let Some(idx) = self.agents_list_state.selected() {
                        let selected = self
                            .agent_sessions
                            .iter_sorted()
                            .get(idx)
                            .map(|s| (*s).clone());
                        if let Some(s) = selected {
                            self.activate_agent_session(&s);
                        }
                    }
                }
                KeyCode::Delete => {
                    if let Some(idx) = self.agents_list_state.selected() {
                        let target = self
                            .agent_sessions
                            .iter_sorted()
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
                                let new_count = self.agent_sessions.iter_sorted().len();
                                if new_count == 0 {
                                    self.agents_list_state.select(None);
                                } else if idx >= new_count {
                                    self.agents_list_state.select(Some(new_count - 1));
                                }
                            }
                        }
                    }
                }
                KeyCode::Esc => {
                    self.current_view = View::Chat;
                }
                _ => {}
            }
            return;
        }

        // If permission modal is showing, route keys there
        if let Some(ref mut perm) = self.current_tab_mut().permission {
            match key.code {
                KeyCode::Up => {
                    if perm.selected > 0 {
                        perm.selected -= 1;
                    }
                }
                KeyCode::Down => {
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
            KeyCode::Up if self.current_tab().input.is_empty() && self.current_tab_mut().recommendations.is_some() => {
                if self.current_tab_mut().selected_recommendation > 0 {
                    self.current_tab_mut().selected_recommendation -= 1;
                    self.current_tab_mut().selected_button = self.default_button_for_selected();
                    self.scroll_rec_to_selected();
                }
            }
            KeyCode::Down if self.current_tab().input.is_empty() && self.current_tab().recommendations.is_some() => {
                let choices_len = self
                    .current_tab()
                    .recommendations
                    .as_ref()
                    .map(|r| r.choices.len())
                    .unwrap_or(0);
                if self.current_tab().selected_recommendation + 1 < choices_len {
                    let default_btn = self.default_button_for_selected();
                    self.current_tab_mut().selected_recommendation += 1;
                    self.current_tab_mut().selected_button = default_btn;
                    self.scroll_rec_to_selected();
                }
            }
            KeyCode::Right | KeyCode::Tab
                if self.current_tab().input.is_empty() && self.current_tab_mut().recommendations.is_some() =>
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
                    && self.current_tab().recommendations.is_none()
                    && !self.current_tab().completed_turns.is_empty() =>
            {
                self.current_tab_mut().select_older_completed_turn();
            }
            KeyCode::BackTab
                if self.current_tab().input.is_empty()
                    && self.current_tab().recommendations.is_none()
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
                if self.current_tab().input.is_empty() && self.current_tab_mut().recommendations.is_some() =>
            {
                // Cycle button focus backward.
                let button_count = self.button_count_for_selected();
                if button_count > 1 {
                    self.current_tab_mut().selected_button = (self.current_tab_mut().selected_button + button_count - 1) % button_count;
                }
            }
            KeyCode::F(2) => {
                // Toggle between Chat (default) and the Agents picker.
                self.current_view = match self.current_view {
                    View::Chat => {
                        // Seed selection on first open if there's anything to select.
                        if self.agents_list_state.selected().is_none()
                            && !self.agent_sessions.iter_sorted().is_empty()
                        {
                            self.agents_list_state.select(Some(0));
                        }
                        View::Agents
                    }
                    View::Agents => View::Chat,
                };
                return;
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
                let in_flight = self.current_tab().prompt_in_flight
                    || self.current_tab().agent_streaming;
                if in_flight {
                    // Send a session/cancel to the ACP client. The client
                    // will fire the protocol notification and signal the
                    // per-prompt oneshot so the spawned task drops out of
                    // conn.prompt() immediately.
                    let session_id = self.current_tab().session_id.clone();
                    if let Some(sid) = session_id {
                        let _ = self.cancel_tx.send(CancelRequest { session_id: sid });
                    }
                    // Optimistically reset the local UI so the spinner
                    // stops immediately — don't wait for the agent's
                    // cancelled-end roundtrip. Late chunks for this prompt
                    // are dropped by the chunk handlers (they bail on
                    // !prompt_in_flight).
                    let tab = self.current_tab_mut();
                    tab.prompt_in_flight = false;
                    tab.agent_streaming = false;
                    tab.pending_agent_response.clear();
                    tab.pending_thought_response.clear();
                    tab.progress_status = None;
                    tab.activity_frame = 0;
                    tab.pending_completed_turn = None;
                    tab.messages.push(ChatMessage::System("Cancelled.".to_string()));
                    tab.scroll_to_bottom();
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Esc if self.help_overlay_visible => {
                self.help_overlay_visible = false;
            }
            KeyCode::Esc if self.show_notification_banner => {
                self.dismiss_notifications();
            }
            KeyCode::Esc
                if self.current_tab_mut().recommendations.is_some()
                    || (self.autofix_pane_id.is_some() && self.current_tab_mut().prompt_in_flight) =>
            {
                // Dismiss armed fix card or cancel in-flight autofix request.
                self.autofix_generation = self.autofix_generation.wrapping_add(1);
                let pane = self.autofix_pane_id.take();
                self.clear_recommendations();
                self.current_tab_mut().prompt_in_flight = false;
                self.current_tab_mut().agent_streaming = false;
                self.current_tab_mut().progress_status = None;
                self.inflight_autofix_generation = None;
                if let Some(p) = pane {
                    self.emit_autofix_state_cleared(&p);
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
                    && self.current_tab().selected_completed_turn_idx.is_some() =>
            {
                // A past turn is highlighted via Tab — Enter toggles its
                // expanded state instead of submitting / activating recs.
                self.current_tab_mut().toggle_selected_completed_turn();
            }
            KeyCode::Enter => {
                let _tab = self.current_tab();
                tracing::debug!(target: "autofix", input_empty = _tab.input.is_empty(), state = ?self.state, has_recs = _tab.recommendations.is_some(), autofix_pane = ?self.autofix_pane_id, selected_idx = _tab.selected_recommendation, "Enter");
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
                    && self.current_tab_mut().recommendations.is_some()
                {
                    if let Some(mut choice) = self.selected_recommendation_choice().cloned() {
                        // Send: index 0 = Run, index 1 = Insert.
                        // OpenAndSend: sole index 0 = open target.
                        let insert_only = self.current_tab_mut().selected_button == 1
                            && self.is_send_choice(&choice);
                        tracing::info!(target: "autofix", choice = choice.choice, actions = choice.actions.len(), insert_only, "Executing choice");
                        // Auto-fill parent for Send actions from auto-fix.
                        if let Some(ref pane_id) = self.autofix_pane_id {
                            for action in &mut choice.actions {
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
                        let armed_pane = self.autofix_pane_id.take();
                        self.current_tab_mut().commit_pending_completed_turn();
                        self.clear_recommendations();
                        let label = if insert_only { "Inserting" } else { "Executing" };
                        self.push_execution_info(format!("{} choice {}.", label, choice.choice));
                        let _ = self.recommendation_tx.send(
                            crate::coordinator::ChoiceExecution { choice, insert_only }
                        );
                        // Clear the bottom-bar Armed state — the fix has been
                        // dispatched to the source pane.
                        if let Some(pane_id) = armed_pane {
                            self.emit_autofix_state_cleared(&pane_id);
                        }
                    }
                } else if !self.current_tab().input.is_empty() && self.state == ConnectionState::Connected {
                    // Same-tab single-flight: refuse a new prompt if this
                    // tab is still streaming the previous one. The ACP
                    // client enforces this server-side too, but bouncing
                    // here keeps the user's input intact instead of
                    // appearing to drop it.
                    if self.current_tab().prompt_in_flight {
                        let tab = self.current_tab_mut();
                        tab.messages.push(ChatMessage::System(
                            "Agent is busy on this tab — wait for the current prompt to finish."
                                .to_string(),
                        ));
                        tab.scroll_to_bottom();
                        return;
                    }
                    // The Enter handler always operates on the active tab —
                    // the user is by definition on the tab they're typing
                    // in. Routing of subsequent ACP events back into this
                    // tab is keyed on the SessionId attached to it.
                    let tab = self.current_tab_mut();
                    let text = std::mem::take(&mut tab.input);
                    tab.cursor_pos = 0;
                    tab.refresh_command_popup();
                    tab.prepare_for_new_prompt(&text);
                    tab.messages.push(ChatMessage::User(text.clone()));
                    tab.scroll_to_bottom();
                    let pane_context = PaneContext {
                        pane_id: self.pane_id.clone(),
                        tab_id: self.tab_id.clone(),
                        window_id: self.window_id.clone(),
                        cwd: None,
                        source_pane_id: None,
                    };
                    let prompt = PromptSubmission::new(text, Some(pane_context));
                    let tab = self.current_tab_mut();
                    tab.current_prompt_id = Some(prompt.id);
                    tab.current_prompt_submitted_at_unix_s = Some(prompt.submitted_at_unix_s);
                    tab.selection_visible_pending = false;
                    prompt_timing_log(
                        prompt.id,
                        prompt.submitted_at_unix_s,
                        "ui_submit",
                        &format!("preview={:?}", prompt.preview()),
                    );
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
                self.current_tab_mut().scroll_offset = self.current_tab_mut().scroll_offset.saturating_add(10);
            }
            KeyCode::PageDown => {
                self.current_tab_mut().scroll_offset = self.current_tab_mut().scroll_offset.saturating_sub(10);
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
        let tab = self.current_tab();
        tab.prompt_in_flight || tab.agent_streaming || tab.progress_status.is_some()
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
        let in_flight = self.current_tab().prompt_in_flight;
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
                    if let Some(sid) = session_id {
                        let _ = self.cancel_tx.send(CancelRequest { session_id: sid });
                    }
                    let tab = self.current_tab_mut();
                    tab.prompt_in_flight = false;
                    tab.agent_streaming = false;
                    tab.pending_agent_response.clear();
                    tab.pending_thought_response.clear();
                    tab.progress_status = None;
                    tab.activity_frame = 0;
                    tab.pending_completed_turn = None;
                    tab.messages
                        .push(ChatMessage::System("Cancelled.".to_string()));
                    tab.scroll_to_bottom();
                } else {
                    let tab = self.current_tab_mut();
                    tab.messages
                        .push(ChatMessage::System("No prompt in flight.".to_string()));
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
                    tab.prompt_in_flight = false;
                    tab.agent_streaming = false;
                    tab.pending_agent_response.clear();
                    tab.pending_thought_response.clear();
                    tab.progress_status = None;
                    tab.activity_frame = 0;
                    tab.pending_completed_turn = None;
                }
                let _ = self.restart_tx.send(RestartRequest);
                self.publish_agent_status();
            }
        }
    }

    /// Height of the recommendations panel — grows to fit content, capped at 40% of pane height.
    pub fn rec_panel_height(&self) -> u16 {
        let recs = match self.current_tab().recommendations.as_ref() {
            Some(r) => r,
            None => return 0,
        };
        // Compute actual total height based on real card content (accounts for wrapped code).
        let panel_width = self.terminal_cols;
        let total_needed: u16 = recs
            .choices
            .iter()
            .map(|c| rec_card_height(c, panel_width) as u16)
            .sum::<u16>()
            .saturating_add(1); // hint line
        // Leave at least 3 rows for chat + 3 for input.
        let max = self.terminal_rows.saturating_sub(6).max(8);
        total_needed.min(max).max(8)
    }

    fn clear_recommendations(&mut self) {
        self.current_tab_mut().clear_recommendations();
    }

    /// Adjusts rec_scroll so the selected recommendation card's title is at the top of the panel.
    fn scroll_rec_to_selected(&mut self) {
        let panel_height = self.rec_panel_height() as usize; // actual panel size, not full pane
        let panel_width = self.terminal_cols;
        let Some(recs) = self.current_tab_mut().recommendations.clone() else { return };

        // Accumulate line offsets to find the exact top of the selected card.
        let mut line_top: usize = 0;
        for (idx, choice) in recs.choices.iter().enumerate() {
            let card_h = rec_card_height(choice, panel_width);
            if idx == self.current_tab_mut().selected_recommendation {
                // Scroll so title is at the top; if the card fits, keep it fully visible.
                let card_bottom = line_top + card_h;
                if line_top < self.current_tab_mut().rec_scroll {
                    self.current_tab_mut().rec_scroll = line_top;
                } else if card_bottom > self.current_tab_mut().rec_scroll + panel_height {
                    self.current_tab_mut().rec_scroll = line_top;
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


    fn session_completion_latency_summary(&self, session_id: &str) -> Option<String> {
        let mut parts = Vec::new();
        let tab = self.session_tab(session_id);

        if let Some(submitted_at) = tab.current_prompt_submitted_at_unix_s {
            let total_s = (now_unix_s() - submitted_at).max(0.0);
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

        // Pass pipe credentials from environment (set when agent pane was created).
        if let Ok(pipe_name) = std::env::var("WT_PIPE_NAME") {
            cmd.arg("--pipe-name").arg(&pipe_name);
        }
        if let Ok(token) = std::env::var("WT_MCP_TOKEN") {
            cmd.arg("--pipe-token").arg(&token);
        }

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

        // Latest event always wins. If we're Pending/Armed for a different
        // pane, or Armed for the same pane, bump the generation to invalidate
        // any in-flight response and start fresh.
        let same_pane = self.autofix_pane_id.as_deref() == Some(notification.pane_id.as_str());

        if same_pane && self.current_tab_mut().prompt_in_flight {
            // Same pane, already Pending: re-emit pending with new summary
            // but don't send another prompt (agent is already working on it).
            tracing::info!(target: "autofix", pane_id = %notification.pane_id, "autofix re-trigger same pane while pending — re-emit only");
            self.emit_autofix_state_pending(&notification.pane_id, &notification.summary);
            return;
        }

        // For all other cases (different pane, or Armed state, or Idle):
        // bump generation to stale any in-flight response, clear current state.
        self.autofix_generation = self.autofix_generation.wrapping_add(1);
        self.clear_recommendations();
        self.current_tab_mut().agent_streaming = false;
        self.current_tab_mut().prompt_in_flight = false;
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

        // Store the failing pane ID so we can auto-fill `parent` on execution.
        self.autofix_pane_id = Some(notification.pane_id.clone());

        self.current_tab_mut().prompt_in_flight = true;
        self.inflight_autofix_generation = Some(self.autofix_generation);
        self.current_tab_mut().progress_status = Some("Preparing context...".to_string());
        self.current_tab_mut().activity_frame = 0;

        let prompt = PromptSubmission::new_autofix(prompt_text, Some(pane_context));
        self.current_tab_mut().current_prompt_id = Some(prompt.id);
        self.current_tab_mut().current_prompt_submitted_at_unix_s = Some(prompt.submitted_at_unix_s);
        tracing::info!(target: "autofix", pane_id = %notification.pane_id, generation = self.autofix_generation, "sending auto-fix prompt");
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
        tracing::info!(target: "autofix", requested_pane = %requested_pane_id, armed_pane = ?self.autofix_pane_id, has_recs = self.current_tab().recommendations.is_some(), "autofix_execute received");
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
        let rec = match self.current_tab_mut().recommendations.clone() {
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
        self.autofix_pane_id = None;
        self.current_tab_mut().commit_pending_completed_turn();
        self.clear_recommendations();
        self.push_execution_info(format!("Auto-executing choice {}.", choice.choice));
        let _ = self
            .recommendation_tx
            .send(crate::coordinator::ChoiceExecution {
                choice,
                insert_only: false,
            });
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
        tab.recommendations
            .as_ref()
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

    fn finalize_agent_response_for(&mut self, session_id: &str) -> FinalizeOutcome {
        if self.session_tab(session_id).pending_agent_response.trim().is_empty() {
            self.log_selection_phase_for(session_id, "selection_parse_failed", "reason=empty_agent_response");
            return FinalizeOutcome::None;
        }

        let text = std::mem::take(&mut self.session_tab_mut(session_id).pending_agent_response);

        // Autofix responses use a minimal prompt/format; parse them separately.
        if self.autofix_pane_id.is_some() {
            return self.finalize_autofix_response_for(session_id, text);
        }

        match parse_recommendation_set(&text).and_then(|recommendations| {
            validate_recommendation_set_for_coordinator_target(
                &recommendations,
                self.pane_id.as_deref(),
            )
        }) {
            Ok(recommendations) => {
                let rec_idx = recommended_choice_index(&recommendations);
                let choice_count = recommendations.choices.len();
                let recommended_choice = recommendations.recommended_choice;
                let tab = self.session_tab_mut(session_id);
                tab.stage_completed_turn(text);
                tab.selected_recommendation = rec_idx;
                tab.recommendations = Some(recommendations);
                tab.selection_visible_pending = true;
                self.log_selection_phase_for(
                    session_id,
                    "selection_ready",
                    &format!(
                        "choice_count={} recommended_choice={:?}",
                        choice_count, recommended_choice
                    ),
                );
                FinalizeOutcome::SelectionReady
            }
            Err(err) => {
                let error_text = format!("{:#}", err).replace('\n', " | ");
                let chars = text.chars().count();
                let has_prompt = self.session_tab(session_id).current_prompt_text.is_some();
                {
                    let tab = self.session_tab_mut(session_id);
                    tab.clear_recommendations();
                    tab.pending_completed_turn = None;
                }
                self.log_selection_phase_for(
                    session_id,
                    "selection_parse_failed",
                    &format!(
                        "response_chars={} error={:?}",
                        chars, error_text
                    ),
                );
                let tab = self.session_tab_mut(session_id);
                if has_prompt {
                    tab.stage_completed_turn(text);
                    tab.commit_pending_completed_turn();
                    tab.clear_chat_history();
                } else {
                    tab.prompt_in_flight = false;
                    tab.progress_status = None;
                    tab.agent_streaming = false;
                }
                FinalizeOutcome::None
            }
        }
    }

    fn finalize_autofix_response_for(&mut self, session_id: &str, text: String) -> FinalizeOutcome {
        let pane_id = match self.autofix_pane_id.clone() {
            Some(p) => p,
            None => return FinalizeOutcome::None,
        };

        match parse_autofix_response(&text) {
            AutofixDecision::Fix(recommendations) => {
                self.log_selection_phase_for(
                    session_id,
                    "autofix_fix",
                    &format!("pane={pane_id} title={:?}", recommendations.choices.first().map(|c| &c.title)),
                );
                let preview = Self::armed_fix_preview(&recommendations);
                self.emit_autofix_state_armed(&pane_id, &preview);
                let rec_idx = recommended_choice_index(&recommendations);
                let tab = self.session_tab_mut(session_id);
                tab.selected_recommendation = rec_idx;
                tab.recommendations = Some(recommendations);
                tab.selection_visible_pending = true;
                FinalizeOutcome::SelectionReady
            }
            AutofixDecision::Explain { title, explanation } => {
                self.log_selection_phase_for(
                    session_id,
                    "autofix_explain",
                    &format!(
                        "pane={pane_id} title={title:?} chars={}",
                        explanation.chars().count()
                    ),
                );

                // Stage the explanation as a chat turn so opening the agent
                // pane reveals it. The autofix prompt is internal so we use a
                // human-readable label as the turn's "prompt" line.
                let turn_prompt = format!("Auto-diagnosed error in pane {pane_id}");
                {
                    let tab = self.session_tab_mut(session_id);
                    let mut details = tab.current_turn_details();
                    details.push(ChatMessage::Agent(explanation));
                    tab.pending_completed_turn = Some(CompletedTurn {
                        prompt: turn_prompt,
                        details,
                        expanded: false,
                    });
                    tab.commit_pending_completed_turn();
                }

                self.emit_autofix_state_suggested(&pane_id, &title);

                // No executable action to remember, but keep `suggested_pane_id`
                // so a successful next command in the same pane can dismiss the
                // bottom bar indicator.
                self.suggested_pane_id = Some(pane_id.clone());
                self.autofix_pane_id = None;
                let tab = self.session_tab_mut(session_id);
                tab.clear_recommendations();
                tab.prompt_in_flight = false;
                tab.progress_status = None;
                tab.agent_streaming = false;
                FinalizeOutcome::None
            }
            AutofixDecision::Ignore => {
                self.log_selection_phase_for(session_id, "autofix_ignore", &format!("pane={pane_id}"));
                self.autofix_pane_id = None;
                self.emit_autofix_state_cleared(&pane_id);
                let tab = self.session_tab_mut(session_id);
                tab.clear_recommendations();
                tab.prompt_in_flight = false;
                tab.progress_status = None;
                tab.agent_streaming = false;
                FinalizeOutcome::None
            }
        }
    }

    fn log_selection_phase_for(&self, session_id: &str, phase: &str, details: &str) {
        // log against the in-flight tab so traces stay coherent with where
        // the prompt was submitted, even after the user switches tabs.
        let tab = self.session_tab(session_id);
        if let (Some(prompt_id), Some(submitted_at_unix_s)) =
            (tab.current_prompt_id, tab.current_prompt_submitted_at_unix_s)
        {
            prompt_timing_log(prompt_id, submitted_at_unix_s, phase, details);
        }
    }

    fn log_selection_visible_if_needed(&mut self) {
        let tab = self.current_tab();
        if !tab.selection_visible_pending || tab.recommendations.is_none() {
            return;
        }
        let details = format!(
            "choice_count={} selected_index={}",
            tab.recommendations
                .as_ref()
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

const THOUGHT_PREVIEW_MAX_CHARS: usize = 1024;

/// Computes the rendered height (in terminal rows) of a recommendation card.
///
/// Card structure: title + top border + content lines + separator + buttons + bottom border + blank
/// Content lines wrap based on the inner width of the card.
fn rec_card_height(choice: &RecommendationChoice, panel_width: u16) -> usize {
    use crate::coordinator::RecommendedAction;
    // Must match the wrapping width used in `recommendations::render`:
    //   h_rec horizontal padding (1 + 1) + card outer indent (2 + 2) + inner card padding (2 + 2) = 10.
    let inner_width = (panel_width as usize).saturating_sub(10).max(1);

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

    // title(at most 1) + top_pad(1) + content + divider(1) + buttons(1) + bottom_pad(1) + blank(1)
    // No outer border — card is a filled rectangle with a single divider
    // and one row of CARD_BG padding above/below the content groups.
    6 + content_lines
}

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
    fn publish_agent_status(&self) {
        let state_str = match &self.state {
            ConnectionState::Connecting(_) => "connecting",
            ConnectionState::Connected => "connected",
            ConnectionState::Failed(_) => "failed",
            ConnectionState::Disconnected => "disconnected",
        };
        let evt = serde_json::json!({
            "type": "event",
            "method": "agent_status",
            "params": {
                "name": self.agent_name,
                "version": self.agent_version,
                "model": self.agent_model,
                "state": state_str,
                // Empty array (not null/missing) when no models advertised, so
                // the C++ side can use "array length > 0" as the "show dropdown"
                // signal without ambiguity.
                "available_models": self.available_models,
                "current_model_id": self.current_model_id,
            }
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
        let (restart_tx, _restart_rx) = tokio::sync::mpsc::unbounded_channel();
        let debug_capture = Arc::new(AtomicBool::new(false));
        App::new(prompt_tx, recommendation_tx, permission_tx, cancel_tx, new_session_tx, restart_tx, debug_capture, true, false)
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
}
