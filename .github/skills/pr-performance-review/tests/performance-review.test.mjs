import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import { execFileSync, spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

import {
    classifyPullRequest,
    gatePublication,
    renderReport,
    validateReport,
    verifyPullRequest,
    validateVerdict,
    prepareScope,
    validateProposal,
    reconstructTree,
    sealProposal,
    createAnalysisPlan,
    analysisComparisonComplete,
} from '../scripts/performance-review.mjs';

const identity = {
    prNumber: 42,
    baseSha: 'a'.repeat(40),
    headSha: 'b'.repeat(40),
    mode: 'guide',
};

test('native plan selects provisional projects and never silently drops shared headers or HLSL', () => {
    const scope = classifyPullRequest([
        { filename: 'src/renderer/atlas/AtlasEngine.cpp' },
        { filename: 'src/renderer/atlas/AtlasEngine.h' },
        { filename: 'src/buffer/out/textBuffer.cpp' },
        { filename: 'src/terminal/parser/stateMachine.cpp' },
        { filename: 'src/cascadia/TerminalCore/Terminal.cpp' },
        { filename: 'src/inc/til.h' },
        { filename: 'src/renderer/atlas/shader_ps.hlsl' },
        { filename: 'tools/wta/build.rs' },
    ], identity);
    const plan = scope.analysisPlan;
    assert.equal(scope.applicable, true);
    assert.equal(plan.rust.required, true);
    assert.equal(plan.coverage, 'partial');
    assert.deepEqual(plan.cpp.projects, [
        'src/buffer/out/lib/bufferout.vcxproj', 'src/cascadia/TerminalCore/lib/terminalcore-lib.vcxproj',
        'src/renderer/atlas/atlas.vcxproj', 'src/terminal/parser/lib/parser.vcxproj',
    ]);
    for (const project of plan.cpp.projects) assert.ok(fs.existsSync(project));
    assert.ok(plan.cpp.candidatePaths.some(candidate => candidate.path === 'src/terminal/parser/stateMachine.cpp'));
    assert.equal(Object.hasOwn(plan.cpp, 'coveredPaths'), false, 'prefix selection cannot establish actual translation-unit coverage');
    assert.deepEqual(plan.manualScope.map(entry => entry.path), [
        'src/renderer/atlas/AtlasEngine.h', 'src/inc/til.h', 'src/renderer/atlas/shader_ps.hlsl',
    ]);
    assert.equal(classifyPullRequest([{ filename: 'src/til/collections.h' }], identity).applicable, true);
    assert.deepEqual(createAnalysisPlan([{ filename: 'doc/example.md' }]).cpp.projects, []);
});

test('Rust plan includes manifests and supporting Rust test changes without changed-line linting', () => {
    for (const filename of ['tools/wta/src/master/mod.rs', 'tools/wta/src/master/tests.rs',
        'tools/wta/Cargo.toml', 'tools/wta/Cargo.lock', 'tools/wta/build.rs']) {
        const plan = createAnalysisPlan([{ filename }]);
        assert.equal(plan.rust.required, true, filename);
        assert.equal(plan.rust.alias, 'wta-perf-extended');
        assert.equal(plan.rust.toolchain, '1.93.0');
        assert.match(plan.rust.scope, /Entire WTA/);
    }
});

test('analysis comparison binds identity, revisions, same trusted plan and tool versions; partial is never complete', t => {
    const directory = fs.mkdtempSync(path.join(process.cwd(), '.performance-analysis-metadata-'));
    t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
    const plan = createAnalysisPlan([{ filename: 'tools/wta/src/main.rs' }]);
    const records = ['BASE', 'HEAD'].map(revision => ({
        version: 2, identity, revision, analyzedSha: identity[revision === 'BASE' ? 'baseSha' : 'headSha'],
        authoringSha: 'c'.repeat(40), plan, tools: { clippy: '1.93.0', cargoConfigurationSha256: 'fixture' }, status: 'completed',
        checks: [{ name: 'rust-analysis', status: 'completed', exitCode: 0 }],
        analyzedScope: { wtaRustCrate: true, cppProjects: [] }, missingPrerequisites: [], analyzedCppTranslationUnits: [], manualScope: [],
    }));
    const write = () => records.forEach(record => {
        const root = path.join(directory, `performance-analysis-${record.revision}`);
        fs.mkdirSync(root, { recursive: true });
        fs.writeFileSync(path.join(root, 'analysis-metadata.json'), JSON.stringify(record));
    });

    write();
    assert.equal(analysisComparisonComplete(directory, identity), true);
    records[1].checks = []; write();
    assert.equal(analysisComparisonComplete(directory, identity), false, 'source scope without actual executed commands is incomplete');
    records[1].checks = records[0].checks; write();
    assert.equal(analysisComparisonComplete(directory, { ...identity, analysisPlan: createAnalysisPlan([]) }), false);
    for (const status of ['partial', 'incomplete', 'failed']) {
        records[1].status = status; write();
        assert.equal(analysisComparisonComplete(directory, identity), false);
    }
    records[1].status = 'completed';
    records[1].tools = { clippy: '1.96.0' }; write();
    assert.equal(analysisComparisonComplete(directory, identity), false);
    records[1].tools = records[0].tools;
    records[1].analyzedSha = identity.baseSha; write();
    assert.equal(analysisComparisonComplete(directory, identity), false);
    records[1].identity = { ...identity, headSha: 'd'.repeat(40) }; write();
    assert.throws(() => analysisComparisonComplete(directory, identity), /immutable workflow input/);
});

test('completed Rust-only crate analysis accepts expected empty C++ units and rejects failed Rust or old metadata', t => {
    const directory = fs.mkdtempSync(path.join(process.cwd(), '.performance-rust-only-language-scope-'));
    t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
    const plan = createAnalysisPlan([{ filename: 'tools/wta/src/lib.rs' }]);
    assert.equal(plan.cpp.required, false);
    assert.deepEqual(plan.cpp.projects, []);
    assert.deepEqual(plan.cpp.candidatePaths, []);
    assert.equal(plan.cpp.profile, 'Extended');
    const records = ['BASE', 'HEAD'].map(revision => ({
        version: 2, identity, revision, analyzedSha: identity[revision === 'BASE' ? 'baseSha' : 'headSha'],
        authoringSha: 'c'.repeat(40), status: 'completed', plan, tools: { clippy: '1.93.0', trustedConfiguration: 'same' },
        checks: [{ name: 'rust-analysis', status: 'completed', exitCode: 0 }],
        analyzedScope: { wtaRustCrate: true, cppProjects: [] }, analyzedCppTranslationUnits: [],
        missingPrerequisites: [], manualScope: [],
    }));
    const write = () => records.forEach(record => {
        const root = path.join(directory, `performance-analysis-${record.revision}`);
        fs.mkdirSync(root, { recursive: true });
        fs.writeFileSync(path.join(root, 'analysis-metadata.json'), JSON.stringify(record));
    });
    write();
    const expected = { ...identity, analysisPlan: plan };
    assert.equal(analysisComparisonComplete(directory, expected), true, 'Rust crate scope is not a C++ file list');
    for (const record of records) {
        record.analyzedScope.wtaRustCrate = false; write();
        assert.equal(analysisComparisonComplete(directory, expected), false);
        record.analyzedScope.wtaRustCrate = true;
        record.checks[0] = { name: 'rust-analysis', status: 'failed', exitCode: 101 }; write();
        assert.equal(analysisComparisonComplete(directory, expected), false);
        record.checks[0] = { name: 'rust-analysis', status: 'completed', exitCode: 0 };
        record.version = 1; write();
        assert.equal(analysisComparisonComplete(directory, expected), false, 'version 1 language-ambiguous metadata is not silently upgraded');
        record.version = 2;
    }
    write();
    assert.equal(analysisComparisonComplete(directory, expected), true);
    const prompt = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance.md', import.meta.url), 'utf8');
    assert.match(prompt, /analyzedScope\.wtaRustCrate: true/);
    assert.match(prompt, /analyzedCppTranslationUnits/);
    assert.match(prompt, /An empty C\+\+ list never blocks a Rust-only repair/);
    assert.match(prompt, /schema version 2/);
});

test('library, unit/feature sibling and unlisted C++ paths are only provisional native membership candidates', () => {
    const filenames = ['src/terminal/parser/stateMachine.cpp', 'src/terminal/parser/ut_parser/StateMachineTest.cpp',
        'src/terminal/parser/ft_fuzzer/main.cpp', 'src/terminal/parser/unlisted.cpp'];
    const plan = createAnalysisPlan(filenames.map(filename => ({ filename })));
    assert.deepEqual(plan.cpp.projects, ['src/terminal/parser/lib/parser.vcxproj']);
    assert.deepEqual(plan.cpp.candidatePaths.map(candidate => candidate.path), filenames);
    assert.equal(plan.coverage, 'unverified-projects');
    assert.equal(Object.hasOwn(plan.cpp, 'coveredPaths'), false);
});

test('successful C++ jobs without actual evaluated TU coverage cannot authorize a mixed WTA proposal', t => {
    const directory = fs.mkdtempSync(path.join(process.cwd(), '.performance-native-membership-'));
    t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
    const own = 'src/terminal/parser/stateMachine.cpp';
    const project = 'src/terminal/parser/lib/parser.vcxproj';
    const plan = createAnalysisPlan([{ filename: own }, { filename: 'tools/wta/src/main.rs' }]);
    const records = ['BASE', 'HEAD'].map(revision => ({
        version: 2, identity, revision, analyzedSha: identity[revision === 'BASE' ? 'baseSha' : 'headSha'],
        authoringSha: 'c'.repeat(40), plan, status: 'completed', tools: { native: 'fixture' },
        checks: ['rust-analysis', 'cpp-items-0', 'cpp-analysis-0'].map(name => ({ name, status: 'completed', exitCode: 0 })),
        analyzedScope: { wtaRustCrate: true, cppProjects: [project] }, missingPrerequisites: [], manualScope: [],
        analyzedCppTranslationUnits: [{ path: own, project, membership: 'MSBuild.ClCompile' }],
    }));
    const write = () => records.forEach(record => {
        const root = path.join(directory, `performance-analysis-${record.revision}`);
        fs.mkdirSync(root, { recursive: true });
        fs.writeFileSync(path.join(root, 'analysis-metadata.json'), JSON.stringify(record));
    });
    write();
    assert.equal(analysisComparisonComplete(directory, { ...identity, analysisPlan: plan }), true);
    records[1].analyzedCppTranslationUnits = []; write();
    assert.equal(analysisComparisonComplete(directory, identity), false, 'passing project checks cannot cover an unverified TU');
    records[1].analyzedCppTranslationUnits = records[0].analyzedCppTranslationUnits;
    records[1].checks = records[1].checks.filter(check => check.name !== 'cpp-items-0'); write();
    assert.equal(analysisComparisonComplete(directory, identity), false, 'native item evaluation must actually complete');
    records[1].checks = records[0].checks;
    for (const filename of ['src/terminal/parser/ut_parser/StateMachineTest.cpp',
        'src/terminal/parser/ft_fuzzer/main.cpp', 'src/terminal/parser/unlisted.cpp']) {
        const mixed = createAnalysisPlan([{ filename: own }, { filename }, { filename: 'tools/wta/src/main.rs' }]);
        records.forEach(record => { record.plan = mixed; }); write();
        assert.equal(analysisComparisonComplete(directory, { ...identity, analysisPlan: mixed }), false, filename);
    }
});

test('Windows analysis reuses trusted normal profiles instead of embedding a second rule list', () => {
    const runner = fs.readFileSync(new URL('../scripts/run-native-performance-checks.ps1', import.meta.url), 'utf8');
    assert.match(runner, /\/getProperty:ClangTidyChecks/);
    assert.match(runner, /\/getItem:ClCompile/);
    assert.match(runner, /\/p:PerformanceAnalysis=Extended/);
    assert.match(runner, /checks\.Replace\(',', '%2C'\)/);
    assert.match(runner, /\/t:Build;ClangTidy/);
    assert.match(runner, /Copy-Item -LiteralPath \$trustedConfig/);
    assert.match(runner, /@\('\+1\.93\.0', 'wta-perf-extended'\)/);
    assert.doesNotMatch(runner, /performance-inefficient-vector-operation|clippy::needless_collect|clippy::large_futures/);
    const config = fs.readFileSync(new URL('../../../../.cargo/config.toml', import.meta.url), 'utf8');
    assert.match(config, /wta-perf-extended = \["wta-perf", "-W", "clippy::needless_collect", "-W", "clippy::large_futures"\]/);
});

function finding(overrides = {}) {
    return {
        id: 'PERF-ABC12345',
        severity: 'medium',
        confidence: 'medium',
        dimension: 'responsiveness',
        category: 'ui-thread',
        title: 'Repeated synchronous enumeration',
        affectedScenario: 'Opening a tab with many panes',
        location: 'src/cascadia/TerminalApp/TerminalPage.cpp:100',
        observed: 'The changed path enumerates every pane on each event.',
        expected: 'The event should perform bounded work per affected pane.',
        impact: 'Large layouts may add UI-thread delay.',
        nativeEnvironment: { architecture: 'not-measured', details: 'Static review; Windows trace unavailable.' },
        evidence: [{ type: 'source', detail: 'Caller and loop inspected at immutable head.' }],
        proposedFix: 'Consider preserving the affected pane identity through dispatch.',
        validation: 'Run the pane-context end-to-end measurement on Windows x64.',
        fixDisposition: 'advice_only',
        ...overrides,
    };
}

function report(findings = [], checks = []) {
    return {
        version: 1,
        review: 'performance',
        mode: identity.mode,
        identity,
        status: findings.some(item => item.severity === 'high') ? 'action_required' :
            findings.length ? 'advisory' : checks.some(check => check.status === 'error') ? 'blocked' : 'pass',
        findings,
        checks,
    };
}

function proposal() {
    const value = report([finding({
        severity: 'high', confidence: 'high', fixDisposition: 'proposed', category: 'wta-runtime',
        location: 'tools/wta/src/master/mod.rs:1',
        evidence: [{ type: 'complexity-proof', detail: 'Repeated nested enumeration requires quadratic work.' }],
    })]);
    value.mode = 'repair';
    value.status = 'pending_validation';
    value.validationPlan = { type: 'wta-unit', testFilter: 'master::tests::preserves_state' };
    return {
        version: 1, identity, treeSha: 'c'.repeat(40), report: value,
        validationPlan: { ...value.validationPlan },
        files: [{ path: 'tools/wta/src/master/mod.rs', mode: '100644', contents: Buffer.from('fn repaired() {}\n').toString('base64') }],
    };
}

const helperPath = fileURLToPath(new URL('../scripts/performance-review.mjs', import.meta.url));

test('immutable root Cargo configuration changes require manual handoff, not new HEAD trust', () => {
    for (const name of ['config', 'config.toml']) {
        for (const change of ['addition', 'deletion', 'modification', 'mode', 'unchanged', 'same-content']) {
            withRepository(({ git, write, expected }) => {
                const filename = `.cargo/${name}`;
                const config = '[target.x86_64-pc-windows-msvc]\nrunner = "fake-runner.cmd"\n';
                const commitTree = parent => git(['commit-tree', git(['write-tree']), '-p', parent, '-m', 'configuration fixture']);
                git(['read-tree', expected.baseSha]);
                if (change !== 'addition') {
                    write(filename, config);
                    git(['add', '--', filename]);
                }
                const base = commitTree(expected.baseSha);
                git(['read-tree', expected.headSha]);
                if (change !== 'deletion') {
                    write(filename, change === 'modification' ? config + '[build]\nrustc-wrapper = "fake-wrapper.cmd"\n' : config);
                    git(['add', '--', filename]);
                    if (change === 'mode') git(['update-index', '--chmod=+x', '--', filename]);
                }
                const head = commitTree(base);
                const files = proposal().files;
                if (change === 'unchanged' || change === 'same-content') {
                    if (change === 'same-content') write(filename, '# mutable workspace is not authority\n');
                    assert.doesNotThrow(() => reconstructTree(files, head, base));
                } else {
                    assert.throws(() => reconstructTree(files, head, base), /original PR changes root Cargo configuration.*manual handoff/);
                }
            });
        }
    }
});

test('fresh immutable CI sealing and upfront native proposal validation reject tracked fake runner configuration', () => {
    for (const configPath of ['.cargo\\config.toml', '.cargo\\CONFIG', '.CARGO\\config', '.CaRgO\\CoNfIg.ToMl']) withRepository(({ root, git, write, expected, output }) => {
        write(configPath, '[target.x86_64-pc-windows-msvc]\nrunner = "fake-runner.cmd"\n');
        write('fake-runner.cmd', '@echo off\necho test result: ok. 1 passed; 0 failed;\n');
        git(['add', '--all']);
        const tree = git(['write-tree']);
        expected.headSha = git(['commit-tree', tree, '-p', expected.headSha, '-m', 'Tracked PR configuration']);
        git(['update-ref', 'HEAD', expected.headSha]);
        const baselinePath = path.join(output, 'baseline.json');
        prepareScope(expected, output, baselinePath);
        const scopePath = path.join(output, 'performance-scope.json');
        const value = proposal();
        value.identity = value.report.identity = expected;
        const proposalPath = path.join(output, 'proposal.json');
        fs.writeFileSync(proposalPath, JSON.stringify(value));
        const args = ['--pr', '42', '--base', expected.baseSha, '--head', expected.headSha];
        const validated = spawnSync(process.execPath, [helperPath, 'validate-proposal', '--input', proposalPath, ...args],
            { encoding: 'utf8' });
        assert.notEqual(validated.status, 0);
        assert.match(validated.stderr, /original PR changes root Cargo configuration.*manual handoff/);
        const trustedRoot = fs.mkdtempSync(path.join(path.dirname(root), 'performance-trusted-unit-'));
        execFileSync('git', ['clone', '--quiet', '--no-local', root, trustedRoot]);
        write('tools\\wta\\src\\master\\mod.rs', 'fn repaired() {}\n');
        const reportPath = path.join(output, 'report.json');
        const queuePath = path.join(output, 'queue.json');
        fs.writeFileSync(reportPath, JSON.stringify(value.report));
        fs.writeFileSync(queuePath, JSON.stringify({ items: nativeRequests(), errors: [] }));
        let sealed;
        try {
            sealed = spawnSync(process.execPath, [helperPath, 'gate', '--mode', 'repair', '--output-dir', output,
                '--baseline', baselinePath, '--scope', scopePath, '--report', reportPath, '--agent-output', queuePath,
                '--trusted-repository-root', trustedRoot, '--agent-worktree-root', root, ...args], { encoding: 'utf8' });
        } finally {
            fs.rmSync(trustedRoot, { recursive: true, force: true });
        }
        assert.notEqual(sealed.status, 0);
        assert.match(sealed.stderr, /original PR changes root Cargo configuration.*manual handoff/);
        assert.equal(fs.existsSync(path.join(output, 'performance-proposal.json')), false);
    });
});

test('Windows-equivalent root configuration additions block even with an inherited TOML configuration', () => {
    for (const filename of ['.cargo/CONFIG', '.CARGO/config', '.CaRgO/CoNfIg.ToMl']) {
        withRepository(({ git, write, expected }) => {
            const inheritedToml = !filename.toLowerCase().endsWith('.toml');
            const commitTree = parent => git(['commit-tree', git(['write-tree']), '-p', parent, '-m', 'Case-equivalent configuration fixture']);
            git(['read-tree', expected.baseSha]);
            if (inheritedToml) {
                write('.cargo\\config.toml', '[net]\noffline = true\n');
                git(['add', '--', '.cargo/config.toml']);
            }
            const base = commitTree(expected.baseSha);
            git(['read-tree', expected.headSha]);
            if (inheritedToml) git(['add', '--', '.cargo/config.toml']);
            write(filename, '[target.x86_64-pc-windows-msvc]\nrunner = "fake-runner.cmd"\n');
            const blob = git(['hash-object', '-w', '--', filename]);
            git(['update-index', '--add', '--cacheinfo', `100644,${blob},${filename}`]);
            const head = commitTree(base);
            assert.throws(() => reconstructTree(proposal().files, head, base), /original PR changes root Cargo configuration.*manual handoff/);
        });
    }
});

function withRepository(action) {
    const previous = process.cwd();
    const root = fs.mkdtempSync(path.join(previous, 'performance-unit-'));
    const git = args => execFileSync('git', ['--no-pager', ...args], { cwd: root, encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'] }).trim();
    const write = (filename, contents) => {
        const target = path.join(root, filename);
        fs.mkdirSync(path.dirname(target), { recursive: true });
        fs.writeFileSync(target, contents);
    };
    try {
        git(['init', '--quiet']);
        git(['config', 'user.name', 'Performance unit test']);
        git(['config', 'user.email', 'performance-unit@example.invalid']);
        git(['config', 'core.autocrlf', 'false']);
        write('tools\\wta\\src\\master\\mod.rs', 'fn base() {}\n');
        write('tools\\wta\\src\\unchanged.rs', 'fn unchanged() {}\n');
        write('doc\\policy.md', 'Reviewed policy.\n');
        git(['add', '--all']);
        git(['commit', '--quiet', '-m', 'base']);
        const baseSha = git(['rev-parse', 'HEAD']);
        write('tools\\wta\\src\\master\\mod.rs', 'fn head() {}\n');
        git(['add', '--all']);
        git(['commit', '--quiet', '-m', 'head']);
        const expected = { prNumber: 42, baseSha, headSha: git(['rev-parse', 'HEAD']), mode: 'repair' };
        process.chdir(root);
        return action({ root, git, write, expected, output: path.join(root, '.git', 'performance-artifacts') });
    } finally {
        process.chdir(previous);
        fs.rmSync(root, { recursive: true, force: true });
    }
}

test('selects repository hot-path source and keeps tests as supporting evidence', () => {
    const scope = classifyPullRequest([
        { filename: 'src/renderer/atlas/AtlasEngine.cpp', additions: 8, deletions: 2 },
        { filename: 'tools/wta/src/master/mod.rs', additions: 4, deletions: 1 },
        { filename: 'src/renderer/ut_renderer/AtlasTests.cpp', additions: 20, deletions: 0 },
    ], identity);
    assert.equal(scope.applicable, true);
    assert.deepEqual(scope.categories, ['rendering', 'wta-runtime']);
    assert.equal(scope.candidates.length, 2);
    assert.equal(scope.supporting.length, 1);
});

test('Rust test basenames are supporting while runtime files remain candidates', () => {
    const supporting = [
        'tools/wta/src/master/tests.rs',
        'tools/wta/src/master/app_tests.rs',
        'tools/wta/src/test_support.rs',
        'tools/wta/src/tests/fix.rs',
    ];
    const testOnly = classifyPullRequest(supporting.map(filename => ({ filename })), identity);
    assert.equal(testOnly.applicable, false);
    assert.deepEqual(testOnly.supporting.map(file => file.filename), supporting);
    assert.deepEqual(testOnly.candidates, []);
    const runtime = 'tools/wta/src/master/app.rs';
    const scope = classifyPullRequest([...supporting, runtime].map(filename => ({ filename })), identity);
    assert.equal(scope.applicable, true);
    assert.deepEqual(scope.candidates.map(file => file.filename), [runtime]);
    assert.deepEqual(scope.supporting.map(file => file.filename), supporting);
    // Classification is file-level: inline test blocks do not exclude runtime modules.
    assert.equal(classifyPullRequest([{ filename: 'tools/wta/src/master/mod.rs' }], identity).applicable, true);
    for (const filename of supporting) {
        const value = proposal();
        value.files[0].path = filename;
        assert.throws(() => validateProposal(value, { ...identity, mode: 'repair' }), /unique, regular WTA/);
    }
});

test('renderer HLSL alone is an applicable rendering candidate', () => {
    const filename = 'src/renderer/atlas/shader_ps.hlsl';
    const scope = classifyPullRequest([{ filename }], identity);
    assert.equal(scope.applicable, true);
    assert.deepEqual(scope.categories, ['rendering']);
    assert.deepEqual(scope.candidates.map(file => file.filename), [filename]);
});

test('only the exact WTA Cargo manifest and lockfile are review candidates', () => {
    for (const filename of ['tools/wta/Cargo.toml', 'tools/wta/Cargo.lock']) {
        const scope = classifyPullRequest([{ filename }], identity);
        assert.equal(scope.applicable, true);
        assert.deepEqual(scope.categories, ['wta-runtime']);
        assert.deepEqual(scope.candidates.map(file => file.filename), [filename]);
    }
    const scope = classifyPullRequest([
        'tools/other/Cargo.toml', 'tools/other/Cargo.lock',
        'tools/wta/src/Cargo.toml', 'tools/wta/src/Cargo.lock',
        'tools/wta/other.toml', 'tools/wta/other.lock',
    ].map(filename => ({ filename })), identity);
    assert.equal(scope.applicable, false);
    assert.equal(scope.excluded.length, 6);
});

test('the exact WTA build script is applicable for CI review but not automatic repair', () => {
    const filename = 'tools/wta/build.rs';
    const scope = classifyPullRequest([{ filename }], identity);
    assert.equal(scope.applicable, true);
    assert.deepEqual(scope.candidates.map(file => file.filename), [filename]);
    assert.ok(scope.dimensions.includes('ci-runtime-cost'));
    assert.equal(classifyPullRequest([{ filename: 'tools/other/build.rs' }], identity).applicable, false);
    const value = proposal();
    value.files[0].path = filename;
    value.report.findings[0].location = `${filename}:1`;
    assert.throws(() => validateProposal(value, { ...identity, mode: 'repair' }), /unique, regular WTA/);
    withRepository(({ git, write, expected, output }) => {
        write('tools\\wta\\build.rs', 'fn main() {}\n');
        git(['add', '--all']);
        git(['commit', '--quiet', '-m', 'build script fixture']);
        const prepared = prepareScope({ ...expected, baseSha: expected.headSha, headSha: git(['rev-parse', 'HEAD']) }, output);
        assert.equal(prepared.applicable, true);
        assert.deepEqual(prepared.candidates.map(file => file.filename), [filename]);
        assert.ok(prepared.dimensions.includes('ci-runtime-cost'));
    });
});

test('prepared input includes exact immutable Git file and line counts', () => {
    withRepository(({ expected, output }) => {
        const scope = prepareScope(expected, output);
        assert.deepEqual(scope.totals, { files: 1, candidates: 1, additions: 1, deletions: 1, binaryFiles: 0 });
        assert.equal(scope.candidates[0].additions, 1);
        assert.equal(scope.candidates[0].deletions, 1);
        assert.deepEqual(JSON.parse(fs.readFileSync(path.join(output, 'performance-scope.json'))).totals, scope.totals);
    });
});

test('prepared change-size input counts all files and does not invent binary line counts', () => {
    withRepository(({ git, write, expected, output }) => {
        write('doc\\extra.md', 'First line.\nSecond line.\n');
        write('image.bin', Buffer.from([0, 1, 2, 3]));
        git(['add', '--all']);
        git(['commit', '--quiet', '-m', 'size fixture']);
        const scope = prepareScope({ ...expected, headSha: git(['rev-parse', 'HEAD']) }, output);
        assert.deepEqual(scope.totals, { files: 3, candidates: 1, additions: 3, deletions: 1, binaryFiles: 1 });
        const binary = scope.excluded.find(file => file.filename === 'image.bin');
        assert.equal(binary.additions, null);
        assert.equal(binary.deletions, null);
    });
});

test('does not infer a hot path from a filename, documentation, benchmark, or test alone', () => {
    const scope = classifyPullRequest([
        { filename: 'doc/performance-plan.md' },
        { filename: 'test/e2e/PerformanceRegression.Tests.ps1' },
        { filename: 'src/tools/ConsoleBench/main.cpp' },
        { filename: 'README.md' },
    ], identity);
    assert.equal(scope.applicable, false);
    assert.equal(scope.candidates.length, 0);
});

test('requires measurement evidence to identify microbenchmark versus end-to-end', () => {
    const bad = report([finding({
        evidence: [{ type: 'measurement', detail: 'Median rose by 20 percent.', kind: 'timing' }],
    })]);
    assert.throws(() => validateReport(bad, identity), /must distinguish microbenchmark/);
});

test('rejects noisy measurements without samples and spread', () => {
    const bad = report([finding({
        evidence: [{ type: 'measurement', kind: 'end-to-end', noisy: true, detail: 'Results varied.' }],
    })]);
    assert.throws(() => validateReport(bad, identity), /at least three samples/);
});

test('source and proof evidence cannot acquire measurement-only metadata', () => {
    for (const type of ['source', 'complexity-proof', 'blocking-proof', 'resource-proof']) {
        for (const [key, value] of Object.entries({ kind: 'profile', noisy: false, samples: 3, spread: '9-10 ms' })) {
            const evidence = { type, detail: 'Immutable source proof.', [key]: value };
            assert.throws(() => validateReport(report([finding({ evidence: [evidence] })]), identity), /measurement-only/);
        }
    }
});

test('optional measurement metadata is typed and meaningful when present', () => {
    const evidence = { type: 'measurement', kind: 'profile', detail: 'Native profile observations.' };
    assert.doesNotThrow(() => validateReport(report([finding({ evidence: [evidence] })]), identity));
    for (const [key, values] of Object.entries({
        noisy: ['true', 0, null], samples: [0, -1, 1.5, '3', null], spread: ['', 'x', 1, null],
    })) {
        for (const value of values) {
            assert.throws(() => validateReport(report([finding({ evidence: [{ ...evidence, [key]: value }] })]), identity));
        }
    }
    assert.doesNotThrow(() => validateReport(report([finding({
        evidence: [{ ...evidence, noisy: true, samples: 3, spread: '9-10 ms' }],
    })]), identity));
});

test('concurrency is a schema category, not a keyword-based proof classifier', () => {
    const valid = report([finding({
        category: 'concurrency',
        evidence: [{ type: 'source', detail: 'A generic function changed; synchronization impact is uncertain.' }],
    })]);
    assert.equal(validateReport(valid, identity), valid);
    const bad = report([finding({
        category: 'concurrency',
        severity: 'high',
        confidence: 'high',
        fixDisposition: 'manual_required',
        evidence: [{ type: 'source', detail: 'lock channel await task deadlock race synchronization' }],
    })]);
    assert.throws(() => validateReport(bad, identity), /strong proof/);
    assert.throws(() => validateReport(report([finding({ category: 'invented' })]), identity), /category/);
});

test('medium and low findings remain advice-only', () => {
    const bad = report([finding({ fixDisposition: 'manual_required' })]);
    assert.throws(() => validateReport(bad, identity), /must be advice_only/);
});

test('failed validation is blocked rather than presented as a passing review', () => {
    const check = { name: 'Native benchmark', status: 'error', command: 'ConsoleBench.exe', exitCode: 1, detail: 'Runner failed.' };
    const bad = report([], [check]);
    bad.status = 'pass';
    assert.throws(() => validateReport(bad, identity), /must be blocked/);
});

test('only evidenced high findings produce action_required status', () => {
    const high = finding({
        severity: 'high',
        confidence: 'high',
        evidence: [
            { type: 'source', detail: 'The immutable head introduces an unbounded loop.' },
            { type: 'complexity-proof', detail: 'Caller nesting changes work from O(n) to O(n squared).' },
        ],
        fixDisposition: 'manual_required',
    });
    assert.equal(validateReport(report([high]), identity).status, 'action_required');
    const bad = report([finding()]);
    bad.status = 'action_required';
    assert.throws(() => validateReport(bad, identity), /must be advisory/);
});

test('regression checks require findings without overriding their severity-derived status', () => {
    const checks = [{
        name: 'Repeated path', status: 'regression', command: 'Inspect immutable source',
        exitCode: null, detail: 'Repeated work increased at the reviewed head.',
    }];
    assert.throws(() => validateReport(report([], checks), identity), /regression checks require at least one finding/);
    for (const severity of ['medium', 'low']) {
        const value = report([finding({ severity })], checks);
        assert.equal(validateReport(value, identity).status, 'advisory');
        value.status = 'action_required';
        assert.throws(() => validateReport(value, identity), /must be advisory/);
    }
    const high = report([finding({
        severity: 'high', confidence: 'high', fixDisposition: 'manual_required',
        evidence: [{ type: 'complexity-proof', detail: 'Nested callers increase repeated work from O(n) to O(n squared).' }],
    })], checks);
    assert.equal(validateReport(high, identity).status, 'action_required');
});

test('HIGH confidence, proof types, stable IDs, and guidance remain explicit schema boundaries', () => {
    const high = finding({
        severity: 'high', confidence: 'high', fixDisposition: 'manual_required',
        evidence: [{ type: 'resource-proof', detail: 'Every event retains another unreachable session.' }],
    });
    for (const confidence of ['medium', 'low']) {
        assert.throws(() => validateReport(report([{ ...high, confidence }]), identity), /high confidence/);
    }
    assert.throws(() => validateReport(report([{ ...high, evidence: [{ type: 'source', detail: 'Repeated call.' }] }]), identity), /strong proof/);
    assert.throws(() => validateReport(report([high, high]), identity), /unique stable/);
    assert.throws(() => validateReport(report([{ ...high, id: 'PERF-1' }]), identity), /unique stable/);
    assert.throws(() => validateReport(report([{ ...high, fixDisposition: 'proposed' }]), identity), /only in repair mode/);
    assert.equal(validateReport(report([finding({ severity: 'low', confidence: 'low' })]), identity).status, 'advisory');
});

test('repair mode accepts HIGH proposals but never authorizes model-authored fixes', () => {
    const repairIdentity = { ...identity, mode: 'repair' };
    const high = finding({
        severity: 'high',
        confidence: 'high',
        evidence: [
            { type: 'source', detail: 'The immutable head repeats the operation.' },
            { type: 'complexity-proof', detail: 'Nested callers make the path O(n squared).' },
        ],
        fixDisposition: 'fixed',
        validation: 'Focused regression test passed on the final patch.',
    });
    const fixed = report([high], [{
        name: 'Focused regression',
        status: 'pass',
        command: 'run-focused-test',
        exitCode: 0,
        detail: 'Final patch passed.',
    }]);
    fixed.mode = 'repair';
    fixed.identity = { prNumber: 42, baseSha: 'a'.repeat(40), headSha: 'b'.repeat(40) };
    fixed.status = 'fixed';
    assert.throws(() => validateReport(fixed, repairIdentity), /model-authored fixed/);
    const scope = classifyPullRequest([{ filename: 'src/buffer/out/TextBuffer.cpp' }], fixed.identity);
    assert.throws(() => gatePublication(
        scope,
        fixed,
        { items: [{ type: 'push_to_pull_request_branch' }], errors: [] },
        { ...repairIdentity, changedFiles: ['src/buffer/out/TextBuffer.cpp'] }
    ), /model-authored fixed/);
    const proposed = structuredClone(fixed);
    proposed.checks = [{ name: 'Focused regression', status: 'unavailable', command: 'not run',
        exitCode: null, detail: 'Awaiting trusted native validation.' }];
    proposed.findings[0].fixDisposition = 'proposed';
    proposed.findings[0].validation = 'Native validation pending.';
    proposed.status = 'pending_validation';
    proposed.validationPlan = { type: 'wta-unit', testFilter: 'tests::actual_test' };
    assert.doesNotThrow(() => gatePublication(
        scope, proposed,
        { items: nativeRequests(), errors: [] },
        { ...repairIdentity, changedFiles: ['src/buffer/out/TextBuffer.cpp'] }
    ));
    assert.throws(() => gatePublication(
        scope,
        proposed,
        { items: nativeRequests(), errors: [] },
        { ...repairIdentity, changedFiles: ['tools/unrelated.ps1'] }
    ), /limited to original candidate files/);
});

test('repair mode cannot edit for medium findings or failed validation', () => {
    const repairIdentity = { ...identity, mode: 'repair' };
    const medium = report([finding({ fixDisposition: 'fixed' })]);
    medium.mode = 'repair';
    assert.throws(() => validateReport(medium, repairIdentity), /model-authored fixed/);

    const high = finding({
        severity: 'high',
        confidence: 'high',
        evidence: [{ type: 'blocking-proof', detail: 'A lock is held across an awaited channel receive.' }],
        fixDisposition: 'proposed',
    });
    const failed = report([high], [{
        name: 'Focused regression',
        status: 'error',
        command: 'run-focused-test',
        exitCode: 1,
        detail: 'Test failed.',
    }]);
    failed.mode = 'repair';
    failed.status = 'blocked';
    assert.equal(validateReport(failed, repairIdentity).status, 'blocked');
    const scope = classifyPullRequest([{ filename: 'tools/wta/src/master/mod.rs' }], identity);
    assert.throws(() => gatePublication(scope, failed, {
        items: nativeRequests(),
    }, { ...repairIdentity, changedFiles: ['tools/wta/src/master/mod.rs'] }), /require pending_validation/);
});

test('rejects malformed output and stale head identity', () => {
    assert.throws(() => validateReport({ version: 1 }, identity), /performance review/);
    const stale = report();
    stale.identity = { ...identity, headSha: 'c'.repeat(40) };
    assert.throws(() => validateReport(stale, identity), /does not match/);
});

test('fork-compatible publication gate allows only an exact deterministic comment', () => {
    const scope = classifyPullRequest([{ filename: 'src/buffer/out/TextBuffer.cpp' }], identity);
    const valid = report([finding()]);
    const body = renderReport(valid);
    assert.doesNotThrow(() => gatePublication(scope, valid, { items: [{ type: 'add_comment', body }], errors: [] }, identity));
    assert.throws(() => gatePublication(scope, valid, { items: [{ type: 'add_comment', body: `${body}changed` }] }, identity), /exactly match/);
    for (const key of ['item_number', 'pr', 'pr_number', 'issue', 'issue_number', 'repo', 'target', 'target_repo',
        'target-repo', 'comment_id', 'reply_to_id', 'discussion_id']) {
        assert.throws(() => gatePublication(scope, valid, {
            items: [{ type: 'add_comment', body, [key]: key === 'repo' ? 'other/repo' : 999 }],
        }, identity), /must not override/);
    }
    assert.throws(() => gatePublication(scope, valid, {
        items: [{ type: 'add_comment', body }, { type: 'noop' }],
    }, identity), /exactly one add_comment/);
});

test('non-applicable scope short-circuits with one noop and no report', () => {
    const scope = classifyPullRequest([{ filename: 'doc/performance.md' }], identity);
    assert.doesNotThrow(() => gatePublication(scope, null, { items: [{ type: 'noop' }], errors: [] }, identity));
    assert.throws(() => gatePublication(scope, report(), { items: [{ type: 'add_comment', body: 'x' }], errors: [] }, identity), /exactly one noop/);
});

test('scope rejects malformed and unsafe paths instead of creating false candidates', () => {
    for (const filename of [
        'src/renderer/../secret.cpp', '/src/renderer/file.cpp',
        'src\\renderer\\file.cpp', 'src/renderer/file.cpp\nother.cpp',
        'src/renderer//file.cpp', 'src/renderer/file.cpp:stream',
    ]) {
        assert.throws(() => classifyPullRequest([{ filename }], identity), /safe repository-relative/);
    }
    assert.throws(() => classifyPullRequest({}, identity), /must be an array/);
    assert.throws(() => classifyPullRequest([[{ filename: 'tools/wta/src/master/mod.rs' }]], identity), /valid filename/);
    assert.throws(() => classifyPullRequest(Array(3001).fill({ filename: 'README.md' }), identity), /at most 3000/);
});

test('repair noop rejects dirty product source and requires an inventory', () => {
    const expected = { ...identity, mode: 'repair' };
    const scope = classifyPullRequest([{ filename: 'src/renderer/base/renderer.cpp' }], identity);
    const valid = report();
    valid.mode = 'repair';
    assert.throws(() => gatePublication(scope, valid, { items: [{ type: 'noop' }] }, expected),
        /final changed-file inventory/);
    assert.throws(() => gatePublication(scope, valid, { items: [{ type: 'noop' }] }, {
        ...expected, changedFiles: ['src/renderer/base/renderer.cpp'],
    }), /cannot discard/);
    assert.doesNotThrow(() => gatePublication(scope, valid, { items: [{ type: 'noop' }] }, {
        ...expected, changedFiles: [],
    }));
});

test('malformed ingestion errors and tampered applicability fail closed', () => {
    const scope = classifyPullRequest([{ filename: 'src/renderer/base/renderer.cpp' }], identity);
    assert.throws(() => gatePublication(scope, report(), { items: [{ type: 'noop' }], errors: {} }, identity),
        /envelope is invalid/);
    scope.applicable = false;
    assert.throws(() => gatePublication(scope, null, { items: [{ type: 'noop' }] }, identity),
        /correctly classified/);
});

test('passing checks cannot hide nonzero or missing exit codes', () => {
    for (const exitCode of [1, null]) {
        const value = report([], [{
            name: 'Fake pass', command: 'test', detail: 'Claimed pass', status: 'pass', exitCode,
        }]);
        assert.throws(() => validateReport(value, identity), /passing checks must exit 0/);
    }
});

test('unavailable checks require null exit codes and render as not run', () => {
    const check = { name: 'Native measurement', command: 'not run', detail: 'Windows unavailable.',
        status: 'unavailable', exitCode: null };
    const value = report([], [check]);
    assert.doesNotThrow(() => validateReport(value, identity));
    const card = renderReport(value);
    assert.match(card, /not run/);
    assert.doesNotMatch(card, /exit 0/);
    for (const exitCode of [-1, 0, 1]) {
        assert.throws(() => validateReport(report([], [{ ...check, exitCode }]), identity),
            /unavailable checks must have null exitCode/);
    }
});

test('live PR verification does not trust a caller-supplied same-repository flag', () => {
    const expected = {
        ...identity, expectedBaseSha: identity.baseSha, repository: 'owner/repo',
        headRepository: 'owner/repo', headRef: 'topic', baseRef: 'main', sameRepo: 'true',
    };
    const pr = {
        number: 42, state: 'open',
        head: { sha: identity.headSha, ref: 'topic', repo: { id: 1, full_name: 'owner/repo' } },
        base: { sha: identity.baseSha, ref: 'main', repo: { id: 1, full_name: 'owner/repo' } },
    };
    assert.doesNotThrow(() => verifyPullRequest(pr, expected));
    for (const mutate of [
        p => { p.head.repo.id = 2; },
        p => { p.head.sha = 'c'.repeat(40); },
        p => { p.base.sha = 'c'.repeat(40); },
        p => { p.state = 'closed'; },
        p => { p.head.ref = 'other'; },
    ]) {
        const modified = structuredClone(pr);
        mutate(modified);
        assert.throws(() => verifyPullRequest(modified, expected), /metadata does not match/);
    }
});

test('successful workflow runs do not override blocking domain verdicts', () => {
    for (const status of ['action_required', 'blocked']) {
        assert.throws(() => validateVerdict({ version: 1, identity, status }, identity), /requires action/);
    }
    for (const status of ['pass', 'advisory', 'pending_validation']) {
        assert.doesNotThrow(() => validateVerdict({ version: 1, identity, status }, identity));
    }
    assert.throws(() => validateVerdict({ version: 1, identity, status: 'fixed' }, identity), /malformed/);
    assert.throws(() => validateVerdict({ version: 1, identity, status: 'unknown' }, identity), /malformed/);
});

test('proposals accept only bounded regular WTA replacements and agreeing fixed test plans', () => {
    const valid = proposal();
    const expected = { ...identity, mode: 'repair' };
    assert.equal(validateProposal(valid, expected), valid);
    const invalid = [
        [value => { value.identity.headSha = 'd'.repeat(40); }, /does not match/],
        [value => { value.report.identity.headSha = 'd'.repeat(40); }, /does not match/],
        [value => { value.treeSha = 'not-a-tree'; }, /Git tree SHA/],
        [value => { value.report.findings[0].fixDisposition = 'manual_required'; value.report.status = 'action_required'; }, /only repair-eligible/],
        [value => { value.validationPlan.type = 'arbitrary-command'; }, /supported focused native/],
        [value => { value.validationPlan.testFilter = 'tests; cmd.exe'; }, /supported focused native/],
        [value => { value.validationPlan.testFilter = 'other::tests::preserves_state'; }, /must agree/],
        [value => { delete value.report.validationPlan; }, /supported focused native/],
        [value => { value.files = []; }, /between one and three/],
        [value => { value.files = Array(4).fill(value.files[0]); }, /between one and three/],
        [value => { value.files.push({ ...value.files[0] }); }, /unique, regular WTA/],
        [value => { value.files[0].mode = '100755'; }, /unique, regular WTA/],
        [value => { value.files[0].mode = '120000'; }, /unique, regular WTA/],
        [value => { value.files[0].path = 'src/renderer/base/renderer.cpp'; }, /unique, regular WTA/],
        [value => { value.files[0].path = 'src/renderer/atlas/shader_ps.hlsl'; }, /unique, regular WTA/],
        [value => { value.files[0].path = 'tools/wta/Cargo.toml'; }, /unique, regular WTA/],
        [value => { value.files[0].path = 'tools/wta/Cargo.lock'; }, /unique, regular WTA/],
        [value => { value.files[0].path = 'tools/wta/src/../policy.rs'; }, /safe repository-relative/],
        [value => { value.files[0].path = 'tools/wta/src/tests/fix.rs'; }, /unique, regular WTA/],
        [value => { value.files[0].contents = 'not base64'; }, /unique, regular WTA/],
        [value => { value.files[0].contents = Buffer.alloc(256 * 1024 + 1).toString('base64'); }, /size limit/],
        [value => { value.files.push({ ...value.files[0], path: 'tools/wta/src/extra.rs' });
            for (const file of value.files) file.contents = Buffer.alloc(128 * 1024 + 1).toString('base64'); }, /size limit/],
    ];
    for (const [mutate, message] of invalid) {
        const value = structuredClone(valid);
        mutate(value);
        assert.throws(() => validateProposal(value, expected), message);
    }
});

test('the model validator rejects the observed copied selector before native dispatch', () => {
    const value = proposal();
    value.report.validationPlan.testFilter = 'module::tests';
    value.validationPlan.testFilter = 'module::tests';
    assert.throws(() => validateReport(value.report, { ...identity, mode: 'repair' }), /not a placeholder/);
    assert.throws(() => validateProposal(value, identity), /not a placeholder/);
});

test('focused selectors require qualified Rust identifiers, not module substring filters', () => {
    for (const selector of [
        'tests', 'master::tests', 'fixture::tests', 'preserves_state',
        'master:tests:preserves_state', 'master::::preserves_state', '::master::preserves_state',
        'master::preserves_state::', '1master::preserves_state', 'master::1test',
        'master::preserves-state', 'master::préserves_state', 'master::' + 'a'.repeat(200),
    ]) {
        const value = proposal();
        value.validationPlan.testFilter = selector;
        value.report.validationPlan.testFilter = selector;
        assert.throws(() => validateReport(value.report, { ...identity, mode: 'repair' }), /supported focused native/);
        assert.throws(() => validateProposal(value, identity), /supported focused native/);
    }
    // Syntax alone is not source proof; native listing on the original head is decisive.
    for (const selector of ['master::tests::preserves_state', 'fixture::tests::preserves_state', 'other_module::_test2']) {
        const value = proposal();
        value.validationPlan.testFilter = selector;
        value.report.validationPlan.testFilter = selector;
        assert.doesNotThrow(() => validateReport(value.report, { ...identity, mode: 'repair' }));
        assert.equal(validateProposal(value, identity), value);
    }
});

test('stable finding IDs are bounded identifiers, not an arbitrary eight-character protocol', () => {
    const value = proposal();
    value.report.findings[0].id = 'PERF-WTAQUAD01';
    assert.doesNotThrow(() => validateReport(value.report, { ...identity, mode: 'repair' }));
    value.report.findings[0].id = 'PERF-' + 'A'.repeat(65);
    assert.throws(() => validateReport(value.report, { ...identity, mode: 'repair' }), /stable PERF identifier/);
});

test('an unvalidated proposal card is visibly pending, not a green success', () => {
    const card = renderReport(proposal().report);
    assert.match(card, /^## ⏳ Intelligent Terminal performance review/);
    assert.match(card, /\*\*Status:\*\* `pending_validation`/);
    assert.match(card, /\*\*Disposition:\*\* proposed/);
    assert.doesNotMatch(card, /✅|GitHub recorded Windows validation success/);
});

test('published cards preserve measurement kind, samples and noisy spread without inventing source metadata', () => {
    const value = proposal();
    value.report.findings[0].evidence = [
        { type: 'source', detail: 'Original source proof.' },
        ...['microbenchmark', 'end-to-end', 'profile'].map(kind => ({
            type: 'measurement', kind, noisy: true, samples: 3, spread: 'range 9–100 ms', detail: `${kind} observations.`,
        })),
    ];
    const card = renderReport(value.report, { publishedRunId: 123 });
    assert.match(card, /source: Original source proof\./);
    for (const kind of ['microbenchmark', 'end-to-end', 'profile'])
        assert.ok(card.includes(`measurement (kind: ${kind}, samples: 3, spread: range 9–100 ms): ${kind} observations.`));
    assert.match(card, /\*\*Status:\*\* `fixed`/);
    value.report.findings[0].evidence = [{ type: 'measurement', kind: 'profile', detail: 'Profile observations.' }];
    const sparse = renderReport(value.report);
    assert.match(sparse, /measurement \(kind: profile\): Profile observations\./);
    assert.doesNotMatch(sparse, /samples:|spread:/);
});

function nativeRequests(confirm) {
    if (arguments.length === 0) confirm = true;
    return ['validate_performance_original_tests', 'validate_performance_focused_tests', 'validate_performance_repair']
        .map(type => ({ type, confirm }));
}

test('repair proposals require exactly three confirmed validation tools; fake pass cannot permit a push', () => {
    const valid = proposal();
    const expected = { ...identity, mode: 'repair', changedFiles: valid.files.map(file => file.path) };
    const scope = classifyPullRequest(valid.files.map(file => ({ filename: file.path })), identity);
    valid.report.checks.push({ name: 'Fake native pass', status: 'pass', command: 'invented', exitCode: 0, detail: 'Model claimed native success.' });
    assert.throws(() => validateReport(valid.report, expected), /cannot contain model-authored pass checks/);
    assert.throws(() => gatePublication(scope, valid.report, {
        items: nativeRequests(),
    }, expected), /cannot contain model-authored pass checks/);
    valid.report.checks[0] = { name: 'Native validation', status: 'unavailable', command: 'not run', exitCode: null, detail: 'Awaiting trusted Windows validation.' };
    for (const items of [
        [{ type: 'push_to_pull_request_branch' }],
        [{ type: 'add_comment', body: renderReport(valid.report) }],
        [{ type: 'validate_performance_repair', confirm: true }, { type: 'noop' }],
        [],
    ]) {
        assert.throws(() => gatePublication(scope, valid.report, { items }, expected), /exactly one each of/);
    }
    for (const confirm of [undefined, false, 'false', 1]) {
        assert.throws(() => gatePublication(scope, valid.report, {
            items: nativeRequests(confirm),
        }, expected), /explicit proposal confirmation/);
    }
    for (const confirm of [true, 'true']) {
        assert.doesNotThrow(() => gatePublication(scope, valid.report, {
            items: nativeRequests(confirm),
        }, expected));
    }
    for (let phase = 0; phase < 3; phase++) {
        const missing = nativeRequests();
        missing.splice(phase, 1);
        assert.throws(() => gatePublication(scope, valid.report, { items: missing }, expected), /exactly one each of/);
        const duplicate = nativeRequests();
        duplicate[(phase + 1) % 3] = { ...duplicate[phase] };
        assert.throws(() => gatePublication(scope, valid.report, { items: duplicate }, expected), /exactly one each of/);
        const unconfirmed = nativeRequests();
        unconfirmed[phase].confirm = false;
        assert.throws(() => gatePublication(scope, valid.report, { items: unconfirmed }, expected), /explicit proposal confirmation/);
    }
});

test('every proposed location is canonical and bound to a covered sealed replacement, independently of category', () => {
    const expected = { ...identity, mode: 'repair' };
    for (const category of ['concurrency', 'session-log-enumeration', 'wta-runtime']) {
        const value = proposal();
        value.report.findings[0].category = category;
        assert.equal(validateProposal(value, expected), value);
    }
    for (const location of [
        'src/renderer/atlas/AtlasEngine.cpp:1', 'tools/wta/src/unchanged.rs:1',
        'tools\\wta\\src\\master\\mod.rs:1', './tools/wta/src/master/mod.rs:1',
        'tools/wta/src/master/mod.rs:0', 'tools/wta/src/master/mod.rs:01',
        'tools/wta/src/master/mod.rs:1:2', 'tools/wta/src/master/mod.rs',
    ]) {
        const value = proposal();
        value.report.findings[0].location = location;
        assert.throws(() => validateProposal(value, expected), /canonical|safe repository-relative/);
    }
    const value = proposal();
    value.files.push({ ...value.files[0], path: 'tools/wta/src/extra.rs' });
    assert.throws(() => validateProposal(value, expected), /covered by a proposed finding/);
    value.report.findings.push(finding({
        ...value.report.findings[0], id: 'PERF-EXTRA', location: 'tools/wta/src/extra.rs:3',
    }));
    assert.equal(validateProposal(value, expected), value);
    value.report.findings[1].location = 'src/renderer/atlas/AtlasEngine.cpp:1';
    assert.throws(() => validateProposal(value, expected), /canonical/);
});

test('inline Rust test source is immutable Git data, including markers, gates, modules and every trailing test', () => {
    withRepository(({ git, write, expected, output }) => {
        const filename = 'tools/wta/src/master/mod.rs';
        const suffix = '#[cfg(test)]\nmod tests {\n    #[test]\n    fn selected() { assert_eq!(super::work(), 2); }\n' +
            '    #[tokio::test]\n    async fn other() { assert!(true); }\n}\n';
        const original = 'fn work() -> usize { 2 }\n' + suffix;
        write('tools\\wta\\src\\master\\mod.rs', original);
        git(['add', '--all']);
        git(['commit', '--quiet', '-m', 'inline original']);
        expected.headSha = git(['rev-parse', 'HEAD']);
        const replacement = contents => [{ path: filename, mode: '100644', contents: Buffer.from(contents).toString('base64') }];
        const candidate = original.replace('usize { 2 }', 'usize { 3 }');
        assert.doesNotThrow(() => reconstructTree(replacement(candidate), expected.headSha, expected.baseSha));
        for (const changed of [
            candidate.replace('super::work(), 2', 'super::work(), 3'),
            candidate.replace('assert!(true)', 'assert!(false)'),
            candidate.replace('cfg(test)', 'cfg(any())'),
            candidate.replace('mod tests', 'mod renamed'),
            candidate.replace('#[test]', '#[ignore]'),
            candidate.replace('#[tokio::test]', ''),
            candidate.replace('fn selected', 'fn renamed'),
            candidate.replace(suffix, ''),
            candidate + '#[test] fn added() {}\n',
            '#[test] fn added() {}\n' + candidate,
        ]) {
            // A model-mutated workspace must never become the comparison authority.
            write('tools\\wta\\src\\master\\mod.rs', changed);
            assert.throws(() => reconstructTree(replacement(changed), expected.headSha), /immutable original Rust test/);
            const baselinePath = path.join(output, 'baseline.json');
            const scope = prepareScope(expected, output, baselinePath);
            const value = proposal();
            value.report.identity = expected;
            write('tools\\wta\\src\\master\\mod.rs', changed + '// model edit\n');
            assert.throws(() => sealProposal(scope, value.report, JSON.parse(fs.readFileSync(baselinePath)), expected),
                /immutable original Rust test/);
        }
        for (const gate of [
            '#[\n cfg(\n any(windows, test)\n )\n]\nmod tests { #[test] fn selected() {} }\n',
            '#[cfg_attr(\n test,\n allow(dead_code)\n)]\nfn helper() {}\n',
            '#[tokio::test]\nasync fn selected() {}\n',
            '#[cfg(windows)]\nmod custom {\n#[tokio::test]\nasync fn selected() {}\n}\n',
            '    #[cfg(windows)]\n    mod custom {\n#[tokio::test]\nasync fn selected() {}\n}\n',
            '    mod custom {\n#[test]\nfn selected() {}\n}\n',
        ]) {
            write('tools\\wta\\src\\master\\mod.rs', 'fn runtime() {}\n' + gate);
            git(['add', filename]);
            git(['commit', '--quiet', '-m', 'gate pattern']);
            const head = git(['rev-parse', 'HEAD']);
            assert.doesNotThrow(() => reconstructTree(replacement('fn repaired() {}\n' + gate), head));
            assert.throws(() => reconstructTree(replacement('fn repaired() {}\n' + gate.replace(/test/g, 'renamed')), head),
                /immutable original Rust test/);
            for (const token of ['#[cfg(windows)]', 'mod custom', '#[test]', '#[tokio::test]']) {
                if (!gate.includes(token)) continue;
                assert.throws(() => reconstructTree(replacement('fn repaired() {}\n' + gate.replace(token, `//${token}`)), head),
                    /immutable original Rust test/, `commenting ${token} must not bypass the suffix guard`);
            }
        }
        assert.throws(() => reconstructTree(replacement('fn runtime() {}\n#[test] fn added() {}\n'), expected.baseSha),
            /immutable original Rust test/);
    });
});

test('large original modules permit small actual edits while preserving trailing inline tests', () => {
    withRepository(({ git, write, expected, output }) => {
        const filename = 'tools/wta/src/master/mod.rs';
        const original = Array.from({ length: 3000 }, (_, i) => `fn work_${i}() -> usize { ${i} }\n`).join('') +
            '#[cfg(test)]\nmod tests { #[test] fn selected() {} }\n';
        assert.ok(Buffer.byteLength(original) > 64 * 1024);
        write(filename, original);
        git(['add', '--all']);
        git(['commit', '--quiet', '-m', 'large immutable module']);
        expected.headSha = git(['rev-parse', 'HEAD']);
        const baselinePath = path.join(output, 'baseline.json');
        const scope = prepareScope(expected, output, baselinePath);
        const candidate = original.replace('fn work_0() -> usize { 0 }', 'fn work_0() -> usize { 1 }');
        const files = [{ path: filename, mode: '100644', contents: Buffer.from(candidate).toString('base64') }];
        // The workspace is not the immutable comparison authority.
        write(filename, candidate);
        const tree = reconstructTree(files, expected.headSha, expected.baseSha);
        const value = proposal();
        value.report.identity = expected;
        assert.equal(sealProposal(scope, value.report, JSON.parse(fs.readFileSync(baselinePath)), expected).proposal.treeSha, tree);
        assert.match(git(['diff', '--no-ext-diff', '--no-textconv', '--no-renames', '--numstat', expected.headSha, tree]), /^1\t1\t/);
    });
});

test('actual immutable-head line ceilings allow 100 and reject 101, including rewrites and aggregate edits', () => {
    withRepository(({ git, write, expected }) => {
        const paths = ['tools/wta/src/master/mod.rs', 'tools/wta/src/second.rs', 'tools/wta/src/third.rs'];
        const originals = paths.map((_, file) => Array.from({ length: 50 }, (_, i) => `fn original_${file}_${i}() {}\n`).join(''));
        paths.forEach((filename, i) => write(filename, originals[i]));
        git(['add', '--all']);
        git(['commit', '--quiet', '-m', 'line ceiling originals']);
        expected.headSha = git(['rev-parse', 'HEAD']);
        const replacements = contents => contents.map((source, i) => ({
            path: paths[i], mode: '100644', contents: Buffer.from(source).toString('base64'),
        }));
        const rewrite = Array.from({ length: 50 }, (_, i) => `fn rewritten_${i}() {}\n`).join('');
        assert.doesNotThrow(() => reconstructTree(replacements([rewrite]), expected.headSha, expected.baseSha));
        assert.throws(() => reconstructTree(replacements([rewrite + 'fn extra() {}\n']), expected.headSha, expected.baseSha),
            /100 added-plus-deleted-line/);
        const edit = (original, count) => original.split('\n').map((line, i) => i < count ? line.replace('original', 'changed') : line).join('\n');
        const hundred = originals.map((original, i) => edit(original, i === 2 ? 16 : 17));
        assert.doesNotThrow(() => reconstructTree(replacements(hundred), expected.headSha, expected.baseSha));
        hundred[2] += 'fn one_more() {}\n';
        assert.throws(() => reconstructTree(replacements(hundred), expected.headSha, expected.baseSha), /100 added-plus-deleted-line/);
        assert.throws(() => reconstructTree(replacements(originals.map(() => rewrite)), expected.headSha), /100 added-plus-deleted-line/);
        assert.throws(() => reconstructTree([...replacements(originals), {
            path: 'tools/wta/src/fourth.rs', mode: '100644', contents: Buffer.from('fn extra() {}\n').toString('base64'),
        }], expected.headSha), /between one and three/);
        assert.throws(() => reconstructTree(replacements(['x'.repeat(256 * 1024 + 1)]), expected.headSha), /transport size limit/);
    });
});

test('exact UTF-8 zero-context diff bytes permit 16 KiB and reject the next byte at every helper gate', () => {
    withRepository(({ root, git, write, expected, output }) => {
        const filename = 'tools/wta/src/master/mod.rs';
        const baselinePath = path.join(output, 'baseline.json');
        const scope = prepareScope(expected, output, baselinePath);
        const baseline = JSON.parse(fs.readFileSync(baselinePath));
        const replacement = contents => [{ path: filename, mode: '100644', contents: Buffer.from(contents).toString('base64') }];
        const diffArgs = ['diff', '--no-ext-diff', '--no-textconv', '--no-renames', '--no-color', '--unified=0'];
        const measure = tree => execFileSync('git', [...diffArgs, expected.headSha, tree, '--'], { cwd: root }).length;
        const initialTree = reconstructTree(replacement('// \n'), expected.headSha, expected.baseSha);
        const padding = 16 * 1024 - measure(initialTree);
        const exact = '// ' + 'é'.repeat(Math.floor(padding / 2)) + 'x'.repeat(padding % 2) + '\n';
        const tree = reconstructTree(replacement(exact), expected.headSha, expected.baseSha);
        assert.equal(measure(tree), 16 * 1024);
        write(filename, exact);
        const value = proposal();
        value.report.identity = expected;
        const sealed = sealProposal(scope, value.report, baseline, expected).proposal;
        assert.equal(sealed.treeSha, tree);
        const oversized = exact.replace('\n', 'x\n');
        write(filename, oversized);
        // Independently measure the rejected tree; don't infer bytes from candidate file length.
        git(['add', '--all']);
        const oversizedTree = git(['write-tree']);
        git(['reset', '--mixed', expected.headSha]);
        assert.equal(measure(oversizedTree), 16 * 1024 + 1);
        assert.throws(() => reconstructTree(replacement(oversized), expected.headSha, expected.baseSha), /16 KiB byte limit/);
        assert.throws(() => sealProposal(scope, value.report, baseline, expected), /16 KiB byte limit/);
        sealed.files = replacement(oversized);
        sealed.treeSha = oversizedTree;
        assert.doesNotThrow(() => validateProposal(sealed, expected)); // Blob transport alone is not the actual-diff gate.
        const proposalPath = path.join(output, 'oversized.json');
        fs.writeFileSync(proposalPath, JSON.stringify(sealed));
        for (const command of ['validate-proposal', 'apply-proposal']) {
            const result = spawnSync(process.execPath, [helperPath, command, '--input', proposalPath,
                '--pr', String(expected.prNumber), '--base', expected.baseSha, '--head', expected.headSha],
            { cwd: root, encoding: 'utf8' });
            assert.equal(result.status, 1);
            assert.match(result.stderr, /16 KiB byte limit/);
            assert.equal(git(['write-tree']), git(['rev-parse', 'HEAD^{tree}']));
        }
        const reportPath = path.join(output, 'report.json');
        const agentPath = path.join(output, 'agent.json');
        fs.writeFileSync(reportPath, JSON.stringify(value.report));
        fs.writeFileSync(agentPath, JSON.stringify({ items: nativeRequests() }));
        const trust = path.join(root, '.performance-trusted');
        execFileSync('git', ['clone', '--quiet', '--no-local', root, trust]);
        const result = spawnSync(process.execPath, [helperPath, 'gate', '--mode', 'repair', '--output-dir', output,
            '--trusted-repository-root', trust, '--agent-worktree-root', root,
            '--baseline', baselinePath, '--report', reportPath, '--agent-output', agentPath,
            '--pr', String(expected.prNumber), '--base', expected.baseSha, '--head', expected.headSha],
        { cwd: root, encoding: 'utf8' });
        assert.equal(result.status, 1);
        assert.match(result.stderr, /16 KiB byte limit/);
        assert.equal(fs.existsSync(path.join(output, 'performance-proposal.json')), false);
    });
});

test('zero-context byte ceilings cover the aggregate patch, not each replacement separately', () => {
    withRepository(({ git, write, expected }) => {
        const paths = ['tools/wta/src/master/mod.rs', 'tools/wta/src/second.rs'];
        write(paths[1], 'fn other() {}\n');
        git(['add', '--all']);
        git(['commit', '--quiet', '-m', 'aggregate byte originals']);
        expected.headSha = git(['rev-parse', 'HEAD']);
        const files = paths.map(filename => ({
            path: filename, mode: '100644',
            contents: Buffer.from('// ' + 'é'.repeat(4050) + '\n').toString('base64'),
        }));
        for (const file of files)
            assert.doesNotThrow(() => reconstructTree([file], expected.headSha, expected.baseSha));
        assert.throws(() => reconstructTree(files, expected.headSha, expected.baseSha), /16 KiB byte limit/);
        const binary = [{ ...files[0], contents: Buffer.from('fn head() {}\n\0').toString('base64') }];
        assert.throws(() => reconstructTree(binary, expected.headSha), /numeric Git line counts/);
    });
});

test('Git preparation and sealing separate trusted restoration from model source edits and preserve raw blobs', () => {
    withRepository(({ root, git, write, expected, output }) => {
        write('doc\\policy.md', 'Trusted restoration, not a model edit.\n');
        const baselinePath = path.join(output, 'baseline.json');
        const scope = prepareScope(expected, output, baselinePath);
        const baseline = JSON.parse(fs.readFileSync(baselinePath, 'utf8'));
        assert.deepEqual(scope.candidates.map(file => file.filename), ['tools/wta/src/master/mod.rs']);
        assert.match(fs.readFileSync(path.join(output, 'performance-patch.txt'), 'utf8'), /fn head/);
        assert.ok(baseline.inventory['doc/policy.md'], 'raw baseline records trusted restoration');
        const unchangedTree = git(['write-tree']);
        git(['config', 'core.autocrlf', 'true']);
        const raw = Buffer.from('fn repaired() {}\r\n');
        write('tools\\wta\\src\\master\\mod.rs', raw);
        const value = proposal();
        value.report.identity = expected;
        const sealed = sealProposal(scope, value.report, baseline, expected);
        assert.deepEqual(sealed.changedFiles, ['tools/wta/src/master/mod.rs']);
        assert.equal(validateProposal(sealed.proposal, expected), sealed.proposal);
        const rawBlob = execFileSync('git', ['cat-file', 'blob', `${sealed.proposal.treeSha}:tools/wta/src/master/mod.rs`], { cwd: root });
        assert.deepEqual(Buffer.from(sealed.proposal.files[0].contents, 'base64'), rawBlob);
        assert.deepEqual(rawBlob, raw);
        assert.equal(git(['show', `${sealed.proposal.treeSha}:doc/policy.md`]), 'Reviewed policy.');
        assert.equal(git(['write-tree']), unchangedTree);
        assert.equal(reconstructTree(sealed.proposal.files, expected.headSha, expected.baseSha), sealed.proposal.treeSha);
        const rawReplacement = { ...sealed.proposal.files[0], contents: raw.toString('base64') };
        const rawTree = reconstructTree([rawReplacement], expected.headSha, expected.baseSha);
        assert.deepEqual(execFileSync('git', ['cat-file', 'blob', `${rawTree}:tools/wta/src/master/mod.rs`], { cwd: root }), raw);
        assert.throws(() => reconstructTree([{
            ...sealed.proposal.files[0], path: 'tools/wta/src/unchanged.rs',
        }], expected.headSha, expected.baseSha), /outside the immutable original/);
        assert.throws(() => reconstructTree([{
            ...sealed.proposal.files[0], path: 'tools/wta/src/missing.rs',
        }], expected.headSha), /existing regular file/);
        assert.throws(() => reconstructTree([{
            ...sealed.proposal.files[0], mode: '100755',
        }], expected.headSha), /existing regular file/);
        write('doc\\policy.md', 'Model tried an unrelated edit.\n');
        assert.throws(() => sealProposal(scope, value.report, baseline, expected), /confined to original candidate/);
        assert.equal(fs.readdirSync(path.join(root, '.git')).some(name => name.startsWith('performance-index-')), false);
    });
});

test('CLI re-reads source scope from Git and checks the sealed tree before applying replacements', () => {
    withRepository(({ root, git, write, expected, output }) => {
        const baselinePath = path.join(output, 'baseline.json');
        const scope = prepareScope(expected, output, baselinePath);
        const baseline = JSON.parse(fs.readFileSync(baselinePath, 'utf8'));
        const value = proposal();
        value.report.identity = expected;
        write('tools\\wta\\src\\master\\mod.rs', 'fn repaired() {}\n');
        const sealed = sealProposal(scope, value.report, baseline, expected).proposal;
        const proposalPath = path.join(output, 'proposal.json');
        fs.writeFileSync(proposalPath, JSON.stringify(sealed));
        const identityArgs = ['--pr', String(expected.prNumber), '--base', expected.baseSha, '--head', expected.headSha];
        const run = (command, args = []) => spawnSync(process.execPath, [helperPath, command, ...identityArgs, ...args], { cwd: root, encoding: 'utf8' });
        assert.equal(run('validate-proposal', ['--input', proposalPath]).status, 0);
        sealed.treeSha = 'c'.repeat(40);
        fs.writeFileSync(proposalPath, JSON.stringify(sealed));
        const wrongTree = run('apply-proposal', ['--input', proposalPath]);
        assert.equal(wrongTree.status, 1);
        assert.match(wrongTree.stderr, /do not match the sealed tree/);
        sealed.treeSha = reconstructTree(sealed.files, expected.headSha, expected.baseSha);
        fs.writeFileSync(proposalPath, JSON.stringify(sealed));
        git(['checkout', '--', 'tools/wta/src/master/mod.rs']);
        assert.equal(run('apply-proposal', ['--input', proposalPath]).status, 0);
        assert.equal(git(['write-tree']), sealed.treeSha);
        assert.equal(fs.readFileSync(path.join(root, 'tools', 'wta', 'src', 'master', 'mod.rs'), 'utf8'), 'fn repaired() {}\n');
        git(['reset', '--hard', expected.headSha]);
        const reportPath = path.join(output, 'report.json');
        const agentPath = path.join(output, 'agent.json');
        fs.writeFileSync(reportPath, JSON.stringify(sealed.report));
        fs.writeFileSync(agentPath, JSON.stringify({ items: nativeRequests() }));
        fs.writeFileSync(path.join(output, 'performance-scope.json'), JSON.stringify({
            ...scope, candidates: [{ filename: 'tools/wta/src/unchanged.rs' }],
        }));
        write('tools\\wta\\src\\unchanged.rs', 'fn model_tried_outside_scope() {}\n');
        const trust = path.join(root, '.performance-trusted');
        execFileSync('git', ['clone', '--quiet', '--no-local', root, trust]);
        const rejected = run('gate', [
            '--trusted-repository-root', trust, '--agent-worktree-root', root,
            '--mode', 'repair', '--output-dir', output, '--baseline', baselinePath,
            '--report', reportPath, '--agent-output', agentPath,
        ]);
        assert.equal(rejected.status, 1);
        assert.match(rejected.stderr, /confined to original candidate/);
        const refreshed = JSON.parse(fs.readFileSync(path.join(output, 'performance-scope.json'), 'utf8'));
        assert.deepEqual(refreshed.candidates.map(file => file.filename), ['tools/wta/src/master/mod.rs']);
        assert.equal(fs.existsSync(path.join(output, 'performance-proposal.json')), false);
    });
});
