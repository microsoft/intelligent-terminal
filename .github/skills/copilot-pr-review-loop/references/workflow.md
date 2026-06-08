# Copilot PR Review Loop — Workflow

Each **round** runs steps 1–9; **step 10** is a one-time cleanup
after convergence. The parent agent coordinates; every substantive
step is delegated to a fresh sub-agent with a bounded budget.
Sub-agents return summary-and-progress before the budget expires so
the parent can extend (via `write_agent`) or re-scope.

Build, test, and lint commands are NOT prescribed here. Every step that
needs them defers to the target repo's own conventions
(`CONTRIBUTING.md`, `AGENTS.md`, `README`, `package.json` /
`Makefile` / language tooling, etc.). Discover and follow the repo's
existing practice — never invent build commands.

## Time-boxing & extension protocol

| Concept | Rule |
|---------|------|
| Default budget | 5 minutes per sub-agent invocation |
| Sub-agent must return | `status` ∈ {`complete`, `partial`, `blocked`} + `next_action` + `needs_extension_minutes` (0 if none). Always summarize progress before the budget expires — never silently overrun. |
| Extension | parent only extends when `status: partial` AND `next_action` is concrete; sends `write_agent "continue for N min"` with `N = min(needs_extension_minutes, 10)` |
| Extension cap (default) | 2 extensions per step; step 6 (build/test) up to 2× for slow suites. Step 2 (wait) is a single bounded sub-agent — see step 2 — not extension-driven. |
| Parent never blocks | step 1 (request), step 7 (commit + push), step 8 reply/resolve mutations, and the `task_complete` decision stay in the parent |

## Sub-agent delegation map

Canonical order per round: **request → wait → list → triage → fix →
build → commit + push → reply + resolve (citing pushed SHA) →
convergence check**. Reply/resolve runs AFTER push so replies can cite
the pushed commit SHA.

