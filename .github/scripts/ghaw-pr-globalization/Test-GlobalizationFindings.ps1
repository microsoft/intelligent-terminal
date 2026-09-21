[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$ReportPath,
    [Parameter(Mandatory)][string]$ContextPath,
    [Parameter(Mandatory)][string]$ExpectedBaseSha,
    [Parameter(Mandatory)][string]$ExpectedHeadSha,
    [ValidateSet('guide', 'repair')][string]$Mode = 'guide',
    [string]$TrustedValidationPath
)

$ErrorActionPreference = 'Stop'

function Assert-ExactSha {
    param([Parameter(Mandatory)][string]$Name, [Parameter(Mandatory)][string]$Value)
    if ($Value -notmatch '^[0-9a-fA-F]{40}$') {
        throw "$Name must be an exact 40-character hexadecimal SHA."
    }
}

function Read-ConfinedJson {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$RuntimeDirectory,
        [Parameter(Mandatory)][string]$Description
    )

    $runtimeItem = Get-Item -LiteralPath $RuntimeDirectory -Force
    if (-not $runtimeItem.PSIsContainer -or $runtimeItem.LinkType -or
        (($runtimeItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0)) {
        throw "$Description runtime directory must be a real directory."
    }

    $item = Get-Item -LiteralPath $Path -Force
    if ($item.PSIsContainer -or $item.LinkType -or
        (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) -or
        $item.Length -lt 2 -or $item.Length -gt 1MB) {
        throw "$Description must be a non-empty regular file no larger than 1 MiB."
    }

    $realRuntime = (Resolve-Path -LiteralPath $RuntimeDirectory).Path.TrimEnd(
        [System.IO.Path]::DirectorySeparatorChar,
        [System.IO.Path]::AltDirectorySeparatorChar)
    $realFile = (Resolve-Path -LiteralPath $Path).Path
    if ([System.IO.Path]::GetDirectoryName($realFile) -cne $realRuntime) {
        throw "$Description resolved outside the fixed runtime directory."
    }

    try {
        return Get-Content -LiteralPath $realFile -Raw | ConvertFrom-Json -Depth 20
    } catch {
        throw "$Description is not valid JSON: $($_.Exception.Message)"
    }
}

