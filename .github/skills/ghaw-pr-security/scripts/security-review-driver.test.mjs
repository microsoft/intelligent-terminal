import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import test from 'node:test';
import {
  buildScope, createReportTemplate, submitSecurityReport, validateCandidate, validateProposal, validateReport,
  SECURITY_REPORT_MAX_BYTES, serializeSecurityReport, readSecurityDiff, readSecuritySource,
} from './security-review.mjs';
import {
  buildPhaseArguments, parseTranscript, runSecurityReviewDriver, validateReviewerTranscript as validateTranscript,
} from './security-review-driver.mjs';

const BASE = '1'.repeat(40);
const HEAD = '2'.repeat(40);
const PATH = 'tools/wta/src/master/mod.rs';
const PATCH = `diff --git a/${PATH} b/${PATH}\n`;
const DIGEST = createHash('sha256').update(PATCH).digest('hex');
const DIFF = `diff --git a/${PATH} b/${PATH}\n@@ -1,2 +1,2 @@\n-Original source.\n+Changed source.\n Context.\n`;
const scope = buildScope(BASE, HEAD, 17, 'same-repo', `M\0${PATH}\0`, BASE, 'repair');
const inspection = { headSha: HEAD, patchSha256: DIGEST, patch: PATCH, paths: [PATH] };

function sourceForRange(revision, path, start, end) {
  return Array.from({ length: end - start + 1 }, (_, index) => {
    const line = start + index;
    return `${line}: ${line === 1 ? (revision === 'base' ? 'Original source.' : 'Changed source.') : 'Context.'}`;
  }).join('\n');
}

function validateReviewerTranscript(output, scope, inspection, originalDiff, paths, diffForPaths = () => DIFF,
  findings = candidate().findings, readSource = sourceForRange) {
  return validateTranscript(output, scope, inspection, originalDiff, paths, diffForPaths, findings, readSource);
}

function candidate() {
  return {
    ...createReportTemplate(scope), summary: 'A localized HIGH-confidence repair is proposed.',
    review: { status: 'pending', reviewer: 'ghaw-pr-security-reviewer', evidence: 'Trusted driver review has not run.' },
    findings: [{
      rule: 'session-route-target-binding', severity: 'high', confidence: 'high', category: 'session-routing',
      file: PATH, startLine: 1, endLine: 2, observed: 'A changed route omits its owner check.',
      expected: 'Bind the route to its owner.', impact: 'A request can reach a different session.',
      evidence: [{ kind: 'source-trace', reference: `${PATH}:1-2`, detail: 'Changed lookup skips the owner check.' }],
      proposedFix: 'Restore the owner check.', validation: 'Run the focused wrong-owner test on Windows.',
      fixDisposition: { state: 'proposed', reason: 'Awaiting independent source review and native validation.' },
    }],
    patch: [{ path: PATH, summary: 'Restore the owner-bound lookup.' }],
  };
}

function call(name, value, args = {}, success = true) {
  return [
    { type: 'tool.execution_start', data: { toolCallId: name, toolName: name, arguments: args,
      ...(name.startsWith('mcpscripts-') ? { mcpServerName: 'mcpscripts', mcpToolName: name.slice('mcpscripts-'.length) } : {}),
    } },
    { type: 'tool.execution_complete', data: { toolCallId: name, success, result: { content: JSON.stringify(value) } } },
  ];
}

function terminal() {
  return { type: 'result', exitCode: 0, sessionId: 'deterministic-test-session', usage: {} };
}

function output(events) {
  return `${events.map(event => JSON.stringify(event)).join('\n')}\n`;
}

function reviewerEvents() {
  return [
    ...call('mcpscripts-read_security_diff', { baseSha: BASE, headSha: HEAD, diff: DIFF }, { paths_json: '[]' }),
    ...call('mcpscripts-read_security_source', { revision: 'base', path: PATH, source: sourceForRange('base', PATH, 1, 2) },
      { revision: 'base', path: PATH, start_line: 1, end_line: 2 }),
    ...call('mcpscripts-read_security_source', { revision: 'head', path: PATH, source: sourceForRange('head', PATH, 1, 2) },
      { revision: 'head', path: PATH, start_line: 1, end_line: 2 }).map(event => ({
      ...event, data: { ...event.data, toolCallId: 'head-source' },
    })),
    ...call('mcpscripts-inspect_security_repair', inspection),
    { type: 'assistant.message', data: { phase: 'final_answer', content: JSON.stringify({
      status: 'SOURCE_PASS', headSha: HEAD, patchSha256: DIGEST,
      evidence: 'Independent native source reads prove the localized repair; tests have not run.',
    }), toolRequests: [] } },
    terminal(),
  ];
}

