---
author: Performance
created_on: 2026-09-03
last_updated: 2026-09-03
issue_id: n/a
---

# Agent pane latency: profiling and optimizations

## Summary

A performance pass over the agent pane's startup and prompt paths, profiling
where wall-clock time actually goes between launching a pane and getting a
response back. This records the hot spots that profiling surfaced, the
optimizations applied to each, and the measured result.

Two changes account for essentially all of the win:

| Scenario | Before | After | Win |
| --- | --- | --- | --- |
| Agent pane: self-discovery on startup | 0.27 s | **0.002 s** | −0.27 s, every pane |
| Agent pane: helper → master pipe connected | 0.39 s | **0.15 s** | −0.24 s, every pane |
| Agent pane: helper → session usable | 3.29 s | **3.04 s** | −0.25 s, every pane |
| Autofix turn: work before the prompt is sent | 4.02 s | **0.39 s** | **−3.6 s** |

All figures are medians of repeated runs on the same machine, comparing the
**same commit and same build configuration** with only `wta.exe` swapped, so the
deltas are attributable to these changes and nothing else. Method and caveats are
in [Measurement](#measurement).

---

## Change 1 — Delete `discover_pane_identity`

**File:** `tools/wta/src/helper/runtime.rs`

### What it did

On every agent pane launch, the helper worked out its own pane identity by
enumerating Windows Terminal:

```rust
let windows = shell_mgr.wt_list_windows().await.ok()?;      // 1 process
for win in windows_arr {
    let tabs = shell_mgr.wt_list_tabs(&window_id).await.ok()?;      // + 1 per window
    for tab in tabs_arr {
        let panes = shell_mgr.wt_list_panes(&tab_id, ...).await.ok()?;  // + 1 per tab
        // ... match panes by our own PID
```

Every one of those calls spawns a **separate `wtcli.exe` process** plus an
out-of-proc COM activation (`cli_channel.rs`, `request()` → `run_wtcli_one_shot`).
So the cost was `1 + windows + tabs` process launches — it grew with how many
tabs the user had open — and it ran *before* the TUI entered the alt-screen, so
the pane was blank for the whole duration.

### Why it was safe to delete

All three values it returned were already available for free, and the code
itself was throwing two of them away:

```rust
if let Some((pane_id, _tab_id, window_id)) = pane_identity {
    app_state.pane_id = Some(pane_id);
    // discover_pane_identity returns the legacy unstable tab
    // index, not the GUID — ignore it. ...
    app_state.window_id = Some(window_id);
}
else if let Some(pane_id) = std::env::var("WT_SESSION") ... {
    app_state.pane_id = Some(pane_id);          // identical value, free
}

if let Some(owner_window_id) = config.owner_window_id... {
    app_state.window_id = Some(owner_window_id.to_string());  // overwrites it anyway
}
```

- `tab_id` — discarded on purpose; the real one comes from `--owner-tab-id`.
- `window_id` — unconditionally overwritten by `--owner-window-id` a few lines later.
- `pane_id` — `WT_SESSION` already holds it. `ConptyConnection.cpp` sets it on
  every pane it spawns, and the `else` branch already read it.

The fix promotes the `WT_SESSION` branch to the primary path and deletes the
enumeration. Behaviour is identical.

**Supporting evidence:** profiling the current shipping build shows this
enumeration spending ~172 ms and then not matching, logging
`seeded app_state.pane_id from WT_SESSION fallback` — it pays the full cost and
still resolves via the free value.

### Impact

| Metric | Before | After |
| --- | --- | --- |
| Self-discovery | 0.248 / 0.268 / 0.281 s | **0.002 / 0.002 / 0.003 s** |
| Helper start → pipe connected | 0.365 / 0.394 / 0.400 s | **0.143 / 0.147 / 0.149 s** |
| Helper start → session usable | 3.245 / 3.290 / 3.562 s | **2.994 / 3.037 / 3.087 s** |

Scales with tab count: the more tabs a user has open, the more this cost.

---

## Change 2 — Stop blocking the prompt on the PowerShell command enumerate

**Files:** `tools/wta/src/command_recall.rs`,
`tools/wta/src/protocol/acp/prompt_context.rs`

### What it did

`CommandNotFoundProvider` adds a "did you mean…" hint when a failed command
doesn't exist. To do that it enumerates the shell's commands — which **loads the
user's interactive PowerShell profile**, bounded at 4 s, and on timeout falls
back to a *second* `-NoProfile` subprocess.

That ran **inline, before the prompt was sent to the agent**. Measured directly
on the test machine:

| Subprocess | Cost |
| --- | --- |
| Profile-loading enumerate | 6,175 ms (exceeds its own 4 s bound → killed) |
| `-NoProfile` fallback enumerate | 5,397 ms |

A real turn from the logs before the fix:
```
bysis aphase=context_provider  id=command_not_found  dt=5.820s
phase=context_ready     context_build=6.231s
phase=first_text        since_submit=14.341s
```

**6.2 of the 14.3 seconds to first token was spent before the agent received
anything.** The result is cached, but per helper — so every new agent pane paid
it again on its first autofix turn.

### The fix

A near-match hint is a best-effort enhancement, so it must never gate the
response. `powershell_near_matches_now` returns a result **only if the cache is
already warm**; on a miss it returns `None` and kicks off a single-flight
background enumerate. Worst case is now "no hint on the first autofix turn"
instead of a multi-second stall.

The warm is deliberately **lazy** — started by the lookup itself on a miss, not
from context resolution. Context resolution runs on every turn including planner
turns, where the hint is never used, so warming there would spawn a ~3 s
PowerShell that competes with the foreground prompt for no benefit. Misses only
happen on autofix turns, which is exactly where the hint is consumed.

### Impact

Identical builds, same forced not-found command:

| Metric | Before | After |
| --- | --- | --- |
| `command_not_found` provider | 3.679 s | **0.009 s** |
| Total context build | 4.017 s | **0.381 s** |
| Elapsed before prompt is sent | 4.020 s | **0.385 s** |

(The pre-fix run spent 3.679 s and still returned `present=false` — it paid the
full enumerate and produced no hint.)

---

## Change 3 — Don't smooth the first reveal of a turn

**File:** `tools/wta/src/app.rs`

Included for completeness; smaller than the two above and **not benchmarked**.

The typewriter animation revealed `max(3, backlog/4)` chars per 33 ms frame, so
the opening characters of a response trailed their arrival by up to 4 frames
(~130 ms). Time-to-first-token is the most visible latency in a turn, so the
first reveal is now un-throttled and only the continuation is paced. Total
response time is unchanged either way.

---

## Measurement

The numbers in this document are medians of **12 cold starts and 24 live prompts**
against Copilot, driven through the Windows Terminal COM protocol rather than by
hand. Timings come from instrumentation that already exists: helper startup
milestones and the `prompt_timing` phases in `turn_metrics.rs` (`WTA_LOG=debug`).
To reproduce or extend this, see [How to verify a change](#how-to-verify-a-change).

### Caveats — read these before quoting numbers

1. **Do not benchmark against the shipping public build for attribution.** It is
   an optimized *Release* build of a much older codebase (0.2.2395.0) and differs
   by far more than these changes. Every table above compares the *same commit
   and same Debug configuration* with only `wta.exe` swapped. For reference only,
   public build medians were: self-discovery 0.19 s, pipe connect 0.28 s, session
   usable 3.31 s.
2. **Time-to-first-token is model/network time** (measured spread 4.8–11.5 s).
   None of these changes affect it and it should not be read as a result.
3. These are Debug-build numbers. Release will be faster across the board; the
   *deltas* are what this document claims.

---

## Deliberately not done

| Candidate | Why not |
| --- | --- |
| Make `list_sessions` concurrent with `session/new` | Measured at **~1 ms**. Would risk the documented stale-snapshot ordering race for no gain. |
| Pre-spawn the default agent CLI at master startup | Unsafe as specified. The pool key (`agent_cmd_key_with_provider`) folds in a `provider_binding` the master cannot know at startup — the helper declares it during `initialize`. Pre-spawning under a guessed key would **add a second agent process** rather than warm the pool. |

---

## Remaining work

Four items, ranked by expected value. Each is independent — take them in any
order. Estimates are from the measurements in this document.

### W1 — Replace per-call `wtcli.exe` spawns with a persistent channel

**Expected win:** ~0.15–0.9 s per prompt, plus startup. Largest remaining item.

**Problem.** `CliChannel::request` (`tools/wta/src/shell/wt_channel/cli_channel.rs`)
maps every protocol method onto a one-shot `wtcli.exe` invocation via
`run_wtcli_one_shot`. Each call is a process creation plus an out-of-proc COM
activation, measured at **66–94 ms**. The prompt path alone makes 2–3 of these
per turn (`get_active_pane`, `read_pane_output`, and `resolve_pane_by_session_id`
when a source pane is known — the last one enumerates, so it costs
`1 + windows + tabs` spawns).

**Do this.** A durable child already exists: `start_reader` runs
`wtcli listen --json` for the lifetime of the helper. Extend that pattern into a
request/response channel — one long-lived connection carrying correlated
request IDs — and route `request()` through it, keeping the one-shot path only as
a fallback when the persistent channel is unavailable.

**Acceptance.** Per-turn overhead (`prompt_timing … phase=prompt_sent`) drops
below ~0.10 s median. No change to `wtcli`'s own CLI behaviour, which is a
supported user-facing surface.

**Watch out for.** `wtcli` is also a user-facing command; do not regress its
standalone semantics. Preserve the existing timeout/reap behaviour
(`WTCLI_ONE_SHOT_TIMEOUT`) so a hung Terminal cannot wedge a pane.

### W2 — Paint the TUI before `session/new` completes

**Expected win:** 0.8–2.5 s of *perceived* startup on every pane.

**Problem.** `session/new` accounts for most of the remaining time to a usable
pane (~1.8 s of the ~3.0 s total). The pane shows "Creating session…" for that
entire window, so it reads as the agent being slow to attach.

**Do this.** The TUI needs no session id to render its frame, history, or input
box. Render immediately and accept typing while `session/new` is in flight, then
attach the session when it resolves. `dispatch_prompt_body`
(`tools/wta/src/protocol/acp/client.rs`) already contains a lazy
create-on-first-prompt path, so a prompt submitted before the bootstrap session
exists has a defined outcome.

**Acceptance.** First paint is independent of `session/new`; a prompt typed
during startup is not lost.

**Watch out for.** Two creation paths racing. The bootstrap `session/new` and the
lazy create must not both produce a session for the same tab — that was
previously a duplicate-row bug in the sessions view.

### W3 — Diagnose duplicate agent CLI spawns

**Expected win:** avoids repeated ~3.7 s agent initializes; also cuts memory.

**Problem.** In one 8-second window the master spawned `copilot.exe` **5 times**
and completed `initialize` **4 times**, all for the same
`agent_cmd=copilot --acp --stdio`, `agent_source=host`. A pooled CLI was also
reaped 1.5 ms after a helper disconnected, suggesting a pooled process's lifetime
is coupled to a departing helper rather than to remaining pool references.

**Do this — diagnosis first.** The "agent CLI spawned" log line does not include
the pool key, so it is currently impossible to tell from logs whether duplicates
share a key. Add `agent_cmd_key_with_provider(...)` to that line
(`tools/wta/src/master/mod.rs`), reproduce, then confirm which of these is
happening:

- **Different keys.** The key folds in `provider_binding.pool_key()` (`native` /
  `legacy` / `<selection>@<generation>`), so two helpers resolving different
  bindings for the same agent get separate CLIs.
- **Respawn after reap.** `acquire_and_bind_agent` loops `acquire()` when
  `bind_helper_to_agent` fails; once a reap has removed the key, the retry
  creates a fresh cell and a fresh process.

Fix whichever it turns out to be. Do not "fix" this by pre-spawning at startup —
see [Deliberately not done](#deliberately-not-done).

**Acceptance.** One agent CLI process per distinct pool key across a session;
helper disconnect never reaps a CLI another helper is still bound to.

### W4 — Investigate helper connect/exit churn

**Expected win:** less CPU contention during the window the user is waiting on.

**Problem.** 8 helpers connected in 8 s and six of them exited within ~20–30 ms.
Each is a full `wta.exe` process spawn plus a pipe handshake, competing with the
startup the user is actually waiting for.

**Do this.** Establish where the short-lived helpers come from — pre-warm firing
per tab (`_InitializeTab` low-priority callback and
`_PrewarmAgentPanesAfterStartup` in `src/cascadia/TerminalApp/`) is the first
place to look — and suppress the ones that are immediately discarded.

**Acceptance.** Helper count over a cold start matches the number of tabs that
actually keep an agent pane.

---

## How to verify a change

There is no committed harness; the following is enough to build one.

**Instrumentation already exists — turn it on.** Set `WTA_LOG=debug` at *User*
scope (a packaged app does not inherit the calling shell's environment), then
read:

- **Connection phases** from the helper log milestones:
  `wta-helper starting` → `Connected to WT COM` → `start_reader: starting` →
  `master pipe connected` → `ACP initialized over master pipe` →
  `Session created (over pipe)`.
- **Per-turn phases** from the `prompt_timing` records emitted by
  `tools/wta/src/protocol/acp/turn_metrics.rs`: `prompt_received`,
  `context_provider` (one per provider, with `dt=`), `context_ready`,
  `prompt_sent`, `first_event`, `first_text`.

Logs live under
`Packages<PFN>\LocalCache\Local\IntelligentTerminal\logs<version>\`.

**Driving prompts without a human.** The helper log prints the COM CLSID in its
`get_capabilities` response, and the agent pane's id appears as
`pane_session_id=…`. Setting `WT_COM_CLSID` from the former lets
`wtcli send-keys -t <pane> --raw <text>` (then `Enter`) submit a real prompt —
this works even while the pane is stashed, so no UI automation is needed.

**Two rules for reporting numbers.**

1. **Hold the build configuration fixed and swap only `wta.exe`.** Comparing
   against a different configuration — or against the shipping Release package,
   which is also an older codebase — measures that difference too, not your
   change.
2. **Report medians over several cold starts, and never quote `first_text` as a
   latency result.** It is dominated by model/network time; the observed spread
   was 4.8–11.5 s. The client-side number that reflects our work is
   `phase=prompt_sent` (everything before the agent receives the prompt).
