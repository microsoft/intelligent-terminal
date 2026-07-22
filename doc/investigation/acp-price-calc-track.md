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
  `test/e2e/artifacts/acp-provider-preflight/` or the user profile and are not committed.

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
| 7. Bottom Bar UI | C++/XAML tests assert hidden/visible/format/accessibility states | Add right-aligned `UsageGroup` before Session button | Pending |
| 8. Outer containment/privacy | Usage failure hides only Usage; logs contain no values | Add one outer boundary and usage-specific redaction | Pending |
| 9. Final integration | Rust full suite, x64 Debug build, and local ignored E2E | Verify end-to-end behavior and update design/current-state tables | Pending |

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
