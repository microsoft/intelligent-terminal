---
name: 'Intelligent Terminal Security Guide Hosted Test'
description: 'TEST ONLY: one read-only hosted transport proof against immutable fork PR 1075; no publication.'

on:
  push:
    branches:
      - test/yeelam/ghaw-security-guide-20261006
    paths:
      - .github/workflows/ghaw-pr-security-guide-test.md
      - .github/workflows/ghaw-pr-security-guide-test.lock.yml

permissions:
  contents: read
  pull-requests: read
  actions: read
  checks: read
  security-events: read
  copilot-requests: write

engine: copilot
imports:
  - .github/agents/ghaw-pr-security.agent.md
  - shared/ghaw-pr-security-tools.md

skills:
  - .github/skills/ghaw-pr-security

checkout:
  repository: ${{ github.repository }}
  ref: ${{ github.workflow_sha }}
  fetch-depth: 0

tools:
  github: false
  bash: []
  cli-proxy: false
  edit: false

jobs:
  prepare:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    permissions:
      contents: read
      pull-requests: read
    outputs:
      trusted_code_revision: ${{ steps.validate.outputs.trusted_code_revision }}
    steps:
      - name: Verify fixed official test identity
        id: validate
        shell: bash
        env:
          GH_TOKEN: ${{ github.token }}
          REPOSITORY: ${{ github.repository }}
          WORKFLOW_REPOSITORY: ${{ github.workflow_ref }}
          BRANCH: ${{ github.ref }}
          TRUSTED_WORKFLOW_SHA: ${{ github.workflow_sha }}
          RUN_ATTEMPT: ${{ github.run_attempt }}
        run: |
          set -euo pipefail
          [ "$REPOSITORY" = microsoft/intelligent-terminal ]
          [ "$WORKFLOW_REPOSITORY" = "microsoft/intelligent-terminal/.github/workflows/ghaw-pr-security-guide-test.lock.yml@refs/heads/test/yeelam/ghaw-security-guide-20261006" ]
          [ "$BRANCH" = refs/heads/test/yeelam/ghaw-security-guide-20261006 ]
          [ "$RUN_ATTEMPT" = 1 ]
          [[ "$TRUSTED_WORKFLOW_SHA" =~ ^[0-9a-f]{40}$ ]]
          identity="$(gh api /repos/microsoft/intelligent-terminal/pulls/1075 \
            --jq '[.number, .head.sha, .head.repo.full_name] | @tsv')"
          [ "$identity" = $'1075\t6e3a41af0a66727dd20dcf0e89b8f467e09db059\tyeelam-gordon/intelligent-terminal' ]
          echo "trusted_code_revision=$TRUSTED_WORKFLOW_SHA" >> "$GITHUB_OUTPUT"

  agent:
    needs: [prepare]

safe-outputs:
  staged: true
  report-failure-as-issue: false
  create-check-run:
    max: 1
    staged: true
  missing-data:
    create-issue: false
  missing-tool:
    create-issue: false
  noop:
    report-as-issue: false
  report-incomplete:
    create-issue: false

steps:
  - name: Fetch only the immutable official PR head
    shell: bash
    env:
      GH_TOKEN: ${{ github.token }}
    run: |
      set -euo pipefail
      header="$(printf 'x-access-token:%s' "$GH_TOKEN" | base64 -w0)"
      git -c "http.extraheader=Authorization: Basic ${header}" fetch --quiet --no-tags origin \
        "+refs/pull/1075/head:refs/gh-aw/security-target"
      [ "$(git rev-parse refs/gh-aw/security-target)" = 6e3a41af0a66727dd20dcf0e89b8f467e09db059 ]

  - name: Initialize historical comparison scope and native report
    shell: bash
    env:
      TRUSTED_SHA: ${{ needs.prepare.outputs.trusted_code_revision }}
    run: |
      set -euo pipefail
      [ "$(git rev-parse HEAD)" = "$TRUSTED_SHA" ]
      trusted_dir="$RUNNER_TEMP/gh-aw/security-guide-test-trusted"
      mkdir -p "$trusted_dir" /tmp/gh-aw/agent
      git show "$TRUSTED_SHA:.github/skills/ghaw-pr-security/scripts/security-review.mjs" > "$trusted_dir/security-review.mjs"
      cp "$trusted_dir/security-review.mjs" "$RUNNER_TEMP/gh-aw/security-review-check.mjs"
      node "$trusted_dir/security-review.mjs" scope \
        --base c40ab2727a3c5c498d320ffe90b761f2982c561f \
        --head 6e3a41af0a66727dd20dcf0e89b8f467e09db059 \
        --pr 1075 --relation fork --mode guide \
        --output /tmp/gh-aw/security-scope.json
      [ "$(node -p "JSON.parse(require('fs').readFileSync('/tmp/gh-aw/security-scope.json','utf8')).baseSha")" = c40ab2727a3c5c498d320ffe90b761f2982c561f ]
      cp /tmp/gh-aw/security-scope.json "$RUNNER_TEMP/gh-aw/security-report-scope.json"
      node "$trusted_dir/security-review.mjs" init-report \
        --scope /tmp/gh-aw/security-scope.json \
        --output /tmp/gh-aw/agent/security-findings.json

