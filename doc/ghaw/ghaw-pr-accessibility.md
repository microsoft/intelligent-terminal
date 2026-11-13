# Native accessibility PR review

The accessibility gh-aw reviews WinUI/XAML/C++ changes using original-head
Windows Axe evidence from PR CI. It also
proposes narrowly scoped HIGH fixes without requiring a post-fix build/test
before committing or pushing. Those are separate responsibilities.

The original PR reviewer, human or agent, owns build/test validation and may
revert an individual fix commit. A source-review or patch-policy PASS is not
native accessibility certification.

The source review also classifies TestHost XAML, C++, headers and project files.
TestHost and project files can require review but are not eligible repair paths.
The daily AI-credit cap is 100,000.

PR automation runs only for branches in the receiving repository. External-fork
PRs are skipped.

## Original-revision CI prerequisite

The default-branch `ghaw-pr-accessibility-controller.yml` wakes on completed
GitHub Actions **PR CI** runs. Only a successful `pull_request` run for the
current open, non-draft, same-repository PR targeting the default branch can
dispatch review. A green unrelated check, a push/manual CI run, a merge SHA,
an earlier head, or a stale run attempt is not sufficient.

The thin `accessibility-ci-admission` action reuses security review's
`validateCiRun`, `validateCiPull`, `validateCiFreshness` and
`validateCiDefinition` foundation. It re-reads the run and active workflow
identity, immutable CI implementation trees, commit-associated PR and live
head/base. It does not trust the webhook's PR list or download candidate artifacts.
Path eligibility remains additive: no relevant paths means no model work.
Relevant paths still receive Related / None / Uncertain delta triage; CI
success alone never selects deep review.

The worker is dispatched on the default branch with exact CI run/attempt,
minimal PR identity and trusted workflow SHA. Admission rechecks before triage,
deep review, CI evidence consumption, repair publication and final summary,
including reusable entrypoints.
Calls from another workflow, candidate workflow revisions, rerunning the same
worker, and duplicate dispatches for an already admitted CI attempt fail closed.
A fresh successful CI attempt has a distinct identity. If the default branch or
PR moves across admission/dispatch, admission fails rather than reviewing a
substituted revision. As with publication, these live checks are not atomic.
The final summary may accept this trusted worker's
confirmed repair tip while retaining the original CI identity; that is not CI
validation of the repairs. Unrelated live head changes still fail admission.

The worker shares security review's native `PR CI-PR_NUMBER` concurrency group
with `cancel-in-progress: false` and `queue: max`. New PR CI requests use that
same group with cancellation enabled; workers do not cancel CI or unrelated
runs. GitHub owns cancellation and queue scheduling, with no polling controller.
Admission rejects an updated PR head, changed CI attempt or a newer same-head
PR CI workflow run, including queued/in-progress runs. Workflow-scoped numeric
run numbers establish order; unrelated workflows and source repositories do not.
These checks do not make admission or publication atomic.

PR CI builds production Terminal, TestHost and C++ tests once in **C++ build**,
in parallel with Rust. Ordinary **C++ unit tests** consume the staged unit-test
payload; path-gated **Native accessibility / Native Axe.Windows smoke** consumes
`ci-accessibility-testhost-RUN-ATTEMPT`. Both are sibling consumers after the
build, not serial dependencies. Producer and admission use the same conservative
path classifier. Docs-only changes do not start native CI. Every accessibility
model, including triage and summary, waits for authoritative entire PR CI success.
There is no Windows job or rebuild fallback in gh-aw.

Ordinary CI consumers carry the C++ producer attempt explicitly. A failed-job-only
rerun can reuse its retained build artifact without pretending it was rebuilt in
the new attempt; native receipts still identify the actual scan attempt.
Model admission remains stricter: retained older build jobs do not satisfy the
fresh exact-attempt prerequisite. A full fresh CI attempt is needed for review.

The payload combines every recipe mapping with loose AfterBuild runtime content:
the host/App/LocalTests and component DLLs/WinMDs, aggregate resources, XBFs,
agent assets, `taef.png`, and the two x64 Microsoft framework packages.
PDBs and build-only libraries are excluded. A portable recipe replaces absolute
build-machine paths; a versioned inventory hashes every file and binds repository
ID/name, exact head, CI run and attempt, Debug configuration and x64 platform.
Stale loose copies that disagree with recipe inputs fail staging.

CI's native job records original-head scans, uploads its result archive as
`ci-accessibility-native-RUN-ATTEMPT`, then enforces scan success. A Linux
`native-evidence` job repeats admission and authenticates the exact successful
native job and its record/upload/enforcement steps, plus artifact ID, name,
run/head/repository, expiry, bounded size and GitHub SHA-256 digest. It downloads
that exact ZIP with a read-only token. Trusted `extract-native` checks the digest,
bounded paths/links/collisions, root `native-ci.json` provenance, complete inventory
and hashes, individual surface identities and PASS receipt before extraction.
The receipt binds repository name/ID, original head, CI run/attempt and PR number.
Raw per-surface result, Axe and UIA tree JSON remain untrusted evidence, never
instructions. Only validated files enter the worker's
`accessibility-native-ci-WORKER_RUN` artifact, then deep review and final summary.
Model-authored triage downloads into a separate `triage-input` directory, never
over the verified native directory. Its producer publishes only the Markdown
handoff and producer receipt, not the whole model working directory.
Missing, failed, skipped or invalid results mean **review incomplete**, never
PASS. A confirmed repair tip is never the native result source.

