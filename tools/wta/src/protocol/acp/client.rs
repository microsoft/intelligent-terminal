use super::conn;
use super::failure::{AgentFailure, HandshakeStage};
use super::prompt_builder::{
    acp_log_built_prompt, build_prompt_text, log_turn_trace, TemplateKind, TemplateMemo,
};
use super::soft_stop::SoftStopReason;
use super::turn_metrics::{now_unix_s, prompt_preview, PromptTimingState};
use agent_client_protocol as acp;
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use tokio::sync::mpsc;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::app_contracts::{AcpModelInfo, AppEvent, PermOption, PlanEntry, PlanEntryStatus};
use crate::pane_context::PaneContext;
use crate::shell::{ShellManager, TerminalConfig};

const ACP_SESSION_USAGE_SCHEMA: &str = "acp.v1.session_usage";
// Normal helper startup can race a slow wta-master cold start: master opens its
// pipe only after spawning and initializing the agent CLI (up to 60s for npx
// adapters), so keep a long budget there.
const MASTER_PIPE_BACKOFF_MS: &[u64] = &[
    50, 100, 100, 200, 200, 500, 500, 1000, 1000, 2000, 2000, 2000, 5000, 5000, 5000, 5000, 10000,
    10000, 10000, 15000,
];
// Post-login reconnect is different: if the old master pipe is gone, the right
// recovery is a fresh master restart. Keep a short bounded retry so brief
// respawn/ERROR_PIPE_BUSY windows are tolerated without stranding the user for
// the full cold-start budget.
const POST_LOGIN_MASTER_PIPE_BACKOFF_MS: &[u64] = &[
    50, 100, 100, 200, 200, 500, 500, 1000, 1000, 2000, 2000, 2000,
];

fn post_login_authenticate_error(method_id: &str, e: &acp::Error) -> anyhow::Error {
    let failure = AgentFailure::from_acp_error(e);
    if failure.is_auth() {
        return anyhow::Error::new(failure).context(format!(
            "authenticate({}) still requires authentication after login: {} (code {})",
            method_id,
            e.message,
            Into::<i32>::into(e.code),
        ));
    }

    anyhow::Error::new(AgentFailure::HandshakeFailed {
        stage: HandshakeStage::Authenticate,
        detail: format!(
            "authenticate({}) failed: {} (code {}). \
             The agent returned an error during authentication. \
             Try restarting Intelligent Terminal.",
            method_id,
            e.message,
            Into::<i32>::into(e.code),
        ),
    })
}

// Form A mock-ACP-agent harness + scenario tests (in-process, deterministic).
// Lives as a sibling file so it stays out of this large module, but is a child
// of `client` so it can reach the private `WtaClient` / `ClientState`.
// `pub(crate)` so app-module tests can borrow `connect_mock_agent` and assert
// on App state.
#[cfg(test)]
#[path = "mock_agent_tests.rs"]
pub(crate) mod mock_agent_tests;

#[derive(Debug, Clone)]
pub struct PromptSubmission {
    pub id: u64,
    pub text: String,
    pub pane_context: Option<PaneContext>,
    pub submitted_at_unix_s: f64,
    /// Distinguishes planner prompts from manual and automatic auto-fix
    /// inputs. Automatic summaries are untrusted diagnostic context, while
    /// text supplied to `/fix` is user intent.
    pub autofix_text_kind: Option<AutofixTextKind>,
    /// Agent-advertised slash commands are sent verbatim, without planner
    /// templates or terminal context.
    agent_command: bool,
    /// Images pasted into the input via Alt+V. Sent to the agent as ACP
    /// `ContentBlock::Image` blocks appended after the text block (only when
    /// the agent advertised `promptCapabilities.image`). Empty for the common
    /// text-only and all auto-fix prompts.
    pub images: Vec<crate::clipboard_image::PastedImage>,
    is_byok: bool,
    agent_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutofixTextKind {
    UserRequest,
    FailureSummary,
}

#[derive(Debug, Clone, Default)]
struct PromptUsageIdentity {
    family_id: Option<String>,
    reporter_id: Option<String>,
}

fn is_redundant_startup_model_error(identity: &PromptUsageIdentity, error: &acp::Error) -> bool {
    identity.family_id.as_deref() == Some(crate::agent_registry::GEMINI_AGENT_ID)
        && identity.reporter_id.as_deref() == Some("gemini-cli")
        && error.code == acp::ErrorCode::MethodNotFound
}

/// User-initiated cancel of an in-flight prompt. The App emits one of
/// these on Ctrl+C; the ACP client task fires `session/cancel` to the
/// agent and signals the per-prompt oneshot so the local task drops
/// out of `conn.prompt().await` immediately even if the agent is slow
/// or doesn't honor cancel.
#[derive(Debug, Clone)]
pub struct CancelRequest {
    pub session_id: String,
}

/// User-initiated request to spin up a fresh ACP session for a given tab,
/// dropping the previous session's history. Emitted by the `/new` slash
/// command. The ACP client task removes the old SessionId from its
/// per-tab cache, cancels any active turn, and calls `new_session(cwd)`.
/// Once the replacement is bound, master retires the old session's routing,
/// live-registry row, and session-scoped capabilities. The resulting
/// [`AppEvent::SessionAttached`] then propagates back to the UI to
/// rewire `session_to_tab` and update the model dropdown.
#[derive(Debug, Clone)]
pub struct NewSessionForTab {
    pub tab_id: String,
    /// Optional cwd override. When `None`, the client falls back to the
    /// process-wide `current_dir()` (same default as the lazy-create path).
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentReconnectRequest {
    pub operation_id: String,
    pub window_id: String,
    pub generation: u64,
    pub agent_id: String,
    pub acp_model: Option<String>,
    pub custom_model_selection: Option<String>,
    pub agent_source: crate::agent_source::AgentSource,
}

/// Control requests that end or replace the helper's current ACP connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentLifecycleRequest {
    /// `/restart` asks C++ to replace the shared master and agent CLI pool.
    RestartMaster,
    /// Agent binding changes keep the helper process and reconnect it to the
    /// same master with a fresh immutable per-connection binding.
    RebindAgent(AgentReconnectRequest),
}

impl Default for AgentLifecycleRequest {
    fn default() -> Self {
        Self::RestartMaster
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpClientExit {
    ChannelsClosed,
    RebindAgent(AgentReconnectRequest),
}

struct ClientTransportGuard {
    conn: conn::ClientLink,
    suppress_transport_error: Arc<AtomicBool>,
    event_tx: mpsc::UnboundedSender<AppEvent>,
}

impl Drop for ClientTransportGuard {
    fn drop(&mut self) {
        // A master can disappear while a setup request is in flight. In that
        // race the request fails and unwinds this guard before the separately
        // scheduled I/O task reports EOF. Claim and report the already-tripped
        // transport latch here so the retained helper still reconnects.
        let report_master_disconnect = claim_unexpected_transport_loss(
            self.conn.transport_ended(),
            &self.suppress_transport_error,
        );
        self.conn.shutdown();
        if report_master_disconnect {
            let _ = self.event_tx.send(AppEvent::MasterDisconnected);
        }
    }
}

fn claim_unexpected_transport_loss(
    transport_ended: bool,
    suppress_transport_error: &AtomicBool,
) -> bool {
    let already_suppressed = suppress_transport_error.swap(true, Ordering::AcqRel);
    transport_ended && !already_suppressed
}

fn complete_transport_io_task(
    io_result: std::result::Result<(), tokio::task::JoinError>,
    suppress_transport_error: &AtomicBool,
    event_tx: &mpsc::UnboundedSender<AppEvent>,
) -> AcpClientExit {
    if let Err(error) = io_result {
        tracing::warn!(
            target: "helper",
            %error,
            "ACP I/O task to master failed"
        );
    }
    if claim_unexpected_transport_loss(true, suppress_transport_error) {
        let _ = event_tx.send(AppEvent::MasterDisconnected);
    }
    AcpClientExit::ChannelsClosed
}

#[derive(Debug, Clone)]
pub enum MasterExtRequest {
    SessionsList {
        request_id: u64,
        /// When true, master re-scans the on-disk historical session logs
        /// (`load_for_cli`) before answering — the F5 refresh path — instead of
        /// returning the cached registry snapshot.
        rescan: bool,
    },
    SessionBornBound {
        event: crate::agent_sessions::SessionEvent,
    },
    SessionResumeDispatched {
        request_id: u64,
        sid: acp::schema::v1::SessionId,
    },
    SessionFocus {
        request_id: u64,
        sid: acp::schema::v1::SessionId,
    },
    /// Hot-swap the ACP model on this helper's live session(s) via
    /// `set_session_model`, without restarting anything. Two callers:
    /// * settings hot-reload (`acpModel` changed) and the per-pane `/model`
    ///   picker, both in `App`.
    ///
    /// `session_id == Some` targets exactly that session (a per-pane `/model`
    /// pick, or a global settings change pushed per-pane to each of this
    /// helper's tabs); `session_id == None` fans out to every session this
    /// helper owns.
    SetSessionModel {
        session_id: Option<acp::schema::v1::SessionId>,
        model: String,
        pane_override: bool,
    },
    SetSessionConfigOption {
        session_id: acp::schema::v1::SessionId,
        config_id: String,
        value: String,
    },
}

fn publish_session_config_options(
    event_tx: &mpsc::UnboundedSender<AppEvent>,
    session_id: &acp::schema::v1::SessionId,
    options: Option<&[acp::schema::v1::SessionConfigOption]>,
) {
    let options = options
        .map(crate::protocol::acp::session_config::select_options)
        .unwrap_or_default();
    let _ = event_tx.send(AppEvent::SessionConfigUpdated {
        session_id: session_id.to_string(),
        options,
    });
}

/// User-initiated request to resume a historical agent session by calling
/// the ACP `session/load` method, binding the loaded session to a
/// specific WT tab. Emitted by the session management view's Enter
/// handler (after WT has created a new tab and reconciled the agent pane
/// onto it). The ACP client task calls `conn.load_session(...)`; on
/// success the loaded SessionId is bound to the tab and `SessionAttached`
/// propagates to the UI so subsequent prompts on that tab reuse the
/// rehydrated session. The agent is expected to replay past session
/// content via `session/update` notifications during/after the
/// `load_session` call.
#[derive(Debug, Clone)]
pub struct LoadSessionForTab {
    pub tab_id: String,
    /// The CLI's own session id (Claude UUID, Gemini sessionId, Copilot
    /// session-state folder name). Sent verbatim as the ACP `sessionId`
    /// — works when the currently-connected ACP agent matches the
    /// historical session's CLI source. CLI mismatches surface as
    /// `AgentError` via the agent's JSON-RPC error response.
    pub session_id: String,
    /// Working directory to associate with the loaded session. When
    /// `None`, falls back to the process-wide `current_dir()`.
    pub cwd: Option<String>,
}

/// Drop the ACP session binding for a tab WITHOUT immediately creating a
/// replacement. Emitted by the Ctrl+C×2 close-pane path when the agent
/// pane is being hidden on a tab while other tabs still need it: we
/// release this tab's SessionId so the next prompt on this tab lazily
/// spawns a fresh session (handled by [`dispatch_prompt_body`]'s
/// lazy-create branch).
///
/// Distinct from [`NewSessionForTab`], which atomically swaps in a new
/// session — we don't want to pay the new_session round-trip until the
/// user actually sends a prompt.
#[derive(Debug, Clone)]
pub struct DropSessionRequest {
    pub tab_id: String,
    /// `false` when WT already published a process-wide reset event that the
    /// master consumes directly. The helper still clears its local binding
    /// and cancellation state, but must not race a duplicate physical close.
    pub notify_master: bool,
}

/// Rekey the `tab_to_session` binding when WT mints a new stable tab id
/// for an existing tab (cross-window tab drag — see
/// `App::rename_tab_session`). The chat-history side rekeys in `app.rs`,
/// but `tab_to_session` lives in the ACP client task and can't be
/// rekeyed from `&mut App` directly. Without this, the next prompt on
/// the dragged tab can't find the old SessionId and falls through to
/// the lazy-create branch — the agent CLI sees a fresh `session/new`
/// and loses turn context even though the visible chat is intact.
///
/// No-op when `old_tab_id` is absent from the map.
#[derive(Debug, Clone)]
pub struct RenameSessionRequest {
    pub old_tab_id: String,
    pub new_tab_id: String,
}

impl PromptSubmission {
    pub fn new(text: String, pane_context: Option<PaneContext>) -> Self {
        Self::new_with_kind(text, pane_context, None)
    }

    pub fn new_autofix(text: String, pane_context: Option<PaneContext>) -> Self {
        Self::new_with_kind(text, pane_context, Some(AutofixTextKind::UserRequest))
    }

    pub fn new_autofix_failure(text: String, pane_context: Option<PaneContext>) -> Self {
        Self::new_with_kind(text, pane_context, Some(AutofixTextKind::FailureSummary))
    }

    pub fn new_agent_command(text: String, pane_context: Option<PaneContext>) -> Self {
        let mut prompt = Self::new_with_kind(text, pane_context, None);
        prompt.agent_command = true;
        prompt
    }

    fn new_with_kind(
        text: String,
        pane_context: Option<PaneContext>,
        autofix_text_kind: Option<AutofixTextKind>,
    ) -> Self {
        static NEXT_PROMPT_ID: AtomicU64 = AtomicU64::new(1);
        Self {
            id: NEXT_PROMPT_ID.fetch_add(1, Ordering::Relaxed),
            text,
            pane_context,
            submitted_at_unix_s: now_unix_s(),
            autofix_text_kind,
            agent_command: false,
            images: Vec::new(),
            is_byok: false,
            agent_id: String::new(),
        }
    }

    pub fn is_autofix(&self) -> bool {
        self.autofix_text_kind.is_some()
    }

    pub fn is_agent_command(&self) -> bool {
        self.agent_command
    }

    pub fn with_byok(mut self, is_byok: bool) -> Self {
        self.is_byok = is_byok;
        self
    }

    pub fn is_byok(&self) -> bool {
        self.is_byok
    }

    pub fn with_agent_id(mut self, agent_id: String) -> Self {
        self.agent_id = agent_id;
        self
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Attach pasted images (Alt+V) to a human-entered prompt.
    pub fn with_images(mut self, images: Vec<crate::clipboard_image::PastedImage>) -> Self {
        self.images = images;
        self
    }

    pub fn preview(&self) -> String {
        prompt_preview(&self.text)
    }
}

async fn complete_prompt_request<T>(
    result: std::result::Result<T, acp::Error>,
    soft_stop: Option<SoftStopReason>,
    prompt_timing: &PromptTimingState,
    event_tx: &mpsc::UnboundedSender<AppEvent>,
    session_id: String,
) {
    match result {
        Ok(_) => {
            let timing_note = prompt_timing.complete(&session_id, true, None);
            if let Some(note) = timing_note {
                let _ = event_tx.send(AppEvent::TimingMetric {
                    session_id: session_id.clone(),
                    note,
                });
            }
            // Defensive workaround for ACP-non-compliant agents.
            //
            // ACP requires the Agent to send all pending `session/update`
            // notifications BEFORE responding to `session/prompt` (see ACP
            // 0.10 agent.rs:80-101 — `prompt` "Returns when the turn is
            // complete with a stop reason"). In practice GitHub Copilot
            // occasionally flushes a few trailing AgentMessageChunk
            // notifications a few hundred microseconds AFTER the
            // PromptResponse, which leaves the streaming buffer truncated
            // when `AgentMessageEnd` triggers `App::turn_close`. We sleep
            // briefly so the stragglers land in the buffer before the
            // state machine commits the turn.
            //
            // Once Copilot honors the spec, this delay can be removed.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let _ = event_tx.send(AppEvent::AgentMessageEnd {
                session_id: session_id.clone(),
            });
            // A successful turn can still end on a soft stop (truncation /
            // request-budget / refusal). It is NOT a connection failure — the
            // session stays Connected — so it rides its own event and only
            // appends an informational line AFTER `AgentMessageEnd` has flushed
            // the agent's streamed content.
            if let Some(reason) = soft_stop {
                let _ = event_tx.send(AppEvent::AgentSoftStop { session_id, reason });
            }
        }
        Err(e) => {
            let error_message = e.to_string();
            let failure = AgentFailure::from_acp_error(&e);
            let timing_note = prompt_timing.complete(&session_id, false, Some(&error_message));
            if let Some(note) = timing_note {
                let _ = event_tx.send(AppEvent::TimingMetric {
                    session_id: session_id.clone(),
                    note,
                });
            }
            let _ = event_tx.send(AppEvent::AgentError {
                session_id: Some(session_id),
                failure,
                message: format!("prompt error: {}", error_message),
            });
        }
    }
}

fn acp_log(msg: &str) {
    tracing::debug!(target: "acp", "{}", msg);
}

fn acp_error_detail(error: &acp::Error) -> String {
    error
        .data
        .as_ref()
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&error.message)
        .to_string()
}

/// Log potentially-sensitive content (user prompt / agent message text,
/// previews, full ACP payloads) at **trace only**, so it never lands in
/// shipping (`info`) or default-troubleshooting (`debug`) logs. Enable with
/// `WTA_LOG=trace` when a human is deliberately deep-debugging.
fn acp_trace_content(msg: &str) {
    tracing::trace!(target: "acp.content", "{}", msg);
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
        acp_log(&format!(
            "{} (t+{:.3}s)",
            msg,
            self.begin.elapsed().as_secs_f64()
        ));
    }
}

/// Shared state accessible from the Client trait impl.
struct ClientState {
    event_tx: mpsc::UnboundedSender<AppEvent>,
    shell_mgr: Arc<ShellManager>,
    prompt_timing: Arc<PromptTimingState>,
    provider_probe_capture: ProviderProbeCapture,
    standard_usage_sessions: Mutex<HashSet<String>>,
    proposal_channels: Arc<crate::agent_tools::action_proposal::channel::ProposalChannelManager>,
    hidden_tool_calls: std::sync::Mutex<HashMap<(String, String), SessionMcpTool>>,
}

#[derive(Default)]
struct ProviderProbeCapture {
    active: Mutex<HashMap<String, String>>,
}

impl ProviderProbeCapture {
    fn begin(&self, session_id: &str) -> bool {
        let mut active = self.active.lock().unwrap();
        if active.contains_key(session_id) {
            return false;
        }
        active.insert(session_id.to_string(), String::new());
        true
    }

    fn capture_text(&self, session_id: &str, text: &str) -> bool {
        let mut active = self.active.lock().unwrap();
        let Some(output) = active.get_mut(session_id) else {
            return false;
        };
        output.push_str(text);
        true
    }

    fn is_active(&self, session_id: &str) -> bool {
        self.active.lock().unwrap().contains_key(session_id)
    }

    fn finish(&self, session_id: &str) -> Option<String> {
        self.active.lock().unwrap().remove(session_id)
    }
}

/// Our Client trait implementation — handles incoming agent requests and notifications.
#[derive(Clone)]
struct WtaClient {
    state: Arc<ClientState>,
}

struct UserInputUiGuard {
    event_tx: mpsc::UnboundedSender<AppEvent>,
    request_id: String,
    session_id: String,
    armed: bool,
}

impl UserInputUiGuard {
    fn new(
        event_tx: mpsc::UnboundedSender<AppEvent>,
        request_id: String,
        session_id: String,
    ) -> Self {
        Self {
            event_tx,
            request_id,
            session_id,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for UserInputUiGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.event_tx.send(AppEvent::CancelUserInputRequest {
                request_id: self.request_id.clone(),
                session_id: self.session_id.clone(),
            });
        }
    }
}

/// Maximum characters kept in a tool-call `location` hint before truncation.
/// Long enough for a typical path or one-line shell command, short enough
/// that a runaway `raw_input` value (e.g. a full file-edit payload) can't
/// blow up the chat card into a wall of text.
const TOOL_CALL_LOCATION_MAX_CHARS: usize = 200;
const TOOL_CALL_OUTPUT_MAX_CHARS: usize = 4000;

fn tool_call_kind(kind: acp::schema::v1::ToolKind) -> crate::app::ToolCallKind {
    use crate::app::ToolCallKind as AppKind;
    use acp::schema::v1::ToolKind;

    match kind {
        ToolKind::Read => AppKind::Read,
        ToolKind::Edit => AppKind::Edit,
        ToolKind::Delete => AppKind::Delete,
        ToolKind::Move => AppKind::Move,
        ToolKind::Search => AppKind::Search,
        ToolKind::Execute => AppKind::Execute,
        ToolKind::Think => AppKind::Think,
        ToolKind::Fetch => AppKind::Fetch,
        ToolKind::SwitchMode => AppKind::SwitchMode,
        _ => AppKind::Other,
    }
}

fn bounded_tool_output_parts<'a>(
    parts: impl DoubleEndedIterator<Item = &'a str>,
) -> Option<crate::app::ToolCallOutput> {
    let mut reversed = String::new();
    let mut kept_chars = 0;
    let mut has_content = false;
    let mut truncated = false;

    'parts: for part in parts.rev().filter(|part| !part.is_empty()) {
        if has_content {
            if kept_chars == TOOL_CALL_OUTPUT_MAX_CHARS {
                truncated = true;
                break;
            }
            reversed.push('\n');
            kept_chars += 1;
        }
        has_content = true;
        for ch in part.chars().rev() {
            if kept_chars == TOOL_CALL_OUTPUT_MAX_CHARS {
                truncated = true;
                break 'parts;
            }
            reversed.push(ch);
            kept_chars += 1;
        }
    }

    has_content.then(|| crate::app::ToolCallOutput {
        text: reversed.chars().rev().collect(),
        truncated,
    })
}

fn tool_call_content_text(
    content: &[acp::schema::v1::ToolCallContent],
) -> Option<crate::app::ToolCallOutput> {
    bounded_tool_output_parts(content.iter().filter_map(|item| {
        let acp::schema::v1::ToolCallContent::Content(content) = item else {
            return None;
        };
        let acp::schema::v1::ContentBlock::Text(text) = &content.content else {
            return None;
        };
        Some(text.text.as_str())
    }))
}

fn bounded_tool_output(text: &str) -> crate::app::ToolCallOutput {
    bounded_tool_output_parts(std::iter::once(text)).unwrap_or(crate::app::ToolCallOutput {
        text: String::new(),
        truncated: false,
    })
}

fn tool_call_content(
    content: &[acp::schema::v1::ToolCallContent],
) -> Vec<crate::app::ToolCallContent> {
    use crate::app::ToolCallContent as AppContent;
    use acp::schema::v1::{ContentBlock, EmbeddedResourceResource, ToolCallContent};

    content
        .iter()
        .map(|item| match item {
            ToolCallContent::Content(content) => match &content.content {
                ContentBlock::Text(text) => AppContent::Text(bounded_tool_output(&text.text)),
                ContentBlock::Image(image) => AppContent::Attachment {
                    label: image.mime_type.clone(),
                    uri: image.uri.clone(),
                },
                ContentBlock::Audio(audio) => AppContent::Attachment {
                    label: audio.mime_type.clone(),
                    uri: None,
                },
                ContentBlock::ResourceLink(resource) => AppContent::Attachment {
                    label: resource
                        .title
                        .clone()
                        .unwrap_or_else(|| resource.name.clone()),
                    uri: Some(resource.uri.clone()),
                },
                ContentBlock::Resource(resource) => match &resource.resource {
                    EmbeddedResourceResource::TextResourceContents(resource) => {
                        AppContent::Attachment {
                            label: resource.uri.clone(),
                            uri: None,
                        }
                    }
                    EmbeddedResourceResource::BlobResourceContents(resource) => {
                        AppContent::Attachment {
                            label: resource
                                .mime_type
                                .as_deref()
                                .map_or_else(|| resource.uri.clone(), str::to_string),
                            uri: resource.mime_type.as_ref().map(|_| resource.uri.clone()),
                        }
                    }
                    _ => AppContent::Attachment {
                        label: "?".to_string(),
                        uri: None,
                    },
                },
                _ => AppContent::Attachment {
                    label: "?".to_string(),
                    uri: None,
                },
            },
            ToolCallContent::Diff(diff) => AppContent::Diff {
                path: diff.path.to_string_lossy().into_owned(),
                old_text: diff.old_text.as_deref().map(bounded_tool_output),
                new_text: bounded_tool_output(&diff.new_text),
            },
            ToolCallContent::Terminal(terminal) => AppContent::Terminal {
                id: terminal.terminal_id.to_string(),
                output: None,
                exit_code: None,
            },
            _ => AppContent::Attachment {
                label: "?".to_string(),
                uri: None,
            },
        })
        .collect()
}

fn tool_call_locations(
    locations: &[acp::schema::v1::ToolCallLocation],
) -> Vec<crate::app::ToolCallLocation> {
    locations
        .iter()
        .map(|location| crate::app::ToolCallLocation {
            path: location.path.to_string_lossy().into_owned(),
            line: location.line,
        })
        .collect()
}

fn raw_output_text(raw_output: &serde_json::Value) -> Option<crate::app::ToolCallOutput> {
    if let Some(text) = raw_output.as_str().filter(|text| !text.is_empty()) {
        return bounded_tool_output_parts(std::iter::once(text));
    }

    let object = raw_output.as_object()?;
    let streams = ["stdout", "stderr"]
        .into_iter()
        .filter_map(|key| object.get(key)?.as_str())
        .filter(|text| !text.is_empty());
    if let Some(output) = bounded_tool_output_parts(streams) {
        return Some(output);
    }

    ["output", "text"]
        .into_iter()
        .find_map(|key| object.get(key)?.as_str().filter(|text| !text.is_empty()))
        .and_then(|text| bounded_tool_output_parts(std::iter::once(text)))
}

fn tool_call_output(
    content: &[acp::schema::v1::ToolCallContent],
    raw_output: Option<&serde_json::Value>,
) -> Option<crate::app::ToolCallOutput> {
    tool_call_content_text(content).or_else(|| raw_output.and_then(raw_output_text))
}

fn tool_call_cwd(raw_input: Option<&serde_json::Value>) -> Option<String> {
    let object = raw_input?.as_object()?;
    ["cwd", "workingDirectory", "working_directory"]
        .into_iter()
        .find_map(|key| object.get(key)?.as_str())
        .map(str::trim)
        .filter(|cwd| !cwd.is_empty())
        .map(str::to_string)
}

