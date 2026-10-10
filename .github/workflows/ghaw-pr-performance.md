---
description: 'Evidence-based review of changed hot paths and callers, with native-tested repair only for eligible HIGH WTA regressions.'
intent: 'Require base/head evidence for repeated work, UI blocking, growing session/log costs, and retained tasks/processes; repair only eligible HIGH WTA changes and otherwise publish a job-summary card without commenting.'

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
  - .github/workflows/ghaw-pr-performance/report-contract.md

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

mcp-scripts:
  validate_performance_report:
    description: 'Validate repair JSON against the fixed caller identity and return field errors before native handoff; never edits source, runs tests, or authorizes publication.'
    inputs:
      report_json:
        type: string
        required: true
        description: 'Complete version-1 repair report as JSON, including location and all other required fields for every finding.'
    env:
      TRUSTED_REVIEW_RUNTIME: '${{ runner.temp }}/performance-trusted.mjs'
      PR_NUMBER: ${{ github.event.inputs.pr_number }}
      BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
      HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
    script: |
      const { pathToFileURL } = await import('node:url');
      const runtime = await import(pathToFileURL(process.env.TRUSTED_REVIEW_RUNTIME).href);
      const report = runtime.validateReport(JSON.parse(report_json), {
        mode: 'repair', prNumber: Number(process.env.PR_NUMBER),
        baseSha: process.env.BASE_SHA, headSha: process.env.HEAD_SHA
      });
      return { status: report.status, findings: report.findings.length };

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
        run: |
          $ErrorActionPreference = 'Stop'
          & node .github/workflows/ghaw-pr-performance/scripts/performance-review.mjs repository-policy --root .
          if ($LASTEXITCODE -ne 0) { throw 'Trusted repository policy no longer matches the native workflow.' }

          if ($env:PR_NUMBER -notmatch '^[1-9][0-9]*$') {
              throw 'PR_NUMBER must be a positive decimal number.'
          }
          foreach ($name in @('BASE_SHA', 'HEAD_SHA', 'EXPECTED_BASE_SHA', 'WORKFLOW_SHA')) {
              if ([Environment]::GetEnvironmentVariable($name) -notmatch '^[0-9a-f]{40}$') {
                  throw "$name must be an immutable lowercase commit SHA."
              }
          }
          if ($env:REPOSITORY -cne $env:GITHUB_REPOSITORY) {
              throw 'Dispatch repository must match the workflow repository.'
          }
          if ($env:SAME_REPO -notin @('true', 'false')) {
              throw 'SAME_REPO must be true or false.'
          }

          $metadataPath = Join-Path $env:RUNNER_TEMP 'performance-live-pr.json'
          $metadata = & gh api "/repos/$env:REPOSITORY/pulls/$env:PR_NUMBER"
          if ($LASTEXITCODE -ne 0) { throw 'Could not read the live PR metadata.' }
          [IO.File]::WriteAllText($metadataPath, ($metadata -join "`n"), [Text.UTF8Encoding]::new($false))
          & node .github/workflows/ghaw-pr-performance/scripts/performance-review.mjs verify-pr `
              --input $metadataPath --pr $env:PR_NUMBER --base $env:BASE_SHA --head $env:HEAD_SHA `
              --expected-base $env:EXPECTED_BASE_SHA --repo $env:REPOSITORY `
              --head-repo $env:HEAD_REPO --same-repo $env:SAME_REPO `
              --head-ref $env:HEAD_REF --base-ref $env:BASE_REF
          if ($LASTEXITCODE -ne 0) { throw 'PR metadata does not match the immutable dispatch.' }

          $remoteRef = "refs/remotes/origin/performance-pr-$env:PR_NUMBER"
          & git -c credential.helper= -c 'credential.helper=!gh auth git-credential' `
              fetch --no-tags origin "refs/pull/$env:PR_NUMBER/head:$remoteRef"
          if ($LASTEXITCODE -ne 0) { throw 'Could not fetch the immutable PR head.' }
          $head = & git rev-parse $remoteRef
          if ($LASTEXITCODE -ne 0 -or $head -cne $env:HEAD_SHA) { throw 'The fetched PR head is stale.' }
          $mergeBase = & git merge-base $env:EXPECTED_BASE_SHA $env:HEAD_SHA
          if ($LASTEXITCODE -ne 0 -or $mergeBase -cne $env:BASE_SHA) {
              throw 'The dispatched comparison base is not the merge base.'
          }
          $changeSummary = (& git diff --no-ext-diff --no-textconv --shortstat --no-renames $env:BASE_SHA $env:HEAD_SHA -- | Out-String).Trim()
          if ($LASTEXITCODE -ne 0) { throw 'Could not summarize the immutable PR change size.' }
          "change_summary=$changeSummary" >> $env:GITHUB_OUTPUT

          if ($env:SAME_REPO -eq 'true') {
              $context = $env:AW_CONTEXT | ConvertFrom-Json
              if ($context.item_type -cne 'pull_request' -or $context.item_number -ne [int]$env:PR_NUMBER -or
                  $context.repo -cne $env:REPOSITORY -or $context.head_sha -cne $env:HEAD_SHA) {
                  throw 'Repair requires the controller-supplied native aw_context for this exact PR.'
              }
          }
          "trusted_code_revision=$env:WORKFLOW_SHA" >> $env:GITHUB_OUTPUT
  performance_analysis:
    needs: [prepare]
    if: github.event.inputs.same_repo == 'true'
    runs-on: windows-latest
    timeout-minutes: 12
    permissions:
      contents: read
    strategy:
      fail-fast: false
      matrix:
        revision: [BASE, HEAD]
    steps:
      - name: Initialize fixed diagnostic artifact before native prerequisites
        shell: pwsh
        env:
          PR_NUMBER: ${{ github.event.inputs.pr_number }}
          BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
          HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
          TRUSTED_SHA: ${{ github.workflow_sha }}
          REVISION: ${{ matrix.revision }}
        run: |
          $output = Join-Path $env:RUNNER_TEMP "performance-analysis-$env:REVISION"
          New-Item -ItemType Directory -Path $output -Force | Out-Null
          $metadata = [ordered]@{
            version = 2; revision = $env:REVISION; status = 'incomplete'
            identity = @{ prNumber = [int]$env:PR_NUMBER; baseSha = $env:BASE_SHA; headSha = $env:HEAD_SHA }
            authoringSha = $env:TRUSTED_SHA
            missingPrerequisites = @('Native checkout or analysis preparation did not complete; inspect GitHub step results.')
          }
          [IO.File]::WriteAllText((Join-Path $output 'analysis-metadata.json'), ($metadata | ConvertTo-Json -Depth 10))
      - name: Checkout trusted analysis code
        uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          repository: ${{ github.event.inputs.repo }}
          ref: ${{ github.workflow_sha }}
          fetch-depth: 0
          persist-credentials: false
          path: trust
      - name: Checkout immutable comparison objects
        uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          repository: ${{ github.event.inputs.repo }}
          ref: ${{ github.event.inputs.expected_head_sha }}
          fetch-depth: 0
          persist-credentials: false
          path: analysis
      - name: Prepare identical HEAD-derived scope before selecting revision
        id: scope
        working-directory: analysis
        shell: pwsh
        env:
          PR_NUMBER: ${{ github.event.inputs.pr_number }}
          BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
          HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
          BASE_TIP: ${{ github.event.inputs.expected_base_sha }}
          REVISION: ${{ matrix.revision }}
        run: |
          $ErrorActionPreference = 'Stop'
          $output = Join-Path $env:RUNNER_TEMP "performance-analysis-$env:REVISION"
          $mergeBase = & git merge-base $env:BASE_TIP $env:HEAD_SHA
          if ($LASTEXITCODE -ne 0 -or $mergeBase -cne $env:BASE_SHA) { throw 'Comparison base must equal actual immutable merge base.' }
          $runtime = Join-Path $env:GITHUB_WORKSPACE 'trust\.github\workflows\ghaw-pr-performance\scripts\performance-review.mjs'
          & node $runtime prepare --output-dir $output --pr $env:PR_NUMBER --base $env:BASE_SHA --head $env:HEAD_SHA
          if ($LASTEXITCODE -ne 0) { throw 'Could not classify immutable comparison.' }
          $scope = Get-Content (Join-Path $output 'performance-scope.json') -Raw | ConvertFrom-Json
          "rust_required=$($scope.analysisPlan.rust.required.ToString().ToLowerInvariant())" >> $env:GITHUB_OUTPUT
          $sha = if ($env:REVISION -eq 'BASE') { $env:BASE_SHA } else { $env:HEAD_SHA }
          & git checkout --detach $sha
          if ($LASTEXITCODE -ne 0) { throw 'Could not checkout immutable analysis revision.' }
      - name: Install explicit comparison Clippy toolchain
        if: steps.scope.outputs.rust_required == 'true'
        shell: pwsh
        run: |
          rustup toolchain install 1.93.0 --profile minimal --component clippy
          if ($LASTEXITCODE -ne 0) { throw 'Public Rust 1.93.0 Clippy is unavailable.' }
      - name: Analyze immutable revision using trusted normal profiles
        shell: pwsh
        env:
          REVISION: ${{ matrix.revision }}
        run: |
          $output = Join-Path $env:RUNNER_TEMP "performance-analysis-$env:REVISION"
          $trust = Join-Path $env:GITHUB_WORKSPACE 'trust'
          & (Join-Path $trust '.github\workflows\ghaw-pr-performance\scripts\run-native-performance-checks.ps1') `
            -Phase Analysis -RepositoryRoot (Join-Path $env:GITHUB_WORKSPACE 'analysis') `
            -TrustedRuntimePath (Join-Path $trust '.github\workflows\ghaw-pr-performance\scripts\performance-review.mjs') `
            -TrustedRepositoryRoot $trust -ScopePath (Join-Path $output 'performance-scope.json') `
            -Revision $env:REVISION -OutputDirectory $output
      - name: Upload fixed source-analysis diagnostics even after failure
        if: always()
        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
        with:
          name: performance-analysis-${{ matrix.revision }}
          path: |
            ${{ runner.temp }}/performance-analysis-${{ matrix.revision }}/analysis-metadata.json
            ${{ runner.temp }}/performance-analysis-${{ matrix.revision }}/performance-scope.json
            ${{ runner.temp }}/performance-analysis-${{ matrix.revision }}/*.log
          if-no-files-found: warn
          retention-days: 7
  agent:
    needs: [prepare, performance_analysis]
    if: always() && needs.prepare.result == 'success'
  safe_outputs:
    if: needs.agent.result == 'success'
    permissions:
      pull-requests: read

pre-agent-steps:
  - name: Download immutable source-analysis inputs into protected runner storage
    continue-on-error: true
    uses: actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c # v8.0.1
    with:
      pattern: performance-analysis-*
      path: ${{ runner.temp }}/gh-aw/performance-analysis
  - name: Expose diagnostic input without granting native validation authority
    shell: bash
    run: |
      set -euo pipefail
      mkdir -p /tmp/gh-aw/performance-analysis
      if [ -d "$RUNNER_TEMP/gh-aw/performance-analysis" ]; then
        cp -R "$RUNNER_TEMP/gh-aw/performance-analysis/." /tmp/gh-aw/performance-analysis/
      fi
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
      git show "${TRUSTED_SHA}:.github/workflows/ghaw-pr-performance/scripts/performance-review.mjs" \
        > "$RUNNER_TEMP/performance-trusted.mjs"
      node "$RUNNER_TEMP/performance-trusted.mjs" prepare \
        --output-dir /tmp/gh-aw \
        --baseline "$RUNNER_TEMP/gh-aw/performance-baseline.json" \
        --safe-outputs "$GH_AW_SAFE_OUTPUTS" \
        --pr "$PR_NUMBER" --base "$BASE_SHA" --head "$HEAD_SHA"

safe-outputs:
  jobs:
    validate-performance-original-tests:
      description: 'List the exact existing test on original HEAD in a fresh Windows VM; never execute candidate replacements.'
      runs-on: windows-latest
      if: needs.agent.result == 'success' && needs.detection.result == 'success'
      permissions:
        contents: read
      inputs:
        confirm:
          description: 'Confirm the sealed HIGH proposal and existing exact test selector.'
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
        - name: Install explicit native Rust toolchain
          timeout-minutes: 10
          shell: pwsh
          run: |
            rustup toolchain install 1.93.0 --profile minimal --component rustfmt
            if ($LASTEXITCODE -ne 0) { throw 'Public Rust 1.93.0 with rustfmt is unavailable.' }
        - name: List the exact test on original HEAD
          timeout-minutes: 32
          working-directory: candidate
          shell: pwsh
          env:
            PR_NUMBER: ${{ github.event.inputs.pr_number }}
            BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
            HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
          run: |
            $ErrorActionPreference = 'Stop'
            $runtime = Join-Path $env:GITHUB_WORKSPACE 'trust\.github\workflows\ghaw-pr-performance\scripts\performance-review.mjs'
            $proposal = Join-Path $env:RUNNER_TEMP 'performance-proposal\performance-proposal.json'
            & node $runtime validate-proposal --input $proposal `
              --pr $env:PR_NUMBER --base $env:BASE_SHA --head $env:HEAD_SHA
            if ($LASTEXITCODE -ne 0) { throw 'Invalid sealed native proposal.' }
            & (Join-Path $env:GITHUB_WORKSPACE 'trust\.github\workflows\ghaw-pr-performance\scripts\run-native-performance-checks.ps1') `
              -Phase OriginalListing -ProposalPath $proposal -RepositoryRoot (Join-Path $env:GITHUB_WORKSPACE 'candidate') `
              -TrustedRuntimePath $runtime
    validate-performance-focused-tests:
      description: 'Check formatting and execute the exact focused candidate test in a fresh Windows VM.'
      runs-on: windows-latest
      if: needs.agent.result == 'success' && needs.detection.result == 'success'
      permissions:
        contents: read
      inputs:
        confirm:
          description: 'Confirm the sealed HIGH proposal and existing exact test selector.'
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
        - name: Install explicit native Rust toolchain
          timeout-minutes: 10
          shell: pwsh
          run: |
            rustup toolchain install 1.93.0 --profile minimal --component rustfmt
            if ($LASTEXITCODE -ne 0) { throw 'Public Rust 1.93.0 with rustfmt is unavailable.' }
        - name: Format and test the exact focused candidate
          timeout-minutes: 32
          working-directory: candidate
          shell: pwsh
          env:
            PR_NUMBER: ${{ github.event.inputs.pr_number }}
            BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
            HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
          run: |
            $ErrorActionPreference = 'Stop'
            $runtime = Join-Path $env:GITHUB_WORKSPACE 'trust\.github\workflows\ghaw-pr-performance\scripts\performance-review.mjs'
            $proposal = Join-Path $env:RUNNER_TEMP 'performance-proposal\performance-proposal.json'
            & node $runtime validate-proposal --input $proposal `
              --pr $env:PR_NUMBER --base $env:BASE_SHA --head $env:HEAD_SHA
            if ($LASTEXITCODE -ne 0) { throw 'Invalid sealed native proposal.' }
            & (Join-Path $env:GITHUB_WORKSPACE 'trust\.github\workflows\ghaw-pr-performance\scripts\run-native-performance-checks.ps1') `
              -Phase Focused -ProposalPath $proposal -RepositoryRoot (Join-Path $env:GITHUB_WORKSPACE 'candidate') `
              -TrustedRuntimePath $runtime
    validate-performance-repair:
      description: 'Run the full Windows suite on the exact sealed HIGH repair in a fresh VM; never publish or rebase.'
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
        - name: Install explicit native Rust toolchain
          timeout-minutes: 10
          shell: pwsh
          run: |
            rustup toolchain install 1.93.0 --profile minimal --component rustfmt
            if ($LASTEXITCODE -ne 0) { throw 'Public Rust 1.93.0 with rustfmt is unavailable.' }
        - name: Test the exact candidate full suite
          timeout-minutes: 32
          working-directory: candidate
          shell: pwsh
          env:
            PR_NUMBER: ${{ github.event.inputs.pr_number }}
            BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
            HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
          run: |
            $ErrorActionPreference = 'Stop'
            $runtime = Join-Path $env:GITHUB_WORKSPACE 'trust\.github\workflows\ghaw-pr-performance\scripts\performance-review.mjs'
            $proposal = Join-Path $env:RUNNER_TEMP 'performance-proposal\performance-proposal.json'
            & node $runtime validate-proposal --input $proposal `
              --pr $env:PR_NUMBER --base $env:BASE_SHA --head $env:HEAD_SHA
            if ($LASTEXITCODE -ne 0) { throw 'Invalid sealed native proposal.' }
            & (Join-Path $env:GITHUB_WORKSPACE 'trust\.github\workflows\ghaw-pr-performance\scripts\run-native-performance-checks.ps1') `
              -Phase FullSuite -ProposalPath $proposal -RepositoryRoot (Join-Path $env:GITHUB_WORKSPACE 'candidate') `
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

  - name: Require an absent post-agent trusted checkout
    if: always()
    shell: bash
    run: |
      set -euo pipefail
      test ! -e "$GITHUB_WORKSPACE/.performance-trusted"
      test ! -L "$GITHUB_WORKSPACE/.performance-trusted"

  - name: Checkout fresh post-agent trust context
    if: always()
    uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
    with:
      repository: ${{ github.event.inputs.repo }}
      ref: ${{ github.workflow_sha }}
      fetch-depth: 0
      persist-credentials: false
      path: .performance-trusted

  - name: Collect review-time Markdown summary independently of report validation
    if: always()
    shell: bash
    run: |
      set -euo pipefail
      mkdir -p "$RUNNER_TEMP/performance-summary"
      if ! node "$GITHUB_WORKSPACE/.performance-trusted/.github/workflows/ghaw-pr-performance/scripts/performance-review.mjs" summary \
        --root "$GITHUB_WORKSPACE" --output-dir "$RUNNER_TEMP/performance-summary"; then
        echo "::warning::Review summary missing or unreadable; see worker run."
      fi

  - name: Upload review-time Markdown summary
    if: always()
    uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
    with:
      name: performance-summary-${{ github.event.inputs.pr_number }}-${{ github.event.inputs.expected_head_sha }}
      path: ${{ runner.temp }}/performance-summary/.performance-summary.md
      include-hidden-files: true
      if-no-files-found: warn
      retention-days: 7

  - name: Validate repair report and changed files
    id: repair_gate
    shell: bash
    env:
      PR_NUMBER: ${{ github.event.inputs.pr_number }}
      BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
      HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
      TRUSTED_SHA: ${{ github.workflow_sha }}
      PERFORMANCE_ANALYSIS_JOB_RESULT: ${{ needs.performance_analysis.result }}
    run: |
      set -euo pipefail
      trusted_root="$GITHUB_WORKSPACE/.performance-trusted"
      cp "$trusted_root/.github/workflows/ghaw-pr-performance/scripts/performance-review.mjs" \
        "$RUNNER_TEMP/performance-trusted.mjs"
      status="$(node "$RUNNER_TEMP/performance-trusted.mjs" gate \
        --trusted-repository-root "$trusted_root" --agent-worktree-root "$GITHUB_WORKSPACE" \
        --analysis-input "$RUNNER_TEMP/gh-aw/performance-analysis" \
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

