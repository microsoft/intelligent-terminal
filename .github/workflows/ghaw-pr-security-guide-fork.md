---
name: 'Intelligent Terminal Security Fork Review'
description: 'Read-only fork PR security worker that may produce one validated guidance artifact.'

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
        description: 'Immutable pull request head'
        required: true
        type: string
      expected_base_sha:
        description: 'Observed pull request base tip'
        required: true
        type: string
      comparison_base_sha:
        description: 'Controller-resolved merge base'
        required: true
        type: string
      head_ref:
        description: 'Pull request head branch'
        required: true
        type: string
      head_repo:
        description: 'Pull request head repository'
        required: true
        type: string

permissions:
  contents: read
  pull-requests: read
  actions: read
  checks: read
  security-events: read
  copilot-requests: write

engine:
  id: copilot
  agent: ghaw-pr-security
  version: '1.0.90'
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
      - name: Validate immutable fork dispatch
        id: validate
        shell: bash
        env:
          GH_TOKEN: ${{ github.token }}
          PR_NUMBER: ${{ github.event.inputs.pr_number }}
          EXPECTED_HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
          EXPECTED_BASE_SHA: ${{ github.event.inputs.expected_base_sha }}
          COMPARISON_BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
          HEAD_REPO: ${{ github.event.inputs.head_repo }}
          TARGET_REPOSITORY: ${{ github.event.inputs.repo }}
          REPOSITORY: ${{ github.repository }}
          TRUSTED_WORKFLOW_SHA: ${{ github.workflow_sha }}
        run: |
          set -euo pipefail
          [[ "$PR_NUMBER" =~ ^[1-9][0-9]*$ ]]
          [[ "$EXPECTED_HEAD_SHA" =~ ^[0-9a-f]{40}$ ]]
          [[ "$EXPECTED_BASE_SHA" =~ ^[0-9a-f]{40}$ ]]
          [[ "$COMPARISON_BASE_SHA" =~ ^[0-9a-f]{40}$ ]]
          [ "$TRUSTED_WORKFLOW_SHA" = "$EXPECTED_BASE_SHA" ]
          [ "$TARGET_REPOSITORY" = "$REPOSITORY" ]
          [ "$HEAD_REPO" != "$REPOSITORY" ]
          current_head="$(gh api "/repos/$REPOSITORY/pulls/$PR_NUMBER" --jq .head.sha)"
          current_head_repo="$(gh api "/repos/$REPOSITORY/pulls/$PR_NUMBER" --jq .head.repo.full_name)"
          [ "$current_head" = "$EXPECTED_HEAD_SHA" ]
          [ "$current_head_repo" = "$HEAD_REPO" ]
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
  - name: Fetch immutable fork head
    shell: bash
    env:
      GH_TOKEN: ${{ github.token }}
      PR_NUMBER: ${{ github.event.inputs.pr_number }}
      EXPECTED_HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
    run: |
      set -euo pipefail
      header="$(printf 'x-access-token:%s' "$GH_TOKEN" | base64 -w0)"
      git -c "http.extraheader=Authorization: Basic ${header}" fetch --quiet --no-tags origin \
        "+refs/pull/${PR_NUMBER}/head:refs/gh-aw/security-target"
      [ "$(git rev-parse refs/gh-aw/security-target)" = "$EXPECTED_HEAD_SHA" ]

  - name: Prepare immutable fork review scope
    shell: bash
    env:
      EXPECTED_BASE_SHA: ${{ github.event.inputs.expected_base_sha }}
      EXPECTED_HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
      COMPARISON_BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
      PR_NUMBER: ${{ github.event.inputs.pr_number }}
      TRUSTED_SHA: ${{ github.workflow_sha }}
    run: |
      set -euo pipefail
      mkdir -p /tmp/gh-aw/agent
      rm -f /tmp/gh-aw/security-scope.json /tmp/gh-aw/agent/security-findings.json
      trusted_validator="$RUNNER_TEMP/security-review.mjs"
      git show "$TRUSTED_SHA:.github/skills/ghaw-pr-security/scripts/security-review.mjs" > "$trusted_validator"
      cp "$trusted_validator" "$RUNNER_TEMP/gh-aw/security-review-check.mjs"
      node "$trusted_validator" scope \
        --base "$EXPECTED_BASE_SHA" \
        --head "$EXPECTED_HEAD_SHA" \
        --pr "$PR_NUMBER" \
        --relation fork \
        --mode guide \
        --output /tmp/gh-aw/security-scope.json
      [ "$(node -p "JSON.parse(require('fs').readFileSync('/tmp/gh-aw/security-scope.json','utf8')).baseSha")" = "$COMPARISON_BASE_SHA" ]
      cp /tmp/gh-aw/security-scope.json "$RUNNER_TEMP/gh-aw/security-report-scope.json"
      node "$trusted_validator" init-report \
        --scope /tmp/gh-aw/security-scope.json \
        --output /tmp/gh-aw/agent/security-findings.json

