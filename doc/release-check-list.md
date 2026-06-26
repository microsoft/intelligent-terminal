# Release Check List

Use this checklist to validate and sign off an Intelligent Terminal release. Each test item should be checked only after the expected behavior is confirmed on the release build.

**Coverage markers:**

- `[UT✓]` — already covered by an existing unit test.
- `[UT+]` — UT-coverable; test not written yet (recommended to add).
- `[UT~]` — partially UT-coverable: decision/logic core can be unit-tested, full behavior still needs E2E/UI.
- `[E2E]` — needs mock-ACP end-to-end or UI automation; not a UT.
- `[MANUAL]` — human judgment (visual polish, real LLM quality, install/auth UX).

> **Checkbox semantics:** a ticked `- [x]` box means the item is fully verified by an automated unit test (pure `[UT✓]` items). Items tagged `[UT✓]` *and* `[E2E]`/`[MANUAL]` keep the `[UT✓]` marker to show the logic core is unit-tested, but stay unchecked because release sign-off still needs the E2E / manual portion.

## How to use this checklist for testing

Read the markers to decide where to spend manual effort — don't re-test what the unit tests already lock down:

- **`[x]` pure `[UT✓]`** — the logic is fully verified by a unit test that re-runs on every build. Do **not** manually test these in isolation; just let them ride along in the final end-to-end smoke pass. (Examples: slash-command dispatch, autofix on/off gating, settings persistence.)
- **`[UT✓]` + `[E2E]` (box left unchecked)** — the **decision/logic half is already UT-covered**, so during E2E you only need to confirm the **UI / interaction half** works (the pane actually opens, the row actually shows the state, the picker renders). You do **not** need to re-verify the underlying branches — those are guarded by UT and regress automatically. (Examples: `Ctrl+Shift+.` opens the pane, session-state display, Enter/Shift+Enter resume, `/model` picker.)
- **`[E2E]` / `[MANUAL]`** — no UT safety net; test these fully by hand / automation.

Net effect: UT shrinks the manual matrix to "did the wiring and UI connect", not "is every logic branch correct". The final gate is one end-to-end run over the `[E2E]`/`[MANUAL]` surface plus a smoke pass that exercises the `[UT✓]` paths in a real build.

## Release sign-off metadata

- [ ] `[MANUAL]` **Build under test:** Version/build number is recorded.
- [ ] `[MANUAL]` **Package type:** Packaged MSIX / Store package / local installer is recorded.
- [ ] `[MANUAL]` **OS matrix:** Windows 10 and Windows 11 coverage is recorded if this release targets both.
- [ ] `[MANUAL]` **Tester:** Primary tester and sign-off owner are recorded.
- [ ] `[MANUAL]` **Agent CLI versions:** Copilot, Claude, Codex, Gemini, and any custom agent versions are recorded.
- [ ] `[MANUAL]` **Known limitations:** Expected limitations are written down before sign-off.

## 0. First-run experience (FRE)

**Feature definition:** FRE guides first-time users through agent selection, pane position, automatic error detection, automatic error suggestion, and session-management hook setup.

- [ ] `[E2E]` **FRE opens correctly:** A clean user profile launches the FRE instead of skipping directly to the terminal.
- [ ] `[E2E]` **FRE can be completed:** The user can go through every page, save settings, and enter the main terminal window.
- [ ] `[E2E]` **FRE can be skipped or closed safely:** Skipping/closing does not crash and leaves settings in a valid state.
- [ ] `[E2E]` **FRE privacy / help links work:** Links open the browser and do not block completion.
- [ ] `[E2E]` **FRE save progress works:** The progress UI appears while setup/install work is running and returns to a usable state.
- [ ] `[UT~]` `[E2E]` **FRE error messages are actionable:** Install/auth/setup failures show a useful message instead of a silent failure or raw OS error. _(UT: WTA `classify_acp_error`.)_
- [ ] `[UT~]` `[E2E]` **FRE respects policy locks:** If agent, autofix, or session-management policy is locked, affected controls are disabled and explain why. _(UT: `IsAgentPolicyLocked`, Effective* gates.)_
- [ ] `[UT~]` `[MANUAL]` **FRE RTL/localized layout is usable:** Layout mirrors correctly for RTL locales and text is not clipped in localized builds. _(UT: `IsRtlLocale`.)_

### FRE agent selection

- [ ] `[UT~]` `[E2E]` **Copilot without install:** Copilot appears as an available/default choice, is labeled as needing install, and the setup path installs or clearly explains how to install it. _(UT: registry/policy filter.)_
- [ ] `[UT~]` `[E2E]` **Copilot preinstalled:** Copilot appears as installed; saving does not reinstall unnecessarily; opening the agent pane uses Copilot successfully.
- [ ] `[UT~]` `[E2E]` **Non-Copilot agents appear when installed:** Claude/Codex/Gemini appear as selectable only when installed; selecting one saves correctly and can connect in agent-pane mode; Node/npx requirement guidance appears when relevant.
- [ ] `[UT~]` `[E2E]` **Unavailable non-Copilot agents:** Claude/Codex/Gemini that are not installed do not appear as broken selectable options.
- [ ] `[UT✓]` `[E2E]` **Agent selection persists:** The selected agent remains selected after FRE completion and app restart. _(UT: `BuiltInAcpAgentRoundtrips`.)_

