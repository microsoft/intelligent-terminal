<#
.SYNOPSIS
    INCREMENTALLY update an existing release-report.md from a PARTIAL test run — only the items
    the run actually covered change; every other tick is preserved exactly as it was.

.DESCRIPTION
    New-ReleaseReport.ps1 regenerates the whole report from the source checklist + results, so a
    single-suite run would blank every item the run didn't cover. This script instead takes an
    ALREADY-GENERATED report.md as the source of truth and overlays just the results from a partial
    run:

        run one suite (writes its own results.xml)  ->  Update-ReleaseReport -Report <md> -ResultsXml <xml>

    For each checklist item in the report:
      * the item's title is matched against the fresh results' test names (same title / override-map
        logic as New-ReleaseReport);
      * MATCHED + a test PASSED    -> box becomes [x];
      * MATCHED + a test FAILED    -> box becomes [ ] ⚠️ AUTOMATION FAILED;
      * MATCHED but only SKIPPED   -> left UNCHANGED (a flaky skip must not un-tick a prior pass);
      * NOT matched (out of scope) -> left UNCHANGED.
    The header "Automated: X passed, Y failed. Manual: Z" line is recomputed from the final boxes.

    So you can run just the suite you touched and refresh only its rows, without a full-suite run.

.PARAMETER Report      Existing release-report.md to update (input). Default test/e2e/artifacts/release-report.md.
.PARAMETER ResultsXml  NUnit results from the partial run(s). Default test/e2e/artifacts/results.xml.
                       Multiple files merge with LATER files overriding earlier per test name.
.PARAMETER OverrideMap PSD1 of @{ '<item title>' = '<regex matched against test names>' }.
.PARAMETER OutFile     Where to write. Default: in place ($Report).

.EXAMPLE
    # 1) run just the delegate suite into its own results file:
    pwsh -File test/e2e/Invoke-ItE2EReport.ps1 -Path test/e2e/tests/Feature.Delegate.Tests.ps1 -SkipReleaseReport
    # 2) overlay only those rows onto the existing report:
    pwsh -File test/e2e/Update-ReleaseReport.ps1
#>
[CmdletBinding()]
param(
    [string]$Report = (Join-Path $PSScriptRoot 'artifacts\release-report.md'),
    [string[]]$ResultsXml = @((Join-Path $PSScriptRoot 'artifacts\results.xml')),
    [string]$OverrideMap = (Join-Path $PSScriptRoot 'release-coverage-map.psd1'),
    [string]$OutFile
)

$ErrorActionPreference = 'Stop'
if (-not $OutFile) { $OutFile = $Report }
if (-not (Test-Path $Report)) { throw "Update-ReleaseReport: report not found: $Report. Generate it first with New-ReleaseReport.ps1." }

# ── Load results (NUnit) → @{ name = 'Passed'|'Failed'|'Skipped' } (same rules as New-ReleaseReport) ──
$results = @{}
foreach ($xml in $ResultsXml) {
    if (-not (Test-Path $xml)) { Write-Host "  (skip: $xml not found)" -ForegroundColor DarkGray; continue }
    foreach ($tc in ([xml](Get-Content -Raw $xml)).SelectNodes('//test-case')) {
        $status = switch -Regex ($tc.result) {
            '^(Success|Passed)$'       { 'Passed' }
            '^(Failure|Failed|Error)$' { 'Failed' }
            default                    { 'Skipped' }
        }
        if ($tc.executed -eq 'False' -and $status -eq 'Passed') { $status = 'Skipped' }
        $results[[string]$tc.name] = $status
    }
}
if (-not $results.Count) { throw "Update-ReleaseReport: no test results loaded from $($ResultsXml -join ', '); nothing to update." }
Write-Host "Loaded $($results.Count) test result(s) from $($ResultsXml -join ', ')" -ForegroundColor Cyan

$overrides = @{}
if (Test-Path $OverrideMap) {
    $rawOverrides = Import-PowerShellDataFile -Path $OverrideMap
    # Keep only entries whose value is a VALID regex — one malformed override pattern must not throw
    # on `$_.Key -match $pattern` and abort an incremental report update.
    foreach ($k in $rawOverrides.Keys) {
        $pat = [string]$rawOverrides[$k]
        try { [void][regex]::new($pat); $overrides[$k] = $pat }
        catch { Write-Warning "release-coverage-map: ignoring invalid regex for '$k' ('$pat'): $($_.Exception.Message)" }
    }
}

