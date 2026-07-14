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

    [string]$Repo = 'microsoft/intelligent-terminal'
)

# Core team members — do NOT thank as community contributors
$CoreTeam = @(
    'yeelam-gordon', 'vanzue', 'DDKinger', 'haonanttt',
    'hamza-usmani', 'pabhojwa'
)

$results = @{
    WithIssues       = @()
    CommunityAuthors = @()
    NoIssues         = @()
}

foreach ($pr in $PRNumbers) {
    try {
        $json = gh pr view $pr --repo $Repo --json number,title,author,closingIssuesReferences 2>$null
        if (-not $json) { continue }
        $data = $json | ConvertFrom-Json

        # Check for linked issues
        $issues = @()
        if ($data.closingIssuesReferences) {
            $issues = $data.closingIssuesReferences | ForEach-Object { $_.number }
        }

        if ($issues.Count -gt 0) {
            $results.WithIssues += [PSCustomObject]@{
                PR     = $pr
                Title  = $data.title
                Issues = $issues -join ', '
            }
        }
        else {
            $results.NoIssues += $pr
        }

        # Check for community contributors
        $author = $data.author.login
        if ($author -and $author -notin $CoreTeam) {
            $results.CommunityAuthors += [PSCustomObject]@{
                PR          = $pr
                Username    = $author
                DisplayName = $data.author.name
            }
        }
    }
    catch {
        Write-Warning "Failed to look up PR #$pr`: $_"
    }
}

# Output
Write-Host "`n## PRs with linked issues" -ForegroundColor Cyan
if ($results.WithIssues.Count -eq 0) { Write-Host "  (none)" }
else {
    $results.WithIssues | ForEach-Object {
        Write-Host "  PR #$($_.PR) → fixes #$($_.Issues): $($_.Title)"
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
Write-Host "  $($results.NoIssues -join ', ')"

# Return structured data for programmatic use
$results
