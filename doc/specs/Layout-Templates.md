---
author: Kai Tao @kaitao
created on: 2026-06-03
last updated: 2026-06-04
issue id: TBD
---

# Layout Templates (named, reusable pane/tab layouts)

## Abstract

Users often arrange a window into a specific set of tabs and split panes — a
"left-half editor, top-right logs, bottom-right shell" arrangement, or a
six-pane grid each pointed at a different repo — and want to bring that exact
arrangement back on demand. Today Intelligent Terminal (like upstream Windows
Terminal) can only restore such an arrangement *passively*: the
`firstWindowPreference: persistedWindowLayout` machinery snapshots the live
window on exit and reopens it on next launch, anonymously and automatically.
There is no way to *name* a layout, save it deliberately, and later open it as a
fresh tab/window from a menu or the Command Palette.

This spec proposes **Layout Templates**: named, user-managed layout snapshots
that capture the *structure* of a tab (its pane tree, split ratios, and
per-pane profile) and can be instantiated on demand. It deliberately scopes
**structure only** — it does **not** promise to restore live process state
(running commands, scrollback) or agent conversation history. The rationale for
that boundary is the core of this design and is spelled out below.

## Inspiration

The desire is long-standing in the upstream Windows Terminal community, where it
recurs as a family of duplicate requests, all funneled together and largely
answered with "you can already script this with actions":

