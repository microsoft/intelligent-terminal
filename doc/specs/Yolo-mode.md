# Yolo mode design

Yolo mode asks a supported agent provider to enable its advertised ACP
session mode for reduced or bypassed confirmations. The provider defines the
resulting permissions, sandbox, file access, and network access. WTA does not
answer ordinary provider permission requests on the user's behalf.

This document describes the current implementation, including its user
experience, state model, built-in agent coverage, approval boundary, policy
enforcement, and lifecycle.

## Architecture and terminology

Yolo mode spans several processes and scopes. The terms used throughout this
document mean:

| Term | Meaning |
|---|---|
| Terminal tab | A user-visible Intelligent Terminal tab. |
| Agent pane | The AI chat pane attached to a Terminal tab. Hiding the pane stashes it; it does not normally destroy its process or chat session. |
| `wta-helper` (helper) | The per-tab WTA process that renders the agent-pane chat UI. Each eligible Terminal tab owns one pre-warmed helper, including while its agent pane is hidden. The helper owns that tab's local Yolo default, session overrides, provider capability state, and ACP operations. |
| `wta-master` (master) | The shared WTA process that manages Agent CLI processes and routes ACP traffic between helpers and the correct Agent instance. One master can host multiple Agent CLI instances. The master does not own the global or per-session Yolo state. |
| Agent CLI instance | A Copilot, Claude adapter, Codex adapter, Gemini, OpenCode, or custom ACP server process managed by the master. Helpers selecting the same agent identity, execution source, and command can share one instance. |
| ACP session | One agent conversation identified by an ACP `session_id`. This is the actual scope of `/yolo`; it is narrower than the lifetime of a tab or helper. |

The relationship is:

```text
Intelligent Terminal
  |
  +-- Tab A
  |     +-- agent pane
  |           +-- wta-helper A
  |                 +-- ACP session A
  |
  +-- Tab B
        +-- agent pane
              +-- wta-helper B
                    +-- ACP session B

wta-helper A --+
               +--> shared wta-master --> one or more Agent CLI instances
wta-helper B --+
```

For Yolo mode, the responsibilities are deliberately split:

- Terminal reads the persistent global setting, passes its effective value to
  each helper at startup, and hot-pushes later setting or policy changes.
- The helper stores the default and the current ACP session override, decides
  which provider-advertised ACP session capability to invoke, and renders
  `/yolo` status or errors. It never answers an ordinary provider permission
  request on the user's behalf.
- The master only routes ACP messages. It forwards each session-scoped config
  or mode request to the Agent instance bound to that helper.
- The Agent applies its own advertised mode to the exact ACP `session_id`.

Therefore, `/yolo` is **session-scoped**, although its state is held inside
the per-tab helper process. Starting `/new` in the same tab creates a new ACP
session and resets the override.

### Working-directory compatibility

Provider-native mode and workspace-policy decisions apply to the ACP session's
actual working directory. This is especially visible with providers such as
Gemini that enforce their own workspace trust before accepting a privileged
mode. During package validation, a WSL pane exposed an existing launch bug:
its POSIX source path could also be used as the Win32 `wta-helper` process
starting directory, preventing the helper from starting. A Host agent selected
from that pane could likewise receive the POSIX path as its ACP context.

Terminal therefore resolves two values rather than changing the user's working
directory:

- A WSL agent receives the source-aware POSIX path through
  `--agent-source-cwd`.
- The Win32 helper starts only in a directory validated as usable by the Win32
  filesystem API, selected from the pane, window, profile, and user-home
  fallbacks.
- A Host agent receives that same validated helper directory instead of the
  raw source path.
- Existing panes that already report a valid Windows directory keep the same
  effective directory and precedence.

This compatibility correction is covered by
`AgentSourceUtilsTests::SeparatesAgentCwdFromHelperLaunchCwd`, including the
no-valid-helper-directory fallback, and by exact-package provider validation.
It does not add a trusted-directory feature or modify provider trust files.

## Goals

- Present provider-advertised ACP Yolo capabilities through one UI.
- Support both a persistent global default and a temporary session override.
- Keep each ACP session isolated when one `wta-master` hosts multiple Agent
  CLI instances and multiplexes many helpers across them.
- Support only reviewed canonical providers and exact advertised contracts.
- Never claim that `/yolo off` succeeded while the provider may still be in
  its native Yolo mode.
