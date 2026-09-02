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

- **`[x]` pure `[UT✓]`** — the logic is fully verified by a unit test that re-runs on every build. Do **not** manually test these in isolation; just let them ride along in the final end-to-end smoke pass. (Examples: slash-command dispatch, Auto error handling option gating, settings persistence.)
- **`[UT✓]` + `[E2E]` (box left unchecked)** — the **decision/logic half is already UT-covered**, so during E2E you only need to confirm the **UI / interaction half** works (the pane actually opens, the row actually shows the state, the picker renders). You do **not** need to re-verify the underlying branches — those are guarded by UT and regress automatically. (Examples: `Ctrl+Shift+.` opens the pane, session-state display, Enter resume, `/model` picker.)
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

**Feature definition:** FRE guides first-time users through agent selection, pane position, Auto error handling, and session-management hook setup.

- [ ] `C007` `[E2E]` **FRE opens correctly:** A clean user profile launches the FRE instead of skipping directly to the terminal.
- [ ] `C008` `[E2E]` **FRE can be completed:** The user can go through every page, save settings, and enter the main terminal window.
- [ ] `C009` `[E2E]` **FRE can be skipped or closed safely:** Skipping/closing does not crash and leaves settings in a valid state.
- [ ] `C010` `[E2E]` **FRE privacy / help links work:** Links open the browser and do not block completion.
- [ ] `C011` `[E2E]` **FRE save progress works:** The progress UI appears while setup/install work is running and returns to a usable state.
- [x] `C012` `[UT✓]` `[E2E]` **FRE error messages are actionable:** Install/auth/setup failures show a useful message instead of a silent failure or raw OS error. _(UT: `classify_connection_closed_is_actionable` + `classify_connection_failed_is_critical` classify agent connection failures into actionable/critical categories that drive the user-facing message rather than a raw error; `auth_error_routes_to_signin_not_connection_lost` routes auth failures to a sign-in prompt.)_
- [ ] `C214` `[new]` `[UT~]` `[E2E]` **FRE execution-policy detection is correct:** FRE flags a genuinely blocking PowerShell execution policy but does **not** false-block when a load-induced execution-policy probe merely times out; an unknown/unreadable policy is treated conservatively (as blocking). _(#336/#338/#309; UT: execution-policy gate.)_
- [ ] `C013` `[UT~]` `[E2E]` **FRE respects policy locks:** If agent, Auto error handling, or session-management policy is locked, affected controls are disabled and explain why. _(UT: `IsAgentPolicyLocked`, effective-setting gates.)_
- [ ] `C014` `[UT~]` `[MANUAL]` **FRE RTL/localized layout is usable:** Layout mirrors correctly for RTL locales and text is not clipped in localized builds. _(UT: `IsRtlLocale`.)_

### FRE agent selection

- [x] `C015` `[UT✓]` `[E2E]` **Copilot without install:** Copilot appears as an available/default choice, is labeled as needing install, and the setup path installs or clearly explains how to install it. _(UT: `is_cli_available_handles_empty_string` / `is_cli_available_returns_false_for_obviously_bogus_name` drive the availability check that labels a CLI as needing install; the FRE agent picker (Feature.FreAgentSetup) shows Copilot as a choice. The actual install action requires a real uninstalled Copilot, which stays MANUAL.)_
- [ ] `C016` `[UT~]` `[E2E]` **Copilot preinstalled:** Copilot appears as installed; saving does not reinstall unnecessarily; opening the agent pane uses Copilot successfully.
- [ ] `C017` `[UT~]` `[E2E]` **Non-Copilot agents appear when installed:** Claude/Codex/Gemini appear as selectable only when installed; selecting one saves correctly and can connect in agent-pane mode; Node/npx requirement guidance appears when relevant.
- [ ] `C018` `[UT~]` `[E2E]` **Unavailable non-Copilot agents:** Claude/Codex/Gemini that are not installed do not appear as broken selectable options.
- [ ] `C019` `[UT✓]` `[E2E]` **Agent selection persists:** The selected agent remains selected after FRE completion and app restart. _(UT: `BuiltInAcpAgentRoundtrips`.)_

### FRE Auto error handling

- [ ] `C020` `[UT✓]` `[E2E]` **All Auto error handling options appear:** The description says "Choose how terminal errors are handled." and the picker shows exactly `Off`; `Detect errors automatically`; `Detect errors and send them to the agent for fixes automatically` _(UT: enum serialization and option population.)_
- [ ] `C021` `[UT✓]` `[E2E]` **`Off` works:** Command failures do not produce Auto error handling activity.
- [ ] `C022` `[UT✓]` `[E2E]` **`Detect errors automatically` works:** Shell failures are detected when shell integration is available, but they are not sent to the agent.
- [ ] `C023` `[UT✓]` `[E2E]` **`Detect errors and send them to the agent for fixes automatically` works:** A detected failure can be sent to a connected agent, which can return a proposed fix.
- [ ] `C024` `[UT✓]` `[E2E]` **The picker stores one valid state:** Switching options never produces an invalid combination or leaves obsolete dependent controls visible.
- [ ] `C025` `[UT✓]` `[E2E]` **The option persists:** The selected `autoErrorHandling` value remains after restart. _(UT: settings round-trip.)_

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

**Feature definition:** Settings is the post-FRE configuration surface for built-in agents, custom agents, model selection, pane position, Auto error handling, and session hooks.

- [ ] `C036` `[E2E]` **AI Agents page opens:** Settings opens the AI Agents page without layout glitches.
- [ ] `C037` `[UT~]` `[E2E]` **Built-in agent dropdown works:** Copilot, Claude, Codex, and Gemini entries show correct installed/available state. _(UT: registry/filter logic.)_
- [ ] `C038` `[UT✓]` `[E2E]` **Agent pane agent save works:** Changing the agent pane provider updates future agent panes. _(UT: `BuiltInAcpAgentRoundtrips` + custom round-trip.)_
- [ ] `C039` `[UT✓]` `[E2E]` **Delegate agent save works:** Changing the delegate provider updates future delegate launches. _(UT: `BuiltInDelegateAgentRoundtrips` + custom round-trip.)_
- [ ] `C040` `[UT~]` `[E2E]` **Model control appears:** Model picker/textbox appears when a selected agent supports or has a configured model.
- [ ] `C041` `[UT✓]` `[E2E]` **Model changes apply:** Changing `acpModel` affects new agent-pane sessions and does not corrupt existing settings. _(UT: `build_acp_command` model handling.)_
- [ ] `C042` `[UT✓]` `[E2E]` **Delegate model changes apply:** Changing `delegateModel` affects new delegate-agent launches. _(UT: command construction.)_
- [ ] `C043` `[UT✓]` `[E2E]` **Pane position setting works:** Bottom/right/left/top can be selected and saved. _(UT: `AgentPanePositionRoundtripsAndDefaults`.)_
- [ ] `C044` `[UT✓]` `[E2E]` **Auto error handling options match FRE:** Settings explains that the control chooses how terminal errors are handled and shows exactly `Off`; `Detect errors automatically`; `Detect errors and send them to the agent for fixes automatically` _(UT: enum settings round-trip.)_
- [ ] `C045` `[UT✓]` `[E2E]` **Auto error handling behavior matches FRE:** Each selected option has the same detection and agent-submission behavior in Settings and FRE.
- [ ] `C046` `[UT~]` `[E2E]` **Session hooks install works:** Install hooks button detects supported CLIs and reports success/failure clearly. _(UT: status parse.)_
- [ ] `C047` `[E2E]` **Session hooks remove works:** Per-CLI remove buttons remove hook state without breaking the Settings page.
- [ ] `C048` `[UT~]` `[E2E]` **Policy lock UI works:** Locked controls are disabled and show the policy message. _(UT: Effective*/IsLocked gates.)_

### Profile Agent pane agent

- [ ] `C237` `[new]` `[UT~]` `[E2E]` **Profile Agent pane agent picker works:** Each profile's Agent pane agent picker lists installed, policy-allowed Windows agents plus agents installed natively in that profile's WSL distro; saving persists `agentPaneBackend`, while an unconfigured profile inherits the global Windows agent. _(#481; UT: `ProfileTests::AgentPaneBackendDefaultsAndInherits`.)_
- [ ] `C238` `[new]` `[E2E]` **Profile WSL agent routing is strict:** Changing a profile to a WSL backend rebuilds its helper, routes the exact selected agent through `wsl:<distro>`, and does not fall back to a Windows-hosted agent. _(#481; E2E: `Feature.WslAgentBackend`.)_

### Profile command palette agent

- [ ] `C244` `[new]` `[UT~]` `[E2E]` **Profile Command palette agent picker works:** Each profile's Command palette agent picker offers Host and WSL-distro delegate agents; saving persists `commandPaletteAgent`, while an unconfigured profile follows the global delegate agent. _(#488; UT: `ProfileTests::AgentPaneBackendDefaultsAndInherits`.)_
- [ ] `C245` `[new]` `[E2E]` **Command palette agent source is strict:** A profile's Command palette agent selects the exact delegate execution source — an explicit WSL selection stays in its distro and surfaces the real in-distro error, and a host selection is never diverted to WSL. _(#488; E2E: `Feature.DelegateSource`.)_

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
- [ ] `C252` `[new]` `[E2E]` **Agent pane move is isolated per tab and preserves input focus:** `/move` changes only the current tab's runtime pane position, leaves the global setting and sibling tabs unchanged, and returns keyboard focus to the moved agent input. _(#429; E2E: `Feature.AgentPaneMove`.)_
- [ ] `C055` `[E2E]` **Stash preserves chat:** Hiding and restoring the pane preserves helper process, connection state, and chat history.
- [ ] `C056` `[E2E]` **Tab close cleans up:** Closing the owning tab physically closes its ACP session, cleans up the helper, and does not leave a broken pane.
- [ ] `C247` `[new]` `[E2E]` **Closing a tab mid-turn leaves sibling agent tabs working:** When one tab closes with a prompt in flight, only its ACP session is closed; the shared agent CLI remains alive and another tab can continue chatting without a restart. _(#419/#425; E2E: `Feature.SharedAgentLifecycle`.)_
- [ ] `C278` `[new]` `[E2E]` **`/new` physically closes the replaced ACP session before creating another:** Replacing a conversation releases the prior ACP session before creating its successor and does not replay the old session. _(#610; E2E: `Feature.AgentProtocolExperience`.)_
- [ ] `C215` `[new]` `[E2E]` **Agent panes are not persisted into saved layout:** Saving and restoring a window layout does not resurrect a previously-open agent pane; restored windows come back without an unexpected agent pane. _(#360/#275.)_

### Built-in agent chat matrix

- [ ] `C057` `[E2E]` `[MANUAL]` **Copilot chat works:** User can send a prompt and Copilot responds successfully.
- [x] `C058` `[UT✓]` `[E2E]` **Copilot missing CLI path works:** Missing Copilot shows actionable setup/auth guidance, not a silent failure. _(UT: `is_cli_available_*` availability check + the auth/setup screen renders `render_auth_sign_in_card` / `render_auth_screen_shows_agent_name`; a missing binary degrades to a guidance screen, not a silent failure. Exercising a truly uninstalled Copilot stays MANUAL.)_
- [ ] `C059` `[E2E]` **Non-Copilot agents chat works:** Each installed+authenticated non-Copilot built-in agent (Claude/Codex/Gemini) connects through its ACP adapter and answers a prompt. _(One consolidated matrix case — all built-in agents share the same agent-pane/ACP path, so per-agent behavioural depth is covered by the Copilot suites.)_
- [ ] `C248` `[new]` `[E2E]` **OpenCode built-in agent chat works:** Selecting OpenCode launches its native `opencode acp` server, reaches a connected agent-pane session, and completes a prompt round-trip. _(#458/#460; E2E: `Feature.OpenCodeAgent`.)_
- [ ] `C239` `[new]` `[E2E]` **Profile WSL agent chat works:** An installed and authenticated agent selected by a WSL profile connects through the helper/master architecture from inside that distro and completes a real chat round trip. _(#481; E2E: `Feature.WslAgentBackend`.)_
- [x] `C060` `[UT✓]` `[E2E]` **Agent auth failure works:** Unauthenticated agents show clear login guidance and can recover after sign-in. _(UT: `auth_error_routes_to_signin_not_connection_lost` (AuthRequired → sign-in, not a generic failure) + the in-pane auth screen renders `render_auth_screen_shows_agent_name` / `render_auth_sign_in_card` / `render_auth_checking_with_status_message` (login guidance + post-sign-in checking state). Driving a real sign-out stays MANUAL.)_
- [ ] `C216` `[new]` `[E2E]` **GitHub Enterprise Copilot sign-in works:** On the auth screen, pressing **E** lets the user enter a GHE domain (e.g. `*.ghe.com`) and sign in; the last-used host is remembered and the device-verification URL targets that host. _(#362.)_
- [ ] `C061` `[E2E]` **Agent restart after settings change works:** Changing the selected agent or model restarts/reconnects cleanly.
- [ ] `C217` `[new]` `[UT~]` `[E2E]` **Master death fails closed and a later pane open starts fresh:** If `wta-master` exits, its helpers and panes exit without automatic session load; a later explicit pane open creates a fresh master, helper, and ACP session. _(#329; E2E: `Feature.AgentMasterDeath`.)_

### Input and rendering

- [ ] `C062` `[E2E]` **Prompt focused appearance is correct:** Input box looks correct when focused.
- [x] `C063` `[UT✓]` `[E2E]` **Prompt out-of-focus appearance is correct:** Input box looks correct when focus leaves the agent pane. _(UT: `render_input_box_intact_when_pane_unfocused` renders with `pane_focused=false` and asserts the input box stays intact — prompt marker + connection placeholder still paint, not blanked/broken; only the caret style dims, input.rs:69/90.)_
- [ ] `C064` `[E2E]` **Typing works:** User can type, edit, and submit prompt text correctly.
- [ ] `C065` `[E2E]` **Paste works:** Pasted multiline text remains in one agent draft without submitting. _(UT: `agent_paste_text_*`; E2E: `Feature.Paste`.)_
- [ ] `C266` `[new]` `[E2E]` **Ctrl+V pastes into the agent input:** Ctrl+V invokes the structured paste path and inserts clipboard text exactly once instead of typing a literal `v`; Ctrl+Shift+V remains a positive control. _(E2E: `Feature.Paste`.)_
- [ ] `C267` `[new]` `[E2E]` **Ctrl+V paste survives input refocus:** After a physical completed-turn interaction and input-dialog click, Ctrl+V pastes into the visible draft. _(E2E: `Feature.Paste` `PasteRefocus`.)_
- [ ] `C268` `[new]` `[E2E]` **Ctrl+V paste stays isolated to its owner tab:** Pasting into one focused agent input does not mutate a sibling tab's agent draft. _(UT: `agent_paste_text_ignores_wrong_window_and_non_owner_helpers`; E2E: `Feature.Paste` `PasteOwnerIsolation`.)_
- [ ] `C234` `[new]` `[E2E]` **Prompt history recall works:** Up/Down recalls submitted prompts from newest to oldest and moves back toward newer entries. _(#478/#479; E2E: `Feature.PromptHistory`.)_
- [ ] `C235` `[new]` `[E2E]` **Prompt history preserves drafts and multiline prompts:** Reviewing history keeps each multiline prompt intact and restores the current unsent draft afterward. _(#478/#479; E2E: `Feature.PromptHistory`.)_
- [ ] `C236` `[new]` `[E2E]` **Prompt history is isolated per tab:** Each agent tab recalls only prompts submitted in that tab. _(#478/#479; E2E: `Feature.PromptHistory`.)_
- [ ] `C262` `[new]` `[UT✓]` `[E2E]` **Completed turns restore multiline prompts when expanded:** A submitted Shift+Enter prompt remains a compact one-line summary while collapsed and restores its original line breaks when expanded. _(#614; UT: `expanded_completed_turn_restores_multiline_prompt`; E2E: `Feature.PromptHistory`.)_
- [ ] `C263` `[new]` `[UT✓]` `[E2E]` **Keyboard selection keeps focused completed turns visible:** Tab and Up/Down navigation scrolls the chat only as needed to keep the focused completed turn visible, without overriding later manual scrolling. _(UT: `render_chat_keeps_keyboard_selected_completed_turn_visible`; E2E: `Feature.CompletedTurnSelection`.)_
- [ ] `C264` `[new]` `[UT✓]` `[E2E]` **Completed turns toggle and select from rendered prompt targets:** A left-button Down/Up anywhere on the same visible prompt row, including its triangle, prefix, text, explicit blank row, or unused row-end space, collapses or expands only that turn and reuses the keyboard selection, highlight, navigation, Esc, and Enter behavior. Hover uses only a hand cursor: action metadata must not add hyperlink underline or tooltip text. Details, separators, drags, overlays, tab changes, scrolling, and stale/offscreen rows do not toggle; clicking the live input dialog clears the history selection and restores draft input. Multiline, wrapped, wide-cell, and double/triple-click paths preserve their expected geometry and text-selection behavior. _(UT: `clicking_multiline_completed_turn_prompt_selects_and_reuses_enter_toggle`, `completed_turn_user_input_hit_spans_full_row_with_wide_cells`, `completed_turn_prompt_rows_expose_state_aware_action_links`, `overlay_preserves_full_rows_and_cell_styles`, `CompletedTurnActionHyperlinksSuppressUnderlines`, `clicking_input_dialog_restores_input_navigation_after_mouse_turn_selection`, `completed_turn_user_input_multi_click_preserves_turn_state_and_text_selection`, `completed_turn_mouse_selection_continues_with_keyboard_navigation`, `completed_turn_triangle_click_ignores_text_drag_and_hidden_chat`, `completed_turn_triangle_hits_follow_visible_scrolled_turns`, `completed_turn_prompt_hits_survive_a_clipped_header_row`; E2E: `Feature.AgentMouse` `CompletedTurnMouse` / `CompletedTurnPromptMouse`; hand cursor and absence of action underline manually verified on the exact deployed Dev build.)_
- [ ] `C242` `[new]` `[UT✓]` `[E2E]` **Mouse wheel scrolls chat without changing the draft:** Wheel input scrolls the chat viewport while Up/Down remain prompt-history controls and the unsent draft stays intact. _(UT: `mouse_wheel_scrolls_chat_without_changing_input_history`; #506; E2E: `Feature.AgentMouse`.)_
- [ ] `C243` `[new]` `[UT✓]` `[E2E]` **Mouse selection copies text and clears after copy:** Double-click selection survives release, `Ctrl+C` copies the exact selected text with confirmation instead of canceling/closing the pane, and the selection is then cleared. _(UT: `mouse_release_does_not_return_text_for_automatic_copy`, `clearing_selection_resets_multi_click_sequence`; #506; E2E: `Feature.AgentMouse`.)_
- [ ] `C265` `[new]` `[UT✓]` `[E2E]` **Right-click copies the agent text selection:** A physical right-click copies the exact WTA-owned selection through the OS clipboard, clears it, shows the existing copied confirmation, and never also pastes; the next right-click follows the no-selection Default Paste path without replaying selected text. _(UT: `right_click_copy_event_is_forwarded`, `right_click_copies_and_clears_text_selection`; E2E: `Feature.AgentMouse` `RightClickCopy`.)_
- [ ] `C269` `[new]` `[UT✓]` `[E2E]` **Right-click without text selection pastes throughout the Chat pane:** A physical right-click on history, input, blank space, or a completed-turn navigation highlight requests Default Paste exactly once into the visible owner draft without submitting; only an actual WTA text selection takes copy precedence. _(UT: `right_click_without_text_selection_requests_owner_default_paste`, `default_paste_request_is_chat_only`; E2E: `Feature.AgentMouse` `RightClickPaste`.)_
- [ ] `C218` `[new]` `[UT✓]` `[E2E]` **Image paste (Alt+V) works:** A copied screenshot (`CF_DIB`/`CF_DIBV5`) appears as a uniquely numbered cyan inline token such as `[image: image-1.png]`; copied image files retain their real file name. The attachment is sent as an ACP image content block on the next prompt. Left/Right cross the token atomically, Backspace/Delete remove the whole token and attachment, and Esc/Ctrl+C clear the whole draft. The action is gated on the agent advertising image support. When the agent does not support images, or the clipboard has no image, it does not paste but surfaces a clear system message rather than silently ignoring the keypress. _(UT: `clipboard_image`, `image_attachment_*`, and `mock_agent_tests` `seen_images` side-channel; #354.)_
- [ ] `C254` `[new]` `[UT✓]` `[E2E]` **Image attachment tokens edit atomically:** Left/Right cross an inline image token as one input unit, and Backspace removes the complete attachment without damaging adjacent prompt text. _(UT: `image_attachment_left_and_right_skip_the_whole_inline_token`, `image_attachment_backspace_in_text_preserves_images`; #536; E2E: `Feature.AgentImageAttachmentEditing`.)_
- [ ] `C066` `[E2E]` **Keyboard navigation works:** Arrow keys, Tab completion, Ctrl combinations, and Esc behave correctly.
- [x] `C067` `[UT✓]` `[E2E]` `[MANUAL]` **IME/non-ASCII input works:** IME and non-ASCII input are usable if the release supports localized typing. _(UT: `render_agent_input_accepts_non_ascii` types accented-Latin/Greek/CJK via the real key handler and asserts the input buffer holds them verbatim (multi-byte caret advance) + they render. E2E send path (wtcli send-keys) cannot carry non-ASCII, so the product side is UT-covered; IME composition stays MANUAL.)_
- [ ] `C068` `[UT✓]` `[E2E]` **Streaming output renders correctly:** Agent response chunks, tool calls, plans, status lines, and literal JSON render without corruption. _(UT: `streaming_two_chunks_coalesce_in_app_chat`, `tool_call_surfaces_card_in_chat`, `tool_call_completion_updates_card_status` (in-place, no dup), `plan_surfaces_card_in_chat`, `render_chat_all_message_variants`; Assistant JSON remains ordinary chat text.)_
- [ ] `C279` `[new]` `[UT✓]` `[E2E]` **ACP tool details and transcript order survive the real process boundary:** Execute commands and output, plans, and trailing prose retain ACP arrival order through the stdio agent, master, Helper, and rendered pane. _(#601/#611/#612; E2E: `Feature.AgentProtocolExperience`.)_
- [ ] `C280` `[new]` `[UT✓]` `[E2E]` **Clarification modal returns the selected answer to the requesting ACP session:** A blocking agent question renders its choices, supports character/word navigation and deletion in its freeform response, and returns the exact choice/index or cursor-edited answer to the owning ACP session. _(#606; E2E: `Feature.AgentProtocolExperience`.)_
- [x] `C069` `[UT✓]` `[E2E]` **Permission UI works:** When the agent requests a command/tool permission, the user can allow or reject it. _(UT: `permission_allow_round_trips_to_agent`, `permission_reject_round_trips_to_agent`, `permission_quick_allow/reject_key_round_trips_to_agent`, `render_permission_card_shows_options`, `render_permission_compact_shows_hint`; the `y`/`n` quick-key case-match bug was fixed here.)_
- [ ] `C070` `[E2E]` **Insert into pane works:** A validated Direct Helper Proposal can be inserted into the target terminal pane without running.
- [ ] `C253` `[new]` `[E2E]` **Insert returns keyboard focus to the target shell pane:** After Insert delivers a command without running it, normal window keyboard input continues in that target pane. _(#533; E2E: `Feature.AgentProposalFocus`.)_
- [ ] `C071` `[E2E]` **Run in pane works:** A validated Direct Helper Proposal can be run in the target terminal pane.
- [ ] `C072` `[E2E]` **Command target is correct:** The Helper-injected trusted target routes Insert/Run to the intended active pane, not the agent pane itself or another tab.

### Agent pane slash commands

- [x] `C073` `[UT✓]` **`/help` works:** Shows available commands.
- [x] `C074` `[UT✓]` **`/clear` works:** Clears chat view as expected without breaking the session.
- [x] `C075` `[UT✓]` **`/new` works:** Starts a fresh session.
- [x] `C076` `[UT✓]` **`/fix` works:** Runs a manual failed-command analysis using recent terminal context. _(UT: classification and the internal `/fix` submission tests.)_
- [x] `C077` `[UT✓]` **`/restart` works:** Restarts the agent stack and reconnects to a clean session. _(UT: `slash_restart_resets_connection_and_clears_sessions`.)_
- [x] `C078` `[UT✓]` **`/stop` works:** Stops/cancels an in-progress turn.
- [x] `C079` `[UT✓]` **`/sessions` works:** Switches to session-management view. _(UT: `slash_sessions_opens_agents_view`.)_
- [ ] `C080` `[UT✓]` `[E2E]` **`/model` works:** Opens/selects model where supported; unsupported agents fail gracefully. _(UT: `slash_model_*`; picker render covered by `render_model_picker_lists_models`, full UI flow still E2E.)_
- [ ] `C281` `[new]` `[UT✓]` `[E2E]` **Slash command search matches substrings and ranks the match:** Typing a middle substring filters the command popup and selects the matching command. _(#615; E2E: `Feature.AgentPopup`.)_
- [ ] `C282` `[new]` `[UT✓]` `[E2E]` **ACP session config picker preserves order and hot-applies a selection:** `/config` renders Agent-provided options in order, sends the selected value to the live ACP session, and reflects its update without restarting. _(#616; E2E: `Feature.AgentProtocolExperience`.)_
- [ ] `C283` `[new]` `[UT✓]` `[E2E]` **Agent pane title reflects the confirmed active model:** After ACP confirms the session model, the XAML agent-pane title displays that model rather than stale configured state. _(#634; E2E: `Feature.AgentProtocolExperience`.)_
- [ ] `C255` `[new]` `[UT✓]` `[E2E]` **ACP model updates refresh the active picker:** A model reported by a later ACP `config_option_update` replaces stale `session/new` selection state in the active session's `/model` picker. _(UT: `session_notification_routes_model_config_update`, `model_config_update_refreshes_active_session_picker`; #538; E2E: `Feature.AgentModelSync`.)_
- [ ] `C256` `[new]` `[UT✓]` `[E2E]` **`/model` hot-applies without restarting the agent:** Selecting another model sends the live ACP session update while preserving the current helper and agent process. _(UT: `slash_model_hot_applies_cloud_model_to_live_session`; #554; E2E: `Feature.AgentModelLifecycle`.)_
- [ ] `C257` `[new]` `[E2E]` **Settings model changes restart and reconnect the agent:** Changing the configured cloud model rebuilds the shared agent stack, reconnects, and applies the new model to the fresh session. _(#554; E2E: `Feature.AgentModelLifecycle`.)_
- [ ] `C259` `[new]` `[UT✓]` `[E2E]` **BYOK provider serves the selected model end to end:** An OpenAI-compatible provider selected in Settings is isolated into Copilot, uses its optional Credential Manager API key, receives chat-completions requests for the configured model, and remains the only `/model` choice. _(UT: custom-provider environment, credential resolution, and settings launch-configuration tests; #447; E2E: `Feature.ByokProvider`.)_
- [ ] `C260` `[new]` `[UT✓]` `[E2E]` **Leaving BYOK restarts and restores cloud models:** Clearing the custom model selection rebuilds the shared agent stack, reconnects Copilot without provider overrides, and restores its native cloud catalog. _(UT: clean cloud discovery and settings rebuild tests; #447; E2E: `Feature.ByokProvider`.)_
- [ ] `C261` `[new]` `[UT✓]` `[E2E]` **Compact height keeps the recommendation and input usable:** At the real Agent Pane splitter minimum, the selected recommendation summary, Run/Insert actions, and editable input remain visible, and Insert still targets the owning shell without discarding its draft. _(UT: compact action-panel planning, recommendation rendering, and minimum-size tests; #580; E2E: `Feature.AgentCompactLayout`.)_
- [ ] `C258` `[new]` `[UT✓]` `[E2E]` **Proposal MCP routing is isolated per tab:** Each ACP session receives a distinct proposal MCP server identity, and tool calls route only to that session's owning Helper even after another tab connects. _(UT: `server_configs_isolate_session_identity_and_capability`, proposal MCP capability routing tests; #560; E2E: `Feature.ProposalMcpRouting`.)_
- [ ] `C284` `[new]` `[UT✓]` `[E2E]` **New-tab command workspaces accept a split-direction hint across Session MCP:** A validated `create_workspace` request with a command may retain a `split_direction` hint when its placement is `new_tab`. _(#599; E2E: `Feature.AgentProtocolExperience`.)_
- [ ] `C285` `[new]` `[UT✓]` `[E2E]` **Empty workspaces open without sending a command across Session MCP:** After confirmation, `create_workspace` with `command` omitted creates a new empty terminal tab without typing the proposal summary into its shell. _(#683; E2E: `Feature.AgentProtocolExperience`.)_
- [ ] `C286` `[new]` `[UT✓]` `[E2E]` **Delegated tasks reach the configured agent in a new workspace across Session MCP:** After confirmation, `delegate_task_in_new_workspace` opens a new terminal tab with the configured delegate command and passes it the exact requested task. _(#683; E2E: `Feature.AgentProtocolExperience`.)_
- [x] `C081` `[UT✓]` **Unknown slash command is safe:** Unknown `/command` does not lose user input or crash.
- [ ] `C225` `[E2E]` **`/agent` picker works:** `/agent` opens a keyboard-operable picker containing the current installed/allowed agents, and selecting the current agent is a safe no-op.
- [ ] `C241` `[new]` `[E2E]` **`/agent` completion selection is safe:** Enter activates the highlighted matching agent without rebuilding the pane or changing the global default when it is already selected. _(#487; E2E: `Feature.PerTabAgent`.)_
- [ ] `C226` `[E2E]` **Invalid `/agent` selection is safe:** `/agent <id>` rejects an unavailable agent without rebuilding the pane or changing the global default.
- [ ] `C082` `[E2E]` **Esc/back navigation works:** User can return from popups/session/model views to chat.

### Chat/session view switching

- [ ] `C083` `[UT✓]` `[E2E]` **Session view opens from chat:** `/sessions`, session button, or `Ctrl+Shift+/` opens the session view. _(UT: `slash_sessions_opens_agents_view` + `DefaultAgentKeybindings`.)_
- [ ] `C084` `[E2E]` **Chat view restores:** User can return to chat view after opening session view.
- [x] `C085` `[UT✓]` `[E2E]` **View switch preserves input:** Draft prompt text is not unexpectedly lost when switching views. _(E2E `Feature.SessionList` 'View switch preserves the draft input': types a draft, switches chat↔sessions via the bottom-bar buttons (SessionToggleButton/AgentToggleButton — not the `/sessions` slash that would type into the draft, not Esc which is overloaded), observing the view via the `AgentLabelText` UIA element (winapp get-value, no jsonl pane ambiguity), then asserts the draft survives. UT: `view_switch_preserves_chat_draft_input` drives the Esc key handler and asserts draft + cursor survive.)_
- [ ] `C086` `[E2E]` **View switch preserves connection:** Agent connection state remains correct after switching views.

## 3. Auto error handling flow

**Feature definition:** Auto error handling detects terminal command failures. With `Detect errors and send them to the agent for fixes automatically`, it captures relevant pane context, asks the configured agent for a fix, and accepts actionable fixes only through a Direct Helper Proposal before offering Insert or Run.

### Shell integration and detection

- [ ] `C087` `[E2E]` **PowerShell shell integration installed:** Supported PowerShell profiles emit command-finished events, including non-zero marks for PowerShell-level failures on Windows PowerShell 5.1.
- [ ] `C219` `[new]` `[E2E]` **Bash / WSL shell integration installed:** Supported bash and WSL-bash profiles emit command-finished events, and the injected `PROMPT_COMMAND` is safe under `set -u` (no errors in strict-mode shells). _(#340.)_
- [ ] `C250` `[new]` `[E2E]` **Bash PROMPT_COMMAND rewrites preserve semantic prompt boundaries:** In Intelligent Terminal, a user hook that rebuilds `PS1` still produces one ordered `OSC 133;D/A/B` cycle per command; the same user-wide integration script stays inert in other terminals. _(#468; E2E: `Feature.BashPromptIntegration`.)_
- [ ] `C220` `[new]` `[E2E]` **Shells self-report identity (`OSC 9001;ShellType`):** The terminal knows which shell owns a pane — including after a nested shell (`pwsh` → `wsl` → `exit`) returns — so Auto error handling proposes commands for the *current* shell (no PowerShell commands inside a WSL/bash pane). `wtcli list-panes` exposes the live shell + version per pane. _(#345.)_
- [ ] `C088` `[E2E]` **Missing shell integration is safe:** Without shell integration, failures do not crash or produce broken UI.
- [x] `C089` `[UT✓]` **Failure detection works:** A failing command emits an event and is detected by Intelligent Terminal. _(UT: `classify_wt_event`.)_
- [x] `C090` `[UT✓]` **Successful commands ignored:** Successful commands do not produce Auto error handling activity. _(UT: failure classification.)_
- [ ] `C230` `[new]` `[E2E]` **PowerShell parser errors produce exactly one agent request:** A malformed PowerShell command emits a non-zero completion mark and submits one Auto error handling turn when the third option is selected. _(#474; E2E: internal parser pipeline coverage.)_
- [ ] `C231` `[new]` `[E2E]` **Parser-error prompt redraw does not repeat the request:** Pressing Enter on the fresh prompt after a parser error does not replay the prior failure or submit another Auto error handling turn. _(#474; E2E: internal parser pipeline coverage.)_
- [ ] `C232` `[new]` `[E2E]` **Successful PowerShell commands produce no request:** A normal PowerShell command emits a zero-exit completion mark and does not submit an Auto error handling turn. _(#474; E2E: internal parser pipeline coverage.)_
- [ ] `C233` `[new]` `[E2E]` **Handled non-terminating PowerShell errors produce no request:** A command that handles a non-terminating error and completes successfully remains distinct from a parser failure. _(#474; E2E: internal parser pipeline coverage.)_
- [x] `C091` `[UT✓]` **`Off` suppresses processing:** Failures do not produce Auto error handling activity.
- [x] `C092` `[UT✓]` **`Detect errors automatically` observes failures locally:** Failure notifications are observed without contacting the agent.
- [x] `C093` `[UT✓]` **`Detect errors automatically` suppresses agent calls:** Detection can show expected local UI but does not ask the agent for a fix.
- [x] `C094` `[UT✓]` **`Detect errors and send them to the agent for fixes automatically` contacts the agent:** With a connected helper, a proposed fix is requested.
- [x] `C095` `[UT✓]` **Cold-start behavior is acceptable:** If failure happens before the helper is connected, UI stays stable and no stale proposed fix appears later.

### Auto error handling with the agent pane

- [ ] `C096` `[E2E]` **Visible agent pane flow works:** Auto error handling works when the agent pane is visible.
- [ ] `C097` `[E2E]` **Stashed agent pane flow works:** Auto error handling works when the per-tab agent pane is pre-warmed but hidden.
- [ ] `C098` `[E2E]` **Auto error handling opens/restores UI correctly:** The proposed-fix UI appears in the expected pane/tab and does not steal unrelated focus unexpectedly.
- [ ] `C099` `[E2E]` **Insert proposed fix works:** A proposed fix can be inserted into the source pane.
- [ ] `C100` `[E2E]` **Run proposed fix works:** A proposed fix can be run in the source pane.
- [ ] `C101` `[UT✓]` `[E2E]` **Reject/dismiss works:** The user can dismiss a proposed fix without side effects.
- [ ] `C102` `[UT✓]` `[E2E]` **Auto error handling target pane is correct:** A failure in one pane does not offer or run a fix in the wrong pane. _(UT: target-tab routing.)_
- [ ] `C103` `[E2E]` `[MANUAL]` **Auto error handling with Copilot works:** Copilot submits a valid Direct Helper Proposal and the card presents a useful proposed fix.
- [ ] `C104` `[E2E]` **Auto error handling with non-Copilot agents works:** A non-Copilot built-in agent (Claude/Codex/Gemini) and a custom ACP agent each execute the canonical command, complete permission arming, and submit a usable Direct Helper Proposal through the same path.
- [ ] `C221` `[new]` `[E2E]` `[MANUAL]` **Environment-aware answers/fixes:** For a failed or "how do I use X" prompt, the agent investigates the live environment first — checks whether the command actually exists on PATH and surfaces local scripts / near-matches for a mistyped command — instead of giving generic advice or fixing a nonexistent command. _(#306.)_
- [ ] `C246` `[new]` `[E2E]` **Profile-defined PowerShell aliases resolve through packaged WTA:** A command defined only in the current user's PowerShell profile is reported as an alias with its real target by the shipped `wta resolve-command` CLI. _(#286/#418; E2E: `Feature.CommandResolution`.)_

### Auto error handling across layout changes

- [ ] `C105` `[UT~]` `[E2E]` **Split pane routing works:** A failure in a split pane is routed to the correct tab/pane. _(UT: tab/pane routing.)_
- [x] `C106` `[UT✓]` `[E2E]` **Moved tab routing works:** After moving a tab to another window, failures route to the correct agent pane. _(UT: owner-tab routing; the agent-request half has dedicated E2E coverage.)_
- [x] `C107` `[UT✓]` `[E2E]` **Multi-window routing works:** Multiple windows with agent panes do not cross-route proposed fixes. _(UT: helpers filter inbound events by window and owner-tab identity.)_
- [ ] `C108` `[UT~]` `[E2E]` **Closed pane cleanup works:** Auto error handling does not target a pane that has already closed.

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
- [ ] `C276` `[new]` `[E2E]` **Non-ASCII working directories survive the resume launch path:** Resuming a session whose stored cwd contains non-ASCII characters (e.g. `D:\Obsidian\我的笔记`) opens a pane that connects and really starts in that directory, instead of failing with `0x8007010b ERROR_DIRECTORY` because the path was transcoded through the process ANSI code page. _(#641; E2E: `Feature.NonAsciiCwd`.)_
- [ ] `C249` `[new]` `[E2E]` **OpenCode historical session resumes from the session picker:** A real OpenCode session discovered through ACP `session/list` appears with its stored title and opens a new tab using `opencode --session <id>` with the prior transcript restored. _(#464; E2E: `Feature.OpenCodeSessionResume`.)_
- [x] `C126` `[UT✓]` `[E2E]` **Restore old agent-pane session:** Supported agent-pane sessions resume through agent-pane/session-load path when enabled. _(UT: `session_mgmt::class_a_ended_enter_resumes_in_agent_pane_when_supported` — ResumeInAgentPane decision.)_
- [x] `C127` `[UT✓]` `[E2E]` **Unsupported restore is clear:** Unknown CLI, missing resume support, or missing on-disk session shows a clear not-resumable message. _(UT: `session_mgmt::class_a_ended_enter_not_resumable_when_load_unsupported`, `class_b_historical_enter_not_resumable_when_cli_has_no_flag`, `unknown_cli_not_resumable_for_any_origin_or_liveness` — NotResumable reasons.)_
- [ ] `C128` `[UT✓]` `[E2E]` **Enter behavior works:** Enter performs the expected focus/resume action.
- [x] `C129` `[UT✓]` **Only a bare Enter activates a row:** A modified Enter (Shift, Alt, Ctrl) neither focuses nor resumes, and is swallowed by the picker instead of leaking to the chat input. _(UT: `modified_enter_on_live_row_dispatches_nothing`, `modified_enter_on_class_a_dead_row_dispatches_nothing`, `modified_enter_on_class_b_dead_row_dispatches_nothing` — each also asserts a bare Enter on the same row still dispatches. Not E2E: "nothing happened" plus a FocusPane that moves WT focus without dismissing the view has no stable E2E observable, and the MVP picker shows only Class B shell sessions whose panes are usually already closed.)_

### Session-management scope and custom agents

- [x] `C130` `[UT✓]` `[E2E]` **Built-in agents tracked:** Copilot, Claude, Codex, and Gemini sessions are tracked when hooks/session support is enabled. _(UT: `CliSource::parse`/`from_agent_id` recognize all four agents — agent_sessions.rs:35-68; `session_registry::registry_assigns_codex_cli_source_when_session_started_via_agent_id` + `agent_sessions::iter_sorted_filtered_keeps_only_matching_cli_source` exercise cli_source tracking/filtering.)_
- [ ] `C251` `[new]` `[E2E]` **OpenCode shell sessions are tracked by installed hooks:** The packaged installer lands the managed OpenCode plugin; a real shell-hosted session crosses the plugin/PowerShell/WT protocol boundary into the session picker, while OpenCode ACP mode suppresses duplicate plugin events. _(#476; E2E: `Feature.OpenCodeHooks`.)_
- [x] `C131` `[UT✓]` `[E2E]` **Custom agent safe behavior:** Custom agents do not crash session management and do not show strange/broken UI. _(UT: `session_mgmt::unknown_cli_not_resumable_for_any_origin_or_liveness` — an unknown/custom CLI degrades to a safe NotResumable(UnknownCli) for every origin and liveness rather than crashing.)_
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
- [ ] `C229` `[UT✓]` `[E2E]` **Delegate session title is clean:** The delegate's baked `## Terminal Context (pane …)` first message never surfaces as the session's `session/list` title — no injected pane-context/GUID leak, and the row heals to the CLI's real summary.

## 6. Custom agents

**Feature definition:** Settings can configure one custom command for the agent pane and one custom command for delegate mode. Custom agents are not configured from FRE.

### Custom agent pane

- [ ] `C145` `[E2E]` **Custom agent is Settings-only:** FRE does not expose custom-agent creation.
- [ ] `C146` `[UT✓]` `[E2E]` **Add custom ACP agent:** In Settings, add an agent-pane custom command such as `qwen.cmd --acp`. _(UT: `DeriveCustomAgentId`.)_
- [x] `C147` `[UT✓]` **Save custom ACP agent:** Saving persists `custom:<cmd>`/custom command settings. _(UT: CustomAgentAndPolicyTests round-trip.)_
- [ ] `C148` `[UT✓]` `[E2E]` **Edit custom ACP agent:** Editing updates the command used by new agent panes.
- [ ] `C149` `[UT~]` `[E2E]` **Delete custom ACP agent:** Deleting returns to a valid built-in/default selection.
- [ ] `C150` `[UT~]` `[E2E]` **Model selection visible:** Model picker/textbox remains visible when custom agent is selected.
- [ ] `C151` `[E2E]` **Custom agent runs the standard agent-pane behaviours:** A configured custom ACP agent can chat, request a command/tool action, insert/run into the pane, and participate in Auto error handling — the same agent-pane behaviours verified in depth with Copilot.
- [ ] `C152` `[UT~]` `[E2E]` **Custom failure is safe:** Bad command, missing executable, or non-ACP behavior shows a clear error and does not crash Terminal. _(UT: failure classification.)_

### Custom delegate agent

- [x] `C153` `[UT✓]` `[E2E]` **Add custom delegate agent:** In Settings, add a delegate custom command such as `qwen.cmd`. _(UT: `DeriveCustomAgentId` (AIAgentsViewModel) derives the `custom:<id>` from the entered command; E2E-adjacent: `Feature.CustomDelegate` runs a configured custom delegate command.)_
- [x] `C154` `[UT✓]` **Save custom delegate agent:** Saving persists the delegate custom command. _(UT: round-trip.)_
- [ ] `C155` `[UT✓]` `[E2E]` **`Alt+Shift+B` uses custom delegate:** Background delegate shortcut launches the custom command. _(UT: `DefaultAgentKeybindings` binding + custom `EffectiveDelegateAgent` resolution.)_
- [ ] `C156` `[UT✓]` `[E2E]` **`Alt+Shift+/` uses custom delegate:** Agent-delegation command palette launches the custom command. _(UT: `DefaultAgentKeybindings` + `AgentActionsParse` delegation mode.)_
- [ ] `C157` `[UT~]` `[E2E]` **Custom delegate cwd is correct:** Custom delegate starts in the source pane's cwd.
- [ ] `C158` `[UT~]` `[E2E]` **Custom delegate errors are clear:** Bad command or auth/setup failure is actionable.

## 7. Multi-pane and multi-window behavior

**Feature definition:** Agent state, session routing, and Auto error handling routing are per-tab and per-window. Moving tabs/windows should not lose or cross-route agent context.

- [ ] `C159` `[E2E]` **Split pane does not break chat:** Splitting the terminal pane keeps agent pane chat usable.
- [ ] `C160` `[UT~]` `[E2E]` **Split pane target selection is correct:** Agent insert/run and Auto error handling target the intended non-agent pane. _(UT: routing core.)_
- [ ] `C161` `[UT~]` `[E2E]` **Multiple tabs work:** Each tab has its own agent pane/session state. _(UT: per-tab state.)_
- [ ] `C162` `[E2E]` **Multiple agent panes work:** Opening agent panes in multiple tabs does not mix conversations.
- [ ] `C227` `[E2E]` **Per-tab agent switching is isolated:** `/agent <id>` can switch one tab to a different built-in agent without rebuilding sibling tabs, losing either conversation, or restarting the shared master.
- [ ] `C228` `[E2E]` **Global agent defaults respect per-tab overrides:** New tabs inherit the global agent; changing that default rebuilds follower tabs while preserving tabs with an explicit runtime override.
- [ ] `C163` `[UT~]` `[E2E]` **Move tab to new window preserves chat:** Both the `moveTab` action and tab-strip drag preserve the live helper, ACP SessionId, chat history, and routing identity; neither path performs crash recovery or `session/load`. _(UT: `tab_renamed_rekeys_active_tab_and_session_map`, `tab_renamed_sends_rename_session_request_to_acp_client`.)_
- [ ] `C224` `[new]` `[E2E]` **Agent-created terminals inherit the active profile:** A terminal/tab the agent opens inherits the active pane's profile (e.g. an agent working in an Ubuntu session spawns new tabs in Ubuntu, not the default PowerShell profile). _(#366, closes #351.)_
- [x] `C164` `[UT✓]` `[E2E]` **Move tab to new window preserves session routing:** Session events remain associated with the moved tab. _(UT: `tab_renamed_rekeys_active_tab_and_session_map`, `master_tab_rename_rekeys_live_and_orphan_ownership`, and `active_retirement_follows_tab_rename_and_clears_moved_fence_on_disconnect`; E2E: `Feature.MultiWindow` sends a fresh prompt to the moved agent pane by its pinned session id after the move and asserts it answers, LLM-skip-guarded.)_
- [ ] `C165` `[UT~]` `[E2E]` **Move tab to new window preserves Auto error handling:** Failures still route to the moved tab/pane.
- [x] `C166` `[UT✓]` `[E2E]` **Multiple windows do not cross-route:** Events from one window do not mutate another window's agent pane/session UI. _(UT: `wt_event_critical_from_other_tab_does_not_surface_in_owner_tab` — a helper owning tab A DROPS a connection-failure event broadcast from tab B (no banner, no chat, no notification), the exact cross-route isolation contract; helpers filter inbound events by window_id + owner_tab_id.)_
- [ ] `C167` `[E2E]` **Close source window is safe:** Closing a source window after moving a tab does not kill the moved tab's agent state.
- [ ] `C168` `[E2E]` **Close target tab cleans up:** Closing moved tabs cleans up helper/session state without affecting other tabs.
- [ ] `C275` `[E2E]` **Move tab back to source window preserves the same ACP session:** Redocking an agent tab keeps its helper, chat, and ACP session alive and does not emit session teardown.

## 8. Agent hooks and session tracking

**Feature definition:** Agent hooks record shell-pane agent sessions and enable session-management state for supported CLIs.

- [ ] `C169` `[E2E]` **Install hooks from FRE works:** Session-management toggle can install supported hooks during first run.
- [ ] `C170` `[E2E]` **Install hooks from Settings works:** Install hooks button works after FRE.
- [ ] `C171` `[E2E]` **Per-CLI hook install works:** Each supported CLI (Copilot/Claude/Gemini) installs its hook or reports why it can't; Codex hook/session support behaves per the current implementation.
- [ ] `C274` `[new]` `[UT~]` `[E2E]` **Bundled hook runs inside its agent CLI:** A real agent CLI session executes the shipped hook command through its own shell and its events reach Terminal. _(#571; UT: `bundled_hook_commands_run_in_every_shell` runs each bundle's command line in that CLI's shell; E2E adds the CLI accepting the bundle.)_
- [ ] `C265` `[new]` `[E2E]` **Hook failure never blocks the agent CLI:** When the protocol server is unreachable the hook fails silently and the agent CLI still completes its turn. _(#571.)_
- [ ] `C266` `[new]` `[UT✓]` `[E2E]` **Uninstalling Terminal never blocks or spams the agent CLI:** With `wtcli.exe` gone from `PATH` — the state an Intelligent Terminal uninstall leaves behind — every stale hook registration still exits 0 and stays silent. No bundle subscribes a fail-closed per-tool-call hook anymore, so no CLI can have its tool calls denied; the guards cover the remaining lifecycle hooks, which would otherwise report a failure per turn. _(UT: `copilot_hook_variants_exit_zero_when_the_bridge_is_missing` + `codex_hooks_exit_zero_when_the_bridge_is_missing` + `gemini_hooks_exit_zero_when_the_bridge_is_missing` + `claude_hooks_exit_zero_when_the_bridge_is_missing` + `bare_hook_command_still_fails_when_the_bridge_is_missing`.)_
- [ ] `C267` `[new]` `[E2E]` **Shipped hook guards do not swallow the happy path:** Every CLI bundle's shipped command, run in the shell that CLI dispatches hooks through, still delivers its event while the bridge is present. _(#571; the uninstall guards force exit 0 either way, so delivery — not exit status — is the only oracle that separates a working guard from one wrapped around a broken command.)_
- [ ] `C273` `[new]` `[E2E]` **OpenCode plugin reaches the bridge without a shell:** OpenCode spawns `wtcli` as an argv array rather than a shell string, so it cannot resolve the MSIX app-execution alias on `PATH`; Terminal injects `WTCLI_PATH` and the plugin's events still arrive. _(#571; the plugin swallows spawn errors by design, so only a delivered event proves it.)_
- [ ] `C270` `[new]` `[E2E]` **Legacy hook bundle still delivers after an upgrade:** Hooks installed by a pre-#571 Terminal keep reaching a post-#571 Terminal, because `wtcli send-event` and the `agent_event` envelope stayed compatible. _(#571; the installed bundle is a copy in the CLI's plugin cache that an upgrade never rewrites, and the auto-refresh can fail while a CLI process holds the plugin directory open.)_
- [ ] `C271` `[new]` `[E2E]` **Legacy hook bundle degrades quietly without Terminal:** With `WT_COM_CLSID` unset — the state an uninstall leaves — a stale pre-#571 hook exits 0, prints nothing, and publishes nothing. _(#571.)_
- [ ] `C278` `[new]` `[E2E]` **Legacy hook bundle without WT_SESSION stays unattributed:** A legacy PowerShell hook whose process never inherited `WT_SESSION` still publishes, but with an empty `pane_id` instead of the focused pane's — so it cannot evict the focused pane's real session or misdirect Enter in the session list. _(#657; `wtcli send-event` no longer falls back to `GetActivePane()`.)_
- [ ] `C172` `[E2E]` **Hook remove works:** Removing a hook disables future session tracking for that CLI.
- [x] `C173` `[UT✓]` `[E2E]` **Disabled plugin is respected:** Disabled agent plugin is skipped and not force-enabled. _(UT: `decide_skip_when_disabled`.)_
- [x] `C174` `[UT✓]` `[E2E]` **Hook auto-upgrade works:** After package upgrade, previously installed hooks are updated silently when bundle version changes. _(UT: `decide_upgrade` + `upgrade_state` round-trip.)_
- [x] `C175` `[UT✓]` `[E2E]` **Opt-in preserved:** Auto-upgrade does not install hooks into a CLI the user never opted into. _(UT: `decide_skip_when_not_installed`.)_
- [ ] `C176` `[E2E]` **Hook logs are available:** Hook decisions and failures are visible in the expected WTA log files.

## 9. Packaging, process, and protocol integration

**Feature definition:** Packaged Intelligent Terminal includes WTA/wtcli integration and uses the packaged COM protocol server correctly.

- [ ] `C177` `[E2E]` **Packaged `wta.exe` is present:** WTA is deployed next to WindowsTerminal in the package layout.
- [ ] `C178` `[E2E]` **Packaged identity works:** WTA/wtcli can activate the Terminal protocol COM server from packaged context.
- [ ] `C179` `[E2E]` **Wrong unpackaged WTA is not used:** The agent pane and Auto error handling do not accidentally use a stale dev-build WTA.
- [ ] `C180` `[E2E]` **`WT_COM_CLSID` is injected:** Shell panes and agent panes inherit protocol discovery environment as expected.
- [ ] `C181` `[E2E]` **`wtcli list-panes` works:** Basic WT protocol query succeeds from a pane.
- [ ] `C182` `[E2E]` **`wtcli capture-pane` works:** Pane output capture succeeds.
- [ ] `C183` `[E2E]` **`wtcli send-keys`/send input path works:** Insert/run operations can send input to the target pane.
- [ ] `C184` `[E2E]` **`wtcli listen` works:** Event subscription receives shell/agent events.
- [ ] `C277` `[new]` `[E2E]` **Delivers a 64 KB JSON payload intact through SendEvent:** WTA writes oversized status/model payloads through `wtcli publish --stdin`, and Terminal receives every byte without hitting the Windows command-line limit. _(#652; E2E: `Feature.WtcliPublishStdin`.)_
- [ ] `C185` `[E2E]` **WTA master starts:** One master process starts per Terminal process when needed.
- [ ] `C186` `[E2E]` **WTA helper starts per tab/pane:** Agent pane helper starts and connects to master.
- [ ] `C187` `[E2E]` **Master/helper crash recovery is acceptable:** Crashes or exits recover or surface an actionable error.

## 10. Diagnostics, logging, and supportability

**Feature definition:** Release builds should leave enough diagnostics for support without overwhelming the user.

- [ ] `C188` `[E2E]` **WTA logs are written:** WTA process logs are created in the expected package-private log directory.
- [ ] `C189` `[E2E]` **C++ agent pane log is written:** Terminal-side agent pane log is created.
- [ ] `C190` `[E2E]` **Native hook bridge publishes events:** `wtcli agent-hook` reads stdin, redacts prompt content, and publishes a pane-scoped hook event. _(Also asserts the full redaction set — `prompt`, `tool_result`, `transcript_path`, `messages`, `model` — since `BuildAgentHookEventJson` has no unit-test project, only a fuzzer.)_
- [ ] `C268` `[new]` `[E2E]` **Hook payload keeps interactive tool input:** `tool_input` is dropped for ordinary tool calls but retained for `ask_user`, so the proposal UI still gets its question without publishing every shell command an agent runs. _(#571.)_
- [ ] `C269` `[new]` `[E2E]` **Hook bridge ignores shells outside Terminal:** A hook fired without `WT_SESSION` exits 0 and publishes nothing, so an unrelated process cannot inject pane-attributed agent events. _(#571.)_
- [ ] `C272` `[new]` `[E2E]` **Hook events stay inside their broadcast budget:** An oversized routing field — not only an oversized payload — is dropped rather than broadcast, so every subscriber can budget its queue. _(#571; payload truncation cannot shrink `agent_session_id`, which is read from the hook JSON on stdin.)_
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
- [ ] `C202` `[MANUAL]` **High contrast theme works:** FRE, Settings, agent pane, and Auto error handling UI remain readable.
- [ ] `C203` `[MANUAL]` **Light/dark theme works:** UI is readable in both themes.
- [ ] `C204` `[MANUAL]` **Text scaling works:** 125%, 150%, and 200% scaling do not clip critical controls.
- [x] `C205` `[UT✓]` `[MANUAL]` **Localization strings are present:** New user-facing strings are localized or intentionally locked. _(UT: `locale_parity_tests::every_locale_has_all_en_us_keys` enforces that every WTA `locales/*.yml` (all 89, incl. pseudo-locales) contains every en-US key — so no agent-feature user-facing string is missing a translation. The C++ `.resw` locales remain enforced by the loc pipeline/manual.)_
- [x] `C206` `[UT✓]` `[MANUAL]` **Pseudo-locales work:** qps pseudo-locales do not clip or corrupt layout. _(UT: `locale_parity_tests::every_locale_has_all_en_us_keys` iterates every `locales/*.yml` including qps-ploc/ploca/plocm and proves each is present + key-complete, so the pseudo-locales load with no missing-key en-US fallthrough. Visual clip/overflow inspection stays MANUAL.)_
- [ ] `C207` `[UT~]` `[MANUAL]` **RTL works:** RTL layout is mirrored where expected. _(UT: `IsRtlLocale`.)_

## 12. Release decision

- [ ] `C208` `[MANUAL]` **All P0/P1 issues resolved:** No blocking agent pane, Auto error handling, FRE, session, custom-agent, or packaging bugs remain.
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
- Auto error handling flow, logging, and runtime layout: `AGENTS.md`.
