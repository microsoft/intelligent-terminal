# WTA (Windows Terminal Agent) — Comprehensive Architecture

## Overview

WTA is a Rust application (crate: both `lib` and `bin`) that bridges AI agent protocols
(ACP, MCP) with Windows Terminal's internal capabilities (panes, tabs, buffers, input).
It communicates with Windows Terminal through an abstracted **channel** layer that
supports multiple transports: VT escape sequences (OSC 9001), named pipes, or
direct in-process calls.

```
┌─────────────────────────────────────────────────────────────────┐
│                      Windows Terminal (C++)                      │
│                                                                 │
│   ProtocolRequestHandler  ◄── single implementation             │
│        ▲         ▲         ▲                                    │
│        │         │         │                                    │
│   VtTransport  PipeTransport  DirectTransport                   │
│   (per-pane)   (named pipe)   (lib/in-process)                  │
└────────┼─────────┼────────────┼─────────────────────────────────┘
         │         │            │
    OSC 9001    named pipe   in-proc pipe
   (stdout/     (\\.\pipe\)   (fd pair)
    stdin)
         │         │            │
┌────────┼─────────┼────────────┼─────────────────────────────────┐
│   VtChannel  PipeChannel  DirectChannel                         │
│        └─────────┼────────────┘                                 │
│           WtChannel trait                                        │
│                  │                                               │
│         ┌────────┴─────────┐                                    │
│         │    WTA Core      │                                    │
│         │  (Rust, shared)  │                                    │
│         └──────────────────┘                                    │
│                WTA (exe or lib)                                  │
└─────────────────────────────────────────────────────────────────┘
```

---

## Deployment Modes

### Mode A: EXE + ACP (running inside a WT pane)

WTA runs as `wta.exe` inside a Windows Terminal pane. It spawns an agent CLI
subprocess (e.g. `copilot --acp --stdio`) and acts as the ACP client. The user
interacts with WTA via the pane's terminal (ratatui TUI).

Communication with WT uses **VtChannel** (preferred) or **PipeChannel**:
- VtChannel: WTA writes OSC 9001 requests to stdout, WT's VT parser intercepts
  them and routes to the protocol handler. WT responds by writing OSC 9001
  responses to WTA's stdin. The VT transport automatically knows which pane
  the request came from — no explicit `pane_id` needed for self-referencing ops.
- PipeChannel: WTA connects to `\\.\pipe\WindowsTerminal-<PID>` using
  `WT_PIPE_NAME` / `WT_MCP_TOKEN` env vars. Requires explicit `pane_id` in all
  pane-specific requests.

```
User ←→ WTA pane (ratatui TUI)
              │
              ├── ShellManager → agent CLI subprocess (ACP over stdio)
              │                   e.g. "copilot --acp --stdio"
              │
              └── WtChannel (VT or Pipe) → WT protocol handler
                    → create_tab, read_pane_output, send_input, etc.
```

### Mode B: EXE + MCP (headless, no TUI)

WTA runs as a headless MCP server. An external AI agent (Claude, Copilot, etc.)
connects to WTA via MCP over stdio. WTA translates MCP tool calls into WT
protocol actions via **PipeChannel** (only option — WTA is not in a pane,
so VT is not available).

```
External agent ←MCP stdio→ WTA (headless)
                                │
                                └── PipeChannel → WT protocol handler
```

### Mode C: LIB (embedded in Windows Terminal)

WTA is compiled as a Rust library and linked into Windows Terminal. The existing
`AcpConnection` (C++) becomes a thin wrapper that:
1. Creates an in-process pipe pair (stdin/stdout for the library)
2. Calls `wta_lib_init(...)` with the pipe endpoints
3. Forwards `WriteInput()` → library's stdin (user keystrokes)
4. Reads library's stdout → scans for OSC 9001:
   - Regular VT output → forwards to `TerminalOutput` event (renders in pane)
   - OSC 9001 `WtaReq` → extracts JSON, routes to ProtocolRequestHandler,
     writes OSC 9001 `WtaRes` back to library's stdin

The library internally uses **VtChannel** (or a DirectChannel that behaves
identically) — it writes OSC 9001 to stdout and reads responses from stdin,
exactly as in Mode A. This means **the same Rust code runs in all modes**
with zero conditional compilation.

