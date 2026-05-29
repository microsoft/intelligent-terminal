# WTA -- Windows Terminal Agent

A Rust TUI client and tmux-like CLI that connects AI agents to Windows Terminal.

Customization:
- See [CUSTOMIZATION.md](CUSTOMIZATION.md) for changing the agent model and runtime prompt.

## Quick Start

### Build

```bash
cd tools/wta
cargo build
```

The binary is output to `tools/wta/target/debug/wta.exe`.

### Run (ACP TUI mode)

```bash
# Default agent (Copilot)
wta

# With a specific agent
wta --agent "copilot --acp --stdio"

# With an initial prompt
wta "list all open tabs"

# Claude via ACP adapter
wta --agent "claude-agent-acp --stdio"
```

When ACP mode is connected to Windows Terminal, the current agent-facing contract is the local `wta` CLI.
The agent is expected to shell out to commands like `wta active-pane --json`, `wta list-panes --json`, and `wta capture-pane --json`.
The CLI then talks to Windows Terminal over the protocol.

### tmux-like CLI

WTA exposes tmux-equivalent subcommands for controlling Windows Terminal from the shell. Useful for humans and AI agents that can shell out.

```bash
wta list-windows                          # list all WT windows
wta list-tabs                             # list tabs in first window
wta list-panes                            # list panes in first tab
wta active-pane                           # show focused pane
wta new-tab -c "pwsh.exe" -n "Build"      # create tab running pwsh
wta split-pane -H -c "pwsh.exe"           # split horizontal
wta capture-pane -t 3 -l 50              # read last 50 lines from pane 3
wta kill-pane -t 3                        # close pane 3
wta pane-status -t 3                      # check if running
wta wait-for -t 3 --timeout 30           # wait for pane 3 to exit
wta list-windows --json                   # raw JSON output
```

Short aliases are supported: `lsw`, `lst`, `lsp`, `neww`, `splitw`, `send`, `capturep`, `killp`, `setenv`.

When `-t` (target pane) is omitted, the active pane is used automatically.

### Protocol Discovery & Environment Setup

WTA finds Windows Terminal via the `WT_COM_CLSID` environment variable, which
WT propagates into every conpty child it spawns. You usually don't need to do
anything — just run `wta` inside a WT pane.

```bash
# Inspect the inherited value
wta pipe-id                               # print CLSID
wta pipe-id --json                        # JSON with metadata

# Re-export it into another shell session (rarely needed)
eval "$(wta set-env)"                     # bash/zsh
wta set-env -s powershell | Invoke-Expression   # PowerShell
wta set-env -s fish | source              # fish
wta set-env -s cmd                        # cmd (copy-paste output)
```

### Test connectivity

```bash
wta test-pipe
wta --test-pipe     # legacy flag, still works
```

Connects to the WT protocol, prints `list_windows` + `get_capabilities`.

## Protocol Connection

WTA discovers Windows Terminal via the `WT_COM_CLSID` environment variable. WT
sets this in its own environment at startup and propagates it to every conpty
shell, so any pane-launched process — including wta and wtcli — inherits it.

## Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `WT_COM_CLSID` | Yes* | Stringified GUID of WT's `TerminalProtocolComServer` COM class |
| `WTA_DEBUG_LOG` | No | Set to `0` to disable `wta-pipe-debug.log` |

