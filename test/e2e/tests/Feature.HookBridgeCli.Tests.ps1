#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §8 — the hook bundle must work when a REAL agent CLI fires it.
#
# Feature.HookTrace (C190) proves the bridge itself: it types `wtcli agent-hook` into a pane and
# asserts the published event. That deliberately bypasses the two layers the bundle actually ships:
# the agent CLI's own hook executor, and the Windows shell that CLI dispatches the `hooks.json`
# `command` string through. Both layers are where PR #571 regressed — Copilot dispatches hooks
# through PowerShell, where a command starting with a quoted path is a parse error, and because
# Copilot's PreToolUse hook is FAIL-CLOSED that error denied every tool call in the session
# ("Denied by preToolUse hook ... (hook errored)").
#
# Unit tests (`bundled_hook_commands_run_in_every_shell`) now execute the shipped command
# lines in each CLI's shell, but they cannot prove that the CLI accepts the bundle or that a
# broken hook does not take the agent down with it. That is what these three cases add:
# a working bundle, a bridge that cannot reach Terminal, and a bridge that is gone from PATH
# entirely — the state an Intelligent Terminal uninstall leaves behind.
#
# The oracle is deliberately LLM-independent: SessionStart/UserPromptSubmit fire on every prompt
# regardless of what the model decides to do, so a model that answers without calling a tool
# cannot make this test flap.
#
#   Invoke-Pester test/e2e/tests/Feature.HookBridgeCli.Tests.ps1 -Tag Feature

BeforeDiscovery { $script:Ready = [bool](Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) }

