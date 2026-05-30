# Triage Criteria

Decision rubric for whether to fix or decline each Copilot finding. The
goal is correctness, not appeasement — decline confidently when warranted.

## The "ROI vs Risk" Frame

Before fixing anything, score the proposed change on two axes:

- **ROI** = value of the fix MINUS the cost to implement it MINUS the
  user's cost to review it.
- **Risk** = blast radius, reversibility, blast effect on unrelated code.

Then:

| ROI | Risk | Action |
|---|---|---|
| Clear positive | Low | Fix unilaterally. |
| Marginal | Low | Fix if cheap; otherwise decline with rationale. |
| Clear positive | High / Irreversible | Propose to the user before acting. |
| Marginal | High | Decline. |
| Negative (over-engineering for hypothetical) | Any | Decline. |

## Fix When the Finding Is...

- A **real correctness bug**:
  - Use-after-free / lifetime violation (especially around coroutines or
    callbacks).
  - Race condition that can drop user intent or corrupt shared state.
  - Gating logic that skips legitimate transitions (e.g. default-value
    edges, explicit-set-to-current-value transitions).
  - Missing link dependency or include that the project doesn't otherwise
    declare.
  - Off-by-one, null dereference, unhandled error path.
- A **cross-cutting concern with a clean, local fix**:
  - Moving a mutex one scope up so all callers share it.
  - Pulling a duplicated check into a helper.
- A **documentation / test-plan drift**:
  - The PR description claims behavior that the code no longer matches.
  - Comment block describes behavior different from the function body.
  - Test plan checkbox describes the opposite of the implemented
    semantics.

## Decline When the Finding Is...

- A **purely hypothetical race** requiring cross-class plumbing to fix,
  where the actual exposure is negligible and the fix would significantly
  complicate the design. Document the actual interleaving you ruled out.
- **Style, naming, or formatting**. Copilot is typically configured not to
  raise these, but sometimes does. They are out of scope for a code-review
  loop.
- **Suggestions to add abstractions** ("introduce a strategy pattern",
  "extract an interface") that don't pay for themselves at the current
  scale of the codebase.
- **Suggestions that contradict an established project convention** — even
  if the suggestion would be reasonable in isolation, consistency with the
  surrounding code is usually more valuable.
- **Microoptimizations** in code that is not on a hot path.

## Always State Reasoning

Whether you fix or decline, the reply must articulate WHY. This:

- Makes the PR self-documenting for future maintainers.
- Gives the next Copilot review visible context — declining with strong
  reasoning typically prevents Copilot from re-raising the same point.
- Forces you to be honest with yourself about whether a finding is real.
  If you can't write a defensible rationale, fix it.

## When to Stop and Ask the User

The default in autopilot is to decide and proceed. Escalate to the user
only when:

- The finding identifies a **design-level tradeoff** with multiple
  reasonable resolutions and no clear winner.
- The fix would be **large or cross-cutting** (hundreds of lines, new
  architecture, refactor across modules).
- The action is **high-risk or irreversible** (force-push that rewrites
  history, deletion of files, credential rotation, production touch).

Otherwise, decide. Repeated confirmation prompts when the answer is
already clear add no value and slow the loop down.

## Sanity Check Before Pushing

Before committing your decision for a round, briefly answer:

1. Did I just accept a finding I would have pushed back on if a human
   colleague had raised it? (If yes, reconsider.)
2. Did I just decline a finding that turns out to point at a real bug?
   (Re-read the affected code; if uncertain, fix.)
3. Will the next reviewer (human or bot) understand my reply without
   reading the whole thread?
