#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Real failures cross Terminal -> helper/master -> ACP. The deterministic agent
# records the received contract and invokes it only for explicitly selected cases.
# No model quota or user-profile modifications are needed.

Describe 'Feature: on-demand Autofix command resolution' -Tag 'Feature' `
    -Skip:($env:ITE2E_PACKAGE -in @('Store', 'Microsoft.IntelligentTerminal_8wekyb3d8bbwe')) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $package = Get-ItTestPackage
        if ($package -ne 'Dev') { throw 'This suite requires the current feature-branch Debug Dev package.' }
        $script:root = Join-Path $(if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else {
            Join-Path $PSScriptRoot '..\artifacts'
        }) ("autofix-command-resolution-" + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:root -Force | Out-Null
        $script:root = (Resolve-Path -LiteralPath $script:root).Path
        $targetApp = Resolve-ItApp -Package $package
        $binaryHash = (Get-FileHash -LiteralPath $targetApp.WtaPath -Algorithm SHA256).Hash
        if ($env:ITE2E_EXPECTED_WTA_SHA256) {
            $binaryHash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256 -Because 'the deployed WTA must match the selected feature build'
        }
        @{
            package = $targetApp.Package; version = $targetApp.Version
            binary = $targetApp.WtaPath; sha256 = $binaryHash
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:root 'package.json') -Encoding utf8
        $script:evidence = Join-Path $script:root 'evidence.jsonl'
        $requestLog = Join-Path $script:root 'requests.log'
        $commandDirectory = Join-Path $script:root 'commands'
        New-Item -ItemType Directory -Path $commandDirectory | Out-Null
        Set-Content -LiteralPath (Join-Path $commandDirectory 'ite2edeploylocal.ps1') -Value "'fixture-only'"
        $script:cases = @(
            @{ marker = 'ITE2E_RECALL_FIRST'; token = 'ite2e-command-missing-844'; query = $false },
            @{ marker = 'ITE2E_RECALL_SECOND'; token = 'Get-Item'; query = $false },
            @{ marker = 'ITE2E_RECALL_QUERY'; token = 'ite2edeploylocl'; query = $true }
        )
        $config = Join-Path $script:root 'fixture.json'
        @{
            evidencePath = $script:evidence
            commandDirectory = $commandDirectory
            cases = $script:cases
        } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $config -Encoding utf8
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpInteractionAgent.ps1')).Path
        $invocation = "& '$($fixture.Replace("'", "''"))' -LogPath '$($requestLog.Replace("'", "''"))' -ResolverFixturePath '$($config.Replace("'", "''"))'"
        $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation))
        $script:app = Start-Terminal -Package $package -PassFre $true -Settings @{
            acpAgent = 'custom:autofix-command-resolution-fixture'
            acpCustomCommand = "pwsh -NoProfile -EncodedCommand $encoded"
            acpModel = ''
            autoErrorDetectionEnabled = $true
            autoFixEnabled = $true
        }
        $script:targets = @()
        foreach ($label in @('source', 'other')) {
            $workspace = Join-Path $script:root $label
            New-Item -ItemType Directory -Path $workspace | Out-Null
            $shell = New-WtTab -App $script:app -Command 'pwsh.exe -NoLogo -NoExit' -Cwd $workspace -Title "recall-$label"
            Set-WtPaneFocus -App $script:app -SessionId $shell.session_id
            Open-AgentPane -App $script:app | Out-Null
            $agent = Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $shell.session_id -TimeoutSec 30
            Wait-AgentReady -App $script:app -PaneSessionId $agent.PaneSessionId -TimeoutSec 60 | Should -BeTrue
            $script:targets += [pscustomobject]@{ Shell = $shell; Agent = $agent; Cwd = $workspace }
        }
        $startupApp = $script:app.PSObject.Copy()
        $startupApp.LogStartOffset = $script:app.PreLaunchLogStartOffset
        $script:startupLog = Get-ItLogText -App $startupApp -Name 'wta-main_*.log' -SinceStart

        function Read-ResolverEvidence {
            if (Test-Path -LiteralPath $script:evidence) {
                Get-Content -LiteralPath $script:evidence |
                    Where-Object { $_.Trim() } | ForEach-Object { $_ | ConvertFrom-Json }
            }
        }

        function Assert-NoAutomaticCommandProbe {
            $text = Get-ItLogText -App $script:app -Name 'wta-main_*.log' -SinceStart
            $text | Should -Not -Match 'powershell_command_probe_started'
            return $text
        }

        function Invoke-ResolverFailure {
            param([Parameter(Mandatory)]$Case, [Parameter(Mandatory)][string]$Command, [switch]$FocusOtherTab)
            $target = $script:targets[0]
            Initialize-LogOffsets -App $script:app | Out-Null
            Set-WtPaneFocus -App $script:app -SessionId $target.Shell.session_id
            $listener = Start-WtEventListener -App $script:app
            try {
                if ($FocusOtherTab) {
                    $gate = Join-Path $script:root "$($Case.marker).release"
                    $gateLiteral = $gate.Replace("'", "''")
                    $gatedCommand = "`$deadline=[DateTime]::UtcNow.AddSeconds(20); while(-not (Test-Path -LiteralPath '$gateLiteral')) { if([DateTime]::UtcNow -gt `$deadline) { throw 'focus gate timed out' }; Start-Sleep -Milliseconds 50 }; $Command"
                    Send-WtInput -App $script:app -SessionId $target.Shell.session_id -Text $gatedCommand
                    Send-WtKeys -App $script:app -SessionId $target.Shell.session_id -Keys @('Enter')
                    Set-WtPaneFocus -App $script:app -SessionId $script:targets[1].Shell.session_id
                    Set-Content -LiteralPath $gate -Value 'release'
                }
                else {
                    Invoke-RunCommand -App $script:app -SessionId $target.Shell.session_id -Command $Command | Out-Null
                }
                Wait-WtCommandFailure -Listener $listener -PaneId $target.Shell.session_id -TimeoutSec 20 | Out-Null
                if ($FocusOtherTab) {
                    (Get-ActivePane -App $script:app).session_id | Should -Be $script:targets[1].Shell.session_id
                }
                $completion = Wait-Until -TimeoutSec 45 -Because 'the fixture receives and completes this Autofix turn' -Condition {
                    $evidence = @(Read-ResolverEvidence)
                    $failure = $evidence | Where-Object event -eq 'fixture-error' | Select-Object -Last 1
                    if ($failure) { return $failure }
                    $evidence | Where-Object { $_.event -eq 'turn-completed' -and $_.marker -eq $Case.marker } | Select-Object -Last 1
                }
                $completion.event | Should -Be 'turn-completed' -Because "ACP fixture result: $($completion.message)"
            }
            finally { Stop-WtEventListener -Listener $listener }
            # Completion is observed before the bounded negative observation window.
            Start-Sleep -Seconds 1
            $records = @(Read-ResolverEvidence | Where-Object marker -eq $Case.marker)
            $received = @($records | Where-Object event -eq 'prompt-received')
            $received | Should -HaveCount 1
            $received[0].sessionId | Should -Be $target.Agent.AcpSessionId
            $received[0].prompt | Should -Match '(?m)^### Shell Context'
            $received[0].prompt | Should -Match '(?m)^### Terminal Output'
            $received[0].prompt | Should -Not -Match '(?m)^### Near Matches\r?$'
            $received[0].prompt | Should -Match 'not routinely on every failure'
            $received[0].prompt | Should -Not -Match ([regex]::Escape($script:targets[1].Cwd.Replace('\', '\\')))
            $arguments = @($received[0].contract.arguments)
            $arguments[0] | Should -Be 'resolve-command'
            $arguments[1] | Should -Be '<name>'
            $arguments[[array]::IndexOf($arguments, '--cwd') + 1] | Should -Be $target.Cwd
            [System.IO.Path]::GetFileName($arguments[[array]::IndexOf($arguments, '--shell') + 1]) |
                Should -Match '^(pwsh|powershell)(\.exe)?$'
            $logs = Assert-NoAutomaticCommandProbe
            $logs | Should -Match 'context_provider.*id=command_resolver present=true' -Because 'debug diagnostics must be enabled, not silently absent'
            Set-Content -LiteralPath (Join-Path $script:root "$($Case.marker)-helper.log") -Value $logs
            return $records
        }
    }

    AfterAll {
        if ($script:app) { Stop-Terminal -App $script:app }
        # Evidence is deliberately retained under the unique artifact directory.
    }

    It 'Tab selection does not prewarm command resolution' {
        $script:startupLog | Should -Match '\bDEBUG\b' -Because 'zero-probe evidence requires enabled diagnostics'
        $script:startupLog | Should -Not -Match 'powershell_command_probe_started'
        Initialize-LogOffsets -App $script:app | Out-Null
        foreach ($target in @($script:targets[0], $script:targets[1], $script:targets[0])) {
            Set-WtPaneFocus -App $script:app -SessionId $target.Shell.session_id
        }
        Start-Sleep -Seconds 1
        Assert-NoAutomaticCommandProbe | Out-Null
        @(Read-ResolverEvidence) | Should -HaveCount 0
    }

    It 'Autofix sends first and later prompts without command enumeration' {
        $first = Invoke-ResolverFailure -Case $script:cases[0] -Command "$($script:cases[0].token) # $($script:cases[0].marker)"
        @($first | Where-Object event -eq 'query-started') | Should -HaveCount 0
        ($first | Where-Object event -eq 'prompt-received').prompt | Should -Match '# Working in Windows Terminal'
        $second = Invoke-ResolverFailure -Case $script:cases[1] -Command "Get-Item '$($script:cases[1].marker)-missing' -ErrorAction Stop"
        @($second | Where-Object event -eq 'query-started') | Should -HaveCount 0
        ($second | Where-Object event -eq 'prompt-received').prompt | Should -Not -Match '# Working in Windows Terminal'
    }

    It 'Autofix agents can query local command candidates on demand' {
        $case = $script:cases[2]
        $records = Invoke-ResolverFailure -Case $case -Command "$($case.token) # $($case.marker)" -FocusOtherTab
        @($records | ForEach-Object event) | Should -Be @('prompt-received', 'query-started', 'query-result', 'turn-completed')
        $result = ($records | Where-Object event -eq 'query-result').result
        $result.status | Should -Be 'not_found'
        $result.matches | Should -Contain 'ite2edeploylocal'
        $probeLog = Get-ItLogText -App $script:app -Name 'wta-cli*.log' -SinceStart
        $probeLog | Should -Match 'powershell_command_probe_started' -Because 'the explicit CLI lookup is the positive control for command-probe diagnostics'
        $query = $records | Where-Object event -eq 'query-started'
        @($query.arguments) | Should -Contain $case.token
        @($query.arguments) | Should -Not -Contain '<name>'
        [System.IO.Path]::GetFileName($query.executable) | Should -Be 'wta.exe'
        Split-Path (Split-Path $query.executable -Parent) -Leaf |
            Should -Be $script:app.Package -Because 'the advertised alias must resolve to the selected package, not a global alias for another brand'
    }
}
