---
author: Kaitao Chen vanzue@github
created on: 2026-08-10
last updated: 2026-08-10
issue id: TBD
---

# ACP path grants and permission policy

## Status

Proposed.

## Abstract

Intelligent Terminal hosts long-running AI Agent sessions in per-tab panes.
Users commonly work across multiple repositories, shared libraries, and
documentation directories, while the primary shell pane changes its working
directory over time.

This specification introduces two related but independent controls:

1. **Path grants** define the filesystem roots an Agent session may treat as
   its workspace.
2. **Permission policy** decides whether Intelligent Terminal should
   automatically answer an Agent's ACP `session/request_permission` request or
   show the existing permission card.

Global path grants are stored by Intelligent Terminal and supplied to each
eligible ACP session through the standard `additionalDirectories` field.
Session grants are scoped to one ACP `SessionId` and are supplied again when
that session is loaded or resumed. A path grant never implies approval for
editing, deleting, or executing commands; those decisions remain controlled by
the operation policy.

## Goals

- Let users grant access to an additional directory once instead of approving
  the same out-of-workspace path repeatedly.
- Support both session-scoped and persistent global grants.
- Use standard ACP `additionalDirectories` when the Agent advertises
  `sessionCapabilities.additionalDirectories`.
- Preserve per-tab isolation under the shared helper + master architecture.
- Keep every Agent tool call visible even when its permission request is
  automatically approved.
- Enforce the existing
  `aiIntegration.confirmation.{read,create,input}Operations` settings at
  runtime.
- Fail closed when a permission request cannot be classified reliably.
- Preserve tool, sandbox, operating-system, content-exclusion, and enterprise
  policy boundaries.

## Non-goals

- Treating `additionalDirectories` as an operating-system sandbox.
- Automatically trusting every directory visited by a shell pane.
- Automatically approving every operation inside an allowed directory.
- Defining a cross-Agent persistent interpretation of ACP
  `PermissionOptionKind::AllowAlways`.
- Adding a non-standard ACP method for mid-session root mutation.
- Making a shared Agent process own one process-wide directory allowlist.
- Replacing Agent-specific permission systems.

## Terminology

| Term | Meaning |
|---|---|
| Primary root | The ACP session's absolute `cwd`. Relative paths continue to resolve against it. |
| Additional root | An absolute directory sent in ACP `additionalDirectories`. |
| Effective roots | The ordered root set `[cwd, ...additionalDirectories]`. |
| Global grant | A persistent Intelligent Terminal grant applied to future eligible sessions in the same execution environment. |
| Session grant | A grant associated with one ACP `SessionId`, including later load/resume of that session. |
| Operation policy | The `auto`, `prompt`, or `deny` decision for a class of Agent operation. |
| Auto-approval | Intelligent Terminal selects an Agent-provided `allow_once` option without showing a blocking permission card. |
| Execution environment | The filesystem namespace in which the Agent runs, such as Windows host or `wsl:<distro>`. |

## Required invariants

1. `cwd` is always the primary root.
2. Additional roots are attached to an ACP **session**, never to the shared
   Agent process.
3. A grant for Tab A must not expand Tab B's session roots unless it is an
   explicit global grant.
4. Global grants are namespaced by execution environment.
5. A path grant answers **where** an Agent may work. Operation policy answers
   **what** Intelligent Terminal may approve automatically.
6. Automatic approval requires both an allowed operation class and sufficient
   structured evidence to evaluate the request.
7. Automatic approval selects `allow_once`; Intelligent Terminal owns its
   persistent policy.
8. Tool calls remain visible when their permission is automatically approved.
9. Missing capability, missing metadata, ambiguous paths, and unsafe path
   resolution fail closed.

## User model

### Grant lifetimes

The permission surface offers three lifetimes:

