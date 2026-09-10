# Yolo mode design

Yolo mode asks a supported agent provider to apply its advertised ACP session
capability for reduced or bypassed confirmations. The provider defines the
resulting permission, sandbox, file-access, and network-access behavior.
Intelligent Terminal does not answer ordinary ACP permission requests on the
user's behalf.

The product exposes a persistent preference for the provider selected as the
Settings default. It does not add a WTA-owned session command or a
per-agent-pane status badge.

## Goals

- Persist one default-provider preference in `agentPane.yoloMode`.
- Reconcile supported ACP sessions that use the Settings default provider to
  that preference through an exact, provider-advertised capability.
- Apply the same default-provider comparison to global, profile, `/agent`, and
  restored-layout bindings.
- Preserve policy-allowed manual or provider-restored session state during
  later automatic Settings reconciliation.
- Keep provider identity and ACP session routing authoritative across tabs,
  windows, and shared Agent CLI processes.
- Apply `AllowAutomaticApproval` policy changes to live sessions and fail closed when a
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
settings.json / Settings UI / AllowAutomaticApproval
  -> GlobalAppSettings::EffectiveAgentPaneYoloMode()
  -> TerminalPage default/current-provider decision
  -> helper startup, rebind_agent, and agent_config_changed
  -> helper YoloState resolved desired value and policy gate
  -> NativeYoloState provider contract and sequenced ACP mutation
  -> provider-owned session behavior
```

Terminal owns the persistent setting, policy-aware value, and decision about
whether it applies to a tab's current provider. Each helper receives the
resolved desired value at startup and rebind, then receives later changes
through `agent_config_changed`.

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

### Default-provider setting

Settings > AI agents contains:

> Automatic approval

The setting is stored as:

```jsonc
{
    "agentPane.yoloMode": true
}
```

The default is `false`. Enabling it requests provider-native Yolo for agent
panes using the Settings default provider; it is not proof that the provider
accepted a privileged mode. Changing the default among Copilot, Claude, Codex,
and Gemini preserves the current preference.

When OpenCode is selected as the Settings default, Settings and FRE clear the
preference and hide the disabled, Off control. A legacy OpenCode-plus-On value
is treated as Off and is normalized when the user next saves Settings. Custom
providers retain their existing behavior.

Gemini keeps the toggle enabled. While it is On, Settings shows the existing
non-closable informational notice that workspace trust and provider policy
govern whether Gemini accepts its native mode.

Administrative policy clears the preference and hides the disabled, Off
control in Settings and FRE. No unavailable/policy explanation is shown beside
the hidden setting.

The first-run settings page reads the localized title and description directly
from the SettingsEditor resource scope and persists the same
`agentPane.yoloMode` value. This keeps Settings and FRE copy and availability
behavior aligned without duplicating localized strings.

### Runtime provider selection

`/agent` does not change the persisted preference. A provider whose canonical
ID differs from the Settings default is actively reconciled to native Off, and
the first prompt remains gated until that transition reaches a known safe
result. Switching back to the default provider reapplies the persisted value
and waits for its native acknowledgement.

Provider identity comparison is case-insensitive and ignores model and
Host/WSL execution source. Existing `/agent` override tabs retain their
provider when the Settings default changes, but recompute whether the
preference applies. Default-following tabs keep their existing rebind behavior.

Profile `agentPaneBackend` selections and freshly-created panes restored from a
saved layout use the same comparison. A non-default provider starts from a
known-safe Off baseline, but the user may enable its reviewed provider-native
mode manually while policy allows it.

Loading an existing ACP session is different from creating a new session. Its
provider-native state is treated as provider-restored and automatic Settings
logic does not mutate it while policy allows. `AllowAutomaticApproval=0` still forces
the loaded session Off before prompts proceed.

### Commands and configuration

WTA intentionally has no built-in `yolo` command. A provider command named
`yolo` received through ACP `availableCommands` remains visible in completion
and is forwarded as an ordinary provider command; WTA must not reserve or mute
it.

GitHub Copilot's provider-owned `/allow_all` command is a reviewed privileged
entry point. WTA forwards it only while `AllowAutomaticApproval` permits Yolo. Other
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
per-session control for that mode. The default-provider preference reconciles
Gemini sessions when provider workspace trust and policy allow it.

## State model

The runtime state has three relevant pieces:

| State | Owner | Lifetime |
|---|---|---|
| Host automatic target and policy gate | `YoloState` | Helper process; initialized and hot-updated from Terminal settings and agent rebinds |
| Per-session control owner (`Automatic`, `Manual`, or `ProviderRestored`) | `YoloState` | Exact ACP session; cleared on replacement/reset |
| Native capability and captured restore value | `NativeYoloState` | Exact ACP session generation |
| Pending reconciliation and config gates | `App` and `NativeYoloState` | Until acknowledgement, known enable failure, or agent reset; failed disables and unknown outcomes remain fail-closed until Agent CLI replacement |

Terminal computes a strict host automatic target for every binding source:

```text
automatic_target =
    configured_default &&
    !policy_blocked &&
    current_provider_id == settings_default_provider_id
