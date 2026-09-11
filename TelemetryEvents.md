# Intelligent Terminal telemetry inventory

This document inventories Intelligent Terminal's event definitions and emission
paths, followed by the inherited Windows Terminal / OpenConsole event reference.
The Intelligent Terminal section was reviewed against the working-tree source
on **2026-09-11**. It describes instrumentation, not verified ingestion from a
released build. See [PRIVACY.md](./PRIVACY.md) for privacy information and Windows
diagnostic data controls.

## Scope and collection status

Count events by **provider + event name**, not event name alone:

| Category | Provider | Event definitions |
|---|---|---|
| Dedicated usage telemetry | `Microsoft.Windows.Terminal.App` | 4 |
| Dedicated settings census | `Microsoft.Windows.Terminal.Setting.Model` | 3 |
| Dedicated usage/performance telemetry | `Microsoft.Windows.Terminal.WTA` | 14 |
| Additional ETW diagnostics without an explicit telemetry keyword | `Microsoft.Windows.Terminal.Settings.Editor` | 3 |
| **Total dedicated instrumentation** | **21 keyword-tagged events + 3 additional ETW events** | **24** |

All 24 definitions have production call sites. `ErrorDetected` and
`DelegateInvoked` each occur under both App and WTA, with different meanings.
The inherited `ActionDispatched`, `JsonSettingsChanged`, and `UISettingsChanged`
events also cover Intelligent Terminal functionality; they are not counted as
new event definitions.

**An emission call does not prove upload or backend availability.** The 21
dedicated telemetry events explicitly use `MICROSOFT_KEYWORD_MEASURES`. Actual
collection depends on the build's telemetry definitions, provider/session
enablement, sampling, and Windows diagnostic data settings. WTA registers its
provider in [main.rs](./tools/wta/src/main.rs); [build.rs](./tools/wta/build.rs)
generates its provider metadata from the official telemetry header overlay when
available, otherwise from the [OSS stub](./dep/telemetry/ProjectTelemetry.h).
The stub's measures/privacy constants are zero and do not configure the
production Microsoft collection path; local ETW tracing is a separate concern.

The three Settings Editor probe events have no explicit measures keyword.
They are listed separately rather than counted as keyword-tagged telemetry.
Neither the presence of a privacy tag nor the absence of a keyword proves
whether a particular ETW collector will capture them. Backend inclusion must
be established separately.

## Intelligent Terminal-specific events

The dedicated event payloads below do not include prompts, responses, terminal
contents, API keys, credential identifiers, custom endpoint URLs, model
identifiers, custom agent names, or custom agent command lines. This statement
describes these event schemas, not the contents of local diagnostic logs or
all inherited upstream events.

Common field conventions:

| Field / metadata | Meaning |
|---|---|
| `AgentId`, `ProviderId` | The WTA/settings-census allowlist is `copilot`, `claude`, `codex`, `gemini`, `opencode`; everything else becomes `custom`. Settings Editor probe events instead replace `custom:` IDs with `custom` and otherwise pass through the selected ID. |
| `Branding` | Numeric build branding: `0` = other/development, `1` = Canary, `2` = Preview, `3` = Release. Only present on events whose schema lists it. |
| `Distribution` | Numeric packaging mode: `0` = other/unpackaged, `1` = portable, `2` = packaged. It does not distinguish Store from other packaged installations. |
| `SessionId` | ACP session identifier, not the terminal's `WT_SESSION`. Failed `session/new` attempts use an empty string. |
| `PaneId` | Terminal pane identity used by WTA error/autofix events; not interchangeable with ACP `SessionId`. |
| `PartA_PrivTags` | Additional `UInt64` privacy metadata on every dedicated event, omitted from the business-field tables. Usage events use `PDT_ProductAndServiceUsage`; performance events use `PDT_ProductAndServicePerformance`. |

WTA strings use `str8`, booleans use `bool32`, duration/latency fields use `f64`
milliseconds, prompt/chunk lengths use `u32` bytes, `TotalResponseBytes` uses
`u64`, and `AcpErrorCode` uses `i32`. WTA durations are monotonic. These byte
lengths are not model token counts.

