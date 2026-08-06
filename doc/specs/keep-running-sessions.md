---
author: Intelligent Terminal
created on: 2026-08-04
last updated: 2026-08-04
issue id: durable-sessions (bucket 2)
---

# Keep-running shell sessions

## Abstract

Durable Sessions has two independent halves. Bucket 1 — *saving sessions for
restore* — is implemented: closing a tab persists its layout, cwd and scrollback,
and restoring re-creates the tab and replays the saved buffer into a **fresh**
shell. This spec covers bucket 2 — *keeping sessions running*. With
`continueRunningCommands` enabled, closing a tab no longer kills the shell. Its
content is detached with no window, the terminal process stays alive with nothing
on screen, and restoring the session reattaches to that same still-running shell.
A build that was halfway through when the last window closed is still halfway
through when the terminal comes back.

## Background: what the terminal already does with ConPTYs

Two existing mechanisms get confused with each other. Which is which is what
determines the design.

### Moving a pane between windows is *not* handle transfer

Since the `WindowEmperor` refactor, every Windows Terminal window lives in one
process. Moving a tab or pane between windows therefore never crosses a process
boundary:

* `TerminalPage::_MoveContent` serializes the move as actions and raises
  `RequestMoveContent`.
* `ContentManager::Detach` calls `TermControl::Detach`, which severs only the
  XAML control and leaves the `ControlInteractivity` — and with it `ControlCore`
  → `Terminal` → `ConptyConnection` — detached in `ContentManager::_content`,
  keyed by a plain `uint64_t` `ContentId`.
* The destination window calls `TerminalPage::AttachContent` →
  `_AttachControlToContent(contentId)` and gets **the same object** back.

No handle is duplicated. Crucially, `AppLogic::_contentManager` is a single
instance owned by `AppLogic`, i.e. one per *process*, not one per window — so
detached content already outlives any individual window.

### The default-terminal handoff *is* a real cross-process ConPTY transfer

`conhost` hands a live console session, with its client process already running,
to Windows Terminal over classic COM: `ITerminalHandoff3::EstablishPtyHandoff`
marshals the signal pipe, a ConDrv `\Reference` handle and the process handles
with MIDL's `system_handle`, and `ConptyConnection::InitializeFromHandoff`
reassembles a working `HPCON` with `ConptyPackPseudoConsole`.

So a running ConPTY *can* change owning process, and an earlier draft of this
feature did exactly that, handing ptys to a custodian process. **We do not need
it.** The cheaper observation is that a ConPTY does not have to go anywhere at
all if the process that owns it simply declines to exit.

## Solution design

### Keep the process, not the handles

Three pieces of existing infrastructure make a windowless terminal work, and all
three predate this feature:

1. **The emperor already tolerates having no windows.**
   `WindowEmperor::_postQuitMessageIfNeeded` only posts `WM_QUIT` when there are
   no windows *and* no message boxes *and* `compatibility.allowHeadless` is off.
   Headless is a supported, settings-exposed state.
2. **Re-launching finds the live process regardless of windows.**
   `acquireMutexOrAttemptHandoff` claims a named mutex and, if it loses, sends
   the new commandline to the existing instance over `WM_COPYDATA`. The receiving
   window is the emperor's own message-only window, which exists whether or not
   any terminal window does.
3. **Content can already live outside a window**, per `ContentManager` above.

So keep-running is: detach the content instead of destroying it, and teach the
emperor that detached content is a reason to stay alive.

### What a detached session is

`ContentManager::DetachForKeepRunning(...)` detaches the control and records
`sessionId → ContentId`, plus the detached tab's committed durable
`shell_sessions` id/revision on the kept-group metadata. The detached
`ControlInteractivity` keeps:

* its `ConptyConnection`, whose output thread keeps reading the pty — so nothing
  ever stalls on a full pipe buffer;
* its `Terminal`, so scrollback, shell-integration marks and search state keep
  accumulating exactly as if the pane were merely off-screen.

Keying on the connection's **session GUID** rather than the `ContentId` is
deliberate: the GUID is what the durable session record is already persisted
under, and unlike a `ContentId` it stays meaningful in a record that outlives the
process. A record whose session is not detached — the terminal was restarted, or
the shell exited — simply misses and falls back to bucket 1's snapshot restore.

### Process lifetime

`_postQuitMessageIfNeeded` gains `&& !_hasKeptSessions()`. Two related rules
follow:

* `WM_CLOSE_TERMINAL_WINDOW` normally keeps the *last* window alive so its layout
  can be persisted on exit. With kept sessions we want to reach the headless
  state, so that rule now also stands down when sessions are detached.
* `ContentManager` raises `KeptSessionsChanged`, which the emperor bounces
  through its message queue as `WM_KEPT_SESSIONS_CHANGED` rather than acting on
  it inside a content callback. When the last detached shell exits, that is what
  finally lets the process quit.

