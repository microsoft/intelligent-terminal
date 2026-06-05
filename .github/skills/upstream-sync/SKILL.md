---
name: upstream-sync
description: 'Periodically sync new commits from microsoft/terminal into this manually-forked intelligent-terminal repo by cherry-picking commit-by-commit onto a dated sync branch, auto-skipping revert pairs and empty commits, auto-resolving known take-upstream files, and stopping cleanly on genuine conflicts. The agent (you, reading this file) is the orchestrator — the PowerShell scripts are atomic operations you invoke one at a time. Use when the user asks to "sync upstream", "pull from microsoft/terminal", "run upstream sync", "catch up to upstream", or wires this into a scheduler (weekly/daily). Designed to be safe under repeated unattended runs.'
license: MIT
---

# Upstream Sync (microsoft/terminal → intelligent-terminal)

Cherry-pick commit-by-commit from `https://github.com/microsoft/terminal`
into this fork, preserving per-commit attribution, skipping commits that
cancel each other out, and stopping cleanly the moment a human-judgment
conflict appears.

**You — the agent reading this file — are the orchestrator.** Each step
below is a single atomic call into one of the `scripts/*.ps1` files. Run
them in order, parse their JSON output, and decide where to branch based
on the result. There is intentionally no PowerShell driver that calls
them for you, because every interesting decision (build-fix vs. open
stuck issue, retry vs. bail, finalize vs. dry-run) wants LLM judgment
the operator can audit in your transcript.

## When to Use This Skill

- User asks to "sync upstream", "pull from microsoft/terminal", "catch up to upstream", or "run upstream sync".
- A scheduler invokes the agent on a weekly/daily cadence to walk this file's [Run a sync](#run-a-sync) procedure.
- The previous run left an `upstream-sync-stuck` labeled issue open and the
  human has finished resolving the conflict — **close the issue** (that IS
  the lock-clear signal) and re-run.

## When NOT to Use This Skill

- The user wants a **one-shot rebase** of a single feature branch onto upstream — that's a normal `git rebase`, not this skill.
- An `upstream-sync-stuck` labeled issue is open on `microsoft/intelligent-terminal` — do not re-run; resolve the conflict on the stuck branch first, then close the issue.

## Prerequisites

