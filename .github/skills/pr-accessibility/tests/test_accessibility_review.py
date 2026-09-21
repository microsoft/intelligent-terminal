import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).parents[1] / "scripts" / "accessibility_review.py"
SPEC = importlib.util.spec_from_file_location("accessibility_review", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
SPEC.loader.exec_module(MODULE)


class StaticAnalysisTests(unittest.TestCase):
    def scan(self, xaml):
        return MODULE._scan_xaml("src/cascadia/TerminalApp/Test.xaml", xaml, set())

    def test_content_derived_and_resource_names_are_not_flagged(self):
        xaml = """
<Grid>
  <Button Content="Open" />
  <Button x:Uid="CloseButton"><SymbolIcon Symbol="Cancel" /></Button>
  <Button AutomationProperties.Name="{x:Bind AccessibleName}"><FontIcon Glyph="x" /></Button>
  <Button Style="{StaticResource InheritedAccessibleButtonStyle}"><FontIcon Glyph="x" /></Button>
</Grid>
"""
        self.assertEqual([], self.scan(xaml))

    def test_decorative_inert_control_is_not_high(self):
        xaml = """
<Grid>
  <Button IsHitTestVisible="False" AutomationProperties.AccessibilityView="Raw">
    <SymbolIcon Symbol="Cancel" />
  </Button>
</Grid>
"""
        findings = self.scan(xaml)
        self.assertFalse(any(item["rule"] == "AXSTATIC001" for item in findings))

    def test_icon_only_control_is_review_not_automatic_high(self):
        findings = self.scan("<Button><SymbolIcon Symbol=\"Cancel\" /></Button>")
        self.assertEqual("AXSTATIC002", findings[0]["rule"])
        self.assertEqual("MEDIUM", findings[0]["severity"])
        self.assertFalse(findings[0]["auto_fix_eligible"])

    def test_interactive_raw_view_is_high_but_not_high_confidence(self):
        findings = self.scan('<Button AutomationProperties.AccessibilityView="Raw" Content="Close" />')
        self.assertEqual("AXSTATIC001", findings[0]["rule"])
        self.assertEqual("HIGH", findings[0]["severity"])
        self.assertEqual("medium", findings[0]["confidence"])

    def test_literal_accessible_name_requires_localization_review(self):
        findings = self.scan('<Button AutomationProperties.Name="Close"><SymbolIcon Symbol="Cancel" /></Button>')
        self.assertEqual(["AXSTATIC003"], [item["rule"] for item in findings])

    def test_output_is_idempotent(self):
        xaml = '<Button AutomationProperties.AccessibilityView="Raw" Content="Close" />'
        self.assertEqual(self.scan(xaml), self.scan(xaml))

    def test_focus_and_runtime_sensitive_surfaces_are_classified(self):
        surfaces = MODULE._review_surfaces(
            "src/cascadia/TerminalApp/Test.cpp",
            "control.Focus(FocusState::Programmatic); RaiseNotificationEvent(); HighContrast;",
            set(),
        )
        self.assertIn("keyboard_focus", surfaces)
        self.assertIn("announcements", surfaces)
        self.assertIn("high_contrast_theme", surfaces)


class PrepareIntegrationTests(unittest.TestCase):
    def test_prepare_reads_immutable_head_blobs_and_is_idempotent(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            subprocess.run(["git", "config", "user.email", "test@example.com"], cwd=root, check=True)
            subprocess.run(["git", "config", "user.name", "Test"], cwd=root, check=True)
            path = root / "src/cascadia/TerminalApp/Test.xaml"
            path.parent.mkdir(parents=True)
            path.write_text('<Button Content="Open" />\n', encoding="utf-8")
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            subprocess.run(["git", "commit", "-qm", "base"], cwd=root, check=True)
            base = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
            path.write_text('<Button AutomationProperties.AccessibilityView="Raw" Content="Open" />\n', encoding="utf-8")
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            subprocess.run(["git", "commit", "-qm", "head"], cwd=root, check=True)
            head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
            first = root / "first.json"
            second = root / "second.json"
            MODULE.prepare(root, base, head, first, None)
            MODULE.prepare(root, base, head, second, None)
            prepared = json.loads(first.read_text())
            self.assertEqual(first.read_text(), second.read_text())
            self.assertEqual(head, prepared["source_sha"])
            self.assertEqual("AXSTATIC001", prepared["static_findings"][0]["rule"])
            self.assertTrue(all(check["status"] == "SKIPPED" for check in prepared["runtime_checks"]))


class ValidationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.workspace = Path(self.temp.name)
        self.root = self.workspace / "repo"
        self.root.mkdir()
        subprocess.run(["git", "init", "-q"], cwd=self.root, check=True)
        subprocess.run(["git", "config", "user.email", "test@example.com"], cwd=self.root, check=True)
        subprocess.run(["git", "config", "user.name", "Test"], cwd=self.root, check=True)
        path = self.root / "src/cascadia/TerminalApp/Test.xaml"
        path.parent.mkdir(parents=True)
        path.write_text(
            '<Button AutomationProperties.AccessibilityView="Raw" Content="Open" />\n',
            encoding="utf-8",
        )
        (path.parent / "Test2.xaml").write_text('<Button Content="Save" />\n', encoding="utf-8")
        subprocess.run(["git", "add", "."], cwd=self.root, check=True)
        subprocess.run(["git", "commit", "-qm", "baseline"], cwd=self.root, check=True)
        self.head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=self.root, text=True).strip()
        self.prepared = self.workspace / "prepared.json"
        self.prepared.write_text(
            json.dumps(
                {
                    "version": 1,
                    "source_sha": self.head,
                    "changed_files": ["src/cascadia/TerminalApp/Test.xaml"],
                    "static_findings": [
                        {
                            "stable_id": "AX-HIGH-1",
                            "rule": "AXSTATIC001",
                            "severity": "HIGH",
                            "confidence": "medium",
                            "file": "src/cascadia/TerminalApp/Test.xaml",
                            "line": 1,
                            "evidence": '<Button AutomationProperties.AccessibilityView="Raw" Content="Open" />',
                        }
                    ],
                    "runtime_checks": [
                        {"id": "axe-windows-uia", "status": "SKIPPED", "reason": "interactive Windows required"}
                    ],
                }
            ),
            encoding="utf-8",
        )

    def tearDown(self):
        self.temp.cleanup()

    def report(self, findings=None, patch_files=None):
        report = self.workspace / "report.json"
        report.write_text(
            json.dumps(
                {
                    "version": 1,
                    "source_sha": self.head,
                    "findings": findings or [],
                    "patch_files": patch_files or [],
                    "runtime_checks": [
                        {"id": "axe-windows-uia", "status": "SKIPPED", "reason": "interactive Windows required"}
                    ],
                }
            ),
            encoding="utf-8",
        )
        return report

    def finding(self, **updates):
        item = {
            "stable_id": "AX-HIGH-1",
            "severity": "HIGH",
            "confidence": "high",
            "file": "src/cascadia/TerminalApp/Test.xaml",
            "line": 1,
            "observed": "Operation is absent from the control view.",
            "expected": "Operation is exposed.",
            "impact": "The operation cannot be invoked.",
            "evidence": "Runtime peer and source evidence.",
            "proposed_fix": "Expose the existing control.",
            "validation": [],
            "disposition": "fixed",
            "repair_recipe": MODULE.STATIC_RAW_VIEW_RECIPE,
        }
        item.update(updates)
        return item

    def test_no_findings_is_valid_and_native_runtime_remains_skipped(self):
        MODULE.validate(self.root, self.head, self.head, True, self.prepared, self.report())

    def test_native_runtime_cannot_be_reported_as_passed(self):
        report = self.report()
        data = json.loads(report.read_text())
        data["runtime_checks"][0]["status"] = "PASS"
        report.write_text(json.dumps(data))
        with self.assertRaisesRegex(ValueError, "SKIPPED/BLOCKED"):
            MODULE.validate(self.root, self.head, self.head, True, self.prepared, report)

    def test_malformed_output_is_rejected(self):
        report = self.root / "report.json"
        report.write_text("{", encoding="utf-8")
        with self.assertRaises(json.JSONDecodeError):
            MODULE.validate(self.root, self.head, self.head, True, self.prepared, report)

    def test_live_head_mismatch_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "live pull request head"):
            MODULE.validate(self.root, self.head, "0" * 40, True, self.prepared, self.report())

    def test_stale_sha_is_rejected(self):
        report = self.report()
        data = json.loads(report.read_text())
        data["source_sha"] = "0" * 40
        report.write_text(json.dumps(data))
        with self.assertRaisesRegex(ValueError, "source_sha"):
            MODULE.validate(self.root, self.head, self.head, True, self.prepared, report)

    def test_fork_fix_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "read-only"):
            MODULE.validate(self.root, self.head, self.head, False, self.prepared, self.report([self.finding()]))

    def test_empty_diff_fixed_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "actual candidate patch"):
            MODULE.validate(self.root, self.head, self.head, True, self.prepared, self.report([self.finding()]))

    def test_trusted_static_raw_view_repair_is_accepted_and_attested(self):
        path = self.root / "src/cascadia/TerminalApp/Test.xaml"
        path.write_text('<Button Content="Open" />\n', encoding="utf-8")
        report = self.report([self.finding()], ["src/cascadia/TerminalApp/Test.xaml"])
        MODULE.validate(self.root, self.head, self.head, True, self.prepared, report)
        validation = json.loads(report.read_text())["findings"][0]["validation"]
        self.assertEqual("PASS", validation[0]["result"])
        self.assertEqual("exact-candidate", validation[0]["scope"])

    def test_fabricated_validation_command_is_rejected(self):
        path = self.root / "src/cascadia/TerminalApp/Test.xaml"
        path.write_text('<Button Content="Open" />\n', encoding="utf-8")
        invented = [{"command": "THIS-COMMAND-WAS-NEVER-EXECUTED", "result": "PASS"}]
        with self.assertRaisesRegex(ValueError, "model-authored validation is not trusted"):
            MODULE.validate(
                self.root,
                self.head,
                self.head,
                True,
                self.prepared,
                self.report([self.finding(validation=invented)], ["src/cascadia/TerminalApp/Test.xaml"]),
            )

    def test_only_high_confidence_high_can_be_fixed(self):
        with self.assertRaisesRegex(ValueError, "high-confidence HIGH"):
            MODULE.validate(
                self.root,
                self.head,
                self.head,
                True,
                self.prepared,
                self.report([self.finding(severity="MEDIUM")]),
            )

    def test_model_authored_validation_failure_is_rejected(self):
        path = self.root / "src/cascadia/TerminalApp/Test.xaml"
        path.write_text('<Button Content="Open" />\n', encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "model-authored validation is not trusted"):
            MODULE.validate(
                self.root,
                self.head,
                self.head,
                True,
                self.prepared,
                self.report(
                    [self.finding(validation=[{"command": "test", "result": "FAIL"}])],
                    ["src/cascadia/TerminalApp/Test.xaml"],
                ),
            )

    def test_extra_width_change_is_rejected_by_exact_candidate_recipe(self):
        path = self.root / "src/cascadia/TerminalApp/Test.xaml"
        path.write_text('<Button Width="200" Content="Open" />\n', encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "beyond the trusted raw-view removal"):
            MODULE.validate(
                self.root,
                self.head,
                self.head,
                True,
                self.prepared,
                self.report([self.finding()], ["src/cascadia/TerminalApp/Test.xaml"]),
            )

    def test_mixed_high_and_medium_patch_is_rejected(self):
        first = self.root / "src/cascadia/TerminalApp/Test.xaml"
        second = self.root / "src/cascadia/TerminalApp/Test2.xaml"
        first.write_text('<Button Content="Open" />\n', encoding="utf-8")
        second.write_text('<Button Width="200" Content="Save" />\n', encoding="utf-8")
        prepared = json.loads(self.prepared.read_text())
        prepared["changed_files"].append("src/cascadia/TerminalApp/Test2.xaml")
        self.prepared.write_text(json.dumps(prepared))
        medium = self.finding(
            stable_id="AX-MEDIUM-1",
            severity="MEDIUM",
            confidence="high",
            file="src/cascadia/TerminalApp/Test2.xaml",
            disposition="advice",
            repair_recipe=None,
        )
        with self.assertRaisesRegex(ValueError, "every patch file must be attributable"):
            MODULE.validate(
                self.root,
                self.head,
                self.head,
                True,
                self.prepared,
                self.report(
                    [self.finding(), medium],
                    ["src/cascadia/TerminalApp/Test.xaml", "src/cascadia/TerminalApp/Test2.xaml"],
                ),
            )

    def test_untracked_allowed_path_is_rejected(self):
        path = self.root / "src/cascadia/TerminalApp/Test.xaml"
        path.write_text('<Button Content="Open" />\n', encoding="utf-8")
        untracked = self.root / "src/cascadia/TerminalApp/NewTest.xaml"
        untracked.write_text('<Button Content="New" />\n', encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "untracked files are not eligible"):
            MODULE.validate(
                self.root,
                self.head,
                self.head,
                True,
                self.prepared,
                self.report([self.finding()], ["src/cascadia/TerminalApp/Test.xaml"]),
            )

    def test_out_of_scope_change_is_rejected(self):
        readme = self.root / "README.md"
        readme.write_text("unexpected\n", encoding="utf-8")
        subprocess.run(["git", "add", "README.md"], cwd=self.root, check=True)
        with self.assertRaisesRegex(ValueError, "outside the native UI/test allowlist"):
            MODULE.validate(
                self.root,
                self.head,
                self.head,
                True,
                self.prepared,
                self.report([], ["README.md"]),
            )

    def test_literal_accessible_string_patch_is_rejected(self):
        path = self.root / "src/cascadia/TerminalApp/Test.xaml"
        path.write_text('<Button AutomationProperties.Name="Open" />\n', encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "literal accessible name"):
            MODULE.validate(
                self.root,
                self.head,
                self.head,
                True,
                self.prepared,
                self.report([self.finding()], ["src/cascadia/TerminalApp/Test.xaml"]),
            )


