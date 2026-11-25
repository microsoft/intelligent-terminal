# Performance PR review

Review changes affecting rendering, typing, tab/pane interactions, WTA sessions,
repeated work, memory and CI cost. Classify each finding HIGH, MEDIUM or LOW
before deciding whether to edit it.

## Ownership

| Owner | Responsibility |
| --- | --- |
| `performance-review` agent and shared skill | Source investigation, severity, minimal corrections and the Markdown summary template. |
| Repair worker | One separate proposal commit per eligible HIGH finding; native gh-aw branch push. |
| Controller | Same-repository, non-draft PR gating, immutable scope and delivery of the agent-written Markdown. |
| Original reviewer | Build/test, acceptance and optional rollback of a proposed fix commit. |

The agent writes the findings table, including each proposal's actual commit
link. No script builds a second findings summary, and no JSON report/card
protocol is required.

CI warning level does not set review importance. The reviewer starts from the
PR diff and affected callers, uses diagnostics as evidence, and reports only
introduced or worsened HIGH/MEDIUM costs as findings. LOW advice and unchanged
baseline warnings are grouped in checks, not repeated as a diagnostic backlog.

## Autofix policy

Only high-confidence HIGH findings supported by relevant scenario evidence
qualify for small, localized, behavior-preserving proposals. Easy replacement
does not make an uncertain-impact optimization HIGH. MEDIUM/LOW remain advice;
unsafe or broad changes require manual handoff.

Each HIGH finding gets one minimal commit with a distinct stable identifier:
`Propose HIGH PERF-<id>: <remedy>`. Do not combine findings, rebase, merge, amend
or include unrelated cleanup. Existing inline tests are preserved.

Automatic edits remain limited to original candidate WTA Rust files: at most
three files, 100 added-plus-deleted lines and 16 KiB of aggregate zero-context
diff. Dedicated tests and dependency/toolchain configuration are not repaired.
Budgets sum every parent-to-fix-commit diff and count the union of touched files,
not merely the final net change; later commits cannot hide an oversized rewrite.
C++ source can be reviewed, but corrections require manual handoff.

**The workflow does not build or test proposals before push.** Successful PR CI
is required for the original reviewed head, not for later proposal commits.
The original reviewer still owns validation of those proposals.

The summary contains one shared notice below the findings table:

> Proposed fixes committed without build/test. The original reviewer owns
> validation and may revert the fix commit.

That notice is not repeated per finding. Committed proposals are not described
as verified or tested fixes.

## Workflow

1. The controller starts only after `PR CI` completes successfully for the
   current head of a non-draft, same-repository PR targeting the default branch.
   Failed, cancelled, pending, stale and foreign CI results cannot launch review.
   Fork PRs are skipped and
   do not dispatch a guidance worker or publish a performance report.
   It loads its scripts from GitHub's automatic `github.workflow_sha` revision,
   independently of the PR's age. For eligible PRs it fetches the immutable head,
   resolves the merge base from the PR's recorded base and skips agent execution
   when no performance-sensitive source changed. No commit ID is hardcoded.
   Controller and worker use the default-branch workflow revision. Admission
   reuses the security workflow's CI identity and unchanged-definition checks.
   The native jobs are a same-repository reusable workflow so GitHub records
   their immutable revision. Admission checks that revision and its helper
   definitions, merge parents, successful native jobs and original default-target
   context. Mutable PR associations alone do not prove which CI ran.
   PRs changing CI implementation require manual review, not self-attested CI.
   Documentation-only PRs excluded by PR CI do not start this review.
2. The repair worker inspects source and callers, creates a separate local
   commit for each eligible HIGH finding, then writes the shared summary
   template with those commit links to the framework's pre-created
   `/tmp/gh-aw/agent/performance-summary.md`. The file is required even with no
   findings or no eligible fixes, before requesting `noop` or push.
3. Trusted post-processing checks the actual captured bundle's commit range
   from the immutable reviewed head, not just the current checkout. Its tip
   must match the recorded proposal. Existing authored C++/Cargo/documentation
   changes stay outside this new-fix range. It does not parse Rust test items;
   the agent preserves tests and the original reviewer checks the diff.
4. gh-aw's existing `push-to-pull-request-branch` handler pushes one bundle.
   `signed-commits: false` preserves the agent's separate commit IDs; fallback
   PRs and extra CI-trigger commits are not enabled.
5. The controller delivers that same Markdown with execution links. It does
   not create a new findings table or insert per-finding validation claims.

The native push handler is fixed to the originating PR, uses its Actions token,
restricts files to WTA source and blocks protected files. The reviewed head is
checked before handoff; an incompatible branch advance fails the push rather
than being automatically rebased.

The controller dispatches only `ghaw-pr-performance.lock.yml`, passing ordinary
pull-request context. The worker independently verifies the live PR identity,
immutable revisions and same-repository status before preparing a proposal.
The shared performance-review agent and skill retain their read-only review
capabilities for use outside this automated same-repository flow.

## CI performance evidence

PR CI retains head/run/attempt-bound raw C++ and Rust diagnostics alongside its
normal production builds and unit tests. C++ reuses those Debug outputs for
`PerformanceAnalysis=Extended` / `ClangTidy`, bounded to 20 changed production
inputs, 2,000 changed lines and three directly declared projects. Evaluated
membership is checked; unsupported/shared-header consumers remain explicit
coverage gaps. Changed WTA Rust inputs use the same size bound and the existing
`cargo wta-perf-extended` Windows-target alias after build/tests, with the same
target outputs. Unchanged languages and dedicated test-only changes skip the
extra analyzer invocation.

