# LLM Agent Event Integration for Windows Terminal

> **Proposal** — Enabling third-party LLM agents to communicate lifecycle state
> through Windows Terminal's event infrastructure via `wtcli` and shell control sequences.

---

## Problem

LLM coding agents (Copilot CLI, Claude Code, aider, Cursor agent, etc.) increasingly
run as CLI processes inside terminal panes. Today, Windows Terminal has no structured
way to know what these agents are *doing*. The orchestrating process (`wta`) must
poll `get_process_status` and scrape pane buffers to infer agent state — burning
tokens, adding latency, and producing unreliable results.

Each LLM agent has natural lifecycle hooks (task started, tool invoked, error,
task completed) that are invisible to the terminal. If agents could **push**
structured events into Windows Terminal, `wta` and other supervisors could react
in real time — routing output, updating UI, coordinating multi-agent workflows —
without polling or guessing.

## Architecture

Two complementary channels for agent→Terminal communication:

```
Channel 1: Shell control sequences (in-band, via stdout)
═══════════════════════════════════════════════════════════════

┌──────────────┐   printf '\e]9001;  ┌──────────────────┐   vt_sequence      ┌──────────────┐
│  Shell       │   AgentEvent;       │  Windows Terminal│   event via        │  wtcli listen│
│  LLM Agent   │   {...}\a'          │  VT Parser       │   OnEvent()        │  (wta)       │
│  (in pane)   │   ─────────────────►│                  │ ─────────────────► │              │
└──────────────┘   (stdout)          └──────────────────┘                    └──────────────┘
   


Channel 2: wtcli send-event (WinRT COM, out-of-band)
═══════════════════════════════════════════════════════════════

┌──────────────┐   wtcli send-event   ┌──────────────────┐   OnEvent()        ┌──────────────┐
│ LLM Agent    │ ───────────────────► │  Windows Terminal│ ────────────────►  │ wtcli listen │
│ (any process)│   (COM IPC via       │  IProtocolServer │   (to subscribed   │ (wta)        │
│              │    WT_COM_CLSID)     │                  │    callbacks)      │              │
└──────────────┘                      └──────────────────┘                    └──────────────┘



```
Channel 1 (control sequences) works from any pane with stdout — no binary dependency.
Shell integration is standard
Agent integration need to extened standard
Channel 2 (`wtcli send-event`) works for **any process** requires with `wtcli` binary and `WT_COM_CLSID` env var.

Both converge on the same `wtcli listen` stream.

### Key components

| Component | What it is | Location |
|-----------|-----------|----------|
| **`wtcli`** | C++ CLI tool using WinRT COM `IProtocolServer` | `src/tools/wtcli/` |
| **`IProtocolServer`** | WinRT COM interface implemented by WT | `src/cascadia/TerminalProtocol/TerminalProtocol.idl` |
| **`IProtocolEventCallback`** | Push-based event callback interface | `TerminalProtocol.idl` |
| **`Subscribe`/`Unsubscribe`** | Event registration on `IProtocolServer` | Already implemented |
| **`wtcli listen`** | Calls `Subscribe()`, prints events to stdout | Already implemented |
| **`ProtocolVtSequenceReceived`** | WT-side event raised on OSC 9001 sequences | `TerminalPage.cpp` |

### What exists today

- `wtcli listen` — subscribes via `IProtocolEventCallback`, receives `vt_sequence` and `connection_state` events
- `s_NotifyEventToComClients()` — broadcasts event JSON to all subscribed COM clients
- `TerminalPage` wires `VtSequenceReceived` + `ConnectionStateChanged` into the event stream
- 17 protocol methods on `IProtocolServer` (queries + mutations) — but **no `send_event`**

### What's missing

1. **`send_event` method** on `IProtocolServer` — client→server event publishing
2. **`wtcli send-event` subcommand** — CLI surface for the above
3. **OSC 9001 `AgentEvent` handler** — in-band control sequence path
4. **Agent-specific event types** — schema for `agent.*` events

---

## Event Schema

All events use the same JSON envelope already emitted by `TerminalPage.cpp`:

```jsonc
{
  "type": "event",
  "method": "agent_event",        // matches existing "vt_sequence" / "connection_state" pattern
  "params": {
    "pane_id": "42",              // source pane (auto-filled by WT for in-pane agents)
    "event": "agent.task.completed",  // namespaced event type
    "timestamp": "2026-04-14T12:00:00Z",
    "agent": "copilot-cli",       // self-reported agent identity
    // ... event-specific fields
  }
}
```

