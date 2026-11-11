---
name: 'Intelligent Terminal Security Repair Analysis'
description: 'Successful exact-head PR CI-gated same-repository security review with independently source-reviewed fix proposals.'

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
      expected_workflow_sha:
        description: 'Controller-resolved trusted worker branch revision, independent of PR comparison base'
        required: true
        type: string
      ci_run_id:
        description: 'Verified successful exact-head PR build CI run'
        required: true
        type: string
      ci_run_attempt:
        description: 'Verified CI run attempt'
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
  copilot-requests: write

env:
  GIT_NO_REPLACE_OBJECTS: '1'
  SECURITY_CI_RUN_ID: "${{ github.event.inputs.ci_run_id }}"
  SECURITY_CI_RUN_ATTEMPT: "${{ github.event.inputs.ci_run_attempt }}"

engine:
  id: copilot
  agent: ghaw-pr-security
  version: '1.0.90'
  env:
    SECURITY_CI_REQUIRED: 'true'
    SECURITY_CI_RUN_ID: "${{ github.event.inputs.ci_run_id }}"
    SECURITY_CI_RUN_ATTEMPT: "${{ github.event.inputs.ci_run_attempt }}"
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
      actions: read
    outputs:
      trusted_code_revision: ${{ steps.validate.outputs.trusted_code_revision }}
    steps:
      - name: Checkout trusted admission code
        uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1
        with:
          ref: ${{ github.workflow_sha }}
          persist-credentials: false
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
          EXPECTED_WORKFLOW_SHA: ${{ github.event.inputs.expected_workflow_sha }}
          CI_RUN_ID: ${{ github.event.inputs.ci_run_id }}
          CI_RUN_ATTEMPT: ${{ github.event.inputs.ci_run_attempt }}
          HEAD_REF: ${{ github.event.inputs.head_ref }}
          GH_TOKEN: ${{ github.token }}
        run: |
          set -euo pipefail
          [[ "$PR_NUMBER" =~ ^[1-9][0-9]*$ ]]
          [[ "$EXPECTED_HEAD_SHA" =~ ^[0-9a-f]{40}$ ]]
          [[ "$EXPECTED_BASE_SHA" =~ ^[0-9a-f]{40}$ ]]
          [[ "$COMPARISON_BASE_SHA" =~ ^[0-9a-f]{40}$ ]]
          [ "$GITHUB_RUN_ATTEMPT" = 1 ]
          [[ "$EXPECTED_WORKFLOW_SHA" =~ ^[0-9a-f]{40}$ ]]
          [ "$TRUSTED_WORKFLOW_SHA" = "$EXPECTED_WORKFLOW_SHA" ]
          [ "$TARGET_REPOSITORY" = "$REPOSITORY" ]
          [ "$HEAD_REPO" = "$REPOSITORY" ]
          current_head="$(gh api "/repos/$REPOSITORY/pulls/$PR_NUMBER" --jq .head.sha)"
          current_head_repo="$(gh api "/repos/$REPOSITORY/pulls/$PR_NUMBER" --jq .head.repo.full_name)"
          [ "$current_head" = "$EXPECTED_HEAD_SHA" ]
          [ "$current_head_repo" = "$HEAD_REPO" ]
          node --input-type=module <<'NODE'
          import { verifyCiSecurity } from './.github/skills/ghaw-pr-security/scripts/security-review.mjs';
          verifyCiSecurity();
          NODE
          echo "trusted_code_revision=$TRUSTED_WORKFLOW_SHA" >> "$GITHUB_OUTPUT"

  agent:
    needs: [prepare]
    timeout-minutes: 60

  finalize:
    needs: [agent]
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
      - name: Download independently source-reviewed proposal
        uses: actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c
        with:
          name: ghaw-pr-security-proposal-${{ github.event.inputs.pr_number }}-${{ github.event.inputs.expected_head_sha }}
          path: ${{ runner.temp }}/security-proposal
      - name: Reconstruct and check the exact source-reviewed proposal without executing PR code
        shell: bash
        env:
          GH_TOKEN: ${{ github.token }}
          REPOSITORY: ${{ github.repository }}
          PR_NUMBER: ${{ github.event.inputs.pr_number }}
          EXPECTED_BASE_SHA: ${{ github.event.inputs.expected_base_sha }}
          EXPECTED_HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
          COMPARISON_BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
          TRUSTED_SHA: ${{ github.workflow_sha }}
          CI_RUN_ID: ${{ github.event.inputs.ci_run_id }}
          CI_RUN_ATTEMPT: ${{ github.event.inputs.ci_run_attempt }}
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
          node --input-type=module - "$validator" "$proposal/security-findings.proposed.json" "$final/security-scope.validated.json" <<'NODE'
          import { readFileSync } from 'node:fs';
          import { pathToFileURL } from 'node:url';
          const [validator, report, scope] = process.argv.slice(2);
          const { validateProposal } = await import(pathToFileURL(validator));
          validateProposal(JSON.parse(readFileSync(report, 'utf8')), JSON.parse(readFileSync(scope, 'utf8')));
          NODE
          patch_count="$(node -p "JSON.parse(require('fs').readFileSync('$proposal/security-findings.proposed.json','utf8')).patch.length")"
          if [ "$patch_count" -gt 0 ]; then
            node "$validator" validate-repair-scope --scope "$final/security-scope.validated.json"
            approved_digest="$(node -p "JSON.parse(require('fs').readFileSync('$proposal/security-findings.proposed.json','utf8')).review.patchSha256")"
            [ "$(sha256sum "$proposal/security-repair.patch" | cut -d ' ' -f 1)" = "$approved_digest" ]
            git apply --check --binary "$proposal/security-repair.patch"
            git apply --binary "$proposal/security-repair.patch"
          else
            [ ! -s "$proposal/security-repair.patch" ]
          fi
          node "$validator" validate --scope "$final/security-scope.validated.json" \
            --report "$proposal/security-findings.proposed.json" --validated "$final/security-findings.validated.json" \
            --summary "$final/security-summary.md" --status "$final/security-status.txt" \
            --ci-warnings "$proposal/security-ci-warning-dispositions.json"
          cp "$proposal/security-ci-warning-dispositions.json" "$final/security-ci-warning-dispositions.json"
          git diff --no-ext-diff --no-textconv --binary --full-index HEAD > "$final/security-repair.patch"
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
            ${{ runner.temp }}/security-final/security-ci-warning-dispositions.json
          if-no-files-found: error
          retention-days: 30

  detection:
    needs: [finalize]
    pre-steps:
      - name: Checkout trusted detector binding code
        uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1
        with:
          ref: ${{ github.workflow_sha }}
          persist-credentials: false
      - name: Download finalized detector inputs
        uses: actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c
        with:
          name: ghaw-pr-security-${{ github.event.inputs.pr_number }}-${{ github.event.inputs.expected_head_sha }}
          path: ${{ runner.temp }}/security-payload
      - name: Install trusted exact-input detector binding
        shell: bash
        env:
          TRUSTED_SHA: ${{ github.workflow_sha }}
          EXPECTED_WORKFLOW_SHA: ${{ github.event.inputs.expected_workflow_sha }}
          EXPECTED_BASE_SHA: ${{ github.event.inputs.expected_base_sha }}
          EXPECTED_HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
          COMPARISON_BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
          PR_NUMBER: ${{ github.event.inputs.pr_number }}
          SECURITY_SCOPE_FILE: security-scope.validated.json
        run: |
          set -euo pipefail
          node .github/skills/ghaw-pr-security/scripts/security-detector.mjs prepare
          for key in TRUSTED_SHA EXPECTED_WORKFLOW_SHA EXPECTED_BASE_SHA EXPECTED_HEAD_SHA COMPARISON_BASE_SHA PR_NUMBER SECURITY_SCOPE_FILE; do
            printf '%s=%s\n' "$key" "${!key}" >> "$GITHUB_ENV"
          done

  publication_gate:
    needs: [agent, detection, finalize]
    runs-on: ubuntu-latest
    timeout-minutes: 5
    permissions:
      contents: read
      actions: read
    steps:
      - name: Checkout trusted detector attestation code
        uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1
        with:
          ref: ${{ github.workflow_sha }}
          persist-credentials: false
      - name: Download final security payload
        uses: actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c
        with:
          name: ghaw-pr-security-${{ github.event.inputs.pr_number }}-${{ github.event.inputs.expected_head_sha }}
          path: ${{ runner.temp }}/security-payload
      - name: Download trusted host detector completion
        uses: actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c
        with:
          name: ghaw-pr-security-detector-host-${{ github.run_id }}-${{ github.run_attempt }}-${{ github.event.inputs.pr_number }}
          path: ${{ runner.temp }}/security-detection
      - name: Attest successful generated detector outcome
        shell: bash
        env:
          DETECTION_SUCCESS: ${{ needs.detection.outputs.detection_success }}
          DETECTION_CONCLUSION: ${{ needs.detection.outputs.detection_conclusion }}
          GH_TOKEN: ${{ github.token }}
          TRUSTED_SHA: ${{ github.workflow_sha }}
          EXPECTED_WORKFLOW_SHA: ${{ github.event.inputs.expected_workflow_sha }}
          EXPECTED_BASE_SHA: ${{ github.event.inputs.expected_base_sha }}
          EXPECTED_HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
          COMPARISON_BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
          PR_NUMBER: ${{ github.event.inputs.pr_number }}
        run: |
          set -euo pipefail
          node .github/skills/ghaw-pr-security/scripts/security-review.mjs attest-detector \
            --scope "$RUNNER_TEMP/security-payload/security-scope.validated.json" \
            --report "$RUNNER_TEMP/security-payload/security-findings.validated.json" \
            --patch "$RUNNER_TEMP/security-payload/security-repair.patch" \
            --host-completion "$RUNNER_TEMP/security-detection/security-detector-host.json" \
            --output "$RUNNER_TEMP/security-detector-proof.json"
      - name: Upload trusted detector publication proof
        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a
        with:
          name: ghaw-pr-security-detector-proof-${{ github.run_id }}-${{ github.run_attempt }}-${{ github.event.inputs.pr_number }}
          path: ${{ runner.temp }}/security-detector-proof.json
          if-no-files-found: error
          retention-days: 14

