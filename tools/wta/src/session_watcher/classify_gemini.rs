//! Gemini chat-snapshot classifier.
//!
//! Gemini's `session-*.jsonl` is NOT an append log: each turn rewrites a
//! trailing `{"$set":{"messages":[…]}}` snapshot in place. We therefore parse
//! the latest snapshot and diff the `messages` array by length, classifying
//! only the messages appended since `prev_len`.
//!
//! Message shapes (verified 2026-06-08):
//!   * assistant w/ tools: `{"type":"gemini","toolCalls":[{"name":"update_topic",...}]}`
//!   * tool result:        `{"type":"user","content":[{"functionResponse":{...}}]}`

use crate::agent_sessions::{is_user_input_tool, AgentKey, SessionEvent};

/// Extract the messages array from the latest snapshot line. The watcher
/// passes the *last non-empty line* of the file (the freshest `$set`).
fn messages_of(snapshot_line: &serde_json::Value) -> Option<&Vec<serde_json::Value>> {
    snapshot_line
        .get("$set")
        .and_then(|s| s.get("messages"))
        .and_then(|m| m.as_array())
        .or_else(|| snapshot_line.get("messages").and_then(|m| m.as_array()))
}

/// Classify messages appended since `prev_len`. Returns the new events and the
/// updated message count to remember for next time.
pub fn classify_snapshot(
    snapshot_line: &serde_json::Value,
    key: &AgentKey,
    prev_len: usize,
) -> (Vec<SessionEvent>, usize) {
    let messages = match messages_of(snapshot_line) {
        Some(m) => m,
        None => return (Vec::new(), prev_len),
    };
    let mut out = Vec::new();
    for msg in messages.iter().skip(prev_len) {
        let ty = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if ty == "gemini" {
            if let Some(calls) = msg.get("toolCalls").and_then(|c| c.as_array()) {
                for call in calls {
                    let tool = call
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
                    // Gemini embeds the result inline once the tool returns;
                    // a call carrying `result` is already complete.
                    if call.get("result").is_some() {
                        out.push(SessionEvent::ToolCompleted { key: key.clone() });
                    }
                }
            }
        } else if ty == "user" {
            let has_resp = msg
                .get("content")
                .and_then(|c| c.as_array())
                .map(|items| items.iter().any(|i| i.get("functionResponse").is_some()))
                .unwrap_or(false);
            if has_resp {
                out.push(SessionEvent::ToolCompleted { key: key.clone() });
            }
        }
    }
    (out, messages.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(s: &str) -> serde_json::Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn new_tool_call_message_emits_tool_starting() {
        let snap = rec(r#"{"$set":{"messages":[
            {"type":"user","content":[{"text":"hi"}]},
            {"type":"gemini","toolCalls":[{"name":"run_shell_command"}]}
        ]}}"#);
        let (out, new_len) = classify_snapshot(&snap, &"k".to_string(), 1);
        assert_eq!(new_len, 2);
        assert_eq!(
            out,
            vec![SessionEvent::ToolStarting {
                key: "k".to_string(),
                tool_name: "run_shell_command".to_string()
            }]
        );
    }

    #[test]
    fn tool_call_with_inline_result_also_completes() {
        let snap = rec(r#"{"$set":{"messages":[
            {"type":"gemini","toolCalls":[{"name":"run_shell_command","result":[{"functionResponse":{}}]}]}
        ]}}"#);
        let (out, _len) = classify_snapshot(&snap, &"k".to_string(), 0);
        assert_eq!(
            out,
            vec![
                SessionEvent::ToolStarting {
                    key: "k".to_string(),
                    tool_name: "run_shell_command".to_string()
                },
                SessionEvent::ToolCompleted {
                    key: "k".to_string()
                },
            ]
        );
    }

    #[test]
    fn already_seen_messages_are_not_reclassified() {
        let snap = rec(r#"{"$set":{"messages":[
            {"type":"gemini","toolCalls":[{"name":"x"}]}
        ]}}"#);
        let (out, _len) = classify_snapshot(&snap, &"k".to_string(), 1);
        assert!(out.is_empty());
    }

    #[test]
    fn user_function_response_completes() {
        let snap = rec(r#"{"$set":{"messages":[
            {"type":"user","content":[{"functionResponse":{"name":"x"}}]}
        ]}}"#);
        let (out, _len) = classify_snapshot(&snap, &"k".to_string(), 0);
        assert_eq!(
            out,
            vec![SessionEvent::ToolCompleted {
                key: "k".to_string()
            }]
        );
    }
}
