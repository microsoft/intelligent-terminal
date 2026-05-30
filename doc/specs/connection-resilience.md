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
  helper for the affected tab. **→ designed in §8 (Phase 2).**
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
| F5 | helper/conpty death → zombie pane, no respawn | **implemented** — §8 (Phase 2); exit-case auto-recovers, wedge deferred |
| F6 | handshake/timeout failures | **handled** — returned as-is (raw), by decision |
| F7 | connecting looked frozen | **fixed** — animated activity line; cold-start not verified live (§4) |
| F8 | agent-side session leak on disconnect | **gap** (§6) |
| F9 | routing to a dead helper | **already graceful** (no work) |
| F10 | autofix event dropped in non-Connected state | **intended**, replay possible (§6) |

## 8. Phase 2 — helper-death recovery (master-detected → C++ respawn)

**Scope.** §2–§7 cover the *helper-survives* direction (the master/agent died; the
helper stays up and shows `connection.lost` + manual `/restart`). This section covers
the **opposite** direction — the **F5** gap: the **helper itself dies or wedges**,
leaving a frozen / zombie agent pane. Goal: the pane recovers **automatically**
instead of sitting dead until the user notices.

Agreed scope is deliberately minimal: **detect death → respawn once → resume history
→ repeat on re-death. No backoff, no degradation ladder, no "agent unavailable"
banner.** The user closing the tab is the escape hatch.

### 8.1 Two ways a helper "dies"

| | **Exit** | **Wedge** |
|---|---|---|
| process | gone (panic→exit 101, killed, OOM) | **alive but stuck** (deadlock / blocked task) |
| conpty child | gone | still alive |
| C++ `ConnectionState` | `Closed` | still `Connected` (invisible to C++) |
| master pipe (`serve_helper`) | **read loop ends → detected for free** | pipe open, read loop just waits → **not detected** |

**Phase 2 handles Exit only** (the common crash shape). **Wedge** needs an active
probe and is deferred (§8.5); until then a wedged pane is the user-close-tab case.

> Note: the Agent Pane profile is `closeOnExit:"always"` (`defaults.json:48`), so a
> helper that *cleanly exits* would have its pane auto-closed by WT. A pane that
> *freezes* instead of vanishing is therefore a **wedge**, not an exit — which is why
> the observed hang (helper frozen, pane still visible) falls in the deferred bucket
> and the user-close-tab path until §8.5 lands.

### 8.2 Detection — reuse the existing master-side pipe sentinel

`serve_helper` (`master/mod.rs:1647`) already reads each helper's pipe in a loop until
EOF/error, then runs `drop_sessions_for_helper` + `live_helpers -= 1` and logs
`"helper disconnected"` (`master/mod.rs:1776–1782`). A helper **exit/kill** breaks the
pipe → this fires **today, for free**. No heartbeat, no new detection code.

This is the mirror image of §2 (where the *helper* is the sentinel for *master*
death). Here the *master* is the sentinel for *helper* death.

### 8.3 Notification — master → C++ over the existing COM event channel

Reuse the exact Rust→C++ path that already carries `close_agent_pane` /
`agent_state_changed`: `IProtocolServer::SendEvent`. The master already holds a
`CliChannel` COM connection to WT (`master/mod.rs:1404`, today used for
`intellterm.wta/focus_session`).

On helper disconnect the master emits:

```json
{ "type": "event", "method": "restart_agent_pane",
  "params": { "tab_id": "<owner StableId>", "session_id": "<last sid>", "reason": "helper_disconnect" } }
```

Routed by `tab_id` (WT StableId — globally unique → correct window **and** tab, via
the same `_FindTabByStableId` path as every per-tab event). No `window_id` needed.

**Prereq — master must know the helper's `owner_tab_id`.** `session_id` is already in
`session_to_helper`. `owner_tab_id` currently lives only on the helper's
`--owner-tab-id` cmdline (set by C++); the helper must **register it with the master
at handshake** (one new field on the connect / `session_hook`). Without it the master
cannot address the event.

