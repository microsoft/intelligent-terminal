---
name: upstream-sync
description: 'Periodically sync new commits from microsoft/terminal into this manually-forked intelligent-terminal repo by cherry-picking commit-by-commit onto a dated sync branch, auto-skipping revert pairs and empty commits, auto-resolving known take-upstream files, and stopping cleanly on genuine conflicts with a written report and a GitHub issue. Use when the user asks to "sync upstream", "pull from microsoft/terminal", "run upstream sync", "catch up to upstream", or wires this into a scheduler (weekly/daily). Designed to be safe under repeated unattended runs.'
license: MIT
---

# Upstream Sync (microsoft/terminal → intelligent-terminal)

Cherry-pick commit-by-commit from `https://github.com/microsoft/terminal`
into this fork, preserving per-commit attribution, skipping commits that
cancel each other out, and stopping cleanly the moment a human-judgment
conflict appears.

## When to Use This Skill

- User asks to "sync upstream", "pull from microsoft/terminal", "catch up to upstream", or "run upstream sync".
- A scheduler (Task Scheduler, cron, GitHub Actions) invokes
  [`scripts/04-run-batch.ps1`](./scripts/04-run-batch.ps1) on a weekly/daily cadence.
- The previous run left an `upstream-sync-stuck` labeled issue open and the
  human has finished resolving the conflict — **close the issue** (that IS
  the lock-clear signal) and re-run.

## When NOT to Use This Skill

- The user wants a **one-shot rebase** of a single feature branch onto upstream — that's a normal `git rebase`, not this skill.
- An `upstream-sync-stuck` labeled issue is open on `microsoft/intelligent-terminal` — do not re-run; resolve the conflict on the stuck branch first, then close the issue.

## Prerequisites

- `git` 2.38+ (needed for `git cherry-pick --keep-redundant-commits`, used by `scripts/03-cherry-pick-one.ps1`) and `gh` CLI authenticated against `microsoft/intelligent-terminal`. The credential needs **push to topic branches** (`upstream-sync/<date>`) and **issue + label create** in the same repo (the stuck-lock is an open labeled issue, not a commit on `main`).
- PowerShell 7+ (`pwsh`) on PATH.
- Windows build host with Visual Studio 2022, Windows SDK, `vswhere`, and the repo's `tools\razzle.cmd`/`bz` build environment for the default validation gates (or use `-SkipBuild` only for explicit dev/debug runs).
- Remote named `upstream` pointing at `https://github.com/microsoft/terminal.git`
  (the scripts create it if missing).
- **No `state.json` to bootstrap.** Watermark comes from the
  `(cherry picked from commit <sha>)` trailers on `origin/main`. If
  the fork has never used `cherry-pick -x` (or trailers were stripped),
  see "First-time sync" below for the one-time operator step.

## State model (no `state.json`)

Every persistent fact lives in the source that owns it:

| Question | Source of truth |
|---|---|
| What's the last-synced upstream commit? | Newest `(cherry picked from commit <sha>)` trailer on `origin/main` whose target is reachable from `upstream/main`. Computed by `Get-LastSyncedUpstreamSha` in `scripts/Common.ps1`. |
| What's pending? | `git log --cherry-pick --right-only --no-merges <watermark>...upstream/main`. Patch-id-based, so a picked-then-reverted commit correctly re-appears. |
| Is the scheduler locked? | Any OPEN issue with the `upstream-sync-stuck` label on `microsoft/intelligent-terminal`. Closing the issue IS the lock-clear signal. |
| What does the lock mean? | A fenced ```yaml # wta-state``` block in the issue body carries `tier`, `kind`, `stuck_on_sha`/`findings_hash`, etc. (parsed by `Get-StuckMetaFromIssue`). |
| Where do reports + build logs go? | `Generated Files/upstream-sync/<YYYY-MM-DD>/` — gitignored by the repo root's `**/Generated Files/` rule. Never committed. |

### First-time sync

If the fork has no `(cherry picked from commit <sha>)` trailer on
`origin/main` yet, `Get-LastSyncedUpstreamSha` will throw. To seed,
the operator commits an **empty seed commit** carrying the trailer
in its message — the same format `cherry-pick -x` would emit, written
by hand exactly once:

```pwsh
git remote add upstream https://github.com/microsoft/terminal.git
git fetch upstream main --no-tags
git switch main
# Pick an upstream SHA you consider "already in this fork" (typically the
# commit the fork was originally branched from). Write the trailer EXACTLY
# as cherry-pick -x would so Get-LastSyncedUpstreamSha picks it up.
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
  trailers. `Get-LastSyncedUpstreamSha` matches the FIRST one it sees
  via regex, which is the oldest cherry-pick in the squashed batch.
  That means the watermark moves backward, and the next sync run
  re-picks everything between the oldest and newest commits of the
  squashed batch.
- The recovery is the same as a first-time seed: commit an empty
  watermark commit on `main` carrying the trailer for the upstream
  HEAD that was actually merged, and push.



| Approach | Why rejected / chosen |
|---|---|
| **Rebase** `upstream/main` | ❌ Fork history contains old "Merge upstream" commits; rebase replays them and explodes conflicts. Verified failure on sister repo `agentic-terminal`. |
| **Merge** `upstream/main` | ⚠️ Works, but collapses the whole sync into one blob commit — kills per-commit review, kills `git bisect`. |
| **Cherry-pick commit-by-commit** | ✅ Preserves authorship + per-commit content, allows mechanical revert-pair skipping, produces a reviewable PR with N small commits. |

## Step-by-Step Workflow

The scheduler entrypoint is a single PowerShell script. Full procedure
with commands, exit codes, and the per-step delegation map lives in
[references/workflow.md](./references/workflow.md).

```
fetch upstream → compute pending → drop revert pairs → drop empties
  → create sync branch → cherry-pick loop (auto-resolve T0/T1, abort on T3)
  → toolchain preflight → static breakage scan → try-build (Tier-4 gates)
  → write report (always) → push + open PR  OR  open stuck issue + lock
```

**Safety guarantees (post-pick validation pipeline).** Even when every
cherry-pick applies cleanly, content-level breakage can slip in (duplicate
`.resw` keys from a fork-local commit + an upstream rename touching the
same names; a take-upstream resolution silently dropping a fork-specific
warning suppression; etc. — see PR #220 audit). Before any push or PR,
the orchestrator now runs three hard gates:

1. **Toolchain preflight** ([`scripts/09-toolchain-preflight.ps1`](./scripts/09-toolchain-preflight.ps1)) — verifies the host has the `PlatformToolset` versions the repo requires. Missing → **Tier-4d infra-stuck**: NO GitHub issue is opened and NO lock is set (PR review can't fix host provisioning). The next scheduler tick simply retries; if this host keeps tripping it, run from a properly provisioned host instead.
2. **Static breakage scan** ([`scripts/08-static-scan.ps1`](./scripts/08-static-scan.ps1)) — baseline-diffs `.resw` files for newly-duplicated `<data name>` keys, and regex-checks fork invariants from [`references/fork-invariants.json`](./references/fork-invariants.json). Blocking → **Tier-4a stuck**.
3. **Try-build** ([`scripts/10-try-build.ps1`](./scripts/10-try-build.ps1)) — runs `tools\razzle.cmd && bz no_clean` with a 45-minute wall-clock cap. Build failed → **Tier-4b stuck**; timeout → **Tier-4c stuck** (unless `-AllowInconclusiveBuild`).

See [references/static-scan.md](./references/static-scan.md), [references/build-verification.md](./references/build-verification.md), and [references/conflict-triage.md](./references/conflict-triage.md) Tier-4 for details.

**Run it:**

```pwsh
# Default: open a PR, let a human pick the merge strategy (must be rebase or merge — NOT squash)
pwsh .github/skills/upstream-sync/scripts/04-run-batch.ps1

# Open a PR AND arm GitHub auto-merge with rebase strategy (hands-off once CI/approvals pass)
pwsh .github/skills/upstream-sync/scripts/04-run-batch.ps1 -AutoMergeStrategy rebase

# Skip the PR entirely — fast-forward main to the sync tip. Requires admin/bypass on main.
pwsh .github/skills/upstream-sync/scripts/04-run-batch.ps1 -PushDirectToMain

# Compute & report without picking
pwsh .github/skills/upstream-sync/scripts/04-run-batch.ps1 -DryRun

# Skip the static scan (debugging only — schedulers must run it)
pwsh .github/skills/upstream-sync/scripts/04-run-batch.ps1 -SkipStaticScan

# Skip the try-build (debugging only — schedulers must run it).
# Also skips toolchain preflight since they share the same infra prerequisite.
pwsh .github/skills/upstream-sync/scripts/04-run-batch.ps1 -SkipBuild

