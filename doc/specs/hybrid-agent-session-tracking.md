# Hooks-Fallback Agent Session Watcher (Hybrid Tracking)

## Shared read model: pane metadata and Recent conversations

Pane identity and conversation identity have different lifetimes. A live agent
pane is identified by its existing Terminal connection GUID and can display
activity before an agent session ID is known. A conversation is identified by
its qualified provider, execution/history store and real agent session ID; it
can remain in Recent after its pane disappears. Neither identity is a
placeholder for the other.

WTA owns conversation discovery, confirmed associations and effective agent
activity. Terminal owns actual pane existence and the immutable launch facts
it publishes. The existing snapshot carries two read projections:

| Projection | Required key | Optional relationship |
| --- | --- | --- |
| `pane_statuses` | Terminal pane GUID | Confirmed agent conversation ID |
| `sessions` | Qualified real conversation identity | Associated live pane GUID |

Tab and pane metadata consume only the pane projection. Recent consumes the
conversation projection, retaining its existing shell-origin, visibility and
represented-session filters. A shared snapshot does not make the conversation
ID mandatory for every record. List-inferred associations remain advisory
metadata, never the confirmed relationship used for control or resume.
The pane projection carries their qualified history key separately. Recent
uses that key only to suppress the duplicate conversation while its native
pane is represented in the current window. It does not assign a live pane to
the conversation or borrow its status. Removing the pane or clearing OFF
ownership removes the advisory suppression, so the conversation returns to Recent.
The full conversation snapshot is retained. Suppression belongs to the existing
represented-session visibility filter, not parsing, so pane removal restores
the cached row immediately without waiting for discovery or its retry backoff.

```text
Existing hooks / parsed OSC/BEL / pane lifecycle
                       |
             Existing COM and WTA model
                       |
            One shared UI snapshot request
                       |
              +--------+---------+
              |                  |
        pane_statuses          sessions
        key: pane GUID         key: conversation
              |                  |
        Tab/pane metadata     Recent + filters
```

This is a read-side diagram; it does not replace the existing signal,
creation or exact-resume flows. Shared refreshes reuse the existing bounded
request and retry machinery, without a new polling loop. A hidden or cancelled
Recent projection must not cancel a request still serving pane metadata.

UI presentation owns labels, icons, filtering and command availability, not
conversation matching or agent activity transitions. Status getters are
lookups: registration occurs at pane creation/attachment, setting changes,
ownership changes and receiver readiness/recovery. Retained launch facts and
versioned transport/replay receipts are legitimate host state, not another
activity authority. Existing SDK routing and conversation IDs remain unchanged;
no new MVVM framework, generic store or conversation-ID migration is required.

## Sessions OFF: native observations in the shared registry

Terminal uses `EffectiveAgentSessionManagementEnabled()` (the user setting
combined with hooks policy), not listener availability, to enable native
OSC/BEL observation publishing. WTA remains the authoritative status source.
With Sessions ON, the existing hook/registry path is unchanged.
With Sessions OFF, only a native agent provider recorded at pane creation or an
explicit resume identifies a CLI pane. ACP helper panes keep their direct status;
ordinary shells do not gain agent status from titles, bells or progress.
Installed hooks may still emit in-band events while Sessions is OFF. Native
hook binding ingestion and retained hook replay are therefore gated in both
the page and shared content manager; they cannot overwrite explicit resume
bindings or launch-owned providers. ACP helper content is explicitly marked
and remains eligible for its direct session events.

Explicit native resume ownership is retained in the shared pane metadata,
including across window transfers. Disabling Sessions discards hook-derived
bindings but preserves a live launch-owned pane's agent eligibility and explicit
resume bindings. Repeating the same mode synchronization from another window
does not retire metadata again or revive ended/closed panes. OFF observations
use the shared launch provider, including provider refinements admitted while ON.
Mode-only clearing does not mark the live conversation superseded: after
OFF-to-ON, a same-conversation prompt or history enrichment can recover its
binding without a new session-start event. An ordinary-shell launch becomes
Unknown while its hook binding is cleared; a native launch stays Starting.
Previously superseded identities and genuinely ended/closed panes keep their
stale-update guards.
Connection restarts rekey metadata before the replacement CLI starts. When the
replacement executes the identical command, an explicit selected resume retains
its qualified source, distro, universe, exact command and command trust in either
mode; activity, failure/closed state and session-start confirmation reset.
Discovered identities and bindings for a different command are not inherited.

