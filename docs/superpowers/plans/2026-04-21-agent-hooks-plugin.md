# Agent Hooks Plugin Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create a Copilot CLI plugin that forwards agent hook events (tool use, session lifecycle, notifications) to Windows Terminal via `wtcli send-event`, and display them in WTA's TUI when `WTA_LOG_AGENT_EVENT=1`.

**Architecture:** Plugin hook scripts read JSON from stdin, normalize across CLI formats (Claude/Copilot/Gemini), and call `wtcli send-event -e <type> '<json>'`. WTA receives events via its existing `wtcli listen --json` subscriber. A new `log_agent_events` flag (from env var) gates display as `ChatMessage::AgentEvent` entries in the TUI chat area.

**Tech Stack:** Bash (hook scripts), Rust (WTA app.rs, theme.rs, chat.rs), JSON (plugin.json, hooks.json)

**Spec:** `docs/superpowers/specs/2026-04-21-agent-hooks-plugin-design.md`

**Note on Task 5 + Task 6 dependency:** Task 5 references `ChatMessage::AgentEvent` which is defined in Task 6. If executing sequentially, either combine Tasks 5 and 6, or use `ChatMessage::System` in Task 5 as a temporary stand-in and switch to `AgentEvent` in Task 6.

---

### Task 1: Plugin Scaffold — plugin.json and hooks.json

**Files:**
- Create: `wta/agent-hooks-plugin/plugin.json`
- Create: `wta/agent-hooks-plugin/hooks/hooks.json`
- Create: `wta/agent-hooks-plugin/README.md`

- [ ] **Step 1: Create plugin.json manifest**

```json
{
  "name": "wt-agent-hooks",
  "description": "Forward CLI agent hook events to Windows Terminal for WTA display",
  "version": "0.1.0",
  "author": { "name": "Agentic Terminal" },
  "license": "MIT",
  "keywords": ["windows-terminal", "agent-hooks", "wta"],
  "hooks": "hooks/hooks.json"
}
```

- [ ] **Step 2: Create hooks/hooks.json registering all lifecycle events**

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "*",
        "hooks": [{
          "type": "command",
          "command": "\"${CLAUDE_PLUGIN_ROOT}/hooks/run-hook.cmd\" pre-tool-use"
        }]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "*",
        "hooks": [{
          "type": "command",
          "command": "\"${CLAUDE_PLUGIN_ROOT}/hooks/run-hook.cmd\" post-tool-use"
        }]
      }
    ],
    "Notification": [
      {
        "matcher": "*",
        "hooks": [{
          "type": "command",
          "command": "\"${CLAUDE_PLUGIN_ROOT}/hooks/run-hook.cmd\" notification"
        }]
      }
    ],
    "Stop": [
      {
        "matcher": "*",
        "hooks": [{
          "type": "command",
          "command": "\"${CLAUDE_PLUGIN_ROOT}/hooks/run-hook.cmd\" session-stop"
        }]
      }
    ],
    "SubagentStop": [
      {
        "matcher": "*",
        "hooks": [{
          "type": "command",
          "command": "\"${CLAUDE_PLUGIN_ROOT}/hooks/run-hook.cmd\" session-stop"
        }]
      }
    ]
  }
}
```

- [ ] **Step 3: Create README.md**

Brief README explaining:
- What the plugin does (forwards CLI agent events to Windows Terminal)
- How to install: `copilot plugin install ./wta/agent-hooks-plugin`
- How to enable display: set `WTA_LOG_AGENT_EVENT=1` env var before launching WTA
- Supported CLIs: Copilot CLI, Claude Code (Gemini CLI planned)
- Requires: `wtcli` on PATH (automatic inside Windows Terminal), `jq` installed

- [ ] **Step 4: Commit**

```bash
git add wta/agent-hooks-plugin/
git commit -m "feat(plugin): scaffold wt-agent-hooks plugin with hooks.json"
```

---

### Task 2: Cross-Platform Hook Launcher — run-hook.cmd

**Files:**
- Create: `wta/agent-hooks-plugin/hooks/run-hook.cmd`

This is a polyglot cmd/bash script (same pattern as the superpowers plugin at `~/.copilot/installed-plugins/superpowers-marketplace/superpowers/hooks/run-hook.cmd`). On Windows it finds Git Bash; on Unix it runs bash directly.

- [ ] **Step 1: Create run-hook.cmd**

```bash
: << 'CMDBLOCK'
@echo off
REM Cross-platform polyglot wrapper for hook scripts.
REM On Windows: cmd.exe runs the batch portion, which finds and calls bash.
REM On Unix: the shell interprets this as a script (: is a no-op in bash).
REM
REM Usage: run-hook.cmd <script-name> [args...]

