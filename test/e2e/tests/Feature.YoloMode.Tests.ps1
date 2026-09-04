#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PR #505: provider-native ACP Yolo modes. This publishable suite is intentionally zero-token:
# deterministic fixtures and provider handshakes prove product behavior without model prompts.
# Real model/tool acceptance lives only in local-tdd-kit/Feature.YoloMode.RealUser.Tests.ps1.

BeforeDiscovery {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    $script:Package = Get-ItTestPackage
    $script:Ready = $false
    $script:copilotStatus = if (Get-Command copilot -ErrorAction SilentlyContinue) { 'probe-failed' } else { 'not-installed' }
    $script:openCodeInstalled = [bool](Get-Command opencode -ErrorAction SilentlyContinue)
    $script:geminiInstalled = [bool](Get-Command gemini -ErrorAction SilentlyContinue)
    try {
        $resolvedApp = Resolve-ItApp -Package $script:Package -ErrorAction Stop
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
    $script:PackageCase = @(@{
        Package = $script:Package
        OpenCodeInstalled = $script:openCodeInstalled
        GeminiInstalled = $script:geminiInstalled
    })
}

Describe 'Feature custom-provider permission baseline' -ForEach $script:PackageCase -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:fixtureLog = Join-Path $env:TEMP "ite2e-yolo-permission-$([guid]::NewGuid().ToString('N')).log"
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpPermissionAgent.ps1')).Path
        $fixtureInvocation = "& '$($fixture.Replace("'", "''"))' -LogPath '$($script:fixtureLog.Replace("'", "''"))'"
        $encodedInvocation = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($fixtureInvocation))
        $fixtureCommand = "pwsh -NoProfile -EncodedCommand $encodedInvocation"
        $script:app = Start-Terminal -Package $Package -PassFre $true -Settings @{
            acpAgent = 'custom:yolo-permission-fixture'
            acpCustomCommand = $fixtureCommand
            'agentPane.yoloMode' = $true
        }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue
        $script:agentPane = (Wait-NewAgentPaneSession -App $script:app -TimeoutSec 30).PaneSessionId
    }
    AfterAll {
        if ($script:app) { Stop-Terminal -App $script:app }
        Remove-Item -LiteralPath $script:fixtureLog -Force -ErrorAction SilentlyContinue
    }

    It 'Permission UI works' {
        Assert-Setting -App $script:app -Key 'agentPane.yoloMode' -Value $true
        $allowMarker = 'PERM' + [guid]::NewGuid().ToString('N').Substring(0, 12)
        Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text $allowMarker | Out-Null

        (Wait-AgentPermission -App $script:app -TimeoutSec 30) |
            Should -BeTrue -Because 'the provider permission must remain pending for the user'
        $before = if (Test-Path $script:fixtureLog) {
            Get-Content -LiteralPath $script:fixtureLog -Raw
        } else { '' }
        $before | Should -Match "permission-requested\|$allowMarker"
        $before | Should -Not -Match "permission-resolved\|.*\|$allowMarker"

        Send-AgentKey -App $script:app -PaneSessionId $script:agentPane -Key Y | Out-Null
        (Test-Until -TimeoutSec 20 -IntervalSec 0.5 -Condition {
            (Get-Content -LiteralPath $script:fixtureLog -Raw -ErrorAction SilentlyContinue) -match
                "permission-resolved\|allow-once\|$allowMarker"
        }) | Should -BeTrue -Because 'only the explicit Y key should select AllowOnce'
        Assert-AgentPaneText -App $script:app -PaneSessionId $script:agentPane `
            -Pattern "PERMISSION_RESULT_${allowMarker}_allow-once" -TimeoutSec 20

        $rejectMarker = 'PERM' + [guid]::NewGuid().ToString('N').Substring(0, 12)
        Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text $rejectMarker | Out-Null
        (Wait-AgentPermission -App $script:app -TimeoutSec 30) |
            Should -BeTrue -Because 'a second provider permission must remain available for rejection'
        Send-AgentKey -App $script:app -PaneSessionId $script:agentPane -Key N | Out-Null
        (Test-Until -TimeoutSec 20 -IntervalSec 0.5 -Condition {
            (Get-Content -LiteralPath $script:fixtureLog -Raw -ErrorAction SilentlyContinue) -match
                "permission-resolved\|reject-once\|$rejectMarker"
        }) | Should -BeTrue -Because 'the explicit N key should select RejectOnce'
        Assert-AgentPaneText -App $script:app -PaneSessionId $script:agentPane `
            -Pattern "PERMISSION_RESULT_${rejectMarker}_reject-once" -TimeoutSec 20
    }
}

