# Upstream Sync — Full Workflow

This is the authoritative per-step procedure. The orchestrator is
[`scripts/04-run-batch.ps1`](../scripts/04-run-batch.ps1); each step below
maps to a script or an in-orchestrator function.

## Entry Conditions

- `state.json` exists (bootstrap done — see [bootstrap.md](./bootstrap.md)).
- Working tree is clean (`git status --porcelain` empty).
- We are on `main` (or the script will `git switch main`).
- `state.stuck_on_sha` is `null` AND `state.stuck_validation` is `null` (otherwise exit early — see "Stuck-lock" below).

## Steps

### 1. Fetch upstream

```pwsh
git remote get-url upstream 2>$null || git remote add upstream https://github.com/microsoft/terminal.git
git fetch upstream main --no-tags
```

Script: [`01-fetch-upstream.ps1`](../scripts/01-fetch-upstream.ps1).

Exits with `state.last_run.status = "no-op"` and writes a "nothing to do"
report if `git rev-parse upstream/main` equals `state.last_synced_upstream_sha`.

### 2. Compute pending range

```pwsh
git log --reverse --format='%H' "$last_synced..upstream/main"
```

Oldest-first ordering is mandatory. Cherry-picking newest-first inverts
dependencies and creates spurious conflicts.

Script: [`02-compute-pending.ps1`](../scripts/02-compute-pending.ps1) emits
a JSON array on stdout.

### 3. Detect & drop revert pairs

A commit is a revert if its **first line** matches `^Revert "..."$` **or**
its body contains `This reverts commit <40-hex>`.

- If `<40-hex>` is **inside** the pending range AND has not been picked
  yet → drop **both** the original and the revert; record the pair.
- If `<40-hex>` is **outside** the pending range (already synced earlier)
  → keep the revert; it must land as a normal pick.

Script: same `02-compute-pending.ps1` (returns `{ pending: [...], dropped_pairs: [[A,B],...] }`).

### 4. Drop upstream-empty commits

Before picking, check `git diff-tree --no-commit-id --name-only -r <sha>`.
If empty, mark skipped and record. (Cheaper to detect upfront than to
pick and reset.)

### 5. Create the sync branch

```pwsh
$branch = "upstream-sync/$(Get-Date -Format 'yyyy-MM-dd')"
git switch -c $branch  # or "git switch $branch" if resuming
```

If the branch already exists (resume from same-day run), reuse it.

### 6. Cherry-pick loop

For each commit in the (now-filtered) pending list:

```pwsh
git cherry-pick --keep-redundant-commits -x <sha>
```

- `-x` adds `(cherry picked from commit <sha>)` to the message — critical
  for audit trail and for the next-run revert-pair detector.
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
4. **Tier 3 — semantic conflict.** Run `git cherry-pick --abort`. Set
   the stuck-lock, write report, exit non-zero. The script that calls
   us will then open the stuck issue.

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
If `ok=false`, this is **Tier-4d infra-stuck**: lock + NO GitHub issue
(PR review cannot fix host provisioning). Skipped when `-SkipBuild` is set.

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
stuck**: lock + GitHub issue + exit 10. Skipped when `-SkipStaticScan`.

#### 7c. Try-build

```pwsh
pwsh .github/skills/upstream-sync/scripts/10-try-build.ps1 -BuildCommand 'tools\razzle.cmd && bz no_clean' -TimeoutMinutes 45
# Emits JSON { kind, exit_code, duration_ms, log_path, log_tail }
```

- `kind = build-ok` → continue to step 8.
- `kind = build-failed` → **Tier-4b stuck**.
- `kind = build-inconclusive` (timeout) → **Tier-4c stuck**, unless
  `-AllowInconclusiveBuild` (dev opt-in; never in a scheduler).

Skipped when `-SkipBuild`. Logs land in `.github/upstream-sync/build-logs/`
(git-ignored).

### 8. Write report (always)

Regardless of outcome (ok / no-op / stuck / stuck-static-scan /
stuck-build-failed / stuck-build-inconclusive / stuck-toolchain-missing),
write `.github/upstream-sync/reports/YYYY-MM-DDTHHmm[-suffix].md` with:

- Run metadata (start, end, duration, host, status)
- Counts: picked / dropped-pair / empty / known-conflict-resolved / stuck-at
- For each picked commit: SHA, subject, author
- For dropped pairs: the two SHAs and their subjects
- If stuck (Tier-3): the conflicting commit, the conflicting paths, what was attempted, the exact resume command
- If stuck (Tier-4): the validation findings, the build log tail, the exact resume command

Template: [`reporting.md`](./reporting.md).

Script: [`05-write-report.ps1`](../scripts/05-write-report.ps1).

### 9a. Success path — push + open PR

```pwsh
git push -u origin $branch
gh pr create -R microsoft/intelligent-terminal --base main --head $branch --title "chore(upstream): sync up to $shortSha" --body-file $reportPath
```

Update `state.last_synced_upstream_sha = upstream/main` and commit
`state.json` + the report into the sync branch (amend the last pick or
add a trailing commit titled `chore(upstream-sync): update state`).

Script: [`06-finalize-pr.ps1`](../scripts/06-finalize-pr.ps1).

### 9b. Stuck path (Tier-3) — open issue + set lock

```pwsh
gh issue create -R microsoft/intelligent-terminal --label upstream-sync-stuck `
  --title "Upstream sync stuck at <shortSha>: <subject>" `
  --body-file $reportPath
```

Set `state.stuck_on_sha = <sha>` and `state.stuck_branch = $branch`.
Commit `state.json` and the report on `main` (yes, directly — this is the
lock, and the PR path is blocked). The next scheduled run sees the lock
and exits.

Script: [`07-open-stuck-issue.ps1`](../scripts/07-open-stuck-issue.ps1).

### 9c. Stuck path (Tier-4) — open issue + set lock

For Tier-4a/b/c, the same flow as 9b but the issue title carries the
validation kind and findings hash; `state.stuck_validation` is set
instead of `state.stuck_on_sha`. For Tier-4d (toolchain-missing), only
the lock is set — NO issue is opened.

Script: [`07b-open-validation-stuck-issue.ps1`](../scripts/07b-open-validation-stuck-issue.ps1).

## Stuck-Lock

When **either** `state.stuck_on_sha` (Tier-3) **or** `state.stuck_validation`
(Tier-4) is non-null, the orchestrator:

1. Logs `"stuck-lock set: <description>; skipping run"`.
2. Writes a `reports/YYYY-MM-DDTHHmm-skipped.md` noting the skip.
3. Exits 0 (the scheduler should not retry on the same lock).

To clear the lock after the human has resolved the underlying issue:

```pwsh
# Tier-3: -ResolvedThroughSha is REQUIRED and advances the watermark.
pwsh .github/skills/upstream-sync/scripts/clear-stuck.ps1 -ResolvedThroughSha <sha>

# Tier-4: -ResolvedThroughSha is OPTIONAL. Omit it to keep the watermark
# and have the next run re-attempt the same range (recommended when the
# fix lands as a separate PR on main — the next sync will pick up the
# upstream batch atop the now-fixed main and re-validate).
pwsh .github/skills/upstream-sync/scripts/clear-stuck.ps1
```

This sets `state.last_synced_upstream_sha` (when advanced), clears the
appropriate lock fields, and commits `state.json` on `main`.

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
