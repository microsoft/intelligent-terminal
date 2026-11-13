import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import test from 'node:test';
import { validatePerformanceFiles, validatePerformanceResult } from '../../.github/workflows/ghaw-pr-performance/scripts/performance-ci.mjs';

const script = fileURLToPath(new URL('./Invoke-CiPerformanceAnalysis.ps1', import.meta.url));

function verifyNativeCompilerFixture(source, extraArguments, libraries, evidenceName) {
    const root = path.dirname(source);
    const name = path.basename(source, '.cpp');
    const exe = path.join(root, `${name}.exe`);
    execFileSync('cl.exe', ['/nologo', '/std:c++20', '/EHsc', ...extraArguments,
        `/Fe${exe}`, `/Fo${path.join(root, `${name}.obj`)}`, source, '/link', ...libraries],
    { cwd: root, encoding: 'utf8', timeout: 120000, stdio: 'pipe' });
    execFileSync(exe, [], { cwd: root, timeout: 15000, stdio: 'pipe' });
    const clang = path.join(process.env.VCINSTALLDIR, 'Tools', 'Llvm', 'x64', 'bin', 'clang-tidy.exe');
    const checked = spawnSync(clang, [source, '--checks=-*,performance-move-const-arg', '--extra-arg=-Wno-error',
        '--', '--driver-mode=cl', '/std:c++20', '/EHsc', ...extraArguments],
    { cwd: root, encoding: 'utf8', timeout: 120000 });
    assert.equal(checked.error, undefined);
    const evidence = path.join(process.cwd(), 'Generated Files', 'ci-performance-validation');
    fs.mkdirSync(evidence, { recursive: true });
    fs.writeFileSync(path.join(evidence, evidenceName), checked.stdout + checked.stderr);
    assert.equal(checked.status, 0, checked.stdout + checked.stderr);
}

test('dependency property defaults preserve values and compile with both MSVC and Clang',
    { skip: process.env.PERFORMANCE_NATIVE_PROOF !== '1' }, t => {
        const parent = process.env.PERFORMANCE_NATIVE_FIXTURE_ROOT ?? path.join(process.cwd(), 'Generated Files');
        fs.mkdirSync(parent, { recursive: true });
        const root = fs.mkdtempSync(path.join(parent, 'dependency-properties-'));
        t.after(() => fs.rmSync(root, { recursive: true, force: true }));
        fs.copyFileSync(new URL('../../src/cascadia/inc/WinrtDependencyProperty.h', import.meta.url),
            path.join(root, 'WinrtDependencyProperty.h'));
        const source = path.join(root, 'properties.cpp');
        fs.writeFileSync(source, `#include <cassert>
// This property test does not exercise DirectX intrinsics.
#define _XM_NO_INTRINSICS_
#include <winrt/Windows.Foundation.Collections.h>
#ifdef GetCurrentTime
#undef GetCurrentTime
#endif
#include <winrt/Windows.UI.Xaml.h>
#include "WinrtDependencyProperty.h"

using winrt::Windows::Foundation::IInspectable;
using winrt::Windows::UI::Xaml::DependencyProperty;
using winrt::Windows::UI::Xaml::Style;
using winrt::Windows::UI::Xaml::Visibility;
using Rows = winrt::Windows::Foundation::Collections::IVector<winrt::hstring>;

// No XAML objects are activated; the fake store exercises null and boxed values.
struct Properties
{
    IInspectable value{ nullptr };
    IInspectable GetValue(const DependencyProperty&) const { return value; }
    void SetValue(const DependencyProperty&, const IInspectable& next) { value = next; }
    DEPENDENCY_PROPERTY(winrt::hstring, Text);
    DEPENDENCY_PROPERTY(bool, Flag);
    DEPENDENCY_PROPERTY(int, Number);
    DEPENDENCY_PROPERTY(double, Opacity);
    DEPENDENCY_PROPERTY(Visibility, Display);
    DEPENDENCY_PROPERTY(IInspectable, Object);
    DEPENDENCY_PROPERTY(Style, Appearance);
    DEPENDENCY_PROPERTY(Rows, Items);
};
DependencyProperty Properties::_TextProperty{ nullptr };
DependencyProperty Properties::_FlagProperty{ nullptr };
DependencyProperty Properties::_NumberProperty{ nullptr };
DependencyProperty Properties::_OpacityProperty{ nullptr };
DependencyProperty Properties::_DisplayProperty{ nullptr };
DependencyProperty Properties::_ObjectProperty{ nullptr };
DependencyProperty Properties::_AppearanceProperty{ nullptr };
DependencyProperty Properties::_ItemsProperty{ nullptr };

struct AttachedProperties
{
    ATTACHED_DEPENDENCY_PROPERTY(winrt::hstring, Text);
    ATTACHED_DEPENDENCY_PROPERTY(bool, Flag);
    ATTACHED_DEPENDENCY_PROPERTY(Style, Appearance);
};
DependencyProperty AttachedProperties::_TextProperty{ nullptr };
DependencyProperty AttachedProperties::_FlagProperty{ nullptr };
DependencyProperty AttachedProperties::_AppearanceProperty{ nullptr };

int main()
{
    Properties p;
    assert(p.Text().empty());
    assert(!p.Flag());
    assert(p.Number() == 0);
    assert(p.Opacity() == 0.0);
    assert(p.Display() == Visibility::Visible);
    assert(!p.Object());
    assert(!p.Appearance());
    assert(!p.Items());
    p.Number(42);
    assert(p.Number() == 42);
    p.Flag(true);
    assert(p.Flag());
    p.Text(L"preserved");
    assert(p.Text() == L"preserved");
}
`);
        const sdk = path.join(process.env.WindowsSdkDir, 'Include', '10.0.26100.0', 'cppwinrt');
        verifyNativeCompilerFixture(source, [`/I${sdk}`], ['WindowsApp.lib'], 'dependency-property-clang.log');
    });

