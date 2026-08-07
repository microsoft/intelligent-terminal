# Run a sync — full procedure

This is the orchestration the agent executes step-by-step. The high-level
flow and invariants are in [`SKILL.md`](../SKILL.md); this file is the
full runbook, with commands. **Do not skip steps.** Each step's "On
failure" path is mandatory.

## 1. Preconditions (bail fast)

```pwsh
# (a) working tree clean
git status --porcelain
# → empty? continue. nonempty? bail and tell the operator.

# (b) on main, fast-forward
git switch main
git pull --ff-only

# (c) no open stuck-lock
gh issue list -R microsoft/intelligent-terminal --label upstream-sync-stuck --state open --json number,title,url
# → []? continue.
# → nonempty? STOP. Surface the issue URL(s) to the operator and exit.
#   The lock-clear signal is the human closing the issue, not a script.
```

## 2. Build a branch name

```pwsh
$utc    = (Get-Date).ToUniversalTime()
$date   = $utc.ToString('yyyy-MM-dd')
$tstamp = $utc.ToString('HHmmss')
$rand   = [guid]::NewGuid().ToString('N').Substring(0,4)
$branch = "upstream-sync/$date-$tstamp-$rand"
git switch -c $branch
```

The fresh suffix means re-runs never collide with stale local branches.

## 3. Fetch upstream

```pwsh
# Accept any equivalent form pointing at microsoft/terminal — HTTPS with
# or without .git, SSH form, etc. We only care about owner/repo identity.
$expectedOwnerRepo = 'microsoft/terminal'
$existing = git remote get-url upstream 2>$null
function Get-OwnerRepo([string] $url) {
    if (-not $url) { return $null }
    $u = $url.Trim()
    # https://host/owner/repo[.git]
    if ($u -match '^https?://[^/]+/([^/]+/[^/]+?)(?:\.git)?/?$') { return $Matches[1].ToLowerInvariant() }
    # ssh://[user@]host[:port]/owner/repo[.git]
    if ($u -match '^ssh://(?:[^@/]+@)?[^/:]+(?::\d+)?/([^/]+/[^/]+?)(?:\.git)?/?$') { return $Matches[1].ToLowerInvariant() }
    # scp-style: [user@]host:owner/repo[.git]
    if ($u -match '^(?:[^@:/]+@)?[^:/]+:([^/]+/[^/]+?)(?:\.git)?/?$') { return $Matches[1].ToLowerInvariant() }
    return $null
}
if (-not $existing) {
    git remote add upstream "https://github.com/$expectedOwnerRepo.git"
} elseif ((Get-OwnerRepo $existing) -ne $expectedOwnerRepo) {
    throw "Remote 'upstream' points at '$existing' but this skill requires $expectedOwnerRepo. Aborting so we don't silently sync from the wrong fork."
}
git fetch upstream main --no-tags 2>&1 | ForEach-Object { [Console]::Error.WriteLine($_) }
if ($LASTEXITCODE -ne 0) { throw "git fetch upstream main failed." }
$upstreamSha = (git rev-parse upstream/main).Trim()
```

## 4. Compute pending

```pwsh
$pendingJson = pwsh -NoProfile -File .github/skills/upstream-sync/scripts/02-compute-pending.ps1
if ($LASTEXITCODE -ne 0) { throw "02-compute-pending.ps1 exited $LASTEXITCODE." }
$pending     = $pendingJson | ConvertFrom-Json
# $pending.from           = last-synced watermark SHA
# $pending.to             = upstream/main SHA
# $pending.pending        = SHAs to pick, oldest-first
# $pending.dropped_pairs  = [[picked, reverted], ...]  — auto-cancelled
# $pending.skipped_empty  = SHAs the script can statically prove are no-ops
```

If `$pending.pending.Count -eq 0`: nothing to do. Delete the branch, tell
the operator "fork is up to date with <upstreamSha>", exit.

## 5. Cherry-pick loop

For each SHA in `$pending.pending`:

```pwsh
$pickJson = pwsh -NoProfile -File .github/skills/upstream-sync/scripts/03-cherry-pick-one.ps1 -Sha $sha
if ($LASTEXITCODE -ne 0) { throw "03-cherry-pick-one.ps1 exited $LASTEXITCODE for $sha." }
$pick     = $pickJson | ConvertFrom-Json
```