Terminal publishes raw observations through the existing COM event channel;
no new COM interface or polling loop is introduced. WTA applies them in its
existing registry. Tab metadata reads the registry snapshot, and known
conversation rows use the same canonical activity in Recent sessions and the
classic agent-pane session picker. C++ retains only transport identity and
epoch fences, not a separate native activity store.
Rich-status and Sidebar snapshot readers pin the process-owned SharedWta master
pipe when available; an empty pipe, including during startup, retains implicit discovery.
Managed masters receive a process-owned receiver generation. Terminal waits for
a matching snapshot that confirms the master's event subscription is ready
before reannouncing each window's retained native panes with fresh epochs and
their current parsed progress. Proven creator or previously qualified pane
identity is born-bound before admission; unknown panes remain conversation-less.
This recovery retains identity, not prior activity or BEL attention.
Within the same master generation, a ready snapshot also acknowledges each
pane's admitted epoch. Missing admissions are retried with the same epoch;
acknowledged panes are left alone. This retry does not replay old progress or
BEL, so activity missed during a subscription gap waits for a fresh signal.

An unbound native pane has a registry observation keyed by its actual pane
GUID, not an invented ACP session ID or a resumable history row. A known
explicit resume can bind a real row using its exact provider, conversation ID
and execution source. Existing authoritative born-bound assignment may arrive
before or after observation admission. When the transport source is unknown,
Terminal accepts the source of a qualified canonical pane observation without
inferring a transport source; known sources must still match. History discovery
alone cannot promote an inferred association into a confirmed pane binding.
An explicit resume of a different conversation in the same retained pane is
not recovery replay. Its ordered birth supplies the expected previous session
and admission epoch; WTA validates that predecessor and a newer epoch atomically
before replacing foreground ownership. The previous conversation becomes
unbound Historical, not Ended: replacement does not prove that its process exited.
An explicit different-owner OFF reset cannot bypass this handoff if birth was lost.

The existing parsed OSC 9;4 progress reports map to the existing agent statuses:
Idle (0/4), Working (1/3, displayed as Active), and Error (2). Paused progress
does not claim active work or add a sidebar status; its existing progress-ring
presentation is unchanged. BEL maps to the existing Attention status, displayed
as Waiting for input, and retains the existing tab bell handling. Here Waiting
for input includes both needing a response and finishing work ready for the
next human request; BEL does not prove a mandatory response or approval.
Leftover unqualified COM hook activity cannot overwrite an active OFF native
owner. Precisely qualified watcher evidence for an exact bound identity remains
supported; ON and helper activity retain their existing paths.
Native status observes the unthrottled parsed bell source; the existing
one-second throttle still applies to visual bell feedback.
Each progress event also retains its parsed state before UI dispatch. A queued
Error-to-Idle burst therefore reaches the registry as two ordered observations,
even when the final rendered progress state is already Idle.
The next progress event clears native-only BEL attention and maps to Working,
Error, or Idle. Generic progress does not dismiss a canonical input request
with an existing attention reason or input-tool context; its matching completion
or a new prompt supplies the transition. Focusing the tab only dismisses its
bell indicator, not its registry activity. OSC 9;4;0 alone clears progress to Idle and does not
prove that the process has ended. With Sessions OFF, only disappearance of the pane ID ends native
ownership. OSC 133;A, OSC 133;D, and a retained pane's Closed/Failed connection do not prove
the agent has ended, and do not expire status, provider identity, or explicit
resume bindings. This pane-presence status is deliberately not a process-liveness
claim. Changing Sessions clears OFF observation ownership and invalidates queued
BEL/progress callbacks through admission epochs. WTA retains activity across a
same-owner OFF transfer or reconnect while updating routing context; older
epochs cannot update the new owner. Splitting, reparenting or transferring
content must not attribute one pane's signal to another.
Actual content retirement publishes removal before its identity is discarded,
including automatic `closeOnExit` retirement of a tab or split leaf. Detaching
or transferring retained content is not retirement.
Public Restart Connection replaces the connection GUID even when the control
and content ID are retained. The old GUID is retired, and the replacement gets
fresh creation metadata, admission epoch and Idle activity before it starts.
Queued signals retain their emission-time GUID and cannot update the replacement.
An explicit resume binding is retained only when a declared native agent is
relaunched with the identical ConPTY command line; discovered or inferred
identities are never carried across the replacement.
While Sessions is ON, matching resume completion or supersession clears the
temporary explicit provider along with its binding. That provider is never
promoted into independent launch ownership, so switching OFF cannot turn the
completed resume's ordinary shell into a native agent. Independent launch
ownership remains intact.
OpenCode's `agent.session.unbind` releases a selected conversation (for example,
returning to its home screen or handing ownership to another client), not the
CLI process. It retires the old conversation and clears its resume command/trust,
but retains the live pane's provider and agent eligibility. The released row
returns to History; stale activity cannot rebind it, while a legitimate session
start may select a conversation again. Ordinary-shell OpenCode panes remain
eligible while Sessions is ON without acquiring independent launch ownership.
Superseded identity checks also prevent stale canonical/advisory caches from
representing or highlighting the released row, regardless of manager/page
notification order; a confirmed new selection retains precedence.
Real session-end and connection-close events still end pane eligibility.
When Sessions is ON, authoritative Ended/closed metadata also releases History
representation immediately, even while an older pane-status cache remains.
Live Starting/Failed panes and OFF retained Closed/Failed connections keep their
existing canonical/advisory History deduplication.

