---
description: 'Review performance changes and push one small proposal commit per HIGH finding without building or testing.'
intent: 'The agent reviews immutable source, commits minimal HIGH fixes and writes the summary; the original reviewer owns build, test and rollback.'

on:
  workflow_dispatch:
    inputs:
      dispatch_id: { description: 'Controller correlation identifier', required: true, type: string }
      pr_number: { description: 'Pull request number', required: true, type: string }
      repo: { description: 'Target owner/repository', required: true, type: string }
      expected_head_sha: { description: 'Immutable pull request head', required: true, type: string }
      expected_base_sha: { description: 'Observed pull request base tip', required: true, type: string }
      comparison_base_sha: { description: 'Controller-resolved merge base', required: true, type: string }
      head_ref: { description: 'Pull request head branch', required: true, type: string }
      base_ref: { description: 'Pull request base branch', required: true, type: string }
      head_repo: { description: 'Pull request head repository', required: true, type: string }
      same_repo: { description: 'Whether head and base repositories match', required: true, type: string }

permissions:
  contents: read
  pull-requests: read
  copilot-requests: write

engine:
  id: copilot
  model: auto
imports:
  - .github/agents/performance-review.agent.md

checkout:
  ref: ${{ github.event.inputs.head_ref }}
  fetch-depth: 0

tools:
  edit: true
  bash:
    - 'git diff:*'
    - 'git grep:*'
    - 'git log:*'
    - 'git merge-base:*'
    - 'git rev-parse:*'
    - 'git show:*'