### FRE automatic error settings

- [ ] `[UT✓]` `[E2E]` **Automatic error detection off:** Turning detection off disables error-event monitoring behavior and disables dependent suggestion UI. _(UT: `EffectiveAutoFixEnabled` + autofix reducer gate.)_
- [ ] `[UT✓]` `[E2E]` **Automatic error detection on:** Turning detection on enables shell failure detection when shell integration is available.
- [ ] `[UT✓]` `[E2E]` **Automatic error suggestion off:** Detection can remain on while LLM-powered suggestions are off; failures do not trigger an agent suggestion. _(UT: autofix reducer no-LLM path.)_
- [ ] `[UT✓]` `[E2E]` **Automatic error suggestion on:** With detection on and suggestion on, failures can trigger autofix suggestions.
- [ ] `[UT✓]` `[E2E]` **Detection/suggestion dependency:** Suggestion cannot be enabled when detection is off; the UI state is visually clear. _(UT: `EffectiveAutoFixFalseWhenDetectionOff`.)_
- [ ] `[UT✓]` `[E2E]` **Settings persist:** Detection and suggestion choices persist after restart. _(UT: `AutoErrorSettingsRoundtrip`.)_

### FRE session management

- [ ] `[UT~]` `[E2E]` **Session management off:** Turning it off does not install hooks and session UI remains stable.
- [ ] `[E2E]` **Session management on:** Turning it on installs or updates agent hooks where supported.
- [ ] `[E2E]` **Session hook hints:** Informational hint rows appear only when the owning toggle is on.
- [ ] `[UT~]` `[E2E]` **Hook install failure:** Missing CLI, disabled plugin, or partial install states show a useful message and do not block FRE completion. _(UT: `wta hooks status --json` parse.)_
- [ ] `[UT~]` `[E2E]` **Session-management choice persists:** The choice is reflected later in Settings. _(UT: `AgentHooksStatusTests` parses the read-back state; the toggle installs hooks on Save rather than persisting a settings bool, so the persistence itself is E2E.)_

### FRE agent pane position

- [ ] `[E2E]` **Bottom:** Agent pane opens at the bottom.
- [ ] `[E2E]` **Right:** Agent pane opens on the right.
- [ ] `[E2E]` **Left:** Agent pane opens on the left.
- [ ] `[E2E]` **Top:** Agent pane opens at the top.
- [ ] `[UT✓]` `[E2E]` **Position persists:** The selected position remains after restart and is used by the hotkey/button. _(UT: `AgentPanePositionRoundtripsAndDefaults`.)_

## 1. Settings > AI Agents

**Feature definition:** Settings is the post-FRE configuration surface for built-in agents, custom agents, model selection, pane position, autofix, and session hooks.

- [ ] `[E2E]` **AI Agents page opens:** Settings opens the AI Agents page without layout glitches.
- [ ] `[UT~]` `[E2E]` **Built-in agent dropdown works:** Copilot, Claude, Codex, and Gemini entries show correct installed/available state. _(UT: registry/filter logic.)_
- [ ] `[UT✓]` `[E2E]` **Agent pane agent save works:** Changing the agent pane provider updates future agent panes. _(UT: `BuiltInAcpAgentRoundtrips` + custom round-trip.)_
- [ ] `[UT✓]` `[E2E]` **Delegate agent save works:** Changing the delegate provider updates future delegate launches. _(UT: `BuiltInDelegateAgentRoundtrips` + custom round-trip.)_
- [ ] `[UT~]` `[E2E]` **Model control appears:** Model picker/textbox appears when a selected agent supports or has a configured model.
- [ ] `[UT✓]` `[E2E]` **Model changes apply:** Changing `acpModel` affects new agent-pane sessions and does not corrupt existing settings. _(UT: `build_acp_command` model handling.)_
- [ ] `[UT✓]` `[E2E]` **Delegate model changes apply:** Changing `delegateModel` affects new delegate-agent launches. _(UT: command construction.)_
- [ ] `[UT✓]` `[E2E]` **Pane position setting works:** Bottom/right/left/top can be selected and saved. _(UT: `AgentPanePositionRoundtripsAndDefaults`.)_
- [ ] `[UT✓]` `[E2E]` **Automatic error detection setting works:** Toggling detection in Settings matches FRE behavior. _(UT: `AutoErrorSettingsRoundtrip`.)_
- [ ] `[UT✓]` `[E2E]` **Automatic error suggestion setting works:** Toggling suggestion in Settings matches FRE behavior. _(UT: `AutoErrorSettingsRoundtrip` + `EffectiveAutoFixFalseWhenDetectionOff`.)_
- [ ] `[UT~]` `[E2E]` **Session hooks install works:** Install hooks button detects supported CLIs and reports success/failure clearly. _(UT: status parse.)_
- [ ] `[E2E]` **Session hooks remove works:** Per-CLI remove buttons remove hook state without breaking the Settings page.
- [ ] `[UT~]` `[E2E]` **Policy lock UI works:** Locked controls are disabled and show the policy message. _(UT: Effective*/IsLocked gates.)_

