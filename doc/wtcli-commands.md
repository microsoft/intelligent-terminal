# wtcli Command Reference

`wtcli` is the CLI client for the Windows Terminal Protocol. It looks up the
running Terminal via the `WT_COM_CLSID` environment variable, calls
`CoCreateInstance(CLSCTX_LOCAL_SERVER)` to obtain the classic COM `ITerminalProtocol`, and
exposes a tmux-style command surface over its IDL methods.

- Source: `src/tools/wtcli/main.cpp`
- COM IDL: `src/host/proxy/ITerminalProtocol.idl`
- Primary in-tree caller: `tools/wta/src/shell/wt_channel/cli_channel.rs` (and
  `tools/wta/src/app.rs` for `publish`).

## Global flags

| Flag | Effect |
|------|--------|
| `--json` | Emit machine-readable JSON. Required for any caller that parses output. |

## Commands

The "Used in repo" column reflects whether some other component in this
repository actually shells out to that subcommand today (not whether the
subcommand is reachable). External callers (third-party agents, ad-hoc
scripts) are not counted.

| Command | Alias | What it does | Example | Used in repo |
|---------|-------|--------------|---------|--------------|
| `list-windows` | `lsw` | List all Terminal windows. | `wtcli --json list-windows` | ✅ `cli_channel.rs` (`list_windows`) |
| `list-tabs` | `lst` | List tabs in a window. `-w` defaults to the first window. | `wtcli --json list-tabs -w 1` | ✅ `cli_channel.rs` (`list_tabs`) |
| `list-panes` | `lsp` | List panes in a tab. `-t`/`-w` default to the first tab of the first window. | `wtcli --json list-panes -t 2` | ✅ `cli_channel.rs` (`list_panes`) |
| `active-pane` | — | Return metadata for the currently focused pane. Used by other subcommands as the default `-t` target. | `wtcli --json active-pane` | ✅ `cli_channel.rs` (`get_active_pane`) |
| `capture-pane` | `capturep` | Read pane scrollback as text. `-l` caps line count. `--last-prompt` returns only the most recent completed shell prompt (requires OSC 133 shell integration). | `wtcli --json capture-pane -t 3 --last-prompt` | ✅ `cli_channel.rs` (`read_pane_output`) |
| `pane-status` | — | Report pane process state: `pid`, `state` (`running`/`exited`), and `exit_code` when applicable. | `wtcli --json pane-status -t 3` | ✅ `cli_channel.rs` (`get_process_status`) |
| `new-tab` | `neww` | Create a new tab. `-c` command, `-n` title, `-d` cwd. | `wtcli --json new-tab -c "pwsh" -n "build" -d C:\src` | ✅ `cli_channel.rs` (`create_tab`) |
| `tmux` | — | Create a **new native window** for an opaque tmux-control backend process. `-d`/`--cwd` defaults to the caller's current directory. | `wtcli --json tmux "wsl.exe -- tmux -C new-session -A -s work"` | ❌ Direct CLI entry point. |
| `split-pane` | `splitw` | Split a pane. `-d right\|left\|up\|down\|auto` (default `automatic`). `-H`/`-v` are legacy aliases for `down`/`right`. `-s` is size fraction; `-c` is the command to run. | `wtcli --json split-pane -t 3 -d right -s 0.4 -c "tail -f log"` | ✅ `cli_channel.rs` (`split_pane`) |
| `kill-pane` | `killp` | Close a pane. | `wtcli kill-pane -t 4` | ✅ `cli_channel.rs` (`close_pane`) |
| `focus-pane` | `focusp` | Move focus to the given pane. | `wtcli focus-pane -t 3` | ✅ `cli_channel.rs` (`focus_pane`) |
| `wait-for` | — | Block (poll `pane-status`) until the pane process exits. `--interval` is poll period in ms; `--timeout` is seconds (`0` = forever). | `wtcli wait-for -t 3 --timeout 60` | ❌ Not called. (`wta` exposes its own `wait-for` subcommand at `tools/wta/src/main.rs:209`, but its handler polls by shelling out to `wtcli pane-status` in a Rust loop — it does **not** invoke `wtcli wait-for`.) |
| `listen` | — | Long-running. Subscribe to `IProtocolServer` and stream its COM-published event JSON lines to stdout until Ctrl-C. Includes local hooks and native tmux v2 events, but not master-direct ordinary SSH v3 hooks. `-t` filters by pane id; `--event` filters by type and supports a trailing `*` wildcard. Internal callers use `--parent-pid` to terminate the listener if its owner crashes. | `wtcli --json listen --event "agent.*"` | ✅ `cli_channel.rs` (background listener task) |
| `send-event` | `se` | Publish an event using the `agent_event` envelope: sets `type=event`, `method=agent_event`, fills `params.event` from `-e` and `params.pane_id` from `-p`. Omitting `-p` publishes an empty `pane_id` meaning "source pane unknown" — it is **not** attributed to the focused pane, because guessing a pane corrupts session-to-pane binding, while an unattributed event is routed by `cli_source` instead. Extra params come from the trailing JSON object. | `wtcli send-event -p 3 -e agent.task.completed '{"exit_code":0}'` | ❌ Not called from in-tree code. Kept as the transport for legacy PowerShell hook bundles (guarded by `Feature.LegacyHookBundle.Tests.ps1`) and as the public CLI surface for external agents in `doc/specs/llm-agent-event-integration.md`. |
| `publish` | — | Low-level escape hatch: forwards raw JSON straight to `IProtocolServer::SendEvent` with no envelope. Pass JSON as a positional argument for compatibility, or use `--stdin` for payloads that may exceed the Windows command-line limit. The two input forms are mutually exclusive. | `Get-Content event.json -Raw \| wtcli publish --stdin` | ✅ `tools/wta/src/wt_protocol_events.rs` |
| `info` | — | Print `WT_COM_CLSID`, connection status, protocol version, and the server's `GetCapabilities()` method list. | `wtcli --json info` | ✅ `cli_channel.rs` maps `get_capabilities` → `wtcli info` |
| `test-pipe` | — | Smoke test: connect, run `list-windows` + `get_capabilities`, print results. Diagnostic only. | `wtcli test-pipe` | ❌ Not called. Manual diagnostic. |
| `set-env` | `setenv` | Print shell-specific export statements for `WT_COM_CLSID` (`-s powershell\|bash\|cmd`). Output is meant to be `eval`'d / `Invoke-Expression`'d by the caller; it does not modify the current process. | `wtcli set-env -s powershell \| Invoke-Expression` | ❌ Not called. Manual recovery for child shells that didn't inherit `WT_COM_CLSID`. |

