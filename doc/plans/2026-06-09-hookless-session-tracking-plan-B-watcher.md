# Hookless Session Tracking — Plan B: Watcher + Classifiers (`session_watcher`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `tools/wta/src/session_watcher/` — a filesystem watcher that turns each agent CLI's on-disk session records into the crate's existing `SessionEvent`s, plus the cwd-correlation binder that resolves a discovered session to its hosting pane (reusing Plan A's `proc_bind`).

**Architecture:** Per-CLI **pure classifier** functions (`record JSON → Vec<SessionEvent>`) are the testable core. A thin `notify`-backed watch loop tails appended bytes (Copilot/Claude/Codex) or re-parses the rewritten `$set.messages` snapshot (Gemini) and feeds lines through the classifiers, emitting `(CliSource, AgentKey, SessionEvent)` to a channel. A binder pairs a discovered session file to a live CLI process and reads its pane GUID via `proc_bind`. **This plan does not touch the registry or master** — it only produces events (Plan C wires them in).

**Tech Stack:** Rust (edition 2021, toolchain `ms-prod-1.93`), `serde_json`, the `notify` crate (new dep), Plan A's `proc_bind`, and reused `history_loader` path helpers.

---

## Prerequisite

Plan A (`proc_bind.rs`) must be merged/available — Task 7 calls `proc_bind::wt_session_for_pid`, `proc_bind::file_owner_pid`, `proc_bind::copilot_pid_from_lock`.

## Reused existing surfaces (verified)

- `agent_sessions::SessionEvent` variants: `SessionStarted { key, cli_source, pane_session_id, cwd, title }`, `ToolStarting { key, tool_name }`, `ToolCompleted { key }`, `Notification { key, message }`, `SessionStopped { key, reason }` (`agent_sessions.rs:256-286`).
- `agent_sessions::AgentKey` = `String` (`agent_sessions.rs:32`); `CliSource` enum + `CliSource::parse` (`:34-53`).
- `agent_sessions::is_user_input_tool(&str) -> bool` (`:298`) — reuse for the Attention escalation.
- `history_loader::decode_claude_cwd(&str) -> PathBuf` (`:1159`), `history_loader::parse_gemini_projects(&Path) -> HashMap<String,PathBuf>` (`:1179`), `history_loader::copilot_session_dir_for_key` (`:366`).

## File Structure

| File | Responsibility | Action |
|---|---|---|
| `tools/wta/Cargo.toml` | add `notify` dep | **Modify** |
| `tools/wta/cgmanifest.json`, `/NOTICE.md` | regenerated dep notices | **Modify** (generated) |
| `tools/wta/src/session_watcher/mod.rs` | module root: `WatchedRoot`, incremental reader, `notify` loop, event channel | **Create** |
| `tools/wta/src/session_watcher/classify_copilot.rs` | `events.jsonl` record → `Vec<SessionEvent>` | **Create** |
| `tools/wta/src/session_watcher/classify_claude.rs` | claude `<id>.jsonl` record → events | **Create** |
| `tools/wta/src/session_watcher/classify_codex.rs` | codex rollout record → events | **Create** |
| `tools/wta/src/session_watcher/classify_gemini.rs` | gemini `$set.messages` snapshot diff → events | **Create** |
| `tools/wta/src/session_watcher/discover.rs` | path → `(CliSource, AgentKey, cwd)` for the four roots | **Create** |
| `tools/wta/src/session_watcher/bind.rs` | discovered session + `proc_bind` → pane GUID | **Create** |
| `tools/wta/src/proc_bind.rs` | add `cwd_for_pid` (validates the PEB CurrentDirectory offset) | **Modify** |
| `tools/wta/src/main.rs` | `mod session_watcher;` + `Command::WatchProbe` | **Modify** |

## Test command (run from repo root)

```bash
cargo test --manifest-path tools/wta/Cargo.toml session_watcher:: -- --nocapture
cargo test --manifest-path tools/wta/Cargo.toml proc_bind::tests::cwd -- --nocapture
```

---

## Task 1: Add the `notify` dependency + regenerate notices

The spec calls for a `notify`/`ReadDirectoryChangesW`-backed watcher (OS push, not O(files) polling — important because users accumulate hundreds of historical session dirs).

**Files:**
- Modify: `tools/wta/Cargo.toml`
- Modify (generated): `tools/wta/cgmanifest.json`, `/NOTICE.md`

- [ ] **Step 1: Add the dependency**

In `tools/wta/Cargo.toml`, under `[dependencies]`, add (alphabetical-ish, after `crossterm`):

```toml
notify = "6"
```

- [ ] **Step 2: Build to resolve the lockfile**

```bash
powershell -NoProfile -Command "Get-Process wta -ErrorAction SilentlyContinue | Stop-Process -Force"
cargo build --manifest-path tools/wta/Cargo.toml
```
Expected: `notify` and its transitive deps resolve and compile under `+crt-static` for `x86_64-pc-windows-msvc`. If a transitive dep fails under static CRT, pin `notify` to the latest 6.x patch and retry; report back rather than switching strategy.

