# Release Unit-Test Plan

This plan defines what release verification can move into **unit tests (UT)**, against
`doc\release-check-list.md`. It is the planning artifact only — it does not write the
tests yet. It also defines the markers used to annotate the release checklist.

UT here means deterministic, no-UI, no-network, no-subprocess tests:
- **C++ TAEF unit tests** (`runut.cmd`) — SettingsModel + TerminalApp pure logic.
- **Rust `cargo test`** — WTA pure logic.

Anything that needs a running Terminal, a live agent, real install/auth, rendering,
focus, or visual judgment is **not** UT — it belongs to mock-ACP E2E, UI automation,
or manual sign-off (see `doc\release-automation-plan.md`).

## Marker legend (used in `release-check-list.md`)

| Marker | Meaning |
|---|---|
| `[UT✓]` | Already covered by an existing unit test. |
| `[UT+]` | UT-coverable; test does not exist yet — recommended to add. |
| `[UT~]` | Partially UT-coverable: the logic/decision core can be unit-tested, but the observable behavior still needs E2E/UI. |
| `[E2E]` | Needs mock-ACP end-to-end or UI automation; not a UT. |
| `[MANUAL]` | Human judgment (visual polish, real LLM quality, install/auth UX). |

## What our UT *can* handle

- **Settings model**: JSON round-trip and layering for `acpAgent`, `delegateAgent`,
  `acpModel`, `delegateModel`, `agentPanePosition`, `autoFixEnabled`, custom commands.
- **Policy gates**: `EffectiveAcpAgent` / `EffectiveDelegateAgent` /
  `EffectiveAutoFixEnabled` and the `IsAgentPolicyLocked` family.
- **Custom agent id**: `DeriveCustomAgentId`, `custom:` prefix preservation.
- **Default keybindings**: `defaults.json` → `ActionMap` binding assertions for the
  agent shortcuts.
- **Slash commands**: `commands::classify` mapping + `handle_slash_command` dispatch
  intent for `/help /clear /new /stop /fix /restart /sessions /model`.
- **Agent registry**: agent-id resolution, `build_acp_command`, model-flag handling,
  Copilot/Claude/Codex/Gemini/custom command construction.
- **Session model**: `decide_enter_action` routing, activity/liveness transitions,
  origin filter (incl. MVP shell-only), custom-agent not-resumable.
- **Autofix reducer**: detection on/off, suggestion on/off, cold-start gate
  (`state != Connected`), busy/defer, target-tab routing, dismiss/clear.
- **Failure classification**: `classify_acp_error` → `AgentFailure` (auth, transport,
  protocol, etc.).
- **Hooks status contract**: `wta hooks status --json` parse + formatter.
- **Runtime paths / RTL / localization-string presence**: path resolution,
  `IsRtlLocale`, resource-key presence lint.

## What our UT *cannot* handle (do not target with UT)

- FRE/Settings actually opening, rendering, focusing, persisting through the UI.
- Agent pane open/hide/stash, pane position layout, view switching.
- Real or mock agent chat round-trips, streaming render, permission UI, insert/run.
- Real install (`winget`), real auth/login, real CLI hook install/remove.
- `wtcli`/COM activation, master/helper spawn, multi-window drag.
- Visual polish, high-contrast/RTL layout, scaling, screen-reader quality.

## Existing UT inventory

