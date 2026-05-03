# wt-agent-hooks Plugin

Forward CLI agent hook events to Windows Terminal for WTA display.

## Overview

This Copilot CLI plugin bridges agent lifecycle events to Windows Terminal's WTA (Windows Terminal Agent) infrastructure, enabling real-time visibility into agent tool use and notifications within the Terminal.

## Installation

```bash
copilot plugin install ./wta/agent-hooks-plugin
```

## Configuration

To enable event logging to the console:

```bash
export WTA_LOG_AGENT_EVENT=1
```

Then launch WTA with the environment variable set.

## Supported CLIs

- **Copilot CLI** — Fully supported
- **Claude Code** — Fully supported
- **Gemini CLI** — Planned

## Requirements

- `wtcli` on PATH (automatic inside Windows Terminal package)

## Events

The plugin registers the following lifecycle events:

- **PreToolUse** — Fired before a tool is invoked
- **PostToolUse** — Fired after tool execution completes
- **Notification** — General notifications from the agent
- **Stop** — Session termination (agent stop event)
- **SubagentStop** — Sub-agent termination
