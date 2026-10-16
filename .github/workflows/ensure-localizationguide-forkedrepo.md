---

description: 'Fork PR localization guidance worker; uses the shared file-based localization procedure against immutable fork localization data and posts one actionable card only for fixable findings. Dispatched by ensure-localization-controller.yml.'

intent: 'Use the shared ensure-localization procedure against the controller-supplied comparison base, stay read-only, and post one actionable guidance card only when deterministic fixes are needed.'



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

      same_repo:

        description: 'Whether the head repository is the workflow repository'

        required: true

        type: string



permissions:

  contents: read

  pull-requests: read

  copilot-requests: write



engine:
  id: copilot
  model: auto

imports:

  - .github/agents/localization-reviewer.agent.md
  - shared/localization-summary.md



checkout:

  ref: ${{ github.workflow_sha }}

  fetch-depth: 0

  fetch: refs/pulls/open/*



tools:

  edit: false

  bash:

    - 'git diff:*'

    - 'git rev-parse:*'

    - 'git show:*'

    - 'git ls-tree:*'

    - 'pwsh:*'



jobs:

  prepare:

    runs-on: ubuntu-latest

    timeout-minutes: 10

    permissions:

      contents: read

      pull-requests: read

    outputs:
      trusted_code_revision: ${{ steps.verify-head.outputs.trusted_code_revision }}
    steps:
      - name: Checkout trusted workflow revision

        uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1

        with:

          ref: ${{ github.workflow_sha }}

          fetch-depth: 0

          persist-credentials: false

      - name: Verify immutable fork head

        id: verify-head
        shell: pwsh
        env:
          GH_TOKEN: ${{ github.token }}
          PR_NUMBER: ${{ github.event.inputs.pr_number }}
          HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
          EXPECTED_BASE_SHA: ${{ github.event.inputs.expected_base_sha }}
          COMPARISON_BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
          WORKFLOW_SHA: ${{ github.workflow_sha }}
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
          $remoteRef = "refs/remotes/origin/localization-pr-$env:PR_NUMBER"
          git -c credential.helper= -c 'credential.helper=!gh auth git-credential' fetch --no-tags origin "refs/pull/$env:PR_NUMBER/head:$remoteRef"
          $currentHead = (git rev-parse $remoteRef).Trim().ToLowerInvariant()
          if ($currentHead -ne $env:HEAD_SHA.ToLowerInvariant()) {
            throw "Fork PR head changed after controller dispatch. Expected $env:HEAD_SHA, found $currentHead."
          }
          "trusted_code_revision=$env:WORKFLOW_SHA" >> $env:GITHUB_OUTPUT


  agent:

    needs: [prepare]

  safe_outputs:
    if: needs.agent.result == 'success'


safe-outputs:

  add-comment:

    target: '${{ github.event.inputs.pr_number }}'

    max: 1

    hide-older-comments: true


pre-agent-steps:
  - name: Prepare immutable localization snapshots
    shell: pwsh
    env:
      COMPARISON_BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
      EXPECTED_HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
      LOCALIZATION_SNAPSHOT_ROOT: /tmp/gh-aw/agent/localization-snapshots
    run: |
      $ErrorActionPreference = 'Stop'
      foreach ($revision in @(
        @{ Name = 'base'; Sha = $env:COMPARISON_BASE_SHA }
        @{ Name = 'head'; Sha = $env:EXPECTED_HEAD_SHA }
      )) {
        if ($revision.Sha -notmatch '^[0-9a-fA-F]{40}$') {
          throw "Invalid $($revision.Name) snapshot revision."
        }
        $tree = @(git -c core.quotePath=false ls-tree -r $revision.Sha -- src/cascadia tools/wta/locales)
        if ($LASTEXITCODE -ne 0) { throw "Cannot read $($revision.Name) resource tree." }
        $resources = @($tree | Where-Object {
          $_ -match "`t(src/cascadia/.+/Resources/(?:.+/)?[^/]+\.resw|tools/wta/locales/[^/]+\.yml)$"
        })
        if ($resources.Count -eq 0) { throw "No resources in $($revision.Name) snapshot." }
        if (@($resources | Where-Object { $_ -notmatch '^100(644|755) blob ' }).Count -gt 0) {
          throw 'Localization snapshots accept regular resource files only, not symlinks.'
        }
        $pathspecs = @()
        if (@($resources | Where-Object { $_ -match "`tsrc/cascadia/" }).Count -gt 0) {
          $pathspecs += ':(glob)src/cascadia/**/Resources/**/*.resw'
        }
        if (@($resources | Where-Object { $_ -match "`ttools/wta/" }).Count -gt 0) {
          $pathspecs += ':(glob)tools/wta/locales/*.yml'
        }
        $destination = Join-Path $env:LOCALIZATION_SNAPSHOT_ROOT $revision.Name
        [System.IO.Directory]::CreateDirectory($destination) | Out-Null
        $archive = Join-Path $env:LOCALIZATION_SNAPSHOT_ROOT "$($revision.Name).zip"
        # Use trusted checkout attributes, never fork-provided export-ignore/export-subst.
        git archive --worktree-attributes --format=zip "--output=$archive" $revision.Sha -- @pathspecs
        if ($LASTEXITCODE -ne 0) { throw "Cannot archive $($revision.Name) resources." }
        try {
          $zip = [System.IO.Compression.ZipFile]::OpenRead($archive)
          try {
            if (@($zip.Entries | Where-Object { $_.Name }).Count -ne $resources.Count) {
              throw 'Resource archive omitted files; refusing incomplete localization evidence.'
            }
          } finally { $zip.Dispose() }
          [System.IO.Compression.ZipFile]::ExtractToDirectory($archive, $destination)
        } finally { Remove-Item -LiteralPath $archive -Force }
      }


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

