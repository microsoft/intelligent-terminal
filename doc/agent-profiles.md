# Native agent profiles

Intelligent Terminal discovers supported native agent CLIs installed on the
Windows host and exposes them as individual entries in the new-tab dropdown,
alongside shell profiles, with each provider's own icon. No agent folder is
created. These profiles can be edited, copied, hidden, launched in tabs
or panes, and selected with `wt -p`, just like a shell profile.

The supported providers are GitHub Copilot, Claude, Codex, Gemini, OpenCode,
and Google Antigravity.
WSL installations are not automatically discovered. Configured custom agents
with an explicit interactive command also generate profiles, as described below.
Native profile discovery uses this dedicated provider list, not the broader
ACP or delegation provider registry.
Discovery does not install software, sign in, or start an ACP server. Native CLI
availability is independent of the adapters needed by the built-in agent pane.

Antigravity profiles discover the interactive `agy.exe` installed by the
[official CLI installer](https://antigravity.google/docs/cli/install), not an
`antigravity.exe` or the separate `agy_acp_server.exe`. The profile's stable
provider ID is `antigravity`. An ACP-only installation does not generate a
native profile; an `agy` installation does not need the ACP server to do so.

The generated profile icons use transparent SVG artwork. Copilot, Claude,
Codex, OpenCode, and Antigravity use dark artwork on light surfaces and their original
artwork on dark surfaces. Menus render the artwork without foreground tinting,
including hover and pressed states. Gemini and custom SVG icons retain their
original colors.

## Launch and configuration

A managed profile starts the native interactive CLI directly in a normal
ConPTY. Native executables are launched by Terminal; npm `.cmd` shims use the
Windows system `cmd.exe` with AutoRun and delayed expansion disabled. Neither
discovery nor profile launch runs WTA. Input, output, environment, working
directory, and process lifetime follow the ordinary Terminal connection path.
This does not draw a chat interface or connect the CLI through the ACP master.

Edit the profile in Settings. Existing starting-directory, environment,
appearance, elevation, and close-on-exit settings still apply. Fonts are
rendered by Terminal; an agent's explicit terminal colors may override colors
from the Terminal palette. Agent themes are not automatically synchronized.

The profile uses these additional, flat JSON properties:

```jsonc
{
    "name": "Claude - Project",
    "agentProfile.id": "claude",
    "agentProfile.model": "",
    "agentProfile.permissionMode": "",
    "agentProfile.arguments": "",
    "startingDirectory": "C:\\source\\project",
    "defaultSplitProfile": "default"
}
```

For generated profiles, keep their generated `guid` and `source` properties.
Agent identity does not depend on the profile's display name. User overrides
are layered on the generated settings and are retained if a CLI disappears.
Copy a profile to create multiple project configurations for the same agent.

An empty model or permission mode means no launcher override: the CLI keeps
its own configured default. Permission choices use provider-specific native
modes, not a cross-provider promise of read-only access. Approval policies and
agent sandbox options do not grant or revoke Windows filesystem permissions.
Unsupported modes and conflicting arguments produce startup errors.

The profile editor exposes model and additional startup arguments, but does not
include a permission-mode selector. Configure approvals through the native CLI;
existing `agentProfile.permissionMode` JSON values remain supported.
For Antigravity, `accept-edits` and `plan` map to `agy --mode`; an empty value
preserves the CLI's configured default. The managed launcher does not accept
`--dangerously-skip-permissions` as an additional argument.
The agent profile's Advanced page exposes only close-on-exit behavior and
environment-variable refresh. It omits the separate ACP agent-pane and
command-palette delegation selectors and the other terminal fine-tuning
controls; those settings remain available through JSON. Ordinary shell pages
retain their existing controls and selectors.
The Run as administrator control is also omitted from agent profile pages.
Shell prompt marks, automatic prompt marking, shell cursor repositioning, and
rainbow input suggestions are omitted from agent profile pages, including
copies and profiles with an edited command line. Ordinary shell profile pages
and existing JSON settings are unchanged.

Additional arguments use Windows command-line quoting, for example an argument
containing a space must be quoted. They are arguments, not PowerShell or command
prompt script text. Supported extras are conservatively allowlisted per provider;
see the [native argument contract](../tools/wta/README.md) for the supported arguments.
Terminal applies the same provider-specific flags and argument allowlist before
starting a managed profile.
Managed commands reject `%` environment references to preserve the validated
arguments. Batch shims additionally reject embedded quotes, `!`, `^`, and line
breaks; use a custom command line when shell expansion is required.
Organization policy remains applicable to managed launches.
Do not put credentials in command-line arguments.

`AllowedAgents` is checked again when launching. If `AllowAutomaticApproval` is
disabled, all managed native launches are blocked, including those using the
CLI's default mode. Native CLIs can change approvals through their own
configuration and interactive controls, so a startup-only launcher cannot
guarantee the policy throughout the session. Settings displays this restriction.

The command-line field is directly editable, just like a shell profile. Saving
an explicit `commandline` runs that command unchanged by the agent command
builder; structured model, permission, and additional-argument settings no
longer apply and are hidden in Settings. Reset the command-line setting to
restore generated native commands and the structured controls. Simply opening
Settings or leaving the displayed command unchanged does not create an override.
Copying a generated profile retains its structured settings; copying an edited
profile retains its explicit command. Both copies retain their agent identity,
including settings inherited from the generated parent. Editing the command
does not turn the profile into a shell profile or remove its native provider
metadata. An explicit command supplied in a new-tab
action also overrides the managed launch, following ordinary Terminal action
precedence. An edited command is not a managed launcher or an operating-system
security boundary.

## Custom agents

In **Settings > AI agents**, add or edit a custom agent and provide its
interactive CLI command to enable a generated terminal profile. ACP startup
remains a separate, optional command: Terminal never removes `--acp` or other
arguments to guess an interactive invocation. A CLI-only definition creates a
profile without selecting an unusable ACP backend. Editing an existing legacy
custom ACP entry promotes that entry without changing its provider ID.
Legacy entries whose executable-derived IDs contain spaces or non-ASCII
characters remain editable as ACP-only commands; add a new custom definition
to configure their interactive startup and resume commands. The editor generates
the stable ID internally and does not display it.
Existing custom delegation commands remain independent.

The corresponding global setting is:

```json
{
    "customAgents": [
        {
            "id": "custom:work-agent",
            "name": "Work Agent",
            "acp": { "command": "adapter.exe", "args": ["--stdio"] },
            "cli": { "command": "work.exe", "args": ["--config", "C:\\work\\agent.json"] },
            "resume": { "command": "work.exe", "args": ["resume", "{sessionId}"] }
        }
    ]
}
```

Specify at least `acp` or `cli`. The `id` must start with `custom:` followed by
ASCII letters, digits, `.`, `_`, or `-`; IDs are unique without regard to case.
Keep it unchanged when editing a name or command, since generated profile GUIDs
and session ownership use it. New entries in Settings receive a persistent ID.
Different definitions can use the same executable without sharing identity.
Configuration changes are not delayed by the built-in discovery cache.

Custom agents with an ACP command participate in automatic session-history
discovery at WTA startup and on explicit history refresh, even when another agent
is selected in the agent pane. `AllowCustomAgents` policy applies before discovery.
The provider must support ACP `session/list` and expose the desired CLI history
through it; configuring a resume command does not itself discover sessions.
CLI-only definitions are not started for history discovery. No interactive login,
chat session, or prompt is started by discovery.

Launching the generated profile identifies the pane as that custom agent.
When the profile runs the CLI directly and the pane closes on exit, ordinary
pane removal also removes it from the active Agent list. This tracks the CLI
process, not individual conversation sessions. Retained exited panes and
persistent shell wrappers do not provide the same lifecycle guarantee.
Typing a custom CLI into an existing shell does not automatically identify it;
custom title rules and session hooks are not part of this support.

ACP history remains provider-defined. Some providers, including Qwen Code,
return only the ACP process's current-workspace history when `session/list`
omits `cwd`. Automatic discovery does not enumerate other workspaces or scan
custom providers' history files, so an empty result does not establish that
the provider has no sessions elsewhere.

Commands are an executable plus literal argument arrays, not shell scripts.
Use an explicit launcher such as `pwsh.exe -File` for a script.
Use the actual Windows launcher filename (for example, `qwen.cmd`, not the
shell-resolved `qwen`); profile launches do not resolve PowerShell aliases.
Custom `.cmd`/`.bat` ACP, interactive startup, and resume require an adjacent same-named `.ps1`
companion and invoke that exact companion through PowerShell, without inserting
arguments into `cmd` text. Missing executables or companions fail at launch,
not during profile generation. `%` environment expansion is not supported in
managed custom CLI commands. Do not put secrets in arguments.
Batch companions reject empty arguments, embedded double quotes, and arguments
containing whitespace that end in a backslash because Windows PowerShell cannot
preserve them when forwarding to a native process. Use a native executable
invocation when these values are needed.
Companion launches set execution policy only for their PowerShell child process
so the default Restricted policy does not block installed shims. Persistent policy
settings are unchanged; higher-priority MachinePolicy and UserPolicy still apply.

`resume` requires `cli` and exactly one complete `{sessionId}` argument.
The resume invocation is independent of the startup invocation; repeat any
configuration arguments it needs. IDs must satisfy the CLI-resume safe-token
contract; leading option tokens, whitespace, and shell syntax are rejected.
Custom profile controls do not invent built-in model, permission, or additional
argument flags. Configure those directly in the custom invocations.

Live sessions focus their existing pane. Historical agent-pane sessions use
that provider's ACP `session/load`; native CLI sessions use its configured
resume invocation. Listing sessions does not prove ACP load support, and a
failed ACP load never silently falls back to a different agent or CLI.
`resume.canResumeAcpSessions` defaults to `false`; it declares shared storage
but does not change Windows-host agent-pane routing. This initial custom
invocation support is Windows-host-only, including resume; WSL custom resume
is rejected explicitly.

For an explicit native resume, use:

```powershell
wt new-tab --native-agent-provider custom:work-agent --custom-agent-session SESSION_ID -d C:\work
```

This conversation ID is separate from Terminal's `--sessionId` buffer identity.
Saved native layouts retain the provider, known conversation ID, and execution
source, then resolve the current command and policy on restore. A saved non-host
custom source is rejected before resolving a Windows invocation; it is never
reinterpreted as a host conversation with the same ID. Older host layouts without
explicit source metadata remain compatible. Without a known conversation
ID, reopening a custom profile only starts its CLI. Custom hooks and arbitrary
history-file scanners are not added. Removing a definition leaves existing
history intact but prevents new resumes through that missing definition.
Master resolves invocations by ID from the current user `settings.json` through
the host's existing settings service; helpers and history rows never supply
trusted launch commands. Store structured custom definitions in user settings,
not settings fragments.

`AllowCustomAgents` governs custom profiles instead of the built-in
`AllowedAgents` list. Managed custom launches and CLI resumes also honor
`AllowAutomaticApproval`. Explicit profile command overrides retain ordinary
Terminal behavior, as with built-in profiles.

## Splitting panes

The default split shortcuts and tab split action use `splitMode: "profile"`.
Configure the source profile's `defaultSplitProfile` in `settings.json`; there
is no default split target control in the profile editor. It determines the target:

| Value | Result |
| --- | --- |
| Empty or omitted | Another instance of the current profile |
| `"default"` | The global default profile |
| A profile GUID, including braces | That profile |

An explicit `splitMode: "duplicate"` always starts another instance of the
current profile. An action specifying a profile, profile index, command, or
another content type keeps its explicit target. Existing user-defined actions
are not rewritten.

All new-terminal splits, including Alt-click profile selections and protocol
splits, inherit the source pane's working directory unless the operation supplies
an explicit directory. Shell-to-shell, shell-to-agent, agent-to-shell and
agent-to-agent splits follow the same rule. Native agents use their current
pane-bound session directory when available; otherwise their resolved launch
directory (including relative `-d` arguments), not subsequently edited settings.
Splitting while the built-in assistant is focused splits its source terminal,
leaving the assistant panel and conversation intact.
Unavailable or invalid directories fall back to the target profile's starting
directory. POSIX directories are inherited only between matching WSL launch
namespaces; no Windows/WSL or cross-distro path conversion is guessed.
Source identity comes from the running connection, not reloaded profile settings.
Commands are expanded before WSL validation; unknown source identities do not
inherit POSIX paths. WSL launches without an explicit distribution selector use
the target's normal startup directory instead, because the default distribution
may have changed since the source started.
Saved-layout, existing-content and session-resume arguments remain unchanged.
New-tab startup behavior is unchanged. A hidden,
missing, or invalid split target produces a warning and falls back to the global
default profile. The target is resolved once, not recursively through another
profile's split preference.

Splitting an agent profile starts a new CLI instance. It does not clone chat
history or automatically resume the source session. Moving a pane keeps its
running process. Managed layout restoration resolves the launcher again instead
of persisting a package-version-specific executable path. If the CLI has been
removed and the profile is orphaned, restoration recovers its native provider
identity and reports the unavailable CLI instead of opening a default shell.
Invalid or mismatched saved provider identities fail explicitly. Explicit
command-line overrides retain ordinary Terminal launch precedence.

## Discovery and lifetime

Discovery checks native CLI files directly during settings loading and Settings
Extensions enumeration, not each time the new-tab menu opens. It reads the fresh
Windows environment PATH and retains process-only PATH entries, preferring
`.exe` over `.cmd` across directories. It does not depend on `wta.exe`.
Windows app execution aliases, such as those registered in `WindowsApps`, are
recognized as launchable entries even when ordinary file-status queries cannot
follow them. The alias path is preserved so Windows can resolve the registered
application when launching the profile.
Successful results are cached for 30 seconds. After installing or removing a
CLI, reload settings after that interval or restart Terminal. There is no
30-second polling timer. A failed filesystem probe is
logged and retains the last successful in-process snapshot rather than treating
the failure as an uninstall. A failed refresh does not renew the cache TTL, so
the next discovery request retries. With no successful snapshot, the first
probe failure propagates to the settings loader.

Removing a CLI stops generation after the next successful uncached discovery.
Its saved profile becomes orphaned and leaves the active profile list, while
retaining user overrides. Reinstalling restores the same generated GUID and
settings. Deleting a generated profile and saving settings suppresses it through
Terminal's generated-profile state; rediscovery does not make it visible again.
Managed launches resolve the executable again at connection creation, so a stale
menu entry cannot run an unavailable CLI.

The default new-tab menu lists ordinary profiles first and automatically generated
agent profiles second, with a separator only when both groups have visible,
available profiles. Each group preserves the existing profile order and shortcuts.
The default layout means an omitted `newTabMenu` or exactly one
`{ "type": "remainingProfiles" }` entry. Grouping affects only the displayed menu
and does not rewrite `settings.json`. User-created copies without the generated
agent source stay in the ordinary group.

Agent profiles can be reordered through the new-tab menu settings or hidden
individually. Any other `newTabMenu` layout, including an empty list, folders,
explicit profiles, or separators, is rendered as configured without automatic
grouping or separators. Profiles also respect disabled profile sources and the
agent allowlist.

There is no WTA or hidden PowerShell process beneath a managed agent. The native CLI
decides what Ctrl+C means during interaction. When it exits, Terminal observes its
exit status and the pane follows `closeOnExit`; startup failures are not converted
into successful exits.

Existing per-tab agent-pane prewarming and AI-assistant toggling remain
independent of these native CLI profiles. Existing hook-based native-session
tracking is reused where supported; the launcher does not create a second ACP
session for the same CLI.
