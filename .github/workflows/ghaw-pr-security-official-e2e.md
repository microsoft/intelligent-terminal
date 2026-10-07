---
name: 'Official security E2E acceptance'
description: 'One official, narrowly push-triggered acceptance run using the production security repair and Windows validation contracts.'

on:
  push:
    branches:
      - 'test/yeelam/ghaw-security-repair-e2e-20261007-r5'

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
  model: gpt-5.6-luna
  version: '1.0.90'
  env:
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
  ref: ${{ needs.prepare.outputs.expected_head_sha }}
  fetch-depth: 0

tools:
  github: false
  bash: []
  cli-proxy: false
  edit: false

jobs:
  prepare:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    permissions:
      contents: write
      pull-requests: read
    outputs:
      trusted_code_revision: ${{ steps.fixture.outputs.expected_head_sha }}
      expected_head_sha: ${{ steps.fixture.outputs.expected_head_sha }}
      expected_base_sha: ${{ steps.fixture.outputs.expected_base_sha }}
      comparison_base_sha: ${{ steps.fixture.outputs.expected_base_sha }}
      head_ref: ${{ steps.fixture.outputs.head_ref }}
      fixture_number: ${{ steps.fixture.outputs.fixture_number }}
    steps:
      - name: Guard the single official test push before any mutation
        shell: bash
        env:
          REPOSITORY: ${{ github.repository }}
          ACTOR: ${{ github.actor }}
          EVENT: ${{ github.event_name }}
          REF: ${{ github.ref }}
          WORKFLOW_SHA: ${{ github.workflow_sha }}
          HEAD_SHA: ${{ github.sha }}
          ATTEMPT: ${{ github.run_attempt }}
        run: |
          set -euo pipefail
          [ "$REPOSITORY" = microsoft/intelligent-terminal ]
          [ "$ACTOR" = yeelam-gordon ]
          [ "$EVENT" = push ]
          [ "$REF" = refs/heads/test/yeelam/ghaw-security-repair-e2e-20261007-r5 ]
          [ "$ATTEMPT" = 1 ]
          [[ "$WORKFLOW_SHA" =~ ^[0-9a-f]{40}$ ]]
          [ "$WORKFLOW_SHA" = "$HEAD_SHA" ]
      - name: Checkout trusted immutable authoring base
        uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1
        with:
          ref: ${{ github.workflow_sha }}
          fetch-depth: 0
          persist-credentials: false
      # No PR mutation is authorized; an existing fixture ref is never reused.
      - name: Allocate the fresh dedicated one-file branch fixture in this run
        id: fixture
        uses: actions/github-script@3a2844b7e9c422d3c10d287c895573f7108da1b3
        env:
          BASE_SHA: ${{ github.workflow_sha }}
        with:
          github-token: ${{ github.token }}
          retries: 0
          script: |
            const {execFileSync} = await import('node:child_process');
            const {mkdirSync, writeFileSync} = await import('node:fs');
            const base = process.env.BASE_SHA;
            const baseRef = 'test/yeelam/ghaw-security-repair-e2e-20261007-r5';
            const headRef = 'test/yeelam/ghaw-security-repair-fixture-20261007-r5';
            const path = 'tools/wta/src/master/session_mcp.rs';
            const fixtureNumber = Number(context.runNumber);
            if (!Number.isSafeInteger(fixtureNumber) || fixtureNumber < 1) throw new Error('Invalid fixture ordinal');
            const git = (...args) => execFileSync('git', ['--no-pager', ...args], {encoding:'utf8'});
            if (git('rev-parse', 'HEAD').trim() !== base) throw new Error('Trusted checkout mismatch');
            const repo = (await github.rest.repos.get(context.repo)).data;
            if (repo.private || repo.full_name !== 'microsoft/intelligent-terminal') throw new Error('Exact public official repository required');
            const liveBase = (await github.rest.git.getRef({...context.repo, ref:`heads/${baseRef}`})).data;
            if (liveBase.object.sha !== base) throw new Error('Base raced');
            const associated = (await github.rest.pulls.list({...context.repo,
              head:'microsoft:test/yeelam/ghaw-security-repair-fixture-20261007-r5', state:'all', per_page:100})).data;
            if (associated.length !== 0) throw new Error('Fixture branch has an associated PR');
            try {
              await github.rest.git.getRef({...context.repo, ref:`heads/${headRef}`});
              throw new Error('Fixture ref already exists; this acceptance run cannot be retried');
            } catch (error) { if (error.status !== 404) throw error; }
            const entry = git('ls-tree', base, '--', path).trim();
            if (!/^100644 blob [0-9a-f]{40}\ttools\/wta\/src\/master\/session_mcp.rs$/.test(entry)) throw new Error('Fixture is not an existing regular source blob');
            const originalBytes = execFileSync('git', ['show', `${base}:${path}`]);
            const original = originalBytes.toString('utf8');
            if (!Buffer.from(original, 'utf8').equals(originalBytes)) throw new Error('Non-UTF8 source');
            const before = '            .get(&hash_secret(secret))';
            if (original.split(before).length !== 2) throw new Error('Fixture replacement is not unique');
            const content = original.replace(before, '            .values().next()');
            if (!content.includes('            .map(|route| route.route_key.clone())')) throw new Error('Option mapping changed unexpectedly');
            const blob = (await github.rest.git.createBlob({...context.repo, content:Buffer.from(content).toString('base64'), encoding:'base64'})).data;
            const parent = (await github.rest.git.getCommit({...context.repo, commit_sha:base})).data;
            const tree = (await github.rest.git.createTree({...context.repo, base_tree:parent.tree.sha,
              tree:[{path, mode:'100644', type:'blob', sha:blob.sha}]})).data;
            const commit = (await github.rest.git.createCommit({...context.repo,
              message:'Official immutable security acceptance fixture; do not merge',
              tree:tree.sha, parents:[base]})).data;
            // Read Git Data back before creating the only reachable fixture ref.
            const saved = (await github.rest.git.getBlob({...context.repo, file_sha:blob.sha})).data;
            if (!Buffer.from(saved.content.replace(/\s/g,''), 'base64').equals(Buffer.from(content))) throw new Error('Fixture blob bytes changed');
            const savedTree = (await github.rest.git.getTree({...context.repo, tree_sha:tree.sha})).data;
            const parentTree = (await github.rest.git.getTree({...context.repo, tree_sha:parent.tree.sha})).data;
            if (savedTree.truncated || parentTree.truncated) throw new Error('Incomplete tree evidence');
            const changes = savedTree.tree.filter(item => !parentTree.tree.some(old => old.path === item.path && old.sha === item.sha && old.mode === item.mode));
            if (changes.length !== 1 || changes[0].path !== 'tools') throw new Error('Unexpected root tree changes');
            await github.rest.git.createRef({...context.repo, ref:`refs/heads/${headRef}`, sha:commit.sha});
            const liveHead = (await github.rest.git.getRef({...context.repo, ref:`heads/${headRef}`})).data;
            if (liveHead.object.sha !== commit.sha) throw new Error('Allocated branch identity mismatch');
            core.setOutput('fixture_number', String(fixtureNumber));
            core.setOutput('expected_head_sha', commit.sha);
            core.setOutput('expected_base_sha', base);
            core.setOutput('head_ref', headRef);
            mkdirSync('security-e2e-identity');
            writeFileSync('security-e2e-identity/identity.json', JSON.stringify({
              fixtureIdentityType:'branch', fixtureNumber,
              repository:repo.full_name, runId:context.runId, attempt:1,
              trustedWorkflowSha:base, baseSha:base, headSha:commit.sha,
              headRef, path, fixtureBlobSha:blob.sha, baseTreeSha:parent.tree.sha,
              headTreeSha:tree.sha, prRoutingExercised:false, commentPathExercised:false}, null, 2));
      - name: Verify complete immutable fixture diff with native Git
        shell: bash
        env:
          GH_TOKEN: ${{ github.token }}
          HEAD_SHA: ${{ steps.fixture.outputs.expected_head_sha }}
          BASE_SHA: ${{ steps.fixture.outputs.expected_base_sha }}
        run: |
          set -euo pipefail
          git -c credential.helper= -c 'credential.helper=!gh auth git-credential' fetch --quiet --no-tags origin "$HEAD_SHA"
          [ "$(git rev-parse "$HEAD_SHA^")" = "$BASE_SHA" ]
          [ "$(git diff --name-status "$BASE_SHA" "$HEAD_SHA")" = $'M\ttools/wta/src/master/session_mcp.rs' ]
          [ -z "$(git diff --summary "$BASE_SHA" "$HEAD_SHA")" ]
          node <<'NODE'
          const {execFileSync} = require('node:child_process');
          const path = 'tools/wta/src/master/session_mcp.rs';
          const get = sha => execFileSync('git', ['show', `${sha}:${path}`]);
          const bytes = get(process.env.BASE_SHA);
          const text = bytes.toString('utf8');
          const expected = Buffer.from(text.replace('            .get(&hash_secret(secret))', '            .values().next()'));
          if (!get(process.env.HEAD_SHA).equals(expected)) throw new Error('Full fixture source differs from exact one-hunk replacement');
          NODE
          git diff --no-ext-diff --binary "$BASE_SHA" "$HEAD_SHA" > security-e2e-identity/fixture.patch
      - name: Retain immutable allocation identity
        if: always()
        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a
        with:
          name: security-e2e-identity-${{ github.run_id }}-1
          path: security-e2e-identity
          if-no-files-found: error
          retention-days: 14

  agent:
    needs: [prepare]
    timeout-minutes: 30

  detection:
    timeout-minutes: 10

  validate_windows:
    needs: [prepare, agent]
    uses: ./.github/workflows/ghaw-pr-security-official-validate-branch.yml
    permissions:
      contents: read
      pull-requests: read
      actions: read
    with:
      trusted_workflow_sha: ${{ github.workflow_sha }}
      expected_base_sha: ${{ needs.prepare.outputs.expected_base_sha }}
      expected_head_sha: ${{ needs.prepare.outputs.expected_head_sha }}
      comparison_base_sha: ${{ needs.prepare.outputs.comparison_base_sha }}
      fixture_number: ${{ needs.prepare.outputs.fixture_number }}
      head_ref: ${{ needs.prepare.outputs.head_ref }}
      proposal_artifact: ghaw-pr-security-proposal-${{ needs.prepare.outputs.fixture_number }}-${{ needs.prepare.outputs.expected_head_sha }}

  finalize:
    needs: [prepare, agent, validate_windows]
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
          ref: ${{ needs.prepare.outputs.expected_head_sha }}
          fetch-depth: 0
          persist-credentials: false
      - name: Download validated proposal
        uses: actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c
        with:
          name: ghaw-pr-security-proposal-${{ needs.prepare.outputs.fixture_number }}-${{ needs.prepare.outputs.expected_head_sha }}
          path: ${{ runner.temp }}/security-proposal
      - name: Promote only the independently reviewed and Windows-tested patch
        shell: bash
        env:
          GH_TOKEN: ${{ github.token }}
          REPOSITORY: ${{ github.repository }}
          FIXTURE_NUMBER: ${{ needs.prepare.outputs.fixture_number }}
          HEAD_REF: ${{ needs.prepare.outputs.head_ref }}
          EXPECTED_BASE_SHA: ${{ needs.prepare.outputs.expected_base_sha }}
          EXPECTED_HEAD_SHA: ${{ needs.prepare.outputs.expected_head_sha }}
          COMPARISON_BASE_SHA: ${{ needs.prepare.outputs.comparison_base_sha }}
          TRUSTED_SHA: ${{ github.workflow_sha }}
          TESTS_PASSED: ${{ needs.validate_windows.outputs.tests_passed }}
          TESTED_PATCH_SHA256: ${{ needs.validate_windows.outputs.tested_patch_sha256 }}
          TESTED_HEAD_SHA: ${{ needs.validate_windows.outputs.source_head_sha }}
        run: |
          set -euo pipefail
          [ "$HEAD_REF" = test/yeelam/ghaw-security-repair-fixture-20261007-r5 ]
          [ "$TRUSTED_SHA" = "$EXPECTED_BASE_SHA" ]
          [ "$(gh api "/repos/$REPOSITORY/git/ref/heads/$HEAD_REF" --jq .object.sha)" = "$EXPECTED_HEAD_SHA" ]
          [ "$(gh api "/repos/$REPOSITORY/git/ref/heads/test/yeelam/ghaw-security-repair-e2e-20261007-r5" --jq .object.sha)" = "$EXPECTED_BASE_SHA" ]
          validator="$RUNNER_TEMP/security-review-final.mjs"
          git show "$TRUSTED_SHA:.github/skills/ghaw-pr-security/scripts/security-review.mjs" > "$validator"
          proposal="$RUNNER_TEMP/security-proposal"
          final="$RUNNER_TEMP/security-final"
          mkdir "$final"
          node "$validator" scope --base "$EXPECTED_BASE_SHA" --head "$EXPECTED_HEAD_SHA" \
            --pr "$FIXTURE_NUMBER" --relation same-repo --mode repair --output "$final/security-scope.validated.json"
          [ "$(node -p "JSON.parse(require('fs').readFileSync('$final/security-scope.validated.json','utf8')).baseSha")" = "$COMPARISON_BASE_SHA" ]
          patch_count="$(node -p "JSON.parse(require('fs').readFileSync('$proposal/security-findings.proposed.json','utf8')).patch.length")"
          [ "$patch_count" -gt 0 ]
          [ "$TESTED_HEAD_SHA" = "$EXPECTED_HEAD_SHA" ]
          [ "$TESTS_PASSED" = true ]
          [ "$(sha256sum "$proposal/security-repair.patch" | cut -d ' ' -f 1)" = "$TESTED_PATCH_SHA256" ]
          git apply --binary "$proposal/security-repair.patch"
          node "$validator" validate-proposal --scope "$final/security-scope.validated.json" \
            --report "$proposal/security-findings.proposed.json" --output "$final/security-proposal.checked.json"
          node "$validator" attest --report "$final/security-proposal.checked.json" --head "$EXPECTED_HEAD_SHA" \
            --output "$final/security-findings.attested.json" --wta-tests-passed
          node "$validator" validate --scope "$final/security-scope.validated.json" \
            --report "$final/security-findings.attested.json" --validated "$final/security-findings.validated.json" \
            --summary "$final/security-summary.md" --status "$final/security-status.txt"
          git diff --binary HEAD > "$final/security-repair.patch"
          cmp "$proposal/security-repair.patch" "$final/security-repair.patch"
          cat "$final/security-summary.md" >> "$GITHUB_STEP_SUMMARY"
      - name: Upload final trusted security artifact
        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a
        with:
          name: ghaw-pr-security-${{ needs.prepare.outputs.fixture_number }}-${{ needs.prepare.outputs.expected_head_sha }}
          path: |
            ${{ runner.temp }}/security-final/security-scope.validated.json
            ${{ runner.temp }}/security-final/security-findings.validated.json
            ${{ runner.temp }}/security-final/security-summary.md
            ${{ runner.temp }}/security-final/security-status.txt
            ${{ runner.temp }}/security-final/security-repair.patch
          if-no-files-found: error
          retention-days: 14

  publish:
    needs: [prepare, agent, detection, safe_outputs, validate_windows, finalize]
    runs-on: ubuntu-latest
    timeout-minutes: 10
    permissions:
      contents: write
      pull-requests: read
      actions: read
    steps:
      - name: Checkout immutable trusted base
        uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1
        with:
          ref: ${{ needs.prepare.outputs.expected_base_sha }}
          fetch-depth: 0
          persist-credentials: false
      - name: Download this run's final trusted artifact
        uses: actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c
        with:
          name: ghaw-pr-security-${{ needs.prepare.outputs.fixture_number }}-${{ needs.prepare.outputs.expected_head_sha }}
          path: ${{ runner.temp }}/security-publication
      - name: Download this run's native Windows proof
        uses: actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c
        with:
          name: ghaw-pr-security-windows-proof-${{ github.run_id }}-${{ github.run_attempt }}-${{ needs.prepare.outputs.fixture_number }}
          path: ${{ runner.temp }}/security-windows-proof
      - name: Verify, publish with the expected-head lease, and read back
        shell: bash
        env:
          GH_TOKEN: ${{ github.token }}
          FIXTURE_NUMBER: ${{ needs.prepare.outputs.fixture_number }}
          TRUSTED_SHA: ${{ github.workflow_sha }}
          EXPECTED_HEAD_SHA: ${{ needs.prepare.outputs.expected_head_sha }}
          EXPECTED_BASE_SHA: ${{ needs.prepare.outputs.expected_base_sha }}
          HEAD_REF: ${{ needs.prepare.outputs.head_ref }}
          TESTS_PASSED: ${{ needs.validate_windows.outputs.tests_passed }}
          TESTED_HEAD_SHA: ${{ needs.validate_windows.outputs.source_head_sha }}
          TESTED_PATCH_SHA256: ${{ needs.validate_windows.outputs.tested_patch_sha256 }}
        run: |
          set -euo pipefail
          [ "$GITHUB_REPOSITORY" = microsoft/intelligent-terminal ]
          [ "$HEAD_REF" = test/yeelam/ghaw-security-repair-fixture-20261007-r5 ]
          [ "$TESTS_PASSED" = true ]
          [ "$TESTED_HEAD_SHA" = "$EXPECTED_HEAD_SHA" ]
          [ "$TRUSTED_SHA" = "$EXPECTED_BASE_SHA" ]
          [ "$(gh api "/repos/$GITHUB_REPOSITORY/git/ref/heads/$HEAD_REF" --jq .object.sha)" = "$EXPECTED_HEAD_SHA" ]
          [ "$(gh api "/repos/$GITHUB_REPOSITORY/git/ref/heads/test/yeelam/ghaw-security-repair-e2e-20261007-r5" --jq .object.sha)" = "$EXPECTED_BASE_SHA" ]
          git -c credential.helper= -c 'credential.helper=!gh auth git-credential' fetch --quiet --no-tags origin "$EXPECTED_HEAD_SHA"
          final="$RUNNER_TEMP/security-publication"
          validator="$GITHUB_WORKSPACE/.github/skills/ghaw-pr-security/scripts/security-review.mjs"
          [ "$(tr -d '\r\n' < "$final/security-status.txt")" = pass ]
          [ "$(sha256sum "$final/security-repair.patch" | cut -d ' ' -f 1)" = "$TESTED_PATCH_SHA256" ]
          node <<'NODE'
          const fs = require('node:fs');
          const dir = `${process.env.RUNNER_TEMP}/security-windows-proof`;
          const identity = JSON.parse(fs.readFileSync(`${dir}/identity.json`, 'utf8'));
          const result = JSON.parse(fs.readFileSync(`${dir}/result.json`, 'utf8'));
          const log = fs.readFileSync(`${dir}/tests.log`, 'utf8');
          if (identity.repository !== process.env.GITHUB_REPOSITORY ||
              identity.trustedWorkflowSha !== process.env.TRUSTED_SHA ||
              identity.baseSha !== process.env.EXPECTED_BASE_SHA || identity.headSha !== process.env.EXPECTED_HEAD_SHA ||
              identity.comparisonBaseSha !== process.env.EXPECTED_BASE_SHA ||
              String(identity.runId) !== process.env.GITHUB_RUN_ID || String(identity.attempt) !== process.env.GITHUB_RUN_ATTEMPT ||
              Number(identity.fixtureNumber) !== Number(process.env.FIXTURE_NUMBER) ||
              identity.proposalArtifact !== `ghaw-pr-security-proposal-${process.env.FIXTURE_NUMBER}-${process.env.EXPECTED_HEAD_SHA}` ||
              result.repository !== process.env.GITHUB_REPOSITORY || result.trustedWorkflowSha !== process.env.TRUSTED_SHA ||
              result.comparisonBaseSha !== process.env.EXPECTED_BASE_SHA ||
              result.headSha !== process.env.EXPECTED_HEAD_SHA || result.baseSha !== process.env.EXPECTED_BASE_SHA ||
              result.patchSha256 !== process.env.TESTED_PATCH_SHA256 || result.testsPassed !== true ||
              result.exitCode !== 0 || !/^test result: ok\. 2444 passed; 0 failed;/m.test(log)) {
            throw new Error('Missing digest-bound native Windows proof of 2444 passing tests');
          }
          NODE
          git checkout --quiet --detach "$EXPECTED_HEAD_SHA"
          node "$validator" scope --base "$EXPECTED_BASE_SHA" --head "$EXPECTED_HEAD_SHA" \
            --pr "$FIXTURE_NUMBER" --relation same-repo --mode repair --output "$final/scope.recomputed.json"
          cmp "$final/security-scope.validated.json" "$final/scope.recomputed.json"
          git apply --check --binary "$final/security-repair.patch"
          git apply --binary "$final/security-repair.patch"
          node "$validator" validate --scope "$final/scope.recomputed.json" \
            --report "$final/security-findings.validated.json" --validated "$final/report.rechecked.json" \
            --summary "$final/summary.rechecked.md" --status "$final/status.rechecked.txt"
          [ "$(tr -d '\r\n' < "$final/status.rechecked.txt")" = pass ]
          REPORT="$final/report.rechecked.json" node <<'NODE'
          const fs = require('node:fs');
          const report = JSON.parse(fs.readFileSync(process.env.REPORT, 'utf8'));
          if (report.patch.length !== 1 || report.patch[0].path !== 'tools/wta/src/master/session_mcp.rs' ||
              !report.findings.some(f => f.severity === 'high' && f.confidence === 'high' && f.fixDisposition.state === 'fixed') ||
              report.findings.some(f => f.severity === 'high' && f.fixDisposition.state !== 'fixed') ||
              report.checks.some(c => ['fail','blocked'].includes(c.status)) || report.review.status !== 'source-pass') {
            throw new Error('Acceptance requires a real HIGH/high-confidence fixed finding with source approval and native tests');
          }
          NODE
          [ "$(git diff --name-only HEAD)" = tools/wta/src/master/session_mcp.rs ]
          [ -z "$(git diff --summary HEAD)" ]
          export GIT_INDEX_FILE="$RUNNER_TEMP/security-repair.index"
          rm -f "$GIT_INDEX_FILE"
          git read-tree "$EXPECTED_HEAD_SHA"
          git apply --cached --binary "$final/security-repair.patch"
          tree="$(git write-tree)"
          [ "$(git rev-parse "$tree:tools/wta/src/master/session_mcp.rs")" = "$(git rev-parse "$EXPECTED_BASE_SHA:tools/wta/src/master/session_mcp.rs")" ]
          commit="$(
            printf '%s\n\n%s\n' 'Apply validated official test-only security E2E repair; do not merge' \
              'Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>' |
              git -c user.name='github-actions[bot]' \
                  -c user.email='41898282+github-actions[bot]@users.noreply.github.com' \
                  commit-tree "$tree" -p "$EXPECTED_HEAD_SHA"
          )"
          git merge-base --is-ancestor "$EXPECTED_HEAD_SHA" "$commit"
          [ "$GITHUB_REPOSITORY" = microsoft/intelligent-terminal ]
          [ "$HEAD_REF" = test/yeelam/ghaw-security-repair-fixture-20261007-r5 ]
          [ "$(gh api "/repos/$GITHUB_REPOSITORY/git/ref/heads/$HEAD_REF" --jq .object.sha)" = "$EXPECTED_HEAD_SHA" ]
          [ "$(gh api "/repos/$GITHUB_REPOSITORY/git/ref/heads/test/yeelam/ghaw-security-repair-e2e-20261007-r5" --jq .object.sha)" = "$EXPECTED_BASE_SHA" ]
          associated_prs="$(gh api --method GET "/repos/$GITHUB_REPOSITORY/pulls" \
            -f state=all -f "head=microsoft:$HEAD_REF" -f per_page=1)"
          printf '%s' "$associated_prs" | node -e \
            'let text=""; process.stdin.setEncoding("utf8"); process.stdin.on("data", chunk => text += chunk); process.stdin.on("end", () => { const prs = JSON.parse(text); if (!Array.isArray(prs) || prs.length !== 0) throw new Error("Fixture branch must remain PR-free before publication"); });'
          git -c credential.helper= -c 'credential.helper=!gh auth git-credential' \
            push --force-with-lease="refs/heads/$HEAD_REF:$EXPECTED_HEAD_SHA" \
              origin "$commit:refs/heads/$HEAD_REF"
          [ "$(gh api "/repos/$GITHUB_REPOSITORY/git/ref/heads/$HEAD_REF" --jq .object.sha)" = "$commit" ]
          [ "$(gh api "/repos/$GITHUB_REPOSITORY/git/commits/$commit" --jq .tree.sha)" = "$tree" ]
          git -c credential.helper= -c 'credential.helper=!gh auth git-credential' fetch --quiet --no-tags origin "$commit"
          published_blob="$(git rev-parse "$commit:tools/wta/src/master/session_mcp.rs")"
          [ "$published_blob" = "$(git rev-parse "$EXPECTED_BASE_SHA:tools/wta/src/master/session_mcp.rs")" ]
          [ "$published_blob" = "$(git hash-object tools/wta/src/master/session_mcp.rs)" ]
          [ "$(git diff --name-only "$EXPECTED_HEAD_SHA" "$commit")" = tools/wta/src/master/session_mcp.rs ]
          gh api "/repos/$GITHUB_REPOSITORY/git/blobs/$published_blob" --jq .content |
            base64 --decode | cmp - tools/wta/src/master/session_mcp.rs
          mkdir security-e2e-published
          jq -n --arg repository "$GITHUB_REPOSITORY" --arg runId "$GITHUB_RUN_ID" \
            --argjson fixture "$FIXTURE_NUMBER" --arg ref "refs/heads/$HEAD_REF" \
            --arg trusted "$TRUSTED_SHA" --arg base "$EXPECTED_BASE_SHA" --arg head "$EXPECTED_HEAD_SHA" \
            --arg commit "$commit" --arg tree "$tree" --arg digest "$TESTED_PATCH_SHA256" \
            '{fixtureIdentityType:"branch",fixtureNumber:$fixture,ref:$ref,
              repository:$repository,runId:$runId,trustedWorkflowSha:$trusted,baseSha:$base,sourceHeadSha:$head,testedHeadSha:$head,
              publishedCommit:$commit,publishedTree:$tree,testedPatchSha256:$digest,testsPassed:true,
              publicationFinished:true,prRoutingExercised:false,commentPathExercised:false}' \
            > security-e2e-published/receipt.json
          echo "Published digest-bound repair commit \`$commit\` to \`refs/heads/$HEAD_REF\` (branch fixture ordinal $FIXTURE_NUMBER). PR routing and comment paths were not exercised." >> "$GITHUB_STEP_SUMMARY"
      - name: Retain verified publication receipt
        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a
        with:
          name: security-e2e-published-${{ github.run_id }}-1
          path: security-e2e-published
          if-no-files-found: error
          retention-days: 14

  cleanup:
    if: always() && needs.prepare.outputs.fixture_number != ''
    needs: [prepare, agent, publish, validate_windows]
    runs-on: ubuntu-latest
    timeout-minutes: 5
    permissions:
      contents: read
      pull-requests: read
    steps:
      - name: Download successful same-run publication receipt
        if: needs.publish.result == 'success'
        uses: actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c
        with:
          name: security-e2e-published-${{ github.run_id }}-1
          path: security-e2e-published
      - name: Assert PR-free audit identity without changing retained refs
        uses: actions/github-script@3a2844b7e9c422d3c10d287c895573f7108da1b3
        env:
          FIXTURE_NUMBER: ${{ needs.prepare.outputs.fixture_number }}
          BASE_SHA: ${{ needs.prepare.outputs.expected_base_sha }}
          HEAD_SHA: ${{ needs.prepare.outputs.expected_head_sha }}
          PUBLISH_RESULT: ${{ needs.publish.result }}
        with:
          github-token: ${{ github.token }}
          retries: 0
          script: |
            if (context.repo.owner !== 'microsoft' || context.repo.repo !== 'intelligent-terminal') throw new Error('Wrong cleanup repository');
            const associated = (await github.rest.pulls.list({...context.repo,
              head:'microsoft:test/yeelam/ghaw-security-repair-fixture-20261007-r5', state:'all', per_page:100})).data;
            if (associated.length !== 0) throw new Error('Fixture branch must remain PR-free');
            if (process.env.PUBLISH_RESULT !== 'success') throw new Error('No completed native publication; retained refs are failure evidence only');
            const {readFileSync} = await import('node:fs');
            const receipt = JSON.parse(readFileSync('security-e2e-published/receipt.json', 'utf8'));
            if (receipt.fixtureIdentityType !== 'branch' || receipt.fixtureNumber !== Number(process.env.FIXTURE_NUMBER) ||
                receipt.runId !== String(context.runId) || receipt.baseSha !== process.env.BASE_SHA ||
                receipt.sourceHeadSha !== process.env.HEAD_SHA || receipt.testedHeadSha !== process.env.HEAD_SHA ||
                receipt.ref !== 'refs/heads/test/yeelam/ghaw-security-repair-fixture-20261007-r5' ||
                receipt.publicationFinished !== true || receipt.prRoutingExercised !== false ||
                receipt.commentPathExercised !== false) throw new Error('Invalid native publication receipt');
            core.summary.addRaw('Audit branch retained. PR routing and comment paths were not exercised; Windows adapter owns local container cleanup.');
            await core.summary.write();

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
  - name: Prepare immutable repair scope and reuse the production procedure
    shell: bash
    env:
      EXPECTED_BASE_SHA: ${{ needs.prepare.outputs.expected_base_sha }}
      EXPECTED_HEAD_SHA: ${{ needs.prepare.outputs.expected_head_sha }}
      COMPARISON_BASE_SHA: ${{ needs.prepare.outputs.comparison_base_sha }}
      FIXTURE_NUMBER: ${{ needs.prepare.outputs.fixture_number }}
      HEAD_REF: ${{ needs.prepare.outputs.head_ref }}
      TRUSTED_SHA: ${{ github.workflow_sha }}
    run: |
      set -euo pipefail
      [ "$HEAD_REF" = test/yeelam/ghaw-security-repair-fixture-20261007-r5 ]
      [ "$TRUSTED_SHA" = "$EXPECTED_BASE_SHA" ]
      mkdir -p /tmp/gh-aw/agent
      rm -f /tmp/gh-aw/security-scope.json /tmp/gh-aw/agent/security-findings.json
      trusted_validator="$RUNNER_TEMP/security-review.mjs"
      git show "$TRUSTED_SHA:.github/skills/ghaw-pr-security/scripts/security-review.mjs" > "$trusted_validator"
      cp "$trusted_validator" "$RUNNER_TEMP/gh-aw/security-review-check.mjs"
      driver_dir="$RUNNER_TEMP/gh-aw/security-review-native"
      mkdir -p "$driver_dir"
      cp "$trusted_validator" "$driver_dir/security-review.mjs"
      git show "$TRUSTED_SHA:.github/skills/ghaw-pr-security/scripts/security-review-driver.mjs" > "$driver_dir/security-review-driver.mjs"
      chmod 444 "$trusted_validator" "$RUNNER_TEMP/gh-aw/security-review-check.mjs" \
        "$driver_dir/security-review.mjs" "$driver_dir/security-review-driver.mjs"
      node "$trusted_validator" scope --base "$EXPECTED_BASE_SHA" --head "$EXPECTED_HEAD_SHA" \
        --pr "$FIXTURE_NUMBER" --relation same-repo --mode repair --output /tmp/gh-aw/security-scope.json
      [ "$(node -p "JSON.parse(require('fs').readFileSync('/tmp/gh-aw/security-scope.json','utf8')).baseSha")" = "$COMPARISON_BASE_SHA" ]
      cp /tmp/gh-aw/security-scope.json "$RUNNER_TEMP/gh-aw/security-report-scope.json"
      node "$trusted_validator" init-report --scope /tmp/gh-aw/security-scope.json --output /tmp/gh-aw/agent/security-findings.json
      node <<'NODE'
      const {execFileSync} = require('node:child_process');
      const {writeFileSync} = require('node:fs');
      const source = execFileSync('git', ['show', `${process.env.TRUSTED_SHA}:.github/workflows/ghaw-pr-security.md`], {encoding:'utf8'});
      const delimiter = '-'.repeat(3);
      const match = source.match(new RegExp(`^${delimiter}\\r?\\n[\\s\\S]*?\\r?\\n${delimiter}\\r?\\n([\\s\\S]*)$`));
      if (!match) throw new Error('Missing trusted production prompt');
      const replacements = {
        pr_number:process.env.FIXTURE_NUMBER,
        comparison_base_sha:process.env.COMPARISON_BASE_SHA,
        expected_head_sha:process.env.EXPECTED_HEAD_SHA
      };
      const body = match[1].replace(/\$\{\{\s*github\.event\.inputs\.([a-z_]+)\s*\}\}/g, (_, key) => {
        if (!Object.hasOwn(replacements, key)) throw new Error('Unknown production prompt identity field');
        return replacements[key];
      });
      if (body.includes('## agent:') || !body.includes('the trusted driver') ||
          !body.includes('review.status: pending')) throw new Error('Expected current native-driver production procedure');
      const identity = `# Native branch fixture identity\n\nfixtureIdentityType: branch\nfixture_number: ${process.env.FIXTURE_NUMBER} (github.run_number)\nhead_ref: ${process.env.HEAD_REF}\nbase_sha: ${process.env.EXPECTED_BASE_SHA}\nhead_sha: ${process.env.EXPECTED_HEAD_SHA}\ntrusted_workflow_sha: ${process.env.TRUSTED_SHA}\nThe legacy report schema prNumber and native --pr argument store this positive synthetic fixture ordinal, NOT an actual GitHub PR ID. No PR or PR URL exists. PR routing and comment paths are not exercised. Preserve the native scope identity and hash unchanged.\n\n`;
      writeFileSync('/tmp/gh-aw/production-security-procedure.md', identity + body);
      writeFileSync('/tmp/gh-aw/security-branch-identity.json', JSON.stringify({
        fixtureIdentityType:'branch', fixtureNumber:Number(process.env.FIXTURE_NUMBER),
        headRef:process.env.HEAD_REF, baseSha:process.env.EXPECTED_BASE_SHA,
        headSha:process.env.EXPECTED_HEAD_SHA, trustedWorkflowSha:process.env.TRUSTED_SHA,
        legacyPrNumberIsSyntheticOrdinal:true, prRoutingExercised:false, commentPathExercised:false}, null, 2));
      NODE

