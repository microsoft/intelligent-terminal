# Autofix MCP Terminal Action Proposal

Author: GitHub Copilot with vanzue
Date: 2026-07-16
Branch: `dev/vanzue/autofix-mcp-proposal`
Status: Implemented

## Summary

Autofix currently asks the model to return a JSON object inside assistant text,
extracts that object from a streamed Markdown response, and converts it into a
recommendation card. This is not a protocol boundary: malformed, partial, or
prose-wrapped model output can silently discard a useful answer.

This change replaces model-authored Autofix JSON with two explicit outcomes:

1. A deterministic one-command fix is submitted through the shared typed MCP
   tool `propose_terminal_actions`.
2. Every non-deterministic, unsafe, ambiguous, or explanatory result is returned
   as normal Markdown.

The MCP tool creates a proposal only. Autofix restricts it to exactly one
`send_input` choice; it never inserts or executes terminal
input by itself. The owning helper renders `Run` and `Insert` actions, and the
user chooses the side effect.

## Ownership

| Component | Responsibility |
|---|---|
| Agent | Decide whether a deterministic one-command fix exists; provide the command and explanation |
| MCP server in `wta-master` | Validate typed arguments and route the proposal to the ACP session's helper |
| `wta-master` | Bind a private MCP route to the existing helper/session route |
| `wta-helper` | Validate the current Autofix turn, retain the trusted target pane, render the proposal, and process user choice |
| Windows Terminal | Insert text into the trusted shell pane, with or without Enter |

The agent never supplies a tab id, pane id, helper id, ACP session id, or
Autofix generation.

## Session-bound MCP routing

The MCP server is hosted once by `wta-master`. A shared `/mcp` endpoint is not
enough for mutating UI tools because an MCP HTTP `tools/call` request does not
carry the ACP session id that selected the server.

When master forwards `session/new` or `session/load`, it:

1. Generates an opaque route id.
2. Registers the route against the helper's existing `AgentLink`.
3. Adds `http://127.0.0.1:<port>/mcp/<route-id>` to that ACP session's MCP
   server list when the selected agent advertises HTTP MCP support.
4. Associates the route with the returned ACP session. A disconnected helper
   deactivates the route; same-master orphan resume rebinds it so the agent's
   existing session URL remains valid.

The route id is transport correlation only. It does not duplicate the session
registry or Autofix context. `session_to_helper` and the helper's
`TurnState::SubmittedPrompt.autofix` remain the sources of truth.

## MCP tool

Name:

```text
propose_terminal_actions
```

Arguments:

```json
{
  "recommended_choice": 1,
  "choices": [{
    "title": "Retry dotnet test",
    "rationale": "The previous command contained a typo.",
    "action": {
      "type": "send_input",
      "input": "dotnet test",
      "preferred_action": "execute"
    }
  }]
}
```

Schema and server validation:

- Autofix requires exactly one choice whose action is `send_input`.
- `input` is required, non-empty, and length-bounded.
- `preferred_action` is required for Autofix and is either `insert` or `execute`.
- `title` is required; `rationale` is optional and length-bounded.
- Unknown fields are rejected.
- NUL characters are rejected.
- No target or routing identifiers are accepted.

`preferred_action` selects the initially focused button. It does not authorize
or perform execution.

The tool returns as soon as the owning helper accepts or rejects the proposal;
it does not wait for the user's choice.

## Master-to-helper bridge

Master delivers the proposal through an ACP extension request on the existing
helper pipe. A request/response is used instead of a notification so the MCP
tool can return a precise accepted/rejected result.

The helper accepts a proposal only when:

- the routed ACP session belongs to the current helper;
- the current turn is an in-flight Autofix turn;
- the Autofix generation still matches;
- the trusted target pane has been resolved;
- the turn has not already accepted a proposal.

The helper derives the target pane from `AutofixContext.target_pane_id`. The
proposal request contains no pane id.

## Turn lifecycle

```text
OSC 133 failure
  -> Detected (when auto-suggest is disabled) or Pending
  -> standard ACP session/prompt
  -> one of:
       MCP proposal -> Proposal card -> Run / Insert / Esc
       Markdown     -> Explanation chat turn
       empty/error  -> explicit failure and cleared pending state
```

Stale protection continues to use the existing per-tab Autofix generation.
Escape, a newer failure, tab close, or session close invalidates an older
proposal.

## User actions

- `Run`: insert the proposed text into the trusted failing shell pane and send
  Enter.
- `Insert`: insert the proposed text without Enter.
- `Esc`: dismiss the proposal without touching the shell pane.

The proposal crosses MCP and ACP as dedicated typed data. The helper constructs a
single-choice `RecommendationSet` locally only after validation, reusing the
existing card renderer and execution path. It is never parsed from model text.

## Prompt contract

The Autofix prompt no longer requests JSON.

- Call `propose_terminal_actions` exactly once with one `send_input` choice only
  when one non-destructive,
  deterministic command resolves the failure.
- Return Markdown when installation, authentication, elevation, destructive
  behavior, multiple steps, multiple plausible corrections, or user intent is
  involved.
- If the tool is unavailable, return Markdown. Do not emulate the tool with a
  JSON or fenced-code response.

Agents that do not advertise HTTP MCP support receive the same Markdown-oriented
prompt without the tool. They degrade to explanation-only behavior; WTA does not
restore the legacy JSON parser.

## Existing Autofix cases

| Case | New behavior |
|---|---|
| Certain one-command correction | MCP proposal |
| Unique grounded near-match | Resolve locally, then MCP proposal |
| Multiple near-matches | Markdown explanation |
| Missing tool/package | Markdown explanation |
| Authentication or credentials | Markdown explanation |
| Elevation required | Markdown explanation |
| Destructive operation | Markdown explanation |
| Multi-step repair or refactor | Markdown explanation or normal agent workflow |
| Output is not an error | Markdown explanation, no proposal |
| Manual `/fix` | Resolve the active working pane, then use the same proposal path |
| Agent lacks HTTP MCP | Markdown-only degradation |
| Agent writes a command but does not call the tool | Render as Markdown; never infer an action |
| Invalid tool arguments | Typed MCP error; no proposal |
| Duplicate or stale tool call | Typed rejection; no proposal |

## Implementation plan

This is delivered as one complete change:

1. Make the master-owned MCP server session-routable and inject routes from
   master for every `session/new` and `session/load`.
2. Add the session-bound `propose_terminal_actions` tool and synchronous typed
   master-to-helper extension request shared by Autofix and normal agent turns.
3. Convert an accepted typed proposal into a helper-owned one-choice card and
   reuse Run/Insert/Esc handling with the trusted Autofix context.
4. Change `auto-fix.md` to tool-or-Markdown behavior.
5. Remove Autofix JSON parsing, eager JSON surfacing, and JSON-specific stream
   extraction from the Autofix path.
6. Update architecture documentation and add routing, validation, stale-turn,
   UI action, prompt, and degradation tests.

## Completion criteria

- Two concurrent tabs cannot receive each other's proposal.
- No model-controlled identifier selects the target pane.
- Malformed, duplicate, stale, or non-Autofix tool calls produce no side effect.
- `Run` and `Insert` differ only by the trailing Enter.
- Every non-tool model response remains visible as Markdown.
- No Autofix code parses model-authored JSON.
- The full WTA test suite passes.
