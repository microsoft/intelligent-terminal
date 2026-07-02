#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Eternal Terminal `/save-tab` + `/restore-tab` end-to-end coverage.
#   Invoke-Pester test/e2e/tests -Tag Feature

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue)) }

Describe 'Feature: Eternal Terminal save/restore tab commands' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        if (-not (Test-WinAppAvailable)) { throw "winapp (Windows App CLI) not found. Run test/e2e/bootstrap.ps1 or install Microsoft.winappcli." }

        $script:SnapshotTitle = 'e2e-snapshot'
        $script:SnapshotId = $null
        $script:SnapshotIndex = $null
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent = 'copilot'
            'experimental.eternalTerminal.enabled' = $true
        }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue -Because 'the copilot agent pane must reach a connected ACP session before driving slash commands'

        $script:GetWtaLocalizedTextPrefixRegex = {
            param([Parameter(Mandatory)][string]$Key, [int]$WordCount = 3, [int]$MaxChars = 32)
            try {
                $localeDir = Join-Path $PSScriptRoot '..\..\..\tools\wta\locales'
                if (-not (Test-Path $localeDir)) { return $null }
                $escKey = [regex]::Escape($Key)
                $prefixes = Select-String -Path (Join-Path $localeDir '*.yml') -Pattern ('^\s*' + $escKey + ':\s*(\S.*)$') |
                    ForEach-Object {
                        $raw = $_.Matches[0].Groups[1].Value.Trim()
                        if ($raw -match '^"((?:[^"\\]|\\.)*)"') {
                            $val = [regex]::Replace($Matches[1], '\\(.)', {
                                param($m)
                                switch ($m.Groups[1].Value) {
                                    'n' { "`n" } 't' { "`t" } 'r' { "`r" }
                                    default { $m.Groups[1].Value }
                                }
                            })
                        }
                        elseif ($raw -match "^'((?:[^']|'')*)'") { $val = ($Matches[1] -replace "''", "'") }
                        else { $val = ($raw -replace '\s+#.*$', '').Trim() }
                        $val -replace '\s+\([^)]*\)\s*$', '' -replace '\.+\s*$', ''
                    } |
                    Where-Object { $_ } |
                    ForEach-Object {
                        $words = @($_ -split '\s+' | Where-Object { $_ })
                        $prefix = if ($words.Count -ge $WordCount) { ($words | Select-Object -First $WordCount) -join ' ' } else { $_ }
                        if ($prefix.Length -gt $MaxChars) { $prefix = $prefix.Substring(0, $MaxChars) }
                        [regex]::Escape($prefix)
                    } |
                    Where-Object { $_ } |
                    Select-Object -Unique
                $prefixes = @($prefixes)
                if ($prefixes.Count) { return '(?i)(' + ($prefixes -join '|') + ')' }
            }
            catch { return $null }
            $null
        }
        # The popup row can clip the full summary at narrow widths; match a localized prefix from the bundled yml instead.
        $script:SaveTabSummaryPrefixRegex = & $script:GetWtaLocalizedTextPrefixRegex -Key 'commands.save_tab.summary'
        $script:SaveTabSummaryPrefixRegex | Should -Not -BeNullOrEmpty -Because 'the /save-tab command summary prefix must be asserted through bundled WTA locale keys'
        $saveTabSavedRegex = Get-WtaLocalizedTextRegex -Key 'commands.save_tab.saved'
        $saveTabSavedRegex | Should -Not -BeNullOrEmpty -Because 'the /save-tab success text must be asserted through bundled WTA locale keys'
        $script:SaveTabSavedRegex = $saveTabSavedRegex.Replace([regex]::Escape('%{title}'), [regex]::Escape($script:SnapshotTitle))
        $restoreOpenedRegex = Get-WtaLocalizedTextRegex -Key 'commands.restore_tab.opened'
        $restoreFocusedRegex = Get-WtaLocalizedTextRegex -Key 'commands.restore_tab.focused'
        $script:RestoreOutcomeRegex = (@($restoreOpenedRegex, $restoreFocusedRegex) | Where-Object { $_ }) -join '|'
        $script:RestoreOutcomeRegex | Should -Not -BeNullOrEmpty -Because 'the /restore-tab outcome text must be asserted through bundled WTA locale keys'

        $script:GetSavedTabs = {
            $rows = Invoke-WtCli -App $script:app -Arguments @('list-saved-tabs') -TimeoutSec 20
            @($rows) | Where-Object { $_ }
        }
        $script:DeleteSavedTab = {
            param([Parameter(Mandatory)][string]$Id)
            Invoke-WtCli -App $script:app -Arguments @('delete-saved-tab', '-i', $Id) -TimeoutSec 20 -NoThrow | Out-Null
        }
        $script:DeleteSnapshotsByTitle = {
            param([Parameter(Mandatory)][string]$Title)
            foreach ($row in @(& $script:GetSavedTabs)) {
                if ($row.title -eq $Title -and $row.id) { & $script:DeleteSavedTab $row.id }
            }
            Wait-Until -TimeoutSec 10 -IntervalSec 0.5 -Quiet -Because "saved tab '$Title' to be absent" -Condition {
                $rows = @(& $script:GetSavedTabs)
                -not @($rows | Where-Object { $_.title -eq $Title }).Count
            } | Out-Null
        }
        & $script:DeleteSnapshotsByTitle $script:SnapshotTitle
    }

    AfterAll {
        try {
            if ($script:app) {
                $ids = @()
                if ($script:SnapshotId) { $ids += $script:SnapshotId }
                if ($script:GetSavedTabs) {
                    foreach ($row in @(& $script:GetSavedTabs)) {
                        if ($row.title -eq $script:SnapshotTitle -and $row.id) { $ids += $row.id }
                    }
                }
                foreach ($id in @($ids | Where-Object { $_ } | Select-Object -Unique)) {
                    & $script:DeleteSavedTab $id
                }
                if ($script:GetSavedTabs) {
                    $removed = Wait-Until -TimeoutSec 10 -IntervalSec 0.5 -Quiet -Because "saved tab '$($script:SnapshotTitle)' to be deleted" -Condition {
                        $rows = @(& $script:GetSavedTabs)
                        -not @($rows | Where-Object { $_.title -eq $script:SnapshotTitle }).Count
                    }
                    $removed | Should -BeTrue -Because 'the e2e snapshot should be removed by teardown'
                }
            }
        }
        finally {
            if ($script:app) { Stop-Terminal -App $script:app }
        }
    }

    It 'shows /save-tab in the command menu when enabled' {
        Clear-AgentInput -App $script:app | Out-Null
        Send-AgentPrompt -App $script:app -Text '/save' -NoSubmit | Out-Null
        Assert-AgentPaneText -App $script:app -Pattern $script:SaveTabSummaryPrefixRegex -TimeoutSec 10
        Clear-AgentInput -App $script:app | Out-Null
    }

    It 'saves a tab and lists it' {
        Clear-AgentInput -App $script:app | Out-Null
        Send-AgentPrompt -App $script:app -Text "/save-tab $($script:SnapshotTitle)" | Out-Null
        Assert-AgentPaneText -App $script:app -Pattern $script:SaveTabSavedRegex -TimeoutSec 20

        $row = Wait-Until -TimeoutSec 15 -IntervalSec 0.5 -Because "wtcli list-saved-tabs to include '$($script:SnapshotTitle)'" -Condition {
            $rows = @(& $script:GetSavedTabs)
            $rows | Where-Object { $_.title -eq $script:SnapshotTitle } | Select-Object -First 1
        }
        $row | Should -Not -BeNullOrEmpty
        $row.id | Should -Not -BeNullOrEmpty
        $script:SnapshotId = $row.id

        $rows = @(& $script:GetSavedTabs)
        for ($i = 0; $i -lt $rows.Count; $i++) {
            if ($rows[$i].id -eq $script:SnapshotId) {
                $script:SnapshotIndex = $i
                break
            }
        }
        $script:SnapshotIndex | Should -Not -Be $null
    }

    It 'restores a saved tab' {
        if (-not $script:SnapshotId) {
            $row = @(& $script:GetSavedTabs | Where-Object { $_.title -eq $script:SnapshotTitle } | Select-Object -First 1)
            $row | Should -Not -BeNullOrEmpty -Because 'the save/list test should have created e2e-snapshot before restore'
            $script:SnapshotId = $row.id
        }

        Clear-AgentInput -App $script:app | Out-Null
        $agentSession = (Send-AgentPrompt -App $script:app -Text '/restore-tab').PaneSessionId
        $savedRows = @(& $script:GetSavedTabs)
        $savedRows.Count | Should -BeGreaterThan 0
        Assert-AgentPaneText -App $script:app -Pattern ([regex]::Escape($savedRows[0].title)) -TimeoutSec 15

        $script:SnapshotIndex = $null
        for ($i = 0; $i -lt $savedRows.Count; $i++) {
            if ($savedRows[$i].id -eq $script:SnapshotId) {
                $script:SnapshotIndex = $i
                break
            }
        }
        $script:SnapshotIndex | Should -Not -Be $null -Because 'the restore picker selection index should resolve to e2e-snapshot'
        $selectedSnapshotTitle = $savedRows[$script:SnapshotIndex].title
        if ($script:SnapshotIndex -gt 0) {
            Send-AgentKey -App $script:app -Key Down -Count $script:SnapshotIndex -PaneSessionId $agentSession | Out-Null
        }
        Assert-AgentPaneText -App $script:app -Pattern ('>\s*' + [regex]::Escape($selectedSnapshotTitle)) -TimeoutSec 10
        Send-AgentKey -App $script:app -Key Enter -PaneSessionId $agentSession | Out-Null
        $restored = Test-Until -TimeoutSec 20 -IntervalSec 0.5 -Condition {
            (Get-WtCapture -App $script:app -SessionId $agentSession -MaxLines 100) -match $script:RestoreOutcomeRegex
        }
        $restored | Should -BeTrue -Because 'the original agent pane should show the localized restore outcome'
    }
}