## 2. Agent pane chat

**Feature definition:** The agent pane is a per-tab AI chat pane backed by WTA helper/master and an ACP-capable agent. It should be reusable, able to be hidden, and stable across tab/window operations.

> **Automated coverage for the agent pane:** a deterministic in-process
> **mock-ACP agent** harness (`tools/wta/src/protocol/acp/mock_agent_tests.rs`) drives the
> **real** `WtaClient` against scripted agent behavior, and a **TestBackend
> render harness** (`tools/wta/src/app.rs::tests::render_to_text`) asserts what the TUI actually
> paints. Together they UT-lock the *display/logic half* of streaming output,
> tool-call/plan cards, and the permission flow (see the `[UT✓]` tags below).
> This work also surfaced and fixed **3 real bugs**: the permission `y`/`n`
> quick-keys never matched (PascalCase vs lowercase), the streaming JSON
> extractor dropped emoji (UTF-16 surrogate pairs), and it bailed when the
> field name appeared earlier as a value.

### Opening, hiding, and focus

- [ ] `[E2E]` **Button opens pane:** The AI assistant button opens the agent pane.
- [ ] `[UT✓]` `[E2E]` **Hotkey opens pane:** `Ctrl+Shift+.` opens the agent pane. _(UT: `DefaultAgentKeybindings` binding; open behavior E2E.)_
- [ ] `[E2E]` **Button hides pane:** The button hides/stashes the agent pane without killing the session.
- [ ] `[E2E]` **Hotkey hides pane:** `Ctrl+Shift+.` hides/stashes the agent pane without killing the session.
- [ ] `[UT✓]` `[E2E]` **Focus hotkey works:** `Ctrl+Shift+I` focuses the agent pane when available. _(UT: `DefaultAgentKeybindings` binding; focus behavior E2E.)_
- [ ] `[E2E]` **Different positions work:** Open/hide/focus works for bottom, right, left, and top pane positions.
- [ ] `[E2E]` **Stash preserves chat:** Hiding and restoring the pane preserves helper process, connection state, and chat history.
- [ ] `[E2E]` **Tab close cleans up:** Closing the owning tab cleans up the helper and does not leave a broken pane.

### Built-in agent chat matrix

- [ ] `[E2E]` `[MANUAL]` **Copilot chat works:** User can send a prompt and Copilot responds successfully.
- [ ] `[UT~]` `[E2E]` **Copilot missing CLI path works:** Missing Copilot shows actionable setup/auth guidance, not a silent failure. _(UT: registry install hint.)_
- [ ] `[E2E]` **Non-Copilot agents chat works:** Each installed+authenticated non-Copilot built-in agent (Claude/Codex/Gemini) connects through its ACP adapter and answers a prompt. _(One consolidated matrix case — all built-in agents share the same agent-pane/ACP path, so per-agent behavioural depth is covered by the Copilot suites.)_
- [ ] `[UT~]` `[E2E]` **Agent auth failure works:** Unauthenticated agents show clear login guidance and can recover after sign-in. _(UT: `AgentFailure::AuthRequired` classification; in-pane auth screen render via `render_auth_screen_shows_agent_name` / `render_auth_sign_in_card` / `render_auth_checking_with_status_message`.)_
- [ ] `[E2E]` **Agent restart after settings change works:** Changing the selected agent or model restarts/reconnects cleanly.

### Input and rendering

- [ ] `[E2E]` **Prompt focused appearance is correct:** Input box looks correct when focused.
- [ ] `[E2E]` **Prompt out-of-focus appearance is correct:** Input box looks correct when focus leaves the agent pane.
- [ ] `[E2E]` **Typing works:** User can type, edit, and submit prompt text correctly.
- [ ] `[E2E]` **Paste works:** Pasted multi-line text is handled correctly.
- [ ] `[E2E]` **Keyboard navigation works:** Arrow keys, Tab completion, Ctrl combinations, and Esc behave correctly.
- [ ] `[E2E]` `[MANUAL]` **IME/non-ASCII input works:** IME and non-ASCII input are usable if the release supports localized typing.
- [ ] `[UT✓]` `[E2E]` **Streaming output renders correctly:** Agent response chunks, tool calls, plans, and status lines render without corruption. _(UT: `streaming_two_chunks_coalesce_in_app_chat`, `tool_call_surfaces_card_in_chat`, `tool_call_completion_updates_card_status` (in-place, no dup), `plan_surfaces_card_in_chat`, `render_chat_all_message_variants`; streaming-JSON unwrap incl. emoji/surrogate pairs in `ui::chat::tests`.)_
- [ ] `[UT✓]` `[E2E]` **Permission UI works:** When the agent requests a command/tool permission, the user can allow or reject it. _(UT: `permission_allow_round_trips_to_agent`, `permission_reject_round_trips_to_agent`, `permission_quick_allow/reject_key_round_trips_to_agent`, `render_permission_card_shows_options`, `render_permission_compact_shows_hint`; the `y`/`n` quick-key case-match bug was fixed here.)_
- [ ] `[E2E]` **Insert into pane works:** Agent-proposed command/text can be inserted into the target terminal pane without running.
- [ ] `[E2E]` **Run in pane works:** Agent-proposed command can be run in the target terminal pane.
- [ ] `[E2E]` **Command target is correct:** Insert/run applies to the intended active pane, not the agent pane itself or another tab.

