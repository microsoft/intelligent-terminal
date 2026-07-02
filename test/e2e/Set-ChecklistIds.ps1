<#
.SYNOPSIS
    Assign STABLE per-item IDs (`C001`, `C002`, …) to every checkbox item in the release checklist,
    so items can be referenced by number across the checklist AND the generated release-report.md.

.DESCRIPTION
    Injects `` `Cnnn` `` right after the checkbox on each `- [ ]` / `- [x]` item line that does not
    already have one. IDEMPOTENT and STABLE: existing IDs are never changed or renumbered, and new
    items get the next number after the current maximum — so adding an item later just runs this
    again to give the new rows fresh IDs, and every previously-assigned number keeps pointing at the
    same item. Non-item bullets (e.g. the "Coverage markers" legend) are skipped (no checkbox).

    The report generators (New-ReleaseReport.ps1 / Update-ReleaseReport.ps1) carry the ID through to
    release-report.md, placed consistently right after the box. The `` `Cnnn` `` token has no square
    brackets, so it survives the report's tag-stripping (which only removes `` `[TAG]` `` markers).

.PARAMETER Checklist  The checklist to number in place. Default doc/release-check-list.md.
.PARAMETER WhatIf     Show what would change without writing.

.EXAMPLE
    pwsh -File test/e2e/Set-ChecklistIds.ps1
#>
[CmdletBinding(SupportsShouldProcess)]
param(
    [string]$Checklist = (Join-Path $PSScriptRoot '..\..\doc\release-check-list.md')
)

$ErrorActionPreference = 'Stop'
if (-not (Test-Path $Checklist)) { throw "Set-ChecklistIds: checklist not found: $Checklist" }

$lines = Get-Content -LiteralPath $Checklist

# Find the current max ID so new assignments continue from it (stability on re-run / appends).
$max = 0
foreach ($l in $lines) {
    if ($l -match '`C(\d+)`') { $n = [int]$Matches[1]; if ($n -gt $max) { $max = $n } }
}

$assigned = 0
$out = for ($i = 0; $i -lt $lines.Count; $i++) {
    $line = $lines[$i]
    # Checkbox item line: "- [ ] rest" or "- [x] rest" (any indent). Non-item bullets are skipped.
    if ($line -match '^(?<pre>\s*-\s*\[[ xX]\]\s*)(?<rest>.*)$') {
        $rest = $Matches['rest']
        if ($rest -match '^`C\d+`') {
            $line   # already has an ID — leave untouched
        }
        else {
            $max++
            $assigned++
            $id = '`C{0:D3}`' -f $max
            "$($Matches['pre'])$id $rest"
        }
    }
    else {
        $line
    }
}

if ($assigned -eq 0) {
    Write-Host "Set-ChecklistIds: all items already numbered (max=C$('{0:D3}' -f $max)); nothing to do." -ForegroundColor DarkGray
    return
}

if ($PSCmdlet.ShouldProcess($Checklist, "assign $assigned new item ID(s)")) {
    Set-Content -LiteralPath $Checklist -Value ($out -join "`n") -Encoding UTF8
    Write-Host "Set-ChecklistIds: assigned $assigned new ID(s); highest is C$('{0:D3}' -f $max)." -ForegroundColor Green
}
