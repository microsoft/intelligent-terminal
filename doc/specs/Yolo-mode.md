# Yolo mode design

Yolo mode asks a supported agent provider to apply its advertised ACP session
capability for reduced or bypassed confirmations. The provider defines the
resulting permission, sandbox, file-access, and network-access behavior.
Intelligent Terminal does not answer ordinary ACP permission requests on the
user's behalf.

The product exposes a persistent, provider-independent global default. It does
not add a WTA-owned session command or a per-agent-pane status badge.

## Goals

- Persist one global default in `agentPane.yoloMode`.
- Reconcile each supported ACP session to that default through an exact,
  provider-advertised capability.
- Keep provider identity and ACP session routing authoritative across tabs,
  windows, and shared Agent CLI processes.
- Apply `AllowYoloMode` policy changes to live sessions and fail closed when a
  disable cannot be confirmed.
- Keep every ordinary ACP permission option under explicit user control.
- Preserve the separate confirmation boundary for terminal action proposals.

## Non-goals

- WTA does not register `/yolo`, `/yolo on`, or `/yolo off`.
- WTA does not persist a per-session Yolo override.
- The agent-pane header does not display Yolo on/off, pending, unavailable, or
  unknown status.
- Yolo mode is not a general bypass for Intelligent Terminal confirmation
  surfaces.
- WTA does not synthesize support for an unsupported or look-alike provider.
- `ToolKind` is display metadata, not an authorization boundary.
- Prompt instructions are not an authorization boundary.

## Architecture

```text
settings.json / Settings UI / AllowYoloMode
  -> GlobalAppSettings::EffectiveAgentPaneYoloMode()
  -> TerminalPage helper startup and agent_config_changed
  -> helper YoloState global default and policy gate
  -> NativeYoloState provider contract and sequenced ACP mutation
  -> provider-owned session behavior
```

Terminal owns the persistent setting and policy-aware effective value. Each
helper receives `--yolo-mode` and `--yolo-policy-blocked` at startup, then
receives later changes through `agent_config_changed`.

The helper shares its runtime Yolo state with the ACP client. `App` owns prompt
gates and reconciliation generations; `NativeYoloState` owns capability
discovery, captured restore values, operation sequencing, lifecycle fencing,
and bounded ACP mutations. The master supplies the canonical
`resolved_agent_id` and routes requests to the Agent CLI instance that owns the
exact ACP `session_id`.

A helper may own several tab sessions over its lifetime, and several helpers
may share one Agent CLI process. Every native mutation carries the exact ACP
session ID. No operation is inferred from the currently focused tab.

## User experience

### Global setting

Settings > AI agents contains:

> Use provider Yolo mode

The setting is stored as:

```jsonc
{
    "agentPane.yoloMode": true
}
```

The default is `false`. Enabling it expresses the user's preferred default for
supported providers; it is not proof that every provider accepted a privileged
mode.

The toggle remains editable when OpenCode or Gemini is the selected default
provider. Those choices do not make the provider-independent preference
invalid because another provider can be selected later, including through
`/agent`.

Settings shows contextual, non-closable notices only while the effective Yolo
preference is on and policy does not lock it:

- OpenCode: warning that no reviewed native Yolo capability is available and
  permission requests remain interactive.
- Gemini: informational notice that workspace trust and provider policy govern
  whether Gemini accepts its native mode.

Only administrative policy disables the toggle. Provider compatibility does
not grey it out.

### Commands and configuration

WTA intentionally has no built-in `yolo` command. A provider command named
`yolo` received through ACP `availableCommands` remains visible in completion
and is forwarded as an ordinary provider command; WTA must not reserve or mute
it.

GitHub Copilot's provider-owned `/allow_all` command is a reviewed privileged
entry point. WTA forwards it only while `AllowYoloMode` permits Yolo. Other
Copilot commands and same-named commands from custom or non-Copilot providers
remain ordinary provider commands.

The generic `/config` picker continues to expose ACP `configOptions`:

- Copilot can expose `allow_all` with `on` and `off` values.
- Claude can expose mode `bypassPermissions` and its restore value.
- Codex can expose mode `agent-full-access` and its restore value.

Selecting one of these reviewed privileged values uses the same policy check,
operation sequencing, timeout, and prompt gate as global reconciliation.
Ordinary config options remain unaffected.

Gemini currently advertises its `yolo` capability through ACP modes rather than
a config option. Without a WTA-owned command, Intelligent Terminal has no
per-session control for that mode. The global default still reconciles Gemini
sessions when provider workspace trust and policy allow it.

## State model

The runtime state has three relevant pieces:

| State | Owner | Lifetime |
|---|---|---|
| Global default and policy gate | `YoloState` | Helper process; initialized and hot-updated from Terminal settings |
| Native capability and captured restore value | `NativeYoloState` | Exact ACP session generation |
| Pending reconciliation and config gates | `App` and `NativeYoloState` | Until acknowledgement, known failure, or agent reset |

The effective desired state is:

```text
effective_yolo = global_default && !policy_blocked
```