### Agent pane slash commands

- [x] `[UT✓]` **`/help` works:** Shows available commands.
- [x] `[UT✓]` **`/clear` works:** Clears chat view as expected without breaking the session.
- [x] `[UT✓]` **`/new` works:** Starts a fresh session.
- [x] `[UT✓]` **`/fix` works:** Runs manual autofix using recent terminal context. _(UT: classify + `slash_fix_when_idle_submits_autofix_turn` / `slash_fix_while_busy_does_not_resubmit`.)_
- [x] `[UT✓]` **`/restart` works:** Restarts the agent stack and reconnects to a clean session. _(UT: `slash_restart_resets_connection_and_clears_sessions`.)_
- [x] `[UT✓]` **`/stop` works:** Stops/cancels an in-progress turn.
- [x] `[UT✓]` **`/sessions` works:** Switches to session-management view. _(UT: `slash_sessions_opens_agents_view`.)_
- [ ] `[UT✓]` `[E2E]` **`/model` works:** Opens/selects model where supported; unsupported agents fail gracefully. _(UT: `slash_model_*`; picker render covered by `render_model_picker_lists_models`, full UI flow still E2E.)_
- [x] `[UT✓]` **Unknown slash command is safe:** Unknown `/command` does not lose user input or crash.
- [ ] `[E2E]` **Esc/back navigation works:** User can return from popups/session/model views to chat.

### Chat/session view switching

- [ ] `[UT✓]` `[E2E]` **Session view opens from chat:** `/sessions`, session button, or `Ctrl+Shift+/` opens the session view. _(UT: `slash_sessions_opens_agents_view` + `DefaultAgentKeybindings`.)_
- [ ] `[E2E]` **Chat view restores:** User can return to chat view after opening session view.
- [ ] `[E2E]` **View switch preserves input:** Draft prompt text is not unexpectedly lost when switching views.
- [ ] `[E2E]` **View switch preserves connection:** Agent connection state remains correct after switching views.

## 3. Autofix flow

**Feature definition:** Autofix detects terminal command failures, captures relevant pane context, asks the configured agent for a fix, and lets the user insert or run the suggested command.

### Shell integration and detection

- [ ] `[E2E]` **PowerShell shell integration installed:** Supported PowerShell profiles emit command-finished events.
- [ ] `[E2E]` **Missing shell integration is safe:** Without shell integration, failures do not crash or produce broken UI.
- [x] `[UT✓]` **Failure detection works:** A failing command emits an event and is detected by Intelligent Terminal. _(UT: `classify_wt_event`.)_
- [x] `[UT✓]` **Successful commands ignored:** Successful commands do not trigger autofix. _(UT: `classify_wt_event` + `success_exit_code_does_not_arm_autofix`.)_
- [x] `[UT✓]` **Detection off suppresses autofix:** With automatic error detection off, failures do not trigger autofix. _(UT: autofix reducer.)_
- [x] `[UT✓]` **Detection on observes failures:** With detection on, failure notifications are observed. _(UT: autofix reducer.)_
- [x] `[UT✓]` **Suggestion off suppresses LLM call:** With suggestion off, detection can show any expected local UI but does not ask the agent for a fix. _(UT: `suggestion_off_emits_detected_without_submitting_turn`.)_
- [x] `[UT✓]` **Suggestion on triggers LLM call:** With suggestion on and a connected helper, an autofix suggestion is requested. _(UT: reducer submit path.)_
- [x] `[UT✓]` **Cold-start behavior is acceptable:** If failure happens before the helper is connected, UI stays stable and no stale suggestion appears later. _(UT: `cold_start_drops_autofix_when_not_connected`.)_

### Autofix with agent pane

