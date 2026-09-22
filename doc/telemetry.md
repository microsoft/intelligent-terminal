# Intelligent Terminal Usage Report

**Report date:** 2026-09-21
**Latest published release:** v0.2.2572 (tag v0.2.2572, published 2026-09-14)

## Summary by distribution channel

| Channel | Metric | Count |
| --- | --- | --- |
| **GitHub Releases** | `.msixbundle` downloads, highest single release (v0.1.1521) | **20,527** |
| **Microsoft Store** (Partner Center) | Store installs / acquisitions | **13,000** |
| **Total (across channels)** | Combined reach | **33,527** |

> GitHub counts the single highest-downloaded release rather than the sum across releases, since the same user downloading each upgrade would otherwise be counted once per release.

> Winget is excluded: its installs are served from the same GitHub Releases assets, so counting it separately double-counts those downloads.

## Product telemetry

28-day window ending 2026-09-07.

| Event | Scaled device count | Scaled event count |
| --- | --- | --- |
| `Setting.Model.AgentProviderConfigured` | 4,362 | 212,782 |
| `App.ConnectionCreated` | 4,309 | 534,066 |
| `Win32Host.SessionBecameInteractive` | 4,145 | 21,667 |
| `WTA.AcpInitializeComplete` | 2,305 | 8,499 |
| `WTA.AcpNewSessionComplete` | 2,274 | 11,487 |
| `WTA.ErrorDetected` | 2,181 | 54,651 |
| `App.ErrorDetected` | 915 | 11,284 |
| `App.ActionDispatched.SplitPane` | 705 | 978 |
| `WTA.ErrorFixResolved` | 87 | 369 |
| `WTA.AgentPromptSent` | 79 | 315 |
| `WTA.AgentResponseComplete` | 77 | 289 |
| `WTA.AgentResponseFirstToken` | 74 | 281 |

### Agent funnel

| Step | Scaled device count | Of previous step | Of configured |
| --- | --- | --- | --- |
| Provider configured | 4,362 | | 100% |
| ACP initialize complete | 2,305 | 53% | 53% |
| ACP new session complete | 2,274 | 99% | 52% |
| Agent prompt sent | 79 | 3.5% | 1.8% |
| Agent response first token | 74 | 94% | 1.7% |
| Agent response complete | 77 | 97% | 1.8% |

Two drop-offs account for nearly all the loss. 53% of provider-configured devices start an agent session within the window, and 3.5% of those send a prompt. Once a prompt is sent, 97% get a complete response back.

## Funnels to build

These are the funnels we need the data team to build so we can tell what is being used and what is useful, across what ships today and what is going out next. Event names use the existing `Microsoft.Windows.Terminal.*` convention, shortened the same way as the table above. The current implementation follows these funnel names where the corresponding product behavior exists.

**Implementation scope (2026-09-22):** instrument existing functionality only. Sidebar search/filter/pinning/rich-row customization and keep-running-across-terminal-exit are not implemented in this checkout, so those events remain **Deferred**, not empty definitions or renamed proxies. "Implemented" below describes source instrumentation, not a claim of release deployment or backend ingestion.

- Anything that is a setting needs a state event on every launch, not only a toggle event when it changes. Someone who turns the sidebar on once and leaves it alone emits nothing after that day, so toggle events on their own cannot measure adoption or retention.
- Keep content out of these events. No prompt or conversation text, no search queries, no directory paths, branch names, repo names, or tab titles. Use lengths, counts, booleans, and enums; opaque correlation IDs identify sessions or offers without encoding their content.
- Report all of it on the same 28-day window used above, cut at day 0, 7, and 28.

### 1. General usage