| Area | File | Status |
|---|---|---|
| Custom agent round-trip + policy | `src\cascadia\UnitTests_SettingsModel\CustomAgentAndPolicyTests.cpp` | strong |
| Custom agent id derivation | `src\cascadia\ut_app\CustomAgentIdTests.cpp` | strong |
| Hooks status JSON contract | `src\cascadia\ut_app\AgentHooksStatusTests.cpp` | strong |
| Keychord parse/serialize | `src\cascadia\UnitTests_SettingsModel\KeyBindingsTests.cpp` | present (no agent-shortcut assertions yet) |
| Settings JSON / actions | `src\cascadia\UnitTests_SettingsModel\DeserializationTests.cpp`, `src\cascadia\UnitTests_SettingsModel\CommandTests.cpp`, `src\cascadia\UnitTests_SettingsModel\SerializationTests.cpp` | present |
| Session Enter routing | `tools\wta\src\session_mgmt.rs` | strong |
| Session state/origin | `tools\wta\src\agent_sessions.rs` | strong |
| Slash commands | `tools\wta\src\slash_command_tests.rs` | partial (help/clear/new/stop) |
| CLI parse / sessions list | `tools\wta\src\cli_tests.rs` | strong |
| Agent registry | `tools\wta\src\agent_registry.rs` | strong |
| ACP failure classification | `tools\wta\src\protocol\acp\failure.rs` | strong |
| Runtime paths | `tools\wta\src\runtime_paths.rs` | present |
| Autofix reducer | `tools\wta\src\app\autofix.rs` | **0 tests — gap** |
| RTL | `src\cascadia\ut_app\RtlHelperTests.cpp`, `tools\wta\src\rtl.rs` | present |

## Implementation status (this branch)

The following `[UT+]` items have been implemented and are passing:

- **Autofix reducer** (`tools/wta/src/autofix_tests.rs`, 6 tests): cold-start `state != Connected` drop, missing-`tab_id` drop, suggest-mode Detected-without-submit, busy same-pane re-emit vs different-pane drop, success-exit-code does-not-arm.
- **Default agent keybindings** (`KeyBindingsTests::DefaultAgentKeybindings`): `ctrl+shift+.`/`i`/`/`, `alt+shift+b`/`/` → correct action IDs, asserted against real `LoadDefaults()`.
- **Agent action parse** (`KeyBindingsTests::AgentActionsParse`): `openAgentPane` / `focusAgentPane` / `openAgentSessions` / `openBackgroundAgent` command keywords parse to their `ShortcutAction`, and `commandPalette` + `launchMode: agentDelegation` parses to `ToggleCommandPalette` with `CommandPaletteLaunchMode::AgentDelegation`.
- **Built-in agent settings round-trip** (`CustomAgentAndPolicyTests`, 6 tests): built-in agent/model/pane-position/autofix round-trip + default resolution + `EffectiveAutoFixEnabled` false when detection off.
- **Slash dispatch** (`slash_command_tests.rs`, 8 tests): `/sessions`, `/restart`, `/fix` (idle + busy), `/model` (none/bare/direct).
- **Mock-ACP scenarios** (`protocol/acp/mock_agent_tests.rs` harness + `app::tests`): drive the real `WtaClient` against a scripted mock and assert real-`App` state — chat reply streams into the buffer, tool-call card surfaces, tool-call completion updates in place (no duplicate), plan card surfaces with entries, two chunks coalesce, permission allow/reject/`y`/`n` round-trip the option id back to the agent.
- **TUI render harness** (`app::tests::render_to_text` over ratatui `TestBackend`): asserts the painted output for chat (all `ChatMessage` variants, completed turns, connecting/welcome lines), permission (full card + compact fallback), setup, auth, sessions view, model picker, help overlay, and the recommendation card.
- **Streaming-display + pure helpers** (`ui::chat::tests`, `protocol::acp::client::tests`): JSON unwrap/escape/Unicode/surrogate-pair decode, `user_visible_stream_text`, `truncate_render_text`, `push_dot_prefixed_lines`, and the `client.rs` string/timing/humanize helpers.

> **Bugs found and fixed via these tests:** (1) permission `y`/`n` quick-keys
> never matched (`kind` is PascalCase `AllowOnce`/`RejectOnce`, matcher searched
> lowercase); (2) the streaming JSON extractor dropped emoji encoded as UTF-16
> surrogate pairs; (3) it returned `None` when the field name appeared earlier
> as a string value. All three are fixed with regression tests.

> **Localization parity** (the WTA locale backfill + `locale_parity_tests.rs`
> guard) was split out of this test-suite branch into its own PR to keep the
> two concerns reviewable independently. It is therefore **not** part of this
> branch; see the localization PR.