- [ ] `[E2E]` **Visible agent pane autofix works:** Autofix works when the agent pane is visible.
- [ ] `[E2E]` **Stashed agent pane autofix works:** Autofix works when the per-tab agent pane is pre-warmed but hidden.
- [ ] `[E2E]` **Autofix opens/restores UI correctly:** Suggestion UI appears in the expected pane/tab and does not steal unrelated focus unexpectedly.
- [ ] `[E2E]` **Insert suggestion works:** Suggested fix can be inserted into the source pane.
- [ ] `[E2E]` **Run suggestion works:** Suggested fix can be run in the source pane.
- [ ] `[UT✓]` `[E2E]` **Reject/dismiss works:** User can dismiss an autofix suggestion without side effects. _(UT: `trigger_echo_pane_clears_when_state_returns_to_idle`.)_
- [ ] `[UT✓]` `[E2E]` **Autofix target pane is correct:** Failure in one pane does not offer/run a fix in the wrong pane. _(UT: target-tab routing — busy-pane tests + `autofix_still_triggers_for_non_agent_pane`.)_
- [ ] `[E2E]` `[MANUAL]` **Autofix with Copilot works:** Copilot returns a useful suggestion.
- [ ] `[E2E]` **Autofix with non-Copilot agents works:** Autofix produces a usable suggestion with a non-Copilot built-in agent (Claude/Codex/Gemini) and a custom ACP agent — same path as Copilot, covered once across the available agents.

### Autofix across layout changes

- [ ] `[UT~]` `[E2E]` **Split pane autofix works:** Failure in a split pane is routed to the correct tab/pane. _(UT: tab/pane routing.)_
- [ ] `[UT~]` `[E2E]` **Moved tab autofix works:** After moving a tab to another window, failures route to the correct agent pane. _(UT: tab_id routing.)_
- [ ] `[UT~]` `[E2E]` **Multi-window autofix works:** Multiple windows with agent panes do not cross-route suggestions. _(UT: window_id filter.)_
- [ ] `[UT~]` `[E2E]` **Closed pane cleanup works:** Autofix does not target a pane that has already closed.

## 4. Session management

**Feature definition:** Session management lists known live and historical agent sessions, shows their state, and lets users focus or resume supported sessions.

### Surfaces

- [ ] `[E2E]` **Session button works:** The session-management button opens the session view.
- [ ] `[UT✓]` `[E2E]` **Hotkey works:** `Ctrl+Shift+/` opens the session view. _(UT: `DefaultAgentKeybindings` binding; open behavior E2E.)_
- [ ] `[UT✓]` `[E2E]` **Slash command works:** `/sessions` opens the session view. _(UT: `/sessions` classify.)_
- [ ] `[UT✓]` `[E2E]` **Command action works:** The `openAgentSessions` action opens the session view. _(UT: `AgentActionsParse` verifies the action parses; opening the view is E2E.)_
- [ ] `[UT✓]` `[E2E]` **Session view empty state works:** Empty/no-session state is useful and not visually broken. _(UT: `render_sessions_view_shows_footer_hint` paints the agents-view chrome/footer with an empty registry; live data still E2E.)_
- [ ] `[E2E]` **Session view refresh works:** Newly created sessions appear without restarting Terminal when hooks are active.

### Session states

- [ ] `[UT✓]` `[E2E]` **Active/Live state is correct:** A currently reachable session is shown as active/live and can be focused. _(UT: `agent_sessions` liveness.)_
- [ ] `[UT✓]` `[E2E]` **Running/Working state is correct:** A session running a tool or long operation shows running/working state. _(UT: activity state.)_
- [ ] `[UT✓]` `[E2E]` **Waiting-for-input state is correct:** A session waiting for user input/attention shows the waiting/attention state. _(UT: Attention activity.)_
- [ ] `[UT✓]` `[E2E]` **Idle state is correct:** A live session waiting for the next prompt shows idle/ready state.
- [ ] `[UT✓]` `[E2E]` **Ended state is correct:** A session whose pane was closed becomes ended and does not stay falsely live. _(UT: PaneClosed tombstone.)_
- [ ] `[UT✓]` `[E2E]` **Historical state is correct:** On-disk sessions show as historical when not live.
- [ ] `[UT✓]` `[E2E]` **State transitions are correct:** Live -> ended, historical -> live, and working -> idle transitions update without duplicate/stale rows. _(UT: `apply_alive_session_join` / `apply_master_session_ended`.)_

### Focus and restore