Focus on changed hot paths and callers for repeated work, UI blocking, growing
session/log costs, and retained tasks/processes. Require base/head evidence,
not speculative allocation cleanup. Only eligible HIGH WTA changes get
native-tested repair.

Use the imported performance reviewer and its
`.github/skills/performance-review/SKILL.md` procedure in `repair` mode.

Immutable PR change size: **${{ needs.prepare.outputs.change_summary }}**.
Use it with the scope's per-file counts and subsystem risk to assess effort;
file and line counts are not proof of severity.

Review PR #${{ github.event.inputs.pr_number }} from comparison base
`${{ github.event.inputs.comparison_base_sha }}` to immutable head
`${{ github.event.inputs.expected_head_sha }}`. Read
`/tmp/gh-aw/performance-scope.json` first and inspect the complete exact patch
and callers.

Before this review, separate fresh Windows BASE and HEAD jobs ran the same
HEAD-derived selected-project plan with trusted extra native profiles. Their
GitHub job result is `${{ needs.performance_analysis.result }}`; the
artifacts under `/tmp/gh-aw/performance-analysis` are diagnostic INPUT, never
proof that a repair passed or permission to publish. Read both fixed
`analysis-metadata.json` files and raw C++ logs / Cargo JSON diagnostic logs.
Verify immutable revisions, authoring configuration, tool versions, scope,
commands and missing prerequisites. Require analysis metadata schema version 2;
an unsupported version is incomplete, never assumed compatible.
Apply the language-specific scope criteria, not a generic source-file list:

- Rust: when `plan.rust.required` is true, completed crate analysis is recorded
  by `analyzedScope.wtaRustCrate: true` and a `rust-analysis` check with
  `status: completed` and `exitCode: 0` in BOTH bound BASE/HEAD records.
  The trusted Cargo alias analyzes the WTA crate and all targets; it does not
  enumerate Rust files as C++ translation units.
- C++: when `plan.cpp.required` is true, inspect metadata
  `analyzedCppTranslationUnits` and `manualScope`. Plan `candidatePaths` use
  provisional prefix recipes, not coverage proof: evaluated MSBuild `ClCompile`
  membership must establish the owning translation units, including imported/
  conditional exclusions. Source absent from either revision is explicitly
  partial, never analyzed there.
- For a Rust-only plan, `plan.cpp.required` is false and the C++ projects,
  candidate paths and `analyzedCppTranslationUnits` lists are EXPECTED empty.
  An empty C++ list never blocks a Rust-only repair. Do not apply C++ membership
  requirements to Rust; use the completed Rust crate/check criteria above.

Require matching revisions/profiles/tools, completed required checks and empty
`missingPrerequisites`/`manualScope` for both records before considering source
analysis complete. This establishes input availability only, not repair safety.
Trace new or worsened warnings in source and affected callers; unchanged
baseline debt, moved lines and intentional
explicit lock drops are not automatic findings. Rank real impact and confidence
using the skill's rule IDs, not warning counts. Record partial/unmapped header
or unsupported source coverage and missing native context in the checks table.
If either required analysis failed, is missing, partial, or has mismatched
profiles, report Incomplete and keep all findings manual: do not edit or request
native autofix jobs. The trusted sealing gate separately enforces this policy.
Even complete source analysis never replaces the three native repair gates.

