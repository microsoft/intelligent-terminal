# Conversation TUI Transcript

**Status**: Implemented  
**Base**: PR #611  
**Scope**: WTA ACP event reduction, turn lifecycle, replay, rendering, and interaction

## Problem

The conversation TUI currently represents one live assistant response in
several places:

- `TurnState::Streaming.buf`;
- `TabSession.pending_agent_response`;
- `TabSession.messages`;
- `TabSession.completed_turns`;
- `TabSession.tool_calls`.

Events then copy, flush, and clear data between those stores. This makes ACP
arrival order an emergent property of event-handler timing and caused visible
regressions such as assistant prose appearing after a later tool call and
completed tool output disappearing from the active view.

## Invariants

1. Each tab has one ordered active transcript.
2. Every visible ACP event is inserted into that transcript at arrival time.
3. Consecutive `agent_message_chunk` events append to the final assistant-text
   item; a tool or plan event closes that text segment naturally.
4. `tool_call_update` and terminal-output events update an existing tool item
   by protocol ID without changing its position.
5. Completing or cancelling a turn moves the active transcript into history;
   it does not clone visible content into a second representation.
6. Session replay has its own replay accumulator. It never shares live-turn
   streaming state.
7. Rendering policy is independent of transcript storage. Active previews and
   expanded history read the same tool data with different bounded limits.
8. Permission and user-input requests remain independent FIFO protocol
   requests. They are blocking action panels, not transcript ownership.

## Target model

```text
TabSession
├─ messages: [ChatMessage]              # current turn, ordered
├─ completed_turns: [CompletedTurn]     # prior turns, ordered
├─ turn: TurnState                      # lifecycle only
├─ replay: ReplayState                  # session/load only
├─ permission: FIFO
└─ user_input: FIFO

TurnState
├─ Idle
├─ Submitted(prompt)
├─ Streaming(prompt)
└─ Surfaced(prompt, outcome, end_pending)
```

The active assistant text lives in the final `ChatMessage::Agent` item, not in
`TurnState`. The typewriter cursor reveals that item without owning a second
copy of its text.

## Activity semantics

Activity is derived from current state in priority order:

1. awaiting permission;
2. awaiting structured user input;
3. running tool;
4. responding with visible assistant text;
5. waiting for the Agent.

Only the final state needs a generic `Thinking...` row. Permission and input
have action panels, running tools have active cards, and responding text is
itself visible progress.

## Rendering contract

Message rendering must produce a bounded block of rendered lines. Height
measurement uses that same block rather than independently reimplementing
wrapping rules. Width calculations use terminal display width so CJK and emoji
measure consistently with ratatui.

## End-to-end scenarios

- prose -> tool -> prose preserves ACP arrival order;
- parallel tools update independently in place;
- Read/Search results preview while active and expand from the same data;
- terminal output routes by TerminalId references;
- permission and user-input queues block one request at a time;
- cancellation commits the exact visible partial transcript once;
- `session/load` rebuilds turns without touching live streaming state;
- narrow panes preserve input and blocking actions before chat detail;
- long output remains bounded in active and expanded views.
