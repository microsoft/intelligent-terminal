# Upstream Sync — Full Workflow

This is the authoritative per-step procedure. The orchestrator is
[`scripts/04-run-batch.ps1`](../scripts/04-run-batch.ps1); each step below
maps to a script or an in-orchestrator function.

## State model — derived, not stored

There is **no `state.json`**. Every persistent fact is derived from an
authoritative source on demand:

| Fact | Source | Helper in [`scripts/Common.ps1`](../scripts/Common.ps1) |
|---|---|---|
| Last-synced upstream SHA | Newest `(cherry picked from commit <sha>)` trailer on `origin/main` whose target is reachable from `upstream/main` | `Get-LastSyncedUpstreamSha` |
| Pending list | `git log --cherry-pick --right-only --no-merges <watermark>...upstream/main` (patch-id-aware; reverted picks reappear) | `Get-PendingUpstreamShas -Since <watermark>` |
| Stuck-lock | Any OPEN issue with the `upstream-sync-stuck` label on `microsoft/intelligent-terminal` | `Get-StuckIssues` |
| Stuck-lock metadata | Fenced ```yaml # wta-state``` block inside the issue body | `Get-StuckMetaFromIssue` |
| Transient artifacts (reports, build logs) | `Generated Files/upstream-sync/<YYYY-MM-DD>/` (gitignored at repo root) | `Get-GeneratedDir [-Sub]` |

## Entry Conditions

- Working tree is clean (`git status --porcelain` empty).
- We are on `main` (or the script will `git switch main` and `git pull --ff-only origin main`).
- `Get-StuckIssues` returns empty (otherwise exit early — see "Stuck-lock" below).

## Steps

### 1. Fetch upstream

```pwsh
git remote get-url upstream 2>$null || git remote add upstream https://github.com/microsoft/terminal.git
git fetch upstream main --no-tags
```

Script: [`01-fetch-upstream.ps1`](../scripts/01-fetch-upstream.ps1).

If `git rev-parse upstream/main` equals `Get-LastSyncedUpstreamSha`, the
orchestrator writes a local no-op report and exits 0.

### 2. Compute pending range

```pwsh
$since = Get-LastSyncedUpstreamSha
git log --cherry-pick --right-only --no-merges --format='%H' --reverse "$since...upstream/main"
```

Oldest-first ordering is mandatory. Cherry-picking newest-first inverts
dependencies and creates spurious conflicts. `--cherry-pick` compares
patch IDs, so a commit that was picked then reverted on `origin/main`
correctly re-appears here as pending.

Script: [`02-compute-pending.ps1`](../scripts/02-compute-pending.ps1) emits
a JSON object on stdout — see step 3 below for the full shape.

### 3. Detect & drop revert pairs

A commit is a revert if its **first line** matches `^Revert "..."$` **or**
its body contains `This reverts commit <40-hex>`.

- If `<40-hex>` is **inside** the pending range AND has not been picked
  yet → drop **both** the original and the revert; record the pair.
- If `<40-hex>` is **outside** the pending range (already synced earlier)
  → keep the revert; it must land as a normal pick.

Script: same `02-compute-pending.ps1`. Full return shape:
`{ from: "<old_sha>", to: "<new_sha>", pending: [...], dropped_pairs: [[A,B],...], skipped_empty: [...] }`.

### 4. Drop upstream-empty commits

Before picking, check `git diff-tree --no-commit-id --name-only -r <sha>`.
If empty, mark skipped and record. (Cheaper to detect upfront than to
pick and reset.)

### 5. Create the sync branch

```pwsh
# Branch name is per-run, never reused: date + UTC HHmmss + 4 random hex chars.
# This guarantees a fresh branch for every run (no risk of replaying onto a
# stale branch that GitHub didn't auto-delete after a rebase-merge).
$branch = "upstream-sync/$((Get-Date).ToString('yyyy-MM-dd'))-$((Get-Date).ToUniversalTime().ToString('HHmmss'))-$(([guid]::NewGuid().ToString('N').Substring(0,4)))"
git switch -c $branch
```

