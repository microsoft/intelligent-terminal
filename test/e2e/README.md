# ItE2E — Intelligent Terminal End-to-End Test Framework

A robust, CLI-composition test framework that drives and verifies a **deployed
(MSIX-packaged)** Intelligent Terminal. Tests are authored in **PowerShell + Pester 5**.
Design rationale is captured in the inline notes below and in each suite's header comments.

## Release-checklist coverage

The `tests/` folder implements the `[E2E]` items from
`doc/release-check-list.md` that are automatable on one machine. Copilot drives
the baseline suites, while the agent matrix covers other installed and
authenticated ACP agents. Current status (run on the Store package):

| Suite (file) | Covers | Cases |
|---|---|---|
| `Feature.Packaging.Tests.ps1` | §9 packaging/protocol (incl. WT_COM_CLSID injected into pane shells) + §10 logging + log retention/cleanup | 18 |
| `Feature.WtcliPublishStdin.Tests.ps1` | PR #652: WTA/wtcli stdin transport delivers command-line-limit-sized events intact and preserves positional compatibility | 3 |
| `Feature.Settings.Tests.ps1` | §1 Settings>AI Agents + §0 FRE settings/positions/auto-error/session-mgmt | 18 |
| `Feature.FreFlow.Tests.ps1` | §0 FRE overlay click-through (Next→Save, privacy link, close-safety) | 5 |
| `Feature.FreExecutionPolicy.Tests.ps1` | §0 FRE automatic CurrentUser execution-policy remediation (**Dev**, auto-skips) | 4 (1 conditional skip) |
| `Feature.FreHooks.Tests.ps1` | §0 FRE progressive setup ordering, session hook installation, failure, and retry (**Dev**, auto-skips) | 3 |
| `Feature.AgentPaneInteraction.Tests.ps1` | open/hide/focus, input/rendering, slash, Copilot chat | 14 |
| `Feature.AgentProtocolExperience.Tests.ps1` | PRs #599/#601/#606/#610/#611/#612/#616/#634/#683: intent-based terminal actions (including empty workspaces and configured delegation), ACP tool/transcript rendering, clarification input, session configuration, model title, and replacement cleanup across the deployed helper/master boundary | 8 |
| `Feature.AgentImageAttachmentEditing.Tests.ps1` | PR #536: inline image tokens move and delete atomically while preserving adjacent prompt text | 1 |
| `Feature.AgentModelSync.Tests.ps1` | PR #538: ACP config-option updates replace stale session model state in the active picker | 1 |
| `Feature.AgentModelLifecycle.Tests.ps1` | PR #554: `/model` hot-apply and Settings-driven model restart/reconnect lifecycle | 2 |
| `Feature.ByokProvider.Tests.ps1` | PR #447: Settings-selected OpenAI-compatible provider request path, credential handling, and BYOK-to-cloud restart lifecycle | 2 |
| `Feature.AgentCompactLayout.Tests.ps1` | PR #580: compact-height recommendation, input, and Insert interaction at the real splitter minimum | 1 |
| `Feature.AgentPanePadding.Tests.ps1` | Issue #793: deterministic full recommendation card, navigation hint, and action alignment across the packaged WTA render boundary | 1 |
| `Feature.ProposalMcpRouting.Tests.ps1` | PR #560: per-session proposal MCP names and two-tab Helper routing isolation | 1 |
| `Feature.AgentMouse.Tests.ps1` | PR #506 and issue #790: physical chat wheel scrolling, Ctrl+wheel zoom, draft preservation, text selection/copy, and stale-selection suppression; completed-turn full-row clicks across multiline prompts with shared keyboard selection/Enter behavior, row-end/drag guards, and input-dialog focus recovery | 7 |
| `Feature.AgentSelectAll.Tests.ps1` | Physical Ctrl+A selects only the focused nonempty draft: exact source copy, cut/delete/replace, repeat/Esc/caret collapse, and pending-turn safety; empty input and history focus retain pane copy and stale-selection clearing. Deterministic ACP fixture; unique evidence under `ITE2E_ARTIFACT_ROOT` (default `artifacts`) | 6 |
| `Feature.AgentInputNavigation.Tests.ps1` | Physical Up/Down edits explicit and soft-wrapped input rows, preserves preferred display columns and viewport following, collapses full-input selection safely, and retains deterministic prompt-history boundary behavior | 5 |
| `Feature.AgentCenterNativePaste.Tests.ps1` | Rounds9–10: real foreground Ctrl+Shift+V preserves ASCII/BMP, supplementary/joined emoji, selection replacement, and CRLF draft rows; physical Shift+Enter inserts a newline without dispatch; explicit Enter rejects an unknown command. Dev/hash gated; empty isolated service, no model | 2 |
| `Feature.AgentCenterWorkFlow.Tests.ps1` | Global conversation: zero-project chat, grounded project approval, one editor across dashboard/work hints, sustained navigation, target-bound proposals, raw resync, typed intake, captured-report acceptance, initial brief/Start, and actual TaskInput continuation. Dev/hash gated, isolated state, no external model | 11 |
| `Feature.WorkStoryDemo.Tests.ps1` | **Compact native Agent Console, zero shell tabs**: four interactive legacy histories, exact resume/branch prompts, F5 evidence, automatic schema handoff, timed owner-scoped attention, explicit 10K/20K/30K cap picker with cancellation and idle budget progression, three Work cards, replay/reopen and isolated reset. Dev/hash pinned; one owned 1360 x 900 window and new-UI-only ConsoleInput. Historical/manual-advance results do not qualify current C334–C344 | 11 |
| `Feature.PromptHistory.Tests.ps1` | PR #478: per-tab Up/Down prompt recall, draft restoration, and multiline preservation; PR #614: completed-turn collapse/expand rendering | 4 |
| `Feature.CompletedTurnSelection.Tests.ps1` | Completed-turn Tab/Up/Down selection keeps focused history inside the chat viewport | 1 |
| `Feature.AutofixPane.Tests.ps1` | Direct Helper Autofix proposal card render/insert/run/reject/target/stashed + across layout + WSL shell identity and Linux fixes | 12 (2 WSL-gated) |
| `Feature.AutofixParser.Tests.ps1` | issue #474: PowerShell ParserError-to-Autofix pipeline + success/handled-error/blank-input negative controls | 4 |
| `Feature.AutofixRouting.Tests.ps1` | Two Detected tabs: real diagnostics clicks submit only to the selected tab's ACP session and preserve the other tab's opt-in | 1 |
| `Feature.PaneContext.Tests.ps1` | issue #838: packaged pane-context capture, marked/unmarked output, explicit routing, missing panes, metadata-only mode, Unicode bounds, and agent-focus source resolution | 7 |
| `Feature.CommandResolution.Tests.ps1` | PR #418: packaged WTA resolves PowerShell profile-only aliases to their real targets | 1 |
| `Feature.AutofixCommandResolution.Tests.ps1` | Issue #844: Debug Dev, deterministic ACP fixture; no startup/tab-selection probes, first/later Autofix contracts without enumeration, and explicit local-candidate lookup | 3 |
| `Feature.SessionList.Tests.ps1` | session view (button + `/sessions` slash), session states, view switching (incl. draft-preservation), focus/restore | 13 (+1 skip) |
| `Feature.NonAsciiCwd.Tests.ps1` | issue #641: a non-ASCII starting directory survives `wtcli` argv → COM → `CreateProcessW`, so the resume launch path connects and starts in that directory | 2 |
| `Feature.AgentPaneCwd.Tests.ps1` | agent-pane source workspace reaches ACP `session/new` and remains stable across `/new` without a model prompt | 1 |
| `Feature.AgentRestart.Tests.ps1` | agent restart after a settings change (/restart reconnects and answers) | 1 |
| `Feature.ShellIntegration.Tests.ps1` | §3 shell-integration OSC 133 marks (success/failure, ParserError dedup, handled errors, WinPS 5.1 errors) + non-integrated cmd.exe safety | 6 |
| `Feature.BashPromptIntegration.Tests.ps1` | PR #468: Bash `PROMPT_COMMAND` PS1 rewrites preserve D/A/B boundaries; non-IT hosts remain gated | 1 (Git Bash-gated) |
| `Feature.AgentProposedCommand.Tests.ps1` | §2 Direct Helper Proposal Insert/Run into the shell pane | 2 |
| `Feature.YoloMode.Tests.ps1` | Default-provider-scoped automatic approval persistence across global, `/agent`, and profile bindings; deterministic permission boundary; hidden unsupported/policy states; retained Gemini guidance; and live policy reconciliation | 8 (OpenCode, Gemini, `/agent`, profile, and policy gated) |
| `Feature.AgentProposalFocus.Tests.ps1` | PR #533: Insert returns real window keyboard focus to the target shell pane | 1 |
| `Feature.AgentMatrix.Tests.ps1` | §2 non-Copilot built-in agents (Claude/Codex/Gemini) connect+chat through the ACP adapter — ONE consolidated case (Copilot is the in-depth suite); skips when none installed+authed | 1 |
| `Feature.HookTrace.Tests.ps1` | C190 + PR #571 C267-C269, C272: every shipped bundle's guarded command still delivers, `tool_input` survives only for interactive prompts, shells outside Terminal are ignored, and the broadcast envelope stays inside its budget | 5 |
| `Feature.SessionHookRouting.Tests.ps1` | PR #761: master consumes one `wtcli agent-hook` COM broadcast directly while multiple helpers update only local pane bindings, a terminal hook for an unseen session fabricates no row, and `agent.error` still records the failure | 3 |
| `Feature.SessionOwnershipRestore.Tests.ps1` | PR #950: a UUID-shaped nested-agent prompt cannot replace the resumable root session persisted for its pane | 1 |
| `Feature.HookBridgeCli.Tests.ps1` | PR #571 C274, C265, C266: a real agent CLI fires the bundled `hooks.json` command through its own shell, and neither an unreachable protocol server nor an uninstalled Terminal blocks the CLI; skips when the CLI isn't installed+authed | 3 (environment-gated) |
| `Feature.LegacyHookBundle.Tests.ps1` | PR #571 C270-C271: a pre-#571 PowerShell hook bundle still delivers against a post-#571 Terminal, and degrades quietly when `WT_COM_CLSID` is unset | 2 |
| `Feature.OpenCodeHookBridge.Tests.ps1` | PR #571 C273: OpenCode's JS plugin spawns `wtcli` through an argv array with no shell, so it resolves the bridge via `WTCLI_PATH` rather than the `PATH` alias | 1 (environment-gated) |
| `Feature.OpenCodeAgent.Tests.ps1` | PR #458: built-in OpenCode launches its native ACP server and completes agent-pane chat | 1 (environment-gated) |
| `Feature.OpenCodeSessionResume.Tests.ps1` | PR #464: OpenCode history discovery and `--session` resume restore the prior transcript | 1 (environment-gated) |
| `Feature.OpenCodeHooks.Tests.ps1` | PR #476: packaged hook install, shell-session lifecycle routing, picker visibility, and ACP duplicate suppression | 1 (environment-gated) |
| `Feature.SharedAgentLifecycle.Tests.ps1` | PR #425 + ACP cleanup: closing a tab mid-turn physically closes only its session without terminating the shared agent CLI or breaking sibling tabs | 1 |
| `Feature.AgentPaneLifetime.Tests.ps1` | Issue #841: hidden retention, split-tab cleanup, lease retirement, draining-only crash suppression, agent-first/later cross-window moves, and rejected pane-move rollback preserving both tabs and sessions; deterministic stdio fixture | 10 |
| `Feature.PerTabAgent.Tests.ps1` | C225-C228 + PR #487: `/agent` picker/direct selection, invalid-id safety, per-tab isolation/shared-master reuse, and global-default/override behavior | 7 |
| `Feature.WslAgentBackend.Tests.ps1` | PR #481 profile-scoped WSL agent backend: settings hot reload, helper/master source routing, and authenticated chat | 2 (environment-gated) |
| `Feature.DelegateSource.Tests.ps1` | PR #488 profile-scoped delegate source: strict host/WSL `wta delegate` routing with no fallback in either direction | 2 (environment-gated) |
| `Feature.AgentChat.Tests.ps1` / `Feature.AgentPopup.Tests.ps1` | agent chat + `/` popup/menu interaction | 1 + 3 |
| `Feature.AgentPaneMove.Tests.ps1` | PR #429: `/move` stays per-tab, preserves global position, and restores agent input focus | 1 |

