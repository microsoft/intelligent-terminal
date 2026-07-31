use super::failure::{AgentFailure, HandshakeStage};
use super::conn;
use super::prompt_builder::{
    acp_log_built_prompt, build_prompt_text, log_turn_trace, TemplateKind, TemplateMemo,
};
use super::soft_stop::SoftStopReason;
use super::turn_metrics::{now_unix_s, prompt_preview, PromptTimingState};
use agent_client_protocol as acp;
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tokio::sync::mpsc;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::app_contracts::{AcpModelInfo, AppEvent, PermOption, PlanEntry, PlanEntryStatus};
use crate::pane_context::PaneContext;
use crate::shell::{ShellManager, TerminalConfig};

// Normal helper startup can race a slow wta-master cold start: master opens its
// pipe only after spawning and initializing the agent CLI (up to 60s for npx
// adapters), so keep a long budget there.
const MASTER_PIPE_BACKOFF_MS: &[u64] = &[
    50, 100, 100, 200, 200, 500, 500, 1000, 1000, 2000, 2000, 2000, 5000, 5000, 5000, 5000,
    10000, 10000, 10000, 15000,
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
    /// True when this prompt was synthesized by the auto-fix flow rather
    /// than typed by a human. The host uses this to skip broadcasting it
    /// as a User message (the client already shows the error line), and
    /// the planner uses it to pick the auto-fix prompt template.
    pub is_autofix: bool,
    /// Images pasted into the input via Alt+V. Sent to the agent as ACP
    /// `ContentBlock::Image` blocks appended after the text block (only when
    /// the agent advertised `promptCapabilities.image`). Empty for the common
    /// text-only and all auto-fix prompts.
    pub images: Vec<crate::clipboard_image::PastedImage>,
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
/// per-tab cache and calls `new_session(cwd)`; the resulting
/// [`AppEvent::SessionAttached`] then propagates back to the UI to
/// rewire `session_to_tab` and update the model dropdown.
#[derive(Debug, Clone)]
pub struct NewSessionForTab {
    pub tab_id: String,
    /// Optional cwd override. When `None`, the client falls back to the
    /// process-wide `current_dir()` (same default as the lazy-create path).
    pub cwd: Option<String>,
}

/// User-initiated full reconnect of the ACP client. Emitted by the
/// `/restart` slash command. The ACP client task kills the agent child
/// process, drops the connection, then respawns the agent and
/// re-initializes from scratch. If `agent_cmd` is set, the supervisor
/// switches to a different agent on restart.
#[derive(Debug, Clone, Default)]
pub struct RestartRequest {
    pub agent_cmd: Option<String>,
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
    },
}

/// User-initiated request to resume a historical agent session by calling
/// the ACP `session/load` method, binding the loaded session to a
/// specific WT tab. Emitted by the session management view's Shift+Enter
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
        Self::new_with_kind(text, pane_context, false)
    }

    pub fn new_autofix(text: String, pane_context: Option<PaneContext>) -> Self {
        Self::new_with_kind(text, pane_context, true)
    }

    fn new_with_kind(text: String, pane_context: Option<PaneContext>, is_autofix: bool) -> Self {
        static NEXT_PROMPT_ID: AtomicU64 = AtomicU64::new(1);
        Self {
            id: NEXT_PROMPT_ID.fetch_add(1, Ordering::Relaxed),
            text,
            pane_context,
            submitted_at_unix_s: now_unix_s(),
            is_autofix,
            images: Vec::new(),
        }
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
}

/// Our Client trait implementation — handles incoming agent requests and notifications.
#[derive(Clone)]
struct WtaClient {
    state: Arc<ClientState>,
}

/// Maximum characters kept in a tool-call `location` hint before truncation.
/// Long enough for a typical path or one-line shell command, short enough
/// that a runaway `raw_input` value (e.g. a full file-edit payload) can't
/// blow up the chat card into a wall of text.
const TOOL_CALL_LOCATION_MAX_CHARS: usize = 200;

