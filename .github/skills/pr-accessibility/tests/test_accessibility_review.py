import importlib.util
import contextlib
import io
import json
import os
import re
import shutil
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path


SCRIPT = Path(__file__).parents[1] / "scripts" / "accessibility_review.py"
SPEC = importlib.util.spec_from_file_location("accessibility_review", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
SPEC.loader.exec_module(MODULE)


class StaticAnalysisTests(unittest.TestCase):
    def test_visual_style_is_not_a_trusted_repair_name_source(self):
        xaml = '<Button Style="{StaticResource VisualButton}" AutomationProperties.AccessibilityView="Raw" />'
        with self.assertRaisesRegex(ValueError, "existing accessible name source"):
            MODULE._remove_raw_view_at_line(xaml, 1)

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

    def test_deletion_only_preserves_base_evidence_and_allows_unrepaired_high(self):
        with self.source_fixture() as (workspace, root, path, base):
            original = path.read_bytes().decode("utf-8")
            relative = path.relative_to(root).as_posix()
            path.unlink()
            MODULE._git(root, "add", ".")
            MODULE._git(root, "commit", "-qm", "delete UI")
            head = MODULE._git(root, "rev-parse", "HEAD").strip()
            prepared_path = workspace / "prepared.json"
            MODULE.prepare(root, base, head, prepared_path, None)
            first = prepared_path.read_text()
            MODULE.prepare(root, base, head, prepared_path, None)
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
                MODULE.prepare(root, base, head, prepared_path, None)
                prepared = json.loads(prepared_path.read_text())
                MODULE._git(root, "config", "diff.renames", "false")
                MODULE.prepare(root, base, head, prepared_path, None)
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
            MODULE.prepare(root, base, head, prepared_path, None)
            prepared = json.loads(prepared_path.read_text())
            signal = MODULE._scan_xaml(relative, original, set())[0]
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
            MODULE.prepare(root, base, head, first, None)
            MODULE.prepare(root, base, head, second, None)
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
        self.temp = tempfile.TemporaryDirectory()
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

    def publication(self):
        subprocess.run(["git", "checkout", "-qb", "fixture/head"], cwd=self.root, check=True)
        path = self.root / "src/cascadia/TerminalApp/Test.xaml"
        original = path.read_bytes().decode("utf-8")
        path.write_bytes(MODULE._remove_raw_view_at_line(original, 1).encode("utf-8"))
        subprocess.run(["git", "add", "."], cwd=self.root, check=True)
        subprocess.run(["git", "commit", "-qm", "Restore UIA view [native-accessibility]"], cwd=self.root, check=True)
        transport = self.workspace / "transport"
        transport.mkdir()
        patch = subprocess.check_output(["git", "format-patch", "-1", "--stdout"], cwd=self.root)
        (transport / "aw-fixture-head.patch").write_bytes(patch)
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
        MODULE.validate(
            self.root, self.head, self.head, True, self.prepared,
            self.report([self.finding()], ["src/cascadia/TerminalApp/Test.xaml"]), queue, transport,
        )

    def test_publication_rejects_changes_after_capture(self):
        queue, transport = self.publication()
        path = self.root / "src/cascadia/TerminalApp/Test.xaml"
        path.write_bytes(MODULE._git(self.root, "show", f"{self.head}:src/cascadia/TerminalApp/Test.xaml").encode("utf-8"))
        with self.assertRaisesRegex(ValueError, "clean committed candidate"):
            MODULE.validate(
                self.root, self.head, self.head, True, self.prepared,
                self.report([self.finding()], []), queue, transport,
            )

    def test_publication_rejects_tampered_patch_body(self):
        queue, transport = self.publication()
        patch = transport / "aw-fixture-head.patch"
        patch.write_bytes(patch.read_bytes().replace(b'+<Button Content="Open"', b'+<Button Content="Altered"'))
        with self.assertRaisesRegex(ValueError, "patch tree differs"):
            MODULE.validate(
                self.root, self.head, self.head, True, self.prepared,
                self.report([self.finding()], ["src/cascadia/TerminalApp/Test.xaml"]), queue, transport,
            )

    def test_publication_rejects_stale_patch_commit_header(self):
        queue, transport = self.publication()
        patch = transport / "aw-fixture-head.patch"
        candidate = MODULE._git(self.root, "rev-parse", "HEAD").strip()
        patch.write_bytes(patch.read_bytes().replace(candidate.encode("ascii"), b"0" * 40, 1))
        with self.assertRaisesRegex(ValueError, "does not identify the validated"):
            MODULE.validate(
                self.root, self.head, self.head, True, self.prepared,
                self.report([self.finding()], ["src/cascadia/TerminalApp/Test.xaml"]), queue, transport,
            )

    def test_publication_rejects_alternate_bundle_transport(self):
        queue, transport = self.publication()
        (transport / "aw-fixture-head.bundle").write_bytes(b"unvalidated alternate transport")
        with self.assertRaisesRegex(ValueError, "alternate bundle transport"):
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
        with self.assertRaisesRegex(ValueError, "one repair commit directly"):
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

    def test_idl_is_reviewable_but_not_patchable(self):
        self.assertIn(".idl", MODULE.UI_SUFFIXES)
        self.assertNotIn(".idl", MODULE.PATCH_SUFFIXES)

    def test_changed_ambient_configuration_blocks_preparation(self):
        for relative in ("AGENTS.md", ".github/hooks/launch.json", "src/cascadia/TerminalApp/AGENTS.md"):
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("PR-controlled operating configuration\n", encoding="utf-8")
        subprocess.run(["git", "add", "."], cwd=self.root, check=True)
        subprocess.run(["git", "commit", "-qm", "Change ambient configuration"], cwd=self.root, check=True)
        head = MODULE._git(self.root, "rev-parse", "HEAD").strip()
        with self.assertRaisesRegex(ValueError, "blocked before agent startup"):
            MODULE.prepare(self.root, self.head, head, self.workspace / "blocked.json", None)
        self.assertFalse((self.workspace / "blocked.json").exists())

    def test_source_only_changes_preserve_instruction_boundary(self):
        path = self.root / "src/cascadia/TerminalApp/Test.xaml"
        path.write_text('<Button Width="200" Content="Open" />\n', encoding="utf-8")
        subprocess.run(["git", "add", "."], cwd=self.root, check=True)
        subprocess.run(["git", "commit", "-qm", "Change only source"], cwd=self.root, check=True)
        head = MODULE._git(self.root, "rev-parse", "HEAD").strip()
        MODULE.prepare(self.root, self.head, head, self.workspace / "source-only.json", None)
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
                self.root, self.head, self.head, True, self.prepared, self.report([finding])
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


class WorkflowContractTests(unittest.TestCase):
    def test_agent_and_detection_have_explicit_spend_caps(self):
        root = Path(__file__).parents[4]
        workflow = (root / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        self.assertIn("max-daily-ai-credits: 90", workflow)
        self.assertEqual(2, workflow.count("max-ai-credits: 30"))
        self.assertIn("timeout-minutes: 15", workflow)
        self.assertIn("retries: 0", workflow)

    @unittest.skipUnless(os.name == "nt" and shutil.which("pwsh"), "Windows PowerShell host required")
    def test_native_enforcement_rejects_success_without_state_evidence(self):
        root = Path(__file__).parents[4]
        workflow = (root / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        section = workflow.split("      - name: Enforce native scan results", 1)[1].split("\nsteps:", 1)[0]
        script = textwrap.dedent(section.split("        run: |\n", 1)[1])
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            sha = "a" * 40
            markers = {"fre": "NextButton", "fre-settings": "TabModeComboBox", "agents": "AcpAgentComboBox"}
            for surface, marker in markers.items():
                output = directory / "accessibility-results" / surface
                output.mkdir(parents=True)
                scan_path = output / "axe-results.json"
                tree_path = output / "uia-tree.json"
                (output / "result.json").write_text(json.dumps({
                    "status": "PASS", "source_sha": sha, "surface": surface,
                    "process_id": 42, "axe_results": str(scan_path),
                }), encoding="utf-8")
                scan_path.write_text(json.dumps({
                    "scan_id": surface, "process_id": 42, "window_count": 1, "error_count": 0,
                    "visible_state_marker": marker, "uia_tree": str(tree_path),
                }), encoding="utf-8")
                tree_path.write_text(json.dumps([{
                    "automation_id": marker, "is_offscreen": False, "process_id": 42,
                }]), encoding="utf-8")
            environment = dict(os.environ, RUNNER_TEMP=temporary, SOURCE_SHA=sha,
                               FRE_OUTCOME="success", AGENTS_OUTCOME="success", FRE_SETTINGS_OUTCOME="success")

            def enforce():
                return subprocess.run(
                    ["pwsh", "-NoProfile", "-Command", script],
                    env=environment, capture_output=True, text=True, timeout=30,
                )

            valid = enforce()
            self.assertEqual(0, valid.returncode, valid.stderr)
            tree_path.write_text("[]", encoding="utf-8")
            empty = enforce()
            self.assertNotEqual(0, empty.returncode)
            self.assertIn("hierarchy lacks", empty.stderr)
            (directory / "accessibility-results" / "fre" / "result.json").unlink()
            missing = enforce()
            self.assertNotEqual(0, missing.returncode)

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
        self.assertLess(workflow.index("name: Validate final findings"), workflow.index("name: Upload validated accessibility report"))
        self.assertIn("path: /tmp/gh-aw/accessibility/final.json", workflow)

    def test_skill_is_staged_from_trusted_base_before_head_checkout(self):
        root = Path(__file__).parents[4]
        workflow = (root / ".github/workflows/ghaw-pr-accessibility.md").read_text(encoding="utf-8")
        stage = workflow.index('git show "$BASE_SHA:.github/skills/pr-accessibility/SKILL.md"')
        checkout = workflow.index('git checkout -B "$HEAD_REF" "$HEAD_SHA"')
        self.assertLess(stage, checkout)
        self.assertIn("Follow `$RUNNER_TEMP/gh-aw/accessibility-trusted/SKILL.md`", workflow)
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
        self.assertIn("create\na real local Git commit", workflow)

    def test_compiled_source_review_does_not_wait_for_native_runtime(self):
        root = Path(__file__).parents[4]
        compiled = (root / ".github/workflows/ghaw-pr-accessibility.lock.yml").read_text(encoding="utf-8")
        agent = re.split(r"\n  [a-zA-Z][\w-]*:\n", compiled.split("\n  agent:\n", 1)[1], maxsplit=1)[0]
        self.assertRegex(agent, r"(?m)^    needs: activation$")
        runtime = re.split(r"\n  [a-zA-Z][\w-]*:\n", compiled.split("\n  native-runtime:\n", 1)[1], maxsplit=1)[0]
        self.assertRegex(runtime, r"(?m)^    needs: agent$")

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
