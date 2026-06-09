# Hookless agent session tracking (Class-B, hook-free)

## Abstract

Replace the `wt-agent-hooks` plugin mechanism — today the *only* way Windows
Terminal observes agent CLI sessions the user runs **directly in a shell pane**
("Class B") — with a hook-free, zero-install subsystem inside `wta-master`.

The new subsystem has two pillars driven by one design:

1. **Discovery + Activity (file-driven).** A filesystem watcher over each CLI's
   on-disk session files derives session existence and live activity
   (Working / Idle / Error / partial Attention) by classifying the records each
   CLI already writes.
2. **Binding + Liveness (process-driven).** Each session is bound to its hosting
   WT pane by an **exact pid→session_id link** — Copilot via its
   `inuse.<pid>.lock`, Claude/Gemini/Codex via reading `*_SESSION_ID` from the
   owning process's environment block (PEB) — then pid→pane via a parent-chain
   walk against `wtcli list-panes`.

Both pillars terminate in the **same `SessionEvent`s** the hook path produces
today, so the entire downstream — session registry, `master→helper` mirror, C++
session-management UI, Enter routing — is **unchanged**. This is a *producer
swap*, not a redesign. Removing hooks then deletes the plugin bundle, the
per-CLI installer, and the marketplace auto-upgrade machinery.

## Inspiration

The hook bundle (`wt-agent-hooks/{copilot,claude,gemini-extension,codex}`) is a
large, fragile maintenance surface:

- It must be **installed into each agent CLI** (Copilot/Claude marketplace
  registration, Gemini extension install), re-run on every `wta` startup, with
  install paths that change on every MSIX version bump — hence a dedicated
  auto-upgrade subsystem (`agent_hooks_installer`, `upgrade_installed_hooks`,
  `hooks-upgrade-state.json`).
- It is **opt-in**: until the user accepts installation, Class-B sessions are
  invisible to WT.
- It spawns **`powershell.exe` + `wtcli.exe` per agent event**.
- Copilot **ACP mode bypasses plugin hooks** entirely (documented caveat), so
  Copilot agent panes never relied on them.
- Per-CLI hook vocabularies drift and must be tracked against each CLI's docs.

A file + process observer removes all of that — **nothing is installed into the
CLIs**, and the 5-second-polling race the original framing feared is largely
eliminated: Copilot and Codex bind exactly, while Claude and Gemini bind by cwd
with only a narrow *same-cwd* residual race (far smaller than a polling window).

### Non-goals

- **Class A** (agent panes WTA itself spawns): already bound via ACP
  `session/new`, already gets activity from ACP `session/update`. Untouched.
- **Autofix** (OSC-133 pipeline): a separate path. Untouched.
- **The generic `SendEvent`/`send_event` COM bus**: stays. It carries many
  non-hook routes (`autofix_state`, `agent_status`, `restart_agent_stack`,
  `close_agent_pane`, …). Only the hook *producer*
  (`send-event.ps1` → `wtcli send-event -e agent.*`) is removed.

## Decisions (settled in design review)

1. **Preserve live activity hook-free**, not just the binding. Source = each
   CLI's on-disk session `.jsonl`.
2. **Attention is partial.** Only explicit user-input tool calls
   (`ask_user`-family — see `is_user_input_tool`) raise Attention. The CLI
   permission-gate ("approve command? `[y/n]`") stays `Working`, because the CLI
   blocks on stdin and writes **no discrete record** before the prompt is
   answered, so the file cannot distinguish it from a running tool. No
   pane-buffer scraping and no timeout heuristics (explicitly rejected for
   simplicity / fidelity reasons).