### Visibility

A terminal with no windows is otherwise invisible and unquittable, so
`_checkWindowsForNotificationIcon` now also shows the notification-area icon
whenever sessions are detached, and clicking "Focus Terminal" with no windows
reattaches every live group directly from the process-wide `ContentManager`.
The notification-area menu also lists each detached tab separately, with
**Restore** and **Close** actions.

### Ordering with bucket 1

Saving a snapshot and keeping a process alive are independent decisions.
`restoreShellSessions` controls the database snapshot; `continueRunningCommands`
controls detachment. When both are enabled, a successful save supplies the
durable database id and revision stored beside the detached group. If saving is
disabled or fails, a qualifying live shell can still remain detached, but it has
no new snapshot id to display or fall back to.

`_PersistShellSession` is normally reached from `_HandleCloseTabRequested`, which
a window close never goes through. Since "close the terminal" is the whole point
of the feature, `TerminalPage::CloseWindow` now runs the same per-tab save-and-detach
before raising `CloseWindowRequested`.

On restore, `_MakeTerminalPane` tries a reattach first. On a hit it attaches the
existing content and returns immediately: no connection is created and
`RestoreFromPath` is skipped, because the live buffer already *is* the history.
On a miss it takes the ordinary path.

The reattach is keyed on the session GUID, and the two restore paths disagree
about who owns it, so the take has to be the arbiter.

A reattached pane's identity is not a choice we get to make: `ConptyConnection`
keeps its `_sessionId`, and `WT_SESSION` was baked into the shell's environment
block when it launched and can never be changed. Everything that reads the
*connection* — the protocol `pane_id`, the durable record's `pane_key`, hooks
using `WT_SESSION` to tell `wtcli` which pane they came from — therefore reports
the original id regardless. Things that read the *args* instead — the
`buffer_{guid}.txt` sidecars, `RestoreFromPath` — would disagree with it if the
restored pane carried a different id.

Restoring from a snapshot still mints a fresh id unconditionally, exactly as
before, because that really is a brand new shell. The `/shell-sessions` handler
additionally passes the original id along in `KeptSessionId`, and
`_MakeTerminalPane` calls `TryReattachKeptSession` on it. That call is the single
arbiter: it is an atomic take, so two restores of the same record cannot both
believe they own the session — the loser falls through and uses the fresh id for
a genuinely new shell. Deciding this earlier, while building the actions, would
leave exactly that window open, and a duplicated id means two panes sharing one
buffer sidecar, one `pane_id` and one agent binding.

### Getting back in

Launching the terminal again does not start a new process — the named-mutex
handoff delivers the commandline to the headless one. A bare launch snapshots
the current detached group ids, atomically takes each group, and rebuilds tabs
from their live `ContentId` values. This deliberately does not use
`PersistedWindowLayouts`: those layouts are controlled by the user's
`firstWindowPreference` and become stale after a partial restore or discard.

Groups from several former windows may be restored into one available window.
The original window topology and pane split ratios are not retained; panes in a
multi-pane group are currently rebuilt with equal splits.

## Settings

`continueRunningCommands` (global, default `true`,
Settings → Startup → "Continue running commands when terminal relaunches"). The
setting was already defined and surfaced in the Settings UI by bucket 1 but had
no consumer; this is that consumer. It controls future detach decisions only.
Turning it off does not prevent an already-detached live session from being
reattached.

## Capabilities and limitations

* A kept session survives its tab closing, its window closing, and every window
  closing. It does **not** survive the terminal process dying — a crash, an MSIX
  update, sign-out or reboot take the shells with them. That is the deliberate
  trade for not maintaining a cross-process handle-custody protocol. The
  alternative, a separate custodian process, survives a terminal crash but costs
  a second process, a wire format, and a raw byte ring buffer in place of a real
  terminal buffer.
* Detached sessions hold their whole `ControlCore` and buffer in memory. That is
  heavier than a raw output ring, and is what buys full scrollback, marks and
  search on reattach.
* Only qualifying sessions are detached: a shell must have received user input
  or already belong to a durable record. Blank or accidental tabs remain
  excluded.
* Restoring several detached tabs can coalesce them into one window. Multi-pane
  tabs retain their live panes but currently use equal split sizes.
* Agent panes are excluded — their durability is the agent CLI's own resume
  mechanism.

## Future considerations

* **Per-session opt-in.** The Durable Sessions spec asks for marking an
  individual session to keep running, with a badge on the pane. Today the setting
  is global. The plumbing is per-pane already, so this is a UI change plus a flag
  consulted in `_DetachShellPanesForKeepRunning`.
* **Releasing the renderer while detached.** Detached content does not need its
  renderer; dropping it would cut the memory cost of a long-detached session.
