# Model-Prepared, Deterministic Pane Actions

Author: vanzue
Date: 2026-08-12
Branch: `dev/vanzue/deterministic-pane-slash-commands`
Status: Proposed

## Summary

Add orthogonal target selection, model-driven command cards, and literal
command execution:

- `@pane` selects the persistent working environment for this tab's Agent.
- A plain `<prompt>` follows the normal Copilot workflow, with the selected
  pane supplied as preferred terminal context.
- `/command <intent>` (`/cmd` alias) requires Copilot to produce exactly one
  terminal command proposal. WTA shows a **Run / Insert / Adjust** card.
- `$ <command>` bypasses the model and runs the literal command in the selected
  pane.
- `/delegate <intent>` immediately creates a new tab or panel and starts the
  configured delegate agent. In parallel, the current model prepares the prompt.
  When both sides are ready, WTA shows a Send card bound to the new agent pane.

The model decides **what text to propose**, but never where or how it executes.
The helper owns the target pane GUID and requested operation. The user confirms a
locally constructed action card before WT receives any input.

The pane picker reads an event-driven pane catalog held in helper memory. It
does not call `list_panes` when opened or while the selection changes.

## Design principles

1. **Orthogonal syntax.** `@` selects an environment, `/command` constrains the
   model result, and `$` marks literal input. None of these concepts implicitly
   changes another.
2. **Host-owned mechanics.** Pane selection, tab/panel creation, agent launch,
   target binding, and Run/Insert/Adjust choice never come from the model.
3. **Confirmation boundary.** A model result becomes a proposal card; only a
   user click/keypress executes it.
4. **In-memory pane discovery.** WT publishes pane snapshots after structural or
   metadata changes; each helper maintains its own current-tab catalog.
5. **No retargeting.** A card carries the exact protocol session GUID selected
   or created by the host. Focus changes cannot alter it.
6. **No silent fallback.** A missing target invalidates the card. It never falls
   back to the active pane.

## User experience

### Select a pane with `@`

1. The user types `@` in the WTA input.
2. WTA filters its in-memory catalog to writable, visible, non-agent panes in
   the helper's owning tab.
3. A popup appears above the input.
4. Up/Down changes the selected row. WT moves the existing Agent chip to the
   selected real pane without moving keyboard focus.
5. Enter commits the pane as the helper's persistent Agent target and removes
   the selector text. WTA stores the pane GUID separately from the UI label.

The popup can show pane ordinal, title/process, shell, and cwd. If catalog data
changes while it is open, WTA preserves selection by GUID when possible.

### Normal prompt

```text
why did the last command fail?
```

The text follows the ordinary Copilot workflow. The selected pane's GUID,
shell/title/cwd, and recent output are supplied as preferred context. `@` does
not force a terminal action card.

### Require a command card

```text
/command install this project's dependencies
```

On Enter:

1. WTA submits a command-proposal turn to the current ACP model with:
   - the user's intent;
   - the persistent target, or the default source pane when no `@` selection
     has been made;
   - target shell/title/cwd and recent output;
   - a requirement to return only the exact terminal input as assistant text.
2. At the ACP turn boundary, WTA treats the complete assistant message as the
   literal payload and locally creates exactly one `send` action.
3. WTA binds that host-created action to the selected GUID snapshotted when the
   turn was submitted. The model cannot choose or override the target.
4. WTA shows the command card with **Run**, **Insert**, and **Adjust** actions.
5. Run sends exactly one trailing Enter; Insert sends no trailing Enter.
6. Adjust focuses the input for a natural-language correction. WTA submits the
   original intent, current command, correction, and the same snapshotted pane
   context through the literal-output-only preparation path, then replaces the
   card without executing it.

Command history presents adjustments as a revision tree with at most two
levels. The original `/command` turn is the root. Every later adjustment is a
sibling child of that root, even when it was derived from the immediately
preceding revision. Once a correction is submitted, the prior version is
marked **Adjusted** rather than canceled, failed, or left with an ambiguous
unexecuted state.

