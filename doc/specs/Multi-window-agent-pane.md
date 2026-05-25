# Multi-Window Behavior of the Agent Pane

Author: kaitao@microsoft.com
Date: 2026-05-25
Branch: `dev/vanzue/window-management`
Base: `main` at `996ffa36f` (PR #50, "Make wta the sole owner of
per-tab agent view state")
Status: Design — implementation not started.

## Problem statement

Windows Terminal supports multi-window operation within a single
process and allows users to drag tabs between windows (or tear a tab
out to a new window). The Agent Pane and its supporting
infrastructure (ACP / wta / Terminal Protocol COM server) were
originally built assuming a single window. The current state has
several persistent issues:

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

3. **Resource overhead**: each wta is a Rust process (Tokio runtime,
   tracing infrastructure, ACP client, Ratatui render loop). Linear
   scaling per window with non-trivial constant per process.

4. **Operational fragility**: cross-window event paths are exercised
   only in multi-window scenarios, which are rare in testing, so
   bugs accumulate there.

This document specifies the target architecture: **one wta process
per Terminal process, shared across all windows**, with one conpty
handle pair per agent pane held by wta and N independent Ratatui
render contexts running in that single process. Under this model,
tab drag across windows is zero-state-loss by construction — the
conpty handle pair stays the same; only the TermControl on the WT
side is reparented, and wta does nothing.

## Verified architectural facts (current state)

These facts describe what's true today. The target architecture
preserves all of them except the per-window wta model.

### Process and component topology

- One Terminal process can host N windows (`WindowEmperor →
  AppHost[] → TerminalWindow → TerminalPage`).
- `TerminalProtocolComServer` is a **per-process singleton**
  registered under a single CLSID. All windows in the process share
  it.
  - `s_emperor`, `g_comRegistration` are `static`
    (`TerminalProtocolComServer.cpp`)
- Dragging a tab between windows never crosses processes — it routes
  through the monarch (`WindowEmperor::CreateNewWindow`,
  `WindowEmperor.cpp:261`).

### wta process model (current — to be replaced)

- **One wta.exe per window with an Agent Pane**, not one global wta.
- wta is spawned by `ConptyConnection::Start()` as a child process
  when the agent pane's TermControl initializes
  (`ConptyConnection.cpp:177`).
- Each wta is assigned to a `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` job
  handle on TerminalPage (`TerminalPage.cpp:1341-1354`,
  `TerminalPage.h:664`), so wta cannot outlive its window.
- wta receives `WT_COM_CLSID` via environment from ConptyConnection
  and uses it to instantiate `IProtocolServer` via
  `CoCreateInstance`.

The target architecture keeps the COM client side and
job-object-based lifecycle; it changes the "one-per-window"
invariant and the conpty-spawn coupling.

### Agent Pane storage and the state-ownership model (post PR #50)

- `TerminalPage` holds a single `std::weak_ptr<Pane> _agentPane`
  (`TerminalPage.h:315`) — per-window, not per-tab.
- The physical Pane lives inside one tab's pane tree at a time.
  `_RelocateAgentPaneToTab` moves it with `DetachPane` /
  `RestorePane`.
- **Per-tab agent state (pane visibility + active view) is owned by
  wta, not by C++.** `Tab::AgentPaneOpen` still exists, but C++ only
  mirrors it from wta's projection. The single inbound writer is
  `OnAgentStateChanged` (`TerminalPage.cpp:4454`).
- Direct C++ writes to `Tab::AgentPaneOpen` survive only at two
  structural boundaries:
  - **Spawn** — wta does not exist yet, so C++ seeds the initial
    value.
  - **Pane `Closed`** — wta is dead, so C++ clears the flag on
    teardown.
- All other code paths (hotkeys, buttons, Ctrl+C×2, etc.) request
  state changes via `_RequestAgentState(view?, pane_open?)`
  (`TerminalPage.cpp:1707`). wta updates its own per-tab state, then
  emits `agent_state_changed`, which `OnAgentStateChanged` mirrors
  and triggers `_ReconcileAgentPaneForActiveTab`.
- The unified bidirectional event pair:
  ```
  C++ → wta:  set_agent_state    { view?, pane_open?, tab_id }
  wta → C++:  agent_state_changed { view, pane_open }
  ```
  `tab_id` on the outbound side defends against `tab_changed` /
  `set_agent_state` ordering ambiguity — wta routes the mutation to
  the named TabSession, not "whatever's active right now."

The target architecture **changes the agent-pane physical model**:
- "One agent pane per window, relocated across tabs on tab switch"
  → "**One agent pane per tab**, each with its own conpty handle and
  RenderCtx in wta."
- `_agentPane` weak_ptr on TerminalPage goes away.
- `_RelocateAgentPaneToTab` goes away.
- Per-tab `AgentPaneOpen` mirror remains as the visibility flag, but
  it now signals "this tab's own agent pane is visible," not
  "whether the shared pane is currently displayed here."

### COM server multi-window behavior

The IDL **is** designed for multi-window
(`TerminalProtocol.idl`):

- Pane identity is a globally-unique `Guid SessionId` (assigned by
  ConptyConnection per pane); no cross-window collisions.
- Query methods accept window filters: `ListTabs(windowIdFilter)`,
  `ListPanes(windowIdFilter, tabIdFilter)`.
- Returned structs carry `WindowId` so callers know placement.
- Mutation methods (`FocusPane`, `SendInput`, `ClosePane`, …) look
  up panes by SessionId across all windows
  (`TerminalProtocolComServer.cpp:603-619` and similar). GUID
  uniqueness makes this correct.
- `Subscribe(IProtocolEventCallback)` has **no window/tab filter**.
  All subscribers receive all events from all windows. Per-window
  filtering is delegated to subscribers.

### wta event-filter behavior (subscriber side)

- wta receives events through direct COM subscription
  (`tools/wta/src/main.rs:1265,1568`). No separate `wtcli listen`
  subprocess in TUI mode.
- Two filter mechanisms:
  1. **Own-pane skip** (`app.rs:3530`) — never act on events from
     wta's own agent pane.
  2. **`tab_sessions` membership** (`app.rs:3686`) — autofix routing
     looks up `event.tab_id` in wta's local `tab_sessions` HashMap.
     Not present → silently dropped.
- **Only `set_agent_state` events** explicitly check `window_id`
  (`app.rs:3420-3434`, post PR#50). All other event types rely on
  the `tab_sessions` filter for cross-window isolation.

Under the target architecture there is exactly one wta with exactly
one ComServer subscription, so the cross-window filtering becomes
moot — wta routes by `tab_id` alone, never by `window_id`.

## Target architecture

### The 6-point spec

#### 1. Process topology

- **One Terminal process** (default WT multi-window-in-process mode).
- **One ComServer** — already per-process singleton, unchanged.
- **One wta** — lazily spawned by WindowEmperor on the first
  agent-pane request; outlives all individual windows; dies with the
  Terminal process (Job Object).

#### 2. Per-tab attachment model

Each tab with an open agent pane holds one connection to wta. WT
allocates a conpty per agent pane; the slave HANDLE pair is passed
to wta via COM. wta binds it to a new `RenderCtx` (Ratatui Terminal
+ input reader task) and a new `TabSession` (chat state + ACP
session).

In wta's internal data model:
- `tab_sessions: HashMap<TabStableId, TabSession>` — business state
  (chat history, ACP session, autofix state).
- `render_ctxs: HashMap<TabStableId, RenderCtx>` — rendering state
  (Ratatui Terminal, conpty handle, input task).

#### 3. No `windowId` in wta's data model

This is the key simplification.

Under the current per-window-wta model, PR #50 introduced `window_id`
on `set_agent_state` events so that wta-A could filter out events
intended for wta-B. With a single shared wta, that filtering becomes
moot — there is no "wrong wta" to filter against.

Audit of every place wta could plausibly want `window_id`:

| Use case | Need? | Why not |
|---|---|---|
| Routing autofix to a tab | No | Already routes by `TabStableId` |
| `set_agent_state` filtering | No | Single wta = no cross-wta confusion |
| ComServer dispatching to wta | No | ComServer keys by `pane_session_id` (GUID), globally unique |
| wta calling `FocusPane(guid)` | No | ComServer walks all windows, GUID resolves uniquely |
| Tab drag notification | No | conpty handle survives the drag; wta needs no signal |
| Window close cleanup | No | Naturally handled by per-pane `_internal.detach_pane` events |
| Global sessions list view | No | Enumerate `tab_sessions`; window-agnostic |
| Diagnostic logging | Optional | Pass as fire-and-forget metadata on `_internal.attach_pane`; never used for routing |

Conclusion: **wta tracks only `TabStableId`**. `windowId` is a
WT-side concept that wta is intentionally ignorant of. Logging may
carry it as opaque metadata.

#### 4. Tab drag = wta does nothing

When a tab moves between windows:

- WT's `_MoveTab` calls `BuildStartupActions(Content)`. The agent
  pane TermControl's `ContentId` is in the serialized JSON — same
  mechanism used for any pane move.
- Target window's `AttachContent → _MakePane` looks up the
  `ContentId`, finds the existing TermControl, reparents it into
  the target window's XAML pane tree.
- The TermControl holds the conpty master HANDLE pair. That handle
  pair is **unchanged**.
- The conpty slave HANDLE pair on the wta side is **unchanged**.
- wta is **not notified**. It keeps writing to the same handle pair.
  The bytes it writes now appear in the new window's TermControl
  automatically because the TermControl is the master-side reader.

**Zero-cost migration**. No detach-then-reattach, no state rekeying,
no ACP session restart. The TabSession key (`TabStableId`) doesn't
change, the render context handle doesn't change, the agent CLI
child process doesn't restart.

#### 5. New window opens an agent pane → lazy attach

- New window creation: wta is not involved.
- User invokes "Toggle AI assistant" in any window for the first
  time (or the first time on a specific tab):
  1. WT creates a conpty pair.
  2. WT calls `DuplicateHandle` to copy the conpty slave HANDLEs
     into wta's process; gets back integer values valid in wta's
     address space.
  3. WT calls `wtaCallback.OnEvent(json)` with method
     `_internal.attach_pane` and the duplicated HANDLE values plus
     `tab_id`, `agent_id`, `initial_cwd`, `initial_view`.
  4. WT closes its own copies of the slave HANDLEs.
  5. wta's OnEvent dispatcher recognizes the method, wraps the
     HANDLEs into a `RenderCtx`, creates a `TabSession`, starts an
     ACP session with the configured agent backend.
  6. wta calls `proxy.SendEvent` with method
     `_internal.attach_pane_ack` carrying the correlation id.
  7. WT's TermControl reads from the master side and renders
     normally.
- If wta is not yet running (first agent-pane request in this
  Terminal process), WindowEmperor spawns it and waits for wta to
  register its `IProtocolEventCallback`. Subsequent
  `_internal.attach_pane` events reuse the existing process.

#### 6. Close path

Triggered by: agent pane closed (UI gesture), tab closed, or window
closed (which closes all its tabs).

- WT calls `wtaCallback.OnEvent(json)` with method
  `_internal.detach_pane` and the tab id.
- wta:
  - Sends ACP `session/end` to the agent CLI subprocess for this
    tab.
  - Waits for ack with a short timeout.
  - Drops the `RenderCtx` (closes the conpty handle on wta's side,
    aborts the input reader task, drops the Ratatui `Terminal`).
  - Drops the `TabSession` (chat history, autofix state).
  - Drops the ACP session record (or transitions it to `Ended`
    depending on whether the underlying agent CLI persisted
    history).
- WT closes its conpty master-side handles. Both ends are now
  closed, the conpty kernel object is reclaimed.

When the Terminal process exits:
- Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` kills wta.
- wta's drop handlers fire (best effort) but the process is going
  away regardless.

### Per-tab persistence across tab switches

A tab whose user is currently looking at a different tab keeps its
agent pane's `RenderCtx` and `TabSession` alive. The pane is just
hidden in the XAML layer; wta keeps its end of the conpty
connection open. Switching back to that tab shows the conversation
exactly where it was left.

This matches the post-PR#50 semantic of "wta owns per-tab state"
and is non-negotiable.

## Background: conpty + stdout multiplexing

This section exists for readers who don't have the conpty mental
model.

A Windows process has a single stdin/stdout pair (HANDLE 0 and
HANDLE 1). Windows Terminal makes a child process appear to run
"inside a terminal" by creating a **conpty** (pseudo console):

```
                  conpty kernel object
                ┌──────────────────────┐
                │                      │
   ┌── master ─►│                      │◄── slave ──┐
   │           └──────────────────────┘             │
   │                                                │
   │   ┌──────────────────────┐    ┌──────────┐    │
   └──►│  Windows Terminal    │    │ wta.exe  │◄───┘
       │  (TermControl reads) │    │ (writes  │
       │                      │    │  stdout) │
       └──────────────────────┘    └──────────┘
```

- **slave side**: handed to the child as its stdin/stdout. From the
  child's perspective it looks like a normal terminal.
- **master side**: held by the parent (WT). Reading produces
  whatever bytes the child wrote; writing sends keystrokes back.

A process can only have one stdin/stdout pair, **but it can hold
any number of additional HANDLEs**. If wta receives multiple conpty
slave handles via `DuplicateHandle` (from WT), it can write to each
one independently. Ratatui's `Terminal<CrosstermBackend<W>>` is
generic over `W: Write`, so wta can construct N independent
`Terminal` instances — one per agent pane — each writing to a
different conpty slave handle:

```rust
let writer_t1 = ConptyWriter::from_handle(handle_for_tab_t1);
let terminal_t1 = Terminal::new(CrosstermBackend::new(writer_t1));

let writer_t2 = ConptyWriter::from_handle(handle_for_tab_t2);
let terminal_t2 = Terminal::new(CrosstermBackend::new(writer_t2));
```

Each `Terminal` is independent state. They render concurrently from
one process. This is the linchpin enabling shared wta.

## Architecture diagram (steady state)

```
┌─────────────────────────────────────────────────────────────────┐
│                  Terminal Process (PID 1234)                    │
│                                                                 │
│  ┌─────────────────────┐  ┌─────────────────────┐               │
│  │  Window A           │  │  Window B           │               │
│  │  TerminalPage_A     │  │  TerminalPage_B     │               │
│  │  ├─ Tab T1 (active) │  │  ├─ Tab T3          │               │
│  │  │  └─ AgentPane    │  │  │  └─ AgentPane    │               │
│  │  │     TermControl1 │  │  │     TermControl3 │               │
│  │  └─ Tab T2          │  │  └─ Tab T4 (active) │               │
│  │     └─ AgentPane    │  │     └─ (no agent)   │               │
│  │        TermControl2 │  │                     │               │
│  └────────┬────────────┘  └─────────┬───────────┘               │
│           │                          │                          │
│           └───────────┬──────────────┘                          │
│                       │ COM (existing IProtocolEventCallback)   │
│                       │   • Terminal → wta: OnEvent(json)       │
│                       │     (commands + push events)            │
│                       │   • wta → Terminal: proxy.* calls       │
│                       │     and proxy.SendEvent(json) for acks  │
│                       ▼                                         │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  wta (singleton, PID 5001)                              │    │
│  │                                                         │    │
│  │  tab_sessions: {                                        │    │
│  │     T1 → TabSession(claude, history, autofix),          │    │
│  │     T2 → TabSession(copilot, history, autofix),         │    │
│  │     T3 → TabSession(claude, history, autofix),          │    │
│  │  }                                                      │    │
│  │                                                         │    │
│  │  render_ctxs: {                                         │    │
│  │     T1 → RenderCtx(Terminal→conpty_slave_handle_1),     │    │
│  │     T2 → RenderCtx(Terminal→conpty_slave_handle_2),     │    │
│  │     T3 → RenderCtx(Terminal→conpty_slave_handle_3),     │    │
│  │  }                                                      │    │
│  │                                                         │    │
│  │  ↓ spawns                                               │    │
│  │  claude-acp child × N    copilot child × M     ...      │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                 │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  TerminalProtocolComServer (singleton, unchanged)       │    │
│  └─────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────┘
```

Notice: **no windowId anywhere inside wta**. Tabs T1/T2 in Window A
and T3 in Window B all live side-by-side in wta's flat map.

## What needs to change

### Control channel: no new IDL — reuse existing COM event path

There is **no new COM interface and no new IDL**. The existing
`IProtocolEventCallback` subscription that wta already creates at
startup carries Terminal → wta commands. The existing
`IProtocolServer.SendEvent` carries wta → Terminal replies and
notifications. Both ends are infrastructure that ships today.

Why this is enough: every Terminal → wta message we need to add
(attach / detach / resize) is structurally a JSON payload with a
`method` field. `IProtocolEventCallback.OnEvent(String eventJson)`
is literally "Terminal sends a JSON string to wta." Reusing it
avoids introducing a parallel transport just to repeat the same
shape of work.

#### Message catalog

Terminal → wta, sent via `callback.OnEvent(json)`:

```jsonc
// New control commands
{ "method": "_internal.attach_pane",
  "id": "<correlation id, optional>",
  "params": {
    "tab_id": "T7",                     // tab StableId
    "pty_in":  968,                     // conpty slave read HANDLE
                                        // (already DuplicateHandle'd
                                        // into wta's process)
    "pty_out": 976,                     // conpty slave write HANDLE
    "agent_id": "copilot",              // wta knows which CLI to spawn
    "initial_cwd": "C:\\...",
    "initial_view": "chat"
  } }

{ "method": "_internal.detach_pane",
  "params": { "tab_id": "T7" } }

{ "method": "_internal.resize_pane",
  "params": { "tab_id": "T7", "rows": 40, "cols": 120 } }

// Existing event types continue to flow over the same callback:
//   vt_sequence, tab_changed, set_agent_state, ...
```

wta → Terminal, sent via `proxy.SendEvent(json)`:

```jsonc
{ "method": "_internal.attach_pane_ack",
  "id": "<correlation id from request>",
  "params": { "tab_id": "T7", "status": "ok" } }

// Other wta-originated events (e.g. agent_state_changed) flow over
// the same SendEvent path as today.
```

Method names prefixed with `_internal.` are by convention reserved
for Terminal ↔ wta coordination. Other ComServer subscribers (e.g.
`wtcli listen --json`) should ignore unknown methods — they already
do for unrecognized events, so this is no behavior change.

#### Handle marshaling

The two HANDLE values in `attach_pane` are duplicated by Terminal
into wta's process via `DuplicateHandle` **before** the OnEvent
call, and the returned numeric values (valid HANDLE numbers in
wta's address space) are serialized as JSON numbers in `pty_in` /
`pty_out`. Once OnEvent returns, Terminal must close its own copies
of the slave-side handles; wta now owns them.

Pseudocode (Terminal side):

```cpp
HANDLE hSlaveIn  = /* conpty slave-in side */;
HANDLE hSlaveOut = /* conpty slave-out side */;
HANDLE hWtaInWta, hWtaOutInWta;

DuplicateHandle(GetCurrentProcess(), hSlaveIn,
                hWtaProcess, &hWtaInWta,
                0, FALSE, DUPLICATE_SAME_ACCESS);
DuplicateHandle(GetCurrentProcess(), hSlaveOut,
                hWtaProcess, &hWtaOutInWta,
                0, FALSE, DUPLICATE_SAME_ACCESS);

CloseHandle(hSlaveIn);
CloseHandle(hSlaveOut);

const auto json = BuildAttachPaneJson(tabId, hWtaInWta, hWtaOutInWta, ...);
wtaCallback.OnEvent(winrt::to_hstring(json));
```

### WT-side changes

- Singleton wta lifecycle owned by `WindowEmperor`. Lazy spawn on
  first agent-pane request; tied to a process-scope Job Object so
  it dies with Terminal.
- `_AutoCreateHiddenAgentPane` no longer spawns wta as a conpty
  client. Instead: create a conpty, `DuplicateHandle` the slave
  ends into wta, send `_internal.attach_pane` via wta's
  `IProtocolEventCallback.OnEvent`.
- The "one agent pane per window, relocated across tabs on tab
  switch" model is replaced by "one agent pane per tab." Each tab
  that wants an agent pane has its own conpty + TermControl pair.
  Functions tied to the shared model are deleted or rewritten:
  - `_agentPane` weak_ptr on TerminalPage → gone.
  - `_RelocateAgentPaneToTab` → gone.
  - `_FindTabContainingAgentPane`, `_FindAgentPane` → gone or
    repurposed as per-tab lookups.
  - "Rescue" code paths in `TabManagement.cpp` (which detach the
    shared pane off a closing tab) → gone: each tab owns its own
    pane that goes away with it.
- Remove `_agentPaneJob`, `_agentPaneWtaHandle`,
  `_SetupAgentPaneWtaWatch` (per-TerminalPage process management).
  Move watch logic to WindowEmperor scope.
- `ConptyConnection` (or a new `AgentConptyConnection` variant)
  needs a "create-but-don't-spawn" mode that returns the conpty
  master HANDLEs to WT and the slave HANDLEs ready for
  `DuplicateHandle` into wta. No `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`
  on a child process for agent panes — the conpty exists, just
  unattached on the slave side until wta takes its handles.
- AgentPaneContent semantics unchanged: still wraps a TermControl
  reading from the conpty master side.
- A small `WtaController` singleton holds wta's process handle and
  its registered `IProtocolEventCallback` proxy reference. This is
  the choke point for issuing control commands.

### wta-side changes

The biggest delta. Roughly:

- **Replace single Ratatui main loop with multi-renderer
  dispatcher**:
  - One tokio task per `RenderCtx` for reading conpty input.
  - Central dispatcher routes incoming events (autofix, ACP
    messages, user input from any pane) to the correct
    `TabSession`.
  - Renders happen on each `RenderCtx`'s task, not in a global
    render thread.
- **OnEvent handler grows control-command dispatch**: wta's
  existing `IProtocolEventCallback` implementation already receives
  JSON via `OnEvent`. Extend the method dispatcher to recognize
  `_internal.attach_pane`, `_internal.detach_pane`,
  `_internal.resize_pane` and route them to the multi-renderer
  dispatcher. **No new COM interface, no new IDL.**
- **Lifecycle**: wta starts headless — does not bind a Ratatui
  Terminal to `std::io::Stdout`. It registers its event callback,
  subscribes to ComServer, then waits. The existing TUI mode is
  retained as `--legacy-tui` for standalone debugging / CLI use.
- **Ratatui per renderer**: each `RenderCtx` owns its own
  `Terminal<CrosstermBackend<ConptyWriter>>`. ConptyWriter is a
  thin `impl Write` around a Windows HANDLE.
- **Acks via SendEvent**: when `_internal.attach_pane` succeeds (or
  fails), wta calls `proxy.SendEvent` with an
  `_internal.attach_pane_ack` payload carrying the correlation id.
  Terminal's existing event subscription path delivers it back to
  `WtaController`.
- **No-op the drag**: nothing in the drag path is needed. wta
  literally ignores the move.
- **Drop the windowId filter**: PR #50's window_id check on
  `set_agent_state` becomes unnecessary; remove it (or assert
  single-window-id and let it become trivially true).
- **ACP child process accounting**: each `TabSession` still spawns
  its own agent CLI child (claude/copilot/gemini). N panes = N
  agent children. Already handled by current per-tab logic.

### Tests / validation

- Multi-pane unit test: simulate two panes, send input to each,
  verify state isolation, verify each pane's render output is
  independent.
- Drag test (in-process): create a tab with agent pane in Window A,
  use `_MoveTab` to move it to Window B, verify conversation
  continues uninterrupted with no observable hitch.
- Stress: 10+ agent panes across 5 windows, verify resource usage
  and responsiveness.
- Crash resilience: kill wta mid-conversation, verify
  WindowEmperor detects and respawns (or surfaces error gracefully).

## Implementation plan

### Stage 0: Spec sign-off and prototype (1–2 weeks)

- Get this document reviewed.
- Write a standalone Rust prototype: a process that receives a
  conpty slave HANDLE pair via command-line (testing harness) and
  renders Ratatui "hello world" into it. Verify end-to-end conpty
  handle passing works.

### Stage 1: Control-command dispatch + wta headless skeleton (2–3 weeks)

- Define JSON schemas for `_internal.attach_pane`,
  `_internal.detach_pane`, `_internal.resize_pane`, and their
  `_ack` counterparts. Document adjacent to the existing event
  catalog.
- Add `--headless` mode to wta that:
  - skips Ratatui-on-stdout binding,
  - registers `IProtocolEventCallback`, subscribes to ComServer,
  - extends OnEvent dispatch to recognize the new `_internal.*`
    methods.
- Implement `_internal.attach_pane` minimally: open the HANDLEs,
  log them, echo keystrokes back via the slave-out handle. Verify
  handle marshaling end-to-end on a test harness.

### Stage 2: wta multi-pane rendering (4–6 weeks)

- Refactor wta's main loop to support N concurrent `RenderCtx`.
- Move all business logic (tab_sessions, ACP routing, autofix) out
  of the TUI render loop into a central dispatcher.
- Per-pane tokio tasks for input + render.
- Keep `--legacy-tui` as an alternative entry point for CLI use;
  under it, the new dispatcher runs with exactly one `RenderCtx`
  bound to process stdio.

### Stage 3: WT-side per-tab agent-pane model (3–4 weeks)

- Remove the "shared agent pane per window" abstractions
  (`_agentPane`, `_RelocateAgentPaneToTab`, the relocate/rescue
  helpers).
- Introduce per-tab agent pane ownership. Each tab that opens an
  agent pane creates its own TermControl + conpty + `attach_pane`
  call to wta. The `Tab.AgentPaneOpen` flag now strictly means
  "this tab has its own agent pane right now."
- Implement singleton wta spawn in `WindowEmperor`. Pass
  `WT_COM_CLSID` via env var as today; wta's existing COM client
  logic finds the ComServer unchanged.
- New `AgentConptyConnection` (or extend `ConptyConnection`) for
  the "create conpty, DuplicateHandle slave ends into wta,
  OnEvent attach_pane" pattern.
- Replace per-TerminalPage wta spawn with `WtaController` calls.
- Migrate `_agentPaneJob` and process watch logic to global scope.
- Behind a setting flag: `aiIntegration.sharedWtaProcess` (default
  false during dev).

### Stage 4: Drag scenario validation + cleanup (1–2 weeks)

- Run drag scenarios; verify zero-loss.
- Verify autofix continues across drag.
- If issues, instrument and iterate.

### Stage 5: Default on, deprecate per-window mode (2 weeks)

- Flip setting default to `true`.
- After bake-in period, remove old per-window code paths entirely.

**Total: 12–17 weeks** (≈3–4 months) for one focused engineer.

## Risks and open questions

### R1. Conpty handle marshaling via OnEvent JSON

HANDLE values are duplicated into wta's process via
`DuplicateHandle` and then serialized as plain JSON numbers in the
`_internal.attach_pane` payload. The COM layer never sees a HANDLE
type; it only sees a JSON string. Ownership and lifetime:
- Pre-OnEvent: Terminal holds the slave HANDLEs and has just
  DuplicateHandle'd them into wta. Both Terminal-side and wta-side
  references exist transiently.
- Post-OnEvent (after Terminal returns from the call): Terminal
  closes its own slave HANDLEs. wta owns its duplicated copies.
- WT keeps the master side throughout. Both sides close their
  respective ends on `_internal.detach_pane`.
- Concrete failure to watch for: if Terminal closes its slave
  HANDLEs *before* OnEvent returns, no harm — wta's duplicated
  copies are independent references to the underlying pipe.

### R2. wta as single point of failure

A wta crash kills all agent panes across all windows. Mitigations:
- panic handlers that confine a panic to one `RenderCtx`'s task
  where possible.
- WindowEmperor monitors wta health, respawns on crash, surfaces
  the event to the user.
- Per-`TabSession` state should be checkpointable so a wta restart
  can attempt to resume. Tracked under F1 below.

### R3. Multiplexed Ratatui rendering

Ratatui itself is not thread-safe; each `Terminal` must be touched
from one task only. With tokio per-pane tasks this is fine, but
shared state (`TabSession`) needs the right synchronization
(`Arc<Mutex<...>>` or actor-style channels).

### R4. ACP child process count

N agent panes = N CLI children (claude / copilot / gemini). Each
brings real resource overhead. We may need to add limits or
on-demand spawn (delay until first message). Existing behavior is
the same, but the visibility of "10 wta-spawned claudes in Task
Manager" is more striking under shared-wta where there's only one
wta to root them at.

### R5. Backwards compatibility / setting rollout

The new shared-wta model and the existing per-window model coexist
behind a setting flag during Stages 3–4. After Stage 5 the old code
is removed. The setting only matters for the rollout window
itself.

### R6. CLI / standalone wta use; rejected alternative transport

`wta delegate ...`, `wta list-windows`, etc. should not require a
running shared wta. These remain standalone invocations that exit
when done. They share no state with the shared wta.

Standalone `wta` and shared `wta` are the same binary,
distinguished by `--headless` (shared mode) vs default (CLI /
one-shot mode). The `--legacy-tui` mode keeps a single-pane
Ratatui-over-stdout entry point alive for debugging and CLI
hand-use.

**Rejected alternative**: a separate stdin/stdout JSON-RPC control
channel between Terminal and wta. Considered because wta already
runs ACP over stdin/stdout against its child agent CLIs, so the
pattern was familiar. Rejected because the COM
`IProtocolEventCallback` link is already established at wta startup
and already carries JSON-typed messages — adding a parallel pipe
would mean two transports to wire up, monitor, and keep alive in
lockstep, for no functional gain. Control volume is low (one
message per pane open/close/resize) so the COM marshaling overhead
is irrelevant.

### R7. Resize protocol

Today TermControl resizes by calling `ResizePseudoConsole` on the
master side. wta sees the new dimensions via Crossterm. Under
shared wta, the same flow works per-pane — WT calls
`ResizePseudoConsole` on the master side and the conpty
SIGWINCH-equivalent propagates to wta automatically. The
`_internal.resize_pane` OnEvent is therefore informational only and
arguably optional; we keep it in the catalog so wta can update any
non-Crossterm-derived sizing state in `TabSession` if needed.

### R8. Drag implementation detail

`TermControl`/`ContentId` reattachment across windows is an
existing mechanism for non-agent panes. Verify it actually
preserves conpty master handle and IPC pipe identity in our
codebase before committing to "zero-touch drag." Add a Stage 0
spike to confirm.

### R9. Per-tab vs per-window agent pane: existing-feature audit

The shift from "one shared pane per window, relocated on tab
switch" to "per-tab independent panes" changes several user-facing
behaviors that the current code relies on:

- **Toggle AI Assistant** opens/hides the active tab's pane —
  natural under the new model, but the action handler needs to
  consult per-tab state rather than the shared `_agentPane`.
- **Autofix routing** is already per-tab (PR #50); no change
  expected.
- **Bottom bar / diagnostics** is per-tab today (PR #49); the
  display logic needs to read from the active tab's own pane, not
  from a singleton.
- **Pre-warming** (`_AutoCreateHiddenAgentPane` at first tab init)
  is the wrong granularity under per-tab. Re-think: maybe no
  pre-warming at all, or pre-warm only the first agent-pane open.

This is a Stage 3 work item but worth flagging early because it
touches multiple UI paths.

## What this does NOT solve (out of scope)

- **Cross-process Terminal instances**: if WT is configured for
  multi-instance, each instance has its own wta. Bridge between
  them not addressed.
- **Persistent state across Terminal restart**: closing and
  reopening WT still loses session state (unless agent CLI itself
  persists; see current behavior of claude/copilot/gemini history
  files).
- **Remote agents**: this spec assumes local ACP child processes.
- **Restructuring agent pane UI to XAML**: shared wta keeps the
  Ratatui TUI model. Migration to native XAML chat UI is a
  separate larger spec, not blocked by or blocking this work.

## Future work

- **F1**: `TabSession` checkpoint + restore across wta restarts.
  Closes R2 mitigation. Persists to disk; wta on respawn loads and
  resumes via ACP `session/load`. Not blocked by this spec but
  only makes sense after it.
- **F2**: `IProtocolEventCallback.Subscribe` with window/tab
  filter parameters. Useful for non-wta subscribers (e.g. wtcli)
  under multi-window. Independent of this spec.
- **F3**: ComServer caller-identity hooks. Defense-in-depth
  against hostile or buggy callers. Not currently exploited;
  deprioritized.
- **F4**: Migration of agent pane UI from Ratatui TUI to native
  XAML chat surface. Separate, larger spec; this spec leaves
  Ratatui in place.
