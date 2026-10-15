import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import {
  buildScope, createRepairCommits, createReportTemplate, readImmutableHunks, validateProposal,
  validateSummaryMarkdown,
} from './security-review.mjs';

function fixture(run, { separateFiles = false, shifted = false, finalNewline = true } = {}) {
  const root = mkdtempSync(join(process.cwd(), 'ghaw-finding-commits-'));
  const git = (...args) => execFileSync('git', ['-c', 'core.autocrlf=false',
    '-c', 'core.hooksPath=/dev/null', '-c', 'commit.gpgsign=false', ...args], {
    cwd: root, encoding: 'utf8', timeout: 30_000,
  }).trimEnd();
  const first = 'tools/wta/src/routing.rs';
  const second = separateFiles ? 'tools/wta/src/ownership.rs' : first;
  const source = Array.from({ length: 30 }, (_, index) => `pub fn owner_${index + 1}() -> bool { true }`)
    .join('\n') + (finalNewline ? '\n' : '');
  const paths = [...new Set([first, second])];
  try {
    mkdirSync(join(root, 'tools', 'wta', 'src'), { recursive: true });
    git('init', '--quiet');
    git('config', 'user.name', 'Security contract tests');
    git('config', 'user.email', 'security-contract@example.invalid');
    for (const path of paths) writeFileSync(join(root, path), source);
    git('add', '--', ...paths);
    git('commit', '--quiet', '-m', 'Immutable base');
    const base = git('rev-parse', 'HEAD');
    const locations = [{ path: first, line: 3 }, { path: second, line: 30 }];
    for (const { path, line } of locations) {
      const file = join(root, path);
      writeFileSync(file, readFileSync(file, 'utf8').replace(`owner_${line}() -> bool { true }`, `owner_${line}() -> bool { false }`));
    }
    git('add', '--', ...paths);
    git('commit', '--quiet', '-m', 'Introduced wrong-owner routing');
    const head = git('rev-parse', 'HEAD');
    const raw = paths.map(path => `M\0${path}\0`).join('');
    const inputs = buildScope(base, head, 17, 'same-repo', raw, base, 'repair');
    const scope = buildScope(base, head, 17, 'same-repo', raw, base, 'repair', readImmutableHunks(inputs, root));
    for (const { path, line } of locations) {
      const file = join(root, path);
      writeFileSync(file, readFileSync(file, 'utf8').replace(`pub fn owner_${line}() -> bool { false }`,
        shifted && line === 3 ? `pub fn owner_3() -> bool {\n    true\n}` : `pub fn owner_${line}() -> bool { true }`));
    }
    const patch = execFileSync('git', ['-c', 'core.autocrlf=false', 'diff', '--no-ext-diff', '--binary', '--full-index', head, '--'], {
      cwd: root, encoding: 'utf8', timeout: 30_000,
    });
    const input = {
      ...createReportTemplate(scope), summary: 'Two independently source-reviewed ownership proposals.',
      summaryMarkdown: '| Finding | Commit |\n| --- | --- |\n| First | {{commit:1}} |\n| Second | {{commit:2}} |\n',
      review: { status: 'source-pass', reviewer: 'ghaw-pr-security-reviewer', headSha: head,
        patchSha256: createHash('sha256').update(patch).digest('hex'), evidence: 'Independent source review; no build or tests.' },
      findings: locations.map(({ path, line }, index) => ({
        rule: `session-owner-${index + 1}`, severity: 'high', confidence: 'high', category: 'session-routing',
        file: path, startLine: line, endLine: line, observed: 'Changed route bypasses ownership.',
        expected: 'Preserve ownership.', impact: 'Wrong-session mutation.',
        evidence: [{ kind: 'source-trace', reference: `${path}:${line}`, detail: 'The changed lookup bypasses ownership.' }],
        proposedFix: 'Restore ownership.', validation: 'Original PR reviewer owns build/test validation.',
        fixDisposition: { state: 'proposed', reason: 'Unbuilt and untested source-reviewed proposal.' },
      })),
      patch: paths.map(path => ({ path, summary: 'Restore owner-bound lookup.' })),
    };
    run({ root, git, scope, input, patch, locations, head });
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

for (const options of [{}, { separateFiles: true }, { shifted: true, finalNewline: false }]) {
  test(`one source-only commit per finding preserves exact final bytes: ${JSON.stringify(options)}`, () => {
    fixture(({ root, git, scope, input, patch, locations, head }) => {
      const report = validateProposal(input, scope);
      const initialStatus = git('status', '--porcelain');
      const receipt = createRepairCommits(report, scope, patch, root);
      assert.equal(receipt.headSha, head);
      assert.equal(receipt.patchSha256, report.review.patchSha256);
      assert.equal(receipt.commits.length, 2);
      let parent = head;
      for (const [index, item] of receipt.commits.entries()) {
        assert.equal(item.findingId, report.findings[index].id);
        assert.equal(git('rev-parse', `${item.commitSha}^`), parent);
        assert.match(git('show', '-s', '--format=%B', item.commitSha), new RegExp(`Finding: ${item.findingId}`));
        assert.equal(git('diff-tree', '--no-commit-id', '--name-only', '-r', item.commitSha), locations[index].path);
        if (index === 0) assert.match(git('show', `${item.commitSha}:${locations[1].path}`), /owner_30\(\) -> bool \{ false \}/);
        parent = item.commitSha;
      }
      assert.equal(receipt.publishedHeadSha, parent);
      for (const path of new Set(locations.map(item => item.path))) {
        const actual = execFileSync('git', ['show', `${parent}:${path}`], { cwd: root, timeout: 30_000 });
        assert.deepEqual(actual, readFileSync(join(root, path)));
      }
      assert.equal(git('rev-parse', 'HEAD'), head);
      assert.equal(git('diff', '--cached', '--name-only'), '');
      assert.equal(git('status', '--porcelain'), initialStatus);
    }, options);
  });
}

test('overlapping finding ownership fails without moving refs or index', () => {
  fixture(({ root, git, scope, input, patch, head }) => {
    const overlapping = structuredClone(input);
    overlapping.findings.push({ ...input.findings[0], rule: 'another-owner-rule' });
    assert.throws(() => createRepairCommits(overlapping, scope, patch, root), /uniquely assigned/);
    assert.equal(git('rev-parse', 'HEAD'), head);
    assert.equal(git('diff', '--cached', '--name-only'), '');
  });
});

test('every proposed finding needs a nonempty dedicated edit', () => {
  fixture(({ root, git, scope, input, head, locations }) => {
    const file = join(root, locations[1].path);
    writeFileSync(file, readFileSync(file, 'utf8').replace('owner_30() -> bool { true }', 'owner_30() -> bool { false }'));
    const patch = execFileSync('git', ['-c', 'core.autocrlf=false', 'diff', '--no-ext-diff', '--binary', '--full-index', head, '--'], {
      cwd: root, encoding: 'utf8', timeout: 30_000,
    });
    input.review.patchSha256 = createHash('sha256').update(patch).digest('hex');
    assert.throws(() => createRepairCommits(input, scope, patch, root), /own nonempty repair commit/);
    assert.equal(git('rev-parse', 'HEAD'), head);
    assert.equal(git('diff', '--cached', '--name-only'), '');
  });
});

test('agent Markdown is preserved verbatim; commit placeholders cannot disappear or duplicate', () => {
  const markdown = '## Agent summary\n\n| Finding | Commit |\n| --- | --- |\n| Owner A | {{commit:1}} |\n\nNot built or tested; the original reviewer may validate or revert the fix.\n';
  const report = { findings: [{ fixDisposition: { state: 'proposed' } }], summaryMarkdown: markdown };
  assert.equal(validateSummaryMarkdown(report), markdown);
  for (const summaryMarkdown of ['', markdown.replace('{{commit:1}}', 'missing'),
    markdown.replace('{{commit:1}}', '{{commit:2}}'), `${markdown}{{commit:1}}`, `${markdown}{{commit:unknown}}`]) {
    assert.throws(() => validateSummaryMarkdown({ ...report, summaryMarkdown }));
  }
  assert.equal(validateSummaryMarkdown({ findings: [], summaryMarkdown: 'No introduced regression found.\n' }), 'No introduced regression found.\n');
});
