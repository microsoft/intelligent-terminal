# wt-agent-hooks

Static plugin/extension bundle that forwards CLI agent lifecycle events from
**Claude Code**, **Copilot CLI**, **Codex CLI**, **Gemini CLI**, and **OpenCode**
to Windows Terminal (WTA)
via `wtcli send-event`. This lets the WTA agent pane display real-time tool
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
│       └── hooks/
│           ├── hooks.json                  # 10 events, -CliSource claude
│           └── send-event.ps1
├── copilot/                                # passed to `copilot plugin marketplace add`
│   └── (identical layout to claude/, only -CliSource differs)
├── gemini-extension/                       # passed to `gemini extensions install`
│   ├── gemini-extension.json
│   └── hooks/
│       ├── hooks.json                      # 7 events, -CliSource gemini
│       └── send-event.ps1
├── opencode/                                # copied to OpenCode's global plugins dir
│   ├── plugin.json                          # managed bundle version
│   ├── wt-agent-hooks.js                    # OpenCode V1 plugin
│   └── send-event.ps1
└── hook-debug/                             # dev utility, not part of the install bundle
    └── state-logger.ps1
```

`send-event.ps1` is byte-identical across all hook subtrees (single source
of truth — a unit test in `tools/wta/src/agent_hooks_installer.rs` enforces this).
Claude and Copilot share the same plugin manifest and `hooks.json` schema
modulo the `-CliSource <name>` token; another unit test enforces parity
between the two so they can never drift.

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
otherwise. It keeps its manifest and bridge in a dedicated `wt-agent-hooks\`
support subdirectory and refuses to overwrite a same-name JavaScript plugin
that does not contain Intelligent Terminal's managed-file marker.

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

## Bridge script

```
Agent CLI ─── hook fires ──▶ send-event.ps1 ──▶ wtcli send-event ──▶ WTA
            (stdin JSON)     (wraps payload)     (COM protocol)
```

`send-event.ps1` reads the hook JSON from stdin, wraps it as
`{cli_source: <claude|copilot|gemini|opencode>, agent_session_id: <sid>, payload: <hook_data>}`,
and calls `wtcli send-event -e <event_type> <json>`. The `cli_source` field
is hard-coded per-CLI via the `-CliSource <name>` argument in each
`hooks.json` — env-var heuristics are unreliable because Copilot CLI
inherits Claude's plugin shape and sets `CLAUDE_PLUGIN_ROOT`, making it
indistinguishable from a real Claude run by env vars alone.

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
| Per-invocation script trace      | `%LOCALAPPDATA%\IntelligentTerminal\logs\hook-trace.log` — one line per `send-event.ps1` invocation, all CLIs. |
| Events not reaching WTA          | `%LOCALAPPDATA%\IntelligentTerminal\logs\wta-ensure-host.log` — search for `agent_event`.   |
| Wrong `cli_source` reported      | Check `hooks.json` in the installed plugin folder — every command must end with `-CliSource <name>`. |

## Why two-level `claude/wt-agent-hooks/` nesting?

Claude/Copilot's `marketplace add` reads `<source>/.claude-plugin/marketplace.json`,
which declares `"source": "./wt-agent-hooks"`. The CLI then copies
`<source>/wt-agent-hooks/` (the plugin folder) into the user's writable plugin
directory. So the on-disk shape mirrors what the CLI expects: an outer marketplace
folder that points at an inner plugin folder by relative path. Gemini has no
marketplace concept and reads the extension folder directly.

## Caveats

- **Copilot ACP mode bypasses plugin hooks.** WTA launches Copilot via
  `copilot --acp --stdio`; ACP mode does not trigger CLI plugin hooks. The
  plugin only works for interactive Copilot CLI sessions running in regular
  terminal panes. Claude and Gemini hooks **do** fire under WTA agent pane
  (interactive mode), so this caveat is Copilot-specific.
- **OpenCode ACP sessions are intentionally ignored by the plugin.** The
  plugin requires both `WT_COM_CLSID` and `WT_SESSION`; the shared ACP process
  used by the agent pane is already tracked through ACP and must not create a
  duplicate hook-backed row.
- **MSIX install paths include the package version.** They change on every
  upgrade, which is why `agent_hooks_installer` re-runs marketplace
  registration on every wta startup and strips stale entries before
  reinstalling.
