# Copilot PR Review Loop — Full Workflow

Detailed procedure for one round of the loop. Repeat until all three
convergence conditions in step 9 hold, then run step 10 once.

## Per-round checklist

Track progress through one round with this list (copy into your scratch
notes or session todos):

- [ ] **1.** Request review — `scripts/01-request-review.ps1 -PrNumber <n>` (snapshots state via GraphQL `reviews(last:50)`, protects in-flight reviews, throws on failure). The script ALWAYS attempts to trigger a fresh review when invoked — re-request is a first-class supported flow. The only case where it exits without triggering is when a copilot_work_started event is genuinely in flight (recent, newer than latest review_requested, newer than latest Copilot review submittedAt). If you see that message, skip directly to step 2 to wait for the in-flight review.
- [ ] **2.** Wait for review submission — `scripts/02-wait-for-review.ps1 -PrNumber <n>` (default 35-min timeout; blocks until a Copilot review against current HEAD is submitted, or returns `ReviewCompleted` / `HeadAdvanced` / `TimedOut` / `Error`). On `ReviewCompleted` the JSON includes `NoNewComments` (boolean) and `BodyHead` so convergence condition (b) can be read mechanically.
- [ ] **3.** List open threads — `scripts/02-list-open-threads.ps1 -PrNumber <n>` (prints every unresolved thread — reply + resolve them all)
- [ ] **4.** Triage each finding using [03-triage-criteria.md](03-triage-criteria.md)
- [ ] **5.** Apply fixes — one sub-agent per independent change
- [ ] **6.** Build + run affected tests (no unverified pushes)
- [ ] **7.** Reply + resolve each thread using [06-reply-templates.md](06-reply-templates.md) → `scripts/06-reply-and-resolve.ps1`
- [ ] **8.** Commit + push the round's changes (one focused commit per round)
- [ ] **9.** Convergence check (ALL THREE must hold):
  - (a) latest Copilot review's `commit.oid` equals current PR HEAD SHA (= `LatestReview.commit.oid` from step 2's `ReviewCompleted` JSON)
  - (b) that review's body is the *"generated no new comments"* form (= `NoNewComments` flag in the same JSON)
  - (c) step 3 (`02-list-open-threads.ps1`) returns empty
- [ ] **10.** (once at end of loop) Cleanup outdated — `scripts/09-cleanup-outdated.ps1 -PrNumber <n>` (safety net only — most loops converge with nothing to clean)

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
that field has stale-cache behavior), then takes one of two paths:

- **AlreadyInFlight (exit 0, no trigger)** — a recent `copilot_work_started`
  event exists AND it's newer than the latest review_requested AND
  newer than the latest Copilot review's submittedAt. Triggering again
  would risk cancelling the in-flight review. Move to step 2 to wait
  for the submission.
- **Stuck-pending re-arm** — Copilot is in `requested_reviewers` but
  no `copilot_work_started` has fired for >5 min after the request.
  The script issues a DELETE+POST cycle to re-arm. This is the ONLY
  path that ever deletes — it never runs while a review is in flight.
- **Trigger** (default path, runs whenever the script is invoked and
  the in-flight protection didn't fire). The script attempts three
  mechanisms in order, verifying each via the `copilot_work_started`
  event:
  1. **PRIMARY: GraphQL `requestReviewsByLogin`** with
     `botLogins:["copilot-pull-request-reviewer"]`. Empirically the
     most reliable. Three traps: use `requestReviewsByLogin` (not
     `requestReviews`), `botLogins` (not `userLogins`), and the
     `copilot-pull-request-reviewer` slug (not `Copilot`).
  2. **FALLBACK: REST POST** `requested_reviewers[]=Copilot`,
     verified by reading the response body's `requested_reviewers`
     and polling.
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

## 2. Wait for the review to actually land

The trigger check in step 1 confirms Copilot accepted the job. Step 2
waits for Copilot to actually **submit** the review against the current
HEAD. These are two different things — past sessions have shipped
"convergence" on a review that was against an earlier commit.

```powershell
pwsh ../scripts/02-wait-for-review.ps1 -PrNumber <pr-number>
```

Default timeout is **35 minutes**, not 10. Small-diff and trivial-diff
reviews can be suppressed/batched for 15–30+ min; shortening the
timeout produces blind retries that compound the suppression and hit
rate limits.

The script returns one of:

- `ReviewCompleted` — a fresh Copilot review at current HEAD landed. Proceed.
- `HeadAdvanced` — someone pushed during the wait. Re-run step 1 + step 2 against the new HEAD.
- `TimedOut` — no review at HEAD in 35 min. Do NOT blindly retry. First verify the `copilot_work_started` event for the trigger that should have produced this review. If the event landed, the bot is suppressing — push a substantive commit and re-trigger.
- `Error` — unrecoverable API/auth issue.

You can run the wait script in a background sub-agent (via the `task`
tool) and use the foreground turn for other independent work
(e.g. drafting reply templates for likely findings). Do NOT end the
turn and "come back later" without a concrete completion signal —
that's how false-done declarations creep in.

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

   Or re-read the `LatestReview` field from the `ReviewCompleted` JSON
   that `02-wait-for-review.ps1` returned in step 2 — it already proves
   `commit.oid == HEAD`. Do **not** re-invoke `02-wait-for-review.ps1`
   to verify convergence; it will time out waiting for a NEWER review.

2. **The review body is the "generated no new comments" form.** Quote
   the body in your task-complete message.

3. **`02-list-open-threads.ps1` returns empty**.

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
