# Intelligent Terminal telemetry reference

This document defines the telemetry emitted by Intelligent Terminal's AI
integration: what each event measures, when it is emitted, its complete
business payload, and the limits on interpreting that payload.

The scope is **27 event definitions**: 7 App, 16 WTA, 1 Settings Model,
and 3 Settings Editor. This includes the existing `AppCreated` event,
extended with the startup configuration snapshot.
An event is identified by **provider name plus event name**, not by event
name alone. In particular, App and WTA each define their own `ErrorDetected`
and `DelegateInvoked`.

This is a source contract, not a statement that every installed release
contains these definitions. Deployment and backend ingestion must be
established separately. See [privacy information](../PRIVACY.md).

## Contents

- [Measurement guide](#measurement-guide)
- [Providers and common metadata](#providers-and-common-metadata)
- [Event catalog](#event-catalog)
- [App event schemas](#app-event-schemas)
- [WTA event schemas](#wta-event-schemas)
- [Settings Model event schemas](#settings-model-event-schemas)
- [Settings Editor event schemas](#settings-editor-event-schemas)
- [Counting and correlation](#counting-and-correlation)
- [Retired events and settings filtering](#retired-events-and-settings-filtering)
- [Privacy and collection boundaries](#privacy-and-collection-boundaries)
- [Source references](#source-references)

## Measurement guide

| Question | Event or fields | Measurement boundary |
|---|---|---|
| How many successful agent session starts or loads occur? | App `AgentSessionStarted`, split by `StartKind` | Includes pre-warmed sessions, not just sessions with a user prompt |
| Which agents and configurations are used at session start? | App `AgentSessionStarted` settings snapshot | Session-weighted configuration, not installation or user adoption |
| How often is the assistant opened through an instrumented UI entry point? | App `AgentPaneOpened`, grouped by `TriggerSource` | Not every pane creation or restoration path |
| How often is foreground agent prompt mode entered or submitted? | App `CommandPaletteAgentPromptEntered` and `CommandPaletteDispatchedAgentPrompt` | Entry and submission are separate boundaries; neither proves task completion |
| Is the sidebar enabled at window creation? | App `AppCreated.SidebarEnabled` | Vertical tab layout at window creation, not a session-weighted snapshot |
| Which providers are configured at startup or changed later? | App `AppCreated` snapshot and Model `AgentProviderChanged` | Configuration, not CLI installation, authentication, or successful session use |
| How many custom agents are configured under policy? | App `AppCreated` custom-agent inventory fields | Both roles in the same window-created snapshot, including unused entries and zero counts; no commands or custom names |
| How often are prompts dispatched? | WTA `AgentPromptSent`, grouped by `AgentId`, `IsAutofix`, `IsByok`, `TemplateKind` | ACP prompt dispatches; Command Palette delegation is a separate path |
| How responsive are agent turns? | WTA `AgentResponseFirstToken` and `AgentResponseComplete` | Dispatch-to-first-counted-text and dispatch-to-RPC-completion durations |
| How reliable and fast are ACP operations? | WTA `AcpInitializeComplete`, `AcpNewSessionComplete`, `AcpLoadSessionComplete` | RPC outcomes; initialize/new timings must be separated by `Route` |
| What does starting a cold agent process cost? | WTA `AgentColdStartComplete` | Master process-pool startup, excluding warm reuse |
| How often are terminal errors classified? | WTA `ErrorDetected`, grouped by `Severity`, `Method`, `AllowAutoFixPolicy`, `AutoFixEnabled` | Classifier signals, not unique incidents or fixes |
| Are concrete repair offers accepted? | WTA `ErrorFixOffered` and `ErrorFixAccepted`, joined by `OfferId` | Presented autofix cards and confirmed Run requests queued for execution, not execution success |
| How often are commands, session views, or resume routes used? | WTA `AgentSlashCommandUsed`, `SessionsViewOpened`, `SessionResumeInvoked` | Entry or dispatch counts, not completion counts |
| How often is delegation requested? | App and WTA `DelegateInvoked`, kept separate | Different launch boundaries; neither measures task completion |
| Which session MCP tools are requested? | WTA `SessionMcpToolCalled`, grouped by `ToolName` | Calls reaching dispatch, not approved or executed actions |
| What are hook installation outcomes? | WTA `HookOperationCompleted` | Per-CLI installation/uninstallation outcome |
| Are Settings model probes producing catalogs? | Settings Editor probe events | Parsed catalog result, not proof of cache acceptance |

These events do **not** establish task success, automatic fix success,
time-to-fix, total response bytes, token consumption, monetary cost, session
lifetime, or unique active users. `ShowTokenUsageAndCost` is a UI setting,
not a usage or cost measurement.

## Providers and common metadata

The original usage funnel also depends on inherited
`Microsoft.Windows.Terminal.Win32Host.SessionBecameInteractive` and
`App.ConnectionCreated`, outside the dedicated agent-event catalog below.
The opt-in `Feature.TelemetryFunnels` suite captures those sources as well as
the agent events, checks real triggers and negative controls, and records
process-scoped typed ETW evidence. See the [live validation instructions](../test/e2e/README.md#opt-in-telemetry-funnel-validation).
Local event capture does not establish backend ingestion or D7/D28 retention.
Startup policy segmentation and live policy refresh are separate acceptance boundaries:
the suite's startup-policy cases require typed WTA `ErrorDetected` raw-policy and
effective-flag values after real failures with policy configured before launch.
Enabled policy uses shell-failure `Method=vt_sequence`. Blocked policy suppresses
OSC forwarding, which the test asserts independently of rendered shell-error
completion. Its typed `disabled`/`false` evidence instead comes from a controlled
nonzero shell exit with `Method=connection_state`, keeping the helper alive via
`closeOnExit=never`. That notification must not create an Autofix prompt or offer;
it does not establish observation of a policy-blocked VT failure.
Hot-policy notification remains unresolved on the validation host, is tracked in
[issue #991](https://github.com/microsoft/intelligent-terminal/issues/991), and is reported
by separate, unchanged tests. Preparing startup coverage does not establish a live pass
or resolve that hot-refresh limitation.

| Alias | Provider name | GUID | Dedicated events |
|---|---|---|---|
| App | `Microsoft.Windows.Terminal.App` | `{24a1622f-7da7-5c77-3303-d850bd1ab2ed}` | 7 |
| WTA | `Microsoft.Windows.Terminal.WTA` | `{4cfcff80-4e6b-5bfd-8ea1-d38e1226f70b}` | 16 |
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
omitted field. In particular, failed `AcpNewSessionComplete` events carry an
empty `SessionId`.

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

## Event catalog

Business-field counts exclude the common `PartA_PrivTags` field.

| Provider | Event | Business fields | Privacy category |
|---|---|---|---|
| App | [AgentPaneOpened](#appagentpaneopened) | 2 | Usage |
| App | [CommandPaletteAgentPromptEntered](#appcommandpaletteagentpromptentered) | 0 | Usage |
| App | [CommandPaletteDispatchedAgentPrompt](#appcommandpalettedispatchedagentprompt) | 1 | Usage |
| App | [AppCreated](#appappcreated) | 13 | Usage |
| App | [DelegateInvoked](#appdelegateinvoked) | 1 | Usage |
| App | [ErrorDetected](#apperrordetected) | 1 | Usage |
| App | [AgentSessionStarted](#appagentsessionstarted) | 24 | Usage |
| WTA | [AcpInitializeComplete](#wtaacpinitializecomplete) | 5 | Performance |
| WTA | [AcpNewSessionComplete](#wtaacpnewsessioncomplete) | 6 | Performance |
| WTA | [AcpLoadSessionComplete](#wtaacploadsessioncomplete) | 2 | Performance |
| WTA | [AgentColdStartComplete](#wtaagentcoldstartcomplete) | 5 | Performance |
| WTA | [AgentPromptSent](#wtaagentpromptsent) | 7 | Usage |
| WTA | [AgentResponseFirstToken](#wtaagentresponsefirsttoken) | 4 | Performance |
| WTA | [AgentResponseComplete](#wtaagentresponsecomplete) | 5 | Performance |
| WTA | [ErrorDetected](#wtaerrordetected) | 5 | Usage |
| WTA | [ErrorFixOffered](#wtaerrorfixoffered) | 1 | Usage |
| WTA | [ErrorFixAccepted](#wtaerrorfixaccepted) | 1 | Usage |
| WTA | [AgentSlashCommandUsed](#wtaagentslashcommandused) | 1 | Usage |
| WTA | [SessionsViewOpened](#wtasessionsviewopened) | 0 | Usage |
| WTA | [SessionResumeInvoked](#wtasessionresumeinvoked) | 2 | Usage |
| WTA | [DelegateInvoked](#wtadelegateinvoked) | 1 | Usage |
| WTA | [SessionMcpToolCalled](#wtasessionmcptoolcalled) | 1 | Usage |
| WTA | [HookOperationCompleted](#wtahookoperationcompleted) | 3 | Usage |
| Model | [AgentProviderChanged](#modelagentproviderchanged) | 3 | Usage |
| Editor | [AcpModelProbeStarted](#editoracpmodelprobestarted) | 2 | Performance |
| Editor | [AcpModelProbeDiscarded](#editoracpmodelprobediscarded) | 1 | Performance |
| Editor | [AcpModelProbeCompleted](#editoracpmodelprobecompleted) | 3 | Performance |

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

**Business fields:** none. The common `PartA_PrivTags` is still present.

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

No prompt text is included. This is a submission event, not evidence that
the selected mode launched or completed an agent task. In particular,
the reserved background entry point is not a completed background workflow.

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
| `SessionId` | String | ACP session ID, not `WT_SESSION`; a saved session can be loaded repeatedly |
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
- Re-loading the same saved session produces a new `StartId`, with the
  existing `SessionId` and `StartKind=Load`. `Load` alone does not identify
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
| `SessionId` | String | Returned ACP session ID on success; empty on failure |
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
| `SessionId` | String | ACP session receiving the prompt |
| `PromptLengthBytes` | UInt32 | Byte length of the constructed dispatch prompt, including context/templates |
| `IsAutofix` | Bool | Whether this dispatch is an autofix prompt |
| `IsByok` | Bool | BYOK state captured for this prompt |
| `AgentId` | String | Agent category |
| `TemplateKind` | String | `Planner`, `Autofix`, or `AgentCommand` |
| `Route` | String | Constant `AcpDispatch` |

Includes manual and automatic autofix analysis. The length is not the user's
typed character count, a token count, or the prompt contents.

### WTA.AgentResponseFirstToken

**Trigger:** the first counted text/thought chunk arrives for an active
turn with a known monotonic dispatch time.

| Field | Type | Meaning / values |
|---|---|---|
| `SessionId` | String | ACP session |
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
| `SessionId` | String | ACP session |
| `TotalDurationMs` | Double | Monotonic prompt-dispatch-to-completion duration in milliseconds |
| `Success` | Bool | Whether the ACP prompt request completed successfully |
| `IsByok` | Bool | BYOK state associated with the prompt |
| `AgentId` | String | Agent category associated with the turn |

RPC success does not mean that the answer was correct, a tool succeeded,
or the user's task was completed. `TotalResponseBytes` is not emitted.

### WTA.ErrorDetected

**Trigger:** a terminal notification is classified as non-acknowledged
`Actionable` or `Critical`, after the owning-tab filter.

| Field | Type | Meaning / values |
|---|---|---|
| `Severity` | String | `Actionable` or `Critical` |
| `Method` | String | Classified terminal event method: `connection_state` or `vt_sequence` |
| `PaneId` | String | Terminal pane identity, not an ACP session ID |
| `AllowAutoFixPolicy` | String | Raw host policy: `notConfigured`, `enabled`, `disabled`, or `unknown` |
| `AutoFixEnabled` | Bool | Effective helper runtime autofix switch, independent of the policy category |

Informational and auto-silenced classifications do not emit. There is no
command text, exit-code field, incident ID, or fix result. Multiple signals
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
| `OfferId` | String | Locally generated random UUID for the concrete recommendation offer |

Repeated renders do not emit again. Stashed panes, hidden/fully clipped
cards, overlays covering the recommendation, stale autofix generations,
generic non-autofix proposals, analysis, and prose-only results do not
count. A previously hidden offer can count when it is later presented.
An autocomplete popup elsewhere in the pane does not suppress the event;
its painted rectangle must overlap the recommendation card to obscure it.
This measures application-level presentation, not proof that the user
looked at the window.

### WTA.ErrorFixAccepted

**Trigger:** the user confirms **Run** for a previously presented autofix
offer, the confirmation claim remains valid, and the execution request is
successfully queued.

| Field | Type | Meaning / values |
|---|---|---|
| `OfferId` | String | UUID of the corresponding `ErrorFixOffered` event |

At most one acceptance is emitted per offer. Insert-only actions,
dismissal, clicking the ask-for-fix entry point, automatic analysis,
generic proposals, stale confirmations, and failed dispatch do not count.
Queuing execution is not proof the command executed or fixed the error.
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

Use a consistent capture/time window and deployment scope for each
measurement. These formulas describe logical aggregations; this document
does not assume a particular backend table or query language.

| Metric | Aggregation | Required qualification |
|---|---|---|
| Startup configuration share | `AppCreated` records matching the configuration / all `AppCreated` records in the same population | Window-created observations, not unique users or processes; primary and delegate are fields on one record |
| Successful starts | Distinct `StartId` on App `AgentSessionStarted` | Split `New` and `Load`; includes prewarm |
| Configuration share at start | Distinct starts matching a snapshot value / all distinct starts in the same population | Session-weighted, not user-weighted; display `unknown` separately |
| ACP operation success rate | Successful completion events / all completion events for the same event and route | Measures observed RPC attempts; exclude probes from chat analysis |
| ACP operation latency | Duration percentiles for one event, route, and outcome | Do not mix helper and master timings or successes and timeouts |
| Cold-start reliability | Successful cold starts / all observed cold starts | Excludes warm pool reuse |
| Prompt dispatch volume | Count WTA `AgentPromptSent` | Split autofix, BYOK, and template category as needed |
| Repair-offer acceptance | Distinct accepted `OfferId` / distinct offered `OfferId` in the same cohort | Attribute acceptance to the offer cohort; allow for acceptance outside the initial window; not fix success |
| First-text latency | Percentiles of `FirstTokenLatencyMs` | Only turns producing this event; thought text can count |
| Prompt RPC success rate | Successful `AgentResponseComplete` / all observed response completions | Not answer quality or task success; unfinished turns are absent |
| Model catalog success rate | `Succeeded=true` completions / all Editor probe completions | Report discards separately; not cache acceptance rate |
| Command/tool usage | Counts by command/tool category within its event | Attempts, not actions successfully executed |

**Correlation rules:**

- `StartId` is the snapshot event deduplication key. `SessionId` is not:
  loading the same session again is a separate start/load observation.
- App snapshots and WTA events that explicitly carry `SessionId` can be
  related at session scope. Use agent identity and capture/process/time
  context where available; do not assume opaque IDs are globally unique
  across all providers or deployments.
- A session can have many prompts and repeated loads. There is no exported
  `TurnId` or `StartId` on turn events, so joining only on `SessionId`
  does not create an exact prompt-to-response or load-to-turn mapping.
- `AcpNewSessionComplete` can report both master and helper layers for one
  creation. Choose the intended route rather than summing them as sessions.
- `AcpLoadSessionComplete` has no payload correlation ID or origin route.
  Do not claim a reliable session-view-versus-layout restore success rate
  from that event.
- `OfferId` joins a concrete repair offer to its confirmed execution request.
  It does not join to `ErrorDetected`: manual `/fix` may have no preceding
  classified error, and multiple classifications can describe one failure.
- The three Editor probe events do not share a unique probe ID. Do not
  construct exact per-probe joins solely from `AgentId` or `CacheRevision`.
- Events without a completion signal, such as session MCP requests and
  delegation launches, cannot produce a completion funnel by themselves.

No delivery or completeness guarantee is implied. Account for disabled
collection, process exit, event loss, and operations still in progress
before comparing counts.

## Retired events and settings filtering

| Retired event or field | Replacement / interpretation |
|---|---|
| `WTA.SlashCommandInvoked.CommandName` | Renamed to `WTA.AgentSlashCommandUsed.command`; no dual-write. Union old and new spellings across the deployment boundary when querying history |
| Legacy `AgentProviderConfigured` | Startup configuration is now carried by `App.AppCreated`; do not compare historical event counts directly. Connected-agent identity still comes from `App.AgentSessionStarted` |
| Proposed `AgentProviderConfigured(schema_version=2)`, `CustomAgentConfigured`, and `SidebarStateOnLaunch` | Consolidated into `App.AppCreated` before this change lands; no standalone emissions or dual-write |
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
event can also describe AI actions; it is not one of these 27 cataloged
events. The Settings Model provider,
`Microsoft.Windows.Terminal.Setting.Model`
(`{be579944-4d33-5202-e5d6-a7a57f1935cb}`), remains in use for inherited
settings telemetry and the independent `AgentProviderChanged` event.

## Privacy and collection boundaries

The dedicated payloads contain categories, booleans, counts, durations,
opaque correlation IDs, and numeric ACP error codes. They do not contain
prompt/response text, terminal contents, command text, custom agent names,
custom commands, model IDs, API keys, credential identifiers, or custom
endpoint URLs.

`OfferId` is a random, locally generated correlation token; it is not
derived from a command, prompt, path, or agent-provided recommendation text.

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
| App pane, delegate, error, and snapshot emission | [TerminalPage.cpp](../src/cascadia/TerminalApp/TerminalPage.cpp) |
| Command Palette entry and submission | [CommandPalette.cpp](../src/cascadia/TerminalApp/CommandPalette.cpp), [CommandPaletteTelemetry.h](../src/cascadia/TerminalApp/CommandPaletteTelemetry.h) |
| Window-created configuration and provider-change tracking | [AppLogic.cpp](../src/cascadia/TerminalApp/AppLogic.cpp), [AgentProviderTelemetry.h](../src/cascadia/TerminalApp/AgentProviderTelemetry.h) |
| Provider-change emission and shared configuration helpers | [CascadiaSettingsSerialization.cpp](../src/cascadia/TerminalSettingsModel/CascadiaSettingsSerialization.cpp), [SettingsTelemetry.h](../src/cascadia/TerminalSettingsModel/SettingsTelemetry.h) |
| Snapshot validation and categories | [AgentSessionTelemetry.h](../src/cascadia/TerminalApp/AgentSessionTelemetry.h) |
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

The [inherited Terminal/OpenConsole inventory](../TelemetryEvents.md#inherited-windows-terminal--openconsole-reference)
is retained separately and is pinned to its historical source revision.
