# ItE2E — Intelligent Terminal End-to-End Test Framework

A robust, CLI-composition test framework that drives and verifies a **deployed
(MSIX-packaged)** Intelligent Terminal. Tests are authored in **PowerShell + Pester 5**.
Design rationale is captured in the inline notes below and in each suite's header comments.

## Parallel worktree Dev verification

To test without replacing another worktree's Dev package, follow
[`doc/dev-worktree-package.md`](../../doc/dev-worktree-package.md).
It uses a separate manifest template, temporary local edits, and this existing
harness. Pin the exact package family and its own CLI paths as described there.
Normal Dev/Store behavior and pipeline configuration stay unchanged.

Separate apps can use HWND-scoped UIA actions; shared foreground input,
clipboard, policy, and agent configuration still need serialization. Check the
suite's requirements rather than assuming all UI tests are parallel-safe.
The live smoke proved separate hosts, package-local CLIs, and sidebar search
invoke/filter/clear. It did not validate concurrent full suites or mark release
checklist items complete.

## Release-checklist coverage

### Startup and failure ownership

`Stop-AppInstances` and `Stop-StaleItInstances` prepare a cold start by
automatically closing **only** processes verified against the exact Dev package
family, installation path and package identity. The current chat ancestry is
never a target, even if it belongs to Dev. An active non-Dev Intelligent Terminal
package remains protected, and ordinary Windows Terminal is not an ItE2E target.
Unknown process identities fail closed. Shutdown requires a one-second quiet
window, rechecking all package executables every 200 ms and retrying bounded
late respawns. Suites that read or change configuration
before `Start-Terminal` use this same Dev-only cleanup rather than requiring the
developer to close a pre-existing Dev window. `Stop-Terminal` still uses the
captured creation-proof process and descendants only; cleanup after a test must
confirm package inactivity before restoring configuration. `Start-Terminal` owns
backup recovery; outer catches must not add package-wide shutdown or unconditional
restoration.

The `tests/` folder implements the `[E2E]` items from
`doc/release-check-list.md` that are automatable on one machine. Copilot drives
the baseline suites, while the agent matrix covers other installed and
authenticated ACP agents. Available suites (results depend on the selected package and revision):

