#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeDiscovery {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
}

Describe 'Exact UIA value oracle' -Tag 'Unit' {
    InModuleScope ItE2E {
        BeforeEach {
            $script:valuePeer = [pscustomobject]@{
                Current = @{ Name = 'Search tabs and recent agent sessions' }
                Pattern = [pscustomobject]@{ Current = @{ Value = '' } }
            }
            $script:valuePeer | Add-Member ScriptMethod GetCurrentPattern { param($pattern) $this.Pattern }
            Mock Get-ItUiValuePatternPeer { $script:valuePeer }
            Mock Invoke-WinAppUi { @{ StdOut = '{"text":"Search tabs and recent agent sessions"}'; ExitCode = 0 } }
        }

        It 'preserves a real empty value instead of the placeholder' {
            Get-UiValue -App @{} -Selector SearchTextBox -ValuePattern | Should -BeExactly ''
            Should -Invoke Invoke-WinAppUi -Times 0
        }

        It 'reads nonempty text through the same exact pattern' {
            $script:valuePeer.Pattern.Current.Value = 'sidebar'
            Get-UiValue -App @{} -Selector SearchTextBox -ValuePattern | Should -BeExactly 'sidebar'
        }

        It 'preserves existing display-text fallback when not requested' {
            Get-UiValue -App @{} -Selector SearchTextBox | Should -BeExactly 'Search tabs and recent agent sessions'
            Should -Invoke Get-ItUiValuePatternPeer -Times 0
        }

        It 'fails when the exact target is unavailable rather than reporting empty' {
            Mock Get-ItUiValuePatternPeer { throw 'exact peer unavailable' }
            { Get-UiValue -App @{} -Selector SearchTextBox -ValuePattern } | Should -Throw '*exact peer unavailable*'
        }

        It 'fails when ValuePattern is unsupported rather than using Name' {
            $script:valuePeer | Add-Member ScriptMethod GetCurrentPattern { param($pattern) throw 'unsupported ValuePattern' } -Force
            { Get-UiValue -App @{} -Selector SearchTextBox -ValuePattern } | Should -Throw '*unsupported ValuePattern*'
            Should -Invoke Invoke-WinAppUi -Times 0
        }
    }
}
