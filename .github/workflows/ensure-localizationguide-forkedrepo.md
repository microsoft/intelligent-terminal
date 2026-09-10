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



engine: copilot

imports:

  - .github/agents/localization-reviewer.agent.md



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

  - name: Validate final localization checker report
    shell: bash
    env:
      LOCALIZATION_REPORT_MODE: guide
    run: |
      set -euo pipefail
      node <<'NODE'
      const fs = require('fs');
      const path = require('path');
      const root = '/tmp/gh-aw';
      const mode = process.env.LOCALIZATION_REPORT_MODE;
      const fail = message => { console.error(`::error::Final localization checker report rejected: ${message}`); process.exit(1); };
      const isObject = value => value !== null && typeof value === 'object' && !Array.isArray(value);

      const readJson = filename => {
        const filenamePath = path.join(root, filename);
        let stat;
        try { stat = fs.lstatSync(filenamePath); } catch { fail(`${filename} is missing`); }
        if (stat.isSymbolicLink() || !stat.isFile() || stat.size < 2 || stat.size > 1024 * 1024) {
          fail(`${filename} must be a regular file within the size limit`);
        }
        let realRoot;
        let realFile;
        try {
          realRoot = fs.realpathSync(root);
          realFile = fs.realpathSync(filenamePath);
        } catch {
          fail(`${filename} could not be resolved`);
        }
        if (path.dirname(realFile) !== realRoot || path.basename(realFile) !== filename) {
          fail(`${filename} resolved outside the fixed runtime location`);
        }
        try { return JSON.parse(fs.readFileSync(filenamePath, 'utf8')); }
        catch { fail(`${filename} is not valid JSON`); }
      };

      const report = readJson('localization-final-checks.json');
      if (!isObject(report) || report.version !== 1 || report.mode !== mode || !Array.isArray(report.bundles) || report.bundles.length === 0) {
        fail('the report envelope is incomplete or has the wrong mode');
      }

      const exitCodes = { PASS: 0, FIXABLE: 20, BLOCKED: 30, INVALID_INPUT: 64 };
      const checks = new Set([
        'Test-ResourceSyntax', 'Test-RequiredKeys', 'Test-PlaceholderParity',
        'Test-LockedContent', 'Test-ResourceEncoding', 'Test-PseudoLocale'
      ]);
      const resultStatuses = new Set(['PASS', 'FIXABLE', 'BLOCKED']);
      const statuses = report.bundles.map((bundle, index) => {
        if (!isObject(bundle) || !checks.has(bundle.check)) {
          fail(`bundle ${index + 1} has an unknown check`);
        }
        if (!Object.hasOwn(exitCodes, bundle.status) || bundle.exitCode !== exitCodes[bundle.status]) {
          fail(`bundle ${index + 1} has an invalid status or exit code`);
        }
        if (!Array.isArray(bundle.results) || bundle.results.length === 0 ||
            bundle.results.some(result => !isObject(result) || !resultStatuses.has(result.status))) {
          fail(`bundle ${index + 1} does not contain valid checker results`);
        }
        const actual = new Set(bundle.results.map(result => result.status));
        const derived = actual.has('BLOCKED') ? 'BLOCKED' : actual.has('FIXABLE') ? 'FIXABLE' : 'PASS';
        if (bundle.status !== derived) {
          fail(`bundle ${index + 1} aggregate status does not match its results`);
        }
        return derived;
      });

      if (statuses.some(status => status === 'BLOCKED')) {
        fail('guide may continue only with PASS or FIXABLE checker bundles');
      }

      const hasFixable = statuses.some(status => status === 'FIXABLE');
      const queuedOutput = readJson('agent_output.json');
      if (!isObject(queuedOutput) || !Array.isArray(queuedOutput.items) ||
          (queuedOutput.errors !== undefined && !Array.isArray(queuedOutput.errors))) {
        fail('the ingested agent output envelope is invalid');
      }
      if (queuedOutput.errors?.length) {
        fail('agent output ingestion reported validation errors');
      }

      const queuedTypes = queuedOutput.items.map(item => isObject(item) ? item.type : undefined);
      if (queuedTypes.some(type => typeof type !== 'string' || type.length === 0)) {
        fail('queued output contains an invalid native type');
      }
      const blockedTypes = new Set(['report_incomplete', 'missing_tool', 'missing_data']);
      if (queuedTypes.some(type => blockedTypes.has(type))) {
        fail('queued output contains a blocked native outcome');
      }

      if (queuedTypes.some(type => !['add_comment', 'noop'].includes(type))) {
        fail('guide permits only its guidance comment and a non-mutating acknowledgement');
      }
      const addCommentCount = queuedTypes.filter(type => type === 'add_comment').length;
      if (addCommentCount > 1) {
        fail(`guide permits at most 1 queued add_comment item, found ${addCommentCount}`);
      }
      if (hasFixable && addCommentCount !== 1) {
        fail(`guide checker outcome requires exactly 1 queued add_comment item when any final checker bundle is FIXABLE, found ${addCommentCount}`);
      }
      NODE

  - name: Upload final localization checker report
    uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
    with:
      name: localization-final-checks
      path: /tmp/gh-aw/localization-final-checks.json
      if-no-files-found: error
      retention-days: 7