test('tab-count telemetry compiles with both MSVC and Clang',
    { skip: process.env.PERFORMANCE_NATIVE_PROOF !== '1' }, t => {
        const parent = process.env.PERFORMANCE_NATIVE_FIXTURE_ROOT ?? path.join(process.cwd(), 'Generated Files');
        fs.mkdirSync(parent, { recursive: true });
        const root = fs.mkdtempSync(path.join(parent, 'tab-telemetry-'));
        t.after(() => fs.rmSync(root, { recursive: true, force: true }));
        const page = fs.readFileSync(new URL('../../src/cascadia/TerminalApp/TerminalPage.cpp', import.meta.url), 'utf8');
        const fields = [...page.matchAll(/TraceLogging(?:Value|UInt32)\((?:this->)?NumberOfTabs\(\), "[^"]+", "[^"]+"\)/g)];
        assert.equal(fields.length, 4, 'Exercise every direct tab-count telemetry call site.');
        const source = path.join(root, 'telemetry.cpp');
        fs.writeFileSync(source, `#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <TraceLoggingProvider.h>
#include <cassert>
#include <cstdint>

TRACELOGGING_DEFINE_PROVIDER(provider, "TabTelemetryRegression",
    (0xa8c2c725, 0x8e45, 0x4633, 0x96, 0xeb, 0x43, 0xc8, 0xf1, 0x22, 0xb6, 0xf9));

struct TabCounter
{
    mutable unsigned reads{};
    uint32_t NumberOfTabs() const { ++reads; return 7; }
    void Emit()
    {
${fields.map(([field]) => `        TraceLoggingWrite(provider, "TabCount", ${field});`).join('\n')}
    }
};

int main()
{
    TabCounter counter;
    counter.Emit();
    assert(counter.reads == 0); // Disabled telemetry must not evaluate the payload.
}
`);
        verifyNativeCompilerFixture(source, [], ['advapi32.lib'], 'tab-telemetry-clang.log');
    });