safe-outputs:
  staged: true
  threat-detection:
    post-steps:
      - name: Conclude detector for publication
        id: security_detector_conclusion
        shell: bash
        env:
          DETECTION_EXECUTION_OUTCOME: ${{ steps.detection_agentic_execution.outcome }}
        run: |
          set -euo pipefail
          node "$RUNNER_TEMP/gh-aw/security-detector-native/security-detector.mjs" host-conclude
      - name: Attest original detector outcomes on host
        id: security_detector_host_completion
        shell: bash
        env:
          DETECTION_EXECUTION_OUTCOME: ${{ steps.detection_agentic_execution.outcome }}
          DETECTION_CONCLUSION_OUTCOME: ${{ steps.security_detector_conclusion.outcome }}
        run: |
          set -euo pipefail
          node "$RUNNER_TEMP/gh-aw/security-detector-native/security-detector.mjs" host-complete
      - name: Upload trusted host detector completion
        if: steps.security_detector_host_completion.outcome == 'success'
        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a
        with:
          name: ghaw-pr-security-detector-host-${{ github.run_id }}-${{ github.run_attempt }}-${{ github.event.inputs.pr_number }}
          path: ${{ runner.temp }}/security-detector-host/security-detector-host.json
          if-no-files-found: error
          retention-days: 14
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

  - name: Install symlink-safe trusted guidance restoration
    shell: bash
    env:
      TRUSTED_SHA: ${{ github.workflow_sha }}
    run: |
      set -euo pipefail
      restore="$RUNNER_TEMP/gh-aw/restore-security-review-trusted-inputs.mjs"
      git -c core.fsmonitor=false show "$TRUSTED_SHA:.github/skills/ghaw-pr-security/scripts/restore-security-review-trusted-inputs.mjs" > "$restore"
      node "$restore" install --workspace "$GITHUB_WORKSPACE" --trusted-sha "$TRUSTED_SHA" --actions-dir "$RUNNER_TEMP/gh-aw/actions"
      bash "${RUNNER_TEMP}/gh-aw/actions/restore_base_github_folders.sh"

  - name: Prepare immutable repair scope
    shell: bash
    env:
      EXPECTED_BASE_SHA: ${{ github.event.inputs.expected_base_sha }}
      EXPECTED_HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
      COMPARISON_BASE_SHA: ${{ github.event.inputs.comparison_base_sha }}
      PR_NUMBER: ${{ github.event.inputs.pr_number }}
      TRUSTED_SHA: ${{ github.workflow_sha }}
      EXPECTED_WORKFLOW_SHA: ${{ github.event.inputs.expected_workflow_sha }}
      CI_RUN_ID: ${{ github.event.inputs.ci_run_id }}
      CI_RUN_ATTEMPT: ${{ github.event.inputs.ci_run_attempt }}
      HEAD_REF: ${{ github.event.inputs.head_ref }}
      GH_TOKEN: ${{ github.token }}
    run: |
      set -euo pipefail
      mkdir -p /tmp/gh-aw/agent
      rm -f /tmp/gh-aw/security-scope.json /tmp/gh-aw/agent/security-findings.json
      rm -f /tmp/gh-aw/agent/security-driver-completion.json
      trusted_validator="$RUNNER_TEMP/security-review.mjs"
      git show "$TRUSTED_SHA:.github/skills/ghaw-pr-security/scripts/security-review.mjs" > "$trusted_validator"
      cp "$trusted_validator" "$RUNNER_TEMP/gh-aw/security-review-check.mjs"
      cp "$trusted_validator" "$RUNNER_TEMP/gh-aw/security-review.mjs"
      [ "$TRUSTED_SHA" = "$EXPECTED_WORKFLOW_SHA" ]
      [ "$GITHUB_RUN_ATTEMPT" = 1 ]
      git show "$TRUSTED_SHA:.github/skills/ghaw-pr-security/scripts/ci-security.mjs" > "$RUNNER_TEMP/gh-aw/ci-security.mjs"
      git show "$TRUSTED_SHA:build/scripts/ci-security-analysis.mjs" > "$RUNNER_TEMP/gh-aw/ci-analysis-contract.mjs"
      node --input-type=module - "$trusted_validator" <<'NODE'
      import { execFileSync } from 'node:child_process';
      import { mkdirSync, writeFileSync, readdirSync, lstatSync } from 'node:fs';
      import { join } from 'node:path';
      import { pathToFileURL } from 'node:url';
      const { verifyCiSecurity } = await import(pathToFileURL(process.argv[2]));
      const verified = verifyCiSecurity();
      const root = join(process.env.RUNNER_TEMP, 'gh-aw', 'ci-security');
      mkdirSync(root, { mode: 0o700 });
      for (const artifact of verified.artifacts) {
        const directory = join(root, artifact.language);
        mkdirSync(directory, { mode: 0o700 });
        execFileSync('gh', ['run', 'download', verified.identity.runId, '--repo',
          process.env.GITHUB_REPOSITORY, '--name', artifact.name, '--dir', directory], { stdio: 'pipe' });
        const names = readdirSync(directory);
        const stat = lstatSync(join(directory, 'findings.json'));
        if (names.length !== 1 || names[0] !== 'findings.json' || !stat.isFile() ||
            stat.isSymbolicLink() || stat.size > 2 * 1024 * 1024) throw Error('Invalid CI findings artifact layout.');
      }
      const confirmed = verifyCiSecurity();
      if (JSON.stringify(confirmed.identity) !== JSON.stringify(verified.identity) ||
          JSON.stringify(confirmed.artifacts) !== JSON.stringify(verified.artifacts)) {
        throw Error('CI identity changed during artifact download.');
      }
      writeFileSync(join(root, 'identity.json'), JSON.stringify(verified.identity), { flag: 'wx', mode: 0o600 });
      NODE
      driver_dir="$RUNNER_TEMP/gh-aw/security-review-native"
      mkdir -p "$driver_dir"
      cp "$trusted_validator" "$driver_dir/security-review.mjs"
      cp "$RUNNER_TEMP/gh-aw/ci-security.mjs" "$driver_dir/ci-security.mjs"
      cp "$RUNNER_TEMP/gh-aw/ci-analysis-contract.mjs" "$driver_dir/ci-analysis-contract.mjs"
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
      node "$RUNNER_TEMP/gh-aw/restore-security-review-trusted-inputs.mjs" restore \
        --workspace "$GITHUB_WORKSPACE" --trusted-sha "$TRUSTED_SHA" --only skill
  - name: Enforce credential-free agent checkout
    shell: bash
    run: |
      set -euo pipefail
      bash "${RUNNER_TEMP}/gh-aw/actions/clean_git_credentials.sh"
      node "$RUNNER_TEMP/gh-aw/security-review-check.mjs" verify-credentials --workspace "$GITHUB_WORKSPACE"
  - name: Revalidate successful exact-head CI immediately before inference
    shell: bash
    env:
      GH_TOKEN: ${{ github.token }}
      CI_RUN_ID: ${{ github.event.inputs.ci_run_id }}
      CI_RUN_ATTEMPT: ${{ github.event.inputs.ci_run_attempt }}
      EXPECTED_HEAD_SHA: ${{ github.event.inputs.expected_head_sha }}
      EXPECTED_BASE_SHA: ${{ github.event.inputs.expected_base_sha }}
      EXPECTED_WORKFLOW_SHA: ${{ github.event.inputs.expected_workflow_sha }}
      HEAD_REF: ${{ github.event.inputs.head_ref }}
      PR_NUMBER: ${{ github.event.inputs.pr_number }}
    run: |
      set -euo pipefail
      [ "$GITHUB_RUN_ATTEMPT" = 1 ]
      node --input-type=module - "$RUNNER_TEMP/gh-aw/security-review-check.mjs" <<'NODE'
      import { readFileSync } from 'node:fs';
      import { pathToFileURL } from 'node:url';
      const { verifyCiSecurity } = await import(pathToFileURL(process.argv[2]));
      const actual = verifyCiSecurity().identity;
      const protectedIdentity = JSON.parse(readFileSync(`${process.env.RUNNER_TEMP}/gh-aw/ci-security/identity.json`, 'utf8'));
      if (JSON.stringify(actual) !== JSON.stringify(protectedIdentity)) throw Error('CI identity changed before inference.');
      NODE

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
      CI_RUN_ID: ${{ github.event.inputs.ci_run_id }}
      CI_RUN_ATTEMPT: ${{ github.event.inputs.ci_run_attempt }}
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
      node "$trusted_validator" check-driver-completion \
        --scope "$RUNNER_TEMP/gh-aw/security-report-scope.json" \
        --report /tmp/gh-aw/agent/security-findings.json \
        --completion /tmp/gh-aw/agent/security-driver-completion.json
      node "$trusted_validator" check-ci-warning-proposals \
        --scope "$RUNNER_TEMP/gh-aw/security-report-scope.json" \
        --report /tmp/gh-aw/agent/security-findings.json \
        --warnings /tmp/gh-aw/agent/security-ci-warning-dispositions.json
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
        parity="$RUNNER_TEMP/security-repair-parity"
        mkdir "$parity"
        cp /tmp/gh-aw/agent/security-repair.source-pass.patch "$parity/source-pass.patch"
        node --input-type=module - "$source_workspace" "$trusted_workspace" "$parity" <<'NODE'
      import { readFileSync, writeFileSync } from 'node:fs';
      import { createHash } from 'node:crypto';
      import path from 'node:path';
      const [source, target, output] = process.argv.slice(2);
      const report = JSON.parse(readFileSync('/tmp/gh-aw/agent/security-findings.json', 'utf8'));
      const hash = bytes => createHash('sha256').update(bytes).digest('hex');
      const approved = readFileSync(path.join(output, 'source-pass.patch'));
      if (hash(approved) !== report.review.patchSha256) throw Error('Retained source-pass patch differs from approved digest');
      writeFileSync(path.join(output, 'before-staging.json'), JSON.stringify({
        headSha: report.headSha, approvedPatchSha256: hash(approved),
        files: report.patch.map(item => ({ path: item.path,
          sourceSha256: hash(readFileSync(path.join(source, item.path))),
          originalTargetSha256: hash(readFileSync(path.join(target, item.path))) })),
      }), { flag: 'wx' });
      NODE
        node "$trusted_validator" validate-repair-scope --scope "$trusted_scope"
        node "$trusted_validator" stage-repair \
          --report /tmp/gh-aw/agent/security-findings.json \
          --source "$source_workspace" \
          --target "$trusted_workspace"
        git diff --no-ext-diff --no-textconv --binary --full-index HEAD > "$parity/reconstructed.patch"
        sha256sum "$parity/source-pass.patch" "$parity/reconstructed.patch" > "$parity/patch-digests.txt"
      fi
      node "$trusted_validator" validate-proposal \
        --scope "$trusted_scope" \
        --report /tmp/gh-aw/agent/security-findings.json \
        --output "$trusted_report"
      node "$trusted_validator" validate-output \
        --validated "$trusted_report" \
        --agent-output /tmp/gh-aw/agent_output.json
      git diff --no-ext-diff --no-textconv --binary --full-index HEAD > /tmp/gh-aw/security-repair.patch
      popd
      cp "$trusted_scope" /tmp/gh-aw/security-scope.proposed.json
      cp /tmp/gh-aw/agent/security-ci-warning-dispositions.json /tmp/gh-aw/security-ci-warning-dispositions.json

  - name: Retain trusted repair parity evidence
    if: always()
    uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a
    with:
      name: ghaw-pr-security-parity-${{ github.run_id }}-${{ github.run_attempt }}
      path: ${{ runner.temp }}/security-repair-parity
      if-no-files-found: warn
      retention-days: 14

  - name: Upload source-reviewed security proposal
    if: always()
    uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
    with:
      name: ghaw-pr-security-proposal-${{ github.event.inputs.pr_number }}-${{ github.event.inputs.expected_head_sha }}
      path: |
        /tmp/gh-aw/security-scope.proposed.json
        /tmp/gh-aw/security-findings.proposed.json
        /tmp/gh-aw/security-repair.patch
        /tmp/gh-aw/security-ci-warning-dispositions.json
      if-no-files-found: error
      retention-days: 30

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

The skill is authoritative for review coverage, evidence, severity, repair
eligibility, independent approval, privacy, and report presentation. Inspect
every changed hunk and follow each applicable trust boundary; do not limit
findings to automatically repairable code.

Follow its CI warning proposal procedure as well: read the matching CI
warnings, record a fix-or-ignore proposal for each, and include priority,
PR relevance and an owner/coding-agent handoff in your summary. Do not apply
or suppress those proposals automatically.

Use the prepared `/tmp/gh-aw/agent/security-findings.json` identity and submit
the completed JSON plus agent-authored Markdown through
`submit-security-report`. Follow the skill's template, field limits and
commit placeholders. Correct rejected submissions through that same tool.
Emit exactly one `noop` after acceptance. Never execute PR code, delegate,
push a commit, or post a PR comment. The caller owns independent review and
publication; build/test validation remains the original PR reviewer's job.
