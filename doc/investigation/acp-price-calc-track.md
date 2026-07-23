# ACP Usage / Cost Implementation Track

This file records completed, tested implementation steps for
[acp-price-calc.md](acp-price-calc.md). Each completed step is committed and pushed with the
code it describes.

## Scope Guardrails

- First release consumes only stable ACP v1 `SessionUpdate::UsageUpdate`.
- No Gemini-specific provider work, private provider adapters, provider APIs, or local price
  conversion.
- Rust owns validation, normalization, state, coalescing, and projection. C++/XAML only cache and
  render normalized data.
- Development paths fail fast. One outer Usage containment boundary is added only after the inner
  pipeline and tests are complete.
- Existing unit/integration frameworks are committed. The local desktop E2E orchestration,
  screenshots, provider configs, and wire captures remain under git-ignored
  `test/e2e/artifacts/` or the user profile and are not committed.

## TDD Plan

| Step | RED test | Smallest GREEN implementation | Status |
|---|---|---|---|
| 0. Provider/build baseline | Existing registry, coordinator, WSL ACP, ItE2E, and live provider gates | Pin verified adapters and preserve historical command identification | Complete |
| 1. Reliable master delivery | Saturated helper queue must retain only the latest `UsageUpdate` | Per-session latest-value state and helper wake/drain path | Complete |
| 2. Standard normalizer | Valid ACP usage normalizes; zero size, non-finite/negative cost, and invalid currency fail | Provider-neutral domain types and stable ACP normalizer | Complete |
| 3. Helper dispatch | `SessionNotification::UsageUpdate` emits a typed app event; malformed input returns `Err` | Route normalizer output through existing `AppEvent` channel and store it on the owner tab | Complete |
| 4. Session lifecycle | Cumulative session usage replaces prior values and clears on new/load/agent identity boundaries | Apply explicit reset rules while model changes preserve usage | Complete |
| 5. Existing state projection | `agent_state_changed` contains normalized usage or explicit null | Extend `project_tab_state`; no new COM/IDL route | Complete |
| 6. C++ cache/parser | Routed normalized JSON updates or clears the correct tab cache | Extend `OnAgentStateChanged` and `AgentPaneContent` | Complete |
| 7. Bottom Bar UI | C++/XAML tests assert hidden/visible/format/accessibility states | Add right-aligned `UsageGroup` before Session button | Complete |
| 8. Outer containment/privacy | Usage failure hides only Usage; logs contain no values | Add one outer boundary and usage-specific redaction | Complete |
| 9. Final integration | Rust full suite, x64 Debug build, and local ignored E2E | Verify end-to-end behavior and update design/current-state tables | Complete |
| 10. Partial/error UI states | Cost-only, tokens-only, malformed, and absent reports never crash UI | Add one tested primary-display state and deterministic local mocks | Complete |
| 11. Provider extension boundary | Every known family has a module; unverified private payloads yield no data | Add a typed provider registry with empty trust allowlists | Complete |

## Completed Steps

### Step 0 - Provider and Build Baseline

**Result**

- Verified published adapter pins on 2026-07-22:
  - `@agentclientprotocol/claude-agent-acp@0.59.0`
  - `@agentclientprotocol/codex-acp@1.1.2`
- Kept the previous unpinned Claude command, Codex `1.1.0`, unpinned official Codex command, and
  deprecated Zed Codex command as identification-only compatibility aliases. New sessions always
  use the pinned commands.
- Kept latest-main OpenCode registry/model behavior while resolving the stash conflict.
- Synchronized the Rust registry, Terminal launch builder, Settings model probe, WSL ACP command,
  and dependency documentation.
- Completed local high-fidelity provider/UI preflight with real adapters, Agent Maestro 2.10.0,
  and VS Code LM. The ignored evidence directory contains Terminal, Agent pane, Claude, Codex,
  Session view, official Codex 1.1.2, and tool-call screenshots.

**Validation**

