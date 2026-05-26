use super::prompt;
use acp::Agent as _;
use agent_client_protocol as acp;
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use std::task::{Context, Poll};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader, ReadBuf};
use tokio::sync::mpsc;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::app::{AppEvent, PermOption, PlanEntry, PlanEntryStatus};
use crate::coordinator::default_supported_delegate_agents;
use crate::pane_context::PaneContext;
use crate::shell::{ShellManager, TerminalConfig};

const ACTIVE_PANE_CONTEXT_MAX_CHARS: usize = 4000;

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

/// How [`run_inner`] terminated. The outer driver in [`run_acp_client`]
/// uses this to decide whether to respawn the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ExitReason {
    /// Loop exited because all sender halves dropped (process shutdown).
    Done,
    /// `/restart` requested. Outer driver should re-enter `run_inner`.
    /// If `agent_cmd` is set, the supervisor should switch to that agent.
    Restart { agent_cmd: Option<String> },
}

impl PromptSubmission {
    pub fn new(text: String, pane_context: Option<PaneContext>) -> Self {
        Self::new_with_kind(text, pane_context, false)
    }

    pub fn new_autofix(text: String, pane_context: Option<PaneContext>) -> Self {
        Self::new_with_kind(text, pane_context, true)
    }

    fn new_with_kind(
        text: String,
        pane_context: Option<PaneContext>,
        is_autofix: bool,
    ) -> Self {
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
                "queue_delay={} preview={:?}",
                format_elapsed(Some(prompt.submitted_at_unix_s), Some(now)),
                preview
            ),
        );
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

    fn observe_stdin_write(&self, bytes: usize) {
        let now = now_unix_s();
        let mut guard = self.active.lock().unwrap();
        let mut updates = Vec::new();
        for active in guard.values_mut() {
            if active.prompt_sent_at_unix_s.is_none() {
                continue;
            }
            active.bytes_written_after_prompt += bytes as u64;
            if active.first_stdin_write_at_unix_s.is_none() {
                active.first_stdin_write_at_unix_s = Some(now);
                updates.push((
                    active.id,
                    active.submitted_at_unix_s,
                    format!(
                        "bytes={} since_prompt_sent={}",
                        bytes,
                        format_elapsed(active.prompt_sent_at_unix_s, Some(now))
                    ),
                ));
            }
        }
        drop(guard);
        for (turn_id, submitted, details) in updates {
            prompt_timing_log(turn_id, submitted, "first_transport_write", &details);
        }
    }

    fn observe_stdout_read(&self, bytes: usize) {
        let now = now_unix_s();
        let mut guard = self.active.lock().unwrap();
        let mut updates = Vec::new();
        for active in guard.values_mut() {
            if active.prompt_sent_at_unix_s.is_none() {
                continue;
            }
            active.bytes_read_after_prompt += bytes as u64;
            if active.first_stdout_byte_at_unix_s.is_none() {
                active.first_stdout_byte_at_unix_s = Some(now);
                updates.push((
                    active.id,
                    active.submitted_at_unix_s,
                    format!(
                        "bytes={} since_prompt_sent={}",
                        bytes,
                        format_elapsed(active.prompt_sent_at_unix_s, Some(now))
                    ),
                ));
            }
        }
        drop(guard);
        for (turn_id, submitted, details) in updates {
            prompt_timing_log(turn_id, submitted, "first_transport_read", &details);
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
                let details = format!(
                    "text_len={} since_prompt_sent={} first_visible_text_gap={} gap_source={}",
                    text_len,
                    format_elapsed(active.prompt_sent_at_unix_s, Some(now)),
                    visible_gap,
                    visible_gap_source
                );
                drop(guard);
                prompt_timing_log(turn_id, submitted_at_unix_s, "first_text", &details);
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
                    "title={:?} since_prompt_sent={}",
                    title_preview,
                    format_elapsed(active.prompt_sent_at_unix_s, Some(now))
                );
                drop(guard);
                prompt_timing_log(turn_id, submitted_at_unix_s, "first_tool_call", &details);
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
                "description={:?} since_prompt_sent={}",
                prompt_preview(description),
                format_elapsed(active.prompt_sent_at_unix_s, Some(now))
            );
            drop(guard);
            prompt_timing_log(
                turn_id,
                submitted_at_unix_s,
                "permission_requested",
                &details,
            );
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
            format!("preview={:?}", active_prompt.preview),
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

        Some(final_timing_note(
            active_prompt.submitted_at_unix_s,
            active_prompt.context_ready_at_unix_s,
            active_prompt.prompt_sent_at_unix_s,
            now,
        ))
    }
}

fn summarize_agent_identity(program: &str, args: &[&str]) -> (String, Option<String>) {
    let brand = crate::agent_registry::display_name_for(program);
    let profile = crate::agent_registry::lookup_profile(program);
    let model = crate::agent_registry::extract_model_from_args(args, profile)
        .map(humanize_model_name);
    (brand, model)
}

fn requested_model_id(program: &str, args: &[&str]) -> Option<String> {
    let profile = crate::agent_registry::lookup_profile(program);
    crate::agent_registry::extract_model_from_args(args, profile).map(str::to_string)
}

async fn complete_prompt_request<T, E: std::fmt::Display>(
    result: std::result::Result<T, E>,
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
            let _ = event_tx.send(AppEvent::AgentMessageEnd { session_id });
        }
        Err(e) => {
            let error_message = e.to_string();
            let timing_note = prompt_timing.complete(&session_id, false, Some(&error_message));
            if let Some(note) = timing_note {
                let _ = event_tx.send(AppEvent::TimingMetric {
                    session_id: session_id.clone(),
                    note,
                });
            }
            let _ = event_tx.send(AppEvent::AgentError {
                session_id: Some(session_id),
                message: format!("prompt error: {}", error_message),
            });
        }
    }
}

fn humanize_model_name(model: &str) -> String {
    let tokens: Vec<String> = model
        .split(|ch| ch == '-' || ch == '_')
        .filter(|token| !token.is_empty())
        .map(humanize_identifier)
        .collect();

    if tokens.is_empty() {
        model.to_string()
    } else {
        tokens.join(" ")
    }
}

