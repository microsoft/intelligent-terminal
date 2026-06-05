<#
.SYNOPSIS
  Orchestrator: run one upstream-sync pass. Safe to invoke from a
  scheduler on a weekly/daily cadence.

.DESCRIPTION
  No state.json. Everything is derived from authoritative sources:
    * last-synced watermark        -> Get-LastSyncedUpstreamSha (origin/main trailers)
    * pending list                 -> Get-PendingUpstreamShas (git log --cherry-pick)
    * stuck-lock                   -> Get-StuckIssues (open upstream-sync-stuck labeled issues)

  If any open `upstream-sync-stuck` labeled issue exists, this run skips
  with a `skipped-locked` report and exits 0. Otherwise:
    1. Fetches upstream/main.
    2. Computes pending commits, dropping revert pairs and empties.
    3. Creates branch upstream-sync/YYYY-MM-DD.
    4. Cherry-picks one-by-one with Tier-0/Tier-1 auto-resolution.
       On cherry-pick conflict -> Tier-3 stuck path (07).
    5. Post-batch HARD GATES (in order, before any push/PR):
         a. Toolchain preflight (09)  - missing toolset = infra stuck.
         b. Static breakage scan (08) - duplicate resw / fork invariants.
         c. Try-build (10)            - razzle + bz no_clean.
       Any failure -> Tier-4 stuck path (07b).
    6. Writes a transient report under `Generated Files/upstream-sync/<date>/`
       (gitignored; never committed).
    7. On success -> pushes branch, opens PR (exit 0).
       On Tier-3  -> pushes branch, opens labeled issue (exit 10).
       On Tier-4  -> pushes branch, opens labeled issue (except infra), (exit 10).
       On no-op   -> exits 0 with a "no-op" report.

.PARAMETER DryRun
  Compute & report only; do not create the branch or pick anything.

.PARAMETER TryTier2
  Reserved: enable LLM-assisted Tier-2 conflict resolution (NOT YET IMPLEMENTED).

.PARAMETER Force
  Override the stuck-lock (Tier-3 OR Tier-4). DANGEROUS - clobbers the
  in-progress branch. Use only when you know the lock is stale.

.PARAMETER MaxPicks
  Cap the number of cherry-picks per run (default: unlimited).

.PARAMETER PushDirectToMain
  Skip the PR and fast-forward main directly to the sync branch tip.
  Requires push permission on main.

.PARAMETER AutoMergeStrategy
  PR mode only. After opening the PR, run `gh pr merge --<strategy> --auto`.
  Allowed: 'rebase' (recommended), 'merge', or 'none' (default).

.PARAMETER SkipStaticScan
  Skip step 5b. Default: scan. Schedulers MUST run the scan.

.PARAMETER SkipBuild
  Skip steps 5a + 5c. Default: build. Schedulers MUST build.

.PARAMETER AllowInconclusiveBuild
  Don't treat a build timeout as Tier-4 stuck - proceed with a warning
  in the report. Dev opt-in only; schedulers should leave it off so
  hung builds don't escape into unproven PRs.

.PARAMETER BuildTimeoutMinutes
  Wall-clock cap for try-build. Default 45.

.PARAMETER BuildCommand
  Override the default build command (passed to cmd.exe). Default:
  'tools\razzle.cmd && bz no_clean'.

.OUTPUTS
  Writes status to stdout. Exit codes:
    0  = success (PR opened) OR no-op OR skipped-locked
    10 = stuck (Tier-3 or Tier-4) - NOT an error
    20 = hard failure (git/gh broken) - alarm-worthy
#>
[CmdletBinding()]
param(
    [switch] $DryRun,
    [switch] $TryTier2,
    [switch] $Force,
    [int]    $MaxPicks = 0,
    [switch] $PushDirectToMain,
    [ValidateSet('rebase','merge','none')] [string] $AutoMergeStrategy = 'none',
    [switch] $SkipStaticScan,
    [switch] $SkipBuild,
    [switch] $AllowInconclusiveBuild,
    [int]    $BuildTimeoutMinutes = 45,
    [string] $BuildCommand = 'tools\razzle.cmd && bz no_clean'
)

. "$PSScriptRoot/Common.ps1"

function Exit-Hard([string] $msg) {
    Write-Error $msg
    exit 20
}

function Invoke-Tier4Stuck {
    param(
        $Ctx,
        [string] $Kind,
        [string] $FromSha,
        [string] $ToSha
    )
    $reportPath = & "$PSScriptRoot/05-write-report.ps1" -Ctx $Ctx -From $FromSha -To $ToSha -Status "stuck-$Kind"
    $Ctx.ReportPath = $reportPath
    Write-Host "Tier-4 stuck report: $reportPath"
    $issueUrl = & "$PSScriptRoot/07b-open-validation-stuck-issue.ps1" -Ctx $Ctx -ReportPath $reportPath -Kind $Kind
    if ($issueUrl) { Write-Host "Stuck issue: $issueUrl" -ForegroundColor Yellow }
    exit 10
}