| # | Step | Event | Status | What it answers |
| --- | --- | --- | --- | --- |
| 1.1 | Window became interactive | `Win32Host.SessionBecameInteractive` | Exists | Active devices, the denominator for everything below |
| 1.2 | Shell connection created | `App.ConnectionCreated` | Exists | Whether a launch turns into real terminal use |
| 1.3 | Agent provider configured | `Setting.Model.AgentProviderConfigured` | Implemented: launch-only schema v2 | Configured provider state, separate from provider changes. See row 7.1 |
| 1.4 | Agent session started | `WTA.AcpNewSessionComplete` | Exists | Crossover from terminal use to agent use |
| 1.5 | Returned on a later day | derived from 1.1 | Derived; backend query required | Day 7 and day 28 retention, using stable backend device identity and timestamps |

### 2. Agent pane

| # | Step | Event | Status | What it answers |
| --- | --- | --- | --- | --- |
| 2.1 | ACP session started | `WTA.AcpNewSessionComplete` | Exists | Setup completing |
| 2.2 | Prompt sent | `WTA.AgentPromptSent` | Exists | First real use |
| 2.3 | Response complete | `WTA.AgentResponseComplete` | Exists | Turn completion rate |
| 2.4 | Second prompt in the same session | derived from 2.2 | Derived; backend query required | At least two qualifying dispatches per scoped `SessionId`; exclude autofix and define whether agent commands count |
| 2.5 | Slash command used | `WTA.AgentSlashCommandUsed` with `command` | Implemented; renamed | Built-in command dispatch, formerly `WTA.SlashCommandInvoked.CommandName`; agent-provided command names remain private |

### 3. Error detection and fix

| # | Step | Event | Status | What it answers |
| --- | --- | --- | --- | --- |
| 3.1 | Command failed | `WTA.ErrorDetected` | Exists; policy fields added | Segment by `AllowAutoFixPolicy` (`notConfigured`, `enabled`, `disabled`, `unknown`) and the separate effective `AutoFixEnabled` switch |
| 3.2 | Fix offered to the user | `WTA.ErrorFixOffered` with `OfferId` | Implemented | First successful unobscured rendering of a concrete autofix recommendation in an open pane; not analysis or prose-only results |
| 3.3 | Offer accepted | `WTA.ErrorFixAccepted` with `OfferId` | Implemented | User confirms Run for the same offer and execution is queued; not Insert, requesting analysis, or proof of fix success |

Join 3.2 to 3.3 by the random `OfferId`. There is no exact 3.1-to-3.2 incident join: multiple error signals can describe one failure, and a manual `/fix` can produce an offer without a preceding classified error.

### 4. Command palette kick off

| # | Step | Event | Status | What it answers |
| --- | --- | --- | --- | --- |
| 4.1 | `?` prompt mode entered | `App.CommandPaletteAgentPromptEntered` | Implemented | Entering visible foreground agent mode, including direct delegation actions; counts entry without requiring submission |

Keep `App.CommandPaletteDispatchedAgentPrompt` as the separate submission event (`IsBackgroundMode=false` for `?`). It is not renamed to an entry event because the boundaries differ.

### 5. Sidebar

| # | Step | Event | Status | What it answers |
| --- | --- | --- | --- | --- |
| 5.1 | Sidebar state at launch | `App.SidebarStateOnLaunch` with `enabled` | Implemented | Vertical-tab sidebar state at app launch, including launches without any agent session |
| 5.2 | Search entered | `App.SidebarSearchOpened` | Deferred: product feature absent | Whether search is reached at all |
| 5.3 | Agent filter applied | `App.SidebarAgentFilterApplied` with `row_count` | Deferred: product feature absent | Use of the filter that sits behind search |
| 5.4 | Tab pinned | `App.SidebarTabPinned` with `pinned_count` | Deferred: product feature absent | Pin use |
| 5.5 | Row fields changed | `App.SidebarRowFieldsChanged` with `fields` | Deferred: product feature absent | Which two rich tab fields people pick, chosen from the filter icon in the sidebar |

### 6. Durable sessions

