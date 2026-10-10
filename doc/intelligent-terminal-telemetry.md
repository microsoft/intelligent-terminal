# Intelligent Terminal telemetry reference

This is the complete catalog of Intelligent Terminal's **AI/Agent telemetry**:
each current event, its trigger, typed business fields, and measurement
meaning. It describes implemented source behavior, not proposed events,
test results, or deployment history.
Implemented source behavior does not imply that the PR has merged or that
backend ingestion and production funnel queries have been verified.

The scope is **38 event definitions**: 16 App, 18 WTA, 1 Settings Model,
and 3 Settings Editor. This includes the existing `AppCreated` event,
extended with the startup configuration snapshot.
An event is identified by **provider name plus event name**, not by event
name alone. In particular, App and WTA each define their own `ErrorDetected`
and `DelegateInvoked`.

Inherited Terminal/OpenConsole events are outside the 38-event catalog.
The report mapping below also references `UserInteract`, `SessionBecameInteractive` and
`ConnectionCreated` because they supply its general-usage measurements.
The separate inherited reference is [TelemetryEvents.md](../TelemetryEvents.md#inherited-windows-terminal--openconsole-reference).
See [privacy information](../PRIVACY.md) for collection controls.

## Contents

- [Event catalog](#event-catalog)
- [Usage report requirements and queries](#usage-report-requirements-and-queries)
- [Providers and common metadata](#providers-and-common-metadata)
- [App event schemas](#app-event-schemas)
- [WTA event schemas](#wta-event-schemas)
- [Settings Model event schemas](#settings-model-event-schemas)
- [Settings Editor event schemas](#settings-editor-event-schemas)
- [Counting and correlation](#counting-and-correlation)
- [Retired events and settings filtering](#retired-events-and-settings-filtering)
- [Privacy and collection boundaries](#privacy-and-collection-boundaries)
- [Source references](#source-references)

## Event catalog

All **38 current AI/Agent event definitions** are listed here. Follow an
event link for its complete field names, types, values, and trigger rules.
Business-field counts exclude the common `PartA_PrivTags` field.

| Provider | Event | What it records / trigger | Business fields | Privacy category |
|---|---|---|---|---|
| App | [AppCreated](#appappcreated) | Window-created provider, custom-agent, policy, and sidebar configuration snapshot | 13 | Usage |
| App | [AgentPaneOpened](#appagentpaneopened) | Instrumented assistant-open or sessions-view request | 2 | Usage |
| App | [CommandPaletteAgentPromptEntered](#appcommandpaletteagentpromptentered) | Entry into visible foreground agent prompt mode | 1 | Usage |
| App | [CommandPaletteDispatchedAgentPrompt](#appcommandpalettedispatchedagentprompt) | Submission of an agent prompt through the Command Palette | 2 | Usage |
| App | [SidebarSearchOpened](#appsidebarsearchopened) | Explicit opening of sidebar tab search | 0 | Usage |
| App | [SidebarAgentFilterApplied](#appsidebaragentfilterapplied) | First successfully loaded Agent-view row count after entry | 1 | Usage |
| App | [SidebarTabPinned](#appsidebartabpinned) | Sidebar Keep running enable action and resulting kept-tab count | 1 | Usage |
| App | [TabPinChanged](#apptabpinchanged) | Successful tab-order pin or unpin | 2 | Usage |
| App | [KeepRunningMarked](#appkeeprunningmarked) | Keep running opt-in with paired total/kept terminal-tab counts | 4 | Usage |
| App | [KeepRunningDetached](#appkeeprunningdetached) | Marked tab successfully retained after closing | 2 | Usage |
| App | [KeepRunningReattachStarted](#appkeeprunningreattachstarted) | Start of one retained-tab restore attempt | 1 | Usage |
| App | [KeepRunningReattached](#appkeeprunningreattached) | Retained-tab restore outcome, including pre-transfer failures | 5 | Usage |
| App | [SidebarRowFieldsChanged](#appsidebarrowfieldschanged) | Selected rich-tab field IDs at window startup or user change | 2 | Usage |
| App | [DelegateInvoked](#appdelegateinvoked) | Successful launch of the delegate process | 1 | Usage |
| App | [ErrorDetected](#apperrordetected) | Receipt of a pending autofix-state projection | 1 | Usage |
| App | [AgentSessionStarted](#appagentsessionstarted) | Successful ACP creation/load and effective configuration snapshot | 23 | Usage |
| WTA | [AcpInitializeComplete](#wtaacpinitializecomplete) | ACP initialization attempt duration and outcome | 5 | Performance |
| WTA | [AcpNewSessionComplete](#wtaacpnewsessioncomplete) | ACP session creation attempt duration and outcome by route | 5 | Performance |
| WTA | [AcpLoadSessionComplete](#wtaacploadsessioncomplete) | ACP session load attempt duration and outcome | 2 | Performance |
| WTA | [AgentColdStartComplete](#wtaagentcoldstartcomplete) | Cold agent-process startup and initialization outcome | 5 | Performance |
| WTA | [AgentPromptSent](#wtaagentpromptsent) | ACP prompt dispatch with observation, turn and restore correlation | 12 | Usage |
| WTA | [AgentResponseFirstToken](#wtaagentresponsefirsttoken) | Latency and byte length of the first counted text/thought chunk | 3 | Performance |
| WTA | [AgentResponseComplete](#wtaagentresponsecomplete) | Correlated prompt RPC completion duration and outcome | 7 | Performance |
| WTA | [ErrorDetected](#wtaerrordetected) | Actionable/critical classification with candidate repair-flow ID | 7 | Usage |
| WTA | [ErrorFixOffered](#wtaerrorfixoffered) | First visible presentation in a repair flow | 2 | Usage |
| WTA | [ErrorFixAccepted](#wtaerrorfixaccepted) | Confirmed Run request successfully queued in a presented flow | 2 | Usage |
| WTA | [ErrorFixRunStarted](#wtaerrorfixrunstarted) | Executor dequeues one confirmed Run | 2 | Usage |
| WTA | [ErrorFixRunResult](#wtaerrorfixrunresult) | Existing dispatch returns; execution remains unknown or dispatch failed | 3 | Usage |
| WTA | [AgentSlashCommandUsed](#wtaagentslashcommandused) | Built-in slash-command dispatch | 1 | Usage |
| WTA | [SessionsViewOpened](#wtasessionsviewopened) | Entry into the session-view open routine | 0 | Usage |
| WTA | [SessionResumeInvoked](#wtasessionresumeinvoked) | Dispatch of an ACP or native-CLI resume route | 2 | Usage |
| WTA | [DelegateInvoked](#wtadelegateinvoked) | Agent-requested delegation after creating its target | 1 | Usage |
| WTA | [SessionMcpToolCalled](#wtasessionmcptoolcalled) | Parsed session MCP tool call reaching dispatch | 1 | Usage |
| WTA | [HookOperationCompleted](#wtahookoperationcompleted) | Per-CLI hook installation/removal outcome | 3 | Usage |
| Model | [AgentProviderChanged](#modelagentproviderchanged) | Primary/delegate provider change on an accepted settings reload | 3 | Usage |
| Editor | [AcpModelProbeStarted](#editoracpmodelprobestarted) | Start of a clean ACP model-catalog probe | 2 | Performance |
| Editor | [AcpModelProbeDiscarded](#editoracpmodelprobediscarded) | Discard of a superseded probe generation | 1 | Performance |
| Editor | [AcpModelProbeCompleted](#editoracpmodelprobecompleted) | Current probe's parsed catalog outcome and model count | 3 | Performance |

## Usage report requirements and queries

The following tables map **all 26 rows, 1.1 through 7.3**, in the
*Intelligent Terminal Usage Report* to current event names and query logic.
They describe how to build the requested views, not validation results.
An available event does not necessarily support the exact funnel implied by
the original wording; the last column identifies those limits.

### Shared query conventions

These are backend-neutral query recipes, not executable queries against
an assumed table. Map provider, event name, timestamp, payload fields, and
the backend's device dimension to the actual ingestion schema first.

| Notation | Query meaning |
|---|---|
| `E(App.Event)` | Records matching the exact provider and event name, after common time/build/population filters. Expand App, WTA, Model, and Editor using the provider table below; Win32Host means `Microsoft.Windows.Terminal.Win32Host`. |
| `N(X)` | Event count after filtering `X`; not a device or session count. |
| `Devices(X)` / `D(X)` | Set / distinct count of backend device IDs observed in `X`. Device identity is backend metadata, not an AI event payload field. |
| `Ids(X, key)` / `U(X, key)` | Set / distinct count of the named payload key, such as `StartId`, `OfferId`, or `KeepId`. These keys are not device IDs. |
| `Active` | `Devices(E(Win32Host.UserInteract))` in the selected observation window. |
| Reach of `X` | `size(Devices(X) intersect Active) / size(Active)`. Use the intersection so the numerator belongs to the denominator's population. |
| Rate | Floating-point division on the same eligible population. An empty denominator is unavailable, not zero. |

Use the same **28-day reporting window** `[end - 28 days, end)`, timezone,
build/schema scope, distribution scope, and device eligibility across views.
Not every event carries `Branding` or `Distribution`; apply those dimensions
only where the payload or a documented backend enrichment supplies them.
Keep retired schemas separate rather than treating a renamed or consolidated
event as a continuous count series. A device without a configuration snapshot
has unknown state; do not silently classify it as disabled or unconfigured.

`UserInteract` observes the first qualifying keyboard message per process per
UTC day. Deduplicate by backend device and UTC event date, including multiple
processes on the same device. There is no idle heartbeat. A backwards clock
adjustment does not re-count an earlier day in that process. Only include
versions implementing the new event; historical missing activity cannot be
backfilled from the once-per-process `SessionBecameInteractive` event.

For **D0/D7/D28**, choose an explicit cohort-entry rule, such as the first
observed interactive day in a documented historical window. D0 is that device's
entry day; exact-day D7/D28 retention means interaction on entry day + 7/+28.
Only include cohorts with enough elapsed follow-up. Fetch follow-up events
beyond the 28-day cohort-selection window where necessary; an immature
cohort is not a non-returning device. First observed is not necessarily first
ever, and a rolling 28-day active-device count is not D28 retention.

The report's **scaled** counts require the data team's sampling/weighting
method. Raw event counts, distinct device counts, and scaled estimates are
different measures; do not sum scaled device counts across events. A funnel
requires intersections of eligible device sets (and event order if required),
not division of two independently aggregated device counts. In particular,
first-token and completion populations need not be nested.

### 1. General usage

| # | Required measurement | Current event / fields | Query / aggregation | Interpretation boundary |
|---|---|---|---|---|
| 1.1 | Keyboard-active devices | `Win32Host.UserInteract` | Deduplicate device/day; use devices in the window as `Active`. Segment by `Branding` and `Distribution`. | Process/day observations, not per-keystroke counts; background output and agent activity are not user interaction. |
| 1.2 | Shell connection created; terminal use after launch | `App.ConnectionCreated` | Report `N(E)`, `D(E)`, and reach; filter `ConnectionTypeGuid` when a specific connection type is required. | Connection creation does not prove the shell became interactive or executed a command. No exact launch-to-shell conversion is established by counts alone. |
| 1.3 | Agent provider configured; base set up for agent work | `App.AppCreated`: `PrimaryProvider`, `DelegateProvider`, corresponding effective-provider fields | For each role, select snapshots whose configured provider is neither `none` nor `unknown`; report snapshot share and reach. Show configured and effective categories separately. | Replaces the legacy `Setting.Model.AgentProviderConfigured` measurement. Configuration is not installation, authentication, or successful agent use. See 7.1. |
| 1.4 | Agent session started; terminal-to-agent crossover | `App.AgentSessionStarted`: `StartId`, `StartKind`; `WTA.AcpNewSessionComplete`: `Success`, `Route` | For host-accepted starts, report `U(E, StartId)` and reach, splitting `New` and `Load`. For successful new-session RPCs, filter `Success=true` and select one route population rather than summing helper and master layers. | Includes pre-warm and does not prove a prompt was sent. RPC completion events without `Success=true` are not successful starts. |
| 1.5 | Returned on a later day | `Win32Host.UserInteract` and backend device/day metadata | For mature cohort `C`, compute `size(C intersect interactive_devices_on_entry_day_plus_n) / size(C)` for `n=7,28`. | Exact-day keyboard activity, including long-lived processes; not mere process survival or first-ever installation. |

### 2. Agent pane

| # | Required measurement | Current event / fields | Query / aggregation | Interpretation boundary |
|---|---|---|---|---|
| 2.1 | ACP setup completing | `WTA.AcpNewSessionComplete`: `Success`, `Route`, `DurationMs`; `App.AgentSessionStarted` | For each selected non-probe RPC route, report attempt count, `N(Success=true) / N(all outcomes)`, and latency percentiles. Use distinct `StartId` for host-accepted session starts. | Do not combine `MasterForward` with helper routes as distinct sessions. Exclude `Probe` from chat setup metrics. |
| 2.2 | Prompt sent; first real user use | `WTA.AgentPromptSent`: `IsAutofix`, `UserPromptOrdinal`, `AgentId` | Filter `IsAutofix=false`; report dispatch count and reach by agent. Filter `UserPromptOrdinal=First` to count first observed user dispatches. | Unfiltered counts include autofix analysis. `First` refers to the helper's observed ACP session, not first-ever use by a device. |
| 2.3 | Response complete; turn completion rate | Prompt and completion `SessionId`, `TurnId`, `IsAutofix` | Form a sent-turn cohort, exclude autofix for user chat, and count matching successful completions / sent turns. | Allow follow-up; missing completion is unknown, not automatically failure. RPC success is not task success. |
| 2.4 | Second prompt in the same session | `WTA.AgentPromptSent`: `SessionId`, `IsAutofix`, `UserPromptOrdinal` | Form a non-autofix `First` session-ID cohort, then count members with `Second` during follow-up. | Opaque helper-observation IDs, not provider transcript identities. Fresh helpers use new IDs and restart at First; Keep running preserves the surviving observation. |
| 2.5 | Slash-command usage | `WTA.AgentSlashCommandUsed`: `command` | Group by `command`; report `N(E)`, `D(E)`, and reach. | Built-in dispatch before command guards; not execution success or agent-provided command names. |

### 3. Error detection and fix

| # | Required measurement | Current event / fields | Query / aggregation | Interpretation boundary |
|---|---|---|---|---|
| 3.1 | Command failures, segmented by policy | `WTA.ErrorDetected`: `Severity`, `Method`, `AllowAutoFixPolicy`, `AutoFixEnabled` | Report counts and devices by all four fields. Treat `AllowAutoFixPolicy=disabled` as policy-blocked; retain `notConfigured`, `enabled`, and `unknown` separately. | Classified actionable/critical signals, not all command failures or unique incidents. `AutoFixEnabled=false` alone does not establish a policy block. |
| 3.2 | Detection-to-offer conversion | Detection and offer `OfferId`, `Source` | Form a detected-ID cohort and intersect with presented IDs during follow-up. Segment `Method` and policy. | Independent Manual flows have no detection. Classifications include non-command signals and do not guarantee analysis eligibility; do not count all offers over all detections. |
| 3.3 | Offer accepted; value of the offer | `WTA.ErrorFixAccepted` joined to `WTA.ErrorFixOffered` by `OfferId` | Choose an offered-ID cohort `O`; compute `size(O intersect Ids(accepted events, OfferId)) / size(O)`. Allow a defined follow-up interval and report pending/unaccepted offers separately. | Deduplicate both sets. Acceptance means confirmed Run successfully queued, not command success or an error fixed. |

### 4. Command palette kick off

| # | Required measurement | Current event / fields | Query / aggregation | Interpretation boundary |
|---|---|---|---|---|
| 4.1 | `?` entry to submission | Entry and dispatch `EntryId` | For an entry-ID cohort count matching foreground submissions / entries. | Editing does not create entries; absence of submission is not proof of intentional abandonment. Submission is not delegate startup. |

### 5. Sidebar

| # | Required measurement | Current event / fields | Query / aggregation | Interpretation boundary |
|---|---|---|---|---|
| 5.1 | Sidebar state at launch; adoption and retention base | `App.AppCreated.SidebarEnabled` | Report `N(SidebarEnabled=true) / N(all AppCreated)` for window-snapshot share. For device adoption, choose an explicit rule, such as latest observed snapshot per device in the window; intersect with `Active`. Define an enabled-at-D0 device cohort for D7/D28 return. | Replaces the proposed `SidebarStateOnLaunch.enabled`. Multiple windows can have different state; snapshot share is not device share. Returning interactively does not itself prove the sidebar remained enabled. |
| 5.2 | Search entered | `App.SidebarSearchOpened` | Report opens, devices, and reach. | Opens of sidebar tab search, not query edits or Agent-history search. |
| 5.3 | Agent filter applied; resulting row count | `App.SidebarAgentFilterApplied.row_count` | Count successful view entries and devices; histogram `row_count`, retaining zero. | Implements entry into the Agent sessions view and its first successful snapshot, not a mandatory search-then-filter sequence. Rows are session rows, not terminal tabs. |
| 5.4 | Tab-order pin usage | `App.TabPinChanged`: `Pinned`, `PinnedCount` | Count successful pin/unpin transitions and devices, histogram post-action counts. | Do not use the older misleadingly named `SidebarTabPinned`, which still means Keep running. |
| 5.5 | Rich-tab selection state and changes | `App.SidebarRowFieldsChanged`: `fields`, `Source` | Use Launch snapshots for configuration, UserChange for edits. Select a documented per-device snapshot rule. | Configuration is not visible use. Retain empty selections; unavailable configuration is not empty. |

### 6. Durable sessions

| # | Required measurement | Current event / fields | Query / aggregation | Interpretation boundary |
|---|---|---|---|---|
| 6.1 | Keep running enable reach | `App.KeepRunningMarked`, `Win32Host.UserInteract` | Marking devices intersect Active / Active in the same 28-day window. Distinct KeepId counts opt-ins. | Enabled during the window, not currently enabled or first-ever adoption. Existing tab counts are auxiliary action-time snapshots, not all-tab state. |
| 6.2 | Terminal closed with the marked tab alive | `App.KeepRunningDetached.KeepId` joined to marked `KeepId` | Report distinct detached IDs. For opt-in cohort `M`, compute `size(M intersect Ids(detached events, KeepId)) / size(M)` as observed retention-after-close incidence. | Successful in-process retention only. Marks that were never closed are in the denominator unless an eligible close cohort is known; this is not a close-operation success rate. |
| 6.3 | Restore a Keep running tab | Started/result `AttemptId`; result `KeepId`, `Outcome` | Form a started-attempt cohort; count matching live, failed and no-result attempts separately. Separately measure detached KeepId cohorts later restored. | Retry creates a new attempt. No-result is unknown, not gone. Historical ACP session/load is a different scenario. |
| 6.4 | Agent-pane prompt after reattachment | Restore result and prompt `KeepId`, `AttemptId` | For live attempts with HasAgentSession=true, count members with matching non-autofix, Reattached user dispatches during follow-up. | Eligibility means a bound agent session at restore, not guaranteed provider readiness. Excludes ordinary CLI chat and shell use. Multiple prompts count as one continued restore. |

### 7. Settings configuration

| # | Required measurement | Current event / fields | Query / aggregation | Interpretation boundary |
|---|---|---|---|---|
| 7.1 | Provider state at launch, separate from changes | `App.AppCreated`: `PrimaryProvider`, `PrimaryEffectiveProvider`, `DelegateProvider`, `DelegateEffectiveProvider`, `DefaultsFallback` | Group window snapshots by each role's configured/effective provider; calculate category share per role. For device share, use a documented snapshot-selection rule and intersect with `Active`, as in 5.1. | One combined event per created window; primary and delegate are fields, not separate events. Do not interpret it as a toggle count or treat legacy `AgentProviderConfigured` counts as equivalent. |
| 7.2 | Provider changed; switching between agents | `Model.AgentProviderChanged`: `role`, `from`, `to` | Group counts and device counts by all three fields; keep `role` in transition analysis. | Accepted settings reloads only. Custom-to-custom changes can report `custom` to `custom`; no custom name is exported. |
| 7.3 | Custom agent configuration and policy segmentation | `App.AppCreated`: `PrimaryCustomConfiguredCount`, `DelegateCustomConfiguredCount`, both `*Provider` and `*CustomSelectedCommandConfigured` fields, `AllowedAgentsPolicy`, `AllowCustomAgentsPolicy` | For each role, select configured count > 0 for inventory reach, provider = `custom` for selection, and the selected-command flag for matching configuration. Group by both policy categories; report count distributions and snapshot/device shares separately. | Replaces the proposed `CustomAgentConfigured`. Counts include unused entries; do not sum roles as unique agents. `AllowedAgents` gates built-ins, while `AllowCustomAgents` governs custom agents. No custom commands or allowlist entries are collected. |

GitHub asset downloads and Microsoft Store acquisitions remain external
distribution metrics, not client telemetry events. Retrieve those through
their respective reporting systems; neither event counts nor backend device
counts establish unique combined reach across distribution channels.

## Providers and common metadata

| Alias | Provider name | GUID | Dedicated events |
|---|---|---|---|
| App | `Microsoft.Windows.Terminal.App` | `{24a1622f-7da7-5c77-3303-d850bd1ab2ed}` | 16 |
| WTA | `Microsoft.Windows.Terminal.WTA` | `{4cfcff80-4e6b-5bfd-8ea1-d38e1226f70b}` | 18 |
| Model | `Microsoft.Windows.Terminal.Setting.Model` | `{be579944-4d33-5202-e5d6-a7a57f1935cb}` | 1 |
| Editor | `Microsoft.Windows.Terminal.Settings.Editor` | `{1b16317d-b594-51f8-c552-5d50572b5efc}` | 3 |

App, WTA, and dedicated Model events use level `Verbose` and
`MICROSOFT_KEYWORD_MEASURES`. Editor probe events use level `Info` and
do not explicitly set a telemetry keyword.

Every event below includes this additional payload field:

| Field | Type | Meaning |
|---|---|---|
| `PartA_PrivTags` | UInt64 | Build-defined privacy classification |

The catalog uses **Usage** for `PDT_ProductAndServiceUsage` and
**Performance** for `PDT_ProductAndServicePerformance`. These are metadata
categories, not guarantees about collection or upload. In the OSS fallback
telemetry header, these values and the measures keyword are `0`.

Provider identity, event name, timestamp, process/thread IDs, level, and
keyword are event metadata, not extra business fields. No implicit
`SessionId`, user ID, turn ID, or success field should be assumed.

### Type and value conventions

| Type | Wire meaning |
|---|---|
| String | Narrow string; WTA emits UTF-8 |
| WideString | UTF-16 string |
| Boolean | 8-bit boolean, used by App `IsBackgroundMode` |
| Bool | 32-bit boolean |
| UInt8 / UInt32 / UInt64 | Unsigned integer of the indicated width |
| Int32 | Signed 32-bit integer |
| Double | 64-bit floating point; duration fields are milliseconds |

Field names and category values are case-sensitive. All fields listed in an
event's table are emitted; an empty string or `unknown` is a value, not an
omitted field. Agent/provider session identifiers are not emitted.

The agent category set is `copilot`, `claude`, `codex`, `gemini`,
`opencode`, and `custom`. WTA and the App snapshot bucket unrecognized agent
identifiers as `custom` in session snapshots; Editor probes bucket custom-provider IDs as
`custom`. `DelegateAgentId` additionally permits `none` when no delegate
is resolved. Startup configuration and provider-change events distinguish
`unknown` from explicit `custom:` IDs and use `none` for an empty selection.
Custom names and command lines are not reported.

`Branding` is `0` for other/development, `1` for Canary, `2` for Preview,
and `3` for Release. `Distribution` is `0` for other/unpackaged, `1` for
portable, and `2` for packaged. Packaged does not mean Store-installed.

## App event schemas

### App.AgentPaneOpened

**Trigger:** `_OpenOrReuseAgentPane` creates/shows a pane or requests its
sessions view and reaches its reporting point.

| Field | Type | Meaning / values |
|---|---|---|
| `TriggerSource` | WideString | `Action`, `SessionsAction`, `Autofix`, `FirstRunExperience`, `AgentSwitch`, `BottomBarToggle`, `BottomBarSessions`, `SettingsReload`, `FocusAction` |
| `Branding` | UInt8 | Build branding category |

Automatic prewarm, direct creation/restoration paths, and stashing do not
themselves emit this event. Count instrumented opening operations, not all
panes, sessions, or users. A sessions-view request does not prove its rows
have loaded.

### App.CommandPaletteAgentPromptEntered

**Trigger:** the Command Palette becomes visible in foreground agent prompt
mode, or a visible palette switches into that mode (for example, by typing
`?`). Direct agent-delegation launch actions are included.

| Field | Type | Meaning |
|---|---|---|
| `EntryId` | WideString | Random ID for this visible foreground-mode visit |

Hidden mode preparation (including a shortcut that closes an already-visible
palette), repeated selection of the same visible mode,
background `&` mode, and editing the prompt do not emit another entry.
Leaving and reentering foreground mode, or closing and reopening it, emits
a new entry. No submission is required, so entering bare `?` and abandoning
the palette still counts. This is a new entry event, not a rename of the
existing submission event below.

### App.CommandPaletteDispatchedAgentPrompt

**Trigger:** an agent prompt is submitted through the Command Palette.

| Field | Type | Meaning / values |
|---|---|---|
| `IsBackgroundMode` | Boolean | `true` for background mode; `false` for foreground mode |
| `EntryId` | WideString | Foreground visit ID; empty for background submissions |

No prompt text is included. This is a submission event, not evidence that
the selected mode launched or completed an agent task. In particular,
the reserved background entry point is not a completed background workflow.

### App.SidebarSearchOpened

**Trigger:** the user opens the sidebar tab search box, transitioning from
inactive to active search in the expanded, visible vertical layout.

**Business fields:** none. `PartA_PrivTags` is still present.

Typing, clearing a query, closing search, layout redraw, collapsing/restoring
the rail, and opening Agent History search do not emit this event. Closing
and explicitly reopening tab search emits again.

### App.SidebarAgentFilterApplied

**Trigger:** the user opens the dedicated Agent sessions sidebar view and its first
explicitly `Ready` session snapshot is committed. `Loading` or `Error` snapshots
may display cached rows but do not emit or consume the pending measurement.
The pending measurement is canceled
if the user leaves the view before a snapshot succeeds.

| Field | Type | Meaning / values |
|---|---|---|
| `row_count` | UInt32 | Visible Agent-view session rows after applying its current session-search query to the first successful snapshot; may be zero |

The unified Agent view reads the global shell-origin session registry,
including historical sessions. It does not count current-window tabs,
split-pane children, or Agent-pane sessions excluded by that view's scope.
Its search is independent of the live-tab search; entering the view clears
the session query, but a query entered while loading is honored.
Repeated open requests while the view is already active, closing the view,
editing search, and automatic refreshes do not emit again. Closing and
reopening Agent sessions arms another measurement. Load failure
does not fabricate a zero-count event; a later successful refresh while
the same entry remains active can complete the pending measurement.

### App.SidebarTabPinned

**Trigger:** the user selects **Turn on headless mode** through the sidebar tab
context menu. The telemetry name uses "pinned" terminology;
it does not mean tab-order pinning or a Windows taskbar pin.

| Field | Type | Meaning / values |
|---|---|---|
| `pinned_count` | UInt32 | Attached terminal tabs in the owning window with headless mode enabled, after the action |

Count includes tabs hidden by search/filter, but excludes detached retained
tabs, other windows, and individual panes. Turning keep-running off does not
emit. Re-enabling emits again. State copying, programmatic setters, layout
changes, closing into background retention, and restoring a retained tab do
not emit. This measures opt-in actions, not successful background work or
retention across an application restart.

### App.TabPinChanged

**Trigger:** a tab-order pin/unpin request successfully changes the target tab's
state, in either horizontal or vertical layout. State copies and blocked or
unchanged requests do not emit.

| Field | Type | Meaning |
|---|---|---|
| `Pinned` | Bool | Post-action pinned state |
| `PinnedCount` | UInt32 | Post-action pinned terminal-tab count in this window |

Pinning does not enable Keep running or prevent closing a tab. No tab content
or routing identifier is recorded.

### App.KeepRunningMarked

**Trigger:** Headless mode is enabled by the sidebar menu or the explicit
tab-control API. The event captures the counts after the false-to-true
transition. Disabling, repeated enable requests, startup, and copying the
choice into a transferred tab do not emit.

| Field | Type | Meaning |
|---|---|---|
| `KeepId` | WideString | Random ID generated for this opt-in; a later disable and re-enable generates a new ID |
| `HasAgentPane` | Bool | Whether an agent pane (including a stashed pane) is present; not proof of a connected ACP session |
| `TotalTabCount` | UInt32 | Attached tabs containing terminal content in the owning window |
| `KeepRunningTabCount` | UInt32 | Those attached terminal tabs with Keep running enabled, including the newly marked tab |

Both counts come from the same post-action window snapshot. Search-hidden
tabs are included; detached background tabs, other windows, Settings tabs,
and other nonterminal tabs are excluded. Split panes do not count as separate
tabs. The marked terminal tab makes `TotalTabCount` nonzero.

For optional enable-time tab analysis, use `KeepRunningTabCount / TotalTabCount` for each
observation, or `SUM(KeepRunningTabCount) / SUM(TotalTabCount)` across the
same selected observations using floating-point division. The latter is a
tab-count-weighted ratio of enable-time snapshots, not distinct tabs, agent
sessions, a time-weighted average, or an all-device adoption rate. Windows
where Keep running is never enabled have no sample. This calculation needs
no `KeepId`; that ID links opt-in, retention, restoration and later prompts.
The primary adoption metric is marking devices intersect Active / Active.
This measures enable actions during the reporting window, not current state.

### App.KeepRunningDetached

**Trigger:** a marked tab is removed from the visible strip and retained by
the process after a tab or window close. A failed close that rolls back does
not emit.

| Field | Type | Meaning |
|---|---|---|
| `KeepId` | WideString | ID from the corresponding opt-in |
| `HasAgentPane` | Bool | Whether the retained tab contains an agent pane |

### App.KeepRunningReattachStarted

**Trigger:** entry into the retained-tab restore operation, before target
lookup or claiming the content. Each target and retry gets its own attempt.

| Field | Type | Meaning |
|---|---|---|
| `AttemptId` | WideString | Random restore-attempt ID |

This event does not require a resolvable target. Only the result supplies
`KeepId` when available; do not invent a missing association.

### App.KeepRunningReattached

**Trigger:** exit from a restore attempt, including exceptions before content
transfer. Existing rollback behavior is preserved; process termination can
still leave a start without a result.

| Field | Type | Meaning |
|---|---|---|
| `AttemptId` | WideString | ID from the start event |
| `KeepId` | WideString | Original opt-in ID, or empty if target lookup/claim failed |
| `Outcome` | String | `live` for committed restoration; `failed` for observed failure |
| `HasAgentPane` | Bool | Whether the tab contains an agent pane |
| `HasAgentSession` | Bool | Whether the source agent pane has a bound nonempty session ID; not a provider-readiness guarantee |

There is no `gone` outcome: a crashed, updated, or terminated process cannot
emit an event, and keep running cannot survive process exit. These are
tab-level events, including tabs with no agent. Only random telemetry IDs are
reported, not the tab's routing GUID or title.

### App.SidebarRowFieldsChanged

**Triggers:** once during each window's Rich Tab initialization, and after
an actual user change in the sidebar metadata controls. Both paths read the
shared Rich Tab provider broker and use the same emitter. This follows the
existing `Feature_RichTabProviders` gate; no event is emitted when that feature
is disabled or the provider selection is unavailable (the latter is logged).

| Field | Type | Meaning / values |
|---|---|---|
| `fields` | String | Comma-separated field IDs in canonical order: `agentStatus`, `workingDirectory`, `repository`, `branch`, `changes`. Contains zero, one, or two IDs; the empty string means no fields selected. |
| `Source` | String | `Launch` for window initialization, `UserChange` for a changed selection |

The payload is the complete current selection, not just the field
that changed. For example, choosing Branch then Repository produces
`repository,branch`, independent of click order. Each deselection also emits,
including the intermediate one-field or empty selection while replacing a
pair. Launch snapshots do not require an agent session or a visible sidebar.
Agent creation/load and prewarm do not emit selection snapshots. Configuration
does not prove that the selected fields were visible or used.

Restoring controls in another window, opening or dismissing the menu, disabled
third-field clicks, layout refresh, and live metadata/status updates do not
themselves emit. Agent session creation does not emit
a selection snapshot. Always filter by `Source`; combined counts must not be interpreted
as a pure edit count or a retention metric. Only the five fixed IDs are collected:
never an agent's status value, directory, repository name, branch name, or
Git-change contents.

### App.AppCreated

**Trigger:** each `AppLogic::Create()` calls `_LogAppCreatedTelemetry()`
after settings have loaded or fallen back to defaults. This preserves the
existing **per-window** creation boundary, including secondary windows in
the same process and windows that never connect an agent session.

| Field | Type | Meaning / values |
|---|---|---|
| `TabsInTitlebar` | Bool | Existing configured tabs-in-titlebar field |
| `PrimaryProvider` | String | Configured primary provider: `copilot`, `claude`, `codex`, `gemini`, `opencode`, `custom`, `unknown`, or `none` |
| `PrimaryEffectiveProvider` | String | Settings-layer effective primary provider in the same bucket set, after fallback/policy resolution |
| `PrimaryCustomConfiguredCount` | UInt32 | Distinct executable-derived custom IDs in the primary role's plural and legacy command settings |
| `PrimaryCustomSelectedCommandConfigured` | Bool | Whether that selected custom ID has a matching configured command entry |
| `DelegateProvider` | String | Configured delegate provider, using the same bucket set |
| `DelegateEffectiveProvider` | String | Settings-layer effective delegate provider, using the same bucket set |
| `DelegateCustomConfiguredCount` | UInt32 | Distinct executable-derived custom IDs in the delegate role's plural and legacy command settings |
| `DelegateCustomSelectedCommandConfigured` | Bool | Whether that selected custom ID has a matching configured command entry |
| `AllowedAgentsPolicy` | String | `not_configured`, `empty`, or `allowlist`; never the allowlist entries |
| `AllowCustomAgentsPolicy` | String | `not_configured`, `allowed`, or `blocked` |
| `SidebarEnabled` | Bool | Whether the configured tab layout is vertical |
| `DefaultsFallback` | Bool | Whether the currently accepted settings came from initial load-failure fallback |

Both roles and shared policy/sidebar state belong to this single record;
there are no separate startup configuration or sidebar events. Later
windows reflect the then-current settings, not a frozen process-start
snapshot. Settings reload and agent session creation/load do not emit
`AppCreated`.

An empty provider is `none`; a `custom:` ID is `custom`; other unrecognized
values are `unknown`. This does not prove a CLI is installed, authenticated,
or usable. WTA/App session events remain the source for connected-agent
identity.

Custom counts include unused configured entries, deduplicated using the
editor's executable-ID derivation; different arguments for the same derived
ID do not create additional agents. Empty/invalid derivations are excluded.
`AllowedAgents` gates built-in providers; custom agents are governed
separately by `AllowCustomAgents`. `SidebarEnabled` measures the existing
vertical-tab sidebar, not the availability of search, pinning, or rich rows.

Custom selection is derivable from the corresponding provider being `custom`.
Policy presence is derivable from its category differing from `not_configured`;
the custom-agent policy gate is blocked only when `AllowCustomAgentsPolicy`
is `blocked`. These facts are not repeated as additional fields. Explicit,
inherited, and default selection origins are not distinguished.

### App.DelegateInvoked

**Trigger:** Terminal successfully creates the `wta delegate` process.

| Field | Type | Meaning / values |
|---|---|---|
| `TriggerSource` | WideString | `CommandPalette` or `Action` |

Process launch is not downstream agent initialization or task completion.
Keep this event separate from WTA `DelegateInvoked`.

### App.ErrorDetected

**Trigger:** Terminal receives an `autofix_state` projection whose state is
`pending`, before subsequent tab/pane routing.

| Field | Type | Meaning / values |
|---|---|---|
| `Branding` | UInt8 | Build branding category |

Repeated pending projections can emit repeatedly. The literal `detected`
state does not emit this event. There is no pane or incident ID in this
payload, so it cannot be joined one-to-one with WTA `ErrorDetected`.

### App.AgentSessionStarted

**Trigger:** a helper publishes a successful ACP session creation or load
through `agent_state_changed.session_started`, and its owning Terminal tab
accepts the payload. The helper supplies session/runtime state; the host
adds its effective settings when processing that notification.

| Field | Type | Meaning / values |
|---|---|---|
| `StartId` | String | New random UUID for this start/load notification; event deduplication key |
| `StartKind` | String | `New` or `Load` |
| `AgentId` | String | Connected agent category |
| `AgentSource` | String | `host`, `wsl`, or `unknown`; no distribution name |
| `DelegateAgentId` | String | Helper's resolved delegate category, or `none` |
| `ModelSource` | String | `byok` or `provider` from the master-resolved process binding; `unknown` if binding metadata is unavailable |
| `AutoErrorDetection` | Bool | Policy-aware host effective automatic error-detection setting |
| `AutoFix` | Bool | Helper runtime autofix switch AND policy-aware host effective autofix setting |
| `AgentSessionManagement` | Bool | Policy-aware host effective session-management setting |
| `AgentPanePosition` | WideString | Owning tab's effective `left`, `right`, `up`, `bottom`, or `unknown`, including pane override |
| `ShowTokenUsageAndCost` | Bool | Usage/cost UI visibility setting |
| `VerticalTabs` | Bool | Whether the host tab layout is vertical |
| `FirstWindowPreference` | String | `defaultProfile`, `persistedLayout`, or `persistedLayoutAndContent` |
| `AutomaticYolo` | String | `enabled` / `disabled` automatic target, or `provider` when no automatic directive applies |
| `YoloPolicyBlocked` | Bool | Whether helper policy prohibits requesting YOLO enablement |
| `YoloControlOwner` | String | `automatic`, `manual`, `provider-restored`, or `unknown` |
| `CoordinatorConfigured` | Bool | Configured legacy coordinator switch; not a running-process indicator |
| `ReadConfirmationConfigured` | WideString | Configured legacy read-operation value: `auto`, `prompt`, or `unknown` |
| `CreateConfirmationConfigured` | WideString | Configured legacy create-operation value: `auto`, `prompt`, or `unknown` |
| `InputConfirmationConfigured` | WideString | Configured legacy input-operation value: `auto`, `prompt`, or `unknown` |
| `SessionMcpConfirmation` | String | Constant `user`; session MCP mutations use the existing user-confirmed action path |
| `Branding` | UInt8 | Build branding category |
| `Distribution` | UInt8 | Package/portable category |

**Lifecycle rules:**

- Initial successful connections, lazy creation, `/new`, and successful ACP
  loads are covered. Pre-warmed sessions count even if no prompt is sent.
- Failed creation/load, handshake-only bootstrap before initial load, and
  ordinary state projections do not produce a successful-session snapshot.
  Duplicate new-session attach notifications are suppressed.
- Initial model selection can defer publication until that exact request
  completes or fails. Other requests for the same session do not release
  that wait. This is not a continuous settings-change feed or an atomic
  snapshot across helper and host.
- `ModelSource` uses the resolved process binding returned by the master on
  connection, not the helper's global selection, pane override, or model
  catalog. Catalog delivery can occur later without changing the snapshot.
  Older masters without binding metadata produce `unknown`, not a guess
  based on a model name. A reconnect resets this telemetry binding.
- Loaded sessions retain their restored model. A BYOK-bound process is
  categorized as `byok` even if the restored agent reports a native model ID.
- Re-loading the same saved session produces a new `StartId` and
  `StartKind=Load`. `Load` alone does not identify
  session-view resume versus saved-layout restoration.
- Stashing/showing the same pane, moving/renaming its tab, or changing a
  setting does not by itself produce another successful-session snapshot.
  A standalone helper without the owning host cannot emit this App event.
  Provider-native CLI resume is not an ACP load.

`AutomaticYolo` is configuration intent, **not confirmed provider permission
mode**. The three `*ConfirmationConfigured` settings are legacy configured
values and are not enforced by the current runtime operation paths. Do not
interpret them as active security policy.

## WTA event schemas

### WTA.AcpInitializeComplete

**Trigger:** an instrumented ACP `initialize` RPC completes or times out.

| Field | Type | Meaning / values |
|---|---|---|
| `DurationMs` | Double | Monotonic elapsed time around the RPC attempt, in milliseconds |
| `Success` | Bool | Whether the RPC returned successfully |
| `Route` | String | `HelperPipe`, `Probe`, or `SessionsCli` |
| `FailureKind` | String | Empty on success; otherwise `AcpError` or `Timeout` |
| `AcpErrorCode` | Int32 | ACP error code for `AcpError`; otherwise `0` |

`HelperPipe` measures helper/master initialization, not a fresh agent
process startup. `Probe` measures model discovery and `SessionsCli` measures
initialization for `wta sessions list`. These are different populations.

### WTA.AcpNewSessionComplete

**Trigger:** an instrumented ACP `session/new` RPC completes, including
failure or an enforced timeout.

| Field | Type | Meaning / values |
|---|---|---|
| `DurationMs` | Double | Monotonic RPC-attempt duration in milliseconds |
| `Success` | Bool | Whether the RPC returned successfully |
| `Route` | String | `MasterForward`, `HelperPipeStartup`, `HelperPipeNewSessionForTab`, `HelperPipeFallback`, `LazyCreateOnFirstPrompt`, or `Probe` |
| `FailureKind` | String | Empty on success; otherwise `AcpError` or `Timeout` |
| `AcpErrorCode` | Int32 | ACP error code for `AcpError`; otherwise `0` |

`MasterForward` is the master-to-agent RPC. Helper routes measure the
helper-side request; both layers can report the same logical creation.
`Probe` creates a model-discovery session, not a user chat session.
RPC success also precedes any later stale-result rejection.

### WTA.AcpLoadSessionComplete

**Trigger:** an ACP `session/load` attempt returns or times out.

| Field | Type | Meaning / values |
|---|---|---|
| `DurationMs` | Double | Monotonic load-attempt duration in milliseconds |
| `Success` | Bool | Successful RPC result; `false` on error or timeout |

Both saved-layout restoration and ACP resume can reach this event. It is
emitted before stale/retired-result checks: `Success=true` does not establish
that the UI adopted the result. There is **no** session ID, agent ID, route,
failure-kind, or error-code field in this event.

### WTA.AgentColdStartComplete

**Trigger:** the master completes a cold process-pool startup attempt,
including process-spawn failure or ACP initialization failure/timeout.

| Field | Type | Meaning / values |
|---|---|---|
| `AgentId` | String | Resolved agent category |
| `Source` | String | `Host` or `Wsl` |
| `DurationMs` | Double | Monotonic elapsed time for the cold-start attempt in milliseconds |
| `Success` | Bool | Whether the process was spawned and initialized successfully |
| `FailureKind` | String | Empty on success; otherwise `SpawnFailed`, `InitializeFailed`, or `Timeout` |

Warm pool reuse does not emit this event. `Source` casing differs from
the App snapshot's `AgentSource`.

### WTA.AgentPromptSent

**Trigger:** WTA dispatches a prompt over ACP.

| Field | Type | Meaning / values |
|---|---|---|
| `PromptLengthBytes` | UInt32 | Byte length of the constructed dispatch prompt, including context/templates |
| `IsAutofix` | Bool | Whether this dispatch is an autofix prompt |
| `IsByok` | Bool | BYOK state captured for this prompt |
| `Reattached` | Bool | The owning tab was reattached by Keep running and this is still the ACP session bound at reattachment |
| `UserPromptOrdinal` | String | `First`, `Second`, or `Later` for dispatched non-autofix prompts in this helper's ACP session; `NotUserPrompt` for autofix |
| `AgentId` | String | Agent category |
| `TemplateKind` | String | `Planner`, `Autofix`, or `AgentCommand` |
| `Route` | String | Constant `AcpDispatch` |
| `SessionId` | String | Random helper-observation UUID, not the raw ACP ID or a persistent alias |
| `TurnId` | String | Random UUID shared with this prompt's completion |
| `KeepId` | String | Keep-running opt-in UUID, empty without a correlated restore |
| `AttemptId` | String | Most recent successful restore UUID for this surviving session, empty otherwise |

Includes manual and automatic autofix analysis. The length is not the user's
typed character count, a token count, or the prompt contents. `Reattached`
is false for a new ACP session started after the tab was restored and for
prompts before the helper receives the scoped reattachment notification.
It stays true for subsequent prompts on that surviving session. Filter
`IsAutofix=false` to measure user-initiated turns rather than background
autofix analysis. `UserPromptOrdinal` advances only when a non-autofix
prompt reaches the ACP dispatch boundary. Blocked or cancelled-before-send
prompts and autofix prompts do not advance it. A second prompt in the same
observed session emits `Second` exactly once; subsequent prompts emit
`Later`. The ordinal is kept in helper memory, separated by actual ACP
session ID, and forgotten when the session is dropped or replaced. The exported
observation UUID is independently generated, not derived from that ID. Loading an older ACP session in
a fresh helper starts a new *observed* sequence at `First`, even if its
provider-side transcript already contains turns.

### WTA.AgentResponseFirstToken

**Trigger:** the first counted text/thought chunk arrives for an active
turn with a known monotonic dispatch time.

| Field | Type | Meaning / values |
|---|---|---|
| `FirstTokenLatencyMs` | Double | Dispatch-to-first-counted-text duration in milliseconds |
| `ChunkLengthBytes` | UInt32 | Byte length of that first counted chunk |
| `AgentId` | String | Agent category associated with the turn |

At most one such event is emitted by the active turn tracker for a turn.
An ephemeral thought chunk can precede final-answer text. A tool-only turn
or one without a reliable dispatch timestamp may have no first-token event.
Do not interpret its absence as zero latency.

### WTA.AgentResponseComplete

**Trigger:** the ACP prompt request completes for a tracked turn with a
known monotonic dispatch time.

| Field | Type | Meaning / values |
|---|---|---|
| `TotalDurationMs` | Double | Monotonic prompt-dispatch-to-completion duration in milliseconds |
| `SessionId` | String | Same observation UUID as the corresponding dispatch |
| `TurnId` | String | Same turn UUID as the corresponding dispatch |
| `IsAutofix` | Bool | Classification captured at dispatch |
| `Success` | Bool | Whether the ACP prompt request completed successfully |
| `IsByok` | Bool | BYOK state associated with the prompt |
| `AgentId` | String | Agent category associated with the turn |

RPC success does not mean that the answer was correct, a tool succeeded,
or the user's task was completed. `TotalResponseBytes` is not emitted.
Join to sent turns by `TurnId` and check `SessionId` and `IsAutofix`.
It does not include `TemplateKind`. A missing result is unknown, not proof
of failure; canceled-before-send requests are excluded.

### WTA.ErrorDetected

**Trigger:** a terminal notification is classified as non-acknowledged
`Actionable` or `Critical`, after the owning-tab filter.

| Field | Type | Meaning / values |
|---|---|---|
| `Severity` | String | `Actionable` or `Critical` |
| `OfferId` | String | Random UUID allocated for this detected candidate repair flow |
| `Source` | String | `Detection` |
| `Method` | String | Classified terminal event method: `connection_state` or `vt_sequence` |
| `PaneId` | String | Terminal pane identity, not an ACP session ID |
| `AllowAutoFixPolicy` | String | Raw host policy: `notConfigured`, `enabled`, `disabled`, or `unknown` |
| `AutoFixEnabled` | Bool | Effective helper runtime autofix switch, independent of the policy category |

Informational and auto-silenced classifications do not emit. There is no
command text, exit-code field, deduplicated real-world incident ID, or fix result. Multiple signals
can relate to the same underlying failure.

The host supplies the raw policy category at helper bootstrap and refreshes
it through scoped runtime configuration. Older hosts and manual launches
without metadata report `unknown`; a disabled effective switch is never
used to infer a policy block. Policy-only changes are propagated even when
the effective autofix switch remains off.
On helper connection, the host resends both the raw policy and the current
effective switch, recovering updates missed between bootstrap argument
capture and event subscription. This refresh does not replay earlier errors.

### WTA.ErrorFixOffered

**Trigger:** the first successfully flushed frame containing an unobscured,
concrete recommendation card for the current valid autofix turn in an open
agent pane.

| Field | Type | Meaning / values |
|---|---|---|
| `OfferId` | String | Candidate repair-flow UUID, allocated at detection or independent manual submission |
| `Source` | String | `Detection`, `Manual`, or `Unknown` when attribution is unavailable |

Repeated renders do not emit again. Stashed panes, hidden/fully clipped
cards, overlays covering the recommendation, stale autofix generations,
generic non-autofix proposals, analysis, and prose-only results do not
count. A previously hidden offer can count when it is later presented.
An autocomplete popup elsewhere in the pane does not suppress the event;
its painted rectangle must overlap the recommendation card to obscure it.
This measures application-level presentation, not proof that the user
looked at the window.
Detected flows retain their ID when the user explicitly activates a Detected
hint. Independent `/fix` submissions allocate new IDs with Manual source and
do not synthesize detections. Replacing a card within the same analysis retains
its ID and offered/accepted flags. Count flows, not individual card versions.
Busy or ineligible detections may never produce a card; multiple detections
must not be assigned to an existing in-flight analysis.

### WTA.ErrorFixAccepted

**Trigger:** the user confirms **Run** for a previously presented autofix
offer, the confirmation claim remains valid, and the execution request is
successfully queued. Acceptance is emitted when the executor dequeues that
request, immediately before `ErrorFixRunStarted`, so a fast executor cannot
report Run events before acceptance. A queued request lost during shutdown
before dequeue emits neither acceptance nor Run events.

| Field | Type | Meaning / values |
|---|---|---|
| `OfferId` | String | UUID of the corresponding `ErrorFixOffered` event |
| `Source` | String | Same flow source as the displayed offer |

At most one acceptance is emitted per offer. Insert-only actions,
dismissal, clicking the ask-for-fix entry point, automatic analysis,
generic proposals, stale confirmations, and failed dispatch do not count.
Queuing execution is not proof the command executed or fixed the error.

### WTA.ErrorFixRunStarted

**Trigger:** the executor dequeues a confirmed Run for a previously displayed
autofix flow. Its random `RunId` was allocated when queuing the request.
Insert does not emit.

| Field | Type | Meaning |
|---|---|---|
| `OfferId` | String | Associated repair flow |
| `RunId` | String | Random UUID shared with this Run's result |

### WTA.ErrorFixRunResult

**Trigger:** the existing recommendation dispatch operation returns. Telemetry
only observes that result; it does not change command delivery or wait for execution.

| Field | Type | Meaning |
|---|---|---|
| `OfferId` | String | Associated repair flow |
| `RunId` | String | Random UUID for this executor attempt |
| `Outcome` | String | `unobservable` after successful dispatch, or `dispatchFailed` |

All shells retain the original dispatch path. Send delivers the command and
Enter through the existing terminal input API; Insert remains insertion-only
and does not emit Run events. There is no telemetry-specific shell channel,
key binding, command wrapper, execution observer, timeout, or retry.
Successful dispatch does not provide a correlated command exit code.
Later OSC marks, history entries and unrelated commands are not attributed to Run.

After an ambiguous transport failure the command is **not retried**.
`dispatchFailed` can therefore mean that dispatch could not be confirmed;
it does not prove nothing executed, including earlier actions in a multi-action
Run. Process shutdown can leave a started Run without any result.

Report dispatch failures, unobservable results and missing results separately,
joining RunId to starts and OfferId to repair flows. These events cannot
calculate command execution success or repair success.
The events cover the typed recommendation workflow, not arbitrary
agent-owned shell tools.

### WTA.AgentSlashCommandUsed

**Trigger:** WTA dispatches a registered built-in slash command, before
command-specific guards.

| Field | Type | Meaning / values |
|---|---|---|
| `command` | String | `help`, `clear`, `new`, `fix`, `restart`, `stop`, `sessions`, `agent`, `model`, `config`, or `move` |

Busy `/new` and idle `/stop` still count. Browsing autocomplete does not.
Agent-provided commands instead use `AgentPromptSent` with
`TemplateKind=AgentCommand`; their names and arguments are not recorded here.
An unknown command may proceed as an ordinary prompt. `/fix` and `/sessions`
can also produce downstream prompt/view events.

### WTA.SessionsViewOpened

**Trigger:** the Agent Session View open routine is entered.

**Business fields:** none. The common `PartA_PrivTags` is still present.

This does not establish that session rows loaded or that the user selected
a session.

### WTA.SessionResumeInvoked

**Trigger:** the session view selects a resume route for dispatch.

| Field | Type | Meaning / values |
|---|---|---|
| `Route` | String | `AgentPane` for ACP load, or `Cli` for provider-native CLI resume |
| `AgentId` | String | Agent category for the selected session |

Emitted before downstream completion. Focusing an already-live session
does not emit it. This payload does not identify the resumed session.

### WTA.DelegateInvoked

**Trigger:** an agent recommendation invokes a configured delegate after
the requested target tab/pane has been created.

| Field | Type | Meaning / values |
|---|---|---|
| `TriggerSource` | String | Constant `Agent` |

Not a task-completion event. It has neither the App event's provider nor
its field encoding and should not be merged with it by event name alone.

### WTA.SessionMcpToolCalled

**Trigger:** a parsed session MCP `tools/call` reaches the dispatch boundary,
before tool/argument validation, user approval, or execution.

| Field | Type | Meaning / values |
|---|---|---|
| `ToolName` | String | `run_command_in_current_shell`, `create_workspace`, `delegate_task_in_new_workspace`, `request_user_input`, or `unknown` |

Earlier malformed-request or capacity rejection can occur without an
event. Unknown names are bucketed; arguments and execution results are not
recorded. This covers session MCP, not every tool owned by an agent CLI.

### WTA.HookOperationCompleted

**Trigger:** hook installation or uninstallation finishes for one supported
CLI.

| Field | Type | Meaning / values |
|---|---|---|
| `Operation` | String | `Install` or `Uninstall` |
| `Cli` | String | `copilot`, `claude`, `gemini`, `codex`, or `opencode` |
| `Outcome` | String | Install: `installed`, `skipped`, `failed`; uninstall: `succeeded`, `skipped`, `failed` |

One command can affect multiple CLIs and emit multiple events.
`skipped` can mean smart reconciliation found no installation work to do;
it is not necessarily a failure. Installation status does not establish
that hook notifications are currently arriving.

## Settings Model event schemas

The provider-change event uses the existing
`Microsoft.Windows.Terminal.Setting.Model` provider. It is not emitted by
model deserialization, provider probes, or agent session creation.
Startup inventory belongs to `App.AppCreated`, not separate Model events.

### Model.AgentProviderChanged

**Trigger:** a successful subsequent settings reload changes a provider's
raw configured ID relative to the previous accepted settings baseline.
Raw IDs are retained only in memory for comparison; payloads are bucketed.
Initial load-failure fallback establishes a baseline from the defaults
actually applied, so a later valid provider change is still counted.

| Field | Type | Meaning / values |
|---|---|---|
| `role` | String | `primary` or `delegate` |
| `from` | String | Previous configured provider: `copilot`, `claude`, `codex`, `gemini`, `opencode`, `custom`, `unknown`, or `none` |
| `to` | String | New configured provider, using the same bucket set |

This covers accepted Settings UI saves and external settings-file changes,
including in-place mutations of the old settings object. Initial baseline
creation, failed reloads, unchanged reloads, and policy-only changes do not
emit. Switching between two custom agents emits `custom` to `custom`.
Per-tab overrides, session resume, and CLI installation are not settings
changes. Intermediate writes coalesced by the existing settings watcher
are not a complete click-by-click edit history.

## Settings Editor event schemas

### Editor.AcpModelProbeStarted

**Trigger:** Settings starts a clean ACP model catalog probe.

| Field | Type | Meaning / values |
|---|---|---|
| `AgentId` | WideString | Selected agent category; custom providers become `custom` |
| `CacheRevision` | UInt64 | Per-agent runtime catalog revision observed at probe start |

`CacheRevision` is not a retry count or a unique probe ID. The underlying
probe can make multiple ACP attempts.

### Editor.AcpModelProbeDiscarded

**Trigger:** a returned probe result belongs to a generation superseded by
a newer Settings probe.

| Field | Type | Meaning / values |
|---|---|---|
| `AgentId` | WideString | Agent category of the superseded probe |

This path returns without emitting `AcpModelProbeCompleted`. Discarding a
stale generation is not evidence that the provider failed.

### Editor.AcpModelProbeCompleted

**Trigger:** the current probe result is processed, before cache acceptance.

| Field | Type | Meaning / values |
|---|---|---|
| `AgentId` | WideString | Agent category of the probe |
| `Succeeded` | Bool | Whether a nonempty model catalog was parsed |
| `ModelCount` | UInt32 | Number of parsed catalog entries; `0` when no entries were parsed |

No model names, identifiers, or probe command are included.
`Succeeded=true` does not guarantee that the cache accepted the catalog:
a newer cache revision can cause rejection after this event.

## Counting and correlation

### Per-event query and aggregation recipes

Filter by **provider and event name**, a consistent time window, build, and
distribution before applying the recipes below. `COUNT(*)` counts emitted
observations, not users. Device-level reach or D7/D28 retention requires a
backend-provided device dimension; it is not in these payloads. Distinct IDs
below are only the independent, explicitly documented correlation keys.
Rates require the same eligible population and capture window in numerator
and denominator. These are logical query operations, not a backend-specific
SQL dialect.

| Provider and event | Query / aggregation | Interpretation boundary |
|---|---|---|
| App `AgentPaneOpened` | Count by `TriggerSource` and `Branding`; use the device-set intersection in the shared reach definition for adoption among interactive devices. | Instrumented open requests, not every pane creation or restoration. |
| App `CommandPaletteAgentPromptEntered` | Form an EntryId cohort and join foreground submissions. | Editing does not create a new entry; missing submission is not proof of abandonment. |
| App `CommandPaletteDispatchedAgentPrompt` | Join foreground EntryId to entry records. Background EntryId is empty. | Dispatch request, not delegate startup or task completion. |
| App `AppCreated` | Count window-created snapshots; group by `SidebarEnabled`, provider/effective-provider fields, policy categories, and both custom-agent counts. Divide matching snapshots by all snapshots in the same cohort. | One observation per created window; do not sum primary and delegate custom counts as distinct agents or infer CLI availability. |
| App `SidebarSearchOpened` | Count explicit search-box opens and distinct devices. | Search entry, not query edits, result views, or Agent-view search. |
| App `SidebarAgentFilterApplied` | Count Agent-view entries; group or histogram `row_count`, including zero. | First successful Ready snapshot after entry, not tab-search filtering or unique sessions. |
| App `SidebarTabPinned` | Count enable actions; group by the post-action `pinned_count`. | Keep-running menu action, not tab-order pinning; count covers attached tabs in one window. |
| App `TabPinChanged` | Count successful pin/unpin actions by Pinned and post-action PinnedCount. | Action-time counts, not all-device state. |
| App `KeepRunningMarked` | Marking devices intersect Active / Active; count opt-ins by distinct KeepId. | Enabled during this window, not currently enabled. Tab counts remain auxiliary action-time snapshots. |
| App `KeepRunningDetached` | Count distinct `KeepId` and join to marked IDs in the same process-lifetime cohort. | Successful retention inside a live process; process exit cannot emit a detach. |
| App `KeepRunningReattachStarted` | Form an AttemptId cohort, including attempts unable to resolve the target. | Missing result is unknown, not failed or gone. |
| App `KeepRunningReattached` | Join AttemptId to starts; separately join KeepId to detached flows and prompts. | Retries have distinct attempt IDs; bound-session eligibility does not guarantee provider readiness. |
| App `SidebarRowFieldsChanged` | Group fields separately for Source=Launch and UserChange. | Launch configuration is not visible use or an edit; select a documented per-device snapshot rule. |
| App `DelegateInvoked` | Count by `TriggerSource`. | App-side process launch only; do not merge with the WTA event solely by name. |
| App `ErrorDetected` | Count by `Branding`. | Pending UI projections can repeat; cannot join one-to-one with WTA classifications. |
| App `AgentSessionStarted` | Count distinct `StartId` by `StartKind`, `AgentId`, `AgentSource`, configuration fields, `Branding`, and `Distribution`. | Successful host-accepted New/Load observations include prewarm; a load is not necessarily a user resume. |
| WTA `AcpInitializeComplete` | Group by `Route`, `Success`, `FailureKind`; compute successes / attempts and latency percentiles from `DurationMs` within each route and outcome. | Helper, probe, and sessions-CLI populations are different; not process cold-start time. |
| WTA `AcpNewSessionComplete` | Group by `Route`, `Success`; count successes and duration percentiles per route. Select one helper route population, excluding `Probe`, for chat-session starts. | `MasterForward` and helper routes can describe the same creation; do not sum them. |
| WTA `AcpLoadSessionComplete` | Count by `Success`; calculate success share and `DurationMs` percentiles separately by outcome. | No route/session ID; cannot distinguish layout restore from session-view resume. |
| WTA `AgentColdStartComplete` | Group by `AgentId`, `Source`, `Success`, `FailureKind`; compute cold-start success share and `DurationMs` percentiles. | Warm process reuse does not emit. |
| WTA `AgentPromptSent` | Use TurnId for completion, SessionId for First-to-Second cohorts, KeepId/AttemptId for restored use. Exclude autofix for user-prompt metrics. | Observation sessions are not lifetime provider sessions. Ordinary CLI chat is not covered. |
| WTA `AgentResponseFirstToken` | Calculate median/P95 `FirstTokenLatencyMs` by `AgentId`; count events with first text. | Tool-only or timestamp-less turns may emit none; not first final-answer text. |
| WTA `AgentResponseComplete` | Match TurnId to a sent cohort, check SessionId/IsAutofix and compute success and latency. | RPC completion, not task success; missing completion is unknown. |
| WTA `ErrorDetected` | Count by `Severity`, `Method`, `AllowAutoFixPolicy`, `AutoFixEnabled`. | Only classified actionable/critical signals, not every failed command or unique incident. |
| WTA `ErrorFixOffered` | Join OfferId to detections and acceptances, keeping Source separate. | Counts repair flows, not card versions; independent Manual flows have no detection. |
| WTA `ErrorFixAccepted` | Count distinct accepted `OfferId` / distinct offered `OfferId` for the same offer cohort; allow later-window acceptances. | Confirmed Run successfully queued, not command execution or successful repair. |
| WTA `ErrorFixRunStarted` | Establish a started RunId cohort with a follow-up window. | Queued UI requests lost before the executor receives them do not emit a start. |
| WTA `ErrorFixRunResult` | Join RunId to starts; separate dispatchFailed, unobservable and missing results, and associate OfferId for repair flows. | No execution-success or repair-success metric; dispatchFailed can be ambiguous. |
| WTA `AgentSlashCommandUsed` | Count by `command`, optionally splitting distinct backend devices by command. | Built-in command dispatch before guards, not agent-provided slash commands or success. |
| WTA `SessionsViewOpened` | Count view-open routine entries and distinct backend devices. | No row-load, selection, or unique view-instance guarantee. |
| WTA `SessionResumeInvoked` | Count by `Route` and `AgentId`. | Resume-route dispatch, not ACP load success or provider-native CLI completion. |
| WTA `DelegateInvoked` | Count where `TriggerSource=Agent` separately from App `DelegateInvoked`. | Agent-requested delegation after target creation, not completed work. |
| WTA `SessionMcpToolCalled` | Count by `ToolName`, retaining `unknown` as its own bucket. | Calls reaching dispatch, before validation, approval, or execution; not agent-owned tools. |
| WTA `HookOperationCompleted` | Count by `Operation`, `Cli`, `Outcome`; compute `failed` share separately for installation and removal. | One command may emit for multiple CLIs; `skipped` is not necessarily an error. |
| Model `AgentProviderChanged` | Count by `role`, `from`, `to`; use App `AppCreated` separately for current configuration share. | Successful settings reloads, not every UI edit; custom-to-custom may hide a raw ID change. |
| Editor `AcpModelProbeStarted` | Count starts by `AgentId`, optionally by `CacheRevision` for diagnostics. | Revision is not a probe ID; do not use it for exact joins or as a retry count. |
| Editor `AcpModelProbeDiscarded` | Count stale discards by `AgentId` separately from completions. | A discarded generation is not a provider failure. |
| Editor `AcpModelProbeCompleted` | Group by `AgentId`, `Succeeded`; compute successful parsed catalogs / completed probes and `ModelCount` distribution. | No per-probe ID to join starts/discards/completions; parsing success is not cache acceptance. |

**Correlation rules:**

- `StartId` is a random per-notification deduplication key, independent of the
  agent's session ID. Loading the same session again is a separate observation.
- Raw agent session IDs are not emitted, hashed, or replaced with persistent aliases.
  WTA prompt/completion SessionId is a random helper-observation ID; TurnId
  joins one dispatch to its completion. App session-start snapshots still
  have no shared session key with these WTA events.
- `AcpNewSessionComplete` can report both master and helper layers for one
  creation. Choose the intended route rather than summing them as sessions.
- `AcpLoadSessionComplete` has no payload correlation ID or origin route.
  Do not claim a reliable session-view-versus-layout restore success rate
  from that event.
- `OfferId` joins detected candidate flows to displayed suggestions, accepted
  requests and executor observability results. Independent manual flows
  have no preceding detection. Multiple classifications can describe one failure.
- `KeepId` joins an opt-in to observed retention and reattachment in the
  same process and WTA prompts in its surviving agent session. `AttemptId`
  distinguishes retries and repeated restores. Cross-language KeepId and
  AttemptId strings use lowercase braced UUIDs, matching the C++ events.
- `EntryId` joins visible foreground palette entries to submissions, but not
  to a delegated agent's response.
- The three Editor probe events do not share a unique probe ID. Do not
  construct exact per-probe joins solely from `AgentId` or `CacheRevision`.
- Events without a completion signal, such as session MCP requests and
  delegation launches, cannot produce a completion funnel by themselves.

No delivery or completeness guarantee is implied. Account for disabled
collection, process exit, event loss, and operations still in progress
before comparing counts.

These events do **not** establish task success, automatic fix success,
time-to-fix, total response bytes, token consumption, monetary cost, session
lifetime, or unique active users. `ShowTokenUsageAndCost` is a UI setting,
not a usage or cost measurement.

## Retired events and settings filtering

| Retired event or field | Replacement / interpretation |
|---|---|
| `WTA.SlashCommandInvoked.CommandName` | Renamed to `WTA.AgentSlashCommandUsed.command`; no dual-write. Union old and new spellings across the deployment boundary when querying history |
| Legacy `AgentProviderConfigured` | Startup configuration is now carried by `App.AppCreated`; do not compare historical event counts directly. Connected-agent identity still comes from `App.AgentSessionStarted` |
| `AgentProviderConfigured(schema_version=2)`, `CustomAgentConfigured`, and `SidebarStateOnLaunch` | Not emitted as standalone events; their configuration fields are consolidated into `App.AppCreated` |
| `CustomModelProviderConfigured` | Use `ModelSource` for active session category; unused configured providers are not inventoried |
| `IntelligentFeatureConfigured` | Use the selected configuration fields on `AgentSessionStarted` |
| `ErrorFixResolved` | No reliable fix-success replacement; clearing a pending UI state is not proof a fix worked |
| `AgentResponseComplete.TotalResponseBytes` | Removed because it was not populated; historical zero values do not mean empty responses |
| Legacy MCP aliases `terminal_send`, `terminal_open`, `terminal_open_and_send` | Canonical tool names are used now; historical `unknown` values cannot be attributed retroactively |

AI-specific settings are excluded from inherited `JsonSettingsChanged` and
`UISettingsChanged` events using the exact filter below:

| Context | Excluded JSON keys |
|---|---|
| `global` agent/model selection | `acpAgent`, `acpModel`, `acpCustomCommand`, `acpCustomCommands`, `delegateAgent`, `delegateModel`, `delegateCustomCommand`, `delegateCustomCommands`, `customModelSelection`, `customModelProviders` |
| `global` feature configuration | `autoErrorDetectionEnabled`, `autoFixEnabled`, `agentSessionManagementEnabled`, `showTokenUsageAndCost`, `agentPanePosition`, `agentPane.yoloMode` |
| `global` coordinator | `aiIntegration.coordinator.enabled`, `aiIntegration.coordinator.commandline`, `aiIntegration.coordinator.profile` |
| `global` legacy confirmation | `aiIntegration.confirmation.readOperations`, `aiIntegration.confirmation.createOperations`, `aiIntegration.confirmation.inputOperations` |
| `profile` and `profileDefaults` | `agentPaneBackend`, `commandPaletteAgent` |

The filter is case-sensitive and context-specific: 22 exact global keys,
plus two keys in each profile context. Descendants starting with
`global.customModelProviders.` or `global.customModelProviders[` are also
excluded. It is not a blanket filter for all future AI settings.

Other Terminal settings retain their existing telemetry behavior, including
`tabLayout` and `firstWindowPreference`. The inherited `ActionDispatched`
event can also describe AI actions; it is not one of these 38 cataloged
events. The Settings Model provider,
`Microsoft.Windows.Terminal.Setting.Model`
(`{be579944-4d33-5202-e5d6-a7a57f1935cb}`), remains in use for inherited
settings telemetry and the independent `AgentProviderChanged` event.

## Privacy and collection boundaries

The dedicated payloads contain categories, booleans, counts, durations,
independent telemetry correlation IDs (`StartId`, `SessionId`, `TurnId`,
`OfferId`, `RunId`, `EntryId`, `KeepId`, and `AttemptId`), terminal pane
identity where documented, and numeric ACP error codes. They do not contain
raw agent/provider session identifiers, prompt/response text, terminal contents, command text, custom agent names,
custom commands, model IDs, API keys, credential identifiers, or custom
endpoint URLs.

`OfferId` is a random, locally generated correlation token; it is not
derived from a command, prompt, path, or agent-provided recommendation text.
The exported `SessionId` is likewise random and scoped to a helper's observation;
it is not a persistent alias or hash of the provider session ID.

This boundary concerns the telemetry schemas above, not every local
diagnostic log or internal IPC message. For example, helper state exchange
is not itself an ETW payload.

Privacy tags and keywords are build metadata. Local ETW emission and
decoding do not prove that a telemetry backend ingested events. Conversely,
zero OSS metadata values or an event without an explicit telemetry keyword
do not mean a local collector cannot capture it. Retention, sampling,
backend routing, and user/device identity enrichment are not specified by
these event definitions.

## Source references

| Contract | Source |
|---|---|
| General-usage interaction boundary used by the report | [WindowEmperor.cpp](../src/cascadia/WindowsTerminal/WindowEmperor.cpp) |
| Shell connection creation used by the report | [TerminalPage.cpp](../src/cascadia/TerminalApp/TerminalPage.cpp) |
| App pane, delegate, error, and snapshot emission | [TerminalPage.cpp](../src/cascadia/TerminalApp/TerminalPage.cpp) |
| Sidebar search and rich-tab field selection | [TerminalPage.cpp](../src/cascadia/TerminalApp/TerminalPage.cpp) |
| Agent-view entry and loaded row count | [TabStrip.cpp](../src/cascadia/TerminalApp/TabStrip.cpp) |
| Keep-running menu, mark counts, detach, and reattach | [TabManagement.cpp](../src/cascadia/TerminalApp/TabManagement.cpp) |
| Command Palette entry and submission | [CommandPalette.cpp](../src/cascadia/TerminalApp/CommandPalette.cpp), [CommandPaletteTelemetry.h](../src/cascadia/TerminalApp/CommandPaletteTelemetry.h) |
| Window-created configuration and provider-change tracking | [AppLogic.cpp](../src/cascadia/TerminalApp/AppLogic.cpp), [AgentProviderTelemetry.h](../src/cascadia/TerminalApp/AgentProviderTelemetry.h) |
| Provider-change emission and shared configuration helpers | [CascadiaSettingsSerialization.cpp](../src/cascadia/TerminalSettingsModel/CascadiaSettingsSerialization.cpp), [SettingsTelemetry.h](../src/cascadia/TerminalSettingsModel/SettingsTelemetry.h) |
| Session snapshot parsing and categories | [AgentSessionTelemetry.h](../src/cascadia/TerminalApp/AgentSessionTelemetry.h) |
| Helper snapshot production | [app_status_projection.rs](../tools/wta/src/app_status_projection.rs), [app_events.rs](../tools/wta/src/app_events.rs) |
| WTA event schemas and privacy tags | [telemetry.rs](../tools/wta/src/telemetry.rs) |
| Concrete autofix offer presentation / acceptance | [autofix.rs](../tools/wta/src/app/autofix.rs), [app_turn.rs](../tools/wta/src/app_turn.rs), [recommendations.rs](../tools/wta/src/ui/recommendations.rs) |
| ACP routes and turn completion | [client.rs](../tools/wta/src/protocol/acp/client.rs), [master](../tools/wta/src/master/mod.rs) |
| First-text timing | [turn_metrics.rs](../tools/wta/src/protocol/acp/turn_metrics.rs) |
| Probe and session-list RPC paths | [probe.rs](../tools/wta/src/protocol/acp/probe.rs), [sessions.rs](../tools/wta/src/cli/sessions.rs) |
| Slash/session-view dispatch and classification | [app.rs](../tools/wta/src/app.rs) |
| Session MCP dispatch | [session_mcp.rs](../tools/wta/src/master/session_mcp.rs) |
| Hook operations | [hooks.rs](../tools/wta/src/cli/hooks.rs) |
| Editor probes | [AIAgentsViewModel.cpp](../src/cascadia/TerminalSettingsEditor/AIAgentsViewModel.cpp) |
| Exact settings filter | [SettingsTelemetry.h](../src/cascadia/TerminalSettingsModel/SettingsTelemetry.h) |
| Settings definitions | [MTSMSettings.h](../src/cascadia/TerminalSettingsModel/MTSMSettings.h) |
| OSS metadata | [ProjectTelemetry.h](../dep/telemetry/ProjectTelemetry.h) |
