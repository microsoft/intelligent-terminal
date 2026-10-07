---
name: 'Intelligent Terminal Security Repair Analysis'
description: 'Same-repository security review worker that may produce one validated HIGH-confidence repair artifact.'

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
  env:
    # Telemetry credentials remain available to native runner/gateway steps only.
    OTEL_EXPORTER_OTLP_HEADERS: "${{ '' }}"
    GH_AW_OTLP_ENDPOINTS: "${{ '' }}"
    GH_AW_OTLP_ALL_HEADERS: "${{ '' }}"
  command: 'exec node "${RUNNER_TEMP}/gh-aw/security-review-native/security-review-driver.mjs" "${RUNNER_TEMP}/gh-aw/bin/copilot"'
  harness:
    max-retries: 0
    watchdog-timeout: 600
imports:
  - .github/agents/ghaw-pr-security.agent.md
  - shared/ghaw-pr-security-tools.md

skills:
  - .github/skills/ghaw-pr-security

checkout:
  ref: ${{ github.event.inputs.expected_head_sha }}
  fetch-depth: 0

tools:
  github: false
  bash: []
  cli-proxy: false
  edit: false

sandbox:
  mcp:
    env:
      MCP_GATEWAY_LOG_DIR: /tmp/gh-aw-security-private/mcp-gateway
      GH_AW_MCP_LOG_DIR: ${{ runner.temp }}/gh-aw/safeoutputs/private-security-logs

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
      - name: Validate immutable dispatch
        id: validate
        shell: bash
        env:
          PR_NUMBER: ${{ github.event.inputs.pr_number }}
          EXPECTED_HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
          EXPECTED_BASE_SHA: ${{ github.event.inputs.expected_base_sha }}
          COMPARISON_BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
          HEAD_REPO: ${{ github.event.inputs.head_repo }}
          TARGET_REPOSITORY: ${{ github.event.inputs.repo }}
          REPOSITORY: ${{ github.repository }}
          TRUSTED_WORKFLOW_SHA: ${{ github.workflow_sha }}
          GH_TOKEN: ${{ github.token }}
        run: |
          set -euo pipefail
          [[ "$PR_NUMBER" =~ ^[1-9][0-9]*$ ]]
          [[ "$EXPECTED_HEAD_SHA" =~ ^[0-9a-f]{40}$ ]]
          [[ "$EXPECTED_BASE_SHA" =~ ^[0-9a-f]{40}$ ]]
          [[ "$COMPARISON_BASE_SHA" =~ ^[0-9a-f]{40}$ ]]
          [ "$TRUSTED_WORKFLOW_SHA" = "$EXPECTED_BASE_SHA" ]
          [ "$TARGET_REPOSITORY" = "$REPOSITORY" ]
          [ "$HEAD_REPO" = "$REPOSITORY" ]
          current_head="$(gh api "/repos/$REPOSITORY/pulls/$PR_NUMBER" --jq .head.sha)"
          current_head_repo="$(gh api "/repos/$REPOSITORY/pulls/$PR_NUMBER" --jq .head.repo.full_name)"
          [ "$current_head" = "$EXPECTED_HEAD_SHA" ]
          [ "$current_head_repo" = "$HEAD_REPO" ]
          echo "trusted_code_revision=$EXPECTED_HEAD_SHA" >> "$GITHUB_OUTPUT"

  agent:
    needs: [prepare]
    timeout-minutes: 60

  validate_windows:
    needs: [agent]
    uses: ./.github/workflows/ghaw-pr-security-validate-windows.yml
    permissions:
      contents: read
      pull-requests: read
      actions: read
    with:
      trusted_workflow_sha: ${{ github.workflow_sha }}
      expected_base_sha: ${{ github.event.inputs.expected_base_sha }}
      expected_head_sha: ${{ github.event.inputs.expected_head_sha }}
      comparison_base_sha: ${{ github.event.inputs.comparison_base_sha }}
      pr_number: ${{ github.event.inputs.pr_number }}
      proposal_artifact: ghaw-pr-security-proposal-${{ github.event.inputs.pr_number }}-${{ github.event.inputs.expected_head_sha }}

  finalize:
    needs: [agent, validate_windows]
    runs-on: ubuntu-latest
    timeout-minutes: 10
    permissions:
      contents: read
      pull-requests: read
      actions: read
    steps:
      - name: Checkout immutable publication candidate
        uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1
        with:
          ref: ${{ github.event.inputs.expected_head_sha }}
          fetch-depth: 0
          persist-credentials: false
      - name: Download validated proposal
        uses: actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c
        with:
          name: ghaw-pr-security-proposal-${{ github.event.inputs.pr_number }}-${{ github.event.inputs.expected_head_sha }}
          path: ${{ runner.temp }}/security-proposal
      - name: Promote only the independently reviewed and Windows-tested patch
        shell: bash
        env:
          GH_TOKEN: ${{ github.token }}
          REPOSITORY: ${{ github.repository }}
          PR_NUMBER: ${{ github.event.inputs.pr_number }}
          EXPECTED_BASE_SHA: ${{ github.event.inputs.expected_base_sha }}
          EXPECTED_HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
          COMPARISON_BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
          TRUSTED_SHA: ${{ github.workflow_sha }}
          TESTS_PASSED: ${{ needs.validate_windows.outputs.tests_passed }}
          TESTED_PATCH_SHA256: ${{ needs.validate_windows.outputs.tested_patch_sha256 }}
          TESTED_HEAD_SHA: ${{ needs.validate_windows.outputs.source_head_sha }}
        run: |
          set -euo pipefail
          [ "$(gh api "/repos/$REPOSITORY/pulls/$PR_NUMBER" --jq .head.sha)" = "$EXPECTED_HEAD_SHA" ]
          validator="$RUNNER_TEMP/security-review-final.mjs"
          git show "$TRUSTED_SHA:.github/skills/ghaw-pr-security/scripts/security-review.mjs" > "$validator"
          proposal="$RUNNER_TEMP/security-proposal"
          final="$RUNNER_TEMP/security-final"
          mkdir "$final"
          node "$validator" scope --base "$EXPECTED_BASE_SHA" --head "$EXPECTED_HEAD_SHA" \
            --pr "$PR_NUMBER" --relation same-repo --mode repair --output "$final/security-scope.validated.json"
          [ "$(node -p "JSON.parse(require('fs').readFileSync('$final/security-scope.validated.json','utf8')).baseSha")" = "$COMPARISON_BASE_SHA" ]
          patch_count="$(node -p "JSON.parse(require('fs').readFileSync('$proposal/security-findings.proposed.json','utf8')).patch.length")"
          [ "$TESTED_HEAD_SHA" = "$EXPECTED_HEAD_SHA" ]
          attest_args=()
          if [ "$patch_count" -gt 0 ]; then
            [ "$TESTS_PASSED" = true ]
            [ "$(sha256sum "$proposal/security-repair.patch" | cut -d ' ' -f 1)" = "$TESTED_PATCH_SHA256" ]
            git apply --binary "$proposal/security-repair.patch"
            attest_args+=(--wta-tests-passed)
          else
            [ "$TESTS_PASSED" = false ]
            [ -z "$TESTED_PATCH_SHA256" ]
            [ ! -s "$proposal/security-repair.patch" ]
          fi
          node "$validator" validate-proposal --scope "$final/security-scope.validated.json" \
            --report "$proposal/security-findings.proposed.json" --output "$final/security-proposal.checked.json"
          node "$validator" attest --report "$final/security-proposal.checked.json" --head "$EXPECTED_HEAD_SHA" \
            --output "$final/security-findings.attested.json" "${attest_args[@]}"
          node "$validator" validate --scope "$final/security-scope.validated.json" \
            --report "$final/security-findings.attested.json" --validated "$final/security-findings.validated.json" \
            --summary "$final/security-summary.md" --status "$final/security-status.txt"
          git diff --binary HEAD > "$final/security-repair.patch"
          cmp "$proposal/security-repair.patch" "$final/security-repair.patch"
          cat "$final/security-summary.md" >> "$GITHUB_STEP_SUMMARY"
      - name: Upload final trusted security artifact
        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a
        with:
          name: ghaw-pr-security-${{ github.event.inputs.pr_number }}-${{ github.event.inputs.expected_head_sha }}
          path: |
            ${{ runner.temp }}/security-final/security-scope.validated.json
            ${{ runner.temp }}/security-final/security-findings.validated.json
            ${{ runner.temp }}/security-final/security-summary.md
            ${{ runner.temp }}/security-final/security-status.txt
            ${{ runner.temp }}/security-final/security-repair.patch
          if-no-files-found: error
          retention-days: 14

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
  - name: Install pinned CLI for the trusted security driver
    shell: bash
    env:
      GH_HOST: github.com
    run: |
      set -euo pipefail
      bash "${RUNNER_TEMP}/gh-aw/actions/install_copilot_cli.sh" 1.0.90
      binary="$(command -v copilot)"
      [ -x "$binary" ]
      mkdir -p "$RUNNER_TEMP/gh-aw/bin"
      cp "$binary" "$RUNNER_TEMP/gh-aw/bin/copilot"
      chmod 755 "$RUNNER_TEMP/gh-aw/bin/copilot"

  - name: Restore trusted runtime imports after immutable head checkout
    shell: bash
    env:
      GH_AW_AGENT_FOLDERS: ".agents .github"
      GH_AW_AGENT_FILES: "AGENTS.md"
    run: |
      set -euo pipefail
      [ -d /tmp/gh-aw/base/.github ]
      bash "${RUNNER_TEMP}/gh-aw/actions/restore_base_github_folders.sh"

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
      mkdir -p /tmp/gh-aw/agent
      rm -f /tmp/gh-aw/security-scope.json /tmp/gh-aw/agent/security-findings.json
      trusted_validator="$RUNNER_TEMP/security-review.mjs"
      git show "$TRUSTED_SHA:.github/skills/ghaw-pr-security/scripts/security-review.mjs" > "$trusted_validator"
      cp "$trusted_validator" "$RUNNER_TEMP/gh-aw/security-review-check.mjs"
      driver_dir="$RUNNER_TEMP/gh-aw/security-review-native"
      mkdir -p "$driver_dir"
      cp "$trusted_validator" "$driver_dir/security-review.mjs"
      git show "$TRUSTED_SHA:.github/skills/ghaw-pr-security/scripts/security-review-driver.mjs" > "$driver_dir/security-review-driver.mjs"
      git show "$TRUSTED_SHA:.github/skills/ghaw-pr-security/scripts/prepare-security-review-private-logs.mjs" > "$driver_dir/prepare-security-review-private-logs.mjs"
      node "$trusted_validator" scope \
        --base "$EXPECTED_BASE_SHA" \
        --head "$EXPECTED_HEAD_SHA" \
        --pr "$PR_NUMBER" \
        --relation same-repo \
        --mode repair \
        --output /tmp/gh-aw/security-scope.json
      [ "$(node -p "JSON.parse(require('fs').readFileSync('/tmp/gh-aw/security-scope.json','utf8')).baseSha")" = "$COMPARISON_BASE_SHA" ]
      cp /tmp/gh-aw/security-scope.json "$RUNNER_TEMP/gh-aw/security-report-scope.json"
      node "$trusted_validator" init-report \
        --scope /tmp/gh-aw/security-scope.json \
        --output /tmp/gh-aw/agent/security-findings.json

