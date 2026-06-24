use super::failure::AgentFailure;
use super::prompt;
use super::soft_stop::SoftStopReason;
use acp::Agent as _;
use agent_client_protocol as acp;
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use tokio::sync::mpsc;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::app::{AppEvent, PermOption, PlanEntry, PlanEntryStatus};
use crate::coordinator::default_supported_delegate_agents;
use crate::pane_context::PaneContext;
use crate::shell::{ShellManager, TerminalConfig};

const ACTIVE_PANE_CONTEXT_MAX_CHARS: usize = 4000;

// Form A mock-ACP-agent harness + scenario tests (in-process, deterministic).
// Lives as a sibling file so it stays out of this large module, but is a child
// of `client` so it can reach the private `WtaClient` / `ClientState`.
// `pub(crate)` so app-module tests can borrow `connect_mock_agent` and assert
// on App state.
#[cfg(test)]
#[path = "mock_agent_tests.rs"]
pub(crate) mod mock_agent_tests;

/// Which prompt template was last shipped on a given ACP session.
/// Used by [`TemplateMemo`] to decide whether the next turn needs to
/// re-include the (~10k char) template body or can ride on the
/// persona already installed in the session's conversation history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TemplateKind {
    Planner,
    Autofix,
}

impl std::fmt::Display for TemplateKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TemplateKind::Planner => f.write_str("planner"),
            TemplateKind::Autofix => f.write_str("autofix"),
        }
    }
}

/// Per-session memo of the last shipped template kind.
///
/// Each ACP session has its own conversation history with the agent.
/// We pay the ~10k-char template body once on the first turn of a
/// session; subsequent turns only carry runtime context + the user
/// request, because the planner persona is already in history. When
/// the kind changes (planner ↔ autofix) we re-ship so the model's
/// most-recent system instructions match the turn's intent.
///
/// Cleanup is driven by the session lifecycle: `forget()` runs
/// whenever a SessionId is dropped (via `/new` or `drop_session_rx`),
/// keeping the map bounded.
#[derive(Clone, Default)]
struct TemplateMemo(Arc<tokio::sync::Mutex<HashMap<String, TemplateKind>>>);

impl TemplateMemo {
    /// Records `kind` as the latest template for this session and
    /// returns whether the caller must ship the template body on this
    /// turn. Autofix always ships (its template *is* the prompt body);
    /// planner ships on the first turn or when the previous turn used
    /// the other kind.
    async fn should_ship(&self, session_id: &str, kind: TemplateKind) -> bool {
        let prev = self.0.lock().await.insert(session_id.to_string(), kind);
        kind == TemplateKind::Autofix || prev != Some(kind)
    }

    /// Drops the memo entry for a session that's going away.
    async fn forget(&self, session_id: &str) {
        self.0.lock().await.remove(session_id);
    }
}

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
    },
    SessionResumeDispatched {
        request_id: u64,
        sid: acp::SessionId,
    },
    SessionFocus {
        request_id: u64,
        sid: acp::SessionId,
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
        session_id: Option<acp::SessionId>,
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
        }
    }

    pub fn preview(&self) -> String {
        prompt_preview(&self.text)
    }
}

fn now_unix_s() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn prompt_preview(text: &str) -> String {
    const MAX_CHARS: usize = 80;

    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let escaped = normalized.replace('\n', "\\n");
    let mut preview = String::new();
    let mut chars = escaped.chars();
    for _ in 0..MAX_CHARS {
        match chars.next() {
            Some(ch) => preview.push(ch),
            None => return preview,
        }
    }

    if chars.next().is_some() {
        preview.push_str("...");
    }

    preview
}

fn format_elapsed(start: Option<f64>, end: Option<f64>) -> String {
    match (start, end) {
        (Some(start), Some(end)) if end >= start => format!("{:.3}s", end - start),
        _ => "n/a".to_string(),
    }
}

fn first_visible_text_gap(
    first_event_at_unix_s: Option<f64>,
    first_stdout_byte_at_unix_s: Option<f64>,
    first_text_at_unix_s: Option<f64>,
) -> (String, &'static str) {
    if first_event_at_unix_s.is_some() {
        return (
            format_elapsed(first_event_at_unix_s, first_text_at_unix_s),
            "first_event",
        );
    }

    if first_stdout_byte_at_unix_s.is_some() {
        return (
            format_elapsed(first_stdout_byte_at_unix_s, first_text_at_unix_s),
            "first_transport_read",
        );
    }

    ("n/a".to_string(), "n/a")
}

fn final_timing_note(
    submitted_at_unix_s: f64,
    context_ready_at_unix_s: Option<f64>,
    prompt_sent_at_unix_s: Option<f64>,
    completed_at_unix_s: f64,
) -> String {
    format!(
        "submit->context_ready {} | prompt_sent->options_shown {}",
        format_elapsed(Some(submitted_at_unix_s), context_ready_at_unix_s),
        format_elapsed(prompt_sent_at_unix_s, Some(completed_at_unix_s))
    )
}

pub fn prompt_timing_log(turn_id: u64, submitted_at_unix_s: f64, phase: &str, details: &str) {
    let since_submit = (now_unix_s() - submitted_at_unix_s).max(0.0);
    if details.is_empty() {
        acp_log(&format!(
            "prompt_timing turn={} phase={} since_submit={:.3}s",
            turn_id, phase, since_submit
        ));
    } else {
        acp_log(&format!(
            "prompt_timing turn={} phase={} since_submit={:.3}s {}",
            turn_id, phase, since_submit, details
        ));
    }
}

#[derive(Debug)]
struct ActivePromptTiming {
    id: u64,
    preview: String,
    submitted_at_unix_s: f64,
    received_at_unix_s: Option<f64>,
    context_ready_at_unix_s: Option<f64>,
    prompt_sent_at_unix_s: Option<f64>,
    /// Monotonic counterpart of `prompt_sent_at_unix_s`. Captured at the
    /// same instant in `mark_prompt_sent()`. Used by ETW telemetry to
    /// compute `first_token_latency_ms` / `total_duration_ms` so the
    /// emitted durations are immune to wall-clock jumps (NTP, DST,
    /// manual time adjustments) — `SystemTime` deltas could otherwise go
    /// negative or skew aggregates.
    prompt_sent_at_mono: Option<std::time::Instant>,
    first_stdin_write_at_unix_s: Option<f64>,
    bytes_written_after_prompt: u64,
    first_stdout_byte_at_unix_s: Option<f64>,
    bytes_read_after_prompt: u64,
    first_event_at_unix_s: Option<f64>,
    first_event_kind: Option<String>,
    first_text_at_unix_s: Option<f64>,
    first_tool_call_at_unix_s: Option<f64>,
    first_permission_at_unix_s: Option<f64>,
    permission_requested_at_unix_s: Option<f64>,
    permission_wait_total_s: f64,
    event_count: u64,
}

/// Concurrent-prompt-aware timing tracker. With M3's spawn-per-prompt
/// model, multiple prompts can be in flight at the same time; each is
/// keyed by its ACP `SessionId`. Byte-level observers (writes/reads on
/// the shared stdio) update every in-flight prompt that hasn't yet
/// recorded its first byte — `is_none()` guards make that a no-op
/// once a value is set.
#[derive(Default)]
struct PromptTimingState {
    active: Mutex<HashMap<String, ActivePromptTiming>>,
}

impl PromptTimingState {
    fn activate(&self, session_id: &str, prompt: &PromptSubmission) {
        let now = now_unix_s();
        let preview = prompt.preview();
        let mut active = self.active.lock().unwrap();
        active.insert(
            session_id.to_string(),
            ActivePromptTiming {
                id: prompt.id,
                preview: preview.clone(),
                submitted_at_unix_s: prompt.submitted_at_unix_s,
                received_at_unix_s: Some(now),
                context_ready_at_unix_s: None,
                prompt_sent_at_unix_s: None,
                prompt_sent_at_mono: None,
                first_stdin_write_at_unix_s: None,
                bytes_written_after_prompt: 0,
                first_stdout_byte_at_unix_s: None,
                bytes_read_after_prompt: 0,
                first_event_at_unix_s: None,
                first_event_kind: None,
                first_text_at_unix_s: None,
                first_tool_call_at_unix_s: None,
                first_permission_at_unix_s: None,
                permission_requested_at_unix_s: None,
                permission_wait_total_s: 0.0,
                event_count: 0,
            },
        );
        drop(active);

        prompt_timing_log(
            prompt.id,
            prompt.submitted_at_unix_s,
            "prompt_received",
            &format!(
                "queue_delay={}",
                format_elapsed(Some(prompt.submitted_at_unix_s), Some(now)),
            ),
        );
        // User prompt preview — trace only.
        acp_trace_content(&format!("turn {} preview={:?}", prompt.id, preview));
    }

