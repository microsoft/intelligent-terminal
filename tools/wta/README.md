# WTA -- Windows Terminal Agent

A Rust TUI client and tmux-like CLI that connects AI agents to Windows Terminal.

Customization:
- See [CUSTOMIZATION.md](CUSTOMIZATION.md) for changing the agent model and runtime prompt.

## Quick Start

### Build

From the repository root:

```bash
cargo build --target x86_64-pc-windows-msvc --manifest-path tools/wta/Cargo.toml
```

The binary is output to
`tools/wta/target/x86_64-pc-windows-msvc/debug/wta.exe`. Always use the explicit
target in this repo: the package project prefers that output over the host-target
fallback.

### How WTA runs

WTA is normally launched **by Windows Terminal**, not by hand. WT spawns one
`wta-master` singleton (owns a lazily populated agent CLI pool) and one
`wta-helper` per agent pane (renders this TUI and speaks ACP to master over a
named pipe). Helpers selecting the same agent identity, source, and command
share one agent process. Bare `wta` with no subcommand and neither `--master`
nor `--connect-master` exits with an error. The existing per-tab assistant does
not have a standalone mode; the separate experimental Agent Center uses `wta ui`.

The default agent is Copilot; the agent and model come from Windows Terminal
settings (`acpAgent` / `acpModel`) and are passed through to master via `--agent`
/ `--agent-id` / `--acp-model`.

When the agent pane is connected to Windows Terminal, the agent-facing contract is
the local `wta` CLI: the agent shells out to commands like `wta active-pane --json`,
`wta list-panes --json`, `wta capture-pane --json`, and
`wta resolve-command <name> --cwd <active-pane-cwd> --json`. Terminal-control commands talk to Windows
Terminal over the COM protocol; `resolve-command` inspects the user's real,
shell-context-selected sources (active working directory, host PATH and, for
PowerShell, the profile-loaded command environment).

Autofix sends the failing command's context without pre-querying similar command
names. Its prompt advertises `wta resolve-command` for agent-initiated diagnosis,
using the failing pane's shell and working directory. Command enumeration is
uncached and runs only when requested; there is no background refresh or
startup/tab-selection prewarming. Query failures or unsupported shell contexts
are not evidence that a command is missing. The prompt directs agents to propose
obvious typos in familiar commands (such as `gti status` -> `git status`) without
lookup, while using local evidence for unfamiliar commands or ambiguous corrections.

### Integrated Work demo

The eight scenes in [`doc/user-story.txt`](../../doc/user-story.txt) use an
isolated demo data source in **the existing Agent Center UI**: its global
conversation, Work list, Work conversations, composer and details. Related Work
creation, decisions and budgets update the same Work list. There is no separate
storyboard renderer or second shell panel underneath unrelated real tasks.

The native recording launcher opens a new Dev window with demo mode scoped to
that window. It does not replace existing windows, edit settings, connect to the
production Agent Center authority, invoke models, or modify real Work records.

```powershell
# From the repository root, with the matching wta.exe deployed to Dev:
.\build\scripts\Start-IntelligentTerminalWorkDemo.ps1
```

The explicit per-window `INTELLIGENT_TERMINAL_WORK_DEMO_STATE` environment
variable selects the isolated state directory for the native `wta ui` Console.
Without that variable, `wta ui` retains its normal production-service behavior.
For terminal-only use, `wta --language en-US demo --reset` opens the same Agent
Center demo UI, but it does not replace the native Console in a surrounding
window; use the launcher for recording.

The story content and accepted phrases are deliberately English fixtures;
the shared UI chrome is localized. The opt-in Work Board also accepts
user-entered goals and completion criteria. A persistent demo disclosure identifies sample
evidence and simulated token usage. This demonstrates the integrated Work
experience, **not** autonomous agent execution, real token accounting,
production cross-work coordination or WorkExecutor acceptance.

Demo content is centered in a column no wider than 108 characters. Chat replies
are brief; **F5** opens compact evidence, schema exchange, budget and lineage.
At approximately 100x36, recorded checks and the labelled schema handoff fit in
one view. **F7** expands details to include full evidence summaries and raw
request/response JSON; press it again to return to the compact view.
The Work overview uses compact two-column cards at 80x24 and wider, retaining
all three Works and their six fields. Smaller views scroll. These presentation
changes apply only to demo mode, not the production Console.

The title distinguishes **Work chat**, **Evidence**, and the owner of an open
action panel. Decision and token-cap menus replace the evidence view rather
than covering its text; their footer is **Up/Down Select | Enter Confirm |
Esc Cancel**. Esc restores the same view, reading position and unsent draft.
The F6 hint follows its current action (Decision, Set cap, Add budget or Actions).
After resuming, the hint suggests checking F5 evidence first; it does not gate
the next prompt.

Start with the four selectable, simulated historical session tabs. These are
interactive views inside the native Console, not real provider processes or
four new shell panes. **Left/Right** changes the selected conversation.
**F1** switches from that old session-centered experience to Work.

The recording journey uses ordinary navigation, two prompts and explicit human
decisions. It does not require `Show ...` director commands:

| Scene | Input | Observable result |
| --- | --- | --- |
| 1 | Left/Right across the four historical session tabs | Distinct Fix bug, Code review, Migration and Research conversations demonstrate the old context-finding problem |
| 2 | `Continue fixing issue #4821` | Open the existing Work, with repository, branch, failure and next step |
| 3 | F5 on the resumed Work | Three completed checks, failed compatibility, documentation and PR pending, with sample commands, exit codes and evidence provenance |
| 4 | `Should we migrate to API v2?` | Offer related Work without creating it yet |
| 4, confirm | F4, then Enter to create related Work | Create one independent related Work; automatically record the structured schema exchange |
| 5 | Inspect the related Work's F5 details | Schema 2.3, three breaking changes, compatibility notes appear automatically; no human handoff |
| 6 | Wait for the proactive attention banner; F6, Enter chooses B | The issue Work requests a human decision after 10 seconds; choosing the recommended fix retains failed evidence until reverified |
| 7, configure | After B, press F6 to choose a migration token cap, then Enter | Presets are 10K / 20K / 30K; 20K is initially selected. Esc or Cancel makes no change. A new demo consumes nothing until the cap is confirmed |
| 7, observe | Observe the related Work after confirming the cap | Simulated usage increases by 5,000 every 3 seconds, pausing exactly at the chosen cap with findings and intent preserved |
| 8 | F2 | Open the same Work list with three selectable Works, status, progress, blockers, decisions, deliverables and pending acceptance |

`Add 10K budget` resumes the same simulated migration identity with the same
findings and raises its current cap by 10,000. F4/F6 also exposes **Set migration
token cap** after the related Work exists. Reconfiguration preserves usage,
rejects a cap below recorded usage, and pauses immediately if the new cap equals
usage. Raising a cap can resume timed consumption; adding 10K after the recorded
pause retains the completed demo phase rather than immediately exhausting again.
The equivalent explicit phrases are `Set token cap to 10K`, `Set token cap to 20K`
and `Set token cap to 30K`.

`Cancel related work` declines a pending branch. Unknown phrases and
out-of-order actions fail visibly without changing saved state; repeated branch,
decision and budget actions do not duplicate their effects.
Automatic attention and consumption are durable system events, not manufactured
user prompts. They preserve the selected view and any draft. Reopening does not
repeat completed events, and adding budget does not immediately exhaust it again.
Older saved demonstrations retain their previous preset-cap/clock semantics
and history; they are not reset or silently converted to require cap setup.
Fresh natural demos record an explicit `capSetupEnabled` event, followed by a
human request and `tokenCapSet` event on confirmation. Legacy `Show ...` commands are
compatibility navigation, not triggers for attention or token consumption.

**F1** opens the global conversation, **F2** opens the Work list, and
**Up/Down + Enter** selects a Work without opening another shell tab.
**F5** opens details. **PgUp/PgDn** scroll the current view.
**Ctrl+Q** or `/quit` closes only this demo.
Reopen without `--reset` to continue the saved story. `/reset` followed by
`/reset confirm` resets only the demo after confirmation; `/help` lists inputs.
The default store is `work-story-demo` beneath the shared package-private runtime
state root. `--state-dir <directory>` selects an isolated demonstration/test
directory containing `work-story.sqlite3` and its writer lock. Only one UI may
write a store at a time. A corrupt snapshot fails explicitly rather than silently
starting over.

