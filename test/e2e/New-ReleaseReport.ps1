<#
.SYNOPSIS
    Turn doc/release-check-list.md into a clean, human-facing RELEASE REPORT driven by the
    automated test results — with all UT/E2E/MANUAL jargon stripped out.

.DESCRIPTION
    The report IS the release checklist, but:
      * every coverage tag (`[UT✓]` `[E2E]` `[UT~]` `[MANUAL]`) and the `_(UT: …)_` notes are removed;
      * each item's box is driven purely by what automation could verify:
          [x]                     -> automation ran a test for this item and it PASSED
          [ ] ⚠️ AUTOMATION FAILED -> a test ran and FAILED — investigate before shipping
          [ ] (plain)             -> NOT covered by automation in this run — a human must verify
    A human reading the report never has to know what UT/E2E/MANUAL mean: filled = done by us,
    flagged = broken, empty = your job.

    Items are matched to tests by their bold title appearing in the test's full name
    (Describe.Context.It), plus an optional override map for tests named differently.

.PARAMETER Checklist   Source checklist (default doc/release-check-list.md).
.PARAMETER ResultsXml  NUnit results from a Pester run (default test/e2e/artifacts/results.xml).
.PARAMETER OverrideMap PSD1 of @{ '<item title>' = '<regex matched against test names>' }.
.PARAMETER ExcludeMap  PSD1 of @{ Exclude = @('<item-title regex>', …) }; matching items are
                       dropped from the report entirely (not counted as passed or manual), to
                       keep the checklist focused. Default test/e2e/release-exclude.psd1.
.PARAMETER OutFile     Output report (default test/e2e/artifacts/release-report.md).

.EXAMPLE
    # 1) run the suites (writes results.xml), 2) generate the report:
    pwsh -File test/e2e/Invoke-ItE2EReport.ps1 -Tag Feature
    pwsh -File test/e2e/New-ReleaseReport.ps1
#>
[CmdletBinding()]
param(
    [string]$Checklist = (Join-Path $PSScriptRoot '..\..\doc\release-check-list.md'),
    [string[]]$ResultsXml = @((Join-Path $PSScriptRoot 'artifacts\results.xml')),
    [string]$OverrideMap = (Join-Path $PSScriptRoot 'release-coverage-map.psd1'),
    [string]$ExcludeMap = (Join-Path $PSScriptRoot 'release-exclude.psd1'),
    [string]$OutFile = (Join-Path $PSScriptRoot 'artifacts\release-report.md')
)

$ErrorActionPreference = 'Stop'

# ── Load test results (NUnit) → @{ name = 'Passed'|'Failed'|'Skipped' } ──────────────
# Multiple files merge with LATER files overriding earlier ones per test name, so an
# isolated re-run of a flaky suite can be layered on top of a full-suite results file.
$results = @{}
foreach ($xml in $ResultsXml) {
    if (-not (Test-Path $xml)) { Write-Host "  (skip: $xml not found)" -ForegroundColor DarkGray; continue }
    foreach ($tc in ([xml](Get-Content -Raw $xml)).SelectNodes('//test-case')) {
        $status = switch -Regex ($tc.result) {
            '^(Success|Passed)$'           { 'Passed' }
            '^(Failure|Failed|Error)$'     { 'Failed' }
            default                        { 'Skipped' }   # Ignored / Inconclusive / NotRun
        }
        if ($tc.executed -eq 'False' -and $status -eq 'Passed') { $status = 'Skipped' }
        $results[[string]$tc.name] = $status
    }
}
Write-Host "Loaded $($results.Count) test results from $($ResultsXml -join ', ')" -ForegroundColor Cyan

$overrides = @{}
if (Test-Path $OverrideMap) {
    $rawOverrides = Import-PowerShellDataFile -Path $OverrideMap
    # Keep only entries whose value is a VALID regex — one malformed override pattern must not throw
    # on `$_.Key -match $pattern` and abort report generation.
    foreach ($k in $rawOverrides.Keys) {
        $pat = [string]$rawOverrides[$k]
        try { [void][regex]::new($pat); $overrides[$k] = $pat }
        catch { Write-Warning "release-coverage-map: ignoring invalid regex for '$k' ('$pat'): $($_.Exception.Message)" }
    }
}

# Item-title regexes to drop entirely from the report (kept out of pass/manual counts) so the
# checklist stays focused on the sign-off set. The canonical checklist doc is unaffected. Guard
# against a malformed map (missing/null Exclude, or null/empty patterns) so report generation
# never throws on `$title -match $null`.
$excludes = @()
if (Test-Path $ExcludeMap) {
    $excludes = @((Import-PowerShellDataFile -Path $ExcludeMap).Exclude) |
        Where-Object { -not [string]::IsNullOrWhiteSpace($_) } |
        ForEach-Object {
            $pat = $_
            # Drop any pattern that isn't a valid regex, so one bad entry in release-exclude.psd1
            # can't throw on `$title -match $pat` and abort the whole report generation.
            try { [void][regex]::new($pat); $pat }
            catch { Write-Warning "release-exclude.psd1: ignoring invalid regex pattern '$pat' ($($_.Exception.Message))" }
        }
}