**Coverage: 156 of 158 automatable `[E2E]` checklist items are implemented.**
**Test status: 136 baseline feature cases pass + 3 documented skips** (`wta sessions list` is
identity-gated — see `Feature.SessionList.Tests.ps1`), plus 2 PR #481 WSL-backend cases and 2
PR #488 delegate-source cases that run only when a runnable distro (and, for the #481 chat
case, an installed+authenticated native agent) is available. The 156 implemented checklist
items map to the baseline cases plus the deterministic settings/persistence assertions. The
remaining new items are the two profile agent picker UIs; they stay explicit E2E work rather
than being falsely credited by the JSON-level runtime tests. Other
environment-dependent items are tracked and auto-skipped when their prerequisite is absent:
**other agent CLIs** (`Feature.AgentMatrix.Tests.ps1` now covers Claude/Codex/Gemini chat,
auth-gated per CLI — each Context runs only when that CLI is installed *and* authenticated,
else skips); custom agents; multi-window drag; hook/CLI install; policy locks; IME/paste; WSL
autofix (needs a dev build with OSC 9001 ShellType + a running distro); WT window-level
keyboard accelerators (command palette / Delegate `Alt+Shift+B` / pane hotkeys — not
injectable via UIA/send-keys in this harness); and manual release-sign-off gates.

Token-consuming simulated-real-user tests are deliberately excluded from this publishable suite
and from CI. They live only in the feature's dev-only local validation harness and run manually
against an exact deployed publish package with explicitly available provider quota.

`local\Invoke-RealAgentCenterValidation.ps1` is a standalone, opt-in real-Copilot
Agent Center qualification, outside Pester/CI discovery. Use a fresh absolute
state directory with
`build\scripts\Start-IntelligentTerminalAgentCenter.ps1 -Package Dev -StateDirectory ... -ConfigureOnly`
and serialize its returned object to a setup JSON file. Start that binary's
`center serve` with `INTELLIGENT_TERMINAL_AGENT_CENTER_STATE` set to the same
directory; keep this owned authority running while validating. Then run:

```powershell
$env:ITE2E_PACKAGE = 'Dev'
pwsh -File test\e2e\local\Invoke-RealAgentCenterValidation.ps1 `
    -SetupJson <setup.json> -ArtifactDirectory <new-absolute-directory> -RunModel
