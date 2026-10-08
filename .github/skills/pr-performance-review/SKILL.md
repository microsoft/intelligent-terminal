---
name: pr-performance-review
description: 'Review and safely repair Intelligent Terminal PR performance regressions. Use for renderer, text-buffer, VT, UI-thread, tab/pane, WTA, async, session, logging, allocation, event-amplification, benchmark, profiling, responsiveness, memory, and CI-cost changes.'
---

# PR Performance Review

Investigate changes that can make typing, rendering, tab switching, WTA startup,
or session refresh slower as work repeats or data grows. Trace changed code
through its callers and lifecycle, compare base/head behavior, and require
concrete impact rather than speculative allocation or async cleanup.
Eligible small HIGH WTA regressions can proceed to native-tested repair;
C++ findings remain manual. These instructions focus the review but do not
establish that it detects more defects than ordinary Copilot review.

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
4. Read caller-supplied BASE/HEAD analysis metadata and diagnostics when
   available. Verify comparison revisions, tool versions, target, profile,
   analyzed projects/crate, and coverage gaps before comparing candidates.
   Same-repository workflow inputs live under
   `/tmp/gh-aw/performance-analysis/performance-analysis-BASE` and
   `/tmp/gh-aw/performance-analysis/performance-analysis-HEAD`.
   Raw logs and Cargo JSON are review input, not a formatted PR result.
   Interpret coverage by language. In version-2 metadata, required WTA Rust
   analysis is complete when `analyzedScope.wtaRustCrate` is true and the
   `rust-analysis` check completed with exit code zero in both bound records.
   `analyzedCppTranslationUnits` is C++-only: it is expected to be empty when
   `plan.cpp.required` is false and cannot block an otherwise-complete
   Rust-only review. When C++ is required, verify its actual translation-unit
   membership and coverage separately. Never infer Rust coverage from a C++
   field.
   Inspect relevant existing evidence: `src/ConsolePerf.wprp`,
   `src/tools/ConsoleBench`, `test/e2e/Measure-PaneContext.ps1`,
   `build/scripts/Measure-AgentHookOverhead.ps1`, focused tests, and duration
   telemetry.
5. Separate application performance, responsiveness, memory growth, and CI
   runtime/cost. Separate microbenchmark, end-to-end, and profile evidence.
6. Produce the human-readable summary below before preparing the mechanical
   version-1 report. Repair writes the caller's fixed summary and report files;
   fork guidance submits its summary and structured report through the declared
   tool without general shell or file-writing access. Follow the caller's
   mechanical validation contract: gh-aw repair uses its scoped report tool
   for early feedback and trusted post-processing for final authority,
   while fork guidance uses its scoped report tool. Request exactly the output
   allowed by the caller.

## Scoped performance rules

Apply the following checks only when their trigger is present in the immutable
patch or its affected callers. Use the rule ID in the finding's evidence so
the result can be traced to a concrete check, not a generic optimization opinion.
Do not launch one reviewer per rule: group applicable checks by the affected
subsystem when the caller permits delegation.

| Rule | Trigger | Required investigation | Intentional cases to distinguish |
| --- | --- | --- | --- |
| `PERF-REPEATED-WORK` | A loop, lookup, copy, or enumeration changes on a repeated path. | Compare base/head work as input size grows; trace the caller's frequency and actual input bounds. Check nested scans and materialized collections used only for a count or membership test. | Small fixed bounds, one-time startup, required snapshots, and amortized work. |
| `PERF-UI-BLOCKING` | A UI callback or dispatcher path gains synchronous work. | Trace the executing thread, I/O, waits, and traversal cost through callees. Identify the user interaction stalled and establish measurement or concrete blocking proof. | Already-background work, bounded cheap callbacks, and required thread-affine operations. |
| `PERF-MVVM-AMPLIFICATION` | A property setter, binding, collection update, or event subscription changes. | Trace notifications to their consumers. Count expensive refreshes per logical change, including bulk updates and reentrant handlers; compare base/head behavior. | Required individual notifications, cheap consumers, and existing batching that already prevents repeated refresh. |
| `PERF-ASYNC-CONTENTION` | Lock scope, awaits, channel usage, or synchronous work in an async task changes. | Establish what is held across suspension, who contends, and whether ordering can stall progress. Verify actual scopes rather than accepting a lint's diagnosis alone. | Async-aware locks, explicitly released guards, intentional backpressure, and operations that cannot suspend. |
| `PERF-OWNER-LIFETIME` | Tabs, panes, tasks, subscriptions, helpers, or pooled processes are created or retained differently. | Trace repeated creation through teardown and cancellation. Show retained work or memory growing beyond its intended owner or lifetime. | Shared resident pools, pre-warmed helpers, stash/restore, and bounded tasks intentionally outliving their caller. |

