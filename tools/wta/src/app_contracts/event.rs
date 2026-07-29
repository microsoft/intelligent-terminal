use crossterm::event::{KeyEvent, MouseEvent};

use super::{AcpModelInfo, AvailableAgent, DebugMessage, PermOption, PlanEntry, PreflightResult};

pub enum AppEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Tick,
    RevealTick,
    Resize(u16, u16),
    FocusChanged(bool),
    ConnectionStage(String),
    AgentConnected {
        name: String,
        model: Option<String>,
        version: Option<String>,
        session_id: String,
        available_models: Vec<AcpModelInfo>,
        current_model_id: Option<String>,
        load_session_supported: bool,
        image_supported: bool,
    },
    SessionAttached {
        tab_id: String,
        session_id: String,
        available_models: Vec<AcpModelInfo>,
        current_model_id: Option<String>,
    },
    TabError {
        tab_id: String,
        message: String,
    },
    TabSystemMessage {
        tab_id: String,
        message: String,
    },
    AgentPasteTextReady {
        tab_id: String,
        generation: u64,
        text: String,
    },
    AgentPasteTextFailed {
        tab_id: String,
        generation: u64,
        error: String,
    },
    PromptTemplateLoaded {
        name: String,
    },
    AutofixTargetResolved {
        tab_id: Option<String>,
        prompt_id: u64,
        pane_id: String,
    },
    AgentError {
        session_id: Option<String>,
        failure: crate::protocol::acp::failure::AgentFailure,
        message: String,
    },
    AgentSoftStop {
        session_id: String,
        reason: crate::protocol::acp::soft_stop::SoftStopReason,
    },
    AgentBusy {
        tab_id: String,
    },
    TabRenamed {
        old_tab_id: String,
        new_tab_id: String,
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
        tool_call_id: String,
        description: String,
        options: Vec<PermOption>,
        responder: tokio::sync::oneshot::Sender<String>,
    },
    SystemMessage(String),
    DebugPipeMessage(DebugMessage),
    WtEvent {
        method: String,
        pane_id: String,
        tab_id: Option<String>,
        params: serde_json::Value,
    },
    AgentInstallComplete,
    LoginProgress {
        device_code: String,
        verify_url: String,
    },
    LoginComplete {
        agent_id: String,
        success: bool,
        error: Option<String>,
    },
    PostLoginAuthRecovery {
        failure: crate::protocol::acp::failure::AgentFailure,
        tab_id: Option<String>,
        agent_id: String,
    },
    AuthRecoveryTimedOut {
        agent_id: String,
        generation: u64,
    },
    AgentSourcesDiscovered {
        generation: u64,
        wsl_sources: Vec<AvailableAgent>,
    },
    PreflightComplete(PreflightResult),
    AgentSessionEvent(crate::agent_sessions::SessionEvent),
    AliveSnapshotLoaded(Vec<crate::session_registry::SessionInfo>),
    AliveSessionAdded(crate::session_registry::SessionInfo),
    AliveSessionRemoved(agent_client_protocol::schema::v1::SessionId),
    AliveJoinUpgrade(Vec<(String, Option<String>)>),
    SessionsChanged,
    AgentsSnapshotLoaded {
        request_id: u64,
        sessions: Vec<crate::session_registry::SessionInfo>,
    },
    AgentsSnapshotFailed {
        request_id: u64,
    },
    RegisterBornBoundSession {
        event: crate::agent_sessions::SessionEvent,
    },
    MasterMutationCompleted {
        request_id: u64,
    },
}
