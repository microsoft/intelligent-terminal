//! Claude `<id>.jsonl` classifier — turn-based status, keyed on `stop_reason`.
//!
//! Claude appends to the transcript AND **re-writes the same assistant message
//! id several times as it streams** (verified 2026-06-13: one `msg_…` appears as
//! text-only, then with a `tool_use`, etc.). Classifying by *content presence*
//! would flicker Idle↔Working across those partial frames, so we key on the
//! assistant message's `stop_reason`, which is stable across the stream:
//!
//!   * `{"type":"user","message":{"role":"user", …}}` — a typed prompt
//!     (`content` is a string) OR a `tool_result` handed back to the agent →
//!     the agent has input to process → **Working**.
//!   * `{"type":"assistant","message":{"role":"assistant","stop_reason":…}}`:
//!     - `stop_reason == "tool_use"`:
//!         * a `tool_use` named with a user-input tool (`AskUserQuestion`, …)
//!           → **Attention** (the agent is waiting on the user);
//!         * otherwise → **Working** (running / streaming toward a tool).
//!     - any other `stop_reason` (`end_turn`, …) → the turn is complete → **Idle**.
//!   * everything else (`system`, `file-history-snapshot`, meta) → nothing.
//!
//! Status rides the crate's existing events: `ToolStarting` is the "→Working"
//! signal (tool name empty for a user turn), `ToolCompleted` the "→Idle" signal,
//! `Notification` the "→Attention" signal.
//!
//! NOTE: a tool that *prompts for permission* (e.g. `Bash` in `default` mode)
//! is indistinguishable from a tool that is merely *running* — the transcript
//! has no approval/pending marker — so a permission wait shows as Working, not
//! Attention. Only an explicit user-input tool (AskUserQuestion) is Attention.

use crate::agent_sessions::{is_user_input_tool, AgentKey, SessionEvent};

pub fn classify(record: &serde_json::Value, key: &AgentKey) -> Vec<SessionEvent> {
    match record.get("type").and_then(|v| v.as_str()).unwrap_or("") {
        // User turn (typed prompt or tool_result): the agent has work to do.
        "user" => vec![SessionEvent::ToolStarting {
            key: key.clone(),
            tool_name: String::new(),
        }],
        // Assistant turn: classify by `stop_reason` (stable across streamed
        // re-writes of the same message id).
        "assistant" => {
            let msg = record.get("message");
            let stop = msg
                .and_then(|m| m.get("stop_reason"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if stop != "tool_use" {
                // end_turn (or any non-tool stop) → the turn is complete.
                return vec![SessionEvent::ToolCompleted { key: key.clone() }];
            }
            // The assistant stopped to call a tool (possibly still streaming
            // toward it, so the tool_use content may not be present yet).
            let tool_names: Vec<&str> = msg
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter(|i| i.get("type").and_then(|v| v.as_str()) == Some("tool_use"))
                        .filter_map(|i| i.get("name").and_then(|v| v.as_str()))
                        .collect()
                })
                .unwrap_or_default();
            if let Some(q) = tool_names.iter().find(|t| is_user_input_tool(t)) {
                // The agent is asking the user (e.g. AskUserQuestion).
                vec![SessionEvent::Notification {
                    key: key.clone(),
                    message: (*q).to_string(),
                }]
            } else {
                // Running (or streaming toward) a tool — still Working.
                vec![SessionEvent::ToolStarting {
                    key: key.clone(),
                    tool_name: tool_names.first().map(|s| (*s).to_string()).unwrap_or_default(),
                }]
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
    fn user_prompt_maps_to_working() {
        let r = rec(r#"{"type":"user","message":{"role":"user","content":"installed hooks"}}"#);
        assert_eq!(
            classify(&r, &"k".to_string()),
            vec![SessionEvent::ToolStarting {
                key: "k".to_string(),
                tool_name: String::new()
            }]
        );
    }

    #[test]
    fn user_tool_result_also_maps_to_working() {
        // A tool_result is a user-role record too — the agent resumes → Working.
        let r = rec(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"ok"}]}}"#,
        );
        assert_eq!(
            classify(&r, &"k".to_string()),
            vec![SessionEvent::ToolStarting {
                key: "k".to_string(),
                tool_name: String::new()
            }]
        );
    }

    #[test]
    fn assistant_text_only_maps_to_idle() {
        let r = rec(
            r#"{"type":"assistant","message":{"role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"done"}]}}"#,
        );
        assert_eq!(
            classify(&r, &"k".to_string()),
            vec![SessionEvent::ToolCompleted {
                key: "k".to_string()
            }]
        );
    }

    #[test]
    fn assistant_normal_tool_use_maps_to_working() {
        let r = rec(
            r#"{"type":"assistant","message":{"role":"assistant","stop_reason":"tool_use","content":[{"type":"tool_use","name":"Bash"}]}}"#,
        );
        assert_eq!(
            classify(&r, &"k".to_string()),
            vec![SessionEvent::ToolStarting {
                key: "k".to_string(),
                tool_name: "Bash".to_string()
            }]
        );
    }

    #[test]
    fn assistant_streaming_partial_tool_use_is_working_not_idle() {
        // A streamed frame of an assistant message can carry only text so far but
        // already have stop_reason=tool_use (the tool_use content lands in a later
        // re-write of the same id). Keying on stop_reason avoids an Idle flicker.
        let r = rec(
            r#"{"type":"assistant","message":{"role":"assistant","stop_reason":"tool_use","content":[{"type":"text","text":"Let me check"}]}}"#,
        );
        assert_eq!(
            classify(&r, &"k".to_string()),
            vec![SessionEvent::ToolStarting {
                key: "k".to_string(),
                tool_name: String::new()
            }]
        );
    }

    #[test]
    fn assistant_ask_user_question_maps_to_attention() {
        // Claude's AskUserQuestion tool_use → Attention, not Working — even when
        // the same message also carries explanatory text.
        let r = rec(
            r#"{"type":"assistant","message":{"role":"assistant","stop_reason":"tool_use","content":[
                {"type":"text","text":"Let me check"},
                {"type":"tool_use","id":"tool_use_1","caller":{"type":"direct"},"name":"AskUserQuestion","input":{}}
            ]}}"#,
        );
        assert_eq!(
            classify(&r, &"k".to_string()),
            vec![SessionEvent::Notification {
                key: "k".to_string(),
                message: "AskUserQuestion".to_string()
            }]
        );
    }

    #[test]
    fn meta_record_yields_nothing() {
        let r = rec(r#"{"type":"file-history-snapshot","messageId":"x"}"#);
        assert!(classify(&r, &"k".to_string()).is_empty());
    }
}