The shared content factory records each connection lifetime's immutable UTC
creation time and producer-owned monotonic creation sequence before observation admission.
The connection SessionId supplies pane identity; OSC does not discover it.
Delayed WTA readiness, OSC/BEL updates, mode resets and retained-content moves
do not retime creation; replacing the connection GUID begins a new lifetime.
Managed native shortcut profiles supply their known
Host source; delegate creation supplies its explicitly selected execution source.
An unknown source is not assumed to be Host.

New native launches initially have an Idle pane record with provider, creation
metadata and no confirmed conversation ID. Existing ACP list results may enrich
that record with a separately marked inferred session identity. Matching uses
newly discovered unmapped IDs from the same provider and execution source,
timestamp evidence within a five-minute launch window and the order of pending
pane creations. Only complete discovery requests begun after pane creation
qualify; stale, tied, extra, source-unknown or universe-qualified results leave
the identity unknown. OSC partial updates retain creation
and identity metadata; historical list status cannot overwrite live pane status.

Historical ACP list rows contain an update timestamp, not necessarily a genuine
creation timestamp or launch ownership token. Even a singleton result cannot
exclude an independently created external session. An inferred association is
therefore metadata, never proof of ownership: it cannot set canonical live pane
bindings, route control actions or be replayed as a confirmed identity. Explicit
identity wins over inference. No additional query, polling loop or cadence is
introduced.

When an eligible native pane is waiting for correlation, the existing retained
history request follows its `session/list` cursors to a complete inventory.
Pages share the original request's soft/hard deadlines and survive soft
timeouts; there is no per-pane query or new worker/cadence. Ordinary discovery
without a pending native pane retains its existing first-page behavior.
An incomplete inventory cannot establish uniqueness. First-list evidence is
retained even if pane admission or source delivery is delayed; known IDs and
evidence first observed before pane creation still cannot become new launches.
The five-minute launch window bounds query start and provider activity, not
delivery of a retained slow inventory. A response that finishes later remains
eligible when its underlying query and activity evidence qualify.

Explicit recent-session resume uses the same pane creation metadata and retains
its known binding under the actual connection
SessionId even without a saved pane ID. Explicit binding ownership transfers with
the pane between windows and remains until the pane is removed or a later
authoritative explicit resume replaces that conversation, including after
command marks or retained Closed/Failed connection states. The existing Sessions
ON hook/completion policy is unchanged. A reused saved pane GUID cannot
inherit the closed pane's explicit ownership marker.

The watcher design below describes registry tracking, not an authority to
infer native pane identity when Sessions is OFF.

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

The master retains **two disjoint legacy raw-ID** ownership sets (`master/mod.rs`):

