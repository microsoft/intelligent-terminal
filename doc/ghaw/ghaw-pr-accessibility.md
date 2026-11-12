# Native accessibility PR review

The accessibility gh-aw reviews WinUI/XAML/C++ changes and runs the Windows
Axe smoke job on the original PR revision to discover problems. It also
proposes narrowly scoped HIGH fixes without requiring a post-fix build/test
before committing or pushing. Those are separate responsibilities.

The original PR reviewer, human or agent, owns build/test validation and may
revert an individual fix commit. A source-review or patch-policy PASS is not
native accessibility certification.

The source review also classifies TestHost XAML, C++, headers and project files.
TestHost and project files can require review but are not eligible repair paths.
The original daily AI-credit cap is 100,000.

PR automation runs only for branches in the receiving repository. External-fork
PRs are skipped.

## Original-revision CI prerequisite

The default-branch `ghaw-pr-accessibility-controller.yml` wakes on completed
GitHub Actions **PR CI** runs. Only a successful `pull_request` run for the
current open, non-draft, same-repository PR targeting the default branch can
dispatch review. A green unrelated check, a push/manual CI run, a merge SHA,
an earlier head, or a stale run attempt is not sufficient.

The thin `accessibility-ci-admission` action reuses security review's
`validateCiRun`, `validateCiPull` and `validateCiDefinition` foundation
(#1361, with CI path scoping from #1380). It re-reads the run and active workflow
identity, immutable CI implementation trees, commit-associated PR and live
head/base. It does not trust the webhook's PR list or download candidate artifacts.
Path eligibility remains additive: no relevant paths means no model work.
Relevant paths still receive Related / None / Uncertain delta triage; CI
success alone never selects deep review or native discovery.

The worker is dispatched on the default branch with exact CI run/attempt,
minimal PR identity and trusted workflow SHA. Admission rechecks before triage,
deep review, native discovery and final summary, including reusable entrypoints.
Calls from another workflow, candidate workflow revisions, rerunning the same
worker, and duplicate dispatches for an already admitted CI attempt fail closed.
A fresh successful CI attempt has a distinct identity. If the default branch or
PR moves across admission/dispatch, admission fails rather than reviewing a
substituted revision. As with publication, these live checks are not atomic.
Native discovery admission and the final summary may accept this trusted worker's
confirmed repair tip while retaining the original CI identity; that is not CI
validation of the repairs. Unrelated live head changes still fail admission.

PR CI's existing C++ job already builds TestHost and LocalTests in parallel with
Rust. For relevant same-repository, non-draft PRs, it now publishes that existing
output as `ci-accessibility-testhost-RUN-ATTEMPT`; no second product/TestHost
build is added. Producer and admission use the same path classifier. Docs-only
changes do not start CI or payload publication; native harness/workflow changes
are narrowly included in PR CI's existing source/build filters.

The payload combines every recipe mapping with loose AfterBuild runtime content:
the host/App/LocalTests and component DLLs/WinMDs, aggregate resources, XBFs,
agent assets, `taef.png`, and the two x64 Microsoft framework packages.
PDBs and build-only libraries are excluded. A portable recipe replaces absolute
build-machine paths; a versioned inventory hashes every file and binds repository
ID/name, exact head, CI run and attempt, Debug configuration and x64 platform.
Stale loose copies that disagree with recipe inputs fail staging.

Selected native discovery revalidates the successful C++ producer and upload
step for the exact attempt, then authenticates artifact ID/name/run/head/repository,
expiry and GitHub's SHA-256 digest using the shared security artifact contract.
It downloads the raw archive with read-only credentials. Trusted consumer code
checks the archive digest, provenance, completeness and per-file hashes before
extracting, rejecting traversal, drive/ADS/device paths, symlinks, collisions and
oversized archives. Missing/skipped producers or invalid/missing artifacts fail
closed: **no rebuild fallback and no PASS**. A confirmed repair tip is never
the payload's source or native scan revision.

CI's security finding artifacts are not accessibility analysis or Axe results.
No applicable existing static analyzer is being newly published or invented.
Axe remains a selected interactive Windows runtime consumer of the original
CI-built payload, not certification from CI success or proposed-fix validation.

Admission also checks successful C++/Rust build/test steps, not just a green
workflow name. GitHub's REST run PR fields are mutable; target history is checked
through typed GraphQL timeline events. Retargeted/base-force-pushed PRs fail
closed for manual review/new PR because immutable original-target provenance
cannot be inferred from queue timestamps. The controller's `github-actions[bot]`
is explicitly allowlisted in reusable triage/summary authorization; CI admission
still gates all model jobs.

## Responsibilities

| Owner | Responsibility |
| --- | --- |
| Workflow Markdown | PR trust boundaries, immutable revisions, publication policy and the summary template |
| `pr-accessibility` skill | Evidence-backed domain review across names, keyboard/focus, announcements, busy state, visual behavior, localization and affected UI operations |
| Existing Python validator | Source/report identity, patch scope, eligible recipe and one-finding commit attribution |
| Agent | Review judgment, each proposed repair commit, JSON evidence and the Markdown summary |
| PR CI C++ job | Reuse its existing TestHost build; publish a complete original-head runtime payload only for relevant PRs |
| Windows native job | Authenticate/consume the CI payload and scan the original immutable PR head; retain results and enforce failures |
| Original PR reviewer | Validate proposed fix commits through builds/tests; accept or revert |

There is no separate summary-rendering script. The agent follows the template
in `.github/workflows/ghaw-pr-accessibility.md`; the job displays its Markdown
unchanged and retains it with the structured report. Safe-output processing
shows whether publication succeeded. Do not mistake a local commit or queued
push for confirmed remote publication.

## Repair and summary policy

- Only same-repository, high-confidence HIGH findings are eligible.
  MEDIUM/LOW findings are not automatic repairs.
- Current repair eligibility stays limited to the trusted
  `AXSTATIC001-remove-raw-view` recipe: remove only the prepared control's raw
  UIA-view property, with an existing content/resource name source.
- Make one commit per eligible finding, not one combined repair commit.
  Each fixed finding's table row links to its own commit ID.
- Put one shared notice below the table: proposed fixes were committed without
  building/testing; the original reviewer owns validation and may revert any
  fix commit. Do not repeat it per finding.
- Keep unrelated files/hunks, dependency changes and resource edits out.
  New customer-facing strings belong to localization.
- Preserve unresolved findings and exact runtime scenarios. Missing native
  evidence is not PASS and must not be disguised as a build/test result.

This policy does not require builds/tests as a substitute for confidence.
Uncertain or unsupported repairs stay `remaining` or `blocked`.

## Trust and publication

The `workflow_run` controller and dispatched worker use default-branch
definitions, never a PR merge commit's operating code. Fork PRs are skipped.
Preparation reads immutable Git blobs from `github.workflow_sha`, records that
code revision as `trusted_base_sha` separately from feature `comparison_base_sha`,
and stages its skill/analyzer in gh-aw's read-only runtime mount. The Windows
harness uses the same workflow revision. A PR event's older base commit can
predate installation of this workflow and must not select its trusted code.
Preparation receives the PR base separately for feature comparison. The trusted
workflow revision is control code, not the feature's comparison ancestor.
PR instructions and artifacts are review data, never operating instructions.

After preparation, the agent inspects the verified PR head branch. Operating
configuration changes fail closed; a separate trusted review must handle them.
The plain spelling dictionaries `allow/allow.txt` and `expect/expect.txt` under
`.github/actions/spelling` are data, not operating instructions. Changing them
does not bypass review of changed UI; hooks, action definitions and agent
instructions remain blocked.
The model does not execute PR code. Each required review dimension cites exact
immutable source, including companion resources/peers/handlers when needed.
Every static signal becomes a finding or an explained false-positive dismissal.
Coverage/provenance checks do not authenticate the model's semantic judgment.

The post-step checks the live PR head, report, allowed files, severity/confidence,
exact repair recipe and captured commit series. Repaired files retain regular
file type, mode, encoding and line endings. Untracked, extra and stale changes
are rejected. These are permission/patch checks, not product tests.

Before the agent starts, the workflow records configuration changes made by
framework restoration in the read-only runtime mount. Before candidate
validation, it reverses that exact setup patch. Modified setup hunks or
committed configuration fail closed; new out-of-scope files and actual repairs
still pass through the unchanged validator.

A repair run requests one branch push for the complete series. A report-only
run requests `noop`. There is no second PR-comment/review output: comments and
code publication are not atomic in gh-aw. Non-fast-forward publication fails;
the workflow does not create a fallback PR.
Bundle transport with publisher re-signing disabled preserves the actual commit
IDs used in the agent's table. The validator checks one linear commit per fixed
finding in report order, including multiple findings in the same source file,
and verifies the captured bundle matches that exact commit series and tree.

The freshness check is not a transactional compare-and-swap with publication.
The PR head can still move afterward; do not claim stronger atomicity.

## Windows problem discovery

The restored `native-runtime` job calls `native-accessibility.yml` with exact
head and trusted-source identities, without inheriting source-agent secrets.
It consumes CI's built TestHost and scans FRE, FRE settings after Next, and the real Agents
page reached through production Frame navigation. Native errors, blocked startup and missing state evidence
remain failures. Its artifacts and job outcome are separate from the source
agent's Markdown and source-only runtime records.

Native discovery follows the source agent and waits for safe-output publication
to finish, avoiding a live-head admission race with this worker's own repairs.
Only the publisher's confirmed `push_commit_sha` reaches native admission.
Failed or skipped publication does not suppress selected discovery; cancellation
does. The publication job does not depend on native discovery, and the native
job still scans the event's original head, not a proposed repair commit.
It is not post-fix validation.

This smoke is not complete keyboard, Narrator, high-contrast or 200% scaling
coverage. Axe output uses its supported no-screenshot format; JSON is not an
`.a11ytest` recording. TestHost uses the production view model and
`NavigateToPageArgs` constructor with normal Frame navigation; no special
Agents-page constructor is needed. The full Settings MainPage requires desktop
filesystem access that this AppContainer test host does not have.
Launch modes and local safety requirements are in
`src/cascadia/LocalTests_TerminalApp/TestHostApp/AGENTS.md`. These fixtures do not
establish persistence, agent/session operations or full terminal-window behavior.

## Workflow maintenance

When changing this workflow, check its own policy/transport contracts and
regenerate the lockfile:

```powershell
python -m unittest discover -s .github/skills/pr-accessibility/tests -p "test_*.py"
python build/scripts/test_ci_accessibility_payload.py
node --test .github/actions/accessibility-ci-admission/admission.test.mjs
gh aw compile ghaw-pr-accessibility ghaw-accessibility-triage ghaw-accessibility-summary --no-check-update
gh aw validate ghaw-pr-accessibility ghaw-accessibility-triage ghaw-accessibility-summary --no-check-update
```

Use gh-aw v0.87.10 and commit all three generated lock files. The Node tests use
mocked GitHub APIs (identity, status, freshness, attempts, duplicates and manual
guards); the Python tests inspect the compiled graph and existing repair policy.
They do not establish hosted Actions pickup or end-to-end dispatch behavior.

The payload fixture tests cover archive traversal/collisions, wrong identity,
missing/changed runtime files, framework versions and portable recipe mappings.
These commands validate the workflow implementation; they do not run product
builds/tests on a PR repair.
