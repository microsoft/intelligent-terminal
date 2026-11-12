import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawn, spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import test from 'node:test';
import { fileURLToPath, pathToFileURL } from 'node:url';
import {
  completedWarningReview, proposeWarningDecisions, readCiWarnings, readWarningPage,
} from './ci-security.mjs';
import { applySuppressionBaseline, collectSarif } from '../../../../build/scripts/ci-security-analysis.mjs';
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
      tool: { driver: { name: 'CodeQL', semanticVersion: '2.27.4', rules: [3, 5, 8].map(score => ({
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
  assert.deepEqual(report.counts, { total: 3, high: 1, medium: 1, low: 1, unclassified: 0, outsideWorkspace: 0, duplicates: 0, suppressed: 0 });
  assert.deepEqual(report.findings.map(item => item.priority), ['high', 'medium']);
  assert.deepEqual(report.analysis, { toolVersion: '2.27.4', rulesInSarif: 3 });
  assert(!JSON.stringify(report).includes('Untrusted source detail'));
  const duplicate = sarif();
  duplicate.runs[0].results.push(duplicate.runs[0].results[2]);
  assert.equal(collectSarif(duplicate, { ...CI, language: 'rust' }).findings.length, 2);
  assert.equal(collectSarif(duplicate, { ...CI, language: 'rust' }).counts.duplicates, 1);
});

test('consumer reads available evidence without knowing CI job names or build selection', () => {
  for (const languages of [['rust'], ['cpp'], []]) {
    fixture(directory => {
      for (const language of ['cpp', 'rust'].filter(item => !languages.includes(item))) {
        rmSync(resolve(directory, language), { recursive: true });
      }
      const read = readCiWarnings(directory, scope);
      assert.deepEqual(read.analyzers.map(item => item.language), languages);
      assert.equal(read.findings.length, languages.includes('rust') ? 2 : 0);
      const page = readWarningPage(directory, scope);
      assert.deepEqual(page.analyzers.map(item => item.language), languages);
      assert.match(page.caution, /absent analyses are not clean scans/);
      if (!languages.includes('rust')) assert.equal(completedWarningReview(directory, scope).findings.length, 0);
    });
  }
});

test('pack-grouped action SARIF preserves warnings and accepts a clean scan with rule metadata', () => {
  const grouped = sarif();
  const run = grouped.runs[0];
  const rules = run.tool.driver.rules;
  run.tool.driver.rules = [];
  run.tool.extensions = [
    { name: 'codeql/rust-queries', rules: rules.slice(0, 2) },
    { name: 'codeql/extra-queries', rules: rules.slice(2) },
  ];
  assert.deepEqual(collectSarif(grouped, { ...CI, language: 'rust' }),
    collectSarif(sarif(), { ...CI, language: 'rust' }));
  delete run.tool.driver.rules;
  run.results = [];
  const clean = collectSarif(grouped, { ...CI, language: 'rust' });
  assert.equal(clean.findings.length, 0);
  assert.equal(clean.analysis.rulesInSarif, 3);
  run.tool.extensions.push({ rules: [rules[0]] });
  assert.throws(() => collectSarif(grouped, { ...CI, language: 'rust' }), /ambiguous.*rule metadata/);
  run.tool.extensions = {};
  assert.throws(() => collectSarif(grouped, { ...CI, language: 'rust' }), /invalid.*rule metadata/);
});

test('distinct columns and same-span result instances each require their own proposal', () => {
  fixture(directory => {
    const input = sarif();
    const first = structuredClone(input.runs[0].results[2]);
    Object.assign(first.locations[0].physicalLocation.region, { startColumn: 3, endColumn: 6 });
    const second = structuredClone(first);
    Object.assign(second.locations[0].physicalLocation.region, { startColumn: 12, endColumn: 15 });
    const differentFlow = structuredClone(first);
    differentFlow.codeFlows = [{ threadFlows: [{ locations: [{
      location: { message: { text: 'Private flow detail must not be forwarded.' },
        physicalLocation: { artifactLocation: { uri: 'tools/wta/src/example.rs' },
          region: { startLine: 42 } } },
    }] }] }];
    const duplicate = Object.fromEntries(Object.entries(first).reverse());
    input.runs[0].results = [first, second, differentFlow, duplicate];
    const report = collectSarif(input, { ...CI, language: 'rust' });
    assert.equal(report.findings.length, 3);
    assert.equal(report.counts.duplicates, 1);
    assert.equal(new Set(report.findings.map(item => item.id)).size, 3);
    assert.deepEqual(report.findings.map(item => item.startColumn).sort((a, b) => a - b), [3, 3, 12]);
    assert(!JSON.stringify(report).includes('Private flow detail'));
    const reordered = collectSarif({ ...input, runs: [{ ...input.runs[0],
      results: [duplicate, differentFlow, second, first] }] }, { ...CI, language: 'rust' });
    assert.deepEqual(reordered, report);
    writeFileSync(resolve(directory, 'rust', 'findings.json'), JSON.stringify(report));
    const page = readWarningPage(directory, scope);
    const decision = item => ({ id: item.id, decision: 'ignore', relatedToPr: 'unrelated',
      reason: 'Existing fixture result reviewed.', proposal: 'Owner decides this instance separately.' });
    proposeWarningDecisions(directory, scope, JSON.stringify([decision(page.findings[0])]));
    assert.throws(() => completedWarningReview(directory, scope), /every reported warning/);
    proposeWarningDecisions(directory, scope, JSON.stringify(page.findings.slice(1).map(decision)));
    assert.equal(completedWarningReview(directory, scope).findings.length, 3);
    const changed = JSON.parse(readFileSync(resolve(directory, 'rust', 'findings.json'), 'utf8'));
    changed.findings[0].startColumn++;
    writeFileSync(resolve(directory, 'rust', 'findings.json'), JSON.stringify(changed));
    assert.throws(() => readCiWarnings(directory, scope), /invalid analyzer warning/);
  });
});

test('collector diagnostics expose contract failures but not raw malformed evidence', () => {
  fixture(directory => {
    const input = resolve(directory, 'input.sarif');
    for (const [body, expected] of [
      ['{}', /invalid CodeQL SARIF/],
      ['never-publish-this-input', /inspect the analyzer execution and evidence format/],
    ]) {
      writeFileSync(input, body);
      const child = spawnSync(process.execPath, [
        fileURLToPath(new URL('../../../../build/scripts/ci-security-analysis.mjs', import.meta.url)), 'collect',
        '--sarif', input, '--language', 'rust', '--head', HEAD,
        '--output', resolve(directory, 'output.json'),
      ], { encoding: 'utf8', env: { ...process.env, GITHUB_RUN_ID: '123', GITHUB_RUN_ATTEMPT: '2' } });
      assert.equal(child.status, 1);
      assert.match(child.stderr, expected);
      assert(!child.stderr.includes('never-publish-this-input'));
    }
  });
});

test('analyzer crashes, unknown formats and traversal do not become clean analysis', () => {
  const failed = sarif();
  failed.runs[0].invocations[0].executionSuccessful = false;
  assert.throws(() => collectSarif(failed, { ...CI, language: 'rust' }), /complete successfully/);
  assert.throws(() => collectSarif({}, { ...CI, language: 'rust' }));
  const noRules = sarif();
  noRules.runs[0].tool.driver.rules = [];
  noRules.runs[0].results = [];
  assert.throws(() => collectSarif(noRules, { ...CI, language: 'rust' }), /no analysis rules/);
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

test('accepted CI suppressions stay auditable without requiring new agent decisions', () => {
  fixture(directory => {
    const path = resolve(directory, 'rust', 'findings.json');
    const original = JSON.parse(readFileSync(path, 'utf8'));
    const source = original.findings[0].file;
    const blob = 'b'.repeat(40);
    const accepted = applySuppressionBaseline(original, {
      version: 1, reviewedHeadSha: HEAD, sourceBlobs: { [source]: blob },
      entries: original.findings.map(finding => ({ language: 'rust',
        toolVersion: original.analysis.toolVersion, finding, sources: [source],
        reason: 'Previously accepted synthetic test fixture.' })),
    }, () => blob);
    writeFileSync(path, JSON.stringify(accepted));
    const page = readWarningPage(directory, scope);
    assert.equal(page.total, 0);
    assert.equal(page.counts.total, 3);
    assert.equal(page.counts.suppressed, 2);
    const completed = completedWarningReview(directory, scope);
    assert.equal(completed.findings.length, 0);
    assert.equal(completed.suppressedFindings.length, 2);
  });
});

test('native consumer packaging uses the CI-owned contract without repository-relative dependencies', () => {
  fixture(directory => {
    const native = resolve(directory, 'native');
    mkdirSync(native);
    for (const [source, target] of [
      [new URL('./ci-security.mjs', import.meta.url), 'ci-security.mjs'],
      [new URL('./security-review.mjs', import.meta.url), 'security-review.mjs'],
      [new URL('../../../../build/scripts/ci-security-analysis.mjs', import.meta.url), 'ci-analysis-contract.mjs'],
    ]) copyFileSync(fileURLToPath(source), resolve(native, target));
    const child = spawnSync(process.execPath, ['--input-type=module', '-e',
      `const module = await import(${JSON.stringify(pathToFileURL(resolve(native, 'ci-security.mjs')).href)});
       console.log(JSON.stringify(module.readWarningPage(${JSON.stringify(directory)}, ${JSON.stringify(scope)})));`],
    { encoding: 'utf8', timeout: 10000 });
    assert.equal(child.status, 0, child.stderr);
    assert.equal(JSON.parse(child.stdout).total, 2);
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

test('parallel native proposal processes cannot silently overwrite an accepted batch', () => {
  fixture(directory => {
    const findings = readWarningPage(directory, scope).findings;
    const decision = item => [{ id: item.id, decision: 'ignore', relatedToPr: 'unrelated',
      reason: 'Existing fixture reviewed.', proposal: 'Owner decides whether to accept the ignore.' }];
    const ready = resolve(directory, 'writer-ready');
    const release = resolve(directory, 'writer-release');
    const done = resolve(directory, 'writer-done');
    const moduleUrl = new URL('./ci-security.mjs', import.meta.url).href;
    const waitFor = path => {
      const deadline = Date.now() + 10000;
      while (!existsSync(path) && Date.now() < deadline) Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 10);
      assert(existsSync(path), `Timed out waiting for fixture writer: ${path}`);
    };
    const first = spawn(process.execPath, ['--input-type=module', '-e', `
      import fs from 'node:fs';
      import { syncBuiltinESMExports } from 'node:module';
      const originalWrite = fs.writeFileSync;
      fs.writeFileSync = (path, ...args) => {
        if (String(path).endsWith('warning-dispositions.json.next')) {
          originalWrite(${JSON.stringify(ready)}, '');
          const deadline = Date.now() + 15000;
          while (!fs.existsSync(${JSON.stringify(release)}) && Date.now() < deadline) {
            Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 10);
          }
        }
        return originalWrite(path, ...args);
      };
      syncBuiltinESMExports();
      const { proposeWarningDecisions } = await import(${JSON.stringify(moduleUrl)});
      try {
        const result = proposeWarningDecisions(${JSON.stringify(directory)}, ${JSON.stringify(scope)},
          ${JSON.stringify(JSON.stringify(decision(findings[0])))});
        originalWrite(${JSON.stringify(done)}, JSON.stringify(result));
      } catch {
        originalWrite(${JSON.stringify(done)}, '{"accepted":false}');
        process.exitCode = 1;
      }
    `], { stdio: 'ignore' });
    try {
      waitFor(ready);
      const second = spawnSync(process.execPath, ['--input-type=module', '-e', `
        const { proposeWarningDecisions } = await import(${JSON.stringify(moduleUrl)});
        try {
          proposeWarningDecisions(${JSON.stringify(directory)}, ${JSON.stringify(scope)},
            ${JSON.stringify(JSON.stringify(decision(findings[1])))});
        } catch (error) {
          if (!error.message.includes('another proposal batch')) throw error;
          process.exitCode = 2;
        }
      `], { encoding: 'utf8', timeout: 10000 });
      assert([0, 2].includes(second.status), second.stderr);
      writeFileSync(release, '');
      waitFor(done);
      assert.equal(JSON.parse(readFileSync(done, 'utf8')).accepted, true);
      if (second.status === 2) proposeWarningDecisions(directory, scope, JSON.stringify(decision(findings[1])));
      assert.equal(completedWarningReview(directory, scope).findings.length, findings.length);
    } finally {
      writeFileSync(release, '');
      if (!existsSync(done)) first.kill();
    }
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