- [#3759 Specify Panes in a Profile](https://github.com/microsoft/terminal/issues/3759)
- [#10223 Workspaces](https://github.com/microsoft/terminal/issues/10223)
- [#8590 Save a pane layout as a profile](https://github.com/microsoft/terminal/issues/8590)
- [#10717 Save panes layout as a profile, openable from the new-tab menu](https://github.com/microsoft/terminal/issues/10717)
- [#1571 New Tab Menu Customization](https://github.com/microsoft/terminal/issues/1571)
- [#766 Persist / restore instance settings](https://github.com/microsoft/terminal/issues/766) (the *passive* restore that shipped)

### Why upstream left this gap (and why we can fill it)

Upstream did **not** decline the feature; it split it in two and shipped only
half:

1. **The passive half — session restore — shipped.** `firstWindowPreference`
   snapshots and reopens the last window automatically. Anonymous, index-based,
   no "save as", no "open which one".

2. **The active half — named templates — was deferred**, for two reasons that
   matter to our design:

   - **Overlap.** Profiles, `startupActions`, `actions` / Command Palette
     (`multipleActions`), and `newTabMenu` can already describe "open these
     panes". The maintainer's standard answer (zadjii-msft on #10223/#3759) was
     to point at those primitives rather than add a new top-level object, to
     avoid a fourth way to configure the same thing.
   - **State is the hard part, and has no clean solution.** The *layout* (the
     pane tree) is resolution-independent and trivial to persist. What users
     *also* implicitly want — the cwd of each pane, the command that was
     running, the ssh session — cannot be captured or safely replayed: ConPTY
     does not expose a child's true cwd or process state, and blindly re-running
     a captured command line (`deploy`, `rm`, …) is dangerous.

We can fill the gap precisely because we **decline the unsolvable part**. A
Layout Template is structure, not a session snapshot. This sidesteps the overlap
objection (we offer a *better UI* over the same replay primitive, deliberately,
rather than a redundant config surface) and the state objection (we never claim
to restore running state).

## Background: how layouts already serialize

This feature is cheap to build because the underlying primitive already exists
and is already battle-tested by session restore.

- A tab's pane arrangement is a binary tree of `Pane` objects. It is **not**
  serialized as a nested tree. `Pane::BuildStartupActions`
  (`src/cascadia/TerminalApp/Pane.cpp`) linearizes the tree into an ordered
  sequence of `ActionAndArgs` — a *replay script* of `SplitPane` and `MoveFocus`
  actions equivalent to "the steps a user would click to rebuild it".
- Split sizes are stored as **proportions** (`_desiredSplitPosition`, a
  `float` in `[0,1]`), never pixels. Every layout calculation multiplies the
  proportion by the *current* available space (e.g. `Pane.cpp` ~L843,
  ~L2334). Therefore a layout is resolution-independent: replaying it in a
  differently sized window simply re-flows every pane proportionally.
- `WindowLayout` (`ApplicationState.idl`) already wraps `TabLayout`
  (`IVector<ActionAndArgs>`) plus window `InitialPosition` / `InitialSize` /
  `LaunchMode`. `ApplicationState` persists a `PersistedWindowLayouts` vector to
  `state.json`. Restore replays `TabLayout` via
  `TerminalWindow::LoadPersistedLayout` →
  `_root->SetStartupActions(layout.TabLayout())`.

Layout Templates reuse `BuildStartupActions(Persist)` to capture and
`SetStartupActions` to instantiate. The new work is **naming, storage, and
invocation surfaces** — not layout math.

## Solution Design

### Data model

Add a **named** collection that is kept distinct from the anonymous
session-restore vector, so deliberate templates and auto-snapshots never pollute
each other.

New WinRT type in `ApplicationState.idl`, alongside `WindowLayout`:

```idl
runtimeclass LayoutTemplate
{
    String Name;                                   // user-facing, unique key
    Windows.Foundation.Collections.IVector<Microsoft.Terminal.Settings.Model.ActionAndArgs> TabLayout { get; };
    // Intentionally NO InitialSize / InitialPosition: a template adapts to the
    // target window. (A future revision may add an optional preferredLaunchMode.)
}
```

Add to `ApplicationState` (and `ApplicationState.idl`):

```idl
Windows.Foundation.Collections.IVector<LayoutTemplate> LayoutTemplates;
void UpsertLayoutTemplate(LayoutTemplate layout);   // replace-by-name or append
Boolean RemoveLayoutTemplate(String name);
```

Persisted to the existing `state.json` (it is `FileSource::Local`, so it is
elevation-safe and already throttled/flushed by the existing machinery). Example
on-disk shape:

```jsonc
{
  "layoutTemplates": [
    {
      "name": "Dev: editor + logs + shell",
      "tabLayout": [
        { "action": "splitPane", "split": "right", "size": 0.5, "profile": "PowerShell" },
        { "action": "moveFocus", "direction": "previousInOrder" },
        { "action": "splitPane", "split": "down", "size": 0.3, "profile": "cmd" }
      ]
    }
  ]
}
```

Note the stored `tabLayout` is exactly what `BuildStartupActions` already emits
for session restore — we are reusing a proven serialization, not inventing one.

### New actions

Two new `ShortcutAction`s in the Terminal Settings Model, both thin wrappers
over existing capture/replay paths:

| Command | Args | Behavior |
| --- | --- | --- |
| `saveLayoutTemplate` | `name` (optional) | Capture the **current active tab**'s pane tree via `Tab::BuildStartupActions(BuildStartupKind::Persist)` and `UpsertLayoutTemplate`. If `name` is omitted, prompt for one. |
| `newTabFromLayout` | `name` (required) | Look up the template by name and instantiate it as a **new tab in the current window** via the same `SetStartupActions` replay path used by restore. |

`saveLayoutTemplate` defaults to the active tab (the common case). A
`scope: "window"` variant capturing all tabs is a straightforward extension
(iterate tabs, concatenate `newTab` + that tab's actions) and is listed under
Future Considerations to keep the first cut small.

Example bindings:

```jsonc
{ "command": { "action": "saveLayoutTemplate" }, "name": "Save current tab as layout template…" },
{ "command": { "action": "newTabFromLayout", "name": "Dev: editor + logs + shell" }, "keys": "ctrl+alt+1" }
```

### Invocation surfaces

1. **Command Palette** — the primary surface (matching upstream's stated
   preference that the palette is "arguably a better UI" than the new-tab
   dropdown). `saveLayoutTemplate` appears as a command; each saved template is
   surfaced as a generated `newTabFromLayout` command so it is searchable by
   name.
2. **New-tab dropdown** — optionally list templates under a "Layouts" section,
   composing with the existing `newTabMenu` customization (#1571).
3. **Settings UI** (`TerminalSettingsEditor`) — a management page to rename and
   delete templates. Creation stays action-driven (you save *from* a live
   arrangement, not by hand-authoring a tree).

### Capture/replay flow

```
saveLayoutTemplate:
  active Tab -> Tab::BuildStartupActions(Persist)
             -> SplitPane / MoveFocus action list (proportional sizes)
             -> LayoutTemplate{ Name, TabLayout }
             -> ApplicationState::UpsertLayoutTemplate -> state.json

newTabFromLayout(name):
  ApplicationState::LayoutTemplates[name]
             -> new tab in current window
             -> SetStartupActions(template.TabLayout)
             -> replay SplitPane(size=ratio) against current tab size
```

Because sizes are proportional, a template saved in a maximized 4K window
instantiates correctly in a small floating window; panes re-flow to the same
ratios.

## What is captured, and what is not

**Captured:** the pane tree shape, split direction, split ratios, and each leaf
pane's `profile` (and any commandline already baked into its `NewTerminalArgs`).

**Optionally captured (off by default):** per-pane `startingDirectory`. We can
record the cwd that the pane was *launched* with from its `NewTerminalArgs`. We
must **not** attempt to read the live cwd of a running shell — ConPTY doesn't
expose it reliably. This is opt-in to avoid surprising "wrong directory"
results.

**Explicitly NOT captured:**

- Running commands / processes in a pane — cannot be observed, must not be
  blindly replayed.
- Scrollback / buffer content — that is the separate
  `persistedLayoutAndContent` session-restore path, not a template concern.
- **Agent pane conversation state.** Per `Multi-window-agent-pane.md`, agent
  session state is lost on Terminal restart today. A template will recreate the
  agent *pane* (structure + profile) but starts a fresh agent session; it will
  not replay chat history. This boundary must be documented in user-facing help
  so the feature doesn't over-promise.

Drawing this line is the whole point: it is exactly the boundary upstream never
committed to crossing, and crossing it is what made the feature perpetually
"complex, budgeted work" rather than shippable.

## Experience Design

This section specifies the three core flows as wireframes. Wireframes are ASCII
approximations of XAML; final visuals follow the Settings UI (#1564) and Command
Palette (#2046) styling. The **layout thumbnail** is treated as a first-class,
cross-cutting element and is described first because every flow uses it.

### Layout thumbnails (the core visual primitive)

A thumbnail is a small render of a template's pane arrangement, used so the user
recognizes a layout *by shape* rather than by name alone. Key design point:

> **Thumbnails are rendered live from the stored proportional layout, not stored
> as bitmaps.** The pane tree (split direction + `[0,1]` ratios) is already in
> `TabLayout`. A small XAML control recursively subdivides a rectangle by the
> same ratios used at runtime, tinting each leaf by its profile's color and
> labeling it with the profile's initials. No image is persisted; thumbnails
> stay correct if the user re-themes or renames a profile, and they cost
> nothing in `state.json`.

```
  ┌──────────┬──────────┐      split ratios drawn directly:
  │          │   cmd    │        outer:  vertical  split @ 0.50
  │   pwsh   ├──────────┤        right:  horizontal split @ 0.30
  │          │   pwsh   │
  └──────────┴──────────┘
```

Degenerate cases: a single-pane template renders one filled cell; very deep
trees clamp to a maximum visible depth (leaves beyond it collapse into a "…"
cell) so the thumbnail stays legible at ~16×6 px-cells.

### Flow 1 — Save current tab as a layout template

Triggered by the `saveLayoutTemplate` action (Command Palette, the "+ From
current" button in Settings, or a user keybinding). Opens a content dialog
showing a **live preview of the tab being saved**, so the user confirms they are
capturing the right arrangement.

```
┌─ Save layout template ──────────────────────────────┐
│                                                      │
│  Name                                                │
│  ┌────────────────────────────────────────────────┐ │
│  │ PowerShell + cmd                              ▌ │ │  ← prefilled suggestion (selected)
│  └────────────────────────────────────────────────┘ │
│                                                      │
│  Preview — current tab                               │
│  ┌──────────┬──────────┐    3 panes                  │
│  │          │   cmd    │    PowerShell ×1             │
│  │   pwsh   ├──────────┤    cmd ×1                    │
│  │          │   pwsh   │                              │
│  └──────────┴──────────┘                              │
│                                                      │
│  ☐ Include each pane's starting directory            │
│                                                      │
│              [ Cancel ]            [ Save ]           │
└──────────────────────────────────────────────────────┘
```

- **Name suggestion** is derived from the distinct profiles in the tab
  (e.g. "PowerShell + cmd"); the text is pre-selected so the user can type over
  it immediately.
- **Starting-directory opt-in** is unchecked by default (see "What is captured")
  to avoid surprising "wrong directory" restores.
- **Overwrite state** — if the typed name matches an existing template, an
  inline warning appears and the primary button relabels:

```
│  ┌────────────────────────────────────────────────┐ │
│  │ Dev: editor + logs + shell                     │ │
│  └────────────────────────────────────────────────┘ │
│  ⚠ A template named “Dev: editor + logs + shell”     │
│    already exists. Saving will replace it.           │
│              [ Cancel ]         [ Overwrite ]        │
```

### Flow 2 — Open a template (Command Palette, primary surface)

Typing in the palette surfaces saved templates (each a generated
`newTabFromLayout` command) alongside the management commands. Each row carries
its thumbnail + a one-line summary so the user picks by shape and contents.

```
┌─ Command Palette ───────────────────────────────────┐
│ > layout                                             │
├──────────────────────────────────────────────────────┤
│  Layout templates                                    │
│   ┌────┐  Dev: editor + logs + shell                 │
│   │▛▟▖ │  3 panes · pwsh, cmd                         │
│   └────┘                                              │
│   ┌────┐  Ops: 4-pane grid                            │
│   │▛▀▜ │  4 panes · pwsh ×4                            │
│   │▙▄▟ │                                              │
│   └────┘                                              │
│  ──────────────────────────────────────────────       │
│   ⚙  Save current tab as layout template…             │
│   ⚙  Manage layout templates…                         │
└──────────────────────────────────────────────────────┘
```

- **Enter** opens the highlighted template as a **new tab in the current
  window** (the common case; "open in new window" is a future submenu action).
- Templates are also offered as a "Layouts" section in the new-tab dropdown,
  composing with `newTabMenu` (#1571) for users who prefer that surface.

### Flow 3 — Manage templates (Settings UI)

A page under Settings for renaming/deleting/opening. Creation stays
action-driven (you save *from* a live tab), so the page's only "create"
affordance is "+ From current", which runs Flow 1 for the active tab.

```
Settings ▸ Layout templates

┌──────────────────────────────────────────────────────────────┐
│  Layout templates                           [ + From current ]│
│  Saved pane arrangements you can open as a new tab.            │
├──────────────────────────────────────────────────────────────┤
│   ┌──────┐  Dev: editor + logs + shell                        │
│   │ ▛▟▖  │  3 panes · PowerShell, cmd                          │
│   └──────┘                    [ Open ] [ Rename ] [ Delete ]   │
│                                                               │
│   ┌──────┐  Ops: 4-pane grid                                  │
│   │ ▛▀▜  │  4 panes · PowerShell ×4                            │
│   │ ▙▄▟  │                                                    │
│   └──────┘                    [ Open ] [ Rename ] [ Delete ]   │
└──────────────────────────────────────────────────────────────┘
```

**Empty state** (first run — important for discoverability, since there is no
hand-authoring path):

```
┌──────────────────────────────────────────────────────────────┐
│  Layout templates                           [ + From current ]│
├──────────────────────────────────────────────────────────────┤
│                                                               │
│              No layout templates yet.                         │
│                                                               │
│   Arrange a tab into the panes you like, then choose          │
│   “Save current tab as layout template…” from the Command     │
│   Palette — or click “+ From current” above.                  │
│                                                               │
└──────────────────────────────────────────────────────────────┘
```

- **Rename** is inline edit on the row; a name collision shows the same warning
  as Flow 1's overwrite state and blocks the rename until resolved.
- **Delete** confirms inline ("Delete ‘Dev…’? [Cancel] [Delete]"); no separate
  modal.
- **Editing a layout** (changing the pane shape) is *not* a v1 affordance: the
  user re-arranges a live tab and re-saves over the same name (Flow 1 overwrite).
  Called out deliberately to keep the first cut small; an in-place layout editor
  is Future work.

### Cross-cutting UX rules

- **Discoverability:** the `saveLayoutTemplate` command, the "+ From current"
  button, and the empty-state copy all teach the save entry point; templates
  then appear in the palette and (optionally) the new-tab dropdown.
- **Missing profile on open:** a leaf whose `profile` no longer exists falls
  back to the default profile and shows a non-blocking info bar naming the
  missing profile (mirrors existing missing-profile behavior).
- **Corrupt / empty template:** an entry with an empty `TabLayout` is skipped
  on load with a warning rather than producing a blank tab.
- **Accessibility:** thumbnails are decorative (`AutomationProperties` ignored);
  each row exposes an accessible name summarizing the layout
  (e.g. "Dev: editor + logs + shell, 3 panes, PowerShell and cmd"). All flows
  are fully keyboard-operable; the save dialog traps focus and Esc cancels.

## Capabilities / Concerns

| Concern | Notes |
| --- | --- |
| **Compatibility** | Additive only: a new `layoutTemplates` key in `state.json` and two new actions. No change to existing `firstWindowPreference` / `persistedWindowLayout` behavior. Older builds ignore the unknown key. |
| **Accessibility** | Command Palette and Settings UI inherit existing XAML accessibility. |
| **Security** | Templates store profile names + optional starting directories — no secrets, no captured command output. Same `FileSource::Local` elevation handling as existing state. We deliberately do not persist running command lines. |
| **Reliability** | Reuses the same replay path exercised by session restore, so layout fidelity is already proven. Missing-profile fallback prevents broken instantiation. |
| **Performance** | Negligible; capture is a tree walk already performed on every persist, instantiation is the existing startup-actions path. |

## Future considerations

- **Window-scope templates** (`scope: "window"`): capture all tabs, instantiate
  as a new window. Natural extension of the active-tab cut.
- **"Workspace" = ordered set of templates** opened together — the closest thing
  to #10223. Build only after single-tab templates prove out.
- **In-place layout editor** — edit a template's pane shape without re-saving
  from a live tab (v1 only supports re-save-over-name).
- **"Open in new window"** — a palette/menu submenu action beside the default
  "open as new tab".
- **Optional `preferredLaunchMode`** on a template (maximized/fullscreen/focus).
- **Export/import / share** a template as a JSON snippet.
- **Agent session checkpoint** — if/when agent state becomes persistable
  (tracked as future work F1 in `Multi-window-agent-pane.md`), a template could
  *optionally* rebind a saved session. Out of scope here by design.

## Footnotes / references

- Codebase primitives: `src/cascadia/TerminalApp/Pane.cpp`
  (`BuildStartupActions`, `_desiredSplitPosition`),
  `src/cascadia/TerminalApp/Tab.cpp`,
  `src/cascadia/TerminalSettingsModel/ApplicationState.{idl,h,cpp}`
  (`WindowLayout`, `PersistedWindowLayouts`),
  `src/cascadia/TerminalApp/TerminalWindow.cpp` (`LoadPersistedLayout`),
  `src/cascadia/TerminalSettingsModel/GlobalAppSettings.idl`
  (`FirstWindowPreference`).
- Related spec: `doc/specs/#532 - Panes and Split Windows.md`,
  `doc/specs/#1571 - New Tab Menu Customization`,
  `doc/specs/#8324 - Application State (TSM).md`,
  `doc/specs/Multi-window-agent-pane.md`.