| Suite (file) | Covers | Cases |
|---|---|---|
| `Feature.Packaging.Tests.ps1` | §9 packaging/protocol (incl. WT_COM_CLSID injected into pane shells) + §10 logging + log retention/cleanup | 18 |
| `Feature.HookShutdown.Tests.ps1` | Fixed-CLSID native/cached hook delivery, passive WTA publisher/listener shutdown suppression, and ordinary headless COM compatibility; no windows, agents, or configuration edits | 4 |
| `Feature.TelemetryFunnels.Tests.ps1` | Opt-in real ETW: daily activity, correlated prompt/completion, detection/offer/Run, foreground palette visits, startup/provider configuration and policy state; Run results remain unknown, not execution success | 18 required + 2 optional hot-policy diagnostics (requires `ITE2E_TELEMETRY=1` and explicit policy approval) |
| `Feature.SidebarTelemetry.Tests.ps1` | Opt-in typed ETW: sidebar actions, real tab-order pin/unpin, KeepId/AttemptId restoration and surviving observation sessions, Launch/UserChange field snapshots, raw-provider-ID exclusion and negative controls | 10 (requires `ITE2E_TELEMETRY=1`; no policy changes) |
| `Feature.WtcliPublishStdin.Tests.ps1` | PR #652: WTA/wtcli stdin transport delivers command-line-limit-sized events intact and preserves positional compatibility | 3 |
| `Feature.Settings.Tests.ps1` | §1 Settings>AI Agents + §0 FRE settings/positions/auto-error/session-mgmt | 18 |
| `Feature.SettingsUi.Tests.ps1` | Live Settings editor: Agent controls, Appearance's localized Tab Mode label matching FRE, and Arabic/Hebrew/mirrored/English layout and footer actions; C409 requires the complete matrix | 9 |
| `Feature.FreFlow.Tests.ps1` | §0 FRE overlay click-through (Next→Save, privacy link, close-safety) plus topmost Tab Mode, Sidebar default, explicit preferences, Save-only persistence, setup failure/retry and restart | 9 (failure injection requires Dev) |
| `Feature.FreExecutionPolicy.Tests.ps1` | §0 FRE automatic CurrentUser execution-policy remediation (**Dev**, auto-skips) | 4 (1 conditional skip) |
| `Feature.FreHooks.Tests.ps1` | §0 FRE progressive setup ordering, session hook installation, failure, and retry (**Dev**, auto-skips) | 3 |
| `Feature.SidebarTabKeyboard.Tests.ps1` | Issue #1045: physical Tab/Up/Down navigate unfiltered and filtered Sidebar tabs without terminal focus; bare Enter activates, Ctrl+Enter does not, pointer selection still works, and Ctrl+Shift+S entry/exit preserves the originating shell while Tab visits a row | 3 |
| `Feature.SidebarSessionScroll.Tests.ps1` | Real shell hooks update the same visible Agents row through Idle/Active/Waiting for input/Idle without changing the search query or scrolling unchanged history order; status filters and genuine activity-time reordering still update. All three cases gate C374. Deterministic seeded rows, no model quota or test settings changes; teardown closes only recorded fixture pane GUIDs in the verified logical window after rechecking HWND/PID, never the shared process or unrelated windows/tabs. Original state is recovered only after the package is inactive, even on screenshot/settings failure; unrelated package activity or unconfirmed ownership/inactivity retains backups and fails explicitly. Changed, missing or unreadable settings and their recovery backup are retained and fail explicitly. Existing Dev is closed during preflight; Sidebar mode, completed FRE, and exact-build App/WTA hashes are required | 3 |
| `Feature.PaneProgress.Tests.ps1` | PR #1043: one-shot OSC progress across real tab right-click moves/layout round trips, shared group-chevron/icon slot and aligned top-level titles, layout-specific Move submenu order/direction, and fixture-owned native hook identity/icon restoration with OSC3/OSC0; six-frame rendered ring evidence, no model quota | 4 checklist cases plus literal one-shot coverage (explicit Dev, exact-source `ITE2E_EXPECTED_APP_SHA256` / `ITE2E_EXPECTED_WTA_SHA256`, automatic Dev preflight and interactive desktop required) |
| `Feature.PinnedTabSelection.Tests.ps1` | PRs #1043/#1052: the primary two-pinned-plus-one-ordinary Horizontal/Sidebar round trip first verifies Alpha's active shell, exactly one selected Alpha Sidebar row, terminal focus, canonical order, shell identities and retained pin menus. A separate visual round trip verifies canonical accessibility labels and matched same-profile title-leading offsets; Beta unpin removes its extra Sidebar slot and keeps first-ordinary positioning. FontIcon peers are diagnostic only. A passing test credits C372's automated selection, identity, accessibility and geometry contracts. Full-header compositor crops and `acceptance.json` leave actual Sidebar glyph presence/Horizontal absence pending independent sign-off under the separate C373 MANUAL item **Pinned tab glyphs render only in Sidebar**, which has no automated coverage mapping | 1 (deterministic ACP fixture, no model quota; explicit Dev, exact-source `ITE2E_EXPECTED_APP_SHA256` / `ITE2E_EXPECTED_WTA_SHA256`, automatic Dev preflight and interactive desktop required) |
| `Feature.CombinedAgentsSidebar.Tests.ps1` | Updated Sidebar UX: static Tabs text; standard independent Agents only/Recent agent sessions toggles; four rendered scope combinations; global search; clear/close and collapse-preference restoration; toggle-during-search; UIA checked states and foreground-gated Ctrl+Shift+G/R. Only visible owned ItemsList descendants count. Existing native CLI resume (Recent preference On/Off), live/group actions, metadata, identity, retention, ownership, scroll, expansion events, focus, mutations and resize protections remain. C367/C381/C397/C408 retain their IDs; C382/C383 remain retired. Explicit feature-head Dev hashes and automatic verified Dev preflight required; deterministic ACP history/provider-labelled cmd tabs, no model quota. Fresh hash-backed settings/state/runtime snapshots restore only after package inactivity. Authored, not live accepted | 32 |
| `Feature.AgentsModeActions.Tests.ps1` | Original plus opens the configured default profile across filters, search and layout; splits retain the source profile without view-specific delegation, assistant panes stay fixed, and explicit native-provider fixtures retain held-hook identity; exact Dev hashes, no provider prompts | 7 |
| `Feature.SidebarRelativeTime.Tests.ps1` | PR #1070: six-unit English/Arabic compact ages, readable RTL geometry, and provider accessibility; deterministic history fixture | 2 |
| `Feature.SidebarUpgrade.Tests.ps1` | Superseding PM/UX: persisted once-only Horizontal migration and later explicit Horizontal restart, independently pending introduction, absent flags/fresh FRE gate, real palette collapse deferral, exact-shell owned second-window suppression, actual rendered tip/restart suppression; real state.json sharing fault, visible warning, Horizontal memory/disk rollback and released-lock retry. No ACL/registry changes, provider quota or callback fault proxies; exact Dev hashes and automatic Dev preflight required | 6 (authored, not live accepted) |
| `Feature.SidebarProviderAppearance.Tests.ps1` | PR #1070: horizontal-first provider creation, native identity/layout preservation, and scoped Light/Dark header evidence; rendered foreground requires independent visual review | 2 |
| `Feature.McpDelegatedAgentIdentity.Tests.ps1` | PR #1070: canonical provider bootstrap and identity-only hot updates across session MCP, helper, and native creation; controlled agent fixtures and ordinary-shell negative control | 1 |
| `Feature.AgentPaneInteraction.Tests.ps1` | open/hide/focus, input/rendering, slash, Copilot chat | 14 |
| `Feature.AgentHotkeys.Tests.ps1` | Physical WT-window accelerators for agent pane/delegation; History navigation preserves search-off state and exact shell/Agent input focus, while explicit shared-search entry retains search-focus baselines and existing on-state/query. Sidebar hotkeys preserve drafts and tab-search focus, including keyboard focus on the titlebar rail toggle. The public palette action retains visibility toggling; mixed pointer sessions, horizontal suppression, and effective Expand/Collapse hints remain covered | 14 |
| `Feature.AgentProtocolExperience.Tests.ps1` | PRs #599/#601/#606/#610/#611/#612/#616/#634/#683: intent-based terminal actions (including empty workspaces and configured delegation), ACP tool/transcript rendering, clarification input, session configuration, model title, and replacement cleanup across the deployed helper/master boundary | 8 |
| `Feature.AcpAuthentication.Tests.ps1` | Cold-user normal-pane sign-in, advertised method and same-process session creation, long authorization waiting, cancellation, stale completion, retry and SDK-delivered manual browser link; deterministic ACP fixture, zero provider tokens. The fallback case uses an invalid client ID and may open a browser error page; never authorize that fixture URL | 3 |
| `Feature.PromptQueue.Tests.ps1` | C095, C410-C421: gated local ACP fixture covering startup Autofix, idempotent diagnostics, independently held FIFO turns and pinned counts during scrolling, attachments, disabled queue controls, retained stop/failure pauses, typed `/fix` snapshots, source-pane priority, and redraw versus command invalidation (no LLM) | 14 |
| `Feature.AgentImageAttachmentEditing.Tests.ps1` | PR #536: inline image tokens move and delete atomically while preserving adjacent prompt text | 1 |
| `Feature.Paste.Tests.ps1` | Physical normal text/image paste, owner routing and refocus; screenshot paste respects right-click menu settings and preserves Alt+V | 6 |
| `Feature.AgentModelSync.Tests.ps1` | PR #538: ACP config-option updates replace stale session model state in the active picker | 1 |
| `Feature.AgentModelLifecycle.Tests.ps1` | PR #554: `/model` hot-apply and Settings-driven model restart/reconnect lifecycle | 2 |
| `Feature.ByokProvider.Tests.ps1` | PR #447: Settings-selected OpenAI-compatible provider request path, credential handling, and BYOK-to-cloud restart lifecycle | 2 |
| `Feature.AgentCompactLayout.Tests.ps1` | PR #580: compact-height recommendation, input, and Insert interaction at the real splitter minimum | 1 |
| `Feature.AgentPanePadding.Tests.ps1` | Issue #793: deterministic full recommendation card, navigation hint, and action alignment across the packaged WTA render boundary | 1 |
| `Feature.ProposalMcpRouting.Tests.ps1` | PR #560: per-session proposal MCP names and two-tab Helper routing isolation | 1 |
| `Feature.AgentMouse.Tests.ps1` | PR #506 and issue #790: physical chat wheel scrolling, Ctrl+wheel zoom, draft preservation, text selection/copy, and stale-selection suppression; completed-turn full-row clicks across multiline prompts with shared keyboard selection/Enter behavior, row-end/drag guards, and input-dialog focus recovery | 7 |
| `Feature.AgentSelectAll.Tests.ps1` | Physical Ctrl+A selects only the focused nonempty draft: exact source copy, cut/delete/replace, repeat/Esc/caret collapse, and pending-turn safety; empty input and history focus retain pane copy and stale-selection clearing. Deterministic ACP fixture; unique evidence under `ITE2E_ARTIFACT_ROOT` (default `artifacts`) | 6 |
| `Feature.AgentInputNavigation.Tests.ps1` | Physical Up/Down edits explicit and soft-wrapped input rows, preserves preferred display columns and viewport following, collapses full-input selection safely, and retains deterministic prompt-history boundary behavior | 5 |
| `Feature.AgentInputMouseCursor.Tests.ps1` | Physical mouse clicks move the draft caret across ASCII, scrolled multiline, soft-wrapped and wide Unicode text; exact clipboard insertion oracles and a deterministic ACP fixture | 4 |
| `Feature.AgentInputUndoRedo.Tests.ps1` | Physical Ctrl+Z/Ctrl+Y: grouped typing, atomic edits, multiline Unicode, real clipboard image payload restoration, redo branching, and submission/history boundaries. Deterministic ACP fixture; exact clipboard/capture evidence under `ITE2E_ARTIFACT_ROOT`; requires an English (US) layout and an explicitly selected package (existing Dev closes automatically; non-Dev stays protected) | 6 |
| `Feature.PromptHistory.Tests.ps1` | PR #478: per-tab Up/Down prompt recall, draft restoration, and multiline preservation; PR #614: completed-turn collapse/expand rendering | 4 |
| `Feature.CompletedTurnSelection.Tests.ps1` | Completed-turn Tab/Up/Down selection keeps focused history inside the chat viewport | 1 |
| `Feature.AutofixPane.Tests.ps1` | Direct Helper Autofix proposal card render/insert/run/reject/target/stashed + across layout + WSL shell identity and Linux fixes | 12 (2 WSL-gated) |
| `Feature.AutofixParser.Tests.ps1` | issue #474: PowerShell ParserError-to-Autofix pipeline + success/handled-error/blank-input negative controls | 4 |
| `Feature.AutofixRouting.Tests.ps1` | Two Detected tabs: real diagnostics clicks submit only to the selected tab's ACP session and preserve the other tab's opt-in | 1 |
| `Feature.PaneContext.Tests.ps1` | issue #838: packaged pane-context capture, marked/unmarked output, explicit routing, missing panes, metadata-only mode, Unicode bounds, and agent-focus source resolution | 7 |
| `Feature.CommandResolution.Tests.ps1` | PR #418: packaged WTA resolves PowerShell profile-only aliases to their real targets | 1 |
| `Feature.AutofixCommandResolution.Tests.ps1` | Issue #844: Debug Dev, deterministic ACP fixture; no startup/tab-selection probes, first/later Autofix contracts without enumeration, and explicit local-candidate lookup | 3 |
| `Feature.SessionList.Tests.ps1` | session view (button + `/sessions` slash), session states, view switching (incl. draft-preservation), focus/restore | 13 (+1 skip) |
| `Feature.SessionRefresh.Tests.ps1` | Master-owned history synchronization with closed views, read-only snapshots, real 60-second layout-specific fallback, live layout switching, and explicit refresh; deterministic listing-capable ACP fixture, no model quota, explicit Dev hashes and automatic Dev preflight required | 4 |
| `Feature.KeepRunningFocus.Tests.ps1` | Explicit history/session `focus-pane` reattachment; ordinary Start-menu and profile launches create a new tab while two kept tabs remain detached; original shell/helper identity and stale-target safety; deterministic ACP fixture | 2 |
| `Feature.NonAsciiCwd.Tests.ps1` | issue #641: a non-ASCII starting directory survives `wtcli` argv → COM → `CreateProcessW`, so the resume launch path connects and starts in that directory | 2 |
| `Feature.AgentPaneCwd.Tests.ps1` | agent-pane source workspace reaches ACP `session/new` and remains stable across `/new` without a model prompt | 1 |
| `Feature.AgentRestart.Tests.ps1` | agent restart after a settings change (/restart reconnects and answers) | 1 |
| `Feature.ShellIntegration.Tests.ps1` | §3 shell-integration OSC 133 marks (success/failure, ParserError dedup, handled errors, WinPS 5.1 errors) + non-integrated cmd.exe safety | 6 |
| `Feature.BashPromptIntegration.Tests.ps1` | PR #468: Bash `PROMPT_COMMAND` PS1 rewrites preserve D/A/B boundaries; non-IT hosts remain gated | 1 (Git Bash-gated) |
| `Feature.AgentProposedCommand.Tests.ps1` | §2 Direct Helper Proposal Insert/Run into the shell pane | 2 |
| `Feature.YoloMode.Tests.ps1` | Default-provider-scoped automatic approval persistence across global, `/agent`, and profile bindings; deterministic permission boundary; hidden unsupported/policy states; retained Gemini guidance; and live policy reconciliation | 8 (OpenCode, Gemini, `/agent`, profile, and policy gated) |
| `Feature.AgentProposalFocus.Tests.ps1` | PR #533: Insert returns real window keyboard focus to the target shell pane | 1 |
| `Feature.AgentMatrix.Tests.ps1` | §2 non-Copilot built-in agents (Claude/Codex/Gemini) connect+chat through the ACP adapter — ONE consolidated case (Copilot is the in-depth suite); skips when none installed+authed | 1 |
| `Feature.HookTrace.Tests.ps1` | C190, C267-C269, C272, C352-C353, C355-C357: guarded commands deliver, sensitive payloads stay private, unsafe identifiers/sources are rejected, and Antigravity identity/cwd/idle/error handling stays source-correct | 10 |
| `Feature.SessionHookRouting.Tests.ps1` | PR #761: master consumes one `wtcli agent-hook` COM broadcast directly while multiple helpers update only local pane bindings, a terminal hook for an unseen session fabricates no row, and `agent.error` still records the failure | 3 |
| `Feature.SessionOwnershipRestore.Tests.ps1` | C318, C354: nested-agent prompts preserve the root owner; Antigravity WSL hooks and saved-layout resume retain the distro and cwd | 2 (WSL case environment-gated) |
| `Feature.HookBridgeCli.Tests.ps1` | PR #571 C274, C265, C266: a real agent CLI fires the bundled `hooks.json` command through its own shell, and neither an unreachable protocol server nor an uninstalled Terminal blocks the CLI; skips when the CLI isn't installed+authed | 3 (environment-gated) |
| `Feature.LegacyHookBundle.Tests.ps1` | PR #571 C270-C271: a pre-#571 PowerShell hook bundle still delivers against a post-#571 Terminal, and degrades quietly when `WT_COM_CLSID` is unset | 2 |
| `Feature.OpenCodeHookBridge.Tests.ps1` | PR #571 C273: OpenCode's JS plugin spawns `wtcli` through an argv array with no shell, so it resolves the bridge via `WTCLI_PATH` rather than the `PATH` alias | 1 (environment-gated) |
| `Feature.OpenCodeAgent.Tests.ps1` | PR #458: built-in OpenCode launches its native ACP server and completes agent-pane chat | 1 (environment-gated) |
| `Feature.AntigravityProvider.Tests.ps1` | Built-in standalone ACP registration and source-specific native Linux discovery, without model requests. Set `ITE2E_ANTIGRAVITY_WSL_DISTRO` for the WSL case. | 2 (WSL case environment-gated) |
| `Feature.OpenCodeSessionResume.Tests.ps1` | PR #464: OpenCode history discovery and `--session` resume restore the prior transcript | 1 (environment-gated) |
| `Feature.OpenCodeHooks.Tests.ps1` | PR #476: packaged hook install, shell-session lifecycle routing, picker visibility, and ACP duplicate suppression | 1 (environment-gated) |
| `Feature.SharedAgentLifecycle.Tests.ps1` | PR #425 + ACP cleanup: closing a tab mid-turn physically closes only its session without terminating the shared agent CLI or breaking sibling tabs | 1 |
| `Feature.AgentPaneLifetime.Tests.ps1` | Issue #841: hidden retention, split-tab cleanup, lease retirement, draining-only crash suppression, agent-first/later cross-window moves, and rejected pane-move rollback preserving both tabs and sessions; deterministic stdio fixture | 10 |
| `Feature.PerTabAgent.Tests.ps1` | C225-C228 + PR #487: `/agent` picker/direct selection, invalid-id safety, per-tab isolation/shared-master reuse, and global-default/override behavior | 7 |
| `Feature.WslAgentBackend.Tests.ps1` | PR #481 profile-scoped WSL agent backend: settings hot reload, helper/master source routing, and authenticated chat | 2 (environment-gated) |
| `Feature.DelegateSource.Tests.ps1` | PR #488 profile-scoped delegate source: strict host/WSL `wta delegate` routing with no fallback in either direction | 2 (environment-gated) |
| `Feature.AgentChat.Tests.ps1` / `Feature.AgentPopup.Tests.ps1` | agent chat + `/` popup/menu interaction | 1 + 3 |
| `Feature.AgentPaneMove.Tests.ps1` | PR #429: `/move` stays per-tab, preserves global position, and restores agent input focus | 1 |