\* Set automatically by WT when it spawns a conpty child. If you launch `wta` from outside WT, run `eval "$(wta set-env)"` to copy the value over (only useful when you've previously captured it from a WT shell).

## Global CLI Options

| Flag | Description |
|------|-------------|
| `--json` | Output raw JSON instead of human-readable tables |
| `--agent <CMD>` | Agent CLI command for ACP mode (default: `copilot --acp --stdio`) |

## TUI Controls

| Key | Action |
|-----|--------|
| Type + Enter | Send prompt to agent |
| Ctrl+C | Cancel streaming / quit |
| PageUp / PageDown | Scroll chat |
| F12 | Toggle debug panel (pipe traffic viewer) |
| Shift+PageUp/Down | Scroll debug panel |
| Y / N | Quick allow/reject on permission dialog |
| Up / Down / Enter | Navigate permission options |

## Debug Panel

Press **F12** to open a side panel showing all JSON-RPC messages between WTA and Windows Terminal in real time.

```
[3456.1] >>> {"type":"request","id":"3","method":"list_windows","params":{}}
[3456.1] <<< {"type":"response","id":"3","result":{"windows":[...]},"error":null}
```

- Green `>>>` = request sent to WT
- Cyan `<<<` = response from WT
- Shift+PageUp/Down to scroll

## Debug Logs

WTA writes structured logs under `%LOCALAPPDATA%\IntelligentTerminal\logs\`
(or the package-sandboxed equivalent when launched packaged):

| File | Contents |
|------|----------|
| `wta-main.log` | Main TUI runtime: lifecycle, agent events, protocol calls |
| `wta-agent-pane.log` | Agent-pane session (per-pane wta instance) |
| `wta-ensure-host.log` | Background host startup / COM connection |
| `wta-acp-debug.log` | ACP protocol debug trace |
| `wta-delegate.log` | `?<prompt>` delegation flow |
| `wta-attach.log` | Agent pane TUI in attach mode |

Set `WTA_LOG=debug` for verbose output (default: `info`). The F12 debug panel
in the TUI shows protocol traffic live without tailing log files.

## Project Structure

```
tools/wta/src/
+-- main.rs                    Entry point, CLI subcommands, protocol discovery
+-- app.rs                     TUI state machine, event loop, debug panel state
+-- event.rs                   Crossterm event reader
+-- theme.rs                   Color constants
+-- protocol/
|   +-- acp/client.rs          ACP client -- spawns agent, handles requests
+-- shell/
|   +-- shell_manager.rs       Terminal abstraction (local subprocess or WT pane)
|   +-- wt_channel/
|       +-- mod.rs             WtChannel trait definition
|       +-- cli_channel.rs     wtcli subprocess (CoCreateInstance via wtcli.exe) — all methods
+-- ui/
    +-- layout.rs              Main layout (+ debug panel split)
    +-- chat.rs                Message rendering
    +-- input.rs               Input box with cursor
    +-- status_bar.rs          Connection status, pane identity, debug hint
    +-- permission.rs          Permission modal dialog
    +-- debug_panel.rs         Protocol traffic viewer (F12)
```

## Development

### Prerequisites

- Rust toolchain (edition 2021)
- Windows Terminal with protocol server enabled (for WT integration)
- An ACP-compatible agent CLI (Copilot, Claude ACP adapter, etc.)

### Build and run

```bash
cd wta
cargo build

# Option 1: Auto-discover pipe (run inside Windows Terminal)
target/debug/wta.exe

# Option 2: Set env vars for the session
eval "$(target/debug/wta.exe set-env)"
target/debug/wta.exe
```

### Development workflow

1. Open Windows Terminal
2. Run `wta pipe-id` to verify `WT_COM_CLSID` is set
3. Run `wta` to start the TUI
4. Press F12 to open the debug panel and see all protocol traffic
5. Interact with the agent -- watch requests/responses flow in real time
6. Use `wta list-panes`, `wta capture-pane` etc. in another pane for debugging

### Adding a new WT protocol method

1. Declare the method in `src/cascadia/TerminalProtocol/TerminalProtocol.idl`
2. Implement it on `TerminalProtocolComServer` (`src/cascadia/WindowsTerminal/TerminalProtocolComServer.cpp`)
3. Add a `wtcli` subcommand in `src/tools/wtcli/main.cpp` that calls the new method
4. Add a `CliChannel::request` arm in `tools/wta/src/shell/wt_channel/cli_channel.rs` mapping a method name to the new `wtcli` subcommand
5. Rebuild WT, wtcli, and wta

## Architecture Notes

- **ShellManager** owns local terminals and the active `WtChannel`
- **CliChannel** shells out to `wtcli.exe` per call; `wtcli` does `CoCreateInstance` to reach WT's COM server. All methods, including `send_input` (via `wtcli send-keys`), go through this path.
- **Protocol discovery**: `WT_COM_CLSID` env var, inherited from the WT-spawned conpty
- **CLI subcommands** call `CliChannel::connect()` directly; no ShellManager needed
- **Pane identity** is discovered at startup via PID matching (list all panes, find ours)
- **Graceful degradation**: if the WT protocol is unavailable, WTA falls back to local-only mode (no WT tools, just local shell operations)
