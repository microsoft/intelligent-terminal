#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §9 Packaging/protocol + §10 Diagnostics/logging.
#   Invoke-Pester test/e2e/tests -Tag Feature
# Maps to checklist items (auto): packaged wta present, packaged identity, WT_COM_CLSID,
# wtcli list-panes/capture-pane/send-keys/listen, master+helper start, logs+version dir.

BeforeDiscovery {
    $script:Ready = [bool](Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' })
    # §10 opens the agent pane (UI automation) so it additionally needs winapp; §9 is
    # pure protocol/packaging and runs without it.
    $script:UiReady = $script:Ready -and [bool](Get-Command winapp -ErrorAction SilentlyContinue)
}

Describe 'Feature §9 Packaging + protocol' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Packaged wta.exe is present (co-located in the install dir)' {
        # Co-located = resolved from the package InstallLocation (WindowsApps for an installed
        # Store package, or the run-from-layout AppX dir for a Dev/F5 build) — NOT the stray
        # unpackaged tools\wta\target fallback, which would lack package identity for COM.
        $script:app.WtaPath | Should -BeLike "$($script:app.InstallLocation)*"
        $script:app.WtaPath | Should -Not -Match 'tools[\\/]wta[\\/]target'
        Test-Path $script:app.WtaPath | Should -BeTrue
    }
    It 'Packaged identity works (wtcli reaches COM via brand CLSID)' {
        $script:app.ComClsid | Should -Match '^\{[0-9A-Fa-f-]+\}$'
        @(Get-WtWindows -App $script:app).Count | Should -BeGreaterThan 0
    }
    It 'Packaged wtcli is co-located in the package (no unpackaged fallback)' {
        # Co-location is the guarantee: wtcli must resolve from the package InstallLocation
        # (WindowsApps for Store, or the Dev layout AppX dir), NOT its non-packaged fallbacks —
        # for wtcli that's a PATH-resolved standalone (Get-Command wtcli) or
        # bin/x64/{Debug,Release}/wtcli/wtcli.exe (cli_channel.rs), none of which live under the
        # InstallLocation. So -BeLike InstallLocation* alone fully covers it (no tools\wta\target
        # check here — that's wta.exe's fallback, asserted in the wta.exe case above).
        $script:app.WtcliPath | Should -BeLike "$($script:app.InstallLocation)*"
    }
    It 'WT_COM_CLSID resolves to a live server' {
        # Probing the brand CLSID succeeded during Start-Terminal; re-confirm a call works.
        { Invoke-WtCli -App $script:app -Arguments @('active-pane') } | Should -Not -Throw
    }
    It 'WT_COM_CLSID is injected into pane shells' {
        # WT injects WT_COM_CLSID into every pane's shell environment so a co-located
        # wtcli/agent can discover the per-brand COM server. Read it back from the active
        # PowerShell pane and assert it is a braced CLSID (not empty / unset).
        $sid = (Get-ActivePane -App $script:app).session_id
        $out = Invoke-RunCommand -App $script:app -SessionId $sid `
            -Command 'Write-Output "CLSIDENV=$env:WT_COM_CLSID"' -SettleSec 8
        $out | Should -Match 'CLSIDENV=\{[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}\}'
    }
    It 'wtcli list-panes works' {
        @(Get-WtPanes -App $script:app).Count | Should -BeGreaterThan 0
    }
    It 'wtcli capture-pane works' {
        $sid = (Get-ActivePane -App $script:app).session_id
        (Get-WtCapture -App $script:app -SessionId $sid -MaxLines 10) | Should -Not -BeNullOrEmpty
    }
    It 'wtcli send-keys / send input path works' {
        $sid = (Get-ActivePane -App $script:app).session_id
        $tag = "PKG_$(Get-Random)"
        Invoke-RunCommand -App $script:app -SessionId $sid -Command "echo $tag" -SettleSec 8 | Out-Null
        Assert-Pane -App $script:app -SessionId $sid -Match $tag -TimeoutSec 10
    }
    It 'wtcli listen streams events' {
        $listener = Start-WtEventListener -App $script:app
        try {
            $sid = (Get-ActivePane -App $script:app).session_id
            Start-Sleep -Milliseconds 500
            Invoke-RunCommand -App $script:app -SessionId $sid -Command 'echo listen-probe' -SettleSec 6 | Out-Null
            # vt_sequence events should flow for shell-integrated output.
            { Wait-WtEvent -Listener $listener -TimeoutSec 15 -Predicate { $_.method -eq 'vt_sequence' } } | Should -Not -Throw
        }
        finally { Stop-WtEventListener -Listener $listener }
    }
    It 'WTA master starts' {
        $cl = Get-CimInstance Win32_Process -Filter "Name='wta.exe'" | ForEach-Object { $_.CommandLine }
        ($cl -join "`n") | Should -Match '--master'
    }
    It 'WTA helper starts per tab/pane (pre-warm)' {
        $cl = Get-CimInstance Win32_Process -Filter "Name='wta.exe'" | ForEach-Object { $_.CommandLine }
        ($cl -join "`n") | Should -Match '--connect-master|--owner-tab-id'
    }
    It 'Master/helper crash recovery is acceptable (WT survives master kill)' {
        $masterPid = (Get-CimInstance Win32_Process -Filter "Name='wta.exe'" | Where-Object { $_.CommandLine -match '--master' } | Select-Object -First 1).ProcessId
        if (-not $masterPid) { Set-ItResult -Skipped -Because 'no master found'; return }
        Stop-Process -Id $masterPid -Force
        Start-Sleep -Seconds 3
        # Acceptable = the terminal itself does NOT crash when the master dies (recovery of
        # the agent stack is lazy/manual via /restart; WT staying alive is the guarantee).
        (Get-Process -Id $script:app.Pid -ErrorAction SilentlyContinue) | Should -Not -BeNullOrEmpty
        # And the protocol surface is still responsive (COM server is in WT, not the master).
        { Invoke-WtCli -App $script:app -Arguments @('list-panes') } | Should -Not -Throw
    }
}