```
AcpConnection (thin C++ wrapper)
    │  in-process pipe
    ▼
WTA Library (Rust)
    ├── ShellManager → agent CLI subprocess (ACP over stdio)
    └── VtChannel (writes OSC to pipe) → AcpConnection intercepts
            → ProtocolRequestHandler → TerminalPage.Protocol bridge
```

---

## WTA Rust Crate Structure

```
tools/wta/
├── Cargo.toml                  # [lib] + [[bin]] targets
├── src/
│   ├── lib.rs                  # Public API for lib mode (C-ABI)
│   ├── main.rs                 # CLI entry: --acp / --mcp dispatch
│   │
│   ├── core/                   # Shared core logic
│   │   ├── mod.rs
│   │   ├── agent.rs            # Agent lifecycle: spawn, handshake, prompt loop
│   │   ├── acp_handler.rs      # ACP request handling:
│   │   │                       #   terminal/create  → channel.request(CreateTab)
│   │   │                       #   terminal/output   → channel.request(ReadPaneOutput)
│   │   │                       #   terminal/kill     → channel.request(ClosePane)
│   │   │                       #   terminal/wait     → channel.request(GetProcessStatus)
│   │   │                       #   permission/request → UI prompt or auto-approve
│   │   └── vt_render.rs        # VT output generation (agent text, tool calls, plans)
│   │
│   ├── channel/                # Channel abstraction (WTA side)
│   │   ├── mod.rs              # WtChannel trait definition
│   │   ├── types.rs            # WtAction enum (= protocol wire format), WtResponse
│   │   ├── vt_channel.rs       # OSC 9001 over stdout/stdin
│   │   └── pipe_channel.rs     # Named pipe client to WT protocol server
│   │
│   ├── protocol/               # Agent-facing protocol adapters
│   │   ├── mod.rs
│   │   ├── acp/
│   │   │   ├── mod.rs
│   │   │   └── client.rs       # ACP client: wraps agent CLI, dispatches to core
│   │   └── mcp/
│   │       ├── mod.rs
│   │       └── server.rs       # MCP server: exposes WT actions as MCP tools
│   │
│   └── ui/                     # TUI rendering (ACP exe mode only)
│       ├── mod.rs
│       ├── layout.rs
│       ├── chat.rs
│       ├── input.rs
│       ├── status_bar.rs
│       └── permission.rs
```

---

## Channel Abstraction

### WTA Side (Rust) — `WtChannel` trait

```rust
#[async_trait]
pub trait WtChannel: Send + Sync {
    /// Send a protocol request and wait for the response.
    async fn request(&self, action: WtAction) -> anyhow::Result<WtResponse>;

    /// Whether the channel is connected and ready.
    fn is_available(&self) -> bool;

    /// For VT channel: returns the pane_id of the pane WTA is running in.
    /// For pipe channel: returns None (caller must supply pane_id explicitly).
    fn self_pane_id(&self) -> Option<String>;
}
```

### WtAction Enum (= Protocol Wire Format)

The `WtAction` enum maps 1:1 to the terminal protocol's methods. This is the
single source of truth for all operations WTA can perform on WT:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum WtAction {
    // === Authentication ===
    Authenticate { token: String },

    // === Query Operations ===
    GetCapabilities,
    GetActivePane,
    ListWindows,
    ListTabs { window_id: Option<String> },
    ListPanes { tab_id: Option<String>, window_id: Option<String> },
    ReadPaneOutput {
        pane_id: Option<String>,    // None = self (VT channel only)
        source: Option<String>,     // "scrollback" (default) or "screen"
        max_lines: Option<i32>,
    },
    GetProcessStatus { pane_id: String },
    GetSessionVariable { pane_id: String, name: String },
    GetSettings,

    // === Mutation Operations ===
    CreateTab {
        window_id: Option<String>,
        profile: Option<String>,
        commandline: Option<String>,
        title: Option<String>,
        cwd: Option<String>,
        inject_mcp_credentials: Option<bool>,
        background: Option<bool>,
    },
    SplitPane {
        pane_id: Option<String>,    // None = self (VT channel only)
        direction: Option<String>,  // "left", "right", "up", "down"
        size: Option<f32>,
        profile: Option<String>,
        commandline: Option<String>,
        inject_mcp_credentials: Option<bool>,
        background: Option<bool>,
    },
    ClosePane { pane_id: String },
    SendInput { pane_id: String, text: String },
    SetSessionVariable { pane_id: String, name: String, value: Option<String> },
}
```

When using VtChannel, fields like `pane_id` in `ReadPaneOutput` and `SplitPane`
can be `None` — the WT-side VtTransport fills in the source pane automatically.
When using PipeChannel, these fields must be explicitly provided.

### VtChannel Implementation

```rust
pub struct VtChannel {
    next_id: AtomicU64,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<WtResponse>>>>,
    self_pane_id: Mutex<Option<String>>,  // learned from first response
}

