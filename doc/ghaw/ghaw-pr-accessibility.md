# Native WinUI accessibility PR review

`Native WinUI Accessibility Review` is a separate gh-aw check for pull requests
that change Intelligent Terminal's WinUI/XAML/C++ UI. It complements, rather
than replaces, Windows UI Automation tests and interactive accessibility
testing.

## Trust and execution model

Installation prerequisite: the workflow and its trusted analyzer, skill, agent,
and native harness must first be installed together on the base branch.
`pull_request_target` does not run this newly introduced workflow from its own
unmerged PR head. It reviews subsequent PR events using installed trusted base
code. Both staged hosted probes installed those resources before testing.
Missing base support is a deployment failure; never bootstrap by executing the
untrusted PR head's analyzer or harness.

The workflow runs on `pull_request_target`, so its compiled definition and
initial checkout come from the trusted base branch. It fetches the pull request
head as a Git object and runs the base revision's
`.github/skills/pr-accessibility/scripts/accessibility_review.py` against those
head blobs. Only after deterministic preparation does it switch the worktree to
the verified immutable head for model inspection. It never imports or executes
pull-request code.
The checkout uses the actual PR head branch name after validating its ref
format. Repair commits therefore have a local branch for the production
safe-output transport to pin; a detached commit is not sufficient.
The agent commits one eligible repair directly on that head, then requests AM
patch transport. The native gate validates the actual queued output mode and
trusted PR identity, requires a clean single-commit candidate, and applies the
captured patch to an isolated Git index. Its resulting tree must equal the
candidate already verified by the exact static recipe. This rejects stale,
tampered, mixed-output, or post-capture edits before publication.
The gate also rejects alternate bundle artifacts: the pinned publisher prefers
a present bundle, so an AM-only validated repair must not upload one.

Preparation also stages the domain skill from the trusted base into
`$RUNNER_TEMP/gh-aw/accessibility-trusted/SKILL.md`. The prepared evidence and
validator live alongside it in gh-aw's read-only runtime mount, rather than
the agent-writable output directory. Post-validation uses that staged validator,
checks Git object integrity, and ignores replacement refs. The agent uses the
trusted skill copy and Copilot bare
mode disables automatic custom instructions. Preparation rejects PR changes
to operating configuration directories or instruction/agent/skill files before
head checkout or agent startup. This conservative policy intentionally blocks
mixed UI-and-configuration PRs; they need a separate trusted review lane rather
than loading their configuration as agent instructions.

For inspection, the model receives only read-only Git shell commands; the
caller separately permits the narrow repair edit and local commit described
above. Pull-request files, comments,
logs, and attachments remain untrusted. A final deterministic gate verifies the
structured report, exact source SHA, allowed patch paths, high-severity and
high-confidence repair policy, exact trusted-recipe candidate, patch
attribution, absence of untracked files, and absence of new literal accessible
strings. It rejects all model-authored validation claims and adds its own
attestation only after the candidate matches. A separate post-step queries the
current PR head immediately before safe-output publication. Workflow
concurrency and that freshness check narrow, but cannot eliminate, the
asynchronous race between the check and publication.

The compiled safe-output job also requires the agent job to succeed, so a
failed post-step validator cannot publish previously queued branch writes.
Validated final JSON is retained as an artifact; advisory details are included
in the job summary without introducing a second publication mode.

Fork pull requests are review-only. The report validator rejects both `fixed`
dispositions and worktree changes for forks. Same-repository fixes use
`push-to-pull-request-branch` against the reviewed head SHA with no fallback PR.
This is compatible with the localization mutation contract: accessibility
repairs cannot edit `.resw` files or introduce literal
`AutomationProperties.Name` values. Findings that need new customer-facing
text remain blocking until the localization workflow supplies the resource.

Like the localization workflow, this workflow imports a deliberately thin
custom agent (`.github/agents/pr-accessibility.agent.md`). The reusable
`.github/skills/pr-accessibility/SKILL.md` owns domain procedure, native-runtime
limitations, finding standards, and test guidance. The workflow Markdown owns
only PR trust, immutable revisions, output shape, freshness, and mutation
policy; the Python script owns deterministic preparation and final gating.