CI keeps shallow checkouts. A read-only API job resolves the immutable merge
base, then each build job fetches that one commit before executing PR code.
The analyzer itself does not fetch or receive fetch credentials. Receipts bind
the comparison base as well as head/run/attempt so a different scope is rejected.
CI analysis tools are checked out from the recorded native-workflow revision,
not inferred from a later PR target change.

Performance warnings remain advisory: the C++ analyzer explicitly uses
`ClangTidyExtraArgs=-Wno-error` without changing the normal build's warning
policy. The extra CI Clippy pass disables the default Clippy group, enables
the official `clippy::perf` group and retains the alias's individual extra
checks. `clippy::correctness` remains fatal; normal build/test checks are
unchanged. Rust compiler diagnostics may still accompany the lint output and
are not automatically performance findings. Compiler/parser errors, missing
tools and analyzer execution failures still fail CI and prevent review kickoff.
There is no second native build job in the performance reviewer.

The worker checks CI identity again and downloads only matching performance artifacts
before inference, then confirms run/attempt identity again. Missing, stale,
failed or malformed evidence stops admission rather than looking clean.
Before inference, the existing collector uses immutable Git change hunks and
native diagnostic locations to write `pr-diagnostics.json`. Only warnings
overlapping added/replaced PR lines enter that default input; duplicate
emissions are removed. Excluded counts stay visible without repeating the
backlog text, and original raw logs remain unchanged.

This is input scoping, not a severity classifier or proof that outside-line
warnings are unrelated. The agent reviews changed callers and can retrieve
relevant raw diagnostics when a change worsens an unchanged implementation.
It does not read or re-triage the full excluded backlog by default.
The agent inspects selected warnings in source, compares with the original base and
continues intelligent diff/caller review even with no diagnostics. Head-only
warnings do not establish a newly introduced regression or a HIGH finding.

## Review coverage and recovery

Review coverage includes relevant C++, XAML and Rust changes; the Rust-only
proposal allowlist restricts edits, not investigation. Findings require an
introduced, unintended cost in an affected scenario. Negligible or intentional
work belongs in checks/exclusions, not filler finding rows.

The controller retries transient 502/503/504 responses from read operations at
most twice and briefly re-reads delayed completed agent-step metadata. It does
not retry uncertain worker dispatches. A cancellation conflict is accepted only
after confirming that the worker completed; failed execution still fails.
Worker preparation also retries its metadata read before applying the unchanged
immutable PR-identity checks.

This workflow still performs source review, not automatic native C++/Rust
analysis. The documented analysis profiles are opt-in. Native Windows analysis
is a separate integration; these recovery changes do not claim that coverage,
artifact-upload recovery or inference-result reuse.

## Reporting

The controller updates only its own marked `github-actions[bot]` comment.
The repair worker copies the agent-written file into its summary artifact;
`noop.message` is not the report transport. No summary is generated or rewritten
during collection. Fork PRs do not run this workflow.
Numeric run/attempt markers prevent an older rerun from replacing a newer
report, including when duplicate marked comments exist: the greatest recorded
run/attempt determines freshness and the update target. It checks the live head against the worker's recorded head before
delivery and suppresses mentions in the Conversation copy.

Summary artifacts must be bounded, regular UTF-8 files, not symlinks/reparse
paths. Missing or unreadable required summaries and failed delivery do not
produce a green check. A successful workflow check means execution/delivery
completed; action-required delivery also fails the controller's publishing step.
It does **not** mean the proposed changes were built or tested.

Review jobs can be superseded, but report publication runs in a separate
per-PR concurrency group with `cancel-in-progress: false` and `queue: max`.
The publisher checks freshness and the live head inside that group, covering
the final comment read/write interval. This serializes cooperating publishers;
it is not API compare-and-swap and does not coordinate older workflow revisions
that used a different group.

## Local workflow-contract checks

```powershell
node --test .github\workflows\ghaw-pr-performance\tests\*.test.mjs
gh aw compile ghaw-pr-performance --validate --no-check-update
actionlint -shellcheck= -pyflakes= -ignore 'unknown permission scope "copilot-requests"' -ignore 'unexpected key "queue" for "concurrency" section' .github\workflows\ghaw-pr-performance-controller.yml .github\workflows\ghaw-pr-performance.lock.yml
```

These are development checks of the automation, not builds/tests performed on
an autofix before push. No new tooling or paid model trial is required by them.

To exercise the CI producer with real Windows Clang-Tidy and Clippy, run
`node --test build\scripts\ci-performance-analysis.test.mjs` from a VS x64
developer shell with `PERFORMANCE_NATIVE_PROOF=1`. For nested local worktrees,
set `PERFORMANCE_NATIVE_FIXTURE_ROOT` to a `Generated Files` directory beneath
only one repository `.cargo` configuration; nested Cargo array aliases merge.

## Historical evidence

The [October 14 trial](https://github.com/yeelam-gordon/ghaw-pr-performance-private-20261014/pull/1)
proved the previous native-tested implementation's agent, three Windows gates,
exact-tree publication, bot result and cleanup. The model also promoted
uncalled allocation advice to HIGH; that observation remains recorded.

That trial used a synthetic one-test library and Luna test-model overrides.
It does not prove production performance, Auto selection or this simplified
commit-without-build/test policy. Historical artifacts are not rewritten.
