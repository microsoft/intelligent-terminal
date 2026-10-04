function Invoke-SidebarSessionCleanup {
    [CmdletBinding()]
    param($App, $Target, [bool]$OwnsConfigBackup, [string]$SettingsHash,
        [AllowNull()][string]$StateHash, [string]$Evidence)

    $ErrorActionPreference = 'Stop'
    $failures = [System.Collections.Generic.List[string]]::new()
    if ($App) {
        try {
            Save-UiScreenshot -App $App -Path (Join-Path $Evidence 'final.png') | Out-Null
        }
        catch { $failures.Add("Final screenshot failed: $($_.Exception.Message)") }
        try {
            if (-not $App.Launched -or -not $App.Pid) {
                throw 'Terminal shutdown requires an owned launch context.'
            }
            Stop-Terminal -App $App -RestoreSettings $false
        }
        catch { $failures.Add("Owned terminal shutdown failed: $($_.Exception.Message)") }
    }
    if ($OwnsConfigBackup) {
        $inactive = $false
        try {
            $inactive = Test-Until -TimeoutSec 10 -IntervalSec 0.2 -Condition {
                -not @(Get-WtProcessesForApp -App $Target -IncludePackageExecutables).Count
            }
            if (-not $inactive) { throw 'Selected package is still active; all recovery backups are retained.' }
        }
        catch { $failures.Add("Package inactivity check failed: $($_.Exception.Message)") }
        if ($inactive) {
            $settingsPreserved = $false
            try {
                $currentHash = (Get-FileHash -LiteralPath $Target.SettingsPath -ErrorAction Stop).Hash
                if ($currentHash -ne $SettingsHash) {
                    throw 'Settings changed; preserving the current file and its recovery backup.'
                }
                $settingsPreserved = $true
            }
            catch { $failures.Add("Settings preservation check failed: $($_.Exception.Message)") }

            # State belongs to the test window; settings checks must not block its recovery.
            $statePreserved = $false
            try {
                $path = $Target.StatePath
                $backup = "$path.e2ebak"
                $missing = "$backup.missing"
                if (Test-Path -LiteralPath $backup) {
                    if ((Get-FileHash -LiteralPath $backup -ErrorAction Stop).Hash -ne $StateHash) {
                        throw 'Original state backup does not match; current state and backup retained.'
                    }
                    Copy-Item -LiteralPath $backup -Destination $path -Force
                    if ((Get-FileHash -LiteralPath $path -ErrorAction Stop).Hash -ne $StateHash) {
                        throw 'Recovered state does not match the original; backup retained.'
                    }
                    Remove-Item -LiteralPath $backup -Force
                }
                elseif (Test-Path -LiteralPath $missing) {
                    if ($StateHash) { throw 'Unexpected missing-state marker; backup retained.' }
                    if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Force }
                    Remove-Item -LiteralPath $missing -Force
                }
                else { throw 'Original application state recovery backup is missing.' }
                $statePreserved = $true
            }
            catch { $failures.Add("Application state recovery failed: $($_.Exception.Message)") }

            if ($settingsPreserved) {
                try {
                    # Settings were never test-owned: do not write them even on the normal path.
                    $backup = "$($Target.SettingsPath).e2ebak"
                    if ((Get-FileHash -LiteralPath $backup -ErrorAction Stop).Hash -ne $SettingsHash) {
                        throw 'Original settings backup does not match; backup retained.'
                    }
                    Remove-Item -LiteralPath $backup -Force
                }
                catch { $failures.Add("Settings backup cleanup failed: $($_.Exception.Message)") }
            }
            try {
                @{ settings_preserved = $settingsPreserved; state_preserved = $statePreserved
                    failures = @($failures.ToArray()) } | ConvertTo-Json -Depth 4 |
                    Set-Content -LiteralPath (Join-Path $Evidence 'cleanup.json')
            }
            catch { $failures.Add("Cleanup evidence failed: $($_.Exception.Message)") }
        }
    }
    if ($failures.Count) { throw ($failures -join "`n") }
}
