#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PR #478 / issue #479: submitted prompts are recalled per tab with Up/Down.

BeforeDiscovery {
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and
        (Get-Command copilot -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue)
    )
}

Describe 'Feature agent prompt input history' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }

        function New-HistoryTestTab {
            param([Parameter(Mandatory)][string]$Title)

            $shell = New-WtTab -App $script:app -Title $Title
            Set-WtPaneFocus -App $script:app -SessionId $shell.session_id
            Open-AgentPane -App $script:app | Out-Null
            $pane = Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $shell.session_id -TimeoutSec 30
            Wait-AgentReady -App $script:app -PaneSessionId $pane.PaneSessionId -TimeoutSec 90 |
                Should -BeTrue -Because "the Copilot pane for '$Title' must connect"
            [pscustomobject]@{ Shell = $shell; Pane = $pane }
        }

        function Submit-HistoryPrompt {
            param(
                [Parameter(Mandatory)][string]$PaneSessionId,
                [Parameter(Mandatory)][string]$Text
            )

            Send-AgentPrompt -App $script:app -PaneSessionId $PaneSessionId -Text $Text | Out-Null
            $transcriptPattern = '(?m)^\s*(?:▼\s*)?>\s*' + [regex]::Escape($Text)
            Test-Until -TimeoutSec 10 -IntervalSec 0.25 -Condition {
                (Get-AgentPaneText -App $script:app -PaneSessionId $PaneSessionId -MaxLines 35) -match $transcriptPattern
            } | Should -BeTrue -Because 'the submitted prompt must reach the real agent-pane transcript'

            # History is recorded synchronously on Enter. Cancel the model turn so these
            # keyboard-routing tests do not depend on model response latency or content.
            Send-AgentWin32Key -App $script:app -PaneSessionId $PaneSessionId -Vk 0x43 -Sc 0x2E -Uc 3 -Modifiers 0x08 | Out-Null
            Start-Sleep -Milliseconds 750
        }

        function Get-HistoryInputText {
            param([Parameter(Mandatory)][string]$PaneSessionId)
            Get-AgentPaneText -App $script:app -PaneSessionId $PaneSessionId -MaxLines 35
        }

        function Get-HistoryInputRowPattern {
            param([Parameter(Mandatory)][string]$Text)
            '(?m)^\s*[│║|]\s*>\s*' + [regex]::Escape($Text)
        }
    }

    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Prompt history recall works' {
        $tab = New-HistoryTestTab -Title 'prompt-history-order'
        $id = [guid]::NewGuid().ToString('N')
        $older = "HISTORY_OLDER_$id reply OK"
        $newer = "HISTORY_NEWER_$id reply OK"

        Submit-HistoryPrompt -PaneSessionId $tab.Pane.PaneSessionId -Text $older
        Submit-HistoryPrompt -PaneSessionId $tab.Pane.PaneSessionId -Text $newer

        Send-AgentKey -App $script:app -PaneSessionId $tab.Pane.PaneSessionId -Key Up | Out-Null
        (Get-HistoryInputText -PaneSessionId $tab.Pane.PaneSessionId) |
            Should -Match (Get-HistoryInputRowPattern -Text $newer) -Because 'Up must recall the newest submitted prompt into the input box'

        Send-AgentKey -App $script:app -PaneSessionId $tab.Pane.PaneSessionId -Key Up | Out-Null
        (Get-HistoryInputText -PaneSessionId $tab.Pane.PaneSessionId) |
            Should -Match (Get-HistoryInputRowPattern -Text $older) -Because 'a second Up must move to the next older prompt'

        Send-AgentKey -App $script:app -PaneSessionId $tab.Pane.PaneSessionId -Key Down | Out-Null
        (Get-HistoryInputText -PaneSessionId $tab.Pane.PaneSessionId) |
            Should -Match (Get-HistoryInputRowPattern -Text $newer) -Because 'Down must move back toward newer prompt history'
        Clear-AgentInput -App $script:app -PaneSessionId $tab.Pane.PaneSessionId | Out-Null
    }

    It 'Prompt history preserves drafts and multiline prompts' {
        $tab = New-HistoryTestTab -Title 'prompt-history-multiline'
        $id = [guid]::NewGuid().ToString('N')
        $lineOne = "HISTORY_LINE_ONE_$id"
        $lineTwo = "HISTORY_LINE_TWO_$id reply OK"
        $draft = "HISTORY_DRAFT_$id"
        Send-AgentPrompt -App $script:app -PaneSessionId $tab.Pane.PaneSessionId -Text $lineOne -NoSubmit | Out-Null
        Send-AgentShiftEnter -App $script:app -PaneSessionId $tab.Pane.PaneSessionId | Out-Null
        Send-AgentPrompt -App $script:app -PaneSessionId $tab.Pane.PaneSessionId -Text $lineTwo -NoSubmit | Out-Null
        Send-AgentKey -App $script:app -PaneSessionId $tab.Pane.PaneSessionId -Key Enter | Out-Null
        $firstTranscriptRow = '(?m)^\s*(?:▼\s*)?>\s*' + [regex]::Escape($lineOne)
        Test-Until -TimeoutSec 10 -IntervalSec 0.25 -Condition {
            $text = Get-AgentPaneText -App $script:app -PaneSessionId $tab.Pane.PaneSessionId -MaxLines 35
            $text -match $firstTranscriptRow -and $text -match [regex]::Escape($lineTwo)
        } | Should -BeTrue -Because 'both lines of the submitted prompt must reach the transcript before cancellation'
        Send-AgentWin32Key -App $script:app -PaneSessionId $tab.Pane.PaneSessionId -Vk 0x43 -Sc 0x2E -Uc 3 -Modifiers 0x08 | Out-Null
        Start-Sleep -Milliseconds 750

        Send-AgentPrompt -App $script:app -PaneSessionId $tab.Pane.PaneSessionId -Text $draft -NoSubmit | Out-Null
        Send-AgentKey -App $script:app -PaneSessionId $tab.Pane.PaneSessionId -Key Up | Out-Null
        $recalled = Get-HistoryInputText -PaneSessionId $tab.Pane.PaneSessionId
        $recalled | Should -Match (Get-HistoryInputRowPattern -Text $lineOne)
        $recalled | Should -Match ('(?m)^\s*[│║|]\s{3,}' + [regex]::Escape($lineTwo)) -Because 'the recalled prompt must retain its second line'

        Send-AgentKey -App $script:app -PaneSessionId $tab.Pane.PaneSessionId -Key Down | Out-Null
        (Get-HistoryInputText -PaneSessionId $tab.Pane.PaneSessionId) |
            Should -Match (Get-HistoryInputRowPattern -Text $draft) -Because 'leaving history navigation must restore the unsent draft'
        Clear-AgentInput -App $script:app -PaneSessionId $tab.Pane.PaneSessionId | Out-Null
    }

    It 'Prompt history is isolated per tab' {
        $id = [guid]::NewGuid().ToString('N')
        $tabA = New-HistoryTestTab -Title 'prompt-history-tab-a'
        $promptA = "HISTORY_TAB_A_$id reply OK"
        Submit-HistoryPrompt -PaneSessionId $tabA.Pane.PaneSessionId -Text $promptA

        $tabB = New-HistoryTestTab -Title 'prompt-history-tab-b'
        $promptB = "HISTORY_TAB_B_$id reply OK"
        Submit-HistoryPrompt -PaneSessionId $tabB.Pane.PaneSessionId -Text $promptB

        Set-WtPaneFocus -App $script:app -SessionId $tabA.Shell.session_id
        Send-AgentKey -App $script:app -PaneSessionId $tabA.Pane.PaneSessionId -Key Up | Out-Null
        $textA = Get-HistoryInputText -PaneSessionId $tabA.Pane.PaneSessionId
        $textA | Should -Match (Get-HistoryInputRowPattern -Text $promptA)
        $textA | Should -Not -Match (Get-HistoryInputRowPattern -Text $promptB)

        Set-WtPaneFocus -App $script:app -SessionId $tabB.Shell.session_id
        Send-AgentKey -App $script:app -PaneSessionId $tabB.Pane.PaneSessionId -Key Up | Out-Null
        $textB = Get-HistoryInputText -PaneSessionId $tabB.Pane.PaneSessionId
        $textB | Should -Match (Get-HistoryInputRowPattern -Text $promptB)
        $textB | Should -Not -Match (Get-HistoryInputRowPattern -Text $promptA)
    }
}