pre-agent-steps:
  - name: Remove checkout credentials and capture restored runtime baseline
    shell: bash
    env:
      TRUSTED_SHA: ${{ needs.prepare.outputs.trusted_code_revision }}
    run: |
      set -euo pipefail
      bash "${RUNNER_TEMP}/gh-aw/actions/clean_git_credentials.sh"
      [ "$(git rev-parse HEAD)" = "$TRUSTED_SHA" ]
      # gh-aw has now restored trusted config and skills. This directory is
      # mounted read-only to the agent, unlike its report/context directory.
      trusted_dir="$RUNNER_TEMP/gh-aw/security-guide-test-trusted"
      git status --porcelain=v1 -z --untracked-files=all > "$trusted_dir/baseline-status"
      git diff --no-ext-diff --binary HEAD > "$trusted_dir/baseline-diff"
      git ls-files --stage -z > "$trusted_dir/baseline-index"
      git ls-files --cached --others --exclude-standard -z |
        xargs -0 -r sha256sum -- > "$trusted_dir/baseline-files"
      git ls-files --cached --others --exclude-standard -z |
        xargs -0 -r stat --printf='%a %F %N\0' -- > "$trusted_dir/baseline-metadata"

post-steps:
  - name: Re-derive trusted scope and validate hosted guide artifact
    shell: bash
    env:
      GH_TOKEN: ${{ github.token }}
      TRUSTED_SHA: ${{ needs.prepare.outputs.trusted_code_revision }}
    run: |
      set -euo pipefail
      identity="$(gh api /repos/microsoft/intelligent-terminal/pulls/1075 \
        --jq '[.number, .head.sha, .head.repo.full_name] | @tsv')"
      [ "$identity" = $'1075\t6e3a41af0a66727dd20dcf0e89b8f467e09db059\tyeelam-gordon/intelligent-terminal' ] || {
        echo "::error::Immutable PR identity is stale; test rejected without retry."
        exit 1
      }
      [ "$(git rev-parse HEAD)" = "$TRUSTED_SHA" ]
      [ "$(git rev-parse refs/gh-aw/security-target)" = 6e3a41af0a66727dd20dcf0e89b8f467e09db059 ]
      trusted_dir="$RUNNER_TEMP/gh-aw/security-guide-test-trusted"
      git status --porcelain=v1 -z --untracked-files=all > "$trusted_dir/final-status"
      git diff --no-ext-diff --binary HEAD > "$trusted_dir/final-diff"
      git ls-files --stage -z > "$trusted_dir/final-index"
      git ls-files --cached --others --exclude-standard -z |
        xargs -0 -r sha256sum -- > "$trusted_dir/final-files"
      git ls-files --cached --others --exclude-standard -z |
        xargs -0 -r stat --printf='%a %F %N\0' -- > "$trusted_dir/final-metadata"
      cmp "$trusted_dir/baseline-status" "$trusted_dir/final-status"
      cmp "$trusted_dir/baseline-diff" "$trusted_dir/final-diff"
      cmp "$trusted_dir/baseline-index" "$trusted_dir/final-index"
      cmp "$trusted_dir/baseline-files" "$trusted_dir/final-files"
      cmp "$trusted_dir/baseline-metadata" "$trusted_dir/final-metadata"
      git show "$TRUSTED_SHA:.github/skills/ghaw-pr-security/scripts/security-review.mjs" > "$trusted_dir/security-review-final.mjs"
      node "$trusted_dir/security-review-final.mjs" scope \
        --base c40ab2727a3c5c498d320ffe90b761f2982c561f \
        --head 6e3a41af0a66727dd20dcf0e89b8f467e09db059 \
        --pr 1075 --relation fork --mode guide \
        --output "$trusted_dir/final-scope.json"
      [ "$(node -p "JSON.parse(require('fs').readFileSync(process.argv[1],'utf8')).baseSha" "$trusted_dir/final-scope.json")" = c40ab2727a3c5c498d320ffe90b761f2982c561f ]
      cmp "$trusted_dir/final-scope.json" /tmp/gh-aw/security-scope.json
      node "$trusted_dir/security-review-final.mjs" validate \
        --scope "$trusted_dir/final-scope.json" \
        --report /tmp/gh-aw/agent/security-findings.json \
        --validated /tmp/gh-aw/security-findings.validated.json \
        --summary /tmp/gh-aw/security-summary.md \
        --status /tmp/gh-aw/security-status.txt
      node "$trusted_dir/security-review-final.mjs" validate-output \
        --validated /tmp/gh-aw/security-findings.validated.json \
        --agent-output /tmp/gh-aw/agent_output.json
      cp "$trusted_dir/final-scope.json" /tmp/gh-aw/security-scope.trusted.json
      # A valid blocking HIGH review is evidence, not a transport failure.
      # Deliberately do not invoke the production publication/enforce command.
      cat /tmp/gh-aw/security-summary.md >> "$GITHUB_STEP_SUMMARY"

  - name: Upload hosted test evidence only
    if: always()
    uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
    with:
      name: security-guide-test-${{ github.run_id }}
      path: |
        /tmp/gh-aw/security-scope.trusted.json
        /tmp/gh-aw/security-findings.validated.json
        /tmp/gh-aw/security-summary.md
        /tmp/gh-aw/security-status.txt
      if-no-files-found: error
      retention-days: 14