```

WTA combines that target with the session owner to produce:

```text
policy blocked                         -> Disable
owner Automatic or new session         -> Enable/Disable to automatic_target
owner Manual or ProviderRestored       -> NoOpinion
```

`NoOpinion` creates no native operation and no prompt gate. A reviewed native
value selected manually through `/config`, or a recognized provider command
such as Copilot `/allow_all`, changes only the current ACP session and marks it
manual without changing the persisted setting. Later ordinary Settings changes
do not overwrite that manual state. Policy remains authoritative and can force
every owner Off.

The client-reconciled-session marker prevents `SessionAttached` from issuing a
duplicate native operation after lazy first-prompt setup; it is not a user
preference or owner.

The owner map is runtime state, but a saved agent pane records the minimal
`Automatic`, `Manual`, or `ProviderRestored` provenance beside its resumable
ACP session ID. It never persists the actual Yolo value. Older or user-edited
layouts without a valid owner restore as `ProviderRestored`. No Yolo value is
written to the session history index or hook data.

## Session lifecycle and prompt gates

A capability-ready new session is reconciled to the current automatic target.
A lazy session establishes that target before its first prompt is sent. A
loaded session waits for its real `SessionAttached`, becomes
`ProviderRestored`, and then keeps its provider state while policy allows.
Session replacement, `/new`, tab reset/close, provider switch, and agent
restart clear the old owner together with stale capability generations and
pending gates.

Normal prompts, manual autofix, and automatic autofix remain blocked while the
session's provider-native reconciliation or privileged `/config` mutation is
pending. A known enable rejection releases the gate and the provider continues
through its normal interactive permission behavior. A failed disable or an
unknown remote outcome retains fail-closed state until the Agent CLI stack is
replaced.

Operations are serialized per session and fenced by lifecycle generation. A
newer desired operation supersedes an older one; stale completions cannot
commit state for a replaced or reused session ID.

Terminal emits `automatic_yolo_target` on ready, hot-config, and rebind events.
It also emits the prior `yolo_enabled` boolean with the same value during the
compatibility period. New WTA builds prefer `automatic_yolo_target`; older
hosts that omit it continue to work through `yolo_enabled`.

WTA includes `yolo_control_owner` in the per-tab `agent_state_changed`
snapshot. C++ applies that owner together with the projected agent session ID,
then writes it into the saved agent-pane command line as
`--initial-yolo-control-owner`. On restore, the helper returns the validated
owner to WTA only for the paired initial loaded session.

Both config-option and mode mutations have a bounded timeout. ACP cancellation
is cooperative, so a timeout is treated as an unknown provider outcome and
requests replacement of the shared master and Agent CLI pool. Fail-closed
multi-session reconciliation has one overall deadline and stops at the first
failure instead of waiting once per session.

## Administrative policy

`AllowAutomaticApproval` overrides and clears the stored preference. When policy blocks
Yolo mode:

- `EffectiveAgentPaneYoloMode()` returns `false`.
- The in-memory setting and `settings.json` are normalized to
  `agentPane.yoloMode=false`.
- Settings disables the toggle and displays the policy lock.
- New helpers receive `--yolo-policy-blocked` and do not receive an effective
  enabled default.
- Existing helpers receive the change through the normal settings-reload path.
- Every live session is reconciled to its captured nonprivileged value.
- Privileged `/config` selections, provider mode changes, and GitHub Copilot's
  advertised `/allow_all` command are rejected before reaching the provider.
- Prompt producers remain gated until the matching disable is acknowledged.
- Any unconfirmed disable restarts the agent stack fail closed.

Removing the policy does not restore the previous On value; the user must
enable the setting again.

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

When an authoritative config update removes a recognized privileged selector,
the current `/config` publication removes it as well. The helper retains only
its session-scoped control identity until teardown so a stale UI selection
cannot fall through the generic config path and bypass `AllowAutomaticApproval`; the
removed selector is not treated as a valid reversible capability.

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