fn tool_call_exit_code(raw_output: Option<&serde_json::Value>) -> Option<i64> {
    let object = raw_output?.as_object()?;
    ["exitCode", "exit_code"]
        .into_iter()
        .find_map(|key| object.get(key)?.as_i64())
}

/// Best-effort extraction of *what* a tool call is touching: a file path
/// (from `locations`/`raw_input.path`/`raw_input.file_path`), a shell
/// command (from `raw_input.command`/`commands`), or a sanitized Fetch URL.
/// Returns
/// `(text, is_command)` so callers can decide how to render it — a path
/// reads fine inline, but a command can be long and benefits from its own
/// code-styled line (see `ChatMessage::ToolCall::location_is_command`,
/// `PermissionState::target_is_command`).
///
/// Falls back to `None` rather than dumping the entire `raw_input` JSON,
/// which would be noisy and could leak large payloads (e.g. file contents
/// for a write/edit call) into the chat scrollback.
fn tool_call_target(
    kind: Option<&acp::schema::v1::ToolKind>,
    locations: &[acp::schema::v1::ToolCallLocation],
    raw_input: Option<&serde_json::Value>,
) -> Option<(String, bool)> {
    if let Some(location) = locations
        .iter()
        .find(|loc| !loc.path.to_string_lossy().trim().is_empty())
    {
        let path = location.path.to_string_lossy().into_owned();
        let target = location
            .line
            .map_or_else(|| path.clone(), |line| format!("{path}:{line}"));
        return Some((target, false));
    }
    let raw_input = raw_input?;
    if let Some(p) = raw_input
        .get("path")
        .or_else(|| raw_input.get("file_path"))
        .and_then(|v| v.as_str())
        .filter(|value| !value.trim().is_empty())
    {
        return Some((p.to_string(), false));
    }
    if let Some(c) = raw_input
        .get("command")
        .and_then(|v| v.as_str())
        .filter(|value| !value.trim().is_empty())
    {
        return Some((c.to_string(), true));
    }
    if let Some(c) = raw_input
        .get("commands")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .find(|value| !value.trim().is_empty())
        })
    {
        return Some((c.to_string(), true));
    }
    let fetch_target = matches!(kind, Some(acp::schema::v1::ToolKind::Fetch))
        || (kind.is_none()
            && ["url", "uri"]
                .into_iter()
                .any(|key| raw_input.get(key).is_some()));
    if fetch_target {
        let target = ["url", "uri"]
            .into_iter()
            .find_map(|key| raw_input.get(key)?.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())?;
        return sanitize_fetch_target(target).map(|target| (target, false));
    }
    None
}

fn sanitize_fetch_target(target: &str) -> Option<String> {
    let (scheme, remainder) = target
        .split_once("://")
        .map_or(("", target), |(scheme, remainder)| (scheme, remainder));
    let safe_end = remainder.find(['?', '#']).unwrap_or(remainder.len());
    let safe = &remainder[..safe_end];
    let (authority, path) = safe.split_once('/').unwrap_or((safe, ""));
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if authority.is_empty() {
        return None;
    }
    let separator = if scheme.is_empty() { "" } else { "://" };
    let path_separator = if path.is_empty() { "" } else { "/" };
    Some(format!(
        "{scheme}{separator}{authority}{path_separator}{path}"
    ))
}

/// Truncates a `tool_call_target` string to `TOOL_CALL_LOCATION_MAX_CHARS`,
/// appending `…` when it had to cut.
fn truncate_target(mut text: String) -> String {
    if let Some((cut_at, _)) = text.char_indices().nth(TOOL_CALL_LOCATION_MAX_CHARS) {
        text.truncate(cut_at);
        text.push('…');
    }
    text
}

/// Best-effort one-line summary of *what* a tool call is touching, shown as
/// a dim suffix (or, for commands, its own line) under the tool-call title
/// in the **chat** card (see `ChatMessage::ToolCall::location`). Without
/// this, cards only ever show the agent's often-generic `title` (e.g.
/// "Access paths outside trusted directories") with no indication of the
/// actual file/command involved.
///
/// `title` is the card's own title text; when the agent has already baked
/// the hint into the title itself (common for read/view tool calls, e.g.
/// title "Viewing C:\...\rust-app" with `locations: [{"path":
/// "C:\...\rust-app"}]`), appending it again would just print the same
/// string twice — `"Viewing X (X)"`. In that case we return `None` so the
/// card shows the title alone. This dedupe is intentionally **not** applied
/// on the permission dialog (see `request_permission`) — that card is a
/// decision point, so restating the target explicitly is useful even if
/// it repeats what a preceding chat tool-call card already showed.
fn tool_call_location_hint(
    title: &str,
    kind: Option<&acp::schema::v1::ToolKind>,
    locations: &[acp::schema::v1::ToolCallLocation],
    raw_input: Option<&serde_json::Value>,
) -> Option<(String, bool)> {
    let (hint, is_command) = tool_call_target(kind, locations, raw_input)?;
    let hint = truncate_target(hint);
    let comparison_hint = hint.strip_suffix('…').unwrap_or(&hint);

    // Don't repeat text the title already contains — case-insensitive so
    // "Viewing C:\...\rust-app" still dedupes against a locations path that
    // differs only in case (e.g. drive-letter casing from a different code
    // path).
    let fetch_target = matches!(kind, Some(acp::schema::v1::ToolKind::Fetch))
        || (kind.is_none()
            && raw_input.is_some_and(|input| {
                ["url", "uri"]
                    .into_iter()
                    .any(|key| input.get(key).is_some())
            }));
    if !fetch_target
        && !title.is_empty()
        && title
            .to_lowercase()
            .contains(&comparison_hint.to_lowercase())
    {
        return None;
    }

    Some((hint, is_command))
}

/// Short icon glyph for an ACP `ToolKind`, shown next to the title on the
/// permission dialog (`PermissionState::kind_label`) so "Always allow" has
/// *some* visual indication of what class of operation it covers — WTA has
/// no visibility into the agent CLI's actual grant scope (that's entirely
/// internal to the agent), so this is deliberately just a hint, not a claim
/// about what "always" will match.
///
/// Deliberately a symbol, not an English word ("Read"/"Edit"/…) — this repo
/// localizes every user-facing string into 85+ locales (see
/// `rust-localization.instructions.md`), and a kind label is exactly the
/// kind of ambiguous 1-2-word string that risks mistranslation (e.g.
/// "Execute" reads as "kill" in several languages). A glyph sidesteps that
/// entirely while still giving a scannable per-kind visual cue, consistent
/// with how the rest of the chat UI already uses unlabeled marker glyphs
/// (bullets, arrows) rather than words.
///
/// `None` for kinds with no useful visual framing (`Think`, `SwitchMode`,
/// `Other`/unset) — the header just shows the title alone.
fn tool_call_kind_label(kind: Option<&acp::schema::v1::ToolKind>) -> Option<&'static str> {
    use acp::schema::v1::ToolKind;
    match kind? {
        ToolKind::Read | ToolKind::Search | ToolKind::Move => Some("→"),
        ToolKind::Edit => Some("✎"),
        ToolKind::Delete => Some("✕"),
        ToolKind::Execute => Some("$"),
        ToolKind::Fetch => Some("%"),
        _ => None,
    }
}

fn session_update_kind(update: &acp::schema::v1::SessionUpdate) -> &'static str {
    match update {
        acp::schema::v1::SessionUpdate::AgentThoughtChunk(_) => "agent_thought_chunk",
        acp::schema::v1::SessionUpdate::AgentMessageChunk(_) => "agent_message_chunk",
        acp::schema::v1::SessionUpdate::ToolCall(_) => "tool_call",
        acp::schema::v1::SessionUpdate::ToolCallUpdate(_) => "tool_call_update",
        acp::schema::v1::SessionUpdate::Plan(_) => "plan",
        acp::schema::v1::SessionUpdate::UsageUpdate(_) => "usage_update",
        acp::schema::v1::SessionUpdate::AvailableCommandsUpdate(_) => "available_commands_update",
        _ => "other",
    }
}

fn canonical_proposal_permission_command(
    args: &acp::schema::v1::RequestPermissionRequest,
) -> Option<&str> {
    if args.tool_call.fields.kind != Some(acp::schema::v1::ToolKind::Execute) {
        return None;
    }
    let raw_input = args.tool_call.fields.raw_input.as_ref()?.as_object()?;
    if raw_input.len() != 2 {
        return None;
    }
    let command = raw_input.get("command")?.as_str()?;
    let commands = raw_input.get("commands")?.as_array()?;
    (commands.len() == 1 && commands.first()?.as_str()? == command).then_some(command)
}

fn proposal_permission_command_candidate(
    args: &acp::schema::v1::RequestPermissionRequest,
) -> Option<&str> {
    if args.tool_call.fields.kind != Some(acp::schema::v1::ToolKind::Execute) {
        return None;
    }
    proposal_command_candidate(args.tool_call.fields.raw_input.as_ref())
}

fn proposal_command_candidate(raw_input: Option<&serde_json::Value>) -> Option<&str> {
    raw_input?.as_object()?.get("command")?.as_str()
}

fn is_session_mcp_server_name(name: &str) -> bool {
    crate::agent_tools::session_mcp::server_name_matches(name)
}

fn is_dynamic_session_mcp_server_name(name: &str) -> bool {
    name != "intelligent_terminal" && is_session_mcp_server_name(name)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionMcpTool {
    TerminalAction(crate::agent_tools::action_proposal::schema::McpActionTool),
    UserInput,
}

impl SessionMcpTool {
    const ALL: [Self; 4] = [
        Self::TerminalAction(
            crate::agent_tools::action_proposal::schema::McpActionTool::RunCommandInCurrentShell,
        ),
        Self::TerminalAction(
            crate::agent_tools::action_proposal::schema::McpActionTool::CreateWorkspace,
        ),
        Self::TerminalAction(
            crate::agent_tools::action_proposal::schema::McpActionTool::DelegateTaskInNewWorkspace,
        ),
        Self::UserInput,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::TerminalAction(tool) => tool.tool_name(),
            Self::UserInput => "request_user_input",
        }
    }
}

fn session_mcp_tool_from_title_matching(
    title: Option<&str>,
    server_name_matches: fn(&str) -> bool,
) -> Option<SessionMcpTool> {
    let Some(title) = title.map(str::trim) else {
        return None;
    };
    let title = title.strip_prefix("Use MCP tool: ").unwrap_or(title);
    for tool in SessionMcpTool::ALL {
        for separator in ["/", "-"] {
            if let Some(server_name) = title.strip_suffix(&format!("{separator}{}", tool.name())) {
                if server_name_matches(server_name) {
                    return Some(tool);
                }
            }
        }
        if title
            .strip_prefix("mcp__")
            .and_then(|title| title.strip_suffix(&format!("__{}", tool.name())))
            .is_some_and(server_name_matches)
        {
            return Some(tool);
        }
    }
    None
}

fn session_mcp_tool_from_title(title: Option<&str>) -> Option<SessionMcpTool> {
    session_mcp_tool_from_title_matching(title, is_session_mcp_server_name)
}

fn session_mcp_tool_from_dynamic_title(title: Option<&str>) -> Option<SessionMcpTool> {
    session_mcp_tool_from_title_matching(title, is_dynamic_session_mcp_server_name)
}

fn looks_like_proposal_command(command: &str) -> bool {
    fn segment_invokes_proposal(segment: &str) -> bool {
        let segment = segment.trim_start();
        let segment = segment
            .strip_prefix('&')
            .map(str::trim_start)
            .unwrap_or(segment);
        let mut words = segment.split_whitespace();
        let Some(executable) = words.next() else {
            return false;
        };
        let executable = executable.trim_matches(['"', '\'']);
        let executable_name = executable.rsplit(['\\', '/']).next().unwrap_or(executable);
        let is_wta = executable.eq_ignore_ascii_case("$env:WTA_CLI_PATH")
            || executable_name.eq_ignore_ascii_case("wta")
            || executable_name.eq_ignore_ascii_case("wta.exe");
        is_wta && words.next() == Some("propose-terminal-actions")
    }

    let mut segment_start = 0;
    let mut quote = None;
    let mut chars = command.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if ch == '`' && quote != Some('\'') {
            chars.next();
            continue;
        }
        if let Some(delimiter) = quote {
            if ch == delimiter {
                if delimiter == '\'' && chars.peek().is_some_and(|(_, next)| *next == '\'') {
                    chars.next();
                } else {
                    quote = None;
                }
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            continue;
        }

        let separator_len = if matches!(ch, '|' | ';' | '\r' | '\n') {
            ch.len_utf8()
        } else if ch == '&' && chars.peek().is_some_and(|(_, next)| *next == '&') {
            chars.next();
            2
        } else {
            continue;
        };
        if segment_invokes_proposal(&command[segment_start..index]) {
            return true;
        }
        segment_start = index + separator_len;
    }
    segment_invokes_proposal(&command[segment_start..])
}

impl WtaClient {
    async fn dispatch_session_notification(&self, args: acp::schema::v1::SessionNotification) {
        let usage_session_id =
            matches!(&args.update, acp::schema::v1::SessionUpdate::UsageUpdate(_))
                .then(|| args.session_id.0.to_string());

        if self.session_notification(args).await.is_err() {
            if let Some(session_id) = usage_session_id {
                tracing::warn!(
                    target: "usage",
                    schema = ACP_SESSION_USAGE_SCHEMA,
                    source = "acp_standard",
                    outcome = "rejected",
                    "usage update rejected"
                );
                let _ = self
                    .state
                    .event_tx
                    .send(AppEvent::UsageCleared { session_id });
            }
        }
    }

    fn hide_session_mcp_tool_call(
        &self,
        session_id: &str,
        tool_call_id: &str,
        tool: SessionMcpTool,
    ) {
        self.state
            .hidden_tool_calls
            .lock()
            .unwrap()
            .insert((session_id.to_string(), tool_call_id.to_string()), tool);
        let _ = self.state.event_tx.send(AppEvent::HideToolCall {
            session_id: session_id.to_string(),
            id: tool_call_id.to_string(),
        });
    }

    fn hidden_session_mcp_tool(
        &self,
        session_id: &str,
        tool_call_id: &str,
    ) -> Option<SessionMcpTool> {
        self.state
            .hidden_tool_calls
            .lock()
            .unwrap()
            .get(&(session_id.to_string(), tool_call_id.to_string()))
            .copied()
    }

    async fn request_permission(
        &self,
        args: acp::schema::v1::RequestPermissionRequest,
    ) -> acp::Result<acp::schema::v1::RequestPermissionResponse> {
        acp_log("request_permission received");
        // Tool-call title is agent-generated content — trace only.
        acp_trace_content(&format!(
            "request_permission title: {:?}",
            args.tool_call.fields.title
        ));
        let session_id = args.session_id.0.to_string();
        let tool_call_id = args.tool_call.tool_call_id.to_string();
        let proposal_candidate = proposal_permission_command_candidate(&args);
        // Permission titles are agent-authored. Only a title carrying the
        // validated per-session MCP server name, or a tool call already
        // correlated through the hidden-call map, is trusted for silent
        // approval. The public action names are intentionally generic and a
        // bare `run_command_in_current_shell` from an unrelated provider tool must not bypass
        // the normal permission UI merely because its payload has the same
        // shape.
        let session_mcp_tool = session_mcp_tool_from_title(args.tool_call.fields.title.as_deref())
            .or_else(|| self.hidden_session_mcp_tool(&session_id, &tool_call_id));
        if let Some(tool) = session_mcp_tool {
            self.hide_session_mcp_tool_call(&session_id, &tool_call_id, tool);
        } else if proposal_candidate.is_some_and(looks_like_proposal_command) {
            self.hide_session_mcp_tool_call(
                &session_id,
                &tool_call_id,
                SessionMcpTool::TerminalAction(
                    crate::agent_tools::action_proposal::schema::McpActionTool::RunCommandInCurrentShell,
                ),
            );
        }
        let title = args
            .tool_call
            .fields
            .title
            .clone()
            .unwrap_or_else(|| "Permission requested".to_string());
        let kind_label = tool_call_kind_label(args.tool_call.fields.kind.as_ref());
        // Unlike the chat tool-call card, the permission dialog never
        // dedupes the target against the title — it's a decision point,
        // so restating exactly what path/command is involved is
        // intentional even if it repeats a preceding tool-call card.
        let target_hint = tool_call_target(
            args.tool_call.fields.kind.as_ref(),
            args.tool_call.fields.locations.as_deref().unwrap_or(&[]),
            args.tool_call.fields.raw_input.as_ref(),
        )
        .map(|(text, is_command)| (truncate_target(text), is_command));
        // Fallback single-line text for the compact (1-row) card — see
        // `PermissionState::description`.
        let description = match &target_hint {
            Some((target, _)) => format!("{title} ({target})"),
            None => title.clone(),
        };
        self.state
            .prompt_timing
            .permission_requested(&session_id, &description);

        if let Some(tool) = session_mcp_tool {
            let Some(option) = args
                .options
                .iter()
                .find(|option| option.kind == acp::schema::v1::PermissionOptionKind::AllowOnce)
            else {
                self.state
                    .prompt_timing
                    .permission_resolved(&session_id, "proposal_cancelled");
                return Ok(acp::schema::v1::RequestPermissionResponse::new(
                    acp::schema::v1::RequestPermissionOutcome::Cancelled,
                ));
            };
            let permission_result = match tool {
                SessionMcpTool::TerminalAction(_) => self
                    .state
                    .proposal_channels
                    .validate_mcp_permission(&session_id),
                SessionMcpTool::UserInput => Ok(()),
            };
            tracing::info!(
                target: "session_mcp_permission",
                session_id = %session_id,
                tool = tool.name(),
                approved = permission_result.is_ok(),
                status = ?permission_result.as_ref().err().map(|failure| failure.status),
                "silently resolving session MCP permission"
            );
            if permission_result.is_err() {
                self.state
                    .prompt_timing
                    .permission_resolved(&session_id, "session_mcp_permission_rejected");
                return Ok(acp::schema::v1::RequestPermissionResponse::new(
                    acp::schema::v1::RequestPermissionOutcome::Cancelled,
                ));
            }
            self.state
                .prompt_timing
                .permission_resolved(&session_id, "session_mcp_allow_once");
            return Ok(acp::schema::v1::RequestPermissionResponse::new(
                acp::schema::v1::RequestPermissionOutcome::Selected(
                    acp::schema::v1::SelectedPermissionOutcome::new(option.option_id.clone()),
                ),
            ));
        }

        if let Some(command) = canonical_proposal_permission_command(&args) {
            match crate::agent_tools::action_proposal::invocation::parse(command) {
                Ok(invocation) => {
                    let Some(option) = args.options.iter().find(|option| {
                        option.kind == acp::schema::v1::PermissionOptionKind::AllowOnce
                    }) else {
                        self.state
                            .prompt_timing
                            .permission_resolved(&session_id, "proposal_cancelled");
                        return Ok(acp::schema::v1::RequestPermissionResponse::new(
                            acp::schema::v1::RequestPermissionOutcome::Cancelled,
                        ));
                    };
                    let permission_result = self
                        .state
                        .proposal_channels
                        .validate_permission(&session_id, &invocation.channel);
                    tracing::info!(
                        target: "proposal_permission",
                        session_id = %session_id,
                        approved = permission_result.is_ok(),
                        status = ?permission_result.as_ref().err().map(|failure| failure.status),
                        "silently resolving canonical proposal permission"
                    );
                    if permission_result.is_err() {
                        self.state
                            .prompt_timing
                            .permission_resolved(&session_id, "proposal_permission_rejected");
                        return Ok(acp::schema::v1::RequestPermissionResponse::new(
                            acp::schema::v1::RequestPermissionOutcome::Cancelled,
                        ));
                    }
                    self.state
                        .prompt_timing
                        .permission_resolved(&session_id, "proposal_allow_once");
                    return Ok(acp::schema::v1::RequestPermissionResponse::new(
                        acp::schema::v1::RequestPermissionOutcome::Selected(
                            acp::schema::v1::SelectedPermissionOutcome::new(
                                option.option_id.clone(),
                            ),
                        ),
                    ));
                }
                Err(reason) if looks_like_proposal_command(command) => {
                    tracing::info!(
                        target: "proposal_permission",
                        session_id = %session_id,
                        reason,
                        "silently cancelled non-canonical proposal command"
                    );
                    self.state
                        .prompt_timing
                        .permission_resolved(&session_id, "proposal_noncanonical");
                    return Ok(acp::schema::v1::RequestPermissionResponse::new(
                        acp::schema::v1::RequestPermissionOutcome::Cancelled,
                    ));
                }
                Err(_) => {}
            }
        } else if proposal_candidate.is_some_and(looks_like_proposal_command) {
            self.state
                .prompt_timing
                .permission_resolved(&session_id, "proposal_noncanonical");
            return Ok(acp::schema::v1::RequestPermissionResponse::new(
                acp::schema::v1::RequestPermissionOutcome::Cancelled,
            ));
        }

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

        let (target, target_is_command) = match target_hint {
            Some((text, is_command)) => (Some(text), is_command),
            None => (None, false),
        };
        let _ = self.state.event_tx.send(AppEvent::PermissionRequest {
            session_id: session_id.clone(),
            tool_call_id,
            description,
            title,
            kind_label: kind_label.map(str::to_string),
            target,
            target_is_command,
            options,
            responder: resp_tx,
        });

        // Wait for user to choose
        match resp_rx.await {
            Ok(option_id) => {
                self.state
                    .prompt_timing
                    .permission_resolved(&session_id, "selected");
                Ok(acp::schema::v1::RequestPermissionResponse::new(
                    acp::schema::v1::RequestPermissionOutcome::Selected(
                        acp::schema::v1::SelectedPermissionOutcome::new(option_id),
                    ),
                ))
            }
            Err(_) => {
                self.state
                    .prompt_timing
                    .permission_resolved(&session_id, "cancelled");
                Ok(acp::schema::v1::RequestPermissionResponse::new(
                    acp::schema::v1::RequestPermissionOutcome::Cancelled,
                ))
            }
        }
    }

