---
name: copilot-pr-review-loop
description: 'Drive a GitHub pull request through repeated rounds of Copilot code review until convergence. Use when the user asks to "request Copilot review", "run a Copilot review loop", iterate on Copilot feedback, or wants automated triage-and-respond on Copilot PR comments. Covers re-request mechanics, open-thread filtering, fix-vs-decline triage, reply-and-resolve, and end-of-loop cleanup.'
---

# Copilot PR Review Loop

A workflow for driving a pull request through repeated rounds of GitHub
Copilot code review until a round produces no new comments and the
open-threads list is empty.

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

## Critical Anti-Patterns (READ THIS FIRST)

Two failure modes will burn a session if you don't internalize them up front:

1. **Never post `@copilot please review` (or any `@copilot` mention) as
   a PR comment to trigger code review.** That summons the Copilot
   **Coding Agent** (which makes commits), not the reviewer bot. It
   will not produce a code review. The valid triggers are the API
   mechanisms in [scripts/01-request-review.ps1](scripts/01-request-review.ps1);
   if they fail, push a substantive commit (auto-assign on
   `synchronize` is the most reliable trigger).
2. **HTTP 200 / exit 0 from a trigger call is NOT proof Copilot
   accepted it.** The server can silently drop a request — quiet-period
   after dismissal, repo without Copilot enabled, bot not a
   collaborator. The authoritative success signal is a
   `copilot_work_started` event in the issue timeline newer than your
   request. Convergence requires a Copilot review whose `commit.oid`
   equals the current HEAD — not just "a review exists" and not just
   "no new comments".

The script + workflow enforce both rules; if you bypass them you will
reproduce the documented "wait for nothing" / false-done failures.

## Prerequisites

- `gh` CLI authenticated against the target repository.
- PowerShell 7+ (`pwsh`) on PATH for the bundled scripts.

## Step-by-Step Workflows

The loop has ten steps. Run steps 1–8 each round; check convergence at
step 9; run step 10 once when the loop terminates. Full procedure, with
commands, rationale, the per-step sub-agent delegation table, and a
resumable checklist, is in [references/workflow.md](references/workflow.md).

```
Request review + verify pickup → Agent-level wait + status check → List
open threads → Triage → Fix → Build/test → Reply + resolve → Commit +
push → Convergence check → Cleanup outdated (final, once)
```

Terminate when a review with `commit.oid == current HEAD` returns "no new
comments" **and** the open-threads list is empty. Three things must be
true simultaneously for convergence:

1. The latest Copilot review's `commit.oid` equals the PR HEAD SHA.
   (A "no new comments" review against an older commit is stale — it
   did not see your most recent fix.)
2. That review's body is the "generated no new comments" form.
3. There are no unresolved threads. Prefer
   `02-check-review-status.ps1`'s `OpenThreadCount == 0`; the human
   `02-list-open-threads.ps1` output prints `No open threads.` for that
   case.

If any one is false, the loop is not done. Do **not** call
`task_complete` until all three are verified — print the review's
commit OID + submittedAt in the completion message as proof, not as
assertion.

