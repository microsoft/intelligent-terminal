# WTA Agent Architecture

## What is WTA?

WTA (Windows Terminal Agent) is a Rust binary that bridges AI agent protocols with Windows Terminal.
It provides three interfaces:

- **ACP client** (default) -- TUI that spawns an agent CLI (Copilot, Claude, Gemini, Codex, or a custom command) and communicates over ACP via stdio JSON-RPC.
- **MCP server** (`wta mcp`) -- headless tool server that an external agent calls to interact with shells and Windows Terminal.
- **CLI helpers** (`wta list-panes`, `wta capture-pane`, `wta new-tab`, etc.) -- thin commands for humans and agents that can shell out. Direct keystroke injection is not exposed by the CLI.

ACP and MCP modes share `ShellManager`, which routes operations to either local subprocesses or Windows Terminal panes. WT pane operations use a `WtChannel` abstraction with a single implementation today:

- `CliChannel` shells out to `wtcli.exe`, which calls WT's COM `IProtocolServer`. All WT operations — including `send_input` (via `wtcli send-keys`) — go through this path.

## System Diagram

```
 Agent CLI (copilot/claude)     External agent       Human / AI shell-out
       |  ACP/stdio                  |  MCP/stdio         |  CLI helpers
       v                             v                    v
 +-----------+                +-----------+        +-------------+
 | ACP Mode  |                | MCP Mode  |        | CLI Mode    |
 | (TUI)     |                | (headless)|        | (one-shot)  |
 | client.rs |                | server.rs |        | main.rs     |
 +-----+-----+                +-----+-----+        +------+------+
       |                             |                     |
       +---------------+-------------+                     |
                       |                                   |
                 ShellManager                              |
                       |                                   |
                  CliChannel <-------------------------+---+
                       |
                       v
              wtcli.exe -> COM IProtocolServer
                       |
                       v
              TerminalProtocolComServer
                       |
                       v
              Windows Terminal
```

## Protocol Stack

### ACP (Agent Client Protocol)

WTA acts as an ACP **client**. It spawns an agent CLI as a child process and speaks JSON-RPC 2.0 over stdin/stdout.

- Crate: `agent-client-protocol = "0.10"`
- Implementation: `src/protocol/acp/client.rs`
- The agent sends requests (`create_terminal`, `request_permission`, etc.) and WTA handles them.
- Session notifications flow from agent to WTA: message chunks, tool calls, plans, and status changes.

Key ACP message types handled:

- `session/update` -- agent message chunks, tool calls, plan entries
- `request_permission` -- permission dialog with options (allow/reject)
- `create_terminal` / `terminal_output` / `wait_for_terminal_exit` -- agent-managed shells
- `release_terminal` / `kill_terminal` -- cleanup

### MCP (Model Context Protocol)

WTA acts as an MCP **server**. An external agent calls tools exposed by WTA over stdio.

- Crate: `rmcp = "1.1"` with `#[tool_router]` / `#[tool_handler]` macros
- Implementation: `src/protocol/mcp/server.rs`

Tools exposed:

| Category | Tool | Description |
|----------|------|-------------|
| Shell | `run_command` | Execute command, return stdout and exit code |
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
| WT Control | `wt_send_input` | Type text into a pane via `wtcli send-keys` / COM `SendInput` |
| WT Control | `wt_close_pane` | Close a pane |

### WT COM Protocol

All WT operations flow through `wtcli.exe` to WT's out-of-process COM server.

- Client wrapper: `src/shell/wt_channel/cli_channel.rs`
- CLI executable: `src/tools/wtcli/main.cpp`
- IDL: `src/cascadia/TerminalProtocol/TerminalProtocol.idl`
- WT-side server: `src/cascadia/WindowsTerminal/TerminalProtocolComServer.cpp`
- Discovery: `WT_COM_CLSID`, injected into panes by WT

The COM surface exposes reads and mutations, including `list_*`, `read_pane_output`, `create_tab`, `split_pane`, `close_pane`, `focus_pane`, `send_input` (via `wtcli send-keys`), and event subscribe/publish.

## Agent Integration

### Copilot

```
wta --agent "copilot --acp --stdio"
```

WTA generates an MCP config file at startup pointing to `wta mcp` and injects it with Copilot's `--additional-mcp-config` option.

### Claude and Codex

Claude and Codex are launched through ACP adapters:

```
wta --agent "npx -y @zed-industries/claude-code-acp"
wta --agent "npx -y @zed-industries/codex-acp"
```

The Terminal settings layer resolves the built-in agent IDs to these adapter commands.

### Gemini

```
wta --agent "gemini --experimental-acp"
```

### Custom agents

Custom agents are configured through the Terminal settings (`custom:<cmd>` plus the stored custom command). WTA receives the resolved command line via `--agent`.

## CLI Helpers

Agents that can shell out, and humans debugging WTA, can use WTA as a small WT helper CLI:

| Command | Alias | WT protocol method |
|---|---|---|
| `list-windows` | `lsw` | `list_windows` |
| `list-tabs` | `lst` | `list_tabs` |
| `list-panes` | `lsp` | `list_panes` |
| `new-tab` | `neww` | `create_tab` |
| `split-pane` | `splitw` | `split_pane` |
| `capture-pane` | `capturep` | `read_pane_output` |
| `kill-pane` | `killp` | `close_pane` |
| `active-pane` | -- | `get_active_pane` |
| `wait-for` | -- | delegated to `wtcli wait-for` |
| `pane-status` | -- | `get_process_status` |
| `listen` | `mon` | COM event subscribe |

## Connection Discovery

`CliChannel` uses `wtcli.exe`, and `wtcli.exe` discovers WT through `WT_COM_CLSID`. WT injects this environment variable into pane shells.

`pipe-id` and `set-env` are diagnostic subcommands that surface the inherited `WT_COM_CLSID` value. They should not be described as a security boundary.

## Pane Identity

WTA discovers which WT pane it is running in by PID matching:

1. Call `list_windows` -> `list_tabs` -> `list_panes` through `CliChannel`.
2. Each pane has a `pid` field.
3. Match against `std::process::id()`.
4. Store `(pane_id, tab_id, window_id)` in app state.
5. Display the identity in the status bar.

## Logging

WTA writes structured logs under the Intelligent Terminal runtime directory:

```
%LOCALAPPDATA%\IntelligentTerminal\logs\
```

When running packaged, `%LOCALAPPDATA%` is redirected to the package sandbox.

Current process logs include:

- `wta-main.log` -- default ACP TUI mode
- `wta-delegate.log` -- `wta delegate`
- `wta-install-hooks.log` -- hook installation and removal

The log level is controlled by `WTA_LOG`; if unset, WTA defaults to a debug filter in `logging.rs`.

## Build Rule

For normal local WTA development, always produce the binary at `tools/wta/target/debug/wta.exe`.

- Before running `cargo build` for WTA, kill any active `wta.exe` processes first. A live shared-host session can keep `target/debug/wta.exe` locked and make the build fail with `Access is denied`.
- Preferred PowerShell sequence:
  - `Get-Process wta -ErrorAction SilentlyContinue | Stop-Process -Force`
  - `cargo build --manifest-path tools/wta/Cargo.toml`
- Do not switch to an alternate `--target-dir` just to work around a locked `wta.exe` unless that is explicitly the task. The default expectation is to refresh `tools/wta/target/debug/wta.exe`.

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
