# Agent History and Sidebar Keyboard Navigation

## Status and scope

This specification defines the agreed behavior implemented by the sidebar keyboard
actions. It is not an acceptance report: build-specific results and remaining
validation belong in the release checklist and validation evidence.

The scenarios below cover the left sidebar in the **vertical** tab layout. The
sidebar and the independent **Agent Pane** are different surfaces. This contract
does not change `tabLayout`, horizontal agent-session behavior, or other
agent/delegation shortcuts.

| Default shortcut | Responsibility |
|---|---|
| `Ctrl+Shift+/` | Show/hide the Agent Session view in the sidebar, called **History** below. Opening History focuses its own search box. |
| `Ctrl+Shift+S` | Enter the sidebar through **Search tabs**, or collapse it and return to the previous input when focus is already inside. |
| `Ctrl+Shift+.` | Show/hide the independent Agent Pane; its behavior is unchanged. |

In horizontal layout, `Ctrl+Shift+S` remains a consumed no-op: no layout/chrome
change and no input leakage into the terminal. User bindings can override or
unbind the defaults.

## Scenario matrix

The two focus policies referenced here are defined separately below.

| State before the action | Action | Resulting surface/state | Focus policy |
|---|---|---|---|
| Sidebar collapsed | `Ctrl+Shift+S` | Expand the sidebar and open **Search tabs**. | Remember the current terminal or Agent input, then focus the tab-search box. |
| Sidebar expanded, focus outside the sidebar | `Ctrl+Shift+S` | Keep the sidebar expanded and open **Search tabs**. | Remember the current input, then focus the tab-search box. |
| Sidebar expanded, focus inside the sidebar | `Ctrl+Shift+S` | Collapse the sidebar and close tab search or History. | Best-effort return to the input used before entering the sidebar; fall back to a visible terminal. |
| Sidebar expanded, History hidden | `Ctrl+Shift+/` | Show History; remember that the sidebar was expanded. | Remember the source input, then focus the History search box. |
| Sidebar collapsed, History hidden | `Ctrl+Shift+/` | Expand the sidebar and show History; remember that the sidebar was originally collapsed. | Remember the source input, then focus the History search box. |
| History visible; sidebar was collapsed before History opened | `Ctrl+Shift+/` or the History close button | Hide History **and collapse the sidebar**. | History source-restoration policy. |
| History visible; sidebar was expanded before History opened | `Ctrl+Shift+/` or the History close button | Hide History; **keep the sidebar expanded**, displaying its ordinary page without History. | History source-restoration policy. |
| Sidebar expanded with History visible and focus inside | `Ctrl+Shift+S` | Collapse the whole sidebar and hide History. | Use the sidebar-hotkey entry input if still available, not History's saved entry state. |
| Sidebar expanded with History visible and focus outside | `Ctrl+Shift+S` | Hide History, keep the sidebar expanded, and open **Search tabs**. | Remember the current input, then focus tab search. |

The History close shortcut and close button have the same behavior. By contrast,
`Ctrl+Shift+S` intentionally opens and focuses ordinary tab search on entry.

## History: restore the entry state and input, best effort

When transitioning from hidden History to visible History, retain:

- Whether the sidebar was collapsed **before** any expansion needed to show
  History.
- The source input location: the Agent Pane chat input or the specific terminal
  pane, including its particular split.

Do not replace this entry context with the History search box when focus moves
there. When History is closed by its shortcut or close button, restore the
remembered sidebar expanded/collapsed state and attempt to restore the source
input.

1. If the source is still visible and focusable, return to that input location,
   preserving its unsent draft.
2. If the source has been collapsed, closed, or is otherwise unavailable, fall
   back to a visible, focusable terminal pane in the current tab.
3. If no suitable terminal target exists, retain any remaining valid focus and
   return without a user-facing error, blocking wait, or retry loop.

Do not expand a hidden Agent Pane, create a pane/tab, or resume a session merely
to recover focus.

**Important:** closing History that originally expanded a collapsed sidebar
also collapses the sidebar, but this is still a **History close**. It must use
History's remembered source, not the separate `Ctrl+Shift+S` entry input.

