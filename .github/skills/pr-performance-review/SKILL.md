---
name: pr-performance-review
description: 'Review and safely repair Intelligent Terminal PR performance regressions. Use for renderer, text-buffer, VT, UI-thread, tab/pane, WTA, async, session, logging, allocation, event-amplification, benchmark, profiling, responsiveness, memory, and CI-cost changes.'
---

# PR Performance Review

Perform repository-specific performance review and tightly constrained repair.

## Caller boundary

The caller owns PR context, immutable revisions, trusted checkout, fork policy,
allowed tools and edits, safe outputs, stale-head checks, report path, and
publication mechanics. This skill owns reusable scope classification, changed
path/caller analysis, evidence standards, severity, repair eligibility, report
validation, and deterministic rendering.

Use [`./scripts/performance-review.mjs`](./scripts/performance-review.mjs) for
classification, report validation, card rendering, and publication gating.

## Procedure

1. Read the caller-generated scope JSON. Stop if the caller classified the PR
   as non-applicable.
   Its `totals` give changed files, candidate files, lines added and deleted,
   and binary-file count; each file also carries its line counts. Use these
   inputs alongside caller depth and subsystem risk to assess review effort
   and whether more evidence is needed. Counts are context, not severity or
   proof. Binary-file line counts are unavailable, not zero.
   The engine starts with `auto`; do not assume you can change its model
   mid-run or claim that these counts control Auto's routing.
2. Verify immutable base/head identities, resolve the merge base, and inspect
   the complete exact patch. Treat file lists/statistics only as discovery.
3. Trace changed functions into callers and repeated lifecycle/event paths:
   renderer/TerminalCore, text buffer, VT parser/adapter, UI thread, tab/pane
   lifecycle, WTA startup/process pooling, Rust tasks/awaits/locks/channels,
   session/log enumeration, allocations/copies, and event amplification.
4. Inspect relevant existing evidence: `src/ConsolePerf.wprp`,
   `src/tools/ConsoleBench`, `test/e2e/Measure-PaneContext.ps1`,
   `build/scripts/Measure-AgentHookOverhead.ps1`, focused tests, and duration
   telemetry.
5. Separate application performance, responsiveness, memory growth, and CI
   runtime/cost. Separate microbenchmark, end-to-end, and profile evidence.
6. Write and validate the caller's version-1 report. Render its card
   deterministically. Request exactly the output allowed by the caller.

## Evidence and severity

- HIGH requires high confidence plus a measured regression or provable
  complexity, blocking, or resource-lifetime defect on a relevant repeated
  path.
- MEDIUM/LOW are suggestions with uncertainty and tradeoffs; never edit for
  them.
- One allocation, copy, lock, or async call is not a defect without
  frequency/lifetime/ordering evidence.
- Noisy measurements require at least three samples and reported spread.
- Unavailable Windows x64/ARM64 measurement is `unavailable`, never a pass.
- Failed validation is `error`/`blocked`; never report an unvalidated fix.

## Repair eligibility

Repair only when every condition holds:

1. severity HIGH and confidence high;
2. strong repository-specific proof;
3. one small, localized, behavior-preserving source change;
4. no broad threading, caching, architecture, dependency, generated-file,
   security-policy, or CI-policy change;
5. a supported native backend can run an existing focused test on the exact final tree;
6. the caller explicitly permits edits and branch push.

If any condition fails, keep the finding unresolved with disposition
`manual_required` or `unsafe`, and do not edit. A repair caller requests native
validation of a proposed patch, never direct publication. A guidance caller
stays read-only and emits at most one comment. **Never combine a PR comment and
branch commit in one worker.**

## Native repair handoff

The current supported backend is focused WTA Rust unit testing on Windows.
For a small HIGH WTA source repair, use `fixDisposition: proposed` and
`status: pending_validation`. Read the existing test's function and enclosing
module declarations, then set `validationPlan.type` to `wta-unit` and
`validationPlan.testFilter` to that actual qualified test name. Confirm the
name in the immutable source with Git before writing the report. Never copy a
sample selector or invent a module name. Cargo test names do not include
repository directories or the crate name; do not prefix `tools`, `wta`, or
`src`. Request `validate_performance_repair` with
`confirm: true` once, then stop editing.

