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

## Prerequisites

- `gh` CLI authenticated against the target repository.
- PowerShell 7+ (`pwsh`) on PATH for the bundled scripts.

## Step-by-Step Workflows

The loop has nine steps. Run steps 1–7 each round; check convergence at
step 8; run step 9 once when the loop terminates. Full procedure, with
commands and rationale for each step, is in
[references/workflow.md](references/workflow.md).

```
Request review → Wait → List open threads → Triage → Fix → Build/test →
Reply + resolve → Loop → Cleanup outdated (final, once)
```

Terminate when a review returns "no new comments" **and** the open-threads
list is empty. A single condition is not enough — a "no new comments"
review can still coexist with a stale open thread you forgot to resolve.

## Delegate Each Step to a Fresh Sub-Agent

The loop is naturally decomposable. Dispatch a fresh sub-agent (via the
`task` tool) for each step that produces substantive content. This keeps
context clean, avoids self-confirmation bias on triage decisions, and
parallelizes independent work.

| Step | Sub-agent role | Why |
|------|----------------|-----|
| 2 — List open threads | Categorize each finding by file/severity | One-shot, deterministic; useful as a fresh read of what's outstanding |
| 3 — Triage | Apply the rubric in [references/03-triage-criteria.md](references/03-triage-criteria.md), return fix/decline per thread | Fresh judgment, not contaminated by the implementer's intent |
| 4 — Fix | One sub-agent per independent fix; run in parallel where possible | Parallelism; isolated context per fix |
| 5 — Build & test | Run the project's build + unit tests, return only failures | Keeps long build output out of the parent context |
| 6 — Reply drafting | Draft replies using [references/06-reply-templates.md](references/06-reply-templates.md) | Consistency; avoids drift between replies on related threads |
| 8 — Convergence check | Re-run step 2's script and re-list, compare to expected empty set | Independent verification of the convergence condition |

The parent agent owns sequencing, commit/push, and the final
`reply-and-resolve` call (which mutates remote state and shouldn't be
delegated until the reply text has been reviewed).

## Gotchas

- **Use `gh pr edit --add-reviewer copilot-pull-request-reviewer`** to
  request a Copilot review. The GraphQL `requestReviews` mutation rejects
  the Copilot bot login, and REST `requested_reviewers` returns HTTP 422
  because bots are not repository collaborators.
- **`git stash push -m` must come before `--`.** The form
  `git stash push -- <paths> -m <msg>` parses `<msg>` as a path and
  silently produces a stash with no message.
- **`gh api graphql -F` type-coerces strings.** Use `-f key=value` for any
  `String!` variable (`owner`, `repo`, `body`, `tid`, `after`); reserve
  `-F` for numeric/boolean variables. A reply body that happens to be
  `"true"` or all digits otherwise fails silently with a type error. See
  [references/api-quirks.md](references/api-quirks.md).
- **Reply *and* resolve every thread, including declines.** Resolving
  without a reply leaves no record of why the issue was considered
  addressed; replying without resolving keeps the open-threads list
  non-empty and blocks convergence.
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
| `gh api` request to add Copilot reviewer returns HTTP 422 | Use `gh pr edit --add-reviewer copilot-pull-request-reviewer` (see [api-quirks.md](references/api-quirks.md)) |
| No new review after ~10 minutes | Re-run the request — `scripts/01-request-review.ps1` is idempotent |
| Outdated threads still appear in the open-threads list | Run [scripts/09-cleanup-outdated.ps1](scripts/09-cleanup-outdated.ps1) once at convergence |
| Unsure whether to fix or decline a finding | Apply the rubric in [references/03-triage-criteria.md](references/03-triage-criteria.md) |
| Need a reply that conveys "fixed", "declined", or "drift" | Use a template from [references/06-reply-templates.md](references/06-reply-templates.md) |
| `list-open-threads` still shows resolved-looking threads | Filter is `!isResolved && !isOutdated` only — the script already does this; resolved-looking but still-open threads usually mean someone resolved the GitHub UI conversation without the GraphQL `resolveReviewThread` mutation completing |

## References

- [references/workflow.md](references/workflow.md) — full nine-step
  procedure with commands and rationale.
- [references/03-triage-criteria.md](references/03-triage-criteria.md) —
  fix-vs-decline decision rubric.
- [references/api-quirks.md](references/api-quirks.md) — verified GitHub
  API dead-ends; read before scripting any Copilot reviewer interaction.
- [references/06-reply-templates.md](references/06-reply-templates.md) — reply
  patterns for accepted fixes, declined-with-rationale findings, and
  description-update acknowledgements.
- [scripts/01-request-review.ps1](scripts/01-request-review.ps1) — re-request a
  Copilot review on a PR.
- [scripts/02-list-open-threads.ps1](scripts/02-list-open-threads.ps1) — fetch
  open, non-outdated Copilot review threads.
- [scripts/06-reply-and-resolve.ps1](scripts/06-reply-and-resolve.ps1) — post a
  reply and resolve in one call.
- [scripts/09-cleanup-outdated.ps1](scripts/09-cleanup-outdated.ps1) —
  batch-resolve outdated Copilot threads at convergence.
