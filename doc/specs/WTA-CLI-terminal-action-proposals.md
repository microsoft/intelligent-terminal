# WTA CLI terminal-action proposals

## Status

Implemented direct-only design. Terminal-action proposals travel from a
short-lived WTA CLI to the owning Helper.

## Summary

Autofix and Terminal Agent use a short-lived WTA CLI to submit typed action
proposals directly to the Helper that owns the current turn:

```text
Agent CLI
  -> wta propose-terminal-actions
  -> per-Helper named pipe
  -> existing recommendation card
  -> user confirmation
  -> existing wtcli/COM executor
```

The proposal command cannot mutate Windows Terminal. It can only ask the
owning Helper to display a recommendation card. The existing card confirmation
remains the sole mutation boundary.

ACP Assistant text is always chat content. It is rendered verbatim and is never
parsed into terminal actions or recommendation cards.

wta-master remains responsible for the shared agent process, ACP multiplexing,
session-to-Helper routing, and forwarding permission requests to the Helper
that owns the ACP session. It does not mint proposal tokens, receive proposal
payloads, correlate proposal results, or acknowledge cards.

## Goals

- Use WTA CLI, not MCP, for typed terminal-action proposals.
- Have the agent session execute one canonical command directly.
- Route the short-lived CLI directly to the owning Helper.
- Keep session, Helper, tab, window, and pane identifiers out of proposal JSON.
- Reject wrong-Helper, stale-turn, already-consumed, and replayed requests.
- Give the agent immediate validation feedback and final user-decision feedback.
- Preserve the existing Run, Insert, Open, Split, and Delegate card UI.
- Preserve exactly one visible user confirmation before terminal mutation.

## Non-goals

- Reintroducing MCP or adding another shared server.
- Using wta-master as a proposal router.
- Letting the proposal CLI execute terminal actions.
- Parsing terminal actions from ACP Assistant text.
- Treating arbitrary model-authored shell commands as trusted.
- Proving that arbitrary proposed shell input is non-destructive.
- Reporting whether a confirmed shell command eventually succeeded.
- Persisting proposal channels across Helper process restarts.

## End-to-end flow

```text
1. Helper starts a turn and creates one opaque channel.
2. Helper injects the channel and canonical invocation contract into the prompt.
3. Agent emits the exact canonical WTA command with inline proposal JSON.
4. If the Agent CLI sends session/request_permission before executing it,
   master routes that request by trusted ACP session ownership to the Helper.
5. Helper parses an exact current-channel invocation and silently selects
   AllowOnce. Agents that execute without a permission callback use the same
   proposal path without this optional preflight.
6. Agent runs the short-lived WTA CLI.
7. The CLI derives the per-Helper pipe from the opaque channel and connects
   directly to it.
8. Helper verifies channel ownership and turn freshness, then atomically
   consumes the issued attempt.
9. Helper validates the typed proposal and stages the existing card.
10. CLI receives a validation response. If accepted, it remains connected.
11. User confirms or cancels the card, or lifecycle invalidation ends the turn.
12. CLI receives the final response and exits.
```

The permission request still crosses master because that is part of the ACP
topology. Proposal data and proposal state do not.

## Channel and endpoint

Each Helper process creates a random instance identifier and one stable named
pipe:

```text
channel: v1.<helper-instance-id>.<turn-nonce>
pipe:    \\.\pipe\IntelligentTerminal.Proposal.<helper-instance-id>
```

Both identifiers use lower-case UUID simple form: 32 hexadecimal characters
without braces or separators. The agent copies the complete channel as an
opaque string. It never constructs or receives separate Helper, session, tab,
window, pane, or prompt identifiers.

The Helper instance and pipe survive pane stash/restore and `/new`. A Helper
process restart creates a new instance and pipe, permanently invalidating all
old channels.

Only one channel is active per Helper. Starting a newer turn invalidates the
previous channel before the new one is issued.

## Canonical invocation

