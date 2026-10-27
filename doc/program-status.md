# OSC 7501 program status

The Dev build recognizes revision 0.4 of the
[Program Status Protocol](https://www.superlogical.com/rex/docs/build/program-status).
Preview, Release and Windows Inbox builds do not advertise this experimental
capability.

Programs report `idle`, `working`, `blocked`, `done` or `error`. `clear` removes
a record or a slash-delimited subtree. Reports replace a record, including its
optional fields; they do not patch older fields. Tasks are scoped to the actual
terminal connection, not a title, executable name or inferred conversation.

The wire field for explanatory text is `msg` (base64 UTF-8), not `message`.
It is exposed as `message` in internal snapshots and events. The status line
appends the current task's message when present, otherwise its title; tooltips
retain both.

## Display and attention

Each pane retains its task hierarchy. Tab metadata counts **panes**, not jobs;
a pane can contribute to both working and blocked counts. A completed root never
hides an active descendant. Progress appears only when it describes one
unambiguous working task; unrelated percentages are not averaged.

In open sidebar tabs and panes, program activity occupies the existing
**Agent status** metadata field, not an additional metadata row. Conversation/tab
titles and working directories are unchanged. Turning that field off hides the
program-status line in those entries. Recent agent-session rows retain their
independent status/age visibility and are not controlled by the tab metadata
selection. The line has one localized primary status, an optional percentage,
and the representative task's message (or title when its message is empty).
It does not add unread-result symbols or secondary completion labels:
for example, `Active 40% · Running tests` or `Completed · Tests passed`.
The status and percentage precede the ellipsized description so long program
text cannot push the important status off the line. Current Idle messages also
appear after the status; old Done descriptions are not relabeled as current
Idle messages. Full task details remain in the tooltip. A real input request
uses its own description, never an unrelated Working task's message.

Blocked work takes precedence over working work. An actual hook/ACP input request
cannot be answered by a program reporting Working, Done or Clear. Unseen errors
and completions remain observable even when a subsequent report immediately
returns to Idle. Focusing a pane does not acknowledge a result; deliberate input
does. The tab context menu also provides **Mark results as seen**, which
acknowledges only the result revisions observed when invoked, across that tab's
panes. Acknowledging a result never clears a blocked task.

**Next pane needing attention** searches only the current window. Actual input
requests take priority over blocked work, errors, general attention and unseen
completions. Repeated invocation cycles through equally urgent panes in tab/pane
order. It does not reorder rows or acknowledge results.
Busy panes with unread completions remain navigation targets. A target in a
stashed assistant pane is restored without changing its chat/session view.

Tab and pane tooltips show bounded, plain-text task details, including task IDs,
application labels, titles, blocking reasons and messages. Program text is
untrusted: it does not grant permissions, open links, execute commands or bind
an agent conversation. Direction controls are removed from untrusted display
text and fields are isolated from surrounding chrome; natural RTL text and
language-shaping joiners are preserved. A retained result whose task has moved
on is explicitly labeled **Previous result**, not presented as its current state.

### Complete status text examples

These examples use the English native UI and illustrative producer messages.
Unless stated otherwise, no other canonical activity or unread result takes
precedence. The previous column compares the same compatible activity value
before OSC support; it does not imply the previous terminal parsed OSC.
Ordinary CLIs could previously have no Agent status field at all.

| OSC report / result situation | Compatible activity | Previous complete text | Current complete text |
|---|---|---|---|
| `idle`, no message or unread result | `Idle` | `Idle` | `Idle` |
| `idle`, `msg=Ready`, no unread result | `Idle` | `Idle` | `Idle · Ready` |
| `working`, progress 40%, `title=Build`, `msg=Running tests` | `Working` | `Active` | `Active 40% · Running tests` |
| The same Working report, plus unread Done | `Working` | `Active` | `Active 40% · Running tests` |
| The same Working report, plus unread Error | `Working` | `Active` | `Active · Running tests` |
| Canonical hook/ACP activity remains Working; OSC reports Done | `Working` | `Active` | `Active` |
| `blocked/question`, `msg=Choose an option` | `Attention` | `Waiting for input` | `Waiting for input · Choose an option` |
| `blocked/permission`, `msg=Approve deployment` | `Attention` | `Waiting for input` | `Permission requested · Approve deployment` |
| `blocked/auth`, `msg=Sign in` | `Attention` | `Waiting for input` | `Waiting for authorization... · Sign in` |
| Canonical input request, `input_message=Approve command?` | `Attention` | `Waiting for input` | `Waiting for input · Approve command?` |
| `done`, `msg=Tests passed`, unread | `Idle` | `Idle` | `Completed · Tests passed` |
| Done replaced by Idle, completion unread, no current message | `Idle` | `Idle` | `Completed` |
| Done acknowledged, otherwise inactive | `Idle` | `Idle` | `Idle` |
| `error`, `msg=Build failed`, unread | `Error` | `Error` | `Error · Build failed` |
| The same Error report, plus another unread Done | `Error` | `Error` | `Error · Build failed` |
| Clear removes all records/results, otherwise inactive | `Idle` | `Idle` | `Idle` |
| Live session in another window, Done unread, `msg=Tests passed` | `Idle` | `Idle · In another window` | `Completed · Tests passed · In another window` |

The composition is `status + optional progress + optional description`, without
decorative result markers. The matching current message wins over its title;
if the message is empty, the title is the fallback. An unread Error suppresses
the percentage. The other-window suffix keeps its existing eligibility rules.
Directory/repository/branch metadata remains in its existing separate fields.

## Shell command fallback

Dev connections export `INTELLIGENT_TERMINAL_PROGRAM_STATUS=1`, including into
WSL. Updated PowerShell and Bash integration scripts use that capability marker
without reading terminal input. Other branding configurations export `0`.
PowerShell needs its existing FullLanguage PSReadLine wrapper; Bash needs 4.4+
for `PS0`. Older or restricted shells keep their existing OSC 133 integration.

The scripts report `working` after OSC 133 C, then `done` or `error` before the
next OSC 133 A, followed by `idle`. Initialization, empty input and prompt
redraws do not manufacture command results. Bash treats exit 130 as cancellation;
PowerShell treats an observed `PipelineStoppedException` as cancellation.
Reports contain no command text, arguments, working directory or output.
The stable record ID is `it-shell/<shell-process-id>/command`, with `app=bash`,
`pwsh` or `powershell`. Changing foreground shells replaces the shell fallback
instead of leaving an outer shell permanently Working.

TerminalCore reserves that ID shape for presentation-only fallback reports.
Any valid non-shell report, including Idle or Clear, takes over the foreground
command and removes the shell fallback. Subsequent shell completion and Idle
reports stay suppressed until the next OSC 133 C. Queries and rejected reports
do not take over. RIS, connection replacement and disabling the feature reset
this arbitration. The ID convention is not authenticated and grants no agent
identity or permissions.

## Hooks and sessions

OSC status is an additive facet of an already admitted native agent pane. It
never creates an ACP session or guesses a conversation from an application
label. Real hooks and ACP own canonical activity when available. For qualified,
born-bound hookless sessions, current program state can supply activity ahead
of file-watcher inference. Done means a completed task, **not** an ended process,
disconnected helper or historical session.
Canonical Working activity also remains visible when a program reports Done,
Idle or Clear; retained results do not imply that the agent has stopped working.
Conversely, a hook's turn-end Stop/Idle does not delete program-reported Working
or Blocked tasks. Each source updates its own facts and the presentation reduces
both, rather than choosing the last event received. While work continues, an
unseen task error is retained in the result facet and task details, not appended
as a decorative status marker or treated as proof that all work failed.
Actual connection/canonical failures still take precedence over Working.

Within a connection/epoch, older or duplicate program revisions are rejected;
receiver generations and qualified pane/session ownership fence stale
connections. Hook and OSC streams do not share a universal turn ID or producer
sequence. Consequently a delayed old-turn hook cannot always be distinguished
from a current-turn hook. Receiving an event later does not prove it describes
newer work, and no timeout fabricates completion when the sources disagree.

The compatible `pane_statuses` response adds optional `program_status` metadata
and explicit `input_required`/`canonical_error`/`canonical_working` flags. These
flags retain real request/error/activity precedence rather than guessing it
from a program-reported state.
The optional `input_message` carries a real request's description. Program
snapshots carry the representative `task_id` and `title` alongside `message`,
allowing local and recent-session rows to produce the same single-line summary
without sending the complete task tree through WTA.
Existing `AgentStatus` strings do not change. Normal programs and shells can
display their status locally without WTA or an agent identity.

Live session-list rows also receive response-only `pane_status` metadata when
exactly one live conversation owns the matching provider, execution source,
history universe and pane. Historical, ambiguous and merely inferred
conversations do not receive it.
The session picker and sidebar history use that facet for the primary status,
working progress and current message without changing canonical status,
identity, activation or resume behavior. Refreshes update retained sidebar rows
in place; closing or rebinding a pane removes the facet. It is not registry or
persisted history state.

## Lifecycle and limits

Reports have no heartbeat or timeout. OSC 133 A and actual process exit remove
live Working/Blocked/Idle records while retaining results. RIS clears protocol
state; DECSTR and alternate-screen transitions preserve it. Once accepted,
program status takes precedence over legacy OSC 9;4/BEL activity until RIS;
Clear does not resurrect stale legacy progress.

The parser uses a conservative 4087-byte body budget so a complete wire sequence
fits within 4096 bytes. It retains at most 256 records and independently bounds
retained results to 256. IDs, hierarchy depth, encoded and decoded strings are
also bounded. Invalid IDs never fall back to the root. Base64 must decode to
valid, control-free UTF-8. Capability replies are constant and synchronous; they
do not wait for UI or WTA.

Only sequences that reach the terminal connection can be observed. A multiplexer
such as tmux or screen may filter them; transparent compatibility is not assumed.
SSH/WSL and multiplexer paths require their own live verification.

## Verification

Native parser/adapter/Core tests cover the wire format and state model.
`ProgramStatusPresentationTests` covers pane aggregation and precedence.
WTA's `program_status` tests cover qualified ownership, revisions/epochs, source
arbitration and completion versus liveness.

`test\e2e\tests\Feature.ProgramStatus.Tests.ps1` crosses real stdout, ConPTY,
Core, Control events and UI metadata. It requires explicitly selected Dev and
exact-source App/Control build hashes. Discovery or a fixture-based run is not
proof that a real agent implements the protocol: adoption must also be checked
with an authenticated supporting CLI, such as Codex 0.163.0 or Claude Code
2.1.295 or newer. Codex 0.163.0's
[upstream TUI](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/tui/src/terminal_program_status.rs)
emits Idle, Working, Blocked and Clear when stdout is a terminal. That version
does not emit `msg`. Its detailed iTerm2 notifications use a different protocol,
not OSC 7501. It cannot demonstrate Done, Error, blocking-kind or message
adoption; those contracts require other producers and dedicated terminal tests.

## Agent adoption evidence

The following are verified upstream releases and source, not successful live
runs against this terminal build. A producer need not use every optional field.

| Agent | Version checked | Verified behavior |
| --- | --- | --- |
| [Pi](https://github.com/earendil-works/pi/releases/tag/v1.1.0) | 1.1.0 | All five states and Clear; blocking kinds and `msg` for session names, dialogs, compaction and errors. |
| [Crush](https://github.com/charmbracelet/crush/releases/tag/v0.98.0) | 0.98.0 | All five states; permission, question and authentication blocking kinds with explanatory `msg`. |
| [Claude Code](https://github.com/anthropics/claude-code/releases/tag/v2.1.295) | 2.1.295 | Release notes confirm working, waiting for the user and completion reporting. They do not establish `msg`, blocking kinds or all five states. |

Pi's tagged [agent reporter](https://github.com/earendil-works/pi/blob/v1.1.0/packages/coding-agent/src/modes/interactive/program-status-reporter.ts)
and [wire encoder](https://github.com/earendil-works/pi/blob/v1.1.0/packages/tui/src/program-status.ts)
make it a useful candidate for real-message validation. Its
[terminal setup guide](https://github.com/earendil-works/pi/blob/v1.1.0/packages/coding-agent/docs/terminal-setup.md#program-status)
documents capability detection and the `PI_PROGRAM_STATUS=1|0` override.
Reports do not include prompts or model output.

Crush's tagged [status reducer](https://github.com/charmbracelet/crush/blob/v0.98.0/internal/ui/model/program_status.go)
supplies blocking messages; its
[turn handlers](https://github.com/charmbracelet/crush/blob/v0.98.0/internal/ui/model/ui.go)
set Done and Error. The pinned Bubble Tea/Ultraviolet dependencies use the
[ANSI encoder](https://github.com/charmbracelet/x/blob/ansi/v0.11.9/ansi/programstatus.go),
which writes the message under `msg`, not `message`.

OpenCode also has a [development implementation](https://github.com/anomalyco/opencode/blob/c7ddff776c59426e7dd63fe5a0d2a348864592ba/packages/tui/src/context/program-status.ts):
Idle, Working, Blocked and Clear, with per-session records, titles and messages.
Do not treat that source as proof of shipped support: the implementation was
temporarily reverted and subsequently restored on the development branch, and
has not been verified in the checked stable release, 1.19.4.
