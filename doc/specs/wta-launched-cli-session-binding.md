# Hook-Independent Pane ↔ Session Binding for WTA-Launched CLI Sessions

## Context & problem

Intelligent Terminal classifies agent sessions by `SessionOrigin`
(`agent_sessions.rs`):

- **Class A — `AgentPane`**: an ACP session WTA created for an agent pane. Bound
  via ACP `session/new`. WTA already records `(session_id, pane_session_id)` for
  these in `agent_pane_origin.rs` (v2 index).
- **Class B — `Unknown`**: an agent CLI (`copilot` / `claude` / `gemini` /
  `codex`) running in a normal shell pane. **On `main`, Class B is tracked by
  hooks**: each CLI's `wt-agent-hooks` plugin reports `agent_session_id` plus
  `WT_SESSION` (the pane GUID), so today's binding is exact — but it depends on
  hooks.

The wider "de-hook" effort aims to stop relying on hooks for that binding. The
easiest part to tackle first is the subset of Class-B sessions that are **not**
user-typed — the ones **WTA launches itself**:

- **(a)** `?<prompt>` delegation and the no-prompt background-agent action
  (`openBackgroundAgent`, Alt+Shift+B) → `wta delegate` opens a new tab whose
  command line is the agent's own interactive CLI.
- **(b)** Agent-pane recommendations that open a CLI in a new tab/panel
  (`RecommendedAction::OpenAndSend { agent }` / `Open { agent }`).
- **(c)** `/sessions` resume via the CLI's own resume flag (`ResumeCliFlag` → new
  pane running `<cli> --resume <key>`).

For these, WTA controls the launch, so it can bind deterministically **without
hooks** — by pinning the session id (`--session-id`) and reading the pane GUID
back from `create_tab`. This is the cleanest, most independent slice of the
de-hook work, so it ships **first, on `main`**.

## Goal

For WTA-launched CLI sessions whose agent supports `--session-id`
(**Copilot / Claude / Gemini**), establish the `(session id → pane GUID)`
binding **at launch, with no hooks**, by registering a *born-bound* session row
directly into the `wta-master` registry. Independently mergeable on `main`.

## Non-goals

- **Not removing hooks.** This branch coexists with `main`'s hooks; it only adds
  a hook-independent binding *source* for the WTA-launched, pinnable subset.
- **No new background scanning.** Binding is established synchronously at launch;
  nothing polls processes or tails session files.
- **No `SessionOrigin` / UI change.** Sessions stay `Unknown` (Class B); no
  change to `OriginFilter`, `/sessions` rendering or Enter/Resume routing,
  registry serialization, or `history_loader`.
- **Codex is out of scope.** It cannot pin `--session-id`, so it keeps using
  `main`'s existing Codex hook (`wt-agent-hooks/codex/.../send-event.ps1`)
  unchanged.
- **User-typed (non-WTA-launched) sessions** are untouched here — they stay on
  the existing hook path.

## Scope: which launches

(a) `?<prompt>` delegation + `openBackgroundAgent` (`wta delegate`).
(b) agent-pane recommendation opens (`OpenAndSend { agent }` / `Open { agent }`).
(c) `/sessions` `ResumeCliFlag` (`<cli> --resume <key>`; id known = the resume key).

For all three, **only the pinnable agents (Copilot / Claude / Gemini)**. Codex
launches are left on the existing path.

## Key enabling facts (empirically verified 2026-06-10, Windows)

**1. `--session-id <uuid>` pins a chosen id for a NEW session:**

| CLI | pins a new id? | file lands at | id recoverable from filename? |
|---|---|---|---|
| Copilot | yes | `~/.copilot/session-state/<uuid>/events.jsonl` | yes — dir name `<uuid>` |
| Claude  | yes | `~/.claude/projects/<enc-cwd>/<uuid>.jsonl` | yes — stem `<uuid>` |
| Gemini  | yes | `~/.gemini/tmp/<cwd-slug>/chats/session-<ts>-<uuid[0:8]>.jsonl` (full uuid in content) | partial — only `uuid[0:8]` in the name |
| Codex   | no | `rollout-<ts>-<uuid>.jsonl` | n/a (cannot choose the id) |