### Standard agent event types

| Event Type | When | Key Fields |
|---|---|---|
| `agent.started` | Agent process ready, accepting tasks | `agent`, `version`, `capabilities[]` |
| `agent.task.started` | Agent begins working on a user request | `task_id`, `description` |
| `agent.tool.invoked` | Agent calls an external tool (shell, file edit, search) | `task_id`, `tool`, `args_summary` |
| `agent.tool.completed` | Tool call returns | `task_id`, `tool`, `exit_code`, `duration_ms` |
| `agent.task.completed` | Agent finishes a task | `task_id`, `exit_code`, `summary` |
| `agent.error` | Recoverable or fatal error | `task_id?`, `error`, `fatal` |
| `agent.idle` | Agent is waiting for input | — |

Agents may define custom events with their own namespace prefix (e.g.,
`copilot.plan.updated`, `aider.diff.applied`). WT treats unknown event types as
opaque pass-through — it stores no schema, just broadcasts them.

---

## Implementation

### 1. WinRT IDL: `SendEvent` method on `IProtocolServer`

Add to `TerminalProtocol.idl`:

```idl
interface IProtocolServer
{
    // ... existing methods ...

    // Events — push-based via callback
    void Subscribe(IProtocolEventCallback callback);
    void Unsubscribe();

    // NEW: Client-originated event publishing
    void SendEvent(String eventJson);
}
```

### 2. C++ server: `TerminalProtocolComServer::SendEvent` (~25 lines)

```cpp
// TerminalProtocolComServer.cpp

void TerminalProtocolComServer::SendEvent(winrt::hstring const& eventJson)
{
    THROW_HR_IF(E_ACCESSDENIED, !_authenticated);

    // Parse and validate — must have "event" field in params
    auto jsonStr = winrt::to_string(eventJson);
    Json::Value evt;
    Json::CharReaderBuilder rb;
    std::string errs;
    std::istringstream ss(jsonStr);
    THROW_HR_IF(E_INVALIDARG, !Json::parseFromStream(rb, ss, &evt, &errs));
    THROW_HR_IF(E_INVALIDARG, !evt.isMember("params") || !evt["params"].isMember("event"));

    // Ensure envelope has type="event" and method="agent_event"
    evt["type"] = "event";
    evt["method"] = "agent_event";

    // Broadcast to all subscribed clients (reuses existing path)
    Json::StreamWriterBuilder wb;
    wb["indentation"] = "";
    s_NotifyEventToComClients(Json::writeString(wb, evt));
}
```

This reuses `s_NotifyEventToComClients()` which already iterates all subscribed
`IProtocolEventCallback` instances — no new IPC path required.

### 3. CLI: `wtcli send-event` subcommand (C++, ~35 lines)

```cpp
// src/tools/wtcli/main.cpp — add subcommand:

std::string sendEventType, sendEventJson;
std::string sendEventPaneTarget;
auto* sendEventCmd = app.add_subcommand("send-event", "Publish an event to all listeners")->alias("se");
sendEventCmd->add_option("-p,--pane", sendEventPaneTarget, "Source pane ID");
sendEventCmd->add_option("-e,--event", sendEventType, "Event type (e.g. agent.task.started)")->required();
sendEventCmd->add_option("json", sendEventJson, "Event params as JSON object");
sendEventCmd->callback([&]() {
    auto server = connect();
    if (!server) return;
    try
    {
        Json::Value evt;
        evt["type"] = "event";
        evt["method"] = "agent_event";

        Json::Value params;
        if (!sendEventJson.empty())
        {
            Json::CharReaderBuilder rb;
            std::string errs;
            std::istringstream ss(sendEventJson);
            Json::parseFromStream(rb, ss, &params, &errs);
        }

        params["event"] = sendEventType;
        if (!sendEventPaneTarget.empty())
            params["pane_id"] = sendEventPaneTarget;
        else
            params["pane_id"] = std::to_string(ResolvePaneId(server, ""));

        evt["params"] = params;

        Json::StreamWriterBuilder wb;
        wb["indentation"] = "";
        server.SendEvent(winrt::to_hstring(Json::writeString(wb, evt)));
    }
    catch (const winrt::hresult_error& e)
    {
        fprintf(stderr, "SendEvent failed: 0x%08X\n", static_cast<uint32_t>(e.code()));
        exitCode = 1;
    }
});
```