```

This consumes the approved provider's model quota. It reuses the existing
framed-pipe helpers to submit human requests and confirm the model's exact
recorded start proposals. It requires two concurrent real ACP sessions,
independent code/report workspaces, failing-before/passing-after Node tests,
unchanged protected inputs, read-only global progress inquiry, and a follow-up
in the same executor session. It saves actual requests, replies, implementation,
report, session bindings and verification receipts. `fixtures\agent-center-real`
contains only the initial problem and test oracle, never the solution or model
responses. The test does not qualify formal WorkExecutor delivery acceptance,
provider-tool sandboxing, or native keyboard interaction; the native window can
be opened separately against the same store. Do not stop other authorities,
overwrite user work, or count this quota-consuming run as a default CI pass.

`tools\AutofixPrompt.Local.Tests.ps1` is an opt-in, quota-consuming Dev validation
of actual Copilot decisions, outside the default `tests`/`selftests` discovery.
It runs three fresh-session samples each of an obvious Git typo and an unfamiliar
local command typo. The oracle inspects session-scoped tool calls: the obvious
typo must go directly to a correction card with no discovery tools, while the
local command must be resolved and its corrected script must run in the source pane.
Run it explicitly through `Invoke-ItE2EReport.ps1 -Path` with `ITE2E_PACKAGE=Dev`
and `ITE2E_EXPECTED_WTA_SHA256` set to the deployed feature build.
`ITE2E_COMMAND_FIXTURE_DIR` must name an existing writable user PATH directory;
the test creates uniquely named scripts there and removes them afterward. This
models an installed local command, rather than assuming a script in the current
directory is on PowerShell's command search path. Set `ITE2E_AUTOFIX_MODEL` to
pin the Copilot model being evaluated. Settings are preserved through ItE2E;
the test does not modify PATH or profiles.

`Feature.AutofixCommandResolution` requires a Debug Dev build so its negative
probe assertions have enabled diagnostic evidence. Store selections (including
the Store package family name) skip this suite during default discovery.
Missing/invalid package selections and Dev build mismatches still fail. Set
`ITE2E_EXPECTED_WTA_SHA256` to the SHA-256 of the feature-branch build when
validating a change; the suite rejects a mismatched deployed binary. Its unique
artifact directory records the package hash, received ACP contracts, query
results, and scoped helper logs. The fixture uses disposable command files and
does not modify the user's PowerShell profile or consume model quota.

## Agent Center task-centered product qualification

`Feature.WorkStoryDemo` now exercises the **integrated native Agent Center**:
one dedicated Console, zero shell tabs, shared Work list/conversations/details
and the opt-in isolated demo provider. It replaces the retired standalone
storyboard suite. C334–C344 retain their independent behavior identities with
new exact **Native demo** titles; historical lower-shell results must not count
as native signoff. The natural journey now requires four interactive legacy-history
tabs, explicit token-cap selection and autonomous attention/budget progression: manually submitting `Show ...`
phrases cannot qualify these behaviors. Generate a fresh report from the current checklist and actual
native results before using incremental updates.

Run the complete ordered journey, not individual `It` filters, with
`ITE2E_PACKAGE=Dev` and `ITE2E_EXPECTED_WTA_SHA256` pinned independently to the
intended packaged feature binary. Startup snapshots all existing windows,
launches one owned native window and verifies zero shell tabs. The automated suite
uses input records scoped to its newly owned Console, without stealing input from
other applications; F1/F2/Enter/F5
must open the same projected Work. The dedicated Console has no pane GUID:
`Get-WtCapture`/`Send-WtInput` to an ordinary shell are not acceptable substitutes.
For this input route, a disposable PowerShell child uses `AttachConsole` and
`WriteConsoleInputW` to submit key/text records to only the newly owned `wta ui`.
The helper verifies its exact installed executable, `ui` command, creation time,
new PID, owned HWND, zero tabs and demo UIA marker. It holds a process handle across
attachment to prevent PID reuse, then releases the input handle and console.
The test runner never detaches from its own console. This exercises the same
native input loop/shared renderer but **does not qualify physical keyboard or
clipboard integration**; `qualification.json` and input artifacts name the route.
Attachment/input failures fail the test rather than claiming a pass.

Every scene pairs isolated `--inspect` state with HWND-local UIA text and
diagnostic PNGs under a unique `native-work-story-*` artifact root. PNG existence
does not prove physical repaint; cached DWM surfaces may be blank or stale.
`qualification.json` keeps that limitation explicit. The simulated evidence,
executor identities and token budget do not qualify autonomous WorkExecutor,
production work-service/protocol coordination, real agents or provider billing.

### Recording the integrated native work demo

`build\scripts\Start-IntelligentTerminalWorkDemo.ps1` is the durable launcher for
the **dedicated native Agent Console**, not a bottom shell pane. It starts
`%LOCALAPPDATA%\Microsoft\WindowsApps\<selected-Dev-PFN>\wtai.exe -w new`,
using the package-scoped execution alias and verifying the resulting host's
package identity. Direct `AppX\WindowsTerminal.exe` activation can lack package
identity and is not used; the global `wtai` alias can target Store and is never
used. A missing package-scoped alias fails clearly. Only that launch's environment is set:
`INTELLIGENT_TERMINAL_AGENT_CENTER=1`,
and `INTELLIGENT_TERMINAL_WORK_DEMO_STATE=<absolute isolated root>`.
**Existing-host prerequisite:** Agent Center enablement is read from the native
host's process environment, not the per-window environment. If Dev is already
running, that host must already have Agent Center enabled. Launch-scoped
`INTELLIGENT_TERMINAL_AGENT_CENTER=1` cannot toggle an existing non-Center host,
although the isolated demo-state variable does reach the new window's `wta ui`.
The helper requires the actual top native Console and zero shell tabs; unsupported
host mode fails instead of accepting a shell substitute. It never kills or
restarts an existing host, changes settings, or forces the user's windows into
another mode. With no existing Dev host, the newly started host inherits the flag.
Cold-start discovery does not activate COM before launch. After activation it
waits for one package-verified HWND to render the demo, then queries COM window
IDs: querying COM during first layout can itself create a second window.
An already-starting/windowless packaged host is rejected rather than raced;
ambiguous windows are retained with diagnostics, never closed speculatively.
The native UI retains its existing OS locale; story fixtures are English.
No invented language environment override, user/machine environment, settings or production work store is
changed. The helper checks the pinned packaged WTA hash, exactly one new native
window/HWND, **zero shell tabs**, the isolated inspect state and an explicit demo
marker in the native UIA document.
The newly owned HWND alone is centered at 1360 x 900 physical pixels by default,
clamped to its monitor's work area. `-WindowWidth` / `-WindowHeight` on the
launcher override that size. It no longer maximizes the demo, and never resizes
preexisting windows or changes the user's font/settings.
Default storage is the selected package's
private `LocalState\IntelligentTerminal\work-story-demo-native`; pass `-StateDir`
for a separate recording. Without an independent expected hash, recording is
allowed with a warning and the installed hash recorded, not feature qualification.

```powershell
$nativeDemo = & .\build\scripts\Start-IntelligentTerminalWorkDemo.ps1 -Package Dev `
    -StateDir .\test\e2e\artifacts\native-demo-recording\state `
    -ArtifactDirectory .\test\e2e\artifacts\native-demo-recording\evidence `
    -ExpectedWtaSha256 $env:ITE2E_EXPECTED_WTA_SHA256
# Record/interact with the owned native window. Its state is retained.
. .\test\e2e\tests\helpers\NativeWorkStoryDemo.ps1
Stop-NativeWorkStoryDemo -Context $nativeDemo
```

The default marker is `Work story demo`; the persistent subtitle includes
`Work story demo | Simulated data | Scene N/8`. `-ExpectedUiMarker` can name the exact
feature-build disclosure. Close targets only the verified new HWND and refuses
if user-added shell tabs appear. Existing windows/processes are never stopped.
Startup text/state/PNG and launch/cleanup provenance remain separate from legacy
shell-suite artifacts. PNGs are diagnostic until physical repaint is reviewed.
Native route/action qualification must use this zero-tab window's UIA and native
input boundaries; shell `send-keys` cannot stand in for native Console input.

The same test helper exposes recording primitives for an **already owned**
`$nativeDemo` context; these functions never create or close a window:

| API | Contract |
| --- | --- |
| `Send-NativeWorkStoryKey -Context $nativeDemo -Vk <virtual-key> [-Ctrl] [-Shift]` | Owner-guarded native control focus and input; records key/timestamp/transport. |
| `Send-NativeWorkStoryPrompt -Context $nativeDemo -Text <phrase> [-Global]` | Preserves the current Work scope by default, verifies exact composer text before Enter, and restores clipboard even on failure. |
| `Set-NativeWorkStoryDraft -Context $nativeDemo -Text <text> [-Global]` | Enters/verifies an unsent draft without Enter, for automatic-attention draft-preservation controls. |
| `Wait-NativeWorkStoryCondition -Context $nativeDemo -StateCondition { param($state) ... } -Text <markers> [-TimeoutSec 30]` | Predicate-based read-only state/UIA wait at one-second intervals; returns `State`, `Text`, `CapturedUtc`. |
| `Save-NativeWorkStoryMarker -Context $nativeDemo -Name <safe-name> [-ArtifactDirectory <path>]` | Paired state/UIA/diagnostic HWND PNG and JSONL timestamp/hash marker, without capturing the desktop. |

The complete natural recording driver uses those same primitives:

```powershell
$walkthrough = Invoke-NativeWorkStoryWalkthrough -Context $nativeDemo `
    -PauseSeconds 4 -OnStep {
        param($step)
        # Scene, Title and Utc are delivered when the scene's native UI is ready.
        # The recording caller can append its chapter timestamp/subtitle here.
        Write-Host "Scene $($step.Scene): $($step.Title)"
    }
