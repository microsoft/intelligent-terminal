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
        $script:DoneMarker = 'IT-HOOK-BRIDGE-DONE'
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

        # Install the hooks the same way FRE/Settings do, so this exercises the SHIPPED bundle
        # rather than whatever the developer happens to have installed. Anything we install is
        # removed again in AfterAll: restoring only the CLI config would leave the plugin files
        # behind as an orphaned install directory that the CLI no longer lists but that can
        # still break the next install.
        $preinstalled = script:Get-HookInstallState
        $script:configBackup = Backup-CopilotConfig
        try {
            Invoke-Wta -App $script:app -Arguments @('hooks', 'install', '--cli', $script:Cli) -TimeoutSec 180 -Raw | Out-Null
            $script:WeInstalled = -not $preinstalled
        }
        catch {
            $script:SkipReason = "wta hooks install --cli $($script:Cli) failed: $_"
            return
        }

        if (-not (script:Get-HookInstallState)) {
            $script:SkipReason = "wt-agent-hooks is not reported installed for $($script:Cli) after install"
        }
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
        $prompt = 'Reply with only the token READY.'
        $command = "$($script:Cli) -p '$prompt' --allow-all-tools; echo $($script:DoneMarker)"

        $listener = Start-WtEventListener -App $script:app
        try {
            Send-WtInput -App $script:app -SessionId $paneId -Text $command
            Send-WtKeys  -App $script:app -SessionId $paneId -Keys @('Enter')

            # The real contract: a hook fired by the CLI itself reaches Terminal, tagged with the
            # pane it came from. SessionStart/UserPromptSubmit fire before any model work, so this
            # resolves quickly and never depends on the model calling a tool.
            $event = Wait-WtEvent -Listener $listener -TimeoutSec 120 -Predicate {
                $_.method -eq 'agent_event' -and
                $_.params.pane_id -eq $paneId -and
                $_.params.cli_source -eq $script:Cli
            }
            $event.params.event | Should -Match '^agent\.' -Because 'the bridge must publish a normalised WTA topic'

            # And the fail-closed half: the hook must not have denied the CLI's own tool calls.
            $doneRe = [regex]::Escape($script:DoneMarker)
            Wait-Until -TimeoutSec 240 -IntervalSec 2 -Because "$($script:Cli) to finish" -Condition {
                (Get-WtCapture -App $script:app -SessionId $paneId -MaxLines 200) -match $doneRe
            } | Out-Null
            $out = Get-WtCapture -App $script:app -SessionId $paneId -MaxLines 200
            $out | Should -Not -Match $script:DenyPattern -Because 'a hook error must never deny the agent its tools'
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
        $prompt = 'Reply with only the token READY.'
        $command = "`$env:WT_COM_CLSID='{00000000-0000-0000-0000-000000000000}'; " +
                   "$($script:Cli) -p '$prompt' --allow-all-tools; echo $($script:DoneMarker)"

        $listener = Start-WtEventListener -App $script:app
        try {
            Send-WtInput -App $script:app -SessionId $paneId -Text $command
            Send-WtKeys  -App $script:app -SessionId $paneId -Keys @('Enter')

            # Prove the action completed before asserting the absence of anything.
            $doneRe = [regex]::Escape($script:DoneMarker)
            Wait-Until -TimeoutSec 240 -IntervalSec 2 -Because "$($script:Cli) to finish" -Condition {
                (Get-WtCapture -App $script:app -SessionId $paneId -MaxLines 200) -match $doneRe
            } | Out-Null

            $out = Get-WtCapture -App $script:app -SessionId $paneId -MaxLines 200
            $out | Should -Match $doneRe -Because 'the CLI must run to completion despite the broken hook bridge'
            $out | Should -Not -Match $script:DenyPattern -Because 'a broken hook bridge must not deny the agent its tools'

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
        # uninstalling Terminal. The CLI is resolved to an absolute path first, so the scrub cannot
        # take the agent itself down with it (Copilot may live in an alias directory too).
        $paneId = (New-WtTab -App $script:app).session_id
        $prompt = 'Run the shell command: echo IT-HOOK-TOOL-RAN'
        $onPathMarker = 'IT-HOOK-BRIDGE-STILL-ON-PATH'
        $command = "`$c=(Get-Command $($script:Cli)).Source; " +
                   "`$env:PATH=((`$env:PATH -split ';') | Where-Object { `$_ -and -not (Test-Path (Join-Path `$_ 'wtcli.exe')) }) -join ';'; " +
                   "if (Get-Command wtcli.exe -ErrorAction SilentlyContinue) { echo $onPathMarker }; " +
                   "& `$c -p '$prompt' --allow-all-tools; echo $($script:DoneMarker)"

        $listener = Start-WtEventListener -App $script:app
        try {
            Send-WtInput -App $script:app -SessionId $paneId -Text $command
            Send-WtKeys  -App $script:app -SessionId $paneId -Keys @('Enter')

            $doneRe = [regex]::Escape($script:DoneMarker)
            Wait-Until -TimeoutSec 240 -IntervalSec 2 -Because "$($script:Cli) to finish" -Condition {
                (Get-WtCapture -App $script:app -SessionId $paneId -MaxLines 200) -match $doneRe
            } | Out-Null

            $out = Get-WtCapture -App $script:app -SessionId $paneId -MaxLines 200
            $out | Should -Match $doneRe -Because 'the CLI must run to completion with no bridge on PATH'
            $out | Should -Not -Match ([regex]::Escape($onPathMarker)) -Because 'the scrub must actually remove wtcli.exe, or this case passes on a working bridge'
            $out | Should -Not -Match $script:DenyPattern -Because 'an uninstalled bridge must not deny the agent its tools'

            # Same bounded negative window as the broken-CLSID case: nothing can have been published.
            @(Get-WtEvents -Listener $listener -Predicate {
                $_.method -eq 'agent_event' -and $_.params.pane_id -eq $paneId
            }).Count | Should -Be 0 -Because 'the bridge was not on PATH, so it must publish nothing'
        }
        finally {
            Stop-WtEventListener -Listener $listener
            try { Close-WtPane -App $script:app -SessionId $paneId } catch { }
        }
    }
}
