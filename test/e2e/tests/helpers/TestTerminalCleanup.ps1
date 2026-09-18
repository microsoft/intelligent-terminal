function Stop-TestTerminal {
    [CmdletBinding()]
    param(
        $App,
        $Target,
        [Nullable[DateTime]]$LaunchStarted
    )
    if ($App) {
        Stop-Terminal -App $App
        return
    }
    if (-not $LaunchStarted) { return }
    if (-not $Target -or -not $Target.WindowsTerminal) {
        throw 'Failed-startup cleanup has no verified target executable.'
    }

    $protected = [Collections.Generic.HashSet[int]]::new()
    $ancestor = $PID
    while ($ancestor -gt 0 -and $protected.Add($ancestor)) {
        $process = Get-CimInstance Win32_Process -Filter "ProcessId=$ancestor"
        if (-not $process) { throw 'Failed-startup cleanup cannot verify caller ancestry.' }
        $ancestor = [int]$process.ParentProcessId
    }
    $candidates = @(Get-WtProcessesForApp -App $Target)
    foreach ($process in $candidates) {
        if ($process.Path -ine $Target.WindowsTerminal -or $process.StartTime -lt $LaunchStarted -or
            $protected.Contains([int]$process.Id)) {
            throw 'Failed-startup cleanup cannot establish test ownership of a candidate process.'
        }
    }
    foreach ($process in $candidates) {
        $current = Get-Process -Id $process.Id -ErrorAction SilentlyContinue
        if (-not $current) { continue }
        if ($current.Path -ine $process.Path -or $current.StartTime -ne $process.StartTime) {
            throw 'Failed-startup process identity changed before cleanup.'
        }
        $recovery = $Target.PSObject.Copy()
        $recovery.Pid = $process.Id
        $recovery | Add-Member -NotePropertyName Launched -NotePropertyValue $true -Force
        Stop-Terminal -App $recovery -RestoreSettings $false
    }
    Restore-WtConfig -App $Target
}