# Don't treat a build timeout as Tier-4 stuck (dev opt-in; never in a scheduler)
pwsh .github/skills/upstream-sync/scripts/04-run-batch.ps1 -AllowInconclusiveBuild

# Override build timeout (default 45 minutes)
pwsh .github/skills/upstream-sync/scripts/04-run-batch.ps1 -BuildTimeoutMinutes 60
```

### Finalize modes — what each preserves

| Mode | Per-commit content | Order on main | Original author + committer dates | Reviewer checkpoint | Requires admin? |
|---|---|---|---|---|---|
| PR + **rebase-merge** | ✅ | ✅ | ✅ | ✅ | No |
| PR + **merge commit** | ✅ | ✅ | ✅ | ✅ | No |
| PR + **squash** | ❌ collapsed | ❌ | ⚠️ folded | ✅ | No |
| **`-PushDirectToMain`** | ✅ | ✅ | ✅ | ❌ | Yes (push to main) |

The cherry-pick loop pins both author and committer identity/dates to the
upstream commit so audit timestamps match the original commit-by-commit history.

Resumability is built into the trailer model — re-running after a successful
run is a fast no-op (the new merged commits extend the watermark, so the
pending-list is empty), and re-running while a stuck-issue is open exits
early without touching the branch.

### After-PR review handling — fix-in-PR vs. follow-up PR

Once the sync PR is open, Copilot and human reviewers will leave
comments. The cherry-pick PR's commits must stay reviewable as
"per-commit, faithful to upstream + minimal must-merge delta" so the
reviewer can spot-check each upstream commit was applied correctly.
That constraint shapes the response policy:

| Comment type | Where to fix |
|---|---|
| **Build-blocking** (compile error, dedup of resw/manifest collisions exposed only at build time, CI gate failure on the sync PR itself) | **One** focused extra commit on the sync branch. The cherry-pick PR is what's broken; the cherry-pick PR is what gets the fix. |
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
  off the sync PR's HEAD (see
  [branch-worktree-workflow](./references/follow-up-pr.md#worktree-setup)).
- Base = the sync branch (e.g. `upstream-sync/2026-06-04`), **not**
  `main`. The follow-up rides along with the sync PR.
- One focused commit per concern (code-bugs / translations /
  spelling-cleanup / etc.) — same "audit trail per finding" rule as the
  Copilot PR review loop skill.
- Reply + resolve every original thread on the sync PR pointing to the
  follow-up PR number.
- If the sync PR merges first, rebase the follow-up onto `main` before
  it merges.

The orchestrator's PR banner ([scripts/06-finalize-pr.ps1](./scripts/06-finalize-pr.ps1))
spells this policy out to the first reviewer so they don't push back on
deferred fixes.

## Gotchas

- **Never squash-merge the sync PR.** Squash collapses every cherry-picked
  upstream commit into one, destroying per-commit attribution, original
  author dates, and `git bisect` resolution. Use **"Rebase and merge"**
  (preferred) or **"Create a merge commit"**. The PR body opens with a
  banner reminding the reviewer; `-AutoMergeStrategy rebase` arms GitHub
  auto-merge with the right strategy so a tired reviewer can't get it
  wrong.
- **Don't amend review fixes into the sync PR.** Only build-blocking
  fixes (compile errors, dedup of conflicts that surface at build time,
  CI gate failures on the sync PR itself) get **one** extra commit on
  the sync branch. Substantive Copilot/human review feedback —
  code-quality, logic, translations, spelling-list migrations, doc nits
  — goes into a separate follow-up PR off the sync branch's HEAD. See
  [references/follow-up-pr.md](./references/follow-up-pr.md). Mixing
  them poisons the "faithful to upstream" audit of the cherry-pick PR.
- **Never rebase `upstream/main` onto this fork.** Old "Merge upstream"
  commits in the fork history replay and cascade conflicts. Use cherry-pick.
  Verified failure mode on the sister repo `agentic-terminal`.
- **`.github/workflows/spelling2.yml` always conflicts** and the correct
  resolution is always "take upstream wholesale". The Tier-0 list in
  [references/known-conflicts.md](./references/known-conflicts.md) handles
  this automatically — extend the list when you discover the next file
  with the same pattern.
- **`gh pr create` on Windows can fail with "Head sha can't be blank"** if the
  branch is freshly pushed and not yet visible. The same-repo finalize script
  intentionally uses `--head <branch>` plus a 5s retry — do not "fix" it to
  `--head <owner>:<branch>`, which would point `gh` at a fork.
- **Do not run the scheduler twice while a stuck issue is open.** The
  open labeled issue makes the second run a no-op, but a human running
  the script manually with `-Force` will overwrite the stuck branch and
  lose their in-progress resolution. The `-Force` flag is documented but
  intentionally not the default.
- **Cherry-pick over `git revert` style commits is intentional, not skipped.**
  We only skip revert-pairs where **both** sides are inside the pending
  range. A revert of a commit we already merged last week must land as a
  normal pick — otherwise the fork diverges silently.
- **Never strip the `(cherry picked from commit <sha>)` trailer** when
  hand-resolving a stuck pick. That trailer IS the watermark the next
  sync run reads. A squash-merge or `--no-commit` workflow that drops
  the trailer breaks the next sync as effectively as deleting a
  `state.json` used to.
- **Reports and build logs live under `Generated Files/upstream-sync/<date>/`.**
  This directory is gitignored at the repo root (`**/Generated Files/`).
  Do not check these in — they're transient diagnostics, and the issue
  body inlines the parts that matter for review.
- **Prefer the PR path.** The default workflow always opens a PR so CI and
  human review checkpoint the upstream batch. Use `-PushDirectToMain` only
  for an explicit admin/bypass run where skipping PR latency is intentional.
- **CRLF/LF on manifest files.** Cherry-picks normally preserve upstream
  line endings, but any in-flight resolution touched by an LLM may downgrade
  to LF. If a Tier-2 resolution touches a `.yml`/`.xml`/`.csproj`/winget
  manifest, re-normalize before staging — see
  [references/conflict-triage.md](./references/conflict-triage.md#line-endings).

- **Single-host scheduler.** The stuck-lock (open labeled issue) is a
  read-then-check gate, not an atomic lease — two hosts running on the
  same tick can both observe "no open issue" and proceed in parallel.
  Run the scheduler from ONE host. For multi-host fan-out, layer atomic
  locking on top (GitHub Actions `concurrency: upstream-sync` group is
  the easiest).

## Troubleshooting

| Issue | Solution |
|---|---|
| `Get-LastSyncedUpstreamSha` throws "No 'cherry picked from commit' trailer ..." | The fork has never used `cherry-pick -x` for an upstream commit yet. Run the one-time seeding pick described in [State model — First-time sync](#first-time-sync). |
| Stuck issue prevents new run | Resolve the conflict on the stuck branch, open a PR, merge it (keep the `(cherry picked from commit <sha>)` trailer!), then **close the stuck issue**. The next scheduler tick proceeds. |
| Cherry-pick reports "empty commit" | Expected for upstream no-op commits and for fork-already-applied patches; the loop auto-resets and marks them skipped. No action needed. |
| Same file conflicts every run | Add it to the Tier-0 list in [references/known-conflicts.md](./references/known-conflicts.md) with the correct resolution strategy (`take-upstream`, `take-ours`, or `union`). |
| `gh pr create` returns "Head sha can't be blank" | Retry — the finalize script already does, but on slow networks may need a manual second run. |
| Report says "no-op" but I expected commits | Run `git fetch upstream main` manually and recompute — the scheduler may have run between upstream pushes. |

## References

- [references/workflow.md](./references/workflow.md) — full per-step procedure with exit codes and delegation map.
- [references/conflict-triage.md](./references/conflict-triage.md) — Tier 0/1/2/3/4 resolution rubric with examples.
- [references/known-conflicts.md](./references/known-conflicts.md) — files that always need a fixed resolution.
- [references/static-scan.md](./references/static-scan.md) — post-pick static breakage scan rules.
- [references/fork-invariants.json](./references/fork-invariants.json) — fork-specific patterns that must survive any upstream pick.
- [references/build-verification.md](./references/build-verification.md) — try-build pipeline + toolchain preflight policy.
- [references/follow-up-pr.md](./references/follow-up-pr.md) — fix-in-PR vs. follow-up PR rubric and worktree workflow for handling post-PR review.
- [scripts/04-run-batch.ps1](./scripts/04-run-batch.ps1) — the scheduler entrypoint.
- [scripts/Common.ps1](./scripts/Common.ps1) — derived-state helpers (`Get-LastSyncedUpstreamSha`, `Get-PendingUpstreamShas`, `Get-StuckIssues`, `Get-GeneratedDir`).
