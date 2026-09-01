#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Agent-pane + Auto-error-handling self-tests against a DEPLOYED Intelligent Terminal with an
# authenticated agent CLI (copilot). These exercise the actual AI features.
#   Invoke-Pester test/e2e/selftests -Tag Agent

BeforeDiscovery {
    $script:HasPackage = [bool](Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' })
    $script:HasCopilot = [bool](Get-Command copilot -ErrorAction SilentlyContinue)
}

Describe 'Agent pane + Auto-error-handling' -Tag 'Agent' -Skip:(-not ($script:HasPackage -and $script:HasCopilot)) {

    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot'; autoErrorHandling = 'detectErrorsAndSendToAgentForFixesAutomatically' }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    Context 'Agent pane lifecycle' {
        It 'is closed initially, opens, and closes again' {
            Test-AgentPaneOpen -App $script:app | Should -BeFalse
            Open-AgentPane -App $script:app -TimeoutSec 30 | Out-Null
            Test-AgentPaneOpen -App $script:app | Should -BeTrue
            Stop-AgentPane -App $script:app -TimeoutSec 15 | Out-Null
            Test-AgentPaneOpen -App $script:app | Should -BeFalse
        }
        It 'reaches a ready (connected) state' {
            Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue
        }
    }

    Context 'Auto-error-handling pipeline' {
        It 'emits a failure mark AND submits an Auto-error-handling prompt for a unique bad command' {
            # Ensure the helper is connected, then trigger ONE unique failure (Auto-error-handling
            # de-dupes repeated identical failures, so each test uses a fresh command).
            Wait-AgentReady -App $script:app -TimeoutSec 60 | Out-Null
            $sid = (Get-ActivePane -App $script:app).session_id
            $bogus = "ggit$(Get-Random) status"
            $listener = Start-WtEventListener -App $script:app
            try {
                Start-Sleep -Milliseconds 500
                Invoke-FailingCommand -App $script:app -SessionId $sid -Command $bogus | Out-Null
                # The OSC 133;D non-zero failure mark fires immediately.
                { Wait-WtCommandFailure -Listener $listener -TimeoutSec 20 } | Should -Not -Throw
                # …and Auto-error-handling submits a "command failed" prompt to copilot.
                $ev = Wait-AutoErrorHandling -Listener $listener -TimeoutSec 45
                $ev | Should -Not -BeNullOrEmpty
            }
            finally { Stop-WtEventListener -Listener $listener }
        }
    }
}