// Wire format:
// WTA → WT (stdout):  \x1b]9001;WtaReq;{json}\x07
// WT → WTA (stdin):   \x1b]9001;WtaRes;{json}\x07
```

### PipeChannel Implementation

```rust
pub struct PipeChannel {
    pipe: tokio::net::windows::named_pipe::NamedPipeClient,
    token: String,
    authenticated: AtomicBool,
    next_id: AtomicU64,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<WtResponse>>>>,
}

// Connects to \\.\pipe\WindowsTerminal-<PID>
// Authenticates with WT_MCP_TOKEN
// Wire format: JSON-RPC lines (same as protocol branch)
```

---

## WT Side — Protocol Request Handler

### Transport Abstraction

The existing `ProtocolRequestHandler` from the protocol branch is reused as-is,
with one extension: a `ProtocolSourceContext` that carries transport metadata.

```cpp
struct ProtocolSourceContext {
    // Set by VtTransport: the protocol ID of the pane that sent the request.
    // Allows pane_id to be omitted in requests (defaults to source pane).
    std::optional<uint32_t> sourcePaneProtocolId;
};

// Extended signature (minor change to existing code):
Json::Value HandleRequest(const Json::Value& request,
                          bool& isAuthenticated,
                          const ProtocolSourceContext& context = {});
```

### Three Transports

**PipeTransport** — Already implemented as `TerminalProtocolServer` in the
protocol branch. No changes needed.

**VtTransport** — New. Intercepts OSC 9001 `WtaReq` sequences in the VT
parser path. Knows which pane the sequence came from (VT parsing is per-pane).

WT-side files to modify:
- `adaptDispatch.cpp`: Add `WtaReq` branch to `DoWTAction()` (or similar)
- `ITerminalApi.hpp`: Add virtual method for WTA requests
- `Terminal.hpp` / `TerminalApi.cpp`: Callback to ControlCore
- `ControlCore.cpp`: Raise event → TerminalPage handles it
- Response path: `ControlCore` → `ConptyConnection::WriteInput()` → WTA's stdin

**DirectTransport** — For lib mode. AcpConnection intercepts OSC 9001 from
the in-process pipe and calls `ProtocolRequestHandler` directly, then writes
the response back. This is implemented entirely within AcpConnection — the
handler doesn't know or care that the transport is in-process.

---

## ShellManager (Agent CLI Only)

ShellManager manages **exactly one** agent CLI subprocess per WTA process.
Multiple agents require multiple WTA instances.

```rust
pub struct ShellManager {
    agent: Mutex<Option<ManagedAgent>>,
}

struct ManagedAgent {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,   // WTA writes ACP JSON-RPC
    stdout: tokio::process::ChildStdout, // WTA reads ACP JSON-RPC
}

impl ShellManager {
    pub fn new() -> Self { ... }

    /// Spawn the agent CLI. Only one at a time.
    pub async fn spawn_agent(&self, command: &str, args: &[&str]) -> Result<()> { ... }

    /// Get the ACP stdio streams for the running agent.
    pub fn agent_streams(&self) -> Result<(&ChildStdin, &ChildStdout)> { ... }

