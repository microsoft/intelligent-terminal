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

PR CI currently publishes `ci-security-cpp-*` and `ci-security-rust-*` findings,
not a supported TestHost package, Axe result or accessibility static report.
Accessibility does not consume those unrelated artifacts or claim CI certified
Axe. Selected native discovery therefore still builds TestHost from the exact
original head, under read-only permissions. No new build artifact contract is
introduced here, and no proposed-fix build/test gate is added.

## Responsibilities

| Owner | Responsibility |
| --- | --- |
| Workflow Markdown | PR trust boundaries, immutable revisions, publication policy and the summary template |
| `pr-accessibility` skill | Evidence-backed domain review across names, keyboard/focus, announcements, busy state, visual behavior, localization and affected UI operations |
| Existing Python validator | Source/report identity, patch scope, eligible recipe and one-finding commit attribution |
| Agent | Review judgment, each proposed repair commit, JSON evidence and the Markdown summary |
| Windows native job | Build and scan the original immutable PR head; retain scan results and enforce failures |
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
It builds TestHost and scans FRE, FRE settings after Next, and the real Agents
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
python .github\skills\pr-accessibility\tests\test_accessibility_review.py
python .github\skills\pr-accessibility\tests\test_accessibility_handoff.py
node --test .github/actions/accessibility-ci-admission/admission.test.mjs
gh aw compile ghaw-pr-accessibility ghaw-accessibility-triage ghaw-accessibility-summary --no-check-update
gh aw validate ghaw-pr-accessibility ghaw-accessibility-triage ghaw-accessibility-summary --no-check-update
```

Use gh-aw v0.87.10 and commit all three generated lockfiles. The Node tests use
mocked GitHub APIs (identity, status, freshness, attempts, duplicates and manual
guards); the Python tests inspect the compiled graph and existing repair policy.
They do not establish hosted Actions pickup or end-to-end dispatch behavior.

These commands validate the workflow implementation; they do not run product
builds/tests on a PR repair.
