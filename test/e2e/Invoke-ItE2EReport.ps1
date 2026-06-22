<#
.SYNOPSIS
    Run the ItE2E suite and emit reports with PRECISE per-failure diagnostics.

.DESCRIPTION
    Wraps Invoke-Pester and writes, into -OutDir:
      - results.xml        NUnit XML (for CI: Azure DevOps / GitHub test reporting)
      - summary.md         Human-readable summary with one block per FAILED test:
                             * full test name (Describe > Context > It)
                             * exact error message (the Assert-*/Should throw text)
                             * file:line of the failing assertion
                             * the failing code line
                             * any artifact paths referenced in the message
                               (screenshots saved by Assert-Ui/Assert-AgentPaneText,
                               plus the per-run framework log)
    Prints the same failure blocks to the console and returns a CI exit code
    (0 = all passed, 1 = any failure).

.EXAMPLE
    pwsh -File test/e2e/Invoke-ItE2EReport.ps1 -Tag Feature
    pwsh -File test/e2e/Invoke-ItE2EReport.ps1 -Path test/e2e/tests/Feature.AutofixPane.Tests.ps1
#>
[CmdletBinding()]
param(
    [string[]]$Path = @("$PSScriptRoot/selftests", "$PSScriptRoot/tests"),
    [string[]]$Tag,
    [string]$OutDir = (Join-Path $env:TEMP ("ite2e-report-{0}" -f (Get-Date -Format 'yyyyMMdd-HHmmss')))
)

$ErrorActionPreference = 'Stop'
Import-Module Pester -MinimumVersion 5.5.0 -Force
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$cfg = New-PesterConfiguration
$cfg.Run.Path = $Path
$cfg.Run.PassThru = $true
if ($Tag) { $cfg.Filter.Tag = $Tag }
$cfg.Output.Verbosity = 'Detailed'
$cfg.TestResult.Enabled = $true
$cfg.TestResult.OutputFormat = 'NUnitXml'
$cfg.TestResult.OutputPath = (Join-Path $OutDir 'results.xml')

$result = Invoke-Pester -Configuration $cfg

# ── Build precise per-failure report ────────────────────────────────────────
$failed = $result.Tests | Where-Object { $_.Result -eq 'Failed' }

function Format-Failure($t) {
    $err = $t.ErrorRecord
    $msg = if ($err) { ($err.Exception.Message).Trim() } else { '(no error record)' }
    # file:line of the failing assertion (last frame inside a .Tests.ps1 or ItE2E .ps1)
    $where = $null
    if ($err -and $err.ScriptStackTrace) {
        $frame = ($err.ScriptStackTrace -split "`n" | Where-Object { $_ -match '\.ps1: line \d+' } | Select-Object -First 1)
        if ($frame -match '(?<file>[A-Za-z]:[^,]+\.ps1): line (?<line>\d+)') { $where = "$($Matches.file.Trim()):$($Matches.line)" }
    }
    # Any artifact paths the assertion embedded in its message (screenshots, logs).
    $artifacts = [regex]::Matches($msg, '[A-Za-z]:\\[^\s"]+\.(png|log|json)') | ForEach-Object { $_.Value } | Select-Object -Unique

    $sb = [System.Text.StringBuilder]::new()
    [void]$sb.AppendLine("### FAIL: $($t.ExpandedPath)")
    [void]$sb.AppendLine("")
    [void]$sb.AppendLine("- **Error:** $msg")
    if ($where) { [void]$sb.AppendLine("- **At:** $where") }
    if ($artifacts) { [void]$sb.AppendLine("- **Artifacts:** " + ($artifacts -join ', ')) }
    [void]$sb.AppendLine("- **Duration:** $([math]::Round($t.Duration.TotalSeconds,1))s")
    [void]$sb.AppendLine("")
    $sb.ToString()
}

$md = [System.Text.StringBuilder]::new()
[void]$md.AppendLine("# ItE2E test report")
[void]$md.AppendLine("")
[void]$md.AppendLine("- When: $(Get-Date -Format o)")
[void]$md.AppendLine("- Passed: $($result.PassedCount)  Failed: $($result.FailedCount)  Skipped: $($result.SkippedCount)")
[void]$md.AppendLine("- Duration: $([math]::Round($result.Duration.TotalSeconds))s")
[void]$md.AppendLine("- NUnit XML: $($cfg.TestResult.OutputPath.Value)")
[void]$md.AppendLine("")
if ($failed) {
    [void]$md.AppendLine("## Failures ($($failed.Count))")
    [void]$md.AppendLine("")
    foreach ($t in $failed) { [void]$md.Append((Format-Failure $t)) }
}
else { [void]$md.AppendLine("## All tests passed ✅") }

$summaryPath = Join-Path $OutDir 'summary.md'
$md.ToString() | Set-Content -LiteralPath $summaryPath -Encoding utf8

# ── Console echo of precise failures ────────────────────────────────────────
Write-Host ""
Write-Host ("=" * 70)
Write-Host "ItE2E REPORT  Passed=$($result.PassedCount) Failed=$($result.FailedCount) Skipped=$($result.SkippedCount)" -ForegroundColor Cyan
Write-Host "  results.xml : $($cfg.TestResult.OutputPath.Value)"
Write-Host "  summary.md  : $summaryPath"
if ($failed) {
    Write-Host ""
    Write-Host "PRECISE FAILURES:" -ForegroundColor Red
    foreach ($t in $failed) {
        $err = $t.ErrorRecord
        $where = ''
        if ($err.ScriptStackTrace -match '(?<f>[A-Za-z]:[^,\n]+\.ps1): line (?<l>\d+)') { $where = " @ $($Matches.f.Trim()):$($Matches.l)" }
        Write-Host ("  [-] {0}{1}" -f $t.ExpandedPath, $where) -ForegroundColor Red
        Write-Host ("      {0}" -f ($err.Exception.Message -replace "`r?`n", ' ').Trim()) -ForegroundColor Yellow
    }
}
Write-Host ("=" * 70)

exit ([int]($result.FailedCount -gt 0))