Resume = pick up the branch name from the run report or the open
`upstream-sync-stuck` issue body, not by deriving from the date.

### 6. Cherry-pick loop

For each commit in the (now-filtered) pending list:

```pwsh
git cherry-pick --keep-redundant-commits -x <sha>
```

- `-x` adds `(cherry picked from commit <sha>)` to the message — critical
  for audit trail, for the next-run revert-pair detector, **and for
  `Get-LastSyncedUpstreamSha` to derive the next watermark.** Never strip it.
- `--keep-redundant-commits` lets us preserve no-op picks for traceability
  (we then `git reset --hard HEAD~1` if Tier-1 fires).

**On conflict, apply resolution tiers in order:**

1. **Tier 0 — known take-upstream / take-ours files.** Read
   [`known-conflicts.md`](./known-conflicts.md). For every conflicting
   path in the Tier-0 list, run `git checkout --theirs <path>` (or
   `--ours`), then `git add <path>`. If **all** conflicting paths are
   resolved, run `git cherry-pick --continue` and move on.
2. **Tier 1 — empty after pick.** If `git diff --cached --quiet` returns
   zero exit code (no staged changes), the commit was already applied or
   is a no-op against fork: `git cherry-pick --skip` and record.
3. **Tier 2 — trivial textual (opt-in via `-TryTier2`).** Delegate to a
   fresh sub-agent with the conflict text. Accept only `high` confidence.
   See [conflict-triage.md](./conflict-triage.md#tier-2-llm-assisted).
4. **Tier 3 — semantic conflict.** Run `git cherry-pick --abort`. Open
   the labeled stuck issue, write report, exit 10. The next scheduler
   tick sees the open labeled issue and skips.

Script: [`03-cherry-pick-one.ps1`](../scripts/03-cherry-pick-one.ps1)
handles one commit, returns a JSON status object. The orchestrator loops.

### 7. Post-pick validation gates (Tier-4)

After all cherry-picks complete cleanly, the orchestrator runs three
hard gates **before** writing the report or pushing anything. The order
matters: cheapest infra check first, then content, then full build.

#### 7a. Toolchain preflight

```pwsh
pwsh .github/skills/upstream-sync/scripts/09-toolchain-preflight.ps1
# Emits JSON { required_toolsets, available_toolsets, missing, vs_installs, ok }
```

Detects required `<PlatformToolset>` values from `src/common.build.*.props`
and checks they exist under `<VS>\MSBuild\Microsoft\VC\<msbuild-ver>\Platforms\x64\PlatformToolsets\<toolset>`.
If `ok=false`, this is **Tier-4d infra-stuck**: NO GitHub issue, NO lock
(PR review cannot fix host provisioning; the next scheduler tick simply
retries from a properly provisioned host). Skipped when `-SkipBuild` is set.

#### 7b. Static breakage scan

```pwsh
pwsh .github/skills/upstream-sync/scripts/08-static-scan.ps1 -BaseSha $preBase
# Emits JSON { base, head, findings: [...], summary: { critical, high, ... }, blocking }
```

`$preBase` is `git rev-parse origin/main` captured BEFORE the cherry-pick
loop. The scan:

- Baseline-diffs every changed `.resw` file for NEW duplicate `<data name>`
  keys (preexisting dups are reported as `info`, not blocking).
- Runs regex assertions from [`fork-invariants.json`](./fork-invariants.json)
  against the post-pick worktree.

If `blocking=true` (any `critical` or `high` finding), this is **Tier-4a
stuck**: opens labeled issue + exit 10. Skipped when `-SkipStaticScan`.

#### 7c. Try-build

```pwsh
pwsh .github/skills/upstream-sync/scripts/10-try-build.ps1 -BuildCommand 'tools\razzle.cmd && bz no_clean' -TimeoutMinutes 45
# Emits JSON { kind, exit_code, duration_ms, log_path, log_tail }
```

- `kind = build-ok` → continue to step 8.
- `kind = build-failed` → **Tier-4b stuck**.
- `kind = build-inconclusive` (timeout) → **Tier-4c stuck**, unless
  `-AllowInconclusiveBuild` (dev opt-in; never in a scheduler).

Skipped when `-SkipBuild`. Logs land in
`Generated Files/upstream-sync/<YYYY-MM-DD>/build-logs/` (gitignored).

### 8. Write report (always)

Regardless of outcome (ok / no-op / dry-run / stuck / stuck-static-scan /
stuck-build-failed / stuck-build-inconclusive / stuck-toolchain-missing),
write `Generated Files/upstream-sync/<YYYY-MM-DD>/<timestamp>-<suffix>.md`
with:

- Run metadata (start, end, duration, host, status)
- Counts: picked / dropped-pair / empty / known-conflict-resolved / stuck-at
- For each picked commit: SHA, subject, author
- For dropped pairs: the two SHAs and their subjects
- If stuck (Tier-3): the conflicting commit, the conflicting paths, what was attempted, the exact resume command
- If stuck (Tier-4): the validation findings, the build log tail, the exact resume command

Reports are **transient** — never committed. The stuck issue body
(step 9b/9c) inlines the parts of the report a reviewer needs without
fetching the local file.

Script: [`05-write-report.ps1`](../scripts/05-write-report.ps1).

### 9a. Success path — push + open PR

```pwsh
git push -u origin $branch
gh pr create -R microsoft/intelligent-terminal --base main --head $branch --title "chore(upstream): sync up to $shortSha" --body-file $reportPath
```

No state-file commit. The `(cherry picked from commit <sha>)` trailer on
each cherry-pick IS the watermark — once the PR merges, the trailer is
on `origin/main` and the next run's `Get-LastSyncedUpstreamSha` finds it.

Script: [`06-finalize-pr.ps1`](../scripts/06-finalize-pr.ps1).

### 9b. Stuck path (Tier-3) — open labeled issue

```pwsh
gh issue create -R microsoft/intelligent-terminal --label upstream-sync-stuck `
  --title "Upstream sync stuck at <shortSha>: <subject>" `
  --body-file $reportPath
```

The issue body carries a fenced ```yaml # wta-state``` block with
`tier`, `kind=cherry-pick-conflict`, `stuck_on_sha`, `branch`, `at`,
`host` so a future run's `Get-StuckMetaFromIssue` can read the lock
context. Nothing is committed to `main`.

Script: [`07-open-stuck-issue.ps1`](../scripts/07-open-stuck-issue.ps1).

### 9c. Stuck path (Tier-4) — open labeled issue

For Tier-4a/b/c, same flow as 9b — open the labeled issue with a
`# wta-state` block carrying `tier=4`, `kind`, `findings_hash`,
`picked_count`. For Tier-4d (toolchain-missing), NO issue is opened
(infra problem); the next scheduler tick simply retries.

Script: [`07b-open-validation-stuck-issue.ps1`](../scripts/07b-open-validation-stuck-issue.ps1).

### 10. After-PR review handling (post-merge gate)

Once the sync PR is open and reviewers (the GitHub Copilot bot, then
humans) start leaving comments, route the response by **comment kind**,
not by reviewer:

| Comment kind | Where to fix |
|---|---|
| Build-blocking on the sync PR — compile errors, dedup of conflicts surfaced only at build time, sync-PR CI gate failures (check-spelling, lint, format) genuinely caused by the cherry-picked content | **Sync PR**, in **one** focused extra commit. Anything more than one extra commit is a smell. |
| Everything else — Copilot correctness findings, logic suggestions, translation corrections, spelling allow/expect migrations, doc/comment typos, design feedback | **Follow-up PR** based on the sync PR's HEAD. |

The cherry-pick PR's value to a reviewer is "N small commits, each
faithful to one upstream commit, plus the bare minimum to make CI
green". Bundling substantive review fixes destroys that audit
property and forces the reviewer to mentally subtract those commits
from every upstream-comparison check.

**Follow-up PR mechanics** (full procedure in
[follow-up-pr.md](./follow-up-pr.md)):

1. Open a sibling worktree on a new branch off the sync PR's head:
   ```pwsh
   git fetch origin <sync-branch>
   git worktree add ..\it-<sync-pr>fix -b dev/<alias>/sync-<sync-pr>-review-fixes "origin/<sync-branch>"
   ```
2. Apply fixes as **one focused commit per concern** (code-bugs /
   translations / spelling-cleanup / etc.) — same "one commit per
   round" rule as
   [`copilot-pr-review-loop`](../../copilot-pr-review-loop/SKILL.md).
3. `gh pr create --base <sync-branch> --head dev/<alias>/sync-<sync-pr>-review-fixes` —
   base is the **sync branch**, not `main`, so only the fix commits
   show in the diff.
4. Walk every deferred review thread on the sync PR and reply +
   resolve pointing to the follow-up PR number, via
   [`copilot-pr-review-loop/scripts/06-reply-and-resolve.ps1`](../../copilot-pr-review-loop/scripts/06-reply-and-resolve.ps1).
5. If the sync PR merges first, rebase the follow-up onto `main` and
   `gh pr edit <follow-up-pr> --base main`.

The PR banner emitted by
[`scripts/06-finalize-pr.ps1`](../scripts/06-finalize-pr.ps1) spells
this policy out so the first reviewer doesn't push back on deferred
fixes.

## Stuck-Lock

When `Get-StuckIssues` returns any OPEN `upstream-sync-stuck` labeled
issue, the orchestrator:

1. Logs `"stuck-lock set (<tier> at <url>); skipping run"`.
2. Writes a transient `<timestamp>-skipped.md` under
   `Generated Files/upstream-sync/<date>/` noting the skip.
3. Exits 0 (the scheduler should not alarm).

To clear the lock after the human has resolved the underlying issue:

```
1. Resolve the conflict on the stuck branch (the exact name is in the
   stuck-issue body, e.g. `upstream-sync/2026-06-04-091512-a3f1`),
   keeping every `(cherry picked from commit <sha>)` trailer intact.
2. Open a PR for the fix, merge it (rebase or merge — NOT squash).
3. CLOSE the stuck issue. That's the lock-clear signal — no script.
```

The next scheduler tick:
- `Get-LastSyncedUpstreamSha` re-derives the watermark from the merged
  PR's trailers (advancing past the resolved batch — for Tier-3 by the
  exact resolved commit; for Tier-4 by whatever extra trailers the fix
  PR carried).
