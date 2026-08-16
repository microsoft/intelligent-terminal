# wt-agent-hooks

Static plugin/extension bundle that forwards CLI agent lifecycle events from
**Claude Code**, **Copilot CLI**, **Codex CLI**, **Gemini CLI**, and **OpenCode**
to Windows Terminal (WTA)
via `wtcli`. This lets the WTA agent pane display real-time tool
use, prompts, and session events from any agent CLI session running in another
pane.

## Layout

This directory is the **single source of truth** for everything WTA installs
into the supported CLIs. Each CLI gets its own self-contained subtree that is
passed verbatim to that CLI's marketplace / extensions command:

```
wt-agent-hooks/
├── claude/                                 # passed to `claude plugin marketplace add`
│   ├── .claude-plugin/marketplace.json
│   └── wt-agent-hooks/                     # the plugin folder Claude copies into ~/.claude/
│       ├── .claude-plugin/plugin.json
│       └── hooks/hooks.json                # native wtcli agent-hook commands
├── copilot/                                # passed to `copilot plugin marketplace add`
│   ├── .github/plugin/marketplace.json      # Copilot-native marketplace
│   └── wt-agent-hooks/
│       ├── plugin.json                      # Copilot-native root manifest
│       └── hooks/hooks.json                 # --cli-source copilot
├── gemini-extension/                       # passed to `gemini extensions install`
│   ├── gemini-extension.json
│   └── hooks/hooks.json                    # 7 native hook commands
├── codex/                                  # passed to `codex plugin marketplace add`
│   └── wt-agent-hooks/hooks/hooks.json     # native hook commands
├── opencode/                                # copied to OpenCode's global plugins dir
│   ├── plugin.json                          # managed bundle version
│   └── wt-agent-hooks.js                    # OpenCode V1 plugin
└── hook-debug/                             # dev utility, not part of the install bundle
    └── state-logger.ps1
```

Every integration dispatches through the native `wtcli agent-hook` command,
invoked directly from `hooks.json` with no script or batch launcher in between.
Claude and Copilot share the same plugin manifest and event schema.

## How install works

The `wta` binary auto-installs each CLI on startup via
`agent_hooks_installer::ensure_installed()`:

```
              wta startup
                   │
   ┌───────────────┼───────────────┐
   ▼               ▼               ▼
install_for_  install_for_  install_for_
  claude       copilot        gemini
   │               │               │
resolve         resolve         resolve
claude/         copilot/        gemini-extension/
   │               │               │
   ▼               ▼               ▼
 claude          copilot         gemini
 plugin          plugin          extensions
 marketplace     marketplace     install
 add ...         add ...         <bundle>
   │               │
   ▼               ▼
 claude          copilot
 plugin          plugin
 install         install
 wt-agent-hooks  wt-agent-hooks
 @wt-local       @wt-local
```

