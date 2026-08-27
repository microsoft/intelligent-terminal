#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PR #505: provider-native ACP Yolo modes. This publishable suite is intentionally zero-token:
# deterministic fixtures and provider handshakes prove product behavior without model prompts.
# Real model/tool acceptance lives only in local-tdd-kit/Feature.YoloMode.RealUser.Tests.ps1.

BeforeDiscovery {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    $script:Ready = $false
    $script:copilotReady = [bool](Get-Command copilot -ErrorAction SilentlyContinue)
    $script:openCodeReady = [bool](Get-Command opencode -ErrorAction SilentlyContinue)
    $script:policyReady = $script:copilotReady -and (Test-WtAgentPolicyControllable)
    try {
        $null = Resolve-ItApp -Package Dev -ErrorAction Stop
        $script:Ready = Test-WinAppAvailable
    }
    catch {
        $script:Ready = $false
    }
}

Describe 'Feature Yolo mode permission boundary' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:fixtureLog = Join-Path $env:TEMP "ite2e-yolo-permission-$([guid]::NewGuid().ToString('N')).log"
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpPermissionAgent.ps1')).Path
        if ($fixture -match '\s' -or $script:fixtureLog -match '\s') {
            throw 'The custom ACP fixture requires whitespace-free test paths'
        }
        $fixtureCommand = "pwsh -NoProfile -File $fixture -LogPath $script:fixtureLog"
        $script:app = Start-Terminal -Package Dev -PassFre $true -Settings @{
            acpAgent = 'custom:yolo-permission-fixture'
            acpCustomCommand = $fixtureCommand
            'agentPane.yoloMode' = $true
        }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue
        $shellPane = Get-ActivePane -App $script:app
        $script:agentPane = (Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $shellPane.session_id -TimeoutSec 30).PaneSessionId
    }
    AfterAll {
        if ($script:app) { Stop-Terminal -App $script:app }
        Remove-Item -LiteralPath $script:fixtureLog -Force -ErrorAction SilentlyContinue
    }

    It 'Yolo never answers ACP permissions' {
        Assert-Setting -App $script:app -Key 'agentPane.yoloMode' -Value $true
        $marker = 'PERM' + [guid]::NewGuid().ToString('N').Substring(0, 12)
        Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text $marker | Out-Null

        (Wait-AgentPermission -App $script:app -TimeoutSec 30) |
            Should -BeTrue -Because 'the provider permission must remain pending for the user'
        $before = if (Test-Path $script:fixtureLog) {
            Get-Content -LiteralPath $script:fixtureLog -Raw
        } else { '' }
        $before | Should -Match "permission-requested\|$marker"
        $before | Should -Not -Match "permission-resolved\|.*\|$marker"

        Send-AgentKey -App $script:app -PaneSessionId $script:agentPane -Key Y | Out-Null
        (Test-Until -TimeoutSec 20 -IntervalSec 0.5 -Condition {
            (Get-Content -LiteralPath $script:fixtureLog -Raw -ErrorAction SilentlyContinue) -match
                "permission-resolved\|allow-once\|$marker"
        }) | Should -BeTrue -Because 'only the explicit Y key should select AllowOnce'
        Assert-AgentPaneText -App $script:app -PaneSessionId $script:agentPane `
            -Pattern "PERMISSION_RESULT_${marker}_allow-once" -TimeoutSec 20
    }
}

Describe 'Feature provider-native Yolo with Copilot' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    }

    It 'Yolo setting persists' -Skip:(-not $script:copilotReady) {
        $app = Start-Terminal -Package Dev -PassFre $true -Settings @{
            acpAgent = 'copilot'
            'agentPane.yoloMode' = $true
        }
        try {
            Assert-Setting -App $app -Key 'agentPane.yoloMode' -Value $true
        }
        finally {
            if ($app) { Stop-Terminal -App $app }
        }
    }

    It 'Provider-native Yolo toggles per live session' -Skip:(-not $script:copilotReady) {
        $app = Start-Terminal -Package Dev -PassFre $true -Settings @{
            acpAgent = 'copilot'
            'agentPane.yoloMode' = $true
        }
        try {
            Open-AgentPane -App $app | Out-Null
            Wait-AgentReady -App $app -TimeoutSec 90 | Should -BeTrue
            $shellPane = Get-ActivePane -App $app
            $agentPane = (Wait-NewAgentPaneSession -App $app -OwnerPaneSessionId $shellPane.session_id -TimeoutSec 30).PaneSessionId

            Initialize-LogOffsets -App $app | Out-Null
            Send-AgentPrompt -App $app -PaneSessionId $agentPane -Text '/yolo off' | Out-Null
            Assert-Log -App $app -Name 'wta-main_helper-*.log' `
                -Pattern 'provider-native Yolo updated for live session.*enabled=false' -TimeoutSec 30
            Initialize-LogOffsets -App $app | Out-Null
            Send-AgentPrompt -App $app -PaneSessionId $agentPane -Text '/yolo on' | Out-Null
            Assert-Log -App $app -Name 'wta-main_helper-*.log' `
                -Pattern 'provider-native Yolo updated for live session.*enabled=true' -TimeoutSec 30
        }
        finally {
            if ($app) { Stop-Terminal -App $app }
        }
    }

}

Describe 'Feature unsupported provider Yolo behavior' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    }

    It 'Unsupported agents reject Yolo safely' -Skip:(-not $script:openCodeReady) {
        $app = Start-Terminal -Package Dev -PassFre $true -Settings @{ acpAgent = 'opencode' }
        try {
            Open-AgentPane -App $app | Out-Null
            Wait-AgentReady -App $app -TimeoutSec 90 | Should -BeTrue
            $shellPane = Get-ActivePane -App $app
            $agentPane = (Wait-NewAgentPaneSession -App $app -OwnerPaneSessionId $shellPane.session_id -TimeoutSec 30).PaneSessionId
            Send-AgentPrompt -App $app -PaneSessionId $agentPane -Text '/yolo on' | Out-Null
            Assert-AgentPaneText -App $app -PaneSessionId $agentPane `
                -Pattern '(?i)/yolo on: opencode does not support ACP session Yolo mode' -TimeoutSec 30
        }
        finally {
            if ($app) { Stop-Terminal -App $app }
        }
    }
}

Describe 'Feature AllowYoloMode policy' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    }

    It 'AllowYoloMode policy blocks Yolo' -Skip:(-not $script:policyReady) {
        $prior = Set-WtAgentPolicy -Policy @{ AllowYoloMode = 'Blocked' }
        $app = $null
        try {
            $app = Start-Terminal -Package Dev -PassFre $true -Settings @{
                acpAgent = 'copilot'
                'agentPane.yoloMode' = $true
            }
            Open-AgentPane -App $app | Out-Null
            Wait-AgentReady -App $app -TimeoutSec 90 | Should -BeTrue
            $shellPane = Get-ActivePane -App $app
            $agentPane = (Wait-NewAgentPaneSession -App $app -OwnerPaneSessionId $shellPane.session_id -TimeoutSec 30).PaneSessionId
            Send-AgentPrompt -App $app -PaneSessionId $agentPane -Text '/yolo on' | Out-Null
            $blocked = Get-WtaLocalizedTextRegex -Key 'system.yolo_blocked_by_policy'
            if (-not $blocked) { $blocked = '(?i)Yolo mode is disabled' }
            Assert-AgentPaneText -App $app -PaneSessionId $agentPane -Pattern $blocked -TimeoutSec 30
        }
        finally {
            if ($app) { Stop-Terminal -App $app }
            if ($prior) { Restore-WtAgentPolicy -State $prior }
        }
    }
}