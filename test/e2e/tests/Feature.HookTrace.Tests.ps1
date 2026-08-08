#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §10 (C190) — the native wtcli hook bridge must read hook
# JSON from stdin, preserve the source pane identity, strip prompt content, and
# publish an agent_event without relying on PowerShell hook scripts.

BeforeDiscovery { $script:Ready = [bool](Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) }

Describe 'Feature §10 native hook bridge' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'wtcli agent-hook publishes a pane-scoped, redacted agent event' {
        $paneId = (Get-ActivePane -App $script:app).session_id
        $agentSessionId = "native-hook-$([guid]::NewGuid())"
        $payload = @{
            session_id = $agentSessionId
            cwd = 'C:\native-hook-test'
            prompt = 'must-not-cross-the-hook-bridge'
        } | ConvertTo-Json -Compress
        $command = "'$payload' | wtcli.exe agent-hook --cli-source copilot --event agent.prompt.submit"

        $listener = Start-WtEventListener -App $script:app
        try {
            Invoke-RunCommand -App $script:app -SessionId $paneId -Command $command -SettleSec 3 | Out-Null
            $event = Wait-WtEvent -Listener $listener -TimeoutSec 20 -Predicate {
                $_.method -eq 'agent_event' -and
                $_.params.agent_session_id -eq $agentSessionId
            }
            $event.params.event | Should -Be 'agent.prompt.submit'
            $event.params.pane_id | Should -Be $paneId
            $event.params.cli_source | Should -Be 'copilot'
            $event.params.payload.cwd | Should -Be 'C:\native-hook-test'
            $event.params.payload.PSObject.Properties.Name | Should -Not -Contain 'prompt'
        }
        finally {
            Stop-WtEventListener -Listener $listener
        }
    }
}