**Coverage and results are tracked by stable checklist IDs and generated release reports.**
The updated combined-sidebar scope/search cases reuse that suite's owned Dev
startup, deterministic history fixture, native resume fixtures and teardown.
The related action, session-refresh, delegated-identity, provider-appearance,
relative-time and opt-in telemetry suites now select scope through the same
standard checked menu items rather than invoking the retired heading. Upgrade
tests observe the static heading for rail visibility. Legacy History hotkey
coverage retains the horizontal agent-pane path; in Sidebar it toggles Recent
without moving source input focus or undoing a prior rail expansion. Menu
dismissal returns to Filter, while active shared-search identity/query remain
intact. Their existing ownership, action, deduplication and persistence oracles
are unchanged.
Its 300-second bound applies to the new scope/search context, not the longer
native-action regression suite. Nonlive discovery and synthetic report checks
are not package acceptance.

SearchTextBox assertions use `Get-UiValue -ValuePattern` to read the real UIA
value, including empty text. The default display-text helper retains winapp's
Name fallback and is not an exact-empty query oracle.
After the build owner proves the deployed
feature-head App/WTA hashes, run with explicit Dev and prerequisite validation:

```powershell
$env:ITE2E_PACKAGE = 'Dev'
pwsh -NoProfile -File test\e2e\bootstrap.ps1 -Check
& .\test\e2e\Invoke-ItE2EReport.ps1 `
    -Path @('test\e2e\tests\Feature.CombinedAgentsSidebar.Tests.ps1', 'test\e2e\tests\Feature.SidebarTabKeyboard.Tests.ps1') `
    -UpdateReport
```

