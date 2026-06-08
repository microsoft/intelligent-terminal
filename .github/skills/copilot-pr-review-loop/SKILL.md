---
name: copilot-pr-review-loop
description: 'Drive a GitHub pull request through repeated rounds of Copilot code review until convergence. Use when the user asks to "request Copilot review", "run a Copilot review loop", iterate on Copilot feedback, or wants automated triage-and-respond on Copilot PR comments. Covers re-request mechanics, open-thread filtering, fix-vs-decline triage, reply-and-resolve, and end-of-loop cleanup.'
---

# Copilot PR Review Loop

Drive any GitHub pull request through repeated rounds of Copilot code
review until a round produces no new comments **and** the open-threads
list is empty. Repository-agnostic — works on any repo that has
Copilot Code Review enabled, run from a machine with `gh` CLI
installed and authenticated (see Prerequisites).

## When to Use This Skill

- The user asks to "request Copilot review" or "run a Copilot review loop"
  on a PR.
- A PR is functionally complete and the user wants a final correctness pass
  via repeated automated review rounds.
- A previous Copilot review on the PR has left open threads that need
  triage, fixing, replying, and resolving.

## When NOT to Use This Skill

- The PR is still under active design — wait until the structure is stable;
  otherwise findings churn round-over-round.
- The user wants human reviewer feedback, not Copilot's.

## Prerequisites

- `gh` CLI installed and authenticated against the target repository.
- PowerShell 7+ (`pwsh`) on PATH — any 7.x. The bundled scripts use
  `System.Diagnostics.ProcessStartInfo.ArgumentList` which is .NET
  Core / .NET 5+ only; Windows PowerShell 5.1 (running on .NET
  Framework) is NOT supported.
- The repository must have Copilot Code Review enabled (repo or
  account-level Copilot Pro/Pro+); if not, the trigger step will
  cleanly throw and the loop cannot proceed.

Every script dot-sources [scripts/_lib.ps1](scripts/_lib.ps1) which
runs `Assert-GhReady` on load: if `gh` is missing OR `gh auth status`
fails, the script halts **before any work** with a single actionable
error message naming the install command and `gh auth login`. The
agent should surface that message to the user verbatim and stop the
loop — do not retry or work around it.

## Step-by-Step Workflow

Each round runs steps 1–9; step 10 is a one-time cleanup after
convergence. Steps are coordinated by the parent agent and **every
substantive step is delegated to a fresh sub-agent with a bounded
budget** (default ≤5 min; per-step exceptions in the delegation
table in [references/workflow.md](references/workflow.md)), so the
parent never blocks on long-running work and each step gets a clean
context. Sub-agents must summarize and return before their budget
expires; the parent extends via `write_agent` when needed. Full
procedure, per-step budgets, return contracts, and the extension
protocol live in [references/workflow.md](references/workflow.md).

```
Request review → Wait for review (sub-agent) → List + categorize open
threads → Triage (sub-agent) → Fix (sub-agents, parallel) → Build/test
per the repo's own conventions → Commit + push → Reply + resolve
(citing pushed SHA) → Convergence check → Cleanup outdated (final, once)
```

**Build, test, and lint commands are NOT prescribed here.** Each
step that runs them defers to the target repo's own conventions —
`CONTRIBUTING.md`, `AGENTS.md`, `README`, `package.json`/`Makefile`/
language-specific tooling, or whatever the repo uses. The skill's job
is the review loop; the repo's job is to tell us how it's built.

Convergence is computed by [scripts/02-check-review-status.ps1](scripts/02-check-review-status.ps1)
as a single `Converged: true` boolean. Do **not** call `task_complete`
until it returns true; print the proof (`HeadOid`,
`LatestCopilotReview.commitOid`, `submittedAt`) in the completion
message.

## Gotchas

- **NEVER post `@copilot please review` (or any `@copilot` mention)
  as a PR comment** to trigger a code review. That summons the Copilot
  **Coding Agent** (which makes commits), not the reviewer bot, and
  will not produce a review. Use [scripts/01-request-review.ps1](scripts/01-request-review.ps1)
  (GraphQL `requestReviewsByLogin`); if it can't land the trigger,
  push a substantive commit (auto-assign on `synchronize` is the most
  reliable fallback) — never fall back to `@`-mentions.
- **HTTP 200 / exit 0 from the trigger call is NOT proof Copilot
  accepted it.** The server can silently drop a request (quiet-period
  after dismissal, trivial-diff suppression, repo without Copilot
  enabled). The authoritative signal is a `copilot_work_started`
  event in the issue timeline newer than your request.
  `01-request-review.ps1` enforces this via event-`id` comparison —
  don't weaken it.
- **A "no new comments" review is necessary but not sufficient for
  convergence.** It must ALSO be at the current `HEAD` SHA and the
  open-threads list must be empty. A stale review on an earlier
  commit lets a regression slip through unreviewed.
  `02-check-review-status.ps1`'s `Converged` flag enforces all three.
- **Reply *and* resolve every open thread, including declines and
  outdated ones.** Resolving without a reply leaves no record of why
  the issue was considered addressed; replying without resolving
  keeps the open-threads list non-empty and blocks convergence.