| Choice | Stored by Intelligent Terminal | Future behavior |
|---|---|---|
| Allow once | No | Approves only the current request. |
| Allow this directory for this session | By ACP `SessionId` | Included when that session is loaded or resumed; removed when the session is deleted. |
| Always allow this directory | Global settings | Included in every future eligible session for that execution environment. |

Selecting a directory grant approves the current request only through an
Agent-provided `allow_once` option. It does not select the Agent's
`allow_always` option.

### Expected behavior

Given:

```jsonc
{
    "aiIntegration.allowedDirectories": {
        "host": [
            "C:\\src\\shared"
        ]
    },
    "aiIntegration.confirmation.readOperations": "auto",
    "aiIntegration.confirmation.createOperations": "prompt",
    "aiIntegration.confirmation.inputOperations": "prompt"
}
```

the expected decisions are:

| Request | Decision |
|---|---|
| Read `C:\src\shared\a.cpp` | Automatically approve and show the tool call. |
| Search under `C:\src\shared` | Automatically approve and show the tool call. |
| Edit `C:\src\shared\a.cpp` | Show the permission card because create operations are `prompt`. |
| Delete `C:\src\shared\a.cpp` | Show the permission card because create operations are `prompt`. |
| Execute PowerShell in `C:\src\shared` | Show the permission card because input operations are `prompt`. |
| Read `D:\private\a.txt` | Show a path-grant prompt because the path is outside the effective roots. |
| Request with no usable kind or target | Show the existing permission card. |

## Settings

### Persistent path grants

Add a global setting conceptually shaped as:

```jsonc
{
    "aiIntegration.allowedDirectories": {
        "host": [
            "C:\\src",
            "\\\\server\\share\\docs"
        ],
        "wsl:Ubuntu": [
            "/home/user/src"
        ]
    }
}
```

The concrete Settings Model projection may use a dedicated runtime class
instead of a JSON object. It must retain the execution-environment key and the
user-entered path for display.

The initial Host-only implementation uses the flat JSON key
`aiIntegration.allowedDirectories.host`. A future WSL implementation may
replace this with an environment-keyed projection; it must provide an explicit
migration rather than interpreting Host paths as WSL paths.

Windows and WSL roots must never be mixed or translated implicitly. A Windows
path is not sent to a WSL Agent, and a Linux path is not sent to a host Agent.
Path translation may be added later only as an explicit, visible operation.

### Operation policy

Use the existing settings:

```jsonc
{
    "aiIntegration.confirmation.readOperations": "auto",
    "aiIntegration.confirmation.createOperations": "prompt",
    "aiIntegration.confirmation.inputOperations": "prompt"
}
```

Valid values:

| Value | Meaning |
|---|---|
| `auto` | Automatically select `allow_once` when classification and scope checks pass. |
| `prompt` | Show the existing permission card. |
| `deny` | Select an Agent-provided `reject_once`; if unavailable, return ACP `cancelled`. |

The current settings exist but are not runtime-enforced. Enabling enforcement
must not silently interpret malformed values as `auto`; malformed and unknown
values resolve to `prompt`.

Fresh installations should default sensitive operation classes to `prompt`, as
recommended by [`security-model.md`](../security-model.md). Migration behavior
for existing installations must be explicit and versioned rather than inferred
from the absence of a setting.

## ACP contract

### Capability negotiation

The Helper reads:

```text
initialize.result.agentCapabilities
    .sessionCapabilities
    .additionalDirectories
```

Only an Agent that advertises this capability receives
`additionalDirectories`. Older Agents may reject unknown request properties,
so capability gating is mandatory.

### Session creation