test('line-one native reads plus view cannot authorize a finding at line twenty', () => {
  const events = reviewerEvents();
  for (const index of [2, 4]) events[index].data.arguments.end_line = 1;
  events.splice(8, 0, ...call('view', { content: '20: Real finding proof.' }));
  const finding = { ...candidate().findings[0], startLine: 20, endLine: 20 };
  assert.throws(() => validateReviewerTranscript(output(events), scope, inspection, DIFF, [PATH],
    () => DIFF, [finding], () => '1: Original source.'), /source.*coverage/);
});

function rangeEvents(ranges, diff = DIFF) {
  const events = reviewerEvents().filter(event => !['mcpscripts-read_security_source', 'head-source']
    .includes(event.data?.toolCallId));
  events[1].data.result.content = JSON.stringify({ baseSha: BASE, headSha: HEAD, diff });
  ranges.forEach(([revision, start, end], index) => {
    const pair = call('mcpscripts-read_security_source', {
      path: PATH, revision, source: sourceForRange(revision, PATH, start, end),
    }, { path: PATH, revision, start_line: start, end_line: end })
      .map(event => ({ ...event, data: { ...event.data, toolCallId: `range-${index}` } }));
    events.splice(2, 0, ...pair);
  });
  return events;
}

test('fragment unions cover every finding and legitimate view context remains allowed', () => {
  const findings = [candidate().findings[0], {
    ...candidate().findings[0], startLine: 20, endLine: 21,
  }];
  const events = rangeEvents([['base', 1, 2], ['head', 1, 2],
    ['base', 20, 20], ['base', 21, 21], ['head', 20, 20], ['head', 21, 21]]);
  events.splice(2, 0, ...call('view', { content: 'Supplementary context.' }));
  assert.equal(validateReviewerTranscript(output(events), scope, inspection, DIFF, [PATH],
    () => DIFF, findings).status, 'SOURCE_PASS');
  const missing = rangeEvents([['base', 1, 2], ['head', 1, 2], ['base', 20, 21], ['head', 20, 20]]);
  assert.throws(() => validateReviewerTranscript(output(missing), scope, inspection, DIFF, [PATH],
    () => DIFF, findings), /source.*coverage/);
});

test('insertions and deletions require offset-mapped base context, not head coordinates', () => {
  for (const [hunk, baseStart] of [
    ['@@ -5,3 +5,5 @@', 18], ['@@ -5,5 +5,3 @@', 22],
    ['@@ -5,0 +6,2 @@', 18], ['@@ -6,2 +5,0 @@', 22],
  ]) {
    const diff = `diff --git a/${PATH} b/${PATH}\n${hunk}\n`;
    const findings = [{ ...candidate().findings[0], startLine: 20, endLine: 21 }];
    const events = rangeEvents([['head', 20, 20], ['head', 21, 21],
      ['base', baseStart, baseStart + 1]], diff);
    assert.equal(validateReviewerTranscript(output(events), scope, inspection, diff, [PATH],
      () => diff, findings).status, 'SOURCE_PASS');
    const wrong = rangeEvents([['head', 20, 21], ['base', 20, 21]], diff);
    assert.throws(() => validateReviewerTranscript(output(wrong), scope, inspection, diff, [PATH],
      () => diff, findings), /source.*coverage/);
  }
});

test('inserted finding requires the original hunk basis and rejects forged or incomplete reads', () => {
  const diff = `diff --git a/${PATH} b/${PATH}\n@@ -5,3 +5,5 @@\n`;
  const findings = [{ ...candidate().findings[0], startLine: 8, endLine: 9 }];
  const ranges = [['head', 8, 9], ['base', 5, 6], ['base', 7, 7]];
  assert.equal(validateReviewerTranscript(output(rangeEvents(ranges, diff)), scope, inspection, diff, [PATH],
    () => diff, findings).status, 'SOURCE_PASS');
  for (const mutate of [
    events => { events[3].data.success = false; },
    events => { events[2].data.arguments.path = 'tools/wta/src/wrong.rs'; },
    events => { events[2].data.arguments.revision = 'head'; },
    events => { events[3].data.result.content = JSON.stringify({ path: PATH, revision: 'base', source: 'Output truncated' }); },
    events => { events[3].data.result.content = JSON.stringify({ path: PATH, revision: 'base', source: '7: Wrong blob' }); },
  ]) {
    const events = rangeEvents(ranges, diff);
    mutate(events);
    assert.throws(() => validateReviewerTranscript(output(events), scope, inspection, diff, [PATH],
      () => diff, findings), /source.*coverage/);
  }
});

