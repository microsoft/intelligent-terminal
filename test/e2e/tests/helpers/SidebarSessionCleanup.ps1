function Assert-SidebarWindowOwnership {
    param($App)
    if (-not $App.Hwnd -or -not $App.Pid) { throw 'Missing owned HWND/PID.' }
    $windows = @(Get-WtWindowHwnds -App $App | Where-Object { $_.hwnd -eq $App.Hwnd })
    if ($windows.Count -ne 1 -or $windows[0].pid -ne $App.Pid) {
        throw 'Owned window handle/PID no longer match; refusing teardown.'
    }
}

function Get-SidebarWindowPanes {
    param($App)
    if (-not $App.WindowId) { throw 'Missing verified logical window identity.' }
    foreach ($tab in @(Get-WtTabs -App $App -WindowId $App.WindowId)) {
        if ($tab.window_id -ne $App.WindowId) { throw 'Tab discovery returned another window.' }
        Get-WtPanes -App $App -TabId $tab.tab_id -WindowId $App.WindowId |
            Where-Object { $_.window_id -eq $App.WindowId }
    }
}

function Invoke-SidebarSessionCleanup {
    [CmdletBinding()]
    param($App, $Target, [bool]$OwnsConfigBackup, [string]$SettingsHash,
        [AllowNull()][string]$StateHash, [string]$Evidence)

    $ErrorActionPreference = 'Stop'
    $failures = [System.Collections.Generic.List[string]]::new()
    $ownershipConfirmed = $false
    if ($App) {
        try {
            Save-UiScreenshot -App $App -Path (Join-Path $Evidence 'final.png') | Out-Null
        }
        catch { $failures.Add("Final screenshot failed: $($_.Exception.Message)") }
        try {
            if (-not $App.OwnedPaneIds.Count) {
                throw 'Window teardown requires recorded fixture pane identities.'
            }
            foreach ($paneId in @($App.OwnedPaneIds)) {
                $ownershipConfirmed = $false
                $panes = @(Get-SidebarWindowPanes -App $App | Where-Object session_id -EQ $paneId)
                if ($panes.Count -ne 1) { throw "Owned pane $paneId is not in the verified window; refusing teardown." }
                Assert-SidebarWindowOwnership -App $App
                $ownershipConfirmed = $true
                # Do not use Close-WtPane: its NoThrow path hides close failures.
                Invoke-WtCli -App $App -Arguments @('kill-pane', '-t', $paneId) | Out-Null
            }
            if (-not (Test-Until -TimeoutSec 10 -IntervalSec 0.2 -Condition {
                -not @(Get-WtWindowHwnds -App $App | Where-Object { $_.hwnd -eq $App.Hwnd }).Count
            })) {
                throw 'Owned window did not close; no process-wide fallback is permitted.'
            }
        }
        catch { $failures.Add("Owned window teardown failed: $($_.Exception.Message)") }
    }
    elseif ($OwnsConfigBackup) { $failures.Add('No verified fixture window was returned; refusing teardown.') }
    if ($OwnsConfigBackup) {
        $inactive = $false
        try {
            $inactive = Test-Until -TimeoutSec 10 -IntervalSec 0.2 -Condition {
                -not @(Get-WtProcessesForApp -App $Target -IncludePackageExecutables).Count
            }
            if (-not $inactive) { throw 'Selected package is still active; all recovery backups are retained.' }
        }
        catch { $failures.Add("Package inactivity check failed: $($_.Exception.Message)") }
        if ($inactive -and $ownershipConfirmed) {
            $settingsPreserved = $false
            try {
                $currentHash = (Get-FileHash -LiteralPath $Target.SettingsPath -ErrorAction Stop).Hash
                if ($currentHash -ne $SettingsHash) {
                    throw 'Settings changed; preserving the current file and its recovery backup.'
                }
                $settingsPreserved = $true
            }
            catch { $failures.Add("Settings preservation check failed: $($_.Exception.Message)") }

            # Recover package state only after inactivity; settings checks must not block it.
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