    /// Kill and clean up the agent subprocess.
    pub fn kill_agent(&self) -> Result<()> { ... }
}
```

ShellManager has **nothing to do with ACP `terminal/create`**. When an agent
asks WTA to create a terminal (via ACP `terminal/create`), WTA sends a
`CreateTab` or `SplitPane` action through the WtChannel → WT creates a real,
visible, user-interactive pane.

---

## ACP Request Mapping

When the agent CLI sends ACP requests, WTA's `acp_handler` maps them to
WtChannel actions:

| ACP Request | WtChannel Action | Notes |
|-------------|-----------------|-------|
| `terminal/create` | `CreateTab` or `SplitPane` | Creates a real WT pane. The pane_id returned becomes the ACP terminal_id. |
| `terminal/output` | `ReadPaneOutput` | Reads the pane's text buffer via the protocol. |
| `terminal/waitForExit` | `GetProcessStatus` (poll) | Polls until process state = exited. |
| `terminal/kill` | `ClosePane` | Closes the pane. |
| `terminal/release` | (no-op or `ClosePane`) | Releases tracking; optionally closes pane. |
| `permission/request` | (local) | Handled by WTA's UI/permission system, not sent to WT. |

---

## MCP Tool Mapping

When WTA runs in MCP server mode, external agents call MCP tools that map
to WtChannel actions:

| MCP Tool | WtChannel Action |
|----------|-----------------|
| `create_tab` | `CreateTab` |
| `split_pane` | `SplitPane` |
| `close_pane` | `ClosePane` |
| `send_input` | `SendInput` |
| `read_pane_output` | `ReadPaneOutput` |
| `get_process_status` | `GetProcessStatus` |
| `list_windows` | `ListWindows` |
| `list_tabs` | `ListTabs` |
| `list_panes` | `ListPanes` |
| `get_active_pane` | `GetActivePane` |
| `get_session_variable` | `GetSessionVariable` |
| `set_session_variable` | `SetSessionVariable` |
| `get_settings` | `GetSettings` |
| `run_command` | `CreateTab` + `SendInput` + poll `ReadPaneOutput` |

---

## Lib Mode — C API

```rust
// lib.rs — C-ABI for embedding in Windows Terminal

/// Opaque handle to a WTA instance.
pub struct WtaHandle { /* runtime, agent, channel, ... */ }

/// Configuration for library initialization.
#[repr(C)]
pub struct WtaLibConfig {
    pub stdin_fd: RawFd,       // WTA reads user input from here
    pub stdout_fd: RawFd,      // WTA writes VT output + OSC 9001 here
    pub agent_cli: *const c_char,
    pub working_dir: *const c_char,
    pub initial_prompt: *const c_char,  // may be null
}

/// Initialize WTA in library mode. Spawns agent, begins ACP session.
/// The caller (AcpConnection) should read stdout_fd for VT output,
/// intercept OSC 9001 sequences, and route them to the protocol handler.
#[no_mangle]
pub extern "C" fn wta_init(config: WtaLibConfig) -> *mut WtaHandle { ... }

/// Feed user input bytes (keystrokes from AcpConnection::WriteInput).
#[no_mangle]
pub extern "C" fn wta_write_input(handle: *mut WtaHandle, data: *const u8, len: usize) { ... }

/// Shut down the WTA instance and clean up.
#[no_mangle]
pub extern "C" fn wta_shutdown(handle: *mut WtaHandle) { ... }
```

AcpConnection (C++) in lib mode is ~200 lines:
1. Set up in-process pipe pair
2. Call `wta_init()` with pipe fds
3. Forward `WriteInput()` → pipe
4. Read pipe output in a thread:
   - Regular VT → `TerminalOutput.raise()`
   - OSC 9001 `WtaReq;{json}` → `ProtocolRequestHandler::HandleRequest()`
     → write `\x1b]9001;WtaRes;{response_json}\x07` back to pipe

---

## AcpConnection in Lib Mode (Thin Wrapper)

```cpp
// Simplified AcpConnection — delegates all ACP logic to WTA library
struct AcpConnection : AcpConnectionT<AcpConnection>, BaseTerminalConnection<AcpConnection> {
    void Initialize(const ValueSet& settings);
    void Start();
    void WriteInput(const array_view<const char16_t> buffer);
    void Resize(uint32_t rows, uint32_t columns);
    void Close() noexcept;

    til::event<TerminalOutputHandler> TerminalOutput;

private:
    // In-process pipes to WTA library
    wil::unique_hfile _toLibWrite;    // WriteInput → WTA stdin
    wil::unique_hfile _fromLibRead;   // WTA stdout → read here

    // WTA library handle
    WtaHandle* _wtaHandle{ nullptr };