pre-agent-steps:
  - name: Prepare private security review log sinks before native services
    shell: bash
    run: |
      set -euo pipefail
      node "$RUNNER_TEMP/gh-aw/security-review-native/prepare-security-review-private-logs.mjs" \
        --actions-dir "$RUNNER_TEMP/gh-aw/actions" \
        --private-root /tmp/gh-aw-security-private \
        --collected-root /tmp/gh-aw \
        --safe-outputs-root "$RUNNER_TEMP/gh-aw/safeoutputs" \
        --workspace "$GITHUB_WORKSPACE"
  - name: Restore immutable skill bytes after generated skill installation
    shell: bash
    env:
      TRUSTED_SHA: ${{ github.workflow_sha }}
    run: |
      set -euo pipefail
      skill=.github/skills/ghaw-pr-security/SKILL.md
      original="$RUNNER_TEMP/gh-aw/security-skill.original.md"
      git -c core.fsmonitor=false show "$TRUSTED_SHA:$skill" > "$original"
      cp "$original" "$GITHUB_WORKSPACE/$skill"
      cmp "$original" "$GITHUB_WORKSPACE/$skill"
  - name: Enforce credential-free agent checkout
    shell: bash
    run: |
      set -euo pipefail
      bash "${RUNNER_TEMP}/gh-aw/actions/clean_git_credentials.sh"
      node "$RUNNER_TEMP/gh-aw/security-review-check.mjs" verify-credentials --workspace "$GITHUB_WORKSPACE"

