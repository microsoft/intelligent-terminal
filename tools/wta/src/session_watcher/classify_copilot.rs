//! Copilot `events.jsonl` classifier.
//!
//! Record shapes (verified 2026-06-09 against a live `events.jsonl`):
//!   * `{"type":"assistant.turn_start","data":{"turnId":"0",...}}`
//!   * `{"type":"assistant.turn_end","data":{"turnId":"0"}}`
//!   * `{"type":"tool.execution_start","data":{"toolName":"skill",...}}`
//!   * `{"type":"tool.execution_complete","data":{"success":true,...}}`
//!
//! Activity model — **turn-based**, not tool-based. One user prompt drives one
//! or more assistant *turns*; the agent is busy for the whole turn (thinking,
//! streaming text, AND running tools), so WORKING is bracketed by
//! `assistant.turn_start` → `assistant.turn_end`, not by the brief
//! `tool.execution_*` windows. Tool starts only refine the picture (the
//! current tool, or ATTENTION for a user-input tool); `tool.execution_complete`
//! is ignored because IDLE is owned by `turn_end`. A `permission.requested`
//! record (the agent waiting for the user to approve/deny a command) also maps
//! to ATTENTION.
//!
//! The session key is the session-state directory name (supplied by the
//! watcher from the file path), never the record body.

use crate::agent_sessions::{is_user_input_tool, AgentKey, SessionEvent};

/// Map one parsed Copilot `events.jsonl` record to zero or more events.
pub fn classify(record: &serde_json::Value, key: &AgentKey) -> Vec<SessionEvent> {
    let kind = record.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match kind {
        // A turn spans the agent's whole working period for one prompt
        // (thinking + streaming text + tool runs). turn_start → WORKING for
        // the entire turn; turn_end → IDLE. This is what keeps the row WORKING
        // during text generation, not just the brief tool windows. WORKING is
        // surfaced by reusing `ToolStarting` with an empty tool name —
        // `current_tool` is not rendered in the session view, so the empty
        // name is invisible and only the WORKING status takes effect.
        "assistant.turn_start" => vec![SessionEvent::ToolStarting {
            key: key.clone(),
            tool_name: String::new(),
        }],
        "assistant.turn_end" => vec![SessionEvent::ToolCompleted { key: key.clone() }],
        // The agent paused to ask the user to approve/deny a command (the
        // permission y/n gate) → ATTENTION. The pending command only runs after
        // approval, at which point the resulting `tool.execution_start`
        // (→ WORKING) or the turn's `turn_end` (→ IDLE) clears the attention.
        "permission.requested" => {
            let reason = record
                .get("data")
                .and_then(|d| d.get("permissionRequest"))
                .and_then(|p| p.get("intention"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("permission requested")
                .to_string();
            vec![SessionEvent::Notification {
                key: key.clone(),
                message: reason,
            }]
        }
        "tool.execution_start" => {
            let tool = record
                .get("data")
                .and_then(|d| d.get("toolName"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // A user-input tool (ask_user, ...) means the agent is blocked on
            // the user → Attention; everything else keeps the turn WORKING and
            // just refreshes the current tool.
            if is_user_input_tool(&tool) {
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
        // tool.execution_complete is intentionally NOT mapped: returning to
        // IDLE is driven by `assistant.turn_end`, so a tool finishing mid-turn
        // must not flip the row to IDLE while the agent keeps working.
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_sessions::SessionEvent;

    fn rec(s: &str) -> serde_json::Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn tool_start_maps_to_tool_starting() {
        let r = rec(r#"{"type":"tool.execution_start","data":{"toolName":"skill"}}"#);
        let out = classify(&r, &"sess-1".to_string());
        assert_eq!(
            out,
            vec![SessionEvent::ToolStarting {
                key: "sess-1".to_string(),
                tool_name: "skill".to_string()
            }]
        );
    }

    #[test]
    fn ask_user_tool_maps_to_notification() {
        let r = rec(r#"{"type":"tool.execution_start","data":{"toolName":"ask_user"}}"#);
        let out = classify(&r, &"sess-1".to_string());
        assert!(matches!(
            out.as_slice(),
            [SessionEvent::Notification { .. }]
        ));
    }

    #[test]
    fn permission_requested_maps_to_attention() {
        // Copilot writes a `permission.requested` record while it waits for the
        // user to approve/deny a command → ATTENTION. The human-readable
        // `intention` becomes the (non-rendered) attention reason.
        let r = rec(
            r#"{"type":"permission.requested","data":{"permissionRequest":{"kind":"shell","intention":"run git log"}}}"#,
        );
        let out = classify(&r, &"sess-1".to_string());
        assert_eq!(
            out,
            vec![SessionEvent::Notification {
                key: "sess-1".to_string(),
                message: "run git log".to_string()
            }]
        );
    }

    #[test]
    fn permission_requested_without_intention_uses_fallback() {
        let r = rec(r#"{"type":"permission.requested","data":{"permissionRequest":{"kind":"shell"}}}"#);
        let out = classify(&r, &"sess-1".to_string());
        assert!(matches!(
            out.as_slice(),
            [SessionEvent::Notification { message, .. }] if message == "permission requested"
        ));
    }

    #[test]
    fn turn_start_maps_to_working() {
        // A turn beginning means the agent is busy for the whole turn —
        // thinking, streaming text, and running tools — not just during the
        // brief tool windows. We surface that as WORKING by reusing
        // ToolStarting with an empty tool name (current_tool isn't rendered,
        // so the empty name is invisible; only the WORKING status matters).
        let r = rec(r#"{"type":"assistant.turn_start","data":{"turnId":"0"}}"#);
        let out = classify(&r, &"sess-1".to_string());
        assert_eq!(
            out,
            vec![SessionEvent::ToolStarting {
                key: "sess-1".to_string(),
                tool_name: String::new()
            }]
        );
    }

    #[test]
    fn turn_end_maps_to_tool_completed() {
        // turn_end is the authoritative "agent is done" signal → IDLE.
        let r = rec(r#"{"type":"assistant.turn_end","data":{"turnId":"0"}}"#);
        let out = classify(&r, &"sess-1".to_string());
        assert_eq!(
            out,
            vec![SessionEvent::ToolCompleted {
                key: "sess-1".to_string()
            }]
        );
    }

    #[test]
    fn tool_complete_is_dropped_in_turn_model() {
        // Returning to IDLE is driven by turn_end, NOT by individual tool
        // completion — a tool finishing mid-turn must not flip the row to
        // IDLE while the agent keeps working. So tool.execution_complete is
        // intentionally ignored.
        let r = rec(r#"{"type":"tool.execution_complete","data":{"success":true}}"#);
        assert!(classify(&r, &"sess-1".to_string()).is_empty());
    }

    #[test]
    fn unrelated_record_yields_nothing() {
        let r = rec(r#"{"type":"assistant.message","data":{"content":"hi"}}"#);
        assert!(classify(&r, &"sess-1".to_string()).is_empty());
    }
}