function withFixture(run) {
  const directory = resolve('scratch');
  mkdirSync(directory, { recursive: true });
  const root = mkdtempSync(join(directory, 'security-driver-fixture-'));
  try {
    const workspace = join(root, 'candidate');
    mkdirSync(workspace);
    const reportPath = join(root, 'report.json');
    const scopePath = join(root, 'scope.json');
    const binary = join(root, 'copilot.exe');
    writeFileSync(binary, 'Fake process fixture; never executed.');
    writeFileSync(scopePath, JSON.stringify(scope));
    writeFileSync(reportPath, JSON.stringify(createReportTemplate(scope)));
    return run({ workspace, reportPath, scopePath, binary });
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function withSourceFixture(baseContent, headContent, run) {
  return withFixture(({ workspace }) => {
    const git = (args, input) => execFileSync('git', ['--no-pager', ...args],
      { cwd: workspace, input, encoding: 'utf8', timeout: 30_000 });
    git(['init', '--quiet']);
    const tree = content => {
      const blob = git(['hash-object', '-w', '--stdin'], content).trim();
      git(['update-index', '--add', '--cacheinfo', `100644,${blob},${PATH}`]);
      return git(['write-tree']).trim();
    };
    const baseSha = tree(baseContent);
    const headSha = tree(headContent);
    const current = buildScope(baseSha, headSha, 17, 'same-repo', `M\0${PATH}\0`, baseSha, 'repair');
    return run(current, workspace);
  });
}

function sourceFixtureEvents(current, diff, ranges) {
  const events = rangeEvents(ranges.map(([revision, start, end]) => [revision, start, end]), diff);
  const starts = new Map();
  for (const event of events) {
    if (event.type === 'tool.execution_start') starts.set(event.data.toolCallId, event.data);
    if (event.type === 'tool.execution_complete') {
      const start = starts.get(event.data.toolCallId);
      const data = JSON.parse(event.data.result.content);
      if (start.toolName === 'mcpscripts-read_security_diff') {
        data.baseSha = current.baseSha;
        data.headSha = current.headSha;
      } else if (start.toolName === 'mcpscripts-read_security_source') {
        data.source = ranges.find(([revision, first, last]) => revision === start.arguments.revision &&
          first === start.arguments.start_line && last === start.arguments.end_line)[3];
      } else if (start.toolName === 'mcpscripts-inspect_security_repair') data.headSha = current.headSha;
      event.data.result.content = JSON.stringify(data);
    }
    if (event.type === 'assistant.message') {
      const response = JSON.parse(event.data.content);
      response.headSha = current.headSha;
      event.data.content = JSON.stringify(response);
    }
  }
  return events;
}

for (const [name, headContent, findingLine, ranges] of [
  ['newline phantom EOF', 'new\n', 2, [['base', 2, 2, '2: '], ['head', 2, 2, '2: ']]],
  ['deleted empty head', '', 1, [['base', 2, 2, '2: '], ['head', 1, 1, '1: ']]],
]) {
  test(`real native helper rejects ${name} finding coverage`, () => {
    withSourceFixture('old\n', headContent, (current, workspace) => {
      const diff = readSecurityDiff(current, [], workspace);
      const events = sourceFixtureEvents(current, diff, ranges);
      const findings = [{ ...candidate().findings[0], startLine: findingLine, endLine: findingLine }];
      assert.throws(() => validateTranscript(output(events), current,
        { ...inspection, headSha: current.headSha }, diff, [PATH],
        paths => readSecurityDiff(current, paths, workspace), findings,
        (revision, path, start, end) => readSecuritySource(current, revision, path, start, end, workspace)),
      /source.*coverage/);
    });
  });
}

test('real native helper preserves blank lines, line endings, EOF clamping, and range limits', () => {
  for (const [content, expected, count] of [
    ['new\n', '1: new', 1],
    ['new', '1: new', 1],
    ['new\r\n', '1: new\r', 1],
    ['new\r\n\r\n', '1: new\r\n2: \r', 2],
    ['\n', '1: ', 1],
    ['\n\n', '1: \n2: ', 2],
    ['new\n\n', '1: new\n2: ', 2],
    ['new\n\nlast', '1: new\n2: \n3: last', 3],
    ['', null, 0],
  ]) {
    withSourceFixture(content, content, (current, workspace) => {
      for (const revision of ['base', 'head']) {
        const read = (start, end) => readSecuritySource(current, revision, PATH, start, end, workspace);
        if (count === 0) {
          assert.throws(() => read(1, 120), /beyond.*EOF/);
        } else {
          assert.equal(read(1, 120), expected);
          assert.equal(read(1, 800), expected);
          assert.equal(read(count, count), expected.split('\n').at(-1));
          assert.throws(() => read(count, 801 + count), /at most 800/);
        }
        assert.throws(() => read(count + 1, count + 1), /beyond.*EOF/);
        assert.throws(() => read(1, 801), /at most 800/);
      }
    });
  }
});

test('real native driver credits only actual EOF-clamped rows and preserves blank-line findings', () => {
  for (const [baseContent, headContent, line] of [['old\n', 'new\n', 1], ['old\n\n', 'new\n\n', 2]]) {
    withSourceFixture(baseContent, headContent, (current, workspace) => {
      const diff = readSecurityDiff(current, [], workspace);
      const ranges = ['base', 'head'].map(revision => [
        revision, 1, 120, readSecuritySource(current, revision, PATH, 1, 120, workspace),
      ]);
      const findings = [{ ...candidate().findings[0], startLine: line, endLine: line }];
      const validate = (events, findings) => validateTranscript(output(events), current,
        { ...inspection, headSha: current.headSha }, diff, [PATH],
        paths => readSecurityDiff(current, paths, workspace), findings,
        (revision, path, start, end) => readSecuritySource(current, revision, path, start, end, workspace));
      assert.equal(validate(sourceFixtureEvents(current, diff, ranges), findings).status, 'SOURCE_PASS');
      assert.throws(() => validate(sourceFixtureEvents(current, diff, ranges),
        [{ ...findings[0], startLine: line + 1, endLine: line + 1 }]), /source.*coverage/);
    });
  }
});

function driverOptions(fixture, events = reviewerEvents(), hooks = {}) {
  let invocation = 0;
  return {
    ...fixture, argv: ['--agent', 'ghaw-pr-security', '--allow-tool', 'mcpscripts',
      '--allow-tool', 'safeoutputs', '--prompt-file', 'trusted-compiler-prompt.txt'],
    inspect: () => inspection, readDiff: () => DIFF,
    readSource: (scope, revision, path, start, end) => sourceForRange(revision, path, start, end),
    emitPrimary: () => {}, logger: () => {},
    runChild(binary, args, options) {
      assert.equal(binary, fixture.binary);
      assert.equal(options.shell, false);
      assert.equal(options.cwd, fixture.workspace);
      assert.equal(options.timeout, 540_000);
      invocation++;
      if (invocation === 1) {
        assert.equal(args[args.lastIndexOf('--agent') + 1], 'ghaw-pr-security');
        const accepted = submitSecurityReport(JSON.stringify(hooks.candidate?.() ?? candidate()), scope, fixture.reportPath);
        return { status: 0, stdout: output([
          ...call('mcpscripts-submit_security_report', accepted),
          ...call('safeoutputs-noop', { accepted: true }), terminal(),
        ]) };
      }
      assert.equal(invocation, 2, 'Exactly two sequential fixed process invocations');
      assert.equal(args[args.lastIndexOf('--agent') + 1], 'ghaw-pr-security-reviewer');
      assert(!args.includes('--prompt-file'));
      assert(args.includes('mcpscripts(write_security_repair)'));
      assert(args.includes('mcpscripts(submit_security_report)'));
      hooks.reviewer?.();
      return hooks.result ?? { status: 0, stdout: output(events) };
    },
  };
}

test('pending candidates are accepted only at native repair submission, never proposal/final', () => {
  const report = candidate();
  assert.equal(validateCandidate(report, scope).review.status, 'pending');
  assert.throws(() => validateProposal(report, scope), /independent review/);
  assert.throws(() => validateReport(report, scope), /independent review/);
  const approved = { ...report, review: { ...report.review, status: 'source-pass', headSha: HEAD, patchSha256: DIGEST } };
  assert.equal(validateProposal(approved, scope).review.status, 'source-pass');
  assert.throws(() => validateCandidate(approved, scope), /independent review/);
  withFixture(({ reportPath }) => {
    submitSecurityReport(JSON.stringify(report), scope, reportPath);
    assert.throws(() => submitSecurityReport(JSON.stringify(approved), scope, reportPath), /cannot claim trusted/);
    assert.equal(JSON.parse(readFileSync(reportPath)).review.status, 'pending');
  });
});

test('candidate validation still rejects claimed final validation, weak fixes, and pending without a patch', () => {
  const report = candidate();
  report.checks.push({ name: 'wta-tests', status: 'pass', headSha: HEAD, evidence: 'local command: claimed tests' });
  assert.throws(() => validateCandidate(report, scope), /cannot claim/);
  const weak = candidate();
  weak.findings[0].confidence = 'medium';
  assert.throws(() => validateCandidate(weak, scope), /high confidence/);
  const empty = candidate();
  empty.patch = [];
  empty.findings = [];
  assert.throws(() => validateCandidate(empty, scope), /pending independent review/);
});

test('compiler arguments retain transport and approvals but both fixed phases exclude delegation and execution', () => {
  const argv = ['--agent=arbitrary', '--available-tools', '*', '--excluded-tools', 'view',
    '--allow-tool', 'mcpscripts', '--prompt-file', 'trusted.txt', '--output-format=text'];
  for (const phase of ['primary', 'reviewer']) {
    const args = buildPhaseArguments(argv, phase, 'fixed reviewer prompt');
    assert(!args.includes('*'));
    assert(!args.includes('--agent=arbitrary'));
    for (const name of ['task', 'read_agent', 'write_agent', 'list_agents']) assert(args.includes(name));
    assert(args.includes('shell') && args.includes('write'));
    assert(args.includes('mcpscripts-read_security_diff'));
    assert(args.includes('mcpscripts-read_security_source'));
    assert(args.includes('mcpscripts-inspect_security_repair'));
    const tools = args.slice(args.indexOf('--available-tools') + 1, args.indexOf('--deny-tool'));
    assert.equal(tools.includes('mcpscripts-write_security_repair'), phase === 'primary');
    assert.equal(tools.includes('mcpscripts-submit_security_report'), phase === 'primary');
    assert.equal(tools.includes('safeoutputs-noop'), phase === 'primary');
  }
  assert.throws(() => buildPhaseArguments(['--resume', 'any-session'], 'primary'), /cannot resume/);
});

test('deterministic fake children prove sequential fixed routing, exact native evidence, and proposal stamping', () => {
  withFixture(fixture => {
    const result = runSecurityReviewDriver(driverOptions(fixture));
    assert.equal(result.reviewed, true);
    assert.equal(result.patchSha256, DIGEST);
    const report = JSON.parse(readFileSync(fixture.reportPath));
    assert.equal(validateProposal(report, scope).review.status, 'source-pass');
    assert.equal(report.findings[0].fixDisposition.state, 'proposed');
    assert(!report.checks.some(check => check.name === 'wta-tests' && check.status === 'pass'));
  });
});

test('fixed transition diagnostics precede each bounded child without exposing model content', () => {
  withFixture(fixture => {
    const options = driverOptions(fixture);
    const observed = [];
    const runChild = options.runChild;
    options.logger = message => observed.push(message);
    options.runChild = (...args) => {
      assert.equal(args[2].timeout, 540_000);
      observed.push('child-start');
      return runChild(...args);
    };
    assert.equal(runSecurityReviewDriver(options).reviewed, true);
    assert.deepEqual(observed, [
      '[security-review-driver] starting primary', 'child-start',
      '[security-review-driver] starting fixed independent reviewer', 'child-start',
    ]);
  });
  withFixture(fixture => {
    const options = driverOptions(fixture);
    options.timeout = 540_001;
    options.runChild = () => assert.fail('Over-bound timeout must reject before launch');
    assert.throws(() => runSecurityReviewDriver(options), /phase timeout exceeds trusted bound/);
  });
});

for (const tool of ['task', 'bash', 'powershell', 'mcpscripts-write_security_repair',
  'mcpscripts-submit_security_report', 'safeoutputs-noop']) {
  test(`reviewer rejects ${tool} even if a child claims it succeeded`, () => {
    const events = reviewerEvents();
    events.splice(0, 0, ...call(tool, { accepted: true }));
    assert.throws(() => validateReviewerTranscript(output(events), scope, inspection, DIFF, [PATH]), /unknown/);
  });
}

for (const missing of ['mcpscripts-read_security_diff', 'mcpscripts-read_security_source', 'mcpscripts-inspect_security_repair']) {
  test(`reviewer requires successful native ${missing} evidence`, () => {
    const events = reviewerEvents().filter(event =>
      !(event.type === 'tool.execution_start' && event.data.toolName === missing) &&
      !(event.type === 'tool.execution_complete' &&
        [missing, ...(missing === 'mcpscripts-read_security_source' ? ['head-source'] : [])].includes(event.data.toolCallId)));
    // The head read has its own correlation ID.
    const cleaned = missing === 'mcpscripts-read_security_source'
      ? events.filter(event => event.data?.toolCallId !== 'head-source') : events;
    assert.throws(() => validateReviewerTranscript(output(cleaned), scope, inspection, DIFF, [PATH]));
  });
}

test('JSONL rejects malformed, truncated, duplicate, failed-terminal, and uncorrelated evidence', () => {
  assert.throws(() => parseTranscript('SOURCE_PASS', []), /malformed/);
  assert.throws(() => parseTranscript(output(reviewerEvents().slice(0, -1)), []));
  assert.throws(() => parseTranscript(output([{ ...terminal(), exitCode: 1 }]), []), /terminal/);
  assert.throws(() => parseTranscript(output([...call('view', {}), ...call('view', {}), terminal()]), ['view']), /replayed/);
  assert.throws(() => parseTranscript(output([{ type: 'tool.execution_complete', data: { toolCallId: 'missing', success: true } }, terminal()]), []), /uncorrelated/);
  assert.throws(() => parseTranscript('x'.repeat(16 * 1024 * 1024 + 1), []), /output limit/);
});

test('native patch/diff mismatches and prose-only or wrong-identity SOURCE_PASS cannot pass', () => {
  for (const mutate of [
    events => { events[1].data.result.content = JSON.stringify({ baseSha: BASE, headSha: HEAD, diff: 'Summary only' }); },
    events => { events[7].data.result.content = JSON.stringify({ ...inspection, patch: 'Copied summary' }); },
    events => { events[3].data.result.content = 'Output too large to read at once. Saved to: caller-path'; },
    events => { events[2].data.arguments.end_line = 800; },
    events => { events[8].data.content = 'SOURCE_PASS'; },
    events => { events[8].data.content = `\`\`\`json\n${events[8].data.content}\n\`\`\``; },
    events => { const response = JSON.parse(events[8].data.content); response.headSha = BASE; events[8].data.content = JSON.stringify(response); },
    events => { const response = JSON.parse(events[8].data.content); response.status = 'FAIL'; events[8].data.content = JSON.stringify(response); },
    events => { events[1].data.success = false; },
  ]) {
    const events = reviewerEvents();
    mutate(events);
    assert.throws(() => validateReviewerTranscript(output(events), scope, inspection, DIFF, [PATH]));
  }
});

test('bounded native diff groups must cover every immutable changed file, not just the repaired path', () => {
  const other = 'tools/wta/src/other.rs';
  const current = buildScope(BASE, HEAD, 17, 'same-repo', `M\0${PATH}\0M\0${other}\0`, BASE, 'repair');
  const events = reviewerEvents();
  events[0].data.arguments.paths_json = JSON.stringify([PATH]);
  const expected = paths => paths.includes(other) ? 'Second complete native diff.' : DIFF;
  assert.throws(() => validateReviewerTranscript(output(events), current, inspection, 'Full two-file diff.',
    [PATH], expected), /complete immutable/);
  const second = call('mcpscripts-read_security_diff', { baseSha: BASE, headSha: HEAD, diff: expected([other]) },
    { paths_json: JSON.stringify([other]) }).map(event => ({
    ...event, data: { ...event.data, toolCallId: 'other-diff' },
  }));
  events.splice(2, 0, ...second);
  assert.equal(validateReviewerTranscript(output(events), current, inspection, 'Full two-file diff.',
    [PATH], expected).status, 'SOURCE_PASS');
});

test('unknown native server identity and hidden subsidiary-agent events cannot forge provenance', () => {
  const events = reviewerEvents();
  events[0].data.mcpServerName = 'arbitrary';
  assert.throws(() => validateReviewerTranscript(output(events), scope, inspection, DIFF, [PATH]), /identity mismatch/);
  const hidden = reviewerEvents();
  hidden.splice(0, 0, { type: 'subagent.started', data: {} });
  assert.throws(() => validateReviewerTranscript(output(hidden), scope, inspection, DIFF, [PATH]), /subsidiary/);
  const commentary = reviewerEvents();
  commentary[8].data.phase = 'commentary';
  assert.throws(() => validateReviewerTranscript(output(commentary), scope, inspection, DIFF, [PATH]), /strict JSON final/);
});

test('reviewer final root JSON supports omitted optional phase and toolRequests independently of provider', () => {
  for (const fields of [['phase'], ['toolRequests'], ['phase', 'toolRequests']]) {
    const events = reviewerEvents();
    for (const field of fields) delete events[8].data[field];
    assert.equal(validateReviewerTranscript(output(events), scope, inspection, DIFF, [PATH]).status, 'SOURCE_PASS');
  }
});

test('nonfinal, nonroot, contradictory, and superseded reviewer messages cannot authorize approval', () => {
  for (const mutate of [
    events => { events[8].data.phase = 'commentary'; },
    events => { events[8].data.toolRequests = [{ name: 'view', arguments: {} }]; },
    events => { events[8].data.toolRequests = null; },
    events => { events[8].data.parentToolCallId = 'subsidiary-agent-call'; },
    events => { events[8].parentToolCallId = 'subsidiary-agent-call'; },
    events => { events[8].sessionId = 'other-session'; },
    events => { events[8].data.sessionId = 'other-session'; },
    events => { events.splice(9, 0, ...call('view', { content: 'Later activity.' })); },
    events => { events.splice(9, 0, { type: 'assistant.message', data: {
      content: 'Later commentary.', phase: 'commentary',
    } }); },
    events => { events.splice(9, 0, { type: 'assistant.message', data: {
      content: '', toolRequests: [{ name: 'view' }],
    } }); },
  ]) {
    const events = reviewerEvents();
    mutate(events);
    assert.throws(() => validateReviewerTranscript(output(events), scope, inspection, DIFF, [PATH]));
  }
});

test('native reviewer evidence accepts exactly 500 characters and rejects 501 before stamping', () => {
  for (const length of [500, 501]) {
    const events = reviewerEvents();
    const response = JSON.parse(events[8].data.content);
    response.evidence = 'x'.repeat(length);
    events[8].data.content = JSON.stringify(response);
    if (length === 500) {
      assert.equal(validateReviewerTranscript(output(events), scope, inspection, DIFF, [PATH]).evidence.length, 500);
      withFixture(fixture => {
        assert.equal(runSecurityReviewDriver(driverOptions(fixture, events)).reviewed, true);
        assert.equal(JSON.parse(readFileSync(fixture.reportPath)).review.evidence.length, 500);
      });
    } else {
      assert.throws(() => validateReviewerTranscript(output(events), scope, inspection, DIFF, [PATH]),
        /at most 500 characters/);
      withFixture(fixture => {
        const options = driverOptions(fixture, events);
        let pendingBytes;
        const runChild = options.runChild;
        options.runChild = (...args) => {
          if (args[1][args[1].lastIndexOf('--agent') + 1] === 'ghaw-pr-security-reviewer') {
            pendingBytes = readFileSync(fixture.reportPath, 'utf8');
            assert(args[1].at(-1).includes('evidence at most 400 characters'));
          }
          return runChild(...args);
        };
        assert.throws(() => runSecurityReviewDriver(options), /at most 500 characters/);
        assert.equal(readFileSync(fixture.reportPath, 'utf8'), pendingBytes);
        assert.equal(JSON.parse(pendingBytes).review.status, 'pending');
      });
    }
  }
});

test('reviewer timeout, process failure, rejected evidence, and report/patch races leave pending artifacts', () => {
  for (const hooks of [
    { result: { status: null, signal: 'SIGKILL', error: new Error('bounded timeout'), stdout: '' } },
    { result: { status: 1, stdout: '' } },
    { result: { status: 0, stdout: output([terminal()]) } },
  ]) {
    withFixture(fixture => {
      assert.throws(() => runSecurityReviewDriver(driverOptions(fixture, reviewerEvents(), hooks)));
      assert.equal(JSON.parse(readFileSync(fixture.reportPath)).review.status, 'pending');
    });
  }
  withFixture(fixture => {
    const options = driverOptions(fixture, reviewerEvents(), {
      reviewer: () => writeFileSync(fixture.reportPath, JSON.stringify({ ...candidate(), summary: 'Changed report.' })),
    });
    assert.throws(() => runSecurityReviewDriver(options), /changed during/);
    assert.equal(JSON.parse(readFileSync(fixture.reportPath)).review.status, 'pending');
  });
  withFixture(fixture => {
    let count = 0;
    const options = driverOptions(fixture);
    options.inspect = () => count++ ? { ...inspection, patch: `${PATCH}changed` } : inspection;
    assert.throws(() => runSecurityReviewDriver(options), /changed during/);
    assert.equal(JSON.parse(readFileSync(fixture.reportPath)).review.status, 'pending');
  });
});

test('unpatched report runs only primary and never requires or invents independent approval', () => {
  withFixture(fixture => {
    const options = driverOptions(fixture, [], { candidate: () => ({
      ...createReportTemplate(scope), summary: 'No safe repair is proposed.',
    }) });
    options.inspect = () => ({ headSha: HEAD, patch: '', patchSha256: createHash('sha256').update('').digest('hex'), paths: [] });
    assert.equal(runSecurityReviewDriver(options).reviewed, false);
    assert.equal(JSON.parse(readFileSync(fixture.reportPath)).review.status, 'not-required');
  });
});

test('candidate-owned scope and mismatched repair paths are rejected before approval', () => {
  withFixture(fixture => {
    const scopePath = join(fixture.workspace, 'scope.json');
    writeFileSync(scopePath, JSON.stringify(scope));
    assert.throws(() => runSecurityReviewDriver({ ...driverOptions(fixture), scopePath }), /outside/);
  });

  withFixture(fixture => {
    const options = driverOptions(fixture);
    options.inspect = () => ({ ...inspection, paths: ['tools/wta/src/unreported.rs'] });
    assert.throws(() => runSecurityReviewDriver(options), /do not match/);
    assert.equal(JSON.parse(readFileSync(fixture.reportPath)).review.status, 'pending');
  });
});

test('failed primary and forged approval stop before any reviewer process starts', () => {
  withFixture(fixture => {
    let calls = 0;
    const options = driverOptions(fixture);
    options.runChild = () => {
      calls++;
      return { status: 1, stdout: '' };
    };
    assert.throws(() => runSecurityReviewDriver(options), /primary CLI failed/);
    assert.equal(calls, 1);
  });
  withFixture(fixture => {
    const options = driverOptions(fixture, reviewerEvents(), { candidate: () => {
      const report = candidate();
      report.review = { ...report.review, status: 'source-pass', headSha: HEAD, patchSha256: DIGEST };
      return report;
    } });
    const original = readFileSync(fixture.reportPath, 'utf8');
    assert.throws(() => runSecurityReviewDriver(options), /cannot claim trusted/);
    assert.equal(readFileSync(fixture.reportPath, 'utf8'), original);
  });
});

test('twenty compact medium findings expand above 16 KiB without losing unchanged driver guidance', () => {
  withFixture(fixture => {
    const report = {
      ...createReportTemplate(scope), summary: 'Medium advice only.',
      findings: Array.from({ length: 20 }, (_, index) => ({
        rule: 'medium-advice', severity: 'medium', confidence: 'medium', category: 'session-routing',
        file: PATH, startLine: index + 1, endLine: index + 1,
        observed: 'x', expected: 'x', impact: 'x',
        evidence: Array.from({ length: 3 }, (_, trace) => ({
          kind: 'source-trace', reference: String(trace + 1), detail: 'x',
        })),
        proposedFix: 'x', validation: 'x', fixDisposition: { state: 'advice-only', reason: 'x' },
      })),
    };
    const compact = JSON.stringify(report);
    assert(Buffer.byteLength(compact) < 10 * 1024);
    const expectedBytes = serializeSecurityReport(validateCandidate(report, scope));
    assert(Buffer.byteLength(expectedBytes) > 16 * 1024);
    assert(Buffer.byteLength(expectedBytes) < SECURITY_REPORT_MAX_BYTES);
    const options = driverOptions(fixture, [], { candidate: () => report });
    options.inspect = () => ({
      headSha: HEAD, patch: '', patchSha256: createHash('sha256').update('').digest('hex'), paths: [],
    });
    let emitted = false;
    options.emitPrimary = () => { emitted = true; };
    assert.equal(runSecurityReviewDriver(options).reviewed, false);
    assert.equal(emitted, true);
    assert.equal(readFileSync(fixture.reportPath, 'utf8'), expectedBytes);
    assert.equal(JSON.parse(expectedBytes).findings.length, 20);
  });
});

test('oversized serialized expansion rejects atomically before native destination truncation', () => {
  withFixture(({ reportPath }) => {
    let nested = [];
    for (let depth = 0; depth < 250; depth++) nested = [nested];
    const report = { ...createReportTemplate(scope), summary: 'No repair proposed.', extra: nested };
    const compact = JSON.stringify(report);
    assert(Buffer.byteLength(compact) < 10 * 1024);
    const normalized = validateCandidate(report, scope);
    assert(Buffer.byteLength(`${JSON.stringify(normalized, null, 2)}\n`) > SECURITY_REPORT_MAX_BYTES);
    const original = readFileSync(reportPath, 'utf8');
    assert.throws(() => serializeSecurityReport(normalized), /64 KiB native output limit/);
    assert.throws(() => submitSecurityReport(compact, scope, reportPath), /64 KiB native output limit/);
    assert.equal(readFileSync(reportPath, 'utf8'), original);
    assert.throws(() => submitSecurityReport('{malformed', scope, reportPath));
    assert.equal(readFileSync(reportPath, 'utf8'), original);
  });
});

test('driver rejects physical report files beyond the shared serialized bound without modification', () => {
  withFixture(fixture => {
    const oversized = 'x'.repeat(SECURITY_REPORT_MAX_BYTES + 1);
    const options = driverOptions(fixture);
    options.runChild = () => {
      writeFileSync(fixture.reportPath, oversized);
      return { status: 0, stdout: output([
        ...call('mcpscripts-submit_security_report', { accepted: true }),
        ...call('safeoutputs-noop', { accepted: true }), terminal(),
      ]) };
    };
    assert.throws(() => runSecurityReviewDriver(options), /native size bound/);
    assert.equal(readFileSync(fixture.reportPath, 'utf8'), oversized);
  });
});
