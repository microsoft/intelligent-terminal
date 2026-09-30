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
| `Ctrl+Shift+S` | Expand/collapse the sidebar only. It does not activate or focus **Search tabs**. |
| `Ctrl+Shift+.` | Show/hide the independent Agent Pane; its behavior is unchanged. |

In horizontal layout, `Ctrl+Shift+S` remains a consumed no-op: no layout/chrome
change and no input leakage into the terminal. User bindings can override or
unbind the defaults.

## Scenario matrix

The two focus policies referenced here are defined separately below.

| State before the action | Action | Resulting surface/state | Focus policy |
|---|---|---|---|
| Sidebar collapsed | `Ctrl+Shift+S` | Expand the sidebar only; do not activate a search button. | Keep current focus. Do not create a source-restoration context. |
| Sidebar expanded, displaying tabs or ordinary tab search | `Ctrl+Shift+S` | Collapse the sidebar. | No-source policy. |
| Sidebar expanded, History hidden | `Ctrl+Shift+/` | Show History; remember that the sidebar was expanded. | Remember the source input, then focus the History search box. |
| Sidebar collapsed, History hidden | `Ctrl+Shift+/` | Expand the sidebar and show History; remember that the sidebar was originally collapsed. | Remember the source input, then focus the History search box. |
| History visible; sidebar was collapsed before History opened | `Ctrl+Shift+/` or the History close button | Hide History **and collapse the sidebar**. | History source-restoration policy. |
| History visible; sidebar was expanded before History opened | `Ctrl+Shift+/` or the History close button | Hide History; **keep the sidebar expanded**, displaying its ordinary page without History. | History source-restoration policy. |
| Sidebar expanded with History visible | `Ctrl+Shift+S` | Collapse the whole sidebar and hide History. | No-source policy; do not use History's remembered source or restore its previous expanded state. |

The close shortcut and History close button have the same behavior. A later
`Ctrl+Shift+S` expansion does not automatically activate or focus either search
box. A future dedicated search shortcut is outside this contract.

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
History's remembered source, not the no-source policy for `Ctrl+Shift+S`.

## Sidebar toggle: no source restoration

`Ctrl+Shift+S` controls visibility, not navigation or search.

- Expansion keeps the current input focus and does not invoke the first
  **Search tabs** button or the History search box.
- Collapse does not record, look up, or restore an opener.
- If current focus remains valid after collapse, leave it unchanged.
- If focus belonged to a sidebar element that is now hidden or otherwise becomes
  invalid, choose a visible, focusable terminal pane in the current tab, best
  effort.
- If no suitable target exists, retain any remaining valid focus without
  surfacing an error, blocking, or repeatedly trying to restore an unavailable
  surface.

This policy also applies when `Ctrl+Shift+S` hides an open History view. That
action is not a History-close shortcut and must not use History's saved origin.
A subsequent History opening captures a new entry context.

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
- Show the label and dimmed shortcut on the same line with 8 units of spacing.
  Keep Segoe UI Variable, `FontSize=12`, normal weight, `LineHeight=16`, and
  shortcut opacity `0.7`.
- Display normal shortcut casing, such as `Ctrl+Shift+S`, rather than serialized
  lowercase text.
- Resolve the effective sidebar binding and refresh the hint when settings
  change. Rebinding changes the displayed chord; unbinding or overriding the
  action hides the obsolete shortcut without leaving an empty gap.

The previously discussed idea of expanding the sidebar directly into
**Search tabs** is superseded. Do not implement it as part of `Ctrl+Shift+S`.

## Acceptance scenarios

These are required checks for this contract, not claims of completed validation:

- Exercise History open/close from both an initially expanded and an initially
  collapsed sidebar, using both the second physical shortcut and the close
  button.
- For both entry states, verify restoration to Agent Pane chat and to the exact
  originating terminal split when each remains available.
- Repeat with an unavailable source and verify the visible-terminal fallback,
  unchanged session data/drafts, and nonblocking behavior.
- Verify that `Ctrl+Shift+S` expansion does not activate search or move input
  focus, and that collapse uses no-source fallback even while History is open.
- Inspect both Expand/Collapse hints against the single-line designer reference,
  including the sidebar wording, exact casing, dimmed shortcut text, remapping,
  and unbinding.

The related release-checklist IDs remain `C110` (History), `C112` (action
dispatch), `C349` (sidebar toggle), and `C350` (hint presentation). Earlier
results for a different behavior contract are not acceptance of this revision.
