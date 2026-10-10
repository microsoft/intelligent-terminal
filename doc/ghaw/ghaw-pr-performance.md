# Performance review workflow

The workflow investigates changes that can worsen typing, rendering, tab
switching, session refresh or repeated work. Findings require an affected
scenario, baseline comparison and concrete impact. Ordinary compilation and
analyzer warnings alone cannot establish caller frequency, ownership or
user-visible cost.

Same-repository PRs can receive small, native-tested HIGH WTA corrections.
MEDIUM/LOW findings remain advice-only; C++ corrections and fork PRs remain
manual. The controller automatically maintains a severity-ordered result in
the PR Conversation, with controller, worker and repair links.

## Ownership

| Layer | Responsibility | Location |
| --- | --- | --- |
| Agent | Goal, domain principles and scope | `.github/agents/performance-review.agent.md` |
| Skill | Reusable investigation, evidence, triage and human results | `.github/skills/performance-review/SKILL.md` |
| Workflow | PR identity, permissions, tools, artifacts and orchestration | `ghaw-pr-performance.md`, `ghaw-pr-performance-fork-guidance.md` |
| Shared workflow contract | Structured report handoff used by both workers | `.github/workflows/ghaw-pr-performance/report-contract.md` |
| Node adapter | Classification, report validation, exact-source sealing and publication | `.github/workflows/ghaw-pr-performance/scripts/performance-review.mjs` |
| Windows runner | Native tooling, process deadlines and physical source invariants | `.github/workflows/ghaw-pr-performance/scripts/run-native-performance-checks.ps1` |
| Controller | Correlated dispatch, server-result verification, publication and Conversation delivery | `ghaw-pr-performance-controller.yml` |

The agent and skill support local diffs, commit ranges and branch comparisons
as well as PRs. They have no workflow-specific tools, JSON schema, job protocol
or publication branches. The caller supplies those contracts.

The two runtimes follow platform and trust boundaries. Node supplies one
report/Git-object contract to Linux workers and the JavaScript publisher.
PowerShell owns Windows process lifetimes, MSBuild and native command
execution, and calls the same Node proposal validator before Cargo runs.
Moving those adapters out of the skill removes caller coupling; rewriting
the Windows runner in Node would not remove its validation responsibility.
Neither runtime is a second performance detector.

## Repository policy and drift

The backend is intentionally bounded to this repository, not a portable
arbitrary-project builder. Explicit source/project recipes and repair limits
are policy; the agent performs the performance reasoning.

Trusted preparation checks WTA's `rust-toolchain.toml`, the extended Cargo
analysis alias and the declared C++ owning projects before review. The public
Rust backend must match the CI language/library version. A changed CI pin,
missing alias or stale project recipe produces an explicit failure requiring
the binding to be updated, rather than silently using old coverage.

New/unmapped `src` source paths remain review candidates with manual scope.
Project prefixes select possible owners, while evaluated native MSBuild
`ClCompile` membership establishes actual translation-unit coverage.
Shared headers, unsupported languages and revision-absent files remain
explicit gaps. Referenced prerequisite builds are not extra analyzer coverage.

## Analysis

| Profile | Added capability |
| --- | --- |
| C++ `PerformanceAnalysis=Extended` | Four selected Clang-Tidy performance checks on top of AuditMode/MSVC/CppCoreCheck infrastructure. |
| Rust `cargo wta-perf` / `cargo wta-perf-extended` | Developer Clippy entrypoints; the extended alias adds `needless_collect` and `large_futures` individually. Ordinary Cargo builds do not run Clippy. |
| Shared skill rules | Repeated work, UI blocking, MVVM amplification, async contention and owner lifetime, investigated only where triggered. |

Before same-repository inference, separate read-only Windows BASE/HEAD jobs
use the same HEAD-derived plan and trusted profiles. Their metadata and raw
logs are diagnostic inputs, not repair validation. Failed or partial analysis
still permits a human source review, but blocks automatic repair.

Version-2 metadata separates completed WTA crate analysis
(`analyzedScope.wtaRustCrate`, zero-exit `rust-analysis`) from actual C++
translation units (`analyzedCppTranslationUnits`). Empty C++ coverage is
normal for a Rust-only plan. Both bound records must have matching
revisions/profiles/tools and completed required checks for repair eligibility.

Rust analysis freezes trusted Cargo aliases in disposable checkouts.
Unexpected ancestor configuration blocks analysis because array aliases can
concatenate and change the command. Existing warnings, explicit lock drops,
intentional snapshots and bounded work require contextual triage.

## Repair and publication

1. The controller resolves immutable PR identity and dispatches the permitted
   same-repository or fork worker.
2. The worker uses the shared skill and submits its report through the
   workflow-owned contract. Repair writes Markdown and the accepted JSON;
   fork guidance captures them through its scoped tool and emits `noop`.
3. After the agent stops, trusted processing reconstructs the proposal from
   immutable Git objects and exact replacement bytes, outside the agent's
   mutable Git metadata.
4. Three independent read-only Windows jobs validate the original test
   identity, the focused candidate, and the complete candidate suite.
5. The controller verifies server-recorded success in that correlated run
   and publishes the exact sealed tree with immutable-head compare-and-swap.

| Native tool/job | Required phase | Required server-recorded step |
| --- | --- | --- |
| `validate_performance_original_tests` | Original exact test listing; no patch applied | `List the exact test on original HEAD` |
| `validate_performance_focused_tests` | Candidate formatting and exact focused test | `Format and test the exact focused candidate` |
| `validate_performance_repair` | Independent complete explicit-target candidate suite | `Test the exact candidate full suite` |

