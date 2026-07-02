# Eternal Terminal — Step 1: Per-tab Save / Restore (layout + content)

*Design doc · 2026-07-02 · owner: @DDKinger (dev/yuazha/eternal-terminal-save-restore)*

## Summary

First, smallest step toward the [Eternal Terminal](https://github.com/shench_microsoft/DFX_specs/blob/hamza/IT/spec-updates-0.1/Intelligent%20Terminal/eternal-terminal/eternal-terminal.md)
vision: let a user **save the current tab as a named, restorable snapshot** and
**restore it later** into a new tab — reusing Windows Terminal's existing
"restore windows layout and content" machinery (`firstWindowPreference =
PersistedLayoutAndContent`).

Two new agent-pane slash commands drive it:

- **`/save-tab <title>`** — snapshot this tab's layout + scrollback content under
  a user-supplied title. Re-saving the same tab overwrites its snapshot.
- **`/restore-tab`** — open a picker of saved snapshots (↑/↓ + Enter); Enter
  restores the snapshot. If the original tab is still open, focus it instead of
  duplicating it.

Both commands are gated behind a new global setting
`experimental.eternalTerminal.enabled` (default `false`): when off, neither
command appears in the `/` menu.

This step restores **layout + scrollback content only**. Restoring the tab's
**agent pane** alongside the shell, **workspaces** (multi-tab / tab-group /
window layout), and **crash-recovery integration** are explicitly deferred to
later steps.

## Goals

- User-initiated, on-demand snapshot of a single tab (working directory, shell
  type, split-pane arrangement, profile, and scrollback content).
- Named snapshots the user can browse and restore from inside the agent pane.
- Restore into a new tab in the **current window**; focus the original tab if it
  is still open (do not duplicate).
- Overwrite-on-re-save so a tab maps to at most one snapshot at a time.
- Gated behind a global experimental flag; invisible when disabled.
- Reuse WT's existing serialization + content-persistence mechanism; **do not**
  modify the crash-recovery buffer-cleanup path.

## Non-goals (deferred to later steps)

- Restoring the tab's **agent pane / conversation** together with the shell
  (Step 2).
- **Workspaces**: saving/restoring the set of tabs, tab groups, and window
  layout together.
- Hooking snapshots into **crash recovery** / auto-restore on relaunch.
- **Live reattach** to still-running commands (spec-deferred, post-GA).
- Rebinding a *restored* tab back to its origin snapshot so re-saving it
  overwrites the original (see Open Questions).

## Background — existing WT layout + content persistence

The feature builds directly on WT's `firstWindowPreference` persistence. Key
mechanics (verified in code):

- **Layout** — `TerminalPage::PersistState()` serializes each tab via
  `Tab::BuildStartupActions(BuildStartupKind::Persist)` into a
  `Vector<ActionAndArgs>`, stored as `ApplicationState::PersistedWindowLayouts`
  (`IVector<WindowLayout>`, `WindowLayout = { TabLayout, InitialPosition,
  InitialSize, LaunchMode }`).
  (`TerminalPage.cpp:5625`, `ApplicationState.{h,cpp,idl}`)
- **Content** — for `PersistedLayoutAndContent`,
  `WindowEmperor::_finalizeSessionPersistence()` writes each pane's scrollback to
  `CascadiaSettings::SettingsDirectory()\buffer_{connection.SessionId}.txt` via
  `control.PersistTo(handle)` (`WindowEmperor.cpp:1310-1370`). Each pane's
  `Persist` startup action embeds its `connection.SessionId` into
  `NewTerminalArgs.SessionId` (`TerminalPaneContent.cpp:135`).
- **Restore** — when a pane is created from a `NewTerminalArgs` that carries a
  `SessionId`, `_MakeTerminalPane` calls
  `control.RestoreFromPath(SettingsDirectory\buffer_{sessionId}.txt)`
  automatically (`TerminalPage.cpp:7300-7308`). So **replaying the startup
  actions restores content for free**, as long as the `buffer_{sessionId}.txt`
  files exist where that code looks.
- **Cleanup** — `_finalizeSessionPersistence()` deletes stray buffer files with a
  **root-only, non-recursive** glob `FindFirstFileExW("{SettingsDir}\buffer_*")`
  / `elevated_*` (`WindowEmperor.cpp:1372-1397`). **A subfolder under
  SettingsDirectory is never touched by this cleanup**, and a live pane rewrites
  its own `buffer_{sessionId}.txt` on every persist cycle (~5 min).

Two consequences shape the design:

1. Because a live pane keeps overwriting `buffer_{liveSessionId}.txt`, a snapshot
   must keep its **own private copy** of the buffer to stay point-in-time.
2. Because the cleanup glob is root-only and prefix-scoped, storing snapshot
   buffers in a **dedicated subfolder** makes them immune to both cleanup and
   live overwrite **without modifying the cleanup code**.

## Architecture

```
Agent pane (Rust wta-helper TUI)
  /save-tab <title>  ─┐
  /restore-tab ───────┤  helper knows its own (pane_id, tab_id, window_id)
                      v
             CliChannel → wtcli.exe ──► COM IProtocolServer
                                          (TerminalProtocolComServer)
                                             │  new methods
                                             v
                                        TerminalPage / WindowEmperor
                                          • SaveTabSession(tabId,title)
                                          • ListSavedTabSessions()
                                          • RestoreTabSession(savedId)
                                          • DeleteSavedTabSession(savedId)
                                             │
              ┌──────────────────────────────┼───────────────────────────┐
              v                               v                           v
   ApplicationState                Snapshot buffer folder        _FindTabByStableId
   .SavedTabSessions      SettingsDirectory\SavedTabSessions\      + FocusTabInAnyWindow
   (IVector<SavedTabSession>)      {savedId}\buffer_{sid}.txt         (focus-if-open)
```

The Rust helper only renders UI and issues COM calls; **C++ owns the store,
the serialization, the snapshot buffers, and the focus/new-tab decision**. This
matches the existing "WTA reads/mutates WT state only through COM/wtcli"
boundary. Reads (`List`) and mutating commands that need a confirmation
(`Save`, `Restore`, `Delete`) are **request/response COM methods**, not the
fire-and-forget `send_wt_protocol_event` bus (which stays for state pushes like
`autofix_state`).

## Detailed design

### 1. Settings gate — `experimental.eternalTerminal.enabled`

- New global bool in `MTSMSettings.h`:
  `X(bool, EternalTerminalEnabled, "experimental.eternalTerminal.enabled", false)`
  (surfaced through `GlobalAppSettings` like any other global; follows the
  `copyOnSelect` / `focusFollowMouse` X-macro pattern).
- Surfaced to the helper the same way `autoFixEnabled` reaches it: when
  `TerminalPage` spawns the agent-pane helper, pass a CLI flag (e.g.
  `--eternal-terminal` / `--no-eternal-terminal`) reflecting
  `GlobalSettings().EternalTerminalEnabled()`.
- The helper stores the flag on `App`. The slash-command registry becomes
  **flag-aware**: `/save-tab` and `/restore-tab` are only returned by
  `commands::matches()` / accepted by `commands::parse()` when the flag is on, so
  they never appear in the `/` popup, `/help`, or Tab-completion when disabled.

### 2. Storage

**Records** — new `ApplicationState::SavedTabSessions`, an
`IVector<SavedTabSession>` in the `Local` file source, parallel to
`PersistedWindowLayouts` (so it is never consumed/cleared by startup restore).

```
SavedTabSession {
    String   Id;              // our snapshot key (guid)
    String   Title;           // user-supplied, shown in the picker
    String   SourceStableId;  // the live Tab StableId this was saved from
    DateTime SavedAt;         // for "saved 3m ago" + sort order
    String   TabActions;      // JSON: Vector<ActionAndArgs> from BuildStartupActions(Persist)
    IVector<String> BufferSessionIds; // pane SessionIds whose buffers live in this snapshot's folder
}
```

JSON (de)serialization mirrors `WindowLayout` in `ApplicationState.cpp`.

**Snapshot buffers** — one folder per snapshot:
`CascadiaSettings::SettingsDirectory()\SavedTabSessions\{Id}\buffer_{sessionId}.txt`.
Discoverable by the user, immune to the root-only cleanup glob, and trivially
deletable (remove the folder).

### 3. COM surface

New methods on `IProtocolServer` (`TerminalProtocol.idl`), implemented in
`TerminalProtocolComServer.cpp`, delegating onto `TerminalPage` /
`WindowEmperor`, with matching `wtcli` subcommands and `CliChannel` wrappers:

| IDL method | wtcli subcommand | Returns |
|---|---|---|
| `SaveTabSession(UInt32 tabId, String title)` | `wtcli save-tab --tab <id> --title <t>` | `savedId` (or error) |
| `ListSavedTabSessions()` | `wtcli list-saved-tabs` | JSON array of records |
| `RestoreTabSession(String savedId)` | `wtcli restore-tab --id <id>` | `{ outcome: "focused" \| "opened" }` |
| `DeleteSavedTabSession(String savedId)` | `wtcli delete-saved-tab --id <id>` | `ok` |

`tabId` is the helper's own tab (StableId), discovered via the existing
PID-matching pane-identity mechanism.

### 4. Save flow (`/save-tab <title>`)

1. Helper parses `/save-tab <title>` (title = the free-form `rest`, like
   `/fix <hint>`). Empty title → advisory: "please provide a title:
   `/save-tab <name>`" and no-op.
2. Helper calls `SaveTabSession(ownTabId, title)`.
3. C++:
   - `_FindTabByStableId(tabId)` → the live `Tab`; error if not found.
   - `tab->BuildStartupActions(BuildStartupKind::Persist)` → actions (each pane's
     `SessionId` already embedded).
   - For each terminal pane in the tab: `control.PersistTo()` into
     `…\SavedTabSessions\{newId}\buffer_{paneSessionId}.txt`; collect
     `paneSessionId` into `BufferSessionIds`. Best-effort per pane (`CATCH_LOG`);
     a failed pane dump does not abort the layout save.
   - **Overwrite**: if a `SavedTabSession` already has
     `SourceStableId == tab.StableId()`, reuse its `Id`, delete its old buffer
     folder, and update the record in place; otherwise append a new record with a
     fresh `Id`.
   - Persist `ApplicationState`.
   - Return `savedId`.
4. Helper pushes a system message: *"Saved this tab as «title». Restore it later
   with /restore-tab."* (localized).

No `SessionId` rewriting is needed: the snapshot's private folder keeps the
buffers frozen regardless of the live pane's later persists.

### 5. Restore flow (`/restore-tab`)

1. Helper calls `ListSavedTabSessions()` and opens a picker view
   (`saved_tabs_view`, modeled on `ui/agents_view.rs`): ↑/↓ move, Enter select,
   Esc close. Each row shows **Title**, relative `SavedAt`, and the snapshot's
   working directory (from the first pane's `startingDirectory`). Empty list → a
   "no saved tabs yet" message.
2. On Enter → `RestoreTabSession(savedId)`. C++:
   - **Focus-if-open**: if `_FindTabByStableId(record.SourceStableId)` finds a
     live tab → `WindowEmperor::FocusTabInAnyWindow(tab)` (cross-window) →
     return `{ outcome: "focused" }`.
   - **Else new-tab restore**: deserialize `record.TabActions`; make each pane's
     `buffer_{sessionId}.txt` resolvable from the snapshot folder
     `…\SavedTabSessions\{savedId}` during replay; replay the actions into the
     **current window** (new tab + splits via the action dispatcher). Each pane
     auto-restores content via the existing `RestoreFromPath` site. Return
     `{ outcome: "opened" }`.
3. Helper shows *"Switched to the original tab."* or *"Restored in a new tab."*
   accordingly, and closes the picker.

**Buffer redirection — key implementation decision.** The automatic content
restore in `_MakeTerminalPane` reads
`SettingsDirectory\buffer_{sessionId}.txt`; our snapshot buffers live in a
subfolder. Two contained ways to bridge that, to be finalized in the
implementation plan after checking whether tab action-replay is synchronous:
  - **(a) Base-dir override** — a short-lived `TerminalPage` member
    (`_savedTabBufferDir`) consulted at the single buffer-path site in
    `_MakeTerminalPane`, set before replay and cleared after. Clean **iff** the
    replay that creates the panes runs synchronously within that bracket.
  - **(b) Stage-into-root** — copy the snapshot's `buffer_{sessionId}.txt` files
    into `SettingsDirectory` root just before replay and let the *unmodified*
    restore path consume them; the restored panes adopt those `SessionId`s and
    the files become normal live-pane buffers thereafter. No restore-path change
    at all; the only risk is a persist-cleanup race in the copy→create window
    (tiny; can be closed by copying under a temporary keep-set).
Either way the **crash-recovery cleanup is untouched**; the choice only affects
how the restored panes find their snapshot buffers.

### 6. Delete flow (picker `D` key)

The `saved_tabs_view` binds `D` on the selected row →
`DeleteSavedTabSession(savedId)` → C++ removes the record and `rmdir`s the
snapshot's buffer folder. Included in this step because the per-snapshot folder
makes it a clean, contained operation.

### 7. Buffer folder lifecycle

- **Create**: on save, into `…\SavedTabSessions\{savedId}\`.
- **Overwrite**: delete the old `{savedId}` folder, re-dump.
- **Delete**: `rmdir {savedId}`.
- **Never** referenced by `_finalizeSessionPersistence` (root-only glob), so no
  cleanup coordination is required.

## Error handling

- **Save**: tab-not-found / serialization failure → COM error → helper advisory.
  Per-pane buffer dump failures are `CATCH_LOG`'d; the layout still saves (pane
  restores empty).
- **Restore**: unknown `savedId` → advisory. Focus target found but focus throws
  → fall back to new-tab restore. Missing/partial buffer file → `RestoreFromPath`
  tolerates it; pane opens empty.
- **Transport independence**: all four operations go helper → wtcli → COM, so
  they work even if the helper↔master ACP pipe is down (unlike `/restart`).

## Testing

- **Rust (`cargo test`, run under VS2022 vcvars64)**:
  - `commands.rs`: `/save-tab`, `/restore-tab` present iff the flag is on;
    hidden from `matches()` / rejected by `parse()` when off; `/save-tab`
    captures the title as `rest`.
  - `saved_tabs_view` render test (mirror `agents_view` render tests):
    rows, selection marker, empty state, `D`/Enter/Esc handling —
    locale-robust.
  - Dispatch-shape tests via the existing `DispatchedCommand` test hook
    (Save/List/Restore/Delete argv).
- **C++ (TAEF unit tests, e.g. `TabTests`)**:
  - Save → List → Restore round-trip reconstructs the tab.
  - Overwrite: two saves of the same StableId yield one record.
  - Focus-if-open vs new-tab-restore branch selection by StableId presence.
  - Content: a saved buffer folder is restored into the new tab's panes;
    delete removes the folder.
- **E2E (`ItE2E`)**: `/save-tab` creates a record; `/restore-tab` shows it;
  Enter opens (closed original) / focuses (open original). Assertions derived
  from `tools/wta/locales/*.yml` (locale-robust, per repo convention).

## Open questions / future steps

1. **Restored-tab rebinding** (deferred): a restored tab gets a new StableId, so
   re-saving it creates a *new* snapshot rather than overwriting its origin. If
   desired later, tag the restored tab with its origin `savedId` and have
   `SaveTabSession` prefer that tag over StableId matching.
2. **Title input UX**: inline `/save-tab <title>` for this step; a pre-filled
   overlay input could be a later polish.
3. **Step 2**: restore the tab's agent pane + conversation alongside the shell
   (couples to the existing agent-session resume / `session/load`).
4. **Workspaces**: multi-tab / tab-group / window-layout snapshots.

## Command / setting naming (decided)

- Commands: `/save-tab`, `/restore-tab` (tab-level, distinct from the existing
  `/sessions` agent-conversation picker).
- Setting: `experimental.eternalTerminal.enabled` (bool, default `false`).