```

It requires fresh legacy Scene 1 with `capConfigured=false`; it never resets or
silently upgrades the caller's state. Older persisted demos retain their earlier
automatic-budget semantics and do not qualify cap-picker coverage. Left/Right
selects four distinct old histories, F1 resumes the Work, F5 exposes evidence,
F4 confirms the related Work, and F5 exposes its automatic schema result.
F5 opens a read-only compact detail view without the composer; F5 returns to
the unchanged draft. F7 expands complete evidence and raw JSON without changing
the stored Work; the natural walkthrough uses compact F5.
Decision and cap menus clear that detail view, identify the action's owning
Work, and show only Select/Confirm/Cancel controls. Escape restores the prior
evidence view and draft; it does not change the current Work or budget.
It then waits for the owner-scoped attention banner, selects the default
recommended B from F6, opens the token-cap picker with F6 and confirms its
default 20K limit (10K and 30K are also available), then observes increasing **persisted and visible**
budget samples without input, and ends at F2's three-Work overview. There are no
manual `Show ...` submissions or Tab-based scene stepping. The scripted clock
requests attention after ten seconds and consumes 5K simulated tokens every
three seconds after both the decision and explicit cap confirmation, pausing
exactly at the selected cap. Opening or cancelling the cap picker does not spend
tokens.

`-OnStep` receives `{ Scene, Title, Utc }` after each scene's positive state/UI
oracle, before its readable hold; Scene 1 is announced before cycling its tabs.
Scene 7 is announced while the cap picker is visible, before confirmation, then
the driver observes idle increments without further input.
Requested pacing uses deadline predicates. The walkthrough writes
`natural-steps.jsonl`, paired text/state snapshots and budget-sample markers;
it deliberately skips synchronous screenshots while the caller records video.
Markers distinguish the persisted scenario `scene` from `displayedScene`
because ordinary view navigation does not fabricate persisted user inputs.
In particular, F5's evidence view displays Scene 3 while the saved scenario
remains at resume stage 2 (F2 alone still displays 2); the final Work Overview displays Scene 8
while the budget-paused scenario remains at stage 7.
It **leaves Scene 8 open** and never stops the recorder or closes the window.

For a viewer-focused presentation, prepare a reviewed `cues.json` timeline
against the actual decoded footage, not wall-clock keyboard timestamps.
Each case needs a short goal and result, with action/automatic phases as
appropriate. Cue text, shortcuts and focus rectangles must match the visible
product state. Hold actual input/menu frames long enough to read; retain the
source-time map if footage is trimmed or slowed. Render with:

```powershell
.\build\scripts\Edit-IntelligentTerminalWorkDemoVideo.ps1 `
    -InputVideo <recording>\work-demo.mp4 -CuesPath <reviewed-cues.json> `
    -OutputDirectory <new-edited-directory> -FfmpegPath <ffmpeg.exe>
