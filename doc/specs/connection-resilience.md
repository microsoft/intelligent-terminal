# Agent Connection Resilience — model, status, and follow-ups

This document captures how the `tab → helper → master → agent CLI` connection
detects and notifies the user of a disconnect, **what is verified**, and — the
main purpose here — **what we deliberately deferred or have not yet done**. It
is the running TODO/known-gaps list for connection resilience, written alongside
the first MVP (PR #141).

See also `Multi-window-agent-pane.md` for the helper+master architecture and the
top-level `AGENTS.md` for the runtime/log layout.

---

## 1. The connection, in one picture

```
helper ──ACP / named pipe──► wta-master ──ACP / stdio──► agent CLI (copilot/node)
            hop 1                              hop 2
```

There are **two** ACP hops. A local named pipe / stdio never "drops on its own"
— a break is always rooted in a process dying:

- **hop 1 breaks** = `wta-master` died → every helper's pipe goes EOF.
- **hop 2 breaks** = the agent CLI (node/copilot) died → master detects it and
  shuts itself down (no in-master agent respawn), which cascades to hop 1.

So from the helper's point of view, **every** disconnect surfaces the same way:
the pipe to master ends.

## 2. Detection & notification model (current, post-PR #141)

**Detection is signal-based, not string-matched.** The helper's `handle_io`
task is the single sentinel: when the pipe to master ends — on **either** a
clean EOF (`Ok`) **or** an error (`Err`) — it emits `MasterDisconnected`.

- `tools/wta/src/protocol/acp/client.rs` ~2174–2191 (`run_acp_client_over_pipe`,
  the `match handle_io.await { … }` block). Both arms emit. **Keying on `Err`
  only would miss the common case** — a killed master resolves the loop as `Ok`
  (clean EOF), confirmed in a real trace.

**Notification rules**:

1. **Connection loss** (master/agent died) → `MasterDisconnected`; the helper
   exits without attempting recovery.
2. **A connection that fails to *establish*** (startup handshake: pipe connect /
   `initialize` / `session/new` timeouts) → the raw error is **returned as-is**
   (`helper ACP transport failed: {e:#}`, `tools/wta/src/main.rs` ~2263). Not
   re-classified. The raw `{e:#}` also stays in the helper log.
3. **Auth failures** → routed to the sign-in screen by the handler's
   `is_auth_error` check (`app.rs` ~4064).
4. A near-simultaneous prompt error and pipe-loss signal may both be logged.
   The terminal `MasterDisconnected` event wins and ends the helper.

**Crash recovery is not attempted.** The helper exits. A later user-initiated
pane open creates a fresh master/helper/session. `/restart` remains available
only while a live helper can explicitly request a fresh stack; it is not
retained as a crash screen.

**Design principle adopted mid-review:** *no fragile substring classification of
error text.* An earlier version classified errors into
`connection.timeout`/`start_failed`/`lost` and even matched auth markers; that
was removed because keyword matching on error strings is brittle (it silently
swallowed auth failures). The clean signal (pipe state) drives the notification;
the only remaining string match is the **preexisting** `is_auth_error` (see
gaps below).

## 3. Verified live (dev build)

| Scenario | Result |
|---|---|
| Idle master death, single tab | `pipe closed (master gone)` → helper exit; no session load |
| Idle master death, multi-tab | each helper observes the shared pipe close and exits independently |
| Agent CLI death (hop 2) | affected transport terminates; helpers exit without session replay |

## 4. NOT yet verified (follow-ups)

These are expected to work from the code/tests but were **not** exercised in the
live app:

- **In-flight master death** (kill master *while a prompt is streaming*):
  confirm the raw prompt failure is logged before the helper exits and no
  session is loaded.
- **Auto error handling gated off after disconnect**: once state leaves `Connected`,
  the request gate returns early. Confirm a shell
  command failure after a disconnect does **not** start an agent request.
- **Normal teardown** (close pane / close tab / quit / `Ctrl+C×2`): confirm
  helper and listener exit without a visible transient failure.
- **F7 connecting animation during a real cold start** (npx adapter download):
  the animated "Connecting to agent…" line only matters when the handshake takes
  tens of seconds; every test so far hit a warm (<2 s) connect.
- **Multi-window** (we covered multi-tab in one window): kill the shared master
  with agent panes in two windows.
- **Startup-failure raw display**: confirm pipe-connect/init/session timeouts
  show the raw line (the intended "return as-is" behavior).

## 5. Deferred work (out of scope for this round, by decision)

The remaining deferred work is **wedge detection**. Pipe EOF detects process
exit, not a live-but-stuck helper. Heartbeat detection must lead to cleanup, not
automatic session recovery.

## 6. Known gaps (genuine, acknowledged)

- **No `conn.prompt()` timeout → a *hung* agent waits forever.** Only
  `initialize` (60 s), `session/new` (30 s), and `session/load` are wrapped in
  timeouts; `conn.prompt()` is not. If the agent CLI is *alive but unresponsive*
  mid-turn (hop-2 protocol hang, not a process death), the helper spins until the
  user presses Esc. Distinct from a disconnect (which is detected) — this is a
  silent hang. Hard to inject (would need to SIGSTOP the agent process).
- **F1 — agent CLI death = whole-master death (single-agent single point of failure).** Master does
  not respawn the agent CLI; it exits and takes every multiplexed helper down
  (`master/mod.rs` ~1335–1357, ~1565). Blast radius grows with tab count.
  In-master agent respawn plus `cached_init_resp` replay is a deferred proposal,
  not current recovery behavior.
- **F8 — agent-side session leak on helper disconnect.** Addressed: master
  cancels the turn, sends bounded `session/close` when supported, and retires
  local routing/registry state.
- **Residual `is_auth_error` string match.** Auth routing still relies on
  substring-matching the error text (`app.rs` ~4064). It is **preexisting**, not
  added by this work, but it is the same fragility we removed elsewhere. The
  clean fix is for the ACP layer to surface a **typed** auth error rather than a
  string, so neither side has to pattern-match.
- **F10 — Auto error handling events in a non-`Connected` state are dropped, not queued.** A
  command failure that lands during cold start / in the `Failed` window is
  denied an agent request and never replayed once the session connects. This is currently
  *intended* (don't send into a dead transport), but a "replay the last
  failure on reconnect" enhancement is possible.

## 7. Failure-point status table

`F1–F10` are the failure points enumerated in the original code-read. Status as
of PR #141:

| # | Failure point | Status |
|---|---|---|
| F1 | agent CLI death → whole master down (single point of failure) | **deferred** (§6) |
| F2 | master crash → C++ lazy respawn, open panes zombie | **fixed** — helpers exit, panes close, and only a later explicit pane open starts fresh (§8) |
| F3 | idle master death silently stayed `Connected` | **fixed** — watchdog exits helper |
| F4 | in-flight prompt death | **fixed** — transport termination exits helper |
| F5 | helper/conpty death → zombie pane, no respawn | **fixed** — pane closes; no automatic recovery |
| F6 | handshake/timeout failures | **handled** — returned as-is (raw), by decision |
| F7 | connecting looked frozen | **fixed** — animated activity line; cold-start not verified live (§4) |
| F8 | agent-side session leak on disconnect | **fixed** — bounded cancel/close + local retirement |
| F9 | routing to a dead helper | **already graceful** (no work) |
| F10 | Auto error handling event dropped in non-Connected state | **intended**, replay possible (§6) |

## 8. Terminal crash semantics — fail closed, never auto-resume

**Scope.** Helper, master, and agent exits are terminal lifecycle events. They do not
trigger process respawn or ACP `session/load`. Automatic conversation restoration is
unsafe because the session may have caused the failure, its provider may have
changed, or its last operation may not be safe to repeat.

Normal pane stash/restore is unaffected because no process or transport failed.
Explicit history Resume is also unaffected: it is a user action and follows the
normal agent-scoped `session/load` path.

Ordered cross-window tab migration is also not crash recovery. Both action-driven
`moveTab` and tab-strip drag carry the live TermControl/conpty/helper through the
ContentId handoff, transfer the `SharedWta` pane reference, and rekey tab/window
ownership with `tab_renamed`. The ACP SessionId and chat history remain live; no
helper restart or `session/load` is involved.

### 8.1 Two ways a helper "dies"

| | **Exit** | **Wedge** |
|---|---|---|
| process | gone (panic→exit 101, killed, OOM) | **alive but stuck** (deadlock / blocked task) |
| conpty child | gone | still alive |
| C++ `ConnectionState` | `Closed` | still `Connected` (invisible to C++) |
| master pipe (`serve_helper`) | **read loop ends → detected for free** | pipe open, read loop just waits → **not detected** |

Exit is detected without a heartbeat. A wedge needs an active probe and remains a
manual close case.

> Note: the Agent Pane profile is `closeOnExit:"always"` (`defaults.json:48`), so a
> helper that *cleanly exits* would have its pane auto-closed by WT. A pane that
> *freezes* instead of vanishing is therefore a **wedge**, not an exit — which is why
> the observed hang (helper frozen, pane still visible) falls in the deferred bucket
> and the user-close-tab path until §8.5 lands.

### 8.2 Helper exit — master cleans up, Terminal does not respawn

`serve_helper` reads each helper's pipe until EOF/error. A helper exit breaks the
pipe, after which master:

1. Cancels each helper-owned active turn.
2. Sends bounded `session/close` when the agent advertises it.
3. Retires local routing, registry, usage, and capability state.
4. Removes helper ownership metadata.

Master emits no pane-restart event. The helper's ConPTY process exit is the
authoritative pane lifecycle signal, and the Agent Pane profile's
`closeOnExit:"always"` closes the pane.

### 8.3 Master exit — helpers exit and release children

The helper's ACP I/O task treats both pipe error and clean EOF as master loss. It
posts `MasterDisconnected`, ends the TUI, and exits. It does not remain in a
transport-lost view and does not reconnect.

Each helper owns one persistent `wtcli --json listen` child. Normal teardown uses
`kill_on_drop(true)`. The listener also watches the owning WTA process through
`--parent-pid`, so it exits even when the owner is force-terminated and Rust
destructors do not run.

`SharedWta` records that the master exited and clears its process state. It does not
recreate panes. If the user later opens an agent pane, that explicit `AcquirePane`
starts a fresh master and the new helper creates a fresh ACP session.

### 8.4 Explicit restart and resume

- `/restart` remains an explicit command. It tears down the current stack and opens
  fresh helpers/sessions; it does not load the crashed session.
- Resume from history remains explicit and may use
  `--initial-load-session-id`. Crash handling never supplies that flag.
- Agent/model settings changes are planned teardown/rebuild operations and start a
  fresh session.

### 8.5 Deliberately deferred

- **Wedge detection (heartbeat).** A helper that hangs without exiting won't break the
  pipe. If wedges prove common, add helper heartbeat detection, but treat timeout as
  terminal cleanup rather than an automatic restart.
- **Panic hook (separate, recommended).** Today a helper main-thread panic leaves the
  conpty in raw / alt-screen (frozen frame) and logs nothing (the non-blocking
  appender's buffered tail is lost on unwind, and the panic text goes to stderr, i.e.
  the alt-screen). A panic hook should restore the terminal, log the panic, and flush
  before exit.

### 8.6 Status

| Item | State |
|---|---|
| Helper exit detection through master pipe EOF | **done** |
| Helper-owned ACP cancel/close on disconnect | **done** |
| Automatic pane respawn/session load | **removed** |
| Helper exit on master pipe EOF | **done** |
| `wtcli listen` child ownership | **done (`kill_on_drop` + parent process watcher)** |
| Explicit history Resume | **preserved** |
| Wedge heartbeat | **deferred (§8.5)** |
| Panic hook (diagnostics) | **separate follow-up (§8.5)** |