- `cargo test --target x86_64-pc-windows-msvc --manifest-path tools/wta/Cargo.toml agent_registry::tests -- --nocapture`
  - 14 passed, 0 failed.
- `cargo test --target x86_64-pc-windows-msvc --manifest-path tools/wta/Cargo.toml coordinator::tests -- --nocapture`
  - 74 passed, 0 failed.
- `cargo test --target x86_64-pc-windows-msvc --manifest-path tools/wta/Cargo.toml wsl_acp::tests -- --nocapture`
  - 7 passed, 0 failed.
- Pre-conflict full Rust suite: 1119 passed, 0 failed.
- x64 Debug incremental solution build: succeeded with 0 errors.
- ItE2E framework baseline: 11 hermetic tests and 12 Dev live tests passed.
- Final local provider checks showed `Claude Agent v0.59.0`, current-path Codex, official
  `Codex v1.1.2`, real prompt replies, terminal tool side effects, and a visible Session view.

**Committed files**

- Design/investigation documentation and this tracking note.
- Adapter pin and compatibility changes in existing product/registry code.
- Dependency setup documentation.
- No local E2E harness, screenshot, provider config, credential, or wire-log files.

### Step 1 - Reliable Master Usage Delivery

**RED**

- Added `rebinding_session_clears_previous_helpers_pending_usage` before adding the shared route
  binding boundary.
- Focused test failed to compile with `E0425` because `bind_session_route` did not exist. This
  exposed that a SessionId rebound from helper A to helper B could retain A's pending usage and
  later deliver it to the wrong helper.

**GREEN**

- Added one `bind_session_route` boundary reused by both `session/new` and `session/load`.
- The boundary holds the documented `session_to_helper -> pending_usage` lock order, clears the
  previous owner's pending usage, installs the new route, and returns the route count.
- Standard ACP `UsageUpdate` notifications bypass the ordinary bounded chunk queue, replace the
  previous value by SessionId, wake helpers through a watch generation, and are drained only by
  their owning helper.
- Disconnect cleanup removes pending usage for sessions owned by the departing helper.
- Usage values remain unlogged; only routing identifiers and schema-level event kinds are traced.

**Validation**

- RED command: focused test failed with two `cannot find function bind_session_route` errors.
- GREEN focused test: 1 passed, 0 failed.
- `master::tests`: 63 passed, 0 failed.
- Full WTA Rust suite: 1120 passed, 0 failed.

**Committed files**

- `tools/wta/src/master/mod.rs`
- `doc/investigation/acp-price-calc-track.md`
- Current-state update in `doc/investigation/acp-price-calc.md`

### Step 5 - Existing State Projection

**RED**

- Added pure event-builder tests before the builder existed.
- The focused build failed with two missing `build_agent_state_changed_event` errors.
- Tests require context and optional cost items, stable metric/unit/source/scope fields, and
  explicit `usage: null` when no snapshot exists.

**GREEN**

- Added typed `UsageProjection` / `UsageProjectionItem` structures.
- Context projects first as `acp.context.window` with decimal used/limit text and unit `token`.
- Optional cumulative cost projects as `acp.billing.cost` with the validated ISO currency code as
  its unit. No amount conversion or arithmetic occurs.
- Both items identify `scope=session`, `source=acp_standard`, and `stale=false`.
- Extracted a pure `build_agent_state_changed_event`; production `project_tab_state` reuses it and
  continues through the existing `agent_state_changed` route.
- Missing usage serializes as null so C++ can clear stale cached UI state.

**Validation**

- RED command failed with two missing-builder compiler errors.
- GREEN projection tests: 2 passed, 0 failed.
- Full WTA Rust suite: 1135 passed, 0 failed, 0 warnings.
- `usage.rs` passes rustfmt; no App rustfmt differences overlap Step 5 lines.

**Committed files**

- `tools/wta/src/usage.rs`
- `tools/wta/src/app.rs`
- `doc/investigation/acp-price-calc-track.md`
- Current-state/transport update in `doc/investigation/acp-price-calc.md`

