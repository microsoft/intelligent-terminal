# Release Automation Plan

This document describes how to reduce manual verification pain for Intelligent Terminal release sign-off. It complements `doc\release-check-list.md`: the checklist is the human sign-off surface; this plan explains which checks can move into automated validation and what kind of automation should own them.

## Goals

- Reduce manual release verification time.
- Prefer deterministic local automation over fragile manual clicking.
- Keep the automation runnable on a fully prepared local machine; it does not need to run in CI/pipeline.
- Use logs, settings files, `wtcli`, and WTA protocol behavior as verification signals whenever possible.
- Reserve manual testing for visual quality, accessibility judgment, and real LLM response quality.

## Proposed automation layers

| Layer | Verification scope | Recommended owner |
|---|---|---|
| Unit tests | Pure logic: settings, policy, keybindings, slash commands, agent registry, session routing, autofix state reducers | Existing C++ TAEF unit tests and Rust `cargo test` |
| Headless local integration | `wta`, `wtcli`, package identity, COM activation, hooks status, logs, settings persistence | PowerShell runner or small native/Rust helper |
| Local UI automation | FRE, Settings page, keyboard shortcuts, agent pane show/hide, pane position, session/chat view switch | WindowsTerminal_UIATests / WinAppDriver / UIA |
| Mock ACP end-to-end | Agent pane chat, tool permission, insert/run, autofix prompt, model selection, restart/new/stop/clear/sessions | Fake ACP agent plus local runner |
| Manual verification | Visual polish, high contrast/RTL/scaling judgment, real agent answer quality, real install/auth flows | Human release checklist sign-off |

## Highest-value new test asset: fake ACP agent

Create a deterministic fake ACP agent executable/script that can be configured as a custom ACP agent in Settings, for example:

```jsonc
{
  "acpAgent": "custom:fake-acp-agent",
  "acpCustomCommand": "fake-acp-agent.exe --mode happy"
}
```

The fake agent should implement enough ACP behavior to validate Intelligent Terminal without calling a real LLM:

- `initialize` and `session/new` succeed.
- Normal prompt returns a fixed response such as `FAKE_AGENT_OK`.
- Model listing/selection returns deterministic model IDs.
- Tool/permission request emits a known command so Insert into pane / Run in pane can be verified.
- Autofix prompt returns a known fix such as `echo fixed`.
- Failure modes can be selected by flag: missing auth, protocol error, hang, disconnect, slow response.

This lets the release gate verify protocol, routing, UI state, and logs without depending on Copilot/Claude/Codex/Gemini availability or network/model behavior.

## Local release-validation runner

Add a local runner, for example:

```powershell
tools\release-validation\Invoke-ITReleaseValidation.ps1
```

Recommended responsibilities:

1. Prepare an isolated test profile/settings state.
2. Configure fake ACP agent and known settings.
3. Launch the packaged Intelligent Terminal build.
4. Drive UI actions through UIA/WinAppDriver or keyboard shortcuts.
5. Verify behavior through `wtcli`, settings files, process state, and WTA/Terminal logs.
6. Emit a machine-readable result file and a Markdown summary.

Suggested outputs:

```text
artifacts\release-validation\results.json
artifacts\release-validation\summary.md
artifacts\release-validation\logs.zip
```

## Checklist-to-automation mapping

| Release checklist area | Automation strategy | Manual remainder |
|---|---|---|
| FRE | UI automation for happy path, skip/close, toggles, agent selection, settings persistence, logs | Visual polish, localized wording, install UX edge cases |
| Settings > AI Agents | UI automation plus settings.json verification | Visual layout and accessibility judgment |
| Agent pane chat | Fake ACP E2E for open/hide/focus/chat/tool request | Real agent quality smoke |
| Slash commands | Rust unit tests for parse/intent; fake ACP E2E for dispatch | Visual polish of popups |
| Autofix | Unit tests for state/routing; fake ACP E2E for suggestion/insert/run | Real failure diagnosis quality |
| Session management | Rust unit tests for routing; fake/hook integration for rows and focus/resume | Human confirmation of UX clarity |
| Delegate shortcuts | UI automation for `Alt+Shift+B` and `Alt+Shift+/`; verify cwd/logs/process args | Real delegate agent quality smoke |
| Custom agents | Existing and expanded SettingsModel UT plus fake ACP E2E | None beyond one manual smoke if desired |
| Multi-pane/window | UI automation smoke for split panes, multiple tabs, moved tabs/windows; verify tab/pane IDs in logs | Complex drag UX and visual behavior |
| Hooks | Headless `wta hooks status/install/remove --json`; filesystem/log assertions | Real third-party CLI plugin UX edge cases |
| Packaging/protocol | Headless `wtcli list-panes`, `capture-pane`, `listen`, `send-keys`; package path/log assertions | None for protocol smoke |
| Diagnostics/logs | Headless filesystem assertions and bug-report zip inspection | Human log readability review |
| A11y/localization | UIA property checks and pseudo-locale smoke | Screen reader quality, high contrast, RTL visual review |

