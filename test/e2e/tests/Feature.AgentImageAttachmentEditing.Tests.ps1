#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PR #536: image attachment tokens behave as one editable input unit.
#
# This crosses the boundary that Rust unit tests cannot cover:
# OS clipboard -> WT/ConPTY Alt+V -> helper attachment state -> rendered input editing.
#
#   Invoke-Pester test/e2e/tests/Feature.AgentImageAttachmentEditing.Tests.ps1 -Tag Feature

BeforeDiscovery {
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and
        (Get-Command copilot -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue)
    )
}

Describe 'Feature: agent image attachment editing' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Out-Null
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Image attachment tokens edit atomically' {
        $prefix = 'atomic-before-'
        $suffix = '-atomic-after'

        Set-AgentPaneFocus -App $script:app | Out-Null
        Clear-AgentInput -App $script:app | Out-Null
        Send-AgentPrompt -App $script:app -Text $prefix -NoSubmit | Out-Null

        Set-ClipboardImage
        Send-AgentAltV -App $script:app | Out-Null
        Assert-AgentPaneText -App $script:app -Pattern 'support image input|\[image:\s+image-\d+\.png\]' -TimeoutSec 15

        $paneText = Get-AgentPaneText -App $script:app -MaxLines 80
        if ($paneText -match 'support image input') {
            Set-ItResult -Skipped -Because 'the configured agent does not advertise the image prompt capability'
            return
        }

        Send-AgentPrompt -App $script:app -Text $suffix -NoSubmit | Out-Null

        # Return to the token's right edge, then one Left must cross the whole token.
        Send-AgentKey -App $script:app -Key Left -Count $suffix.Length | Out-Null
        Send-AgentKey -App $script:app -Key Left | Out-Null
        Send-AgentPrompt -App $script:app -Text 'X' -NoSubmit | Out-Null
        Assert-AgentPaneText -App $script:app -Pattern "${prefix}X\[image:\s+image-\d+\.png\]${suffix}" -TimeoutSec 10

        # One Right crosses back over the token; Backspace then removes the complete
        # attachment while preserving the text on both sides.
        Send-AgentKey -App $script:app -Key Right | Out-Null
        Send-AgentKey -App $script:app -Key BSpace | Out-Null
        Assert-AgentPaneText -App $script:app -Pattern "${prefix}X${suffix}" -TimeoutSec 10

        $after = Get-AgentPaneText -App $script:app -MaxLines 80
        $after | Should -Not -Match '\[image:\s+image-\d+\.png\]'
    }
}
