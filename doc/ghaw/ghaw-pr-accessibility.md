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

- Only same-repository, high-confidence HIGH findings are eligible. Fork PRs
  remain read-only. MEDIUM/LOW findings are not automatic repairs.
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

The workflow must first be installed on the trusted base:
`pull_request_target` does not activate an unmerged workflow from its own PR.
Preparation reads immutable Git blobs, records `trusted_base_sha` separately
from feature `comparison_base_sha`, and stages the base's skill/analyzer in
gh-aw's read-only runtime mount. PR instructions and artifacts are review data,
never operating instructions.

After preparation, the agent inspects the verified PR head branch. Operating
configuration changes fail closed; a separate trusted review must handle them.
The model does not execute PR code. Each required review dimension cites exact
immutable source, including companion resources/peers/handlers when needed.
Every static signal becomes a finding or an explained false-positive dismissal.
Coverage/provenance checks do not authenticate the model's semantic judgment.

The post-step checks the live PR head, report, allowed files, severity/confidence,
exact repair recipe and captured commit series. Repaired files retain regular
file type, mode, encoding and line endings. Untracked, extra and stale changes
are rejected. These are permission/patch checks, not product tests.

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

As in the pre-simplification workflow, native discovery follows the source
agent, independently of branch publication. The publication job does not
depend on native discovery, and the native job scans the event's original
head, not a proposed repair commit. It is not post-fix validation.

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
gh aw compile ghaw-pr-accessibility --no-check-update
gh aw validate ghaw-pr-accessibility --no-check-update
```

These commands validate the workflow implementation; they do not run product
builds/tests on a PR repair.