fn humanize_identifier(token: &str) -> String {
    if token.is_empty() {
        return String::new();
    }

    let lower = token.to_ascii_lowercase();
    match lower.as_str() {
        "gpt" => "GPT".to_string(),
        "claude" => "Claude".to_string(),
        "haiku" => "Haiku".to_string(),
        "sonnet" => "Sonnet".to_string(),
        "opus" => "Opus".to_string(),
        "codex" => "Codex".to_string(),
        "mini" => "Mini".to_string(),
        "turbo" => "Turbo".to_string(),
        "max" => "Max".to_string(),
        _ if lower.chars().all(|ch| ch.is_ascii_digit() || ch == '.') => token.to_string(),
        _ => {
            let mut chars = lower.chars();
            match chars.next() {
                Some(first) => {
                    let mut title = String::with_capacity(token.len());
                    title.push(first.to_ascii_uppercase());
                    title.extend(chars);
                    title
                }
                None => String::new(),
            }
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
            "pane_id={:?} tab_id={:?} window_id={:?} source_pane_id={:?} effective_source_pane_id={:?} cwd={:?}",
            context.pane_id,
            context.tab_id,
            context.window_id,
            context.source_pane_id,
            context.effective_source_pane_id(),
            context.cwd
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

/// Resolve the user's active pane cwd via WT's `get_active_pane` COM call.
///
/// Used at agent-session startup to pin both the agent child process cwd and
/// the ACP `new_session` cwd to the user's project, so `execute_command` lands
/// there on its first call. Returns `None` when WT isn't connected, when WT
/// reports the active pane is the agent pane itself (no source resolved yet),
/// or when the cwd field is missing/empty — callers fall back to
/// `std::env::current_dir()`.
async fn resolve_active_pane_cwd(
    shell_mgr: &ShellManager,
    wt_connected: bool,
) -> Option<std::path::PathBuf> {
    if !wt_connected {
        return None;
    }
    let active = shell_mgr.wt_get_active_pane().await.ok()?;
    let is_agent = active
        .get("is_agent_pane")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if is_agent {
        return None;
    }
    active
        .get("cwd")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
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
    // Shell profile (e.g. "PowerShell", "Command Prompt", "Ubuntu") is
    // load-bearing for the planner: any `send` action it emits has to
    // match the active pane's shell syntax (`Get-ChildItem` vs `ls`,
    // `Set-Location` vs `cd`, etc.). Without this the agent has to
    // guess from the buffer's prompt prefix, which silently fails on
    // renamed or unusual profiles.
    let target_profile = active
        .get("profile")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    tracing::debug!(
        target: "acp.terminal_context",
        target_pane_id = %target_pane_id,
        profile = ?target_profile,
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
        "profile": target_profile,
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
) -> (String, String, String) {
    let total_started = std::time::Instant::now();
    let mut runtime_sections = Vec::new();

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
            if let Some(source_pane_id) = pane_context
                .and_then(|ctx| ctx.effective_source_pane_id())
            {
                tracing::debug!(
                    target: "acp.terminal_context",
                    source_pane_id,
                    mode = "autofix",
                    "terminal_context_target_resolved"
                );

                // Shell context — best-effort. WT returns the profile name
                // (e.g. "PowerShell", "Command Prompt", "Ubuntu") which is a
                // strong signal even when the user has renamed the profile.
                if let Ok(active) = shell_mgr.wt_get_active_pane().await {
                    let profile = active
                        .get("profile")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let cwd = active
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let json = serde_json::to_string(&serde_json::json!({
                        "profile": profile,
                        "cwd": cwd,
                        "locale": user_locale_tag(),
                    }))
                    .unwrap_or_else(|_| "{}".to_string());
                    runtime_sections.push(format!(
                        "### Shell Context\n```json\n{}\n```",
                        json
                    ));
                }

                if let Some(content) = read_pane_last_message(
                    shell_mgr,
                    source_pane_id,
                    30,
                    ACTIVE_PANE_CONTEXT_MAX_CHARS,
                )
                .await
                {
                    runtime_sections.push(format!(
                        "### Terminal Output\n```\n{}\n```",
                        content
                    ));
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
        prompt_body
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
    )
}

fn acp_log(msg: &str) {
    tracing::debug!(target: "acp", "{}", msg);
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
    tracing::debug!(target: "acp", "planner_prompt_text:\n{}", prompt_text);
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
        body_head = %head,
        body_tail = %tail,
        "turn_sent"
    );
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
        acp_log(&format!("{} (t+{:.3}s)", msg, self.begin.elapsed().as_secs_f64()));
    }
}

struct StartupInstrumentedReader<R> {
    inner: R,
    probe: StartupProbe,
    label: &'static str,
    saw_data: bool,
    prompt_timing: Arc<PromptTimingState>,
}

impl<R> StartupInstrumentedReader<R> {
    fn new(
        inner: R,
        probe: StartupProbe,
        label: &'static str,
        prompt_timing: Arc<PromptTimingState>,
    ) -> Self {
        Self {
            inner,
            probe,
            label,
            saw_data: false,
            prompt_timing,
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for StartupInstrumentedReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let filled_before = buf.filled().len();
        match Pin::new(&mut self.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                let read_len = buf.filled().len().saturating_sub(filled_before);
                if read_len > 0 && !self.saw_data {
                    self.saw_data = true;
                    self.probe.log(&format!(
                        "first data received on agent {}: {} byte(s)",
                        self.label, read_len
                    ));
                }
                if read_len > 0 {
                    self.prompt_timing.observe_stdout_read(read_len);
                }
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

struct InstrumentedAgentWriter<W> {
    inner: W,
    prompt_timing: Arc<PromptTimingState>,
}

impl<W> InstrumentedAgentWriter<W> {
    fn new(inner: W, prompt_timing: Arc<PromptTimingState>) -> Self {
        Self {
            inner,
            prompt_timing,
        }
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for InstrumentedAgentWriter<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match Pin::new(&mut self.inner).poll_write(cx, buf) {
            Poll::Ready(Ok(written)) => {
                if written > 0 {
                    self.prompt_timing.observe_stdin_write(written);
                }
                Poll::Ready(Ok(written))
            }
            other => other,
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
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
        acp_log(&format!("request_permission: {:?}", args.tool_call.fields.title));
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
        acp_log(&format!("session_notification: {:?}", args.update));
        let sid = args.session_id.0.to_string();
        self.state
            .prompt_timing
            .observe_session_update(&sid, session_update_kind(&args.update));
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
        acp_log(&format!("create_terminal called: cmd={} args={:?}", args.command, args.args));
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
}

/// Helper-mode variant of [`run_acp_client`]. Instead of spawning the
/// agent CLI as a child process and talking ACP over its stdio, this
/// connects to a wta-master singleton over the named pipe whose path
/// is passed in `pipe_name` and speaks ACP over that pipe. The master
/// (from this helper's perspective) plays the role of the agent.
///
/// Wires the same App-facing select-loop as `run_inner`, minus the
/// restart-loop wrapper: helper mode doesn't own the agent CLI lifetime
/// (master does), so `restart_rx` is drained but logged as unsupported.
///
/// See doc/specs/Multi-window-agent-pane.md for the helper+master
/// architecture, and `tools/wta/src/master/mod.rs` for the peer.
#[allow(clippy::too_many_arguments)]
pub async fn run_acp_client_over_pipe(
    pipe_name: String,
    acp_model_override: Option<String>,
    owner_tab_id: Option<String>,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    mut prompt_rx: mpsc::UnboundedReceiver<PromptSubmission>,
    mut cancel_rx: mpsc::UnboundedReceiver<CancelRequest>,
    mut new_session_rx: mpsc::UnboundedReceiver<NewSessionForTab>,
    mut load_session_rx: mpsc::UnboundedReceiver<LoadSessionForTab>,
    mut drop_session_rx: mpsc::UnboundedReceiver<DropSessionRequest>,
    mut rename_session_rx: mpsc::UnboundedReceiver<RenameSessionRequest>,
    mut restart_rx: mpsc::UnboundedReceiver<RestartRequest>,
    shell_mgr: Arc<ShellManager>,
    wt_connected: bool,
) -> Result<()> {
    let startup_probe = StartupProbe::new();
    startup_probe.log(&format!(
        "run_acp_client_over_pipe task start pipe={} acp_model={:?} wt_connected={}",
        pipe_name, acp_model_override, wt_connected
    ));

    // Whether this WTA process is hosting an Intelligent Terminal agent
    // pane. Same semantics as in `run_inner`: `--owner-tab-id` is the
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
            50, 100, 100, 200, 200, 500, 500, 1000, 1000, 2000, 2000, 2000, 5000, 5000, 5000,
            5000, 10000, 10000, 10000, 15000,
        ];
        loop {
            match tokio::net::windows::named_pipe::ClientOptions::new().open(&pipe_name) {
                Ok(pipe) => {
                    if attempt > 0 {
                        tracing::info!(
                            target: "helper",
                            step = "pipe_connect",
                            pipe = %pipe_name,
                            attempts = attempt + 1,
                            "master pipe connected after retry"
                        );
                    }
                    startup_probe.log(&format!(
                        "master pipe connected (attempt {})",
                        attempt + 1
                    ));
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

    let (conn, handle_io) =
        acp::ClientSideConnection::new(client, outgoing, incoming, |fut| {
            tokio::task::spawn_local(fut);
        });
    startup_probe.log("ACP client connection created (over pipe)");

    let io_probe = startup_probe.clone();
    tokio::task::spawn_local(async move {
        io_probe.log("ACP handle_io task started (over pipe)");
        if let Err(e) = handle_io.await {
            io_probe.log(&format!("ACP handle_io failed: {:#}", e));
            eprintln!("helper ACP I/O failed: {:#}", e);
        } else {
            io_probe.log("ACP handle_io completed (over pipe)");
        }
    });

    // Initialize — same as the child-process path. We use a 60s timeout
    // here because the first helper to connect to a fresh master may
    // ride along with the master's own agent CLI spawn (especially the
    // npx adapter cold start). After the first init, subsequent inits
    // are fast because master just re-forwards.
    let _ = event_tx.send(AppEvent::ConnectionStage(
        "Initializing ACP...".to_string(),
    ));
    startup_probe.log("Initializing ACP (over pipe)");
    let init_future = conn.initialize(
        acp::InitializeRequest::new(acp::ProtocolVersion::V1)
            .client_capabilities(acp::ClientCapabilities::new().terminal(true))
            .client_info(
                acp::Implementation::new("wta-helper", env!("CARGO_PKG_VERSION"))
                    .title("Windows Terminal Agent (helper)"),
            ),
    );
    let init_resp = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        init_future,
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "ACP initialize over master pipe timed out after 60s — \
             wta-master did not respond"
        )
    })?
    .map_err(|e| anyhow::anyhow!("initialize over master pipe failed: {}", e))?;
    startup_probe.log(&format!(
        "Agent init response received (over pipe): {:?}",
        init_resp
    ));

    // Create the initial session bound to the owner tab.
    let _ = event_tx.send(AppEvent::ConnectionStage(
        "Creating session...".to_string(),
    ));
    startup_probe.log("Creating session (over pipe)");
    let cwd = std::env::current_dir().unwrap_or_default();
    let session_future = conn.new_session(acp::NewSessionRequest::new(cwd));
    let session = tokio::time::timeout(std::time::Duration::from_secs(30), session_future)
        .await
        .map_err(|_| anyhow::anyhow!("new_session over master pipe timed out after 30s"))?
        .map_err(|e| anyhow::anyhow!("new_session over master pipe failed: {}", e))?;

    let session_id = session.session_id.clone();
    startup_probe.log(&format!("Session created (over pipe): {}", session_id));
    if is_agent_pane {
        // Carry the WT pane-session GUID alongside the ACP session id so
        // agent-pane origin tracking can recover the owning pane later
        // (matches origin/main's `append_default` signature after #66).
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

    let (available_models, current_model_id) = match &session.models {
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

    // Apply --acp-model if requested. No `--agent` cmdline to parse in
    // helper mode (master owns the agent CLI), so this is the only
    // model-selection input.
    if let Some(requested_model) =
        acp_model_override.filter(|s| !s.trim().is_empty())
    {
        let _ = event_tx.send(AppEvent::ConnectionStage(format!(
            "Selecting model {}...",
            requested_model
        )));
        startup_probe.log(&format!(
            "Setting ACP session model to {} (over pipe)",
            requested_model
        ));
        conn.set_session_model(acp::SetSessionModelRequest::new(
            session_id.clone(),
            requested_model.clone(),
        ))
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "set_session_model failed for requested model {}: {}",
                requested_model,
                e
            )
        })?;
        startup_probe.log(&format!(
            "ACP session model set to {} (over pipe)",
            requested_model
        ));
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

    // Per-tab session cache. Same semantics as in `run_inner`.
    let tab_to_session: Arc<tokio::sync::Mutex<HashMap<String, acp::SessionId>>> =
        Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    {
        let mut g = tab_to_session.lock().await;
        let initial_tab_key = owner_tab_id
            .clone()
            .unwrap_or_else(|| "0".to_string());
        g.insert(initial_tab_key, session_id.clone());
    }

    let template_memo = TemplateMemo::default();
    let in_flight_tabs: Arc<std::sync::Mutex<HashSet<String>>> =
        Arc::new(std::sync::Mutex::new(HashSet::new()));
    let cancel_signals: Arc<
        std::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>,
    > = Arc::new(std::sync::Mutex::new(HashMap::new()));

    let conn = Arc::new(conn);

    // Main event loop. Mirrors `run_inner`'s select arms, minus the
    // restart-loop wrapper (helper mode can't restart — master owns
    // the agent CLI). A `/restart` signal is logged and reported back
    // to the user as a no-op; deeper restart support is a Phase 3+
    // concern (the master itself would need a "respawn" path).
    loop {
        tokio::select! {
            biased;
            Some(req) = restart_rx.recv() => {
                tracing::warn!(
                    target: "helper",
                    new_agent = ?req.agent_cmd,
                    "restart requested in helper mode — not supported \
                     (master owns the agent CLI lifetime); reporting to user"
                );
                let _ = event_tx.send(AppEvent::AgentError {
                    session_id: None,
                    message: "/restart is not available in this agent pane \
                              — restart is owned by the shared wta-master \
                              process. Close and reopen the pane to recover."
                        .to_string(),
                });
            }
            Some(req) = cancel_rx.recv() => {
                let session_id_str = req.session_id.clone();
                tracing::info!(target: "acp_cancel", session_id = %session_id_str, "cancel requested (helper)");
                if let Some(sig) = cancel_signals.lock().unwrap().remove(&session_id_str) {
                    let _ = sig.send(());
                }
                let conn_for_cancel = Arc::clone(&conn);
                tokio::task::spawn_local(async move {
                    let session_id = acp::SessionId::new(session_id_str.clone());
                    if let Err(e) = conn_for_cancel
                        .cancel(acp::CancelNotification::new(session_id))
                        .await
                    {
                        tracing::warn!(target: "acp_cancel", session_id = %session_id_str, error = ?e, "session/cancel rpc failed");
                    }
                });
            }
            Some(req) = new_session_rx.recv() => {
                tracing::info!(
                    target: "acp_new_session",
                    tab = %req.tab_id,
                    "new_session requested (helper)"
                );
                let conn_for_new = Arc::clone(&conn);
                let tab_to_session_for_new = Arc::clone(&tab_to_session);
                let template_memo_for_new = template_memo.clone();
                let cancel_signals_for_new = Arc::clone(&cancel_signals);
                let event_tx_for_new = event_tx.clone();
                let is_agent_pane_for_new = is_agent_pane;
                tokio::task::spawn_local(async move {
                    let cwd = req
                        .cwd
                        .clone()
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

                    let old_sid: Option<acp::SessionId> = {
                        let mut g = tab_to_session_for_new.lock().await;
                        g.remove(&req.tab_id)
                    };

                    if let Some(ref old) = old_sid {
                        let old_str = old.to_string();
                        template_memo_for_new.forget(&old_str).await;
                        if let Some(sig) = cancel_signals_for_new
                            .lock()
                            .unwrap()
                            .remove(&old_str)
                        {
                            let _ = sig.send(());
                        }
                        let _ = conn_for_new
                            .cancel(acp::CancelNotification::new(old.clone()))
                            .await;
                    }

                    let new_session = match conn_for_new
                        .new_session(acp::NewSessionRequest::new(cwd))
                        .await
                    {
                        Ok(s) => s,
                        Err(e) => {
                            let _ = event_tx_for_new.send(AppEvent::AgentError {
                                session_id: None,
                                message: format!("/new failed for tab {}: {}", req.tab_id, e),
                            });
                            return;
                        }
                    };

                    let new_sid = new_session.session_id.clone();
                    if is_agent_pane_for_new {
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

                    {
                        let mut g = tab_to_session_for_new.lock().await;
                        g.insert(req.tab_id.clone(), new_sid.clone());
                    }

                    let _ = event_tx_for_new.send(AppEvent::SessionAttached {
                        tab_id: req.tab_id.clone(),
                        session_id: new_sid.to_string(),
                        available_models: per_tab_models,
                        current_model_id: per_tab_current,
                    });
                });
            }
            Some(req) = load_session_rx.recv() => {
                tracing::info!(
                    target: "acp_load_session",
                    tab = %req.tab_id,
                    session_id = %req.session_id,
                    "load_session requested (helper)"
                );
                let conn_for_load = Arc::clone(&conn);
                let tab_to_session_for_load = Arc::clone(&tab_to_session);
                let cancel_signals_for_load = Arc::clone(&cancel_signals);
                let event_tx_for_load = event_tx.clone();
                tokio::task::spawn_local(async move {
                    let cwd = req
                        .cwd
                        .clone()
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

                    let old_sid: Option<acp::SessionId> = {
                        let mut g = tab_to_session_for_load.lock().await;
                        g.remove(&req.tab_id)
                    };

                    if let Some(ref old) = old_sid {
                        let old_str = old.to_string();
                        if let Some(sig) = cancel_signals_for_load
                            .lock()
                            .unwrap()
                            .remove(&old_str)
                        {
                            let _ = sig.send(());
                        }
                        let _ = conn_for_load
                            .cancel(acp::CancelNotification::new(old.clone()))
                            .await;
                    }

                    let session_id = acp::SessionId::new(req.session_id.clone());
                    let load_req = acp::LoadSessionRequest::new(session_id.clone(), cwd);
                    let load_future = conn_for_load.load_session(load_req);
                    let load_result = tokio::time::timeout(
                        std::time::Duration::from_secs(60),
                        load_future,
                    )
                    .await;

                    match load_result {
                        Ok(Ok(_resp)) => {
                            tracing::info!(
                                target: "acp_load_session",
                                tab = %req.tab_id,
                                session_id = %req.session_id,
                                "load_session succeeded (helper)"
                            );
                            {
                                let mut g = tab_to_session_for_load.lock().await;
                                g.insert(req.tab_id.clone(), session_id.clone());
                            }
                            let _ = event_tx_for_load.send(AppEvent::SessionAttached {
                                tab_id: req.tab_id.clone(),
                                session_id: session_id.to_string(),
                                available_models: Vec::new(),
                                current_model_id: None,
                            });
                            let _ = event_tx_for_load.send(AppEvent::TabSystemMessage {
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
                                "load_session failed (helper)"
                            );
                            let _ = event_tx_for_load.send(AppEvent::TabError {
                                tab_id: req.tab_id.clone(),
                                message: format!(
                                    "Failed to resume session in agent pane: {}. \
                                     The connected agent may not recognize this \
                                     session id (CLI mismatch), or `session/load` \
                                     is unsupported.",
                                    e
                                ),
                            });
                        }
                        Err(_) => {
                            tracing::warn!(
                                target: "acp_load_session",
                                tab = %req.tab_id,
                                session_id = %req.session_id,
                                "load_session timed out after 60s (helper)"
                            );
                            let _ = event_tx_for_load.send(AppEvent::TabError {
                                tab_id: req.tab_id.clone(),
                                message:
                                    "Resume timed out after 60s — the agent \
                                     did not respond to `session/load`."
                                        .to_string(),
                            });
                        }
                    }
                });
            }
            Some(req) = drop_session_rx.recv() => {
                tracing::info!(
                    target: "acp_drop_session",
                    tab = %req.tab_id,
                    "drop_session requested (helper, no replacement)"
                );
                let conn_for_drop = Arc::clone(&conn);
                let tab_to_session_for_drop = Arc::clone(&tab_to_session);
                let template_memo_for_drop = template_memo.clone();
                let cancel_signals_for_drop = Arc::clone(&cancel_signals);
                tokio::task::spawn_local(async move {
                    let old_sid: Option<acp::SessionId> = {
                        let mut g = tab_to_session_for_drop.lock().await;
                        g.remove(&req.tab_id)
                    };
                    if let Some(old) = old_sid {
                        let old_str = old.to_string();
                        template_memo_for_drop.forget(&old_str).await;
                        if let Some(sig) = cancel_signals_for_drop
                            .lock()
                            .unwrap()
                            .remove(&old_str)
                        {
                            let _ = sig.send(());
                        }
                        if let Err(e) = conn_for_drop
                            .cancel(acp::CancelNotification::new(old.clone()))
                            .await
                        {
                            tracing::warn!(
                                target: "acp_drop_session",
                                tab = %req.tab_id,
                                error = ?e,
                                "session/cancel after drop failed (helper)"
                            );
                        }
                    }
                });
            }
            Some(req) = rename_session_rx.recv() => {
                let tab_to_session_for_rename = Arc::clone(&tab_to_session);
                tokio::task::spawn_local(async move {
                    let mut g = tab_to_session_for_rename.lock().await;
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

/// Top-level ACP client task: spawn agent, handshake, prompt loop.
pub async fn run_acp_client(
    mut agent_cmd: String,
    acp_model_override: Option<String>,
    owner_tab_id: Option<String>,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    mut prompt_rx: mpsc::UnboundedReceiver<PromptSubmission>,
    mut cancel_rx: mpsc::UnboundedReceiver<CancelRequest>,
    mut new_session_rx: mpsc::UnboundedReceiver<NewSessionForTab>,
    mut load_session_rx: mpsc::UnboundedReceiver<LoadSessionForTab>,
    mut drop_session_rx: mpsc::UnboundedReceiver<DropSessionRequest>,
    mut rename_session_rx: mpsc::UnboundedReceiver<RenameSessionRequest>,
    mut restart_rx: mpsc::UnboundedReceiver<RestartRequest>,
    shell_mgr: Arc<ShellManager>,
    wt_connected: bool,
) {
    let startup_probe = StartupProbe::new();
    startup_probe.log(&format!(
        "run_acp_client task start agent_cmd={} acp_model={:?} wt_connected={}",
        agent_cmd, acp_model_override, wt_connected
    ));

    // Restart loop. `run_inner` returns `ExitReason::Restart` when the
    // user invokes `/restart`; we re-enter to spawn a fresh agent
    // process. Any other return (Done or Err) breaks the loop.
    loop {
        startup_probe.log("run_acp_client entering run_inner");
        match run_inner(
            &agent_cmd,
            acp_model_override.clone(),
            owner_tab_id.clone(),
            event_tx.clone(),
            &mut prompt_rx,
            &mut cancel_rx,
            &mut new_session_rx,
            &mut load_session_rx,
            &mut drop_session_rx,
            &mut rename_session_rx,
            &mut restart_rx,
            Arc::clone(&shell_mgr),
            wt_connected,
        )
        .await
        {
            Ok(ExitReason::Done) => {
                startup_probe.log("run_acp_client completed");
                break;
            }
            Ok(ExitReason::Restart { agent_cmd: new_cmd }) => {
                if let Some(cmd) = new_cmd {
                    startup_probe.log(&format!("run_acp_client switching agent to: {}", cmd));
                    agent_cmd = cmd;
                } else {
                    startup_probe.log("run_acp_client restart requested — respawning agent");
                }
                let _ = event_tx.send(AppEvent::ConnectionStage(
                    "Restarting agent...".to_string(),
                ));
                continue;
            }
            Err(e) => {
                startup_probe.log(&format!(
                    "run_acp_client failed: {:#} — waiting for /restart",
                    e
                ));
                let _ = event_tx.send(AppEvent::AgentError {
                    session_id: None,
                    message: format!("{:#}", e),
                });
                // Don't break — a transient failure (e.g. agent crashed
                // during a self-update race) shouldn't permanently kill
                // the supervisor. Park here listening for /restart so the
                // user can recover without restarting the whole terminal.
                match restart_rx.recv().await {
                    Some(req) => {
                        if let Some(new_cmd) = req.agent_cmd {
                            startup_probe.log(&format!(
                                "run_acp_client switching agent: {} -> {}",
                                agent_cmd, new_cmd
                            ));
                            agent_cmd = new_cmd;
                        } else {
                            startup_probe.log(
                                "run_acp_client restart requested after failure — respawning agent",
                            );
                        }
                        let _ = event_tx.send(AppEvent::ConnectionStage(
                            "Restarting agent...".to_string(),
                        ));
                        continue;
                    }
                    None => {
                        startup_probe
                            .log("run_acp_client restart channel closed — exiting supervisor");
                        break;
                    }
                }
            }
        }
    }
}

async fn run_inner(
    agent_cmd: &str,
    acp_model_override: Option<String>,
    owner_tab_id: Option<String>,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    prompt_rx: &mut mpsc::UnboundedReceiver<PromptSubmission>,
    cancel_rx: &mut mpsc::UnboundedReceiver<CancelRequest>,
    new_session_rx: &mut mpsc::UnboundedReceiver<NewSessionForTab>,
    load_session_rx: &mut mpsc::UnboundedReceiver<LoadSessionForTab>,
    drop_session_rx: &mut mpsc::UnboundedReceiver<DropSessionRequest>,
    rename_session_rx: &mut mpsc::UnboundedReceiver<RenameSessionRequest>,
    restart_rx: &mut mpsc::UnboundedReceiver<RestartRequest>,
    shell_mgr: Arc<ShellManager>,
    wt_connected: bool,
) -> Result<ExitReason> {
    let startup_probe = StartupProbe::new();

    // Local re-parse for downstream model-handling (selection,
    // identity summary). The spawn itself reparses inside
    // `spawn_agent_process` — keeping a local parse avoids threading
    // lifetimes through the shared helper.
    let parts: Vec<&str> = agent_cmd.split_whitespace().collect();
    let raw_program = parts
        .first()
        .ok_or_else(|| anyhow::anyhow!("empty agent command"))?;
    let args = &parts[1..];

    // Whether this WTA process is hosting an Intelligent Terminal agent
    // pane (vs. a plain `wta --agent ...` invocation from a shell). Used
    // by `agent_pane_origin` to record only agent-pane-originated ACP
    // sessions in the on-disk index. `--owner-tab-id` is the load-bearing
    // signal: TerminalPage sets it when spawning the agent pane's WTA;
    // manual runs never set it.
    let is_agent_pane = owner_tab_id
        .as_ref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);

    // Resolve the user's active pane cwd before spawning the agent. Both the
    // child's working directory and the ACP `new_session` cwd derive from it,
    // so the agent's `execute_command` tool runs in the user's project rather
    // than wta.exe's own process cwd (which is typically %USERPROFILE% under
    // packaged identity — that's the bug we saw where `cargo run` resolved
    // against C:\Users\<user> and failed to find Cargo.toml).
    let active_pane_cwd = resolve_active_pane_cwd(&shell_mgr, wt_connected).await;
    startup_probe.log(&format!(
        "resolved active pane cwd: {:?}",
        active_pane_cwd.as_ref().map(|p| p.display().to_string())
    ));

    let spawned = crate::protocol::acp::spawn::spawn_agent_process(
        agent_cmd,
        active_pane_cwd.as_deref(),
    )?;
    let resolved_program = spawned.resolved_program.clone();
    let is_npx_launch = spawned.is_npx;
    let adapter_package = spawned.adapter_package.clone();
    let mut child = spawned.child;

    // For npx adapter launches, first run downloads the package
    // (~10s); surface that instead of a generic "Spawning…".
    let spawn_stage = if is_npx_launch {
        format!(
            "Setting up {} (first run downloads adapter, ~10s)…",
            adapter_package.as_deref().unwrap_or("agent")
        )
    } else {
        format!("Spawning {}...", resolved_program)
    };
    let _ = event_tx.send(AppEvent::ConnectionStage(spawn_stage.clone()));
    startup_probe.log(&format!(
        "{} cmd={} resolved={} pid={:?}",
        spawn_stage,
        agent_cmd,
        resolved_program,
        child.id()
    ));

    let prompt_timing = Arc::new(PromptTimingState::default());
    let outgoing = InstrumentedAgentWriter::new(child.stdin.take().unwrap(), prompt_timing.clone())
        .compat_write();
    startup_probe.log("Agent stdin pipe attached");

    let stdout = child.stdout.take().unwrap();
    startup_probe.log("Agent stdout pipe attached");
    let incoming = StartupInstrumentedReader::new(
        stdout,
        startup_probe.clone(),
        "stdout",
        prompt_timing.clone(),
    )
    .compat();

    if let Some(stderr) = child.stderr.take() {
        let stderr_probe = startup_probe.clone();
        tokio::task::spawn_local(async move {
            stderr_probe.log("Agent stderr pipe attached");
            let mut lines = BufReader::new(stderr).lines();
            let mut line_no = 0usize;
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        line_no += 1;
                        stderr_probe.log(&format!("agent stderr[{line_no}]: {}", line));
                    }
                    Ok(None) => {
                        stderr_probe.log("Agent stderr closed");
                        break;
                    }
                    Err(e) => {
                        stderr_probe.log(&format!("Agent stderr read error: {}", e));
                        break;
                    }
                }
            }
        });
    }

    // The wait task either logs a natural agent exit, or — when the
    // /restart slash-command fires `kill_req_tx` — terminates the child
    // synchronously so the next `run_inner` iteration can spawn a fresh
    // process without orphaning the old one.
    let (kill_req_tx, kill_req_rx) = tokio::sync::oneshot::channel::<()>();
    let mut kill_req_tx = Some(kill_req_tx);
    let child_probe = startup_probe.clone();
    tokio::task::spawn_local(async move {
        let mut kill_req_rx = kill_req_rx;
        tokio::select! {
            _ = &mut kill_req_rx => {
                if let Err(e) = child.kill().await {
                    child_probe.log(&format!("Agent kill failed: {}", e));
                } else {
                    child_probe.log("Agent process killed (restart)");
                }
                // Reap to avoid zombies on Unix; on Windows it's a no-op.
                let _ = child.wait().await;
            }
            status = child.wait() => {
                match status {
                    Ok(s) => child_probe.log(&format!("Agent process exited: {}", s)),
                    Err(e) => child_probe.log(&format!("Agent wait failed: {}", e)),
                }
            }
        }
    });

    let state = Arc::new(ClientState {
        event_tx: event_tx.clone(),
        shell_mgr: shell_mgr.clone(),
        prompt_timing,
    });

    let client = WtaClient {
        state: state.clone(),
    };

    let (conn, handle_io) = acp::ClientSideConnection::new(client, outgoing, incoming, |fut| {
        tokio::task::spawn_local(fut);
    });
    startup_probe.log("ACP client connection created");

    let io_probe = startup_probe.clone();
    tokio::task::spawn_local(async move {
        io_probe.log("ACP handle_io task started");
        if let Err(e) = handle_io.await {
            io_probe.log(&format!("ACP handle_io failed: {:#}", e));
            eprintln!("ACP I/O error: {:#}", e);
        } else {
            io_probe.log("ACP handle_io completed");
        }
    });

    // Initialize — with a timeout so misconfigured agents (e.g. non-ACP CLIs)
    // fail fast instead of hanging forever.
    let _ = event_tx.send(AppEvent::ConnectionStage("Initializing ACP...".to_string()));
    startup_probe.log("Initializing ACP");
    let init_future = conn.initialize(
        acp::InitializeRequest::new(acp::ProtocolVersion::V1)
            .client_capabilities(acp::ClientCapabilities::new().terminal(true))
            .client_info(
                acp::Implementation::new("wta", env!("CARGO_PKG_VERSION"))
                    .title("Windows Terminal Agent"),
            ),
    );
    // npx first-run downloads the adapter package (~5MB, can take
    // 20–30s on slow links). Native CLIs respond in <1s so the longer
    // timeout costs nothing on the hot path.
    let init_timeout_secs = if is_npx_launch { 60 } else { 15 };
    let agent_label: String = adapter_package
        .clone()
        .unwrap_or_else(|| raw_program.to_string());
    let init_resp = tokio::time::timeout(
        std::time::Duration::from_secs(init_timeout_secs),
        init_future,
    )
        .await
        .map_err(|_| anyhow::anyhow!(
            "ACP initialize timed out after {} s — '{}' did not respond. \
             First-run npx adapters download ~5MB; check network. \
             Built-in ACP agents: copilot, claude (via @zed-industries/claude-code-acp), \
             codex (via @zed-industries/codex-acp), gemini.",
            init_timeout_secs, agent_label
        ))?
        .map_err(|e| anyhow::anyhow!("initialize failed: {}", e))?;

    // Log the agent's initialize response for debugging
    startup_probe.log(&format!("Agent init response received: {:?}", init_resp));

    // Create session — also with a timeout.
    let _ = event_tx.send(AppEvent::ConnectionStage("Creating session...".to_string()));
    startup_probe.log("Creating session");
    let cwd = active_pane_cwd
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    startup_probe.log(&format!("Using session cwd={}", cwd.display()));
    let session_future = conn.new_session(acp::NewSessionRequest::new(cwd));
    let session = tokio::time::timeout(std::time::Duration::from_secs(15), session_future)
        .await
        .map_err(|_| anyhow::anyhow!("new_session timed out after 15 s"))?
        .map_err(|e| anyhow::anyhow!("new_session failed: {}", e))?;

    let session_id = session.session_id.clone();
    startup_probe.log(&format!("Session created: {}", session_id));
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
            "recording agent-pane session origin (startup)",
        );
        crate::agent_pane_origin::append_default(session_id.0.as_ref(), pane_for_index);
    }

    // Capture the agent's advertised model list. Settings UI rebuilds its
    // ComboBox from the `agent_status` event payload, where this gets
    // forwarded by App::publish_agent_status.
    let (available_models, current_model_id) = match &session.models {
        Some(state) => {
            startup_probe.log(&format!(
                "Session models: agent advertised {} model(s), current={}",
                state.available_models.len(),
                state.current_model_id.0,
            ));
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
        None => {
            startup_probe.log(
                "Session models: agent did not advertise any models (NewSessionResponse.models is None)",
            );
            (Vec::new(), None)
        }
    };

    // Resolve the model to apply: explicit `--acp-model` flag wins (used by
    // adapters like claude/codex via npx that can't carry --model on the
    // adapter cmdline), else fall back to extracting from the agent's own
    // `--model X` flag (copilot, gemini).
    let requested_model = acp_model_override
        .filter(|s| !s.trim().is_empty())
        .or_else(|| requested_model_id(raw_program, args));
    if let Some(requested_model) = requested_model {
        let _ = event_tx.send(AppEvent::ConnectionStage(format!(
            "Selecting model {}...",
            requested_model
        )));
        startup_probe.log(&format!("Setting ACP session model to {}", requested_model));
        conn.set_session_model(acp::SetSessionModelRequest::new(
            session_id.clone(),
            requested_model.clone(),
        ))
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "set_session_model failed for requested model {}: {}",
                requested_model,
                e
            )
        })?;
        startup_probe.log(&format!("ACP session model set to {}", requested_model));
    }

    // Notify app of connection
    let (registry_name, agent_model) = summarize_agent_identity(raw_program, args);
    let agent_version = init_resp
        .agent_info
        .as_ref()
        .map(|info| format!("v{}", info.version));
    // Prefer the agent's self-reported title/name from ACP over the registry fallback.
    let agent_name = init_resp
        .agent_info
        .as_ref()
        .and_then(|info| info.title.clone().or_else(|| Some(info.name.clone())))
        .unwrap_or(registry_name);
    let load_session_supported = init_resp.agent_capabilities.load_session;
    startup_probe.log(&format!(
        "Agent capabilities: loadSession={}",
        load_session_supported
    ));
    let _ = event_tx.send(AppEvent::AgentConnected {
        name: agent_name,
        model: agent_model,
        version: agent_version,
        session_id: session_id.to_string(),
        available_models,
        current_model_id,
        load_session_supported,
    });

    // Per-tab session cache, shared across all in-flight prompt tasks.
    // The startup session is bound to the owner tab GUID passed in by WT
    // (via --owner-tab-id) so the first prompt on that tab reuses the
    // already-created session instead of spawning a redundant one. When
    // `owner_tab_id` is None (manual `wta` runs, no host pane), fall back
    // to the legacy "0" key to match the App-side DEFAULT_TAB_ID
    // placeholder.
    let tab_to_session: Arc<tokio::sync::Mutex<HashMap<String, acp::SessionId>>> =
        Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    {
        let mut g = tab_to_session.lock().await;
        let initial_tab_key = owner_tab_id
            .clone()
            .unwrap_or_else(|| "0".to_string());
        g.insert(initial_tab_key, session_id.clone());
    }

    // Tracks which template each session last saw, so we can drop the
    // template body on subsequent same-kind turns. Cleared on session
    // teardown (see new_session_rx / drop_session_rx arms).
    let template_memo = TemplateMemo::default();

    // Same-tab single-flight guard: at most one prompt in flight per tab.
    // The ACP protocol allows concurrent prompts across sessions, but
    // within a session the turns must be ordered, so we enforce per-tab
    // serialization here. Per-tab + per-session match because each tab
    // gets its own session.
    let in_flight_tabs: Arc<std::sync::Mutex<HashSet<String>>> =
        Arc::new(std::sync::Mutex::new(HashSet::new()));

    // Per-prompt cancel oneshot, keyed on SessionId. Each spawned prompt
    // task registers a sender here on entry and removes it on exit. The
    // cancel listener task signals through it to break the spawned task
    // out of `conn.prompt().await` even if the agent is slow to honor
    // session/cancel.
    let cancel_signals: Arc<std::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>> =
        Arc::new(std::sync::Mutex::new(HashMap::new()));

    // The connection is shared across all spawned prompt tasks.
    let conn = Arc::new(conn);

    // Main event loop. `tokio::select!` lets the cancel/new_session/restart
    // receivers stay borrowed by `&mut` (rather than moved into detached
    // tasks via `mem::replace`) so they survive across reconnects.
    //
    // The async work for cancel and new_session is offloaded to
    // `spawn_local` subtasks so a slow agent (e.g. a 15s new_session
    // call) doesn't stall prompt dispatch.
    let exit_reason = loop {
        tokio::select! {
            biased;
            // /restart: priority over other arms via `biased;` so a
            // queued prompt can't sneak in front of a kill request.
            Some(req) = restart_rx.recv() => {
                tracing::info!(target: "acp_restart", "restart requested, new_agent={:?}", req.agent_cmd);
                if let Some(tx) = kill_req_tx.take() {
                    let _ = tx.send(());
                }
                // Signal every in-flight prompt task to drop out, so
                // they don't keep emitting chunks against the dead
                // connection.
                let mut signals = cancel_signals.lock().unwrap();
                for (_, sig) in signals.drain() {
                    let _ = sig.send(());
                }
                break ExitReason::Restart { agent_cmd: req.agent_cmd };
            }
            Some(req) = cancel_rx.recv() => {
                let session_id_str = req.session_id.clone();
                tracing::info!(target: "acp_cancel", session_id = %session_id_str, "cancel requested");
                // Local oneshot first — it's the critical path for
                // breaking the spawned prompt task out of conn.prompt().
                if let Some(sig) = cancel_signals.lock().unwrap().remove(&session_id_str) {
                    let _ = sig.send(());
                }
                // Best-effort agent notification. Spawned so the loop
                // stays responsive even if the agent is slow to ack.
                let conn_for_cancel = Arc::clone(&conn);
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
            Some(req) = new_session_rx.recv() => {
                tracing::info!(
                    target: "acp_new_session",
                    tab = %req.tab_id,
                    "new_session requested"
                );
                let conn_for_new = Arc::clone(&conn);
                let tab_to_session_for_new = Arc::clone(&tab_to_session);
                let template_memo_for_new = template_memo.clone();
                let cancel_signals_for_new = Arc::clone(&cancel_signals);
                let event_tx_for_new = event_tx.clone();
                let is_agent_pane_for_new = is_agent_pane;
                tokio::task::spawn_local(async move {
                    let cwd = req
                        .cwd
                        .clone()
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

                    let old_sid: Option<acp::SessionId> = {
                        let mut g = tab_to_session_for_new.lock().await;
                        g.remove(&req.tab_id)
                    };

                    if let Some(ref old) = old_sid {
                        let old_str = old.to_string();
                        template_memo_for_new.forget(&old_str).await;
                        if let Some(sig) = cancel_signals_for_new
                            .lock()
                            .unwrap()
                            .remove(&old_str)
                        {
                            let _ = sig.send(());
                        }
                        let _ = conn_for_new
                            .cancel(acp::CancelNotification::new(old.clone()))
                            .await;
                    }

                    let new_session = match conn_for_new
                        .new_session(acp::NewSessionRequest::new(cwd))
                        .await
                    {
                        Ok(s) => s,
                        Err(e) => {
                            let _ = event_tx_for_new.send(AppEvent::AgentError {
                                session_id: None,
                                message: format!("/new failed for tab {}: {}", req.tab_id, e),
                            });
                            return;
                        }
                    };

                    let new_sid = new_session.session_id.clone();
                    if is_agent_pane_for_new {
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

                    {
                        let mut g = tab_to_session_for_new.lock().await;
                        g.insert(req.tab_id.clone(), new_sid.clone());
                    }

                    let _ = event_tx_for_new.send(AppEvent::SessionAttached {
                        tab_id: req.tab_id.clone(),
                        session_id: new_sid.to_string(),
                        available_models: per_tab_models,
                        current_model_id: per_tab_current,
                    });
                });
            }
            Some(req) = load_session_rx.recv() => {
                tracing::info!(
                    target: "acp_load_session",
                    tab = %req.tab_id,
                    session_id = %req.session_id,
                    "load_session requested"
                );
                let conn_for_load = Arc::clone(&conn);
                let tab_to_session_for_load = Arc::clone(&tab_to_session);
                let cancel_signals_for_load = Arc::clone(&cancel_signals);
                let event_tx_for_load = event_tx.clone();
                tokio::task::spawn_local(async move {
                    let cwd = req
                        .cwd
                        .clone()
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

                    // If the target tab already holds a session, cancel
                    // any in-flight prompt for it and drop the binding —
                    // we're about to replace it with the loaded one.
                    // Mirrors the new_session_rx prelude.
                    let old_sid: Option<acp::SessionId> = {
                        let mut g = tab_to_session_for_load.lock().await;
                        g.remove(&req.tab_id)
                    };

                    if let Some(ref old) = old_sid {
                        let old_str = old.to_string();
                        if let Some(sig) = cancel_signals_for_load
                            .lock()
                            .unwrap()
                            .remove(&old_str)
                        {
                            let _ = sig.send(());
                        }
                        let _ = conn_for_load
                            .cancel(acp::CancelNotification::new(old.clone()))
                            .await;
                    }

                    let session_id = acp::SessionId::new(req.session_id.clone());
                    let load_req = acp::LoadSessionRequest::new(session_id.clone(), cwd);

                    // 60s timeout: matches new_session's first-run npx
                    // adapter timeout. `session/load` may replay history
                    // before returning, so on large session stores the
                    // call can take a while; but a 60s ceiling keeps us
                    // from hanging forever if the agent never responds.
                    let load_future = conn_for_load.load_session(load_req);
                    let load_result = tokio::time::timeout(
                        std::time::Duration::from_secs(60),
                        load_future,
                    )
                    .await;

                    match load_result {
                        Ok(Ok(_resp)) => {
                            tracing::info!(
                                target: "acp_load_session",
                                tab = %req.tab_id,
                                session_id = %req.session_id,
                                "load_session succeeded"
                            );
                            {
                                let mut g = tab_to_session_for_load.lock().await;
                                g.insert(req.tab_id.clone(), session_id.clone());
                            }
                            // The agent replays past content via
                            // session/update notifications that route
                            // through the existing session_to_tab map.
                            // SessionAttached primes that mapping.
                            let _ = event_tx_for_load.send(AppEvent::SessionAttached {
                                tab_id: req.tab_id.clone(),
                                session_id: session_id.to_string(),
                                // load_session/LoadSessionResponse does
                                // not carry the per-session model list
                                // (only modes); leave the previously-
                                // published list alone.
                                available_models: Vec::new(),
                                current_model_id: None,
                            });
                            // Confirmation note so the user sees the
                            // tab transition out of "Resuming..." even
                            // if the agent's replay is empty or
                            // delayed. The "Resuming..." note was
                            // pushed by the inbound load_session
                            // handler before this task ran.
                            let _ = event_tx_for_load.send(AppEvent::TabSystemMessage {
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
                            // TabError routes to the specific new tab
                            // (the historical session has no live
                            // session_id we could thread through
                            // AgentError, and AgentError with
                            // session_id=None would land in the
                            // currently-active tab instead).
                            let _ = event_tx_for_load.send(AppEvent::TabError {
                                tab_id: req.tab_id.clone(),
                                message: format!(
                                    "Failed to resume session in agent pane: {}. \
                                     The connected agent may not recognize this \
                                     session id (CLI mismatch), or `session/load` \
                                     is unsupported.",
                                    e
                                ),
                            });
                        }
                        Err(_) => {
                            tracing::warn!(
                                target: "acp_load_session",
                                tab = %req.tab_id,
                                session_id = %req.session_id,
                                "load_session timed out after 60s"
                            );
                            let _ = event_tx_for_load.send(AppEvent::TabError {
                                tab_id: req.tab_id.clone(),
                                message:
                                    "Resume timed out after 60s — the agent \
                                     did not respond to `session/load`."
                                        .to_string(),
                            });
                        }
                    }
                });
            }
            Some(req) = drop_session_rx.recv() => {
                tracing::info!(
                    target: "acp_drop_session",
                    tab = %req.tab_id,
                    "drop_session requested (no replacement)"
                );
                let conn_for_drop = Arc::clone(&conn);
                let tab_to_session_for_drop = Arc::clone(&tab_to_session);
                let template_memo_for_drop = template_memo.clone();
                let cancel_signals_for_drop = Arc::clone(&cancel_signals);
                tokio::task::spawn_local(async move {
                    let old_sid: Option<acp::SessionId> = {
                        let mut g = tab_to_session_for_drop.lock().await;
                        g.remove(&req.tab_id)
                    };
                    if let Some(old) = old_sid {
                        // Signal any in-flight prompt for this session to
                        // bail out of conn.prompt().await immediately, then
                        // send a session/cancel to the agent. Mirrors the
                        // new_session_rx cancel path, minus the new_session
                        // round-trip.
                        let old_str = old.to_string();
                        template_memo_for_drop.forget(&old_str).await;
                        if let Some(sig) = cancel_signals_for_drop
                            .lock()
                            .unwrap()
                            .remove(&old_str)
                        {
                            let _ = sig.send(());
                        }
                        if let Err(e) = conn_for_drop
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
            Some(req) = rename_session_rx.recv() => {
                let tab_to_session_for_rename = Arc::clone(&tab_to_session);
                tokio::task::spawn_local(async move {
                    let mut g = tab_to_session_for_rename.lock().await;
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
                    &state.prompt_timing,
                    wt_connected,
                    is_agent_pane,
                );
            }
            else => break ExitReason::Done,
        }
    };

    Ok(exit_reason)
}

/// Spawn a per-prompt task that resolves the tab's ACP session (lazily
/// creating one if needed), instruments timing, runs `conn.prompt`, and
/// cleans up state on completion. Extracted from the old inline body in
/// the prompt while-loop so the new select-based loop body stays terse.
#[allow(clippy::too_many_arguments)]
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
                    let new_session = match conn_task
                        .new_session(acp::NewSessionRequest::new(cwd))
                        .await
                    {
                        Ok(s) => s,
                        Err(e) => {
                            let _ = event_tx_task.send(AppEvent::AgentError {
                                session_id: None,
                                message: format!(
                                    "new_session failed for tab {}: {}",
                                    tab_key_task, e
                                ),
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
            let (text, prompt_source, prompt_name) = build_prompt_text(
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
                    complete_prompt_request(
                        result,
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
        complete_prompt_request, requested_model_id, summarize_agent_identity, user_locale_tag,
        PromptTimingState,
    };
    use crate::app::AppEvent;
    use tokio::sync::mpsc;

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

    #[test]
    fn humanizes_brand_and_model_for_copilot() {
        let args = ["--acp", "--stdio", "--model=claude-haiku-4.5"];
        let (brand, model) = summarize_agent_identity("copilot", &args);

        assert_eq!(brand, "GitHub Copilot");
        assert_eq!(model.as_deref(), Some("Claude Haiku 4.5"));
    }

    #[test]
    fn humanizes_gpt_5_mini_for_copilot() {
        let args = ["--acp", "--stdio", "--model=gpt-5-mini"];
        let (brand, model) = summarize_agent_identity("copilot", &args);

        assert_eq!(brand, "GitHub Copilot");
        assert_eq!(model.as_deref(), Some("GPT 5 Mini"));
    }

    #[test]
    fn requested_model_returns_owned_value() {
        let args = ["--acp", "--stdio", "--model", "claude-haiku-4.5"];
        assert_eq!(
            requested_model_id("copilot", &args).as_deref(),
            Some("claude-haiku-4.5")
        );
    }

    #[tokio::test]
    async fn successful_prompt_completion_emits_message_end_only() {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let prompt_timing = PromptTimingState::default();

        complete_prompt_request(
            Ok::<(), &str>(()),
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
    async fn failed_prompt_completion_emits_error_only() {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let prompt_timing = PromptTimingState::default();

        complete_prompt_request(
            Err::<(), _>("boom"),
            &prompt_timing,
            &event_tx,
            "test-session".to_string(),
        )
        .await;

        match event_rx.try_recv() {
            Ok(AppEvent::AgentError { session_id, message }) => {
                assert_eq!(session_id.as_deref(), Some("test-session"));
                assert_eq!(message, "prompt error: boom");
            }
            Ok(_) => panic!("expected AgentError"),
            Err(err) => panic!("expected AgentError, got channel error: {err}"),
        }
        assert!(event_rx.try_recv().is_err());
    }
}