### Step 6 - C++ Parser and Per-Pane Cache

**RED**

- Added five TerminalApp TAEF parser tests before creating `AgentUsage.h`; the focused project
  build failed with C1083 (missing header).
- After the parser passed, added two atomic cache tests before `UpdateCache` existed; the build
  failed with C2039 (missing member).

**GREEN**

- Added a pure `AgentUsage` parser independent of XAML/WinRT construction.
- Accepts null/empty as explicit clear and validates the complete item array atomically before
  replacing the cache. Malformed input throws and preserves the previous cache.
- Bounds item count and string lengths, validates decimal/scientific text, and requires typed
  metric/value/unit/scope/source/stale fields. No provider calculation or raw provider JSON is
  stored.
- `AgentPaneContent` caches only parsed items and raises its existing `StateChanged` event after a
  successful replace/clear. No IDL or new COM event route was added.
- `TerminalPage::OnAgentStateChanged` consumes the optional `usage` member for the routed tab.
  Missing means no change; null clears; object updates; another JSON type fails fast.

**Validation**

- Parser RED: focused C++ build failed because `AgentUsage.h` did not exist.
- Cache RED: focused C++ build failed because `AgentUsage::UpdateCache` did not exist.
- `AgentUsageTests`: 7 passed, 0 failed, 0 skipped.
- TerminalApp unit-test project build: succeeded with 0 errors.
- Full x64 Debug incremental solution build: succeeded with 0 errors (existing XAML/PRI warnings).
- clang-format reports no violations on new files or changed lines.

**Committed files**

- `src/cascadia/TerminalApp/AgentUsage.h/.cpp`
- `src/cascadia/TerminalApp/AgentPaneContent.h/.cpp`
- `src/cascadia/TerminalApp/TerminalPage.cpp`
- `src/cascadia/TerminalApp/TerminalAppLib.vcxproj`
- `src/cascadia/ut_app/AgentUsageTests.cpp`
- `src/cascadia/ut_app/TerminalApp.UnitTests.vcxproj`
- `doc/investigation/acp-price-calc-track.md`
- Current-state update in `doc/investigation/acp-price-calc.md`

### Step 7 - Bottom Bar Usage UI

**RED**

- Added two TerminalApp TAEF display-model tests before adding any display builder.
- The focused build failed with C2039 because `AgentUsage::BuildPrimaryDisplayTexts` and
  `AgentUsage::MaxPrimaryItems` did not exist.
- The tests require `1024 / 8192 Tokens`, `0.004 USD`, and a two-item maximum so Usage cannot
  crowd out the Session button.

**GREEN**

- Added one pure, provider-neutral display builder over the validated normalized cache. It emits
  at most two texts, formats the standard context metric with its limit and localized Tokens
  unit, and otherwise preserves normalized value/unit text without conversion or arithmetic.
- Added a right-aligned `UsageGroup` in the existing Bottom Bar star column immediately before
  the Session button. It starts collapsed and has a localized UI Automation name.
- `_UpdateBottomBarState` clears and rebuilds the group from the active tab's
  `AgentPaneContent` cache before the diagnostics gate, so Usage remains independent of
  diagnostics connection state. Empty or cleared usage collapses the group.
- Added the en-US source resources for the accessibility name and locked Tokens unit while
  preserving the resource file's UTF-8 BOM.

**Validation**

- RED build reported the expected C2039 errors for `BuildPrimaryDisplayTexts` and
  `MaxPrimaryItems`.
- `AgentUsageTests`: 9 passed, 0 failed, 0 skipped.
- TerminalApp unit-test project build: succeeded with 0 errors.
- Full x64 Debug incremental solution build: succeeded with 0 errors.
- `Resources.resw`: UTF-8 BOM preserved and XML parse valid.
- Rebuilt WTA and CascadiaPackage, clean-deployed Dev package 0.8.0.2, and verified the installed
  WTA and WindowsTerminal binary hashes match the current build outputs.
