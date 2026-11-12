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
        PAYLOAD.stage(self.workspace, self.output, IDENTITY)

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


if __name__ == "__main__":
    unittest.main()
