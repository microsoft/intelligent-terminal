//! Gemini chat-transcript classifier.
//!
//! File format (re-verified 2026-06-14 against a live `session-*.jsonl`): the
//! file **is an append log** — every line is a record appended at the end, never
//! an in-place rewrite. Lines are a mix of:
//!   * per-message records `{"id","type":"gemini"|"user","content",…}` — the
//!     `type` is top-level, *not* inside a `messages` array;
//!   * a `{"$set":{"lastUpdated":…}}` bump after almost every message;
//!   * a `{"$set":{"messages":[…full history…]}}` FULL snapshot only at session
//!     **start** and **resume** — NOT once per turn.
//! A `gemini` assistant message is appended **twice** under the same id: phase 1
//! carries text/thoughts only, phase 2 re-appends the same id WITH `toolCalls`.
//!
//! We therefore read the file by **byte offset** (like Copilot/Codex/Claude) and
//! classify each appended single-message record in [`classify_record`], skipping
//! every `$set` op — crucially the resume `$set.messages` snapshot, which would
//! otherwise replay the entire prior conversation. See [`classify_record`] for
//! the status model (Working-only; Idle deferred — no turn-completion signal)
//! and the post-hoc Attention caveat.

use crate::agent_sessions::{is_user_input_tool, AgentKey, SessionEvent};

