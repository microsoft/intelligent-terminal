#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeAll {
    . (Join-Path $PSScriptRoot '..\tools\PaneContextBenchmark.ps1')
}

Describe 'Pane-context benchmark statistics and Unicode' -Tag 'Unit' {
    It 'Uses nearest-rank p50 and p95 without interpolation' {
        Get-PcbPercentile -Values (1..40) -Quantile 0.5 | Should -Be 20
        Get-PcbPercentile -Values (1..40) -Quantile 0.95 | Should -Be 38
        Get-PcbPercentile -Values @(8) -Quantile 0.95 | Should -Be 8
    }

    It 'Computes speedup and reduction from path percentiles' {
        $result = Get-PcbComparison -Legacy @(40, 10, 30, 20) -Consolidated @(20, 5, 15, 10)
        $result.SamplesPerPath | Should -Be 4
        $result.LegacyP50Ms | Should -Be 20
        $result.ConsolidatedP95Ms | Should -Be 20
        $result.P50Speedup | Should -Be 2
        $result.P95ReductionPercent | Should -Be 50
    }

    It 'Reports regressions rather than clamping improvement to zero' {
        $result = Get-PcbComparison -Legacy @(10) -Consolidated @(20)
        $result.P50Speedup | Should -Be 0.5
        $result.P50ReductionPercent | Should -Be -100
    }

    It 'Rejects unpaired or invalid timings' {
        { Get-PcbComparison @(1, 2) @(1) } | Should -Throw '*paired*'
        { Get-PcbComparison @(0) @(1) } | Should -Throw '*positive*'
        { Get-PcbComparison @([double]::NaN) @(1) } | Should -Throw '*finite*'
    }

    It 'Preserves supplementary characters at the 4000-scalar prefix boundary' {
        $emoji = [char]::ConvertFromUtf32(0x1F366)
        $bounded = Limit-PcbPrompt (('a' * 3999) + $emoji + 'z')
        $bounded.Content | Should -Be (('a' * 3999) + $emoji)
        Get-PcbScalarCount $bounded.Content | Should -Be 4000
        $bounded.Prompt | Should -Be ($bounded.Content + "`n...<truncated>")
        $bounded.Truncated | Should -BeTrue
    }

    It 'Keeps protocol truncation visible without duplicating its suffix' {
        (Limit-PcbPrompt 'short' 4000 $true).Prompt | Should -Be "short`n...<truncated>"
        (Limit-PcbPrompt 'short...<truncated>' 4000 $true).Prompt | Should -Be 'short...<truncated>'
        (Limit-PcbPrompt 'short').Prompt | Should -Be 'short'
        (Limit-PcbPrompt '').Content | Should -Be ''
    }

    It 'Rejects unpaired surrogates instead of measuring damaged payloads' {
        { Get-PcbScalarCount ([string][char]0xD800) } | Should -Throw '*surrogate*'
    }
}

