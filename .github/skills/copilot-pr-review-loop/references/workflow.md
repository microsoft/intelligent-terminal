# Copilot PR Review Loop — Full Workflow

Detailed procedure for one round of the loop. Repeat until all three
convergence conditions in step 9 hold, then run step 10 once.

## Per-round checklist

Track progress through one round with this list (copy into your scratch
notes or session todos):

- [ ] **1.** Request review — `pwsh ../scripts/01-request-review.ps1 -PrNumber <n>`. Returns JSON immediately with `Status: TriggerLanded | InFlight` on success, or throws on failure. NEVER blocks waiting for the actual review submission — that's the agent's job (see step 2).
- [ ] **2.** Wait at the agent level, then snapshot — schedule a check 3-5 minutes after step 1, then call `pwsh ../scripts/02-check-review-status.ps1 -PrNumber <n>`. Returns single-shot JSON with `ReviewAtHead`, `NoNewComments`, `OpenThreadCount`, `Converged` (all booleans). If `ReviewAtHead == false` after 5 min, wait another few minutes and call again — the bot usually responds in 3-15 min total. **Do NOT use a blocking wait script** — that approach was deprecated; the agent's own scheduling is the right place for the wait loop.
- [ ] **3.** List open threads — `pwsh ../scripts/02-list-open-threads.ps1 -PrNumber <n>` (prints every unresolved thread — reply + resolve them all)
- [ ] **4.** Triage each finding using [03-triage-criteria.md](03-triage-criteria.md)
- [ ] **5.** Apply fixes — one sub-agent per independent change
- [ ] **6.** Build + run affected tests (no unverified pushes)
- [ ] **7.** Reply + resolve each thread using [06-reply-templates.md](06-reply-templates.md) → `pwsh ../scripts/06-reply-and-resolve.ps1`
- [ ] **8.** Commit + push the round's changes (one focused commit per round)
- [ ] **9.** Convergence check — call `pwsh ../scripts/02-check-review-status.ps1 -PrNumber <n>`; converged iff its JSON shows `Converged: true` (which is set when ALL THREE of `ReviewAtHead && NoNewComments && OpenThreadCount==0` hold).
- [ ] **10.** (once at end of loop) Cleanup outdated — `pwsh ../scripts/09-cleanup-outdated.ps1 -PrNumber <n>` (safety net only — most loops converge with nothing to clean)

If step 9 fails on any condition, loop back to step 1. If step 9 passes
on all three, run step 10 once and you're done. Print the review's
`commit.oid` and `submittedAt` in your task-complete message — proof,
not assertion.

## Sub-agent delegation map

Most steps below are best executed by a **fresh sub-agent** (via the
`task` tool), not directly by the parent agent. The parent owns
sequencing, the `git commit`/`git push`, and the final
`reply-and-resolve` call after replies have been reviewed.

| Step | Sub-agent role | Why |
|------|----------------|-----|
| 3 — List open threads | Categorize each finding by file/severity | One-shot, deterministic; useful as a fresh read of what's outstanding |
| 4 — Triage | Apply the rubric in [03-triage-criteria.md](03-triage-criteria.md), return fix/decline per thread | Fresh judgment, not contaminated by the implementer's intent |
| 5 — Fix | One sub-agent per independent fix; run in parallel where possible | Parallelism; isolated context per fix |
| 6 — Build & test | Run the project's build + unit tests, return only failures | Keeps long build output out of the parent context |
| 7 — Reply drafting | Draft replies using [06-reply-templates.md](06-reply-templates.md) | Consistency; avoids drift between replies on related threads |
| 9 — Convergence check | Re-run step 3's script + re-query latest review's `commit.oid`, compare to current HEAD | Independent verification of all three convergence conditions |

## 1. Request a Copilot review

Run [scripts/01-request-review.ps1](../scripts/01-request-review.ps1). It snapshots
state via the GraphQL `reviews(last:50)` connection (NOT `latestReviews` —
that field has stale-cache behavior), then returns one of these outcomes:

- **InFlight (exit 0, no trigger)** — a recent `copilot_work_started`
  event exists and is newer than the latest Copilot review's
  submittedAt. Triggering again would risk cancelling the in-flight
  review. Move to step 2 to wait for the submission.
- **TriggerLanded** (default success path, returned when the in-flight
  protection didn't fire and a trigger attempt produced a
  `copilot_work_started` event). The script attempts three mechanisms in
  order:
  1. **PRIMARY: GraphQL `requestReviewsByLogin`** with
     `botLogins:["copilot-pull-request-reviewer"]`. Empirically the
     most reliable. Three traps: use `requestReviewsByLogin` (not
     `requestReviews`), `botLogins` (not `userLogins`), and the
     `copilot-pull-request-reviewer` slug (not `Copilot`).
  2. **FALLBACK: REST POST** `requested_reviewers[]=Copilot`,
     verified by polling for a `copilot_work_started` event.
  3. **FALLBACK: `gh pr edit --add-reviewer Copilot`**. Known to
     return "not found" on current gh CLI for many accounts; kept
     as last-ditch.

Re-request is supported as a first-class flow — the script does NOT
silently skip when Copilot has already reviewed; it issues the same
mutation and verifies via the event log.

HTTP / exit status alone is NOT sufficient — the server can silently
drop re-reviews while returning success. See [api-quirks.md](api-quirks.md).

```powershell
pwsh ../scripts/01-request-review.ps1 -PrNumber <pr-number>
```

If no `copilot_work_started` event lands, the script throws with
actionable diagnostics. The canonical remedy when triggers are
silently dropped is to push a substantive (non-whitespace,
non-comment-only) commit — most repos auto-assign Copilot on
`synchronize` and that path is the most reliable.

**DO NOT** post `@copilot please review` (or any @copilot mention) as a
PR comment. That summons the Copilot **Coding Agent** (which makes
commits), not the reviewer bot. This anti-pattern has been observed
across multiple Copilot CLI sessions and is a confirmed waste of time.

## 2. Wait for the review at the AGENT level, then snapshot

The trigger from step 1 returned `TriggerLanded` (or `InFlight`). The
review submission usually lands 3-15 minutes later. **Do NOT block in
a script**; the agent owns the wait loop.

Pattern: schedule a status snapshot 3-5 minutes after step 1, then
call:

```powershell
pwsh ../scripts/02-check-review-status.ps1 -PrNumber <pr-number>
```

This is a single-shot, no-wait JSON snapshot. Key fields:

- `ReviewAtHead` (boolean) — latest Copilot review's `commit.oid` matches PR HEAD
- `NoNewComments` (boolean) — latest review body matches "no new comments" / "generated 0 comments"
- `OpenThreadCount` (integer) — unresolved review threads from any reviewer
- `Converged` (boolean) — `ReviewAtHead && NoNewComments && OpenThreadCount == 0`

If `ReviewAtHead == false`: review hasn't landed yet. Wait another
3-5 minutes (or longer for trivial-diff suppression) and snapshot
again. **Do not block** — the agent can do other work between checks.

If you're using the `task` tool to dispatch a sub-agent for the wait
loop: have the sub-agent loop on `02-check-review-status.ps1` with
sleeps between calls. The sub-agent reports back when `Converged ==
true` or after a hard deadline.

**Hard rule**: do NOT call `task_complete` on the review loop until
the snapshot shows `Converged: true`. The previous "wait for nothing"
failure mode came from blocking-wait scripts that timed out without
clear signal. The single-shot snapshot avoids that.

## 3. Fetch open threads

```powershell
pwsh ../scripts/02-list-open-threads.ps1 -PrNumber <pr-number>
```

The script emits every unresolved thread and does not truncate comment
bodies. There is no "actionable-only" or "not outdated" mode; current
unresolved thread state is the source of truth for convergence.

`-Owner` / `-Repo` default to the current repo via `gh repo view`.

## 4. Triage each finding

Apply the decision rubric in [03-triage-criteria.md](03-triage-criteria.md). The
short version:

- **Fix** real correctness bugs (use-after-free, races that drop user
  intent, gating logic that skips legitimate transitions, missing link
  dependencies), and documentation/test-plan drift from implemented
  behavior.
- **Decline** purely hypothetical races needing cross-class plumbing,
  style/naming nits, and abstraction suggestions that don't pay for
  themselves at current scale.

Always **state your reasoning** in the reply, whether you fix or decline.
This makes the PR self-documenting and gives the next Copilot review
visible context.

## 5. Implement fixes — one focused commit per round

Keep commits granular: one commit per review round (or per finding if a
round has multiple unrelated findings). This makes the PR history narrate
the review evolution and keeps `git bisect` honest.

For projects with **uncommitted local-build patches** (e.g. toolchain
overrides held out of the PR), stash before committing and restore after:

```powershell
git stash push -m "local-build" -- <paths-to-stash>
git add <files-you-changed>
git commit -m "Short title" `
           -m "Body explaining the finding and the fix" `
           -m "Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>"
git push
git stash pop
```

Always include the Copilot `Co-authored-by` trailer when the fix came from
a Copilot finding.

## 6. Build and test before pushing

Never push a fix you haven't compiled. If the project has unit tests for
the changed code, re-run them. A fix that breaks the build wastes another
full review cycle.

## 7. Reply to and resolve each thread

Reply first (explain what you did, cite the commit SHA), then resolve. Use
the templates in [06-reply-templates.md](06-reply-templates.md).

```powershell
pwsh ../scripts/06-reply-and-resolve.ps1 `
    -ThreadId <thread-id> `
    -Body "Did X because Y. Fixed in <commit-sha>."
```

For **declined** findings, the reply explains why you're not fixing it —
then still resolve the thread. Leaving threads open without explanation
clutters the PR and signals you're avoiding the feedback.

## 8. Commit + push the round's changes

One focused commit per round. Include the `Co-authored-by: Copilot`
trailer when fixes came from Copilot findings.

## 9. Convergence check (all THREE must hold)

You are done ONLY when all three conditions hold simultaneously:

1. **The latest Copilot review's `commit.oid` equals the current PR
   HEAD SHA.** A "no new comments" review against an earlier commit is
   stale — it did not see your most recent fix. Verify with:

   ```powershell
   gh api graphql `
     -f owner=<owner> -f repo=<repo> -F pr=<n> `
     -f query='query($owner:String!,$repo:String!,$pr:Int!){repository(owner:$owner,name:$repo){pullRequest(number:$pr){headRefOid reviews(last:50){nodes{author{login} submittedAt state commit{oid}}}}}}' `
     --jq '{head:.data.repository.pullRequest.headRefOid, latest:(.data.repository.pullRequest.reviews.nodes | map(select(.author.login|test("^(?i)copilot"))) | sort_by(.submittedAt) | last | {submittedAt,state,commit:.commit.oid})}'
   ```

   Or just call `02-check-review-status.ps1` — it returns
   `ReviewAtHead` (boolean) and `LatestCopilotReview.commitOid` so
   you can verify (a) directly from the snapshot.

2. **The review body is the "generated no new comments" form.**
   `02-check-review-status.ps1` returns `NoNewComments` (boolean) for
   this. Quote `LatestCopilotReview.bodyHead` in your task-complete
   message as proof.

3. **`02-list-open-threads.ps1` returns empty** — or equivalently,
   `02-check-review-status.ps1` returns `OpenThreadCount: 0`.

The `02-check-review-status.ps1` script computes all three as a
single `Converged: true` boolean. The loop is done iff that's true.

If any one is false, the loop is not done. Print the
`commit.oid` + `submittedAt` in your completion message — proof, not
assertion.

## 10. Cleanup outdated Copilot threads (final, once)

Even after convergence, the PR may show old `isOutdated: true` Copilot
threads still listed as open. They are already addressed by later commits,
but they clutter the conversation tab. Batch-resolve them:

```powershell
pwsh ../scripts/09-cleanup-outdated.ps1 -Owner <owner> -Repo <repo> -PrNumber <pr-number>
```
