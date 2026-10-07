---
description: 'Same-repository HIGH-only performance repair worker dispatched by ghaw-pr-performance-controller.yml.'
intent: 'Apply only small, evidenced, behavior-preserving HIGH performance fixes and otherwise publish a job-summary card without commenting.'

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
  - .github/agents/ghaw-pr-performance.agent.md

checkout:
  ref: ${{ github.event.inputs.expected_head_sha }}
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
    - 'pwsh:*'

jobs:
  prepare:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    permissions:
      contents: read
      pull-requests: read
    outputs:
      trusted_code_revision: ${{ steps.verify.outputs.trusted_code_revision }}
      change_summary: ${{ steps.verify.outputs.change_summary }}
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          ref: ${{ github.workflow_sha }}
          fetch-depth: 0
          persist-credentials: false
      - name: Verify live PR identity and repair context
        id: verify
        shell: pwsh
        env:
          GH_TOKEN: ${{ github.token }}
          PR_NUMBER: ${{ github.event.inputs.pr_number }}
          BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
          HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
          EXPECTED_BASE_SHA: ${{ github.event.inputs.expected_base_sha }}
          WORKFLOW_SHA: ${{ github.workflow_sha }}
          REPOSITORY: ${{ github.event.inputs.repo }}
          HEAD_REPO: ${{ github.event.inputs.head_repo }}
          SAME_REPO: ${{ github.event.inputs.same_repo }}
          HEAD_REF: ${{ github.event.inputs.head_ref }}
          BASE_REF: ${{ github.event.inputs.base_ref }}
          AW_CONTEXT: ${{ github.event.inputs.aw_context }}
        run: ./.github/scripts/ghaw-pr-performance/prepare-worker.ps1
  agent:
    needs: [prepare]
  safe_outputs:
    if: needs.agent.result == 'success'
    permissions:
      pull-requests: read

pre-agent-steps:
  - name: Classify immutable performance scope
    shell: bash
    env:
      GH_TOKEN: ${{ github.token }}
      PR_NUMBER: ${{ github.event.inputs.pr_number }}
      BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
      HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
      SAME_REPO: ${{ github.event.inputs.same_repo }}
      TRUSTED_SHA: ${{ github.workflow_sha }}
      GH_AW_SAFE_OUTPUTS: ${{ steps.set-runtime-paths.outputs.GH_AW_SAFE_OUTPUTS }}
    run: |
      set -euo pipefail
      test "$SAME_REPO" = "true"
      mkdir -p /tmp/gh-aw "$(dirname "$GH_AW_SAFE_OUTPUTS")"
      test "$(git rev-parse HEAD)" = "$HEAD_SHA"
      git show "${TRUSTED_SHA}:.github/skills/pr-performance-review/scripts/performance-review.mjs" \
        > "$RUNNER_TEMP/performance-trusted.mjs"
      node "$RUNNER_TEMP/performance-trusted.mjs" prepare \
        --output-dir /tmp/gh-aw \
        --baseline "$RUNNER_TEMP/gh-aw/performance-baseline.json" \
        --safe-outputs "$GH_AW_SAFE_OUTPUTS" \
        --pr "$PR_NUMBER" --base "$BASE_SHA" --head "$HEAD_SHA"

