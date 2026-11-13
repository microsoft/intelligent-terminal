"""Guard the reusable native workflow's credential and identity boundary."""
from pathlib import Path
import json
import os
import re
import subprocess
import tempfile
import textwrap
import unittest
import zipfile
import yaml


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


class NativeCiIdentityTests(unittest.TestCase):
    def test_actual_windows_step_binds_event_source_base_and_repository(self):
        workflow = yaml.safe_load((ROOT / ".github/workflows/native-accessibility.yml").read_text(encoding="utf-8"))
        step = next(s for s in workflow["jobs"]["native-smoke"]["steps"]
                    if s.get("name") == "Verify immutable consumer")
        environment = {**os.environ, "GITHUB_EVENT_NAME": "pull_request",
                       "GITHUB_REPOSITORY_ID": "42", "PR_REPOSITORY_ID": "42",
                       "EXPECTED_TRUSTED_SHA": "c" * 40, "TRUSTED_HARNESS_SHA": "c" * 40,
                       "PR_BASE_SHA": "b" * 40, "EXPECTED_HEAD_SHA": "a" * 40, "PR_HEAD_SHA": "a" * 40}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "verify.ps1"
            path.write_text("function git { '" + "c" * 40 + "' }\n" + step["run"], encoding="utf-8")
            for key, value in ((None, None), ("GITHUB_EVENT_NAME", "workflow_dispatch"),
                               ("PR_REPOSITORY_ID", "43"), ("EXPECTED_HEAD_SHA", "d" * 40),
                               ("TRUSTED_HARNESS_SHA", "d" * 40), ("EXPECTED_TRUSTED_SHA", "d" * 40)):
                with self.subTest(key=key):
                    result = subprocess.run(["pwsh", "-NoProfile", "-File", str(path)],
                                            env={**environment, **({key: value} if key else {})},
                                            capture_output=True, text=True, timeout=30)
                    self.assertEqual(key is None, result.returncode == 0, result.stderr)


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
$env:GITHUB_WORKSPACE = $PWD.Path
$env:ACCESSIBILITY_PAYLOAD = $PWD.Path
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
    def test_worker_consumes_ci_evidence_without_windows_or_builds(self):
        compiled = (ROOT / ".github/workflows/ghaw-pr-accessibility.lock.yml").read_text(encoding="utf-8")
        self.assertIn("\n  native-evidence:\n", compiled)
        self.assertNotIn("uses: ./.github/workflows/native-accessibility.yml", compiled)
        self.assertNotIn("windows-", compiled)
        self.assertNotIn("call tools\\razzle.cmd", compiled)

    def test_problem_discovery_does_not_gate_or_retest_repair_publication(self):
        compiled = (ROOT / ".github/workflows/ghaw-pr-accessibility.lock.yml").read_text(encoding="utf-8")
        publication = re.split(r"\n  [a-zA-Z][\w-]*:\n", compiled.split("\n  safe_outputs:\n", 1)[1], maxsplit=1)[0]
        self.assertNotIn("native-runtime", publication)
        discovery = re.split(r"\n  [a-zA-Z][\w-]*:\n", compiled.split("\n  native-evidence:\n", 1)[1], maxsplit=1)[0]
        self.assertNotIn("- native-selection", discovery)
        self.assertNotIn("- safe_outputs", discovery)
        self.assertIn("EXPECTED_HEAD_SHA: ${{ fromJSON(github.event.inputs.pull_request).head.sha }}", discovery)
        self.assertIn("require-native: \"true\"", discovery)
        self.assertNotIn("push_commit_sha", discovery)

    def test_reusable_native_definition_has_no_telemetry_or_secret_references(self):
        native = (ROOT / ".github/workflows/native-accessibility.yml").read_text(encoding="utf-8")
        self.assertIn("workflow_call:", native)
        self.assertNotIn("secrets:", native)
        self.assertNotIn("secrets.", native)
        self.assertNotIn("OTEL_EXPORTER_OTLP_HEADERS", native)
        self.assertNotIn("GH_AW_OTLP_ENDPOINTS", native)
        self.assertNotIn("environment:", native)
        self.assertNotIn(": write", native)

    def test_native_ci_discovery_receives_immutable_identities(self):
        caller = (ROOT / ".github/workflows/pr-ci.yml").read_text(encoding="utf-8")
        native = (ROOT / ".github/workflows/native-accessibility.yml").read_text(encoding="utf-8")
        for key in ("head-sha", "trusted-sha"):
            self.assertIn(f"      {key}:", caller)
            self.assertRegex(native, rf"{key}:\n(?:\s+description:.*\n)?\s+required: true\n\s+type: string")
            self.assertIn(f"${{{{ inputs.{key} }}}}", native)
        self.assertIn("trusted-sha: ${{ github.workflow_sha }}", caller)
        self.assertIn("head-sha: ${{ github.event.pull_request.head.sha }}", caller)
        self.assertIn("$env:TRUSTED_HARNESS_SHA -ne $env:EXPECTED_TRUSTED_SHA", native)
        self.assertIn("$env:EXPECTED_HEAD_SHA -ne $env:PR_HEAD_SHA", native)
        self.assertIn("$env:PR_REPOSITORY_ID -ne $env:GITHUB_REPOSITORY_ID", native)
        self.assertNotIn("github.event.inputs.pull_request", native)

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
        self.assertNotIn("call tools\\razzle.cmd", native)
        self.assertNotIn("call bx", native)
        self.assertIn("ci-accessibility-payload.py extract", native)
        for surface in ("fre", "fre-settings", "agents", "sidebar"):
            self.assertIn(f"-Surface {surface}", native)
        self.assertRegex(native, r"timeout-minutes: \d+")

    def test_sidebar_uses_existing_production_route_and_visible_tree_rows(self):
        launch = (ROOT / "test/accessibility/Invoke-AxeWindowsTestHost.ps1").read_text(encoding="utf-8")
        scan = (ROOT / "test/accessibility/Invoke-AxeWindowsScan.ps1").read_text(encoding="utf-8")
        host = (ROOT / "src/cascadia/LocalTests_TerminalApp/TestHostApp/UnitTestApp.xaml.cpp").read_text(encoding="utf-8")
        fixture = (ROOT / "src/cascadia/LocalTests_TerminalApp/SidebarVisualFixture.cpp").read_text(encoding="utf-8")
        self.assertIn("if ($Surface -eq 'sidebar') { '--visual-page=sidebar' }", launch)
        self.assertIn("$launchArgument)", launch)
        self.assertIn('L"--visual-page=sidebar"', host)
        self.assertIn('"InitializeSidebarVisualFixture"', host)
        for source in ('winrt::TerminalApp::TabStrip strip', 'winrt::TerminalApp::TabRowControl chrome',
                       'L"FixtureSidebar"', 'strip.TabItems().Append(tab)'):
            self.assertIn(source, fixture)
        for assertion in ("'sidebar' { 'FixtureSidebar' }", "$marker.FindFirst(",
                          "'ItemsList'", "ControlType]::TreeItem", "$_.Current.ProcessId -eq $ProcessId",
                          "$_.Current.Name -eq 'PowerShell'", "$_.Current.AutomationId -ne 'HistoryHeaderButton'",
                          "if ($rows.Count -eq 0) { throw"):
            self.assertIn(assertion, scan)
        self.assertIn("warning_coverage = 'Unknown'", scan)
        self.assertIn("needs_review_coverage = 'Unknown'", scan)

    def test_instruction_and_configuration_trigger_scope_is_complete(self):
        caller = (ROOT / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        self.assertIn("uses: ./.github/actions/accessibility-ci-admission", caller)
        scope = (ROOT / "build/scripts/ci-accessibility-scope.mjs").read_text(encoding="utf-8")
        admission = (ROOT / ".github/actions/accessibility-ci-admission/admission.mjs").read_text(encoding="utf-8")
        producer = (ROOT / ".github/workflows/pr-ci.yml").read_text(encoding="utf-8")
        self.assertIn("export function relevantPath", scope)
        self.assertIn("build/scripts/ci-accessibility-scope.mjs", admission)
        self.assertIn("build/scripts/ci-accessibility-scope.mjs", producer)

    def test_actual_sidebar_row_predicate_rejects_history_header_only_without_a_desktop(self):
        scan = (ROOT / "test/accessibility/Invoke-AxeWindowsScan.ps1").read_text(encoding="utf-8")
        predicate = scan.split("Where-Object { ", 1)[1].split(" } |", 1)[0]
        current = {"ProcessId": 45, "IsOffscreen": False, "Name": "PowerShell",
                   "ClassName": "Microsoft.UI.Xaml.Controls.TreeViewItem", "AutomationId": ""}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "row-predicate.ps1"
            for mode, edits, expected in (
                ("live-tab", {}, True),
                ("history-header-only", {"Name": "Recent agent sessions", "AutomationId": "HistoryHeaderButton"}, False),
                ("spoofed-header-name", {"AutomationId": "HistoryHeaderButton"}, False),
                ("wrong-row-class", {"ClassName": "Button"}, False),
                ("hidden-row", {"IsOffscreen": True}, False),
                ("foreign-row", {"ProcessId": 46}, False),
            ):
                row = json.dumps({"Current": {**current, **edits}})
                path.write_text(
                    "$ProcessId = 45\n$row = ConvertFrom-Json @'\n" + row + "\n'@\n" +
                    "$accepted = @($row | Where-Object { " + predicate + " }).Count -gt 0\n" +
                    f"if ($accepted -ne ${str(expected).lower()}) {{ throw 'Wrong row admission' }}\n",
                    encoding="utf-8")
                with self.subTest(mode=mode):
                    result = subprocess.run(["pwsh", "-NoProfile", "-File", str(path)],
                                            capture_output=True, text=True, timeout=30)
                    self.assertEqual(0, result.returncode, result.stderr)

    def test_actual_sidebar_selection_guards_require_native_single_selection_and_row_owner(self):
        # Execute the production function/condition, with UIA calls represented
        # by snapshots. This is portable predicate coverage, not a native scan.
        scan_path = ROOT / "test/accessibility/Invoke-AxeWindowsScan.ps1"
        script = r"""
$ErrorActionPreference = 'Stop'
Add-Type @'
namespace Windows.Automation {
    public static class SelectionItemPattern { public static string Pattern = "SelectionItem"; }
    public static class SelectionPattern { public static string Pattern = "Selection"; }
}
'@
$tokens = $null; $errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile($args[0], [ref]$tokens, [ref]$errors)
if ($errors) { throw 'Scanner parse failure' }
$function = $ast.Find({
    param($n)
    $n -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
        $n.Name -eq 'Test-SidebarSelectionOwner'
}, $true)
. ([scriptblock]::Create($function.Extent.Text))
$condition = $ast.Find({
    param($n)
    $n -is [System.Management.Automation.Language.IfStatementAst] -and
        $n.Extent.Text.StartsWith('if (-not $sidebarItems.TryGetCurrentPattern(')
}, $true)
if (-not $condition) { throw 'Missing actual single-selection admission guard' }
$nativeSelectionGuard = [scriptblock]::Create($condition.Extent.Text)
function Element($id, $process = 45) {
    $element = [pscustomobject]@{ Current = [pscustomobject]@{ ProcessId = $process }; RuntimeId = @($id) }
    $element | Add-Member ScriptMethod GetRuntimeId { $this.RuntimeId }
    return $element
}
$sidebarItems = Element @(42, 2)
$sidebarItems | Add-Member NoteProperty HasPattern $true
$sidebarItems | Add-Member NoteProperty Multiple $false
$sidebarItems | Add-Member ScriptMethod TryGetCurrentPattern {
    param($pattern, $result)
    $result.Value = [pscustomobject]@{ Current = [pscustomobject]@{ CanSelectMultiple = $this.Multiple } }
    return $this.HasPattern -and $pattern -eq 'Selection'
}
foreach ($case in @('single', 'missing-selection', 'multiple-selection')) {
    $sidebarItems.HasPattern = $case -ne 'missing-selection'
    $sidebarItems.Multiple = $case -eq 'multiple-selection'
    $selection = $null
    $threw = $false
    try { . $nativeSelectionGuard } catch { $threw = $true }
    if ($threw -ne ($case -ne 'single')) { throw "Wrong Tree selection admission: $case" }
}
$row = [pscustomobject]@{ HasPattern = $true; Owner = (Element @(42, 2)) }
$row | Add-Member ScriptMethod TryGetCurrentPattern {
    param($pattern, $result)
    $result.Value = [pscustomobject]@{ Current = [pscustomobject]@{ SelectionContainer = $this.Owner } }
    return $this.HasPattern -and $pattern -eq 'SelectionItem'
}
foreach ($case in @('owned', 'missing-pattern', 'missing-owner', 'foreign-owner', 'other-tree', 'empty-tree-id')) {
    $row.HasPattern = $case -ne 'missing-pattern'
    $row.Owner = switch ($case) {
        'missing-owner' { $null }
        'foreign-owner' { Element @(42, 2) 46 }
        'other-tree' { Element @(42, 3) }
        default { Element @(42, 2) }
    }
    $sidebarItems.RuntimeId = if ($case -eq 'empty-tree-id') { @() } else { @(42, 2) }
    $accepted = Test-SidebarSelectionOwner -Row $row -ItemsList $sidebarItems
    if ($accepted -ne ($case -eq 'owned')) { throw "Wrong row selection owner admission: $case" }
}
"""
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "selection-predicate.ps1"
            path.write_text(script, encoding="utf-8")
            result = subprocess.run(["pwsh", "-NoProfile", "-File", str(path), str(scan_path)],
                                    capture_output=True, text=True, timeout=30)
            self.assertEqual(0, result.returncode, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