Usage:

```bash
# Simple lifecycle event
wtcli send-event -e agent.started '{"agent":"copilot-cli","version":"1.2.0"}'

# Task completion with exit code
wtcli send-event -p 3 -e agent.task.completed '{"task_id":"abc","exit_code":0,"summary":"Built successfully"}'

# Minimal — just the event type (pane auto-resolved from active pane)
wtcli send-event -e agent.idle
```

### 4. Listener enhancement: `--event` filter on `wtcli listen` (~10 lines)

Extend existing `wtcli listen` callback with event-type filtering:

```bash
# All events (existing behavior)
wtcli listen

# Only agent lifecycle events from pane 3
wtcli listen -t 3 --event "agent.*"

# Only task completions from any pane
wtcli listen --event agent.task.completed
```

Add glob matching on `evt["params"]["event"]` inside the `EventCallback` lambda,
alongside the existing `pane_id` filter.

---

## Channel 2: Shell Integration via Control Sequences

The `wtcli send-event` path (Channel 1) requires the `wtcli` binary and
`WT_COM_CLSID`. For lighter-weight integration, agents can emit events as
**OSC 9001 control sequences** written directly to stdout. This works from
any language/runtime with zero binary dependencies.

### Mechanism

Windows Terminal already intercepts OSC 9001 sequences from pane output and
raises them as `VtSequenceReceived` events (see `TerminalPage.cpp:2328`).
The `wtcli listen` subscriber already receives these as `vt_sequence` events.

We define a new OSC 9001 sub-action `AgentEvent` that WT parses into a
structured `agent_event` rather than a raw `vt_sequence`:

```
ESC ] 9001 ; AgentEvent ; <json-payload> BEL
```

### Agent writes to stdout

Any process running in a pane can emit:

```bash
# Bash / sh / zsh
printf '\e]9001;AgentEvent;{"event":"agent.task.started","task_id":"abc","description":"fixing tests"}\a'

# PowerShell
Write-Host "`e]9001;AgentEvent;{`"event`":`"agent.task.started`",`"task_id`":`"abc`"}`a" -NoNewline

# Python
print('\x1b]9001;AgentEvent;{"event":"agent.started","agent":"my-agent"}\x07', end='', flush=True)

# Node.js
process.stdout.write('\x1b]9001;AgentEvent;{"event":"agent.idle"}\x07');
```

### WT parses and broadcasts

In `TerminalPage.cpp`, extend `VtSequenceReceived` handler to detect the
`AgentEvent` prefix and emit a structured `agent_event` instead of raw
`vt_sequence`:

```cpp
term.VtSequenceReceived(
    [weakThis = get_weak(), weakTerm](auto&&, const winrt::hstring& seq) {
        auto strongThis = weakThis.get();
        auto strongTerm = weakTerm.get();
        if (!strongThis || !strongTerm)
            return;

        const auto paneIdStr = strongThis->_FindPaneIdForControl(strongTerm);
        if (paneIdStr.empty())
            return;

        auto seqStr = winrt::to_string(seq);

        // Check for AgentEvent sub-action
        static constexpr std::string_view prefix = "AgentEvent;";
        if (seqStr.starts_with(prefix))
        {
            // Parse the JSON payload after the prefix
            auto jsonPayload = seqStr.substr(prefix.size());
            Json::Value params;
            Json::CharReaderBuilder rb;
            std::string errs;
            std::istringstream ss(jsonPayload);
            if (Json::parseFromStream(rb, ss, &params, &errs))
            {
                params["pane_id"] = paneIdStr;

                Json::Value evt;
                evt["type"] = "event";
                evt["method"] = "agent_event";
                evt["params"] = params;

                Json::StreamWriterBuilder wb;
                wb["indentation"] = "";
                strongThis->ProtocolVtSequenceReceived.raise(
                    *strongThis,
                    winrt::to_hstring(Json::writeString(wb, evt)));
                return;  // Don't also emit as raw vt_sequence
            }
        }

        // Existing behavior for non-AgentEvent sequences
        Json::Value evt;
        evt["type"] = "event";
        evt["method"] = "vt_sequence";
        // ... existing code ...
    });
```

### What `wtcli listen` sees

Both channels produce identical output on the listener's stdout:

```jsonc
// From wtcli send-event -e agent.task.started '{"task_id":"abc"}'
{"type":"event","method":"agent_event","params":{"pane_id":"3","event":"agent.task.started","task_id":"abc"}}

// From printf '\e]9001;AgentEvent;{"event":"agent.task.started","task_id":"abc"}\a'
{"type":"event","method":"agent_event","params":{"pane_id":"3","event":"agent.task.started","task_id":"abc"}}
```

The supervisor doesn't need to know which channel the agent used.

### Why two channels?

| | Channel 1: `wtcli send-event` | Channel 2: OSC 9001 control sequence |
|---|---|---|
| **Dependency** | `wtcli` binary + `WT_COM_CLSID` | None (stdout only) |
| **Works from** | Any process with env vars set | Any process in a WT pane |
| **Pane ID** | Explicit or auto-resolved | Auto-filled by WT (knows which pane emitted) |
| **Latency** | COM call (~1-5ms) | Inline in stdout stream (~0ms) |
| **Best for** | Orchestrator processes, scripts | Agents with direct stdout control |
| **Out-of-pane** | Yes (can target any pane) | No (must be running inside a pane) |

---

## Agent Integration Guide

Each LLM agent needs to emit events at its lifecycle hooks. Three patterns, from
lowest to highest integration effort:

### Pattern A: Control sequence hooks (zero dependencies)

Any agent that writes to stdout can emit structured events with no binary
dependency. This is the recommended path for quick integration.

**Agent startup script (bash):**
```bash
#!/bin/bash
# agent-hooks.sh — source this in your agent wrapper
emit_event() {
  printf '\e]9001;AgentEvent;%s\a' "$1"
}

emit_event '{"event":"agent.started","agent":"'"$AGENT_NAME"'","pid":'$$'}'
trap 'emit_event "{\"event\":\"agent.task.completed\",\"exit_code\":$?}"' EXIT
```

**Agent startup script (PowerShell):**
```powershell
# agent-hooks.ps1 — dot-source this in your agent wrapper
function Send-AgentEvent($Json) {
    Write-Host "`e]9001;AgentEvent;$Json`a" -NoNewline
}

Send-AgentEvent '{"event":"agent.started","agent":"my-agent"}'
try { & $AgentCommand @AgentArgs }
finally { Send-AgentEvent "{`"event`":`"agent.task.completed`",`"exit_code`":$LASTEXITCODE}" }
```

**Python agent (native integration):**
```python
import sys, json

def emit_event(event_type: str, **kwargs):
    payload = json.dumps({"event": event_type, **kwargs})
    sys.stdout.write(f'\x1b]9001;AgentEvent;{payload}\x07')
    sys.stdout.flush()

emit_event("agent.started", agent="my-python-agent", version="0.1.0")
# ... agent work ...
emit_event("agent.tool.invoked", task_id="t1", tool="shell", args_summary="pytest")
# ... tool runs ...
emit_event("agent.tool.completed", task_id="t1", tool="shell", exit_code=0, duration_ms=3200)
emit_event("agent.task.completed", task_id="t1", exit_code=0, summary="All tests pass")
```

### Pattern B: wtcli send-event (CLI, out-of-band)

For orchestrator scripts or agents that don't control their own stdout:

```bash
#!/bin/bash
# run-agent.sh — generic wrapper
PANE_ID=$(wtcli active-pane --json | jq -r '.pane_id')

wtcli send-event -p "$PANE_ID" -e agent.started \
  '{"agent":"'"$1"'","pid":'$$'}'

"$@"
EXIT_CODE=$?

wtcli send-event -p "$PANE_ID" -e agent.task.completed \
  '{"exit_code":'"$EXIT_CODE"'}'

exit $EXIT_CODE
```

### Pattern C: Native ACP hooks (zero-effort for ACP agents)

Agents that speak ACP already have structured tool-use lifecycle. The ACP host
(`wta` TUI mode) can emit events on their behalf — no agent modification needed:

```
ACP agent calls tool "run_command"
  → wta emits agent.tool.invoked {tool: "run_command", args_summary: "cargo test"}
  → wta executes in pane
  → wta emits agent.tool.completed {tool: "run_command", exit_code: 0, duration_ms: 4200}
```

### Pattern D: MCP tool (for MCP-connected agents)

Expose `send_event` as an MCP tool so agents calling `wta mcp` can publish events
natively from their tool-use loop:

```jsonc
{
  "name": "send_event",
  "description": "Publish a lifecycle event to Windows Terminal",
  "inputSchema": {
    "type": "object",
    "properties": {
      "event": { "type": "string", "description": "Event type (e.g. agent.task.started)" },
      "params": { "type": "object", "description": "Event-specific data" }
    },
    "required": ["event"]
  }
}
```

---

## Orchestration Examples

### WTA TUI integration

The `wta` TUI (ACP mode) can subscribe to the same event stream to render
agent status in its status bar — replacing the current `get_process_status` poll loop:

```
┌─ WTA ─────────────────────────────────────────────────┐
│ [Pane 3: copilot-cli] Working: "fix failing tests"    │
│ [Pane 5: aider]       Idle                             │
│ [Pane 7: cargo test]  Exited (0)                       │
└───────────────────────────────────────────────────────┘
```

---

## Scope & Phases

### Phase 1 — Core plumbing (this proposal)

- [ ] `SendEvent` method on `IProtocolServer` IDL + COM implementation (~25 lines C++)
- [ ] `wtcli send-event` CLI subcommand (~35 lines C++)
- [ ] `--event` filter on `wtcli listen` callback (~10 lines C++)
- [ ] OSC 9001 `AgentEvent` handler in `TerminalPage.cpp` (~30 lines C++)

**Total: ~100 lines of new C++ code. No breaking changes to existing interfaces.**

### Phase 2 — WT-native events (builds on shell integration work)

Leverage the event types proposed in
[terminal-acp-shell-integration.md](../../../tools/wta/terminal-acp-shell-integration.md):

- [ ] `pane.process.exited` — emitted by WT when a pane's child process exits
- [ ] `pane.cwd.changed` — emitted when CWD changes (OSC 7 / OSC 9;9)
- [ ] `pane.command.completed` — emitted at OSC 133;D (FTCS command end) with exit code

These are **server-originated** events (WT → listeners) as opposed to Phase 1's
client-originated events (agent → WT → listeners). Both flow through the same
`s_NotifyEventToComClients()` path.

### Phase 3 — Agent SDK & conventions

- [ ] Publish `agent-events` JSON Schema for all `agent.*` types
- [ ] Reference integration scripts for popular agents (Copilot CLI, Claude Code, aider)
- [ ] `wtcli wrap` convenience command that auto-instruments any CLI with start/complete events
- [ ] Event history ring buffer for late-connecting listeners

---

## Design Decisions

| Decision | Rationale |
|----------|-----------|
| **Two channels (COM + control sequence)** | COM for out-of-band orchestration; control sequences for zero-dependency in-pane agents |
| **Reuse `s_NotifyEventToComClients`** | Existing broadcast machinery works. No new IPC channel. |
| **No server-side event storage** | WT is a relay, not a message queue. Keep it simple for v1. |
| **Namespace convention, not enforcement** | `agent.*` is a convention. WT passes through any event type. Avoids schema coupling. |
| **pane_id auto-fill for control sequences** | WT knows which pane emitted the OSC sequence — agents don't need to discover their own ID. |
| **Control sequences invisible to user** | OSC sequences are consumed by the VT parser and not rendered — agent events don't pollute visible output. |
| **Same JSON shape from both channels** | Listeners don't need to know which channel was used. |
| **Events are fire-and-forget** | `SendEvent` returns void (no delivery guarantee). Matches terminal's ephemeral nature. |

---

## Open Questions

1. **Rate limiting** — Should WT throttle `SendEvent` / OSC AgentEvent to prevent
   a misbehaving agent from flooding listeners? (Suggest: no for v1 — trust
   authenticated clients and pane processes.)

2. **Event history** — Should WT keep a bounded ring buffer of recent events so
   late-connecting listeners can catch up? (Suggest: defer to Phase 3.)

3. **Structured event registration** — Should agents declare their event types
   upfront via `GetCapabilities`, or is convention-based enough?
   (Suggest: convention-based for v1.)

4. **Control sequence payload size** — OSC sequences have no formal length limit
   in WT, but very large payloads may be impractical. Recommend documenting a
   soft limit (e.g., 4KB per event).