if "%~1"=="" (
    echo run-hook.cmd: missing script name >&2
    exit /b 1
)

set "HOOK_DIR=%~dp0"

REM Try Git for Windows bash in standard locations
if exist "C:\Program Files\Git\bin\bash.exe" (
    "C:\Program Files\Git\bin\bash.exe" "%HOOK_DIR%%~1" %2 %3 %4 %5 %6 %7 %8 %9
    exit /b %ERRORLEVEL%
)
if exist "C:\Program Files (x86)\Git\bin\bash.exe" (
    "C:\Program Files (x86)\Git\bin\bash.exe" "%HOOK_DIR%%~1" %2 %3 %4 %5 %6 %7 %8 %9
    exit /b %ERRORLEVEL%
)

REM Try bash on PATH
where bash >nul 2>nul
if %ERRORLEVEL% equ 0 (
    bash "%HOOK_DIR%%~1" %2 %3 %4 %5 %6 %7 %8 %9
    exit /b %ERRORLEVEL%
)

REM No bash found - exit silently (hooks must never block the agent)
exit /b 0
CMDBLOCK

# Unix: run the named script directly
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SCRIPT_NAME="$1"
shift
exec bash "${SCRIPT_DIR}/${SCRIPT_NAME}" "$@"
```

- [ ] **Step 2: Commit**

```bash
git add wta/agent-hooks-plugin/hooks/run-hook.cmd
git commit -m "feat(plugin): add cross-platform hook launcher"
```

---

### Task 3: Hook Scripts — pre-tool-use and post-tool-use

**Files:**
- Create: `wta/agent-hooks-plugin/hooks/pre-tool-use`
- Create: `wta/agent-hooks-plugin/hooks/post-tool-use`

Each script reads JSON from stdin, detects the CLI source via env vars, extracts relevant fields using `jq`, and calls `wtcli send-event`. If `wtcli` is not on PATH or `WT_COM_CLSID` is not set, the script exits silently with code 0.

**Dependencies:** `jq` must be installed. On Windows this is available via `winget install jqlang.jq` or Git Bash may include it.

- [ ] **Step 1: Create hooks/pre-tool-use**

```bash
#!/usr/bin/env bash
# PreToolUse hook — forward tool-starting event to Windows Terminal
set -euo pipefail

# Exit silently if not inside Windows Terminal
[ -z "${WT_COM_CLSID:-}" ] && exit 0

# Exit silently if wtcli not available
command -v wtcli >/dev/null 2>&1 || exit 0

# Read JSON from stdin
INPUT="$(cat)"
[ -z "$INPUT" ] && exit 0

# Detect CLI source
if [ -n "${COPILOT_CLI:-}" ]; then
  CLI_SOURCE="copilot"
  TOOL_NAME=$(echo "$INPUT" | jq -r '.toolName // .tool_name // "unknown"')
  TOOL_ARGS=$(echo "$INPUT" | jq -c '.toolArgs // .tool_input // {}' | head -c 200)