- Local ignored UI proof published typed `agent_state_changed.usage` to a stable tab ID. UIA read
  `UsageGroup` with `1024 / 8192 Tokens` and `0.004 USD`; the screenshot showed both values fully
  visible before the Session button. Publishing `usage: null` removed the group and both texts.

**Committed files**

- `src/cascadia/TerminalApp/AgentUsage.h/.cpp`
- `src/cascadia/TerminalApp/TerminalPage.xaml/.cpp`
- `src/cascadia/TerminalApp/Resources/en-US/Resources.resw`
- `src/cascadia/ut_app/AgentUsageTests.cpp`
- `doc/investigation/acp-price-calc-track.md`
- Current-state update in `doc/investigation/acp-price-calc.md`
- No local E2E script or screenshot files.

### Step 8 - Outer Containment and Privacy

**RED**

- Added a production-boundary test before defining `dispatch_session_notification` or
  `AppEvent::UsageCleared`; the focused build failed with E0599 for both missing symbols.
- The boundary test requires malformed Usage to emit a clear event and the following agent text
  chunk to keep flowing on the same session.
- Added a captured-tracing test with two sentinel token values and an App state test requiring
  only the owner tab's Usage to clear while its chat, another tab's Usage, and `Connected` state
  remain unchanged.

**GREEN**

- Kept `session_notification` and `normalize_standard_usage` as fail-fast inner functions. Their
  direct malformed-input test still returns `Err` and emits no state event.
- Added one `dispatch_session_notification` boundary at the production ACP notification entry.
  Only errors from a recognized Usage update are contained; the boundary emits
  `AppEvent::UsageCleared` and returns to the notification stream.
- `App::handle_event` resolves `UsageCleared` through the existing SessionId-to-tab map and clears
  only that tab's snapshot. The next state projection emits `usage: null` through the existing
  route, so C++ hides `UsageGroup` without a new COM/IDL path.
- Removed value-bearing normalizer error text from tracing and ACP error data. The outer warning
  records only fixed `schema=acp.v1.session_usage`, `source=acp_standard`, and
  `outcome=rejected` fields. Usage trace continues to suppress the full update payload.

**Validation**

- RED build reported missing `AppEvent::UsageCleared` and
  `WtaClient::dispatch_session_notification`.
- Containment/chat-continuity test: 1 passed, 0 failed.
- Inner fail-fast test: 1 passed, 0 failed.
- Captured-log privacy test: 1 passed, 0 failed; schema was present and both sentinel values were
  absent.
- Owner-tab clear/isolation test: 1 passed, 0 failed.
- Usage normalizer tests: 5 passed, 0 failed.
- ACP mock-agent/client tests: 31 passed, 0 failed.
- Full WTA Rust suite: 1138 passed, 0 failed.
- No rustfmt differences overlap Step 8 production or test lines; broader crate formatting drift
  remains outside this change.

**Committed files**

- `tools/wta/src/usage.rs`
- `tools/wta/src/protocol/acp/client.rs`
- `tools/wta/src/protocol/acp/mock_agent_tests.rs`
- `tools/wta/src/app.rs`
- `doc/investigation/acp-price-calc-track.md`
- Current-state update in `doc/investigation/acp-price-calc.md`

### Step 9 - Final Integration

**RED**

- Added a local ignored standalone ACP agent that emits a valid standard `usage_update` followed
  by an agent text chunk. The first full desktop run proved master delivery and helper
  normalization (`first_event=usage_update`) but the Bottom Bar stayed hidden: updating App state
  did not immediately project `agent_state_changed` to C++.
- Added an existing-framework App test requiring `UsageReported` to project an owner-tab usage
  object immediately and `UsageCleared` to project null. Before the projection capture and refresh
  existed, the focused build failed with E0599 for `take_projected_test_events`.

**GREEN**