3. **Binding is per-CLI** (finalized after a live prototype on 2026-06-08 —
   see [Verification](#verification-spike-results-2026-06-08); the earlier
   "race-free via PEB `*_SESSION_ID` for all of Claude/Gemini/Codex" assumption
   was disproven):
   - **Copilot** → `inuse.<pid>.lock` filename → pid (**exact**).
   - **Codex** → Windows **Restart Manager** `RmGetList(rollout)` → owning pid
     (**exact**; Codex holds the rollout file open for the whole session).
   - **Claude / Gemini** → **cwd correlation** (the session file's path-encoded
     cwd ↔ a candidate process's working directory, paired by start /
     file-creation order). Residual race only for two *same-CLI* sessions in the
     *same* cwd inside the (millisecond) watch window — far smaller than the
     original 5-second polling window.
4. **Home = `wta-master`.** The watcher feeds the session registry **in-process**
   (no COM round-trip), emitting existing `SessionEvent`s.

### Verification (spike results, 2026-06-08)

The pid↔session link was prototyped against live Copilot, Codex, Claude, and
Gemini sessions on Windows before settling Decision #3. Throwaway probes
(PowerShell + inline C# P/Invoke) established:

| CLI | `*_SESSION_ID` in its own PEB env? | Holds session file open between writes? | Chosen link |
|---|---|---|---|
| Copilot | no (only set for child procs) | no (`events.jsonl` not held) | `inuse.<pid>.lock` (exact) |
| Codex | no | **yes** (rollout held all session) | Restart Manager (exact) |
| Claude | no | no (sub-ms append-close) | cwd correlation |
| Gemini | no | no (sub-ms append-close) | cwd correlation |

Two candidate mechanisms were **tried and rejected**:

- **PEB `*_SESSION_ID`** — none of Claude/Gemini/Codex keeps the session id in
  its own process environment block. The id reaches the hook out-of-band via
  stdin JSON (`send-event.ps1` reads `session_id` from stdin), so it is not
  recoverable from the process environment. (Copilot's own session env var only
  appears in a child process, not the pane-rooted CLI process.)
- **Restart Manager at the file-create instant** — Claude/Gemini *do* briefly
  hold the file during each write (a sub-millisecond exclusive-open probe caught
  it, including at new-file creation), but Restart Manager's own startup is
  multi-millisecond, so by the time `RmGetList` runs the handle is already
  closed: it returned `(none)` on every Claude/Gemini lock-hit, versus the codex
  positive control which returned the owning pid on 68688/68688 probes. A faster
  raw handle-table snapshot (`NtQuerySystemInformation`) was considered and
  rejected — complex, carries the `NtQueryObject`-on-pipe hang hazard, and even
  the microsecond probe only caught Gemini 1-in-3.

## Solution Design

### Class A vs Class B (why this is only about Class B)

| | Class A (agent pane) | Class B (user-run CLI in shell pane) |
|---|---|---|
| Who spawns the CLI | WTA (`wta-master`) | the user |
| Binding today | ACP `session/new` returns the id | **hooks** (`agent_session_id` + `WT_SESSION`) |
| Activity today | ACP `session/update` | **hooks** (`agent.tool.*` / `stop` / `notification`) |
| After this change | unchanged | **the new watcher** |

Class B is the entire reason hooks exist. This spec replaces it; Class A is
left alone.

### Pillar 1 — Discovery + Activity (file-driven)

A filesystem watcher (`notify` crate, backed by `ReadDirectoryChangesW`) over
the per-CLI session roots:

```
Copilot     : ~/.copilot/session-state/<UUID>/events.jsonl
Claude      : ~/.claude/projects/<encoded-cwd>/<UUID>.jsonl
Gemini      : ~/.gemini/tmp/<slug>/chats/session-*.jsonl          (sunset 2026-06-18)
Codex       : ~/.codex/sessions/YYYY/MM/DD/rollout-<iso-ts>-<UUID>.jsonl
Antigravity : ~/.gemini/antigravity-cli/brain/<UUID>/.system_generated/logs/transcript.jsonl   (deferred — see Antigravity section)
```

- **A new session file** (or the first *real* record in one) ⇒ `SessionStarted`
  (id from the path; cwd from the path / first record).
- **Appended records** ⇒ a per-CLI classifier maps them to the existing
  `SessionEvent`s: `ToolStarting`, `ToolCompleted`, `Stop`, `Error`,
  `Notification` (only for `ask_user`-family tool calls).
- The per-CLI file layouts and "is this a real vs. phantom record" rules already
  exist in `history_loader.rs` (which reads these same files for the
  *Historical* rows). The classifier is the **live-tail counterpart** of that
  loader and should share its per-CLI parsing helpers, not re-derive them.

**Flush dependency (Spike B) — verified incremental on 2026-06-08/09.** All four
CLIs write tool records *during* the turn, not batched at turn-end, so live
`Working`/`Idle` is observable. The per-CLI record model and read strategy:

| CLI | On-disk model | Tool-start record | Tool-end record | Watcher read |
|---|---|---|---|---|
| Copilot | append-only `events.jsonl` | `tool.execution_start` | `tool.execution_complete` | tail appended lines |
| Claude | append-only `<id>.jsonl` | `assistant` w/ `tool_use` | `user` w/ `tool_result` | tail appended lines |
| Codex | append-only rollout | `response_item` payload `function_call` | `function_call_output` (+ `task_complete`) | tail appended lines |
| Gemini | **running `$set.messages` snapshot (rewritten in place)** | `gemini` msg w/ `toolCalls` | `user` msg w/ `functionResponse[]` | **re-parse latest `$set.messages` on change, diff by message count** |
| Antigravity *(deferred)* | append-only `transcript.jsonl` | `PLANNER_RESPONSE` w/ `tool_calls[]` | tool-type record (`status: DONE`/`ERROR`) | tail appended lines |

> **Gemini is the one real caveat.** Its session file is not a clean append log:
> each turn rewrites a trailing `{"$set":{"messages":[…]}}` snapshot in place, so
> a byte-offset tail (correct for the other three) silently misses the rewritten
> records. The Gemini classifier must re-read the file on every change and diff
> the `messages` array by length, deriving `Working` from a trailing message that
> carries `toolCalls` with no matching `functionResponse` yet.

Evidence (record-level tail of live turns): Claude appended interleaved
`tool_use`/`tool_result` across 33 s with distinct timestamps (last `tool_use`
landed before its `tool_result`); Codex wrote `function_call name=shell_command`
at tool-request time, then `task_complete` with the shell output at turn end;
Gemini wrote a `gemini` message carrying `toolCalls` then a `user` message with
`functionResponse[]` during the turn; Copilot emits discrete
`tool.execution_start`/`tool.execution_complete` records (99/98 in this session).

### Pillar 2 — Binding + Liveness (process-driven)

For each discovered session, resolve **process → pane**:

1. **session_id ↔ pid (per CLI — verified 2026-06-08, see
   [Verification](#verification-spike-results-2026-06-08)):**
   - **Copilot (exact):** scan `~/.copilot/session-state/<UUID>/` for
     `inuse.<pid>.lock` ⇒ pid. (`history_loader` already knows this marker;
     today it only *skips* it.)
   - **Codex (exact):** Codex holds its rollout `.jsonl` open for the whole
     session, so the Windows **Restart Manager** API
     (`RmStartSession` → `RmRegisterResources` → `RmGetList`) returns the owning
     pid for the watched file directly. Preferred over raw handle-table
     enumeration: documented API, no `NtQueryObject`-on-pipe hang hazard.
   - **Claude / Gemini / Antigravity (cwd correlation):** none keeps a
     session-id env var in its PEB, and none holds the session `.jsonl` open
     between writes, so there is no exact file/env link. Bind by matching the
     session file's path-encoded cwd (Claude `projects\<encoded-cwd>\`, Gemini
     `tmp\<slug>\chats\`, Antigravity via the conversation's project mapping) to
     a candidate CLI process's working directory, paired by start-time /
     file-creation order. (Antigravity additionally runs one `agy.exe` per pane,
     which narrows the candidate set to a single process — see the Antigravity
     section. *Antigravity is a deferred phase — see its section.*)
2. **pid → pane (trivial — every CLI process carries the pane GUID):** read
   `WT_SESSION` straight from the bound process's PEB environment
   (`ProcessParameters.Environment`) ⇒ the pane GUID. Verified present in every
   CLI process probed (Claude, Gemini, Codex, Copilot, `agy.exe`). WT injects
   `WT_SESSION` into each pane's shell and it is inherited by every descendant,
   so the **hard part of binding is only step 1 (file-id → pid); step 1's pid
   then yields the pane for free.** Parent-chain walk
   (`NtQueryInformationProcess(...).InheritedFromUniqueProcessId` /
   `Toolhelp32Snapshot.th32ParentProcessID` up to a `wtcli list-panes` pid) is
   the fallback when a process's own `WT_SESSION` is somehow missing, and also
   yields `owner_tab_id` / `window_id`.
3. **Fallback** (Copilot lock absent / Codex RM returns `(none)`): same cwd +
   start/creation-order correlation as Claude/Gemini/Antigravity. Carries a
   residual race only when two *same-CLI* sessions start in the *same cwd* inside
   the (millisecond) watch window.

**Liveness:** owning process alive ⇒ `Live`; process exit (or `inuse.lock`
removal) ⇒ `SessionEnded`. Detected via a low-cadence reconcile and/or a process
handle wait.

**Window scoping:** only sessions whose process descends from a pane owned by
*this* `wta-master`'s window(s) are bound and published; events carry
`window_id` + `owner_tab_id`, matching the existing per-tab/per-window routing
(see `Multi-window-agent-pane.md` §7).

### Antigravity CLI (`agy.exe`) — Gemini CLI's successor

> **Scope: documented, deferred — NOT in the initial implementation.** The
> findings below are recorded from a 2026-06-09 live probe so the work is
> ready-to-pick-up, but Antigravity support ships **later**, gated on WTA's ACP
> layer gaining Antigravity support (Class A). Rationale: Antigravity's ACP
> availability is still unsettled at launch (Gemini→Antigravity migration), and
> WTA can't host it as an agent pane until that lands — so the Class-B watcher
> for Antigravity is deferred to be built together with the Class-A ACP work.
> **Initial implementation covers Copilot, Claude, Codex, and Gemini** (Gemini
> until its 2026-06-18 sunset).

Google is **retiring Gemini CLI for non-enterprise users on 2026-06-18** and
steering them to **Antigravity CLI** (`agy`). The migration was probed live on
2026-06-09; Antigravity turns out to be *more* amenable to this hookless design
than gemini-cli, so when it is picked up it will be a first-class fifth CLI:

| Aspect | Finding |
|---|---|
| Binary / process | `%LOCALAPPDATA%\agy\bin\agy.exe` — a single Go binary, **one `agy.exe` per pane**, rooted directly in the pane shell (`agy.exe → pwsh → WindowsTerminal.exe`). |
| Session file | `~/.gemini/antigravity-cli/brain/<conversation-uuid>/.system_generated/logs/transcript.jsonl` — plain, **append-only** JSONL (grows monotonically; byte-tail works, unlike gemini-cli's snapshot rewrite). |
| Record shape | `{step_index, source, type, status, created_at, content, tool_calls[], thinking}`. `source` ∈ {`USER_EXPLICIT`, `MODEL`}; `type` ∈ {`USER_INPUT`, `PLANNER_RESPONSE`, `LIST_DIRECTORY`, `VIEW_FILE`, `GREP_SEARCH`, …}; `status` ∈ {`DONE`, `ERROR`, …}; tool-start = a `PLANNER_RESPONSE` carrying `tool_calls:[{name,args}]`. |
| session id | the `<conversation-uuid>` in the path. |
| Binding | `agy.exe` carries `WT_SESSION` in its PEB (pane GUID, free); RM does **not** hold the transcript, so conversation→pid uses cwd correlation, narrowed by one-`agy`-per-pane. |
| Liveness | `agy.exe` alive ⇒ `Live`. |
| Ignore | the parallel proto + SQLite "trajectory store" (`brain/.../*.pb`, encrypted-looking) — the readable `transcript.jsonl` is sufficient. |

When picked up, Antigravity needs a `classify_antigravity.rs` (append-tail of
`transcript.jsonl`, mapping `tool_calls` / `status` to
`ToolStarting`/`ToolCompleted`/`Error`) plus one extra watched root. **Open item
for that phase:** a long-running tool's in-flight `status` (e.g. `RUNNING`) was
not captured (the probe turn completed too fast); confirm the live-`Working`
signal with a slow tool before implementing. **gemini-cli is kept in the initial
scope** (still works until 2026-06-18 and for enterprise) but its row is a sunset
path; Antigravity is the go-forward Google CLI for the deferred phase.

### The invariant that keeps the change contained

Both pillars terminate in `route_agent_event_to_registry` (or a sibling emitting
identical `SessionEvent`s) against the **existing** `AgentSessionRegistry`.
Everything after is untouched:

```
                         (REMOVED producer)
  agent CLI ── hook ──▶ send-event.ps1 ──▶ wtcli send-event ──▶ COM ──▶ wta
                                                                          │
  ┌───────────────────────── NEW producer (in wta-master) ───────────────┤
  │  session-file watcher ─┐                                              │
  │  proc/PEB binder ──────┼─▶ SessionEvent ─▶ route_agent_event_to_registry
  └────────────────────────┘                                             │
                                                                          ▼
                       AgentSessionRegistry ── unchanged downstream ──────▶
            (intellterm.wta/session_added|removed mirror, helper session
             view, Enter/Shift+Enter routing, `wta sessions list`)
```

### Components & files

**New** (`tools/wta/src/`):

| Module | Responsibility |
|---|---|
| `session_watcher/mod.rs` | `notify` watcher over the four roots; debounce; dispatch new-file / changed-file events (byte-tail for Copilot/Claude/Codex, full re-parse + message-array diff for Gemini's rewritten snapshot) |
| `session_watcher/classify_{copilot,claude,gemini,codex}.rs` (+ `antigravity`, deferred) | record → `SessionEvent`; reuse `history_loader` per-CLI helpers. Gemini classifier reads the trailing `$set.messages` snapshot (not appended bytes); Antigravity (later phase) tails `brain/<uuid>/.system_generated/logs/transcript.jsonl` |
| `proc_bind.rs` | `inuse.<pid>.lock` reader (Copilot), Restart Manager owner query (Codex), PEB working-directory reader for cwd correlation (Claude/Gemini), parent-chain → pane walk (Win32: `RmStartSession`/`RmRegisterResources`/`RmGetList`, `NtQueryInformationProcess`, `ReadProcessMemory`, `Toolhelp32Snapshot`) |
| wiring in `master/mod.rs` | own the watcher + binder, feed the registry |

**Deleted:**

- `tools/wta/wt-agent-hooks/**` (the whole bundle, incl. `send-event.ps1`).
- `tools/wta/src/agent_hooks_installer.rs` (+ its tests + the `include_str!`
  blobs + `upgrade_installed_hooks` + `hooks-upgrade-state.json` logic).
- Hook env plumbing: `WTA_HOOK_LOG_DIR` / `WTA_CLI_SOURCE` set by
  `ConptyConnection` (C++) and `protocol/acp/spawn.rs`.
- Hook docs (`wt-agent-hooks/README.md`; references in `AGENTS.md` / copilot
  instructions).

**Kept (corrected):** the `SendEvent`/`send_event` COM bus and method. If hooks
were the *only* caller of the `wtcli send-event` **subcommand**, that subcommand
may also be removed — to be verified during implementation; the COM `SendEvent`
method itself stays for the other routes.

### Activity-state mapping

| On-disk signal | `SessionEvent` | `AgentStatus` |
|---|---|---|
| assistant `tool_use` / tool-call record | `ToolStarting{tool}` | `Working` |
| `tool_use` where tool ∈ `ask_user`-family | `Notification` | `Attention` |
| `tool_result` / matching completion | `ToolCompleted` | `Working`→`Idle` |
| turn-end record | `Stop` | `Idle` |
| error / failure record (or unclean exit) | `Error` | `Error` |
| owning process exits | `SessionEnded` | (liveness → `Ended`) |
| CLI permission-gate prompt (`y/n`) | — none — | stays `Working` (accepted gap) |

## UI/UX Design

No visible UI change. The session-management view, Enter/Shift+Enter routing, and
`wta sessions list` render Class-B rows exactly as today. The single behavioral
difference: a Class-B session blocked on a permission `y/n` prompt shows
`Working` instead of `Attention`.

## Capabilities

### Accessibility

No UI surface changes; screen-reader / assistive behavior is unaffected.

### Security

- **PEB read** opens same-user processes with
  `PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ` — no elevation, cannot
  read other users' processes, and reads only the session-id env value of
  processes that are WT-pane descendants.
- **Net reduction:** removes the per-agent-event `powershell.exe` execution
  surface and the marketplace-write surface.
- Session files are user-owned untrusted input; the classifier must tolerate
  malformed / partial / truncated lines (same discipline as `history_loader`).

### Reliability

- Exact pid→session binding **eliminates the polling race** that motivated the
  design.
- Event-driven watcher latency (ms) is comparable to synchronous hooks.
- Best-effort throughout: any watcher/binder failure degrades to "no row" or a
  Historical row — it never breaks the pane or the CLI.

### Compatibility

- Class-B rows are visually identical.
- **Breaking for existing installs:** users who already accepted
  `wt-agent-hooks` will have stale plugins. The change must **uninstall** hooks
  on upgrade (reuse the installer's removal path one final time, then delete it)
  or document manual cleanup.
- **Behavioral gap:** permission-gate Attention is no longer surfaced (accepted).

### Performance, Power, and Efficiency

- One watcher per `wta-master`; a handful of directory handles. Process
  enumeration runs only on discovery events plus a low-cadence liveness
  reconcile.
- Removes one `powershell.exe` + one `wtcli.exe` spawn **per agent event** — a
  net efficiency win under active agents.

## Potential Issues

1. **pid↔session binding — resolved (see
   [Verification](#verification-spike-results-2026-06-08)).** Copilot binds via
   its lock file and Codex via Restart Manager (both exact); Claude and Gemini
   fall back to cwd correlation. The original "exact pid→session for all four via
   PEB `*_SESSION_ID`" assumption was disproven, as was the "Restart Manager at
   file-create instant" enhancement.
2. **Spike B — incremental flush — resolved (verified incremental).** All four
   CLIs write tool records during the turn, so live `Working`/`Idle` works; see
   the Pillar 1 record-model table. Only caveat: Gemini rewrites a trailing
   `$set.messages` snapshot, so its watcher path must re-parse + diff rather than
   byte-tail.
3. **Process-tree depth.** The parent-chain walk must climb through `.cmd`
   shims / `node` / `npx` wrappers.
4. **WOW64 bitness.** The Claude/Gemini cwd-correlation path reads the target
   process's working directory from its PEB; a 64-bit `wta` reading a 32-bit
   process's PEB needs the correct layout. CLIs are 64-bit today — document the
   limit.
5. **Pre-discovery / non-WT sessions.** Sessions started before pane discovery,
   or in a non-WT terminal, are filtered out by the descends-from-pane check.
6. **Multi-window routing.** pane→window mapping must be correct so events reach
   the right window's helpers.
7. **Same-cwd residual race (Claude/Gemini).** Two same-CLI sessions started in
   the *same* working directory within the watch window can mis-pair; both
   bindings still point at valid live panes, so worst case is a swapped row.

## Future considerations

- ETW / WMI `Win32_ProcessStartTrace` subscription for lower-latency binding
  (vs discovery-triggered enumeration).
- Recover full Attention if/when a CLI begins writing a permission-request
  record to its session file.
- Reuse the live classifier to enrich `wta sessions list` titles / last-activity
  without a separate history pass.

## Resources

- `doc/specs/Multi-window-agent-pane.md` — per-tab / per-window routing (§7).
- `doc/specs/llm-agent-event-integration.md` — agent event model.
- `tools/wta/src/history_loader.rs` — per-CLI file layouts + record parsing.
- `tools/wta/src/agent_sessions.rs` — the two-axis state model events feed.
- `tools/wta/wt-agent-hooks/README.md` — the mechanism being replaced.