elif [ -n "${CLAUDE_PLUGIN_ROOT:-}" ]; then
  CLI_SOURCE="claude"
  TOOL_NAME=$(echo "$INPUT" | jq -r '.tool_name // "unknown"')
  TOOL_ARGS=$(echo "$INPUT" | jq -c '.tool_input // {}' | head -c 200)
else
  CLI_SOURCE="unknown"
  TOOL_NAME=$(echo "$INPUT" | jq -r '.tool_name // .toolName // "unknown"')
  TOOL_ARGS=$(echo "$INPUT" | jq -c '.tool_input // .toolArgs // {}' | head -c 200)
fi

# Build params JSON
PARAMS=$(jq -nc \
  --arg tool "$TOOL_NAME" \
  --arg args "$TOOL_ARGS" \
  --arg src  "$CLI_SOURCE" \
  '{tool_name: $tool, args_summary: $args, cli_source: $src}')

# Send event (fire-and-forget, suppress errors)
wtcli send-event -e "agent.tool.starting" "$PARAMS" 2>/dev/null || true
exit 0
```

- [ ] **Step 2: Create hooks/post-tool-use**

```bash
#!/usr/bin/env bash
# PostToolUse hook — forward tool-completed event to Windows Terminal
set -euo pipefail

[ -z "${WT_COM_CLSID:-}" ] && exit 0
command -v wtcli >/dev/null 2>&1 || exit 0

INPUT="$(cat)"
[ -z "$INPUT" ] && exit 0

if [ -n "${COPILOT_CLI:-}" ]; then
  CLI_SOURCE="copilot"
  TOOL_NAME=$(echo "$INPUT" | jq -r '.toolName // .tool_name // "unknown"')
  RESULT_TYPE=$(echo "$INPUT" | jq -r '.toolResult.resultType // "completed"')
  RESULT_SUMMARY=$(echo "$INPUT" | jq -r '(.toolResult.textResultForLlm // "")[0:200]')
elif [ -n "${CLAUDE_PLUGIN_ROOT:-}" ]; then
  CLI_SOURCE="claude"
  TOOL_NAME=$(echo "$INPUT" | jq -r '.tool_name // "unknown"')
  if echo "$INPUT" | jq -e '.tool_response.interrupted == true' >/dev/null 2>&1; then
    RESULT_TYPE="interrupted"
  else
    RESULT_TYPE="success"
  fi
  RESULT_SUMMARY=$(echo "$INPUT" | jq -r '(.tool_response.stdout // "")[0:200]')
else
  CLI_SOURCE="unknown"
  TOOL_NAME=$(echo "$INPUT" | jq -r '.tool_name // .toolName // "unknown"')
  RESULT_TYPE="completed"
  RESULT_SUMMARY=""
fi

PARAMS=$(jq -nc \
  --arg tool    "$TOOL_NAME" \
  --arg result  "$RESULT_TYPE" \
  --arg summary "$RESULT_SUMMARY" \
  --arg src     "$CLI_SOURCE" \
  '{tool_name: $tool, result_type: $result, result_summary: $summary, cli_source: $src}')

wtcli send-event -e "agent.tool.completed" "$PARAMS" 2>/dev/null || true
exit 0
```

- [ ] **Step 3: Make scripts executable and commit**

```bash
chmod +x wta/agent-hooks-plugin/hooks/pre-tool-use
chmod +x wta/agent-hooks-plugin/hooks/post-tool-use
git add wta/agent-hooks-plugin/hooks/pre-tool-use wta/agent-hooks-plugin/hooks/post-tool-use
git commit -m "feat(plugin): add pre-tool-use and post-tool-use hook scripts"
```

---

### Task 4: Hook Scripts — session-stop and notification

**Files:**
- Create: `wta/agent-hooks-plugin/hooks/session-stop`
- Create: `wta/agent-hooks-plugin/hooks/notification`

- [ ] **Step 1: Create hooks/session-stop**

```bash
#!/usr/bin/env bash
# Stop / SubagentStop hook — forward session-ended event to Windows Terminal
set -euo pipefail