First write `.performance-summary.md` at the repository root using the skill's
human summary template: normal-prose summary, results tables ordered HIGH,
MEDIUM, LOW and then proposed repair, manual handoff, advice-only. Include the
separate checks table; state "No actionable findings" without invented rows.
These are review-time proposals: never claim `Fixed` before trusted publication.
The Markdown summary is diagnostic only and cannot authorize repair or native success.

Use the imported workflow report contract for the mechanical JSON.
Every finding needs its own `location`
and every other required field, not just the first finding. Submit the complete
JSON to `validate_performance_report`. If it returns a field or identity error,
correct the report and call that tool again; do not request native jobs or
finish with an invalid report. After acceptance, write that exact JSON to
`/tmp/gh-aw/performance-report.json`. Any subsequent report change requires
another tool validation. This gives you correction feedback before handoff;
trusted post-processing still independently validates the persisted report
before sealing or native execution. Do not run a shell validator or renderer:
the human summary is already your explicit Markdown artifact.
Medium/low findings are advice only. Unresolved
or unsafe HIGH findings are not edited. Edit only one or more original
candidate files when the skill's HIGH repair eligibility is fully met, identify the
supported existing focused WTA test selector, and record a validation plan.
Do not execute product tests on Linux or invent native test results.
Keep the source proposal formatted, inspect the resulting diff, and keep only
permitted repair changes. Use standard `cargo fmt` if the caller's permitted
formatter is available; otherwise follow the existing Rust formatting without
claiming that a formatter ran. Windows validation will check
formatting, run that focused selector, and run the required full WTA suite;
all stages must pass on the sealed proposal.
Write the fixed report and summary through the permitted file-editing tool.
Direct Node command variants are not permitted; a denied operation is not
permission to try alternate executable names or bypass trusted validation.
Agent-side shell availability is not native validation authority.