All three requests use `confirm: true`. Missing, skipped, failed or zero-test
results block publication. Each job has a fresh VM, checkout, installed
toolchain and original artifact download, with no cache/process state imported
from another phase.

Corrections are confined to original candidate WTA Rust files, with at most
three replacements, 100 added-plus-deleted lines and 16 KiB of UTF-8
zero-context diff. The independent replacement transport cap is 256 KiB.
Every replacement has a corresponding proposed finding at its actual
`path:line`. C++ findings cannot authorize unrelated WTA edits.

Original tests remain unchanged. A conservative lexical suffix guard can
reject otherwise valid repairs involving comments, early attributes/modules
or custom test arrangements; those require a manual handoff.
It is not a Rust parser or a semantic
equivalence proof. Source replacements are read and decoded before candidate
code runs; later mutable proposal/helper files cannot authorize other bytes.

Native stages enforce physical tracked-source inventories, reject
untracked/ignored paths and reparse points, and preserve ancestor Cargo
configuration. They use `--locked`, fresh external `CARGO_HOME` and target
directories. PR changes to root Cargo configuration or WTA CI toolchain policy
require manual validation. Product tests and receipts never become publication
authority.

These controls are not a full sandbox for code executed by Cargo. Passing
tests cover their actual assertions, not all behavior or production speedup.

## Reporting and operational bounds

The human summary comes directly from the agent's results template, not log
extraction. JSON governs identity, eligibility and source handoff; it cannot
replace the human explanation or declare native success.

The controller is the sole Conversation publisher. It updates only its own
marked `github-actions[bot]` comment, checks the live head immediately before
posting, suppresses mentions in that copy, and keeps an otherwise-successful
check non-green until delivery succeeds. Workers do not combine comment and
commit operations.

An entered fork submission invalidates previous accepted JSON before summary
capture and validation. Rejected replacement data therefore cannot reuse an
old report; transport rejection before the handler changes neither artifact.

| Bound | Enforcement |
| --- | --- |
| BASE/HEAD analysis | 12-minute jobs, 10-minute command deadline |
| Native phase | 30-minute validator / 32-minute step, plus bounded 10-minute toolchain installation |
| Controller | 150-minute worker wait within a 175-minute job |
| Model | Auto; 500 main-agent and 100000 daily AI-credit caps; framework detection has a separate budget |

The compiled critical phases permit 132 minutes. Queue/setup delays can still
exhaust the controller headroom. Pinned gh-aw v0.87.10 does not emit a custom
safe-job timeout override; step deadlines and controller cancellation are the
enforced bounds, not a claimed 35-minute native job limit.

## Validation and evidence

```powershell
node --test .github\workflows\ghaw-pr-performance\tests\*.test.mjs
pwsh -NoProfile -File .github\workflows\ghaw-pr-performance\tests\RepairValidationPhase.Tests.ps1
pwsh -NoProfile -File .github\workflows\ghaw-pr-performance\tests\RepairValidationPhase.Tests.ps1 -AnalysisOnly
gh aw compile ghaw-pr-performance ghaw-pr-performance-fork-guidance --validate --no-check-update
actionlint -shellcheck= -pyflakes= -ignore 'unknown permission scope "copilot-requests"' -ignore 'unexpected key "queue" for "concurrency" section' .github\workflows\ghaw-pr-performance.lock.yml .github\workflows\ghaw-pr-performance-fork-guidance.lock.yml .github\workflows\ghaw-pr-performance-controller.yml
```

Use actionlint 1.7.12; only its two known field-schema gaps are excluded.
Fixtures exercise report, scope, drift, native and immutable-publication
contracts. They do not establish product speedup or detector superiority.

| Historical hosted evidence | Verified outcome |
| --- | --- |
| [Controller 37758597556](https://github.com/yeelam-gordon/ghaw-pr-performance-private-20261004/actions/runs/37758597556) / [worker 37758613236](https://github.com/yeelam-gordon/ghaw-pr-performance-private-20261004/actions/runs/37758613236) | Synthetic Rust fixture: all three Windows phases passed; one test in focused/full phases; HIGH repaired, MEDIUM unchanged. |
| [Published repair](https://github.com/yeelam-gordon/ghaw-pr-performance-private-20261004/commit/23c87098fd0e01c70abed0bb1d20edb1cb0ba68e) | Exact reviewed parent and sealed native-tested tree; one source file changed, original inline tests preserved. |
| [Reporting replay 37768180616](https://github.com/yeelam-gordon/ghaw-pr-performance-private-20261004/actions/runs/37768180616) | Actual controller created then updated the same [bot result](https://github.com/yeelam-gordon/ghaw-pr-performance-private-20261004/pull/5#issuecomment-6058555491), without another repair/inference run. |

This evidence predates the instruction/ownership correction and is not a new
hosted run of that correction. No production performance, hosted C++ coverage,
MSRustup equivalence or detection superiority is claimed. The earlier
single-phase success is not proof of the current three-phase architecture;
failed trials remain failure evidence. Private performance workflows were
disabled after verification; merge and deployment remain separate decisions.

## References

- Existing localization controller and repair/guide workers: caller-owned
  orchestration with shared domain expertise.
- #1073: immutable preparation and explicit permitted tools.
- [gh-aw custom safe outputs](https://github.com/github/gh-aw/blob/v0.87.10/docs/src/content/docs/reference/custom-safe-outputs.md).
- [GraphQL createCommitOnBranch](https://docs.github.com/en/graphql/reference/input-objects#createcommitonbranchinput).
- [Chromium measurement discipline](https://github.com/chromium/chromium/blob/c7ea1cb426922e7541d010aaed7f68e469e674f2/docs/speed/microbenchmark_regressions.md).