- `hook_owned: Mutex<HashSet<SessionId>>` — sessions a **real** producer owns
  outright (native CLI hooks, ACP agent-pane events). Owns binding **and**
  activity.
- `born_bound: Mutex<HashSet<SessionId>>` — #266 **born-bound** sessions
  (WTA-launched delegate `?<prompt>` and `/sessions` resume). These provide a
  pane binding but **no activity** — binding-only.

`scoped_hook_ownership` additionally records real-hook versus binding-only
ownership by `SessionIdentity` (provider, SID, source/distro and universe).
Qualified hook transitions claim that exact identity rather than letting one
store's raw-SID claim suppress another store's activity.

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

Watcher events still carry raw SIDs. Processing first requires a unique registry
identity, then prefers its qualified ownership over any stale legacy claim:

1. real-hook ownership → **drop** (the hook owns binding and activity);
2. binding-only ownership → **apply status only**, never re-bind;
3. anything else → drop rather than guess.

If multiple stores/providers have the same raw SID, watcher-only activity is
conservatively dropped: transcript events lack the provenance needed to choose
one. Real scoped hooks continue to update the matching row independently.

### Hook-owned Copilot activity

Hook-owned sessions do not supplement hooks with transcript permission events.
Each blocking Copilot notification starts or resets a session-local 30-second
cooldown measured from registry receipt time, not the provider timestamp.
Successful tool returns during it leave Waiting and its metadata unchanged,
regardless of tool name or missing/invalid source timestamps. Informational
notifications do not reset the deadline, which survives session snapshots.

Expiry does not automatically restore activity. Later successful returns may
restore Working, but a source timestamp older than the newest waiting
notification remains ignored. Numeric Unix milliseconds and RFC 3339 source
timestamps share that comparison. `Stop` immediately owns Idle even during the
cooldown; prompts, errors, and session/pane closure keep their normal behavior,
and late returns cannot reactivate Idle or terminal-state rows.

This is a time-window heuristic, not per-request approval correlation: quick
approvals can remain Waiting until another accepted event or Stop, and unrelated
parallel completions after the cooldown can still replace an outstanding wait.
The original born-bound, hookless file-watcher fallback is unchanged.

### Born-bound activity fallback

Born-bound (see [wta-launched-cli-session-binding.md](./wta-launched-cli-session-binding.md))
registers a WTA-launched session's `(session id → pane)` at launch and records
it in `born_bound`. It supplies the **binding** but emits **no activity**. With
hooks installed the CLI's hook supplies activity (and takes over); with **no**
hooks the row would otherwise sit at `Idle` forever.

When that SID uniquely identifies a registry row, the watcher already knows
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

**Terminal restore metadata.** In-band VT agent events may identify a session,
but cannot supply an executable `resume_commandline`: the page removes that
field before applying or forwarding the event. ContentManager also records
command provenance from the ingress API, not from JSON. Only explicit
out-of-band protocol ingress and application-owned bindings may retain raw
resume wrappers (including WSL distro, cwd and session-universe selection).
Other bindings are rebuilt with the validated native/WSL resume builder;
failed validation leaves the original shell command unchanged. A late identity
binding does not reset a failed connection. Pending restore replay is reconciled
against authoritative pane identity both after manager updates and at replay,
so independent page/manager dispatch order cannot revive an ended or replaced
session. Retaining an existing command during identity binding retains its
existing provenance; an empty command argument does not promote trust.
Pane lifecycle transitions compare provider, SID, source, distro and universe.
A start without an SID confirms an application-prebound resume while that
binding awaits its real start; it does not retire the requested conversation.
Once the real start has arrived, a subsequent sessionless start begins a new
lifecycle and retires the old identity. Retired identities are scope-qualified:
late activity, end events and History enrichment cannot resurrect them or clear
a different store's current binding. History enrichment is not a new birth.
Identity replacement clears the previous command, cwd and trust provenance;
only enrichment of the same identity may inherit them.
Native start, prompt and end hooks distinguish an omitted universe from an
explicit empty one. Omission resolves through a confirmed binding on the same
pane with matching SID, provider and source/distro; it preserves that binding's
store and command provenance. Explicit empty selects the default store, and
explicit different values retain their scope rather than borrowing the current
store. The same resolution applies before stale-event checks and end handling.

