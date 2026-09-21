[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$BaseSha,
    [Parameter(Mandatory)][string]$HeadSha,
    [Parameter(Mandatory)][string]$OutputPath
)

$ErrorActionPreference = 'Stop'

function Test-ExactCommit {
    param([Parameter(Mandatory)][string]$Revision)

    if ($Revision -notmatch '^[0-9a-fA-F]{40}$') {
        throw "Revision must be an exact 40-character hexadecimal SHA: '$Revision'."
    }

    & git --no-replace-objects cat-file -e "$Revision^{commit}" 2>$null
    if ($LASTEXITCODE -ne 0) {
        throw "Revision is not an available commit: '$Revision'."
    }
}

function Get-Surface {
    param([Parameter(Mandatory)][string]$Path)

    $normalized = $Path.Replace('\', '/')
    $surfaces = [System.Collections.Generic.List[string]]::new()
    $isResw = $normalized -match '/Resources/(?:.+/)?[^/]+\.resw$'

    if ($normalized -match '\.xaml$' -or
        ($normalized -match '^src/cascadia/(TerminalApp|TerminalSettingsEditor|TerminalControl)/' -and -not $isResw)) {
        $surfaces.Add('xaml-ui')
    }
    if ($isResw) {
        $surfaces.Add('resw')
    }
    if ($normalized -match '^tools/wta/locales/[^/]+\.yml$') {
        $surfaces.Add('rust-localization')
    }
    if ($normalized -match '^tools/wta/.+\.rs$') {
        $surfaces.Add('rust-ui')
    }
    if ($normalized -match '^src/cascadia/(TerminalSettingsModel|TerminalProtocol)/' -or
        $normalized -match '^src/cascadia/WindowsTerminal/TerminalProtocolComServer\.' -or
        $normalized -match '^tools/wta/src/protocol/') {
        $surfaces.Add('persistence-protocol')
    }
    if ($normalized -match '^src/(buffer|terminal|renderer|types)/') {
        $surfaces.Add('terminal-semantics')
    }
    if ($surfaces.Count -eq 0) {
        $surfaces.Add('product-code')
    }

    return @($surfaces | Sort-Object -Unique)
}

function Get-Checks {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string[]]$Surfaces
    )

    $checks = [System.Collections.Generic.List[string]]::new()
    if ($Surfaces -contains 'xaml-ui' -or $Surfaces -contains 'rust-ui') {
        $checks.Add('rtl-layout')
        $checks.Add('mixed-direction-text')
        $checks.Add('keyboard-focus-order')
        $checks.Add('expansion-clipping')
    }
    if ($Surfaces -contains 'resw' -or $Surfaces -contains 'rust-localization') {
        $checks.Add('message-construction')
        $checks.Add('placeholder-reordering')
        $checks.Add('translator-context')
    }
    if ($Surfaces -contains 'persistence-protocol') {
        $checks.Add('locale-invariant-roundtrip')
        $checks.Add('machine-token-exclusion')
    }
    if ($Surfaces -contains 'terminal-semantics') {
        $checks.Add('unicode-boundaries')
        $checks.Add('grapheme-and-cell-width')
        $checks.Add('protocol-semantics-exception')
    }
    if ($Path -match '\.(cpp|cxx|cc|h|hpp|rs)$') {
        $checks.Add('unicode-indexing')
        $checks.Add('comparison-and-casing')
    }
    return @($checks | Sort-Object -Unique)
}

Test-ExactCommit -Revision $BaseSha
Test-ExactCommit -Revision $HeadSha

$rows = @(& git --no-replace-objects diff --no-ext-diff --no-textconv --name-status --find-renames $BaseSha $HeadSha --)
if ($LASTEXITCODE -ne 0) {
    throw 'git diff failed while enumerating the immutable change set.'
}

$files = [System.Collections.Generic.List[object]]::new()
foreach ($row in $rows) {
    if ([string]::IsNullOrWhiteSpace($row)) {
        continue
    }
    $parts = $row -split "`t"
    $status = $parts[0]
    $path = if ($status.StartsWith('R') -or $status.StartsWith('C')) { $parts[-1] } else { $parts[1] }
    if ([string]::IsNullOrWhiteSpace($path)) {
        throw "Could not parse git diff row: '$row'."
    }

    $normalized = $path.Replace('\', '/')
    $surfaces = @(Get-Surface -Path $normalized)
    $hunks = [System.Collections.Generic.List[object]]::new()
    $diffLines = @(& git --no-replace-objects diff --no-ext-diff --no-textconv --unified=0 --format= $BaseSha $HeadSha -- $normalized)
    if ($LASTEXITCODE -ne 0) {
        throw "git diff failed while identifying changed lines for '$normalized'."
    }
    foreach ($diffLine in $diffLines) {
        if ($diffLine -match '^@@ -(?<oldStart>[0-9]+)(?:,(?<oldCount>[0-9]+))? \+(?<newStart>[0-9]+)(?:,(?<newCount>[0-9]+))? @@') {
            $hunks.Add([ordered]@{
                oldStart = [int]$Matches.oldStart
                oldCount = if ([string]::IsNullOrEmpty($Matches.oldCount)) { 1 } else { [int]$Matches.oldCount }
                newStart = [int]$Matches.newStart
                newCount = if ([string]::IsNullOrEmpty($Matches.newCount)) { 1 } else { [int]$Matches.newCount }
            })
        }
    }
    $files.Add([ordered]@{
        path = $normalized
        status = $status
        hunks = @($hunks)
        surfaces = $surfaces
        checks = @(Get-Checks -Path $normalized -Surfaces $surfaces)
        customerFacingLikelihood = if (
            $surfaces -contains 'xaml-ui' -or
            $surfaces -contains 'resw' -or
            $surfaces -contains 'rust-ui' -or
            $surfaces -contains 'rust-localization'
        ) { 'likely' } elseif (
            $surfaces -contains 'persistence-protocol' -or
            $surfaces -contains 'terminal-semantics'
        ) { 'conditional' } else { 'unknown' }
    })
}

$report = [ordered]@{
    version = 1
    baseSha = $BaseSha.ToLowerInvariant()
    headSha = $HeadSha.ToLowerInvariant()
    files = @($files.ToArray())
}

$parent = Split-Path -Path $OutputPath -Parent
if (-not [string]::IsNullOrWhiteSpace($parent)) {
    [System.IO.Directory]::CreateDirectory($parent) | Out-Null
}
[System.IO.File]::WriteAllText(
    $OutputPath,
    ($report | ConvertTo-Json -Depth 8),
    [System.Text.UTF8Encoding]::new($false))
