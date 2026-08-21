#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Issue #641: resuming a stored session whose working directory contains non-ASCII
# characters.
#
# `dispatch_resume` (tools/wta/src/app.rs) launches a historical session with
# `wtcli new-tab -c <command> -n <title> -d <cwd>`. `wtcli` used to read that argv from
# the CRT's narrow `__argv`, which the CRT transcodes from the real UTF-16 command line
# through the *process ANSI code page*. `D:\Obsidian\<CJK>` therefore reached the
# terminal as GBK-bytes-decoded-as-UTF-8 mojibake on ACP 936, and as `D:\Obsidian\????`
# on a Latin ACP. Either way `CreateProcessW` received a path that does not exist and
# the pane died with 0x8007010b ERROR_DIRECTORY before the agent CLI ever started.
#
# No unit test can see this. The corruption happens inside the *packaged* wtcli's argv
# decoding and only becomes observable after the string has crossed
# wtcli -> `Bstr()`/`winrt::to_hstring` -> COM BSTR ->
# `TerminalProtocolComServer::CreateTab` -> `ConptyConnection` -> `CreateProcessW`.
# That whole hop is what this suite exercises, against the real deployed package.
#
# Both oracles are deliberately ASCII-only so the assertion itself cannot be distorted
# by the harness reading wtcli's UTF-8 stdout (`Invoke-Native` does not pin
# `StandardOutputEncoding`, so non-ASCII text read back through wtcli would be
# unreliable evidence in either direction):
#
#   1. the `connection_state` event for the new pane reports `connected` and never
#      `failed` -- the exact signal quoted in the issue report;
#   2. the shell running in that pane writes an ASCII-named probe file, and that file
#      has to appear INSIDE the non-ASCII directory. A corrupted starting directory
#      produces no file anywhere (the process never launches); a silently dropped one
#      puts the file in the profile-default directory instead.

