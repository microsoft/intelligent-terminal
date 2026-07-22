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
| 2. Standard normalizer | Valid ACP usage normalizes; zero size, non-finite/negative cost, and invalid currency fail | Provider-neutral domain types and stable ACP normalizer | Pending |
| 3. Helper dispatch | `SessionNotification::UsageUpdate` emits a typed app event; malformed input returns `Err` | Route normalizer output through existing `AppEvent` channel | Pending |
| 4. Per-tab state | Cumulative session usage replaces prior values and resets on session lifecycle boundaries | Store `UsageSnapshot` in `TabSession` | Pending |
| 5. Existing state projection | `agent_state_changed` contains normalized usage or explicit null | Extend `project_tab_state`; no new COM/IDL route | Pending |
| 6. C++ cache/parser | Routed normalized JSON updates or clears the correct tab cache | Extend `OnAgentStateChanged` and `AgentPaneContent` | Pending |
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
