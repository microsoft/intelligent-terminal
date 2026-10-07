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
node --test .github\skills\pr-performance-review\tests\performance-review.test.mjs .github\scripts\ghaw-pr-performance\controller.test.mjs .github\scripts\ghaw-pr-performance\runtime.test.mjs .github\scripts\ghaw-pr-performance\publish-repair.test.mjs .github\scripts\ghaw-pr-performance\guide-report.test.mjs
pwsh -NoProfile -File .github\scripts\ghaw-pr-performance\NativeValidation.Tests.ps1
node --test .github\scripts\ghaw-pr-performance\repair-pipeline.test.mjs
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
