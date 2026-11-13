---
name: Architecture and Design Review
description: 'Read-only source-backed design review for materially sized same-repository PRs.'

on:
  pull_request_target:
    types: [opened, reopened, synchronize, ready_for_review]
  roles: [admin, maintainer, write]

if: github.event.pull_request.draft == false && github.event.pull_request.head.repo.id == github.repository_id

permissions:
  contents: read
  pull-requests: read
  copilot-requests: write

engine:
  id: copilot
  agent: architecture-design-reviewer
  bare: true
  env:
    OTEL_EXPORTER_OTLP_HEADERS: "${{ '' }}"
    GH_AW_OTLP_ENDPOINTS: "${{ '' }}"
    GH_AW_OTLP_ALL_HEADERS: "${{ '' }}"
  harness:
    max-retries: 0

imports:
  - .github/agents/architecture-design-reviewer.agent.md
skills:
  - .github/skills/architecture-design-review

checkout:
  # Candidate commits remain Git objects; the workspace is trusted workflow code.
  repository: ${{ github.repository }}
  ref: ${{ github.workflow_sha }}
  fetch-depth: 0

env:
  GIT_NO_REPLACE_OBJECTS: '1'
  ARCHITECTURE_REVIEW_DIR: Generated Files/architecture-design-${{ github.run_id }}-${{ github.run_attempt }}

tools:
  github: false
  edit: false
  cli-proxy: false
  bash: false

mcp-scripts:
  read-architecture-evidence:
    description: 'Read bounded immutable diff/source/search/tree evidence. No shell, Git options, arbitrary revisions, or filesystem reads. Errors are coverage gaps.'
    inputs:
      operation:
        type: string
        description: 'Allowed enum: diff, source, search, tree (validated by the trusted helper).'
        required: true
      revision:
        type: string
        description: 'Allowed enum: base, head (validated by the trusted helper).'
        default: head
      path:
        type: string
        default: ''
      start_line:
        type: number
        default: 1
      end_line:
        type: number
        default: 400
      query:
        type: string
        default: ''
      offset:
        type: number
        default: 0
      limit:
        type: number
        default: 100
    env:
      ARCHITECTURE_WORKSPACE: ${{ github.workspace }}
      ARCHITECTURE_RUN_DIR: Generated Files/architecture-design-${{ github.run_id }}-${{ github.run_attempt }}
    script: |
      const { readFileSync } = await import('node:fs');
      const { pathToFileURL } = await import('node:url');
      const root = process.env.ARCHITECTURE_WORKSPACE;
      const { readEvidence } = await import(pathToFileURL(
        `${root}/.github/workflows/ghaw-pr-architecture-design/scripts/evidence-reader.mjs`).href);
      const scope = JSON.parse(readFileSync(
        `${root}/${process.env.ARCHITECTURE_RUN_DIR}/scope.json`, 'utf8'));
      return readEvidence(root, scope, { operation, revision, path, start_line, end_line, query, offset, limit });

jobs:
  prepare:
    needs: []
    runs-on: ubuntu-latest
    timeout-minutes: 5
    permissions:
      contents: read
      pull-requests: read
    outputs:
      admitted: ${{ steps.gate.outputs.admitted }}
      comparison_base: ${{ steps.gate.outputs.comparison_base }}
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          ref: ${{ github.workflow_sha }}
          fetch-depth: 0
          persist-credentials: false
      - name: Admit material immutable changes before inference
        id: gate
        uses: actions/github-script@3a2844b7e9c422d3c10d287c895573f7108da1b3 # v9.0.0
        env:
          GH_TOKEN: ${{ github.token }}
        with:
          script: |
            const { pathToFileURL } = require('node:url');
            const { prepareReview } = await import(pathToFileURL(
              `${process.env.GITHUB_WORKSPACE}/.github/workflows/ghaw-pr-architecture-design/scripts/architecture-design-review.mjs`));
            const scope = await prepareReview(github, context, process.env.GITHUB_WORKSPACE);
            core.setOutput('admitted', String(scope.admitted));
            core.setOutput('comparison_base', scope.comparisonBase);
            await core.summary.addRaw(`Architecture/design admission: ${scope.relevantLines} relevant added+deleted lines, ${scope.relevantFiles} files.\n\n` +
              (scope.admitted ? 'Source review admitted; size alone is not a finding.' :
                'Below size threshold: inference and publication skipped. This is not a clean design assessment. AI review credits: 0.')).write();

  activation:
    needs: [prepare]
    if: needs.prepare.outputs.admitted == 'true'

  agent:
    needs: [prepare]
    if: needs.prepare.outputs.admitted == 'true'
    timeout-minutes: 35

  safe_outputs:
    needs: [agent]
    if: needs.agent.result == 'success' && needs.detection.result == 'success'
    permissions:
      contents: read
      issues: write
      pull-requests: write

