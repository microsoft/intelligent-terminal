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
6. Prepare and validate the caller's version-1 report through its permitted
   interface. Repair uses the fixed report file; fork guidance uses structured
   report data without a shell or file-writing tool. Request exactly the output
   allowed by the caller.

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
changing the sealed proposal, then runs the focused test and the required full
explicit-target WTA suite. Native validation must resolve the exact qualified test in the original
immutable head and execute it with exact matching; passing an unrelated
substring-selected group is not proof. Failure in any stage blocks publication.

Native Cargo stages require a source-only checkout: untracked paths, including
ignored files, and changes to original index path membership are rejected
initially and between stages. Physical file inspection uses the actual
checkout root independently of mutable Git metadata; source reparse points
are rejected. Tests use `--locked`, and every Cargo stage uses a fresh external
`CARGO_HOME` as well as a separate target directory. Later stages do not reuse
configuration, registry state or artifacts that earlier code could modify.
Cargo's ancestor `config`/`config.toml` files are captured before execution,
including absent paths, and must retain presence and bytes between stages.
Cold registry downloads and builds may exhaust the unchanged
30-minute validation deadline; report that as blocked with a manual handoff,
not a pass or a reason to weaken the gates.

The trusted validator validates and decodes the sealed proposal before Cargo
executes any candidate build scripts. After original-head listing, it applies
only that private in-memory snapshot: it never reloads the downloaded proposal
or executes the runtime helper again. Expected source hashes derive from the
original inventory plus those sealed bytes, not a newly observed worktree.
Mutable sibling files cannot authorize a different patch.

Do not claim that Linux has run Windows tests. Do not commit, call a branch-push
tool, or mark a proposal `fixed`. Trusted post-processing captures the exact
candidate blobs; a read-only Windows job runs the fixed validation commands
and requires executed passing tests. GitHub records the job result. The trusted publisher
checks that result and the original sealed blobs before committing with
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

## Report contract

Produce the following report. Use the caller-provided file path in repair
mode; in shell-disabled fork guidance, validate the JSON through the declared
read-only checker and submit its unchanged JSON string as the safe-output
body. Native post-processing owns file writes and final card rendering.

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
Use `pending_validation` for eligible HIGH proposals, `action_required` when unresolved HIGH remains,
`advisory` for only MEDIUM/LOW, `pass` for no findings, and `blocked` when a
check errors.

Run the validator and renderer before any safe output:

```powershell
pwsh -NoProfile -Command "node '.github/skills/pr-performance-review/scripts/performance-review.mjs' validate --report '<report>' --mode '<repair-or-guide>' --pr '<number>' --base '<sha>' --head '<sha>'"
pwsh -NoProfile -Command "node '.github/skills/pr-performance-review/scripts/performance-review.mjs' render --report '<report>' --mode '<repair-or-guide>' --pr '<number>' --base '<sha>' --head '<sha>'"
```

The PowerShell examples above apply only to the repair caller. A shell-disabled
fork caller must use its read-only MCP checker and GitHub read tools; it never
executes a shell or materializes fork source as executable code. If an
operation is denied, use the declared interface instead of trying alternate
executables or bypassing validation.

## Gotchas

- Never execute fork-controlled scripts or binaries with model credentials.
- Never infer end-to-end improvement from a microbenchmark alone.
- Never use a Linux CI timing as native Terminal runtime proof.
- Never claim fixed until validation covers the final patch.
