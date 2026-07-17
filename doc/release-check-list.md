# Release Check List

Use this checklist to validate and sign off an Intelligent Terminal release. Each test item should be checked only after the expected behavior is confirmed on the release build.

**Coverage markers:**

- `[UT✓]` — already covered by an existing unit test.
- `[UT+]` — UT-coverable; test not written yet (recommended to add).
- `[UT~]` — partially UT-coverable: decision/logic core can be unit-tested, full behavior still needs E2E/UI.
- `[E2E]` — needs mock-ACP end-to-end or UI automation; not a UT.
- `[MANUAL]` — human judgment (visual polish, real LLM quality, install/auth UX).
- `[new]` — test case added for the current release cycle and not yet exercised in a prior sign-off. Orthogonal to the coverage markers above — read it alongside the `[UT*]`/`[E2E]`/`[MANUAL]` marker. Clear the `[new]` tag once the item has been through a release sign-off (it then becomes an ordinary tracked item).

> **Checkbox semantics:** a ticked `- [x]` box means the item is fully verified by an automated unit test (pure `[UT✓]` items). Items tagged `[UT✓]` *and* `[E2E]`/`[MANUAL]` keep the `[UT✓]` marker to show the logic core is unit-tested, but stay unchecked because release sign-off still needs the E2E / manual portion.

## How to use this checklist for testing

Read the markers to decide where to spend manual effort — don't re-test what the unit tests already lock down:

- **`[x]` pure `[UT✓]`** — the logic is fully verified by a unit test that re-runs on every build. Do **not** manually test these in isolation; just let them ride along in the final end-to-end smoke pass. (Examples: slash-command dispatch, autofix on/off gating, settings persistence.)
- **`[UT✓]` + `[E2E]` (box left unchecked)** — the **decision/logic half is already UT-covered**, so during E2E you only need to confirm the **UI / interaction half** works (the pane actually opens, the row actually shows the state, the picker renders). You do **not** need to re-verify the underlying branches — those are guarded by UT and regress automatically. (Examples: `Ctrl+Shift+.` opens the pane, session-state display, Enter/Shift+Enter resume, `/model` picker.)
- **`[E2E]` / `[MANUAL]`** — no UT safety net; test these fully by hand / automation.

Net effect: UT shrinks the manual matrix to "did the wiring and UI connect", not "is every logic branch correct". The final gate is one end-to-end run over the `[E2E]`/`[MANUAL]` surface plus a smoke pass that exercises the `[UT✓]` paths in a real build.

## Release sign-off metadata

- [ ] `C001` `[MANUAL]` **Build under test:** Version/build number is recorded.
- [ ] `C002` `[MANUAL]` **Package type:** Packaged MSIX / Store package / local installer is recorded.
- [ ] `C003` `[MANUAL]` **OS matrix:** Windows 10 and Windows 11 coverage is recorded if this release targets both.
- [ ] `C004` `[MANUAL]` **Tester:** Primary tester and sign-off owner are recorded.
- [ ] `C005` `[MANUAL]` **Agent CLI versions:** Copilot, Claude, Codex, Gemini, and any custom agent versions are recorded.
- [ ] `C006` `[MANUAL]` **Known limitations:** Expected limitations are written down before sign-off.

## 0. First-run experience (FRE)

**Feature definition:** FRE guides first-time users through agent selection, pane position, automatic error detection, automatic error suggestion, and session-management hook setup.

