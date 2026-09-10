# Pure statistics and the read-only wtcli transport used by Measure-PaneContext.ps1.

function Get-PcbPercentile {
    param([Parameter(Mandatory)][double[]]$Values, [ValidateRange(0.01, 1)][double]$Quantile)
    if (-not $Values.Count) { throw 'A percentile needs at least one sample.' }
    $sorted = @($Values | Sort-Object)
    $sorted[[int][Math]::Ceiling($Quantile * $sorted.Count) - 1]
}

function Get-PcbComparison {
    param([Parameter(Mandatory)][double[]]$Legacy, [Parameter(Mandatory)][double[]]$Consolidated)
    if ($Legacy.Count -ne $Consolidated.Count -or -not $Legacy.Count) { throw 'Expected nonempty paired samples.' }
    if (@($Legacy + $Consolidated | Where-Object { $_ -le 0 -or -not [double]::IsFinite($_) }).Count) {
        throw 'Durations must be finite and positive.'
    }
    $old50 = Get-PcbPercentile $Legacy 0.5
    $new50 = Get-PcbPercentile $Consolidated 0.5
    $old95 = Get-PcbPercentile $Legacy 0.95
    $new95 = Get-PcbPercentile $Consolidated 0.95
    [pscustomobject]@{
        SamplesPerPath = $Legacy.Count
        LegacyP50Ms = $old50
        ConsolidatedP50Ms = $new50
        P50Speedup = $old50 / $new50
        P50ReductionPercent = 100 * (1 - $new50 / $old50)
        LegacyP95Ms = $old95
        ConsolidatedP95Ms = $new95
        P95Speedup = $old95 / $new95
        P95ReductionPercent = 100 * (1 - $new95 / $old95)
    }
}

function Get-PcbScalarCount {
    param([AllowEmptyString()][string]$Text)
    if ($Text -match '[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]') {
        throw 'Content contains an unpaired UTF-16 surrogate.'
    }
    [regex]::Matches($Text, '[\uD800-\uDBFF][\uDC00-\uDFFF]|[\s\S]').Count
}

function Limit-PcbPrompt {
    param([AllowEmptyString()][string]$Text, [int]$MaxChars = 4000, [bool]$ProtocolTruncated = $false)
    $count = Get-PcbScalarCount $Text
    $content = $Text
    if ($count -gt $MaxChars) {
        $scalars = [regex]::Matches($Text, '[\uD800-\uDBFF][\uDC00-\uDFFF]|[\s\S]')
        $end = $scalars[$MaxChars - 1]
        $content = $Text.Substring(0, $end.Index + $end.Length)
    }
    $prompt = $content
    # Match truncate_for_prompt / preserve_protocol_truncation, including the suffix budget.
    if ($count -gt $MaxChars -or ($ProtocolTruncated -and -not $content.EndsWith('...<truncated>'))) {
        $prompt += "`n...<truncated>"
    }
    [pscustomobject]@{ Content = $content; Prompt = $prompt; Truncated = ($count -gt $MaxChars -or $ProtocolTruncated) }
}

function Get-PcbTextHash {
    param([AllowEmptyString()][string]$Text)
    [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($Text)))
}