For a focused Pester run, use `Filter.FullName` selectors
`*Independent scope filters and global search*` and
`*History Enter resumes an unbound native session*`. The report driver does not
accept `-FullNameFilter`; use its whole-suite `-Path` option for release results.

Ordinary revised Sidebar fixtures explicitly pass `-State @{
sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true }`
to `Start-Terminal`. This is opt-in, applied only after the existing owned
settings/state backup, and self-verifies persistence; default harness startup
and fresh FRE behavior are unchanged. Upgrade tests intentionally supply false
flags and preserve the migrated state across relaunch, restoring the original
bytes only after their last owned process exits.
The persistence-failure case prepares its protected baseline before launch and
holds an existing `state.json` read handle without write/delete sharing.
`Start-Terminal -Backup $false -CleanSettings $false -PassFre $false` preserves
that caller-owned baseline and performs no setup writes into the locked file.
The lock is disposed in `finally` before stopping/restoring its owned process.
Nonlive `SidebarPersistenceFault.Unit.Tests.ps1` verifies real Windows
overwrite/replacement denial, readable original bytes and released-lock retry;
it does not claim product acceptance.

**Explicit fault-coverage gap:** introduction completion failure after actual
presentation requires delivery between writable preflight and the completion
write. Holding state.json before startup tests only preflight suppression, and
locking it after discovering a visible tip is an unproven race. No such case is
credited. The planned real-boundary case must observe process A's valid claim
before presentation, block only completion, establish a distinct owned process
B's visible eligible Sidebar, and prove B has no tip while disk shown remains
false; release the fault and require A's durable true flag before permitting
cross-process suppression. This needs a proven real observer/control boundary
or directly wired native tests, not an invented callback or model flag proxy.
The source owner has now authored actual-file/actual-ApplicationState native
tests (not run): `SidebarMigrationStateFailureWarnsAndRollsBackRealSettings`,
`SidebarMigrationRollbackFailureWarnsAndKeepsLastSavedLayout`,
`SidebarIntroductionPersistenceFailureAfterPresentation`, and
`SidebarIntroductionCloseRetriesPendingDurability`. These are under
`SettingsModelUnitTests::ApplicationStateTests`, not E2E acceptance. The second
migration oracle intentionally differs: when rollback also fails, the effective
layout must match the last successfully saved Vertical preference and completion
must remain false. No live test currently delivers that two-stage fault.
The known native project is
`src\cascadia\UnitTests_SettingsModel\SettingsModel.UnitTests.vcxproj`, output
`SettingsModel.Unit.Tests.dll`. After ROOT's correct dependency build and
TestHostApp hosting/output aggregation for MUX/resources, the targeted commands
are `te.exe SettingsModel.Unit.Tests.dll /name:*ApplicationStateTests::Sidebar*`
(13 authored methods) and, separately,
`te.exe SettingsModel.Unit.Tests.dll /name:*DeserializationTests::TabLayoutSetting*`.
Neither command has been run for this revision; do not substitute direct DLL
execution without the documented host/resource setup.

Latest build status: root reports compiler invocation `shell746` completed with
exit **1**, not pending; compiler diagnostics are still being verified. The
frozen TabStrip hashes do not constitute a successful build or deployed runtime
proof. New migration/timer fixes and the focused-history/viewport correction
require new frozen/deployed provenance before any execution authorized by ROOT.

Failure-first selection now starts with `Feature.SidebarUpgrade` and the
`Combined sidebar mixed rows share one scroll viewport`,
`Recent Sessions collapses without hiding live agents`, header/search and
physical resize cases in `Feature.CombinedAgentsSidebar`. Recompute discovered
full names against the frozen source contract; do not reuse the old 17/60 totals.
Follow with metadata/ownership/resume/retention, Agents actions, six-unit Arabic
RTL, provider appearance, keyboard, progress, pin, refresh and lifetime suites.
Run ordinary cases under independent 300-second execution and 60-second cleanup
budgets (app 60/UI 30), exact-PID/proven-descendant ownership only. These are
planned selectors, not an acceptance result. No new screenshot is evidence of
acceptance until the corresponding actual UIA/protocol/persistence case passes.
The final nonlive essential matrix is recorded by exact expanded Pester names in
`artifacts\sidebar-final-authored-discovery-20261007.json`: 86 cases across 14
related suites, zero executed. It excludes retired C382/C383 names and includes
their replacement shared-scroll/resize regressions, native collapse/Content
events, focused-row fallback (including owned background action focus), mixed
keyboard, actual tooltips, fresh/absent-state/FRE/rail/window introduction cases,
live tab append/OSC title replacement/removal, refresh, pin, ordinary keyboard,
identity/MCP, retention and lifetime protections. C390 adds physical modified
Enter negative controls before its existing exact-once native resume oracle.
Arabic/English compact-age cases now also physically realize and focus each row
at narrow and normal Sidebar widths, asserting mirrored actual screen bounds.

Remaining material live gaps are explicitly not credited: reliably delivering
the post-presentation persistence fault between real preflight and completion
(the 13 authored actual-file native methods remain its own boundary coverage),
custom modifier-keybinding action forwarding, and actual HC/color/Narrator
evidence. Light/Dark compositor evidence still requires its existing independent
visual sign-off. UIA tooltip popup ownership, Button/ExpandCollapse events and
focused-history fallback are authored fail-fast oracles, not runtime passes.

