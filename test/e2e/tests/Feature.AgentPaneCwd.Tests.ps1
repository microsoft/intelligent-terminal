#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Standalone agent-pane CWD regression: a tab created in a disposable workspace
# must propagate that workspace through Terminal -> helper -> master -> ACP
# session/new, including a later /new. The stdio fixture is deterministic and
# never submits a model prompt or consumes provider quota.

BeforeDiscovery {
    $fixturePath = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpInteractionAgent.ps1')).Path
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and
        (Get-Command pwsh -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue) -and
        ($fixturePath -notmatch '\s') -and
        ($env:TEMP -notmatch '\s')
    )
}

Describe 'Feature: agent pane working directory' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpInteractionAgent.ps1')).Path
    }

    BeforeEach {
        $script:root = Join-Path ([System.IO.Path]::GetTempPath()) ("ite2e-agent-cwd-{0}" -f [guid]::NewGuid().ToString('N'))
        $script:workspace = Join-Path $script:root 'workspace'
        New-Item -ItemType Directory -Path $script:workspace -Force | Out-Null
        $script:requestLog = Join-Path $env:TEMP ("ite2e-agent-cwd-{0}.log" -f [guid]::NewGuid().ToString('N'))
        $command = "pwsh -NoProfile -File $script:fixture -LogPath $script:requestLog"
        $package = Get-ItTestPackage
        $targetApp = Resolve-ItApp -Package $package
        try {
            $script:app = Start-Terminal -Package $package -PassFre $true -Settings @{
                acpAgent = 'custom:cwd-fixture'
                acpCustomCommand = $command
                acpModel = ''
            }
        }
        catch {
            Stop-AppInstances -App $targetApp
            Restore-WtConfig -App $targetApp
            throw
        }

        Wait-NewAgentPaneSession -App $script:app -TimeoutSec 30 | Out-Null
        # Exclude every run-local pane that exists before the workspace tab.
        # OwnerPaneSessionId uses a VT probe whose tab identity is a stable GUID,
        # while active-pane/new-tab expose numeric protocol indices; creation
        # order plus explicit exclusion avoids conflating those identity domains.
        $existingAgentPanes = @(
            Get-AgentPaneSessions -App $script:app |
                ForEach-Object { $_.PaneSessionId }
        )

        $workspaceTab = New-WtTab -App $script:app -Cwd $script:workspace -Title 'agent-cwd-workspace'
        Set-WtPaneFocus -App $script:app -SessionId $workspaceTab.session_id
        Open-AgentPane -App $script:app | Out-Null
        $script:agentSession = Wait-NewAgentPaneSession -App $script:app `
            -ExcludePaneSessionId $existingAgentPanes -TimeoutSec 30
        Wait-AgentReady -App $script:app -PaneSessionId $script:agentSession.PaneSessionId -TimeoutSec 60 |
            Should -BeTrue -Because 'the deterministic cwd fixture must connect in the workspace tab'
    }

    AfterEach {
        if ($script:app) {
            Stop-Terminal -App $script:app
            $script:app = $null
        }
        if ($script:requestLog -and (Test-Path -LiteralPath $script:requestLog)) {
            Remove-Item -LiteralPath $script:requestLog -Force
        }
        if ($script:root -and (Test-Path -LiteralPath $script:root)) {
            Remove-Item -LiteralPath $script:root -Recurse -Force
        }
    }

    It 'ACP sessions inherit the source workspace across /new' {
        $readCwd = {
            param([string]$SessionId)

            $line = Wait-Until -TimeoutSec 20 -IntervalSec 0.2 -Because "session/new cwd for $SessionId" -Condition {
                Get-Content -LiteralPath $script:requestLog -ErrorAction SilentlyContinue |
                    Where-Object { $_ -match "\|session/new-cwd\|$([regex]::Escape($SessionId))\|" } |
                    Select-Object -Last 1
            }
            ($line -replace '^.*\|session/new-cwd\|[^|]+\|', '') | ConvertFrom-Json
        }

        $expected = [System.IO.Path]::GetFullPath($script:workspace)
        (& $readCwd $script:agentSession.AcpSessionId) |
            Should -Be $expected -Because 'initial ACP session/new must use the source terminal workspace'

        $priorSessionId = $script:agentSession.AcpSessionId
        Invoke-AgentMenuItem -App $script:app -PaneSessionId $script:agentSession.PaneSessionId -Name '/new'
        $replacement = Wait-Until -TimeoutSec 30 -IntervalSec 0.5 -Because 'a replacement ACP session after /new' -Condition {
            $session = Get-AgentPaneSession -App $script:app -PaneSessionId $script:agentSession.PaneSessionId
            if ($session -and $session.AcpSessionId -ne $priorSessionId) { $session }
        }

        (& $readCwd $replacement.AcpSessionId) |
            Should -Be $expected -Because '/new must reuse the normalized source workspace, not WTA_SOURCE_CWD or helper current_dir'
        Get-Content -LiteralPath $script:requestLog |
            Should -Not -Match '\|session/prompt\|' -Because 'cwd propagation is a zero-token protocol test'
    }
}