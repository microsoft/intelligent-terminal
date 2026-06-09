//! Copilot `events.jsonl` classifier.
//!
//! Record shapes (verified 2026-06-08 against a live `events.jsonl`):
//!   * `{"type":"tool.execution_start","data":{"toolName":"skill",...}}`
//!   * `{"type":"tool.execution_complete","data":{"success":true,...}}`
//!   * `{"type":"session.start",...}` / `{"type":"assistant.turn_end",...}`
//!
//! The session key is the session-state directory name (supplied by the
//! watcher from the file path), never the record body.

use crate::agent_sessions::{is_user_input_tool, AgentKey, SessionEvent};

/// Map one parsed Copilot `events.jsonl` record to zero or more events.
pub fn classify(record: &serde_json::Value, key: &AgentKey) -> Vec<SessionEvent> {
    let kind = record.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match kind {
        "tool.execution_start" => {
            let tool = record
                .get("data")
                .and_then(|d| d.get("toolName"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // A user-input tool (ask_user, ...) means the agent is blocked on
            // the user → Attention; everything else is autonomous → Working.
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
        "tool.execution_complete" => vec![SessionEvent::ToolCompleted { key: key.clone() }],
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
        assert!(matches!(out.as_slice(), [SessionEvent::Notification { .. }]));
    }

    #[test]
    fn tool_complete_maps_to_tool_completed() {
        let r = rec(r#"{"type":"tool.execution_complete","data":{"success":true}}"#);
        let out = classify(&r, &"sess-1".to_string());
        assert_eq!(out, vec![SessionEvent::ToolCompleted { key: "sess-1".to_string() }]);
    }

    #[test]
    fn unrelated_record_yields_nothing() {
        let r = rec(r#"{"type":"assistant.turn_end"}"#);
        assert!(classify(&r, &"sess-1".to_string()).is_empty());
    }
}
