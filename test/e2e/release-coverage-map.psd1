@{
    # Checklist item title  ->  regex matched against test full-names (Describe.Context.It).
    # Only add entries you are CONFIDENT about. Unmapped items fall through to "manual" (a
    # human verifies), which is the safe default — never assert a false [x].

    # §2 Insert/Run/target — covered by both the autofix path and the chat (proposed-command) path
    'Insert into pane works'            = 'Insert suggestion types the fix|inserted into the active shell pane'
    'Run in pane works'                 = 'Run suggestion executes the fix|runs in the active shell pane'
    'Command target is correct'         = 'Autofix target pane is correct'
    'Insert suggestion works'           = 'Insert suggestion types the fix|inserted into the active shell pane'
    'Run suggestion works'              = 'Run suggestion executes the fix|runs in the active shell pane'

    # §2 agent pane open/hide/focus + slash
    'Different positions work'          = 'at all four pane positions'
    '`/model` works'                    = '/model opens the model picker'
    'Esc/back navigation works'         = 'Esc/back navigation works|TRIGGERS the selected option'

    # §2 built-in chat matrix
    'Claude chat works'                 = 'Claude chat works'
    'Codex chat works'                  = 'Codex chat works'
    'Gemini chat works'                 = 'Gemini\b.*chat works'

    # §3 autofix matrix
    'Autofix with Claude works'         = 'Claude autofix works'
    'Autofix with Codex works'          = 'Codex autofix works'
    'Autofix with Gemini works'         = 'Gemini\b.*autofix works'

    # §3 shell integration
    'PowerShell shell integration installed' = 'PowerShell shell integration emits'
    'Visible agent pane autofix works'  = 'Visible agent pane autofix works'
    'Stashed agent pane autofix works'  = 'Stashed agent pane autofix works'

    # §0 FRE
    'FRE can be skipped or closed safely' = 'FRE can be closed safely'
    'FRE privacy / help links work'     = 'FRE privacy / help link'
    'FRE save progress works'           = 'FRE save progress'

    # §4 session view
    'View switch preserves input'       = 'View switch preserves the draft input'
    'Hotkey works'                      = 'Session button works'   # session view open is covered; the hotkey itself isn't injectable

    # §9 packaging / §10 logging (titles vs test names)
    'Packaged `wta.exe` is present'     = 'Packaged wta.exe is present'
    'Packaged identity works'           = 'Packaged identity works'
    '`wtcli list-panes` works'          = 'wtcli list-panes works'
    '`wtcli capture-pane` works'        = 'wtcli capture-pane works'
    '`wtcli send-keys`/send input path works' = 'wtcli send-keys / send input path works'
    '`wtcli listen` works'              = 'wtcli listen streams events'
    'WTA master starts'                 = 'WTA master starts'
    'WTA helper starts per tab/pane'    = 'WTA helper starts per tab/pane'
    'WTA logs are written'              = 'WTA logs are written'
    'C++ agent pane log is written'     = 'C\+\+ agent pane log is written'
}