jobs:
  conclusion:
    if: 'false'
  prepare:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    permissions:
      contents: read
      pull-requests: read
    outputs:
      change_summary: ${{ steps.verify.outputs.change_summary }}
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          ref: ${{ github.workflow_sha }}
          fetch-depth: 0
          persist-credentials: false
      - name: Verify live PR identity
        id: verify
        shell: pwsh
        env:
          GH_TOKEN: ${{ github.token }}
          PR_NUMBER: ${{ github.event.inputs.pr_number }}
          BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
          HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
          EXPECTED_BASE_SHA: ${{ github.event.inputs.expected_base_sha }}
          REPOSITORY: ${{ github.event.inputs.repo }}
          HEAD_REPO: ${{ github.event.inputs.head_repo }}
          HEAD_REF: ${{ github.event.inputs.head_ref }}
          BASE_REF: ${{ github.event.inputs.base_ref }}
          SAME_REPO: ${{ github.event.inputs.same_repo }}
        run: |
          $ErrorActionPreference = 'Stop'
          if ($env:PR_NUMBER -notmatch '^[1-9][0-9]*$') { throw 'PR_NUMBER must be a positive number.' }
          foreach ($name in @('BASE_SHA', 'HEAD_SHA', 'EXPECTED_BASE_SHA')) {
              if ([Environment]::GetEnvironmentVariable($name) -notmatch '^[0-9a-f]{40}$') { throw "$name must be an immutable SHA." }
          }
          if ($env:REPOSITORY -cne $env:GITHUB_REPOSITORY -or $env:SAME_REPO -cne 'true') { throw 'Proposal push requires this repository PR.' }
          $metadata = & gh api "/repos/$env:REPOSITORY/pulls/$env:PR_NUMBER"
          if ($LASTEXITCODE -ne 0) { throw 'Could not read live PR metadata.' }
          $file = Join-Path $env:RUNNER_TEMP 'performance-pr.json'
          [IO.File]::WriteAllText($file, ($metadata -join "`n"), [Text.UTF8Encoding]::new($false))
          & node .github/workflows/ghaw-pr-performance/scripts/performance-review.mjs verify-pr `
            --input $file --pr $env:PR_NUMBER --base $env:BASE_SHA --head $env:HEAD_SHA `
            --expected-base $env:EXPECTED_BASE_SHA --repo $env:REPOSITORY `
            --head-repo $env:HEAD_REPO --head-ref $env:HEAD_REF --base-ref $env:BASE_REF --same-repo true
          if ($LASTEXITCODE -ne 0) { throw 'Live PR does not match the immutable review inputs.' }
          $remoteRef = "refs/remotes/origin/performance-pr-$env:PR_NUMBER"
          & git -c credential.helper= -c 'credential.helper=!gh auth git-credential' fetch --no-tags origin "refs/pull/$env:PR_NUMBER/head:$remoteRef"
          if ($LASTEXITCODE -ne 0) { throw 'Could not fetch reviewed head.' }
          if ((& git rev-parse $remoteRef) -cne $env:HEAD_SHA) { throw 'Fetched PR head is stale.' }
          if ((& git merge-base $env:EXPECTED_BASE_SHA $env:HEAD_SHA) -cne $env:BASE_SHA) { throw 'Comparison base is not the merge base.' }
          $summary = (& git diff --no-ext-diff --no-textconv --shortstat --no-renames $env:BASE_SHA $env:HEAD_SHA -- | Out-String).Trim()
          if ($LASTEXITCODE -ne 0) { throw 'Could not summarize source changes.' }
          "change_summary=$summary" >> $env:GITHUB_OUTPUT
  agent:
    needs: [prepare]
  safe_outputs:
    if: needs.agent.result == 'success' && needs.detection.result == 'success'

pre-agent-steps:
  - name: Prepare immutable review inputs
    shell: bash
    env:
      PR_NUMBER: ${{ github.event.inputs.pr_number }}
      BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
      HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
      HEAD_REF: ${{ github.event.inputs.head_ref }}
      TRUSTED_SHA: ${{ github.workflow_sha }}
    run: |
      set -euo pipefail
      test "$(git rev-parse HEAD)" = "$HEAD_SHA"
      git show "${TRUSTED_SHA}:.github/workflows/ghaw-pr-performance/scripts/performance-review.mjs" > "$RUNNER_TEMP/performance-trusted.mjs"
      node "$RUNNER_TEMP/performance-trusted.mjs" prepare --output-dir /tmp/gh-aw \
        --pr "$PR_NUMBER" --base "$BASE_SHA" --head "$HEAD_SHA"
      test "$HEAD_REF" != "performance-proposal-$PR_NUMBER"
      git update-ref "refs/remotes/origin/$HEAD_REF" "$HEAD_SHA"
      git update-ref "refs/remotes/origin/performance-proposal-$PR_NUMBER" "$HEAD_SHA"
      git checkout -B "performance-proposal-$PR_NUMBER" "$HEAD_SHA"

safe-outputs:
  report-failure-as-issue: false
  report-incomplete:
    create-issue: false
  noop:
    report-as-issue: false
  missing-tool: false
  missing-data: false
  threat-detection:
    continue-on-error: false
  push-to-pull-request-branch:
    target: '${{ github.event.inputs.pr_number }}'
    base-branch: '${{ github.event.inputs.head_ref }}'
    signed-commits: false
    patch-format: bundle
    max: 1
    allowed-files:
      - tools/wta/src/**/*.rs
    protected-files: blocked
    fallback-as-pull-request: false
    github-token: ${{ secrets.GITHUB_TOKEN }}
    github-token-for-extra-empty-commit: "${{ '' }}"

post-steps:
  - name: Verify proposal commits and unchanged PR head
    shell: bash
    env:
      GH_TOKEN: ${{ github.token }}
      PR_NUMBER: ${{ github.event.inputs.pr_number }}
      BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
      HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
      REPOSITORY: ${{ github.event.inputs.repo }}
    run: |
      set -euo pipefail
      test "$(gh api "/repos/${REPOSITORY}/pulls/${PR_NUMBER}" --jq .head.sha)" = "$HEAD_SHA"
      mkdir -p "$RUNNER_TEMP/performance-summary"
      node "$RUNNER_TEMP/performance-trusted.mjs" verify-commits \
        --pr "$PR_NUMBER" --base "$BASE_SHA" --head "$HEAD_SHA" --repo "$REPOSITORY" \
        --agent-output /tmp/gh-aw/agent_output.json --actions-dir "$RUNNER_TEMP/gh-aw/actions" \
        > "$RUNNER_TEMP/performance-summary/review-head.txt"
  - name: Collect agent-written summary
    if: always()
    shell: bash
    run: |
      set -euo pipefail
      mkdir -p "$RUNNER_TEMP/performance-summary"
      RUNTIME="$RUNNER_TEMP/performance-trusted.mjs" OUTPUT="$RUNNER_TEMP/performance-summary" node --input-type=module <<'NODE'
      import fs from 'node:fs';
      import path from 'node:path';
      import { pathToFileURL } from 'node:url';
      const { readReviewSummary } = await import(pathToFileURL(process.env.RUNTIME).href);
      fs.writeFileSync(path.join(process.env.OUTPUT, '.performance-summary.md'), readReviewSummary('/tmp/gh-aw/agent/performance-summary.md'));
      NODE
  - name: Upload agent-written summary
    if: always()
    uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
    with:
      name: performance-summary-${{ github.event.inputs.pr_number }}-${{ github.event.inputs.expected_head_sha }}
      path: |
        ${{ runner.temp }}/performance-summary/.performance-summary.md
        ${{ runner.temp }}/performance-summary/review-head.txt
      include-hidden-files: true
      if-no-files-found: warn
      retention-days: 7

timeout-minutes: 15
max-ai-credits: 500
max-daily-ai-credits: 100000
concurrency:
  group: 'ghaw-pr-performance-repair-${{ github.event.inputs.pr_number }}'
  job-discriminator: ${{ github.run_id }}
  cancel-in-progress: true
run-name: 'Performance repair ${{ github.event.inputs.dispatch_id }}'
---

Use the imported role and shared performance-review skill in repair mode.
Review PR #${{ github.event.inputs.pr_number }} from base
`${{ github.event.inputs.comparison_base_sha }}` to reviewed head
`${{ github.event.inputs.expected_head_sha }}`. Read `/tmp/gh-aw/performance-scope.json`
and the complete patch/callers. Change size: **${{ needs.prepare.outputs.change_summary }}**.

Classify every finding HIGH/MEDIUM/LOW before selecting edits. Only small,
localized, high-confidence HIGH findings with relevant impact evidence qualify.
MEDIUM/LOW remain advice; unsafe, uncertain or broad HIGH changes require manual
handoff. Do not build, test or execute product code. The original reviewer owns
validation and may revert a proposed fix commit.

For each eligible HIGH finding, make exactly one separate local Git commit on
the checked-out `performance-proposal-${{ github.event.inputs.pr_number }}` branch.
Subject: `Propose HIGH PERF-<stable-id>: <short remedy>`.
Stage only its original candidate WTA Rust source; never the summary or tests.
Do not combine findings, rebase, merge, amend or add unrelated cleanup. Preserve
existing inline tests byte-for-byte. The aggregate proposal limit is three files,
100 added-plus-deleted lines and 16 KiB of zero-context diff.

Always write `/tmp/gh-aw/agent/performance-summary.md` using the shared template,
even when there are no findings or no eligible commits. This directory is
pre-created by the framework. Use the existing file-write tool; do not create
another directory or put the report in the repository root.
Each fixed finding's Commit cell links its actual local SHA:
`${{ github.server_url }}/${{ github.repository }}/commit/<sha>`.
Include the single shared unbuilt/untested reviewer/rollback notice below the
findings table. Do not claim a verified fix or fabricate build/test results.
Finish this file before requesting either push or `noop`. A report in
`noop.message` does not replace the required file.

If commits exist, request exactly one `push_to_pull_request_branch` for this PR
with branch `performance-proposal-${{ github.event.inputs.pr_number }}`;
bundle transport and unsigned push preserve the separate commit IDs. No fallback
PR, extra empty CI commit, direct shell push or PR comment is permitted.
After requesting push, stop; do not change commits or the summary afterward.
If there are no eligible commits, leave source unchanged and request exactly
one `noop`. The controller delivers your Markdown after this worker completes;
it does not generate or rewrite your findings table.
