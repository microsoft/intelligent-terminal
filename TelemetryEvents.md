# Intelligent Terminal telemetry events

The first section inventories Intelligent Terminal's event definitions and
production emission paths. The second preserves the inherited Windows
Terminal / OpenConsole reference. See [`PRIVACY.md`](./PRIVACY.md) for privacy
information and telemetry controls.

## Intelligent Terminal-specific events

Dedicated Intelligent Terminal events use controlled categories and aggregate
measurements. They do not include prompts, responses, terminal contents, API
keys, credential identifiers, custom endpoint URLs, model identifiers, custom
agent names, or custom agent command lines.

Agent identifiers are limited to `copilot`, `claude`, `codex`, `gemini`, and
`opencode`; custom/unknown identifiers are bucketed as `custom`. The session
snapshot additionally uses `none` for an absent delegate. Correlation fields
such as ACP `SessionId` and pane `PaneId` are opaque identifiers, not content.

There are **22 dedicated event definitions**: 5 App events, 14 WTA events,
and 3 Settings Editor diagnostic ETW events. The former three AI-specific
Settings Model census events are retired. The inherited `ActionDispatched`
event can still describe AI actions; it is not a dedicated event.

`MICROSOFT_KEYWORD_MEASURES` and privacy tags describe event metadata, not
proof of backend ingestion. Collection also depends on the build's telemetry
header, enabled ETW sessions, and deployment policy. The OSS fallback header
uses stub metadata. Likewise, an ETW event without a telemetry keyword can
still be captured by a collector; do not equate it with an impossible upload.

### Common wire metadata

The event tables enumerate **business fields**. The dedicated events also
include the following payload metadata, which is not repeated in each table:

| Field | Type | Meaning | Observed in the Debug capture |
|---|---|---|---|
| `PartA_PrivTags` | UInt64 | Privacy classification supplied by the build's telemetry header | `0` |

Provider name/GUID, process/thread IDs, timestamp, level, and keyword belong
to the ETW event header, not the business payload. The 2026-09-11 capture used
the OSS [telemetry header](./dep/telemetry/ProjectTelemetry.h), so keyword was
`0x0` and `PartA_PrivTags` was `0`. These stub values do not mean collection
failed: the events were decoded from the ETL. They also do not establish
Microsoft backend ingestion.

### Provider: Microsoft.Windows.Terminal.App

This existing Terminal provider emits the following Intelligent
Terminal-specific events:

| Event | Trigger | Fields |
|---|---|---|
| `AgentPaneOpened` | An agent pane is created, restored, or opened into a requested view. Closing or stashing the pane does not emit this event. | `TriggerSource`, `Branding` |
| `CommandPaletteDispatchedAgentPrompt` | A foreground or background agent prompt is submitted through the Command Palette. | `IsBackgroundMode` |
| `DelegateInvoked` | Terminal successfully launches `wta delegate`. | `TriggerSource` (`CommandPalette` or `Action`) |
| `ErrorDetected` | Terminal receives `autofix_state` with state `pending`. This is not a deduplicated count of unique errors; the literal `detected` state does not emit it. | `Branding` |
| `AgentSessionStarted` | A helper reports successful ACP session creation or load, with any initial model override settled. | Settings snapshot below |

Current `AgentPaneOpened.TriggerSource` values are `Action`,
`SessionsAction`, `Autofix`, `FirstRunExperience`, `AgentSwitch`,
`BottomBarToggle`, `BottomBarSessions`, `SettingsReload`, and `FocusAction`.

#### `AgentSessionStarted`: per-session settings snapshot

This event belongs to `Microsoft.Windows.Terminal.App`, with measures keyword
and `PDT_ProductAndServiceUsage`. The helper supplies confirmed session
identity and runtime agent state through `agent_state_changed.session_started`;
the owning Terminal tab supplies effective host settings. This avoids treating
a settings-file load or a switch toggle as a session start.

The wire payload contains **24 business fields plus `PartA_PrivTags`**.
The observed-value column comes from the 2026-09-11 live ETW captures; values
are examples, not defaults or a test of every allowed value. Correlation IDs
are omitted below rather than publishing actual session identifiers.

| Field | Type | Meaning / controlled values | Observed value |
|---|---|---|---|
| `StartId` | String | New random UUID for this successful start/load; event-level deduplication key | Unique UUID per event |
| `SessionId` | String | ACP session identifier, not `WT_SESSION`; reused when the same saved session is loaded again | ACP session UUID |
| `StartKind` | String | `New` or `Load`; `Load` covers both ACP session-view resume and saved-layout restore when ACP load succeeds | `New`, `Load` |
| `AgentId` | String | Actually connected agent category | `copilot` |
| `AgentSource` | String | `host`, `wsl`, or `unknown`; no distribution name | `host` |
| `DelegateAgentId` | String | Helper's current resolved delegate category, or `none` | `copilot` |
| `ModelSource` | String | `byok`, `provider`, or `unknown`; helper's active BYOK process binding or confirmed session model category, not a model identifier or inventory of configured providers | `provider` (new), `unknown` (load) |
| `AutoErrorDetection` | Bool | Policy-aware host effective setting | `true` |
| `AutoFix` | Bool | Helper runtime autofix switch AND policy-aware host effective setting | `false`, `true` |
| `AgentSessionManagement` | Bool | Policy-aware host effective setting | `true` |
| `AgentPanePosition` | WideString | Owning tab's effective `left`, `right`, `up`, or `bottom`; otherwise `unknown` | `bottom` |
| `ShowTokenUsageAndCost` | Bool | Current usage/cost UI setting | `true` |
| `VerticalTabs` | Bool | Current tab layout is vertical | `false` |
| `FirstWindowPreference` | String | `defaultProfile`, `persistedLayout`, or `persistedLayoutAndContent` | `defaultProfile` |
| `AutomaticYolo` | String | `enabled` / `disabled` automatic reconciliation target, or `provider` when no automatic directive applies | `disabled` (new), `provider` (load) |
| `YoloPolicyBlocked` | Bool | Helper runtime policy prohibits requesting YOLO enablement | `false` |
| `YoloControlOwner` | String | `automatic`, `manual`, `provider-restored`, or `unknown` | `automatic` (new), `provider-restored` (load) |
| `CoordinatorConfigured` | Bool | Configured legacy coordinator switch; not evidence a coordinator is running | `false` |
| `ReadConfirmationConfigured` | WideString | Configured read-operation value: `auto`, `prompt`, or `unknown` | `auto` |
| `CreateConfirmationConfigured` | WideString | Configured create-operation value: `auto`, `prompt`, or `unknown` | `auto` |
| `InputConfirmationConfigured` | WideString | Configured input-operation value: `auto`, `prompt`, or `unknown` | `auto` |
| `SessionMcpConfirmation` | String | `user`: session MCP mutations follow their existing user-confirmed action path | `user` |
| `Branding` | UInt8 | 0 other/development, 1 Canary, 2 Preview, 3 Release | `0` |
| `Distribution` | UInt8 | 0 other/unpackaged, 1 portable, 2 packaged; packaged does not mean Store-installed | `2` |

Emission rules:

- Successful initial connections, lazy-created sessions, `/new`, and successful
  loads are covered. Re-loading the same saved session emits a fresh `StartId`.
- A handshake-only bootstrap used before initial load, failed creation/load,
  and ordinary state projections do not emit a snapshot. Duplicate attach
  notifications for an already-reported new session are suppressed.
- For a fresh session with a tab/global model override, emission waits for the
  model-set result. A failed override reports the prior confirmed model, not
  the requested value. Loaded sessions retain their restored model; a
  BYOK-bound process remains `byok` even if the agent returns a provider-native
  model identifier for the restored session.