- `Get-StuckIssues` returns empty (the issue is closed).
- The run proceeds from the new watermark.

For Tier-4 where the operator wants to **re-attempt the same range**
(e.g. because the fix landed as a separate PR on `main` that doesn't
itself carry trailers), simply close the issue without merging a sync
fix: the next run will recompute pending against the same watermark and
re-validate.

## Sub-Agent Delegation Map

| Step | Delegate to fresh sub-agent? | Why |
|---|---|---|
| 1–2 (fetch, compute) | No | Pure git plumbing, deterministic. |
| 3 (revert-pair detection) | No | Mechanical; the script does it. |
| 6 / Tier-2 (LLM-assisted textual resolution) | **Yes — required** | Implementer bias risk; require `high` confidence and a different agent to verify before staging. |
| 7 (write report) | No | Template fill. |
| 8a (PR body polish) | Optional | If picked > 20 commits, a sub-agent can group them by area for the PR body. |
| 8b (issue summary) | **Yes** | A fresh agent writes a clearer "what's hard about this conflict" summary than the loop that aborted. |

## Exit Codes (from `04-run-batch.ps1`)

| Code | Meaning |
|---|---|
| 0 | Success (PR opened) **or** no-op **or** skipped because lock is set |
| 10 | Stuck — issue opened, lock set (this is **not** an error; scheduler should not alarm) |
| 20 | Hard failure (git command failed unexpectedly, network down, gh auth missing) — scheduler **should** alarm |

Wrap the scheduler invocation accordingly: treat 0 and 10 as healthy,
20 as paging-worthy.
