# wt-agent-hooks

Static plugin/extension bundle that forwards CLI agent lifecycle events from
**Claude Code**, **Copilot CLI**, **Codex CLI**, **Gemini CLI**, **OpenCode**, and **Antigravity**
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
│       └── hooks/hooks.json                 # --cli-source copilot, per-shell fields
├── gemini-extension/                       # passed to `gemini extensions install`
│   ├── gemini-extension.json
│   └── hooks/hooks.json                    # 7 native hook commands
├── codex/                                  # passed to `codex plugin marketplace add`
│   └── wt-agent-hooks/hooks/hooks.json     # native hook commands
├── opencode/                                # copied to OpenCode's global plugins dir
│   ├── plugin.json                          # managed bundle version
│   └── wt-agent-hooks.js                    # OpenCode V1/V2 plugin
└── hook-debug/                             # dev utility, not part of the install bundle
    └── state-logger.ps1
```

Every integration dispatches through the native `wtcli agent-hook` command,
invoked directly from `hooks.json` with no script or batch launcher in between.
Claude and Copilot share the same non-version plugin metadata and event schema.

Hook versions are independent per provider. Bump only the manifest for the
provider whose installed payload changes; for Claude and Copilot, also update
that provider's marketplace plugin version. Keep the already-shipped versions
of unchanged providers: do not roll them back to undo a coordinated release.
Update only that provider's exact expected release version in the version-policy
test with its manifest; unrelated providers keep their own expected versions.
Reconciliation compares each provider's installed version against its own
bundle version. A JavaScript-only OpenCode change still needs a version bump
because its installed plugin is a copy, not a live link to the bundle.

## How install works

The first-run setup installs hooks for the agent selected in FRE. Afterward,
automatic reconciliation runs when session management is enabled at
`wta-master` startup, when the setting changes from off to on, and when the
selected built-in agent changes. Manual repair remains available through
`wta hooks install`. All automatic paths end up in
`agent_hooks_installer::reconcile_agent_hooks()`, which dispatches per CLI:

```
           wta hooks install
                   │
             status + plan
        ┌──────────┼──────────┐
        ▼          ▼          ▼
       skip    install_one  upgrade_one_cli
                   │          │
                   └────┬─────┘
                        ▼
          native install / update / reinstall