#### Work-definition presentation

For the continuous release-intake story, enter `Open release journey` in a
**fresh isolated natural demo**. Keep the main conversation and release Work
side by side (at least 110x32). Tab suggests each next request: express the
release goal, confirm the legacy contract and both test gates, then approve
creation and execution. No Work or executor binding exists before confirmation.
The two execution tasks retain their bindings through a repair; the coordinating
goal has no executor binding. F1 returns to the main conversation, F2 shows the
same goal's overview.

This English fixture scripts coordination and agent bindings; it does not call
ACP providers. Only after approval does a bounded local Node worker write fixed
fixtures under the isolated store's `release-checks` directory and execute
unchanged unit and compatibility tests. Initially unit tests pass and the
double-quote compatibility test fails. A separate repair approval authorizes the
fixed local implementation correction and reruns **both** checks. Exact stdout,
stderr and exit codes persist alongside earlier failures. Both passing gates
mean **Ready for release approval**, never published or human-accepted. Worker
errors are explicit; reopening interrupted execution never automatically retries.
No user repository files or live agent sessions are modified.

While the release is executing, request `While that runs, investigate the startup
warning. Report the cause and a safe fix; don't change files.` and confirm with
`Start the separate diagnosis Work.`. The same view expands into two independent
goal cards; the diagnosis is not another release subtask or completion gate.
Its isolated Node worker reads a fixed deprecated-setting fixture and returns an
actual diagnosis report. It receives only its own goal, read-only instruction and
fixture context, not the release session history. Release bindings and instructions survive
unchanged. Diagnosis completion does not advance release checks, and a release
repair or worker failure does not reset the diagnosis. F2 shows both goals and
their next human action. These are scoped local demo workers, not a claim that
production ACP context isolation or automatic model selection has been implemented.
For recording, `Open parallel Work journey` starts the same flow with an extended
initial compatibility-check delay, so both independent workers can visibly overlap
while the user confirms the second goal. It does not change check outcomes or
the existing `Open release journey` pacing.

Ask `What needs my attention across my Work?` after creating a Work to read each
goal's saved status and next action, plus the release-check count, through the coordinator.
The question is recorded only in the coordination conversation; it does not
change Work state, executor bindings, instructions or worker requests. Reopening
the demo reconstructs the answer from the state at that conversation turn.

For a goal-first portfolio, enter `Open Work board` from F1 in a **fresh isolated
demo**. This native English presentation mode shows independent outcome cards
instead of session names: each Work has a goal, its own Planned / Running /
Blocked / Done status, completion-criteria progress, and a blocker or next step.
Three seeded examples show a blocked release fix, a running API assessment,
and a completed onboarding guide. These outcomes are fixtures, not live work.

Press **N** to create a Work with a required **Goal** and **Done when** condition.
Use Tab to change fields, Enter to advance/create, and Esc to cancel. User-entered
briefs are persisted in the isolated SQLite store and replay-validated; they are
not limited to the seeded goals. New Work starts **Planned**, with no executor
binding. **S** explicitly starts the selected Planned Work's simulated execution;
it does not fabricate completion evidence or launch a provider. Starting a
Running, Blocked or Done Work is rejected. Arrow keys select a card, **Enter**
opens its criteria and Work memory, **F2** returns to all Work, and **F1** opens
the main-agent conversation (`new work` opens the same brief form). Opening a
card never restarts execution. Reopening the demo restores the same saved Works.

At 150x42 the three seeded Works and a newly created fourth Work are visible
together. Narrower windows use one column and selection-based paging. The board
requires at least 76x28 and is deliberately separate from the existing single-goal
graph and conversation demos; use a new state directory to switch stories.

For the multi-Work story, enter `Open Work graph` in a **fresh isolated natural
demo**. The dark three-column view keeps saved relationships, the Main agent
conversation and the compatibility executor visible together. It requires a
110-column by 30-row terminal. This is a scripted English presentation fixture,
not a new implementation of the production Work protocol.

In F1 (Main agent), follow these exact prompts; Tab suggests the next one:

1. `Plan the fix. Require implementation and compatibility. High priority, 30K each.`
2. `Start the required work.`
3. `Continue yesterday's fix.`
4. `Where are we on the fix?`
5. `Investigate the startup warning. Related work, Low priority, 5K cap.`
6. `Summarize progress, decisions and spend.`

For the decision-support conversation, F1 also accepts:

- `Has compatibility been verified? Show evidence and next steps.`
- `Show the compatibility evidence.`

These read-only queries distinguish missing results from recorded passing and
failing checks, cite the stored evidence IDs, explain the limited test scope and
recommend the next check. They do not execute a fresh verification, inject an
executor prompt or grant acceptance. Responses and their evidence remain
available after SQLite reopen.

For the decision-to-execution handoff, F1 additionally accepts:

- `Record the decision: preserve legacy double quotes. Send it to compatibility and run the check.`
- `Show decision handoff.`

The first command requires the original failed compatibility result and its
running Work binding. It records one decision, then sends that decision to a
bounded **local Node.js check worker** in a unique directory beneath the
isolated demo state. This worker validates the decision and target, reports
delivery and acknowledgement, and actually runs the embedded, read-only
`node --test --test-reporter=tap test-compatibility.cjs` fixture. The unchanged single-quote
implementation still fails the decided double-quote contract. The point is a
completed handoff and fresh evidence, **not** a fabricated passing gate.

Receipt stages are paced for presentation. Each receipt is persisted with the
decision, Work and binding identity; the final receipt retains captured test
output. A duplicate command is rejected without launching another check.
Status queries, event replay and inspection do not execute the worker.
Reopening completed work retains the result; reopening an interrupted handoff
records a visible failure rather than assuming completion or silently retrying.
The worker has a 30-second limit and its test subprocess has a 10-second limit.
Node launch, protocol and persistence failures are surfaced explicitly.
The worker uses no model/provider and changes no repository files. It is not
the production ACP executor or an implementation of the complete Work protocol.

The normal Rust suite covers receipt ordering, routing, duplicate rejection,
replay, scoped evidence, separate drafts and rendered chat histories. To also
exercise the real local process and SQLite reopen, with Node.js installed, run:

```powershell
cargo test --target x86_64-pc-windows-msvc --manifest-path tools\wta\Cargo.toml graph_local_worker_delivers_acknowledges_checks_and_persists_actual_output -- --ignored
```

The goal persists two **REQUIRED** Works. The additional investigation is
**RELATED**, never a prerequisite. Each executor has its own workspace,
binding, criterion, priority, cap, usage and conversation. The two-slot
scripted scheduler admits High before Low; it does not preempt active work.
Every three seconds active High work consumes 500 simulated tokens and Low
work consumes 1,000. Implementation finishes after three checkpoints with a
recorded passing unit check. Compatibility reports a failing check and
continues investigating. The Low investigation pauses at exactly 5,000;
Compatibility continues within its separate 30,000 allocation.

Main-agent progress queries, resume and audit read recorded Work state without
sending a management prompt to an executor or restarting its runtime. New
related work receives the goal specification, not another executor's transcript.
F4 deliberately switches to a **direct executor conversation**:
`What is blocking compatibility?` adds a turn only to that runtime. F1 returns
to management. Both drafts are kept separate. SQLite reopen retains Work
relationships, bindings, conversations, evidence, allocations and usage;
closed-UI wall-clock time is not simulated.

Audit responses cite persisted event numbers and evidence IDs. Neither a
finished response nor the related investigation satisfies the required
compatibility gate. Agent replies, scheduler activity and token accounting
remain fixtures; the explicit decision handoff is the sole local-process
exception and runs only its embedded check. There are **no live provider calls,
shared-account quota controls, quota-refresh guarantees or real project
workspace execution**. Management itself is
not claimed to be free; this demonstrates context separation and per-Work
allocation, not provider billing enforcement.

In a **fresh isolated demo**, press F1 and enter `Open Work model` to opt into
the dark, conversation-first single-Work presentation. Existing eight-scene recordings and saved
stories are not converted. This is an English presentation fixture, not a new
provider or a complete Work protocol implementation.

