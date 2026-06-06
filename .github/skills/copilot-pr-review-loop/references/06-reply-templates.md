# Reply Templates

Patterns for the reply you post on each review thread before
resolving it. The reply has to do real work — it documents the
decision for future maintainers and shapes what the next Copilot
review will surface.

Be **concrete** (cite file paths, commit SHAs, function names),
**direct** (no hedging when you have a position), and **brief** (2–4
sentences is typical). Long replies usually mean the round should
have been broken up.

## Accepted fix

```
<one sentence acknowledging the finding>.
<one or two sentences describing the fix>.
Fixed in <commit-sha>.
```

Example (language-neutral):

> The lock did not cover the install side of the path, so two
> parallel writers could read the same baseline and clobber each
> other. Promoted the per-instance lock to a process-wide
> function-local static so all read-modify-write paths share it.
> Fixed in abc1234.

When the fix is in a tested area, add a one-line test confirmation:

> Replaced the platform UUID dependency with a PID + monotonic-clock
> + atomic counter so the test target no longer pulls in the
> platform UUID library. All 42 tests in the affected suite still
> pass. Fixed in abc1234.

## Declined with rationale

The reply must explain WHY declining is the right call — not just
that you considered it. Always resolve the thread after replying;
an open thread with no reply signals avoidance.

```
Considered this, but declining: <concrete reason rooted in code or
design>. <Optional: the interleaving / scenario you ruled out, or
the alternative cost>. Happy to revisit if <specific trigger>.
```

Example (domain-neutral):

> Considered extending the lock into the initialization path, but
> declining: initialization runs to completion before any concurrent
> caller can reach this code, so the race window only opens after
> the init callback has returned. Sharing the lock across modules
> costs more in coupling than the actual exposure justifies. Happy
> to revisit if telemetry shows a real interleaving.

## Documentation / test-plan drift

When Copilot points out the PR description, a comment, or the test
plan no longer matches the code:

```
Good catch — updated the <PR description | comment in <file> | test
plan> to match the implemented behavior: <one-line summary of the
now-correct statement>.
```

## Partial fix with deferred follow-up

When the finding has both an immediate fix and a deeper structural
concern, address the immediate part now and acknowledge the rest:

```
Fixed the immediate <X> in <commit-sha>. The broader <Y> would
benefit from <larger change>, which I'd prefer to land separately
because <reason>. Tracking as <issue link / TODO / next-PR
commitment>.
```

## Anti-patterns — DO NOT use

- ❌ `"Thanks!"` / `"Good point."` with no substance.
- ❌ `"Will fix later."` Either fix it now or decline with rationale;
  deferred fixes that aren't tracked anywhere get lost.
- ❌ Resolve-without-reply. The next reviewer cannot reconstruct why
  the thread was closed.
- ❌ `"I disagree."` with no reasoning. State the actual technical
  disagreement.