- The tab-owned deduplication marker follows tab state through renaming/moves.
  Opening, closing, or stashing an existing pane and changing a setting do not
  create another snapshot.
- This is a successful-session population, including pre-warmed sessions;
  it does not measure users who never establish a helper ACP session. A helper
  without the owning Terminal host cannot emit this host-provider event.
  Provider-native `Cli` resume is not an ACP load and does not itself emit it.

`AutomaticYolo` describes configuration intent and ownership, **not confirmed
provider-native permission mode**. The three `*ConfirmationConfigured` fields
are deliberately labelled configured: the legacy `aiIntegration.confirmation.*`
settings are not enforced by the current runtime operation paths. They must
not be interpreted as active security policy. This change does not introduce
or remove confirmation behavior.

Sources: [helper projection](./tools/wta/src/app_status_projection.rs),
[session lifecycle](./tools/wta/src/app_events.rs),
[host consumer](./src/cascadia/TerminalApp/TerminalPage.cpp),
[boundary allowlists](./src/cascadia/TerminalApp/AgentSessionTelemetry.h), and
[authoritative settings](./src/cascadia/TerminalSettingsModel/MTSMSettings.h).

#### Retired settings telemetry

`AgentProviderConfigured`, `CustomModelProviderConfigured`, and
`IntelligentFeatureConfigured` are no longer emitted. Use `AgentSessionStarted`
for session-level configuration analysis instead of combining a settings-load
census with subsequent sessions. Its BYOK category intentionally does not
report unused configured providers or credential-reference state.

AI-specific setting keys are also excluded from the inherited
`JsonSettingsChanged` and `UISettingsChanged` events. Other Terminal settings
retain their existing behavior; in particular `tabLayout` and
`firstWindowPreference` remain ordinary Terminal settings. AI action dispatch
telemetry is unaffected.

### Provider: Microsoft.Windows.Terminal.WTA

- **GUID:** `{4cfcff80-4e6b-5bfd-8ea1-d38e1226f70b}`
- **Keyword:** `MICROSOFT_KEYWORD_MEASURES`
- **Level:** `Verbose`

#### ACP lifecycle and performance

| Event | Trigger | Fields |
|---|---|---|
| `AcpInitializeComplete` | An ACP `initialize` attempt completes or times out. | `DurationMs`, `Success`, `Route`, `FailureKind`, `AcpErrorCode` |
| `AcpNewSessionComplete` | An ACP `session/new` attempt completes or times out. | `SessionId` (empty on failure), `DurationMs`, `Success`, `Route`, `FailureKind`, `AcpErrorCode` |
| `AcpLoadSessionComplete` | An ACP `session/load` attempt completes, covering both durable agent-pane restore and an explicit resume from the session view. | `DurationMs`, `Success` |
| `AgentColdStartComplete` | A newly spawned agent process finishes or fails its ACP initialization. Warm process-pool reuse does not emit this event. | `AgentId`, `Source`, `DurationMs`, `Success`, `FailureKind` |

ACP lifecycle durations use monotonic clocks. `FailureKind` is empty on success;
ACP RPC failures use `AcpError` or `Timeout`. Cold-start failures use
`SpawnFailed`, `InitializeFailed`, or `Timeout`. `AgentColdStartComplete.Source`
is `Host` or `Wsl`.

`Route` identifies the instrumented RPC layer, not a distinct logical session.
In the live capture, four new sessions produced eight
`AcpNewSessionComplete` events: one `MasterForward` event and one
`HelperPipeStartup` or `HelperPipeNewSessionForTab` event per session.
Filter by route when analyzing RPC timings; do not count all of these events
as separate sessions. Use `AgentSessionStarted` for logical successful
starts/loads, with `StartId` as the event deduplication key.

#### Agent turns and autofix

| Event | Trigger | Fields |
|---|---|---|
| `AgentPromptSent` | WTA dispatches a prompt to an agent over ACP, including manual and automatic autofix prompts. | `SessionId`, `PromptLengthBytes`, `IsAutofix`, `IsByok`, `AgentId`, `TemplateKind`, `Route` (`AcpDispatch`) |
| `AgentResponseFirstToken` | The first user-visible response chunk arrives. | `SessionId`, `FirstTokenLatencyMs`, `ChunkLengthBytes`, `AgentId` |
| `AgentResponseComplete` | The ACP prompt request completes. | `SessionId`, `TotalDurationMs`, `Success`, `IsByok`, `AgentId` |
| `ErrorDetected` | WTA's classifier identifies an actionable or critical pane error. | `Severity`, `Method`, `PaneId` |

Prompt and response text is never included. Length fields contain byte counts
only, not UI character counts; `PromptLengthBytes` describes the constructed
ACP dispatch prompt, not just the user's typed text. `IsByok` is captured for
the specific prompt so live model changes are attributed correctly. The
always-zero `TotalResponseBytes` field has been
removed; do not interpret historical zero values as empty responses.

`ErrorFixResolved` is retired. Its old timestamp started when autofix
**analysis** was submitted, not when a fix was executed. Execution paths clear
that timestamp; the old event could instead fire when a later prompt-start or
exit-zero merely cleared the pending UI. Neither signal establishes that an
applied fix succeeded. UI clearing is unchanged, but it no longer emits a
success-shaped metric. No current event provides a reliable automatic
fix-success rate or time-to-fix; those require execution-correlated tracking.

#### Commands, sessions, delegation, MCP, and hooks

| Event | Trigger | Fields and controlled values |
|---|---|---|
| `SlashCommandInvoked` | WTA dispatches a registered slash command. | `CommandName`: `help`, `clear`, `new`, `fix`, `restart`, `stop`, `sessions`, `agent`, `model`, `config`, or `move` |
| `SessionsViewOpened` | The Agent Session View is opened. | None |
| `SessionResumeInvoked` | A resume operation is dispatched from Agent Session View. | `Route` (`AgentPane` for ACP `session/load`, or `Cli` for provider-native CLI resume), `AgentId` |
| `DelegateInvoked` | An agent recommendation invokes the configured delegate through WTA. | `TriggerSource` (`Agent`) |
| `SessionMcpToolCalled` | A session MCP function is invoked. | `ToolName`: `run_command_in_current_shell`, `create_workspace`, `delegate_task_in_new_workspace`, `request_user_input`, or `unknown` |
| `HookOperationCompleted` | Hook installation or uninstallation finishes for one supported CLI. | `Operation` (`Install` or `Uninstall`), `Cli` (`copilot`, `claude`, `gemini`, `codex`, or `opencode`), `Outcome` |

Install outcomes are `installed`, `skipped`, or `failed`. Uninstall outcomes
are `succeeded`, `skipped`, or `failed`.

The MCP allowlist now uses the canonical registered tool names instead of the
obsolete `terminal_send` / `terminal_open` / `terminal_open_and_send` aliases.
Historical `unknown` buckets cannot be retroactively attributed to a tool.

`SlashCommandInvoked` records built-in dispatch attempts before command guards,
not successful completion. Busy `/new` and idle `/stop` still count; merely
browsing autocomplete does not. ACP agent-provided slash commands instead
produce `AgentPromptSent` with `TemplateKind=AgentCommand`; their names and
arguments are not recorded. An unknown slash command may proceed as an ordinary
prompt. `/fix` and `/sessions` can also produce downstream prompt/view events.

Sources: [WTA schemas](./tools/wta/src/telemetry.rs),
[turn accounting](./tools/wta/src/protocol/acp/turn_metrics.rs),
[slash dispatch](./tools/wta/src/app.rs), and
[session MCP](./tools/wta/src/agent_tools/session_mcp.rs).