CI's security finding artifacts are not accessibility analysis or Axe results.
No applicable existing static analyzer is being newly published or invented.
Axe runs on an interactive Windows CI desktop using the original CI-built
payload, not as proposed-fix validation. Deep review can select whether those
existing scans inform its changed-UI scope; it cannot start or skip them.
Related / None / Uncertain is the routing contract. None skips the
deep model only, not independent path-gated CI scan cost or final reporting.

Admission also checks successful C++/Rust build/test steps, not just a green
workflow name. Relevant PRs also require successful native receipt, upload and
enforcement steps before every model entry, not only before archive consumption.
GitHub's REST run PR fields are mutable; target history is checked
through typed GraphQL timeline events. PRs with changed targets or force-pushed bases fail
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
| PR CI C++ job | Build once; publish ordinary unit-test and relevant original-head TestHost payloads |
| PR CI sibling consumers | Run ordinary C++ units and path-gated native scans independently after the build |
| Linux gh-aw evidence consumer | Re-admit entire successful CI; authenticate and extract exact original-head native results before model access |
| Original PR reviewer | Validate proposed fix commits through builds/tests; accept or revert |

There is no separate summary-rendering script. The agent follows the template
in `.github/workflows/ghaw-pr-accessibility.md`; the job displays its Markdown
unchanged and retains it with the structured report. Safe-output processing
shows whether publication succeeded. Do not mistake a local commit or queued
push for confirmed remote publication.

Every AI stage must emit its own non-empty file handoff. Missing, empty,
incomplete or crashed production is **No result**. Files left by a failed
producer are diagnostic evidence, not completed analysis. The final model
receives actual files, producer receipts and job outcomes; it cannot invent a
missing decision or report. If the final model fails, Actions exposes the failed
job and artifacts without rendering a replacement AI summary. No automatic
model retry is enabled.

The existing gh-aw `agent` artifact is a downloadable diagnostic ZIP containing
session logs, SDK events and stdio, using the framework's secret redaction.
Reusable stages use the native artifact prefix. Producer receipts bind stage,
source, head, run and attempt, and record actual SDK-created session IDs or
`unknown`; only those owned session directories remain in native capture.
Abrupt runner loss can prevent best-effort uploads.

### Executable responsibilities

| File | Single purpose | Disposition |
| --- | --- | --- |
| `.github/actions/accessibility-ci-admission/admission.mjs` | Authenticate CI producer steps/artifacts | Necessary; reuses shared security identity/freshness validators |
| `build/scripts/ci-accessibility-scope.mjs` | One pure CI/admission path predicate | Reused by both owners |
| `build/scripts/ci-accessibility-payload.py` | Stage/verify portable payloads, bounded archives, receipts and hashes | Necessary transport boundary |
| `.github/skills/pr-accessibility/scripts/accessibility_handoff.py` | Validate actual handoffs and record producer/diagnostic identity | Reused across all AI stages |
| `.github/skills/pr-accessibility/scripts/accessibility_review.py` | Prepare immutable source evidence and validate authorized repairs/commit series | Necessary repair boundary |
| `test/accessibility/Invoke-AxeWindowsTestHost.ps1` | Own TestHost deployment/process lifetime and invoke scanner | Necessary native lifetime owner |
| `test/accessibility/Invoke-AxeWindowsScan.ps1` | Collect per-state UIA/Axe evidence and enforce production-sidebar identity | Necessary native scan owner |

These are separate trust or execution boundaries. CI identity helpers, native
diagnostic ZIP/redaction and queue cancellation are reused, not duplicated.
Tests stay with these owners; no test-only runner or recovery engine is needed.
Admission consumes native receipts only; it has no TestHost-payload dispatch mode.

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
and stages its skill/analyzer in gh-aw's read-only runtime mount. CI's Windows
harness is pinned to the immutable CI workflow revision, not a caller-selected
head/ref; candidate binaries run without
secrets. This is not a privileged attestation from an untrusted PR build.
Admission rejects changed CI implementation through the security foundation's
strict immutable definition comparison, including both native harness scripts
and their dependency helper. Changed harness/CI code cannot become a model's
authoritative prerequisite until installed on the default branch.
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

PR CI calls `native-accessibility.yml` after the C++ build, without model
credentials or inherited secrets. It scans FRE, FRE settings after Next, and
the real Agents page reached through production Frame navigation, and Sidebar
through the existing `--visual-page=sidebar` TestHost route. Sidebar loads
production TabStrip/TabRowControl and requires a visible `FixtureSidebar`, its
descendant `ItemsList` and visible `TreeItem` tab rows in the activated PID's
hierarchy. FRE's `welcomeSidebarImage` and empty window roots are not accepted.
Native errors,
blocked startup and missing state evidence remain failures. Upload occurs before
enforcement, retaining failure evidence without admitting unsuccessful CI to
models. The gh-aw Linux consumer never launches Windows or repeats a scan.

Deep review waits for verified CI evidence; final summary runs after worker
skips/failures and reports consumption failures as incomplete. Summary cites the
receipt's original CI identity, not its own run, and reports native evidence even
when triage is None or deep review selects Native checks: None. Proposed repair
publication has no post-fix build/test/native gate.

This smoke is not complete keyboard, Narrator, high-contrast or 200% scaling
coverage. Sidebar is a deterministic synthetic tab/pane control fixture; this
scan does not certify populated history, session routing/persistence,
terminal-window or lifecycle behavior. Fixture toolbar controls are also within
the scanned window. Error-level rules alone are reported; Warning and NeedsReview
coverage is explicitly Unknown, not zero or PASS.
Axe output uses its supported no-screenshot format; JSON is not an
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

The payload/native fixture tests cover archive traversal/collisions, wrong identity,
missing/changed runtime files, framework versions and portable recipe mappings.
These commands validate the workflow implementation; they do not run product
builds/tests on a PR repair.