The model may rewrite prose into a shell command, complete a partial command, or
prepare text appropriate for an interactive program already running in the
pane. It cannot execute the result.

`/cmd` is an alias. `/command @` and `/cmd @` open the same pane picker; after
selection, the input returns to the command prefix and the user enters intent.

### Run literal input

```text
$ cargo test parser
```

WTA bypasses Copilot and sends the literal command plus exactly one trailing
Enter to the snapshotted persistent target. With no explicit `@` selection it
uses the default source pane. This path does not create a model proposal card.

### Delegate

```text
/delegate investigate why the parser tests are flaky
```

1. WTA opens a Tab/Panel destination picker, preselecting the last confirmed
   choice for this helper/tab.
2. On confirmation WTA starts two independent branches:
   - **Host branch:** create the tab/panel and immediately launch the configured
     delegate agent interactively, without a startup prompt.
   - **Model branch:** ask the current ACP model to turn the user's intent and
     current terminal context into a concise delegate prompt.
3. WTA joins the two branches by an internal operation ID.
4. Once the new pane GUID and model payload both exist, WTA shows a single
   **Send** card bound to the new agent pane.
5. User confirmation sends the prepared prompt plus Enter to that pane.

The tab/panel and delegate process can therefore become visible before model
preparation completes. If preparation fails, the new interactive delegate stays
open and the original intent remains available for retry.

`@pane` is not valid inside `/delegate`. Delegation always creates a new
destination; the selected destination pane comes from host creation, not user
mention or model output.

## Pane catalog

### Ownership

Each helper keeps a catalog for its owning tab:

```rust
struct PaneCatalog {
    generation: u64,
    panes: HashMap<PaneSessionId, PaneDescriptor>,
}

struct PaneDescriptor {
    session_id: PaneSessionId,
    tab_id: String,
    window_id: String,
    ordinal: u32,
    title: String,
    process_name: String,
    shell: Option<String>,
    cwd: Option<String>,
    visible: bool,
    read_only: bool,
    is_agent: bool,
}
```

The protocol GUID is the key. Display labels and ordinals are mutable metadata.

### Snapshot delivery

Do not maintain the catalog with picker-time queries or fragile individual
deltas. WT emits an authoritative full snapshot:

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

WT sends a snapshot:

- when a helper attaches or its owning tab is initialized;
- after pane create, split, close, detach, attach, move, hide, or restore;
- after title, cwd, read-only, process, or agent-pane metadata changes that
  affect picker display or eligibility.

Snapshots are scoped by both `window_id` and stable owner `tab_id`. The helper
ignores snapshots for another owner and generations older than its current one.
Full snapshots make missed/coalesced events self-healing and make create/close
ordering irrelevant.

The picker only reads `PaneCatalog`. Sending input also does not refresh the
catalog. WT's `SendInput` result is the authoritative final liveness check.

### Catalog lifecycle

- A missing GUID in a newer snapshot removes the candidate and invalidates any
  pending selection, persistent Agent target, or action card bound to it.
- Stashing the agent pane does not discard the catalog.
- Tab close clears the catalog.
- Helper reconnect requests one fresh snapshot before enabling `@`.
- Until the initial snapshot arrives, `@` shows a loading state rather than
  querying WT.

## Pane preview

The picker reuses the existing `set_agent_chip_target` route. While the picker
is open, the helper publishes the highlighted pane GUID as the temporary chip
target. Committing a pane stores a per-tab persistent Agent target and keeps the chip
there. Escape, Backspace, and Delete cancel only the uncommitted picker/input
state. Removing the target pane releases the override and restores source-driven
chip placement.

No border state is added to `Pane`, and preview never moves keyboard focus or
reuses input-broadcast state.