### Provider: Microsoft.Windows.Terminal.App

Provider definition: [TerminalApp/init.cpp](./src/cascadia/TerminalApp/init.cpp).
All four events use `MICROSOFT_KEYWORD_MEASURES`, usage privacy metadata, and
the default `Verbose` level.

| Event | Trigger | Fields |
|---|---|---|
| `AgentPaneOpened` | `_OpenOrReuseAgentPane` creates or restores a pane, or opens an existing pane into Sessions View. | `TriggerSource`, `Branding` |
| `CommandPaletteDispatchedAgentPrompt` | A nonempty foreground or background agent prompt is submitted through the Command Palette, before raising the corresponding request. | `IsBackgroundMode` |
| `DelegateInvoked` | Terminal successfully creates the `wta delegate` process. This does not establish successful delegation completion. | `TriggerSource` (`CommandPalette` or `Action`) |
| `ErrorDetected` | `OnAutofixStateChanged` receives `state="pending"`, before locating the owning tab/pane. It is not emitted for `state="detected"`. | `Branding` |

Current `AgentPaneOpened.TriggerSource` values are `Action`,
`SessionsAction`, `Autofix`, `FirstRunExperience`, `AgentSwitch`,
`BottomBarToggle`, `BottomBarSessions`, `SettingsReload`, and `FocusAction`.

`AgentPaneOpened` is not a census of all helper creation or all visibility
changes. Hidden pre-warm, toggle-to-stash, and focus-only paths do not emit
this event. Opening Sessions View on an already visible pane can emit it.

The background Command Palette handler is currently a no-op. Therefore
`CommandPaletteDispatchedAgentPrompt` with `IsBackgroundMode=true` measures a
submission, not an executed background task. The C++ `ErrorDetected` call
site has no first-occurrence/deduplication guard; do not count it as a unique
error per pane or sum it with WTA's error event.

Emission sources:
[TerminalPage.cpp](./src/cascadia/TerminalApp/TerminalPage.cpp)
(`_OpenOrReuseAgentPane`, `_LaunchDelegate`, `OnAutofixStateChanged`) and
[CommandPalette.cpp](./src/cascadia/TerminalApp/CommandPalette.cpp)
(`_dispatchAgentPrompt`).

### Provider: Microsoft.Windows.Terminal.Setting.Model

Provider definition:
[TerminalSettingsModel/init.cpp](./src/cascadia/TerminalSettingsModel/init.cpp).
All three events use `MICROSOFT_KEYWORD_MEASURES`, usage privacy metadata, and
the default `Verbose` level.

These census events run in `CascadiaSettings::LogSettingChanges(true)`, using
the loaded settings values including defaults, not just keys explicitly
present in JSON. They do not run in the `false` branch used when saving from
Settings UI. In non-Debug builds the function returns early if the provider is
not enabled for measures. Repeated loads can repeat these events; event count
is not a unique-device count.

| Event | Trigger | Fields and controlled values |
|---|---|---|
| `AgentProviderConfigured` | Once per nonempty ACP-agent setting and once per nonempty delegate-agent setting in each census invocation. | `ProviderType` (`AcpAgent` or `DelegateAgent`), `ProviderId`, `Branding`, `Distribution` |
| `CustomModelProviderConfigured` | Once for each configured BYOK/custom model provider, whether selected or not. | `HasApiKey` (a credential reference is configured, not proof that a usable key exists), `ApiKeyRequired`, `Branding`, `Distribution` |
| `IntelligentFeatureConfigured` | Once for each of the feature settings below; omit pane position if empty. | `FeatureName`, `FeatureValue`, `Branding`, `Distribution` |

`IntelligentFeatureConfigured` currently reports:

| `FeatureName` | Setting getter | `FeatureValue` |
|---|---|---|
| `AutoErrorDetection` | `AutoErrorDetectionEnabled()` | String `true` or `false` |
| `AutoFix` | `AutoFixEnabled()` | String `true` or `false` |
| `AgentPanePosition` | `AgentPanePosition()` | Nonempty configured position string, passed through without a telemetry allowlist |
| `QuotaUsage` | `ShowTokenUsageAndCost()` | String `true` or `false`; visibility of usage/cost UI, not consumed quota |
| `VerticalTabs` | `TabLayout() == Vertical` | String `true` or `false` |

