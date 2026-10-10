# Native agent profiles

Intelligent Terminal discovers supported native agent CLIs installed on the
Windows host and exposes them as individual entries in the new-tab dropdown,
alongside shell profiles, with each provider's own icon. No agent folder is
created. These profiles can be edited, copied, hidden, launched in tabs
or panes, and selected with `wt -p`, just like a shell profile.

The initial providers are GitHub Copilot, Claude, Codex, Gemini, and OpenCode.
WSL installations and arbitrary custom agents are not automatically generated.
Native profile discovery uses this dedicated provider list, not the broader
ACP or delegation provider registry.
Discovery does not install software, sign in, or start an ACP server. Native CLI
availability is independent of the adapters needed by the built-in agent pane.

The generated profile icons use transparent SVG artwork. The new-tab menu
renders the SVG's original colors without foreground tinting, preserving them
when hovering, pressing, or switching between light and dark themes.

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
The separate ACP agent-pane and command-palette delegation selections are in
the agent profile's Advanced page and do not select or configure its native CLI.
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

When splitting into a configured target, an explicitly supplied directory wins.
Otherwise a valid Windows directory reported by the current terminal is used;
if none is available, the target profile's starting directory applies. A hidden,
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

Agent profiles use the ordinary profile menu mechanism and can be reordered
through the new-tab menu settings or hidden individually. Existing custom menu
layouts are preserved. Profiles also respect disabled profile sources and the
agent allowlist.

There is no WTA or hidden PowerShell process beneath a managed agent. The native CLI
decides what Ctrl+C means during interaction. When it exits, Terminal observes its
exit status and the pane follows `closeOnExit`; startup failures are not converted
into successful exits.

Existing per-tab agent-pane prewarming and AI-assistant toggling remain
independent of these native CLI profiles. Existing hook-based native-session
tracking is reused where supported; the launcher does not create a second ACP
session for the same CLI.