| # | Step | Event | Status | What it answers |
| --- | --- | --- | --- | --- |
| 6.1 | Marked keep running | `WTA.KeepRunningMarked` | Deferred: product lifecycle absent | Mark rate per agent session |
| 6.2 | Terminal closed with it alive | `WTA.KeepRunningDetached` | Deferred: product lifecycle absent | Whether the mark took effect |
| 6.3 | Reattached on relaunch | `WTA.KeepRunningReattached` | Deferred: product lifecycle absent | Whether the promise held. Carry the outcome: live, gone, or failed |
| 6.4 | Prompt sent after reattach | `WTA.AgentPromptSent` with `reattached=true` | Deferred: property not emitted | Whether the surviving session got used |

Existing ACP load/resume events and `App.AgentSessionStarted(StartKind=Load)` remain unchanged. They measure loading saved history, not a process surviving terminal exit; no synthetic `reattached` flag is added.

### 7. Settings configuration

| # | Step | Event | Status | What it answers |
| --- | --- | --- | --- | --- |
| 7.1 | Provider configured state at launch | `Setting.Model.AgentProviderConfigured` | Implemented: launch-only schema v2 | Filter `schema_version=2`; one record per `role` (`primary`, `delegate`) per app launch, with configured/effective provider categories, selection origin, and defaults-fallback flag |
| 7.2 | Provider changed | `Setting.Model.AgentProviderChanged` with `from`, `to` | Implemented | Per-role provider changes between successful settings loads, not sessions, unchanged reloads, or policy-only changes; custom-to-custom switches remain visible without disclosing names |
| 7.3 | Custom agent configured | `Setting.Model.CustomAgentConfigured` | Implemented | Per-role launch counts, selected/matching-command flags, `AllowedAgents` / `AllowCustomAgents` policy categories; includes unused configuration and zero counts without emitting names or commands |

**Migration and counting:** `AgentProviderConfigured` was previously retired and is now reintroduced with `schema_version=2`; the historical 212,782 count is not directly comparable to this launch-only contract. Each launch has two role records, so filter/group by `role`, not their sum. `WTA.SlashCommandInvoked.CommandName` is renamed to `WTA.AgentSlashCommandUsed.command` without dual-writing. Keep distinct entry/submission and configuration/session events distinct rather than renaming semantically different signals.

**Current disposition of the original New items:** 7 implemented (2.5, 3.2, 3.3, 4.1, 5.1, 7.2, 7.3), 2 derived (1.5, 2.4), and 7 deferred pending product functionality (5.2-5.5, 6.1-6.3). The additional new property in 6.4 is also deferred. Existing rows 3.1 and 7.1 now have policy segmentation and launch/change separation respectively. Full typed schemas and lifecycle rules are in `doc\intelligent-terminal-telemetry.md`.

### Pre-implementation New-item audit

**Historical baseline:** audited 2026-09-22 at source revision `ad9413b25`, before the funnel implementation. This table preserves the original 16 New requirements and their old coverage for migration comparison; the numbered funnel tables above describe the implementation status. "Exists" here means an ETW definition **and a wired emission path**, not proof that a published release or the telemetry backend contains it. Historical counts above are unchanged. Local diagnostic logs and internal state notifications are not telemetry events.

Of the **16 rows marked New**, **2 are derivable**, **1 has an existing differently named event for built-in commands**, **6 have partial coverage only**, and **7 have no matching event found**. Row 6.4 is listed separately because it requests a new property on an existing event. Field names below use their actual, case-sensitive spelling. See `doc\intelligent-terminal-telemetry.md` for full schemas and provider identities.

