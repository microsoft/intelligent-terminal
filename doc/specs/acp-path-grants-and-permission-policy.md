---
author: Kaitao Chen vanzue@github
created on: 2026-08-10
last updated: 2026-08-12
status: Implemented
---

# ACP path grants and permission policy

## Purpose

An Agent session has two independent controls:

- **Path grants** define where the Agent is expected to work.
- **Operation policy** decides which permission requests Intelligent Terminal
  may answer automatically.

A path grant is not approval to edit or execute, and it is not an operating
system sandbox. The Agent process retains the access granted by its user token,
its own sandbox, and the operating system.

## Session model

Each tab owns one ACP session. Its effective roots are:

```text
[primary cwd, global roots, session roots]
```

The primary `cwd` is selected when the tab's pre-warmed Agent helper starts:
an explicit virtual cwd wins, then that tab's active-pane cwd, the profile
starting directory, and finally the user home directory. It remains the base
for relative paths for the life of the ACP session; later shell `cd` operations
do not silently change it.

This explicit model is required because a Terminal tab can contain multiple
panes with independent, changing working directories. Shell-reported
directories are useful suggestions, but are not trusted authorization input.

### Root lifetimes

| Root | Scope | Storage |
|---|---|---|
| Primary `cwd` | One ACP session | Agent session metadata |
| Session root | Agent + execution environment + ACP `SessionId` | Package-private `session-path-grants.json` |
| Global root | Future and active host sessions | `settings.json`: `aiIntegration.allowedDirectories.host` |

Global roots are currently Windows-host only. Windows and WSL paths are never
mixed or translated implicitly.

## ACP behavior

Intelligent Terminal reads
`agentCapabilities.sessionCapabilities.additionalDirectories` during
initialization.

- If advertised, global roots are sent on `session/new`.
- Global and persisted session roots are sent on `session/load`.
- If not advertised, lifecycle requests omit `additionalDirectories`.
- The ACP standard has no mid-session root-mutation method. Adding or removing
  a root updates Intelligent Terminal's local policy immediately, but the
  Agent receives the new root set only when Intelligent Terminal later creates
  or loads a session with that root.

`cwd` is always primary. Additional roots do not change relative-path
resolution.

## User surfaces

### Agent pane

```text
/add-dir <absolute-path>
/remove-dir <absolute-path>
/list-dirs
```

These commands operate on the current ACP session only. Global roots are
managed in **AI Agents > Allowed directories**. For `/add-dir`, ghost
completion proposes the owning tab's active-pane directory. Tab or right-click
accepts the completion without executing it; Enter accepts and executes it.

When an ACP permission request contains one usable location and an
Agent-provided `allow_once` option, the permission card may also offer:

```text
Once | This session | Always | Deny
```

`This session` and `Always` store the displayed directory, then answer the
current ACP request with the Agent's exact `allow_once` option. Intelligent
Terminal never invents an option ID or converts `allow_once` to
`allow_always`.

### Settings

**AI Agents > Allowed directories** manages global host roots. Paths must be
absolute; Windows comparison is case-insensitive, ignores trailing separators,
and treats `/` and `\` as equivalent.

Operation policies are currently JSON-only:

```jsonc
{
    "aiIntegration.confirmation.readOperations": "auto",
    "aiIntegration.confirmation.createOperations": "prompt",
    "aiIntegration.confirmation.inputOperations": "prompt"
}
```

Each value is `auto`, `prompt`, or `deny`. Missing or invalid values fail closed
to `prompt`.

## Permission evaluation

Intelligent Terminal classifies only structured ACP `ToolKind`; it does not
parse shell command text.

| ACP kind | Policy |
|---|---|
| `read`, `search` | read |
| `edit`, `move`, `delete` | create |
| `execute` | input |
| Other or missing | always prompt |

Evaluation order:

1. Map `ToolKind` to an operation class. Unknown kinds prompt.
2. Apply that class's policy.
3. `prompt` shows the Agent-provided permission choices.
4. `deny` selects `reject_once`, or returns `cancelled` if unavailable.
5. `auto` requires an Agent-provided `allow_once`.
6. For read/create, every reported location must resolve safely inside an
   effective root. Missing, relative, ambiguous, or out-of-root locations
   prompt.
7. WSL read/create auto-approval is disabled until containment can be resolved
   safely in the WSL namespace.
8. Input auto-approval does not infer safety from a path. Shell commands remain
   `execute`, even when the command appears read-only.

Automatic approval removes only the blocking permission card. The tool call
remains visible and is marked as automatically approved by Intelligent
Terminal policy.

## Data and event flow

```text
settings.json
    │
    ├─ helper argv: initial roots + operation policies
    └─ agent_config_changed: hot global-root updates
                         │
active pane cwd ──► wta-helper / SessionRoots
                         │
                         ├─ session/new|load
                         │      cwd + capability-gated additionalDirectories
                         │
ACP request_permission ──┴─► permission_policy::evaluate
                                │
                                ├─ allow_once
                                ├─ reject_once / cancelled
                                └─ compact permission card
```

Global mutations initiated in WTA are routed to the owning `TerminalPage`,
written through the Settings Model, and broadcast back as
`allowed_directories_changed`. Window and stable-tab IDs prevent another
window or tab from consuming the result.

## Storage and safety

Session grants are written atomically under the package-private state root and
serialized with a named mutex across helper processes. The store is versioned
and bounded to 1 MiB, 2,048 records, and 128 directories per record.

Host containment canonicalizes the longest existing path prefix before
component-wise, case-insensitive comparison. This prevents lexical prefix
mistakes and rejects paths that cannot be resolved safely.

Removing a grant affects local decisions and future lifecycle requests. It
cannot revoke filesystem access already held by an Agent process.

## Capability limitations

- Agents without `additionalDirectories` support can still use locally stored
  roots for Intelligent Terminal policy, but do not receive them as ACP
  workspace roots.
- Copilot CLI 1.0.79 was verified to advertise
  `additionalDirectories=false`.
- Adding a root to an active session does not restart it automatically.
- Global WSL roots and WSL containment-based auto-approval are not implemented.

## Implementation map

| Area | File |
|---|---|
| Grant storage, effective roots, lifecycle requests | `tools/wta/src/path_grants.rs` |
| Policy classification and containment | `tools/wta/src/protocol/acp/permission_policy.rs` |
| Capability negotiation and permission dispatch | `tools/wta/src/protocol/acp/client.rs` |
| Slash commands and Agent-pane state | `tools/wta/src/app.rs`, `tools/wta/src/commands.rs` |
| Initial settings and active-pane cwd | `src/cascadia/TerminalApp/TerminalPage.cpp` |
| Persistent settings | `src/cascadia/TerminalSettingsModel/MTSMSettings.h` |
| Settings UI | `src/cascadia/TerminalSettingsEditor/AIAgents.xaml` |

## Validation

Tests cover capability gating, lifecycle root composition, store corruption and
bounds, Windows path comparison, policy decisions, containment, slash-command
behavior, permission-card routing, and active-pane completion. Run:

```powershell
cargo test --target x86_64-pc-windows-msvc --manifest-path tools\wta\Cargo.toml
```
