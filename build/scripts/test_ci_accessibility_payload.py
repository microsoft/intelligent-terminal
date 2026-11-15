"""Recipe staging and native result tests; fixtures are not usable native builds."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
import zipfile

SPEC = importlib.util.spec_from_file_location("payload", Path(__file__).with_name("ci-accessibility-payload.py"))
PAYLOAD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PAYLOAD)
SCRATCH = Path(__file__).resolve().parents[2] / "Generated Files"
SCRATCH.mkdir(exist_ok=True)
IDENTITY = {"GITHUB_REPOSITORY": "microsoft/intelligent-terminal", "GITHUB_REPOSITORY_ID": "42",
            "EXPECTED_HEAD_SHA": "a" * 40, "CI_RUN_ID": "91", "CI_RUN_ATTEMPT": "2", "PR_NUMBER": "17"}


class PayloadTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(dir=SCRATCH)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.workspace = self.root / "source"
        self.sdk = self.root / "Program Files (x86)/Microsoft SDKs/Windows Kits/10/ExtensionSDKs"
        self.environment = {**IDENTITY, "ProgramFiles(x86)": str(self.root / "Program Files (x86)")}
        self.host = self.workspace / "bin/x64/Debug/TestHostApp"
        self.host.mkdir(parents=True)
        self.recipe = ET.Element("Project")
        props = ET.SubElement(self.recipe, "PropertyGroup")
        ET.SubElement(props, "PackageArchitecture").text = "x64"
        self.items = ET.SubElement(self.recipe, "ItemGroup")
        for name in sorted(PAYLOAD.REQUIRED):
            file = self.host / name[4:]
            file.parent.mkdir(parents=True, exist_ok=True)
            file.write_bytes(name.encode())
            # AfterBuild copies these runtime files without adding recipe entries.
            if name in {"app/taef.png", "app/TerminalApp.LocalTests.dll", "app/Wex.Common.dll"}:
                continue
            item = ET.SubElement(self.items, "AppXManifest" if name.endswith("/AppxManifest.xml") else "AppxPackagedFile",
                                 Include=str(file))
            ET.SubElement(item, "PackagePath").text = name[4:].replace("/", "\\")
        self.external = self.workspace / "src/resource/Agent.svg"
        self.external.parent.mkdir(parents=True)
        self.external.write_bytes(b"resource outside the loose layout")
        item = ET.SubElement(self.items, "AppxPackagedFile", Include=str(self.external))
        ET.SubElement(item, "PackagePath").text = "AgentIcons/Agent.svg"
        for name in sorted(PAYLOAD.FRAMEWORKS):
            file = self.workspace / f"packages/{name}.appx"
            file.parent.mkdir(parents=True, exist_ok=True)
            with zipfile.ZipFile(file, "w") as archive:
                archive.writestr("AppxManifest.xml", f'<Package><Identity Name="{name}" /></Package>')
            item = ET.SubElement(self.items, "ResolvedSDKReference", Include=str(file))
            for key, value in {"Name": name, "Architecture": "x64", "AppxLocation": str(file)}.items():
                ET.SubElement(item, key).text = value
        self.output = self.root / "staging"

    def stage(self):
        ET.ElementTree(self.recipe).write(self.host / "TestHostApp.build.appxrecipe")
        PAYLOAD.stage(self.workspace, self.output, self.environment)

    def sdk_runtime(self):
        source = self.sdk / "Microsoft.UniversalCRT.Debug/10.0.26100.0/Redist/Debug/x64/ucrtbased.dll"
        source.parent.mkdir(parents=True, exist_ok=True)
        source.write_bytes(b"MZ" + bytes(58) + (64).to_bytes(4, "little") + b"PE\x00\x00\x64\x86")
        return source

    def test_actual_sdk_recipe_and_after_build_content_are_portable(self):
        source = self.sdk_runtime()
        fragment = ET.fromstring(Path(__file__).with_name("testhost-sdk-recipe.xml").read_bytes())
        item, = PAYLOAD.children(fragment, "AppxPackagedFile")
        item.set("Include", str(source).replace("(x86)", "%28x86%29"))
        self.items.append(item)
        package = self.workspace / "packages/Microsoft.VCLibs.140.00.Debug.appx"
        external = self.sdk / "Microsoft.VCLibs/14.0/AppX/Debug/x64/Microsoft.VCLibs.x64.Debug.14.00.appx"
        external.parent.mkdir(parents=True)
        external.write_bytes(package.read_bytes())
        for entry in self.items.findall("ResolvedSDKReference"):
            if entry.findtext("Name") == "Microsoft.VCLibs.140.00.Debug":
                entry.find("AppxLocation").text = str(external)
                entry.find("Name").text = "Microsoft.VCLibs"
        (self.host / "ignored.pdb").write_bytes(b"not runtime")
        self.stage()
        for name in PAYLOAD.REQUIRED:
            self.assertEqual((self.host / name[4:]).read_bytes(), (self.output / name).read_bytes())
        self.assertEqual(source.read_bytes(), (self.output / "app/ucrtbased.dll").read_bytes())
        self.assertEqual(self.external.read_bytes(), (self.output / "app/AgentIcons/Agent.svg").read_bytes())
        self.assertFalse((self.output / "app/ignored.pdb").exists())
        self.assertNotIn(str(self.workspace), (self.output / "TestHostApp.build.appxrecipe").read_text())
        # Actions' same-run artifact identity is authoritative, not copied metadata.
        self.assertFalse((self.output / "payload.json").exists())

    def test_sdk_allowance_rejects_arbitrary_machine_files_and_wrong_architecture(self):
        source = self.sdk_runtime()
        for path, target in ((source, "arbitrary.dll"), (source.with_name("other.dll"), "ucrtbased.dll"),
                             (self.root / "other/ucrtbased.dll", "ucrtbased.dll")):
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(source.read_bytes())
            with self.subTest(path=path, target=target), self.assertRaises(ValueError):
                PAYLOAD.recipe_source(str(path), target, self.workspace, self.environment)
        source.write_bytes(b"not an x64 PE")
        with self.assertRaisesRegex(ValueError, "x64 PE"):
            PAYLOAD.recipe_source(str(source), "ucrtbased.dll", self.workspace, self.environment)
        with self.assertRaises(ValueError):
            PAYLOAD.framework_source(str(self.external), self.workspace, self.environment)

    def test_missing_after_build_runtime_or_framework_is_not_published(self):
        (self.host / "taef.png").unlink()
        with self.assertRaisesRegex(ValueError, "Incomplete"):
            self.stage()
        self.assertFalse(self.output.exists())
        (self.host / "taef.png").write_bytes(b"runtime")
        self.items.remove(self.items.findall("ResolvedSDKReference")[0])
        with self.assertRaisesRegex(ValueError, "Incomplete"):
            self.stage()
        self.assertFalse(self.output.exists())

    def test_cross_workspace_and_conflicting_recipe_sources_are_not_published(self):
        source = self.root / "old-tree/TerminalApp.dll"
        source.parent.mkdir()
        source.write_bytes(b"stale")
        item = next(i for i in self.items.findall("AppxPackagedFile") if i.findtext("PackagePath") == "TerminalApp.dll")
        item.set("Include", str(source))
        with self.assertRaisesRegex(ValueError, "outside the active workspace"):
            self.stage()
        source = self.workspace / "bin/x64/Debug/TerminalApp/TerminalApp.dll"
        source.parent.mkdir()
        source.write_bytes(b"different stale loose copy")
        item.set("Include", str(source))
        with self.assertRaisesRegex(ValueError, "Stale"):
            self.stage()
        self.assertFalse(self.output.exists())


class NativeResultTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(dir=SCRATCH)
        self.addCleanup(self.temporary.cleanup)
        self.output = Path(self.temporary.name) / "native"
        for surface, marker in PAYLOAD.SURFACES.items():
            path = self.output / surface
            path.mkdir(parents=True)
            scan_path = f"C:\\a\\Generated Files\\accessibility-91-2\\native\\{surface}\\axe-results.json"
            for log in ("axe.stdout.log", "axe.stderr.log"):
                (path / log).write_text("", encoding="utf-8")
            for name, data in {
                "result": {"version": 1, "source_sha": IDENTITY["EXPECTED_HEAD_SHA"], "surface": surface, "process_id": 45,
                           "status": "PASS", "axe_results": scan_path},
                "axe-results": {"version": 1, "scan_id": surface, "process_id": 45, "window_count": 1, "error_count": 0,
                                "errors": [], "visible_state_marker": marker,
                                "uia_tree": scan_path.replace("axe-results", "uia-tree")},
                "uia-tree": ([{"process_id": 45, "automation_id": marker, "is_offscreen": False}] if surface != "sidebar" else [
                    {"process_id": 45, "automation_id": marker, "is_offscreen": False,
                     "runtime_id": [1], "parent_runtime_id": [], "control_type": "ControlType.Tab", "class_name": "TabStrip"},
                    {"process_id": 45, "automation_id": "ItemsList", "is_offscreen": False,
                     "runtime_id": [2], "parent_runtime_id": [1], "control_type": "ControlType.Tree", "class_name": "ListView"},
                    {"process_id": 45, "automation_id": "", "is_offscreen": False,
                     "runtime_id": [3], "parent_runtime_id": [2], "control_type": "ControlType.TreeItem",
                     "class_name": "Microsoft.UI.Xaml.Controls.TreeViewItem", "name": "PowerShell"},
                ]),
            }.items():
                (path / f"{name}.json").write_text(json.dumps(data), encoding="utf-8")

    def test_complete_real_scan_schema_produces_readable_summary_without_inventory(self):
        receipt = PAYLOAD.record_native(self.output, IDENTITY)
        self.assertEqual("PASS", receipt["status"])
        self.assertEqual(IDENTITY, receipt["identity"])
        self.assertEqual({"fre", "agents", "fre-settings", "sidebar"}, set(receipt["surfaces"]))
        self.assertNotIn("files", receipt)
        summary = (self.output / "native-summary.md").read_text()
        self.assertIn(IDENTITY["EXPECTED_HEAD_SHA"], summary)
        self.assertIn("| sidebar | PASS | 0 |", summary)

    def test_missing_results_logs_and_stale_identity_cannot_pass(self):
        for surface in PAYLOAD.SURFACES:
            for filename in ("result.json", "axe-results.json", "uia-tree.json", "axe.stdout.log", "axe.stderr.log"):
                path = self.output / surface / filename
                original = path.read_bytes()
                path.unlink()
                with self.subTest(surface=surface, missing=filename):
                    self.assertEqual("INCOMPLETE", PAYLOAD.record_native(self.output, IDENTITY)["status"])
                path.write_bytes(original)
        path = self.output / "fre/result.json"
        original = json.loads(path.read_text())
        for change in ({"source_sha": "b" * 40}, {"surface": "agents"}, {"status": "BLOCKED"}, {"process_id": 46}):
            path.write_text(json.dumps({**original, **change}))
            with self.subTest(change=change):
                self.assertEqual("INCOMPLETE", PAYLOAD.record_native(self.output, IDENTITY)["status"])

    def test_actual_native_errors_remain_fail(self):
        for filename, change in (("result", {"status": "FAIL"}),
                                 ("axe-results", {"error_count": 1, "errors": [{"rule_id": "real-error"}]})):
            path = self.output / "sidebar" / f"{filename}.json"
            path.write_text(json.dumps({**json.loads(path.read_text()), **change}))
        receipt = PAYLOAD.record_native(self.output, IDENTITY)
        self.assertEqual("FAIL", receipt["status"])
        self.assertEqual(1, receipt["surfaces"]["sidebar"]["error_count"])
        self.assertIn("| sidebar | FAIL | 1 |", (self.output / "native-summary.md").read_text())

    def test_command_exit_status_keeps_failures_and_missing_results_red(self):
        path = self.output / "fre/result.json"
        original = json.loads(path.read_text())
        for state in ("PASS", "FAIL", "missing"):
            path.write_text(json.dumps({**original, "status": state}), encoding="utf-8")
            scan = self.output / "fre/axe-results.json"
            data = json.loads(scan.read_text())
            data.update(error_count=1 if state == "FAIL" else 0,
                        errors=[{"rule_id": "real-error"}] if state == "FAIL" else [])
            scan.write_text(json.dumps(data), encoding="utf-8")
            if state == "missing":
                path.unlink()
            result = subprocess.run([sys.executable, SPEC.origin, "record-native", "--output", str(self.output)],
                                    env={**os.environ, **IDENTITY}, capture_output=True, text=True, timeout=30)
            with self.subTest(state=state):
                self.assertEqual(0 if state == "PASS" else 1, result.returncode, result.stderr)
                expected = "INCOMPLETE" if state == "missing" else state
                self.assertEqual(expected, json.loads((self.output / "native-ci.json").read_text())["status"])
                self.assertIn(f"**{expected}**", (self.output / "native-summary.md").read_text())

    def test_sidebar_requires_production_tree_and_live_tab_not_promo_or_history(self):
        path = self.output / "sidebar/uia-tree.json"
        tree = json.loads(path.read_text())
        for role in ("ControlType.Tab", "ControlType.Group"):
            tree[0]["control_type"] = role
            self.assertTrue(PAYLOAD.sidebar_state(tree, 45))
            cases = [(0, "automation_id", "welcomeSidebarImage"), (0, "control_type", "ControlType.Image"),
                     (0, "class_name", "Image"), (0, "process_id", 46), (0, "is_offscreen", True),
                     (1, "parent_runtime_id", [99]), (1, "parent_runtime_id", [2]),
                     (1, "control_type", "ControlType.List"), (1, "class_name", "ItemsControl"),
                     (1, "process_id", 46), (2, "parent_runtime_id", [1]), (2, "is_offscreen", True),
                     (2, "process_id", 46), (2, "control_type", "ControlType.Image"),
                     (2, "name", "Recent agent sessions"), (2, "automation_id", "HistoryHeaderButton")]
            for index, key, value in cases:
                candidate = json.loads(json.dumps(tree))
                candidate[index][key] = value
                path.write_text(json.dumps(candidate))
                with self.subTest(role=role, key=key, value=value):
                    self.assertEqual("INCOMPLETE", PAYLOAD.record_native(self.output, IDENTITY)["status"])
            for candidate in ([], tree[1:], tree[:2]):
                path.write_text(json.dumps(candidate))
                self.assertEqual("INCOMPLETE", PAYLOAD.record_native(self.output, IDENTITY)["status"])


if __name__ == "__main__":
    unittest.main()
