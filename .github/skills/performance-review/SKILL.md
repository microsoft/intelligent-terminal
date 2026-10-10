---
name: performance-review
description: 'Review Intelligent Terminal performance changes in local work, commit ranges, branches, or PRs. Use for renderer, text-buffer, VT, UI-thread, tab/pane, WTA, async, session, logging, allocation, event-amplification, benchmark, profiling, responsiveness, memory, and CI-cost changes.'
---

# Performance Review

Investigate how a change affects typing, rendering, tab switching, WTA startup,
session refresh and repeated work. Establish the affected scenario and cost
before recommending an optimization.

## Review context

Establish the baseline and reviewed changes from the caller's local diff,
commit range, branch comparison or PR. Record the exact snapshot for mutable
work so later edits cannot be mistaken for reviewed evidence.

Identify changed files, affected components and callers. File and line counts
help estimate effort; execution frequency, data growth and subsystem risk
determine which paths merit deeper investigation. Caller-supplied scope and
analyzer output are discovery inputs, not proof of complete coverage.

Review produces Markdown using the results template below. The caller owns
execution permissions, additional output formats, validation and delivery.
Review-only is the default; authorized corrections follow the repair criteria.

## Investigation

Inspect the complete patch and compare behavior with the baseline. Trace the
changed functions into repeated lifecycle and event paths: renderer and
TerminalCore, text buffer, VT parser/adapter, UI thread, tab/pane ownership,
WTA process pooling, tasks, locks, channels, session/log enumeration,
allocations, copies and notification amplification.

Apply each rule when its trigger appears in the changes or affected callers.
Record the rule ID with its evidence.

| Rule | Trigger | Investigation | Intentional cases |
| --- | --- | --- | --- |
| `PERF-REPEATED-WORK` | A loop, lookup, copy or enumeration changes on a repeated path. | Compare work as input grows; establish caller frequency and actual bounds. Inspect nested scans and collections used only for count or membership. | Fixed small bounds, one-time startup, required snapshots and amortized work. |
| `PERF-UI-BLOCKING` | A UI callback or dispatcher path gains synchronous work. | Trace the executing thread, I/O, waits and traversal through callees; identify the stalled interaction and its blocking cost. | Already-background work, bounded cheap callbacks and required thread affinity. |
| `PERF-MVVM-AMPLIFICATION` | A property, binding, collection update or subscription changes. | Trace notifications to consumers; count expensive refreshes per logical change, including bulk updates and reentrancy. | Required individual notifications, cheap consumers and existing batching. |
| `PERF-ASYNC-CONTENTION` | Lock scope, awaits, channels or synchronous work in a task changes. | Establish what survives suspension, who contends and how ordering affects progress; verify actual scopes. | Async-aware locks, released guards, intentional backpressure and non-suspending operations. |
| `PERF-OWNER-LIFETIME` | Tabs, panes, tasks, subscriptions, helpers or pools have different lifetimes. | Trace repeated creation through teardown and cancellation; identify retained work or memory beyond its intended owner. | Resident pools, pre-warmed helpers, stash/restore and deliberately longer-lived bounded tasks. |

## Analyzer and measurement evidence

Inspect each diagnostic in source before adopting it. Comparable results
identify revisions, tool versions, configuration, target and analyzed scope.
Separate new or worsened costs from unchanged baseline debt; moved lines alone
are not new defects. Shared headers or callers can worsen behavior outside
edited lines.

Available repository entrypoints include the opt-in C++
`PerformanceAnalysis=Extended` profile, `cargo wta-perf` and
`cargo wta-perf-extended`. Use their definitions in the build configuration
and Cargo aliases as the authority for checks and commands. Ordinary Cargo
builds do not run Clippy.

C++ project selection is provisional until evaluated MSBuild `ClCompile`
membership establishes translation-unit coverage. Successful prerequisite
builds are not additional analyzer coverage. Unmapped sources, shared callers,
headers and unsupported languages remain explicit coverage gaps.

Interpret warnings in context. `await_holding_lock` can warn after an explicit
drop; collections may preserve iterator or clone side effects; boxing a large
future trades stack size for allocation. Group membership and machine-fix
availability do not establish impact or behavioral safety.

Inspect existing evidence where relevant: `src/ConsolePerf.wprp`,
`src/tools/ConsoleBench`, `test/e2e/Measure-PaneContext.ps1`,
`build/scripts/Measure-AgentHookOverhead.ps1`, focused tests and duration
telemetry.

| Evidence | What it establishes |
| --- | --- |
| Source / complexity / blocking / lifetime proof | A concrete causal cost under stated execution and input bounds. |
| Microbenchmark | Cost of the measured operation, not end-to-end improvement. |
| End-to-end measurement | Impact on the measured user scenario and environment. |
| Profile | Where measured work or resource use accumulates. |
| Native test | Behavior covered by that test on the exact validated source. |
| Missing, partial or failed analysis | A coverage gap or blocker, not a clean result. |

Separate application performance, responsiveness, memory growth and CI
runtime/cost. Record the native OS, architecture, tooling and measurement
conditions. Noisy measurements need at least three samples and their spread;
cross-platform timings remain evidence for the platform actually measured.

## Triage and repair

Rank findings by demonstrated impact, frequency or growth, and confidence in
causality. Collapse duplicate warnings for the same underlying defect.

| Result | Disposition |
| --- | --- |
| HIGH, high confidence, measured regression or proven complexity/blocking/lifetime defect on a relevant path | Propose the smallest behavior-preserving correction. |
| Important defect with redesign, ordering/lifetime uncertainty or unsupported validation | Manual handoff with evidence and missing proof. |
| Plausible cost with uncertain impact or frequency | MEDIUM/LOW advice with uncertainty and tradeoffs. |
| Unrelated baseline debt, negligible bounded cost, intentional behavior or false positive | Explain the exclusion in check evidence. |

A source correction is eligible when it is explicitly authorized, HIGH and
high-confidence, small and localized, behavior-preserving, and supported by
repository-specific proof and an existing focused native test. Required input
analysis must be complete and comparable. Broad threading, caching, architecture,
dependency or policy changes require separate design work.

Preserve existing tests and intended semantics. Analyzer replacements need the
same proof as hand-written changes: a reference parameter can affect ABI or
coroutine lifetime, and reserved capacity can increase retained memory.
Insufficient evidence or validation results in an unresolved handoff rather
than a broader edit. Report source changes and validation separately so a
proposal cannot be mistaken for a verified improvement.

## Results template

Keep the opening summary as prose. Use tables for findings and checks, ordered
HIGH, MEDIUM, LOW; within a severity, show validated corrections, proposals,
manual handoffs and advice in that order. Include exact commands/results and
missing evidence. With no actionable findings, use that plain statement
instead of placeholder rows.

```markdown
## Performance review

<One sentence describing the outcome and most important impact.>

### Findings

| Severity | Rule / location | Finding and impact | Disposition | Validation |
| --- | --- | --- | --- | --- |
| HIGH / MEDIUM / LOW | <rule ID; path:line> | <scenario and evidenced cost> | Validated correction / Proposed repair / Manual handoff / Advice only | <performed check or missing proof> |

### Checks performed

| Check | Scope / command | Result | Missing evidence |
| --- | --- | --- | --- |
| <analyzer or architecture rule> | <actual scope and command/inspection> | Completed / Failed / Not run | <gap, or None> |
```

`Completed` records that a check ran; it does not imply no findings or measured
gain. A validated correction names its exact source and completed validation.
Unavailable or failed checks remain visible even when source review is complete.