test('property registration declaration remains lazy and initializes once with both compilers',
    { skip: process.env.PERFORMANCE_NATIVE_PROOF !== '1' }, t => {
        const parent = process.env.PERFORMANCE_NATIVE_FIXTURE_ROOT ?? path.join(process.cwd(), 'Generated Files');
        fs.mkdirSync(parent, { recursive: true });
        const root = fs.mkdtempSync(path.join(parent, 'property-registration-'));
        t.after(() => fs.rmSync(root, { recursive: true, force: true }));
        const control = fs.readFileSync(new URL('../../src/cascadia/TerminalApp/HighlightedTextControl.cpp', import.meta.url), 'utf8');
        const declaration = control.match(/^\s*(?:\[\[maybe_unused\]\]\s*)?static auto(?:\s*\[\[maybe_unused\]\])? registered = \[\] \{/m);
        assert.ok(declaration, 'Exercise the actual property-registration declaration.');
        const source = path.join(root, 'registration.cpp');
        fs.writeFileSync(source, `#include <cassert>
unsigned registrations{};
void Initialize()
{
${declaration[0]}
        ++registrations;
        return true;
    }();
}
int main()
{
    assert(registrations == 0);
    Initialize();
    Initialize();
    assert(registrations == 1);
}
`);
        verifyNativeCompilerFixture(source, [], [], 'property-registration-clang.log');
    });

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
        write('tools/wta/src/main.rs', `fn style_only(value: usize) -> usize { return value; }
fn copy_nonempty(rows: &[String]) -> Vec<String> {
    rows.iter().cloned().filter(|row| !row.is_empty()).collect()
}
fn main() {
    let mut v=Vec::new(); v.push(1); v.push(2);
    let count=std::env::args().collect::<Vec<_>>().len();
    println!("{v:?} {} {:?}", style_only(count), copy_nonempty(&[String::from("a")]));
}
`);
        const head = commit();
        execFileSync('MSBuild.exe', ['src\\probe\\Probe.vcxproj', '/nologo', '/t:Build',
            '/p:Configuration=Debug', '/p:Platform=x64'], { cwd: root, stdio: 'pipe', timeout: 120000 });
        for (const language of ['cpp', 'rust']) {
            const proof = run(language, head, base);
            assert.equal(proof.result.status, 0, proof.result.stderr + JSON.stringify(proof.receipt) +
                fs.readFileSync(path.join(proof.output, language === 'cpp' ? 'diagnostics-1.log' : 'diagnostics.jsonl'), 'utf8'));
            assert.equal(proof.receipt.status, 'completed');
            assert.equal(proof.receipt.headSha, head);
            validatePerformanceResult(proof.receipt, language,
                { headSha: head, runId: '99', runAttempt: 2, comparisonBaseSha: base });
            validatePerformanceFiles(proof.output, language,
                { headSha: head, runId: '99', runAttempt: 2, comparisonBaseSha: base });
            if (language === 'cpp') {
                assert.equal(proof.receipt.phases[1].stderrLog, 'project-1-stderr.log');
                assert.ok(fs.existsSync(path.join(proof.output, 'project-1-stderr.log')));
            }
            assert.match(fs.readFileSync(path.join(proof.output, language === 'cpp' ? 'diagnostics-1.log' : 'diagnostics.jsonl'), 'utf8'),
                language === 'cpp' ? /performance-inefficient-vector-operation/ : /vec_init_then_push/);
            if (language === 'rust') {
                const diagnostics = fs.readFileSync(path.join(proof.output, 'diagnostics.jsonl'), 'utf8');
                assert.match(diagnostics, /clippy::iter_overeager_cloned/);
                assert.match(diagnostics, /clippy::needless_collect/);
                assert.doesNotMatch(diagnostics, /clippy::needless_return/);
            }
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
        write('tools/wta/src/main.rs', 'fn main() { let count=std::env::args().len(); println!("{}", count == count); }\n');
        const incorrectHead = commit();
        const incorrect = run('rust', incorrectHead, base);
        assert.equal(incorrect.result.status, 1, JSON.stringify(incorrect.receipt));
        assert.equal(incorrect.receipt.status, 'incomplete');
        assert.match(fs.readFileSync(path.join(incorrect.output, 'diagnostics.jsonl'), 'utf8'), /clippy::eq_op/);
        save(incorrect, 'rust-correctness-errors');
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