function Test-SafeRepositoryPath {
    param([string]$Path)
    return -not [string]::IsNullOrWhiteSpace($Path) -and
        -not [System.IO.Path]::IsPathRooted($Path) -and
        $Path.Replace('\', '/') -notmatch '(^|/)\.\.(/|$)' -and
        $Path.Replace('\', '/') -notmatch '^\.github/' -and
        $Path.Replace('\', '/') -notmatch '(^|/)\.(gitattributes|gitmodules)$'
}

Assert-ExactSha -Name 'ExpectedBaseSha' -Value $ExpectedBaseSha
Assert-ExactSha -Name 'ExpectedHeadSha' -Value $ExpectedHeadSha

$runtimeDirectory = [System.IO.Path]::GetDirectoryName([System.IO.Path]::GetFullPath($ReportPath))
if ([string]::IsNullOrWhiteSpace($runtimeDirectory)) {
    throw 'ReportPath must name a file in an explicit runtime directory.'
}
$report = Read-ConfinedJson -Path $ReportPath -RuntimeDirectory $runtimeDirectory -Description 'Findings report'
$context = Read-ConfinedJson -Path $ContextPath -RuntimeDirectory $runtimeDirectory -Description 'Immutable change context'

$base = $ExpectedBaseSha.ToLowerInvariant()
$head = $ExpectedHeadSha.ToLowerInvariant()
if ($context.version -ne 1 -or $context.baseSha -cne $base -or $context.headSha -cne $head -or
    $null -eq $context.files) {
    throw 'Immutable change context envelope is incomplete or stale.'
}
$changedPaths = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
$contextByPath = @{}
foreach ($file in @($context.files)) {
    $path = ([string]$file.path).Replace('\', '/')
    if (-not (Test-SafeRepositoryPath -Path $path) -or -not $changedPaths.Add($path)) {
        throw 'Immutable change context contains an invalid or duplicate path.'
    }
    if (@($file.hunks).Count -eq 0) {
        throw "Immutable change context contains no changed-line ranges for '$path'."
    }
    $contextByPath[$path] = $file
}

if ($report.version -ne 1 -or $report.baseSha -cne $base -or
    $report.headSha -cne $head -or $null -eq $report.findings) {
    throw 'Findings report envelope is incomplete or stale.'
}

$findings = @($report.findings)
if ($findings.Count -gt 50) {
    throw 'Findings report exceeds the 50-item publication limit.'
}

$ids = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
$fixedFindings = [System.Collections.Generic.List[object]]::new()
foreach ($finding in $findings) {
    if ($finding.stableId -notmatch '^GLOB-[A-Z0-9][A-Z0-9-]{2,63}$' -or -not $ids.Add([string]$finding.stableId)) {
        throw 'Every finding must have a unique stable ID in the GLOB-* namespace.'
    }
    if ($finding.severity -notin @('HIGH', 'MEDIUM', 'LOW')) {
        throw "Finding $($finding.stableId) has an invalid severity."
    }
    if ($finding.confidence -notin @('strong', 'moderate', 'weak')) {
        throw "Finding $($finding.stableId) has an invalid confidence."
    }
    if ($finding.sourceSha -cne $base -or $finding.headSha -cne $head) {
        throw "Finding $($finding.stableId) is stale."
    }
    $findingPath = ([string]$finding.file).Replace('\', '/')
    if (-not (Test-SafeRepositoryPath -Path $findingPath) -or -not $changedPaths.Contains($findingPath)) {
        throw "Finding $($finding.stableId) is outside the immutable pull request change set."
    }
    if ($finding.line -isnot [long] -or $finding.line -lt 1) {
        throw "Finding $($finding.stableId) must identify a positive source line."
    }
    $touchesImmutableHunk = @($contextByPath[$findingPath].hunks | Where-Object {
        $start = if ($_.newCount -gt 0) { [long]$_.newStart } else { [long]$_.oldStart }
        $count = if ($_.newCount -gt 0) { [long]$_.newCount } else { [long]$_.oldCount }
        $end = $start + [Math]::Max($count, 1) - 1
        $finding.line -ge $start -and $finding.line -le $end
    }).Count -gt 0
    if (-not $touchesImmutableHunk) {
        throw "Finding $($finding.stableId) line is outside the immutable pull request hunks."
    }
    foreach ($field in @('scenario', 'localeOrScript', 'observed', 'expected', 'impact', 'proposedFix')) {
        if ([string]::IsNullOrWhiteSpace($finding.$field)) {
            throw "Finding $($finding.stableId) is missing $field."
        }
    }
    if (@($finding.evidence).Count -eq 0 -or @($finding.validation).Count -eq 0) {
        throw "Finding $($finding.stableId) requires evidence and validation."
    }
    if ($finding.disposition -notin @('fixed', 'remaining', 'suggestion', 'blocked', 'skipped')) {
        throw "Finding $($finding.stableId) has an invalid disposition."
    }
    $allowedDispositions = if ($finding.severity -eq 'HIGH') {
        if ($Mode -eq 'repair') { @('fixed', 'remaining', 'blocked') } else { @('remaining', 'blocked') }
    } else {
        @('suggestion', 'skipped')
    }
    if ($finding.disposition -notin $allowedDispositions) {
        throw "Finding $($finding.stableId) has a disposition that is invalid for its severity and workflow mode."
    }
    if ($finding.disposition -eq 'fixed' -and
        ($Mode -ne 'repair' -or $finding.severity -ne 'HIGH' -or $finding.confidence -ne 'strong')) {
        throw "Only strongly evidenced HIGH findings in repair mode may be marked fixed."
    }
    if ($finding.disposition -eq 'fixed') {
        $fixedFindings.Add($finding)
    }
}

$patchFiles = @($report.patchFiles)
if ($Mode -eq 'guide' -and ($patchFiles.Count -gt 0 -or @($report.executedValidation).Count -gt 0)) {
    throw 'Guide reports cannot claim patch files or executed repair validation.'
}
if ($Mode -eq 'repair' -and $patchFiles.Count -gt 0 -and $fixedFindings.Count -eq 0) {
    throw 'A non-empty patchFiles manifest requires at least one fixed finding.'
}

if ($fixedFindings.Count -gt 0) {
    $fixedById = @{}
    foreach ($finding in $fixedFindings) {
        $fixedById[[string]$finding.stableId] = $finding
    }

    if ($patchFiles.Count -eq 0) {
        throw 'Fixed findings require an explicit patchFiles manifest.'
    }
    $declaredPatchPaths = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    foreach ($patchFile in $patchFiles) {
        $path = ([string]$patchFile.path).Replace('\', '/')
        if (-not (Test-SafeRepositoryPath -Path $path) -or $patchFile.kind -ne 'fix' -or
            $path -notmatch '\.(cpp|cxx|cc|h|hpp|xaml|rs)$' -or
            -not $changedPaths.Contains($path) -or -not $declaredPatchPaths.Add($path)) {
            throw 'Every patchFiles entry must be a unique immutable-scope C++/XAML/Rust product path with kind fix.'
        }
        $linkedIds = @($patchFile.findingIds)
        if ($linkedIds.Count -eq 0) {
            throw 'Every patch file must link to at least one fixed finding ID.'
        }
        foreach ($linkedId in $linkedIds) {
            $id = [string]$linkedId
            if (-not $fixedById.ContainsKey($id) -or
                ([string]$fixedById[$id].file).Replace('\', '/') -cne $path) {
                throw 'Every patch path must exactly equal the file of each linked fixed finding.'
            }
        }
        if ($path -match '\.(resw|ya?ml)$') {
            throw 'Automatic globalization repair does not modify resource files; report resource findings without fixing them.'
        }
    }
    foreach ($finding in $fixedFindings) {
        if (-not $declaredPatchPaths.Contains(([string]$finding.file).Replace('\', '/'))) {
            throw "Fixed finding $($finding.stableId) has no exact patchFiles entry."
        }
    }

    $executed = @($report.executedValidation)
    if ($executed.Count -eq 0) {
        throw 'Fixed findings require agent validation evidence in addition to the trusted gate.'
    }
    foreach ($check in $executed) {
        if ([string]::IsNullOrWhiteSpace($check.command) -or $check.exitCode -ne 0 -or
            [string]::IsNullOrWhiteSpace($check.result)) {
            throw 'Every executed validation entry must contain a command, exitCode 0, and result.'
        }
    }

    if ([string]::IsNullOrWhiteSpace($TrustedValidationPath)) {
        throw 'Fixed findings require independently executed trusted validation.'
    }
    $trusted = Read-ConfinedJson -Path $TrustedValidationPath -RuntimeDirectory $runtimeDirectory -Description 'Trusted validation report'
    $trustedChecks = @($trusted.checks)
    if ($trusted.version -ne 1 -or $trustedChecks.Count -eq 0 -or
        @($trustedChecks | Where-Object {
            $_.name -notin @('git-diff-check', 'patch-manifest', 'patch-shape', 'hunk-scope') -or
            $_.status -ne 'PASS' -or $_.exitCode -ne 0
        }).Count -gt 0 -or
        @($trustedChecks.name | Sort-Object -Unique).Count -ne 4) {
        throw 'Trusted validation must contain passing diff, manifest, shape, and hunk-scope checks.'
    }
}

if (@($report.resourceChecks).Count -gt 0) {
    $requiredChecks = @(
        'Test-ResourceSyntax',
        'Test-ResourceEncoding',
        'Test-RequiredKeys',
        'Test-PlaceholderParity',
        'Test-LockedContent',
        'Test-PseudoLocale'
    )
    $allowedStatuses = @{ PASS = 0; FIXABLE = 20; BLOCKED = 30; INVALID_INPUT = 64 }
    $seenChecks = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    foreach ($bundle in @($report.resourceChecks)) {
        if ($bundle.check -notin $requiredChecks -or -not $seenChecks.Add([string]$bundle.check) -or
            -not $allowedStatuses.ContainsKey([string]$bundle.status) -or
            $bundle.exitCode -ne $allowedStatuses[[string]$bundle.status]) {
            throw 'Resource checker bundle has an unknown check, duplicate check, or invalid status/exit-code mapping.'
        }
        $results = @($bundle.results)
        if ($results.Count -eq 0 -or
            @($results | Where-Object { $_.status -notin @('PASS', 'FIXABLE', 'BLOCKED') }).Count -gt 0) {
            throw 'Every resource checker bundle requires non-empty results with known statuses.'
        }
        $statuses = @($results.status)
        $derived = if ($statuses -contains 'BLOCKED') { 'BLOCKED' } elseif ($statuses -contains 'FIXABLE') { 'FIXABLE' } else { 'PASS' }
        if ($bundle.status -cne $derived) {
            throw 'Resource checker aggregate status does not match its results.'
        }
    }
    if ($seenChecks.Count -ne $requiredChecks.Count) {
        throw 'Resource checker evidence must contain each of the six required checks exactly once.'
    }
}

[pscustomobject]@{
    status = 'PASS'
    findingCount = $findings.Count
    high = @($findings | Where-Object severity -eq 'HIGH').Count
    medium = @($findings | Where-Object severity -eq 'MEDIUM').Count
    low = @($findings | Where-Object severity -eq 'LOW').Count
} | ConvertTo-Json -Compress
