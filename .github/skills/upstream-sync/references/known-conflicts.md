# Known Conflicts (Tier-0 auto-resolution list)

This file is the **source of truth** for paths that always conflict the
same way and have a known correct resolution. The Tier-0 step in the
cherry-pick loop parses this file and auto-resolves matching paths.

## Format

Each entry is an H2 path heading, a one-line `Strategy:` line, and a
"Why" paragraph. The parser reads the heading text as the literal path.
Implemented Tier-0 strategies are `take-upstream` and `take-ours`;
`union` is reserved and currently escalates instead of auto-resolving.

---

## `.github/workflows/spelling2.yml`

**Strategy:** `take-upstream`

**Why:** Upstream maintains this workflow and updates it frequently with
new excludes/patterns. The fork has never had a reason to diverge from
upstream's version — every prior conflict on this file was resolved by
taking upstream wholesale. Verified on sister repo `agentic-terminal`.

---

## How to add a new entry

When the cherry-pick loop hits a Tier-3 conflict on a file that, on
inspection, has the same shape every time (e.g., a generated file, a
workflow upstream owns, a config we copy verbatim), add it here **after**
the manual resolution PR merges. Format:

```markdown
## `<exact-repo-relative-path>`

**Strategy:** `take-upstream`   <!-- or take-ours -->

**Why:** <one paragraph: what's in this file, why the strategy is safe,
when it would stop being safe.>
```

Then re-run the next sync — the previously-stuck file should now
auto-resolve under Tier 0.

## What does NOT belong here

- Files that conflict but where the correct resolution **depends on the
  commit** (those need Tier-2 or human judgement, not a fixed rule).
- Fork-only files (`tools/wta/**`, `src/cascadia/TerminalProtocol/**`)
  — these shouldn't conflict at all; if they do, the cause is structural
  and a Tier-0 entry would mask it.
- Files where "take upstream" would lose a fork-specific feature.
  Re-check by diffing fork-`HEAD` vs upstream for that file before adding.