Trusted commands persist in an `intellterm-resume-v1` layout envelope containing
the exact command, provider, SID, host/WSL backend (including distro), cwd, and
session universe. Restoration validates it before creating settings and uses a
separate unwrapped settings argument. The forwarding argument retains its envelope
through automatic elevation, including split restoration; only the process that
creates the connection unwraps it for execution and prebinding. Malformed envelopes
fail rather than execute literally.
StartingDirectory remains a normal layout field, and restored agent conversations
skip saved shell-buffer replay. Pending births bind and replay the same qualified
identity, including the expected universe; a replacement in any scope cannot
replay the old birth. The helper-to-master birth request retains that universe;
explicit-store births bind the qualified row without first mutating a raw-SID
collision in another store. VT command strings never enter this envelope.

**Subsequent hooks.** `SessionHookUpdate` retains routing scope across start,
tool and end events. COM and typed helper ingress resolve validated explicit
scope or matching authoritative pane ownership before applying a qualified
registry transition. An omitted universe does not constrain store selection;
a present empty universe explicitly selects the default store. A supplied full
identity must agree with the event SID and any supplied scope. Legacy events
remain valid when their matching identity is unique. Ambiguity is rejected and
logged, not interpreted as an absent row that should be inserted as Host/default.
Helper queues preserve a uniquely resolved snapshot identity instead of choosing
arbitrarily among same-SID stores. Pane-only close and connection-failure hooks
resolve that identity using the normalized pane ID before queueing, so a delayed
hook cannot terminate or fail a replacement owner. Missing or ambiguous snapshot
ownership is logged and the pane-only hook is not queued.

CLI resume rejects unsupported alternate universes before creating a pane or
marking/binding a session, both in Sidebar History and the helper picker. An empty
universe denotes the source's current store. Explicit universes are rejected:
new panes inherit Terminal's environment, not master's `COPILOT_HOME`, and a WSL
login shell may also select another store. Even matching master's home cannot
prove the new CLI's store. The helper also rejects ambiguous same-SID snapshot rows
from different universes.

**Native History resume metadata.** Explicit Sidebar activation keeps the
selected qualified session identity. Both the master Sidebar path and helper
picker send the canonical qualified launch envelope before the CLI starts, so
Terminal prebinds the actual new connection even if a bootstrap hook precedes
the creation reply. The envelope retains the exact resolved executable,
supported PowerShell shim wrapper, or WSL command; the completion binding still
carries that underlying command, not a nested envelope. The stale-event guard
continues to reject attempts to replace another selected identity.
After tab creation the master confirms the returned pane binding.
Historical/Ended becomes Idle; newer live activity is retained.
After binding, the master invalidates session lists and Terminal's status
projection through the existing structural notification, without waiting for
a hook. An exact matching binding established by an earlier hook is successful
and keeps that hook's activity. Assistant source-pane context uses the existing
registry lookup for the real native CLI session ID, not the helper's ACP ID.
This known binding keeps the resumed tab visible under Agents only before its
first hook. Idle is an initial baseline, not evidence that the CLI loaded or
that subsequent activity is being observed.
For an unambiguous native host row, a newly assigned resume pane also registers
`born_bound` watcher ownership and clears the previous generation's hook claim.
The binding and ownership transition share the hook lifecycle gate. If a real
hook already bound that exact pane during tab creation, its activity and
ownership remain authoritative. No helper subscription or hook installation is
required for this native activity fallback; saved-layout restore routing is
unchanged.

Watcher ownership checks and activity updates share that lifecycle gate too.
Native file records only update a unique row for the matching CLI, never a WSL,
agent-pane, alternate-universe, or ambiguous raw-ID row.

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

Terminal's `ContentManager` applies the same startup protection to direct resume
prebinding and trusted WTA `pane_agent_session_changed` bindings carrying a resume
command, even when `params.event` is absent. While awaiting the selected session's
start, a sessionless startup preserves the binding, and a different bootstrap SID
from the same provider and compatible source/distro/universe cannot replace it.
An omitted universe does not change that confirmed scope; an explicit empty or
different universe remains a different identity. Prompt activity for the selected
SID is accepted without ending the protection. Its own Start confirms the binding
and allows later session replacement. A delayed duplicate WTA binding cannot
re-arm a confirmed session, and untrusted terminal output cannot establish this
resume protection.

