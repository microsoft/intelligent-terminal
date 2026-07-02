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
    'Esc/back navigation works'         = 'Esc/back navigation works|TRIGGERS the selected option'

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
    'PowerShell shell integration installed' = 'PowerShell shell integration emits'

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

    # §0 FRE flow
    'FRE can be skipped or closed safely' = 'FRE can be closed safely'
    'FRE privacy / help links work'     = 'FRE privacy / help link'
    'FRE save progress works'           = 'FRE save progress'

    # §4 session view switching
    'View switch preserves input'       = 'View switch preserves the draft input'

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
}
