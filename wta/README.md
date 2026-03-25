# WTA -- Windows Terminal Agent

A Rust TUI client, MCP tool server, and tmux-like CLI that connects AI agents to Windows Terminal.

## Quick Start

### Build

```bash
cd wta
cargo build
```

The binary is output to `wta/target/debug/wta.exe`.

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
The agent is expected to shell out to commands like `wta active-pane --json`, `wta list-panes --json`, and `wta send-keys --json`.
The CLI then talks to Windows Terminal over the named pipe.

### Run (MCP server mode)

Headless -- intended to be spawned by an agent as an MCP tool server.

```bash
wta mcp
wta --mcp          # legacy flag, still works
```

### tmux-like CLI

WTA exposes tmux-equivalent subcommands for controlling Windows Terminal from the shell. Useful for humans and AI agents that can shell out.

```bash
wta list-windows                          # list all WT windows
wta list-tabs                             # list tabs in first window
wta list-panes                            # list panes in first tab
wta active-pane                           # show focused pane
wta new-tab -c "pwsh.exe" -n "Build"      # create tab running pwsh
wta split-pane -H -c "pwsh.exe"           # split horizontal
wta send-keys -t 3 "cargo build" Enter    # send keys to pane 3
wta capture-pane -t 3 -l 50              # read last 50 lines from pane 3
wta kill-pane -t 3                        # close pane 3
wta pane-status -t 3                      # check if running
wta wait-for -t 3 --timeout 30           # wait for pane 3 to exit
wta list-windows --json                   # raw JSON output
```

Short aliases are supported: `lsw`, `lst`, `lsp`, `neww`, `splitw`, `send`, `capturep`, `killp`, `setenv`.

When `-t` (target pane) is omitted, the active pane is used automatically.

### Pipe Discovery & Environment Setup

WTA discovers the Windows Terminal pipe automatically via VT OSC sequences or `WT_PIPE_NAME` env var. You can also specify it explicitly:

```bash
# Discover pipe name
wta pipe-id                               # print pipe name
wta pipe-id --json                        # JSON with source info

# Set env vars for current shell session
eval "$(wta set-env)"                     # bash/zsh
wta set-env -s powershell | Invoke-Expression   # PowerShell
wta set-env -s fish | source              # fish
wta set-env -s cmd                        # cmd (copy-paste output)

# Explicit pipe name (overrides all discovery)
wta --pipe-name '\\.\pipe\WT-12345' list-windows
wta --pipe-name '\\.\pipe\WT-12345' --mcp
```

### Test pipe connectivity

```bash
wta test-pipe
wta --test-pipe     # legacy flag, still works
```

Connects to the WT pipe, authenticates, and prints `list_windows` + `get_capabilities`.

## Pipe Connection

WTA uses a priority chain to find the Windows Terminal pipe:

1. **`--pipe-name` CLI flag** (highest priority) -- works with all commands and modes
2. **VT OSC 9001 discovery** -- sends an escape sequence to WT, works in any pane
3. **`WT_PIPE_NAME` environment variable** -- set by WT for coordinator panes

## Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `WT_PIPE_NAME` | No* | Named pipe path, e.g. `\\.\pipe\WindowsTerminal-12345` |
| `WT_MCP_TOKEN` | No | Auth token. Empty string triggers dev bypass |
| `WTA_DEBUG_LOG` | No | Set to `0` to disable `wta-pipe-debug.log` |

\* Not required if running inside Windows Terminal (VT discovery) or using `--pipe-name`.

Windows Terminal sets `WT_PIPE_NAME` and `WT_MCP_TOKEN` automatically for processes launched through its protocol server. For other shells, use `eval "$(wta set-env)"` to auto-detect and export the variables.

## Global CLI Options

| Flag | Description |
|------|-------------|
| `--pipe-name <NAME>` | WT pipe path (overrides VT discovery and env var) |
| `--pipe-token <TOKEN>` | WT auth token (use with `--pipe-name`) |
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

## Debug Log Files

WTA writes three log files to the current working directory:

| File | Contents | Control |
|------|----------|---------|
| `wta-pipe-debug.log` | Named pipe request/response JSON (WT protocol layer) | `WTA_DEBUG_LOG=0` to disable |
| `wta-acp-debug.log` | ACP protocol events (session notifications, permissions, terminal ops) | Always on |
| `wta-mcp-debug.log` | MCP tool invocations with params and results (when running `--mcp`) | `WTA_DEBUG_LOG=0` to disable |

Tail them in a separate pane for live debugging:

```bash
# In another WT pane -- watch all three layers at once
tail -f wta-pipe-debug.log wta-acp-debug.log wta-mcp-debug.log
```

## Debugging MCP Protocol

When `wta mcp` is used as a headless tool server, the MCP traffic flows through three layers:

```
Agent CLI  <--MCP/stdio-->  wta --mcp  <--named pipe-->  Windows Terminal
                            ^                            ^
                    wta-mcp-debug.log            wta-pipe-debug.log
```

### Method 1: Log files (recommended)

The `wta --mcp` subprocess logs every tool call to `wta-mcp-debug.log`:

```
[1742123456.789] === MCP server starting ===
[1742123456.790] WT_PIPE_NAME=Some("\\\\.\pipe\\WindowsTerminal-12345")
[1742123457.001] >>> wt_list_windows()
[1742123457.015] <<< wt_list_windows: { "windows": [...] }
[1742123457.200] >>> wt_list_panes(tab_id=0)
[1742123457.210] <<< wt_list_panes: { "panes": [...] }
```

The pipe layer is also logged (from the MCP subprocess's own pipe connection) to `wta-pipe-debug.log`.

### Method 2: Run wta --mcp manually

Test MCP tools interactively by running `wta --mcp` directly and typing JSON-RPC on stdin:

```bash
set WT_PIPE_NAME=\\.\pipe\WindowsTerminal-12345
set WT_MCP_TOKEN=
wta --mcp
```

Then paste MCP JSON-RPC requests:

```json
{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"wt_list_windows","arguments":{}}}
```

Responses appear on stdout. Check `wta-mcp-debug.log` for the tool-level trace.

### Method 3: F12 debug panel (pipe layer only)

In ACP TUI mode, press F12 to see the named pipe traffic from the **main** wta process. This shows the pane identity discovery at startup but not the MCP subprocess's traffic (that goes to the log files).

## Project Structure

```
wta/src/
+-- main.rs                    Entry point, CLI subcommands, pipe discovery, pane identity
+-- app.rs                     TUI state machine, event loop, debug panel state
+-- event.rs                   Crossterm event reader
+-- theme.rs                   Color constants
+-- protocol/
|   +-- acp/client.rs          ACP client -- spawns agent, handles requests
|   +-- mcp/server.rs          MCP server -- 15 tools (shell + WT state + WT control)
+-- shell/
|   +-- shell_manager.rs       Terminal abstraction (local subprocess or WT pane)
|   +-- wt_channel/
|       +-- mod.rs             WtChannel trait definition
|       +-- pipe_channel.rs    Named pipe transport + debug emitter
|       +-- vt_channel.rs      VT OSC 9001 discovery (pipe name + token)
|       +-- types.rs           Wire format structs (WireRequest, WireResponse)
+-- ui/
    +-- layout.rs              Main layout (+ debug panel split)
    +-- chat.rs                Message rendering
    +-- input.rs               Input box with cursor
    +-- status_bar.rs          Connection status, pane identity, debug hint
    +-- permission.rs          Permission modal dialog
    +-- debug_panel.rs         Pipe traffic viewer (F12)
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

# Option 3: Explicit pipe name
target/debug/wta.exe --pipe-name '\\.\pipe\WindowsTerminal-12345'
```

### Development workflow

1. Open Windows Terminal
2. Run `wta pipe-id` to verify pipe discovery works
3. Run `wta` to start the TUI (or `eval "$(wta set-env)"` first if discovery fails)
4. Press F12 to open the debug panel and see all pipe traffic
5. Interact with the agent -- watch requests/responses flow in real time
6. Use `wta list-panes`, `wta capture-pane` etc. in another pane for debugging

### Adding a new WT protocol method

1. Add the handler in `ProtocolRequestHandler.cpp` (C++ side)
2. Add a wrapper in `shell_manager.rs` calling `self.wt()?.request("method_name", params)`
3. Add an MCP tool in `mcp/server.rs` if it should be agent-callable
4. Rebuild both WT and wta

### Adding a new MCP tool

1. Define a params struct with `#[derive(Deserialize, schemars::JsonSchema)]`
2. Add a `#[tool(...)]` method in the `#[tool_router]` impl block in `server.rs`
3. The tool calls into `ShellManager` for the actual work

## Architecture Notes

- **ShellManager** is shared between ACP and MCP modes via `Arc<ShellManager>`
- **PipeChannel** holds a single `Mutex<NamedPipeClient>` -- all requests are serialized
- **Pipe discovery priority**: `--pipe-name` CLI flag > VT OSC 9001 > `WT_PIPE_NAME` env var
- **CLI subcommands** are thin wrappers over `PipeChannel::request()` -- no ShellManager needed
- **Pane identity** is discovered at startup via PID matching (list all panes, find ours)
- **ACP WT contract**: In ACP mode with WT connected, WTA adds prompt context that tells the agent to use local `wta` CLI commands for WT inspection/control. MCP remains a separate headless server mode.
- **Graceful degradation**: If the WT pipe is unavailable, WTA falls back to local-only mode (no WT tools, just local shell operations)
