# Window-scoped Workspace Save/Restore — Design

Status: approved (brainstorming)
Date: 2026-07-08
Branch: `dev/yuazha/eternal-terminal-save-restore`
Owner: @DDKinger

## 1. Motivation & Summary

The "Eternal Terminal" workspace feature currently:

- Saves a per-tab selection (multi-select picker).
- Persists a **UI snapshot** of each agent pane's chat (`completed_turns`,
  expanded state, executed/dismissed markers) to
  `agent-pane-history\{tabId}.json`, copies it into the workspace dir as
  `agent-chat-N.json`, and rehydrates it verbatim on restore
  (`--initial-chat-history`).
- Always restores into a brand-new window.

Two findings drive this redesign:

1. **The chat snapshot is redundant for content.** A controlled probe
   (drive Copilot's ACP directly, `session/load` a real session) confirmed the
   agent replays each turn's response **verbatim, including the recommendation
   JSON** that WTA parses into the "Suggested … ✓ Run: …" cards. So the rich UI
   is re-derivable from `session/load` — we do not need to store it.
2. **A window is the natural unit.** Users think of "the set of tabs in this
   window" as the thing to save and bring back.

This redesign makes the **window** the unit of a workspace, **drops the chat
snapshot entirely** (re-deriving agent-pane content from `session/load`), and
makes restore **window-aware**: it restores only the tabs that are missing,
into the window that still anchors the workspace, and opens a new window only
when nothing of the workspace is left.

### What we keep vs. drop

Keep:

- Per-tab `agentSessionId` in the saved record (needed for `session/load`).
- The friendly "session is open elsewhere / can't resume" message on
  `session/load` failure, then a fresh session.
- The buffer/layout replay (`bufferSessionIds`, `tabActions`).

Drop (delete code):

- `chatHistoryFile` field + the workspace-dir `agent-chat-N.json` copies.
- `tools/wta/src/chat_history.rs` (`ChatHistoryFile`, `write`, `read_path`,
  `path_for`, `dir`).
- `App::persist_chat_history_if_changed` (+ the `last_persisted_history_sig`
  field and its UI fingerprint) and the two call sites in the event loop.
- `App::rehydrate_owner_tab_history`.
- The `--initial-chat-history` CLI flag (Rust) and the C++ code that reads the
  per-tab history file, computes `hasConversation`, copies the file, and passes
  `--initial-chat-history`.
- The `agent-pane-history` directory writer usage (the directory itself is no
  longer written by this feature).

## 2. Data Model

Persisted in `state.json` under `savedWorkspaceSessions[]` (types in
`ApplicationState.idl` / `SavedWorkspaceSession*`):

```jsonc
{
  "id": "{workspace-guid}",
  "title": "my workspace",
  "savedAt": "<unix-ms>",
  "tabs": [
    {
      "workspaceTabId": "{stable-guid}", // minted at save; identifies this tab
                                         // within the workspace across restore
      "bufferSessionIds": ["..."],
      "tabActions": [ /* newTab / splitPane replay */ ],
      "agentPane": {                     // optional; present only if this tab's
                                         // agent pane had a real conversation
        "cli": "copilot",
        "model": null,
        "position": "bottom",
        "agentSessionId": "<acp-session-id>"
      }
    }
  ]
}
```

Changes vs. today:

- **Add** `workspaceTabId` per tab (stable identity for anchor-matching).
- **Remove** `agentPane.chatHistoryFile`.
- `sourceStableId` is no longer needed for identity and is removed (the live
  StableId at save time is not stable across restore; `workspaceTabId` replaces
  it).

### In-memory binding (not persisted)

Each **live** `Tab` carries, in memory, set at save time (and re-set on
overwrite):

- `_boundWorkspaceId` — which workspace (already exists).
- `_boundWorkspaceTabId` — which tab within the workspace (**new**).
- `_boundHomeWindowId` — the WT window id (peasant id) the tab was saved in
  (**new**).

These are session-scoped and intentionally lost on a full WT restart (see
Restore §5, "no anchors" path).

## 3. session_id / has_conversation channel (helper → C++)

The chat file previously served two jobs: (a) the UI snapshot [dropped], and
(b) telling C++ each agent pane's ACP `session_id` and whether it had a
conversation. With the file gone, (b) moves onto the existing per-tab event
stream:

- The helper includes `session_id` and `has_conversation`
  (`completed_turns` non-empty) in its `agent_state_changed` event (already
  tagged with `tab_id`). It re-emits when either changes (session attach, first
  completed turn).
- C++ caches these per tab (on the `Tab`, alongside the binding fields) when it
  handles `agent_state_changed` (routed via `_FindTabByStableId`).
- At `/save-ws`, C++ reads the cache — no file, no save-time round-trip.

`agentSessionId` is written to the record only when `has_conversation` is true
(mirrors today's gate: a pre-warmed-but-unused pane's bootstrap session id was
never persisted by the CLI and would fail `session/load`).

## 4. Save flow (`/save-ws`)

1. **Scope:** save the **entire current window** — every tab, with **no
   interactive selection**. The multi-select picker UI is removed
   (`SaveWorkspaceSelectState`, `ui/save_ws_select_view.rs`,
   `handle_save_ws_select_key`). The window's tab set is still enumerated (to
   pass tab ids to the save), but non-interactively — either via the
   window-scoped `list-tabs` with all tabs implicitly selected, or by having
   C++ save every tab of the initiating window directly. (Exact enumeration
   mechanism is an implementation-plan detail; the behavior is "all tabs, no
   prompt to choose".)
2. **Already-saved detection:** the window is "already a workspace" if its tabs
   are bound to some workspace `W` with `_boundHomeWindowId == currentWindowId`.
   - **Not saved** → prompt for a title, mint a new workspace `id`, save.
   - **Already saved** → confirm dialog: *"This window is already saved as
     workspace «<W.title>». Overwrite?"* → **overwrite `W`** (re-snapshot the
     window's current tabs). Cancel/Esc → no-op. **No "save as new" option.**
     (Rationale: save-as-new would put the same live agent session under two
     workspaces, which complicates resume/liveness.)
3. For each saved tab: mint `workspaceTabId`, bind the live tab
   `(W, workspaceTabId, homeWindowId)`. Write `agentPane.agentSessionId` only
   when the cached `has_conversation` is true.

## 5. Restore flow (window-aware)

Given a saved workspace `W` with tabs `[t1..tn]`:

1. **Enumerate** all live tabs, across all windows, bound to `W` — each yields
   `(workspaceTabId, currentWindowId, homeWindowId)`.
2. **Anchors** = bound tabs whose `currentWindowId == homeWindowId` (a tab
   dragged to another window has `currentWindowId != homeWindowId` and is **not**
   an anchor — it counts as "closed/missing").
3. **Decide target:**
   - **Anchors exist** → target = the anchor's home window (all anchors share
     the same home window, since a window is saved as one workspace). Restore
     **only the missing tabs** — saved tabs whose `workspaceTabId` has no anchor
     in the target window — **into that existing window**.
   - **No anchors** (all closed, all dragged away, or the home window is gone) →
     **open a new window** and restore **all** tabs.
