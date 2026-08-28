#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PR #505: provider-native ACP Yolo modes. This publishable suite is intentionally zero-token:
# deterministic fixtures and provider handshakes prove product behavior without model prompts.
# Real model/tool acceptance lives only in local-tdd-kit/Feature.YoloMode.RealUser.Tests.ps1.

BeforeDiscovery {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    $script:Ready = $false
    $script:copilotStatus = if (Get-Command copilot -ErrorAction SilentlyContinue) { 'probe-failed' } else { 'not-installed' }
    $script:openCodeReady = [bool](Get-Command opencode -ErrorAction SilentlyContinue)
    try {
        $resolvedApp = Resolve-ItApp -Package Dev -ErrorAction Stop
        $script:Ready = Test-WinAppAvailable
        if ($script:copilotStatus -ne 'not-installed') {
            $script:copilotStatus = Get-AgentAcpStatus -App $resolvedApp -AgentCommand 'copilot --acp --stdio'
        }
    }
    catch {
        $script:Ready = $false
    }
    $script:copilotBlocked = $script:copilotStatus -in @('not-installed', 'installed-unauthenticated')
    $script:policyReady = (-not $script:copilotBlocked) -and (Test-WtAgentPolicyControllable)
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

    It 'Yolo setting persists' -Skip:$script:copilotBlocked {
        $configOwner = Resolve-ItApp -Package Dev
        $firstApp = $null
        $secondApp = $null
        try {
            $firstApp = Start-Terminal -Package Dev -PassFre $true -Settings @{
                acpAgent = 'copilot'
                'agentPane.yoloMode' = $false
            }
            Assert-Setting -App $firstApp -Key 'agentPane.yoloMode' -Value $false
            Set-WtSetting -App $firstApp -Key 'agentPane.yoloMode' -Value $true | Out-Null
            Assert-Setting -App $firstApp -Key 'agentPane.yoloMode' -Value $true

            Stop-Terminal -App $firstApp -RestoreSettings $false
            $firstApp = $null

            $secondApp = Start-Terminal -Package Dev -PassFre $true -Backup $false -CleanSettings $false
            Assert-Setting -App $secondApp -Key 'agentPane.yoloMode' -Value $true
            Wait-AgentReady -App $secondApp -TimeoutSec 90 | Should -BeTrue
            $shellPane = Get-ActivePane -App $secondApp
            $agentSession = Wait-NewAgentPaneSession -App $secondApp -OwnerPaneSessionId $shellPane.session_id -TimeoutSec 30
            (Test-Until -TimeoutSec 30 -IntervalSec 0.5 -Condition {
                Test-AgentNativeYoloUpdate -App $secondApp -AcpSessionId $agentSession.AcpSessionId -Enabled $true
            }) | Should -BeTrue -Because 'the relaunched default session must receive the persisted native Yolo setting'
        }
        finally {
            if ($secondApp) { Stop-Terminal -App $secondApp -RestoreSettings $false }
            if ($firstApp) { Stop-Terminal -App $firstApp -RestoreSettings $false }
            Restore-WtConfig -App $configOwner
        }
    }

    It 'Provider-native Yolo toggles per live session' -Skip:$script:copilotBlocked {
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
        $prior = Set-WtAgentPolicy -Policy @{ AllowYoloMode = 'Allowed' }
        $app = $null
        try {
            $app = Start-Terminal -Package Dev -PassFre $true -Settings @{
                acpAgent = 'copilot'
                'agentPane.yoloMode' = $true
            }
            Open-AgentPane -App $app | Out-Null
            Wait-AgentReady -App $app -TimeoutSec 90 | Should -BeTrue
            $shellPane = Get-ActivePane -App $app
            $agentSession = Wait-NewAgentPaneSession -App $app -OwnerPaneSessionId $shellPane.session_id -TimeoutSec 30
            (Test-Until -TimeoutSec 30 -IntervalSec 0.5 -Condition {
                Test-AgentNativeYoloUpdate -App $app -AcpSessionId $agentSession.AcpSessionId -Enabled $true
            }) | Should -BeTrue -Because 'the allowed policy must permit the persisted global-on setting'

            Initialize-LogOffsets -App $app | Out-Null
            Set-WtAgentPolicy -Policy @{ AllowYoloMode = 'Blocked' } | Out-Null
            (Test-Until -TimeoutSec 30 -IntervalSec 0.5 -Condition {
                Test-AgentNativeYoloUpdate -App $app -AcpSessionId $agentSession.AcpSessionId -Enabled $false
            }) | Should -BeTrue -Because 'a live policy block must reconcile the provider session to native Yolo off'
            $agentPane = $agentSession.PaneSessionId
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