- Allow administrators to disable both entry points with one policy.

## Non-goals

- Yolo mode is not a general bypass for every Intelligent Terminal
  confirmation surface.
- It does not execute tool calls itself.
- It does not change whether an agent chooses to issue a tool call.
- It does not select `AllowOnce`, `AllowAlways`, or any other provider
  permission response.
- It does not synthesize a Yolo capability for an unsupported provider.
- It does not persist a `/yolo` override across ACP sessions.

## User experience

### Global setting

Settings > AI agents contains:

> Use provider Yolo mode

The warning explains that the agent may run commands and edit files without
asking for confirmation. The setting is stored in `settings.json` as:

```jsonc
{
    "agentPane.yoloMode": true
}
```

The default is `false`.

When a helper is created, `TerminalPage` evaluates the policy-aware
`EffectiveAgentPaneYoloMode()` value. If it is enabled, Terminal starts that
helper with `--yolo-mode`. Later setting or policy changes are sent
to existing helpers through `agent_config_changed`.

The global value is a default for every ACP session owned by that helper. It
does not merge into, or rewrite, individual session records.

### Session command

The agent chat accepts:

```text
/yolo
/yolo on
/yolo off
```

Bare `/yolo` means `/yolo on`. Typing `/yolo ` opens an `on`/`off` completion
list.

The command applies only to the active tab's current ACP `session_id`. On
success, the chat displays an explicit low-emphasis status:

```text
[on marker] /yolo on
[off marker] /yolo off
```

The actual UI uses a filled circle for on and a hollow circle for off.

If a provider-native session update fails, the chat shows an error such as:

```text
/yolo off: <ACP error>
```

The prior effective state is retained. In particular, `/yolo off` never
reports success before the provider acknowledges restoration of the prior
session config or mode.

Other command behavior:

- A newer `/yolo` command for the same session supersedes any pending update;
  stale acknowledgements from the older update are ignored.
- Running `/yolo` before the tab has a `session_id` is currently a no-op and
  produces no success message.
- `/new` removes the previous session override.
- `/restart` clears all session overrides owned by the helper.
- Closing the helper destroys all of its in-memory override state.

### Policy-blocked experience

The `AllowYoloMode` administrative policy gates both the global setting and
the session command.

When the policy is blocked:

- `EffectiveAgentPaneYoloMode()` returns false even if the raw JSON setting is
  true.
- The Settings toggle is disabled and shows the policy-lock message.
- Terminal does not pass `--yolo-mode`.
- Terminal passes `--yolo-command-blocked`.
- A later policy change is hot-applied to existing helpers. Their effective
  state becomes off immediately, session overrides are cleared, and native
  sessions are reconciled to their captured restore values.
- Prompt submission, including manual and automatic autofix, is gated for each
  affected session until the provider acknowledges the requested native state.
- If native disable fails, WTA restarts the agent stack rather than leaving a
  policy-blocked session running in a provider-native Yolo mode.
- `/yolo` refuses without changing state and displays:

```text
Yolo mode is disabled by your organization's policy.
```

The policy changes the effective value; it does not erase or rewrite the
user's stored `agentPane.yoloMode` value.

## State model

There are three distinct pieces of state.

| State | Storage | Lifetime | Mutable at runtime |
|---|---|---|
| Global default and policy gate | `YoloState`, initialized from `GlobalAppSettings` and updated through `agent_config_changed` | Persistent setting; helper-owned runtime copy | Yes |
| Session overrides | `YoloState::session_overrides`, a `HashMap<session_id, bool>` shared by `App` and `ClientState` | Current helper process and ACP session | Yes, through `/yolo` after acknowledgement |
| Pending changes | `App::pending_yolo_changes`, keyed by ACP `session_id` | Until the provider-native acknowledgement arrives | Yes, internal only |
| Pending reconciliation | `App::pending_yolo_reconciles`, keyed by transaction and target ACP session | Until startup, `/new`, settings, or policy reconciliation settles | Yes, internal only |

Each session override stores the explicit value selected by `/yolo on` or
`/yolo off`. Sessions without an override follow the current global default.

```text
effective_yolo(session) =
    false,                                      if policy is blocked
    session_overrides[session_id],              if an override exists
    global_default,                             otherwise
```

