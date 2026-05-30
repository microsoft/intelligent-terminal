---
name: copilot-pr-review-loop
description: 'Drive a GitHub pull request through repeated rounds of Copilot code review until convergence. Use when the user asks to "request Copilot review", "run a Copilot review loop", iterate on Copilot feedback, or wants automated triage-and-respond on Copilot PR comments. Covers re-request mechanics, open-thread filtering, fix-vs-decline triage, reply-and-resolve, and end-of-loop cleanup.'
---

# Copilot PR Review Loop

A verified workflow for driving a pull request through repeated rounds of
GitHub Copilot code review until convergence (a round that produces "no new
comments"). The loop fixes findings that are real and low-risk, declines
findings that would over-engineer the solution, and keeps an audit trail of
both decisions on the PR itself.

## Bundled Assets

- [`references/triage-criteria.md`](references/triage-criteria.md): Decision
  rubric for fix-vs-decline triage and the "ROI vs risk" frame for choosing
  unilateral action vs. asking the user.
- [`references/api-quirks.md`](references/api-quirks.md): Verified GitHub
  REST/GraphQL behaviors and dead-ends — read this before scripting any
  Copilot reviewer interaction.
- [`references/reply-templates.md`](references/reply-templates.md): Reply
  patterns for accepted fixes, declined-with-rationale findings, and
  description-update acknowledgements.
- [`scripts/request-review.ps1`](scripts/request-review.ps1): Re-request a
  Copilot review on a PR.
- [`scripts/list-open-threads.ps1`](scripts/list-open-threads.ps1): Fetch
  open, non-outdated Copilot review threads for a PR.
- [`scripts/reply-and-resolve.ps1`](scripts/reply-and-resolve.ps1): Post a
  reply to a thread and resolve it, in one call.
- [`scripts/cleanup-outdated.ps1`](scripts/cleanup-outdated.ps1): Batch-resolve
  Copilot threads that have already been outdated by later commits — run once
  at convergence.

## When to Use This Skill

- The user asks to "request Copilot review" or "run a Copilot review loop"
  on a PR.
- A PR is functionally complete and the user wants a final correctness pass
  via repeated automated review rounds.
- A previous Copilot review on the PR has left open threads that need
  triage, fixing, replying, and resolving.

## When NOT to Use This Skill

- Trivial PRs (typos, comment-only changes) — a single review is enough.
- The PR is still under active design — wait until the structure is stable;
  otherwise findings churn round-over-round.
- The user wants human reviewer feedback, not Copilot's.

## Loop Overview

```
1. Request a Copilot review                       (scripts/request-review.ps1)
2. Wait ~5-7 min for the review to post
3. List open, non-outdated Copilot threads        (scripts/list-open-threads.ps1)
4. Triage each finding                            (references/triage-criteria.md)
5. Implement accepted fixes; build + run tests
6. Commit + push (one commit per round, granular)
7. Reply + resolve every thread                   (scripts/reply-and-resolve.ps1)
                                                  (references/reply-templates.md)
8. Goto 1

Terminate when: a round returns "no new comments"
                AND the open-threads list is empty.

Final step:    cleanup outdated Copilot threads   (scripts/cleanup-outdated.ps1)
```

## Step-by-Step

### 1. Request a Copilot review

Use the `gh pr edit --add-reviewer` form. The GraphQL `requestReviews`
mutation no longer accepts the Copilot bot login, and REST
`requested_reviewers` rejects bots with HTTP 422. See
[`references/api-quirks.md`](references/api-quirks.md) for details.

```powershell
pwsh scripts/request-review.ps1 -PrNumber <pr-number>
```

`scripts/request-review.ps1` is idempotent — re-running it triggers a fresh
review.

### 2. Wait, then fetch open threads

Copilot typically posts a new review within 3–6 minutes; allow up to 10.
Don't poll faster than ~3 minutes — there is no progress signal and polling
faster wastes API budget without speeding up the review.

```powershell
Start-Sleep -Seconds 360
pwsh scripts/list-open-threads.ps1 -Owner <owner> -Repo <repo> -PrNumber <pr-number>
```

Filter for **open AND non-outdated** threads only. Outdated threads point at
lines you've since rewritten and are not actionable; they get batch-resolved
at convergence (step 9) instead.

### 3. Triage each finding

Apply the decision rubric in
[`references/triage-criteria.md`](references/triage-criteria.md). The short
version:

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

### 4. Implement fixes — one focused commit per round

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

`git stash push` syntax pitfall: `-m` **must come before** `--`. The form
`git stash push -- <paths> -m <msg>` does NOT work.

### 5. Build and test before pushing

Never push a fix you haven't compiled. If the project has unit tests for
the changed code, re-run them. A fix that breaks the build wastes another
full review cycle.

### 6. Reply to and resolve each thread

Reply first (explain what you did, cite the commit SHA), then resolve. Use
the templates in
[`references/reply-templates.md`](references/reply-templates.md).

```powershell
pwsh scripts/reply-and-resolve.ps1 `
    -ThreadId <thread-id> `
    -Body "Did X because Y. Fixed in <commit-sha>."
```

For **declined** findings, the reply explains why you're not fixing it —
then still resolve the thread. Leaving threads open without explanation
clutters the PR and signals you're avoiding the feedback.

### 7. Request the next round and loop

Go back to step 1. Each round, Copilot sees:

- the new diff,
- your replies on prior threads,
- the updated PR description.

Your replies actively shape what the next round will surface — declining a
finding with strong reasoning typically prevents Copilot from re-raising it.

### 8. Convergence

You are done when **both** conditions hold:

- A review returns: *"Copilot reviewed N out of N changed files in this
  pull request and generated no new comments."*
- The open-threads list (step 2) is empty.

A single condition is not enough. A "no new comments" review can still
coexist with an open thread from a prior round if you forgot to resolve it.

### 9. Cleanup outdated Copilot threads

Even after convergence, the PR may show old `isOutdated: true` Copilot
threads still listed as open. They are already addressed by later commits,
but they clutter the conversation tab. Batch-resolve them:

```powershell
pwsh scripts/cleanup-outdated.ps1 -Owner <owner> -Repo <repo> -PrNumber <pr-number>
```

## Things That Do NOT Work (Verified)

See [`references/api-quirks.md`](references/api-quirks.md) for the full
list. Highlights:

- GraphQL `requestReviews` with `botLogins` is rejected by the API.
- REST `POST /repos/.../pulls/<n>/requested_reviewers` with the Copilot bot
  login returns HTTP 422 because bots are not repository collaborators.
- Polling more often than every ~3 minutes does not produce reviews faster.

## Anti-Patterns to Avoid

- **Auto-accept every finding.** Push back with written rationale when a
  suggestion materially complicates the design for a hypothetical edge
  case.
- **Bundle multiple rounds into one commit.** You lose the audit trail of
  which finding drove which change, and `git bisect` becomes much harder.
- **Resolve a thread without replying.** The next reviewer (human or bot)
  has no record of why the issue was considered addressed.
- **Skip the build step.** "Looks right" is not the same as "compiles and
  passes tests."
- **Treat spell-check / format-check findings the same as code-review
  findings.** Those are separate CI signals and follow project-specific
  policies (e.g. some repos reword for English words rather than allowlist
  them).
