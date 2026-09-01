# Agent Failure Handling — a typed, graceful model

This is the **design** for a complete, graceful failure-handling system for the
`tab → helper → master → agent CLI` stack. It supersedes the ad-hoc,
string-matched handling described as gaps in
[`connection-resilience.md`](./connection-resilience.md) (§6 "residual
`is_auth_error` string match", §6 "no `conn.prompt()` timeout", §5 deferred
auto-recovery) and gives every failure a single, typed classification with a
defined recovery policy.

Guiding goal: **every failure either auto-recovers, or leaves the user in a
state with one obvious next action — and never silently loses their input.**

See also [`Multi-window-agent-pane.md`](./Multi-window-agent-pane.md) for the
helper+master architecture.

---

## 1. Why the current model is not enough

ACP errors are already **strongly typed** — `agent-client-protocol` 0.10 /
schema 0.11 carry a JSON-RPC `acp::Error { code: ErrorCode, message, data, …
}`, with a stable `ErrorCode` enum (`AuthRequired = -32000`,
`ResourceNotFound = -32002`, `InvalidParams = -32602`, `MethodNotFound`,
`InternalError`, `RequestCancelled`, …). But we throw that type away:

- `complete_prompt_request<T, E: std::fmt::Display>` (`protocol/acp/client.rs`
  ~797) is **generic over any `Display` error**, so the prompt arm stringifies
  the code away (`e.to_string()`, ~830) before anyone can branch on it.
- `AppEvent::AgentError { session_id, message }` (`app.rs` ~1056) carries
  **only a `String`**.
- The handler (`app.rs` ~4076) then **reverse-engineers** the class with
  `lower.contains("authentication required" | "401" | "api key" | …)`. This is
  the fragility `connection-resilience.md` calls out: keyword matching silently
  mis-routes (a non-English agent, a reworded message, or an unrelated error
  containing "401" all break it).

Meanwhile **transport** failures (pipe EOF) and **handshake/timeout** failures
are *signals*, not ACP errors, and are handled in yet another place
(`run_acp_client_over_pipe` watchdog, `main.rs` raw return). There is no single
taxonomy that unifies "agent said no", "transport died", and "nothing
answered".

## 2. The taxonomy — one enum, transport lifecycle separate

ACP errors and startup/watchdog failures collapse into a single `AgentFailure`,
classified at the **helper boundary**. Master-pipe closure is a separate
lifecycle signal and bypasses this taxonomy by posting
`AppEvent::MasterDisconnected` directly.

```
acp::Error (typed)                                  timeout / watchdog
   │  code: ErrorCode                                  │ no progress past deadline
   ▼                                                   ▼
                    ┌───────────────────────┐
                    │   classify_failure()  │   protocol/acp/failure.rs (new)
                    └───────────┬───────────┘
                                ▼
                          AgentFailure
```

```rust
// protocol/acp/failure.rs (new)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentFailure {
    /// Agent advertised it needs sign-in. From ErrorCode::AuthRequired, OR a
    /// handshake/new_session that came back auth-coded. → sign-in screen.
    AuthRequired { message: String },

    /// The connection never *established*: pipe-connect / `initialize` /
    /// `session/new` / `session/load` timed out or errored at startup.
    /// `stage` tells the user (and the log) where. → retry w/ backoff.
    HandshakeFailed { stage: HandshakeStage, detail: String },

    /// Agent process is ALIVE but produced no progress before the inactivity
    /// deadline (hop-2 protocol hang). Distinct from a closed master pipe —
    /// this is the silent hang. → cancel turn, session survives.
    Unresponsive { stage: Stage },

    /// A referenced resource is gone — `session/load` of an expired session,
    /// a missing file. From ErrorCode::ResourceNotFound. → offer fresh session.
    ResourceGone { message: String },

    /// Agent returned a *protocol* error that does not kill the session:
    /// InvalidParams / InvalidRequest / MethodNotFound / ParseError /
    /// InternalError / Other(_). Almost always an agent-side or our-side bug.
    /// → show with code, turn ends, session stays Connected.
    Protocol { code: acp::ErrorCode, message: String },

    /// User-initiated cancel surfaced as an error (ErrorCode::RequestCancelled).
    /// Not a failure — swallowed, turn just ends.
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeStage { PipeConnect, Initialize, NewSession, LoadSession }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage { Initialize, NewSession, LoadSession, Prompt }
```

### 2.1 The classifier (the only place that pattern-matches)

```rust
pub fn classify_acp_error(e: &acp::Error) -> AgentFailure {
    use acp::ErrorCode::*;
    match e.code {
        AuthRequired                  => AgentFailure::AuthRequired { message: e.message.clone() },
        ResourceNotFound              => AgentFailure::ResourceGone  { message: e.message.clone() },
        RequestCancelled              => AgentFailure::Cancelled,
        InvalidParams | InvalidRequest | MethodNotFound | ParseError | InternalError
                                      => AgentFailure::Protocol { code: e.code, message: e.message.clone() },
        Other(_)                      => AgentFailure::Protocol { code: e.code, message: e.message.clone() },
    }
}
```