The same `Fix Issue #4821` owns its workspace, simulated Copilot runtime,
priority, acceptance criteria, evidence, execution binding and token budget.
F1 opens the **Main agent** conversation (coordination, not code execution).
Enter `Continue fixing issue #4821`, confirm a 10K, 20K or 30K cap with F6,
then enter `Investigate compatibility` to delegate to the Work's runtime.
F4 opens the **Work agent** conversation with its runtime status and persistent
Work context visible beside it. Enter `Keep the legacy API unchanged.` to save
an execution instruction, or `Why are you paused?` after the cap is reached.
The input header always names the recipient; requests sent to the wrong
conversation are explicitly rejected. Each conversation retains its own draft
and history, including after reopening the persisted demo.

F5 shows criterion-linked sample evidence. On the Main agent surface,
`Review progress` checks outstanding acceptance criteria; `Add 10K budget`
authorizes continued execution after a pause. The Main agent also receives
the runtime's budget-blocked notification without copying its conversation.
Tab suggests a request for the current recipient. F8 delegates the simulated
compatibility investigation via Main agent. Usage advances by 5K
every three seconds and pauses exactly at the cap. F7 adds 10K and continues
the same Work without resetting usage, evidence or its definition. Opening or
resuming a paused Work does not itself restart execution. Priority and runtime
are fixture metadata, not a demonstrated live scheduler or launched provider.
Both agents' replies and runtime activities are explicitly scripted; this is
an interactive routing/state demonstration, not a live model conversation.
No files in the example workspace are executed or modified.

For the merge-readiness conflict, use `Open merge story` instead in a fresh
isolated demo. It seeds a separate recorded fixture with three passing Node
unit tests and a failed legacy quoting gate. In F1, ask
`Is yesterday's fix ready to merge?`. The Main agent correlates both results
with the Work and proposes a compatibility investigation using its existing
runtime, High priority and the displayed 20K cap; merely asking does not run it.
`Proceed. Keep the legacy API unchanged.` explicitly approves that initial
proposal, records the instruction, confirms its cap and delegates execution.
Repeated approval cannot add budget or restart the runtime.
In F4, ask `Why did compatibility fail?` to discuss the attached failure with
the Work agent. `Merge: BLOCKED` remains visible even while the runtime runs:
neither approval nor activity is replacement evidence. Both conversations
remain scripted; the separate before-comparison uses real CLI sessions on a
synthetic project and must not be presented as production repository evidence.

```powershell
# Read-only, also while the demo UI is open; does not initialize absent state.
wta demo --inspect
# Use the same explicit directory for both the UI and its inspection.
wta demo --state-dir C:\Demo\WorkStory --inspect
```

`Feature.WorkStoryDemo` in ItE2E verifies the journey through the native Dev
Agent Center Console, its Work list and conversations, and persisted demo data.
A shell-tab storyboard run does not qualify that boundary. The suite records
build provenance and HWND-local UI Automation evidence. Screenshots can contain
stale surfaces on a non-interactive desktop; they are not, by themselves, proof
of painted pixels. This coverage is separate from real-model product qualification.

To record a fresh journey, use FFmpeg and its sibling `ffprobe.exe`:

```powershell
.\build\scripts\Record-IntelligentTerminalWorkDemo.ps1 `
    -FfmpegPath C:\Tools\ffmpeg\bin\ffmpeg.exe `
    -OutputDirectory C:\Recordings\WorkDemo `
    -ExpectedWtaSha256 (Get-FileHash .\tools\wta\target\x86_64-pc-windows-msvc\debug\wta.exe).Hash
