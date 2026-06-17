# Per-CLI history filtering

## Summary

The agent **session management view** only ever displays sessions for the
**currently configured agent CLI** (Copilot / Claude / Gemini / Codex). Yet
both processes that scan on-disk session history —
`wta-master` and every per-tab `wta-helper` — load **all four CLIs** every
time. This spec narrows the history scan to the current CLI and removes the
helper's redundant local scan entirely, so the whole WT process performs
**one** filtered history scan (in master) instead of `N+1` full scans
(`N` helpers + master).

## Motivation

`history_loader::load_all()` opens and parses every session transcript on
disk for all four CLIs. After the two-phase optimization
(`per-CLI cap by mtime, skip Class A`) this is ~940 ms on a populated
machine; before it was ~3.5 s. That cost is paid:

- **once per `wta-master`** at startup (seeds the registry), and
- **once per `wta-helper`** — eagerly, at helper startup
  (`ensure_history_loaded` → `load_all`), for **every pre-warmed tab**, even
  tabs whose session view the user never opens.

Three facts make most of that work wasteful:

1. The view filters to the current CLI (`current_cli_filter()` =
   `CliSource::from_agent_id(current_agent_id)`), so 3 of the 4 CLIs scanned
   are never shown.
2. The view renders from **master's snapshot** (a `session/list` reply
   issued fresh every time the view opens), **not** from the helper's local
   registry. The helper's local scan result is shadowed.
3. Switching the agent in Settings **restarts** master + all helpers, so a
   fresh process re-scans automatically — no runtime "agent switched" signal
   exists or is needed.

## Background: how the session view gets its data

There are two stores in a helper:

| Store | Populated by | Role |
|---|---|---|
| local registry `App::agent_sessions` | helper's own `load_all` (`HistoricalSessionsLoaded` → `merge_historical`) + live `apply(...)` events | fallback / live-session tracker |
| per-tab `agents_view.snapshot` | master's `session/list` reply (`AgentsSnapshotLoaded`) | **the rendered source** |

`agents_view::render` and `agents_rows_for_tab` use the snapshot when it is
`Some`, and fall back to the local registry only when it is `None`
(`app.rs` `agents_rows_for_tab`).

Snapshot lifecycle:

- **open view** (`open_agents_view_for_tab`) → sets `snapshot = Some(vec![])`
  and fires a `session/list` refetch to master.
- **master replies** → `snapshot = Some(rows)`.
- **close view** → `snapshot = None` (but then `current_view != Agents`, so
  the agents view is not rendered at all).