[ -z "${WT_COM_CLSID:-}" ] && exit 0
command -v wtcli >/dev/null 2>&1 || exit 0

INPUT="$(cat)"

if [ -n "${COPILOT_CLI:-}" ]; then
  CLI_SOURCE="copilot"
  REASON=$(echo "$INPUT" | jq -r '.reason // "unknown"')
elif [ -n "${CLAUDE_PLUGIN_ROOT:-}" ]; then
  CLI_SOURCE="claude"
  REASON=$(echo "$INPUT" | jq -r '.hook_event_name // "Stop"')
else
  CLI_SOURCE="unknown"
  REASON=$(echo "$INPUT" | jq -r '.reason // .hook_event_name // "unknown"')
fi

PARAMS=$(jq -nc \
  --arg src    "$CLI_SOURCE" \
  --arg reason "$REASON" \
  '{cli_source: $src, reason: $reason}')

wtcli send-event -e "agent.session.stopped" "$PARAMS" 2>/dev/null || true
exit 0
```

- [ ] **Step 2: Create hooks/notification**

```bash
#!/usr/bin/env bash
# Notification hook — forward agent notification to Windows Terminal
set -euo pipefail

[ -z "${WT_COM_CLSID:-}" ] && exit 0
command -v wtcli >/dev/null 2>&1 || exit 0

INPUT="$(cat)"
[ -z "$INPUT" ] && exit 0

if [ -n "${COPILOT_CLI:-}" ]; then
  CLI_SOURCE="copilot"
  MESSAGE=$(echo "$INPUT" | jq -r '(.notification // .message // "")[0:300]')
elif [ -n "${CLAUDE_PLUGIN_ROOT:-}" ]; then
  CLI_SOURCE="claude"
  MESSAGE=$(echo "$INPUT" | jq -r '(.notification // .message // "")[0:300]')
else
  CLI_SOURCE="unknown"
  MESSAGE=$(echo "$INPUT" | jq -r '(.notification // .message // "")[0:300]')
fi

[ -z "$MESSAGE" ] && exit 0

PARAMS=$(jq -nc \
  --arg src "$CLI_SOURCE" \
  --arg msg "$MESSAGE" \
  '{cli_source: $src, message: $msg}')

wtcli send-event -e "agent.notification" "$PARAMS" 2>/dev/null || true
exit 0
```

- [ ] **Step 3: Make scripts executable and commit**

```bash
chmod +x wta/agent-hooks-plugin/hooks/session-stop
chmod +x wta/agent-hooks-plugin/hooks/notification
git add wta/agent-hooks-plugin/hooks/session-stop wta/agent-hooks-plugin/hooks/notification
git commit -m "feat(plugin): add session-stop and notification hook scripts"
```

---

### Task 5: WTA — Add `log_agent_events` flag and agent event handling

**Files:**
- Modify: `wta/src/app.rs:354` — Add `log_agent_events: bool` field to `App` struct
- Modify: `wta/src/app.rs:358-416` — Update `App::new()` to accept and store the flag
- Modify: `wta/src/app.rs:814-895` — Add agent event interception in WtEvent handler
- Modify: `wta/src/main.rs` — Read env var and pass to `App::new()`

- [ ] **Step 1: Add `log_agent_events` field to `App` struct**

In `wta/src/app.rs`, add after line 354 (`pub autofix_enabled: bool,`):

```rust
    /// When true, display agent hook events (from wt-agent-hooks plugin) in the chat area.
    /// Controlled by the WTA_LOG_AGENT_EVENT env var.
    pub log_agent_events: bool,
```

- [ ] **Step 2: Update `App::new()` to accept and store the flag**

Add `log_agent_events: bool` parameter to `App::new()` signature (after `autofix_enabled: bool`). In the `Self { ... }` initializer block, add after `autofix_enabled,`:

```rust
            log_agent_events,
