import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import test from 'node:test';
import { validatePerformanceFiles, validatePerformanceResult } from '../../.github/workflows/ghaw-pr-performance/scripts/performance-ci.mjs';

const script = fileURLToPath(new URL('./Invoke-CiPerformanceAnalysis.ps1', import.meta.url));

test('real CI profiles preserve performance warnings but reject actual compilation errors',
    { skip: process.env.PERFORMANCE_NATIVE_PROOF !== '1' }, t => {
        const parent = process.env.PERFORMANCE_NATIVE_FIXTURE_ROOT ?? path.join(process.cwd(), 'Generated Files');
        fs.mkdirSync(parent, { recursive: true });
        const root = fs.mkdtempSync(path.join(parent, 'ci-native-'));
        t.after(() => fs.rmSync(root, { recursive: true, force: true }));
        const git = args => execFileSync('git', args, { cwd: root, encoding: 'utf8' }).trim();
        const write = (name, value) => {
            fs.mkdirSync(path.dirname(path.join(root, name)), { recursive: true });
            fs.writeFileSync(path.join(root, name), value);
        };
        const commit = () => {
            git(['add', '.']);
            git(['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '--quiet', '-m', 'fixture']);
            return git(['rev-parse', 'HEAD']);
        };
        const run = (language, head, base, cwd = root) => {
            const output = path.join(root, '.git', `${language}-${head}`);
            const result = spawnSync('pwsh', ['-NoLogo', '-NoProfile', '-File', script,
                '-Language', language, '-OutputDirectory', output], {
                cwd, encoding: 'utf8', timeout: 180000,
                env: { ...process.env, EXPECTED_HEAD_SHA: head, PR_COMPARISON_BASE_SHA: base,
                    GITHUB_RUN_ID: '99', GITHUB_RUN_ATTEMPT: '2', RUSTUP_TOOLCHAIN: '1.93.0',
                    CARGO_TARGET_DIR: path.join(root, '.git', 'target') },
            });
            assert.equal(result.error, undefined);
            return { result, output, receipt: JSON.parse(fs.readFileSync(path.join(output, 'native-results.json'), 'utf8').replace(/^\uFEFF/, '')) };
        };
        const save = (proof, name) => fs.cpSync(proof.output,
            path.join(process.cwd(), 'Generated Files', 'ci-performance-validation', 'native', name), { recursive: true });
        git(['init', '--quiet', '--initial-branch=main']);
        write('.gitignore', '**/x64/\n');
        const profile = fs.readFileSync(new URL('../../src/common.build.pre.props', import.meta.url), 'utf8')
            .match(/  <PropertyGroup Condition="'\$\(PerformanceAnalysis\)'=='Extended'">[\s\S]*?<\/PropertyGroup>/)[0];
        write('src/probe/Performance.props', `<Project>${profile}</Project>`);
        write('src/probe/Probe.vcxproj', `<Project DefaultTargets="Build">
  <ItemGroup Label="ProjectConfigurations"><ProjectConfiguration Include="Debug|x64"><Configuration>Debug</Configuration><Platform>x64</Platform></ProjectConfiguration></ItemGroup>
  <Import Project="$(VCTargetsPath)\\Microsoft.Cpp.Default.props" />
  <PropertyGroup Label="Configuration"><ConfigurationType>StaticLibrary</ConfigurationType><PlatformToolset>v145</PlatformToolset><WindowsTargetPlatformVersion>10.0.26100.0</WindowsTargetPlatformVersion></PropertyGroup>
  <Import Project="$(VCTargetsPath)\\Microsoft.Cpp.props" />
  <Import Project="Performance.props" />
  <ItemGroup><ClCompile Include="Probe.cpp" /></ItemGroup>
  <Import Project="$(VCTargetsPath)\\Microsoft.Cpp.targets" />
</Project>`);
        write('src/probe/Probe.cpp', '#include <vector>\nstd::vector<int> copy(const std::vector<int>& v) { return v; }\n');
        write('tools/wta/Cargo.toml', `[package]
name="ci-native-fixture"
version="0.1.0"
edition="2024"
`);
        write('tools/wta/Cargo.lock', `version = 4

[[package]]
name="ci-native-fixture"
version="0.1.0"
`);
        write('tools/wta/src/main.rs', 'fn main() {}\n');
        const base = commit();
        write('src/probe/Probe.cpp', '#include <vector>\nstd::vector<int> copy(const std::vector<int>& v) { std::vector<int> r; for (int n : v) r.push_back(n); return r; }\n');
        write('tools/wta/src/main.rs', 'fn main() { let mut v=Vec::new(); v.push(1); v.push(2); println!("{v:?}"); }\n');
        const head = commit();
        execFileSync('MSBuild.exe', ['src\\probe\\Probe.vcxproj', '/nologo', '/t:Build',
            '/p:Configuration=Debug', '/p:Platform=x64'], { cwd: root, stdio: 'pipe', timeout: 120000 });
        for (const language of ['cpp', 'rust']) {
            const proof = run(language, head, base);
            assert.equal(proof.result.status, 0, proof.result.stderr + JSON.stringify(proof.receipt));
            assert.equal(proof.receipt.status, 'completed');
            assert.equal(proof.receipt.headSha, head);
            validatePerformanceResult(proof.receipt, language,
                { headSha: head, runId: '99', runAttempt: 2, comparisonBaseSha: base });
            validatePerformanceFiles(proof.output, language,
                { headSha: head, runId: '99', runAttempt: 2, comparisonBaseSha: base });
            assert.match(fs.readFileSync(path.join(proof.output, language === 'cpp' ? 'diagnostics-1.log' : 'diagnostics.jsonl'), 'utf8'),
                language === 'cpp' ? /performance-inefficient-vector-operation/ : /vec_init_then_push/);
            save(proof, `${language}-warnings`);
        }
        const shallow = path.join(root, '.git', 'shallow-checkout');
        git(['clone', '--quiet', '--depth=1', pathToFileURL(root).href, shallow]);
        const shallowGit = args => execFileSync('git', args, { cwd: shallow, encoding: 'utf8' }).trim();
        assert.equal(shallowGit(['rev-parse', '--is-shallow-repository']), 'true');
        shallowGit(['fetch', '--quiet', '--no-tags', '--depth=1', 'origin', base]);
        const shallowProof = run('rust', head, base, shallow);
        assert.equal(shallowProof.result.status, 0, shallowProof.result.stderr);
        validatePerformanceResult(shallowProof.receipt, 'rust',
            { headSha: head, runId: '99', runAttempt: 2, comparisonBaseSha: base });
        save(shallowProof, 'rust-shallow-checkout');
        write('src/probe/Probe.cpp', 'not valid C++;\n');
        write('tools/wta/src/main.rs', 'not valid Rust;\n');
        const invalidHead = commit();
        for (const language of ['cpp', 'rust']) {
            const proof = run(language, invalidHead, base);
            assert.equal(proof.result.status, 1, JSON.stringify(proof.receipt));
            assert.equal(proof.receipt.status, 'incomplete');
            assert.ok(proof.receipt.phases.some(phase => phase.exitCode !== 0));
            assert.throws(() => validatePerformanceResult(proof.receipt, language,
                { headSha: invalidHead, runId: '99', runAttempt: 2, comparisonBaseSha: base }));
            save(proof, `${language}-errors`);
        }
        write('README.md', 'Documentation-only follow-up.\n');
        const docsHead = commit();
        for (const language of ['cpp', 'rust']) {
            const proof = run(language, docsHead, invalidHead);
            assert.equal(proof.result.status, 0, proof.result.stderr);
            assert.equal(proof.receipt.status, 'skipped');
            assert.deepEqual(proof.receipt.phases, []);
            validatePerformanceResult(proof.receipt, language,
                { headSha: docsHead, runId: '99', runAttempt: 2, comparisonBaseSha: invalidHead });
            save(proof, `${language}-skipped`);
        }
    });