`Feature.CombinedAgentsSidebar` requires `ITE2E_PACKAGE=Dev`,
`ITE2E_EXPECTED_APP_SHA256`, `ITE2E_EXPECTED_WTA_SHA256`, and
`ITE2E_SOURCE_COMMIT` from the exact-source build receipt. The source revision must
match the worktree HEAD (a receipt suffix may describe uncommitted build fixes).
History retains its shell-origin contract: deterministic native hooks establish
the root identity before a held ACP fixture prompt, and uniquely named rows avoid
matching existing user history. For a single retention diagnostic, set
`ITE2E_COMBINED_RETENTION_STATUS=Idle` or `Working` and filter the Pester full name
with `*retains unattached*`; unset that variable for the normal eleven-case suite.
The metadata case uses actual row-control bounds from the raw UIA tree, not XAML
coordinates, and writes screenshots and geometry receipts. Content view must
expose one provider semantic leaf and omit the decorative provider icon; the icon
retains its Raw-view name and provider tooltip. This does not verify screen-reader
speech. The lower heading is
**Recent Sessions** (`HistoryHeaderButton`, default-expanded native
ExpandCollapse peer). Agents uses the real mixed `ItemsList`; `HistoryList`
and `HistorySplitter` are retired, not hidden compatibility controls. C382/C383
are retired stable IDs and old results cannot credit the replacement shared
viewport/collapse requirements. A missing decorative
header peer is a failed oracle, not evidence that its glyph rendered; compositor
visual review may be needed before adapting that assertion. This Windows-only
fixture proves provider aliases; WSL distro aliases still need source-specific
coverage and must not be credited from this case.

The suite table describes available cases, not a blanket pass result for every package or
environment. Use `Invoke-ItE2EReport.ps1` and its full or incremental release report for the
selected revision's actual passed, failed, skipped and remaining checklist items.
`Feature.AgentHotkeys` keeps physical Ctrl+Shift+/ input and observes the native
`VerticalTabsHeader`/`HistoryList` projection. `Feature.SessionRefresh` opens the
current Tabs/Agents header, not the retired History toolbar or search box, and
keeps shared search off. Keep-running and progress context routes resolve the
canonical tab from its shell pane ID before right-clicking its real title bounds;
duplicate pane titles are not tab headers. Progress menu screenshots preserve the
owned flyout foreground instead of reactivating its root and dismissing it.
Header geometry and hit testing share a scoped per-monitor physical-coordinate
context; `tab-header-context-*.json` records exact header/row runtime identities,
hit ancestry, HWND ownership and coordinates even when the input guard rejects.
Some XAML-island hosts expose only the hosting Window to both managed and native
UIA point queries. These two fixtures permit that exact opaque-root result only
with independent canonical identity, full title visibility, top-header-band and
pane/action exclusion, stable fresh geometry, owned run/process lease, known
overlay checks and immediate foreground/native-point/cursor/held-input guards.
They reuse the established paired physical right-click contract; they do not
treat an owned root as sufficient or claim to detect every possible XAML overlay.
The guarded route requires `ITE2E_RUN_TOKEN` and the harness's
`ITE2E_OWNED_PROCESS_RECEIPT` for the original fixture process.
The keep-running explicit-profile baseline activates the selected package through
public `IApplicationActivationManager` with its original AUMID and profile arguments,
then verifies the fulfilling packaged process and the ordinary one-tab window.
Starting the payload EXE directly can create a separate unpackaged host and is not
a packaged Start-menu/profile-launch oracle.
`Feature.AgentInputUndoRedo` maps its deterministic editing cases to stable checklist IDs,
including real clipboard image insertion/replacement and restored ACP payload verification.

Environment-dependent suites declare their prerequisites, including installed/authenticated
agents, WSL availability, hook or policy provisioning, and interactive-desktop input support.
Unavailable external prerequisites may be skipped; product failures must remain failures.
Manual release-sign-off items are not credited by unrelated unit or protocol checks.

The focused UI controller checks both an absolute UTC deadline and monotonic elapsed
time between short (500 ms) process waits. Windows handle waits alone do not include
system sleep. A suspended controller cannot enforce a deadline while suspended; on
resume an expired deadline aborts the run, not a successful timeout-compliance receipt.
Loss of owned foreground or interactive cursor access stops the focused batch through
an input-prerequisite receipt; remaining cases are unproven, not passed or skipped.
Emergency cleanup accepts only the run-token receipt's exact executable/PID/start-time
identity and its captured descendants. Foreground acquisition never taps ALT into an
unrelated app or modifies the user's foreground-lock timeout.

For **PR validation** of `Feature.AgentInputUndoRedo`, build/deploy the intended source revision
and set `ITE2E_EXPECTED_WTA_SHA256` from that build's receipt before running the suite. Verify
source-to-package freshness as well; copying the hash from an arbitrary installed binary is not
proof of its source revision. The suite records the actual package and hash in `package.json`.
An intentional Store/production baseline run may omit the expected hash, but its results must
not be presented as validation of unshipped PR code.

Token-consuming simulated-real-user tests are deliberately excluded from this publishable suite
and from CI. They live only in the feature's dev-only local validation harness and run manually
against an exact deployed publish package with explicitly available provider quota.

### Kept-tab regression checks

`Feature.KeepRunningFocus` targets the actual vertical-tab header and context-menu
item, not title text shared by pane rows and terminal documents. It uses
`warning.confirmOnClose` for the intended fixture setting and activates the
selected package by AUMID for profile launches. Retained-session readiness is
verified through pane identity, tab counts and unchanged process IDs rather than
mutable tab-title text.

### Deterministic mouse and paste regression checks

The `CompletedTurnMouse` group contains four fixture-backed cases; it can run without a real
provider prompt. Run the group together as well as the `RightClickCopy`/`RightClickPaste` cases
individually. Each case verifies an empty connected draft at entry and cleanup, with a fresh
fixture conversation so accumulated history cannot suppress the transient copy hint. The group refuses
an already-used selected package, preserves clipboard formats and mouse position, and records
unique captures below `ITE2E_ARTIFACT_ROOT` (or the default artifacts directory).
The paired paste suite also preserves full clipboard formats. Both fixtures retain a recovery
target before startup. If startup never returns a launch context, any remaining package process
blocks automatic recovery; a matching path and recent creation time are not termination authority.
Configuration is restored only after the selected package is confirmed inactive. Ambiguous
ownership or ineffective termination fails with the configuration backup retained.

The shared `Get-UiTextBounds` helper locates the first literal match in the single visible named
TermControl belonging to the test window. It verifies the exact range text before returning
rectangles; UTF-16 string indexes are not UIA Character offsets. An unrepresentable or mismatched
range fails instead of guessing a coordinate. `Send-AgentKey -Key Escape` uses a complete Win32
key event so a pending escape prefix cannot consume the next prompt character.

