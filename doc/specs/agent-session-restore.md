# Restoring agent sessions with a persisted window layout

## Abstract

Windows Terminal already brings a window's tabs back after the window closes,
after Terminal exits, and after Terminal crashes. It replays a serialized list
of actions from `state.json` and, with **Restore window layout and content**,
seeds each pane's scrollback from a `buffer_{guid}.txt` file.

That restore stops at the shell. A pane that was running an agent CLI comes back
as a bare prompt, and the tab's agent pane comes back empty — the conversation
the user was in the middle of is gone even though the panes around it survived.

This describes how the persisted layout is extended to carry enough agent
identity to put both back.

Keeping the shells themselves alive, saving individual closed tabs, and browsing
a history of past sessions are separate features and are not described here.

## Storage

Nothing new is written. The agent metadata rides the two files Terminal already
maintains:

| What | Where |
| --- | --- |
| Serialized tab layout, including agent metadata | `persistedWindowLayouts` in `state.json` |
| Per-pane scrollback | `buffer_{guid}.txt` next to `state.json` |

Elevated windows already write to `elevated-state.json` and `elevated_{guid}.txt`,
so an elevated session and an unelevated one never see each other's agents.

## Saving

`TerminalPage::GetWindowLayout` builds the same `WindowLayout` it always did,
then `_AddAgentRestoreMetadata` stamps each tab's agent bindings onto the
`NewTerminalArgs` of the actions that rebuild it. Because the metadata is part of
the ordinary layout, every existing writer picks it up for free:

| Writer | Covers |
| --- | --- |
| `WindowEmperor::_persistState`, on a five-minute timer | **a crash**, and anything else that never runs shutdown code |
| `WindowEmperor::_finalizeSessionPersistence` | closing the last window, quitting, sign-out, shutdown |
| `TerminalPage::_SaveWorkspaceIfNeeded` | named windows, which persist as workspaces |

Two kinds of binding are recorded.

**Shell panes.** `_paneAgentSessions` holds the most recent agent session seen in
each shell pane, keyed by the pane's connection `SessionId`. The binding arrives
either from the agent hooks or, when no hook is installed, from wta's own
`pane_agent_session_changed` event. It is kept after the CLI exits, so a save
that happens later can still describe how to resume it.

**The agent pane.** Its ACP session, agent identity (including the WSL distro,
folded into one `AgentPaneBackend` token), custom command, view, open state,
position, and split size are recorded on the tab's first action.

`RemoveAgentPaneSessionFromShellBindings` then clears the agent pane's own ACP
session from the shell pane that hosts the helper. Without it the restore would
relaunch that CLI twice — once as an agent pane and once as a shell.

An empty pre-warmed helper is never recorded: wta only projects a session id
through `TabSession::resumable_session_id` once the conversation is meaningful,
so a tab the user never chatted in restores its pane layout and no conversation.

## Restoring

`TerminalWindow::Initialize` already replays `WindowLayout::TabLayout()` for both
"Restore window layout" modes. Before handing the actions to `TerminalPage`, it
calls `SetPersistedLayoutAgentRestorePaths`, which points each shell pane at its
`buffer_{guid}.txt` through `NewTerminalArgs::PersistedBufferPath`.

That path does double duty. It is the file `ControlCore::RestoreFromPath` seeds
the buffer from, and it is the marker that says "this pane came out of a
persisted layout" — `ShouldResumeAgentSession` requires it, so an `AgentSessionId`
arriving any other way (a `wt` commandline, say) starts a normal shell rather
than silently re-attaching to an old conversation.

**Shell panes.** `_MakeTerminalPane` rewrites the pane's commandline to the
agent's resume command, either the one recorded at save time or one rebuilt from
the agent id. A resumed pane skips buffer restore: the CLI replays its own
transcript, and seeding the buffer as well would show the conversation twice and
compound it on every restart.

**The agent pane.** Panes whose agent session belongs to an agent pane are
skipped by `SetPersistedLayoutAgentRestorePaths` for the same reason. The pane
itself is rebuilt through `_pendingAgentPaneRestores` with its recorded view,
open state, position, and size, and its conversation comes back through a
boot-time ACP `session/load` driven by wta's `--initial-load-session-id`.

## Capabilities and limitations

* Layout and agent bindings survive a crash, because the five-minute timer has
  already written them. At most the last five minutes of arrangement is lost.
* Scrollback does **not** survive a crash. `buffer_{guid}.txt` is only written on
  the way out, which is Windows Terminal's existing behavior and is unchanged
  here. A resumed agent CLI is unaffected — it replays its own history.
* Running processes are not preserved. A build that was halfway through when the
  window closed is not halfway through when it comes back.
* Restoring is gated on the existing **Settings → Startup → "When Terminal
  starts"** preference. `defaultProfile` restores nothing, as before.