post-steps:
  - name: Reject stale or malformed repair output
    shell: bash
    env:
      GH_TOKEN: ${{ github.token }}
      EXPECTED_BASE_SHA: ${{ github.event.inputs.expected_base_sha }}
      EXPECTED_HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
      COMPARISON_BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
      PR_NUMBER: ${{ github.event.inputs.pr_number }}
      REPOSITORY: ${{ github.repository }}
      TRUSTED_SHA: ${{ github.workflow_sha }}
    run: |
      set -euo pipefail
      set +x
      cleanup_fetch_credentials() {
        local status=$?
        trap - EXIT
        unset GIT_ASKPASS GH_TOKEN
        if [ -n "${askpass:-}" ] && ! rm -f "$askpass"; then
          echo "::error::Failed to remove trusted fetch credential helper." >&2
          if [ "$status" -eq 0 ]; then
            status=1
          fi
        fi
        exit "$status"
      }
      trap cleanup_fetch_credentials EXIT
      current_head="$(gh api "/repos/$REPOSITORY/pulls/$PR_NUMBER" --jq .head.sha)"
      [ "$current_head" = "$EXPECTED_HEAD_SHA" ] || {
        echo "::error::Stale security repair rejected: expected $EXPECTED_HEAD_SHA, found $current_head."
        exit 1
      }
      trusted_validator="$RUNNER_TEMP/security-review-final.mjs"
      git show "$TRUSTED_SHA:.github/skills/ghaw-pr-security/scripts/security-review.mjs" > "$trusted_validator"
      source_workspace="$GITHUB_WORKSPACE"
      trusted_workspace="$RUNNER_TEMP/security-repair-workspace"
      trusted_scope="$RUNNER_TEMP/security-scope.final.json"
      trusted_report=/tmp/gh-aw/security-findings.proposed.json
      rm -rf "$trusted_workspace"
      rm -f "$trusted_scope" "$trusted_report" \
        /tmp/gh-aw/security-findings.validated.json \
        /tmp/gh-aw/security-summary.md \
        /tmp/gh-aw/security-status.txt \
        /tmp/gh-aw/security-repair.patch
      mkdir "$trusted_workspace"
      askpass="$RUNNER_TEMP/security-git-askpass.sh"
      cat > "$askpass" <<'EOF'
      #!/bin/sh
      case "$1" in
        *Username*) printf '%s\n' x-access-token ;;
        *) printf '%s\n' "$GH_TOKEN" ;;
      esac
      EOF
      chmod 700 "$askpass"
      export GIT_ASKPASS="$askpass" GIT_TERMINAL_PROMPT=0
      export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
      git -C "$trusted_workspace" init --quiet
      git -C "$trusted_workspace" remote add origin "$GITHUB_SERVER_URL/$REPOSITORY.git"
      git -C "$trusted_workspace" fetch --quiet --no-tags origin \
        "$EXPECTED_BASE_SHA" "$EXPECTED_HEAD_SHA"
      git -C "$trusted_workspace" checkout --quiet --detach "$EXPECTED_HEAD_SHA"
      git -C "$trusted_workspace" remote remove origin
      rm -f "$askpass"
      unset GIT_ASKPASS GH_TOKEN
      pushd "$trusted_workspace"
      node "$trusted_validator" scope \
        --base "$EXPECTED_BASE_SHA" \
        --head "$EXPECTED_HEAD_SHA" \
        --pr "$PR_NUMBER" \
        --relation same-repo \
        --mode repair \
        --output "$trusted_scope"
      [ "$(node -p "JSON.parse(require('fs').readFileSync('$trusted_scope','utf8')).baseSha")" = "$COMPARISON_BASE_SHA" ]
      patch_count="$(node -p "JSON.parse(require('fs').readFileSync('/tmp/gh-aw/agent/security-findings.json','utf8')).patch.length")"
      if [ "$patch_count" -gt 0 ]; then
        node "$trusted_validator" validate-repair-scope --scope "$trusted_scope"
        node "$trusted_validator" stage-repair \
          --report /tmp/gh-aw/agent/security-findings.json \
          --source "$source_workspace" \
          --target "$trusted_workspace"
      fi
      node "$trusted_validator" validate-proposal \
        --scope "$trusted_scope" \
        --report /tmp/gh-aw/agent/security-findings.json \
        --output "$trusted_report"
      node "$trusted_validator" validate-output \
        --validated "$trusted_report" \
        --agent-output /tmp/gh-aw/agent_output.json
      git diff --binary HEAD > /tmp/gh-aw/security-repair.patch
      popd
      cp "$trusted_scope" /tmp/gh-aw/security-scope.proposed.json

  - name: Upload source-reviewed security proposal
    if: always()
    uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
    with:
      name: ghaw-pr-security-proposal-${{ github.event.inputs.pr_number }}-${{ github.event.inputs.expected_head_sha }}
      path: |
        /tmp/gh-aw/security-scope.proposed.json
        /tmp/gh-aw/security-findings.proposed.json
        /tmp/gh-aw/security-repair.patch
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
finding with strong repository evidence, a minimal patch to an existing
`tools/wta/src/**/*.rs` file may be submitted as a candidate marked `proposed`,
with `review.status: pending`. Never claim `SOURCE_PASS` or mark an
agent-authored result `fixed`. The trusted driver must obtain independent source
approval before native post-validation can accept a proposal, and the trusted
post-step alone can promote it after final-patch validation passes and the
reviewed patch digest still matches. All other repairs remain blocked with guidance.
Write proposed source changes only with `write-security-repair`; generic file
editing is disabled. Native scope/path checks reject report, workflow, Git
metadata, new-file and unrelated destinations.

