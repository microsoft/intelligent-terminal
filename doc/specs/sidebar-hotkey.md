# Sidebar and Tab Search Shortcuts

## Status

Decision record as of October 20, 2026. Development is authorized following
scenario alignment. This document describes the target behavior, not an
implementation or acceptance report.

The confirmed decisions below supersede the coupled Ctrl+Shift+S/search-entry
behavior in [Agent History and Sidebar Keyboard Navigation](agent-history-sidebar-keyboard.md).
Other behavior in that specification is unchanged unless explicitly agreed here.

## Confirmed Decisions

### Separate sidebar control from search

Ctrl+Shift+S controls the sidebar rather than opening search.

- When it expands a collapsed sidebar, focus the upper-left sidebar toggle
  button, identified by its Ctrl+Shift+S hover hint.
- When the sidebar is already expanded and the shortcut moves focus into it,
  focus that same toggle button.
- Neither entry path activates search or focuses its search box.
- Existing sidebar collapse behavior is not being redesigned by this change.

Ctrl+F is an Intelligent Terminal application shortcut for search, not a
sidebar-focus-only shortcut. It must work when terminal input owns focus.

### Search entry follows the selected layout

| Layout | Ctrl+F |
| --- | --- |
| Sidebar / vertical tabs | Open sidebar search and focus its search box; expand the sidebar first if it is collapsed. |
| Horizontal tabs | Open the existing floating tab-search palette through a new default shortcut binding. Do not change the layout or create a sidebar. |

The horizontal palette already has a `tabSearch` action and implementation.
The inspected baseline does not provide a default shortcut binding or default
command-list entry for it. Reusing that implementation does not mean Ctrl+F
already opens it today.

### Refocusing an existing query does not select all

If search already contains a query, Ctrl+F focuses the existing search input.
It must not clear or replace the query or explicitly select all of it.
Do not introduce a cursor or selection reset as part of refocusing.

### Settings uses ordinary window routing

Do not add Settings-specific shortcut behavior or a new Settings/Agent-pane
classification layer. Use normal window shortcut routing and the existing
control event-handling precedence.

The current routing paths are different but share the same action map:

- Ordinary terminal and Agent-pane input use TermControl's application-keybinding
  dispatch before forwarding unhandled input to their backend.
- Settings forwards its unhandled KeyDown events to the page-level shortcut
  handler. A child control can handle an event before that path receives it.

This decision does not authorize bypassing control handling to make shortcuts
unconditionally override every Settings control.

### Preserve Escape and Enter

Search-input Escape and Enter behavior remains unchanged. This feature separates
the entry shortcuts; it does not redesign search dismissal or result activation.

### Preserve existing overlays and window modes

Neither shortcut adds special handling for command palettes, menus, flyouts, or
dialogs. Preserve their existing event routing, control precedence, and dismissal
behavior. Do not force-close an overlay or bypass a modal dialog to execute a
shortcut.

Likewise, neither shortcut changes Focus mode or fullscreen behavior. Do not
automatically exit a mode or force sidebar chrome visible when the existing mode
hides it. These preservation rules take precedence over the ordinary Sidebar
entry scenarios below; the scenarios do not authorize overriding existing modes.

## Scenario Matrix

The first table records confirmed entry behavior. The second records the
preservation baseline rather than a new focus/navigation design.

| Starting state | Action | Required outcome |
| --- | --- | --- |
| Sidebar collapsed; terminal input focused | Ctrl+Shift+S | Expand and focus the upper-left sidebar toggle without opening search. |
| Sidebar expanded; terminal input focused; search closed | Ctrl+Shift+S | Keep expanded and focus the toggle without opening search. |
| Sidebar collapsed; terminal input focused | Ctrl+F | Expand, open search, and focus its input. |
| Sidebar expanded; terminal input focused; search closed | Ctrl+F | Open search and focus its input. |
| Search already has a query | Ctrl+F | Focus search; preserve the query and do not select all. |
| Horizontal tab layout | Ctrl+F | Open the existing tab-search palette using the new default binding. |
| Search input focused | Escape or Enter | Retain the existing behavior. |
| Settings has focus | Either shortcut | Use normal window/control routing with no Settings-specific implementation. |
| Command palette, menu, flyout, or dialog open | Either shortcut | Preserve existing routing and dismissal behavior; add no special handling. |
| Focus mode or fullscreen configuration hides sidebar chrome | Either shortcut | Preserve the mode; do not force sidebar visibility or automatically exit the mode. |

| Existing behavior to preserve | Scope |
| --- | --- |
| Ctrl+Shift+S inside the expanded sidebar collapses it and best-effort returns to input. | Preserve the existing return-target/fallback policy rather than inventing a new one. |
| Ctrl+Shift+S is a consumed no-op in horizontal layout. | No request to change its horizontal behavior. |
| Terminal content find uses Ctrl+Shift+F. | No request to replace or modify it. |
| User-defined bindings and explicitly unbound shortcuts use the action map. | Do not add a hard-coded chord interception path. |
| Drafts, selected tabs/panes, sessions, and filter preferences survive focus changes. | No unrelated input, session, or filter redesign. |
| Pointer entry points retain their behavior. | The requested change concerns keyboard entry responsibilities. |

## Preserved Search Lifecycle

When Ctrl+Shift+S enters an expanded sidebar while search is already open, move
focus to the toggle without closing search or changing its query. This is the
preservation default: removing automatic search activation does not introduce
another search-close/reset rule. Subsequent collapse continues to use the
existing search lifecycle.
