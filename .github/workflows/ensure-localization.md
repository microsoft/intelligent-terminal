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
  - .github/agents/localization-expert.agent.md


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

safe-outputs:

  github-token: ${{ secrets.GITHUB_TOKEN }}

  push-to-pull-request-branch:

    base-branch: ${{ github.event.inputs.expected_head_sha }}

    github-token-for-extra-empty-commit: "${{ '' }}"

    allowed-files:

      - 'src/cascadia/**/Resources/*.resw'

      - 'src/cascadia/**/Resources/**/*.resw'

      - 'tools/wta/locales/*.yml'

    excluded-files:

      - 'src/cascadia/**/Resources/*.resw'

      - 'src/cascadia/**/Resources/en-US/*.resw'

      - 'tools/wta/locales/en-US.yml'

    protected-files: blocked

    if-no-changes: error

    fallback-as-pull-request: false

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
      LOCALIZATION_REPORT_MODE: repair
      EXPECTED_HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
    run: |
      set -euo pipefail
      node <<'NODE'
      const fs = require('fs');
      const path = require('path');
      const { execFileSync } = require('child_process');
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

      if (statuses.some(status => status !== 'PASS')) {
        fail('repair requires every final checker bundle to be PASS');
      }

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

      const addCommentCount = queuedTypes.filter(type => type === 'add_comment').length;
      const pushCount = queuedTypes.filter(type => type === 'push_to_pull_request_branch').length;
      if (queuedTypes.length !== 1 || !((pushCount === 1 && addCommentCount === 0) || (pushCount === 0 && addCommentCount === 1))) {
        fail('repair PASS requires exactly one queued branch push or visible no-change comment');
      }
      if (addCommentCount === 1) {
        const head = process.env.EXPECTED_HEAD_SHA;
        if (!/^[0-9a-f]{40}$/.test(head || '')) fail('repairs not published check received an invalid expected head SHA');
        const paths = [
          ':(glob)src/cascadia/**/Resources/*.resw',
          ':(glob)src/cascadia/**/Resources/**/*.resw',
          ':(glob)tools/wta/locales/*.yml'
        ];
        let dirty;
        try {
          dirty = Buffer.concat([
            execFileSync('git', ['diff', '--name-only', '-z', '--no-ext-diff', '--no-textconv', head, '--', ...paths], { timeout: 15000, maxBuffer: 1024 * 1024 }),
            execFileSync('git', ['ls-files', '--others', '-z', '--', ...paths], { timeout: 15000, maxBuffer: 1024 * 1024 })
          ]);
        } catch (error) { fail(`repairs not published check failed: ${error.message}`); }
        if (dirty.length !== 0) fail('repairs not published: a no-change comment cannot discard working localization repairs');
      }
      NODE

  - name: Upload final localization checker report
    uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
    with:
      name: localization-final-checks
      path: /tmp/gh-aw/localization-final-checks.json
      if-no-files-found: error
      retention-days: 7

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

## Goal

Verify `git rev-parse HEAD` equals
`${{ github.event.inputs.expected_head_sha }}`, then follow
`.github/skills/ensure-localization/SKILL.md` using:

- comparison base `${{ github.event.inputs.comparison_base_sha }}`
- immutable head `${{ github.event.inputs.expected_head_sha }}`
- observed base metadata `${{ github.event.inputs.expected_base_sha }}`
- exact original patch inspection with
  `git diff --no-ext-diff --unified=3 ${{ github.event.inputs.comparison_base_sha }} ${{ github.event.inputs.expected_head_sha }} -- <resource paths>` before choosing scoped keys or targets; treat `--stat`, `--name-only`, `--name-status`, `--numstat`, `git status`, and worktree-only diffs as supporting signals only, and keep reading if the patch output truncates until every relevant hunk is covered

Repair only localized targets. Keep source authority read-only and finish with
the required independent review. Invoke the registered
`localization-review-gate` agent after the final checks and require its explicit
`PASS` before requesting any branch write. The root repair agent owns all git
inspection, scope discovery, edits, the final checker rerun, and writing
`/tmp/gh-aw/localization-final-checks.json`; do not delegate those steps.
Derive the precise source-added or updated keys, values, and surrounding
context from that original patch, preserve that scope through the final rerun,
and do not replace it with guessed keys from unchanged source lines, file
prefixes, samples, or PR summaries.

## Output contract

Remove `/tmp/gh-aw/localization-final-checks.json` at startup. After repair and
review, write only actual final checker bundles to that fixed path:

```json
{"version":1,"mode":"repair","bundles":[/* actual final checker JSON bundles */]}
```

Never hand-author bundle fields or include initial attempts. The native gate
requires final `PASS` bundles and exactly one successful outcome:

- Use the SKILL.md batching example for the final rerun: dot-source
  `.github/skills/ensure-localization/scripts/localization_checks.ps1` once in
  one `pwsh` process, collect the actual function-return bundles, and write the
  envelope with PowerShell file operations before any safe output.
- The native gate validates report shape and output mechanics only; it does not
  prove that you preserved the original patch scope. Your own git evidence and
  independent review must establish that.
- Before any safe output becomes eligible, the native post-step re-reads the
  live PR head and rejects stale output when it no longer matches
  `${{ github.event.inputs.expected_head_sha }}`. That last-moment check helps,
  but it is not a blanket guarantee: the pinned gh-aw docs for
  `push-to-pull-request-branch` document no additional expected-head
  compare-and-swap option beyond workflow concurrency and the final head check,
  and GitHub cancellation/publication remain asynchronous.
- Follow the scoped-key rules in the shared SKILL: keep `RequiredKeys` limited
  to source-present additions or updates, handle source removals with the
  skill's step-1 explicit review/cleanup path, and recompute each row's
  `ComparableKeys` from the final on-disk source/target files after every edit
  so newly translated source-added keys are included before dependent reruns.
- The independent reviewer is read-only and separate. It returns only its
  review verdict and findings; it never owns the final report path or the safe
  output call. Give that reviewer the comparison base, immutable head, exact
  repaired file list, and an explicit requirement to independently re-derive
  expected keys from the original patch instead of from your selected-key list.
- Before emitting any comment, inspect the actual native `add_comment` schema
  or help that the runtime exposes. Then call that native tool directly with
  inline arguments only: explicit `pr_number`
  `${{ github.event.inputs.pr_number }}` plus the final `body`. Do not stage a
  temp file, heredoc, shell-composed script, `target=triggering`, or `noop`
  substitute for a required visible comment.

- No edit: one visible `PASS` / no-change `add-comment` naming checked files.
- Edited: one focused commit whose subject
  ends with `[localization-expert]`, then use `push-to-pull-request-branch`.

Do not claim success for source-only, blocked, invalid, or excluded changes. No
`noop`, extra output, extra commit, or source-authority edit.

## agent: `localization-review-gate`
{{#runtime-import .github/agents/localization-reviewer.agent.md}}

Caller contract for this reviewer: pass the comparison base, immutable head,
the exact repaired or reviewed resource paths, and the current repair summary.
Require the reviewer to inspect the original
`git diff --no-ext-diff --unified=3 <comparison-base> <immutable-head> -- <resource paths>`
content independently, audit all shipped localized counterparts implicated by
that patch scope, and fail `PASS` when any expected key block is mismatched or
omitted.
## end agent: `localization-review-gate`
