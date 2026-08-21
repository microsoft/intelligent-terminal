#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Completed-turn keyboard selection must keep the selected turn in the visible
# chat viewport while Tab/Up/Down navigates beyond the initially visible rows.

BeforeDiscovery {
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and
        (Get-Command pwsh -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue)
    )
}

Describe 'Feature: completed-turn keyboard selection' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $fixtureSource = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpChatAgent.ps1')).Path
        $script:fixtureDir = Join-Path $env:TEMP "ItE2E completed turn scroll $([guid]::NewGuid().ToString('N'))"
        New-Item -ItemType Directory -Path $script:fixtureDir | Out-Null
        $fixture = Join-Path $script:fixtureDir 'Mock ACP Chat Agent.ps1'
        Copy-Item -LiteralPath $fixtureSource -Destination $fixture
        $script:fixtureLog = Join-Path $script:fixtureDir 'fixture output.log'
        $fixtureInvocation = "& '$($fixture.Replace("'", "''"))' -LogPath '$($script:fixtureLog.Replace("'", "''"))'"
        $encodedInvocation = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($fixtureInvocation))
        $command = "pwsh -NoProfile -EncodedCommand $encodedInvocation"
        $script:evidenceDir = Join-Path $PSScriptRoot '..\artifacts\completed-turn-selection-scroll'
        New-Item -ItemType Directory -Force -Path $script:evidenceDir | Out-Null

        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent = 'custom:chat-fixture'
            acpCustomCommand = $command
        }
        $shell = Get-ActivePane -App $script:app
        Open-AgentPane -App $script:app | Out-Null
        $script:agentPane = (Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $shell.session_id -TimeoutSec 30).PaneSessionId
        Wait-AgentReady -App $script:app -PaneSessionId $script:agentPane -TimeoutSec 60 |
            Should -BeTrue -Because 'the deterministic ACP chat fixture must connect before creating completed turns'
    }

    AfterAll {
        if ($script:app) {
            Stop-Terminal -App $script:app
        }
        if ($script:fixtureLog -and (Test-Path -LiteralPath $script:fixtureLog)) {
            Copy-Item -LiteralPath $script:fixtureLog -Destination (Join-Path $script:evidenceDir 'fixture.log') -Force
        }
        if ($script:fixtureDir -and (Test-Path -LiteralPath $script:fixtureDir)) {
            Remove-Item -LiteralPath $script:fixtureDir -Recurse -Force
        }
    }

    It 'Keyboard selection keeps focused completed turns visible' {
        $id = [guid]::NewGuid().ToString('N')
        $prompts = @(0..11 | ForEach-Object { 'SCROLL_TURN_{0:D2}_{1}' -f $_, $id })
        $readyPattern = Get-WtaLocalizedTextRegex -Key 'input.placeholder.connected'
        if (-not $readyPattern) {
            $readyPattern = '(?i)Ask anything.*for commands'
        }

        for ($index = 0; $index -lt $prompts.Count; $index++) {
            $prompt = $prompts[$index]
            $replyPattern = [regex]::Escape("ACK_$prompt")
            Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text $prompt | Out-Null
            $turnCompleted = Test-Until -TimeoutSec 10 -IntervalSec 0.25 -Condition {
                $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
                $text -match $replyPattern -and $text -match $readyPattern
            }
            $turnCompleted | Should -BeTrue -Because "turn $index must render its deterministic reply and return input focus before the next prompt"
        }

        $fixturePrompts = @(Get-Content -LiteralPath $script:fixtureLog | Where-Object { $_ -match '\|prompt\|SCROLL_TURN_' })
        $fixturePrompts.Count | Should -Be $prompts.Count -Because 'the deterministic ACP fixture must receive every prompt exactly once'

        $oldest = $prompts[0]
        $newest = $prompts[-1]
        $oldestCompletedRow = '(?m)^[^\r\n]*>\s*' + [regex]::Escape($oldest) + '[^\r\n]*\r?$'
        $newestCompletedRow = '(?m)^[^\r\n]*>\s*' + [regex]::Escape($newest) + '[^\r\n]*\r?$'

        $before = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
        Set-Content -LiteralPath (Join-Path $script:evidenceDir 'before-navigation.txt') -Value $before -Encoding utf8NoBOM
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'before-navigation.png') | Out-Null
        $before | Should -Match $newestCompletedRow -Because 'the newest completed turn must start inside the viewport'
        $before | Should -Not -Match $oldestCompletedRow -Because 'the oldest completed turn must start outside the viewport'

        Send-AgentKey -App $script:app -PaneSessionId $script:agentPane -Key Tab | Out-Null
        Send-AgentKey -App $script:app -PaneSessionId $script:agentPane -Key Up -Count ($prompts.Count - 1) | Out-Null

        $targetVisible = Test-Until -TimeoutSec 8 -IntervalSec 0.25 -Condition {
            (Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100) -match $oldestCompletedRow
        }
        $after = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
        Set-Content -LiteralPath (Join-Path $script:evidenceDir 'after-navigation.txt') -Value $after -Encoding utf8NoBOM
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'after-navigation.png') | Out-Null
        $targetVisible | Should -BeTrue -Because 'the chat viewport must follow keyboard focus to the oldest completed turn'
        Send-AgentKey -App $script:app -PaneSessionId $script:agentPane -Key Escape | Out-Null
    }
}