# ── Match a checklist item title to test outcomes ───────────────────────────────────
function Get-ItemStatus([string]$title) {
    if (-not $title) { return 'Untested' }
    $pattern = if ($overrides.ContainsKey($title)) { $overrides[$title] } else { [regex]::Escape($title) }
    $matched = $results.GetEnumerator() | Where-Object { $_.Key -match $pattern }
    if (-not $matched) { return 'Untested' }
    $outcomes = @($matched | ForEach-Object { $_.Value })
    if ($outcomes -contains 'Failed')  { return 'Failed' }
    if ($outcomes -contains 'Passed')  { return 'Passed' }   # passed (ignore co-matched skips)
    return 'Untested'                                        # only skips matched -> human
}

# ── Strip coverage tags + UT notes, keep the readable item text ─────────────────────
function Clear-ItemText([string]$rest) {
    $t = $rest -replace '`\[[^\]]*\]`\s*', ''      # `[E2E]` `[UT✓]` `[MANUAL]` …
    $t = $t -replace '\s*_\(.*?\)_\s*', ' '         # _(UT: …)_ notes
    $t.Trim()
}
function Get-ItemTitle([string]$rest) {
    if ($rest -match '\*\*(.+?):?\*\*') { return ($Matches[1] -replace '`', '').Trim().TrimEnd(':').Trim() }
    return $null
}

# ── Walk the checklist, emit the report ─────────────────────────────────────────────
$pass = 0; $fail = 0; $manual = 0; $excluded = 0
$body = [System.Collections.Generic.List[string]]::new()
foreach ($line in Get-Content -LiteralPath $Checklist) {
    if ($line -match '^\s*#{1,6}\s') { $body.Add($line); continue }              # section headers
    if ($line -match '^\s*-\s*\[(?<box>[ xX])\]\s*(?<rest>.*)$') {
        $box = $Matches['box']
        $rest = $Matches['rest']
        $title = Get-ItemTitle $rest
        $clean = Clear-ItemText $rest
        # Pull the stable item ID (`Cnnn`, assigned by Set-ChecklistIds.ps1) out of the cleaned
        # text so it can be placed CONSISTENTLY right after the box in every branch. The ID has no
        # square brackets, so Clear-ItemText's `[TAG]`-stripping leaves it in $clean.
        $id = ''
        if ($clean -match '^(`C\d+`)\s*(.*)$') { $id = $Matches[1]; $clean = $Matches[2] }
        $idpfx = if ($id) { "$id " } else { '' }
        # Drop intentionally-excluded items (e.g. RTL) entirely — not passed, not manual.
        if ($title -and ($excludes | Where-Object { $title -match $_ })) { $excluded++; continue }
        $status = Get-ItemStatus $title
        # An originally-ticked box means the item is already fully verified by an automated
        # UNIT test (the checklist's [x] convention). Unit tests are automation too, so unless
        # an end-to-end test for it actually FAILED, credit it as passed — the human needn't
        # re-verify it. (Items needing an E2E half were left unticked in the source.)
        if ($status -eq 'Untested' -and $box -match 'x') { $status = 'Passed' }
        switch ($status) {
            'Passed'   { $body.Add("- [x] $idpfx$clean"); $pass++ }
            'Failed'   { $body.Add("- [ ] $idpfx⚠️ **AUTOMATION FAILED** — $clean"); $fail++ }
            default    { $body.Add("- [ ] $idpfx$clean"); $manual++ }
        }
        continue
    }
    # passthrough prose only inside the intro; drop the old marker legend block
    if ($line -notmatch '^\s*-\s*`\[' -and $line -notmatch 'Coverage marker') { $body.Add($line) }
}

$total = $pass + $fail + $manual
$header = @(
    '# Release Report'
    ''
    '> This is the release checklist, filled in by the automated test run. You do not need to'
    '> know how each item is tested — just read the boxes:'
    '>'
    '> - **[x]** — verified automatically (an automated unit or end-to-end test passed).'
    '> - **[ ] ⚠️ AUTOMATION FAILED** — a test ran and FAILED; investigate before shipping.'
    '> - **[ ]** (plain) — not covered by automation in this run; please verify manually.'
    '>'
    "> **Automated: $pass passed, $fail failed. Manual: $manual item(s) left for you.** (total $total)"
    ''
    '---'
    ''
) -join "`n"

$outDir = Split-Path -Parent $OutFile
if ($outDir -and -not (Test-Path $outDir)) { New-Item -ItemType Directory -Path $outDir -Force | Out-Null }
Set-Content -LiteralPath $OutFile -Value ($header + ($body -join "`n")) -Encoding UTF8
Write-Host "Report -> $OutFile" -ForegroundColor Green
Write-Host ("  [x] passed={0}  [!] FAILED={1}  [ ] manual={2}  (total {3}; excluded {4})" -f $pass, $fail, $manual, $total, $excluded)
if ($fail -gt 0) { Write-Host "  WARNING: $fail item(s) have FAILED automation." -ForegroundColor Yellow }
