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

use crate::coordinator::{
    parse_autofix_response, parse_recommendation_set, recommended_choice_index,
    validate_recommendation_set_for_coordinator_target, AutofixDecision, RecommendationChoice,
    RecommendationSet,
};
use crate::preflight::{CheckStatus, PreflightResult};
use crate::protocol::acp::client::{prompt_timing_log, PromptSubmission};
use crate::shared_host::SharedStateSnapshot;
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
    AgentEvent(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompletedTurn {
    pub prompt: String,
    #[serde(default)]
    pub details: Vec<ChatMessage>,
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

// --- Setup / OOBE ---

/// Application mode — controls which UI is shown.
#[derive(Debug, Clone, PartialEq)]
pub enum AppMode {
    /// Normal agent chat.
    Chat,
    /// Setup wizard (agent not ready — CLI missing or not authenticated).
    Setup,
}

/// State for the setup wizard screen.
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
    pub session_id: String,
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

/// Route a parsed `agent_event` payload into the AgentSessionRegistry.
///
/// Returns `true` if the registry was updated and the UI should redraw.
pub fn route_agent_event_to_registry(
    reg: &mut crate::agent_sessions::AgentSessionRegistry,
    pane_session_id: &str,
    params: &serde_json::Value,
) -> bool {
    use crate::agent_sessions::{CliSource, SessionEvent};
    use std::path::PathBuf;

    // The COM broadcast wraps the hook payload as:
    //   { "event": "agent.tool.starting",
    //     "cli_source": "claude",
    //     "agent_session_id": "...",
    //     "payload": { ...original hook stdin... } }
    let event = params.get("event").and_then(|v| v.as_str()).unwrap_or("");
    if !event.starts_with("agent.") {
        return false;
    }

    let cli_source = CliSource::parse(params.get("cli_source").and_then(|v| v.as_str()));
    let asid       = params.get("agent_session_id").and_then(|v| v.as_str()).unwrap_or("");
    let key        = reg.resolve_or_synthesize_key(asid, pane_session_id);

    let payload = params.get("payload").cloned().unwrap_or(serde_json::Value::Null);
    let cwd = payload.get("cwd")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_default();
    let cwd_label = cwd.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
    let title_for_synth = format!("{:?} — {}", cli_source, cwd_label);

    // Synthesize SessionStarted on first sighting since the hooks plugin
    // doesn't ship a session-start hook (PreToolUse always fires before any
    // user-visible activity).
    let session_known = reg.has_session(&key);
    let needs_synthetic_start = event != "agent.session.started" && !session_known;
    if needs_synthetic_start {
        reg.apply(SessionEvent::SessionStarted {
            key: key.clone(),
            cli_source: cli_source.clone(),
            pane_session_id: pane_session_id.to_string(),
            cwd: cwd.clone(),
            title: title_for_synth.clone(),
        });
    }

    // Real agent.session.started supersedes any synthetic placeholder.
    if event == "agent.session.started" && !asid.is_empty() {
        reg.drop_synthetic_for_pane(pane_session_id);
    }

    let ev = match event {
        "agent.session.started" => SessionEvent::SessionStarted {
            key,
            cli_source,
            pane_session_id: pane_session_id.to_string(),
            cwd,
            title: title_for_synth,
        },
        "agent.tool.starting" => SessionEvent::ToolStarting {
            key,
            tool_name: payload.get("tool_name").or_else(|| payload.get("toolName"))
                .and_then(|v| v.as_str()).unwrap_or("").to_string(),
        },
        "agent.tool.completed" => SessionEvent::ToolCompleted { key },
        "agent.notification"   => SessionEvent::Notification {
            key,
            message: payload.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        },
        "agent.session.stopped" => SessionEvent::SessionStopped {
            key,
            reason: payload.get("reason").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        },
        _ => return reg.take_dirty(),
    };

    reg.apply(ev);
    reg.take_dirty()
}

/// Classify a WT protocol event into a notification.
pub fn classify_wt_event(method: &str, session_id: &str, params: &serde_json::Value) -> WtNotification {
    match method {
        "connection_state" => {
            let state = params
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            match state {
                "failed" => WtNotification {
                    severity: WtEventSeverity::Critical,
                    session_id: session_id.to_string(),
                    summary: format!("Session {}: connection failed", session_id),
                    acknowledged: false,
                    age_ticks: 0,
                },
                "closed" => WtNotification {
                    severity: WtEventSeverity::Actionable,
                    session_id: session_id.to_string(),
                    summary: format!("Session {}: process exited", session_id),
                    acknowledged: false,
                    age_ticks: 0,
                },
                "connected" => WtNotification {
                    severity: WtEventSeverity::Informational,
                    session_id: session_id.to_string(),
                    summary: format!("Session {}: connected", session_id),
                    acknowledged: false,
                    age_ticks: 0,
                },
                // "unknown" is sent when the C++ try_as cast fails — ignore it.
                "unknown" => return WtNotification {
                    severity: WtEventSeverity::Informational,
                    session_id: session_id.to_string(),
                    summary: String::new(),
                    acknowledged: true, // auto-acknowledge so it never shows
                    age_ticks: 100,     // will be auto-dismissed immediately
                },
                _ => WtNotification {
                    severity: WtEventSeverity::Informational,
                    session_id: session_id.to_string(),
                    summary: format!("Session {}: {}", session_id, state),
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
                            session_id: session_id.to_string(),
                            summary: format!("Command failed (exit {})", exit_code),
                            acknowledged: false,
                            age_ticks: 0,
                        };
                    } else {
                        // exit code 0 = success, not interesting
                        return WtNotification {
                            severity: WtEventSeverity::Informational,
                            session_id: session_id.to_string(),
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
                session_id: session_id.to_string(),
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
                session_id: session_id.to_string(),
                summary: format!("agent_prompt:{}", prompt),
                acknowledged: false,
                age_ticks: 0,
            }
        }
        _ => WtNotification {
            severity: WtEventSeverity::Informational,
            session_id: session_id.to_string(),
            summary: format!("Session {}: {}", session_id, method),
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

pub enum AppEvent {
    Key(KeyEvent),
    /// Mouse wheel scroll: delta<0 = scroll up, delta>0 = scroll down, row = terminal row of event
    MouseScroll { delta: i32, row: u16 },
    Tick,
    Resize(u16, u16), // terminal resize (handled by ratatui)
    ConnectionStage(String),
    ProgressStatus(String),
    UserMessage(String),
    AgentConnected {
        name: String,
        model: Option<String>,
        version: Option<String>,
        session_id: String,
    },
    PromptTemplateLoaded {
        name: String,
    },
    AgentError(String),
    ExecutionInfo(String),
    AgentThoughtChunk(String),
    AgentMessageChunk(String),
    AgentMessageEnd,
    TimingMetric(String),
    ToolCall {
        id: String,
        title: String,
        status: String,
    },
    ToolCallUpdate {
        id: String,
        status: String,
    },
    Plan(Vec<PlanEntry>),
    PermissionRequest {
        description: String,
        options: Vec<PermOption>,
        responder: tokio::sync::oneshot::Sender<String>,
    },
    SharedPermissionRequest {
        description: String,
        options: Vec<PermOption>,
    },
    PermissionCleared,
    SystemMessage(String),
    DebugPipeMessage(DebugMessage),
    SharedStateSnapshot(SharedStateSnapshot),
    /// Push event from Windows Terminal protocol (VT sequence or connection state).
    WtEvent {
        method: String,
        session_id: String,
        params: serde_json::Value,
    },
    /// Preflight checks completed — transition from Setup to Chat if all passed.
    PreflightComplete(PreflightResult),
    /// Onboarding install: install task started.
    InstallStarted,
    /// Onboarding install: a line of stdout/stderr from winget.
    InstallProgress(String),
    /// Onboarding install: install task finished. Ok = success, Err = error message.
    InstallComplete(Result<(), String>),
}

// --- Per-tab session storage ---

#[derive(Default)]
struct TabSession {
    messages: Vec<ChatMessage>,
    completed_turns: Vec<CompletedTurn>,
    selected_history: Option<usize>,
    expanded_history: Option<usize>,
    scroll_offset: usize,
}

// --- App ---

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatchedCommandKind {
    FocusPane,
    SplitPaneResume,
}

#[derive(Clone, Debug)]
pub struct DispatchedCommand {
    pub kind:       DispatchedCommandKind,
    pub session_id: Option<String>,
    pub argv:       Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Chat,
    Agents,
}

pub struct App {
    pub mode: AppMode,
    pub setup: Option<SetupState>,
    pub state: ConnectionState,
    pub agent_name: String,
    pub agent_model: Option<String>,
    pub agent_version: Option<String>,
    pub prompt_name: Option<String>,
    pub progress_status: Option<String>,
    pub activity_frame: usize,
    pub session_id: String,
    pub wt_connected: bool,
    pub messages: Vec<ChatMessage>,
    pub completed_turns: Vec<CompletedTurn>,
    pub selected_history: Option<usize>,
    pub expanded_history: Option<usize>,
    pub input: String,
    pub cursor_pos: usize,
    pub tool_calls: HashMap<String, (String, String)>, // id -> (title, status)
    pub permission: Option<PermissionState>,
    pub scroll_offset: usize,
    pub agent_streaming: bool,
    pub recommendations: Option<RecommendationSet>,
    pub selected_recommendation: usize,
    pub selected_button: usize, // Send: 0 = Copy, 1 = Insert, 2 = Run (default). OpenAndSend: 0 = sole button.
    pub rec_scroll: usize,
    pub terminal_rows: u16,
    pub terminal_cols: u16,
    pub should_quit: bool,
    pub prompt_in_flight: bool,
    current_prompt_id: Option<u64>,
    current_prompt_submitted_at_unix_s: Option<f64>,
    selection_visible_pending: bool,
    prompt_tx: mpsc::UnboundedSender<PromptSubmission>,
    recommendation_tx: mpsc::UnboundedSender<crate::coordinator::ChoiceExecution>,
    permission_tx: mpsc::UnboundedSender<String>,
    pub pending_thought_response: String,
    pub pending_agent_response: String,
    pub timing_note: Option<String>,
    debug_capture_enabled: Arc<AtomicBool>,
    // Debug panel
    pub debug_messages: Vec<DebugMessage>,
    pub show_debug_panel: bool,
    pub debug_scroll: usize,
    // Pane identity (populated via VT channel)
    pub pane_session_id: Option<String>,
    pub tab_id: Option<String>,
    pub window_id: Option<String>,
    // Source pane context (from WTA_SOURCE_* env vars set by WT)
    pub source_session_id: Option<String>,
    pub source_cwd: Option<String>,
    // Agent session registry (tracks CLI-agent panes)
    pub agent_sessions: crate::agent_sessions::AgentSessionRegistry,
    pub current_view: View,
    pub agents_list_state: ratatui::widgets::ListState,
    #[cfg(test)]
    pub last_dispatched_command: Option<DispatchedCommand>,
    current_prompt_text: Option<String>,
    pending_completed_turn: Option<CompletedTurn>,
    // WT event notifications
    pub wt_notifications: std::collections::VecDeque<WtNotification>,
    pub show_notification_banner: bool,
    // Auto-fix: the session ID where the error occurred (used to auto-fill Send parent)
    pub autofix_session_id: Option<String>,
    // Auto-fix Suggested state: session ID with a non-actionable suggestion shown on
    // the bottom bar. Cleared when the user runs a successful command in the
    // same pane (signal that they've moved on) or when a new autofix triggers.
    pub suggested_session_id: Option<String>,
    pub autofix_enabled: bool,
    /// When true, display agent hook events in the chat area.
    /// Controlled by WTA_LOG_AGENT_EVENT env var.
    pub log_agent_events: bool,
    // Generation counter: incremented on every new trigger or cancel.
    // AgentMessageEnd responses whose generation doesn't match are discarded.
    autofix_generation: u64,
    // Generation captured when the current in-flight autofix prompt was sent.
    // None means the in-flight prompt is not an autofix prompt.
    inflight_autofix_generation: Option<u64>,
    // Per-tab conversation sessions. Keyed by tab_id string (0-based index).
    tab_sessions: HashMap<String, TabSession>,
    // Onboarding: signals main.rs to spawn `winget install GitHub.Copilot`.
    install_request_tx: Option<mpsc::UnboundedSender<()>>,
}

impl App {
    pub fn new(
        prompt_tx: mpsc::UnboundedSender<PromptSubmission>,
        recommendation_tx: mpsc::UnboundedSender<crate::coordinator::ChoiceExecution>,
        permission_tx: mpsc::UnboundedSender<String>,
        debug_capture_enabled: Arc<AtomicBool>,
        wt_connected: bool,
        autofix_enabled: bool,
        log_agent_events: bool,
    ) -> Self {
        Self {
            mode: AppMode::Chat,
            setup: None,
            state: ConnectionState::Connecting("Starting agent...".to_string()),
            agent_name: String::new(),
            agent_model: None,
            agent_version: None,
            prompt_name: None,
            progress_status: None,
            activity_frame: 0,
            session_id: String::new(),
            wt_connected,
            messages: Vec::new(),
            completed_turns: Vec::new(),
            selected_history: None,
            expanded_history: None,
            input: String::new(),
            cursor_pos: 0,
            tool_calls: HashMap::new(),
            permission: None,
            scroll_offset: 0,
            agent_streaming: false,
            recommendations: None,
            selected_recommendation: 0,
            selected_button: 2, // default to "Run" button (Send: Copy=0, Insert=1, Run=2)
            rec_scroll: 0,
            terminal_rows: 24,
            terminal_cols: 80,
            should_quit: false,
            prompt_in_flight: false,
            current_prompt_id: None,
            current_prompt_submitted_at_unix_s: None,
            selection_visible_pending: false,
            prompt_tx,
            recommendation_tx,
            permission_tx,
            pending_thought_response: String::new(),
            pending_agent_response: String::new(),
            timing_note: None,
            debug_capture_enabled,
            debug_messages: Vec::new(),
            show_debug_panel: false,
            debug_scroll: 0,
            pane_session_id: None,
            tab_id: None,
            window_id: None,
            source_session_id: None,
            source_cwd: None,
            agent_sessions: crate::agent_sessions::AgentSessionRegistry::new(),
            current_view: View::Chat,
            agents_list_state: {
                let mut s = ratatui::widgets::ListState::default();
                s.select(Some(0));
                s
            },
            #[cfg(test)]
            last_dispatched_command: None,
            current_prompt_text: None,
            pending_completed_turn: None,
            wt_notifications: VecDeque::new(),
            show_notification_banner: false,
            autofix_session_id: None,
            suggested_session_id: None,
            autofix_enabled,
            log_agent_events,
            autofix_generation: 0,
            inflight_autofix_generation: None,
            tab_sessions: HashMap::new(),
            install_request_tx: None,
        }
    }

    /// Wire up the channel that signals main.rs to spawn `winget install`.
    pub fn set_install_request_tx(&mut self, tx: mpsc::UnboundedSender<()>) {
        self.install_request_tx = Some(tx);
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
            AppEvent::ProgressStatus(_) => "progress_status",
            AppEvent::UserMessage(_) => "user_message",
            AppEvent::AgentConnected { .. } => "agent_connected",
            AppEvent::PromptTemplateLoaded { .. } => "prompt_template_loaded",
            AppEvent::AgentError(_) => "agent_error",
            AppEvent::ExecutionInfo(_) => "execution_info",
            AppEvent::AgentThoughtChunk(_) => "agent_thought_chunk",
            AppEvent::AgentMessageChunk(_) => "agent_message_chunk",
            AppEvent::AgentMessageEnd => "agent_message_end",
            AppEvent::TimingMetric(_) => "timing_metric",
            AppEvent::ToolCall { .. } => "tool_call",
            AppEvent::ToolCallUpdate { .. } => "tool_call_update",
            AppEvent::Plan(_) => "plan",
            AppEvent::PermissionRequest { .. } => "permission_request",
            AppEvent::SharedPermissionRequest { .. } => "shared_permission_request",
            AppEvent::PermissionCleared => "permission_cleared",
            AppEvent::SystemMessage(_) => "system_message",
            AppEvent::DebugPipeMessage(_) => "debug_pipe_message",
            AppEvent::SharedStateSnapshot(_) => "shared_state_snapshot",
            AppEvent::WtEvent { .. } => "wt_event",
            AppEvent::PreflightComplete(_) => "preflight_complete",
            AppEvent::InstallStarted => "install_started",
            AppEvent::InstallProgress(_) => "install_progress",
            AppEvent::InstallComplete(_) => "install_complete",
        }
    }

    fn trace_state(&self) -> String {
        format!(
            "state={:?} messages={} completed_turns={} input_chars={} thought_chars={} pending_chars={} scroll={} streaming={} activity_frame={} recommendations={} permission={} timing_note={}",
            self.state,
            self.messages.len(),
            self.completed_turns.len(),
            self.input.chars().count(),
            self.pending_thought_response.chars().count(),
            self.pending_agent_response.chars().count(),
            self.scroll_offset,
            self.agent_streaming,
            self.activity_frame,
            self.recommendations
                .as_ref()
                .map(|recs| recs.choices.len())
                .unwrap_or(0),
            self.permission.is_some(),
            self.timing_note.is_some()
        )
    }

    fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Key(key) => self.handle_key(key),
            AppEvent::MouseScroll { delta, row } => {
                if self.recommendations.is_some() {
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
                            self.rec_scroll = self.rec_scroll.saturating_add(delta as usize);
                        } else {
                            self.rec_scroll = self.rec_scroll.saturating_sub((-delta) as usize);
                        }
                    } else {
                        // Mouse is in the chat area: scroll chat history.
                        if delta < 0 {
                            self.scroll_offset = self.scroll_offset.saturating_add((-delta) as usize);
                        } else {
                            self.scroll_offset = self.scroll_offset.saturating_sub(delta as usize);
                        }
                    }
                } else {
                    // No recs visible — scroll chat.
                    if delta < 0 {
                        self.scroll_offset = self.scroll_offset.saturating_add((-delta) as usize);
                    } else {
                        self.scroll_offset = self.scroll_offset.saturating_sub(delta as usize);
                    }
                }
            }
            AppEvent::Tick => {
                if self.has_activity_indicator() {
                    self.activity_frame = (self.activity_frame + 1) % 10; // Must match ACTIVITY_HIGHLIGHT_WINDOWS.len() in ui/chat.rs
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
            AppEvent::ProgressStatus(status) => {
                self.progress_status = Some(status);
                self.scroll_to_bottom();
            }
            AppEvent::UserMessage(text) => {
                self.prepare_for_new_prompt(&text);
                self.messages.push(ChatMessage::User(text));
                self.scroll_to_bottom();
            }
            AppEvent::AgentConnected {
                name,
                model,
                version,
                session_id,
            } => {
                self.agent_name = name;
                self.agent_model = model;
                self.agent_version = version;
                self.session_id = session_id;
                self.state = ConnectionState::Connected;
                self.publish_agent_status();
            }
            AppEvent::PromptTemplateLoaded { name } => {
                self.prompt_name = Some(name);
            }
            AppEvent::AgentError(msg) => {
                // Check if this is an auth-related error — if so, show the
                // Setup wizard with CLI ✓ and Auth ✗ instead of a raw error.
                let lower = msg.to_ascii_lowercase();
                let is_auth_error = lower.contains("auth")
                    || lower.contains("login")
                    || lower.contains("unauthorized")
                    || lower.contains("401")
                    || lower.contains("credentials");

                if is_auth_error && self.mode != AppMode::Setup {
                    // Extract agent id from the agent_name or fall back
                    let agent_id = if self.agent_name.is_empty() {
                        "copilot".to_string()
                    } else {
                        self.agent_name.to_ascii_lowercase()
                    };
                    let profile = crate::agent_registry::lookup_profile(&agent_id);

                    // Build a preflight result with CLI passed, auth failed
                    let auth_reason = msg
                        .lines()
                        .find(|l| {
                            let ll = l.to_ascii_lowercase();
                            ll.contains("auth") || ll.contains("login")
                        })
                        .unwrap_or("Not authenticated")
                        .trim()
                        .to_string();

                    let preflight = PreflightResult {
                        agent_id: profile.id.to_string(),
                        display_name: profile.display_name.to_string(),
                        cli_status: CheckStatus::Passed,
                        cli_path: None,
                        auth_status: CheckStatus::Failed(auth_reason),
                        install_hint: profile.install_hint.to_string(),
                        install_url: profile.install_url.to_string(),
                        auth_hint: profile.auth_hint.to_string(),
                    };

                    self.mode = AppMode::Setup;
                    self.setup = Some(SetupState {
                        preflight,
                        selected_index: 1, // select auth row
                        install_in_progress: false,
                        install_log: Vec::new(),
                        install_error: None,
                    });
                    self.state = ConnectionState::Disconnected;
                    self.publish_agent_status();
                    self.prompt_in_flight = false;
                    self.agent_streaming = false;
                    self.progress_status = None;
                    self.pending_thought_response.clear();
                    self.activity_frame = 0;
                    self.pending_agent_response.clear();
                    self.timing_note = None;
                    self.pending_completed_turn = None;
                } else {
                    self.state = ConnectionState::Failed(msg.clone());
                    self.publish_agent_status();
                    self.prompt_in_flight = false;
                    self.agent_streaming = false;
                    self.progress_status = None;
                    self.pending_thought_response.clear();
                    self.activity_frame = 0;
                    self.pending_agent_response.clear();
                    self.timing_note = None;
                    self.pending_completed_turn = None;
                    self.messages.push(ChatMessage::Error(msg));
                }
            }
            AppEvent::ExecutionInfo(message) => {
                self.push_execution_info(message);
                self.scroll_to_bottom();
            }
            AppEvent::AgentThoughtChunk(text) => {
                self.prompt_in_flight = true;
                if self.progress_status.is_none() {
                    self.progress_status = Some("Thinking...".to_string());
                }
                append_thought_preview(&mut self.pending_thought_response, &text);
            }
            AppEvent::AgentMessageChunk(text) => {
                self.agent_streaming = true;
                self.prompt_in_flight = true;
                self.progress_status = None;
                self.pending_thought_response.clear();
                self.pending_agent_response.push_str(&text);
            }
            AppEvent::AgentMessageEnd => {
                // Check if this response is stale (generation bumped since we sent).
                let is_stale_autofix = match self.inflight_autofix_generation {
                    Some(gen) => gen != self.autofix_generation,
                    None => false,
                };

                if is_stale_autofix {
                    // Discard: a newer error or cancel superseded this response.
                    tracing::info!(target: "autofix", inflight_gen = ?self.inflight_autofix_generation, current_gen = self.autofix_generation, "discarding stale autofix response");
                    self.agent_streaming = false;
                    self.prompt_in_flight = false;
                    self.progress_status = None;
                    self.pending_thought_response.clear();
                    self.pending_agent_response.clear();
                    self.activity_frame = 0;
                    self.inflight_autofix_generation = None;
                    return;
                }

                // Always reset streaming flags so autofix guards don't get stuck.
                self.agent_streaming = false;
                self.prompt_in_flight = false;
                self.progress_status = None;
                self.pending_thought_response.clear();
                self.activity_frame = 0;
                self.inflight_autofix_generation = None;

                {
                    if let Some(summary) = self.completion_latency_summary() {
                        self.push_execution_info(summary);
                    }
                    match self.finalize_agent_response() {
                        FinalizeOutcome::SelectionReady => {
                            self.clear_completed_turn_history();
                        }
                        FinalizeOutcome::None => {
                            self.scroll_to_bottom();
                        }
                    }
                }
            }
            AppEvent::TimingMetric(note) => {
                self.timing_note = Some(note);
            }
            AppEvent::ToolCall { id, title, status } => {
                self.tool_calls
                    .insert(id.clone(), (title.clone(), status.clone()));
                self.messages
                    .push(ChatMessage::ToolCall { id, title, status });
                self.scroll_to_bottom();
            }
            AppEvent::ToolCallUpdate { id, status } => {
                if let Some(entry) = self.tool_calls.get_mut(&id) {
                    entry.1 = status.clone();
                }
                // Update in-place in messages
                for msg in &mut self.messages {
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
            AppEvent::Plan(entries) => {
                self.messages.push(ChatMessage::Plan(entries));
                self.scroll_to_bottom();
            }
            AppEvent::PermissionRequest {
                description,
                options,
                responder,
            } => {
                self.permission = Some(PermissionState {
                    description,
                    options,
                    selected: 0,
                    responder: Some(responder),
                });
            }
            AppEvent::SharedPermissionRequest {
                description,
                options,
            } => {
                self.permission = Some(PermissionState {
                    description,
                    options,
                    selected: 0,
                    responder: None,
                });
            }
            AppEvent::PermissionCleared => {
                self.permission = None;
            }
            AppEvent::SystemMessage(message) => {
                self.messages.push(ChatMessage::System(message));
                self.scroll_to_bottom();
            }
            AppEvent::DebugPipeMessage(msg) => {
                self.debug_messages.push(msg);
                // Cap at 500 messages
                if self.debug_messages.len() > 500 {
                    self.debug_messages.remove(0);
                }
            }
            AppEvent::SharedStateSnapshot(snapshot) => {
                self.apply_shared_snapshot(snapshot);
            }
            AppEvent::WtEvent {
                method,
                session_id,
                params,
            } => {
                tracing::debug!(target: "autofix", method = %method, session_id = %session_id, self_pane_session_id = ?self.pane_session_id, "WtEvent");

                // autofix_execute is an inbound UI action ("run the armed
                // fix now") from TerminalPage. session_id is the failing
                // pane — NOT our own — so this check must run before the
                // same-pane skip below. Ignore the event if we don't
                // actually have a cached autofix for that pane.
                if method == "autofix_execute" {
                    self.handle_autofix_execute_request(&session_id);
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

                // Agent hook events (from wt-agent-hooks plugin) — display if enabled.
                // Must check before same-pane skip: agent events originate from
                // hooks in the agent's own pane, so session_id would match ours.
                if method == "agent_event" {
                    // Track CLI-agent sessions in OTHER panes (not WTA's own pane).
                    // The chat-display below logs ALL agent events, including ours.
                    if self.pane_session_id.as_deref() != Some(session_id.as_str()) {
                        let _ = route_agent_event_to_registry(
                            &mut self.agent_sessions,
                            &session_id,
                            &params,
                        );
                    }

                    if let Some(event_type) = params.get("event").and_then(|v| v.as_str()) {
                        if event_type.starts_with("agent.") {
                            if self.log_agent_events {
                                self.display_agent_hook_event(event_type, &params);
                            }
                            return;
                        }
                    }
                }

                // Skip events from our own pane
                if self.pane_session_id.as_deref() == Some(session_id.as_str()) {
                    tracing::debug!(target: "autofix", "skipped: own pane");
                    return;
                }

                // Route connection_state into the registry as well as classify_wt_event.
                if method == "connection_state" {
                    use crate::agent_sessions::SessionEvent;
                    let state = params.get("state").and_then(|v| v.as_str()).unwrap_or("");
                    match state {
                        "failed" => {
                            let reason = params.get("reason")
                                .and_then(|v| v.as_str())
                                .unwrap_or("connection failed").to_string();
                            self.agent_sessions.apply(SessionEvent::ConnectionFailed {
                                pane_session_id: session_id.clone(),
                                reason,
                            });
                        }
                        "closed" => {
                            self.agent_sessions.apply(SessionEvent::PaneClosed {
                                pane_session_id: session_id.clone(),
                            });
                        }
                        _ => {}
                    }
                    let _ = self.agent_sessions.take_dirty();
                    // fall through to classify_wt_event for autofix
                }

                let notification = classify_wt_event(&method, &session_id, &params);
                tracing::debug!(target: "autofix", severity = ?notification.severity, summary = %notification.summary, "classified");

                // Always log to chat for critical/actionable events
                match notification.severity {
                    WtEventSeverity::Critical => {
                        self.messages
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

                        // When auto-fix is disabled, skip notification display entirely.
                        if !self.autofix_enabled {
                            return;
                        }

                        // maybe_trigger_autofix pushes ChatMessage::Error (red dot)
                        // itself — don't double-push here as a System message.
                        self.show_notification_banner = true;
                        self.maybe_trigger_autofix(&notification);
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
                            if is_exit_zero && self.autofix_session_id.as_deref() == Some(session_id.as_str()) {
                                self.autofix_generation = self.autofix_generation.wrapping_add(1);
                                // Do NOT clear inflight_autofix_generation: the stale
                                // check in AgentMessageEnd relies on Some(old) != new_gen.
                                let pane = self.autofix_session_id.take().unwrap();
                                self.clear_recommendations();
                                self.prompt_in_flight = false;
                                self.agent_streaming = false;
                                self.progress_status = None;
                                self.emit_autofix_state_cleared(&pane);
                            }
                            // Suggested: dismiss on prompt activity (exit-zero or
                            // a fresh prompt-start) in ANY pane. Emit cleared
                            // against the original suggested pane so the bar's
                            // lastErrorPaneId stays consistent.
                            if (is_exit_zero || is_prompt_start)
                                && self.suggested_session_id.is_some()
                            {
                                let pane = self.suggested_session_id.take().unwrap();
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
            AppEvent::InstallStarted => {
                if let Some(ref mut setup) = self.setup {
                    setup.install_in_progress = true;
                    setup.install_error = None;
                }
            }
            AppEvent::InstallProgress(line) => {
                if let Some(ref mut setup) = self.setup {
                    setup.install_log.push(line);
                    // Cap to last ~8 lines so the UI doesn't grow unbounded.
                    let len = setup.install_log.len();
                    if len > 8 {
                        setup.install_log.drain(0..len - 8);
                    }
                }
            }
            AppEvent::InstallComplete(result) => {
                if let Some(ref mut setup) = self.setup {
                    setup.install_in_progress = false;
                    match result {
                        Ok(()) => {
                            setup.install_log.push("Installation complete.".to_string());
                            setup.install_error = None;
                        }
                        Err(err) => {
                            setup.install_error = Some(err);
                        }
                    }
                }
            }
            AppEvent::PreflightComplete(result) => {
                if result.all_passed() {
                    // All checks passed — transition to Chat mode
                    self.mode = AppMode::Chat;
                    self.setup = None;
                    self.state = ConnectionState::Connecting("Starting agent...".to_string());
                } else {
                    // Show the setup wizard. Preserve any install state so the user
                    // sees the result of a just-finished install attempt.
                    let (install_log, install_error) = match self.setup.take() {
                        Some(prev) => (prev.install_log, prev.install_error),
                        None => (Vec::new(), None),
                    };
                    self.mode = AppMode::Setup;
                    self.setup = Some(SetupState {
                        preflight: result,
                        selected_index: 0,
                        install_in_progress: false,
                        install_log,
                        install_error,
                    });
                    self.state = ConnectionState::Disconnected;
                }
            }
        }
    }

    fn event_requires_redraw(&self, event: &AppEvent) -> bool {
        match event {
            AppEvent::Tick => self.has_activity_indicator() || self.show_notification_banner,
            AppEvent::AgentMessageChunk(_) => true,
            AppEvent::DebugPipeMessage(_) => self.show_debug_panel,
            _ => true,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        match (key.code, key.modifiers) {
            (crossterm::event::KeyCode::F(2), crossterm::event::KeyModifiers::NONE)
            | (crossterm::event::KeyCode::Tab, crossterm::event::KeyModifiers::CONTROL) => {
                self.current_view = match self.current_view {
                    View::Chat   => View::Agents,
                    View::Agents => View::Chat,
                };
                return;
            }
            _ => {}
        }

        if self.current_view == View::Agents {
            let count = self.agent_sessions.iter_sorted().len();
            match key.code {
                crossterm::event::KeyCode::Down => {
                    let cur = self.agents_list_state.selected().unwrap_or(0);
                    let next = if count == 0 { 0 } else { (cur + 1).min(count - 1) };
                    self.agents_list_state.select(Some(next));
                }
                crossterm::event::KeyCode::Up => {
                    let cur = self.agents_list_state.selected().unwrap_or(0);
                    self.agents_list_state.select(Some(cur.saturating_sub(1)));
                }
                crossterm::event::KeyCode::Enter => {
                    if let Some(idx) = self.agents_list_state.selected() {
                        let selected = self.agent_sessions
                            .iter_sorted()
                            .get(idx)
                            .map(|s| (*s).clone());
                        if let Some(s) = selected {
                            self.activate_session(&s);
                        }
                    }
                }
                crossterm::event::KeyCode::Delete => {
                    if let Some(idx) = self.agents_list_state.selected() {
                        let target = self.agent_sessions
                            .iter_sorted()
                            .get(idx)
                            .map(|s| (s.key.clone(), s.status.clone()));
                        if let Some((key, status)) = target {
                            use crate::agent_sessions::AgentStatus::*;
                            if matches!(status, Ended | Historical) {
                                self.agent_sessions.remove(&key);
                            }
                        }
                    }
                }
                crossterm::event::KeyCode::Esc => {
                    self.current_view = View::Chat;
                }
                _ => {}
            }
            return;
        }

        // If in setup mode, route keys to setup handler
        if self.mode == AppMode::Setup {
            self.handle_setup_key(key);
            return;
        }

        // If permission modal is showing, route keys there
        if let Some(ref mut perm) = self.permission {
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
                    if let Some(perm) = self.permission.take() {
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
                        if let Some(perm) = self.permission.take() {
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
                        if let Some(perm) = self.permission.take() {
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
            KeyCode::Up if self.input.is_empty() && self.recommendations.is_some() => {
                if self.selected_recommendation > 0 {
                    self.selected_recommendation -= 1;
                    self.selected_button = self.default_button_for_selected();
                    self.scroll_rec_to_selected();
                }
            }
            KeyCode::Down if self.input.is_empty() && self.recommendations.is_some() => {
                if let Some(recs) = &self.recommendations {
                    if self.selected_recommendation + 1 < recs.choices.len() {
                        self.selected_recommendation += 1;
                        self.selected_button = self.default_button_for_selected();
                        self.scroll_rec_to_selected();
                    }
                }
            }
            KeyCode::Right | KeyCode::Tab
                if self.input.is_empty() && self.recommendations.is_some() =>
            {
                // Cycle button focus forward within the selected card.
                // Send: 0=Copy, 1=Insert, 2=Run. OpenAndSend has only index 0.
                let button_count = self.button_count_for_selected();
                if button_count > 1 {
                    self.selected_button = (self.selected_button + 1) % button_count;
                }
            }
            KeyCode::Left
                if self.input.is_empty() && self.recommendations.is_some() =>
            {
                // Cycle button focus backward.
                let button_count = self.button_count_for_selected();
                if button_count > 1 {
                    self.selected_button = (self.selected_button + button_count - 1) % button_count;
                }
            }
            KeyCode::Up if self.history_navigation_enabled() => {
                self.select_previous_history_turn();
            }
            KeyCode::Down if self.history_navigation_enabled() => {
                self.select_next_history_turn();
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
                if self.agent_streaming {
                    // TODO: send cancel to agent
                    self.agent_streaming = false;
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Esc if self.show_notification_banner => {
                self.dismiss_notifications();
            }
            KeyCode::Esc
                if self.recommendations.is_some()
                    || (self.autofix_session_id.is_some() && self.prompt_in_flight) =>
            {
                // Dismiss armed fix card or cancel in-flight autofix request.
                self.autofix_generation = self.autofix_generation.wrapping_add(1);
                let pane = self.autofix_session_id.take();
                self.clear_recommendations();
                self.prompt_in_flight = false;
                self.agent_streaming = false;
                self.progress_status = None;
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
            // In shared-host attach mode `suggested_session_id` lives on the host;
            // the attach client would need to send a HostCommand::DismissSuggestion.
            // TODO: wire that path when shared-host mode is exercised.
            KeyCode::Esc if self.suggested_session_id.is_some() => {
                let pane = self.suggested_session_id.take().unwrap();
                self.emit_autofix_state_cleared(&pane);
            }
            KeyCode::Esc if self.input.is_empty() => {
                self.collapse_selected_history_turn();
            }
            KeyCode::Esc => {
                self.input.clear();
                self.cursor_pos = 0;
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.insert_input_char('\n');
            }
            KeyCode::Enter => {
                tracing::debug!(target: "autofix", input_empty = self.input.is_empty(), state = ?self.state, has_recs = self.recommendations.is_some(), autofix_session = ?self.autofix_session_id, selected_idx = self.selected_recommendation, "Enter");
                if self.input.is_empty()
                    && self.state == ConnectionState::Connected
                    && self.recommendations.is_some()
                {
                    if let Some(mut choice) = self.selected_recommendation().cloned() {
                        // Copy button (Send index 0): copy the command text to
                        // clipboard via OSC 52, then dismiss the card the same
                        // way Insert/Run do. We still commit the pending turn
                        // and clear any armed autofix state so the UI returns
                        // to its quiescent state.
                        if self.selected_button == 0 && self.is_send_choice(&choice) {
                            if let Some(input) = first_send_input(&choice) {
                                crate::osc52::copy(&input);
                            }
                            let armed_pane = self.autofix_session_id.take();
                            self.commit_pending_completed_turn();
                            self.clear_recommendations();
                            self.push_execution_info("Copied to clipboard.".to_string());
                            if let Some(session_id) = armed_pane {
                                self.emit_autofix_state_cleared(&session_id);
                            }
                        } else {
                            // Send: index 1 = Insert, index 2 = Run.
                            // OpenAndSend: sole index 0 = open target.
                            let insert_only = self.selected_button == 1
                                && self.is_send_choice(&choice);
                            tracing::info!(target: "autofix", choice = choice.choice, actions = choice.actions.len(), insert_only, "Executing choice");
                            // Auto-fill parent for Send actions from auto-fix.
                            if let Some(ref session_id) = self.autofix_session_id {
                                for action in &mut choice.actions {
                                    if let crate::coordinator::RecommendedAction::Send {
                                        ref mut parent, ..
                                    } = action
                                    {
                                        if parent.is_empty() {
                                            *parent = session_id.clone();
                                        }
                                    }
                                }
                            }
                            let armed_pane = self.autofix_session_id.take();
                            self.commit_pending_completed_turn();
                            self.clear_recommendations();
                            let label = if insert_only { "Inserting" } else { "Executing" };
                            self.push_execution_info(format!("{} choice {}.", label, choice.choice));
                            let _ = self.recommendation_tx.send(
                                crate::coordinator::ChoiceExecution { choice, insert_only }
                            );
                            // Clear the bottom-bar Armed state — the fix has been
                            // dispatched to the source pane.
                            if let Some(session_id) = armed_pane {
                                self.emit_autofix_state_cleared(&session_id);
                            }
                        }
                    }
                } else if self.history_navigation_enabled() {
                    self.toggle_selected_history_turn();
                } else if !self.input.is_empty() && self.state == ConnectionState::Connected {
                    let text = self.input.clone();
                    self.input.clear();
                    self.cursor_pos = 0;
                    self.prepare_for_new_prompt(&text);
                    self.messages.push(ChatMessage::User(text.clone()));
                    self.scroll_to_bottom();
                    let pane_context = crate::shared_host::PaneContext {
                        session_id: self.pane_session_id.clone(),
                        tab_id: self.tab_id.clone(),
                        window_id: self.window_id.clone(),
                        cwd: self.source_cwd.clone(),
                        source_session_id: self.source_session_id.clone(),
                    };
                    let prompt = PromptSubmission::new(text, Some(pane_context));
                    self.current_prompt_id = Some(prompt.id);
                    self.current_prompt_submitted_at_unix_s = Some(prompt.submitted_at_unix_s);
                    self.selection_visible_pending = false;
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
                self.delete_before_cursor();
            }
            KeyCode::Delete => {
                self.delete_at_cursor();
            }
            KeyCode::Left if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_cursor_word_left();
            }
            KeyCode::Right if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_cursor_word_right();
            }
            KeyCode::Left => {
                self.move_cursor_left();
            }
            KeyCode::Right => {
                self.move_cursor_right();
            }
            KeyCode::Home => {
                self.cursor_pos = 0;
            }
            KeyCode::End => {
                self.cursor_pos = self.input.len();
            }
            KeyCode::PageUp => {
                self.scroll_offset = self.scroll_offset.saturating_add(10);
            }
            KeyCode::PageDown => {
                self.scroll_offset = self.scroll_offset.saturating_sub(10);
            }
            KeyCode::Char(c) => {
                self.insert_input_char(c);
            }
            _ => {}
        }
    }

    fn handle_setup_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Esc => {
                self.should_quit = true;
            }
            KeyCode::Enter => {
                // Trigger winget install when the CLI row is selected and CLI is missing.
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
                // Open install page in browser as a fallback.
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

    fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    fn has_activity_indicator(&self) -> bool {
        self.prompt_in_flight
            || self.agent_streaming
            || self.progress_status.is_some()
            || self
                .setup
                .as_ref()
                .map(|s| s.install_in_progress)
                .unwrap_or(false)
    }

    /// Get the most recent unacknowledged notification (for the banner).
    pub fn active_notification(&self) -> Option<&WtNotification> {
        self.wt_notifications
            .iter()
            .rev()
            .find(|n| !n.acknowledged)
    }

    /// Count of unacknowledged actionable/critical notifications.
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
    pub fn notification_badge(&self) -> Option<(&str, &WtEventSeverity)> {
        // Show the most severe unacknowledged notification
        self.wt_notifications
            .iter()
            .rev()
            .find(|n| !n.acknowledged)
            .map(|n| (n.summary.as_str(), &n.severity))
    }

    fn insert_input_char(&mut self, ch: char) {
        self.cursor_pos = clamp_cursor_to_boundary(&self.input, self.cursor_pos);
        self.input.insert(self.cursor_pos, ch);
        self.cursor_pos += ch.len_utf8();
    }

    fn delete_before_cursor(&mut self) {
        self.cursor_pos = clamp_cursor_to_boundary(&self.input, self.cursor_pos);
        if self.cursor_pos == 0 {
            return;
        }

        let previous = prev_char_boundary(&self.input, self.cursor_pos);
        self.input.replace_range(previous..self.cursor_pos, "");
        self.cursor_pos = previous;
    }

    fn delete_at_cursor(&mut self) {
        self.cursor_pos = clamp_cursor_to_boundary(&self.input, self.cursor_pos);
        if self.cursor_pos >= self.input.len() {
            return;
        }

        let next = next_char_boundary(&self.input, self.cursor_pos);
        self.input.replace_range(self.cursor_pos..next, "");
    }

    fn move_cursor_left(&mut self) {
        self.cursor_pos = prev_char_boundary(&self.input, self.cursor_pos);
    }

    fn move_cursor_right(&mut self) {
        self.cursor_pos = next_char_boundary(&self.input, self.cursor_pos);
    }

    fn move_cursor_word_left(&mut self) {
        self.cursor_pos = prev_word_boundary(&self.input, self.cursor_pos);
    }

    fn move_cursor_word_right(&mut self) {
        self.cursor_pos = next_word_boundary(&self.input, self.cursor_pos);
    }

    /// Height of the recommendations panel — grows to fit content, capped at 40% of pane height.
    pub fn rec_panel_height(&self) -> u16 {
        let recs = match self.recommendations.as_ref() {
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
        self.recommendations = None;
        self.selected_recommendation = 0;
        self.selected_button = 2;
        self.rec_scroll = 0;
    }

    /// Adjusts rec_scroll so the selected recommendation card's title is at the top of the panel.
    fn scroll_rec_to_selected(&mut self) {
        let panel_height = self.rec_panel_height() as usize; // actual panel size, not full pane
        let panel_width = self.terminal_cols;
        let Some(recs) = self.recommendations.clone() else { return };

        // Accumulate line offsets to find the exact top of the selected card.
        let mut line_top: usize = 0;
        for (idx, choice) in recs.choices.iter().enumerate() {
            let card_h = rec_card_height(choice, panel_width);
            if idx == self.selected_recommendation {
                // Scroll so title is at the top; if the card fits, keep it fully visible.
                let card_bottom = line_top + card_h;
                if line_top < self.rec_scroll {
                    self.rec_scroll = line_top;
                } else if card_bottom > self.rec_scroll + panel_height {
                    self.rec_scroll = line_top;
                }
                return;
            }
            line_top += card_h;
        }
    }

    pub fn history_navigation_enabled(&self) -> bool {
        self.input.is_empty()
            && self.recommendations.is_none()
            && self.permission.is_none()
            && !self.prompt_in_flight
            && !self.agent_streaming
            && self.messages.is_empty()
            && self.pending_agent_response.is_empty()
            && self.pending_thought_response.is_empty()
            && !self.completed_turns.is_empty()
    }

    pub fn history_row_selected(&self, index: usize) -> bool {
        self.selected_history == Some(index)
    }

    pub fn history_row_expanded(&self, index: usize) -> bool {
        self.expanded_history == Some(index)
    }

    fn switch_tab_session(&mut self, new_tab_id: String) {
        let old_tab = self.tab_id.clone();
        tracing::info!(
            target: "tab_session",
            from = ?old_tab,
            to = %new_tab_id,
            completed_turns = self.completed_turns.len(),
            messages = self.messages.len(),
            "switch_tab_session"
        );

        if let Some(ref cur) = old_tab {
            if *cur != new_tab_id {
                let s = self.tab_sessions.entry(cur.clone()).or_default();
                s.messages = std::mem::take(&mut self.messages);
                s.completed_turns = std::mem::take(&mut self.completed_turns);
                s.selected_history = self.selected_history.take();
                s.expanded_history = self.expanded_history.take();
                s.scroll_offset = self.scroll_offset;
                tracing::info!(
                    target: "tab_session",
                    tab = %cur,
                    saved_turns = s.completed_turns.len(),
                    "saved session"
                );
            }
        }

        let loaded = self.tab_sessions.remove(&new_tab_id).unwrap_or_default();
        tracing::info!(
            target: "tab_session",
            tab = %new_tab_id,
            loaded_turns = loaded.completed_turns.len(),
            "loaded session"
        );
        self.messages = loaded.messages;
        self.completed_turns = loaded.completed_turns;
        self.selected_history = loaded.selected_history;
        self.expanded_history = loaded.expanded_history;
        self.scroll_offset = loaded.scroll_offset;

        self.tab_id = Some(new_tab_id);
    }

    fn clear_chat_history(&mut self) {
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

    fn clear_completed_turn_history(&mut self) {
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
        let same_pane = self.autofix_session_id.as_deref() == Some(notification.session_id.as_str());

        if same_pane && self.prompt_in_flight {
            // Same pane, already Pending: re-emit pending with new summary
            // but don't send another prompt (agent is already working on it).
            tracing::info!(target: "autofix", pane_id = %notification.session_id, "autofix re-trigger same pane while pending — re-emit only");
            self.emit_autofix_state_pending(&notification.session_id, &notification.summary);
            return;
        }

        // For all other cases (different pane, or Armed state, or Idle):
        // bump generation to stale any in-flight response, clear current state.
        self.autofix_generation = self.autofix_generation.wrapping_add(1);
        self.clear_recommendations();
        self.agent_streaming = false;
        self.prompt_in_flight = false;
        // A new analysis supersedes any leftover suggestion. The C++ side
        // will swap to Pending on the new pending event below; emitting an
        // explicit cleared first would create a flicker.
        self.suggested_session_id = None;

        // The auto-fix kind is carried by PromptSubmission::is_autofix,
        // so the text doesn't need a marker prefix — just the raw error
        // summary + instruction.
        let prompt_text = format!(
            "{}\nDiagnose the error and suggest a fix.",
            notification.summary
        );

        // Use the failing pane as the source so the agent reads its buffer.
        let pane_context = crate::shared_host::PaneContext {
            session_id: self.pane_session_id.clone(),
            tab_id: self.tab_id.clone(),
            window_id: self.window_id.clone(),
            cwd: self.source_cwd.clone(),
            source_session_id: Some(notification.session_id.clone()),
        };

        // Store the failing session ID so we can auto-fill `parent` on execution.
        self.autofix_session_id = Some(notification.session_id.clone());

        // Push the error line (red dot) so the user sees it directly.
        self.messages
            .push(ChatMessage::Error(notification.summary.clone()));
        self.scroll_to_bottom();

        self.prompt_in_flight = true;
        self.inflight_autofix_generation = Some(self.autofix_generation);
        self.progress_status = Some("Preparing context...".to_string());
        self.activity_frame = 0;

        let prompt = PromptSubmission::new_autofix(prompt_text, Some(pane_context));
        self.current_prompt_id = Some(prompt.id);
        self.current_prompt_submitted_at_unix_s = Some(prompt.submitted_at_unix_s);
        tracing::info!(target: "autofix", session_id = %notification.session_id, generation = self.autofix_generation, "sending auto-fix prompt");
        let _ = self.prompt_tx.send(prompt);

        // Light up the bottom-bar diagnostic icon in "Pending" state — the
        // user knows something went wrong even before the agent responds.
        self.emit_autofix_state_pending(&notification.session_id, &notification.summary);
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
    fn handle_autofix_execute_request(&mut self, requested_session_id: &str) {
        tracing::info!(target: "autofix", requested_session = %requested_session_id, armed_pane = ?self.autofix_session_id, has_recs = self.recommendations.is_some(), "autofix_execute received");
        // Only execute if we have a cached autofix for the requested pane.
        // The pane_id check prevents a stale UI click from running against
        // an unrelated, more recent error.
        let armed_pane = match self.autofix_session_id.clone() {
            Some(p) if p == requested_session_id => p,
            _ => {
                tracing::info!(target: "autofix", "autofix_execute: no armed fix for this pane");
                // Tell the UI anyway so it returns to Idle.
                self.emit_autofix_state_cleared(requested_session_id);
                return;
            }
        };
        let rec = match self.recommendations.clone() {
            Some(r) => r,
            None => {
                self.emit_autofix_state_cleared(&armed_pane);
                self.autofix_session_id = None;
                return;
            }
        };
        let idx = rec
            .recommended_choice
            .unwrap_or(self.selected_recommendation)
            .min(rec.choices.len().saturating_sub(1));
        let Some(mut choice) = rec.choices.get(idx).cloned() else {
            self.emit_autofix_state_cleared(&armed_pane);
            self.autofix_session_id = None;
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
        self.autofix_session_id = None;
        self.commit_pending_completed_turn();
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

    fn prepare_for_new_prompt(&mut self, prompt_text: &str) {
        self.clear_chat_history();
        self.current_prompt_text = Some(prompt_text.to_string());
        self.prompt_in_flight = true;
        self.progress_status = Some("Preparing context...".to_string());
        self.activity_frame = 0;
    }

    fn push_execution_info(&mut self, _message: String) {}

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
        self.focus_latest_completed_turn();
    }

    fn focus_latest_completed_turn(&mut self) {
        let Some(last) = self.completed_turns.len().checked_sub(1) else {
            self.selected_history = None;
            self.expanded_history = None;
            return;
        };

        self.selected_history = Some(last);
        self.expanded_history = None;
        self.scroll_to_bottom();
    }

    fn select_previous_history_turn(&mut self) {
        let Some(selected) = self.selected_history else {
            self.selected_history = Some(self.completed_turns.len().saturating_sub(1));
            return;
        };

        if selected > 0 {
            self.selected_history = Some(selected - 1);
        }
    }

    fn select_next_history_turn(&mut self) {
        let Some(selected) = self.selected_history else {
            self.selected_history = Some(self.completed_turns.len().saturating_sub(1));
            return;
        };

        if selected + 1 < self.completed_turns.len() {
            self.selected_history = Some(selected + 1);
        }
    }

    fn toggle_selected_history_turn(&mut self) {
        let Some(selected) = self.selected_history else {
            return;
        };

        if self.expanded_history == Some(selected) {
            self.expanded_history = None;
        } else {
            self.expanded_history = Some(selected);
        }
    }

    fn collapse_selected_history_turn(&mut self) {
        if self.expanded_history == self.selected_history {
            self.expanded_history = None;
        }
    }

    fn normalize_history_state(&mut self) {
        if self.completed_turns.is_empty() {
            self.selected_history = None;
            self.expanded_history = None;
            return;
        }

        let last = self.completed_turns.len() - 1;
        self.selected_history = Some(self.selected_history.unwrap_or(last).min(last));
        if let Some(expanded) = self.expanded_history {
            self.expanded_history = Some(expanded.min(last));
        }
    }

    fn selected_recommendation(&self) -> Option<&RecommendationChoice> {
        self.recommendations
            .as_ref()
            .and_then(|recs| recs.choices.get(self.selected_recommendation))
    }

    /// Returns the number of buttons for the currently selected choice card.
    /// Send actions have 3 buttons (Copy, Insert, Run); OpenAndSend has 1 button.
    fn button_count_for_selected(&self) -> usize {
        self.selected_recommendation()
            .map(|c| if self.is_send_choice(c) { 3 } else { 1 })
            .unwrap_or(1)
    }

    /// Default focused button index when landing on a card.
    /// Send cards default to the rightmost button (Run); OpenAndSend cards
    /// have a single button at index 0.
    fn default_button_for_selected(&self) -> usize {
        let count = self.button_count_for_selected();
        if count > 1 { count - 1 } else { 0 }
    }

    /// Returns true if the choice's primary action is Send (shell command).
    fn is_send_choice(&self, choice: &RecommendationChoice) -> bool {
        choice.actions.iter().any(|a| matches!(a, crate::coordinator::RecommendedAction::Send { .. }))
    }

    fn finalize_agent_response(&mut self) -> FinalizeOutcome {
        if self.pending_agent_response.trim().is_empty() {
            self.log_selection_phase("selection_parse_failed", "reason=empty_agent_response");
            return FinalizeOutcome::None;
        }

        let text = std::mem::take(&mut self.pending_agent_response);

        // Autofix responses use a minimal prompt/format; parse them separately.
        if self.autofix_session_id.is_some() {
            return self.finalize_autofix_response(text);
        }

        match parse_recommendation_set(&text).and_then(|recommendations| {
            validate_recommendation_set_for_coordinator_target(
                &recommendations,
                self.pane_session_id.as_deref(),
            )
        }) {
            Ok(recommendations) => {
                self.stage_completed_turn(text);
                self.selected_recommendation = recommended_choice_index(&recommendations);
                self.log_selection_phase(
                    "selection_ready",
                    &format!(
                        "choice_count={} recommended_choice={:?}",
                        recommendations.choices.len(),
                        recommendations.recommended_choice
                    ),
                );
                self.recommendations = Some(recommendations);
                self.selection_visible_pending = true;
                FinalizeOutcome::SelectionReady
            }
            Err(err) => {
                self.clear_recommendations();
                self.pending_completed_turn = None;
                let error_text = format!("{:#}", err).replace('\n', " | ");
                self.log_selection_phase(
                    "selection_parse_failed",
                    &format!(
                        "response_chars={} error={:?}",
                        text.chars().count(),
                        error_text
                    ),
                );
                if self.current_prompt_text.is_some() {
                    self.stage_completed_turn(text);
                    self.commit_pending_completed_turn();
                    self.clear_chat_history();
                } else {
                    self.prompt_in_flight = false;
                    self.progress_status = None;
                    self.agent_streaming = false;
                }
                FinalizeOutcome::None
            }
        }
    }

    fn finalize_autofix_response(&mut self, text: String) -> FinalizeOutcome {
        let pane_id = match self.autofix_session_id.clone() {
            Some(p) => p,
            None => return FinalizeOutcome::None,
        };

        match parse_autofix_response(&text) {
            AutofixDecision::Fix(recommendations) => {
                self.log_selection_phase(
                    "autofix_fix",
                    &format!("pane={pane_id} title={:?}", recommendations.choices.first().map(|c| &c.title)),
                );
                let preview = Self::armed_fix_preview(&recommendations);
                self.emit_autofix_state_armed(&pane_id, &preview);
                self.selected_recommendation = recommended_choice_index(&recommendations);
                self.recommendations = Some(recommendations);
                self.selection_visible_pending = true;
                FinalizeOutcome::SelectionReady
            }
            AutofixDecision::Explain { title, explanation } => {
                self.log_selection_phase(
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
                let mut details = self.current_turn_details();
                details.push(ChatMessage::Agent(explanation));
                self.pending_completed_turn = Some(CompletedTurn {
                    prompt: turn_prompt,
                    details,
                });
                self.commit_pending_completed_turn();
                // Auto-expand: the diagnosis is the whole point of this turn,
                // and the user shouldn't have to guess that the prompt header
                // is collapsible to reveal it.
                self.expanded_history = self.selected_history;

                self.emit_autofix_state_suggested(&pane_id, &title);

                // No executable action to remember, but keep `suggested_session_id`
                // so a successful next command in the same pane can dismiss the
                // bottom bar indicator.
                self.suggested_session_id = Some(pane_id.clone());
                self.autofix_session_id = None;
                self.clear_recommendations();
                self.prompt_in_flight = false;
                self.progress_status = None;
                self.agent_streaming = false;
                FinalizeOutcome::None
            }
            AutofixDecision::Ignore => {
                self.log_selection_phase("autofix_ignore", &format!("pane={pane_id}"));
                self.autofix_session_id = None;
                self.clear_recommendations();
                self.emit_autofix_state_cleared(&pane_id);
                self.prompt_in_flight = false;
                self.progress_status = None;
                self.agent_streaming = false;
                FinalizeOutcome::None
            }
        }
    }

    fn apply_shared_snapshot(&mut self, snapshot: SharedStateSnapshot) {
        // Check if the snapshot contains an auth-related error — if so,
        // switch to Setup wizard instead of showing a raw error.
        if let ConnectionState::Failed(ref msg) = snapshot.state {
            let lower = msg.to_ascii_lowercase();
            let is_auth_error = lower.contains("auth")
                || lower.contains("login")
                || lower.contains("unauthorized")
                || lower.contains("401")
                || lower.contains("credentials");

            if is_auth_error && self.mode != AppMode::Setup {
                let agent_id = if snapshot.agent_name.is_empty() {
                    "copilot".to_string()
                } else {
                    snapshot.agent_name.to_ascii_lowercase()
                };
                let profile = crate::agent_registry::lookup_profile(&agent_id);

                let auth_reason = msg
                    .lines()
                    .find(|l| {
                        let ll = l.to_ascii_lowercase();
                        ll.contains("auth") || ll.contains("login")
                    })
                    .unwrap_or("Not authenticated")
                    .trim()
                    .to_string();

                let preflight = PreflightResult {
                    agent_id: profile.id.to_string(),
                    display_name: profile.display_name.to_string(),
                    cli_status: CheckStatus::Passed,
                    cli_path: None,
                    auth_status: CheckStatus::Failed(auth_reason),
                    install_hint: profile.install_hint.to_string(),
                    install_url: profile.install_url.to_string(),
                    auth_hint: profile.auth_hint.to_string(),
                };

                self.mode = AppMode::Setup;
                self.setup = Some(SetupState {
                    preflight,
                    selected_index: 1,
                    install_in_progress: false,
                    install_log: Vec::new(),
                    install_error: None,
                });
                self.state = ConnectionState::Disconnected;
                return;
            }
        }

        let recommendations_changed = self.recommendations != snapshot.recommendations;
        let completed_turns_changed = self.completed_turns != snapshot.completed_turns;
        let permission_changed = self
            .permission
            .as_ref()
            .map(|perm| (&perm.description, &perm.options))
            != snapshot
                .permission
                .as_ref()
                .map(|perm| (&perm.description, &perm.options));

        self.state = snapshot.state;
        self.agent_name = snapshot.agent_name;
        self.agent_model = snapshot.agent_model;
        self.agent_version = snapshot.agent_version;
        self.prompt_name = snapshot.prompt_name;
        self.progress_status = snapshot.progress_status;
        self.session_id = snapshot.session_id;
        self.wt_connected = snapshot.wt_connected;
        self.messages = snapshot.messages;
        self.completed_turns = snapshot.completed_turns;
        self.recommendations = snapshot.recommendations;
        self.agent_streaming = snapshot.agent_streaming;
        self.pending_thought_response = snapshot.pending_thought_response;
        self.pending_agent_response = snapshot.pending_agent_response;
        self.timing_note = snapshot.timing_note;
        self.prompt_in_flight = snapshot.prompt_in_flight;

        if recommendations_changed {
            self.selected_recommendation = self
                .recommendations
                .as_ref()
                .map(recommended_choice_index)
                .unwrap_or(0);
            if self.recommendations.is_some() {
                self.selection_visible_pending = true;
            }
        }

        if completed_turns_changed {
            if self.completed_turns.is_empty() {
                self.selected_history = None;
                self.expanded_history = None;
            } else {
                self.focus_latest_completed_turn();
            }
        }

        if let Some(permission) = snapshot.permission {
            let selected = if permission_changed {
                0
            } else {
                self.permission
                    .as_ref()
                    .map(|current| current.selected)
                    .unwrap_or(0)
            };
            let max_selected = permission.options.len().saturating_sub(1);
            self.permission = Some(PermissionState {
                description: permission.description,
                options: permission.options,
                selected: selected.min(max_selected),
                responder: None,
            });
        } else {
            self.permission = None;
        }

        self.normalize_history_state();
    }

    /// Format and display an agent hook event as a chat message.
    fn display_agent_hook_event(&mut self, event_type: &str, params: &serde_json::Value) {
        let cli_source = params
            .get("cli_source")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        // Raw hook JSON is nested under "payload" (scripts pass stdin as-is)
        let payload = params.get("payload").unwrap_or(params);

        // Helper: truncate by chars (not bytes) to avoid panicking on UTF-8 boundaries
        let truncate = |s: &str, max: usize| -> String {
            if s.chars().count() <= max {
                s.to_string()
            } else {
                s.chars().take(max).collect::<String>() + "…"
            }
        };

        let detail = match event_type {
            "agent.tool.starting" => {
                let tool = payload.get("tool_name")
                    .or_else(|| payload.get("toolName"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let args = payload.get("tool_input")
                    .or_else(|| payload.get("toolArgs"))
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                format!("─ {} ─\n  Tool: {}\n  Args: {}\n  Source: {}", event_type, tool, truncate(&args, 80), cli_source)
            }
            "agent.tool.finished" | "agent.tool.completed" | "agent.tool.failed" => {
                let tool = payload.get("tool_name")
                    .or_else(|| payload.get("toolName"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let result = if event_type == "agent.tool.failed" {
                    "failed"
                } else if payload.get("tool_response")
                    .and_then(|r| r.get("interrupted"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    "interrupted"
                } else {
                    payload.get("toolResult")
                        .and_then(|r| r.get("resultType"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("completed")
                };
                let summary = payload.get("toolResult")
                    .and_then(|r| r.get("textResultForLlm"))
                    .or_else(|| payload.get("tool_response").and_then(|r| r.get("stdout")))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let summary_trunc = truncate(summary, 80);
                if summary_trunc.is_empty() {
                    format!("─ {} ─\n  Tool: {}\n  Result: {}\n  Source: {}", event_type, tool, result, cli_source)
                } else {
                    format!("─ {} ─\n  Tool: {}\n  Result: {}\n  Output: {}\n  Source: {}", event_type, tool, result, summary_trunc, cli_source)
                }
            }
            "agent.prompt.submit" => {
                let prompt = payload.get("prompt")
                    .or_else(|| payload.get("initialPrompt"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let prompt_trunc = truncate(prompt, 120);
                if prompt_trunc.is_empty() {
                    format!("─ {} ─\n  Source: {}", event_type, cli_source)
                } else {
                    format!("─ {} ─\n  Prompt: {}\n  Source: {}", event_type, prompt_trunc, cli_source)
                }
            }
            "agent.session.start" => {
                let cwd = payload.get("cwd")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                format!("─ {} ─\n  CWD: {}\n  Source: {}", event_type, cwd, cli_source)
            }
            "agent.session.end" | "agent.stop" | "agent.subagent.stop"
            | "agent.session.stopped" | "agent.session" => {
                let reason = payload.get("reason")
                    .or_else(|| payload.get("stopReason"))
                    .or_else(|| payload.get("hook_event_name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                format!("─ {} ─\n  Reason: {}\n  Source: {}", event_type, reason, cli_source)
            }
            "agent.error" => {
                let err = payload.get("error")
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                let err_trunc = truncate(&err, 120);
                format!("─ {} ─\n  Error: {}\n  Source: {}", event_type, err_trunc, cli_source)
            }
            "agent.notification" => {
                let msg = payload.get("notification")
                    .or_else(|| payload.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                format!("─ {} ─\n  {}\n  Source: {}", event_type, msg, cli_source)
            }
            _ => {
                format!("─ {} ─\n  Source: {}", event_type, cli_source)
            }
        };

        self.messages.push(ChatMessage::AgentEvent(detail));
        self.scroll_to_bottom();
    }
}

impl App {
    fn log_selection_phase(&self, phase: &str, details: &str) {
        if let (Some(prompt_id), Some(submitted_at_unix_s)) = (
            self.current_prompt_id,
            self.current_prompt_submitted_at_unix_s,
        ) {
            prompt_timing_log(prompt_id, submitted_at_unix_s, phase, details);
        }
    }

    fn log_selection_visible_if_needed(&mut self) {
        if !self.selection_visible_pending || self.recommendations.is_none() {
            return;
        }

        let details = format!(
            "choice_count={} selected_index={}",
            self.recommendations
                .as_ref()
                .map(|set| set.choices.len())
                .unwrap_or(0),
            self.selected_recommendation
        );
        self.log_selection_phase("selection_visible", &details);
        self.selection_visible_pending = false;
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

/// Returns the `input` string of the first `Send` action in this choice,
/// if any. Used by the Copy button to extract the command text to put on
/// the clipboard.
fn first_send_input(choice: &RecommendationChoice) -> Option<String> {
    for action in &choice.actions {
        if let crate::coordinator::RecommendedAction::Send { input, .. } = action {
            return Some(input.clone());
        }
    }
    None
}

/// Extract a short preview string from the recommended choice's first
/// Send action, for display in the bottom-bar tooltip on Armed state.
/// Free function so both `App` (attach TUI) and the shared host can call it.
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
            }
        });
        send_wt_protocol_event(evt.to_string());
    }

    fn activate_session(&mut self, s: &crate::agent_sessions::AgentSession) {
        use crate::agent_sessions::AgentStatus::*;
        match s.status {
            Idle | Working | Attention | Error => {
                if let Some(pane) = &s.pane_session_id {
                    self.dispatch_focus_pane(pane.clone());
                }
            }
            Ended | Historical => {
                self.dispatch_resume(s);
            }
        }
    }

    fn dispatch_focus_pane(&mut self, pane_session_id: String) {
        let argv = vec![
            "focus-pane".to_string(),
            "-t".to_string(),
            pane_session_id.clone(),
        ];
        crate::shell::wt_channel::spawn_wtcli_async(&argv);
        #[cfg(test)]
        {
            self.last_dispatched_command = Some(DispatchedCommand {
                kind: DispatchedCommandKind::FocusPane,
                session_id: Some(pane_session_id),
                argv,
            });
        }
    }

    fn dispatch_resume(&mut self, s: &crate::agent_sessions::AgentSession) {
        // Synthetic placeholder: we never knew the upstream session id, so
        // resume is not feasible. Silently no-op (matches the empty-resume_flag contract).
        if s.key.starts_with("pane:") {
            return;
        }
        let cli_id = match s.cli_source {
            crate::agent_sessions::CliSource::Claude  => "claude",
            crate::agent_sessions::CliSource::Copilot => "copilot",
            crate::agent_sessions::CliSource::Gemini  => "gemini",
            crate::agent_sessions::CliSource::Unknown(_) => return,
        };
        let profile = crate::agent_registry::lookup_profile_by_id(cli_id);
        if profile.resume_flag.is_empty() {
            // v1: silently no-op for CLIs without resume support.
            return;
        }
        let commandline = format!("{} {} {}", cli_id, profile.resume_flag, s.key);

        // TODO(v2): wtcli's split-pane does not currently accept --starting-directory.
        // The resumed pane inherits the splitting pane's cwd; the CLI's own --resume
        // typically restores the session's original working directory anyway.
        let argv = vec![
            "split-pane".to_string(),
            "-c".to_string(),
            commandline,
        ];
        crate::shell::wt_channel::spawn_wtcli_async(&argv);
        #[cfg(test)]
        {
            self.last_dispatched_command = Some(DispatchedCommand {
                kind: DispatchedCommandKind::SplitPaneResume,
                session_id: None,
                argv,
            });
        }
    }

    #[cfg(test)]
    pub fn last_dispatched_command_for_test(&self) -> Option<DispatchedCommand> {
        self.last_dispatched_command.clone()
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


/// Open a URL in the default browser (Windows).
fn open_url_in_browser(url: &str) -> std::io::Result<()> {
    std::process::Command::new("cmd")
        .args(["/c", "start", "", url])
        .spawn()?;
    Ok(())
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
        let debug_capture = Arc::new(AtomicBool::new(false));
        let mut app = App::new(prompt_tx, recommendation_tx, permission_tx, debug_capture, true, true, false);
        app.agent_sessions = crate::agent_sessions::AgentSessionRegistry::new();
        app.current_view = View::Chat;
        app.agents_list_state.select(Some(0));
        app
    }

    #[test]
    fn f2_toggles_between_chat_and_agents_view() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app();
        assert_eq!(app.current_view, View::Chat);

        app.handle_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
        assert_eq!(app.current_view, View::Agents);

        app.handle_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
        assert_eq!(app.current_view, View::Chat);
    }

    #[test]
    fn arrow_keys_move_cursor_in_agents_view() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use crate::agent_sessions::{CliSource, SessionEvent};
        use std::path::PathBuf;
        let mut app = test_app();
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "a".into(), cli_source: CliSource::Claude,
            pane_session_id: "p1".into(), cwd: PathBuf::from("/x"), title: "t".into(),
        });
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "b".into(), cli_source: CliSource::Copilot,
            pane_session_id: "p2".into(), cwd: PathBuf::from("/y"), title: "u".into(),
        });
        app.current_view = View::Agents;

        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.agents_list_state.selected(), Some(1));

        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.agents_list_state.selected(), Some(0));
    }

    #[test]
    fn enter_on_live_row_dispatches_focus_command() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use crate::agent_sessions::{CliSource, SessionEvent};
        use std::path::PathBuf;
        let mut app = test_app();
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "a".into(), cli_source: CliSource::Claude,
            pane_session_id: "00000000-0000-0000-0000-0000000000aa".into(),
            cwd: PathBuf::from("/x"), title: "t".into(),
        });
        app.current_view = View::Agents;
        app.agents_list_state.select(Some(0));

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let cmd = app.last_dispatched_command_for_test()
            .expect("a command was dispatched");
        assert_eq!(cmd.kind, DispatchedCommandKind::FocusPane);
        assert_eq!(cmd.session_id.as_deref(), Some("00000000-0000-0000-0000-0000000000aa"));
    }

    #[test]
    fn enter_on_history_row_dispatches_split_pane_with_resume() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use crate::agent_sessions::{CliSource, SessionEvent};
        use std::path::PathBuf;
        let mut app = test_app();
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "abc-123".into(), cli_source: CliSource::Claude,
            pane_session_id: "p".into(), cwd: PathBuf::from("/work/proj"), title: "t".into(),
        });
        app.agent_sessions.apply(SessionEvent::SessionStopped {
            key: "abc-123".into(), reason: "user_exit".into(),
        });

        app.current_view = View::Agents;
        app.agents_list_state.select(Some(0));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let cmd = app.last_dispatched_command_for_test()
            .expect("a command was dispatched");
        assert_eq!(cmd.kind, DispatchedCommandKind::SplitPaneResume);
        let argv = cmd.argv.join(" ");
        assert!(argv.contains("split-pane"), "argv: {}", argv);
        assert!(argv.contains("claude --resume abc-123"), "argv: {}", argv);
    }

    #[test]
    fn delete_on_history_row_removes_session_from_registry() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use crate::agent_sessions::{CliSource, SessionEvent};
        use std::path::PathBuf;
        let mut app = test_app();
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "k".into(), cli_source: CliSource::Claude,
            pane_session_id: "p".into(), cwd: PathBuf::from("/x"), title: "t".into(),
        });
        app.agent_sessions.apply(SessionEvent::SessionStopped {
            key: "k".into(), reason: "".into(),
        });
        app.current_view = View::Agents;
        app.agents_list_state.select(Some(0));

        app.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert!(!app.agent_sessions.has_session(&"k".to_string()));
    }

    #[test]
    fn delete_on_live_row_is_noop() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use crate::agent_sessions::{CliSource, SessionEvent};
        use std::path::PathBuf;
        let mut app = test_app();
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: "k".into(), cli_source: CliSource::Claude,
            pane_session_id: "p".into(), cwd: PathBuf::from("/x"), title: "t".into(),
        });
        app.current_view = View::Agents;
        app.agents_list_state.select(Some(0));

        app.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert!(app.agent_sessions.has_session(&"k".to_string()));
    }

    // ─── Issue #1: Agents view key leak ─────────────────────────────────────

    #[test]
    fn agents_view_swallows_chat_input_keys() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app();
        app.current_view = View::Agents;
        let input_before = app.input.clone();
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(app.input, input_before, "Chat input must not change while in Agents view");
    }

    #[test]
    fn agents_view_esc_returns_to_chat() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app();
        app.current_view = View::Agents;
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.current_view, View::Chat);
    }

    // ─── Issue #4: Synthetic history row resume ────────────────────────────

    #[test]
    fn enter_on_synthetic_history_row_does_not_dispatch_resume() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use crate::agent_sessions::{CliSource, SessionEvent};
        use std::path::PathBuf;
        let mut app = test_app();
        let pane = "00000000-0000-0000-0000-0000000000aa";
        app.agent_sessions.apply(SessionEvent::SessionStarted {
            key: format!("pane:{}", pane),
            cli_source: CliSource::Claude,
            pane_session_id: pane.into(),
            cwd: PathBuf::from("/x"), title: "t".into(),
        });
        app.agent_sessions.apply(SessionEvent::PaneClosed { pane_session_id: pane.into() });
        app.current_view = View::Agents;
        app.agents_list_state.select(Some(0));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(app.last_dispatched_command_for_test().is_none(),
            "must not dispatch resume for synthetic pane:<guid> key");
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
            session_id: "1".to_string(),
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
            session_id: "1".to_string(),
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
            session_id: "1".to_string(),
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
            session_id: "3".to_string(),
            params: json!({"session_id": "3", "state": "failed"}),
        });
        assert!(app.show_notification_banner);
        assert_eq!(app.wt_notifications.len(), 1);
        assert_eq!(app.wt_notifications[0].severity, WtEventSeverity::Critical);
        // Should have an Error message in chat
        assert!(app.messages.iter().any(|m| matches!(m, ChatMessage::Error(_))));
    }

    #[test]
    fn wt_event_actionable_shows_banner_and_triggers_autofix() {
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            session_id: "5".to_string(),
            params: json!({"session_id": "5", "state": "closed"}),
        });
        assert!(app.show_notification_banner);
        // Actionable events go through maybe_trigger_autofix which pushes Error (red dot)
        assert!(app.messages.iter().any(|m| matches!(m, ChatMessage::Error(_))));
    }

    #[test]
    fn wt_event_informational_no_banner_no_chat_message() {
        let mut app = test_app();
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            session_id: "1".to_string(),
            params: json!({"session_id": "1", "state": "connected"}),
        });
        assert!(!app.show_notification_banner);
        assert!(app.messages.is_empty());
        assert_eq!(app.wt_notifications.len(), 1);
    }

    #[test]
    fn wt_event_from_own_pane_is_ignored() {
        let mut app = test_app();
        app.pane_session_id = Some("42".to_string());
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            session_id: "42".to_string(),
            params: json!({"session_id": "42", "state": "failed"}),
        });
        // Events from our own pane should be completely ignored
        assert!(!app.show_notification_banner);
        assert!(app.wt_notifications.is_empty());
        assert!(app.messages.is_empty());
    }

    #[test]
    fn dismiss_notifications_clears_banner_and_acknowledges() {
        let mut app = test_app();
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            session_id: "3".to_string(),
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
            session_id: "1".to_string(),
            params: json!({"session_id": "1", "state": "closed"}),
        });
        // Second event (more recent)
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            session_id: "2".to_string(),
            params: json!({"session_id": "2", "state": "failed"}),
        });

        let (summary, severity) = app.notification_badge().unwrap();
        assert!(summary.contains("Session 2"));
        assert_eq!(*severity, WtEventSeverity::Critical);
        assert_eq!(app.unacknowledged_count(), 2);
    }

    #[test]
    fn notification_queue_caps_at_20() {
        let mut app = test_app();
        for i in 0..25 {
            app.handle_event(AppEvent::WtEvent {
                method: "connection_state".to_string(),
                session_id: format!("{}", i),
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
            session_id: "1".to_string(),
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
            session_id: "3".to_string(),
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
            session_id: "3".to_string(),
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
            session_id: "3".to_string(),
            params: json!({"session_id": "3", "state": "closed"}),
        });
        assert!(app.active_notification().is_some());

        app.dismiss_notifications();
        assert!(app.active_notification().is_none());
    }

    #[test]
    fn multiple_events_different_panes() {
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        // Informational from pane 1
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            session_id: "1".to_string(),
            params: json!({"session_id": "1", "state": "connected"}),
        });
        // Critical from pane 2
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            session_id: "2".to_string(),
            params: json!({"session_id": "2", "state": "failed"}),
        });
        // Actionable from pane 3
        app.handle_event(AppEvent::WtEvent {
            method: "connection_state".to_string(),
            session_id: "3".to_string(),
            params: json!({"session_id": "3", "state": "closed"}),
        });

        assert_eq!(app.wt_notifications.len(), 3);
        // Unacknowledged count only counts actionable + critical
        assert_eq!(app.unacknowledged_count(), 2);
        // Banner should show (due to critical + actionable)
        assert!(app.show_notification_banner);
        // Chat should have 2 messages (critical Error + actionable autofix Error)
        assert_eq!(app.messages.len(), 2);
    }

    // ─── agent_event hook payload rendering ─────────────────────────────────

    #[test]
    fn agent_event_tool_starting_renders_tool_name_from_payload() {
        let mut app = test_app();
        app.log_agent_events = true;
        app.handle_event(AppEvent::WtEvent {
            method: "agent_event".to_string(),
            session_id: "abc".to_string(),
            params: json!({
                "event": "agent.tool.starting",
                "cli_source": "copilot",
                "payload": { "tool_name": "Bash", "tool_input": { "command": "ls" } },
            }),
        });
        let rendered = app.messages.iter().find_map(|m| {
            if let ChatMessage::AgentEvent(s) = m { Some(s.clone()) } else { None }
        }).expect("should have rendered an AgentEvent");
        assert!(rendered.contains("Bash"), "expected tool name in: {}", rendered);
        assert!(rendered.contains("ls"), "expected tool_input in: {}", rendered);
        assert!(rendered.contains("copilot"), "expected cli_source in: {}", rendered);
    }

    #[test]
    fn agent_event_tool_finished_event_name_is_recognized() {
        // agent.tool.finished should hit a real arm, not fall through to default.
        let mut app = test_app();
        app.log_agent_events = true;
        app.handle_event(AppEvent::WtEvent {
            method: "agent_event".to_string(),
            session_id: "abc".to_string(),
            params: json!({
                "event": "agent.tool.finished",
                "cli_source": "copilot",
                "payload": { "tool_name": "Bash" },
            }),
        });
        let rendered = app.messages.iter().find_map(|m| {
            if let ChatMessage::AgentEvent(s) = m { Some(s.clone()) } else { None }
        }).expect("should have rendered an AgentEvent");
        // Real arm includes "Tool:" and "Result:"; default fall-through does not.
        assert!(rendered.contains("Bash"), "expected tool name in: {}", rendered);
        assert!(rendered.contains("Tool:"), "expected Tool: line (not default arm) in: {}", rendered);
        assert!(rendered.contains("Result:"), "expected Result: line (not default arm) in: {}", rendered);
    }

    // ─── route_agent_event_to_registry ──────────────────────────────────────

    #[test]
    fn route_agent_event_creates_session_on_tool_starting() {
        use crate::agent_sessions::AgentSessionRegistry;
        let mut reg = AgentSessionRegistry::new();
        let pane = "00000000-0000-0000-0000-000000000001";
        let params = serde_json::json!({
            "event": "agent.tool.starting",
            "cli_source": "claude",
            "agent_session_id": "abc",
            "payload": {"tool_name": "bash", "cwd": "/work"}
        });
        let dirty = route_agent_event_to_registry(&mut reg, pane, &params);
        assert!(dirty);
        assert!(reg.has_session(&"abc".to_string()));
    }

    #[test]
    fn route_agent_event_falls_back_to_pane_keyed_placeholder() {
        use crate::agent_sessions::AgentSessionRegistry;
        let mut reg = AgentSessionRegistry::new();
        let pane = "00000000-0000-0000-0000-000000000001";
        let params = serde_json::json!({
            "event": "agent.tool.starting",
            "cli_source": "copilot",
            "payload": {"tool_name": "bash"}
        });
        route_agent_event_to_registry(&mut reg, pane, &params);
        assert!(reg.has_session(&format!("pane:{}", pane)));
    }

    #[test]
    fn route_agent_event_ignores_non_agent_events() {
        use crate::agent_sessions::AgentSessionRegistry;
        let mut reg = AgentSessionRegistry::new();
        let params = serde_json::json!({"event": "something.else"});
        let dirty = route_agent_event_to_registry(&mut reg, "p", &params);
        assert!(!dirty);
    }
}
