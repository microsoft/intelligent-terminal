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

Nothing new is written, and no new field is added to what is written. Both kinds
of agent-bearing pane persist through layout fields Terminal already has:

| What | Where |
| --- | --- |
| Serialized tab layout | `persistedWindowLayouts` in `state.json` |
| Per-pane scrollback | `buffer_{guid}.txt` next to `state.json` |

Elevated windows already write to `elevated-state.json` and `elevated_{guid}.txt`,
so an elevated session and an unelevated one never see each other's agents.

Concretely, `state.json` gains no agent key and `wt` gains no agent option:

| What has to survive | Field it rides |
| --- | --- |
| Which conversation a shell pane was in | `commandline`, rewritten to the agent's resume invocation |
| That a pane is the agent pane | `type`, a key the layout format already has, with the new values `agent` and `agentStashed` |
| Which conversation, agent and view the agent pane had | `commandline`, in the stable `wta` resume form |
| Where the agent pane sat and how big it was | `SplitPaneArgs`' existing `split` and `size` |
| Where it started | `startingDirectory` |

## Saving

`TerminalPage::GetWindowLayout` builds the same `WindowLayout` it always did.
Because the agent data is part of the ordinary layout, every existing writer
picks it up for free:

| Writer | Covers |
| --- | --- |
| `WindowEmperor::_persistState`, on a five-minute timer | **a crash**, and anything else that never runs shutdown code |
| `WindowEmperor::_finalizeSessionPersistence` | closing the last window, quitting, sign-out, shutdown |
| `TerminalPage::_SaveWorkspaceIfNeeded` | named windows, which persist as workspaces |

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

`_StampAgentResumeCommandlines` is the whole of the save-side work: for each
persisted pane that has a binding, it replaces `commandline` with
`AgentPaneRestore::BuildResumeCommandline`. That command is rebuilt from the
agent id rather than taken from whatever string a hook supplied, and the session
id is validated before it lands inside a command line that will be executed.

**The agent pane.** It is an ordinary terminal pane hosting a `wta` helper, so
`BuildStartupActions` serializes it like any other pane — including when it is
stashed, because `StashAgentPane` only hides the pane and leaves it in the tree.
`AgentPaneContent::GetNewTerminalArgs` stamps the content type and swaps the live
helper command line for the stable resume form; `Pane::GetTerminalArgsForPane`
upgrades that type to `agentStashed` when the pane is hidden.

What it may **not** persist is the live helper command line. That names this
run's master pipe, this window's id, this tab's id, and a CLI path already
resolved through GPO `AllowedAgents` — meaningless after a restart, and in the
last case a frozen policy decision. So the saved form keeps only the session,
the agent identity (with the WSL distro folded into one `AgentPaneBackend`
token), the view, and a custom provider's command.

The agent identity has to be written down rather than recovered from the session
id, because nothing on the wta side outlives the process: `session_registry` is
an in-memory view of currently-connected sessions, and ACP `session/list` can
only be asked of an agent that is already spawned — so working out which agent
owns a session would mean starting every candidate agent first. `session/list`
is also an UNSTABLE capability that an agent may not implement at all.

For the pane to be there at all when a save runs, the Agent Pane profile is
`closeOnExit: graceful` rather than `always`. A helper that exits cleanly still
takes its pane with it, but one that was *killed* leaves the pane behind exactly
like every other profile. That matters because a system shutdown kills wta and
the agent CLIs while Terminal's own `WM_ENDSESSION` save is still queued.

An empty pre-warmed helper contributes no conversation: wta only projects a
session id through `TabSession::resumable_session_id` once the conversation is
meaningful, so a tab the user never chatted in restores its pane layout and no
conversation.

## Restoring

`TerminalWindow::Initialize` replays `WindowLayout::TabLayout()` unchanged — the
restore path needed no new pre-processing step, because there is no separate
agent metadata to distribute.

**Shell panes.** Nothing special happens: the pane is created from its persisted
`commandline`, which is the resume invocation. The one adjustment is that
`_MakeTerminalPane` skips seeding the saved scrollback when
`AgentPaneRestore::IsResumeCommandline` recognises what the pane is about to run.
The CLI replays its own transcript, so seeding as well would show the
conversation twice and compound it on every restart. Asking what the pane will
run — rather than consulting a persisted marker — means a pane the user pointed
at a resume command themselves behaves the same way.

**The agent pane.** `_HandleSplitPane` sees the agent content type and hands the
action to `_RestoreAgentPaneFromLayout` instead of `_MakePane`, because the pane
cannot be built from saved state alone. That reads the session, agent, view and
custom command back out of the command line and calls the ordinary spawn path,
which re-detects wta, re-acquires the shared master, re-derives the owner ids and
— importantly — re-applies GPO `AllowedAgents`. A saved layout can therefore
never launch an agent that policy now forbids. The conversation comes back
through a boot-time ACP `session/load` driven by wta's
`--initial-load-session-id`, and `agentStashed` restores the pane already
toggled away.

Pre-warm is suppressed for the duration of a startup replay
(`_replayingStartupActions`), because a tab is created before the `splitPane`
that carries its agent pane. `_PrewarmAgentPanesAfterStartup` then gives a
stashed helper to every tab the replay left without one.

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
* An agent pane whose helper was killed now stays visible instead of vanishing,
  because the profile is `closeOnExit: graceful`. That is what makes it
  survivable in a save, but it is a visible behavior change.