/// Best-effort extraction of *what* a tool call is touching: a file path
/// (from `locations`/`raw_input.path`/`raw_input.file_path`) or a shell
/// command (from `raw_input.command`/`commands`). Returns
/// `(text, is_command)` so callers can decide how to render it — a path
/// reads fine inline, but a command can be long and benefits from its own
/// code-styled line (see `ChatMessage::ToolCall::location_is_command`,
/// `PermissionState::target_is_command`).
///
/// Falls back to `None` rather than dumping the entire `raw_input` JSON,
/// which would be noisy and could leak large payloads (e.g. file contents
/// for a write/edit call) into the chat scrollback.
fn tool_call_target(
    locations: &[acp::schema::v1::ToolCallLocation],
    raw_input: Option<&serde_json::Value>,
) -> Option<(String, bool)> {
    if let Some(path) = locations
        .iter()
        .map(|loc| loc.path.to_string_lossy())
        .find(|path| !path.trim().is_empty())
    {
        return Some((path.into_owned(), false));
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
    None
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
    locations: &[acp::schema::v1::ToolCallLocation],
    raw_input: Option<&serde_json::Value>,
) -> Option<(String, bool)> {
    let (hint, is_command) = tool_call_target(locations, raw_input)?;
    let hint = truncate_target(hint);
    let comparison_hint = hint.strip_suffix('…').unwrap_or(&hint);

    // Don't repeat text the title already contains — case-insensitive so
    // "Viewing C:\...\rust-app" still dedupes against a locations path that
    // differs only in case (e.g. drive-letter casing from a different code
    // path).
    if !title.is_empty()
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
        _ => "other",
    }
}

impl WtaClient {
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
                    acp::schema::v1::RequestPermissionOutcome::Selected(acp::schema::v1::SelectedPermissionOutcome::new(
                        option_id,
                    )),
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

