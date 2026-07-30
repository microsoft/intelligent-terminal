---
name: pr-integration-test
description: 'Design, implement, and validate Intelligent Terminal integration tests for a target pull request or regression. Use when asked to add PR integration tests, convert a bug fix into E2E coverage, prove existing behavior still works, map tests to the release checklist, or verify E2E reports mark checklist cases complete.'
---

# PR Integration Test

Turn a target PR into durable, behavior-focused integration coverage that proves
the fixed path, protects existing behavior, and updates the generated release
checklist.

## When to Use This Skill

- Convert an open or merged PR into cross-component regression coverage.
- Extend `doc/release-check-list.md` and make report scripts check the new rows.
- Audit whether a PR's existing tests cover the user-visible behavior rather
  than only its implementation details.

## Prerequisites

- Read `test/e2e/README.md` and reuse the ItE2E framework instead of creating a
  parallel harness.
- Run `pwsh -File test/e2e/bootstrap.ps1 -Check` before live E2E validation.

## Workflow

Follow [workflow.md](./references/workflow.md). Track the phases with a TODO
list.

`analyze PR -> reconstruct behavior -> audit coverage -> design matrix -> write ItE2E -> wire checklist -> validate -> deliver`

## Test Design Standard

For every proposed case, record:

| Field | Required answer |
|-------|-----------------|
| Contract | What user-visible behavior must remain true? |
| Trigger | What exact action or input exercises it? |
| Boundary | Which real component handoff does this test add beyond unit tests? |
| Oracle | What deterministic observable proves success or suppression? |
| Negative control | What similar input must not trigger the behavior? |
| Existing protection | Which existing tests protect old behavior and must still run? |
| Checklist title | Which exact bold release-checklist title contains the Pester test name? |

## Oracle Priority

Prefer the earliest deterministic product-owned signal:

1. Protocol/event stream
2. Persisted or queryable application state
3. Structured diagnostic log
4. Rendered terminal or UI state
5. LLM-generated text

Use model output only when it is the behavior under test; never use it to prove
routing, triggers, suppression, or idempotency.

## Release Checklist Contract

- Add one unchecked `[E2E]` checklist item per independently releasable behavior.
- Give each item a concise bold title that appears verbatim in the matching
  Pester full name (`Describe.Context.It`).
- Prefer exact-title matching. Add `test/e2e/release-coverage-map.psd1` entries
  only when an exact test name would be misleading or one case intentionally
  covers multiple checklist items.
- Assign stable IDs and verify `[x]` output through the full and incremental
  report paths in [workflow.md](./references/workflow.md).

## Completion Gate

Do not call the work complete until all of these are true:

- Pre-fix evidence identifies the regression, and the fixed path crosses the
  real integration boundary.
- Relevant false positives, replay risks, and existing behavior are covered.
- The correct build passes related suites and marks new checklist IDs `[x]`;
  every skip is explained.

## Gotchas

- **Do not test only the implementation detail named in the PR.** Reconstruct
  the end-to-end user path and assert the observable contract.
- **Do not duplicate a unit test at E2E level.** Add the missing process,
  protocol, persistence, packaging, or UI boundary.
- **Do not use a successful lower-layer event as proof of the final feature.**
  When the contract is downstream, assert both the trigger and its downstream
  effect.
- **Do not turn product failures into skips.** Skip only when an external
  prerequisite is genuinely unavailable. A connected product that behaves
  incorrectly must fail.

## References

- [Detailed PR-to-integration-test workflow](./references/workflow.md)
- [ItE2E framework](../../../test/e2e/README.md)
- [Release checklist](../../../doc/release-check-list.md)
