use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::prelude::*;
use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::coordinator::{
    parse_recommendation_set, recommended_choice_index, RecommendationChoice, RecommendationSet,
};
use crate::ui;

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

#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionState {
    Disconnected,
    Connecting(String),
    Connected,
    Failed(String),
}

#[derive(Debug, Clone)]
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
}

#[derive(Debug, Clone)]
pub struct PlanEntry {
    pub content: String,
    pub status: PlanEntryStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlanEntryStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone)]
pub struct PermOption {
    pub id: String,
    pub name: String,
    pub kind: String,
}

pub struct PermissionState {
    pub description: String,
    pub options: Vec<PermOption>,
    pub selected: usize,
    pub responder: tokio::sync::oneshot::Sender<String>,
}

// --- Events ---

pub enum AppEvent {
    Key(KeyEvent),
    Resize(u16, u16), // terminal resize (handled by ratatui)
    ConnectionStage(String),
    AgentConnected {
        name: String,
        session_id: String,
    },
    AgentError(String),
    AgentMessageChunk(String),
    AgentMessageEnd,
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
    SystemMessage(String),
    DebugPipeMessage(DebugMessage),
}

// --- App ---

pub struct App {
    pub state: ConnectionState,
    pub agent_name: String,
    pub session_id: String,
    pub wt_connected: bool,
    pub messages: Vec<ChatMessage>,
    pub input: String,
    pub cursor_pos: usize,
    pub tool_calls: HashMap<String, (String, String)>, // id -> (title, status)
    pub permission: Option<PermissionState>,
    pub scroll_offset: usize,
    pub agent_streaming: bool,
    pub recommendations: Option<RecommendationSet>,
    pub selected_recommendation: usize,
    pub should_quit: bool,
    prompt_tx: mpsc::UnboundedSender<String>,
    recommendation_tx: mpsc::UnboundedSender<RecommendationChoice>,
    pending_agent_response: String,
    debug_capture_enabled: Arc<AtomicBool>,
    // Debug panel
    pub debug_messages: Vec<DebugMessage>,
    pub show_debug_panel: bool,
    pub debug_scroll: usize,
    // Pane identity (populated via VT channel)
    pub pane_id: Option<String>,
    pub tab_id: Option<String>,
    pub window_id: Option<String>,
}

impl App {
    pub fn new(
        prompt_tx: mpsc::UnboundedSender<String>,
        recommendation_tx: mpsc::UnboundedSender<RecommendationChoice>,
        debug_capture_enabled: Arc<AtomicBool>,
        wt_connected: bool,
    ) -> Self {
        Self {
            state: ConnectionState::Connecting("Starting agent...".to_string()),
            agent_name: String::new(),
            session_id: String::new(),
            wt_connected,
            messages: Vec::new(),
            input: String::new(),
            cursor_pos: 0,
            tool_calls: HashMap::new(),
            permission: None,
            scroll_offset: 0,
            agent_streaming: false,
            recommendations: None,
            selected_recommendation: 0,
            should_quit: false,
            prompt_tx,
            recommendation_tx,
            pending_agent_response: String::new(),
            debug_capture_enabled,
            debug_messages: Vec::new(),
            show_debug_panel: false,
            debug_scroll: 0,
            pane_id: None,
            tab_id: None,
            window_id: None,
        }
    }

