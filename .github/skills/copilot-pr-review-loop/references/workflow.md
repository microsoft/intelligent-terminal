# Copilot PR Review Loop — Full Workflow

Detailed procedure for one round of the loop. Repeat until both convergence
conditions in step 8 hold, then run step 9 once.

## How to run this workflow

Most steps below are best executed by a **fresh sub-agent** (via the
`task` tool), not directly by the parent agent. The mapping is in
SKILL.md → "Delegate Each Step to a Fresh Sub-Agent". The parent owns
sequencing, the `git commit`/`git push`, and the final
`reply-and-resolve` call after replies have been reviewed.

## 1. Request a Copilot review

Use the `gh pr edit --add-reviewer` form — the only consistently working
mechanism. The GraphQL `requestReviews` mutation no longer accepts the
Copilot bot login, and REST `requested_reviewers` rejects bots with HTTP
422. See [api-quirks.md](api-quirks.md) for details.

```powershell
pwsh ../scripts/01-request-review.ps1 -PrNumber <pr-number>
```

The script is idempotent — re-running it triggers a fresh review.

## 2. Wait, then fetch open threads

Copilot typically posts a new review within 3–6 minutes; allow up to 10.
Don't poll faster than ~3 minutes — there is no progress signal and faster
polling only wastes API budget.

```powershell
Start-Sleep -Seconds 360
pwsh ../scripts/02-list-open-threads.ps1 -Owner <owner> -Repo <repo> -PrNumber <pr-number>
```

Filter for **open AND non-outdated** threads only. Outdated threads point at
lines you've since rewritten and are not actionable; they get batch-resolved
at convergence (step 9) instead.

## 3. Triage each finding

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

## 4. Implement fixes — one focused commit per round

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

## 5. Build and test before pushing

Never push a fix you haven't compiled. If the project has unit tests for
the changed code, re-run them. A fix that breaks the build wastes another
full review cycle.

## 6. Reply to and resolve each thread

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

## 7. Request the next round and loop

Go back to step 1. Each round, Copilot sees:

- the new diff,
- your replies on prior threads,
- the updated PR description.

Your replies actively shape what the next round will surface — declining a
finding with strong reasoning typically prevents Copilot from re-raising it.

## 8. Convergence

You are done when **both** conditions hold:

- A review returns: *"Copilot reviewed N out of N changed files in this
  pull request and generated no new comments."*
- The open-threads list (step 2) is empty.

A single condition is not enough. A "no new comments" review can still
coexist with an open thread from a prior round if you forgot to resolve it.

## 9. Cleanup outdated Copilot threads (final, once)

Even after convergence, the PR may show old `isOutdated: true` Copilot
threads still listed as open. They are already addressed by later commits,
but they clutter the conversation tab. Batch-resolve them:

```powershell
pwsh ../scripts/09-cleanup-outdated.ps1 -Owner <owner> -Repo <repo> -PrNumber <pr-number>
```