## Native tmux-control windows

An ordinary SSH tab (a generated SSH profile or a tab launched directly with
`ssh.exe <alias>`) shows the workspace-style button at the upper left. Open it
to list sessions on that SSH host's **default** tmux server, then select a row
to attach in a new native window, or focus the existing window for the same SSH
destination, port, and session. No existing tmux frontend is required.
Switching to a local tab hides the button; switching tabs or panes cancels a
pending request so one host's results cannot appear under another host.
User and port arguments (`-l` and `-p`) are retained. Put other connection
options in an SSH config Host alias; unsupported direct options display an
explicit failure rather than silently querying a different connection.
SSH entered inside an already-running local shell is not detected.

To open a tmux frontend directly, use the structured connection form:

```powershell
ssh.exe -T ubuntu "tmux -L default new-session -Ad -s work"
wtcli tmux --ssh ubuntu --session work
```

`--ssh` accepts an SSH config alias or destination. SSH uses noninteractive
authentication and no TTY; configure keys/ssh-agent and trust the host before
launching. `--session` identifies an existing session. The upper-left workspace
icon opens a freshly queried list of sessions on that host's **default** tmux
server. Selecting a row opens a new native window without switching or closing
the source window. Rows use stable session IDs, so spaces, Unicode and session
renames do not turn a selection into shell syntax. Reopen the menu to refresh or
retry a failed request; loading, empty and failed lists have explicit states.
The list is bounded to 256 sessions and 64 KiB, with a 10-second response timeout.
Other socket names and custom socket paths are not enumerated.

The existing opaque backend form remains available:

```powershell
wtcli --json tmux "wsl.exe -- tmux -C new-session -A -s work"
wtcli tmux --cwd "C:\src\项目" "ssh.exe build-host tmux -C attach-session -t work"
wtcli tmux '"C:\Program Files\Backend\control.exe" --session work'
```

The structured `--ssh` form also exposes the session menu. Arbitrary opaque
backend commands do not supply reusable SSH connection metadata and retain
their static label. Do not combine the two forms. Both support `--cwd`.

Pass the complete backend Windows process commandline as **one argument**.
Intelligent Terminal does not detect SSH, interpret the commandline, or insert
a local shell; the supplied process must speak tmux control mode over stdio.
Quote arguments for the calling shell as usual. Unicode commandlines and paths
are preserved. An explicit relative `--cwd` is resolved in the caller before the
request is sent; omitted `--cwd` captures the caller's current directory.
Empty values, control characters, and values exceeding the Windows commandline
length limit are rejected.

Structured `--ssh` requests and session-menu selections reuse an existing window
for the same destination, port, and tmux session, including an attachment still
connecting. Once connected, matching uses the current stable session ID/name,
not the window caption or an old name. Failed, closing, or closed attachments
are not reused. Opaque commandline requests still create independent windows.
`wtcli` exits after the request and does not own the backend's lifetime. A
successful JSON response is:

