# Architecture and design PR review

The workflow reviews material same-repository PRs using the architecture
reviewer/skill, read-only Git inspection and native gh-aw comment
publication with threat detection. It provides advice, not approval or repairs.
Trusted `pull_request_target` code authorizes `admin`, `maintainer` and `write`
actors. Fork PRs and drafts are excluded.

## Admission before inference

One small helper reads GitHub's paginated PR changed-files API and admits
**100 relevant added+deleted lines OR five relevant files**, inclusively.
Only the two constants in
`.github/workflows/ghaw-pr-architecture-design/scripts/architecture-design-review.mjs`
define those thresholds. These are cost controls, not architectural-risk scores.

The counting exclusions are:

- Root `dep`, `oss`, `vendor`, `node_modules` and `Generated Files` trees.
- Dependency locks: `Cargo.lock`, `package-lock.json`, `npm-shrinkwrap.json`,
  `yarn.lock`, `pnpm-lock.yaml` and `packages.lock.json`.
- Compiler-owned `.github/workflows/*.lock.yml`, not workflow Markdown.

Other source, tests, settings, project files, agents, skills and documentation
count. Exclusions affect counting only, never research scope. A reported rename
counts once and remains relevant if either path is not excluded. Deletions
contribute their deleted lines; binary files contribute a file and GitHub's
reported line counts.

The helper checks the returned file count against PR metadata and rejects
incomplete results, invalid counts and API errors. GitHub caps this endpoint at
3,000 files; larger PRs fail explicitly rather than receive a partial or clean
assessment. This is an API limitation, not an architectural review-depth policy.
The comparison merge base comes from GitHub's commit comparison API.

Both activation and review depend on admission. Below threshold, the run summary
explains that inference and publication were skipped; no clean-assessment
comment is posted.

## Research and delivery

The agent defines the architect role; the skill owns the goal, source-led
research method and report contract. The workflow supplies the PR, revisions,
tools and authorized result destination. Local invocation returns Markdown
unless the caller authorizes another destination.

As in Accessibility Review, trusted workflow code stays checked out and a
trusted runner step fetches the PR commits separately. It fetches only the two
exact review revisions, at depth one. Standard Git commands write the patch
and materialize both source trees in an isolated, read-only mount. The reviewer
uses ordinary file reads and search, including for unchanged callers. Neither
tree replaces the trusted working directory. There is no custom reader,
evidence server or model shell.

Missing required evidence is reported with the native incomplete signal rather
than a PR comment. The publication check fails an incomplete or missing report.

The helper verifies live PR state, repositories, refs and revisions around
admission. After detection and artifact setup, the same validator checks again
immediately before native publication. Validation or detection failure prevents
the comment. PR-number concurrency cancels older runs on `synchronize`,
independently of the replacement's size.

The workflow requests one snapshot-labelled Markdown comment with immutable
source citations. GitHub offers no atomic head-SHA precondition for comment
creation: a change after the final check can still leave a clearly labelled
historical report. No absolute freshness or regression guarantee is implied.
Hosted authorization, tool startup, inference, detection and publication still
require end-to-end execution evidence; compilation and local tests are not that.

## Local validation

```powershell
node --test .github\workflows\ghaw-pr-architecture-design\tests\*.test.mjs
gh aw compile ghaw-pr-architecture-design --no-check-update
markdownlint -c .github\linters\.markdown-lint.yml .github\agents\architecture-design-reviewer.agent.md .github\skills\architecture-design-review\SKILL.md doc\ghaw\ghaw-pr-architecture-design.md
actionlint -shellcheck= -pyflakes= -ignore 'unknown permission scope "copilot-requests"' -ignore 'unexpected key "queue" for "concurrency" section' .github\workflows\ghaw-pr-architecture-design.lock.yml
```

Compiler v0.87.10 emits the lock; never hand-edit it. Its generic
`pull_request_target` checkout warning must be checked against the trusted refs
actually emitted. Native actionlint v1.7.12 needs only the two exact extension
exceptions above; shellcheck and pyflakes are not run by that command.

References: [GitHub tools](https://github.github.com/gh-aw/reference/github-tools/),
[safe outputs](https://github.github.com/gh-aw/reference/safe-outputs/),
[changed-file API limits](https://docs.github.com/en/rest/pulls/pulls#list-pull-requests-files).