```

Reconciliation installs a missing bridge for every detected CLI in scope and
uses each CLI's own update command for an existing stale bridge, because a
second `install` is a no-op once the plugin is registered.
`wta hooks install --force` bypasses this plan and reruns the first-install
flow as a manual recovery path when status cannot observe the real problem.

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

### Antigravity CLI

The `antigravity` subtree contains a native `plugin.json`, `hooks.json`, and a
separate ownership/version marker. `wta hooks install --cli antigravity` invokes
`agy plugin install` and verifies the copied files under
`~/.gemini/config/plugins/wt-agent-hooks`; it commits the version marker last.
The status reader combines `import_manifest.json` and the plugin's enablement in
`config.json`. Existing `local-install` registrations are also recognized when
the plugin has an Intelligent Terminal ownership marker or managed descriptor.
These legacy plugins are uninstalled through `agy` before reinstalling, because
native install otherwise retains the old entry alongside its new registration.
The same migration repairs a partial reinstall with one entry from each source;
duplicate entries from the same source or foreign sources are rejected.
Ordinary upgrades use native install directly. Uninstall checks
ownership and invokes the native plugin manager, then verifies removal. If the
native CLI is absent, cleanup fails explicitly without rewriting shared JSON or
discarding managed files. This does not claim a cross-provider filesystem transaction.

`PreInvocation` maps to `agent.prompt.submit`, `PreToolUse` to
`agent.tool.starting`, and `Stop` to `agent.stop` only when `fullyIdle` is true;
provider errors become `agent.error`. The native bridge projects `conversationId`
and the first `workspacePaths` entry onto the shared session/cwd fields and drops
transcript, model, artifact and injected-content metadata. Only callbacks with a
CLI transcript are accepted, because the upstream plugin directory is also shared
by other Antigravity frontends. The ordinary `WT_SESSION`/COM gate still suppresses
shared ACP processes.

Antigravity's Windows hook executor splits the command into argv without stripping
quote characters. Keep the script after PowerShell's `-Command` **unquoted**:
PowerShell joins those arguments into a script. Quoting the entire script instead
makes PowerShell evaluate a string literal and print the script rather than execute
it. This uses the same readable `try`/`catch` guard as the Copilot, Codex, and Gemini
hooks, with no Base64 or additional launcher:

```text
powershell.exe -NoLogo -NoProfile -NonInteractive -Command try { wtcli.exe agent-hook --cli-source antigravity --event agent.prompt.submit } catch { }; [Console]::Out.WriteLine('{}'); exit 0
```

Antigravity parses hook responses as JSON even though `wtcli agent-hook` itself is
silent. `PreInvocation` and `Stop` return `{}` without injecting steps or requesting
that the execution loop continue. `PreToolUse` requires a permission decision:
return `{"decision":"ask"}` so the provider retains normal approval and existing
"Always Allow" grants. An empty response denies tools, while `"allow"` bypasses
their approval. Do not return either for `PreToolUse`, including when `wtcli.exe`
is unavailable.

The executable bundle tests reproduce the provider's Windows argv dispatch, rather
than incorrectly treating the command as a PowerShell or Bash shell expression.
The `antigravity-wsl` bundle uses a Bash launch guard and single-quotes the readable
PowerShell script so Bash preserves its inner double quotes.
Child-scoped `WSLENV` propagation preserves the distro and actual hook cwd for
callbacks without mounted-workspace metadata.
Its guard returns the same event-specific response even outside Terminal or when
Windows interop is unavailable. The bridge never automatically grants provider
tool permissions. WSL plugins are installed separately
with the in-distro `agy plugin install`, not by host-profile reconciliation.
The bridge uses that source metadata, or queries only the exact pane's shell
metadata (no terminal text), so WSL sessions keep their distro even when a
delegated CLI starts without an interactive shell prompt. No dedicated session-start/end or notification hook
exists in the exercised API; shell/pane lifecycle supplies exit detection.

The ordinary CLI and native ACP server use different default history stores.
Do not treat an ACP session ID as a CLI `--conversation` ID or claim cold-start
CLI history enumeration from ACP `session/list`.

WTA normalises hook events from all supported CLIs into a single set of topic
names. Event vocabularies differ per CLI:

| WTA event topic         | Claude Code            | Copilot CLI            | Gemini CLI       |
| ----------------------- | ---------------------- | ---------------------- | ---------------- |
| `agent.session.start`   | `SessionStart`         | `SessionStart`         | `SessionStart`   |
| `agent.session.end`     | `SessionEnd`           | `SessionEnd`           | `SessionEnd`     |
| `agent.notification`    | `Notification`         | `Notification`         | `Notification`   |
| `agent.prompt.submit`   | `UserPromptSubmit`     | `UserPromptSubmit`     | `BeforeAgent`    |
| `agent.tool.starting`   | *(not subscribed)*     | *(not subscribed)*     | `BeforeTool`     |
| `agent.error`           | `StopFailure`          | `StopFailure`          | *(not emitted)*  |
| `agent.stop`            | `Stop`                 | `Stop`                 | `AfterAgent`     |
| `agent.subagent.stop`   | *(not subscribed)*     | *(not subscribed)*     | *(not emitted)*  |

All event names are validated against each CLI's documented hook catalog.
`StopFailure` is the Claude-documented name for "turn ended due to API
error" — earlier wta builds shipped an undocumented `ErrorOccurred` name
which is no longer used. Gemini's manifest has no native equivalents for
the failure topics, so those rows are silent on Gemini.

Copilot's `shell_completed` notification reports background command completion,
not a request for input. WTA recognises both `notificationType` and
`notification_type` for this notification and leaves the session status unchanged.
It does not clear an existing approval/question wait or add approval-completion
tracking; other notification and lifecycle behavior remains unchanged.

Two topics keep a routing arm in `app.rs` that outlives their subscription in
the Claude and Copilot bundles: `agent.subagent.stop`, which no shipped manifest
claims at all, and `agent.tool.starting`, which Gemini still sends as
`BeforeTool` in the table above and OpenCode still sends through the plugin API
described below. The arms stay so a CLI that adds the event later, or a bundle a
user installed earlier, is routed rather than mis-handled.

**Tool-completion events are deliberately not subscribed.** `app.rs` discards
`agent.tool.finished` / `agent.tool.failed`: tool completion does not end a
turn, so `agent.stop` owns the transition back to Idle. Subscribing them cost a
shell spawn plus a COM round trip *per tool call*, only to be dropped — so
Copilot's `PostToolUse` / `PostToolUseFailure`, Gemini's `AfterTool`, and
OpenCode's `tool.execute.after` were removed. The routing arm remains so an
older installed bundle that still emits them is ignored rather than mis-routed.

**Copilot's `PreToolUse` went the same way, one release later** — note this is
narrower than the rule above: `agent.tool.starting` is still subscribed by
Gemini (`BeforeTool`) and OpenCode (`tool.execute.before`), and `app.rs` still
routes it. Copilot kept it for the Attention path `app.rs` synthesizes when
`tool_name` names a user-input tool, which was the only signal available while
Copilot's `Notification` hook did not cover questions. It does now: on 1.0.81-2
a single `AskUserQuestion` produces both the tool hook and a `Notification`,
~0.9 s apart, each carrying the question text. Measured cost of the duplicate
was ~536 ms per tool call — ~388 ms of it `pwsh` startup — on a **fail-closed**
hook, so the CLI blocks on it. Turn-level Working is unaffected because
`UserPromptSubmit` already maps to `ToolStarting`; what is given up is the
tool's name in the session row, not its status.

OpenCode uses a plugin definition with a V1 `server()` entrypoint and a V2
`setup()` entrypoint rather than a hook manifest. Both forward prompt, tool,
session lifecycle, and permission/question events to the same WTA topics.
Child sessions with `parentID` are ignored so OpenCode's internal subagents
do not create extra rows.
OpenCode's explicit session titles reach both the helper and master registry;
rename events replace the previous title, while prompts without a title retain
it. Other providers keep their existing title/history behavior.

The V1 server path has been checked with OpenCode 1.17.20 and 1.18.16; older
loaders that accept only function exports are not supported. The V2 entrypoint
targets the `id`/`setup()` plugin API; the original loading report used
OpenCode 2.0.22. Automated real-server evidence records the CLI version actually
tested and does not imply V2 acceptance when run with a V1 CLI. Entry selection
belongs to OpenCode's loader, not a version probe inside this plugin.
Hook attribution uses the server process's inherited
`WT_SESSION`; location filtering does not establish ownership for multiple
Terminal clients sharing one external server.

Run the dependency-free Windows component regressions from the repository root:

```powershell
node --test test\e2e\selftests\OpenCodeHooks.test.mjs
```

These execute both entrypoints with controlled SDK inputs and capture bridge
requests. The packaged delivery tests are documented in `test\e2e\README.md`;
controlled SDK inputs do not substitute for validation of a real V2 loader.

OpenCode's `question` tool is recognized as a user-input tool by both the native
hook bridge and WTA. Its `agent.tool.starting` event and the `agent.notification`
from `question.asked` both leave the session in Attention (Waiting for input),
regardless of their arrival order. `question.replied` maps to
`agent.prompt.submit`, returning the session to Working (Active).

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

### Known limitation: Codex managed daemon pane identity

Codex's shared managed app-server can execute hooks for a new CLI session using
the environment inherited when the daemon first started. If that was another
tab, `wtcli agent-hook` receives the old tab's `WT_SESSION` even though the
hook payload contains the new session ID and correct cwd. Terminal cannot bind
the event to the new pane; if the old pane is closed, explicit pane lookup fails
with `0x80070490`. This is distinct from the shared WTA ACP process, which has
no pane identity and whose redundant hooks are dropped.

Reproduced with Codex 0.162.1 on Windows: a new tab's hooks kept reporting the
previous pane. Starting the CLI in that same tab with `codex --no-daemon`
reported its actual pane, bound the session, and updated Rich Tab. This is a
per-invocation workaround, not a change to the user's default Codex command.
Do not substitute the focused pane or relax pane ownership checks: a durable
fix needs authoritative per-client pane identity from Codex. Track the
upstream issue at <https://github.com/openai/codex/issues/48500>. Copilot and
Claude bound their tested sessions correctly; Gemini has not had the same
cross-tab test.

### Gemini CLI cwd semantics

Gemini CLI has no built-in command to switch the current process cwd within
an interactive session. `/directory add` only adds workspace directories; it
does not change cwd and must not be treated as an agent cwd update. To run
Gemini from another cwd, exit, change the shell directory, and start Gemini
again; that tests a new session rather than an in-session cwd transition.
Gemini's existing `BeforeAgent` hook includes `cwd` and already serves prompt
activity; WTA reuses that field to correct a known session's recorded cwd on
the next prompt. No separate cwd-change hook or extra hook invocation is
needed. Cross-tab Gemini pane attribution has not yet been verified.

**A shared `command` field must stay shell-agnostic.** Each CLI decides for
itself which shell interprets that string. For hooks with a documented
platform-specific override, use that override instead:

```
wtcli.exe agent-hook --cli-source <cli> --event <topic>
```

A bare executable name with plain arguments: nothing for PowerShell to read as
an expression, no `cmd.exe` metacharacters, and nothing bash rewrites.

| CLI | Hook shell | How we know |
| --- | --- | --- |
| Copilot | PowerShell 7+ | GitHub hooks documentation |
| Codex | Session-selected shell | `commandWindows` selects the text, not the shell; Windows sessions may run hooks through PowerShell or cmd.exe |
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

### Surviving an Intelligent Terminal uninstall

`wtcli.exe` reaches `PATH` through the MSIX app-execution alias, which uninstall
deletes — but the hook config stays registered with each CLI. From that moment
the *shell*, not the bridge, decides the hook's exit code, and a missing command
makes it 1. Copilot's `preToolUse` hook is fail-closed, so every tool call in
every later session is denied:

```
✗ Echo a probe marker string (shell)
  └ Denied by preToolUse hook from "…" (hook errored)