```

The 1920 x 1080 presentation keeps the 1360 x 900 native footage unscaled at
`x=520, y=120`, with English burn-in captions, shortcut pills, an eight-case
progress indicator and focused highlights. Before/After/How headings distinguish
session management from work management and its concrete interactions.
Captions occupy a separate sidebar; a persistent label identifies simulated
execution and token usage.
To verify product content independently of editorial text, crop
`1360:900:520:120` from the final video and run the actual-pixel checker with
the edited `chapters.json`. Existing verified footage may also be edited;
retain its source hash and a source-time map for cuts or reading holds, and do
not describe older footage as a recording of a newer deployed build.

`Context.InputRoute` is `Keyboard` or `ConsoleInput`; absent an explicit value,
the helper selects keyboard when a foreground desktop exists and ConsoleInput
otherwise. ConsoleInput never qualifies physical clipboard/key delivery.
Video capture remains separately owned by the recording caller.
The recorder brings only its owned demo window to the foreground and requires
an unlocked interactive desktop; it fails explicitly if activation is refused.
Before sharing an MP4, run `Test-IntelligentTerminalWorkDemoVideo.ps1` against
its chapters. A background or noninteractive desktop can return an unchanged
cached `PrintWindow` surface even while UIA and the persisted Work advance.
Successful encoding is not proof of a usable recording; reject frozen footage
and retain the last verified video.

The [Agent Center verification plan](../../doc/specs/agent-center-verification-plan.md)
defines a separate task-centered product matrix and continuous human work
journey. Developer/Verifier coordination for `LocalCode | Report` is the first
pilot toward one conversation for using the OS to finish tasks, not the whole
product. Success means verified, human-accepted goal outcomes without requiring
the user to choose internal shells/providers, launch agents, compact context,
copy diagnostics or manually schedule progress.

Keep three evidence layers separate: component/transport conformance, deployed
native UI interaction, and real-model business autonomy. In particular,
`Feature.AgentCenterNativePaste` and offline editor/parser passes protect input,
not autonomous planning, internal handoffs, failed-check rework or accepted
delivery. A scripted coordinator and successful model text cannot prove that
the product carries those responsibilities.

The plan retains R1-R9 defect regressions and J1-J8 journey meanings, adds J9
for grounded global status/execution location/rationale and an actual confirmed
alternative plan while
another work advances, and maps planned T-* cases to US/AC requirements.
Full current continuous-pilot completion requires every named current required
J1-J9 gate, current AC-91 through AC-95, applicable R1-R9 gates, and two verified,
explicitly human-accepted Completed deliveries on the same identified build.
Natural language plus explicit confirmation is the main path; structured CLI
requests alone cannot prove the conversational experience. J5 must include
an actual failed check and fresh checked correction; J8 report revision is
not a substitute. No new suite or `[E2E]` coverage is created by these spec IDs.

Use the plan's frozen cohort, separate intake funnel and human-assistance
ledger to report unassisted accepted delivery, retaining failed, blocked and
unfinished and postapproval cancelled approved work in the denominator (zero
is N/A); numerator works require verified goals, explicit acceptance, Completed
state and zero required human orchestration. Distinguish test
setup/build provenance from product-required assistance. Report
PASS/FAIL/BLOCKED/NOT RUN separately from planned/not implemented coverage.
Supported requirements cannot silently become skips; later managed-runtime
adoption and future OS-state effects/recovery remain unqualified adapter/type
scenarios, not capabilities proved by a local report. The plan maps future
OS-A isolated test-service restoration to T-10/T-11, OS-B safe test-file
organization to T-12, and OS-C authorized existing-runtime adoption/replacement
to T-09; AC-96 through AC-98 remain unqualified.

Future deterministic packaged cases should reuse ItE2E and exact checklist-title
mapping only where their actual boundaries warrant it. Keep quota-consuming
real-model journeys outside default published/CI discovery with explicit
package/build and provider-budget authorization. See the
[experimental release guidance](../../doc/release-check-list.md#experimental-task-centered-work-journey-roadmap).

### Global conversation native contract

`Feature.AgentCenterWorkFlow` has eleven native cases, plus the two separate
NativePaste cases. It pins `--language en-US`; NativePaste retains its existing
locale behavior. One lifetime global console/conversation identity and one
global editor replace per-work chats/drafts. Default UI is conversation only;
F7 explicitly opens the dashboard and Esc returns. F2 and readable F4 selectors
change optional context hints, not the message destination or editor.
Work-hint helpers use the explicit F4 `Global conversation` Home action before
selecting a work and waiting for its rendered hint.
They never send an unconditional Esc after selection: Esc in plain chat clears
the selection anchor, unlike Esc closing an actual dashboard or modal layer.

The fixture explicitly sets `conversationCapabilityId = ite2e-local-global`
among its local scripted adapters and supplies all seven bounded
`conversationLimits` fields. It starts with **zero Projects and zero Works**.
Only the actual native hello/assistant round trip qualifies project-free chat.
Global adapters read scope from `coordinationInput.snapshot.scope`: the runtime
wire deliberately omits the internal top-level `Invocation.scope`. The snapshot
itself is the captured `Conversation`, including its identity, messages and
project/work summaries. Both captured summaries and current list reads must
be empty for the hello proof. The offline
`selftests\fixtures\AgentCenterGlobalHelloInvocation.json` preserves the
credential-free `runtime.invoke` payload from the failed native run on Dev
`99EA7596B3D215026D997ADA8854CF5EA6B6AAF9272840C35643C54F62F7C18B`.
It exercises the current `agent-center-console.md` prompt boundary, not the
legacy coordinator contract; replaying it offline does not qualify native chat.
The next case approves one existing, literally named owned directory through
a real `HumanActionProposal`. Additional never-started work/project fixtures
are seeded through public APIs **after** the zero-project chat proof for the
two-project navigation/load and existing safety cases. Those setup operations
are not credited as conversational project creation or autonomous execution.
The owned Harbor, Orchard and Horizon fixtures explicitly approve
`ite2e-local-global` in `capabilityIds`, without changing their intake,
fail-closed or decision execution adapters. Other projects are not implicitly
approved: global snapshots and MCP lists remain privacy-filtered. Status requires
exactly one listed Work view per named project, followed by identity-checked
`work_get` facts; hidden, duplicate or malformed records fail rather than falling
back to unrelated state.

| Contract / native trigger | Real boundary and oracle | Negative control / retained protection | Checklist |
|---|---|---|---|
| Ordinary `Hello.` with no selectors or slash command | Native input -> global `conversation.submit` -> bound ACP/MCP -> recorded and visible assistant reply; zero Projects/Works | Existing project makes zero-project proof fail; no dashboard/actions in default view | C329 **Agent Center chats globally without configuring a project** |
| Human report goal literally names an owned existing directory | Bound `conversation.propose_action` -> authoritative project preview -> exact native confirmation -> real Project | Enter/Esc create nothing; no automatic Work/start; capabilities/limits cannot exceed approved global policy | C330 **Agent Center creates an execution project only after global conversation approval** |
| Global status question and F7/Esc/F4 work-hint navigation | Actual reads across two projects and correlated assistant reply; same lifetime conversation and global draft | Context hint is not an authority/destination; F5 Enter and dashboard-focused paste cannot send/edit | C331 **Agent Center preserves one global draft across dashboard and work hints** |
| Ten physical dashboard/hint/chat transitions under sustained guarded traffic | Exact global draft/caret plus real selection replacement, actual receipts and phase timings | No idle padding, per-work editor substitution, manual refresh or shortened stall bound | C332 **Agent Center preserves global input through sustained dashboard navigation** |
| Human request targets B, then focus hint changes to A | Real bound proposal -> unchanged frozen request/guards -> native Ctrl+Enter -> only B's version advances | Enter/Esc and preview paste do not commit or retarget; entire request compared structurally | C333 **Agent Center confirms a global proposal without retargeting to another work hint** |
| Global report goal names the owned approved directory without selecting it first | Bound Draft + grant preview + Start proposal -> native frozen approval -> actual provisioning/context-waiting worker | A receipt alone is not running/completed; same proposal nonce survives Esc/reopen | C327 **Agent Center creates an initial brief and explicitly starts the captured work** |

**Retirement:** C331 replaces C321's independent-draft contract; C332 replaces
C323's per-work draft restoration contract. Historical artifacts are retained,
but those two IDs are no longer active checkbox rows or native test titles.
Generate a fresh report from the current checklist before incremental updates;
do not overlay new runs onto a historical report retaining the retired rows.
No legacy UI flag preserves old checkmarks. C319/C320, C322 and C324-C328 retain
their distinct safety/delivery meanings. This is not full J1-J9 qualification,
prototype range editing, a regex language parser, or natural-model autonomy.

C332 retains **at least 180 seconds / 784 successful updates**, four early
transitions, pairs around 60 and 130 seconds, and two immediately after producer
exit. Each F7/F2/Esc transition plus exact editor/caret observation must finish
within **15 seconds**. Around 130 seconds, actual clipboard replacement proves
the same global selection under both work hints before restoring it. The
producer remains active throughout the wait; its total budget remains 240
seconds. Receipts distinguish genuine pressure from post-exit recovery.
Fresh native status messages after navigation prove the current UI still sends
with the original console/conversation identity, rather than merely querying an
old conversation that remains in storage. These probes run outside the measured
transitions and restore the test draft/selection. Status evidence uses complete,
immutable per-source files; the wait excludes all prior filenames. No mutable
latest-file index or overwritten evidence can qualify a new message.
Transient missing/sharing-blocked snapshots retain observed counts and clocks.
Only `IOException` HRESULT `0x80070020` is deferred with a warning; real held-file
tests preserve first-receipt/stall/total limits and propagate other failures.

The distinct C324 raw subscriber must actually receive `RESYNC_REQUIRED`; load
alone does not prove reconnect. C325 still answers a real typed IntakeRequest.
C328 still requires an actual blocking worker ContextRequest, a TaskInput
DecisionRequest and the original bound continuation acknowledgement before
recording Resolved/Applied and captured report bytes. It depends on C327 and
fails explicitly when that prerequisite is missing. C326 still reads actual
fixed report/evidence bytes before separate explicit acceptance and Completed.
Focused Start/continuation retries must select C329, C330, C331, C327 and C328:
C331 performs the owned project setup and populates `decisionProject`. Omitting
C331 is an invalid fixture selection, not evidence of a product failure.
Report writes retain native canonicalization and exact authorized workspace /
ACP-session equality for normal, verbatim, long and existing 8.3 aliases.

Parsers use the literal `Global conversation` scope, optional header-only
`Context hint: ...`, full-width F4/form/confirmation/F12 regions, and actual
composer borders. The optional completion hint is **outside** the closing
border; no style or slash-text heuristic trims real draft data. Historical
frame/Unicode/decoder controls remain offline protections, not legacy UI passes.
F12 verifies frozen protocol identities but is not a required human interaction.
Pending F12 captures require the root `requestUtf8Hex` field, generated by the
product from the actual frozen operation. Only its lowercase hex transport may
have physical row breaks and ASCII padding removed. The reader waits for the
root ending to exclude duplicate fields, validates canonical even-length hex,
strict UTF-8, duplicate-free JSON and the exact four-field request shape, then
preserves decoded strings verbatim. Missing or partial encoding never falls
back to readable data. Strict legacy parsing remains available for older
offline frames; wrapped readable strings are still rejected, never normalized.
Each read preserves `captured-action-*.json` with
expected/observed requests, physical frames/rows and any parsing error before
restoring the preview. The strict four-field comparison is unchanged.
WorkFlow explicitly opts its owned UI into
`WTA_LOG=warn,agent_center::navigation=debug`; configuration/service commands
retain their prior environment, which is restored after the UI exits.
`runtime.json` records this filter. `navigation-log-provenance.json` identifies
the actual `wta-center-ui[.<UTC-date>].log` files discovered only beneath the
owned runtime's LocalAppData, with UI PID/creation time, deployed hash and final
log hashes. The files themselves remain in that fixture directory.
For PageUp/PageDown/F12 only, `navigation-input.jsonl` pairs requested/returned
or thrown records by ID, with requested VK/modifiers, UTC call boundaries,
expected HWND/pane/UI identity, and before/after foreground HWND/PID/thread
plus protocol active-pane observations. Observation failures are recorded and
warned, not treated as proof of focus. The existing keyboard primitive uses
`keybd_event` and returns App: it exposes **no SendInput count or delivery
receipt**. Protocol active pane is not proof of OS child-control keyboard focus;
the existing helpers do not expose that child HWND. The foreground guard,
single key call, modifiers, sleeps, page limits and wait deadlines are unchanged.
`navigation-captures.jsonl` links every raw frame (including unchanged wait
frames) to the most recent navigation ID, capture artifact and UTC interval.
Correlate these with the traced build's `agent_center::navigation` records by
owned UI PID, time and pending command ID; `lastDecodedCommandId` in input
evidence is historical, not an asserted current pending operation. A framework
return alone cannot distinguish missing application ingress from handler behavior.
Proposal cases require actual UI ingestion/opening of HumanActionProposal
records; direct CLI mutation is not a substitute if that integration is missing.
The native run also saves `visual-<boundary>.png`, `.txt`, and `.json` under its
unique fixture directory for `overview`, `current-work`, `typed-intake`,
`fixed-report`, and `post-start-question`. These pair actual owned-window images
with exact captured text and HWND/process/pane/deployed-hash provenance for human
visual review; they do not automatically approve the design. The existing
`Save-UiScreenshot` primitive is HWND-targeted with full-screen capture disabled.
Its opt-in `-RequireSuccess` makes capture-command errors explicit; this suite
also rejects missing, invalid, or reused images and retains failed-capture
metadata. Ownership and the active test pane are checked before capture.
No visual capture is inserted into C332's measured load/navigation intervals.
No scripted worker result may be replaced by a direct database insertion.
The full current-pilot and future adapter gates above remain unchanged.

## What it gives you

### Agent Center native clipboard regression

`Feature.AgentCenterNativePaste` targets **Dev only** and requires
`ITE2E_EXPECTED_WTA_SHA256` from the independently built/deployed feature revision.
Round9's pre-fix foreground paste dropped U+1F469 from `a👩z` (`az` remained),
dropped surrogate codepoints from joined emoji, and concatenated the CRLF payload
`/round9-invalid-one\r\n/round9-invalid-two`, producing `METHOD_UNSUPPORTED`
before Enter. Round8 unit/editor/caret coverage did not cross this clipboard boundary.

The suite launches the **deployed `wta ui` in a test-owned ordinary TermControl**:
OS clipboard → foreground Ctrl+Shift+V → Terminal paste action → ConPTY bracketed
paste → Windows native input decoder → exact rendered draft. It does **not**
validate the dedicated Console's shell-key routing. No `send-keys` text injection,
synthetic VT paste, model prompts, existing works, or prior verification state are used.

| Contract | Trigger / negative control | Oracle / checklist title |
|---|---|---|
| Preserve text and replace the selected draft only | ASCII/BMP baseline; exact `a👩z`; joined `👩‍💻`; Shift+Left selection and Ctrl+A replacement | Exact ordinal draft rows: **Agent Center native paste preserves Unicode and selection replacement** |
| Multiline paste stays atomic until explicit send | Actual CRLF clipboard with two unknown slash commands; redraw observation; physical Shift+Enter between two unknown-command pastes; single unknown command plus physical Enter | Exact separate rows without premature `METHOD_UNSUPPORTED`; unmodified Enter then yields that error and preserves the rejected draft: **Agent Center multiline native paste waits for explicit Enter** |

`selftests\AgentCenterDraft.Unit.Tests.ps1` exercises the same small parser used
by live readiness and draft assertions, entirely offline. Round10 exposed that a
case-insensitive search for `Enter` also matches **Center** in the top product
header. The shared parser now requires a case-sensitive, complete `Enter` token
at the start of the input title (allowing pseudo-locale decoration), complete
bordered draft rows, and the closing border. It accepts clipped/localized help,
rejects unrelated/incomplete frames, preserves leading and non-ASCII whitespace,
and removes only right-hand ASCII cell padding. Its offline self-tests do not credit
C319/C320 as native acceptance; the empty-draft assertion remains exact.

The fixture explicitly configures **zero provider capabilities**, owns a separate
service PID, and redirects only its child processes' `LOCALAPPDATA`/`APPDATA` into
a unique artifact directory. The harness temporarily backs up/restores Terminal
settings/state byte-for-byte, uses a disposable profile and a no-provider
`cmd.exe /d /c exit 0` prewarm command, and restores the OS clipboard from memory.
It refuses to start if any Dev host or earlier configuration backup exists:
**close Dev hosts yourself; the suite never treats them as stale test processes**.
An unlocked desktop must remain dedicated to the test. Missing Dev/hash/foreground
prerequisites fail clearly; an explicitly selected Store run skips this Dev-only suite.
Do not run alongside another settings-mutating suite.

After building/deploying the intended revision separately, from an independent
PowerShell runner:

```powershell
$env:ITE2E_PACKAGE = 'Dev'
$env:ITE2E_EXPECTED_WTA_SHA256 = '<SHA-256 recorded by the feature-build/deployment owner>'
pwsh -NoProfile -File test\e2e\bootstrap.ps1 -Check
pwsh -NoProfile -File test\e2e\Invoke-ItE2EReport.ps1 `
    -Path test\e2e\tests\Feature.AgentCenterNativePaste.Tests.ps1 `
    -OutDir test\e2e\artifacts\agent-center-native-paste-report