pre-agent-steps:
  - name: Prepare the admitted snapshot using trusted code
    uses: actions/github-script@3a2844b7e9c422d3c10d287c895573f7108da1b3
    env:
      GH_TOKEN: ${{ github.token }}
      COMPARISON_BASE: ${{ needs.prepare.outputs.comparison_base }}
    with:
      script: |
        const { pathToFileURL } = require('node:url');
        const path = require('node:path');
        const { prepareReview } = await import(pathToFileURL(
          `${process.env.GITHUB_WORKSPACE}/.github/workflows/ghaw-pr-architecture-design/scripts/architecture-design-review.mjs`));
        const scope = await prepareReview(github, context, process.env.GITHUB_WORKSPACE,
          path.join(process.env.GITHUB_WORKSPACE, process.env.ARCHITECTURE_REVIEW_DIR));
        if (!scope.admitted || scope.comparisonBase !== process.env.COMPARISON_BASE)
          throw Error('Review no longer matches the admitted snapshot.');

safe-outputs:
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1
      with:
        ref: ${{ github.workflow_sha }}
        persist-credentials: false
    - name: Reject stale review before publication
      uses: actions/github-script@3a2844b7e9c422d3c10d287c895573f7108da1b3
      with:
        script: |
          const { pathToFileURL } = require('node:url');
          const { verifySnapshot } = await import(pathToFileURL(
            `${process.env.GITHUB_WORKSPACE}/.github/workflows/ghaw-pr-architecture-design/scripts/architecture-design-review.mjs`));
          const expected = context.payload.pull_request;
          const pr = (await github.rest.pulls.get({ ...context.repo, pull_number: expected.number })).data;
          verifySnapshot(pr, expected, context.payload.repository);
  add-comment:
    target: triggering
    max: 1
  noop:
    report-as-issue: false
  missing-tool: false
  missing-data: false
  report-failure-as-issue: false
  report-failed-jobs: false
  report-incomplete:
    create-issue: false
  threat-detection:
    continue-on-error: false
    max-ai-credits: 30
    retries: 0

timeout-minutes: 25
max-ai-credits: 500
concurrency:
  group: ghaw-pr-architecture-design-${{ github.event.pull_request.number }}
  cancel-in-progress: true
---

# Architecture and design review

Use the imported reviewer and architecture-design-review skill. Read
the admitted snapshot through `read-architecture-evidence`; every result identifies
its comparison base and reviewed head. Use operation `diff` (optional exact path),
`source` (required path, base/head selector, up to 400 lines), `search` (literal
query up to 256 bytes, optional path), or `tree` (offset/limit paging, up to 200).
The working tree holds trusted workflow code, not the candidate: inspect candidate
documents and callers through the bounded tool. There is no shell access.
Record tool errors and binary/oversized/missing data as coverage gaps, never a
clean assessment. Do not load candidate agents, skills, hooks or MCP settings.
Counting exclusions are not review exclusions; inspect supporting excluded
changes where they affect a traced design consequence.

Request exactly one `add_comment` on the triggering PR with the skill's report.
Start its body with `## Architecture and design review` and the full comparison
base and reviewed head SHAs. Include immutable source links using
`${{ github.server_url }}/${{ github.repository }}/blob/<revision>/<path>#L<line>`.
Report accepted tradeoffs and coverage gaps even when no finding is supported.
This workflow provides advice, not approval, repairs or build/test validation.
