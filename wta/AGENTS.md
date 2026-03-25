# WTA Agent Architecture

## What is WTA?

WTA (Windows Terminal Agent) is a Rust binary that bridges AI agent protocols with Windows Terminal.
It provides three interfaces:

- **ACP client** (default) -- TUI that spawns an agent CLI (Copilot, Claude, etc.) as a subprocess and communicates over ACP (Agent Client Protocol) via stdio JSON-RPC
- **MCP server** (`wta mcp`) -- headless tool server that an external agent calls to interact with shells and Windows Terminal
- **tmux-like CLI** (`wta list-panes`, `wta send-keys`, etc.) -- thin subcommands for controlling WT from the shell, useful for humans and agents that can shell out

Both ACP and MCP modes share a common ShellManager that routes operations to either local subprocesses or Windows Terminal panes via a named pipe. The CLI subcommands are lightweight wrappers that connect directly to the pipe without ShellManager.

## System Diagram

```
 Agent CLI (copilot/claude)     External agent       Human / AI shell-out
       |  ACP/stdio                  |  MCP/stdio         |  CLI subcommands
       v                             v                    v
 +-----------+                +-----------+        +-------------+
 | ACP Mode  |                | MCP Mode  |        | CLI Mode    |
 | (TUI)     |                | (headless)|        | (one-shot)  |
 | client.rs |                | server.rs |        | main.rs     |
 +-----+-----+                +-----+-----+        +------+------+
       |                             |                     |
       +---------------+-------------+                     |
                       |                                   |
                 ShellManager                        PipeChannel
                  |         |                        (direct call)
           Local subprocess  WtChannel (named pipe)        |
                                  |                        |
                                  +------------------------+
                                  |
                         Windows Terminal
                      ProtocolRequestHandler
```

## Protocol Stack

### ACP (Agent Client Protocol)

WTA acts as an ACP **client**. It spawns an agent CLI as a child process and speaks JSON-RPC 2.0 over stdin/stdout.

- Crate: `agent-client-protocol = "0.10"`
- Implementation: `src/protocol/acp/client.rs`
- The agent sends requests (create_terminal, permission, etc.) and WTA handles them
- Session notifications flow from agent to WTA: message chunks, tool calls, plans, status changes

Key ACP message types handled:
- `session/update` -- agent message chunks, tool calls, plan entries
- `request_permission` -- permission dialog with options (allow/reject)
- `create_terminal` / `terminal_output` / `wait_for_terminal_exit` -- agent-managed shells
- `release_terminal` / `kill_terminal` -- cleanup

### MCP (Model Context Protocol)

WTA acts as an MCP **server**. An external agent (via stdio) calls tools exposed by WTA.

- Crate: `rmcp = "1.1"` with `#[tool_router]` / `#[tool_handler]` macros
- Implementation: `src/protocol/mcp/server.rs`

Tools exposed (15 total):

| Category | Tool | Description |
|----------|------|-------------|
| Shell | `run_command` | Execute command, return stdout+exit code |
| Shell | `create_terminal` | Spawn persistent terminal session |
| Shell | `get_terminal_output` | Read buffered output |
| Shell | `wait_for_terminal` | Block until exit |
| Shell | `kill_terminal` | Terminate session |
| WT Query | `wt_list_windows` | All WT windows |
| WT Query | `wt_list_tabs` | Tabs in a window |
| WT Query | `wt_list_panes` | Panes in a tab |
| WT Query | `wt_get_active_pane` | Currently focused pane |
| WT Query | `wt_read_pane_output` | Terminal buffer text |
| WT Query | `wt_get_process_status` | Running/exit status |
| WT Control | `wt_create_tab` | Create new tab |
| WT Control | `wt_split_pane` | Split a pane |
| WT Control | `wt_send_input` | Type text into a pane |
| WT Control | `wt_close_pane` | Close a pane |

### Windows Terminal Protocol (Named Pipe)

WTA communicates with WT over a named pipe (`\\.\pipe\WindowsTerminal-<PID>`).

- Wire format: newline-delimited JSON-RPC 2.0
- Authentication: token-based (empty token = dev bypass)
- Implementation: `src/shell/wt_channel/pipe_channel.rs`

```
Request:  {"type":"request","id":"1","method":"list_windows","params":{}}\n
Response: {"type":"response","id":"1","result":{"windows":[...]},"error":null}\n
```

WT-side handler: `src/cascadia/WindowsTerminal/ProtocolRequestHandler.cpp`

Supported methods (18):
`authenticate`, `get_capabilities`, `get_active_pane`, `list_windows`, `list_tabs`, `list_panes`, `read_pane_output`, `get_process_status`, `get_session_variable`, `get_settings`, `create_tab`, `split_pane`, `close_pane`, `send_input`, `set_session_variable`, `set_settings`

### VT Escape Sequences (Planned)

OSC 9001 sequences for in-pane communication (no external pipe needed):

