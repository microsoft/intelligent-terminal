<#
.SYNOPSIS
  Orchestrator: run one upstream-sync pass. Safe to invoke from a
  scheduler on a weekly/daily cadence.

.DESCRIPTION
  Reads state.json. If the stuck-lock (Tier-3 stuck_on_sha OR Tier-4
  stuck_validation) is set, writes a skipped-locked report and exits 0.
  Otherwise:
    1. Fetches upstream/main.
    2. Computes pending commits, dropping revert pairs and empties.
    3. Creates branch upstream-sync/YYYY-MM-DD.
    4. Cherry-picks one-by-one with Tier-0/Tier-1 auto-resolution.
       On cherry-pick conflict → Tier-3 stuck path (07).
    5. Post-batch HARD GATES (in order, before any push/PR):
         a. Toolchain preflight (09)  — missing toolset = infra stuck.
         b. Static breakage scan (08) — duplicate resw / fork invariants.
         c. Try-build (10)            — razzle + bz no_clean.
       Any failure → Tier-4 stuck path (07b).
    6. Writes a report.
    7. On success → pushes branch, opens PR (exit 0).
       On Tier-3 → pushes branch, opens issue, sets lock (exit 10).
       On Tier-4 → pushes branch, opens issue (except infra), sets lock (exit 10).
       On no-op  → exits 0 with a "no-op" report.

.PARAMETER DryRun
  Compute & report only; do not create the branch or pick anything.

.PARAMETER TryTier2
  Reserved: enable LLM-assisted Tier-2 conflict resolution (NOT YET IMPLEMENTED).

