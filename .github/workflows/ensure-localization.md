---

description: 'Detached same-repo localization repair worker; uses the shared file-based localization procedure for the actual PR diff, repairs localized targets, and requests one final read-only review. Dispatched by ensure-localization-controller.yml.'

intent: 'Use the shared ensure-localization procedure against the controller-supplied comparison base, repair only localized targets, rerun the same checks, and finish with an independent read-only review.'



on:

  workflow_dispatch:

    inputs:

      dispatch_id:

        description: 'Controller correlation identifier'

        required: true

        type: string

      pr_number:

        description: 'Pull request number'

        required: true

        type: string

      repo:

        description: 'Target owner/repository'

        required: true

        type: string

      expected_head_sha:

        description: 'Immutable pull request head reviewed by the controller'

        required: true

        type: string

      expected_base_sha:

        description: 'Observed pull request base tip for metadata only'

        required: true

        type: string

      comparison_base_sha:

        description: 'Controller-resolved merge base for actual PR diff inspection'

        required: true

        type: string

      head_ref:

        description: 'Pull request head branch'

        required: true

        type: string

      base_ref:

        description: 'Pull request base branch'

        required: true

        type: string

      head_repo:

        description: 'Pull request head repository'

        required: true

        type: string

permissions:

  contents: read

  pull-requests: read

  copilot-requests: write



engine: copilot
imports:
  - .github/agents/localization-expert.agent.md
  - shared/localization-summary.md


checkout:

  ref: ${{ github.event.inputs.expected_head_sha }}

  fetch-depth: 0



network:

  allowed:

    - defaults

    - 'learn.microsoft.com'



tools:

  bash:

    - 'git diff:*'

    - 'git grep:*'

    - 'git rev-parse:*'

    - 'git show:*'

    - 'git status:*'

    - 'pwsh:*'

jobs:
  prepare:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    outputs:
      trusted_code_revision: ${{ steps.validate-inputs.outputs.trusted_code_revision }}
    steps:
      - name: Validate dispatch inputs
        id: validate-inputs
        shell: pwsh
        env:
          PR_NUMBER: ${{ github.event.inputs.pr_number }}
          HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
          EXPECTED_BASE_SHA: ${{ github.event.inputs.expected_base_sha }}
          COMPARISON_BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
        run: |
          $ErrorActionPreference = 'Stop'
          if ($env:PR_NUMBER -notmatch '^[1-9][0-9]*$') {
            throw 'pr_number must be a positive decimal pull request number.'
          }
          $shaPattern = '^[0-9a-fA-F]{40}$'
          foreach ($candidate in @(
            @{ Name = 'expected_head_sha'; Value = $env:HEAD_SHA }
            @{ Name = 'expected_base_sha'; Value = $env:EXPECTED_BASE_SHA }
            @{ Name = 'comparison_base_sha'; Value = $env:COMPARISON_BASE_SHA }
          )) {
            if (($candidate.Value ?? '') -notmatch $shaPattern) {
              throw "$($candidate.Name) must be an exact 40-character hexadecimal SHA."
            }
          }
          "trusted_code_revision=$env:HEAD_SHA" >> $env:GITHUB_OUTPUT

  agent:
    needs: [prepare]

  safe_outputs:
    if: needs.agent.result == 'success'
    permissions:
      pull-requests: read

safe-outputs:

  github-token: ${{ secrets.GITHUB_TOKEN }}

  push-to-pull-request-branch:

    base-branch: ${{ github.event.inputs.expected_head_sha }}

    github-token-for-extra-empty-commit: "${{ '' }}"

    allowed-files:

      - 'src/cascadia/**/Resources/*.resw'

      - 'src/cascadia/**/Resources/**/*.resw'

      - 'tools/wta/locales/*.yml'

    protected-files: blocked

    if-no-changes: error

    fallback-as-pull-request: false



post-steps:
  - name: Reject stale worker output before publication
    shell: pwsh
    env:
      GH_TOKEN: ${{ github.token }}
      EXPECTED_HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
      PR_NUMBER: ${{ github.event.inputs.pr_number }}
      REPOSITORY: ${{ github.event.inputs.repo }}
    run: |
      $ErrorActionPreference = 'Stop'
      if ($env:PR_NUMBER -notmatch '^[1-9][0-9]*$') {
        throw 'Freshness check received an invalid pull request number.'
      }
      if (($env:EXPECTED_HEAD_SHA ?? '') -notmatch '^[0-9a-f]{40}$') {
        throw 'Freshness check received an invalid expected head SHA.'
      }
      $currentHeadOutput = & gh api "/repos/$env:REPOSITORY/pulls/$env:PR_NUMBER" --header 'Accept: application/vnd.github+json' --jq '.head.sha'
      if ($LASTEXITCODE -ne 0) {
        throw "Failed to read the current head SHA for PR #$env:PR_NUMBER."
      }
      $currentHead = ($currentHeadOutput | Out-String).Trim()
      if (($currentHead ?? '') -notmatch '^[0-9a-fA-F]{40}$') {
        throw "Freshness check returned an invalid current head SHA: '$currentHead'."
      }
      if ($currentHead.ToLowerInvariant() -cne $env:EXPECTED_HEAD_SHA.ToLowerInvariant()) {
        throw "Stale localization output rejected: PR #$env:PR_NUMBER head changed from $env:EXPECTED_HEAD_SHA to $currentHead before publication."
      }

timeout-minutes: 45

max-ai-credits: 1000

