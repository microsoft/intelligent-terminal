import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { applySuppressionBaseline, collectSarif, validateAnalysisReport } from './ci-security-analysis.mjs';

const CI = { language: 'rust', headSha: 'a'.repeat(40), ciRunId: '123', ciRunAttempt: 2 };
const SOURCE = 'tools/wta/src/example.rs';
const BLOB = 'b'.repeat(40);

function sarif() {
  return { version: '2.1.0', runs: [{
    tool: { driver: { name: 'CodeQL', semanticVersion: '2.27.4', rules: [{
      id: 'rust/example', shortDescription: { text: 'Example analyzer diagnostic' },
      properties: { 'security-severity': '7.5' },
    }] } },
    invocations: [{ executionSuccessful: true }],
    results: [{ ruleId: 'rust/example', message: { text: 'Private diagnostic content' },
      locations: [{ physicalLocation: { artifactLocation: { uri: SOURCE },
        region: { startLine: 10, startColumn: 4, endColumn: 8 } } }] }],
  }] };
}

function baseline(report) {
  return { version: 1, reviewedHeadSha: CI.headSha, sourceBlobs: { [SOURCE]: BLOB, 'src/context.cpp': BLOB },
    entries: [{ language: CI.language, toolVersion: report.analysis.toolVersion,
      finding: report.findings[0], reason: 'Reviewed synthetic fixture, not a production credential.',
      sources: [SOURCE, 'src/context.cpp'] }] };
}

test('accepted instances stay auditable without active warnings or raw diagnostic text', () => {
  const report = collectSarif(sarif(), CI);
  const accepted = applySuppressionBaseline(report, baseline(report), () => BLOB);
  assert.equal(accepted.findings.length, 0);
  assert.equal(accepted.counts.high, 0);
  assert.equal(accepted.counts.total, 1);
  assert.equal(accepted.counts.suppressed, 1);
  assert.equal(accepted.suppressedFindings[0].finding.id, report.findings[0].id);
  assert.match(accepted.suppressedFindings[0].reason, /Reviewed synthetic/);
  assert(!JSON.stringify(accepted).includes('Private diagnostic content'));
  validateAnalysisReport(accepted, CI);
});

test('source, context, analyzer or finding changes reactivate accepted warnings', () => {
  const original = collectSarif(sarif(), CI);
  const accepted = baseline(original);
  for (const changed of [SOURCE, 'src/context.cpp']) {
    const report = applySuppressionBaseline(original, accepted, path => path === changed ? 'c'.repeat(40) : BLOB);
    assert.equal(report.findings.length, 1);
    assert.equal(report.counts.suppressed, 0);
  }
  assert.equal(applySuppressionBaseline(original, accepted, () => undefined).findings.length, 1);
  const version = { ...original, analysis: { ...original.analysis, toolVersion: '2.28.0' } };
  assert.equal(applySuppressionBaseline(version, accepted, () => BLOB).findings.length, 1);
  const changed = sarif();
  changed.runs[0].results[0].locations[0].physicalLocation.region.startColumn = 5;
  assert.equal(applySuppressionBaseline(collectSarif(changed, CI), accepted, () => BLOB).findings.length, 1);
});

test('baseline cannot broadly suppress a rule, file or unreviewed source', () => {
  const report = collectSarif(sarif(), CI);
  const accepted = baseline(report);
  accepted.entries[0].sources = ['src/context.cpp'];
  assert.throws(() => applySuppressionBaseline(report, accepted, () => BLOB), /source binding/);
  const changed = sarif();
  changed.runs[0].results.push({ ...structuredClone(changed.runs[0].results[0]),
    message: { text: 'A new independent instance' } });
  const both = collectSarif(changed, CI);
  const result = applySuppressionBaseline(both, baseline(report), () => BLOB);
  assert.equal(result.counts.high, 1);
  assert.equal(result.counts.suppressed, 1);
  const corrupted = structuredClone(result);
  corrupted.counts.suppressed = 0;
  assert.throws(() => validateAnalysisReport(corrupted, CI), /classifications/);
});

test('CI producer CLI runs without any gh-aw code in its checkout', () => {
  const directory = mkdtempSync(resolve(process.cwd(), '.ci-analysis-test-'));
  try {
    const cli = resolve(directory, 'ci-security-analysis.mjs');
    copyFileSync(fileURLToPath(new URL('./ci-security-analysis.mjs', import.meta.url)), cli);
    const input = resolve(directory, 'input.sarif');
    const output = resolve(directory, 'findings.json');
    writeFileSync(input, JSON.stringify(sarif()));
    const child = spawnSync(process.execPath, [cli, 'collect', '--sarif', input,
      '--language', CI.language, '--head', CI.headSha, '--output', output],
    { cwd: directory, encoding: 'utf8',
      env: { ...process.env, GITHUB_RUN_ID: CI.ciRunId, GITHUB_RUN_ATTEMPT: String(CI.ciRunAttempt) } });
    assert.equal(child.status, 0, child.stderr);
    const report = JSON.parse(readFileSync(output, 'utf8'));
    assert.equal(report.findings.length, 1);
    validateAnalysisReport(report, CI);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