Do not claim that Linux has run Windows tests. Do not commit, call a branch-push
tool, or mark a proposal `fixed`. Trusted post-processing captures the exact
candidate blobs; a read-only Windows job runs the fixed command and requires at
least one passing test. GitHub records the job result. The trusted publisher
checks that result and the original sealed blobs before committing with
immutable-head CAS. It never consumes a receipt or files written by test code.
Only the publisher can render a proposal as `fixed`.

Native C++ repairs currently lack a supported backend; keep them unresolved and
unedited with the relevant MSBuild/TAEF handoff. This limitation does not block
eligible WTA fixes and must not be hidden by substituting a Linux timing or
schema check.

## Report contract

Write the caller-provided report path with:

```json
{
  "version": 1,
  "review": "performance",
  "mode": "repair | guide",
  "identity": {
    "prNumber": 123,
    "baseSha": "40 lowercase hex",
    "headSha": "40 lowercase hex"
  },
  "status": "pass | advisory | action_required | pending_validation | blocked",
  "findings": [{
    "id": "PERF-STABLE-SOURCE-IDENTIFIER",
    "severity": "high | medium | low",
    "confidence": "high | medium | low",
    "dimension": "application-performance | responsiveness | memory-growth | ci-runtime-cost",
    "category": "rendering | text-buffer | vt-parsing | ui-thread | tab-pane-lifecycle | wta-runtime | session-log-enumeration | concurrency | other",
    "title": "short title",
    "affectedScenario": "specific user or CI scenario",
    "location": "path:line",
    "observed": "what the immutable head does",
    "expected": "baseline or required behavior",
    "impact": "evidenced consequence",
    "nativeEnvironment": {
      "architecture": "windows-x64 | windows-arm64 | not-measured",
      "details": "OS/build/tooling or why unavailable"
    },
    "evidence": [{
      "type": "source | measurement | complexity-proof | blocking-proof | resource-proof",
      "kind": "microbenchmark | end-to-end | profile",
      "noisy": false,
      "samples": 5,
      "spread": "p50/p95/range when noisy",
      "detail": "specific proof"
    }],
    "proposedFix": "small recommendation or applied change",
    "validation": "exact command/result or handoff",
    "fixDisposition": "proposed | manual_required | unsafe | advice_only"
  }],
  "validationPlan": {"type": "wta-unit"},
  "checks": [{
    "name": "check name",
    "status": "pass | regression | noisy | unavailable | error",
    "command": "exact command or read-only inspection",
    "exitCode": 0,
    "detail": "result and limitation"
  }]
}
```

Stable IDs start with `PERF-` and use a bounded source/defect identifier,
not array order or an arbitrary fixed character count. `exitCode` is `null`
when not run. `validationPlan` is required only for proposals.
The schema example deliberately omits `testFilter`: add that required field
using the actual test name you read, not a sample value.
Status `fixed` is reserved for the published card after native validation.
Use `pending_validation` for eligible HIGH proposals, `action_required` when unresolved HIGH remains,
`advisory` for only MEDIUM/LOW, `pass` for no findings, and `blocked` when a
check errors.

Run the validator and renderer before any safe output:

```powershell
pwsh -NoProfile -Command "node '.github/skills/pr-performance-review/scripts/performance-review.mjs' validate --report '<report>' --mode '<repair-or-guide>' --pr '<number>' --base '<sha>' --head '<sha>'"
pwsh -NoProfile -Command "node '.github/skills/pr-performance-review/scripts/performance-review.mjs' render --report '<report>' --mode '<repair-or-guide>' --pr '<number>' --base '<sha>' --head '<sha>'"
```

Use the permitted PowerShell route for report writes and checker execution.
If direct Node is denied, switch to that route immediately; do not try alternate
Node executable names or bypass validation.

## Gotchas

- Never execute fork-controlled scripts or binaries with model credentials.
- Never infer end-to-end improvement from a microbenchmark alone.
- Never use a Linux CI timing as native Terminal runtime proof.
- Never claim fixed until validation covers the final patch.