```

The fix does not require guessing a shell after all. Copilot's command hooks
take **`powershell` and `bash` fields**, and measurement shows the field name
*selects* the interpreter rather than merely matching a fixed one: with only
`bash` present the events still arrive on Windows, which could not happen if
Copilot always used PowerShell — `command -v` short-circuits there and the
bridge would never run. Each spelling therefore only has to be valid in the one
shell that names it:

| Field | Command |
| --- | --- |
| `powershell` | `try { wtcli.exe agent-hook --cli-source copilot --event <topic> } catch { }; exit 0` |
| `bash` | `command -v wtcli.exe >/dev/null 2>&1 && wtcli.exe agent-hook --cli-source copilot --event <topic>; exit 0` |
| `command` | **not shipped** — see below |

`try`/`catch` is what makes the PowerShell form work: a missing native command
raises a catchable `CommandNotFoundException`, and the trailing `exit 0`
overrides a non-zero exit from the bridge itself. Both keep passing the hook
JSON on stdin, since PowerShell hands its own stdin to the native child.

### Why Copilot ships no `command` at all

A portable `command` cannot be guarded: no single spelling both runs in every
shell and survives a missing bridge (the `cmd /c "… & exit 0"` form is
self-guarding but is destroyed by bash's MSYS path conversion, which rewrites
`/c` into a Windows path so `cmd.exe` starts interactively and never runs the
bridge — and that happens whether or not the bridge exists). Shipping an
unguarded fallback next to a **fail-closed** `preToolUse` hook leaves a path
that only has to be taken once to deny every tool call for the rest of a
session.

Measured on Copilot CLI 1.0.81-0, one field at a time, with delivery as the
oracle:

| Fields present | Events delivered | Tool calls denied |
| --- | --- | --- |
| `powershell` only | 5 | no |
| `bash` only | 5 | no |
| `command` only | 5 | no |
| none | **0** | **no** |

The last row is the one that decides it: a handler with nothing runnable is a
silent no-op, not an error. So dropping `command` costs nothing today — the
per-shell fields deliver exactly as before — and if a future Copilot ever stops
honouring those fields, the hook degrades fail-**open** (no events) instead of
fail-closed (no tools).

Verified against Copilot CLI 1.0.81-0: the bare spelling denies tool calls once
the bridge is missing, and the per-shell fields do not. `timeoutSec: 5` bounds
the other fail-closed edge — a hung COM call — and a hook *timeout* was measured
to be fail-open, so shortening it only reduces the stall.

Claude has no `powershell` / `bash` field pair, but it does document a `shell`
field accepting `"bash"` or `"powershell"`, so it pins the shell and guards
inside `command` instead:

```json
{
  "type": "command",
  "command": "command -v wtcli.exe >/dev/null 2>&1 && wtcli.exe agent-hook --cli-source claude --event <topic>; exit 0",
  "shell": "bash"
}
```

Pinning is what makes this safe. Claude defaults to bash but falls back to
PowerShell when Git Bash is absent, and a guard written for the wrong shell is
noisy *even when the bridge is present* — measured both ways — so the guard and
the `shell` field have to agree. bash is also the cheaper of the two to start
(~62 ms versus ~380 ms for PowerShell).

Gemini has neither field pair, but it does not need one: its
`getShellConfiguration()` has no non-PowerShell branch on Windows — a
PowerShell `ComSpec`, then `pwsh.exe`, then a `powershell.exe` fallback, all
three returning `shell: "powershell"`. That single-shell guarantee makes it safe
to write the PowerShell guard straight into `command`:

```json
{
  "type": "command",
  "command": "try { wtcli.exe agent-hook --cli-source gemini --event <topic> } catch { }; exit 0"
}
```

Codex has neither field pair nor a `shell` field either, and needs the same
PowerShell guard as Gemini:

```json
{
  "type": "command",
  "command": "try { wtcli.exe agent-hook --cli-source codex --event <topic> } catch { }; exit 0"
}
```

Codex shipped the bare spelling until 0.1.5, on the theory that its marketplace
entry points at the package directory, so an uninstall would take the plugin
with it and the hook would never load. A manual run disproved that: Codex keeps
its own copy under `~/.codex/plugins/cache/<marketplace>/<plugin>/<version>/`,
outside the package, and `enabled = true` plus the trusted hashes stay in
`~/.codex/config.toml`. With the bridge gone the hooks still load, still run,
and Codex reports every one of them in the conversation:

```text
• UserPromptSubmit hook (failed)
  error: hook exited with code 1
