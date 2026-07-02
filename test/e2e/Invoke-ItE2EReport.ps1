<#
.SYNOPSIS
    Run the ItE2E suite and emit reports with PRECISE per-failure diagnostics.

.DESCRIPTION
    Wraps Invoke-Pester and writes, into -OutDir (default: test/e2e/artifacts/):
      - report.html        Self-contained HTML report (open in a browser):
                             * pass/fail/skip banner (green / red)
                             * one card per FAILED test with the exact error,
                               file:line of the failing assertion, duration, and
                               clickable artifact links (screenshots / logs)
                             * full results table grouped by Describe > Context
      - results.xml        NUnit XML (for CI: Azure DevOps / GitHub test reporting)
      - summary.md         Human-readable Markdown summary (same per-failure blocks)
      - release-report.md  Clean, jargon-free RELEASE CHECKLIST driven by the results
                           ([x] = automation verified it; plain [ ] = verify manually).
                           Generated via New-ReleaseReport.ps1; suppress with -SkipReleaseReport.
    Prints the same failure blocks to the console and returns a CI exit code
    (0 = all passed, 1 = any failure).

.EXAMPLE
    pwsh -File test/e2e/Invoke-ItE2EReport.ps1 -Tag Feature
    pwsh -File test/e2e/Invoke-ItE2EReport.ps1 -Path test/e2e/tests/Feature.AutofixPane.Tests.ps1
    pwsh -File test/e2e/Invoke-ItE2EReport.ps1            # full suite -> test/e2e/artifacts/