**Configured is not necessarily runtime-effective.** The census calls
`AcpAgent()`, `DelegateAgent()`, and `AutoFixEnabled()`, not their policy-aware
`Effective*` equivalents. A configured agent or autofix value can differ from
what GPO or runtime gating permits. Custom provider events contain no provider,
endpoint, model, credential, or key value and cannot identify the selected model.

Sources:
[CascadiaSettingsSerialization.cpp](./src/cascadia/TerminalSettingsModel/CascadiaSettingsSerialization.cpp)
(`LogSettingChanges`),
[MTSMSettings.h](./src/cascadia/TerminalSettingsModel/MTSMSettings.h), and
[GlobalAppSettings.cpp](./src/cascadia/TerminalSettingsModel/GlobalAppSettings.cpp)
(`EffectiveAcpAgent`, `EffectiveDelegateAgent`, `EffectiveAutoFixEnabled`).

### Provider: Microsoft.Windows.Terminal.WTA

- **GUID:** `{4cfcff80-4e6b-5bfd-8ea1-d38e1226f70b}`
- **Keyword:** `MICROSOFT_KEYWORD_MEASURES`
- **Level:** `Verbose`
- **Definitions:** [telemetry.rs](./tools/wta/src/telemetry.rs), `log_*` wrappers.
  Every business field in each wrapper is listed below.

#### ACP lifecycle and performance

All three events in this table use performance privacy metadata.

| Event | Trigger | Fields |
|---|---|---|
| `AcpInitializeComplete` | An ACP `initialize` attempt completes or times out. | `DurationMs`, `Success`, `Route`, `FailureKind`, `AcpErrorCode` |
| `AcpNewSessionComplete` | An ACP `session/new` attempt completes or times out. | `SessionId` (empty on failure), `DurationMs`, `Success`, `Route`, `FailureKind`, `AcpErrorCode` |
| `AgentColdStartComplete` | A newly spawned agent process finishes or fails its ACP initialization. Warm process-pool reuse does not emit this event. | `AgentId`, `Source`, `DurationMs`, `Success`, `FailureKind` |

Current production routes:

| Event | `Route` values |
|---|---|
| `AcpInitializeComplete` | `HelperPipe`, `Probe`, `SessionsCli` |
| `AcpNewSessionComplete` | `MasterForward`, `Probe`, `HelperPipeStartup`, `HelperPipeFallback`, `HelperPipeNewSessionForTab`, `LazyCreateOnFirstPrompt` |

`HelperPipe` and `SessionsCli` initialize the connection to master, not a
standalone agent CLI. `Probe` measures the probe's agent connection. Master
records agent-process startup separately as `AgentColdStartComplete`.
`MasterForward` and a helper-side `session/new` event can describe two hops of
the same logical session creation; group/filter by route instead of adding
them as independent user sessions.

`FailureKind` is empty on success. ACP failures use `AcpError` or `Timeout`;
`AcpErrorCode` is the ACP error's numeric code for `AcpError`, otherwise `0`.
Helper-side errors propagated from master can be classified as `AcpError`
even when the underlying master attempt timed out. Cold-start failures use
`SpawnFailed`, `InitializeFailed`, or `Timeout`, with `Source=Host` or `Wsl`.
Cold-start duration covers process spawn and initialization, not first-response
latency or the entire pane-opening experience.

Emission sources:
[client.rs](./tools/wta/src/protocol/acp/client.rs),
[probe.rs](./tools/wta/src/protocol/acp/probe.rs),
[cli/sessions.rs](./tools/wta/src/cli/sessions.rs), and
[master/mod.rs](./tools/wta/src/master/mod.rs).

#### Agent turns and autofix