/// Classify ONE appended record from Gemini's `session-*.jsonl`. The watcher
/// reads the file by byte offset (append model) and hands each new line here.
///
/// We process only single-message records (`{"id","type":"user"|"gemini",…}`)
/// and **skip every `$set` op** — including the full `{"$set":{"messages":[…]}}`
/// history snapshot Gemini writes at session start and resume, which would
/// otherwise replay the entire prior conversation (re-emitting Working for old
/// activity) the first time we read a resumed file.
///
/// ## Status model — Working-only (turn-based Idle deferred)
/// Every record that represents activity maps to **Working**:
///   * `type:user` (a typed prompt *or* a `functionResponse` tool result),
///   * `type:gemini` text (phase 1 or a final answer),
///   * `type:gemini` with `toolCalls` (tool name surfaced for display); a
///     user-input tool (`ask_user`) maps to **Attention** instead.
///
/// We deliberately **never emit `ToolCompleted`/Idle**. Gemini's transcript has
/// no turn-completion signal, and a `toolCall` carrying a `result` does NOT mean
/// the turn ended — the agent keeps going (more tools / a final answer follow).
/// So a Gemini row stays Working through the conversation and only leaves the
/// live state on `PaneClosed`. Telling "still working" from "final answer" needs
/// a turn-end marker Gemini doesn't write, so that (and thus Idle) is deferred.
///
/// ## Attention caveat (post-hoc transcript)
/// Gemini writes tool records **after** the tool completes — every on-disk
/// `toolCall` is `status:"success"` with its `result` inlined, and an `ask_user`
/// record already contains the user's answer. So the `ask_user` line appears
/// only *after* the user replied; during the actual wait the file is silent and
/// the last record (the agent's phase-1 text) shows **Working**. The `ask_user`
/// → Attention mapping is kept (and is correct if a future Gemini build writes a
/// pending state), but in today's transcript it is typically superseded by the
/// immediately-following result record. Reliable wait-state Attention needs
/// hooks — the same limitation as Claude's permission prompts.
///
/// Message shapes (verified 2026-06-14 against a live transcript):
///   * typed prompt:  `{"type":"user","content":[{"text":"…"}]}`
///   * tool result:   `{"type":"user","content":[{"functionResponse":{…}}]}`
///   * assistant text:`{"type":"gemini","content":"…","thoughts":…}`  (no tools)
///   * assistant tool:`{"type":"gemini","toolCalls":[{"name":"ask_user","status":"success",…}]}`
pub fn classify_record(record: &serde_json::Value, key: &AgentKey) -> Vec<SessionEvent> {
    // Skip all `$set` ops (lastUpdated / messages snapshot / summary / …).
    if record.get("$set").is_some() {
        return Vec::new();
    }
    let mut out = Vec::new();
    match record.get("type").and_then(|v| v.as_str()).unwrap_or("") {
        "gemini" => {
            let mut input_tool: Option<String> = None;
            if let Some(calls) = record.get("toolCalls").and_then(|c| c.as_array()) {
                for call in calls {
                    let tool = call
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if is_user_input_tool(&tool) {
                        // Remember it; emit Attention last so it wins over any
                        // sibling tool in the same record.
                        input_tool = Some(tool);
                    } else {
                        out.push(SessionEvent::ToolStarting {
                            key: key.clone(),
                            tool_name: tool,
                        });
                    }
                }
            }
            if let Some(tool) = input_tool {
                out.push(SessionEvent::Notification {
                    key: key.clone(),
                    message: tool,
                });
            } else if out.is_empty() {
                // Text-only (phase 1 / final answer) or an empty toolCalls array
                // — the agent is still producing output → Working.
                out.push(SessionEvent::ToolStarting {
                    key: key.clone(),
                    tool_name: String::new(),
                });
            }
        }
        "user" => {
            // A typed prompt or a `functionResponse` tool result — both mean the
            // agent is actively in a turn → Working. (We can't, and needn't,
            // tell them apart for status.)
            out.push(SessionEvent::ToolStarting {
                key: key.clone(),
                tool_name: String::new(),
            });
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(s: &str) -> serde_json::Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn set_op_is_skipped() {
        // `$set` lines (lastUpdated / messages snapshot / summary) emit nothing —
        // this is what prevents the resume snapshot from replaying history.
        let snap = rec(r#"{"$set":{"messages":[
            {"type":"user","content":[{"text":"hi"}]},
            {"type":"gemini","toolCalls":[{"name":"read_file"}]}
        ]}}"#);
        assert!(classify_record(&snap, &"k".to_string()).is_empty());
        let bump = rec(r#"{"$set":{"lastUpdated":"2026-06-14T00:00:00Z"}}"#);
        assert!(classify_record(&bump, &"k".to_string()).is_empty());
    }

    #[test]
    fn user_prompt_is_working() {
        let r = rec(r#"{"type":"user","content":[{"text":"explore the repo"}]}"#);
        assert_eq!(
            classify_record(&r, &"k".to_string()),
            vec![SessionEvent::ToolStarting {
                key: "k".to_string(),
                tool_name: String::new()
            }]
        );
    }

    #[test]
    fn user_function_response_is_working_not_completed() {
        // A tool result means the agent is continuing the turn → Working. We do
        // NOT emit ToolCompleted (Idle is deferred for Gemini).
        let r = rec(r#"{"type":"user","content":[{"functionResponse":{"name":"read_file"}}]}"#);
        assert_eq!(
            classify_record(&r, &"k".to_string()),
            vec![SessionEvent::ToolStarting {
                key: "k".to_string(),
                tool_name: String::new()
            }]
        );
    }

    #[test]
    fn gemini_text_only_is_working() {
        let r = rec(r#"{"type":"gemini","content":"here is my analysis","thoughts":"…"}"#);
        assert_eq!(
            classify_record(&r, &"k".to_string()),
            vec![SessionEvent::ToolStarting {
                key: "k".to_string(),
                tool_name: String::new()
            }]
        );
    }

    #[test]
    fn gemini_tool_call_is_working_with_name_no_completion() {
        // The on-disk toolCall is already `status:success` with a result, but we
        // must NOT emit ToolCompleted — the turn isn't over.
        let r = rec(r#"{"type":"gemini","toolCalls":[
            {"name":"read_file","status":"success","result":[{"functionResponse":{}}]}
        ]}"#);
        assert_eq!(
            classify_record(&r, &"k".to_string()),
            vec![SessionEvent::ToolStarting {
                key: "k".to_string(),
                tool_name: "read_file".to_string()
            }]
        );
    }

    #[test]
    fn gemini_ask_user_is_attention() {
        let r = rec(r#"{"type":"gemini","toolCalls":[
            {"name":"ask_user","status":"success","result":[{"functionResponse":{}}]}
        ]}"#);
        assert_eq!(
            classify_record(&r, &"k".to_string()),
            vec![SessionEvent::Notification {
                key: "k".to_string(),
                message: "ask_user".to_string()
            }]
        );
    }

    #[test]
    fn gemini_mixed_tools_attention_wins() {
        // A record carrying both a normal tool and ask_user → Attention is
        // emitted last so it wins in the reducer.
        let r = rec(r#"{"type":"gemini","toolCalls":[
            {"name":"glob","status":"success"},
            {"name":"ask_user","status":"success"}
        ]}"#);
        assert_eq!(
            classify_record(&r, &"k".to_string()),
            vec![
                SessionEvent::ToolStarting {
                    key: "k".to_string(),
                    tool_name: "glob".to_string()
                },
                SessionEvent::Notification {
                    key: "k".to_string(),
                    message: "ask_user".to_string()
                },
            ]
        );
    }

    #[test]
    fn header_and_unknown_types_emit_nothing() {
        let header = rec(r#"{"sessionId":"e42015ce-7b10-49f1-ad9d-dca02e033cd7","projectHash":"x"}"#);
        assert!(classify_record(&header, &"k".to_string()).is_empty());
    }
}