OpenCode has no separate hook marketplace. `wta hooks install --cli opencode`
copies `wt-agent-hooks.js` into `%XDG_CONFIG_HOME%\opencode\plugins\` when
`XDG_CONFIG_HOME` is set, or `%USERPROFILE%\.config\opencode\plugins\`
otherwise. It keeps its ownership/version manifest in a dedicated
`wt-agent-hooks\` support subdirectory and refuses to overwrite a same-name
JavaScript plugin that does not contain Intelligent Terminal's managed-file
marker.

Bundle resolution chain (first hit wins, see
`agent_hooks_installer::bundle::candidate_roots`):

1. `WTA_HOOKS_BUNDLE_DIR` env var — explicit override (highest priority).
2. `<dir-of-current-exe>/wt-agent-hooks/` — where MSIX deposits the bundle
   next to `wta.exe` (configured by `CascadiaPackage.wapproj`'s Content glob).
3. Walk parents of `current_exe()` looking for `tools/wta/wt-agent-hooks/` —
   dev-tree fallback.
4. Materialize the embedded `include_str!` blobs into
   `%LOCALAPPDATA%\IntelligentTerminal\hook-bundle-fallback\<cli>\` —
   last-resort safety net for "MSIX layout changed and we forgot to update
   `candidate_roots`".

## Event vocabulary

WTA normalises hook events from all supported CLIs into a single set of topic
names. Event vocabularies differ per CLI:

| WTA event topic         | Claude Code            | Copilot CLI            | Gemini CLI       |
| ----------------------- | ---------------------- | ---------------------- | ---------------- |
| `agent.session.start`   | `SessionStart`         | `SessionStart`         | `SessionStart`   |
| `agent.session.end`     | `SessionEnd`           | `SessionEnd`           | `SessionEnd`     |
| `agent.notification`    | `Notification`         | `Notification`         | `Notification`   |
| `agent.prompt.submit`   | `UserPromptSubmit`     | `UserPromptSubmit`     | `BeforeAgent`    |
| `agent.tool.starting`   | `PreToolUse`           | `PreToolUse`           | `BeforeTool`     |
| `agent.tool.finished`   | `PostToolUse`          | `PostToolUse`          | `AfterTool`      |
| `agent.tool.failed`     | `PostToolUseFailure`   | `PostToolUseFailure`   | *(not emitted)*  |
| `agent.error`           | `StopFailure`          | `StopFailure`          | *(not emitted)*  |
| `agent.stop`            | `Stop`                 | `Stop`                 | `AfterAgent`     |
| `agent.subagent.stop`   | `SubagentStop`         | `SubagentStop`         | *(not emitted)*  |

All event names are validated against each CLI's documented hook catalog.
`StopFailure` is the Claude-documented name for "turn ended due to API
error" — earlier wta builds shipped an undocumented `ErrorOccurred` name
which is no longer used. Gemini's manifest has no native equivalents for
the failure topics, so those rows are silent on Gemini.

OpenCode uses its V1 plugin API rather than a hook manifest. The plugin maps
`session.created/updated`, `chat.message`, `tool.execute.before/after`,
`permission.*`, `question.*`, `session.idle/error/deleted`, and `dispose` to
the same WTA topics. Child sessions with `parentID` are ignored so OpenCode's
internal subagents do not create extra rows.

References:
- Claude: <https://docs.claude.com/en/docs/claude-code/hooks>
- Gemini: <https://github.com/google-gemini/gemini-cli/blob/main/docs/hooks/reference.md>
- OpenCode: <https://opencode.ai/docs/plugins/>

## Hook bridge

```
Agent CLI ─── hook fires ──▶ wtcli agent-hook ──▶ WTA
                             (stdin JSON + COM)
```

The native bridge reads the hook JSON from stdin and wraps it as
`{cli_source: <claude|codex|copilot|gemini|opencode>, agent_session_id: <sid>, payload: <hook_data>}`,
then publish an `agent_event` through Terminal's COM protocol. The `cli_source`
field is hard-coded per CLI in `hooks.json`; env-var heuristics are unreliable
because Copilot CLI inherits Claude's plugin shape and sets
`CLAUDE_PLUGIN_ROOT`, making it indistinguishable from a real Claude run.

`wtcli agent-hook` requires `WT_COM_CLSID` and `WT_SESSION`, writes nothing, and
always exits successfully. The shared ACP process has no `WT_SESSION`, so its
redundant hooks are dropped and cannot be incorrectly attributed to the active
shell pane.

**The command must stay shell-agnostic.** Each CLI decides for itself which
shell interprets the `command` string. That choice is undocumented, differs per
CLI, and we guessed it wrong twice — so the bundle assumes nothing and ships one
spelling that survives all of them:

```
wtcli.exe agent-hook --cli-source <cli> --event <topic>
```

A bare executable name with plain arguments: nothing for PowerShell to read as
an expression, no `cmd.exe` metacharacters, and nothing bash rewrites.

| CLI | Hook shell | How we know |
| --- | --- | --- |
| Copilot | PowerShell 7+ | GitHub hooks documentation |
| Codex | PowerShell | sandbox log dispatches every command as `pwsh.exe -NoProfile -Command` |
| Gemini | PowerShell | `hookRunner.ts` → `getShellConfiguration()` resolves ComSpec-powershell → `pwsh.exe` → `powershell.exe`, with no `cmd.exe` branch |
| Claude | **bash** (`/usr/bin/bash`) | its own debug log reports `/usr/bin/bash: line 1: …` |

Spellings that were tried and failed, each in a shell that had not been
considered at the time:

| Spelling | Fails in | Why |
| --- | --- | --- |
| `"<path>/agent-hook.cmd" …` | PowerShell | a leading quoted string is an expression, so the words after it are a parse error |
| `& "<path>/agent-hook.cmd" …` | `cmd.exe` | `&` is a command separator with nothing before it — and this one still *parses* in PowerShell, which is what made it look correct |
| `cmd /c "wtcli.exe … & exit 0"` | **bash** | MSYS path conversion rewrites `/c`, so `cmd.exe` launches interactively, prints its banner, echoes the hook payload, and never runs the bridge — while still exiting 0, so the CLI reports the hook as successful |

That last row is why `agent_hooks_installer_tests` executes every shipped
command under PowerShell, `cmd.exe`, **and** bash rather than reasoning about
which shell each CLI uses. Exit status alone is not enough evidence that a hook
worked.

> **Known gap.** Because the command invokes `wtcli.exe` off `PATH` with no
> wrapper, uninstalling Intelligent Terminal while hook config remains
> registered leaves each hook failing with "not recognized" (exit 1). Copilot's
> `PreToolUse` hook is fail-closed, so that would deny its tool calls. Every
> wrapper that fixes this broke at least one shell, so the resilience needs a
> different mechanism — tracked separately.

OpenCode needs none of this: its plugin spawns `wtcli.exe` through an argv array
rather than a shell string, already gates on `WT_COM_CLSID` / `WT_SESSION`,
ignores both output streams, and wraps the spawn in `try`/`catch`.

## Manual install (for testing without `wta` startup)

The auto-installer in `wta` is the supported path. For ad-hoc testing
against a freshly cloned repo:

```powershell
# Claude
claude plugin marketplace add .\wta\wt-agent-hooks\claude
claude plugin install wt-agent-hooks@wt-local

