# Custom agent profiles and session resume

Status: Implemented for Windows-host custom definitions in user settings.
See [native agent profiles](../agent-profiles.md#custom-agents) for setup.

## Goals

1. Generate an ordinary terminal profile for a configured custom interactive CLI.
2. Let users configure interactive startup independently of ACP startup.
3. Resume a custom session using its provider's configured invocation or ACP
   `session/load`, without confusing providers or execution sources.
4. Preserve existing custom ACP commands, generated-profile overrides, and
   built-in behavior.

## Baseline before this change

- `AgentProfileGenerator.cpp` generates profiles only for
  `AgentRegistry::BuiltinNativeProfileAgents`. Discovery checks Windows host
  executables and does not start WTA or an ACP process.
- `AgentProfileUtils.h` builds and validates native commands using built-in
  provider-specific rules. Adding a custom ID to the generator alone would not
  make that profile launchable.
- Custom ACP and delegate definitions are separate command-string collections:
  `acpCustomCommands` and `delegateCustomCommands`, plus the legacy singular
  settings. Settings derives `custom:<name>` from the executable filename.
  Commands sharing a launcher such as `npx` can therefore share an ID.
- `TerminalPage::_ResolveAgentCliPathForId` uses a custom ACP command unchanged.
  It does not append the built-in ACP flags to a custom command.
- Master can discover custom history through ACP `session/list`, retaining the
  resolved custom provider ID. Listing does not prove resume support.
- `CliSource::from_agent_id` recognizes only built-ins, and
  `session_mgmt::decide_enter_action` rejects `CliSource::Unknown`.
  Master's activation path also rejects providers resolving to the default
  static profile. Both activation paths need runtime custom-provider metadata.
- Shell-pane layout restore still has a built-in resume-invocation table in
  `AgentPaneRestore.h`; it cannot infer arbitrary custom resume syntax.
- `wta sessions activate` already accepts a qualified session identity and an
  owning window. A new public resume subcommand is not required for this work.

## Recommended configuration

Introduce an optional `customAgents` collection. Each entry has a persistent,
explicit ID and independent launch descriptions:

```json
{
    "customAgents": [
        {
            "id": "custom:work-agent",
            "name": "Work Agent",
            "acp": {
                "command": "work-agent.exe",
                "args": ["--config", "C:\\agent-config\\work.json", "--acp"]
            },
            "cli": {
                "command": "work-agent.exe",
                "args": ["--config", "C:\\agent-config\\work.json"]
            },
            "resume": {
                "command": "work-agent.exe",
                "args": [
                    "--config",
                    "C:\\agent-config\\work.json",
                    "resume",
                    "{sessionId}"
                ],
                "canResumeAcpSessions": false
            }
        }
    ]
}
```

These names are supported by the settings model, schema, and custom-agent editor.

`command` is one executable or launcher path, not a shell command string.
`args` is an ordered array of literal arguments. The complete resume invocation
is specified independently; it does not implicitly append to `cli.args`.
This handles different executable names, adapters, subcommands, and argument
ordering without another provider-specific implementation.

| Field | Meaning |
| --- | --- |
| `id` | Unique, stable `custom:<name>` identity, independent of command and display name |
| `name` | Display name; changing it does not change profile or history identity |
| `acp` | Optional long-running ACP stdio invocation for the agent pane |
| `cli` | Optional interactive invocation; its presence enables automatic profile generation |
| `resume` | Optional interactive invocation for one exact historical session |
| `resume.canResumeAcpSessions` | Explicit shared-store compatibility claim; default `false` |

Require at least one of `acp` or `cli`. A resume-only entry is invalid.
Do not infer delegation prompt syntax, model flags, approval flags, hooks, or
historical listing from these commands. Existing delegation remains unchanged.

An ACP-only entry remains supported but does not generate a terminal profile:
an ACP server is not a terminal UI. Existing users see an optional "Interactive
CLI command" field explaining that filling it enables a profile.

### Do not strip ACP flags

Never implement interactive startup by removing `--acp`, `--stdio`, or an `acp`
subcommand. For example, Claude's configured ACP invocation can be an adapter,
whereas its interactive invocation is `claude`. Antigravity uses separate
executables. A wrapper may encode ACP mode without any visible flag.

Users specify the interactive invocation explicitly. Built-ins retain their
existing mappings. The Settings concept can be "Custom agent" with an optional
ACP section; ACP remains necessary for agent-pane use, not for a native profile.

## Settings experience and compatibility

Provide one custom-agent editor with:

- Display name.
- Optional ACP startup command.
- Optional interactive CLI startup command.
- Optional resume command, whose header documents the `{sessionId}` argument.
- Interactive command previews; validation and profile generation never execute
  the CLI or resume invocation.

The editor generates persistent IDs internally and does not display them.
Shared ACP/CLI session-store compatibility is an advanced JSON-only setting,
off by default. Editing preserves existing compatibility metadata; clearing
the resume command also clears that compatibility claim.

The UI may accept familiar command-line text, but must parse it into the
structured representation and report invalid quoting. It must not silently
treat shell syntax as arguments or execute the CLI or resume invocation while
saving settings. Existing ACP model probing remains a separate operation.
Advanced users can configure explicit wrappers when necessary.

Normalize old and new settings into one runtime catalog:

1. Existing singular/plural ACP and delegate settings continue working.
2. Preserve each legacy custom ID during normalization. Do not rename a saved
   provider or merge ACP and delegate entries merely because their launchers
   have the same filename.
3. Legacy ACP entries remain ACP-only until an interactive command is supplied.
   Do not guess a CLI by stripping flags. Existing delegate entries retain their
   existing behavior; converting one into a profile is an explicit edit.
4. Editing a legacy ACP entry can promote it to `customAgents`, retaining its
   ID and removing only the legacy entry it replaces. Legacy IDs outside the
   structured ID grammar remain editable as ACP-only entries; add a new
   definition to configure their interactive commands. Do not rewrite the
   whole configuration merely because Settings was opened.
5. An explicit structured entry takes precedence over its legacy representation.
   Reject duplicate explicit IDs. Legacy executable-derived collisions retain
   their previous behavior until the user assigns separate explicit definitions.

New IDs are generated once when adding an entry and remain unchanged when its
command or name changes. Multiple agents using `npx.exe` or `pwsh.exe` must be
representable independently.

## Automatic profile generation

Reuse `IntelligentTerminal.AgentProfiles` and the existing dynamic-profile
layering, hidden/deleted-profile suppression, and disabled-source mechanism.

- Generate one profile per allowed custom entry with a valid `cli` description.
  Being configured is sufficient; do not require every wrapper dependency to
  pass a speculative installation check. The command may be a user script.
- Use the generator namespace plus the persistent custom ID for the stable
  profile GUID. Never derive it from the display name or command.
- Store `agentProfile.id = custom:<name>` and use a generic agent icon, with
  ordinary profile icon overrides available.
- Resolve the launch description from the effective settings catalog when
  launching; do not freeze it into a user-owned `commandline` override.
- Reuse ordinary starting-directory, environment, appearance, and explicit
  commandline precedence. An explicit override remains a user command.
- Do not apply built-in native model/approval/argument allowlists to an
  arbitrary CLI, or assume it accepts `--model`. Hide unsupported structured
  controls and let its configured arguments express its behavior.
- Missing executables produce an actionable launch error, not a default shell.
  Removing the definition orphans the generated profile while preserving its
  overrides; restoring the same ID restores them.
- Continue checking custom-agent and managed-launch approval policy at launch.
  Do not weaken built-in restrictions while adding the custom launch path.

The settings loader supplies the generator with the configured custom
definitions. Generation and Extensions enumeration use that configuration.
Structured custom definitions belong in user settings, not fragments. The existing
30-second executable-discovery cache must not delay configuration edits.
Profile generation must not initialize ACP, execute a wrapper, or probe models.

### Profile identity and lifetime

The generated profile supplies the custom provider identity at pane creation.
For direct CLI launches whose pane closes on exit, normal pane removal completes
the active Agent-list lifecycle. No additional title matching is required.
This identifies the CLI process, not individual conversations. Retained exited
panes, persistent shell wrappers, and custom CLIs typed into existing shells are
outside this lifecycle guarantee; this change does not add generic title rules
or custom session hooks.

## Session discovery

At master startup and on explicit history rescan, include every configured
Windows-host custom ACP provider, not just the agent selected in the agent pane.
Read commands only from trusted Terminal settings and check `AllowCustomAgents`
before reading or starting custom providers. Include structured definitions and
legacy ACP entries, preserving their identities and structured-definition
precedence. Do not start CLI-only entries or infer ACP commands from their CLI.

Use the existing provider-qualified pool, ACP capability checks, history merge,
five-second refresh workers, and bounded startup admission. Settings lookup
failures retry separately with delays capped at 60 seconds. Distinct custom identities
remain separate even if their executables or raw session IDs match. No chat
session, prompt, or interactive login is created. Settings lookup failures are
logged and retried without discarding existing rows or blocking other providers'
history. Discovery can only list histories the provider exposes through ACP.
In particular, Qwen Code scopes an omitted `session/list` cwd to its process
working directory. This change does not add cross-workspace enumeration or a
Qwen-specific filesystem scanner.

## Session resume

### Routing contract

| Selected session | Behavior |
| --- | --- |
| Bound live pane | Focus that pane; no resume command is needed |
| Agent-pane session | Use the same provider/source and ACP `session/load` |
| CLI history session | Use that provider's configured `resume` invocation in a new terminal tab |
| Custom WSL session without a live bound pane | Reject explicitly; this scope supports host invocations only |
| Missing definition or required capability | Keep the row visible and explain why resume is unavailable |

Preserve the existing origin-sensitive behavior. `session/list` does not imply
`session/load`, nor that CLI and ACP session IDs refer to the same store.
Do not automatically fall back from a failed ACP load to a CLI command.

For a custom ACP listing that cannot establish CLI-store compatibility, listing
alone is insufficient to enable CLI resume. The custom resume configuration is
the user's explicit mapping contract and must be described as such in Settings.

Introduce a distinct `CliSource::Custom` identity, or an equivalent typed
provider reference backed by the runtime catalog. Do not make every
`Unknown(String)` launchable. Preserve full custom IDs through history,
serialization, filters, routing, and layout persistence.

Qualify identity by provider, execution source/distro, session universe where
present, and raw session ID. Two providers returning the same raw ID must never
resume one another's conversation. Do not select the global default agent to
resume a row belonging to a different provider.

### Resume arguments, not another generic flag

Different CLIs need different grammar:

```text
["--resume", "{sessionId}"]
["resume", "{sessionId}"]
["--session", "{sessionId}"]
["conversation", "open", "--id", "{sessionId}"]
```

Require exactly one complete `{sessionId}` argument in a configured resume
invocation. Reject missing, duplicate, embedded, and unknown placeholders.
Initially support no general template language: cwd is a process launch
property, not text substituted into a shell command.

Substitute the selected session ID as one argument before platform-specific
quoting. Reuse the existing bounded safe-token validation at CLI resume
boundaries; leave ACP-only identifiers opaque. Do not broaden that contract
incidentally. Reject unsupported IDs explicitly.

Native executables use argument-vector quoting. Batch or script wrappers must
use a reviewed existing launcher path or fail explicitly; no unsafe `cmd`
string-interpolation fallback. Apply the existing source validation and launch
quoting to WSL, and never fall back to the Windows host for a WSL session.
New custom profiles in this first scope target Windows host commands;
automatic WSL custom profile discovery is separate work.
For custom OSC 9001 Resume bindings, an explicit owning-control WSL source
overrides the existing binding; otherwise preserve the qualified backend for
the same provider and session, falling back to host only for a new binding.
A default host shell must not downgrade known WSL provenance. Publish that
same backend (and WSL distro when present) with the session birth event, and
retain it through persistence so unsupported custom WSL restores still fail
explicitly rather than launching a Windows command.

### Configuration delivery and shared implementation

The Terminal settings layer owns custom definitions. Master reads current user
settings through the existing host-owned `get_settings` service and rechecks
custom-agent policy before selecting a command. A helper supplies a canonical
ID, never a trusted executable. Removed structured definitions fail explicitly.
Legacy default commands remain supported through their existing host-owned path.

Use the same capability resolver and resume invocation builder for helper
session management and master sidebar activation. A per-helper resume flag is
insufficient: the sidebar can activate another provider, and master manages
multiple providers concurrently.

Keep command descriptions out of session-list responses and telemetry.
Custom providers remain bucketed as `custom` in telemetry. Do not place
credentials in startup arguments. Existing process diagnostics can include
launcher information and must not be treated as a secret store.

### Window/layout restoration

Persist custom provider ID, cwd, and the known CLI session ID
as explicit restore metadata. Re-resolve current policy and configuration on
restore. Do not parse arbitrary custom commands to rediscover identity or
persist stale package paths/master pipe names.

`NewTerminalArgs.customAgentSessionId` holds the conversation ID independently
of Terminal's buffer `sessionId`; `--custom-agent-session` carries it across
command-line/elevation boundaries alongside `--native-agent-provider`.
The execution source is Windows host in this initial scope.

A pane without a known agent session ID can relaunch its CLI, but cannot promise
conversation restoration. Custom hooks and arbitrary CLI history-file scanning
are not part of this proposal. Missing hooks must not break ACP chat, Autofix,
listing, or supported resume behavior.

## Implementation boundaries

1. Settings model/schema and custom editor: structured definitions, stable IDs,
   legacy compatibility, validation, and current user definitions.
2. Profile generator and launch utilities: custom profiles, Settings previews,
   policy, and explicit-command precedence without built-in argument assumptions.
3. WTA runtime provider catalog: typed custom identity, configuration updates,
   capabilities, and master-owned resume invocation construction.
4. Helper and master activation: catalog-based routing for both surfaces.
5. Layout persistence: explicit custom provider/session metadata and re-resolution.
6. Documentation/localization: custom-agent setup, native profiles, resume
   limitations, policy semantics, and user-facing errors.

Do not change built-in command mappings, default session filters, ACP process
prewarming, automatic approval behavior, or delegation prompt delivery as a
side effect.

## Acceptance criteria

- A configured CLI creates exactly one stable profile across reloads, command
  edits, and display-name edits; hiding/deleting it remains respected.
- Two custom agents sharing an executable remain separate. Legacy ACP-only
  definitions continue launching their original ACP command unchanged.
- Generation never executes user commands. Missing CLI executables are reported
  at launch, and removing/re-adding definitions preserves profile overrides.
- Resume argument tests cover flag and subcommand grammar, config arguments,
  paths with spaces, invalid placeholders, unsafe IDs, and supported wrappers.
- Helper and sidebar activation select the same provider, cwd, and source.
  Equal raw IDs from different providers or distros remain independent.
- Live sessions focus; unsupported listing/load/resume combinations produce
  explicit errors and never silently launch a different agent or a new session.
- Layout restore re-resolves current configuration and policy. CLI panes with
  no known session ID do not claim to restore a conversation.
- Custom-agent and approval policies cannot be bypassed by the new generated
  launch path. Telemetry does not contain configured commands.