(The Gemini filename caveat is irrelevant here: we register by the full pinned
uuid — this feature never discovers the file by name.)

**2. `create_tab` / `split_pane` return the new pane's identity:**
`TabCreationResult { UInt32 TabId; Guid SessionId; UInt64 WindowId; UInt32 Pid }`
(`TerminalProtocol.idl`). WTA receives the **pane GUID (`SessionId`) and the
pane's root Pid** synchronously at launch.

## Design

### Mechanism — born-bound registration at launch

At each in-scope launch:

```
1. id  = (a,b) generate a v4 UUID  |  (c) the resume key
2. cmd = <cli> ... <new_session_id_flag> <id>          (flag from agent_registry)
3. TabCreationResult = COM create_tab / split_pane(cmd)   -> pane GUID + Pid
4. register the session with wta-master   { id, cli, cwd, pane_guid }
       -> master upserts a SessionInfo with
            pane_session_id = pane_guid,
            cli_source      = cli,
            origin          = Unknown,        (Class B — unchanged)
            status          = Idle
```

No file discovery, no hooks, no PEB read — the row is **born bound** to the pane.

> **As built:** the register step sends a `SessionStarted` over a dedicated
> `intellterm.wta/session_born_bound` ext-method (same wire body as
> `…/session_hook`). The distinct method lets the master record the session as
> *binding-only* (`born_bound`) rather than hook-owned, so the file/process
> watcher can still supply **status** (Working/Idle/Attention) for it when no
> hook is installed — without re-binding the pane. Resume's
> `ResumeDispatched`/`ResumePaneAssigned` get the same binding-only treatment.
> See [hybrid-agent-session-tracking.md](./hybrid-agent-session-tracking.md).

**Precedent**: master already records `(session_id, pane_session_id)` for Class A
agent panes (`agent_pane_origin.rs` v2), and the registry already carries the
`pane_session_id` field. This reuses that shape for the
(`Unknown`-origin) WTA-launched shell CLIs.

### `agent_registry` change

Add a capability field, mirroring the existing `resume_flag`:

```rust
/// Flag the CLI uses to pin a caller-chosen id on a NEW session,
/// e.g. "--session-id". None when unsupported.
pub new_session_id_flag: Option<&'static str>,
```

`Some("--session-id")` for copilot / claude / gemini; `None` for codex (and
unknown/custom). The launch path only does born-bound registration when this is
`Some`.

### Transport

- **(a)** runs in the short-lived `wta delegate` process. It opens its own
  connection to the master pipe (`master-pipe.txt` rendezvous), registers, and
  exits — the same transport `wta sessions list` already uses. Master not
  running → no-op (no registry to populate; harmless).
- **(c)** runs in the helper, which already creates the resume tab and binds its
  pane (`ResumePaneAssigned`) — it is born-bound today, so no new transport is
  needed.
- **(b)** runs in the recommendation executor (`coordinator.rs`), which only
  holds an `AppEvent` channel back to the app, **not** a direct master
  connection — see *Deferred* below.

### Storage / lifetime

The binding lives in master's **in-memory** registry row (`pane_session_id`).
**Ephemeral** — pane GUIDs are regenerated each WT run, so the binding is **never
persisted to disk**. Conversation history still lives in the CLI's own session
files and is reconstructed by `history_loader` as today; the pane binding is not
needed after a restart.

### Liveness — unchanged here

`main`'s `SessionInfo` has no `bound_pid` field and no liveness reaper, so this
feature does not change liveness: a born-bound row's Live/Ended state keeps
coming from the existing hooks + WT pane events. The `create_tab` Pid is captured
for diagnostics/logging only.

### Coexistence with hooks (on this branch)

On `main` the pinnable CLIs' hooks still fire and report the same session id
(= the pinned uuid) and `WT_SESSION`. Because both key by the same id, a hook
event merges into the born-bound row (same pane) — no conflict. The born-bound
registration is the part that keeps binding working once hooks are removed.

### Scope boundary — binding vs activity (decided: binding-only)