timeout-minutes: 35

max-ai-credits: 500

max-daily-ai-credits: 5000

concurrency:

  group: 'localization-guide-fork-${{ github.event.inputs.pr_number }}'

  job-discriminator: ${{ github.run_id }}

  cancel-in-progress: true

run-name: 'Ensure Localization Guide Forked Repo ${{ github.event.inputs.dispatch_id }}'

---

Imported runtime role: `localization-reviewer`.

Fork localization guidance for PR #${{ github.event.inputs.pr_number }} in
`${{ github.repository }}`.

Use `pwsh` for programmatic file reads, XML/YAML processing, and summary writes.
Python, shell redirects, `touch`, and edit tools are unavailable in this worker.
If a tool call is denied, switch directly to the permitted PowerShell operation;
do not probe permissions or retry variants of the denied operation.

## Goal

Stay read-only on the trusted workflow checkout; never check out or execute fork
code. The immutable fork PR objects were fetched before agent credentials were
removed; use local `git show` and `git diff` to inspect them, then follow
`.github/skills/ensure-localization/SKILL.md` using:

- comparison base `${{ github.event.inputs.comparison_base_sha }}`
- immutable head `${{ github.event.inputs.expected_head_sha }}`
- observed base metadata `${{ github.event.inputs.expected_base_sha }}`
- exact original patch inspection with
  `git diff --no-ext-diff --unified=3 ${{ github.event.inputs.comparison_base_sha }} ${{ github.event.inputs.expected_head_sha }} -- <resource paths>` before choosing scoped keys or targets; treat `--stat`, `--name-only`, `--name-status`, `--numstat`, `git status`, and worktree-only diffs as supporting signals only, and keep reading if the patch output truncates until every relevant hunk is covered

Derive this workflow's scope only from English source-authority additions,
updates, or deletions in that original patch. Expand additions or updates to
every shipped localized counterpart that should carry the affected keys.
Expand removals to stale localized counterpart review for the removed keys or
files. Localized-only edits or deletions do not independently create guidance
scope. If the original patch yields no English-derived scope, keep the run
read-only and do not promote localized-only edits into guidance scope just to
manufacture work. Explain that no English-derived localization work was needed
in the Markdown summary.

## Execution discipline

The native setup has already materialized every resource, including unchanged
counterparts, under `/tmp/gh-aw/agent/localization-snapshots/base/` and `head/`.
These preserve repository-relative paths and exact blob bytes. Use the `head/`
files for checks and translated-value review; use `base/` files as `OriginalFile`
for existing-file encoding checks and as historical evidence for deletions.
Never use resources from the trusted checkout as though they were the PR head.
Do not recreate snapshots, guess locale lists, or generate snapshot scripts.