```json
{"window_id": 2, "state": "starting"}
```

`starting` acknowledges the selected or newly created window, **not** a connected backend. Process or
protocol startup failures are reported by the destination window. There is no
automatic restart or reconnect. These transient windows are excluded from
automatic layout/buffer persistence and named-workspace restoration.

This command uses the optional `ITerminalTmuxWindow` COM extension
(`76F10E43-6D63-4EB0-A860-0F5A7F291BE9`), advertised as
`create_tmux_window` by `GetCapabilities`. Older Terminal versions are rejected
with an explicit unsupported-version message rather than calling a changed
version of the original COM interface.

The upper-left corner shows a compact workspace-style `socket/session` label,
separate from the tmux tab names. A missing socket or tmux's default socket shows
only the session name. The socket name comes from the final component of the
backend's `#{socket_path}`, not from parsing the opaque startup command; session
changes and renames update the label. The OS window title uses the same identity.
The full backend command is available on hover rather than reserving space to the
right of the tabs. Avoid embedding credentials in the commandline, since it is
visible in that tooltip.

### Supported frontend behavior

One control client maps its attached session to the native window, backend
windows to tabs, and backend panes to terminal panes. Ordinary output, Unicode
input, terminal mouse sequences, and paste use the single control stream.
Tabs follow the backend's current `window_index` order, while permanent window
IDs remain the routing identity. A client-local order subscription catches
index-only swaps and renumbering without recreating panes, changing the selected
backend window, or modifying remote configuration.
The `+` button, new-tab action, directional split actions, keyboard pane resize,
pane zoom, and individual pane/tab close commands operate on the backend.
The existing tab rename editor and `renameTab` action rename the corresponding
tmux **window** by its stable ID, not just a local title and not the tmux session.
Names are literal, including Unicode, quotes, shell characters, and tmux format
markers. The backend confirmation updates all attached native clients, and the
name survives reattachment. Clearing the custom name restores automatic naming
for that backend window; cancelling the editor leaves it unchanged.
tmux's printable-name escaping (such as doubled backslashes) is displayed
consistently; confirming an unchanged title does not rename it again.
Closing the **native window** disconnects the frontend; it does not send
`kill-session`, `kill-window`, or `kill-pane`. Whether processes survive is a
property of the backend; a real tmux server normally keeps them running.

Layout notifications from another client are reconciled while retaining existing
terminal contents and connection identities, including panes moved between tabs.
Backend-driven tab selection is not sent back as a new selection command, so
reattaching to an existing multi-window session does not undo native tab clicks.
Initial attachment captures up to 2,000 history lines and restores the main/alternate
screen, cursor position, scroll region, wrap, insert, cursor-key/keypad and exposed
mouse modes. tmux's unset saved-cursor sentinel is accepted, including alternate
screens entered without saving a cursor. This is not full iTerm2 state parity: saved terminal modes that tmux
does not expose (including bracketed-paste state on current supported releases),
tab stops, and some extended keyboard modes are not reconstructed on attachment.
Modes changed by subsequent application output work through the normal VT parser.
Initial pane capture waits until the local terminal has initialized at the backend's
actual row/column dimensions. The captured dimensions and native viewport are
checked again before applying the snapshot on the UI thread; a resize during
capture triggers a fresh snapshot rather than clamping the cursor to a temporary
grid. Cursor positions inside an edited command line are preserved, not forced
to the end of the prompt.

The initial implementation supports ordinary tiled layouts. Mouse divider dragging,
local pane creation, pane/tab transfer between native windows, local assistant panes,
and generic `wtcli new-tab` / `split-pane` mutations into a managed window are not
supported. Keyboard resize is available. Managed panes currently hide their scrollbar
to keep the terminal's cell dimensions equal to the backend; keyboard scrolling and
selection remain available. The frontend advertises its cell dimensions only after
font initialization, updates them after layout/DPI changes, and refreshes the
window inventory after resizing. If another client or backend policy keeps a
window smaller, unused pane space uses the terminal background rather than a
transparent hole. Backend-generated tmux copy-mode/menu UI is not a
control-mode output stream.

