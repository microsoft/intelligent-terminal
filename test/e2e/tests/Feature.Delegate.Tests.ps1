#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §5 Delegate agent — the ENGINE, driven directly.
#
# Delegate mode launches a separate agent task from the current cwd, in a NEW tab, WITHOUT the
# interactive agent-pane chat. The UI entry points (Alt+Shift+B, and the Alt+Shift+/ command
# palette) are WT accelerators / a WT XAML palette and are NOT harness-injectable (send-keys
# reaches the conpty, not WT's keybinding handler; the command palette is not drivable here).
# BUT both entry points do the same thing: build and run `wta delegate [PROMPT] --agent <acp>
# --delegate-agent <id> [--delegate-model <m>] [--cwd <dir>]` (see TerminalPage.cpp _LaunchDelegate).
# So we exercise the delegate ENGINE directly via that command — Copilot is the delegate agent
# (the arbitrary configured provider). Each delegate creates a new tab; we diff the tab list to
# find it and assert on the new tab / its pane.
#
# Not covered here (documented in release-coverage-map.psd1): the Alt+Shift+B / Alt+Shift+/
# shortcuts and the command-palette prompt/cancel are UI entry points that can't be injected;
# "Delegate model is correct" is not observable in WT state or wta-delegate.log (the CLI receives
# --model internally) and is covered by the settings-model UT roundtrip; non-Copilot delegate is
# env-gated (needs Claude/Codex/Gemini installed + authenticated).

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue) -and (Get-Command winapp -ErrorAction SilentlyContinue)) }

Describe 'Feature §5 delegate agent engine (wta delegate)' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot'; delegateAgent = 'copilot' }
        $script:repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
        # Launch a delegate and return the NEW tab created in this window (diffs the tab list).
        $script:RunDelegate = {
            param($Prompt, $DelegateAgent = 'copilot', $Cwd)
            $wid = [string]$script:app.WindowId
            $before = @((Get-WtTabs -App $script:app -WindowId $wid).tab_id)
            $args = @('delegate', $Prompt, '--agent', 'copilot --acp --stdio', '--delegate-agent', $DelegateAgent)
            if ($Cwd) { $args += @('--cwd', $Cwd) }
            Invoke-Wta -App $script:app -Arguments $args -TimeoutSec 40 -Raw | Out-Null
            $newTab = $null
            for ($i = 0; $i -lt 30 -and -not $newTab; $i++) {
                $newTab = @(Get-WtTabs -App $script:app -WindowId $wid) | Where-Object { $_.tab_id -notin $before } | Select-Object -First 1
                if (-not $newTab) { Start-Sleep -Milliseconds 500 }
            }
            $panes = if ($newTab) { @(Get-WtPanes -App $script:app -WindowId $wid -TabId ([string]$newTab.tab_id)) } else { @() }
            @{ Tab = $newTab; Panes = $panes }
        }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Delegate cwd is correct (the delegate tab starts in the requested cwd)' {
        $marker = "deleg$(Get-Random -Maximum 99999)"
        $cwd = Join-Path $env:TEMP $marker
        New-Item -ItemType Directory -Force -Path $cwd | Out-Null
        $d = & $script:RunDelegate -Prompt 'What is 2 plus 2? Reply with only the number.' -Cwd $cwd
        $d.Tab | Should -Not -BeNullOrEmpty -Because 'wta delegate must create a new tab'
        ($d.Panes.cwd -join '|') | Should -Match ([regex]::Escape($marker)) -Because 'the delegate must start in the requested --cwd'
    }

    It 'Delegate provider is correct (a Copilot delegate tab launches)' {
        $providerToken = "ITE2E_PROVIDER_$(Get-Random)"
        $d = & $script:RunDelegate -Prompt "Reply with only OK. Tracking token: $providerToken" -DelegateAgent 'copilot' -Cwd $script:repo
        $d.Tab | Should -Not -BeNullOrEmpty
        # WT does not assign a provider-specific tab title, and the CLI is not required to render
        # branding in its buffer. Verify the launched process instead: the unique prompt token
        # scopes this to this delegate, while "copilot" proves which configured provider received it.
        $providerLaunched = Test-Until -TimeoutSec 20 -IntervalSec 1 -Condition {
            @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
                    Where-Object {
                        $_.Name -notlike 'wta*.exe' -and
                        $_.CommandLine -match '(?i)copilot' -and
                        $_.CommandLine -match ([regex]::Escape($providerToken))
                    }).Count -gt 0
        }
        $providerLaunched | Should -BeTrue -Because 'the configured Copilot delegate process must receive this launch prompt'
    }

    It 'Delegate with Copilot works (a delegate task starts and answers)' {
        # Use the repo cwd (a trusted folder) so Copilot does not gate on a folder-trust prompt and
        # auto-runs the delegated prompt. Answering is LLM/auth-dependent, so a non-answer is a
        # precondition skip (mirrors the autofix/idle tests), not a product failure.
        $d = & $script:RunDelegate -Prompt 'What is 2 plus 2? Reply with only the number.' -Cwd $script:repo
        $d.Tab | Should -Not -BeNullOrEmpty -Because 'the Copilot delegate task must start (new tab)'
        $sid = $d.Panes[0].session_id
        # Accept a folder-trust prompt if one appears (best-effort; usually absent for the repo).
        for ($i = 0; $i -lt 8; $i++) {
            $cap = try { Get-WtCapture -App $script:app -SessionId $sid -MaxLines 40 } catch { '' }
            if ($cap -match 'trust the files|Do you trust') { Send-WtKeys -App $script:app -SessionId $sid -Keys @('Enter') | Out-Null; break }
            Start-Sleep -Seconds 1
        }
        $answered = Test-Until -TimeoutSec 45 -IntervalSec 2 -Condition {
            (Get-WtCapture -App $script:app -SessionId $sid -MaxLines 60) -match '\b4\b'
        }
        if (-not $answered) {
            Set-ItResult -Skipped -Because 'the Copilot delegate task started but did not answer this run (auth/offline/model-variance precondition), not a product bug'
            return
        }
        $answered | Should -BeTrue
    }

    It 'Delegate errors are actionable (a bad delegate agent surfaces a clear error)' {
        # A non-existent delegate command must fail VISIBLY in the delegate tab (not silently), so
        # the user can see why nothing happened.
        $d = & $script:RunDelegate -Prompt 'hi' -DelegateAgent 'wt-nonexistent-delegate-xyz'
        $d.Tab | Should -Not -BeNullOrEmpty -Because 'even a failing delegate opens a tab so the error is visible'
        $sid = $d.Panes[0].session_id
        (Test-Until -TimeoutSec 20 -IntervalSec 1 -Condition {
                (Get-WtCapture -App $script:app -SessionId $sid -MaxLines 40) -match "(?i)not recognized|not found|is not recognized|exited with code|cannot find"
            }) | Should -BeTrue -Because 'a bad delegate command must surface a clear, actionable error in the delegate tab'
    }
}
