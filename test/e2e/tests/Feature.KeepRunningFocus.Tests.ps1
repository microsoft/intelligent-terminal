#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Exercises the shared focus-pane boundary used by Agent history and session
# activation: a kept tab must be reattached, not recreated from a command line.

BeforeDiscovery {
    $script:Ready = [bool]((Get-Command pwsh -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue))
}

Describe 'Feature: focus kept sessions' -Tag 'Feature', 'KeepRunning' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = $null
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpInteractionAgent.ps1')).Path
        $requestLog = Join-Path $TestDrive 'keep-running-acp.log'
        $invocation = "& '$($fixture.Replace("'", "''"))' -LogPath '$($requestLog.Replace("'", "''"))'"
        $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation))
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            language = 'en-US'
            tabLayout = 'vertical'
            confirmOnClose = 'never'
            autoErrorDetectionEnabled = $false
            acpAgent = 'custom:keep-running-fixture'
            acpCustomCommand = "pwsh -NoProfile -EncodedCommand $encoded"
            acpModel = ''
        }
    }
    AfterAll {
        if ($script:app) { Stop-Terminal -App $script:app }
    }

    It 'Focusing a kept session reattaches its original tab' {
        $other = Get-ActivePane -App $script:app
        Wait-NewAgentPaneSession -App $script:app -TimeoutSec 40 | Out-Null
        $existingHelpers = @(Get-AgentPaneSessions -App $script:app).PaneSessionId
        $title = 'IT-kept-' + [guid]::NewGuid().ToString('N').Substring(0, 12)
        $target = New-WtTab -App $script:app -Command 'pwsh -NoProfile' -Title $title
        $split = Split-WtPane -App $script:app -SessionId $target.session_id -Direction right -Size 0.35 -Command 'pwsh -NoProfile'
        Set-WtPaneFocus -App $script:app -SessionId $target.session_id
        $helper = Wait-NewAgentPaneSession -App $script:app -ExcludePaneSessionId $existingHelpers -TimeoutSec 40
        $helper.AcpSessionId | Should -Not -BeNullOrEmpty
        $before = @{}
        foreach ($id in @($target.session_id, $split.session_id, $helper.PaneSessionId)) {
            $before[$id] = Get-WtPaneStatus -App $script:app -SessionId $id
        }
        $marker = [guid]::NewGuid().ToString('N')
        Send-WtInput -App $script:app -SessionId $target.session_id -Text "`$keepFocusProbe = '$marker'`r"
        Send-WtInput -App $script:app -SessionId $target.session_id -Text 'Write-Output ($keepFocusProbe + ":before")'
        Send-WtKeys -App $script:app -SessionId $target.session_id -Keys Enter
        Wait-Until -TimeoutSec 10 -Because 'the original shell to initialize its retained variable' -Condition {
            (Get-WtCapture -App $script:app -SessionId $target.session_id) -match "$marker`:before"
        } | Out-Null
        $windowId = [string]$target.window_id
        $getTabCount = {
            @((Get-WtWindows -App $script:app) | Where-Object { [string]$_.window_id -eq $windowId })[0].tab_count
        }
        $tabCount = & $getTabCount

        foreach ($cycle in 1..2) {
            Invoke-UiClick -App $script:app -Selector $title -Right | Out-Null
            if ($cycle -eq 1) {
                Invoke-UiElement -App $script:app -Selector 'KeepTabRunningMenuItem' | Out-Null
                Invoke-UiClick -App $script:app -Selector $title -Right | Out-Null
            }
            Invoke-UiElement -App $script:app -Selector 'Close tab' | Out-Null
            Wait-Until -TimeoutSec 10 -Because 'the kept tab to leave the visible tab strip' -Condition {
                (& $getTabCount) -eq ($tabCount - 1)
            } | Out-Null

            Set-WtPaneFocus -App $script:app -SessionId $other.session_id
            (& $getTabCount) | Should -Be ($tabCount - 1) -Because 'focusing another tab must not restore this one'
            { Invoke-WtCli -App $script:app -Arguments @('focus-pane', '-t', [guid]::NewGuid().ToString()) } |
                Should -Throw
            (& $getTabCount) | Should -Be ($tabCount - 1) -Because 'an unknown pane must not restore an unrelated kept tab'

            Set-WtPaneFocus -App $script:app -SessionId $target.session_id
            (& $getTabCount) | Should -Be $tabCount
            foreach ($id in $before.Keys) {
                $current = Get-WtPaneStatus -App $script:app -SessionId $id
                $current.pid | Should -Be $before[$id].pid -Because 'reattach must retain each original shell/helper process'
                $current.state | Should -Be 'running'
            }
            $currentHelper = Get-AgentPaneSession -App $script:app -PaneSessionId $helper.PaneSessionId
            $currentHelper.HelperProcessId | Should -Be $helper.HelperProcessId
            $currentHelper.AcpSessionId | Should -Be $helper.AcpSessionId
            Send-WtInput -App $script:app -SessionId $target.session_id -Text "Write-Output (`$keepFocusProbe + ':after$cycle')`r"
            Wait-Until -TimeoutSec 10 -Because 'the restored original shell to retain variables and accept input' -Condition {
                (Get-WtCapture -App $script:app -SessionId $target.session_id) -match "$marker`:after$cycle"
            } | Out-Null
            Set-WtPaneFocus -App $script:app -SessionId $target.session_id
            (& $getTabCount) | Should -Be $tabCount -Because 'repeated activation must not duplicate the tab'
        }
    }
}
