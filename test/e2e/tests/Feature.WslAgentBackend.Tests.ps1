#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PR #481: a profile can pin its agent pane to a supported ACP agent installed
# inside that profile's WSL distro. The routing assertion is deterministic once
# a native WSL agent is present; the chat assertion additionally requires auth.

BeforeDiscovery {
    $devPkg = Get-AppxPackage | Where-Object { $_.PackageFamilyName -like 'IntelligentTerminal_*' }
    $script:Ready = [bool](
        $devPkg -and
        (Get-Command winapp -ErrorAction SilentlyContinue) -and
        (Get-Command wsl.exe -ErrorAction SilentlyContinue)
    )
}

Describe 'Feature profile-scoped WSL agent backend' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force

        $script:app = $null
        $script:skipReason = $null
        $script:oldAgentPaneId = $null
        $script:profileGuid = '{e2e48100-4810-4810-9810-000000000001}'
        $script:profileName = 'ITE2E PR481 WSL Agent'

        $distroProbe = Invoke-Native -FilePath 'wsl.exe' -Arguments @(
            '-e', 'sh', '-lc', 'printf "%s" "${WSL_DISTRO_NAME:-}"'
        ) -TimeoutSec 45
        $script:distro = $distroProbe.StdOut.Trim()
        if ($distroProbe.ExitCode -ne 0 -or -not $script:distro) {
            $script:skipReason = 'no runnable default WSL distro is available'
            return
        }
        if ($script:distro -match '["\r\n]') {
            $script:skipReason = 'the default WSL distro name cannot be represented safely in a test profile'
            return
        }

        # Match the product's source probe: reject Windows executables leaked
        # through WSL interop, and require npx for adapter-backed agents.
        $agentProbeScript = @'
for id in copilot gemini opencode claude codex; do
    path="$(command -v "$id" 2>/dev/null || true)"
    [ -n "$path" ] || continue
    lower="$(printf '%s' "$path" | tr '[:upper:]' '[:lower:]')"
    case "$lower" in
        /mnt/*|*.exe|*.cmd|*.bat) continue ;;
    esac
    case "$id" in
        claude|codex)
            npx_path="$(command -v npx 2>/dev/null || true)"
            npx_lower="$(printf '%s' "$npx_path" | tr '[:upper:]' '[:lower:]')"
            case "$npx_lower" in
                ''|/mnt/*|*.exe|*.cmd|*.bat) continue ;;
            esac
            ;;
    esac
    printf '%s\n' "$id"
    break
done
'@
        $agentProbe = Invoke-Native -FilePath 'wsl.exe' -Arguments @(
            '-d', $script:distro, '--', 'bash', '-lc', $agentProbeScript
        ) -TimeoutSec 45
        $knownAgents = @('copilot', 'gemini', 'opencode', 'claude', 'codex')
        $script:agent = @(
            $agentProbe.StdOut -split '\r?\n' |
                ForEach-Object { $_.Trim() } |
                Where-Object { $_ -in $knownAgents }
        ) | Select-Object -First 1
        if ($agentProbe.ExitCode -ne 0 -or -not $script:agent) {
            $script:skipReason = "$($script:distro) has no supported native Linux ACP agent"
            return
        }

        $unconfiguredProfile = [pscustomobject][ordered]@{
            guid        = $script:profileGuid
            name        = $script:profileName
            commandline = "wsl.exe -d `"$($script:distro)`""
        }
        $unconfiguredProfiles = [pscustomobject][ordered]@{
            defaults = [pscustomobject]@{}
            list     = @($unconfiguredProfile)
        }
        $script:backend = "wsl:$($script:distro):$($script:agent)"
        $script:app = Start-Terminal -Package Dev -PassFre $true -Settings @{
            acpAgent       = $script:agent
            defaultProfile = $script:profileGuid
            profiles       = $unconfiguredProfiles
        }
        $oldAgentPane = Get-AgentPaneSession -App $script:app
        if ($oldAgentPane) {
            $script:oldAgentPaneId = $oldAgentPane.PaneSessionId
        }

        # Apply the profile backend only after Start-Terminal has captured log
        # offsets. This exercises settings hot reload and keeps source-routing
        # assertions scoped to this run.
        $configuredProfile = [pscustomobject][ordered]@{
            guid             = $script:profileGuid
            name             = $script:profileName
            commandline      = "wsl.exe -d `"$($script:distro)`""
            agentPaneBackend = $script:backend
        }
        $configuredProfiles = [pscustomobject][ordered]@{
            defaults = [pscustomobject]@{}
            list     = @($configuredProfile)
        }
        Set-WtSetting -App $script:app -Key 'profiles' -Value $configuredProfiles | Out-Null
        Open-AgentPane -App $script:app | Out-Null
    }
    AfterAll {
        if ($script:app) {
            Stop-Terminal -App $script:app
        }
    }

    It 'Hot reload routes the profile agent through its WSL distro without host fallback' {
        if ($script:skipReason) {
            Set-ItResult -Skipped -Because $script:skipReason
            return
        }

        $settings = Get-WtSettingsObject -App $script:app
        $profile = @($settings.profiles.list) |
            Where-Object { $_.guid -eq $script:profileGuid } |
            Select-Object -First 1
        $profile.agentPaneBackend | Should -Be $script:backend

        if ($script:oldAgentPaneId) {
            (Test-Until -TimeoutSec 30 -IntervalSec 0.5 -Condition {
                    -not (Get-AgentPaneSession -App $script:app -PaneSessionId $script:oldAgentPaneId)
                }) | Should -BeTrue -Because 'changing this profile backend must replace its old host helper'
        }

        $agentPattern = [regex]::Escape($script:agent)
        $sourcePattern = [regex]::Escape("wsl:$($script:distro)")
        Assert-Log -App $script:app -Name 'wta-main_master.log' -Pattern (
            'resolving agent CLI for helper.*requested_agent_id=Some\("' + $agentPattern +
            '"\).*resolved_agent_source=' + $sourcePattern
        ) -TimeoutSec 60
        Assert-Log -App $script:app -Name 'wta-main_master.log' -Pattern (
            'agent CLI spawned.*agent_source=' + $sourcePattern
        ) -TimeoutSec 60
    }

    It 'The profile-selected WSL agent connects and answers a chat round trip' {
        if ($script:skipReason) {
            Set-ItResult -Skipped -Because $script:skipReason
            return
        }
        if (-not (Wait-AgentReady -App $script:app -TimeoutSec 150)) {
            Set-ItResult -Skipped -Because "$($script:agent) is installed in $($script:distro) but is not authenticated or its ACP server did not connect"
            return
        }

        $oldAgentPaneIds = @($script:oldAgentPaneId) | Where-Object { $_ }
        $agentPane = Wait-NewAgentPaneSession -App $script:app `
            -ExcludePaneSessionId $oldAgentPaneIds -TimeoutSec 30
        Send-AgentPrompt -App $script:app -PaneSessionId $agentPane.PaneSessionId `
            -Text 'What is 480 plus 1? Reply with only the number.' | Out-Null
        Assert-AgentPaneText -App $script:app -PaneSessionId $agentPane.PaneSessionId `
            -Pattern '\b481\b' -TimeoutSec 150
    }
}