pre-agent-steps:
  - name: Record trusted fork workspace baseline
    shell: bash
    run: |
      set -euo pipefail
      {
        git rev-parse HEAD
        git status --porcelain --untracked-files=all
        git diff --binary HEAD
      } | sha256sum > "$RUNNER_TEMP/security-guide-workspace.sha256"
  - name: Enforce credential-free agent checkout
    shell: bash
    run: |
      set -euo pipefail
      bash "${RUNNER_TEMP}/gh-aw/actions/clean_git_credentials.sh"
      node "$RUNNER_TEMP/gh-aw/security-review-check.mjs" verify-credentials --workspace "$GITHUB_WORKSPACE"

post-steps:
  - name: Reject stale or malformed fork guidance
    shell: bash
    env:
      GH_TOKEN: ${{ github.token }}
      EXPECTED_HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
      EXPECTED_BASE_SHA: ${{ github.event.inputs.expected_base_sha }}
      COMPARISON_BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
      PR_NUMBER: ${{ github.event.inputs.pr_number }}
      REPOSITORY: ${{ github.repository }}
      TRUSTED_SHA: ${{ github.workflow_sha }}
    run: |
      set -euo pipefail
      current_head="$(gh api "/repos/$REPOSITORY/pulls/$PR_NUMBER" --jq .head.sha)"
      [ "$current_head" = "$EXPECTED_HEAD_SHA" ] || {
        echo "::error::Stale fork security review rejected: expected $EXPECTED_HEAD_SHA, found $current_head."
        exit 1
      }
      {
        git rev-parse HEAD
        git status --porcelain --untracked-files=all
        git diff --binary HEAD
      } | sha256sum > "$RUNNER_TEMP/security-guide-workspace.final.sha256"
      cmp "$RUNNER_TEMP/security-guide-workspace.sha256" "$RUNNER_TEMP/security-guide-workspace.final.sha256" || {
        echo "::error::Fork security guide modified the trusted checkout."
        exit 1
      }
      trusted_validator="$RUNNER_TEMP/security-review-final.mjs"
      git show "$TRUSTED_SHA:.github/skills/ghaw-pr-security/scripts/security-review.mjs" > "$trusted_validator"
      trusted_scope="$RUNNER_TEMP/security-scope.final.json"
      node "$trusted_validator" scope \
        --base "$EXPECTED_BASE_SHA" \
        --head "$EXPECTED_HEAD_SHA" \
        --pr "$PR_NUMBER" \
        --relation fork \
        --mode guide \
        --output "$trusted_scope"
      [ "$(node -p "JSON.parse(require('fs').readFileSync('$trusted_scope','utf8')).baseSha")" = "$COMPARISON_BASE_SHA" ]
      node "$trusted_validator" validate \
        --scope "$trusted_scope" \
        --report /tmp/gh-aw/agent/security-findings.json \
        --validated /tmp/gh-aw/security-findings.validated.json \
        --summary /tmp/gh-aw/security-summary.md \
        --status /tmp/gh-aw/security-status.txt
      node "$trusted_validator" validate-output \
        --validated /tmp/gh-aw/security-findings.validated.json \
        --agent-output /tmp/gh-aw/agent_output.json
      cp "$trusted_scope" /tmp/gh-aw/security-scope.json
      cat /tmp/gh-aw/security-summary.md >> "$GITHUB_STEP_SUMMARY"

  - name: Upload validated fork security report
    if: always()
    uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
    with:
      name: ghaw-pr-security-${{ github.event.inputs.pr_number }}-${{ github.event.inputs.expected_head_sha }}
      path: |
        /tmp/gh-aw/security-scope.json
        /tmp/gh-aw/security-findings.validated.json
        /tmp/gh-aw/security-summary.md
        /tmp/gh-aw/security-status.txt
      if-no-files-found: error
      retention-days: 14

timeout-minutes: 25
max-ai-credits: 400
max-daily-ai-credits: 4000

concurrency:
  group: 'ghaw-pr-security-guide-${{ github.event.inputs.pr_number }}'
  job-discriminator: ${{ github.run_id }}
  cancel-in-progress: true

run-name: 'Security Guide ${{ github.event.inputs.dispatch_id }}'
---

Read `/tmp/gh-aw/security-scope.json`, then follow
`.github/skills/ghaw-pr-security/SKILL.md` in `guide` mode against comparison
base `${{ github.event.inputs.comparison_base_sha }}` and immutable fork head
`${{ github.event.inputs.expected_head_sha }}`.

Stay on the trusted checkout. Inspect every immutable hunk through bounded
`read-security-diff` path groups and trace base/head source with
`read-security-source`. Never execute or copy fork-controlled scripts into an
executable location. Preserve the prepared report's native identity fields and
empty `patch`, then submit complete JSON as the `report_json` string to
`submit-security-report`. This fixed capability validates against protected
native scope before writing `/tmp/gh-aw/agent/security-findings.json`; the
advisory scope/template is not tool-server authority. Shared tools accept data,
not shell commands or PR code. Do not write reports through filesystem tools,
PowerShell, shell redirects, or a validator CLI. If submission reports contract
errors, correct the JSON and resubmit; never probe denied tool variants.

Call `noop` exactly once whether or not findings exist. Never publish or write
the fork branch. The trusted controller alone publishes the validated rendered
summary when findings exist.