    fn mark_context_ready(&self, session_id: &str, prompt_len: usize) {
        let now = now_unix_s();
        let mut guard = self.active.lock().unwrap();
        if let Some(active) = guard.get_mut(session_id) {
            active.context_ready_at_unix_s = Some(now);
            let turn_id = active.id;
            let submitted_at_unix_s = active.submitted_at_unix_s;
            let details = format!(
                "prompt_len={} context_build={}",
                prompt_len,
                format_elapsed(active.received_at_unix_s, Some(now))
            );
            drop(guard);
            prompt_timing_log(turn_id, submitted_at_unix_s, "context_ready", &details);
        }
    }

    fn mark_prompt_sent(&self, session_id: &str) {
        let now = now_unix_s();
        let mut guard = self.active.lock().unwrap();
        if let Some(active) = guard.get_mut(session_id) {
            active.prompt_sent_at_unix_s = Some(now);
            active.prompt_sent_at_mono = Some(std::time::Instant::now());
            let turn_id = active.id;
            let submitted_at_unix_s = active.submitted_at_unix_s;
            let details = format!(
                "after_context_ready={}",
                format_elapsed(active.context_ready_at_unix_s, Some(now))
            );
            drop(guard);
            prompt_timing_log(turn_id, submitted_at_unix_s, "prompt_sent", &details);
        }
    }

    fn observe_session_update(&self, session_id: &str, kind: &str) {
        let now = now_unix_s();
        let mut guard = self.active.lock().unwrap();
        if let Some(active) = guard.get_mut(session_id) {
            active.event_count += 1;
            if active.first_event_at_unix_s.is_none() {
                active.first_event_at_unix_s = Some(now);
                active.first_event_kind = Some(kind.to_string());
                let turn_id = active.id;
                let submitted_at_unix_s = active.submitted_at_unix_s;
                let details = format!(
                    "event_kind={} since_prompt_sent={}",
                    kind,
                    format_elapsed(active.prompt_sent_at_unix_s, Some(now))
                );
                drop(guard);
                prompt_timing_log(turn_id, submitted_at_unix_s, "first_event", &details);
            }
        }
    }

    fn observe_first_text(&self, session_id: &str, text_len: usize) {
        let now = now_unix_s();
        let mut guard = self.active.lock().unwrap();
        if let Some(active) = guard.get_mut(session_id) {
            if active.first_text_at_unix_s.is_none() {
                active.first_text_at_unix_s = Some(now);
                let (visible_gap, visible_gap_source) = first_visible_text_gap(
                    active.first_event_at_unix_s,
                    active.first_stdout_byte_at_unix_s,
                    Some(now),
                );
                let turn_id = active.id;
                let submitted_at_unix_s = active.submitted_at_unix_s;
                let prompt_sent_at = active.prompt_sent_at_unix_s;
                let prompt_sent_at_mono = active.prompt_sent_at_mono;
                let details = format!(
                    "text_len={} since_prompt_sent={} first_visible_text_gap={} gap_source={}",
                    text_len,
                    format_elapsed(prompt_sent_at, Some(now)),
                    visible_gap,
                    visible_gap_source
                );
                drop(guard);
                prompt_timing_log(turn_id, submitted_at_unix_s, "first_text", &details);

                // Telemetry: agent's first text chunk arrived. Time-to-first-token
                // is the key responsiveness metric — emit only when we can
                // compute it reliably (i.e. we observed `prompt_sent_at_mono`).
                // Use the monotonic `Instant` captured at the same moment as
                // `prompt_sent_at_unix_s` so the latency is immune to wall-clock
                // jumps (NTP/DST) that could otherwise produce a negative delta
                // we'd silently drop, skewing the aggregate.
                if let Some(sent_mono) = prompt_sent_at_mono {
                    let first_token_latency_ms =
                        sent_mono.elapsed().as_secs_f64() * 1000.0;
                    crate::telemetry::log_agent_response_first_token(
                        session_id,
                        first_token_latency_ms,
                        u32::try_from(text_len).unwrap_or(u32::MAX),
                    );
                }
            }
        }
    }

    fn observe_first_tool_call(&self, session_id: &str, title: Option<&str>) {
        let now = now_unix_s();
        let mut guard = self.active.lock().unwrap();
        if let Some(active) = guard.get_mut(session_id) {
            if active.first_tool_call_at_unix_s.is_none() {
                active.first_tool_call_at_unix_s = Some(now);
                let turn_id = active.id;
                let submitted_at_unix_s = active.submitted_at_unix_s;
                let title_preview = title.map(prompt_preview).unwrap_or_default();
                let details = format!(
                    "since_prompt_sent={}",
                    format_elapsed(active.prompt_sent_at_unix_s, Some(now))
                );
                drop(guard);
                prompt_timing_log(turn_id, submitted_at_unix_s, "first_tool_call", &details);
                // Tool-call title is agent-generated content — trace only.
                acp_trace_content(&format!("turn {turn_id} first_tool_call title={title_preview:?}"));
            }
        }
    }

    fn permission_requested(&self, session_id: &str, description: &str) {
        let now = now_unix_s();
        let mut guard = self.active.lock().unwrap();
        if let Some(active) = guard.get_mut(session_id) {
            if active.first_permission_at_unix_s.is_none() {
                active.first_permission_at_unix_s = Some(now);
            }
            active.permission_requested_at_unix_s = Some(now);
            let turn_id = active.id;
            let submitted_at_unix_s = active.submitted_at_unix_s;
            let details = format!(
                "since_prompt_sent={}",
                format_elapsed(active.prompt_sent_at_unix_s, Some(now))
            );
            drop(guard);
            prompt_timing_log(
                turn_id,
                submitted_at_unix_s,
                "permission_requested",
                &details,
            );
            // Permission description is agent-generated content — trace only.
            acp_trace_content(&format!(
                "turn {turn_id} permission_requested description={:?}",
                prompt_preview(description)
            ));
        }
    }

    fn permission_resolved(&self, session_id: &str, outcome: &str) {
        let now = now_unix_s();
        let mut guard = self.active.lock().unwrap();
        if let Some(active) = guard.get_mut(session_id) {
            let wait_s = active
                .permission_requested_at_unix_s
                .map(|start| (now - start).max(0.0))
                .unwrap_or(0.0);
            active.permission_wait_total_s += wait_s;
            active.permission_requested_at_unix_s = None;
            let turn_id = active.id;
            let submitted_at_unix_s = active.submitted_at_unix_s;
            drop(guard);
            prompt_timing_log(
                turn_id,
                submitted_at_unix_s,
                "permission_resolved",
                &format!("outcome={} wait={:.3}s", outcome, wait_s),
            );
        }
    }

