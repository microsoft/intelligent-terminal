# Performance review workflow

This separate gh-aw workflow reviews Intelligent Terminal hot paths and can
repair small, strongly evidenced HIGH WTA regressions. The agent and reusable
skill own the actual analysis; mechanical steps enforce scope and publication.

**Hosted full-path status: passed on the controlled private WTA fixture.**
After three failed worker trials and their contract corrections, the two
additional authorized performance runs verified the controller, actual Copilot
agent/skill, Windows test, and immutable-head publication. This is a scoped
end-to-end proof, not a claim that every native performance scenario is covered.

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
4. A read-only Windows Actions job resolves the exact test in the immutable
   original head before applying the proposal. It then checks formatting,
   runs that test with exact matching, and runs the full explicit-target suite.
   It rejects failed/zero tests and tracked-source mutation.
   Actions owns process cleanup; there is no custom Windows process manager
   or native receipt file.
5. The trusted controller requires GitHub-recorded native-job success, checks
   the original sealed proposal, and publishes its exact tree with immutable
   `expectedHeadOid`. It never rebases or retries a stale repair.

For every applicable review, the controller publishes a **Performance review**
check with direct controller and agentic-worker report links, including failed
outcomes. After a repair, the check is attached to the validated published
head, following localization's completion-check pattern; otherwise it belongs
to the immutable reviewed head. No manual PR-description update or additional
agent comment is required.

The Windows test job has no model or branch-write credentials. Its files are
not publication authority: the publisher consumes the immutable Linux-sealed
proposal and GitHub's job result. Same-repo and fork workers cannot both
comment and commit.

The fork agent has no shell or edit permission. It reads the prepared patch
and immutable source through read-only tools, validates structured report data
through the declared checker, and queues that JSON as its safe-output body.
Trusted post-processing validates the data and renders the comment; fork
content is never executed to produce the report.

## Evidence and limitations

HIGH requires high confidence plus measurement or source proof of complexity,
blocking, or resource lifetime. MEDIUM/LOW are advice-only. A microbenchmark
is not end-to-end proof; noisy measurements need samples and spread.

Use existing WPR/ConsoleBench, pane-context and hook-overhead facilities when
relevant. Missing measurement is unavailable, not a passing benchmark.
The current automatic native backend supports focused Windows WTA Rust tests;
C++ findings require an applicable MSBuild/TAEF handoff and remain manual.
Fork guidance names the reviewed head and uses best-effort freshness checking.

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
The native job checks original test identity, formatting, exact focused and
full-suite execution, and tracked-source mutation. It also rejects all
untracked repository-local paths, including ignored files, before and after
each stage, and rejects changes to the immutable original index path
membership so staging injected files cannot hide them. Physical source
inspection is anchored to the actual checkout, not candidate-mutable
`core.worktree`; source reparse points are rejected. Cargo tests use
tracked locks with `--locked` and separate fresh
external target directories to prevent mutable artifact reuse between stages.
These cold builds can exceed the unchanged total 30-minute deadline, requiring
a blocked outcome and manual handoff. This is not a full host sandbox
for candidate code executed by Cargo, nor proof of behavioral equivalence;
the sealed publication tree and GitHub-recorded result remain separate authority.

Native input binding happens before any candidate code runs: trusted proposal
validation confirms the sealed tree, then the parent validator captures decoded
replacement bytes in memory. Following original-head listing, it applies only
that snapshot without rereading the proposal or rerunning the mutable helper.
Expected source hashes are computed from original bytes plus sealed replacements,
never reset from post-execution observations. Compiled build-script fixtures
overwrite sibling proposal/helper files and verify that alternative bytes
cannot become the tested or published artifact. This is not an OS memory-injection
or full host-sandbox guarantee.

Pending reports reject all model-authored `pass` checks, not just native claims.
Not-run checks are honestly `unavailable`; the publisher appends native success
independently from GitHub's recorded result. Guide-source checks and other
statuses retain their existing contract. This round has local fixture evidence
only; no additional hosted replay was authorized or performed.

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
node --test .github\skills\pr-performance-review\tests\performance-review.test.mjs .github\scripts\ghaw-pr-performance\controller.test.mjs .github\scripts\ghaw-pr-performance\runtime.test.mjs .github\scripts\ghaw-pr-performance\publish-repair.test.mjs .github\scripts\ghaw-pr-performance\repair-pipeline.test.mjs .github\scripts\ghaw-pr-performance\guide-report.test.mjs
pwsh -NoProfile -File .github\scripts\ghaw-pr-performance\NativeValidation.Tests.ps1
gh aw compile ghaw-pr-performance ghaw-pr-performance-guide-forkedrepo --validate
```

Fixtures check the boundaries; they do not establish native product speedup
or a numerical production-reliability rate. The actual model repair evaluation
is retained in the session's `performance-model-artifacts` directory.

The controller suite also checks the actual compiled pre-agent steps and
model CLI permissions against their Markdown sources. It reproduces the
missing-output-variable failure and requires the permitted PowerShell route.
Pending proposals display an hourglass, not the green published-fix icon.

## Hosted evidence

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
suite before repair publication. Those changes have local execution/compiled
contract coverage, but are not represented as another hosted replay.

## Primary references

- Existing localization controller, repair/guide workers and shared skill.
- PR #1073 at `fd8977057152cb3d1ea5383348ed966f13bd6565`: immutable
  preparation, explicit permitted tools, no success-shaped missing checks.
- [Supported custom safe-output jobs](https://github.com/github/gh-aw/blob/v0.87.10/docs/src/content/docs/reference/custom-safe-outputs.md).
- [GraphQL createCommitOnBranch](https://docs.github.com/en/graphql/reference/input-objects#createcommitonbranchinput).
- [Chromium microbenchmark false positives](https://github.com/chromium/chromium/blob/c7ea1cb426922e7541d010aaed7f68e469e674f2/docs/speed/microbenchmark_regressions.md):
  useful measurement discipline, not a native Terminal solution.