## Unit-test candidates

### Existing coverage to rely on or extend

- Custom agent ID and settings round-trip: `src\cascadia\ut_app\CustomAgentIdTests.cpp`, `src\cascadia\UnitTests_SettingsModel\CustomAgentAndPolicyTests.cpp`.
- Hooks status JSON/UI contract: `src\cascadia\ut_app\AgentHooksStatusTests.cpp`.
- Session Enter/Shift+Enter routing: `tools\wta\src\session_mgmt.rs`.
- WTA CLI parsing and session list formatting: `tools\wta\src\cli_tests.rs`.
- Slash command parsing: `tools\wta\src\commands.rs` and WTA slash-command tests.
- Agent registry command/model resolution: `tools\wta\src\agent_registry.rs`.

### New or expanded UT areas

- Default keybindings include:
  - `Alt+Shift+B` -> `openBackgroundAgent`
  - `Alt+Shift+/` -> command palette agent delegation
  - `Ctrl+Shift+.` -> `openAgentPane`
  - `Ctrl+Shift+/` -> `openAgentSessions`
- Autofix state reducer:
  - detection off
  - suggestion off
  - suggestion on
  - missing `tab_id`
  - busy turn drop/defer behavior
  - correct target tab/pane routing
- FRE/Settings view-model logic:
  - detection/suggestion dependency
  - session-management hint visibility
  - pane position persistence
  - policy lock states
- Agent registry:
  - Copilot/Claude/Codex/Gemini command construction
  - model flag behavior
  - custom command passthrough
  - missing CLI/setup metadata
- Session registry:
  - Live/Working/Attention/Ended/Historical transitions
  - MVP origin filter behavior
  - custom-agent not-resumable behavior

## Headless checks

Run these after launching the packaged build:

- `wtcli list-panes` returns panes.
- `wtcli capture-pane` returns current pane text.
- `wtcli send-keys` can type known text into the active pane.
- `wtcli listen` receives relevant events.
- `wta sessions list --json` returns valid JSON lines.
- `wta hooks status --json` returns supported schema.
- WTA logs are created under the expected package-private log directory.
- `terminal-agent-pane.log` and hook logs appear when relevant.
- The packaged `wta.exe` path is used instead of a stale dev binary.

## UI automation checks

Recommended smoke path:

1. Start packaged Intelligent Terminal with isolated settings.
2. Complete or skip FRE.
3. Open Settings with `Ctrl+,`.
4. Navigate to AI Agents.
5. Configure fake custom ACP agent.
6. Set each agent pane position and open/hide the agent pane with `Ctrl+Shift+.`.
7. Send a prompt and verify `FAKE_AGENT_OK` appears.
8. Run `/sessions`, `/new`, `/clear`, `/restart`, `/stop`, and `/model`.
9. Trigger a known shell failure and verify autofix state/logs.
10. Use Insert into pane and Run in pane for a fake tool suggestion.
11. Exercise `Alt+Shift+B` and `Alt+Shift+/` delegate entry points.
12. Create split panes/tabs and verify target pane routing with `wtcli`.

## Recommended implementation order

1. Add UT coverage for default shortcuts, agent registry, autofix reducer, and session transitions.
2. Build `fake-acp-agent.exe`.
3. Add `Invoke-ITReleaseValidation.ps1` that configures fake agent and runs headless checks.
4. Extend existing WindowsTerminal UIA tests for Settings, agent pane open/hide, and command shortcuts.
5. Add fake ACP E2E scenarios for chat, autofix, and insert/run.
6. Add optional real-agent smoke tests for Copilot, Claude, Codex, and Gemini.

## Expected outcome

Most release sign-off items become automatic pass/fail checks with logs attached. Manual release verification should focus on:

- real LLM quality,
- install/auth experience,
- accessibility judgment,
- visual polish,
- high contrast / scaling / RTL review,
- complex multi-window drag behavior.
