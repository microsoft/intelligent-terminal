# WTA (Windows Terminal Agent) — Architecture Plan & Progress

## Overview

WTA is a Rust application that provides three modes of operation:

- **ACP mode** (default): TUI client that calls an agent subprocess via the Agent Client Protocol (ACP) over stdio
- **MCP mode** (`wta mcp`): Headless MCP server exposing shell tools for an external agent to call
- **CLI mode** (subcommands): tmux-like commands (`wta list-panes`, `wta capture-pane`, etc.) that talk directly to the WT pipe -- useful for humans and agents that can shell out

Both ACP and MCP modes share a common **ShellManager** shell integration layer. CLI subcommands are thin wrappers over `PipeChannel::request()` that don't need ShellManager. A **WtChannel** abstraction enables bidirectional communication with Windows Terminal for tab/pane management and state queries.

```
                  ┌──────────────────────────┐
                  │   CLI Entry (main.rs)     │
                  │  --acp or --mcp dispatch  │
                  └─────┬──────────┬──────────┘
                        │          │
              ┌─────────▼──┐   ┌──▼───────────┐
              │  ACP Mode  │   │   MCP Mode    │
              │  (TUI)     │   │  (Headless)   │
              │  WTA=client│   │  WTA=server   │
              │  calls agent│  │  agent calls  │
              ├────────────┤   ├───────────────┤
              │ UI Layer   │   │ (no UI)       │
              │ ratatui    │   │               │
              ├────────────┤   ├───────────────┤
              │ ACP Client │   │ MCP Server    │
              │ Adapter    │   │ Adapter       │
              └─────┬──────┘   └──────┬────────┘
                    │                 │
                    └────────┬────────┘
                             │
                  ┌──────────▼──────────┐
                  │  Shell Integration  │
                  │  Layer (shared)     │
                  │  - ShellManager     │
                  │  - WtChannel (new)  │
                  └─────────────────────┘
```

---

## Directory Structure

```
src/
├── main.rs                         # CLI dispatch: subcommands, pipe discovery, TUI/MCP/CLI
├── app.rs                          # TUI app state + event loop (ACP mode only)
├── event.rs                        # Crossterm event reader
├── theme.rs                        # TUI theme constants
├── shell/                          # SHARED CORE (protocol-agnostic)
│   ├── mod.rs                      #   re-exports ShellManager, TerminalConfig
│   ├── shell_manager.rs            #   ShellManager — process spawn + terminal mgmt
│   └── wt_channel/                 #   Windows Terminal integration channel
│       ├── mod.rs                  #     WtChannel trait definition
│       ├── types.rs                #     WtAction, WtRequest, WtResponse
│       ├── vt_channel.rs           #     OSC 9001 escape sequence transport
│       └── pipe_channel.rs         #     Named pipe transport (stub)
├── protocol/                       # Protocol Adapters
│   ├── mod.rs                      #   pub mod acp; pub mod mcp;
│   ├── acp/                        #   ACP client mode
│   │   ├── mod.rs                  #     pub mod client;
│   │   └── client.rs               #     WtaClient + run_acp_client
│   └── mcp/                        #   MCP server mode
│       ├── mod.rs                  #     pub mod server;
│       └── server.rs               #     WtaMcpServer + tool definitions
└── ui/                             # TUI rendering (ACP mode only)
    ├── mod.rs
    ├── layout.rs
    ├── chat.rs
    ├── input.rs
    ├── status_bar.rs
    └── permission.rs
```

---

## Part 1: Dual-Mode Architecture (COMPLETE)

Steps 1–9 refactored WTA from an ACP-only client into the dual-mode architecture shown above.

| Step | Description | Status |
|------|-------------|--------|
| 1 | Create `shell/shell_manager.rs` — extract from `acp/terminal_mgr.rs`, make Arc-safe | Done |
| 2 | Create `shell/mod.rs` | Done |
| 3 | Move `acp/client.rs` → `protocol/acp/client.rs`, update to `Arc<ShellManager>` | Done |
| 4 | Create `protocol/mcp/server.rs` — MCP server using rmcp v1.1 | Done |
| 5 | Update `main.rs` — `--acp`/`--mcp` mode dispatch | Done |
| 6 | Create module files (`protocol/mod.rs`, `protocol/acp/mod.rs`, `protocol/mcp/mod.rs`) | Done |
| 7 | Delete old `acp/` directory | Done |
| 8 | Update `Cargo.toml` — add rmcp, serde | Done |
| 9 | Update `main.rs` module declarations (`mod shell; mod protocol;`) | Done |