Describe 'Feature provider-native Yolo with Copilot' -ForEach $script:PackageCase -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    }

    It 'Yolo setting persists' -Skip:$script:copilotBlocked {
        $configOwner = Resolve-ItApp -Package $Package
        $firstApp = $null
        $secondApp = $null
        try {
            $firstApp = Start-Terminal -Package $Package -PassFre $true -Settings @{
                acpAgent = 'copilot'
                'agentPane.yoloMode' = $false
            }
            Assert-Setting -App $firstApp -Key 'agentPane.yoloMode' -Value $false
            Set-WtSetting -App $firstApp -Key 'agentPane.yoloMode' -Value $true | Out-Null
            Assert-Setting -App $firstApp -Key 'agentPane.yoloMode' -Value $true

            Stop-Terminal -App $firstApp -RestoreSettings $false
            $firstApp = $null

            $secondApp = Start-Terminal -Package $Package -PassFre $true -Backup $false -CleanSettings $false
            Assert-Setting -App $secondApp -Key 'agentPane.yoloMode' -Value $true
            Wait-AgentReady -App $secondApp -TimeoutSec 90 | Should -BeTrue
            $agentSession = Wait-NewAgentPaneSession -App $secondApp -TimeoutSec 30
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

}

Describe 'Feature Settings Yolo provider compatibility' -ForEach $script:PackageCase -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    }

    It 'Settings warns that OpenCode Yolo remains interactive' {
        if (-not $OpenCodeInstalled) {
            Set-ItResult -Skipped -Because 'OpenCode is not installed, so it is intentionally absent from the default-provider picker'
            return
        }

        $app = Start-Terminal -Package $Package -PassFre $true -Settings @{
            acpAgent = 'opencode'
            'agentPane.yoloMode' = $true
        }
        try {
            try { Open-WtSettings -App $app -TimeoutSec 20 | Out-Null }
            catch {
                Set-ItResult -Skipped -Because "the WT window could not take foreground to open Settings: $($_.Exception.Message)"
                return
            }
            Invoke-SettingsNav -App $app -NavItem 'AIAgentsNavItem' | Out-Null

            Wait-UiElement -App $app -Selector 'OpenCodeYoloCompatibilityInfoBar' -TimeoutSec 15 | Out-Null
            $title = Get-WtReswTextRegex -Key 'AIAgents_YoloOpenCodeWarning.Title'
            (Get-UiTree -App $app -Selector 'OpenCodeYoloCompatibilityInfoBar' -Depth 4) |
                Should -Match $title -Because 'the warning must explain the selected default provider limitation'
            Test-UiElementExists -App $app -Selector 'GeminiYoloCompatibilityInfoBar' -TimeoutSec 1 |
                Should -BeFalse -Because 'only the selected default provider should have a compatibility notice'

            Invoke-UiElement -App $app -Selector 'AgentPaneYoloModeToggle' | Out-Null
            Wait-UiElement -App $app -Selector 'OpenCodeYoloCompatibilityInfoBar' -Gone -TimeoutSec 10 | Out-Null
            Invoke-UiElement -App $app -Selector 'AgentPaneYoloModeToggle' | Out-Null
            Wait-UiElement -App $app -Selector 'OpenCodeYoloCompatibilityInfoBar' -TimeoutSec 10 | Out-Null
        }
        finally {
            if ($app) { Stop-Terminal -App $app }
        }
    }

    It 'Settings explains Gemini workspace trust dependency' {
        if (-not $GeminiInstalled) {
            Set-ItResult -Skipped -Because 'Gemini is not installed, so it is intentionally absent from the default-provider picker'
            return
        }

        $app = Start-Terminal -Package $Package -PassFre $true -Settings @{
            acpAgent = 'gemini'
            'agentPane.yoloMode' = $true
        }
        try {
            try { Open-WtSettings -App $app -TimeoutSec 20 | Out-Null }
            catch {
                Set-ItResult -Skipped -Because "the WT window could not take foreground to open Settings: $($_.Exception.Message)"
                return
            }
            Invoke-SettingsNav -App $app -NavItem 'AIAgentsNavItem' | Out-Null

            Wait-UiElement -App $app -Selector 'GeminiYoloCompatibilityInfoBar' -TimeoutSec 15 | Out-Null
            $title = Get-WtReswTextRegex -Key 'AIAgents_YoloGeminiInfo.Title'
            (Get-UiTree -App $app -Selector 'GeminiYoloCompatibilityInfoBar' -Depth 4) |
                Should -Match $title -Because 'the informational notice must describe Gemini workspace trust'
            Test-UiElementExists -App $app -Selector 'OpenCodeYoloCompatibilityInfoBar' -TimeoutSec 1 |
                Should -BeFalse -Because 'only the selected default provider should have a compatibility notice'
        }
        finally {
            if ($app) { Stop-Terminal -App $app }
        }
    }
}

