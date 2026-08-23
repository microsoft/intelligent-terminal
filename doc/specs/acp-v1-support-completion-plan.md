# ACP v1 Support Completion Plan

**Status**: Implementation in progress; Phase 1 core tool details are on `main`;
Phase 4 select-option support is implemented locally
**Scope**: `tools/wta`, the agent-pane TUI, and the helper/master ACP bridge  
**Baseline**: ACP wire `protocolVersion: 1`  
**Last updated**: 2026-08-12

## Summary

Intelligent Terminal already supports the core ACP v1 flow:

- initialize, authenticate, `session/new`, `session/load`, prompt, and cancel;
- streamed text and thought messages;
- tool-call creation and updates;
- permission requests;
- Client Terminal callbacks;
- plans, images, model selection, and session usage;
- helper/master multiplexing across tabs and windows.

PR #601 improved ACP tool-call visibility by surfacing locations, commands,
working directories, output, and exit codes. This plan continues that work and
fills the remaining gaps in the **stable ACP v1** surface.

This is not an ACP wire v2 migration. References to a "follow-up" below mean
additional ACP v1 work after PR #601.

## Goals

1. Render the standard ACP data that agents already send instead of relying on
   agent-specific `rawInput` and `rawOutput` conventions.
2. Make every capability we advertise conform to its ACP v1 lifecycle and data
   semantics.
3. Use capability negotiation consistently and degrade cleanly when an agent
   omits an optional capability.
4. Replace model-specific UI paths with reusable ACP configuration surfaces.
5. Keep the helper/master proxy transparent for standard ACP requests,
   responses, notifications, metadata, and cancellation.

## Non-goals

- Enabling ACP wire `protocolVersion: 2` in production.
- Removing ACP v1 support or changing the helper/master architecture.
- Implementing unstable proposals such as session fork, MCP-over-ACP, remote
  HTTP/WebSocket ACP transport, session compaction, or plan operations.
- Adding direct Client filesystem callbacks solely for protocol completeness.
  Agents already execute in the target environment, Intelligent Terminal has
  no unsaved editor buffer to expose, and ACP v2 removes that surface. Add them
  later only if a supported agent has a concrete compatibility requirement.

## Current gaps

| Area | Current behavior | Missing ACP v1 behavior |
|---|---|---|
| Tool-call details | Text content plus selected `rawInput`/`rawOutput` fields are rendered. | Standard diff and terminal content, rich content blocks, location line numbers, and a full-detail view. |
| Client Terminals | Commands can run in a WT pane or local subprocess. | `outputByteLimit`, retained-output semantics, accurate truncation, final output after kill, and display retention after release. |
| Session lifecycle | New, load, cancel, and partial list flows exist. | Capability-gated resume, close, delete, list pagination/filtering, and `session_info_update`. |
| Session config | Model select options are extracted. | Generic select options, boolean options, mode/reasoning/model-config categories, ordering, and dependent option updates. |
| Agent commands | WTA has its own slash commands. | `available_commands_update` is ignored instead of contributing session-scoped Agent commands. |
| Authentication | CLI-specific login plus first-method post-login authenticate. | Auth-method selection, standard logout, and clearer capability-driven state. |
| Cancellation | `session/cancel` stops the active prompt locally and notifies the Agent. | Stable request-ID `$/cancel_request` and consistent cancellation of pending reverse requests. |
| Structured input | Permission cards handle approval choices. | Stable ACP elicitation form and URL modes. |
| Rich content | Text and outgoing pasted images are supported. | Message IDs, resource links, embedded resources, audio, and non-text Agent/tool output. |
| Workspace/MCP | One cwd and the proposal HTTP MCP endpoint are supported. | Additional workspace roots and a general session MCP configuration surface. |

## Design principles

### Standard fields first

Standard typed ACP fields are authoritative. `rawInput`, `rawOutput`, and
implementation metadata remain best-effort fallbacks. A missing agent-specific
field must not erase a standard diff, terminal reference, resource, status, or
location.

### Capability-gated behavior

The initialize response is stored as the per-Agent capability snapshot.
Optional methods are called only when advertised. Unsupported capabilities are
hidden or disabled in the UI rather than attempted and handled through
`MethodNotFound`.

### Preserve unknown data through the bridge

The master should route standard and extension data without narrowing it to the
fields currently rendered by the helper. Unknown variants must degrade to a
generic representation or be preserved for diagnostics, not break the ACP
connection.

### Session-scoped state

Commands, config options, messages, tool calls, plans, terminals, permissions,
and elicitation requests are keyed by ACP SessionId and their protocol IDs.
State from one tab or Agent session must not affect another session sharing the
same master.

### Aggregate permission presentation, not authorization

Concurrent Agent permission requests remain independent ACP requests with
independent responders and Agent-provided options. The TUI presents one
actionable FIFO request, shows the remaining count and a bounded preview of
queued requests, and resolves exactly one request per user action. It must not
merge authorization scopes or synthesize options such as `AllowAlways`.