#>
[CmdletBinding()]
param(
    [string[]]$Path = @("$PSScriptRoot/selftests", "$PSScriptRoot/tests"),
    [string[]]$Tag,
    # Fixed in-repo location by default so the latest report is always at a known path.
    [string]$OutDir = (Join-Path $PSScriptRoot 'artifacts'),
    # Also emit the clean, jargon-free release checklist (release-report.md) from the results.
    [switch]$SkipReleaseReport,
    # INCREMENTAL mode: instead of regenerating release-report.md from scratch (which blanks every
    # item this run didn't cover), OVERLAY just this run's results onto the EXISTING report — only
    # the items this run covered change. Use for single-suite runs so you don't need a full-suite
    # run to refresh one area. No-op if the report doesn't exist yet (falls back to full generate).
    [switch]$UpdateReport
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

# ── Shared helpers ──────────────────────────────────────────────────────────
function Get-FailureWhere($err) {
    if ($err -and $err.ScriptStackTrace) {
        $frame = ($err.ScriptStackTrace -split "`n" | Where-Object { $_ -match '\.ps1: line \d+' } | Select-Object -First 1)
        if ($frame -match '(?<file>[A-Za-z]:[^,]+\.ps1): line (?<line>\d+)') { return "$($Matches.file.Trim()):$($Matches.line)" }
    }
    return $null
}
function Get-FailureArtifacts($msg) {
    [regex]::Matches($msg, '[A-Za-z]:\\[^\s"'']+\.(png|log|json)') | ForEach-Object { $_.Value } | Select-Object -Unique
}
function HtmlEnc($s) { if ($null -eq $s) { return '' } [System.Net.WebUtility]::HtmlEncode([string]$s) }
function FileUri($p) { try { ([uri]([System.IO.Path]::GetFullPath($p))).AbsoluteUri } catch { $p } }

$failed = $result.Tests | Where-Object { $_.Result -eq 'Failed' }

# ── Markdown summary ────────────────────────────────────────────────────────
function Format-Failure($t) {
    $err = $t.ErrorRecord
    $msg = if ($err) { ($err.Exception.Message).Trim() } else { '(no error record)' }
    $where = Get-FailureWhere $err
    $artifacts = Get-FailureArtifacts $msg

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
[void]$md.AppendLine("- HTML report: $(Join-Path $OutDir 'report.html')")
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

# ── HTML report ─────────────────────────────────────────────────────────────
$allPass = ($result.FailedCount -eq 0)
$bannerClass = if ($allPass) { 'ok' } else { 'bad' }
$bannerText = if ($allPass) { "ALL PASSED" } else { "$($result.FailedCount) FAILED" }

$h = [System.Text.StringBuilder]::new()
[void]$h.AppendLine('<!DOCTYPE html><html lang="en"><head><meta charset="utf-8">')
[void]$h.AppendLine('<meta name="viewport" content="width=device-width, initial-scale=1">')
[void]$h.AppendLine('<title>ItE2E test report</title><style>')
[void]$h.AppendLine(@'
:root{--ok:#1a7f37;--bad:#cf222e;--skip:#9a6700;--bg:#f6f8fa;--fg:#1f2328;--mut:#656d76;--bd:#d0d7de;--card:#fff}
*{box-sizing:border-box}body{font:14px/1.5 -apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;margin:0;color:var(--fg);background:var(--bg)}
.wrap{max-width:1100px;margin:0 auto;padding:24px}
.banner{border-radius:10px;padding:18px 22px;color:#fff;display:flex;align-items:center;gap:18px;flex-wrap:wrap}
.banner.ok{background:var(--ok)}.banner.bad{background:var(--bad)}
.banner h1{font-size:22px;margin:0}.banner .meta{opacity:.92;font-size:13px}
.stats{display:flex;gap:10px;margin:18px 0;flex-wrap:wrap}
.stat{background:var(--card);border:1px solid var(--bd);border-radius:8px;padding:10px 16px;min-width:96px}
.stat .n{font-size:22px;font-weight:700}.stat .l{color:var(--mut);font-size:12px;text-transform:uppercase;letter-spacing:.04em}
.stat.pass .n{color:var(--ok)}.stat.fail .n{color:var(--bad)}.stat.skip .n{color:var(--skip)}
h2{margin:26px 0 10px;font-size:17px}
.card{background:var(--card);border:1px solid var(--bd);border-left:4px solid var(--bad);border-radius:8px;padding:14px 16px;margin:12px 0}
.card .name{font-weight:600;margin-bottom:8px}
.card .row{display:flex;gap:8px;margin:4px 0;font-size:13px}.card .k{color:var(--mut);min-width:74px}
.card pre{background:var(--bg);border:1px solid var(--bd);border-radius:6px;padding:10px;overflow:auto;margin:6px 0 0;white-space:pre-wrap;font:12px/1.45 ui-monospace,Consolas,monospace}
.card a{color:#0969da}.card img{max-width:320px;border:1px solid var(--bd);border-radius:6px;display:block;margin-top:8px}
table{width:100%;border-collapse:collapse;background:var(--card);border:1px solid var(--bd);border-radius:8px;overflow:hidden;font-size:13px}
th,td{text-align:left;padding:8px 12px;border-bottom:1px solid var(--bd)}th{background:var(--bg);color:var(--mut);font-weight:600}
tr:last-child td{border-bottom:0}td.s{white-space:nowrap;width:1%}.grp td{background:var(--bg);font-weight:600}
.pill{display:inline-block;padding:1px 8px;border-radius:999px;font-size:12px;font-weight:600}
.pill.Passed{background:#dafbe1;color:var(--ok)}.pill.Failed{background:#ffebe9;color:var(--bad)}
.pill.Skipped{background:#fff8c5;color:var(--skip)}.pill.Inconclusive,.pill.NotRun{background:#eaeef2;color:var(--mut)}
td.dur{color:var(--mut);text-align:right;white-space:nowrap}
.foot{color:var(--mut);font-size:12px;margin-top:22px}
'@)
[void]$h.AppendLine('</style></head><body><div class="wrap">')

[void]$h.AppendLine("<div class=`"banner $bannerClass`"><h1>ItE2E &middot; $bannerText</h1>" +
    "<div class=`"meta`">$(HtmlEnc (Get-Date -Format 'yyyy-MM-dd HH:mm:ss')) &middot; $([math]::Round($result.Duration.TotalSeconds))s" +
    "$(if($Tag){' &middot; tag: ' + (HtmlEnc ($Tag -join ', '))})</div></div>")

[void]$h.AppendLine('<div class="stats">')
[void]$h.AppendLine("<div class=`"stat`"><div class=`"n`">$($result.TotalCount)</div><div class=`"l`">Total</div></div>")
[void]$h.AppendLine("<div class=`"stat pass`"><div class=`"n`">$($result.PassedCount)</div><div class=`"l`">Passed</div></div>")
[void]$h.AppendLine("<div class=`"stat fail`"><div class=`"n`">$($result.FailedCount)</div><div class=`"l`">Failed</div></div>")
[void]$h.AppendLine("<div class=`"stat skip`"><div class=`"n`">$($result.SkippedCount)</div><div class=`"l`">Skipped</div></div>")
[void]$h.AppendLine('</div>')

# Failure cards
if ($failed) {
    [void]$h.AppendLine("<h2>Failures ($($failed.Count))</h2>")
    foreach ($t in $failed) {
        $err = $t.ErrorRecord
        $msg = if ($err) { ($err.Exception.Message).Trim() } else { '(no error record)' }
        $where = Get-FailureWhere $err
        $artifacts = Get-FailureArtifacts $msg
        [void]$h.AppendLine('<div class="card">')
        [void]$h.AppendLine("<div class=`"name`">$(HtmlEnc $t.ExpandedPath)</div>")
        if ($where) { [void]$h.AppendLine("<div class=`"row`"><span class=`"k`">At</span><code>$(HtmlEnc $where)</code></div>") }
        [void]$h.AppendLine("<div class=`"row`"><span class=`"k`">Duration</span>$([math]::Round($t.Duration.TotalSeconds,1))s</div>")
        if ($artifacts) {
            $links = ($artifacts | ForEach-Object { "<a href=`"$(FileUri $_)`">$(HtmlEnc (Split-Path $_ -Leaf))</a>" }) -join ' '
            [void]$h.AppendLine("<div class=`"row`"><span class=`"k`">Artifacts</span><span>$links</span></div>")
            foreach ($a in $artifacts) { if ($a -match '\.png$' -and (Test-Path $a)) { [void]$h.AppendLine("<img src=`"$(FileUri $a)`" alt=`"artifact`">") } }
        }
        [void]$h.AppendLine("<pre>$(HtmlEnc $msg)</pre>")
        [void]$h.AppendLine('</div>')
    }
}

# Full results table grouped by Describe > Context
[void]$h.AppendLine('<h2>All results</h2><table><thead><tr><th>Test</th><th class="s">Result</th><th class="dur">Time</th></tr></thead><tbody>')
$groups = $result.Tests | Group-Object { try { $_.Block.ExpandedPath } catch { '(ungrouped)' } } | Sort-Object Name
foreach ($g in $groups) {
    [void]$h.AppendLine("<tr class=`"grp`"><td colspan=`"3`">$(HtmlEnc $g.Name)</td></tr>")
    foreach ($t in ($g.Group | Sort-Object { $_.ExpandedName })) {
        $r = [string]$t.Result
        [void]$h.AppendLine("<tr><td>$(HtmlEnc $t.ExpandedName)</td>" +
            "<td class=`"s`"><span class=`"pill $r`">$r</span></td>" +
            "<td class=`"dur`">$([math]::Round($t.Duration.TotalSeconds,2))s</td></tr>")
    }
}
[void]$h.AppendLine('</tbody></table>')

[void]$h.AppendLine("<div class=`"foot`">NUnit XML: $(HtmlEnc $cfg.TestResult.OutputPath.Value) &middot; Markdown: $(HtmlEnc $summaryPath)</div>")
[void]$h.AppendLine('</div></body></html>')

$htmlPath = Join-Path $OutDir 'report.html'
$h.ToString() | Set-Content -LiteralPath $htmlPath -Encoding utf8

# ── Console echo of precise failures ────────────────────────────────────────
Write-Host ""
Write-Host ("=" * 70)
Write-Host "ItE2E REPORT  Passed=$($result.PassedCount) Failed=$($result.FailedCount) Skipped=$($result.SkippedCount)" -ForegroundColor Cyan
Write-Host "  report.html : $htmlPath"
Write-Host "  results.xml : $($cfg.TestResult.OutputPath.Value)"
Write-Host "  summary.md  : $summaryPath"

# ── Release checklist (clean, jargon-free) ──────────────────────────────────
# Final workflow step: turn the raw test outcomes into doc/release-check-list.md with each
# box filled by what automation verified ([x] = passed, plain [ ] = verify manually). This is
# the human-facing "what's tested / what you still need to run" artifact.
if (-not $SkipReleaseReport) {
    $releaseReport = Join-Path $OutDir 'release-report.md'
    try {
        if ($UpdateReport -and (Test-Path $releaseReport)) {
            # Incremental: overlay only this run's rows onto the existing report.
            & (Join-Path $PSScriptRoot 'Update-ReleaseReport.ps1') -Report $releaseReport -ResultsXml $cfg.TestResult.OutputPath.Value
            Write-Host "  release-report.md : $releaseReport (incrementally updated)" -ForegroundColor Green
        }
        else {
            if ($UpdateReport) { Write-Host "  (-UpdateReport: no existing report at $releaseReport; generating fresh)" -ForegroundColor DarkGray }
            & (Join-Path $PSScriptRoot 'New-ReleaseReport.ps1') -ResultsXml $cfg.TestResult.OutputPath.Value -OutFile $releaseReport
            Write-Host "  release-report.md : $releaseReport (clean release checklist)" -ForegroundColor Green
        }
    }
    catch { Write-Host "  release-report.md : SKIPPED ($($_.Exception.Message))" -ForegroundColor Yellow }
}
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