safe-outputs:
  jobs:
    validate-performance-repair:
      description: 'Run actual focused Windows tests on the exact natively sealed HIGH repair proposal; never publish or rebase.'
      runs-on: windows-latest
      if: needs.agent.result == 'success' && needs.detection.result == 'success'
      permissions:
        contents: read
      inputs:
        confirm:
          description: 'Confirm that the final workspace contains only eligible HIGH proposals and the report names a supported native validation plan.'
          required: true
          type: boolean
      steps:
        - name: Checkout trusted validation code
          uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
          with:
            ref: ${{ github.workflow_sha }}
            fetch-depth: 0
            persist-credentials: false
            path: trust
        - name: Checkout immutable candidate head
          uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
          with:
            ref: ${{ github.event.inputs.expected_head_sha }}
            fetch-depth: 0
            persist-credentials: false
            path: candidate
        - name: Download natively sealed proposal
          uses: actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c # v8.0.1
          with:
            name: performance-result
            path: '${{ runner.temp }}\performance-proposal'
        - name: Validate and test the exact candidate tree
          timeout-minutes: 32
          shell: pwsh
          env:
            PR_NUMBER: ${{ github.event.inputs.pr_number }}
            BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
            HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
          run: |
            $ErrorActionPreference = 'Stop'
            $runtime = Join-Path $env:GITHUB_WORKSPACE 'trust\.github\skills\pr-performance-review\scripts\performance-review.mjs'
            $proposal = Join-Path $env:RUNNER_TEMP 'performance-proposal\performance-proposal.json'
            & node $runtime validate-proposal --input $proposal `
              --pr $env:PR_NUMBER --base $env:BASE_SHA --head $env:HEAD_SHA
            if ($LASTEXITCODE -ne 0) { throw 'Invalid sealed native proposal.' }
            & (Join-Path $env:GITHUB_WORKSPACE 'trust\.github\scripts\ghaw-pr-performance\validate-native.ps1') `
              -ProposalPath $proposal -RepositoryRoot (Join-Path $env:GITHUB_WORKSPACE 'candidate') `
              -TrustedRuntimePath $runtime

post-steps:
  - name: Reject stale repair output
    shell: bash
    env:
      GH_TOKEN: ${{ github.token }}
      PR_NUMBER: ${{ github.event.inputs.pr_number }}
      EXPECTED_HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
      REPOSITORY: ${{ github.event.inputs.repo }}
    run: |
      set -euo pipefail
      current_head="$(gh api "/repos/${REPOSITORY}/pulls/${PR_NUMBER}" --jq .head.sha)"
      test "$current_head" = "$EXPECTED_HEAD_SHA" || {
        echo "::error::Stale performance repair rejected."
        exit 1
      }

  - name: Validate repair report and changed files
    id: repair_gate
    shell: bash
    env:
      PR_NUMBER: ${{ github.event.inputs.pr_number }}
      BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
      HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
      TRUSTED_SHA: ${{ github.workflow_sha }}
    run: |
      set -euo pipefail
      git show "${TRUSTED_SHA}:.github/skills/pr-performance-review/scripts/performance-review.mjs" \
        > "$RUNNER_TEMP/performance-trusted.mjs"
      status="$(node "$RUNNER_TEMP/performance-trusted.mjs" gate \
        --output-dir /tmp/gh-aw/performance-result \
        --baseline "$RUNNER_TEMP/gh-aw/performance-baseline.json" \
        --report /tmp/gh-aw/performance-report.json \
        --agent-output /tmp/gh-aw/agent_output.json \
        --mode repair --pr "$PR_NUMBER" --base "$BASE_SHA" --head "$HEAD_SHA")"
      echo "status=$status" >> "$GITHUB_OUTPUT"

  - name: Upload validated performance verdict
    uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
    with:
      name: performance-result
      path: |
        /tmp/gh-aw/performance-result/performance-verdict.json
        /tmp/gh-aw/performance-result/performance-proposal.json
      if-no-files-found: error
      retention-days: 7

  - name: Publish deterministic performance card to job summary
    if: always()
    shell: bash
    env:
      PR_NUMBER: ${{ github.event.inputs.pr_number }}
      BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
      HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
    run: |
      set -euo pipefail
      if [ -f /tmp/gh-aw/performance-report.json ]; then
        node "$RUNNER_TEMP/performance-trusted.mjs" render \
          --report /tmp/gh-aw/performance-report.json \
          --mode repair --pr "$PR_NUMBER" --base "$BASE_SHA" --head "$HEAD_SHA" \
          >> "$GITHUB_STEP_SUMMARY"
      fi

timeout-minutes: 15
max-ai-credits: 500
max-daily-ai-credits: 100000
concurrency:
  group: 'ghaw-pr-performance-repair-${{ github.event.inputs.pr_number }}'
  job-discriminator: ${{ github.run_id }}
  cancel-in-progress: true
run-name: 'Performance repair ${{ github.event.inputs.dispatch_id }}'
---

Use the imported performance reviewer and its
`.github/skills/pr-performance-review/SKILL.md` procedure in `repair` mode.

Immutable PR change size: **${{ needs.prepare.outputs.change_summary }}**.
Use it with the scope's per-file counts and subsystem risk to assess effort;
file and line counts are not proof of severity.

Review PR #${{ github.event.inputs.pr_number }} from comparison base
`${{ github.event.inputs.comparison_base_sha }}` to immutable head
`${{ github.event.inputs.expected_head_sha }}`. Read
`/tmp/gh-aw/performance-scope.json` first and inspect the complete exact patch
and callers.

Write `/tmp/gh-aw/performance-report.json`, validate it with `--mode repair`,
and render it for inspection. Medium/low findings are advice only. Unresolved
or unsafe HIGH findings are not edited. Edit only one or more original
candidate files when the skill's HIGH repair eligibility is fully met, run the
supported existing focused WTA test selector, and record a validation plan.
Do not execute product tests on Linux or invent native test results.
Format the source proposal using standard `cargo fmt`, inspect the resulting
diff, and keep only permitted repair changes. Windows validation will check
formatting, run that focused selector, and run the required full WTA suite;
all stages must pass on the sealed proposal.
Use the skill's permitted PowerShell route for report writes and checker
execution. Direct Node command variants are not permitted; a denied call is
not permission to skip validation or try alternate executable names.

For an eligible WTA Rust repair, mark each HIGH repair as `proposed`, status
`pending_validation`. Set `validationPlan.type` to `wta-unit` and select the
actual qualified test name from its function and enclosing module declarations.
Confirm it in the immutable source with Git; do not copy an example selector.
Request exactly one `validate_performance_repair` with
`confirm: true`. Native post-processing seals the exact replacements, the
read-only Windows job runs the tests, and only a separate controller publisher
may atomically commit those tested blobs against the immutable reviewed head.
Do not commit, push, or claim `fixed` yourself.

Unsupported validation plans (including native C++ for now), unsafe HIGH
findings, and medium/low findings remain unedited with `noop`. Never request a
comment. The card remains in the worker job summary.