```powershell
$env:ITE2E_PACKAGE = 'Dev'
$env:ITE2E_EXPECTED_WTA_SHA256 = '<hash from the exact-source build receipt>'
$run = Join-Path $PWD ('test\e2e\artifacts\mouse-' + [guid]::NewGuid().ToString('N'))
$env:ITE2E_ARTIFACT_ROOT = $run
.\test\e2e\Invoke-ItE2EReport.ps1 `
    -Path @('test\e2e\tests\Feature.AgentMouse.Tests.ps1', 'test\e2e\tests\Feature.Paste.Tests.ps1') `
    -Tag @('CompletedTurnMouse', 'PasteCore', 'PasteRefocus', 'PasteOwnerIsolation') `
    -OutDir $run
Invoke-Pester test\e2e\selftests\MouseInput.Unit.Tests.ps1 -Tag Unit
```

The unit file keeps one representative regression per major helper or cleanup behavior, using
small stubs without a deployed app. The existing mouse and paste E2E cases retain the broader
interaction coverage. Existing checklist titles remain unchanged; use fresh full/incremental
reports rather than treating historical failures as passing after a helper change.

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

## What it gives you

### Opt-in telemetry funnel validation

`Feature.SidebarTelemetry` reuses the same bounded elevated ETW collector and
typed decoder without policy writes. Select `ITE2E_PACKAGE=Dev`, opt in with
`ITE2E_TELEMETRY=1`, and supply `ITE2E_EXPECTED_APP_SHA256` and
`ITE2E_EXPECTED_WTA_SHA256` from the new build. The suite refuses an active Dev
package, uses a deterministic ACP fixture without model quota, restores
settings/state byte-for-byte, and retains real UI phase evidence and raw ETW
artifacts. Before deploying Dev from another branch, compare its generated
`AppxManifest.xml` version with the installed package: the safe Debug deployment
script rejects downgrades before `DeployAppRecipe.exe` can unregister the working
package. Bump `Package-Dev.appxmanifest` and rebuild rather than removing the
installed package (which would discard LocalState).

If registration fails with `0x80070020` while updating
`AppRepository\Packages\<Dev-package>\PackagedCom\OpenConsoleProxy.dll`,
check loaded modules in other Terminal processes as well as processes in the
Dev layout. An ordinary Windows Terminal can retain that COM proxy after Dev
closes. Rebuild with a fresh Dev manifest version to avoid overwriting the
mapped proxy; do not terminate the current CLI host or delete AppRepository
files to release it.

Its `row_count` oracle counts the unified Agent view's session rows
on first successful load, independently of live-tab search and split-pane
children. `SidebarTabPinned` means enabling headless mode,
not tab-order pinning. Row-field selection verifies canonical field IDs for
empty, single, and paired selections, a disabled third choice, and suppression
during menu-only actions and metadata/layout refresh.
Window creation emits the selection with Source=Launch, while actual edits use
Source=UserChange. A new fixture session after a non-default selection must not
emit another field snapshot. Real tab-order pin/unpin is tested separately from
Keep running and carries typed Pinned/PinnedCount.
The same scenario checks that explicit opt-ins emit distinct random `KeepId`
values and typed post-enable `TotalTabCount` / `KeepRunningTabCount` snapshots,
including search-hidden attached tabs. A real tab close and restoration emit
matching detached/live events, and matching AttemptId values connect restoration
starts/results. A prompt on the unchanged ACP session after reattachment carries
`AgentPromptSent.Reattached=true` and `UserPromptOrdinal=Second` while that
session's earlier prompt carries `false` and `First`. A separate helper's
first prompt also remains `First`.
Disabling or restoring a tab alone must not mark it again or dispatch a prompt.
The decoder explicitly selects startup/sidebar event names; unrelated structured
diagnostic events remain in the raw ETL rather than blocking these typed
assertions. Missing or unsupported schemas for selected events still fail.
The same capture includes a real fixture prompt and verifies that App session
starts and WTA session creation, prompt, first-text, and completion payloads
contain no provider session identifiers. Prompt metrics are scoped by process
and action phase, then joined by random telemetry SessionId/TurnId. These IDs
never expose the provider's session ID.

To validate only keep-running telemetry without running the unrelated row-field
and Agent-view scenarios, retain the same Dev selection, telemetry opt-in, and
build-hash environment variables, then run the existing three contracts:

```powershell
$cfg = New-PesterConfiguration
$cfg.Run.Container = New-PesterContainer `
    -Path test\e2e\tests\Feature.SidebarTelemetry.Tests.ps1 `
    -Data @{ KeepRunningOnly = $true }