    async fn session_notification(&self, args: acp::schema::v1::SessionNotification) -> acp::Result<()> {
        let kind = session_update_kind(&args.update);
        // Per-streamed-chunk; trace-only (not via acp_log's debug) so default
        // debug logs aren't flooded with one line per token chunk.
        tracing::trace!(target: "acp", "session_notification: kind={}", kind);
        // The full update carries agent message/thought text, tool-call
        // content, plan bodies, and replayed user-message chunks — trace only.
        acp_trace_content(&format!("session_notification update: {:?}", args.update));
        let sid = args.session_id.0.to_string();
        self.state
            .prompt_timing
            .observe_session_update(&sid, kind);
        match args.update {
            acp::schema::v1::SessionUpdate::UserMessageChunk(chunk) => {
                // Replayed historical user prompt from `session/load`.
                // In the normal prompt flow the agent doesn't emit
                // these (the client sent the user text itself), so
                // this branch only fires during a load replay. The
                // App handler gates on `loading_session` and drops
                // late-arrivers.
                if let acp::schema::v1::ContentBlock::Text(text_content) = chunk.content {
                    let _ = self.state.event_tx.send(AppEvent::UserMessageReplayChunk {
                        session_id: sid,
                        text: text_content.text,
                    });
                }
            }
            acp::schema::v1::SessionUpdate::AgentThoughtChunk(chunk) => {
                if let acp::schema::v1::ContentBlock::Text(text_content) = chunk.content {
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
                self.state
                    .prompt_timing
                    .observe_first_tool_call(&sid, Some(tool_call.title.as_str()));
                let (location, location_is_command) = match tool_call_location_hint(
                    &tool_call.title,
                    &tool_call.locations,
                    tool_call.raw_input.as_ref(),
                ) {
                    Some((text, is_command)) => (Some(text), is_command),
                    None => (None, false),
                };
                let _ = self.state.event_tx.send(AppEvent::ToolCall {
                    session_id: sid,
                    id: tool_call.tool_call_id.to_string(),
                    title: tool_call.title.clone(),
                    status: format!("{:?}", tool_call.status),
                    location,
                    location_is_command,
                });
            }
            acp::schema::v1::SessionUpdate::ToolCallUpdate(update) => {
                if let Some(status) = &update.fields.status {
                    // Failed updates frequently carry a `raw_output.message`
                    // explaining *why* (e.g. Copilot in non-interactive ACP
                    // mode emits `{"code":"rejected","message":"The user
                    // rejected this tool call."}` when permission is auto-
                    // denied). Surface it through the existing status string
                    // so the chat view renders something more useful than a
                    // bare "Failed".
                    let reason = update
                        .fields
                        .raw_output
                        .as_ref()
                        .and_then(|v| v.get("message"))
                        .and_then(|m| m.as_str())
                        .map(|s| s.trim())
                        .filter(|s| !s.is_empty());
                    let status_str = match reason {
                        Some(msg) => format!("{:?}: {}", status, msg),
                        None => format!("{:?}", status),
                    };
                    // Only compute a location when this update actually
                    // carried fresh `locations`/`raw_input` — otherwise send
                    // `None` so `app_events.rs` leaves the card's existing
                    // hint alone instead of blanking it on every status-only
                    // update (e.g. Pending -> InProgress -> Completed).
                    let (location, location_is_command) = if update.fields.locations.is_some()
                        || update.fields.raw_input.is_some()
                    {
                        match tool_call_location_hint(
                            update.fields.title.as_deref().unwrap_or(""),
                            update.fields.locations.as_deref().unwrap_or(&[]),
                            update.fields.raw_input.as_ref(),
                        ) {
                            Some((text, is_command)) => (Some(text), is_command),
                            None => (None, false),
                        }
                    } else {
                        (None, false)
                    };
                    let _ = self.state.event_tx.send(AppEvent::ToolCallUpdate {
                        session_id: sid,
                        id: update.tool_call_id.to_string(),
                        status: status_str,
                        location,
                        location_is_command,
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
                            acp::schema::v1::PlanEntryStatus::Completed => PlanEntryStatus::Completed,
                            acp::schema::v1::PlanEntryStatus::InProgress => PlanEntryStatus::InProgress,
                            _ => PlanEntryStatus::Pending,
                        },
                    })
                    .collect();
                let _ = self.state.event_tx.send(AppEvent::Plan {
                    session_id: sid,
                    entries,
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
                    location,
                    location_is_command: false,
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
                let mut resp = acp::schema::v1::TerminalOutputResponse::new(output.data, false);
                if let Some(code) = output.exit_status {
                    resp = resp.exit_status(acp::schema::v1::TerminalExitStatus::new().exit_code(code));
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
                    status: format!("exited ({})", code),
                    location: None,
                    location_is_command: false,
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

/// The helper-mode ACP client loop. Instead of spawning the agent CLI
/// as a child process and talking ACP over its stdio, this connects to
/// a wta-master singleton over the named pipe whose path is passed in
/// `pipe_name` and speaks ACP over that pipe. The master (from this
/// helper's perspective) plays the role of the agent.
///
/// Wires the App-facing select-loop, minus the
/// restart-loop wrapper: helper mode doesn't own the agent CLI lifetime
/// (master does). `/restart` is delegated to the C++ side via a
/// `restart_agent_stack` `SendEvent`; that path tears down every agent
/// pane, force-restarts master under the same stable pipe name, and
/// re-toggles the active pane so the user lands on a fresh session.
///
/// See doc/specs/Multi-window-agent-pane.md for the helper+master
/// architecture, and `tools/wta/src/master/mod.rs` for the peer.

/// Process-wide owner tab StableId for this helper, seeded once at
/// startup from `--owner-tab-id`. A helper process owns exactly one WT
/// tab for its lifetime, so a `OnceLock` is the right shape: set once in
/// `main()`, read by [`inject_wta_pane_meta`] on every `session/new` /
/// `session/load` so master can record `owner_tab_id` on the routing
/// entry and address `restart_agent_pane` recovery events by StableId.
static HELPER_OWNER_TAB_ID: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

/// Seed the process-wide owner tab StableId. Idempotent — only the first
/// call wins (subsequent calls are ignored), matching the "one tab per
/// helper for its whole life" invariant. Empty/blank ids are stored as
/// `None`.
pub fn set_helper_owner_tab_id(owner_tab_id: Option<&str>) {
    let normalized = owner_tab_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let _ = HELPER_OWNER_TAB_ID.set(normalized);
}

fn helper_owner_tab_id() -> Option<String> {
    HELPER_OWNER_TAB_ID.get().cloned().flatten()
}

/// Inject `_meta.wta.pane_session_id = $WT_SESSION` (lowercased, no
/// braces) and `_meta.wta.owner_tab_id = <this helper's StableId>` into
/// an outbound ACP `session/new` or `session/load` request, when this
/// helper is running inside a Windows Terminal pane.
///
/// Used by the helper-over-master path to tell `wta-master` which WT
/// pane owns the session it's about to create or rehydrate (so focus /
/// session-list resolution works) and which WT tab owns it (so master
/// can drive `restart_agent_pane` recovery). Master records both in
/// `SessionRegistry` / its per-helper recovery map.
///
/// No-op for whichever fields are unavailable: `pane_session_id` when
/// `WT_SESSION` is unset/empty (e.g. running outside a WT pane in
/// tests), `owner_tab_id` when `--owner-tab-id` wasn't supplied.
fn inject_wta_pane_meta(meta: &mut Option<acp::schema::v1::Meta>) {
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
    if pane_session_id.is_none() && owner_tab_id.is_none() {
        return;
    }
    crate::session_registry::inject_wta_meta(
        meta,
        &crate::session_registry::WtaMeta {
            pane_session_id,
            owner_tab_id,
            ..Default::default()
        },
    );
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
    inject_wta_pane_meta(&mut new_req.meta);
    let fallback_started = std::time::Instant::now();
    let fallback = conn.new_session(new_req).await;
    log_acp_new_session_result("HelperPipeFallback", fallback_started, &fallback);
    match fallback {
        Ok(resp) => {
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
    mut restart_rx: mpsc::UnboundedReceiver<RestartRequest>,
    mut session_hook_rx: mpsc::UnboundedReceiver<crate::agent_sessions::SessionEvent>,
    mut master_ext_rx: mpsc::UnboundedReceiver<MasterExtRequest>,
    shell_mgr: Arc<ShellManager>,
    wt_connected: bool,
    post_login_reconnect: bool,
) -> Result<()> {
    let startup_probe = StartupProbe::new();
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
    });

    let client = WtaClient {
        state: state.clone(),
    };

    let builder = acp::Client
        .builder()
        .name("wta-helper")
        .on_receive_request({ let c = client.clone(); move |req: acp::schema::v1::AgentRequest, responder, _cx| { let c = c.clone(); async move {
            use acp::schema::v1::{AgentRequest as Q, ClientResponse as R};
            match req {
                Q::RequestPermissionRequest(a) => conn::respond_enum(responder, c.request_permission(a).await.map(R::RequestPermissionResponse)),
                Q::CreateTerminalRequest(a) => conn::respond_enum(responder, c.create_terminal(a).await.map(R::CreateTerminalResponse)),
                Q::TerminalOutputRequest(a) => conn::respond_enum(responder, c.terminal_output(a).await.map(R::TerminalOutputResponse)),
                Q::WaitForTerminalExitRequest(a) => conn::respond_enum(responder, c.wait_for_terminal_exit(a).await.map(R::WaitForTerminalExitResponse)),
                Q::ReleaseTerminalRequest(a) => conn::respond_enum(responder, c.release_terminal(a).await.map(R::ReleaseTerminalResponse)),
                Q::KillTerminalRequest(a) => conn::respond_enum(responder, c.kill_terminal(a).await.map(R::KillTerminalResponse)),
                _ => responder.respond_with_error(acp::Error::method_not_found()),
            }
        } } }, acp::on_receive_request!())
        .on_receive_notification({ let c = client.clone(); move |notif: acp::schema::v1::AgentNotification, _cx| { let c = c.clone(); async move {
            use acp::schema::v1::AgentNotification as N;
            match notif {
                N::SessionNotification(n) => { let _ = c.session_notification(n).await; }
                N::ExtNotification(n) => { let _ = c.ext_notification(n).await; }
                _ => {}
            }
            Ok(())
        } } }, acp::on_receive_notification!());

    let (conn, handle_io) =
        conn::spawn_client(builder, conn::byte_streams(outgoing, incoming));
    startup_probe.log("ACP client connection created (over pipe)");

    let io_probe = startup_probe.clone();
    let io_event_tx = event_tx.clone();
    tokio::task::spawn_local(async move {
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
        // Either way the transport is dead. Emit an AgentError so the state
        // machine leaves `Connected`, the user sees a clear "connection lost —
        // /restart" line, and autofix stops firing into a dead transport (F3).
        // `session_id: None` → current (only) tab. A near-simultaneous in-flight
        // prompt error is collapsed by the AgentError handler's dedup. On normal
        // shutdown the helper process is being torn down, so this event is moot.
        let _ = io_event_tx.send(AppEvent::AgentError {
            session_id: None,
            failure: AgentFailure::TransportLost,
            message: t!("connection.lost").into_owned(),
        });
    });

    // Initialize — same as the child-process path. We use a 60s timeout
    // here because the first helper to connect to a fresh master may
    // ride along with the master's own agent CLI spawn (especially the
    // npx adapter cold start). After the first init, subsequent inits
    // are fast because master just re-forwards.
    let _ = event_tx.send(AppEvent::ConnectionStage("Initializing ACP...".to_string()));
    startup_probe.log("Initializing ACP (over pipe)");
    let init_started = std::time::Instant::now();
    let init_request = {
        let mut req =
            acp::schema::v1::InitializeRequest::new(acp::schema::ProtocolVersion::V1)
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
                agent_id: agent_id.and_then(|s| {
                    let id = s.trim().to_ascii_lowercase();
                    crate::agent_registry::is_known_id(&id).then_some(id)
                }),
                model: acp_model_override
                    .clone()
                    .filter(|s| !s.trim().is_empty()),
                agent_source: Some(agent_source.kind().to_string()),
                wsl_distro: agent_source.distro().map(str::to_string),
                ..Default::default()
            },
        );
        req
    };
    let init_future = conn.initialize(init_request);
    let init_result =
        tokio::time::timeout(std::time::Duration::from_secs(60), init_future).await;
    log_acp_initialize_timeout_result("HelperPipe", init_started, &init_result);
    let init_resp = init_result
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
            anyhow::anyhow!("initialize over master pipe failed: {}", e)
        })?;
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
        let auth_method_id = init_resp
            .auth_methods
            .first()
            .map(|m| m.id().clone());
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
    match conn.list_sessions(acp::schema::v1::ListSessionsRequest::new()).await {
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
    let cwd = match &agent_source {
        crate::agent_source::AgentSource::Host => std::env::current_dir().unwrap_or_default(),
        crate::agent_source::AgentSource::Wsl { .. } => source_cwd
            .as_deref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("/")),
    };
    let (session_id, available_models, current_model_id, has_bootstrap) =
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
            // Resume is intentionally silent: show the same neutral connecting
            // stage a fresh pane would, never "Resuming session …", so a
            // resumed pane is indistinguishable from a normal connection.
            let _ = event_tx.send(AppEvent::ConnectionStage("Connecting...".to_string()));
            (
                acp::schema::v1::SessionId::new(load_sid.to_string()),
                Vec::<AcpModelInfo>::new(),
                None,
                false,
            )
        } else {
            let _ = event_tx.send(AppEvent::ConnectionStage("Creating session...".to_string()));
            startup_probe.log("Creating session (over pipe)");
            let mut new_session_req = acp::schema::v1::NewSessionRequest::new(cwd.clone());
            inject_wta_pane_meta(&mut new_session_req.meta);
            let new_session_started = std::time::Instant::now();
            let new_session_result = conn.new_session(new_session_req).await;
            log_acp_new_session_result(
                "HelperPipeStartup",
                new_session_started,
                &new_session_result,
            );
            let session = new_session_result.map_err(|e| {
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
            (session_id, available_models, current_model_id, true)
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
            crate::protocol::acp::model_select::apply_session_model(
                &conn,
                session_id.clone(),
                requested_model.clone(),
            )
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "failed to set requested model {}: {}",
                    requested_model,
                    e
                )
            })?;
            startup_probe.log(&format!(
                "ACP session model set to {} (over pipe)",
                requested_model
            ));
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
    // helper mode can't restart in-process — master
    // owns the agent CLI. `/restart` fires a `restart_agent_stack`
    // `SendEvent` to the C++ side; that path force-restarts the whole
    // agent stack (tear down panes → `SharedWta::Restart()` → respawn on
    // the same stable pipe name → re-toggle active pane).
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
                // Helper can't restart the agent CLI in-process — master owns
                // its lifetime, and master itself is a singleton owned by
                // `SharedWta` on the C++ side. Ask the C++ side to do a full
                // force-restart of the agent stack: tear down every agent
                // pane, kill master via `SharedWta::Restart()` (bypassing
                // refcount), respawn master under the same stable pipe name,
                // and re-toggle the active tab's pane. The new wta-helper
                // that gets spawned will reconnect to the new master and
                // the user sees a fresh session.
                //
                // Signal travels: helper → `wtcli publish` (see
                // `wt_protocol_events::send`) → `IProtocolServer::SendEvent`
                // (route `RestartAgentStack`) →
                // `TerminalPage::OnRestartAgentStackRequested`.
                tracing::info!(
                    target: "helper",
                    new_agent = ?req.agent_cmd,
                    "restart requested — asking WT to force-restart the agent stack"
                );
                let evt = serde_json::json!({
                    "type": "event",
                    "method": "restart_agent_stack",
                    "params": {},
                });
                crate::wt_protocol_events::send(evt.to_string());
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
                );
            }
            Some(req) = drop_session_rx.recv() => {
                dispatch_drop_session(req, &conn, &tab_to_session, &template_memo, &cancel_signals);
            }
            Some(req) = rename_session_rx.recv() => {
                dispatch_rename_session(req, &tab_to_session);
            }
            Some(prompt) = prompt_rx.recv() => {
                dispatch_prompt(
                    prompt,
                    &conn,
                    &tab_to_session,
                    &template_memo,
                    &in_flight_tabs,
                    &cancel_signals,
                    &event_tx,
                    &shell_mgr,
                    &prompt_timing,
                    wt_connected,
                    is_agent_pane,
                );
            }
            else => break,
        }
    }

    startup_probe.log("run_acp_client_over_pipe loop ended");
    Ok(())
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
                const BORN_BOUND_TIMEOUT: std::time::Duration =
                    std::time::Duration::from_secs(8);
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
            MasterExtRequest::SetSessionModel { session_id, model } => {
                // Apply to the targeted session, or to every live session
                // this helper owns when no target is given (normally just the
                // one bound to its owner tab). Best-effort: a failure on one
                // session is logged, not fatal — the next prompt still works
                // on the previously-selected model.
                let sessions: Vec<acp::schema::v1::SessionId> = {
                    let g = tab_to_session.lock().await;
                    match &session_id {
                        Some(target) => {
                            g.values().filter(|s| *s == target).cloned().collect()
                        }
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
                        Ok(_) => tracing::info!(
                            target: "acp",
                            session_id = %sid.0,
                            model = %model,
                            "acp-model hot-applied to live session"
                        ),
                        Err(err) => tracing::warn!(
                            target: "acp",
                            session_id = %sid.0,
                            model = %model,
                            error = ?err,
                            "model hot-update failed"
                        ),
                    }
                }
            }
        }
    });
}

