# WTA CLI terminal-action proposals

## Status

Proposed design. This document supersedes the MCP-based proposal work in
PR #428. MCP is not part of the repository architecture and is not a transport
option for this feature.

## Summary

Autofix and Terminal Agent currently turn model-authored JSON in assistant text
into recommendation cards. This works, but card creation depends on extracting
and validating structured data from a streamed chat response.

The replacement keeps the existing card and execution pipeline while moving
proposal submission to a WTA CLI contract:

```text
wta propose-terminal-actions --route <opaque-turn-token>
```

The command is proposal-only. It cannot send input, create panes, launch
delegates, or otherwise mutate Windows Terminal. A valid proposal becomes a
local recommendation card; the existing card confirmation remains the only
path to terminal mutation.

## Goals

- Use WTA CLI, not MCP, for typed terminal-action proposals.
- Expect the agent session to execute the WTA CLI directly.
- Reuse the existing short-lived CLI-to-master ACP connection pattern.
- Share one versioned wire schema between Autofix and Terminal Agent.
- Keep session, helper, tab, window, and pane identifiers out of model-authored
  payloads.
- Reject malformed, stale, duplicate, or wrong-origin proposals without side
  effects.
- Preserve the existing Run, Insert, Open, Split, and Delegate confirmation UI.
- Preserve one user confirmation between proposal and terminal mutation.
- Keep the current assistant-text JSON path as a compatibility fallback until
  each built-in agent proves reliable CLI and permission behavior.

## Non-goals

- Reintroducing an MCP server, MCP route, or MCP dependency.
- Letting the proposal CLI execute terminal actions directly.
- Relying on ACP `create_terminal` to proxy the WTA CLI through the helper.
- Treating model-authored shell text as trusted or intrinsically
  non-destructive.
- Replacing the existing `wtcli` to COM execution path.
- Solving delegate startup latency tracked by #445.

## Current flow

Today both features submit a prompt over ACP and parse the streamed assistant
message:

```text
shell failure or user prompt
    -> wta-helper builds terminal context
    -> helper -> master -> agent CLI over ACP
    -> assistant message chunks
    -> parse_autofix_response / parse_recommendation_set
    -> RecommendationSet
    -> recommendation card
    -> user confirms
    -> recommendation executor
    -> ShellManager -> wtcli -> COM IProtocolServer -> Windows Terminal
```

The execution half is already the desired trust boundary. This proposal changes
only how a typed `RecommendationSet` reaches the helper.

## Proposed flow

```text
1. The helper sends an Autofix or Terminal Agent prompt to wta-master over ACP.
2. Master already knows the source HelperId and ACP session id. It increments
   that session's turn generation, mints a short-lived one-use route token, and
   stores token -> {session id, helper id, generation, expiry}.
3. Master injects the opaque token and CLI instruction into the prompt, then
   forwards the prompt to the agent CLI over ACP stdio.
4. The agent session directly executes `wta propose-terminal-actions`, passing
   the token and versioned proposal JSON.
5. The short-lived WTA CLI discovers and connects to the existing master named
   pipe, performs an ACP initialize, and sends a WTA ExtRequest.
6. Master atomically consumes the token, derives the owning session/helper, and
   forwards the proposal to that helper as an ExtNotification containing the
   trusted session id.
7. The helper routes by session id. App performs the authoritative active-turn,
   Autofix-generation, origin, target, and duplicate checks.
8. App converts an accepted wire proposal into the existing RecommendationSet,
   displays the existing confirmation card, and immediately sends a correlated
   disposition ExtRequest back to master.
9. Master resolves the pending CLI request. The CLI prints "presented" and
   exits, unblocking the agent's command tool. This does not mean an action ran.
10. Only a later user card confirmation sends ChoiceExecution to the existing
    executor and reaches wtcli/COM.
```

No new server is introduced. The proposal command reuses the master named pipe,
ACP handshake, and ExtRequest mechanism already used by `wta sessions list`.

## CLI contract

### Invocation

The agent session executes:

```text
wta propose-terminal-actions --route <opaque-turn-token>
```

The CLI reads one UTF-8 JSON proposal from stdin. Agents whose command tools
cannot provide stdin may use:

```text
wta propose-terminal-actions --route <opaque-turn-token> --payload-file <path>
```

