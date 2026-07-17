# Planner MCP Terminal Action Proposal

Author: GitHub Copilot with vanzue
Date: 2026-07-16
Branch: `dev/vanzue/autofix-mcp-proposal`
Status: Implemented

## Summary

Terminal Agent currently asks the model to write a `RecommendationSet` JSON
object inside assistant text. The helper scans the streamed text for that
object, parses it, and turns it into Run, Insert, Open, or Delegate cards.
Malformed JSON, prose around the object, partial streaming, and model-authored
routing identifiers make that boundary unreliable.

This change replaces planner-authored recommendation JSON with one typed MCP
tool:

```text
propose_terminal_actions
```

The tool creates proposals only. It never mutates Windows Terminal. The owning
helper validates the current turn, injects trusted routing, builds the existing
card model locally, and waits for explicit user confirmation.

## Ownership

| Component | Responsibility |
|---|---|
| Agent | Choose Markdown or propose one to three typed user actions |
| MCP server in `wta-master` | Validate typed arguments and route them to the owning ACP session |
| `wta-helper` | Validate turn state, inject trusted target/delegate data, and render cards |
| User | Confirm Run, Insert, Open, or Delegate |
| Windows Terminal | Perform the confirmed operation |

The agent never supplies a pane id, tab id, helper id, ACP session id, or agent
CLI id.

## Tool schema

The request contains one to three ordered choices. The helper assigns their
1-based choice numbers.

```json
{
  "recommended_choice": 1,
  "choices": [
    {
      "title": "Run tests",
      "rationale": "Use the active shell and working directory.",
      "action": {
        "type": "send_input",
        "input": "cargo test"
      }
    }
  ]
}
```

Each choice has exactly one action:

1. `send_input`
   - Required: `input`
   - Optional: `preferred_action` (`execute` or `insert`)
   - The helper injects the active working pane captured for this prompt.
   - The card offers Run and Insert.
2. `open`
   - Required: `target` (`tab` or `panel`)
   - Optional: `cwd`, `title`, `direction`, `profile`
   - A panel's parent is injected from the captured active working pane.
3. `open_and_send`
   - Required: `target`, `destination` (`shell` or `delegate`), `input`
   - Optional: `cwd`, `title`, `direction`, `profile`
   - A panel's parent is injected from the captured active working pane.
   - A delegate uses the configured delegate agent; the model does not name it.

Unknown fields, NUL characters, empty required text, over-limit text, an
out-of-range recommended choice, and a direction on a tab action are rejected.

## Trusted planner target

The ACP client resolves the active working pane once while assembling the
planner prompt. The same resolved value drives two outputs:

1. Model-facing context contains `active_pane_available`, shell, cwd, title, and
   buffer, but no pane identifier.
2. A session-and-prompt-id-keyed helper event stores the pane identifier in the
   in-flight `SubmittedPrompt`, so tab rekeys cannot misroute it.

An agent pane is never a valid working target. If no target is available,
`send_input` and panel actions are rejected, while tab actions remain valid.

## Turn lifecycle

```text
user prompt
  -> Submitted with planner context pending
  -> planner target resolution stored
  -> one of:
       MCP proposal -> local RecommendationSet -> user confirmation
       Markdown     -> normal chat turn
       empty/error  -> existing empty/error handling
```

The first accepted tool proposal surfaces the card immediately as the turn's
single outcome. Duplicate, stale, or invalid-context calls are rejected.
Assistant text is never inspected for action JSON. If an agent emits legacy JSON
as text, it is displayed as ordinary Markdown and cannot create a card.

The same MCP tool also serves Autofix turns. The helper identifies the trusted
turn type and restricts Autofix to exactly one `send_input` choice with a
`preferred_action`; open, split, delegate, and multi-choice proposals are
rejected there.

## Prompt contract

- Chat and self-execute modes return Markdown.
- Shell recommendation and delegation modes call
  `propose_terminal_actions` exactly once.
- A tool call has no framing or trailing assistant prose.
- If the tool is unavailable or rejects the proposal, explain in Markdown.
- Never emulate the tool with JSON, a fenced block, XML, or another text format.
- `send_input` and panel actions require `active_pane_available: true`.
- Tab actions are allowed without an active working pane.

## Existing cases

| Existing recommendation | Typed proposal |
|---|---|
| Run or insert in active pane | `send_input`; helper injects pane |
| Open empty tab | `open(target=tab)` |
| Split empty panel | `open(target=panel)`; helper injects parent |
| Open shell and execute | `open_and_send(destination=shell)` |
| Delegate to configured agent | `open_and_send(destination=delegate)` |
| Multiple ranked alternatives | One tool call with 1-3 ordered choices |
| Chat or explanation | Markdown |
| Agent lacks HTTP MCP | Markdown-only degradation |
| Legacy recommendation JSON | Visible text, never executable |

## Completion criteria

- No planner code parses model-authored JSON.
- No model-controlled identifier selects an existing pane or delegate agent.
- Concurrent ACP sessions cannot receive each other's proposal.
- Duplicate, stale, malformed, or wrong-turn calls create no card or side effect.
- Accepted proposals preserve existing Run, Insert, Open, and Delegate behavior.
- Every non-tool response remains visible as Markdown.
- The full WTA test suite passes.