The persistent target is the default context for ordinary prompts,
`/command`, `/fix`, and `$`. Every submitted turn or direct operation snapshots
the target GUID, so changing the persistent target cannot redirect work already
in flight.

## Model preparation contract

### Host-synthesize command proposals

For `/command`, the model returns only literal terminal input in its final
assistant message. At `AgentMessageEnd`, the helper consumes the complete
message and locally constructs the single `send` action. The model does not
decide whether to call `request_terminal_actions`; such calls are rejected
during command preparation.

Add an internal preparation mode to `PromptSubmission`:

```rust
enum PreparationKind {
    TerminalCommand,
    DelegatePrompt { operation_id: u64 },
}

enum PreparedActionMode {
    Command,
    SendToDelegate,
}
```

The helper supplies trusted context containing the bound target GUID or pending
delegate operation. None of this trusted context is serialized into the
model-visible action schema.

### Preparation prompt

Add dedicated prompt templates:

- `prepare-pane-input.md` (terminal command proposal)
- `prepare-delegate-prompt.md`

The command preparation prompt tells the model:

- understand the user's intent using supplied terminal context;
- return only the exact terminal input as the final assistant message;
- do not call tools, use Markdown, explain, execute, or choose a pane;
- treat the full response as literal terminal input.

The delegate preparation prompt still uses `request_terminal_actions` with one
`send` action because its host-created destination and model payload complete
independently. It also tells the model not to perform the delegated task and to
end without additional assistant prose.

The command template requires a shell command without explanatory prose or its
own newline. The delegate template includes the configured delegate name/model
and asks for a self-contained task prompt rather than a shell command.

### Authoritative validation

When a proposal belongs to a preparation turn, the helper accepts only:

- one choice;
- one action;
- `ProposalActionWire::Send`;
- non-empty input within the existing size limit;
- the same live preparation operation and prompt ID.

`open`, `open_and_send`, multiple choices, or a second tool call reject the
proposal. The helper constructs a new local card from the validated payload and
trusted target:

```rust
struct PreparedPaneAction {
    operation_id: u64,
    target_session_id: PaneSessionId,
    payload: String,
    mode: PreparedActionMode,
}
```

The model never supplies `target_session_id` or `mode`.

### Card behavior

Command preparation uses a three-button terminal card:

| Action | Behavior |
|---|---|
| Insert | trailing CR/LF removed |
| Run | trailing CR/LF removed, then one `\r` |
| Adjust | recompile from the original intent, current command, and correction; execute nothing |

Delegate preparation remains a single **Send** action with exactly one trailing
`\r`.

Confirmation is the only execution point. If the catalog no longer contains
the target, or a `pane_catalog_changed` snapshot removed it, the card becomes
expired. A failed `SendInput` keeps the card and payload for retry.

## Delegate orchestration

### Host branch

Factor delegate process construction from the existing CLI/coordinator path,
but do not use `DelegatePromptDelivery` for this flow. The model-prepared prompt
does not exist at launch time, so the host always:

1. builds an interactive delegate command line with no prompt;
2. creates a tab or splits the current working pane;
3. captures the returned pane GUID;
4. registers the born-bound session when supported;
5. records the GUID against `operation_id`.

Tab creation uses `wt_create_tab`. Panel creation uses `wt_split_pane` with the
current working pane as the parent. The destination picker selects only the
kind; it does not expose or persist a parent GUID.

### Model branch

The current ACP session receives the dedicated delegate-preparation turn.
Terminal context comes from the current working pane, not the newly launched
blank agent pane. The validated Send payload is stored against `operation_id`.

### Join

```rust
struct PendingDelegate {
    operation_id: u64,
    destination: DelegateTarget,
    original_intent: String,
    target_pane_id: Option<PaneSessionId>,
    prepared_payload: Option<String>,
    status: DelegatePreparationStatus,
}
```

The card appears only when both optional values exist. Either branch may finish
first. Stale completions are ignored by operation ID.

