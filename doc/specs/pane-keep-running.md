# Per-pane keep running

Keep running is an explicit, runtime-only choice for a shell pane that currently
hosts an agent CLI session. It is independent of startup-layout restoration.
The vertical-tab pane menu is a separate UX change.

## UI integration contract

`TerminalPage` exposes APIs keyed by the pane connection's stable `WT_SESSION`
GUID, not the tab index or the pane's mutable tree index:

- `CanKeepPaneRunning(id)` determines whether the choice is available.
- `IsPaneKeepRunning(id)` returns the current choice.
- `SetPaneKeepRunning(id, enabled)` changes it. Enabling an ineligible pane
  fails with `E_ILLEGAL_METHOD_CALL`; an unknown pane or an assistant pane
  fails with `E_INVALIDARG`.

Only observed agent lifecycle hooks establish eligibility. A matching title,
an old resume command, and a session restored from disk are not sufficient.
Shell panes in Connected state qualify; the embedded AI assistant and other
nonterminal content do not. An agent waiting for input or finishing a task
remains eligible. A matching CLI-session end clears eligibility and the choice;
an end event for an older session cannot clear its replacement. Copilot nested
prompt IDs do not replace the owner. A new CLI session starts unselected.

## Closing and restoring

After the existing close confirmation, closing a tab detaches only its selected,
still-eligible shell panes and closes all other panes, including the embedded
assistant. Closing a window applies the same policy to each tab. Explicitly
closing a pane, including the last pane, still terminates that pane.
Undo-close records only panes that actually closed, never another launch of a
CLI still running in the background. Kept panes are restored through the tray.

The process-wide `ContentManager` retains the original content, connection,
session GUID and terminal buffer. Detached panes from one tab form one group
identified by the original stable tab GUID. The shell continues reading and
rendering output into its buffer without a window. Kept groups retain a lease
on an already-running WTA master so hook tracking can continue after the last
assistant closes. Both COM hooks and in-band agent events update the runtime
binding while detached.

An agent CLI exiting back to its shell does **not** discard a detached pane.
That shell remains available until Restore or Close. When the shell/ConPTY
itself exits or fails, only that pane is reaped; the remaining panes and their
tab group survive. Detached terminal-end notifications are emitted once,
without incorrectly reporting a live detachment as a pane closure.

The notification-area icon remains visible while any group exists, including
with zero windows. Each group's submenu has Restore and Close. Restore
reattaches the same live content; Close terminates all panes in that group.
A bare launch or tray activation with no windows restores the kept groups.
No default shell or disk snapshot is launched in place of a headless group.

Restoration claims the group, borrows its content into prepared controls, builds
the new tab, and only then commits ownership. Failed preparation detaches the
borrowed controls and releases the claim for retry. Claimed groups cannot be
restored or discarded a second time and continue keeping the process alive.
Runtime choices follow content during existing tab/pane moves and reattachment.

## Boundaries

- This is in-process headless execution, not a separate daemon. Process crashes,
  forced exit, updates, sign-out and reboot are not survivable.
- Preferences are not saved to settings or persisted layouts.
- Restore retains only the kept panes. Their original split ratios, orientation,
  zoom and focus are not retained; the current backend builds equal-width splits.
- Keep-running menu/chips in the vertical tab rail are not implemented here.
- Missing hooks do not interrupt an agent. They can prevent enabling keep running
  or delay knowledge that a CLI session ended.

Focused coverage lives in `TabTests::KeepRunning*` in
`src/cascadia/LocalTests_TerminalApp/TabTests.cpp`.
