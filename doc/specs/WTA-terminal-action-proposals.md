# WTA terminal-action proposals

## Status

Implemented session-level Streamable HTTP MCP design. The direct WTA CLI
remains as a fallback transport.

## Summary

`wta-master` owns one Windows-loopback proposal MCP endpoint. Host ACP
sessions use it directly. Each WSL distro gets an on-demand loopback relay,
and every eligible ACP session receives a distinct bearer capability:

```text
ACP session
  -> HTTP MCP: intellterm_<public-id>/request_terminal_actions
  -> wta-master capability -> ACP SessionId
  -> session_to_helper -> existing master/helper ACP pipe
  -> owning Helper
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

MCP is session-level, not agent-level. `wta-master` may own multiple Agent CLI
processes, keyed by Agent identity, command, and Host/WSL source, while every
ACP `session/new` or `session/load` request includes a distinct
`McpServer::Http` entry. Host entries point directly to master's ephemeral
Windows `127.0.0.1` endpoint. WSL entries point to an ephemeral
`127.0.0.1` relay inside that Agent's distro. Every entry has an independent,
non-sensitive server name and a different `Authorization` header. The public
name prevents Agent implementations that cache MCP configuration globally by
server name from overwriting another ACP session's header; it is not used for
authorization or routing.

The WSL relay exists because NAT-mode WSL cannot reliably reach an inbound
Windows listener without a Hyper-V firewall rule. Master starts one relay per
distro with `wsl.exe`; the relay invokes a fixed, encoded Windows PowerShell
byte forwarder for each HTTP request, which reaches master's Windows-loopback
endpoint. The bearer header remains in the forwarded bytes and never enters a
process command line. If Python 3 or Windows interop is unavailable in a
distro, master does not advertise proposal MCP to that Helper. The relay is
cached for that master's lifetime, but its stdin remains connected to an
ownership pipe: normal child cleanup or unexpected master termination closes
the pipe and makes the Python relay exit. Each distro has an independent
startup lock, so a slow distro does not block endpoint selection for another.
Master also supervises the relay and restarts it on the same loopback port
after an unexpected relay failure, preserving URLs already stored by ACP
sessions.

Before creating or loading a session:

1. the Helper marks the private ACP request as proposal-MCP eligible;
2. master generates an opaque capability and sends the HTTP MCP configuration
   to the Agent CLI;
3. master binds the capability to the returned ACP SessionId on success; or
4. master discards the pending capability on failure.

Helper disconnect preserves the committed capability while the same Agent CLI
still owns an orphaned ACP session, allowing direct rebind without
`session/load`. When that Agent CLI instance dies, master revokes every
capability owned by that exact instance. Reapers compare the pool cell identity
before removal, so a late reaper from an old process cannot revoke or remove a
replacement process using the same command.

A failed replacement leaves the prior session capability valid. A successful
replacement retires it. Orphan Helper rebinds preserve the existing capability
because the Agent CLI session and MCP client are still alive.

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
over stateless Streamable HTTP JSON-RPC. POST responses use JSON or HTTP 202
for notifications; GET and DELETE return 405 because server-initiated streams
are unnecessary. It exposes no terminal read or execution tools.

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

Master owns:

- one Windows-loopback HTTP listener and the per-distro WSL relay lifecycle;
- hashed pending and committed session capabilities;
- the `SessionId -> current HelperRoute` map.

The Helper's `ProposalChannelManager` owns:

- one active turn binding;
- bounded CLI channel tombstones.

An MCP request is accepted only when:

1. master recognizes its bearer capability;
2. that capability is committed to an ACP SessionId;
3. `session_to_helper` resolves that SessionId to the current Helper;
4. the same SessionId owns the Helper's active proposal-enabled turn;
5. the turn is still in `Issued`;
6. strict schema, size, count, origin, target, and delegate policy passes.

The active binding contains the trusted prompt ID, active pane target, and
Autofix bit. These values never come from model input.

An accepted MCP proposal transitions synchronously:

```text
Issued -> Validating -> AwaitingUser
```

The private master-to-Helper ACP extension responds immediately after `Commit`
is queued. Confirmation later claims the proposal and drives the existing card
execution path.

## Permission and tool-call UI

Permission remains an optional compatibility preflight. Some agents call MCP
without requesting permission.

When an adapter requests permission for either the current
`intellterm_<public-id>/request_terminal_actions` tool or the legacy
`intelligent_terminal/request_terminal_actions` name, the Helper:

1. verifies the trusted ACP SessionId owns the current issued turn;
2. silently selects `AllowOnce`; and
3. does not consume or arm proposal state.

Unrelated MCP and shell permissions continue through the normal permission UI.
The proposal MCP tool-call row is hidden because the recommendation card is the
user-facing representation.

## HTTP and ACP boundaries

The HTTP server binds only to an ephemeral IPv4 Windows-loopback port. WSL
relays bind only to ephemeral loopback ports inside their distro and rewrite
loopback Host/Origin authorities to the upstream loopback authority before
byte-forwarding. A relay limits concurrent handlers, applies a request read
timeout and the same header/body size ceilings, and returns explicit HTTP
errors when request parsing or the Windows bridge fails. The master server requires the session bearer capability,
validates Host and any Origin header, rejects duplicate or oversized headers,
rejects transfer encoding, and caps request bodies. Capabilities are stored
hashed and are never logged.

After authentication, master resolves capability to SessionId, then resolves
SessionId through the live `session_to_helper` map. It forwards the typed
arguments over the existing ACP named pipe with a private extension request.
There is no second MCP-specific master/helper channel. Capabilities are process
credentials in ACP session configuration, not model-visible arguments.

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

The MCP server is attached only for host agents that advertise ACP HTTP MCP
support. Assistant text remains ordinary chat content and is never parsed into
terminal actions.