# Copilot
copilot plugin marketplace add .\wta\wt-agent-hooks\copilot
copilot plugin install wt-agent-hooks@wt-local

# Gemini
gemini extensions install .\wta\wt-agent-hooks\gemini-extension

# OpenCode (managed copy into ~/.config/opencode/plugins)
wta hooks install --cli opencode
```

## Troubleshooting

| Symptom                          | Where to look                                                                               |
| -------------------------------- | ------------------------------------------------------------------------------------------- |
| Hooks not firing (Claude)        | `~/.claude/logs/*.log` (or `claude --debug`); search for `hook` / `wt-agent-hooks`.         |
| Hooks not firing (Copilot)       | `~/.copilot/logs/process-*.log`; verify `Loaded N hook(s) from M plugin(s)`.                |
| Hooks not firing (Gemini)        | `~/.gemini/logs/*.log` and `gemini extensions list`.                                        |
| Hooks not firing (OpenCode)      | Verify `~/.config/opencode/plugins/wt-agent-hooks.js` contains the managed-file marker.      |
| Events not reaching WTA          | `%LOCALAPPDATA%\IntelligentTerminal\logs\wta-ensure-host.log` — search for `agent_event`.   |
| Wrong `cli_source` reported      | Check `hooks.json` in the installed plugin folder — every command must contain `--cli-source <name>`. |

## Marketplace layouts

Claude uses its native `.claude-plugin/marketplace.json` and
`.claude-plugin/plugin.json` sentinels. Copilot uses its native
`.github/plugin/marketplace.json` location and a root-level `plugin.json`.
Both marketplaces declare `"source": "./wt-agent-hooks"`, so each CLI copies
the self-contained inner plugin folder into its writable plugin directory.
Gemini has no marketplace concept and reads the extension folder directly.

## Caveats

- **ACP modes may invoke plugin hooks.** `wtcli agent-hook` ignores invocations
  without `WT_SESSION`, including WTA's shared ACP processes. Agent-pane
  sessions are already tracked through ACP; only interactive CLI sessions in
  regular terminal panes produce hook-backed rows.
- **OpenCode ACP sessions are intentionally ignored by the plugin.** The
  plugin requires both `WT_COM_CLSID` and `WT_SESSION`; the shared ACP process
  used by the agent pane is already tracked through ACP and must not create a
  duplicate hook-backed row.
- **MSIX install paths include the package version.** They change on every
  upgrade, which is why `agent_hooks_installer` re-runs marketplace
  registration on every wta startup and strips stale entries before
  reinstalling.
- **Codex must re-trust the 0.1.5 commands once.** Codex hashes each hook
  command for trust, so replacing the PowerShell command with
  `wtcli agent-hook` requires reviewing the updated plugin through `/hooks`.