- Both Usage event branches now resolve the owner tab through the existing SessionId map, mutate
  that tab, and immediately reuse `project_tab_state`. No new transport or C++ route was added.
- Added test-only capture at the existing projection boundary so the focused test verifies the
  exact production event builder and owner-tab ID without replacing the production publisher.
- Kept the local desktop harness ignored. Its standalone agent returns a unique SessionId for
  every `session/new`, and the verifier pins the agent pane to the same foreground window/tab as
  UI Automation so multi-window restore state cannot cross-wire the proof.

**Validation**

- Projection RED: focused build failed because `take_projected_test_events` did not exist.
- Projection GREEN: 1 passed, 0 failed.
- All usage-filtered Rust tests: 15 passed, 0 failed.
- Full WTA Rust suite: 1139 passed, 0 failed.
- Final full x64 Debug solution build: succeeded with 0 errors (170 existing warnings).
- Rebuilt WTA, refreshed the deployed loose Dev 0.8.0.2 package, and verified its WTA hash matches
  the current build.
- Existing ACP probe verified the ignored standalone agent's protocol-v1 initialize, unique
  session, valid `usage_update`, `FINAL_USAGE_CHAT_OK` chunk, and `end_turn` response.
- Full desktop pipeline passed through agent -> master -> helper -> App ->
  `agent_state_changed` -> C++: UIA read `1024 / 8192 Tokens` and `0.004 USD`; the visible
  screenshot is 62,486 bytes.
- A second prompt emitted malformed Usage and then `FINAL_CONTAINMENT_CHAT_OK`. Both chat replies
  remained visible, Usage collapsed, and the contained screenshot is 64,222 bytes.
- Deployed-run logs contained the fixed schema/source/outcome rejection warning and neither
  malformed sentinel value. Visual inspection confirmed no overlap with the Session button.

**Committed files**

- `tools/wta/src/app.rs`
- `doc/investigation/acp-price-calc-track.md`
- Current-state update in `doc/investigation/acp-price-calc.md`
- No local standalone agent, E2E verifier, log, or screenshot files.

### Step 10 - Partial, Error, and Missing Usage States

**RED**

- Added four TerminalApp TAEF tests before `BuildPrimaryDisplay` existed. The focused build failed
  with C2039/C3861 for the missing builder.
- The tests require cost-only normalized items to show one currency value, tokens-only items to
  show one context ratio, and both contained-error clear and no-report states to collapse Usage.
- Standard ACP v1 requires `used` and `size`; only `cost` is optional. Therefore tokens-only,
  malformed, and no-report mocks run through ACP wire, while cost-only is tested at the
  normalized projection contract consumed by C++ (the shape a future trusted extension uses).

**GREEN**

- Added a pure `PrimaryDisplay { texts, visible }` state that reuses the existing formatter.
- `_UpdateBottomBarState` consumes that state, so one tested visibility decision owns empty,
  one-item, and two-item rendering. XAML does not contain provider or scenario branches.
- Extended the ignored standalone ACP agent with deterministic `tokens-only`, `error`, and
  `none` scenarios. Added a local four-scenario desktop verifier with chat markers, UIA
  visibility/text checks, screenshots, and process-liveness assertions.
- The local verifier resolves each pane from its current structured helper log. This avoids a
  known local test-framework race where concurrent helpers can interleave append-only JSONL
  records; no test-framework file is committed in this feature change.

**Validation**

- RED build reported missing `AgentUsage::BuildPrimaryDisplay`.
- `AgentUsageTests`: 13 passed, 0 failed, 0 skipped.
- Full x64 Debug solution build: succeeded with 0 errors (169 existing warnings).
- CascadiaPackage clean build: succeeded with 0 errors; deployed Dev 0.8.0.2 Terminal and WTA
  hashes matched current build outputs.
- ACP probes: tokens-only emitted valid `usage_update` without cost; error emitted malformed
  Usage then a chat chunk; no-report emitted only a chat chunk. All returned `end_turn`.