For a host session:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "session/new",
  "params": {
    "cwd": "C:\\src\\app",
    "additionalDirectories": [
      "C:\\src\\shared",
      "D:\\product-docs"
    ],
    "mcpServers": []
  }
}
```

The effective additional-root list is:

```text
normalize(
    global grants for this execution environment
    + session grants for this SessionId, when applicable
    - entries identical to cwd
    - exact duplicates
)
```

The first occurrence order is preserved. Overlapping or nested roots are not
removed merely because their accessible file union overlaps.

### Load and resume

ACP requires `session/load` and `session/resume` to carry the full intended
additional-root list. Omission or an empty list activates no additional roots.

Intelligent Terminal therefore persists or reconstructs the complete session
grant set and sends:

```text
global grants + grants stored for the target SessionId
```

on every supported load/resume path.

### Mid-session changes

ACP does not currently define mid-session mutation of
`additionalDirectories`.

When the user adds a directory during an active turn:

1. Intelligent Terminal records the session or global grant.
2. It answers the current permission request using an Agent-provided
   `allow_once`, when available.
3. The new root is sent formally on the next new/load/resume lifecycle request.
4. A verified Agent-specific command, such as Copilot `/add-dir`, may be used
   as a compatibility optimization, but never as the cross-Agent contract.

Intelligent Terminal must not silently discard conversation state merely to
recreate a session with a larger root set. A future reconnect action may apply
the updated roots through `session/load`.

### Unsupported Agents

When an Agent does not advertise additional-directory support:

- do not send the field;
- retain the Intelligent Terminal grant;
- continue to evaluate incoming permission requests using the local policy;
- state in `/list-dirs` that the root is stored but not advertised to the
  active Agent;
- do not claim that the Agent is sandboxed to, or aware of, that root set.

Copilot CLI must follow this capability rule. Its interactive `/add-dir`
feature is not evidence that its ACP server implements the standard field.

## Helper + master architecture

The directory list is per session:

```text
Terminal settings / session grant store
        |
        v
per-tab wta-helper
        |
        | session/new or session/load
        | { cwd, additionalDirectories }
        v
wta-master
        |
        | forwards the same ACP lifecycle request
        v
Agent CLI process
```

The one shared Agent CLI process may host multiple ACP sessions with different
effective roots. The master must not union roots across Helpers or convert
them into process-level command-line arguments.

All lifecycle construction sites must use one shared request builder. This
includes:

- initial pre-warm `session/new`;
- `/new`;
- startup load;
- load failure fallback;
- reconnect/restart;
- resume;
- tests and mock-Agent harnesses where path behavior is under test.

## Permission policy engine

### Placement

The engine runs in the owning Helper before
`WtaClient::request_permission()` emits `AppEvent::PermissionRequest`.

The master continues to route `session/request_permission` by ACP `SessionId`
to the owning Helper. It does not decide user policy.

### Inputs

```rust
struct PermissionContext {
    session_id: SessionId,
    tool_call_id: ToolCallId,
    kind: Option<ToolKind>,
    locations: Vec<PathBuf>,
    raw_input: Option<serde_json::Value>,
    options: Vec<PermissionOption>,
    effective_roots: EffectiveRootSet,
    operation_policy: OperationPolicy,
    managed_policy: ManagedPolicy,
}
```

The exact Rust types may differ, but the structured ACP request must remain
available until after the policy decision. It must not be reduced first to the
current display-only icon, target string, and option labels.

### Operation classification

| ACP `ToolKind` | Operation class |
|---|---|
| `read`, `search` | Read |
| `edit`, `move`, `delete` | Create/mutate |
| `execute` | Input/execute |
| `fetch` | Unclassified by this spec; prompt |
| `think`, `switch_mode` | No automatic permission decision; prompt if an Agent requests one |
| `other` or missing | Unknown; prompt |

For `move`, every source and destination location must be in scope.

`rawInput` is Agent-defined and may improve display or an Agent-specific
classifier, but generic auto-approval must not depend on undocumented
`rawInput` shapes.

### Path evaluation

Read and create/mutate requests are eligible for automatic approval only when:

1. the request supplies at least one absolute location;
2. every location can be resolved safely for the relevant execution
   environment;
3. every resolved location is contained by at least one effective root; and
4. content-exclusion and managed policy permit the access.

Containment must account for:

- Windows case-insensitive comparison;
- path-component boundaries (`C:\src2` is not inside `C:\src`);
- `.` and `..`;
- UNC paths;
- existing symlinks and junctions;
- broken links and nonexistent create targets;
- WSL's case-sensitive Linux path rules.

Authorization must not rely only on lexical prefix matching. Ambiguous
resolution fails closed.

Execute requests are governed by `inputOperations`. An allowed working
directory does not make a shell command safe. Generic command-path extraction
is not reliable enough to turn a path grant into execute approval.

### Decision algorithm

```text
1. Apply managed deny and content-exclusion policy.
2. Preserve the existing trusted Proposal MCP special-case validation.
3. Classify the operation.
4. Resolve and validate required locations.
5. Read the operation class's auto/prompt/deny setting.
6. For auto:
     a. require a reliable classification;
     b. require all required paths to be in scope;
     c. require an Agent-provided allow_once option;
     d. return that exact optionId.
