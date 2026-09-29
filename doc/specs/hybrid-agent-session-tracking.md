# Hooks-Fallback Agent Session Watcher (Hybrid Tracking)

## Abstract

Intelligent Terminal (IT) surfaces a live list of agent-CLI sessions
(Copilot / Claude / Codex / Gemini) in the `/sessions` view. On `main`, the
data that powers that list — *which* sessions exist, *which* pane each one runs
in, and *what* each one is doing (Working / Idle / Attention) — comes from the
PowerShell `wt-agent-hooks` bridge that every supported CLI loads, plus the
*born-bound* registration for sessions IT launches itself (see
[wta-launched-cli-session-binding.md](./wta-launched-cli-session-binding.md)).

That coverage has a hole: a session the user **types themselves** (`codex`,
`copilot`, … in a normal shell pane) is only tracked **if the hooks plugin is
installed** for that CLI. If the user never opted in, or uninstalled the hooks,
the session is invisible.

This spec adds a **file/process watcher as a pure fallback** that fills exactly
that hole, for all four CLIs, **without changing anything about the hook path**.
The design principle is one sentence:

> **A real hook owns a session outright; born-bound owns only its binding; the
> watcher supplies status when hooks are absent; and the paths never
> double-track.**

The C++ side is unchanged — this is entirely a `wta` (Rust) addition.

## Background: Class A / Class B

IT classifies every session by `SessionOrigin` (`agent_sessions.rs`):

- **Class A — `AgentPane`**: an ACP session IT created for an agent pane. Bound
  via ACP `session/new`; activity comes from the ACP stream. Never depends on
  hooks. **Out of scope here** — the watcher never tracks Class A.
- **Class B — `Unknown`**: an agent CLI running in an ordinary shell pane. On
  `main`, Class B is tracked by:
  - **born-bound** when IT launched it (`?<prompt>` delegation, recommended
    opens, `/sessions` resume) — see the companion spec; or
  - **hooks** when the user typed it *and* the CLI has `wt-agent-hooks`.

## Goals

- Never produce a duplicate, a ghost, or a wrong-pane row when hooks **are**
  present — the watcher must be a no-op for any session a hook owns.
- Never surface a session that is not actually running in **this** IT window
  (the four CLIs write their session state to per-user roots shared by VS Code,
  language servers, other terminals, and other IT windows).

## Non-goals

- Replacing or modifying the hook path, the born-bound path, or any C++ code.
- Tracking Class-A agent panes (ACP already does).
- Perfect, instant fidelity. The watcher is a *fallback*; a few seconds of lag
  or a missed transient state is acceptable. We deliberately avoid adding
  polling sweeps or CLI-specific heroics to chase the last 1%.

## Principle: hooks own, born-bound binds, the watcher fills the gap

```
   real hook / ACP event   ──►  `hook_owned`  ──► watcher fully suppressed
   #266 born-bound              `born_bound`   ──► watcher supplies STATUS only
   (delegate / resume)     ──►                     (never re-binds)
                    wta-master registry (one row per session)
```

All paths feed the **same reducer** and the **same registry rows**. Two
master-side sets are the seam (see *Dedup*): `hook_owned` (a real hook / ACP
agent-pane event owns binding **and** activity → watcher dropped) and
`born_bound` (WTA-launched delegate/resume — binding only → the watcher may
still supply **status**). Everything runs always; there is no "hooks mode" vs
"watcher mode" switch and no install-state probing.

## Solution design

### The watcher (discovery + activity)

