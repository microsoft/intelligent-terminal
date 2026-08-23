# Remote Agent Host implementation status

- Branch: `dev/vanzue/remote-agent-host-mvp`
- Updated: 2026-08-23
- Architecture: [Remote Intelligent Terminal Sessions via Agent Host Protocol](remote-agent-host-via-ahp.md)

## Implemented

- Standalone authenticated loopback AHP host with durable canonical state,
  host identity, ordered actions, bounded replay, snapshot fallback, and
  session catalogue notifications.
- Long-lived Remote Client integration in `wta-master`, including reconnect,
  catalogue refetch, retry, and read-only remote rows in `/sessions`.
- Host-native Session lifecycle:
  - `createSession` and `disposeSession`
  - ACP `initialize`, `session/new`, and `session/close`
  - durable `creating`, `ready`, and `creationFailed` states
  - working-directory and ACP Session ID persistence
- Host-native Chat lifecycle:
  - `createChat`
  - `chat/turnStarted` mapped to ACP `session/prompt`
  - streamed ACP text mapped to `chat/responsePart` and `chat/delta`
  - durable complete, cancelled, and error turns
  - AHP cancellation mapped to ACP `session/cancel`
- Parent Session aggregation:
  - Chat status, activity, and modification time update the Session
  - replayable `session/chatAdded`, `session/defaultChatChanged`, and
    `session/chatUpdated` actions
  - root catalogue summary notifications
- Provider lifecycle:
  - agent CLI processes pooled by provider
  - ACP I/O termination and prompt/session creation failures evict the provider
  - generation checks prevent stale exits from evicting a replacement process
  - active prompts are interrupted and affected Sessions/turns become Error
  - subsequent Session creation starts a fresh provider process

## Current limitations

- The external transport is still authenticated loopback TCP; Azure Web PubSub
  and native Entra client authentication are not connected.
- TerminalApp has no remote Chat or terminal pane UI.
- The Host does not yet own PTYs or implement AHP terminal channels.
- ACP tool calls, permission requests, file operations, and terminal requests
  are explicitly unsupported.
- The Chat MVP supports one Chat per Session, text-only prompts, and Markdown
  response parts.
- Existing Sessions cannot be resumed after their provider process exits;
  newly created Sessions restart the provider.

## Next implementation stages

1. Add a process-backed mock ACP agent test that exercises the real stdio
   `initialize` -> `session/new` -> `session/prompt` -> `session/cancel` ->
   `session/close` path, including streaming and provider crashes.
2. Add the Intelligent Terminal remote Chat UI with snapshot/replay,
   reconnect, prompt, cancellation, and error handling.
3. Implement Host-owned PTYs and AHP terminal channels, followed by baseline
   ACP permission and file-operation handling.
4. Replace the loopback transport with Azure Web PubSub and native Entra
   authentication.
5. Complete multi-client authorization, conflict handling, observability, and
   production hardening.

## Validation

The explicit-target WTA suite currently passes:

```text
1674 passed; 0 failed
```
