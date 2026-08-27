#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PR #505: provider-native ACP Yolo modes. The deterministic fixture proves WTA never
# selects a provider permission option; real providers prove native mode acknowledgement
# and a normal-user-cost chat/tool workflow against the deployed package.

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
        $script:workRoot = Join-Path $env:TEMP ('ite2e-yolo-tool-' + [guid]::NewGuid().ToString('N'))
    }
    AfterAll {
        Remove-Item -LiteralPath $script:workRoot -Recurse -Force -ErrorAction SilentlyContinue
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

    It 'Yolo completes a real tool task' -Skip:(-not $script:copilotReady) {
        New-Item -ItemType Directory -Path $script:workRoot -Force | Out-Null
        $marker = 'YOLO_TOOL_' + [guid]::NewGuid().ToString('N')
        $file = Join-Path $script:workRoot 'result.txt'
        $app = Start-Terminal -Package Dev -PassFre $true -Settings @{
            acpAgent = 'copilot'
            'agentPane.yoloMode' = $false
        }
        try {
            Open-AgentPane -App $app | Out-Null
            Wait-AgentReady -App $app -TimeoutSec 90 | Should -BeTrue
            $shellPane = Get-ActivePane -App $app
            $agentPane = (Wait-NewAgentPaneSession -App $app -OwnerPaneSessionId $shellPane.session_id -TimeoutSec 30).PaneSessionId
            Initialize-LogOffsets -App $app | Out-Null
            Send-AgentPrompt -App $app -PaneSessionId $agentPane -Text '/yolo on' | Out-Null
            Assert-Log -App $app -Name 'wta-main_helper-*.log' `
                -Pattern 'provider-native Yolo updated for live session.*enabled=true' -TimeoutSec 30

            $prompt = "Use your shell tool to create the exact file `"$file`" containing only `"$marker`". Read it back, then reply with only $marker."
            Send-AgentPrompt -App $app -PaneSessionId $agentPane -Text $prompt | Out-Null
            (Test-Until -TimeoutSec 150 -IntervalSec 1 -Condition {
                (Test-Path -LiteralPath $file) -and
                ((Get-Content -LiteralPath $file -Raw).Trim() -eq $marker)
            }) | Should -BeTrue -Because 'provider-native Yolo should allow the bounded tool task to finish'
            Assert-AgentPaneText -App $app -PaneSessionId $agentPane -Pattern ([regex]::Escape($marker)) -TimeoutSec 60

            Initialize-LogOffsets -App $app | Out-Null
            Send-AgentPrompt -App $app -PaneSessionId $agentPane -Text '/yolo off' | Out-Null
            Assert-Log -App $app -Name 'wta-main_helper-*.log' `
                -Pattern 'provider-native Yolo updated for live session.*enabled=false' -TimeoutSec 30
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
        if ((Get-AgentCliStatus -Agent opencode -TimeoutSec 60) -ne 'authed') {
            Set-ItResult -Skipped -Because 'OpenCode cannot answer through its configured provider'
            return
        }
        $app = Start-Terminal -Package Dev -PassFre $true -Settings @{ acpAgent = 'opencode' }
        try {
            Open-AgentPane -App $app | Out-Null
            Wait-AgentReady -App $app -TimeoutSec 90 | Should -BeTrue
            $shellPane = Get-ActivePane -App $app
            $agentPane = (Wait-NewAgentPaneSession -App $app -OwnerPaneSessionId $shellPane.session_id -TimeoutSec 30).PaneSessionId
            Send-AgentPrompt -App $app -PaneSessionId $agentPane -Text '/yolo on' | Out-Null
            Assert-AgentPaneText -App $app -PaneSessionId $agentPane `
                -Pattern '(?i)/yolo on: opencode does not support ACP session Yolo mode' -TimeoutSec 30
            Send-AgentPrompt -App $app -PaneSessionId $agentPane -Text 'What is 8 plus 9? Reply with only the number.' | Out-Null
            Assert-AgentPaneText -App $app -PaneSessionId $agentPane -Pattern '\b17\b' -TimeoutSec 150
        }
        finally {
            if ($app) { Stop-Terminal -App $app }
        }
    }
}