### Additional Settings Editor ETW probes

`AcpModelProbeStarted`, `AcpModelProbeDiscarded`, and `AcpModelProbeCompleted`
are diagnostic events on the Settings Editor provider. They do not explicitly
set a telemetry keyword; all use level `Info` and
`PDT_ProductAndServicePerformance`.

| Event | Trigger | Fields |
|---|---|---|
| `AcpModelProbeStarted` | A clean catalog probe begins | `AgentId`, `CacheRevision` |
| `AcpModelProbeDiscarded` | A newer generation supersedes its result | `AgentId` |
| `AcpModelProbeCompleted` | The current probe returns | `AgentId`, `Succeeded`, `ModelCount` |

Custom agents are bucketed as `custom`. No probe command or model identifier
is included. See [AI settings](./src/cascadia/TerminalSettingsEditor/AIAgentsViewModel.cpp).

### Live ETW field verification (2026-09-11)

A local Debug capture collected the App, WTA, and Settings Model providers
at verbose level with all keywords enabled. It contained **46 events:
44 product events and 2 trace infrastructure events, with 0 events lost**.
This was an actual application/helper run, not an injected telemetry payload
or unit-test event sink.

| Event / check | Count | Observed field result |
|---|---|---|
| `AgentSessionStarted` | 4 | All 24 business fields and `PartA_PrivTags` decoded; one `New` snapshot per distinct session, with unique `StartId` values and matching configured settings |
| `AcpInitializeComplete` | 2 | `DurationMs`, `Success`, `Route`, `FailureKind`, `AcpErrorCode` |
| `AcpNewSessionComplete` | 8 | `SessionId`, `DurationMs`, `Success`, `Route`, `FailureKind`, `AcpErrorCode`; two instrumented layers per logical session |
| `AgentColdStartComplete` | 1 | `AgentId`, `Source`, `DurationMs`, `Success`, `FailureKind` |
| `AgentPromptSent` | 2 | `SessionId`, `PromptLengthBytes`, `IsAutofix`, `IsByok`, `AgentId`, `TemplateKind`, `Route`; observed `TemplateKind=Planner`, `Route=AcpDispatch` |
| `AgentResponseFirstToken` | 1 | `SessionId`, `FirstTokenLatencyMs`, `ChunkLengthBytes`, `AgentId` |
| `AgentResponseComplete` | 2 | `SessionId`, `TotalDurationMs`, `Success`, `IsByok`, `AgentId`; `TotalResponseBytes` absent |
| `SessionMcpToolCalled` | 1 | `ToolName=run_command_in_current_shell`; the harmless command was confirmed and executed through the normal action path |
| `SlashCommandInvoked` | 4 | `CommandName` values `help`, `new`, `new`, `stop`; idle `/stop` still counted as a dispatch attempt |
| Retired AI census events and `ErrorFixResolved` | 0 | None observed during the exercised startup/turn/action paths |
| `JsonSettingsChanged` | 3 | Retained `Setting` values `global.tabLayout`, `global.warning.confirmOnClose`, and `profile.hidden`; AI keys absent in this sample |

Every dedicated event observed above also carried `PartA_PrivTags=0`.
The smoke prompt, reply marker, and command text were absent from the decoded
telemetry payloads. User settings remained unchanged.

**Sampling-build scope:** the first capture used the telemetry-only changes.
Its `resume_in_new_agent_tab` control request did not reach `session/load`,
so it contained neither `AcpLoadSessionComplete` nor a `StartKind=Load`
snapshot. A temporary resume/prewarm startup-order experiment was used for
the subsequent three completed captures, allowing a saved ACP session to be
loaded and its historical conversation displayed during the follow-up run.

That experiment and its associated C318 regression test have since been
removed from this PR. **This telemetry work retains the original session
resume/prewarm routing; it does not ship a resume behavior fix.** The records
below preserve the actual event payload observations from those sampling
builds, not a claim that the experiment identifies a product bug or that
the final telemetry-only branch's resume behavior was revalidated.

The follow-up capture ran from **04:36:18 to 04:37:27 UTC**, with **27 events:
25 product events and 2 trace infrastructure events, with 0 events lost**.

| Event / check | Count | Observed field result |
|---|---|---|
| `AcpLoadSessionComplete` | 1 | `DurationMs=1856.459100` (Double), `Success=true` (Bool), `PartA_PrivTags=0` (UInt64); this event has no `SessionId` or `Route` field |
| `AgentSessionStarted` (`StartKind=Load`) | 1 | All 24 business fields plus `PartA_PrivTags`; `SessionId` matched the requested saved session, with a new `StartId` |
| `AgentSessionStarted` (`StartKind=New`) | 1 | All 24 business fields plus `PartA_PrivTags`, from the ordinary tab's independently prewarmed session |

For the loaded snapshot, `ModelSource=unknown`, `AutomaticYolo=provider`, and
`YoloControlOwner=provider-restored`; the table above records these actual
values rather than assuming that the global model/YOLO preference was applied
to a restored session. User settings remained unchanged. Existing provider
names/GUIDs, registration, and keyword/privacy constants were not changed.

At this stage, **10 of the 22 dedicated event types** had been observed.
The Settings Editor provider was not enabled in those first two captures.

#### Completed event-type coverage

Two additional completed captures exercised the remaining event types,
including the Settings Editor provider
`Microsoft.Windows.Terminal.Settings.Editor`
(`{1b16317d-b594-51f8-c552-5d50572b5efc}`). This added the existing provider to
the collector; no product provider identity or registration was changed.
The additional runs covered **05:53:33-06:13:35 UTC** and
**06:43:03-06:49:19 UTC**, collecting 83 and 29 events respectively.

Across the **four completed captures described above**, all **22/22 dedicated
event types** were observed. Every captured instance of these events passed an exact
field-name-set check against the documented business fields plus
`PartA_PrivTags`. The captures contain **185 total events: 177 product events
and 8 trace infrastructure events, with 0 events lost**. An interrupted
intermediate capture is excluded from these results.

| Provider | Event | Total instances | Business fields verified |
|---|---|---|---|
| App | `AgentPaneOpened` | 1 | 2 |
| App | `CommandPaletteDispatchedAgentPrompt` | 1 | 1 |
| App | `DelegateInvoked` | 1 | 1 |
| App | `ErrorDetected` | 3 | 1 |
| App | `AgentSessionStarted` | 11 | 24 |
| WTA | `AcpInitializeComplete` | 15 | 5 |
| WTA | `AcpNewSessionComplete` | 25 | 6 |
| WTA | `AcpLoadSessionComplete` | 1 | 2 |
| WTA | `AgentColdStartComplete` | 3 | 5 |
| WTA | `AgentPromptSent` | 4 | 7 |
| WTA | `AgentResponseFirstToken` | 2 | 4 |
| WTA | `AgentResponseComplete` | 4 | 5 |
| WTA | `ErrorDetected` | 2 | 3 |
| WTA | `SlashCommandInvoked` | 5 | 1 |
| WTA | `SessionsViewOpened` | 1 | 0 |
| WTA | `SessionResumeInvoked` | 1 | 2 |
| WTA | `DelegateInvoked` | 1 | 1 |
| WTA | `SessionMcpToolCalled` | 3 | 1 |
| WTA | `HookOperationCompleted` | 1 | 3 |
| Settings Editor | `AcpModelProbeStarted` | 5 | 2 |
| Settings Editor | `AcpModelProbeDiscarded` | 2 | 1 |
| Settings Editor | `AcpModelProbeCompleted` | 3 | 3 |

