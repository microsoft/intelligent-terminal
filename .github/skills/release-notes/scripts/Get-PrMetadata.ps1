<#
.SYNOPSIS
    Look up GitHub PRs to find linked issues and community contributors.

.DESCRIPTION
    Given a list of PR numbers, queries the GitHub API via `gh` CLI to find:
    1. Which PRs have linked/closing issues
    2. Which PRs were authored by community members (not core team)

.PARAMETER PRNumbers
    Array of PR numbers to look up.

.PARAMETER Repo
    GitHub repository in owner/repo format. Default: microsoft/intelligent-terminal

.EXAMPLE
    .\Get-PrMetadata.ps1 -PRNumbers 206,208,219 -Repo microsoft/intelligent-terminal
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [int[]]$PRNumbers,

    # Intelligent Terminal PRs live on the canonical repo, not on personal
    # forks/clones, so this defaults to microsoft/intelligent-terminal on
    # purpose (a fork wouldn't have the PRs). Override -Repo to microsoft/terminal
    # for upstream #20xxx PRs, or to another repo if needed.
    [string]$Repo = 'microsoft/intelligent-terminal'
)

# Core team members — do NOT thank as community contributors.
# Source of truth is references/core-team.md, parsed at runtime so the
# script and the docs can't drift. Falls back to a built-in list only
# if that reference file is missing or unparseable.
$coreTeamRef = [IO.Path]::Combine($PSScriptRoot, '..', 'references', 'core-team.md')
$CoreTeam = @()
if (Test-Path $coreTeamRef) {
    $CoreTeam = Get-Content $coreTeamRef |
        Where-Object { $_ -match '^\s*\|' } |
        ForEach-Object { ($_ -split '\|')[1].Trim() } |
        Where-Object { $_ -and $_ -notmatch '^-+$' -and $_ -ne 'GitHub Username' }
}
if (-not $CoreTeam -or $CoreTeam.Count -eq 0) {
    $CoreTeam = @(
        'yeelam-gordon', 'vanzue', 'DDKinger', 'haonanttt',
        'hamza-usmani', 'pabhojwa'
    )
}

$results = @{
    WithIssues       = @()
    CommunityAuthors = @()
    NoIssues         = @()
}

foreach ($pr in $PRNumbers) {
    # Capture stdout and stderr separately (stderr -> temp file) so a
    # 'gh' warning/progress line can never corrupt the JSON stream fed
    # to ConvertFrom-Json. Mirrors the repo's Invoke-Gh pattern in
    # .github/skills/copilot-pr-review-loop/scripts/_lib.ps1.
    $errFile = [IO.Path]::GetTempFileName()
    try {
        $json = & gh pr view $pr --repo $Repo --json number,title,author,closingIssuesReferences 2>$errFile
        if ($LASTEXITCODE -ne 0) {
            $err = (Get-Content -Raw -LiteralPath $errFile -ErrorAction SilentlyContinue)
            if ($null -eq $err) { $err = '' }
            Write-Warning "Failed to look up PR #$pr (gh exited $LASTEXITCODE): $($err.Trim())"
            continue
        }
        if (-not $json) { continue }
        $data = ($json | Out-String) | ConvertFrom-Json

        # Check for linked issues
        $issues = @()
        if ($data.closingIssuesReferences) {
            $issues = $data.closingIssuesReferences | ForEach-Object { $_.number }
        }

        if ($issues.Count -gt 0) {
            $results.WithIssues += [PSCustomObject]@{
                PR     = $pr
                Title  = $data.title
                Issues = ($issues | ForEach-Object { "#$_" }) -join ', '
            }
        }
        else {
            $results.NoIssues += $pr
        }

        # Check for community contributors (exclude core team and bot accounts)
        $author = $data.author.login
        if ($author -and $author -notin $CoreTeam -and $author -notmatch '\[bot\]$') {
            $results.CommunityAuthors += [PSCustomObject]@{
                PR          = $pr
                Username    = $author
                DisplayName = $data.author.name
            }
        }
    }
    catch {
        Write-Warning "Failed to process PR #$pr`: $_"
    }
    finally {
        if (Test-Path -LiteralPath $errFile) {
            Remove-Item -LiteralPath $errFile -ErrorAction SilentlyContinue
        }
    }
}

# Output
Write-Host "`n## PRs with linked issues" -ForegroundColor Cyan
if ($results.WithIssues.Count -eq 0) { Write-Host "  (none)" }
else {
    $results.WithIssues | ForEach-Object {
        Write-Host "  PR #$($_.PR) → fixes $($_.Issues): $($_.Title)"
    }
}

Write-Host "`n## Community contributors" -ForegroundColor Cyan
if ($results.CommunityAuthors.Count -eq 0) { Write-Host "  (none)" }
else {
    $results.CommunityAuthors | ForEach-Object {
        Write-Host "  PR #$($_.PR) by @$($_.Username) ($($_.DisplayName))"
    }
}

Write-Host "`n## PRs with no linked issues" -ForegroundColor Cyan
if ($results.NoIssues.Count -eq 0) { Write-Host "  (none)" }
else { Write-Host "  $($results.NoIssues -join ', ')" }

# Return structured data for programmatic use
$results