timeout-minutes: 25
max-ai-credits: 400
max-daily-ai-credits: 1200

concurrency:
  group: ghaw-pr-security-guide-test-20261006
  job-discriminator: ${{ github.run_id }}
  cancel-in-progress: false

run-name: 'Security GUIDE TEST PR1075 ${{ github.run_id }}'
---

TEST ONLY: prove hosted GUIDE transport for immutable official fork PR 1075.
This is not production publication or authorization to mutate any branch.

Read `/tmp/gh-aw/security-scope.json`, then follow the trusted
`.github/skills/ghaw-pr-security/SKILL.md` and shared agent in `guide` mode.
Use exactly `scope.baseSha` and `scope.headSha` for the historical comparison,
not the authoring/test branch, current main, or workflow SHA. Read every patch
hunk in bounded path groups with `read-security-diff`; summaries alone do not
constitute review. Trace immutable base/head source bytes with
`read-security-source`, never execute PR-controlled code.

Remain on the trusted authoring checkout. Preserve every native report identity
field and an empty `patch`, then submit complete JSON as the `report_json` string
to `submit-security-report`. This fixed data-only capability checks the contract
against protected native scope before writing
`/tmp/gh-aw/agent/security-findings.json`; the advisory scope/template does not
authorize tool-server operations. Do not recreate the report envelope, write
runtime agent output, add report wrappers, run another agent or CLI session,
probe denied tools, or expose secret-bearing source data. The shared skill
supplies all review reasoning and the report contract. Never write reports via
filesystem tools, PowerShell, shell redirects, temporary payload files, or a
validator CLI. Correct rejected JSON and resubmit until the tool accepts it
before `noop`. Keep the summary under 800 characters, use only accepted
categories, and mark medium/low findings `advice-only`.

The earlier hosted PowerShell transport pass does not prove these fixed MCP
capabilities work in the hosted runtime. This workflow requires a fresh hosted
capability proof before transport readiness can be claimed.

Call `noop` exactly once, including when HIGH findings remain blocked. Do not
emit comments, commits, issues, check runs, or any other output. The trusted
native post-step validates report, queued output, stale head, re-derived scope,
and restored worktree integrity before producing test evidence only.