4. **Dragged-out example** — workspace `[A,B,C]` saved in window 1; C dragged to
   window 2; B closed. On restore: A is an anchor in window 1 ⇒ target = window
   1; missing = {B, C} ⇒ recreate B and a **fresh** C in window 1. The real C
   stays in window 2 (its live agent session keeps running — see §6).
5. **Full WT restart** — all in-memory bindings are gone ⇒ no anchors ⇒ new
   window, restore all. (Matches the "all closed → new window" rule.)

### Restore mechanics

- **New-window path:** the existing `WindowEmperor::RequestRestoreWorkspaceInNewWindow`
  → `_RestoreWorkspaceOnInit` flow, restoring **all** tabs, then removing the
  default startup tab.
- **Existing-window path (new):** a page method that injects **only the missing
  tabs'** replay actions into the current window (no new window, no default-tab
  removal), then binds each newly created tab and requests its agent pane. This
  reuses the `_BindRestoredWorkspaceTabs` incremental-binding discipline (bind
  synchronously right after each `DoAction`, before the pre-warm tick) so the
  agent-pane load hint is registered in time.
- The emperor/COM layer decides target (it already has cross-window visibility
  via `GetWindows()`), then routes to new-window or existing-window path.

## 6. Agent-pane resume (no snapshot)