Describe 'Feature AllowYoloMode policy' -ForEach $script:PackageCase -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    }

    It 'AllowYoloMode policy blocks Yolo' -Skip:(-not $script:policyReady) {
        $prior = Set-WtAgentPolicy -Policy @{ AllowYoloMode = 'Allowed' }
        $app = $null
        try {
            $app = Start-Terminal -Package $Package -PassFre $true -Settings @{
                acpAgent = 'copilot'
                'agentPane.yoloMode' = $true
            }
            Open-AgentPane -App $app | Out-Null
            Wait-AgentReady -App $app -TimeoutSec 90 | Should -BeTrue
            $agentSession = Wait-NewAgentPaneSession -App $app -TimeoutSec 30
            (Test-Until -TimeoutSec 30 -IntervalSec 0.5 -Condition {
                Test-AgentNativeYoloUpdate -App $app -AcpSessionId $agentSession.AcpSessionId -Enabled $true
            }) | Should -BeTrue -Because 'the allowed policy must permit the persisted global-on setting'

            Initialize-LogOffsets -App $app | Out-Null
            Set-WtAgentPolicy -Policy @{ AllowYoloMode = 'Blocked' } | Out-Null
            (Test-Until -TimeoutSec 30 -IntervalSec 0.5 -Condition {
                Test-AgentNativeYoloUpdate -App $app -AcpSessionId $agentSession.AcpSessionId -Enabled $false
            }) | Should -BeTrue -Because 'a live policy block must reconcile the provider session to native Yolo off'

            Open-AgentCommandMenu -App $app -PaneSessionId $agentSession.PaneSessionId | Out-Null
            (Test-AgentPopupShown -App $app -PaneSessionId $agentSession.PaneSessionId `
                    -Pattern '(?i)/allow_all' -TimeoutSec 15) |
                Should -BeTrue -Because 'the policy check targets Copilot''s advertised command, not ordinary prompt text'
            Clear-AgentInput -App $app -PaneSessionId $agentSession.PaneSessionId | Out-Null
            Send-AgentPrompt -App $app -PaneSessionId $agentSession.PaneSessionId -Text '/allow_all' | Out-Null
            $policyError = Get-WtaLocalizedTextRegex -Key 'system.provider_command_blocked_by_policy'
            $policyError = $policyError.Replace(
                [regex]::Escape('%{command}'),
                [regex]::Escape('/allow_all'))
            Assert-AgentPaneText -App $app -PaneSessionId $agentSession.PaneSessionId `
                -Pattern $policyError -TimeoutSec 15
            $blockedLog = Wait-Until -TimeoutSec 15 -IntervalSec 0.5 `
                -Because 'the helper to record that policy suppressed the provider command before ACP' -Condition {
                    $log = Get-ItLogText -App $app -Name 'wta-main_helper-*.log' -SinceStart
                    if ($log -match 'AllowYoloMode blocked provider command /allow_all') { $log }
                }
            $blockedLog | Should -Not -Match 'sending Agent command verbatim' `
                -Because 'a policy-blocked provider command must not cross the ACP prompt boundary'
            (Test-AgentNativeYoloUpdate -App $app -AcpSessionId $agentSession.AcpSessionId -Enabled $true) |
                Should -BeFalse -Because 'the blocked provider command must not re-enable native Yolo'
        }
        finally {
            if ($app) { Stop-Terminal -App $app }
            if ($prior) { Restore-WtAgentPolicy -State $prior }
        }
    }
}