- [ ] **Step 3: Regenerate third-party notices** (required by `tools/wta/AGENTS.md` whenever direct deps change)

```powershell
$env:RUSTUP_TOOLCHAIN = 'stable'
pwsh -File .\build\scripts\Generate-WtaThirdPartyNotices.ps1
```
Expected: `tools/wta/cgmanifest.json` and the `<!-- BEGIN wta-rust-deps -->` block in `/NOTICE.md` gain the `notify` crate family. Requires PowerShell 7+.

- [ ] **Step 4: Commit**

```bash
git add tools/wta/Cargo.toml tools/wta/Cargo.lock tools/wta/cgmanifest.json NOTICE.md
git commit -m "build(wta): add notify dependency for session_watcher + regen notices"
```

---

## Task 2: Module skeleton + `classify_copilot`

**Files:**
- Create: `tools/wta/src/session_watcher/mod.rs`
- Create: `tools/wta/src/session_watcher/classify_copilot.rs`
- Modify: `tools/wta/src/main.rs` (add `mod session_watcher;`)

- [ ] **Step 1: Register the module**

In `tools/wta/src/main.rs` module list, add after `mod session_registry;`:

```rust
mod session_registry;
mod session_watcher;
```

- [ ] **Step 2: Create the module root declaring the submodules**

Create `tools/wta/src/session_watcher/mod.rs`:

```rust
//! session_watcher — turn each agent CLI's on-disk session records into the
//! crate's existing [`crate::agent_sessions::SessionEvent`]s, hook-free.
//!
//! The per-CLI `classify_*` functions are the pure, testable core: they take
//! one parsed record (or, for Gemini, the rewritten snapshot) plus the
//! session key and return zero or more `SessionEvent`s. The watch loop
//! ([`watch`]) is the thin impure shell that tails files and feeds records
//! through them. Binding a discovered session to its pane lives in
//! [`bind`]; path → identity in [`discover`].

pub mod bind;
pub mod classify_claude;
pub mod classify_codex;
pub mod classify_copilot;
pub mod classify_gemini;
pub mod discover;
```

- [ ] **Step 3: Create `classify_copilot.rs` with the failing test**

Create `tools/wta/src/session_watcher/classify_copilot.rs`:

```rust
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
```

- [ ] **Step 4: Run the tests**

