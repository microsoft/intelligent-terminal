#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PR #464: discover an OpenCode historical session through ACP session/list,
# render it in /sessions, and resume it with OpenCode's --session CLI contract.

BeforeDiscovery {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    $script:Ready = $false
    try {
        $null = Resolve-ItApp -Package Dev -ErrorAction Stop
        $script:Ready = [bool]((Get-Command opencode -ErrorAction SilentlyContinue) -and (Test-WinAppAvailable))
    }
    catch {
        $script:Ready = $false
    }
}

Describe 'Feature: OpenCode historical session resume' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:opencode = (Get-Command opencode -ErrorAction Stop).Source
        $script:openCodeStatus = Get-AgentCliStatus -Agent opencode -TimeoutSec 60
        $script:app = $null
        $script:fixtureSessionId = $null
    }

    AfterAll {
        if ($script:app) {
            Stop-Terminal -App $script:app
        }
        if ($script:fixtureSessionId) {
            & $script:opencode session delete $script:fixtureSessionId 2>&1 | Out-Null
        }
    }

    It 'OpenCode historical session resumes from the session picker' {
        if ($script:openCodeStatus -ne 'authed') {
            Set-ItResult -Skipped -Because "OpenCode cannot answer non-interactively ($script:openCodeStatus)"
            return
        }

        $suffix = [guid]::NewGuid().ToString('N').Substring(0, 8)
        $title = "ITE2E OpenCode Resume $suffix"
        $seed = "RESUMESEED$suffix"

        Push-Location (Join-Path $env:WINDIR 'System32')
        try {
            $seedOutput = (& $script:opencode run --title $title "Reply with only the token $seed." 2>&1) -join "`n"
            $seedExitCode = $LASTEXITCODE
            $sessions = ((& $script:opencode session list --format json 2>&1) -join "`n") | ConvertFrom-Json
        }
        finally {
            Pop-Location
        }
        $seedExitCode | Should -Be 0 -Because "OpenCode fixture creation failed: $seedOutput"
        $seedOutput | Should -Match ([regex]::Escape($seed)) -Because 'the historical session must contain the seed response'

        $fixture = @($sessions | Where-Object title -eq $title)
        $fixture | Should -HaveCount 1 -Because 'the unique OpenCode fixture must be discoverable in its session store'
        $script:fixtureSessionId = $fixture[0].id

        $script:app = Start-Terminal -Package Dev -PassFre $true -Settings @{ acpAgent = 'opencode' }
        $shellPane = Get-ActivePane -App $script:app
        Open-AgentPane -App $script:app | Out-Null
        $agentPane = (Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $shellPane.session_id -TimeoutSec 30).PaneSessionId
        Wait-AgentReady -App $script:app -PaneSessionId $agentPane -TimeoutSec 90 |
            Should -BeTrue -Because 'OpenCode ACP must connect before session/list can discover history'

        Initialize-LogOffsets -App $script:app | Out-Null
        Open-SessionList -App $script:app -TimeoutSec 30 | Out-Null
        Send-AgentKey -App $script:app -PaneSessionId $agentPane -Key F5 | Out-Null
        $rowVisible = Test-Until -TimeoutSec 60 -IntervalSec 1 -Condition {
            (Get-AgentPaneText -App $script:app -PaneSessionId $agentPane -MaxLines 80) -match [regex]::Escape($title)
        }
        $rowVisible | Should -BeTrue -Because 'ACP session/list must surface the OpenCode historical session in /sessions'

        $windowId = [string]$script:app.WindowId
        $beforeTabs = @((Get-WtTabs -App $script:app -WindowId $windowId).tab_id)
        Resume-Session -App $script:app -Match ([regex]::Escape($title)) | Out-Null

        $created = Test-Until -TimeoutSec 30 -IntervalSec 0.5 -Condition {
            $newTab = @(Get-WtTabs -App $script:app -WindowId $windowId) |
                Where-Object { $_.tab_id -notin $beforeTabs } |
                Select-Object -First 1
            $null -ne $newTab
        }
        $created | Should -BeTrue -Because 'resuming the historical row must open a new terminal tab'
        $newTab = @(Get-WtTabs -App $script:app -WindowId $windowId) |
            Where-Object { $_.tab_id -notin $beforeTabs } |
            Select-Object -First 1

        $scheduled = Test-Until -TimeoutSec 15 -IntervalSec 0.5 -Condition {
            $log = Get-ItLogText -App $script:app -Name 'wta-main_helper-*.log' -SinceStart
            $log -match 'dispatch_resume: new-tab scheduled' -and
            $log -match [regex]::Escape("opencode --session $script:fixtureSessionId")
        }
        $scheduled | Should -BeTrue -Because 'the picker must dispatch OpenCode with its real --session contract'

        $paneReady = Test-Until -TimeoutSec 20 -IntervalSec 0.5 -Condition {
            $resumedPane = @(Get-WtPanes -App $script:app -TabId $newTab.tab_id -WindowId $windowId) |
                Select-Object -First 1
            $null -ne $resumedPane
        }
        $paneReady | Should -BeTrue
        $resumedPane = @(Get-WtPanes -App $script:app -TabId $newTab.tab_id -WindowId $windowId) |
            Select-Object -First 1
        Assert-Pane -App $script:app -SessionId $resumedPane.session_id -Match ([regex]::Escape($seed)) -TimeoutSec 90
    }
}