    fn complete(&self, session_id: &str, success: bool, error: Option<&str>) -> Option<String> {
        let now = now_unix_s();
        let mut active = self.active.lock().unwrap();
        let Some(active_prompt) = active.remove(session_id) else {
            return None;
        };
        drop(active);

        let (first_visible_text_gap, first_visible_text_gap_source) = first_visible_text_gap(
            active_prompt.first_event_at_unix_s,
            active_prompt.first_stdout_byte_at_unix_s,
            active_prompt.first_text_at_unix_s,
        );

        let mut details = vec![
            format!("success={}", success),
            format!(
                "queue_delay={}",
                format_elapsed(
                    Some(active_prompt.submitted_at_unix_s),
                    active_prompt.received_at_unix_s
                )
            ),
            format!(
                "context_build={}",
                format_elapsed(
                    active_prompt.received_at_unix_s,
                    active_prompt.context_ready_at_unix_s
                )
            ),
            format!(
                "prompt_send_gap={}",
                format_elapsed(
                    active_prompt.context_ready_at_unix_s,
                    active_prompt.prompt_sent_at_unix_s
                )
            ),
            format!(
                "first_transport_write={}",
                format_elapsed(
                    active_prompt.prompt_sent_at_unix_s,
                    active_prompt.first_stdin_write_at_unix_s
                )
            ),
            format!(
                "first_transport_read={}",
                format_elapsed(
                    active_prompt.prompt_sent_at_unix_s,
                    active_prompt.first_stdout_byte_at_unix_s
                )
            ),
            format!(
                "first_event={}",
                format_elapsed(
                    active_prompt.prompt_sent_at_unix_s,
                    active_prompt.first_event_at_unix_s
                )
            ),
            format!(
                "first_event_kind={}",
                active_prompt
                    .first_event_kind
                    .unwrap_or_else(|| "n/a".to_string())
            ),
            format!(
                "first_text={}",
                format_elapsed(
                    active_prompt.prompt_sent_at_unix_s,
                    active_prompt.first_text_at_unix_s
                )
            ),
            format!("first_visible_text_gap={}", first_visible_text_gap),
            format!(
                "first_visible_text_gap_source={}",
                first_visible_text_gap_source
            ),
            format!(
                "first_tool_call={}",
                format_elapsed(
                    active_prompt.prompt_sent_at_unix_s,
                    active_prompt.first_tool_call_at_unix_s
                )
            ),
            format!(
                "first_permission={}",
                format_elapsed(
                    active_prompt.prompt_sent_at_unix_s,
                    active_prompt.first_permission_at_unix_s
                )
            ),
            format!(
                "bytes_out_after_prompt={}",
                active_prompt.bytes_written_after_prompt
            ),
            format!(
                "bytes_in_after_prompt={}",
                active_prompt.bytes_read_after_prompt
            ),
            format!(
                "permission_wait_total={:.3}s",
                active_prompt.permission_wait_total_s
            ),
            format!(
                "prompt_rpc_total={}",
                format_elapsed(active_prompt.prompt_sent_at_unix_s, Some(now))
            ),
            format!(
                "total={}",
                format_elapsed(Some(active_prompt.submitted_at_unix_s), Some(now))
            ),
            format!("event_count={}", active_prompt.event_count),
        ];

        if let Some(error) = error {
            details.push(format!("error={:?}", error));
        }

        prompt_timing_log(
            active_prompt.id,
            active_prompt.submitted_at_unix_s,
            "prompt_complete",
            &details.join(" "),
        );
        // User prompt preview — trace only.
        acp_trace_content(&format!(
            "turn {} complete preview={:?}",
            active_prompt.id, active_prompt.preview
        ));

        // Telemetry: emit the prompt-complete signal with aggregate metrics.
        // Use the monotonic `Instant` (captured alongside `prompt_sent_at_unix_s`
        // in `mark_prompt_sent`) so `total_duration_ms` is wall-clock-jump-
        // immune. Skip emission when the monotonic anchor is missing rather
        // than reporting 0ms, mirroring the first-token guard.
        if let Some(sent_mono) = active_prompt.prompt_sent_at_mono {
            let total_duration_ms = sent_mono.elapsed().as_secs_f64() * 1000.0;
            crate::telemetry::log_agent_response_complete(
                session_id,
                total_duration_ms,
                active_prompt.bytes_read_after_prompt as u64,
                success,
            );
        }

        Some(final_timing_note(
            active_prompt.submitted_at_unix_s,
            active_prompt.context_ready_at_unix_s,
            active_prompt.prompt_sent_at_unix_s,
            now,
        ))
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

fn truncate_for_prompt(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let truncated: String = text.chars().take(max_chars).collect();
        format!("{truncated}\n...<truncated>")
    }
}

fn format_pane_context_summary(pane_context: Option<&PaneContext>) -> String {
    match pane_context {
        Some(context) => format!(
            "pane_id={:?} tab_id={:?} window_id={:?} source_pane_id={:?} effective_source_pane_id={:?}",
            context.pane_id,
            context.tab_id,
            context.window_id,
            context.source_pane_id,
            context.effective_source_pane_id(),
        ),
        None => "none".to_string(),
    }
}

fn json_str_or_num(value: Option<&serde_json::Value>) -> Option<String> {
    match value {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

/// Read the most recent shell-integration command (prompt + command + output)
/// for `pane_id`. Falls back to a line-count read when shell integration is
/// not active (e.g. CMD, plain bash without OSC 133 support).
///
/// Returns the (possibly truncated) content as a string. `None` on failure.
///
/// Emits structured tracing under target `acp.last_message` so the call chain
/// is visible in `wta-{process}.log`:
///   * `last_message_request`  — start, with pane_id and budgets
///   * `last_message_result`   — outcome: marks_hit | fallback_used | empty
async fn read_pane_last_message(
    shell_mgr: &ShellManager,
    pane_id: &str,
    fallback_lines: u32,
    max_chars: usize,
) -> Option<String> {
    let started = std::time::Instant::now();
    tracing::debug!(
        target: "acp.last_message",
        pane_id,
        fallback_lines,
        max_chars,
        "last_message_request"
    );

    let mark_call_started = std::time::Instant::now();
    let mark_result = shell_mgr.wt_read_last_prompt(pane_id).await;
    let mark_call_ms = mark_call_started.elapsed().as_millis() as u64;

    match &mark_result {
        Ok(value) => {
            let has_marks = value
                .get("has_marks")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let raw_len = value
                .get("content")
                .and_then(|c| c.as_str())
                .map(str::len)
                .unwrap_or(0);
            tracing::debug!(
                target: "acp.last_message",
                pane_id,
                has_marks,
                raw_len,
                rpc_ms = mark_call_ms,
                "last_message_rpc_ok"
            );
            if has_marks {
                if let Some(content) = value.get("content").and_then(|c| c.as_str()) {
                    if !content.is_empty() {
                        let truncated = truncate_for_prompt(content, max_chars);
                        tracing::debug!(
                            target: "acp.last_message",
                            pane_id,
                            path = "marks_hit",
                            out_len = truncated.len(),
                            total_ms = started.elapsed().as_millis() as u64,
                            "last_message_result"
                        );
                        return Some(truncated);
                    }
                }
            }
        }
        Err(err) => {
            tracing::debug!(
                target: "acp.last_message",
                pane_id,
                rpc_ms = mark_call_ms,
                error = %err,
                "last_message_rpc_err"
            );
        }
    }

    // Fallback: shell integration absent or call failed — use line-count read.
    let fb_started = std::time::Instant::now();
    let result = shell_mgr
        .wt_read_pane_output(pane_id, Some(fallback_lines))
        .await
        .ok()
        .and_then(|value| {
            value
                .get("content")
                .and_then(|content| content.as_str())
                .map(|content| truncate_for_prompt(content, max_chars))
        });
    let fb_ms = fb_started.elapsed().as_millis() as u64;

    match &result {
        Some(text) => tracing::debug!(
            target: "acp.last_message",
            pane_id,
            path = "fallback_used",
            fallback_lines,
            out_len = text.len(),
            fallback_ms = fb_ms,
            total_ms = started.elapsed().as_millis() as u64,
            "last_message_result"
        ),
        None => tracing::debug!(
            target: "acp.last_message",
            pane_id,
            path = "empty",
            fallback_lines,
            fallback_ms = fb_ms,
            total_ms = started.elapsed().as_millis() as u64,
            "last_message_result"
        ),
    }

    result
}

/// Best-effort canonical shell executable for a pid — e.g. `pwsh.exe`,
/// `powershell.exe`, `cmd.exe`, `bash.exe`, `wsl.exe`. Unlike the WT profile
/// *name* (which the user can rename), this is the actual running process, so
/// the agent can reliably pick shell syntax. Returns the file name only;
/// `None` on any failure (or off Windows).
#[cfg(windows)]
fn process_image_name(pid: u32) -> Option<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    if pid == 0 {
        return None;
    }
    // SAFETY: a standard Win32 handle dance. The handle from OpenProcess is
    // closed on every return path; the buffer is sized up front and the
    // written length comes back in `size`.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        // Not MAX_PATH: QueryFullProcessImageNameW can return paths longer than
        // 260 for processes under long roots (WindowsApps installs, `\\?\`
        // extended paths). Use the extended-length max so a valid pid never
        // silently drops the `shell` field. Heap-allocated to keep it off the
        // (smaller) task stack.
        let mut size: u32 = 32768;
        let mut buf = vec![0u16; size as usize];
        let ok =
            QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut size);
        CloseHandle(handle);
        if ok == 0 || size == 0 {
            return None;
        }
        let full = String::from_utf16_lossy(&buf[..size as usize]);
        full.rsplit(['\\', '/'])
            .next()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    }
}

#[cfg(not(windows))]
fn process_image_name(_pid: u32) -> Option<String> {
    None
}

/// Resolve the shell identity for an active-pane JSON object. The agent gets
/// this as the `shell` field — the shell-type signal that drives PowerShell vs
/// bash vs cmd syntax in any fix command it suggests.
///
/// Resolution order:
///   1. The `shell` field reported by shell integration via `OSC 9001;ShellType`
///      (e.g. `pwsh`, `powershell`, `bash`, `wsl:Ubuntu`). This is the only
///      signal that survives a nested shell — `pwsh` → `wsl` → `exit` reports
///      `wsl:<distro>` while inside WSL and `pwsh` again after exit, because the
///      shell re-emits it on every prompt. The pid-based fallback below can't
///      see this: the pane's host process stays `wsl.exe`/`pwsh.exe` regardless
///      of which shell is actually drawing the prompt.
///   2. Otherwise, the canonical shell exe from the pane's `pid` (covers panes
///      without shell integration installed, or before the first prompt).
fn shell_from_active(active: &serde_json::Value) -> Option<String> {
    if let Some(shell) = active
        .get("shell")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(shell.to_string());
    }
    active
        .get("pid")
        .and_then(|v| v.as_u64())
        .and_then(|pid| process_image_name(pid as u32))
}

