# wt-agent-hooks (Gemini Extension)

Forward Gemini CLI hook events to Windows Terminal for WTA display.

This is one of three CLI-specific bundles under `wta/wt-agent-hooks/` —
see the [top-level README](../README.md) for the full picture and how
the auto-installer in `wta` consumes this folder.

## Installation

The `wta` binary installs this extension automatically on startup. For
manual install (e.g. fresh-clone testing):

```bash
gemini extensions install <repo>\wta\wt-agent-hooks\gemini-extension
```

Gemini copies the extension into `~/.gemini/extensions/wt-agent-hooks/`.

## Verify

```bash
gemini extensions list
```

You should see `wt-agent-hooks` with its hooks active.

## Events Forwarded

| Gemini event   | wta event topic       |
| :------------- | :-------------------- |
| `SessionStart` | `agent.session.start` |
| `SessionEnd`   | `agent.session.end`   |
| `BeforeAgent`  | `agent.prompt.submit` |
| `BeforeTool`   | `agent.tool.starting` |
| `AfterTool`    | `agent.tool.finished` |
| `AfterAgent`   | `agent.stop`          |
| `Notification` | `agent.notification`  |

Gemini does not have native equivalents for tool-failure
(`PostToolUseFailure`), session-error (`StopFailure`), or sub-agent stop
(`SubagentStop`), so those WTA topics never fire from Gemini. The
Claude / Copilot bundles cover those events.

## Requirements

- Running inside a Windows Terminal pane (the script no-ops outside).
- `wtcli` on PATH (automatic inside the deployed Windows Terminal package).

## Notes

- This extension uses `${extensionPath}` (Gemini's variable). The
  Claude/Copilot bundles use `${CLAUDE_PLUGIN_ROOT}` instead — both
  resolve to the plugin's installed root directory.
- Event names differ from Claude/Copilot (`BeforeTool`/`AfterTool` vs.
  `PreToolUse`/`PostToolUse`); see the [Gemini hooks reference][hooks-ref].

[hooks-ref]: https://github.com/google-gemini/gemini-cli/blob/main/docs/hooks/reference.md