    async fn session_notification(
        &self,
        args: acp::schema::v1::SessionNotification,
    ) -> acp::Result<()> {
        let kind = session_update_kind(&args.update);
        let sid = args.session_id.0.to_string();
        if self.state.provider_probe_capture.is_active(&sid) {
            if let acp::schema::v1::SessionUpdate::AgentMessageChunk(chunk) = &args.update {
                if let acp::schema::v1::ContentBlock::Text(text_content) = &chunk.content {
                    self.state
                        .provider_probe_capture
                        .capture_text(&sid, &text_content.text);
                }
                return Ok(());
            }
            if !matches!(args.update, acp::schema::v1::SessionUpdate::UsageUpdate(_)) {
                return Ok(());
            }
        }
        // Per-streamed-chunk; trace-only (not via acp_log's debug) so default
        // debug logs aren't flooded with one line per token chunk.
        tracing::trace!(target: "acp", "session_notification: kind={}", kind);
        // The full update carries agent message/thought text, tool-call
        // content, plan bodies, and replayed user-message chunks — trace only.
        // Usage values remain redacted even at trace level.
        if kind != "usage_update" {
            acp_trace_content(&format!("session_notification update: {:?}", args.update));
        }
        self.state.prompt_timing.observe_session_update(&sid, kind);
        match args.update {
            acp::schema::v1::SessionUpdate::UserMessageChunk(chunk) => {
                // Replayed historical user prompt from `session/load`.
                // In the normal prompt flow the agent doesn't emit
                // these (the client sent the user text itself), so
                // this branch only fires during a load replay. The
                // App handler gates on `loading_session` and drops
                // late-arrivers.
                let message_id = chunk.message_id.map(|id| id.to_string());
                if let acp::schema::v1::ContentBlock::Text(text_content) = chunk.content {
                    let _ = self.state.event_tx.send(AppEvent::UserMessageReplayChunk {
                        session_id: sid,
                        message_id,
                        text: text_content.text,
                    });
                }
            }
            acp::schema::v1::SessionUpdate::AgentThoughtChunk(chunk) => {
                if let acp::schema::v1::ContentBlock::Text(text_content) = chunk.content {
                    if !text_content.text.trim().is_empty() {
                        self.state
                            .prompt_timing
                            .observe_first_text(&sid, text_content.text.len());
                    }
                    let _ = self.state.event_tx.send(AppEvent::AgentThoughtChunk {
                        session_id: sid,
                        text: text_content.text,
                    });
                }
            }
            acp::schema::v1::SessionUpdate::AgentMessageChunk(chunk) => {
                if let acp::schema::v1::ContentBlock::Text(text_content) = chunk.content {
                    self.state
                        .prompt_timing
                        .observe_first_text(&sid, text_content.text.len());
                    let _ = self.state.event_tx.send(AppEvent::AgentMessageChunk {
                        session_id: sid,
                        text: text_content.text,
                    });
                }
            }
            acp::schema::v1::SessionUpdate::ToolCall(tool_call) => {
                let tool_call_id = tool_call.tool_call_id.to_string();
                if let Some(tool) = session_mcp_tool_from_dynamic_title(Some(&tool_call.title)) {
                    self.hide_session_mcp_tool_call(&sid, &tool_call_id, tool);
                    return Ok(());
                }
                if proposal_command_candidate(tool_call.raw_input.as_ref())
                    .is_some_and(looks_like_proposal_command)
                {
                    self.hide_session_mcp_tool_call(
                        &sid,
                        &tool_call_id,
                        SessionMcpTool::TerminalAction(
                            crate::agent_tools::action_proposal::schema::McpActionTool::RunCommandInCurrentShell,
                        ),
                    );
                    return Ok(());
                }
                if self.hidden_session_mcp_tool(&sid, &tool_call_id).is_some() {
                    return Ok(());
                }
                self.state
                    .prompt_timing
                    .observe_first_tool_call(&sid, Some(tool_call.title.as_str()));
                let (location, location_is_command) = match tool_call_location_hint(
                    &tool_call.title,
                    Some(&tool_call.kind),
                    &tool_call.locations,
                    tool_call.raw_input.as_ref(),
                ) {
                    Some((text, is_command)) => (Some(text), is_command),
                    None => (None, false),
                };
                let _ = self.state.event_tx.send(AppEvent::ToolCall {
                    session_id: sid,
                    id: tool_call_id,
                    title: tool_call.title.clone(),
                    status: format!("{:?}", tool_call.status),
                    kind: tool_call_kind(tool_call.kind),
                    location,
                    location_is_command,
                    cwd: tool_call_cwd(tool_call.raw_input.as_ref()),
                    output: tool_call_output(&tool_call.content, tool_call.raw_output.as_ref()),
                    exit_code: tool_call_exit_code(tool_call.raw_output.as_ref()),
                    content: tool_call_content(&tool_call.content),
                    locations: tool_call_locations(&tool_call.locations),
                });
            }
            acp::schema::v1::SessionUpdate::ToolCallUpdate(update) => {
                let tool_call_id = update.tool_call_id.to_string();
                if let Some(tool) =
                    session_mcp_tool_from_dynamic_title(update.fields.title.as_deref())
                {
                    self.hide_session_mcp_tool_call(&sid, &tool_call_id, tool);
                    return Ok(());
                }
                if proposal_command_candidate(update.fields.raw_input.as_ref())
                    .is_some_and(looks_like_proposal_command)
                {
                    self.hide_session_mcp_tool_call(
                        &sid,
                        &tool_call_id,
                        SessionMcpTool::TerminalAction(
                            crate::agent_tools::action_proposal::schema::McpActionTool::RunCommandInCurrentShell,
                        ),
                    );
                    return Ok(());
                }
                if self.hidden_session_mcp_tool(&sid, &tool_call_id).is_some() {
                    return Ok(());
                }
                // Failed updates frequently carry a `raw_output.message`
                // explaining why. Keep that concise reason in the status
                // while also forwarding any reported output independently.
                let status = update.fields.status.as_ref().map(|status| {
                    let reason = update
                        .fields
                        .raw_output
                        .as_ref()
                        .and_then(|v| v.get("message"))
                        .and_then(|m| m.as_str())
                        .map(str::trim)
                        .filter(|message| !message.is_empty());
                    match reason {
                        Some(message)
                            if matches!(status, acp::schema::v1::ToolCallStatus::Failed) =>
                        {
                            format!("{:?}: {}", status, message)
                        }
                        _ => format!("{:?}", status),
                    }
                });
                // Collections in ACP updates replace their previous value.
                // An empty content collection therefore emits an empty output
                // patch so the reducer clears stale text.
                let output = if let Some(content) = &update.fields.content {
                    Some(
                        tool_call_content_text(content)
                            .or_else(|| update.fields.raw_output.as_ref().and_then(raw_output_text))
                            .unwrap_or(crate::app::ToolCallOutput {
                                text: String::new(),
                                truncated: false,
                            }),
                    )
                } else {
                    update.fields.raw_output.as_ref().and_then(raw_output_text)
                };
                let (location, location_is_command) =
                    if update.fields.locations.is_some() || update.fields.raw_input.is_some() {
                        match tool_call_location_hint(
                            update.fields.title.as_deref().unwrap_or(""),
                            update.fields.kind.as_ref(),
                            update.fields.locations.as_deref().unwrap_or(&[]),
                            update.fields.raw_input.as_ref(),
                        ) {
                            Some((text, is_command)) => (Some(text), is_command),
                            None => (None, false),
                        }
                    } else {
                        (None, false)
                    };
                let cwd = tool_call_cwd(update.fields.raw_input.as_ref());
                let exit_code = tool_call_exit_code(update.fields.raw_output.as_ref());
                let content = update.fields.content.as_deref().map(tool_call_content);
                let locations = update.fields.locations.as_deref().map(tool_call_locations);
                if update.fields.title.is_some()
                    || status.is_some()
                    || update.fields.kind.is_some()
                    || location.is_some()
                    || output.is_some()
                    || cwd.is_some()
                    || exit_code.is_some()
                    || content.is_some()
                    || locations.is_some()
                {
                    let _ = self.state.event_tx.send(AppEvent::ToolCallUpdate {
                        session_id: sid,
                        id: tool_call_id,
                        title: update.fields.title,
                        status,
                        kind: update.fields.kind.map(tool_call_kind),
                        location,
                        location_is_command,
                        output,
                        content,
                        locations,
                        cwd,
                        exit_code,
                    });
                }
            }
            acp::schema::v1::SessionUpdate::Plan(plan) => {
                let entries = plan
                    .entries
                    .iter()
                    .map(|e| PlanEntry {
                        content: e.content.clone(),
                        status: match e.status {
                            acp::schema::v1::PlanEntryStatus::Completed => {
                                PlanEntryStatus::Completed
                            }
                            acp::schema::v1::PlanEntryStatus::InProgress => {
                                PlanEntryStatus::InProgress
                            }
                            _ => PlanEntryStatus::Pending,
                        },
                    })
                    .collect();
                let _ = self.state.event_tx.send(AppEvent::Plan {
                    session_id: sid,
                    entries,
                });
            }
            acp::schema::v1::SessionUpdate::UsageUpdate(update) => {
                self.state
                    .standard_usage_sessions
                    .lock()
                    .unwrap()
                    .insert(sid.clone());
                let snapshot = crate::usage::normalize_standard_usage(&update);
                let _ = self.state.event_tx.send(AppEvent::UsageReported {
                    session_id: sid,
                    snapshot,
                });
            }
            acp::schema::v1::SessionUpdate::AvailableCommandsUpdate(update) => {
                let commands =
                    crate::protocol::acp::session_commands::normalize(&update.available_commands);
                let _ = self.state.event_tx.send(AppEvent::SessionCommandsUpdated {
                    session_id: sid,
                    commands,
                });
            }
            acp::schema::v1::SessionUpdate::ConfigOptionUpdate(update) => {
                let (available_models, current_model_id) =
                    crate::protocol::acp::model_select::models_from_config_options(
                        &sid,
                        &update.config_options,
                    )
                    .unwrap_or_default();
                let _ = self.state.event_tx.send(AppEvent::SessionConfigUpdated {
                    session_id: sid.clone(),
                    options: crate::protocol::acp::session_config::select_options(
                        &update.config_options,
                    ),
                });
                let _ = self.state.event_tx.send(AppEvent::ModelConfigUpdated {
                    session_id: sid,
                    available_models,
                    current_model_id,
                });
            }
            _ => {} // Ignore other update types for now
        }
        Ok(())
    }

    async fn create_terminal(
        &self,
        args: acp::schema::v1::CreateTerminalRequest,
    ) -> acp::Result<acp::schema::v1::CreateTerminalResponse> {
        acp_log(&format!(
            "create_terminal called: arg_count={}",
            args.args.len()
        ));
        // Agent-requested command line can carry user/file content — trace only.
        acp_trace_content(&format!(
            "create_terminal cmd={} args={:?}",
            args.command, args.args
        ));
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

        let session_id = args.session_id.0.to_string();
        let title = format!("{} {}", args.command, args.args.join(" "));
        // Working directory doubles as this card's location hint — the
        // title already has the full command line, but `cwd` is otherwise
        // shown nowhere and is useful context for a relative-path command.
        // Skip it if the command line already names that directory, to
        // avoid printing the same path twice on one line.
        let location = args
            .cwd
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .filter(|cwd| !title.to_lowercase().contains(&cwd.to_lowercase()));
        match self.state.shell_mgr.create_terminal(config).await {
            Ok(id) => {
                // Show tool-call-like feedback
                let _ = self.state.event_tx.send(AppEvent::ToolCall {
                    session_id,
                    id: id.clone(),
                    title,
                    status: "running".to_string(),
                    kind: crate::app::ToolCallKind::Execute,
                    location,
                    location_is_command: false,
                    cwd: None,
                    output: None,
                    exit_code: None,
                    content: Vec::new(),
                    locations: Vec::new(),
                });
                Ok(acp::schema::v1::CreateTerminalResponse::new(id))
            }
            Err(e) => Err(acp::Error::internal_error().data(e.to_string())),
        }
    }

    async fn terminal_output(
        &self,
        args: acp::schema::v1::TerminalOutputRequest,
    ) -> acp::Result<acp::schema::v1::TerminalOutputResponse> {
        match self
            .state
            .shell_mgr
            .get_output(&args.terminal_id.to_string())
            .await
        {
            Ok(output) => {
                let terminal_id = args.terminal_id.to_string();
                let session_id = args.session_id.0.to_string();
                let app_output = bounded_tool_output(&output.data);
                let exit_code = output.exit_status.map(i64::from);
                let _ = self.state.event_tx.send(AppEvent::ToolTerminalOutput {
                    session_id,
                    terminal_id,
                    output: app_output,
                    exit_code,
                });
                let mut resp = acp::schema::v1::TerminalOutputResponse::new(output.data, false);
                if let Some(code) = output.exit_status {
                    resp = resp
                        .exit_status(acp::schema::v1::TerminalExitStatus::new().exit_code(code));
                }
                Ok(resp)
            }
            Err(e) => Err(acp::Error::internal_error().data(e.to_string())),
        }
    }

    async fn wait_for_terminal_exit(
        &self,
        args: acp::schema::v1::WaitForTerminalExitRequest,
    ) -> acp::Result<acp::schema::v1::WaitForTerminalExitResponse> {
        let tid = args.terminal_id.to_string();
        let session_id = args.session_id.0.to_string();

        match self.state.shell_mgr.wait_for_exit(&tid).await {
            Ok(code) => {
                // Update tool call status
                let _ = self.state.event_tx.send(AppEvent::ToolCallUpdate {
                    session_id,
                    id: tid,
                    title: None,
                    status: Some(format!("exited ({})", code)),
                    kind: None,
                    location: None,
                    location_is_command: false,
                    output: None,
                    content: None,
                    locations: None,
                    cwd: None,
                    exit_code: Some(i64::from(code)),
                });
                Ok(acp::schema::v1::WaitForTerminalExitResponse::new(
                    acp::schema::v1::TerminalExitStatus::new().exit_code(code),
                ))
            }
            Err(e) => Err(acp::Error::internal_error().data(e.to_string())),
        }
    }

    async fn release_terminal(
        &self,
        args: acp::schema::v1::ReleaseTerminalRequest,
    ) -> acp::Result<acp::schema::v1::ReleaseTerminalResponse> {
        let _ = self
            .state
            .shell_mgr
            .release(&args.terminal_id.to_string())
            .await;
        Ok(acp::schema::v1::ReleaseTerminalResponse::new())
    }

    async fn kill_terminal(
        &self,
        args: acp::schema::v1::KillTerminalRequest,
    ) -> acp::Result<acp::schema::v1::KillTerminalResponse> {
        let _ = self
            .state
            .shell_mgr
            .kill(&args.terminal_id.to_string())
            .await;
        Ok(acp::schema::v1::KillTerminalResponse::new())
    }

