function Stop-TestTerminal {
    [CmdletBinding()]
    param(
        $App,
        $Target,
        [Nullable[DateTime]]$LaunchStarted
    )
    if ($App) {
        if ($App.PSObject.Properties.Name -notcontains 'Launched' -or -not $App.Launched -or -not $App.Pid) {
            throw 'Terminal cleanup requires a returned owned launch context.'
        }
        Stop-Terminal -App $App -RestoreSettings $false
        if (-not (Test-Until -TimeoutSec 5 -IntervalSec 0.2 -Condition ({
            -not @(Get-WtProcessesForApp -App $App -IncludePackageExecutables).Count
        }.GetNewClosure()))) {
            throw 'The selected package is still active; configuration backup is retained.'
        }
        Restore-WtConfig -App $App
        return
    }
    if (-not $LaunchStarted) { return }
    if (-not $Target -or -not $Target.WindowsTerminal) {
        throw 'Failed-startup cleanup has no verified target executable.'
    }

    if (@(Get-WtProcessesForApp -App $Target -IncludePackageExecutables).Count) {
        throw 'There is no returned launch context; process ownership cannot be inferred and configuration backup is retained.'
    }
    Restore-WtConfig -App $Target
}