This feature makes **binding** hook-independent. **Activity** (Working / Idle /
Attention) for these sessions still comes from the existing hook path. So a
WTA-launched session is born **bound + Live with coarse status**; fine-grained
activity arrives via hooks as today. **Decided 2026-06-10: binding-only** —
keeping this the smallest, most independent slice; activity is intentionally
left on the existing path.

## What is explicitly unchanged

`SessionOrigin` (stays `Unknown`), `OriginFilter`, `/sessions` UI + routing,
registry serialization, `history_loader`, user-typed Class-B binding, and Codex.

## Edge cases & failure modes

- **Master not running at (a) launch** → register is a no-op; harmless.
- **`create_tab` returns no/unexpected pane** → skip registration; the session,
  if it surfaces at all, falls to the existing path.
- **(c) resume reuses an existing row** (same id = resume key) → re-bind that
  Ended/Historical row to the new pane and flip it Live; no duplicate row.
- **Pinnable CLI with hook also present** (this branch) → hook event merges into
  the born-bound row by id; consistent (same pane).
- **Codex / user-typed** → untouched (existing hooks path).
- **Gemini filename only carries `uuid[0:8]`** → irrelevant here: we register by
  the full pinned uuid; this feature never discovers the file by name.

## Testing

- **Unit**: `new_session_id_flag` per CLI; the launch→command builder (contains
  `<flag> <uuid>` for pinnable agents, absent for Codex); the registration step
  upserts a `SessionInfo` with `pane_session_id` + `origin = Unknown`.
- **Integration** (run 2026-06-10; keep as a documented manual test): each
  pinnable CLI launched headless with `--session-id <uuid>` writes its session
  under that uuid (Copilot dir / Claude stem `== uuid`; Gemini filename trailing
  group `== uuid[0:8]`, full uuid in content); `create_tab` returns a pane GUID.
- **End-to-end**: `?<prompt>` a Copilot/Claude/Gemini delegate; assert the
  `/sessions` row is born bound to the `create_tab` pane GUID with no hook
  involved in the binding, and Focus targets that pane.

## Rejected / deferred alternatives

- **New `SessionOrigin::Delegated` (three-way enum).** Rejected: conflates *who
  owns the conversation* (shell CLI vs agent pane) with *who initiated the
  launch* (user vs WTA). If a future requirement needs to surface delegate
  sessions distinctly, prefer an orthogonal provenance field
  (`initiated_by: User | Wta`). Not needed for the binding goal.
- **On-disk launch-intent index.** Rejected: the binding is ephemeral (pane
  GUIDs are per-WT-run); the in-memory registry row suffices.
- **Codex via `--session-id`.** The CLI can't pin a chosen id on a new session,
  so Codex isn't covered here; it stays on its existing path.

## Deferred: (b) the recommendation-open path

(a) and (c) are implemented; (b) is intentionally left for a follow-up — not
because its logic differs, but because it's the awkward one to wire. The
difference is **where the launch runs and what it can reach**:

| Path | Runs in | Has at hand | Registering the binding |
|---|---|---|---|
| **(a)** `?delegate` / `Alt+Shift+B` | a dedicated, short-lived `wta delegate` process | creates the tab itself (gets the pane GUID); can open its own master connection | clean — **done** |
| **(c)** `/sessions` resume | the helper (`dispatch_resume`) | already creates the tab and binds the pane (`ResumePaneAssigned`) | already born-bound — **nothing to add** |
| **(b)** agent-pane recommendation `OpenAndSend{agent}` | the recommendation executor in `coordinator.rs` | creates the tab (gets the pane id) but only holds an `AppEvent` channel, **not** a master connection | a registration route must be chosen |

So (b)'s extra cost is **plumbing, not logic**: its executor is embedded in the
running helper alongside the recommendation's other actions, and it has no direct
line to master. Registering its born-bound row means either (i) emitting an
`AppEvent` that the app turns into the existing master publish, or (ii) giving the
executor its own master sender like (a)'s. Threading that through without
disturbing the recommendation's other steps is why (b) is deferred to its own
change. It will reuse the exact same id-pinning and born-bound registration as
(a); only the transport differs.