- **Copilot threads are loop-owned; human / advanced-security /
  other-bot
  threads default to escalate-to-user.** Auto-replying or auto-
  resolving a human review thread can hide unaddressed concerns and
  is socially wrong. The triage rubric explicitly distinguishes
  reviewer types.
- **One focused commit per round, not one per PR.** Bundling rounds
  destroys the audit trail of which finding drove which change and
  breaks `git bisect`.
- **Build/test/lint with the repo's own commands** (per its
  `CONTRIBUTING`/`AGENTS`/`README`) before pushing a fix. A broken
  build wastes the next full review cycle (3–10 minutes).
- **Research the repo's own docs before generating any fix, build,
  or test command.** Read `.github/instructions/*.md` (often with
  `applyTo` globs pinning them to specific files), `.github/skills/`,
  `AGENTS.md`, `CONTRIBUTING.md`, and recent commits to similar
  files. Fan out multiple sub-agents in parallel when several axes
  need checking. Never invent generic answers that contradict repo
  practice — that's the "elephant in school" anti-pattern.
- **Don't poll the review state faster than ~3 minutes.** There is
  no progress signal; faster polling only wastes API budget.
- **Respect repo-specific spell-check / lint / format policies.**
  Some repos prefer rewording over allowlist entries; some have a
  patterns/regex file; some accept inline-ignore directives. Inspect
  the repo's existing config and recent commits before applying a
  generic Copilot suggestion.
- **Push back with written rationale** when a Copilot finding would
  over-engineer the design for a hypothetical edge case. Auto-accepting
  every suggestion erodes the design.
- **Scripting traps** (`gh api graphql -F` type-coercion, `git stash
  push -m` positional parsing, the three GraphQL traps for the
  reviewer mutation) are documented in
  [references/api-quirks.md](references/api-quirks.md). Read before
  modifying any script.

## Troubleshooting

| Issue | Solution |
|-------|----------|
| Script throws `prerequisite missing — gh CLI is not on PATH` | Install `gh` (`winget install GitHub.cli` on Windows; `brew install gh` on macOS; package manager on Linux; or download from https://cli.github.com). Then `gh auth login`. Surface the message to the user and STOP the loop — do not retry. |
| Script throws `prerequisite missing — gh CLI is not authenticated` | Run `gh auth login`. STOP the loop until the user completes auth. |
| Trigger fails or no `copilot_work_started` event lands | Push a substantive (non-whitespace) commit — auto-assign on `synchronize` is the most reliable trigger. Persistent failure indicates Copilot Code Review may not be enabled on the repo / account (check repo Settings → Code & automation → Copilot, or account-level Copilot Pro/Pro+). |
| No new review after waiting ~10 min | Quiet-period after recent dismissal or trivial-diff suppression. Push a substantive commit and retry. Do not blindly re-run `01-request-review.ps1` — it reports `InFlight` while Copilot is still a requested reviewer. |
| Outdated-but-unresolved threads in the open list | Expected: unresolved state is the source of truth. Reply + resolve them like any other open thread. `10-cleanup-outdated.ps1` is only a final safety net. |
| Unsure whether to fix or decline a finding | See [references/03-triage-criteria.md](references/03-triage-criteria.md). |
| Need a reply phrasing for "fixed", "declined", or "drift" | See [references/06-reply-templates.md](references/06-reply-templates.md). |

## References

- [references/workflow.md](references/workflow.md) — ten-step
  procedure with per-step sub-agent budgets, return contracts, and
  the extension protocol.
- [references/03-triage-criteria.md](references/03-triage-criteria.md) —
  fix-vs-decline decision rubric.
- [references/api-quirks.md](references/api-quirks.md) — verified
  GitHub API behavior, dead-ends, and the GraphQL traps for the
  reviewer mutation.
- [references/06-reply-templates.md](references/06-reply-templates.md) —
  reply patterns for accepted fixes, declined-with-rationale
  findings, and description-update acknowledgements.
- [scripts/_lib.ps1](scripts/_lib.ps1) — shared helpers (`Invoke-Gh`,
  `Invoke-GhGraphQL`, `Resolve-RepoCoords`); dot-sourced by every
  script.
- [scripts/01-request-review.ps1](scripts/01-request-review.ps1) —
  trigger Copilot review and verify pickup via the
  `copilot_work_started` event.
- [scripts/02-check-review-status.ps1](scripts/02-check-review-status.ps1) —
  single-shot snapshot of the PR's Copilot review state; emits
  `Converged: true` only when all three conditions hold.
- [scripts/03-list-open-threads.ps1](scripts/03-list-open-threads.ps1) —
  every unresolved PR review thread from **all reviewers** (Copilot,
  humans, github-advanced-security, etc.).
- [scripts/08-reply-and-resolve.ps1](scripts/08-reply-and-resolve.ps1) —
  post a reply and resolve in one call.
- [scripts/10-cleanup-outdated.ps1](scripts/10-cleanup-outdated.ps1) —
  safety net for outdated Copilot threads.
