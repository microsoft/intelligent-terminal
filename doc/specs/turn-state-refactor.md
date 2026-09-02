# TurnState Refactor — Plan v2

**Status**: Plan, not implemented
**Scope**: `tools/wta/src/app.rs`, `tools/wta/src/ui/*.rs`, new `tools/wta/src/app/turn_state.rs`
**Estimated effort**: 9.5–10.5 h, split across 4 steps

## Why

The per-turn lifecycle in `TabSession` is currently encoded in 10+ scattered
boolean / Option fields plus 4 App-level fields. Each event handler (Enter,
AgentMessageChunk, AgentMessageEnd, Esc, Auto error handling request) reads and
writes its own subset of them. Recent bugs all trace to the same root cause:
adding a flag for one concern (`eagerly_finalized`) collided with an existing
"clean up" function (`clear_recommendations`) that was reused across four
unrelated intents (new turn / execute card / Esc cancel / explain fallback).

Goal: replace the scattered fields with one explicit state machine. Every
user action and every ACP event maps to a transition. Each transition owns
its own cleanup; nothing leaks between intents.

## Decisions

| ID | Decision |
|---|---|
| #1 | Drop `pending_thought_response`. `AgentThoughtChunk` only drives the state into `Streaming`; no buffer accumulation. |
| #2 | The Auto error handling context generation snapshots the current generation at submit time. `observe_chunk` / `close_turn` compare against current; mismatch = stale, drop. |
| #3 | `Idle → Submitted` explicitly clears `messages`, `tool_calls`, `permission`, `chat_scroll`. No more relying on the `clear_chat_history` side effect. |
| #4 | `Surfaced { end_pending: true }` **rejects** new prompt submissions (consistent with ACP single-flight). Execute card and Esc cancel are still allowed. |
| #5 | One-shot cutover, not dual-track. Invariant assertions step is dropped. |
| #6 | `TurnState` is pure data + small pure helpers. All complex transitions live on `App` methods. |

## Types

```rust
// per-tab — replaces ~10 scattered fields on TabSession
enum TurnState {
    Idle,
    Submitted(SubmittedPrompt),
    Streaming { prompt: SubmittedPrompt, buf: String },
    Surfaced {
        prompt: SubmittedPrompt,
        outcome: TurnOutcome,
        end_pending: bool,   // true until AgentMessageEnd arrives
    },
}

struct SubmittedPrompt {
    id: u64,
    text: String,
    submitted_at_unix_s: f64,
    auto_error_handling: Option<AutoErrorHandlingContext>,
}

struct AutoErrorHandlingContext {
    target_pane_id: String,
    generation: u64,   // snapshot of App.auto_error_handling_generation at submit time
}

enum TurnOutcome {
    Recommendation(RecommendationSet),   // card visible (Auto error handling Fix / planner task unified)
    ChatTurn,                            // prose / explain text already committed to completed_turns
    Empty,                               // no visible response (cancelled / model returned nothing parseable)
}
```

### Pure helpers on `TurnState` (unit-testable)

```rust
impl TurnState {
    fn is_idle(&self) -> bool;
    fn is_streaming(&self) -> bool;
    fn is_surfaced(&self) -> bool;
    fn accepts_new_prompt(&self) -> bool;     // Idle | Surfaced { end_pending: false }
    fn buffer(&self) -> Option<&str>;          // streaming buffer if Streaming
    fn recommendations(&self) -> Option<&RecommendationSet>;
    fn prompt(&self) -> Option<&SubmittedPrompt>;
    fn auto_error_handling_generation(&self) -> Option<u64>;
}
```

## App transition methods

All business logic and side effects live here.

```rust
impl App {
    fn submit_prompt(&mut self, session_id: &str, prompt: SubmittedPrompt);
    fn observe_chunk(&mut self, session_id: &str, kind: ChunkKind, text: &str);
    fn try_eager_surface(&mut self, session_id: &str);
    fn close_turn(&mut self, session_id: &str);
    fn execute_card(&mut self, session_id: &str);
    fn cancel_turn(&mut self, session_id: &str);
    fn user_can_submit(&self) -> bool;        // = current tab's turn.accepts_new_prompt()
}

enum ChunkKind { Thought, Message }
```

Each method:
- Computes the state transition by reading `tab.turn`.
- Mutates `tab.turn` to the new state (or leaves it unchanged for ignored cases).
- Performs the corresponding side effects: clearing orthogonal tab fields,
  emitting Auto error handling state events, calling `log_selection_phase_for`,
  dispatching to the coordinator, updating
  `TabSession.auto_error_handling.result_pane_id`.

### Why `execute_card` does not return to `Idle` immediately

ACP single-flight is held by the in-flight `session/prompt` RPC, which is only
released when `AgentMessageEnd` arrives. If `execute_card` transitioned to
`Idle`, the user could submit a new prompt while ACP still rejects it with
"busy". Instead:

```
Surfaced { Recommendation, end_pending: true }
  --[execute_card]--> Surfaced { Empty, end_pending: true }
  --[AgentMessageEnd]--> Surfaced { Empty, end_pending: false }
  --[user submits new]--> Idle → Submitted
```