`session_watcher/*` is a [`notify`](https://crates.io/crates/notify)-based
recursive file watcher over the four CLIs' session-state roots:

| CLI     | Root watched                       | Session key |
|---------|------------------------------------|-------------|
| Copilot | `~/.copilot/session-state/`        | state-dir name (uuid) |
| Claude  | `~/.claude/projects/**/*.jsonl`    | jsonl stem (uuid) |
| Codex   | `~/.codex/sessions/**/*.jsonl`     | rollout id (last 5 hyphen groups) |
| Gemini  | `~/.gemini/tmp/**/*.json`          | chat id |

It is **event-driven**: a single `for res in raw_rx` loop reacts to create /
modify events (`process_change`). There is **no periodic sweep** — an earlier
3-second hot-set sweep was removed once the watcher became a fallback, to keep
the cost proportionate. At startup, `seed_existing_progress_in` records the
files already on disk so a fresh master doesn't replay the user's entire history
as "new activity".

Each change is handed to the per-CLI classifier
(`classify_{copilot,codex,claude,gemini}.rs`), which decides whether the file
represents a real, top-level user session and what activity state it implies.
Codex subagent rollouts (`multi_agent_v1` / `spawn_agent` forks, identified by
`source.subagent` in the rollout `session_meta`) are **skipped** — they inherit
the parent's history and would otherwise appear as a duplicate row with the same
title.

### Dedup: how the watcher coordinates with hooks and born-bound

The master keeps **two disjoint** ownership sets (`master/mod.rs`):

- `hook_owned: Mutex<HashSet<SessionId>>` — sessions a **real** producer owns
  outright (native CLI hooks, ACP agent-pane events). Owns binding **and**
  activity.
- `born_bound: Mutex<HashSet<SessionId>>` — #266 **born-bound** sessions
  (WTA-launched delegate `?<prompt>` and `/sessions` resume). These provide a
  pane binding but **no activity** — binding-only.

`handle_session_hook` routes each inbound event: a binding-only event (the
dedicated `intellterm.wta/session_born_bound` method, or a
`ResumeDispatched`/`ResumePaneAssigned` resume-binding event) → `born_bound`
(and drops any stale `hook_owned` claim — see below); anything else (a real
hook / ACP event) → `hook_owned` (and, if the session was born-bound, drops it
from `born_bound` — a real hook **takes over**).

The two sets are disjoint **in both directions**. A born-bound event means WTA
has just (re)launched that session id, so an ownership claim left by an earlier
generation of the same id is over. Without the reverse removal, resuming a
session that had already run once in the same master process left it in
`hook_owned` forever; since `apply_watcher_event` checks `hook_owned` first,
every watcher status event for the resumed row was dropped and the row sat at
`Idle` for its whole life. A real hook re-claims ownership on its very next
event, so nothing is lost when hooks are working.

Watcher processing then, in order:

1. `hook_owned.contains(sid)` → **drop** (the hook owns binding and activity);
2. `born_bound` → **apply status only**, never re-bind;
3. anything else → drop rather than guess.

There is no ordering requirement and the row identity is the same session id
throughout, so no duplicate is ever produced.

### Born-bound activity fallback

Born-bound (see [wta-launched-cli-session-binding.md](./wta-launched-cli-session-binding.md))
registers a WTA-launched session's `(session id → pane)` at launch and records
it in `born_bound`. It supplies the **binding** but emits **no activity**. With
hooks installed the CLI's hook supplies activity (and takes over); with **no**
hooks the row would otherwise sit at `Idle` forever.

Because born-bound hands us the **exact** session id, the watcher already knows
which transcript to read — the binding ambiguity that limits the *typed*
Claude/Gemini path doesn't apply. So for a `born_bound` session
`apply_watcher_event` applies the watcher's **status** event (Working / Idle /
Attention) to the existing row **without** re-binding the pane or touching the
origin — `emitted.event` is always a keyed status event (`ToolStarting` /
`ToolCompleted` / `Notification`), never a `SessionStarted`, so the binding is
safe. The liveness gate and `ensure_watched_session_row` are skipped (born-bound
already owns the live, vetted binding).

This covers all born-bound CLIs (**Copilot / Claude / Gemini**); Codex has no
`--session-id`, is never born-bound, and is naturally excluded.

**Resume.** `/sessions` resume publishes `ResumeDispatched` / `ResumePaneAssigned`
over the generic `session_hook` method (not the born-bound method). These are
the hook-free resume binding, so `handle_session_hook` records them in
`born_bound` rather than `hook_owned` — without this, a resumed session would be
treated as hook-owned and its row would sit at `Idle` forever even as the watcher
saw activity.