### Key Design Decisions (Part 1)

- **ShellManager** uses `Mutex<HashMap>` for interior mutability so it can be wrapped in `Arc` and shared across async tasks
- **rmcp v1.1 API patterns**: `#[tool_router]` on tool impl blocks, `#[tool_handler]` on `ServerHandler` impl, `Parameters<T>` wrapper for tool params
- MCP tools: `run_command`, `create_terminal`, `get_terminal_output`, `wait_for_terminal`, `kill_terminal`

---

## Part 2: Windows Terminal Integration (DONE, COM-first)

The current implementation talks to Windows Terminal primarily through a local
COM server. WTA shells out to `wtcli.exe`, which calls `CoCreateInstance` on
the `IProtocolServer` interface registered by Windows Terminal. An inherited
duplex anonymous-pipe pair carries `send_input` only — every other method
goes through COM.

> Historical note: this part of the doc originally planned an OSC 9001 / named-pipe
> protocol (see "Wire Format" and "WT C++ Side" sections below). That approach was
> abandoned in favour of COM activation; sections describing the OSC route are
> kept for historical context but no longer reflect what was shipped.

```
WTA (Rust, child process)                Windows Terminal (C++)
─────────────────────────                ─────────────────────────

 ShellManager                         TerminalProtocolComServer
    │                                        ▲
    ├── Local (existing)                     │ CoCreateInstance(WT_COM_CLSID)
    │                                        │ via wtcli.exe subprocess
    └── WtChannel (trait)                    │
         │                                   │
         ├── CliChannel  ────────────────────┘  (primary, all methods)
         │
         └── PipeChannel (inherited duplex pipe pair, send_input only)
```

### WTA Rust-Side Steps

| Step | Description | Status |
|------|-------------|--------|
| 10 | Create `shell/wt_channel/types.rs` — WtAction enum, WtRequest, WtResponse structs | Done |
| 11 | Create `shell/wt_channel/mod.rs` — WtChannel trait (`request`, `is_available`) | Done |
| 12 | Create `shell/wt_channel/vt_channel.rs` — VT discovery helper | Done |
| 13 | Create `shell/wt_channel/pipe_channel.rs` — named pipe transport | Done |
| 14 | Enhance `shell_manager.rs` — add `wt_channel: Option<Arc<dyn WtChannel>>`, dispatch to WT or local, add tab/pane/query ops | Done |
| 15 | Update `main.rs` — wire CLI subcommands and pipe discovery | Done |
| 16 | Verify with `cargo build` and CLI/manual testing | Done |

### WtAction Enum (Defined in types.rs)

```rust
pub enum WtAction {
    // Terminal execution
    CreateTerminal { command, args, cwd },
    GetOutput { terminal_id },
    WaitForExit { terminal_id },
    Kill { terminal_id },
    Release { terminal_id },

    // Tab/Pane management
    NewTab { profile, command, cwd },
    SplitPane { direction, size, profile, command },
    FocusPane { pane_id },
    ClosePane { pane_id },

    // State queries
    GetCwd,
    GetScrollback { lines },
    GetShellMarks,
}
```

### OSC 9001 Wire Format

```
WTA → WT (stdout):  \x1b]9001;WtaReq;{json}\x07
WT → WTA (stdin):   \x1b]9001;WtaRes;{json}\x07
```

Uses the existing `WTAction` OSC 9001 namespace. `WtaReq`/`WtaRes` prefixes distinguish from the existing `CmdNotFound` sub-action.

### ShellManager Enhancement (Step 14 — Next)

```rust
pub struct ShellManager {
    terminals: Mutex<HashMap<String, ManagedTerminal>>,
    next_id: Mutex<u64>,
    wt_channel: Option<Arc<dyn WtChannel>>,  // NEW
}
```

