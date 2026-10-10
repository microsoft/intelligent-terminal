---
---

## Performance workflow report contract

The workflow fixes review mode, PR identity, output paths and available tools.
Use the shared skill for performance reasoning and the human summary; this
contract defines only the structured handoff to trusted workflow processing.

Submit this version-1 report through `validate_performance_report`. Correct
field or identity errors before completing the handoff. Each finding needs
every required field, including its own `location`.

```json
{
  "version": 1,
  "review": "performance",
  "mode": "repair | guide",
  "identity": {
    "prNumber": 123,
    "baseSha": "40 lowercase hex",
    "headSha": "40 lowercase hex"
  },
  "status": "pass | advisory | action_required | pending_validation | blocked",
  "findings": [{
    "id": "PERF-STABLE-SOURCE-IDENTIFIER",
    "severity": "high | medium | low",
    "confidence": "high | medium | low",
    "dimension": "application-performance | responsiveness | memory-growth | ci-runtime-cost",
    "category": "rendering | text-buffer | vt-parsing | ui-thread | tab-pane-lifecycle | wta-runtime | session-log-enumeration | concurrency | other",
    "title": "short title",
    "affectedScenario": "specific user or CI scenario",
    "location": "path:line",
    "observed": "what the immutable head does",
    "expected": "baseline or required behavior",
    "impact": "evidenced consequence",
    "nativeEnvironment": {
      "architecture": "windows-x64 | windows-arm64 | not-measured",
      "details": "OS/build/tooling or why unavailable"
    },
    "evidence": [{
      "type": "source | complexity-proof | blocking-proof | resource-proof",
      "detail": "specific proof"
    }],
    "proposedFix": "small recommendation or applied change",
    "validation": "exact command/result or handoff",
    "fixDisposition": "proposed | manual_required | unsafe | advice_only"
  }],
  "checks": [{
    "name": "check name",
    "status": "pass | regression | noisy | unavailable | error",
    "command": "exact command or read-only inspection",
    "exitCode": 0,
    "detail": "result and limitation"
  }]
}
```

Use the fixed workflow mode and identity, not the alternatives/placeholders in
the example. Stable IDs begin `PERF-` and identify the source defect rather than
array order. A not-run check has `exitCode: null`; an `unavailable` check always
uses that value.

Measurement evidence uses `type: measurement` plus `kind`:
`microbenchmark`, `end-to-end` or `profile`. Optional `noisy` is boolean,
`samples` is a positive integer, and `spread` describes observed variation in
at least three characters. Noisy measurements require three or more samples
and their spread. Source/proof evidence omits measurement-only fields.

| Review state | Report status |
| --- | --- |
| Check with `status: error` | `blocked` |
| Eligible HIGH source proposal | `pending_validation` |
| Unresolved HIGH | `action_required` |
| Only MEDIUM/LOW advice | `advisory` |
| No findings | `pass` |

Apply these rows in order. Report status describes findings and check errors,
not coverage completeness. An unavailable check remains explicit with a null
exit code and a human-summary gap; incomplete required analysis still prevents
repair through the workflow's eligibility and trusted sealing gates.

A repair proposal additionally supplies
`validationPlan: { "type": "wta-unit", "testFilter": "<actual qualified test function>" }`.
Read the selector from immutable source; native listing verifies its identity.
Proposal checks describe pending repair validation with `unavailable` and
`exitCode: null`. Put completed input-analysis results in the human summary
and finding evidence, since those runs do not validate the new source.

Review-time reports contain proposed changes, not `fixed` claims. Trusted
processing independently validates the actual report, source and native job
results; only the controller can publish a tested correction as fixed.