- **refetch fails / times out** (`handle_agents_snapshot_failed`) → snapshot
  is **left untouched** ("rendered rows stay on the last good data instead of
  flashing empty").

Consequence: whenever the agents view is on screen, `snapshot` is always
`Some` (a loading placeholder, the last good rows, or fresh master rows). The
local-registry `else` branch is effectively never rendered. The local scan
therefore drives **display** for nothing; its only live effects are:

- the instant-open cursor pre-selection (see *Consumer audit*), and
- being the upgrade target for `apply_alive_session_join` (Historical → Live).

Both are non-critical, because master's snapshot already carries liveness and
seeds selection on arrival.

### Why live / watcher sessions are unaffected

Filtering the history scan only drops **historical** (on-disk, dead) rows of
other CLIs. Live rows do not come from `load_all`:

- Sessions created through master are all the current CLI (master spawns a
  single agent CLI).
- The hookless **watcher** discovers Class B shell sessions **machine-wide /
  cross-CLI** (`master/mod.rs` "the file watcher sees session files
  machine-wide"). Those are narrowed to the current CLI by the **view filter**
  (`cli_filter` retain), not by `load_all`.

So the existing view filter remains the mechanism that keeps live/watcher
cross-CLI sessions out of the display. This spec does not touch it.

## Design

### 1. Master side — filter the authoritative scan (primary lever)

Add a filtered entry point to `history_loader`:

```rust
/// Scan on-disk history for a single CLI, or all CLIs when `None`.
pub fn load_for_cli(cli_filter: Option<&CliSource>) -> Vec<AgentSession>;
```

Dispatch on the filter:

| `cli_filter` | scans |
|---|---|
| `Some(Copilot)` / `Claude` / `Gemini` / `Codex` | that one CLI's loader |
| `None` or `Some(Unknown(_))` (custom / unrecognized agent) | all four (current behavior) |

The "scan everything" path is `load_for_cli(None)`, reached via master when
the agent is custom / unrecognized. The dispatch decision lives in a small
testable helper, `cli_scan_flags`. (The old unconditional `load_all()` is
removed — every caller now passes a filter.) The two-phase scan internals
(per-CLI cap by mtime, Class A skip) are unchanged.

Master passes its already-resolved CLI:

- `master/mod.rs` history-seed task → `load_for_cli(inner.cli_source.as_ref())`
  (`cli_source` is resolved once at startup from the `--agent` arg via
  `resolve_agent_id_from_cmd` → `CliSource::from_agent_id`).

Master's registry, its `sessions/changed` broadcasts, and the `session/list`
snapshot the view renders are now scoped to the current CLI.

### 2. Helper side — remove the redundant local scan

The helper no longer scans history. Remove:

- the eager `ensure_history_loaded()` calls at helper startup
  (`main.rs` ACP TUI path) and the lazy calls from the `/sessions` open path,
- `ensure_history_loaded` itself,
- the `AppEvent::HistoricalSessionsLoaded` variant, its handler, and the
  `merge_historical` call site. `merge_historical` is now used only by registry
  tests (seeding Historical rows to exercise the still-live alive-join logic),
  so it is gated `#[cfg(test)]`. The `HistoryLoadState` enum / field and the
  unconditional `load_all()` are removed; the loading-shimmer animation now
  keys off `App::agents_view_awaiting_snapshot()` (open agents view + empty
  placeholder snapshot + in-flight refetch) instead of the old scan state.

The helper's `agent_sessions` registry continues to exist and is populated by
live `apply(...)` events (SessionStarted hooks, new_session, pane events) and
`apply_alive_session_join`. It now holds only **live** rows this helper learns
about — used by pane lookups (`key_for_pane`, `origin_for_pane`). Display
comes from master's snapshot.

### 3. Re-load on agent switch — automatic

Changing `acpAgent` in Settings runs
`_AgentSettingsChanged → _RebuildAgentStack → SharedWta::Restart` (kill +
respawn master with the new `--agent`) and tears down + re-warms all helper
agent panes. The new master re-runs `load_for_cli` with the new CLI; helpers
have no scan to redo. No runtime "switch" message is added.

## Consumer audit (local registry with no historical rows)

| Consumer | Behavior after removal |
|---|---|
| Display render (`agents_view::render`) | Safe — renders from master snapshot; local-registry branch was already unreachable while the view is visible. |
| Enter / resume / delete dispatch | Safe — operate on `agents_rows_for_tab`, which uses the snapshot while the view is open. |
| `key_for_pane` / `origin_for_pane` | Safe — live-only pane lookups, never depended on historical rows. |
| Instant-open cursor pre-selection (`open_agents_view_for_tab`, the `rows_available` check) | Degrades gracefully — local registry is empty at that instant so no instant pre-select; `restore_agents_selection` seeds row 0 when master's snapshot arrives a moment later. Cosmetic. |
| `apply_alive_session_join` | No-op without historical rows to upgrade; display liveness comes from the snapshot regardless. |
| Optimistic resume flip on a dead historical row (`apply(resume_event)`) | Applies to a row not in the local registry → local no-op; real state arrives on the next snapshot refetch. Cosmetic. |
| `wta sessions list --origin all` (asks master) | Now shows only the current CLI's history (master is filtered). Accepted — debug eye-of-god view narrows to the current agent. |

## Decisions

- **Custom / unrecognized agent** (`CliSource` resolves to `None`): scan all
  four CLIs, matching the view's behavior when `current_cli_filter()` is
  `None` (it shows all CLIs).
- **`wta sessions list --origin all`** narrowing to the current CLI's history
  is accepted; it reads master's (now filtered) registry. Live rows are
  unaffected.
- **Setup / FRE mode** (`current_agent_id` empty before an agent is picked):
  not applicable to the helper scan anymore (the helper no longer scans);
  master is launched with a concrete `--agent` so its filter is always
  resolved.

## Edge case: cold-open latency

Master scans history asynchronously at startup (~520 ms filtered for Copilot,
much less for other CLIs). If the user opens a session view before that scan
completes, the first `session/list` reply returns only the live rows scanned
so far; the loading shimmer shows until master finishes and broadcasts
`sessions/changed`, which triggers a refetch that fills in the historicals.
The window is small because master starts scanning at process startup, well
before a user typically opens the view. No local helper scan is needed to
cover it.

## Performance

| | Before | After |
|---|---|---|
| master history scan | all 4 CLIs (~940 ms) | 1 CLI (Copilot ~520 ms; Gemini ~50 ms) |
| helper history scan | all 4 CLIs, **per pre-warmed tab**, eager | **none** |
| total scans per WT process | `N` helpers + master, full | **1** (master), filtered |
| master registry / broadcast / snapshot size | up to ~200 historical rows | up to ~50 (one CLI) |

## Testing

- `cli_scan_flags(Some(CliSource::Copilot))` selects only the Copilot loader;
  `Some(Gemini)` only Gemini.
- `cli_scan_flags(None)` and `Some(CliSource::Unknown("custom:x"))` select all
  four (the custom / unrecognized-agent path).
- Helper-side: `App::agents_view_awaiting_snapshot()` is true only while the
  agents view is open and waiting on its first `session/list` reply; the
  existing snapshot tests already exercise the (now always empty) local
  registry rendering from the master snapshot.
- Existing two-phase / per-loader history tests are unaffected (they call
  `load_copilot` etc. directly).

## Out of scope

- Changing the view's CLI filter or the MVP origin filter.
- Any runtime (non-restart) agent-switch mechanism.
- Reworking the watcher's machine-wide discovery.
