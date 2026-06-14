//! Codex rollout classifier.
//!
//! Activity model — **turn-based** (same as Copilot). A Codex session writes a
//! per-task `event_msg/task_started` … `event_msg/task_complete` bracket; the
//! agent is busy for that whole turn (thinking + tool calls + text). So:
//!   * turn start: `{"type":"event_msg","payload":{"type":"task_started"}}`   → WORKING
//!   * turn end:   `{"type":"event_msg","payload":{"type":"task_complete"}}`   → IDLE
//!   * tool start: `{"type":"response_item","payload":{"type":"function_call","name":"shell_command"}}`
//!   * tool end:   `{"type":"response_item","payload":{"type":"function_call_output",...}}` (ignored)
//!
//! task_started is what makes the row appear in the session view *immediately*
//! (the first `function_call` can be 10-30s into the turn). task_complete is a
//! per-turn boundary → IDLE, NOT a session end — the session stays resumable;
//! `function_call_output` is ignored because IDLE is owned by task_complete.

use crate::agent_sessions::{is_user_input_tool, AgentKey, SessionEvent};

pub fn classify(record: &serde_json::Value, key: &AgentKey) -> Vec<SessionEvent> {
    let kind = record.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let payload_type = record
        .get("payload")
        .and_then(|p| p.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match (kind, payload_type) {
        // Turn boundaries. task_started → WORKING surfaces the session in the
        // view immediately and brackets the whole working turn; task_complete
        // → IDLE (a per-turn boundary, NOT a session end — the session stays
        // resumable, unlike the previous SessionStopped mapping which drove a
        // Class-B row to Ended / "no status").
        ("event_msg", "task_started") => vec![SessionEvent::ToolStarting {
            key: key.clone(),
            tool_name: String::new(),
        }],
        ("event_msg", "task_complete") => vec![SessionEvent::ToolCompleted { key: key.clone() }],
        ("response_item", "function_call")
        | ("response_item", "local_shell_call")
        | ("response_item", "custom_tool_call") => {
            let payload = record.get("payload");
            let tool = payload
                .and_then(|p| p.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // Codex's permission gate lives inside the call's `arguments`: a
            // command requesting escalated sandbox permissions is blocked on
            // the user → ATTENTION. Check it before the generic tool handling.
            if let Some(reason) = codex_permission_reason(payload) {
                vec![SessionEvent::Notification {
                    key: key.clone(),
                    message: reason,
                }]
            } else if is_user_input_tool(&tool) {
                vec![SessionEvent::Notification {
                    key: key.clone(),
                    message: tool,
                }]
            } else {
                vec![SessionEvent::ToolStarting {
                    key: key.clone(),
                    tool_name: tool,
                }]
            }
        }
        // function_call_output / custom_tool_call_output intentionally dropped:
        // IDLE is owned by task_complete, so a tool finishing mid-turn must not
        // flip the row to IDLE while the agent keeps working.
        _ => Vec::new(),
    }
}

/// If a Codex function_call requests escalated sandbox permissions, return the
/// human-readable approval prompt (`justification`, falling back to a generic
/// message). `None` otherwise. `arguments` is a JSON *string* nested inside the
/// payload, so it needs a second parse.
fn codex_permission_reason(payload: Option<&serde_json::Value>) -> Option<String> {
    let args_str = payload?.get("arguments")?.as_str()?;
    let args: serde_json::Value = serde_json::from_str(args_str).ok()?;
    let needs_escalation = args
        .get("sandbox_permissions")
        .and_then(|v| v.as_str())
        .map(|s| s == "require_escalated")
        .unwrap_or(false);
    if !needs_escalation {
        return None;
    }
    let reason = args
        .get("justification")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("permission requested")
        .to_string();
    Some(reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(s: &str) -> serde_json::Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn function_call_maps_to_tool_starting() {
        let r = rec(
            r#"{"type":"response_item","payload":{"type":"function_call","name":"shell_command"}}"#,
        );
        let out = classify(&r, &"k".to_string());
        assert_eq!(
            out,
            vec![SessionEvent::ToolStarting {
                key: "k".to_string(),
                tool_name: "shell_command".to_string()
            }]
        );
    }

    #[test]
    fn require_escalated_function_call_maps_to_attention() {
        // Codex embeds its permission gate inside the function_call's
        // `arguments` (a JSON *string*): a command needing escalated sandbox
        // permissions is waiting for the user to approve/deny → ATTENTION,
        // with the human-readable `justification` as the reason.
        let r = rec(
            r#"{"type":"response_item","payload":{"type":"function_call","name":"shell_command","arguments":"{\"command\":\"ls\",\"sandbox_permissions\":\"require_escalated\",\"justification\":\"Allow listing System32?\"}"}}"#,
        );
        assert_eq!(
            classify(&r, &"k".to_string()),
            vec![SessionEvent::Notification {
                key: "k".to_string(),
                message: "Allow listing System32?".to_string()
            }]
        );
    }

    #[test]
    fn require_escalated_without_justification_uses_fallback() {
        let r = rec(
            r#"{"type":"response_item","payload":{"type":"function_call","name":"shell_command","arguments":"{\"sandbox_permissions\":\"require_escalated\"}"}}"#,
        );
        assert!(matches!(
            classify(&r, &"k".to_string()).as_slice(),
            [SessionEvent::Notification { message, .. }] if message == "permission requested"
        ));
    }

    #[test]
    fn normal_sandbox_function_call_stays_working() {
        // A command that does NOT require escalation is autonomous → WORKING.
        let r = rec(
            r#"{"type":"response_item","payload":{"type":"function_call","name":"shell_command","arguments":"{\"command\":\"ls\",\"sandbox_permissions\":\"auto\"}"}}"#,
        );
        assert!(matches!(
            classify(&r, &"k".to_string()).as_slice(),
            [SessionEvent::ToolStarting { .. }]
        ));
    }

    #[test]
    fn task_started_maps_to_working() {
        // task_started makes the session appear immediately (before the first
        // function_call, which can be 10-30s in) and marks the whole turn
        // WORKING. Reuses ToolStarting with an empty tool name.
        let r = rec(r#"{"type":"event_msg","payload":{"type":"task_started"}}"#);
        assert_eq!(
            classify(&r, &"k".to_string()),
            vec![SessionEvent::ToolStarting {
                key: "k".to_string(),
                tool_name: String::new()
            }]
        );
    }

    #[test]
    fn function_call_output_is_dropped() {
        // IDLE is owned by task_complete; a tool finishing mid-turn must not
        // flip the row to IDLE while the agent keeps working.
        let r = rec(r#"{"type":"response_item","payload":{"type":"function_call_output"}}"#);
        assert!(classify(&r, &"k".to_string()).is_empty());
    }

    #[test]
    fn task_complete_maps_to_idle() {
        // task_complete is a per-turn boundary → IDLE, NOT a session end. The
        // session stays in the registry as Idle (resumable).
        let r = rec(r#"{"type":"event_msg","payload":{"type":"task_complete"}}"#);
        assert_eq!(
            classify(&r, &"k".to_string()),
            vec![SessionEvent::ToolCompleted {
                key: "k".to_string()
            }]
        );
    }

    #[test]
    fn plain_message_yields_nothing() {
        let r = rec(r#"{"type":"response_item","payload":{"type":"message","role":"user"}}"#);
        assert!(classify(&r, &"k".to_string()).is_empty());
    }
}
