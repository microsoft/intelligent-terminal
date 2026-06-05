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
- **Micro-optimizations** in code that is not on a hot path.

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

## Project-specific policy hooks — check before deciding

Some findings are decided by **project policy**, not by general
correctness reasoning. Always check the repo's contributing docs and
existing config before applying a generic "fix" — what looks like an
obvious fix may violate a project rule.

Common cases:

- **Spell-check / dictionary findings**: many projects (including
  `microsoft/intelligent-terminal`) follow this priority order:
  1. **Reword the document** to use plain English when the finding is
     a genuine misspelling or an avoidable jargon term — this is the
     preferred fix.
  2. **Add a regex to `.github/actions/spelling/patterns/patterns.txt`**
     for OS API constants and code identifier *families* (e.g.
     `\bLOCALE_[A-Z][A-Z0-9_]+\b`). Preferred over per-word entries
     because one regex covers all future identifiers in the family.
  3. **Add stable real words** (names, APIs, product terms that will
     recur) to the `allow/` files — these are dictionary supplements
     and persist for the life of the project.
  4. **Use `expect/expect.txt` ONLY for transient, one-off non-words**
     that no reasonable regex covers and that aren't real dictionary
     words. Per this repo's own `expect/README.md`: "These terms are
     things which temporarily exist in the project, but which aren't
     necessarily words." Growing `expect.txt` unboundedly with stable
     terms hides real typos and is the anti-pattern.

  Always check the project's spelling config READMEs and recent
  commits to `patterns.txt` / `allow/` / `expect.txt` before adding any
  entry, so your fix matches the project's convention.
- **Lint suppressions / inline ignore directives**: most projects only
  accept suppressions with an inline rationale comment. Bare
  suppressions get pushed back.
- **License headers / file boilerplate**: project-specific format,
  often enforced by CI. Copy the existing header from a neighbor file.
- **Test framework choices, mock libraries, formatting tool versions**:
  follow the convention of the surrounding test files; do not
  introduce a new framework just because Copilot suggested one.

The rule: when Copilot proposes a "standard" fix for a class of finding
the project already has a policy for, follow the project's policy, not
Copilot's generic suggestion. Cite the project's config file or
convention in your reply.

## Conflicting Comments — Break Oscillation Before It Becomes a Loop

A real failure mode (sometimes spanning rounds, sometimes between two
findings in the same round): you "fix" what comment A asked for in round
N, then in round N+1 a *new* comment B objects to that exact change and
asks you to revert it. Or two findings in one round directly contradict
each other. Blindly flip-flopping ships oscillation and burns rounds.

**Detection**: before applying any fix, ask "does this directly undo or
contradict a change I made in a prior round of this PR?" If yes, the
comment is in conflict with a prior decision — do **not** treat it like
a fresh finding.

**Resolution rules** — apply in order, stop at the first that fires:

1. **Re-derive from principles, not from the latest comment.** Look at
   what the *code itself* should do given the function's contract, the
   surrounding patterns, and the user's stated preferences. Pick the
   side that wins on first principles.
2. **Prefer the position with the explicit rationale.** Whichever
   comment cites a concrete failure mode (security, correctness, data
   loss, perf) wins over the one that's stylistic or aesthetic.
3. **Prefer user/human comments over bot comments** when they directly
   conflict. The user's PR review is authoritative; bot feedback is
   advisory.
4. **If still ambiguous, escalate to the user** with both positions
   summarized side-by-side and your recommendation. Do not silently
   pick one and proceed — that's how loops start.

**When you decline the second comment**, your reply must reference the
prior round and explain why the existing form is correct. Example:
*"Declining — round 3 (commit `abc1234`) intentionally moved this to X
for reason Y; reverting would re-introduce <concrete bug>. Open to
alternative approaches that address Y differently."* This gives the
next reviewer (human or bot) the context to not raise it a third time.

**Hard stop**: if you find yourself about to make the same edit you
*reverted* in an earlier round of this PR, **stop and escalate**. That
is the unambiguous signature of an oscillation loop, and no amount of
auto-reasoning will resolve it without human input.

## Sanity Check Before Pushing

Before committing your decision for a round, briefly answer:

1. Did I just accept a finding I would have pushed back on if a human
   colleague had raised it? (If yes, reconsider.)
2. Did I just decline a finding that turns out to point at a real bug?
   (Re-read the affected code; if uncertain, fix.)
3. Will the next reviewer (human or bot) understand my reply without
   reading the whole thread?
