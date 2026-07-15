#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §3 "Shell integration and detection" — the two pure-[E2E] items that had
# no automated coverage:
#   * "PowerShell shell integration installed" — a shell-integrated pane emits command-finished
#     OSC 133;D;<exit> marks (the foundation autofix detection relies on).
#   * "Missing shell integration is safe" — a non-integrated shell (cmd.exe) that fails a
#     command does not crash Windows Terminal or break the protocol surface.
#
# Deterministic: no agent/LLM involved — assert on the wtcli event stream + protocol liveness.
#   Invoke-Pester test/e2e/tests -Tag Feature

BeforeDiscovery { $script:Ready = [bool](Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) }

Describe 'Feature §3 Shell integration and detection' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        # autofix OFF: this suite is about the shell-integration marks themselves, not the
        # downstream autofix UI (which is covered by Feature.AutofixPane.Tests.ps1). No agent is
        # pinned — these tests never open the agent pane, so leaving acpAgent at its default avoids
        # an unnecessary copilot install/auth dependency (the pre-warmed helper is irrelevant here).
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ autoFixEnabled = $false }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'PowerShell shell integration emits a command-finished mark on failure (OSC 133;D;<nonzero>)' {
        # The default pane runs the IT PowerShell profile with shell integration enabled, so a
        # failing command must surface an OSC 133;D mark with a non-zero exit code on the stream.
        $pane = Get-ActivePane -App $script:app
        $sid = $pane.session_id
        $listener = Start-WtEventListener -App $script:app
        try {
            Invoke-FailingCommand -App $script:app -SessionId $sid -Command "nope$(Get-Random) status" | Out-Null
            # Scope to the active pane (the event's pane_id == the pane session_id) so an unrelated
            # OSC 133;D failure mark (e.g. from profile/startup activity in another pane) can't
            # satisfy the assertion.
            $ev = Wait-WtCommandFailure -Listener $listener -PaneId $pane.session_id -TimeoutSec 20
            "$($ev.params.sequence)" | Should -Match '(?i)osc:133;D;'
        }
        finally { Stop-WtEventListener -Listener $listener }
    }

    It 'PowerShell shell integration emits a zero-exit mark on success (OSC 133;D;0)' {
        $sid = (Get-ActivePane -App $script:app).session_id
        $listener = Start-WtEventListener -App $script:app
        try {
            Invoke-RunCommand -App $script:app -SessionId $sid -Command "echo ok$(Get-Random)" -SettleSec 8 | Out-Null
            { Wait-WtEvent -Listener $listener -TimeoutSec 20 -Predicate {
                    $_.method -eq 'vt_sequence' -and "$($_.params.sequence)" -match '(?i)osc:133;D;0(\b|;|$)'
                } } | Should -Not -Throw
        }
        finally { Stop-WtEventListener -Listener $listener }
    }

    It 'Missing shell integration is safe (a cmd.exe pane that fails does not crash the terminal)' {
        # cmd.exe has no OSC 133 shell integration, so failures here are NOT detected — the
        # contract is simply that this stays safe: no crash, the error still renders, and the
        # protocol surface keeps responding.
        $tab = New-WtTab -App $script:app -Command 'cmd.exe' -Title 'no-shellint'
        try {
            # Run the bad command, then on a SEPARATE line echo the errorlevel. cmd expands
            # %errorlevel% at PARSE time, so a single-line `badcmd & echo %errorlevel%` captures the
            # OLD value (0) — the echo must be its own command to see the failure's 9009.
            Send-WtInput -App $script:app -SessionId $tab.session_id -Text "nonexistentcmd$(Get-Random)"
            Send-WtKeys -App $script:app -SessionId $tab.session_id -Keys @('Enter')
            Send-WtInput -App $script:app -SessionId $tab.session_id -Text 'echo ITERR=%errorlevel%'
            Send-WtKeys -App $script:app -SessionId $tab.session_id -Keys @('Enter')
            # cmd sets errorlevel 9009 for an unrecognized command, so the pane prints "ITERR=9009".
            # Asserting that is deterministic AND locale-robust — unlike the localized "not
            # recognized" text, and unlike the command name (which the echoed input line contains
            # even if nothing ran). The echoed line shows "%errorlevel%" literally; only the
            # expanded OUTPUT is "ITERR=9009", so this also proves Enter actually executed it.
            Assert-Pane -App $script:app -SessionId $tab.session_id -Match 'ITERR=9009' -TimeoutSec 12
            # The protocol surface is still alive and the WT process did not crash.
            { Invoke-WtCli -App $script:app -Arguments @('list-panes') } | Should -Not -Throw
            (Get-Process -Id $script:app.Pid -ErrorAction SilentlyContinue) | Should -Not -BeNullOrEmpty
        }
        finally { Close-WtPane -App $script:app -SessionId $tab.session_id }
    }
}
