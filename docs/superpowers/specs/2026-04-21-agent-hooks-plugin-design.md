# Agent Hooks Plugin — Design Spec

## Problem

CLI agents (Claude Code, Copilot CLI, Gemini CLI) running in a Windows Terminal pane have no way to report their activity to WTA running in the agent pane. WTA can't see what tools the agent is calling, when sessions start/stop, or what errors occur. This makes the agent pane a passive observer rather than a coordinated partner.

## Approach

Create a **Copilot CLI plugin** (`wt-agent-hooks`) that registers hooks for all major agent lifecycle events. Each hook script reads the event payload from stdin, normalizes it across CLI formats, and forwards it to Windows Terminal via `wtcli send-event`. WTA receives these events through its existing `wtcli listen --json` subscriber and displays them as formatted multi-line blocks in the TUI.

## Architecture

```
┌─────────────────────┐     stdin JSON      ┌──────────────────┐
│  CLI Agent (pane 1) │ ──────────────────> │  Hook Script     │
│  Claude/Copilot/    │   (PreToolUse,      │  (normalize +    │
│  Gemini             │    PostToolUse,      │   wtcli send-    │
└─────────────────────┘    Stop, etc.)       │   event)         │
                                             └────────┬─────────┘
                                                      │ wtcli send-event
                                                      ▼
                                    ┌─────────────────────────────┐
                                    │  TerminalProtocolComServer   │
                                    │  SendEvent → broadcast to   │
                                    │  all subscribers             │
                                    └────────────┬────────────────┘
                                                 │ OnEvent callback
                                                 ▼
                                    ┌─────────────────────────────┐
                                    │  WTA (pane 2)               │
                                    │  wtcli listen --json        │
                                    │  → display in TUI           │
                                    └─────────────────────────────┘
```

## Plugin Structure

```
wta/agent-hooks-plugin/
├── plugin.json                 # Plugin manifest
├── hooks/
│   ├── hooks.json              # Hook event registrations
│   ├── run-hook.cmd            # Cross-platform polyglot launcher
│   ├── pre-tool-use            # bash: normalize + send PreToolUse
│   ├── post-tool-use           # bash: normalize + send PostToolUse
│   ├── session-start           # bash: normalize + send SessionStart
│   ├── session-stop            # bash: normalize + send Stop/SessionEnd
│   └── notification            # bash: normalize + send Notification
└── README.md
```

### plugin.json

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

### hooks/hooks.json

Registers all supported hook events. Each hook calls `run-hook.cmd <script-name>`.

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

## Hook Scripts

### Normalization

Each CLI agent sends different JSON schemas on stdin. The hook scripts normalize to a standard format before calling `wtcli send-event`.

**Normalized event types:**

| Hook Event | `wtcli send-event -e` value | Key params |
|---|---|---|
| PreToolUse | `agent.tool.starting` | `tool_name`, `tool_args_summary` |
| PostToolUse | `agent.tool.completed` | `tool_name`, `result_type`, `result_summary` |
| SessionStart | `agent.session.started` | `cli_source`, `cwd` |
| Stop | `agent.session.stopped` | `cli_source`, `reason` |
| SubagentStop | `agent.subagent.stopped` | `cli_source`, `reason` |
| Notification | `agent.notification` | `cli_source`, `message` |

### Input formats by CLI

**Claude Code** (all hooks): `{ session_id, tool_name, tool_input, tool_response, hook_event_name, cwd, ... }`

**Copilot CLI** (all hooks): `{ timestamp, cwd, toolName, toolArgs, toolResult, prompt, source, reason, ... }`

**Gemini CLI** (all hooks): `{ hook_type, tool_name, tool_input, session_id, ... }`

### Detection logic

```bash
if [ -n "${COPILOT_CLI:-}" ]; then
  CLI_SOURCE="copilot"
elif [ -n "${CLAUDE_PLUGIN_ROOT:-}" ]; then
  CLI_SOURCE="claude"
elif [ -n "${GEMINI_CLI:-}" ]; then
  CLI_SOURCE="gemini"
else
  CLI_SOURCE="unknown"
fi
```