```

That exit code is also what settles the shell. An unresolvable command exits 1
under PowerShell, 9009 under `cmd.exe`, and 127 under bash, which confirms the
`pwsh.exe -NoProfile -Command` dispatch already recorded in the shell table
above — so the guard is written for PowerShell rather than guessed.

Current state with the bridge missing:

| CLI | Mechanism | Result |
| --- | --- | --- |
| Copilot | `powershell` / `bash` fields, no bare `command` | exit 0, silent |
| Claude | `shell: "bash"` + `command -v` guard | exit 0, silent |
| Gemini | PowerShell guard in `command` | exit 0, silent |
| Codex | POSIX guard in `command`, cmd.exe wrapper in `commandWindows` | exit 0, silent in both Windows shells |
| OpenCode | JS `try`/`catch`, output ignored | exit 0, silent |

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
| Hooks not firing (OpenCode)      | `hook-trace.log` in the WTA log directory — the plugin records every spawn failure there. A present, correct `wt-agent-hooks.js` proves nothing on its own: the plugin spawns `wtcli` by argv, so it cannot resolve the MSIX app-execution alias on `PATH` and depends on the injected `WTCLI_PATH`. |
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
- **Codex must re-trust the native commands once.** Codex hashes each hook
  command for trust, so replacing the PowerShell command with
  `wtcli agent-hook` requires reviewing the updated plugin through `/hooks`.
  The hashes are taken over the command string, not the bundle version, so a
  later version bump that leaves Codex's commands alone does not ask again.