- `git` 2.38+ (needed for `git cherry-pick --keep-redundant-commits`) and `gh` CLI authenticated against `microsoft/intelligent-terminal`. The credential needs **push to topic branches matching `upstream-sync/*`** and **issue + label create** on the same repo.
- **`origin` MUST point at `microsoft/intelligent-terminal`.** All scripts push to `origin` and the PR / stuck-issue creation passes `-R microsoft/intelligent-terminal` explicitly. If `origin` is a personal fork, the push will land on the fork and the PR will fail to find its head. Use `git remote -v` to verify before running.
- PowerShell 7+ (`pwsh`) on PATH.
- Windows build host with Visual Studio 2022, Windows SDK, `vswhere`, and the repo's `tools\razzle.cmd`/`bz` build environment (build is a hard gate before finalize — see [step 7](#7-build)).
- Remote named `upstream` — the scripts create it if missing.
- **No `state.json` to bootstrap.** Watermark comes from the
  `(cherry picked from commit <sha>)` trailers on `origin/main`. If
  the fork has never used `cherry-pick -x` (or trailers were stripped),
  see [First-time sync](#first-time-sync) below for the one-time operator step.

## State Model (no state file)

Every persistent fact lives in the source that owns it:

| Question | Source of truth |
|---|---|
| What's the last-synced upstream commit? | Newest `(cherry picked from commit <sha>)` trailer on `origin/main` whose target is reachable from `upstream/main`. Derived inline by [`scripts/02-compute-pending.ps1`](./scripts/02-compute-pending.ps1). |
| What's pending? | `git log --cherry-pick --right-only --no-merges origin/main...upstream/main`, then drop SHAs older than (or equal to) the watermark above. Patch-id-based, so a picked-then-reverted commit correctly re-appears. |
| Is the scheduler locked? | Any OPEN issue with the `upstream-sync-stuck` label on `microsoft/intelligent-terminal`. Closing the issue IS the lock-clear signal. |
| What does the lock mean? | The issue body (plain markdown — no machine-parseable block) names the stuck commit, the branch, and the conflicting paths. Closing the issue clears the lock. |
| Where do build logs go? | `Generated Files/upstream-sync/<YYYY-MM-DD>/` — gitignored by the repo root's `**/Generated Files/` rule. Never committed. |

### Why cherry-pick (and not rebase or merge)

| Approach | Why rejected / chosen |
|---|---|
| **Rebase** `upstream/main` | ❌ Fork history contains old "Merge upstream" commits; rebase replays them and explodes conflicts. Verified failure on sister repo `agentic-terminal`. |
| **Merge** `upstream/main` | ⚠️ Works, but collapses the whole sync into one blob commit — kills per-commit review, kills `git bisect`. |
| **Cherry-pick commit-by-commit** | ✅ Preserves authorship + per-commit content, allows mechanical revert-pair skipping, produces a reviewable PR with N small commits. |

## Run a sync

This is the orchestration you (the agent) execute. **Do not skip steps.**
Each step's "On failure" path is mandatory.

### 1. Preconditions (bail fast)

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

### 2. Build a branch name

```pwsh
$utc    = (Get-Date).ToUniversalTime()
$date   = $utc.ToString('yyyy-MM-dd')
$tstamp = $utc.ToString('HHmmss')
$rand   = [guid]::NewGuid().ToString('N').Substring(0,4)
$branch = "upstream-sync/$date-$tstamp-$rand"
git switch -c $branch
```

The fresh suffix means re-runs never collide with stale local branches.

### 3. Fetch upstream

```pwsh
# Accept any equivalent form pointing at microsoft/terminal — HTTPS with
# or without .git, SSH form, etc. We only care about owner/repo identity.
$expectedOwnerRepo = 'microsoft/terminal'
$existing = git remote get-url upstream 2>$null
function Get-OwnerRepo([string] $url) {
    if (-not $url) { return $null }
    $u = $url.Trim().TrimEnd('/')
    if ($u -match '^(?:https?://[^/]+/|git@[^:]+:)([^/]+/[^/]+?)(?:\.git)?$') { return $Matches[1].ToLowerInvariant() }
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

### 4. Compute pending

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

### 5. Cherry-pick loop

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

#### 5a. On stuck — open the Tier-3 issue and exit

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
$pathLines   = ($pick.conflict_paths | ForEach-Object { "- ``$_``" }) -join "`n"
$body = @"
> [!CAUTION]
> Upstream sync stopped at a conflict that needs human judgment.
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

Surface `$issueUrl` and `$branch` to the operator. The human is expected to:

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

See [references/03-conflict-triage.md](./references/03-conflict-triage.md)
for what "Tier-3" means and the resolution rubric.

### 6. (No commits picked? exit clean.)

If the loop emitted only `empty` results (rare — every pending commit was
a no-op) there is nothing to build or finalize. Delete the branch, report
"sync completed with 0 commits picked (all empty)", exit.

### 7. Build

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

#### 7a. On build failure — surface and exit

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

### 8. Finalize the PR

The agent (you) push the branch and open the PR directly with `git` and
`gh`. There is no wrapper script — there's nothing here that needs more
than `gh pr create` plus a body string you compose from the data you
already have in `$picked` / `$build` / `$pending`.

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
    $prUrl = gh pr create -R microsoft/intelligent-terminal `
        --base main --head $branch --title $title --body-file $bodyFile 2>&1 |
        Select-Object -Last 1
    if ($LASTEXITCODE -eq 0 -and $prUrl -match '^https://github.com/') { break }
    Write-Warning "gh pr create attempt $attempt failed: $prUrl"
    Start-Sleep -Seconds 5
}
Remove-Item -LiteralPath $bodyFile -Force
if ($prUrl -notmatch '^https://github.com/') { throw "gh pr create did not return a URL after 3 attempts." }

# Optional: arm GitHub auto-merge with rebase (NEVER squash).
# Skip this if the operator wants to merge by hand.
gh pr merge -R microsoft/intelligent-terminal $prUrl --rebase --auto --delete-branch
```

Surface `$prUrl` to the operator. Done.

### Direct-to-main (admin-only escape hatch)

If the operator explicitly says "skip the PR, push straight to main": after
step 7 succeeds, skip step 8 and instead:

```pwsh
git switch main
git merge --ff-only $branch
git push origin main
git branch -D $branch
```

This requires bypass-branch-protection rights. No PR, no review checkpoint —
use only for explicit admin runs.

### First-time sync

If the fork has no `(cherry picked from commit <sha>)` trailer on
`origin/main` yet, `02-compute-pending.ps1` will throw. To seed,
the operator commits an **empty seed commit** carrying the trailer
in its message — the same format `cherry-pick -x` would emit, written
by hand exactly once:

```pwsh
git remote add upstream https://github.com/microsoft/terminal.git
git fetch upstream main --no-tags
git switch main
# Pick an upstream SHA you consider "already in this fork" (typically the
# commit the fork was originally branched from). Write the trailer EXACTLY
# as cherry-pick -x would so the next sync picks it up.
$seedSha = '<40-char-upstream-sha-already-in-fork>'
git commit --allow-empty -m "chore(upstream): seed upstream-sync watermark" -m "(cherry picked from commit $seedSha)"
git push origin main
```

That one merged trailer becomes the watermark. From then on, every
run extends it. No script needed — the bootstrap is just one
`commit --allow-empty` ever.

### Squash-merge recovery (don't do this, but if you did)

The PR banner shouts "do not squash" and `-AutoMergeStrategy rebase`
locks in the safe path. If a reviewer squash-merges anyway:

- The squash commit's body usually concatenates every cherry-picked
  message, so it contains MANY `(cherry picked from commit <sha>)`
  trailers. The watermark resolver matches the FIRST one it sees,
  which is the oldest cherry-pick in the squashed batch. That means
  the watermark moves backward, and the next sync run re-picks
  everything between the oldest and newest commits of the squashed batch.
- The recovery is the same as a first-time seed: commit an empty
  watermark commit on `main` carrying the trailer for the upstream
  HEAD that was actually merged, and push.

## After-PR review handling — fix-in-PR vs. follow-up PR

Once the sync PR is open, Copilot and human reviewers will leave
comments. The cherry-pick PR's commits must stay reviewable as
"per-commit, faithful to upstream + minimal must-merge delta" so the
reviewer can spot-check each upstream commit was applied correctly.
That constraint shapes the response policy:

| Comment type | Where to fix |
|---|---|
| **Build-blocking** (compile error, dedup of resw/manifest collisions exposed only at build time, CI gate failure on the sync PR itself) | **One** focused extra commit on the sync branch. The cherry-pick PR is what's broken; the cherry-pick PR is what gets the fix. The build-then-finalize order in this file already lands this commit in the same PR as the picks. |
| **Everything else** — code-quality findings, logic-bug suggestions, translation corrections, spelling-allowlist migrations, typo fixes, doc nits, design feedback | **Follow-up PR** on top of the cherry-pick PR (see [references/follow-up-pr.md](./references/follow-up-pr.md)) |

**Why split.** A reviewer scanning the cherry-pick PR is auditing
"did the sync engine faithfully apply the upstream batch?" Mixing
substantive review feedback into the same PR forces them to mentally
subtract those commits from every upstream-comparison check. Worse,
amending or squashing in review fixes destroys the per-commit
attribution the cherry-pick approach was chosen to preserve.

**Follow-up PR shape** (template in
[references/follow-up-pr.md](./references/follow-up-pr.md)):

- New worktree + branch `dev/<alias>/sync-<sync-pr-number>-review-fixes`
  off the sync PR's HEAD.
- Base = the sync branch (e.g. `upstream-sync/2026-06-04-091512-a3f1` —
  copy the exact name from the sync PR), **not** `main`. The follow-up
  rides along with the sync PR.
- One focused commit per concern (code-bugs / translations /
  spelling-cleanup / etc.) — same "audit trail per finding" rule as the
  Copilot PR review loop skill.
- Reply + resolve every original thread on the sync PR pointing to the
  follow-up PR number.
- If the sync PR merges first, rebase the follow-up onto `main` before
  it merges.

The PR body's banner (see [step 8](#8-finalize-the-pr)) spells this
policy out to the first reviewer so they don't push back on deferred
fixes.

## Gotchas

- **Never squash-merge the sync PR.** Use **"Rebase and merge"**
  (preferred) or **"Create a merge commit"**. The PR body opens with a
  banner reminding the reviewer; the step-8 recipe shows the
  `gh pr merge --rebase --auto` invocation so a tired reviewer can't get
  it wrong.
- **Don't amend substantive review fixes into the sync PR.** Only
  build-blocking fixes get **one** extra commit on the sync branch. See
  [references/follow-up-pr.md](./references/follow-up-pr.md).
- **Never rebase `upstream/main` onto this fork.** Use cherry-pick.
  Verified failure mode on the sister repo `agentic-terminal`.
- **`.github/workflows/spelling2.yml` always conflicts** and the correct
  resolution is always "take upstream wholesale". The Tier-0 list in
  [references/03-known-conflicts.md](./references/03-known-conflicts.md)
  handles this automatically — extend the list when you discover the
  next file with the same pattern.
- **`gh pr create` on Windows can fail with "Head sha can't be blank"** if the
  branch is freshly pushed and not yet visible. The step-8 recipe wraps
  the call in a 3× retry loop — do not "fix" the recipe to use
  `--head <owner>:<branch>` (which would point `gh` at a fork).
- **Do not run the orchestration twice while a stuck issue is open.** The
  step-1 preflight catches it, but a human bypassing that gate manually
  would overwrite the stuck branch and lose their in-progress resolution.
- **Cherry-pick over `git revert` style commits is intentional, not skipped.**
  We only skip revert-pairs where **both** sides are inside the pending
  range. A revert of a commit we already merged last week must land as a
  normal pick — otherwise the fork diverges silently.
- **Never strip the `(cherry picked from commit <sha>)` trailer** when
  hand-resolving a stuck pick. That trailer IS the watermark the next
  sync run reads.
- **Build logs live under `Generated Files/upstream-sync/<date>/`.**
  This directory is gitignored at the repo root (`**/Generated Files/`).
  Do not check these in.
- **Single-host scheduler.** The stuck-lock (open labeled issue) is a
  read-then-check gate, not an atomic lease — two hosts running on the
  same tick can both observe "no open issue" and proceed in parallel.
  Run the scheduler from ONE host. For multi-host fan-out, layer atomic
  locking on top (GitHub Actions `concurrency: upstream-sync` group is
  the easiest).
- **CRLF/LF on manifest files.** Cherry-picks normally preserve upstream
  line endings, but any in-flight resolution touched by an LLM may
  downgrade to LF. If a Tier-2 resolution touches a
  `.yml`/`.xml`/`.csproj`/winget manifest, re-normalize before staging —
  see [references/03-conflict-triage.md](./references/03-conflict-triage.md#line-endings).

## Troubleshooting

| Issue | Solution |
|---|---|
| `02-compute-pending.ps1` throws "No 'cherry picked from commit' trailer ..." | The fork has never used `cherry-pick -x` for an upstream commit yet. Run the one-time seeding pick described in [First-time sync](#first-time-sync). |
| Stuck issue prevents new run | Resolve the conflict on the stuck branch, open a PR, merge it (keep the `(cherry picked from commit <sha>)` trailer!), then **close the stuck issue**. The next scheduler tick proceeds. |
| Cherry-pick reports "empty commit" | Expected for upstream no-op commits and for fork-already-applied patches; `03-cherry-pick-one.ps1` returns `"skipped-empty"` and the agent's loop skips it. No action needed. |
| Same file conflicts every run | Add it to the Tier-0 list in [references/03-known-conflicts.md](./references/03-known-conflicts.md) with the correct resolution strategy (`take-upstream`, `take-ours`, or `union`). |
| `gh pr create` returns "Head sha can't be blank" | The step-8 recipe retries 3× automatically. On slow networks, the operator may need a manual second run of the whole sync. |

## References

- [references/03-conflict-triage.md](./references/03-conflict-triage.md) — Tier 0/1/2/3 resolution rubric with examples.
- [references/03-known-conflicts.md](./references/03-known-conflicts.md) — files that always need a fixed resolution.
- [references/04-build-verification.md](./references/04-build-verification.md) — try-build pipeline expectations.
- [references/follow-up-pr.md](./references/follow-up-pr.md) — fix-in-PR vs. follow-up PR rubric and worktree workflow.
- [scripts/02-compute-pending.ps1](./scripts/02-compute-pending.ps1) — derive watermark + pending list (no state file).
- [scripts/03-cherry-pick-one.ps1](./scripts/03-cherry-pick-one.ps1) — cherry-pick one SHA with author/date pinning + Tier-0/Tier-1.
- [scripts/04-try-build.ps1](./scripts/04-try-build.ps1) — run `bz no_clean`; log to `Generated Files/...`.