| Event | Trigger | Fields | Privacy category |
|---|---|---|---|
| `AgentPromptSent` | WTA starts dispatching a prompt over ACP, including manual and automatic autofix prompts. | `SessionId`, `PromptLengthBytes`, `IsAutofix`, `IsByok`, `AgentId`, `TemplateKind`, `Route` (`AcpDispatch`) | Usage |
| `AgentResponseFirstToken` | The first observed user-visible text chunk arrives, including an ephemeral thought chunk if it arrives first. | `SessionId`, `FirstTokenLatencyMs`, `ChunkLengthBytes`, `AgentId` | Performance |
| `AgentResponseComplete` | A tracked prompt completes and has a dispatch-time monotonic timing anchor. | `SessionId`, `TotalDurationMs`, `TotalResponseBytes`, `Success`, `IsByok`, `AgentId` | Performance |
| `ErrorDetected` | An unacknowledged classification is `Critical` or `Actionable`, after the helper's cross-tab filter. | `Severity`, `Method`, `PaneId` | Usage |
| `ErrorFixResolved` | An armed autofix pane observes exit-zero **or an effective prompt-start** and still has an `armed_at` timestamp. | `PaneId`, `TimeSinceFixMs`, `AgentId` | Usage |

Interpretation:

- `TemplateKind` is `Planner`, `Autofix`, or `AgentCommand`.
  `PromptLengthBytes` measures the constructed outgoing prompt text, which may
  include template/context text, not just the user's typed input.
- `FirstTokenLatencyMs` and `TotalDurationMs` start at ACP dispatch, not user
  submission or pane opening. Missing timing anchors suppress these events;
  a turn without text need not emit `AgentResponseFirstToken`.
- `IsByok` and response-event `AgentId` are captured for the specific prompt.
  `Success` describes the prompt operation, not answer quality or task success.
- `TotalResponseBytes` is **currently always zero**. The master/helper
  migration removed the stdout-read instrumentation; this field is retained
  for schema compatibility, not a working measure of answer size.
- WTA `ErrorDetected.Severity` is `Critical` or `Actionable`; `Method` is the
  classified WT event method, such as `connection_state` or `vt_sequence`.
  These are different signals from App's `state="pending"` event.
- `ErrorFixResolved` is a **state-clearing proxy, not verified fix success**.
  Its implementation accepts `osc:133;D;0` or a new `osc:133;A` prompt after
  excluding the triggering prompt echo. `TimeSinceFixMs` measures time from
  arming, not from successful execution of an agent-recommended command.
  `AgentId` comes from the current agent at resolution time.

Emission sources:
[client.rs](./tools/wta/src/protocol/acp/client.rs) (prompt dispatch),
[turn_metrics.rs](./tools/wta/src/protocol/acp/turn_metrics.rs)
(`observe_first_text`, `complete`), and
[app_events.rs](./tools/wta/src/app_events.rs) (WT event/autofix handling).
The arming timestamp is set in [app/autofix.rs](./tools/wta/src/app/autofix.rs).

#### Commands, sessions, delegation, MCP, and hooks

All six events in this table use usage privacy metadata.

| Event | Trigger | Fields and controlled values |
|---|---|---|
| `SlashCommandInvoked` | WTA enters dispatch for a registered slash command, before command-specific guards. Not proof the action succeeded. | `CommandName`: `help`, `clear`, `new`, `fix`, `restart`, `stop`, `sessions`, `agent`, `model`, `config`, `move` |
| `SessionsViewOpened` | `open_agents_view_for_tab` is called. | No business fields |
| `SessionResumeInvoked` | Sessions View selects a resume action, before executing it. Focus-only and non-resumable actions do not emit it. | `Route` (`AgentPane` for the ACP load route, `Cli` for provider-native CLI resume), `AgentId` |
| `DelegateInvoked` | The coordinator's open-and-send operation creates a target pane with a resolved delegate runtime. It precedes completion of prompt delivery/task execution. | `TriggerSource` (`Agent`) |
| `SessionMcpToolCalled` | Master receives a `tools/call` request past the HTTP/capability checks, before tool dispatch/validation completes. | `ToolName`: legacy allowlist `terminal_send`, `terminal_open`, `terminal_open_and_send`, `request_user_input`; anything else becomes `unknown` |
| `HookOperationCompleted` | The CLI hook install/uninstall report is produced, once per CLI in that report, including skipped operations. | `Operation` (`Install`, `Uninstall`), `Cli` (`copilot`, `claude`, `gemini`, `codex`, `opencode`), `Outcome` |