function Invoke-PcbProcess {
    param(
        [Parameter(Mandatory)][string]$FilePath,
        [Parameter(Mandatory)][string[]]$Arguments,
        [ValidateRange(1, 120)][int]$TimeoutSec = 20,
        [hashtable]$Environment = @{}
    )
    $psi = [Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $FilePath
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.StandardOutputEncoding = [Text.UTF8Encoding]::new($false, $true)
    $psi.StandardErrorEncoding = [Text.UTF8Encoding]::new($false, $true)
    foreach ($argument in $Arguments) { $psi.ArgumentList.Add($argument) }
    foreach ($key in $Environment.Keys) { $psi.Environment[$key] = $Environment[$key] }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $psi
    $clock = [Diagnostics.Stopwatch]::new()
    try {
        $clock.Start()
        if (-not $process.Start()) { throw "Could not start $FilePath." }
        $process.StandardInput.Close()
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $completion = [Threading.Tasks.Task]::WhenAll([Threading.Tasks.Task[]]@(
            $process.WaitForExitAsync(), $stdout, $stderr
        ))
        if (-not $completion.Wait($TimeoutSec * 1000)) {
            # Only the child owned by this invocation; never kill the terminal or process names.
            if (-not $process.HasExited) { $process.Kill() }
            if (-not $process.WaitForExit(2000)) { throw "Timed out and could not reap child PID $($process.Id)." }
            throw "Timed out after ${TimeoutSec}s: $FilePath $($Arguments -join ' ')"
        }
        $out = $stdout.GetAwaiter().GetResult()
        $err = $stderr.GetAwaiter().GetResult()
        $clock.Stop()
        if ($process.ExitCode -ne 0) {
            throw "$FilePath $($Arguments -join ' ') exited $($process.ExitCode): $err"
        }
        [pscustomobject]@{
            StdOut = $out
            StdErr = $err
            BoundaryMs = $clock.Elapsed.TotalMilliseconds
            StdoutBytes = [Text.Encoding]::UTF8.GetByteCount($out)
            StderrBytes = [Text.Encoding]::UTF8.GetByteCount($err)
        }
    }
    finally { $process.Dispose() }
}

function Invoke-PcbRequest {
    param($Context, [string[]]$Arguments)
    # Avoid COM activation when the attached package has exited. This is outside the timer.
    foreach ($process in $Context.Processes) {
        $process.Refresh()
        if ($process.HasExited) { throw "Attached Terminal PID $($process.Id) exited; refusing COM activation." }
    }
    $result = Invoke-PcbProcess -FilePath $Context.App.WtcliPath -Arguments (@('--json') + $Arguments) `
        -TimeoutSec $Context.TimeoutSec -Environment @{ WT_COM_CLSID = $Context.App.ComClsid }
    $json = ConvertFrom-Json -InputObject $result.StdOut -AsHashtable -Depth 64 -ErrorAction Stop
    if ($null -eq $json) { throw "wtcli $($Arguments -join ' ') returned no JSON." }
    $Context.Requests.Add([pscustomobject]@{
        Command = $Arguments -join ' '
        BoundaryMs = $result.BoundaryMs
        StdoutBytes = $result.StdoutBytes
        StderrBytes = $result.StderrBytes
        StdoutSha256 = Get-PcbTextHash $result.StdOut
    })
    $json
}

function Assert-PcbPane {
    param($Pane, [string]$TargetPaneId)
    if (-not $Pane -or [string]$Pane.session_id -ne $TargetPaneId -or $Pane.is_agent_pane) {
        throw "Context resolved to a missing/wrong/agent pane; expected $TargetPaneId."
    }
    foreach ($field in @('session_id', 'tab_id', 'window_id', 'pid', 'cwd', 'shell', 'is_agent_pane')) {
        if (-not $Pane.ContainsKey($field)) { throw "Pane metadata omitted $field." }
    }
}

function Invoke-PcbCollector {
    param(
        $Context,
        [ValidateSet('Legacy', 'Consolidated')][string]$Path,
        [ValidateSet('Planner', 'ManualFix', 'ExplicitAutofix')][string]$Mode,
        [string]$TargetPaneId
    )
    $Context.Requests.Clear()
    $maxLines = if ($Mode -eq 'Planner') { 24 } else { 30 }
    $clock = [Diagnostics.Stopwatch]::StartNew()
    if ($Path -eq 'Consolidated') {
        $arguments = @('get-pane-context', '--max-lines', "$maxLines", '--max-chars', '4000')
        if ($Mode -eq 'ExplicitAutofix') { $arguments += @('--target', $TargetPaneId) }
        $capture = Invoke-PcbRequest $Context $arguments
        $pane = $capture.pane
        Assert-PcbPane $pane $TargetPaneId
        foreach ($field in @('content', 'has_marks', 'output_source', 'fallback_reason', 'line_count', 'truncated')) {
            if (-not $capture.ContainsKey($field)) { throw "get-pane-context omitted $field." }
        }
        if ($capture.content -isnot [string] -or $capture.has_marks -isnot [bool] -or $capture.truncated -isnot [bool]) {
            throw 'Invalid pane-context content/marks/truncation types.'
        }
        if ((Get-PcbScalarCount $capture.content) -gt 4000 -or $capture.line_count -gt $maxLines) {
            throw 'get-pane-context exceeded its line/scalar budget.'
        }
        $source = $capture.output_source
        if ($source -eq 'last_command') {
            if (-not $capture.has_marks -or $capture.fallback_reason -ne '') { throw 'Inconsistent last-command metadata.' }
        }
        elseif ($source -eq 'buffer_tail') {
            if ($capture.has_marks -or $capture.fallback_reason -ne 'marks_unavailable') {
                throw "Unexpected buffer fallback reason: $($capture.fallback_reason)"
            }
        }
        else { throw "Unexpected capture source '$source'." }
        $bounded = Limit-PcbPrompt $capture.content 4000 $capture.truncated
    }
    else {
        # HEAD db609f8 resolves active even for explicit autofix; do not "optimize" the baseline.
        $pane = Invoke-PcbRequest $Context @('active-pane')
        if ($Mode -eq 'ExplicitAutofix') {
            $pane = $null
            $windows = Invoke-PcbRequest $Context @('list-windows')
            :findPane foreach ($window in $windows.windows) {
                $tabs = Invoke-PcbRequest $Context @('list-tabs', '-w', [string]$window.window_id)
                foreach ($tab in $tabs.tabs) {
                    $panes = Invoke-PcbRequest $Context @('list-panes', '-w', [string]$window.window_id, '-t', [string]$tab.tab_id)
                    foreach ($candidate in $panes.panes) {
                        if ([string]$candidate.session_id -eq $TargetPaneId) {
                            $pane = $candidate
                            break findPane
                        }
                    }
                }
            }
        }
        Assert-PcbPane $pane $TargetPaneId
        $capture = Invoke-PcbRequest $Context @('capture-pane', '-t', $TargetPaneId, '--last-prompt')
        if ([string]$capture.session_id -ne $TargetPaneId -or $capture.content -isnot [string] -or $capture.has_marks -isnot [bool]) {
            throw 'Invalid legacy marked capture or mismatched target.'
        }
        $hasMarks = $capture.has_marks
        $source = 'last_command'
        if (-not $hasMarks -or [string]::IsNullOrEmpty($capture.content)) {
            $source = 'buffer_tail'
            $capture = Invoke-PcbRequest $Context @('capture-pane', '-t', $TargetPaneId, '-l', "$maxLines")
            if ([string]$capture.session_id -ne $TargetPaneId -or $capture.content -isnot [string]) {
                throw 'Invalid legacy buffer capture or mismatched target.'
            }
        }
        # ReadPaneOutput captures first, then trims lines; chars are truncated in WTA.
        # Legacy marks have NO line cap and legacy ignores the server's truncated flag.
        $bounded = Limit-PcbPrompt $capture.content
        $capture.has_marks = $hasMarks
    }
    $clock.Stop()
    $content = $bounded.Content
    $lines = if ($content.Length) { [regex]::Matches($content, "`n").Count + 1 } else { 0 }
    if ($Path -eq 'Consolidated' -and ($lines -gt $maxLines -or $capture.line_count -ne $lines)) {
        throw "Inconsistent bounded line count: reported $($capture.line_count), actual $lines."
    }
    if ((Get-PcbScalarCount $content) -gt 4000) { throw 'Prompt content exceeded 4000 Unicode scalars.' }
    [pscustomobject]@{
        Path = $Path
        TargetPaneId = $TargetPaneId
        Pane = $pane
        Content = $content
        CaptureContentSha256 = Get-PcbTextHash $capture.content
        ContentSha256 = Get-PcbTextHash $content
        PromptSha256 = Get-PcbTextHash $bounded.Prompt
        PromptBytes = [Text.Encoding]::UTF8.GetByteCount($bounded.Prompt)
        CaptureContentBytes = [Text.Encoding]::UTF8.GetByteCount($capture.content)
        ContentBytes = [Text.Encoding]::UTF8.GetByteCount($content)
        ContentScalars = Get-PcbScalarCount $content
        ContentLines = $lines
        HasMarks = $capture.has_marks
        OutputSource = $source
        Truncated = $bounded.Truncated
        BoundaryMs = ($Context.Requests | Measure-Object BoundaryMs -Sum).Sum
        CollectorMs = $clock.Elapsed.TotalMilliseconds
        Requests = @($Context.Requests.ToArray())
        RequestCount = $Context.Requests.Count
        StdoutBytes = ($Context.Requests | Measure-Object StdoutBytes -Sum).Sum
        StderrBytes = ($Context.Requests | Measure-Object StderrBytes -Sum).Sum
    }
}