| Requirement | Proposed event / metric | Existing event / fields to search | Finding and remaining gap | Evidence |
| --- | --- | --- | --- | --- |
| 1.5 Returned on a later day | Derived retention | `Win32Host.SessionBecameInteractive` | **Derivable; no new client event needed.** Build day-0 cohorts and day-7/day-28 return queries using event timestamps and the backend's stable device identity, if available. The event payload itself has no device ID; aggregate scaled counts alone cannot produce retention. | S1 |
| 2.4 Second prompt in the same session | Derived conversation depth | `WTA.AgentPromptSent`: `SessionId`, `AgentId`, `IsAutofix`, `TemplateKind` | **Derivable; no new client event needed.** Count at least two qualifying dispatches per session, scoped by device/agent and time window. Exclude `IsAutofix=true`; decide whether `TemplateKind=AgentCommand` counts as conversation. Session IDs survive reloads, so this is not necessarily two turns in one app run. It measures dispatch, not successful conversation. | S2 |
| 2.5 Slash command used | `WTA.AgentSlashCommandUsed(command)` | `WTA.SlashCommandInvoked`: `CommandName` | **Exists under a different name for built-in commands.** Emitted on dispatch, before busy/other guards, for `help`, `clear`, `new`, `fix`, `restart`, `stop`, `sessions`, `agent`, `model`, `config`, `move`. Agent-provided commands instead appear as `WTA.AgentPromptSent` with `TemplateKind=AgentCommand`; their command names are not emitted. The original "uninstrumented" description is therefore too broad. | S3, S2 |
| 3.2 Fix offered | `WTA.ErrorFixOffered` | `App.ErrorDetected`; `WTA.AgentPromptSent` with `IsAutofix=true` | **Partial signals, not an offer event.** App `ErrorDetected` reports receipt of `autofix_state=pending`, before UI routing; it does not report the `detected` ask-for-fix pill or the `review` result. Autofix prompts measure analysis dispatch, not a displayed recommendation. Neither proves the user saw an offer, and no incident ID joins the funnel. | S4, S2 |
| 3.3 Offer accepted | `WTA.ErrorFixAccepted` | `App.AgentPaneOpened` with `TriggerSource=Autofix`; `WTA.AgentPromptSent` with `IsAutofix=true`; `WTA.SlashCommandInvoked` with `CommandName=fix` | **Partial signals, not acceptance.** Opening the pane can mean requesting analysis or reviewing a result, and already-open paths need not emit the open event. Autofix dispatch includes automatic and manual requests; `/fix` is a command attempt. There is no event identifying acceptance of a specific offer or execution of its fix. | S5, S2, S3 |
| 4.1 `?` prompt mode entered | `App.CommandPaletteAgentPromptEntered` | `App.CommandPaletteDispatchedAgentPrompt`: `IsBackgroundMode=false` | **Partial: submission exists, entry does not.** Counts submission of a nonempty foreground prompt, not merely entering `?` mode, abandoning it, or submitting bare `?`. If the question is whether users submit a `?` prompt, reuse this event; an entry-to-submit conversion funnel still needs the entry signal. The original "nothing in this flow is instrumented" description is incorrect. | S6 |
| 5.1 Sidebar state at launch | `App.SidebarStateOnLaunch(enabled)` | `App.AgentSessionStarted`: `VerticalTabs`; inherited settings-change events for `tabLayout` | **Partial: session snapshot / changes, not launch state.** `VerticalTabs` records vertical layout after a successful agent session start/load, including prewarm. It excludes launches without a successful agent session and can repeat within a launch. Settings changes record setting paths, not the resulting enabled state. Neither measures sidebar adoption across all launches. | S7, S8 |
| 5.2 Sidebar search entered | `App.SidebarSearchOpened` | No matching event found | **Missing.** Command Palette and Suggestions search telemetry does not identify sidebar search entry. | S9 |
| 5.3 Sidebar agent filter applied | `App.SidebarAgentFilterApplied(row_count)` | No matching event or result-count field found | **Missing.** Generic `App.ActionDispatched` is not a substitute for an identified sidebar-filter operation and its resulting row count. | S9 |
| 5.4 Tab pinned | `App.SidebarTabPinned(pinned_count)` | No matching event or pin-count field found | **Missing.** No source-backed pin operation plus resulting pin count was found in telemetry. | S9 |
| 5.5 Sidebar row fields changed | `App.SidebarRowFieldsChanged(fields)` | No matching event or selected-field payload found | **Missing.** Generic action/settings-change events do not provide the selected pair of rich row fields. | S8, S9 |
| 6.1 Marked keep running | `WTA.KeepRunningMarked` | No matching event found | **Missing.** Persisting an agent session's resume metadata is not a user mark to keep a live process running. Do not count ordinary persistence as this step. | S10 |
| 6.2 Terminal closed with session alive | `WTA.KeepRunningDetached` | No matching event found | **Missing.** No telemetry establishes that the marked session remained live after terminal closure. Pane stashing or moving between windows is not this lifecycle boundary. | S10 |
| 6.3 Reattached on relaunch | `WTA.KeepRunningReattached` with live/gone/failed outcome | `WTA.AcpLoadSessionComplete`: `Success`, `DurationMs`; `App.AgentSessionStarted`: `StartKind=Load`, `SessionId`, `StartId`; `WTA.SessionResumeInvoked`: `Route`, `AgentId` | **Partial: load/resume telemetry, not live reattachment.** ACP loads cover saved-layout restore and explicit resume. Load completion has no session ID or origin and occurs before stale-result rejection; the App snapshot reports an accepted successful load. Resume-invoked is a session-view dispatch. None proves process survival or supplies live/gone/failed outcomes. | S7, S10, S11 |
| 7.2 Provider changed | `Setting.Model.AgentProviderChanged(from,to)` | No matching change event; `App.AgentSessionStarted.AgentId` is only a session snapshot | **Missing.** Consecutive session agent IDs can differ because of different tabs or resumes, not a settings change. AI provider keys are explicitly excluded from `Setting.Model.JsonSettingsChanged` / `UISettingsChanged`, and those generic events carry no old/new values anyway. | S7, S8 |
| 7.3 Custom agent configured | `Setting.Model.CustomAgentConfigured` | `App.AgentSessionStarted`: `AgentId=custom` or `DelegateAgentId=custom`; `WTA.AgentPromptSent`: `AgentId=custom` | **Partial: custom-agent use, not configuration inventory.** These identify connected/resolved agents or prompt dispatches, not unused custom-agent configuration. No `AllowedAgents` / `AllowCustomAgents` policy dimensions are emitted. `custom` is the privacy-safe bucket, not a custom name or command. | S7, S2, S8 |
| 6.4 Prompt after reattach (new property, not a New event) | `WTA.AgentPromptSent(reattached=true)` | `WTA.AgentPromptSent.SessionId`; `App.AgentSessionStarted` with `StartKind=Load` and `SessionId` | **Partial; property missing.** Session/time correlation can approximate prompts after a successful load, but not specifically live reattachment. Prompts have neither `reattached` nor `StartId`/load-origin fields; repeated loads prevent an exact load-to-turn join from `SessionId` alone. | S2, S7, S11 |