```

- [ ] **Step 3: Read env var at WTA startup and pass to `App::new()`**

In `wta/src/main.rs`, find where `App::new(...)` is called. Before the call, compute:

```rust
let log_agent_events = std::env::var("WTA_LOG_AGENT_EVENT")
    .map(|v| matches!(v.as_str(), "1" | "true" | "yes"))
    .unwrap_or(false);
```

Pass `log_agent_events` as the new last argument to `App::new()`.

Note: `App::new()` is called in multiple places — search for `App::new(` in `main.rs`. Update all call sites.

- [ ] **Step 4: Add agent event interception in WtEvent handler**

In the `AppEvent::WtEvent { method, session_id, params }` match arm in `app.rs` (around line 832), add this block **after** the `autofix_execute` check and **before** the same-pane skip:

```rust
                // Agent hook events (from wt-agent-hooks plugin) — display if enabled
                if method == "agent_event" {
                    if let Some(event_type) = params.get("event").and_then(|v| v.as_str()) {
                        if event_type.starts_with("agent.") {
                            if self.log_agent_events {
                                self.display_agent_hook_event(event_type, &params);
                            }
                            return;
                        }
                    }
                }
```

- [ ] **Step 5: Implement `display_agent_hook_event` method on App**

Add this method inside `impl App` (after the autofix methods, around line 1400):

```rust
    /// Format and display an agent hook event as a chat message.
    fn display_agent_hook_event(&mut self, event_type: &str, params: &serde_json::Value) {
        let cli_source = params
            .get("cli_source")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let detail = match event_type {
            "agent.tool.starting" => {
                let tool = params.get("tool_name").and_then(|v| v.as_str()).unwrap_or("?");
                let args = params.get("args_summary").and_then(|v| v.as_str()).unwrap_or("");
                let args_trunc = if args.len() > 80 { &args[..80] } else { args };
                format!("─ {} ─\n  Tool: {}\n  Args: {}\n  Source: {}", event_type, tool, args_trunc, cli_source)
            }
            "agent.tool.completed" => {
                let tool = params.get("tool_name").and_then(|v| v.as_str()).unwrap_or("?");
                let result = params.get("result_type").and_then(|v| v.as_str()).unwrap_or("?");
                let summary = params.get("result_summary").and_then(|v| v.as_str()).unwrap_or("");
                let summary_trunc = if summary.len() > 80 { &summary[..80] } else { summary };
                if summary_trunc.is_empty() {
                    format!("─ {} ─\n  Tool: {}\n  Result: {}\n  Source: {}", event_type, tool, result, cli_source)
                } else {
                    format!("─ {} ─\n  Tool: {}\n  Result: {}\n  Output: {}\n  Source: {}", event_type, tool, result, summary_trunc, cli_source)
                }
            }
            "agent.session.stopped" => {
                let reason = params.get("reason").and_then(|v| v.as_str()).unwrap_or("unknown");
                format!("─ {} ─\n  Reason: {}\n  Source: {}", event_type, reason, cli_source)
            }
            "agent.notification" => {
                let msg = params.get("message").and_then(|v| v.as_str()).unwrap_or("");
                format!("─ {} ─\n  {}\n  Source: {}", event_type, msg, cli_source)
            }
            _ => {
                format!("─ {} ─\n  Source: {}", event_type, cli_source)
            }
        };

        self.messages.push(ChatMessage::AgentEvent(detail));
        self.scroll_to_bottom();
    }
```

- [ ] **Step 6: Verify Rust compilation**

Run: `cargo check --manifest-path wta/Cargo.toml`
Expected: Compiles with 0 errors (warnings OK). Will fail until Task 6 adds the `AgentEvent` variant — if doing tasks sequentially, use `ChatMessage::System(detail)` temporarily and switch in Task 6.

- [ ] **Step 7: Commit**

```bash
git add wta/src/app.rs wta/src/main.rs
git commit -m "feat(wta): display agent hook events gated by WTA_LOG_AGENT_EVENT"
```

---

### Task 6: Distinct Visual Style for Agent Events

**Files:**
- Modify: `wta/src/theme.rs:45` — Add `AGENT_EVENT_HEADER` and `AGENT_EVENT_DETAIL` styles
- Modify: `wta/src/app.rs:47-58` — Add `AgentEvent(String)` variant to `ChatMessage` enum
- Modify: `wta/src/ui/chat.rs:273-282` — Add rendering match arm for `AgentEvent`

- [ ] **Step 1: Add AGENT_EVENT styles to theme.rs**

In `wta/src/theme.rs`, add after line 45 (`pub const BANNER_HINT`):

```rust
// Agent hook event styles
pub const AGENT_EVENT_HEADER: Style = Style::new().fg(Color::Magenta);
pub const AGENT_EVENT_DETAIL: Style = Style::new().fg(Color::DarkGray);
```

- [ ] **Step 2: Add `AgentEvent` variant to `ChatMessage`**

In `wta/src/app.rs`, in the `ChatMessage` enum (after `Error(String),`), add:

```rust
    AgentEvent(String),
```

- [ ] **Step 3: Add rendering in chat.rs**

In `wta/src/ui/chat.rs`, in the `build_message_lines` function, add a match arm after the `ChatMessage::Error` arm (after line ~282):

```rust
        ChatMessage::AgentEvent(text) => {
            for (i, line_text) in text.lines().enumerate() {
                if i == 0 {
                    lines.push(Line::from(Span::styled(
                        truncate_render_text(line_text),
                        theme::AGENT_EVENT_HEADER,
                    )));
                } else {
                    lines.push(Line::from(Span::styled(
                        truncate_render_text(line_text),
                        theme::AGENT_EVENT_DETAIL,
                    )));
                }
            }
            lines.push(Line::default());
        }
```

- [ ] **Step 4: Verify Rust compilation**

Run: `cargo check --manifest-path wta/Cargo.toml`
Expected: Compiles with 0 errors

- [ ] **Step 5: Commit**

```bash
git add wta/src/theme.rs wta/src/app.rs wta/src/ui/chat.rs
git commit -m "feat(wta): distinct visual style for agent hook events"
```

---

### Task 7: End-to-End Smoke Test

**Files:** No new files — manual verification

- [ ] **Step 1: Build WTA**

```bash
cargo build --manifest-path wta/Cargo.toml
```

Expected: Build succeeds with 0 errors.

- [ ] **Step 2: Install the plugin locally**

```bash
copilot plugin install ./wta/agent-hooks-plugin
```

Verify: `copilot plugin list` shows `wt-agent-hooks`.

- [ ] **Step 3: Test hook script outside WT (graceful degradation)**

```bash
echo '{"tool_name":"bash","tool_input":{"command":"ls"}}' | bash wta/agent-hooks-plugin/hooks/pre-tool-use
echo $?
```

Expected: Exits with code 0, no output (no `WT_COM_CLSID` set).

- [ ] **Step 4: Build and deploy Terminal for full end-to-end test**

```bash
# Build C++ (only needed if TerminalProtocolComServer changed)
cmd /c ".\tools\razzle.cmd && bz"
# Deploy
DeployAppRecipe.exe src\cascadia\CascadiaPackage\bin\x64\Debug\CascadiaPackage.build.appxrecipe
```

- [ ] **Step 5: End-to-end test in deployed Terminal**

1. Launch AgenticTerminal
2. Open agent pane (should be running WTA)
3. In agent pane's environment, verify `WTA_LOG_AGENT_EVENT=1` is set
4. In another pane, start Copilot CLI — tool use events should appear in agent pane as magenta-styled blocks
5. Verify events show tool name, args, source

- [ ] **Step 6: Final fixups and commit**

```bash
git add -A
git commit -m "feat: wt-agent-hooks plugin complete — forward CLI agent events to WTA"
```
