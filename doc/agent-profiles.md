# Native agent profiles

Intelligent Terminal discovers supported native agent CLIs installed on the
Windows host and exposes them as individual entries in the new-tab dropdown,
alongside shell profiles, with each provider's own icon. No agent folder is
created. These profiles can be edited, copied, hidden, launched in tabs
or panes, and selected with `wt -p`, just like a shell profile.

The initial providers are GitHub Copilot, Claude, Codex, Gemini, and OpenCode.
WSL installations and arbitrary custom agents are not automatically generated.
Discovery does not install software, sign in, or start an ACP server. Native CLI
availability is independent of the adapters needed by the built-in agent pane.

## Launch and configuration

A managed profile starts the application's `wta launch-agent` command in a
normal ConPTY. WTA starts the native interactive CLI with inherited input,
output, environment, and working directory. It does not draw a chat interface
or connect this CLI through the ACP master.

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
    "agentProfile.customCommand": false,
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

Additional arguments use Windows command-line quoting, for example an argument
containing a space must be quoted. They are arguments, not PowerShell or command
prompt script text. Supported extras are conservatively allowlisted per provider;
see the [launcher contract](../tools/wta/README.md) for the supported arguments.
Organization policy remains applicable to managed launches.
Do not put credentials in command-line arguments.

`AllowedAgents` is checked again when launching. If `AllowAutomaticApproval` is
disabled, all managed native launches are blocked, including those using the
CLI's default mode. Native CLIs can change approvals through their own
configuration and interactive controls, so a startup-only launcher cannot
guarantee the policy throughout the session. Settings displays this restriction.

**Use a custom command line** switches the profile to its normal `commandline`
setting. The structured model, permission, and additional-argument settings no
longer apply. An explicit command supplied in a new-tab action also overrides
the managed launch, following ordinary Terminal action precedence. A custom
command is not a managed launcher or an operating-system security boundary.

## Splitting panes

The default split shortcuts and tab split action use `splitMode: "profile"`.
The source profile's `defaultSplitProfile` determines the target:

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
of persisting a package-version-specific executable path.

## Discovery and lifetime

Discovery runs during settings loading, not each time the new-tab menu opens.
Successful results are cached for 30 seconds. After installing or removing a
CLI, reload settings after that interval or restart Terminal. A failed probe is
logged and retains the last successful in-process snapshot rather than treating
the failure as an uninstall.

Agent profiles use the ordinary profile menu mechanism and can be reordered
through the new-tab menu settings or hidden individually. Existing custom menu
layouts are preserved. Profiles also respect disabled profile sources and the
agent allowlist.

There is no hidden PowerShell process beneath a managed agent. The native CLI
decides what Ctrl+C means during interaction. When it exits, WTA returns its exit
status and the pane follows `closeOnExit`; startup failures are not converted
into successful exits.

Existing per-tab agent-pane prewarming and AI-assistant toggling remain
independent of these native CLI profiles. Existing hook-based native-session
tracking is reused where supported; the launcher does not create a second ACP
session for the same CLI.