**Related corrections to existing rows:** `Setting.Model.AgentProviderConfigured` (1.3/7.1) is retired in current source, not an existing event waiting to be split. Its replacement, `App.AgentSessionStarted`, is a successful-session settings snapshot, not an every-launch configured-base census or a change feed. `WTA.ErrorFixResolved` in the historical report is also retired: dismissal or a later successful shell command cannot establish fix success. Keep historical and current event contracts separate.

**Policy segmentation is not already covered:** `WTA.ErrorDetected` has only `Severity`, `Method`, and `PaneId`, not `AllowAutoFix`. The App snapshot's `AutoFix` is an effective boolean; `false` does not distinguish user preference from policy blocking. It cannot supply the requested policy breakdown for 3.1 or 7.3.

#### Audit evidence / lookup locations

Locations are relative to the repository root at the audited revision. For missing events, the evidence is the current telemetry catalog and searched emitters, not a claim that the underlying product feature is absent.

| Ref | Source / lookup location |
| --- | --- |
| S1 | `src\cascadia\WindowsTerminal\WindowEmperor.cpp:720-745` — first-interaction emission and actual payload. |
| S2 | `tools\wta\src\telemetry.rs:188-214` — prompt schema; `tools\wta\src\protocol\acp\client.rs:5954-5974` — guarded dispatch emission. |
| S3 | `tools\wta\src\telemetry.rs:275-285` — slash schema; `tools\wta\src\app.rs:5791-5818` — built-in dispatch callsite. |
| S4 | `src\cascadia\TerminalApp\TerminalPage.cpp:6665-6757` — pending-only App event before routing; `tools\wta\src\app\autofix.rs:106-176` — automatic/manual analysis and detected-pill branch. |
| S5 | `src\cascadia\TerminalApp\TerminalPage.cpp:4014-4080` — diagnostics request/review paths; `:4946-4953` — pane-open event. |
| S6 | `src\cascadia\TerminalApp\CommandPalette.cpp:909-934` — nonempty submission event; `:1081-1095` — mode transition. |
| S7 | `src\cascadia\TerminalApp\TerminalPage.cpp:7277-7307` — successful-session snapshot and all fields; `doc\intelligent-terminal-telemetry.md`, "App.AgentSessionStarted" — lifecycle and category contract. |
| S8 | `src\cascadia\TerminalSettingsModel\SettingsTelemetry.h:10-61` — AI settings exclusions; `src\cascadia\TerminalSettingsModel\CascadiaSettingsSerialization.cpp:1907-1986` — generic changes and path-only payload; `doc\intelligent-terminal-telemetry.md`, "Retired events and settings filtering". |
| S9 | Current C++ telemetry emitters under `src\cascadia\TerminalApp`; `AppLogic.cpp:193-201` — `AppCreated` only has `TabsInTitlebar`; `ShortcutActionDispatch.cpp:67-77` — generic action/branding payload, no sidebar-specific operation or counts. |
| S10 | `src\cascadia\TerminalApp\AgentPaneContent.cpp:382-429` — persisted resume metadata; `tools\wta\src\helper\runtime.rs:1027-1105` — boot-time ACP load; `tools\wta\src\telemetry.rs` — WTA event definitions, no keep-running mark/detach events. |
| S11 | `tools\wta\src\telemetry.rs:169-180,298-309` — load/resume schemas; `tools\wta\src\protocol\acp\client.rs:4650-4678` — load event before stale-result checks; `tools\wta\src\app.rs:2957-2965` — session-view resume dispatch. |

## GitHub Releases detail

Download counts per published release, as of 2026-09-21.

| Release | Tag | Published | `.msixbundle` | GPO templates zip |
| --- | --- | --- | --- | --- |
| v0.2.2572 | `v0.2.2572` | 2026-09-14 | 4,110 | 65 |
| v0.2.2395 | `v0.2.23` | 2026-08-28 | 2,873 | 82 |
| v0.2.2192 | `v0.2` | 2026-08-10 | 6,621 | 145 |
| v0.1.1841 | `v0.1.18` | 2026-07-10 | 7,033 | 140 |
| v0.1.1681 | `v0.1.1` | 2026-06-17 | 11,537 | 205 |
| v0.1.1521 | `v0.1.0` | 2026-05-29 | 20,527 | 266 |
| **Sum (all releases)** | | | **52,701** | **903** |

## Microsoft Store detail

Partner Center acquisitions for product `9NMQC2SSJX24`, last 6 months.

| Metric | Count |
| --- | --- |
| Page views | 76,320 |
| Install attempts | 13,450 |
| Successful installs | 13,240 |
| First-time launches from Store | 8,070 |
| Conversion (installs by page views) | 17.35% |
| Install success rate | 99.47% |
