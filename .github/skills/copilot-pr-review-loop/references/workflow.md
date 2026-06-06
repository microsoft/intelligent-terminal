# Copilot PR Review Loop — Workflow

One round = ten steps. Loop steps 1–9 until convergence; run step 10 once
when done. The **parent agent coordinates**; every substantive step is
**delegated to a fresh sub-agent with a ≤5-minute budget**. Sub-agents
return summary-and-progress before the budget expires so the parent can
extend (via `write_agent`) or re-scope.

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
| Parent never blocks | step 1 trigger, step 4 commit, step 5 push, step 6 reply+resolve mutations, and the `task_complete` decision stay in the parent |

## Sub-agent delegation map

Canonical order per round: **request → wait → list → triage → fix →
build → commit + push → reply + resolve (citing pushed SHA) →
convergence check**. Reply/resolve runs AFTER push so replies can cite
the pushed commit SHA.

| Step | Agent type | Budget | Returns | Notes |
|------|------------|--------|---------|-------|
| 1 — Request review | _(parent)_ | n/a | `01-request-review.ps1` JSON; record `WorkStartedAt` and the pre-trigger `LatestCopilotReview.submittedAt` as **baselines** for step 2 | — |
| 2 — Wait for review | `general-purpose` | **20 min hard cap**, poll every ~3 min | `02-check-review-status.ps1` JSON + recommendation (`ready` \| `give-up-push-commit`); `ready` iff `LatestCopilotReview.submittedAt > baseline` AND `ReviewAtHead: true` | one bounded sub-agent, not extension-driven; on `give-up-push-commit`, parent falls back to a substantive commit |
| 3 — List + categorize open threads | `explore` | 5 min | table of `{thread_id, file, line, author, severity, summary}` from `02-list-open-threads.ps1` | classify each row's `author` as `copilot` vs `human-or-bot` so triage can apply the correct policy |
| 4 — Triage | `general-purpose` | 5 min per ≤5 threads (parent batches if more) | table of `{thread_id, fix \| decline \| escalate-to-user, one-line rationale}` per [03-triage-criteria.md](03-triage-criteria.md) | human / GHAS threads default to `escalate-to-user` unless the user explicitly scoped them in |
| 5 — Apply fix (one per finding, parallel **max 5 concurrent**) | `general-purpose` | 5 min each | `{files_touched, one-line summary, status}` | parent merges and reconciles before step 6 |
| 6 — Build + test per repo conventions | `task` | 10 min | pass/fail + failure excerpts; sub-agent first identifies the repo's build/test commands (`CONTRIBUTING.md`/`AGENTS.md`/`README`/`package.json`/`Makefile`/etc.) then runs them on the changed code | — |
| 7 — Commit + push | _(parent)_ | n/a | parent runs `git commit` + `git push` directly | one focused commit per round; record the pushed SHA |
| 8 — Draft + post replies | `general-purpose` drafts → _(parent)_ posts | draft 5 min | sub-agent returns `{thread_id, reply_body}` per open thread citing the pushed SHA; parent then runs `06-reply-and-resolve.ps1` for each | reply+resolve are mutations; the parent owns mutations |
| 9 — Convergence verify | `explore` | 3 min | `02-check-review-status.ps1` JSON + independent HEAD-vs-`LatestCopilotReview.commitOid` sanity check | converged iff `Converged: true`; otherwise loop back to step 1 |
| 10 — Cleanup outdated (once after convergence) | _(parent)_ | n/a | `09-cleanup-outdated.ps1` | safety net only |

When the cap is reached and the work is still `partial`, the parent
narrows the input (batch smaller in step 4 / split fix scope in step 5)
or takes the step over itself.

## Per-round checklist

Command snippets assume your current directory is the skill root.

- [ ] **1.** **Request review (parent):**
  `pwsh ./scripts/01-request-review.ps1 -PrNumber <n>`. Returns JSON
  immediately. Before this call, capture
  `baseline_submitted_at = <current LatestCopilotReview.submittedAt or null>`
  via `02-check-review-status.ps1` — step 2 uses this baseline to
  distinguish the new review from any pre-existing one.

