# Agent Connection Resilience — model, status, and follow-ups

This document captures how the `tab → helper → master → agent CLI` connection
detects and notifies the user of a disconnect, **what is verified**, and — the
main purpose here — **what we deliberately deferred or have not yet done**. It
is the running TODO/known-gaps list for connection resilience, written alongside
the first MVP (PR #141).

See also `Multi-window-agent-pane.md` for the helper+master architecture and the
top-level `CLAUDE.md` for the runtime/log layout.

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
clean EOF (`Ok`) **or** an error (`Err`) — it emits `AgentError { message:
connection.lost }`.

- `tools/wta/src/protocol/acp/client.rs` ~2174–2191 (`run_acp_client_over_pipe`,
  the `match handle_io.await { … }` block). Both arms emit. **Keying on `Err`
  only would miss the common case** — a killed master resolves the loop as `Ok`
  (clean EOF), confirmed in a real trace.

**Notification rules** (`AppEvent::AgentError` handler,
`tools/wta/src/app.rs` ~4049):

1. **Connection loss** (master/agent died) → the localized, actionable line
   **"Connection to the agent was lost. Type `/restart` to reconnect."**
   (`connection.lost`). This is the user-facing recovery hint.
2. **A connection that fails to *establish*** (startup handshake: pipe connect /
   `initialize` / `session/new` timeouts) → the raw error is **returned as-is**
   (`helper ACP transport failed: {e:#}`, `tools/wta/src/main.rs` ~2263). Not
   re-classified. The raw `{e:#}` also stays in the helper log.
3. **Auth failures** → routed to the sign-in screen by the handler's
   `is_auth_error` check (`app.rs` ~4064).
4. The two message kinds **coexist**: the raw error says *what broke*, the
   `connection.lost` line says *how to recover*. Dedup collapses only
   **identical** consecutive errors (`app.rs` ~4129), so the `/restart` hint is
   never hidden behind a different/in-flight error.

**Recovery is manual** (MVP): `/restart` (a registered slash command) tears down
and respawns the whole agent stack on the same stable pipe name. The helper's
main loop keeps running after the pipe dies, so `/restart` is always available.

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
| Idle master death, single tab | `pipe closed (master gone)` → `connection.lost`; `/restart` recovered on same pipe name |
| Idle master death, multi-tab | both helpers detected in the same ms; per-tab isolation held; one `/restart` healed the whole stack (`live_helpers` N→0→N) |
| Agent CLI death (hop 2) | killing `node`/`copilot` under master cascaded in ~12 ms: master logged `agent CLI … initiating master shutdown` → exit (no silent death) → every helper `connection.lost` |

## 4. NOT yet verified (follow-ups)

These are expected to work from the code/tests but were **not** exercised in the
live app:

- **In-flight master death** (kill master *while a prompt is streaming*): expect
  two lines — the raw `prompt error: …` (returned as-is) **and** the
  `connection.lost` `/restart` line. Unit-tested
  (`transport_loss_surfaces_restart_hint_even_behind_another_error`), not seen
  live.
- **Autofix gated off after disconnect**: once state leaves `Connected`,
  `trigger_autofix_inner` early-returns (`app/autofix.rs:97`). Confirm a shell
  command failure after a disconnect does **not** fire an autofix.
- **No spurious "connection lost" flash on normal teardown** (close pane / close
  tab / quit / `Ctrl+C×2`): the watchdog emits on the `Ok` arm too, so confirm a
  clean shutdown doesn't surface the error (it shouldn't — the process is being
  torn down).
- **F7 connecting animation during a real cold start** (npx adapter download):
  the animated "Connecting to agent…" line only matters when the handshake takes
  tens of seconds; every test so far hit a warm (<2 s) connect.
- **Multi-window** (we covered multi-tab in one window): kill the shared master
  with agent panes in two windows.
- **Startup-failure raw display**: confirm pipe-connect/init/session timeouts
  show the raw line (the intended "return as-is" behavior).