Only one pending delegate preparation is allowed per tab for MVP. A second
`/delegate` is rejected until the first produces a card or is cancelled.

## Concurrency and failure behavior

Pane-input preparation and delegate-prompt preparation use the current ACP
session and are refused while a turn is already in flight. `/stop` cancels only
the model branch; it never closes an already-created delegate pane.

| Condition | Behavior |
|---|---|
| Initial pane snapshot not received | `@` shows loading; no query fallback. |
| No eligible pane | Show "No writable panes in this tab." |
| Selected pane removed during preparation | Cancel/expire proposal; keep original intent. |
| Model returns empty command text | Mark preparation failed; execute nothing. |
| Model calls the proposal tool during `/command` | Reject the call and wait for final command text. |
| Model preparation fails after delegate launch | Keep interactive delegate pane open; offer retry. |
| Delegate launch fails while model runs | Cancel operation; discard later proposal by operation ID. |
| Delegate pane closes before confirmation | Expire Send card. |
| `SendInput` fails | Keep card and payload for retry; never retarget. |
| ACP transport is lost | Disable preparation commands; `/restart` remains available. |
| Preview event fails | Keep picker usable without claiming a visible preview. |

No failure converts model-prepared content into literal input automatically.

## End-to-end flows

### `@pane`, then a normal prompt

```text
WT pane snapshots
  -> helper PaneCatalog
  -> @ picker + visual preview
  -> persistent trusted target GUID
plain prompt
  -> ordinary ACP prompt with target context
```

### `/command <intent>`

```text
persistent/default target GUID
  -> ACP command-proposal prompt
  -> model returns exact terminal input as final text
  -> helper locally creates one send action with trusted GUID
  -> helper builds Run / Insert / Adjust card
  -> user chooses Run, Insert, or Adjust
  -> Adjust repeats preparation against the same trusted GUID
  -> wtcli -> COM SendInput -> exact TermControl
```

### `$ <command>`

The helper snapshots the persistent/default target, bypasses ACP, and dispatches
the literal command through the same host-owned SendInput execution path as the
Run action.

### `/delegate <intent>`

```text
                         +-> WT creates Tab/Panel
/delegate + destination -+   -> interactive delegate starts
                         |   -> exact new pane GUID
                         |
                         +-> current model prepares delegate prompt
                             -> request_terminal_actions(send payload)

new pane GUID + validated payload
  -> helper builds Send card
  -> user confirms
  -> SendInput(new pane GUID, payload + Enter)
```

## Implementation plan

### Phase 1: pane catalog and preview

- Define pane catalog event schema and descriptor fields.
- Emit initial and mutation-triggered full snapshots from TerminalApp.
- Maintain per-tab catalog state in the helper with generation filtering.
- Add `@` popup backed only by the catalog.
- Add preview SendEvent routing and transient border state.

Primary files:

- `src/cascadia/TerminalApp/TerminalPage.cpp`
- `src/cascadia/TerminalApp/TerminalPage.Protocol.cpp`
- `src/cascadia/TerminalApp/TabManagement.cpp`
- `src/cascadia/TerminalApp/Pane.*`
- `src/cascadia/WindowsTerminal/TerminalProtocolComServer.cpp`
- `tools/wta/src/app.rs`
- `tools/wta/src/app/tab_state.rs`
- `tools/wta/src/app_events.rs`
- `tools/wta/src/ui/*`

### Phase 2: preparation-turn contract

- Add preparation kinds and trusted target context.
- Add the two prompt templates.
- Restrict preparation proposals to exactly one Send action.
- Convert validated payloads into single-button local cards.
- Preserve original intent for retry and cancellation.

Primary files:

- `tools/wta/src/protocol/acp/prompt_context.rs`
- `tools/wta/src/protocol/acp/prompt_builder.rs`
- `tools/wta/src/protocol/acp/client.rs`
- `tools/wta/src/agent_tools/action_proposal/schema.rs`
- `tools/wta/src/app_turn.rs`
- `tools/wta/src/app_events.rs`
- `tools/wta/prompts/*`

### Phase 3: `@`, `/command`, and `$`

- Add persistent target selection state.
- Add `/command` parsing and `/cmd` alias.
- Submit command-proposal turns with standard Run/Insert cards.
- Add model-bypass `$` execution through the exact-GUID SendInput path.
- Wire card confirmation to exact-GUID `SendInput`.
- Invalidate targets and cards from catalog snapshots.

### Phase 4: `/delegate`

- Add destination picker and per-tab last choice.
- Extract interactive delegate command construction.
- Launch host and model branches under one operation ID.
- Join new pane GUID with validated model payload.
- Show the host-bound Send card and register born-bound sessions.

### Phase 5: integration

- Localize all strings.
- Log operation kind, operation ID, pane GUID, and payload length, never payload
  text.
- Test packaged Debug behavior across tab/pane lifecycle, helper stash/restore,
  agent failure, and ACP reconnect.

## Test matrix

### Rust unit tests

- Pane snapshots replace the catalog atomically and reject stale generations.
- Picker never invokes `list_panes`.
- Picker selection survives metadata updates by GUID.
- Removed panes clear persistent targets and expire cards.
- Committed targets survive subsequent ordinary prompts.
- `/command` and `/cmd` produce the same Run/Insert/Adjust card.
- Adjust preserves the card target and deterministically replaces only its
  command payload.
- Adjusted command versions form sibling children under the original command;
  revision history never nests beyond two levels.
- `$` bypasses the model and binds literal execution to the snapshotted target.
- Preparation accepts one Send and rejects open, open_and_send, multiple
  choices/actions, empty payloads, and stale prompt IDs.
- Insert/Run/Send cards apply the correct newline policy.
- The model cannot change target GUID or execution mode.
- Delegate joins correctly when host or model finishes first.
- Delegate launch failure discards a later model proposal.
- Model failure leaves an already-open delegate pane alone.
- ACP transport loss disables all model-preparation entry points.

### C++ tests

- Initial and mutation snapshots contain the authoritative current tab.
- Snapshot generation is monotonic per helper/tab.
- Preview validates same-tab source and target.
- Preview never moves focus or enables broadcast.
- Source/target close and helper disconnect clear preview state.
- Focused/preview/broadcast border priority is stable.

### Packaged integration tests

1. Open the picker repeatedly and verify no `list_panes` process is spawned.
2. Split/close/move panes and verify popup contents update from snapshots.
3. Move picker selection and verify the real pane border follows without focus
   leaving WTA.
4. Ask for prose intent, verify the model-prepared command appears in a card,
   and verify nothing reaches the pane before confirmation.
5. Confirm Insert and verify no execution; confirm Run and verify one execution.
6. Change focus after selection and verify the bound GUID still wins.
7. Close the target during model preparation and verify no other pane receives
   input.
8. Delegate to Tab and Panel; verify the agent opens immediately while prompt
   preparation is still running.
9. Complete model preparation before and after pane creation to exercise both
   join orders.
10. Fail/cancel either delegate branch and verify deterministic cleanup.

## Acceptance criteria

- Opening or navigating the pane picker makes no `list_panes` call.
- Pane choices come from a helper-owned, event-updated catalog.
- Model preparation produces no terminal side effect by itself.
- Every executable card is built locally from a validated payload plus a
  host-owned GUID and execution mode.
- No content reaches a target before explicit user confirmation.
- Insert never executes; Run and delegate Send append exactly one Enter.
- `/delegate` creates and launches the destination independently of model
  preparation.
- The model cannot select a pane, choose Tab/Panel, start an agent, or change
  Insert/Run/Send semantics.
- Missing or stale targets fail closed without fallback.
