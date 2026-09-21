[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$ReportPath,
    [Parameter(Mandatory)][string]$ExpectedBaseSha,
    [Parameter(Mandatory)][string]$ExpectedHeadSha,
    [ValidateSet('guide', 'repair')][string]$Mode = 'guide'
)

$ErrorActionPreference = 'Stop'

function Assert-ExactSha {
    param([Parameter(Mandatory)][string]$Name, [Parameter(Mandatory)][string]$Value)
    if ($Value -notmatch '^[0-9a-fA-F]{40}$') {
        throw "$Name must be an exact 40-character hexadecimal SHA."
    }
}

Assert-ExactSha -Name 'ExpectedBaseSha' -Value $ExpectedBaseSha
Assert-ExactSha -Name 'ExpectedHeadSha' -Value $ExpectedHeadSha

$item = Get-Item -LiteralPath $ReportPath -Force
if (-not $item.PSIsContainer -and $item.Length -gt 0 -and $item.Length -le 1MB) {
    $report = Get-Content -LiteralPath $ReportPath -Raw | ConvertFrom-Json -Depth 20
} else {
    throw 'Findings report must be a non-empty regular file no larger than 1 MiB.'
}

if ($report.version -ne 1 -or $report.baseSha -cne $ExpectedBaseSha.ToLowerInvariant() -or
    $report.headSha -cne $ExpectedHeadSha.ToLowerInvariant() -or $null -eq $report.findings) {
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
    if ($finding.sourceSha -cne $ExpectedBaseSha.ToLowerInvariant() -or
        $finding.headSha -cne $ExpectedHeadSha.ToLowerInvariant()) {
        throw "Finding $($finding.stableId) is stale."
    }
    if ([string]::IsNullOrWhiteSpace($finding.file) -or
        [System.IO.Path]::IsPathRooted([string]$finding.file) -or
        $finding.file.Replace('\', '/') -match '(^|/)\.\.(/|$)' -or
        $finding.file.Replace('\', '/') -match '^\.github/') {
        throw "Finding $($finding.stableId) has an unsafe or out-of-scope path."
    }
    if ($finding.line -isnot [long] -or $finding.line -lt 1) {
        throw "Finding $($finding.stableId) must identify a positive source line."
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
    if ($finding.severity -ne 'HIGH' -and $finding.disposition -eq 'blocked') {
        throw "Only HIGH findings may use the blocking disposition."
    }
    if ($finding.disposition -eq 'fixed' -and
        ($Mode -ne 'repair' -or $finding.severity -ne 'HIGH' -or $finding.confidence -ne 'strong')) {
        throw "Only strongly evidenced HIGH findings in repair mode may be marked fixed."
    }
    if ($finding.disposition -eq 'fixed') {
        $fixedFindings.Add($finding)
    }
}

if ($fixedFindings.Count -gt 0) {
    $fixedIds = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    foreach ($finding in $fixedFindings) {
        $null = $fixedIds.Add([string]$finding.stableId)
    }
    $patchFiles = @($report.patchFiles)
    if ($patchFiles.Count -eq 0) {
        throw 'Fixed findings require an explicit patchFiles manifest.'
    }
    foreach ($patchFile in $patchFiles) {
        $path = [string]$patchFile.path
        if ([string]::IsNullOrWhiteSpace($path) -or [System.IO.Path]::IsPathRooted($path) -or
            $path.Replace('\', '/') -match '(^|/)\.\.(/|$)' -or $path.Replace('\', '/') -match '^\.github/' -or
            $patchFile.kind -notin @('fix', 'test')) {
            throw 'Every patchFiles entry must contain a safe path and fix/test kind.'
        }
        $linkedIds = @($patchFile.findingIds)
        if ($linkedIds.Count -eq 0 -or @($linkedIds | Where-Object { -not $fixedIds.Contains([string]$_) }).Count -gt 0) {
            throw 'Every patch file must link only to fixed finding IDs.'
        }
    }

    $executed = @($report.executedValidation)
    if ($executed.Count -eq 0) {
        throw 'Fixed findings require executedValidation evidence.'
    }
    foreach ($check in $executed) {
        if ([string]::IsNullOrWhiteSpace($check.command) -or $check.exitCode -ne 0 -or
            [string]::IsNullOrWhiteSpace($check.result)) {
            throw 'Every executed validation entry must contain a command, exitCode 0, and result.'
        }
    }

    $resourceFix = @($fixedFindings | Where-Object { $_.file -match '\.(resw|ya?ml)$' }).Count -gt 0
    if ($resourceFix) {
        $resourceChecks = @($report.resourceChecks)
        if ($resourceChecks.Count -eq 0 -or
            @($resourceChecks | Where-Object { $_.status -ne 'PASS' -or $_.exitCode -ne 0 }).Count -gt 0) {
            throw 'Fixed resource findings require actual passing final resource checker bundles.'
        }
    }
}

[pscustomobject]@{
    status = 'PASS'
    findingCount = $findings.Count
    high = @($findings | Where-Object severity -eq 'HIGH').Count
    medium = @($findings | Where-Object severity -eq 'MEDIUM').Count
    low = @($findings | Where-Object severity -eq 'LOW').Count
} | ConvertTo-Json -Compress
