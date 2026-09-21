---
name: 'Intelligent Terminal Security Repair'
description: 'Same-repository security review worker that may publish one validated HIGH-confidence repair.'

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
  - .github/agents/ghaw-pr-security.agent.md

skills:
  - .github/skills/ghaw-pr-security

checkout:
  ref: ${{ github.event.inputs.expected_head_sha }}
  fetch-depth: 0

tools:
  bash:
    - 'git diff:*'
    - 'git grep:*'
    - 'git rev-parse:*'
    - 'git show:*'
    - 'git status:*'
    - 'cargo fmt:*'
    - 'cargo test:*'
    - 'node --test:*'

jobs:
  prepare:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    outputs:
      trusted_code_revision: ${{ steps.validate.outputs.trusted_code_revision }}
    steps:
      - name: Validate immutable dispatch
        id: validate
        shell: bash
        env:
          PR_NUMBER: ${{ github.event.inputs.pr_number }}
          EXPECTED_HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
          EXPECTED_BASE_SHA: ${{ github.event.inputs.expected_base_sha }}
          COMPARISON_BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
          HEAD_REPO: ${{ github.event.inputs.head_repo }}
          REPOSITORY: ${{ github.repository }}
          GH_TOKEN: ${{ github.token }}
        run: |
          set -euo pipefail
          [[ "$PR_NUMBER" =~ ^[1-9][0-9]*$ ]]
          [[ "$EXPECTED_HEAD_SHA" =~ ^[0-9a-f]{40}$ ]]
          [[ "$EXPECTED_BASE_SHA" =~ ^[0-9a-f]{40}$ ]]
          [[ "$COMPARISON_BASE_SHA" =~ ^[0-9a-f]{40}$ ]]
          [ "$HEAD_REPO" = "$REPOSITORY" ]
          current_head="$(gh api "/repos/$REPOSITORY/pulls/$PR_NUMBER" --jq .head.sha)"
          current_head_repo="$(gh api "/repos/$REPOSITORY/pulls/$PR_NUMBER" --jq .head.repo.full_name)"
          [ "$current_head" = "$EXPECTED_HEAD_SHA" ]
          [ "$current_head_repo" = "$HEAD_REPO" ]
          echo "trusted_code_revision=$EXPECTED_HEAD_SHA" >> "$GITHUB_OUTPUT"

  agent:
    needs: [prepare]

safe-outputs:
  github-token: ${{ secrets.GITHUB_TOKEN }}
  push-to-pull-request-branch:
    base-branch: ${{ github.event.inputs.expected_head_sha }}
    github-token-for-extra-empty-commit: "${{ '' }}"
    allowed-files:
      - 'src/**'
      - 'tools/wta/src/**'
      - 'test/**'
    protected-files: blocked
    if-no-changes: error
    fallback-as-pull-request: false

steps:
  - name: Prepare immutable repair scope
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
        --relation same-repo \
        --mode repair \
        --output /tmp/gh-aw/security-scope.json
      [ "$(node -p "JSON.parse(require('fs').readFileSync('/tmp/gh-aw/security-scope.json','utf8')).baseSha")" = "$COMPARISON_BASE_SHA" ]

post-steps:
  - name: Reject stale or malformed repair output
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
        echo "::error::Stale security repair rejected: expected $EXPECTED_HEAD_SHA, found $current_head."
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

  - name: Upload validated security repair report
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

timeout-minutes: 45
max-ai-credits: 800
max-daily-ai-credits: 5000

concurrency:
  group: 'ghaw-pr-security-repair-${{ github.event.inputs.pr_number }}'
  job-discriminator: ${{ github.run_id }}
  cancel-in-progress: true

run-name: 'Security Repair ${{ github.event.inputs.dispatch_id }}'
---

Same-repository security review and eligible repair for PR
#${{ github.event.inputs.pr_number }}.

Read `/tmp/gh-aw/security-scope.json`, then follow
`.github/skills/ghaw-pr-security/SKILL.md` in `repair` mode against comparison
base `${{ github.event.inputs.comparison_base_sha }}` and immutable head
`${{ github.event.inputs.expected_head_sha }}`.

Review every applicable changed trust boundary. Only a HIGH/high-confidence
finding with strong repository evidence, a minimal allowlisted patch, passing
applicable validation against the final patch, and independent review `PASS`
may be marked `fixed`.

For a proposed repair, invoke the registered `ghaw-pr-security-reviewer` after the
final validation. Give it the comparison base, immutable original head, exact
finding, final diff, and command/exit evidence. Do not publish if it does not
return explicit `PASS`.

Write `/tmp/gh-aw/security-findings.json` exactly as the skill specifies.
List every modified path in `patch`. For a validated non-empty patch, call
`push-to-pull-request-branch` exactly once after inspecting its runtime schema.
Use a focused commit message ending with `[security-expert]`. With no patch,
call `noop` exactly once. Never add a PR comment: gh-aw PR output cannot combine
comment and commit in this worker. Remaining HIGH findings stay blocking with a
concrete reason.

## agent: `ghaw-pr-security-reviewer`
{{#runtime-import .github/agents/ghaw-pr-security-reviewer.agent.md}}
## end agent: `ghaw-pr-security-reviewer`