- Existing ops (`create_terminal`, `get_output`, etc.) try WT channel first, fall back to local
- New WT-only ops: `new_tab`, `split_pane`, `focus_pane`, `close_pane`, `get_cwd`, `get_scrollback`, `get_shell_marks`

### VT / OSC Follow-Up

1. Keep VT OSC 9001 for discovery/bootstrap where it is useful.
2. Treat named pipes as the stable control path for CLI, ACP, and MCP integrations.
3. Add deeper VT request/response handling later only if an in-pane control channel becomes necessary.

---

## Part 2 (continued): Windows Terminal C++ Side (Step 17 — Future)

### Files to Modify in WT

| File | Change |
|------|--------|
| `src/terminal/adapter/adaptDispatch.cpp` | Add `WtaReq` branch to `DoWTAction()` |
| `src/terminal/adapter/ITerminalApi.hpp` | Add `HandleWtaRequest()` virtual method |
| `src/cascadia/TerminalCore/Terminal.hpp` | Add callback + setter for WTA requests |
| `src/cascadia/TerminalCore/TerminalApi.cpp` | Implement `HandleWtaRequest()` |
| `src/cascadia/TerminalControl/ControlCore.cpp` | Wire callback, raise WinRT event |
| `src/cascadia/TerminalControl/ControlCore.idl` | Declare WinRT event for WTA requests |
| `src/cascadia/TerminalApp/TerminalPage.cpp` | Subscribe to event, handle tab/pane operations |

### WT Response Path

```
HandleWtaRequest(json)
  → Terminal callback → ControlCore._handleWtaRequest()
  → process request:
      - State queries (GetCwd, GetScrollback, GetShellMarks) → answered directly from terminal buffer
      - Tab/Pane ops (NewTab, SplitPane) → raise WinRT event → TerminalPage handles using existing code
  → build response JSON
  → _ReturnOscResponse("9001;WtaRes;{response_json}")
  → ReturnResponse() → ConptyConnection::WriteInput() → WTA stdin
```

---

## Dependencies

```toml
[dependencies]
agent-client-protocol = "0.10"
tokio = { version = "1", features = ["full"] }
tokio-util = { version = "0.7", features = ["compat"] }
async-trait = "0.1"
anyhow = "1"
serde_json = "1"
clap = { version = "4", features = ["derive"] }
ratatui = "0.30"
crossterm = { version = "0.29", features = ["event-stream"] }
futures = "0.3"
unicode-width = "0.2"
textwrap = "0.16"
rmcp = { version = "1.1", features = ["server", "transport-io", "macros"] }
serde = { version = "1", features = ["derive"] }
```

---

## Verification Checklist

### Part 1 (Done)
- [x] `cargo build` — both modes compile
- [x] `wta --agent "copilot --acp --stdio"` — ACP TUI mode works
- [x] `wta --mcp` — starts headless, responds to MCP tool discovery

### Part 2 (Done)
- [x] `cargo build` — compiles with wt_channel module
- [x] `WT_COM_CLSID` discovery via inherited env var works
- [x] CliChannel (wtcli + COM) carries all methods
- [x] PipeChannel (inherited duplex pipe pair) carries `send_input`

### Part 3: CLI Subcommands (Done)
- [x] `cargo build` — compiles with all subcommands
- [x] `wta list-windows` — prints windows table
- [x] `wta list-tabs --json` — prints raw JSON
- [x] `wta capture-pane -l 5` — prints last 5 lines from active pane
- [x] `wta new-tab -c "pwsh" -n "Test"` — creates a new tab
- [x] `wta split-pane -v` — splits active pane vertically
- [x] `wta` (no args) — still launches ACP TUI mode
- [x] `wta --mcp` — still works (backward compat)
- [x] `wta pipe-id` — prints the discovered `WT_COM_CLSID`
- [x] `wta set-env` — prints eval-able `WT_COM_CLSID` export commands

### Future
- [ ] WT C++ side: build WT, verify DoWTAction receives WtaReq
- [ ] `focus_pane` protocol method + `select-pane` subcommand
- [ ] `rename-window`, `resize-pane`, `swap-pane` (need WT protocol support)