# ── Title extraction + result matching (mirrors New-ReleaseReport) ────────────────────
function Get-ReportItemTitle([string]$body) {
    # $body is the item text after the checkbox, with any "⚠️ **AUTOMATION FAILED** — " prefix
    # already stripped. Title is the first bold run, backticks removed, trailing ':' dropped.
    if ($body -match '\*\*(.+?):?\*\*') { return ($Matches[1] -replace '`', '').Trim().TrimEnd(':').Trim() }
    return $null
}
function Get-CoverageStatus([string]$title) {
    if (-not $title) { return 'Untested' }
    $pattern = if ($overrides.ContainsKey($title)) { $overrides[$title] } else { [regex]::Escape($title) }
    $matched = $results.GetEnumerator() | Where-Object { $_.Key -match $pattern }
    if (-not $matched) { return 'Untested' }        # out of scope for this run
    $outcomes = @($matched | ForEach-Object { $_.Value })
    if ($outcomes -contains 'Failed') { return 'Failed' }
    if ($outcomes -contains 'Passed') { return 'Passed' }
    return 'SkippedOnly'                            # matched but only skips → keep as-is
}

$failedPrefix = '⚠️ **AUTOMATION FAILED** — '
$updated = 0
$out = [System.Collections.Generic.List[string]]::new()

foreach ($line in Get-Content -LiteralPath $Report) {
    # Item line: "- [ ] ..." / "- [x] ...". Leave everything else (headers, prose) untouched here;
    # the summary line is recomputed in a second pass below.
    if ($line -match '^(?<indent>\s*)-\s*\[(?<box>[ xX])\]\s*(?<body>.*)$') {
        $indent = $Matches['indent']
        $body = $Matches['body']
        # Peel the stable item ID (`Cnnn`) so it is preserved and re-emitted right after the box.
        $id = ''
        if ($body -match '^(`C\d+`)\s*(.*)$') { $id = $Matches[1]; $body = $Matches[2] }
        $idpfx = if ($id) { "$id " } else { '' }
        # Strip an existing FAILED marker to recover the clean item text (title + desc).
        $clean = $body
        if ($clean.StartsWith($failedPrefix)) { $clean = $clean.Substring($failedPrefix.Length) }
        $title = Get-ReportItemTitle $clean
        switch (Get-CoverageStatus $title) {
            'Passed' { $out.Add("$indent- [x] $idpfx$clean"); $updated++ }
            'Failed' { $out.Add("$indent- [ ] $idpfx$failedPrefix$clean"); $updated++ }
            default { $out.Add($line) }   # Untested (out of scope) or SkippedOnly → unchanged
        }
        continue
    }
    $out.Add($line)
}

# ── Recompute the header summary from the FINAL boxes ─────────────────────────────────
$pass = 0; $fail = 0; $manual = 0
foreach ($l in $out) {
    if ($l -match '^\s*-\s*\[[xX]\]\s') { $pass++ }
    elseif ($l -match '^\s*-\s*\[\s\]\s*(`C\d+`\s*)?⚠️\s*\*\*AUTOMATION FAILED\*\*') { $fail++ }
    elseif ($l -match '^\s*-\s*\[\s\]\s') { $manual++ }
}
$total = $pass + $fail + $manual
for ($i = 0; $i -lt $out.Count; $i++) {
    if ($out[$i] -match '^\s*>\s*\*\*Automated:') {
        $out[$i] = "> **Automated: $pass passed, $fail failed. Manual: $manual item(s) left for you.** (total $total)"
        break
    }
}

$outDir = Split-Path -Parent $OutFile
if ($outDir -and -not (Test-Path $outDir)) { New-Item -ItemType Directory -Path $outDir -Force | Out-Null }
Set-Content -LiteralPath $OutFile -Value ($out -join "`n") -Encoding UTF8
Write-Host "Updated $updated item(s) from this run -> $OutFile" -ForegroundColor Green
Write-Host ("  [x] passed={0}  [!] FAILED={1}  [ ] manual={2}  (total {3})" -f $pass, $fail, $manual, $total)
if ($fail -gt 0) { Write-Host "  WARNING: $fail item(s) have FAILED automation." -ForegroundColor Yellow }
