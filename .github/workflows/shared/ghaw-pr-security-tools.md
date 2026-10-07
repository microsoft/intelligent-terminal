---
mcp-scripts:
  submit-security-report:
    description: 'Validate and write one security report to its fixed native destination; accepts data, never commands.'
    inputs:
      report_json:
        type: string
        required: true
        description: 'Complete report JSON preserving the native immutable identity and report contract.'
    env:
      SECURITY_NATIVE_DIR: ${{ runner.temp }}/gh-aw
    script: |
      const { readFileSync } = await import("node:fs");
      const { pathToFileURL } = await import("node:url");
      const directory = process.env.SECURITY_NATIVE_DIR;
      const validator = await import(pathToFileURL(`${directory}/security-review-check.mjs`).href);
      const scope = JSON.parse(readFileSync(`${directory}/security-report-scope.json`, "utf8"));
      return validator.submitSecurityReport(report_json, scope, "/tmp/gh-aw/agent/security-findings.json");

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
