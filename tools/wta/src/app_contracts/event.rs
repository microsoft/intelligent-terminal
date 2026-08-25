use crossterm::event::{KeyEvent, MouseEvent};

use super::{
    AcpModelInfo, AcpSessionCommand, AcpSessionConfigOption, AvailableAgent, DebugMessage,
    PermOption, PlanEntry, PreflightResult,
};

pub enum AppEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Tick,
    RevealTick,
    Resize(u16, u16),
    FocusChanged(bool),
    ConnectionStage(String),
    /// Native cloud catalog carried over the private helper↔master channel.
    /// Kept separate from the current session's ACP-advertised models so a
    /// BYOK session that reports no selector cannot erase clean-probed models.
    CloudModelsAvailable(Vec<AcpModelInfo>),
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
    /// The old helper↔master ACP task has closed its pipe intentionally and
    /// the stable helper process may start the replacement connection.
    AgentReconnectReady(crate::protocol::acp::client::AgentReconnectRequest),
    /// The ACP task exited before it could consume a queued reconnect request.
    AgentClientFailed,
    SessionAttached {
        tab_id: String,
        session_id: String,
        available_models: Vec<AcpModelInfo>,
        current_model_id: Option<String>,
    },
    UsageReported {
        session_id: String,
        snapshot: crate::usage::UsageSnapshot,
    },
    UsageCleared {
        session_id: String,
    },
    ModelConfigUpdated {
        session_id: String,
        available_models: Vec<AcpModelInfo>,
        current_model_id: Option<String>,
    },
    ModelSetCompleted {
        session_id: String,
        model: String,
        pane_override: bool,
    },
    ModelSetFailed {
        session_id: String,
        model: String,
        pane_override: bool,
        message: String,
    },
    SessionConfigUpdated {
        session_id: String,
        options: Vec<AcpSessionConfigOption>,
    },
    SessionCommandsUpdated {
        session_id: String,
        commands: Vec<AcpSessionCommand>,
    },
    SessionConfigSetCompleted {
        session_id: String,
        config_id: String,
        value: String,
        model_compat: bool,
    },
    SessionConfigSetFailed {
        session_id: String,
        config_id: String,
        message: String,
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
    PromptTargetResolved {
        tab_id: Option<String>,
        prompt_id: u64,
        pane_id: String,
    },
    AgentError {
        session_id: Option<String>,
        failure: crate::protocol::acp::failure::AgentFailure,
        message: String,
    },
    /// The helper's pipe to wta-master closed. A retained helper reconnects
    /// its existing immutable binding over the stable pipe.
    MasterDisconnected,
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
        kind: crate::app::ToolCallKind,
        /// See `ChatMessage::ToolCall::location`.
        location: Option<String>,
        /// See `ChatMessage::ToolCall::location_is_command`.
        location_is_command: bool,
        cwd: Option<String>,
        output: Option<crate::app::ToolCallOutput>,
        exit_code: Option<i64>,
        content: Vec<crate::app::ToolCallContent>,
        locations: Vec<crate::app::ToolCallLocation>,
    },
    ToolCallUpdate {
        session_id: String,
        id: String,
        title: Option<String>,
        status: Option<String>,
        kind: Option<crate::app::ToolCallKind>,
        /// `Some` only when the agent's `tool_call_update` actually
        /// reported new `locations`/`raw_input` — `None` means "no
        /// change", so the existing card's location hint (if any) is
        /// left untouched rather than being blanked out.
        location: Option<String>,
        /// See `ChatMessage::ToolCall::location_is_command`. Only
        /// meaningful when `location.is_some()`.
        location_is_command: bool,
        /// `Some` means the Agent replaced its reported content. An empty
        /// string clears the previous output.
        output: Option<crate::app::ToolCallOutput>,
        /// Replacement ACP content collection. `Some(vec![])` clears it.
        content: Option<Vec<crate::app::ToolCallContent>>,
        /// Replacement ACP locations collection. `Some(vec![])` clears it.
        locations: Option<Vec<crate::app::ToolCallLocation>>,
        cwd: Option<String>,
        exit_code: Option<i64>,
    },
    ToolTerminalOutput {
        session_id: String,
        terminal_id: String,
        output: crate::app::ToolCallOutput,
        exit_code: Option<i64>,
    },
    HideToolCall {
        session_id: String,
        id: String,
    },
    Plan {
        session_id: String,
        entries: Vec<PlanEntry>,
    },
    PermissionRequest {
        session_id: String,
        tool_call_id: String,
        description: String,
        /// See `PermissionState::title`.
        title: String,
        /// See `PermissionState::kind_label`.
        kind_label: Option<String>,
        /// See `PermissionState::target`.
        target: Option<String>,
        /// See `PermissionState::target_is_command`.
        target_is_command: bool,
        options: Vec<PermOption>,
        responder: tokio::sync::oneshot::Sender<String>,
    },
    UserInputRequest {
        request_id: String,
        session_id: String,
        request: crate::agent_tools::user_input::UserInputRequest,
        responder: tokio::sync::oneshot::Sender<crate::agent_tools::user_input::UserInputResponse>,
    },
    CancelUserInputRequest {
        request_id: String,
        session_id: String,
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
    AgentReconnectPreflightComplete {
        operation_id: String,
        generation: u64,
        result: PreflightResult,
    },
    AgentSessionEvent(crate::agent_sessions::SessionEvent),
    AliveSnapshotLoaded(Vec<crate::session_registry::SessionInfo>),
    AliveSessionAdded(crate::session_registry::SessionInfo),
    AliveSessionRemoved(agent_client_protocol::schema::v1::SessionId),
    AliveJoinUpgrade(Vec<(String, Option<String>)>),
    SessionsChanged,
    DirectTerminalActionProposal {
        context: crate::agent_tools::action_proposal::channel::ValidationContext,
        payload: String,
        source: crate::agent_tools::action_proposal::pipe::ProposalPayloadSource,
        responder: tokio::sync::oneshot::Sender<
            crate::agent_tools::action_proposal::pipe::ProposalValidationDecision,
        >,
    },
    DirectTerminalActionProposalCommit {
        proposal_id: String,
        responder: tokio::sync::oneshot::Sender<bool>,
    },
    DirectTerminalActionProposalInvalidate {
        proposal_id: String,
        session_id: String,
    },
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
