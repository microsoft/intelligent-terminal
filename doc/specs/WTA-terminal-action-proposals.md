# WTA terminal-action proposals

## Status

Implemented session-level Streamable HTTP MCP design. The direct WTA CLI
remains as a fallback transport.

## Summary

`wta-master` owns one Windows-loopback session MCP endpoint. Host ACP
sessions use it directly. Each WSL distro gets an on-demand loopback relay,
and every eligible ACP session receives a distinct bearer capability:

```text
ACP session
  -> HTTP MCP: intellterm_<public-id>/{run_command_in_current_shell,create_workspace,delegate_task_in_new_workspace,request_user_input}
  -> wta-master capability -> ACP SessionId
  -> session_to_helper -> existing master/helper ACP pipe
  -> owning Helper
       -> action tool -> recommendation card -> wtcli/COM executor
       -> request_user_input -> blocking choice/freeform modal -> structured answer
```

The endpoint presents typed terminal actions for review and blocking
clarification questions. It cannot read or mutate Windows Terminal. The
existing card confirmation is the sole mutation boundary.

The three action tools return as soon as the Helper commits its card;
confirmation or cancellation then happens independently. `request_user_input`
deliberately keeps the MCP call open until the user answers, cancels, the
caller disconnects, or the ten-minute timeout expires.

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
distro, master does not advertise session MCP to that Helper. The relay is
cached for that master's lifetime, but its stdin remains connected to an
ownership pipe: normal child cleanup or unexpected master termination closes
the pipe and makes the Python relay exit. Each distro has an independent
startup lock, so a slow distro does not block endpoint selection for another.
Master also supervises the relay and restarts it on the same loopback port
after an unexpected relay failure, preserving URLs already stored by ACP
sessions.

Before creating or loading a session:

1. the Helper marks the private ACP request as session-MCP eligible using the
   legacy `_meta.wta.proposal_mcp` compatibility field;
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
intellterm_<public-id>
```

Tools (four total):

```text
run_command_in_current_shell
create_workspace
delegate_task_in_new_workspace
request_user_input
```

The first three are terminal-action proposal tools. `request_user_input` is a
separate blocking clarification tool and does not create a terminal-action
proposal.

The server supports MCP `initialize`, `ping`, `tools/list`, and `tools/call`
over stateless Streamable HTTP JSON-RPC. POST responses use JSON or HTTP 202
for notifications; GET and DELETE return 405 because server-initiated streams
are unnecessary. It exposes no terminal read or execution tools.

Input, for `run_command_in_current_shell`:

```json
{
  "summary": "Run tests",
  "reason": "Verify the current change.",
  "command": "cargo test"
}
```

Each terminal-action MCP call proposes exactly one user-visible terminal
outcome. The tools separate current-shell execution, workspace creation, and
delegation rather than exposing a mechanical action discriminator. Each
schema advertises exactly the fields that intent accepts, and
`additionalProperties: false` rejects fields belonging to another intent. The
action tools are:

- `run_command_in_current_shell`: propose one shell command for the trusted
  active pane;
- `create_workspace`: create a new terminal workspace, optionally initialized
  with one shell command;
- `delegate_task_in_new_workspace`: start the configured delegate agent in a
  new workspace with a self-contained task.

The exact public payloads are:

| Tool | Required | Optional |
|---|---|---|
| `run_command_in_current_shell` | `summary`, `command` | `reason` |
| `create_workspace` | `summary`, `placement` | `reason`, `command`, `working_directory`, `split_direction`, `profile` |
| `delegate_task_in_new_workspace` | `summary`, `task`, `placement` | `reason`, `working_directory`, `split_direction` |

`placement` is `new_tab` or `new_split`. `split_direction` is `right`, `left`,
`up`, `down`, or `auto`. `create_workspace.command` is optional: omitting it
creates an empty workspace, while supplying it runs that command after
creation. `delegate_task_in_new_workspace` deliberately exposes neither a
profile nor an agent selector. The Helper always substitutes the configured
delegate agent and rejects the request when no valid delegate is configured. A
model cannot name an arbitrary agent.

The Helper maps these public intents into the existing internal action model:

| Public tool | Internal action |
|---|---|
| `run_command_in_current_shell` | `RecommendedAction::Send` |
| `create_workspace` without `command` | `RecommendedAction::Open` |
| `create_workspace` with `command` | `RecommendedAction::OpenAndSend` with delegation disabled |
| `delegate_task_in_new_workspace` | `RecommendedAction::OpenAndSend` with delegation enabled and no profile |

`new_tab` maps to the existing `Tab` target and `new_split` maps to `Panel`.
The user-facing `summary` becomes the recommendation title and, for workspace
tools, the requested destination title. `working_directory`,
`split_direction`, and `profile` map to the existing internal fields without
changing trusted active-pane routing.

Autofix uses only `run_command_in_current_shell`; the Helper supplies the
trusted Autofix origin and requires the resulting internal `Send` action.

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

`request_user_input` accepts one question, up to eight single-line choices,
and an optional freeform answer. Master routes the request through the same
session capability and `session_to_helper` ownership lookup, then waits for
the owning Helper's modal response. Enter submits the selected choice or a
non-empty freeform answer; Esc returns `cancelled`. The request times out
after ten minutes. Each session may have only one outstanding user-input
request. A request ID removes the exact modal when its HTTP caller disconnects,
the Helper connection drops, the turn is cancelled, or master times out. It
does not inspect, intercept, or translate an Agent's provider-specific
`ask_user` implementation.

## Helper validation

Master owns:

- one Windows-loopback HTTP listener and the per-distro WSL relay lifecycle;
- hashed pending and committed session capabilities;
- the `SessionId -> current HelperRoute` map.

The Helper's `ProposalChannelManager` owns:

- one active turn binding;
- bounded CLI channel tombstones.

An MCP terminal-action request is accepted only when:

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

When an adapter requests permission for one of the three terminal-action tools
advertised by an `intellterm_<public-id>` server, the Helper:

1. verifies the trusted ACP SessionId owns the current issued turn;
2. silently selects `AllowOnce`; and
3. does not consume or arm proposal state.

Unrelated MCP and shell permissions continue through the normal permission UI.
Permission preflight requests for `request_user_input` are also resolved with
`AllowOnce`: the blocking modal itself is the user-facing decision point.
Session MCP tool-call rows are hidden because the recommendation card or
user-input modal is the user-facing representation.

## HTTP and ACP boundaries

The HTTP server binds only to an ephemeral IPv4 Windows-loopback port. WSL
relays bind only to ephemeral loopback ports inside their distro and rewrite
loopback Host/Origin authorities to the upstream loopback authority before
byte-forwarding. A relay limits concurrent handlers, applies a request read
timeout and the same header/body size ceilings, and returns explicit HTTP
errors when request parsing or the Windows bridge fails. Long-held user-input
requests use a separate connection pool from short MCP requests, and duplicate
user-input requests for one ACP session are rejected. The master server
requires the session bearer capability,
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

The MCP server is attached only for agents that advertise ACP HTTP MCP
support. Assistant text remains ordinary chat content and is never parsed into
terminal actions.
