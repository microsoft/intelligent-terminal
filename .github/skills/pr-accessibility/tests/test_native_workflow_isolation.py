"""Guard the reusable native workflow's credential and identity boundary."""
from pathlib import Path
import json
import re
import subprocess
import tempfile
import textwrap
import unittest
import zipfile


ROOT = Path(__file__).parents[4]


class TrustedHarnessIdentityTests(unittest.TestCase):
    def verify(self, expected):
        workflow = (ROOT / ".github/workflows/native-accessibility.yml").read_text(encoding="utf-8")
        step = workflow.split("      - name: Verify immutable trusted harness\n", 1)[1]
        script = textwrap.dedent(step.split("        run: |\n", 1)[1].split("      - name:", 1)[0])
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            harness = root / "trusted-accessibility"
            harness.mkdir()
            for args in (["init", "-q"], ["config", "user.name", "Test"],
                         ["config", "user.email", "test@example.com"],
                         ["commit", "--allow-empty", "-qm", "Trusted fixture"]):
                subprocess.run(["git", *args], cwd=harness, check=True, capture_output=True, timeout=15)
            sha = subprocess.run(["git", "rev-parse", "HEAD"], cwd=harness, check=True,
                                 capture_output=True, text=True, timeout=15).stdout.strip()
            runner = root / "verify.ps1"
            runner.write_text(f"$env:EXPECTED_TRUSTED_SHA = '{sha if expected is None else expected}'\n" + script,
                              encoding="utf-8")
            return subprocess.run(["pwsh", "-NoProfile", "-File", str(runner)], cwd=root,
                                  capture_output=True, text=True, timeout=30)

    def test_accepts_the_exact_checked_out_commit(self):
        result = self.verify(None)
        self.assertEqual(0, result.returncode, result.stderr)

    def test_rejects_a_moving_ref(self):
        result = self.verify("main")
        self.assertNotEqual(0, result.returncode)
        self.assertIn("exact 40-character", result.stderr)

    def test_rejects_a_different_commit(self):
        result = self.verify("0" * 40)
        self.assertNotEqual(0, result.returncode)
        self.assertIn("does not match", result.stderr)


class FrameworkDependencyInstallationTests(unittest.TestCase):
    publisher = "CN=Microsoft Corporation, O=Microsoft Corporation, L=Redmond, S=Washington, C=US"

    def install(self, packages, *, already_installed=False):
        workflow = (ROOT / ".github/workflows/native-accessibility.yml").read_text(encoding="utf-8")
        step = workflow.split("      - name: Install test host framework dependencies\n", 1)[1]
        script = textwrap.dedent(step.split("        run: |\n", 1)[1].split("      - name: Scan FRE\n", 1)[0])
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            helper_dir = root / "trusted-accessibility/build/scripts"
            package_dir = helper_dir / "packages"
            package_dir.mkdir(parents=True)
            (helper_dir / "Get-DependenciesFromAppxRecipe.ps1").write_text(
                "param([string]$Path)\nGet-ChildItem -LiteralPath (Join-Path $PSScriptRoot 'packages') -Filter '*.appx'\n",
                encoding="utf-8",
            )
            for index, package in enumerate(packages):
                identity = {
                    "name": "Microsoft.UI.Xaml.2.8", "publisher": self.publisher,
                    "architecture": "x64", "framework": "true", **package,
                }
                manifest = (
                    f'<Package><Identity Name="{identity["name"]}" Publisher="{identity["publisher"]}" '
                    f'ProcessorArchitecture="{identity["architecture"]}" Version="14.0.0.0" />'
                    f'<Properties><Framework>{identity["framework"]}</Framework></Properties></Package>'
                )
                with zipfile.ZipFile(package_dir / f"framework-{index}.appx", "w") as archive:
                    archive.writestr("AppxManifest.xml", manifest)
            installed_flag = "$true" if already_installed else "$false"
            prefix = f"""
$script:installs = [Collections.Generic.List[string]]::new()
$script:alreadyInstalled = {installed_flag}
function Get-AppxPackage {{
    param([string]$Name)
    if ($script:alreadyInstalled) {{
        [pscustomobject]@{{ Name=$Name; Architecture='x64'; Version='99.0.0.0'; Publisher='{self.publisher}' }}
    }}
}}
function Add-AppxPackage {{
    [CmdletBinding()]
    param([string]$Path)
    $script:installs.Add($Path)
}}
$failure = $null
try {{
"""
            suffix = """
} catch { $failure = $_.Exception.Message }
@{ installed=@($script:installs.ToArray()); failure=$failure } | ConvertTo-Json -Compress
if ($failure) { exit 1 }
"""
            runner = root / "install.ps1"
            runner.write_text(prefix + script + suffix, encoding="utf-8")
            result = subprocess.run(
                ["pwsh", "-NoProfile", "-File", str(runner)], cwd=root,
                capture_output=True, text=True, timeout=30,
            )
            self.assertTrue(result.stdout.strip(), result.stderr)
            return result.returncode, json.loads(result.stdout)

    def test_installs_only_expected_x64_microsoft_frameworks(self):
        code, result = self.install([{}, {"name": "Microsoft.VCLibs.140.00.Debug"}])
        self.assertEqual(0, code, result)
        self.assertEqual(2, len(result["installed"]))

    def test_rejects_unexpected_identity_architecture_or_nonframework_before_install(self):
        for package in (
            {"name": "Unexpected.App"},
            {"publisher": "CN=Other publisher"},
            {"architecture": "x86"},
            {"framework": "false"},
        ):
            with self.subTest(package=package):
                code, result = self.install([package])
                self.assertNotEqual(0, code)
                self.assertEqual([], result["installed"])
                self.assertIn("not an expected Microsoft x64", result["failure"])

    def test_newer_installed_framework_is_not_downgraded(self):
        code, result = self.install([{}], already_installed=True)
        self.assertEqual(0, code, result)
        self.assertEqual([], result["installed"])

    def test_installed_package_does_not_bypass_identity_guard(self):
        code, result = self.install([{"name": "Unexpected.App"}], already_installed=True)
        self.assertNotEqual(0, code)
        self.assertEqual([], result["installed"])


