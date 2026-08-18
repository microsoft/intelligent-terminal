#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §10 (C190, C268, C269) and §8 (C267) — the native wtcli hook
# bridge must read hook JSON from stdin, preserve the source pane identity,
# redact model/user content, honour the WT_SESSION gate, and keep working when
# the shipped command strings are wrapped in their per-CLI uninstall guards.
#
# Why these live at E2E rather than in a unit test: `BuildAgentHookEventJson` has
# no unit-test project — `src/tools/wtcli` ships only `ft_fuzzer`, which proves
# the builder does not crash and asserts nothing about what it emits. Until that
# gap is closed, the published event is the only place these contracts are
# checked at all.
#
#   Invoke-Pester test/e2e/tests/Feature.HookTrace.Tests.ps1 -Tag Feature

BeforeDiscovery { $script:Ready = [bool](Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) }

Describe 'Feature §10 native hook bridge' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true

        # Prefer the bundle inside the installed package: that is the artifact that
        # actually ships, so a guard that landed in source but never made it into the
        # package still fails here. Fall back to the repo copy for a source-only run.
        $script:BundleRoot = $null
        if ($script:app.InstallLocation -and (Test-Path (Join-Path $script:app.InstallLocation 'wt-agent-hooks'))) {
            $script:BundleRoot = Join-Path $script:app.InstallLocation 'wt-agent-hooks'
        }
        else {
            $src = Join-Path $PSScriptRoot '..\..\..\tools\wta\wt-agent-hooks'
            if (Test-Path $src) { $script:BundleRoot = (Resolve-Path $src).Path }
        }
        Write-ItLog -Level INFO -Message "HookTrace: bundle root = $($script:BundleRoot)"

        $script:BundleRel = @{
            copilot = 'copilot\wt-agent-hooks\hooks\hooks.json'
            claude  = 'claude\wt-agent-hooks\hooks\hooks.json'
            gemini  = 'gemini-extension\hooks\hooks.json'
            codex   = 'codex\wt-agent-hooks\hooks\hooks.json'
        }

        # Claude pins its hooks to bash (`"shell": "bash"`); without one installed the
        # claude row cannot be exercised, so it is reported instead of silently passing.
        $script:GitBash = @(
            'C:\Program Files\Git\bin\bash.exe'
            'C:\Program Files (x86)\Git\bin\bash.exe'
        ) | Where-Object { Test-Path $_ } | Select-Object -First 1

        function script:Get-ShippedHook {
            <# The hook object a CLI ships for one WTA topic, straight out of hooks.json. #>
            param([string]$Cli, [string]$Event)
            $path = Join-Path $script:BundleRoot $script:BundleRel[$Cli]
            if (-not (Test-Path $path)) { throw "missing hook bundle for '$Cli': $path" }
            $json = Get-Content -Raw -LiteralPath $path | ConvertFrom-Json
            # Guarded forms terminate the event name with ';' (bash) or ' }' (PowerShell
            # try-block), so the delimiter class must cover both, not just whitespace.
            $needle = '--event ' + [regex]::Escape($Event) + '(?=[\s;]|$)'
            foreach ($topic in $json.hooks.PSObject.Properties) {
                foreach ($matcher in $topic.Value) {
                    foreach ($h in $matcher.hooks) {
                        foreach ($field in @('command', 'powershell', 'bash')) {
                            if ($h.$field -and $h.$field -match $needle) { return $h }
                        }
                    }
                }
            }
            throw "no shipped hook for '$Cli' / '$Event'"
        }

        function script:New-HookInvocation {
            <#
            .SYNOPSIS
                Pane command that pipes a payload file into a CLI's SHIPPED hook string,
                executed by the shell that CLI really dispatches hooks through.
            .DESCRIPTION
                Running the string in the wrong shell is not a neutral simplification: the
                guards are shell-specific (`try/catch` is a parse error in bash, `command -v`
                is not a PowerShell command), so a mismatched shell tests a spelling nobody
                ships. Returns $null when the required shell is unavailable.
            #>
            param([string]$Cli, [string]$Event, [string]$PayloadFile, [ValidateSet('auto', 'bash', 'powershell')][string]$Variant = 'auto')
            $h = script:Get-ShippedHook -Cli $Cli -Event $Event
            switch ($Variant) {
                'bash' { $cmd = if ($h.bash) { $h.bash } else { $h.command }; $shell = 'bash' }
                'powershell' { $cmd = if ($h.powershell) { $h.powershell } else { $h.command }; $shell = 'pwsh' }
                default {
                    if ($h.shell -eq 'bash') { $cmd = $h.command; $shell = 'bash' }
                    elseif ($h.powershell) { $cmd = $h.powershell; $shell = 'pwsh' }
                    else { $cmd = $h.command; $shell = 'pwsh' }
                }
            }
            if ($cmd.Contains("'")) { throw "shipped command contains a single quote, which this driver's quoting cannot carry: $cmd" }
            $feed = "Get-Content -Raw -LiteralPath '$PayloadFile' | "
            if ($shell -eq 'bash') {
                if (-not $script:GitBash) { return $null }
                return $feed + "& '$($script:GitBash)' -c '$cmd'"
            }
            $feed + "& pwsh -NoProfile -Command '$cmd'"
        }

        function script:Write-HookPayload {
            <# Payloads travel by file so no assertion depends on quoting surviving Send-WtInput. #>
            param([string]$Name, [string]$Json, [string]$Dir)
            $p = Join-Path $Dir "$Name.json"
            [System.IO.File]::WriteAllText($p, $Json)
            $p
        }
    }

    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Native hook bridge publishes events (wtcli agent-hook publishes a pane-scoped, redacted agent event)' {
        $paneId = (Get-ActivePane -App $script:app).session_id
        $agentSessionId = "native-hook-$([guid]::NewGuid())"
        $secret = 'must-not-cross-the-hook-bridge'
        # Every field the bridge is supposed to drop carries the SAME sentinel, so one
        # substring check proves none of them leaked by any route — including nested
        # inside a field the redactor forgot to walk.
        $payload = @{
            session_id      = $agentSessionId
            cwd             = 'C:\native-hook-test'
            prompt          = $secret
            tool_result     = $secret
            transcript_path = $secret
            messages        = @($secret)
            model           = $secret
        } | ConvertTo-Json -Compress
        $command = "'$payload' | wtcli.exe agent-hook --cli-source copilot --event agent.prompt.submit"

        $listener = Start-WtEventListener -App $script:app
        try {
            Invoke-RunCommand -App $script:app -SessionId $paneId -Command $command -SettleSec 3 | Out-Null
            $event = Wait-WtEvent -Listener $listener -TimeoutSec 20 -Predicate {
                $_.method -eq 'agent_event' -and
                $_.params.agent_session_id -eq $agentSessionId
            }
            $event.params.event | Should -Be 'agent.prompt.submit'
            $event.params.pane_id | Should -Be $paneId
            $event.params.cli_source | Should -Be 'copilot'
            $event.params.payload.cwd | Should -Be 'C:\native-hook-test'

            $kept = @($event.params.payload.PSObject.Properties.Name)
            foreach ($dropped in @('prompt', 'tool_result', 'transcript_path', 'messages', 'model')) {
                $kept | Should -Not -Contain $dropped -Because "'$dropped' carries model or user content and must not cross the bridge"
            }
            ($event.params.payload | ConvertTo-Json -Depth 10 -Compress) |
                Should -Not -Match ([regex]::Escape($secret)) -Because 'no redacted value may survive anywhere in the payload'
        }
        finally {
            Stop-WtEventListener -Listener $listener
        }
    }

    It 'Hook payload keeps interactive tool input (tool_input is dropped for ordinary tools and kept for ask_user)' {
        # A conditional retention rule fails in both directions: drop too much and the
        # proposal UI loses the question it must render; drop too little and every shell
        # command an agent runs is published to any listener.
        $paneId = (Get-ActivePane -App $script:app).session_id
        $ordinaryId = "tool-drop-$([guid]::NewGuid())"
        $interactiveId = "tool-keep-$([guid]::NewGuid())"
        $secret = 'rm -rf / --no-preserve-root'

        $ordinary = script:Write-HookPayload -Name 'tool-ordinary' -Dir $TestDrive -Json (@{
                session_id = $ordinaryId; tool_name = 'bash'; tool_input = @{ command = $secret }
            } | ConvertTo-Json -Compress)
        $interactive = script:Write-HookPayload -Name 'tool-interactive' -Dir $TestDrive -Json (@{
                session_id = $interactiveId; tool_name = 'ask_user'; tool_input = @{ question = 'continue?' }
            } | ConvertTo-Json -Compress)

        $listener = Start-WtEventListener -App $script:app
        try {
            foreach ($f in @($ordinary, $interactive)) {
                $cmd = "Get-Content -Raw -LiteralPath '$f' | wtcli.exe agent-hook --cli-source copilot --event agent.tool.starting"
                Invoke-RunCommand -App $script:app -SessionId $paneId -Command $cmd -SettleSec 3 | Out-Null
            }

            $dropEvent = Wait-WtEvent -Listener $listener -TimeoutSec 20 -Predicate {
                $_.method -eq 'agent_event' -and $_.params.agent_session_id -eq $ordinaryId
            }
            @($dropEvent.params.payload.PSObject.Properties.Name) |
                Should -Not -Contain 'tool_input' -Because 'an ordinary tool call must not publish its arguments'
            ($dropEvent.params.payload | ConvertTo-Json -Depth 10 -Compress) |
                Should -Not -Match ([regex]::Escape($secret)) -Because 'the dropped arguments must not survive by another route'
            $dropEvent.params.payload.tool_name | Should -Be 'bash' -Because 'the tool name itself is not sensitive and drives routing'

            $keepEvent = Wait-WtEvent -Listener $listener -TimeoutSec 20 -Predicate {
                $_.method -eq 'agent_event' -and $_.params.agent_session_id -eq $interactiveId
            }
            $keepEvent.params.payload.tool_input.question |
                Should -Be 'continue?' -Because 'ask_user input is the payload the interactive prompt UI renders'
        }
        finally {
            Stop-WtEventListener -Listener $listener
        }
    }

    It 'Hook bridge ignores shells outside Terminal (no WT_SESSION means no published event)' {
        # The bridge is reachable from any shell on the machine once wtcli is on PATH.
        # WT_SESSION is what separates "a hook fired inside a Terminal pane" from "some
        # unrelated process piped JSON at us", so losing the gate would let any process
        # inject pane-attributed agent events.
        $paneId = (New-WtTab -App $script:app).session_id
        $gatedId = "gated-$([guid]::NewGuid())"
        $controlId = "ungated-$([guid]::NewGuid())"

        $gated = script:Write-HookPayload -Name 'gated' -Dir $TestDrive -Json (@{ session_id = $gatedId } | ConvertTo-Json -Compress)
        $control = script:Write-HookPayload -Name 'ungated' -Dir $TestDrive -Json (@{ session_id = $controlId } | ConvertTo-Json -Compress)

        $listener = Start-WtEventListener -App $script:app
        try {
            $gatedCmd = "`$saved=`$env:WT_SESSION; `$env:WT_SESSION=''; " +
            "Get-Content -Raw -LiteralPath '$gated' | wtcli.exe agent-hook --cli-source copilot --event agent.stop; " +
            '"GATED_EXIT=$LASTEXITCODE"'
            $out = Invoke-RunCommand -App $script:app -SessionId $paneId -Command $gatedCmd -SettleSec 10
            $out | Should -Match 'GATED_EXIT=0' -Because 'a gated hook must still succeed, or a fail-closed CLI would break'

            # Positive control: the same pane with the gate restored. Waiting for THIS
            # event is what makes the negative assertion below sound — it proves the
            # listener was live and that enough time passed for a gated event to appear.
            $controlCmd = "`$env:WT_SESSION=`$saved; " +
            "Get-Content -Raw -LiteralPath '$control' | wtcli.exe agent-hook --cli-source copilot --event agent.stop"
            Invoke-RunCommand -App $script:app -SessionId $paneId -Command $controlCmd -SettleSec 10 | Out-Null
            $controlEvent = $null
            try {
                $controlEvent = Wait-WtEvent -Listener $listener -TimeoutSec 30 -Predicate {
                    $_.method -eq 'agent_event' -and $_.params.agent_session_id -eq $controlId
                }
            }
            catch { }
            $controlEvent | Should -Not -BeNullOrEmpty -Because 'restoring WT_SESSION must restore publishing, or this case proves nothing'

            @(Get-WtEvents -Listener $listener -Predicate {
                    $_.method -eq 'agent_event' -and $_.params.agent_session_id -eq $gatedId
                }).Count | Should -Be 0 -Because 'a hook fired without WT_SESSION must publish nothing'
        }
        finally {
            Stop-WtEventListener -Listener $listener
            try { Close-WtPane -App $script:app -SessionId $paneId } catch { }
        }
    }

    It 'Hook events stay inside their broadcast budget (an oversized routing field cannot escape the cap)' {
        # The bridge caps the SERIALIZED envelope, not just the payload, because
        # every subscriber has to budget a queue for whatever arrives. Truncation
        # only ever rewrites `payload`, so an oversized routing field — and
        # `agent_session_id` is read straight out of the hook JSON on stdin — is
        # not something payload truncation can fix. Either the whole envelope
        # comes in under budget, or nothing is published.
        $paneId = (Get-ActivePane -App $script:app).session_id
        $marker = "boundprobe-$([guid]::NewGuid())"
        # Far past the 8192-char budget, and oversized in the one field
        # truncation cannot reach.
        $oversizedId = $marker + ('S' * 200000)
        $payload = script:Write-HookPayload -Name 'oversized-routing' -Dir $TestDrive -Json (@{
                session_id = $oversizedId
                cwd        = 'C:\bound-test'
            } | ConvertTo-Json -Compress)

        $listener = Start-WtEventListener -App $script:app
        try {
            $cmd = "Get-Content -Raw -LiteralPath '$payload' | wtcli.exe agent-hook --cli-source copilot --event agent.session.start; " +
            '"BOUND" + "_EXIT=$LASTEXITCODE"'
            $out = Invoke-RunCommand -App $script:app -SessionId $paneId -Command $cmd -SettleSec 20
            $out | Should -Match 'BOUND_EXIT=0' -Because 'the bridge must never fail its CLI, whatever it decides to publish'

            # A control event proves the listener was live, so "no oversized
            # event arrived" means it was dropped rather than merely missed.
            $controlId = "bound-control-$([guid]::NewGuid())"
            $control = script:Write-HookPayload -Name 'bound-control' -Dir $TestDrive -Json (@{ session_id = $controlId } | ConvertTo-Json -Compress)
            Invoke-RunCommand -App $script:app -SessionId $paneId -SettleSec 20 `
                -Command "Get-Content -Raw -LiteralPath '$control' | wtcli.exe agent-hook --cli-source copilot --event agent.session.start" | Out-Null
            $controlEvent = $null
            try {
                $controlEvent = Wait-WtEvent -Listener $listener -TimeoutSec 30 -Predicate {
                    $_.method -eq 'agent_event' -and $_.params.agent_session_id -eq $controlId
                }
            }
            catch { }
            $controlEvent | Should -Not -BeNullOrEmpty -Because 'the listener must be live, or the assertion below proves nothing'

            $oversized = @(Get-WtEvents -Listener $listener -Predicate {
                    $_.method -eq 'agent_event' -and $_.params.agent_session_id -like "$marker*"
                })
            foreach ($e in $oversized) {
                ($e | ConvertTo-Json -Depth 20 -Compress).Length |
                    Should -BeLessOrEqual 8192 -Because 'a published envelope must fit the budget its own truncation promises'
            }
        }
        finally {
            Stop-WtEventListener -Listener $listener
        }
    }

    It 'Shipped hook guards do not swallow the happy path (every CLI bundle still delivers with the bridge present)' {
        # Each CLI wraps the bridge call in a guard so an uninstalled Terminal cannot break
        # it: PowerShell try/catch (copilot, gemini), bash `command -v` (copilot, claude),
        # nothing (codex, whose marketplace entry is removed with the package).
        #
        # Those guards make the exit code useless as an oracle — they force 0 whether the
        # command ran, failed, or was never parsed as a command at all. A typo inside the
        # try-block would look exactly like success. Only an event arriving at Terminal
        # tells the two apart, which is why this asserts delivery and not exit status
        # alone. Unit tests cover the mirror case (guards exit 0 when wtcli is missing);
        # nothing else covers "the guard still lets a working bridge through".
        $paneId = (Get-ActivePane -App $script:app).session_id
        $cases = @(
            @{ Cli = 'copilot'; Variant = 'powershell'; Label = 'copilot (PowerShell try/catch)' }
            @{ Cli = 'copilot'; Variant = 'bash'; Label = 'copilot (bash command -v)' }
            @{ Cli = 'claude'; Variant = 'auto'; Label = 'claude (shell-pinned bash guard)' }
            @{ Cli = 'gemini'; Variant = 'auto'; Label = 'gemini (PowerShell try/catch)' }
            @{ Cli = 'codex'; Variant = 'auto'; Label = 'codex (unguarded)' }
        )

        $listener = Start-WtEventListener -App $script:app
        try {
            foreach ($c in $cases) {
                $sid = "shipped-$($c.Cli)-$($c.Variant)-$([guid]::NewGuid())"
                $file = script:Write-HookPayload -Name "shipped-$($c.Cli)-$($c.Variant)" -Dir $TestDrive -Json (@{ session_id = $sid } | ConvertTo-Json -Compress)
                $invocation = script:New-HookInvocation -Cli $c.Cli -Event 'agent.session.start' -PayloadFile $file -Variant $c.Variant
                if (-not $invocation) {
                    Set-ItResult -Inconclusive -Because "no Git Bash on this machine, so the bash-dispatched bundle ($($c.Label)) cannot be exercised"
                    return
                }

                $out = Invoke-RunCommand -App $script:app -SessionId $paneId -Command ($invocation + '; "SHIPPED_EXIT=$LASTEXITCODE"') -SettleSec 15
                $out | Should -Match 'SHIPPED_EXIT=0' -Because "the shipped hook for $($c.Label) must never fail its CLI"

                # Wait-WtEvent throws on timeout, which would surface as a bare "timed out"
                # with no hint of WHICH bundle broke. Catching it lets the assertion below
                # name the CLI — the only thing that makes this loop debuggable.
                $delivered = $null
                try {
                    $delivered = Wait-WtEvent -Listener $listener -TimeoutSec 30 -Predicate {
                        $_.method -eq 'agent_event' -and $_.params.agent_session_id -eq $sid
                    }
                }
                catch { }
                $delivered | Should -Not -BeNullOrEmpty -Because "the guard around $($c.Label) must not swallow a working bridge call"
            }
        }
        finally {
            Stop-WtEventListener -Listener $listener
        }
    }
}