Graceful connection closure is distinct from an agent session ending inside a
live shell. It retires the binding even if only the provider is known, and blocks
late hooks, resume callbacks, direct bindings, and history enrichment until
`ResetPaneConnection` installs fresh connection metadata. Failed connections keep
their existing failure and resume metadata.

Helper-initiated resumes carry the selected qualified identity through optimistic
dispatch and pane creation completion; they do not infer it from a possibly
delayed sidebar snapshot or publish a second raw-ID dispatch. The new-tab command
uses the existing `intellterm-resume-v1` envelope to carry backend, session ID,
cwd, universe, and the trusted executable command. Terminal unwraps it and binds
the actual new connection GUID before attaching the pane, including fresh tabs
without a saved SessionId. Bootstrap hooks and the completion callback may arrive
in either order without replacing the selected qualified identity.
Helper-originated
Host resume persistence retains the executable CMD wrapper needed by CLI shims,
without a loading-banner shell chain; native master launch commands remain
unchanged. Registry-only WSL bindings use their confirmed source and distro when
constructing the safe fallback command; missing or invalid WSL qualification
never falls back to Host.

### User-typed PowerShell resume: binding before the first conversation

When Intelligent Terminal's shell integration is loaded in PowerShell, accepting
a literal resume command emits `OSC 9001;Resume;<agent>;<session-id>` before the
CLI starts. `TerminalPage` binds the emitting pane's connection GUID directly to
that session and publishes `session_born_bound`; neither a hook nor a first
conversation is required. Saved layouts reuse the binding to persist the agent's
resume invocation, including when Terminal closes before any conversation.

Both `copilot --resume=<id>` and `copilot --resume <id>` are recognized, including
quoted literal IDs, additional CLI options, and `.cmd`/`.exe` launchers. Claude
and Gemini use the same resume flag; Codex uses `resume <id>` and OpenCode uses
`--session <id>`. Antigravity uses `agy --conversation <id>` for CLI conversations,
not ACP sessions, and emits the canonical agent ID `antigravity`. Only a single
foreground command with literal arguments is inspected. Variables, expressions,
pipelines, compound commands, and duplicate resume flags are not evaluated or
guessed. Bash/WSL and CMD do not emit this PowerShell-specific mark.

Resume flags and Codex's `resume` subcommand must be unquoted `BareWord` AST
arguments. Both `copilot --resume="id"` and `copilot --resume "id"` are recognized:
the session ID may be quoted, but `copilot "--resume=id"` and
`copilot "--resume" "id"` do not establish an OSC binding. The same rule excludes
quoted resume-looking prompt text, regardless of which CLI option precedes it.

The detector preserves AST quote boundaries instead of flattening arguments or
maintaining provider-specific option lists. It does not interpret CLI option
semantics or infer which options consume values; an unquoted resume-looking word
used as another option's value remains ambiguous. Unquoted single-dash options
are PowerShell parameter AST nodes and remain outside the literal-only scope.
Codex only inspects the leading `resume <id>` subcommand, not later options.

The binding is retired when the shell reports command completion (`OSC 133;D`).
The v11 script upgrades earlier v10 installations to the literal resume parser.
After shell-integration installation or an upgrade completes, open a new
PowerShell session to load the updated script; an already-running shell keeps
its previously loaded functions. Existing development copies already named v11
must be refreshed separately. This binding path does not require automatic
error detection or Autofix to be enabled once the script is loaded.

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