Install outcomes are `installed`, `skipped`, or `failed`; uninstall outcomes
are `succeeded`, `skipped`, or `failed`. `hooks status` does not emit
`HookOperationCompleted`. These events do not measure hook listener health or
the delivery of individual session-status updates.

**Current MCP name mismatch:** the advertised tools have been renamed, but
the telemetry allowlist has not:

| Actual session MCP tool | Currently emitted `ToolName` |
|---|---|
| `run_command_in_current_shell` | `unknown` |
| `create_workspace` | `unknown` |
| `delegate_task_in_new_workspace` | `unknown` |
| `request_user_input` | `request_user_input` |

The old allowlisted names are not the currently advertised tool API. There is
no success/result field, so this event counts received calls, including calls
that later fail, not completed terminal mutations.

Emission sources:
[app.rs](./tools/wta/src/app.rs) (slash dispatch and Sessions View),
[commands.rs](./tools/wta/src/commands.rs) (slash registry),
[coordinator.rs](./tools/wta/src/coordinator.rs),
[master/session_mcp.rs](./tools/wta/src/master/session_mcp.rs), and
[cli/hooks.rs](./tools/wta/src/cli/hooks.rs).
Current tool names come from
[action_proposal/schema.rs](./tools/wta/src/agent_tools/action_proposal/schema.rs)
and [agent_tools/session_mcp.rs](./tools/wta/src/agent_tools/session_mcp.rs).

### Additional ETW: Microsoft.Windows.Terminal.Settings.Editor

Provider definition:
[TerminalSettingsEditor/init.cpp](./src/cascadia/TerminalSettingsEditor/init.cpp).
These three events use `Info` level and performance privacy metadata, but no
explicit `TraceLoggingKeyword`. Their collection status is distinct from the
21 measures-tagged events above.

| Event | Trigger | Fields |
|---|---|---|
| `AcpModelProbeStarted` | Settings UI starts a clean ACP model catalog probe. | `AgentId` (wide string), `CacheRevision` (`UInt64`) |
| `AcpModelProbeDiscarded` | A completed probe belongs to an older generation and its result is discarded. This path does not also emit `AcpModelProbeCompleted`. | `AgentId` (wide string) |
| `AcpModelProbeCompleted` | The current-generation probe returns to the UI thread. | `AgentId` (wide string), `Succeeded` (`Bool`), `ModelCount` (`UInt32`) |

`Succeeded` requires a parsed catalog with at least one model. These events
have no duration or probe-correlation field shared across all three schemas.
`CacheRevision` appears only on the started event. Custom-prefixed agent IDs
are bucketed to `custom`; no model identifiers are included.

Source:
[AIAgentsViewModel.cpp](./src/cascadia/TerminalSettingsEditor/AIAgentsViewModel.cpp)
(`_TriggerAcpModelProbe`, `_RunAcpModelProbeAsync`).

### Inherited events that also cover Intelligent Terminal

These are additional coverage, not new dedicated event definitions:

| Event | Provider suffix | Intelligent Terminal coverage | Fields |
|---|---|---|---|
| `ActionDispatched` | `App` | Any action dispatched through `ShortcutActionDispatch` whose handler marks it handled, including registered AI actions. | `Action` (integer enum), `Branding` |
| `JsonSettingsChanged` | `Setting.Model` | Setting keys gathered from the JSON-load change log, not their values. | `Setting`, `Branding`, `Distribution` |
| `UISettingsChanged` | `Setting.Model` | Setting keys gathered when saving Settings UI, not their values. | `Setting`, `Branding`, `Distribution` |

AI actions include `OpenAgentPane`, `FocusAgentPane`, `OpenAgentSessions`,
`TriggerAutofix`, and `OpenBackgroundAgent`. A handled action does not imply
that an asynchronous agent operation completed successfully. Decode the
integer against the matching build's action enum.