## Delivery plan

### Phase 0: Capability baseline and conformance harness

1. Record all Agent and Client capabilities from `initialize` in a typed,
   session-independent connection snapshot.
2. Update stale assumptions that still describe stable methods such as
   `session/list` as unstable.
3. Add mock-Agent scenarios for every capability used by the following phases,
   including omitted capabilities and unknown enum variants.
4. Upgrade the Rust ACP SDK within the stable ACP v1 wire surface so the crate
   exposes the current stable schemas needed for boolean config,
   request cancellation, and elicitation.
5. Regenerate `tools/wta/cgmanifest.json` and the WTA block in `NOTICE.md` if
   the dependency graph changes.

**Exit criteria**

- The negotiated wire version remains `1`.
- Unsupported optional methods are never called.
- Capability decisions are covered by deterministic mock-Agent tests.

### Phase 1: Tool Details follow-up to PR #601

**Implementation status**: Core support landed on `main` through PRs #601 and
#611.
Standard content, diffs, terminal references/output, rich-content placeholders,
location lines, collection replacement, persistence compatibility, and
expanded completed-turn details are covered. A direct file-open action and a
separate raw-JSON diagnostics surface remain follow-ups because the TUI does
not currently have a safe file-handler selection model.

1. Extend the internal tool content model beyond plain text:
   - `Content`;
   - `Diff`;
   - `Terminal`.
2. Render ACP v1 diffs using `path`, `oldText`, and `newText`, including new
   files where `oldText` is absent.
3. Link terminal content to the matching Client Terminal and stream its output
   into the tool details.
4. Preserve and display location line numbers; add a follow/open action where
   the path can be resolved safely.
5. Render image/resource content or provide an explicit attachment placeholder
   instead of silently dropping it.
6. Keep the compact card concise and provide an expanded details view for full
   output, raw diagnostics, and truncation state.

**Exit criteria**

- No standard ACP v1 `ToolCallContent` variant is silently discarded.
- Tool updates replace collection fields according to ACP v1 semantics.
- PR #601 behavior remains unchanged for agents that only send text and raw
  output.

### Phase 2: Client Terminal conformance

1. Track retained output per terminal rather than returning deltas for local
   subprocesses and snapshots with incompatible semantics for WT panes.
2. Honor `outputByteLimit`, trimming from the beginning at a valid UTF-8
   character boundary.
3. Return an accurate `truncated` flag and complete exit status.
4. Make `terminal/kill` terminate the command while preserving final output and
   a valid terminal ID until `terminal/release`.
5. Make `terminal/release` free execution resources while retaining any output
   already attached to a tool-call card.
6. Replace the silent WT-to-local fallback with explicit logging and UI status;
   do not make a hidden execution mode look like a WT-backed terminal.
7. Add tests for repeated output reads, split UTF-8 sequences, byte limits,
   kill-then-output, wait-after-kill, and release.

**Exit criteria**

- Every method covered by the advertised `terminal: true` capability follows
  the ACP v1 terminal contract.
- A command's final output remains visible after terminal release.

### Phase 3: Session lifecycle and metadata

1. Gate history enumeration on `sessionCapabilities.list`.
2. Follow opaque `nextCursor` values until pagination completes and support the
   optional cwd filter.
3. Apply `session_info_update` patches to title and update time in the master
   registry and every owning helper.
4. Add `session/resume` for reconnecting without replay, while retaining
   `session/load` for explicit history reconstruction.
5. Call `session/close` when a live agent-pane session is intentionally
   destroyed and the Agent advertises close support.
6. Add capability-gated session deletion with a confirmation flow that clearly
   distinguishes:
   - close active resources;
   - remove a session from history.
7. Carry additional directories through list/load/resume once Phase 6 exposes
   workspace-root selection.

**Exit criteria**

- Session history handles multiple pages and live metadata updates.
- Pane destruction does not leave close-capable Agent sessions active.
- Close and delete are separate user actions with separate tests.

### Phase 4: Generic config options and Agent commands

**Implementation status**: The first Config Options slice is implemented
locally. WTA preserves ordered select options per Session, replaces complete
snapshots from new/load/update/set responses, exposes a generic `/config`
two-level picker, and routes `/model` through the standard model option when
one exists. Boolean options and Agent commands remain follow-ups.

1. Store the complete ordered `configOptions` collection per session.
2. Render all supported select options, not only the `model` category.
3. Advertise and render boolean config options after the client UI can round
   trip boolean values.
4. Treat categories as presentation hints:
   - `model`;
   - `model_config`;
   - `mode`;
   - `thought_level`;
   - unknown/custom categories.
5. Send `session/set_config_option` with typed values and replace local state
   with the complete response collection.
6. Keep legacy session modes only as compatibility fallback when config options
   are absent.
7. Merge `available_commands_update` into the command popup per session while
   keeping WTA-reserved commands deterministic and collision-safe.

**Exit criteria**

