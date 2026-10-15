import importlib.util
import contextlib
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).parents[1] / "scripts" / "accessibility_review.py"
SPEC = importlib.util.spec_from_file_location("accessibility_review", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
SPEC.loader.exec_module(MODULE)

def review_check_fixture(files, text, *, path=None, revision="head", line=1):
    return [{
        "id": check_id,
        "status": "source-reviewed",
        "files": files,
        "assessment": "Synthetic validator fixture; not native runtime evidence.",
        "evidence": [{"file": path or files[0], "revision": revision, "line": line, "text": text}],
    } for check_id in MODULE.REVIEW_CHECK_IDS] if files else []


class StaticAnalysisTests(unittest.TestCase):
    def test_native_test_host_sources_and_project_are_relevant(self):
        for suffix in (".xaml", ".cpp", ".h", ".vcxproj"):
            with self.subTest(suffix=suffix):
                self.assertTrue(MODULE._is_relevant(
                    f"src/cascadia/LocalTests_TerminalApp/TestHostApp/Test{suffix}"))
        self.assertFalse(MODULE._is_relevant("tools/Example.vcxproj"))
        self.assertNotIn(".vcxproj", MODULE.PATCH_SUFFIXES)

    def test_comment_text_is_not_a_trusted_control_name(self):
        source = '<Button AutomationProperties.AccessibilityView="Raw"><FontIcon Glyph="x" /><!-- 2 > 1 --></Button>'
        with self.assertRaisesRegex(ValueError, "existing accessible name source"):
            MODULE._remove_raw_view_at_line(source, 1)

    def test_comment_and_cdata_examples_are_not_controls_or_repairs(self):
        control = '<Button Content="Example" AutomationProperties.AccessibilityView="Raw" />'
        for source in (f"<!-- {control} -->", f"<![CDATA[{control}]]>", f"<!-- {control}"):
            with self.subTest(source=source):
                self.assertEqual([], self.scan(source))
                with self.assertRaisesRegex(ValueError, "exactly one interactive"):
                    MODULE._remove_raw_view_at_line(source, 1)

    def test_real_control_after_example_remains_repairable(self):
        example = '<Button Content="Example" AutomationProperties.AccessibilityView="Raw" />'
        source = f'<!-- {example} -->\n<Button Content="Actual" AutomationProperties.AccessibilityView="Raw" />'
        findings = self.scan(source)
        self.assertEqual(1, len(findings))
        self.assertEqual(2, findings[0]["line"])
        repaired = MODULE._remove_raw_view_at_line(source, 2)
        self.assertTrue(repaired.startswith(f"<!-- {example} -->"))
        self.assertIn('<Button Content="Actual" />', repaired)

    def test_empty_changed_line_set_does_not_scan_untouched_controls(self):
        xaml = '<Button Content="Existing" AutomationProperties.AccessibilityView="Raw" />'
        self.assertEqual([], MODULE._scan_xaml("src/cascadia/TerminalApp/Test.xaml", xaml, set()))

    def test_visual_style_is_not_a_trusted_repair_name_source(self):
        xaml = '<Button Style="{StaticResource VisualButton}" AutomationProperties.AccessibilityView="Raw" />'
        with self.assertRaisesRegex(ValueError, "existing accessible name source"):
            MODULE._remove_raw_view_at_line(xaml, 1)

    def scan(self, xaml):
        return MODULE._scan_xaml("src/cascadia/TerminalApp/Test.xaml", xaml, None)

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
    def test_test_host_only_changes_require_review(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            subprocess.run(["git", "config", "user.email", "test@example.com"], cwd=root, check=True)
            subprocess.run(["git", "config", "user.name", "Test"], cwd=root, check=True)
            host = root / "src/cascadia/LocalTests_TerminalApp/TestHostApp"
            host.mkdir(parents=True)
            sources = {
                "UnitTestApp.xaml": '<Button Content="Open" />\n',
                "UnitTestApp.xaml.cpp": "void ShowHost() {}\n",
                "TestHostApp.vcxproj": "<Project />\n",
            }
            for name, content in sources.items():
                (host / name).write_text(content, encoding="utf-8")
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            subprocess.run(["git", "commit", "-qm", "Base host"], cwd=root, check=True)
            base = MODULE._git(root, "rev-parse", "HEAD").strip()
            (host / "UnitTestApp.xaml").write_text(
                '<Button Content="Open" AutomationProperties.AccessibilityView="Raw" />\n', encoding="utf-8")
            (host / "UnitTestApp.xaml.cpp").write_text("void ShowHost() { Open(); }\n", encoding="utf-8")
            (host / "TestHostApp.vcxproj").write_text("<Project><PropertyGroup /></Project>\n", encoding="utf-8")
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            subprocess.run(["git", "commit", "-qm", "Change only host"], cwd=root, check=True)
            head = MODULE._git(root, "rev-parse", "HEAD").strip()
            output = root / "prepared.json"
            MODULE.prepare(root, base, head, output)
            prepared = json.loads(output.read_text())
            self.assertTrue(prepared["relevant"])
            self.assertEqual(sorted(f"{host.relative_to(root).as_posix()}/{name}" for name in sources),
                             prepared["changed_files"])
            self.assertEqual(list(MODULE.REVIEW_CHECK_IDS), prepared["required_review_checks"])
            self.assertEqual("AXSTATIC001", prepared["static_findings"][0]["rule"])

    def test_base_only_policy_and_ui_updates_are_not_pr_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            subprocess.run(["git", "config", "user.email", "test@example.com"], cwd=root, check=True)
            subprocess.run(["git", "config", "user.name", "Test"], cwd=root, check=True)
            subprocess.run(["git", "config", "core.autocrlf", "false"], cwd=root, check=True)
            path = root / "src/cascadia/TerminalApp/Test.xaml"
            path.parent.mkdir(parents=True)
            path.write_bytes(b'<Button Content="Open" />\n')
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            subprocess.run(["git", "commit", "-qm", "Common ancestor"], cwd=root, check=True)
            ancestor = MODULE._git(root, "rev-parse", "HEAD").strip()
            subprocess.run(["git", "checkout", "-qb", "base-update"], cwd=root, check=True)
            skill = root / ".github/skills/pr-accessibility/SKILL.md"
            skill.parent.mkdir(parents=True)
            skill.write_text("Trusted base policy\n", encoding="utf-8")
            (path.parent / "BaseOnly.xaml").write_bytes(b'<Button Content="Base only" />\n')
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            subprocess.run(["git", "commit", "-qm", "Base policy and UI advance"], cwd=root, check=True)
            trusted_base = MODULE._git(root, "rev-parse", "HEAD").strip()
            subprocess.run(["git", "checkout", "-qb", "feature", ancestor], cwd=root, check=True)
            path.write_bytes(b'<Button Content="Open" AutomationProperties.AccessibilityView="Raw" />\n')
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            subprocess.run(["git", "commit", "-qm", "Feature changes one control"], cwd=root, check=True)
            head = MODULE._git(root, "rev-parse", "HEAD").strip()
            with self.assertRaisesRegex(ValueError, "operating instructions"):
                MODULE._verify_instruction_boundary(root, trusted_base, head)
            output = root / "prepared.json"
            MODULE.prepare(root, trusted_base, head, output)
            prepared = json.loads(output.read_text())
            self.assertEqual(trusted_base, prepared["trusted_base_sha"])
            self.assertEqual(ancestor, prepared["comparison_base_sha"])
            self.assertEqual(["src/cascadia/TerminalApp/Test.xaml"], prepared["changed_files"])
            self.assertEqual(1, len(prepared["static_findings"]))
            self.assertEqual("AXSTATIC001", prepared["static_findings"][0]["rule"])
            self.assertEqual(ancestor, prepared["source_evidence"]["src/cascadia/TerminalApp/Test.xaml"]["base"]["source_sha"])

    def test_deletion_only_hunk_does_not_authorize_untouched_raw_control(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            subprocess.run(["git", "config", "user.email", "test@example.com"], cwd=root, check=True)
            subprocess.run(["git", "config", "user.name", "Test"], cwd=root, check=True)
            path = root / "src/cascadia/TerminalApp/Test.xaml"
            path.parent.mkdir(parents=True)
            original = '<Grid>\n<Button Content="Existing" AutomationProperties.AccessibilityView="Raw" />\n<TextBlock Text="Removed" />\n</Grid>\n'
            path.write_bytes(original.encode("utf-8"))
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            subprocess.run(["git", "commit", "-qm", "Base"], cwd=root, check=True)
            base = MODULE._git(root, "rev-parse", "HEAD").strip()
            path.write_bytes(original.replace('<TextBlock Text="Removed" />\n', "").encode("utf-8"))
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            subprocess.run(["git", "commit", "-qm", "Delete only text"], cwd=root, check=True)
            head = MODULE._git(root, "rev-parse", "HEAD").strip()
            output = root / "prepared.json"
            MODULE.prepare(root, base, head, output)
            prepared = json.loads(output.read_text())
            self.assertTrue(prepared["relevant"])
            self.assertEqual([], prepared["static_findings"])
            self.assertIn('-<TextBlock Text="Removed" />', prepared["source_evidence"]["src/cascadia/TerminalApp/Test.xaml"]["diff"])
            self.assertEqual(set(), MODULE._added_lines(root, base, head, "src/cascadia/TerminalApp/Test.xaml"))

    @contextlib.contextmanager
    def source_fixture(self):
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as directory:
            workspace = Path(directory)
            root = workspace / "repo"
            root.mkdir()
            MODULE._git(root, "init", "-q")
            MODULE._git(root, "config", "user.email", "test@example.com")
            MODULE._git(root, "config", "user.name", "Test")
            MODULE._git(root, "config", "core.autocrlf", "false")
            path = root / "src/cascadia/TerminalApp/Deleted.xaml"
            path.parent.mkdir(parents=True)
            path.write_bytes(b'<Button AutomationProperties.AccessibilityView="Raw" Content="Open" />\n')
            MODULE._git(root, "add", ".")
            MODULE._git(root, "commit", "-qm", "base")
            yield workspace, root, path, MODULE._git(root, "rev-parse", "HEAD").strip()

    def test_prepare_uses_immutable_head_despite_dirty_worktree(self):
        with self.source_fixture() as (workspace, root, path, base):
            reviewed = b'<Button AutomationProperties.AccessibilityView="Raw" Content="Reviewed" />\n'
            path.write_bytes(reviewed)
            MODULE._git(root, "add", ".")
            MODULE._git(root, "commit", "-qm", "Reviewed UI change")
            head = MODULE._git(root, "rev-parse", "HEAD").strip()
            path.write_bytes(b'<Button Content="Mutable worktree" />\n')
            output = workspace / "prepared.json"
            MODULE.prepare(root, base, head, output)
            report = json.loads(output.read_text())
            relative = path.relative_to(root).as_posix()
            self.assertEqual(reviewed.decode("utf-8"), report["source_evidence"][relative]["head"]["text"])
            self.assertEqual("AXSTATIC001", report["static_findings"][0]["rule"])

    def test_prepare_cli_rejects_changed_files_override(self):
        with self.source_fixture() as (workspace, root, path, base):
            override = workspace / "files.txt"
            override.write_text(path.relative_to(root).as_posix(), encoding="utf-8")
            result = subprocess.run(
                [sys.executable, str(SCRIPT), "prepare", "--root", str(root),
                 "--base", base, "--head", base, "--output", str(workspace / "prepared.json"),
                 "--changed-files", str(override)],
                capture_output=True, text=True, timeout=30,
            )
            self.assertEqual(2, result.returncode)
            self.assertIn("--changed-files", result.stderr)

    def test_deletion_only_preserves_base_evidence_and_allows_unrepaired_high(self):
        with self.source_fixture() as (workspace, root, path, base):
            original = path.read_bytes().decode("utf-8")
            relative = path.relative_to(root).as_posix()
            path.unlink()
            MODULE._git(root, "add", ".")
            MODULE._git(root, "commit", "-qm", "delete UI")
            head = MODULE._git(root, "rev-parse", "HEAD").strip()
            prepared_path = workspace / "prepared.json"
            MODULE.prepare(root, base, head, prepared_path)
            first = prepared_path.read_text()
            MODULE.prepare(root, base, head, prepared_path)
            self.assertEqual(first, prepared_path.read_text())
            prepared = json.loads(first)
            self.assertTrue(prepared["relevant"])
            self.assertEqual([relative], prepared["changed_files"])
            self.assertEqual(head, prepared["source_sha"])
            self.assertEqual(base, prepared["comparison_base_sha"])
            evidence = prepared["source_evidence"][relative]
            self.assertEqual("deleted", evidence["change"])
            self.assertIsNone(evidence["head"])
            self.assertEqual(base, evidence["base"]["source_sha"])
            self.assertEqual(relative, evidence["base"]["file"])
            self.assertEqual(original, evidence["base"]["text"])
            self.assertEqual(
                MODULE._git(root, "rev-parse", f"{base}:{relative}").strip(),
                evidence["base"]["blob_sha"],
            )
            self.assertIn("deleted file mode", evidence["diff"])
            self.assertIn("-" + original.strip(), evidence["diff"])
            self.assertIn("names_roles_values", prepared["review_surfaces"][relative])
            self.assertEqual([], prepared["static_findings"])
            with self.assertRaises(RuntimeError):
                MODULE._git(root, "show", f"{head}:{relative}")
            finding = MODULE._finding(
                "AX-DELETION", relative, 1, "HIGH", "high",
                "Accessible operation was deleted.", "Keep an accessible operation.",
                "Users cannot invoke the operation.", evidence["diff"],
            )
            report_path = workspace / "report.json"
            for disposition in ("remaining", "blocked"):
                finding["disposition"] = disposition
                report_path.write_text(json.dumps({
                    "version": 1, "source_sha": head, "findings": [finding],
                    "patch_files": [], "runtime_checks": prepared["runtime_checks"],
                    "review_checks": review_check_fixture([relative], original.strip(), revision="base"),
                }), encoding="utf-8")
                MODULE.validate(root, head, head, True, prepared_path, report_path)

    def test_renames_expose_old_and_new_revision_paths_without_false_head_source(self):
        for destination in ("src/cascadia/TerminalApp/Renamed.xaml", "doc/Moved.xaml"):
            with self.subTest(destination=destination), self.source_fixture() as (workspace, root, path, base):
                old = path.relative_to(root).as_posix()
                target = root / destination
                target.parent.mkdir(parents=True, exist_ok=True)
                path.rename(target)
                MODULE._git(root, "add", ".")
                MODULE._git(root, "commit", "-qm", "rename UI")
                head = MODULE._git(root, "rev-parse", "HEAD").strip()
                self.assertIn("R100", MODULE._git(root, "diff", "--name-status", "-M", base, head))
                prepared_path = workspace / "prepared.json"
                MODULE.prepare(root, base, head, prepared_path)
                prepared = json.loads(prepared_path.read_text())
                MODULE._git(root, "config", "diff.renames", "false")
                MODULE.prepare(root, base, head, prepared_path)
                self.assertEqual(prepared, json.loads(prepared_path.read_text()))
                expected = sorted([old, destination]) if MODULE._is_relevant(destination) else [old]
                self.assertTrue(prepared["relevant"])
                self.assertEqual(expected, prepared["changed_files"])
                deleted = prepared["source_evidence"][old]
                self.assertEqual(base, deleted["base"]["source_sha"])
                self.assertIsNone(deleted["head"])
                self.assertIn("deleted file mode", deleted["diff"])
                with self.assertRaises(RuntimeError):
                    MODULE._git(root, "show", f"{head}:{old}")
                if destination in expected:
                    added = prepared["source_evidence"][destination]
                    self.assertIsNone(added["base"])
                    self.assertEqual(head, added["head"]["source_sha"])
                    self.assertEqual(destination, added["head"]["file"])
                    self.assertEqual(deleted["base"]["blob_sha"], added["head"]["blob_sha"])
                    self.assertIn("new file mode", added["diff"])
                    self.assertEqual([destination], [item["file"] for item in prepared["static_findings"]])

    def test_deleted_base_signal_cannot_authorize_restoring_a_head_missing_file(self):
        with self.source_fixture() as (workspace, root, path, base):
            original = path.read_bytes().decode("utf-8")
            relative = path.relative_to(root).as_posix()
            path.unlink()
            MODULE._git(root, "add", ".")
            MODULE._git(root, "commit", "-qm", "delete UI")
            head = MODULE._git(root, "rev-parse", "HEAD").strip()
            prepared_path = workspace / "prepared.json"
            MODULE.prepare(root, base, head, prepared_path)
            prepared = json.loads(prepared_path.read_text())
            signal = MODULE._scan_xaml(relative, original, None)[0]
            prepared["static_findings"] = [signal]
            prepared_path.write_text(json.dumps(prepared), encoding="utf-8")
            finding = dict(signal, confidence="high", disposition="fixed",
                           repair_recipe=MODULE.STATIC_RAW_VIEW_RECIPE)
            path.write_bytes(MODULE._remove_raw_view_at_line(original, 1).encode("utf-8"))
            MODULE._git(root, "add", ".")
            report_path = workspace / "report.json"
            report_path.write_text(json.dumps({
                "version": 1, "source_sha": head, "findings": [finding],
                "patch_files": [relative], "runtime_checks": prepared["runtime_checks"],
                "review_checks": review_check_fixture([relative], original.strip(), revision="base"),
            }), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "trusted static repair verification failed"):
                MODULE.validate(root, head, head, True, prepared_path, report_path)

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
            MODULE.prepare(root, base, head, first)
            MODULE.prepare(root, base, head, second)
            prepared = json.loads(first.read_text())
            self.assertEqual(first.read_text(), second.read_text())
            self.assertEqual(head, prepared["source_sha"])
            evidence = prepared["source_evidence"]["src/cascadia/TerminalApp/Test.xaml"]
            self.assertEqual("modified", evidence["change"])
            self.assertEqual(base, evidence["base"]["source_sha"])
            self.assertEqual(head, evidence["head"]["source_sha"])
            self.assertNotIn("AccessibilityView", evidence["base"]["text"])
            self.assertIn('AccessibilityView="Raw"', evidence["head"]["text"])
            self.assertEqual("AXSTATIC001", prepared["static_findings"][0]["rule"])
            self.assertTrue(all(check["status"] == "SKIPPED" for check in prepared["runtime_checks"]))


class ValidationTests(unittest.TestCase):
    def test_prepared_high_signal_cannot_be_silently_downgraded(self):
        for severity, disposition in (("MEDIUM", "advice"), ("LOW", "skipped")):
            with self.subTest(severity=severity):
                with self.assertRaisesRegex(ValueError, "prepared HIGH signal requires HIGH severity"):
                    MODULE.validate(
                        self.root, self.head, self.head, True, self.prepared,
                        self.report([self.finding(severity=severity, disposition=disposition)]),
                    )

    def test_false_positive_high_dismissal_allows_independent_advice(self):
        dismissal = [{
            "stable_id": "AX-HIGH-1",
            "reason": "The original raw-view signal concerns a decorative duplicate, not the accessible primary operation.",
            "evidence": "The inspected companion control exposes the same operation and preserves keyboard activation.",
        }]
        MODULE.validate(
            self.root, self.head, self.head, True, self.prepared,
            self.report([self.finding(stable_id="AX-ADVICE-1", severity="MEDIUM", disposition="advice")],
                        dismissed_signals=dismissal),
        )

    def test_prepared_signal_rule_cannot_be_rewritten(self):
        with self.assertRaisesRegex(ValueError, "reported rule must match"):
            MODULE.validate(
                self.root, self.head, self.head, True, self.prepared,
                self.report([self.finding(disposition="remaining", rule="AXSTATIC002")]),
            )

    def test_prepared_signal_id_cannot_be_reused_at_an_unrelated_location(self):
        with self.assertRaisesRegex(ValueError, "reported signal location must match"):
            MODULE.validate(
                self.root, self.head, self.head, True, self.prepared,
                self.report([self.finding(disposition="remaining", line=2)]),
            )

    def test_style_only_control_cannot_authorize_recipe(self):
        path = self.root / "src/cascadia/TerminalApp/Test.xaml"
        original = '<Button Style="{StaticResource VisualButton}" AutomationProperties.AccessibilityView="Raw" />\n'
        path.write_bytes(original.encode("utf-8"))
        subprocess.run(["git", "add", "."], cwd=self.root, check=True)
        subprocess.run(["git", "commit", "-qm", "Style-only reviewed control"], cwd=self.root, check=True)
        self.head = MODULE._git(self.root, "rev-parse", "HEAD").strip()
        prepared = json.loads(self.prepared.read_text())
        prepared["source_sha"] = self.head
        self.prepared.write_text(json.dumps(prepared))
        path.write_bytes(original.replace(' AutomationProperties.AccessibilityView="Raw"', "").encode("utf-8"))
        with self.assertRaisesRegex(ValueError, "existing accessible name source"):
            MODULE.validate(
                self.root, self.head, self.head, True, self.prepared,
                self.report([self.finding()], ["src/cascadia/TerminalApp/Test.xaml"]),
            )

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir=Path.cwd())
        self.workspace = Path(self.temp.name)
        self.root = self.workspace / "repo"
        self.root.mkdir()
        subprocess.run(["git", "init", "-q"], cwd=self.root, check=True)
        subprocess.run(["git", "config", "user.email", "test@example.com"], cwd=self.root, check=True)
        subprocess.run(["git", "config", "user.name", "Test"], cwd=self.root, check=True)
        subprocess.run(["git", "config", "core.autocrlf", "false"], cwd=self.root, check=True)
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

    def report(self, findings=None, patch_files=None, dismissed_signals=None):
        if dismissed_signals is None:
            dismissed_signals = [] if findings else [{
                "stable_id": "AX-HIGH-1",
                "reason": "Synthetic fixture assumes Test.xaml delegates the operation to an accessible companion control.",
                "evidence": "Synthetic companion Test2.xaml exposes the Save button in the control view.",
            }]
        report = self.workspace / "report.json"
        files = json.loads(self.prepared.read_text())["changed_files"]
        citations = []
        for path in files:
            blob = MODULE._source_blob(self.root, self.head, path)
            if blob:
                line, text = next((number, text) for number, text in
                                  enumerate(blob["text"].splitlines(), 1) if text.strip())
                citations.append({"file": path, "revision": "head", "line": line, "text": text})
        checks = review_check_fixture(
            files, citations[0]["text"] if citations else "",
            path=citations[0]["file"] if citations else None)
        for check in checks:
            check["evidence"] = citations
        report.write_text(
            json.dumps(
                {
                    "version": 1,
                    "source_sha": self.head,
                    "findings": findings or [],
                    "dismissed_signals": dismissed_signals,
                    "patch_files": patch_files or [],
                    "review_checks": checks,
                    "runtime_checks": [
                        {"id": "axe-windows-uia", "status": "SKIPPED", "reason": "interactive Windows required"}
                    ],
                }
            ),
            encoding="utf-8",
        )
        return report

    def publication(self, candidate_file_mode=None):
        subprocess.run(["git", "checkout", "-qb", "fixture/head"], cwd=self.root, check=True)
        path = self.root / "src/cascadia/TerminalApp/Test.xaml"
        original = path.read_bytes().decode("utf-8")
        path.write_bytes(MODULE._remove_raw_view_at_line(original, 1).encode("utf-8"))
        subprocess.run(["git", "add", "."], cwd=self.root, check=True)
        if candidate_file_mode is not None:
            blob = MODULE._git(self.root, "hash-object", str(path)).strip()
            MODULE._git(self.root, "update-index", "--cacheinfo",
                        f"{candidate_file_mode},{blob},src/cascadia/TerminalApp/Test.xaml")
        subprocess.run(["git", "commit", "-qm", "Restore UIA view [native-accessibility]"], cwd=self.root, check=True)
        transport = self.workspace / "transport"
        transport.mkdir()
        MODULE._git(self.root, "bundle", "create", str(transport / "aw-fixture-head.bundle"),
                    "fixture/head", f"^{self.head}")
        prepared = json.loads(self.prepared.read_text())
        prepared["publication"] = {"head_ref": "fixture/head", "repository": "test/accessibility", "pr_number": 1}
        self.prepared.write_text(json.dumps(prepared))
        queue = self.workspace / "safeoutputs.jsonl"
        queue.write_text(json.dumps({
            "type": "push_to_pull_request_branch", "branch": "fixture/head",
            "head_repo": "test/accessibility", "pull_request_number": 1,
            "base_commit": self.head,
        }) + "\n")
        return queue, transport

    def test_publication_accepts_exact_captured_commit(self):
        queue, transport = self.publication()
        report = self.report([self.finding()], ["src/cascadia/TerminalApp/Test.xaml"])
        MODULE.validate(
            self.root, self.head, self.head, True, self.prepared,
            report, queue, transport,
        )
        self.assertEqual(MODULE._git(self.root, "rev-parse", "HEAD").strip(),
                         json.loads(report.read_text())["findings"][0]["commit_sha"])

    def two_findings(self):
        path = self.root / "src/cascadia/TerminalApp/Test.xaml"
        original = ('<Grid>\n<Button\n AutomationProperties.AccessibilityView="Raw" Content="Open" />\n'
                    '<Button AutomationProperties.AccessibilityView="Raw" Content="Save" />\n</Grid>\n')
        path.write_bytes(original.encode("utf-8"))
        MODULE._git(self.root, "add", ".")
        MODULE._git(self.root, "commit", "-qm", "Two reviewed controls")
        self.head = MODULE._git(self.root, "rev-parse", "HEAD").strip()
        findings = [self.finding(line=2), self.finding(stable_id="AX-HIGH-2", line=4)]
        prepared = json.loads(self.prepared.read_text())
        prepared["source_sha"] = self.head
        prepared["static_findings"] = [
            dict(item, rule="AXSTATIC001") for item in findings
        ]
        self.prepared.write_text(json.dumps(prepared))
        return path, original, findings

    def capture_series(self):
        transport = self.workspace / "transport"
        transport.mkdir(exist_ok=True)
        bundle = transport / "aw-fixture-head.bundle"
        bundle.unlink(missing_ok=True)
        MODULE._git(self.root, "bundle", "create", str(bundle), "fixture/head", f"^{self.head}")
        prepared = json.loads(self.prepared.read_text())
        prepared["publication"] = {"head_ref": "fixture/head", "repository": "test/accessibility", "pr_number": 1}
        self.prepared.write_text(json.dumps(prepared))
        queue = self.workspace / "safeoutputs.jsonl"
        queue.write_text(json.dumps({
            "type": "push_to_pull_request_branch", "branch": "fixture/head",
            "head_repo": "test/accessibility", "pull_request_number": 1, "base_commit": self.head,
        }) + "\n")
        return queue, transport

    def commit_text(self, path, text, message):
        path.write_bytes(text.encode("utf-8"))
        MODULE._git(self.root, "add", ".")
        MODULE._git(self.root, "commit", "-qm", message)
        return MODULE._git(self.root, "rev-parse", "HEAD").strip()

    def test_two_high_findings_have_separate_authenticated_commits_and_original_lines(self):
        path, original, findings = self.two_findings()
        MODULE._git(self.root, "checkout", "-qb", "fixture/head")
        first = MODULE._remove_raw_view_at_line(original, 2)
        commits = [self.commit_text(path, first, "First finding")]
        # The multiline first removal shifts the second control from original line 4 to line 3.
        final = MODULE._remove_raw_view_at_line(first, 3)
        commits.append(self.commit_text(path, final, "Second finding"))
        queue, transport = self.capture_series()
        report = self.report(findings, ["src/cascadia/TerminalApp/Test.xaml"])
        MODULE.validate(self.root, self.head, self.head, True, self.prepared, report, queue, transport)
        self.assertEqual(commits, [item["commit_sha"] for item in json.loads(report.read_text())["findings"]])

    def test_two_findings_in_one_commit_are_rejected(self):
        path, original, findings = self.two_findings()
        MODULE._git(self.root, "checkout", "-qb", "fixture/head")
        final = MODULE._remove_raw_view_at_line(MODULE._remove_raw_view_at_line(original, 4), 2)
        self.commit_text(path, final, "Combined findings")
        queue, transport = self.capture_series()
        with self.assertRaisesRegex(ValueError, "one repair commit per fixed finding"):
            MODULE.validate(self.root, self.head, self.head, True, self.prepared,
                            self.report(findings, ["src/cascadia/TerminalApp/Test.xaml"]), queue, transport)

    def test_one_commit_with_two_fixes_and_empty_padding_is_rejected(self):
        path, original, findings = self.two_findings()
        MODULE._git(self.root, "checkout", "-qb", "fixture/head")
        final = MODULE._remove_raw_view_at_line(MODULE._remove_raw_view_at_line(original, 4), 2)
        self.commit_text(path, final, "Combined findings")
        MODULE._git(self.root, "commit", "--allow-empty", "-qm", "Padding")
        queue, transport = self.capture_series()
        with self.assertRaisesRegex(ValueError, "exactly its trusted recipe"):
            MODULE.validate(self.root, self.head, self.head, True, self.prepared,
                            self.report(findings, ["src/cascadia/TerminalApp/Test.xaml"]), queue, transport)

    def test_reordered_finding_commits_are_rejected(self):
        path, original, findings = self.two_findings()
        MODULE._git(self.root, "checkout", "-qb", "fixture/head")
        first = MODULE._remove_raw_view_at_line(original, 4)
        self.commit_text(path, first, "Second finding first")
        self.commit_text(path, MODULE._remove_raw_view_at_line(first, 2), "First finding second")
        queue, transport = self.capture_series()
        with self.assertRaisesRegex(ValueError, "exactly its trusted recipe"):
            MODULE.validate(self.root, self.head, self.head, True, self.prepared,
                            self.report(findings, ["src/cascadia/TerminalApp/Test.xaml"]), queue, transport)

    def test_report_cannot_self_assert_commit_sha(self):
        queue, transport = self.publication()
        with self.assertRaisesRegex(ValueError, "commit_sha does not match"):
            MODULE.validate(self.root, self.head, self.head, True, self.prepared,
                            self.report([self.finding(commit_sha="0" * 40)],
                                        ["src/cascadia/TerminalApp/Test.xaml"]), queue, transport)

    def test_merge_in_repair_series_is_rejected(self):
        path, original, findings = self.two_findings()
        MODULE._git(self.root, "checkout", "-qb", "fixture/head")
        first = MODULE._remove_raw_view_at_line(original, 2)
        parent = self.commit_text(path, first, "First finding")
        final = MODULE._remove_raw_view_at_line(first, 3)
        path.write_bytes(final.encode("utf-8"))
        MODULE._git(self.root, "add", ".")
        tree = MODULE._git(self.root, "write-tree").strip()
        merge = MODULE._git(self.root, "commit-tree", tree, "-p", parent, "-p", self.head,
                            "-m", "Merge second repair").strip()
        MODULE._git(self.root, "update-ref", "refs/heads/fixture/head", merge)
        queue, transport = self.capture_series()
        with self.assertRaisesRegex(ValueError, "linear series directly"):
            MODULE.validate(self.root, self.head, self.head, True, self.prepared,
                            self.report(findings, ["src/cascadia/TerminalApp/Test.xaml"]), queue, transport)

    def test_bundle_cannot_publish_extra_refs(self):
        queue, transport = self.publication()
        bundle = transport / "aw-fixture-head.bundle"
        bundle.unlink()
        MODULE._git(self.root, "bundle", "create", str(bundle), "fixture/head", "HEAD", f"^{self.head}")
        with self.assertRaisesRegex(ValueError, "identify only the validated"):
            MODULE.validate(self.root, self.head, self.head, True, self.prepared,
                            self.report([self.finding()], ["src/cascadia/TerminalApp/Test.xaml"]),
                            queue, transport)

    def test_bundle_filename_must_match_publisher_identity(self):
        queue, transport = self.publication()
        (transport / "aw-fixture-head.bundle").rename(transport / "aw-other-branch.bundle")
        with self.assertRaisesRegex(ValueError, "filename does not match"):
            MODULE.validate(self.root, self.head, self.head, True, self.prepared,
                            self.report([self.finding()], ["src/cascadia/TerminalApp/Test.xaml"]),
                            queue, transport)

    def test_publication_rejects_symlink_tree_with_regular_worktree_file(self):
        MODULE._git(self.root, "config", "core.symlinks", "false")
        queue, transport = self.publication(candidate_file_mode="120000")
        path = self.root / "src/cascadia/TerminalApp/Test.xaml"
        self.assertFalse(path.is_symlink())
        self.assertTrue(MODULE._git(self.root, "ls-tree", "HEAD", "--",
                                   "src/cascadia/TerminalApp/Test.xaml").startswith("120000"))
        with self.assertRaisesRegex(ValueError, "preserve the reviewed regular-file Git mode"):
            MODULE.validate(
                self.root, self.head, self.head, True, self.prepared,
                self.report([self.finding()], ["src/cascadia/TerminalApp/Test.xaml"]), queue, transport,
            )

    def test_publication_rejects_executable_mode_change(self):
        MODULE._git(self.root, "config", "core.filemode", "false")
        queue, transport = self.publication(candidate_file_mode="100755")
        with self.assertRaisesRegex(ValueError, "preserve the reviewed regular-file Git mode"):
            MODULE.validate(
                self.root, self.head, self.head, True, self.prepared,
                self.report([self.finding()], ["src/cascadia/TerminalApp/Test.xaml"]), queue, transport,
            )

    def test_recipe_rejects_reviewed_symlink_tree(self):
        MODULE._git(self.root, "config", "core.symlinks", "false")
        path = self.root / "src/cascadia/TerminalApp/Test.xaml"
        blob = MODULE._git(self.root, "hash-object", str(path)).strip()
        MODULE._git(self.root, "update-index", "--cacheinfo",
                    f"120000,{blob},src/cascadia/TerminalApp/Test.xaml")
        MODULE._git(self.root, "commit", "-qm", "Reviewed symlink fixture")
        self.head = MODULE._git(self.root, "rev-parse", "HEAD").strip()
        prepared = json.loads(self.prepared.read_text())
        prepared["source_sha"] = self.head
        self.prepared.write_text(json.dumps(prepared))
        path.write_bytes(MODULE._remove_raw_view_at_line(path.read_bytes().decode("utf-8"), 1).encode("utf-8"))
        with self.assertRaisesRegex(ValueError, "reviewed source must be a regular Git blob"):
            MODULE.validate(
                self.root, self.head, self.head, True, self.prepared,
                self.report([self.finding()], ["src/cascadia/TerminalApp/Test.xaml"]),
            )

    def test_recipe_rejects_physical_symlink_to_exact_repaired_text(self):
        path = self.root / "src/cascadia/TerminalApp/Test.xaml"
        target = self.workspace / "repaired.xaml"
        target.write_bytes(MODULE._remove_raw_view_at_line(path.read_bytes().decode("utf-8"), 1).encode("utf-8"))
        path.unlink()
        try:
            path.symlink_to(target)
        except OSError as error:
            if os.name == "nt" and getattr(error, "winerror", None) == 1314:
                self.skipTest("Windows symlink creation privilege is unavailable")
            raise
        errors = MODULE._verify_static_repairs(
            self.root, self.head, [self.finding()], json.loads(self.prepared.read_text()),
            ["src/cascadia/TerminalApp/Test.xaml"],
        )
        self.assertTrue(any("candidate must be a regular file, not a symlink" in error for error in errors), errors)

    def test_publication_rejects_changes_after_capture(self):
        queue, transport = self.publication()
        path = self.root / "src/cascadia/TerminalApp/Test.xaml"
        path.write_bytes(MODULE._git(self.root, "show", f"{self.head}:src/cascadia/TerminalApp/Test.xaml").encode("utf-8"))
        with self.assertRaisesRegex(ValueError, "clean committed candidate"):
            MODULE.validate(
                self.root, self.head, self.head, True, self.prepared,
                self.report([self.finding()], []), queue, transport,
            )

    def test_publication_rejects_tampered_bundle_body(self):
        queue, transport = self.publication()
        bundle = transport / "aw-fixture-head.bundle"
        bundle.write_bytes(bundle.read_bytes()[:-20])
        with self.assertRaisesRegex(ValueError, "publication verification failed"):
            MODULE.validate(
                self.root, self.head, self.head, True, self.prepared,
                self.report([self.finding()], ["src/cascadia/TerminalApp/Test.xaml"]), queue, transport,
            )

    def test_publication_rejects_stale_bundle_tip(self):
        queue, transport = self.publication()
        patch = transport / "aw-fixture-head.bundle"
        candidate = MODULE._git(self.root, "rev-parse", "HEAD").strip()
        patch.write_bytes(patch.read_bytes().replace(candidate.encode("ascii"), b"0" * 40, 1))
        with self.assertRaisesRegex(ValueError, "identify only the validated"):
            MODULE.validate(
                self.root, self.head, self.head, True, self.prepared,
                self.report([self.finding()], ["src/cascadia/TerminalApp/Test.xaml"]), queue, transport,
            )

    def test_publication_rejects_alternate_patch_transport(self):
        queue, transport = self.publication()
        (transport / "aw-fixture-head.patch").write_bytes(b"unvalidated alternate transport")
        with self.assertRaisesRegex(ValueError, "alternate patch transport"):
            MODULE.validate(
                self.root, self.head, self.head, True, self.prepared,
                self.report([self.finding()], ["src/cascadia/TerminalApp/Test.xaml"]), queue, transport,
            )

    def test_publication_rejects_foreign_repo_override(self):
        queue, transport = self.publication()
        entry = json.loads(queue.read_text())
        entry["repo"] = "test/other"
        queue.write_text(json.dumps(entry) + "\n")
        with self.assertRaisesRegex(ValueError, "queued repair identity"):
            MODULE.validate(
                self.root, self.head, self.head, True, self.prepared,
                self.report([self.finding()], ["src/cascadia/TerminalApp/Test.xaml"]), queue, transport,
            )

    def test_publication_rejects_multiple_commits(self):
        queue, transport = self.publication()
        subprocess.run(["git", "commit", "--allow-empty", "-qm", "Extra commit"], cwd=self.root, check=True)
        with self.assertRaisesRegex(ValueError, "one repair commit per fixed finding"):
            MODULE.validate(
                self.root, self.head, self.head, True, self.prepared,
                self.report([self.finding()], ["src/cascadia/TerminalApp/Test.xaml"]), queue, transport,
            )

    def test_publication_rejects_mixed_outputs(self):
        queue, transport = self.publication()
        with queue.open("a") as stream:
            stream.write(json.dumps({"type": "noop", "message": "Also report"}) + "\n")
        with self.assertRaisesRegex(ValueError, "exactly one push"):
            MODULE.validate(
                self.root, self.head, self.head, True, self.prepared,
                self.report([self.finding()], ["src/cascadia/TerminalApp/Test.xaml"]), queue, transport,
            )

    def test_publication_noop_requires_no_push_request(self):
        queue = self.workspace / "safeoutputs.jsonl"
        queue.write_text(json.dumps({"type": "noop", "message": "No eligible repair"}) + "\n")
        MODULE.validate(self.root, self.head, self.head, False, self.prepared, self.report(), queue, self.workspace)
        queue.write_text(json.dumps({"type": "push_to_pull_request_branch"}) + "\n")
        with self.assertRaisesRegex(ValueError, "exactly one noop"):
            MODULE.validate(self.root, self.head, self.head, False, self.prepared, self.report(), queue, self.workspace)

    def test_recipe_rejects_unrelated_line_ending_changes(self):
        path = self.root / "src/cascadia/TerminalApp/Test.xaml"
        original = MODULE._git(self.root, "show", f"{self.head}:src/cascadia/TerminalApp/Test.xaml")
        expected = MODULE._remove_raw_view_at_line(original, 1)
        changed = expected.replace("\r\n", "\n") if "\r\n" in expected else expected.replace("\n", "\r\n")
        path.write_bytes(changed.encode("utf-8"))
        with self.assertRaisesRegex(ValueError, "changes beyond the trusted"):
            MODULE.validate(
                self.root, self.head, self.head, True, self.prepared,
                self.report([self.finding()], ["src/cascadia/TerminalApp/Test.xaml"]),
            )

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

    def test_omitted_high_signal_with_noop_is_rejected(self):
        report = self.report(dismissed_signals=[])
        data = json.loads(report.read_text())
        del data["dismissed_signals"]
        report.write_text(json.dumps(data))
        queue = self.workspace / "safeoutputs.jsonl"
        queue.write_text(json.dumps({"type": "noop", "message": "No eligible repair"}) + "\n")
        with self.assertRaisesRegex(ValueError, "AX-HIGH-1: prepared signal must be reported"):
            MODULE.validate(self.root, self.head, self.head, True, self.prepared, report, queue, self.workspace)

    def test_matching_remaining_finding_accounts_for_signal_without_dismissal_field(self):
        report = self.report([self.finding(disposition="remaining")])
        data = json.loads(report.read_text())
        del data["dismissed_signals"]
        report.write_text(json.dumps(data))
        MODULE.validate(self.root, self.head, self.head, True, self.prepared, report)

    def test_explicit_false_positive_is_not_serialized_as_finding_or_pass(self):
        report = self.report()
        before = report.read_text()
        MODULE.validate(self.root, self.head, self.head, True, self.prepared, report)
        self.assertEqual(before, report.read_text())
        data = json.loads(report.read_text())
        self.assertEqual([], data["findings"])
        self.assertEqual([], data["patch_files"])
        self.assertEqual("SKIPPED", data["runtime_checks"][0]["status"])
        self.assertEqual("AX-HIGH-1", data["dismissed_signals"][0]["stable_id"])

    def test_invalid_dismissals_are_rejected(self):
        valid = json.loads(self.report().read_text())["dismissed_signals"][0]
        cases = [
            ("unknown", [{**valid, "stable_id": "AX-UNKNOWN"}], "known prepared"),
            ("duplicate", [valid, valid], "duplicate dismissed"),
            ("missing reason", [{key: value for key, value in valid.items() if key != "reason"}], "reason"),
            ("missing evidence", [{key: value for key, value in valid.items() if key != "evidence"}], "evidence"),
            ("blank reason", [{**valid, "reason": "  "}], "reason"),
            ("blank evidence", [{**valid, "evidence": "\n"}], "evidence"),
            ("verdict only", [{**valid, "reason": "false positive"}], "reason"),
            ("wrong type", [{**valid, "evidence": ["Test.xaml"]}], "evidence"),
            ("nonobject", [None], "object"),
            ("nonarray", {}, "array"),
        ]
        for name, dismissals, message in cases:
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, message):
                MODULE.validate(
                    self.root, self.head, self.head, True, self.prepared,
                    self.report(dismissed_signals=dismissals),
                )

    def test_signal_cannot_be_reported_and_dismissed(self):
        dismissal = json.loads(self.report().read_text())["dismissed_signals"][0]
        with self.assertRaisesRegex(ValueError, "both reported and dismissed"):
            MODULE.validate(
                self.root, self.head, self.head, True, self.prepared,
                self.report([self.finding(disposition="remaining")], dismissed_signals=[dismissal]),
            )

    def test_duplicate_findings_are_rejected(self):
        finding = self.finding(disposition="remaining")
        with self.assertRaisesRegex(ValueError, "duplicate finding"):
            MODULE.validate(self.root, self.head, self.head, True, self.prepared, self.report([finding, finding]))

    def test_new_findings_do_not_account_for_prepared_signal(self):
        finding = self.finding(stable_id="AX-NEW", disposition="remaining")
        with self.assertRaisesRegex(ValueError, "AX-HIGH-1: prepared signal must be reported"):
            MODULE.validate(self.root, self.head, self.head, True, self.prepared, self.report([finding]))
        dismissal = json.loads(self.report().read_text())["dismissed_signals"][0]
        MODULE.validate(
            self.root, self.head, self.head, True, self.prepared,
            self.report([finding], dismissed_signals=[dismissal]),
        )

    def test_empty_prepared_signals_allow_legacy_empty_report(self):
        prepared = json.loads(self.prepared.read_text())
        prepared["static_findings"] = []
        self.prepared.write_text(json.dumps(prepared))
        report = self.report(dismissed_signals=[])
        data = json.loads(report.read_text())
        del data["dismissed_signals"]
        report.write_text(json.dumps(data))
        MODULE.validate(self.root, self.head, self.head, True, self.prepared, report)

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

    def test_agent_markdown_summary_is_not_parsed_as_a_verdict(self):
        summary = self.workspace / "summary.md"
        for encoding in ("utf-8", "utf-8-sig"):
            with self.subTest(encoding=encoding):
                summary.write_text("## Source review\n\n- No automatic repair requested.\n", encoding=encoding)
                MODULE.validate(self.root, self.head, self.head, True, self.prepared,
                                self.report(), summary_path=summary)

    def test_agent_summary_must_be_present_nonempty_utf8_and_bounded(self):
        summary = self.workspace / "summary.md"
        with self.assertRaisesRegex(ValueError, "agent summary verification failed"):
            MODULE.validate(self.root, self.head, self.head, True, self.prepared,
                            self.report(), summary_path=summary)
        for content in (b"", b" \n\t", b"\xef\xbb\xbf", b"\xef\xbb\xbf \n", b"\xff", b"x" * (32 * 1024 + 1)):
            with self.subTest(content_length=len(content)):
                summary.write_bytes(content)
                with self.assertRaisesRegex(ValueError, "agent summary verification failed"):
                    MODULE.validate(self.root, self.head, self.head, True, self.prepared,
                                    self.report(), summary_path=summary)

    def test_agent_summary_symlink_is_rejected(self):
        target = self.workspace / "narrative.md"
        target.write_text("## Outcome\nReview complete.\n", encoding="utf-8")
        summary = self.workspace / "summary.md"
        try:
            summary.symlink_to(target)
        except OSError as error:
            if os.name == "nt" and getattr(error, "winerror", None) == 1314:
                self.skipTest("Windows symlink creation privilege is unavailable")
            raise
        with self.assertRaisesRegex(ValueError, "summary must be a regular file"):
            MODULE.validate(self.root, self.head, self.head, True, self.prepared,
                            self.report(), summary_path=summary)

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

    def test_idl_is_reviewable_but_not_patchable(self):
        self.assertIn(".idl", MODULE.UI_SUFFIXES)
        self.assertNotIn(".idl", MODULE.PATCH_SUFFIXES)

    def test_test_host_review_does_not_expand_publication_paths(self):
        relative = f"{MODULE.TEST_HOST_ROOT}Test.xaml"
        path = self.root / relative
        path.parent.mkdir(parents=True)
        path.write_text('<Button Content="Open" />\n', encoding="utf-8")
        subprocess.run(["git", "add", relative], cwd=self.root, check=True)
        with self.assertRaisesRegex(ValueError, "outside the native UI/test allowlist"):
            MODULE.validate(self.root, self.head, self.head, True, self.prepared,
                            self.report([], [relative]))

    def test_changed_ambient_configuration_blocks_preparation(self):
        for relative in ("AGENTS.md", ".github/hooks/launch.json", "src/cascadia/TerminalApp/AGENTS.md",
                         "test/accessibility/Invoke-AxeWindowsScan.ps1"):
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("PR-controlled operating configuration\n", encoding="utf-8")
        subprocess.run(["git", "add", "."], cwd=self.root, check=True)
        subprocess.run(["git", "commit", "-qm", "Change ambient configuration"], cwd=self.root, check=True)
        head = MODULE._git(self.root, "rev-parse", "HEAD").strip()
        with self.assertRaisesRegex(ValueError, "blocked before agent startup"):
            MODULE.prepare(self.root, self.head, head, self.workspace / "blocked.json")
        self.assertFalse((self.workspace / "blocked.json").exists())

    def test_source_only_changes_preserve_instruction_boundary(self):
        path = self.root / "src/cascadia/TerminalApp/Test.xaml"
        path.write_text('<Button Width="200" Content="Open" />\n', encoding="utf-8")
        subprocess.run(["git", "add", "."], cwd=self.root, check=True)
        subprocess.run(["git", "commit", "-qm", "Change only source"], cwd=self.root, check=True)
        head = MODULE._git(self.root, "rev-parse", "HEAD").strip()
        MODULE.prepare(self.root, self.head, head, self.workspace / "source-only.json")
        self.assertTrue((self.workspace / "source-only.json").exists())

    def test_git_replacement_refs_do_not_change_reviewed_blobs(self):
        path = self.root / "src/cascadia/TerminalApp/Test.xaml"
        path.write_text('<Button Content="Forged" />\n', encoding="utf-8")
        subprocess.run(["git", "add", "."], cwd=self.root, check=True)
        subprocess.run(["git", "commit", "-qm", "Create replacement candidate"], cwd=self.root, check=True)
        replacement = MODULE._git(self.root, "rev-parse", "HEAD").strip()
        subprocess.run(["git", "replace", self.head, replacement], cwd=self.root, check=True)
        original = MODULE._git(self.root, "show", f"{self.head}:src/cascadia/TerminalApp/Test.xaml")
        self.assertIn('AccessibilityView="Raw"', original)
        self.assertNotIn("Forged", original)

    def test_advisory_details_survive_in_summary(self):
        finding = self.finding(
            severity="MEDIUM",
            disposition="advice",
            observed="Observed advisory detail",
            expected="Expected advisory detail",
            evidence="Advisory source evidence",
            proposed_fix="Suggested advisory fix",
        )
        summary = io.StringIO()
        with contextlib.redirect_stdout(summary):
            MODULE.validate(
                self.root, self.head, self.head, True, self.prepared,
                self.report([self.finding(disposition="remaining"),
                             {**finding, "stable_id": "AX-ADVICE-1"}])
            )
        for detail in ("Advisory findings", finding["file"], finding["observed"], finding["expected"],
                       finding["evidence"], finding["proposed_fix"]):
            self.assertIn(detail, summary.getvalue())

    def test_literal_accessible_string_patch_is_rejected(self):
        path = self.root / "src/cascadia/TerminalApp/Test.xaml"
        path.write_text('<Button AutomationProperties.Name="Open" />\n', encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "changes beyond the trusted"):
            MODULE.validate(
                self.root,
                self.head,
                self.head,
                True,
                self.prepared,
                self.report([self.finding()], ["src/cascadia/TerminalApp/Test.xaml"]),
            )

    def test_recipe_preserves_existing_literal_name_advice_on_same_line(self):
        path = self.root / "src/cascadia/TerminalApp/Test.xaml"
        original = '<Button Content="Open" AutomationProperties.AccessibilityView="Raw" AutomationProperties.Name="Open document" />\n'
        path.write_bytes(original.encode("utf-8"))
        subprocess.run(["git", "add", "."], cwd=self.root, check=True)
        subprocess.run(["git", "commit", "-qm", "Review head has literal name"], cwd=self.root, check=True)
        self.head = MODULE._git(self.root, "rev-parse", "HEAD").strip()
        prepared = json.loads(self.prepared.read_text())
        prepared["source_sha"] = self.head
        self.prepared.write_text(json.dumps(prepared))
        path.write_bytes(MODULE._remove_raw_view_at_line(original, 1).encode("utf-8"))
        advice = self.finding(stable_id="AX-LITERAL-1", severity="MEDIUM", disposition="advice", repair_recipe=None)
        MODULE.validate(
            self.root, self.head, self.head, True, self.prepared,
            self.report([self.finding(), advice], ["src/cascadia/TerminalApp/Test.xaml"]),
        )

    def test_stale_prepared_evidence_is_rejected(self):
        prepared = json.loads(self.prepared.read_text())
        prepared["source_sha"] = "0" * 40
        self.prepared.write_text(json.dumps(prepared))
        with self.assertRaisesRegex(ValueError, "prepared evidence does not match"):
            MODULE.validate(self.root, self.head, self.head, True, self.prepared, self.report())

    def test_signal_only_report_without_domain_review_is_rejected(self):
        report = self.report()
        data = json.loads(report.read_text())
        del data["review_checks"]
        report.write_text(json.dumps(data))
        with self.assertRaisesRegex(ValueError, "review_checks must be an array"):
            MODULE.validate(self.root, self.head, self.head, True, self.prepared, report)

    def test_review_dimensions_cannot_be_omitted_repeated_or_invented(self):
        for change in ("omit", "repeat", "invent"):
            with self.subTest(change=change):
                report = self.report()
                data = json.loads(report.read_text())
                if change == "omit":
                    data["review_checks"].pop()
                elif change == "repeat":
                    data["review_checks"].append(data["review_checks"][0])
                else:
                    data["review_checks"][0]["id"] = "unrecognized"
                report.write_text(json.dumps(data))
                with self.assertRaisesRegex(ValueError, "every required review dimension"):
                    MODULE.validate(self.root, self.head, self.head, True, self.prepared, report)

    def test_domain_evidence_must_match_immutable_source_not_worktree(self):
        report = self.report()
        data = json.loads(report.read_text())
        data["review_checks"][0]["evidence"][0]["text"] = "Fabricated accessible implementation"
        report.write_text(json.dumps(data))
        with self.assertRaisesRegex(ValueError, "does not match immutable"):
            MODULE.validate(self.root, self.head, self.head, True, self.prepared, report)

    def test_model_cannot_mark_a_review_dimension_as_runtime_pass(self):
        report = self.report()
        data = json.loads(report.read_text())
        data["review_checks"][0]["status"] = "PASS"
        report.write_text(json.dumps(data))
        with self.assertRaisesRegex(ValueError, "distinguish source review"):
            MODULE.validate(self.root, self.head, self.head, True, self.prepared, report)


@unittest.skipUnless(shutil.which("node"), "Node required for trusted report scenarios")
class WorkflowContractTests(unittest.TestCase):
    def test_agent_summary_is_written_from_template_and_displayed_unchanged(self):
        root = Path(__file__).parents[4]
        workflow = (root / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        self.assertIn("--summary /tmp/gh-aw/accessibility/summary.md", workflow)
        self.assertIn("        /tmp/gh-aw/accessibility/summary.md", workflow)
        self.assertIn("## Human action and limits", workflow)
        self.assertIn("| Proposed fix commit |", workflow)
        self.assertIn("[commit SHA](COMMIT_URL)", workflow)
        self.assertIn("`${{ github.server_url }}`", workflow)
        self.assertIn("prepared `publication.repository`", workflow)
        self.assertIn('cat /tmp/gh-aw/accessibility/summary.md >> "$GITHUB_STEP_SUMMARY"', workflow)
        self.assertIn("original PR", workflow)
        self.assertIn("may revert any fix commit", workflow)
        self.assertIn("only once below the table", workflow)
        self.assertIn("set `review_checks`, `findings`, `dismissed_signals` and `patch_files`", workflow)
        self.assertIn("preserve every prepared runtime", workflow)
        self.assertIn("Include this paragraph only if repair commits exist; otherwise remove it.", workflow)
        self.assertNotIn("script: |", workflow)
        self.assertNotIn("summary.push(", workflow)

    def test_native_discovery_is_preserved_without_a_summary_renderer(self):
        root = Path(__file__).parents[4]
        workflow = (root / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        self.assertNotIn("\n  accessibility-report:", workflow)
        self.assertIn("\n  native-runtime:", workflow)
        self.assertNotIn("checks: write", workflow)
        compiled = (root / ".github/workflows/ghaw-pr-accessibility.lock.yml").read_text(encoding="utf-8")
        self.assertNotIn("\n  accessibility-report:", compiled)
        self.assertIn("\n  native-runtime:", compiled)
        self.assertIn('cat /tmp/gh-aw/accessibility/summary.md', compiled)

    def test_support_paths_trigger_trusted_base_validation(self):
        root = Path(__file__).parents[4]
        workflow = (root / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        trigger = workflow.split("\npermissions:", 1)[0]
        for path in ("src/cascadia/LocalTests_TerminalApp/TestHostApp/**",
                     ".github/agents/pr-accessibility.agent.md", ".github/skills/pr-accessibility/**",
                     ".github/workflows/ghaw-pr-accessibility.lock.yml", "test/accessibility/**",
                     "build/scripts/Get-DependenciesFromAppxRecipe.ps1"):
            self.assertIn(path, trigger)

    def test_agent_and_detection_have_explicit_spend_caps(self):
        root = Path(__file__).parents[4]
        workflow = (root / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        self.assertIn("  model: auto", workflow)
        self.assertIn("max-daily-ai-credits: 100000", workflow)
        self.assertEqual(1, workflow.count("max-ai-credits: 500"))
        self.assertEqual(1, workflow.count("max-ai-credits: 30"))
        self.assertIn("timeout-minutes: 15", workflow)
        self.assertIn("retries: 0", workflow)

    @unittest.skipUnless(os.name == "nt" and shutil.which("pwsh"), "Windows PowerShell host required")
    def test_compiled_agent_disables_custom_instructions(self):
        root = Path(__file__).parents[4]
        compiled = (root / ".github/workflows/ghaw-pr-accessibility.lock.yml").read_text(encoding="utf-8")
        self.assertIn("--no-custom-instructions", compiled)
        self.assertIn("--agent pr-accessibility", compiled)

    def test_failed_validation_blocks_compiled_publication(self):
        root = Path(__file__).parents[4]
        compiled = (root / ".github/workflows/ghaw-pr-accessibility.lock.yml").read_text(encoding="utf-8")
        publication = re.split(r"\n  [a-zA-Z][\w-]*:\n", compiled.split("\n  safe_outputs:\n", 1)[1], maxsplit=1)[0]
        condition = publication.split("\n    if:", 1)[1].split("\n    runs-on:", 1)[0]
        self.assertIn("needs.agent.result == 'success'", condition)

    def test_validated_report_is_uploaded_after_validation(self):
        root = Path(__file__).parents[4]
        workflow = (root / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        self.assertLess(workflow.index("name: Check findings and repair publication policy"),
                        workflow.index("name: Upload source review and agent-written summary"))
        upload = workflow.split("name: Upload source review and agent-written summary", 1)[1].split("\n---", 1)[0]
        self.assertIn("/tmp/gh-aw/accessibility/final.json", upload)
        self.assertIn("/tmp/gh-aw/accessibility/summary.md", upload)

    def test_skill_is_staged_from_trusted_base_before_head_checkout(self):
        root = Path(__file__).parents[4]
        workflow = (root / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        stage = workflow.index('git show "$BASE_SHA:.github/skills/pr-accessibility/SKILL.md"')
        checkout = workflow.index('git checkout -B "$HEAD_REF" "$HEAD_SHA"')
        self.assertLess(stage, checkout)
        self.assertRegex(workflow, r"follow the\s+staged skill")
        self.assertIn('--prepared "$TRUSTED_ACCESSIBILITY/prepared.json"', workflow)
        self.assertIn("--no-replace-objects", workflow)
        compiled = (root / ".github/workflows/ghaw-pr-accessibility.lock.yml").read_text(encoding="utf-8")
        self.assertIn('--mount "${RUNNER_TEMP}/gh-aw:${RUNNER_TEMP}/gh-aw:ro"', compiled)

    def test_preparation_preserves_named_pr_branch_for_transport(self):
        root = Path(__file__).parents[4]
        workflow = (root / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        self.assertIn("HEAD_REF: ${{ github.event.pull_request.head.ref }}", workflow)
        self.assertLess(workflow.index('git check-ref-format --branch "$HEAD_REF"'),
                        workflow.index('git checkout -B "$HEAD_REF" "$HEAD_SHA"'))
        self.assertIn("one separate commit per eligible HIGH finding", workflow)

    def test_source_review_and_publication_are_independent_of_native_discovery(self):
        root = Path(__file__).parents[4]
        compiled = (root / ".github/workflows/ghaw-pr-accessibility.lock.yml").read_text(encoding="utf-8")
        agent = re.split(r"\n  [a-zA-Z][\w-]*:\n", compiled.split("\n  agent:\n", 1)[1], maxsplit=1)[0]
        self.assertRegex(agent, r"(?m)^    needs: activation$")
        self.assertIn("\n  native-runtime:\n", compiled)
        self.assertIn("uses: ./.github/workflows/native-accessibility.yml", compiled)
        publisher = re.split(r"\n  [a-zA-Z][\w-]*:\n", compiled.split("\n  safe_outputs:\n", 1)[1], maxsplit=1)[0]
        dependencies = publisher.split("    needs:\n", 1)[1].split("\n    if:", 1)[0]
        self.assertNotIn("native", dependencies)

    def test_pr_publication_is_single_mode_and_fail_closed(self):
        root = Path(__file__).parents[4]
        workflow = (root / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        self.assertIn("push-to-pull-request-branch:", workflow)
        self.assertIn("fallback-as-pull-request: false", workflow)
        self.assertIn("--current-head \"$CURRENT_HEAD_SHA\"", workflow)
        self.assertIn('gh api --hostname "${GITHUB_SERVER_URL#*://}"', workflow)
        frontmatter = workflow.split("---", 2)[1]
        self.assertNotIn("add-comment:", frontmatter)
        self.assertNotIn("submit-pull-request-review:", frontmatter)

    def test_native_discovery_keeps_its_isolated_windows_lane(self):
        root = Path(__file__).parents[4]
        caller = (root / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        self.assertIn("\n  native-runtime:\n", caller)
        workflow = (root / ".github/workflows/native-accessibility.yml").read_text(encoding="utf-8")
        self.assertIn("runs-on: windows-2025-vs2026", workflow)
        self.assertIn("persist-credentials: false", workflow)
        self.assertIn("AECA43F41C89B3FFB1DB84011539E609ECD7CB3BADD6E78FADA2ADA327D10A64", workflow)
        self.assertIn("-Surface fre", workflow)
        self.assertIn("-Surface agents", workflow)
        self.assertIn("-Surface fre-settings", workflow)
        self.assertIn("Get-DependenciesFromAppxRecipe.ps1", workflow)
        self.assertIn("shell: powershell", workflow)
        self.assertIn("FRE_SETTINGS_OUTCOME", workflow)
        self.assertIn("name: Checkout trusted native harness", workflow)
        self.assertIn("trusted-accessibility\\test\\accessibility\\Invoke-AxeWindowsTestHost.ps1", workflow)
        self.assertIn("hierarchy lacks its visible state marker", workflow)
        self.assertIn("Native accessibility scan failed or was blocked", workflow)

    def test_native_runtime_harness_targets_only_test_host_package(self):
        root = Path(__file__).parents[4]
        harness = (root / "test/accessibility/Invoke-AxeWindowsTestHost.ps1").read_text(encoding="utf-8")
        scan = (root / "test/accessibility/Invoke-AxeWindowsScan.ps1").read_text(encoding="utf-8")
        self.assertIn("WindowsTerminal.TestHost", harness)
        self.assertNotIn("Microsoft.IntelligentTerminal", harness)
        self.assertIn("SourceSha", harness)
        self.assertIn("failed before producing results", harness)
        self.assertIn("Failed to restore the previous WindowsTerminal.TestHost registration", harness)
        self.assertIn("OutputFileFormat]::None", scan)
        self.assertIn("Axe.Windows.Automation.ScannerFactory", scan)
        self.assertIn("there is no runtime accessibility evidence", scan)
        self.assertIn("VerifyInteractiveDesktop", harness)
        self.assertIn("$process.SessionId", harness)
        self.assertIn("$dependency.MinVersion", harness)
        self.assertIn("[version]$_.Version -ge [version]$dependency.MinVersion", harness)
        self.assertIn("Wait-VisibleElement", scan)
        self.assertIn("'TabModeComboBox'", scan)
        self.assertIn("'uia-tree.json'", scan)


if __name__ == "__main__":
    unittest.main()
