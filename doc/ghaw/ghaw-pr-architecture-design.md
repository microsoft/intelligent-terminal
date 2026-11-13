# Architecture and design PR review

`ghaw-pr-architecture-design.md` provides a read-only review of material
same-repository PR changes. It uses trusted `pull_request_target` workflow code,
the existing gh-aw actor authorization (`admin`, `maintainer`, `write`),
immutable Git inspection and built-in comment publication with threat detection.
Forks and drafts are not admitted. Candidate source is fetched but never checked
out or executed. Candidate workflow/agent/skill documents remain review data,
not active instructions.

## Admission before inference

The deterministic `prepare` job verifies live PR identity, fetches the exact
head and observed base, resolves their merge base and reads Git numstat metadata.
No model runs unless **relevant added+deleted lines >= 100 OR relevant changed
files >= 5**. Equality passes; either threshold suffices independently.
Tune only the two constants in
`.github/workflows/ghaw-pr-architecture-design/scripts/architecture-design-review.mjs`.

These are provisional effort/cost defaults, not an architectural-risk score:
100 lines admits a material single-owner change; five files admits a
distributed contract change even with small per-file edits. Small architectural
changes can matter; request a local review when they fall below this cost gate.

Counts include source, tests, settings, project manifests, workflow sources,
agents, skills, instructions and Markdown. The narrow excluded set is:

- Root `dep`, `oss`, `vendor`, `node_modules` and `Generated Files` trees.
- Dependency lock basenames `Cargo.lock`, `package-lock.json`,
  `npm-shrinkwrap.json`, `yarn.lock`, `pnpm-lock.yaml`, `packages.lock.json`.
- Compiler-owned `.github/workflows/*.lock.yml` (not their Markdown sources).

All other paths count, including unknown or generated-looking source; a naming
heuristic must not conceal substantive contracts. Exclusions affect admission
only: relevant supporting changes remain inspectable during review.
Deletions contribute deleted lines and one file. Git's explicit 50% rename
detection counts a detected rename once, including either relevant path; a
pure rename contributes zero lines. A rewrite below Git's rename similarity
threshold counts as separate deletion/addition. Binary changes contribute one
file and zero lines because no textual count exists. Invalid, duplicate,
overflowed, incomplete or oversized metadata fails the job, never a clean
assessment. Git output is bounded to 16 MiB and 10,000 change records.

Below-threshold changes produce only a run summary explaining that review and
publication were skipped, not an approving/no-findings PR comment.

## Responsibility and evidence

`Architecture and Design Reviewer` is a thin, repository-agnostic read-only
role. `architecture-design-review/SKILL.md` is the single methodology/report
owner, reusable for local or hosted review. The workflow owns admission,
immutable context and delivery; the script owns deterministic metadata logic.
No custom model driver, dependency graph job, repair path or configuration
framework is needed. Available code intelligence, Git/source inspection and
project metadata suffice; build graphs alone would not prove runtime ownership
or MVVM state flow.

Unlike the existing repair workflows, this source-only advisory workflow does
not require native PR CI. `pr-ci.yml` does not trigger for general `.github`
or documentation changes; requiring it would prevent reviewing declarative
architecture. The workflow neither executes candidate code nor claims
build/test success, and does not alter native CI or its gates.

Preparation revalidates the PR before and after fetching, and again before
inference. A separate freshness job checks live head/base, refs, repositories,
open state and draft status before the built-in safe-output handler publishes
one non-blocking comment. Detection failure prevents publication. Every report
identifies its exact comparison base/head and immutable citations. GitHub
comment creation has no atomic head-SHA precondition: an update after the final
freshness check can still leave a clearly snapshot-labeled historical report.
No source approval or absolute regression guarantee is implied.

PR #1399's latest CI-controller fix separates requested/completed event
concurrency and rechecks the live PR immediately before cancelling an older
security worker. This advisory workflow has no detached worker or CI-event
controller: its PR-number concurrency cancels an older run directly on
`synchronize`, even when the new head is below threshold. Freshness checks
prevent stale publication. It therefore does not depend on #1399 or on a
native CI trigger to review declarative changes.

## Local validation

From the repository root:

```powershell
node --test .github\workflows\ghaw-pr-architecture-design\tests\*.test.mjs
gh aw compile ghaw-pr-architecture-design --no-check-update --actionlint
markdownlint -c .github\linters\.markdown-lint.yml .github\agents\architecture-design-reviewer.agent.md .github\skills\architecture-design-review\SKILL.md doc\ghaw\ghaw-pr-architecture-design.md
```

The tests cover boundaries, generated-only churn, declarative changes,
renames/deletions/binaries, invalid metadata and immutable identity. Contract
evaluation must distinguish fixture/static inspection from actual model
execution; neither substitutes for hosted authorization, detection and
publication E2E. Generated locks are emitted only by `gh aw compile`.

With compiler v0.87.10, compilation warns generically about the target-event
checkout: inspect the generated lock to confirm every workspace checkout uses
the trusted workflow revision and there is no PR checkout. Newer reference
documentation describes checkout options that this installed version rejects;
they are not used here. Native actionlint v1.7.12 does not yet recognize
`copilot-requests` or the compiler-owned concurrency `queue` key. Lint the lock
with only these two exact diagnostics ignored:

```powershell
actionlint -shellcheck= -pyflakes= -ignore 'unknown permission scope "copilot-requests"' -ignore 'unexpected key "queue" for "concurrency" section' .github\workflows\ghaw-pr-architecture-design.lock.yml
```

This command checks workflow structure, expressions and scripts but does not
run external shellcheck/pyflakes. It does not suppress other lint failures.

Official references: [triggers](https://github.github.com/gh-aw/reference/triggers/),
[checkout](https://github.github.com/gh-aw/reference/checkout/),
[safe outputs](https://github.github.com/gh-aw/reference/safe-outputs/).