The model must not base64-encode the payload. Payload size is capped before
deserialization. Shell wrapping is allowed when required by the agent's native
command tool, but each built-in agent must prove its quoting and stdin/file
behavior before CLI proposal mode is enabled.

The route token is the only routing input. The CLI does not accept a
model-provided master pipe, session id, helper id, or target id.

### Master discovery

The CLI resolves master in this order:

1. `WTA_MASTER_PIPE`, set by master on the agent process so direct child
   commands inherit the exact pipe;
2. the existing package-private `master-pipe.txt` discovery file used by
   `wta sessions list`.

The route token still must validate on the connected master. A stale discovery
file therefore fails closed instead of routing to another helper.

### Output

For every protocol-complete request, stdout contains exactly one compact JSON
object:

```json
{"schema_version":1,"status":"presented"}
```

Defined statuses:

| Status | Meaning |
|---|---|
| `presented` | The proposal was accepted and a card was displayed. No action has run. |
| `duplicate` | This active prompt already surfaced an equivalent proposal. |
| `stale` | The prompt, Autofix generation, or target context is no longer active. |
| `rejected` | Schema or origin policy rejected the proposal. |
| `unavailable` | The master/helper route or required target context is unavailable. |

Protocol-complete dispositions exit with code 0 so agents do not retry rejected
or stale proposals as transport failures. Nonzero exit codes are reserved for
invalid CLI syntax, unreadable payloads, broken master transport, or internal
failures.

stderr is diagnostic-only; stdout contains only the disposition object.

## Wire schema

The public CLI schema is separate from the internal `RecommendationSet`:

```json
{
  "schema_version": 1,
  "origin": "terminal_agent",
  "recommended_choice": 1,
  "choices": [
    {
      "choice": 1,
      "title": "Run tests",
      "rationale": "Uses the active shell and working directory.",
      "actions": [
        {
          "type": "send_input",
          "input": "cargo test"
        }
      ]
    }
  ]
}
```

The wire types use `deny_unknown_fields`, explicit size/count limits, and
hand-written conversion to internal types. They never accept:

- ACP session ids
- helper ids
- window, tab, or pane ids
- proposal pipe names or capability tokens
- arbitrary executable paths

Supported actions:

- `send_input`
- `open`
- `open_and_send`

`open` and `open_and_send` may describe `tab` or `panel`, cwd, title, profile,
direction, and whether the destination is the configured delegate. The helper
injects the real parent pane and resolves the configured delegate runtime.

## Trusted binding and freshness

Master mints the route token while handling the helper-originated prompt, where
it already has the source HelperId and ACP session id. The token registry is
bounded and each entry contains:

- ACP session id;
- source HelperId;
- master-owned per-session turn generation;
- expiry.

Starting a newer prompt invalidates the previous token for that session. The
token is consumed atomically on the first proposal submission. The CLI payload
contains no routing identifiers.

App remains authoritative for state that the ACP client does not own:

- current `TurnState`;
- Autofix generation;
- failing pane recorded by `AutofixContext`;
- active pane captured for a Terminal Agent prompt;
- whether a recommendation already surfaced;
- configured delegate availability.

The token is visible to the agent session and may be retained in its transcript.
One-use, short expiry, per-turn invalidation, and master-owned routing limit its
authority to proposing one card for the session that received it. Explicit card
confirmation remains the final security boundary.

## Origin policies

### Terminal Agent

- One to three ordered choices.
- `send_input` targets the active pane captured for the prompt.
- Panel actions use that same captured pane as parent.
- Delegate actions resolve only to the configured, policy-allowed delegate.
- Existing target availability and coordinator-self-target checks remain.

### Autofix

- Exactly one choice with exactly one `send_input` action.
- The target is always the failing pane from `AutofixContext`; the payload
  cannot override it.
- No open, split, or delegate actions.
- The Autofix generation must still match when the proposal arrives.
- Ambiguous, destructive, multi-step, or explanatory outcomes remain normal
  Markdown.

The validator cannot prove that arbitrary shell text is non-destructive. The
prompt narrows eligible fixes, the card displays the command, and the user
confirmation controls execution.

## Permission and single-confirmation requirement

