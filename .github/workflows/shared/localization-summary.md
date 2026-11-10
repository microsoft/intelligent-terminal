---
description: 'Agent-authored localization summary, uploaded unchanged for the controller to publish.'
pre-agent-steps:
  - name: Reset localization summary
    shell: pwsh
    run: |
      $ErrorActionPreference = 'Stop'
      $path = '/tmp/gh-aw/agent/localization-summary.md'
      [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($path)) | Out-Null
      [IO.File]::WriteAllText($path, '')
post-steps:
  - name: Require complete localization outcome before publication
    shell: pwsh
    run: |
      $ErrorActionPreference = 'Stop'
      $output = Get-Content -LiteralPath '/tmp/gh-aw/agent_output.json' -Raw | ConvertFrom-Json
      $items = @($output.items)
      if (@($output.errors).Count -gt 0 -or $items.Count -ne 1 -or
          $items[0].type -cnotin @('noop', 'push_to_pull_request_branch')) {
        throw 'Localization did not complete: require exactly one native noop or push outcome, without errors. Inspect agent-output-fallback and agent logs for report_incomplete, missing_tool, or ambiguous output.'
      }
      $path = '/tmp/gh-aw/agent/localization-summary.md'
      $file = Get-Item -LiteralPath $path
      if ($file.PSIsContainer -or $file.LinkType -or $file.Length -eq 0 -or $file.Length -gt 64 * 1024) {
        throw 'The agent-authored localization summary must be a non-empty regular Markdown file at most 64 KiB.'
      }
      $summary = [IO.File]::ReadAllText($path)
      foreach ($field in @('Final checks', 'Independent review')) {
        $verdicts = [regex]::Matches($summary, "(?m)^\*\*$field`:\*\* (.+?)\r?$")
        if ($verdicts.Count -ne 1 -or $verdicts[0].Groups[1].Value -cne 'PASS') {
          throw "Localization is incomplete: $field must explicitly be PASS, based on actual execution."
        }
      }
  - name: Upload agent-authored localization summary
    if: always()
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
The workflow pre-creates an empty file; use the native file edit tool to write
your final Markdown, including when execution is blocked. Do not generate the
prose with a script, render it from checker JSON, or ask another agent to write
it. The workflow publishes this file verbatim; it does not compose a summary.
Keep it under 64 KiB and replace every placeholder with observed evidence.
Keep each verdict line to the status token alone; put qualifications and
execution evidence in the Verification section, not after `PASS`.

```markdown
## Localization review

**Result:** <what was verified, repaired, or still needs attention>
**Final checks:** <PASS, FIXABLE, BLOCKED, or INVALID_INPUT>
**Independent review:** <PASS, FIXABLE, BLOCKED, or INVALID_INPUT>

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
The two verdict lines are attestations, not substitute checks: use `BLOCKED`
when a command was denied, failed to execute, or the independent reviewer did
not run. A prose claim of success cannot override an incomplete native outcome.
