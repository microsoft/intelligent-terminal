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
/// re-initializes from scratch.
#[derive(Debug, Clone, Default)]
pub struct RestartRequest;

/// How [`run_inner`] terminated. The outer driver in [`run_acp_client`]
/// uses this to decide whether to respawn the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExitReason {
    /// Loop exited because all sender halves dropped (process shutdown).
    Done,
    /// `/restart` requested. Outer driver should re-enter `run_inner`.
    Restart,
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
            // PromptResponse, which leaves `pending_agent_response`
            // truncated when `AgentMessageEnd` triggers finalize. We sleep
            // briefly so stragglers land in pending_agent_response before
            // finalize_agent_response_for takes ownership of it.
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

    tracing::debug!(
        target: "acp.terminal_context",
        target_pane_id = %target_pane_id,
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
        "buffer": buffer,
    }))
    .ok()
}

async fn build_prompt_text(
    prompt_id: u64,
    submitted_at_unix_s: f64,
    user_text: &str,
    is_autofix: bool,
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
                    let platform = if cfg!(target_os = "windows") {
                        "windows"
                    } else if cfg!(target_os = "macos") {
                        "macos"
                    } else {
                        "linux"
                    };
                    let json = serde_json::to_string(&serde_json::json!({
                        "platform": platform,
                        "profile": profile,
                        "cwd": cwd,
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
    let prompt_body = prompt::merge_runtime_sections(&planner_template.content, &runtime_sections);
    // Autofix prompt is self-contained — terminal buffer is injected via the
    // runtime context marker and the instructions are in the template itself.
    // No "## User Request" section is needed.
    let prompt = if is_autofix {
        prompt_body
    } else {
        format!("{}\n\n## User Request\n{}", prompt_body, user_text)
    };
    prompt_timing_log(
        prompt_id,
        submitted_at_unix_s,
        "prompt_assembled",
        &format!(
            "assemble_dt={:.3}s total_context_dt={:.3}s prompt_len={}",
            assemble_started.elapsed().as_secs_f64(),
            total_started.elapsed().as_secs_f64(),
            prompt.len()
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
                    let _ = self.state.event_tx.send(AppEvent::ToolCallUpdate {
                        session_id: sid,
                        id: update.tool_call_id.to_string(),
                        status: format!("{:?}", status),
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

/// Top-level ACP client task: spawn agent, handshake, prompt loop.
pub async fn run_acp_client(
    agent_cmd: String,
    acp_model_override: Option<String>,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    mut prompt_rx: mpsc::UnboundedReceiver<PromptSubmission>,
    mut cancel_rx: mpsc::UnboundedReceiver<CancelRequest>,
    mut new_session_rx: mpsc::UnboundedReceiver<NewSessionForTab>,
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
            event_tx.clone(),
            &mut prompt_rx,
            &mut cancel_rx,
            &mut new_session_rx,
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
            Ok(ExitReason::Restart) => {
                startup_probe.log("run_acp_client restart requested — respawning agent");
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
                    Some(_) => {
                        startup_probe.log(
                            "run_acp_client restart requested after failure — respawning agent",
                        );
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
    event_tx: mpsc::UnboundedSender<AppEvent>,
    prompt_rx: &mut mpsc::UnboundedReceiver<PromptSubmission>,
    cancel_rx: &mut mpsc::UnboundedReceiver<CancelRequest>,
    new_session_rx: &mut mpsc::UnboundedReceiver<NewSessionForTab>,
    restart_rx: &mut mpsc::UnboundedReceiver<RestartRequest>,
    shell_mgr: Arc<ShellManager>,
    wt_connected: bool,
) -> Result<ExitReason> {
    let startup_probe = StartupProbe::new();

    // Parse agent command into program + args, resolving bare names (e.g.
    // "gemini" → "gemini.cmd") via the agent registry so npm-installed CLIs
    // are found on PATH.
    let parts: Vec<&str> = agent_cmd.split_whitespace().collect();
    let raw_program = parts
        .first()
        .ok_or_else(|| anyhow::anyhow!("empty agent command"))?;
    let args = &parts[1..];
    let resolved_program = crate::agent_registry::resolve_bare_agent_name(raw_program);
    let needs_cmd = crate::coordinator::needs_shell_launch(&resolved_program);

    // Spawn agent subprocess
    let program = if needs_cmd { "cmd" } else { resolved_program.as_str() };
    // For adapter-style launches (npx -y @zed/...-acp), surface a more
    // accurate stage hint — first run downloads the package (~10s).
    let spawn_stage = if resolved_program.eq_ignore_ascii_case("npx")
        || resolved_program.eq_ignore_ascii_case("npx.cmd")
        || resolved_program.eq_ignore_ascii_case("npx.exe")
    {
        let adapter = args
            .iter()
            .find(|a| a.starts_with('@'))
            .copied()
            .unwrap_or("agent");
        format!("Setting up {} (first run downloads adapter, ~10s)…", adapter)
    } else {
        format!("Spawning {}...", resolved_program)
    };
    let _ = event_tx.send(AppEvent::ConnectionStage(spawn_stage.clone()));
    startup_probe.log(&format!("{} cmd={} resolved={} needs_cmd={}", spawn_stage, agent_cmd, resolved_program, needs_cmd));

    let mut cmd = tokio::process::Command::new(program);
    if needs_cmd {
        cmd.arg("/c").arg(&resolved_program);
    }
    let mut child = cmd
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn agent '{}': {}", agent_cmd, e))?;

    let child_pid = child.id();
    startup_probe.log(&format!("Spawned {} pid={:?}", program, child_pid));

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
    // npx-launched adapters need a generous window because the first run
    // downloads the package (~5MB, can take 20–30s on slow links). Native
    // ACP CLIs respond in <1s, so the longer timeout has zero cost on the
    // hot path — it only matters when a download is actually happening.
    let is_npx_launch = resolved_program.eq_ignore_ascii_case("npx")
        || resolved_program.eq_ignore_ascii_case("npx.cmd")
        || resolved_program.eq_ignore_ascii_case("npx.exe");
    let init_timeout_secs = if is_npx_launch { 60 } else { 15 };
    // Pick a friendly name for error reporting. For npx launches the
    // first @-prefixed arg is the adapter package; otherwise use the
    // resolved program path.
    let agent_label: String = if is_npx_launch {
        args.iter()
            .find(|a| a.starts_with('@'))
            .map(|s| s.to_string())
            .unwrap_or_else(|| raw_program.to_string())
    } else {
        raw_program.to_string()
    };
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
    let cwd = std::env::current_dir().unwrap_or_default();
    startup_probe.log(&format!("Using session cwd={}", cwd.display()));
    let session_future = conn.new_session(acp::NewSessionRequest::new(cwd));
    let session = tokio::time::timeout(std::time::Duration::from_secs(15), session_future)
        .await
        .map_err(|_| anyhow::anyhow!("new_session timed out after 15 s"))?
        .map_err(|e| anyhow::anyhow!("new_session failed: {}", e))?;

    let session_id = session.session_id.clone();
    startup_probe.log(&format!("Session created: {}", session_id));

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
    let _ = event_tx.send(AppEvent::AgentConnected {
        name: agent_name,
        model: agent_model,
        version: agent_version,
        session_id: session_id.to_string(),
        available_models,
        current_model_id,
    });

    // Per-tab session cache, shared across all in-flight prompt tasks.
    // The startup session is bound to tab "0" so the agent_status event
    // pipeline lights up immediately. New tabs lazily create their own
    // session on first prompt — see `ensure_session_for_tab`.
    let tab_to_session: Arc<tokio::sync::Mutex<HashMap<String, acp::SessionId>>> =
        Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    {
        let mut g = tab_to_session.lock().await;
        g.insert("0".to_string(), session_id.clone());
    }

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
            Some(_) = restart_rx.recv() => {
                tracing::info!(target: "acp_restart", "restart requested");
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
                break ExitReason::Restart;
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
                let cancel_signals_for_new = Arc::clone(&cancel_signals);
                let event_tx_for_new = event_tx.clone();
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
            Some(prompt) = prompt_rx.recv() => {
                dispatch_prompt(
                    prompt,
                    &conn,
                    &tab_to_session,
                    &in_flight_tabs,
                    &cancel_signals,
                    &event_tx,
                    &shell_mgr,
                    &state.prompt_timing,
                    wt_connected,
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
    in_flight_tabs: &Arc<std::sync::Mutex<HashSet<String>>>,
    cancel_signals: &Arc<std::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>>,
    event_tx: &mpsc::UnboundedSender<AppEvent>,
    shell_mgr: &Arc<ShellManager>,
    prompt_timing: &Arc<PromptTimingState>,
    wt_connected: bool,
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
        in_flight_tabs_task,
        cancel_signals_task,
        event_tx_task,
        shell_mgr_task,
        prompt_timing_task,
        tab_key_task,
        wt_connected,
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
    in_flight_tabs_task: Arc<std::sync::Mutex<HashSet<String>>>,
    cancel_signals_task: Arc<std::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>>,
    event_tx_task: mpsc::UnboundedSender<AppEvent>,
    shell_mgr_task: Arc<ShellManager>,
    prompt_timing_task: Arc<PromptTimingState>,
    tab_key_task: String,
    wt_connected: bool,
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

            prompt_timing_task.activate(&prompt_session_id_str, &prompt);
            let _ = event_tx_task.send(AppEvent::ProgressStatus {
                session_id: Some(prompt_session_id_str.clone()),
                status: "Preparing context...".to_string(),
            });
            let (text, prompt_source, prompt_name) = build_prompt_text(
                prompt.id,
                prompt.submitted_at_unix_s,
                &prompt.text,
                prompt.is_autofix,
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
        complete_prompt_request, requested_model_id, summarize_agent_identity,
        PromptTimingState,
    };
    use crate::app::AppEvent;
    use tokio::sync::mpsc;

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
