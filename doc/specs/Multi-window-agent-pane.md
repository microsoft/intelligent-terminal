# Multi-Window Behavior of the Agent Pane

Author: kaitao@microsoft.com
Date: 2026-05-26 (revised — pivot to helper + master architecture)
Branch: `dev/vanzue/window-management`
Status: Spec under active revision. The original singleton + anonymous-pipe
design (M3-M6 on this branch) is being **superseded** by the helper + master
architecture described below. See "Design history" for the rationale.

## TL;DR

- **Each agent pane runs as its own `wta-helper` process** spawned by
  Windows Terminal as a normal conpty child (same Win32 pattern as today's
  legacy mode and any other conpty-backed pane).
- **One `wta-master` process per Terminal process** owns the single agent
  CLI subprocess (claude / copilot / gemini).
- Helpers connect to the master via a **named pipe** and speak **ACP
  JSON-RPC** — the master multiplexes per-tab `sessionId`s onto its single
  ACP connection to the agent CLI.
- TermControl ↔ helper communication is plain conpty (no custom protocol);
  helper ↔ master is plain ACP JSON-RPC over a pipe; master ↔ agent CLI is
  plain ACP JSON-RPC over stdio. **No bespoke wire format is invented.**

```
WT (one process, N windows)
 └─ SharedWta singleton spawns ─► wta-master ◄──── ACP JSON-RPC ───► agent CLI
                                       ▲                              (one child)
                                       │ ACP JSON-RPC over named pipe
                                       │
 (per agent pane:)                     │
 TerminalControl ── ConptyConnection ──┴──► wta-helper  (conpty child)
                                            crossterm + ratatui
                                            owns this tab's TabSession
```

Per agent pane: 1 conpty + 1 helper process. Per Terminal: 1 master + 1
agent CLI. **N panes ⇒ N helpers + 1 master + 1 agent CLI.**

## Design history

This document was first written as a "singleton wta" design (one wta process
serves all panes via per-pane anonymous-pipe pairs marshaled by
`DuplicateHandle`). M3–M6 on this branch landed the bulk of that
implementation: `AgentPipeConnection`, `_internal.attach_pane / detach_pane
/ resize_pane` event family, `SharedWta` singleton with refcount + crash
detection, per-pane writer-task (Plan B) for head-of-line isolation,
per-pane input task feeding the singleton's main loop, etc.

While integration-testing M6 we hit the predictable consequence of using
**anonymous pipes instead of pseudoconsoles**: the wta side receives raw VT
byte streams with no console-emulation layer, so crossterm's
`ReadConsoleInputW`-based parser doesn't apply and we'd have to hand-roll a
VT-to-KeyEvent parser. The first attempt (ASCII + Enter + Backspace only)
shipped in M6 but was nowhere near crossterm parity — no arrow keys,
no Ctrl combos, no Tab, no IME, no bracketed paste.

Three workarounds were evaluated:

| Option | Verdict |
|---|---|
| **A.** Have the singleton wta call `ReadConsoleInputW` on a `DuplicateHandle`'d conpty slave HANDLE from a peer process | **Infeasible.** `CreatePseudoConsole` does not expose slave HANDLEs to anyone outside the conpty child it spawns via `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`. Confirmed against MSDN, Windows source (`src/winconpty/winconpty.cpp`), Microsoft's `EchoCon` sample, and Alacritty/Wezterm/conpty-rs prior art — no project has demonstrated cross-process conpty input. |
| **B.** Singleton + anonymous pipes + hand-rolled VT parser (`vte`-crate level) | Feasible but accumulates technical debt: bespoke event protocol, bespoke parser, bespoke per-pane writer task, custom resize message, no IME / bracketed paste / mouse without continuous extension. ~3–5 days to ship a working parser; future features each cost extension work. |
| **Z.** **Per-pane wta-helper + one wta-master** *(chosen)* | Each helper is a conpty child of TermControl — crossterm + ratatui work natively, no custom parser. Master owns the shared agent CLI; helpers speak ACP JSON-RPC to master over a named pipe. Reuses 90% of existing wta code; throws away ~1,500 LOC of M3-M6 bespoke layers. ~11–16 days. |

Z was chosen because:

1. **Win32 abstraction reuse.** Helper-as-conpty-child is the same shape WT
   already uses for every other pane. Helpers inherit decades of console
   work — crossterm key parsing, IME, bracketed paste, mouse SGR — for
   free.
2. **No new protocol to design.** The helper↔master wire format is ACP
   JSON-RPC, which both sides already speak. Master is a JSON-RPC
   multiplexer keyed by ACP `sessionId`; no schema invented.
