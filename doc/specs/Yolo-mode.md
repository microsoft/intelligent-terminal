# Yolo mode design

Yolo mode lets an agent continue through ACP tool-call permission requests
without showing the normal approval card. It is intended for trusted agents
running in trusted environments.

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
| `wta-helper` (helper) | The per-tab WTA process that renders the agent-pane chat UI. Each eligible Terminal tab owns one pre-warmed helper, including while its agent pane is hidden. The helper owns that tab's local Yolo default, session overrides, and permission-interception logic. |
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
  whether an incoming permission request should be auto-approved, and renders
  `/yolo` status or errors.
- The master only routes ACP messages. For an Agent-native `allow_all` change,
  it forwards the helper's session-scoped request to the Agent instance bound
  to that helper.
- The Agent applies native permission configuration to the exact ACP
  `session_id`, when it supports that capability.

Therefore, `/yolo` is **session-scoped**, although its state is held inside
the per-tab helper process. Starting `/new` in the same tab creates a new ACP
session and resets the override.

## Goals

- Offer one agent-independent auto-approve experience.
- Support both a persistent global default and a temporary session override.
- Keep each ACP session isolated when one `wta-master` hosts multiple Agent
  CLI instances and multiplexes many helpers across them.
- Use Copilot's explicitly allowlisted native session permission mode when
  its exact contract is present, while retaining a protocol-level fallback.
- Never claim that `/yolo off` succeeded while the agent may still be in its
  native allow-all mode.
- Allow administrators to disable both entry points with one policy.

## Non-goals

- Yolo mode is not a general bypass for every Intelligent Terminal
  confirmation surface.
- It does not execute tool calls itself.
- It does not change whether an agent chooses to issue a tool call.
- It does not grant an ACP permission request when the agent offers no
  allow-shaped response.
- It does not persist a `/yolo` override across ACP sessions.

## User experience

### Global setting

Settings > AI agents contains:

> Auto-approve tool calls (Yolo mode)

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
helper with `--auto-approve-tools`. Later setting or policy changes are sent
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

If a native permission-mode update fails, the chat shows an error such as:

```text
/yolo off: <ACP error>
```

The prior effective state is retained. In particular, `/yolo off` never
reports success before an agent-native `allow_all=off` operation is
acknowledged.

Other command behavior:

- A second `/yolo` command for the same session is ignored while an update is
  pending.
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
- Terminal does not pass `--auto-approve-tools`.
- Terminal passes `--yolo-command-blocked`.
- A later policy change is hot-applied to existing helpers. Their effective
  state becomes off immediately, session overrides are cleared, and Copilot
  native sessions are reconciled to `allow_all=off`.
- If native disable fails, WTA restarts the agent stack rather than leaving a
  policy-blocked session running with native allow-all enabled.
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
| Pending changes | `App::pending_yolo_changes`, keyed by ACP `session_id` | Until the native/fallback acknowledgement arrives | Yes, internal only |

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
- Runtime changes recompute every live session. WTA reconciles Copilot's
  native `allow_all` value for each session while preserving explicit session
  overrides.
- Disabling native allow-all is fail-closed: if the ACP update fails, WTA
  requests an agent-stack restart.
- For master-attested Copilot, a missing or malformed selector cannot count as
  a successful disable. This also protects resumed sessions that may have
  persisted native `allow_all=on` inside the Agent.
- `/new`, session replacement, and `/restart` remove stale session overrides.
- An ACP server's native permission state is changed by WTA when a session is
  created in Yolo mode or when `/yolo on|off` succeeds.
- WTA does not continuously poll native permission configuration. If another
  actor changes the agent's native session config outside WTA, the two states
  can drift until WTA applies another change or the session is replaced.

No Yolo session state is written to `state.json`, the session history index,
or the hook data. Resuming or creating another ACP session does not inherit a
previous `/yolo` override.

## Approval architecture

There are two implementation paths.

### 1. Agent-native permission mode

The native path is allowlisted to the canonical built-in Agent ID `copilot`.
WTA does not enable it for another built-in or custom Agent based only on a
similar-looking config option.

The helper does not trust the Agent ID it requested. The master validates and
resolves the selection, returns `resolved_agent_id` in private initialize
metadata, and only that master-attested identity can enable the Copilot path.

After a Copilot `session/new`, WTA additionally requires the exact wire
contract:

- id `allow_all`;
- category `permissions`;
- ACP config kind `Select`; and
- selectable values containing both `on` and `off`.

If any check fails, Copilot also uses the WTA interception fallback.

WTA records the selector id and uses:

```text
session/set_config_option(
    sessionId = <current session>,
    configId = <advertised allow-all id>,
    value = "on" | "off")
```

When native allow-all is on, the agent normally stops sending
`session/request_permission` for that session. This avoids even the short
agent-to-WTA approval round trip.

Native mode is applied:

- immediately after a new session is created when its effective state is on;
- immediately after a session is loaded, using the loaded response's selector
  to reconcile native state both on and off; and
  and
- retroactively for a live session through
  `MasterExtRequest::SetSessionAllowAll` when `/yolo` changes.

For `/yolo`, `App` keeps the old local state until
`AppEvent::YoloModeChangeCompleted` arrives. On success it commits the
explicit session override. On failure it preserves the old state and reports
the error.

### 2. ACP permission interception fallback

For agents without a matching native config option, WTA handles incoming
`session/request_permission` in `WtaClient::request_permission`.

