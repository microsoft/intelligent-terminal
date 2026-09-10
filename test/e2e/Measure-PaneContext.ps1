#Requires -Version 7.2
<#
.SYNOPSIS
    Compare legacy and consolidated context collection against existing, stable panes.
.DESCRIPTION
    Read-only attachment: never starts/stops Terminal, changes focus/settings, or sends input.
    Both collectors use the SAME deployed server and wtcli, not before/after app builds.
    See README.md "Pane-context performance benchmark" for semantics and interpretation.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateScript({ $_ -ne 'Auto' -and -not [string]::IsNullOrWhiteSpace($_) })][string]$Package,
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$Configuration,
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string[]]$TargetPaneId,
    [Parameter(Mandatory)][ValidateSet('Planner', 'ManualFix', 'ExplicitAutofix')][string]$Mode,
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$Scenario,
    [Parameter(Mandatory)][string]$OutDir,
    [ValidateSet('Any', 'Marked', 'Unmarked')][string]$ExpectedMarks = 'Any',
    [string]$ExpectedMarker,
    [ValidateRange(5, 1000)][int]$Warmup = 5,
    [ValidateRange(40, 10000)][int]$Samples = 40,
    [ValidateRange(1, 120)][int]$TimeoutSec = 20
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'tools\PaneContextBenchmark.ps1')
Import-Module (Join-Path $PSScriptRoot 'ItE2E\ItE2E.psd1') -Force
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
$outPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutDir)
$artifactRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot 'artifacts')) + '\'
if (-not $outPath.StartsWith($artifactRoot, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'OutDir must be a subdirectory of the ignored test\e2e\artifacts directory.'
}
foreach ($name in @('samples.csv', 'requests.csv', 'summary.json', 'metadata.json')) {
    if (Test-Path (Join-Path $outPath $name)) { throw "Output already exists: $name. Choose a fresh OutDir." }
}
if ($Mode -ne 'ExplicitAutofix' -and $TargetPaneId.Count -ne 1) {
    throw 'Planner/ManualFix require one expected active pane. This script never changes focus.'
}
$TargetPaneId = @($TargetPaneId | ForEach-Object { ([guid]$_).ToString() })
if (@($TargetPaneId | Select-Object -Unique).Count -ne $TargetPaneId.Count -or $TargetPaneId -contains [guid]::Empty.ToString()) {
    throw 'TargetPaneId must contain distinct nonempty GUIDs.'
}

function Invoke-BenchmarkGit {
    param([string[]]$Arguments)
    (Invoke-PcbProcess -FilePath 'git' -Arguments (@('-C', $repo, '--no-pager') + $Arguments) -TimeoutSec $TimeoutSec).StdOut.TrimEnd()
}

$app = Resolve-ItApp -Package $Package
if (-not $app.WindowsTerminal -or -not (Test-Path $app.WindowsTerminal)) { throw 'Package executable is not readable.' }
$expectedWtcli = Join-Path $app.InstallLocation 'wtcli.exe'
if ($app.WtcliPath -ne $expectedWtcli) { throw 'Refusing wtcli alias/fallback: package-local wtcli.exe is required.' }
$processes = @(Get-WtProcessesForApp -App $app | Where-Object Path -eq $app.WindowsTerminal)
if (-not $processes.Count) { throw 'The selected package must already be running. No app will be launched.' }

# Reuse package discovery and brand constants, but NOT Resolve-WtComClsid's probing:
# probing arbitrary brands for a custom PFN could activate another package.
$knownClsids = & (Get-Module ItE2E) { @($script:ItBrandClsids.Values) }
[xml]$manifest = Get-Content -LiteralPath (Join-Path $app.InstallLocation 'AppxManifest.xml') -Raw
$classes = @($manifest.SelectNodes("//*[local-name()='ExeServer' and @Executable='WindowsTerminal.exe']/*[local-name()='Class']"))
$clsids = @($classes | ForEach-Object { ([guid]$_.Id).ToString('B').ToUpperInvariant() } | Where-Object { $_ -in $knownClsids })
if ($clsids.Count -ne 1) { throw 'Cannot unambiguously resolve the package protocol CLSID from its manifest.' }
$app.ComClsid = $clsids[0]

# Pinned to HEAD when the restored issue was benchmarked; later commits cannot redefine "old".
$baselineRevision = 'db609f8061f81c2eb9a4bdaf3e0666392596bce4'
$baselinePath = 'tools/wta/src/protocol/acp/prompt_context.rs'
$baselineSource = Invoke-BenchmarkGit @('show', "${baselineRevision}:$baselinePath")
$binaryFiles = @(Get-ChildItem -LiteralPath $app.InstallLocation -File | Where-Object {
    $_.Name -in @('wtcli.exe', 'wta.exe', 'WindowsTerminal.exe') -or
    ($_.Extension -eq '.dll' -and $_.Name -match 'Terminal|Control')
})
$binaries = @($binaryFiles | ForEach-Object {
    [pscustomobject]@{
        Path = $_.FullName
        Sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash
        FileVersion = $_.VersionInfo.FileVersion
        ProductVersion = $_.VersionInfo.ProductVersion
    }
})
$sourceStatus = Invoke-BenchmarkGit @('status', '--porcelain=v1', '--untracked-files=all')
$metadata = [ordered]@{
    SchemaVersion = 1
    StartedUtc = [DateTime]::UtcNow.ToString('o')
    PackageSelector = $Package
    PackageFamilyName = $app.Package
    PackageFullName = $app.PackageFullName
    PackageVersion = $app.Version
    ConfigurationLabel = $Configuration
    ComClsid = $app.ComClsid
    TerminalProcesses = @($processes | ForEach-Object { @{ Pid = $_.Id; Path = $_.Path; StartedUtc = $_.StartTime.ToUniversalTime().ToString('o') } })
    Binaries = $binaries
    SourceRevision = Invoke-BenchmarkGit @('rev-parse', 'HEAD')
    SourceBranch = Invoke-BenchmarkGit @('branch', '--show-current')
    SourceDirty = -not [string]::IsNullOrEmpty($sourceStatus)
    SourceStatus = $sourceStatus
    SourceDiffSha256 = Get-PcbTextHash (Invoke-BenchmarkGit @('diff', 'HEAD', '--binary'))
    BaselineRevision = $baselineRevision
    BaselineSourcePath = $baselinePath
    BaselineSourceBlob = Invoke-BenchmarkGit @('rev-parse', "${baselineRevision}:$baselinePath")
    BaselineSourceTextSha256 = Get-PcbTextHash $baselineSource
    BenchmarkScriptSha256 = (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash
    BenchmarkHelpersSha256 = (Get-FileHash -LiteralPath (Join-Path $PSScriptRoot 'tools\PaneContextBenchmark.ps1') -Algorithm SHA256).Hash
    PowerShellVersion = $PSVersionTable.PSVersion.ToString()
    OSVersion = [Environment]::OSVersion.VersionString
    ProcessorCount = [Environment]::ProcessorCount
    Mode = $Mode
    Scenario = $Scenario
    TargetPaneIds = $TargetPaneId
    ExpectedMarks = $ExpectedMarks
    ExpectedMarker = $ExpectedMarker
    WarmupPairsPerPane = $Warmup
    RecordedPairsPerPane = $Samples
    CommandTimeoutSeconds = $TimeoutSec
    MaxLines = $(if ($Mode -eq 'Planner') { 24 } else { 30 })
    MaxContentScalars = 4000
    TimingBoundary = 'Sum of each wtcli process Start through exit and asynchronous stdout/stderr EOF; includes authentication/COM/capture, excludes PowerShell JSON parsing, validation, prompt assembly and LLM.'
    SecondaryTiming = 'CollectorMs includes PowerShell emulation, JSON parsing and inter-request overhead; not native Rust collector latency.'
    Scope = 'Both paths on the same new server isolate context collection, not before/after app binaries or full model/prompt latency. Request counts mean wtcli subprocesses, not raw COM calls; consolidated negotiates capabilities internally.'
    Semantics = 'Legacy planner/manual fix: active-pane + last-prompt + optional full-buffer capture/line-tail fallback. Explicit autofix also queries active-pane, then walks windows/tabs/panes to exact source. No unsupported-capability probe. Legacy marked output has only a 4000-scalar prefix cap, no line cap. New marks also obey 24/30 lines; unmarked scalar truncation retains the tail, whereas legacy keeps the prefix of its line tail. Prompt truncation suffix is outside the 4000-scalar content budget. Exact cross-path text equality is reported, not required.'
}
New-Item -ItemType Directory -Path $outPath -Force | Out-Null
$metadata | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $outPath 'metadata.json') -Encoding utf8
$context = @{
    App = $app
    Processes = $processes
    TimeoutSec = $TimeoutSec
    Requests = [Collections.Generic.List[object]]::new()
}
$recorded = [Collections.Generic.List[object]]::new()
$comparisons = [Collections.Generic.List[object]]::new()
foreach ($target in $TargetPaneId) {
    $fingerprints = @{}
    for ($pair = 0; $pair -lt ($Warmup + $Samples); $pair++) {
        $phase = if ($pair -lt $Warmup) { 'Warmup' } else { 'Recorded' }
        $index = if ($phase -eq 'Warmup') { $pair + 1 } else { $pair - $Warmup + 1 }
        $order = if ($pair % 2 -eq 0) { @('Legacy', 'Consolidated') } else { @('Consolidated', 'Legacy') }
        $pairResults = @{}
        for ($position = 0; $position -lt 2; $position++) {
            $path = $order[$position]
            $result = Invoke-PcbCollector $context $path $Mode $target
            if ($ExpectedMarker -and -not $result.Content.Contains($ExpectedMarker, [StringComparison]::Ordinal)) {
                throw "$path/$target omitted expected marker from bounded content."
            }
            if (($ExpectedMarks -eq 'Marked' -and -not $result.HasMarks) -or
                ($ExpectedMarks -eq 'Unmarked' -and $result.HasMarks)) { throw "$path/$target has unexpected shell marks." }
            $fingerprint = @($result.CaptureContentSha256, $result.PromptSha256, $result.OutputSource, $result.HasMarks,
                $result.Pane.session_id, $result.Pane.tab_id, $result.Pane.window_id,
                $result.Pane.pid, $result.Pane.cwd, $result.Pane.shell, $result.RequestCount) -join '|'
            if ($fingerprints.ContainsKey($path) -and $fingerprints[$path] -cne $fingerprint) {
                throw "$path/$target content, metadata or topology changed; discard this run and use stable panes."
            }
            $fingerprints[$path] = $fingerprint
            $pairResults[$path] = $result
            $row = [pscustomobject][ordered]@{
                Scenario = $Scenario; Mode = $Mode; TargetPaneId = $target; Phase = $phase
                Pair = $index; Position = $position + 1; Path = $path
                BoundaryMs = $result.BoundaryMs; CollectorMs = $result.CollectorMs
                RequestCount = $result.RequestCount; StdoutBytes = $result.StdoutBytes; StderrBytes = $result.StderrBytes
                CaptureContentBytes = $result.CaptureContentBytes; ContentBytes = $result.ContentBytes
                ContentScalars = $result.ContentScalars; ContentLines = $result.ContentLines
                PromptBytes = $result.PromptBytes; HasMarks = $result.HasMarks; OutputSource = $result.OutputSource
                Truncated = $result.Truncated; CaptureContentSha256 = $result.CaptureContentSha256
                ContentSha256 = $result.ContentSha256; PromptSha256 = $result.PromptSha256
            }
            $row | Export-Csv -LiteralPath (Join-Path $outPath 'samples.csv') -NoTypeInformation -Append -Encoding utf8
            $requestIndex = 0
            foreach ($request in $result.Requests) {
                [pscustomobject]@{
                    TargetPaneId = $target; Phase = $phase; Pair = $index; Path = $path; Position = $position + 1
                    Request = ++$requestIndex; Command = $request.Command; BoundaryMs = $request.BoundaryMs
                    StdoutBytes = $request.StdoutBytes; StderrBytes = $request.StderrBytes; StdoutSha256 = $request.StdoutSha256
                } | Export-Csv -LiteralPath (Join-Path $outPath 'requests.csv') -NoTypeInformation -Append -Encoding utf8
            }
            if ($phase -eq 'Recorded') { $recorded.Add($row) }
        }
        foreach ($field in @('session_id', 'tab_id', 'window_id', 'pid', 'cwd', 'shell')) {
            if ([string]$pairResults.Legacy.Pane[$field] -cne [string]$pairResults.Consolidated.Pane[$field]) {
                throw "Collectors disagree on pane metadata '$field'."
            }
        }
        if ($pairResults.Legacy.HasMarks -ne $pairResults.Consolidated.HasMarks -or
            $pairResults.Legacy.OutputSource -ne $pairResults.Consolidated.OutputSource) {
            throw 'Collectors disagree on marks/output source; unstable or incompatible capture.'
        }
    }
    $old = @($recorded | Where-Object { $_.TargetPaneId -eq $target -and $_.Path -eq 'Legacy' })
    $new = @($recorded | Where-Object { $_.TargetPaneId -eq $target -and $_.Path -eq 'Consolidated' })
    $comparison = Get-PcbComparison -Legacy $old.BoundaryMs -Consolidated $new.BoundaryMs
    $comparisons.Add([pscustomobject]@{
        TargetPaneId = $target
        Latency = $comparison
        LegacyRequestsPerSample = @($old.RequestCount | Select-Object -Unique)
        ConsolidatedRequestsPerSample = @($new.RequestCount | Select-Object -Unique)
        LegacyMeanStdoutBytes = ($old | Measure-Object StdoutBytes -Average).Average
        ConsolidatedMeanStdoutBytes = ($new | Measure-Object StdoutBytes -Average).Average
        LegacyContentScalars = @($old.ContentScalars | Select-Object -Unique)
        ConsolidatedContentScalars = @($new.ContentScalars | Select-Object -Unique)
        ExactBoundedContentEqual = $old[0].ContentSha256 -ceq $new[0].ContentSha256
        ExactPromptPayloadEqual = $old[0].PromptSha256 -ceq $new[0].PromptSha256
    })
}
foreach ($binary in $binaries) {
    if ((Get-FileHash -LiteralPath $binary.Path -Algorithm SHA256).Hash -ne $binary.Sha256) {
        throw "Deployed binary changed during the run: $($binary.Path)"
    }
}
$summary = [ordered]@{
    Status = 'Passed'
    CompletedUtc = [DateTime]::UtcNow.ToString('o')
    Metadata = $metadata
    PercentileMethod = 'Nearest rank: sorted[ceil(p*N)-1]; warmups excluded; ratio of path percentiles, not percentile of pair ratios.'
    Comparisons = $comparisons.ToArray()
}
$summary | ConvertTo-Json -Depth 15 | Set-Content -LiteralPath (Join-Path $outPath 'summary.json') -Encoding utf8
$comparisons | ForEach-Object {
    Write-Host ('{0}: p50 {1:F2} -> {2:F2} ms ({3:F2}x, {4:F1}% reduction); p95 {5:F2} -> {6:F2} ms' -f
        $_.TargetPaneId, $_.Latency.LegacyP50Ms, $_.Latency.ConsolidatedP50Ms, $_.Latency.P50Speedup,
        $_.Latency.P50ReductionPercent, $_.Latency.LegacyP95Ms, $_.Latency.ConsolidatedP95Ms)
}
Write-Host "Verified benchmark artifacts: $outPath"
