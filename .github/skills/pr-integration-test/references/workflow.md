# PR-to-Integration-Test Workflow

Use this procedure for a target Intelligent Terminal PR. Adapt the exact test
suite and build commands to the files changed by that PR.

## 1. Resolve the Target and Branch Strategy

Inspect the PR before reading implementation files:

```powershell
gh pr view <number> --repo microsoft/intelligent-terminal `
  --json number,title,body,state,mergedAt,mergeCommit,baseRefName,headRefName,commits,files,closingIssuesReferences,reviews,url
gh pr diff <number> --repo microsoft/intelligent-terminal
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

Never force-delete a branch based only on its name. Verify the PR state and a
clean worktree first. A squash-merged branch may require `git branch -D`
because Git cannot infer ancestry even though GitHub confirms the merge.

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

Do not accept a PR description as the sole source of truth. Compare it with the
code, issue reproduction, logs/events, and live behavior when available.

## 3. Audit Existing Coverage and Harness Primitives

Search before adding helpers:

```powershell
git grep -n -E "<pattern>" -- test/e2e tools/wta/src src/cascadia
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

Reuse public ItE2E primitives. Add a shared helper only when multiple suites
need the same operation or the helper itself provides a more precise oracle.

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

Avoid combinatorial matrices. Each case must correspond to a plausible failure
mode introduced or exposed by the target PR.

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

- Start event listeners before triggering the behavior.
- Scope event predicates by stable IDs; do not accept unrelated startup events.
- Use `Wait-WtEvent`, `Test-Until`, or assertion helpers for positive outcomes.
- For negative outcomes, first prove the command/action completed, then observe
  a short bounded window and assert the forbidden event/state is absent.
- Use unique command text when the product intentionally deduplicates repeated
  requests.
- Use fresh applications or explicit state cleanup when one case can leave an
  agent turn, setting, pane, or listener active.
- Put cleanup in `finally`/`AfterAll`.
- Keep model-semantic tests separate from deterministic routing tests. If model
  variance is accepted, skip only the semantic assertion after proving the
  deterministic pipeline succeeded.

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
pwsh -NoProfile -File test/e2e/Invoke-ItE2EReport.ps1 `
  -Path test/e2e/tests/Feature.<Name>.Tests.ps1 `
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
pwsh -NoProfile -File test/e2e/Update-ReleaseReport.ps1 `
  -Report <existing-release-report.md> `
  -ResultsXml <results.xml> `
  -OutFile <updated-release-report.md>
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

Build and deploy according to the changed area. For WTA + Terminal changes:

1. Build WTA with the explicit target matching the package architecture:
   `--target x86_64-pc-windows-msvc` for x64 or
   `--target aarch64-pc-windows-msvc` for ARM64. Package deployment prefers the
   explicit-target artifact.
2. Build the C++ package after WTA.
3. Deploy/redeploy the package and select it with `ITE2E_PACKAGE` or
   `-Package Dev`.
4. Verify package version/path, runtime logs, or a changed observable.

Do not infer deployment success from compilation alone. A stale packaged
`wta.exe`, generated shell-integration script, profile reference, or AppX
staging directory can make a new test exercise old code.

## 9. Deliver the Test PR

Before committing:

```powershell
git -c core.whitespace=cr-at-eol diff --check
git status --short --branch
```

The PR description must include:

- Target PR and issue.
- Missing integration boundary now covered.
- Positive, negative, and compatibility cases added.
- New checklist IDs and proof they become `[x]`.
- Exact targeted and regression test totals.
- Every skip with its reason.
- Package/build tested.

Do not claim complete regression coverage if environment-dependent suites were
not run. Distinguish deterministic passes from accepted model variance and
unavailable prerequisites.