- Desktop cost-only: displayed only `0.004 USD`; chat continued; process remained alive.
- Desktop tokens-only: displayed only `1024 / 8192 Tokens`; chat continued; process remained
  alive.
- Desktop malformed error: Usage collapsed, `EDGE_ERROR_OK` remained visible, and the process
  remained alive.
- Desktop no-report: Usage stayed collapsed, `EDGE_NO_USAGE_OK` remained visible, and the process
  remained alive.
- Visual inspection found no Bottom Bar overlap. Error-run logs contained the redacted rejection
  warning and neither malformed sentinel value.

**Committed files**

- `src/cascadia/TerminalApp/AgentUsage.h/.cpp`
- `src/cascadia/TerminalApp/TerminalPage.cpp`
- `src/cascadia/ut_app/AgentUsageTests.cpp`
- `doc/investigation/acp-price-calc-track.md`
- Current-state/edge-contract update in `doc/investigation/acp-price-calc.md`
- No local mock, E2E verifier, helper log, or screenshot files.

### Step 11 - Modular Provider Usage Boundary

**RED**

- Added provider registry contract tests before the module existed. The focused build failed with
  E0583 because `usage/providers` was missing.
- Tests require one adapter for every `KNOWN_AGENTS` family, explicit private-usage policy,
  fail-closed lookup for unknown/custom agents, and no data from unverified private payloads.

**GREEN**

- Added `tools/wta/src/usage/providers/` with separate `copilot`, `claude`, `codex`, `gemini`, and
  `opencode` modules behind one `ProviderUsageAdapter` interface and registry.
- Centralized the five Rust family IDs in `agent_registry`; launch profiles, historical command
  aliases, and provider modules now share those constants. C++-to-Rust codegen remains separate.
- The interface accepts session-update metadata, prompt-response metadata, extension
  notifications, and already-fetched provider API responses. Network/auth and CLI credential
  access are deliberately outside this parser boundary.
- Provider contributions can independently contain context, cost, or custom metrics, preserving
  the cost-only normalized shape without inventing standard ACP token fields.
- Every module explicitly implements extraction but currently returns an empty contribution.
  Trusted reporter allowlists are empty until a real wire schema and reporter identity are
  verified. Unknown/custom agents receive no private adapter and continue through standard ACP.
- Policies are explicit: Copilot `Reserved`; Claude/Codex/OpenCode `StandardAcpOnly`; Gemini
  `OutOfScope`. Standard ACP remains provider-neutral and runs before this future extension layer.
- The private registry is intentionally not runtime-wired yet: effective family and exact
  reporter identity must first be carried from the trusted master handshake into helper state.

**Validation**

- RED build reported missing module `providers`.
- Usage tests: 9 passed, 0 failed (5 standard normalizer + 4 provider contracts).
- Agent registry tests: 14 passed, 0 failed.
- Full WTA Rust suite: 1143 passed, 0 failed.
- No compiler warning originated from `usage.rs` or `usage/providers`.

**Committed files**

- `tools/wta/src/agent_registry.rs`
- `tools/wta/src/usage.rs`
- `tools/wta/src/usage/providers/mod.rs`
- `tools/wta/src/usage/providers/{copilot,claude,codex,gemini,opencode}.rs`
- `doc/investigation/acp-price-calc-track.md`
- Current-state/interface update in `doc/investigation/acp-price-calc.md`

### Step 4 - Session Usage Lifecycle

**RED**

- Added four lifecycle tests before changing reset behavior. `/clear`, `/new`, and
  `load_session` retained stale usage and failed; global model change already preserved usage and
  passed.
- Added a second RED test showing that a reconnect binding a new ACP SessionId retained the old
  session's usage snapshot.

**GREEN**

- Added usage reset to the existing `TabSession::clear_chat_history` owner reused by `/clear`,
  `/new`, and `load_session`; no duplicate reset logic was added at call sites.
