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
    $script:geminiStatus = if ($script:geminiInstalled) { 'probe-failed' } else { 'not-installed' }
    try {
        $resolvedApp = Resolve-ItApp -Package $script:Package -ErrorAction Stop
        $script:Ready = Test-WinAppAvailable
        if ($script:copilotStatus -ne 'not-installed') {
            $script:copilotStatus = Get-AgentAcpStatus -App $resolvedApp -AgentCommand 'copilot --acp --stdio'
        }
        if ($script:geminiStatus -ne 'not-installed') {
            $script:geminiStatus = Get-AgentAcpStatus -App $resolvedApp -AgentCommand 'gemini --acp'
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
        GeminiStatus = $script:geminiStatus
    })
}

Describe 'Feature custom-provider permission baseline' -ForEach $script:PackageCase -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        . (Join-Path $PSScriptRoot 'helpers\TestWindowKeyboardLayout.ps1')
        $script:app = $null
        $script:permissionKeyboardLayout = $null
        $script:permissionLaunchStarted = $null
        $artifactRoot = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:permissionEvidence = Join-Path ([IO.Path]::GetFullPath($artifactRoot)) "permission-shortcuts\$([guid]::NewGuid().ToString('N'))"
        New-Item -ItemType Directory -Force -Path $script:permissionEvidence | Out-Null
        $script:fixtureLog = Join-Path $script:permissionEvidence 'fixture.log'
        $script:permissionTarget = Resolve-ItApp -Package $Package
        $target = $script:permissionTarget
        if ($env:ITE2E_EXPECTED_WTA_SHA256) {
            (Get-FileHash -LiteralPath $target.WtaPath -Algorithm SHA256).Hash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
        }
        @(Get-WtProcessesForApp -App $target).Count | Should -Be 0 -Because 'physical permission checks must not replace a user-owned window'
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpPermissionAgent.ps1')).Path
        $fixtureInvocation = "& '$($fixture.Replace("'", "''"))' -LogPath '$($script:fixtureLog.Replace("'", "''"))'"
        $encodedInvocation = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($fixtureInvocation))
        $fixtureCommand = "pwsh -NoProfile -EncodedCommand $encodedInvocation"
        $script:permissionLaunchStarted = Get-Date
        $script:app = Start-Terminal -Package $Package -PassFre $true -Settings @{
            acpAgent = 'custom:yolo-permission-fixture'
            acpCustomCommand = $fixtureCommand
            'agentPane.yoloMode' = $true
        }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue
        $script:agentPane = (Wait-NewAgentPaneSession -App $script:app -TimeoutSec 30).PaneSessionId
        $script:permissionKeyboardLayout = Enable-TestWindowEnglishKeyboardLayout -App $script:app
    }
    AfterAll {
        try {
            if ($script:permissionKeyboardLayout) {
                Restore-TestWindowKeyboardLayout -App $script:app -Context $script:permissionKeyboardLayout
            }
        }
        finally {
            if ($script:app) {
                Stop-Terminal -App $script:app
            }
            elseif ($script:permissionLaunchStarted) {
                foreach ($process in @(Get-WtProcessesForApp -App $script:permissionTarget)) {
                    if ($process.Path -ine $script:permissionTarget.WindowsTerminal -or
                        $process.StartTime -lt $script:permissionLaunchStarted) {
                        throw 'Failed-startup cleanup cannot identify a test-owned process.'
                    }
                    $recovery = $script:permissionTarget.PSObject.Copy()
                    $recovery.Pid = $process.Id
                    $recovery | Add-Member -NotePropertyName Launched -NotePropertyValue $true -Force
                    Stop-Terminal -App $recovery -RestoreSettings $false
                }
                Restore-WtConfig -App $script:permissionTarget
            }
        }
    }

    It 'Permission UI works' -Tag 'PermissionShortcutCompatibility' {
        Assert-Setting -App $script:app -Key 'agentPane.yoloMode' -Value $true
        Invoke-WtCli -App $script:app -Arguments @('focus-pane', '-t', $script:agentPane) | Out-Null
        if (-not (Set-WtWindowForeground -App $script:app)) {
            Set-ItResult -Skipped -Because 'physical permission shortcuts require an unlocked foreground desktop'
            return
        }
        foreach ($chord in @(
            @{ Name = 'Y'; Vk = 0x59; Ctrl = $false; Outcome = 'allow-once' }
            @{ Name = 'Enter'; Vk = 0x0D; Ctrl = $false; Outcome = 'allow-once' }
            @{ Name = 'CtrlY'; Vk = 0x59; Ctrl = $true; Outcome = 'allow-once' }
            @{ Name = 'N'; Vk = 0x4E; Ctrl = $false; Outcome = 'reject-once' }
        )) {
            $marker = 'PERM' + [guid]::NewGuid().ToString('N').Substring(0, 12)
            Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text $marker | Out-Null
            (Wait-AgentPermission -App $script:app -TimeoutSec 30) |
                Should -BeTrue -Because 'the provider request must remain pending until an explicit shortcut'
            $before = Get-Content -LiteralPath $script:fixtureLog -Raw
            $before | Should -Match "permission-requested\|$marker"
            $before | Should -Not -Match "permission-resolved\|.*\|$marker"
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:permissionEvidence "$($chord.Name)-before.png") | Out-Null
            Send-WtWindowKey -App $script:app -Vk $chord.Vk -Ctrl:$chord.Ctrl -RequireForeground | Out-Null
            $script:permissionExpected = "permission-resolved\|$($chord.Outcome)\|$marker"
            (Test-Until -TimeoutSec 20 -IntervalSec 0.5 -Condition {
                (Get-Content -LiteralPath $script:fixtureLog -Raw) -match $script:permissionExpected
            }) | Should -BeTrue -Because "$($chord.Name) must retain its existing permission outcome"
            Assert-AgentPaneText -App $script:app -PaneSessionId $script:agentPane `
                -Pattern "PERMISSION_RESULT_${marker}_$($chord.Outcome)" -TimeoutSec 20
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:permissionEvidence "$($chord.Name)-after.png") | Out-Null
        }
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

Describe 'Feature default-provider Yolo through /agent' -ForEach $script:PackageCase -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    }

    It '/agent Yolo stays scoped to the default provider' -Skip:($script:copilotBlocked -or $GeminiStatus -ne 'ready') {
        $app = Start-Terminal -Package $Package -PassFre $true -Settings @{
            acpAgent = 'copilot'
            'agentPane.yoloMode' = $true
        }
        try {
            Open-AgentPane -App $app | Out-Null
            Wait-AgentReady -App $app -TimeoutSec 90 | Should -BeTrue
            $shellPane = Get-ActivePane -App $app
            $defaultSession = Wait-NewAgentPaneSession -App $app -OwnerPaneSessionId $shellPane.session_id -TimeoutSec 30
            (Test-Until -TimeoutSec 30 -IntervalSec 0.5 -Condition {
                Test-AgentNativeYoloUpdate -App $app -AcpSessionId $defaultSession.AcpSessionId -Enabled $true
            }) | Should -BeTrue -Because 'the Settings default provider must inherit the persisted Yolo preference'

            Initialize-LogOffsets -App $app | Out-Null
            Send-AgentPrompt -App $app -PaneSessionId $defaultSession.PaneSessionId -Text '/agent gemini' | Out-Null
            $geminiSession = Wait-Until -TimeoutSec 90 -IntervalSec 0.5 -Because 'Gemini /agent session' -Condition {
                Get-AgentPaneSessions -App $app |
                    Where-Object { $_.AcpSessionId -and $_.AcpSessionId -ne $defaultSession.AcpSessionId } |
                    Select-Object -Last 1
            }
            Wait-AgentReady -App $app -PaneSessionId $geminiSession.PaneSessionId -TimeoutSec 90 |
                Should -BeTrue
            (Test-Until -TimeoutSec 30 -IntervalSec 0.5 -Condition {
                Test-AgentNativeYoloUpdate -App $app -AcpSessionId $geminiSession.AcpSessionId -Enabled $false
            }) | Should -BeTrue -Because 'a non-default /agent provider must be actively reconciled to Yolo off'
            (Test-AgentNativeYoloUpdate -App $app -AcpSessionId $geminiSession.AcpSessionId -Enabled $true) |
                Should -BeFalse

            Send-AgentPrompt -App $app -PaneSessionId $geminiSession.PaneSessionId -Text '/agent copilot' | Out-Null
            $restoredSession = Wait-Until -TimeoutSec 90 -IntervalSec 0.5 -Because 'restored default Copilot session' -Condition {
                Get-AgentPaneSessions -App $app |
                    Where-Object {
                        $_.AcpSessionId -and
                        $_.AcpSessionId -notin @($defaultSession.AcpSessionId, $geminiSession.AcpSessionId)
                    } |
                    Select-Object -Last 1
            }
            Wait-AgentReady -App $app -PaneSessionId $restoredSession.PaneSessionId -TimeoutSec 90 |
                Should -BeTrue
            (Test-Until -TimeoutSec 30 -IntervalSec 0.5 -Condition {
                Test-AgentNativeYoloUpdate -App $app -AcpSessionId $restoredSession.AcpSessionId -Enabled $true
            }) | Should -BeTrue -Because 'switching back to the default provider must restore the persisted preference'

            Assert-Setting -App $app -Key 'acpAgent' -Value 'copilot'
            Assert-Setting -App $app -Key 'agentPane.yoloMode' -Value $true
        }
        finally {
            if ($app) { Stop-Terminal -App $app }
        }
    }

    It 'Settings provider change never enables Yolo on the outgoing provider' -Skip:($script:copilotBlocked -or $GeminiStatus -ne 'ready') {
        $app = Start-Terminal -Package $Package -PassFre $true -Settings @{
            acpAgent = 'copilot'
            'agentPane.yoloMode' = $false
        }
        try {
            Open-AgentPane -App $app | Out-Null
            Wait-AgentReady -App $app -TimeoutSec 90 | Should -BeTrue
            $shellPane = Get-ActivePane -App $app
            $copilotSession = Wait-NewAgentPaneSession -App $app -OwnerPaneSessionId $shellPane.session_id -TimeoutSec 30
            (Test-Until -TimeoutSec 30 -IntervalSec 0.5 -Condition {
                Test-AgentNativeYoloUpdate -App $app -AcpSessionId $copilotSession.AcpSessionId -Enabled $false
            }) | Should -BeTrue

            Initialize-LogOffsets -App $app | Out-Null
            $settings = Get-WtSettingsObject -App $app
            $settings.acpAgent = 'gemini'
            $settings.'agentPane.yoloMode' = $true
            $settings | ConvertTo-Json -Depth 64 |
                Set-Content -LiteralPath $app.SettingsPath -Encoding utf8

            $geminiSession = Wait-Until -TimeoutSec 90 -IntervalSec 0.5 -Because 'Settings-rebound Gemini session' -Condition {
                Get-AgentPaneSessions -App $app |
                    Where-Object { $_.AcpSessionId -and $_.AcpSessionId -ne $copilotSession.AcpSessionId } |
                    Select-Object -Last 1
            }
            Wait-AgentReady -App $app -PaneSessionId $geminiSession.PaneSessionId -TimeoutSec 90 |
                Should -BeTrue -Because 'the settings change must finish rebinding before the negative assertion'

            (Test-AgentNativeYoloUpdate -App $app -AcpSessionId $copilotSession.AcpSessionId -Enabled $true) |
                Should -BeFalse -Because 'the future provider preference must not be applied to the outgoing provider'
        }
        finally {
            if ($app) { Stop-Terminal -App $app }
        }
    }
}

Describe 'Feature default-provider Yolo across profile bindings' -ForEach $script:PackageCase -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    }

    It 'Profile automatic approval stays scoped to the Settings default provider' -Skip:($script:copilotBlocked -or -not $GeminiInstalled) {
        $profileGuid = '{' + [guid]::NewGuid().ToString() + '}'
        $profiles = [pscustomobject][ordered]@{
            defaults = [pscustomobject]@{}
            list     = @(
                [pscustomobject][ordered]@{
                    guid             = $profileGuid
                    name             = 'Automatic approval profile scope'
                    commandline      = 'pwsh.exe'
                    agentPaneBackend = 'host:copilot'
                }
            )
        }
        $app = Start-Terminal -Package $Package -PassFre $true -Settings @{
            acpAgent             = 'gemini'
            'agentPane.yoloMode' = $true
            defaultProfile       = $profileGuid
            profiles             = $profiles
        }
        try {
            Open-AgentPane -App $app | Out-Null
            Wait-AgentReady -App $app -TimeoutSec 90 | Should -BeTrue
            $shellPane = Get-ActivePane -App $app
            $profileSession = Wait-NewAgentPaneSession -App $app -OwnerPaneSessionId $shellPane.session_id -TimeoutSec 30

            (Test-Until -TimeoutSec 30 -IntervalSec 0.5 -Condition {
                Test-AgentNativeYoloUpdate -App $app -AcpSessionId $profileSession.AcpSessionId -Enabled $false
            }) | Should -BeTrue -Because 'a non-default profile backend must start from the automatic Off baseline'
            (Test-AgentNativeYoloUpdate -App $app -AcpSessionId $profileSession.AcpSessionId -Enabled $true) |
                Should -BeFalse -Because 'the Settings default preference must not automatically enable the profile provider'
        }
        finally {
            if ($app) { Stop-Terminal -App $app }
        }
    }
}

Describe 'Feature Settings automatic approval availability' -ForEach $script:PackageCase -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    }

    It 'Settings hides unsupported automatic approval and forces it off' {
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

            Test-UiElementExists -App $app -Selector 'AgentPaneYoloModeToggle' -TimeoutSec 1 |
                Should -BeFalse -Because 'unsupported automatic approval must be hidden instead of explained by a disabled row'
            Test-UiElementExists -App $app -Selector 'OpenCodeYoloCompatibilityInfoBar' -TimeoutSec 1 |
                Should -BeFalse -Because 'the hidden setting no longer needs a provider-unavailable message'
            Test-UiElementExists -App $app -Selector 'GeminiYoloCompatibilityInfoBar' -TimeoutSec 1 |
                Should -BeFalse -Because 'only the selected default provider should have a compatibility notice'

            Invoke-UiElement -App $app -Selector 'SaveButton' | Out-Null
            (Test-Until -TimeoutSec 15 -IntervalSec 0.5 -Condition {
                (Get-WtSettingsObject -App $app).'agentPane.yoloMode' -eq $false
            }) | Should -BeTrue -Because 'saving OpenCode as default must persist Yolo off'
        }
        finally {
            if ($app) { Stop-Terminal -App $app }
        }
    }

    It 'Settings explains Gemini automatic approval restrictions' {
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
            $message = Get-WtReswTextRegex -Key 'AIAgents_YoloGeminiInfo.Message'
            (Get-UiTree -App $app -Selector 'GeminiYoloCompatibilityInfoBar' -Depth 4) |
                Should -Match $message -Because 'the informational notice must describe Gemini workspace trust'
            Test-UiElementExists -App $app -Selector 'AgentPaneYoloModeToggle' -TimeoutSec 8 |
                Should -BeTrue -Because 'Gemini automatic approval remains visible'
            Test-UiElementEnabled -App $app -Selector 'AgentPaneYoloModeToggle' |
                Should -BeTrue -Because 'Gemini automatic approval remains available when its trust prerequisites are met'
        }
        finally {
            if ($app) { Stop-Terminal -App $app }
        }
    }
}

Describe 'Feature AllowAutomaticApproval policy' -ForEach $script:PackageCase -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    }

    It 'AllowAutomaticApproval hides automatic approval and turns it off' -Skip:(-not $script:policyReady) {
        $prior = Set-WtAgentPolicy -Policy @{ AllowAutomaticApproval = 'Allowed' }
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
            Set-WtAgentPolicy -Policy @{ AllowAutomaticApproval = 'Blocked' } | Out-Null
            (Test-Until -TimeoutSec 15 -IntervalSec 0.5 -Condition {
                (Get-WtSettingsObject -App $app).'agentPane.yoloMode' -eq $false
            }) | Should -BeTrue -Because 'a policy block must clear the persisted Yolo preference'
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
                    if ($log -match 'AllowAutomaticApproval blocked provider command /allow_all') { $log }
                }
            $blockedLog | Should -Not -Match 'sending Agent command verbatim' `
                -Because 'a policy-blocked provider command must not cross the ACP prompt boundary'
            (Test-AgentNativeYoloUpdate -App $app -AcpSessionId $agentSession.AcpSessionId -Enabled $true) |
                Should -BeFalse -Because 'the blocked provider command must not re-enable native Yolo'

            Open-WtSettings -App $app -TimeoutSec 20 | Out-Null
            Invoke-SettingsNav -App $app -NavItem 'AIAgentsNavItem' | Out-Null
            Test-UiElementExists -App $app -Selector 'AgentPaneYoloModeToggle' -TimeoutSec 1 |
                Should -BeFalse -Because 'policy-blocked automatic approval must be hidden'
            Test-UiElementExists -App $app -Selector 'GeminiYoloCompatibilityInfoBar' -TimeoutSec 1 |
                Should -BeFalse -Because 'a hidden policy-blocked setting has no provider notice'
        }
        finally {
            if ($app) { Stop-Terminal -App $app }
            if ($prior) { Restore-WtAgentPolicy -State $prior }
        }
    }
}