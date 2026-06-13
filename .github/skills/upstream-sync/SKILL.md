---
name: upstream-sync
description: 'Periodically sync new commits from microsoft/terminal into this manually-forked intelligent-terminal repo by cherry-picking commit-by-commit onto a dated sync branch, auto-skipping revert pairs and empty commits, auto-resolving known take-upstream files, and stopping cleanly on genuine conflicts. The agent (you, reading this file) is the orchestrator — the PowerShell scripts are atomic operations you invoke one at a time. Use when the user asks to "sync upstream", "pull from microsoft/terminal", "run upstream sync", "catch up to upstream", or wires this into a scheduler (weekly/daily). Designed to be safe under repeated unattended runs.'
license: MIT
---

# Upstream Sync (microsoft/terminal → intelligent-terminal)

Cherry-pick commit-by-commit from `https://github.com/microsoft/terminal`
into this fork, preserving per-commit attribution, auto-skipping picks
that cancel each other out, and stopping cleanly the moment a
human-judgment conflict appears.

**You — the agent reading this file — are the orchestrator.** Each step
in the run is a single atomic call into one of the `scripts/*.ps1` files.
There is intentionally no PowerShell driver, because every interesting
decision (build-fix vs. open stuck issue, retry vs. bail, finalize vs.
dry-run) wants LLM judgment the operator can audit in your transcript.

## When to Use This Skill

- User asks to "sync upstream", "pull from microsoft/terminal", "catch up to upstream", or "run upstream sync".
- A scheduler invokes the agent on a weekly/daily cadence.
- The previous run left an `upstream-sync-stuck` labeled issue open and the human has finished resolving the conflict — **close the issue** (that IS the lock-clear signal) and re-run.

## When NOT to Use This Skill

- User wants a **one-shot rebase** of a single feature branch onto upstream — that's a normal `git rebase`, not this skill.
- An `upstream-sync-stuck` labeled issue is open on `microsoft/intelligent-terminal` — do not re-run; resolve the conflict on the stuck branch first, then close the issue.

## Prerequisites

