---
description: 'Agent-authored localization summary, uploaded unchanged for the controller to publish.'
pre-agent-steps:
  - name: Reset localization summary
    shell: pwsh
    run: |
      $ErrorActionPreference = 'Stop'
      $path = '/tmp/gh-aw/agent/localization-summary.md'
      if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Force }
post-steps:
  - name: Upload agent-authored localization summary
    uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
    with:
      name: localization-summary
      path: /tmp/gh-aw/agent/localization-summary.md
      if-no-files-found: error
      retention-days: 7
---

## Human-readable summary

After the checks and required review, author
`/tmp/gh-aw/agent/localization-summary.md` yourself using the template below.
Write your final Markdown with PowerShell file operations. Do not generate the
prose with a script, render it from checker JSON, or ask another agent to write
it. The workflow publishes this file verbatim; it does not compose a summary.
Keep it under 64 KiB and replace every placeholder with observed evidence.

```markdown
## Localization review

**Result:** <what was verified, repaired, or still needs attention>

### Scope and findings
<English-source changes reviewed; affected files/locales and key identifiers;
actionable checker findings and human-language findings, distinguished clearly>

### Verification
<Actual checks and their outcomes; required independent review verdict, or
state that no separate reviewer was required; historical snapshot evidence
must not be described as current-tree validation>

### Changes and limitations
<Localized repairs committed or no changes needed;
anything blocked, unverified, or excluded from scope>

[Workflow run and usage artifacts](https://github.com/${{ github.repository }}/actions/runs/${{ github.run_id }})
```

Do not quote untrusted resource values or invent counts, check results,
language-quality certification, AI Credits, or token usage. Final billing
telemetry is collected after your execution; link to the run's usage artifacts
instead of guessing those numbers. This summary does not replace running the
actual checks, assessing scope and language quality, or the required safe-output
action.