The vertical sidebar's dedicated Agents button uses the Fluent UI System Icons
`Agents 16 Regular` vector icon and opens live and historical sessions in a view
with an **Agents** header, search box, and close button. Closing it
returns to the live tab/pane groups and stops session refreshes, preserving the tab
search and foreground selection. The Filter flyout contains only Tab Metadata
controls; it does not switch between All tabs and Agents only. The Agents
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
The bottom-right session-management button is hidden only in the Vertical tab
layout; other layouts retain it. Its visibility updates on startup and live
layout changes, independently of whether the vertical sidebar is expanded,
collapsed, or hidden. The button shares the existing `openAgentSessions` action.
The final agreed keyboard and focus behavior is specified in
[Agent History and Sidebar Keyboard Navigation](./agent-history-sidebar-keyboard.md).
That contract does not change horizontal agent-session behavior.
In vertical layout, `Ctrl+Shift+/` opens the Agents view
and focuses its search box. Closing it with the same shortcut or close button
restores the sidebar's previous expanded/collapsed state and attempts to restore
the source chat input or terminal split, with a visible-terminal fallback.
In contrast, `Ctrl+Shift+S` only expands/collapses the sidebar: expansion does not
move focus or activate search, and collapse uses the no-source focus policy even
when the Agents view was visible. Neither action deletes session data or stops agent tasks.
The sidebar hint uses **Expand sidebar** / **Collapse sidebar** and shows the
effective binding on the same line in dimmed text, with casing such as `Ctrl+Shift+S`.

Session titles use only the text before the first CR or LF. An empty first line
uses the existing missing-title fallback. The title occupies one non-wrapping
line with ellipsis; the metadata line below it is unchanged.
The second line is left-aligned as `Agent name · relative age · status` for Host
sessions and `Agent name · distro name · relative age · status` for WSL sessions,
using the provider's display name, the exact WSL distro name, and
`last_activity_at_ms`. The sidebar uses Windows ICU short relative-time formatting
for minutes, hours, days, weeks, and completed Gregorian UTC months and years.
It retains the master-provided timestamp and checks the local clock every second
while the Agents view is loaded and expanded. Age text is recalculated when the
elapsed-minute bucket changes, and only changed text raises a property notification.
This does not request a session snapshot or replace unchanged list rows; selection,
focus, and scroll position are preserved. Search terms follow the updated age text.
Successive registry snapshots also preserve surviving Recent session rows by
their full provider/session/backend identity. Status, subtitle age and pane-binding changes
update the retained row in place; genuine membership or ordering changes insert,
remove, or move only the affected rows instead of replacing unchanged neighbors.
Collapsing or unloading the sidebar, or leaving Agents, stops the local timer;
showing it again immediately corrects cached ages. Session data refreshes and
retry backoff remain independent, so retained ages continue advancing during a
failed or in-flight refresh. Missing,
zero, or invalid timestamps display Unknown, and future timestamps display just now.
Active uses a theme-aware green success accent, Waiting for input a yellow caution
accent, and Error a red critical accent, matching the session management view.
Only the status text is accented; the provider, distro name,
age, and separators stay muted, and search matches remain highlighted.
Live tab metadata and expanded split-pane metadata use the same status accents
at full opacity. Only neutral text retains the existing 0.72 opacity, applied
to its foreground rather than the entire metadata control.
Only the provider's `agentStatus` field is colored; working-directory and Git
fields retain their existing appearance, including when they contain the same
text as a status label. Idle and non-actionable states retain the normal metadata
color. Status colors follow theme changes without removing search emphasis.
Live metadata text and its semantic status come from the same accepted provider
request. While a refresh is queued, both retain the previous snapshot; a failed
refresh clears that provider's metadata rather than recoloring stale text.
Host/WSL location remains searchable and available for routing; Host has no extra location
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
The default current-row palette pairs the theme's selected background and
selected foreground, including theme changes. UI Automation exposes a localized
Current session item status on the current row's list container and clears it
on deselection or recycling; keyboard selection remains independent.
Missing or unrecognized states display Unknown rather than implying a historical
session. Search matches both the displayed status and the raw registry value;
the existing `live` and `history` search terms remain available. This presentation
does not change shell-session visibility, liveness classification, or focus/resume
routing. Registry-change notifications and the existing 60-second snapshot
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

External Copilot **In use** evidence is informational, not a resume restriction.
Selecting such a row resumes it through the CLI in a new tab; activation does not
probe external ownership or wait for an in-use lock to be released. Failure to
read ownership evidence likewise does not block resume. The session list still
shows **In use** when supported by external evidence. A live session with an
authoritative Terminal pane binding continues to focus that pane instead of
starting another CLI. The agent CLI remains responsible for accepting the resume
request and handling any concurrent access to its session store.

