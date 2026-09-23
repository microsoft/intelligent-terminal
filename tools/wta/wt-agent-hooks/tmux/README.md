# Remote Linux hooks and managed SSH setup

`it-agent-hook.sh` is a thin **POSIX-shell transport**, not an agent-event
processor. It frames and forwards stdin **unchanged**, including malformed
JSON, conflicting IDs, prompts, tool arguments/output, Unicode, and CR/LF.
Intelligent Terminal (IT) reassembles the body and owns JSON parsing, identity
checks, child-session filtering, redaction/projection, and activity/status
processing. The sender never parses, truncates, or rewrites JSON.

Requirements: Linux, **tmux 3.4+**, `sh`, and GNU/coreutils-compatible `timeout`,
`mktemp`, `rm`, `rmdir`, `head`, `wc`, and `base64` (`--wrap=6000`). No remote
`wtcli`, WTA executable, Python, Node, or `jq` is required by the transport.
OpenCode's own plugin API naturally uses its existing JavaScript runtime.
Managed SSH routing also requires `stat` and `id`. The installer uses standard
Linux tools including `getent`, `flock`, `sha256sum`, `awk`, and `sed`; it never
installs a language runtime, an agent CLI, or a system package.

These prerequisites must be available **inside the CLI's hook execution
environment**, not merely on the host. A confined package can load the plugin
and read its script while lacking `tmux` or access to the host tmux socket.
For example, the tested Copilot Snap environment has no tmux executable and
cannot see the host's `/tmp/tmux-1000/default`; the sender then intentionally
no-ops. Successful plugin installation alone does not establish transport
readiness. Use a CLI environment with supported tmux access; do not bypass
package confinement to expose the host socket.

There are two independent routing modes. Managed `wta ssh` sessions use v3
outside tmux panes, through a dedicated control-mode side channel. Existing
manual tmux-pane integrations continue to use v2 when no managed SSH route is
present. Neither installation replaces managed local `wt-agent-hooks`
plugins. The matching Windows receiver is required.

The local receivers differ: v2 is handled by C++ and published through COM,
so `wtcli --json listen --event "agent.*"` observes it. V3 is read by the
master-owned background SSH client and updates the existing SSH source
registry directly; it does not appear in that COM listener.
For native v2 backends launched directly through SSH, the controller adds its
locally resolved SSH destination to the envelope. Master then merges those
live sessions into the same SSH source list as ordinary v3 sessions and remote
history. No new remote variables or hook registration are needed for this
source association. Non-SSH or opaque backends retain the Host tmux list.
`wta sessions list --master --ssh <target> --cli copilot --json` returns one
current state snapshot, not an event stream. For the reasoning and diagnostic
boundaries, see the [ordinary SSH specification](../../../../doc/specs/ordinary-ssh-agent-hooks.md).

## Automatically managed ordinary SSH sessions

The Windows wrapper supplies these variables to the foreground login shell or
remote command:

```text
IT_SSH_HOOK_ROUTE=<UUID registered by Windows for this SSH route>
IT_SSH_HOOK_SOCKET=<original-login-home>/.intelligent-terminal/run/tmux-hooks.sock
IT_SSH_HOOK_SESSION=it-hooks
```

The agent need not run inside a tmux pane. If `IT_SSH_HOOK_ROUTE` is present,
the sender selects v3 exclusively: an empty/malformed UUID or missing managed
channel never falls back to unrelated `TMUX`/`TMUX_PANE` context.

`install-remote-hooks.sh` owns automatic remote setup. Its interface is:

```sh
sh /private-upload/install-remote-hooks.sh \
  --hook-source /private-upload/it-agent-hook.sh \
  --login-home "$HOME" \
  --socket "$HOME/.intelligent-terminal/run/tmux-hooks.sock" \
  --session it-hooks \
  --allowed-clis claude,copilot,codex,gemini,opencode \
  --attach-control
```

`--hook-source` is required. The login home defaults to the effective login
user's passwd/NSS home; an explicit `--login-home` must match it. The socket
defaults to the path shown and cannot be redirected into another namespace.
The only session name is `it-hooks`. Omitting `--attach-control` performs
setup only. **Route UUIDs are not installer arguments and are never written
to installer diagnostics or configuration.**
`--allowed-clis` is a comma-separated list of canonical provider IDs; Windows
must pass its GPO-permitted subset. A missing or empty selection is a no-op,
not an implicit selection of all providers. Unknown or duplicate IDs are
rejected. `--agents` remains an equivalent
compatibility spelling. Unselected providers are not queried or changed. Each provider has its
own generation receipt, so selecting it after a previously skipped upgrade
still refreshes that provider without modifying another provider's settings.

The background SSH uploader streams the two bundled files with known byte
lengths into private staging, verifies both lengths, then invokes the
installer **by filename**, not by consuming its continued stdin as a script.
Leave SSH stdin open after the assets: installation subprocesses receive
`/dev/null`, and the final `exec tmux -N -S ... -C attach-session -t '=it-hooks'`
inherits the remaining stream. Setup writes diagnostics only to stderr.
Stdout becomes native tmux control protocol without a custom readiness banner.
Foreground-login failure handling and upload framing belong to the Windows
SSH wrapper/background channel, not this installer.