Therefore:

| Global default | Session override | Effective state |
|---|---|---|---|
| off | None | off |
| off | `/yolo on` | on |
| on | None | on |
| on | `/yolo off` (**opt out of global Yolo**) | off |

An explicit session choice continues to win when the user changes the global
default. Applying a blocking policy is stronger than either setting: it
forces every session off and clears all session overrides so they cannot
silently reactivate if the policy is later removed.

### Does the state change automatically?

- The persistent global setting changes only through settings editing or
  policy evaluation.
- A helper receives the effective global value at startup and hot-applies
  subsequent global or policy changes in place.
- Runtime changes recompute every live session. WTA reconciles the provider's
  native config or mode while preserving explicit session overrides.
- Startup and replacement sessions gate their first prompt until native
  reconciliation acknowledges success. A known unsupported enable falls back
  to interactive provider permissions; disable failures remain fail-closed.
- Disabling provider-native Yolo is fail-closed: if the ACP update fails, WTA
  requests an agent-stack restart.
- For a loaded session on a supported provider, a missing or malformed
  capability cannot count as a successful disable. This protects resumed
  sessions that may have persisted a provider-native mode.
- `/new`, session replacement, and `/restart` remove stale session overrides.
- An ACP server's native permission state is changed by WTA when a session is
  created in Yolo mode or when `/yolo on|off` succeeds.
- WTA does not continuously poll native configuration. If another actor
  changes the provider's session config outside WTA, the two states
  can drift until WTA applies another change or the session is replaced.

No Yolo session state is written to `state.json`, the session history index,
or the hook data. Resuming or creating another ACP session does not inherit a
previous `/yolo` override.

## ACP capability architecture

WTA is an ACP UI for Yolo mode. It invokes only capabilities advertised by a
reviewed provider on the current session. It never infers support from a CLI
flag or a similarly named custom option.

The helper does not trust the Agent ID it requested. The master validates and
resolves the selection, returns `resolved_agent_id` in private initialize
metadata, and only that master-attested canonical identity selects a provider
contract.

`NativeYoloState` records the advertised channel and original value per ACP
`session_id`. Config-option channels use `session/set_config_option`; legacy
mode channels use `session/set_mode`. Turning Yolo off restores the value that
session advertised before WTA enabled Yolo, rather than applying one global
default to every provider.

| Agent | Advertised contract | Enable operation | Disable operation |
|---|---|---|---|
| GitHub Copilot | `configOptions`: `allow_all`, category `permissions`, Select values `on` and `off` | `session/set_config_option(allow_all, on)` | Restore the captured value, normally `off` |
| Claude | `configOptions`: `mode`, category `mode`, including `bypassPermissions`; legacy `modes` is accepted when config options are unavailable | `session/set_config_option(mode, bypassPermissions)` | Restore the captured mode, normally `default` |
| Codex | `configOptions`: `mode`, category `mode`, including `agent-full-access`; legacy `modes` is accepted when config options are unavailable | `session/set_config_option(mode, agent-full-access)` | Restore the captured mode, normally `agent` |
| Gemini | `modes` includes `yolo` | `session/set_mode(yolo)` | Restore the captured mode, normally `default` |
| OpenCode | No reviewed ACP session Yolo capability | Unsupported; ordinary permission UI remains | No operation |
| Custom Agent | No trusted canonical provider contract | Unsupported; ordinary permission UI remains | No operation |

Codex's `agent-full-access` contract combines `approvalPolicy=never` with its
`dangerFullAccess` sandbox policy. Gemini and other providers likewise own the
full semantics of their advertised modes. WTA does not narrow, split, or
reinterpret those contracts; Settings warns that the provider defines the
resulting permissions, sandbox, file access, and network access.

Capability discovery occurs on `session/new` and `session/load`, with later
config and current-mode updates refreshing the recorded state. A missing
capability on a new, already-off session is safe to leave unchanged. A loaded
session on a supported provider remains fail-closed if WTA cannot confirm a
disable, because the provider may have persisted its native mode.

For `/yolo`, `App` keeps the old local state until
`AppEvent::YoloModeChangeCompleted` arrives. On success it commits the
explicit session override. On failure it preserves the old state and reports
the error. Session attach performs a second reconciliation against the latest
runtime setting so a setting change racing session creation cannot leave the
provider in a stale mode.

