<#
.SYNOPSIS
    Re-request a Copilot code review on a pull request.

.DESCRIPTION
    Uses `gh pr edit --add-reviewer copilot-pull-request-reviewer` to trigger
    a fresh Copilot review. This form is idempotent and is the only one that
    currently works for the Copilot reviewer bot — see
    ../references/api-quirks.md for why the GraphQL and REST alternatives
    fail.

.PARAMETER Owner
    Repository owner (org or user). Defaults to the current repo's owner
    (resolved via `gh repo view`).

.PARAMETER Repo
    Repository name. Defaults to the current repo's name.

.PARAMETER PrNumber
    The pull request number to re-request review on.

.EXAMPLE
    pwsh 01-request-review.ps1 -PrNumber 122
#>
[CmdletBinding()]
param(
    [string]$Owner,
    [string]$Repo,

    [Parameter(Mandatory = $true)]
    [int]$PrNumber
)

$ErrorActionPreference = 'Stop'

if (-not $Owner -or -not $Repo) {
    $repoInfo = gh repo view --json owner,name | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) {
        throw "gh repo view failed (exit $LASTEXITCODE). Pass -Owner and -Repo explicitly or run from inside a gh-detected repo."
    }
    if (-not $Owner) { $Owner = $repoInfo.owner.login }
    if (-not $Repo)  { $Repo  = $repoInfo.name }
}

$repoArg = "$Owner/$Repo"
gh pr edit $PrNumber --repo $repoArg --add-reviewer copilot-pull-request-reviewer
if ($LASTEXITCODE -ne 0) {
    throw "gh pr edit failed with exit code $LASTEXITCODE while requesting Copilot review on PR #$PrNumber."
}