The dedicated server is created with explicit `-S` and `-f /dev/null`; it
does not load the user's tmux configuration or touch ordinary tmux servers,
sessions, or panes. A private idle session keeps it alive after individual
control clients disconnect. No disconnect handler kills the shared server.
Ownership, canonical login-home identity, private directory permissions, and
the Linux UNIX-socket path limit are checked. The installer does not fall back
to `/tmp` or another user's socket when these checks fail.

After native control attachment, `@it-ssh-hooks-protocol` must be `3`.
`@it-ssh-hooks-setup` reports `transport-ready` or `transport-partial`.
These are **transport/installation metadata, not proof that an agent has
executed a working hook**. Per-provider outcomes are emitted separately on
stderr, including `installed`, `not-found`, `user-disabled`, `user-removed`,
`foreign-plugin`, and explicit unsupported/error results.

For a nonce-correlated readiness query, write this command to the control
client's still-open stdin, with a caller-generated ASCII hexadecimal nonce:

```text
display-message -p 'IT_HOOK_READY/3 <nonce> #{@it-ssh-hooks-protocol} #{@it-ssh-hooks-installed-clis} #{@it-ssh-hooks-unavailable-clis}'
```

Unlike hook emission, this query deliberately expands managed options (no
`-l`). Its native `%begin`/`%end` response contains
`IT_HOOK_READY/3 <nonce> 3 <installed-cli-csv> <unavailable-cli-csv>`.
Each CSV contains only canonical provider IDs, or `-` when empty. Unavailable
means detected but not safely installed: it includes unsupported runtimes/
APIs, foreign or disabled/removed registrations, and installation failures.
Absent or policy-unselected CLIs are not reported as unavailable. Warn
neutrally; do not infer permission to re-enable plugins or bypass confinement.

Asynchronous `%message` hook lines can arrive while waiting; keep
demultiplexing them. The options are the latest shared-server setup snapshot,
not route authorization or proof of runtime readiness. Intersect capabilities
with local allowed agents and enforce live consent and active route identity.
The older `@it-hook-channel` summary remains available as
`3 ready <installed-csv>` or `3 partial <installed-csv>`, but the finite
installed/unavailable lists avoid needing to retain arbitrary remote stderr.

### Ownership, update, and provider policy

All installer-owned assets are under the real login user's
`~/.intelligent-terminal/ssh-hooks`, independently of a CLI's runtime `HOME`.
Generated commands use safely shell-quoted and JSON-escaped **absolute**
paths to `ssh-hooks/current/it-agent-hook.sh`; no generated command expands
`$HOME`. Immutable content-addressed releases include checksums and versioned
plugin manifests. A process-safe `flock` serializes setup; its descriptor is
not inherited by CLIs or the persistent control client.

Staging is atomic, a pending transaction records incomplete work, and the
installation manifest is committed last after registration/version checks.
An API failure after a side effect can be repaired on retry. Existing
operator-modified assets, foreign same-name plugins/marketplaces, and
user-disabled or user-removed registrations are not overwritten or
automatically re-enabled. No `--force`, blanket update/uninstall, credential
change, shell-RC edit, or sandbox-permission change is performed.

### Narrow migration of the earlier owned v2 Copilot bundle

The earlier direct Copilot registration named `it-tmux-hooks` is checked
before creating `it-ssh-hooks`, so setup does not silently install duplicate
bridges. Automatic migration is limited to this exact legacy source:

```text
<original-login-home>/.intelligent-terminal/plugins/it-tmux-hooks
```

