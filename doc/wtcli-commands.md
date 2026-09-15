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
| `listen` | — | Long-running. Subscribe to `IProtocolServer` and stream every event JSON line to stdout until Ctrl-C. `-t` filters by pane id; `--event` filters by type and supports a trailing `*` wildcard. Internal callers use `--parent-pid` to terminate the listener if its owner crashes. | `wtcli --json listen --event "agent.*"` | ✅ `cli_channel.rs` (background listener task) |
| `send-event` | `se` | Publish an event using the `agent_event` envelope: sets `type=event`, `method=agent_event`, fills `params.event` from `-e` and `params.pane_id` from `-p`. Omitting `-p` publishes an empty `pane_id` meaning "source pane unknown" — it is **not** attributed to the focused pane, because guessing a pane corrupts session-to-pane binding, while an unattributed event is routed by `cli_source` instead. Extra params come from the trailing JSON object. | `wtcli send-event -p 3 -e agent.task.completed '{"exit_code":0}'` | ❌ Not called from in-tree code. Kept as the transport for legacy PowerShell hook bundles (guarded by `Feature.LegacyHookBundle.Tests.ps1`) and as the public CLI surface for external agents in `doc/specs/llm-agent-event-integration.md`. |
| `publish` | — | Low-level escape hatch: forwards raw JSON straight to `IProtocolServer::SendEvent` with no envelope. Pass JSON as a positional argument for compatibility, or use `--stdin` for payloads that may exceed the Windows command-line limit. The two input forms are mutually exclusive. | `Get-Content event.json -Raw \| wtcli publish --stdin` | ✅ `tools/wta/src/wt_protocol_events.rs` |
| `info` | — | Print `WT_COM_CLSID`, connection status, protocol version, and the server's `GetCapabilities()` method list. | `wtcli --json info` | ✅ `cli_channel.rs` maps `get_capabilities` → `wtcli info` |
| `test-pipe` | — | Smoke test: connect, run `list-windows` + `get_capabilities`, print results. Diagnostic only. | `wtcli test-pipe` | ❌ Not called. Manual diagnostic. |
| `set-env` | `setenv` | Print shell-specific export statements for `WT_COM_CLSID` (`-s powershell\|bash\|cmd`). Output is meant to be `eval`'d / `Invoke-Expression`'d by the caller; it does not modify the current process. | `wtcli set-env -s powershell \| Invoke-Expression` | ❌ Not called. Manual recovery for child shells that didn't inherit `WT_COM_CLSID`. |

## Native tmux-control windows

```powershell
wtcli --json tmux "wsl.exe -- tmux -C new-session -A -s work"
wtcli tmux --cwd "C:\src\项目" "ssh.exe build-host tmux -C attach-session -t work"
wtcli tmux '"C:\Program Files\Backend\control.exe" --session work'
```

Pass the complete backend Windows process commandline as **one argument**.
Intelligent Terminal does not detect SSH, interpret the commandline, or insert
a local shell; the supplied process must speak tmux control mode over stdio.
Quote arguments for the calling shell as usual. Unicode commandlines and paths
are preserved. An explicit relative `--cwd` is resolved in the caller before the
request is sent; omitted `--cwd` captures the caller's current directory.
Empty values, control characters, and values exceeding the Windows commandline
length limit are rejected.

The request always creates a new native window; identical commandlines are not
deduplicated. `wtcli` exits after window creation and does not own the backend's
lifetime. A successful JSON response is:

```json
{"window_id": 2, "state": "starting"}
```

`starting` acknowledges the new window, **not** a connected backend. Process or
protocol startup failures are reported by the destination window. There is no
automatic restart or reconnect. These transient windows are excluded from
automatic layout/buffer persistence and named-workspace restoration.

This command uses the optional `ITerminalTmuxWindow` COM extension
(`76F10E43-6D63-4EB0-A860-0F5A7F291BE9`), advertised as
`create_tmux_window` by `GetCapabilities`. Older Terminal versions are rejected
with an explicit unsupported-version message rather than calling a changed
version of the original COM interface.

### Supported frontend behavior

One control client maps its attached session to the native window, backend
windows to tabs, and backend panes to terminal panes. Ordinary output, Unicode
input, terminal mouse sequences, and paste use the single control stream.
The `+` button, new-tab action, directional split actions, keyboard pane resize,
pane zoom, and individual pane/tab close commands operate on the backend.
Closing the **native window** disconnects the frontend; it does not send
`kill-session`, `kill-window`, or `kill-pane`. Whether processes survive is a
property of the backend; a real tmux server normally keeps them running.

Layout notifications from another client are reconciled while retaining existing
terminal contents and connection identities, including panes moved between tabs.
Initial attachment captures up to 2,000 history lines and restores the main/alternate
screen, cursor position, scroll region, wrap, insert, cursor-key/keypad and exposed
mouse modes. This is not full iTerm2 state parity: saved terminal modes that tmux
does not expose (including bracketed-paste state on current supported releases),
tab stops, and some extended keyboard modes are not reconstructed on attachment.
Modes changed by subsequent application output work through the normal VT parser.

The initial implementation supports ordinary tiled layouts. Mouse divider dragging,
local pane creation, pane/tab transfer between native windows, local assistant panes,
and generic `wtcli new-tab` / `split-pane` mutations into a managed window are not
supported. Keyboard resize is available. Managed panes currently hide their scrollbar
to keep the terminal's cell dimensions equal to the backend; keyboard scrolling and
selection remain available. Backend-generated tmux copy-mode/menu UI is not a
control-mode output stream.

The process transport uses raw pipes, not a local ConPTY. Both unframed `-C` and
DCS-framed `-CC` protocol streams are accepted. Real tmux `-CC` additionally needs a
TTY, so use `-C` for the simple WSL/SSH pipe examples, or supply a backend command
that provides its own TTY. IT does not infer or rewrite that command.

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