BeforeDiscovery {
    $script:Ready = [bool](Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' })
}

Describe 'Feature §4 non-ASCII session working directories' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true

        # One disposable root per run so a failed case never leaks into the next one.
        $script:root = Join-Path ([System.IO.Path]::GetTempPath()) ("ite2e-cwd-{0}" -f [guid]::NewGuid().ToString('N'))

        # Covers both ways the old narrow-argv path lost data: CJK is unrepresentable in
        # a Latin ANSI code page and collapsed to literal '?', while the Latin-1
        # characters survived as single ANSI bytes that are invalid UTF-8 by the time
        # `Bstr()` decodes them. A directory holding both fails under either code page.
        $script:nonAsciiDir = Join-Path $script:root '我的笔记-café-Über'
        $script:asciiDir = Join-Path $script:root 'plain-ascii'
        New-Item -ItemType Directory -Path $script:nonAsciiDir -Force | Out-Null
        New-Item -ItemType Directory -Path $script:asciiDir -Force | Out-Null

        # Mirrors dispatch_resume's launch shape: a `cmd` wrapper, a tab title, and a
        # starting directory. `/k` keeps the pane alive after the probe is written so
        # `New-WtTab`'s pane-status check can still resolve it.
        $script:NewProbeTab = {
            param([string]$Cwd, [string]$Title)

            $probeName = "ite2e-probe-{0}.txt" -f [guid]::NewGuid().ToString('N')
            $tab = New-WtTab -App $script:app `
                -Command "cmd /k echo ITE2E-CWD-OK>$probeName" `
                -Cwd $Cwd `
                -Title $Title

            [pscustomobject]@{
                SessionId = $tab.session_id
                ProbeName = $probeName
                ProbePath = (Join-Path $Cwd $probeName)
            }
        }

        # Deterministic wait for the pane's own shell to land the probe in its cwd.
        $script:WaitForProbe = {
            param([string]$Path, [int]$TimeoutSec = 20)
            Test-Until -TimeoutSec $TimeoutSec -IntervalSec 0.5 -Condition {
                Test-Path -LiteralPath $Path -PathType Leaf
            }
        }

        $script:ConnectionStates = {
            @(Get-WtEvents -Listener $script:listener -Predicate {
                    $_.method -eq 'connection_state' -and
                    "$($_.params.pane_id)".Trim('{}') -eq $script:watchedPaneId
                }) | ForEach-Object { $_.params.state }
        }

        # NOTE: deliberately NO -EventFilter. `wtcli listen --event` matches
        # `params.event` (see wtcli_functions.h::MatchesEventFilter), which only agent
        # event envelopes carry; `connection_state` params hold pane_id/tab_id/state, so
        # any --event value silently discards every one of them. Filter on `method` in
        # the predicates instead.
        $script:listener = Start-WtEventListener -App $script:app

        # `wtcli listen` subscribes asynchronously and never replays what it missed, so a
        # pane created too soon after the listener starts can lose its `connected`.
        # Handshake with throwaway panes until the listener demonstrably delivers, which
        # keeps the measured cases free of that race without a blind sleep.
        $script:warmupPanes = @()
        $script:listenerReady = $false
        foreach ($attempt in 1..5) {
            $script:warmupPanes += (New-WtTab -App $script:app -Command 'cmd /k rem ite2e-listener-warmup').session_id
            $script:listenerReady = Test-Until -TimeoutSec 6 -IntervalSec 0.5 -Condition {
                @(Get-WtEvents -Listener $script:listener -Predicate { $_.method -eq 'connection_state' }).Count -gt 0
            }
            if ($script:listenerReady) { break }
        }
        foreach ($pane in $script:warmupPanes) { Close-WtPane -App $script:app -SessionId $pane }
    }

    AfterAll {
        if ($script:listener) { Stop-WtEventListener -Listener $script:listener }
        if ($script:app) { Stop-Terminal -App $script:app }
        if ($script:root -and (Test-Path -LiteralPath $script:root)) {
            Remove-Item -LiteralPath $script:root -Recurse -Force -ErrorAction SilentlyContinue
        }
    }

    It 'Non-ASCII working directories survive the resume launch path' {
        $script:listenerReady | Should -BeTrue -Because 'the connection_state listener must be delivering events before the measured action'

        $probe = $null
        try {
            $probe = & $script:NewProbeTab -Cwd $script:nonAsciiDir -Title '我的笔记 会话恢复'
            $script:watchedPaneId = "$($probe.SessionId)".Trim('{}')

            # Oracle 1 -- the pane-lifecycle signal the bug report quoted as `failed`.
            # Polled rather than Wait-WtEvent so a timeout reports the observed states
            # instead of throwing a bare "Wait-Until timed out".
            $sawConnected = Test-Until -TimeoutSec 30 -IntervalSec 0.5 -Condition {
                (& $script:ConnectionStates) -contains 'connected'
            }
            $observed = & $script:ConnectionStates

            $sawConnected | Should -BeTrue -Because "the pane must connect instead of failing with ERROR_DIRECTORY; observed connection_state for the pane: [$($observed -join ', ')]"
            $observed | Should -Not -Contain 'failed' -Because 'a non-ASCII starting directory must not be rejected as an invalid directory (issue #641)'

            # Oracle 2 -- the shell really started in that exact directory, rather than
            # merely starting somewhere that happens to connect.
            (& $script:WaitForProbe -Path $probe.ProbePath) | Should -BeTrue -Because "the pane's shell must run inside '$($script:nonAsciiDir)' and write '$($probe.ProbeName)' there"

            # A dropped cwd falls back to the profile default; prove that did not happen.
            $fallback = Join-Path $env:USERPROFILE $probe.ProbeName
            Test-Path -LiteralPath $fallback -PathType Leaf | Should -BeFalse -Because 'the starting directory must be honored, not silently replaced by the profile default'
        }
        finally {
            if ($probe) { Close-WtPane -App $script:app -SessionId $probe.SessionId }
        }
    }

    It 'ASCII working directories still launch unchanged' {
        # Baseline control for the `main` -> `wmain` entry-point change: the ordinary
        # path must be completely unaffected, so a failure here separates "Unicode
        # handling broke" from "argument handling broke".
        $probe = $null
        try {
            $probe = & $script:NewProbeTab -Cwd $script:asciiDir -Title 'ite2e ascii cwd'
            (& $script:WaitForProbe -Path $probe.ProbePath) | Should -BeTrue -Because "an ASCII starting directory must keep working: expected '$($probe.ProbePath)'"
        }
        finally {
            if ($probe) { Close-WtPane -App $script:app -SessionId $probe.SessionId }
        }
    }
}
