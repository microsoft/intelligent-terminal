#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Real native profile -> Windows argv -> ConPTY -> installed native CLI.
# Version and parser-error cases submit no model work.

BeforeDiscovery {
    # This opt-in proof must not change an arbitrary Dev/Store installation.
    # Missing candidate inputs are an external prerequisite, not release credit.
    $script:NativeProfileProofReady = $env:ITE2E_PACKAGE -like 'IntelligentTerminal.Worktree.*_rd9vj3e6a2mbr' -and
        $env:ITE2E_SOURCE_COMMIT -and $env:ITE2E_EXPECTED_APP_SHA256 -and
        $env:ITE2E_EXPECTED_WTA_SHA256 -and $env:ITE2E_NATIVE_PROFILE_MODEL
}

Describe 'Feature: Native agent profile arguments' -Tag @('Feature', 'NativeAgentProfileArguments') -Skip:(-not $script:NativeProfileProofReady) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = $null
        $script:copilotHomeOwned = $false
        if ($env:ITE2E_PACKAGE -notlike 'IntelligentTerminal.Worktree.*_rd9vj3e6a2mbr') {
            throw 'Select an explicit inactive private Dev worktree package.'
        }
        $script:target = Resolve-ItApp -Package $env:ITE2E_PACKAGE
        Assert-WtPackageInactive -App $script:target
        $head = (& git -C (Join-Path $PSScriptRoot '..\..\..') rev-parse HEAD).Trim()
        if ($LASTEXITCODE -ne 0 -or $env:ITE2E_SOURCE_COMMIT -ne $head) {
            throw 'Supply the exact source commit from the tested build.'
        }
        foreach ($entry in @(
            @{ Path = (Join-Path $script:target.InstallLocation 'TerminalApp.dll'); Hash = $env:ITE2E_EXPECTED_APP_SHA256 },
            @{ Path = $script:target.WtaPath; Hash = $env:ITE2E_EXPECTED_WTA_SHA256 }
        )) {
            if (-not $entry.Hash -or (Get-FileHash -LiteralPath $entry.Path).Hash -ne $entry.Hash) {
                throw 'Installed payload does not match the candidate build receipt.'
            }
        }
        $script:automationHome = Join-Path $env:LOCALAPPDATA 'CopilotAutomation'
        # The host's prewarmed WTA helpers also need the automation home, not just
        # the native profile process. This changes only this test runner's environment.
        $script:previousCopilotHome = $env:COPILOT_HOME
        $env:COPILOT_HOME = $script:automationHome
        $script:copilotHomeOwned = $true
        $script:runId = [guid]::NewGuid().ToString('N')
        $script:provider = if ($env:ITE2E_NATIVE_PROFILE_PROVIDER) { $env:ITE2E_NATIVE_PROFILE_PROVIDER } else { 'copilot' }
        if ($script:provider -notin @('copilot', 'claude', 'codex', 'gemini', 'opencode', 'antigravity')) {
            throw 'Select a supported native profile provider.'
        }
        $script:model = $env:ITE2E_NATIVE_PROFILE_MODEL
        if (-not $script:model) { throw 'Supply a model supported by the installed native CLI.' }
        $script:cwd = Join-Path $env:ITE2E_ARTIFACT_ROOT "auto-profile-args-$script:runId"
        New-Item -ItemType Directory -Force -Path $script:cwd | Out-Null
        function Start-NativeArgumentProfile {
            param([string]$Arguments, [string]$PermissionMode = '')
            $profileId = '{b22d61a8-5509-4694-82fc-263214f54fb3}'
            $script:app = Start-Terminal -Package $env:ITE2E_PACKAGE -PassFre $true -Settings @{
                defaultProfile = $profileId
                firstWindowPreference = 'defaultProfile'; startupActions = ''
                windowingBehavior = 'useNew'; tabLayout = 'horizontal'
                agentSessionManagementEnabled = $false
                autoErrorDetectionEnabled = $false; autoFixEnabled = $false
                'warning.confirmOnClose' = 'never'
                profiles = @{ list = @(@{
                    guid = $profileId; name = "auto:test:native-profile-args:$script:runId"
                    'agentProfile.id' = $script:provider
                    'agentProfile.model' = $script:model
                    'agentProfile.arguments' = $Arguments
                    'agentProfile.permissionMode' = $PermissionMode
                    startingDirectory = $script:cwd; closeOnExit = 'never'
                    environment = @{
                        COPILOT_HOME = $script:automationHome
                        PATH = "$($script:target.InstallLocation);$env:PATH"
                        WT_WTCLI_PATH = $script:target.WtcliPath
                        WTCLI_PATH = $script:target.WtcliPath
                    }
                }) }
            }
            Get-ActivePane -App $script:app
        }
    }

    AfterEach {
        if ($script:app) {
            Stop-Terminal -App $script:app
            $script:app = $null
        }
    }

    AfterAll {
        if ($script:copilotHomeOwned) {
            $env:COPILOT_HOME = $script:previousCopilotHome
        }
    }

    It 'Native agent profile customizations reach the CLI' {
        $arguments = if ($script:provider -eq 'copilot') { '--yolo --version' } else { '--version' }
        $mode = if ($script:provider -eq 'copilot') { 'allow-all-tools' } else { '' }
        $pane = Start-NativeArgumentProfile -Arguments $arguments -PermissionMode $mode
        $pane.native_agent_provider_id | Should -Be $script:provider
        $pane.cwd | Should -Be $script:cwd
        Wait-Until -TimeoutSec 30 -Because 'the native CLI version reaches the profile buffer' -Condition {
            (Get-WtCapture -App $script:app -SessionId $pane.session_id) -match '\b\d+\.\d+\.\d+'
        } | Out-Null
        $text = Get-WtCapture -App $script:app -SessionId $pane.session_id
        $text | Should -Not -Match '0x80070057|Unsupported or conflicting native agent argument'
        Wait-WtPaneExit -App $script:app -SessionId $pane.session_id
        # NativePath regenerates PATH; the app can resolve a different installed CLI
        # than the runner. Assert its output and ConPTY's exit footer, not runner version.
        (Get-WtPaneStatus -App $script:app -SessionId $pane.session_id).state | Should -Be 'exited'
        (Get-WtCapture -App $script:app -SessionId $pane.session_id) |
            Should -Match '\[process exited with code 0 \(0x00000000\)\]'
    }

    It 'Native agent CLI owns invalid argument errors' {
        $unknown = "--it-native-$script:runId-%USERPROFILE%-quoted value"
        # One quoted argv value also exercises literal percent through ConPTY.
        # Do not append --version: Claude Code exits successfully for version
        # before reporting unknown options, unlike Copilot.
        $pane = Start-NativeArgumentProfile -Arguments ('"{0}"' -f $unknown)
        Wait-Until -TimeoutSec 30 -Because 'the native CLI reports the unknown argument and exits' -Condition {
            $capture = Get-WtCapture -App $script:app -SessionId $pane.session_id
            ($capture -replace '\r?\n', '').Contains($unknown) -and
                $capture -match '\[process exited with code (?!0\b)\d+'
        } | Out-Null
        $text = Get-WtCapture -App $script:app -SessionId $pane.session_id
        $text | Should -Not -Match '0x80070057|Unsupported or conflicting native agent argument'
    }
}