7. For deny:
     a. select reject_once when provided;
     b. otherwise return cancelled.
8. Otherwise, emit the existing blocking permission card.
```

The engine never invents an option ID and never upgrades `allow_once` into
`allow_always`.

### Decision type

```rust
enum PermissionDecision {
    AutoApprove {
        option_id: PermissionOptionId,
        operation: OperationClass,
        reason: ApprovalReason,
    },
    AutoReject {
        option_id: Option<PermissionOptionId>,
        reason: RejectionReason,
    },
    Prompt {
        reason: PromptReason,
    },
}
```

The decision type is pure and independently unit-testable.

## Tool-call and permission UI

### Visibility

Automatic approval removes only the blocking permission card. It does not hide
the Agent's tool call.

The tool-call row shows a non-interactive status:

```text
→ Reading C:\src\shared\a.cpp
  Auto-approved by Intelligent Terminal policy
```

Tool-call updates remain keyed by `toolCallId` so an embedded tool-call update
in `request_permission` does not duplicate a row already created by
`session/update`.

If the Agent never sent a separate tool-call notification, Intelligent
Terminal may materialize the tool-call information carried by
`request_permission`, but must not invent completion or success.

### Interactive directory grant

When a request contains a usable file location, the permission card may add
Intelligent Terminal-owned actions:

```text
Allow once
Allow this directory for this session
Always allow this directory
Reject
```

The UI must display the exact directory that will be stored.

- A directory target proposes that directory.
- A file target proposes its containing directory.
- Multiple unrelated locations must not silently collapse to a broad common
  ancestor. The UI grants each displayed directory or requires explicit
  selection.
- A drive root, home directory, UNC share root, or similarly broad grant
  requires an additional warning.

These Intelligent Terminal actions still respond to the current ACP request
with an Agent-provided `allow_once`. If the Agent does not offer `allow_once`,
the directory-lifetime actions are not shown and the request falls back to the
Agent-provided choices.

### Slash commands

Add local WTA commands:

```text
/add-dir <absolute-path>
/add-dir --global <absolute-path>
/list-dirs
/remove-dir <absolute-path>
/remove-dir --global <absolute-path>
```

`/list-dirs` distinguishes:

- primary `cwd`;
- active session grants;
- global grants;
- execution environment;
- whether the active Agent received the root through standard ACP;
- grants stored locally but unsupported by the active Agent.

Local WTA commands take precedence over same-named Agent commands. A
Copilot-specific `/add-dir` compatibility call is internal and must not change
the user-facing command semantics.

## Working-directory changes

Changing a shell pane's directory does not automatically create a global or
session grant.

For a pre-warmed Agent session that has not processed a user or Autofix turn,
Intelligent Terminal may replace the unused session so its primary `cwd`
matches the tab's current trusted working directory. Once a session has
processed a turn, changing primary `cwd` requires an explicit session
transition; adding the new directory as an additional root does not change
relative-path semantics.

OSC-reported working directories are untrusted input for authorization. They
may propose a path for display, but cannot create a grant without user action
or an existing broader grant.

## Storage and lifecycle

### Global grants

Global grants live in Terminal settings and survive application and Agent
restarts. Removing a global grant affects future lifecycle requests and local
policy decisions immediately. It cannot revoke access already held by an
Agent process with direct filesystem access; the UI must not claim otherwise.

### Session grants

Session grants live in package-private LocalState, keyed by:

```text
Agent identity + execution environment + ACP SessionId
```

They survive helper/master restart so load/resume can reproduce the intended
root set. They are removed when the corresponding session is explicitly
deleted. Retention for inaccessible historical sessions follows session
history retention.

The store must use atomic replacement and bounded parsing. A malformed store
fails closed to no session grants and surfaces a diagnostic warning.

## Security

### Trust boundaries

`additionalDirectories` communicates intended workspace scope. In the current
direct-filesystem Agent model, it is not a sandbox. An Agent process still runs
with the user's operating-system permissions.

If Intelligent Terminal later advertises ACP
`fs.readTextFile`/`fs.writeTextFile`, the Client implementation must enforce
the effective roots itself before reading or writing.

### Policy precedence

From strongest to weakest:

```text
managed deny / content exclusion
    > explicit operation deny
    > sandbox and OS restrictions
    > path grants
    > automatic operation approval
