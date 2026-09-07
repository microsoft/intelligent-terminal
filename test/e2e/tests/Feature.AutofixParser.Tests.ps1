#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Regression coverage for issue #474 and PR #477.
#
# Feature.ShellIntegration.Tests.ps1 verifies the PowerShell-side OSC 133 contract in
# isolation. Feature.AutofixPane.Tests.ps1 verifies the existing Autofix UI and actions
# with ordinary command failures. This suite closes the integration gap between them:
# a real PowerShell parser error must cross the complete shell -> WT protocol -> WTA
# pipeline and submit exactly one Autofix prompt, while ambiguous successful commands
# must not submit one.

BeforeDiscovery {
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and
        (Get-Command copilot -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue)
    )
}

BeforeAll {
    function Get-ParserAutofixEvidence {
        param($App, [string]$PaneId, [string]$TabId, [string]$AcpSessionId)

        # Pending UI states can be re-emitted without submitting a turn. Count the
        # dispatcher handoff and the master's actual receipt of session/prompt instead.
        $dispatchPattern = 'autofix: sending auto-fix prompt\s+pane_id=' +
            [regex]::Escape($PaneId) + '\s+tab_id=' + [regex]::Escape($TabId) + '(?=\s|$)'
        $promptPattern = 'master: forwarding prompt to agent CLI \(non-blocking\).*op="prompt".*session_id=' +
            [regex]::Escape(('SessionId("{0}")' -f $AcpSessionId)) + '(?=\s|$)'
        [pscustomobject]@{
            Dispatches = @((Get-ItLogText -App $App -Name 'wta-main_helper-*.log' -SinceStart) -split '\r?\n' |
                    Where-Object { $_ -match $dispatchPattern })
            Prompts = @((Get-ItLogText -App $App -Name 'wta-main_master.log' -SinceStart) -split '\r?\n' |
                    Where-Object { $_ -match $promptPattern })
        }
    }
}

Describe 'Feature: PowerShell parser errors trigger Autofix end-to-end' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent      = 'copilot'
            autoFixEnabled = $true
        }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 |
            Should -BeTrue -Because 'Autofix requires a connected ACP session'
        $script:sid = (Get-ActivePane -App $script:app).session_id
        $tabId = Resolve-AgentOwnerTabId -App $script:app -OwnerPaneSessionId $script:sid
        $agent = Wait-NewAgentPaneSession -App $script:app -TabId $tabId
        $agent.AcpSessionId | Should -Not -BeNullOrEmpty
        $script:scope = @{ App = $script:app; PaneId = $script:sid; TabId = $tabId; AcpSessionId = $agent.AcpSessionId }
    }
    BeforeEach { Initialize-LogOffsets -App $script:app | Out-Null }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'PowerShell parser errors trigger exactly one Autofix prompt' {
        $listener = Start-WtEventListener -App $script:app
        try {
            Start-Sleep -Milliseconds 400
            Invoke-RunCommand -App $script:app -SessionId $script:sid -Command "'x'.like '*x*'" | Out-Null

            $failure = Wait-WtCommandFailure -Listener $listener -PaneId $script:sid -TimeoutSec 20
            "$($failure.params.sequence)" |
                Should -Match '(?i)osc:133;D;(?!0(\b|;|$))' -Because 'the parser error must be corrected from stale exit code 0'

            "$($failure.params.tab_id)" | Should -Be $script:scope.TabId
            $autofix = Wait-Until -TimeoutSec 45 -IntervalSec 0.4 -Because 'the parser Autofix prompt to cross the helper/master ACP boundary' -Condition {
                $evidence = Get-ParserAutofixEvidence @script:scope
                if ($evidence.Dispatches.Count -gt 0 -and $evidence.Prompts.Count -gt 0) { $evidence }
            }
            $autofix | Should -Not -BeNullOrEmpty -Because 'a local dispatcher log alone does not prove ACP submission'

            Start-Sleep -Seconds 2
            $evidence = Get-ParserAutofixEvidence @script:scope
            $evidence.Dispatches | Should -HaveCount 1 -Because 'one malformed command must dispatch one Autofix turn'
            $evidence.Prompts | Should -HaveCount 1 -Because 'exactly one prompt must reach the owning ACP session'
        }
        finally { Stop-WtEventListener -Listener $listener }
    }

    It 'Parser-error prompt redraw does not retrigger Autofix' {
        $listener = Start-WtEventListener -App $script:app
        try {
            Start-Sleep -Milliseconds 400
            Send-WtKeys -App $script:app -SessionId $script:sid -Keys @('Enter') | Out-Null
            Wait-WtEvent -Listener $listener -TimeoutSec 20 -Predicate {
                $_.method -eq 'vt_sequence' -and
                "$($_.params.pane_id)" -eq "$script:sid" -and
                "$($_.params.sequence)" -match '(?i)osc:133;A(\b|;|$)'
            } | Out-Null
            Start-Sleep -Seconds 3

            @(Get-WtEvents -Listener $listener -Predicate {
                    $_.method -eq 'vt_sequence' -and
                    "$($_.params.pane_id)" -eq "$script:sid" -and
                    "$($_.params.sequence)" -match '(?i)osc:133;D;(?!0(\b|;|$))'
                }) | Should -BeNullOrEmpty -Because 'blank input must not replay the previous parser failure'
            $evidence = Get-ParserAutofixEvidence @script:scope
            $evidence.Dispatches | Should -BeNullOrEmpty -Because 'a prompt redraw must not dispatch another Autofix turn'
            $evidence.Prompts | Should -BeNullOrEmpty -Because 'a prompt redraw must not submit another ACP prompt'
        }
        finally { Stop-WtEventListener -Listener $listener }
    }
}