`YoloState` has no session override map, and WTA persists no per-session Yolo
preference. By default, each session is reconciled to the current effective
global value. A reviewed native value selected manually through `/config`
changes only the current ACP session without changing `YoloState`, so that
session can differ until a later global or policy reconciliation, session
replacement, or reset reapplies the global value.

The client-reconciled-session marker prevents `SessionAttached` from issuing a
duplicate native operation after lazy first-prompt setup; it is not a user
preference or persisted override.

No Yolo runtime state is written to the session history index, hook data, or
`state.json`.

## Session lifecycle and prompt gates

A capability-ready bootstrap or attached session is reconciled to the latest
effective global value. A lazy session establishes that value before its first
prompt is sent. Session replacement, `/new`, tab reset/close, provider switch,
and agent restart remove stale capability generations and pending gates.

Normal prompts, manual autofix, and automatic autofix remain blocked while the
session's provider-native reconciliation or privileged `/config` mutation is
pending. A known enable rejection releases the gate and the provider continues
through its normal interactive permission behavior. A failed disable or an
unknown remote outcome retains fail-closed state until the Agent CLI stack is
replaced.

Operations are serialized per session and fenced by lifecycle generation. A
newer desired operation supersedes an older one; stale completions cannot
commit state for a replaced or reused session ID.

Both config-option and mode mutations have a bounded timeout. ACP cancellation
is cooperative, so a timeout is treated as an unknown provider outcome and
requests replacement of the shared master and Agent CLI pool. Fail-closed
multi-session reconciliation has one overall deadline and stops at the first
failure instead of waiting once per session.

## Administrative policy

`AllowYoloMode` overrides the stored global preference without erasing it.
When policy blocks Yolo mode:

- `EffectiveAgentPaneYoloMode()` returns `false`.
- Settings disables the toggle and displays the policy lock.
- New helpers receive `--yolo-policy-blocked` and do not receive an effective
  enabled default.
- Existing helpers receive the change through the normal settings-reload path.
- Every live session is reconciled to its captured nonprivileged value.
- Privileged `/config` selections, provider mode changes, and GitHub Copilot's
  advertised `/allow_all` command are rejected before reaching the provider.
- Prompt producers remain gated until the matching disable is acknowledged.
- Any unconfirmed disable restarts the agent stack fail closed.

Policy watchers cover HKLM and HKCU, including creation of a previously missing
policy path by watching and rebinding from the deepest existing ancestor.

## Provider contracts

WTA selects a contract only from the master-attested canonical provider
identity and the exact advertised capability shape. A similarly named custom
option is not sufficient.

| Provider | Advertised contract | Enable | Restore |
|---|---|---|---|
| GitHub Copilot | `configOptions` ID `allow_all`, category `permissions`, Select values `on`/`off`; provider command `/allow_all` | `session/set_config_option(allow_all, on)` or the policy-gated provider command | Captured value, normally `off` |
| Claude | `configOptions` ID `mode` with `bypassPermissions`; legacy mode fallback | `session/set_config_option(mode, bypassPermissions)` | Captured value, normally `default` |
| Codex | `configOptions` ID `mode` with `agent-full-access`; legacy mode fallback | `session/set_config_option(mode, agent-full-access)` | Captured value, normally `agent` |
| Gemini | ACP mode `yolo` | `session/set_mode(yolo)` | Captured mode, normally `default` |
| OpenCode | No reviewed reversible capability | Unsupported; normal permission UI remains | No operation |
| Custom provider | No trusted canonical contract | Unsupported; normal permission UI remains | No operation |

Codex `agent-full-access` combines `approvalPolicy=never` with
`dangerFullAccess`. Gemini and other providers likewise own the complete
semantics of their mode. WTA does not split permission behavior from sandbox,
file, or network effects.

Capability discovery occurs on `session/new` and `session/load`; later config
and mode updates refresh the captured state. A fresh session that lacks a
capability can safely remain off. A loaded or previously privileged session
that cannot prove restoration fails closed.

## Permission and terminal-action boundaries

Ordinary `session/request_permission` requests always enter the normal
interactive permission UI. WTA never selects `AllowOnce`, `AllowAlways`, or any
other provider option, including while the global Yolo setting is enabled.
Invalid, stale, or non-canonical proposal permissions remain cancelled.

`request_terminal_actions` is a separate product-owned workflow for proposed
changes to user-owned Terminal panes. Valid proposals render a recommendation
card and require explicit Run or Insert confirmation. Provider-native Yolo does
not bypass that card.

This is a workflow boundary, not a complete same-user sandbox. An Agent CLI
that can execute arbitrary commands may reach existing WT protocol/COM surfaces
directly. The system prompt's instruction to use `request_terminal_actions` is
defense in depth, not authorization. See `doc/security-model.md`.

## Current limitations

- OpenCode and custom providers remain interactive until a reviewed reversible
  ACP session capability is implemented.
- Gemini has no per-session WTA control because its current adapter advertises
  a mode but no corresponding config option.
- Provider mode semantics and managed restrictions remain provider-owned.
- WTA does not continuously poll for changes made by another actor; the next
  config update, reconciliation, or replacement session refreshes state.
- Live acceptance remains version-specific. Deterministic tests cover contract
  discovery, routing, restoration, timeout handling, policy, and permission
  invariants without consuming model quota.
