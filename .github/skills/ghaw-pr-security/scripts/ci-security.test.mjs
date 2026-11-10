import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import test from 'node:test';
import {
  collectSarif, completedWarningReview, proposeWarningDecisions, readCiWarnings, readWarningPage,
} from './ci-security.mjs';
import { validateCiWarningProposals } from './security-review.mjs';

const HEAD = 'a'.repeat(40);
const CI = { headSha: HEAD, ciRunId: '123', ciRunAttempt: 2 };
const scope = { headSha: HEAD, immutableHunks: [{
  path: 'tools/wta/src/example.rs', hunks: [{ headStart: 10, headCount: 2 }],
}] };

function sarif() {
  return {
    version: '2.1.0',
    runs: [{
      tool: { driver: { name: 'CodeQL', rules: [3, 5, 8].map(score => ({
        id: `rust/example-${score}`, shortDescription: { text: 'Example analyzer diagnostic' },
        helpUri: 'https://codeql.github.com/codeql-query-help/rust/example/',
        properties: { 'security-severity': String(score) },
      })) } },
      invocations: [{ executionSuccessful: true }],
      results: [3, 5, 8].map(score => ({
        ruleId: `rust/example-${score}`,
        message: { text: 'Untrusted source detail must not be copied.' },
        locations: [{ physicalLocation: {
          artifactLocation: { uri: 'tools/wta/src/example.rs' },
          region: { startLine: score === 8 ? 10 : 30 },
        } }],
      })),
    }],
  };
}

