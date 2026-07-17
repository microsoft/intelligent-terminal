@{
    # Checklist item title (AS STRIPPED by the report: backticks removed, trailing ':' gone)
    # -> regex matched against test full-names (Describe.Context.It). Add only entries you are
    # CONFIDENT about. Unmapped items fall through to "manual" — never assert a false [x].

    # §2 Insert/Run/target — covered by both the autofix path and the chat (proposed-command) path
    'Insert into pane works'            = 'Insert suggestion types the fix|inserted into the active shell pane'
    'Run in pane works'                 = 'Run suggestion executes the fix|runs in the active shell pane'
    'Command target is correct'         = 'Autofix target pane is correct'
    'Insert suggestion works'           = 'Insert suggestion types the fix|inserted into the active shell pane'
    'Run suggestion works'              = 'Run suggestion executes the fix|runs in the active shell pane'

    # §2 agent pane open/hide/focus + slash
    'Different positions work'          = 'at all four pane positions'
    'Focus hotkey works'                = 'Focus hotkey / focus works'
    '/model works'                      = '/model opens the model picker'
    '/agent picker works'               = '/agent appears in the slash menu and opens a keyboard-operable picker'
    'Invalid /agent selection is safe'  = '/agent rejects an unavailable id without changing the pane or global setting'
    'Esc/back navigation works'         = 'Esc/back navigation works|TRIGGERS the selected option'
    # §2/§5 WT accelerators + delegation palette (Feature.AgentHotkeys) — driven via window-level
    # OS keystrokes (Send-WtWindowKey), which reach WT's keybinding layer (the conpty path can't).
    'Hotkey opens pane'                 = 'Hotkey opens pane \(Ctrl'
    'Hotkey hides pane'                 = 'Hotkey hides pane \(Ctrl'
    'Alt+Shift+B launches background delegate' = 'Alt\+Shift\+B launches background delegate'
    'Alt+Shift+/ opens agent delegation palette' = 'Alt\+Shift\+/ opens agent delegation palette'
    'Command palette prompt launches delegate' = 'Command palette prompt launches delegate'
    'Command palette cancel is safe'    = 'Command palette cancel is safe'
    # §6 custom delegate via accelerators (Feature.CustomDelegate)
    'Alt+Shift+B uses custom delegate'  = 'Alt\+Shift\+B uses custom delegate'
    'Alt+Shift+/ uses custom delegate'  = 'Alt\+Shift\+/ uses custom delegate'
    # §6 custom delegate ENGINE (Feature.CustomDelegate, second Describe) — the custom-command
    # delegate path driven directly via `wta delegate --delegate-agent <custom cmdline>`.
    'Custom delegate cwd is correct'    = 'Custom delegate cwd is correct'
    'Custom delegate errors are clear'  = 'Custom delegate errors are clear'

    # §1 settings
    'Model control appears'             = 'Model control / model changes apply'
    'Model changes apply'               = 'Model control / model changes apply'

    # §0 FRE auto-error (on/off both covered by the single off/on test)
    'Automatic error detection on'      = 'Automatic error detection off/on'
    'Automatic error suggestion on'     = 'Automatic error suggestion off/on'
    'Session-management choice persists' = 'Session management choice persists'

    # §2 built-in chat: the non-Copilot agents are one consolidated matrix case now. Anchor on the
    # "non-Copilot agent" phrase so this does NOT also match the Copilot restart test name
    # ("/restart reconnects and answers"), which would otherwise credit this item incorrectly.
    'Non-Copilot agents chat works'     = 'non-Copilot agent.*connects and answers'

    # §3 autofix (copilot)
    'Autofix with Copilot works'        = 'Visible agent pane autofix works'
    'Visible agent pane autofix works'  = 'Visible agent pane autofix works'
    'Stashed agent pane autofix works'  = 'Stashed agent pane autofix works'

    # §3 shell integration
    'PowerShell shell integration installed' = 'PowerShell shell integration emits|PowerShell-level errors emit a non-zero command-finished mark on Windows PowerShell 5\.1'

    # §4 session view / focus
    # NOTE: 'Shift+Enter behavior works' is NOT E2E-mapped — its contract (Live row Shift+Enter ->
    # FocusPane) is deterministically covered by the Rust unit test
    # shift_enter_on_class_a_live_row_focuses; focus-pane semantics aren't stably observable in E2E.
    #
    # NOTE: 'Idle state is correct' is covered end-to-end by Feature.SessionState (the It name
    # contains the item title, so the report auto-credits it): it runs a real shell copilot
    # session to turn-completion and asserts the live row's Idle badge in the /sessions view. That
    # test opens the view via the `/sessions` SLASH command, NOT the bottom-bar SessionToggleButton
    # — in this build `winapp ui invoke SessionToggleButton` reliably fails to switch the pane to
    # the sessions view (it also breaks Feature.SessionList's toggle-based cases), whereas the slash
    # command typed into the agent pane by its session id is reliable.
    #
    # NOTE: 'Ended state is correct' is NOT E2E-mapped: Ended and Historical rows both render an
    # EMPTY badge (agents_view.rs:430), so they are indistinguishable in the picker's text. It is
    # covered deterministically by the Rust unit tests agent_sessions.rs ("Ended must stay Ended" +
    # PaneClosed tombstone) and status_badge_renders_expected_text_per_state (empty-for-Ended).
    #
    # 'Session view opens from chat' (C083): the checklist lists three triggers (`/sessions`, session
    # button, or Ctrl+Shift+/); the `/sessions` slash trigger is deterministically asserted by
    # Feature.SessionState's Idle test ("the /sessions slash command must open the session view",
    # a hard Should -BeTrue before any skip path). The Ctrl+Shift+/ WT-accelerator variant (C110) is
    # binding-covered by the Rust UT DefaultAgentKeybindings; its live open-behavior isn't stably
    # observable in E2E (harness pane-resolution vs. the per-tab pre-warm's extra stashed pane).
    'Session view opens from chat'      = 'Idle state is correct.*Idle badge'
    # C111 'Slash command works' (`/sessions` opens the session view) is the same hard assertion in
    # Feature.SessionState's Idle test as the C083 `/sessions` trigger above.
    'Slash command works'               = 'Idle state is correct.*Idle badge'

    # §0 FRE flow
    'FRE can be skipped or closed safely' = 'FRE can be closed safely'
    'FRE privacy / help links work'     = 'FRE privacy / help link'
    'FRE save progress works'           = 'FRE save progress'

    # §4 session view switching
    # 'Ended state is correct' (C121-adjacent) — see below.
    # C085 'View switch preserves input' — covered live by Feature.SessionList's 'View switch
    # preserves the draft input' case: it switches chat<->sessions via the bottom-bar buttons
    # (SessionToggleButton / AgentToggleButton, never the /sessions slash that would type into the
    # draft, never Esc which is overloaded), observing the view via the AgentLabelText UIA element
    # (winapp get-value on the focused window — no jsonl pane ambiguity). Also has a deterministic
    # Rust unit (app.rs::view_switch_preserves_chat_draft_input).
    'View switch preserves input'       = 'View switch preserves the draft input'

    # §8 per-CLI hook status (Feature.PerCliHooks) — wta hooks status enumerates each CLI's state/reason.
    'Per-CLI hook install works'        = 'Per-CLI hook install works'
    # §10 diagnostics — bug-report zip (Feature.BugReport). The "Report a bug (collect logs)"
    # command palette entry zips the logs dir to the Desktop; assert it contains agent logs.
    'Bug report zip includes agent logs' = 'Bug report zip includes agent logs'
    # §10 diagnostics — hook trace log (Feature.HookTrace). A hooked copilot prompt is traced.
    'Hook trace log is written'         = 'Hook trace log is written'
    # §2 agent pane paste (Feature.Paste) — clipboard text pasted with Ctrl+V lands in the input.
    'Paste works'                       = 'Paste works'
    # §7 multi-window (Feature.MultiWindow) — move an agent tab to a new window via the command
    # palette (moveTab window:new), assert chat preserved + closing the source window is safe.
    'Move tab to new window preserves chat' = 'Move tab to new window preserves chat'
    'Close source window is safe'          = 'Close source window is safe'

    # §11 accessibility — keyboard-only agent pane (Feature.KeyboardOnly).
    'Keyboard-only agent pane works'    = 'Keyboard-only agent pane works'
    # §11 accessibility — keyboard-only FRE + Settings (Feature.KeyboardFre / Feature.KeyboardSettings).
    'Keyboard-only FRE works'           = 'Keyboard-only FRE works'
    'Keyboard-only Settings works'      = 'Keyboard-only Settings works'

    # §9 packaging / §10 logging (titles differ from test names)
    'Packaged wta.exe is present'       = 'Packaged wta.exe is present'
    # The wta.exe case asserts the resolved binary is NOT the stale dev-build (tools\wta\target);
    # the wtcli case asserts wtcli co-location — together they cover "no unpackaged WTA is used".
    'Wrong unpackaged WTA is not used'  = 'Packaged wta\.exe is present|Packaged wtcli is co-located in the package'
    'wtcli send-keys/send input path works' = 'wtcli send-keys / send input path works'
    'wtcli listen works'                = 'wtcli listen streams events'
    'C++ agent pane log is written'     = 'C\+\+ agent pane log is written'
    'Early startup failures are logged' = 'Early startup failures would be logged'

    # §0/§1 agent Group Policy locks — the Feature.AgentPolicy suite drives the GPO registry
    # (AllowAutoFix / AllowedAgents / AllowCustomAgents / AllowAgentSessionHooks) and asserts both
    # the ENFORCEMENT (autofix suppressed; blocked built-in/custom agents can't open a pane) and
    # the UI policy MESSAGE (the FRE SessionHooksPolicyNotice "managed by your organization").
    # 'Group Policy locks' is in every Describe name, so the FRE-locks item is credited by the
    # whole suite; the Settings-style "policy message" item is credited by the FRE notice case
    # specifically (same managed-by-org control mechanism).
    'FRE respects policy locks'         = 'Group Policy locks'
    'Policy lock UI works'              = 'disables the session toggle and shows the policy notice'

    # §1 Settings session-hook install/remove (Feature.SettingsHooks). The Settings editor's
    # Install/Remove-hooks buttons run the `wta hooks install|uninstall|status` engine
    # (agent_hooks_installer); we drive that engine directly and assert the on-disk plugin state,
    # exactly as the rest of §1 tests the settings model rather than the (non-openable) editor UI.
    'Session hooks install works'       = 'Session hooks install works'
    'Session hooks remove works'        = 'Session hooks remove works'
    # C170/C172 checklist titles differ from the test names — map them to the same passing
    # Feature.SettingsHooks cases (the Settings Install/Remove-hooks buttons run the same engine).
    'Install hooks from Settings works' = 'Session hooks install works'
    'Hook remove works'                 = 'Session hooks remove works'

    # §0 FRE agent-setup overlay controls (Feature.FreAgentSetup) — the FRE-specific UI half.
    'Copilot preinstalled'              = 'FRE agent dropdown shows Copilot as installed'
    'Non-Copilot agents appear when installed' = 'Non-Copilot agents appear as installed in the FRE'
    'Session hook hints'                = 'Session hook hints appear only when'
    'Detection/suggestion dependency'   = 'the suggestion toggle disables when detection is off'
    # §0 FRE session-management hook install (Feature.FreHooks) — observed on disk via config.json.
    'Session management on'              = 'Session management on installs agent hooks'
    'Session management off'             = 'Session management off does not install hooks'
    'Install hooks from FRE works'       = 'Session management on installs agent hooks'
    'Hook logs are available'            = 'Session management on installs agent hooks'
    # §4 focus/restore — the Enter-on-the-live-row case selects the active session and focuses/resumes it.
    'Focus active session'               = 'Enter behavior works'

    # §7 multi-pane / multi-tab (Feature.MultiPane) — single-window protocol-driven cases.
    'Split pane does not break chat'    = 'Split pane does not break chat'
    'Multiple tabs work'                = 'Multiple tabs each get an independent agent session'
    'Multiple agent panes work'         = 'Multiple tabs each get an independent agent session'
    'Per-tab agent switching is isolated' = 'Switching tab B rebuilds only tab B and leaves tab A conversation alive|Both agent tabs remain independently usable after the switch'
    'Global agent defaults respect per-tab overrides' = 'A newly opened tab still follows the global default agent|Changing the global default rebuilds follower tabs but preserves overridden tab B'
    'Close target tab cleans up'        = 'Close target tab cleans up'
    # Agent insert/run/autofix targeting the intended split pane is proven by the autofix-in-a-split
    # case plus the autofix-target-pane assertion.
    'Split pane target selection is correct' = 'Split pane autofix works|Autofix target pane is correct'

    # §6 custom agents (Feature.CustomAgent)
    'Custom agent is Settings-only'     = 'Custom agent is Settings-only'
    'Custom agent runs the standard agent-pane behaviours' = 'A configured custom ACP agent connects and chats'
    'Custom failure is safe'            = 'Custom failure is safe'
    # "Add" a custom agent = configure custom:<id> + command and have it launch/connect — exactly
    # what the connects-and-chats case proves (same basis as the already-ticked "Save custom ACP
    # agent"). Edit/Delete are exercised behaviorally (Copilot as the arbitrary ACP agent): edit
    # rewrites acpCustomCommand and the new command is what launches; delete switches back to a
    # valid built-in. "Model selection visible" stays manual (Settings-editor UI, not harness-openable).
    'Add custom ACP agent'              = 'A configured custom ACP agent connects and chats'
    'Edit custom ACP agent'             = 'Edit custom ACP agent updates the command used by new agent panes'
    'Delete custom ACP agent'           = 'Delete custom ACP agent returns to a valid built-in selection'
    # §6 "Model selection visible" — the Settings editor IS drivable: Open-WtSettings opens it via
    # the Ctrl+, accelerator (OS-level key to the WT window), then winapp navigates to the AI Agents
    # page and asserts the Model control renders for a custom agent (Feature.SettingsUi).
    'Model selection visible'           = 'Model selection visible: the model control shows'

    # §5 delegate agent (Feature.Delegate) — the delegate ENGINE driven directly via `wta delegate`
    # (the exact command Alt+Shift+B / the Alt+Shift+/ palette build), with Copilot as the delegate.
    # NOT mapped (stay manual): 'Alt+Shift+B launches background delegate' + 'Alt+Shift+/ opens
    # agent delegation palette' are WT accelerators (send-keys reaches the conpty, not WT's keybinding
    # handler); 'Command palette prompt launches delegate' + 'Command palette cancel is safe' are the
    # WT XAML command-palette UI (not harness-drivable) — though the engine the prompt path drives is
    # covered by "Delegate with Copilot works". 'Delegate model is correct' is not observable in WT
    # state or wta-delegate.log (the model is passed into the launched CLI) — UT-locked (command
    # construction / delegateModel roundtrip). 'Delegate with non-Copilot agents' is env-gated.
    'Delegate cwd is correct'           = 'Delegate cwd is correct'
    'Delegate provider is correct'      = 'Delegate provider is correct'
    'Delegate with Copilot works'       = 'Delegate with Copilot works'
    'Delegate errors are actionable'    = 'Delegate errors are actionable'
    # §5 non-Copilot delegate (Feature.DelegateNonCopilot) — claude/gemini delegate tab launches.
    'Delegate with non-Copilot agents works' = 'Delegate with non-Copilot agents works'
}