.PARAMETER Force
  Override the stuck-lock (Tier-3 OR Tier-4). DANGEROUS — clobbers the
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
  Don't treat a build timeout as Tier-4 stuck — proceed with a warning
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
    10 = stuck (Tier-3 or Tier-4) — NOT an error
    20 = hard failure (git/gh broken) — alarm-worthy
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
    $state = Read-State
    $ctx = New-RunContext

    # --- Stuck-lock gate (Tier-3 OR Tier-4) ---
    $stuckTier3 = [bool] $state.stuck_on_sha
    $stuckTier4 = [bool] $state.stuck_validation
    if (($stuckTier3 -or $stuckTier4) -and -not $Force) {
        $lockDesc = if ($stuckTier3) {
            "Tier-3 at $($state.stuck_on_sha) (issue: $($state.stuck_issue_url))"
        } else {
            $v = $state.stuck_validation
            "Tier-4 $($v.kind) [hash $($v.findings_hash)] (issue: $($v.issue_url))"
        }
        Write-Host "Stuck-lock set: $lockDesc. Skipping." -ForegroundColor Yellow
        $reportPath = & "$PSScriptRoot/05-write-report.ps1" -Ctx $ctx -From $state.last_synced_upstream_sha -To $state.last_synced_upstream_sha -Status 'skipped-locked'
        Write-Host "Skip report: $reportPath"
        exit 0
    }

    # --- Existing-PR gate ---
    # Schedulers run unattended. If a prior run already opened an
    # upstream-sync PR that hasn't merged yet, our baseline (origin/main)
    # is unchanged, so we'd recompute the SAME pending range and then
    # either (a) collide on branch creation or (b) 06-finalize-pr.ps1
    # would fail because the PR already exists for that branch. Worse,
    # under a per-day branch-name scheme the second run would open a
    # NEW PR with identical content. Bail early with a no-op report
    # instead, unless -Force is given. Skipped entirely under
    # -PushDirectToMain, which never opens a PR and shouldn't require
    # `gh` auth on the host.
    # Skip the existing-PR gate when there's nothing to publish anyway:
    # -PushDirectToMain never opens a PR, and -DryRun stops well before
    # 06-finalize-pr.ps1 — neither should require `gh` auth on the host.
    if (-not $Force -and -not $PushDirectToMain -and -not $DryRun) {
        # Drop the GitHub `head:` search qualifier — it matches exact
        # branch names, not prefixes, so `head:upstream-sync/` would
        # return nothing even when an `upstream-sync/2026-06-04` PR is
        # open. List all open PRs (--limit 200 covers the corner case
        # where a repo has more than the default 30 open) and filter
        # client-side by headRefName.
        #
        # Capture stdout (JSON) and stderr separately: merging them with
        # `2>&1` breaks ConvertFrom-Json when `gh` emits any warning or
        # progress text on stderr even at exit 0 (e.g. version-update
        # notice, deprecation warning). stderr is only used for the
        # failure message.
        $errFile = [System.IO.Path]::GetTempFileName()
        try {
            $existingJson = gh pr list --repo microsoft/intelligent-terminal --state open --limit 200 --json number,headRefName,url 2>$errFile
            $ghExit = $LASTEXITCODE
            if ($ghExit -ne 0) {
                # `gh` missing / not authenticated / network-blocked. Don't
                # silently continue and waste a full pick + scan + build
                # only to fail later in 06-finalize-pr.ps1 — fail fast.
                $errText = if (Test-Path $errFile) { (Get-Content -Raw -LiteralPath $errFile) } else { '' }
                Exit-Hard "gh pr list failed (exit $ghExit): $errText. The existing-PR gate requires gh to be installed and authenticated. Re-run with -Force to bypass (at your own risk), or with -DryRun / -PushDirectToMain to skip the gate."
            }
        }
        finally {
            Remove-Item -LiteralPath $errFile -ErrorAction SilentlyContinue
        }
        if ($existingJson) {
            $existing = @($existingJson | ConvertFrom-Json) | Where-Object { $_.headRefName -like 'upstream-sync/*' }
            if ($existing.Count -gt 0) {
                $first = $existing[0]
                Write-Host "An upstream-sync PR is already open: #$($first.number) ($($first.headRefName)) -> $($first.url). Skipping until it merges or is closed (use -Force to override)." -ForegroundColor Yellow
                $reportPath = & "$PSScriptRoot/05-write-report.ps1" -Ctx $ctx -From $state.last_synced_upstream_sha -To $state.last_synced_upstream_sha -Status 'skipped-pr-open'
                Write-Host "Skip report: $reportPath"
                exit 0
            }
        }
    }

    Assert-CleanWorktree
    git switch main 2>&1 | Out-Host
    if ($LASTEXITCODE -ne 0) { Exit-Hard "git switch main failed." }
    git pull --ff-only origin main 2>&1 | Out-Host
    if ($LASTEXITCODE -ne 0) { Exit-Hard "git pull --ff-only origin main failed." }

    # --- 1. Fetch upstream ---
    $toSha = (& "$PSScriptRoot/01-fetch-upstream.ps1").Trim()
    $fromSha = $state.last_synced_upstream_sha

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

    # Capture pre-pick base SHA (origin/main) — used as static-scan baseline.
    # Trim defensively: native git output occasionally carries a trailing \r
    # depending on shim, and an untrimmed SHA breaks `"$Base..$Head"` ranges.
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
                    Write-Warning "Stuck at $sha — no conflict paths reported$errSuffix"
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
            Write-Warning "Toolchain preflight FAILED — missing: $($ctx.Preflight.missing -join ', ')"
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
                    Write-Warning "Build inconclusive — proceeding (--AllowInconclusiveBuild)."
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
        $mainHead = & "$PSScriptRoot/06b-finalize-direct.ps1" -Ctx $ctx -To $toSha -ReportPath $reportPath
        Write-Host ""
        Write-Host "✅ Sync fast-forwarded onto main at $($mainHead.Substring(0,9))" -ForegroundColor Green
        exit 0
    }

    $prUrl = & "$PSScriptRoot/06-finalize-pr.ps1" -Ctx $ctx -To $toSha -ReportPath $reportPath -AutoMergeStrategy $AutoMergeStrategy
    Write-Host ""
    Write-Host "✅ Sync PR opened: $prUrl" -ForegroundColor Green
    exit 0
}
catch {
    Write-Error $_.Exception.Message
    Write-Error $_.ScriptStackTrace
    exit 20
}