$cfg.Filter.FullName = @(
    '*Sidebar pin telemetry counts explicit keep-running opt-ins'
    '*Keep-running telemetry correlates opt-in, retention, and live reattachment'
    '*Restored agent prompt telemetry identifies the surviving ACP session'
)
$cfg.TestResult.Enabled = $true
$cfg.TestResult.OutputFormat = 'NUnitXml'
$cfg.TestResult.OutputPath = 'test\e2e\artifacts\keep-running-results.xml'
Invoke-Pester -Configuration $cfg
```

This focused run is not a pass for the complete sidebar suite. Expanded split
tabs repeat their title in child rows; context-menu targeting selects the
shallowest matching tab header. The elevated ETW collector runs with its window
hidden so it does not compete with the unelevated UI runner for foreground.

`Feature.TelemetryFunnels` requires a **Dev** package built from the target revision,
the build receipt's `ITE2E_EXPECTED_WTA_SHA256` and `ITE2E_EXPECTED_APP_SHA256`
(`TerminalApp.dll`), explicit UAC approval, and permission for temporary HKCU policy
changes (`ITE2E_TELEMETRY_POLICY_APPROVED=1`). Its preflight closes existing
verified Dev processes before the policy transaction; other installed Intelligent
Terminal packages (including headless helpers) must remain inactive because HKCU
policy affects them too.
Set `ITE2E_TELEMETRY=1` and
`ITE2E_PACKAGE=Dev`, then pass the suite to `Invoke-ItE2EReport.ps1`.

One bounded elevated `Collect-TelemetryTrace.ps1` capture covers the suite: only the Win32Host,
App, WTA, and Settings Model providers are enabled. No kernel/session-wide process tracing or
third-party upload is used. Captures contain these providers' events from any concurrently
running process; `scoped-events.json` includes only owned App/master/helper PIDs.
For Win32Host, the typed funnel output includes only `SessionBecameInteractive` and `UserInteract`;
unrelated structured diagnostics remain available in the raw ETL/XML.
Raw ETL, tracerpt XML, TDH-extracted TraceLogging schemas, logman results, and package hashes remain
in a unique artifact directory. Missing/ambiguous self-describing metadata fails validation;
diagnostic logs never substitute for typed telemetry. The collector stops only its unique
session, including on timeout (20 minutes by default, at most 30 minutes).
Settings/state bytes are restored and hash-checked. Only the approved HKCU
`AllowAutoFix`, `AllowedAgents`, and `AllowCustomAgents` values are temporarily changed;
original presence, registry types, and values are retained for verified restoration.
Machine policy takes precedence and causes a refusal rather than an override.
Keep the UI/COM test runner non-elevated. Windows may protect the HKCU policy path
from normal writes, while an elevated UI runner cannot necessarily reach the
ordinary packaged COM server. The approved elevated collector handles the three
allowlisted policy values separately; it must not change registry ACLs or HKLM.

The readiness regression uses real fixture ACP model/config updates to produce repeated
Connected statuses. Native responses must include the boolean Autofix flag in both settings
states and the correct tab/window scope. The pre-fix native payload is missing this field.
A wrong-tab status elicits no response; old errors are not replayed, and new helper error
telemetry carries the effective flag separately from raw policy. This verifies the native
boundary and downstream state, not recovery of artificially stale helper state. It neither
publishes privileged host config events nor suspends processes.

The expanded suite triggers the original funnel's 12 distinct client events through real
UI, shell, ACP, settings, and Session MCP paths. It checks matching offer/acceptance IDs,
Run versus Insert/Reject, redraw deduplication, foreground palette entry versus hiding and
submission, both-role provider changes, same-session ordinary prompts, inventory/sidebar
variants, policy categories, and a second window in the same process versus reactivation.
Phase-scoped ETW evidence records typed fields and negatives;
an incomplete trigger phase fails rather than being interpreted as an absent event.

The 18 cases include two independent **Startup Autofix policy labels match effective
helper state** variants. They set approved `AllowAutoFix=0/1` before a fresh owned host
launch and require typed WTA `ErrorDetected` values `disabled`/`false` and
`enabled`/`true`, respectively. Enabled policy uses an actual shell failure with
`Method=vt_sequence`. Blocked policy intentionally suppresses OSC forwarding:
the test discovers the sole owned helper without an OSC tab probe, proves a unique
shell exception rendered, and asserts no forwarded VT event or Autofix prompt.
It then exits that controlled shell with code 37 and `closeOnExit=never`, retaining
the helper to observe the real `connection_state` failure. Its disabled-policy
`ErrorDetected` must have `Method=connection_state`, with no Autofix prompt or offer;
it is not evidence that the blocked shell failure's VT event was captured.
These cases can establish the original funnel requirement 3.1 only
after those fields and controls pass; event names alone are insufficient.

The required policy contract is next-process-start effectiveness, not hot refresh.
Set `ITE2E_TELEMETRY_HOT_POLICY=1` only to add the two strict diagnostic cases for
[issue #991](https://github.com/microsoft/intelligent-terminal/issues/991).
Without this opt-in neither their registry writes nor their assertions run.
Their historical failures remain evidence; startup acceptance does not establish
hot-refresh behavior. The release checklist maps policy segmentation to the two
startup variants rather than imposing an unrequired notification contract.

The original requirement rows are not equivalent to event counts: several share `AppCreated`
or ACP session creation. Sidebar and keep-running contracts are covered separately
by `Feature.SidebarTelemetry`. D7/D28 retention still requires backend device identity, elapsed days, and cohort
queries; two-prompt depth can be demonstrated locally but does not validate a backend query.
`DefaultsFallback=true` and compatibility-only unknown categories are not credited by the
normal-startup scenarios. Results belong to the selected build's generated report and retained
capture, not a blanket assertion that every environment or backend metric passed.

After both captures, validate the local business funnels rather than dividing
unrelated event totals:

```powershell
.\test\e2e\tools\Test-CapturedTelemetryFunnels.ps1 `
  -FunnelCapture '<funnel scenario directory containing scoped-events.json>' `
  -SidebarCapture '<sidebar scenario directory containing scoped-events.json>' `
  -OutputPath '<local output directory>\business-funnel-counts.json'
```

The command requires successful source phases, joins opaque IDs in timestamp
order, asserts eleven deterministic numerator/denominator pairs, and checks
duplicate delivery, missing completions, unrelated sessions and empty cohorts.
It does not credit unrelated failed UI phases, replace full-suite results, or
claim backend ingestion, production conversion rates, execution success or
D7/D28 retention.

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

`Start-Terminal -Package Dev` may close an existing Dev window or helper, including
its unsaved input and in-progress commands. Run Dev tests from another terminal;
the current chat process tree is always protected. The Store build remains
fail-closed when it already has a process, and ordinary Windows Terminal is never
an ItE2E target. At teardown `Stop-Terminal` still stops only the process this run
proved it created; it does not treat every same-package process as test-owned.

For an opt-in **Dev PR package consistency check**, run the report driver against
a clean worktree at the intended source HEAD and provide all four proof inputs:

```powershell
$env:ITE2E_PACKAGE = 'Dev'
$sourceRoot = (Get-Location).Path # clean worktree for the PR head
$expectedHead = '<FULL_40_CHARACTER_PR_HEAD>'
$recipe = Join-Path $sourceRoot 'src\cascadia\CascadiaPackage\bin\x64\Debug\CascadiaPackage.build.appxrecipe'
$msix = Join-Path $sourceRoot 'src\cascadia\CascadiaPackage\AppPackages\<FRESH_PACKAGE>.msix'
pwsh -File test\e2e\Invoke-ItE2EReport.ps1 `
  -Path test\e2e\tests\<Feature>.Tests.ps1 -RequireNoSkips `
  -SourceRoot $sourceRoot -ExpectedHead $expectedHead `
  -RecipePath $recipe -MsixPath $msix
```

The read-only `Verify-PackageProvenance.ps1` preflight checks the clean Git HEAD,
the full recipe/MSIX/registered manifest hashes, every recipe source/MSIX/installed
payload hash, and registered package identity/version. A missing or stale payload stops the run
**before Pester**.
Only documented MSIX metadata may appear beyond the recipe payloads. Recipe
paths are separator-normalized before duplicate detection; archive paths are
URI-decoded before matching (packaging can encode braces in icon names);
scale-qualified profile icons may be absent, but must still match the recipe in
the installed layout. Recipe sources must be inside the selected worktree except
the Windows SDK debug `ucrtbased.dll`, which is reported with its source path and
hash under `ExternalInputs`. Keep build-time source/command evidence
separately: matching hashes do not themselves prove which source the compiler
used. These options are not required for an explicitly requested Store baseline.

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
Invoke-Pester test/e2e/selftests              # all self-tests
```

The self-tests are the framework's own proof: every primitive is exercised against a
running terminal (`selftests/ItE2E.Live.Tests.ps1`) and the core helpers are unit-tested
in `selftests/ItE2E.Unit.Tests.ps1` (hermetic, no terminal needed).
The Live fixture pins completed onboarding and a default-profile horizontal tab so
the same smoke test also works with a newly registered isolated Dev package.
The harness restores the original settings and state once the package is inactive.

### Deterministic Queue regressions

`Feature.PromptQueue.Tests.ps1` uses `fixtures/Mock-AcpQueueAgent.ps1`, not an
authenticated agent or an LLM. Every case crosses the deployed WT/ConPTY →
helper → master → ACP boundary. The fixture records ordered prompt/completion
JSON under `artifacts/prompt-queue-fixtures/<run-id>/`; its ten
hermetic cases in `selftests/ItE2E.QueueFixture.Tests.ps1` cover gate polling,
cancellation settlement, same-session overlap rejection, and recovery. The
fixture advertises image input and records ACP image metadata (MIME type, decoded
byte count, and signature), never raw image data or MCP credentials. Attachment
preservation is checked through the received PNG payload, not only a rendered token.