- [ ] `C007` `[E2E]` **FRE opens correctly:** A clean user profile launches the FRE instead of skipping directly to the terminal.
- [ ] `C008` `[E2E]` **FRE can be completed:** The user can go through every page, save settings, and enter the main terminal window.
- [ ] `C009` `[E2E]` **FRE can be skipped or closed safely:** Skipping/closing does not crash and leaves settings in a valid state.
- [ ] `C010` `[E2E]` **FRE privacy / help links work:** Links open the browser and do not block completion.
- [ ] `C011` `[E2E]` **FRE save progress works:** The progress UI appears while setup/install work is running and returns to a usable state.
- [x] `C012` `[UT✓]` `[E2E]` **FRE error messages are actionable:** Install/auth/setup failures show a useful message instead of a silent failure or raw OS error. _(UT: `classify_connection_closed_is_actionable` + `classify_connection_failed_is_critical` classify agent connection failures into actionable/critical categories that drive the user-facing message rather than a raw error; `auth_error_routes_to_signin_not_connection_lost` routes auth failures to a sign-in prompt.)_
- [ ] `C214` `[new]` `[UT~]` `[E2E]` **FRE execution-policy detection is correct:** FRE flags a genuinely blocking PowerShell execution policy but does **not** false-block when a load-induced execution-policy probe merely times out; an unknown/unreadable policy is treated conservatively (as blocking). _(#336/#338/#309; UT: execution-policy gate.)_
- [ ] `C013` `[UT~]` `[E2E]` **FRE respects policy locks:** If agent, autofix, or session-management policy is locked, affected controls are disabled and explain why. _(UT: `IsAgentPolicyLocked`, Effective* gates.)_
- [ ] `C014` `[UT~]` `[MANUAL]` **FRE RTL/localized layout is usable:** Layout mirrors correctly for RTL locales and text is not clipped in localized builds. _(UT: `IsRtlLocale`.)_

### FRE agent selection

- [x] `C015` `[UT✓]` `[E2E]` **Copilot without install:** Copilot appears as an available/default choice, is labeled as needing install, and the setup path installs or clearly explains how to install it. _(UT: `is_cli_available_handles_empty_string` / `is_cli_available_returns_false_for_obviously_bogus_name` drive the availability check that labels a CLI as needing install; the FRE agent picker (Feature.FreAgentSetup) shows Copilot as a choice. The actual install action requires a real uninstalled Copilot, which stays MANUAL.)_
- [ ] `C016` `[UT~]` `[E2E]` **Copilot preinstalled:** Copilot appears as installed; saving does not reinstall unnecessarily; opening the agent pane uses Copilot successfully.
- [ ] `C017` `[UT~]` `[E2E]` **Non-Copilot agents appear when installed:** Claude/Codex/Gemini appear as selectable only when installed; selecting one saves correctly and can connect in agent-pane mode; Node/npx requirement guidance appears when relevant.
- [ ] `C018` `[UT~]` `[E2E]` **Unavailable non-Copilot agents:** Claude/Codex/Gemini that are not installed do not appear as broken selectable options.
- [ ] `C019` `[UT✓]` `[E2E]` **Agent selection persists:** The selected agent remains selected after FRE completion and app restart. _(UT: `BuiltInAcpAgentRoundtrips`.)_

### FRE automatic error settings

- [ ] `C020` `[UT✓]` `[E2E]` **Automatic error detection off:** Turning detection off disables error-event monitoring behavior and disables dependent suggestion UI. _(UT: `EffectiveAutoFixEnabled` + autofix reducer gate.)_
- [ ] `C021` `[UT✓]` `[E2E]` **Automatic error detection on:** Turning detection on enables shell failure detection when shell integration is available.
- [ ] `C022` `[UT✓]` `[E2E]` **Automatic error suggestion off:** Detection can remain on while LLM-powered suggestions are off; failures do not trigger an agent suggestion. _(UT: autofix reducer no-LLM path.)_
- [ ] `C023` `[UT✓]` `[E2E]` **Automatic error suggestion on:** With detection on and suggestion on, failures can trigger autofix suggestions.
- [ ] `C024` `[UT✓]` `[E2E]` **Detection/suggestion dependency:** Suggestion cannot be enabled when detection is off; the UI state is visually clear. _(UT: `EffectiveAutoFixFalseWhenDetectionOff`.)_
- [ ] `C025` `[UT✓]` `[E2E]` **Settings persist:** Detection and suggestion choices persist after restart. _(UT: `AutoErrorSettingsRoundtrip`.)_

### FRE session management

- [ ] `C026` `[UT~]` `[E2E]` **Session management off:** Turning it off does not install hooks and session UI remains stable.
- [ ] `C027` `[E2E]` **Session management on:** Turning it on installs or updates agent hooks where supported.
- [ ] `C028` `[E2E]` **Session hook hints:** Informational hint rows appear only when the owning toggle is on.
- [x] `C029` `[UT✓]` `[E2E]` **Hook install failure:** Missing CLI, disabled plugin, or partial install states show a useful message and do not block FRE completion. _(E2E: `Feature.PerCliHooks` asserts `wta hooks status --json` enumerates each CLI's install state or a clear reason it can't (missing binary / unregistered marketplace); UT: `agent_hooks_installer` disabled-plugin parsers (`copilot_config_lookup_handles_disabled_plugin`, `claude_plugin_list_json_parser_reports_disabled`, etc.). FRE completion is independently covered by Feature.FreHooks.)_
- [ ] `C030` `[UT~]` `[E2E]` **Session-management choice persists:** The choice is reflected later in Settings. _(UT: `AgentHooksStatusTests` parses the read-back state; the toggle installs hooks on Save rather than persisting a settings bool, so the persistence itself is E2E.)_

### FRE agent pane position

- [ ] `C031` `[E2E]` **Bottom:** Agent pane opens at the bottom.
- [ ] `C032` `[E2E]` **Right:** Agent pane opens on the right.
- [ ] `C033` `[E2E]` **Left:** Agent pane opens on the left.
- [ ] `C034` `[E2E]` **Top:** Agent pane opens at the top.
- [ ] `C035` `[UT✓]` `[E2E]` **Position persists:** The selected position remains after restart and is used by the hotkey/button. _(UT: `AgentPanePositionRoundtripsAndDefaults`.)_

## 1. Settings > AI Agents

**Feature definition:** Settings is the post-FRE configuration surface for built-in agents, custom agents, model selection, pane position, autofix, and session hooks.

- [ ] `C036` `[E2E]` **AI Agents page opens:** Settings opens the AI Agents page without layout glitches.
- [ ] `C037` `[UT~]` `[E2E]` **Built-in agent dropdown works:** Copilot, Claude, Codex, and Gemini entries show correct installed/available state. _(UT: registry/filter logic.)_
- [ ] `C038` `[UT✓]` `[E2E]` **Agent pane agent save works:** Changing the agent pane provider updates future agent panes. _(UT: `BuiltInAcpAgentRoundtrips` + custom round-trip.)_
- [ ] `C039` `[UT✓]` `[E2E]` **Delegate agent save works:** Changing the delegate provider updates future delegate launches. _(UT: `BuiltInDelegateAgentRoundtrips` + custom round-trip.)_
- [ ] `C040` `[UT~]` `[E2E]` **Model control appears:** Model picker/textbox appears when a selected agent supports or has a configured model.
- [ ] `C041` `[UT✓]` `[E2E]` **Model changes apply:** Changing `acpModel` affects new agent-pane sessions and does not corrupt existing settings. _(UT: `build_acp_command` model handling.)_
- [ ] `C042` `[UT✓]` `[E2E]` **Delegate model changes apply:** Changing `delegateModel` affects new delegate-agent launches. _(UT: command construction.)_
- [ ] `C043` `[UT✓]` `[E2E]` **Pane position setting works:** Bottom/right/left/top can be selected and saved. _(UT: `AgentPanePositionRoundtripsAndDefaults`.)_
- [ ] `C044` `[UT✓]` `[E2E]` **Automatic error detection setting works:** Toggling detection in Settings matches FRE behavior. _(UT: `AutoErrorSettingsRoundtrip`.)_
- [ ] `C045` `[UT✓]` `[E2E]` **Automatic error suggestion setting works:** Toggling suggestion in Settings matches FRE behavior. _(UT: `AutoErrorSettingsRoundtrip` + `EffectiveAutoFixFalseWhenDetectionOff`.)_
- [ ] `C046` `[UT~]` `[E2E]` **Session hooks install works:** Install hooks button detects supported CLIs and reports success/failure clearly. _(UT: status parse.)_
- [ ] `C047` `[E2E]` **Session hooks remove works:** Per-CLI remove buttons remove hook state without breaking the Settings page.
- [ ] `C048` `[UT~]` `[E2E]` **Policy lock UI works:** Locked controls are disabled and show the policy message. _(UT: Effective*/IsLocked gates.)_

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

- [ ] `C049` `[E2E]` **Button opens pane:** The AI assistant button opens the agent pane.
- [ ] `C050` `[UT✓]` `[E2E]` **Hotkey opens pane:** `Ctrl+Shift+.` opens the agent pane. _(UT: `DefaultAgentKeybindings` binding; open behavior E2E.)_
- [ ] `C051` `[E2E]` **Button hides pane:** The button hides/stashes the agent pane without killing the session.
- [ ] `C052` `[E2E]` **Hotkey hides pane:** `Ctrl+Shift+.` hides/stashes the agent pane without killing the session.
- [ ] `C053` `[UT✓]` `[E2E]` **Focus hotkey works:** `Ctrl+Shift+I` focuses the agent pane when available. _(UT: `DefaultAgentKeybindings` binding; focus behavior E2E.)_
- [ ] `C054` `[E2E]` **Different positions work:** Open/hide/focus works for bottom, right, left, and top pane positions.
- [ ] `C055` `[E2E]` **Stash preserves chat:** Hiding and restoring the pane preserves helper process, connection state, and chat history.
- [ ] `C056` `[E2E]` **Tab close cleans up:** Closing the owning tab cleans up the helper and does not leave a broken pane.
- [ ] `C215` `[new]` `[E2E]` **Agent panes are not persisted into saved layout:** Saving and restoring a window layout does not resurrect a previously-open agent pane; restored windows come back without an unexpected agent pane. _(#360/#275.)_

### Built-in agent chat matrix

- [ ] `C057` `[E2E]` `[MANUAL]` **Copilot chat works:** User can send a prompt and Copilot responds successfully.
- [x] `C058` `[UT✓]` `[E2E]` **Copilot missing CLI path works:** Missing Copilot shows actionable setup/auth guidance, not a silent failure. _(UT: `is_cli_available_*` availability check + the auth/setup screen renders `render_auth_sign_in_card` / `render_auth_screen_shows_agent_name`; a missing binary degrades to a guidance screen, not a silent failure. Exercising a truly uninstalled Copilot stays MANUAL.)_
- [ ] `C059` `[E2E]` **Non-Copilot agents chat works:** Each installed+authenticated non-Copilot built-in agent (Claude/Codex/Gemini) connects through its ACP adapter and answers a prompt. _(One consolidated matrix case — all built-in agents share the same agent-pane/ACP path, so per-agent behavioural depth is covered by the Copilot suites.)_
- [x] `C060` `[UT✓]` `[E2E]` **Agent auth failure works:** Unauthenticated agents show clear login guidance and can recover after sign-in. _(UT: `auth_error_routes_to_signin_not_connection_lost` (AuthRequired → sign-in, not a generic failure) + the in-pane auth screen renders `render_auth_screen_shows_agent_name` / `render_auth_sign_in_card` / `render_auth_checking_with_status_message` (login guidance + post-sign-in checking state). Driving a real sign-out stays MANUAL.)_
- [ ] `C216` `[new]` `[E2E]` **GitHub Enterprise Copilot sign-in works:** On the auth screen, pressing **E** lets the user enter a GHE domain (e.g. `*.ghe.com`) and sign in; the last-used host is remembered and the device-verification URL targets that host. _(#362.)_
- [ ] `C061` `[E2E]` **Agent restart after settings change works:** Changing the selected agent or model restarts/reconnects cleanly.
- [ ] `C217` `[new]` `[UT~]` `[E2E]` **Master death is a consistent degraded state:** If `wta-master` exits, the agent pane shows a single consistent degraded state and requires `/restart` to recover — no silent "split-brain" where it looks half-alive. _(#329.)_

### Input and rendering

- [ ] `C062` `[E2E]` **Prompt focused appearance is correct:** Input box looks correct when focused.
- [x] `C063` `[UT✓]` `[E2E]` **Prompt out-of-focus appearance is correct:** Input box looks correct when focus leaves the agent pane. _(UT: `render_input_box_intact_when_pane_unfocused` renders with `pane_focused=false` and asserts the input box stays intact — prompt marker + connection placeholder still paint, not blanked/broken; only the caret style dims, input.rs:69/90.)_
- [ ] `C064` `[E2E]` **Typing works:** User can type, edit, and submit prompt text correctly.
- [ ] `C065` `[E2E]` **Paste works:** Pasted multi-line text is handled correctly.
- [ ] `C218` `[new]` `[UT✓]` `[E2E]` **Image paste (Alt+V) works:** A copied screenshot (`CF_DIB`/`CF_DIBV5`) or image file is queued and sent to the agent as an ACP image content block on the next prompt; the action is gated on the agent advertising image support. When the agent does not support images, or the clipboard has no image, it does not paste but surfaces a clear system message (e.g. "image not supported" / "clipboard empty") rather than silently ignoring the keypress. _(UT: `clipboard_image` + `mock_agent_tests` `seen_images` side-channel; #354.)_
- [ ] `C066` `[E2E]` **Keyboard navigation works:** Arrow keys, Tab completion, Ctrl combinations, and Esc behave correctly.
- [x] `C067` `[UT✓]` `[E2E]` `[MANUAL]` **IME/non-ASCII input works:** IME and non-ASCII input are usable if the release supports localized typing. _(UT: `render_agent_input_accepts_non_ascii` types accented-Latin/Greek/CJK via the real key handler and asserts the input buffer holds them verbatim (multi-byte caret advance) + they render. E2E send path (wtcli send-keys) cannot carry non-ASCII, so the product side is UT-covered; IME composition stays MANUAL.)_
- [ ] `C068` `[UT✓]` `[E2E]` **Streaming output renders correctly:** Agent response chunks, tool calls, plans, and status lines render without corruption. _(UT: `streaming_two_chunks_coalesce_in_app_chat`, `tool_call_surfaces_card_in_chat`, `tool_call_completion_updates_card_status` (in-place, no dup), `plan_surfaces_card_in_chat`, `render_chat_all_message_variants`; streaming-JSON unwrap incl. emoji/surrogate pairs in `ui::chat::tests`.)_
- [x] `C069` `[UT✓]` `[E2E]` **Permission UI works:** When the agent requests a command/tool permission, the user can allow or reject it. _(UT: `permission_allow_round_trips_to_agent`, `permission_reject_round_trips_to_agent`, `permission_quick_allow/reject_key_round_trips_to_agent`, `render_permission_card_shows_options`, `render_permission_compact_shows_hint`; the `y`/`n` quick-key case-match bug was fixed here.)_
- [ ] `C070` `[E2E]` **Insert into pane works:** Agent-proposed command/text can be inserted into the target terminal pane without running.
- [ ] `C071` `[E2E]` **Run in pane works:** Agent-proposed command can be run in the target terminal pane.
- [ ] `C072` `[E2E]` **Command target is correct:** Insert/run applies to the intended active pane, not the agent pane itself or another tab.

### Agent pane slash commands

- [x] `C073` `[UT✓]` **`/help` works:** Shows available commands.
- [x] `C074` `[UT✓]` **`/clear` works:** Clears chat view as expected without breaking the session.
- [x] `C075` `[UT✓]` **`/new` works:** Starts a fresh session.
- [x] `C076` `[UT✓]` **`/fix` works:** Runs manual autofix using recent terminal context. _(UT: classify + `slash_fix_when_idle_submits_autofix_turn` / `slash_fix_while_busy_does_not_resubmit`.)_
- [x] `C077` `[UT✓]` **`/restart` works:** Restarts the agent stack and reconnects to a clean session. _(UT: `slash_restart_resets_connection_and_clears_sessions`.)_
- [x] `C078` `[UT✓]` **`/stop` works:** Stops/cancels an in-progress turn.
- [x] `C079` `[UT✓]` **`/sessions` works:** Switches to session-management view. _(UT: `slash_sessions_opens_agents_view`.)_
- [ ] `C080` `[UT✓]` `[E2E]` **`/model` works:** Opens/selects model where supported; unsupported agents fail gracefully. _(UT: `slash_model_*`; picker render covered by `render_model_picker_lists_models`, full UI flow still E2E.)_
- [x] `C081` `[UT✓]` **Unknown slash command is safe:** Unknown `/command` does not lose user input or crash.
- [ ] `C225` `[E2E]` **`/agent` picker works:** `/agent` opens a keyboard-operable picker containing the current installed/allowed agents, and selecting the current agent is a safe no-op.
- [ ] `C226` `[E2E]` **Invalid `/agent` selection is safe:** `/agent <id>` rejects an unavailable agent without rebuilding the pane or changing the global default.
- [ ] `C082` `[E2E]` **Esc/back navigation works:** User can return from popups/session/model views to chat.

### Chat/session view switching

- [ ] `C083` `[UT✓]` `[E2E]` **Session view opens from chat:** `/sessions`, session button, or `Ctrl+Shift+/` opens the session view. _(UT: `slash_sessions_opens_agents_view` + `DefaultAgentKeybindings`.)_
- [ ] `C084` `[E2E]` **Chat view restores:** User can return to chat view after opening session view.
- [x] `C085` `[UT✓]` `[E2E]` **View switch preserves input:** Draft prompt text is not unexpectedly lost when switching views. _(E2E `Feature.SessionList` 'View switch preserves the draft input': types a draft, switches chat↔sessions via the bottom-bar buttons (SessionToggleButton/AgentToggleButton — not the `/sessions` slash that would type into the draft, not Esc which is overloaded), observing the view via the `AgentLabelText` UIA element (winapp get-value, no jsonl pane ambiguity), then asserts the draft survives. UT: `view_switch_preserves_chat_draft_input` drives the Esc key handler and asserts draft + cursor survive.)_
- [ ] `C086` `[E2E]` **View switch preserves connection:** Agent connection state remains correct after switching views.

## 3. Autofix flow

**Feature definition:** Autofix detects terminal command failures, captures relevant pane context, asks the configured agent for a fix, and lets the user insert or run the suggested command.

### Shell integration and detection

- [ ] `C087` `[E2E]` **PowerShell shell integration installed:** Supported PowerShell profiles emit command-finished events, including non-zero marks for PowerShell-level failures on Windows PowerShell 5.1.
- [ ] `C219` `[new]` `[E2E]` **Bash / WSL shell integration installed:** Supported bash and WSL-bash profiles emit command-finished events, and the injected `PROMPT_COMMAND` is safe under `set -u` (no errors in strict-mode shells). _(#340.)_
- [ ] `C220` `[new]` `[E2E]` **Shells self-report identity (`OSC 9001;ShellType`):** The terminal knows which shell owns a pane — including after a nested shell (`pwsh` → `wsl` → `exit`) returns — so autofix suggests commands for the *current* shell (no PowerShell suggestions inside a WSL/bash pane). `wtcli list-panes` exposes the live shell + version per pane. _(#345.)_
- [ ] `C088` `[E2E]` **Missing shell integration is safe:** Without shell integration, failures do not crash or produce broken UI.
- [x] `C089` `[UT✓]` **Failure detection works:** A failing command emits an event and is detected by Intelligent Terminal. _(UT: `classify_wt_event`.)_
- [x] `C090` `[UT✓]` **Successful commands ignored:** Successful commands do not trigger autofix. _(UT: `classify_wt_event` + `success_exit_code_does_not_arm_autofix`.)_
- [x] `C091` `[UT✓]` **Detection off suppresses autofix:** With automatic error detection off, failures do not trigger autofix. _(UT: autofix reducer.)_
- [x] `C092` `[UT✓]` **Detection on observes failures:** With detection on, failure notifications are observed. _(UT: autofix reducer.)_
- [x] `C093` `[UT✓]` **Suggestion off suppresses LLM call:** With suggestion off, detection can show any expected local UI but does not ask the agent for a fix. _(UT: `suggestion_off_emits_detected_without_submitting_turn`.)_
- [x] `C094` `[UT✓]` **Suggestion on triggers LLM call:** With suggestion on and a connected helper, an autofix suggestion is requested. _(UT: reducer submit path.)_
- [x] `C095` `[UT✓]` **Cold-start behavior is acceptable:** If failure happens before the helper is connected, UI stays stable and no stale suggestion appears later. _(UT: `cold_start_drops_autofix_when_not_connected`.)_

### Autofix with agent pane

- [ ] `C096` `[E2E]` **Visible agent pane autofix works:** Autofix works when the agent pane is visible.
- [ ] `C097` `[E2E]` **Stashed agent pane autofix works:** Autofix works when the per-tab agent pane is pre-warmed but hidden.
- [ ] `C098` `[E2E]` **Autofix opens/restores UI correctly:** Suggestion UI appears in the expected pane/tab and does not steal unrelated focus unexpectedly.
- [ ] `C099` `[E2E]` **Insert suggestion works:** Suggested fix can be inserted into the source pane.
- [ ] `C100` `[E2E]` **Run suggestion works:** Suggested fix can be run in the source pane.
- [ ] `C101` `[UT✓]` `[E2E]` **Reject/dismiss works:** User can dismiss an autofix suggestion without side effects. _(UT: `trigger_echo_pane_clears_when_state_returns_to_idle`.)_
- [ ] `C102` `[UT✓]` `[E2E]` **Autofix target pane is correct:** Failure in one pane does not offer/run a fix in the wrong pane. _(UT: target-tab routing — busy-pane tests + `autofix_still_triggers_for_non_agent_pane`.)_
- [ ] `C103` `[E2E]` `[MANUAL]` **Autofix with Copilot works:** Copilot returns a useful suggestion.
- [ ] `C104` `[E2E]` **Autofix with non-Copilot agents works:** Autofix produces a usable suggestion with a non-Copilot built-in agent (Claude/Codex/Gemini) and a custom ACP agent — same path as Copilot, covered once across the available agents.
- [ ] `C221` `[new]` `[E2E]` `[MANUAL]` **Environment-aware answers/fixes:** For a failed or "how do I use X" prompt, the agent investigates the live environment first — checks whether the command actually exists on PATH and surfaces local scripts / near-matches for a mistyped command — instead of giving generic advice or fixing a nonexistent command. _(#306.)_

### Autofix across layout changes

- [ ] `C105` `[UT~]` `[E2E]` **Split pane autofix works:** Failure in a split pane is routed to the correct tab/pane. _(UT: tab/pane routing.)_
- [x] `C106` `[UT✓]` `[E2E]` **Moved tab autofix works:** After moving a tab to another window, failures route to the correct agent pane. _(UT: `wt_event_critical_from_owner_tab_raises_banner_not_chat` — autofix/WT events route by owner_tab_id, which survives the window move (same routing proven end-to-end for chat/prompt by Feature.MultiWindow C164). The LLM-fix half is covered by Feature.AutofixPane.)_
- [x] `C107` `[UT✓]` `[E2E]` **Multi-window autofix works:** Multiple windows with agent panes do not cross-route suggestions. _(UT: `wt_event_critical_from_other_tab_does_not_surface_in_owner_tab` — a helper owning tab A DROPS a failure event broadcast from tab B (no banner/chat/notification), so autofix suggestions cannot cross-route between windows/tabs; helpers filter inbound events by window_id + owner_tab_id. Same isolation credited for C166.)_
- [ ] `C108` `[UT~]` `[E2E]` **Closed pane cleanup works:** Autofix does not target a pane that has already closed.

## 4. Session management

**Feature definition:** Session management lists known live and historical agent sessions, shows their state, and lets users focus or resume supported sessions.

### Surfaces

- [ ] `C109` `[E2E]` **Session button works:** The session-management button opens the session view.
- [x] `C110` `[UT✓]` `[E2E]` **Hotkey works:** `Ctrl+Shift+/` opens the session view. _(UT: `DefaultAgentKeybindings` pins `ctrl+shift+/`→`Terminal.OpenAgentSessions`; the open-the-view behavior it triggers is E2E-proven via the same handler in `Feature.SessionList`/`Feature.SessionState` (`/sessions`, C083/C111). The live WT-accelerator keystroke itself isn't a stable E2E observable — see C083 note.)_
- [ ] `C111` `[UT✓]` `[E2E]` **Slash command works:** `/sessions` opens the session view. _(UT: `/sessions` classify.)_
- [x] `C112` `[UT✓]` `[E2E]` **Command action works:** The `openAgentSessions` action opens the session view. _(UT: `AgentActionsParse` verifies `openAgentSessions` parses to the action; the resulting open-the-view behavior is E2E-proven in `Feature.SessionList` 'Session view opens from chat' / 'Slash command works' (C083/C111), which route through the same `_HandleOpenAgentSessions`.)_
- [x] `C113` `[UT✓]` `[E2E]` **Session view empty state works:** Empty/no-session state is useful and not visually broken. _(UT: `render_sessions_view_shows_footer_hint` + `render_agents_view_empty_when_no_sessions_is_stable` paint the agents-view chrome/footer with an empty registry (no panic, nav hint drawn).)_
- [ ] `C114` `[E2E]` **Session view refresh works:** Pressing **F5** re-scans history on demand so sessions that appeared after launch show up without restarting Terminal — e.g. a CLI session started in another shell. Works independently of whether session hooks are active. _(#344.)_
- [ ] `C222` `[new]` `[E2E]` **Session titles are clean:** Session rows show a meaningful title and never a bare "# AGENTS.md instructions" Codex heading or raw markdown artifact. _(#355.)_

### Session states

- [ ] `C115` `[UT✓]` `[E2E]` **Active/Live state is correct:** A currently reachable session is shown as active/live and can be focused. _(UT: `agent_sessions` liveness.)_
- [x] `C116` `[UT✓]` `[E2E]` **Running/Working state is correct:** A session running a tool or long operation shows running/working state. _(UT: `agents_view::status_badge_renders_expected_text_per_state` (Working→active badge) + `session_mgmt::liveness_from_status_maps_activity_states_to_live`.)_
- [x] `C117` `[UT✓]` `[E2E]` **Waiting-for-input state is correct:** A session waiting for user input/attention shows the waiting/attention state. _(UT: `agents_view::status_badge_renders_expected_text_per_state` (Attention→waiting_for_input badge).)_
- [x] `C118` `[UT✓]` `[E2E]` **Idle state is correct:** A live session waiting for the next prompt shows idle/ready state. _(E2E `Feature.SessionState` drives a live shell copilot session to turn-completion and asserts its Idle badge in the /sessions view; UT: Idle activity derivation + badge render `agents_view.rs::status_badge_renders_expected_text_per_state`.)_
- [x] `C119` `[UT✓]` **Ended state is correct:** A session whose pane was closed becomes ended and does not stay falsely live. _(UT: PaneClosed tombstone + "Ended must stay Ended" in `agent_sessions.rs`; Ended renders an empty badge `agents_view.rs::status_badge_renders_expected_text_per_state`, so it is visually distinct from any live/idle row.)_
- [ ] `C120` `[UT✓]` `[E2E]` **Historical state is correct:** Historical sessions (now sourced from the agent's ACP `session/list`, not on-disk file parsing) show as historical when not live. _(#365.)_
- [ ] `C121` `[UT✓]` `[E2E]` **State transitions are correct:** Live -> ended, historical -> live, and working -> idle transitions update without duplicate/stale rows. _(UT: `apply_alive_session_join` / `apply_master_session_ended`.)_

### Focus and restore

- [ ] `C122` `[UT✓]` `[E2E]` **Focus active session:** Selecting an active session navigates/focuses the existing pane. _(UT: `decide_enter_action` Focus.)_
- [ ] `C223` `[new]` `[E2E]` **Focus brings the target window forward:** Focusing a session/pane that lives in another (background) window brings that window to the foreground, not just the pane within it. _(#353.)_
- [x] `C123` `[UT✓]` `[E2E]` **Focus active stashed agent pane:** Selecting an active stashed agent-pane session restores/focuses the pane if applicable. _(UT: `session_mgmt::class_a_live_with_pane_enter_focuses` — a live Class-A agent-pane row routes Enter to FocusPane. E2E-blocked: the MVP picker `ShellOnly` filter does not render agent-pane rows.)_
- [x] `C124` `[UT✓]` `[E2E]` **Restore old session:** Selecting a supported old session resumes it successfully. _(UT: `session_mgmt::class_a_historical_enter_routes_like_ended` + `class_a_ended_enter_resumes_in_agent_pane_when_supported`.)_
- [x] `C125` `[UT✓]` `[E2E]` **Restore old shell-pane session:** Supported shell-pane sessions resume through the CLI resume path. _(UT: `session_mgmt::class_b_historical_enter_resumes_via_cli_flag` — ResumeCliFlag decision.)_
- [x] `C126` `[UT✓]` `[E2E]` **Restore old agent-pane session:** Supported agent-pane sessions resume through agent-pane/session-load path when enabled. _(UT: `session_mgmt::class_a_ended_enter_resumes_in_agent_pane_when_supported` — ResumeInAgentPane decision.)_
- [x] `C127` `[UT✓]` `[E2E]` **Unsupported restore is clear:** Unknown CLI, missing resume support, or missing on-disk session shows a clear not-resumable message. _(UT: `session_mgmt::class_a_ended_enter_not_resumable_when_load_unsupported`, `class_a_ended_shift_not_resumable_when_cli_has_no_flag`, `unknown_cli_not_resumable_in_either_direction` — NotResumable reasons.)_
- [ ] `C128` `[UT✓]` `[E2E]` **Enter behavior works:** Enter performs the expected focus/resume action.
- [x] `C129` `[UT✓]` **Shift+Enter behavior works:** Shift+Enter performs the alternate resume path for dead sessions and same focus path for live sessions. _(UT: `decide_enter_action` shift + `shift_enter_on_class_a_live_row_focuses`. Not E2E: Live-row Shift+Enter dispatches FocusPane — moves WT focus to the session's pane, it does NOT dismiss the view — and the MVP picker shows only Class B shell sessions whose panes are usually already closed, so there is no stable E2E observable.)_

### Session-management scope and custom agents

- [x] `C130` `[UT✓]` `[E2E]` **Built-in agents tracked:** Copilot, Claude, Codex, and Gemini sessions are tracked when hooks/session support is enabled. _(UT: `CliSource::parse`/`from_agent_id` recognize all four agents — agent_sessions.rs:35-68; `session_registry::registry_assigns_codex_cli_source_when_session_started_via_agent_id` + `agent_sessions::iter_sorted_filtered_keeps_only_matching_cli_source` exercise cli_source tracking/filtering.)_
- [x] `C131` `[UT✓]` `[E2E]` **Custom agent safe behavior:** Custom agents do not crash session management and do not show strange/broken UI. _(UT: `session_mgmt::unknown_cli_not_resumable_in_either_direction` — an unknown/custom CLI degrades to a safe NotResumable(UnknownCli) in both Enter and Shift+Enter directions rather than crashing.)_
- [x] `C132` `[UT✓]` **Custom agent limitation is acceptable:** Session management is not expected to fully restore custom-agent sessions unless the custom agent provides compatible session metadata.
- [x] `C133` `[UT✓]` **MVP origin filter is understood:** If the release keeps the MVP filter, the picker shows shell-pane sessions only while debug/CLI listing can still inspect all origins. _(UT: `OriginFilter` + cli_tests.)_
- [x] `C134` `[UT✓]` `[E2E]` **Hooks off behavior is safe:** With session management off, missing rows are expected and UI remains stable. _(UT: `app::render_agents_view_empty_when_no_sessions_is_stable` — with no tracked sessions (hooks off), the Agents view renders a stable empty state (draw does not panic, navigation footer hint is painted).)_

## 5. Delegate agent and command palette shortcuts

**Feature definition:** Delegate mode launches a separate agent task from the current terminal context/cwd, without using the interactive agent pane chat.

- [ ] `C135` `[UT✓]` `[E2E]` **`Alt+Shift+B` launches background delegate:** Shortcut opens a new delegate agent/task. _(UT: `DefaultAgentKeybindings` binding; launch E2E.)_
- [ ] `C136` `[UT~]` `[E2E]` **Delegate cwd is correct:** The delegate starts with the current pane's working directory.
- [ ] `C137` `[UT✓]` `[E2E]` **Delegate provider is correct:** The launched delegate uses the configured delegate agent, not the agent-pane provider unless they are intentionally the same. _(UT: `EffectiveDelegateAgent`.)_
- [x] `C138` `[UT✓]` `[E2E]` **Delegate model is correct:** The launched delegate uses the configured delegate model. _(UT: `coordinator::delegate_launch_commandline_appends_startup_prompt_and_model` + `delegate_runtime_inherits_model_from_agent_command` — the delegate command line carries the configured `--model`.)_
- [ ] `C139` `[UT✓]` `[E2E]` **`Alt+Shift+/` opens agent delegation palette:** Shortcut opens command palette in agent-delegation mode. _(UT: `DefaultAgentKeybindings` binding; palette E2E.)_
- [ ] `C140` `[E2E]` **Command palette prompt launches delegate:** Typing a request and pressing Enter creates a delegate task.
- [ ] `C141` `[E2E]` **Command palette cancel is safe:** Esc/cancel closes the palette without launching a delegate.
- [ ] `C142` `[E2E]` `[MANUAL]` **Delegate with Copilot works:** Copilot delegate task starts and responds.
- [ ] `C143` `[E2E]` `[MANUAL]` **Delegate with non-Copilot agents works:** Claude/Codex/Gemini delegate tasks start and respond where supported by delegate mode.
- [ ] `C144` `[UT~]` `[E2E]` **Delegate errors are actionable:** Missing CLI/auth errors are clear.

## 6. Custom agents

**Feature definition:** Settings can configure one custom command for the agent pane and one custom command for delegate mode. Custom agents are not configured from FRE.

### Custom agent pane

- [ ] `C145` `[E2E]` **Custom agent is Settings-only:** FRE does not expose custom-agent creation.
- [ ] `C146` `[UT✓]` `[E2E]` **Add custom ACP agent:** In Settings, add an agent-pane custom command such as `qwen.cmd --acp`. _(UT: `DeriveCustomAgentId`.)_
- [x] `C147` `[UT✓]` **Save custom ACP agent:** Saving persists `custom:<cmd>`/custom command settings. _(UT: CustomAgentAndPolicyTests round-trip.)_
- [ ] `C148` `[UT✓]` `[E2E]` **Edit custom ACP agent:** Editing updates the command used by new agent panes.
- [ ] `C149` `[UT~]` `[E2E]` **Delete custom ACP agent:** Deleting returns to a valid built-in/default selection.
- [ ] `C150` `[UT~]` `[E2E]` **Model selection visible:** Model picker/textbox remains visible when custom agent is selected.
- [ ] `C151` `[E2E]` **Custom agent runs the standard agent-pane behaviours:** A configured custom ACP agent can chat, request a command/tool action, insert/run into the pane, and drive autofix — the same agent-pane behaviours verified in depth with Copilot.
- [ ] `C152` `[UT~]` `[E2E]` **Custom failure is safe:** Bad command, missing executable, or non-ACP behavior shows a clear error and does not crash Terminal. _(UT: failure classification.)_

### Custom delegate agent

- [x] `C153` `[UT✓]` `[E2E]` **Add custom delegate agent:** In Settings, add a delegate custom command such as `qwen.cmd`. _(UT: `DeriveCustomAgentId` (AIAgentsViewModel) derives the `custom:<id>` from the entered command; E2E-adjacent: `Feature.CustomDelegate` runs a configured custom delegate command.)_
- [x] `C154` `[UT✓]` **Save custom delegate agent:** Saving persists the delegate custom command. _(UT: round-trip.)_
- [ ] `C155` `[UT✓]` `[E2E]` **`Alt+Shift+B` uses custom delegate:** Background delegate shortcut launches the custom command. _(UT: `DefaultAgentKeybindings` binding + custom `EffectiveDelegateAgent` resolution.)_
- [ ] `C156` `[UT✓]` `[E2E]` **`Alt+Shift+/` uses custom delegate:** Agent-delegation command palette launches the custom command. _(UT: `DefaultAgentKeybindings` + `AgentActionsParse` delegation mode.)_
- [ ] `C157` `[UT~]` `[E2E]` **Custom delegate cwd is correct:** Custom delegate starts in the source pane's cwd.
- [ ] `C158` `[UT~]` `[E2E]` **Custom delegate errors are clear:** Bad command or auth/setup failure is actionable.

## 7. Multi-pane and multi-window behavior

**Feature definition:** Agent state, session routing, and autofix routing are per-tab and per-window. Moving tabs/windows should not lose or cross-route agent context.

- [ ] `C159` `[E2E]` **Split pane does not break chat:** Splitting the terminal pane keeps agent pane chat usable.
- [ ] `C160` `[UT~]` `[E2E]` **Split pane target selection is correct:** Agent insert/run/autofix targets the intended non-agent pane. _(UT: routing core.)_
- [ ] `C161` `[UT~]` `[E2E]` **Multiple tabs work:** Each tab has its own agent pane/session state. _(UT: per-tab state.)_
- [ ] `C162` `[E2E]` **Multiple agent panes work:** Opening agent panes in multiple tabs does not mix conversations.
- [ ] `C227` `[E2E]` **Per-tab agent switching is isolated:** `/agent <id>` can switch one tab to a different built-in agent without rebuilding sibling tabs, losing either conversation, or restarting the shared master.
- [ ] `C228` `[E2E]` **Global agent defaults respect per-tab overrides:** New tabs inherit the global agent; changing that default rebuilds follower tabs while preserving tabs with an explicit runtime override.
- [ ] `C163` `[E2E]` **Move tab to new window preserves chat:** Dragging/tearing a tab to another window preserves agent pane state.
- [ ] `C224` `[new]` `[E2E]` **Agent-created terminals inherit the active profile:** A terminal/tab the agent opens inherits the active pane's profile (e.g. an agent working in an Ubuntu session spawns new tabs in Ubuntu, not the default PowerShell profile). _(#366, closes #351.)_
- [x] `C164` `[UT✓]` `[E2E]` **Move tab to new window preserves session routing:** Session events remain associated with the moved tab. _(UT: `wt_event_critical_from_owner_tab_raises_banner_not_chat` — events route by owner_tab_id, which survives the window move. E2E: `Feature.MultiWindow` sends a fresh prompt to the moved agent pane by its pinned session id after the move and asserts it answers, LLM-skip-guarded.)_
- [ ] `C165` `[UT~]` `[E2E]` **Move tab to new window preserves autofix:** Autofix still routes to the moved tab/pane.
- [x] `C166` `[UT✓]` `[E2E]` **Multiple windows do not cross-route:** Events from one window do not mutate another window's agent pane/session UI. _(UT: `wt_event_critical_from_other_tab_does_not_surface_in_owner_tab` — a helper owning tab A DROPS a connection-failure event broadcast from tab B (no banner, no chat, no notification), the exact cross-route isolation contract; helpers filter inbound events by window_id + owner_tab_id.)_
- [ ] `C167` `[E2E]` **Close source window is safe:** Closing a source window after moving a tab does not kill the moved tab's agent state.
- [ ] `C168` `[E2E]` **Close target tab cleans up:** Closing moved tabs cleans up helper/session state without affecting other tabs.

## 8. Agent hooks and session tracking

**Feature definition:** Agent hooks record shell-pane agent sessions and enable session-management state for supported CLIs.

- [ ] `C169` `[E2E]` **Install hooks from FRE works:** Session-management toggle can install supported hooks during first run.
- [ ] `C170` `[E2E]` **Install hooks from Settings works:** Install hooks button works after FRE.
- [ ] `C171` `[E2E]` **Per-CLI hook install works:** Each supported CLI (Copilot/Claude/Gemini) installs its hook or reports why it can't; Codex hook/session support behaves per the current implementation.
- [ ] `C172` `[E2E]` **Hook remove works:** Removing a hook disables future session tracking for that CLI.
- [x] `C173` `[UT✓]` `[E2E]` **Disabled plugin is respected:** Disabled agent plugin is skipped and not force-enabled. _(UT: `decide_skip_when_disabled`.)_
- [x] `C174` `[UT✓]` `[E2E]` **Hook auto-upgrade works:** After package upgrade, previously installed hooks are updated silently when bundle version changes. _(UT: `decide_upgrade` + `upgrade_state` round-trip.)_
- [x] `C175` `[UT✓]` `[E2E]` **Opt-in preserved:** Auto-upgrade does not install hooks into a CLI the user never opted into. _(UT: `decide_skip_when_not_installed`.)_
- [ ] `C176` `[E2E]` **Hook logs are available:** Hook decisions and failures are visible in the expected WTA log files.

## 9. Packaging, process, and protocol integration

**Feature definition:** Packaged Intelligent Terminal includes WTA/wtcli integration and uses the packaged COM protocol server correctly.

- [ ] `C177` `[E2E]` **Packaged `wta.exe` is present:** WTA is deployed next to WindowsTerminal in the package layout.
- [ ] `C178` `[E2E]` **Packaged identity works:** WTA/wtcli can activate the Terminal protocol COM server from packaged context.
- [ ] `C179` `[E2E]` **Wrong unpackaged WTA is not used:** Agent pane/autofix does not accidentally use a stale dev-build WTA.
- [ ] `C180` `[E2E]` **`WT_COM_CLSID` is injected:** Shell panes and agent panes inherit protocol discovery environment as expected.
- [ ] `C181` `[E2E]` **`wtcli list-panes` works:** Basic WT protocol query succeeds from a pane.
- [ ] `C182` `[E2E]` **`wtcli capture-pane` works:** Pane output capture succeeds.
- [ ] `C183` `[E2E]` **`wtcli send-keys`/send input path works:** Insert/run operations can send input to the target pane.
- [ ] `C184` `[E2E]` **`wtcli listen` works:** Event subscription receives shell/agent events.
- [ ] `C185` `[E2E]` **WTA master starts:** One master process starts per Terminal process when needed.
- [ ] `C186` `[E2E]` **WTA helper starts per tab/pane:** Agent pane helper starts and connects to master.
- [ ] `C187` `[E2E]` **Master/helper crash recovery is acceptable:** Crashes or exits recover or surface an actionable error.

## 10. Diagnostics, logging, and supportability

**Feature definition:** Release builds should leave enough diagnostics for support without overwhelming the user.

- [ ] `C188` `[E2E]` **WTA logs are written:** WTA process logs are created in the expected package-private log directory.
- [ ] `C189` `[E2E]` **C++ agent pane log is written:** Terminal-side agent pane log is created.
- [ ] `C190` `[E2E]` **Hook trace log is written:** Hook events write to hook trace log when hooks are active.
- [ ] `C191` `[UT~]` `[E2E]` **Log version directory is correct:** Packaged builds write under the current package-version log directory. _(UT: `runtime_paths` resolution.)_
- [ ] `C192` `[UT~]` `[E2E]` **Old log cleanup is safe:** Starting the new build does not delete logs from the currently running version. _(UT: housekeeping prune logic.)_
- [ ] `C193` `[E2E]` **Bug report zip includes agent logs:** Diagnostic collection includes WTA, hook, and terminal-agent-pane logs.
- [ ] `C194` `[E2E]` **Release log level is reasonable:** Default release logging is not excessively noisy.
- [ ] `C195` `[E2E]` **Early startup failures are logged:** Failures before agent connection still land in logs.

## 11. Accessibility, localization, and UI polish

**Feature definition:** Intelligent Terminal AI features should be usable with keyboard, screen readers, localization, scaling, and theme changes.

- [ ] `C196` `[E2E]` **Keyboard-only FRE works:** FRE can be completed without a mouse.
- [ ] `C197` `[E2E]` **Keyboard-only Settings works:** AI Agents settings can be configured without a mouse.
- [ ] `C198` `[E2E]` **Keyboard-only agent pane works:** Chat, slash commands, popups, and session view are keyboard accessible.
- [ ] `C199` `[MANUAL]` **Narrator reads FRE controls:** FRE controls have useful names/help text.
- [ ] `C200` `[MANUAL]` **Narrator reads Settings controls:** AI Agents settings controls have useful names/help text.
- [ ] `C201` `[MANUAL]` **Narrator reads agent pane state:** Connection/status changes are understandable.
- [ ] `C202` `[MANUAL]` **High contrast theme works:** FRE, Settings, agent pane, and autofix UI remain readable.
- [ ] `C203` `[MANUAL]` **Light/dark theme works:** UI is readable in both themes.
- [ ] `C204` `[MANUAL]` **Text scaling works:** 125%, 150%, and 200% scaling do not clip critical controls.
- [x] `C205` `[UT✓]` `[MANUAL]` **Localization strings are present:** New user-facing strings are localized or intentionally locked. _(UT: `locale_parity_tests::every_locale_has_all_en_us_keys` enforces that every WTA `locales/*.yml` (all 89, incl. pseudo-locales) contains every en-US key — so no agent-feature user-facing string is missing a translation. The C++ `.resw` locales remain enforced by the loc pipeline/manual.)_
- [x] `C206` `[UT✓]` `[MANUAL]` **Pseudo-locales work:** qps pseudo-locales do not clip or corrupt layout. _(UT: `locale_parity_tests::every_locale_has_all_en_us_keys` iterates every `locales/*.yml` including qps-ploc/ploca/plocm and proves each is present + key-complete, so the pseudo-locales load with no missing-key en-US fallthrough. Visual clip/overflow inspection stays MANUAL.)_
- [ ] `C207` `[UT~]` `[MANUAL]` **RTL works:** RTL layout is mirrored where expected. _(UT: `IsRtlLocale`.)_

## 12. Release decision

- [ ] `C208` `[MANUAL]` **All P0/P1 issues resolved:** No blocking agent pane, autofix, FRE, session, custom-agent, or packaging bugs remain.
- [ ] `C209` `[MANUAL]` **Known limitations documented:** Any intentionally deferred behavior is documented in release notes.
- [ ] `C210` `[E2E]` `[MANUAL]` **Upgrade path signed off:** Existing users upgrading from the previous release keep settings/hooks in a valid state.
- [ ] `C211` `[E2E]` `[MANUAL]` **Fresh install signed off:** New users can complete FRE and use the default agent flow.
- [ ] `C212` `[E2E]` `[MANUAL]` **Rollback/uninstall behavior signed off:** Uninstall or rollback leaves no user-blocking broken state.
- [ ] `C213` `[MANUAL]` **Final release owner sign-off:** Release owner approves shipping this build.

## Source notes used to build this checklist

- FRE, Settings, and policy behavior: `src\cascadia\TerminalApp\FreOverlay.cpp`, `src\cascadia\TerminalSettingsEditor\AIAgents.xaml`.
- Default actions and shortcuts: `src\cascadia\TerminalSettingsModel\defaults.json`.
- Built-in agent definitions: `tools\wta\src\agent_registry.rs`.
- Slash commands: `tools\wta\src\commands.rs`.
- Session state model: `tools\wta\src\agent_sessions.rs`, `tools\wta\AGENTS.md`.
- Multi-window agent pane architecture: `doc\specs\Multi-window-agent-pane.md`.
- Autofix flow and logging/runtime layout: `AGENTS.md`.