    async fn request_terminal_actions(
        &self,
        args: acp::schema::v1::ExtRequest,
    ) -> acp::Result<acp::schema::v1::ExtResponse> {
        use crate::agent_tools::action_proposal::channel::{
            ProposalFinalStatus, ProposalValidationStatus,
        };
        use crate::agent_tools::action_proposal::pipe::{
            ProposalPayloadSource, ProposalValidationDecision, ProposalValidationResponse,
            ValidationPhase,
        };

        let request: crate::agent_tools::session_mcp::HelperRequest =
            serde_json::from_str(args.params.get()).map_err(|error| {
                acp::Error::invalid_params().data(format!(
                    "invalid terminal action request parameters: {error}"
                ))
            })?;
        let action_tool = {
            use crate::agent_tools::action_proposal::schema::McpActionTool;
            McpActionTool::from_tool_name(&request.tool).ok_or_else(|| {
                acp::Error::invalid_params().data(format!(
                    "unknown terminal action tool `{}`; expected one of: {}",
                    request.tool,
                    McpActionTool::ALL
                        .iter()
                        .map(|tool| tool.tool_name())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?
        };
        let payload = serde_json::to_string(&request.arguments).map_err(|error| {
            acp::Error::internal_error().data(format!(
                "failed to encode terminal action arguments: {error}"
            ))
        })?;
        if payload.len() > crate::agent_tools::action_proposal::schema::MAX_PAYLOAD_BYTES {
            let response = ProposalValidationResponse {
                phase: ValidationPhase::Validation,
                status: ProposalValidationStatus::InvalidSchema,
                proposal_id: None,
                reason: Some("terminal action request exceeds the payload limit".to_string()),
                retryable: false,
            };
            let raw = serde_json::value::to_raw_value(&response).map_err(|error| {
                acp::Error::internal_error().data(format!("encode proposal response: {error}"))
            })?;
            return Ok(acp::schema::v1::ExtResponse::new(raw.into()));
        }

        let context = match self
            .state
            .proposal_channels
            .begin_mcp_validation(&request.session_id)
        {
            Ok(context) => context,
            Err(failure) => {
                let response = ProposalValidationResponse {
                    phase: ValidationPhase::Validation,
                    status: failure.status,
                    proposal_id: None,
                    reason: Some(failure.reason.to_string()),
                    retryable: failure.retryable,
                };
                let raw = serde_json::value::to_raw_value(&response).map_err(|error| {
                    acp::Error::internal_error().data(format!("encode proposal response: {error}"))
                })?;
                return Ok(acp::schema::v1::ExtResponse::new(raw.into()));
            }
        };
        let proposal_id = context.proposal_id.clone();
        let (validation_tx, validation_rx) = tokio::sync::oneshot::channel();
        if self
            .state
            .event_tx
            .send(AppEvent::DirectTerminalActionProposal {
                context,
                payload,
                source: ProposalPayloadSource::Mcp(action_tool),
                responder: validation_tx,
            })
            .is_err()
        {
            self.state
                .proposal_channels
                .reject_validation(&proposal_id, false);
            return Err(acp::Error::internal_error().data("Helper UI is unavailable"));
        }
        let decision =
            match tokio::time::timeout(std::time::Duration::from_secs(10), validation_rx).await {
                Ok(Ok(decision)) => decision,
                Ok(Err(_)) => ProposalValidationDecision {
                    status: ProposalValidationStatus::Unavailable,
                    reason: Some("Helper dropped the validation response".to_string()),
                    retryable: false,
                },
                Err(_) => ProposalValidationDecision {
                    status: ProposalValidationStatus::Unavailable,
                    reason: Some("Helper validation timed out".to_string()),
                    retryable: false,
                },
            };
        if decision.status != ProposalValidationStatus::Accepted {
            let retryable = self
                .state
                .proposal_channels
                .reject_validation(&proposal_id, decision.retryable);
            let response = ProposalValidationResponse {
                phase: ValidationPhase::Validation,
                status: decision.status,
                proposal_id: None,
                reason: decision.reason,
                retryable,
            };
            let raw = serde_json::value::to_raw_value(&response).map_err(|error| {
                acp::Error::internal_error().data(format!("encode proposal response: {error}"))
            })?;
            return Ok(acp::schema::v1::ExtResponse::new(raw.into()));
        }
        if !self
            .state
            .proposal_channels
            .accept_validation_detached(&proposal_id)
        {
            return Err(acp::Error::internal_error()
                .data("terminal action request became stale after validation"));
        }

        let (commit_tx, commit_rx) = tokio::sync::oneshot::channel();
        if self
            .state
            .event_tx
            .send(AppEvent::DirectTerminalActionProposalCommit {
                proposal_id: proposal_id.clone(),
                responder: commit_tx,
            })
            .is_err()
        {
            self.state
                .proposal_channels
                .resolve_final(&proposal_id, ProposalFinalStatus::Unavailable);
            return Err(acp::Error::internal_error().data("Helper UI is unavailable"));
        }
        let committed = matches!(
            tokio::time::timeout(std::time::Duration::from_secs(10), commit_rx).await,
            Ok(Ok(true))
        );
        if !committed {
            self.state
                .proposal_channels
                .resolve_final(&proposal_id, ProposalFinalStatus::Unavailable);
            return Err(acp::Error::internal_error()
                .data("Helper did not confirm the terminal action card"));
        }

        let response = ProposalValidationResponse {
            phase: ValidationPhase::Validation,
            status: ProposalValidationStatus::Accepted,
            proposal_id: Some(proposal_id),
            reason: None,
            retryable: false,
        };
        let raw = serde_json::value::to_raw_value(&response).map_err(|error| {
            acp::Error::internal_error().data(format!("encode proposal response: {error}"))
        })?;
        Ok(acp::schema::v1::ExtResponse::new(raw.into()))
    }

    async fn request_user_input(
        &self,
        args: acp::schema::v1::ExtRequest,
    ) -> acp::Result<acp::schema::v1::ExtResponse> {
        let helper_request: crate::agent_tools::session_mcp::UserInputHelperRequest =
            serde_json::from_str(args.params.get()).map_err(|error| {
                acp::Error::invalid_params()
                    .data(format!("invalid user input request parameters: {error}"))
            })?;
        let request = helper_request
            .request
            .validate()
            .map_err(|error| acp::Error::invalid_params().data(error.to_string()))?;
        let (responder, response) = tokio::sync::oneshot::channel();
        let mut guard = UserInputUiGuard::new(
            self.state.event_tx.clone(),
            helper_request.request_id.clone(),
            helper_request.session_id.clone(),
        );
        self.state
            .event_tx
            .send(AppEvent::UserInputRequest {
                request_id: helper_request.request_id,
                session_id: helper_request.session_id,
                request,
                responder,
            })
            .map_err(|_| acp::Error::internal_error().data("Helper UI is unavailable"))?;
        let response = response
            .await
            .unwrap_or(crate::agent_tools::user_input::UserInputResponse::Cancelled);
        guard.disarm();
        let raw = serde_json::value::to_raw_value(&response).map_err(|error| {
            acp::Error::internal_error().data(format!("encode user input response: {error}"))
        })?;
        Ok(acp::schema::v1::ExtResponse::new(raw.into()))
    }

    async fn cancel_user_input(
        &self,
        args: acp::schema::v1::ExtRequest,
    ) -> acp::Result<acp::schema::v1::ExtResponse> {
        let request: crate::agent_tools::session_mcp::CancelUserInputHelperRequest =
            serde_json::from_str(args.params.get()).map_err(|error| {
                acp::Error::invalid_params().data(format!(
                    "invalid user input cancellation parameters: {error}"
                ))
            })?;
        self.state
            .event_tx
            .send(AppEvent::CancelUserInputRequest {
                request_id: request.request_id,
                session_id: request.session_id,
            })
            .map_err(|_| acp::Error::internal_error().data("Helper UI is unavailable"))?;
        Ok(acp::schema::v1::ExtResponse::new(
            serde_json::value::RawValue::from_string("{}".to_string())
                .map_err(|error| acp::Error::internal_error().data(error.to_string()))?
                .into(),
        ))
    }

    /// Receive `intellterm.wta/session_{added,removed}` notifications
    /// pushed by master so the helper's local `alive` mirror stays in
    /// sync without polling. We translate to an `AppEvent` rather than
    /// mutating the registry here because the registry is owned by
    /// `App` (constructed after the ACP client task spawns); routing
    /// through the event loop also keeps registry mutation
    /// single-writer and trace-able alongside other state changes.
    ///
    /// Unknown / malformed notifications are silently dropped — a
    /// future master may broadcast new methods we don't recognise, and
    /// surfacing the error here would tear down the connection on what
    /// is by definition optional, advisory data.
    async fn ext_notification(&self, args: acp::schema::v1::ExtNotification) -> acp::Result<()> {
        if let Some(catalog) =
            crate::protocol::acp::model_select::parse_wta_cloud_catalog_notification(&args)
        {
            match catalog {
                Ok(catalog) => {
                    tracing::info!(
                        target: "cloud_models",
                        source = ?catalog.source,
                        model_count = catalog.models.len(),
                        "received asynchronous native cloud model catalog from master"
                    );
                    let _ = self
                        .state
                        .event_tx
                        .send(AppEvent::CloudModelsAvailable(catalog.models));
                }
                Err(error) => {
                    tracing::warn!(
                        target: "cloud_models",
                        %error,
                        "dropping malformed asynchronous cloud model catalog"
                    );
                }
            }
            return Ok(());
        }

        use crate::session_registry::{parse_ext_notification, WtaExtNotification};
        match parse_ext_notification(&args) {
            WtaExtNotification::SessionAdded(info) => {
                let _ = self.state.event_tx.send(AppEvent::AliveSessionAdded(info));
            }
            WtaExtNotification::SessionRemoved(sid) => {
                let _ = self.state.event_tx.send(AppEvent::AliveSessionRemoved(sid));
            }
            WtaExtNotification::SessionsChanged => {
                let _ = self.state.event_tx.send(AppEvent::SessionsChanged);
            }
            WtaExtNotification::Unknown => {
                tracing::trace!(
                    target: "acp_client",
                    method = %args.method,
                    "ignoring ext-notification from unknown namespace"
                );
            }
            WtaExtNotification::MalformedParams { method, error } => {
                tracing::warn!(
                    target: "acp_client",
                    %method,
                    %error,
                    "dropping malformed intellterm.wta ext-notification"
                );
            }
        }
        Ok(())
    }
}

async fn capture_provider_command(
    conn: &conn::ClientLink,
    client: &WtaClient,
    session_id: &acp::schema::v1::SessionId,
    command: &'static str,
) -> Result<String> {
    const PROVIDER_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    let session_id_text = session_id.to_string();
    if !client.state.provider_probe_capture.begin(&session_id_text) {
        anyhow::bail!("provider probe already active for session");
    }
    let result = tokio::time::timeout(
        PROVIDER_PROBE_TIMEOUT,
        conn.prompt(acp::schema::v1::PromptRequest::new(
            session_id.clone(),
            vec![command.to_string().into()],
        )),
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let output = client
        .state
        .provider_probe_capture
        .finish(&session_id_text)
        .unwrap_or_default();

    match result {
        Ok(Ok(_)) => Ok(output),
        Ok(Err(error)) => anyhow::bail!("{} probe failed: {}", command, error),
        Err(_) => anyhow::bail!("{} probe timed out", command),
    }
}

async fn probe_private_usage(
    conn: &conn::ClientLink,
    client: &WtaClient,
    identity: &PromptUsageIdentity,
    session_id: acp::schema::v1::SessionId,
) -> Result<Option<crate::usage::UsageSnapshot>> {
    let Some(family_id) = identity.family_id.as_deref() else {
        return Ok(None);
    };
    let session_id_text = session_id.to_string();
    if client
        .state
        .standard_usage_sessions
        .lock()
        .unwrap()
        .contains(&session_id_text)
    {
        return Ok(None);
    }
    let Some(reporter_id) = identity.reporter_id.as_deref() else {
        return Ok(None);
    };
    let Some(adapter) = crate::usage::providers::lookup(family_id) else {
        return Ok(None);
    };
    if adapter.private_usage_policy()
        != crate::usage::providers::PrivateUsagePolicy::VerifiedCommandProbe
        || !adapter.trusted_reporter_ids().contains(&reporter_id)
    {
        return Ok(None);
    }

    let mut snapshot = crate::usage::normalize_provider_contribution(Default::default());

    for command in adapter.post_turn_commands() {
        match capture_provider_command(conn, client, &session_id, command).await {
            Ok(output) => {
                let contribution = adapter.extract_private_usage(
                    crate::usage::providers::ProviderUsageRequest {
                        reporter_id: Some(reporter_id),
                        input: crate::usage::providers::ProviderUsageInput::ProviderCommandOutput {
                            command,
                            text: &output,
                        },
                    },
                )?;
                snapshot.merge(crate::usage::normalize_provider_contribution(contribution));
            }
            Err(error) => {
                tracing::warn!(
                    target: "usage",
                    %family_id,
                    session_id = %session_id_text,
                    %command,
                    error = %error,
                    "optional provider usage command failed"
                );
            }
        }
    }

    if snapshot.context.is_none() && snapshot.cost.is_none() && snapshot.provider_metrics.is_empty()
    {
        return Ok(None);
    }
    Ok(Some(snapshot))
}

/// The helper-mode ACP client loop. Instead of spawning the agent CLI
/// as a child process and talking ACP over its stdio, this connects to
/// a wta-master singleton over the named pipe whose path is passed in
/// `pipe_name` and speaks ACP over that pipe. The master (from this
/// helper's perspective) plays the role of the agent.
///
/// Wires the App-facing select-loop, minus the restart-loop wrapper: helper
/// mode doesn't own the agent CLI lifetime (master does). `/restart` is
/// delegated to C++ via `restart_agent_stack`; C++ replaces the master while
/// retaining viable panes, and each helper reconnects its saved binding over
/// the stable pipe when the old transport closes.
///
/// See doc/specs/Multi-window-agent-pane.md for the helper+master
/// architecture, and `tools/wta/src/master/mod.rs` for the peer.

/// Process-wide owner tab StableId for this helper, seeded at startup and
/// rekeyed when WT drags the tab into another window. Every later
/// `session/new` / `session/load` reads the current value so master does not
/// get re-poisoned with the pre-drag StableId.
static HELPER_OWNER_TAB_ID: std::sync::OnceLock<std::sync::RwLock<Option<String>>> =
    std::sync::OnceLock::new();

/// Seed the process-wide owner tab StableId. Empty/blank ids are stored as
/// `None`.
pub fn set_helper_owner_tab_id(owner_tab_id: Option<&str>) {
    let normalized = owner_tab_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    *HELPER_OWNER_TAB_ID
        .get_or_init(|| std::sync::RwLock::new(None))
        .write()
        .unwrap() = normalized;
}

fn helper_owner_tab_id() -> Option<String> {
    HELPER_OWNER_TAB_ID
        .get()
        .and_then(|owner| owner.read().unwrap().clone())
}

fn rename_helper_owner_tab_id(old_tab_id: &str, new_tab_id: &str) {
    let Some(owner) = HELPER_OWNER_TAB_ID.get() else {
        return;
    };
    let mut owner = owner.write().unwrap();
    if owner.as_deref() == Some(old_tab_id) {
        *owner = Some(new_tab_id.to_string());
    }
}

/// Inject `_meta.wta.pane_session_id = $WT_SESSION` (lowercased, no
/// braces) and `_meta.wta.owner_tab_id = <this helper's StableId>` into
/// an outbound ACP `session/new` or `session/load` request, when this
/// helper is running inside a Windows Terminal pane.
///
/// Used by the helper-over-master path to tell `wta-master` which WT
/// pane owns the session it's about to create or rehydrate (so focus /
/// session-list resolution works) and which WT tab owns it (so close-by-tab
/// can resolve the exact helper). Master records both in `SessionRegistry`
/// and its per-helper ownership map.
///
/// No-op for whichever fields are unavailable: `pane_session_id` when
/// `WT_SESSION` is unset/empty (e.g. running outside a WT pane in
/// tests), `owner_tab_id` when `--owner-tab-id` wasn't supplied.
fn inject_wta_pane_meta(meta: &mut Option<acp::schema::v1::Meta>, proposal_mcp_enabled: bool) {
    let wt_session = std::env::var("WT_SESSION").unwrap_or_default();
    let pane_session_id = {
        let normalized = wt_session
            .trim_matches(|c| c == '{' || c == '}')
            .to_ascii_lowercase();
        if normalized.is_empty() {
            None
        } else {
            Some(normalized)
        }
    };
    let owner_tab_id = helper_owner_tab_id();
    if pane_session_id.is_none() && owner_tab_id.is_none() && !proposal_mcp_enabled {
        return;
    }
    crate::session_registry::inject_wta_meta(
        meta,
        &crate::session_registry::WtaMeta {
            pane_session_id,
            owner_tab_id,
            proposal_mcp: proposal_mcp_enabled.then(|| "http-v1".to_string()),
            ..Default::default()
        },
    );
}

fn take_retired_session_result(meta: &mut Option<acp::schema::v1::Meta>) -> bool {
    crate::session_registry::extract_wta_meta(meta)
        .session_result
        .as_deref()
        == Some("retired")
}

fn elapsed_ms_since(start: std::time::Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

fn acp_result_failure_fields<T>(result: &acp::Result<T>) -> (&'static str, i32) {
    match result {
        Ok(_) => ("", 0),
        Err(e) => ("AcpError", e.code.into()),
    }
}

fn timeout_result_failure_fields<T>(
    result: &std::result::Result<acp::Result<T>, tokio::time::error::Elapsed>,
) -> (&'static str, i32) {
    match result {
        Ok(inner) => acp_result_failure_fields(inner),
        Err(_) => ("Timeout", 0),
    }
}

fn log_acp_initialize_timeout_result(
    route: &str,
    started: std::time::Instant,
    result: &std::result::Result<
        acp::Result<acp::schema::v1::InitializeResponse>,
        tokio::time::error::Elapsed,
    >,
) {
    let (failure_kind, acp_error_code) = timeout_result_failure_fields(result);
    crate::telemetry::log_acp_initialize_complete(
        elapsed_ms_since(started),
        matches!(result, Ok(Ok(_))),
        route,
        failure_kind,
        acp_error_code,
    );
}

fn log_acp_new_session_result(
    route: &str,
    started: std::time::Instant,
    result: &acp::Result<acp::schema::v1::NewSessionResponse>,
) {
    let session_id = result.as_ref().ok().map(|resp| resp.session_id.to_string());
    let (failure_kind, acp_error_code) = acp_result_failure_fields(result);
    crate::telemetry::log_acp_new_session_complete(
        session_id.as_deref(),
        elapsed_ms_since(started),
        result.is_ok(),
        route,
        failure_kind,
        acp_error_code,
    );
}

/// Handle a `session/load` failure (Err or timeout) in the
/// `load_session_rx` arm of `run_acp_client_over_pipe`.
///
/// Two cases:
///   * `old_sid = Some` (mid-life session management load failure): restore the prior
///     binding so the pane keeps a usable session. The user sees a
///     `TabError` and their existing session is still alive.
///   * `old_sid = None` (boot-time load failure with no bootstrap):
///     fall back to creating a fresh `new_session` so the pane is
///     still usable. The user sees a `TabError` AND a working blank
///     session, matching the pre-Plan-B UX where a bootstrap session
///     was always created.
async fn handle_load_failure(
    old_sid: Option<&acp::schema::v1::SessionId>,
    tab_id: String,
    cwd: std::path::PathBuf,
    conn: conn::ClientLink,
    tab_to_session: Arc<tokio::sync::Mutex<HashMap<String, acp::schema::v1::SessionId>>>,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    error_message: String,
    _proposal_channels: Arc<crate::agent_tools::action_proposal::channel::ProposalChannelManager>,
    proposal_mcp_enabled: bool,
) {
    if let Some(old) = old_sid {
        // Mid-life session management load failure path: restore prior binding.
        let mut g = tab_to_session.lock().await;
        g.insert(tab_id.clone(), old.clone());
        drop(g);
        let _ = event_tx.send(AppEvent::TabError {
            tab_id,
            message: error_message,
        });
        return;
    }

    // Boot-time load failure: helper has no prior session for this
    // tab (we skipped the bootstrap when `--initial-load-session-id`
    // was set). Create a fresh `new_session` so prompts have
    // somewhere to land.
    let _ = event_tx.send(AppEvent::TabError {
        tab_id: tab_id.clone(),
        message: format!("{} Starting a fresh session instead.", error_message),
    });
    let mut new_req = acp::schema::v1::NewSessionRequest::new(cwd);
    inject_wta_pane_meta(&mut new_req.meta, proposal_mcp_enabled);
    let fallback_started = std::time::Instant::now();
    let fallback = conn.new_session(new_req).await;
    log_acp_new_session_result("HelperPipeFallback", fallback_started, &fallback);
    match fallback {
        Ok(mut resp) => {
            if take_retired_session_result(&mut resp.meta) {
                tracing::info!(
                    target: "acp_load_session",
                    tab = %tab_id,
                    session_id = %resp.session_id,
                    "ignoring boot-time fallback session retired during tab reset or close"
                );
                return;
            }
            let new_sid = resp.session_id.clone();
            tracing::info!(
                target: "acp_load_session",
                tab = %tab_id,
                fallback_session_id = %new_sid,
                "boot-time load fell back to new_session successfully"
            );
            {
                let mut g = tab_to_session.lock().await;
                g.insert(tab_id.clone(), new_sid.clone());
            }
            // Index the fallback session as an agent-pane origin so
            // session management view can show it as a Historical row on next cold start
            // (it is now a real, persistent session).
            let pane_session_id = std::env::var("WT_SESSION").unwrap_or_default();
            let pane_for_index = if pane_session_id.is_empty() {
                None
            } else {
                Some(pane_session_id.as_str())
            };
            crate::agent_pane_origin::append_default(new_sid.0.as_ref(), pane_for_index);
            let (available_models, current_model_id) =
                crate::protocol::acp::model_select::models_from_new_session(&resp);
            let _ = event_tx.send(AppEvent::SessionAttached {
                tab_id,
                session_id: new_sid.to_string(),
                available_models,
                current_model_id,
            });
            publish_session_config_options(&event_tx, &new_sid, resp.config_options.as_deref());
        }
        Err(e) => {
            tracing::error!(
                target: "acp_load_session",
                tab = %tab_id,
                error = ?e,
                "boot-time load fallback new_session failed"
            );
            let _ = event_tx.send(AppEvent::TabError {
                tab_id,
                message: format!("Fallback new_session also failed: {}", e),
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run_acp_client_over_pipe(
    pipe_name: String,
    acp_model_override: Option<String>,
    custom_model_selection: Option<String>,
    supplied_cloud_models: Vec<AcpModelInfo>,
    // Per-tab agent identity. Forwarded to the multi-agent master in the
    // `initialize` handshake's `_meta.wta.agent_id` so master selects and
    // reconstructs the matching agent CLI for THIS tab from the id alone
    // (it never executes a command string sent over the pipe). `None` →
    // master uses its `--agent` default (the legacy single-agent behavior).
    agent_id: Option<String>,
    agent_source: crate::agent_source::AgentSource,
    source_cwd: Option<String>,
    owner_tab_id: Option<String>,
    initial_load_session_id: Option<String>,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    mut prompt_rx: mpsc::UnboundedReceiver<PromptSubmission>,
    mut cancel_rx: mpsc::UnboundedReceiver<CancelRequest>,
    mut new_session_rx: mpsc::UnboundedReceiver<NewSessionForTab>,
    mut load_session_rx: mpsc::UnboundedReceiver<LoadSessionForTab>,
    mut drop_session_rx: mpsc::UnboundedReceiver<DropSessionRequest>,
    mut rename_session_rx: mpsc::UnboundedReceiver<RenameSessionRequest>,
    mut restart_rx: mpsc::UnboundedReceiver<AgentLifecycleRequest>,
    mut session_hook_rx: mpsc::UnboundedReceiver<crate::app::QueuedSessionHook>,
    mut master_ext_rx: mpsc::UnboundedReceiver<MasterExtRequest>,
    shell_mgr: Arc<ShellManager>,
    wt_connected: bool,
    post_login_reconnect: bool,
    proposal_channels: Arc<crate::agent_tools::action_proposal::channel::ProposalChannelManager>,
) -> Result<AcpClientExit> {
    let startup_probe = StartupProbe::new();
    let usage_family_id = agent_id.as_deref().and_then(|agent_id| {
        let family_id = agent_id.trim().to_ascii_lowercase();
        crate::agent_registry::is_known_id(&family_id).then_some(family_id)
    });
    startup_probe.log(&format!(
        "run_acp_client_over_pipe task start pipe={} acp_model={:?} wt_connected={}",
        pipe_name, acp_model_override, wt_connected
    ));

    // Whether this WTA process is hosting an Intelligent Terminal agent
    // pane: `--owner-tab-id` is the
    // load-bearing signal. Helper mode is always spawned by WT with an
    // owner-tab-id, but we keep the same defensive default.
    let is_agent_pane = owner_tab_id
        .as_ref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);

    // Connect to the master singleton over the named pipe. The C++
    // SharedWta side spawns the master and the helper basically back
    // to back, so the helper races against master's startup — master
    // has to spawn its agent CLI subprocess and call `initialize`
    // (up to 60s for `npx` adapter cold-starts) BEFORE it opens the
    // pipe. Retry-with-backoff until master is ready or we give up
    // (spec Z-R6).
    let _ = event_tx.send(AppEvent::ConnectionStage(
        "Connecting to wta-master...".to_string(),
    ));
    startup_probe.log(&format!("opening master pipe: {}", pipe_name));
    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const ERROR_PIPE_BUSY: i32 = 231;
    let pipe = {
        let mut attempt: u32 = 0;
        let backoff_ms = if post_login_reconnect {
            POST_LOGIN_MASTER_PIPE_BACKOFF_MS
        } else {
            MASTER_PIPE_BACKOFF_MS
        };
        loop {
            match tokio::net::windows::named_pipe::ClientOptions::new().open(&pipe_name) {
                Ok(pipe) => {
                    // Always log the connect milestone at info (not just on
                    // retry) so a clean helper→master connect is visible in
                    // release logs, not only failures/retries.
                    tracing::info!(
                        target: "helper",
                        step = "pipe_connect",
                        pipe = %pipe_name,
                        attempts = attempt + 1,
                        "master pipe connected"
                    );
                    break pipe;
                }
                Err(e) => {
                    let raw = e.raw_os_error().unwrap_or(0);
                    let retryable = raw == ERROR_FILE_NOT_FOUND || raw == ERROR_PIPE_BUSY;
                    if !retryable || attempt as usize >= backoff_ms.len() {
                        tracing::warn!(
                            target: "helper",
                            step = "pipe_connect",
                            pipe = %pipe_name,
                            attempts = attempt + 1,
                            error = %e,
                            "master pipe connect giving up"
                        );
                        let detail = format!(
                            "connect to master pipe '{}' after {} attempt(s): {}",
                            pipe_name,
                            attempt + 1,
                            e
                        );
                        return Err(anyhow::Error::new(AgentFailure::HandshakeFailed {
                            stage: HandshakeStage::PipeConnect,
                            detail,
                        }));
                    }
                    let wait = backoff_ms[attempt as usize];
                    tracing::debug!(
                        target: "helper",
                        step = "pipe_connect",
                        pipe = %pipe_name,
                        attempt = attempt + 1,
                        wait_ms = wait,
                        error = %e,
                        "master pipe not ready, retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
                    attempt += 1;
                }
            }
        }
    };

    let (read_half, write_half) = tokio::io::split(pipe);
    let prompt_timing = Arc::new(PromptTimingState::default());
    let outgoing = write_half.compat_write();
    let incoming = read_half.compat();

    let state = Arc::new(ClientState {
        event_tx: event_tx.clone(),
        shell_mgr: shell_mgr.clone(),
        prompt_timing: prompt_timing.clone(),
        provider_probe_capture: ProviderProbeCapture::default(),
        standard_usage_sessions: Mutex::new(HashSet::new()),
        proposal_channels: Arc::clone(&proposal_channels),
        hidden_tool_calls: std::sync::Mutex::new(std::collections::HashMap::new()),
    });

    let client = WtaClient {
        state: state.clone(),
    };

    let builder = acp::Client
        .builder()
        .name("wta-helper")
        .on_receive_request(
            {
                let c = client.clone();
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
                            Q::ExtMethodRequest(a)
                                if crate::agent_tools::session_mcp::helper_method_matches(
                                    &a.method,
                                ) =>
                            {
                                conn::respond_enum(
                                    responder,
                                    c.request_terminal_actions(a)
                                        .await
                                        .map(R::ExtMethodResponse),
                                )
                            }
                            Q::ExtMethodRequest(a)
                                if crate::agent_tools::session_mcp::user_input_helper_method_matches(
                                    &a.method,
                                ) =>
                            {
                                conn::respond_enum(
                                    responder,
                                    c.request_user_input(a)
                                        .await
                                        .map(R::ExtMethodResponse),
                                )
                            }
                            Q::ExtMethodRequest(a)
                                if crate::agent_tools::session_mcp::cancel_user_input_helper_method_matches(
                                    &a.method,
                                ) =>
                            {
                                conn::respond_enum(
                                    responder,
                                    c.cancel_user_input(a)
                                        .await
                                        .map(R::ExtMethodResponse),
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
                let c = client.clone();
                move |notif: acp::schema::v1::AgentNotification, _cx| {
                    let c = c.clone();
                    async move {
                        use acp::schema::v1::AgentNotification as N;
                        match notif {
                            N::SessionNotification(n) => c.dispatch_session_notification(n).await,
                            N::ExtNotification(n) => {
                                let _ = c.ext_notification(n).await;
                            }
                            _ => {}
                        }
                        Ok(())
                    }
                }
            },
            acp::on_receive_notification!(),
        );

    let (conn, handle_io) = conn::spawn_client(builder, conn::byte_streams(outgoing, incoming));
    startup_probe.log("ACP client connection created (over pipe)");

    let intentional_shutdown = Arc::new(AtomicBool::new(false));
    let _transport_guard = ClientTransportGuard {
        conn: conn.clone(),
        suppress_transport_error: Arc::clone(&intentional_shutdown),
        event_tx: event_tx.clone(),
    };
    let io_probe = startup_probe.clone();
    let mut io_task = tokio::task::spawn_local(async move {
        io_probe.log("ACP handle_io task started (over pipe)");
        // The I/O loop only ends when the pipe to wta-master is gone. Crucially,
        // a *killed* master resolves this as **Ok(())** (clean EOF on the pipe),
        // not Err — confirmed from a real trace where `taskkill` on wta-master
        // produced "ACP handle_io completed", after which the UI sat on
        // `Connected` until the next prompt failed with "server shut down
        // unexpectedly". So BOTH arms must signal connection loss; keying only on
        // Err (the original F3 fix) would miss the common case.
        match handle_io.await {
            Err(e) => {
                tracing::warn!(target: "helper", error = %format!("{:#}", e), "ACP I/O loop to master failed");
            }
            Ok(()) => {
                io_probe.log("ACP handle_io completed (over pipe)");
                tracing::warn!(target: "helper", "ACP I/O loop to master ended — pipe closed (master gone)");
            }
        }
    });

    // Initialize — same as the child-process path. We use a 60s timeout
    // here because the first helper to connect to a fresh master may
    // ride along with the master's real agent CLI spawn (especially the
    // npx adapter cold start). Clean cloud discovery runs asynchronously
    // after that initialize and is delivered later, so it never consumes
    // this timeout budget. Subsequent inits are cached replays.
    let _ = event_tx.send(AppEvent::ConnectionStage("Initializing ACP...".to_string()));
    startup_probe.log("Initializing ACP (over pipe)");
    let init_started = std::time::Instant::now();
    let supplied_cloud_models = if matches!(&agent_source, crate::agent_source::AgentSource::Host) {
        supplied_cloud_models
    } else {
        if !supplied_cloud_models.is_empty() {
            tracing::warn!(
                target: "cloud_models",
                agent_source = %agent_source,
                "ignoring Host startup cloud catalog for WSL helper"
            );
        }
        Vec::new()
    };
    let init_request = {
        let mut req = acp::schema::v1::InitializeRequest::new(acp::schema::ProtocolVersion::V1)
            .client_capabilities(acp::schema::v1::ClientCapabilities::new().terminal(true))
            .client_info(
                acp::schema::v1::Implementation::new("wta-helper", env!("CARGO_PKG_VERSION"))
                    .title("Windows Terminal Agent (helper)"),
            );
        // Declare which agent this tab wants by *identity* — id + model.
        // The master selects + reconstructs the agent command from these
        // (it deliberately does NOT execute a command string sent over
        // the pipe — that would be an arbitrary-spawn surface for any
        // same-user process). Two tabs with different ids land on
        // different CLIs; same-id tabs share one. No command string is
        // ever put on the wire.
        crate::session_registry::inject_wta_meta(
            &mut req.meta,
            &crate::session_registry::WtaMeta {
                // Canonicalize + filter the same way the master does (trim,
                // ASCII-lowercase) and forward only *known* selectable ids.
                // The master reconstructs the command from the id and rejects
                // unknown / `custom:*` ids — forwarding those would trip an
                // "unknown selection" warn on every connect and then fall back
                // to the default anyway. Sending `None` makes that fallback
                // silent (master applies its own `--agent` default).
                agent_id: usage_family_id.clone(),
                model: acp_model_override.clone().filter(|s| !s.trim().is_empty()),
                provider_binding: Some(
                    custom_model_selection
                        .clone()
                        .filter(|selection| !selection.trim().is_empty())
                        .unwrap_or_else(|| "default".to_string()),
                ),
                agent_source: Some(agent_source.kind().to_string()),
                wsl_distro: agent_source.distro().map(str::to_string),
                cloud_models: if supplied_cloud_models.is_empty() {
                    None
                } else {
                    match serde_json::to_string(&supplied_cloud_models) {
                        Ok(models) => Some(models),
                        Err(error) => {
                            tracing::warn!(
                                target: "cloud_models",
                                %error,
                                "failed to serialize helper cloud model catalog metadata"
                            );
                            None
                        }
                    }
                },
                cloud_models_source: (!supplied_cloud_models.is_empty())
                    .then(|| "helper".to_string()),
                ..Default::default()
            },
        );
        req
    };
    let init_future = conn.initialize(init_request);
    let init_result = tokio::time::timeout(std::time::Duration::from_secs(60), init_future).await;
    log_acp_initialize_timeout_result("HelperPipe", init_started, &init_result);
    let mut init_resp = init_result
        .map_err(|_| {
            tracing::error!(
                target: "helper",
                step = "acp_initialize",
                pipe = %pipe_name,
                "ACP initialize over master pipe timed out after 60s — wta-master did not respond"
            );
            anyhow::anyhow!(
                "ACP initialize over master pipe timed out after 60s — \
             wta-master did not respond"
            )
        })?
        .map_err(|e| {
            tracing::error!(
                target: "helper",
                step = "acp_initialize",
                pipe = %pipe_name,
                error = %e,
                "ACP initialize over master pipe failed"
            );
            anyhow::Error::new(AgentFailure::HandshakeFailed {
                stage: HandshakeStage::Initialize,
                detail: acp_error_detail(&e),
            })
            .context("initialize over master pipe failed")
        })?;
    let wta_meta = crate::session_registry::extract_wta_meta(&mut init_resp.meta);
    let cloud_catalog = crate::protocol::acp::model_select::cloud_catalog_from_wta_meta(&wta_meta);
    if matches!(&agent_source, crate::agent_source::AgentSource::Host)
        && !cloud_catalog.models.is_empty()
    {
        tracing::info!(
            target: "cloud_models",
            source = ?cloud_catalog.source,
            model_count = cloud_catalog.models.len(),
            "received native cloud model catalog from master"
        );
        let _ = event_tx.send(AppEvent::CloudModelsAvailable(cloud_catalog.models));
    }
    let prompt_usage_identity = PromptUsageIdentity {
        family_id: usage_family_id,
        reporter_id: init_resp.agent_info.as_ref().map(|info| info.name.clone()),
    };
    let proposal_commands_supported = init_resp.agent_capabilities.mcp_capabilities.http
        && wta_meta.proposal_mcp.as_deref() == Some("http-v1");
    // Connection milestone at info so a clean handshake is visible in release.
    tracing::info!(
        target: "helper",
        step = "acp_initialize",
        pipe = %pipe_name,
        "ACP initialized over master pipe"
    );
    startup_probe.log(&format!(
        "Agent init response received (over pipe): {:?}",
        init_resp
    ));

    // ── Post-login authenticate ──────────────────────────────────────────
    // If this is a reconnect after LoginComplete (the user just completed
    // `copilot login` / `codex auth` / etc.), we MUST call `authenticate`
    // per ACP spec before attempting `new_session`. Without this, the
    // long-running agent CLI subprocess (owned by master) may not have
    // noticed the new disk-stored token — its internal auth state was set
    // at spawn time and may still be "not authenticated". The
    // `authenticate` RPC is the deterministic signal that tells the agent
    // "credentials changed, please re-check". See:
    // https://agentclientprotocol.com/protocol/initialization
    //
    // Tracks whether we actually completed a post-login `authenticate` (vs.
    // skipped it because the agent advertised no auth methods). Only then may
    // a still-AuthRequired `new_session` be classified as the distinct
    // "authenticate-OK-but-still-auth" recovery signal below.
    let mut post_login_authenticated = false;
    if post_login_reconnect {
        let auth_method_id = init_resp.auth_methods.first().map(|m| m.id().clone());
        if let Some(method_id) = auth_method_id {
            tracing::info!(
                target: "helper",
                method_id = %method_id.0,
                auth_methods_count = init_resp.auth_methods.len(),
                "post-login reconnect: sending authenticate to agent"
            );
            let auth_result = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                conn.authenticate(acp::schema::v1::AuthenticateRequest::new(method_id.clone())),
            )
            .await;
            match &auth_result {
                Ok(Ok(_)) => {
                    tracing::info!(
                        target: "helper",
                        method_id = %method_id.0,
                        "post-login authenticate succeeded"
                    );
                    post_login_authenticated = true;
                }
                Ok(Err(e)) => {
                    let failure = AgentFailure::from_acp_error(e);
                    tracing::error!(
                        target: "helper",
                        method_id = %method_id.0,
                        error_code = Into::<i32>::into(e.code),
                        error_message = %e.message,
                        "post-login authenticate failed"
                    );
                    if failure.is_auth() {
                        tracing::warn!(
                            target: "auth_recovery",
                            method_id = %method_id.0,
                            "post-login authenticate still AuthRequired; requesting fresh-master recovery"
                        );
                    }
                    return Err(post_login_authenticate_error(&method_id.0, e));
                }
                Err(_timeout) => {
                    tracing::error!(
                        target: "helper",
                        method_id = %method_id.0,
                        "post-login authenticate timed out (10s) — agent unresponsive"
                    );
                    return Err(anyhow::Error::new(AgentFailure::HandshakeFailed {
                        stage: crate::protocol::acp::failure::HandshakeStage::Authenticate,
                        detail: format!(
                            "authenticate({}) timed out after 10s — agent unresponsive. \
                             Try restarting Intelligent Terminal.",
                            method_id.0,
                        ),
                    }));
                }
            }
        } else {
            tracing::warn!(
                target: "helper",
                "post-login reconnect: no auth_methods advertised in initialize response; \
                 skipping authenticate (agent may not require it)"
            );
        }
    }

    // Bootstrap the alive-session mirror BEFORE creating our own
    // session. We want master's existing view in the registry first so
    // that any `intellterm.wta/session_added` notification for our own
    // brand-new session arrives after the snapshot — otherwise a stale
    // snapshot could overwrite it. Doing this before `new_session`
    // guarantees ordering: list_sessions completes → AliveSnapshotLoaded
    // queued → new_session → master broadcasts session_added →
    // AliveSessionAdded queued → both applied in arrival order on the
    // App event loop.
    //
    // The call is fire-and-forget: if list_sessions fails (e.g. an
    // older master without `unstable_session_list`) the alive mirror
    // just stays empty and `alive_loaded` stays false, which keeps
    // session management routing on the legacy path.
    match conn
        .list_sessions(acp::schema::v1::ListSessionsRequest::new())
        .await
    {
        Ok(resp) => {
            let items: Vec<crate::session_registry::SessionInfo> = resp
                .sessions
                .iter()
                .map(|wire| {
                    let mut meta = wire.meta.clone();
                    let wta = crate::session_registry::extract_wta_meta(&mut meta);
                    let mut info = crate::session_registry::SessionInfo::new(
                        wire.session_id.clone(),
                        wire.cwd.clone(),
                    );
                    info.title = wire.title.clone();
                    info.updated_at = wire.updated_at.clone();
                    info.pane_session_id = wta.pane_session_id;
                    info
                })
                .collect();
            startup_probe.log(&format!(
                "alive-session bootstrap: {} sessions from master",
                items.len()
            ));
            let _ = event_tx.send(AppEvent::AliveSnapshotLoaded(items));
        }
        Err(e) => {
            startup_probe.log(&format!(
                "alive-session bootstrap skipped (list_sessions failed): {e}"
            ));
        }
    }

    // Create the initial session bound to the owner tab — unless this
    // helper was spawned with `--initial-load-session-id`, in which case
    // we skip the bootstrap entirely and let the boot-time `load_session`
    // (queued by main.rs as an `AppEvent::WtEvent`) be the helper's
    // first session. Skipping the bootstrap avoids the session management duplicate-row
    // bug: master used to register both the bootstrap and the loaded
    // sid (both bound to the same WT pane) and the session management view showed two
    // Live rows for the same agent pane.
    let cwd = source_cwd
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| match &agent_source {
            crate::agent_source::AgentSource::Host => std::env::current_dir().unwrap_or_default(),
            crate::agent_source::AgentSource::Wsl { .. } => std::path::PathBuf::from("/"),
        });
    let (session_id, mut available_models, mut current_model_id, mut session_config, has_bootstrap) =
        if let Some(load_sid) = initial_load_session_id.as_deref() {
            // No bootstrap. AgentConnected fires with the to-be-loaded
            // sid as a placeholder so the App flips to Connected (and
            // binds session_id → owner_tab in `session_to_tab` early,
            // so any session/update chunks arriving before the
            // load_session response route to the right tab). The
            // actual `load_session` is driven by the App after it
            // processes the queued WtEvent — see `load_session_rx`
            // arm below for success/failure handling, including the
            // fallback-to-new-session on boot-time load failure.
            startup_probe.log(&format!(
                "skipping bootstrap session/new (initial_load_session_id={} set)",
                load_sid,
            ));
            // The connection stage stays neutral; the pane's own
            // "Resuming session …" indicator (driven by `loading_session`)
            // is what tells the user a conversation is being restored.
            let _ = event_tx.send(AppEvent::ConnectionStage("Connecting...".to_string()));
            (
                acp::schema::v1::SessionId::new(load_sid.to_string()),
                Vec::<AcpModelInfo>::new(),
                None,
                Vec::new(),
                false,
            )
        } else {
            let _ = event_tx.send(AppEvent::ConnectionStage("Creating session...".to_string()));
            startup_probe.log("Creating session (over pipe)");
            let mut new_session_req = acp::schema::v1::NewSessionRequest::new(cwd.clone());
            inject_wta_pane_meta(&mut new_session_req.meta, proposal_commands_supported);
            let new_session_started = std::time::Instant::now();
            let new_session_result = conn.new_session(new_session_req).await;
            log_acp_new_session_result(
                "HelperPipeStartup",
                new_session_started,
                &new_session_result,
            );
            let mut session = new_session_result.map_err(|e| {
                let failure = AgentFailure::from_acp_error(&e);
                // If we just completed post-login authenticate successfully
                // but new_session STILL returns AuthRequired, do NOT route
                // back to the login screen (that would recreate the auth
                // loop). Surface a terminal HandshakeFailed tagged with the
                // `NewSession` stage — the distinct signal the App's
                // post-login recovery policy matches via `failed_at`. This is
                // deliberately NOT the `Authenticate` stage: an authenticate
                // RPC that itself fails/times out (above) stays `Authenticate`
                // and must NOT trigger a master restart, only this
                // "authenticate-OK-but-new_session-still-auth" case should.
                // Gate on `post_login_authenticated`: if `authenticate` was
                // skipped (agent advertised no auth methods) we did not prove
                // credentials refreshed, so don't emit the "after successful
                // authenticate" signal — fall through to the normal auth
                // classification instead (the App still recovers genuine auth
                // failures via its `AuthRequired` arm, bounded to one restart).
                if post_login_reconnect && post_login_authenticated && failure.is_auth() {
                    tracing::error!(
                        target: "helper",
                        error_code = Into::<i32>::into(e.code),
                        "new_session still AuthRequired after successful authenticate — \
                         agent has a deeper auth issue; not routing back to login screen"
                    );
                    return anyhow::Error::new(AgentFailure::HandshakeFailed {
                        stage: crate::protocol::acp::failure::HandshakeStage::NewSession,
                        detail: format!(
                            "Agent still requires authentication after successful authenticate. \
                             This may indicate a Copilot subscription or organization access issue. \
                             Try restarting Intelligent Terminal or check https://github.com/settings/copilot"
                        ),
                    });
                }
                // Normal path: attach the typed classification so an auth error
                // (or any ACP code) survives the `?`-collapse into
                // `anyhow` and can be recovered by `classify_anyhow`
                // downcast at the receiver (main.rs).
                anyhow::Error::new(failure)
                    .context(format!("new_session over master pipe failed: {e}"))
            })?;
            if take_retired_session_result(&mut session.meta) {
                anyhow::bail!("bootstrap session retired during tab reset or close");
            }

            let session_id = session.session_id.clone();
            startup_probe.log(&format!("Session created (over pipe): {}", session_id));
            if is_agent_pane {
                let pane_session_id = std::env::var("WT_SESSION").unwrap_or_default();
                let pane_for_index = if pane_session_id.is_empty() {
                    None
                } else {
                    Some(pane_session_id.as_str())
                };
                tracing::info!(
                    target: "agent_pane_origin",
                    session_id = %session_id,
                    pane_session_id = %pane_session_id,
                    "recording agent-pane session origin (startup over pipe)",
                );
                crate::agent_pane_origin::append_default(session_id.0.as_ref(), pane_for_index);
            }

            let (available_models, current_model_id) =
                crate::protocol::acp::model_select::models_from_new_session(&session);
            let session_config = session
                .config_options
                .as_deref()
                .map(crate::protocol::acp::session_config::select_options)
                .unwrap_or_default();
            (
                session_id,
                available_models,
                current_model_id,
                session_config,
                true,
            )
        };

    // Apply --acp-model if requested. Only valid when we actually have
    // a bootstrap session to mutate; for the initial-load path the
    // loaded session's model is whatever the agent stored — overriding
    // it before the load completes would race the load itself.
    if has_bootstrap {
        if let Some(requested_model) = acp_model_override.filter(|s| !s.trim().is_empty()) {
            let _ = event_tx.send(AppEvent::ConnectionStage(format!(
                "Selecting model {}...",
                requested_model
            )));
            startup_probe.log(&format!(
                "Setting ACP session model to {} (over pipe)",
                requested_model
            ));
            let model_result = crate::protocol::acp::model_select::apply_session_model(
                &conn,
                session_id.clone(),
                requested_model.clone(),
            )
            .await;
            match model_result {
                Ok(config_options) => {
                    if let Some(config_options) = config_options {
                        (available_models, current_model_id) =
                            crate::protocol::acp::model_select::models_from_config_options(
                                session_id.0.as_ref(),
                                &config_options,
                            )
                            .unwrap_or_default();
                        session_config =
                            crate::protocol::acp::session_config::select_options(&config_options);
                    }
                    startup_probe.log(&format!(
                        "ACP session model set to {} (over pipe)",
                        requested_model
                    ));
                }
                Err(error) if is_redundant_startup_model_error(&prompt_usage_identity, &error) => {
                    tracing::warn!(
                        target: "helper",
                        model = %requested_model,
                        "Gemini CLI does not implement session/set_model; using the model already supplied on its launch command"
                    );
                    startup_probe.log(&format!(
                        "Gemini startup model {} already applied by launch command",
                        requested_model
                    ));
                }
                Err(error) => {
                    return Err(anyhow::anyhow!(
                        "failed to set requested model {}: {}",
                        requested_model,
                        error
                    ));
                }
            }
        }
    }

    // Notify app of connection. No raw `program/args` to summarise in
    // helper mode — pull what the master/agent advertised via `init_resp`.
    let agent_version = init_resp
        .agent_info
        .as_ref()
        .map(|info| format!("v{}", info.version));
    let agent_name = init_resp
        .agent_info
        .as_ref()
        .and_then(|info| info.title.clone().or_else(|| Some(info.name.clone())))
        .unwrap_or_else(|| "wta-master".to_string());
    let load_session_supported = init_resp.agent_capabilities.load_session;
    let image_supported = init_resp.agent_capabilities.prompt_capabilities.image;
    startup_probe.log(&format!(
        "Agent capabilities (over pipe): loadSession={} image={}",
        load_session_supported, image_supported
    ));
    let _ = event_tx.send(AppEvent::AgentConnected {
        name: agent_name,
        // We have no `--agent` cmdline to mine a model identifier
        // from; the per-session `current_model_id` covers the UI.
        model: None,
        version: agent_version,
        session_id: session_id.to_string(),
        available_models,
        current_model_id,
        load_session_supported,
        image_supported,
    });
    let _ = event_tx.send(AppEvent::SessionConfigUpdated {
        session_id: session_id.to_string(),
        options: session_config,
    });
    // Per-tab session cache. Only
    // prepopulate the owner-tab binding when we actually have a
    // bootstrap session — otherwise the `load_session_rx` arm would
    // see the placeholder sid as a prior session, try to `cancel` it,
    // and the agent CLI would reject the cancel for an unknown sid.
    // With no entry, the load arm sees `old_sid = None` and loads
    // cleanly.
    let tab_to_session: Arc<tokio::sync::Mutex<HashMap<String, acp::schema::v1::SessionId>>> =
        Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    if has_bootstrap {
        let mut g = tab_to_session.lock().await;
        let initial_tab_key = owner_tab_id.clone().unwrap_or_else(|| "0".to_string());
        g.insert(initial_tab_key, session_id.clone());
    }

    let template_memo = TemplateMemo::default();
    let in_flight_tabs: Arc<std::sync::Mutex<HashSet<String>>> =
        Arc::new(std::sync::Mutex::new(HashSet::new()));
    let cancel_signals: Arc<std::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>> =
        Arc::new(std::sync::Mutex::new(HashMap::new()));
    let mut prompt_tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    let conn = Arc::new(conn);

    // Periodic 5s tick that fans out an AppEvent::SessionsChanged to
    // force a refetch in any open session management view. Belt-and-suspenders against
    // missed `intellterm.wta/sessions/changed` broadcasts. Cheap:
    // refetch only fires for tabs whose snapshot.is_some() (i.e. session management view is
    // currently open).
    let mut periodic_refetch = tokio::time::interval(std::time::Duration::from_secs(5));
    periodic_refetch.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Burn the first tick (fires immediately on creation).
    periodic_refetch.tick().await;

    // Main event loop. The select arms are extracted into `dispatch_*`
    // free fns (so they're unit-testable). No restart-loop wrapper here:
    // helper mode can't restart the master in-process, so `/restart` asks C++
    // to replace it. The retained helper reconnects when the old pipe closes.
    loop {
        tokio::select! {
            biased;
            _ = periodic_refetch.tick() => {
                let _ = event_tx.send(AppEvent::SessionsChanged);
            }
            Some(event) = session_hook_rx.recv() => {
                let conn_for_hook = conn.clone();
                tokio::task::spawn_local(async move {
                    let req = crate::session_registry::build_session_hook_request(&event);
                    match conn_for_hook.ext_method(req).await {
                        Ok(response) => tracing::debug!(
                            target: "session_hook",
                            event = ?event,
                            response = %response.0.get(),
                            "session_hook sent to master"
                        ),
                        Err(err) => tracing::warn!(
                            target: "session_hook",
                            event = ?event,
                            error = ?err,
                            "session_hook ext-request to master failed"
                        ),
                    }
                });
            }
            Some(req) = master_ext_rx.recv() => {
                dispatch_master_ext_request(req, &conn, &event_tx, &tab_to_session);
            }
            Some(req) = restart_rx.recv() => {
                match req {
                    AgentLifecycleRequest::RestartMaster => {
                        // Helper can't restart the shared master in-process.
                        tracing::info!(
                            target: "helper",
                            "restart requested — asking WT to replace the shared master"
                        );
                        crate::wt_protocol_events::send(
                            crate::wt_protocol_events::restart_agent_stack_event(),
                        );
                    }
                    AgentLifecycleRequest::RebindAgent(request) => {
                        tracing::info!(
                            target: "helper",
                            operation_id = %request.operation_id,
                            generation = request.generation,
                            agent_id = %request.agent_id,
                            "ending helper ACP connection for Agent rebind"
                        );
                        stop_prompt_tasks(
                            &mut prompt_tasks,
                            &in_flight_tabs,
                            &cancel_signals,
                        )
                        .await;
                        intentional_shutdown.store(true, Ordering::Release);
                        conn.shutdown();
                        let _ = (&mut io_task).await;
                        return Ok(AcpClientExit::RebindAgent(request));
                    }
                }
            }
            io_result = &mut io_task => {
                let exit = complete_transport_shutdown(
                    io_result,
                    &intentional_shutdown,
                    &event_tx,
                    &mut prompt_tasks,
                    &in_flight_tabs,
                    &cancel_signals,
                )
                .await;
                startup_probe.log("run_acp_client_over_pipe transport ended");
                return Ok(exit);
            }
            Some(req) = cancel_rx.recv() => {
                dispatch_cancel(req, &conn, &cancel_signals);
            }
            Some(req) = new_session_rx.recv() => {
                dispatch_new_session(
                    req,
                    &conn,
                    &tab_to_session,
                    &template_memo,
                    &cancel_signals,
                    &event_tx,
                    is_agent_pane,
                    true,
                    "HelperPipeNewSessionForTab",
                    &proposal_channels,
                    proposal_commands_supported,
                );
            }
            Some(req) = load_session_rx.recv() => {
                dispatch_load_session(
                    req,
                    &conn,
                    &tab_to_session,
                    &cancel_signals,
                    &event_tx,
                    true,
                    true,
                    std::time::Duration::from_secs(60),
                    &proposal_channels,
                    proposal_commands_supported,
                );
            }
            Some(req) = drop_session_rx.recv() => {
                dispatch_drop_session(
                    req,
                    &conn,
                    &tab_to_session,
                    &template_memo,
                    &cancel_signals,
                ).await;
            }
            Some(req) = rename_session_rx.recv() => {
                dispatch_rename_session(req, &tab_to_session).await;
            }
            Some(prompt) = prompt_rx.recv() => {
                prompt_tasks.retain(|task| !task.is_finished());
                if let Some(task) = dispatch_prompt(
                    prompt,
                    &conn,
                    &tab_to_session,
                    &template_memo,
                    &in_flight_tabs,
                    &cancel_signals,
                    &event_tx,
                    &shell_mgr,
                    &prompt_timing,
                    &client,
                    &prompt_usage_identity,
                    wt_connected,
                    is_agent_pane,
                    proposal_commands_supported,
                    &proposal_channels,
                ) {
                    prompt_tasks.push(task);
                }
            }
            else => break,
        }
    }

    stop_prompt_tasks(&mut prompt_tasks, &in_flight_tabs, &cancel_signals).await;
    intentional_shutdown.store(true, Ordering::Release);
    conn.shutdown();
    let _ = io_task.await;
    startup_probe.log("run_acp_client_over_pipe loop ended");
    Ok(AcpClientExit::ChannelsClosed)
}

/// Spawn a per-prompt task that resolves the tab's ACP session (lazily
/// creating one if needed), instruments timing, runs `conn.prompt`, and
/// cleans up state on completion. Extracted from the old inline body in
/// the prompt while-loop so the new select-based loop body stays terse.
#[allow(clippy::too_many_arguments)]
fn dispatch_master_ext_request(
    req: MasterExtRequest,
    conn: &conn::ClientLink,
    event_tx: &mpsc::UnboundedSender<AppEvent>,
    tab_to_session: &Arc<tokio::sync::Mutex<HashMap<String, acp::schema::v1::SessionId>>>,
) {
    let conn = conn.clone();
    let event_tx = event_tx.clone();
    let tab_to_session = Arc::clone(tab_to_session);
    tokio::task::spawn_local(async move {
        match req {
            MasterExtRequest::SessionsList { request_id, rescan } => {
                let wire = crate::session_registry::build_sessions_list_request(rescan);
                // Bound the wait so a single dropped RPC response can't
                // permanently strand the tab's `refetch_in_flight=true`.
                //
                // Root cause is in agent-client-protocol@0.10's
                // `RpcConnection::handle_io`: `read_line` is *not*
                // cancellation-safe, but it's polled in a
                // `select_biased!` whose outgoing arm has priority. When
                // a concurrent outgoing message preempts an in-progress
                // `read_line`, BufReader bytes already pulled off the
                // pipe vanish; the next read starts mid-message, JSON
                // parse fails, and the pending response future for the
                // request whose response was being read never resolves.
                // From our side `conn.ext_method(...)` then awaits
                // forever.
                //
                // Without this timeout the failure mode is: helper opens
                // /sessions, fires `sessions/list`, response gets
                // truncated → `refetch_in_flight` stuck `true` → every
                // subsequent `sessions/changed` broadcast and 5s tick
                // hits `if refetch_in_flight { dirty=true; return; }`
                // and never refetches → the tab's row activity / status
                // is frozen until the user toggles /sessions off and
                // on (which calls `close_agents_view_for_tab` and
                // resets the gate).
                //
                // 8s > the 5s periodic tick so a healthy in-flight
                // request never gets cancelled spuriously; under the
                // bug the worst-case visible staleness becomes
                // ~timeout + tick ≈ 13s instead of "until next manual
                // toggle".
                //
                // The proper fix lives upstream — ACP 0.12 rewrote
                // `handle_io` into separate incoming/outgoing actors,
                // which is cancellation-safe by construction. Until we
                // upgrade, this timeout is the guardrail.
                const SESSIONS_LIST_TIMEOUT: std::time::Duration =
                    std::time::Duration::from_secs(8);
                let result =
                    tokio::time::timeout(SESSIONS_LIST_TIMEOUT, conn.ext_method(wire)).await;
                match result {
                    Ok(Ok(resp)) => {
                        let sessions =
                            crate::session_registry::parse_sessions_list_response(&resp.0)
                                .map(|r| r.sessions)
                                .unwrap_or_default();
                        let _ = event_tx.send(AppEvent::AgentsSnapshotLoaded {
                            request_id,
                            sessions,
                        });
                    }
                    Ok(Err(err)) => {
                        tracing::warn!(
                            target: "agents_view",
                            request_id,
                            error = ?err,
                            "sessions/list ext-request failed"
                        );
                        let _ = event_tx.send(AppEvent::AgentsSnapshotFailed { request_id });
                    }
                    Err(_elapsed) => {
                        tracing::warn!(
                            target: "agents_view",
                            request_id,
                            timeout_secs = SESSIONS_LIST_TIMEOUT.as_secs(),
                            "sessions/list timed out — likely ACP-0.10 \
                             cancellation-safety bug; unblocking refetch_in_flight \
                             so 5s tick can retry"
                        );
                        let _ = event_tx.send(AppEvent::AgentsSnapshotFailed { request_id });
                    }
                }
            }
            MasterExtRequest::SessionBornBound { event } => {
                const BORN_BOUND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);
                let wire = crate::session_registry::build_born_bound_request(&event);
                match tokio::time::timeout(BORN_BOUND_TIMEOUT, conn.ext_method(wire)).await {
                    Ok(Ok(response)) => tracing::debug!(
                        target: "session_hook",
                        event = ?event,
                        response = %response.0.get(),
                        "born-bound registration sent to master"
                    ),
                    Ok(Err(err)) => tracing::warn!(
                        target: "session_hook",
                        event = ?event,
                        error = ?err,
                        "born-bound registration ext-request failed"
                    ),
                    Err(_) => tracing::warn!(
                        target: "session_hook",
                        event = ?event,
                        timeout_secs = BORN_BOUND_TIMEOUT.as_secs(),
                        "born-bound registration timed out"
                    ),
                }
            }
            MasterExtRequest::SessionResumeDispatched { request_id, sid } => {
                let wire = crate::session_registry::build_session_resume_dispatched_request(&sid);
                match conn.ext_method(wire).await {
                    Ok(resp) => {
                        let _ = crate::session_registry::parse_session_resume_dispatched_response(
                            &resp.0,
                        );
                    }
                    Err(err) => {
                        tracing::warn!(target: "agents_view", request_id, session_id = %sid.0, error = ?err, "session_resume_dispatched ext-request failed");
                    }
                }
                let _ = event_tx.send(AppEvent::MasterMutationCompleted { request_id });
            }
            MasterExtRequest::SessionFocus { request_id, sid } => {
                let wire = crate::session_registry::build_session_focus_request(&sid);
                match conn.ext_method(wire).await {
                    Ok(resp) => {
                        let _ = crate::session_registry::parse_session_focus_response(&resp.0);
                    }
                    Err(err) => {
                        tracing::warn!(target: "agents_view", request_id, session_id = %sid.0, error = ?err, "session_focus ext-request failed");
                    }
                }
                let _ = event_tx.send(AppEvent::MasterMutationCompleted { request_id });
            }
            MasterExtRequest::SetSessionModel {
                session_id,
                model,
                pane_override,
            } => {
                // Apply to the targeted session, or to every live session
                // this helper owns when no target is given (normally just the
                // one bound to its owner tab). Best-effort: a failure on one
                // session is logged, not fatal — the next prompt still works
                // on the previously-selected model.
                let sessions: Vec<acp::schema::v1::SessionId> = {
                    let g = tab_to_session.lock().await;
                    match &session_id {
                        Some(target) => g.values().filter(|s| *s == target).cloned().collect(),
                        None => g.values().cloned().collect(),
                    }
                };
                // A targeted update that matches no live session is a silent
                // no-op the UI can't see — surface it so a stale session id
                // (e.g. a race with `/new`) is diagnosable instead of the UI
                // claiming the model changed when nothing happened.
                if let Some(target) = &session_id {
                    if sessions.is_empty() {
                        tracing::warn!(
                            target: "acp",
                            session_id = %target.0,
                            model = %model,
                            "set_session_model targeted an unknown/stale session; no live session updated"
                        );
                        let _ = event_tx.send(AppEvent::ModelSetFailed {
                            session_id: target.to_string(),
                            model: model.clone(),
                            pane_override,
                            message: "the session is no longer active".to_string(),
                        });
                    }
                }
                for sid in sessions {
                    match crate::protocol::acp::model_select::apply_session_model(
                        &conn,
                        sid.clone(),
                        model.clone(),
                    )
                    .await
                    {
                        Ok(config_options) => {
                            if let Some(config_options) = config_options {
                                let (available_models, current_model_id) =
                                    crate::protocol::acp::model_select::models_from_config_options(
                                        sid.0.as_ref(),
                                        &config_options,
                                    )
                                    .unwrap_or_default();
                                publish_session_config_options(
                                    &event_tx,
                                    &sid,
                                    Some(&config_options),
                                );
                                let _ = event_tx.send(AppEvent::ModelConfigUpdated {
                                    session_id: sid.to_string(),
                                    available_models,
                                    current_model_id,
                                });
                            }
                            let _ = event_tx.send(AppEvent::ModelSetCompleted {
                                session_id: sid.to_string(),
                                model: model.clone(),
                                pane_override,
                            });
                            tracing::info!(
                                target: "acp",
                                session_id = %sid.0,
                                model = %model,
                                "acp-model hot-applied to live session"
                            );
                        }
                        Err(err) => {
                            tracing::warn!(
                                target: "acp",
                                session_id = %sid.0,
                                model = %model,
                                error = ?err,
                                "model hot-update failed"
                            );
                            let _ = event_tx.send(AppEvent::ModelSetFailed {
                                session_id: sid.to_string(),
                                model: model.clone(),
                                pane_override,
                                message: err.to_string(),
                            });
                        }
                    }
                }
            }
            MasterExtRequest::SetSessionConfigOption {
                session_id,
                config_id,
                value,
            } => {
                let is_live = {
                    let sessions = tab_to_session.lock().await;
                    sessions.values().any(|known| known == &session_id)
                };
                if !is_live {
                    let _ = event_tx.send(AppEvent::SessionConfigSetFailed {
                        session_id: session_id.to_string(),
                        config_id,
                        message: "the session is no longer active".to_string(),
                    });
                    return;
                }

                let model_compat = crate::protocol::acp::model_select::is_model_config(
                    session_id.0.as_ref(),
                    &config_id,
                );
                let result = if model_compat {
                    crate::protocol::acp::model_select::apply_session_model(
                        &conn,
                        session_id.clone(),
                        value.clone(),
                    )
                    .await
                } else {
                    let request = acp::schema::v1::SetSessionConfigOptionRequest::new(
                        session_id.clone(),
                        config_id.clone(),
                        value.as_str(),
                    );
                    conn.set_session_config_option(request)
                        .await
                        .map(|response| Some(response.config_options))
                };
                match result {
                    Ok(config_options) => {
                        if let Some(config_options) = config_options {
                            let (available_models, current_model_id) =
                                crate::protocol::acp::model_select::models_from_config_options(
                                    session_id.0.as_ref(),
                                    &config_options,
                                )
                                .unwrap_or_default();
                            publish_session_config_options(
                                &event_tx,
                                &session_id,
                                Some(&config_options),
                            );
                            let _ = event_tx.send(AppEvent::ModelConfigUpdated {
                                session_id: session_id.to_string(),
                                available_models,
                                current_model_id,
                            });
                        }
                        let _ = event_tx.send(AppEvent::SessionConfigSetCompleted {
                            session_id: session_id.to_string(),
                            config_id,
                            value,
                            model_compat,
                        });
                    }
                    Err(error) => {
                        tracing::warn!(
                            target: "acp",
                            session_id = %session_id,
                            config_id,
                            value,
                            error = ?error,
                            "session config update failed"
                        );
                        let _ = event_tx.send(AppEvent::SessionConfigSetFailed {
                            session_id: session_id.to_string(),
                            config_id,
                            message: error.to_string(),
                        });
                    }
                }
            }
        }
    });
}

/// Resume a historical agent session for a tab via ACP `session/load`
/// (the session-management Enter resume path). Cancels and
/// drops any existing binding, calls `load_session` under a timeout, and
/// on success rebinds the tab and emits `SessionAttached` +
/// `TabSystemMessage`. Called by `run_acp_client_over_pipe`.
///
/// `inject_pane_meta` injects WT_SESSION into the request meta so master
/// records `pane_session_id` on the resumed row.
/// `use_load_failure_handler` selects the richer [`handle_load_failure`]
/// (restore prior binding / boot-time fallback `new_session`); when
/// `false`, a load failure instead surfaces a plain `TabError`.
/// `timeout` bounds the `session/load` call (60s in production; injectable
/// for tests).
#[allow(clippy::too_many_arguments)]
fn dispatch_load_session(
    req: LoadSessionForTab,
    conn: &conn::ClientLink,
    tab_to_session: &Arc<tokio::sync::Mutex<HashMap<String, acp::schema::v1::SessionId>>>,
    cancel_signals: &Arc<std::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>>,
    event_tx: &mpsc::UnboundedSender<AppEvent>,
    inject_pane_meta: bool,
    use_load_failure_handler: bool,
    timeout: std::time::Duration,
    proposal_channels: &Arc<crate::agent_tools::action_proposal::channel::ProposalChannelManager>,
    proposal_mcp_enabled: bool,
) {
    tracing::info!(
        target: "acp_load_session",
        tab = %req.tab_id,
        session_id = %req.session_id,
        inject_pane_meta,
        use_load_failure_handler,
        timeout_ms = timeout.as_millis() as u64,
        "load_session requested"
    );
    let conn = conn.clone();
    let tab_to_session = Arc::clone(tab_to_session);
    let cancel_signals = Arc::clone(cancel_signals);
    let event_tx = event_tx.clone();
    let proposal_channels = Arc::clone(proposal_channels);
    tokio::task::spawn_local(async move {
        let cwd = req
            .cwd
            .clone()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

        // If the target tab already holds a session, cancel any in-flight
        // prompt for it and drop the binding — we're about to replace it
        // with the loaded one. Mirrors the new_session prelude.
        let old_sid: Option<acp::schema::v1::SessionId> = {
            let mut g = tab_to_session.lock().await;
            g.remove(&req.tab_id)
        };

        if let Some(ref old) = old_sid {
            let old_str = old.to_string();
            if let Some(sig) = cancel_signals.lock().unwrap().remove(&old_str) {
                let _ = sig.send(());
            }
            if let Err(e) = conn
                .cancel(acp::schema::v1::CancelNotification::new(old.clone()))
                .await
            {
                tracing::warn!(
                    target: "acp_load_session",
                    tab = %req.tab_id,
                    session_id = %old,
                    error = ?e,
                    "session/cancel before load failed (likely unsupported)"
                );
            }
        }

        let session_id = acp::schema::v1::SessionId::new(req.session_id.clone());
        let mut load_req =
            acp::schema::v1::LoadSessionRequest::new(session_id.clone(), cwd.clone());
        // Tell master which WT pane owns the session we're about to
        // rehydrate, so the registry row for the resumed sid carries
        // `pane_session_id = <this pane's GUID>` and cross-helper Focus
        // actions can resolve to a real WT pane. Only the helper path
        // needs this.
        if inject_pane_meta {
            inject_wta_pane_meta(&mut load_req.meta, proposal_mcp_enabled);
        }
        // `session/load` may replay history before returning, so on large
        // session stores the call can take a while; the timeout ceiling
        // keeps us from hanging forever if the agent never responds.
        let load_result = tokio::time::timeout(timeout, conn.load_session(load_req)).await;

        match load_result {
            Ok(Ok(mut resp)) => {
                if take_retired_session_result(&mut resp.meta) {
                    tracing::info!(
                        target: "acp_load_session",
                        tab = %req.tab_id,
                        session_id = %req.session_id,
                        "ignoring session/load result retired during tab reset or close"
                    );
                    return;
                }
                tracing::info!(
                    target: "acp_load_session",
                    tab = %req.tab_id,
                    session_id = %req.session_id,
                    "load_session succeeded"
                );
                {
                    let mut g = tab_to_session.lock().await;
                    g.insert(req.tab_id.clone(), session_id.clone());
                }
                if let Some(old) = old_sid
                    .as_ref()
                    .filter(|old| old.0.as_ref() != session_id.0.as_ref())
                {
                    crate::protocol::acp::model_select::forget_session(old.0.as_ref());
                }
                // The agent replays past content via session/update
                // notifications that route through the existing
                // session_to_tab map. SessionAttached primes that mapping.
                let (available_models, current_model_id) =
                    crate::protocol::acp::model_select::models_from_load_session(
                        session_id.0.as_ref(),
                        &resp,
                    );
                // No "Session loaded" note is added to the transcript: the
                // restored conversation speaks for itself, and the in-pane
                // resuming indicator ends when this event clears
                // `loading_session`.
                let _ = event_tx.send(AppEvent::SessionAttached {
                    tab_id: req.tab_id.clone(),
                    session_id: session_id.to_string(),
                    available_models,
                    current_model_id,
                });
                publish_session_config_options(
                    &event_tx,
                    &session_id,
                    resp.config_options.as_deref(),
                );
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    target: "acp_load_session",
                    tab = %req.tab_id,
                    session_id = %req.session_id,
                    error = ?e,
                    "load_session failed"
                );
                let message = format!(
                    "Failed to resume session in agent pane: {}. \
                     The connected agent may not recognize this \
                     session id (CLI mismatch), or `session/load` \
                     is unsupported.",
                    e
                );
                dispatch_load_failure(
                    use_load_failure_handler,
                    old_sid.as_ref(),
                    &req.tab_id,
                    &cwd,
                    &conn,
                    &tab_to_session,
                    &event_tx,
                    message,
                    &proposal_channels,
                    proposal_mcp_enabled,
                )
                .await;
            }
            Err(_) => {
                tracing::warn!(
                    target: "acp_load_session",
                    tab = %req.tab_id,
                    session_id = %req.session_id,
                    "load_session timed out"
                );
                let human_timeout = if timeout.as_secs() >= 1 {
                    format!("{}s", timeout.as_secs())
                } else {
                    format!("{}ms", timeout.as_millis())
                };
                let message = format!(
                    "Resume timed out after {human_timeout} — the agent \
                     did not respond to `session/load`."
                );
                dispatch_load_failure(
                    use_load_failure_handler,
                    old_sid.as_ref(),
                    &req.tab_id,
                    &cwd,
                    &conn,
                    &tab_to_session,
                    &event_tx,
                    message,
                    &proposal_channels,
                    proposal_mcp_enabled,
                )
                .await;
            }
        }
    });
}

/// Failure-strategy switch for [`dispatch_load_session`]: the helper path
/// uses the richer [`handle_load_failure`] (restore prior binding /
/// boot-time fallback `new_session`); the direct path surfaces a plain
/// `TabError` routed to the specific tab.
#[allow(clippy::too_many_arguments)]
async fn dispatch_load_failure(
    use_load_failure_handler: bool,
    old_sid: Option<&acp::schema::v1::SessionId>,
    tab_id: &str,
    cwd: &std::path::Path,
    conn: &conn::ClientLink,
    tab_to_session: &Arc<tokio::sync::Mutex<HashMap<String, acp::schema::v1::SessionId>>>,
    event_tx: &mpsc::UnboundedSender<AppEvent>,
    message: String,
    proposal_channels: &Arc<crate::agent_tools::action_proposal::channel::ProposalChannelManager>,
    proposal_mcp_enabled: bool,
) {
    if use_load_failure_handler {
        handle_load_failure(
            old_sid,
            tab_id.to_string(),
            cwd.to_path_buf(),
            conn.clone(),
            Arc::clone(tab_to_session),
            event_tx.clone(),
            message,
            Arc::clone(proposal_channels),
            proposal_mcp_enabled,
        )
        .await;
    } else {
        // TabError routes to the specific new tab (the historical session
        // has no live session_id we could thread through AgentError, and
        // AgentError with session_id=None would land in the currently-
        // active tab instead).
        let _ = event_tx.send(AppEvent::TabError {
            tab_id: tab_id.to_string(),
            message,
        });
    }
}

/// Spin up a fresh ACP session for a tab (the `/new` path), atomically
/// replacing any existing session. Cancels and forgets the old session,
/// calls `new_session`, records the agent-pane origin, rebinds the tab,
/// and emits `SessionAttached` (or `AgentError` on failure). Called by
/// `run_acp_client_over_pipe`.
///
/// `inject_pane_meta` controls whether WT_SESSION is injected into the
/// request meta — the helper pipe path needs it so master can record
/// `pane_session_id` on the registry row; the direct-agent path does not.
/// `log_label` distinguishes the two paths in the timing log.
#[allow(clippy::too_many_arguments)]
fn dispatch_new_session(
    req: NewSessionForTab,
    conn: &conn::ClientLink,
    tab_to_session: &Arc<tokio::sync::Mutex<HashMap<String, acp::schema::v1::SessionId>>>,
    template_memo: &TemplateMemo,
    cancel_signals: &Arc<std::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>>,
    event_tx: &mpsc::UnboundedSender<AppEvent>,
    is_agent_pane: bool,
    inject_pane_meta: bool,
    log_label: &'static str,
    _proposal_channels: &Arc<crate::agent_tools::action_proposal::channel::ProposalChannelManager>,
    proposal_mcp_enabled: bool,
) {
    tracing::info!(
        target: "acp_new_session",
        tab = %req.tab_id,
        "new_session requested"
    );
    let conn = conn.clone();
    let tab_to_session = Arc::clone(tab_to_session);
    let template_memo = template_memo.clone();
    let cancel_signals = Arc::clone(cancel_signals);
    let event_tx = event_tx.clone();
    tokio::task::spawn_local(async move {
        let cwd = req
            .cwd
            .clone()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

        let old_sid: Option<acp::schema::v1::SessionId> = {
            let mut g = tab_to_session.lock().await;
            g.remove(&req.tab_id)
        };

        if let Some(ref old) = old_sid {
            let old_str = old.to_string();
            crate::protocol::acp::model_select::forget_session(&old_str);
            template_memo.forget(&old_str).await;
            if let Some(sig) = cancel_signals.lock().unwrap().remove(&old_str) {
                let _ = sig.send(());
            }
            let _ = conn
                .cancel(acp::schema::v1::CancelNotification::new(old.clone()))
                .await;
        }

        // Inject WT_SESSION into the request meta so master can record
        // pane_session_id on the registry row. Without this, focus_session
        // RPCs against the new sid return {"focused": false, "reason":
        // "no_pane"} because master has the row but no pane GUID to feed
        // wtcli focus-pane. Only the helper pipe path needs this.
        let mut new_session_req = acp::schema::v1::NewSessionRequest::new(cwd);
        if inject_pane_meta {
            inject_wta_pane_meta(&mut new_session_req.meta, proposal_mcp_enabled);
        }
        let new_session_started = std::time::Instant::now();
        let new_session_result = conn.new_session(new_session_req).await;
        log_acp_new_session_result(log_label, new_session_started, &new_session_result);
        let mut new_session = match new_session_result {
            Ok(s) => s,
            Err(e) => {
                let _ = event_tx.send(AppEvent::AgentError {
                    session_id: None,
                    failure: AgentFailure::from_acp_error(&e),
                    message: format!("/new failed for tab {}: {}", req.tab_id, e),
                });
                return;
            }
        };
        if take_retired_session_result(&mut new_session.meta) {
            tracing::info!(
                target: "acp_new_session",
                tab = %req.tab_id,
                session_id = %new_session.session_id,
                "ignoring session/new result retired during tab reset or close"
            );
            return;
        }

        let new_sid = new_session.session_id.clone();
        if is_agent_pane {
            let pane_session_id = std::env::var("WT_SESSION").unwrap_or_default();
            let pane_for_index = if pane_session_id.is_empty() {
                None
            } else {
                Some(pane_session_id.as_str())
            };
            tracing::info!(
                target: "agent_pane_origin",
                session_id = %new_sid,
                pane_session_id = %pane_session_id,
                "recording agent-pane session origin (new_session_for_tab)",
            );
            crate::agent_pane_origin::append_default(new_sid.0.as_ref(), pane_for_index);
        }
        let (available_models, current_model_id) =
            crate::protocol::acp::model_select::models_from_new_session(&new_session);
        {
            let mut g = tab_to_session.lock().await;
            g.insert(req.tab_id.clone(), new_sid.clone());
        }

        let _ = event_tx.send(AppEvent::SessionAttached {
            tab_id: req.tab_id.clone(),
            session_id: new_sid.to_string(),
            available_models,
            current_model_id,
        });
        publish_session_config_options(&event_tx, &new_sid, new_session.config_options.as_deref());
    });
}

/// Close a tab's ACP session without creating a replacement (tab close or
/// Ctrl+C×2 close-pane path). Signals any in-flight prompt to bail out of
/// `conn.prompt().await`, forgets its template memo, and asks master to
/// physically release the session through master's close-by-tab extension.
/// No-op when the tab holds no session. Called by
/// `run_acp_client_over_pipe`.
async fn dispatch_drop_session(
    req: DropSessionRequest,
    conn: &conn::ClientLink,
    tab_to_session: &Arc<tokio::sync::Mutex<HashMap<String, acp::schema::v1::SessionId>>>,
    template_memo: &TemplateMemo,
    cancel_signals: &Arc<std::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>>,
) {
    tracing::info!(
        target: "acp_drop_session",
        tab = %req.tab_id,
        "close session requested (no replacement)"
    );
    let old_sid = {
        let mut sessions = tab_to_session.lock().await;
        sessions.remove(&req.tab_id)
    };
    if let Some(old) = old_sid {
        let old_str = old.to_string();
        crate::protocol::acp::model_select::forget_session(&old_str);
        template_memo.forget(&old_str).await;
        if let Some(sig) = cancel_signals.lock().unwrap().remove(&old_str) {
            let _ = sig.send(());
        }
    }

    if !req.notify_master {
        return;
    }

    // The helper that owns the closing tab may be destroyed before it can
    // process this event. Every surviving helper therefore asks master to
    // resolve the stable tab id against its authoritative routing metadata.
    // Duplicate requests are intentionally idempotent. Keep the bounded
    // master RPC off the helper's main receive loop so sibling work remains
    // responsive while the agent unwinds the cancelled turn.
    let close_tab_request = crate::session_registry::build_close_tab_session_request(&req.tab_id);
    let conn = conn.clone();
    let tab_id = req.tab_id;
    tokio::task::spawn_local(async move {
        match tokio::time::timeout(
            std::time::Duration::from_secs(18),
            conn.ext_method(close_tab_request),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => tracing::warn!(
                target: "acp_drop_session",
                tab = %tab_id,
                error = ?error,
                "master close-by-tab request failed"
            ),
            Err(_) => tracing::warn!(
                target: "acp_drop_session",
                tab = %tab_id,
                "master close-by-tab request timed out"
            ),
        }
    });
}

/// Fire the local per-session cancel oneshot (the critical path that
/// breaks a spawned prompt task out of `conn.prompt().await`) and
/// best-effort notify the agent via `session/cancel`. Called by
/// `run_acp_client_over_pipe`.
fn dispatch_cancel(
    req: CancelRequest,
    conn: &conn::ClientLink,
    cancel_signals: &Arc<std::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>>,
) {
    let session_id_str = req.session_id.clone();
    tracing::info!(target: "acp_cancel", session_id = %session_id_str, "cancel requested");
    // Local oneshot first — it's the critical path for breaking the
    // spawned prompt task out of conn.prompt().
    if let Some(sig) = cancel_signals.lock().unwrap().remove(&session_id_str) {
        let _ = sig.send(());
    }
    // Best-effort agent notification. Spawned so the loop stays
    // responsive even if the agent is slow to ack.
    let conn_for_cancel = conn.clone();
    tokio::task::spawn_local(async move {
        let session_id = acp::schema::v1::SessionId::new(session_id_str.clone());
        if let Err(e) = conn_for_cancel
            .cancel(acp::schema::v1::CancelNotification::new(session_id))
            .await
        {
            tracing::warn!(target: "acp_cancel", session_id = %session_id_str, error = ?e, "session/cancel rpc failed (likely unsupported)");
        }
    });
}

/// Rekey the `tab_to_session` binding when WT mints a new stable tab id
/// for an existing tab (cross-window tab drag). Extracted from the
/// `rename_session_rx` arm of `run_acp_client_over_pipe`, so the rekey
/// can be unit-tested against
/// the shared map. No-op when `old_tab_id` is absent.
async fn dispatch_rename_session(
    req: RenameSessionRequest,
    tab_to_session: &Arc<tokio::sync::Mutex<HashMap<String, acp::schema::v1::SessionId>>>,
) {
    rename_helper_owner_tab_id(&req.old_tab_id, &req.new_tab_id);
    let mut g = tab_to_session.lock().await;
    let old_existed = if let Some(sid) = g.remove(&req.old_tab_id) {
        g.insert(req.new_tab_id.clone(), sid);
        true
    } else {
        false
    };
    tracing::info!(
        target: "acp_rename_session",
        old_tab_id = %req.old_tab_id,
        new_tab_id = %req.new_tab_id,
        old_existed,
        "tab_to_session rekeyed via drag"
    );
}

/// Assemble the ACP prompt content: the (already-templated) text block,
/// followed by one `ContentBlock::Image` per pasted (Alt+V) image. Extracted
/// so the text→Image ordering and base64/mime mapping are unit-testable
/// without standing up a full ACP session.
fn build_prompt_content(
    text: &str,
    images: &[crate::clipboard_image::PastedImage],
) -> Vec<acp::schema::v1::ContentBlock> {
    let mut content: Vec<acp::schema::v1::ContentBlock> = vec![text.to_string().into()];
    for image in images {
        content.push(acp::schema::v1::ContentBlock::Image(
            acp::schema::v1::ImageContent::new(image.data_base64.clone(), image.mime_type.clone()),
        ));
    }
    content
}

async fn stop_prompt_tasks(
    prompt_tasks: &mut Vec<tokio::task::JoinHandle<()>>,
    in_flight_tabs: &Arc<std::sync::Mutex<HashSet<String>>>,
    cancel_signals: &Arc<std::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>>,
) {
    let signals = std::mem::take(&mut *cancel_signals.lock().unwrap());
    for (_, signal) in signals {
        let _ = signal.send(());
    }
    for task in prompt_tasks.drain(..) {
        task.abort();
        let _ = task.await;
    }
    in_flight_tabs.lock().unwrap().clear();
    cancel_signals.lock().unwrap().clear();
}

async fn complete_transport_shutdown(
    io_result: std::result::Result<(), tokio::task::JoinError>,
    suppress_transport_error: &AtomicBool,
    event_tx: &mpsc::UnboundedSender<AppEvent>,
    prompt_tasks: &mut Vec<tokio::task::JoinHandle<()>>,
    in_flight_tabs: &Arc<std::sync::Mutex<HashSet<String>>>,
    cancel_signals: &Arc<std::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>>,
) -> AcpClientExit {
    stop_prompt_tasks(prompt_tasks, in_flight_tabs, cancel_signals).await;
    complete_transport_io_task(io_result, suppress_transport_error, event_tx)
}

fn dispatch_prompt(
    prompt: PromptSubmission,
    conn: &conn::ClientLink,
    tab_to_session: &Arc<tokio::sync::Mutex<HashMap<String, acp::schema::v1::SessionId>>>,
    template_memo: &TemplateMemo,
    in_flight_tabs: &Arc<std::sync::Mutex<HashSet<String>>>,
    cancel_signals: &Arc<std::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>>,
    event_tx: &mpsc::UnboundedSender<AppEvent>,
    shell_mgr: &Arc<ShellManager>,
    prompt_timing: &Arc<PromptTimingState>,
    client: &WtaClient,
    prompt_usage_identity: &PromptUsageIdentity,
    wt_connected: bool,
    is_agent_pane: bool,
    proposal_commands_supported: bool,
    proposal_channels: &Arc<crate::agent_tools::action_proposal::channel::ProposalChannelManager>,
) -> Option<tokio::task::JoinHandle<()>> {
    let tab_key = prompt
        .pane_context
        .as_ref()
        .and_then(|c| c.tab_id.clone())
        .unwrap_or_else(|| "0".to_string());

    {
        let mut g = in_flight_tabs.lock().unwrap();
        if !g.insert(tab_key.clone()) {
            let _ = event_tx.send(AppEvent::AgentBusy {
                tab_id: tab_key.clone(),
            });
            return None;
        }
    }

    let conn_task = conn.clone();
    let tab_to_session_task = Arc::clone(tab_to_session);
    let template_memo_task = template_memo.clone();
    let in_flight_tabs_task = Arc::clone(in_flight_tabs);
    let cancel_signals_task = Arc::clone(cancel_signals);
    let event_tx_task = event_tx.clone();
    let shell_mgr_task = Arc::clone(shell_mgr);
    let prompt_timing_task = Arc::clone(prompt_timing);
    let client_task = client.clone();
    let prompt_usage_identity_task = prompt_usage_identity.clone();
    let proposal_channels_task = Arc::clone(proposal_channels);
    let tab_key_task = tab_key.clone();

    Some(tokio::task::spawn_local(dispatch_prompt_body(
        prompt,
        conn_task,
        tab_to_session_task,
        template_memo_task,
        in_flight_tabs_task,
        cancel_signals_task,
        event_tx_task,
        shell_mgr_task,
        prompt_timing_task,
        client_task,
        prompt_usage_identity_task,
        tab_key_task,
        wt_connected,
        is_agent_pane,
        proposal_commands_supported,
        proposal_channels_task,
    )))
}

/// The per-prompt task body: lazily resolves the tab's ACP session,
/// streams the prompt, listens for cancel, and cleans up. Spawned by
/// [`dispatch_prompt`] and never called directly from the event loop.
#[allow(clippy::too_many_arguments)]
async fn dispatch_prompt_body(
    prompt: PromptSubmission,
    conn_task: conn::ClientLink,
    tab_to_session_task: Arc<tokio::sync::Mutex<HashMap<String, acp::schema::v1::SessionId>>>,
    template_memo: TemplateMemo,
    in_flight_tabs_task: Arc<std::sync::Mutex<HashSet<String>>>,
    cancel_signals_task: Arc<std::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>>,
    event_tx_task: mpsc::UnboundedSender<AppEvent>,
    shell_mgr_task: Arc<ShellManager>,
    prompt_timing_task: Arc<PromptTimingState>,
    client_task: WtaClient,
    prompt_usage_identity_task: PromptUsageIdentity,
    tab_key_task: String,
    wt_connected: bool,
    is_agent_pane: bool,
    proposal_commands_supported: bool,
    proposal_channels: Arc<crate::agent_tools::action_proposal::channel::ProposalChannelManager>,
) {
    // Resolve (or lazily create) the ACP session for this tab.
    let prompt_session_id = {
        let mut g = tab_to_session_task.lock().await;
        if let Some(sid) = g.get(&tab_key_task) {
            sid.clone()
        } else {
            let cwd = prompt
                .pane_context
                .as_ref()
                .and_then(|c| c.cwd.clone())
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let new_session_started = std::time::Instant::now();
            let mut new_session_req = acp::schema::v1::NewSessionRequest::new(cwd);
            inject_wta_pane_meta(&mut new_session_req.meta, proposal_commands_supported);
            let new_session_result = conn_task.new_session(new_session_req).await;
            log_acp_new_session_result(
                "LazyCreateOnFirstPrompt",
                new_session_started,
                &new_session_result,
            );
            let mut new_session = match new_session_result {
                Ok(s) => s,
                Err(e) => {
                    let _ = event_tx_task.send(AppEvent::AgentError {
                        session_id: None,
                        failure: AgentFailure::from_acp_error(&e),
                        message: format!("new_session failed for tab {}: {}", tab_key_task, e),
                    });
                    in_flight_tabs_task.lock().unwrap().remove(&tab_key_task);
                    return;
                }
            };
            if take_retired_session_result(&mut new_session.meta) {
                tracing::info!(
                    target: "acp_new_session",
                    tab = %tab_key_task,
                    session_id = %new_session.session_id,
                    "abandoning prompt because its lazy session was retired during tab reset or close"
                );
                in_flight_tabs_task.lock().unwrap().remove(&tab_key_task);
                return;
            }
            let new_sid = new_session.session_id.clone();
            if is_agent_pane {
                let pane_session_id = std::env::var("WT_SESSION").unwrap_or_default();
                let pane_for_index = if pane_session_id.is_empty() {
                    None
                } else {
                    Some(pane_session_id.as_str())
                };
                tracing::info!(
                    target: "agent_pane_origin",
                    session_id = %new_sid,
                    pane_session_id = %pane_session_id,
                    "recording agent-pane session origin (lazy_create_on_first_prompt)",
                );
                crate::agent_pane_origin::append_default(new_sid.0.as_ref(), pane_for_index);
            }
            let (available_models, current_model_id) =
                crate::protocol::acp::model_select::models_from_new_session(&new_session);
            let _ = event_tx_task.send(AppEvent::SessionAttached {
                tab_id: tab_key_task.clone(),
                session_id: new_sid.to_string(),
                available_models,
                current_model_id,
            });
            publish_session_config_options(
                &event_tx_task,
                &new_sid,
                new_session.config_options.as_deref(),
            );
            g.insert(tab_key_task.clone(), new_sid.clone());
            new_sid
        }
    };
    let prompt_session_id_str = prompt_session_id.to_string();

    let kind = if prompt.is_autofix() {
        TemplateKind::Autofix
    } else {
        TemplateKind::Planner
    };
    let include_base_prompt = if prompt.is_agent_command() {
        false
    } else {
        template_memo.should_ship_base(&prompt_session_id_str).await
    };

    prompt_timing_task.activate(
        &prompt_session_id_str,
        prompt.id,
        &prompt.text,
        prompt.submitted_at_unix_s,
        prompt.is_byok(),
        prompt.agent_id(),
    );
    let (text, prompt_source, resolved_target_pane) = if prompt.is_agent_command() {
        (prompt.text.clone(), "agent_command".to_string(), None)
    } else {
        let (text, source, name, target) = build_prompt_text(
            prompt.id,
            prompt.submitted_at_unix_s,
            &prompt.text,
            prompt.autofix_text_kind,
            include_base_prompt,
            &shell_mgr_task,
            wt_connected,
            prompt.pane_context.as_ref(),
        )
        .await;
        let _ = event_tx_task.send(AppEvent::PromptTemplateLoaded { name });
        (text, source, target)
    };
    if proposal_commands_supported {
        match proposal_channels.issue(
            prompt_session_id_str.clone(),
            prompt.id,
            resolved_target_pane.clone(),
            prompt.is_autofix(),
        ) {
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    target: "proposal_channel",
                    status = ?error.status,
                    reason = error.reason,
                    "failed to issue proposal channel for prompt"
                );
            }
        }
    }
    // Bind the pane used to build this prompt to the matching turn. The host
    // uses this authoritative value instead of a model-generated action target.
    if let Some(pane_id) = resolved_target_pane {
        let _ = event_tx_task.send(AppEvent::PromptTargetResolved {
            tab_id: prompt.pane_context.as_ref().and_then(|c| c.tab_id.clone()),
            prompt_id: prompt.id,
            pane_id,
        });
    }
    prompt_timing_task.mark_context_ready(&prompt_session_id_str, text.len());
    acp_log_built_prompt(
        &prompt.text,
        prompt.pane_context.as_ref(),
        &prompt_source,
        &text,
    );
    if prompt.is_agent_command() {
        tracing::info!(
            target: "acp",
            prompt_id = prompt.id,
            session_id = %prompt_session_id_str,
            prompt_len = text.len(),
            "sending Agent command verbatim"
        );
    } else {
        log_turn_trace(
            prompt.id,
            &prompt_session_id_str,
            kind,
            include_base_prompt,
            &text,
        );
    }
    prompt_timing_task.mark_prompt_sent(&prompt_session_id_str);

    // Telemetry: prompt dispatched over ACP. WTA emits `AgentPromptSent`
    // for the agent-pane prompt-entry route; the C++ side emits
    // `CommandPaletteDispatchedAgentPrompt` for the `?<prompt>` delegation
    // route under the same provider.
    crate::telemetry::log_agent_prompt_sent(
        &prompt_session_id_str,
        u32::try_from(text.len()).unwrap_or(u32::MAX),
        prompt.is_autofix(),
        match kind {
            TemplateKind::Autofix => "Autofix",
            TemplateKind::Planner if prompt.is_agent_command() => "AgentCommand",
            TemplateKind::Planner => "Planner",
        },
        prompt.is_byok(),
        prompt.agent_id(),
    );

    // Register a cancel oneshot for this prompt. The cancel
    // listener picks the sender out by session_id and signals it
    // when the user presses Ctrl+C.
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
    cancel_signals_task
        .lock()
        .unwrap()
        .insert(prompt_session_id_str.clone(), cancel_tx);

    // Build the prompt content: the (templated) text block, followed by any
    // images pasted via Alt+V as ACP `ContentBlock::Image` blocks. Images ride
    // through master → agent CLI verbatim; the agent only receives them if it
    // advertised `promptCapabilities.image` (the UI gates Alt+V on that flag).
    let content = build_prompt_content(&text, &prompt.images);
    let prompt_fut = conn_task.prompt(acp::schema::v1::PromptRequest::new(
        prompt_session_id.clone(),
        content,
    ));
    tokio::pin!(prompt_fut);

    let completed_successfully = tokio::select! {
        result = &mut prompt_fut => {
            // Peek the successful turn's stop_reason (the response is consumed
            // by `complete_prompt_request`). A soft stop is not an error; the
            // Err arm is classified separately by `from_acp_error`.
            let soft_stop = result
                .as_ref()
                .ok()
                .and_then(|resp| SoftStopReason::from_stop_reason(resp.stop_reason));
            let successful = result.is_ok();
            complete_prompt_request(
                result,
                soft_stop,
                &prompt_timing_task,
                &event_tx_task,
                prompt_session_id_str.clone(),
            )
            .await;
            successful
        }
        _ = cancel_rx => {
            // The user cancelled. Synthesize an AgentMessageEnd
            // so the App's session_tab cleanup runs even if the
            // agent never resolves the prompt future.
            tracing::info!(target: "acp_cancel", session_id = %prompt_session_id_str, "prompt task aborted by cancel");
            let _ = prompt_timing_task.complete(
                &prompt_session_id_str,
                false,
                Some("cancelled"),
            );
            let _ = event_tx_task.send(AppEvent::AgentMessageEnd {
                session_id: prompt_session_id_str.clone(),
            });
            false
        }
    };
    // Drop the in-flight prompt future eagerly when cancelled to
    // release the connection slot for the next prompt on this tab.
    drop(prompt_fut);

    if completed_successfully {
        match probe_private_usage(
            &conn_task,
            &client_task,
            &prompt_usage_identity_task,
            prompt_session_id.clone(),
        )
        .await
        {
            Ok(Some(snapshot)) => {
                let _ = event_tx_task.send(AppEvent::UsageReported {
                    session_id: prompt_session_id_str.clone(),
                    snapshot,
                });
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    target: "usage",
                    session_id = %prompt_session_id_str,
                    error = %error,
                    "optional provider usage probe failed"
                );
            }
        }
    }

    cancel_signals_task
        .lock()
        .unwrap()
        .remove(&prompt_session_id_str);
    in_flight_tabs_task.lock().unwrap().remove(&tab_key_task);
}

#[cfg(test)]
mod tests {
    use super::acp;
    use super::{
        acp_error_detail, acp_result_failure_fields, bounded_tool_output_parts,
        claim_unexpected_transport_loss, complete_prompt_request, complete_transport_shutdown,
        inject_wta_pane_meta, is_redundant_startup_model_error, post_login_authenticate_error,
        session_mcp_tool_from_title, stop_prompt_tasks, timeout_result_failure_fields,
        tool_call_exit_code, tool_call_kind_label, tool_call_location_hint, tool_call_target,
        AcpClientExit, ClientState, PromptTimingState, PromptUsageIdentity, SessionMcpTool,
        SoftStopReason, WtaClient,
    };
    use crate::app_contracts::AppEvent;
    use crate::protocol::acp::failure::{AgentFailure, HandshakeStage};
    use crate::shell::ShellManager;
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;

    #[test]
    fn fetch_target_strips_credentials_query_and_fragment() {
        let raw_input = serde_json::json!({
            "url": "https://user:secret@example.com/api/items?token=secret#result"
        });

        assert_eq!(
            tool_call_target(
                Some(&acp::schema::v1::ToolKind::Fetch),
                &[],
                Some(&raw_input)
            ),
            Some(("https://example.com/api/items".to_string(), false))
        );
        assert_eq!(
            tool_call_target(None, &[], Some(&raw_input)),
            Some(("https://example.com/api/items".to_string(), false))
        );
        assert_eq!(
            tool_call_location_hint(
                "Fetching https://user:secret@example.com/api/items?token=secret#result",
                Some(&acp::schema::v1::ToolKind::Fetch),
                &[],
                Some(&raw_input),
            ),
            Some(("https://example.com/api/items".to_string(), false))
        );
    }

    #[test]
    fn unexpected_transport_loss_is_claimed_exactly_once() {
        let claimed = std::sync::atomic::AtomicBool::new(false);

        assert!(claim_unexpected_transport_loss(true, &claimed));
        assert!(!claim_unexpected_transport_loss(true, &claimed));
    }

    #[test]
    fn direct_setup_failure_suppresses_guard_induced_transport_loss() {
        let claimed = std::sync::atomic::AtomicBool::new(false);

        assert!(!claim_unexpected_transport_loss(false, &claimed));
        assert!(!claim_unexpected_transport_loss(true, &claimed));
    }

    #[tokio::test]
    async fn transport_completion_exits_despite_perpetual_refetch_and_cleans_prompts() {
        struct DropFlag(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::Release);
            }
        }

        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let dropped_for_task = Arc::clone(&dropped);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let mut prompt_tasks = vec![tokio::spawn(async move {
            let _flag = DropFlag(dropped_for_task);
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
        })];
        started_rx.await.expect("prompt task started");

        let in_flight_tabs = Arc::new(Mutex::new(HashSet::from(["tab-1".to_string()])));
        let (cancel_tx, _cancel_rx) = tokio::sync::oneshot::channel();
        let cancel_signals = Arc::new(Mutex::new(HashMap::from([(
            "session-1".to_string(),
            cancel_tx,
        )])));
        let intentional_shutdown = std::sync::atomic::AtomicBool::new(false);
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let mut io_task = tokio::spawn(async {});

        let io_result = tokio::select! {
            result = &mut io_task => result,
            _ = std::future::pending::<()>() => unreachable!("perpetual refetch cannot win"),
        };
        let exit = complete_transport_shutdown(
            io_result,
            &intentional_shutdown,
            &event_tx,
            &mut prompt_tasks,
            &in_flight_tabs,
            &cancel_signals,
        )
        .await;

        assert_eq!(exit, AcpClientExit::ChannelsClosed);
        assert!(matches!(
            event_rx.try_recv(),
            Ok(AppEvent::MasterDisconnected)
        ));
        assert!(event_rx.try_recv().is_err(), "disconnect must be sent once");
        assert!(prompt_tasks.is_empty());
        assert!(in_flight_tabs.lock().unwrap().is_empty());
        assert!(cancel_signals.lock().unwrap().is_empty());
        assert!(dropped.load(std::sync::atomic::Ordering::Acquire));
    }

    #[tokio::test]
    async fn intentional_transport_completion_exits_without_disconnect_notification() {
        let intentional_shutdown = std::sync::atomic::AtomicBool::new(true);
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let mut prompt_tasks = Vec::new();
        let in_flight_tabs = Arc::new(Mutex::new(HashSet::new()));
        let cancel_signals = Arc::new(Mutex::new(HashMap::new()));
        let io_result = tokio::spawn(async {}).await;

        let exit = complete_transport_shutdown(
            io_result,
            &intentional_shutdown,
            &event_tx,
            &mut prompt_tasks,
            &in_flight_tabs,
            &cancel_signals,
        )
        .await;

        assert_eq!(exit, AcpClientExit::ChannelsClosed);
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn stop_prompt_tasks_aborts_pre_prompt_work_and_clears_tracking() {
        struct DropFlag(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::Release);
            }
        }

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        tokio::task::LocalSet::new().block_on(&runtime, async {
            let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let dropped_for_task = Arc::clone(&dropped);
            let mut tasks = vec![tokio::task::spawn_local(async move {
                let _flag = DropFlag(dropped_for_task);
                std::future::pending::<()>().await;
            })];
            tokio::task::yield_now().await;

            let in_flight_tabs = Arc::new(Mutex::new(HashSet::from(["tab-1".to_string()])));
            let (cancel_tx, _cancel_rx) = tokio::sync::oneshot::channel();
            let cancel_signals = Arc::new(Mutex::new(HashMap::from([(
                "session-1".to_string(),
                cancel_tx,
            )])));

            stop_prompt_tasks(&mut tasks, &in_flight_tabs, &cancel_signals).await;

            assert!(tasks.is_empty());
            assert!(in_flight_tabs.lock().unwrap().is_empty());
            assert!(cancel_signals.lock().unwrap().is_empty());
            assert!(dropped.load(std::sync::atomic::Ordering::Acquire));
        });
    }

    #[test]
    fn acp_error_detail_prefers_actionable_data() {
        let error = acp::Error::internal_error()
            .data("The saved API key was not found in Windows Credential Manager.");

        assert_eq!(
            acp_error_detail(&error),
            "The saved API key was not found in Windows Credential Manager."
        );
    }

    #[test]
    fn bounded_tool_output_parts_keeps_unicode_tail_without_joining_full_input() {
        let prefix = "界".repeat(4000);
        let output = bounded_tool_output_parts([prefix.as_str(), "TAIL"].into_iter())
            .expect("expected bounded output");

        assert!(output.truncated);
        assert_eq!(output.text.chars().count(), 4000);
        assert!(output.text.ends_with("\nTAIL"));
    }

    #[test]
    fn tool_call_exit_code_ignores_generic_code_fields() {
        assert_eq!(
            tool_call_exit_code(Some(&serde_json::json!({ "code": 200 }))),
            None
        );
        assert_eq!(
            tool_call_exit_code(Some(&serde_json::json!({ "exitCode": 7 }))),
            Some(7)
        );
        assert_eq!(
            tool_call_exit_code(Some(&serde_json::json!({ "exit_code": 9 }))),
            Some(9)
        );
    }

    fn proposal_permission_request(command: &str) -> acp::schema::v1::RequestPermissionRequest {
        use acp::schema::v1::{
            PermissionOption, PermissionOptionKind, RequestPermissionRequest, ToolCallId,
            ToolCallUpdate, ToolCallUpdateFields, ToolKind,
        };

        RequestPermissionRequest::new(
            acp::schema::v1::SessionId::new("proposal-session"),
            ToolCallUpdate::new(
                ToolCallId::new("proposal-tool"),
                ToolCallUpdateFields::new()
                    .kind(ToolKind::Execute)
                    .raw_input(serde_json::json!({
                        "command": command,
                        "commands": [command],
                    })),
            ),
            vec![PermissionOption::new(
                "allow-once",
                "Allow once",
                PermissionOptionKind::AllowOnce,
            )],
        )
    }

    fn proposal_mcp_permission_request() -> acp::schema::v1::RequestPermissionRequest {
        use acp::schema::v1::{
            PermissionOption, PermissionOptionKind, RequestPermissionRequest, ToolCallId,
            ToolCallUpdate, ToolCallUpdateFields,
        };

        RequestPermissionRequest::new(
            acp::schema::v1::SessionId::new("proposal-session"),
            ToolCallUpdate::new(
                ToolCallId::new("proposal-mcp-tool"),
                ToolCallUpdateFields::new()
                    .title("intellterm_0123456789abcdef/run_command_in_current_shell")
                    .raw_input(serde_json::json!({
                        "summary": "Run test",
                        "command": "cargo test"
                    })),
            ),
            vec![PermissionOption::new(
                "allow-once",
                "Allow once",
                PermissionOptionKind::AllowOnce,
            )],
        )
    }

    fn proposal_test_client(
        manager: Arc<crate::agent_tools::action_proposal::channel::ProposalChannelManager>,
    ) -> (WtaClient, mpsc::UnboundedReceiver<AppEvent>) {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let state = Arc::new(ClientState {
            event_tx,
            shell_mgr: Arc::new(ShellManager::new()),
            prompt_timing: Arc::new(PromptTimingState::default()),
            provider_probe_capture: super::ProviderProbeCapture::default(),
            standard_usage_sessions: Mutex::new(HashSet::new()),
            proposal_channels: manager,
            hidden_tool_calls: Mutex::new(HashMap::new()),
        });
        (WtaClient { state }, event_rx)
    }

    #[tokio::test]
    async fn canonical_proposal_permission_is_silent_and_does_not_gate_submission() {
        let manager =
            Arc::new(crate::agent_tools::action_proposal::channel::ProposalChannelManager::new());
        let payload = r#"{"schema_version":1,"origin":"terminal_agent","choices":[{"choice":1,"title":"run test","rationale":"","actions":[{"type":"send","input":"cargo test"}]}]}"#;
        let channel = manager
            .issue("proposal-session".into(), 1, None, false)
            .unwrap();
        let command =
            crate::agent_tools::action_proposal::invocation::render(&channel, payload).unwrap();
        let (client, mut event_rx) = proposal_test_client(Arc::clone(&manager));

        let response = client
            .request_permission(proposal_permission_request(&command))
            .await
            .unwrap();

        assert!(matches!(
            response.outcome,
            acp::schema::v1::RequestPermissionOutcome::Selected(_)
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(AppEvent::HideToolCall { session_id, id })
                if session_id == "proposal-session" && id == "proposal-tool"
        ));
        assert!(manager.begin_validation(&channel).is_ok());
    }

    #[tokio::test]
    async fn canonical_proposal_permission_is_cancelled_for_another_session() {
        let manager =
            Arc::new(crate::agent_tools::action_proposal::channel::ProposalChannelManager::new());
        let payload = r#"{"schema_version":1,"origin":"terminal_agent","choices":[{"choice":1,"title":"run test","rationale":"","actions":[{"type":"send","input":"cargo test"}]}]}"#;
        let channel = manager
            .issue("different-session".into(), 1, None, false)
            .unwrap();
        let command =
            crate::agent_tools::action_proposal::invocation::render(&channel, payload).unwrap();
        let (client, mut event_rx) = proposal_test_client(Arc::clone(&manager));

        let response = client
            .request_permission(proposal_permission_request(&command))
            .await
            .unwrap();

        assert!(matches!(
            response.outcome,
            acp::schema::v1::RequestPermissionOutcome::Cancelled
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(AppEvent::HideToolCall { .. })
        ));
        assert!(manager.begin_validation(&channel).is_ok());
    }

    #[tokio::test]
    async fn proposal_mcp_permission_is_silent_and_does_not_consume_submission() {
        let manager =
            Arc::new(crate::agent_tools::action_proposal::channel::ProposalChannelManager::new());
        manager
            .issue("proposal-session".into(), 1, None, false)
            .unwrap();
        let (client, mut event_rx) = proposal_test_client(Arc::clone(&manager));

        let response = client
            .request_permission(proposal_mcp_permission_request())
            .await
            .unwrap();

        assert!(matches!(
            response.outcome,
            acp::schema::v1::RequestPermissionOutcome::Selected(_)
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(AppEvent::HideToolCall { session_id, id })
                if session_id == "proposal-session" && id == "proposal-mcp-tool"
        ));
        assert!(manager.begin_mcp_validation("proposal-session").is_ok());
    }

    #[tokio::test]
    async fn user_input_mcp_permission_is_silent() {
        use acp::schema::v1::{
            PermissionOption, PermissionOptionKind, RequestPermissionRequest, ToolCallId,
            ToolCallUpdate, ToolCallUpdateFields,
        };

        let manager =
            Arc::new(crate::agent_tools::action_proposal::channel::ProposalChannelManager::new());
        let (client, mut event_rx) = proposal_test_client(manager);
        let response = client
            .request_permission(RequestPermissionRequest::new(
                acp::schema::v1::SessionId::new("input-session"),
                ToolCallUpdate::new(
                    ToolCallId::new("input-tool"),
                    ToolCallUpdateFields::new()
                        .title("intellterm_0123456789abcdef/request_user_input")
                        .raw_input(serde_json::json!({
                            "question": "Choose",
                            "choices": ["A", "B"]
                        })),
                ),
                vec![PermissionOption::new(
                    "allow-once",
                    "Allow once",
                    PermissionOptionKind::AllowOnce,
                )],
            ))
            .await
            .unwrap();

        assert!(matches!(
            response.outcome,
            acp::schema::v1::RequestPermissionOutcome::Selected(_)
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(AppEvent::HideToolCall { session_id, id })
                if session_id == "input-session" && id == "input-tool"
        ));
        assert!(event_rx.try_recv().is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dropped_user_input_extension_cancels_its_modal() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let manager = Arc::new(
                    crate::agent_tools::action_proposal::channel::ProposalChannelManager::new(),
                );
                let (client, mut event_rx) = proposal_test_client(manager);
                let params = serde_json::value::to_raw_value(
                    &crate::agent_tools::session_mcp::UserInputHelperRequest {
                        request_id: "request-1".into(),
                        session_id: "input-session".into(),
                        request: crate::agent_tools::user_input::UserInputRequest {
                            question: "Choose".into(),
                            choices: vec!["A".into()],
                            allow_freeform: false,
                        },
                    },
                )
                .unwrap();
                let task = tokio::task::spawn_local(async move {
                    client
                        .request_user_input(acp::schema::v1::ExtRequest::new(
                            crate::agent_tools::session_mcp::USER_INPUT_HELPER_REQUEST_METHOD,
                            params.into(),
                        ))
                        .await
                });

                assert!(matches!(
                    event_rx.recv().await,
                    Some(AppEvent::UserInputRequest {
                        request_id,
                        session_id,
                        ..
                    }) if request_id == "request-1" && session_id == "input-session"
                ));
                task.abort();
                let _ = task.await;
                assert!(matches!(
                    event_rx.recv().await,
                    Some(AppEvent::CancelUserInputRequest {
                        request_id,
                        session_id,
                    }) if request_id == "request-1" && session_id == "input-session"
                ));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn user_input_extension_returns_the_modal_answer() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let manager = Arc::new(
                    crate::agent_tools::action_proposal::channel::ProposalChannelManager::new(),
                );
                let (client, mut event_rx) = proposal_test_client(manager);
                let params = serde_json::value::to_raw_value(
                    &crate::agent_tools::session_mcp::UserInputHelperRequest {
                        request_id: "request-2".into(),
                        session_id: "input-session".into(),
                        request: crate::agent_tools::user_input::UserInputRequest {
                            question: "Choose".into(),
                            choices: vec!["A".into(), "B".into()],
                            allow_freeform: false,
                        },
                    },
                )
                .unwrap();
                let task = tokio::task::spawn_local(async move {
                    client
                        .request_user_input(acp::schema::v1::ExtRequest::new(
                            crate::agent_tools::session_mcp::USER_INPUT_HELPER_REQUEST_METHOD,
                            params.into(),
                        ))
                        .await
                });

                let responder = match event_rx.recv().await {
                    Some(AppEvent::UserInputRequest { responder, .. }) => responder,
                    _ => panic!("expected user input request"),
                };
                responder
                    .send(
                        crate::agent_tools::user_input::UserInputResponse::Answered {
                            answer: "B".into(),
                            selected_index: Some(1),
                        },
                    )
                    .unwrap();
                let response = task.await.unwrap().unwrap();
                let decoded: crate::agent_tools::user_input::UserInputResponse =
                    serde_json::from_str(response.0.get()).unwrap();
                assert_eq!(
                    decoded,
                    crate::agent_tools::user_input::UserInputResponse::Answered {
                        answer: "B".into(),
                        selected_index: Some(1),
                    }
                );
                assert!(event_rx.try_recv().is_err());
            })
            .await;
    }

    #[tokio::test]
    async fn proposal_mcp_extension_validates_and_commits_on_the_owning_helper() {
        let manager =
            Arc::new(crate::agent_tools::action_proposal::channel::ProposalChannelManager::new());
        manager
            .issue("proposal-session".into(), 1, None, false)
            .unwrap();
        let (client, mut event_rx) = proposal_test_client(Arc::clone(&manager));
        let params =
            serde_json::value::to_raw_value(&crate::agent_tools::session_mcp::HelperRequest {
                session_id: "proposal-session".to_string(),
                tool: "run_command_in_current_shell".to_string(),
                arguments: serde_json::json!({
                    "summary": "Run test",
                    "command": "cargo test"
                }),
            })
            .unwrap();
        let request = acp::schema::v1::ExtRequest::new(
            crate::agent_tools::session_mcp::HELPER_REQUEST_METHOD,
            params.into(),
        );

        let call = client.request_terminal_actions(request);
        let ui = async {
            let proposal_id = match event_rx.recv().await.unwrap() {
                AppEvent::DirectTerminalActionProposal {
                    context,
                    payload,
                    source,
                    responder,
                    ..
                } => {
                    assert_eq!(
                        source,
                        crate::agent_tools::action_proposal::pipe::ProposalPayloadSource::Mcp(
                            crate::agent_tools::action_proposal::schema::McpActionTool::RunCommandInCurrentShell
                        )
                    );
                    assert_eq!(
                        serde_json::from_str::<serde_json::Value>(&payload).unwrap(),
                        serde_json::json!({
                            "summary": "Run test",
                            "command": "cargo test"
                        })
                    );
                    responder
                        .send(
                            crate::agent_tools::action_proposal::pipe::ProposalValidationDecision::accepted(),
                        )
                        .unwrap();
                    context.proposal_id
                }
                _ => panic!("expected proposal validation event"),
            };
            match event_rx.recv().await.unwrap() {
                AppEvent::DirectTerminalActionProposalCommit {
                    proposal_id: committed,
                    responder,
                } => {
                    assert_eq!(committed, proposal_id);
                    responder.send(true).unwrap();
                }
                _ => panic!("expected proposal commit event"),
            }
        };
        let (response, ()) = tokio::join!(call, ui);
        let response = response.unwrap();
        let validation: crate::agent_tools::action_proposal::pipe::ProposalValidationResponse =
            serde_json::from_str(response.0.get()).unwrap();
        assert_eq!(
            validation.status,
            crate::agent_tools::action_proposal::channel::ProposalValidationStatus::Accepted
        );
    }

    #[tokio::test]
    async fn noncanonical_proposal_permission_is_silently_cancelled() {
        let manager =
            Arc::new(crate::agent_tools::action_proposal::channel::ProposalChannelManager::new());
        let channel = manager
            .issue("proposal-session".into(), 1, None, false)
            .unwrap();
        let command = format!(
            "'{{}}' | & \"$env:WTA_CLI_PATH\" propose-terminal-actions --channel {channel}"
        );
        let (client, mut event_rx) = proposal_test_client(manager);

        let response = client
            .request_permission(proposal_permission_request(&command))
            .await
            .unwrap();

        assert!(matches!(
            response.outcome,
            acp::schema::v1::RequestPermissionOutcome::Cancelled
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(AppEvent::HideToolCall { .. })
        ));
    }

    /// Each `ToolKind` that has a visual cue maps to a distinct, stable
    /// glyph — not a translatable word (see `tool_call_kind_label`'s doc
    /// comment: kind labels are exactly the kind of ambiguous 1-2-word
    /// string this repo's 85+-locale localization flags as
    /// mistranslation-prone, e.g. "Execute" reading as "kill"). Kinds with
    /// no useful visual framing (`Think`, `SwitchMode`, `Other`) get `None`
    /// so the permission card just shows the title alone.
    #[test]
    fn tool_call_kind_label_maps_each_kind_to_a_stable_glyph() {
        use acp::schema::v1::ToolKind;
        assert_eq!(tool_call_kind_label(Some(&ToolKind::Read)), Some("→"));
        assert_eq!(tool_call_kind_label(Some(&ToolKind::Search)), Some("→"));
        assert_eq!(tool_call_kind_label(Some(&ToolKind::Move)), Some("→"));
        assert_eq!(tool_call_kind_label(Some(&ToolKind::Edit)), Some("✎"));
        assert_eq!(tool_call_kind_label(Some(&ToolKind::Delete)), Some("✕"));
        assert_eq!(tool_call_kind_label(Some(&ToolKind::Execute)), Some("$"));
        assert_eq!(tool_call_kind_label(Some(&ToolKind::Fetch)), Some("%"));
        assert_eq!(tool_call_kind_label(Some(&ToolKind::Think)), None);
        assert_eq!(tool_call_kind_label(Some(&ToolKind::SwitchMode)), None);
        assert_eq!(tool_call_kind_label(Some(&ToolKind::Other)), None);
        assert_eq!(tool_call_kind_label(None), None);
    }

    #[test]
    fn post_login_authenticate_auth_required_routes_to_recovery_failure() {
        let err = post_login_authenticate_error("copilot-login", &acp::Error::auth_required());
        let failure =
            crate::protocol::acp::failure::classify_anyhow(&err, HandshakeStage::Authenticate);
        assert!(
            matches!(failure, AgentFailure::AuthRequired { .. }),
            "AuthRequired from post-login authenticate should stay recoverable, got {failure:?}"
        );
    }

    #[test]
    fn post_login_authenticate_non_auth_stays_authenticate_handshake_failure() {
        let err = post_login_authenticate_error("copilot-login", &acp::Error::new(-32603, "boom"));
        let failure =
            crate::protocol::acp::failure::classify_anyhow(&err, HandshakeStage::Authenticate);
        assert!(
            matches!(
                failure,
                AgentFailure::HandshakeFailed {
                    stage: HandshakeStage::Authenticate,
                    ..
                }
            ),
            "non-auth authenticate errors should not trigger fresh-master recovery, got {failure:?}"
        );
    }

    /// Helper-only: round-trip a `_meta` blob through `inject_wta_pane_meta`
    /// and report the `pane_session_id` that the master would see in
    /// `extract_wta_meta`. Returns `None` when the meta is empty after
    /// injection (i.e. `WT_SESSION` was missing/empty and we correctly
    /// emitted no namespace).
    fn injected_pane_session_id() -> Option<String> {
        let mut meta: Option<agent_client_protocol::schema::v1::Meta> = None;
        inject_wta_pane_meta(&mut meta, false);
        crate::session_registry::extract_wta_meta(&mut meta).pane_session_id
    }

    #[test]
    fn inject_wta_pane_meta_injects_lowercased_pane_session_id_with_braces_stripped() {
        let _g = crate::test_support::lock_env();
        // SAFETY: env is process-global; lock_env serializes parallel tests.
        unsafe {
            std::env::set_var("WT_SESSION", "{A86EAF3B-1234-5678-9ABC-DEF012345678}");
        }
        assert_eq!(
            injected_pane_session_id(),
            Some("a86eaf3b-1234-5678-9abc-def012345678".to_string()),
            "WT_SESSION should be lowercased and have braces stripped before going on the wire",
        );
        unsafe { std::env::remove_var("WT_SESSION") };
    }

    #[test]
    fn inject_wta_pane_meta_is_noop_when_wt_session_is_absent() {
        let _g = crate::test_support::lock_env();
        unsafe { std::env::remove_var("WT_SESSION") };
        assert_eq!(
            injected_pane_session_id(),
            None,
            "no WT_SESSION → master must not record a phantom pane binding",
        );
    }

    #[test]
    fn inject_wta_pane_meta_is_noop_when_wt_session_is_empty() {
        let _g = crate::test_support::lock_env();
        unsafe { std::env::set_var("WT_SESSION", "") };
        assert_eq!(injected_pane_session_id(), None);
        unsafe { std::env::remove_var("WT_SESSION") };
    }

    #[test]
    fn inject_wta_pane_meta_is_noop_when_wt_session_is_only_braces() {
        let _g = crate::test_support::lock_env();
        unsafe { std::env::set_var("WT_SESSION", "{}") };
        assert_eq!(
            injected_pane_session_id(),
            None,
            "stripping braces from `{{}}` leaves the empty string — must not write `pane_session_id`: \"\"",
        );
        unsafe { std::env::remove_var("WT_SESSION") };
    }

    #[test]
    fn inject_wta_pane_meta_requests_master_http_mcp_without_wt_session() {
        let _g = crate::test_support::lock_env();
        unsafe { std::env::remove_var("WT_SESSION") };
        let mut meta: Option<agent_client_protocol::schema::v1::Meta> = None;
        inject_wta_pane_meta(&mut meta, true);
        let extracted = crate::session_registry::extract_wta_meta(&mut meta);
        assert_eq!(extracted.proposal_mcp.as_deref(), Some("http-v1"));
    }

    #[test]
    fn session_mcp_tool_title_accepts_supported_permission_shapes() {
        let dynamic = "intellterm_0123456789abcdef";
        for tool in crate::agent_tools::action_proposal::schema::McpActionTool::ALL {
            let name = tool.tool_name();
            for title in [
                format!("Use MCP tool: intelligent_terminal/{name}"),
                format!("intelligent_terminal-{name}"),
                format!("Use MCP tool: {dynamic}/{name}"),
                format!("mcp__{dynamic}__{name}"),
            ] {
                assert_eq!(
                    session_mcp_tool_from_title(Some(&title)),
                    Some(SessionMcpTool::TerminalAction(tool)),
                    "{title}"
                );
            }
        }
        assert_eq!(
            session_mcp_tool_from_title(Some(&format!(
                "Use MCP tool: {dynamic}/request_user_input"
            ))),
            Some(SessionMcpTool::UserInput)
        );
        for title in [
            "run_command_in_current_shell",
            "create_workspace",
            "delegate_task_in_new_workspace",
            "request_user_input",
            "run_command",
            "open_workspace",
            "run_command_in_workspace",
            "delegate_task",
            "intellterm_01234567890123456789/run_command_in_current_shell",
            "intellterm_0123456789abcde/run_command_in_current_shell",
            "intellterm_0123456789abcdeA/run_command_in_current_shell",
            "Use MCP tool: other/run_command_in_current_shell",
            "Use MCP tool: intelligent_terminal/terminal_send",
            "Use MCP tool: intelligent_terminal/terminal_open",
            "Use MCP tool: intelligent_terminal/terminal_open_and_send",
            "Use MCP tool: intelligent_terminal/run_command",
            "Use MCP tool: intelligent_terminal/open_workspace",
            "Use MCP tool: intelligent_terminal/run_command_in_workspace",
            "Use MCP tool: intelligent_terminal/delegate_task",
            // The superseded single-tool name is no longer a tool.
            "Use MCP tool: intelligent_terminal/request_terminal_actions",
            "mcp__intellterm_0123456789abcdef__request_terminal_actions",
        ] {
            assert_eq!(session_mcp_tool_from_title(Some(title)), None, "{title}");
        }
    }

    #[tokio::test]
    async fn session_mcp_display_keeps_bare_schema_valid_action_calls_visible() {
        use crate::agent_tools::action_proposal::schema::McpActionTool;

        let manager =
            Arc::new(crate::agent_tools::action_proposal::channel::ProposalChannelManager::new());
        let (client, mut event_rx) = proposal_test_client(manager);
        for (tool, arguments) in [
            (
                McpActionTool::RunCommandInCurrentShell,
                serde_json::json!({"summary":"Run tests","command":"cargo test"}),
            ),
            (
                McpActionTool::CreateWorkspace,
                serde_json::json!({
                    "summary":"Run tests separately",
                    "command":"cargo test",
                    "placement":"new_split"
                }),
            ),
            (
                McpActionTool::DelegateTaskInNewWorkspace,
                serde_json::json!({
                    "summary":"Investigate failures",
                    "task":"Find and fix the failing tests.",
                    "placement":"new_tab"
                }),
            ),
        ] {
            let tool_call_id = format!("provider-{}", tool.tool_name());
            client
                .session_notification(acp::schema::v1::SessionNotification::new(
                    acp::schema::v1::SessionId::new("provider-session"),
                    acp::schema::v1::SessionUpdate::ToolCall(
                        acp::schema::v1::ToolCall::new(
                            acp::schema::v1::ToolCallId::new(tool_call_id.as_str()),
                            tool.tool_name(),
                        )
                        .raw_input(Some(arguments)),
                    ),
                ))
                .await
                .unwrap();

            assert!(matches!(
                event_rx.try_recv(),
                Ok(AppEvent::ToolCall {
                    session_id,
                    id,
                    title,
                    ..
                }) if session_id == "provider-session"
                    && id == tool_call_id
                    && title == tool.tool_name()
            ));
        }
        assert!(event_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn session_mcp_display_hides_namespace_qualified_calls() {
        let manager =
            Arc::new(crate::agent_tools::action_proposal::channel::ProposalChannelManager::new());
        let (client, mut event_rx) = proposal_test_client(manager);
        client
            .session_notification(acp::schema::v1::SessionNotification::new(
                acp::schema::v1::SessionId::new("proposal-session"),
                acp::schema::v1::SessionUpdate::ToolCall(acp::schema::v1::ToolCall::new(
                    acp::schema::v1::ToolCallId::new("qualified-tool"),
                    "intellterm_0123456789abcdef/run_command_in_current_shell",
                )),
            ))
            .await
            .unwrap();

        assert!(matches!(
            event_rx.try_recv(),
            Ok(AppEvent::HideToolCall { session_id, id })
                if session_id == "proposal-session" && id == "qualified-tool"
        ));
        assert!(event_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn session_mcp_display_keeps_stable_namespace_calls_visible() {
        let manager =
            Arc::new(crate::agent_tools::action_proposal::channel::ProposalChannelManager::new());
        let (client, mut event_rx) = proposal_test_client(manager);

        client
            .session_notification(acp::schema::v1::SessionNotification::new(
                acp::schema::v1::SessionId::new("provider-session"),
                acp::schema::v1::SessionUpdate::ToolCall(acp::schema::v1::ToolCall::new(
                    acp::schema::v1::ToolCallId::new("stable-tool"),
                    "intelligent_terminal/run_command_in_current_shell",
                )),
            ))
            .await
            .unwrap();
        assert!(matches!(
            event_rx.try_recv(),
            Ok(AppEvent::ToolCall { id, .. }) if id == "stable-tool"
        ));

        client
            .session_notification(acp::schema::v1::SessionNotification::new(
                acp::schema::v1::SessionId::new("provider-session"),
                acp::schema::v1::SessionUpdate::ToolCallUpdate(
                    acp::schema::v1::ToolCallUpdate::new(
                        acp::schema::v1::ToolCallId::new("stable-update"),
                        acp::schema::v1::ToolCallUpdateFields::new()
                            .title("intelligent_terminal-run_command_in_current_shell"),
                    ),
                ),
            ))
            .await
            .unwrap();
        assert!(matches!(
            event_rx.try_recv(),
            Ok(AppEvent::ToolCallUpdate { id, .. }) if id == "stable-update"
        ));
        assert!(event_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn session_mcp_display_hides_calls_correlated_by_tool_call_id() {
        let manager =
            Arc::new(crate::agent_tools::action_proposal::channel::ProposalChannelManager::new());
        manager
            .issue("proposal-session".into(), 1, None, false)
            .unwrap();
        let (client, mut event_rx) = proposal_test_client(manager);
        let response = client
            .request_permission(proposal_mcp_permission_request())
            .await
            .unwrap();
        assert!(matches!(
            response.outcome,
            acp::schema::v1::RequestPermissionOutcome::Selected(_)
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(AppEvent::HideToolCall { session_id, id })
                if session_id == "proposal-session" && id == "proposal-mcp-tool"
        ));

        client
            .session_notification(acp::schema::v1::SessionNotification::new(
                acp::schema::v1::SessionId::new("proposal-session"),
                acp::schema::v1::SessionUpdate::ToolCall(
                    acp::schema::v1::ToolCall::new(
                        acp::schema::v1::ToolCallId::new("proposal-mcp-tool"),
                        "run_command_in_current_shell",
                    )
                    .raw_input(Some(serde_json::json!({
                        "summary": "Run test",
                        "command": "cargo test"
                    }))),
                ),
            ))
            .await
            .unwrap();

        assert!(
            event_rx.try_recv().is_err(),
            "the correlated tool call must remain hidden"
        );
    }

    /// Regression for the cross-window focus bug: the helper-over-pipe
    /// `session/load` path must inject `_meta.wta.pane_session_id`
    /// alongside the request so master's `SessionInfo.pane_session_id`
    /// for the resumed sid points at THIS pane's GUID. Without the
    /// binding the row in a sibling window's session management list appears live but
    /// `decide_enter_action` returns `NotResumable { LiveWithoutPane }`
    /// and the user sees "Cannot focus session …: it appears live but
    /// no pane GUID is bound yet."
    ///
    /// Exercises the same shape of code as the actual call site
    /// (build `LoadSessionRequest` + call `inject_wta_pane_meta` on its
    /// meta field) and asserts master would extract the same pane id
    /// via `extract_wta_meta`.
    #[test]
    fn load_session_request_carries_pane_session_id_after_injection() {
        use agent_client_protocol as acp;
        let _g = crate::test_support::lock_env();
        unsafe {
            std::env::set_var("WT_SESSION", "{B1234567-89AB-CDEF-0123-456789ABCDEF}");
        }

        let sid = acp::schema::v1::SessionId::new("sess-target".to_string());
        let cwd = std::path::PathBuf::from("/repo");
        let mut req = acp::schema::v1::LoadSessionRequest::new(sid, cwd);
        assert!(req.meta.is_none(), "fresh LoadSessionRequest has no meta");

        inject_wta_pane_meta(&mut req.meta, false);

        let extracted = crate::session_registry::extract_wta_meta(&mut req.meta);
        assert_eq!(
            extracted.pane_session_id.as_deref(),
            Some("b1234567-89ab-cdef-0123-456789abcdef"),
            "master must be able to extract the pane GUID from the load_session request"
        );

        unsafe { std::env::remove_var("WT_SESSION") };
    }

    #[test]
    fn parses_model_from_separate_flag() {
        let profile = crate::agent_registry::lookup_profile("copilot");
        let args = ["--acp", "--stdio", "--model", "claude-haiku-4.5"];
        assert_eq!(
            crate::agent_registry::extract_model_from_args(&args, profile),
            Some("claude-haiku-4.5")
        );
    }

    #[test]
    fn gemini_method_not_found_is_a_redundant_startup_model_error() {
        let identity = PromptUsageIdentity {
            family_id: Some("gemini".to_string()),
            reporter_id: Some("gemini-cli".to_string()),
        };

        assert!(is_redundant_startup_model_error(
            &identity,
            &acp::Error::method_not_found(),
        ));
        assert!(!is_redundant_startup_model_error(
            &PromptUsageIdentity {
                family_id: Some("copilot".to_string()),
                reporter_id: Some("gemini-cli".to_string()),
            },
            &acp::Error::method_not_found(),
        ));
        assert!(!is_redundant_startup_model_error(
            &PromptUsageIdentity {
                family_id: Some("gemini".to_string()),
                reporter_id: Some("impostor-gemini".to_string()),
            },
            &acp::Error::method_not_found(),
        ));
        assert!(!is_redundant_startup_model_error(
            &identity,
            &acp::Error::internal_error(),
        ));
    }

    #[tokio::test]
    async fn successful_prompt_completion_emits_message_end_only() {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let prompt_timing = PromptTimingState::default();

        complete_prompt_request(
            Ok::<(), acp::Error>(()),
            None,
            &prompt_timing,
            &event_tx,
            "test-session".to_string(),
        )
        .await;

        match event_rx.try_recv() {
            Ok(AppEvent::AgentMessageEnd { session_id }) => {
                assert_eq!(session_id, "test-session");
            }
            Ok(_) => panic!("expected AgentMessageEnd"),
            Err(err) => panic!("expected AgentMessageEnd, got channel error: {err}"),
        }
        assert!(event_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn soft_stop_emits_message_end_then_soft_stop() {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let prompt_timing = PromptTimingState::default();

        complete_prompt_request(
            Ok::<(), acp::Error>(()),
            Some(SoftStopReason::Refusal),
            &prompt_timing,
            &event_tx,
            "test-session".to_string(),
        )
        .await;

        // Order matters: the turn-closing AgentMessageEnd must land first so the
        // soft-stop notice appends after the agent's streamed content.
        match event_rx.try_recv() {
            Ok(AppEvent::AgentMessageEnd { session_id }) => {
                assert_eq!(session_id, "test-session");
            }
            Ok(_) => panic!("expected AgentMessageEnd first"),
            Err(err) => panic!("expected AgentMessageEnd first, got channel error: {err}"),
        }
        match event_rx.try_recv() {
            Ok(AppEvent::AgentSoftStop { session_id, reason }) => {
                assert_eq!(session_id, "test-session");
                assert_eq!(reason, SoftStopReason::Refusal);
            }
            Ok(_) => panic!("expected AgentSoftStop second"),
            Err(err) => panic!("expected AgentSoftStop second, got channel error: {err}"),
        }
        assert!(event_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn failed_prompt_completion_emits_error_only() {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let prompt_timing = PromptTimingState::default();

        complete_prompt_request(
            Err::<(), acp::Error>(acp::Error::new(-32603, "boom")),
            None,
            &prompt_timing,
            &event_tx,
            "test-session".to_string(),
        )
        .await;

        match event_rx.try_recv() {
            Ok(AppEvent::AgentError {
                session_id,
                failure,
                message,
            }) => {
                assert_eq!(session_id.as_deref(), Some("test-session"));
                assert_eq!(message, "prompt error: boom");
                assert_eq!(
                    failure,
                    crate::protocol::acp::failure::AgentFailure::Protocol {
                        code: -32603,
                        message: "boom".to_string(),
                    }
                );
            }
            Ok(_) => panic!("expected AgentError"),
            Err(err) => panic!("expected AgentError, got channel error: {err}"),
        }
        assert!(event_rx.try_recv().is_err());
    }

    // ── telemetry failure-field mapping ─────────────────────────────────────

    /// `acp_result_failure_fields` reports no failure for `Ok`, and surfaces
    /// the ACP error code (as i32) under the `AcpError` kind for `Err`.
    #[test]
    fn acp_result_failure_fields_maps_ok_and_err() {
        let ok: acp::Result<()> = Ok(());
        assert_eq!(acp_result_failure_fields(&ok), ("", 0));

        let err: acp::Result<()> = Err(acp::Error::new(-32603, "boom"));
        assert_eq!(acp_result_failure_fields(&err), ("AcpError", -32603));
    }

    /// `timeout_result_failure_fields` forwards the inner ACP result when the
    /// call completed in time (both Ok and Err), and reports the `Timeout`
    /// kind only when the outer future actually elapsed.
    #[tokio::test]
    async fn timeout_result_failure_fields_maps_inner_and_elapsed() {
        // Completed in time, inner Ok → no failure.
        let inner_ok: Result<acp::Result<()>, tokio::time::error::Elapsed> = Ok(Ok(()));
        assert_eq!(timeout_result_failure_fields(&inner_ok), ("", 0));

        // Completed in time, inner Err → surface the ACP error code.
        let inner_err: Result<acp::Result<()>, tokio::time::error::Elapsed> =
            Ok(Err(acp::Error::new(-32000, "nope")));
        assert_eq!(
            timeout_result_failure_fields(&inner_err),
            ("AcpError", -32000)
        );

        // Outer future elapsed → Timeout, no ACP code.
        let elapsed = tokio::time::timeout(std::time::Duration::ZERO, std::future::pending::<()>())
            .await
            .expect_err("a zero-duration timeout over a pending future must elapse");
        let timed_out: Result<acp::Result<()>, tokio::time::error::Elapsed> = Err(elapsed);
        assert_eq!(timeout_result_failure_fields(&timed_out), ("Timeout", 0));
    }

    #[test]
    fn build_prompt_content_text_only_is_single_text_block() {
        let content = super::build_prompt_content("hello", &[]);
        assert_eq!(content.len(), 1);
        match &content[0] {
            acp::schema::v1::ContentBlock::Text(t) => assert_eq!(t.text, "hello"),
            other => panic!("expected text block, got {other:?}"),
        }
    }

    #[test]
    fn build_prompt_content_appends_image_blocks_after_text() {
        let images = vec![
            crate::clipboard_image::PastedImage {
                data_base64: "AAA=".to_string(),
                mime_type: "image/png".to_string(),
                label: "screenshot".to_string(),
            },
            crate::clipboard_image::PastedImage {
                data_base64: "BBB=".to_string(),
                mime_type: "image/jpeg".to_string(),
                label: "photo.jpg".to_string(),
            },
        ];
        let content = super::build_prompt_content("look at these", &images);
        assert_eq!(content.len(), 3, "1 text + 2 image blocks");
        assert!(matches!(content[0], acp::schema::v1::ContentBlock::Text(_)));
        match (&content[1], &content[2]) {
            (acp::schema::v1::ContentBlock::Image(a), acp::schema::v1::ContentBlock::Image(b)) => {
                assert_eq!(a.data, "AAA=");
                assert_eq!(a.mime_type, "image/png");
                assert_eq!(b.data, "BBB=");
                assert_eq!(b.mime_type, "image/jpeg");
            }
            other => panic!("expected two image blocks, got {other:?}"),
        }
    }

    #[test]
    fn build_prompt_content_image_only_keeps_empty_leading_text_block() {
        // Image-only paste (no typed text) still ships a (empty) text block
        // first so the agent's content array always leads with text.
        let images = vec![crate::clipboard_image::PastedImage {
            data_base64: "ZZZ=".to_string(),
            mime_type: "image/png".to_string(),
            label: "screenshot".to_string(),
        }];
        let content = super::build_prompt_content("", &images);
        assert_eq!(content.len(), 2);
        assert!(matches!(content[0], acp::schema::v1::ContentBlock::Text(_)));
        assert!(matches!(
            content[1],
            acp::schema::v1::ContentBlock::Image(_)
        ));
    }

    /// Test the helper's mirror of master's session-broadcast feed.
    ///
    /// `WtaClient::ext_notification` is the helper's sole inbound path
    /// for `intellterm.wta/session_{added,removed}` extension
    /// notifications. It must translate them into the matching
    /// `AppEvent::AliveSession{Added,Removed}` variants so the App
    /// event loop — the single writer to `App.alive` — can keep the
    /// per-helper registry mirror consistent. The tests below
    /// construct a `WtaClient` with a fake `event_tx` and assert the
    /// translation contract: well-formed notifications produce typed
    /// events, malformed/unknown notifications produce nothing (and do
    /// not tear down the connection).
    mod ext_notification_tests {
        use super::super::{ClientState, WtaClient};
        use crate::app_contracts::AppEvent;
        use crate::session_registry::{
            build_session_added_notification, build_session_removed_notification,
            INTELLTERM_METHOD_SESSION_REMOVED,
        };
        use crate::shell::ShellManager;
        use agent_client_protocol::{self as acp};
        use std::path::PathBuf;
        use std::sync::Arc;
        use tokio::sync::mpsc;

        fn make_client() -> (WtaClient, mpsc::UnboundedReceiver<AppEvent>) {
            let (tx, rx) = mpsc::unbounded_channel();
            let state = Arc::new(ClientState {
                event_tx: tx,
                shell_mgr: Arc::new(ShellManager::new()),
                prompt_timing: Arc::new(super::super::PromptTimingState::default()),
                provider_probe_capture: super::super::ProviderProbeCapture::default(),
                standard_usage_sessions: std::sync::Mutex::new(std::collections::HashSet::new()),
                proposal_channels: Arc::new(
                    crate::agent_tools::action_proposal::channel::ProposalChannelManager::new(),
                ),
                hidden_tool_calls: std::sync::Mutex::new(std::collections::HashMap::new()),
            });
            (WtaClient { state }, rx)
        }

        #[tokio::test]
        async fn session_added_translates_to_alive_session_added_event() {
            let (client, mut rx) = make_client();
            let info = crate::session_registry::SessionInfo::new(
                acp::schema::v1::SessionId::new("sess-1".to_string()),
                PathBuf::from("/work"),
            )
            .with_pane_session_id("pane-A".to_string());
            let ext = build_session_added_notification(&info);

            client.ext_notification(ext).await.unwrap();

            match rx.try_recv() {
                Ok(AppEvent::AliveSessionAdded(got)) => {
                    assert_eq!(got.session_id, info.session_id);
                    assert_eq!(got.pane_session_id.as_deref(), Some("pane-A"));
                    assert_eq!(got.cwd, info.cwd);
                }
                other => panic!(
                    "expected AliveSessionAdded, got something else: {}",
                    match &other {
                        Ok(_) => "Ok(<other variant>)",
                        Err(_) => "Err(<recv error>)",
                    }
                ),
            }
            assert!(rx.try_recv().is_err(), "exactly one event emitted");
        }

        #[tokio::test]
        async fn session_removed_translates_to_alive_session_removed_event() {
            let (client, mut rx) = make_client();
            let sid = acp::schema::v1::SessionId::new("sess-dead".to_string());
            let ext = build_session_removed_notification(&sid);

            client.ext_notification(ext).await.unwrap();

            match rx.try_recv() {
                Ok(AppEvent::AliveSessionRemoved(got)) => assert_eq!(got, sid),
                other => panic!(
                    "expected AliveSessionRemoved, got something else: {}",
                    match &other {
                        Ok(_) => "Ok(<other variant>)",
                        Err(_) => "Err(<recv error>)",
                    }
                ),
            }
            assert!(rx.try_recv().is_err());
        }

        #[tokio::test]
        async fn sessions_changed_translates_to_app_event() {
            let (client, mut rx) = make_client();
            let ext = crate::session_registry::build_sessions_changed_notification();

            client.ext_notification(ext).await.unwrap();

            match rx.try_recv() {
                Ok(AppEvent::SessionsChanged) => {}
                _ => panic!("expected SessionsChanged"),
            }
            assert!(rx.try_recv().is_err());
        }

        #[tokio::test]
        async fn unknown_namespace_is_silently_dropped() {
            let (client, mut rx) = make_client();
            let raw = serde_json::value::RawValue::from_string("{}".into()).unwrap();
            let ext = acp::schema::v1::ExtNotification::new(
                Arc::<str>::from("some.other.vendor/event"),
                Arc::from(raw),
            );

            client.ext_notification(ext).await.unwrap();

            assert!(
                rx.try_recv().is_err(),
                "unknown notification must not emit any AppEvent"
            );
        }

        #[tokio::test]
        async fn malformed_intellterm_params_are_silently_dropped() {
            let (client, mut rx) = make_client();
            let raw = serde_json::value::RawValue::from_string(r#"{"not_session_id":"x"}"#.into())
                .unwrap();
            let ext = acp::schema::v1::ExtNotification::new(
                Arc::<str>::from(INTELLTERM_METHOD_SESSION_REMOVED),
                Arc::from(raw),
            );

            // Must NOT return Err — that would close the ACP connection.
            client.ext_notification(ext).await.unwrap();

            assert!(
                rx.try_recv().is_err(),
                "malformed notification must not emit any AppEvent"
            );
        }
    }
}
