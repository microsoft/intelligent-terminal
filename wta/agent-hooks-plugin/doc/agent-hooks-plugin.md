# Agent Hooks Plugin

The `wt-agent-hooks` Copilot CLI plugin forwards agent lifecycle events to
Windows Terminal (WTA) via `wtcli send-event`. This lets the WTA agent pane
display real-time tool use, prompts, and session events from any Copilot CLI
session running in another pane.

## How It Works

```
Copilot CLI ─── hook fires ──▶ send-event.ps1 ──▶ wtcli send-event ──▶ WTA
              (stdin JSON)     (wraps payload)      (COM protocol)     (displays)
```

1. Copilot CLI triggers hooks at lifecycle points (tool use, prompt, session start/end).
2. The plugin's `send-event.ps1` reads the hook JSON from stdin.
3. It wraps the payload as `{cli_source: "copilot", payload: <hook_data>}` and
   calls `wtcli send-event -e <event_type> <json>`.
4. WTA receives the event and displays it in the agent pane (if enabled).

## Installation

```powershell
# Install from local path (use forward slashes)
copilot plugin install "./wta/agent-hooks-plugin"
```

The plugin is installed to `~/.copilot/installed-plugins/_direct/agent-hooks-plugin/`.

## Event Types

| Copilot CLI Hook   | WTA Event Type        | Description                    |
|--------------------|-----------------------|--------------------------------|
| `SessionStart`     | `agent.session.start` | Copilot session begins         |
| `SessionEnd`       | `agent.session.end`   | Copilot session ends           |
| `UserPromptSubmit` | `agent.prompt.submit` | User submits a prompt          |
| `PreToolUse`       | `agent.tool.starting` | Tool call about to execute     |
| `PostToolUse`      | `agent.tool.finished` | Tool call completed            |
| `PostToolUseFailure`| `agent.tool.failed`  | Tool call failed               |
| `ErrorOccurred`    | `agent.error`         | Error during agent operation   |
| `Stop`             | `agent.stop`          | Agent stops (end_turn, etc.)   |
| `SubagentStop`     | `agent.subagent.stop` | Sub-agent stops                |

## Environment Variables

### Required (set automatically by Windows Terminal)

| Variable        | Description                                              |
|-----------------|----------------------------------------------------------|
| `WT_COM_CLSID`  | COM class ID for the WT Protocol server. Set by Windows Terminal in each pane. If not set, hooks exit silently (not running inside WT). |

### Required (must be on PATH)

| Binary   | Description                                                |
|----------|------------------------------------------------------------|
| `wtcli`  | Windows Terminal CLI client. Must be on PATH for hooks to send events. If not found, hooks exit silently. |

### Optional

| Variable              | Description                                           |
|-----------------------|-------------------------------------------------------|
| `WTA_LOG_AGENT_EVENT` | Set to `1` to enable agent event display in WTA. When unset, WTA silently ignores agent events. |

## Per-Repo Hooks (Alternative)

The plugin sends events from **any directory** where Copilot CLI runs. For
per-repo hooks (only active in a specific project), use the `.github/hooks/`
approach instead. See `wta/agent-hooks/` for a working example that combines
file logging with `wtcli send-event`.

Key difference: per-repo hooks use `.github/hooks/hooks.json` at the **git
root** (not a subdirectory). Copilot CLI discovers hooks relative to the git
root of the working directory.

## Troubleshooting

**Hooks not firing?**
- Check Copilot CLI logs: `~/.copilot/logs/process-*.log`
- Search for `"hook"` or `"Invalid"` in the latest log
- Verify the plugin loaded: look for `"Loaded N hook(s) from 2 plugin(s)"`

**Events not showing in WTA?**
- Ensure `WTA_LOG_AGENT_EVENT=1` is set in the WTA process environment
- Check `%TEMP%\wta-event-diag.log` for `agent_event` entries
- Verify `wtcli` is on PATH inside the pane where Copilot CLI runs

**"Invalid JSON" from wtcli?**
- The plugin uses `ProcessStartInfo` to bypass PowerShell's native command
  argument mangling. If you modify `send-event.ps1`, avoid passing JSON
  directly as a PowerShell native command argument — use `ProcessStartInfo`
  with escaped quotes instead.

**Plugin hooks don't work in WTA agent pane (ACP mode)?**
- WTA launches agents via `copilot --acp --stdio` (Agent Control Protocol).
  ACP mode does **not** trigger CLI plugin hooks. The plugin only works for
  interactive Copilot CLI sessions running in regular terminal panes.