### 8.4 C++ receiving side — respawn + resume history

1. `ProtocolParsing.h`: `method == "restart_agent_pane"` → new
   `SendEventRoute::RestartAgentPane`.
2. `TerminalProtocolComServer.cpp`: `_dispatchRestartAgentPaneToPage` → route by
   `tab_id` → `page.OnAgentPaneRestartRequested(eventJson)` (mirror of
   `_dispatchCloseAgentPaneToPage`, ~837).
3. `TerminalPage::OnAgentPaneRestartRequested(tab_id, session_id)`:
   `_FindTabByStableId` → tear down the dead/zombie pane (kills the wedged conpty
   child if still present) → `_AutoCreateHiddenAgentPaneShared(… --initial-load-session-id=session_id)`.
   The fresh helper reconnects to the **still-alive master** (persistent pipe name →
   master replays cached `initialize`; only `session/new` / `session/load` round-trip)
   and ACP `session/load`s the prior session → **chat history resumes**. The
   `--initial-load-session-id` flag already exists (`TerminalPage.cpp:1703`).

**Idempotent w.r.t. `closeOnExit:"always"`.** On an *exit*, the pane may already be
auto-closing when `restart_agent_pane` lands. `OnAgentPaneRestartRequested` must treat
"pane already gone" as "create a fresh stashed helper for this tab", not assume a pane
exists.

**Must not fight a user-initiated close.** `Ctrl+C×2 → close_agent_pane` (the user
deliberately closing) must still close and **not** respawn. The master must emit
`restart_agent_pane` only on an *unsolicited* disconnect — a helper that exits because
C++ tore it down (user close / tab close / settings rebuild) must be distinguishable
(e.g. C++ marks the pane intentionally-closing before teardown, or the master
suppresses restart when it saw a preceding `close_agent_pane` for that tab).

### 8.5 Deliberately deferred

- **Wedge detection (heartbeat).** A helper that hangs without exiting won't break the
  pipe → §8.2 misses it. If wedges prove common, add a **helper → master heartbeat**
  (helper periodically pings; master scans for "no heartbeat in N s" and treats it as a
  disconnect). Helper-pushed is simpler than master-pull — the master only timestamps,
  with no per-helper ping/timeout state machine. Until then: user closes the tab.
- **Backoff / degradation / "agent unavailable" banner.** None. Respawn latency
  (~1–2 s: teardown + conpty spawn + master reconnect + `session/load`) self-throttles
  even a deterministic "poison-session reload crashes again" loop to ~once / 1–2 s — no
  CPU spin, no flashing storm. The user closes the tab to stop it.
- **Panic hook (separate, recommended).** Today a helper main-thread panic leaves the
  conpty in raw / alt-screen (frozen frame) and logs nothing (the non-blocking
  appender's buffered tail is lost on unwind, and the panic text goes to stderr, i.e.
  the alt-screen). A `std::panic::set_hook` installed early in `main()` that restores
  the terminal + logs the panic + `shutdown_flush()` is **orthogonal** to this recovery
  (recovery works via pipe-disconnect regardless of *why* the helper died) but is the
  only way to learn *which* line panicked. Tracked as a diagnostics follow-up.

### 8.6 Status

| Item | State |
|---|---|
| Detection (exit/kill) via `serve_helper` pipe disconnect | **reuse existing — no new code** |
| `restart_agent_pane` SendEvent + helper `owner_tab_id` registration | **done (Rust)** |
| `SendEventRoute::RestartAgentPane` + dispatch + `OnAgentPaneRestartRequested` | **done (C++)** |
| History resume via `--initial-load-session-id` | **reuse existing flag** |
| Idempotency vs `closeOnExit:always` + user-close suppression | **done (C++ `_agentPaneRestartSuppression`)** |
| Wedge heartbeat / backoff / banner | **deferred (§8.5)** |
| Panic hook (diagnostics) | **separate follow-up (§8.5)** |