/// Resume a historical agent session for a tab via ACP `session/load`
/// (the session-management Enter/Shift+Enter resume path). Cancels and
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
            let _ = conn
                .cancel(acp::schema::v1::CancelNotification::new(old.clone()))
                .await;
        }

        let session_id = acp::schema::v1::SessionId::new(req.session_id.clone());
        let mut load_req = acp::schema::v1::LoadSessionRequest::new(session_id.clone(), cwd.clone());
        // Tell master which WT pane owns the session we're about to
        // rehydrate, so the registry row for the resumed sid carries
        // `pane_session_id = <this pane's GUID>` and cross-helper Focus
        // actions can resolve to a real WT pane. Only the helper path
        // needs this.
        if inject_pane_meta {
            inject_wta_pane_meta(&mut load_req.meta);
        }
        // `session/load` may replay history before returning, so on large
        // session stores the call can take a while; the timeout ceiling
        // keeps us from hanging forever if the agent never responds.
        let load_result = tokio::time::timeout(timeout, conn.load_session(load_req)).await;

        match load_result {
            Ok(Ok(_resp)) => {
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
                // The agent replays past content via session/update
                // notifications that route through the existing
                // session_to_tab map. SessionAttached primes that mapping.
                // load_session/LoadSessionResponse does not carry the
                // per-session model list (only modes); leave the
                // previously-published list alone.
                //
                // Resume is intentionally silent: no "Session loaded" note
                // and no "Resuming…" marker (see the `load_session` handler),
                // so a resumed pane presents exactly like a normal connection.
                let _ = event_tx.send(AppEvent::SessionAttached {
                    tab_id: req.tab_id.clone(),
                    session_id: session_id.to_string(),
                    available_models: Vec::new(),
                    current_model_id: None,
                });
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
            inject_wta_pane_meta(&mut new_session_req.meta);
        }
        let new_session_started = std::time::Instant::now();
        let new_session_result = conn.new_session(new_session_req).await;
        log_acp_new_session_result(log_label, new_session_started, &new_session_result);
        let new_session = match new_session_result {
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
        let (per_tab_models, per_tab_current) =
            crate::protocol::acp::model_select::models_from_new_session(&new_session);

        {
            let mut g = tab_to_session.lock().await;
            g.insert(req.tab_id.clone(), new_sid.clone());
        }

        let _ = event_tx.send(AppEvent::SessionAttached {
            tab_id: req.tab_id.clone(),
            session_id: new_sid.to_string(),
            available_models: per_tab_models,
            current_model_id: per_tab_current,
        });
    });
}

