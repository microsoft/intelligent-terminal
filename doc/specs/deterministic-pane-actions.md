# Model-Prepared, Deterministic Pane Actions

Author: vanzue
Date: 2026-08-12
Branch: `dev/vanzue/deterministic-pane-slash-commands`
Status: Proposed

## Summary

Deterministic pane actions use the helper's source pane as their working
environment:

- A plain prompt follows the normal ACP workflow with source-pane context.
- `/command <intent>` and its `/cmd` alias ask the model for exactly one
  terminal command, then show a **Run / Insert / Adjust** card.
- `$ <command>` bypasses the model and runs literal input in the source pane.
- `/delegate <intent>` creates a new tab or panel and prepares a prompt for the
  configured delegate agent.

The model decides what text to propose, but never where or how it executes.
The helper snapshots the protocol pane GUID and requested operation before
submission. Only a user-confirmed, locally constructed action card may send
model-prepared input to Windows Terminal.

## Design principles

1. **Source-owned targeting.** Plain prompts, `/command`, `/cmd`, and `$` bind
   to the helper source pane. Typing or focus changes do not retarget them.
2. **Host-owned mechanics.** Target binding, destination creation, agent
   launch, and Run/Insert/Adjust selection never come from the model.
3. **Confirmation boundary.** A model result is a proposal card; only explicit
   user confirmation executes it.
4. **Event-driven metadata.** Windows Terminal publishes pane snapshots and
   each helper maintains an in-memory catalog for source metadata, target
   validation, and exact invalidation.
5. **Stable target.** A submitted command and every later adjustment carry the
   same snapshotted target GUID.
6. **No silent fallback after submission.** If that target disappears, the
   command is canceled or its card expires. It never moves to the focused pane.

## Source-pane behavior

The helper receives its source pane GUID when Windows Terminal launches it.
For every normal prompt, WTA supplies that GUID plus the source cwd and cached
catalog metadata when available. A missing catalog descriptor does not cause a
focus-based substitution; the known source GUID remains authoritative.

During early bootstrap, a helper may not yet have a source GUID. Deterministic
command entry points may then use the catalog's active writable, visible,
non-agent pane. This compatibility fallback is selected before submission and
is snapshotted immediately. It is never recomputed from later focus changes.

## Require a command card

```text
/command install this project's dependencies
```

`/cmd` is an alias with identical behavior.

On Enter:

1. WTA snapshots the helper source pane GUID.
2. It resolves the matching descriptor from the catalog using normalized GUID
   comparison, so braced, unbraced, and case-varied forms refer to one pane.
3. It submits the user's intent, source shell/title/cwd and recent output, plus
   a requirement to return only exact terminal input.
4. At the ACP turn boundary, WTA treats the complete assistant message as the
   literal payload and locally constructs one `send` action.
5. The action is bound to the snapshotted source GUID. The model cannot choose
   or override the target.
6. WTA shows **Run**, **Insert**, and **Adjust** actions.

Run removes trailing line endings and sends exactly one Enter. Insert removes
trailing line endings and sends no Enter.

Adjust opens a natural-language refinement editor. Its request always contains:

- the original user intent;
- the previous command;
- the current adjustment;
- the original command card's snapshotted target and metadata.

The adjustment is semantic feedback about the previous command, not replacement
terminal input. Relative wording such as "here", "this", or "that" is resolved
against the original intent, previous command, and terminal context. The model
must use the previous suggestion as the revision baseline, preserve every
unaffected part, and return a complete revised command rather than regenerating
from the feedback alone, echoing the feedback, or returning a diff.

Changing source state while Adjust is open cannot redirect the revision.

Command history presents adjustments as a two-level revision tree. The
original `/command` turn is the root. Every later adjustment is a sibling child
of that root, including a correction derived from the immediately preceding
revision. A submitted prior version is marked **Adjusted**.

## Run literal input

```text
$ cargo test parser
```

WTA snapshots the same source target, bypasses ACP, and sends the literal
command through the host-owned terminal action path with exactly one trailing
Enter. No model proposal card is created.

## Delegate

```text
/delegate investigate why the parser tests are flaky
```

1. WTA opens a Tab/Panel destination picker and remembers only the last
   destination kind.
2. On confirmation it starts two independent branches:
   - Windows Terminal creates the destination and launches the delegate agent.
   - The current ACP model prepares a concise delegate prompt from the literal
     intent and source-pane context.
3. WTA joins the branches by internal operation ID.
4. Once both the new pane GUID and prepared payload exist, it shows one
   **Send** card bound to the new pane.