### Provider permission requests

Ordinary `session/request_permission` requests always use the normal
interactive permission UI, regardless of global or per-session Yolo state.
WTA does not select `AllowOnce`, `AllowAlways`, or any other provider option.

## Which tool calls may be affected?

Yolo mode selects an advertised provider session mode, not a WTA-maintained
tool allowlist. Providers may apply that mode to kinds including:

- `Read`
- `Edit`
- `Delete`
- `Move`
- `Search`
- `Execute`
- `Think`
- `Fetch`
- `SwitchMode`
- agent-defined or unknown kinds represented as `Other`

`ToolKind` controls display metadata; it is not a WTA authorization boundary.

This has several important consequences:

- File reads, edits, deletes, shell commands, searches, network fetches, and
  future agent-defined tools may all be affected according to the provider's
  mode contract.
- A tool call that the agent executes without sending
  `session/request_permission` remains provider-owned. WTA does not intercept
  that decision.
- ACP `tool_call` and `tool_call_update` notifications are status/output
  reporting. Receiving one does not itself trigger an approval decision.
- ACP client callbacks such as terminal creation retain their normal WTA
  handling. Provider-native Yolo does not create a new WTA callback bypass.

### Terminal action proposals

`request_terminal_actions` is the agent-facing method for proposing changes
to the user's existing Terminal state. These are not ordinary agent-owned
tool calls: they can mutate user-owned windows, tabs, and panes, including
sending input into a pane so that a command is executed there.

Terminal action proposals are always gated by Intelligent Terminal's explicit
confirmation state. **Yolo mode does not bypass this confirmation.** Even
when Yolo mode is on, sending text or commands into a user pane and executing
them still requires the user to explicitly approve the proposal.

The proposal channel also has its own capability and invocation validation.
Proposal-MCP and canonical proposal permission requests use their own
validation path:

- a valid proposal proceeds to the normal permission UI for user selection;
- an invalid, stale, or non-canonical proposal is cancelled; and
- Yolo mode does not convert an invalid proposal into an approved one.

After a user permits the validated request, the resulting action card still
requires explicit user approval before operations such as creating, splitting,
focusing, or closing panes, or sending input that executes a command.

## Multi-tab and multi-window isolation

The global default is copied into each helper and later hot-updated.
Per-session overrides are keyed by ACP `session_id`, never by the currently
focused tab.

One `wta-master` can lazily host multiple Agent CLI instances. Each helper is
bound to one selected Agent instance at a time, and helpers selecting the same
agent identity, source, and command may share that instance. The native
capability is invoked with the exact session id, so enabling one session does
not enable another session,
whether the sessions use the same Agent instance or different ones.

Pending changes also retain the originating tab id. Completion output is sent
back to that tab even if focus changes. Before committing, `App` verifies that
the tab still owns the same session id; stale acknowledgements are ignored.

## End-to-end flows

### Global on, new session

```text
settings.json
  -> EffectiveAgentPaneYoloMode()
  -> wta-helper --yolo-mode
  -> YoloState.global_default = true
  -> session/new
    -> discover the master-attested provider's exact advertised capability
    -> apply its native config or mode to that session
    -> unsupported providers remain interactive
```

### `/yolo on` with global off

```text
/yolo on
  -> queue SetSessionYolo(session, true)
  -> provider-native ACP operation succeeds
  -> YoloModeChangeCompleted(Ok)
  -> store session override = true
  -> display success
```

### `/yolo off` with global on

```text
/yolo off
  -> queue SetSessionYolo(session, false)
  -> wait; keep effective state on
  -> restore the captured provider config/mode
  -> YoloModeChangeCompleted(Ok)
  -> store session override = false
  -> effective state becomes off
  -> display success
```

If native disable fails, the session override is not changed, so the UI and
WTA continue to describe the session as on.

## Current limitations

- `/yolo` entered before `session/new` completes is not queued.
- Provider-native mode behavior beyond the ACP wire contract is controlled by
  the provider and may change permissions, sandbox, file access, or network
  access together.
- OpenCode and custom Agents remain interactive until an explicit provider
  contract is reviewed and implemented.
- Live acceptance remains required for each exact packaged provider version;
  deterministic tests cover discovery, routing, restoration, and isolation.