**Resume pane ownership.** `ResumePaneAssigned` marks the row's pane binding
`born_bound_pane` (`session_registry.rs`). WTA creates the resume pane and binds
it *before* the agent CLI starts, so that pane belongs to exactly one session
id. Copilot's `--resume` boots a throwaway bootstrap session and only switches
to the requested one seconds later, so its deferred `SessionStart` hook reports
the **bootstrap** id against the resumed pane's GUID. Master's `SessionStarted`
reducer therefore refuses the `active_by_pane` handoff when the pane's current
owner is a live born-bound row with a different key: the incoming session is
still recorded, it just gets no pane binding. Without the guard the resumed row
was demoted to `Ended`, and because terminal-state rows refuse resurrection it
stayed there for the rest of the CLI's life — the watcher's status fallback
silently dropped every event. The flag clears itself whenever the row gives up
the pane (`SessionStopped`, `PaneClosed`, `end_entry`) or once a `SessionStarted`
for the *same* key claims it, so the protection needs no timer.

### Liveness gate: scoping to this IT window

The four CLIs write their session state to **per-user** roots, so the watcher
sees *every* such session on the machine — VS Code's copilot, the
copilot-language-server, agent CLIs in other terminals, and sessions in **other
IT windows**. Surfacing those would pollute this window's list (observed: two
"Idle" copilot rows appearing before the user opened anything).

The gate (`watcher_row_allowed` + `live_it_pane_guids`) admits a watcher session
only when its **bound pane is a live pane in *this* IT instance**:

- `live_it_pane_guids` walks `list_windows → list_tabs → list_panes` over the
  COM `IProtocolServer` channel, lowercases every pane `session_id`, and caches
  the set for 2 seconds. It returns `None` when there is no WT channel (tests,
  detached master), in which case the gate is permissive.
- `watcher_row_allowed(pane, Some(set))` = `pane` is `Some` **and** in `set`.

> **Implementation note (bug class to remember):** the COM JSON returns
> `window_id` / `tab_id` as **numbers** (`"window_id": 1`), so the walk must
> match `String | Number`. An earlier `as_str()`-only extraction skipped every
> window, produced an empty live set, and silently rejected **all** watcher
> sessions. The cross-shape match is load-bearing.

The gate runs **only** when a row is being created (`None`) or revived from a
terminal state (`Historical` / `Ended`); already-live rows skip it so a chatty
session doesn't re-walk COM on every keystroke.

### Title resolution (and the codex AGENTS.md fix)

> **Superseded.** The on-disk title scan described below was removed with the
> rest of the disk scanner: titles now come from ACP `session/list`
> (`SessionInfo::title`, mapped in `session_history.rs`, with
> `session_history::short_id` as the fallback label). Only the codex
> subagent-fork check survives, as
> `session_watcher::classify_codex::record_is_subagent_meta`. The rationale
> below is kept for history — every other file, function, and helper it
> names (`history_loader.rs`, `try_refresh_title_from_disk`,
> `lookup_title_for_session`, `codex_title_from_file`,
> `codex_user_text_is_synthetic`, `codex_session_has_real_content`) no
> longer exists in the tree, so do not go looking for it.

A watcher row is created with a **synthetic** title (cwd basename, or empty),
then upgraded from the CLI's on-disk artefacts by `try_refresh_title_from_disk`
→ `lookup_title_for_session`, the **same** disk-title path the hook and
born-bound rows use:

- Copilot → `workspace.yaml` `summary:` (fallback `name:`)
- Claude / Gemini → first real user message in the jsonl
- Codex → `codex_title_from_file` (first non-synthetic user turn)

