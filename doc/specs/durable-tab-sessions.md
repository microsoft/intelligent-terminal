---
author: Intelligent Terminal
created on: 2026-08-04
last updated: 2026-08-19
issue id: durable-sessions (bucket 1)
---

# Durable tab sessions

## Abstract

Closing a tab throws away everything the user had arranged in it: the pane
layout, each pane's profile and working directory, the scrollback, and any agent
CLI conversation that was running inside it. Restarting Terminal — deliberately
or after a crash — throws away the same thing for every tab at once.

Durable tab sessions save that arrangement so it can be brought back. A saved
session is a row in a master-owned SQLite database plus, optionally, one
scrollback sidecar per pane. It survives the tab closing, the window closing,
Terminal exiting, and Terminal crashing, because nothing about it lives in the
Terminal process.

This is deliberately a *snapshot*, not a live process: the shells themselves are
gone, and restoring one starts fresh shells in the same places with the same
history on screen. Keeping the shells alive is a separate feature and is not
described here.

## Storage

`wta-master` owns the store; Terminal never opens the database directly.

| What | Where |
| --- | --- |
| Session rows | `durable-tab-sessions.db` under the runtime state directory |
| Scrollback sidecars | `durable-tab-sessions/` under the same directory |

Both paths resolve through `runtime_paths.rs`, so a packaged build keeps them
package-private and an unpackaged dev build falls back to
`%LOCALAPPDATA%\IntelligentTerminal`.

A row holds the tab's name, its serialized `WindowLayout` (the same action list
Terminal already uses for persisted layouts), the owning elevation scope, and a
monotonically increasing revision. Elevated and unelevated sessions are stored
in one database but never mix: every request carries the caller's elevation and
the store scopes reads and writes by it.

Master exposes the store over ACP as `_intellterm.wta/durable_tab_sessions/*`:

| Method | Used by |
| --- | --- |
| `save` | Terminal, when a tab or window closes |
| `list` | the `/tab-history` view, `wtcli list-tab-sessions` |
| `get` | Terminal, when restoring one session |
| `delete` | the `/tab-history` view |

Maintenance runs once a day: rows past their retention window are deleted, and
buffer sidecars that no live row references are removed with them.

## Saving

`TerminalPage::_PersistDurableTabSession` runs from `_HandleCloseTabRequested`, and
`TerminalPage::CloseWindow` runs the same per-tab save before raising
`CloseWindowRequested` — closing the window is the case that matters most, and
it never goes through the tab-close path.

Two settings-driven gates decide what is written. `GetDurableTabSessionCloseActions`
maps the existing **Settings → Startup → "When Terminal starts"** preference:

| `firstWindowPreference` | Saved |
| --- | --- |
| `defaultProfile` | nothing |
| `persistedWindowLayout` | layout, cwd, profiles, agent bindings |
| `persistedWindowLayoutAndContent` | the above plus scrollback sidecars |

`ShouldPersistDurableTabSession` then requires the tab to be worth saving at all: it
must have received user input, already own a durable id, or have a resumable
agent session bound to one of its panes. A tab that only ever ran its profile's
startup commands is not a session the user built.

Scrollback is written to a staging directory first and only committed once the
`save` response confirms the row, so a failed save leaves no orphan sidecars.

### Revisions and forks

A tab remembers the `id` and `revision` it last saved under. The store refuses
to overwrite a row whose revision has moved on and forks a new row instead,
returning `forked: true`. That is what keeps two windows holding stale copies of
the same tab from silently clobbering each other; the fork is visible in the
list rather than lost.

### Agent bindings

A pane that is running an agent CLI records the ACP session id, the agent, and
the command that resumes it. The binding arrives either from the agent hooks or,
when no hook is installed, from wta's own `pane_agent_session_changed` event.
`_paneAgentSessions` holds the most recent binding per pane and keeps it after
the CLI exits, so a save that happens later can still describe how to resume it.

The agent pane itself is recorded separately — its ACP session, view, open
state, and position — so a restored tab comes back with the same assistant
conversation, not just the same shells.

## Restoring

There are two entry points, and they converge on the same actions.

**Startup.** `TerminalWindow` already replays `WindowLayout::TabLayout()` for
both "Restore window layout" modes. `SetPersistedLayoutAgentRestorePaths` walks
those actions and points each shell pane at its `buffer_{guid}.txt` sidecar,
skipping panes whose agent session belongs to an agent pane (that history is
replayed through ACP instead, and replaying both would double it).

**On demand.** `/tab-history` lists saved sessions; Enter on a row calls
`wtcli restore-tab-session`, which reaches `TerminalPage::RestoreProtocolDurableTabSession`
through the COM protocol server. That handler fetches the row, rebuilds the
action list, and attaches each pane's sidecar path.

Restoring is idempotent across windows: the protocol first looks for a tab whose
durable id matches, and focuses that tab instead of opening a second copy. The
picker also allows only one Enter request in flight at a time.

Every restored pane gets a fresh `SessionId` — it really is a new shell — and
`DurableTabSessionBufferPath` tells `ControlCore::RestoreFromPath` to seed its buffer
from the sidecar. `_AddTabIdentityMetadata` stamps the durable id back onto the
first pane's args so the restored tab keeps its identity across a later save or
a move to another window.

Agent panes are restored through `_pendingDurableAgentPaneRestores`: the pane is
rebuilt with its recorded view and position, and its conversation comes back
through a boot-time ACP `session/load` rather than from saved terminal output.

## Capabilities and limitations

* A saved session survives a crash, a reboot, and an MSIX update, because it is
  only files on disk.
* It does **not** preserve running processes. A build that was halfway through
  when the tab closed is not halfway through when it comes back.
* Scrollback is only captured at close time, and only when
  "Restore window layout and content" is selected. There is no periodic write.
* Pane split ratios are preserved through the serialized layout; panes that
  fail to restore are skipped rather than replaced with a blank pane.
* Elevated and unelevated sessions never appear in each other's lists.

## Future considerations

* Keeping opted-in shells alive across a close, so a restore reattaches to the
  original process instead of replaying a snapshot.
* Periodic scrollback capture, so a crash loses less than the last close.
