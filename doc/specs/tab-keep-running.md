# Per-tab keep running

Keep running is an explicit, runtime-only choice for an entire terminal tab.
It is available to ordinary shell tabs without an agent CLI or lifecycle hooks,
and is independent of startup-layout restoration.

In vertical layout, right-click a terminal tab and select **Keep tab running**,
the first menu item. When enabled, that action changes to **Turn off keep running**;
selecting it disables background retention without closing the tab or stopping
its current processes. It targets the clicked tab even when another
tab has focus. The item is absent from horizontal-tab and pane context menus,
and from nonterminal tabs such as Settings. Changing tab orientation does not
reset an existing choice.

The enable action uses the system RepeatAll (`E8EE`) font icon and the tooltip:
"Keep this tab running in the background after closing the tab or window."
The disable action uses the system Cancel (`E711`) cross icon and the tooltip:
"This tab will no longer stay running after you close the tab or window."
The menu shows an action rather than a checked state, and refreshes its label,
icon, tooltip, and accessibility help when the choice changes or the menu opens.
The RepeatAll icon appears after the title of a tab with keep running enabled,
including after restore, whole-tab moves, and switching to horizontal layout.
Disabling keep running removes the title icon. Long titles truncate before the
indicator so it stays visible.
When Rich Tab metadata is visible, the indicator is vertically centered across
the title and metadata rows without changing its horizontal position. Metadata
also truncates before the indicator column.
The menu icons and title indicator use Segoe Fluent Icons with Segoe MDL2 Assets as a fallback,
not a bitmap asset.

**Pin tab** is a separate context-menu action in both tab layouts. It keeps a
terminal tab before unpinned tabs and can be undone with **Unpin tab**. Pinned
tabs still close normally, including with bulk close actions; pinning does not
enable background retention. The order is kept while moving a tab to another
window or restoring it from Keep running in the same process, but is not saved
across application restarts. Settings tabs cannot be pinned.

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
with zero windows. Its menu preserves the original Focus Terminal action and
Windows submenu for selecting an existing window. Kept tabs add their own
Restore and Close submenus without removing inherited tray actions. Restore reattaches
the same live content; Close terminates the entire kept tab, including its helper.
The kept-tab entries and **Close all keep-running tabs** form one section between
native menu separators. The bulk-close command appears immediately below the
last kept tab and closes all currently detached, unclaimed tabs, including their
panes and helpers. Tabs still in a window or already being restored are not
closed. The section is absent when no kept tabs are available.
Tray Restore targets the most recently active terminal window, or creates a
receiver window when none exists.
An ordinary Start-menu or command-line launch follows the original startup
logic, opening a new tab/window without attaching the kept tabs. They remain
detached and available in the tray or Agent history.
Tray activation with no windows restores the kept tabs. No replacement shell
or disk snapshot is launched for a kept group during this explicit recovery.
All requested tabs are queued into one receiver window, sized from the retained
windows. Restoration waits for host registration and a nonzero content layout,
then runs on a later UI turn. A failed tab remains available in the tray and
does not close the receiver before other tabs in the batch can restore.
Explicit profile launches keep their normal new-tab behavior.
Initial and re-armed layout callbacks hold a weak page reference, so a queued
layout notification cannot dereference a page that was destroyed before delivery.

Selecting a live session in Agent history or the agent session list also
reattaches its kept tab and focuses the original pane. The shared `focus-pane`
protocol path restores the entire tab into the most recently active terminal
window, preserving its connections and layout rather than launching a second
CLI session. Sessions in already-attached tabs continue to focus their existing
window; stale pane IDs do not restore unrelated tabs.

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
- A horizontal-tab menu entry is not implemented here.
- Missing hooks may delay agent status updates but do not gate keeping a tab.

## Telemetry

`App.KeepRunningMarked` records explicit menu/API opt-in together with
post-enable `TotalTabCount` and `KeepRunningTabCount` for attached terminal
tabs in the owning window. Search-hidden tabs count; detached tabs, other
windows, Settings, and other nonterminal tabs do not. The paired counts measure
the tab share at enable time, not a per-agent-session mark rate. Disabling,
startup, and repeated enable requests do not emit a snapshot.

`App.KeepRunningDetached` records successful background retention.
`App.KeepRunningReattachStarted` and `App.KeepRunningReattached` share a random
`AttemptId` for each restoration, including pre-transfer failures. Results are
`live` or `failed`; missing results remain unknown. A random `KeepId` correlates
the opt-in, detach and resolved-target restore without publishing the tab routing ID.
`HasAgentPane` distinguishes
tabs containing an agent pane from shell-only tabs, but does not assert an
active ACP session. `HasAgentSession` records whether the pane has a bound session ID,
not a guarantee of provider readiness. After reattachment, `WTA.AgentPromptSent.Reattached`
is true only for prompts on the same ACP session that survived the transfer;
its `KeepId` and `AttemptId` join it to that restore. Process exit has no `gone` event.

`App.TabPinChanged` separately records successful tab-order pin/unpin actions
with `Pinned` and post-action `PinnedCount`. The older `SidebarTabPinned` event
still means the Keep running menu action and is not tab-order pin telemetry.

Focused coverage lives in `TabTests::KeepRunning*` in
`src/cascadia/LocalTests_TerminalApp/TabTests.cpp`. The shared history/session
focus boundary also has the `Feature.KeepRunningFocus` ItE2E suite, covering
real UI detachment and protocol reattachment without launching a second session.
Native fixtures initialize mock controls through actual XAML layout before parking
them; UI coverage activates the selected package and checks retained pane/process
identity rather than relying on tab-title text as a readiness signal.
