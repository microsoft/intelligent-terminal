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
}