## Deterministic source evidence

The analyzer classifies changed native UI files and highlights changed source
associated with names/roles/values, automation peers and patterns,
keyboard/focus, announcements, disabled/busy states, themes/high contrast,
scaling/clipping, tabs, panes, settings, and agent cards.

Deleted UI source remains relevant. Prepared evidence records its base blob and
exact deletion diff with a null head blob; the agent must inspect whether an
operation disappeared or moved, not assume a nonexistent head file is a repair
candidate. Renames expose both sides, including moves out of UI scope.

Its XAML checks are intentionally conservative:

- An interactive control explicitly assigned to the raw UIA view is a HIGH,
  medium-confidence investigation lead. After repository inspection establishes
  high confidence and an existing name source, only the exact raw-view-property
  removal can use the trusted static recipe.
  Style-only naming remains blocked because the recipe does not authenticate
  style setters.
- An apparently icon-only control without a local name source is MEDIUM
  needs-review evidence. Content, `x:Uid`, bindings, styles, labels, and peers
  must be inspected before calling it inaccessible.
- A literal accessible name is MEDIUM, high-confidence localization evidence;
  it is not repaired by this workflow.
- Inert decorative controls and content/resource-derived names are excluded.

These are repository-owned source checks. Axe.Windows does not parse XAML or
C++; it scans the runtime Windows UI Automation tree.

## Native runtime coverage