Describe 'Feature: successful PowerShell completion does not trigger Autofix' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent      = 'copilot'
            autoFixEnabled = $true
        }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 |
            Should -BeTrue -Because 'negative assertions require a connected Autofix pipeline'
        $script:sid = (Get-ActivePane -App $script:app).session_id
        $tabId = Resolve-AgentOwnerTabId -App $script:app -OwnerPaneSessionId $script:sid
        $agent = Wait-NewAgentPaneSession -App $script:app -TabId $tabId
        $agent.AcpSessionId | Should -Not -BeNullOrEmpty
        $script:scope = @{ App = $script:app; PaneId = $script:sid; TabId = $tabId; AcpSessionId = $agent.AcpSessionId }
    }
    BeforeEach { Initialize-LogOffsets -App $script:app | Out-Null }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Successful PowerShell commands do not trigger Autofix' {
        $listener = Start-WtEventListener -App $script:app
        try {
            Start-Sleep -Milliseconds 400
            Invoke-RunCommand -App $script:app -SessionId $script:sid -Command "Write-Output normal-ok-$([guid]::NewGuid())" | Out-Null
            { Wait-WtEvent -Listener $listener -TimeoutSec 20 -Predicate {
                    $_.method -eq 'vt_sequence' -and
                    "$($_.params.pane_id)" -eq "$script:sid" -and
                    "$($_.params.sequence)" -match '(?i)osc:133;D;0(\b|;|$)'
                } } | Should -Not -Throw
            Start-Sleep -Seconds 2
            $evidence = Get-ParserAutofixEvidence @script:scope
            $evidence.Dispatches | Should -BeNullOrEmpty -Because 'successful commands must not dispatch Autofix'
            $evidence.Prompts | Should -BeNullOrEmpty -Because 'successful commands must not submit an ACP prompt'
        }
        finally { Stop-WtEventListener -Listener $listener }
    }

    It 'Handled non-terminating PowerShell errors do not trigger Autofix' {
        $missingPath = Join-Path $PSScriptRoot "it-autofix-parser-missing-$([guid]::NewGuid())"
        $escapedPath = $missingPath.Replace("'", "''")
        $listener = Start-WtEventListener -App $script:app
        try {
            Start-Sleep -Milliseconds 400
            Invoke-RunCommand -App $script:app -SessionId $script:sid -Command "Get-Item '$escapedPath' -ErrorAction SilentlyContinue; Write-Output ok" | Out-Null
            { Wait-WtEvent -Listener $listener -TimeoutSec 20 -Predicate {
                    $_.method -eq 'vt_sequence' -and
                    "$($_.params.pane_id)" -eq "$script:sid" -and
                    "$($_.params.sequence)" -match '(?i)osc:133;D;0(\b|;|$)'
                } } | Should -Not -Throw
            Start-Sleep -Seconds 2
            $evidence = Get-ParserAutofixEvidence @script:scope
            $evidence.Dispatches | Should -BeNullOrEmpty -Because 'handled errors that finish successfully must remain distinct from parser failures'
            $evidence.Prompts | Should -BeNullOrEmpty -Because 'handled errors must not submit an ACP prompt'
        }
        finally { Stop-WtEventListener -Listener $listener }
    }
}