If the session's effective Yolo state is on, WTA:

1. skips the normal `PermissionRequest` UI event;
2. selects only an option whose ACP `PermissionOptionKind` is `AllowOnce`;
   and
3. returns that option immediately.

WTA deliberately never selects fallback `AllowAlways`: an Agent may stop
asking for permission afterward, leaving `/yolo off` unable to revoke that
Agent-side choice. If `AllowOnce` is unavailable, WTA falls back to the normal
interactive permission UI. It does not silently reject or fabricate an
option.

The fallback is agent-independent and also applies to custom ACP commands.

## Built-in agent support

The current built-in agents all support Yolo mode. Only Copilot is permitted
to use the native permission configuration; every other Agent uses WTA
permission interception.

| Agent | ACP transport | Native `allow_all` status | Current Yolo path |
|---|---|---|---|
| GitHub Copilot | Native: `copilot --acp --stdio` | Confirmed | Native per-session config, with WTA interception as fallback |
| Claude | `@agentclientprotocol/claude-agent-acp` wrapper | Not currently advertised | WTA `request_permission` interception |
| Codex | `@agentclientprotocol/codex-acp` wrapper | Not currently advertised | WTA `request_permission` interception |
| Gemini | Native: `gemini --acp` | Not currently advertised | WTA `request_permission` interception |
| OpenCode | Native: `opencode acp` | Not currently advertised | WTA `request_permission` interception |

Native support is intentionally hardcoded to the trusted canonical Agent ID
`copilot` and then validated against Copilot's exact advertised contract. A
new Agent is not promoted to the native path automatically. Supporting
another native implementation requires an explicit code change, contract
review, and tests.

The fallback means wrappers do not need to expose a wrapper-specific
`--yolo`, `--dangerously-skip-permissions`, or similar process flag.

## Which tool calls are auto-approved?

Yolo mode operates on the ACP permission protocol, not on a hardcoded tool
allowlist.

Any ordinary tool call that reaches WTA as `session/request_permission` is
eligible, regardless of its ACP `ToolKind`. Known kinds include:

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

`ToolKind` controls display metadata; it is not a Yolo security filter.

This has several important consequences:

- File reads, file edits, deletes, shell commands, searches, network fetches,
  and future agent-defined tools can all be auto-approved if the agent asks
  permission and offers an allow-shaped option.
- A tool call that the agent executes without sending
  `session/request_permission` is outside WTA's approval path. Yolo mode
  neither causes nor prevents that execution.
- ACP `tool_call` and `tool_call_update` notifications are status/output
  reporting. Receiving one does not itself trigger an approval decision.
- ACP client callbacks such as terminal creation are not automatically
  approved merely because Yolo mode is on; only their associated
  `request_permission`, if any, is handled by this mechanism.

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
Proposal-MCP and canonical proposal permission requests are handled before
the generic Yolo branch:

- a valid proposal channel may receive ACP `AllowOnce`;
- an invalid, stale, or non-canonical proposal is cancelled; and
- Yolo mode does not convert an invalid proposal into an approved one.

The internal ACP `AllowOnce` above only allows the validated proposal request
to reach Intelligent Terminal. It does **not** approve the proposed Terminal
mutation itself. The resulting action card still requires explicit user
approval before operations such as creating, splitting, focusing, or closing
panes, or sending input that executes a command.

## Multi-tab and multi-window isolation

The global default is copied into each helper and later hot-updated.
Per-session overrides are keyed by ACP `session_id`, never by the currently
focused tab.

One `wta-master` can lazily host multiple Agent CLI instances. Each helper is
bound to one selected Agent instance at a time, and helpers selecting the same
agent identity, source, and command may share that instance. The native
selector is invoked with the exact session id, and the fallback checks the
exact session id, so enabling one session does not enable another session,
whether the sessions use the same Agent instance or different ones.

Pending changes also retain the originating tab id. Completion output is sent
back to that tab even if focus changes. Before committing, `App` verifies that
the tab still owns the same session id; stale acknowledgements are ignored.

## End-to-end flows

### Global on, new session

```text
settings.json
  -> EffectiveAgentPaneYoloMode()
  -> wta-helper --auto-approve-tools
  -> YoloState.global_default = true
  -> session/new
  -> for canonical Copilot, apply native allow_all=on only when the exact
     verified config contract is advertised
  -> otherwise auto-answer request_permission in WTA
```

### `/yolo on` with global off

```text
/yolo on
  -> queue SetSessionAllowAll(session, true)
  -> native config succeeds, or agent reports no native channel
  -> YoloModeChangeCompleted(Ok)
  -> store session override = true
  -> display success
```

### `/yolo off` with global on

```text
/yolo off
  -> queue SetSessionAllowAll(session, false)
  -> wait; keep effective state on
  -> YoloModeChangeCompleted(Ok)
  -> store session override = false
  -> effective state becomes off
  -> display success
```

If native disable fails, the session override is not changed, so the UI and
WTA continue to describe the session as on.

## Current limitations

- `/yolo` entered before `session/new` completes is not queued.
- WTA trusts the ACP server's `AllowOnce` semantics. It cannot guarantee that
  an agent or adapter assigns the correct meaning to that option.
- Native allow-all behavior beyond the ACP contract is controlled by the
  agent. Copilot is the only built-in for which this path has been confirmed
  live; all other listed built-ins currently use the WTA fallback.
