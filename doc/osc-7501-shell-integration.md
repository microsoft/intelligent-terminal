# Report command status automatically from PowerShell

A PowerShell integration can report basic [OSC 7501 status](https://www.superlogical.com/rex/docs/build/program-status) for ordinary commands. Users type commands normally. The two snippets below extend Intelligent Terminal's [existing shell integration](configuring-shell-integration-autofix.md); they are insertion points in the integration script, not commands to paste at the prompt.

See the [complete PowerShell sample](samples/osc-7501-shell-integration.ps1) for both hooks in context. This is a reference copy, not the script installed by default.

**When a command is submitted:** initialize `$Global:__ShellInteg_StatusPending = $false` when installing the hooks. In the existing `PSConsoleHostReadLine` hook, just before `return $line`, add:

```powershell
if (-not [string]::IsNullOrWhiteSpace($line)) {
    $Global:__ShellInteg_StatusPending = $true
    [Console]::Write("$($Global:__ShellInteg_ESC)]7501;state=working$($Global:__ShellInteg_BEL)")
}
```

**When the prompt returns:** just before the existing OSC 133 prompt-start mark, append the result using the exit code (`$gle`) already calculated by the integration. Keep the existing OSC 133 marks.

```powershell
if ($Global:__ShellInteg_StatusPending) {
    $state = if ($gle -eq 0) { 'done' } else { 'error' }
    $prefix += "${E}]7501;state=${state}${B}"
    $Global:__ShellInteg_StatusPending = $false
}
```

Open a new PowerShell pane and run `Start-Sleep -Seconds 3`: the sidebar should show **Active**, then **Completed**. On Windows, `cmd /c exit 7` should show **Error**. These reports describe the foreground command line, not background jobs or an application's internal progress. Programs can send richer status while they run. An integration update may replace local edits to the installed script.