```

The first run deliberately generates a **full current-checklist report**. Only
subsequent runs against that output directory should add `-UpdateReport`:
incremental updates cannot insert newly added IDs into an older report. To repair
a stale report **without rerunning UI**, preserve the actual `results.xml`, run
`New-ReleaseReport.ps1 -ResultsXml <actual-results> -OutFile <fresh-report>`,
then apply `Update-ReleaseReport.ps1` to that current-checklist baseline.
A foreground-blocked `BeforeAll` is not a native pass; both IDs must remain
unchecked and carry `AUTOMATION FAILED` when Pester reports those failures.

Retain source revision **and dirty-source manifest**, build/deployment hashes,
the report/results XML, and the unique `agent-center-native-paste-*` artifact
directory (`package.json`, `runtime.json`, sequential frames, final screenshot,
empty before/after work lists, and service logs). `settings-before.json` and
`cleanup.json` record byte-hash restoration and owned PID/start-time identities,
never clipboard contents. Pre-input failures skip keyboard cleanup entirely and
use owned-process teardown; a final work-list capture requires a successful
foreground exit and is absent when setup was blocked. A deployed hash without its
source/build provenance is not proof of the intended fix. Offline discovery and
synthetic report-mapping checks do **not** count as live passes.
Existing `Feature.AgentSelectAll` and `Feature.AgentInputNavigation` protect the
separate helper input path; run them separately with their normal prerequisites,
not as proof of this native decoder. Frozen-confirmation preview paste remains
outside this minimal empty-service suite: it needs a separately owned work fixture.

Three planes, all built on self-verifying primitives:

| Plane | Backed by | Examples |
|-------|-----------|----------|
| **Control** | `wtcli` (COM `IProtocolServer`) | panes/tabs, `Send-WtInput`, `Invoke-RunCommand`, `Get-WtCapture`, `Send-WtEvent` |
| **UI** | `winapp ui` (Windows App CLI) | `Invoke-UiElement`, `Set-UiValue`, `Wait-UiElement`, `Save-UiScreenshot` |
| **State/Logs** | settings.json / state.json / versioned logs / event stream | `Set-WtSetting`, `Get-FreCompleted`, `Get-ItLogText`, `Start-WtEventListener` |

…plus verification oracles: `Assert-Setting`, `Assert-Ui`/`Assert-Xaml`,
`Assert-Script`, `Assert-Pane`, `Assert-WtEvent`, `Assert-Log`, and the AI oracle
`Assert-AI` (LLM judge wrapping an agent CLI's print mode, e.g. `copilot -p`).

## Prerequisites

- Windows, **PowerShell 7+**
- **Windows App CLI**: `winget install Microsoft.WinAppCli` (gives `winapp ui`)
- **Pester 5**: `Install-Module Pester -MinimumVersion 5.5.0 -Scope CurrentUser`
- A deployed Intelligent Terminal package (Store `Microsoft.IntelligentTerminal_8wekyb3d8bbwe`
  or Dev `IntelligentTerminal_rd9vj3e6a2mbr`).
- `Feature.AgentSelectAll` and `Feature.AgentInputNavigation` physical letter-key cases require
  an already loaded English (US) keyboard layout. Each suite activates it only for its own
  verified window/thread and restores the previous layout before closing, so an active IME
  cannot retain the probe text as a composition.

When an action's event is the oracle, start its listener with
`Start-WtEventListener -WaitForReady` before triggering the action. This uses the
subscription handshake rather than a fixed startup delay.

One-shot setup + verify:

```powershell
pwsh -File test/e2e/bootstrap.ps1          # install deps, import module
pwsh -File test/e2e/bootstrap.ps1 -Check   # verify only
```

## Choosing the build: Dev vs Store

Every harness entry point takes a **`-Package`** selector, so a test can target
either the production build or the build you're developing:

| `-Package` | Resolves to | When to use |
|---|---|---|
| `Store` | `Microsoft.IntelligentTerminal_8wekyb3d8bbwe` | The shipped/production package — real user environment. |
| `Dev` | `IntelligentTerminal_rd9vj3e6a2mbr` | A locally **sideloaded** build (e.g. your F5 / `bx` output). Use this to validate a change before it ships. |
| *(explicit PFN)* | the family name you pass | Any other package. |

```powershell
$app = Start-Terminal       -Package Dev    # control/UI tests against the dev build
$app = Start-TerminalFre    -Package Store  # drive the FRE overlay on the store build
```

**Both builds can be installed at once and targeted independently.** The harness
launches via **AUMID** (`shell:AppsFolder\<PackageFamilyName>!App`), which is
package-specific, so `-Package Dev` always hits the dev build even while the
store build is also installed. (The global `wtai` AppExecutionAlias is owned by a
single package and is therefore ambiguous in that scenario — it is kept only as a
last-resort fallback.)

To make a build selectable:
- **Dev**: build + deploy it once, e.g. `cd src/cascadia/CascadiaPackage; bx` then
  `DeployAppRecipe.exe bin\x64\Debug\CascadiaPackage.build.appxrecipe`.
- **Store**: install the shipped MSIX.

A suite that asserts on diagnostics only present in a particular build should pin
its `-Package` and **`-Skip`** itself when that package isn't installed (see
`Feature.FreExecutionPolicy.Tests.ps1`, which targets `Dev` and skips when the
dev package is absent — keeping CI green on machines that only have the store build).

## Running the self-tests

```powershell
Import-Module Pester
Invoke-Pester test/e2e/selftests -Tag Unit    # hermetic, no terminal needed
Invoke-Pester test/e2e/selftests -Tag Live    # launches/closes the real terminal
Invoke-Pester test/e2e/selftests -Tag AI      # AI oracle (needs an agent CLI, e.g. copilot)
Invoke-Pester test/e2e/selftests -Tag Agent   # agent pane + autofix (needs copilot auth)
Invoke-Pester test/e2e/selftests              # everything (30 tests)
```

The self-tests are the framework's own proof: every primitive is exercised against a
running terminal (`selftests/ItE2E.Live.Tests.ps1`) and the core helpers are unit-tested
in `selftests/ItE2E.Unit.Tests.ps1` (hermetic, no terminal needed).

## Pane-context performance benchmark

`Measure-PaneContext.ps1` measures issue #838's **wtcli subprocess → COM →
capture** boundary, without sending an agent prompt or consuming model tokens.
It attaches to **already running**, explicitly selected Dev/Store/PFN packages and
existing pane GUIDs. It never launches/closes Terminal, changes settings/focus,
creates fixtures, or types into panes. Package-local binaries and a readable
package manifest are required; it does not use an ambiguous `wtcli` PATH alias
or probe other brands' COM servers. Dependencies: Windows, PowerShell **7.2+**,
Git, and a deployed package supporting `get-pane-context`. Neither WinApp CLI,
agent authentication, nor Pester is needed for the benchmark itself.

First prepare stable terminal output yourself, and obtain the existing pane's
`session_id` through the harness/package-specific `wtcli`. Run the benchmark
from a **separate process/pane**, not the pane being measured. Leave its content,
focus, window/tab layout, and package binaries unchanged until completion:

```powershell
$env:ITE2E_PACKAGE = 'Dev'
pwsh -NoProfile -File test\e2e\bootstrap.ps1 -Check

