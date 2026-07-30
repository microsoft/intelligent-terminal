# PR-to-Integration-Test Workflow

Use this procedure for a target Intelligent Terminal PR. Adapt the exact test
suite and build commands to the files changed by that PR.

## 1. Resolve the Target and Branch Strategy

Inspect the PR before reading implementation files:

```powershell
$prNumber = 482
gh pr view $prNumber --repo microsoft/intelligent-terminal `
  --json number,title,body,state,mergedAt,mergeCommit,baseRefName,headRefName,commits,files,closingIssuesReferences,reviews,url
gh pr diff $prNumber --repo microsoft/intelligent-terminal
```

Read linked issues and relevant review threads. Record:

- The pre-fix user symptom and exact reproduction.
- The intended behavior and explicitly accepted limitations.
- The changed components and every boundary crossed at runtime.
- Whether the PR is open, merged, or superseded.

Choose the branch deliberately:

- **Merged PR / follow-up test PR:** update the PR's base branch with
  `git pull --ff-only`, delete the merged feature branch only after verifying
  merge state, then create a new test branch from the merged commit.
- **Open PR and tests belong in it:** use the PR head only when the user expects
  commits on that branch and it is safe to push there.
- **Open PR but independent validation is requested:** create a branch from the
  PR head and clearly state that the test branch depends on the unmerged PR.

Verify merge state and a clean worktree before deleting a branch. Squash-merged
branches may require `git branch -D` because Git cannot infer ancestry.

## 2. Reconstruct the Behavioral Contract

Describe the full path as components and observable handoffs:

```text
user trigger
  -> producer/component A
  -> protocol or process boundary
  -> consumer/component B
  -> user-visible effect
```

For a regression, distinguish:

- What the producer emitted before the fix.
- What downstream code interpreted.
- Why existing tests did not catch the gap.
- Which observable separates the regression from a legitimate success case.

## 3. Audit Existing Coverage and Harness Primitives

Search before adding helpers:

```powershell
$pattern = 'feature|event|setting|command'
git grep -n -E $pattern -- test/e2e tools/wta/src src/cascadia
```

Read:

- Related `test/e2e/tests/Feature.*.Tests.ps1` suites.
- Relevant functions under `test/e2e/ItE2E/Public/`.
- Unit tests around classification, state transitions, and parsing.
- Matching items in `doc/release-check-list.md`.
- `test/e2e/release-coverage-map.psd1`.
- `test/e2e/release-exclude.psd1`, so a new title is not silently omitted from
  the generated report.

Classify current coverage:

| Layer | What it should prove |
|-------|----------------------|
| Unit | Local decisions, parsing, reducer/state-machine branches |
| Component | Generated scripts, serialization, API adapters |
| Integration/E2E | Real process/protocol/package/UI wiring |
| Release checklist | Which user-facing behaviors count as signed off |

Add a shared helper only when multiple suites need it or it provides a more
precise oracle.

## 4. Build the Behavior Matrix

Start with the regression, then add only risk-driven controls:

1. **Regression positive:** the exact old failure now reaches the intended final
   effect.
2. **Ordinary baseline:** the preexisting successful or failure path still
   works.
3. **False-positive control:** a similar but legitimate case remains ignored.
4. **Replay/idempotency:** redraw, retry, duplicate event, or repeated command
   does not double-submit or reuse stale state.
5. **Lifecycle/routing:** hidden panes, split panes, moved tabs, reconnects, or
   process restarts only when the PR touches those risks.
6. **Compatibility:** alternate shell, agent, policy, or language mode only when
   the changed code is shared with it.

Each case must correspond to a plausible failure mode introduced or exposed by
the target PR.

For every case, identify both the immediate trigger and the downstream effect.
For example, proving a failure event exists is insufficient when the user
contract is that Autofix receives it; assert the event and the Autofix request.

## 5. Implement Deterministic ItE2E Tests

Follow existing suite structure:

```powershell
#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeDiscovery {
    $package = Get-AppxPackage | Where-Object Name -like '*IntelligentTerminal*'
    $script:Ready = $null -ne $package
}

Describe 'Feature: <behavior>' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It '<exact release-checklist title>' {
        # Start observers before the action, scope the oracle, and assert the
        # real downstream contract.
    }
}
```

Implementation rules:

- Start listeners before the action; scope predicates by stable IDs and poll for
  positive outcomes.
- For a negative case, first prove the action completed, then use a bounded
  observation window.
- Use unique inputs when the product deduplicates, isolate state between cases,
  and clean up in `finally`/`AfterAll`.
- Keep model-semantic assertions separate from deterministic pipeline checks.

## 6. Wire the Release Checklist

Add unchecked E2E items near the related feature section:

```markdown
- [ ] `[new]` `[E2E]` **Exact behavior title:** User-visible contract. _(#issue; E2E: `Feature.Suite`.)_
```

Use the same `Exact behavior title` in the Pester `It` name. Then assign IDs:

```powershell
pwsh -NoProfile -File test/e2e/Set-ChecklistIds.ps1
```

Never reuse, insert, or renumber IDs manually. If exact-title matching is not
appropriate, add a narrow regex to `test/e2e/release-coverage-map.psd1` and
explain why it cannot over-credit another behavior.

Update the suite table in `test/e2e/README.md` when adding a new feature file.

## 7. Validate Tests and Report Mapping

Verify prerequisites:

```powershell
pwsh -NoProfile -File test/e2e/bootstrap.ps1 -Check
```

Run the new suite through the report driver:

```powershell
$suite = 'test/e2e/tests/Feature.AutofixParser.Tests.ps1'
pwsh -NoProfile -File test/e2e/Invoke-ItE2EReport.ps1 `
  -Path $suite `
  -UpdateReport
```

Confirm:

- No new case failed or skipped unexpectedly.
- Every new checklist ID appears as `- [x]` in
  `test/e2e/artifacts/release-report.md`.
- A failure would produce `AUTOMATION FAILED`, not a false pass.

`-UpdateReport` incrementally overlays matched results when a prior report
exists and falls back to a fresh report otherwise. To validate the underlying
incremental script explicitly or use a custom output directory, run:

```powershell
$report = 'test/e2e/artifacts/release-report.md'
$results = 'test/e2e/artifacts/results.xml'
$updated = 'test/e2e/artifacts/release-report-updated.md'
pwsh -NoProfile -File test/e2e/Update-ReleaseReport.ps1 `
  -Report $report `
  -ResultsXml $results `
  -OutFile $updated
```

Verify only matched items changed and every new ID is `[x]`. A skipped-only
result must leave an existing checkbox unchanged.

Run related existing suites in the same Pester invocation when practical. At
minimum include:

- The suite that previously covered the ordinary path.
- The lower-layer suite that produces the new trigger.
- Routing/lifecycle suites affected by shared state.

Escalate to the broader Feature suite only when targeted runs expose shared
regressions or the PR changes common harness/product infrastructure.

## 8. Prove the Correct Build Ran

Build and deploy the changed area. For WTA changes, build the explicit target
matching the package architecture before the C++ package, deploy it, select it
with `ITE2E_PACKAGE` or `-Package Dev`, and verify a runtime version, path, log,
or changed observable. Compilation alone does not prove the deployed package
contains the new `wta.exe` or generated shell integration.

## 9. Deliver the Test PR

Before committing, run `git -c core.whitespace=cr-at-eol diff --check`.

The PR description must include:

- Target PR/issue and the integration boundary added.
- Cases and checklist IDs.
- Targeted/regression totals and skip reasons.
- Package/build tested.