try {
    $ctx = New-RunContext

    # Fast-forward local main from origin BEFORE any state-derivation calls
    # so Get-LastSyncedUpstreamSha / Get-PendingUpstreamShas see the
    # authoritative refs. A stale local clone would otherwise compute a
    # wrong pending list (or repeat picks already on origin/main from a
    # concurrent run on another host). Worktree cleanliness is checked
    # first so an unrelated dirty file can't block the FF mid-script.
    Assert-CleanWorktree
    git switch main 2>&1 | Out-Host
    if ($LASTEXITCODE -ne 0) { Exit-Hard "git switch main failed." }
    git pull --ff-only origin main 2>&1 | Out-Host
    if ($LASTEXITCODE -ne 0) { Exit-Hard "git pull --ff-only origin main failed." }

    # --- Stuck-lock gate ---
    # Derived from open `upstream-sync-stuck` labeled issues. Any open
    # issue with that label blocks the scheduler until a human closes it
    # (the close acts as the "lock cleared" signal - no clear-stuck.ps1
    # needed). The gate ALSO needs `upstream` fetched so that the report's
    # range / watermark fields can be computed even when we skip.
    Ensure-UpstreamRemote
    git fetch upstream main --no-tags 2>&1 | Out-Host
    if ($LASTEXITCODE -ne 0) { Exit-Hard "git fetch upstream main failed." }

    if (-not $Force) {
        $stuck = Get-StuckIssues
        if ($stuck.Count -gt 0) {
            $first = $stuck[0]
            $meta  = Get-StuckMetaFromIssue -Issue $first
            $lockDesc = if ($meta -and ($meta.PSObject.Properties.Name -contains 'tier')) {
                "$($meta.tier) at $($first.url)"
            } else {
                "labeled issue $($first.url)"
            }
            Write-Host "Stuck-lock set ($lockDesc). Skipping. Close the issue to clear the lock." -ForegroundColor Yellow
            $fromSha = try { Get-LastSyncedUpstreamSha } catch { '(unknown)' }
            $reportPath = & "$PSScriptRoot/05-write-report.ps1" -Ctx $ctx -From $fromSha -To $fromSha -Status 'skipped-locked'
            Write-Host "Skip report: $reportPath"
            exit 0
        }
    }

    # --- Existing-PR gate ---
    # Don't open a second concurrent upstream-sync PR. Same stderr-temp-file
    # pattern as everywhere else: a gh banner on stderr must not be merged
    # into stdout (would break ConvertFrom-Json on the JSON payload).
    if (-not $Force -and -not $PushDirectToMain -and -not $DryRun) {
        $errFile = [System.IO.Path]::GetTempFileName()
        $existingJson = $null
        try {
            $existingJson = gh pr list --repo microsoft/intelligent-terminal --state open --limit 200 --json number,headRefName,url 2>$errFile
            $ghExit = $LASTEXITCODE
            if ($ghExit -ne 0) {
                $errText = if (Test-Path -LiteralPath $errFile) { (Get-Content -Raw -LiteralPath $errFile) } else { '' }
                Exit-Hard "gh pr list failed (exit $ghExit): $errText. The existing-PR gate requires gh to be installed and authenticated. Re-run with -Force to bypass (at your own risk), or with -DryRun / -PushDirectToMain to skip the gate."
            }
        }
        finally {
            Remove-Item -LiteralPath $errFile -Force -ErrorAction SilentlyContinue
        }
        if ($existingJson) {
            $existing = @($existingJson | ConvertFrom-Json) | Where-Object { $_.headRefName -like 'upstream-sync/*' }
            if ($existing.Count -gt 0) {
                $first = $existing[0]
                Write-Host "An upstream-sync PR is already open: #$($first.number) ($($first.headRefName)) -> $($first.url). Skipping until it merges or is closed (use -Force to override)." -ForegroundColor Yellow
                $fromSha = try { Get-LastSyncedUpstreamSha } catch { '(unknown)' }
                $reportPath = & "$PSScriptRoot/05-write-report.ps1" -Ctx $ctx -From $fromSha -To $fromSha -Status 'skipped-pr-open'
                Write-Host "Skip report: $reportPath"
                exit 0
            }
        }
    }

    Assert-CleanWorktree

    # --- 1. Resolve from/to (upstream already fetched above) ---
    $toSha   = (git rev-parse upstream/main).Trim()
    if ($LASTEXITCODE -ne 0) { Exit-Hard "git rev-parse upstream/main failed." }
    $fromSha = Get-LastSyncedUpstreamSha

    if ($toSha -eq $fromSha) {
        Write-Host "Already at upstream HEAD ($toSha). No-op." -ForegroundColor Green
        $reportPath = & "$PSScriptRoot/05-write-report.ps1" -Ctx $ctx -From $fromSha -To $toSha -Status 'no-op'
        Write-Host "No-op report: $reportPath"
        exit 0
    }

    # --- 2. Compute pending ---
    $pendingJson = & "$PSScriptRoot/02-compute-pending.ps1"
    $pending = $pendingJson | ConvertFrom-Json
    Write-Host ("Pending: {0} commits, {1} revert pairs dropped, {2} empties dropped." -f $pending.pending.Count, $pending.dropped_pairs.Count, $pending.skipped_empty.Count)

    $ctx.Pending = @($pending.pending)
    $ctx.DroppedPairs = @($pending.dropped_pairs)
    $ctx.SkippedEmpty = @($pending.skipped_empty)

    if ($pending.pending.Count -eq 0) {
        Write-Host "Nothing to pick after filtering. Effective no-op." -ForegroundColor Green
        $reportPath = & "$PSScriptRoot/05-write-report.ps1" -Ctx $ctx -From $fromSha -To $toSha -Status 'no-op'
        Write-Host "Report: $reportPath"
        exit 0
    }

    if ($DryRun) {
        Write-Host "DryRun: skipping branch creation and cherry-picks." -ForegroundColor Cyan
        $reportPath = & "$PSScriptRoot/05-write-report.ps1" -Ctx $ctx -From $fromSha -To $toSha -Status 'dry-run'
        Write-Host "DryRun report: $reportPath"
        exit 0
    }

    # Capture pre-pick base SHA (origin/main) - used as static-scan baseline.
    $preBase = (git rev-parse origin/main).Trim()
    if ($LASTEXITCODE -ne 0) { Exit-Hard "Could not resolve origin/main for scan baseline." }

    # --- 3. Create / switch to sync branch ---
    $branch = $ctx.Branch
    git switch -c $branch 2>$null
    if ($LASTEXITCODE -ne 0) {
        git switch $branch 2>&1 | Out-Host
        if ($LASTEXITCODE -ne 0) { Exit-Hard "Could not create or switch to $branch." }
    }

    # --- 4. Cherry-pick loop ---
    $picks = $pending.pending
    if ($MaxPicks -gt 0 -and $picks.Count -gt $MaxPicks) { $picks = $picks[0..($MaxPicks-1)] }

    foreach ($sha in $picks) {
        Write-Host ""
        Write-Host "=== Cherry-pick $sha ===" -ForegroundColor Cyan
        $resJson = & "$PSScriptRoot/03-cherry-pick-one.ps1" -Sha $sha
        $res = $resJson | ConvertFrom-Json
        switch ($res.status) {
            'picked' {
                $ctx.Picked += $sha
                foreach ($p in @($res.tier0_paths)) {
                    $ctx.Tier0 += [pscustomobject] @{ Sha = $sha; Path = $p }
                }
            }
            'skipped-empty' {
                $ctx.SkippedEmpty += $sha
            }
            'stuck' {
                $ctx.StuckSha   = $sha
                $ctx.StuckPaths = @($res.conflict_paths)
                $ctx.StuckError = if ($res.PSObject.Properties.Name -contains 'error') { [string]$res.error } else { $null }
                $ctx.Status     = 'stuck'
                $errSuffix = if ($ctx.StuckError) { " (error: $($ctx.StuckError))" } else { '' }
                if ($ctx.StuckPaths.Count -gt 0) {
                    Write-Warning "Stuck at $sha on paths: $($ctx.StuckPaths -join ', ')$errSuffix"
                } else {
                    Write-Warning "Stuck at $sha - no conflict paths reported$errSuffix"
                }
                break
            }
            default { Exit-Hard "Unknown cherry-pick-one status: $($res.status)" }
        }
        if ($ctx.Status -eq 'stuck') { break }
    }

    # --- 5. Tier-3 short-circuit ---
    if ($ctx.Status -eq 'stuck') {
        $reportPath = & "$PSScriptRoot/05-write-report.ps1" -Ctx $ctx -From $fromSha -To $toSha -Status 'stuck'
        $ctx.ReportPath = $reportPath
        Write-Host "Stuck report: $reportPath"
        $issueUrl = & "$PSScriptRoot/07-open-stuck-issue.ps1" -Ctx $ctx -ReportPath $reportPath
        Write-Host "Stuck issue: $issueUrl" -ForegroundColor Yellow
        exit 10
    }

    # --- 5a. Toolchain preflight (Tier-4 gate: infra-missing) ---
    if (-not $SkipBuild) {
        Write-Host ""
        Write-Host "=== Toolchain preflight ===" -ForegroundColor Cyan
        $preflightJson = & "$PSScriptRoot/09-toolchain-preflight.ps1"
        $ctx.Preflight = $preflightJson | ConvertFrom-Json
        Write-Host "Required: $($ctx.Preflight.required_toolsets -join ', '); available: $($ctx.Preflight.available_toolsets -join ', ')"
        if (-not $ctx.Preflight.ok) {
            Write-Warning "Toolchain preflight FAILED - missing: $($ctx.Preflight.missing -join ', ')"
            Invoke-Tier4Stuck -Ctx $ctx -Kind 'toolchain-missing' -FromSha $fromSha -ToSha $toSha
        }
    }

    # --- 5b. Static breakage scan (Tier-4 gate: scan-blocking) ---
    if (-not $SkipStaticScan) {
        Write-Host ""
        Write-Host "=== Static breakage scan ===" -ForegroundColor Cyan
        $scanJson = & "$PSScriptRoot/08-static-scan.ps1" -BaseSha $preBase -HeadRef 'HEAD'
        $ctx.Scan = $scanJson | ConvertFrom-Json
        $sm = $ctx.Scan.summary
        Write-Host "Findings: critical=$($sm.critical), high=$($sm.high), medium=$($sm.medium), low=$($sm.low), info=$($sm.info); blocking=$($ctx.Scan.blocking)"
        if ($ctx.Scan.blocking) {
            Invoke-Tier4Stuck -Ctx $ctx -Kind 'static-scan' -FromSha $fromSha -ToSha $toSha
        }
    }

    # --- 5c. Try-build (Tier-4 gate: build-failed / build-inconclusive) ---
    if (-not $SkipBuild) {
        Write-Host ""
        Write-Host "=== Try-build (timeout ${BuildTimeoutMinutes}m) ===" -ForegroundColor Cyan
        $buildJson = & "$PSScriptRoot/10-try-build.ps1" -BuildCommand $BuildCommand -TimeoutMinutes $BuildTimeoutMinutes
        $ctx.Build = $buildJson | ConvertFrom-Json
        Write-Host "Build: $($ctx.Build.kind) (exit=$($ctx.Build.exit_code), duration=$([int]($ctx.Build.duration_ms / 1000))s)"
        switch ($ctx.Build.kind) {
            'build-failed'       { Invoke-Tier4Stuck -Ctx $ctx -Kind 'build-failed'       -FromSha $fromSha -ToSha $toSha }
            'build-inconclusive' {
                if ($AllowInconclusiveBuild) {
                    Write-Warning "Build inconclusive - proceeding (--AllowInconclusiveBuild)."
                } else {
                    Invoke-Tier4Stuck -Ctx $ctx -Kind 'build-inconclusive' -FromSha $fromSha -ToSha $toSha
                }
            }
            'build-ok' { Write-Host "Build OK." -ForegroundColor Green }
        }
    }

    # --- 6. Report + finalize ---
    $ctx.Status = 'ok'
    $reportPath = & "$PSScriptRoot/05-write-report.ps1" -Ctx $ctx -From $fromSha -To $toSha -Status 'ok'
    $ctx.ReportPath = $reportPath
    Write-Host "Report: $reportPath"

    if ($PushDirectToMain) {
        # No more state.json -> no backfill commit needed. Just push the
        # sync branch's commits directly onto main as a fast-forward.
        git switch main | Out-Host
        if ($LASTEXITCODE -ne 0) { Exit-Hard "git switch main failed before direct-push." }
        git merge --ff-only $branch | Out-Host
        if ($LASTEXITCODE -ne 0) { Exit-Hard "git merge --ff-only $branch failed (main moved during the run?)." }
        git push origin main | Out-Host
        if ($LASTEXITCODE -ne 0) { Exit-Hard "git push origin main failed; sync content is local only." }
        $mainHead = (git rev-parse HEAD).Trim()
        Write-Host ""
        Write-Host ("[OK] Sync fast-forwarded onto main at " + $mainHead.Substring(0,9)) -ForegroundColor Green
        exit 0
    }

    $prUrl = & "$PSScriptRoot/06-finalize-pr.ps1" -Ctx $ctx -To $toSha -ReportPath $reportPath -AutoMergeStrategy $AutoMergeStrategy
    Write-Host ""
    Write-Host "[OK] Sync PR opened: $prUrl" -ForegroundColor Green
    exit 0
}
catch {
    Write-Error $_.Exception.Message
    Write-Error $_.ScriptStackTrace
    exit 20
}