3. **Per-pane process boundary.** A helper crash localizes to one pane;
   debugging artifacts (logs, dumps) are naturally per-pane.
4. **Architecturally evolvable.** Per-pane different agent (future), per-
   pane sandboxing, per-pane resource limits all become natural extensions
   — each is "give the helper different cmdline args" or "master spawns a
   second agent CLI."
5. **Tab drag is still zero-cost.** The conpty handle pair stays with the
   helper; only the TermControl reparents. Same property as the original
   spec promised.

The costs (process count, helper memory, IPC hop) are real but bounded:
typical user load (≤5 panes) means ≤6 processes total and ≤75 MB. Per-pane
ACP message rate is low enough that the IPC hop is invisible.

The M3-M6 implementation is **not wasted work**:
- `SharedWta` singleton + Job Object containment + crash detection +
  refcount transfers directly (now spawning the master instead of the
  headless singleton).
- Multi-window event registration in `TerminalProtocolComServer`
  (Sprint 5 #2) is still required for non-agent events.
- GPO `AllowedAgents` policy check, settings propagation via cmdline
  (Sprint 4) is still required for master spawn.
- Closed-handler refcount discipline (Sprint 2) transfers to
  per-helper lifecycle.

What gets deleted: `AgentPipeConnection` (C++ class + IDL),
`_internal.attach_pane` / `detach_pane` / `resize_pane` (the event
family), per-pane `BufferedWriter` + writer task (Plan B), per-pane input
task in singleton, `pane_registry`, `pane_writer_txs`, `test_writers`,
`handle_pane_input` (the basic ASCII parser), `conpty_handle.rs`
HANDLE-wrappers.

## Problem statement

Windows Terminal supports multi-window operation within a single
process and allows users to drag tabs between windows (or tear a tab
out to a new window). The Agent Pane and its supporting infrastructure
(ACP / wta / Terminal Protocol COM server) were originally built
assuming a single window. Pre-pivot state has several persistent
issues:

1. **Tab drag loses agent state**: dragging a tab from one window to
   another tears down the source window's agent pane, kills wta, and
   leaves the dragged tab on the target side with no agent context.
2. **Architectural asymmetry**: `TerminalProtocolComServer` is
   per-process (one instance shared across all windows), yet wta is
   per-window. When two windows each open an agent pane, two
   independent wta processes exist; they receive each other's
   ComServer events and must filter by `tab_sessions` membership.
   PR #50 added explicit window filtering on `set_agent_state` events
   specifically because multiple wta instances were interfering.
3. **Resource overhead**: each legacy wta is a Rust process (Tokio
   runtime, tracing infrastructure, ACP client, Ratatui render loop)
   that also spawns its own agent CLI child. Linear scaling per pane
   with a non-trivial constant per process. 3 panes ≈ 6 processes ≈
   100 MB+.
4. **Operational fragility**: cross-window event paths are exercised
   only in multi-window scenarios, which are rare in testing, so
   bugs accumulate there.

The target architecture (Z): **one master per Terminal process owns
the agent CLI**; **N lightweight helpers** each render one pane and
talk to the master via ACP JSON-RPC. Tab drag is zero-state-loss by
construction — the helper's conpty handle stays the same; only the
TermControl is reparented.

## Verified architectural facts (current state)

These facts describe what's true today on this branch (post-M6). The
target architecture preserves all of them except the items explicitly
listed under "What changes."

### Process and component topology

- One Terminal process can host N windows (`WindowEmperor →
  AppHost[] → TerminalWindow → TerminalPage`).
- `TerminalProtocolComServer` is a **per-process singleton**
  registered under a single CLSID. All windows in the process share
  it.
  - `s_emperor`, `g_comRegistration` are `static`
    (`TerminalProtocolComServer.cpp`)
  - After Sprint 5 #2 (this branch), `_ensurePageEventsRegistered`
    registers `ProtocolVtSequenceReceived` on **every** window's
    TerminalPage, not just the first.
- Dragging a tab between windows never crosses processes — it routes
  through the monarch (`WindowEmperor::CreateNewWindow`,
  `WindowEmperor.cpp:261`).
- `ConptyConnection` (`src/cascadia/TerminalConnection/ConptyConnection.cpp`)
  is the canonical conpty-child spawn machinery. Z reuses it for
  helper spawn.

### SharedWta singleton (current — kept under Z)

- `src/cascadia/TerminalApp/SharedWta.{cpp,h}` (added on this branch)
  is a process-singleton owning **one** child process.
  - Pre-pivot: that child is `wta.exe --headless` (the original M3
    singleton).
  - Post-pivot: that child will be `wta.exe --master <pipe>`.
- Refcount via `AcquirePane` / `ReleasePane`. Spawn lazily on first
  acquire; tear down on last release.
- Job Object with `KILL_ON_JOB_CLOSE` binds the child's lifetime to
  Terminal.
- `RegisterWaitForSingleObject` for crash detection; child exit
  clears state so next acquire respawns.
- `CREATE_SUSPENDED` + post-job-assignment `ResumeThread` to close the
  race window where the child could run before being contained.

These mechanisms transfer to Z unchanged.

### Conpty + crossterm primer (carried forward)

A Windows process has a single stdin/stdout pair (HANDLE 0 and
HANDLE 1). Windows Terminal makes a child process appear to run
"inside a terminal" by creating a **conpty** (pseudo console):

```
                  conpty kernel object
                ┌──────────────────────┐
                │                      │
   ┌── master ─►│                      │◄── slave ──┐
   │            │                      │            │
   │           └──────────────────────┘             │
   ▼                                                ▼
 Windows                                        wta-helper
 Terminal                                       (stdin/stdout
 (master)                                        wired to slave
                                                 by the kernel
                                                 via PROC_THREAD_
                                                 ATTRIBUTE_PSEUDO
                                                 CONSOLE)
```

The slave-side stdin HANDLE delivered to the conpty child is a
**console input HANDLE** — `ReadConsoleInputW` on it returns
structured `INPUT_RECORD`s, not raw VT bytes. crossterm reads
console input via `ReadConsoleInputW`, so a wta-helper that is a
conpty child gets full keystroke parsing (arrow keys, Ctrl combos,
Tab, mouse SGR, bracketed paste, IME) for free.

A **peer process** that holds, via `DuplicateHandle`, a different
HANDLE that points at the same conpty does **not** get this — it can
only `ReadFile` raw VT bytes. This is what M3-M6 ran into and what
makes Z's "helper as conpty child" structurally important.

## Target architecture

### 1. Process topology

```
WindowsTerminal.exe (one process)
 ├─ Window A, Window B, Window C  (each a TerminalPage / AppHost)
 │   └─ Per agent pane: TerminalControl with a ConptyConnection
 │                                          │
 │                                          │ child
 │                                          ▼
 │                                       wta-helper
 │                                       (per pane)
 │
 └─ SharedWta singleton ─── spawns ──► wta-master
                                          │
                                          │ named pipe (ACP JSON-RPC)
                                          ▲
                                          │ (per helper, lazily on first prompt)
                                          │
                                       wta-helper(s)
                                          │
                                          │ stdio (ACP JSON-RPC)
                                          ▼
                                       agent CLI subprocess
                                       (one — claude/copilot/gemini)
```

- **`wta-master`**: singleton per Terminal process, spawned by
  `SharedWta` on first agent-pane request. Headless (no UI). Owns the
  single ACP connection to the agent CLI subprocess. Listens on a
  named pipe for helper connections.
- **`wta-helper`**: one per agent pane. Conpty child of its
  TerminalControl. Runs the full Ratatui chat UI for one tab.
  Connects to master via named pipe; speaks ACP JSON-RPC as a
  client.
- **`agent CLI`**: existing claude/copilot/gemini ACP-protocol
  subprocess. One per master (= one per Terminal process). Owns N
  ACP sessions internally, one per attached helper.

### 2. Per-pane attachment model

Each agent pane has its own `wta-helper` process. The helper owns
exactly one tab's worth of state:

- One `TabSession` (chat history, turn state, autofix state, input
  editor) — same struct as legacy `wta`, but the helper holds it for
  exactly one tab.
- One `RenderCtx`-equivalent — a Ratatui `Terminal<CrosstermBackend<Stdout>>`
  writing to the conpty slave-out via the helper's normal stdout.
- One ACP `SessionId` (lazily allocated on first prompt; bound to this
  helper for its lifetime).
- One JSON-RPC client connection to the master over a named pipe.

There is **no shared state** across helpers. State lives in the
helper that owns it.

### 3. wta-master responsibilities

The master is a **thin ACP-protocol multiplexer**. It owns the single
agent CLI subprocess and serves N helper connections.

State held by master:
- The agent CLI subprocess (stdio, JSON-RPC framing) — existing
  `protocol/acp/client.rs` machinery.
- A table mapping `ACP SessionId → helper-connection`.
- Process-wide config inherited from spawn cmdline: `--agent`,
  `--agent-id`, `--acp-model`, `--no-autofix`, `--language`,
  `--delegate-agent`, `--delegate-model` (Sprint 4 wiring carries
  over).
- Existing WT-COM event subscription (autofix, agent_state_changed,
  etc.) — master fans out the relevant events to the helper that
  owns the affected tab.

State **not** held by master:
- No chat history (lives in helpers).
- No render state (lives in helpers).
- No per-tab autofix state machine (helper-local; master only forwards
  autofix events from WT).

### 4. wta-helper responsibilities

A helper is a slimmed legacy `wta` with the ACP-subprocess-spawn
replaced by an ACP-client-over-named-pipe.

- Conpty child of TermControl; stdin/stdout/stderr wired to conpty
  slave HANDLEs by the kernel via `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`.
- Runs the existing crossterm event loop on its stdin → full
  `KeyEvent` parsing for free.
- Runs the existing Ratatui chat UI on its stdout.
- Owns the existing `App` / `TabSession` data model from legacy wta —
  but only one tab's worth.
- Connects to master via the named pipe whose name is passed in
  `--connect-master <pipe>` (or env var).
- Speaks ACP JSON-RPC to master as if master were the agent CLI:
  `initialize`, `session/new`, `session/prompt`, `session/cancel`,
  `session/load`, `set_session_model`.

### 5. Wire protocols

| Boundary | Protocol | Why |
|---|---|---|
| TerminalControl ↔ wta-helper | Standard conpty (existing `ConptyConnection`) | Same as every other pane; full console-input fidelity |
| wta-helper ↔ wta-master | ACP JSON-RPC over named pipe (`\\.\pipe\wta-master-<guid>`) | Reuses the protocol both sides already speak |
| wta-master ↔ agent CLI | ACP JSON-RPC over stdio (existing) | Unchanged from today |
| Terminal ↔ wta-master | Existing COM `IProtocolEventCallback` / `SendEvent` for autofix, agent_state_changed, etc. | Existing channel, no change |

**No new IDL.** **No `_internal.*` event family.** **No custom binary
protocol.** Everything off-the-shelf.

### 6. Lifecycle

#### Master spawn (lazy, once per Terminal process)

```
First agent-pane request anywhere in Terminal:
 ├─ SharedWta::AcquirePane(wtaPath, extraArgs, masterPipeName)
 │   └─ if refcount == 0:
 │       └─ spawn wta-master --master <masterPipeName> <other args>
 │           CREATE_SUSPENDED → assign to Job Object → ResumeThread
 │       └─ master initializes ACP connection to agent CLI subprocess
 │       └─ master starts named-pipe server, accept loop
 │   └─ refcount += 1
 └─ proceed to helper spawn
```

#### Helper spawn (per pane)

```
Per agent pane:
 ├─ ConptyConnection with cmdline:
 │   wta.exe --connect-master <masterPipeName>
 │           --agent-id copilot
 │           --owner-tab-id "{guid}"
 │           --initial-view chat
 │           [--no-autofix] [--language ja] ...
 │
 ├─ TermControl spawns helper as conpty child
 │   └─ helper inherits conpty slave-in/out HANDLEs as stdin/stdout
 │
 ├─ helper boots its existing TUI:
 │   - crossterm event loop on stdin
 │   - Ratatui Terminal on stdout
 │   - App / TabSession initialized for owner-tab-id
 │
 ├─ helper opens named pipe to master
 │   └─ sends ACP `initialize` (handshake — master responds with cached
 │       initialize state)
 │   └─ sends ACP `session/new` lazily when the first prompt is submitted
 │       (or eagerly if --eager-session is set)
 │
 └─ helper renders welcome screen; awaits user input
```

#### Per-tab session creation (first prompt)

```
User types a prompt, hits Enter:
 ├─ helper builds ACP `session/prompt` request
 ├─ helper: if no SessionId yet, sends `session/new` first
 │   └─ master forwards to agent CLI
 │   └─ agent CLI returns new SessionId
 │   └─ master records SessionId → helper-connection
 │   └─ master responds to helper with SessionId
 ├─ helper sends `session/prompt(SessionId, text)`
 ├─ master forwards to agent CLI on the shared connection
 ├─ agent CLI emits `session/update` chunks with SessionId
 ├─ master routes each chunk to the owning helper via SessionId lookup
 └─ helper updates TabSession.messages, re-renders
```

#### Pane close (detach)

```
User closes the pane (Ctrl+W, tab close, window close):
 ├─ TermControl tears down ConptyConnection
 │   └─ conpty kernel object closes its master side
 │   └─ helper sees EOF on stdin → crossterm event loop exits
 │
 ├─ helper sends ACP `session/cancel` for any in-flight prompt
 ├─ helper sends `session/end` (or just drops the connection)
 ├─ helper exits
 │
 ├─ named pipe disconnect on master side
 │   └─ master cleans up SessionId → helper mapping
 │   └─ optionally informs agent CLI to release the session
 │
 └─ SharedWta::ReleasePane (in WT-side Closed handler)
     └─ refcount -= 1
     └─ if refcount == 0:
         └─ KILL_ON_JOB_CLOSE on the Job Object closes master cleanly
         └─ master's shutdown drops the agent CLI subprocess
```

#### Tab drag between windows

The drag triggers existing WT mechanics: `ContentId` lookup,
`AttachContent → _MakePane`, reparent of the existing TermControl into
the target window's pane tree. The conpty kernel object is owned by
the master side (WT), so the master-side HANDLE pair stays the same.
The helper's stdin/stdout HANDLEs (slave side) are unchanged in the
helper process — the helper has no awareness of the drag.

**Helper does nothing.** **Master does nothing.** The chat
continues uninterrupted because no part of the agent stack moved.

#### Master crash

`SharedWta::_OnProcessExited` (existing wait-callback) fires. State is
cleared so the next `AcquirePane` respawns the master. All existing
helpers' pipe connections drop:
- Helper detects pipe disconnect → surfaces a "disconnected from
  master" banner in its TUI.
- User can keep typing locally; submission fails until master
  reconnects.
- On next `AcquirePane`, a fresh master spawns. Helpers reconnect (via
  retry loop in helper).

(Auto-reconnect protocol is part of Phase 5 testing — see
implementation plan.)

#### Helper crash

ConptyConnection on the WT side sees EOF on the master pipe. The
pane's TermControl shows the standard "process exited" UI. Master
notices the named pipe disconnect; cleans up the SessionId mapping.
User must re-open the pane to recover.

(Lifetimes are intentionally per-pane independent — one helper
crashing does not affect any other pane.)

### 7. Per-tab independent agents (future)

`AttachPaneParams.agent_id` is currently metadata-only because the
master holds a single agent CLI subprocess shared across helpers.
Future: master could maintain a pool of agent CLI subprocesses keyed
by `agent_id`. Each helper requests sessions from the agent CLI
matching its `agent_id`. Wire format is already extensible (see Z-R4
risk below). Not in v1.

## What needs to change

### Code that **gets deleted** (current M3-M6 work that doesn't fit Z)

- `src/cascadia/TerminalConnection/AgentPipeConnection.{cpp,h,idl}`
  (~340 LOC) — replaced by ConptyConnection reuse.
- `tools/wta/src/protocol/internal_control.rs` (~378 LOC) — the
  `_internal.attach_pane / detach_pane / resize_pane` event family
  becomes unnecessary because helper lifecycle = process lifecycle.
- `tools/wta/src/pane_registry.rs` (~256 LOC) — each helper holds
  one tab; no registry needed.
- `tools/wta/src/render_ctx.rs`: `BufferedWriter`,
  `spawn_pane_writer_task`, `drain_bytes` (~80 LOC). Plan B
  decoupling vanishes because the helper's ratatui writes directly
  to stdout (conpty slave-out) on its own process; no shared event
  loop to block.
- `tools/wta/src/conpty_handle.rs` (~150 LOC) — only used by the
  singleton's pane registry.
- `App.pane_registry`, `App.pane_writer_txs`, `App.test_writers`
  fields and their handle_internal_control / handle_pane_input
  arms in `tools/wta/src/app.rs` (~400 LOC of changes to revert).
- `AppEvent::InternalControl`, `AppEvent::PaneInput` variants.
- `_AutoCreateHiddenAgentPaneShared`'s DuplicateHandle + cleanup
  dance in `src/cascadia/TerminalApp/TerminalPage.cpp` (the
  helper-spawn path becomes plain ConptyConnection wiring, ~50 LOC
  net deletion).
- `--headless` mode in `tools/wta/src/main.rs` →
  replaced by `--master` mode.

### Code that **stays** (M3-M6 work that transfers)

- `src/cascadia/TerminalApp/SharedWta.{cpp,h}` — now spawns
  master instead of headless wta. Refcount, Job Object,
  CREATE_SUSPENDED, RegisterWaitForSingleObject all unchanged.
- Sprint 2 + Sprint 9 work: GPO policy check, cwd resolution,
  per-process settings propagation via cmdline (`--agent`,
  `--agent-id`, `--no-autofix`, `--language`, model overrides) —
  passed to master, master inherits.
- Sprint 5 #2: `_ensurePageEventsRegistered` per-window
  registration in `TerminalProtocolComServer.cpp`. Still needed for
  autofix and other non-agent events.
- Sprint 5 #3: non-headless wta's `_internal.*` event drop. Becomes
  moot when `_internal.*` events go away entirely.
- All wta TUI code: `tools/wta/src/ui/*`, `event.rs`, `app.rs`'s
  rendering and chat-state machinery — survives in the helper as-is.

### Code that **gets added**

- New file `tools/wta/src/master/mod.rs` (~400-700 LOC):
  - `MasterServer`: named pipe listener
  - `MuxConnection`: per-helper protocol handler
  - SessionId-keyed routing tables
  - Forwarder loop: agent CLI → helpers
- New CLI args in `tools/wta/src/main.rs`:
  - `--master <pipe-name>` — start in master mode
  - `--connect-master <pipe-name>` — start in helper mode and
    connect to master
- In `protocol/acp/client.rs`: a new `Transport` enum so the ACP
  client can use named pipes (for helpers) or process stdio (for
  the existing legacy path, kept as `--legacy-tui`).
- Pipe-name generation utility in `SharedWta.cpp`: produce a
  process-unique name like `\\.\pipe\wta-master-<GUID>`.

### File-level inventory of changes

| File | Action |
|---|---|
| `doc/specs/Multi-window-agent-pane.md` | Rewrite to reflect Z (this commit). |
| `src/cascadia/TerminalApp/SharedWta.cpp` | Change spawn cmdline from `--headless` to `--master <pipe>`; generate pipe name. |
| `src/cascadia/TerminalApp/SharedWta.h` | Add `MasterPipeName()` getter. |
| `src/cascadia/TerminalApp/TerminalPage.cpp` | `_AutoCreateHiddenAgentPaneShared` and `_OpenOrReuseAgentPane` switch from `AgentPipeConnection` to a `ConptyConnection`-based helper spawn (cmdline includes `--connect-master`). Delete DuplicateHandle / ReleaseWtaHandles / DimensionsChanged wiring. |
| `src/cascadia/TerminalConnection/AgentPipeConnection.{cpp,h,idl}` | **Delete.** |
| `src/cascadia/TerminalConnection/TerminalConnection.vcxproj{,.filters}` | Remove AgentPipeConnection entries. |
| `tools/wta/src/main.rs` | Add `--master` and `--connect-master` modes. `--headless` becomes an alias for `--master` during transition, then deleted. |
| `tools/wta/src/master/mod.rs` | **New file.** Named-pipe listener + ACP muxer. |
| `tools/wta/src/protocol/acp/client.rs` | Generalize transport — accept named pipe in addition to child stdio. |
| `tools/wta/src/protocol/acp/spawn.rs` | Unchanged (agent CLI spawn still uses stdio). |
| `tools/wta/src/app.rs` | Delete `handle_internal_control`, `handle_pane_input`, `pane_registry`, `pane_writer_txs`, `test_writers`. Rest unchanged — App in a helper is the same App that runs today in non-headless mode, single-tab. |
| `tools/wta/src/pane_registry.rs` | **Delete.** |
| `tools/wta/src/render_ctx.rs` | **Delete.** (Helper uses crossterm-on-stdout direct.) |
| `tools/wta/src/conpty_handle.rs` | **Delete.** |
| `tools/wta/src/protocol/internal_control.rs` | **Delete.** |
| `tools/wta/src/protocol/mod.rs` | Remove `internal_control` mod. |
| `tools/wta/Cargo.toml` | Remove `unstable-backend-writer` feature on ratatui (added in M3 for `BufferedWriter`). |

## Implementation plan

### Phase 0 (this commit): Doc + task setup

- Land this revised spec.
- Update task tracking with Z-M1 through Z-M5.

### Phase 1: Master mode (3-5 days) — Z-M1

- Add `--master <pipe-name>` CLI arg.
- New module `tools/wta/src/master/mod.rs`:
  - Named pipe server (Windows `CreateNamedPipe`,
    `tokio_util::compat` or direct `tokio::net::windows::named_pipe`)
  - Accept loop spawns one `MuxConnection` task per helper
  - Each `MuxConnection`:
    - JSON-RPC framing over the pipe
    - Forwards helper's outgoing ACP requests to the shared agent CLI
      connection (existing `run_acp_client` route)
    - Routes agent CLI's incoming notifications back to the helper
      whose `SessionId` matches
  - Special-case `initialize`: master responds from cached agent
    capabilities (agent CLI only `initialize`s once at master
    startup).
  - Special-case `session/new`: master forwards, records the
    returned `SessionId → helper-connection` mapping before
    responding to the helper.
- Master also subscribes to existing WT COM events (`autofix_state`,
  `agent_state_changed`, etc.) and forwards them to the relevant
  helper (looking up `tab_id` → helper from the SessionId map plus
  the helper's announced `owner-tab-id`).
- Unit tests: simulate 2 helpers, verify per-session routing,
  verify cross-helper isolation.

### Phase 2: Helper mode (2-3 days) — Z-M2

- Add `--connect-master <pipe-name>` CLI arg.
- Modify ACP client to use a pipe-based transport when in helper
  mode (`protocol/acp/client.rs::run_acp_client` becomes
  transport-agnostic via a `Transport` trait).
- The helper still uses `agent_cmd`, `agent_id` etc. from cmdline,
  but instead of `spawn_agent_process` it connects to the master
  pipe.
- TUI / App / TabSession unchanged — helper is the existing
  non-headless wta with one tab.
- Tests: spin up master + helper in a single-process tokio test,
  exchange `initialize` + `session/new` + `session/prompt`, verify
  end-to-end JSON-RPC flow.

### Phase 3: C++ migration (2 days) — Z-M3

- `SharedWta::AcquirePane`:
  - Generate unique pipe name on first acquire (e.g.
    `\\.\pipe\wta-master-` + GUID).
  - Spawn `wta.exe --master <pipe> <existing extraArgs>`.
  - Expose pipe name via getter.
- `TerminalPage::_AutoCreateHiddenAgentPaneShared`:
  - Replace `TerminalConnection::AgentPipeConnection` construction
    with `NewTerminalArgs` carrying a cmdline:
    ```
    wta.exe --connect-master <pipe> --owner-tab-id <stable-id>
            --agent-id <effective-agent>
            --initial-view chat
            [--no-autofix] [--language <lang>] [...]
    ```
  - Wire via existing `ConptyConnection` (the same machinery legacy
    mode uses).
  - Delete: DuplicateHandle blocks, ReleaseWtaHandles call,
    DimensionsChanged subscription, `closeHandleInWta` helper.
- `_OpenOrReuseAgentPane`'s shared branch: same change. Both
  callers now produce identical helper-spawn cmdlines.

### Phase 4: Cleanup (1-2 days) — Z-M4 — DONE

- Delete files listed under "Code that gets deleted" above.
- Remove `unstable-backend-writer` feature from `Cargo.toml`.
- Remove all references in `TerminalConnection.vcxproj{,.filters}`.

### Phase 5: Default on, deprecate per-window mode — Z-M6 — DONE

Legacy per-pane-wta path and the `aiIntegration.sharedWtaProcess`
setting have both been removed. Helper+master is now the only
architecture:

- Removed setting `aiIntegration.sharedWtaProcess` from
  `MTSMSettings.h` + `GlobalAppSettings.idl`.
- Collapsed `_AutoCreateHiddenAgentPane` to a thin forwarder around
  `_AutoCreateHiddenAgentPaneShared`; deleted the legacy cmdline
  construction (~300 LOC) and the legacy Closed handler that called
  `_TearDownAgentPaneWtaWatch`.
- Collapsed `_OpenOrReuseAgentPane` to use the helper+master path
  unconditionally; deleted the legacy cmdline construction +
  per-pane wta-watch arming (~250 LOC).
- Deleted the per-page wta watch infrastructure:
  `_SetupAgentPaneWtaWatch`, `_TearDownAgentPaneWtaWatch`,
  `_OnAgentPaneWtaExit`, `AgentPaneWtaWaitContext`,
  `_agentPaneJob`, `_agentPaneWtaHandle`, `_agentPaneWtaWait`,
  `_agentPaneWtaWaitContext`, `_agentPaneWtaGen` from
  `TerminalPage.{h,cpp}`. Helper processes are conpty children of
  TermControl, so their lifetime is managed by the standard
  ConptyConnection / pane teardown path.
- Updated `~TerminalPage` and `_TeardownAgentPane` to no longer
  reference the removed watch.

### Phase 5 (legacy task name): Integration testing (2-3 days) — Z-M5

- Multi-pane in one window: open 3 agent panes, verify each has its
  own helper, verify per-tab session isolation.
- Multi-window: open agent pane in each of 2 windows, verify each
  has its own helper, both connect to one master.
- Tab drag: drag agent-pane tab between windows, verify chat
  continues, verify helper process persists across the move.
- Window close: close window with agent pane open, verify that
  helper exits and master refcount decrements.
- Last-pane close: close last agent pane, verify master exits via
  Job Object.
- Master crash: kill master mid-conversation, verify helpers
  surface error and reconnect on next prompt.
- Helper crash: kill helper, verify only its pane is affected.
- Resize: window resize → conpty resize → helper sees it via
  crossterm `Event::Resize` (no `_internal.resize_pane` needed).
- Settings reload (autofix toggle): verify master receives the
  COM event and forwards to all helpers; helpers update their
  autofix UI state.

**Total: ~10-15 days.**

## Risks and open questions

### Z-R1. Master ACP muxer correctness

Master must handle ACP semantics carefully:
- `initialize` happens once with the agent CLI; subsequent helper
  `initialize` requests must be answered from cached capabilities.
- `session/new` from each helper produces a fresh `SessionId`;
  master must atomically record `SessionId → helper` before
  responding (otherwise an `update` could arrive before the mapping
  is in place).
- `session/cancel`, `session/load` are scoped to a `SessionId` —
  master forwards verbatim.
- Concurrent `session/prompt`s from different helpers are fine —
  the agent CLI multiplexes by `SessionId`.

Mitigation: dedicated muxer tests (Phase 1).

### Z-R2. Helper crash UX

A helper crash leaves its pane in "process exited" state; the user
must re-open. This is the **same UX as a conpty child of any other
TermControl crashing**, so it's familiar Windows Terminal behavior.

### Z-R3. Master pipe naming

Pipe name must be process-unique. We use
`\\.\pipe\wta-master-<GUID>` generated once per `SharedWta` instance.
Helpers receive the name via cmdline arg.

### Z-R4. Per-pane agent_id mismatch

If a future helper sends `agent_id=claude` but the master has
`agent_id=copilot`, two behaviors are possible:
- v1: master ignores `agent_id` (metadata only); all helpers share
  the master's agent.
- future: master spawns / maintains additional agent CLI processes
  keyed by `agent_id` and routes per-helper to the matching one.

v1 ships with the simple behavior; future extension is non-breaking.

### Z-R5. Agent CLI version skew across helpers

A helper started under master version A may continue running when
master is restarted at version B (e.g. user updates Terminal mid-
session). The new master may speak a different ACP protocol version
than the helper. Mitigation: helper performs `initialize` handshake
on reconnect; on incompatible version, surfaces a clear error.

### Z-R6. Master spawn race

When two helpers both trigger `AcquirePane` simultaneously
(unlikely but possible), `SharedWta`'s mutex serializes the spawn.
Helpers may briefly find the pipe non-existent if they spawn before
master finishes startup. Mitigation: helper retries pipe connection
for ~2-3 seconds with exponential backoff.

### Z-R7. Tab drag verification (carried from old R8)

`TermControl`/`ContentId` reattachment across windows is an existing
mechanism for non-agent panes. Verify it actually preserves conpty
master handle, helper process identity, and the helper's ACP
connection to master during the drag. **Stage 0 spike in Z-M5.**

### Z-R8. Old behaviors that need re-validation (carried from old R9)

The shift from "one shared pane per window" to "per-tab independent
panes" changes user-facing behaviors:
- **Toggle AI Assistant** opens/hides the active tab's pane —
  natural under the new model; action handler reads per-tab state.
- **Autofix routing** is already per-tab (PR #50); master forwards
  the WT-side autofix event to the helper owning the affected tab
  (via `session_to_tab` reverse lookup).
- **Bottom bar / diagnostics** is per-tab today (PR #49); display
  logic reads from the active tab's helper-reported state.
- **Pre-warming**: M3's auto-create-on-first-tab pre-warm becomes
  optional under per-tab. Default: no pre-warming; first user
  toggle creates the helper on demand. Re-evaluate in Phase 5.

## What this does NOT solve (out of scope)

- **Cross-process Terminal instances**: if WT is configured for
  multi-instance, each instance has its own master. Bridge between
  them not addressed.
- **Persistent state across Terminal restart**: closing and
  reopening WT loses session state (unless the agent CLI itself
  persists; see current behavior of claude/copilot/gemini history
  files).
- **Remote agents**: this spec assumes local ACP child processes.
- **Restructuring agent pane UI to XAML**: Z keeps the Ratatui TUI
  model. Migration to native XAML chat UI is a separate larger spec.

## Future work

- **F1**: `TabSession` checkpoint + restore across master restarts.
  Persists chat history to disk; helper reattach via ACP
  `session/load`. Not blocked by Z but only useful after it.
- **F2**: Per-pane different agent CLI. Master spawns additional
  agent CLI processes keyed by `agent_id`; routes helpers to the
  matching child. Wire format already supports it via
  `--agent-id` per helper.
- **F3**: ComServer caller-identity hooks. Defense-in-depth against
  hostile or buggy callers. Deprioritized.
- **F4**: Migration of agent pane UI from Ratatui TUI to native
  XAML chat surface. Separate, larger spec.
- **F5**: Master high-availability — if master crashes, helpers
  reconnect to a freshly-respawned master and resume via ACP
  `session/load`. Builds on F1.