The only auto-approvable PowerShell form is:

```powershell
& "$env:WTA_CLI_PATH" propose-terminal-actions --channel <channel> --payload-json '<compact-json>'
```

For a packaged installation, WTA sets `WTA_CLI_PATH` to the package-family
specific App Execution Alias at
`%LOCALAPPDATA%\Microsoft\WindowsApps\<package-family>\wta.exe`. This lets an
external Agent launch WTA through the registered package boundary instead of
directly executing the protected package file. An unpackaged development build
uses its trusted current executable path instead. Proposal JSON must be compact
UTF-8 JSON encoded as one PowerShell single-quoted argument; literal
apostrophes are escaped by doubling them. The command has no pipeline,
redirection, here-string, command substitution, temporary file, extra
argument, or alternate executable spelling.

The Helper uses one renderer/parser implementation for prompt generation and
permission matching. It does not infer safety from the agent-authored tool
title.

The permission request must describe one execute operation with a `command`
field and a single-entry `commands` array containing the same canonical command.
Agent adapters that emit another permission envelope are silently cancelled,
but adapters that execute without a permission callback can submit directly.

Permission policy has three outcomes:

| Input | Outcome |
|---|---|
| Exact canonical command for the current channel and compact JSON payload | Silently `AllowOnce` without changing proposal state |
| Recognizable proposal command with unsafe or non-canonical syntax | Silently cancel |
| Any unrelated command | Use the existing Permission UI |

If current-channel validation fails for any reason, including a different or
stale channel, the Helper silently cancels the permission request. The
permission callback is an optional compatibility preflight, not a prerequisite
for proposal submission: some agents execute tool commands without emitting
`request_permission`. `AllowAlways` is never selected.

## CLI contract

```text
wta propose-terminal-actions --channel <channel> --payload-json <compact-json>
```

The command does not accept stdin, `--payload-file`, a master pipe, a Helper
pipe, or separate routing identifiers. Inline payload size is limited to
8 KiB UTF-8 before deserialization.

The CLI parses the channel, derives the pipe name, sends one request frame, and
reads newline-delimited compact JSON responses. stdout is protocol-only.
Diagnostics go to stderr.

Protocol-complete rejections exit successfully so the agent can interpret the
response and decide whether to retry. CLI syntax, malformed channel, broken
transport, and internal failures use a nonzero exit status.

## Pipe protocol

Protocol version 1 uses UTF-8 JSON Lines with a maximum encoded line size of
49 KiB, covering worst-case JSON escaping for the 8 KiB payload plus protocol
overhead. One connection carries exactly one request and its responses.

Request:

```json
{"version":1,"channel":"v1.<helper>.<turn>","payload":"{...}"}
```

Immediate validation success:

```json
{"phase":"validation","status":"accepted","proposal_id":"<uuid>","retryable":false}
```

Immediate validation failure:

```json
{"phase":"validation","status":"invalid_schema","reason":"...","retryable":true}
```

Final response after an accepted validation:

```json
{"phase":"final","status":"confirmed","proposal_id":"<uuid>"}
```

Validation statuses:

- `accepted`
- `unknown_channel`
- `helper_mismatch`
- `stale`
- `superseded`
- `already_consumed`
- `invalid_schema`
- `rejected`
- `unavailable`

Final statuses:

- `confirmed`
- `cancelled`
- `superseded`
- `timed_out`
- `unavailable`

`confirmed` means the selected card action was dispatched to the existing
executor. It does not claim that a target shell command finished successfully.

## Helper channel state

```text
Issued
  -> Validating
  -> AwaitingUser
  -> Confirmed | Cancelled | Superseded | TimedOut | Unavailable
```

The Helper stores:

```text
ProposalChannelManager
  helper_instance_id
  active_channel
  bounded_tombstones
```

An active channel contains its nonce, trusted Helper-local binding, state,
retry count, and optional pending final responder. The binding records the ACP
session, prompt, active pane target, and autofix origin. None of these values
come from the proposal payload.