The zero-business-field `SessionsViewOpened` event still carries
`PartA_PrivTags`. App and WTA events with the same name are separate
provider/event pairs, not one combined event definition.

Observed operations and values:

- Opening the agent pane through the bottom bar produced
  `AgentPaneOpened(TriggerSource=BottomBarToggle, Branding=0)`.
- A harmless real `cmd /c exit 17`, with autofix temporarily enabled,
  produced WTA `ErrorDetected(Severity=Actionable, Method=vt_sequence)`
  and App `ErrorDetected(Branding=0)`. A later test CLI exit also produced
  WTA `ErrorDetected` with `Method=connection_state`.
- Submitting a harmless foreground prompt through the real Command Palette
  produced `CommandPaletteDispatchedAgentPrompt(IsBackgroundMode=0)` and
  App `DelegateInvoked(TriggerSource=CommandPalette)`.
- Confirming a harmless agent-requested new-tab delegation through the
  normal action card produced WTA `DelegateInvoked(TriggerSource=Agent)`.
  The additional MCP calls used `request_user_input` and
  `delegate_task_in_new_workspace`.
- Opening `/sessions` produced `SessionsViewOpened`. Selecting the completed
  synthetic CLI session in that view and pressing Enter produced
  `SessionResumeInvoked(Route=Cli, AgentId=copilot)` and launched the same
  saved session ID. This is distinct from the earlier successful ACP load
  smoke: that used the production `resume_in_new_agent_tab` control request.
  Neither test establishes restart-time window/layout restoration coverage.
- Smart hook reconciliation for the already-current Copilot installation
  produced `HookOperationCompleted(Operation=Install, Cli=copilot,
  Outcome=skipped)`; no forced reinstall or uninstall was used.
- Opening AI Settings and switching its agent selection from Copilot to
  Claude and back exercised real catalog probes. Started events included
  `AgentId` and `CacheRevision`; completed events reported
  `Succeeded=true` with 18 Copilot models or 6 Claude models. Rapid selection
  changes superseded two probes, producing `AcpModelProbeDiscarded` with
  the corresponding `AgentId`. These are observed catalog sizes, not defaults.

All dedicated-event payloads were checked for the known smoke prompt,
reply, and command markers; none were present. Retired events remained absent.
Temporary test settings were restored byte-for-byte, and Settings UI probe
selections were not saved. Raw traces and state backups remain local and are
not part of the repository.

These historical captures provide **event-type and payload-field coverage**,
not a post-removal live validation of the final branch's routing or coverage
of every enum value, failure branch, or timing permutation. BYOK, WSL, and GPO/override
matrices were not covered; Claude was exercised for Settings catalog probes,
not a full chat/resume workflow. Local ETW emission and decoding do not
establish backend ingestion.

## Inherited Windows Terminal / OpenConsole reference

The following historical inventory covers `TraceLoggingWrite(...)` call sites
whose arguments contain a Microsoft telemetry keyword
(`MICROSOFT_KEYWORD_MEASURES`, `MICROSOFT_KEYWORD_TELEMETRY`, or
`MICROSOFT_KEYWORD_CRITICAL_DATA`). A keyword alone does not establish upload
or ingestion.

Events grouped by their `TRACELOGGING_DEFINE_PROVIDER`. Diagnostic-only ETW traces tagged with `TIL_KEYWORD_TRACE` (`UiaTracing.cpp`, `parser/tracing.cpp`, most of `host/tracing.cpp`, the server `*Dispatchers.cpp`, `VtIo.cpp`, `VtInputThread.cpp`, etc.) are excluded.

