import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import test from 'node:test';

const script = fileURLToPath(new URL('../scripts/Invoke-NativePerformanceAnalysis.ps1', import.meta.url));
const workflow = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance.md', import.meta.url), 'utf8');

function fixture(t) {
    const parent = process.env.PERFORMANCE_NATIVE_FIXTURE_ROOT ?? path.join(process.cwd(), 'Generated Files');
    fs.mkdirSync(parent, { recursive: true });
    const root = fs.mkdtempSync(path.join(parent, 'native-analysis-'));
    t.after(() => fs.rmSync(root, { recursive: true, force: true }));
    const git = args => execFileSync('git', args, { cwd: root, encoding: 'utf8', timeout: 15000 }).trim();
    const write = (name, value) => {
        fs.mkdirSync(path.dirname(path.join(root, name)), { recursive: true });
        fs.writeFileSync(path.join(root, name), value);
    };
    const commit = message => {
        git(['add', '.']);
        git(['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '--quiet', '-m', message]);
        return git(['rev-parse', 'HEAD']);
    };
    const run = (scope, suffix = 'analysis', cwd = root) => {
        const output = path.join(root, '.git', suffix);
        fs.mkdirSync(output, { recursive: true });
        const input = path.join(root, '.git', 'scope.json');
        fs.writeFileSync(input, JSON.stringify(scope));
        const result = spawnSync('pwsh', ['-NoLogo', '-NoProfile', '-File', script,
            '-ScopePath', input, '-OutputDirectory', output], { cwd, encoding: 'utf8', timeout: 180000 });
        assert.equal(result.error, undefined);
        const metadata = JSON.parse(fs.readFileSync(path.join(output, 'native-results.json'), 'utf8'));
        return { result, metadata, output };
    };
    git(['init', '--quiet', '--initial-branch=main']);
    return { root, git, write, commit, run };
}

test('Windows native job is original-source, read-only and feeds Linux review even after analysis failure', () => {
    assert.match(workflow, /native_analysis:[\s\S]*?runs-on: windows-2025-vs2026/);
    assert.match(workflow, /native_analysis:[\s\S]*?contents: read/);
    assert.match(workflow, /needs: \[prepare, native_analysis\]/);
    assert.match(workflow, /if: always\(\) && !cancelled\(\) && needs\.prepare\.result == 'success'/);
    assert.match(workflow, /Read `\/tmp\/gh-aw\/native-performance\/`/);
    const native = workflow.split(/^  native_analysis:\r?\n/m)[1].split(/^  agent:\r?\n/m)[0];
    assert.doesNotMatch(native, /secrets\.|contents: write|Deploy|TestHost|cargo test/);
    assert.match(native, /if: always\(\)[\s\S]*?performance-native-\$\{\{ github\.run_id \}\}/);
    assert.match(workflow, /safe_outputs:[\s\S]*?needs\.native_analysis\.result == 'success' \|\| needs\.native_analysis\.result == 'skipped'/);
    const compiled = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance.lock.yml', import.meta.url), 'utf8');
    assert.match(compiled, /^  safe_outputs:\r?\n    needs:\r?\n(?:      - .+\r?\n)*      - native_analysis\r?\n/m);
});

test('controller budget includes the compiled native-analysis phase and reporting headroom', () => {
    const compiled = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance.lock.yml', import.meta.url), 'utf8');
    const controller = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance-controller.yml', import.meta.url), 'utf8');
    const timeout = name => {
        const job = compiled.split(new RegExp(`^  ${name}:\\r?\\n`, 'm'))[1].split(/^  [a-z_]+:\r?\n/m)[0];
        return Number(job.match(/^    timeout-minutes: (\d+)/m)[1]);
    };
    const critical = ['prepare', 'native_analysis', 'agent', 'detection', 'safe_outputs']
        .reduce((sum, job) => sum + timeout(job), 5);
    const deadline = Number(controller.match(/const deadline = Date\.now\(\) \+ (\d+) \* 60 \* 1000/)[1]);
    const controllerBudget = Number(controller.match(/timeout-minutes: (\d+)/)[1]);
    assert.ok(deadline >= critical, 'controller must not cancel an in-budget native/review/push sequence');
    assert.ok(controllerBudget >= deadline + 10, 'reserve linked-report delivery headroom');
});

test('non-production scope produces explicit skipped evidence without running build tools', { skip: process.platform !== 'win32' }, t => {
    const f = fixture(t);
    const { result, metadata } = f.run({
        identity: { baseSha: 'a'.repeat(40), headSha: 'b'.repeat(40) },
        nativeAnalysis: { enabled: false, cpp: [], rust: false, reason: 'No production native changes.' },
    });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(metadata.status, 'skipped');
    assert.deepEqual(metadata.phases, []);
});

test('changed analyzer configuration is incomplete, not a clean comparison', { skip: process.platform !== 'win32' }, t => {
    const f = fixture(t);
    f.write('.cargo/config.toml', '[alias]\nwta-perf = "clippy"\n');
    const baseSha = f.commit('baseline');
    f.write('.cargo/config.toml', '[alias]\nwta-perf = "build"\n');
    const headSha = f.commit('changed analysis');
    const { result, metadata } = f.run({ identity: { baseSha, headSha },
        nativeAnalysis: { enabled: true, cpp: [], rust: true } });
    assert.equal(result.status, 1);
    assert.equal(metadata.status, 'incomplete');
    assert.match(metadata.reason, /configuration changed/);
    assert.deepEqual(metadata.phases, []);
});

test('unmapped C++ inputs cannot be reported as completed coverage', { skip: process.platform !== 'win32' }, t => {
    const f = fixture(t);
    f.write('src/no-owner.cpp', 'int value() { return 1; }\n');
    const baseSha = f.commit('baseline');
    f.write('src/no-owner.cpp', 'int value() { return 2; }\n');
    const headSha = f.commit('changed source');
    const { result, metadata } = f.run({ identity: { baseSha, headSha },
        nativeAnalysis: { enabled: true, cpp: ['src/no-owner.cpp'], rust: false } });
    assert.equal(result.status, 1);
    assert.equal(metadata.status, 'incomplete');
    assert.deepEqual(metadata.unmappedCpp, ['src/no-owner.cpp']);
    assert.match(metadata.reason, /no verified direct project membership/);
});

test('a checkout missing the reviewed head fetches that exact commit before analysis', { skip: process.platform !== 'win32' }, t => {
    const f = fixture(t);
    f.write('src/no-owner.cpp', 'int value() { return 1; }\n');
    const baseSha = f.commit('baseline');
    f.git(['branch', 'baseline', baseSha]);
    f.write('src/no-owner.cpp', 'int value() { return 2; }\n');
    const headSha = f.commit('reviewed head');
    const checkout = path.join(f.root, '.git', 'narrow-checkout');
    f.git(['clone', '--quiet', '--single-branch', '--branch', 'baseline', pathToFileURL(f.root).href, checkout]);
    assert.notEqual(spawnSync('git', ['cat-file', '-e', `${headSha}^{commit}`], { cwd: checkout }).status, 0);
    const { result, metadata } = f.run({ identity: { baseSha, headSha },
        nativeAnalysis: { enabled: true, cpp: ['src/no-owner.cpp'], rust: false } }, 'recovery-analysis', checkout);
    assert.equal(result.status, 1);
    assert.match(metadata.reason, /no verified direct project membership/);
    assert.ok(metadata.phases.some(phase => phase.command.join(' ') === `git fetch --no-tags origin ${headSha}` &&
        phase.exitCode === 0));
    assert.equal(spawnSync('git', ['cat-file', '-e', `${headSha}^{commit}`], { cwd: checkout }).status, 0);
});

test('a C++ coverage failure does not suppress valid Rust analysis',
    { skip: process.env.PERFORMANCE_NATIVE_PROOF !== '1' }, t => {
        const f = fixture(t);
        f.write('src/probe/Probe.vcxproj', '<Project><ItemGroup><ClCompile Include="Probe.cpp" /></ItemGroup></Project>');
        f.write('src/probe/Probe.cpp', 'int value() { return 1; }\n');
        f.write('tools/wta/Cargo.toml', '[package]\nname="partial-native-fixture"\nversion="0.1.0"\nedition="2024"\n');
        f.write('tools/wta/Cargo.lock', 'version = 4\n\n[[package]]\nname = "partial-native-fixture"\nversion = "0.1.0"\n');
        f.write('tools/wta/src/main.rs', 'fn main() {}\n');
        const baseSha = f.commit('baseline');
        f.write('tools/wta/src/main.rs', 'fn main() { println!("changed"); }\n');
        const headSha = f.commit('reviewed change');
        const { result, metadata, output } = f.run({ identity: { baseSha, headSha },
            nativeAnalysis: { enabled: true, cpp: ['src/probe/Probe.cpp'], rust: true } });
        assert.equal(result.status, 1);
        assert.equal(metadata.status, 'incomplete');
        assert.equal(metadata.failures.length, 2);
        for (const phase of ['base', 'head']) {
            assert.ok(fs.existsSync(path.join(output, `${phase}-rust.jsonl`)));
            assert.ok(metadata.phases.some(item => item.log === `${phase}-rust.jsonl` && item.exitCode === 0));
        }
    });

test('real Windows Clang-Tidy and Clippy analyze both immutable fixture revisions',
    { skip: process.env.PERFORMANCE_NATIVE_PROOF !== '1' }, t => {
        const f = fixture(t);
        const cpp = 'src/probe/Probe.cpp';
        const rust = 'tools/wta/src/main.rs';
        f.write('.gitignore', '**/x64/\n');
        // Use the owning repository's aliases; nested array-valued Cargo aliases concatenate.
        f.write('tools/wta/Cargo.toml', '[package]\nname="native-analysis-fixture"\nversion="0.1.0"\nedition="2024"\n');
        f.write('tools/wta/Cargo.lock', 'version = 4\n\n[[package]]\nname = "native-analysis-fixture"\nversion = "0.1.0"\n');
        const props = fs.readFileSync(new URL('../../../../src/common.build.pre.props', import.meta.url), 'utf8')
            .match(/  <PropertyGroup Condition="'\$\(PerformanceAnalysis\)'=='Extended'">[\s\S]*?<\/PropertyGroup>/)[0];
        f.write('src/probe/Performance.props', `<Project>${props}</Project>`);
        f.write('src/probe/Probe.vcxproj', `<Project DefaultTargets="Build">
  <ItemGroup Label="ProjectConfigurations"><ProjectConfiguration Include="AuditMode|x64"><Configuration>AuditMode</Configuration><Platform>x64</Platform></ProjectConfiguration></ItemGroup>
  <Import Project="$(VCTargetsPath)\\Microsoft.Cpp.Default.props" />
  <PropertyGroup Label="Configuration"><ConfigurationType>StaticLibrary</ConfigurationType><PlatformToolset>v145</PlatformToolset><WindowsTargetPlatformVersion>10.0.26100.0</WindowsTargetPlatformVersion></PropertyGroup>
  <Import Project="$(VCTargetsPath)\\Microsoft.Cpp.props" />
  <Import Project="Performance.props" />
  <ItemGroup><ClCompile Include="Probe.cpp" /></ItemGroup>
  <Import Project="$(VCTargetsPath)\\Microsoft.Cpp.targets" />
</Project>`);
        f.write(cpp, '#include <vector>\nstd::vector<int> copy(const std::vector<int>& input) { std::vector<int> result; result.reserve(input.size()); for (int n : input) result.push_back(n); return result; }\n');
        f.write(rust, 'fn main() { let v = vec![1,2]; println!("{v:?}"); }\n');
        const baseSha = f.commit('bounded baseline');
        f.write(cpp, '#include <vector>\nstd::vector<int> copy(const std::vector<int>& input) { std::vector<int> result; for (int n : input) result.push_back(n); return result; }\n');
        f.write(rust, 'fn main() { let mut v = Vec::new(); v.push(1); v.push(2); println!("{v:?}"); }\n');
        const headSha = f.commit('native diagnostic seeds');
        const { result, metadata, output } = f.run({ identity: { baseSha, headSha },
            nativeAnalysis: { enabled: true, cpp: [cpp], rust: true } });
        const evidence = path.join(process.cwd(), 'Generated Files', 'native-performance-validation');
        fs.mkdirSync(evidence, { recursive: true });
        for (const name of fs.readdirSync(output).filter(name => /\.(json|log|jsonl)$/.test(name))) {
            fs.copyFileSync(path.join(output, name), path.join(evidence, name));
        }
        assert.equal(result.status, 0, JSON.stringify(metadata) + result.stderr);
        assert.equal(metadata.status, 'completed');
        assert.ok(metadata.phases.every(phase => phase.exitCode === 0));
        const cppLog = fs.readFileSync(path.join(output, 'head-cpp-1.log'), 'utf8');
        assert.match(cppLog, /performance-inefficient-vector-operation/);
        assert.doesNotMatch(fs.readFileSync(path.join(output, 'base-cpp-1.log'), 'utf8'), /performance-inefficient-vector-operation\]/);
        const rustLog = fs.readFileSync(path.join(output, 'head-rust.jsonl'), 'utf8');
        assert.match(rustLog, /vec_init_then_push/);
        assert.doesNotMatch(fs.readFileSync(path.join(output, 'base-rust.jsonl'), 'utf8'), /vec_init_then_push/);
        const repeated = f.run({ identity: { baseSha, headSha },
            nativeAnalysis: { enabled: true, cpp: [cpp], rust: true } });
        assert.equal(repeated.result.status, 0, JSON.stringify(repeated.metadata));
        assert.equal(repeated.metadata.status, 'completed');
        const unexpected = path.join(output, 'source-head', '.cargo');
        fs.mkdirSync(unexpected);
        fs.writeFileSync(path.join(unexpected, 'config.toml'), '# unexpected analysis configuration\n');
        const rejected = f.run({ identity: { baseSha, headSha },
            nativeAnalysis: { enabled: true, cpp: [cpp], rust: true } });
        assert.equal(rejected.result.status, 1);
        assert.match(rejected.metadata.reason, /unexpected files/);
        f.write(cpp, 'this is not valid C++;\n');
        const invalidHead = f.commit('real compiler error');
        const invalid = f.run({ identity: { baseSha: headSha, headSha: invalidHead },
            nativeAnalysis: { enabled: true, cpp: [cpp], rust: true } }, 'invalid-source');
        assert.equal(invalid.result.status, 1);
        assert.equal(invalid.metadata.status, 'incomplete');
        assert.ok(invalid.metadata.phases.some(item => item.log === 'head-cpp-1.log' && item.exitCode !== 0));
        fs.copyFileSync(path.join(invalid.output, 'native-results.json'), path.join(evidence, 'invalid-source-native-results.json'));
    });