Before accepting a pipe request, the Helper checks in order:

1. protocol version and frame limits;
2. Helper instance encoded in the channel;
3. active channel or bounded tombstone;
4. channel state is `Issued`;
5. atomic one-use transition to `Validating`;
6. strict proposal schema and origin policy;
7. trusted active target injection by App.

After a retryable schema rejection, the channel returns to `Issued`. The agent
may correct the payload and retry up to two times. An accepted proposal is
one-use. Lifecycle and user-decision terminal states are not retryable.

## Proposal schema and trusted target

The public payload is the versioned schema defined by
`tools/wta/src/agent_tools/action_proposal/schema.rs`. It uses
`deny_unknown_fields`, explicit count and size limits, and hand-written
conversion to `RecommendationSet`.

It never accepts:

- ACP session or Helper identifiers;
- window, tab, pane, prompt, or pipe identifiers;
- model-authored parent pane identifiers;
- arbitrary agent executable paths.

Terminal Agent proposals contain one to three choices. App binds `send` and
panel actions to the trusted pane captured for that turn and resolves delegate
actions only through configured policy.

Autofix accepts exactly one choice with one `send` action. App binds it to the
recorded failing pane and matching Autofix generation.

## Lifecycle

The ownership hierarchy is:

```text
Helper process
  -> active turn channel
```

Implemented lifecycle transitions are:

| Event | Result |
|---|---|
| New prompt in same session | Previous channel becomes `superseded`; issue a new channel |
| Card confirm | Atomically claim the live proposal, dispatch through the existing executor, then send `confirmed` |
| Card cancel/dismiss | Send `cancelled` |
| User-decision timeout | Send `timed_out` after 10 minutes |
| ACP transport lost | Send `unavailable` and refuse new channels |
| Helper pipe shutdown | Send `unavailable` and refuse new channels |

The Helper retains at most four terminal tombstones for three minutes.
Tombstones contain only a channel hash, terminal status, and timestamp. They
improve errors for late clients without retaining payload or target data. They
are not persisted.

## Security model

The channel is an unguessable, per-turn bearer handle. The per-user named pipe
ACL, Helper-instance routing, per-turn nonce, trusted Helper-local turn
binding, and one-use transition prevent normal cross-tab mistakes, stale turns,
and replay.

The Helper-side permission decision is optional. When present it confirms that
the canonical command references the current channel owned by the requesting
ACP session, but agents that do not emit a permission callback can submit the
same one-use channel directly. The channel only proposes a visible card; it
cannot mutate the terminal.

A malicious process that can read the complete issued channel from the agent
process and race that process can still use the bearer handle to choose a
schema-valid payload.
The direct-pipe design does not claim process attestation. This residual risk
is bounded because the CLI only proposes a visible card and user confirmation
is still required before mutation.

## Validation

Direct Helper routing is the only card-producing path for every configured ACP
agent. Integrations must prove canonical command execution, permission routing,
and Windows/WSL reachability directly; Assistant text cannot mask a broken
proposal path.

Automated and live coverage must include:

- channel parsing, uniqueness, one-use, retries, and tombstones;
- canonical rendering/parsing, quoting, extra-token rejection, and auto-policy;
- pipe framing, size limits, disconnects, and two-phase responses;
- wrong Helper, wrong turn, stale, direct submission without permission, and replay;
- schema and origin policy validation with trusted target injection;
- card confirm, cancel, supersede, timeout, and Helper shutdown;
- multi-tab and multi-window isolation;
- no Permission UI for an exact canonical proposal;
- Assistant JSON remains visible chat text and never surfaces a card;
- no terminal mutation before card confirmation;
- explicit Windows-target WTA build and packaged live verification.

## Related work

- PR #428: superseded MCP-based implementation.
- PR #484: WTA CLI proposal implementation and this direct Helper revision.
- Issue #445: delegate creation latency after card confirmation; independent of
  proposal transport.