[Axe.Windows](https://github.com/microsoft/axe-windows/tree/59e107e14168e4d8dae81cc34bf83e7c9785142e)
uses UIA hierarchy, properties, patterns, and child elements, then evaluates
independent Error, Warning, and NeedsReview rules. Its CLI targets a running
process or window and emits `.a11ytest` output. Those capabilities make it
appropriate for names, control types, states, required patterns, and other
UIA-tree invariants on a built application.

It does not replace journey/state setup or human checks. A focused Windows
validation should:

1. Build without deploying over the user's running app, launch a test package
   on an interactive desktop, and navigate to each affected state.
2. Run Axe.Windows against the exact process/window after each state change and
   retain the caller-supported scan artifacts. Use `.a11ytest` when screenshot
   capture works; the pinned smoke harness instead retains the JSON evidence
   described below.
3. Verify keyboard-only traversal/activation, focus restoration and traps, and
   UIA pattern invocation.
4. Observe Narrator announcements and live-region payloads.
5. Inspect disabled/busy states, contrast themes, 200% text size/display scale,
   clipping, wrapping, reflow, tabs, panes, settings, and agent cards.

Linux review jobs have no interactive Windows desktop or built application.
The workflow therefore records Axe/UIA, screen-reader, keyboard/focus,
contrast/theme, and scaling checks as `SKIPPED`, never as passing native
results.

The same workflow also runs an independent `Native Axe.Windows smoke` job on
an ephemeral GitHub-hosted Windows runner. It checks out the immutable PR head
without persisted credentials, builds the existing packaged `TestHostApp`, and
launches real product surfaces in three visible states:

- `--accessibility-page=fre` hosts `TerminalApp::FreOverlay` initialized from
  default settings.
- `--accessibility-page=agents` hosts the real Agents settings page and its
  production view model directly, avoiding unrelated full-settings startup.
- The `fre-settings` scan launches FRE, invokes the real Next button through
  UI Automation, and waits for the visible `TabModeComboBox` before scanning.

The job downloads Axe.Windows 2.4.2, verifies the archive's pinned SHA-256,
scans all three states through its supported automation API, and uploads
structured rule/element evidence and per-surface logs. The API uses
`OutputFileFormat.None` because `.a11ytest` generation unconditionally captures
a screenshot and has failed with a Win32 invalid-handle error even when UI
Automation is available. These JSON artifacts are not `.a11ytest` files and
do not supply its screenshot or replay capabilities; screenshot capture remains
an explicit gap.
A UIA hierarchy JSON is retained for each state, including successful scans,
with runtime/parent IDs, names, control types, visibility, and keyboard focus
properties.
Each scan also records its visible state marker; window count alone is not a
surface-readiness assertion.
A blocked launch, scan failure, or Error-level Axe rule fails the native job.
These results are deliberately separate from the model-authored report and
cannot authorize an automatic repair.
The scanner and launch harness are checked out from the immutable trusted base,
not executed from PR-controlled script files. Enforcement requires all three
structured results and matching source, process, state, and hierarchy evidence,
rather than accepting successful step exit codes alone. The build and scan
still share a runner: this is smoke evidence, not tamper-resistant attestation
against hostile candidate build code. It must never authorize a runtime repair.

The native job installs the framework packages listed in the generated
TestHostApp appx recipe using the existing build helper and Windows PowerShell.
The harness checks required x64 package identities/versions for the activating
user, input-desktop access, and matching host/scanner user sessions. These are
fail-closed prerequisites, not evidence that every hosted Windows image provides
an interactive desktop.

The source-review agent depends only on activation, not on the Windows job.
The Windows job runs after the agent (including an agent failure), unless the
run was cancelled or the agent was skipped.
A missing Windows prerequisite therefore fails the native check without
suppressing source review. A scan that returns zero windows is blocked rather
than treated as a zero-error pass.

The smallest repository-owned runtime extension is a focused test in
`src/cascadia/WindowsTerminal_UIATests` that navigates to the affected state,
asserts UIA names/control types/patterns and keyboard focus, and invokes an
Axe.Windows scan from the same Windows test session. Adding the NuGet dependency
requires owner approval and a separate dependency review; this workflow does
not install or add it.

A lighter Windows lane does not need to build or launch the full product. A
repository-owned packaged WinUI test host can load the affected page/control,
the same product resource dictionaries, styles, and code-behind dependencies,
then expose deterministic test-state navigation. An ephemeral Windows VM with
an interactive signed-in desktop can:

1. build the test host from the exact PR SHA with a read-only token and no
   secrets;
2. launch it with a bounded timeout and identify its process/window;
3. run a pinned Axe.Windows version from a trusted harness against that window;
4. execute separate keyboard/focus/UIA pattern assertions for behavior Axe does
   not prove; and
5. upload structured Axe findings, assertions, logs, test-host revision, and
   tool version as artifacts.

Prefer an ephemeral, isolated self-hosted Windows VM configured for an
interactive desktop. A persistent privileged runner must not execute
pull-request code. Runtime artifacts are advisory unless the harness, output
location, and attestation boundary prevent the candidate process from
fabricating results; until then, they must not authorize automatic repair or
publication.

For local verification after building `TestHostApp`, use the same harness:

```powershell
test\accessibility\Invoke-AxeWindowsTestHost.ps1 `
    -Surface fre `
    -ManifestPath bin\x64\Debug\TestHostApp\AppxManifest.xml `
    -AxePath <path-to-AxeWindowsCLI.exe> `
    -OutputDirectory <artifact-directory> `
    -SourceSha (git rev-parse HEAD)

test\accessibility\Invoke-AxeWindowsTestHost.ps1 `
    -Surface agents `
    -ManifestPath bin\x64\Debug\TestHostApp\AppxManifest.xml `
    -AxePath <path-to-AxeWindowsCLI.exe> `
    -OutputDirectory <artifact-directory> `
    -SourceSha (git rev-parse HEAD)

test\accessibility\Invoke-AxeWindowsTestHost.ps1 `
    -Surface fre-settings `
    -ManifestPath bin\x64\Debug\TestHostApp\AppxManifest.xml `
    -AxePath <path-to-AxeWindowsCLI.exe> `
    -OutputDirectory <artifact-directory> `
    -SourceSha (git rev-parse HEAD)
```

The harness uses bounded launch/scan waits, closes only the launched process,
removes only `WindowsTerminal.TestHost`, and restores a prior registration of
that test package when one existed. It never removes or deploys the Intelligent
Terminal package.

## Findings and publication

The final JSON uses stable IDs and records severity independently from
confidence, source SHA, path/line, observed and expected behavior, user impact,
evidence, proposed repair, trusted validation, and disposition. The job summary
groups HIGH findings as must-fix/blocking and reports fixed, remaining, skipped,
and blocked checks without posting duplicate PR comments.

The workflow exposes only `push-to-pull-request-branch` (plus the system
`noop`) and never exposes `add-comment` or PR-review output to the same agent.
Repair runs request one branch push; report-only runs request `noop` and use the
workflow check summary. This follows the existing localization split between
the same-repository repair worker and fork guidance worker and avoids coupling
a code push to a second PR-comment output. gh-aw can process multiple output
types, but its documented failure semantics cancel remaining non-code outputs
when a code push fails, so a comment is not an atomic companion to a commit.

The final validator receives both the reviewed SHA and a fresh live PR head SHA
and rejects a mismatch. This check is covered by a unit test. It is still not a
transactional compare-and-swap with publication: the head can move after the
check. With `fallback-as-pull-request: false`, a resulting non-fast-forward push
fails instead of creating another PR, but gh-aw exposes no stronger expected-head
CAS for this safe output.

Only high-confidence HIGH defects matching the trusted
`AXSTATIC001-remove-raw-view` recipe can be published as fixes. The native gate
reconstructs the complete expected candidate from the immutable head and
prepared finding, rejects every extra file/hunk and all untracked files, and
writes the PASS attestation itself. It never trusts model-authored command
results. Runtime-dependent fixes and all medium/low findings remain
`remaining`, `blocked`, or advice.

## Validation

### Evidence boundary and remaining gaps

The recorded hosted native PASS in run `37387409312` is evidence for the
three-state TestHostApp/Axe.Windows smoke lane, not for hosted AI review or
safe-output publication. Hosted execution of the AI gh-aw path and hosted
read-only fork review remain unverified. Local validation of those contracts
does not establish a full production automation run, a coverage percentage,
or accessibility compliance. Narrator, complete keyboard/focus journeys,
contrast, scaling, and screenshot capture remain separate validation gaps.

```powershell
python .github\skills\pr-accessibility\tests\test_accessibility_review.py
gh aw compile ghaw-pr-accessibility --no-check-update
gh aw validate ghaw-pr-accessibility --no-check-update
```

The unit suite covers content/resource-derived names, intentionally hidden
decorative controls, icon-only false-positive handling, focus/announcement/
theme classification, stable output, malformed output, stale SHA, fork
read-only behavior, high-only repair gating, exact trusted repair acceptance,
empty-diff and fabricated-validation rejection, mixed-severity patch
attribution, untracked/out-of-scope rejection, and localized string
enforcement.

## References

- Axe.Windows overview, pinned at commit
  [`59e107e`](https://github.com/microsoft/axe-windows/blob/59e107e14168e4d8dae81cc34bf83e7c9785142e/docs/Overview.md)
- Axe.Windows rule model, pinned at commit
  [`59e107e`](https://github.com/microsoft/axe-windows/blob/59e107e14168e4d8dae81cc34bf83e7c9785142e/docs/RulesOverview.md)
- Axe.Windows CLI, pinned at commit
  [`59e107e`](https://github.com/microsoft/axe-windows/blob/59e107e14168e4d8dae81cc34bf83e7c9785142e/src/CLI/README.MD)
- [Windows app accessibility checklist](https://learn.microsoft.com/windows/apps/design/accessibility/accessibility-checklist)
- Repository UIA architecture and expectations:
  `doc/terminal-a11y-2023.md`,
  `src/cascadia/TerminalApp/TabStripAutomationPeer.cpp`, and
  `src/cascadia/WindowsTerminal_UIATests/SmokeTests.cs`

Browser axe-core and Playwright accessibility snapshots were rejected: they
test web accessibility trees and are not native WinUI UIA/Axe.Windows coverage.
Accessibility Insights for Windows remains useful for interactive inspection,
but its desktop application is not a source linter and is not available in this
Ubuntu review job.