timeout-minutes: 15

max-ai-credits: 150

max-daily-ai-credits: 750

concurrency:

  group: 'localization-guide-fork-${{ github.event.inputs.pr_number }}'

  job-discriminator: ${{ github.run_id }}

  cancel-in-progress: true

run-name: 'Ensure Localization Guide Forked Repo ${{ github.event.inputs.dispatch_id }}'

---

Imported runtime role: `localization-reviewer`.

Fork localization guidance for PR #${{ github.event.inputs.pr_number }} in
`${{ github.repository }}`.

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

Materialize trusted file bytes in the workspace only as needed.
You own the git inspection, scope discovery, final checker rerun, final report
write, and the one allowed safe output for this read-only workflow.
Derive the precise source-added or updated keys, values, and surrounding
context from that original patch, preserve that scope through the final rerun,
and do not replace it with guessed keys from unchanged source lines, file
prefixes, samples, or PR summaries.

## Output contract

Remove `/tmp/gh-aw/localization-final-checks.json` at startup. After review,
write only actual final checker bundles to that fixed path:

```json
{"version":1,"mode":"guide","bundles":[/* actual final checker JSON bundles */]}
```

Never hand-author bundle fields or include initial attempts. Final checker
`PASS` bundles do not suppress concrete read-only review findings that stay
outside checker JSON. Emit zero safe outputs only when the final checker bundles
are all `PASS` and the independent read-only review has no findings. Emit
exactly one concise `add-comment` when any final checker bundle is `FIXABLE` or
the independent read-only review finds a concrete human-language issue.
`BLOCKED` or `INVALID_INPUT` is not a successful guide outcome.

Use the SKILL.md batching example for the final rerun: dot-source
`.github/skills/ensure-localization/scripts/localization_checks.ps1` once in
one `pwsh` process, collect the actual function-return bundles, and write the
envelope with PowerShell file operations before emitting either `add-comment` or
`noop`.

The native gate validates report shape and output mechanics only; it does not
prove that you preserved the original patch scope. Your own git evidence must
establish that.

Before any visible output becomes eligible, the native post-step re-reads the
live PR head and rejects stale output when it no longer matches
`${{ github.event.inputs.expected_head_sha }}`. That last-moment check helps,
but it is not a blanket guarantee: the pinned gh-aw docs for `add_comment`
document no additional expected-head compare-and-swap option beyond workflow
concurrency and the final head check, and GitHub cancellation/publication
remain asynchronous.

Follow the shared SKILL's scoped-key rules: keep `RequiredKeys` limited to
source-present additions or updates, handle source removals with the skill's
step-1 explicit review/cleanup path, and pass only source/target-comparable
keys to `Test-PlaceholderParity`, `Test-LockedContent`, and
`Test-PseudoLocale`. Missing comparable entries stay with
`Test-RequiredKeys`; dependent checks across absent entries are genuine
`BLOCKED` outcomes and must not be manufactured into the final guide report.
Perform this review independently from the caller's proposed key list: derive again
the expected keys from the original patch, audit all shipped localized
counterparts implicated by that scope, and fail `PASS` when any expected key
block is mismatched or omitted.

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