Conventions used in the field tables below:
- **Type** uses the trailing portion of the `TraceLogging<Type>(...)` macro (e.g., `TraceLoggingBool` → `Bool`, `TraceLoggingValue` → `Value (auto)`).
- **Description** is the optional 3rd-argument string literal on the metadata macro; a dash (`—`) means none was supplied.
- **Source expression** is paraphrased; literal constants are shown in quotes.
- **Source links** are pinned to commit [`fb71a04`](https://github.com/microsoft/terminal/tree/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e) so that the line numbers stay accurate even as the codebase evolves.

---

## Provider: Microsoft.Windows.Terminal.App

- **Symbol:** `g_hTerminalAppProvider`
- **GUID:** `{24a1622f-7da7-5c77-3303-d850bd1ab2ed}`
- **Defined in:** [`src\cascadia\TerminalApp\init.cpp`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/init.cpp)
- **Privacy tag (all events):** `PDT_ProductAndServiceUsage`
- **Keyword (all events):** `MICROSOFT_KEYWORD_MEASURES`

### `ActionDispatched`
- **Description:** Event emitted when an action was successfully performed.
- **Source:** [`src\cascadia\TerminalApp\ShortcutActionDispatch.cpp:67`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/ShortcutActionDispatch.cpp#L67)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `Action` | Value (int) | `static_cast<int>(actionAndArgs.Action())` | — |
  | `Branding` | Value | `branding` (build branding string) | — |

### `AppCreated`
- **Description:** Event emitted when the application is started.
- **Source:** [`src\cascadia\TerminalApp\AppLogic.cpp:193`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/AppLogic.cpp#L193)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `TabsInTitlebar` | Bool | `_settings.GlobalSettings().ShowTabsInTitlebar()` | — |

### `AppInitialized`
- **Description:** Event emitted once the app is initialized.
- **Source:** [`src\cascadia\TerminalApp\AppLogic.cpp:475`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/AppLogic.cpp#L475)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `latency` | Float32 | `latency` (initialization latency) | — |

### `CommandPaletteDismissed`
- **Description:** Event emitted when the user dismisses the Command Palette without selecting an action.
- **Source:** [`src\cascadia\TerminalApp\CommandPalette.cpp:913`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/CommandPalette.cpp#L913)
- **Fields:** _(none)_

### `CommandPaletteDispatchedAction`
- **Description:** Event emitted when the user selects an action in the Command Palette.
- **Source:** [`src\cascadia\TerminalApp\CommandPalette.cpp:801`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/CommandPalette.cpp#L801)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `SearchTextLength` | UInt32 | `searchTextLength` | Number of characters in the search string. |
  | `NestedCommandDepth` | UInt32 | `nestedCommandDepth` | The depth in the tree of commands for the dispatched action. |

### `CommandPaletteDispatchedCommandline`
- **Description:** Event emitted when the user runs a commandline in the Command Palette.
- **Source:** [`src\cascadia\TerminalApp\CommandPalette.cpp:873`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/CommandPalette.cpp#L873)
- **Fields:** _(none)_

### `CommandPaletteOpened`
- **Description:** Event emitted when the Command Palette is opened.
- **Source:** [`src\cascadia\TerminalApp\CommandPalette.cpp:64`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/CommandPalette.cpp#L64)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `Mode` | WideString | `L"Action"` (literal) | Which mode the palette was opened in. |

### `ConnectionCreated`
- **Description:** Event emitted upon the creation of a connection.
- **Source:** [`src\cascadia\TerminalApp\TerminalPage.cpp:1617`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalPage.cpp#L1617)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `ConnectionTypeGuid` | Guid | `connectionType` | The type of the connection. |
  | `ProfileGuid` | Guid | `profile.Guid()` | The profile's GUID. |
  | `SessionGuid` | Guid | `connection.SessionId()` | The `WT_SESSION`'s GUID. |

### `NewTabByDragDrop`
- **Description:** Event emitted when the user drag&drops onto the new tab button.
- **Source:** [`src\cascadia\TerminalApp\TerminalPage.cpp:591`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalPage.cpp#L591)
- **Fields:** _(none)_

### `NewTabMenuClosed`
- **Description:** Event emitted when the new tab menu is closed.
- **Source:** [`src\cascadia\TerminalApp\TerminalPage.cpp:1110`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalPage.cpp#L1110)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `TabCount` | Value (int) | `page->NumberOfTabs()` | The count of tabs currently opened in this window. |

### `NewTabMenuCreatedNewTerminalSession`
- **Description:** Event emitted when a new terminal was created via the new tab menu.
- **Source:** [`src\cascadia\TerminalApp\TerminalPage.cpp:1498`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalPage.cpp#L1498)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `NewTabCount` | Value (int) | `NumberOfTabs()` | The count of tabs currently opened in this window. |
  | `SessionType` | Value | `sessionType` | The type of session that was created. |

### `NewTabMenuDefaultButtonClicked`
- **Description:** Event emitted when the default button from the new tab split button is invoked.
- **Source:** [`src\cascadia\TerminalApp\TerminalPage.cpp:400`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalPage.cpp#L400)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `TabCount` | Value (int) | `page->NumberOfTabs()` | The count of tabs currently opened in this window. |

### `NewTabMenuItemClicked`
- **Description:** Event emitted when an item from the new tab menu is invoked.
- **Source (5 call sites, distinct `ItemType` constant per call site):**
  - [`src\cascadia\TerminalApp\TerminalPage.cpp:1322`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalPage.cpp#L1322) — `ItemType = "Profile"`
  - [`src\cascadia\TerminalApp\TerminalPage.cpp:1377`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalPage.cpp#L1377) — `ItemType = "Action"`
  - [`src\cascadia\TerminalApp\TerminalPage.cpp:1721`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalPage.cpp#L1721) — `ItemType = "Settings"` (also adds the extra `SettingsTarget` field)
  - [`src\cascadia\TerminalApp\TerminalPage.cpp:1743`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalPage.cpp#L1743) — `ItemType = "CommandPalette"`
  - [`src\cascadia\TerminalApp\TerminalPage.cpp:1764`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalPage.cpp#L1764) — `ItemType = "About"`
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `TabCount` | Value (int) | `page->NumberOfTabs()` / `NumberOfTabs()` | The count of tabs currently opened in this window. |
  | `ItemType` | Value (string) | `"Profile"` / `"Action"` / `"Settings"` / `"CommandPalette"` / `"About"` | The type of item that was clicked in the new tab menu. |
  | `SettingsTarget` | Value (string) | `targetAsString` (only for the `"Settings"` variant at line 1721) | The target settings file or UI. |

### `NewTabMenuItemElevateSubmenuItemClicked`
- **Description:** Event emitted when the elevate submenu item from the new tab menu is invoked.
- **Source:** [`src\cascadia\TerminalApp\TerminalPage.cpp:5935`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalPage.cpp#L5935)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `TabCount` | Value (int) | `page->NumberOfTabs()` | The count of tabs currently opened in this window. |

### `NewTabMenuOpened`
- **Description:** Event emitted when the new tab menu is opened.
- **Source:** [`src\cascadia\TerminalApp\TerminalPage.cpp:1092`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalPage.cpp#L1092)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `TabCount` | Value (int) | `page->NumberOfTabs()` | The count of tabs currently opened in this window. |

### `QuickFixSuggestionUsed`
- **Description:** Event emitted when a winget suggestion is used.
- **Source (2 call sites, distinct `Source` constant per call site):**
  - [`src\cascadia\TerminalApp\SuggestionsControl.cpp:739`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/SuggestionsControl.cpp#L739) — `Source = "SuggestionsUI"`
  - [`src\cascadia\TerminalApp\TerminalPage.cpp:5667`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalPage.cpp#L5667) — `Source = "QuickFixMenu"`
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `Source` | Value (string) | `"SuggestionsUI"` / `"QuickFixMenu"` | — |

### `SuggestionsControlDismissed`
- **Description:** Event emitted when the user dismisses the Command Palette without selecting an action.
- **Source:** [`src\cascadia\TerminalApp\SuggestionsControl.cpp:798`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/SuggestionsControl.cpp#L798)
- **Fields:** _(none)_

### `SuggestionsControlDispatchedAction`
- **Description:** Event emitted when the user selects an action in the Command Palette.
- **Source:** [`src\cascadia\TerminalApp\SuggestionsControl.cpp:749`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/SuggestionsControl.cpp#L749)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `SearchTextLength` | UInt32 | `searchTextLength` | Number of characters in the search string. |
  | `NestedCommandDepth` | UInt32 | `nestedCommandDepth` | The depth in the tree of commands for the dispatched action. |

### `SuggestionsControlOpened`
- **Description:** Event emitted when the Command Palette is opened.
- **Source:** [`src\cascadia\TerminalApp\SuggestionsControl.cpp:86`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/SuggestionsControl.cpp#L86)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `Mode` | WideString | `L"Action"` (literal) | Which mode the palette was opened in. |

### `TabRenamerClosed`
- **Description:** Event emitted when the tab renamer is closed.
- **Source:** [`src\cascadia\TerminalApp\TabHeaderControl.cpp:112`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TabHeaderControl.cpp#L112)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `CancelledRename` | Boolean | `_renameCancelled` | True if the user cancelled the rename, false if they committed. |

### `TabRenamerOpened`
- **Description:** Event emitted when the tab renamer is opened.
- **Source:** [`src\cascadia\TerminalApp\TabHeaderControl.cpp:85`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TabHeaderControl.cpp#L85)
- **Fields:** _(none)_

### `WindowCreated`
- **Description:** Event emitted when the window is started.
- **Source:** [`src\cascadia\TerminalApp\TerminalWindow.cpp:226`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/TerminalWindow.cpp#L226)
- **Fields:** _(none)_

---

## Provider: Microsoft.Windows.Terminal.Settings.Editor

- **Symbol:** `g_hTerminalSettingsEditorProvider`
- **GUID:** `{1b16317d-b594-51f8-c552-5d50572b5efc}`
- **Defined in:** [`src\cascadia\TerminalSettingsEditor\init.cpp`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/init.cpp)
- **Privacy tag (all events):** `PDT_ProductAndServiceUsage`
- **Keyword (all events):** `MICROSOFT_KEYWORD_MEASURES`

### `AddNewProfile`
- **Description:** Event emitted when the user adds a new profile.
- **Source (2 call sites, distinct `Type` constant per call site):**
  - [`src\cascadia\TerminalSettingsEditor\AddProfile.cpp:45`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/AddProfile.cpp#L45) — `Type = "EmptyProfile"`
  - [`src\cascadia\TerminalSettingsEditor\AddProfile.cpp:62`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/AddProfile.cpp#L62) — `Type = "Duplicate"` (also adds `SourceProfileHasSource`)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `Type` | Value (string) | `"EmptyProfile"` / `"Duplicate"` | The type of the creation method (i.e. empty profile, duplicate). |
  | `SourceProfileHasSource` | Value (bool) | `!selectedProfile.Source().empty()` (only for the `"Duplicate"` variant at line 62) | True if the source profile has a `source` (i.e. dynamic profile generator namespace, fragment). Otherwise, false, indicating it's based on a custom profile. |

### `CreateUnfocusedAppearance`
- **Description:** Event emitted when the user creates an unfocused appearance for a profile.
- **Source:** [`src\cascadia\TerminalSettingsEditor\Profiles_Appearance.cpp:87`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Profiles_Appearance.cpp#L87)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `IsProfileDefaults` | Value (bool) | `_Profile.IsBaseLayer()` | If the modified profile is the `profile.defaults` object. |
  | `ProfileGuid` | Value (GUID) | `static_cast<GUID>(_Profile.Guid())` | The guid of the profile that was navigated to. |
  | `ProfileSource` | Value (wide string) | `_Profile.Source().c_str()` | The source of the profile that was navigated to. |

### `DeleteProfile`
- **Description:** Event emitted when the user deletes a profile.
- **Source:** [`src\cascadia\TerminalSettingsEditor\Profiles_Base.cpp:93`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Profiles_Base.cpp#L93)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `ProfileGuid` | Value (string) | `to_hstring(_Profile.Guid()).c_str()` | The guid of the profile that was navigated to. |
  | `ProfileSource` | Value (wide string) | `_Profile.Source().c_str()` | The source of the profile that was navigated to. |
  | `Orphaned` | Value (bool) | `false` (literal) | Tracks if the profile is orphaned. |
  | `Hidden` | Value (bool) | `_Profile.Hidden()` | Tracks if the profile is hidden. |

### `NavigatedToPage`
- **Description:** Event emitted when the user navigates to a page in the settings UI.
- **Source (17 call sites; each call site emits a distinct `PageId` constant — see the list below):**

  | Source | `PageId` constant | Extra fields beyond `PageId` |
  |---|---|---|
  | [`src\cascadia\TerminalSettingsEditor\Launch.cpp:47`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Launch.cpp#L47) | `"startup"` | — |
  | [`src\cascadia\TerminalSettingsEditor\Interaction.cpp:28`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Interaction.cpp#L28) | `"interaction"` | — |
  | [`src\cascadia\TerminalSettingsEditor\Extensions.cpp:53`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Extensions.cpp#L53) | `"extensions.extensionView"` | `FragmentSource`, `FragmentCount`, `Enabled` |
  | [`src\cascadia\TerminalSettingsEditor\Extensions.cpp:66`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Extensions.cpp#L66) | `"extensions"` | `ExtensionPackageCount`, `ProfilesModifiedCount`, `ProfilesAddedCount`, `ColorSchemesAddedCount` |
  | [`src\cascadia\TerminalSettingsEditor\GlobalAppearance.cpp:30`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/GlobalAppearance.cpp#L30) | `"globalAppearance"` | — |
  | [`src\cascadia\TerminalSettingsEditor\AddProfile.cpp:33`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/AddProfile.cpp#L33) | `"addProfile"` | — |
  | [`src\cascadia\TerminalSettingsEditor\Actions.cpp:37`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Actions.cpp#L37) | `"actions"` | — |
  | [`src\cascadia\TerminalSettingsEditor\Compatibility.cpp:62`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Compatibility.cpp#L62) | `"compatibility"` | — |
  | [`src\cascadia\TerminalSettingsEditor\ColorSchemes.cpp:48`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/ColorSchemes.cpp#L48) | `"colorSchemes"` | — |
  | [`src\cascadia\TerminalSettingsEditor\EditColorScheme.cpp:48`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/EditColorScheme.cpp#L48) | `"colorSchemes.editColorScheme"` | `SchemeName` |
  | [`src\cascadia\TerminalSettingsEditor\NewTabMenu.cpp:48`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/NewTabMenu.cpp#L48) | `"newTabMenu"` or `"newTabMenu.folderView"` | — |
  | [`src\cascadia\TerminalSettingsEditor\Profiles_Advanced.cpp:33`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Profiles_Advanced.cpp#L33) | `"profile.advanced"` | `IsProfileDefaults`, `ProfileGuid`, `ProfileSource` |
  | [`src\cascadia\TerminalSettingsEditor\Profiles_Appearance.cpp:65`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Profiles_Appearance.cpp#L65) | `"profile.appearance"` | `IsProfileDefaults`, `ProfileGuid`, `ProfileSource`, `HasBackgroundImage`, `HasUnfocusedAppearance` |
  | [`src\cascadia\TerminalSettingsEditor\Profiles_Base.cpp:59`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Profiles_Base.cpp#L59) | `"profile"` | `IsProfileDefaults`, `ProfileGuid`, `ProfileSource` |
  | [`src\cascadia\TerminalSettingsEditor\Profiles_Base_Orphaned.cpp:44`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Profiles_Base_Orphaned.cpp#L44) | `"profileOrphaned"` | `ProfileGuid`, `ProfileSource` |
  | [`src\cascadia\TerminalSettingsEditor\Profiles_Terminal.cpp:27`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Profiles_Terminal.cpp#L27) | `"profile.terminal"` | `IsProfileDefaults`, `ProfileGuid`, `ProfileSource` |
  | [`src\cascadia\TerminalSettingsEditor\Rendering.cpp:23`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Rendering.cpp#L23) | `"rendering"` | — |

- **Fields (union across all variants):**

  | Name | Type | Description |
  |---|---|---|
  | `PageId` | Value (string) | The identifier of the page that was navigated to. |
  | `IsProfileDefaults` | Value (bool) | If the modified profile is the `profile.defaults` object. |
  | `ProfileGuid` | Value (GUID) | The guid of the profile that was navigated to. (For `Profiles_Base`, set to `{3ad42e7b-e073-5f3e-ac57-1c259ffa86a8}` if the `profiles.defaults` object is being modified.) |
  | `ProfileSource` | Value (wide string) | The source of the profile that was navigated to. |
  | `HasBackgroundImage` | Value (bool) | If the profile has a background image defined. |
  | `HasUnfocusedAppearance` | Value (bool) | If the profile has an unfocused appearance defined. |
  | `SchemeName` | Value (string) | The name of the color scheme that's being edited. |
  | `FragmentSource` | Value (wide string) | The source of the fragment included in this extension package. |
  | `FragmentCount` | Value (int) | The number of fragments included in this extension package. |
  | `Enabled` | Value (bool) | The enabled status of the extension. |
  | `ExtensionPackageCount` | Value (int) | The number of extension packages displayed. |
  | `ProfilesModifiedCount` | Value (int) | The number of profiles modified by enabled extensions. |
  | `ProfilesAddedCount` | Value (int) | The number of profiles added by enabled extensions. |
  | `ColorSchemesAddedCount` | Value (int) | The number of color schemes added by enabled extensions. |

### `OpenJson`
- **Description:** Event emitted when the user clicks the Open JSON button in the settings UI.
- **Source:** [`src\cascadia\TerminalSettingsEditor\MainPage.cpp:417`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/MainPage.cpp#L417)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `SettingsTarget` | Value (string) | `target == SettingsTarget::DefaultsFile ? "DefaultsFile" : "SettingsFile"` | The target settings file. |

### `ResetApplicationState`
- **Description:** Event emitted when the user resets their application state.
- **Source:** [`src\cascadia\TerminalSettingsEditor\Compatibility.cpp:29`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Compatibility.cpp#L29)
- **Fields:** _(none)_

### `ResetToDefaultSettings`
- **Description:** Event emitted when the user resets their settings to their default value.
- **Source:** [`src\cascadia\TerminalSettingsEditor\Compatibility.cpp:41`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/Compatibility.cpp#L41)
- **Fields:** _(none)_

---

## Provider: Microsoft.Windows.Terminal.Setting.Model

- **Symbol:** `g_hSettingsModelProvider`
- **GUID:** `{be579944-4d33-5202-e5d6-a7a57f1935cb}`
- **Defined in:** [`src\cascadia\TerminalSettingsModel\init.cpp`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsModel/init.cpp)
- **Privacy tag (all events):** `PDT_ProductAndServiceUsage`
- **Keyword (all events):** `MICROSOFT_KEYWORD_MEASURES`

### `DefaultTerminalChanged`
- **Description:** _(no `TraceLoggingDescription` supplied.)_
- **Source:** [`src\cascadia\TerminalSettingsModel\DefaultTerminal.cpp:102`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsModel/DefaultTerminal.cpp#L102)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `TerminalName` | WideString | `term.Name().c_str()` | The name of the default terminal. |
  | `TerminalVersion` | WideString | `term.Version().c_str()` | The version of the default terminal. |
  | `TerminalAuthor` | WideString | `term.Author().c_str()` | The author of the default terminal. |

### `JsonSettingsChanged`
- **Description:** Event emitted when `settings.json` change[s].
- **Source:** [`src\cascadia\TerminalSettingsModel\CascadiaSettingsSerialization.cpp:1930`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsModel/CascadiaSettingsSerialization.cpp#L1930)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `Setting` | Value (string) | `change.data()` | — |
  | `Branding` | Value | `branding` (build branding) | — |
  | `Distribution` | Value | `distribution` (build distribution) | — |

### `MarksProfilesUsage`
- **Description:** Event emitted upon settings load, containing the number of profiles opted-in to scrollbar marks.
- **Source:** [`src\cascadia\TerminalSettingsModel\CascadiaSettingsSerialization.cpp:1378`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsModel/CascadiaSettingsSerialization.cpp#L1378)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `NumberOfAutoMarkPromptsProfiles` | Int32 | `totalAutoMark` | Number of profiles for which `AutoMarkPrompts` is enabled. |
  | `NumberOfShowMarksProfiles` | Int32 | `totalShowMarks` | Number of profiles for which `ShowMarks` is enabled. |

### `SendInputUsage`
- **Description:** Event emitted upon settings load, containing the number of `sendInput` actions a user has.
- **Source:** [`src\cascadia\TerminalSettingsModel\CascadiaSettingsSerialization.cpp:1361`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsModel/CascadiaSettingsSerialization.cpp#L1361)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `NumberOfSendInputActions` | Int32 | `collectSendInput()` | Number of `sendInput` actions in the user's settings. |

### `ThemesInUse`
- **Description:** Data about the themes in use.
- **Source:** [`src\cascadia\TerminalSettingsModel\CascadiaSettingsSerialization.cpp:1337`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsModel/CascadiaSettingsSerialization.cpp#L1337)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `ThemeClass` | Int32 | `themeChoice` | Identifier for the theme chosen. `0` = system (legacySystem = 6), `1` = light (legacyLight = 5), `2` = dark (legacyDark = 4), `3` = any custom theme. |
  | `ChangedTheme` | Bool | `changedTheme` | True if the user actually changed the theme from the default theme. |
  | `NumberOfThemes` | Int32 | `numThemes` | Number of themes in the user's settings. |

### `UISettingsChanged`
- **Description:** Event emitted when settings change via the UI.
- **Source:** [`src\cascadia\TerminalSettingsModel\CascadiaSettingsSerialization.cpp:1941`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsModel/CascadiaSettingsSerialization.cpp#L1941)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `Setting` | Value (string) | `change.data()` | — |
  | `Branding` | Value | `branding` | — |
  | `Distribution` | Value | `distribution` | — |

---

## Provider: Microsoft.Windows.Terminal.Connection

- **Symbol:** `g_hTerminalConnectionProvider`
- **GUID:** `{e912fe7b-eeb6-52a5-c628-abe388e5f792}`
- **Defined in:** [`src\cascadia\TerminalConnection\init.cpp`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalConnection/init.cpp)
- **Keyword (all events):** `MICROSOFT_KEYWORD_MEASURES`

### `ConPtyConnected`
- **Description:** Event emitted when ConPTY connection is started.
- **Source:** [`src\cascadia\TerminalConnection\ConptyConnection.cpp:185`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalConnection/ConptyConnection.cpp#L185)
- **Privacy tag:** `PDT_ProductAndServiceUsage`
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `SessionGuid` | Guid | `_sessionId` | The `WT_SESSION`'s GUID. |
  | `Client` | WideString | `_clientName.c_str()` | The attached client process. |

### `ConPtyConnectedToDefterm`
- **Description:** Event emitted when ConPTY connection is started, for a defterm session.
- **Source:** [`src\cascadia\TerminalConnection\ConptyConnection.cpp:433`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalConnection/ConptyConnection.cpp#L433)
- **Privacy tag:** `PDT_ProductAndServiceUsage`
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `SessionGuid` | Guid | `_sessionId` | The `WT_SESSION`'s GUID. |
  | `Client` | WideString | `_clientName.c_str()` | The attached client process. |

### `ReceivedFirstByte`
- **Description:** An event emitted when the connection receives the first byte.
- **Source:** [`src\cascadia\TerminalConnection\ConptyConnection.cpp:785`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalConnection/ConptyConnection.cpp#L785)
- **Privacy tag:** `PDT_ProductAndServicePerformance` _(differs from the rest of this provider)_
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `SessionGuid` | Guid | `_sessionId` | The `WT_SESSION`'s GUID. |
  | `Duration` | Float64 | `delta.count()` | — |

### `ReceiveTerminalHandoff_Success`
- **Description:** Successfully received a terminal handoff.
- **Source:** [`src\cascadia\TerminalConnection\CTerminalHandoff.cpp:93`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalConnection/CTerminalHandoff.cpp#L93)
- **Privacy tag:** `PDT_ProductAndServiceUsage`
- **Fields:** _(none)_

---

## Provider: Microsoft.Terminal.Core

- **Symbol:** `g_hCTerminalCoreProvider`
- **GUID:** `{103ac8cf-97d2-51aa-b3ba-5ffd5528fa5f}`
- **Defined in:** [`src\cascadia\TerminalCore\TerminalApi.cpp`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalCore/TerminalApi.cpp)
- **Privacy tag (all events):** `PDT_ProductAndServiceUsage`
- **Keyword (all events):** `MICROSOFT_KEYWORD_MEASURES`

> **Registered by:** [`src\cascadia\TerminalControl\init.cpp`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalControl/init.cpp) (alongside `g_hTerminalControlProvider`).

### `ShellIntegrationWorkingDirSet`
- **Description:** The CWD was set by the client application.
- **Source:** [`src\cascadia\TerminalCore\TerminalApi.cpp:224`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalCore/TerminalApi.cpp#L224)
- **Fields:** _(none)_

---

## Provider: Microsoft.Windows.Terminal.Win32Host

- **Symbol:** `g_hWindowsTerminalProvider`
- **GUID:** `{56c06166-2e2e-5f4d-7ff3-74f4b78c87d6}`
- **Defined in:** [`src\cascadia\WindowsTerminal\main.cpp`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/WindowsTerminal/main.cpp)
- **Privacy tag (all events):** `PDT_ProductAndServiceUsage`
- **Keyword (all events):** `MICROSOFT_KEYWORD_MEASURES`

### `ExeCreated`
- **Description:** Event emitted when the terminal process is started.
- **Source:** [`src\cascadia\WindowsTerminal\main.cpp:90`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/WindowsTerminal/main.cpp#L90)
- **Fields:** _(none)_

### `SessionBecameInteractive`
- **Description:** Event emitted when the session was interacted with.
- **Source:** [`src\cascadia\WindowsTerminal\WindowEmperor.cpp:602`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/WindowsTerminal/WindowEmperor.cpp#L602)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `Branding` | Value | `branding` | — |
  | `Distribution` | Value | `distribution` | — |

---

## Provider: Microsoft.Windows.Console.Host

- **Symbol:** `g_hConhostV2EventTraceProvider`
- **GUID:** `{fe1ff234-1f09-50a8-d38d-c44fab43e818}`
- **Defined in:** [`src\host\tracing.cpp`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/host/tracing.cpp)
- **Privacy tag (all events):** `PDT_ProductAndServiceUsage`
- **Keyword (all events):** `MICROSOFT_KEYWORD_MEASURES`

> Most events written to this provider are diagnostic ETW traces (tagged with `TIL_KEYWORD_TRACE`) and are out of scope. Only the three telemetry-keyword events below are listed.

### `ConsoleHandoffFailed`
- **Description:** Failed while attempting handoff.
- **Source:** [`src\server\IoDispatchers.cpp:385`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/server/IoDispatchers.cpp#L385)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `handoffCLSID` | Guid | `Globals.delegationPair.console` | — |
  | _(unnamed)_ | HResult | `hr` | — |

### `ConsoleHandoffSessionStarted`
- **Description:** A new interactive console session was started.
- **Source:** [`src\server\IoDispatchers.cpp:275`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/server/IoDispatchers.cpp#L275)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `handoffCLSID` | Guid | `Globals.delegationPair.console` | — |
  | `handoffTargetChosenByWindows` | Bool | `handoffTargetChosenByWindows` | — |

### `ConsoleHandoffSucceeded`
- **Description:** Successfully handed off console connection.
- **Source:** [`src\server\IoDispatchers.cpp:365`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/server/IoDispatchers.cpp#L365)
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `handoffCLSID` | Guid | `Globals.delegationPair.console` | — |

---

## Provider: Microsoft.Windows.Console.Launcher

- **Symbol:** `g_ConhostLauncherProvider`
- **GUID:** `{770aa552-671a-5e97-579b-151709ec0dbd}`
- **Defined in:** [`src\host\exe\exemain.cpp`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/host/exe/exemain.cpp)
- **Privacy tag (all events):** `PDT_ProductAndServiceUsage`

### `IsLegacyLoaded`
- **Description:** _(no `TraceLoggingDescription` supplied.)_ Indicates that the legacy `ConhostV1.dll` console host was loaded by the launcher.
- **Source:** [`src\host\exe\exemain.cpp:150`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/host/exe/exemain.cpp#L150)
- **Keyword:** `MICROSOFT_KEYWORD_TELEMETRY` _(differs from the other providers, which all use `MICROSOFT_KEYWORD_MEASURES`)_
- **Fields:**

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `ConsoleLegacy` | Bool | `true` (literal) | — |

---

## Cross-provider: WIL fallback failure event

The helper `Microsoft::Console::ErrorReporting::EnableFallbackFailureReporting(<provider>)` (see [`src\inc\WilErrorReporting.h`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/inc/WilErrorReporting.h)) installs a WIL fallback that emits a single named telemetry event whenever a WIL `THROW_…` / `LOG_…` macro reports a failure that hasn't already been logged. The event is written to **whichever provider was most recently passed to `EnableFallbackFailureReporting`** in the current module.

Each Terminal DLL/EXE in this list passes its own provider, so this event can appear under any of them:
- `Microsoft.Windows.Terminal.App` ([`src\cascadia\TerminalApp\init.cpp:22`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalApp/init.cpp#L22))
- `Microsoft.Windows.Terminal.Settings.Editor` ([`src\cascadia\TerminalSettingsEditor\init.cpp:23`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsEditor/init.cpp#L23))
- `Microsoft.Windows.Terminal.Setting.Model` ([`src\cascadia\TerminalSettingsModel\init.cpp:22`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalSettingsModel/init.cpp#L22))
- `Microsoft.Windows.Terminal.Connection` ([`src\cascadia\TerminalConnection\init.cpp:24`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalConnection/init.cpp#L24))
- `Microsoft.Windows.Terminal.Control` ([`src\cascadia\TerminalControl\init.cpp:26`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalControl/init.cpp#L26))

### `FallbackError`
- **Description:** WIL-reported failure that was not already reported elsewhere. (HRESULT `0x80131515` — XAML accessibility — is filtered out.)
- **Source:** [`src\inc\WilErrorReporting.h:32`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/inc/WilErrorReporting.h#L32)
- **Privacy tag:** `PDT_ProductAndServicePerformance`
- **Keyword:** `MICROSOFT_KEYWORD_TELEMETRY`
- **Level:** `WINEVENT_LEVEL_ERROR`
- **Fields** (all wrapped inside a `TraceLoggingStruct(14, "wilResult")`):

  | Name | Type | Source expression | Description |
  |---|---|---|---|
  | `hresult` | UInt32 | `failure.hr` | Failure error code. |
  | `fileName` | String | `failure.pszFile` | Source code file name where the error occurred. |
  | `lineNumber` | UInt32 | `failure.uLineNumber` | Line number within the source code file where the error occurred. |
  | `module` | String | `failure.pszModule` | Name of the binary where the error occurred. |
  | `failureType` | UInt32 | `static_cast<DWORD>(failure.type)` | Indicates what type of failure was observed (exception, returned error, logged error or fail fast). |
  | `message` | WideString | `failure.pszMessage` | Custom message associated with the failure (if any). |
  | `threadId` | UInt32 | `failure.threadId` | Identifier of the thread the error occurred on. |
  | `callContext` | String | `failure.pszCallContext` | List of telemetry activities containing this error. |
  | `originatingContextId` | UInt32 | `failure.callContextOriginating.contextId` | Identifier for the oldest telemetry activity containing this error. |
  | `originatingContextName` | String | `failure.callContextOriginating.contextName` | Name of the oldest telemetry activity containing this error. |
  | `originatingContextMessage` | WideString | `failure.callContextOriginating.contextMessage` | Custom message associated with the oldest telemetry activity containing this error (if any). |
  | `currentContextId` | UInt32 | `failure.callContextCurrent.contextId` | Identifier for the newest telemetry activity containing this error. |
  | `currentContextName` | String | `failure.callContextCurrent.contextName` | Name of the newest telemetry activity containing this error. |
  | `currentContextMessage` | WideString | `failure.callContextCurrent.contextMessage` | Custom message associated with the newest telemetry activity containing this error (if any). |

---

## Providers with no telemetry events

These providers are defined and registered but emit no telemetry-keyword events. They are listed here for completeness; their (diagnostic) events are out of scope.

| Provider | GUID | Defined in |
|---|---|---|
| `g_hTerminalControlProvider` (`Microsoft.Windows.Terminal.Control`) | `{28c82e50-57af-5a86-c25b-e39cd990032b}` | [`src\cascadia\TerminalControl\init.cpp`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/cascadia/TerminalControl/init.cpp) |
| `g_UiaProviderTraceProvider` (`Microsoft.Windows.Console.UIA`) | `{e7ebce59-2161-572d-b263-2f16a6afb9e5}` | [`src\types\UiaTracing.cpp`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/types/UiaTracing.cpp) |
| `g_hConsoleVirtTermParserEventTraceProvider` (`Microsoft.Windows.Console.VirtualTerminal.Parser`) | `{c9ba2a84-d3ca-5e19-2bd6-776a0910cb9d}` | [`src\terminal\parser\tracing.cpp`](https://github.com/microsoft/terminal/blob/fb71a0462edaf32a7ac4a5ebb4df3bd05bacb41e/src/terminal/parser/tracing.cpp) |

> `g_hConhostV2EventTraceProvider` is also used for many diagnostic events; only its telemetry-keyword events are listed above. `g_hTerminalControlProvider` has no `TraceLoggingWrite` calls at all in the source tree but receives the cross-provider `FallbackError` event when its DLL is the most recent caller of `EnableFallbackFailureReporting`.