Branch on `$pick.status`:

- `"picked"` — record `$sha` in `$picked`, continue.
- `"skipped-empty"` — record `$sha` in `$skippedEmpty`, continue.
- `"stuck"` — **stop the loop** and go to step 5a.

### 5a. On stuck — open the Tier-3 issue and exit

Push the branch, ensure the lock label exists, and file the issue with a
plain-markdown body. Closing the issue is the lock-clear signal — no
machine-readable metadata is needed.

```pwsh
git push -u origin $branch 2>&1 | ForEach-Object { [Console]::Error.WriteLine($_) }
if ($LASTEXITCODE -ne 0) { throw "git push of $branch failed." }

# Idempotent — `gh label create` returns non-zero when the label already
# exists. We swallow that specific case but surface everything else so an
# auth/repo-typo failure doesn't get silently absorbed.
$labelOut = gh label create 'upstream-sync-stuck' `
    --color B60205 `
    --description 'Upstream sync paused — close to clear the lock' `
    -R microsoft/intelligent-terminal 2>&1
$labelOutText = ($labelOut | Out-String)
if ($LASTEXITCODE -ne 0 -and $labelOutText -notmatch 'already exists') {
    throw "gh label create failed: $labelOutText"
}

$author      = git log -1 --format='%an <%ae>' $sha
$subject     = git log -1 --format='%s' $sha
$shortSha    = $sha.Substring(0,9)
$hasConflicts = $pick.conflict_paths -and $pick.conflict_paths.Count -gt 0
$pathLines   = if ($hasConflicts) {
    ($pick.conflict_paths | ForEach-Object { "- ``$_``" }) -join "`n"
} else { '_(none — cherry-pick failed without producing unmerged paths)_' }
$headline = if ($hasConflicts) {
    'Upstream sync stopped at a conflict that needs human judgment.'
} else {
    'Upstream sync stopped on a non-conflict cherry-pick failure (e.g. merge commit without `-m`, unsupported git option, hook failure).'
}
$body = @"
> [!CAUTION]
> $headline
> **Close this issue when resolved — that IS the lock-clear signal.**

**Stuck on:** ``$sha`` — $subject
**Upstream commit:** https://github.com/microsoft/terminal/commit/$sha
**Author:** $author
**Sync branch:** ``$branch``

**Conflicting paths:**
$pathLines

$(if ($pick.error) { "**Error:** ``$($pick.error)``" })

**To resume** (the script ran ``git cherry-pick --abort`` — the branch
has no in-progress conflict state to resolve directly):

``````pwsh
git fetch origin
git switch $branch
git cherry-pick -x $sha   # re-run to reproduce the conflict; -x keeps the trailer
# ... resolve conflicts ...
git add -A
git cherry-pick --continue
git push
# Open a PR against main, merge without squashing (the trailer must survive),
# then close this issue to clear the lock.
``````