```

The output directory must be new. Recording captures only the owned native
window, never the desktop; it produces an MP4, English scene captions, timed
chapter metadata and UI/state evidence. It does not generate substitute frames
from text. The default continuously captures real `PrintWindow` frames at up to
8 fps and encodes a 24 fps MP4. Input is directed only to the newly owned demo
Console, without global keyboard injection. `-CaptureBackend GraphicsCapture`
uses Windows Graphics Capture when a D3D11 device is available; an interactive,
unlocked desktop is required for that path. A successful encoder exit alone is
not visual proof: some environments return black or stale capture surfaces.
`Test-IntelligentTerminalWorkDemoVideo.ps1` verifies decoded video frames with
offline Windows OCR so blank, stale or unreadable footage is not qualified by
UI Automation alone.

### Experimental Agent Center

Agent Center is an opt-in implementation of the
[work-mode product experiment](../../doc/specs/agent-center-product.md).
It does not replace the default per-tab assistant. Its authoritative message
contract is [Collaboration Protocol v1](../../doc/specs/agent-center-protocol.md);
the target specification is not a statement of production readiness.

Before first launch, configure a real ACP server. `adapters.json` contains
explicit executable/argument arrays rather than an inferred interactive CLI
command. Replace the sample path below with your server and its documented
stdio arguments. Terminal's built-in resolved commands are defined in
[`AcpModelUtils.h`](../../src/cascadia/inc/AcpModelUtils.h).

```json
{
  "capabilities": [
    {
      "id": "local-agent",
      "adapter": {
        "kind": "ACP",
        "approvedModelDestination": "Approved provider/account or endpoint",
        "executable": "C:\\Tools\\acp-server.exe",
        "args": []
      }
    }
  ]
}
```

```powershell
wta center configure --input-json .\adapters.json
```

This validates and installs configuration while the authority is stopped.
`approvedModelDestination` must name the destination you have approved for
conversation and authorized work data. It records human approval, not verified endpoint enforcement or
network isolation. Optional `model` and `environment` fields belong inside
`adapter`; avoid putting credentials in this file.
Configuration is loaded on service startup and never
launches a model by itself. Each configured ID can coordinate, produce results,
and review; the separately registered `native-check` capability runs declared
local command checks. Without an adapter configuration, agent work is explicitly
unavailable. The ACP server must support HTTP MCP and the work-tool contract.

With one approved adapter, startup configures the global conversation assistant
without creating an execution project or launching a model. With multiple
adapters, set top-level `conversationCapabilityId` to the approved default.
Optional top-level `conversationLimits` uses the same complete limits shape
shown below. Defaults are 1 execution slot, 8 execution attempts, 16 evaluation
attempts, 64 coordination turns per conversation, 3 context rounds, 600 seconds
per execution and 120 seconds per coordination turn. These conversation defaults
do not themselves authorize work execution. Changing assistant approval waits
for existing global invocations to settle.

Ordinary chat does not require `/project use`, project IDs, or an execution
project. Existing Terminal provider settings are not silently imported as new
model-destination approval; initial assistant configuration remains a separate
explicit prerequisite.

For a real, non-demo Dev window, build and deploy the matching WTA binary, then
run from the repository root:

```powershell
.\build\scripts\Start-IntelligentTerminalAgentCenter.ps1 -Package Dev
```

The launcher reuses the Dev package's explicitly approved `adapters.json`,
checks the deployed binary against the Windows-target build, and opens the
package-scoped native Console with the demo selector removed. It does not
preload Work, generate replies, change settings, or restart existing hosts.
A reused Dev host must already have Agent Center enabled. The default isolated,
persistent store is `IntelligentTerminal\agent-center-live` under the package's
LocalState. Use `-StateDirectory <absolute-directory>` to select another store
and `-AdapterConfiguration <approved-json>` to select an explicit configuration;
an existing different configuration is never overwritten implicitly.
`-ConfigureOnly` returns the launch metadata without opening a window.

`INTELLIGENT_TERMINAL_AGENT_CENTER_STATE` selects that same absolute state
directory for `center configure`, `center serve`, CLI commands and `ui`.
Set it identically in clients and their authority. Without it, the existing
runtime-path default is unchanged; empty and relative overrides fail explicitly.
Closing a Console is not a request to stop its Work executors.

Agent Center and per-tab agent panes share the conversation renderer, input-box
presentation, card chrome/buttons, activity animation, and `theme` styles.
Both follow the terminal color scheme rather than maintaining separate chat
palettes. Agent Center adapts its durable conversation records into the shared
presentation; it does not create a helper ACP session or reuse the helper's
mutable `App` state. Work details, exact approval previews, delivery inspection,
and their backend authorization remain Agent Center responsibilities.

For a new project, an unambiguous saved `workspace.code_root` preference can
supply the parent directory. The assistant suggests a new direct child and
offers the exact path for approval instead of asking you to repeat the parent.
The current active project is not silently reused for unrelated new work.
Approval to create the directory and initialize its local Git repository is
still required. An existing target is never overwritten or automatically
adopted; request a different location in chat to replace the proposal.

`project.configure` retains its existing-directory behavior by default.
With `createDirectory:true`, it returns a pending operation and reserved
project ID. The project is registered only after the directory and initial
Git commit are ready; the original conversation is notified to continue its
work draft. The pending submission updates the Console without starting an
assistant turn. Only the terminal creation receipt wakes the assistant with
fresh project access, avoiding a redundant streaming turn that cannot yet
create the work. Creation failures and uncertain crash outcomes remain recorded;
partially created directories are retained for reconciliation, not deleted
or automatically retried.

Global conversation turns do not wait for execution work. New work owns a
persistent executing agent session; the global master cannot execute shell or
filesystem work. Status comes from recorded progress and
bounded queries. A new human message requests a scoped stop of the obsolete
global turn, preserves committed questions/proposals and conversation history,
then dispatches the latest captured input after that provider settles. It does
not cancel work executors, legacy workers/coordinators, or another window's conversation.
Multiple arrivals during settlement coalesce; an intentional conversational
supersession is not a protocol failure. Provider startup and model latency
still apply, but the previous turn's full deadline is not the normal wait.

For WorkExecutor work, `work.get.executionSummary` exposes up to three recorded
executor replies, newest first, capped at 4,096 Unicode characters each with
explicit truncation and message/turn status. The projection checks Work, turn
and invocation ownership and does not submit executor input or open another
session. The global assistant can therefore answer progress questions from
actual recorded execution without interrupting its workers. These are
provider-reported statements, not host-verified test outcomes or final human
acceptance; missing, streaming or truncated evidence must be described as such.
`continuation` remains the source of service-observed activity. An idle executor
or completed reply still leaves its Work Active.

The opt-in local real-Copilot qualification is
`test\e2e\local\Invoke-RealAgentCenterValidation.ps1` (see the E2E README).
It uses deliberately unimplemented source plus independent tests, not a canned
repair: the coordinator creates two Works and real executors write the code and
review report. Model output is never substituted for an independent test run.

Coordinator ACP permission requests admit only their bound work tools, not
provider-owned shell execution or delegation. This role guard is not an OS
sandbox or a guarantee about provider tools that never request permission.
Intake replies advertise their exact outer `IntakeRequest` guard and return
an entity reference for finishing a waiting turn. Human action proposals
advertise the typed payload and guards for each supported nested operation,
so preparing a project does not require guessing a schema or searching history.

Execution project setup approves a real directory and finite planning/execution
allowances. Global conversation can propose it with a readable human confirmation,
then prepare a work brief and a separate captured Start approval. The precise
administrative CLI remains available. For example, `project.json` can contain:

```json
{
  "name": "Local experiment",
  "root": "C:\\Source\\project",
  "coordinatorCapabilityId": "local-agent",
  "workerCapabilityId": "local-agent",
  "checkCapabilityId": "native-check",
  "limits": {
    "concurrency": 2,
    "executionAttempts": 8,
    "evaluationAttempts": 8,
    "coordinationTurns": 12,
    "contextRounds": 4,
    "executionSeconds": 600,
    "coordinationSeconds": 180
  }
}
```

```powershell
wta project configure --input-json .\project.json --confirm --json
```

Use the returned project ID with `/project use <id>`. Project planning approval
is distinct from work execution approval: `/work new "<goal>"` prepares an
intake/brief; `/work start` previews the exact grant and asks for confirmation.
Changing a work or accepting a result must name its current recorded versions.

`/work revise "<change>"` asks the coordinator for a proposal without applying
it. `/work apply <proposal-id>` previews the current grant and captures the
exact work/proposal versions for a final `Ctrl+Enter` confirmation. The CLI
equivalent is `wta work apply <proposal-id> --work <work-id> --confirm`.
Applying a revision preserves grant limits and consumed usage, revokes old
dispatch authority, and waits for affected execution to settle and release
before replanning. Old results and delivery candidates remain historical, not
current acceptance targets.

```powershell
wta ui
wta ui --work <work-id>
wta work list --json
wta work show <work-id> --json
wta task list --work <work-id> --json
wta result show <result-id> --json
wta work events <work-id> --after <cursor> --jsonl
```

`wta ui` and structured clients connect to the same local authority. The first
connection starts `wta center serve` in an independent process; this does not
itself approve a project or start model work. Closing a Console does not stop
the authority. If a launcher job disallows independent process creation, startup
fails explicitly: run `wta center serve` outside that job rather than tying
background work to the Console's lifetime.

State is stored in `agent-center\work.db` below the shared application-state
root, with immutable artifacts and managed workspaces alongside it. A private
current-user named pipe and an exclusive state-root lock protect the local
authority; SQLite uses WAL and durable commits. Diagnostics use the
`wta-center-service` and `wta-center-ui` log streams. This prototype does not
qualify provider-owned tools as an operating-system isolation boundary.

Agent Center remembers durable, non-sensitive interaction/workflow preferences
from current human input during ordinary conversation; natural-language
corrections and forgetting need no new command or UI. Preferences are `Preference`
records in the existing `work.db`, with no migration, new database or dependency.
They are not per-tab WTA memory, a vector database, synchronization or history
mining. Automatic extraction uses the conversational coordinator, not an extra
model or background scan; it is a policy, not a hard classifier guarantee.
Credentials, personal/sensitive/third-party information, task progress,
transient requests and agent/tool/web output must not become preferences.

User preferences apply across Agent Center; project preferences apply only to
their authorized project. Ambiguous scope is clarified or skipped, never
broadened from a task. Current instructions take precedence; recalled content
does not grant permission or change an approved task contract. Coordinators
receive bounded applicable preferences and can page through `memory_list`,
including content-free tombstones for version checks/restoration. Each scope
is capped at 512 records. Only conversational intake coordinators receive
`memory_store`/`memory_forget`.
Both mutation MCP schemas require `sourceMessageId` referencing the invocation's
latest captured human intake; direct human protocol callers may omit it.
Source validation cannot prove semantic truth. Workers receive only
relevant constraints through approved task contracts, not full memory.

The assistant reports remembered/forgotten only after a successful tool response
and exposes errors. Forgetting clears live preference content and fences memory
stores from already-captured invocations, not further forgets. All requested
deletions can complete with exact versions and latest-source checks before the
turn finishes; storing again requires fresh human intake. Old snapshots,
chat, command receipts and backups may retain historical content, so this is not
full erasure. A fresh later human request can restore a forgotten key at its
current version; replaying the original or forgetting message cannot.

To try the native window host, launch a new Intelligent Terminal process with
`INTELLIGENT_TERMINAL_AGENT_CENTER=1` in its environment. It creates one direct
`wta ui` terminal surface, not a PowerShell/cmd process or a per-tab helper.
The initial shell area is empty. `Ctrl+Shift+G` focuses the Console;
`Ctrl+Shift+H` shows/hides existing shell content without closing its tabs.
The configurable `focusAgentConsole` action is also available. Explicit user
key bindings, including unbindings, take precedence over these defaults.

The experimental native layout does not replay or overwrite legacy saved
workspace layouts and disables tab dragging. Native policy configuration
currently blocks this independent entry rather than bypassing the agent
allowlist. Shell slash-command presentation adapters, external publication,
and production recovery/isolation qualification are not implied by enabling
the experiment. Unsupported commands report a capability error; they do not
claim that a shell, publication, or repair completed.

Structured output distinguishes `ok` (exit 0), `pending` (2), `needs_input`
(3), `conflict` (4), `unsupported` (5), and `error` (1). A recorded task result
is not an accepted result, and a successful provider turn is not delivery.
Acceptance requires the exact current candidate and its evidence. Preserve a
mutation's command ID and payload when retrying transport delivery; changing
the intention requires a new command ID.

Initial plans must preserve approved criterion descriptions and evidence rules,
not just criterion IDs. Delivery acceptance verifies captured manifests and
content again, including required check/review evidence; changing bytes at the
same locator does not retain a valid acceptance proof. Coordinators can read
recorded progress bodies with `progress.get` and verified, bounded diagnostic
content with `artifact.read`, without asking the user to copy logs.

Console/CLI named-pipe reads retain partial frame state across service updates.
Split headers and payloads do not become invalid frames when notifications
arrive, and subscription events remain available while a request is incomplete.
The 1 MiB limit, malformed-frame rejection and bounded disconnect cleanup remain
unchanged.

The Console's ordinary work flow is keyboard-operated without copying internal
IDs or writing request JSON:

| Key / action | Behavior |
|---|---|
| Up/Down, Enter | Select and open a task from the default task list |
| F4 | Open task controls, including Continue task, explicit session recovery choices and Open in tab; routine controls are not expanded below task chat |
| F1 / Global conversation | Return directly to the existing global chat from Tasks or a task conversation, preserving chat history and drafts |
| N in Tasks | Compatibility shortcut for F1; it does not create a new conversation |
| F2 | Return to the task list; each task retains its own chat and draft |
| Esc in Tasks | Return to the already-open conversation, including the empty global chat on first launch |
| F5 | Open or close the selected work's read-only brief, recorded facts, progress, recovery details, locations and evidence |
| F6 | Review a single inline approval card; with multiple actions, Up/Down selects and Enter opens one |
| Continue task | Inspect current execution, attach to a live run or request safe continuation of this work; never create a duplicate task |
| Open in tab | Open `wta ui --work <id>` for the same service-owned work conversation; no additional provider or workspace writer |
| Review brief and approve start | Inspect goal, scope, criteria and requested execution limits, then explicitly confirm the captured start |
| Respond | Answer a service-owned question through typed fields/options rather than authoring JSON |
| Inspect / read evidence | Inspect the fixed delivery and bounded evidence without accepting it or taking write ownership |
| Accept the inspected delivery | Review and explicitly confirm acceptance of that candidate; pending settlement is not Completed |
| F12 | Toggle read-only diagnostic protocol details, including exact request identities and guards |

Within the action menu use Up/Down, Enter and Esc. Simple question forms support
strings, integers, booleans, enumerations and flat objects; Up/Down changes
fields, Left/Right changes choices, Enter advances, and Ctrl+Enter prepares the
answer for explicit confirmation. Unsupported schemas are reported without
submitting fabricated defaults. Provider/destination approval remains a separate
initial prerequisite; a real execution project can be proposed in global chat
and explicitly approved. Ordinary work does not launch or select internal agents
manually.

For Agent Center navigation diagnostics, set
`WTA_LOG=warn,agent_center::navigation=debug`. This records PageUp, PageDown and
F12 ingress/handling, modifiers, pending-command identity and scroll transitions;
it does not record typed characters or pasted text.

The persistent top navigation shows **F1 Global conversation** and **F2 Tasks**
in task and chat views. The default task list shows readable goals, observed status and recorded
activity. Opening a task shows its own persistent conversation rather than
retargeting a shared global chat. Normal task chat shows human and assistant
messages, not repeated internal event or recorded-fact cards. Those technical
records remain available in F5 details without pushing real replies out of
the chat viewport. The compact task heading appears once, followed by its
observed status and a next-action hint; unrelated tasks' global attention
counts are not repeated in the selected task. F4 retains routine task controls,
while required questions and actionable approval proposals remain inline.
Each client retains independent drafts and
reading positions per task. New goals and cross-task questions remain available
through global intake. Plans, protocol diagnostics and execution details are
optional, not prerequisites to viewing a historical task.

The task status distinguishes running, waiting for a response, paused, needing
recovery and unavailable sessions. A stored `Running` value past its deadline
is not proof of live execution. The Thinking indicator represents a real
conversation response, not proof that the whole work is advancing.
Task Thinking requires a matching, unexpired primary response reported by the
service; a historical `Streaming` message alone cannot activate it. Failed
stop/release recovery shows its actual error outside the scrollable history.
An accepted recovery request is not displayed as successfully resumed work.
Continuation receipts use the service's projected task state rather than a
generic request-success message; accepted or pending recovery can still mean
Needs recovery. Stale status is marked as last known.
Necessary questions, approvals, failures and unknown outcomes remain visible.

Continuing a task uses `work.continue`, not merely the legacy scheduler-only
`work.control` Resume flag. The service must settle stale execution before
starting another writer and restore the correct primary ACP session with a
fresh tool binding. If the provider cannot restore that session, it reports
the limitation; reconstructing from saved context is a separate explicit
choice, never a silent successful-resume fallback. Opening another tab or
reading the task does not implicitly resume it.

Newly created tasks use `WorkExecutor`. Work chat, follow-up questions, Continue
and another task tab share that work's executor and workspace. Busy inputs are
queued in order; the task composer shows the number still waiting. Each admitted
prompt consumes one approved execution attempt and has an execution-time limit.
Attaching to a live session does not consume an attempt or submit another prompt.
An idle executor shows Ready and retains its provider process, workspace
reservation and owned background processes, so replying does not kill a
development server. Pause, Cancel and authority shutdown settle its owned
process tree; simply closing a task view does not.

Historical tasks require an explicit **Connect to work agent** confirmation
(`work.claim_executor`). This fences the legacy scheduler and settles existing
writers before adopting a verified actual worker session, never the historical
coordinator. Claiming queues a continuation prompt; a successful request receipt
alone does not prove the session loaded. If no execution session can be restored,
starting a new one requires separate confirmation. Typing into an unclaimed
historical task opens that confirmation and preserves the unsent draft instead
of sending it to the old coordinator.

This iteration deliberately covers execution and recovery only. Formal delivery
acceptance for WorkExecutor returns `EXECUTOR_DELIVERY_UNAVAILABLE`; a normal
reply does not complete or accept the work. Legacy delivery/gate contracts remain
available for explicitly legacy work. `wta work resume` is still the legacy
scheduler control, not the new task-chat continuation or migration operation.

For legacy work only, a disconnected non-writing coordinator is recovered separately from worker
process settlement. When all worker invocations are released, the service can
verify the original invocation/provider digest, recover its session association,
revoke the old coordinator's tool authority, and load the original ACP session.
This does not certify that the old process tree ended: the historical record
keeps `processSettlement: Unknown` and the old journal is preserved. Live
coordinators and unsettled workers cannot use this path; provider/configuration
mismatches fail explicitly, without falling back to a new session.

Runtime observations retain their exact pending identity until acknowledged.
Timeout/cancellation recovery replays an interrupted report before sending the
terminal state, rather than leaving a finished invocation permanently Running.
ACP text notifications use a bounded reporting queue and coalesce adjacent
chunks without crossing turn/flush boundaries. Permission requests and MCP
calls no longer wait behind a database transaction for every streamed token;
queued text is flushed before terminal classification. Transactions write only
changed records, not the entire retained history.
Windows command checks resolve extensionless tools such as `npm` through PATH
and PATHEXT before launch (including the recipe's PATH override); arguments are
passed individually, not interpolated into shell commands. A missing executable
remains an explicit inconclusive check, never a pass.

Details appear beside the conversation when space permits and use the main
reading area otherwise. Closing them returns to the same work and draft.
Natural-language proposals appear as **Approval required** cards in the
conversation, not only under Attention. F6 opens the frozen review without
approving anything. With the card focused, Left/Right or Tab selects **Approve
and start** (or **Approve**), **Change requirements**, or **Not now**; Enter
selects that explicit button. F5 expands/collapses the authoritative details,
and PageUp/PageDown scrolls the preview. Ctrl+Enter also confirms the exact
captured action. Chat remains visible during normal review.

Change requirements returns to the composer with the proposal's work context
and the existing draft intact;
describe the requested revision there. Not now or Esc defers this proposal
version locally without cancelling work; F4 can reopen it, and a revised version
appears again. Neither option submits approval. Incoming proposals do not take
input focus, and Enter in the composer remains chat submission. Cards retain
the same scope/version checks and delivery inspection requirement. Legacy
slash-command/dashboard confirmations still require Ctrl+Enter. The navigation
rail omits repeated full paths, but actual locations and evidence remain
accessible. A concurrency version is not presented as a report edition, and
an unknown execution directory is not replaced by the project source root.

Chat uses readable work/project labels where a context hint or action needs them;
the task status and next action stay compact, with delivery and technical details
available on request.
Actual paths, evidence and failures
remain available; user content is not rewritten merely because it contains an
ID. Slash commands remain an expert/automation path. Full J9 global reasoning
and confirmed alternative-plan replacement remain a separate implementation
increment, not a claim made by these controls.

UI rendering is paced rather than repeated for every event, and subscription
requests run off the input loop. Event pressure is bounded by both frame count
and bytes, with request/recovery capacity retained and terminal causes recorded
outside a full event queue. A stale or disconnected Console retains drafts and
blocks new mutations while replacing the connection and obtaining fresh
same-store snapshots. Automatic recovery is bounded to three attempts; F4's
refresh action explicitly rearms it. Recovery never silently repeats mutations
or turns an unknown outcome into success. Explicit reconciliation retains the
original command identity.

Pre-work coordination directories, like managed workspace roots, are resolved
before launching a provider. Windows process startup still limits the current
directory length even with a verbatim path. For long directories WTA uses an
existing native short name only after verifying that it resolves to the exact
same directory. No data is moved and no junction or alternate execution root is
created. If the filesystem has no usable short name, startup fails explicitly
rather than running elsewhere. This also applies to native-check directories.

Confirmation previews have their own reading position, starting at the top
without changing the current work's activity position. Paging and window resizing
clamp the stored offset to the actual wrapped content, so the end cannot become
a blank confirmation body and PageUp responds immediately after repeated PageDown.
Cancelling a preview restores the prior activity position; confirming still sends
the exact frozen operation and guards. PageUp from the latest activity starts at
the displayed tail rather than jumping to the beginning.

Each Console work keeps its own draft caret and selection. Left/Right move by
Unicode grapheme; Ctrl+Left/Right use the helper's word-navigation rules.
Home/End move to the logical line boundaries; Ctrl+Home moves to the draft start.
Shift extends a selection, Ctrl+A selects the draft, and typing, Backspace,
Delete or bracketed paste replaces that selection. Shift+Enter inserts a newline.
Up/Down move through wrapped draft rows unless unmodified keys are choosing slash
completions; Tab still accepts a completion and moves the caret to its end.
The input viewport follows the actual caret, including after resize and work
switching. PageUp/PageDown and Ctrl+End remain activity-history controls.
Confirmation previews freeze draft edits, including paste; cancelling restores
the same caret and selection. An older response never clears a changed draft.

On Windows the Console owns a native input reader instead of Crossterm's
Windows key-event reader. It enables VT input before requesting bracketed paste,
so ConPTY preserves paste delimiters. UTF-16 surrogate pairs are assembled from
key-down records; ordinary key-up records cannot discard non-BMP characters.
Windows Alt-code text on Alt release is retained as well, including the legacy
ConPTY representation of non-BMP input.
A bracketed paste is delivered as one draft edit, with CRLF normalized to LF,
never as individual Enter shortcuts. Physical key modifiers and repeat counts
remain available to the editor. The reader is stopped and the prior input mode
restored when the Console exits; existing per-tab helper input is unchanged.
An incomplete paste is never submitted, and invalid UTF-16 or input exceeding
the 4 MiB paste limit fails explicitly instead of exposing a partial command.

The Console also requests Win32 keyboard packets (`CSI ? 9001 h`) from its
console host. VT input alone can reduce modified Enter to a bare CR/LF and
standalone Escape to an ambiguous sequence prefix. Lossless packets retain
virtual keys, modifiers and repeat counts, so Shift+Enter edits, Ctrl+Enter
confirms, and Escape cancels without waiting for another key or an escape timer.
Packet framing and paste framing have separate decoders and surrogate state;
older hosts may packet-encode paste text as virtual-key-zero records. Exit
disables the packet mode and restores the original input/output console modes.
This does not change ordinary per-tab helper input.

Command checks support File-only reports as well as code snapshots. Evaluation
pins both the exact producing dispatch inputs and the submitted outputs. A fresh
check directory combines their captured member paths, with current output files
superseding prior input files; differing same-layer collisions fail explicitly.
File captures use their basename; capture a Tree when a nested layout matters.
A complete submitted Code/Tree/GitCommit snapshot remains authoritative, so older
inputs cannot resurrect deleted files. Checks never fall back to mutable
workspace contents. Independently pinned File inputs remain available as
supplemental check scripts or data when input snapshots are replaced.
Read-only LocalCode integration can retain a single
unambiguous accepted input code snapshot, with provenance and content rechecked
at final acceptance; report-only delivery remains supported independently.

Failures before a check starts carry their exact bounded diagnostic and
evaluation/result identities into coordinator snapshots and rework records,
rather than masquerading as a process test failure or a missing model submission.
The coordinator can revise captures under the unchanged contract, or explicitly
retry evaluation after repairing a transient cause. Unchanged repeated failures
remain bounded and visible; generic evidence collection is not an automatic fix.

Plan tools advertise the exact, case-sensitive output categories: `File`,
`Tree`, `GitCommit`, `Report`, `Code`, and `Evidence`. Admission and the tool
schema share that vocabulary. Unknown categories return `INVALID_ARGUMENT`
with an indexed `fieldErrors` path and the allowed values, rather than claiming
that plan creation is unsupported. Corrected proposals use a new command ID;
rejected proposals do not admit tasks or change the work version. Code, Tree
and GitCommit outputs still require a concrete required check. Approved scope,
exclusions and criterion/evidence constraints are not relaxed.

Approved prose evidence rules are copied verbatim into the integration task's
`criteria[].evidenceRule` and remain in its immutable dispatch. Separately,
`requiredEvidence` names concrete required gates or captured output slots.
This permits ordinary-language work briefs without treating prose as a gate ID
or silently replacing the approved meaning. Explicit `command:<gate>`,
`artifact:<slot>`, and legacy bare ASCII references retain their exact binding;
existing reference-based contracts do not need the new optional field.

Before a worker acknowledges its dispatch, ACP permission permits only an
exact bound `task_acknowledge` call, including its current revision and
continuation ID. Copilot's bare `task_acknowledge` title with kind `other` and
known server-qualified spellings are supported; a title alone never grants
permission. The tool must be in the invocation's bindings, with a valid command
ID and closed acknowledgement arguments. Permission selects only `allow_once`
and does not itself acknowledge the task: the successful MCP receipt does.
Unrelated pre-acknowledgement tools and permissions after cancellation,
release, or the end of a running turn remain denied. Host-policy denial is
logged explicitly, rather than treated as evidence of a human rejection.
Continuation turns reset text chunk numbering for their new message part.

Work MCP initialization negotiates a supported protocol version rather than
closing the connection when a client offers a newer version. It preserves known
versions and otherwise offers `2025-06-18`, matching the existing session MCP
negotiation. The client decides whether it supports that response. Diagnostics
record the offered/selected versions and invocation ID, not authorization
headers; malformed version fields receive a JSON-RPC invalid-params error.
Notification responses now use the same bounded writer and explicit TCP send
shutdown as JSON responses. A real Windows queued-input close/reset regression
is covered; the exact trigger of the earlier intermittent HTTP test reset
remains unproven.

Managed Git worktrees are registered with `--no-checkout`, then populated from
within the worktree. This avoids Git for Windows' absolute `GIT_DIR` size limit
in its automatic checkout subprocess. WTA's Git commands use command-local
`core.longpaths=true`; no user/global Git setting changes or relocation outside
the managed state root are involved. Failed checkout explicitly reports the
failure and cleanup outcome. Cleanup removes incomplete worktree registration,
not source-repository branch references; a failed cleanup can leave state for
reconciliation. External provider Git commands retain their own
configuration; this is not a claim of unlimited Windows/Git path support.

Immutable capture publication retries only the atomic rename on Windows access,
sharing, or lock errors, with at most six attempts and 620 ms of scheduled delay.
It never recopies changing source data, replaces an existing capture, or falls
back to mutable output. Persistent failure remains explicit. Acceptance failures
identify the affected artifact reference. This handles reproduced transient
delete-sharing locks; it does not identify the holder of the lock in the earlier
verification failure.

`--input-json <file>` reads a complete protocol request; `--input-json -` reads
it from CLI stdin. The Console accepts files, not stdin. Command IDs, guards,
approval references, and payloads from complete requests are preserved, and
conflicting command-line arguments are rejected. Configuration and transfer
commands also accept documented params-only files; `--params-json` is the
explicit inline-JSON form.

Workspace inspection is available before a delivery candidate exists. Guided
transfers use the selected work, or an explicit `--work <work-id>`:

```text
/workspace takeover
/workspace handback --summary "Updated input" --resume-affected false
/inbox
/inbox --work <work-id>
```

The Console previews the exact workspace version and affected contracts;
**Ctrl+Enter** confirms and **Esc** cancels without clearing drafts. Handback
requires a summary and an explicit `--resume-affected true|false`; even `true`
does not override Work Hold. Structured CLI guided transfers require
`--work <work-id> --confirm`. `/inbox` is global; the optional work filter does
not replace the Console's global attention set.

This experiment supports command gates and internal ACP review, not human
checkpoint gates. Spec revisions stay within the existing project,
destination, and authority; permission/budget expansion, removal of executed
tasks, cross-work dependency edits, external publication, and automated repair
are not implemented. Manual takeover/handback and same-grant revisions have
controller coverage; the deterministic real-process conformance journey
requests ACP permission before invoking MCP (including initial and continuation
acknowledgements), rejects unrelated pre-acknowledgement permission probes, and
covers two independent works, context continuation, failure/rework, immutable
captures, and accepted local delivery. That evidence is not a live-model
product-acceptance run.

The [next verification plan](../../doc/specs/agent-center-verification-plan.md)
separates defect regressions, packaged Console behavior, and the real-model
two-work journey. Manual handback conservatively invalidates consumers in the
edited workspace; it does not yet preserve unaffected same-workspace results
at fine granularity. The proposed `$` ordinary-shell shortcut remains deferred.

The packaged app registers `wta.exe` as an App Execution Alias. Before spawning
the host agent, WTA puts the current package family's alias directory first on
`PATH`; unpackaged builds use the running binary's directory. Agent prompts can
therefore use short `wta.exe` commands without selecting another installed
branding or reproducing a protected package path.

### tmux-like CLI

WTA exposes tmux-equivalent subcommands for controlling Windows Terminal from the shell. Useful for humans and AI agents that can shell out.

```bash
wta list-windows                          # list all WT windows
wta list-tabs                             # list tabs in first window
wta list-panes                            # list panes in first tab
wta active-pane                           # show focused pane
wta new-tab -c "pwsh.exe" -n "Build"      # create tab running pwsh
wta split-pane -H -c "pwsh.exe"           # split horizontal
wta capture-pane -t 3 -l 50              # read last 50 lines from pane 3
wta kill-pane -t 3                        # close pane 3
wta pane-status -t 3                      # check if running
wta wait-for -t 3 --timeout 30           # wait for pane 3 to exit
wta resolve-command which --cwd . --json  # resolve from cwd + PATH + shell-specific sources
wta list-windows --json                   # raw JSON output
```

Short aliases are supported: `lsw`, `lst`, `lsp`, `neww`, `splitw`, `capturep`,
`killp`, `setenv`, and `mon`.

When `-t` (target pane) is omitted, the active pane is used automatically.

### Protocol Discovery & Environment Setup

WTA finds Windows Terminal via the `WT_COM_CLSID` environment variable, which
WT propagates into every conpty child it spawns. You usually don't need to do
anything — just run `wta` inside a WT pane.

```bash
# Inspect the inherited value
wta pipe-id                               # print CLSID
wta pipe-id --json                        # JSON with metadata