# Replace the GUID and marker with those of an existing, settled marked pane.
# Planner/ManualFix require this to remain the resolved active working pane.
pwsh -NoProfile -File test\e2e\Measure-PaneContext.ps1 `
    -Package Dev -Configuration Debug -Mode Planner `
    -TargetPaneId '11111111-2222-3333-4444-555555555555' `
    -Scenario 'marked-short' -ExpectedMarks Marked -ExpectedMarker 'BENCH-DONE' `
    -Warmup 5 -Samples 40 -OutDir test\e2e\artifacts\pane-context-benchmark\debug-marked-planner

# ExplicitAutofix can target an unfocused pane; no focus change is performed.
pwsh -NoProfile -File test\e2e\Measure-PaneContext.ps1 `
    -Package Dev -Configuration Debug -Mode ExplicitAutofix `
    -TargetPaneId '11111111-2222-3333-4444-555555555555' `
    -Scenario 'unmarked-long-scrollback' -ExpectedMarks Unmarked `
    -OutDir test\e2e\artifacts\pane-context-benchmark\debug-unmarked-autofix
```

`-Package`, `-Configuration` (a label, not a build action), `-Mode`,
`-TargetPaneId`, `-Scenario`, and `-OutDir` are mandatory; `Auto` is rejected.
`ExplicitAutofix` also accepts an array of existing pane IDs when called from
PowerShell with `& .\test\e2e\Measure-PaneContext.ps1 ... -TargetPaneId @($id1, $id2)`.
Each pane gets its own paired measurements. Use distinct output directories
under ignored `test\e2e\artifacts`; existing result files are never overwritten.
Run marked, unmarked, and long-scrollback scenarios separately and label them
honestly. For optional `-ExpectedMarker`, choose text that survives **both**
paths' intentional bounds.

### Baseline and interpretation

- The baseline is a source-faithful PowerShell reproduction of the collector at
  `db609f8061f81c2eb9a4bdaf3e0666392596bce4` (HEAD when #838 was restored),
  pinned in the script. Its `prompt_context.rs` is retrieved with `git show` for
  provenance. **That planner already used marks**, not a buffer-only read.
- `Planner`: `active-pane` → `capture-pane --last-prompt` → optional
  `capture-pane -l 24`. `ManualFix` uses the same sequence with 30 fallback lines.
  `ExplicitAutofix` preserves the old **unconditional active-pane query**, then
  walks windows → tabs → panes until the exact source GUID is found, followed by
  marked capture / 30-line fallback. Enumeration cost depends on target position
  and topology. Errors abort instead of being silently turned into samples.
- No unsupported-capability request is added to the legacy baseline. The new
  path uses **one** `get-pane-context --max-lines 24|30 --max-chars 4000`
  subprocess (with `--target` only for explicit autofix). Its normal
  authentication/capability negotiation remains inside that subprocess.
- Legacy read methods still capture first and trim locally; the benchmark does
  not retrofit bounded capture into them. Both paths use a 4000-Unicode-scalar
  content budget, but **legacy marked output has no line cap**, while new marked
  output also observes 24/30 lines. For oversized unmarked output, new capture
  keeps a scalar **tail** versus legacy's scalar **prefix** of its line tail.
  Legacy also ignores the read result's truncation flag. WTA's
  `\n...<truncated>` prompt suffix is outside the content budget. Therefore exact
  cross-path payload equality is **reported, not asserted**.
- The same transport runs both paths: `.NET ProcessStartInfo.ArgumentList`,
  no shell, UTF-8, concurrent asynchronous stdout/stderr reads, closed stdin,
  normal authentication and a shared per-command timeout (default 20 seconds).
  The existing harness also uses asynchronous process waits, not polling; the
  benchmark-specific transport adds precise timing and one deadline covering
  both process exit and pipe EOF, and fails loudly on parse/read/exit errors.
- Each pane runs at least five warmup pairs, then at least 40 recorded pairs,
  alternating legacy-first/new-first order. Primary `BoundaryMs` is the **sum
  of process-start-to-exit-and-EOF durations**, excluding PowerShell parsing,
  assertions and report writes. Secondary `CollectorMs` includes PowerShell
  emulation overhead and is **not** a native Rust collector measurement.
  Warmups are exported but excluded from statistics. p50/p95 use nearest rank;
  speedup is `legacy/new`, reduction is `100*(1-new/legacy)`, including regressions.
- Output must resolve to the requested pane with matching shell/cwd/process
  metadata, valid Unicode/bounds, consistent mark/source metadata, and an optional
  literal marker. Each path's payload/metadata fingerprint must stay stable.
  Different payload hashes across paths may be expected from the bounds above.

Artifacts are `samples.csv` (raw paired samples, bytes, hashes, marks, bounds),
`requests.csv` (individual subprocess commands/timings/response bytes),
`metadata.json` (written before measurement), and `summary.json` (written **only
after complete validation**). Summaries include per-path request counts,
nearest-rank p50/p95, speedups/reductions, byte metrics, payload equality,
package version/CLSID, deployed binary hashes/versions, process identity, source
revision/dirty status/diff hash, and benchmark source hashes. Raw terminal text
is not saved, but pane IDs/paths and the optional marker are; keep artifacts local.
A failed run may retain partial CSVs but has no success summary.

**Scope:** this isolates old versus new **context collection on the same new
server**, not old/new application binaries and not full prompt/LLM latency.
Subprocess counts are not COM-call counts. Configuration labels and source
hashes alone cannot prove a deployment came from that revision: the operator
must build/deploy the intended code and compare packaged binary hashes.
Debug and Release results are not interchangeable. Busy UI threads, antivirus,
background output and changing topology can affect measurements; repeat runs.

Hermetic benchmark tests (no app launches or installs):

```powershell
Invoke-Pester test\e2e\selftests\PaneContextBenchmark.Unit.Tests.ps1 -Output Detailed
```

## Reports (HTML + precise per-failure diagnostics)

`Invoke-ItE2EReport.ps1` wraps Pester and, by default, writes the report to the **fixed
in-repo path `test/e2e/artifacts/`** (override with `-OutDir`; the dir is git-ignored):

```powershell
pwsh -File test/e2e/Invoke-ItE2EReport.ps1                 # full suite -> test/e2e/artifacts/
pwsh -File test/e2e/Invoke-ItE2EReport.ps1 -Tag Feature
pwsh -File test/e2e/Invoke-ItE2EReport.ps1 -Path test/e2e/tests/Feature.AutofixPane.Tests.ps1
```

Outputs (all under `test/e2e/artifacts/`):
- `report.html` — **self-contained HTML** (open in a browser): green/red pass-fail banner,
  total/passed/failed/skipped stat cards, one **failure card** per failed test (exact error,
  `file:line` of the failing assertion, duration, clickable artifact links + inline screenshot
  thumbnails), and a full results table grouped by `Describe > Context`.
- `results.xml` — **NUnit XML** for CI test reporting (Azure DevOps / GitHub).
- `summary.md` — Markdown: one block per **failed** test with the **exact error**, **file:line**,
  and any **artifact paths** (screenshots saved by `Assert-Ui`/`Assert-AgentPaneText`, log slices).
- `release-report.md` — the **clean, jargon-free release checklist**, auto-generated as the final
  step from `doc/release-check-list.md` + this run's `results.xml` (via `New-ReleaseReport.ps1`).
  Every coverage tag (`[UT✓]`/`[E2E]`/`[MANUAL]`) and `_(UT: …)_` note is stripped, and each box is
  driven purely by automation: **`[x]`** = a test passed, **`[ ] ⚠️ AUTOMATION FAILED`** = a test ran
  and failed, plain **`[ ]`** = not covered this run, verify manually. Suppress with
  `-SkipReleaseReport`; regenerate standalone from an existing `results.xml` with
  `pwsh -File test/e2e/New-ReleaseReport.ps1`. Items listed in `test/e2e/release-exclude.psd1`
  (by title regex, e.g. RTL) are dropped from the report to keep it focused on the sign-off set.

  **Stable item IDs (`C001`, `C002`, …).** Every checkbox item in `doc/release-check-list.md`
  carries a stable ID right after the box, and the generators carry it verbatim into
  `release-report.md` — so you can refer to a case by number ("C136 is failing") and it means the
  same item in both files. Assign/refresh IDs with `pwsh -File test/e2e/Set-ChecklistIds.ps1`
  (idempotent: existing IDs are never renumbered; a newly-added item gets the next free number).

  **Incremental update (no full-suite re-run needed).** `New-ReleaseReport.ps1` regenerates the
  whole report, so a single-suite run would blank every item it didn't cover. To refresh just the
  rows a partial run touched, use `Update-ReleaseReport.ps1`, which takes the EXISTING
  `release-report.md` as the source of truth and overlays only this run's results: a covered item
  that **passed** becomes `[x]`, one that **failed** becomes `[ ] ⚠️ AUTOMATION FAILED`, a covered
  item that only **skipped** is left unchanged (a flaky skip never un-ticks a prior pass), and every
  item **out of scope** for the run is preserved exactly. One-liner via the runner:
  `pwsh -File test/e2e/Invoke-ItE2EReport.ps1 -Path test/e2e/tests/Feature.Delegate.Tests.ps1 -UpdateReport`
  (runs the suite, then overlays only its items onto the existing report; falls back to a fresh
  generate if no report exists yet). Or standalone after a run wrote `results.xml`:
  `pwsh -File test/e2e/Update-ReleaseReport.ps1`.
- Console echo of the same precise failures; exit code `1` on any failure (CI-friendly).

Every failure is precise because each `Assert-*` throws a descriptive message — e.g.
`Assert-Pane: pane <id> never matched /git status/ within 12s. Screenshot: <path>` or
`Assert-AI FAILED: '<claim>' -> <reason> (confidence=0.7)` — and Pester records the exact
`Should` line and `file:line`.
(`selftests/ItE2E.Unit.Tests.ps1`, incl. a regression test for output truncation).

## Authoring a test

```powershell
Describe 'Agent pane' -Tag 'Live' {
    BeforeAll {
        Import-Module test/e2e/ItE2E/ItE2E.psd1 -Force
        $script:app = Start-Terminal -Package Store -Settings @{ acpAgent = 'copilot' }
    }
    AfterAll { Stop-Terminal -App $script:app }   # restores settings/state

    It 'opens the agent pane from the bottom bar' {
        Open-AgentPane -App $script:app
        Assert-Ui -App $script:app -Selector 'AgentToggleButton'
        Test-AgentPaneOpen -App $script:app | Should -BeTrue
    }
}
```

`Start-Terminal` requires an explicit package, backs up `settings.json`/`state.json`, marks the
FRE complete, applies your settings, launches the app, brings COM online (probes the
per-brand `WT_COM_CLSID`), and resolves the window HWND. `Stop-Terminal` closes it and
restores the backup.

> **Picking the build**: pass `-Package Dev` / `-Package Store` — see
> [Choosing the build](#choosing-the-build-dev-vs-store). Launch is package-specific
> (AUMID), so both builds can be installed and targeted independently. The feature/self
> -test suites don't hardcode a build — they call `Start-Terminal -Package (Get-ItTestPackage)`,
> which requires the `ITE2E_PACKAGE` env var (`Store`|`Dev`|`<PackageFamilyName>`).
> Set `$env:ITE2E_PACKAGE='Dev'` or `$env:ITE2E_PACKAGE='Store'` before invoking
> live tests. `Auto` is rejected so the harness cannot select a package implicitly.


## How it works (key facts)

- **COM discovery**: `wtcli` reaches WT through the per-brand CLSID in `WT_COM_CLSID`
  (braced, e.g. `{A2E4F6B8-...}` for Release). The harness probes the four brand CLSIDs
  against a *running* terminal until one connects (the server is registered with
  `CoRegisterClassObject(CLSCTX_LOCAL_SERVER)`, so WT must already be up). The co-located
  `wtcli.exe` in the package install dir connects fine without needing the AppExecutionAlias.
- **FRE**: completion is the `agentFreCompleted` flag in the shared `state.json`;
  `Invoke-FrePass` sets it instantly.
- **Settings**: the AI keys (`acpAgent`, `autoFixEnabled`, `agentPanePosition`,
  `aiIntegration.coordinator.enabled`, …) are *top-level* properties whose names contain
  dots. `Set-WtSetting` patches them and waits for the on-disk write.
- **UI selectors**: prefer XAML `AutomationProperties.AutomationId` (confirmed present:
  `AgentToggleButton`, `SessionToggleButton`, `NewTabButton`, `NextButton`, `SaveButton`).
  `winapp ui` also accepts generated slugs and plain text.
- **Agent pane** is a XAML `AgentPaneContent` area — **NOT** a wtcli/protocol pane (it does
  not appear in `list-panes` and has no protocol session_id). Detect it by the UI element
  `AgentLabelText` (`Test-AgentPaneOpen`), open/close it via the `AgentToggleButton`.
- **Events**: `Start-WtEventListener` runs `wtcli listen --json` and buffers events. The
  envelope is `{ "method": "<name>", "params": {...}, "type": "event" }` — the event **name
  is `.method`** (`vt_sequence`, `agent_event`, …), and `.type` is *always* `"event"`. Start
  the listener *before* the triggering action, then `Wait-WtEvent`/`Assert-WtEvent`.
- **Autofix signals**: a failed command emits `method=vt_sequence, params.sequence ~
  "osc:133;D;<nonzero>"` (`Wait-WtCommandFailure`); autofix then submits a prompt observable
  as `method=agent_event` whose `params.payload.initial_prompt` contains "A command failed.
  Diagnose…" — note this rides on the `agent.session.start` sub-event, not `agent.prompt.submit`
  (`Wait-Autofix`). This build emits no dedicated `autofix_state` event. Autofix **de-dupes
  repeated identical failures**, so tests use a unique bogus command each time.

## Limitations

- **`Get-WtSessions`** runs `wta.exe`, but the *packaged* `wta.exe` cannot be launched by
  an external process (Access denied) and an *unpackaged* copy resolves the wrong
  (non-package-private) runtime paths, so it can't find the in-package master. This
  feature needs to run inside a WT pane with package identity; it's gated behind `-Tag
  Live`.
- The **AI oracle (`Assert-AI`)** wraps an agent CLI's non-interactive print mode
  (`copilot -p`, `claude -p`, …) **directly** — it is independent of wta and needs only an
  authenticated agent CLI on PATH (override with `$env:ITE2E_AI_AGENT`). Gated behind
  `-Tag AI`.
- Multiple WT windows of the same package share one process (single-instance
  `WindowEmperor`); the harness targets by PID + HWND.

## Layout

```
test/e2e/
  bootstrap.ps1                 install/verify deps, import module
  ItE2E/
    ItE2E.psd1 / ItE2E.psm1     manifest + loader
    Private/  Core.ps1          Invoke-Native, Wait-Until, JSON, logging
              Paths.ps1         Resolve-ItApp, CLSID probe, runnable-wta
    Public/   Harness.ps1       Start-Terminal / Stop-Terminal / Reset-TerminalState
              Wt.ps1            panes/tabs/input/capture/events (wtcli)
              Settings.ps1 Fre.ps1  settings.json / state.json
              Ui.ps1            winapp ui wrappers
              Agent.ps1 Autofix.ps1 Sessions.ps1
              Observe.ps1       logs / event stream / context bundle
              Verify.ps1        Assert-* oracles
  selftests/  *.Tests.ps1       Pester proof for every primitive
  tests/                        your feature scenario tests go here
```