The global AI settings participating in the generic change-log mechanism
include the following keys. `Setting` uses a context prefix, for example
`global.acpAgent`; it never substitutes the configured value for the key.

| Area | Global JSON keys |
|---|---|
| ACP and custom models | `acpAgent`, `acpModel`, `customModelSelection`, `customModelProviders`, `acpCustomCommand`, `acpCustomCommands` |
| Delegation | `delegateAgent`, `delegateModel`, `delegateCustomCommand`, `delegateCustomCommands` |
| Features and pane behavior | `autoErrorDetectionEnabled`, `autoFixEnabled`, `agentSessionManagementEnabled`, `showTokenUsageAndCost`, `agentPanePosition`, `agentPane.yoloMode` |
| Coordinator | `aiIntegration.coordinator.enabled`, `aiIntegration.coordinator.commandline`, `aiIntegration.coordinator.profile` |
| Confirmation | `aiIntegration.confirmation.readOperations`, `aiIntegration.confirmation.createOperations`, `aiIntegration.confirmation.inputOperations` |

Generic change events are not a current-value census of these settings.
Defaults and loaded settings are covered separately by the dedicated census
only for the fields listed earlier.

Sources:
[ShortcutActionDispatch.cpp](./src/cascadia/TerminalApp/ShortcutActionDispatch.cpp),
[AllShortcutActions.h](./src/cascadia/TerminalSettingsModel/AllShortcutActions.h),
[CascadiaSettingsSerialization.cpp](./src/cascadia/TerminalSettingsModel/CascadiaSettingsSerialization.cpp),
[GlobalAppSettings.cpp](./src/cascadia/TerminalSettingsModel/GlobalAppSettings.cpp),
and [MTSMSettings.h](./src/cascadia/TerminalSettingsModel/MTSMSettings.h).

### Known limitations and query cautions

| Area | Current limitation |
|---|---|
| MCP tool adoption | Three current tool names collapse to `unknown`; the event has no completion/outcome field. |
| Model catalog probes | Three ETW events lack an explicit telemetry keyword; production collection is not established by this inventory. |
| Response volume | `TotalResponseBytes` is unpopulated (zero), not a valid answer-size metric. |
| Autofix effectiveness | `ErrorFixResolved` can mean prompt-start/state clearing, not a successful fix. App and WTA error events have different triggers and no shared incident ID. |
| Session/turn funnels | Helper and master can emit for the same logical session creation. Turn events have an ACP `SessionId` but no exported per-turn ID; session-only joins can mix multiple turns. |
| Resume success | `SessionResumeInvoked` counts Sessions View dispatch decisions, not successful resume, focus-only activation, or every saved-layout restore. |
| Runtime configuration | Census values include defaults but are not policy-aware runtime values; only five features have dedicated value census. |
| Usage/cost | `QuotaUsage` reports whether the UI is enabled; no dedicated event here reports consumed quota, model tokens, monetary cost, or model identity. |
| Pane lifecycle | No dedicated close/stash or complete visible-duration event. `AgentPaneOpened` is not a count of all helper starts. |
| Background delegation | The palette submission event can fire even though its background handler is a no-op. |
| Hook health | Install/uninstall outcomes do not report listener availability or status-update delivery. |

Local `tracing` output, startup timing, ACP debug logs, and protocol/session
notifications are not additional telemetry events merely because they contain
timings or state changes. This inventory does not validate a deployed ETW
session, ingestion pipeline, dashboard, or retention policy.

## Inherited Windows Terminal / OpenConsole reference

The reference below is the inherited upstream inventory, with source links
pinned to the upstream commit shown below. It is retained for context, not
presented as a fresh audit of every inherited call site in this fork.

Its scope is `TraceLoggingWrite(...)` calls under `src\` that explicitly use
one of the Microsoft telemetry keywords (`MICROSOFT_KEYWORD_MEASURES`,
`MICROSOFT_KEYWORD_TELEMETRY`, or `MICROSOFT_KEYWORD_CRITICAL_DATA`). Such tags
indicate intended telemetry classification, not proof that an event was
uploaded to Microsoft.

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