Run the shared checker's batch once after scope discovery and assess its actual
results directly.
For comparable keys in `qps-ploc`, `qps-ploca`, and `qps-plocm`, include
`Test-PseudoLocale` in that batch. Skipping it is not an acceptable way to finish
under the credit cap.
Read the scoped translated values for language quality, not every unrelated
resource value. Do not create a second checker harness, repeat successful
batches, or delegate another review. The existing checker owns validation;
you own patch-derived scope, language judgment, and the safe-output decision.

## Output contract

Do not create an aggregate checker-report file. Write the human-readable
Markdown summary using the shared template. Passing checks do not suppress
concrete human-language findings from your read-only review.
Emit zero safe outputs only when all actual checks are `PASS` and your review
has no findings. Emit exactly one concise `add_comment` tool call when any actual check
is `FIXABLE` or your review finds a concrete human-language issue.
`BLOCKED` or `INVALID_INPUT` is not a successful guide outcome: describe the
limitation in the summary and use the appropriate native incomplete outcome
instead of a success acknowledgement.

Do not print all bundles into the model context; inspect summaries and non-PASS
findings only. Do not overwrite the runtime-owned safe-output queue. Use
PowerShell file operations for the summary; shell redirects, `mkdir`, `touch`,
and the edit tool are not permitted.

If the English-derived scope is deletion-only and the current tree no longer
contains one or more removed source/target files, use the prepared immutable
pre-deletion snapshots for exactly those files and run syntax and encoding
checks on those snapshots. Treat those results as historical evidence only,
and use explicit git inspection separately to prove the live tree really
removed the files.

If an English-source deletion leaves obsolete localized counterparts in the
current tree, report that cleanup in the guidance comment even when historical
syntax and encoding bundles are `PASS`. Use `noop` only when the scoped cleanup
is complete and the review finds no other in-scope issues. Localized-only
deletions still do not independently create guidance scope.

You own scope completeness and language judgment; checker output alone cannot
establish them.

Before any visible output becomes eligible, the native post-step re-reads the
live PR head and rejects stale output when it no longer matches
`${{ github.event.inputs.expected_head_sha }}`. That last-moment check helps,
but it is not a blanket guarantee: the pinned gh-aw docs for `add_comment`
document no additional expected-head compare-and-swap option beyond workflow
concurrency and the final head check, and GitHub cancellation/publication
remain asynchronous.

Follow the shared SKILL's scoped-key rules: keep required keys limited to
source-present additions or updates, handle source removals with the skill's
step-1 explicit review/cleanup path, and pass only source/target-comparable
keys to `Test-PlaceholderParity`, `Test-LockedContent`, and
`Test-PseudoLocale`. Missing comparable entries stay with
`Test-RequiredKeys`; dependent checks across absent entries are genuine
`BLOCKED` outcomes, not successful validation.
Perform this review independently from the caller's proposed key list: derive
again the expected keys from the original patch, treat only English
source-authority additions, updates, or deletions as scope-creating, audit all
shipped localized counterparts implicated by that scope, and fail `PASS` when
any expected key block in that scope is mismatched or omitted.

The comment must:

- say trusted-base file checks found actionable localization issues;
- for mechanical checker findings, list only actual `check`, `file`, and
  `resource` identifiers;
- for human-language findings outside checker JSON, label them separately as
  read-only review findings and list only `file` and `resource` identifiers;
- never invent a checker name for a human review finding or quote untrusted file
  content;
- link only to these applicable references pinned to
  `${{ needs.prepare.outputs.trusted_code_revision }}`:
  - `.github/skills/ensure-localization/SKILL.md`
  - `.github/instructions/localization.instructions.md` for `.resw`
  - `.github/instructions/rust-localization.instructions.md` for WTA YAML;
- include one copyable local Copilot prompt to fix only those findings, rerun
  the matching skill checks, and finish with an independent read-only review;
- state that the workflow neither edited the fork branch nor performed full
  language-quality validation, and do not claim exhaustive localization-quality
  certification.
- before emitting it, inspect the actual native `add_comment` schema or help
  that the runtime exposes, then call that native tool directly with inline
  arguments only: explicit `pr_number`
  `${{ github.event.inputs.pr_number }}` plus the final `body`. Do not stage a
  temp file, heredoc, shell-composed script, or `noop` substitute for the
  required guidance comment.

No branch writes or extra PR comments. A `noop` log acknowledgement is allowed,
but it never replaces a required guidance comment.
