---
name: 'Intelligent Terminal Security Guide Fork'
description: 'Read-only fork PR security worker that may publish one validated guidance comment.'

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

engine: copilot
imports:
  - .github/agents/ghaw-pr-security-reviewer.agent.md

skills:
  - .github/skills/ghaw-pr-security

checkout:
  repository: ${{ github.repository }}
  ref: ${{ github.workflow_sha }}
  fetch-depth: 0
  fetch: refs/pulls/open/*

tools:
  edit: true
  bash:
    - 'git diff:*'
    - 'git grep:*'
    - 'git rev-parse:*'
    - 'git show:*'

jobs:
  prepare:
    runs-on: ubuntu-latest
    timeout-minutes: 5
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
          REPOSITORY: ${{ github.repository }}
        run: |
          set -euo pipefail
          [[ "$PR_NUMBER" =~ ^[1-9][0-9]*$ ]]
          [[ "$EXPECTED_HEAD_SHA" =~ ^[0-9a-f]{40}$ ]]
          [[ "$EXPECTED_BASE_SHA" =~ ^[0-9a-f]{40}$ ]]
          [[ "$COMPARISON_BASE_SHA" =~ ^[0-9a-f]{40}$ ]]
          [ "$HEAD_REPO" != "$REPOSITORY" ]
          current_head="$(gh api "/repos/$REPOSITORY/pulls/$PR_NUMBER" --jq .head.sha)"
          current_head_repo="$(gh api "/repos/$REPOSITORY/pulls/$PR_NUMBER" --jq .head.repo.full_name)"
          [ "$current_head" = "$EXPECTED_HEAD_SHA" ]
          [ "$current_head_repo" = "$HEAD_REPO" ]
          echo "trusted_code_revision=$GITHUB_SHA" >> "$GITHUB_OUTPUT"

  agent:
    needs: [prepare]

safe-outputs:
  add-comment:
    target: '${{ github.event.inputs.pr_number }}'
    max: 1
    hide-older-comments: true

steps:
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
      rm -f /tmp/gh-aw/security-scope.json /tmp/gh-aw/security-findings.json
      trusted_validator="$RUNNER_TEMP/security-review.mjs"
      git show "$TRUSTED_SHA:.github/skills/ghaw-pr-security/scripts/security-review.mjs" > "$trusted_validator"
      node "$trusted_validator" scope \
        --base "$EXPECTED_BASE_SHA" \
        --head "$EXPECTED_HEAD_SHA" \
        --pr "$PR_NUMBER" \
        --relation fork \
        --mode guide \
        --output /tmp/gh-aw/security-scope.json
      [ "$(node -p "JSON.parse(require('fs').readFileSync('/tmp/gh-aw/security-scope.json','utf8')).baseSha")" = "$COMPARISON_BASE_SHA" ]

post-steps:
  - name: Reject stale or malformed fork guidance
    shell: bash
    env:
      GH_TOKEN: ${{ github.token }}
      EXPECTED_HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
      PR_NUMBER: ${{ github.event.inputs.pr_number }}
      REPOSITORY: ${{ github.event.inputs.repo }}
      TRUSTED_SHA: ${{ github.workflow_sha }}
    run: |
      set -euo pipefail
      current_head="$(gh api "/repos/$REPOSITORY/pulls/$PR_NUMBER" --jq .head.sha)"
      [ "$current_head" = "$EXPECTED_HEAD_SHA" ] || {
        echo "::error::Stale fork security review rejected: expected $EXPECTED_HEAD_SHA, found $current_head."
        exit 1
      }
      [ -z "$(git status --porcelain --untracked-files=all)" ] || {
        echo "::error::Fork security guide modified the trusted checkout."
        exit 1
      }
      trusted_validator="$RUNNER_TEMP/security-review-final.mjs"
      git show "$TRUSTED_SHA:.github/skills/ghaw-pr-security/scripts/security-review.mjs" > "$trusted_validator"
      node "$trusted_validator" validate \
        --scope /tmp/gh-aw/security-scope.json \
        --report /tmp/gh-aw/security-findings.json \
        --validated /tmp/gh-aw/security-findings.validated.json \
        --summary /tmp/gh-aw/security-summary.md \
        --status /tmp/gh-aw/security-status.txt
      node "$trusted_validator" validate-output \
        --validated /tmp/gh-aw/security-findings.validated.json \
        --agent-output /tmp/gh-aw/agent_output.json
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

Stay on the trusted checkout. Inspect fork objects only with read-only Git
commands and never execute or copy fork-controlled scripts into an executable
location. Write `/tmp/gh-aw/security-findings.json` with an empty `patch`.

When findings exist, call `add-comment` exactly once with a concise card listing
stable IDs, severity/confidence, file/line, impact, proposed fix, validation,
and whether evidence is a verified trace/reproducer or hypothesis. Do not quote
untrusted content or secrets. Inspect the runtime `add-comment` schema first and
call it directly with inline arguments. When no findings exist, call `noop`
exactly once. Never write the fork branch.