- An Agent can add, remove, reorder, and update options dynamically.
- Model, reasoning, mode, and boolean settings use one reusable picker/control
  model.
- Agent slash commands disappear when a later update removes them.

### Phase 5: Elicitation, authentication, and cancellation

1. Implement stable form-mode elicitation:
   - restricted schema validation;
   - editable review before submission;
   - accept, decline, and cancel outcomes;
   - explicit rejection of secret/credential collection.
2. Implement URL-mode elicitation:
   - show the full URL and emphasized host;
   - require consent before opening;
   - do not prefetch;
   - track opaque completion IDs and ignore unknown completions.
3. Advertise only the elicitation modes whose UI is implemented.
4. Present multiple advertised authentication methods instead of choosing the
   first silently.
5. Add capability-gated ACP logout and transition active sessions to an
   explicit re-authentication/error state if later operations fail.
6. Add `$/cancel_request` routing by JSON-RPC request ID.
7. Ensure prompt cancellation resolves pending permission and elicitation
   requests as cancelled while still tolerating late Agent notifications.

**Exit criteria**

- Structured input never falls back to an unsafe secret-bearing form.
- User cancellation cannot leave an Agent waiting indefinitely on a reverse
  request.
- Authentication method choice and logout are protocol-driven.

### Phase 6: Rich content, workspace roots, and general MCP

1. Preserve optional message IDs and use them to group replayed/streamed
   messages correctly.
2. Support resource links as baseline prompt content.
3. Add capability-gated embedded resources and audio input.
4. Render or explicitly represent non-text Agent message content.
5. Add a workspace-root picker and send the complete absolute
   `additionalDirectories` list on new/load/resume only when advertised.
6. Generalize session MCP configuration beyond the built-in proposal endpoint:
   - stdio is baseline;
   - HTTP is sent only when advertised;
   - SSE remains compatibility-only because MCP deprecated it;
   - credentials in headers/environment are never shown in logs or chat.

**Exit criteria**

- Rich content is preserved end to end instead of silently flattened.
- Multi-root sessions restore the exact intended root set.
- User-configured MCP servers coexist with the proposal MCP endpoint.

## Cross-cutting implementation work

### Data model

The TUI needs typed, ID-addressed models for:

- messages keyed by optional ACP message ID;
- tool calls keyed by ToolCallId;
- Client Terminals keyed by TerminalId;
- config options keyed by ConfigId while preserving Agent ordering;
- pending reverse requests keyed by JSON-RPC request ID;
- Agent commands and metadata scoped to SessionId.

This refactor remains an ACP v1 implementation improvement. It should avoid
assuming that a session update is valid only while one local prompt future is
pending, but it does not enable ACP v2 behavior.

### Helper/master routing

For every added method or notification:

1. route by SessionId to the owning helper;
2. preserve `_meta`;
3. preserve cancellation and error codes;
4. avoid blocking the shared Agent connection on one stalled helper;
5. remove routing state when a session closes or is deleted.

### Diagnostics

Add structured fields for capability decisions, session method, SessionId,
ToolCallId/TerminalId, request ID, and result. User content, commands, file
content, form responses, URLs with sensitive query data, and credentials remain
trace-only or redacted according to existing logging policy.

## Validation strategy

Each phase requires:

- parser/reducer unit tests for every supported variant and patch rule;
- mock-Agent end-to-end tests through the real helper ACP client;
- master routing tests for multi-helper ownership and disconnects;
- TUI render/input tests for new cards, pickers, and confirmation flows;
- live smoke tests against each built-in Agent that advertises the capability;
- regression coverage for capability omission and `MethodNotFound`.

The WTA test suite remains the minimum local validation:

```powershell
cargo test --manifest-path tools/wta/Cargo.toml
```

## Rollout

Each phase can ship independently behind capability checks. New UI should remain
invisible for Agents that do not advertise the matching feature. Where a
behavior changes an already-advertised capability, especially Client
Terminals, land conformance tests before changing the runtime path.

ACP wire v2 experimentation, if pursued later, must use separate version
negotiation and a feature flag. It must not be bundled into this plan.

## References

- [ACP v1 overview](https://agentclientprotocol.com/protocol/v1/overview)
- [ACP v1 schema](https://agentclientprotocol.com/protocol/v1/schema)
- [ACP initialization and capabilities](https://agentclientprotocol.com/protocol/v1/initialization)
- [ACP session setup](https://agentclientprotocol.com/protocol/v1/session-setup)
- [ACP tool calls](https://agentclientprotocol.com/protocol/v1/tool-calls)
- [ACP Client Terminals](https://agentclientprotocol.com/protocol/v1/terminals)
- [ACP session config options](https://agentclientprotocol.com/protocol/v1/session-config-options)
- [ACP elicitation](https://agentclientprotocol.com/protocol/v1/elicitation)
- [ACP cancellation](https://agentclientprotocol.com/protocol/v1/cancellation)
- [PR #601: Improve ACP tool call output visibility](https://github.com/microsoft/intelligent-terminal/pull/601)