### wtcli invocation

Hook scripts call `wtcli send-event` which is already on PATH inside a WT pane (via `WT_COM_CLSID`):

```bash
wtcli send-event -e "agent.tool.starting" "{\"tool_name\":\"bash\",\"args_summary\":\"ls -la\",\"cli_source\":\"claude\"}"
```

If `wtcli` is not available (agent running outside WT), the hook exits silently (exit 0) — hooks must never block the agent.

### run-hook.cmd

Polyglot cmd/bash launcher copied from superpowers pattern. Finds bash on Windows (Git for Windows), runs named script directly on Unix.

## WTA Display

### Event handling in WTA

WTA already receives events via `wtcli listen --json`. The `classify_wt_event` function in `app.rs` needs a new arm for `agent_event` events where `params.event` starts with `agent.`.

These are **informational** events — they don't trigger autofix or notifications.

### Gating: `WTA_LOG_AGENT_EVENT` env var

Display of agent hook events is **off by default**. It is enabled by setting the environment variable `WTA_LOG_AGENT_EVENT=1` (or any truthy value) before launching WTA.

When disabled, WTA still receives the events via the listener but silently discards them. This keeps the TUI clean for users who don't need hook telemetry.

```rust
// At startup, read once and cache:
let log_agent_events = std::env::var("WTA_LOG_AGENT_EVENT")
    .map(|v| matches!(v.as_str(), "1" | "true" | "yes"))
    .unwrap_or(false);
```

### Display format (when enabled)

Multi-line block with details, using WTA's existing VT color system:

```
─── agent.tool.starting ──────────────────
  Tool:   bash
  Args:   ls -la src/
  Source: claude
──────────────────────────────────────────

─── agent.tool.completed ─────────────────
  Tool:   bash
  Result: success
  Output: (12 files listed)
  Source: claude
──────────────────────────────────────────
```

### Implementation in app.rs

Add a new match arm in the WtEvent handler:

```rust
// In handle_wt_event or the WtEvent match arm:
if event_method == "agent_event" {
    if let Some(event_type) = params.get("event").and_then(|v| v.as_str()) {
        if event_type.starts_with("agent.") {
            if self.log_agent_events {
                self.display_agent_hook_event(event_type, &params);
            }
            return; // Don't classify as autofix-triggerable
        }
    }
}
```

The `display_agent_hook_event` method formats the event as a multi-line block and appends it to the chat/output area. The `log_agent_events` field is set once at `App` construction from the env var.

## Installation

### Local development

```bash
copilot plugin install ./wta/agent-hooks-plugin
```

### From GitHub (future)

After pushing to a public repo or marketplace:
```bash
copilot plugin install yeelam-gordon/agentic-terminal:wta/agent-hooks-plugin
```

### Claude Code (if plugin system differs)

Claude Code also reads `.claude/settings.json` for hooks. The plugin's `hooks.json` format is compatible. If Claude Code's plugin install doesn't work, fallback is to symlink or copy the hooks config:

```bash
# In project root:
ln -s wta/agent-hooks-plugin/.claude/settings.json .claude/settings.json
```

## Testing

1. **Hook script standalone**: Run `echo '{"tool_name":"bash","tool_input":{"command":"ls"}}' | bash hooks/pre-tool-use` and verify `wtcli send-event` is called
2. **End-to-end**: Install plugin, run Claude Code in one pane, verify WTA in agent pane shows events
3. **Graceful degradation**: Run hook outside WT (no `WT_COM_CLSID`) — should exit silently

## Scope boundaries

**In scope:**
- Plugin structure with hooks.json
- Hook scripts for all lifecycle events (PreToolUse, PostToolUse, Stop, SubagentStop, Notification)
- Cross-platform launcher (run-hook.cmd)
- WTA display of agent events in TUI
- Local install via `copilot plugin install`

**Out of scope (future):**
- Filtering/muting specific event types in WTA settings
- Rich TUI visualizations (progress bars, tool use timelines)
- Two-way communication (WTA sending commands back to agent)
- Gemini CLI install support (different plugin mechanism)
- Marketplace publication