class WorkflowContractTests(unittest.TestCase):
    def test_pr_publication_is_single_mode_and_fail_closed(self):
        root = Path(__file__).parents[4]
        workflow = (root / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        self.assertIn("push-to-pull-request-branch:", workflow)
        self.assertIn("fallback-as-pull-request: false", workflow)
        self.assertIn("--current-head \"$CURRENT_HEAD_SHA\"", workflow)
        frontmatter = workflow.split("---", 2)[1]
        self.assertNotIn("add-comment:", frontmatter)
        self.assertNotIn("submit-pull-request-review:", frontmatter)

    def test_native_runtime_job_uses_pinned_ephemeral_windows_lane(self):
        root = Path(__file__).parents[4]
        workflow = (root / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        self.assertIn("runs-on: windows-latest", workflow)
        self.assertIn("persist-credentials: false", workflow)
        self.assertIn("AECA43F41C89B3FFB1DB84011539E609ECD7CB3BADD6E78FADA2ADA327D10A64", workflow)
        self.assertIn("-Surface fre", workflow)
        self.assertIn("-Surface agents", workflow)
        self.assertIn("Native accessibility scan failed or was blocked", workflow)

    def test_native_runtime_harness_targets_only_test_host_package(self):
        root = Path(__file__).parents[4]
        harness = (root / "test/accessibility/Invoke-AxeWindowsTestHost.ps1").read_text(encoding="utf-8")
        scan = (root / "test/accessibility/Invoke-AxeWindowsScan.ps1").read_text(encoding="utf-8")
        self.assertIn("WindowsTerminal.TestHost", harness)
        self.assertNotIn("Microsoft.IntelligentTerminal", harness)
        self.assertIn("SourceSha", harness)
        self.assertIn("failed before producing results", harness)
        self.assertIn("OutputFileFormat]::None", scan)
        self.assertIn("Axe.Windows.Automation.ScannerFactory", scan)


if __name__ == "__main__":
    unittest.main()