See [references/03-conflict-triage.md](https://github.com/microsoft/intelligent-terminal/blob/main/.github/skills/upstream-sync/references/03-conflict-triage.md) for the resolution rubric.
"@

$bodyFile = New-TemporaryFile
try {
    Set-Content -Encoding utf8 -Path $bodyFile -Value $body
    $issueUrl = gh issue create -R microsoft/intelligent-terminal `
        --title "Upstream sync stuck at $shortSha" `
        --label upstream-sync-stuck `
        --body-file $bodyFile
    if ($LASTEXITCODE -ne 0) { throw "gh issue create failed." }
} finally {
    Remove-Item -Force -ErrorAction SilentlyContinue $bodyFile
}
```

Surface `$issueUrl` and `$branch` to the operator. The human:

1. `git fetch origin && git switch $branch` to pick up the sync branch.
2. **Re-run the cherry-pick to reproduce the conflict** — `03-cherry-pick-one.ps1`
   calls `git cherry-pick --abort` before returning `stuck`, so the branch
   itself has no conflict markers / `MERGE_MSG`. Use the stuck SHA from
   the issue body: `git cherry-pick -x <stuck_sha>` (the `-x` preserves
   the `(cherry picked from commit <sha>)` trailer — critical for the
   watermark).
3. Resolve the conflicts, `git add` the resolutions, `git cherry-pick --continue`.
4. Push, open a PR against `main`, merge it (the `-x` trailer must
   survive the merge — DO NOT squash).
5. Close the stuck issue.

EXIT — do not attempt the build.
See [`03-conflict-triage.md`](./03-conflict-triage.md) for the Tier-0/1/2/3
resolution rubric.

## 6. (No commits picked? exit clean.)

If the loop emitted only `empty` results (rare — every pending commit was
a no-op) there is nothing to build or finalize. Delete the branch, report
"sync completed with 0 commits picked (all empty)", exit.

## 7. Build

### Pre-build check — re-pin the PGO database if upstream bumped its version

This fork keeps its own product version (`custom.props` → `VersionMajor.VersionMinor`
= `0.1`), so `build/pgo/Terminal.PGO.props` **cannot** derive the PGO database
package version automatically — it carries a hard-coded pin to the upstream
Windows Terminal `Major.Minor`. When a sync pulls in an upstream version bump
(e.g. `1.25 → 1.26`), that pin must follow, or the build fails with:

```
Microsoft.PGO-Helpers.Cpp.targets : Error : Could not find matching PGO package.
```

(No `Microsoft.Internal.Windows.Terminal.PGODatabase` is published for `0.1` on the
TerminalDependencies feed — only upstream versions exist.)

Before building, compare the two values — no script needed, just read both:

```
# upstream product version the picked commits may have advanced:
git show upstream/main:custom.props        # → <VersionMajor> / <VersionMinor>

# current pin in build/pgo/Terminal.PGO.props:
#   <PGOPackageVersionMajor> / <PGOPackageVersionMinor>
```

If upstream's `Major.Minor` is newer than the pin, edit those two
`<PGOPackageVersionMajor>` / `<PGOPackageVersionMinor>` values in
`build/pgo/Terminal.PGO.props` to match, and land it as its own mechanical
commit on the sync branch (it's a deterministic, upstream-driven re-pin, not a
judgment fix, so it does not consume the step-7 build-fix budget):

```
git add build/pgo/Terminal.PGO.props
git commit -m "chore(upstream): re-pin PGO database to <maj>.<min>"
```

If the values already match, do nothing and proceed to the build.

### Run the build

```pwsh
$buildJson = pwsh -NoProfile -File .github/skills/upstream-sync/scripts/04-try-build.ps1
if ($LASTEXITCODE -ne 0) { throw "04-try-build.ps1 exited $LASTEXITCODE before producing a JSON result." }
$build     = $buildJson | ConvertFrom-Json
```

Build runs BEFORE finalize on purpose — if it fails, the fix lands as ONE
extra commit on the same branch so it ends up in the same PR.

Branch on `$build.kind`:

- `"build-ok"` — continue to step 8.
- `"build-failed"` — try ONE focused build-fix:
  1. Read `$build.log_tail`. Decide if the failure is small, mechanical, and
     clearly caused by the cherry-pick batch (e.g. duplicate `.resw` key
     from a fork-local commit colliding with an upstream rename, missing
     `#include` resolved by the upstream batch, etc.).
  2. If yes — fix it, `git add` only the affected files, commit with
     subject `chore(upstream): fix build after upstream sync` (carry the
     `Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>`
     trailer if you authored the fix), re-run `04-try-build.ps1`, go back
     to start of step 7.
  3. If no (large change, scope creep, requires design decisions) — go to step 7a.
  4. If the fix attempt itself fails to compile — go to step 7a. Do NOT
     pile up multiple fix commits; the one-fix-per-PR rule is policy.
- `"build-inconclusive"` — go to step 7a (timeout, treated as a real failure
  unless the operator explicitly opts out).

### 7a. On build failure — surface and exit

The fix attempt didn't land or the failure is too big for a one-commit
fix. Don't open an issue. Surface the failure to the operator and exit —
they'll either fix the underlying defect on `main` and re-run, or push a
manual fix commit on top of `$branch` and continue to step 8 by hand.

```pwsh
[Console]::Error.WriteLine("Build failed after upstream sync:")
[Console]::Error.WriteLine("  branch:    $branch")
[Console]::Error.WriteLine("  kind:      $($build.kind)")
[Console]::Error.WriteLine("  exit_code: $($build.exit_code)")
[Console]::Error.WriteLine("  log_path:  $($build.log_path)")
[Console]::Error.WriteLine("--- log tail ---")
[Console]::Error.WriteLine($build.log_tail)
exit 1
```

## 8. Finalize the PR

The agent pushes the branch and opens the PR directly with `git` and
`gh`. There is no wrapper script — there's nothing here that needs more
than `gh pr create` plus a body string composed from `$picked` /
`$build` / `$pending`.

Compose the body:

```pwsh
$banner = @"
> [!WARNING]
> **DO NOT squash-merge this PR.** Squashing collapses every cherry-picked
> upstream commit into one, destroying per-commit attribution, original
> author dates, the ``(cherry picked from commit <sha>)`` trailers that the
> NEXT upstream sync uses as its watermark, and ``git bisect`` resolution.
> Merge with **"Rebase and merge"** (preferred — flat history, all
> $($picked.Count) commit(s) land individually) or **"Create a merge commit"**.

> [!NOTE]
> **Review-fix policy.** Only build-blocking fixes belong on this branch
> as **one** focused extra commit. All other Copilot / human review
> feedback goes into a **follow-up PR** based on this PR's head. See
> [``.github/skills/upstream-sync/references/follow-up-pr.md``](https://github.com/microsoft/intelligent-terminal/blob/main/.github/skills/upstream-sync/references/follow-up-pr.md).

---

"@

$summary = @"
Syncs **$($picked.Count)** commit(s) from microsoft/terminal up to ``$($upstreamSha.Substring(0,9))``.

## Picked
$( ($picked | ForEach-Object { "- ``$($_.Substring(0,9))``  $(git log -1 --format='%s' $_)" }) -join "`n" )

$(if ($pending.dropped_pairs.Count) { @"
## Auto-dropped revert pairs
$( ($pending.dropped_pairs | ForEach-Object { "- ``$($_[0].Substring(0,9))`` ↔ ``$($_[1].Substring(0,9))`` (cancel out within range)" }) -join "`n" )
"@ })

$(if ($skippedEmpty.Count) { @"
## Skipped (empty / already applied)
$( ($skippedEmpty | ForEach-Object { "- ``$($_.Substring(0,9))``" }) -join "`n" )
"@ })

## Build
``04-try-build.ps1`` → ``$($build.kind)`` (exit $($build.exit_code), $($build.duration_ms) ms). Log: ``$($build.log_path)`` (gitignored).
"@

$bodyFile = New-TemporaryFile
[System.IO.File]::WriteAllText($bodyFile, ($banner + $summary), (New-Object System.Text.UTF8Encoding($false)))
```

Push and create the PR. `gh pr create` on Windows can occasionally fail
with "Head sha can't be blank" right after a push — retry up to 3× with
a short delay:

```pwsh
git push -u origin $branch 2>&1 | ForEach-Object { [Console]::Error.WriteLine($_) }
if ($LASTEXITCODE -ne 0) { throw "git push of $branch failed." }

$short = $upstreamSha.Substring(0,9)
$title = "chore(upstream): sync microsoft/terminal up to $short"

$prUrl = $null
for ($attempt = 1; $attempt -le 3; $attempt++) {
    $prOut = gh pr create -R microsoft/intelligent-terminal `
        --base main --head $branch --title $title --body-file $bodyFile 2>&1
    $prExit = $LASTEXITCODE
    $prText = ($prOut | Out-String).Trim()
    if ($prExit -eq 0) {
        $urlLine = ($prOut | Where-Object { "$_" -match '^https://github.com/' } | Select-Object -Last 1)
        if ($urlLine) { $prUrl = "$urlLine".Trim(); break }
    }
    Write-Warning "gh pr create attempt $attempt failed (exit $prExit). Full output:`n$prText"
    Start-Sleep -Seconds 5
}
Remove-Item -LiteralPath $bodyFile -Force
if (-not $prUrl) { throw "gh pr create did not return a URL after 3 attempts." }

# Optional: arm GitHub auto-merge with rebase (NEVER squash).
# Skip this if the operator wants to merge by hand.
gh pr merge -R microsoft/intelligent-terminal $prUrl --rebase --auto --delete-branch
```

Surface `$prUrl` to the operator. Done.