function fixture(run) {
  const directory = mkdtempSync(resolve(process.cwd(), '.ci-security-test-'));
  try {
    for (const language of ['cpp', 'rust']) {
      mkdirSync(resolve(directory, language));
      const input = sarif();
      if (language === 'cpp') input.runs[0].results = [];
      writeFileSync(resolve(directory, language, 'findings.json'),
        JSON.stringify(collectSarif(input, { ...CI, language })));
    }
    writeFileSync(resolve(directory, 'identity.json'), JSON.stringify({
      headSha: HEAD, runId: '123', runAttempt: 2,
    }));
    run(directory);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

test('security analysis keeps high/medium warnings without source snippets or untrusted messages', () => {
  const report = collectSarif(sarif(), { ...CI, language: 'rust' });
  assert.deepEqual(report.counts, { total: 3, high: 1, medium: 1, low: 1, unclassified: 0, outsideWorkspace: 0, duplicates: 0 });
  assert.deepEqual(report.findings.map(item => item.priority), ['high', 'medium']);
  assert(!JSON.stringify(report).includes('Untrusted source detail'));
  const duplicate = sarif();
  duplicate.runs[0].results.push(duplicate.runs[0].results[2]);
  assert.equal(collectSarif(duplicate, { ...CI, language: 'rust' }).findings.length, 2);
  assert.equal(collectSarif(duplicate, { ...CI, language: 'rust' }).counts.duplicates, 1);
});

test('analyzer crashes, unknown formats and traversal do not become clean analysis', () => {
  const failed = sarif();
  failed.runs[0].invocations[0].executionSuccessful = false;
  assert.throws(() => collectSarif(failed, { ...CI, language: 'rust' }), /complete successfully/);
  assert.throws(() => collectSarif({}, { ...CI, language: 'rust' }));
  const outside = sarif();
  outside.runs[0].results[2].locations[0].physicalLocation.artifactLocation.uri = '../outside.rs';
  const result = collectSarif(outside, { ...CI, language: 'rust' });
  assert.equal(result.counts.outsideWorkspace, 1);
  assert.equal(result.findings.length, 1);
});

test('source CI run, attempt and head must match for both languages', () => {
  for (const extra of [{ headSha: 'b'.repeat(40) }, { ciRunId: '456' }, { ciRunAttempt: 3 }]) {
    fixture(directory => {
      const path = resolve(directory, 'rust', 'findings.json');
      writeFileSync(path, JSON.stringify({ ...JSON.parse(readFileSync(path, 'utf8')), ...extra }));
      assert.throws(() => readCiWarnings(directory, scope), /mismatched analyzer artifact/);
    });
  }
  fixture(directory => assert.throws(() => readCiWarnings(directory, { ...scope, headSha: 'b'.repeat(40) })));
  fixture(directory => {
    const path = resolve(directory, 'rust', 'findings.json');
    const report = JSON.parse(readFileSync(path, 'utf8'));
    report.findings[0].rawSource = 'Do not forward unknown artifact fields.';
    writeFileSync(path, JSON.stringify(report));
    assert.throws(() => readCiWarnings(directory, scope), /invalid analyzer warning/);
  });
});

test('changed-line flags are hints, not an automatic unrelated/ignore decision', () => {
  fixture(directory => {
    const first = readWarningPage(directory, scope, 0, 1);
    assert.equal(first.findings[0].onChangedLines, true);
    assert.equal(first.nextOffset, 1);
    const second = readWarningPage(directory, scope, 1, 1);
    assert.equal(second.findings[0].onChangedLines, false);
    assert.equal(second.nextOffset, null);
    assert.match(second.caution, /not necessarily unrelated/);
    assert.throws(() => readWarningPage(directory, scope, 0, 21));
  });
});

test('each reported warning requires an explicit fix-or-ignore proposal for owner approval', () => {
  fixture(directory => {
    const { findings } = readCiWarnings(directory, scope);
    assert.throws(() => completedWarningReview(directory, scope), /every reported warning/);
    const first = { id: findings[0].id, decision: 'fix', relatedToPr: 'introduced',
      reason: 'Changed source removed the required ownership check.', proposal: 'Restore the owner-bound lookup at the listed location.' };
    const second = { id: findings[1].id, decision: 'ignore', relatedToPr: 'unrelated',
      reason: 'The immutable base already uses the documented trusted local setting.',
      proposal: 'Propose ignoring this warning after the owner confirms the existing contract; do not add a blanket suppression.' };
    assert.equal(proposeWarningDecisions(directory, scope, JSON.stringify([first])).remaining, 1);
    assert.throws(() => completedWarningReview(directory, scope), /every reported warning/);
    assert.equal(proposeWarningDecisions(directory, scope, JSON.stringify([second])).remaining, 0);
    const complete = completedWarningReview(directory, scope);
    assert.deepEqual(complete.findings.map(item => item.decision), ['fix', 'ignore']);
    assert.match(complete.ownerAction, /owner or their coding agent decides/);
    assert.match(complete.ownerAction, /does not suppress/);
    assert.equal(complete.findings[0].priority, 'high');
  });
});

test('unknown, duplicate, incomplete or priority-changing proposals are rejected atomically', () => {
  fixture(directory => {
    const item = readCiWarnings(directory, scope).findings[0];
    const valid = { id: item.id, decision: 'fix', relatedToPr: 'uncertain',
      reason: 'Impact requires the owner to confirm the caller contract.', proposal: 'Trace the caller before deciding whether to apply an owner check.' };
    for (const input of [
      [{ ...valid, id: 'CISEC-0000000000000000' }], [valid, valid],
      [{ ...valid, proposal: '' }], [{ ...valid, priority: 'low' }],
      [{ ...valid, decision: 'suppress' }], [{ ...valid, relatedToPr: 'not-related' }],
    ]) assert.throws(() => proposeWarningDecisions(directory, scope, JSON.stringify(input)));
    assert.throws(() => completedWarningReview(directory, scope), /every reported warning/);
  });
});

test('publication binds the complete proposal artifact to the report and exact CI attempt', () => {
    const proposals = { version: 1, ci: { headSha: HEAD, runId: '123', runAttempt: 2 },
      findings: [{ id: 'CISEC-0000000000000001', decision: 'ignore', relatedToPr: 'unrelated',
        reason: 'Existing source uses the documented local-user policy.', proposal: 'Ask the owner to confirm ignoring this existing warning; do not suppress the rule.' }] };
    const bytes = JSON.stringify(proposals);
    const report = { ciWarningDispositionsSha256: createHash('sha256').update(bytes).digest('hex') };
    const environment = { CI_RUN_ID: '123', CI_RUN_ATTEMPT: '2' };
    assert.equal(validateCiWarningProposals(bytes, report, scope, environment).findings.length, 1);
    assert.throws(() => validateCiWarningProposals(`${bytes}\n`, report, scope, environment), /do not match/);
    assert.throws(() => validateCiWarningProposals(bytes, report, scope, { ...environment, CI_RUN_ATTEMPT: '3' }));
    assert.throws(() => validateCiWarningProposals(bytes, report, { ...scope, headSha: 'b'.repeat(40) }, environment));
    const changed = JSON.stringify({ ...proposals, findings: [...proposals.findings, proposals.findings[0]] });
    assert.throws(() => validateCiWarningProposals(changed, {
      ciWarningDispositionsSha256: createHash('sha256').update(changed).digest('hex'),
    }, scope, environment), /duplicate/);
});