/// Drop a tab's ACP session binding without creating a replacement
/// (Ctrl+C×2 close-pane path). Signals any in-flight prompt for that
/// session to bail out of `conn.prompt().await`, forgets its template
/// memo, and best-effort notifies the agent via `session/cancel`.
/// No-op when the tab holds no session. Called by
/// `run_acp_client_over_pipe`.
fn dispatch_drop_session(
    req: DropSessionRequest,
    conn: &conn::ClientLink,
    tab_to_session: &Arc<tokio::sync::Mutex<HashMap<String, acp::schema::v1::SessionId>>>,
    template_memo: &TemplateMemo,
    cancel_signals: &Arc<std::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>>,
) {
    tracing::info!(
        target: "acp_drop_session",
        tab = %req.tab_id,
        "drop_session requested (no replacement)"
    );
    let conn = conn.clone();
    let tab_to_session = Arc::clone(tab_to_session);
    let template_memo = template_memo.clone();
    let cancel_signals = Arc::clone(cancel_signals);
    tokio::task::spawn_local(async move {
        let old_sid: Option<acp::schema::v1::SessionId> = {
            let mut g = tab_to_session.lock().await;
            g.remove(&req.tab_id)
        };
        if let Some(old) = old_sid {
            // Signal any in-flight prompt for this session to bail out of
            // conn.prompt().await immediately, then send a session/cancel
            // to the agent. Mirrors the new_session cancel path, minus the
            // new_session round-trip.
            let old_str = old.to_string();
            template_memo.forget(&old_str).await;
            if let Some(sig) = cancel_signals.lock().unwrap().remove(&old_str) {
                let _ = sig.send(());
            }
            if let Err(e) = conn
                .cancel(acp::schema::v1::CancelNotification::new(old.clone()))
                .await
            {
                tracing::warn!(
                    target: "acp_drop_session",
                    tab = %req.tab_id,
                    error = ?e,
                    "session/cancel after drop failed (likely unsupported)"
                );
            }
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
fn dispatch_rename_session(
    req: RenameSessionRequest,
    tab_to_session: &Arc<tokio::sync::Mutex<HashMap<String, acp::schema::v1::SessionId>>>,
) {
    let tab_to_session = Arc::clone(tab_to_session);
    tokio::task::spawn_local(async move {
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
    });
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
        content.push(acp::schema::v1::ContentBlock::Image(acp::schema::v1::ImageContent::new(
            image.data_base64.clone(),
            image.mime_type.clone(),
        )));
    }
    content
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
    wt_connected: bool,
    is_agent_pane: bool,
) {
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
            return;
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
    let tab_key_task = tab_key.clone();

    tokio::task::spawn_local(dispatch_prompt_body(
        prompt,
        conn_task,
        tab_to_session_task,
        template_memo_task,
        in_flight_tabs_task,
        cancel_signals_task,
        event_tx_task,
        shell_mgr_task,
        prompt_timing_task,
        tab_key_task,
        wt_connected,
        is_agent_pane,
    ));
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
    tab_key_task: String,
    wt_connected: bool,
    is_agent_pane: bool,
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
            let new_session_result = conn_task
                .new_session(acp::schema::v1::NewSessionRequest::new(cwd))
                .await;
            log_acp_new_session_result(
                "LazyCreateOnFirstPrompt",
                new_session_started,
                &new_session_result,
            );
            let new_session = match new_session_result {
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
            let (per_tab_models, per_tab_current) =
                crate::protocol::acp::model_select::models_from_new_session(&new_session);
            let _ = event_tx_task.send(AppEvent::SessionAttached {
                tab_id: tab_key_task.clone(),
                session_id: new_sid.to_string(),
                available_models: per_tab_models,
                current_model_id: per_tab_current,
            });
            g.insert(tab_key_task.clone(), new_sid.clone());
            new_sid
        }
    };
    let prompt_session_id_str = prompt_session_id.to_string();

    let kind = if prompt.is_autofix {
        TemplateKind::Autofix
    } else {
        TemplateKind::Planner
    };
    let include_template = template_memo
        .should_ship(&prompt_session_id_str, kind)
        .await;

    prompt_timing_task.activate(
        &prompt_session_id_str,
        prompt.id,
        &prompt.text,
        prompt.submitted_at_unix_s,
    );
    let (text, prompt_source, prompt_name, resolved_fix_pane) = build_prompt_text(
        prompt.id,
        prompt.submitted_at_unix_s,
        &prompt.text,
        prompt.is_autofix,
        include_template,
        &shell_mgr_task,
        wt_connected,
        prompt.pane_context.as_ref(),
    )
    .await;
    // A manual `/fix` resolved its working pane in build_prompt_text (it had no
    // explicit source pane). Plumb it back so the App fills the turn's
    // `target_pane_id`; the host fills `Send.parent` from it at execute time.
    if let Some(pane_id) = resolved_fix_pane {
        let _ = event_tx_task.send(AppEvent::AutofixTargetResolved {
            tab_id: prompt
                .pane_context
                .as_ref()
                .and_then(|c| c.tab_id.clone()),
            prompt_id: prompt.id,
            pane_id,
        });
    }
    let _ = event_tx_task.send(AppEvent::PromptTemplateLoaded { name: prompt_name });
    prompt_timing_task.mark_context_ready(&prompt_session_id_str, text.len());
    acp_log_built_prompt(
        &prompt.text,
        prompt.pane_context.as_ref(),
        &prompt_source,
        &text,
    );
    log_turn_trace(
        prompt.id,
        &prompt_session_id_str,
        kind,
        include_template,
        &text,
    );
    prompt_timing_task.mark_prompt_sent(&prompt_session_id_str);

    // Telemetry: prompt dispatched over ACP. WTA emits `AgentPromptSent`
    // for the agent-pane prompt-entry route; the C++ side emits
    // `CommandPaletteDispatchedAgentPrompt` for the `?<prompt>` delegation
    // route under the same provider.
    crate::telemetry::log_agent_prompt_sent(
        &prompt_session_id_str,
        u32::try_from(text.len()).unwrap_or(u32::MAX),
        prompt.is_autofix,
        match kind {
            TemplateKind::Autofix => "Autofix",
            TemplateKind::Planner => "Planner",
        },
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

    let cancelled = tokio::select! {
        result = &mut prompt_fut => {
            // Peek the successful turn's stop_reason (the response is consumed
            // by `complete_prompt_request`). A soft stop is not an error; the
            // Err arm is classified separately by `from_acp_error`.
            let soft_stop = result
                .as_ref()
                .ok()
                .and_then(|resp| SoftStopReason::from_stop_reason(resp.stop_reason));
            complete_prompt_request(
                result,
                soft_stop,
                &prompt_timing_task,
                &event_tx_task,
                prompt_session_id_str.clone(),
            )
            .await;
            false
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
            true
        }
    };
    // Drop the in-flight prompt future eagerly when cancelled to
    // release the connection slot for the next prompt on this tab.
    drop(prompt_fut);
    let _ = cancelled;

    cancel_signals_task
        .lock()
        .unwrap()
        .remove(&prompt_session_id_str);
    in_flight_tabs_task.lock().unwrap().remove(&tab_key_task);
}

#[cfg(test)]
mod tests {
    use super::{
        acp_result_failure_fields, complete_prompt_request, inject_wta_pane_meta,
        post_login_authenticate_error, timeout_result_failure_fields, tool_call_kind_label,
        PromptTimingState, SoftStopReason,
    };
    use super::acp;
    use crate::protocol::acp::failure::{AgentFailure, HandshakeStage};
    use crate::app_contracts::AppEvent;
    use tokio::sync::mpsc;

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
        let failure = crate::protocol::acp::failure::classify_anyhow(
            &err,
            HandshakeStage::Authenticate,
        );
        assert!(
            matches!(failure, AgentFailure::AuthRequired { .. }),
            "AuthRequired from post-login authenticate should stay recoverable, got {failure:?}"
        );
    }

    #[test]
    fn post_login_authenticate_non_auth_stays_authenticate_handshake_failure() {
        let err = post_login_authenticate_error(
            "copilot-login",
            &acp::Error::new(-32603, "boom"),
        );
        let failure = crate::protocol::acp::failure::classify_anyhow(
            &err,
            HandshakeStage::Authenticate,
        );
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
        inject_wta_pane_meta(&mut meta);
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

        inject_wta_pane_meta(&mut req.meta);

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
        assert_eq!(timeout_result_failure_fields(&inner_err), ("AcpError", -32000));

        // Outer future elapsed → Timeout, no ACP code.
        let elapsed = tokio::time::timeout(
            std::time::Duration::ZERO,
            std::future::pending::<()>(),
        )
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
        assert!(matches!(content[1], acp::schema::v1::ContentBlock::Image(_)));
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