class NativeWorkflowIsolationTests(unittest.TestCase):
    def test_windows_discovery_is_wired_without_building_in_the_source_agent(self):
        compiled = (ROOT / ".github/workflows/ghaw-pr-accessibility.lock.yml").read_text(encoding="utf-8")
        self.assertIn("\n  native-runtime:\n", compiled)
        self.assertIn("uses: ./.github/workflows/native-accessibility.yml", compiled)
        self.assertNotIn("call tools\\razzle.cmd", compiled)

    def test_problem_discovery_does_not_gate_or_retest_repair_publication(self):
        compiled = (ROOT / ".github/workflows/ghaw-pr-accessibility.lock.yml").read_text(encoding="utf-8")
        publication = re.split(r"\n  [a-zA-Z][\w-]*:\n", compiled.split("\n  safe_outputs:\n", 1)[1], maxsplit=1)[0]
        self.assertNotIn("native-runtime", publication)
        discovery = re.split(r"\n  [a-zA-Z][\w-]*:\n", compiled.split("\n  native-runtime:\n", 1)[1], maxsplit=1)[0]
        self.assertRegex(discovery, r"(?m)^    needs:\n      - agent\n      - native-selection$")
        self.assertIn("head-sha: ${{ github.event.pull_request.head.sha }}", discovery)
        self.assertNotIn("published", discovery)
        self.assertNotIn("safe_outputs", discovery)

    def test_reusable_native_definition_has_no_telemetry_or_secret_references(self):
        native = (ROOT / ".github/workflows/native-accessibility.yml").read_text(encoding="utf-8")
        self.assertIn("workflow_call:", native)
        self.assertNotIn("secrets:", native)
        self.assertNotIn("secrets.", native)
        self.assertNotIn("OTEL_EXPORTER_OTLP_HEADERS", native)
        self.assertNotIn("GH_AW_OTLP_ENDPOINTS", native)
        self.assertNotIn("environment:", native)
        self.assertNotIn(": write", native)

    def test_native_discovery_receives_four_immutable_identities(self):
        caller = (ROOT / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        native = (ROOT / ".github/workflows/native-accessibility.yml").read_text(encoding="utf-8")
        for key in ("head-repository", "head-sha", "trusted-repository", "trusted-sha"):
            self.assertIn(f"      {key}:", caller)
            self.assertRegex(native, rf"{key}:\n\s+required: true\n\s+type: string")
            self.assertIn(f"${{{{ inputs.{key} }}}}", native)
        self.assertNotIn("github.event.pull_request", native)

    def test_agents_host_uses_production_settings_navigation(self):
        host = (ROOT / "src/cascadia/LocalTests_TerminalApp/TestHostApp/UnitTestApp.xaml.cpp").read_text(encoding="utf-8")
        self.assertIn("frame->Navigate(", host)
        self.assertIn("ref new Microsoft::Terminal::Settings::Editor::AIAgentsViewModel(settings->GlobalSettings)", host)
        self.assertIn("ref new Microsoft::Terminal::Settings::Editor::NavigateToPageArgs(viewModel, nullptr", host)
        self.assertIn("Microsoft::Terminal::Settings::Editor::AIAgents::typeid", host)
        self.assertNotIn("ref new Microsoft::Terminal::Settings::Editor::MainPage(", host)
        self.assertIn("CoreDispatcherPriority::Normal", host)
        self.assertNotIn("ref new Microsoft::Terminal::Settings::Editor::AIAgents(", host)
        header = (ROOT / "src/cascadia/TerminalSettingsEditor/AIAgents.h").read_text(encoding="utf-8")
        self.assertNotIn("AIAgents(Model::GlobalAppSettings", header)
        navigation = (ROOT / "src/cascadia/TerminalSettingsEditor/MainPage.idl").read_text(encoding="utf-8")
        self.assertIn("NavigateToPageArgs(IInspectable viewModel, IHostedInWindow windowRoot, String elementToFocus);",
                      navigation)

    def test_native_step_execution_is_not_duplicated_in_the_privileged_caller(self):
        caller = (ROOT / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        self.assertNotIn("call tools\\razzle.cmd", caller)
        self.assertNotIn("-Surface fre-settings", caller)
        native = (ROOT / ".github/workflows/native-accessibility.yml").read_text(encoding="utf-8")
        self.assertIn("call tools\\razzle.cmd", native)
        for surface in ("fre", "fre-settings", "agents"):
            self.assertIn(f"-Surface {surface}", native)
        self.assertRegex(native, r"timeout-minutes: \d+")

    def test_instruction_and_configuration_trigger_scope_is_complete(self):
        caller = (ROOT / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        trigger = caller.split("\npermissions:", 1)[0]
        for pattern in (".github/**", "**/.github/**", "**/AGENTS.md", "**/CLAUDE.md",
                        "**/GEMINI.md", "**/copilot-instructions.md", "**/SKILL.md",
                        "**/*.instructions.md", "**/*.agent.md", "**/.mcp.json",
                        "**/.claude/**", "**/.copilot/**", "**/.agents/**", "**/.gemini/**"):
            self.assertIn(f"'{pattern}'", trigger)
        self.assertIn("'.github/workflows/native-accessibility.yml'", trigger)


if __name__ == "__main__":
    unittest.main()