## 5. Deferred work (out of scope for this round, by decision)

The first round was **Rust/helper-side, manual-retry only**. These were
explicitly left for later:

- **C++ master auto-respawn on crash.** `SharedWta::_OnProcessExited`
  (`SharedWta.cpp` ~385) only *clears state* on master death; recovery is
  **lazy** — it respawns on the next `AcquirePane`. Already-open agent panes
  whose helper lost its master become "process exited" zombies until the user
  toggles them. → make respawn active when live agent panes exist.
- **C++ dead-pane auto re-warm.** A helper conpty death leaves a dead pane with
  only the generic `CloseOnExitInfoBar`; no helper respawn. → re-warm a fresh
  helper for the affected tab.
- **Helper auto-reconnect.** The pipe-connect retry loop only covers the
  **initial** connect (`client.rs` ~2084). After connect, a master death just
  surfaces `connection.lost` and waits for manual `/restart`. The substrate for
  transparent reconnect already exists (the pipe-name GUID is stable across
  master respawns, `SharedWta.cpp` ~189–211) — only the reconnect loop is
  missing.

## 6. Known gaps (genuine, acknowledged)

- **No `conn.prompt()` timeout → a *hung* agent waits forever.** Only
  `initialize` (60 s), `session/new` (30 s), and `session/load` are wrapped in
  timeouts; `conn.prompt()` is not. If the agent CLI is *alive but unresponsive*
  mid-turn (hop-2 protocol hang, not a process death), the helper spins until the
  user presses Esc. Distinct from a disconnect (which is detected) — this is a
  silent hang. Hard to inject (would need to SIGSTOP the agent process).
- **F1 — agent CLI death = whole-master death (single-agent single point of failure).** Master does
  not respawn the agent CLI; it exits and takes every multiplexed helper down
  (`master/mod.rs` ~1335–1357, ~1565). Blast radius grows with tab count. → an
  in-master agent respawn + `cached_init_resp` replay would downgrade "agent
  crash" from "everyone dies" to "brief blip".
- **F8 — agent-side session leak on helper disconnect.** When a helper
  disconnects, master drops its local routing/registry entries but does **not**
  tell the agent CLI to release those sessions (no `session/cancel`). They linger
  in the agent CLI, unreachable.
- **Residual `is_auth_error` string match.** Auth routing still relies on
  substring-matching the error text (`app.rs` ~4064). It is **preexisting**, not
  added by this work, but it is the same fragility we removed elsewhere. The
  clean fix is for the ACP layer to surface a **typed** auth error rather than a
  string, so neither side has to pattern-match.
- **F10 — autofix events in a non-`Connected` state are dropped, not queued.** A
  command failure that lands during cold start / in the `Failed` window is
  denied autofix and never replayed once the session connects. This is currently
  *intended* (don't autofix into a dead transport), but a "replay the last
  failure on reconnect" enhancement is possible.

## 7. Failure-point status table

`F1–F10` are the failure points enumerated in the original code-read. Status as
of PR #141:

| # | Failure point | Status |
|---|---|---|
| F1 | agent CLI death → whole master down (single point of failure) | **deferred** (§6) |
| F2 | master crash → C++ lazy respawn, open panes zombie | **deferred** (§5) |
| F3 | idle master death silently stayed `Connected` | **fixed** — watchdog both-arm emit |
| F4 | in-flight prompt death | **fixed** — surfaces error + `connection.lost`, not verified live (§4) |
| F5 | helper/conpty death → zombie pane, no respawn | **deferred** (§5) |
| F6 | handshake/timeout failures | **handled** — returned as-is (raw), by decision |
| F7 | connecting looked frozen | **fixed** — animated activity line; cold-start not verified live (§4) |
| F8 | agent-side session leak on disconnect | **gap** (§6) |
| F9 | routing to a dead helper | **already graceful** (no work) |
| F10 | autofix event dropped in non-Connected state | **intended**, replay possible (§6) |