- [ ] **2.** **Wait for review (sub-agent, one bounded run, 20-min
  hard cap, polls every ~3 min):** dispatch a `general-purpose`
  sub-agent. The sub-agent polls
  `pwsh ./scripts/02-check-review-status.ps1 -PrNumber <n>` and
  returns `ready` ONLY when both
  `LatestCopilotReview.submittedAt > baseline_submitted_at` AND
  `ReviewAtHead: true`. On budget exhaustion, returns
  `give-up-push-commit`; parent then pushes a substantive commit
  (auto-assign on `synchronize` is the most reliable fallback).
  A single 20-min run is deliberately preferred over 5-min
  extensions — the parent has no useful work during a passive wait.

- [ ] **3.** **List + categorize open threads (sub-agent, 5-min
  budget):** `pwsh ./scripts/02-list-open-threads.ps1 -PrNumber <n>`
  emits every unresolved thread from every reviewer. Sub-agent
  classifies each row's `author` as `copilot` (loop-owned) vs
  `human-or-other-bot` (default `escalate-to-user` in triage unless
  the user explicitly scoped them in) and groups by file + severity.

- [ ] **4.** **Triage (sub-agent, 5-min budget per ≤5 threads —
  parent batches if more):** apply the rubric in
  [03-triage-criteria.md](03-triage-criteria.md); return
  `{thread_id, fix | decline | escalate-to-user, one-line rationale}`
  per thread.

- [ ] **5.** **Apply fixes (sub-agents, parallel — max 5 concurrent,
  5-min budget each):** one sub-agent per independent fix. Parent
  collects, reconciles file conflicts, and continues to step 6.

- [ ] **6.** **Build + test per the repo's conventions (sub-agent,
  10-min budget):** the sub-agent FIRST discovers the repo's
  build/test commands (`CONTRIBUTING.md`, `AGENTS.md`, `README`,
  `package.json` scripts, `Makefile`, etc.), THEN runs them on the
  changed code. Returns pass/fail + failure excerpts. Never push a
  fix you haven't built and tested with the repo's own commands.

- [ ] **7.** **Commit + push (parent):** one focused commit per
  round. Include the
  `Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>`
  trailer when the fix came from a Copilot finding. Record the
  pushed commit SHA — step 8 cites it.

- [ ] **8.** **Reply + resolve, citing the pushed SHA (sub-agent
  drafts, parent posts):** sub-agent drafts a reply per open thread
  using [06-reply-templates.md](06-reply-templates.md) and returns
  `{thread_id, reply_body}` pairs that quote the step-7 SHA. Parent
  then runs
  `pwsh ./scripts/06-reply-and-resolve.ps1 -ThreadId <id> -Body <text>`
  for each. Reply + resolve are mutations — the parent owns mutations.

- [ ] **9.** **Convergence check (sub-agent, 3-min budget):**
  `pwsh ./scripts/02-check-review-status.ps1 -PrNumber <n>` is
  converged iff its JSON shows `Converged: true` (=
  `ReviewAtHead && NoNewComments && OpenThreadCount == 0`). The
  sub-agent re-runs the snapshot AND independently re-queries HEAD
  vs. `LatestCopilotReview.commitOid` as a sanity check. If
  converged → step 10. Otherwise loop back to step 1.

- [ ] **10.** **(Once, after convergence) Cleanup outdated (parent):**
  `pwsh ./scripts/09-cleanup-outdated.ps1 -PrNumber <n>` — safety
  net for stale `isOutdated: true` Copilot threads. Most loops
  converge with nothing to clean.

Print the proof of convergence (HEAD SHA,
`LatestCopilotReview.commitOid`, `submittedAt`,
`OpenThreadCount: 0`) in your `task_complete` message. Proof, not
assertion.

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
