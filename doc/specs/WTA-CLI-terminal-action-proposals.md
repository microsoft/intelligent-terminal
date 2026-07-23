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
wta propose-terminal-actions --payload-base64 <base64url-json>
```

The command is proposal-only. It cannot send input, create panes, launch
delegates, or otherwise mutate Windows Terminal. A valid proposal becomes a
local recommendation card; the existing card confirmation remains the only
path to terminal mutation.

## Goals

- Use WTA CLI, not MCP, for typed terminal-action proposals.
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
- Treating model-authored shell text as trusted or intrinsically
  non-destructive.
- Supporting arbitrary shell-wrapped proposal invocations in v1.
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
1. WTA submits an Autofix or Terminal Agent prompt over ACP.
2. The agent requests create_terminal for direct argv:
     command = "wta"
     args    = ["propose-terminal-actions", "--payload-base64", "..."]
3. wta-master routes create_terminal by ACP session id to the owning helper.
4. The helper recognizes the reserved direct WTA subcommand.
5. The helper mints a short-lived, one-use capability bound to the active
   session and prompt, injects its private pipe name and capability into the
   child environment, and launches the co-located wta.exe locally.
6. The CLI decodes the versioned payload and submits it over the helper-local
   pipe.
7. The helper validates the wire schema. App performs the authoritative
   active-turn, generation, origin, target, and duplicate checks.
8. App converts the accepted wire proposal into the existing
   RecommendationSet and displays the existing confirmation card.
9. The CLI returns a structured "presented" disposition. This means the card
   was shown; it does not mean an action ran.
10. Only a later user card confirmation sends ChoiceExecution to the existing
    executor and reaches wtcli/COM.
```

The master does not host the proposal endpoint. Its only role in this flow is
the existing ACP `session_id -> helper` routing for `create_terminal`.

## CLI contract

### Invocation

v1 accepts only a direct structured ACP terminal request:

```text
command: wta or wta.exe
args:
  - propose-terminal-actions
  - --payload-base64
  - <base64url-encoded UTF-8 JSON>
```

The helper rewrites the executable to its own trusted, co-located `wta.exe`
before spawning. The payload has a decoded size limit.

v1 deliberately does not support:

- `pwsh -Command "wta propose-terminal-actions ..."`
- `cmd /c wta propose-terminal-actions ...`
- `bash -lc "wta propose-terminal-actions ..."`
- stdin or shell pipelines
- a model-provided proposal pipe, token, session id, or target id

These forms either lose the structured argv boundary or bypass the current
`ShellManager` direct-WTA local execution rule.

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
| `unavailable` | The helper proposal channel or required target context is unavailable. |

Protocol-complete dispositions exit with code 0 so agents do not retry rejected
or stale proposals as transport failures. Nonzero exit codes are reserved for
invalid CLI syntax, undecodable payloads, broken local transport, or internal
failures.

stderr is diagnostic-only. The implementation must stop merging proposal
stdout and stderr before the agent consumes the result.

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

The helper removes reserved proposal environment variables
case-insensitively, then injects:

- a cryptographically random helper-local pipe name;
- a cryptographically random, one-use capability;
- no reusable session, tab, window, or pane credential.

Each capability is stored in a bounded map with a short TTL and is bound to:

- ACP session id from `create_terminal`;
- the helper's active prompt id;
- proposal origin;
- the owning helper.

The capability is consumed on first submission. Unused entries expire.

App remains authoritative for state that the ACP client does not own:

- current `TurnState`;
- Autofix generation;
- failing pane recorded by `AutofixContext`;
- active pane captured for a Terminal Agent prompt;
- whether a recommendation already surfaced;
- configured delegate availability.

The shared agent process can still send a `create_terminal` request containing
another live ACP session id. The capability does not turn the shared agent into
a security boundary. Freshness checks prevent unsolicited or stale cards, pane
targets are injected locally, and explicit card confirmation remains the final
security boundary.

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

Calling the proposal CLI is non-mutating, but some agents may issue an ACP
`request_permission` before `create_terminal`. The current permission request's
human-readable title is agent-authored and cannot be used for safe
auto-approval.

Therefore CLI proposal mode is enabled per built-in agent only after live
verification proves one of:

1. the agent does not request permission for this exact direct WTA command;
2. the permission request exposes trustworthy structured command/argv data
   that WTA can match to its co-located executable and reserved subcommand; or
3. the agent has an official command allowlist that can permit only this
   proposal subcommand.

If none applies, that agent stays on assistant-text JSON fallback. Shipping a
permission confirmation followed by a card confirmation is not acceptable.

## Local transport

The proposal channel is a per-helper local named pipe, not COM and not the
helper-master ACP pipe.

Required hardening:

- random, unguessable pipe name;
- first-instance creation;
- DACL restricted to the current user;
- one-use capability required before payload processing;
- bounded payload, capability map, and TTL;
- one response per connection;
- no terminal-operation methods on the pipe.

Using the helper-master pipe would require the short-lived CLI to impersonate a
helper or extend the ACP multiplexer with a non-ACP protocol. COM events would
unnecessarily expose proposal routing to the WT process and weaken
session/helper ownership.

## Compatibility and rollout

### Phase 1: contract only

- Add versioned wire types, limits, validators, and conversion tests.
- Add the CLI parser and stable disposition schema behind a feature gate.
- Keep assistant-text JSON as the only live card source.

### Phase 2: helper-local transport

- Add the hardened per-helper proposal pipe and capability registry.
- Recognize only direct WTA proposal invocations.
- Add AppEvent plus oneshot response plumbing.
- Surface Terminal Agent cards through the existing `TurnState`.
- Keep the JSON fallback enabled.

### Phase 3: agent compatibility

- Verify direct invocation and permission behavior for every built-in agent.
- Enable CLI proposals only for agents that preserve single confirmation.
- Suppress the internal proposal command's ToolCall row only after it is
  positively identified from the helper-owned invocation.

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
- direct invocation matching and shell-wrapper rejection;
- trusted executable rewriting;
- case-insensitive reserved environment stripping;
- capability one-use, TTL, bounded-map, and wrong-pipe rejection;
- current prompt, generation, target, duplicate, and stale checks;
- no model-authored pane/session/helper identifiers;
- stdout/stderr separation and disposition exit behavior;
- one visible card and one execution after confirmation;
- no terminal mutation before confirmation;
- JSON fallback for agents without CLI proposal support;
- multi-tab, multi-window, stashed-pane, and session-load routing;
- per-agent live permission behavior.

## Related work

- PR #428: superseded MCP-based implementation.
- Issue #445: delegate new-tab creation latency after card confirmation. The
  execution problem remains valid but is independent of proposal transport.