Describe 'Pane-context benchmark collector request fidelity' -Tag 'Unit' {
    BeforeEach {
        $script:paneId = '21a19cef-d793-405b-b6c7-094162071111'
        $script:pane = @{
            session_id = $script:paneId; tab_id = 3; window_id = 5; pid = 42
            cwd = 'C:\workspace'; shell = 'pwsh'; is_agent_pane = $false
        }
        $script:ctx = @{ Requests = [Collections.Generic.List[object]]::new() }
        $script:marked = $true
        $script:text = 'marker'
        $script:newTruncated = $false
        $script:newLineCount = 1
        $script:newReason = ''
        Mock Invoke-PcbRequest {
            param($Context, $Arguments)
            $Context.Requests.Add([pscustomobject]@{
                Command = $Arguments -join ' '; BoundaryMs = 5; StdoutBytes = 100; StderrBytes = 0
            })
            switch ($Arguments[0]) {
                'active-pane' { return $script:pane.Clone() }
                'list-windows' { return @{ windows = @(@{ window_id = 5 }) } }
                'list-tabs' { return @{ tabs = @(@{ tab_id = 3 }) } }
                'list-panes' { return @{ panes = @($script:pane.Clone()) } }
                'capture-pane' {
                    $content = if (-not $script:marked -and $Arguments -contains '--last-prompt') { '' } else { $script:text }
                    return @{ session_id = $script:paneId; has_marks = $script:marked; content = $content; truncated = $true }
                }
                'get-pane-context' {
                    return @{
                        pane = $script:pane.Clone(); content = $script:text; has_marks = $script:marked
                        output_source = $(if ($script:marked) { 'last_command' } else { 'buffer_tail' })
                        fallback_reason = $script:newReason; line_count = $script:newLineCount; truncated = $script:newTruncated
                    }
                }
                default { throw "Unexpected request: $($Arguments -join ' ')" }
            }
        }
    }

    It 'Uses active then marks for the actual HEAD planner, without a capability probe' {
        $result = Invoke-PcbCollector $script:ctx Legacy Planner $script:paneId
        $result.RequestCount | Should -Be 2
        $result.Requests[0].Command | Should -Be 'active-pane'
        $result.Requests[1].Command | Should -Be "capture-pane -t $script:paneId --last-prompt"
        $result.Truncated | Should -BeFalse -Because 'legacy ignores the server truncation flag'
        $result.BoundaryMs | Should -Be 10
    }

    It 'Preserves planner and manual-fix fallback line budgets' {
        $script:marked = $false
        foreach ($case in @(@('Planner', 24), @('ManualFix', 30))) {
            $result = Invoke-PcbCollector $script:ctx Legacy $case[0] $script:paneId
            $result.RequestCount | Should -Be 3
            $result.Requests[2].Command | Should -Be "capture-pane -t $script:paneId -l $($case[1])"
            $result.OutputSource | Should -Be 'buffer_tail'
        }
    }

    It 'Retains the pre-change explicit autofix active query before exact-source enumeration' {
        $result = Invoke-PcbCollector $script:ctx Legacy ExplicitAutofix $script:paneId
        $result.RequestCount | Should -Be 5
        $result.Requests.Command | Should -Be @(
            'active-pane', 'list-windows', 'list-tabs -w 5', 'list-panes -w 5 -t 3',
            "capture-pane -t $script:paneId --last-prompt"
        )
    }

    It 'Adds exactly one unmarked buffer fallback for explicit autofix' {
        $script:marked = $false
        $result = Invoke-PcbCollector $script:ctx Legacy ExplicitAutofix $script:paneId
        $result.RequestCount | Should -Be 6
        $result.Requests[-1].Command | Should -Be "capture-pane -t $script:paneId -l 30"
    }

    It 'Sends one consolidated subprocess with default resolution or explicit source as appropriate' {
        foreach ($mode in @('Planner', 'ManualFix', 'ExplicitAutofix')) {
            $result = Invoke-PcbCollector $script:ctx Consolidated $mode $script:paneId
            $lines = if ($mode -eq 'Planner') { 24 } else { 30 }
            $suffix = if ($mode -eq 'ExplicitAutofix') { " --target $script:paneId" } else { '' }
            $result.RequestCount | Should -Be 1
            $result.Requests[0].Command | Should -Be "get-pane-context --max-lines $lines --max-chars 4000$suffix"
        }
    }

    It 'Does not retrofit the new marked line cap into the baseline' {
        $script:text = (1..35) -join "`n"
        (Invoke-PcbCollector $script:ctx Legacy Planner $script:paneId).ContentLines | Should -Be 35
        $script:newLineCount = 35
        { Invoke-PcbCollector $script:ctx Consolidated Planner $script:paneId } | Should -Throw '*budget*'
    }

    It 'Rejects a different active pane instead of silently benchmarking another target' {
        { Invoke-PcbCollector $script:ctx Legacy Planner 'wrong-pane' } | Should -Throw '*wrong*'
        { Invoke-PcbCollector $script:ctx Consolidated Planner 'wrong-pane' } | Should -Throw '*wrong*'
    }

    It 'Rejects protocol errors and inconsistent payload metadata' {
        $script:newReason = 'last_command_error'
        $script:marked = $false
        { Invoke-PcbCollector $script:ctx Consolidated ManualFix $script:paneId } | Should -Throw '*fallback*'
        $script:newReason = 'marks_unavailable'
        $script:newLineCount = 2
        { Invoke-PcbCollector $script:ctx Consolidated ManualFix $script:paneId } | Should -Throw '*line count*'
    }

    It 'Does not turn a failed legacy RPC into a successful fallback sample' {
        Mock Invoke-PcbRequest { throw 'RPC failed' }
        { Invoke-PcbCollector $script:ctx Legacy Planner $script:paneId } | Should -Throw '*RPC failed*'
    }
}
