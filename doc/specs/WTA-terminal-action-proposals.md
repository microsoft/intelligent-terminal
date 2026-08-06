# WTA terminal-action proposals

## Status

Implemented session-level stdio MCP design. The direct WTA CLI remains as a
fallback transport.

## Summary

Each host ACP session receives its own lightweight proposal MCP server:

```text
ACP session
  -> stdio MCP: intelligent_terminal/request_terminal_actions
  -> wta proposal-mcp-server
  -> owning Helper's authenticated named pipe
  -> existing recommendation card
  -> user confirmation
  -> existing wtcli/COM executor
```

The tool can only present typed actions for review. It cannot read or mutate
Windows Terminal. The existing card confirmation is the sole mutation
boundary.

The MCP call returns as soon as the Helper commits the card. User confirmation
or cancellation happens independently, so an agent turn never waits on a
human-held tool call.

## Ownership and routing

MCP is session-level, not agent-level. `wta-master` still owns one shared Agent
CLI process, while every ACP `session/new` or `session/load` request includes a
distinct `McpServer::Stdio` entry.

Before creating or loading a session, the Helper:

1. generates an opaque capability;
2. places the Helper pipe and capability in structured MCP process arguments;
3. sends the MCP server configuration in the ACP request;
4. binds the capability to the returned ACP SessionId on success; or
5. discards the pending capability on failure.

A failed replacement leaves the prior session capability valid. A successful
replacement retires it.

The model never receives or supplies a Helper, session, prompt, tab, window,
pane, channel, capability, origin, schema version, or choice ID. The Helper
injects these trusted values.

## MCP contract

Server name:

```text
intelligent_terminal
```

Tool:

```text
request_terminal_actions
```

The server supports MCP `initialize`, `ping`, `tools/list`, and `tools/call`
over newline-delimited JSON-RPC on stdio. It exposes no terminal read or
execution tools.

Input:

```json
{
  "recommended_choice": 1,
  "choices": [
    {
      "title": "Run tests",
      "rationale": "Verify the current change.",
      "actions": [
        {
          "type": "send",
          "input": "cargo test"
        }
      ]
    }
  ]
}
```

There are one to three choices and one to three actions per choice. Supported
actions are:

- `send`: submit input to the trusted active pane;
- `open`: open an empty tab or panel;
- `open_and_send`: open a tab or panel and submit input there.

Open actions may include `cwd`, `title`, `profile`, and panel `direction`.
`open_and_send` may set `delegate: true`; the Helper substitutes the configured
delegate agent. A model cannot name an arbitrary agent.

Autofix uses the same tool but the Helper supplies the trusted Autofix origin
and requires exactly one choice with exactly one `send` action.

Tool result statuses:

| Status | Meaning |
|---|---|
| `accepted` | Intelligent Terminal accepted the requested actions. |
| `duplicate` | This turn already consumed its proposal attempt. |
| `stale` | The session no longer owns the active turn. |
| `rejected` | Schema or policy validation failed. |
| `unavailable` | The owning Helper or proposal transport was unavailable. |

`accepted` means the handoff completed. The agent ends its turn after
this result.

## Helper validation

The Helper's `ProposalChannelManager` owns:

- one stable per-Helper proposal pipe;
- one committed MCP session capability;
- pending capabilities for in-flight session creation or loading;
- one active turn binding;
- bounded CLI channel tombstones.

An MCP request is accepted only when:

1. its capability is the committed capability;
2. that capability is bound to the current ACP SessionId;
3. the same SessionId owns the active proposal-enabled turn;
4. the turn is still in `Issued`;
5. strict schema, size, count, origin, target, and delegate policy passes.

The active binding contains the trusted prompt ID, active pane target, and
Autofix bit. These values never come from model input.

An accepted MCP proposal transitions synchronously:

```text
Issued -> Validating -> AwaitingUser
```

The pipe responds immediately after `Commit` is queued. Confirmation later
claims the proposal and drives the existing card execution path.

## Permission and tool-call UI

Permission remains an optional compatibility preflight. Some agents call MCP
without requesting permission.

When an adapter requests permission for the exact
`intelligent_terminal/request_terminal_actions` tool, the Helper:

1. verifies the trusted ACP SessionId owns the current issued turn;
2. silently selects `AllowOnce`; and
3. does not consume or arm proposal state.

Unrelated MCP and shell permissions continue through the normal permission UI.
The proposal MCP tool-call row is hidden because the recommendation card is the
user-facing representation.

## Named-pipe boundary

The MCP sidecar connects only to the pipe named in its process arguments and
sends:

```json
{
  "version": 1,
  "capability": "<opaque capability>",
  "payload": "{...}"
}
```

The pipe uses the existing current-user ACL and rejects remote clients.
Capabilities are stored hashed by the Helper. They are process credentials,
not model-visible arguments.

## CLI fallback

The retained command is:

```text
wta propose-terminal-actions --channel <channel> --payload-json <compact-json>
```

It uses the same strict schema conversion, Helper validation, card, and
execution pipeline. Unlike MCP, the CLI connection returns both the immediate
validation response and the final user decision. The canonical CLI permission
preflight remains optional and unchanged.

Normal host sessions use MCP and do not receive the per-turn PowerShell command
in their prompt. The CLI remains available for compatibility, diagnostics, and
future agents that work better through shell commands.

## Scope

The MCP server is attached only for host agents. WSL-side MCP process launching
is not implemented. Assistant text remains ordinary chat content and is never
parsed into terminal actions.
