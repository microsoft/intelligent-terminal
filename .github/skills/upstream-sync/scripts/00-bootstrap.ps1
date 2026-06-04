<#
.SYNOPSIS
  One-time bootstrap: initialize state.json with the baseline upstream SHA.

.DESCRIPTION
  Use this exactly once when the skill is first installed in the repo.
  See references/bootstrap.md for how to discover the right baseline SHA.

.PARAMETER BaselineSha
  The upstream/microsoft/terminal commit SHA the fork is currently
  "caught up to". Must be reachable from upstream/main.

.PARAMETER Force
  Overwrite an existing state.json. Refuses by default to prevent
  accidentally rewinding the baseline.

.EXAMPLE
  pwsh scripts/00-bootstrap.ps1 -BaselineSha 93bdbfaa3d62304f4b50b4ca4484da4dd08e4a1f
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $BaselineSha,
    [switch] $Force
)

. "$PSScriptRoot/Common.ps1"

Ensure-UpstreamRemote
git fetch upstream main --no-tags | Out-Null
if ($LASTEXITCODE -ne 0) { throw "git fetch upstream main failed." }

# Verify the SHA exists on upstream/main and persist the canonical 40-hex form.
$BaselineSha = Resolve-FullCommitSha $BaselineSha
$null = git merge-base --is-ancestor $BaselineSha upstream/main
if ($LASTEXITCODE -ne 0) {
    throw "Baseline SHA $BaselineSha is not an ancestor of upstream/main. Refusing to write state.json."
}

$statePath = Get-StatePath
if ((Test-Path $statePath) -and -not $Force) {
    throw "state.json already exists at $statePath. Pass -Force to overwrite (rewinding the baseline can cause re-picks)."
}

$state = @{
    version                  = 1
    upstream_remote_url      = 'https://github.com/microsoft/terminal.git'
    upstream_branch          = 'main'
    last_synced_upstream_sha = $BaselineSha
    stuck_on_sha             = $null
    stuck_branch             = $null
    stuck_at                 = $null
    stuck_issue_url          = $null
    stuck_validation         = $null
    last_run                 = $null
    history                  = @()
}
Write-State $state

# Stage and commit on a dedicated branch so the human can open the PR.
$branch = 'chore/upstream-sync-bootstrap'
git switch -c $branch 2>$null
if ($LASTEXITCODE -ne 0) {
    git switch $branch | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "Could not create or switch to bootstrap branch '$branch'. Refusing to commit state.json on the current HEAD." }
}

git add -- (Get-StatePath)
if ($LASTEXITCODE -ne 0) { throw "git add of state.json failed." }

git commit -m "chore(upstream-sync): bootstrap baseline at $($BaselineSha.Substring(0,9))" | Out-Host
if ($LASTEXITCODE -ne 0) { throw "git commit failed; bootstrap aborted." }

Write-Host ""
Write-Host "Bootstrap committed on branch '$branch'." -ForegroundColor Green
Write-Host "Next:  git push -u origin $branch  &&  gh pr create"
