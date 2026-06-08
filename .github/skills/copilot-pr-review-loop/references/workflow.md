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

## Per-round checklist

Command snippets assume your current directory is the skill root.

- [ ] **1.** **Request review (parent):**
  FIRST call `pwsh ./scripts/02-check-review-status.ps1 -PrNumber <n>`
  and capture `baseline_submitted_at = LatestCopilotReview.submittedAt`
  (may be null) AND read `CopilotPending`.
  - If `CopilotPending: true`, skip the trigger — Copilot is already
    reviewing; go to step 2 with this baseline.
  - Otherwise call `pwsh ./scripts/01-request-review.ps1 -PrNumber <n>`
    to trigger and verify via the `copilot_work_started` event.
  Both paths end with the same baseline that step 2 uses to
  distinguish the new review from any preexisting one.

  **Parsing tip**: pipe the snapshot through
  `ConvertFrom-Json -DateKind String` (PS 7.3+) so `submittedAt`
  stays an ISO-8601 string. The default `ConvertFrom-Json` re-binds
  ISO timestamps to `[datetime]` and string interpolation on those
  renders PowerShell's local culture (e.g. `06/08/2026 02:02:44`),
  which silently breaks the lexicographic baseline comparison the
  wait sub-agent does in step 2.

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
  budget):** `pwsh ./scripts/03-list-open-threads.ps1 -PrNumber <n>`
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
  5-min budget each):** one sub-agent per independent fix. Each fix
  sub-agent MUST first research the repo's own conventions for the
  area it's editing — read `.github/instructions/*.md` files matching
  the changed file's `applyTo` glob, `.github/skills/` for any
  relevant skill, `AGENTS.md` / `CONTRIBUTING.md`, and the patterns
  in neighboring files. If a fix would contradict repo practice,
  push back in the reply instead of forcing it. Parent collects,
  reconciles file conflicts, and continues to step 6.

- [ ] **6.** **Build + test per the repo's conventions (1+
  sub-agents, 10-min budget total):** **research first, run second.**
  Discovery sub-agents fan out (parallel `explore`) across the axes
  the change touches — build tool, test runner, lint, format, spell-
  check, license-header CI, etc. — by reading
  `.github/instructions/*.md`, `AGENTS.md`, `CONTRIBUTING.md`,
  `README`, `package.json` scripts, `Makefile`, language tooling,
  and recent CI workflow runs. The build/test execution sub-agent
  then runs exactly the discovered commands on the changed code and
  returns pass/fail + failure excerpts. Never invent generic build
  or test commands — if discovery turns up no convention, surface
  that explicitly rather than guessing.

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
  `pwsh ./scripts/08-reply-and-resolve.ps1 -ThreadId <id> -Body <text>`
  for each. Reply + resolve are mutations — the parent owns mutations.

- [ ] **9.** **Convergence check (sub-agent, 3-min budget):**
  `pwsh ./scripts/02-check-review-status.ps1 -PrNumber <n>` is
  converged iff its JSON shows `Converged: true` (=
  `ReviewAtHead && NoNewComments && OpenThreadCount == 0`). The
  sub-agent re-runs the snapshot AND independently re-queries HEAD
  vs. `LatestCopilotReview.commitOid` as a sanity check. If
  converged → step 10. Otherwise, loop back to step 1.

- [ ] **10.** **(Once, after convergence) Cleanup outdated (parent):**
  `pwsh ./scripts/10-cleanup-outdated.ps1 -PrNumber <n>` — safety
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