- [ ] `[UT✓]` `[E2E]` **Focus active session:** Selecting an active session navigates/focuses the existing pane. _(UT: `decide_enter_action` Focus.)_
- [ ] `[UT✓]` `[E2E]` **Focus active stashed agent pane:** Selecting an active stashed agent-pane session restores/focuses the pane if applicable.
- [ ] `[UT✓]` `[E2E]` **Restore old session:** Selecting a supported old session resumes it successfully.
- [ ] `[UT✓]` `[E2E]` **Restore old shell-pane session:** Supported shell-pane sessions resume through the CLI resume path. _(UT: `ResumeCliFlag` decision.)_
- [ ] `[UT✓]` `[E2E]` **Restore old agent-pane session:** Supported agent-pane sessions resume through agent-pane/session-load path when enabled. _(UT: `ResumeInAgentPane` decision.)_
- [ ] `[UT✓]` `[E2E]` **Unsupported restore is clear:** Unknown CLI, missing resume support, or missing on-disk session shows a clear not-resumable message. _(UT: `NotResumable` reasons.)_
- [ ] `[UT✓]` `[E2E]` **Enter behavior works:** Enter performs the expected focus/resume action.
- [ ] `[UT✓]` **Shift+Enter behavior works:** Shift+Enter performs the alternate resume path for dead sessions and same focus path for live sessions. _(UT: `decide_enter_action` shift + `shift_enter_on_class_a_live_row_focuses`. Not E2E: Live-row Shift+Enter dispatches FocusPane — moves WT focus to the session's pane, it does NOT dismiss the view — and the MVP picker shows only Class B shell sessions whose panes are usually already closed, so there is no stable E2E observable.)_

### Session-management scope and custom agents

- [ ] `[UT✓]` `[E2E]` **Built-in agents tracked:** Copilot, Claude, Codex, and Gemini sessions are tracked when hooks/session support is enabled. _(UT: cli_source/origin.)_
- [ ] `[UT✓]` `[E2E]` **Custom agent safe behavior:** Custom agents do not crash session management and do not show strange/broken UI. _(UT: `NotResumable` UnknownCli.)_
- [x] `[UT✓]` **Custom agent limitation is acceptable:** Session management is not expected to fully restore custom-agent sessions unless the custom agent provides compatible session metadata.
- [x] `[UT✓]` **MVP origin filter is understood:** If the release keeps the MVP filter, the picker shows shell-pane sessions only while debug/CLI listing can still inspect all origins. _(UT: `OriginFilter` + cli_tests.)_
- [ ] `[UT~]` `[E2E]` **Hooks off behavior is safe:** With session management off, missing rows are expected and UI remains stable.

## 5. Delegate agent and command palette shortcuts

**Feature definition:** Delegate mode launches a separate agent task from the current terminal context/cwd, without using the interactive agent pane chat.

- [ ] `[UT✓]` `[E2E]` **`Alt+Shift+B` launches background delegate:** Shortcut opens a new delegate agent/task. _(UT: `DefaultAgentKeybindings` binding; launch E2E.)_
- [ ] `[UT~]` `[E2E]` **Delegate cwd is correct:** The delegate starts with the current pane's working directory.
- [ ] `[UT✓]` `[E2E]` **Delegate provider is correct:** The launched delegate uses the configured delegate agent, not the agent-pane provider unless they are intentionally the same. _(UT: `EffectiveDelegateAgent`.)_
- [ ] `[UT✓]` `[E2E]` **Delegate model is correct:** The launched delegate uses the configured delegate model. _(UT: command construction.)_
- [ ] `[UT✓]` `[E2E]` **`Alt+Shift+/` opens agent delegation palette:** Shortcut opens command palette in agent-delegation mode. _(UT: `DefaultAgentKeybindings` binding; palette E2E.)_
- [ ] `[E2E]` **Command palette prompt launches delegate:** Typing a request and pressing Enter creates a delegate task.
- [ ] `[E2E]` **Command palette cancel is safe:** Esc/cancel closes the palette without launching a delegate.
- [ ] `[E2E]` `[MANUAL]` **Delegate with Copilot works:** Copilot delegate task starts and responds.
- [ ] `[E2E]` `[MANUAL]` **Delegate with non-Copilot agents works:** Claude/Codex/Gemini delegate tasks start and respond where supported by delegate mode.
- [ ] `[UT~]` `[E2E]` **Delegate errors are actionable:** Missing CLI/auth errors are clear.

## 6. Custom agents

**Feature definition:** Settings can configure one custom command for the agent pane and one custom command for delegate mode. Custom agents are not configured from FRE.

### Custom agent pane

- [ ] `[E2E]` **Custom agent is Settings-only:** FRE does not expose custom-agent creation.
- [ ] `[UT✓]` `[E2E]` **Add custom ACP agent:** In Settings, add an agent-pane custom command such as `qwen.cmd --acp`. _(UT: `DeriveCustomAgentId`.)_
- [x] `[UT✓]` **Save custom ACP agent:** Saving persists `custom:<cmd>`/custom command settings. _(UT: CustomAgentAndPolicyTests round-trip.)_
- [ ] `[UT✓]` `[E2E]` **Edit custom ACP agent:** Editing updates the command used by new agent panes.
- [ ] `[UT~]` `[E2E]` **Delete custom ACP agent:** Deleting returns to a valid built-in/default selection.
- [ ] `[UT~]` `[E2E]` **Model selection visible:** Model picker/textbox remains visible when custom agent is selected.
- [ ] `[E2E]` **Custom agent runs the standard agent-pane behaviours:** A configured custom ACP agent can chat, request a command/tool action, insert/run into the pane, and drive autofix — the same agent-pane behaviours verified in depth with Copilot.
- [ ] `[UT~]` `[E2E]` **Custom failure is safe:** Bad command, missing executable, or non-ACP behavior shows a clear error and does not crash Terminal. _(UT: failure classification.)_

### Custom delegate agent

- [ ] `[UT✓]` `[E2E]` **Add custom delegate agent:** In Settings, add a delegate custom command such as `qwen.cmd`. _(UT: `DeriveCustomAgentId`.)_
- [x] `[UT✓]` **Save custom delegate agent:** Saving persists the delegate custom command. _(UT: round-trip.)_
- [ ] `[UT✓]` `[E2E]` **`Alt+Shift+B` uses custom delegate:** Background delegate shortcut launches the custom command. _(UT: `DefaultAgentKeybindings` binding + custom `EffectiveDelegateAgent` resolution.)_
- [ ] `[UT✓]` `[E2E]` **`Alt+Shift+/` uses custom delegate:** Agent-delegation command palette launches the custom command. _(UT: `DefaultAgentKeybindings` + `AgentActionsParse` delegation mode.)_
- [ ] `[UT~]` `[E2E]` **Custom delegate cwd is correct:** Custom delegate starts in the source pane's cwd.
- [ ] `[UT~]` `[E2E]` **Custom delegate errors are clear:** Bad command or auth/setup failure is actionable.

## 7. Multi-pane and multi-window behavior

**Feature definition:** Agent state, session routing, and autofix routing are per-tab and per-window. Moving tabs/windows should not lose or cross-route agent context.

- [ ] `[E2E]` **Split pane does not break chat:** Splitting the terminal pane keeps agent pane chat usable.
- [ ] `[UT~]` `[E2E]` **Split pane target selection is correct:** Agent insert/run/autofix targets the intended non-agent pane. _(UT: routing core.)_
- [ ] `[UT~]` `[E2E]` **Multiple tabs work:** Each tab has its own agent pane/session state. _(UT: per-tab state.)_
- [ ] `[E2E]` **Multiple agent panes work:** Opening agent panes in multiple tabs does not mix conversations.
- [ ] `[E2E]` **Move tab to new window preserves chat:** Dragging/tearing a tab to another window preserves agent pane state.
- [ ] `[UT~]` `[E2E]` **Move tab to new window preserves session routing:** Session events remain associated with the moved tab. _(UT: tab_id routing.)_
- [ ] `[UT~]` `[E2E]` **Move tab to new window preserves autofix:** Autofix still routes to the moved tab/pane.
- [ ] `[UT~]` `[E2E]` **Multiple windows do not cross-route:** Events from one window do not mutate another window's agent pane/session UI. _(UT: window_id filter.)_
- [ ] `[E2E]` **Close source window is safe:** Closing a source window after moving a tab does not kill the moved tab's agent state.
- [ ] `[E2E]` **Close target tab cleans up:** Closing moved tabs cleans up helper/session state without affecting other tabs.

## 8. Agent hooks and session tracking

**Feature definition:** Agent hooks record shell-pane agent sessions and enable session-management state for supported CLIs.

- [ ] `[E2E]` **Install hooks from FRE works:** Session-management toggle can install supported hooks during first run.
- [ ] `[E2E]` **Install hooks from Settings works:** Install hooks button works after FRE.
- [ ] `[E2E]` **Per-CLI hook install works:** Each supported CLI (Copilot/Claude/Gemini) installs its hook or reports why it can't; Codex hook/session support behaves per the current implementation.
- [ ] `[E2E]` **Hook remove works:** Removing a hook disables future session tracking for that CLI.
- [ ] `[UT✓]` `[E2E]` **Disabled plugin is respected:** Disabled agent plugin is skipped and not force-enabled. _(UT: `decide_skip_when_disabled`.)_
- [ ] `[UT✓]` `[E2E]` **Hook auto-upgrade works:** After package upgrade, previously installed hooks are updated silently when bundle version changes. _(UT: `decide_upgrade` + `upgrade_state` round-trip.)_
- [ ] `[UT✓]` `[E2E]` **Opt-in preserved:** Auto-upgrade does not install hooks into a CLI the user never opted into. _(UT: `decide_skip_when_not_installed`.)_
- [ ] `[E2E]` **Hook logs are available:** Hook decisions and failures are visible in the expected WTA log files.

## 9. Packaging, process, and protocol integration

**Feature definition:** Packaged Intelligent Terminal includes WTA/wtcli integration and uses the packaged COM protocol server correctly.

- [ ] `[E2E]` **Packaged `wta.exe` is present:** WTA is deployed next to WindowsTerminal in the package layout.
- [ ] `[E2E]` **Packaged identity works:** WTA/wtcli can activate the Terminal protocol COM server from packaged context.
- [ ] `[E2E]` **Wrong unpackaged WTA is not used:** Agent pane/autofix does not accidentally use a stale dev-build WTA.
- [ ] `[E2E]` **`WT_COM_CLSID` is injected:** Shell panes and agent panes inherit protocol discovery environment as expected.
- [ ] `[E2E]` **`wtcli list-panes` works:** Basic WT protocol query succeeds from a pane.
- [ ] `[E2E]` **`wtcli capture-pane` works:** Pane output capture succeeds.
- [ ] `[E2E]` **`wtcli send-keys`/send input path works:** Insert/run operations can send input to the target pane.
- [ ] `[E2E]` **`wtcli listen` works:** Event subscription receives shell/agent events.
- [ ] `[E2E]` **WTA master starts:** One master process starts per Terminal process when needed.
- [ ] `[E2E]` **WTA helper starts per tab/pane:** Agent pane helper starts and connects to master.
- [ ] `[E2E]` **Master/helper crash recovery is acceptable:** Crashes or exits recover or surface an actionable error.

## 10. Diagnostics, logging, and supportability

**Feature definition:** Release builds should leave enough diagnostics for support without overwhelming the user.

- [ ] `[E2E]` **WTA logs are written:** WTA process logs are created in the expected package-private log directory.
- [ ] `[E2E]` **C++ agent pane log is written:** Terminal-side agent pane log is created.
- [ ] `[E2E]` **Hook trace log is written:** Hook events write to hook trace log when hooks are active.
- [ ] `[UT~]` `[E2E]` **Log version directory is correct:** Packaged builds write under the current package-version log directory. _(UT: `runtime_paths` resolution.)_
- [ ] `[UT~]` `[E2E]` **Old log cleanup is safe:** Starting the new build does not delete logs from the currently running version. _(UT: housekeeping prune logic.)_
- [ ] `[E2E]` **Bug report zip includes agent logs:** Diagnostic collection includes WTA, hook, and terminal-agent-pane logs.
- [ ] `[E2E]` **Release log level is reasonable:** Default release logging is not excessively noisy.
- [ ] `[E2E]` **Early startup failures are logged:** Failures before agent connection still land in logs.

## 11. Accessibility, localization, and UI polish

**Feature definition:** Intelligent Terminal AI features should be usable with keyboard, screen readers, localization, scaling, and theme changes.

- [ ] `[E2E]` **Keyboard-only FRE works:** FRE can be completed without a mouse.
- [ ] `[E2E]` **Keyboard-only Settings works:** AI Agents settings can be configured without a mouse.
- [ ] `[E2E]` **Keyboard-only agent pane works:** Chat, slash commands, popups, and session view are keyboard accessible.
- [ ] `[MANUAL]` **Narrator reads FRE controls:** FRE controls have useful names/help text.
- [ ] `[MANUAL]` **Narrator reads Settings controls:** AI Agents settings controls have useful names/help text.
- [ ] `[MANUAL]` **Narrator reads agent pane state:** Connection/status changes are understandable.
- [ ] `[MANUAL]` **High contrast theme works:** FRE, Settings, agent pane, and autofix UI remain readable.
- [ ] `[MANUAL]` **Light/dark theme works:** UI is readable in both themes.
- [ ] `[MANUAL]` **Text scaling works:** 125%, 150%, and 200% scaling do not clip critical controls.
- [ ] `[MANUAL]` **Localization strings are present:** New user-facing strings are localized or intentionally locked. _(WTA locale key-parity is enforced by a separate PR's `every_locale_has_all_en_us_keys` guard; .resw locales remain manual/pipeline.)_
- [ ] `[UT~]` `[MANUAL]` **Pseudo-locales work:** qps pseudo-locales do not clip or corrupt layout.
- [ ] `[UT~]` `[MANUAL]` **RTL works:** RTL layout is mirrored where expected. _(UT: `IsRtlLocale`.)_

## 12. Release decision

- [ ] `[MANUAL]` **All P0/P1 issues resolved:** No blocking agent pane, autofix, FRE, session, custom-agent, or packaging bugs remain.
- [ ] `[MANUAL]` **Known limitations documented:** Any intentionally deferred behavior is documented in release notes.
- [ ] `[E2E]` `[MANUAL]` **Upgrade path signed off:** Existing users upgrading from the previous release keep settings/hooks in a valid state.
- [ ] `[E2E]` `[MANUAL]` **Fresh install signed off:** New users can complete FRE and use the default agent flow.
- [ ] `[E2E]` `[MANUAL]` **Rollback/uninstall behavior signed off:** Uninstall or rollback leaves no user-blocking broken state.
- [ ] `[MANUAL]` **Final release owner sign-off:** Release owner approves shipping this build.

## Source notes used to build this checklist

- [ ] FRE, Settings, and policy behavior: `src\cascadia\TerminalApp\FreOverlay.cpp`, `src\cascadia\TerminalSettingsEditor\AIAgents.xaml`.
- [ ] Default actions and shortcuts: `src\cascadia\TerminalSettingsModel\defaults.json`.
- [ ] Built-in agent definitions: `tools\wta\src\agent_registry.rs`.
- [ ] Slash commands: `tools\wta\src\commands.rs`.
- [ ] Session state model: `tools\wta\src\agent_sessions.rs`, `tools\wta\AGENTS.md`.
- [ ] Multi-window agent pane architecture: `doc\specs\Multi-window-agent-pane.md`.
- [ ] Autofix flow and logging/runtime layout: `AGENTS.md`.