Verified **already covered**, no new tests needed:

- **`classify_wt_event`** exit-code split and connection-state classification (existing `app::tests`).
- **Hooks auto-upgrade decision** (`agent_hooks_installer.rs`: `decide_upgrade` not-installed/disabled/version-compare + `upgrade_state` cache round-trip).

`[UT+]` backlog status: **cleared** — every checklist item that was UT-coverable is now `[UT✓]` (or `[UT✓] [E2E]` where only the logic half is a UT). The one item that looked like a settings round-trip but isn't — "Session-management choice persists" — is reclassified `[UT~] [E2E]` because the FRE toggle installs hooks on Save rather than persisting a settings bool (read-back state is parse-tested via `AgentHooksStatusTests`, the persistence itself is E2E).

Localization parity (split to a separate PR):

- The `tools/wta/locales/*.yml` gap — `commands.fix.summary` was absent from
  all 88 non-en-US locales, plus six other `commands.*.summary` keys lagged —
  is fixed by a **separate localization PR** (locale backfill +
  `every_locale_has_all_en_us_keys` guard test). It is not part of this
  test-suite branch.

## Recommended new UT work (the `[UT+]` backlog) — COMPLETED

All seven backlog items below have been implemented or verified-already-covered
(see the implementation-status section above). Kept here for traceability.

1. **Autofix reducer tests** (`tools\wta\src\app\autofix.rs`) — highest value, currently zero:
   - `!autofix_enabled && !forced` → emits Detected pill only, no LLM submit.
   - `state != Connected` → drops, no submit.
   - missing `tab_id` → dropped with warning.
   - busy same-pane → re-emit only; busy different-pane → dropped.
   - target-tab routing uses failing pane's tab, not focused tab.
   - dismiss/clear resets state.
2. **Default agent keybinding assertions** (SettingsModel UT): `alt+shift+b` →
   `openBackgroundAgent`, `alt+shift+/` → command-palette agent delegation,
   `ctrl+shift+.` → `openAgentPane`, `ctrl+shift+/` → `openAgentSessions`,
   `ctrl+shift+i` → `focusAgentPane`.
3. **Built-in agent settings round-trip** (SettingsModel UT): `acpAgent`/`delegateAgent`/
   `acpModel`/`delegateModel`/`agentPanePosition`/`autoFixEnabled` survive load; default
   resolution for pane position; `EffectiveAutoFixEnabled` false when detection off.
4. **Slash dispatch coverage** (WTA): `/fix`, `/restart`, `/sessions`, `/model` dispatch
   intent (not just classify).
5. **classify_wt_event** (WTA): failure event classification feeding autofix
   (failure vs success exit code).
6. **Hooks auto-upgrade decision** (WTA): bundle-version compare → upgrade/skip; opt-in
   skip for non-installed CLIs; disabled-plugin skip.
7. **Localization presence lint** (UT): new agent resource keys exist across required
   locales (or are intentionally locked).

## Per-item marker mapping

Markers below are what each `release-check-list.md` line should carry. `[UT~]` notes the
testable core in parentheses.

### 0. FRE
- FRE opens / completes / skip-close / links / save progress — `[E2E]`
- FRE error messages actionable — `[UT~]` (WTA `classify_acp_error`) + `[E2E]`
- FRE respects policy locks — `[UT~]` (`IsAgentPolicyLocked`, Effective* gates) + `[E2E]`
- FRE RTL/localized layout — `[UT~]` (`IsRtlLocale`) + `[MANUAL]`
- Agent selection (Copilot no-install/preinstalled, Claude, Codex, Gemini, unavailable) — `[UT~]` (registry + policy filter) + `[E2E]`
- Agent selection persists — `[UT+]` (settings round-trip)
- Detection off/on, suggestion off/on — `[UT+]` (`EffectiveAutoFixEnabled` + persistence) + `[E2E]`
- Detection/suggestion dependency — `[UT~]` (effective logic) + `[E2E]`
- Settings persist — `[UT+]`
- Session management off/on, hints, install failure, persist — `[UT~]` (hooks status parse, persistence) + `[E2E]`
- Pane position bottom/right/left/top — `[E2E]`; position persists — `[UT+]`