Do not invoke another agent. After your invocation finishes, the trusted driver
alone launches the fixed `ghaw-pr-security-reviewer` profile for a candidate.
It supplies the immutable head, native final patch digest, comparison base,
finding hypotheses, and required validation plans from the validated candidate.
The reviewer has only read-only native inspection capabilities and must fetch
the full original diff, base/head source traces and invariants, and full final
candidate itself. Parent-copied source and patch text is not independent proof.
Only the trusted driver can record `review.status: source-pass` after checking
the reviewer's successful native reads and explicit matching `SOURCE_PASS`.
Missing or incomplete native evidence fails closed. Source approval does not
claim that later native tests already passed.

Complete the prepared `/tmp/gh-aw/agent/security-findings.json` exactly as the
skill specifies, preserve its native identity fields, and list every modified
path in `patch`. Submit complete JSON as the `report_json` string to
`submit-security-report`; only this fixed capability validates against the
protected native scope and writes the report. Do not write it through filesystem
tools, shell commands, PowerShell, or a validator CLI. The advisory scope and
prepared template are context, not tool-server authority. These shared tools
accept data only; they do not execute model commands or PR code. Never execute
PR-controlled build scripts, Cargo commands, tests, or other code in the agent
environment; only trusted isolated post-validation may run them.
Call `noop` exactly once whether or not a
validated patch exists. Never publish code or add a PR comment: the trusted
controller consumes the validated artifact and performs the mutually exclusive
fast-forward repair or guidance-comment operation. Remaining HIGH findings stay
blocking with a concrete reason.
