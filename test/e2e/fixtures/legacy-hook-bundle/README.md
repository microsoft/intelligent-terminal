# Frozen legacy hook bundle

These files are a **verbatim copy of the hook bundle Intelligent Terminal
shipped before PR #571** replaced the PowerShell bridge with `wtcli agent-hook`.
They exist so `Feature.LegacyHookBundle.Tests.ps1` can prove that a user who
installed hooks from an older release still gets agent events after upgrading
Terminal.

## Why a frozen copy rather than reading from git

The hooks a user already installed live in that CLI's own plugin cache — a
*copy* made at install time (`${CLAUDE_PLUGIN_ROOT}` resolves there, not into
the package). Upgrading Terminal does not rewrite them, and the auto-upgrade
that should refresh them can fail: `<cli> plugin install` replaces the whole
plugin directory, which Windows refuses while a CLI process holds a handle on
it. So old hooks calling into a new Terminal is a state real users reach, and it
has to keep working.

Reading the old bundle out of `origin/main` at test time would stop working the
moment #571 merges. Freezing it here is the point: this is a compatibility
contract, and a contract that silently disappears is not one.

## Do not edit these files

They are not a bundle we ship. They are the shape of an artifact already on
users' disks, so editing them to match new behaviour would delete the very
evidence the test depends on. If a change makes this test fail, the answer is
either to keep the old path working or to consciously drop support for it — not
to update the fixture.

## What they pin

Everything the legacy path needs from Terminal, all of which Terminal still owns
and could regress:

| Contract | Where |
|---|---|
| `wtcli send-event -e <type> [-p <pane>] "<json>"` accepts the legacy argv | `send-event.ps1` line ~339 |
| The published envelope is `method="agent_event"` with `params.{event,pane_id,cli_source,agent_session_id,payload}` | `BuildSendEventJson` |
| `WT_COM_CLSID` is the discovery variable, and its absence means "not in Terminal" | `send-event.ps1` line ~43 |
| `WT_SESSION` carries the pane id | `send-event.ps1` `-p` argument |
| `wtcli` is resolvable by that name on `PATH` | `send-event.ps1` lookup order |

`send-event` in particular is a **silent dependency**: nothing else in the tree
records that it must outlive `agent-hook`. Deleting it as "superseded" would
break every user whose hooks never refreshed, with no other test going red.