The CLI must report an enabled, unqualified direct registration of version
`2.0.0` from that exact path. When an older Copilot version reports only
`source: "installed"`, the installer verifies the origin using its bounded,
user-owned configuration metadata (`COPILOT_HOME/config.json`, or the runtime
HOME's `.copilot/config.json`). JSONC comments and trailing commas are accepted
only for that configuration fallback; it is never printed or rewritten.
The CLI's status remains authoritative for enablement. Missing, malformed, or
ambiguous origin metadata does not authorize migration.

The source directories and files must be owned by the
login user, non-symlinked, and not group/world-writable. The `.it-managed` file
must equal `Intelligent Terminal managed remote hook bundle v2`, `plugin.json`
must declare the expected name/version, and `scripts/it-agent-hook.sh` must be
a readable valid v2-only shell script. A matching name alone is never proof
of ownership.

After these checks, the installer journals the transition, uses Copilot's
native uninstall API for the old registration, and verifies it disappeared
**before** installing the new managed registration. It then verifies the
new source/version/enablement and checks again that the old registration is
absent before reporting Copilot installed. It does not rewrite the old source
bundle in place. Interrupted unregister/install operations retain ownership
journals and are repairable without a duplicate-registration window.

Disabled legacy registrations, foreign/missing markers or sources, symlinks,
unexpected versions, already-modified v3 scripts, and ambiguous registrations
are not migrated. They produce an explicit conflict/unavailable outcome and
do not advertise Copilot as v3-installed. A legacy registration deliberately
re-added after a completed migration also requires operator resolution.
The regression suite exercises these cases using isolated CLI fakes; it does
not operate on a real user's authenticated Copilot configuration.

| Provider | Managed registration |
| --- | --- |
| Copilot | Owned local marketplace, `plugin install/update it-ssh-hooks@it-ssh-local`, and JSON state queries |
| Claude | Owned local marketplace plus plugin install/update and JSON state queries |
| Codex | The same explicit plugin/marketplace APIs when supplied by the installed CLI; otherwise an unsupported-API result |
| Gemini | `extensions install --consent --skip-settings`, named update, and `extensions list --output-format json` |
| OpenCode | Explicit `unsupported-registration-api`: its documented local-autoload mechanism has no CLI registration API, so setup does not edit its user configuration or plugin directory |

Managed plugins are named `it-ssh-hooks`; their private marketplace is
`it-ssh-local`. Gemini's manifest declares the route/socket/session/opt-out
environment names without persisting their per-session values. Unknown or
ambiguous management response schemas fail closed. CLIs must be available in
the bootstrap's PATH; no package installation or shell-profile mutation is
used to make a missing CLI appear.

Copilot uses the same marketplace-based installation pattern as the Windows
hook bundle:

```sh
copilot plugin marketplace add "$HOME/.intelligent-terminal/ssh-hooks/current/copilot"
copilot plugin install it-ssh-hooks@it-ssh-local
```

These illustrate the installer's CLI calls, not a replacement for its ownership
checks. The marketplace is local to the remote machine: Windows still uploads
the packaged installer and sender over SSH, and the installer generates the
marketplace files there. No marketplace download or direct-path plugin install
is used. Updates refresh the owned marketplace and use the qualified plugin
identity.

Existing IT-owned direct `it-ssh-hooks` registrations are migrated through the
CLI only after validating the managed files, receipt, source, version, and
enablement. The direct registration is removed and verified absent before
adding the marketplace; otherwise Copilot can resolve an unqualified uninstall
to the new live plugin instead. Pending receipts make interrupted migrations
repairable. Foreign or disabled registrations are not migrated.

Copilot lists plugins from a newly added local marketplace as disabled even
before they have been installed. Only a pending managed installation with no
explicit `enabledPlugins` entry in the bounded, read-only Copilot
`settings.json` may enable such a catalog entry. Explicit user disablement is
preserved, including after an interrupted marketplace registration. Copilot's
separate `marketplace` identity field and `Local: ` source prefix are verified
rather than assuming its listing matches the other CLIs.

For Snap launchers, the installer uses `snap run --shell` to probe the actual
confinement. It checks script readability, required utility behavior, socket
ownership/server identity, and a real `source-file -` round trip. It does not
infer capability from the package type alone or expose host sockets by
bypassing confinement. Native launch environments are also probed, but a
wrapper may further alter its hook environment; an actual received hook
remains the final readiness evidence. First install or update still requires
an agent restart before new hook registrations become active.

The installer parses CLI management responses and, when necessary, read-only
registration metadata to verify ownership, effective enablement, and the
installed version. It does not parse agent stdin or make agent activity/status
decisions.

`WTA_TMUX_HOOKS_DISABLED` disables both setup and sending and is a durable,
explicit per-process opt-out (the wrapper's `--no-hooks` case). Do not set it
merely because Session Management was disabled at startup: Windows keeps the
registered foreground route context so a later live-consent off-to-on change
can start setup without reopening the SSH shell. Bootstrap must re-check the
effective `agentSessionManagementEnabled` boolean and an opt-in registered
live target on register/reconnect/enable transitions. Setup failure must not
prevent an ordinary SSH login.
Unsupported providers do not prevent the side channel from serving supported
or manually configured providers, but they are never reported as installed.

For operator removal, disable management first and use the CLI's own disable/
uninstall commands for **`it-ssh-hooks`** (qualified with `@it-ssh-local` where
required). Do not remove unrelated `wt-agent-hooks` or manual `it-tmux-hooks`
plugins. The installer respects disabled/removed registrations on later runs.

## Manual v2 installation: copy just the shell file

Copy `it-agent-hook.sh` to the remote host. This example normalizes Windows
checkout line endings and refuses to overwrite an existing installed file:

```sh
tmux -V
destination="$HOME/.local/lib/intelligent-terminal/it-agent-hook.sh"
mkdir -p -- "${destination%/*}"
if ! (umask 077; set -C; sed 's/\r$//' ./it-agent-hook.sh >"$destination"); then
    printf '%s\n' 'Sender already exists or could not be installed; inspect it before updating.' >&2
    exit 1
fi
chmod 700 -- "$destination"
```

Manual invocation uses the same explicit arguments as the native hook bridge:

```sh
sh "$HOME/.local/lib/intelligent-terminal/it-agent-hook.sh" \
  --cli-source copilot --event agent.stop < agent-hook-input.json
```

Run the CLI **inside the tmux pane** attached through IT's integration. Never
forge `TMUX` or `TMUX_PANE` in shell startup files. **Restart the CLI after
installing or changing its hooks.** These manual examples are separate from
the automatically managed installation above.

## Disable, including ACP launches

Any nonempty `WTA_TMUX_HOOKS_DISABLED` value disables this transport (`0` also
disables it). For example:

```sh
WTA_TMUX_HOOKS_DISABLED=1 claude
WTA_TMUX_HOOKS_DISABLED=1 copilot
WTA_TMUX_HOOKS_DISABLED=1 codex
WTA_TMUX_HOOKS_DISABLED=1 gemini
WTA_TMUX_HOOKS_DISABLED=1 opencode
```

Set the same variable on ACP/already-tracked launchers to avoid duplicate hook
tracking; unset it for ordinary interactive launches. OpenCode additionally
honors `OPENCODE_CLIENT=acp`. `WT_SESSION` and `WT_COM_CLSID` are irrelevant.
In particular, the shell sender **does not inspect `sidekick-*` IDs** or any
other stdin fields. That filtering now belongs to IT/WTA.

## Manual CLI-specific configuration

The following examples use a distinct **`it-tmux-hooks`** plugin name and
**`it-tmux-local`** marketplace. Each setup creates a fresh private directory
and stops if that directory already exists. Do not redirect these snippets
over existing user or managed hook files.

The static catalogs match the existing adjacent agent bundles. No
`ErrorOccurred`, tool-completion, or new subagent subscription is invented.
`agent.subagent.stop` is accepted as a compatibility topic, not subscribed
by these examples. CLI plugin APIs are version-dependent; these are Linux
configuration examples, not verification of every installed CLI version.

### Claude Code

```sh
umask 077
root="$HOME/.local/share/it-tmux-hooks/claude"
mkdir -p -- "${root%/*}"
mkdir -- "$root" || exit 1
mkdir -p -- "$root/.claude-plugin" "$root/it-tmux-hooks/.claude-plugin" "$root/it-tmux-hooks/hooks"
cat >"$root/.claude-plugin/marketplace.json" <<'JSON'
{"name":"it-tmux-local","owner":{"name":"Local user"},"plugins":[{"name":"it-tmux-hooks","source":"./it-tmux-hooks","version":"2.0.0"}]}
JSON
cat >"$root/it-tmux-hooks/.claude-plugin/plugin.json" <<'JSON'
{"name":"it-tmux-hooks","version":"2.0.0","description":"Opt-in remote tmux hook transport"}
JSON
cat >"$root/it-tmux-hooks/hooks/hooks.json" <<'JSON'
{"hooks":{
  "SessionStart":[{"matcher":".*","hooks":[{"type":"command","shell":"bash","command":"sh \"$HOME/.local/lib/intelligent-terminal/it-agent-hook.sh\" --cli-source claude --event agent.session.start; exit 0"}]}],
  "SessionEnd":[{"matcher":".*","hooks":[{"type":"command","shell":"bash","command":"sh \"$HOME/.local/lib/intelligent-terminal/it-agent-hook.sh\" --cli-source claude --event agent.session.end; exit 0"}]}],
  "Notification":[{"matcher":".*","hooks":[{"type":"command","shell":"bash","command":"sh \"$HOME/.local/lib/intelligent-terminal/it-agent-hook.sh\" --cli-source claude --event agent.notification; exit 0"}]}],
  "UserPromptSubmit":[{"matcher":".*","hooks":[{"type":"command","shell":"bash","command":"sh \"$HOME/.local/lib/intelligent-terminal/it-agent-hook.sh\" --cli-source claude --event agent.prompt.submit; exit 0"}]}],
  "StopFailure":[{"matcher":".*","hooks":[{"type":"command","shell":"bash","command":"sh \"$HOME/.local/lib/intelligent-terminal/it-agent-hook.sh\" --cli-source claude --event agent.error; exit 0"}]}],
  "Stop":[{"matcher":".*","hooks":[{"type":"command","shell":"bash","command":"sh \"$HOME/.local/lib/intelligent-terminal/it-agent-hook.sh\" --cli-source claude --event agent.stop; exit 0"}]}]
}}
JSON
claude plugin marketplace add "$root"
claude plugin install it-tmux-hooks@it-tmux-local
```

### Copilot CLI

```sh
umask 077
root="$HOME/.local/share/it-tmux-hooks/copilot"
mkdir -p -- "${root%/*}"
mkdir -- "$root" || exit 1
mkdir -p -- "$root/.github/plugin" "$root/it-tmux-hooks/hooks"
cat >"$root/.github/plugin/marketplace.json" <<'JSON'
{"name":"it-tmux-local","owner":{"name":"Local user"},"plugins":[{"name":"it-tmux-hooks","source":"./it-tmux-hooks","version":"2.0.0"}]}
JSON
cat >"$root/it-tmux-hooks/plugin.json" <<'JSON'
{"name":"it-tmux-hooks","version":"2.0.0","hooks":"hooks/hooks.json"}
JSON
cat >"$root/it-tmux-hooks/hooks/hooks.json" <<'JSON'
{"hooks":{
  "SessionStart":[{"matcher":".*","hooks":[{"type":"command","bash":"sh \"$HOME/.local/lib/intelligent-terminal/it-agent-hook.sh\" --cli-source copilot --event agent.session.start; exit 0","timeoutSec":5}]}],
  "SessionEnd":[{"matcher":".*","hooks":[{"type":"command","bash":"sh \"$HOME/.local/lib/intelligent-terminal/it-agent-hook.sh\" --cli-source copilot --event agent.session.end; exit 0","timeoutSec":5}]}],
  "Notification":[{"matcher":".*","hooks":[{"type":"command","bash":"sh \"$HOME/.local/lib/intelligent-terminal/it-agent-hook.sh\" --cli-source copilot --event agent.notification; exit 0","timeoutSec":5}]}],
  "UserPromptSubmit":[{"matcher":".*","hooks":[{"type":"command","bash":"sh \"$HOME/.local/lib/intelligent-terminal/it-agent-hook.sh\" --cli-source copilot --event agent.prompt.submit; exit 0","timeoutSec":5}]}],
  "StopFailure":[{"matcher":".*","hooks":[{"type":"command","bash":"sh \"$HOME/.local/lib/intelligent-terminal/it-agent-hook.sh\" --cli-source copilot --event agent.error; exit 0","timeoutSec":5}]}],
  "Stop":[{"matcher":".*","hooks":[{"type":"command","bash":"sh \"$HOME/.local/lib/intelligent-terminal/it-agent-hook.sh\" --cli-source copilot --event agent.stop; exit 0","timeoutSec":5}]}]
}}
JSON
copilot plugin marketplace add "$root"
copilot plugin install it-tmux-hooks@it-tmux-local
```

### Codex CLI

This catalog has no native end/error subscription.

```sh
umask 077
root="$HOME/.local/share/it-tmux-hooks/codex"
mkdir -p -- "${root%/*}"
mkdir -- "$root" || exit 1
mkdir -p -- "$root/.agents/plugins" "$root/it-tmux-hooks/.codex-plugin" "$root/it-tmux-hooks/hooks"
cat >"$root/.agents/plugins/marketplace.json" <<'JSON'
{"name":"it-tmux-local","interface":{"displayName":"Remote tmux hooks"},"plugins":[{"name":"it-tmux-hooks","source":{"source":"local","path":"./it-tmux-hooks"},"policy":{"installation":"AVAILABLE","authentication":"ON_INSTALL"},"category":"Productivity"}]}
JSON
cat >"$root/it-tmux-hooks/.codex-plugin/plugin.json" <<'JSON'
{"name":"it-tmux-hooks","version":"2.0.0","description":"Opt-in remote tmux hook transport"}
JSON
cat >"$root/it-tmux-hooks/hooks/hooks.json" <<'JSON'
{"hooks":{
  "SessionStart":[{"matcher":"startup|resume","hooks":[{"type":"command","command":"sh \"$HOME/.local/lib/intelligent-terminal/it-agent-hook.sh\" --cli-source codex --event agent.session.start; exit 0"}]}],
  "PermissionRequest":[{"hooks":[{"type":"command","command":"sh \"$HOME/.local/lib/intelligent-terminal/it-agent-hook.sh\" --cli-source codex --event agent.notification; exit 0"}]}],
  "UserPromptSubmit":[{"hooks":[{"type":"command","command":"sh \"$HOME/.local/lib/intelligent-terminal/it-agent-hook.sh\" --cli-source codex --event agent.prompt.submit; exit 0"}]}],
  "Stop":[{"hooks":[{"type":"command","command":"sh \"$HOME/.local/lib/intelligent-terminal/it-agent-hook.sh\" --cli-source codex --event agent.stop; exit 0"}]}]
}}
JSON
codex plugin marketplace add "$root"
codex plugin install it-tmux-hooks@it-tmux-local
```

### Gemini CLI

```sh
umask 077
root="$HOME/.local/share/it-tmux-hooks/gemini"
mkdir -p -- "${root%/*}"
mkdir -- "$root" || exit 1
mkdir -- "$root/hooks"
cat >"$root/gemini-extension.json" <<'JSON'
{"name":"it-tmux-hooks","version":"2.0.0","description":"Opt-in remote tmux hook transport"}
JSON
cat >"$root/hooks/hooks.json" <<'JSON'
{"hooks":{
  "SessionStart":[{"matcher":".*","hooks":[{"type":"command","command":"sh \"$HOME/.local/lib/intelligent-terminal/it-agent-hook.sh\" --cli-source gemini --event agent.session.start; exit 0"}]}],
  "SessionEnd":[{"matcher":".*","hooks":[{"type":"command","command":"sh \"$HOME/.local/lib/intelligent-terminal/it-agent-hook.sh\" --cli-source gemini --event agent.session.end; exit 0"}]}],
  "BeforeAgent":[{"matcher":".*","hooks":[{"type":"command","command":"sh \"$HOME/.local/lib/intelligent-terminal/it-agent-hook.sh\" --cli-source gemini --event agent.prompt.submit; exit 0"}]}],
  "BeforeTool":[{"matcher":".*","hooks":[{"type":"command","command":"sh \"$HOME/.local/lib/intelligent-terminal/it-agent-hook.sh\" --cli-source gemini --event agent.tool.starting; exit 0"}]}],
  "Notification":[{"matcher":".*","hooks":[{"type":"command","command":"sh \"$HOME/.local/lib/intelligent-terminal/it-agent-hook.sh\" --cli-source gemini --event agent.notification; exit 0"}]}],
  "AfterAgent":[{"matcher":".*","hooks":[{"type":"command","command":"sh \"$HOME/.local/lib/intelligent-terminal/it-agent-hook.sh\" --cli-source gemini --event agent.stop; exit 0"}]}]
}}
JSON
gemini extensions install "$root"
```

### OpenCode

Create the user-owned file
`${XDG_CONFIG_HOME:-$HOME/.config}/opencode/plugins/it-tmux-hooks.js` only if it
does not already exist. Use your editor's create-new/no-overwrite operation;
do not replace the managed `wt-agent-hooks.js`.

The adapter below subscribes to native events and supplies their canonical
topic/identity. It serializes the API objects without filtering prompt/tool
content or child sessions. The shell still receives and forwards raw stdin;
IT owns validation and status decisions. This example does not interpret
`session.status`; it uses native chat/tool/idle notifications instead.

```js
import { homedir } from "node:os"
import { join } from "node:path"

export const ItTmuxHooks = async ({ directory }) => {
  const sessions = new Set()
  const enabled = Boolean(Object.hasOwn(process.env, "IT_SSH_HOOK_ROUTE") ||
    (process.env.TMUX && process.env.TMUX_PANE)) &&
    !process.env.WTA_TMUX_HOOKS_DISABLED && process.env.OPENCODE_CLIENT !== "acp"
  async function emit(topic, id, payload) {
    if (!enabled) return
    const bytes = new TextEncoder().encode(JSON.stringify({
      cwd: directory, ...payload, session_id: id,
    }))
    let child
    try {
      child = Bun.spawn({
        cmd: ["sh", join(homedir(), ".local/lib/intelligent-terminal/it-agent-hook.sh"),
          "--cli-source", "opencode", "--event", topic],
        stdin: bytes, stdout: "ignore", stderr: "inherit",
      })
    } catch (error) {
      console.error("it-tmux-hooks: sender could not start")
      return
    }
    await child.exited.then(
      code => { if (code !== 0) console.error("it-tmux-hooks: sender failed") },
      () => console.error("it-tmux-hooks: sender process failed"),
    )
  }
  const topics = {
    "session.created": "agent.session.start",
    "session.updated": "agent.session.start",
    "session.deleted": "agent.session.end",
    "session.idle": "agent.stop",
    "session.error": "agent.error",
    "permission.asked": "agent.notification",
    "question.asked": "agent.notification",
    "permission.replied": "agent.prompt.submit",
    "question.replied": "agent.prompt.submit",
  }
  return {
    "chat.message": async (input, output) => {
      sessions.add(input.sessionID)
      await emit("agent.prompt.submit", input.sessionID, { ...input, ...output })
    },
    "tool.execute.before": async (input, output) => {
      await emit("agent.tool.starting", input.sessionID,
        { ...input, ...output, tool_name: input.tool, tool_input: output.args })
    },
    event: async ({ event }) => {
      const topic = topics[event.type]
      if (!topic) return
      const properties = event.properties || {}
      const id = properties.info?.id || properties.sessionID
      if (event.type === "session.deleted") sessions.delete(id)
      else if (id) sessions.add(id)
      await emit(topic, id, { ...properties, opencode_event: event })
    },
    dispose: async () => {
      await Promise.all([...sessions].map(id =>
        emit("agent.session.end", id, { reason: "OpenCode exited" })))
      sessions.clear()
    },
  }
}
```

Restart OpenCode after saving the plugin.

## Wire, routing, limits, and privacy

Manual pane routing retains this exact v2 shape, with single ASCII spaces:

```text
IT_AGENT_HOOK/2 <session> <pane> <source> <event> <transfer> <index> <count> <data>
```

Managed ordinary-SSH routing uses this v3 shape instead:

```text
IT_AGENT_HOOK/3 <route_uuid> <source> <event> <transfer> <index> <count> <data>
```

V3 carries no remote pane, tab, or window identity. Windows binds the route
UUID to its own foreground SSH context. A present managed route takes
precedence even when the CLI also inherited a `TMUX` environment.

For example, raw `{}` becomes:

```text
IT_AGENT_HOOK/2 $0 %1 copilot agent.stop it-agent-hook.A1b2C3d4E5f6 0 1 e30=
```

`session`/`pane` are the original tmux IDs, never Windows GUIDs. `source` is
one of `claude`, `copilot`, `codex`, `gemini`, `opencode`. Events are
`agent.session.start`, `agent.session.end`, `agent.prompt.submit`,
`agent.notification`, `agent.tool.starting`, `agent.stop`, `agent.error`, or
`agent.subagent.stop`.

The transfer token is a per-invocation, securely created `mktemp` nonce using
only ASCII letters, digits, `.`, `_`, and `-` (at most 64 characters). It
separates simultaneous hooks; it is **not authentication**. Indexes are
zero-based. Count is 1..234. Standard Base64 data is wrapped at **6000
characters**, always a multiple of four; non-final chunks have exactly 6000
characters and no padding. Empty stdin sends index 0/count 1/empty data,
including the final space before that empty field.

The raw limit is **exactly 1 MiB**. The sender reads one extra byte solely to
detect overflow; 1 MiB+1 is rejected before any notification. Partial reads
that time out are also rejected, never mistaken for complete input.
Large bodies are chunked, not truncated. Every literal remains under 8 KiB.
IT's assembler limits storage to 16 in-flight transfers and 4 MiB of aggregate
encoded data, with a 1 MiB raw-body limit and 10-second expiry. It requires
strictly sequential chunks and consistent metadata throughout a transfer.
Only then does IT parse/project/redact the body. Base64 decoding preserves
even invalid UTF-8 bytes; JSON validation subsequently rejects invalid UTF-8.
Unreleased v1 is explicitly rejected, with no fallback.

V2 routing is explicit and fail-closed:

1. Split `TMUX` at its last two commas, preserving commas in socket paths.
   Validate the numeric session and `%integer` pane, and bind all tmux calls
   to `-N -S <socket>` so no server is started accidentally.
2. Use `list-panes -s -t '$N'` to verify that the pane still belongs to the
   original session. A stale or moved pane is a no-op; never guess a different
   session. Linked windows still use only the original session.
3. Enumerate only that session with `list-clients -t '$N'` and
   `client_control_mode`, `client_name`, and `session_id`. Retain only control
   clients whose session ID matches.
4. Deliver each literal with `display-message -l -c <client>`. A fixed batch
   of these commands is submitted with tmux `source-file -` to avoid hundreds of
   process startups for a large body. All command fields are validated tokens
   or Base64 and are single-quoted in tmux syntax; raw stdin is never shell
   code, `eval` input, or a command argument.

V3 validates the UUID and the owned, explicitly supplied managed socket,
requires `@it-ssh-hooks-protocol=3`, resolves the exact `it-hooks` session,
and enumerates only its control clients. It does not inspect ordinary pane
membership. Both versions stream the fixed command batch on tmux stdin, so
the server does not need filesystem access to a CLI's private temporary
directory. Client discovery uses printable separators, which remain intact
for non-UTF-8 tmux clients launched outside panes.

`-l` prevents tmux format/strftime expansion. `%message` bodies arrive as raw
literal text; they are not `%output` escape sequences. **`display-message -C`
is not broadcast** and is not used. No pane text or normal-client status
message is emitted.

Every invocation returns **exit 0 with empty stdout**. Missing tmux,
unsupported tmux, missing environment/socket/session/pane/control clients,
and explicit opt-outs are silent no-ops. Missing required utilities and
actual failures produce a concise stderr diagnostic without raw data or
subprocess arguments.

GNU `timeout` bounds stdin and each tmux call to one second each. The entire
worker process group has a **3.2-second deadline** plus a 0.1-second kill
grace. Scratch creation and each cleanup command have a 0.1-second timeout
plus a 0.05-second kill grace. These budgets total at most 3.75 seconds,
leaving process-startup headroom under the CLI's five-second hook timeout.
No retries or cross-session replay are attempted. IT discards incomplete
transfers if a client disconnects or a batch cannot finish. **An incomplete
transfer never produces partial agent status.**

Temporary data lives in one private `mktemp -d` directory beneath
`${TMPDIR:-/tmp}`, with `umask 077`. Exit/signal traps remove only that
invocation's named files and exact directory; the outer shell also cleans
after worker timeouts. Raw/Base64 data is not retained after normal or timeout
completion. Filesystem failures are reported, not silently ignored.

**Every control client in the same tmux session can see the original raw
payload, including prompts and tool data, before IT redacts it.** This is an
intentional tradeoff of the thin remote transport: trust all those clients
and the remote tmux server. **Base64 is not encryption.** tmux command
history/debugging may also retain the encoded body. Redaction occurs in IT
only after arrival and full reassembly. This opt-in transport is not an
authentication boundary.
For the managed `it-hooks` session, this includes raw events from other SSH
routes sharing that user's side channel, before Windows filters route UUIDs.

## Manual update and uninstall

For an update, disable the remote plugin first, inspect/back up your existing
user-owned sender/configuration, replace only those files intentionally, and
restart the CLI. Setup examples never overwrite a previous installation.

Remove the remote plugin/extension with the matching CLI's supported command:

```sh
claude plugin uninstall it-tmux-hooks@it-tmux-local
copilot plugin uninstall it-tmux-hooks@it-tmux-local
codex plugin uninstall it-tmux-hooks@it-tmux-local
gemini extensions uninstall it-tmux-hooks
# OpenCode: remove only the user-owned example created above.
rm -- "${XDG_CONFIG_HOME:-$HOME/.config}/opencode/plugins/it-tmux-hooks.js"
```

For Claude/Copilot/Codex, also remove the `it-tmux-local` marketplace
registration using that CLI's marketplace removal command and delete only
your generated `~/.local/share/it-tmux-hooks/<cli>` directory after inspection.
For OpenCode, remove only your `it-tmux-hooks.js` file. Restart all affected
CLIs, then delete the installed `it-agent-hook.sh` when nothing references it.
Do **not** remove managed `wt-agent-hooks` plugins. Local WTA install/status/
uninstall commands do not manage this remote installation.

## Tests

From a fresh Windows checkout with PowerShell 7 and WSL Ubuntu:

```powershell
.\tools\wta\wt-agent-hooks\tmux\Test-TmuxAgentHook.ps1 -Distribution Ubuntu
```

For focused installer iteration, add `-InstallerOnly`. Capture export requires
the transport tests and cannot be combined with that switch.

`Test-TmuxAgentHook.ps1` copies the two shipped shell assets and the two Bash
test drivers into a private, short Linux-native directory, normalizing CRLF.
This avoids `/mnt/c` Unix-socket/interop issues without requiring an extra
Linux language runtime. The Bash test drivers additionally use standard
GNU text/file tools and util-linux `script` to attach an ordinary PTY client.
Missing test prerequisites fail explicitly; nothing is installed automatically.

The real tmux suite owns one unique socket containing a comma, two sessions,
two origin control clients, another-session control client, and an ordinary
client. A FIFO-driven worker invokes the sender inside an actual pane's
inherited environment. Control-protocol barriers and `tmux wait-for` provide
synchronization without sleeps.

Assertions validate exact v2 framing, ordering, Base64, raw byte equality,
empty stdin, large Unicode/tool output, exactly 1 MiB acceptance, 1 MiB+1
rejection, distinct concurrent transfer IDs, no cross-session/status/pane
injection, opt-outs, utility failures, deadlines, and scratch cleanup.
Only owned client PIDs and the test socket's server are terminated; the
PowerShell wrapper removes its private Linux-native files even on failure.
V3 tests execute outside tmux panes, share the dedicated session across
multiple route UUIDs, and verify missing/malformed routes never fall back to
v2. `test-install-remote-hooks.sh` uses isolated fake CLI homes and management
APIs for fresh install, upgrade, retry, ownership, disabled/removed/no-CLI
cases, and accessible/inaccessible Snap namespace probes. It also verifies
control-stdin handoff and server lifetime. PowerShell parses every generated
configuration and executes its absolute command under a different `HOME`,
including login-home paths containing quotes, backslashes, and dollar signs.
No real user CLI configuration or agent credentials are changed by the suite.

### Capture a real stream for native receiver tests

After building the x64 Debug TerminalApp unit tests, use a TAEF-enabled
developer shell to export and consume a real two-chunk transfer:

```powershell
$capture = Join-Path $PWD ("obj\tmux-hook-capture-" + [guid]::NewGuid().ToString("N"))
.\tools\wta\wt-agent-hooks\tmux\Test-TmuxAgentHook.ps1 -Distribution Ubuntu `
  -CaptureDirectory $capture
te.exe .\bin\x64\Debug\UnitTests_TerminalApp\Terminal.App.Unit.Tests.dll `
  '/name:*ConsumesCapturedShellMessages*' "/p:TmuxHookCaptureDirectory=$capture"
```

The destination must not already exist. It contains:

- `real-shell-v2.messages`: exact `%message IT_AGENT_HOOK/2 ...` lines with
  their original tmux IDs, transfer nonce, and LF delimiters.
- `real-shell-v2.payload`: the exact unredacted stdin bytes, including UTF-8
  tool output and trailing CR/LF, for a byte-for-byte native comparison.

The body uses `session_id: native-fixture-session`, `cwd: /repo`, and a
`TEST_PROMPT_REDACT_ME` prompt plus tool output to exercise native projection.
The native test feeds the captured messages through the production parser
and assembler, compares the original bytes, and checks native redaction.
These requested artifacts persist in the ignored build directory; Linux
scratch resources are still cleaned. They contain only generated test data.

No CI workflow is added. A future Windows job needs WSL Ubuntu and the command
above; a Linux job can run the Bash driver in the same private-directory
layout. This transport coverage does not exercise packaged XAML pane routing
or live provider authentication.