For an eligible WTA Rust repair, mark each HIGH repair as `proposed`, status
`pending_validation`. Set `validationPlan.type` to `wta-unit` and select the
actual qualified test name from its function and enclosing module declarations.
Confirm it in the immutable source with Git; do not copy an example selector.
The native backend accepts original candidate `tools/wta/src/*.rs` replacements
only: at most three files, 100 added-plus-deleted lines and 16 KiB of UTF-8
zero-context diff; the replacement transport limit is separately 256 KiB.
Existing inline tests remain byte-identical; dedicated test/support files,
Cargo configuration and toolchain policy are outside automatic repair.
These bounds keep the correction reviewable; trusted reconstruction enforces
them independently of the model.
Request exactly one each of `validate_performance_original_tests`,
`validate_performance_focused_tests`, and `validate_performance_repair`, all with
`confirm: true`. Native post-processing seals the exact replacements. Three
parallel read-only Windows jobs each download the original sealed artifact and
use a fresh hosted VM, checkout and installed toolchain. No native phase imports
state from another. Publication requires all three exact jobs and steps to succeed
in this worker run. Only a separate controller publisher
may atomically commit those tested blobs against the immutable reviewed head.
Do not commit, push, or claim `fixed` yourself.

Unsupported validation plans (including native C++ for now), unsafe HIGH
findings, and medium/low findings remain unedited with `noop`. Never request a
comment. The card remains in the worker job summary.