max-daily-ai-credits: 5000

concurrency:

  group: 'localization-expert-${{ github.event.inputs.pr_number }}'

  job-discriminator: ${{ github.run_id }}

  cancel-in-progress: true

run-name: 'Ensure Localization ${{ github.event.inputs.dispatch_id }}'

---

Same-repo localization repair for PR #${{ github.event.inputs.pr_number }} in
`${{ github.repository }}`.

Imported runtime role: `localization-expert`.

Use `pwsh` for programmatic file reads, XML/YAML processing, and summary writes.
If a tool call is denied, switch directly to the permitted PowerShell operation;
do not probe permissions or retry variants of the denied operation.

## Goal

Verify `git rev-parse HEAD` equals
`${{ github.event.inputs.expected_head_sha }}`, then follow
`.github/skills/ensure-localization/SKILL.md` using:

- comparison base `${{ github.event.inputs.comparison_base_sha }}`
- immutable head `${{ github.event.inputs.expected_head_sha }}`
- observed base metadata `${{ github.event.inputs.expected_base_sha }}`
- exact original patch inspection with
  `git diff --no-ext-diff --unified=3 ${{ github.event.inputs.comparison_base_sha }} ${{ github.event.inputs.expected_head_sha }} -- <resource paths>` before choosing scoped keys or targets; treat `--stat`, `--name-only`, `--name-status`, `--numstat`, `git status`, and worktree-only diffs as supporting signals only, and keep reading if the patch output truncates until every relevant hunk is covered

Derive this workflow's scope only from English source-authority additions,
updates, or deletions in that original patch. Expand additions or updates to
every shipped localized counterpart that should carry the affected keys.
Expand removals to stale localized counterpart cleanup for the removed keys or
files. Localized-only edits or deletions do not independently create repair
scope. If the original patch yields no English-derived scope, keep the run
read-only and do not promote localized-only edits into repair scope just to
manufacture work. Explain that no English-derived localization work was needed
in the Markdown summary.

Repair only localized targets in that English-derived scope. Keep source
authority read-only and finish with the required independent review. Invoke the
registered `localization-review-gate` agent after the final checks and require
its explicit `PASS` before requesting any branch write. The root repair agent
owns all git inspection, scope discovery, edits, and the final checker rerun;
do not delegate those steps. Preserve the exact English-derived keys, values,
and surrounding context through the final rerun; do not replace them with
guesses from unchanged source lines, file prefixes, samples, or PR summaries.

## Output contract

Run the actual skill checks and assess their results directly. Before requesting
a branch write or acknowledging success, require all final checks and the
independent review to be `PASS`. Do not create an aggregate checker-report file.
Write the human-readable Markdown summary using the shared template.

- Use the shared skill's batching guidance for the final rerun.
- Do not print all bundles into the model context; inspect summaries and
  non-PASS findings only. Do not build a second checker harness or rerun
  successful batches unless a repair or reviewer finding requires it.
- If the English-derived scope is deletion-only and the current tree no longer
  contains one or more removed source/target files, materialize immutable
  pre-deletion snapshots for exactly those files and run syntax and encoding
  checks on those snapshots. Treat those results as historical evidence only,
  and use explicit git inspection separately to prove the live tree really
  removed the files.
- You and the independent reviewer own scope completeness and language judgment;
  checker output alone cannot establish them.
- Before any safe output becomes eligible, the native post-step re-reads the
  live PR head and rejects stale output when it no longer matches
  `${{ github.event.inputs.expected_head_sha }}`. That last-moment check helps,
  but it is not a blanket guarantee: the pinned gh-aw docs for
  `push-to-pull-request-branch` document no additional expected-head
  compare-and-swap option beyond workflow concurrency and the final head check,
  and GitHub cancellation/publication remain asynchronous.
- Follow the scoped-key rules in the shared SKILL: keep required keys limited
  to source-present additions or updates, handle source removals with the
  skill's step-1 explicit review/cleanup path, and recompute the comparable key
  subset from the final on-disk source/target files after every edit
  so newly translated source-added keys are included before dependent reruns.
- The independent reviewer is read-only and separate. It returns only its
  review verdict and findings; it never owns the safe-output call.
  Give that reviewer the comparison base, immutable head, exact
  repaired file list, and an explicit requirement to independently re-derive
  expected keys from the original patch instead of from your selected-key list.
- No edit: one `noop` acknowledgement after the final `PASS` rerun and
  independent review confirm no localized repairs were necessary.
- Edited: one focused commit whose subject
  ends with `[localization-expert]`, then use `push-to-pull-request-branch`.
  Do not discard working localization repairs through a `noop`.

Do not claim success for source-only, blocked, invalid, or excluded changes.
If checks or review cannot complete, describe the limitation in the summary and
use the appropriate native incomplete outcome instead of a push or success
acknowledgement. No extra output, extra commit, or source-authority edit.

## agent: `localization-review-gate`
{{#runtime-import .github/agents/localization-reviewer.agent.md}}

Caller contract for this reviewer: pass the comparison base, immutable head,
the exact repaired or reviewed resource paths, and the current repair summary.
Require the reviewer to inspect the original
`git diff --no-ext-diff --unified=3 <comparison-base> <immutable-head> -- <resource paths>`
content independently, treat only English source-authority additions, updates,
or deletions as scope-creating, audit all shipped localized counterparts
implicated by that scope, and fail `PASS` when any expected key block in that
scope is mismatched or omitted.
## end agent: `localization-review-gate`
