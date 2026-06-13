//! Claude `<id>.jsonl` classifier.
//!
//! Record shapes (verified 2026-06-08):
//!   * assistant tool call:
//!     `{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash"}]}}`
//!   * tool result:
//!     `{"type":"user","message":{"content":[{"type":"tool_result","is_error":false}]}}`
//!
//! Meta records (`permission-mode`, `file-history-snapshot`, `system`, ...)
//! yield nothing.

use crate::agent_sessions::{is_user_input_tool, AgentKey, SessionEvent};

pub fn classify(record: &serde_json::Value, key: &AgentKey) -> Vec<SessionEvent> {
    let kind = record.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let content = record
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array());

    match (kind, content) {
        ("assistant", Some(items)) => {
            let mut out = Vec::new();
            for item in items {
                if item.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                    let tool = item
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if is_user_input_tool(&tool) {
                        out.push(SessionEvent::Notification {
                            key: key.clone(),
                            message: tool,
                        });
                    } else {
                        out.push(SessionEvent::ToolStarting {
                            key: key.clone(),
                            tool_name: tool,
                        });
                    }
                }
            }
            out
        }
        ("user", Some(items)) => {
            let has_result = items
                .iter()
                .any(|i| i.get("type").and_then(|v| v.as_str()) == Some("tool_result"));
            if has_result {
                vec![SessionEvent::ToolCompleted { key: key.clone() }]
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(s: &str) -> serde_json::Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn tool_use_maps_to_tool_starting() {
        let r = rec(
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash"}]}}"#,
        );
        let out = classify(&r, &"k".to_string());
        assert_eq!(
            out,
            vec![SessionEvent::ToolStarting {
                key: "k".to_string(),
                tool_name: "Bash".to_string()
            }]
        );
    }

    #[test]
    fn tool_result_maps_to_tool_completed() {
        let r = rec(
            r#"{"type":"user","message":{"content":[{"type":"tool_result","is_error":false}]}}"#,
        );
        let out = classify(&r, &"k".to_string());
        assert_eq!(
            out,
            vec![SessionEvent::ToolCompleted {
                key: "k".to_string()
            }]
        );
    }

    #[test]
    fn text_only_assistant_yields_nothing() {
        let r = rec(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#);
        assert!(classify(&r, &"k".to_string()).is_empty());
    }

    #[test]
    fn meta_record_yields_nothing() {
        let r = rec(r#"{"type":"file-history-snapshot","messageId":"x"}"#);
        assert!(classify(&r, &"k".to_string()).is_empty());
    }
}