When the caller supplies analyzer results, inspect each diagnostic in the
immutable source before adopting it. Require the rule/tool version, target,
configuration, analyzed scope, and base/head results. Separate new or worsened
PR-related diagnostics from unchanged baseline debt. A moved line alone is
not a new defect; a shared-header or caller change can worsen an existing
diagnostic outside the edited lines.

An incomplete analysis, unsupported check, compile failure, or missing native
context is unavailable or blocked, not clean coverage. Do not replace a denied
native operation with a Linux result. Tool warnings are leads: they do not
establish HIGH severity, measured gain, or permission to apply a suggested fix.

The shared C++ `PerformanceAnalysis=Extended` profile adds four selected
Clang-Tidy performance checks to the existing AuditMode build infrastructure.
For WTA, `cargo wta-perf` is a normal developer entrypoint and
`cargo wta-perf-extended` adds `needless_collect` and `large_futures` individually.
These profiles are reusable outside PR review. Neither alias changes what
ordinary `cargo build` runs. Do not enable whole
nursery, pedantic, or restriction groups merely to increase the warning count.
`await_holding_lock` is a suspicious lint and can warn after an explicit drop;
`needless_collect` is nursery, and `large_futures` is pedantic. Judge the
actual source and scenario, not the group name. Collecting values can have
intentional iterator or clone side effects; a large future may be cold, and
boxing it trades stack size for allocation.

Declared C++ recipes do not cover every project or transitive caller.
Directory-prefix candidates are provisional; actual coverage requires native
MSBuild `ClCompile` membership and successful analysis. Unlisted or excluded
translation units and revision-absent sources require manual scope. Never
infer coverage from a successful build of a nearby library.
Referenced prerequisite builds are not additional Clang-Tidy coverage.
Unmapped/shared sources and HLSL, IDL, or XAML require a manual scope review.
Fork callers do not run source builds: record native analysis as not run and
continue permitted source/rule review without weakening the caller boundary.

## Triage before repair

Rank by demonstrated user/CI impact, execution frequency or data growth, and
confidence in PR causality—not warning count or an analyzer's default level.
Collapse duplicate diagnostics that describe the same underlying defect.

| Result | Action |
| --- | --- |
| Proven HIGH regression, high confidence, and every repair gate satisfied | Propose the smallest behavior-preserving WTA repair for trusted native validation. |
| Important defect, but redesign, ordering/lifetime uncertainty, or unsupported native validation | Report the evidence and manual handoff; do not edit. |
| Plausible cost with uncertain impact or missing frequency/measurement | Report MEDIUM/LOW with the missing evidence and tradeoff; do not edit. |
| Unchanged unrelated baseline debt, negligible bounded cost, intentional behavior, or false positive | Do not present it as a new performance defect. Record the reason in the review evidence or check detail rather than flooding the PR with suggestions. |

Never run blanket analyzer auto-fix or treat a machine-applicable suggestion
as proof of safety. For example, changing a C++ value parameter to a reference
can affect ABI or coroutine lifetime; reserving capacity can increase retained
memory. Evidence-based prioritization remains mandatory even when the tool's
syntactic replacement is straightforward.

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
- A recorded regression must have a finding with evidence and severity. It
  cannot coexist with an empty-findings passing report.

## Repair eligibility

Repair only when every condition holds:

1. severity HIGH and confidence high;
2. strong repository-specific proof;
3. one small, localized, behavior-preserving source change;
4. no broad threading, caching, architecture, dependency, generated-file,
   security-policy, or CI-policy change;
5. a supported native backend can run an existing focused test on the exact final tree;
6. the caller explicitly permits edits and branch push;
7. any caller-required base/head analysis is complete, correctly bound, and
   comparable. Missing, failed, partial, or mismatched required analysis
   requires a manual handoff, even when a source finding looks repairable.

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
`src`. Request exactly one each of `validate_performance_original_tests`,
`validate_performance_focused_tests`, and `validate_performance_repair`, all
with `confirm: true`, then stop editing.

Dedicated `tests.rs`, `*_tests.rs`, and WTA's `test_support.rs` are supporting evidence, not
automatic repair targets. Do not alter them to make a repair pass.
The exact WTA `build.rs` and Cargo manifest/lockfile receive CI-runtime review,
but automatic replacements remain confined to eligible `tools/wta/src/*.rs`.

Bind each proposed finding's canonical repository `path:line` location to a
sealed WTA Rust replacement, and cover every replacement with a proposed
finding. Category labels alone do not authorize edits: concurrency and session
findings can qualify at WTA locations; C++ findings cannot justify unrelated
WTA changes.

Keep proposals within the fixed conservative native-autofix ceilings: at most
three replacement files, 100 total added plus deleted lines, and 16 KiB of
UTF-8 zero-context Git diff bytes against the immutable reviewed head.
Trusted reconstruction enforces these limits during sealing, native
validation/application, and publisher reconstruction. Git `--numstat` supplies
line counts; `--unified=0` supplies actual patch bytes, with external diff,
text conversion, and renames disabled. The aggregate 256 KiB replacement-blob
transport cap remains separate; file length is not an edit-size proxy.
Large original modules can qualify when their actual edits meet every gate.
These necessary ceilings do not prove behavior preservation or semantic
locality and do not configure model routing. If any ceiling is exceeded, leave
the finding unresolved with a manual handoff rather than splitting or weakening
the proposal to bypass policy.

Preserve original inline tests byte-for-byte. Trusted reconstruction compares
Git blobs from the immutable head, never the editable workspace. Its
dependency-free lexical guard freezes the suffix from the first `test` or
`rstest` word, extended back to any preceding attribute opener or `mod` word.
Freeze their complete lines, including prefixes that could comment out tokens.
This deliberately overblocks comments, strings, early test helpers, and runtime
code after preceding attributes/modules; use manual handoff when blocked.
It covers common multiline `cfg`/`cfg_attr` gates and qualified `tokio::test`
attributes, but is not a Rust parser or proof of semantic equivalence. Unknown
custom test macros without those words and runtime helpers remain limitations.
Do not introduce, remove, rename, or weaken markers, gates, modules, or tests.
A runtime-only prefix repair with unchanged trailing inline tests is supported.

Format the proposed Rust change before sealing it. Use the standard
`cargo fmt --manifest-path tools/wta/Cargo.toml`, inspect its diff, and keep only
the permitted repair changes. Native validation checks formatting without
changing the sealed proposal in the focused phase. Three parallel read-only
`windows-latest` jobs separately list the original test, check formatting and
run the exact focused candidate test, and run the full explicit-target candidate
suite. Each job checks out immutable HEAD, loads trusted validation code and
downloads the original sealed artifact independently on a fresh hosted VM
and installed public Rust 1.93.0 toolchain with rustfmt. Every native Cargo
command explicitly selects `+1.93.0`, regardless of the runner's active
toolchain, to preserve the CI language/library version requirement. This
checks public Rust compatibility, not equivalence to the MSRustup distribution.
Never import toolchains, caches, native artifacts or
process state from another phase. Job-end platform cleanup removes descendants;
directory changes alone are not isolation. Native validation must resolve the exact qualified test in the original
immutable head and execute it with exact matching; passing an unrelated
substring-selected group is not proof. Failure in any stage blocks publication.