Some Bash/readline colored prompts retain an incorrect cursor position after tmux
resizes a previously wrapped line. This can also be reproduced with raw tmux control
mode and no IT frontend: compare `#{cursor_x}` with the prompt before treating it
as a local rendering error. At an empty shell prompt, pressing Enter to draw a fresh
prompt can recover the position; IT does not inject Enter or move the cursor to the
line end automatically, because the backend may be running an editor or another
application. See [tmux/tmux#817](https://github.com/tmux/tmux/issues/817).

The process transport uses raw pipes, not a local ConPTY. Both unframed `-C` and
DCS-framed `-CC` protocol streams are accepted. Real tmux `-CC` additionally needs a
TTY, so use `-C` for the simple WSL/SSH pipe examples, or supply a backend command
that provides its own TTY. IT does not infer or rewrite that command.

### Remote Linux agent hooks

The optional [tmux hook bridge](../tools/wta/wt-agent-hooks/tmux/README.md)
forwards agent lifecycle/status events without installing `wtcli` or WTA on
the remote host. Install it separately in the Linux CLI's hook configuration;
the existing Windows hook installer does not modify remote machines.

For the end-to-end transport diagrams, implementation map, status transitions,
and Session Management refresh flow, see the
[remote tmux agent hook specification](specs/tmux-remote-agent-hooks.md).

Ordinary managed SSH panes use a separate background channel and reuse the
SSH source registry for status and focus. See
[ordinary SSH agent hooks](specs/ordinary-ssh-agent-hooks.md); this does not
intercept arbitrary SSH commands typed inside an unrelated shell.

It uses a shell script, standard Linux utilities, and **tmux 3.4 or newer**;
Python, Node.js, and `jq` are not required for the sender. Inside tmux, it checks
`TMUX` and `TMUX_PANE`, verifies that the pane still belongs to its originating
session, and enumerates only that session's control clients. For each client it
uses `display-message -l -c <client>` to deliver a literal `%message` notification:

```text
%message IT_AGENT_HOOK/2 $0 %1 copilot agent.stop transfer-id 0 1 eyJzZXNzaW9uX2lkIjoic2lkIn0=
```

The fields after the version are session, pane, CLI source, event, transfer ID,
zero-based chunk index, chunk count, and Base64 data. The shell does not parse,
redact, or truncate JSON. It forwards up to 1 MiB of raw stdin using chunks of at
most 6,000 Base64 characters, so each tmux message stays below 8 KiB.

`display-message -C` is **not** a broadcast switch. Ordinary status messages,
`wait-for`, and user options do not automatically carry hook JSON to the
frontend. The bridge uses the documented control-client message path; it never
writes an OSC sequence or hook JSON into a pane's terminal output.

IT checks the version, size, source, event, attached session and pane inventory,
then reassembles complete transfers with bounded memory and a 10-second expiry.
Missing, duplicate, inconsistent, or oversized chunks never become partial
agent events. IT parses the complete UTF-8 JSON, projects only consumed metadata,
resolves the native pane/tab/window itself (including zoom-hidden panes), and
applies the native hook's redaction and final event budget.
WTA scopes these live rows separately from local agent sessions and supports
focusing their visible native panes. Leave zoom mode before focusing a hidden
pane's row; its hook status is still tracked while hidden. An ended tmux row does
not resume a CLI on Windows:
the opaque backend command is not enough information to reconstruct a remote
resume invocation.

Missing tmux, unsupported tmux versions, execution outside tmux, stale pane
membership, and no attached control client are successful no-ops. Delivery is
live and best-effort, not a durable queue: disconnected clients receive no replay.
Linked windows send only to the originating session, not every session sharing
the pane. Other control clients attached to that same session can read the
original hook payload, including prompts and tool data, before IT redacts it.
Base64 is framing, not encryption. Access to the tmux socket also permits
spoofing status events; this is a status channel, never proof of identity,
permission or shell-input authorization.

### Deterministic local fixture

`src\tools\wtcli\tests\TmuxBackend.ps1` is a PowerShell 7 stdio backend
that does not launch shells or require SSH, WSL, or tmux. From the repository
root, with the matching native build deployed:

```powershell
$fixture = (Resolve-Path .\src\tools\wtcli\tests\TmuxBackend.ps1).Path
wtcli tmux "pwsh.exe -NoProfile -File `"$fixture`" -CC -Mode RestructureOnInput"
```

It begins with two backend windows and three panes. The first input in
`RestructureOnInput` mode moves the same three pane IDs into one nested layout;
`SwapOnInput` swaps existing panes instead. Omit `-CC` to exercise unframed
control mode. `-InvocationLogPath <path>` optionally records command traffic
to JSONL, separate from protocol stdout.

## Summary

- **Wired into `wta` runtime (13):** `list-windows`, `list-tabs`,
  `list-panes`, `active-pane`, `capture-pane`, `pane-status`,
  `new-tab`, `split-pane`, `kill-pane`, `focus-pane`,
  `listen`, `info`, `publish`.
- **Defined but not invoked from in-tree code (5):** `wait-for`,
  `send-event`, `test-pipe`, `set-env`, `tmux`. These remain as public surface for
  external agents / shell scripts and for manual debugging.
