#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeAll {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    . (Join-Path $PSScriptRoot '..\tests\helpers\SidebarSessionCleanup.ps1')
}

Describe 'Sidebar scroll safe cleanup' -Tag Unit {
    BeforeEach {
        $script:target = [pscustomobject]@{
            SettingsPath = Join-Path $TestDrive 'settings.json'
            StatePath = Join-Path $TestDrive 'state.json'
        }
        'original settings' | Set-Content -LiteralPath $script:target.SettingsPath
        'original state' | Set-Content -LiteralPath $script:target.StatePath
        $script:settingsHash = (Get-FileHash $script:target.SettingsPath).Hash
        $script:stateHash = (Get-FileHash $script:target.StatePath).Hash
        Copy-Item $script:target.SettingsPath "$($script:target.SettingsPath).e2ebak" -Force
        Copy-Item $script:target.StatePath "$($script:target.StatePath).e2ebak" -Force
        'test state' | Set-Content -LiteralPath $script:target.StatePath
        $script:owned = [pscustomobject]@{ Launched = $false; Pid = 123; Hwnd = 456; WindowId = 7; OwnedPaneIds = @('pane-a', 'pane-b') }
        $script:remaining = @('pane-a', 'pane-b')
        Mock Save-UiScreenshot {}
        Mock Stop-Terminal { throw 'Process-wide shutdown is forbidden.' }
        Mock Get-WtWindowHwnds {
            if ($script:remaining.Count) { [pscustomobject]@{ hwnd = 456; pid = 123 } }
            [pscustomobject]@{ hwnd = 999; pid = 123 }
        }
        Mock Get-WtTabs { [pscustomobject]@{ tab_id = 0; window_id = 7 } }
        Mock Get-WtPanes {
            foreach ($id in $script:remaining) { [pscustomobject]@{ session_id = $id; window_id = 7 } }
            [pscustomobject]@{ session_id = 'unrelated'; window_id = 8 }
        }
        Mock Invoke-WtCli {
            param($Arguments)
            $Arguments[0] | Should -Be 'kill-pane'
            $Arguments[2] | Should -BeIn @('pane-a', 'pane-b')
            $script:remaining = @($script:remaining | Where-Object { $_ -ne $Arguments[2] })
        }
        Mock Get-WtProcessesForApp { @() }
        Mock Test-Until { param($Condition) & $Condition }
        function Invoke-Cleanup {
            Invoke-SidebarSessionCleanup -App $script:owned -Target $script:target -OwnsConfigBackup $true `
                -SettingsHash $script:settingsHash -StateHash $script:stateHash -Evidence $TestDrive
        }
    }

    AfterEach {
        Should -Invoke Stop-Terminal -Exactly -Times 0
        Should -Invoke Invoke-WtCli -Exactly -Times 0 -ParameterFilter {
            $Arguments[2] -eq 'unrelated'
        }
        Should -Invoke Get-WtPanes -Exactly -Times 0 -ParameterFilter { $WindowId -ne 7 }
    }

    It 'recovers normal state without writing unchanged settings' {
        Mock Copy-Item { throw 'Settings must not be written.' } -ParameterFilter {
            $Destination -eq $script:target.SettingsPath
        }
        Invoke-Cleanup
        (Get-FileHash $script:target.SettingsPath).Hash | Should -Be $script:settingsHash
        (Get-FileHash $script:target.StatePath).Hash | Should -Be $script:stateHash
        Test-Path "$($script:target.SettingsPath).e2ebak" | Should -BeFalse
        Test-Path "$($script:target.StatePath).e2ebak" | Should -BeFalse
        Should -Invoke Invoke-WtCli -Exactly -Times 2
    }

    It 'reports screenshot failure after shutdown and state recovery' {
        Mock Save-UiScreenshot { throw 'screenshot unavailable' }
        { Invoke-Cleanup } | Should -Throw '*screenshot unavailable*'
        Should -Invoke Invoke-WtCli -Exactly -Times 2
        (Get-FileHash $script:target.StatePath).Hash | Should -Be $script:stateHash
    }

    It 'reports both diagnostic and settings failures after mandatory state recovery' {
        Mock Save-UiScreenshot { throw 'screenshot unavailable' }
        'external edit' | Set-Content -LiteralPath $script:target.SettingsPath
        $failure = try { Invoke-Cleanup; $null } catch { $_ }
        $failure.Exception.Message | Should -Match 'screenshot unavailable'
        $failure.Exception.Message | Should -Match 'Settings preservation check failed'
        Should -Invoke Invoke-WtCli -Exactly -Times 2
        (Get-FileHash $script:target.StatePath).Hash | Should -Be $script:stateHash
        (Get-Content $script:target.SettingsPath -Raw).Trim() | Should -Be 'external edit'
        (Get-FileHash "$($script:target.SettingsPath).e2ebak").Hash | Should -Be $script:settingsHash
    }

    It 'preserves <Fault> settings and backup while recovering original state' -ForEach @(
        @{ Fault = 'changed' }; @{ Fault = 'missing' }; @{ Fault = 'unreadable' }
    ) {
        switch ($Fault) {
            changed { 'external edit' | Set-Content -LiteralPath $script:target.SettingsPath }
            missing { Remove-Item -LiteralPath $script:target.SettingsPath }
            unreadable {
                Mock Get-FileHash { throw 'settings unreadable' } -ParameterFilter {
                    $LiteralPath -eq $script:target.SettingsPath
                }
            }
        }
        { Invoke-Cleanup } | Should -Throw '*Settings preservation check failed*'
        (Get-FileHash -LiteralPath $script:target.StatePath).Hash | Should -Be $script:stateHash
        Test-Path "$($script:target.SettingsPath).e2ebak" | Should -BeTrue
        (Get-FileHash -LiteralPath "$($script:target.SettingsPath).e2ebak").Hash | Should -Be $script:settingsHash
        Test-Path "$($script:target.StatePath).e2ebak" | Should -BeFalse
        if ($Fault -eq 'changed') { (Get-Content $script:target.SettingsPath -Raw).Trim() | Should -Be 'external edit' }
        if ($Fault -eq 'missing') { Test-Path $script:target.SettingsPath | Should -BeFalse }
    }

    It 'refuses all file recovery while the selected package remains active' {
        Mock Get-WtProcessesForApp { [pscustomobject]@{ Id = 123 } }
        { Invoke-Cleanup } | Should -Throw '*still active*'
        Should -Invoke Invoke-WtCli -Exactly -Times 2
        (Get-Content $script:target.StatePath -Raw).Trim() | Should -Be 'test state'
        Test-Path "$($script:target.SettingsPath).e2ebak" | Should -BeTrue
        Test-Path "$($script:target.StatePath).e2ebak" | Should -BeTrue
    }

    It 'refuses teardown when the owned HWND has another PID' {
        Mock Get-WtWindowHwnds { [pscustomobject]@{ hwnd = 456; pid = 999 } }
        { Invoke-Cleanup } | Should -Throw '*handle/PID no longer match*'
        Should -Invoke Invoke-WtCli -Exactly -Times 0
        Test-Path "$($script:target.SettingsPath).e2ebak" | Should -BeTrue
        Test-Path "$($script:target.StatePath).e2ebak" | Should -BeTrue
    }

    It 'retains backups when fixture pane ownership was never established' {
        $script:owned.OwnedPaneIds = @()
        { Invoke-Cleanup } | Should -Throw '*recorded fixture pane identities*'
        Should -Invoke Invoke-WtCli -Exactly -Times 0
        (Get-Content $script:target.StatePath -Raw).Trim() | Should -Be 'test state'
        Test-Path "$($script:target.SettingsPath).e2ebak" | Should -BeTrue
        Test-Path "$($script:target.StatePath).e2ebak" | Should -BeTrue
    }

    It 'reports failed activation without guessing process ownership' {
        $script:owned = $null
        { Invoke-Cleanup } | Should -Throw '*No verified fixture window*'
        Should -Invoke Invoke-WtCli -Exactly -Times 0
        Test-Path "$($script:target.SettingsPath).e2ebak" | Should -BeTrue
        Test-Path "$($script:target.StatePath).e2ebak" | Should -BeTrue
    }

    It 'refuses to close a fixture pane moved to another shared-PID window' {
        Mock Get-WtPanes { [pscustomobject]@{ session_id = 'pane-a'; window_id = 8 } }
        Mock Get-WtProcessesForApp { [pscustomobject]@{ Id = 123 } }
        { Invoke-Cleanup } | Should -Throw '*not in the verified window*'
        Should -Invoke Invoke-WtCli -Exactly -Times 0
        Test-Path "$($script:target.StatePath).e2ebak" | Should -BeTrue
    }

    It 'rechecks HWND/PID before closing the second fixture tab' {
        Mock Get-WtWindowHwnds {
            [pscustomobject]@{ hwnd = 456; pid = $(if ($script:remaining.Count -eq 2) { 123 } else { 999 }) }
        }
        { Invoke-Cleanup } | Should -Throw '*handle/PID no longer match*'
        Should -Invoke Invoke-WtCli -Exactly -Times 1
        $script:remaining | Should -Contain 'pane-b'
        Test-Path "$($script:target.SettingsPath).e2ebak" | Should -BeTrue
        Test-Path "$($script:target.StatePath).e2ebak" | Should -BeTrue
    }

    It 'never closes an unrecorded tab in the fixture window' {
        Mock Get-WtWindowHwnds { [pscustomobject]@{ hwnd = 456; pid = 123 } }
        Mock Get-WtProcessesForApp { [pscustomobject]@{ Id = 123 } }
        { Invoke-Cleanup } | Should -Throw '*no process-wide fallback*'
        Should -Invoke Invoke-WtCli -Exactly -Times 2
        Test-Path "$($script:target.SettingsPath).e2ebak" | Should -BeTrue
        Test-Path "$($script:target.StatePath).e2ebak" | Should -BeTrue
    }

    It 'reports close failure without a process-wide fallback' {
        Mock Invoke-WtCli { throw 'pane close failed' }
        Mock Get-WtProcessesForApp { [pscustomobject]@{ Id = 123 } }
        { Invoke-Cleanup } | Should -Throw '*pane close failed*'
        Test-Path "$($script:target.StatePath).e2ebak" | Should -BeTrue
    }
    It 'retains both backups when package inactivity cannot be confirmed' {
        Mock Get-WtProcessesForApp { throw 'process discovery unavailable' }
        { Invoke-Cleanup } | Should -Throw '*process discovery unavailable*'
        (Get-Content $script:target.StatePath -Raw).Trim() | Should -Be 'test state'
        Test-Path "$($script:target.SettingsPath).e2ebak" | Should -BeTrue
        Test-Path "$($script:target.StatePath).e2ebak" | Should -BeTrue
    }

    It 'removes test-created state using the standard missing-file marker' {
        Remove-Item "$($script:target.StatePath).e2ebak"
        New-Item "$($script:target.StatePath).e2ebak.missing" -ItemType File -Force | Out-Null
        $script:stateHash = $null
        Invoke-Cleanup
        Test-Path $script:target.StatePath | Should -BeFalse
        Test-Path "$($script:target.StatePath).e2ebak.missing" | Should -BeFalse
    }

    It 'retains current state when its recovery backup is corrupted' {
        'unexpected backup content' | Set-Content -LiteralPath "$($script:target.StatePath).e2ebak"
        { Invoke-Cleanup } | Should -Throw '*Original state backup does not match*'
        (Get-Content -LiteralPath $script:target.StatePath -Raw).Trim() | Should -Be 'test state'
        Test-Path "$($script:target.StatePath).e2ebak" | Should -BeTrue
    }
}

Describe 'C374 report aggregation' -Tag Unit {
    It 'gates full and incremental sign-off on <Viewport>/<Filter>/<Ordering>' -ForEach @(
        @{ Viewport = 'Success'; Filter = 'Success'; Ordering = 'Success'; Failed = $false }
        @{ Viewport = 'Failure'; Filter = 'Success'; Ordering = 'Success'; Failed = $true }
        @{ Viewport = 'Success'; Filter = 'Failure'; Ordering = 'Success'; Failed = $true }
        @{ Viewport = 'Success'; Filter = 'Success'; Ordering = 'Failure'; Failed = $true }
        @{ Viewport = 'Failure'; Filter = 'Failure'; Ordering = 'Failure'; Failed = $true }
    ) {
        $source = Join-Path $PSScriptRoot '..\tests\Feature.SidebarSessionScroll.Tests.ps1'
        $tokens = $null; $errors = $null
        $ast = [Management.Automation.Language.Parser]::ParseFile($source, [ref]$tokens, [ref]$errors)
        $errors.Count | Should -Be 0
        $describe = $ast.FindAll({
            param($node)
            $node -is [Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq 'Describe'
        }, $true)[0]
        $title = 'Sidebar Agents status updates preserve scroll'
        $describe.CommandElements[1].Value | Should -BeLike "*$title*"
        $cases = @($describe.FindAll({
            param($node)
            $node -is [Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq 'It'
        }, $true))
        $cases.Count | Should -Be 3
        $outcomes = @($Viewport, $Filter, $Ordering)
        $xml = '<test-results><test-suite><results>' + ((0..2 | ForEach-Object {
            '<test-case name="' + $describe.CommandElements[1].Value + '.' +
                $cases[$_].CommandElements[1].Value + '" executed="True" result="' + $outcomes[$_] + '" />'
        }) -join '') + '</results></test-suite></test-results>'
        $results = Join-Path $TestDrive 'results.xml'
        $xml | Set-Content $results
        $checklist = Join-Path $TestDrive 'checklist.md'
        ('- [ ] `C374` `[E2E]` **' + $title + ':** fixture') | Set-Content $checklist
        $report = Join-Path $TestDrive 'full.md'
        & (Join-Path $PSScriptRoot '..\New-ReleaseReport.ps1') -Checklist $checklist -ResultsXml $results -OutFile $report
        $full = Get-Content $report -Raw
        if ($Failed) {
            $full | Should -Match 'AUTOMATION FAILED'
            $full | Should -Not -Match '\[x\] `C374`'
        } else { $full | Should -Match '\[x\] `C374`' }
        ('- [x] `C374` **' + $title + ':** fixture') | Set-Content $report
        & (Join-Path $PSScriptRoot '..\Update-ReleaseReport.ps1') -Report $report -ResultsXml $results
        $updated = Get-Content $report -Raw
        if ($Failed) {
            $updated | Should -Match 'AUTOMATION FAILED'
            $updated | Should -Not -Match '\[x\] `C374`'
        } else { $updated | Should -Match '\[x\] `C374`' }
    }
}