Because the path is shared, a latent codex bug affected **all three origins**,
not just the watcher: codex auto-loads `AGENTS.md` when the cwd has one and
prepends it as a synthetic user-role record headed by codex's
`# AGENTS.md instructions` marker *before* the user's first prompt. The marker
has two forms (codex `UserInstructions::body`): `# AGENTS.md instructions for
<dir>` for a project `AGENTS.md` (`directory = Some`) and the bare
`# AGENTS.md instructions` for a global `~/.codex/AGENTS.md`
(`directory = None`). The old scanner skipped only `<environment_context>`, so
it titled the session with that doc heading instead of the prompt.

The fix is a shared `codex_user_text_is_synthetic` helper
(`history_loader.rs`) that recognises codex's injected blocks —
`<environment_context>`, `<user_instructions>`, `<subagent_notification>`,
`<turn_aborted>`, and the `# AGENTS.md instructions` heading in **both** its
`for <dir>` and bare forms (matched only as a whole heading line so a real
prompt that merely opens with the phrase isn't swallowed) — and is used by
**both** the title scan (`codex_title_from_file`) and the phantom-session check
(`codex_session_has_real_content`, so a never-prompted codex opened in an
`AGENTS.md` repo is correctly treated as empty rather than surfaced with a doc
title).

### Components & files

| Concern | File(s) |
|---------|---------|
| Watcher loop, roots, seed | `tools/wta/src/session_watcher/mod.rs` |
| Per-CLI discovery / classify | `session_watcher/{discover,classify_copilot,classify_codex,classify_claude,classify_gemini}.rs` |
| Apply / ownership | `tools/wta/src/master/mod.rs` (`apply_watcher_event`, `hook_owned`, `born_bound`) |
| Born-bound registration | `session_registry.rs` (`build_born_bound_request`, `INTELLTERM_METHOD_SESSION_BORN_BOUND`), `main.rs` (`register_launched_session_with_master`) |
| Codex subagent fork detection | `session_watcher/classify_codex.rs` (`record_is_subagent_meta`) |
| User-input tool heuristic | `agent_sessions.rs` (`is_user_input_tool`) |

### Status detection (per-CLI)

The watcher maps each CLI's on-disk transcript to the same `AgentStatus` the
hook reducer uses, via three events: `ToolStarting` → **Working**,
`ToolCompleted` → **Idle**, `Notification` → **Attention**. A fresh/bound session
starts `Idle`; terminal states are `Historical` (startup history scan) and
`Ended` (pane/process gone); lock removal, pane close, or a hook lifecycle event
moves a row out of the live states.

The vertical sidebar's dedicated Agent sessions button opens live and historical
sessions in a view with its own header, search box, and close button. Closing it
returns to the live tab/pane groups and stops session refreshes, preserving the tab
search and foreground selection. The Filter flyout contains only Tab Metadata
controls; it does not switch between All tabs and Agents only. The Agent sessions
view displays the registry activity:
`Idle` (Idle), `Working` (Active), `Attention` (Waiting for input), `Error`
(Error), and both `Ended` and `Historical` as Historical, with localized labels.
Automatic Host discovery and prewarming exclude Gemini; opening this sidebar
does not start a Gemini ACP process. Explicit Gemini chat selection remains
available, and existing Gemini registry rows are still eligible for display.
This is presentation-only: the raw status, liveness, and focus/resume routing remain
unchanged. Each row has its provider's vector icon on the left, shared with the agent
pane header and tinted using the row foreground; unknown/custom providers use a
generic session icon rather than another provider's brand.
The bottom bar no longer has a Show sessions button. The existing
`Ctrl+Shift+/` binding and `openAgentSessions` action still open the agent pane's
session manager; the sidebar Agent sessions view remains a separate entry point.
Session titles use only the text before the first CR or LF. An empty first line
uses the existing missing-title fallback. The title occupies one non-wrapping
line with ellipsis; the metadata line below it is unchanged.
The second line is left-aligned as `Agent name · relative age · status` for Host
sessions and `Agent name · distro name · relative age · status` for WSL sessions,
using the provider's display name, the exact WSL distro name, and
`last_activity_at_ms`. Like the session manager,
timestamps less than seven days old use localized relative time; timestamps at
least seven days old use the UTC calendar date formatted with Windows' localized
long-date format. The display refreshes with each snapshot. Missing,
zero, or invalid timestamps display Unknown, and future timestamps display just now.
Active uses a theme-aware green success accent, Waiting for input a yellow caution
accent, and Error a red critical accent, matching the session management view.
Only the status text is accented; the provider, distro name,
age, and separators stay muted, and search matches remain highlighted. Host/WSL
location remains searchable and available for routing; Host has no extra location
label. Status-only updates preserve the provider, distro name, and age from the
latest snapshot.
The live session bound to the current terminal pane has a selected background.
This follows the active pane and its current agent-session binding, not the last
clicked row; failed activation and search do not change the displayed session.
The background reuses the current tab's selected color when one is configured,
with the same contrasting foreground, or the theme's default list selection
background otherwise. Switching tabs or panes, changing the tab color, and
refreshing the snapshot update the marker without resetting the session list.
No row is highlighted when the active pane has no matching live session.
Unselected rows keep their original container styling and inherited foreground.
The theme selection background is a separate visual shown only for the current
row; custom tab-color foreground overrides are cleared when a row loses the
marker or its container is recycled.
Missing or unrecognized states display Unknown rather than implying a historical
session. Search matches both the displayed status and the raw registry value;
the existing `live` and `history` search terms remain available. This presentation
does not change shell-session visibility, liveness classification, or focus/resume
routing. Registry-change notifications and the existing five-second snapshot
refresh update the displayed status.
Rows whose raw status is neither `Ended` nor `Historical` appear first, followed by
closed/history rows. Within each group, rows retain newest-first ordering by
`last_activity_at_ms`, the same timestamp used for relative age. This stable grouping
is applied by the sidebar when accepting each snapshot, including after session
closure or resume, and is preserved by search. The WTA CLI's time-based ordering
and other session-management views are unchanged.
For imported history the timestamp comes from ACP `session/list.updated_at`;
live registry events update it, including tool activity, notifications, and session
or pane closure. It is not a creation time or the time History was opened. Missing
timestamps sort last within their group.
Background snapshots update individual list slots rather than resetting the
collection, retaining unchanged row objects and the scroll offset. Changes to
the search query still rebuild the filtered results; periodic refreshes do not
pull the user's viewport back to the top.

Activating a History row focuses or resumes its session without leaving History
or clearing its search query. Protocol pane focus (including kept-tab restore)
preserves the sidebar view while changing the selected tab and terminal keyboard
focus. Sidebar resume creates a background tab, then explicitly focuses its newly
returned pane using the existing `focus_pane` operation. This preservation is
activation-specific: generic foreground `CreateProtocolTab` creation exits History.
Ordinary background tab creation and kept-tab focus retain their existing behavior.
Successful activation restarts History refreshes; explicit user new-tab actions
and closing History retain their existing behavior.
Activation has a separate busy state from list loading: existing rows and the search
query stay visible without the full-list loading spinner while focus/resume runs.
Repeated activation clicks are ignored until completion, and background snapshots
cannot clear the activation guard. Closing History resets that guard.

At startup, once its named pipe is ready, master checks policy and local
native agent CLI and required `npx` prerequisites, then initializes installed
Windows-host providers through the existing native-provider agent pool. Discovery
never automatically installs a native agent CLI or starts an interactive login.
The pinned Claude and Codex ACP adapters are separate from those native CLIs;
adapter cache presence is not checked. Existing `npx -y` behavior is allowed to
download and bootstrap an uncached adapter during initial startup or a later
refresh that starts a provider, so discovery may require network access.
Each provider lists its own history; no helper or chat session is created.
Connections stay warm for the lifetime of master,
including while History is closed. Discovery is asynchronous and single-flight across
windows, so slow or failed providers do not block existing rows. History synchronization
preserves live status and pane bindings. WSL/custom sessions already known to the registry
are still displayed, but this pass does not start WSL distros or unknown custom commands.
Sidebar snapshots use `--all-agents` to refresh the same resident pool; opening History
is not required to establish these connections or load the initial histories.

- **Claude** (`classify_claude.rs`) — **turn-based, keyed on `stop_reason`**.
  Claude re-writes the same assistant message id several times as it streams
  (text first, then `+tool_use`), so classifying by content presence flickers;
  `stop_reason` is stable across the stream. A `type:user` record (typed prompt
  or `tool_result`) → Working; an assistant `stop_reason == "tool_use"` →
  Working, unless a `tool_use` names a user-input tool (`AskUserQuestion`) →
  Attention; any other `stop_reason` (`end_turn`, …) → Idle.
- **Copilot / Codex** (`classify_copilot.rs` / `classify_codex.rs`) —
  **turn-based**, not tool-based. One user prompt drives one or more agent
  *turns*; the agent is Working for the whole turn (thinking + streaming text +
  tool runs), so Working is bracketed by the turn boundary
  (`assistant.turn_start` → `assistant.turn_end` for Copilot,
  `event_msg/task_started` → `task_complete` for Codex), not by the brief
  tool-execution windows. Tool starts only refine the picture (current tool, or
  Attention for a user-input tool); the tool-completion record is ignored
  because Idle is owned by the turn end. An explicit permission/escalation
  record (`permission.requested` for Copilot, sandbox `require_escalated` for
  Codex) → Attention.
- **Gemini** (`classify_gemini.rs`) — **Working-only; turn-based Idle deferred.**
  Gemini's `session-*.jsonl` is an **append log** (re-verified 2026-06-14): every
  line is a standalone `{"id","type":"user"|"gemini",…}` record or a `$set` op,
  so the watcher reads it by byte offset like the other three CLIs. Each `gemini`
  message is appended **twice** under the same id (phase 1 text/thoughts, phase 2
  `+toolCalls`), interleaved with `$set:lastUpdated` bumps; a full
  `$set:{messages:[…]}` snapshot is written only at session **start** and
  **resume**. `classify_record` **skips every `$set` op** — crucially the resume
  snapshot, which would otherwise replay the entire prior conversation — and maps
  each activity record to **Working**: a `type:user` record (typed prompt *or* a
  `functionResponse` tool result), a `type:gemini` text record (phase 1 / final
  answer), or a `type:gemini` with `toolCalls` (tool name surfaced; a user-input
  `ask_user` → **Attention** instead). It **never emits Idle**: Gemini writes no
  turn-completion signal (no `stop_reason`/`finishReason`), and a `toolCall`
  carrying a `result` does **not** mean the turn ended — so a `gemini`-without-
  `toolCalls` line is ambiguous (intermediate text vs final answer). A Gemini row
  therefore stays **Working** through the conversation and only leaves the live
  state on `PaneClosed`; a clean turn-based Idle is **deferred** (needs a
  turn-end marker Gemini doesn't write, or hooks).

**Limitation — permission / ask-for-input prompts.**

- **Claude.** A tool that pauses for **permission** (e.g. `Bash`/`Edit` in
  `default` mode) is **indistinguishable** from a tool that is merely running —
  there is no approval/pending marker (only `permissionMode`). So a Claude
  permission wait shows as **Working**, not Attention; only an explicit
  user-input tool (`AskUserQuestion`) is Attention. (A `dangerous-tool →
  Attention` name heuristic was considered and rejected: it is wrong under
  auto-approve and conflates a running write-tool with a wait.)
- **Gemini.** The transcript is written **post-completion** — every on-disk
  `toolCall` is `status:"success"` with its `result` inlined, and an `ask_user`
  record already contains the user's answer. So the `ask_user` line lands only
  *after* the user replied; during the actual wait the file is silent and the
  last record (the agent's phase-1 text) shows **Working**. The `ask_user` →
  Attention mapping is kept (and is correct if a future Gemini build writes a
  pending state), but in today's transcript it is typically superseded by the
  immediately-following result record → the wait effectively shows Working,
  same class of limitation as Claude. Reliable wait-state Attention needs hooks.
- **Copilot / Codex** are **not** affected — they write explicit
  `permission.requested` / `require_escalated` records that map to Attention.

## What is explicitly unchanged

- The native hook path and the `intellterm.wta/session_hook` reducer
  semantics. (Born-bound now uses its own `…/session_born_bound` method so it can
  be treated as binding-only — see *Dedup* — but the wire body is identical.)
- All C++ (FRE "Install hooks", Settings UI, `ConptyConnection`,
  `agent_hooks_installer`, the four hook bundles).
- Class-A agent-pane tracking.
- The `/sessions` UI and its 5 s re-poll.

## Edge cases & failure modes

- **Hooks installed mid-session**: the first hook event marks the session
  `hook_owned`; the watcher row (if any) is adopted by the hook from then on,
  same session id, no duplicate.
- **`notify` miss**: a dropped FS event means a late or missing appearance; the
  fallback nature makes this acceptable, and the startup seed bounds the blast
  radius after a restart.

## Capabilities

- **Security / Privacy**: reads only the user's own CLI session-state files; no
  new network or cross-user access.
- **Reliability**: every mapping failure degrades to "no row" rather than a
  wrong row.
- **Performance**: event-driven with no polling sweep.
- **Compatibility**: additive; with hooks installed, behaviour is identical to
  `main` (watcher events are all deduped).

## Testing

- Unit: `master::tests` (`watcher_event_*`, `watcher_row_allowed_*`,
  `live_it_pane_guids_*` — incl. numeric `window_id`/`tab_id` mock, `reap_*`,
  `session_hook_marks_*`; born-bound: `session_born_bound_marks_born_bound_not_hook_owned`,
  `born_bound_session_gets_watcher_activity_without_rebinding`,
  `real_hook_takes_over_born_bound_session`, `resume_binding_events_are_born_bound_not_hook_owned`),
  `classify_codex::tests` (codex subagent fork detection), and
  `session_watcher` discovery/classify tests (incl. `classify_claude` turn-based:
  user→Working, `stop_reason` end_turn→Idle / tool_use→Working, streaming-partial
  stays Working, AskUserQuestion→Attention).
- Manual matrix:
  - hooks installed → row tracked by hook; master log shows watcher events
    deduped.
  - hooks uninstalled → a **delegate** (`?<prompt>`) and a **resumed** Claude
    session show live status (Working/Idle/Attention) from the watcher, not a
    frozen `Idle`.

## Diagnostics

`wta-main_master*.log` (`target: "session_watcher"`):

- `refreshed live IT pane set panes={…}` — the COM-walked live pane set.
- `watcher liveness gate decision … resolved_pane=… gated=… live_pane_count=…
  allowed=…` — per-session admit/withhold.
- `upgraded synthetic title from on-disk session artefacts … title_len=…` —
  title resolution (a 69-char codex title was the AGENTS.md regression).

## Rejected / deferred alternatives

- **Hookless for all four CLIs (the original #258 approach)** — rejected:
  Claude/Gemini binding is too ambiguous and codex's RM binding is fragile, so
  hooks must stay authoritative. This spec is the salvaged *fallback* half of
  that work.
- **Polling sweep for perfect liveness** — rejected: the watcher is a fallback,
  and an idle process scan is disproportionate.
- **Gemini turn-end → Idle** — **deferred** (see *Status detection*): Gemini's
  transcript has no turn-completion signal and a 2-phase / `$set`-interleaved
  shape, so the *end* of a turn can't be told from the log. Gemini already shows
  live **Working/Attention** (read per record from the append log); only the
  turn-end → Idle transition awaits hooks or a cleaner Gemini format.
- **`dangerous-tool → Attention` heuristic** — rejected: a permission wait is
  indistinguishable from a running tool in the transcript, and the heuristic is
  wrong under auto-approve.

## Future considerations

- A stronger Claude/Gemini bind (e.g. a CLI-provided pid file) would let the
  watcher cover those two as confidently as Copilot/Codex.
- A reliable Gemini turn signal (a `finishReason`, or a stable per-message
  completion marker) would let Gemini join the turn-based status model.