A restored tab that had an agent pane is reopened and resumed via
`agentSessionId` → `session/load`:

- **Replay parsing (new):** the resume path routes the replayed agent messages
  through the **same recommendation parser** used live
  (`turn_observe_chunk` / `turn_try_eager_surface` /
  `format_recommendations_for_chat`), and **filters out the injected
  system-prompt turn** (the leading `# Terminal Agent …` `user_message_chunk`)
  and any WTA-internal scaffolding, so the reopened pane renders the original
  rich cards / turns instead of raw text/JSON.
  - Turns are reconstructed as `completed_turns` (default collapsed — the
    expanded/executed UI state is intentionally not restored; it is pure UI
    state and, per the redesign, not persisted).
- **Busy / failure:** if `session/load` fails — the session is live elsewhere
  (a dragged-out tab's original pane), a CLI mismatch, or `session/load` is
  unsupported (e.g. gemini) — keep the existing **friendly message** and start a
  **fresh** session. Two panes then legitimately show two sessions (the dragged
  original + the restored fresh copy).

## 7. Code removal checklist

- Rust: `chat_history.rs`; `persist_chat_history_if_changed` + call sites +
  `last_persisted_history_sig`; `rehydrate_owner_tab_history`;
  `--initial-chat-history` CLI arg + its main.rs seeding; the save-ws
  **interactive** multi-select UI (`SaveWorkspaceSelectState`,
  `ui/save_ws_select_view.rs`, `handle_save_ws_select_key`) — replaced by the
  auto-all-tabs save. (Tab enumeration for the save may still reuse the
  window-scoped `list-tabs` path; only the *selection* step is removed.)
- C++: history-file read / `hasConversation` / copy / `--initial-chat-history`
  in `TerminalPage.Protocol.cpp` save path; `chatHistoryFile` in
  `ApplicationState` + IDL + `SaveWorkspaceSessionProtocol`; add
  `workspaceTabId`, per-tab session cache, binding fields.

## 8. Testing

- **Rust unit tests:**
  - Replay parsing: given a canned `session/load` replay (system-prompt turn +
    a recommendation-JSON agent turn + a plain agent turn), assert the produced
    `completed_turns` drop the system prompt and render the recommendation as
    the card text; assert plain turns pass through.
  - Target-window selection: extract the anchor/target/missing decision into a
    pure function over `[(workspaceTabId, currentWindow, homeWindow)]` +
    `savedTabIds` and table-test: all-closed→new; one-anchor→existing+missing;
    dragged→treated-as-missing.
- **e2e (ItE2E):** partial restore into an existing window; new-window restore
  when all closed; dragged-out tab → busy message + fresh session. Locale-robust
  assertions (derive expected strings from locale YAMLs, per repo convention).

## 9. Out of scope / non-goals

- Persisting bindings across a full WT restart (Approach B) — rejected: window
  ids regenerate and the old tabs are gone, so there is nothing to reuse.
- Restoring expanded/collapsed state and executed/dismissed markers — dropped
  as pure UI state that `session/load` cannot reproduce.
- Cleanup/retention of the (now-unused) `agent-pane-history` directory — a
  separate concern; this redesign simply stops writing new snapshot payloads.
