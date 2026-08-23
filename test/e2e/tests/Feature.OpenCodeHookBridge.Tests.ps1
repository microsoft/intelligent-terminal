#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §8 (C273) — OpenCode's plugin must reach the bridge.
#
# OpenCode is the one integration that does NOT dispatch through a shell: its
# plugin spawns an argv array through Bun. Every other CLI hands a command
# string to PowerShell/bash/cmd, so `CreateProcess` resolves the `wtcli.exe` on
# PATH — which is the MSIX app-execution alias, a zero-byte reparse point. Bun
# performs its own PATH lookup instead and rejects it outright.
#
# That broke every OpenCode hook event, and nothing caught it:
#   * the installer tests read the bundle, they do not run it;
#   * C267 executes the shipped command strings, and OpenCode has none;
#   * the uninstall suite runs with the bridge deliberately absent, where zero
#     events is the expected result;
#   * the plugin swallows spawn errors by design, so the CLI looked healthy.
#
# The only thing that would have caught it is what this file does: run the real
# CLI with the bridge present and require the events to arrive.
#
#   Invoke-Pester test/e2e/tests/Feature.OpenCodeHookBridge.Tests.ps1 -Tag Feature

BeforeDiscovery { $script:Ready = [bool](Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) }

Describe 'Feature §8 OpenCode hook bridge' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force

        $script:SkipReason = $null
        if (-not (Get-Command opencode -ErrorAction SilentlyContinue)) {
            $script:SkipReason = 'opencode CLI is not installed'
            return
        }
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true

        # Same guard as the Copilot suite: a plugin left over from an earlier
        # build satisfies "installed" just as well as the one under test, and
        # would quietly turn this into a test of code that is not in this branch.
        $shipped = Join-Path $script:app.InstallLocation 'wt-agent-hooks\opencode\wt-agent-hooks.js'
        $active = Join-Path $HOME '.config\opencode\plugins\wt-agent-hooks.js'
        if (-not (Test-Path $shipped)) { $script:SkipReason = "packaged OpenCode plugin not found at $shipped"; return }
        if (-not (Test-Path $active)) { $script:SkipReason = "OpenCode plugin is not installed at $active"; return }
        $shippedHash = (Get-FileHash -LiteralPath $shipped).Hash
        $activeHash = (Get-FileHash -LiteralPath $active).Hash
        if ($shippedHash -ne $activeHash) {
            $script:SkipReason = "OpenCode is running a different plugin than the package ships " +
            "(installed=$($activeHash.Substring(0,12)) shipped=$($shippedHash.Substring(0,12))). Run ``wta hooks install --cli opencode``."
            Write-ItLog -Level WARN -Message $script:SkipReason
        }
    }

    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    BeforeEach { if ($script:SkipReason) { Set-ItResult -Skipped -Because $script:SkipReason } }

    It 'OpenCode plugin reaches the bridge without a shell (argv spawn resolves wtcli and events arrive)' {
        $paneId = (New-WtTab -App $script:app).session_id
        $token = 'IT-OPENCODE-TOOL-RAN'
        # Output goes to a file: the pane capture echoes the typed command, so a
        # token read back from it would match its own setup line.
        $outFile = Join-Path ([System.IO.Path]::GetTempPath()) "opencode-bridge-$([guid]::NewGuid().ToString('N').Substring(0,6)).txt"

        $listener = Start-WtEventListener -App $script:app
        try {
            $command = "`$c=(Get-Command opencode).Source; " +
            "& `$c run 'Run the shell command: echo $token' *> '$outFile'; " +
            '"IT-OPENCODE" + "-DONE=$LASTEXITCODE"'
            Send-WtInput -App $script:app -SessionId $paneId -Text $command
            Send-WtKeys  -App $script:app -SessionId $paneId -Keys @('Enter')
            $finished = Wait-Until -TimeoutSec 300 -IntervalSec 3 -Quiet -Condition {
                (Get-WtCapture -App $script:app -SessionId $paneId -MaxLines 200) -match 'IT-OPENCODE-DONE=(-?\d+)'
            }
            # Trailing topics (stop / session.end) land just after the turn ends.
            Start-Sleep -Seconds 3

            $out = Get-Content -Raw -LiteralPath $outFile -ErrorAction SilentlyContinue
            Remove-Item -LiteralPath $outFile -Force -ErrorAction SilentlyContinue

            $finished | Should -BeTrue -Because 'opencode must run to completion'
            $out | Should -Match ([regex]::Escape($token)) -Because 'the CLI must have carried out the prompt, or its hooks were never reached'

            $events = @(Get-WtEvents -Listener $listener -Predicate {
                    $_.method -eq 'agent_event' -and $_.params.pane_id -eq $paneId -and $_.params.cli_source -eq 'opencode'
                })
            $events.Count | Should -BeGreaterThan 0 -Because 'the plugin spawns wtcli by argv, which cannot resolve the MSIX alias off PATH — Terminal injects WTCLI_PATH for exactly this'
            @($events | ForEach-Object { $_.params.event }) |
                Should -Contain 'agent.session.start' -Because 'session tracking starts from this topic'
        }
        finally {
            Stop-WtEventListener -Listener $listener
            try { Close-WtPane -App $script:app -SessionId $paneId } catch { }
        }
    }
}
