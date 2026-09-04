use std::collections::HashMap;
use std::sync::Mutex;

pub(crate) fn now_unix_s() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

pub(crate) fn prompt_preview(text: &str) -> String {
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

pub(crate) fn prompt_timing_log(
    turn_id: u64,
    submitted_at_unix_s: f64,
    phase: &str,
    details: &str,
) {
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
    is_byok: bool,
    agent_id: String,
}

/// Concurrent-prompt-aware timing tracker. With M3's spawn-per-prompt
/// model, multiple prompts can be in flight at the same time; each is
/// keyed by its ACP `SessionId`. Byte-level observers (writes/reads on
/// the shared stdio) update every in-flight prompt that hasn't yet
/// recorded its first byte — `is_none()` guards make that a no-op
/// once a value is set.
#[derive(Default)]
pub(crate) struct PromptTimingState {
    active: Mutex<HashMap<String, ActivePromptTiming>>,
}

impl PromptTimingState {
    pub(crate) fn activate(
        &self,
        session_id: &str,
        prompt_id: u64,
        _prompt_text: &str,
        submitted_at_unix_s: f64,
        is_byok: bool,
        agent_id: &str,
    ) {
        let now = now_unix_s();
        let mut active = self.active.lock().unwrap();
        active.insert(
            session_id.to_string(),
            ActivePromptTiming {
                id: prompt_id,
                submitted_at_unix_s,
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
                is_byok,
                agent_id: agent_id.to_string(),
            },
        );
        drop(active);

        prompt_timing_log(
            prompt_id,
            submitted_at_unix_s,
            "prompt_received",
            &format!(
                "queue_delay={}",
                format_elapsed(Some(submitted_at_unix_s), Some(now)),
            ),
        );
    }

    pub(crate) fn mark_context_ready(&self, session_id: &str, prompt_len: usize) {
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

    pub(crate) fn mark_prompt_sent(&self, session_id: &str) {
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

    pub(crate) fn observe_session_update(&self, session_id: &str, kind: &str) {
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

    pub(crate) fn observe_first_text(&self, session_id: &str, text_len: usize) {
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
                let agent_id = active.agent_id.clone();
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
                    let first_token_latency_ms = sent_mono.elapsed().as_secs_f64() * 1000.0;
                    crate::telemetry::log_agent_response_first_token(
                        session_id,
                        first_token_latency_ms,
                        u32::try_from(text_len).unwrap_or(u32::MAX),
                        &agent_id,
                    );
                }
            }
        }
    }

    pub(crate) fn observe_first_tool_call(&self, session_id: &str, title: Option<&str>) {
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
                acp_trace_content(&format!(
                    "turn {turn_id} first_tool_call title={title_preview:?}"
                ));
            }
        }
    }

    pub(crate) fn permission_requested(&self, session_id: &str, description: &str) {
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

    pub(crate) fn permission_resolved(&self, session_id: &str, outcome: &str) {
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

    pub(crate) fn complete(
        &self,
        session_id: &str,
        success: bool,
        error: Option<&str>,
    ) -> Option<String> {
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
                active_prompt.is_byok,
                &active_prompt.agent_id,
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

fn acp_log(msg: &str) {
    tracing::debug!(target: "acp", "{}", msg);
}

fn acp_trace_content(msg: &str) {
    tracing::trace!(target: "acp.content", "{}", msg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_preview_escapes_newlines_and_normalizes_crlf() {
        assert_eq!(prompt_preview("a\r\nb\rc\nd"), "a\\nb\\nc\\nd");
    }

    #[test]
    fn prompt_preview_truncates_past_80_chars_with_ellipsis() {
        let long: String = std::iter::repeat('x').take(100).collect();
        let out = prompt_preview(&long);
        assert_eq!(out.chars().count(), 83, "80 chars + \"...\"");
        assert!(out.ends_with("..."));
        // Exactly 80 chars must NOT get an ellipsis.
        let exact: String = std::iter::repeat('y').take(80).collect();
        let out80 = prompt_preview(&exact);
        assert_eq!(out80, exact);
        assert!(!out80.ends_with("..."));
    }

    #[test]
    fn prompt_preview_is_char_safe_with_multibyte() {
        let long: String = std::iter::repeat('é').take(100).collect();
        let out = prompt_preview(&long);
        // Must not panic and must cut on a char boundary at 80 + "...".
        assert_eq!(out.chars().count(), 83);
    }

    #[test]
    fn format_elapsed_formats_positive_delta_and_handles_invalid() {
        assert_eq!(format_elapsed(Some(1.0), Some(2.5)), "1.500s");
        assert_eq!(format_elapsed(Some(2.0), Some(2.0)), "0.000s");
        // end < start, or any missing endpoint → "n/a".
        assert_eq!(format_elapsed(Some(2.0), Some(1.0)), "n/a");
        assert_eq!(format_elapsed(None, Some(1.0)), "n/a");
        assert_eq!(format_elapsed(Some(1.0), None), "n/a");
        assert_eq!(format_elapsed(None, None), "n/a");
    }

    #[test]
    fn first_visible_text_gap_prefers_first_event_then_transport() {
        // first_event present → measured from it, labeled "first_event".
        let (gap, label) = first_visible_text_gap(Some(1.0), Some(0.5), Some(1.4));
        assert_eq!(label, "first_event");
        assert_eq!(gap, "0.400s");
        // No first_event but transport read present → from transport.
        let (gap, label) = first_visible_text_gap(None, Some(0.5), Some(1.5));
        assert_eq!(label, "first_transport_read");
        assert_eq!(gap, "1.000s");
        // Neither present → n/a.
        let (gap, label) = first_visible_text_gap(None, None, Some(1.5));
        assert_eq!(label, "n/a");
        assert_eq!(gap, "n/a");
    }

    #[test]
    fn final_timing_note_composes_both_phases() {
        let note = final_timing_note(1.0, Some(1.2), Some(1.5), 2.0);
        assert_eq!(
            note,
            "submit->context_ready 0.200s | prompt_sent->options_shown 0.500s"
        );
    }
}