| Exact checklist/test title | Trigger and deterministic oracle | Negative control / existing protection |
|---|---|---|
| Queued user messages run once in submission order | Hold A, enqueue B/C, release A while independently holding B; C must remain absent until B is released, with intact request content and completion-before-next-prompt sequence numbers. A viewport-sized active transcript proves chat moves to its hidden top while the count row stays fixed. | Immediate A has no queued notice; any same-session overlap is a fixture error. |
| Pending queue appears automatically and updates above input | Hold A, trigger automatic Autofix, then enqueue held B. Observe count 0→1→2, release A and B separately to observe 1→0. No message previews or queue buttons appear. | Idle/active-only states have no queue header; automatic requests have no queued Info notice; rendering never submits an ACP prompt. |
| Queue recall shortcut is disabled while attachments remain intact | Hold A, enqueue B and image-bearing C; physical Alt+R leaves the count and both empty/occupied drafts unchanged. Release A and assert ACP order A/B/C with exactly one PNG block. | No recall button; no duplicate submission or lost attachment. |
| Stopping a turn keeps explicit requests paused without recovery controls | Hold A and cancellation settlement; queue explicit B/C plus automatic Autofix. `/stop` and unselected Ctrl+C retain B/C, discard Autofix, and accept D while stopped. Physical Alt+S and count clicks do not resume, even after settlement. | No automatic replay, cancelled-turn retry, or hidden recovery entry point. |
| Failed turns keep explicit requests paused without recovery controls | Fail held A with explicit and automatic pending requests. Only explicit requests remain; new Enter increments the count. Physical Alt+R/S/D and count clicks preserve waiting work and the draft. | Neither failure nor new input implicitly resumes old work. |
| Queue discard shortcut and count clicks preserve waiting work and draft | Hold A, enqueue B, enter a draft, and attempt physical Alt+D and a content-located count click. B still dispatches after A; the draft submits separately afterward. | No discard, extra cancellation, draft edit, or implicit draft submission. |
| Typed fix preserves captured evidence while waiting | Type `/fix` behind a held turn, wait for accepted snapshot diagnostics, then run a different shell command; the eventual ACP request must retain the old failure, hint, shell and cwd. | Automatic suggestions are off; new output/cwd must not replace the captured evidence. |
| Repeated diagnostics activation submits one fix per failure | Click the same real diagnostics button three times with `session/new` held; one pending entry becomes exactly one ACP prompt after release. | A fresh later failure still needs a click and then runs once; `Feature.AutofixRouting` separately protects two-tab routing. |
| Prompt redraws preserve queued Autofix until a real command starts | Bind Ctrl+L to PSReadLine `InvokePrompt` in the test shell, redraw twice, and observe real OSC 133 A/B without C; the queued request survives and its evidence reaches ACP once. | A subsequent real shell command invalidates a fresh queued automatic fix; `New shell commands invalidate obsolete queued Autofix` also protects later failure recovery. |

Queue-count assertions poll the owning helper's current alternate-screen viewport
for the automatic pinned header, or its absence when no requests remain. They
never type an inspection command, consult diagnostics, or scroll chat history to
find a count. The single line includes waiting automatic requests and remains outside chat
scrolling, with no previews, overflow suffix, or controls. Existing
startup, actionable-detection, pending-list, cancellation, failure, source-pane,
and invalidation cases remain in the same suite.

Enter always admits an independent FIFO request. Stopped requests are only
in-memory: there is no cross-restart persistence. The en-US count is
`1 message queued` or `N messages queued`, including when paused. Alt+R,
Alt+S, and Alt+D are disabled, and the count is not clickable. Stopped
requests temporarily have no recovery/discard UI. Internal recall, resume,
and discard logic remains covered by unit tests. Completed `/fix` captures
survive waiting or stopping; incomplete/invalid captures still require
resubmission internally, but that entry point is currently unavailable.
Detailed soft-stop, snapshot lifecycle, readiness, and stale-click cases are
unit-tested; this suite adds real packaged input/render/ACP boundaries, not
claims of E2E coverage for every lifecycle branch.

Disabled queue shortcuts and unselected Ctrl+C use `Send-WtWindowKey` with required
foreground ownership after focusing the known helper pane. They traverse the
real Terminal keybinding layer before reaching ConPTY; raw `wtcli send-keys`
would bypass that layer and could falsely pass a conflicting shortcut. Alt+R,
Alt+S, and Alt+D deliberately avoid Terminal's default Alt+Up pane-focus and
Alt+Enter fullscreen bindings. Noninteractive count clicks remain content-located
ConPTY clicks. Run physical shortcut cases with an English (US) input layout active.

**Safe-run prerequisite:** the user must save their work and close all existing
Dev windows before running this suite. `Start-Terminal` closes stale instances
of the selected package and temporarily replaces its settings; it is not a
non-disruptive test against an already open window. Do not run these live cases
while the user is testing or working in Dev. Discovery-only checks and the
hermetic queue-fixture self-tests do not launch Terminal or change its settings.

After the user has closed Dev, and the combined changes have been built and
deployed into the explicitly selected **Dev** package:

```powershell
$env:ITE2E_PACKAGE = 'Dev'
pwsh -NoProfile -File test\e2e\bootstrap.ps1 -Check
$paths = @(
    'test\e2e\tests\Feature.PromptQueue.Tests.ps1'
    'test\e2e\tests\Feature.AgentSelectAll.Tests.ps1'
    'test\e2e\tests\Feature.AutofixRouting.Tests.ps1'
    'test\e2e\tests\Feature.AutofixParser.Tests.ps1'
    'test\e2e\tests\Feature.AgentPanePadding.Tests.ps1'
)
& .\test\e2e\Invoke-ItE2EReport.ps1 -Path $paths `
    -OutDir test\e2e\artifacts\pending-queue-dev
```

Use just the first path for the smallest queue-only live run. The related
SelectAll suite protects selected Ctrl+C from becoming Stop; the Autofix and
padding suites protect routing, failure producers, and the shared pinned layout.
The first report run above regenerates from the current checklist in a dedicated
output directory. Later focused reruns can add `-UpdateReport` with the same
`-OutDir` to preserve unrelated results. An old report with obsolete Stop/failure
titles or missing new IDs must first be regenerated via `New-ReleaseReport.ps1`
using retained applicable results XML; incremental overlay cannot add or rename
checklist rows. Verify C095 and C410-C421 are `[x]` after a passing run; failures
must show `AUTOMATION FAILED`, and skipped-only overlays must preserve prior
checkboxes. These unshipped cases are not credited by the historical Store
totals above. Existing checklist IDs are not renumbered.

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
  total/passed/failed/skipped stat cards, one **failure card** per failed test or setup/cleanup block (exact error,
  `file:line` of the failing assertion, duration, clickable artifact links + inline screenshot
  thumbnails), and a full results table grouped by `Describe > Context`.
- `results.xml` — **NUnit XML** for CI test reporting (Azure DevOps / GitHub).
- `summary.md` — Markdown: one block per **failed** test or setup/cleanup block with the **exact error**, **file:line**,
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
  If Pester reports a structural setup/cleanup failure or no test passes, both full and incremental
  runner modes replace the release report with a blocked notice and **no checked
  items**, rather than crediting test cases that passed before the failure.
- Console echo of the same precise failures; exit code `1` on any failure (CI-friendly).
  Zero selected or passing cases and setup/report-generation errors are non-green,
  even with no failing test assertion. A mixture of passes and externally gated
  skips is reported as `PASSED WITH SKIPS`; use `-RequireNoSkips` for strict PR
  acceptance. An all-skipped run never claims `ALL PASSED`.

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
