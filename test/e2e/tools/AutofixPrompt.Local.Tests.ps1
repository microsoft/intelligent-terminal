#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Opt-in, quota-consuming model validation. Excluded from the default tests/selftests paths.

BeforeDiscovery {
    $samples = foreach ($sample in 1..3) {
        @{ Name = 'Obvious command typos bypass lookup'; Sample = $sample; Query = $false }
        @{ Name = 'Unfamiliar local command typos use lookup'; Sample = $sample; Query = $true }
    }
}

Describe 'Local: Autofix prompt decisions' -Tag 'LocalModel' {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        if ((Get-ItTestPackage) -ne 'Dev') { throw 'This local validation requires the feature-branch Dev package.' }
        if (-not $env:ITE2E_EXPECTED_WTA_SHA256) { throw 'Pin the feature build with ITE2E_EXPECTED_WTA_SHA256.' }
        if (-not $env:ITE2E_COMMAND_FIXTURE_DIR) { throw 'Set ITE2E_COMMAND_FIXTURE_DIR to an existing writable user PATH directory.' }
        $script:commandDirectory = (Resolve-Path -LiteralPath $env:ITE2E_COMMAND_FIXTURE_DIR).Path
        if ($script:commandDirectory -notin ($env:PATH -split ';')) { throw 'The fixture directory must already be on PATH.' }
        $target = Resolve-ItApp -Package Dev
        (Get-FileHash -LiteralPath $target.WtaPath).Hash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
        $script:root = Join-Path $PSScriptRoot ("..\artifacts\autofix-prompt-" + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:root | Out-Null
        $script:root = (Resolve-Path -LiteralPath $script:root).Path
        @{
            package = $target.Package; sha256 = $env:ITE2E_EXPECTED_WTA_SHA256
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:root 'package.json')
        $script:app = Start-Terminal -Package Dev -PassFre $true -Settings @{
            acpAgent = 'copilot'
            acpModel = $(if ($env:ITE2E_AUTOFIX_MODEL) { $env:ITE2E_AUTOFIX_MODEL } else { '' })
            autoErrorDetectionEnabled = $true
            autoFixEnabled = $true
        }

        function Read-SessionEvents([string]$Path) {
            if (Test-Path -LiteralPath $Path) {
                Get-Content -LiteralPath $Path |
                    ForEach-Object { $_ | ConvertFrom-Json }
            }
        }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It '<Name> (sample <Sample>)' -ForEach $samples {
        $workspace = Join-Path $script:root "$Query-$Sample"
        New-Item -ItemType Directory -Path $workspace | Out-Null
        $fixtureFile = $null
        $shell = $null
        $events = $null
        try {
            if ($Query) {
                $commandName = "ite2e$([guid]::NewGuid().ToString('N').Substring(0, 8))deploylocal"
                $created = New-Item -ItemType File -Path (Join-Path $script:commandDirectory "$commandName.ps1") -Value "'ITE2E_LOCAL_COMMAND_OK'"
                $fixtureFile = $created.FullName
            }
            $shell = New-WtTab -App $script:app -Command 'pwsh.exe -NoLogo -NoExit' -Cwd $workspace
            Set-WtPaneFocus -App $script:app -SessionId $shell.session_id
            $agent = Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $shell.session_id -TimeoutSec 30
            Wait-AgentReady -App $script:app -PaneSessionId $agent.PaneSessionId -TimeoutSec 60 | Should -BeTrue
            $events = Join-Path $HOME ".copilot\session-state\$($agent.AcpSessionId)\events.jsonl"
            @{ acpSessionId = $agent.AcpSessionId; paneSessionId = $shell.session_id } |
                ConvertTo-Json | Set-Content -LiteralPath (Join-Path $workspace 'session.json')
            $command = if ($Query) { $commandName.Replace('local', 'locl') } else { 'gti status' }
            $listener = Start-WtEventListener -App $script:app
            try {
                Invoke-RunCommand -App $script:app -SessionId $shell.session_id -Command $command | Out-Null
                Wait-WtCommandFailure -Listener $listener -PaneId $shell.session_id -TimeoutSec 20 | Out-Null
            }
            finally { Stop-WtEventListener -Listener $listener }

            $gate = Wait-TerminalActionProposal -App $script:app -PaneSessionId $agent.PaneSessionId -TimeoutSec 60 -ReturnOnPermission:$Query
            if ($gate.Mode -eq 'Permission') {
                Send-AgentKey -App $script:app -PaneSessionId $agent.PaneSessionId -Key Y | Out-Null
                Wait-TerminalActionProposal -App $script:app -PaneSessionId $agent.PaneSessionId -TimeoutSec 60 | Out-Null
            }
            $calls = @(Wait-Until -TimeoutSec 10 -Because 'the proposal tool call is persisted in this ACP session' -Condition {
                $trace = @(Read-SessionEvents $events | Where-Object type -eq 'tool.execution_start')
                if ($trace | Where-Object { $_.data.mcpToolName -eq 'run_command_in_current_shell' }) { return $trace }
            })
            $proposals = @($calls | Where-Object { $_.data.mcpToolName -eq 'run_command_in_current_shell' })
            $proposals | Should -HaveCount 1
            if ($Query) {
                $queries = @($calls | Where-Object {
                    $_.data.toolName -eq 'powershell' -and $_.data.arguments.command -match '\bresolve-command\b'
                })
                $queries | Should -HaveCount 1 -Because 'a genuinely unfamiliar local command needs local evidence'
                $queries[0].data.arguments.command | Should -Match ([regex]::Escape($workspace))
                $queryResult = @(Read-SessionEvents $events | Where-Object {
                    $_.type -eq 'tool.execution_complete' -and $_.data.toolCallId -eq $queries[0].data.toolCallId
                })
                $queryResult | Should -HaveCount 1
                $queryResult[0].data.success | Should -BeTrue
                $queryResult[0].data.result.content | Should -Match ('"matches"\s*:\s*\[\s*"' + [regex]::Escape($commandName) + '"')
                $proposals[0].data.arguments.command | Should -Match ([regex]::Escape($commandName))
                Send-AgentKey -App $script:app -PaneSessionId $agent.PaneSessionId -Key Enter | Out-Null
                Assert-Pane -App $script:app -SessionId $shell.session_id -Match 'ITE2E_LOCAL_COMMAND_OK' -TimeoutSec 20
            }
            else {
                $calls | Should -HaveCount 1 -Because 'an obvious typo must go straight to the proposal, without resolver or substitute discovery tools'
                $calls[0].data.mcpToolName | Should -Be 'run_command_in_current_shell'
                $proposals[0].data.arguments.command | Should -Be 'git status'
                Send-AgentKey -App $script:app -PaneSessionId $agent.PaneSessionId -Key Escape | Out-Null
            }
        }
        finally {
            try {
                if ($events -and (Test-Path -LiteralPath $events)) {
                    # Retain tool evidence, not full conversations or MCP connection credentials.
                    @(Read-SessionEvents $events | Where-Object type -eq 'tool.execution_start' | ForEach-Object {
                        @{ timestamp = $_.timestamp; tool = $_.data.toolName; command = $_.data.arguments.command; model = $_.data.model }
                    }) | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $workspace 'tool-calls.json')
                }
            }
            finally {
                try { if ($shell) { Close-WtPane -App $script:app -SessionId $shell.session_id | Out-Null } }
                finally { if ($fixtureFile) { Remove-Item -LiteralPath $fixtureFile } }
            }
        }
    }
}
