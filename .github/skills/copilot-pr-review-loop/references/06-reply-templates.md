# Reply Templates

Patterns for the reply you post on each Copilot review thread before
resolving it. The reply has to do real work — it documents the decision
for future maintainers and shapes what the next Copilot review will
surface.

## Template: Accepted Fix

```
<one sentence acknowledging what the finding was>.
<one or two sentences describing the fix>.
Fixed in <commit-sha>.
```

Example:

> The mutex did not cover the install side of the path, so two parallel
> writers could read the same baseline and clobber each other. Promoted
> the per-instance mutex to a process-wide function-local static so all
> read-modify-write paths share it. Fixed in abc1234.

## Template: Accepted Fix With Test Confirmation

When the fix is in a unit-tested area, mention that the suite still
passes — this is high signal for the next review.

```
<acknowledgement>. <fix description>. All <N> <test-suite> tests still
pass. Fixed in <commit-sha>.
```

Example:

> Replaced `CoCreateGuid`/`StringFromGUID2` with a PID + tick + atomic
> counter so the test project does not take an implicit `ole32.lib`
> dependency. All 42 tests in the affected suite still pass. Fixed in
> abc1234.

## Template: Declined With Rationale

The reply must explain WHY declining is the right call — not just that
you considered it. Always resolve the thread after replying; an open
thread with no reply signals avoidance.

```
Considered this, but declining: <concrete reason rooted in code or
design>. <Optional: describe the interleaving / scenario you ruled out,
or the alternative cost>. Happy to revisit if <specific trigger>.
```

Example:

> Considered extending the mutex into the FRE initialization path, but
> declining: FRE runs to completion before any settings reload can reach
> the reconciler, so the race window only opens once the FRE coroutine
> has returned and the page is fully up. Adding cross-class mutex sharing
> for that case costs more in coupling than the actual exposure
> justifies. Happy to revisit if we see a real interleaving in telemetry.

## Template: Documentation / Test-Plan Drift

When Copilot points out that the PR description, a comment, or the test
plan no longer matches the code:

```
Good catch — updated the <PR description | comment in <file> | test plan>
to match the implemented behavior: <one-line summary of the now-correct
statement>.
```

Example:

> Good catch — updated the PR description (both the Plan and the Test
> plan sections) to match the implemented behavior: the uninstall flow
> now strips orphan markers together with their recognizable body lines,
> rather than leaving them in place. Single expected recovery policy now.

## Template: Partial Fix With Followup Deferred

When the finding has both an immediate fix and a deeper structural
concern, address the immediate part now and acknowledge the rest.

```
Fixed the immediate <X> in <commit-sha>. The broader <Y> would benefit
from <larger change>, which I'd prefer to land separately because <reason>.
Tracking as <issue link / TODO / next-PR commitment>.
```

## Anti-Templates (Do Not Use)

❌ "Thanks!" / "Good point." with no substance. Reads as dismissal of the
   review.

❌ "Will fix later." Either fix it now or decline with rationale. Deferred
   fixes that aren't tracked anywhere get lost.

❌ Resolve-without-reply. The next reviewer cannot reconstruct why the
   thread was closed.

❌ "I disagree." with no reasoning. State the actual technical disagreement.

## Tone Guidance

- Be concrete: cite file paths, commit SHAs, scenarios, or function names.
- Be direct: avoid hedging ("perhaps", "might") when you actually have a
  position.
- Be brief: 2–4 sentences is typical. Long replies signal the change is
  too big and should probably have been broken up.
- Be honest: if a finding revealed something you missed, say so.
