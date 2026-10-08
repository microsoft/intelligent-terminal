# Performance review workflow

Correct code can still make typing, rendering, tab switching, or session
refresh slower. This workflow asks Copilot to investigate those performance
scenarios specifically, rather than treating every allocation or async call
as an optimization opportunity.

| Performance question | Required investigation |
| --- | --- |
| Does a small change multiply work? | Trace callers and event frequency; compare base/head loops, copies, and enumeration as buffers or session counts grow. |
| Can the UI or another task be stalled? | Trace synchronous work, locks, awaits, and the thread that executes them. |
| Does work or memory survive its owner? | Trace tab/pane, helper, task, and process creation through teardown. |
| Is an apparent improvement real? | Use existing native tests and measurements; distinguish source proof, microbenchmarks, end-to-end results, and missing evidence. |

**The added capability is an evidence-based review and repair process, not a
proven better detector.** Findings identify the affected scenario, repeated
path, base/head behavior, and concrete impact. Eligible small HIGH WTA fixes
can be tested on Windows and published to the exact reviewed head; C++ fixes
remain manual. Every applicable review gets a linked run report. The agent
and shared skill do the analysis; scripts enforce the report and publication
rules.

**We have not shown that this finds more performance defects than ordinary
Copilot review.** The current path passed a real hosted synthetic Rust trial:
[controller](https://github.com/yeelam-gordon/ghaw-pr-performance-private-20261004/actions/runs/37758597556)
and [worker](https://github.com/yeelam-gordon/ghaw-pr-performance-private-20261004/actions/runs/37758613236).
All three independent Windows phases succeeded and the controller published
the exact sealed repair tree. The HIGH refresh regression was fixed; the
MEDIUM collection candidate remained advice-only and unchanged. This proves
the workflow path, not detection superiority, production speedup, or hosted
C++ coverage. Earlier failed trials remain documented below.

Shared build profiles and base/head
PR analysis are implemented. C++ developers can select the opt-in `PerformanceAnalysis=Extended`
MSBuild profile; Rust developers can use `cargo wta-perf` and `cargo wta-perf-extended`
from the repository root. The ordinary Cargo build does not run Clippy.
Normal CI already enables C++ AuditMode; availability of Clippy in its pinned
MSRustup distribution remains unverified. Do not present these profiles as
additional checks already running on every normal CI build or PR.

## File layout and localization comparison

Like localization, the workflow imports a thin agent and keeps reusable logic
inside its skill. Runtime files live in `pr-performance-review/scripts`;
fixtures live in `pr-performance-review/tests`, not alongside runtime helpers
in another scripts directory.

| Runtime file | Concrete responsibility |
| --- | --- |
| `performance-review.mjs` | Classify scope, validate/render evidence, seal/reconstruct the permitted patch, process fork reports, and publish only the GitHub-validated exact tree. |
| `run-native-performance-checks.ps1` | Run scoped C++/Rust analysis, or the fixed original-test listing, focused candidate test, and full Windows WTA suite. |

Short dispatch checks stay inline in the workflows. Fork report processing
and publication reuse the shared helper rather than separate wrapper scripts.

Localization uses one shared resource checker and built-in branch push;
its checks do not execute edited native product code. This workflow executes
WTA code, so the Windows test boundary and exact-tested-tree publisher serve
different requirements. The extra files are not additional agents or
performance detectors.

## PR result summary

The agent writes one explicit Markdown summary using the shared skill's
template: an ordinary opening sentence, a findings table ordered HIGH,
MEDIUM, LOW, and a separate checks-performed table. Findings include rule and
source location, impact, review-time disposition, and validation or missing
evidence. Within a severity, proposed repairs precede manual handoffs and
advice-only results.

Repair writes `.performance-summary.md` before its mechanical JSON report.
Fork guidance supplies the Markdown through its existing scoped report tool;
the caller captures only the fixed output artifact, without granting source
edits or general filesystem access. Summary capture/upload is independent of
JSON validation, so an invalid mechanical report does not erase the findings.

The controller displays the bounded UTF-8 Markdown directly, after trusted
job status, reviewed SHA, and run links, in both the check and an automatically
maintained PR Conversation comment. The comment starts with a fixed marker;
only the controller's own `github-actions[bot]` comment is updated, so later
runs do not duplicate it or overwrite a user's comment. The controller checks
the live PR head immediately before posting and does not replace current
conversation results with a stale reviewed head. Mention notifications are
disabled in the conversation copy without changing the original artifact.
It does not extract the review from
agent logs or parse the Markdown to decide whether publication is permitted.
Missing or unreadable artifacts produce an explicit **Review summary missing**
message and run link, and must prevent an otherwise-successful PR check from
remaining green. JSON policy failures and failed native jobs remain
failures; readable Markdown cannot turn them into passes.
Conversation posting errors fail the reporting step and leave an otherwise
successful check non-successful until delivery completes. This trusted
reporting action is separate from the gh-aw workers: repair still never
combines a comment with a branch commit, and fork guidance never edits source.
The controller is the sole Conversation publisher for both modes. Fork
guidance captures its validated JSON and Markdown, then emits `noop`; it has
no later `add-comment` job that could post after an earlier freshness check.
Each entered guide submission handler invalidates any previous accepted JSON before
capturing the summary or validating the replacement. A rejected latest
submission therefore retains diagnostic Markdown but cannot reuse an old
report to obtain a successful verdict. Input rejected before the handler
does not modify either previously accepted artifact.

Automatic conversation delivery was verified in a reporting-only
[GitHub Actions run](https://github.com/yeelam-gordon/ghaw-pr-performance-private-20261004/actions/runs/37768180616).
It executed the actual controller reporting script twice against the already
validated fixture: first creating, then updating the same
[`github-actions[bot]` result comment](https://github.com/yeelam-gordon/ghaw-pr-performance-private-20261004/pull/5#issuecomment-6058555491).
No agent inference, native repair or publication was rerun. The replay workflow
exists only in the private validation repository, not this implementation.

The model's table uses `Proposed repair`, never a premature `Fixed` claim.
Actual validated/publication status comes separately from the trusted
controller. Mechanical JSON remains necessary for proposal identity, scope,
sealing, and publication; it is not the source of the human PR summary.
The repair agent first submits the complete JSON to the caller's scoped
`validate_performance_report` tool. Field and immutable-identity errors are
returned while the agent can correct them; after acceptance it writes the
identical JSON to the fixed report path. The tool reuses the existing shared
validator, reads no source, writes no files, and does not grant native success.
The successful hosted trial exercised this feedback: the agent's initial
report claimed passing checks before native validation, the tool rejected it,
and the agent corrected its JSON within the same run.
Trusted post-processing independently validates the actual persisted report again
before sealing or native execution. The agent does not need to invoke a
duplicate shell validator or renderer. This removes the workflow's dependency
on the failed agent-side PowerShell route without broadening permissions or
claiming that the underlying CLI exception is fixed. Native formatting,
original-test, focused/full-suite and publication checks remain mandatory.
Each native job independently installs public Rust 1.93.0 with rustfmt and
explicitly selects it for every Cargo command; the hosted image's active
toolchain cannot silently raise the repair's language/library requirement.
This is not proof of equivalence to CI's MSRustup distribution.

## Additional rule coverage

Before same-repository inference, two read-only Windows jobs analyze the
comparison base and reviewed head. Both use the scope derived from the
immutable head and the same trusted analysis profiles. The agent receives
metadata and raw diagnostic files as input, reviews candidates against
base/head source and caller frequency, then writes its own result tables.
No script reconstructs the PR report from those logs.

| Profile | Existing baseline | Extended checks |
| --- | --- | --- |
| C++ | AuditMode with MSVC analysis and CppCoreCheck; already enabled in the checked normal CI pipeline. | Four explicit Clang-Tidy checks: inefficient vector operations, range-loop copies, unnecessary value parameters, and moves from const values. |
| Rust | Normal builds/tests do not invoke Clippy. The shared `wta-perf` alias provides a developer analysis entrypoint. | `wta-perf-extended` adds `needless_collect` and `large_futures` individually, not entire nursery or pedantic groups. |
| Architecture | Ordinary compilation cannot establish caller frequency, notification amplification, or intended ownership. | Five scoped skill rules cover repeated work, UI blocking, MVVM amplification, async contention, and owner lifetime. |

C++ analysis selects provisional project recipes in renderer, buffer, VT,
TerminalCore, TerminalControl, TerminalApp, WindowsTerminal, settings editor,
and shared types. Directory prefixes are not coverage proof. After restore,
native MSBuild evaluates `ClCompile` membership, including imported items,
conditions, and build exclusions. Only successfully analyzed translation units
appear in actual coverage, and the sealing gate requires that coverage in both
revisions. Unlisted files, unsupported owners, and revision-absent additions
or deletions require manual scope rather than a false completion claim.
Headers still require affected-caller tracing. Referenced
projects receive normal prerequisite builds, not promised Clang-Tidy coverage.
Unmapped/shared sources and HLSL, IDL, or XAML remain manual scope; they are
not silently counted as analyzed.

Rust analysis copies the trusted authoring revision's Cargo configuration
only into disposable analysis checkouts to use identical profiles. Actual PR
configuration changes remain visible in the immutable source review, and
the native repair configuration guard is unchanged. Unexpected ancestor
Cargo configurations block analysis: even identical arrays can concatenate
and change alias arguments.

The `performance_analysis` BASE/HEAD jobs retain 12-minute job bounds and a
10-minute analysis deadline. Failed analysis does not skip inference or erase
the human summary. Missing, partial, failed, or mismatched required analysis
blocks proposal sealing. Diagnostics are review leads, not proof of impact
or permission to apply analyzer fix-its.

Coverage metadata is language-specific. Version 2 uses
`analyzedScope.wtaRustCrate` with a completed zero-exit `rust-analysis` check
for Rust, and `analyzedCppTranslationUnits` for actual C++ membership.
An empty C++ list is normal when no C++ project is required; it does not
indicate missing Rust coverage. Version-1 metadata is rejected rather than
silently interpreted under the new contract.

Isolated fixtures demonstrate added diagnostics and intentional/negative cases,
including the known explicitly-dropped-lock false positive. They do not prove
production speedup, complete project coverage, or better real-PR detection
than ordinary Copilot review. Public Rust 1.93 fixtures passed; availability
of Clippy in normal CI's pinned MSRustup distribution remains unverified.

## Flow

1. `ghaw-pr-performance-controller.yml` checks the immutable PR diff, skips
   non-runtime changes, and dispatches a same-repo repair or read-only fork
   worker using the localization pattern.
2. The thin `ghaw-pr-performance.agent.md` role follows
   `pr-performance-review/SKILL.md`: trace callers, distinguish native runtime,
   responsiveness, memory and CI cost, and avoid speculative optimizations.
3. Eligible same-repo HIGH changes are proposals, not model-authored claims of
   passing tests. Trusted post-processing seals their exact Git blobs and
   confines them to the original candidate files.
   Each proposed canonical `path:line` must identify a sealed WTA Rust
   replacement, and every replacement must be covered. Concurrency/session
   categories remain eligible by actual location, not category label.
4. Three parallel read-only `windows-latest` custom safe-output jobs each use
   a fresh hosted VM, independent trusted-code and immutable-head checkouts,
   and a separate download of the original sealed artifact:
   `validate_performance_original_tests` lists the exact test on original HEAD;
   `validate_performance_focused_tests` applies the private sealed snapshot,
   checks formatting and runs that test with exact matching;
   `validate_performance_repair` independently applies the snapshot and runs
   the full explicit-target suite.
   It rejects failed/zero tests and tracked-source mutation.
   No toolchain, cache, native artifacts, mutable downloads or processes are
   imported from another native phase. Actions owns VM/job-end process cleanup;
   there is no custom Windows process manager
   or native receipt file.
5. The trusted controller requires server-recorded success for all three exact
   job names and their respective exact phase step names in the same correlated
   worker run. Missing, skipped or failed phases block publication. It checks
   the original sealed proposal, and publishes its exact tree with immutable
   `expectedHeadOid`. It never rebases or retries a stale repair.

For every applicable review, the controller publishes a **Performance review**
check with direct controller and agentic-worker report links, including failed
outcomes. After a repair, the check is attached to the validated published
head, following localization's completion-check pattern; otherwise it belongs
to the immutable reviewed head. No manual PR-description update or additional
agent comment is required.

The Windows test jobs have only `contents: read`, no model or branch-write
credentials and no cache exports. Their files are
not publication authority: the publisher consumes the immutable Linux-sealed
proposal and GitHub's job result. Same-repo and fork workers cannot both
comment and commit.

The fork agent has no shell or edit permission. It reads the prepared patch
and immutable source through read-only tools, validates and captures structured
report data through the declared checker, and requests only `noop`.
Trusted post-processing validates the captured JSON and emits a verdict;
the controller publishes the summary after its live-head check. Fork
content is never executed to produce the report.

## Evidence and limitations

HIGH requires high confidence plus measurement or source proof of complexity,
blocking, or resource lifetime. MEDIUM/LOW are advice-only. A microbenchmark
is not end-to-end proof; noisy measurements need samples and spread.
Published evidence entries retain measurement kind, sample count and spread
when supplied; source-only evidence retains its existing format.
Measurement-only fields are rejected on source/proof entries; optional
measurement metadata is typed, with positive sample counts and reported spread.

After the agent stops, the repair worker requires an absent
`.performance-trusted` directory and creates a fresh pinned checkout of the
immutable workflow revision, using the canonical caller repository input.
Both the runtime helper and all sealing Git operations use this fresh context,
not the agent's repository metadata or objects. The gate receives separate
trusted-repository and physical agent-worktree roots. It compares a protected
pre-agent raw source inventory (including trusted instruction restoration)
with the final physical files, including ignored/untracked files, deletions,
symlinks and filesystem modes. The freshly created trusted checkout is the
only additional excluded source directory; the original root `.git` is never
consulted. Candidate bytes are hashed directly through stdin and reconstructed
with a private index in the trusted checkout, without staging the agent tree,
clean filters, attribute conversion, or trusting its remote/replace refs.
Original scope, inline-test protection and all diff ceilings use immutable
objects from the fresh checkout. Git helper calls also ignore inherited
Git overrides and global/system configuration.

This boundary assumes the agent has stopped and cannot mutate the protected
baseline or fresh checkout; it is not a concurrent-writer or host sandbox.
Windows physical inventories cannot infer Git executable-bit changes from
filesystem permissions; replacements must still match immutable `100644`
head entries. Framework credential setup remains compiler-owned and precedes
these post-steps; this change neither replaces nor claims to sandbox it.
Local malicious-filter, worktree-redirect, replace-ref and compiled-post-step
fixtures cover this boundary. No additional hosted performance trials ran.

Use existing WPR/ConsoleBench, pane-context and hook-overhead facilities when
relevant. Missing measurement is unavailable, not a passing benchmark.
The current automatic native backend supports focused Windows WTA Rust tests;
C++ findings require an applicable MSBuild/TAEF handoff and remain manual.
Fork guidance names the reviewed head and uses best-effort freshness checking.
The exact WTA build script, manifest and lockfile receive CI-runtime review,
while automatic Rust replacements remain confined to eligible `src` files.
Unavailable checks always use a null exit code and render as not run.
The controller keeps a 150-minute worker deadline inside a 175-minute job
budget. The actual compiled critical-phase bounds total 132 minutes: preparation
5, analysis 12, agent job 60, detection 10, and the longest parallel output/native
path 45. The 15-minute model step is not the agent job's timeout. The worker
deadline includes 18 minutes of orchestration headroom; the controller reserves
25 more for preparation, dispatch correlation, publication and the linked report.
Queue and setup delays can still exhaust these explicit bounds.

Native autofix uses fixed conservative limits, not model-routing settings:
at most **three replacement files**, **100 total added plus deleted lines**,
and **16 KiB of UTF-8 zero-context Git diff bytes** against the immutable
reviewed head. The shared reconstruction path enforces these limits at
sealing, native validation/application, and publisher reconstruction, before
publication. Line counts come from Git `--numstat`; byte counts measure the
complete `--unified=0` patch, including headers. Both disable external diff,
text conversion, and renames. Binary/non-numeric line counts fail closed.
The aggregate **256 KiB replacement-blob transport cap** remains independent;
it is not a small-diff boundary. Large original files with small actual edits
remain eligible when inline-test protection and all other gates permit them.
Local fixtures verify the inclusive 100-line/16,384-byte boundaries and reject
101 lines/16,385 bytes, aggregate multi-file edits, and larger module rewrites.
These are necessary ceilings, not proof of behavior preservation or semantic
locality. Over-limit repairs require an unresolved finding and manual handoff;
do not split proposals to evade the policy.

Local third-review hardening protects inline test source using immutable Git
head blobs during sealing, native proposal validation/application, and publisher
reconstruction. The dependency-free lexical policy freezes the suffix from the
first `test`/`rstest` word, extended to any earlier attribute opener or `mod`
word. It handles common multiline cfg/cfg_attr and qualified test attributes.
The complete marker/token lines are frozen, including potential comment prefixes.
All selected/other test bodies, trailing additions, gates and module declarations
in that suffix must remain byte-identical. Runtime prefix repairs with unchanged
trailing inline tests remain supported. Comments/strings and early attributes,
modules or test-only helpers can conservatively block otherwise safe repairs.
This is not a Rust parser: custom macros without recognized test words and
changes to runtime helpers are not semantic test-preservation guarantees.
Original compiled test listing and exact native execution remain mandatory.
The native phases separately check original test identity, formatting, exact
focused and full-suite execution, and tracked-source mutation. Each rejects all
untracked repository-local paths, including ignored files, before and after
each stage, and rejects changes to the immutable original index path
membership so staging injected files cannot hide them. Physical source
inspection is anchored to the actual checkout, not candidate-mutable
`core.worktree`; source reparse points are rejected. Cargo tests use
tracked locks with `--locked` and separate fresh
external target directories. Fresh hosted VMs prevent state reuse between
native phases, including toolchain mutations and background descendants.
Each Cargo command also uses a fresh external `CARGO_HOME`. Captured ancestor Cargo
configurations also remain unchanged: both config names, including initially
absent files, are checked before and after stages.
Changed ancestor inputs or reparse points block validation. Cold downloads and
builds can exceed each phase's 30-minute validator / 32-minute step deadline, requiring
a blocked outcome and manual handoff. This is not a full host sandbox
for candidate code executed by Cargo, nor proof of behavioral equivalence;
the sealed publication tree and GitHub-recorded result remain separate authority.
PR changes to repository-root `.cargo/config` or `.cargo/config.toml` block
native repair before Cargo runs; unchanged comparison-base configuration
remains supported.
The gate uses Windows case-equivalent root path names even when sealing on Linux.

Native input binding happens independently in every job before any code runs:
trusted proposal validation confirms the sealed tree, then that validator
captures decoded replacement bytes in memory. OriginalListing leaves original
HEAD unchanged. Focused and FullSuite apply only that snapshot upfront, before
any candidate code, without rereading the proposal or rerunning the mutable helper.
Expected source hashes are computed from original bytes plus sealed replacements,
never reset from post-execution observations. Compiled build-script fixtures
overwrite sibling proposal/helper files and verify that alternative bytes
cannot become the tested or published artifact. Local pipeline phases use three
independent checkouts and validator subprocesses with separate original-artifact
downloads. Phase-local toolchain/config markers and an explicitly cleaned-up
background subprocess simulate state/lifecycle isolation; they are not local
fresh VMs or proof of hosted cleanup. This is not an OS memory-injection
or full host-sandbox guarantee.

Repair proposals require exactly one confirmed request for each of the three
native tools; noop still requires exactly one noop.

| Tool/job ID | Mandatory validator phase | Server-recorded phase step |
| --- | --- | --- |
| `validate_performance_original_tests` | `OriginalListing` | `List the exact test on original HEAD` |
| `validate_performance_focused_tests` | `Focused` | `Format and test the exact focused candidate` |
| `validate_performance_repair` | `FullSuite` | `Test the exact candidate full suite` |

All three requests use `confirm: true`. They authorize validation only, not
publication; model requests, native-written files and native receipts never
substitute for the server-recorded proof.

Pinned gh-aw v0.87.10 validates the three custom jobs and emits parallel
agent/detection dependencies with matching output-type conditions. Its actual
schema rejects `timeout-minutes` inside custom safe jobs despite the pinned
reference documenting it; root job timeout overrides are not emitted for these
jobs. No generated lock edits or unsupported overrides are used. Native steps
retain 32-minute limits after a separately bounded 10-minute toolchain install,
and the controller cancels workers after 150 minutes inside its 175-minute
budget. A 35-minute per-native-job timeout requires a
compiler fix/upgrade; it is not claimed as enforced here.

Pending reports reject all model-authored `pass` checks, not just native claims.
Not-run checks are honestly `unavailable`; the publisher appends native success
independently from GitHub's recorded result. Guide-source checks and other
statuses retain their existing contract. These controls were first checked
with local fixtures; the current hosted proof is recorded below.

Authentication is unchanged from localization: `copilot-requests: write`
uses the Actions token. No speculative `COPILOT_GITHUB_TOKEN` requirement.

Both workers explicitly start with model `auto`, with `max-ai-credits: 500`
and `max-daily-ai-credits: 100000`. The per-run setting is gh-aw's main-agent
cap; built-in threat detection retains its separate framework budget.
The existing scope input includes Git-derived
file counts and lines added/deleted, with per-file counts and binary entries
marked unavailable. A direct `git diff --shortstat` also supplies the size
summary in the initial agent prompt. The skill uses size and subsystem risk to assess effort;
no custom model router or mid-run switching mechanism is introduced.

## Local checks

```powershell
node --test .github\skills\pr-performance-review\tests\*.test.mjs
pwsh -NoProfile -File .github\skills\pr-performance-review\tests\RepairValidationPhase.Tests.ps1
pwsh -NoProfile -File .github\skills\pr-performance-review\tests\RepairValidationPhase.Tests.ps1 -AnalysisOnly
gh aw compile ghaw-pr-performance ghaw-pr-performance-fork-guidance --validate --no-check-update
actionlint -shellcheck= -pyflakes= -ignore 'unknown permission scope "copilot-requests"' -ignore 'unexpected key "queue" for "concurrency" section' .github\workflows\ghaw-pr-performance.lock.yml .github\workflows\ghaw-pr-performance-fork-guidance.lock.yml .github\workflows\ghaw-pr-performance-controller.yml
```

Use pinned actionlint 1.7.12. Only its two known field-schema gaps are ignored;
other workflow errors must still fail.

Fixtures check the boundaries; they do not establish native product speedup
or a numerical production-reliability rate. The actual model repair evaluation
is retained in the session's `performance-model-artifacts` directory.

The controller suite also checks the actual compiled pre-agent steps and
model CLI permissions against their Markdown sources. It reproduces the
missing-output-variable failure and checks the permitted shell and report-tool
surfaces. Early report validation uses the scoped MCP tool, not model-side
PowerShell.
Pending proposals display an hourglass, not the green published-fix icon.

## Hosted evidence

### Current three-phase path

Private
`yeelam-gordon/ghaw-pr-performance-private-20261004#5`
ran the reviewed `4e732eb6dad02e45958df80c581511c0024ddc34` implementation.
[Controller 37758597556](https://github.com/yeelam-gordon/ghaw-pr-performance-private-20261004/actions/runs/37758597556)
and [worker 37758613236](https://github.com/yeelam-gordon/ghaw-pr-performance-private-20261004/actions/runs/37758613236)
both succeeded.

| Result | Evidence |
| --- | --- |
| Rust BASE/HEAD analysis | Successful version-2 records, public Rust/Clippy 1.93.0. No C++ projects were selected. |
| Early report feedback | Initial model-authored passing checks were rejected; corrected pending-validation JSON was accepted within the same agent run. |
| Original test identity | A separate Windows job listed the exact existing test without applying candidate changes. |
| Focused candidate | A separate Windows job passed formatting and executed the exact selected test: one passed, zero failed. |
| Complete fixture suite | A separate Windows job executed the full explicit-target suite: one passed, zero failed. |
| HIGH refresh regression | Published as a one-file source-only repair; original inline tests remained unchanged. |
| MEDIUM collection candidate | Advice-only, not edited; caller frequency and user-visible impact remain unmeasured. |
| PR result delivery | Severity-ordered Markdown tables, both run links and the repair link appeared on the validated published head. |

The published commit
[`23c87098fd0e01c70abed0bb1d20edb1cb0ba68e`](https://github.com/yeelam-gordon/ghaw-pr-performance-private-20261004/commit/23c87098fd0e01c70abed0bb1d20edb1cb0ba68e)
has the exact reviewed parent `703570a9644471ca4ca816efd1c02bf4c85e359c`
and tree `f4467c00c94ea050c145a08caa19c7992a44129a`, identical to the sealed
native-tested proposal. Only `tools/wta/src/lib.rs` changed, with three lines
added and eight removed. Auto selected `gpt-5.6-luna`; main-agent usage was
2.885152 AI credits and detection 0.57273, totaling 3.457882.

The private copy changes only controller trigger selection to
`ready_for_review`, preventing publication from starting another trial.
The 500 main-agent and 100000 daily caps match the reviewed workflow.
All private performance workflows were disabled after completion; no automatic
workflow retry or production deployment occurred. This synthetic Rust fixture
does not prove production performance or the separate MSRustup installation.

### Earlier failed trials

| Trial | Outcome |
| --- | --- |
| Controller 37726650291 / worker 37726663401 | Rust analysis and summary delivery succeeded; empty C++ coverage was mistaken for missing Rust analysis, so all native phases were skipped. |
| Controller 37751837734 / worker 37751853969 | Rust analysis and summary delivery succeeded; the second finding omitted its JSON location, so trusted validation failed before native execution. |

Their original artifacts remain preserved; they are not successful validation
evidence for the current path.

### Earlier successful path

[Controller 37410853746](https://github.com/yeelam-gordon/ghaw-pr-performance-private-20261004/actions/runs/37410853746)
and [worker 37410869400](https://github.com/yeelam-gordon/ghaw-pr-performance-private-20261004/actions/runs/37410869400)
passed on private
`yeelam-gordon/ghaw-pr-performance-private-20261004#2`.
The worker actually invoked Copilot and ran the existing Windows test:
`tests::refresh_is_linear_and_preserves_statuses`, one passed and zero failed.

The controller published
[`2ce69e540a9c8970a28cc6f4b0870fc19c1c71e9`](https://github.com/yeelam-gordon/ghaw-pr-performance-private-20261004/commit/2ce69e540a9c8970a28cc6f4b0870fc19c1c71e9).
Its parent is the immutable reviewed head and its tree is
`98439089de28db62394173508d558bc89524c683`, identical to the native-tested
sealed proposal. Only the source loop changed; tests and manifest stayed
unchanged. Auto selected `gpt-5.6-luna`; the entire worker used 3.835089 AI
credits including detection, without a PAT secret.

The private test changes only trigger selection (`ready_for_review` only) and
credit caps (100 per worker, 300 daily); it deliberately prevents repair
publication from starting another worker. Both performance workflows are now
disabled. GitHub's separate automatic Copilot PR review also ran on the ready
event; its cost is not included above.

Official PR review subsequently tightened fork confinement to the structured,
shell-disabled route and required format checking plus the complete WTA test
suite before repair publication. The earlier trial predates the three-phase
architecture; the current trial above provides the new hosted proof.

## Primary references

- Existing localization controller, repair/guide workers and shared skill.
- PR #1073 at `fd8977057152cb3d1ea5383348ed966f13bd6565`: immutable
  preparation, explicit permitted tools, no success-shaped missing checks.
- [Supported custom safe-output jobs](https://github.com/github/gh-aw/blob/v0.87.10/docs/src/content/docs/reference/custom-safe-outputs.md).
- [GraphQL createCommitOnBranch](https://docs.github.com/en/graphql/reference/input-objects#createcommitonbranchinput).
- [Chromium microbenchmark false positives](https://github.com/chromium/chromium/blob/c7ea1cb426922e7541d010aaed7f68e469e674f2/docs/speed/microbenchmark_regressions.md):
  useful measurement discipline, not a native Terminal solution.
