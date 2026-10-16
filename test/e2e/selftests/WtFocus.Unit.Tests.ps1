#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeAll {
    . (Join-Path $PSScriptRoot '..\ItE2E\Public\Wt.ps1')
    function Get-AgentPaneSession { param($App, $PaneSessionId, $TabId) }
    function Wait-Until { param($TimeoutSec, $Because, $Condition) }
    function Send-WtWindowKey { param($App, $Vk) }
}

Describe 'Exact assistant focus ownership' -Tag Unit {
    BeforeEach {
        $script:app = @{ Pid = 42; Hwnd = 123; WindowId = '1' }
        $script:helper = @{ PaneSessionId = 'helper'; HelperProcessId = 99; AcpSessionId = 'acp' }
        $script:source = @{ session_id = 'source'; tab_id = 3; window_id = 1 }
        $script:active = @{ session_id = 'source'; tab_id = 3; window_id = 1 }
        Mock Get-AgentPaneSession { $script:helper }
        Mock Invoke-WtCli {
            if ($Arguments[0] -eq 'get-pane-context') { return @{ pane = $script:source } }
        }
        Mock Get-ActivePane { $script:active }
        # A valid Agent Pane peer in the correct HWND/PID is deliberately insufficient.
        Mock Test-WtAgentPaneKeyboardFocus { $true }
        Mock Wait-Until {
            if (-not (& $Condition)) { throw 'Focus verification failed' }
        }
        Mock Send-WtWindowKey {}
    }

    It 'accepts the exact independently supplied owner before subsequent input' {
        Set-WtPaneFocus -App $script:app -SessionId helper -OwnerPaneSessionId source -OwnerTabId stable-guid
        Send-WtWindowKey -App $script:app -Vk 1
        Should -Invoke Test-WtAgentPaneKeyboardFocus -Times 1 -Exactly
        Should -Invoke Send-WtWindowKey -Times 1 -Exactly
        Should -Invoke Get-AgentPaneSession -ParameterFilter { $TabId -eq 'stable-guid' } -Times 2 -Exactly
    }

    It 'rejects <Fault> despite valid same-window same-PID Agent Pane focus, with zero subsequent input' -ForEach @(
        @{ Fault = 'wrong assistant' }, @{ Fault = 'wrong tab' }, @{ Fault = 'wrong window' },
        @{ Fault = 'missing active' }, @{ Fault = 'stale source' }, @{ Fault = 'stale helper' }
    ) {
        switch ($Fault) {
            'wrong assistant' { $script:active.session_id = 'other-source' }
            'wrong tab' { $script:active.tab_id = 4 }
            'wrong window' { $script:active.window_id = 2 }
            'missing active' { $script:active = $null }
            'stale source' {
                Mock Wait-Until {
                    $script:source.tab_id = 4
                    if (-not (& $Condition)) { throw 'Focus verification failed' }
                }
            }
            'stale helper' {
                Mock Wait-Until {
                    $script:helper = @{ PaneSessionId = 'other-helper'; HelperProcessId = 99; AcpSessionId = 'acp' }
                    if (-not (& $Condition)) { throw 'Focus verification failed' }
                }
            }
        }
        {
            Set-WtPaneFocus -App $script:app -SessionId helper -OwnerPaneSessionId source -OwnerTabId stable-guid
            Send-WtWindowKey -App $script:app -Vk 1
        } | Should -Throw '*Focus verification failed*'
        Should -Invoke Send-WtWindowKey -Times 0 -Exactly
    }

    It 'rejects missing <Mapping> before requesting focus or input' -ForEach @(
        @{ Mapping = 'owner source' }, @{ Mapping = 'stable tab' }, @{ Mapping = 'window' }
    ) {
        $ownership = @{ OwnerPaneSessionId = 'source'; OwnerTabId = 'stable-guid' }
        switch ($Mapping) {
            'owner source' { $ownership.Remove('OwnerPaneSessionId') }
            'stable tab' { $ownership.Remove('OwnerTabId') }
            'window' { $script:app.WindowId = $null }
        }
        {
            Set-WtPaneFocus -App $script:app -SessionId helper @ownership
            Send-WtWindowKey -App $script:app -Vk 1
        } | Should -Throw '*independent owner*'
        Should -Invoke Invoke-WtCli -Times 0 -Exactly
        Should -Invoke Send-WtWindowKey -Times 0 -Exactly
    }

    It 'rejects an unrelated stable-tab helper before requesting focus or input' {
        Mock Get-AgentPaneSession {
            if ($TabId) { return @{ PaneSessionId = 'other-helper'; HelperProcessId = 99; AcpSessionId = 'acp' } }
            $script:helper
        }
        {
            Set-WtPaneFocus -App $script:app -SessionId helper -OwnerPaneSessionId source -OwnerTabId stable-guid
            Send-WtWindowKey -App $script:app -Vk 1
        } | Should -Throw '*does not match the pinned helper*'
        Should -Invoke Invoke-WtCli -Times 0 -Exactly
        Should -Invoke Send-WtWindowKey -Times 0 -Exactly
    }

    It 'rejects stale initial ownership before requesting focus or input' {
        $script:source.session_id = 'other-source'
        {
            Set-WtPaneFocus -App $script:app -SessionId helper -OwnerPaneSessionId source -OwnerTabId stable-guid
            Send-WtWindowKey -App $script:app -Vk 1
        } | Should -Throw '*missing or stale*'
        Should -Invoke Invoke-WtCli -ParameterFilter { $Arguments[0] -eq 'focus-pane' } -Times 0 -Exactly
        Should -Invoke Send-WtWindowKey -Times 0 -Exactly
    }

    It 'preserves ordinary shell focus without assistant ownership arguments' {
        Mock Get-AgentPaneSession {}
        Set-WtPaneFocus -App $script:app -SessionId source
        Should -Invoke Test-WtAgentPaneKeyboardFocus -Times 0 -Exactly
    }
}
