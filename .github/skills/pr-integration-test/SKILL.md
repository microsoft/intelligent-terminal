---
name: pr-integration-test
description: 'Design, implement, and validate Intelligent Terminal integration tests for a target pull request or regression. Use when asked to add PR integration tests, convert a bug fix into E2E coverage, prove existing behavior still works, map tests to the release checklist, or verify E2E reports mark checklist cases complete.'
---

# PR Integration Test

Turn a target PR into durable, behavior-focused integration coverage that proves
the fixed path, protects existing behavior, and updates the generated release
checklist.

## When to Use This Skill

- Add integration or E2E tests for a specific PR.
- Add follow-up regression coverage after a fix has merged.
- Prove a bug from an issue cannot recur across real component boundaries.
- Extend `doc/release-check-list.md` and make report scripts check the new rows.
- Audit whether a PR's existing tests cover the user-visible behavior rather
  than only its implementation details.

## Prerequisites

- Work on a feature branch, never directly on `main` or `master`.
- Use `gh` to inspect the target PR, linked issues, review discussion, and state.
- Read repository and area-specific instructions before editing.
- Read `test/e2e/README.md` and reuse the ItE2E framework instead of creating a
  parallel harness.
- Run `pwsh -File test/e2e/bootstrap.ps1 -Check` before live E2E validation.

## Workflow

Follow [workflow.md](./references/workflow.md). Track the phases with a TODO
list because PR analysis, test design, checklist wiring, live validation, and
delivery must all complete.

1. Resolve the target PR and select the correct base commit and branch strategy.
2. Reconstruct the user-visible behavior and the complete component path.
3. Audit existing unit, integration, E2E, and release-checklist coverage.
4. Build a positive/negative/regression behavior matrix before writing tests.
5. Implement the smallest deterministic integration suite using existing ItE2E
   primitives.
6. Map each new release-signoff behavior to a stable checklist item.
7. Run the new suite, related existing regressions, and release-report scripts.
8. Commit, push, and prepare a PR whose evidence names passes, skips, checklist
   IDs, and the exact build tested.

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

An integration test is justified only when it proves a boundary that lower-level
tests do not. Keep focused unit tests for branch logic; use E2E for wiring,
packaging, process boundaries, protocol events, persistence, and real UI
interaction.

## Oracle Priority

Prefer the earliest deterministic product-owned signal:

1. Protocol/event stream
2. Persisted or queryable application state
3. Structured diagnostic log
4. Rendered terminal or UI state
5. LLM-generated text

Use an LLM output or AI judge only when usefulness or semantic correctness is
the behavior under test. Do not make routing, trigger, suppression, or
single-flight regressions depend on model wording.

## Release Checklist Contract

- Add one unchecked `[E2E]` checklist item per independently releasable behavior.
- Give each item a concise bold title that appears verbatim in the matching
  Pester full name (`Describe.Context.It`).
- Prefer exact-title matching. Add `test/e2e/release-coverage-map.psd1` entries
  only when an exact test name would be misleading or one case intentionally
  covers multiple checklist items.
- Run `pwsh -File test/e2e/Set-ChecklistIds.ps1`; never assign or renumber
  stable checklist IDs manually.
- Run the suite through the E2E report driver, not only `Invoke-Pester`, and
  inspect `release-report.md` to prove every new ID is `[x]`.
- Verify `Update-ReleaseReport.ps1` too when the suite is expected to support
  partial release-signoff runs.

## Completion Gate

Do not call the work complete until all of these are true:

- The original regression fails on the old behavior or is otherwise tied to a
  verified pre-fix symptom.
- The fixed path passes through the real integration boundary.
- False-positive and replay/idempotency risks are covered where applicable.
- Related preexisting suites pass, or every skip/failure is explicitly
  classified as environment, model variance, product regression, or test bug.
- New checklist IDs become `[x]` through both applicable report paths.
- The deployed package or executable under test is proven to contain the target
  change; no stale artifact is being exercised.

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
- **Do not rely on fixed sleeps for positive completion.** Start listeners
  before actions and poll for a scoped event/state. Use a short bounded
  observation window only to prove that something does not happen.
- **Do not let unrelated panes or windows satisfy assertions.** Scope events by
  pane/session/tab/window identifiers whenever the protocol exposes them.
- **Do not trust a build command alone.** Confirm package selection, deployed
  version, co-located binaries, and runtime logs when stale artifacts are
  possible.
- **Do not add checklist text without validating report matching.** A passing
  Pester case that leaves its release row unchecked is incomplete coverage.

## References

- [Detailed PR-to-integration-test workflow](./references/workflow.md)
- [ItE2E framework](../../../test/e2e/README.md)
- [Release checklist](../../../doc/release-check-list.md)
