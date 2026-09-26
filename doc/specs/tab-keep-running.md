# Per-tab keep running

Keep running is an explicit, runtime-only choice for an entire terminal tab.
It is available to ordinary shell tabs without an agent CLI or lifecycle hooks,
and is independent of startup-layout restoration.

In vertical layout, right-click a terminal tab and select **Keep tab running**,
the first menu item. Its checkmark reflects that tab's current choice; selecting
it again disables keep running. It targets the clicked tab even when another
tab has focus. The item is absent from horizontal-tab and pane context menus,
and from nonterminal tabs such as Settings. Changing tab orientation does not
reset an existing choice.

The menu item includes a monochrome icon and the tooltip: "Keep this tab
running in the background after closing the tab or window."

## UI integration contract

`TerminalPage` exposes APIs keyed by `Tab::StableId()`, parsed as a GUID, not
the mutable tab index or a pane's `WT_SESSION`:

- `CanKeepTabRunning(id)` is true for an attached tab containing terminal content.
- `IsTabKeepRunning(id)` returns the current choice, including for a kept tab
  owned by that page.
- `SetTabKeepRunning(id, enabled)` changes an attached tab's choice. Unknown or
  already-kept tab IDs fail with `E_INVALIDARG`; enabling a nonterminal tab
  (such as Settings) fails with `E_ILLEGAL_METHOD_CALL`.

The choice belongs to the tab, not individual panes. New splits are included
automatically. Moving a whole tab carries its choice; moving one pane does not
opt its destination tab in. Agent CLI start/end events do not change the choice.

## Closing and restoring

After the existing close confirmation, closing an opted-in tab removes it from
the visible tab strip without shutting down any of its panes. This includes
ordinary shells and the embedded AI assistant, whether visible or stashed.
Closing a window applies this policy independently to each tab; unselected tabs
close normally. Explicit pane close, including the last pane, remains destructive.
Kept tabs are not added to undo-close history.

The process-wide `ContentManager` retains the live tab, its pane tree and its
owning page's event routing. Connections, session GUIDs, terminal buffers and
the assistant's helper/ACP session stay alive; no resume command is launched.
Rendering is hidden while terminal output continues to populate the buffers.
An already-running WTA master retains a keep-running lease. COM status events
and explicit pane reads/input continue routing to kept tabs even with no windows.
Confirmation-requiring actions still wait for the user; keeping a tab does not
bypass the existing session-MCP confirmation path.

Agent CLI exit does not cancel the tab choice or discard the shell. Normal
profile `closeOnExit` behavior continues to apply to each connection. With
`closeOnExit: never`, an exited pane and its output remain part of the layout.
Closing the last pane removes the kept tab; closing one split does not discard
the others.

The notification-area icon remains visible while any kept tab exists, including
with zero windows. Each tab's submenu has Restore and Close. Restore reattaches
the same live content; Close terminates the entire kept tab, including its helper.
A bare launch or tray activation with no windows restores the kept tabs.
No default shell or disk snapshot is launched in place of a headless group.

Restoration claims the tab and reuses the transactional content-transfer path.
It rebuilds the original split orientations, ratios, active pane, zoom, stashed
assistant, title and color around the same live content before committing
ownership. The tab's stable GUID is retained, while WTA updates its owning window.
Failed preparation rolls back without killing the retained content or helper;
the claim is released for retry. A claimed tab cannot be restored or discarded
twice and continues keeping the process alive.

## Boundaries

- This is in-process headless execution, not a separate daemon. Process crashes,
  forced exit, updates, sign-out and reboot are not survivable.
- Preferences are not saved to settings or persisted layouts.
- Keep-running badges and a horizontal-tab menu entry are not implemented here.
- Missing hooks may delay agent status updates but do not gate keeping a tab.

Focused coverage lives in `TabTests::KeepRunning*` in
`src/cascadia/LocalTests_TerminalApp/TabTests.cpp`.