# Re-export it into another shell session (rarely needed)
eval "$(wta set-env)"                     # bash/zsh
wta set-env -s powershell | Invoke-Expression   # PowerShell
wta set-env -s fish | source              # fish
wta set-env -s cmd                        # cmd (copy-paste output)
```

### Test connectivity

```bash
wta test-pipe
wta --test-pipe     # legacy flag, still works
```

Connects to the WT protocol, prints `list_windows` + `get_capabilities`.

## Protocol Connection

WTA discovers Windows Terminal via the `WT_COM_CLSID` environment variable. WT
sets this in its own environment at startup and propagates it to every conpty
shell, so any pane-launched process — including wta and wtcli — inherits it.

## Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `WT_COM_CLSID` | Yes* | Stringified GUID of WT's `TerminalProtocolComServer` COM class |
| `WTA_LOG` | No | Rust tracing filter, such as `debug` or `trace` |

\* Set automatically by WT when it spawns a conpty child. If you launch `wta` from outside WT, run `eval "$(wta set-env)"` to copy the value over (only useful when you've previously captured it from a WT shell).

## Global CLI Options

| Flag | Description |
|------|-------------|
| `--json` | Output raw JSON instead of human-readable tables |
| `--agent <CMD>` | Agent CLI command for ACP mode (default: `copilot --acp --stdio`) |

## TUI Controls

Tool rows keep a localized type label such as **Run**, **Read**, **Search**, or
**Edit** visible across pending, running, and completed states. Consecutive
successful Read, Search, Edit, and Delete calls collapse into one summary row;
click that row to inspect each call. ACP thought chunks appear in an expanded
**Think** block with muted italic text and a left rule. Each thinking phase
automatically collapses when an answer or tool activity starts, thinking ends,
or the turn completes or is canceled. Click its header to reopen it, including
in completed history. Ctrl+O toggles thinking in the selected history turn, or
the active/latest turn when none is selected. Phase duration is measured locally;
replayed thinking has no duration because ACP does not supply historical timing.
Each block retains the latest 4,000 Unicode characters. No thought text is
invented when a provider is silent. Synthetic waiting feedback uses only the
shimmering Thinking indicator above the input box, never a transcript row.
Expanded Edit details show bounded line-level `+`/`-` hunks computed from ACP
snapshots. Tool headers and groups can be expanded during the active turn as well
as in history. Expanded Search details wrap the provider's `rawInput.query`
(or its title when no query is supplied) and any returned text results. WTA
does not reconstruct queries or results omitted by the provider. Queries retain
the first 4,000 Unicode characters, all scrollable when expanded. Text results show
up to 12 wrapped lines, with `…` for omitted text. Expansion follows the tool
into completed history.

Chat follows new output while you are at the bottom. Scrolling up preserves your
reading position as text streams, tools update, and turns finish; scrolling back
to the bottom resumes following. Sending a prompt or clearing/loading a session
still resets the view. Streaming thinking retains its latest 4,000 characters;
your reading position follows the same retained text even when older text is
trimmed. If the text you were reading is removed or a thinking block collapses,
the view clamps to surviving content.

| Key | Action |
|-----|--------|
| Type + Enter | Send prompt to agent |
| Ctrl+C | Copy selected text; otherwise cancel streaming / quit |
| Up / Down | Browse prompt input history |
| Mouse wheel | Scroll chat (hold Alt to scroll one line) |
| Click a tool header | Expand or collapse that tool's details, live or completed |
| Click a thinking header | Expand or collapse that block, live or completed |
| Ctrl+O | Expand or collapse thinking in the selected/latest turn (or the active turn), and all live and completed tool details |
| Mouse drag | Select a continuous text range |
| Double / triple click | Select a word / line |
| PageUp / PageDown | Scroll chat |
| F12 | Toggle debug panel (pipe traffic viewer) |
| Shift+PageUp/Down | Scroll debug panel |
| Y / N | Quick allow/reject on permission dialog |
| Up / Down / Enter | Navigate permission options |

WTA automatically selects **Allow once** only when the tool matches the exact MCP
server currently bound to that ACP session by master. Master overwrites provider
metadata with that identity on each forwarded permission request and tool update;
correlated calls must match the session, call ID, and current server identity.
Terminal actions still require their action-card confirmation, and
`request_user_input` still presents its question. Foreign or missing identities
(even with the same tool name or server-name prefix) and requests without an
**Allow once** option keep the normal permission dialog. WTA does not grant
persistent approval automatically.

Pending and replayed command suggestions show only the command, without assuming
Run or Insert. After the user chooses, history uses the localized
`Run: <command>` or `Insert: <command>` label. Cancelling retains the command with
a localized cancellation status on the same line, not on the conversation title.
History has no suggestion counts, numbering, or recommendation checkmarks.

## Debug Panel

Press **F12** to open a side panel showing all JSON-RPC messages between WTA and Windows Terminal in real time.

```
[3456.1] >>> {"type":"request","id":"3","method":"list_windows","params":{}}
[3456.1] <<< {"type":"response","id":"3","result":{"windows":[...]},"error":null}
```

- Green `>>>` = request sent to WT
- Cyan `<<<` = response from WT
- Shift+PageUp/Down to scroll

## Debug Logs

WTA writes structured logs under the package log dir, in a per-version
subfolder: `…\LocalCache\Local\IntelligentTerminal\logs\<pkgver>\` when
packaged (or bare `%LOCALAPPDATA%\IntelligentTerminal\logs\` unpackaged):

| File | Contents |
|------|----------|
| `wta-main_master.<UTC-date>.log` | `wta-master`: agent CLI pool, pipe accept loop, per-helper routing |
| `wta-main_helper-{pid}.<UTC-date>.log` | each `wta-helper`: pipe connect, ACP init, prompts, agent responses, TUI lifecycle |
| `wta-cli.<UTC-date>.log` | short-lived CLI helpers (`list-*`, `capture-pane`, `listen`, `sessions`) |
| `terminal-agent-pane.log` | Agent-pane chrome (C++ TerminalApp side) |
| `wta-ensure-host.log` | Background host startup / COM connection / SharedWta lifecycle |
| `wta-acp-debug.log` | ACP protocol debug trace |
| `wta-delegate.<UTC-date>.log` | `?<prompt>` delegation flow |
| `wta-probe.<UTC-date>.log` | Agent/model/session capability probes |
| `wta-install-hooks.<UTC-date>.log` | Hook installation and upgrade diagnostics |
| `wta-panic.<UTC-date>.log` | Synchronous panic backstop when the normal buffered record may not flush |
| `hook-trace.log` | Shell-hook event diagnostics |

Rust WTA streams with dated names rotate daily and retain up to three matching
files. If a daily writer cannot initialize, that stream uses the fixed
`wta-<stream>.log` name in the same directory. Per-PID helper logs are also
reclaimed after three days.

Set `WTA_LOG=debug` for verbose output (debug builds default to `debug`, release
to `info`). The F12 debug panel in the TUI shows protocol traffic live without
tailing log files.

## Project Structure

```
tools/wta/src/
+-- main.rs                    Entry point, role/CLI dispatch, protocol discovery
+-- master/mod.rs             wta-master: owns the agent CLI pool, multiplexes helpers
+-- helper/mod.rs             wta-helper: per-pane entry (reuses the TUI over a pipe)
+-- app.rs                     TUI state machine, event loop, per-tab sessions
|   +-- app/autofix.rs         Autofix detection + suggestion
|   +-- app/turn_state.rs      Per-turn state machine
+-- event.rs                   Crossterm event reader
+-- coordinator.rs             Delegate (?<prompt>) execution
+-- agent_sessions.rs          Session registry (status / liveness model)
+-- session_watcher/           CLI-log status classification per agent
+-- theme.rs                   Color constants
+-- protocol/
|   +-- acp/client.rs          ACP client (agent-CLI side) + helper-side WtaClient
+-- shell/
|   +-- shell_manager.rs       Terminal abstraction (local subprocess or WT pane)
|   +-- wt_channel/
|       +-- mod.rs             WtChannel trait definition
|       +-- cli_channel.rs     wtcli subprocess (CoCreateInstance via wtcli.exe) — all methods
+-- ui/
    +-- layout.rs              Main layout (+ debug panel split)
    +-- chat.rs                Message rendering
    +-- input.rs               Input box with cursor
    +-- permission.rs          Permission modal dialog
    +-- agents_view.rs         Session-management (/sessions) view
    +-- debug_panel.rs         Protocol traffic viewer (F12)