- `AgentConnected` clears usage only when the bound SessionId changes. Repeated connected events
  for the same session do not erase its usage.
- Global and per-tab model changes do not clear session-cumulative usage.
- Tab close and app restart already drop their in-memory `TabSession`; no persistence was added.

**Validation**

- Initial RED run: 1 passed (model preservation), 3 failed (clear/new/load stale usage).
- Connection RED run: 1 failed because the old snapshot survived a new SessionId.
- GREEN lifecycle tests: 5 passed, 0 failed.
- Full WTA Rust suite: 1133 passed, 0 failed.
- No rustfmt differences overlap Step 4 changed lines.

**Committed files**

- `tools/wta/src/app.rs`
- `doc/investigation/acp-price-calc-track.md`
- Current-state update in `doc/investigation/acp-price-calc.md`

### Step 3 - Helper Dispatch and Owner-Tab Storage

**RED**

- Added existing-framework ACP client tests for a valid `UsageUpdate` and a malformed zero-size
  update before adding the event variant.
- Added an App state test that binds a session to a non-active tab and requires usage to update
  only that owner.
- RED failures showed the missing `AppEvent::UsageReported` variant and `TabSession.usage` field.

**GREEN**

- `WtaClient::session_notification` now recognizes typed ACP v1 `UsageUpdate`, runs the standard
  normalizer, and emits `AppEvent::UsageReported`.
- Recognized malformed usage returns ACP `invalid_params`; it never reaches App state.
- `App::handle_event` resolves the event's SessionId through the existing `session_to_tab` map and
  stores the latest snapshot only on the owner `TabSession`.
- Raw Usage values are no longer formatted into the trace-level full-notification log. Normalizer
  failures log only schema ID and error class/message, not amount/token values.
- No new transport, COM route, provider branch, dependency, or UI behavior was added.

**Validation**

- RED client test failed because `AppEvent::UsageReported` did not exist.
- RED App test failed because `AppEvent::UsageReported` and `TabSession.usage` did not exist.
- Valid client dispatch: 1 passed, 0 failed.
- Malformed client dispatch: 1 passed, 0 failed.
- Owner-tab state routing: 1 passed, 0 failed.
- Full WTA Rust suite: 1128 passed, 0 failed, 0 warnings.
- No rustfmt differences overlap Step 3 changed lines.

**Committed files**

- `tools/wta/src/protocol/acp/client.rs`
- `tools/wta/src/protocol/acp/mock_agent_tests.rs`
- `tools/wta/src/app.rs`
- `doc/investigation/acp-price-calc-track.md`
- Current-state update in `doc/investigation/acp-price-calc.md`

### Step 2 - Standard ACP Usage Normalizer

**RED**

- Added five contract tests before defining any Usage production types or normalizer.
- The focused build failed with 11 missing-symbol errors for `UsageCost`, `UsageError`, and
  `normalize_standard_usage`.

**GREEN**

- Added a provider-neutral `UsageSnapshot` containing context `used` / `size` and optional
  cumulative `UsageCost`.
- Validates non-zero context size, `used <= size`, finite non-negative cost, and a canonical
  three-uppercase-ASCII-letter currency shape.
- Converts the ACP wire `f64` amount to decimal display text once. The text does not recover wire
  precision and is never used for arithmetic or local price conversion.
- Ignores ACP `_meta`; no provider-specific schema or private adapter is introduced.
- Added no dependency, so Component Governance and third-party notices are unchanged.

**Validation**

- RED command: focused build failed with 11 expected missing-symbol errors.
- GREEN focused tests: 5 passed, 0 failed.
- `rustfmt --check` passes for `usage.rs` and the `main.rs` module registration.
- Full WTA Rust suite: 1125 passed, 0 failed.

**Committed files**

- `tools/wta/src/usage.rs`
- `tools/wta/src/main.rs`
- `doc/investigation/acp-price-calc-track.md`
- Current-state update in `doc/investigation/acp-price-calc.md`