Run: `cargo test --manifest-path tools/wta/Cargo.toml session_watcher::classify_copilot -- --nocapture`
Expected: 4 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add tools/wta/src/session_watcher/ tools/wta/src/main.rs
git commit -m "feat(wta/session_watcher): module skeleton + copilot classifier"
```

---

## Task 3: `classify_claude`

**Files:**
- Modify: `tools/wta/src/session_watcher/classify_claude.rs` (create)

- [ ] **Step 1: Create `classify_claude.rs` with the failing test**

Create `tools/wta/src/session_watcher/classify_claude.rs`:

```rust
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
        let r = rec(r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash"}]}}"#);
        let out = classify(&r, &"k".to_string());
        assert_eq!(out, vec![SessionEvent::ToolStarting { key: "k".to_string(), tool_name: "Bash".to_string() }]);
    }

    #[test]
    fn tool_result_maps_to_tool_completed() {
        let r = rec(r#"{"type":"user","message":{"content":[{"type":"tool_result","is_error":false}]}}"#);
        let out = classify(&r, &"k".to_string());
        assert_eq!(out, vec![SessionEvent::ToolCompleted { key: "k".to_string() }]);
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
```

- [ ] **Step 2: Run the tests**

Run: `cargo test --manifest-path tools/wta/Cargo.toml session_watcher::classify_claude -- --nocapture`
Expected: 4 tests PASS.

- [ ] **Step 3: Commit**

```bash
git add tools/wta/src/session_watcher/classify_claude.rs
git commit -m "feat(wta/session_watcher): claude classifier"
```

---

## Task 4: `classify_codex`

**Files:**
- Create: `tools/wta/src/session_watcher/classify_codex.rs`

- [ ] **Step 1: Create `classify_codex.rs` with the failing test**

Create `tools/wta/src/session_watcher/classify_codex.rs`:

```rust
//! Codex rollout classifier.
//!
//! Record shapes (verified 2026-06-08):
//!   * tool start: `{"type":"response_item","payload":{"type":"function_call","name":"shell_command"}}`
//!   * tool end:   `{"type":"response_item","payload":{"type":"function_call_output",...}}`
//!   * turn end:   `{"type":"event_msg","payload":{"type":"task_complete"}}`

use crate::agent_sessions::{is_user_input_tool, AgentKey, SessionEvent};

pub fn classify(record: &serde_json::Value, key: &AgentKey) -> Vec<SessionEvent> {
    let kind = record.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let payload_type = record
        .get("payload")
        .and_then(|p| p.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match (kind, payload_type) {
        ("response_item", "function_call")
        | ("response_item", "local_shell_call")
        | ("response_item", "custom_tool_call") => {
            let tool = record
                .get("payload")
                .and_then(|p| p.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if is_user_input_tool(&tool) {
                vec![SessionEvent::Notification { key: key.clone(), message: tool }]
            } else {
                vec![SessionEvent::ToolStarting { key: key.clone(), tool_name: tool }]
            }
        }
        ("response_item", "function_call_output")
        | ("response_item", "custom_tool_call_output") => {
            vec![SessionEvent::ToolCompleted { key: key.clone() }]
        }
        ("event_msg", "task_complete") => vec![SessionEvent::SessionStopped {
            key: key.clone(),
            reason: "complete".to_string(),
        }],
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
    fn function_call_maps_to_tool_starting() {
        let r = rec(r#"{"type":"response_item","payload":{"type":"function_call","name":"shell_command"}}"#);
        let out = classify(&r, &"k".to_string());
        assert_eq!(out, vec![SessionEvent::ToolStarting { key: "k".to_string(), tool_name: "shell_command".to_string() }]);
    }

    #[test]
    fn function_call_output_maps_to_tool_completed() {
        let r = rec(r#"{"type":"response_item","payload":{"type":"function_call_output"}}"#);
        assert_eq!(classify(&r, &"k".to_string()), vec![SessionEvent::ToolCompleted { key: "k".to_string() }]);
    }

    #[test]
    fn task_complete_maps_to_session_stopped() {
        let r = rec(r#"{"type":"event_msg","payload":{"type":"task_complete"}}"#);
        assert_eq!(classify(&r, &"k".to_string()), vec![SessionEvent::SessionStopped { key: "k".to_string(), reason: "complete".to_string() }]);
    }

    #[test]
    fn plain_message_yields_nothing() {
        let r = rec(r#"{"type":"response_item","payload":{"type":"message","role":"user"}}"#);
        assert!(classify(&r, &"k".to_string()).is_empty());
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test --manifest-path tools/wta/Cargo.toml session_watcher::classify_codex -- --nocapture`
Expected: 4 tests PASS.

- [ ] **Step 3: Commit**

```bash
git add tools/wta/src/session_watcher/classify_codex.rs
git commit -m "feat(wta/session_watcher): codex classifier"
```

---

## Task 5: `classify_gemini` (snapshot diff)

Gemini rewrites a trailing `{"$set":{"messages":[…]}}` snapshot, so its classifier diffs the messages array by length instead of tailing appended lines.

**Files:**
- Create: `tools/wta/src/session_watcher/classify_gemini.rs`

- [ ] **Step 1: Create `classify_gemini.rs` with the failing test**

Create `tools/wta/src/session_watcher/classify_gemini.rs`:

```rust
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
                        out.push(SessionEvent::Notification { key: key.clone(), message: tool });
                    } else {
                        out.push(SessionEvent::ToolStarting { key: key.clone(), tool_name: tool });
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
        assert_eq!(out, vec![SessionEvent::ToolStarting { key: "k".to_string(), tool_name: "run_shell_command".to_string() }]);
    }

    #[test]
    fn tool_call_with_inline_result_also_completes() {
        let snap = rec(r#"{"$set":{"messages":[
            {"type":"gemini","toolCalls":[{"name":"run_shell_command","result":[{"functionResponse":{}}]}]}
        ]}}"#);
        let (out, _len) = classify_snapshot(&snap, &"k".to_string(), 0);
        assert_eq!(out, vec![
            SessionEvent::ToolStarting { key: "k".to_string(), tool_name: "run_shell_command".to_string() },
            SessionEvent::ToolCompleted { key: "k".to_string() },
        ]);
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
        assert_eq!(out, vec![SessionEvent::ToolCompleted { key: "k".to_string() }]);
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test --manifest-path tools/wta/Cargo.toml session_watcher::classify_gemini -- --nocapture`
Expected: 4 tests PASS.

- [ ] **Step 3: Commit**

```bash
git add tools/wta/src/session_watcher/classify_gemini.rs
git commit -m "feat(wta/session_watcher): gemini snapshot-diff classifier"
```

---

## Task 6: `discover` — path → `(CliSource, AgentKey, cwd)`

The watcher needs to turn a changed file path into (which CLI, session key, working dir) for the `SessionStarted` event and for cwd-correlation binding.

**Files:**
- Create: `tools/wta/src/session_watcher/discover.rs`

- [ ] **Step 1: Create `discover.rs` with the failing test**

Create `tools/wta/src/session_watcher/discover.rs`:

```rust
//! Map a changed session-file path under one of the four watched roots to the
//! CLI, session key, and (where the path encodes it) the session's cwd.
//!
//! Path → identity, verified against real layouts:
//!   Copilot : ~/.copilot/session-state/<UUID>/events.jsonl        key=<UUID>
//!   Claude  : ~/.claude/projects/<encoded-cwd>/<UUID>.jsonl       key=<UUID>, cwd=decode(<encoded-cwd>)
//!   Codex   : ~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<UUID>.jsonl  key=<UUID>
//!   Gemini  : ~/.gemini/tmp/<slug>/chats/session-*.jsonl          key=<file-stem>

use crate::agent_sessions::CliSource;
use crate::history_loader::decode_claude_cwd;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    pub cli: CliSource,
    pub key: String,
    /// Path-encoded cwd when available (Claude only, today).
    pub cwd: Option<PathBuf>,
}

/// Classify a changed path. Returns `None` for paths that are not a
/// recognized session file (e.g. a sibling `workspace.yaml`).
pub fn identify(path: &Path) -> Option<Discovered> {
    let name = path.file_name()?.to_str()?;

    // Copilot: .../session-state/<UUID>/events.jsonl
    if name == "events.jsonl" {
        let key = path.parent()?.file_name()?.to_str()?.to_string();
        if path.components().any(|c| c.as_os_str() == "session-state") {
            return Some(Discovered { cli: CliSource::Copilot, key, cwd: None });
        }
    }

    // Codex: rollout-<ts>-<UUID>.jsonl
    if name.starts_with("rollout-") && name.ends_with(".jsonl") {
        // key is the trailing UUID after the last '-'.
        let stem = name.trim_end_matches(".jsonl");
        let key = stem.rsplit('-').next()?.to_string();
        return Some(Discovered { cli: CliSource::Codex, key, cwd: None });
    }

    // Gemini: .../tmp/<slug>/chats/session-*.jsonl
    if name.starts_with("session-") && name.ends_with(".jsonl")
        && path.components().any(|c| c.as_os_str() == "chats")
    {
        let key = name.trim_end_matches(".jsonl").to_string();
        return Some(Discovered { cli: CliSource::Gemini, key, cwd: None });
    }

    // Claude: .../projects/<encoded-cwd>/<UUID>.jsonl
    if name.ends_with(".jsonl") && path.components().any(|c| c.as_os_str() == "projects") {
        let key = name.trim_end_matches(".jsonl").to_string();
        let cwd = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|d| d.to_str())
            .map(decode_claude_cwd);
        return Some(Discovered { cli: CliSource::Claude, key, cwd });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copilot_path() {
        let p = Path::new(r"C:\Users\u\.copilot\session-state\abc-123\events.jsonl");
        let d = identify(p).unwrap();
        assert_eq!(d.cli, CliSource::Copilot);
        assert_eq!(d.key, "abc-123");
    }

    #[test]
    fn codex_path() {
        let p = Path::new(r"C:\Users\u\.codex\sessions\2026\06\08\rollout-2026-06-08T21-29-13-019ea76c-4c47.jsonl");
        let d = identify(p).unwrap();
        assert_eq!(d.cli, CliSource::Codex);
        assert_eq!(d.key, "4c47");
    }

    #[test]
    fn gemini_path() {
        let p = Path::new(r"C:\Users\u\.gemini\tmp\slug\chats\session-2026-06-08T14-01-d6ce.jsonl");
        let d = identify(p).unwrap();
        assert_eq!(d.cli, CliSource::Gemini);
        assert_eq!(d.key, "session-2026-06-08T14-01-d6ce");
    }

    #[test]
    fn claude_path_decodes_cwd() {
        let p = Path::new(r"C:\Users\u\.claude\projects\C--Users-u\aaaa-bbbb.jsonl");
        let d = identify(p).unwrap();
        assert_eq!(d.cli, CliSource::Claude);
        assert_eq!(d.key, "aaaa-bbbb");
        assert!(d.cwd.is_some());
    }

    #[test]
    fn unrelated_path_is_none() {
        assert!(identify(Path::new(r"C:\Users\u\.copilot\session-state\abc\workspace.yaml")).is_none());
    }
}
```

> Note: the codex key here is the last `-`-delimited token of the stem (`4c47` in the test fixture). The real watcher uses the full trailing UUID; the production `discover` should split on the rollout timestamp boundary. For the MVP, callers compare the key against `history_loader`'s codex rollout finder, so a stable suffix is sufficient. If a later task needs the exact UUID, reuse `history_loader::find_codex_rollout_by_id`'s inverse.

- [ ] **Step 2: Run the tests**

Run: `cargo test --manifest-path tools/wta/Cargo.toml session_watcher::discover -- --nocapture`
Expected: 5 tests PASS.

- [ ] **Step 3: Commit**

```bash
git add tools/wta/src/session_watcher/discover.rs
git commit -m "feat(wta/session_watcher): path->identity discovery"
```

---

## Task 7: `cwd_for_pid` (validate PEB offset) + cwd-correlation binder

**Files:**
- Modify: `tools/wta/src/proc_bind.rs` (add `cwd_for_pid`)
- Create: `tools/wta/src/session_watcher/bind.rs`

- [ ] **Step 1: Add a failing test for `cwd_for_pid` in `proc_bind.rs`**

Add to `proc_bind.rs`'s `tests` module:

```rust
    #[test]
    fn cwd_for_pid_reads_child_working_dir() {
        let dir = tmp_dir("cwd-child");
        // Canonicalize so the comparison is robust to short/long path forms.
        let canonical = std::fs::canonicalize(&dir).unwrap();
        let mut child = spawn_probe_child(&[], Some(&canonical));
        let pid = child.id();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let got = cwd_for_pid(pid);
        let _ = child.kill();
        let _ = child.wait();
        let got = got.expect("cwd_for_pid returned None");
        // Compare case-insensitively on the final component to avoid
        // \\?\ prefix / drive-letter-case differences.
        assert!(
            got.to_string_lossy().to_lowercase().contains(
                &dir.file_name().unwrap().to_string_lossy().to_lowercase()
            ),
            "expected cwd containing {:?}, got {:?}",
            dir.file_name().unwrap(),
            got
        );
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --manifest-path tools/wta/Cargo.toml proc_bind::tests::cwd_for_pid -- --nocapture`
Expected: FAIL to compile ("cannot find function `cwd_for_pid`").

- [ ] **Step 3: Implement `cwd_for_pid` in `proc_bind.rs`**

Add below `read_process_env_block` in `proc_bind.rs`. The x64 `RTL_USER_PROCESS_PARAMETERS.CurrentDirectory.DosPath` is a `UNICODE_STRING` at offset `0x38` (Length u16 at `0x38`, Buffer ptr at `0x40`); this test validates the offset:

```rust
// RTL_USER_PROCESS_PARAMETERS + 0x38 -> CurrentDirectory.DosPath (UNICODE_STRING)
//   +0x38: Length (u16, bytes)   +0x40: Buffer (ptr to UTF-16)
const RUPP_OFFSET_CURDIR_LENGTH: usize = 0x38;
const RUPP_OFFSET_CURDIR_BUFFER: usize = 0x40;

/// Read a process's current working directory from its PEB. `None` if the
/// process is inaccessible or the path is empty.
pub fn cwd_for_pid(pid: u32) -> Option<std::path::PathBuf> {
    let handle = ProcHandle::open(pid)?;
    let pbi = basic_information(handle.0)?;
    let pp = read_remote_ptr(handle.0, pbi.peb_base_address + PEB_OFFSET_PROCESS_PARAMETERS)?;

    // Length is the low u16 of the pointer-sized read at the UNICODE_STRING base.
    let len_word = read_remote_ptr(handle.0, pp + RUPP_OFFSET_CURDIR_LENGTH)?;
    let len_bytes = (len_word & 0xFFFF) as usize;
    if len_bytes == 0 || len_bytes > 0x8000 {
        return None;
    }
    let buf_ptr = read_remote_ptr(handle.0, pp + RUPP_OFFSET_CURDIR_BUFFER)?;
    let raw = read_remote_bytes(handle.0, buf_ptr, len_bytes)?;
    let utf16: Vec<u16> = raw
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let s = String::from_utf16_lossy(&utf16);
    let trimmed = s.trim_end_matches(['\\', '\0']);
    if trimmed.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(trimmed))
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --manifest-path tools/wta/Cargo.toml proc_bind::tests::cwd_for_pid -- --nocapture`
Expected: PASS. (If it fails, the PEB CurrentDirectory offset differs on this build — dump `pp+0x30..0x50` for the test child and adjust `RUPP_OFFSET_CURDIR_*`. This is exactly why it's behind a test.)

- [ ] **Step 5: Commit `cwd_for_pid`**

```bash
git add tools/wta/src/proc_bind.rs
git commit -m "feat(wta/proc_bind): cwd_for_pid (validated PEB CurrentDirectory read)"
```

- [ ] **Step 6: Create `bind.rs` with the failing test for the pure correlation core**

Create `tools/wta/src/session_watcher/bind.rs`:

```rust
//! Bind a discovered session to its hosting WT pane.
//!
//! Strategy per the spec's finalized Decision #3:
//!   * Copilot → `inuse.<pid>.lock` in the session dir (exact).
//!   * Codex   → Restart Manager owner of the rollout file (exact).
//!   * Claude/Gemini → cwd correlation: among live CLI processes, pick the
//!     one whose working directory matches the session's cwd; ties (same cwd)
//!     are left unresolved (returns None) to avoid a wrong bind.
//! Once a pid is chosen, the pane GUID comes from `proc_bind::wt_session_for_pid`.

use crate::proc_bind;
use std::path::{Path, PathBuf};

/// A candidate live CLI process for correlation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub pid: u32,
    pub cwd: PathBuf,
}

/// Pure core: pick the unique candidate whose cwd matches `target`. Returns
/// `None` when there is no match OR more than one match (ambiguous — never
/// guess). Comparison is case-insensitive with trailing separators ignored
/// (Windows paths).
pub fn correlate_by_cwd(candidates: &[Candidate], target: &Path) -> Option<u32> {
    let norm = |p: &Path| {
        p.to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_lowercase()
    };
    let want = norm(target);
    let mut hits = candidates.iter().filter(|c| norm(&c.cwd) == want);
    let first = hits.next()?;
    if hits.next().is_some() {
        None // ambiguous: two same-cwd candidates
    } else {
        Some(first.pid)
    }
}

/// Resolve the pane GUID hosting a Copilot session via its lock file, then PEB.
pub fn bind_copilot(session_dir: &Path) -> Option<String> {
    let pid = proc_bind::copilot_pid_from_lock(session_dir)?;
    proc_bind::wt_session_for_pid(pid)
}

/// Resolve the pane GUID hosting a Codex session via Restart Manager, then PEB.
pub fn bind_codex(rollout_path: &Path) -> Option<String> {
    let pid = proc_bind::file_owner_pid(rollout_path)?;
    proc_bind::wt_session_for_pid(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(pid: u32, cwd: &str) -> Candidate {
        Candidate { pid, cwd: PathBuf::from(cwd) }
    }

    #[test]
    fn unique_cwd_match_binds() {
        let cands = vec![cand(10, r"C:\Users\u\proj"), cand(20, r"C:\Users\u\other")];
        assert_eq!(correlate_by_cwd(&cands, Path::new(r"C:\Users\u\proj")), Some(10));
    }

    #[test]
    fn case_and_trailing_sep_insensitive() {
        let cands = vec![cand(10, r"c:\users\u\proj\")];
        assert_eq!(correlate_by_cwd(&cands, Path::new(r"C:\Users\U\Proj")), Some(10));
    }

    #[test]
    fn ambiguous_same_cwd_returns_none() {
        let cands = vec![cand(10, r"C:\p"), cand(20, r"C:\p")];
        assert_eq!(correlate_by_cwd(&cands, Path::new(r"C:\p")), None);
    }

    #[test]
    fn no_match_returns_none() {
        let cands = vec![cand(10, r"C:\a")];
        assert_eq!(correlate_by_cwd(&cands, Path::new(r"C:\b")), None);
    }
}
```

- [ ] **Step 7: Run the tests**

Run: `cargo test --manifest-path tools/wta/Cargo.toml session_watcher::bind -- --nocapture`
Expected: 4 tests PASS (the `bind_copilot`/`bind_codex` wrappers are exercised end-to-end later by `watch-probe`).

- [ ] **Step 8: Commit**

```bash
git add tools/wta/src/session_watcher/bind.rs
git commit -m "feat(wta/session_watcher): cwd-correlation + copilot/codex binders"
```

---

## Task 8: Incremental reader + `notify` watch loop

Tie discovery + classifiers together: watch the four roots, and on each change read the new records (tail for Copilot/Claude/Codex; whole-file reparse for Gemini) and emit `(CliSource, AgentKey, SessionEvent)`.

**Files:**
- Modify: `tools/wta/src/session_watcher/mod.rs`

- [ ] **Step 1: Add the failing test for the incremental tail reader**

Add to `mod.rs` a pure helper + test (this is the testable half of the loop):

```rust
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Read the bytes appended to `path` since byte offset `from`, returning the
/// decoded text and the new end offset. Used for the append-only CLIs.
pub fn read_appended(path: &Path, from: u64) -> std::io::Result<(String, u64)> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    if len <= from {
        return Ok((String::new(), len));
    }
    file.seek(SeekFrom::Start(from))?;
    let mut buf = Vec::with_capacity((len - from) as usize);
    file.take(len - from).read_to_end(&mut buf)?;
    Ok((String::from_utf8_lossy(&buf).into_owned(), len))
}
```

Add the test in a `#[cfg(test)] mod tests` block in `mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_appended_returns_only_new_bytes() {
        let dir = std::env::temp_dir().join(format!("wta-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.jsonl");
        std::fs::write(&path, b"line1\n").unwrap();
        let (first, off1) = read_appended(&path, 0).unwrap();
        assert_eq!(first, "line1\n");
        assert_eq!(off1, 6);
        // Append more, read only the delta.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"line2\n")
            .unwrap();
        let (second, off2) = read_appended(&path, off1).unwrap();
        assert_eq!(second, "line2\n");
        assert_eq!(off2, 12);
    }
}
```

(Add `use std::io::Write;` to the test module for `write_all`.)

- [ ] **Step 2: Run it**

Run: `cargo test --manifest-path tools/wta/Cargo.toml session_watcher::tests::read_appended -- --nocapture`
Expected: PASS.

- [ ] **Step 3: Add the watch loop**

Add to `mod.rs`. It owns per-file read offsets (and per-Gemini-file message counts) and routes each record through the right classifier:

```rust
use crate::agent_sessions::{CliSource, SessionEvent};
use crate::history_loader; // for the home roots

/// One emitted event with its routing identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Emitted {
    pub cli: CliSource,
    pub key: String,
    pub event: SessionEvent,
}

/// Per-file progress so we only classify new records.
#[derive(Default)]
struct Progress {
    /// Byte offset for append-only CLIs.
    offset: u64,
    /// Message count for Gemini's snapshot model.
    gemini_msgs: usize,
}

/// Process one changed file path into emitted events, advancing `progress`.
/// Pure w.r.t. everything except the on-disk file and the passed-in map.
pub fn process_change(
    path: &Path,
    progress: &mut HashMap<PathBuf, Progress>,
) -> Vec<Emitted> {
    let Some(disc) = discover::identify(path) else { return Vec::new() };
    let entry = progress.entry(path.to_path_buf()).or_default();
    let mut out = Vec::new();

    match disc.cli {
        CliSource::Gemini => {
            // Reparse the whole file; take the last non-empty snapshot line.
            let Ok(text) = std::fs::read_to_string(path) else { return out };
            let Some(last) = text.lines().rev().find(|l| !l.trim().is_empty()) else { return out };
            let Ok(val) = serde_json::from_str::<serde_json::Value>(last) else { return out };
            let (events, new_len) =
                classify_gemini::classify_snapshot(&val, &disc.key, entry.gemini_msgs);
            entry.gemini_msgs = new_len;
            for event in events {
                out.push(Emitted { cli: disc.cli.clone(), key: disc.key.clone(), event });
            }
        }
        _ => {
            let Ok((text, new_off)) = read_appended(path, entry.offset) else { return out };
            entry.offset = new_off;
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() { continue; }
                let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else { continue };
                let events = match disc.cli {
                    CliSource::Copilot => classify_copilot::classify(&val, &disc.key),
                    CliSource::Claude => classify_claude::classify(&val, &disc.key),
                    CliSource::Codex => classify_codex::classify(&val, &disc.key),
                    _ => Vec::new(),
                };
                for event in events {
                    out.push(Emitted { cli: disc.cli.clone(), key: disc.key.clone(), event });
                }
            }
        }
    }
    out
}

/// The four watched roots under the user profile.
pub fn watched_roots() -> Vec<PathBuf> {
    let home = std::env::var("USERPROFILE").map(PathBuf::from).unwrap_or_default();
    vec![
        home.join(".copilot").join("session-state"),
        home.join(".claude").join("projects"),
        home.join(".codex").join("sessions"),
        home.join(".gemini").join("tmp"),
    ]
}
```

(If `history_loader` import is unused here, drop it — it's listed only if a later step references it.)

- [ ] **Step 4: Add an integration test for `process_change` over a copilot fixture**

Add to the `tests` module in `mod.rs`:

```rust
    #[test]
    fn process_change_emits_copilot_events_incrementally() {
        let dir = std::env::temp_dir()
            .join(format!("wta-pc-{}", std::process::id()))
            .join("session-state")
            .join("sess-9");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events.jsonl");
        std::fs::write(
            &path,
            b"{\"type\":\"tool.execution_start\",\"data\":{\"toolName\":\"bash\"}}\n",
        )
        .unwrap();
        let mut progress = HashMap::new();
        let first = process_change(&path, &mut progress);
        assert_eq!(first.len(), 1);
        assert!(matches!(first[0].event, SessionEvent::ToolStarting { .. }));
        // No new bytes -> no duplicate events.
        let second = process_change(&path, &mut progress);
        assert!(second.is_empty());
    }
```

- [ ] **Step 5: Run the module tests**

Run: `cargo test --manifest-path tools/wta/Cargo.toml session_watcher::tests -- --nocapture`
Expected: both `read_appended_*` and `process_change_*` PASS.

- [ ] **Step 6: Add the `notify`-backed `watch` entry point**

Add to `mod.rs`. This is the impure driver; it has no unit test (exercised via `watch-probe` in Task 9):

```rust
use std::sync::mpsc::Sender;

/// Spawn a blocking `notify` watcher over the four roots. Each emitted event
/// is sent on `tx`. Runs until `tx` is dropped or the watcher errors.
///
/// Recursive mode is required: session files live several levels below each
/// root (e.g. `.codex/sessions/YYYY/MM/DD/...`).
pub fn watch(tx: Sender<Emitted>) -> notify::Result<()> {
    use notify::{RecursiveMode, Watcher};

    let (raw_tx, raw_rx) = std::sync::mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = raw_tx.send(res);
    })?;
    for root in watched_roots() {
        // A missing root is fine (the user may not have that CLI) — log + skip.
        if root.exists() {
            if let Err(err) = watcher.watch(&root, RecursiveMode::Recursive) {
                tracing::warn!(target: "session_watcher", root = %root.display(), error = %err, "watch failed");
            }
        }
    }

    let mut progress: HashMap<PathBuf, Progress> = HashMap::new();
    for res in raw_rx {
        let event = match res {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!(target: "session_watcher", error = %err, "notify error");
                continue;
            }
        };
        for path in event.paths {
            for emitted in process_change(&path, &mut progress) {
                if tx.send(emitted).is_err() {
                    return Ok(()); // receiver gone
                }
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 7: Build to confirm it compiles**

```bash
cargo build --manifest-path tools/wta/Cargo.toml
```
Expected: clean build.

- [ ] **Step 8: Commit**

```bash
git add tools/wta/src/session_watcher/mod.rs
git commit -m "feat(wta/session_watcher): incremental reader + notify watch loop"
```

---

## Task 9: `wta watch-probe` debug subcommand

**Files:**
- Modify: `tools/wta/src/main.rs` (`enum Command` + dispatch)

- [ ] **Step 1: Add the `WatchProbe` variant**

In `enum Command`:

```rust
    /// Diagnostics: run the session watcher and print emitted SessionEvents.
    WatchProbe,
```

- [ ] **Step 2: Add the dispatch arm**

In `match cli.command`:

```rust
        Some(Command::WatchProbe) => {
            run_watch_probe();
            Ok(())
        }
```

- [ ] **Step 3: Add the handler**

Add the free function in `main.rs`:

```rust
/// Run the session watcher synchronously, printing each emitted event. Ctrl+C
/// to stop. Diagnostics only.
fn run_watch_probe() {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if let Err(err) = session_watcher::watch(tx) {
            eprintln!("watcher error: {err}");
        }
    });
    println!("watching… drive a copilot/claude/codex/gemini session, Ctrl+C to stop");
    for emitted in rx {
        println!(
            "{:?} key={} -> {:?}",
            emitted.cli, emitted.key, emitted.event
        );
    }
}
```

- [ ] **Step 4: Build, run, and manually verify**

```bash
powershell -NoProfile -Command "Get-Process wta -ErrorAction SilentlyContinue | Stop-Process -Force"
cargo run --manifest-path tools/wta/Cargo.toml -- watch-probe
```
Then, in another pane, run a `copilot`/`claude`/`codex`/`gemini` session and issue a prompt that calls a tool. Expected: lines like `Copilot key=<uuid> -> ToolStarting { ... }` then `ToolCompleted`.

- [ ] **Step 5: Commit**

```bash
git add tools/wta/src/main.rs
git commit -m "feat(wta): add watch-probe diagnostic subcommand"
```

---

## Task 10: Lint, format, finalize

**Files:**
- Modify: any of the new files (only if fmt/clippy require)

- [ ] **Step 1: Format**

Run: `cargo fmt --manifest-path tools/wta/Cargo.toml`
Expected: no diff or whitespace-only.

- [ ] **Step 2: Clippy**

Run: `cargo clippy --manifest-path tools/wta/Cargo.toml -- -D warnings`
Expected: no warnings. Likely fixes: drop any unused `use` (e.g. `history_loader` in `mod.rs` if not referenced), prefer `let-else` (already used).

- [ ] **Step 3: Full module test pass**

Run: `cargo test --manifest-path tools/wta/Cargo.toml session_watcher:: -- --nocapture`
Expected: all classifier (16) + discover (5) + bind (4) + mod (2) tests PASS.

- [ ] **Step 4: Commit**

```bash
git add -A tools/wta/src/session_watcher/ tools/wta/src/main.rs tools/wta/src/proc_bind.rs
git commit -m "chore(wta/session_watcher): fmt + clippy clean"
```

---

## Self-Review Checklist (completed during planning)

- **Spec coverage:** Implements Pillar 1 (discovery + activity classifiers for all four initial CLIs, incl. the Gemini snapshot-reparse caveat) and the binding half of Pillar 2 (copilot lock, codex RM, claude/gemini cwd correlation) on top of Plan A. The registry feed, master ownership, and window scoping are deferred to Plan C (this plan only *emits* `Emitted` events).
- **Placeholder scan:** No TBD/TODO; every code step is complete with real, probe-verified field names (`data.toolName`, `tool_use`/`tool_result`, `payload.function_call`, `$set.messages`/`toolCalls`).
- **Type consistency:** `classify` (copilot/claude/codex) and `classify_snapshot` (gemini) all return `Vec<SessionEvent>` keyed by `&AgentKey`; `process_change` routes by `CliSource` and wraps in `Emitted { cli, key, event }`; `Discovered { cli, key, cwd }` and `Candidate { pid, cwd }` are each defined once and consumed consistently.
- **Dep change:** Task 1 adds `notify` and regenerates `cgmanifest.json` + `NOTICE.md` per AGENTS.md.

## What Plan C will build (preview, not part of this plan)

- Own a `session_watcher::watch` thread in `master/mod.rs`; for each `Emitted`, resolve the pane via `bind` (+ candidate gathering with `proc_bind::cwd_for_pid`/`wt_session_for_pid`), construct the full `SessionStarted` once per new key, and `apply` to the `AgentSessionRegistry`; push the existing `session_added`/`session_removed` mirror to helpers; apply window scoping.
- Delete the hook apparatus: `agent_hooks_installer.rs`, `wt-agent-hooks/**`, the `send-event.ps1` env plumbing (`WTA_HOOK_LOG_DIR`/`WTA_CLI_SOURCE`), and the `hooks` subcommand — then regenerate notices if any dep drops.