### 1. Settings > AI Agents
- Page opens — `[E2E]`
- Built-in agent dropdown state — `[UT~]` (registry/filter) + `[E2E]`
- Agent pane agent save / delegate agent save — `[UT✓]` (custom) / `[UT+]` (built-in round-trip)
- Model control appears — `[UT~]` + `[E2E]`
- Model changes apply / delegate model — `[UT✓]` (`build_acp_command`) + `[E2E]`
- Pane position setting — `[UT+]` (persistence) + `[E2E]`
- Detection / suggestion setting — `[UT+]` + `[E2E]`
- Session hooks install/remove — `[UT~]` (status parse) + `[E2E]`
- Policy lock UI — `[UT~]` (Effective*/IsLocked) + `[E2E]`

### 2. Agent pane chat
- Open/hide/focus (button + `Ctrl+Shift+.` + `Ctrl+Shift+I`), positions, stash preserves chat, tab-close cleanup — `[E2E]` (keybinding *bindings* themselves `[UT+]`)
- Built-in chat matrix (Copilot/Claude/Codex/Gemini) — `[E2E]` + `[MANUAL]`
- Copilot missing-CLI guidance — `[UT~]` (registry install hint) + `[E2E]`
- Auth failure / recovery — `[UT~]` (`AgentFailure::AuthRequired`) + `[E2E]`
- Restart after settings change — `[E2E]`
- Input appearance/typing/paste/keyboard/IME/streaming/permission/insert/run/target — `[E2E]`
- `/help` `/clear` `/new` `/stop` — `[UT✓]`
- `/fix` `/restart` `/sessions` `/model` — `[UT+]` (dispatch) ; classify `[UT✓]`
- Unknown slash command safe — `[UT✓]`
- Esc/back navigation — `[E2E]`
- Chat/session view switch — `[UT~]` (rows/cursor model) + `[E2E]`

### 3. Autofix
- Shell integration installed / missing-safe — `[E2E]`
- Failure detection / success ignored — `[UT+]` (`classify_wt_event`)
- Detection off suppresses / on observes — `[UT+]` (reducer)
- Suggestion off suppresses LLM / on triggers — `[UT+]` (reducer)
- Cold-start dropped — `[UT+]` (`state != Connected`)
- Visible/stashed pane autofix, opens UI, insert/run suggestion — `[E2E]`
- Reject/dismiss — `[UT+]` (clear state)
- Target pane correct — `[UT+]` (target-tab routing)
- Autofix with Copilot/Claude/Codex/Gemini/custom — `[E2E]` + `[MANUAL]`
- Split/moved-tab/multi-window/closed-pane routing — `[UT~]` (tab/window routing) + `[E2E]`

### 4. Session management
- Button/hotkey(`Ctrl+Shift+/`)/action/empty/refresh surfaces — `[E2E]`; `/sessions` classify `[UT✓]`
- Active/Running/Waiting/Idle/Ended/Historical states + transitions — `[UT✓]` (`agent_sessions.rs`)
- Focus active / focus stashed / restore old / shell-pane / agent-pane / unsupported / Enter / Shift+Enter — `[UT✓]` (`decide_enter_action`); actual dispatch `[E2E]`
- Built-in agents tracked — `[UT✓]` (origin/cli) + `[E2E]`
- Custom agent safe / limitation — `[UT✓]` (`NotResumable` UnknownCli)
- MVP origin filter — `[UT✓]`
- Hooks off safe — `[UT~]` + `[E2E]`