| Step | Agent type | Budget | Returns | Notes |
|------|------------|--------|---------|-------|
| 1 — Request review | _(parent)_ | n/a | call `02-check-review-status.ps1` first; capture `LatestCopilotReview.submittedAt` as **baseline** for step 2; if `CopilotPending: true` skip the trigger and go to step 2 directly; otherwise run `01-request-review.ps1` (which returns its own `WorkStartedAt` for diagnostics). | `01-request-review.ps1` keeps its own InFlight short-circuit as a safety net, but the canonical "is Copilot pending?" signal lives in `02-check-review-status.ps1`. |
| 2 — Wait for review | `general-purpose` | **20 min hard cap**, poll every ~3 min | `02-check-review-status.ps1` JSON + recommendation (`ready` \| `give-up-push-commit`); `ready` iff `LatestCopilotReview.submittedAt > baseline` AND `ReviewAtHead: true` | one bounded sub-agent, not extension-driven; on `give-up-push-commit`, parent falls back to a substantive commit |
| 3 — List + categorize open threads | `explore` | 5 min | table of `{thread_id, file, line, author, severity, summary}` from `03-list-open-threads.ps1` | classify each row's `author` as `copilot` vs `human-or-bot` so triage can apply the correct policy |
| 4 — Triage | `general-purpose` | 5 min per ≤5 threads (parent batches if more) | table of `{thread_id, fix \| decline \| escalate-to-user, one-line rationale}` per [03-triage-criteria.md](03-triage-criteria.md) | human / advanced-security threads default to `escalate-to-user` unless the user explicitly scoped them in |
| 5 — Apply fix (one per finding, parallel **max 5 concurrent**) | `general-purpose` | 5 min each | `{files_touched, one-line summary, status}` | each fix sub-agent **first researches the repo's own conventions** for the area it's editing (`.github/instructions/*.md` matching the file's `applyTo` pattern, `.github/skills/`, `AGENTS.md`, `CONTRIBUTING.md`, neighbor-file patterns) — never invent a generic answer that contradicts repo practice. Parent merges and reconciles file conflicts before step 6; the 5-cap prevents fix-fanout chaos. If step 3 returned >5 findings, parent runs step 5 in waves of ≤5. |
| 6 — Build + test per repo conventions | `task` (may fan out to several `explore` sub-agents for discovery) | 10 min | pass/fail + failure excerpts; **discovery first** — read `.github/instructions/*.md`, `AGENTS.md`, `CONTRIBUTING.md`, `README.md`, `package.json` scripts, `Makefile`, language tooling, AND recent CI workflow runs to learn the *actual* command set in use; THEN run those exact commands on the changed code | independent discovery axes (build tool / test runner / lint / spelling / format) can run as separate `explore` sub-agents in parallel; cache discovered commands per round |
| 7 — Commit + push | _(parent)_ | n/a | parent runs `git commit` + `git push` directly | one focused commit per round; record the pushed SHA |
| 8 — Draft + post replies | `general-purpose` drafts → _(parent)_ posts | draft 5 min | sub-agent returns `{thread_id, reply_body}` per open thread citing the pushed SHA; parent then runs `08-reply-and-resolve.ps1` for each | reply+resolve are mutations; the parent owns mutations |
| 9 — Convergence verify | `explore` | 3 min | `02-check-review-status.ps1` JSON + independent HEAD-vs-`LatestCopilotReview.commitOid` sanity check | converged iff `Converged: true`; otherwise loop back to step 1 |
| 10 — Cleanup outdated (once after convergence) | _(parent)_ | n/a | `10-cleanup-outdated.ps1` | safety net only |

When the cap is reached and the work is still `partial`, the parent
narrows the input (batch smaller in step 4 / split fix scope in step 5)
or takes the step over itself.

## Per-round commands

Quick reference — see the delegation map above for the contract per
step. Command snippets assume cwd is the skill root.

| Step | Command | Notes |
|------|---------|-------|
| 1 | `pwsh ./scripts/02-check-review-status.ps1 -PrNumber <n> \| ConvertFrom-Json -DateKind String` to capture `baseline_submitted_at` + `CopilotPending`. If `CopilotPending: true` skip to step 2; else `pwsh ./scripts/01-request-review.ps1 -PrNumber <n>`. | `-DateKind String` (PS 7.3+) keeps `submittedAt` an ISO string so the lexicographic compare in step 2 works across the parent→sub-agent boundary. |
| 2 | Dispatch wait sub-agent — polls `02-check-review-status.ps1` every ~3 min; `ready` iff `submittedAt > baseline` AND `ReviewAtHead: true`. | Single bounded 20-min run. On `give-up-push-commit`, push a substantive commit (auto-assign on `synchronize` is the most reliable fallback). |
| 3 | `pwsh ./scripts/03-list-open-threads.ps1 -PrNumber <n>` | Classify each row's `author`; default human / advanced-security to `escalate-to-user`. |
| 4 | Triage sub-agent applies the rubric in [03-triage-criteria.md](03-triage-criteria.md). | Batch in waves of ≤5 threads per sub-agent. |
| 5 | Fix sub-agents, parallel, max 5 concurrent. | Each researches `.github/instructions/*.md` (matching `applyTo`), `.github/skills/`, `AGENTS.md`, `CONTRIBUTING.md`, neighbor files BEFORE writing the fix. |
| 6 | Build/test sub-agent: discover commands from the same set of repo docs + recent CI runs, then run them. | Never invent generic commands; surface the gap if discovery turns up nothing. |
| 7 | Parent: `git commit` + `git push`. | One focused commit per round; include `Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>`. Record the pushed SHA. |
| 8 | Drafting sub-agent returns `{thread_id, reply_body}` citing the step-7 SHA, using [06-reply-templates.md](06-reply-templates.md). Parent runs `pwsh ./scripts/08-reply-and-resolve.ps1 -ThreadId <id> -Body <text>` for each. | Reply+resolve are mutations; the parent owns mutations. |
| 9 | Convergence sub-agent: `pwsh ./scripts/02-check-review-status.ps1 -PrNumber <n>` — converged iff `Converged: true`. | Re-query HEAD vs. `LatestCopilotReview.commitOid` as an independent sanity check. |
| 10 | _(after convergence, once)_ `pwsh ./scripts/10-cleanup-outdated.ps1 -PrNumber <n>` | Safety net only; most loops converge with nothing to clean. |

Print the proof of convergence (`HeadOid`, `LatestCopilotReview.commitOid`,
`submittedAt`, `OpenThreadCount: 0`) in the `task_complete` message. Proof,
not assertion.

## Notes

- **Re-request is first-class.** `01-request-review.ps1` does not
  silently skip when Copilot has already reviewed; it issues the
  same mutation and verifies via a new `copilot_work_started` event.
- **HTTP / exit status alone is not sufficient.** GitHub can return
  HTTP 200 while silently dropping a re-review request. See
  [api-quirks.md](api-quirks.md).
- **Outdated threads still need reply + resolve.** They show up in
  the PR UI as unresolved until you explicitly close them; step 10
  is a safety net, not the primary mechanism.
- **Reopened / revisit requests reset the thread to step 4.** If a
  declined finding is reopened by the user (or by a follow-up
  Copilot review), pull it back into triage with the prior rationale
  as input rather than re-running the whole loop.
- **Resumability after interruption.** On restart, snapshot HEAD,
  the latest Copilot review's `commit.oid` + `submittedAt`, the
  open-threads list, and any uncommitted local changes. Discard
  cached triage / drafts if HEAD or the open-threads set changed.
- **Local-build patches.** For projects with uncommitted local-build
  patches held out of the PR: `git stash push -m "local-build" --
  <paths>` before committing, `git stash pop` after. Note `-m` must
  come BEFORE `--` (see [api-quirks.md](api-quirks.md)).