The visual UI clears as if the turn ended (card gone, spinner off), but the
state preserves the single-flight gate.

## Transition table

| Trigger | Source state | Target state | Side effects |
|---|---|---|---|
| User submits prompt (chat or planner) | `Idle` / `Surfaced{end_pending:false}` | `Submitted` | Clear messages / tool_calls / permission / chat_scroll. Push User msg. Send to ACP. Log `prompt_received`. |
| User submits while busy | `Submitted` / `Streaming` / `Surfaced{end_pending:true}` | unchanged | Push System "Agent is busy, wait…" message. |
| Auto error handling sends a failure to the agent | any | Submitted with Auto error handling context | Increment the internal generation. Snapshot it into the submitted prompt. Clear messages / tool calls. Emit the pending state. Log. |
| `AgentThoughtChunk` | `Submitted` / `Streaming` | `Streaming` (buf unchanged) | Log. |
| `AgentMessageChunk` (first) | `Submitted` | `Streaming(buf=text)` | Log. |
| `AgentMessageChunk` (later) | `Streaming` | `Streaming(buf+=text)` | Call `try_eager_surface`. |
| `AgentMessageChunk` | `Surfaced` (trailing) | unchanged | Drop. |
| `AgentMessageChunk` | stale (gen mismatch) | unchanged | Drop. |
| Eager surface (Fix / planner Recommendation) | `Streaming` | `Surfaced{Recommendation, end_pending:true}` | Emit the armed state for Auto error handling. Log the selection phase. |
| Eager surface (Auto error handling Explain) | `Streaming` | `Surfaced{ChatTurn, end_pending:true}` | Commit the completed turn. Emit the result state and record its pane. |
| `AgentMessageEnd` (no eager fired) | `Streaming` | `Surfaced{...}` | Final parse. Same emits / logs as eager path but with non-`_eager` phase names. |
| `AgentMessageEnd` (after eager) | `Surfaced{end_pending:true}` | `Surfaced{end_pending:false}` | Log `prompt_complete`. ACP single-flight released. |
| `AgentMessageEnd` | stale | unchanged | Drop. |
| User executes card | `Surfaced{Recommendation, ep}` | `Surfaced{Empty, ep}` | Dispatch `ChoiceExecution` to coordinator. Clear the Auto error handling state when applicable. Log. |
| User Esc cancel | `Submitted` / `Streaming` / `Surfaced` | `Idle` | Increment the generation so stale chunks cannot pollute the next Auto error handling request. Clear the state when applicable. Log. |
| User Enter on history turn | `Idle` / `Surfaced` | unchanged | Toggle CompletedTurn.expanded. |

## Field disposition

### Removed from `TabSession`

- `prompt_in_flight` — derive: `!tab.turn.is_idle()`
- `agent_streaming` — derive: `tab.turn.is_streaming()`
- `pending_agent_response` — moved to `Streaming { buf }`
- `pending_thought_response` — deleted entirely
- `progress_status` — derive: `tab.turn.spinner_label()` (function pure on state)
- `recommendations` — moved to `Surfaced { outcome: Recommendation(_) }`
- `pending_completed_turn` — deleted; `Surfaced` transition commits directly
- `eagerly_finalized` — subsumed by `end_pending`
- `current_prompt_id` / `current_prompt_text` / `current_prompt_submitted_at_unix_s` — moved to `SubmittedPrompt`

### Removed from `App`

- The internal Auto error handling target pane moved into the submitted prompt.
- The internal in-flight generation moved into the submitted prompt.

### Kept on `TabSession` (orthogonal to turn state)

- `messages` (in-flight chat: tool calls, system msgs, errors) — cleared on submit
- `tool_calls` — cleared on submit
- `permission` — cleared on submit
- `completed_turns` — chat history, cross-turn
- `input` / `cursor_pos` / `command_popup_*` — input editor
- `selected_recommendation` / `selected_button` / `rec_scroll` / `selection_visible_pending` — card-internal focus
- `selected_completed_turn_idx` — history navigation focus
- `chat_scroll` — chat scrollback
- `activity_frame` — shimmer animation phase (independent timer)

### Kept on `App`

- The internal generation counter increments for each Auto error handling request and Esc cancellation.
- The result pane ID drives the bottom-bar explanation indicator and is mutated by transitions.
- `pane_id` / `tab_id` / `window_id` — protocol bindings

## Single-flight semantics

Two gates exist and both must be respected:

| Gate | Held by | Releases on |
|---|---|---|
| UI input gate | `tab.turn.accepts_new_prompt()` returns false | Surfaced → end_pending=false, or Idle |
| ACP transport gate | `in_flight_tabs.contains(&tab_key)` | `complete_prompt_request` after `prompt_fut` resolves |

These are now aligned: ACP releases when `AgentMessageEnd` arrives, which is
the same moment the UI gate flips `end_pending=false`. Before this refactor
the UI gate (`prompt_in_flight`) was flipped at eager-surface time, which is
~8s earlier on Windows — a window in which the user could submit a new
prompt and have it rejected by the ACP layer. v2 closes that gap by keeping
the UI gate held until ACP single-flight releases.