```

A global grant cannot override an enterprise deny, content-exclusion rule, or
operating-system failure.

### Persistent-policy changes

Global grants and automatic-approval settings are security-sensitive,
persistent configuration. Settings changes require the same meta-confirmation
hardening identified in `security-model.md`; a process writing `settings.json`
must not silently expand trusted roots or automatic approvals without the
product's settings-integrity policy.

### Logging

Log:

- decision (`auto_approve`, `auto_reject`, `prompt`);
- operation class;
- grant scope (`primary`, `session`, `global`, `none`);
- Agent capability support;
- reason code;
- SessionId and toolCallId using existing diagnostic conventions.

Do not log file contents, raw permission input, or full paths at the normal
shipping log level. Full paths may appear only at trace level under the
existing sensitive-content logging policy.

## Compatibility

- Requests omit `additionalDirectories` for Agents that do not advertise the
  capability.
- Existing permission cards remain the fallback for every ambiguous request.
- Existing Proposal MCP auto-approval remains earlier and narrower than the
  generic policy engine.
- Custom Agents receive no Agent-name-based special case.
- Agent-specific compatibility behavior must be capability- or profile-driven
  and independently live-tested.
- Global grants are not passed as shared Agent command-line arguments.

## Accessibility

- Automatic approval must be represented in timeline text, not color alone.
- Permission buttons retain keyboard navigation and `Y`/`N` quick actions.
- Screen readers announce the exact directory and grant lifetime before a
  persistent grant is stored.
- Broad-directory warnings are part of the accessible name/description.
- `/list-dirs` provides a text representation equivalent to the Settings UI.

## Reliability

- Policy evaluation is pure and deterministic.
- Settings and session-grant snapshots are captured before evaluating one
  request so a hot settings reload cannot produce a mixed decision.
- Removing a grant during an in-flight decision affects the next request; the
  current request completes against its captured snapshot.
- Helper disconnect cancels pending permission requests as today.
- Master routing remains keyed by ACP `SessionId`.

## Performance, power, and efficiency

- Root lists are expected to be small and are normalized once per settings or
  session-grant update.
- Permission containment checks use the normalized snapshot and avoid directory
  enumeration.
- No filesystem indexing is implied by storing a grant.
- No additional process is introduced.

## Implementation phases

### Phase 1: Policy core

- Add typed operation classes, policies, decisions, and reason codes.
- Preserve structured ACP `ToolKind`, locations, and options through the
  permission boundary.
- Add a pure policy evaluator and path-containment abstraction.
- Keep all decisions at `prompt` until settings propagation is complete.

### Phase 2: Runtime confirmation enforcement

- Propagate existing confirmation settings from Terminal to each Helper.
- Run the policy evaluator in `WtaClient::request_permission`.
- Auto-select only exact Agent-provided `allow_once`/`reject_once` options.
- Show auto-approved status on the tool-call row.
- Preserve Proposal MCP handling.

### Phase 3: Global and session path grants

- Add Settings Model, Settings UI, storage, slash commands, and removal.
- Add execution-environment-aware normalization.
- Add permission-card grant actions.
- Add hot settings propagation.

### Phase 4: ACP additional roots

- Read `sessionCapabilities.additionalDirectories`.
- Centralize new/load/resume request construction.
- Attach the full root list to every lifecycle path.
- Persist session roots required for load/resume.
- Surface unsupported/stored state in `/list-dirs`.

### Phase 5: Compatibility and live validation

- Live-test the pinned Copilot, Gemini, Claude adapter, Codex adapter, and a
  strict mock Agent.
- Add only verified Agent-specific dynamic-add compatibility.
- Validate Host and WSL path namespaces independently.
- Update user, administrator, and security documentation.

## Validation matrix

### Unit tests

- ToolKind to operation-class mapping.
- `auto`/`prompt`/`deny` for every class.
- Missing kind, locations, and options fail closed.
- `allow_once` is preferred; `allow_always` is never selected automatically.
- Windows component-aware, case-insensitive containment.
- Linux component-aware, case-sensitive containment.
- UNC, `..`, symlink, junction, broken-link, and nonexistent-target behavior.
- Multiple locations require all locations in scope.
- Global and session grants do not cross execution environments.
- Duplicate roots and `cwd` handling preserve ACP ordering requirements.

### Mock ACP tests

- Capability present: new/load requests contain the expected complete list.
- Capability absent: requests omit additional roots.
- Auto-approved request returns the exact option ID and creates no permission
  card.
- Auto-approved request leaves one visible tool-call row.
- Prompted request round-trips the selected option as today.
- Denied request selects `reject_once` or returns cancelled.
- Two Helpers sharing one master receive different session root lists.
- Reconnect and fallback session paths retain the correct grants.

### Settings and UI tests

- JSON parse, serialization, copy, inheritance, and malformed-value fallback.
- Add/remove session and global grants.
- Broad-directory warning.
- `/list-dirs` scope and capability labels.
- Screen-reader text for automatic approval and persistent grants.
- Settings hot reload changes the next decision.

### Live tests

- Copilot ACP capability advertisement and permission behavior.
- One host session with two additional roots.
- Two tabs with disjoint session grants under one shared Agent process.
- Application restart followed by session load.
- Unsupported Agent fallback.
- WSL distro isolation.
- Tool permission remains independent from path permission.

## Open questions

1. Which settings migration marker distinguishes fresh installations from
   existing users when changing confirmation defaults?
2. Should session grants expire with session-history retention or use a shorter
   independent retention period?
3. Which managed policy controls persistent user grants and automatic approval?
4. Should reconnect-to-apply be offered automatically when a standard-only
   Agent cannot mutate roots mid-session?

## Resources

- [ACP Additional Workspace Roots](https://agentclientprotocol.com/protocol/v1/session-setup#additional-workspace-roots)
- [ACP Additional Directories RFD](https://agentclientprotocol.com/rfds/additional-directories)
- [ACP Tool Calls and Permission Requests](https://agentclientprotocol.com/protocol/v1/tool-calls)
- [ACP Client File System Methods](https://agentclientprotocol.com/protocol/v1/file-system)
- [`Multi-window-agent-pane.md`](./Multi-window-agent-pane.md)
- [`WTA-terminal-action-proposals.md`](./WTA-terminal-action-proposals.md)
- [`security-model.md`](../security-model.md)