Describe 'Feature §8 hook bundle runs inside a real agent CLI' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force

        $script:Cli = 'copilot'
        # Signature of the fail-closed regression this suite exists to catch.
        $script:DenyPattern = 'Denied by preToolUse hook'

        $script:app = $null
        $script:configBackup = $null
        $script:SkipReason = $null
        $script:WeInstalled = $false

        $status = Get-AgentCliStatus -Agent $script:Cli
        Write-ItLog -Level INFO -Message "HookBridgeCli: $($script:Cli) CLI status = $status"
        if ($status -ne 'authed') {
            $script:SkipReason = "$($script:Cli) CLI is not installed+authenticated ($status)"
            return
        }

        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = $script:Cli }

        function script:Get-HookInstallState {
            try {
                $raw = (Invoke-Wta -App $script:app -Arguments @('hooks', 'status', '--json') -TimeoutSec 60 -Raw).StdOut
                $entry = @(($raw | ConvertFrom-Json).clis) | Where-Object { $_.name -eq $script:Cli } | Select-Object -First 1
                return [bool]$entry.plugin_installed
            }
            catch { return $false }
        }

        # Every assertion in this suite reads the CLI's own output from a FILE,
        # never from the pane capture. The capture echoes the command being typed,
        # so any literal used as evidence matches its own setup line: a done marker
        # makes the completion wait return before the CLI has even started, and a
        # negative check fires on the command that arranged it. Both happened here
        # — the first real run of this suite had two cases passing vacuously and a
        # third failing on its own echo.
        #
        # The done marker still travels through the capture, so it is assembled at
        # runtime and never appears whole in the typed line.
        function script:Invoke-CliInPane {
            <#
            .SYNOPSIS
                Run the agent CLI in a pane and return what IT printed, plus its exit code.
            .PARAMETER Prelude
                Statements to run before the CLI, e.g. breaking the bridge. The CLI is
                always resolved to an absolute path BEFORE the prelude runs, so a prelude
                that scrubs PATH cannot take the agent down with it.
            #>
            param(
                [string]$PaneId,
                [string]$Prompt,
                [string]$Prelude = '',
                [string]$Tag,
                [int]$TimeoutSec = 240
            )
            $outFile = Join-Path ([System.IO.Path]::GetTempPath()) "ithookbridge-$Tag-$([guid]::NewGuid().ToString('N').Substring(0,6)).txt"
            $doneExpr = '"IT-HOOK" + "-DONE=$LASTEXITCODE"'
            $command = "`$c=(Get-Command $($script:Cli)).Source; $Prelude" +
            "& `$c -p '$Prompt' --allow-all-tools *> '$outFile'; $doneExpr"

            Send-WtInput -App $script:app -SessionId $PaneId -Text $command
            Send-WtKeys  -App $script:app -SessionId $PaneId -Keys @('Enter')
            $finished = Wait-Until -TimeoutSec $TimeoutSec -IntervalSec 2 -Quiet -Condition {
                (Get-WtCapture -App $script:app -SessionId $PaneId -MaxLines 200) -match 'IT-HOOK-DONE=(-?\d+)'
            }
            $capture = Get-WtCapture -App $script:app -SessionId $PaneId -MaxLines 200
            $text = Get-Content -Raw -LiteralPath $outFile -ErrorAction SilentlyContinue
            Remove-Item -LiteralPath $outFile -Force -ErrorAction SilentlyContinue
            [pscustomobject]@{
                Finished = [bool]$finished
                ExitCode = if ($capture -match 'IT-HOOK-DONE=(-?\d+)') { [int]$Matches[1] } else { $null }
                Output   = if ($text) { $text } else { '' }
            }
        }

        # Install the hooks the same way FRE/Settings do, so this exercises the SHIPPED bundle
        # rather than whatever the developer happens to have installed. Anything we install is
        # removed again in AfterAll: restoring only the CLI config would leave the plugin files
        # behind as an orphaned install directory that the CLI no longer lists but that can
        # still break the next install.
        $preinstalled = script:Get-HookInstallState
        $script:configBackup = Backup-CopilotConfig
        $installExit = $null
        try {
            $install = Invoke-Wta -App $script:app -Arguments @('hooks', 'install', '--cli', $script:Cli) -TimeoutSec 180 -Raw
            $installExit = $install.ExitCode
            $script:WeInstalled = -not $preinstalled
        }
        catch {
            $script:SkipReason = "wta hooks install --cli $($script:Cli) failed: $_"
            return
        }

        if (-not (script:Get-HookInstallState)) {
            $script:SkipReason = "wt-agent-hooks is not reported installed for $($script:Cli) after install"
            return
        }

        # `plugin_installed` only says SOME wt-agent-hooks is registered, which a
        # plugin left over from an earlier build satisfies just as well as the one
        # we meant to install. `<cli> plugin install` replaces the whole plugin
        # directory and Windows denies that while a CLI process holds it open, so
        # a failed install plus a stale plugin is a state this machine reaches
        # routinely — and it silently turns every case below into a test of code
        # that is not in this branch. It cost two days of believing the native
        # bridge was under test while Copilot still ran the pre-#571 spelling.
        #
        # Compare what the CLI actually has against what the package ships. A
        # mismatch is reported as a skip rather than a pass, because the honest
        # answer is "not tested", not "works".
        $shipped = Join-Path $script:app.InstallLocation 'wt-agent-hooks\copilot\wt-agent-hooks\hooks\hooks.json'
        $active = Join-Path $HOME '.copilot\installed-plugins\wt-local\wt-agent-hooks\hooks\hooks.json'
        if (-not (Test-Path $shipped)) {
            $script:SkipReason = "packaged $($script:Cli) bundle not found at $shipped"
            return
        }
        if (-not (Test-Path $active)) {
            $script:SkipReason = "$($script:Cli) reports the plugin installed but its hooks.json is missing at $active"
            return
        }
        $shippedHash = (Get-FileHash -LiteralPath $shipped).Hash
        $activeHash = (Get-FileHash -LiteralPath $active).Hash
        if ($shippedHash -ne $activeHash) {
            $script:SkipReason = "$($script:Cli) is running a different bundle than the package ships " +
            "(installed=$($activeHash.Substring(0,12)) shipped=$($shippedHash.Substring(0,12)); " +
            "`wta hooks install` exited $installExit). Close every running $($script:Cli) session and reinstall — " +
            'a running CLI holds its plugin directory open, which blocks the install from replacing it.'
            Write-ItLog -Level WARN -Message $script:SkipReason
            return
        }
        Write-ItLog -Level INFO -Message "HookBridgeCli: $($script:Cli) is running the packaged bundle ($($activeHash.Substring(0,12)))"
    }

    AfterAll {
        if ($script:WeInstalled -and $script:app) {
            try { Invoke-Wta -App $script:app -Arguments @('hooks', 'uninstall', '--cli', $script:Cli) -TimeoutSec 180 -Raw | Out-Null } catch { }
        }
        if ($script:configBackup) { Restore-CopilotConfig -State $script:configBackup }
        if ($script:app) { Stop-Terminal -App $script:app }
    }

    BeforeEach {
        if ($script:SkipReason) { Set-ItResult -Skipped -Because $script:SkipReason }
    }

    It 'Bundled hook runs inside its agent CLI (a real CLI session delivers hook events to Terminal)' {
        # Fresh tab so the CLI runs in a pane with a known, unshared WT_SESSION binding.
        $paneId = (New-WtTab -App $script:app).session_id
        # The prompt has to force a TOOL CALL. `preToolUse` is the only fail-closed
        # hook, so a prompt the model can answer from memory never reaches the hook
        # that can deny anything, and "was not denied" would prove nothing.
        $token = 'IT-HOOK-TOOL-RAN'

        $listener = Start-WtEventListener -App $script:app
        try {
            $run = script:Invoke-CliInPane -PaneId $paneId -Tag 'delivers' -Prompt "Run the shell command: echo $token"

            # The real contract: a hook fired by the CLI itself reaches Terminal,
            # tagged with the pane it came from.
            $event = $null
            try {
                $event = Wait-WtEvent -Listener $listener -TimeoutSec 120 -Predicate {
                    $_.method -eq 'agent_event' -and
                    $_.params.pane_id -eq $paneId -and
                    $_.params.cli_source -eq $script:Cli
                }
            }
            catch { }
            $event | Should -Not -BeNullOrEmpty -Because 'a hook fired by the CLI itself must reach Terminal'
            $event.params.event | Should -Match '^agent\.' -Because 'the bridge must publish a normalised WTA topic'

            $run.Finished | Should -BeTrue -Because "$($script:Cli) must run to completion"
            $run.Output | Should -Match ([regex]::Escape($token)) -Because 'the tool call must actually have run, or the fail-closed hook was never exercised'
            $run.Output | Should -Not -Match $script:DenyPattern -Because 'a hook error must never deny the agent its tools'
        }
        finally {
            Stop-WtEventListener -Listener $listener
            try { Close-WtPane -App $script:app -SessionId $paneId } catch { }
        }
    }

    It 'Hook failure never blocks the agent CLI (an unreachable protocol server degrades silently)' {
        # Point the pane at a CLSID that is not registered, so `wtcli agent-hook` genuinely fails to reach
        # Terminal. Its exit-0 contract is the only thing standing between that
        # failure and a dead agent session.
        $paneId = (New-WtTab -App $script:app).session_id
        $token = 'IT-HOOK-TOOL-RAN'

        $listener = Start-WtEventListener -App $script:app
        try {
            $run = script:Invoke-CliInPane -PaneId $paneId -Tag 'broken-clsid' `
                -Prompt "Run the shell command: echo $token" `
                -Prelude "`$env:WT_COM_CLSID='{00000000-0000-0000-0000-000000000000}'; "

            $run.Finished | Should -BeTrue -Because 'the CLI must run to completion despite the broken hook bridge'
            $run.Output | Should -Match ([regex]::Escape($token)) -Because 'tool calls must still run when the bridge cannot reach Terminal'
            $run.Output | Should -Not -Match $script:DenyPattern -Because 'a broken hook bridge must not deny the agent its tools'

            # Bounded negative window: no event may arrive from this pane, confirming the bridge
            # really was broken and this case is not silently passing on a working one.
            @(Get-WtEvents -Listener $listener -Predicate {
                    $_.method -eq 'agent_event' -and $_.params.pane_id -eq $paneId
                }).Count | Should -Be 0 -Because 'the hook could not reach Terminal, so it must publish nothing'
        }
        finally {
            Stop-WtEventListener -Listener $listener
            try { Close-WtPane -App $script:app -SessionId $paneId } catch { }
        }
    }

    It 'Uninstalling Terminal never blocks the agent CLI (a bridge missing from PATH degrades silently)' {
        # The other failure shape: `wtcli.exe` reaches PATH through the MSIX app-execution alias,
        # which uninstall deletes while the CLI keeps the hook config registered. The shell — not
        # the bridge — then decides the exit code, and a missing command makes it 1, which a
        # fail-closed PreToolUse hook turns into a denial of every tool call.
        #
        # Scrubbing every PATH entry that supplies wtcli.exe reproduces that state without
        # uninstalling Terminal.
        $paneId = (New-WtTab -App $script:app).session_id
        $token = 'IT-HOOK-TOOL-RAN'
        # The scrub's own result goes to a file for the same reason the CLI's output
        # does: a marker echoed in the command line would match itself.
        $probeFile = Join-Path ([System.IO.Path]::GetTempPath()) "ithookbridge-scrub-$([guid]::NewGuid().ToString('N').Substring(0,6)).txt"

        $listener = Start-WtEventListener -App $script:app
        try {
            $prelude = "`$env:PATH=((`$env:PATH -split ';') | Where-Object { `$_ -and -not (Test-Path (Join-Path `$_ 'wtcli.exe')) }) -join ';'; " +
            "(`$null -ne (Get-Command wtcli.exe -ErrorAction SilentlyContinue)) | Out-File -LiteralPath '$probeFile' -Encoding utf8; "
            $run = script:Invoke-CliInPane -PaneId $paneId -Tag 'no-bridge' `
                -Prompt "Run the shell command: echo $token" -Prelude $prelude

            (Get-Content -Raw -LiteralPath $probeFile -ErrorAction SilentlyContinue) |
                Should -Match 'False' -Because 'the scrub must actually remove wtcli.exe, or this case passes on a working bridge'

            $run.Finished | Should -BeTrue -Because 'the CLI must run to completion with no bridge on PATH'
            $run.Output | Should -Match ([regex]::Escape($token)) -Because 'tool calls must still run once the bridge is gone'
            $run.Output | Should -Not -Match $script:DenyPattern -Because 'an uninstalled bridge must not deny the agent its tools'

            # Same bounded negative window as the broken-CLSID case: nothing can have been published.
            @(Get-WtEvents -Listener $listener -Predicate {
                    $_.method -eq 'agent_event' -and $_.params.pane_id -eq $paneId
                }).Count | Should -Be 0 -Because 'the bridge was not on PATH, so it must publish nothing'
        }
        finally {
            Remove-Item -LiteralPath $probeFile -Force -ErrorAction SilentlyContinue
            Stop-WtEventListener -Listener $listener
            try { Close-WtPane -App $script:app -SessionId $paneId } catch { }
        }
    }
}