**Delegate substantive steps to a fresh sub-agent.** Each round's triage,
fix-drafting, and reply-drafting benefit from a clean context (no
implementer bias, parallelizable, less noise in the parent). The parent
agent owns sequencing, commits, and the final mutating
`reply-and-resolve` calls. The per-step delegation map is in
[references/workflow.md](references/workflow.md#sub-agent-delegation-map).

## Gotchas

- **NEVER post `@copilot please review` (or any @copilot mention) as a
  PR comment** to trigger a code review. That summons the Copilot
  **Coding Agent** (which makes commits), not the reviewer bot. It will
  not produce a review. The valid triggers are the API mechanisms in
  [scripts/01-request-review.ps1](scripts/01-request-review.ps1):
  GraphQL `requestReviewsByLogin` with
  `botLogins:["copilot-pull-request-reviewer"]` — empirically the
  most reliable **API** trigger across personal/org repos. The
  trigger is verified via the `copilot_work_started` event in the
  issue timeline. If none works, push a substantive commit and
  retry — do not fall back to @-mentions.
- **Pushing a substantive commit is the most reliable overall
  fallback.** Most repos auto-assign Copilot on `synchronize`. When
  `01-request-review.ps1` fails (quiet-period after dismissal, silent
  server-side drop, Copilot not enabled), the recommended remedy is
  to commit a real change (non-whitespace, non-comment-only) and rely
  on auto-assignment.
- **HTTP 200 / exit 0 from a re-request call is NOT proof Copilot
  accepted it.** The server can silently drop trivial-diff re-reviews.
  The only authoritative signal is a `copilot_work_started` event newer
  than your request. `01-request-review.ps1` already enforces this; do
  not weaken it.
- **A "no new comments" review is necessary but not sufficient for
  convergence.** Use `02-check-review-status.ps1` which returns a
  `Converged: true` flag iff all three hold: latest review's
  `commit.oid == HEAD`, body matches "no new comments", open thread
  count is 0. A stale review on an earlier commit lets a regression
  slip through unreviewed.
- **Do not improvise alternate trigger APIs.** Use
  [scripts/01-request-review.ps1](scripts/01-request-review.ps1) and see
  [references/api-quirks.md](references/api-quirks.md) for the verified
  trigger details.
- **`git stash push -m` must come before `--`.** The form
  `git stash push -- <paths> -m <msg>` parses `<msg>` as a path and
  silently produces a stash with no message.
- **`gh api graphql -F` type-coerces strings.** Use `-f key=value` for any
  `String!` variable (`owner`, `repo`, `body`, `tid`, `after`); reserve
  `-F` for numeric/boolean variables. A reply body that happens to be
  `"true"` or all digits otherwise fails silently with a type error. See
  [references/api-quirks.md](references/api-quirks.md).
- **Reply *and* resolve every thread, including declines and outdated
  ones.** Resolving without a reply leaves no record of why the issue
  was considered addressed; replying without resolving keeps the
  open-threads list non-empty and blocks convergence. Outdated threads
  (whose cited lines have since shifted) still need reply + resolve —
  they show up in the PR UI as unresolved until you explicitly close
  them.
- **One focused commit per round, not one per PR.** Bundling rounds
  destroys the audit trail of which finding drove which change and breaks
  `git bisect`.
- **Don't push a fix you haven't compiled.** A broken build wastes the
  next full review cycle (3–10 minutes).
- **Don't poll for the new review faster than ~3 minutes.** There is no
  progress signal; faster polling only wastes API budget.
- **Spell-check / format-check findings follow project-specific policies.**
  Some repos reword text rather than adding to an allowlist; check the
  project's spelling config conventions before adding entries.
- **Push back with written rationale** when a finding would over-engineer
  the design for a hypothetical edge case. Auto-accepting every Copilot
  suggestion erodes the design.

## Troubleshooting

| Issue | Solution |
|-------|----------|
| Trigger fails or no `copilot_work_started` event lands | Push a substantive (non-whitespace) commit — repo auto-assign on `synchronize` is the most reliable trigger. Persistent failure after a substantive commit indicates Copilot Code Review may not be enabled on the repo or account (Settings → Code & automation → Copilot, or account-level Copilot Pro/Pro+). |
| No new review after waiting ~10 min between snapshots | Quiet-period after recent dismissal or trivial-diff suppression. Push a substantive commit (auto-assign on `synchronize` is the most reliable trigger). Do not blindly re-run `01-request-review.ps1` — it reports `InFlight` only while Copilot is still in `requested_reviewers`; otherwise it may attempt the GraphQL trigger again. |
| Outdated-but-unresolved threads appear in the open-threads list | This is expected: current unresolved state is the source of truth. Reply + resolve them like any other open thread. `09-cleanup-outdated.ps1` is only a final safety net, not the primary mechanism. |
| Unsure whether to fix or decline a finding | Apply the rubric in [references/03-triage-criteria.md](references/03-triage-criteria.md) |
| Need a reply that conveys "fixed", "declined", or "drift" | Use a template from [references/06-reply-templates.md](references/06-reply-templates.md) |
| `list-open-threads` still shows resolved-looking threads | The script lists every `!isResolved` thread. Resolved-looking but still-open threads usually mean someone resolved the GitHub UI conversation without the GraphQL `resolveReviewThread` mutation completing. |

## References

- [references/workflow.md](references/workflow.md) — full ten-step
  procedure with commands and rationale.
- [references/03-triage-criteria.md](references/03-triage-criteria.md) —
  fix-vs-decline decision rubric.
- [references/api-quirks.md](references/api-quirks.md) — verified GitHub
  API dead-ends; read before scripting any Copilot reviewer interaction.
- [references/06-reply-templates.md](references/06-reply-templates.md) — reply
  patterns for accepted fixes, declined-with-rationale findings, and
  description-update acknowledgements.
- [scripts/01-request-review.ps1](scripts/01-request-review.ps1) —
  single-job script: trigger Copilot review and verify the trigger
  landed (via `copilot_work_started` event in the issue timeline).
  Returns JSON; does NOT wait for the review submission — that's the
  agent's job.
- [scripts/02-check-review-status.ps1](scripts/02-check-review-status.ps1) —
  single-shot JSON snapshot of the PR's current Copilot review state
  (HeadOid, LatestCopilotReview, ReviewAtHead, NoNewComments,
  OpenThreadCount, Converged). Call this in your agent's wait loop.
- [scripts/02-list-open-threads.ps1](scripts/02-list-open-threads.ps1) —
  fetch every unresolved PR review thread from **all reviewers** (Copilot,
  humans, github-advanced-security, etc.); reply + resolve every one.
- [scripts/06-reply-and-resolve.ps1](scripts/06-reply-and-resolve.ps1) — post a
  reply and resolve in one call.
- [scripts/09-cleanup-outdated.ps1](scripts/09-cleanup-outdated.ps1) —
  safety net for outdated threads that slipped past the per-round loop.
