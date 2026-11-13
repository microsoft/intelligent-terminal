"""Transport tests only; fixtures are not usable native builds or signed frameworks."""
import hashlib
import importlib.util
import json
from pathlib import Path
import stat
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET
import zipfile

SPEC = importlib.util.spec_from_file_location("payload", Path(__file__).with_name("ci-accessibility-payload.py"))
PAYLOAD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PAYLOAD)
IDENTITY = {"GITHUB_REPOSITORY": "microsoft/intelligent-terminal", "GITHUB_REPOSITORY_ID": "42",
            "EXPECTED_HEAD_SHA": "a" * 40, "CI_RUN_ID": "91", "CI_RUN_ATTEMPT": "2", "PR_NUMBER": "17"}


class PayloadTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
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
            # Test that manual AfterBuild runtime content survives recipe omissions.
            if name in {"app/taef.png", "app/TerminalApp.LocalTests.dll", "app/Wex.Common.dll"}:
                continue
            item = ET.SubElement(self.items, "AppXManifest" if name.endswith("/AppxManifest.xml") else "AppxPackagedFile",
                                 Include=str(file))
            ET.SubElement(item, "PackagePath").text = name[4:].replace("/", "\\")
        (self.host / "AppxManifest.xml").write_text(
            '<Package><Identity Name="WindowsTerminal.TestHost" Publisher="CN=Windows Terminal Team" '
            'ProcessorArchitecture="x64" Version="1.0.0.0" /><Applications>'
            '<Application Id="taef.executionengine.universal.App" Executable="TestHostApp.exe" />'
            '</Applications><Dependencies>' +
            "".join(f'<PackageDependency Name="{n}" Publisher="{PAYLOAD.PUBLISHER}" MinVersion="1.0.0.0" />'
                    for n in sorted(PAYLOAD.FRAMEWORKS)) + '</Dependencies></Package>', encoding="utf-8")
        self.external = self.workspace / "src/resource/Agent.svg"
        self.external.parent.mkdir(parents=True)
        self.external.write_bytes(b"resource outside the loose layout")
        item = ET.SubElement(self.items, "AppxPackagedFile", Include=str(self.external))
        ET.SubElement(item, "PackagePath").text = "AgentIcons/Agent.svg"
        for name in sorted(PAYLOAD.FRAMEWORKS):
            file = self.workspace / f"packages/{name}.appx"
            file.parent.mkdir(parents=True, exist_ok=True)
            with zipfile.ZipFile(file, "w") as archive:
                archive.writestr("AppxManifest.xml", f'<Package><Identity Name="{name}" '
                                 f'Publisher="{PAYLOAD.PUBLISHER}" ProcessorArchitecture="x64" Version="1.0.0.0" />'
                                 '<Properties><Framework>true</Framework></Properties></Package>')
            item = ET.SubElement(self.items, "ResolvedSDKReference", Include=str(file))
            for key, value in {"Name": name, "Architecture": "x64", "AppxLocation": str(file)}.items():
                ET.SubElement(item, key).text = value
        self.output = self.root / "staging"
        self.archive = self.root / "artifact.zip"
        self.destination = self.root / "consumer"

    def stage(self):
        ET.ElementTree(self.recipe).write(self.host / "TestHostApp.build.appxrecipe")
        PAYLOAD.stage(self.workspace, self.output, self.environment)

    def zip(self, changes=None, extra=()):
        members = {p.relative_to(self.output).as_posix(): p.read_bytes() for p in self.output.rglob("*") if p.is_file()}
        if changes:
            changes(members)
        with zipfile.ZipFile(self.archive, "w", zipfile.ZIP_DEFLATED) as archive:
            for name, value in members.items():
                archive.writestr(name, value)
            for name, value in extra:
                archive.writestr(name, value)
        return "sha256:" + hashlib.sha256(self.archive.read_bytes()).hexdigest()

    def reject(self, digest, environment=IDENTITY):
        with self.assertRaises((ValueError, KeyError, zipfile.BadZipFile)):
            PAYLOAD.extract(self.archive, self.destination, digest, environment)
        self.assertFalse(self.destination.exists(), "Invalid archives must fail before writing")

    # Negative controls before the positive roundtrip.
    def test_digest_mismatch_and_absent_digest_rejected(self):
        self.stage()
        self.zip()
        for digest in ("", "sha256:" + "0" * 64):
            with self.subTest(digest=digest):
                self.reject(digest)

    def test_archive_path_traversal_absolute_ads_device_and_case_collisions_rejected(self):
        self.stage()
        for path in ("../escape", "/absolute", "C:/escape", "app/../../escape", "app\\escape",
                     "app/file:stream", "app/NUL.txt", "app/trailing.", "APP/TAEF.PNG"):
            with self.subTest(path=path):
                self.reject(self.zip(extra=[(path, b"unsafe")]))

    def test_symlink_directory_and_prefix_collision_rejected(self):
        self.stage()
        link = zipfile.ZipInfo("app/link")
        link.create_system = 3
        link.external_attr = (stat.S_IFLNK | 0o777) << 16
        for entry in ((link, b"../escape"), ("app/directory/", b""), ("app/taef.png/child", b"conflict")):
            with self.subTest(entry=str(entry[0])):
                self.reject(self.zip(extra=[entry]))

    def test_wrong_repository_source_run_attempt_or_platform_rejected(self):
        self.stage()
        digest = self.zip()
        for key in IDENTITY:
            value = "b" * 40 if key == "EXPECTED_HEAD_SHA" else ("other/repo" if key == "GITHUB_REPOSITORY" else "92")
            with self.subTest(key=key):
                self.reject(digest, {**IDENTITY, key: value})
        def mutate(members):
            data = json.loads(members["payload.json"])
            data["platform"] = "x86"
            members["payload.json"] = json.dumps(data).encode()
        self.reject(self.zip(mutate))

    def test_missing_inventory_entry_required_runtime_and_tampered_bytes_rejected(self):
        self.stage()
        for path in ("payload.json", "app/TerminalApp.LocalTests.dll", "app/resources.pri",
                     "app/taef.png", "frameworks/Microsoft.UI.Xaml.2.8.appx"):
            with self.subTest(path=path):
                self.reject(self.zip(lambda members: members.pop(path)))
        self.reject(self.zip(lambda members: members.__setitem__("app/TestHostApp.exe", b"tampered")))
        self.reject(self.zip(extra=[("extra.dll", b"not in manifest")]))

    def test_archive_size_bound_rejected_before_extracting(self):
        self.stage()
        digest = self.zip()
        with patch.object(PAYLOAD, "MAX_BYTES", 1):
            self.reject(digest)

    def test_portable_recipe_stale_absolute_mapping_rejected_even_with_matching_inventory(self):
        self.stage()
        def mutate(members):
            recipe = ET.fromstring(members["TestHostApp.build.appxrecipe"])
            next(iter(PAYLOAD.children(recipe, "AppXManifest"))).set("Include", "C:/old/AppxManifest.xml")
            value = ET.tostring(recipe)
            members["TestHostApp.build.appxrecipe"] = value
            data = json.loads(members["payload.json"])
            data["files"]["TestHostApp.build.appxrecipe"] = {"size": len(value), "sha256": hashlib.sha256(value).hexdigest()}
            members["payload.json"] = json.dumps(data).encode()
        self.reject(self.zip(mutate))

    def test_missing_loose_runtime_stale_source_or_cross_workspace_recipe_rejected(self):
        for tag in self.recipe.iter("AppxPackagedFile"):
            if tag.findtext("PackagePath") == "TerminalApp.dll":
                tag.set("Include", str(self.root / "old-tree/TerminalApp.dll"))
                old = self.root / "old-tree/TerminalApp.dll"
                old.parent.mkdir()
                old.write_bytes(b"stale")
                break
        with self.assertRaisesRegex(ValueError, "outside the active workspace"):
            self.stage()
        self.assertFalse(self.output.exists())

    def test_missing_afterbuild_asset_or_framework_rejected(self):
        (self.host / "taef.png").unlink()
        with self.assertRaisesRegex(ValueError, "Incomplete"):
            self.stage()
        self.assertFalse(self.output.exists())

    def test_wrong_host_or_insufficient_framework_identity_rejected_before_publication(self):
        manifest = self.host / "AppxManifest.xml"
        original = manifest.read_text(encoding="utf-8")
        for value in (original.replace("WindowsTerminal.TestHost", "Other.Host"),
                      original.replace('ProcessorArchitecture="x64"', 'ProcessorArchitecture="x86"'),
                      original.replace('MinVersion="1.0.0.0"', 'MinVersion="2.0.0.0"')):
            with self.subTest(value=value[:80]):
                manifest.write_text(value, encoding="utf-8")
                with self.assertRaises(ValueError):
                    self.stage()
                self.assertFalse(self.output.exists())
        manifest.write_text(original, encoding="utf-8")
        framework = self.workspace / "packages/Microsoft.UI.Xaml.2.8.appx"
        with zipfile.ZipFile(framework) as archive:
            data = archive.read("AppxManifest.xml").replace(b'ProcessorArchitecture="x64"', b'ProcessorArchitecture="x86"')
        with zipfile.ZipFile(framework, "w") as archive:
            archive.writestr("AppxManifest.xml", data)
        with self.assertRaisesRegex(ValueError, "Unexpected framework"):
            self.stage()

    def test_conflicting_recipe_and_loose_dll_rejected(self):
        source = self.workspace / "bin/x64/Debug/TerminalApp/TerminalApp.dll"
        source.parent.mkdir()
        source.write_bytes(b"different stale loose copy")
        for item in self.recipe.iter("AppxPackagedFile"):
            if item.findtext("PackagePath") == "TerminalApp.dll":
                item.set("Include", str(source))
        with self.assertRaisesRegex(ValueError, "Stale"):
            self.stage()
        self.assertFalse(self.output.exists())

    def test_complete_portable_roundtrip_preserves_recipe_resources_dlls_winmd_and_afterbuild_assets(self):
        (self.host / "ignored.pdb").write_bytes(b"not runtime")
        self.stage()
        self.assertNotIn(str(self.workspace), (self.output / "TestHostApp.build.appxrecipe").read_text())
        digest = self.zip()
        PAYLOAD.extract(self.archive, self.destination, digest, IDENTITY)
        for name in PAYLOAD.REQUIRED:
            self.assertEqual((self.host / name[4:]).read_bytes(), (self.destination / name).read_bytes())
        self.assertEqual(self.external.read_bytes(), (self.destination / "app/AgentIcons/Agent.svg").read_bytes())
        self.assertFalse((self.destination / "app/ignored.pdb").exists())
        self.assertEqual(IDENTITY, json.loads((self.destination / "payload.json").read_text())["identity"])

    def sdk_runtime(self):
        source = self.sdk / "Microsoft.UniversalCRT.Debug/10.0.26100.0/Redist/Debug/x64/ucrtbased.dll"
        source.parent.mkdir(parents=True, exist_ok=True)
        source.write_bytes(b"MZ" + bytes(58) + (64).to_bytes(4, "little") + b"PE\x00\x00\x64\x86")
        return source

    def test_actual_msbuild_sdk_recipe_mapping_is_portable(self):
        # Emitted by MSBuild on Windows: namespace, %28/%29 escaping, doubled
        # separator and SDK-owned CRT mapping. Hosted43214343855 logs confirm
        # the same SDK version/file. This is a transport fixture, not a build.
        source = self.sdk_runtime()
        fragment = ET.fromstring(Path(__file__).with_name("testhost-sdk-recipe.xml").read_bytes())
        entries = PAYLOAD.children(fragment, "AppxPackagedFile")
        self.assertEqual(1, len(entries))
        item = entries[0]
        item.set("Include", str(source).replace("(x86)", "%28x86%29"))
        self.items.append(item)
        # Actual recipe resolves the x64 Debug VCLibs appx outside the workspace.
        package = self.workspace / "packages/Microsoft.VCLibs.140.00.Debug.appx"
        external = self.sdk / "Microsoft.VCLibs/14.0/AppX/Debug/x64/Microsoft.VCLibs.x64.Debug.14.00.appx"
        external.parent.mkdir(parents=True)
        external.write_bytes(package.read_bytes())
        for entry in self.items.findall("ResolvedSDKReference"):
            if entry.findtext("Name") == "Microsoft.VCLibs.140.00.Debug":
                entry.find("AppxLocation").text = str(external)
        self.stage()
        PAYLOAD.extract(self.archive, self.destination, self.zip(), IDENTITY)
        self.assertEqual(source.read_bytes(), (self.destination / "app/ucrtbased.dll").read_bytes())

    def test_sdk_allowance_does_not_allow_arbitrary_machine_files_or_wrong_architecture(self):
        source = self.sdk_runtime()
        for path, target in ((source, "arbitrary.dll"),
                             (source.with_name("other.dll"), "ucrtbased.dll"),
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

    def test_all_ordinary_units_and_loose_resources_survive_without_testhost_or_symbols(self):
        root = self.workspace / "bin/x64/Debug"
        for name in PAYLOAD.UNIT_DLLS | {"te.exe", "Wex.Common.dll", "resources.pri", "ignored.pdb", "ignored.lib"}:
            path = root / "units" / name
            path.parent.mkdir(exist_ok=True)
            path.write_bytes(name.encode())
        PAYLOAD.stage_units(self.workspace, self.output, IDENTITY)
        PAYLOAD.check_units(self.output, IDENTITY)
        self.assertFalse((self.output / "TestHostApp").exists())
        self.assertFalse((self.output / "units/ignored.pdb").exists())
        self.assertTrue((self.output / "units/resources.pri").exists())
        for identity in ({**IDENTITY, "CI_RUN_ATTEMPT": "3"}, {**IDENTITY, "EXPECTED_HEAD_SHA": "b" * 40}):
            with self.assertRaisesRegex(ValueError, "provenance"):
                PAYLOAD.check_units(self.output, identity)
        (self.output / "units/te.exe").write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "differs"):
            PAYLOAD.check_units(self.output, IDENTITY)


class NativeReceiptTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.output = self.root / "native"
        self.archive = self.root / "native.zip"
        self.destination = self.root / "consumed"
        for surface, marker in PAYLOAD.SURFACES.items():
            path = self.output / surface
            path.mkdir(parents=True)
            scanpath = f"C:\\a\\Generated Files\\accessibility-91-2\\native\\{surface}\\axe-results.json"
            for name, data in {
                "result": {"version": 1, "source_sha": IDENTITY["EXPECTED_HEAD_SHA"], "surface": surface, "process_id": 45,
                           "status": "PASS", "axe_results": scanpath},
                "axe-results": {"version": 1, "scan_id": surface, "process_id": 45, "window_count": 1, "error_count": 0,
                                "errors": [], "visible_state_marker": marker,
                                "uia_tree": scanpath.replace("axe-results", "uia-tree")},
                "uia-tree": [{"process_id": 45, "automation_id": marker, "is_offscreen": False}],
            }.items():
                (path / f"{name}.json").write_text(json.dumps(data), encoding="utf-8")

    def consume(self):
        PAYLOAD.record_native(self.output, IDENTITY)
        with zipfile.ZipFile(self.archive, "w") as archive:
            for path in self.output.rglob("*"):
                if path.is_file():
                    archive.writestr(path.relative_to(self.output).as_posix(), path.read_bytes())
        PAYLOAD.extract(self.archive, self.destination, "sha256:" + PAYLOAD.digest(self.archive), IDENTITY, native=True)

    def test_real_scan_schema_passes_with_original_identity_and_surface_errors(self):
        self.consume()
        receipt = json.loads((self.destination / "native-ci.json").read_text())
        self.assertEqual("PASS", receipt["status"])
        self.assertEqual(IDENTITY, receipt["identity"])
        self.assertEqual(set(PAYLOAD.SURFACES), set(receipt["surfaces"]))
        self.assertEqual(0, receipt["surfaces"]["fre"]["error_count"])

    def test_missing_skipped_failed_stale_hidden_empty_and_cleanup_blocked_are_not_pass(self):
        path = self.output / "fre/result.json"
        result = json.loads(path.read_text())
        cases = [
            ("result.json", None), ("result.json", {**result, "status": "BLOCKED"}),
            ("result.json", {**result, "status": "FAIL"}),
            ("result.json", {**result, "source_sha": "b" * 40}),
            ("uia-tree.json", []),
            ("uia-tree.json", [{"automation_id": "NextButton", "process_id": 45, "is_offscreen": True}]),
        ]
        for name, data in cases:
            target = self.output / "fre" / name
            original = target.read_bytes()
            if data is None:
                target.unlink()
            else:
                target.write_text(json.dumps(data))
            with self.subTest(name=name, data=data), self.assertRaisesRegex(ValueError, "incomplete or failed"):
                self.consume()
            self.assertFalse(self.destination.exists())
            target.write_bytes(original)

    def test_actual_axe_errors_are_fail_not_missing_or_pass(self):
        path = self.output / "fre/axe-results.json"
        scan = json.loads(path.read_text())
        scan.update(error_count=1, errors=[{"rule_id": "10001", "description": "actual Axe finding"}])
        path.write_text(json.dumps(scan))
        path = self.output / "fre/result.json"
        result = json.loads(path.read_text())
        result["status"] = "FAIL"
        path.write_text(json.dumps(result))
        with self.assertRaisesRegex(ValueError, "incomplete or failed"):
            self.consume()
        receipt = json.loads((self.output / "native-ci.json").read_text())
        self.assertEqual("FAIL", receipt["status"])
        self.assertEqual(1, receipt["surfaces"]["fre"]["error_count"])

    def test_native_transport_rejects_wrong_digest_provenance_tampering_and_traversal(self):
        PAYLOAD.record_native(self.output, IDENTITY)
        originals = {p.relative_to(self.output).as_posix(): p.read_bytes()
                     for p in self.output.rglob("*") if p.is_file()}
        for mode in ("digest", "identity", "tampered", "traversal", "collision", "missing"):
            members = dict(originals)
            environment = IDENTITY
            if mode == "identity":
                environment = {**IDENTITY, "CI_RUN_ATTEMPT": "3"}
            elif mode == "tampered":
                members["fre/result.json"] = b"{}"
            elif mode == "traversal":
                members["../escape"] = b"not allowed"
            elif mode == "collision":
                members["FRE/RESULT.JSON"] = members["fre/result.json"]
            elif mode == "missing":
                members.pop("fre/result.json")
            with zipfile.ZipFile(self.archive, "w") as archive:
                for name, value in members.items():
                    archive.writestr(name, value)
            digest = "sha256:" + ("0" * 64 if mode == "digest" else PAYLOAD.digest(self.archive))
            with self.subTest(mode=mode), self.assertRaises(ValueError):
                PAYLOAD.extract(self.archive, self.destination, digest, environment, native=True)
            self.assertFalse(self.destination.exists())

    def test_forged_pass_receipt_cannot_override_actual_missing_or_failed_scan(self):
        receipt = PAYLOAD.record_native(self.output, IDENTITY)
        (self.output / "fre/uia-tree.json").write_text("[]")
        # Candidate can change inventory as well; the trusted consumer must derive
        # the native outcome again, not merely trust a signed-looking PASS field.
        path = self.output / "fre/uia-tree.json"
        receipt["files"]["fre/uia-tree.json"] = {"size": path.stat().st_size, "sha256": PAYLOAD.digest(path)}
        (self.output / "native-ci.json").write_text(json.dumps(receipt))
        with zipfile.ZipFile(self.archive, "w") as archive:
            for path in self.output.rglob("*"):
                if path.is_file():
                    archive.writestr(path.relative_to(self.output).as_posix(), path.read_bytes())
        with self.assertRaisesRegex(ValueError, "incomplete or failed"):
            PAYLOAD.extract(self.archive, self.destination, "sha256:" + PAYLOAD.digest(self.archive), IDENTITY, native=True)
        self.assertFalse(self.destination.exists())


if __name__ == "__main__":
    unittest.main()