- `git` 2.38+ (for `cherry-pick --keep-redundant-commits`), `gh` CLI authenticated against `microsoft/intelligent-terminal` (needs push to `upstream-sync/*` topic branches + issue/label create), PowerShell 7+ (`pwsh`) on PATH.
- **`origin` MUST point at `microsoft/intelligent-terminal`.** All scripts push to `origin` and pass `-R microsoft/intelligent-terminal` explicitly. Verify with `git remote -v` before running.
- **Full git history on `origin/main`** (no shallow clone). Watermark discovery walks up to 5000 commits scanning for `cherry picked from commit <sha>` trailers. In CI, use `actions/checkout@v4` with `fetch-depth: 0`.
- Windows build host with Visual Studio 2022, Windows SDK, `vswhere`, and `tools\razzle.cmd` / `bz`. Build is a hard gate before finalize.
- **No `state.json` to bootstrap.** Watermark = newest `(cherry picked from commit <sha>)` trailer on `origin/main`. If the fork has never used `cherry-pick -x`, run the one-time [First-time sync](./references/recovery-procedures.md#first-time-sync-seeding-the-watermark).

## State Model (no state file)

Every persistent fact lives in the source that owns it:

| Question | Source of truth |
|---|---|
| Last-synced upstream commit? | Newest `(cherry picked from commit <sha>)` trailer on `origin/main` whose target is reachable from `upstream/main`. Derived by [`02-compute-pending.ps1`](./scripts/02-compute-pending.ps1). |
| What's pending? | `git log --cherry-pick --right-only --no-merges origin/main...upstream/main`, drop SHAs at/before the watermark. Patch-id based, so a picked-then-reverted commit correctly re-appears. |
| Is the scheduler locked? | Any open issue with the `upstream-sync-stuck` label on `microsoft/intelligent-terminal`. Closing it IS the lock-clear signal. |
| Where do build logs go? | `Generated Files/upstream-sync/<YYYY-MM-DD>/` — gitignored by the repo root's `**/Generated Files/`. Never committed. |

### Why cherry-pick (not rebase or merge)

- **Rebase upstream/main** — ❌ fork history contains old "Merge upstream" commits; rebase replays them and explodes conflicts (verified failure on sister repo).
- **Merge upstream/main** — ⚠️ works, but collapses the whole sync into one blob commit, killing per-commit review and `git bisect`.
- **Cherry-pick commit-by-commit** — ✅ preserves authorship + per-commit content, allows mechanical revert-pair skipping, produces a reviewable PR.

## Run a sync

Eight-step orchestration. Full commands, JSON contracts, and
failure-handling for every step are in
[`references/run-a-sync.md`](./references/run-a-sync.md). The flow:

```
1. Preconditions (clean tree, on main FF, no open stuck issue)
2. Create branch  upstream-sync/<UTC-yyyy-MM-dd-HHmmss-rand4>
3. Fetch upstream (verify owner/repo identity of the `upstream` remote)
4. Compute pending     ← scripts/02-compute-pending.ps1  (→ JSON)
5. Cherry-pick loop    ← scripts/03-cherry-pick-one.ps1 per SHA  (→ JSON)
       picked / skipped-empty → continue
       stuck → 5a (push branch, file upstream-sync-stuck issue, EXIT)
6. (No commits picked? exit clean.)
7. Build               ← scripts/04-try-build.ps1  (→ JSON)
       build-ok → step 8
       build-failed → ONE focused fix commit, re-build; else 7a
       build-inconclusive → 7a
   7a. Surface failure to operator, EXIT (no issue filed for build failures)
8. Finalize: push, `gh pr create`, optional `gh pr merge --rebase --auto`
```

**Key invariants** (the agent MUST hold these — full rationale in the runbook):

- **Atomic per-commit.** Each pick is one commit on the sync branch carrying the upstream author/date and the `(cherry picked from commit <sha>)` trailer. Never amend across picks.
- **Stuck → exit clean.** On a Tier-3 conflict (script returns `status: "stuck"`), do NOT continue past it — push the branch, file the labeled issue, exit. The human resolves on the stuck branch, merges (no squash), closes the issue.
- **One build-fix commit, max.** Build-blocking fixes land as exactly one extra commit on the sync branch. Anything bigger → exit; the operator pushes a manual fix or runs a follow-up PR.
- **Never squash-merge the sync PR.** Squashing destroys per-commit attribution AND the trailer the next sync uses as its watermark. The PR body banner warns reviewers; the recipe arms `--rebase --auto`.

## Recovery procedures (rare)

Direct-to-main escape hatch, first-time watermark seeding, squash-merge
recovery: [`references/recovery-procedures.md`](./references/recovery-procedures.md).
Each requires explicit operator action.

## After-PR review handling — fix-in-PR vs. follow-up PR

Once the sync PR is open, reviewers (Copilot + humans) will comment.
**Only build-blocking fixes** belong on the sync branch as one focused
extra commit. Everything else — code-quality, logic-bug suggestions,
translation corrections, spelling-allowlist migrations, doc nits, design
feedback — goes into a **follow-up PR** that targets the sync branch
(not `main`). The cherry-pick PR must stay reviewable as "per-commit,
faithful to upstream + minimum must-merge delta".

Full rubric, worktree mechanics, PR-body template:
[`references/follow-up-pr.md`](./references/follow-up-pr.md).

## Gotchas

- **Never squash-merge the sync PR.** Squashing destroys per-commit
  attribution AND collapses the trailers the next run uses as its
  watermark. The PR body opens with a banner; step 8 arms
  `gh pr merge --rebase --auto`.
- **Never strip the `(cherry picked from commit <sha>)` trailer** when
  hand-resolving a stuck pick. That trailer IS the watermark.
- **Never rebase `upstream/main` onto this fork.** Use cherry-pick —
  rebase replays old "Merge upstream" commits and explodes.
- **Don't amend substantive review fixes into the sync PR.** Only
  build-blocking fixes (max one extra commit). Everything else → follow-up PR.
- **`.github/workflows/spelling2.yml` always conflicts** and is always
  "take upstream wholesale". The Tier-0 list in
  [`references/03-known-conflicts.md`](./references/03-known-conflicts.md)
  handles it — extend the list when you find the next file with this pattern.
- **`gh pr create` on Windows can fail with "Head sha can't be blank"**
  on freshly-pushed branches. The step-8 recipe wraps it in a 3× retry.
  Do not "fix" it to use `--head <owner>:<branch>` (that points `gh` at a fork).
- **Cherry-pick over `git revert`-style commits is intentional, not
  skipped.** We only skip revert-pairs where **both** sides are inside
  the pending range. A revert of an already-merged commit must land —
  otherwise the fork diverges silently.
- **Single-host scheduler.** The stuck-lock is a read-then-check gate,
  not an atomic lease. Run from ONE host. For multi-host fan-out, layer
  atomic locking on top (GitHub Actions `concurrency: upstream-sync` is
  easiest).
- **CRLF/LF on manifest files.** Cherry-picks preserve upstream endings,
  but Tier-2 LLM-touched resolutions on `.yml`/`.xml`/`.csproj`/winget
  manifests may downgrade to LF. Re-normalize before staging — see
  [`references/03-conflict-triage.md`](./references/03-conflict-triage.md#line-endings).

## Troubleshooting

| Issue | Solution |
|---|---|
| `02-compute-pending.ps1` throws "No 'cherry picked from commit' trailer …" | Fork has never used `cherry-pick -x` yet. Run the one-time [First-time sync](./references/recovery-procedures.md#first-time-sync-seeding-the-watermark). |
| Stuck issue prevents new run | Resolve on the stuck branch, open + merge a PR (keep the trailer, don't squash), then **close the stuck issue**. The next scheduler tick proceeds. |
| `03-cherry-pick-one.ps1` returns `"skipped-empty"` | Expected for upstream no-op commits and fork-already-applied patches. The loop skips and continues. |
| Same file conflicts every run | Add it to [`references/03-known-conflicts.md`](./references/03-known-conflicts.md) with the right strategy (`take-upstream` or `take-ours`; `union` is reserved and currently escalates instead of auto-resolving). |
| `gh pr create` returns "Head sha can't be blank" | Step 8 retries 3×. On slow networks, the operator may need a manual second run. |

## References

- [`references/run-a-sync.md`](./references/run-a-sync.md) — full eight-step procedure with commands.
- [`references/03-conflict-triage.md`](./references/03-conflict-triage.md) — Tier 0/1/2/3 conflict-resolution rubric.
- [`references/03-known-conflicts.md`](./references/03-known-conflicts.md) — files with fixed Tier-0 resolutions.
- [`references/follow-up-pr.md`](./references/follow-up-pr.md) — fix-in-PR vs. follow-up PR rubric and worktree workflow.
- [`references/recovery-procedures.md`](./references/recovery-procedures.md) — direct-to-main, first-time seed, squash-merge recovery.
- [`scripts/02-compute-pending.ps1`](./scripts/02-compute-pending.ps1) — derive watermark + pending list (no state file).
- [`scripts/03-cherry-pick-one.ps1`](./scripts/03-cherry-pick-one.ps1) — cherry-pick one SHA with author/date pinning + Tier-0/Tier-1.
- [`scripts/04-try-build.ps1`](./scripts/04-try-build.ps1) — run `bz no_clean`; log to `Generated Files/...`.