## Sidebar hotkey: enter search and return to input

`Ctrl+Shift+S` navigates between the current input and the sidebar's tab search.

- When the sidebar is collapsed, expand it, open ordinary **Search tabs**, and
  focus its search box. When expanded with focus outside, open/focus the same
  box without collapsing the sidebar.
- Remember the terminal or Agent chat input used just before this hotkey entry.
  A later entry from another input replaces that best-effort return target.
  Closing Search tabs with Escape or its button expires the target; opening
  Search tabs without the hotkey (including its pointer or keyboard button)
  starts without a saved hotkey source. A new hotkey entry captures its input
  only after search has opened successfully.
- When focus is inside the expanded sidebar, collapse it. If the remembered
  input remains visible and focusable, return there without changing its
  unsent draft. Otherwise, try the currently active visible terminal or Agent
  input, then a visible terminal pane in the current tab, best effort; never
  reopen a hidden Agent Pane or create a pane to recover focus.
- If no target exists, leave any remaining valid focus unchanged; do not
  display an error, block, or retry indefinitely.

The titlebar's Expand/Collapse button retains its existing visibility behavior;
it does not itself open tab search. `Ctrl+Shift+S` while History is visible
does not invoke History's source-restoration path. A subsequent History opening
captures its own new entry context.

## Data and lifecycle invariants

- These actions change visibility and focus only. They do not delete, clear, or
  modify underlying session records or conversation contents.
- They do not terminate agent processes/tasks, restart agents, or resume
  conversations for focus recovery.
- Preserve unsent chat and terminal input.
- History list refreshing and transient loading/error UI may follow the existing
  view-close lifecycle. Stopping a list refresh must not stop an agent task.
- Closing History does not mean closing the independent Agent Pane.
- Expected unavailable-focus cases are normal best-effort fallbacks, not reasons
  to display an error or leave keyboard interaction blocked.

## Sidebar toggle hint

- Use the localized labels **Expand sidebar** and **Collapse sidebar** for the
  tooltip and automation name, retaining the existing resource identifiers.
- Show the label and, on the collapsed **Expand sidebar** button, the dimmed
  shortcut on the same line with 8 units of spacing. The expanded
  **Collapse sidebar** button still collapses on click, but does not advertise
  `Ctrl+Shift+S`: from focus outside the sidebar that key now opens Search tabs.
  Keep Segoe UI Variable, `FontSize=12`, normal weight, `LineHeight=16`, and
  shortcut opacity `0.7`.
- Display normal shortcut casing, such as `Ctrl+Shift+S`, rather than serialized
  lowercase text.
- Resolve the effective sidebar binding and refresh the hint when settings
  change. Rebinding changes the displayed chord; unbinding or overriding the
  action hides the obsolete shortcut without leaving an empty gap.

Opening Search tabs with `Ctrl+Shift+S` does not change the separate
`Ctrl+Shift+/` History shortcut or the ordinary Tab traversal of sidebar items.

## Acceptance scenarios

These are required checks for this contract, not claims of completed validation:

- Exercise History open/close from both an initially expanded and an initially
  collapsed sidebar, using both the second physical shortcut and the close
  button.
- For both entry states, verify restoration to Agent Pane chat and to the exact
  originating terminal split when each remains available.
- Repeat with an unavailable source and verify the visible-terminal fallback,
  unchanged session data/drafts, and nonblocking behavior.
- Verify that `Ctrl+Shift+S` from either collapsed or expanded/outside focus
  activates tab search and focuses its box; a second press from inside
  collapses and best-effort restores the originating shell or Agent input.
- Inspect both Expand/Collapse hints against the single-line designer
  reference, including the sidebar wording, accurate shortcut presence,
  casing, remapping, and unbinding.

The related release-checklist IDs remain `C110` (History), `C112` (action
dispatch), `C349` (sidebar toggle), and `C350` (hint presentation). Earlier
results for a different behavior contract are not acceptance of this revision.
