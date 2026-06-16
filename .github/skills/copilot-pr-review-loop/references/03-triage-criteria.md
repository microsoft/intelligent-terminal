# Triage Criteria

Decision rubric for whether to fix or decline each Copilot finding.
The goal is correctness, not appeasement — decline confidently when
warranted.

## ROI vs Risk

Score every proposed change on two axes, then decide:

- **ROI** = value of the fix MINUS the cost to implement MINUS the
  user's cost to review.
- **Risk** = blast radius, reversibility, effect on unrelated code.

| ROI | Risk | Action |
|---|---|---|
| Clear positive | Low | Fix unilaterally. |
| Marginal | Low | Fix if cheap; otherwise decline with rationale. |
| Clear positive | High / Irreversible | Propose to the user first. |
| Marginal | High | Decline. |
| Negative (over-engineering for hypothetical) | Any | Decline. |

## Reviewer-type policy (foundation, not a nit)

Each thread's `author` (returned by `03-list-open-threads.ps1`) drives
who can decide it:

| Reviewer | Default action |
|----------|----------------|
| `copilot-pull-request-reviewer` / `copilot-pull-request-reviewer[bot]` | Loop-owned — triage with the rubric below. (`03-list-open-threads.ps1` reports the raw `author.login`, which may carry the `[bot]` suffix on some surfaces; match both forms.) |
| Human reviewer | **Default `escalate-to-user`** unless the user explicitly scoped them into the loop. Auto-replying or auto-resolving a human thread can hide unaddressed concerns and is socially wrong. |
| `github-advanced-security` / other automated bots | **Default `escalate-to-user`** unless the project has a documented suppression / fix convention you can follow. |

## Fix when the finding is...

- A **real correctness bug**: use-after-free / lifetime violation,
  race that drops user intent, gating logic that skips legitimate
  transitions, missing link dependency, off-by-one, null deref,
  unhandled error path.
- A **cross-cutting concern with a clean local fix**: moving a mutex
  one scope up, pulling a duplicated check into a helper.
- **Documentation / test-plan drift**: PR description claims behavior
  the code no longer matches; comment block describes the wrong
  thing; test-plan checkbox is inverted vs. the code.

## Decline when the finding is...

- A **purely hypothetical race** requiring cross-class plumbing,
  where actual exposure is negligible. Cite the interleaving you
  ruled out.
- **Style, naming, or formatting**. Out of scope for the review loop.
- **Suggestions to add abstractions** ("introduce a strategy pattern",
  "extract an interface") that don't pay for themselves at the
  current scale of the codebase.
- **Suggestions that contradict an established project convention** —
  consistency with surrounding code is usually more valuable than the
  suggestion in isolation.
- **Micro-optimizations** in code that is not on a hot path.

## Project-specific policy hooks

Some findings are decided by **project policy**, not by general
correctness reasoning. Before applying a generic "fix", **research
the repo's own conventions first** — `.github/instructions/*.md`
files (often have an `applyTo` glob that pins them to the changed
file's path), `.github/skills/`, `AGENTS.md`, `CONTRIBUTING.md`, CI
config, and recent commits to similar files. Fan out multiple
`explore` sub-agents when several axes need checking (lint, format,
spell-check, license header, etc.) — don't invent answers. What
looks like an obvious fix may violate a project rule:

- **Spell-check / dictionary findings.** If the project uses a
  spell-checker (`check-spelling`, `cspell`, `typos`, or similar),
  inspect its config and recent commits to learn the local
  convention (reword the document, add a pattern/regex, extend a
  dictionary/allowlist, use an inline ignore). Follow that
  convention; don't invent a new mechanism.
- **Lint suppressions / inline ignore directives.** Most projects
  require an inline rationale comment. Bare suppressions get pushed
  back.
- **License headers / file boilerplate.** Project-specific format,
  often CI-enforced — copy the existing header from a neighbor file.
- **Test framework, mock library, formatter choices.** Follow the
  convention of the surrounding test files; never introduce a new
  framework because Copilot suggested one.

Cite the project's config file or convention in your reply.

## Conflicting comments — break oscillation early

Failure mode: round N "fixes" what comment A asked for; round N+1
comment B objects and asks to revert it. Blindly flip-flopping
ships oscillation and burns rounds.

Resolution rules — apply in order, stop at the first that fires:

1. **Re-derive from principles.** Pick the side that wins on the
   function's contract + surrounding patterns + user's stated
   preferences — not the latest comment.
2. **Prefer the position with the explicit rationale.** Concrete
   failure modes (security, correctness, data loss, perf) win over
   stylistic positions.
3. **Prefer human comments over bot comments** when they directly
   conflict.
4. **If still ambiguous, escalate to the user.** Summarize both
   positions side-by-side with a recommendation; do not silently
   pick.

**Hard stop**: if you are about to make the same edit you
*reverted* in an earlier round, stop and escalate. That is the
unambiguous signature of an oscillation loop.

## Reply hygiene

Every reply — fix or decline — states the reasoning. This makes the
PR self-documenting and gives the next review visible context.
Declined findings cite the prior round and explain why the existing
form is correct, so the next reviewer doesn't raise it again.

## Escalation rules

Stay in autopilot by default; escalate to the user when:

- The finding identifies a **design-level tradeoff** with multiple
  reasonable resolutions and no clear winner.
- The fix would be **large or cross-cutting** (hundreds of lines,
  new architecture, refactor across modules).
- The action is **high-risk or irreversible** (force-push, deletion,
  credentials, production).

Otherwise, decide and proceed.