Describe 'Feature §10 Diagnostics + logging' -Tag 'Feature' -Skip:(-not $script:UiReady) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Out-Null
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'WTA logs are written (master + helper)' {
        $dir = Get-ItLogDir -App $script:app
        $dir | Should -Not -BeNullOrEmpty
        @(Get-ChildItem $dir -Filter 'wta-*.log').Count | Should -BeGreaterThan 0
    }
    It 'C++ agent pane log is written' {
        $dir = Get-ItLogDir -App $script:app
        Test-Path (Join-Path $dir 'terminal-agent-pane.log') | Should -BeTrue
    }
    It 'Log version directory is correct (matches package version)' {
        $dir = Get-ItLogDir -App $script:app
        (Split-Path $dir -Leaf) | Should -Be $script:app.Version
    }
    It 'Release log level is reasonable (no crash, level present)' {
        (Get-ItLogText -App $script:app -Name 'wta-main_master.log') | Should -Match 'INFO|WARN|DEBUG'
    }
    It 'Early startup failures would be logged (master log has startup entries)' {
        (Get-ItLogText -App $script:app -Name 'wta-main_master.log') | Should -Not -BeNullOrEmpty
    }
}

Describe 'Feature §10 log retention' -Tag 'Feature' -Skip:(-not $script:Ready) {
    # Isolated: this case restarts the build, so it owns its own app lifecycle.
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true
        $script:staleDir = $null
    }
    AfterAll {
        if ($script:staleDir -and (Test-Path $script:staleDir)) {
            Remove-Item $script:staleDir -Recurse -Force -ErrorAction SilentlyContinue
        }
        if ($script:app) { Stop-Terminal -App $script:app }
    }

    It 'Old log cleanup is safe (running version survives; stale version dirs are pruned)' {
        $curDir = Get-ItLogDir -App $script:app
        $curDir | Should -Not -BeNullOrEmpty
        # Sentinel inside the CURRENT (running) version dir — must survive a restart.
        $sentinel = Join-Path $curDir 'retention-sentinel.log'
        Set-Content -LiteralPath $sentinel -Value 'keep-me' -Encoding utf8
        # A stale OTHER-version dir — startup housekeeping should prune it wholesale.
        $script:staleDir = Join-Path $script:app.LogRootDir '0.0.0.1'
        New-Item -ItemType Directory -Path $script:staleDir -Force | Out-Null
        Set-Content -LiteralPath (Join-Path $script:staleDir 'old.log') -Value 'prune-me' -Encoding utf8

        # Restart the build: the new wta-master startup runs prune_old_version_dirs, which keeps
        # only the current version's dir and deletes every other version dir wholesale.
        Stop-Terminal -App $script:app
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true
        Test-Until -TimeoutSec 25 -IntervalSec 1 -Condition { -not (Test-Path $script:staleDir) } | Out-Null

        Test-Path $sentinel        | Should -BeTrue   # running version's logs were NOT deleted
        Test-Path $script:staleDir | Should -BeFalse  # stale version dir was pruned
    }
}