Describe 'Feature provider-native Yolo matrix' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:providerWorkRoot = Join-Path $env:TEMP ('ite2e-yolo-providers-' + [guid]::NewGuid().ToString('N'))
        $script:runProviderYolo = {
            param(
                [Parameter(Mandatory)][string]$Agent,
                [int]$TimeoutSec = 240
            )

            if (-not (Get-Command $Agent -ErrorAction SilentlyContinue)) {
                Set-ItResult -Skipped -Because "$Agent is not installed"
                return
            }

            $providerRoot = Join-Path $script:providerWorkRoot $Agent
            New-Item -ItemType Directory -Path $providerRoot -Force | Out-Null
            $marker = "YOLO_${Agent}_" + [guid]::NewGuid().ToString('N')
            $file = Join-Path $providerRoot 'result.txt'
            $settings = @{
                acpAgent = $Agent
                'agentPane.yoloMode' = $false
            }
            $geminiTrustPath = $null
            $geminiTrustBackup = $null
            $geminiTrustNewMarker = $null
            $app = $null
            try {
                if ($Agent -eq 'gemini') {
                    $geminiTrustPath = $env:GEMINI_CLI_TRUSTED_FOLDERS_PATH
                    if ([string]::IsNullOrWhiteSpace($geminiTrustPath)) {
                        $geminiTrustPath = Join-Path ([Environment]::GetFolderPath('UserProfile')) '.gemini\trustedFolders.json'
                    }
                    $geminiTrustBackup = "$geminiTrustPath.ite2ebak"
                    $geminiTrustNewMarker = "$geminiTrustPath.ite2enew"
                    if (Test-Path -LiteralPath $geminiTrustBackup) {
                        Copy-Item -LiteralPath $geminiTrustBackup -Destination $geminiTrustPath -Force
                        Remove-Item -LiteralPath $geminiTrustBackup -Force
                    }
                    if (Test-Path -LiteralPath $geminiTrustNewMarker) {
                        Remove-Item -LiteralPath $geminiTrustPath -Force -ErrorAction SilentlyContinue
                        Remove-Item -LiteralPath $geminiTrustNewMarker -Force
                    }

                    New-Item -ItemType Directory -Path (Split-Path -Parent $geminiTrustPath) -Force | Out-Null
                    if (Test-Path -LiteralPath $geminiTrustPath) {
                        Copy-Item -LiteralPath $geminiTrustPath -Destination $geminiTrustBackup -Force
                        $trustedFolders = Get-Content -LiteralPath $geminiTrustPath -Raw | ConvertFrom-Json -AsHashtable
                    }
                    else {
                        New-Item -ItemType File -Path $geminiTrustNewMarker -Force | Out-Null
                        $trustedFolders = [ordered]@{}
                    }
                    $trustedFolders[$providerRoot] = 'TRUST_FOLDER'
                    $trustedFolders | ConvertTo-Json -Depth 16 | Set-Content -LiteralPath $geminiTrustPath -Encoding utf8
                }
                $modelVariable = 'ITE2E_{0}_MODEL' -f $Agent.ToUpperInvariant()
                $model = [Environment]::GetEnvironmentVariable($modelVariable)
                if (-not [string]::IsNullOrWhiteSpace($model)) {
                    $settings.acpModel = $model.Trim()
                }
                $app = Start-Terminal -Package Dev -PassFre $true -Settings $settings
                if ($Agent -eq 'gemini') {
                    $workspaceTab = New-WtTab -App $app -Cwd $providerRoot
                    Set-WtPaneFocus -App $app -SessionId $workspaceTab.session_id | Out-Null
                }
                Open-AgentPane -App $app | Out-Null
                Wait-AgentReady -App $app -TimeoutSec $TimeoutSec |
                    Should -BeTrue -Because "$Agent should reach a connected ACP session"
                $shellPane = Get-ActivePane -App $app
                $agentPane = (Wait-NewAgentPaneSession -App $app -OwnerPaneSessionId $shellPane.session_id -TimeoutSec 45).PaneSessionId

                Initialize-LogOffsets -App $app | Out-Null
                Send-AgentPrompt -App $app -PaneSessionId $agentPane -Text '/yolo on' | Out-Null
                Assert-Log -App $app -Name 'wta-main_helper-*.log' `
                    -Pattern 'provider-native Yolo updated for live session.*enabled=true' -TimeoutSec 45

                $prompt = "Use your shell tool to create the exact file `"$file`" containing only `"$marker`". Read it back, then reply with only $marker."
                Send-AgentPrompt -App $app -PaneSessionId $agentPane -Text $prompt | Out-Null
                (Test-Until -TimeoutSec $TimeoutSec -IntervalSec 1 -Condition {
                    (Test-Path -LiteralPath $file) -and
                    ((Get-Content -LiteralPath $file -Raw).Trim() -eq $marker)
                }) | Should -BeTrue -Because "$Agent provider-native Yolo should complete the bounded tool task"
                Assert-AgentPaneText -App $app -PaneSessionId $agentPane `
                    -Pattern ([regex]::Escape($marker)) -TimeoutSec 90

                Initialize-LogOffsets -App $app | Out-Null
                Send-AgentPrompt -App $app -PaneSessionId $agentPane -Text '/yolo off' | Out-Null
                Assert-Log -App $app -Name 'wta-main_helper-*.log' `
                    -Pattern 'provider-native Yolo updated for live session.*enabled=false' -TimeoutSec 45
            }
            finally {
                if ($app) { Stop-Terminal -App $app }
                if ($geminiTrustBackup -and (Test-Path -LiteralPath $geminiTrustBackup)) {
                    Copy-Item -LiteralPath $geminiTrustBackup -Destination $geminiTrustPath -Force
                    Remove-Item -LiteralPath $geminiTrustBackup -Force
                }
                elseif ($geminiTrustNewMarker -and (Test-Path -LiteralPath $geminiTrustNewMarker)) {
                    Remove-Item -LiteralPath $geminiTrustPath -Force -ErrorAction SilentlyContinue
                    Remove-Item -LiteralPath $geminiTrustNewMarker -Force
                }
            }
        }
    }
    AfterAll {
        Remove-Item -LiteralPath $script:providerWorkRoot -Recurse -Force -ErrorAction SilentlyContinue
    }

    It 'Claude provider-native Yolo completes a real tool task' {
        & $script:runProviderYolo -Agent 'claude'
    }

    It 'Codex provider-native Yolo completes a real tool task' {
        & $script:runProviderYolo -Agent 'codex'
    }

    It 'Gemini provider-native Yolo completes a real tool task' {
        & $script:runProviderYolo -Agent 'gemini'
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