Native Cargo stages require a source-only checkout: untracked paths, including
ignored files, and changes to original index path membership are rejected
initially and before/after each phase's commands. Physical file inspection uses the actual
checkout root independently of mutable Git metadata; source reparse points
are rejected. Tests use `--locked`, and every Cargo stage uses a fresh external
`CARGO_HOME` as well as a separate target directory. The next native phase
never runs on that VM, including after failure.
Cargo's ancestor `config`/`config.toml` files are captured before execution,
including absent paths, and must retain presence and bytes between stages.
If repository-root `.cargo/config` or `.cargo/config.toml` differs between
comparison base and reviewed head, native repair is unavailable: keep the
finding manual rather than executing PR-controlled runner/wrapper settings.
Windows case-equivalent path names are treated identically on every sealing host.
Cold registry downloads and builds may exhaust the per-phase
30-minute validator / 32-minute step deadline; report that as blocked with a manual handoff,
not a pass or a reason to weaken the gates.

Each trusted validator validates and decodes the original sealed proposal before Cargo
executes any candidate build scripts. OriginalListing leaves HEAD unchanged.
Focused and FullSuite apply only that private in-memory snapshot upfront,
before any candidate code: neither reloads the downloaded proposal
or executes the runtime helper again. Expected source hashes derive from the
original inventory plus those sealed bytes, not a newly observed worktree.
Mutable sibling files cannot authorize a different patch.

Do not claim that Linux has run Windows tests. Do not commit, call a branch-push
tool, or mark a proposal `fixed`. Trusted post-processing captures the exact
candidate blobs; separate read-only Windows jobs run their fixed phase commands
and require executed passing tests in focused/full phases. GitHub records each job result. The trusted publisher
requires all three exact job and step names to have server-recorded success
in the same correlated worker run and checks the original sealed blobs before committing with
immutable-head CAS. It never consumes a receipt or files written by test code.
Only the publisher can render a proposal as `fixed`.
These are publication and validation gates, not a full sandbox for candidate
code executed by Cargo; passing tests alone do not prove behavioral equivalence.

Post-agent sealing must use a fresh caller-owned checkout created after the
agent has stopped. Pass `--trusted-repository-root` and `--agent-worktree-root`
separately to the repair gate. Load the helper from that fresh immutable
workflow checkout, never via `git show` in the agent's repository. Its Git
metadata/objects define scope, original tests, and reconstruction. Raw physical
source inventories compare against the protected pre-agent snapshot, including
trusted instruction restoration, ignored/untracked files, deletions and
filesystem modes. Do not use the agent's index, config, attributes, hooks,
replace refs, remote URLs or `core.worktree` for those operations. Candidate
bytes are read directly and inserted with `hash-object --stdin`/`update-index`
in the fresh Git context; clean filters and line-ending conversion do not run.
The caller must keep the baseline outside the agent's writable sandbox and
create the fresh checkout from canonical caller repository/revision inputs.

Native C++ repairs currently lack a supported backend; keep them unresolved and
unedited with the relevant MSBuild/TAEF handoff. This limitation does not block
eligible WTA fixes and must not be hidden by substituting a Linux timing or
schema check.

## Human-readable results template

Produce one Markdown summary using this template. Keep the opening summary
as an ordinary sentence; put findings and check results in tables.
Repair writes `.performance-summary.md` before the mechanical report.
Shell-disabled fork guidance supplies the same Markdown as `summaryMarkdown`
to its declared report tool; only the caller captures the output artifact.
Do not extract this summary from agent or build logs.

```markdown
## Performance review

<One sentence describing the outcome and most important impact.>

### Findings at review time

| Severity | Rule / location | Finding and impact | Disposition | Validation |
| --- | --- | --- | --- | --- |
| HIGH / MEDIUM / LOW | <rule ID; path:line> | <specific scenario and evidenced cost> | Proposed repair / Manual handoff / Advice only | <performed check or missing proof> |

### Checks performed

| Check | Scope / command | Result | Missing evidence |
| --- | --- | --- | --- |
| <analyzer or architecture rule> | <actual scope and command/inspection> | Completed / Failed / Not run | <gap, or None> |
```

