---
mcp-scripts:
  read-ci-security-findings:
    description: 'Read one bounded page of matching-CI high/medium warnings. Changed-line flags do not establish whether a warning is related to the PR.'
    inputs:
      offset:
        type: integer
        default: 0
      limit:
        type: integer
        default: 20
    env:
      SECURITY_NATIVE_DIR: ${{ runner.temp }}/gh-aw
      SECURITY_REVIEW_RUN_URL: ${{ github.server_url }}/${{ github.repository }}/actions/runs/${{ github.run_id }}
      SECURITY_REPOSITORY_URL: ${{ github.server_url }}/${{ github.repository }}
    script: |
      const { readFileSync } = await import("node:fs");
      const { pathToFileURL } = await import("node:url");
      const directory = process.env.SECURITY_NATIVE_DIR;
      const validator = await import(pathToFileURL(`${directory}/ci-security.mjs`).href);
      const scope = JSON.parse(readFileSync(`${directory}/security-report-scope.json`, "utf8"));
      const page = validator.readWarningPage(`${directory}/ci-security`, scope, offset, limit);
      return {
        ...page,
        ciRunUrl: `${process.env.SECURITY_REPOSITORY_URL}/actions/runs/${page.ci.runId}`,
        proposalArtifact: "security-ci-warning-dispositions.json",
        reviewRunUrl: process.env.SECURITY_REVIEW_RUN_URL,
      };

  propose-ci-warning-decisions:
    description: 'Record fix-or-ignore proposals for one to twenty known CI warnings. These are owner/coding-agent handoff proposals, not applied fixes or suppressions.'
    inputs:
      decisions_json:
        type: string
        required: true
        description: 'JSON array with id, decision (fix/ignore), relatedToPr (introduced/existing-related/unrelated/uncertain), reason and proposal. Maximum 10 KiB.'
    env:
      SECURITY_NATIVE_DIR: ${{ runner.temp }}/gh-aw
    script: |
      const { readFileSync } = await import("node:fs");
      const { pathToFileURL } = await import("node:url");
      const directory = process.env.SECURITY_NATIVE_DIR;
      const validator = await import(pathToFileURL(`${directory}/ci-security.mjs`).href);
      const scope = JSON.parse(readFileSync(`${directory}/security-report-scope.json`, "utf8"));
      return validator.proposeWarningDecisions(`${directory}/ci-security`, scope, decisions_json);

  submit-security-report:
    description: 'Validate and write one guidance report or pending source-fix proposal before build/test to its fixed native destination; accepts data, never commands.'
    inputs:
      report_json:
        type: string
        required: true
        description: 'Complete report JSON preserving immutable identity; repair candidates await trusted independent review and cannot submit source approval.'
      summary_markdown:
        type: string
        required: true
        description: 'Agent-authored Markdown using the skill template, maximum 10 KiB UTF-8. One {{commit:N}} token per proposed finding in report order; never claim built, tested, or Fixed.'
    env:
      SECURITY_NATIVE_DIR: ${{ runner.temp }}/gh-aw
    script: |
      const { readFileSync } = await import("node:fs");
      const { pathToFileURL } = await import("node:url");
      const directory = process.env.SECURITY_NATIVE_DIR;
      const validator = await import(pathToFileURL(`${directory}/security-review-check.mjs`).href);
      const scope = JSON.parse(readFileSync(`${directory}/security-report-scope.json`, "utf8"));
      return validator.submitSecurityReport(report_json, scope, "/tmp/gh-aw/agent/security-findings.json", summary_markdown);

  read-security-diff:
    description: 'Read immutable base/head diff hunks without shell, external diff drivers, or text conversion.'
    inputs:
      paths_json:
        type: string
        default: '[]'
        description: 'JSON array of up to 20 normalized repository paths; empty means the complete immutable diff.'
    env:
      SECURITY_NATIVE_DIR: ${{ runner.temp }}/gh-aw
      SECURITY_WORKSPACE: ${{ github.workspace }}
    script: |
      const { readFileSync } = await import("node:fs");
      const { pathToFileURL } = await import("node:url");
      const directory = process.env.SECURITY_NATIVE_DIR;
      const validator = await import(pathToFileURL(`${directory}/security-review-check.mjs`).href);
      const scope = JSON.parse(readFileSync(`${directory}/security-report-scope.json`, "utf8"));
      return { baseSha: scope.baseSha, headSha: scope.headSha, diff: validator.readSecurityDiff(scope, JSON.parse(paths_json), process.env.SECURITY_WORKSPACE) };

  read-security-source:
    description: 'Read a bounded source range from immutable Git blobs, never filesystem commands or executable scripts.'
    inputs:
      revision:
        type: string
        enum: [base, head]
        required: true
      path:
        type: string
        required: true
      start_line:
        type: integer
        default: 1
      end_line:
        type: integer
        default: 400
    env:
      SECURITY_NATIVE_DIR: ${{ runner.temp }}/gh-aw
      SECURITY_WORKSPACE: ${{ github.workspace }}
    script: |
      const { readFileSync } = await import("node:fs");
      const { pathToFileURL } = await import("node:url");
      const directory = process.env.SECURITY_NATIVE_DIR;
      const validator = await import(pathToFileURL(`${directory}/security-review-check.mjs`).href);
      const scope = JSON.parse(readFileSync(`${directory}/security-report-scope.json`, "utf8"));
      return { revision, path, source: validator.readSecuritySource(scope, revision, path, start_line, end_line, process.env.SECURITY_WORKSPACE) };

  inspect-security-repair:
    description: 'Read the current repair patch and its native SHA-256 against the immutable original head; no execution or mutation.'
    env:
      SECURITY_NATIVE_DIR: ${{ runner.temp }}/gh-aw
      SECURITY_WORKSPACE: ${{ github.workspace }}
    script: |
      const { readFileSync } = await import("node:fs");
      const { pathToFileURL } = await import("node:url");
      const directory = process.env.SECURITY_NATIVE_DIR;
      const validator = await import(pathToFileURL(`${directory}/security-review-check.mjs`).href);
      const scope = JSON.parse(readFileSync(`${directory}/security-report-scope.json`, "utf8"));
      return validator.inspectSecurityRepair(scope, process.env.SECURITY_WORKSPACE);

  write-security-repair:
    description: 'Replace text only in an existing modified WTA Rust file from authorized same-repository repair scope; guide mode always rejects.'
    inputs:
      path:
        type: string
        required: true
      edits_json:
        type: string
        required: true
        description: 'JSON array of 1 to 8 exact oldText/newText replacements, maximum 8 KiB. Each oldText must match once; no commands or new-file paths.'
    env:
      SECURITY_NATIVE_DIR: ${{ runner.temp }}/gh-aw
      SECURITY_WORKSPACE: ${{ github.workspace }}
    script: |
      const { readFileSync } = await import("node:fs");
      const { pathToFileURL } = await import("node:url");
      const directory = process.env.SECURITY_NATIVE_DIR;
      const validator = await import(pathToFileURL(`${directory}/security-review-check.mjs`).href);
      const scope = JSON.parse(readFileSync(`${directory}/security-report-scope.json`, "utf8"));
      return validator.replaceSecurityRepairText(scope, process.env.SECURITY_WORKSPACE, path, edits_json);
---