Calling the proposal CLI is non-mutating, but some agents may request permission
before directly executing the command. The human-readable tool-call title is
agent-authored and cannot be used for safe auto-approval.

Therefore CLI proposal mode is enabled per built-in agent only after live
verification proves one of:

1. the agent does not request permission for this exact direct WTA command;
2. the agent exposes trustworthy structured command/argv data that can be
   matched to the WTA proposal subcommand; or
3. the agent has an official command allowlist that can permit only this
   proposal subcommand.

If none applies, that agent stays on assistant-text JSON fallback. Shipping a
permission confirmation followed by a card confirmation is not acceptable.

## CLI-to-master transport

The proposal CLI follows the existing `wta sessions list` connection shape:

1. resolve and open the master named pipe;
2. initialize as a short-lived ACP client named `wta-proposal`;
3. send `_intellterm.wta/terminal_actions/propose` with the route token and
   versioned payload;
4. wait for a bounded structured response and disconnect.

Master recognizes `wta-proposal` during initialize and does not bind/spawn an
agent or register the connection as a helper live-set subscriber.

After validating the token, master sends the target helper an ExtNotification
containing `{proposal_id, session_id, payload}`. The helper immediately returns
`_intellterm.wta/terminal_actions/result` with the proposal id and disposition.
The result acknowledges that the card was presented or rejected; it never waits
for user confirmation, which would deadlock the in-flight agent tool and prompt.

Master keeps a bounded pending-response map keyed by proposal id and tagged with
the target HelperId. Helper disconnect, timeout, or master shutdown resolves
pending CLI requests as unavailable. Late or duplicate results are ignored.

This reuses the existing ACP pipe and WTA extension namespace. It does not use
COM, MCP, or a second local server.

## Compatibility and rollout

### Phase 1: contract only

- Add versioned wire types, limits, validators, and conversion tests.
- Add the CLI parser and stable disposition schema behind a feature gate.
- Keep assistant-text JSON as the only live card source.

### Phase 2: direct CLI routing

- Inject master-owned turn route tokens while forwarding prompts.
- Reuse the existing CLI-to-master ACP connection and add proposal/result
  extension methods.
- Add the bounded token and pending-response registries.
- Add AppEvent plus immediate disposition plumbing.
- Surface Terminal Agent cards through the existing `TurnState`.
- Keep the JSON fallback enabled.

### Phase 3: agent compatibility

- Verify direct process execution, payload delivery, and permission behavior for
  every built-in agent.
- Enable CLI proposals only for agents that preserve single confirmation.
- Suppress the internal proposal command's ToolCall row only when the agent
  exposes a trustworthy structured identity for that invocation.
- Keep the JSON fallback for WSL-hosted agents unless their WTA command can
  reach the Windows master pipe.

### Phase 4: Autofix

- Reuse the same wire schema and transport with Autofix policy.
- Bind the action to the recorded failing pane and generation.
- Verify stashed, split-pane, tab-switch, and stale-response behavior.

### Phase 5: fallback decision

- Measure CLI proposal success and fallback use by canonical agent id.
- Remove assistant-text card parsing only after all supported agents reach
  parity. Markdown explanations remain unchanged.

## Validation

Focused automated coverage must include:

- wire round-trip and schema-version rejection;
- unknown-field, payload-size, choice-count, and action-count limits;
- origin-specific policy rejection;
- master pipe environment/discovery fallback and stale-master rejection;
- stdin and payload-file handling across supported agent command tools;
- capability one-use, TTL, per-turn invalidation, and bounded-map behavior;
- short-lived proposal clients do not spawn agents or register as helpers;
- proposal/result correlation, timeout, helper disconnect, and late-result handling;
- current prompt, generation, target, duplicate, and stale checks;
- no model-authored pane/session/helper identifiers;
- stdout/stderr separation and disposition exit behavior;
- immediate proposal acknowledgement does not deadlock the in-flight turn;
- one visible card and one execution after confirmation;
- no terminal mutation before confirmation;
- JSON fallback for agents without CLI proposal support;
- multi-tab, multi-window, stashed-pane, and session-load routing;
- per-agent live permission behavior.

## Related work

- PR #428: superseded MCP-based implementation.
- Issue #445: delegate new-tab creation latency after card confirmation. The
  execution problem remains valid but is independent of proposal transport.