History list requests are single-flight per window. Notifications received during
activation are coalesced and serviced after activation completes, without hiding
the existing rows. Initial load failures (including non-zero CLI exits) show a
warning rather than an empty-history success state. Background refresh failures
retain the previous snapshot and display a warning alongside it. Refresh errors
and activation errors are independent: a successful refresh clears only the
refresh warning, not a failed focus/resume result. Existing localized error
messages are reused.

Provider ACP history queries are separate from these short-lived sidebar list
commands. Each pooled command/source owns a reusable history-only ACP process;
chat, Autofix and session restoration keep using the original shared chat agent.
Only one provider query is outstanding, even after a soft timeout. Its response
receiver survives the 5/10/20/40/60-second absolute wait checkpoints; after 60
seconds, checks continue once a minute. Three consecutive responses under five
seconds reduce the wait budget by one step. Successful queries impose five
seconds from completion before another send. Completed failures impose
5/10/20/40/60-second backoff; neither manual refresh nor initialization bypasses
or resets it.

A query still unresolved after 300 seconds retires the history instance. Its
Windows launcher and descendants are contained in a private Job Object; WSL
queries have an identity-checked process group and stdin-EOF cleanup guardian.
Replacement waits for confirmed exit and failure backoff. Unconfirmed cleanup
keeps the old instance quarantined across chat-agent replacement. Late retired
responses cannot mutate the registry, and cached rows remain usable. Hooks only
schedule the existing worker and never await a slow history response.

Consecutive list failures impose a 5, 10, 20, 40, then 60-second retry delay,
measured from completion. Both registry notifications and the 60-second timer
respect it; the timer retries on its first eligible tick. Failed requests discard
the coalesced pending refresh instead of immediately retrying. A `ready` snapshot
or leaving/reopening the Agents view resets the retry delay. A `loading` discovery
snapshot is not a failure and does not increase the retry delay.

Closing the view, destroying its page, or starting activation signals cancellation
of the in-flight list command. The background capture loop checks cancellation
before launch, while draining output, and between bounded process waits. It
terminates only its own short-lived list process, not shared WTA master, provider
discovery, or agent sessions. The UI thread never waits for process exit.
Late/canceled responses cannot replace the snapshot, display an error, or add
retry backoff. Reopening coalesces a fresh request until the old worker completes,
using a new cancellation flag for the new request.

Activation operations are reserved by activation ID before dispatch and owned by
master, not by the short-lived CLI connection. Concurrent requests with the same
ID return its pending or completed receipt rather than focusing or restoring
again. Reusing an ID for another qualified session identity or window is rejected.
The bounded receipt cache never evicts pending operations.
Timeouts or unreadable responses from the master's own `wtcli` mutation also
remain unknown, rather than being converted into a definitive rejection that
would permit a duplicate restore. A created tab without a usable pane binding
is likewise not safe to restore again.

A client timeout does not prove that the backend took no action. After an
unconfirmed activation response, Terminal makes one bounded, read-only
`sessions activate --status-only` request with the original activation ID and
qualified identity. This uses the separate `session/activation_status` extension
method, so an older master cannot mistake a status lookup for another activation.
An unavailable, pending, malformed, or mismatched receipt leaves the operation
unresolved and displays the existing activation error. Clicking that row again
checks the same operation instead of generating a new ID or restoring again.
Other rows remain independently activatable.

Unresolved IDs survive History close/reopen and list refreshes for the lifetime
of the page. A matching completed receipt releases the ID, even if the view closed
while the request was running, without applying a stale UI callback. A late
receipt cannot release a newer operation. If master restarts or evicts a completed
receipt before it is observed, status is `unknown`; Terminal conservatively keeps
the ID and does not automatically redispatch a potentially completed mutation.

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
Master refreshes initialized, listing-capable pooled connections every five seconds,
including already-connected WSL and custom agents. Snapshot reads never perform ACP
queries; push notifications remain immediate. The 60-second open-view fallback runs
in Sidebar for vertical layout and in the helper for nonvertical layout. The host
propagates the actual window layout through helper-ready and hot runtime config,
so layout switches do not require an ACP reconnect.
Host discovery runs at startup, after confirmed installation, or through the explicit
`wta sessions refresh` command, not on a timer or ordinary snapshot reads. Opening
History is not required to establish these connections or load the initial histories.

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
