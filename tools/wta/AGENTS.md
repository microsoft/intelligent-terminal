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

---

## Session liveness + Enter routing (F2 view)

The F2 "Session management" view (a.k.a. `/sessions`) lists every
agent session that WTA knows about — both currently connected ("Live")
and replayed from on-disk history ("Historical" / "Ended"). Enter and
Shift+Enter on a row are routed through a closed-form state machine
that splits the legacy one-dimensional `AgentStatus` into a
**two-dimensional** model.

### The two axes

* **Activity** — `Idle | Working | Attention | Error`. Surfaces what
  the connected agent is *doing* right now. Driven by ACP
  `session/update` (`ToolCall`, `ToolCallUpdate`) on Class A
  sessions, and by shell-integration hooks on Class B.

* **Liveness** — `Live { pane_session_id } | Ended | Historical`.
  Surfaces whether the session is currently attached to a process.
  `Historical` = reconstructed from disk only; `Ended` = was Live
  in this WTA process at some point, then closed.

The legacy `AgentStatus` field is still the storage; `activity()`
and `liveness()` derive from it (see `agent_sessions.rs`).

### Class A vs Class B

* **Class A** (`SessionOrigin::AgentPane`) — created by WTA on behalf
  of an Intelligent Terminal agent pane (recorded in
  `agent-pane-sessions.jsonl`). For these rows the natural Enter
  target is the *same* agent pane; the natural Resume target is ACP
  `session/load` so the conversation rehydrates in-place.

* **Class B** (`SessionOrigin::Unknown`) — user ran the CLI directly
  (`copilot`, `claude`, `gemini`) in a normal pane. The natural
  Enter target is to focus that pane; the natural Resume target is
  the CLI's own `--resume` flag (because the CLI owns the
  conversation, not WTA).

### Liveness data sources (composite)

* **Class A** is composite: `liveness = Live` requires *both*
  `alive_mirror.lookup(sid).is_some()` (the master-pushed registry
  mirror via `intellterm.wta/session_added|removed` ext
  notifications) *and* no local `PaneClosed` tombstone. Local
  pane-close events trump the mirror so the row flips to `Ended`
  immediately, even if master's `session_removed` notification
  hasn't landed yet.

* **Class B** is driven entirely by hooks + WT pane events
  (`SessionStarted` / `SessionStopped` / `PaneClosed`).

### Cold-startup race

History scan and alive-mirror bootstrap arrive in either order. Both
paths post `AppEvent::AliveJoinUpgrade`, which calls
`AgentSessionRegistry::apply_alive_session_join` to upgrade
**Historical** rows whose ACP session_id is in the alive mirror to
`LivenessState::Live`. **Ended** rows (locally tombstoned by a
`PaneClosed` event in this process) are **not** upgraded — local
tombstones are authoritative, so a stale `session_added` broadcast
arriving after `PaneClosed` cannot resurrect a row that has no
demotion path back. Live rows are never demoted — preserves
tool/attention state. See `agent_sessions.rs::apply_alive_session_join`.

### Steady-state alive-mirror updates

Every `intellterm.wta/session_added` notification from master also
runs the incremental join synchronously in `app.rs` (calls
`apply_alive_session_join([(sid, pane)])` before the async mirror
upsert), so a Historical row matched by an arriving alive broadcast
becomes Live in the same tick. Every `intellterm.wta/session_removed`
notification calls `apply_master_session_ended(sid)` — the
counterpart of `apply_alive_pane_snapshot` for a single explicit
disappearance — which demotes a Live row to Ended, clears the pane
binding, and prunes `known_alive_panes`. Without these two
synchronous reducer calls, F2 rows would stay frozen at whatever
state the last bootstrap snapshot saw.

### Enter / Shift+Enter dispatch

The pure-function core is `session_mgmt::decide_enter_action`. It
takes a `RowSnapshot` (origin + liveness + cli + agent capabilities)
and a `shift` boolean, and returns one of:

* `Focus { pane_session_id }` — hand off to `wtcli focus-pane`
  (which transparently restores a stashed agent pane after PR A).
* `ResumeInAgentPane { key, cli }` — new tab + agent pane + ACP
  `session/load(key)` (requires `loadSession` capability).
* `ResumeCliFlag { key, cli }` — new tab + plain pane running
  `<cli> --resume <key>` (requires the CLI to advertise a resume
  flag — Codex doesn't).
* `NotResumable { reason }` — surface a system message.

The table (matching `session_mgmt.rs` and the plan):

| Row state                  | Enter            | Shift+Enter      |
| -------------------------- | ---------------- | ---------------- |
| Any Live (Class A or B)    | Focus            | Focus (same)     |
| Class A dead (Ended/Hist)  | ResumeInAgentPane| ResumeCliFlag    |
| Class B dead (Ended/Hist)  | ResumeCliFlag    | ResumeInAgentPane|
| Unknown CLI                | NotResumable     | NotResumable     |
| Missing capability         | NotResumable     | NotResumable     |

Shift on Live is intentionally identical to Enter — agents forbid two
clients on one session, so any "force second copy" attempt would just
error out. Shift is a safety alias there.

### Dispatch boundary

`App::activate_agent_session_with_shift` (`app.rs`) is the only
caller of `decide_enter_action` from production code. It then
dispatches into the existing helpers (`dispatch_focus_pane`,
`dispatch_resume_in_agent_pane`, `dispatch_resume`) — those own
all the side effects:

* phantom-on-disk guard (`key_is_resumable_on_disk`),
* optimistic `SessionEvent::ResumeDispatched` state flip,
* `resume_in_new_agent_tab` event publish (for the WT side to open
  a new tab + reconcile the agent pane),
* on-failure `PaneClosed` rebroadcast (so a stuck Live row
  transitions to Ended after `wtcli focus-pane` returns NotFound).
