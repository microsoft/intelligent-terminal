#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Live self-tests: prove each primitive against a DEPLOYED Intelligent Terminal.
#   Invoke-Pester test/e2e/selftests -Tag Live
# Backs up and restores settings.json/state.json; launches and closes the terminal.

BeforeDiscovery {
    $script:HasPackage = [bool](Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' })
}

Describe 'ItE2E live primitives' -Tag 'Live' -Skip:(-not $script:HasPackage) {

    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true
    }
    AfterAll {
        if ($script:app) { Stop-Terminal -App $script:app }
    }

    Context 'Harness + connection' {
        It 'resolves a braced COM CLSID and a window HWND' {
            $script:app.ComClsid | Should -Match '^\{[0-9A-Fa-f-]+\}$'
            $script:app.Pid | Should -BeGreaterThan 0
        }
        It 'marks the FRE complete' {
            Get-FreCompleted -App $script:app | Should -BeTrue
        }
    }

    Context 'Wt control' {
        It 'returns an active pane with a GUID session id' {
            (Get-ActivePane -App $script:app).session_id | Should -Match '[0-9A-Fa-f-]{36}'
        }
        It 'lists at least one pane' {
            @(Get-WtPanes -App $script:app).Count | Should -BeGreaterThan 0
        }
        It 'runs a command and captures its output' {
            $sid = (Get-ActivePane -App $script:app).session_id
            $tag = "ITE2E_$(Get-Random)"
            Invoke-RunCommand -App $script:app -SessionId $sid -Command "echo $tag" -SettleSec 8 | Out-Null
            Assert-Pane -App $script:app -SessionId $sid -Match $tag -TimeoutSec 10
        }
        It 'creates and closes a tab' {
            $tab = New-WtTab -App $script:app -Title 'ite2e-tab'
            $tab.session_id | Should -Match '[0-9A-Fa-f-]{36}'
            (Get-WtPaneStatus -App $script:app -SessionId $tab.session_id) | Should -Not -BeNullOrEmpty
            { Close-WtPane -App $script:app -SessionId $tab.session_id } | Should -Not -Throw
        }
    }

    Context 'Settings' {
        It 'sets and reads back a top-level AI setting' {
            Set-WtSetting -App $script:app -Key 'acpAgent' -Value 'gemini' | Out-Null
            Assert-Setting -App $script:app -Key 'acpAgent' -Value 'gemini'
        }
        It 'toggles autoFixEnabled via the typed wrapper' {
            Set-WtAutofix -App $script:app -Enabled $true | Out-Null
            Assert-Setting -App $script:app -Key 'autoFixEnabled' -Value $true
        }
    }

    Context 'UI (winapp ui)' {
        It 'sees the AgentToggleButton AutomationId' {
            Assert-Ui -App $script:app -Selector 'AgentToggleButton' -TimeoutSec 10
        }
        It 'sees the NewTabButton AutomationId' {
            Assert-Ui -App $script:app -Selector 'NewTabButton' -TimeoutSec 10
        }
    }

    Context 'Observation' {
        It 'resolves a versioned log directory' {
            Get-ItLogDir -App $script:app | Should -Not -BeNullOrEmpty
        }
        It 'starts and stops an event listener without error' {
            $l = Start-WtEventListener -App $script:app
            Start-Sleep -Milliseconds 500
            { Stop-WtEventListener -Listener $l } | Should -Not -Throw
        }
    }
}