pre-agent-steps:
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
      EXPECTED_BASE_SHA: ${{ needs.prepare.outputs.expected_base_sha }}
      EXPECTED_HEAD_SHA: ${{ needs.prepare.outputs.expected_head_sha }}
      COMPARISON_BASE_SHA: ${{ needs.prepare.outputs.comparison_base_sha }}
      FIXTURE_NUMBER: ${{ needs.prepare.outputs.fixture_number }}
      HEAD_REF: ${{ needs.prepare.outputs.head_ref }}
      REPOSITORY: ${{ github.repository }}
      TRUSTED_SHA: ${{ github.workflow_sha }}
    run: |
      set -euo pipefail
      [ "$HEAD_REF" = test/yeelam/ghaw-security-repair-fixture-20261007-r5 ]
      [ "$TRUSTED_SHA" = "$EXPECTED_BASE_SHA" ]
      current_head="$(gh api "/repos/$REPOSITORY/git/ref/heads/$HEAD_REF" --jq .object.sha)"
      [ "$current_head" = "$EXPECTED_HEAD_SHA" ]
      [ "$(gh api "/repos/$REPOSITORY/git/ref/heads/test/yeelam/ghaw-security-repair-e2e-20261007-r5" --jq .object.sha)" = "$EXPECTED_BASE_SHA" ]
      trusted_validator="$RUNNER_TEMP/security-review-final.mjs"
      git show "$TRUSTED_SHA:.github/skills/ghaw-pr-security/scripts/security-review.mjs" > "$trusted_validator"
      source_workspace="$GITHUB_WORKSPACE"
      trusted_workspace="$RUNNER_TEMP/security-repair-workspace"
      trusted_scope="$RUNNER_TEMP/security-scope.final.json"
      trusted_report=/tmp/gh-aw/security-findings.proposed.json
      rm -rf "$trusted_workspace"
      rm -f "$trusted_scope" "$trusted_report" \
        /tmp/gh-aw/security-findings.validated.json /tmp/gh-aw/security-summary.md \
        /tmp/gh-aw/security-status.txt /tmp/gh-aw/security-repair.patch
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
      git -C "$trusted_workspace" fetch --quiet --no-tags origin "$EXPECTED_BASE_SHA" "$EXPECTED_HEAD_SHA"
      git -C "$trusted_workspace" checkout --quiet --detach "$EXPECTED_HEAD_SHA"
      git -C "$trusted_workspace" remote remove origin
      rm -f "$askpass"
      unset GIT_ASKPASS GH_TOKEN
      pushd "$trusted_workspace"
      node "$trusted_validator" scope --base "$EXPECTED_BASE_SHA" --head "$EXPECTED_HEAD_SHA" \
        --pr "$FIXTURE_NUMBER" --relation same-repo --mode repair --output "$trusted_scope"
      [ "$(node -p "JSON.parse(require('fs').readFileSync('$trusted_scope','utf8')).baseSha")" = "$COMPARISON_BASE_SHA" ]
      patch_count="$(node -p "JSON.parse(require('fs').readFileSync('/tmp/gh-aw/agent/security-findings.json','utf8')).patch.length")"
      if [ "$patch_count" -gt 0 ]; then
        node "$trusted_validator" validate-repair-scope --scope "$trusted_scope"
        node "$trusted_validator" stage-repair --report /tmp/gh-aw/agent/security-findings.json \
          --source "$source_workspace" --target "$trusted_workspace"
      fi
      node "$trusted_validator" validate-proposal --scope "$trusted_scope" \
        --report /tmp/gh-aw/agent/security-findings.json --output "$trusted_report"
      node "$trusted_validator" validate-output --validated "$trusted_report" --agent-output /tmp/gh-aw/agent_output.json
      git diff --binary HEAD > /tmp/gh-aw/security-repair.patch
      popd
      cp "$trusted_scope" /tmp/gh-aw/security-scope.proposed.json
  - name: Upload source-reviewed security proposal
    if: always()
    uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a
    with:
      name: ghaw-pr-security-proposal-${{ needs.prepare.outputs.fixture_number }}-${{ needs.prepare.outputs.expected_head_sha }}
      path: |
        /tmp/gh-aw/security-scope.proposed.json
        /tmp/gh-aw/security-findings.proposed.json
        /tmp/gh-aw/security-repair.patch
      if-no-files-found: error
      retention-days: 14

  - name: Retain branch-only scope identity separately
    if: always()
    uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a
    with:
      name: security-e2e-branch-scope-${{ github.run_id }}
      path: /tmp/gh-aw/security-branch-identity.json
      if-no-files-found: error
      retention-days: 14

timeout-minutes: 15
max-ai-credits: 30
max-daily-ai-credits: 60
concurrency:
  group: official-security-e2e-single-attempt
  job-discriminator: ${{ github.run_id }}
  cancel-in-progress: false
run-name: 'Official corrected security repair E2E'
---

Read `/tmp/gh-aw/security-scope.json` for the actual immutable source scope and
`/tmp/gh-aw/security-branch-identity.json` for the branch fixture identity.
`fixture_number` is `github.run_number`; the legacy report `prNumber` and native
`--pr` argument store this positive synthetic ordinal, NOT a GitHub PR ID.
There is no PR or fabricated PR URL. PR routing and comment paths are not tested.
Read and follow `/tmp/gh-aw/production-security-procedure.md`: it is the exact
trusted production caller procedure, with only its dispatch identities replaced
by native scope identities. Use the imported production role, skill, and tools.
This caller's native publication job, not the agent, owns publication.
Do not infer a finding, repair, or pass from this workflow's name or purpose.
Do not invoke another agent. Submit any candidate with `review.status: pending`
and disposition `proposed`; the unchanged production native driver alone starts
the fixed read-only reviewer profile and verifies its actual native diff,
base/head source, and final candidate reads before accepting source approval.