`is_auth_error`'s substring list is **deleted**. The only residual string check
is a *narrow, transitional* fallback for agents that wrongly return
`InternalError` for auth (some CLIs do): if `code == InternalError` AND the
message clearly says auth, upgrade to `AuthRequired`. This is opt-in, logged as
`target=failure non_compliant_auth=true`, and removable once agents comply —
contrast with today's behavior where string-matching is the *only* path.

## 3. Per-class policy — the graceful table

Each class has a fixed **(UI surface, state transition, recovery, input
preservation)** policy. "Preserve input" = the user's prompt text is restored
to the composer so a resubmit/`/restart` loses nothing.

| `AgentFailure` | UI surface | `ConnectionState` | Turn | Recovery | Preserve input |
|---|---|---|---|---|---|
| `AuthRequired` | Sign-in screen (`AppMode::Setup`, `SetupReason::AgentError`) | `Disconnected` | Idle | run login cmd → auto re-connect | ✅ |
| `HandshakeFailed` | inline raw line w/ `stage`, "retrying…" / `/restart` | `Failed(detail)` | Idle | bounded retry w/ backoff, then manual | ✅ |
| `Unresponsive` | inline "Agent isn't responding — Esc to cancel" + elapsed | `Connected` | Idle on cancel | cancel turn (session/cancel), keep session | ✅ |
| `ResourceGone` | inline system "That session is no longer available — starting fresh" | `Connected` | Idle | offer/auto `session/new` | ✅ |
| `Protocol` | inline error w/ `[code]` prefix | `Connected` | Idle | none auto; user may retry prompt | ✅ |
| `Cancelled` | none (expected) | unchanged | Idle | — | ✅ |

### 3.1 Seven graceful principles (the invariants the table encodes)

1. **Never lose input.** On any non-`Cancelled` failure, the in-flight prompt
   text is pushed back into the composer (today it is lost). One place:
   `App::handle_agent_failure`.
2. **Fail closed at transport boundaries.** `MasterDisconnected` terminates
   the helper and never reloads its session. `Protocol` / `ResourceGone` /
   `Unresponsive` keep the session because the transport remains valid.
3. **Automate only state-preserving recovery.** Auth retry and a recoverable
   missing resource may stay in-process. Process/transport crashes require a
   fresh user-initiated pane/session.
4. **Typed, not string-matched.** Every branch keys on `AgentFailure` /
   `ErrorCode`, never on message text (except the one logged, transitional
   non-compliant-auth shim).
5. **Always actionable.** Every surviving state shows exactly one next step
   (sign in / Esc / Enter to start fresh). A terminal transport failure exits.
6. **Idempotent & deduped.** Identical consecutive lines collapse; *different*
   lines coexist (keep `connection-resilience.md` §2 rule — raw cause + recovery
   hint both show). Keyed on `AgentFailure` discriminant, not string equality.
7. **Observable.** Every classification logs `target=failure class=… code=…
   stage=… recoverable=…` so a single grep reconstructs any incident.

## 4. Threading the type through (Phase 1 — the cheap, high-value change)

This is the minimal change that kills the string-matching and unlocks
everything else.