## Implementation steps

### Step 1 — Define types (~1.5 h)

- New file `tools/wta/src/app/turn_state.rs`.
- Define the turn state, submitted prompt, Auto error handling context, outcome,
  and chunk kind.
- Implement pure helpers listed above.
- Add unit tests covering each helper for every state variant.
- `cargo test` passes. No integration with `app.rs` yet.

### Step 2 — Define App transition methods (~2 h)

- Add `App::submit_prompt`, `observe_chunk`, `try_eager_surface`,
  `close_turn`, `execute_card`, `cancel_turn`, `user_can_submit`.
- These methods own all side effects: clearing orthogonal tab fields, emitting
  bottom-bar events, logging selection phases, dispatching to the coordinator,
  updating the Auto error handling result pane.
- Methods exist but no event handler calls them yet — `cargo build` passes
  with unused-method warnings.

### Step 3 — One-shot cutover (~4–5 h)

- Rewrite `AgentMessageChunk` / `AgentThoughtChunk` / `AgentMessageEnd` /
  Enter / Esc / internal Auto error handling handlers to call only the new App
  methods.
- Delete the old eager-finalization and response-surfacing helpers for
  Auto error handling, planning, recommendations, and ordinary agent responses —
  their logic is absorbed by the new transition methods.
- Migrate UI renderers in `chat.rs`, `layout.rs`, `recommendations.rs`,
  `input.rs` to read `tab.turn.*` accessors instead of the removed fields.
- Delete obsolete fields from `TabSession` and `App` (listed above).
- Fix all compile errors.
- Preserve all `prompt_timing` phase log names:
  prompt receipt, planner readiness, first transport/event/text, selection
  readiness, Auto error handling fix/explanation/ignore outcomes, and prompt
  completion.

### Step 4 — Scenario verification + integration tests (~2 h)

Manual scenarios — each must match the transition table:

- Auto error handling typo: `listdir` → Fix card surfaces, Enter dispatches `Get-ChildItem`.
- Auto error handling install: `claude` (uninstalled) → Explain chat turn, bottom-bar result.
- Planner chat: "Why is the sky blue?" → prose streams in, commits as chat turn.
- Planner task: "How many items in this folder?" → prose preamble streams,
  card surfaces with the proposed command.
- Execute-during-eager race: surface eager, press Enter before
  AgentMessageEnd arrives. Card dispatches, no duplicate completed turn.
- Esc mid-stream: cancel during streaming, no orphan state.
- Stale Auto error handling request: a new error fires while the old request is
  streaming. The old response is dropped at chunk-level via generation check.
- Back-to-back prompts: submit one, wait for full completion, submit another.
  Both render correctly.
- New-prompt-while-eager: submit one, eager surface arrives, attempt new
  submit before AgentMessageEnd. Should be rejected with "wait" message
  (consistent with ACP single-flight).

Integration tests:
- 2–3 tests in `app.rs` `#[cfg(test)]` that drive `App` methods with a
  mocked sequence of `AppEvent`s and assert resulting `tab.turn` and
  observable side effects (logs / completed_turns / coordinator dispatches).

## Risks

1. **Step 3 is large** — 4–5 h of mechanical changes touching many files.
   Strategy: complete on a branch, verify all scenarios pass, then optionally
   split into reviewable commits at the end (cosmetic, doesn't change
   correctness).

2. **`activity_frame` lifecycle** — animation phase is a tab field but
   semantically belongs to "spinner state". Decision: keep as orthogonal
   field; `TurnState::spinner_label()` returns Option<&str>, animation runs
   if Some.

3. **Internal state-event call sites** — every Auto error handling transition has a
   corresponding bottom-bar emit. Easy to miss one during cutover. Mitigation:
   the transition table is the checklist.

4. **No existing test coverage of turn lifecycle** — Step 1 unit tests only
   cover pure helpers. Step 4 adds 2–3 integration tests. Manual scenario
   testing is still primary verification.

5. **ACP single-flight release timing** — `dispatch_prompt_body` removes
   `in_flight_tabs` after `prompt_fut` resolves. This stays unchanged, but the
   refactor must verify `tab.turn.end_pending` flips false at the same logical
   moment.

## Out of scope

- ACP layer single-flight relaxation
- Multiple concurrent turns per tab
- Chat history pagination
- Hook delay optimization (deleting Stop / SessionEnd from `hooks.json`)
- Prompt shrinking / model swap (Haiku)
- Tool call display revamp

## Out-of-band dependencies

None. The refactor is local to `tools/wta/src/`. Protocol layer (`acp/client.rs`),
COM server, and hook scripts are not touched.

## Future work (post-refactor)

- Once turn state is explicit, adding more outcomes (`PermissionDenied`,
  `TimedOut`, `ModelRefusal`) is mechanical.
- The hook-delay optimization (deleting Stop / SessionEnd) becomes trivial:
  ACP `PromptResponse` arrives sooner, `close_turn` fires sooner, no other
  code path needs updating.
- A planner-mode "delegate to subagent" outcome could be added as a
  `TurnOutcome::Delegated` variant without restructuring.
