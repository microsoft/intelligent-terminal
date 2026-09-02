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
| `Feature.FreExecutionPolicy.Tests.ps1` | §0 FRE execution-policy verdict (deterministic via registry; **Dev**, auto-skips) | 3 (1 conditional skip) |
| `Feature.AgentPaneInteraction.Tests.ps1` | open/hide/focus, input/rendering, slash, Copilot chat | 14 |
| `Feature.AgentProtocolExperience.Tests.ps1` | PRs #599/#601/#606/#610/#611/#612/#616/#634/#683: intent-based terminal actions (including empty workspaces and configured delegation), ACP tool/transcript rendering, clarification input, session configuration, model title, and replacement cleanup across the deployed helper/master boundary | 8 |
| `Feature.AgentImageAttachmentEditing.Tests.ps1` | PR #536: inline image tokens move and delete atomically while preserving adjacent prompt text | 1 |
| `Feature.AgentModelSync.Tests.ps1` | PR #538: ACP config-option updates replace stale session model state in the active picker | 1 |
| `Feature.AgentModelLifecycle.Tests.ps1` | PR #554: `/model` hot-apply and Settings-driven model restart/reconnect lifecycle | 2 |
| `Feature.ByokProvider.Tests.ps1` | PR #447: Settings-selected OpenAI-compatible provider request path, credential handling, and BYOK-to-cloud restart lifecycle | 2 |
| `Feature.AgentCompactLayout.Tests.ps1` | PR #580: compact-height recommendation, input, and Insert interaction at the real splitter minimum | 1 |
| `Feature.ProposalMcpRouting.Tests.ps1` | PR #560: per-session proposal MCP names and two-tab Helper routing isolation | 1 |
| `Feature.AgentMouse.Tests.ps1` | PR #506: chat wheel scrolling, draft preservation, text selection/copy, and stale-selection suppression; completed-turn full-row clicks across multiline prompts with shared keyboard selection/Enter behavior, row-end/drag guards, and input-dialog focus recovery | 4 |
| `Feature.PromptHistory.Tests.ps1` | PR #478: per-tab Up/Down prompt recall, draft restoration, and multiline preservation; PR #614: completed-turn collapse/expand rendering | 4 |
| `Feature.CompletedTurnSelection.Tests.ps1` | Completed-turn Tab/Up/Down selection keeps focused history inside the chat viewport | 1 |
| `Feature.AutofixPane.Tests.ps1` | Direct Helper Autofix proposal card render/insert/run/reject/target/stashed + across layout | 10 |
| `Feature.AutofixParser.Tests.ps1` | issue #474: PowerShell ParserError-to-Autofix pipeline + success/handled-error/blank-input negative controls | 4 |
| `Feature.CommandResolution.Tests.ps1` | PR #418: packaged WTA resolves PowerShell profile-only aliases to their real targets | 1 |
| `Feature.SessionList.Tests.ps1` | session view (button + `/sessions` slash), session states, view switching (incl. draft-preservation), focus/restore | 13 (+1 skip) |
| `Feature.NonAsciiCwd.Tests.ps1` | issue #641: a non-ASCII starting directory survives `wtcli` argv → COM → `CreateProcessW`, so the resume launch path connects and starts in that directory | 2 |
| `Feature.AgentPaneCwd.Tests.ps1` | agent-pane source workspace reaches ACP `session/new` and remains stable across `/new` without a model prompt | 1 |
| `Feature.AgentRestart.Tests.ps1` | agent restart after a settings change (/restart reconnects and answers) | 1 |
| `Feature.ShellIntegration.Tests.ps1` | §3 shell-integration OSC 133 marks (success/failure, ParserError dedup, handled errors, WinPS 5.1 errors) + non-integrated cmd.exe safety | 6 |
| `Feature.BashPromptIntegration.Tests.ps1` | PR #468: Bash `PROMPT_COMMAND` PS1 rewrites preserve D/A/B boundaries; non-IT hosts remain gated | 1 (Git Bash-gated) |
| `Feature.AgentProposedCommand.Tests.ps1` | §2 Direct Helper Proposal Insert/Run into the shell pane | 2 |
| `Feature.AgentProposalFocus.Tests.ps1` | PR #533: Insert returns real window keyboard focus to the target shell pane | 1 |
| `Feature.AgentMatrix.Tests.ps1` | §2 non-Copilot built-in agents (Claude/Codex/Gemini) connect+chat through the ACP adapter — ONE consolidated case (Copilot is the in-depth suite); skips when none installed+authed | 1 |
| `Feature.HookTrace.Tests.ps1` | C190 + PR #571 C267-C269, C272: every shipped bundle's guarded command still delivers, `tool_input` survives only for interactive prompts, shells outside Terminal are ignored, and the broadcast envelope stays inside its budget | 5 |
| `Feature.SessionHookRouting.Tests.ps1` | PR #761: one `wtcli agent-hook` broadcast applies once across the real multi-helper fan-out (with independent dedupe per expansion slot), a terminal hook for an unseen session fabricates no row, and `agent.error` still records the failure | 3 |
| `Feature.HookBridgeCli.Tests.ps1` | PR #571 C274, C265, C266: a real agent CLI fires the bundled `hooks.json` command through its own shell, and neither an unreachable protocol server nor an uninstalled Terminal blocks the CLI; skips when the CLI isn't installed+authed | 3 (environment-gated) |
| `Feature.LegacyHookBundle.Tests.ps1` | PR #571 C270-C271: a pre-#571 PowerShell hook bundle still delivers against a post-#571 Terminal, and degrades quietly when `WT_COM_CLSID` is unset | 2 |
| `Feature.OpenCodeHookBridge.Tests.ps1` | PR #571 C273: OpenCode's JS plugin spawns `wtcli` through an argv array with no shell, so it resolves the bridge via `WTCLI_PATH` rather than the `PATH` alias | 1 (environment-gated) |
| `Feature.OpenCodeAgent.Tests.ps1` | PR #458: built-in OpenCode launches its native ACP server and completes agent-pane chat | 1 (environment-gated) |
| `Feature.OpenCodeSessionResume.Tests.ps1` | PR #464: OpenCode history discovery and `--session` resume restore the prior transcript | 1 (environment-gated) |
| `Feature.OpenCodeHooks.Tests.ps1` | PR #476: packaged hook install, shell-session lifecycle routing, picker visibility, and ACP duplicate suppression | 1 (environment-gated) |
| `Feature.SharedAgentLifecycle.Tests.ps1` | PR #425 + ACP cleanup: closing a tab mid-turn physically closes only its session without terminating the shared agent CLI or breaking sibling tabs | 1 |
| `Feature.PerTabAgent.Tests.ps1` | C225-C228 + PR #487: `/agent` picker/direct selection, invalid-id safety, per-tab isolation/shared-master reuse, and global-default/override behavior | 7 |
| `Feature.WslAgentBackend.Tests.ps1` | PR #481 profile-scoped WSL agent backend: settings hot reload, helper/master source routing, and authenticated chat | 2 (environment-gated) |
| `Feature.DelegateSource.Tests.ps1` | PR #488 profile-scoped delegate source: strict host/WSL `wta delegate` routing with no fallback in either direction | 2 (environment-gated) |
| `Feature.AgentChat.Tests.ps1` / `Feature.AgentPopup.Tests.ps1` | agent chat + `/` popup/menu interaction | 1 + 3 |
| `Feature.AgentPaneMove.Tests.ps1` | PR #429: `/move` stays per-tab, preserves global position, and restores agent input focus | 1 |