5. User confirmation sends the prompt plus Enter.

All intent text after `/delegate` is ordinary free text. Delegation's execution
target is always the host-created destination pane, not a parsed mention.

## Pane catalog

Each helper retains a catalog for its owning tab:

```rust
struct PaneCatalogState {
    generation: u64,
    panes: HashMap<NormalizedPaneSessionId, PaneDescriptor>,
}
```

Descriptors include the protocol session ID, ordinal, title, profile, cwd,
shell, active status, visibility, read-only status, and whether the pane is an
agent pane.

The catalog is internal infrastructure, not a persistent user-selected target.
It supports:

- normalized source descriptor lookup;
- cached shell/cwd/title/profile context;
- bootstrap-only active writable pane resolution;
- delegate panel parent selection;
- Adjust target validation;
- exact command invalidation.

### Snapshot delivery

Windows Terminal sends an authoritative `pane_catalog_changed` snapshot after
initialization and relevant structural or metadata changes:

```json
{
  "method": "pane_catalog_changed",
  "params": {
    "window_id": "1",
    "tab_id": "42",
    "generation": 17,
    "panes": []
  }
}
```

Snapshots are scoped by window and stable owner tab. Helpers ignore another
owner's snapshot and generations older than the current one. A malformed
snapshot is ignored atomically; it cannot replace the current catalog or
invalidate a command.

Pane IDs are normalized before becoming map keys. A target remains present
across braced/unbraced or case-only GUID representation changes.

### Invalidation

Only a command preparation or command card with an exact snapshotted target is
invalidated when a newer valid snapshot removes that target. Ordinary chat,
delegate preparation, unrelated cards, and a command whose normalized target
is still present are not canceled.

The catalog never performs a picker-time query. `SendInput` remains the final
authoritative liveness check.

## Model preparation contract

For `/command`, the model returns only literal terminal input in its final
assistant message. WTA rejects model-authored terminal action proposals during
command preparation and constructs the card itself.

The preparation prompt requires the model to:

- understand the intent using supplied source terminal context;
- return only exact terminal input;
- avoid tools, Markdown, explanation, execution, and pane selection;
- treat the full response as literal terminal input.

Trusted target GUID and execution mode remain outside the model-visible action
schema.

## Card and chip behavior

| Action | Behavior |
|---|---|
| Run | Remove trailing line endings, then send one Enter |
| Insert | Remove trailing line endings and send no Enter |
| Adjust | Prepare a revision against the same target; execute nothing |
| Delegate Send | Send the prepared prompt plus one Enter to the new pane |

The Agent chip may temporarily follow the active command card's snapshotted
target so the confirmation UI communicates where Run or Insert will act.
Outside card interaction, chip placement remains source-driven. No input text
or persistent helper state moves the chip.

## ACP slash autocomplete

Client slash commands and metadata advertised by the current ACP session share
the existing autocomplete pipeline. Completion behavior metadata controls
whether Enter executes immediately, opens a picker, accepts optional free text,
or requires free text. Deterministic pane actions do not replace, filter, or
special-case agent-provided slash commands.

## Failure behavior

| Condition | Behavior |
|---|---|
| No source GUID during bootstrap and no active writable pane | Refuse deterministic command submission |
| Malformed pane snapshot | Ignore it and preserve catalog and in-flight work |
| Command target removed | Cancel or expire only that exact command |
| Model returns empty command text | Mark preparation failed; execute nothing |
| Model calls the proposal tool during `/command` | Reject it and await final command text |
| Adjust target missing | Keep the adjustment editor and execute nothing |
| Delegate launch fails | Cancel the operation and discard later preparation |
| Delegate preparation fails after launch | Keep the interactive delegate pane open |
| `SendInput` fails | Keep the card and payload for retry; never retarget |
| ACP transport is lost | Disable preparation; `/restart` remains available |

## Test matrix

Rust tests cover:

- source target selection for normal prompt, `/command`, `/cmd`, and `$`;
- bootstrap fallback eligibility;
- Adjust preserving original intent, prior command, current correction, and
  target;
- two-level revision history;
- normalized catalog lookup and refresh;
- malformed snapshot protection;
- exact command invalidation without canceling ordinary prompts;
- Run/Insert/Adjust newline and dispatch behavior;
- host ownership of target GUID and execution mode;
- delegate host/model join behavior;
- literal handling of punctuation in ordinary and delegate intent;
- metadata-driven ACP slash autocomplete.

Runtime verification should confirm that focus changes after submission do not
retarget cards, command target removal fails closed, and card-driven chip
placement releases when the card interaction ends.