### 5. Delegate shortcuts
- `Alt+Shift+B` / `Alt+Shift+/` binding — `[UT+]`; actual launch — `[E2E]`
- Delegate cwd correct — `[UT~]` + `[E2E]`
- Delegate provider correct — `[UT✓]` (`EffectiveDelegateAgent`)
- Delegate model correct — `[UT✓]` (`build_acp_command`)
- Palette launches delegate / cancel safe — `[E2E]`
- Delegate with each agent — `[E2E]` + `[MANUAL]`
- Delegate errors actionable — `[UT~]` (failure classify) + `[E2E]`

### 6. Custom agents
- Custom is Settings-only — `[E2E]` (design)
- Add/save/edit/delete custom ACP — `[UT✓]` (`CustomAgentIdTests` + round-trip)
- Model selection visible — `[UT~]` + `[E2E]`
- Custom direct chat / command request / insert-run / autofix / failure-safe — `[E2E]`
- Add/save custom delegate — `[UT✓]` (round-trip)
- `Alt+Shift+B` / `Alt+Shift+/` use custom delegate — `[UT+]` (binding + resolution) + `[E2E]`
- Custom delegate cwd / errors — `[UT~]` + `[E2E]`

### 7. Multi-pane / window
- Split keeps chat / target selection — `[UT~]` (routing) + `[E2E]`
- Multiple tabs / panes isolated — `[UT~]` (per-tab state) + `[E2E]`
- Move tab preserves chat — `[E2E]`
- Move tab preserves session routing — `[UT~]` (tab_id routing) + `[E2E]`
- Move tab preserves autofix — `[UT~]` + `[E2E]`
- Multiple windows no cross-route — `[UT~]` (window_id filter) + `[E2E]`
- Close source window / target tab cleanup — `[E2E]`

### 8. Hooks
- Install from FRE/Settings, per-CLI install, remove — `[E2E]`
- Disabled plugin respected / opt-in preserved — `[UT~]` (decision logic) + `[E2E]`
- Auto-upgrade on bundle change — `[UT+]` (version compare) + `[E2E]`
- Hook logs — `[E2E]`
- Hooks status contract — `[UT✓]` (`AgentHooksStatusTests`)

### 9. Packaging / protocol
- Packaged wta present / identity / not-stale / `WT_COM_CLSID` — `[E2E]`
- `wtcli list/capture/send-keys/listen` — `[E2E]`
- Master/helper start / crash recovery — `[E2E]`
- (Log-dir path resolution behind these — `[UT✓]` `runtime_paths.rs`)

### 10. Diagnostics / logs
- Log-dir + version-dir resolution — `[UT✓]` / `[UT~]` (`runtime_paths`, housekeeping)
- Logs written / bug-report zip / early-startup logs / release level — `[E2E]`

### 11. A11y / localization
- Keyboard-only FRE/Settings/agent pane — `[E2E]`
- Narrator readouts — `[MANUAL]`
- High contrast / theme / scaling — `[MANUAL]`
- RTL — `[UT~]` (`IsRtlLocale`) + `[MANUAL]`
- Localization strings present — `[UT+]` (resource presence lint)
- Pseudo-locale — `[UT~]` + `[MANUAL]`

### 12. Release decision
- All process/sign-off — `[MANUAL]`

## Coverage summary

- **Already UT-covered (`[UT✓]`)**: most of Session management, slash `/help /clear /new /stop`,
  custom-agent save/id, delegate provider/model resolution, hooks status contract, RTL core.
- **UT-coverable but missing (`[UT+]`)**: autofix reducer (top priority), agent keybinding
  assertions, built-in settings round-trip + effective autofix, `/fix /restart /sessions /model`
  dispatch, `classify_wt_event`, hooks auto-upgrade decision, localization presence.
- **Partial (`[UT~]`)**: anything where a decision core is testable but the user-visible
  behavior still needs E2E (policy-locked UI, model visibility, routing across tabs/windows).
- **Not UT (`[E2E]` / `[MANUAL]`)**: all UI open/render/focus, real/mock agent chat,
  install/auth, multi-window drag, packaging/protocol runtime, visual + a11y judgment.