**Coverage: 141 of 143 automatable `[E2E]` checklist items are implemented.**
**Test status: 126 baseline feature cases pass + 3 documented skips** (`wta sessions list` is
identity-gated — see `Feature.SessionList.Tests.ps1`), plus 2 PR #481 WSL-backend cases and 2
PR #488 delegate-source cases that run only when a runnable distro (and, for the #481 chat
case, an installed+authenticated native agent) is available. The 141 implemented checklist
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

## What it gives you

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
| `Auto` *(default)* | First fully-resolvable of Store → Dev | Most feature suites; picks whatever is installed. |
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

`Start-Terminal` resolves the package, backs up `settings.json`/`state.json`, marks the
FRE complete, applies your settings, launches the app, brings COM online (probes the
per-brand `WT_COM_CLSID`), and resolves the window HWND. `Stop-Terminal` closes it and
restores the backup.

> **Picking the build**: pass `-Package Dev` / `-Package Store` (default `Auto`) — see
> [Choosing the build](#choosing-the-build-dev-vs-store). Launch is package-specific
> (AUMID), so both builds can be installed and targeted independently. The feature/self
> -test suites don't hardcode a build — they call `Start-Terminal -Package (Get-ItTestPackage)`,
> which honors the `ITE2E_PACKAGE` env var (`Auto`|`Store`|`Dev`|`<PackageFamilyName>`)
> and defaults to `Auto`. So on a dev-only machine the suites resolve to the sideload
> build automatically; set `$env:ITE2E_PACKAGE='Store'` to pin them to the store build.


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