async fn build_terminal_context_json(shell_mgr: &ShellManager) -> Option<String> {
    // WT's GetActivePane already resolves the agent pane to the user's working
    // pane (the "source"), so a single active-pane query gives us the right
    // target. Pane IDs are process-globally unique, so we only need the pane
    // id itself — tab/window aren't needed for addressing.
    let active = shell_mgr.wt_get_active_pane().await.ok()?;

    let is_agent = active
        .get("is_agent_pane")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if is_agent {
        return None;
    }

    let target_pane_id = json_str_or_num(active.get("session_id"))?;
    let target_window_title = active
        .get("title")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let target_cwd = active
        .get("cwd")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    // Canonical shell exe (pwsh.exe / cmd.exe / wsl.exe …) from the pane's pid.
    // Load-bearing for the planner: any `send` action it emits has to match the
    // active pane's shell syntax (`Get-ChildItem` vs `ls`, `Set-Location` vs
    // `cd`, etc.). We use the real process rather than the WT profile name,
    // which the user can rename.
    let target_shell = shell_from_active(&active);

    tracing::debug!(
        target: "acp.terminal_context",
        target_pane_id = %target_pane_id,
        shell = ?target_shell,
        "terminal_context_target_resolved"
    );

    let buffer = read_pane_last_message(
        shell_mgr,
        &target_pane_id,
        24,
        ACTIVE_PANE_CONTEXT_MAX_CHARS,
    )
    .await;

    serde_json::to_string(&serde_json::json!({
        "activeTarget": target_pane_id,
        "window_title": target_window_title,
        "cwd": target_cwd,
        "shell": target_shell,
        "locale": user_locale_tag(),
        "buffer": buffer,
    }))
    .ok()
}

/// User's UI locale as a BCP-47 tag, suitable for embedding in
/// runtime context JSON shipped to the agent.
///
/// Pseudo-locales (`qps-ploc*`) are passed through verbatim. Unlike
/// `LANG`/`LC_ALL` in `spawn.rs` — which feed libc and have to be real
/// POSIX locales — this field is just metadata for an LLM, which will
/// either recognise the tag or treat it as opaque text. Either way it's
/// honest: it reflects exactly what the user picked in the UI.
fn user_locale_tag() -> String {
    rust_i18n::locale().to_string()
}