    pub async fn run(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        mut ui_rx: mpsc::UnboundedReceiver<AppEvent>,
        mut event_rx: mpsc::UnboundedReceiver<AppEvent>,
    ) -> Result<()> {
        const MAX_EVENTS_PER_FRAME: usize = 64;

        terminal.draw(|frame| ui::render(frame, self))?;

        loop {
            tokio::select! {
                biased;

                Some(event) = ui_rx.recv() => {
                    self.handle_event(event);
                    terminal.draw(|frame| ui::render(frame, self))?;
                }

                Some(event) = event_rx.recv() => {
                    let mut processed = 0usize;

                    let mut should_redraw_now = self.event_requires_redraw(&event);
                    self.handle_event(event);
                    processed += 1;

                    while processed < MAX_EVENTS_PER_FRAME {
                        match event_rx.try_recv() {
                            Ok(event) => {
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

                    if should_redraw_now {
                        terminal.draw(|frame| ui::render(frame, self))?;
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

    fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Key(key) => self.handle_key(key),
            AppEvent::Resize(_, _) => {} // ratatui handles resize
            AppEvent::ConnectionStage(stage) => {
                self.state = ConnectionState::Connecting(stage);
            }
            AppEvent::AgentConnected { name, session_id } => {
                self.agent_name = name;
                self.session_id = session_id;
                self.state = ConnectionState::Connected;
            }
            AppEvent::AgentError(msg) => {
                self.state = ConnectionState::Failed(msg.clone());
                self.pending_agent_response.clear();
                self.messages.push(ChatMessage::Error(msg));
            }
            AppEvent::AgentMessageChunk(text) => {
                self.agent_streaming = true;
                self.pending_agent_response.push_str(&text);
                self.scroll_to_bottom();
            }
            AppEvent::AgentMessageEnd => {
                self.agent_streaming = false;
                self.finalize_agent_response();
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
                    responder,
                });
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
        }
    }

    fn event_requires_redraw(&self, event: &AppEvent) -> bool {
        match event {
            AppEvent::AgentMessageChunk(_) => !self.agent_streaming,
            AppEvent::DebugPipeMessage(_) => self.show_debug_panel,
            _ => true,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
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
                        let _ = perm.responder.send(option_id);
                    }
                }
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    // Quick allow: find first allow option
                    if let Some(idx) = perm.options.iter().position(|o| o.kind.contains("allow")) {
                        let option_id = perm.options[idx].id.clone();
                        if let Some(perm) = self.permission.take() {
                            let _ = perm.responder.send(option_id);
                        }
                    }
                }
                KeyCode::Char('n') | KeyCode::Char('N') => {
                    // Quick deny: find first reject option
                    if let Some(idx) = perm.options.iter().position(|o| o.kind.contains("reject")) {
                        let option_id = perm.options[idx].id.clone();
                        if let Some(perm) = self.permission.take() {
                            let _ = perm.responder.send(option_id);
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
                }
            }
            KeyCode::Down if self.input.is_empty() && self.recommendations.is_some() => {
                if let Some(recs) = &self.recommendations {
                    if self.selected_recommendation + 1 < recs.choices.len() {
                        self.selected_recommendation += 1;
                    }
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
                if self.agent_streaming {
                    // TODO: send cancel to agent
                    self.agent_streaming = false;
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Enter => {
                if self.input.is_empty()
                    && self.state == ConnectionState::Connected
                    && self.recommendations.is_some()
                {
                    if let Some(choice) = self.selected_recommendation().cloned() {
                        let _ = self.recommendation_tx.send(choice);
                    }
                } else if !self.input.is_empty() && self.state == ConnectionState::Connected {
                    let text = self.input.clone();
                    self.input.clear();
                    self.cursor_pos = 0;
                    self.recommendations = None;
                    self.selected_recommendation = 0;
                    self.pending_agent_response.clear();
                    self.messages.push(ChatMessage::User(text.clone()));
                    self.scroll_to_bottom();
                    let _ = self.prompt_tx.send(text);
                }
            }
            KeyCode::Backspace => {
                if self.cursor_pos > 0 {
                    self.cursor_pos -= 1;
                    self.input.remove(self.cursor_pos);
                }
            }
            KeyCode::Delete => {
                if self.cursor_pos < self.input.len() {
                    self.input.remove(self.cursor_pos);
                }
            }
            KeyCode::Left => {
                if self.cursor_pos > 0 {
                    self.cursor_pos -= 1;
                }
            }
            KeyCode::Right => {
                if self.cursor_pos < self.input.len() {
                    self.cursor_pos += 1;
                }
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
                self.input.insert(self.cursor_pos, c);
                self.cursor_pos += 1;
            }
            _ => {}
        }
    }

    fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    fn selected_recommendation(&self) -> Option<&RecommendationChoice> {
        self.recommendations
            .as_ref()
            .and_then(|recs| recs.choices.get(self.selected_recommendation))
    }

    fn finalize_agent_response(&mut self) {
        if self.pending_agent_response.trim().is_empty() {
            return;
        }

        let text = std::mem::take(&mut self.pending_agent_response);

        match parse_recommendation_set(&text).ok() {
            Some(recommendations) => {
                self.selected_recommendation = recommended_choice_index(&recommendations);
                self.recommendations = Some(recommendations);
            }
            None => {
                self.recommendations = None;
                self.selected_recommendation = 0;
                self.messages.push(ChatMessage::Agent(text));
                self.scroll_to_bottom();
            }
        }
    }
}