```

## Development

### Prerequisites

- Rust toolchain (edition 2021)
- Windows Terminal with protocol server enabled (for WT integration)
- An ACP-compatible agent CLI (Copilot, Claude ACP adapter, etc.)

### Build and run

Run these commands from the repository root. CI resolves the
`tools/wta/rust-toolchain.toml` `ms-prod-1.93` pin through MSRustup; local
repo-root commands use your installed active toolchain, so changes must remain
compatible with Rust 1.93.

```bash
# A live process may lock the output. Stop only a PID whose executable path
# matches this target; do not kill every wta.exe by name.
cargo build --target x86_64-pc-windows-msvc --manifest-path tools/wta/Cargo.toml

# cargo build does not compile #[cfg(test)] code.
cargo test --target x86_64-pc-windows-msvc --manifest-path tools/wta/Cargo.toml
```

The TUI (master + helper) is launched by Windows Terminal as an agent pane — see
the C++ F5 / `bcz` flow in the repo `AGENTS.md`. From a WT pane you can exercise
the CLI helpers directly with the packaged `wta` app execution alias.

### Development workflow

1. Open Windows Terminal (with the agent pane / protocol server enabled)
2. Run `wta pipe-id` to verify `WT_COM_CLSID` is set
3. Open the agent pane (`>Toggle AI assistant` / `Ctrl+Shift+.`) — WT spawns the
   helper, which connects to master and renders this TUI
4. Press F12 to open the debug panel and see all protocol traffic
5. Interact with the agent -- watch requests/responses flow in real time
6. Use `wta list-panes`, `wta capture-pane` etc. in another pane for debugging

While connecting, the chat activity row shows WTA's current operation: preparing
the agent connection, connecting to the local coordinator, initializing the
connection, refreshing user authentication after login (when supported), reading
the coordinator's session registry snapshot, creating a session, or setting its
model (when requested). Initialization and creation include local preparation
and registration, not just waiting on the agent. `/restart` first shows
"Restarting agent" while old sessions retire. These are local operation
boundaries, not agent-reported progress: they do not expose internal MCP or
model-catalog loading, and reading the registry does not fetch agent history.
A queued session restore shows the actual connection stage first, followed by
short resume context; once connected, it shows only "Resuming session" until
the load completes. The pane does not become connected earlier, and these labels
do not reduce startup time.
After loading, the pane header and model picker use the restored session's
agent-reported model, when available, without switching it to the current
default model. Settings still supplies the requested model for new sessions
and later model changes; an existing confirmed selection stays visible until
the agent confirms the switch.

### Diagnosing a missing current-shell pane

Default logs record failures without requiring `WTA_LOG=debug`:

- `terminal-agent-pane.log`: the actual server PID/window/tab, requested source,
  and why pane selection failed (for example, `active_agent_without_source`,
  `selected_pane_has_no_session`, or `explicit_source_unresolved`). Exceptions from
  the page-context query are logged once at the COM boundary with their HRESULT.
- `wta-main_helper-{pid}.<UTC-date>.log` (or the fixed
  `wta-main_helper-{pid}.log` fallback): `pane_context_unavailable` reasons distinguish
  protocol failure, an agent pane, and unresolved legacy lookup.
  `pane_context_response_contract_error` records invalid responses.
  `prompt_has_no_bound_pane` identifies the affected helper/prompt;
  `terminal_action_no_active_target` records rejection at the action check.

Use **Report a bug** to collect these in the existing log ZIP. These new lines
omit commands, terminal output, titles, and working directories; other existing
logs may contain private data, so inspect the ZIP before sharing it. These are
failure-time observations, not a history of how pane/source state changed.

### Adding a new WT protocol method

1. Declare the method in `src/cascadia/TerminalProtocol/TerminalProtocol.idl`
2. Implement it on `TerminalProtocolComServer` (`src/cascadia/WindowsTerminal/TerminalProtocolComServer.cpp`)
3. Add a `wtcli` subcommand in `src/tools/wtcli/main.cpp` that calls the new method
4. Add a `CliChannel::request` arm in `tools/wta/src/shell/wt_channel/cli_channel.rs` mapping a method name to the new `wtcli` subcommand
5. Rebuild WT, wtcli, and wta

## Architecture Notes

- **ShellManager** owns local terminals and the active `WtChannel`
- **CliChannel** shells out to `wtcli.exe` per call; `wtcli` does `CoCreateInstance` to reach WT's COM server. All methods, including `send_input` (via `wtcli send-keys`), go through this path.
- **Protocol discovery**: `WT_COM_CLSID` env var, inherited from the WT-spawned conpty
- **CLI subcommands** call `CliChannel::connect()` directly; no ShellManager needed
- **Pane identity** is discovered at startup via PID matching (list all panes, find ours)