    // Reader thread: scans output for VT vs OSC 9001
    wil::unique_handle _readerThread;
    DWORD _ReaderThread();

    // Protocol handler reference (for routing OSC 9001 requests)
    // Obtained from WindowEmperor or passed via settings
    ProtocolRequestHandler* _protocolHandler{ nullptr };
};
```

---

## WT C++ Side — Summary of Changes

### From Protocol Branch (reuse as-is)

| Component | New/Modified | Lines |
|-----------|-------------|-------|
| `TerminalProtocolServer.h/cpp` | New file | ~300 |
| `ProtocolRequestHandler.h/cpp` | New file | ~935 |
| `TerminalPage.Protocol.cpp` | New file | ~816 |
| `Pane.h/cpp` (ProtocolId, SessionVars) | Modified | ~80 |
| `WindowEmperor.h/cpp` (init protocol server) | Modified | ~50 |
| `GlobalAppSettings.idl` / `MTSMSettings.h` (AI settings) | Modified | ~30 |
| `TabManagement.cpp` (openInBackground) | Modified | ~10 |

### New for VtTransport (Phase 3)

| Component | Change | Lines (est) |
|-----------|--------|-------------|
| `adaptDispatch.cpp` | `WtaReq` branch in `DoWTAction()` | ~30 |
| `ITerminalApi.hpp` | Add `HandleWtaRequest()` virtual | ~5 |
| `Terminal.hpp/cpp` | Callback + setter for WTA requests | ~30 |
| `ControlCore.cpp/idl` | Wire callback, raise WinRT event | ~40 |
| `TerminalPage.cpp` | Subscribe to event, route to handler | ~30 |
| `ProtocolRequestHandler` | Add `ProtocolSourceContext` param | ~30 |

### New for Lib Mode (Phase 4)

| Component | Change | Lines (est) |
|-----------|--------|-------------|
| `AcpConnection.h/cpp` | Rewrite as thin wrapper | ~200 (down from ~1250) |

### From Current WTA Branch (keep)

| Component | Lines |
|-----------|-------|
| Command palette `?`/`&` agent prefix | ~100 |
| `_DetectAgentCli()` + `AgentCliPath` setting | ~40 |
| `_OpenOrReuseAgentPane` + agent pane tracking | ~80 |
| `openAgentPane` action + `AgentPanePosition` | ~30 |

### From Coordinator Branch (keep)

| Component | Lines |
|-----------|-------|
| Coordinator sidecar XAML panel | ~100 |
| `ToggleCoordinator` action | ~10 |

---

## Implementation Phases

### Phase 1: Channel Trait + PipeChannel

**Goal:** WTA exe can control WT via named pipe (MCP mode works end-to-end).

**WTA changes:**
- Refactor `channel/types.rs`: align `WtAction` with protocol wire format
- Implement `channel/pipe_channel.rs`: connect to named pipe, authenticate,
  send/receive JSON-RPC
- Update `protocol/mcp/server.rs`: MCP tools → `channel.request(WtAction)`
- Simplify `ShellManager` to single-agent model

**WT changes:** None — protocol branch's `TerminalProtocolServer` works as-is.

**Test:** `wta --mcp` + external agent → MCP tool calls → real WT panes created.

### Phase 2: ACP terminal/create via Channel

**Goal:** ACP agent requests create real WT panes instead of hidden subprocesses.

**WTA changes:**
- Create `core/acp_handler.rs`: map ACP `terminal/*` requests to `WtAction`
- Refactor `protocol/acp/client.rs` to use `acp_handler` + channel
- Agent CLI management stays in simplified `ShellManager`

**WT changes:** None.

**Test:** `wta --acp --agent "copilot --acp --stdio"` → agent asks to run
`cargo build` → new WT pane appears with the build running.

### Phase 3: VT Transport (Both Sides)

**Goal:** WTA in a pane communicates with WT via OSC 9001. Pane identity is
implicit.

**WTA changes:**
- Verify/update `channel/vt_channel.rs` for the aligned wire format
- Add `self_pane_id()` support (learned from initial handshake response)
- Wire VtChannel into ACP mode when `--vt` flag is set (or auto-detect)

**WT changes:**
- `adaptDispatch.cpp`: intercept OSC 9001 `WtaReq` → route to handler
- `Terminal.hpp/cpp`: callback for WTA requests
- `ControlCore.cpp`: wire callback, raise event
- `TerminalPage.cpp`: subscribe, route to `ProtocolRequestHandler` with
  `ProtocolSourceContext` (pane ID filled from the source ControlCore)
- Response path: write OSC 9001 `WtaRes` to pane's ConptyConnection stdin
- `ProtocolRequestHandler`: accept optional `ProtocolSourceContext`, use
  `sourcePaneProtocolId` as default `pane_id` when request omits it

**Test:** WTA in pane → `list_tabs` → gets response with tab info.
`read_pane_output` without `pane_id` → reads WTA's own pane output.

### Phase 4: Lib Crate + Thin AcpConnection

**Goal:** WTA can be linked as a library. AcpConnection is ~200 lines.

**WTA changes:**
- Add `[lib]` target to `Cargo.toml`
- Implement `lib.rs` with C-ABI: `wta_init`, `wta_write_input`, `wta_shutdown`
- Internal channel: VtChannel over the in-process pipe (same code as Phase 3)

**WT changes:**
- Rewrite `AcpConnection` as thin wrapper: pipe setup + OSC scan + forwarding
- Link WTA library (static or dynamic)
- `_CreateAcpAgentPane` → uses new thin AcpConnection

**Test:** Agent pane in WT → AcpConnection → WTA lib → agent CLI → terminal ops.

### Phase 5: UI Integration

**Goal:** Coordinator panel + command palette agent integration.

- Coordinator sidecar XAML panel (from coordinator branch)
- Command palette `?`/`&` prefix (from current WTA branch)
- `_DetectAgentCli()` with `AgentCliPath` fallback
- Agent pane reuse logic

---

## VT Channel vs Pipe Channel — Decision Matrix

| Scenario | VtChannel | PipeChannel |
|----------|-----------|-------------|
| WTA exe in ACP mode (in pane) | Preferred — implicit pane identity | Works, but must discover own pane_id |
| WTA exe in MCP mode (headless) | Not possible — no pane | Required |
| WTA lib (embedded via AcpConnection) | Natural — AcpConnection intercepts OSC | Possible but unnecessary indirection |
| Cross-window operations | Works (WT handler can access all windows) | Works |
| Knowing "which pane am I" | Free (VT parser context) | Must query via GetActivePane or env var |
| Requires WT VT parser changes | Yes (Phase 3) | No |

**Recommendation:** Implement PipeChannel first (Phase 1, no WT changes needed),
then add VtChannel (Phase 3) for ACP-in-pane and lib modes where implicit pane
identity is valuable.

---

## Wire Format

### VT Channel (OSC 9001)

```
WTA → WT (stdout):
  \x1b]9001;WtaReq;{"id":"1","method":"list_tabs","params":{}}\x07

WT → WTA (stdin):
  \x1b]9001;WtaRes;{"id":"1","result":{"tabs":[...]},"error":null}\x07
```

### Pipe Channel (JSON-RPC lines)

```
WTA → WT (named pipe write):
  {"type":"request","id":"1","method":"list_tabs","params":{}}\n

WT → WTA (named pipe read):
  {"type":"response","id":"1","result":{"tabs":[...]},"error":null}\n
```

Both use the same JSON schema for request/response payloads. The only difference
is the transport framing.

---

## Key Design Principles

1. **Single source of truth for protocol actions:** `WtAction` enum in Rust =
   `ProtocolRequestHandler` method list in C++. Both derived from the same
   set of 16 operations.

2. **Channel abstraction hides transport:** The vast majority of WTA code is
   channel-agnostic. Only `main.rs` / `lib.rs` selects which channel to use.

3. **ShellManager is only for agent CLI:** It spawns one agent subprocess and
   pipes ACP JSON-RPC. It never creates WT panes. ACP `terminal/create`
   always goes through WtChannel → WT creates real panes.

4. **WT changes are minimal:** Protocol handler + transports are additive.
   The VtTransport is the only part that touches the existing VT parser path,
   and it's a small addition (~135 lines).

5. **Same Rust code in all modes:** The lib API exposes stdin/stdout pipes.
   Whether those pipes go to a real terminal (exe mode) or an in-process
   buffer (lib mode), the WTA core code is identical.