```
WTA -> WT:  \x1b]9001;WtaReq;{json}\x07
WT -> WTA:  \x1b]9001;WtaRes;{json}\x1b\\
```

C++ handler: `DoWTAction()` in `src/terminal/adapter/adaptDispatch.cpp`.
Currently returns a simple ack; full pane identity plumbing is future work.

## Agent Integration

### Copilot

```
wta --agent "copilot --acp --stdio"
```

WTA generates an MCP config file at startup pointing to `wta --mcp` and injects it:
`copilot --acp --stdio --additional-mcp-config @<temp_path>`

### Claude

```
wta --agent "claude --acp --stdio"
```

Uses `--mcp-config <path>` flag instead.

### Claude Agent ACP Adapter

The [zed-industries/claude-agent-acp](https://github.com/zed-industries/claude-agent-acp) adapter wraps Claude's SDK for ACP clients. WTA can connect to it as the agent subprocess.

### CLI Subcommands (tmux-like)

Agents that can shell out (or humans) can use WTA as a tmux-like CLI instead of MCP:

| tmux command | wta subcommand | Alias | WT protocol method |
|---|---|---|---|
| `list-sessions` | `list-windows` | `lsw` | `list_windows` |
| `list-windows` | `list-tabs` | `lst` | `list_tabs` |
| `list-panes` | `list-panes` | `lsp` | `list_panes` |
| `new-window` | `new-tab` | `neww` | `create_tab` |
| `split-window` | `split-pane` | `splitw` | `split_pane` |
| `send-keys` | `send-keys` | `send` | `send_input` |
| `capture-pane -p` | `capture-pane` | `capturep` | `read_pane_output` |
| `kill-pane` | `kill-pane` | `killp` | `close_pane` |
| `display -p #{pane_id}` | `active-pane` | — | `get_active_pane` |
| `wait-for` | `wait-for` | — | `get_process_status` (poll) |
| — | `pane-status` | — | `get_process_status` |
| — | `pipe-id` | — | (discovery only, no pipe call) |
| — | `set-env` | `setenv` | (discovery only, no pipe call) |

**send-keys** supports tmux key names: `Enter`, `Space`, `Escape`, `Tab`, `BSpace`, `C-c`, `C-d`, `C-{letter}`.

### Pipe Connection

WTA resolves the WT pipe using a priority chain:

1. `--pipe-name <NAME>` CLI flag (highest priority)
2. VT OSC 9001 discovery (works in any WT pane)
3. `WT_PIPE_NAME` environment variable

The `pipe-id` and `set-env` subcommands use the same chain but do not connect to the pipe -- they only resolve and print the info.

```bash
# Discover pipe
wta pipe-id
wta pipe-id --json

# Export to shell environment
eval "$(wta set-env)"                          # bash
wta set-env -s powershell | Invoke-Expression  # PowerShell

# Explicit pipe for any command
wta --pipe-name '\\.\pipe\WT-12345' list-windows
wta --pipe-name '\\.\pipe\WT-12345' --pipe-token 'abc' mcp
```

## Pane Identity

WTA discovers which WT pane it's running in via PID matching:

1. Call `list_windows` -> `list_tabs` -> `list_panes` over the pipe
2. Each pane has a `pid` field
3. Match against `std::process::id()`
4. Store `(pane_id, tab_id, window_id)` in App state
5. Displayed in status bar as `pane:X tab:Y`

## Logging

WTA log files are written to the **current working directory of the `wta` process**. They are not written to a fixed app-data directory.

Current log files:

- `wta-acp-debug.log` -- ACP client / agent session log
- `wta-mcp-debug.log` -- MCP server startup and tool log
- `wta-pipe-debug.log` -- Windows Terminal named-pipe request/response log

This matters when WTA is launched from a pane:

- If `wta` is launched while the pane cwd is `C:\Users\kaitao`, logs will be written under `C:\Users\kaitao\`
- If `wta` is launched while the pane cwd is `C:\Users\kaitao\codes\agentic-terminal\wta`, logs will be written there instead

For the March 24, 2026 run, the logs were found at:

- `C:\Users\kaitao\wta-acp-debug.log`
- `C:\Users\kaitao\wta-pipe-debug.log`

Rule of thumb: if a log is "missing" from the repo, check the directory from which the pane launched `wta` first.

Current limitation:

- ACP child-process stderr is currently discarded, so some crashes can appear in the UI as a generic ACP shutdown error without a full root-cause trace in the debug logs.

## Key Crates

| Crate | Version | Purpose |
|-------|---------|---------|
| `agent-client-protocol` | 0.10 | ACP client library |
| `rmcp` | 1.1 | MCP server framework |
| `tokio` | 1 | Async runtime |
| `ratatui` | 0.30 | TUI rendering |
| `crossterm` | 0.29 | Terminal I/O |
| `clap` | 4 | CLI parsing |
| `serde_json` | 1 | JSON handling |