async fn build_prompt_text(
    prompt_id: u64,
    submitted_at_unix_s: f64,
    user_text: &str,
    is_autofix: bool,
    include_template: bool,
    shell_mgr: &ShellManager,
    wt_connected: bool,
    pane_context: Option<&PaneContext>,
) -> (String, String, String, Option<String>) {
    let total_started = std::time::Instant::now();
    let mut runtime_sections = Vec::new();
    // Working pane resolved from the active pane for a manual `/fix` (one with
    // no explicit `source_pane_id`). Plumbed back to the App so it can fill
    // `AutofixContext.target_pane_id` — empty otherwise (auto-fix carries its
    // failing pane explicitly; planner turns let the agent fill `Send.parent`).
    let mut resolved_fix_pane: Option<String> = None;

    let template_started = std::time::Instant::now();
    let planner_template = if is_autofix {
        prompt::load_autofix_prompt_template()
    } else {
        prompt::load_planner_prompt_template()
    };
    prompt_timing_log(
        prompt_id,
        submitted_at_unix_s,
        "planner_template_ready",
        &format!(
            "name={:?} source={} dt={:.3}s",
            planner_template.display_name,
            planner_template.source_label,
            template_started.elapsed().as_secs_f64()
        ),
    );

    if !is_autofix {
        // Full planner prompt: include delegate agents and terminal layout.
        let agents_started = std::time::Instant::now();
        let supported_agents_json = serde_json::to_string(&default_supported_delegate_agents())
            .unwrap_or_else(|_| "[]".to_string());
        runtime_sections.push(format!(
            "### Supported Delegate Agents\n```json\n{}\n```",
            supported_agents_json
        ));
        prompt_timing_log(
            prompt_id,
            submitted_at_unix_s,
            "delegate_agents_ready",
            &format!("dt={:.3}s", agents_started.elapsed().as_secs_f64()),
        );

        if wt_connected {
            let terminal_context_started = std::time::Instant::now();
            let terminal_context_json = build_terminal_context_json(shell_mgr).await;
            prompt_timing_log(
                prompt_id,
                submitted_at_unix_s,
                "terminal_context_ready",
                &format!(
                    "present={} dt={:.3}s",
                    terminal_context_json.is_some(),
                    terminal_context_started.elapsed().as_secs_f64()
                ),
            );
            if let Some(terminal_context_json) = terminal_context_json {
                runtime_sections.push(format!(
                    "### Terminal Context JSON\n```json\n{}\n```",
                    terminal_context_json
                ));
            }
        } else {
            prompt_timing_log(
                prompt_id,
                submitted_at_unix_s,
                "terminal_context_skipped",
                "wt_connected=false",
            );
        }
    } else {
        // Auto-fix prompt: read the source pane buffer + a small shell-context
        // header so the agent can choose PowerShell vs bash vs cmd syntax for
        // any file-edit fix it suggests.
        if wt_connected {
            // Resolve the active pane once: it supplies the shell-context
            // header and — for a manual `/fix`, which carries no explicit
            // `source_pane_id` — doubles as the source-pane resolver. WT's
            // GetActivePane already maps the agent pane to the user's working
            // pane, so this is the same target the error-triggered path gets
            // from its notification.
            let active = shell_mgr.wt_get_active_pane().await.ok();

            // Shell context — best-effort. The canonical shell exe (from the
            // pane's pid) tells the agent which syntax the fix command must use.
            if let Some(active) = active.as_ref() {
                let cwd = active
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // Canonical shell exe from the pane's pid — see `shell_from_active`.
                let shell = shell_from_active(active);
                let json = serde_json::to_string(&serde_json::json!({
                    "shell": shell,
                    "cwd": cwd,
                    "locale": user_locale_tag(),
                }))
                .unwrap_or_else(|_| "{}".to_string());
                runtime_sections.push(format!("### Shell Context\n```json\n{}\n```", json));
            }

            // Explicit source pane (error-triggered autofix) wins; otherwise
            // fall back to the resolved active working pane (`/fix`). An
            // active pane that is itself an agent pane is skipped — there's
            // no terminal output to read there.
            let explicit_source = pane_context.and_then(|ctx| ctx.source_pane_id.clone());
            let source_pane_id = explicit_source.clone().or_else(|| {
                active.as_ref().and_then(|a| {
                    let is_agent = a
                        .get("is_agent_pane")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if is_agent {
                        None
                    } else {
                        json_str_or_num(a.get("session_id"))
                    }
                })
            });
            // When we resolved the pane ourselves (manual `/fix`, no explicit
            // source), remember it so the App can fill `target_pane_id` — that
            // is the pane the eventual fix command is sent to.
            if explicit_source.is_none() {
                resolved_fix_pane = source_pane_id.clone();
            }

            if let Some(source_pane_id) = source_pane_id {
                tracing::debug!(
                    target: "acp.terminal_context",
                    source_pane_id = %source_pane_id,
                    // The shell type shipped in `### Shell Context` above; log it
                    // here too so a `/fix` run shows what the agent received.
                    shell = ?active.as_ref().and_then(shell_from_active),
                    mode = "autofix",
                    "terminal_context_target_resolved"
                );
                if let Some(content) = read_pane_last_message(
                    shell_mgr,
                    &source_pane_id,
                    30,
                    ACTIVE_PANE_CONTEXT_MAX_CHARS,
                )
                .await
                {
                    runtime_sections.push(format!("### Terminal Output\n```\n{}\n```", content));
                }
            }
        }
    }

    let assemble_started = std::time::Instant::now();
    // First turn of a session (or kind change): ship the full template
    // body. Subsequent same-kind turns drop the template — the agent
    // already has the persona in its conversation history. Autofix
    // turns always carry the template because the template *is* the
    // prompt body (no user_text alongside it).
    let prompt_body = if include_template {
        prompt::merge_runtime_sections(&planner_template.content, &runtime_sections)
    } else {
        runtime_sections
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    let prompt = if is_autofix {
        // Autofix prompts historically ignored `user_text` — the template +
        // terminal output was the whole prompt. Now a non-empty `user_text` is
        // appended as a `## User Request`: a manual `/fix <hint>` passes the
        // hint here, and the error-triggered path passes its failure summary.
        if user_text.trim().is_empty() {
            prompt_body
        } else {
            format!("{}\n\n## User Request\n{}", prompt_body, user_text)
        }
    } else if prompt_body.is_empty() {
        format!("## User Request\n{}", user_text)
    } else {
        format!("{}\n\n## User Request\n{}", prompt_body, user_text)
    };
    prompt_timing_log(
        prompt_id,
        submitted_at_unix_s,
        "prompt_assembled",
        &format!(
            "assemble_dt={:.3}s total_context_dt={:.3}s prompt_len={} include_template={}",
            assemble_started.elapsed().as_secs_f64(),
            total_started.elapsed().as_secs_f64(),
            prompt.len(),
            include_template
        ),
    );
    (
        prompt,
        planner_template.source_label,
        planner_template.display_name,
        resolved_fix_pane,
    )
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

fn acp_log_built_prompt(
    user_text: &str,
    pane_context: Option<&PaneContext>,
    prompt_source: &str,
    prompt_text: &str,
) {
    tracing::debug!(
        target: "acp",
        user_text_len = user_text.len(),
        pane_context = %format_pane_context_summary(pane_context),
        prompt_source,
        "planner_prompt_begin"
    );
    // Full assembled prompt = user text + captured terminal buffer + cwd.
    // Sensitive — trace only.
    acp_trace_content(&format!("planner_prompt_text:\n{}", prompt_text));
    tracing::debug!(target: "acp", "planner_prompt_end");
}

/// Per-turn audit log: one structured info-level line per round.
///
/// Use this to verify rounds 2+ on a session are "clean" — i.e. the
/// prompt body no longer carries the planner template. Look for
/// `include_template=false` paired with a `body_head` that does NOT
/// start with `# Terminal Agent`.
///
/// Snippets are short on purpose (newlines escaped) so each turn fits
/// on one log line and stays greppable.
fn log_turn_trace(
    prompt_id: u64,
    session_id: &str,
    kind: TemplateKind,
    include_template: bool,
    prompt_text: &str,
) {
    const HEAD_LEN: usize = 200;
    const TAIL_LEN: usize = 150;
    let head = snippet(prompt_text, HEAD_LEN, true);
    let tail = if prompt_text.chars().count() > HEAD_LEN + TAIL_LEN {
        snippet(prompt_text, TAIL_LEN, false)
    } else {
        String::new()
    };
    tracing::info!(
        target: "acp.turn_trace",
        turn = prompt_id,
        session = %session_short(session_id),
        kind = %kind,
        include_template,
        prompt_len = prompt_text.len(),
        "turn_sent"
    );
    // The prompt body snippets carry user text / template content — trace only.
    acp_trace_content(&format!(
        "turn {turn} body_head={head:?} body_tail={tail:?}",
        turn = prompt_id
    ));
}

/// Take `max_chars` from either end of `text` and inline newlines as
/// `\n` so the snippet fits on a single log line.
fn snippet(text: &str, max_chars: usize, from_start: bool) -> String {
    let mut chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let slice: String = if from_start {
        chars.truncate(max_chars.min(len));
        chars.into_iter().collect()
    } else {
        let start = len.saturating_sub(max_chars);
        chars.drain(..start);
        chars.into_iter().collect()
    };
    slice.replace('\n', "\\n")
}

/// Last 8 chars of a SessionId for compact logging.
fn session_short(session_id: &str) -> String {
    let chars: Vec<char> = session_id.chars().collect();
    let start = chars.len().saturating_sub(8);
    chars[start..].iter().collect()
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
struct WtaClient {
    state: Arc<ClientState>,
}

fn session_update_kind(update: &acp::SessionUpdate) -> &'static str {
    match update {
        acp::SessionUpdate::AgentThoughtChunk(_) => "agent_thought_chunk",
        acp::SessionUpdate::AgentMessageChunk(_) => "agent_message_chunk",
        acp::SessionUpdate::ToolCall(_) => "tool_call",
        acp::SessionUpdate::ToolCallUpdate(_) => "tool_call_update",
        acp::SessionUpdate::Plan(_) => "plan",
        _ => "other",
    }
}

#[async_trait::async_trait(?Send)]
impl acp::Client for WtaClient {
    async fn request_permission(
        &self,
        args: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        acp_log("request_permission received");
        // Tool-call title is agent-generated content — trace only.
        acp_trace_content(&format!(
            "request_permission title: {:?}",
            args.tool_call.fields.title
        ));
        let session_id = args.session_id.0.to_string();
        let description = args
            .tool_call
            .fields
            .title
            .clone()
            .unwrap_or_else(|| "Permission requested".to_string());
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

        let _ = self.state.event_tx.send(AppEvent::PermissionRequest {
            session_id: session_id.clone(),
            description,
            options,
            responder: resp_tx,
        });

        // Wait for user to choose
        match resp_rx.await {
            Ok(option_id) => {
                self.state
                    .prompt_timing
                    .permission_resolved(&session_id, "selected");
                Ok(acp::RequestPermissionResponse::new(
                    acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome::new(
                        option_id,
                    )),
                ))
            }
            Err(_) => {
                self.state
                    .prompt_timing
                    .permission_resolved(&session_id, "cancelled");
                Ok(acp::RequestPermissionResponse::new(
                    acp::RequestPermissionOutcome::Cancelled,
                ))
            }
        }
    }

    async fn session_notification(&self, args: acp::SessionNotification) -> acp::Result<()> {
        let kind = session_update_kind(&args.update);
        acp_log(&format!("session_notification: kind={}", kind));
        // The full update carries agent message/thought text, tool-call
        // content, plan bodies, and replayed user-message chunks — trace only.
        acp_trace_content(&format!("session_notification update: {:?}", args.update));
        let sid = args.session_id.0.to_string();
        self.state
            .prompt_timing
            .observe_session_update(&sid, kind);
        match args.update {
            acp::SessionUpdate::UserMessageChunk(chunk) => {
                // Replayed historical user prompt from `session/load`.
                // In the normal prompt flow the agent doesn't emit
                // these (the client sent the user text itself), so
                // this branch only fires during a load replay. The
                // App handler gates on `loading_session` and drops
                // late-arrivers.
                if let acp::ContentBlock::Text(text_content) = chunk.content {
                    let _ = self.state.event_tx.send(AppEvent::UserMessageReplayChunk {
                        session_id: sid,
                        text: text_content.text,
                    });
                }
            }
            acp::SessionUpdate::AgentThoughtChunk(chunk) => {
                if let acp::ContentBlock::Text(text_content) = chunk.content {
                    let _ = self.state.event_tx.send(AppEvent::AgentThoughtChunk {
                        session_id: sid,
                        text: text_content.text,
                    });
                }
            }
            acp::SessionUpdate::AgentMessageChunk(chunk) => {
                if let acp::ContentBlock::Text(text_content) = chunk.content {
                    self.state
                        .prompt_timing
                        .observe_first_text(&sid, text_content.text.len());
                    let _ = self.state.event_tx.send(AppEvent::AgentMessageChunk {
                        session_id: sid,
                        text: text_content.text,
                    });
                }
            }
            acp::SessionUpdate::ToolCall(tool_call) => {
                self.state
                    .prompt_timing
                    .observe_first_tool_call(&sid, Some(tool_call.title.as_str()));
                let _ = self.state.event_tx.send(AppEvent::ToolCall {
                    session_id: sid,
                    id: tool_call.tool_call_id.to_string(),
                    title: tool_call.title.clone(),
                    status: format!("{:?}", tool_call.status),
                });
            }
            acp::SessionUpdate::ToolCallUpdate(update) => {
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
                    let _ = self.state.event_tx.send(AppEvent::ToolCallUpdate {
                        session_id: sid,
                        id: update.tool_call_id.to_string(),
                        status: status_str,
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
        args: acp::CreateTerminalRequest,
    ) -> acp::Result<acp::CreateTerminalResponse> {
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
        match self.state.shell_mgr.create_terminal(config).await {
            Ok(id) => {
                // Show tool-call-like feedback
                let _ = self.state.event_tx.send(AppEvent::ToolCall {
                    session_id,
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
        let session_id = args.session_id.0.to_string();

        match self.state.shell_mgr.wait_for_exit(&tid).await {
            Ok(code) => {
                // Update tool call status
                let _ = self.state.event_tx.send(AppEvent::ToolCallUpdate {
                    session_id,
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
    async fn ext_notification(&self, args: acp::ExtNotification) -> acp::Result<()> {
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
fn inject_wta_pane_meta(meta: &mut Option<acp::Meta>) {
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
        acp::Result<acp::InitializeResponse>,
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
    result: &acp::Result<acp::NewSessionResponse>,
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
    old_sid: Option<&acp::SessionId>,
    tab_id: String,
    cwd: std::path::PathBuf,
    conn: Arc<acp::ClientSideConnection>,
    tab_to_session: Arc<tokio::sync::Mutex<HashMap<String, acp::SessionId>>>,
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
    let mut new_req = acp::NewSessionRequest::new(cwd);
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
        // Backoff schedule, summing to ~75s total. Most masters come
        // up in 1-2s; the long tail is npx adapter cold starts.
        let backoff_ms: &[u64] = &[
            50, 100, 100, 200, 200, 500, 500, 1000, 1000, 2000, 2000, 2000, 5000, 5000, 5000, 5000,
            10000, 10000, 10000, 15000,
        ];
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
                        return Err(anyhow::anyhow!(
                            "connect to master pipe '{}' after {} attempt(s): {}",
                            pipe_name,
                            attempt + 1,
                            e
                        ));
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

    let (conn, handle_io) = acp::ClientSideConnection::new(client, outgoing, incoming, |fut| {
        tokio::task::spawn_local(fut);
    });
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
    let init_future = conn.initialize(
        acp::InitializeRequest::new(acp::ProtocolVersion::V1)
            .client_capabilities(acp::ClientCapabilities::new().terminal(true))
            .client_info(
                acp::Implementation::new("wta-helper", env!("CARGO_PKG_VERSION"))
                    .title("Windows Terminal Agent (helper)"),
            ),
    );
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
                conn.authenticate(acp::AuthenticateRequest::new(method_id.clone())),
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
                    tracing::error!(
                        target: "helper",
                        method_id = %method_id.0,
                        error_code = Into::<i32>::into(e.code),
                        error_message = %e.message,
                        "post-login authenticate failed — agent rejected credentials"
                    );
                    return Err(anyhow::Error::new(AgentFailure::HandshakeFailed {
                        stage: crate::protocol::acp::failure::HandshakeStage::Authenticate,
                        detail: format!(
                            "authenticate({}) failed: {} (code {}). \
                             The agent did not accept the credentials. \
                             Try restarting Intelligent Terminal.",
                            method_id.0,
                            e.message,
                            Into::<i32>::into(e.code),
                        ),
                    }));
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
    match conn.list_sessions(acp::ListSessionsRequest::new()).await {
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
    let cwd = std::env::current_dir().unwrap_or_default();
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
            let _ = event_tx.send(AppEvent::ConnectionStage(format!(
                "Resuming session {}...",
                load_sid
            )));
            (
                acp::SessionId::new(load_sid.to_string()),
                Vec::<crate::app::AcpModelInfo>::new(),
                None,
                false,
            )
        } else {
            let _ = event_tx.send(AppEvent::ConnectionStage("Creating session...".to_string()));
            startup_probe.log("Creating session (over pipe)");
            let mut new_session_req = acp::NewSessionRequest::new(cwd.clone());
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
                // `NewSession` stage — the DISTINCT signal the App's auth
                // recovery matches on (`is_post_login_auth_failure`). This is
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
    startup_probe.log(&format!(
        "Agent capabilities (over pipe): loadSession={}",
        load_session_supported
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
    });

    // Per-tab session cache. Only
    // prepopulate the owner-tab binding when we actually have a
    // bootstrap session — otherwise the `load_session_rx` arm would
    // see the placeholder sid as a prior session, try to `cancel` it,
    // and the agent CLI would reject the cancel for an unknown sid.
    // With no entry, the load arm sees `old_sid = None` and loads
    // cleanly.
    let tab_to_session: Arc<tokio::sync::Mutex<HashMap<String, acp::SessionId>>> =
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
                let conn_for_hook = Arc::clone(&conn);
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
                // `app::send_wt_protocol_event`) → `IProtocolServer::SendEvent`
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
                crate::app::send_wt_protocol_event(evt.to_string());
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
    conn: &Arc<acp::ClientSideConnection>,
    event_tx: &mpsc::UnboundedSender<AppEvent>,
    tab_to_session: &Arc<tokio::sync::Mutex<HashMap<String, acp::SessionId>>>,
) {
    let conn = Arc::clone(conn);
    let event_tx = event_tx.clone();
    let tab_to_session = Arc::clone(tab_to_session);
    tokio::task::spawn_local(async move {
        match req {
            MasterExtRequest::SessionsList { request_id } => {
                let wire = crate::session_registry::build_sessions_list_request();
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
                let sessions: Vec<acp::SessionId> = {
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
    conn: &Arc<acp::ClientSideConnection>,
    tab_to_session: &Arc<tokio::sync::Mutex<HashMap<String, acp::SessionId>>>,
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
    let conn = Arc::clone(conn);
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
        let old_sid: Option<acp::SessionId> = {
            let mut g = tab_to_session.lock().await;
            g.remove(&req.tab_id)
        };

        if let Some(ref old) = old_sid {
            let old_str = old.to_string();
            if let Some(sig) = cancel_signals.lock().unwrap().remove(&old_str) {
                let _ = sig.send(());
            }
            let _ = conn
                .cancel(acp::CancelNotification::new(old.clone()))
                .await;
        }

        let session_id = acp::SessionId::new(req.session_id.clone());
        let mut load_req = acp::LoadSessionRequest::new(session_id.clone(), cwd.clone());
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
                let _ = event_tx.send(AppEvent::SessionAttached {
                    tab_id: req.tab_id.clone(),
                    session_id: session_id.to_string(),
                    available_models: Vec::new(),
                    current_model_id: None,
                });
                // Confirmation note so the user sees the tab transition
                // out of "Resuming..." even if the agent's replay is
                // empty or delayed.
                let _ = event_tx.send(AppEvent::TabSystemMessage {
                    tab_id: req.tab_id.clone(),
                    message: "Session loaded. Past content from \
                              the agent (if any) will appear above."
                        .to_string(),
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
    old_sid: Option<&acp::SessionId>,
    tab_id: &str,
    cwd: &std::path::Path,
    conn: &Arc<acp::ClientSideConnection>,
    tab_to_session: &Arc<tokio::sync::Mutex<HashMap<String, acp::SessionId>>>,
    event_tx: &mpsc::UnboundedSender<AppEvent>,
    message: String,
) {
    if use_load_failure_handler {
        handle_load_failure(
            old_sid,
            tab_id.to_string(),
            cwd.to_path_buf(),
            Arc::clone(conn),
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
    conn: &Arc<acp::ClientSideConnection>,
    tab_to_session: &Arc<tokio::sync::Mutex<HashMap<String, acp::SessionId>>>,
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
    let conn = Arc::clone(conn);
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

        let old_sid: Option<acp::SessionId> = {
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
                .cancel(acp::CancelNotification::new(old.clone()))
                .await;
        }

        // Inject WT_SESSION into the request meta so master can record
        // pane_session_id on the registry row. Without this, focus_session
        // RPCs against the new sid return {"focused": false, "reason":
        // "no_pane"} because master has the row but no pane GUID to feed
        // wtcli focus-pane. Only the helper pipe path needs this.
        let mut new_session_req = acp::NewSessionRequest::new(cwd);
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
    conn: &Arc<acp::ClientSideConnection>,
    tab_to_session: &Arc<tokio::sync::Mutex<HashMap<String, acp::SessionId>>>,
    template_memo: &TemplateMemo,
    cancel_signals: &Arc<std::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>>,
) {
    tracing::info!(
        target: "acp_drop_session",
        tab = %req.tab_id,
        "drop_session requested (no replacement)"
    );
    let conn = Arc::clone(conn);
    let tab_to_session = Arc::clone(tab_to_session);
    let template_memo = template_memo.clone();
    let cancel_signals = Arc::clone(cancel_signals);
    tokio::task::spawn_local(async move {
        let old_sid: Option<acp::SessionId> = {
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
                .cancel(acp::CancelNotification::new(old.clone()))
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
    conn: &Arc<acp::ClientSideConnection>,
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
    let conn_for_cancel = Arc::clone(conn);
    tokio::task::spawn_local(async move {
        let session_id = acp::SessionId::new(session_id_str.clone());
        if let Err(e) = conn_for_cancel
            .cancel(acp::CancelNotification::new(session_id))
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
    tab_to_session: &Arc<tokio::sync::Mutex<HashMap<String, acp::SessionId>>>,
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

fn dispatch_prompt(
    prompt: PromptSubmission,
    conn: &Arc<acp::ClientSideConnection>,
    tab_to_session: &Arc<tokio::sync::Mutex<HashMap<String, acp::SessionId>>>,
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

    let conn_task = Arc::clone(conn);
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
    conn_task: Arc<acp::ClientSideConnection>,
    tab_to_session_task: Arc<tokio::sync::Mutex<HashMap<String, acp::SessionId>>>,
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
                .new_session(acp::NewSessionRequest::new(cwd))
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
            let (per_tab_models, per_tab_current) = match &new_session.models {
                Some(state) => {
                    let models: Vec<crate::app::AcpModelInfo> = state
                        .available_models
                        .iter()
                        .map(|m| crate::app::AcpModelInfo {
                            id: m.model_id.0.to_string(),
                            name: m.name.clone(),
                            description: m.description.clone(),
                        })
                        .collect();
                    (models, Some(state.current_model_id.0.to_string()))
                }
                None => (Vec::new(), None),
            };
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

    prompt_timing_task.activate(&prompt_session_id_str, &prompt);
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
    let _ = event_tx_task.send(AppEvent::ProgressStatus {
        session_id: Some(prompt_session_id_str.clone()),
        status: "Thinking...".to_string(),
    });
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

    let prompt_fut = conn_task.prompt(acp::PromptRequest::new(
        prompt_session_id.clone(),
        vec![text.into()],
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
        acp_result_failure_fields, complete_prompt_request, inject_wta_pane_meta, shell_from_active,
        timeout_result_failure_fields, user_locale_tag, PromptTimingState, SoftStopReason,
    };
    use super::acp;
    use crate::app::AppEvent;
    use tokio::sync::mpsc;

    /// `shell_from_active` resolves our own pid to a real exe name (the test
    /// binary). Proves the pid → image-name path works end to end on Windows;
    /// a missing/zero pid yields `None`.
    #[cfg(windows)]
    #[test]
    fn shell_from_active_resolves_pid() {
        let me = serde_json::json!({ "pid": std::process::id() });
        let name = shell_from_active(&me).expect("own pid should resolve");
        assert!(
            name.to_ascii_lowercase().ends_with(".exe"),
            "expected an .exe image name, got {name:?}"
        );

        assert_eq!(shell_from_active(&serde_json::json!({ "pid": 0 })), None);
        assert_eq!(shell_from_active(&serde_json::json!({})), None);
    }

    /// The `shell` field reported via `OSC 9001;ShellType` wins over the
    /// pid-based fallback — even when a real pid is present. This is the
    /// nested-shell case (`pwsh` → `wsl` → bash): the pane's host process is
    /// still pwsh/wsl.exe, but the prompt is drawn by bash, so the OSC-reported
    /// `wsl:Ubuntu` must reach the agent. Platform-independent (no pid lookup).
    #[test]
    fn shell_from_active_prefers_osc_reported_shell() {
        // Reported shell wins over a live pid.
        let pane = serde_json::json!({ "pid": std::process::id(), "shell": "wsl:Ubuntu" });
        assert_eq!(shell_from_active(&pane), Some("wsl:Ubuntu".to_string()));

        // Empty/whitespace reported shell is ignored; falls back to pid (or None).
        assert_eq!(
            shell_from_active(&serde_json::json!({ "shell": "  ", "pid": 0 })),
            None
        );
        assert_eq!(shell_from_active(&serde_json::json!({ "shell": "" })), None);
    }

    /// Helper-only: round-trip a `_meta` blob through `inject_wta_pane_meta`
    /// and report the `pane_session_id` that the master would see in
    /// `extract_wta_meta`. Returns `None` when the meta is empty after
    /// injection (i.e. `WT_SESSION` was missing/empty and we correctly
    /// emitted no namespace).
    fn injected_pane_session_id() -> Option<String> {
        let mut meta: Option<agent_client_protocol::Meta> = None;
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

        let sid = acp::SessionId::new("sess-target".to_string());
        let cwd = std::path::PathBuf::from("/repo");
        let mut req = acp::LoadSessionRequest::new(sid, cwd);
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
    fn user_locale_tag_returns_current_locale_verbatim() {
        let _g = crate::test_support::lock_locale();
        // Real locales pass through unchanged.
        rust_i18n::set_locale("zh-CN");
        assert_eq!(user_locale_tag(), "zh-CN");
        rust_i18n::set_locale("en-US");
        assert_eq!(user_locale_tag(), "en-US");
        // Pseudo-locales are passed through too — agents treat unknown
        // BCP-47 tags as opaque metadata, so there's no need to remap.
        rust_i18n::set_locale("qps-ploca");
        assert_eq!(user_locale_tag(), "qps-ploca");
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

    // ── Pure string/timing helpers ──────────────────────────────────────────

    #[test]
    fn prompt_preview_escapes_newlines_and_normalizes_crlf() {
        assert_eq!(super::prompt_preview("a\r\nb\rc\nd"), "a\\nb\\nc\\nd");
    }

    #[test]
    fn prompt_preview_truncates_past_80_chars_with_ellipsis() {
        let long: String = std::iter::repeat('x').take(100).collect();
        let out = super::prompt_preview(&long);
        assert_eq!(out.chars().count(), 83, "80 chars + \"...\"");
        assert!(out.ends_with("..."));
        // Exactly 80 chars must NOT get an ellipsis.
        let exact: String = std::iter::repeat('y').take(80).collect();
        let out80 = super::prompt_preview(&exact);
        assert_eq!(out80, exact);
        assert!(!out80.ends_with("..."));
    }

    #[test]
    fn prompt_preview_is_char_safe_with_multibyte() {
        let long: String = std::iter::repeat('é').take(100).collect();
        let out = super::prompt_preview(&long);
        // Must not panic and must cut on a char boundary at 80 + "...".
        assert_eq!(out.chars().count(), 83);
    }

    #[test]
    fn format_elapsed_formats_positive_delta_and_handles_invalid() {
        assert_eq!(super::format_elapsed(Some(1.0), Some(2.5)), "1.500s");
        assert_eq!(super::format_elapsed(Some(2.0), Some(2.0)), "0.000s");
        // end < start, or any missing endpoint → "n/a".
        assert_eq!(super::format_elapsed(Some(2.0), Some(1.0)), "n/a");
        assert_eq!(super::format_elapsed(None, Some(1.0)), "n/a");
        assert_eq!(super::format_elapsed(Some(1.0), None), "n/a");
        assert_eq!(super::format_elapsed(None, None), "n/a");
    }

    #[test]
    fn first_visible_text_gap_prefers_first_event_then_transport() {
        // first_event present → measured from it, labeled "first_event".
        let (gap, label) = super::first_visible_text_gap(Some(1.0), Some(0.5), Some(1.4));
        assert_eq!(label, "first_event");
        assert_eq!(gap, "0.400s");
        // No first_event but transport read present → from transport.
        let (gap, label) = super::first_visible_text_gap(None, Some(0.5), Some(1.5));
        assert_eq!(label, "first_transport_read");
        assert_eq!(gap, "1.000s");
        // Neither present → n/a.
        let (gap, label) = super::first_visible_text_gap(None, None, Some(1.5));
        assert_eq!(label, "n/a");
        assert_eq!(gap, "n/a");
    }

    #[test]
    fn final_timing_note_composes_both_phases() {
        let note = super::final_timing_note(1.0, Some(1.2), Some(1.5), 2.0);
        assert_eq!(
            note,
            "submit->context_ready 0.200s | prompt_sent->options_shown 0.500s"
        );
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

    // ── pane-context / template-kind formatting ─────────────────────────────

    #[test]
    fn format_pane_context_summary_none_is_literal_none() {
        assert_eq!(super::format_pane_context_summary(None), "none");
    }

    /// The summary must surface `effective_source_pane_id`, which drives autofix
    /// routing: it prefers `source_pane_id` (the pane that produced the failing
    /// command) and only falls back to `pane_id` (the agent pane) when absent.
    #[test]
    fn format_pane_context_summary_reflects_effective_source_precedence() {
        let ctx = crate::pane_context::PaneContext {
            pane_id: Some("agent-pane".to_string()),
            tab_id: Some("tab-1".to_string()),
            window_id: Some("win-1".to_string()),
            cwd: Some("C:\\work".to_string()),
            source_pane_id: Some("src-pane".to_string()),
        };
        let s = super::format_pane_context_summary(Some(&ctx));
        assert!(s.contains("pane_id=Some(\"agent-pane\")"), "got: {s}");
        assert!(s.contains("source_pane_id=Some(\"src-pane\")"), "got: {s}");
        assert!(
            s.contains("effective_source_pane_id=Some(\"src-pane\")"),
            "effective must prefer source_pane_id; got: {s}"
        );

        let ctx2 = crate::pane_context::PaneContext {
            pane_id: Some("agent-pane".to_string()),
            source_pane_id: None,
            ..Default::default()
        };
        let s2 = super::format_pane_context_summary(Some(&ctx2));
        assert!(
            s2.contains("effective_source_pane_id=Some(\"agent-pane\")"),
            "effective must fall back to pane_id; got: {s2}"
        );
    }

    #[test]
    fn template_kind_display_matches_label() {
        assert_eq!(super::TemplateKind::Planner.to_string(), "planner");
        assert_eq!(super::TemplateKind::Autofix.to_string(), "autofix");
    }

    // ── terminal-context / prompt assembly ──────────────────────────────────

    /// Minimal [`WtChannel`] that answers `get_active_pane` with a canned pane
    /// and errors every other request. `read_pane_last_message` degrades to
    /// `None` on those errors, which is all the assembly tests need (no buffer
    /// content is asserted).
    struct MockWtChannel {
        active_pane: serde_json::Value,
    }

    #[async_trait::async_trait]
    impl crate::shell::wt_channel::WtChannel for MockWtChannel {
        async fn request(
            &self,
            method: &str,
            _params: serde_json::Value,
        ) -> anyhow::Result<serde_json::Value> {
            match method {
                "get_active_pane" => Ok(self.active_pane.clone()),
                other => Err(anyhow::anyhow!("MockWtChannel: unhandled method {other}")),
            }
        }
        fn is_available(&self) -> bool {
            true
        }
    }

    fn shell_mgr_with_pane(active: serde_json::Value) -> crate::shell::ShellManager {
        crate::shell::ShellManager::new()
            .with_wt_channel(std::sync::Arc::new(MockWtChannel { active_pane: active }))
    }

    #[tokio::test]
    async fn build_terminal_context_json_none_without_wt_channel() {
        let mgr = crate::shell::ShellManager::new();
        assert!(super::build_terminal_context_json(&mgr).await.is_none());
    }

    #[tokio::test]
    async fn build_terminal_context_json_skips_agent_pane() {
        let mgr = shell_mgr_with_pane(serde_json::json!({
            "session_id": "p1",
            "is_agent_pane": true,
        }));
        assert!(
            super::build_terminal_context_json(&mgr).await.is_none(),
            "an active agent pane has no terminal output to ship"
        );
    }

    #[tokio::test]
    async fn build_terminal_context_json_assembles_fields_for_real_pane() {
        let mgr = shell_mgr_with_pane(serde_json::json!({
            "session_id": "pane-9",
            "title": "My Tab",
            "cwd": "C:\\workspace",
            "pid": std::process::id(),
            "is_agent_pane": false,
        }));
        let json = super::build_terminal_context_json(&mgr)
            .await
            .expect("a non-agent active pane must yield context json");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["activeTarget"], "pane-9");
        assert_eq!(v["window_title"], "My Tab");
        assert_eq!(v["cwd"], "C:\\workspace");
        // The mock errors the buffer reads, so `buffer` is null.
        assert!(v["buffer"].is_null());
        // pid is our own test process → shell resolves to the test binary exe.
        if cfg!(windows) {
            assert!(
                v["shell"]
                    .as_str()
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .ends_with(".exe"),
                "shell should resolve from pid; got {:?}",
                v["shell"]
            );
        }
    }

    /// A planner turn with `include_template=true` ships the persona template,
    /// the delegate-agents section, and appends the user request. It never
    /// resolves a fix pane.
    #[tokio::test]
    async fn build_prompt_text_planner_includes_template_and_user_request() {
        let mgr = crate::shell::ShellManager::new();
        let expected = super::prompt::load_planner_prompt_template();
        let (prompt, _source, display_name, fix_pane) =
            super::build_prompt_text(1, 0.0, "list files", false, true, &mgr, false, None).await;
        assert_eq!(display_name, expected.display_name);
        assert!(
            prompt.contains("### Supported Delegate Agents"),
            "planner must ship the delegate-agents section"
        );
        assert!(
            prompt.contains("## User Request\nlist files"),
            "planner must append the user text"
        );
        assert!(fix_pane.is_none(), "planner turns never resolve a fix pane");
    }

    /// An autofix turn loads the *autofix* persona (not the planner), appends a
    /// non-empty hint as a User Request, and omits planner-only sections.
    #[tokio::test]
    async fn build_prompt_text_autofix_appends_hint_and_omits_planner_sections() {
        let mgr = crate::shell::ShellManager::new();
        let planner = super::prompt::load_planner_prompt_template();
        let autofix = super::prompt::load_autofix_prompt_template();
        let (prompt, _s, display_name, fix_pane) =
            super::build_prompt_text(2, 0.0, "fix the build", true, true, &mgr, false, None).await;
        assert_eq!(display_name, autofix.display_name);
        assert_ne!(
            display_name, planner.display_name,
            "autofix must not reuse the planner persona"
        );
        assert!(
            !prompt.contains("### Supported Delegate Agents"),
            "autofix prompt is not the planner prompt"
        );
        let user_request = format!("## User Request\n{}", "fix the build");
        assert!(
            prompt.contains(&user_request),
            "a non-empty autofix hint is appended"
        );
        assert!(fix_pane.is_none(), "no wt channel → nothing to resolve");
    }

    /// A blank autofix hint must not produce an empty `## User Request` section.
    #[tokio::test]
    async fn build_prompt_text_autofix_blank_hint_has_no_user_request() {
        let mgr = crate::shell::ShellManager::new();
        let (prompt, _s, _d, _f) =
            super::build_prompt_text(3, 0.0, "   ", true, true, &mgr, false, None).await;
        assert!(
            !prompt.contains("## User Request"),
            "blank autofix hint must not add a User Request section"
        );
    }

    /// With `include_template=false` the (large) persona body is dropped — only
    /// runtime sections and the user request remain. This is the per-session
    /// "template already in history" optimization.
    #[tokio::test]
    async fn build_prompt_text_without_template_drops_persona_body() {
        let mgr = crate::shell::ShellManager::new();
        let planner = super::prompt::load_planner_prompt_template();
        assert!(
            !planner.content.trim().is_empty(),
            "test precondition: planner template body is non-empty"
        );
        let (prompt, _s, _d, _f) =
            super::build_prompt_text(4, 0.0, "hi", false, false, &mgr, false, None).await;
        assert!(
            !prompt.contains(planner.content.trim()),
            "include_template=false must omit the template body"
        );
        let user_request = format!("## User Request\n{}", "hi");
        assert!(prompt.contains(&user_request));
    }

    /// A manual `/fix` (autofix, no explicit `source_pane_id`) resolves the
    /// active working pane from WT and reports it as the fix target so the App
    /// can address the eventual fix command.
    #[tokio::test]
    async fn build_prompt_text_autofix_fix_resolves_active_pane() {
        let mgr = shell_mgr_with_pane(serde_json::json!({
            "session_id": "work-pane",
            "cwd": "C:\\proj",
            "pid": std::process::id(),
            "is_agent_pane": false,
        }));
        let (prompt, _s, _d, fix_pane) =
            super::build_prompt_text(5, 0.0, "", true, true, &mgr, true, None).await;
        assert_eq!(
            fix_pane.as_deref(),
            Some("work-pane"),
            "manual /fix must resolve the active working pane"
        );
        assert!(
            prompt.contains("### Shell Context"),
            "autofix with a wt channel must ship shell context"
        );
    }

    /// Error-triggered autofix carries its own `source_pane_id`; the explicit
    /// source wins and `resolved_fix_pane` stays `None` (the App already knows
    /// the target).
    #[tokio::test]
    async fn build_prompt_text_autofix_explicit_source_not_reported_as_resolved() {
        let mgr = shell_mgr_with_pane(serde_json::json!({
            "session_id": "work-pane",
            "pid": std::process::id(),
            "is_agent_pane": false,
        }));
        let ctx = crate::pane_context::PaneContext {
            source_pane_id: Some("explicit-src".to_string()),
            ..Default::default()
        };
        let (_p, _s, _d, fix_pane) =
            super::build_prompt_text(6, 0.0, "", true, true, &mgr, true, Some(&ctx)).await;
        assert!(
            fix_pane.is_none(),
            "error-triggered autofix carries its source; resolved_fix_pane stays None"
        );
    }

    // ── truncate / snippet / session_short ──────────────────────────────────

    #[test]
    fn truncate_for_prompt_appends_marker_only_when_over_budget() {
        assert_eq!(super::truncate_for_prompt("hello", 10), "hello");
        assert_eq!(super::truncate_for_prompt("hello", 5), "hello");
        assert_eq!(super::truncate_for_prompt("hello", 3), "hel\n...<truncated>");
    }

    #[test]
    fn truncate_for_prompt_is_char_safe() {
        let s: String = std::iter::repeat('é').take(10).collect();
        // 5-char budget must cut on a char boundary, no panic.
        let out = super::truncate_for_prompt(&s, 5);
        assert!(out.starts_with("ééééé"));
        assert!(out.ends_with("...<truncated>"));
    }

    #[test]
    fn snippet_takes_head_or_tail() {
        assert_eq!(super::snippet("hello world", 5, true), "hello");
        assert_eq!(super::snippet("hello world", 5, false), "world");
        // Budget larger than the text returns the whole thing either way.
        assert_eq!(super::snippet("hi", 5, true), "hi");
        assert_eq!(super::snippet("hi", 5, false), "hi");
        // Newlines are escaped for single-line logging.
        assert_eq!(super::snippet("a\nb", 5, true), "a\\nb");
    }

    #[test]
    fn session_short_returns_last_eight_chars() {
        assert_eq!(super::session_short("0123456789abcdef"), "89abcdef");
        // Shorter than 8 → whole string.
        assert_eq!(super::session_short("abc"), "abc");
    }

    // ── json_str_or_num ─────────────────────────────────────────────────────

    #[test]
    fn json_str_or_num_accepts_strings_and_numbers_only() {
        use serde_json::json;
        let s = json!("hello");
        let n = json!(42);
        let f = json!(1.5);
        let b = json!(true);
        let null = json!(null);
        let arr = json!([1, 2]);
        assert_eq!(super::json_str_or_num(Some(&s)).as_deref(), Some("hello"));
        assert_eq!(super::json_str_or_num(Some(&n)).as_deref(), Some("42"));
        assert_eq!(super::json_str_or_num(Some(&f)).as_deref(), Some("1.5"));
        assert_eq!(super::json_str_or_num(Some(&b)), None);
        assert_eq!(super::json_str_or_num(Some(&null)), None);
        assert_eq!(super::json_str_or_num(Some(&arr)), None);
        assert_eq!(super::json_str_or_num(None), None);
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
        use crate::app::AppEvent;
        use crate::session_registry::{
            build_session_added_notification, build_session_removed_notification,
            INTELLTERM_METHOD_SESSION_REMOVED,
        };
        use crate::shell::ShellManager;
        use agent_client_protocol::{self as acp, Client};
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
                acp::SessionId::new("sess-1".to_string()),
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
            let sid = acp::SessionId::new("sess-dead".to_string());
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
            let ext = acp::ExtNotification::new(
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
            let ext = acp::ExtNotification::new(
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
