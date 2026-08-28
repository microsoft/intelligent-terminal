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

Two kinds of binding are recorded, and both are designed to outlive the process
they describe — the same property that lets an ordinary pane come back after its
program was killed.

**Shell panes.** `_paneAgentSessions` holds the most recent agent session seen in
each shell pane, keyed by the pane's connection `SessionId`. The binding arrives
from the agent hooks, which reach `OnPaneAgentSessionChanged` through the COM
broadcast route, or from an in-band `OSC 9001;AgentEvent` on the pane's own VT
stream. Both carry a real pane identity: a hook reports the `WT_SESSION` its
subprocess inherited, and an in-band event is attributed to the pane it arrived
on. A hook subprocess that inherited no `WT_SESSION` publishes an empty `pane_id`
and is dropped rather than attributed to the focused pane. A CLI that exits
gracefully drops its binding — there is nothing left to resume, and `closeOnExit`
closes that pane anyway. A CLI that was *killed* keeps it: the pane survives a
failed exit, so a save that happens later can still describe how to bring the CLI
back. The binding is dropped for good in `_NotifyPanesClosing`, when the pane
itself goes away.

**The agent pane.** Its ACP session, agent identity (including the WSL distro,
folded into one `AgentPaneBackend` token), custom command, view, open state,
position, and split size live in `AgentPaneContent` — a XAML object that dies
with its pane, and the Agent Pane profile is `closeOnExit: always`, so the pane
dies the moment the helper does. A save that had to read the live pane would
therefore describe no agent at all whenever the helper went down first, which is
exactly what a system shutdown produces: it kills wta and the agent CLIs while
Terminal's own `WM_ENDSESSION` save is still queued.

So the pane does not own its restore data. `Tab::AgentRestoreRecord` does.
`_RefreshAgentRestoreRecord` writes it whenever the live pane can be read — at
creation, and after every `agent_state_changed` projection from wta — and
`_AddAgentRestoreMetadata` stamps the record, never the pane. The record is
cleared only when the agent pane is deliberately taken away: `_TeardownAgentPane`,
a user closing the pane, or a move to another window. A pane lost to a dying
helper keeps its record and comes back.

An agent pane's own CLI never lands in `_paneAgentSessions`. `wta-master` spawns
it outside every conpty, so it inherits no `WT_SESSION`, its hooks publish an
empty `pane_id`, and the event is dropped. The one way an agent pane's ACP
session can also be a shell binding is a session that was first run in a shell
pane and later resumed into an agent pane; `SetPersistedLayoutAgentRestorePaths`
covers that on the restore side, across the whole window.

An empty pre-warmed helper is never recorded: wta only projects a session id
through `TabSession::resumable_session_id` once the conversation is meaningful,
so a tab the user never chatted in restores its pane layout and no conversation.

## Restoring

`TerminalWindow::Initialize` already replays `WindowLayout::TabLayout()` for both
"Restore window layout" modes. Before handing the actions to `TerminalPage`, it
calls `SetPersistedLayoutAgentRestorePaths`, which points each agent-bound shell
pane at its `buffer_{guid}.txt` through `NewTerminalArgs::PersistedBufferPath`.

That property is an override, not the only source of the path: `_MakeTerminalPane`
derives the same `buffer_{guid}.txt` from `SessionId` for every pane, which is how
ordinary panes get their scrollback back. What the stamp adds is a marker saying
"this pane came out of a persisted layout" — `ShouldResumeAgentSession` requires
it, so an `AgentSessionId` arriving any other way (a `wt` commandline, say) starts
a normal shell rather than silently re-attaching to an old conversation.

**Shell panes.** `_MakeTerminalPane` rewrites the pane's commandline to the
agent's resume command, either the one recorded at save time or one rebuilt from
the agent id. A resumed pane skips buffer restore: the CLI replays its own
transcript, and seeding the buffer as well would show the conversation twice and
compound it on every restart.

**The agent pane.** A pane whose agent session belongs to an agent pane is
skipped by `SetPersistedLayoutAgentRestorePaths`, so it never gets the marker and
is never resumed as a shell command on top of the agent pane that owns that
conversation. The pane itself is rebuilt through `_pendingAgentPaneRestores` with
its recorded view, open state, position, and size, and its conversation comes
back through a boot-time ACP `session/load` driven by wta's
`--initial-load-session-id`.

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