Order findings HIGH, MEDIUM, LOW; within a severity, put proposed repairs before
manual handoffs, then advice-only results. Remove placeholder rows. If there
are no actionable findings, replace the findings table with that plain statement.
Do not turn absent or failed analysis into a clean review.
`Completed` means a check ran, not that it found no candidates or proved a fix.
Describe reviewed warnings and intentional cases in the result or evidence.

The summary describes the review-time decision. Never label a proposed repair
`Fixed`: only the trusted publisher can establish that status after native
validation and publication. The PR check supplies actual job/publication status,
reviewed SHA, and run links separately; the summary cannot override them.

## Report contract

Produce the following report. Use the caller-provided file path in repair
mode; in shell-disabled fork guidance, validate the JSON through the declared
scoped report tool and submit its unchanged JSON string as the safe-output
body. That tool captures only the caller's fixed summary artifact; it does not
permit general file writes or source execution. Native post-processing owns
mechanical artifacts and final card rendering.

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
      "type": "source | complexity-proof | blocking-proof | resource-proof",
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
when not run; every `unavailable` check requires `exitCode: null`.
`validationPlan` is required only for proposals.
For measurement evidence use `type: measurement` and a required `kind` of
`microbenchmark`, `end-to-end` or `profile`. Optional `noisy` must be boolean,
`samples` must be a positive integer, and `spread` must be a string of at least
three characters describing the observed spread.
Noisy measurements require at least three samples and reported spread.
Source/proof evidence must omit all four measurement-only fields.
The schema example deliberately omits `testFilter`: add that required field
using the actual test name you read, not a sample value.
Status `fixed` is reserved for the published card after native validation.
Pending proposal checks must not use `pass`, including source-only passes or
claimed native/benchmark successes. Record not-run checks as `unavailable`
with `exitCode: null`. The trusted publisher independently records
GitHub-reported native success; model checks never become native authority.
For a pending proposal, describe completed input analysis in the human summary
and finding evidence; reserve its mechanical validation checks for the proposed
repair. Do not falsely mark an analyzer that actually ran as unavailable.
Use `pending_validation` for eligible HIGH proposals, `action_required` when unresolved HIGH remains,
`advisory` for only MEDIUM/LOW, `pass` for no findings, and `blocked` when a
check errors.

Read this complete Report contract before authoring JSON. Every finding needs
all required fields, including its own `location`; a valid first finding does
not validate the others.

The gh-aw repair caller exposes `validate_performance_report` for field and
immutable-identity feedback while the agent can still correct its report.
Submit the full JSON, correct any rejection, and write the identical accepted
JSON to the fixed report path. Revalidate any later report change before
requesting native jobs or noop. Keep the explicit Markdown summary independent.
Trusted post-processing validates the persisted report again before sealing,
native validation, or publication; tool acceptance is not native authority.
Do not require a
duplicate agent-side shell validator or renderer, and never treat a shell error
as proof that native source analysis is incomplete. The native report, source,
test-selector, and publication gates remain mandatory.

A shell-disabled fork caller must use its scoped MCP report tool and GitHub
read tools; it never executes a shell or materializes fork source as executable
code. If an operation is denied, use the declared interface instead of trying
alternate executables or bypassing trusted validation.

For standalone callers that explicitly permit local report inspection, these
commands remain available; they are not required model steps in gh-aw repair:

```powershell
pwsh -NoProfile -Command "node '.github/skills/pr-performance-review/scripts/performance-review.mjs' validate --report '<report>' --mode '<repair-or-guide>' --pr '<number>' --base '<sha>' --head '<sha>'"
pwsh -NoProfile -Command "node '.github/skills/pr-performance-review/scripts/performance-review.mjs' render --report '<report>' --mode '<repair-or-guide>' --pr '<number>' --base '<sha>' --head '<sha>'"
```

## Gotchas

- Never execute fork-controlled scripts or binaries with model credentials.
- Never infer end-to-end improvement from a microbenchmark alone.
- Never use a Linux CI timing as native Terminal runtime proof.
- Never claim fixed until validation covers the final patch.
