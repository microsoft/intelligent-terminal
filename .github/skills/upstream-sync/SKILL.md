---
name: upstream-sync
description: 'Periodically sync new commits from microsoft/terminal into this manually-forked intelligent-terminal repo by cherry-picking commit-by-commit onto a dated sync branch, auto-skipping revert pairs and empty commits, auto-resolving known take-upstream files, and stopping cleanly on genuine conflicts with a written report and a GitHub issue. Use when the user asks to "sync upstream", "pull from microsoft/terminal", "run upstream sync", "catch up to upstream", or wires this into a scheduler (weekly/daily). Designed to be safe under repeated unattended runs.'
license: MIT
---

# Upstream Sync (microsoft/terminal → intelligent-terminal)

Cherry-pick commit-by-commit from `https://github.com/microsoft/terminal`
into this fork, preserving per-commit attribution, skipping commits that
cancel each other out, and stopping cleanly the moment a human-judgement
conflict appears.

## When to Use This Skill

- User asks to "sync upstream", "pull from microsoft/terminal", "catch up to upstream", or "run upstream sync".
- A scheduler (Task Scheduler, cron, GitHub Actions) invokes
  [`scripts/04-run-batch.ps1`](./scripts/04-run-batch.ps1) on a weekly/daily cadence.
- The previous run left a stuck-lock and the human has finished resolving
  the conflict — use [`scripts/clear-stuck.ps1`](./scripts/clear-stuck.ps1) and re-run.

## When NOT to Use This Skill

- The user wants a **one-shot rebase** of a single feature branch onto upstream — that's a normal `git rebase`, not this skill.
- The fork has never been initialized (`state.json` missing) — first do the one-time bootstrap from [references/bootstrap.md](./references/bootstrap.md).
- A stuck-lock is set — do not re-run; resolve the conflict on the stuck branch first.

## Prerequisites

- `git` 2.30+ and `gh` CLI authenticated against `microsoft/intelligent-terminal`.
- PowerShell 7+ (`pwsh`) on PATH.
- Windows build host with Visual Studio 2022, Windows SDK, `vswhere`, and the repo's `tools\razzle.cmd`/`bz` build environment for the default validation gates (or use `-SkipBuild` only for explicit dev/debug runs).
- Remote named `upstream` pointing at `https://github.com/microsoft/terminal.git`
  (the scripts create it if missing).
- `state.json` initialized once (see [references/bootstrap.md](./references/bootstrap.md)).

## Why Cherry-Pick (Not Rebase, Not Merge)

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

1. **Toolchain preflight** ([`scripts/09-toolchain-preflight.ps1`](./scripts/09-toolchain-preflight.ps1)) — verifies the host has the `PlatformToolset` versions the repo requires. Missing → **Tier-4d infra-stuck** (lock set, NO GitHub issue — PR review can't fix host provisioning).
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

Resumability is built into the state file — re-running after a successful
run is a fast no-op (nothing pending), and re-running while the stuck-lock
is set exits early without touching the branch.

## Gotchas

- **Never squash-merge the sync PR.** Squash collapses every cherry-picked
  upstream commit into one, destroying per-commit attribution, original
  author dates, and `git bisect` resolution. Use **"Rebase and merge"**
  (preferred) or **"Create a merge commit"**. The PR body opens with a
  banner reminding the reviewer; `-AutoMergeStrategy rebase` arms GitHub
  auto-merge with the right strategy so a tired reviewer can't get it
  wrong.
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
- **Do not run the scheduler twice while stuck.** The lock in
  `state.json` makes the second run a no-op, but a human running the
  script manually with `-Force` will overwrite the stuck branch and lose
  their in-progress resolution. The `-Force` flag is documented but
  intentionally not the default.
- **Cherry-pick over `git revert` style commits is intentional, not skipped.**
  We only skip revert-pairs where **both** sides are inside the pending
  range. A revert of a commit we already merged last week must land as a
  normal pick — otherwise the fork diverges silently.
- **Always commit `state.json` and `reports/`** so the next scheduler
  invocation (possibly on a different machine) starts from the right
  checkpoint. The finalize PR includes the state update.
- **Prefer the PR path.** The default workflow always opens a PR so CI and
  human review checkpoint the upstream batch. Use `-PushDirectToMain` only
  for an explicit admin/bypass run where skipping PR latency is intentional.
- **CRLF/LF on manifest files.** Cherry-picks normally preserve upstream
  line endings, but any in-flight resolution touched by an LLM may downgrade
  to LF. If a Tier-2 resolution touches a `.yml`/`.xml`/`.csproj`/winget
  manifest, re-normalize before staging — see
  [references/conflict-triage.md](./references/conflict-triage.md#line-endings).

## Troubleshooting

| Issue | Solution |
|---|---|
| `state.json` is missing | Run the one-time bootstrap — see [references/bootstrap.md](./references/bootstrap.md). Do not guess the baseline SHA. |
| Stuck-lock prevents new run | Resolve the conflict on the stuck branch, open a PR, merge, then run [`scripts/clear-stuck.ps1`](./scripts/clear-stuck.ps1) and re-run the batch. |
| Cherry-pick reports "empty commit" | Expected for upstream no-op commits and for fork-already-applied patches; the loop auto-resets and marks them skipped. No action needed. |
| Same file conflicts every run | Add it to the Tier-0 list in [references/known-conflicts.md](./references/known-conflicts.md) with the correct resolution strategy (`take-upstream`, `take-ours`, or `union`). |
| `gh pr create` returns "Head sha can't be blank" | Retry — the finalize script already does, but on slow networks may need a manual second run. |
| Report says "no-op" but I expected commits | Run `git fetch upstream main` manually and recompute — the scheduler may have run between upstream pushes. |

## References

- [references/workflow.md](./references/workflow.md) — full per-step procedure with exit codes and delegation map.
- [references/state-schema.md](./references/state-schema.md) — `state.json` shape and field semantics.
- [references/bootstrap.md](./references/bootstrap.md) — one-time baseline-SHA discovery and initialization.
- [references/conflict-triage.md](./references/conflict-triage.md) — Tier 0/1/2/3/4 resolution rubric with examples.
- [references/known-conflicts.md](./references/known-conflicts.md) — files that always need a fixed resolution.
- [references/static-scan.md](./references/static-scan.md) — post-pick static breakage scan rules.
- [references/fork-invariants.json](./references/fork-invariants.json) — fork-specific patterns that must survive any upstream pick.
- [references/build-verification.md](./references/build-verification.md) — try-build pipeline + toolchain preflight policy.
- [references/reporting.md](./references/reporting.md) — report template and stuck-issue template.
- [scripts/04-run-batch.ps1](./scripts/04-run-batch.ps1) — the scheduler entrypoint.
- [scripts/clear-stuck.ps1](./scripts/clear-stuck.ps1) — clear the stuck-lock after human resolution.