1. **`AppEvent::AgentError` → `AppEvent::AgentFailed`** (or add `kind` to the
   existing variant):
   ```rust
   AgentFailed {
       session_id: Option<String>,
       failure: AgentFailure,
       /// raw `{e:#}` for the log / "what broke" line; UI cause text
       detail: String,
   }
   ```
2. **`complete_prompt_request`** stops being `E: Display`. The prompt path keeps
   the concrete `acp::Error`, runs `classify_acp_error`, and emits
   `AgentFailed { failure, detail }`. (The few non-ACP callers wrap as
   `Protocol`/`HandshakeFailed` explicitly.)
3. **The watchdog** (`run_acp_client_over_pipe`, both `Ok`/`Err` arms) logs the
   transport result and emits `MasterDisconnected`, which terminates the helper.
4. **Handshake sites** (`main.rs` raw return, `initialize`/`new_session`/
   `session/load` timeouts in `client.rs`) emit
   `HandshakeFailed { stage, detail }`.
5. **`App::handle_agent_failure`** replaces the `is_auth_error` block with a
   `match failure { … }` driving the §3 table. `publish_agent_status`,
   `ConnectionState`, dedup, and tab routing all hang off the typed value.

The auth screen now triggers on the typed `AuthRequired` (covering localized /
reworded agent messages the substring list misses). Transport loss remains a
separate terminal signal.

## 5. New mechanisms the table requires

These are the genuinely new pieces, each closing a named gap.

### 5.1 Prompt inactivity watchdog → closes §6 "no `conn.prompt()` timeout"

A hard deadline on `conn.prompt()` would kill legitimately long turns. Instead,
an **inactivity** watchdog: the prompt task holds a `last_progress: Instant`
that every inbound `session/update` for that session resets. A timer fires
`Unresponsive` only if **no** progress arrives for `T_idle` (default 90 s,
setting-overridable) *and* the prompt future is still pending. On fire we
surface the actionable line and let the existing Ctrl+C / Esc cancel path
(already wired through `cancel_signals`, `client.rs` ~3335) tear the turn down —
the session and pane survive.

### 5.2 Helper transport loss → deterministic exit

On `MasterDisconnected`, the helper does not reconnect to the stable pipe or
call `session/load`. It ends the TUI and drops owned children. This makes the
process boundary deterministic and prevents a crashed session from being
replayed automatically.

### 5.3 Master/helper crash → terminal cleanup, no automatic recovery

A helper, master, or agent crash is a terminal lifecycle event. Helpers exit
when their master pipe closes, and Terminal's `closeOnExit:"always"` closes the
pane. C++ does not re-warm the pane, and crash handling never passes
`--initial-load-session-id`.

The next user-initiated pane open lazily starts a fresh master/helper/session.
History Resume remains a separate explicit action. This avoids poison-session
restart loops, cross-agent resume, and duplicate operations after an uncertain
failure boundary.

### 5.4 Agent-side session release on disconnect → closes §6 / F8

When a helper disconnects, master treats the helper as terminal and fences new
session transactions for it. While holding the helper's replacement gate,
master sends bounded `session/cancel` and, where supported, `session/close` for
each owned ACP session. It then retires the local route, registry entry,
session-scoped MCP capability, and pending state even if provider-side cleanup
is unsupported, fails, or times out. Provider history remains available only
for an explicit user-initiated `session/load`; disconnect never preserves an
orphan for automatic resume.

## 6. Phasing

| Phase | Scope | Closes | Risk |
|---|---|---|---|
| **1** ✅ | Typed `AgentFailure` + classifier; replace `is_auth_error`; structured logging (`target=failure`). *Input-preservation split to Phase 1.5 — restoring the composer only when empty/per-tab has UX subtleties best done deliberately.* | §6 string-match, principle 4/7 | low — pure refactor, no new async |
| **1.B** ✅ | Soft-stop axis: classify a *successful* turn's `StopReason` (`MaxTokens` / `MaxTurnRequests` / `Refusal`) into `SoftStopReason` and surface a `ChatMessage::System` line via the separate `AppEvent::AgentSoftStop` event — **off** the `AgentFailure` axis, session stays `Connected`. `EndTurn`/`Cancelled` classify to `None`. (`protocol/acp/soft_stop.rs`, `target=soft_stop` log, 89-locale strings.) | silent truncation/refusal | low — pure additive, no new async |
| **1.5** | Preserve in-flight prompt text on non-`Cancelled` failure (restore to composer only if empty) | principle 1 | low |
| **2** | Prompt inactivity watchdog | §6 hung-agent | low — reuses cancel path |
| **3** | Helper exits on master EOF; no reconnect state | §5 helper cleanup | low — terminal failure semantics |
| **4** | Remove C++ dead-pane re-warm and automatic session load | §5 / F2 / F5 | low — deletes recovery races |
| **5** ✅ | Master bounded `session/cancel`/`session/close` plus local retirement on disconnect | §6 / F8 leak | low |

Phase 1 stands alone and is worth landing first: it removes the only fragile
classification in the codebase and is the substrate every later phase keys off.

## 7. Failure-point status after this design

Re-mapping the `F1–F10` table from `connection-resilience.md` §7 to the target
state:

| # | Failure point | Target |
|---|---|---|
| F1 | agent CLI death → whole master down (single point of failure) | **fail closed** via §5.3: master/helpers/panes exit; a later explicit pane open starts a fresh session. In-master agent respawn is deferred and is not current behavior. |
| F2 | master crash → C++ lazy respawn, zombie panes | **fixed** §5.3 fail-closed helper exit |
| F3 | idle master death stayed `Connected` | **fixed** by direct `MasterDisconnected` termination |
| F4 | in-flight prompt death | **fixed** by direct `MasterDisconnected` termination |
| F5 | helper/conpty death → zombie pane | **fixed** §5.3 close-on-exit, no re-warm |
| F6 | handshake/timeout failures | **typed** `HandshakeFailed{stage}` + retry |
| F7 | connecting looked frozen | covered by `Reconnecting`/`Connecting` spinner |
| F8 | agent-side session leak | **fixed** §5.4 |
| F9 | routing to a dead helper | already graceful |
| F10 | Auto-error-handling request dropped in non-`Connected` | expected after terminal failure; a new explicit session starts clean |
| — | **auth mis-routing (new)** | **fixed** typed `AuthRequired` |
| — | **silent hung agent (new)** | **fixed** §5.1 `Unresponsive` |
