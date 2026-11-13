"""Portable TestHost layout transport. Does not build, deploy or execute payloads."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath, PureWindowsPath
import re
import shutil
import stat
from urllib.parse import unquote
import xml.etree.ElementTree as ET
import zipfile

MAX_BYTES = 2 * 1024**3
MAX_FILES = 10000
FRAMEWORKS = {"Microsoft.UI.Xaml.2.8", "Microsoft.VCLibs.140.00.Debug"}
PUBLISHER = "CN=Microsoft Corporation, O=Microsoft Corporation, L=Redmond, S=Washington, C=US"
REQUIRED = {
    "app/AppxManifest.xml", "app/TestHostApp.exe", "app/TestHostApp.winmd",
    "app/TerminalApp.dll", "app/TerminalApp.winmd", "app/TerminalApp.LocalTests.dll",
    "app/resources.pri", "app/UnitTestApp.xbf", "app/taef.png", "app/TerminalApp/App.xaml",
    "app/Microsoft.Terminal.Control.dll", "app/Microsoft.Terminal.Control.winmd",
    "app/Microsoft.Terminal.Settings.Editor.dll", "app/Microsoft.Terminal.Settings.Editor.winmd",
    "app/Microsoft.Terminal.Settings.Model.dll", "app/Microsoft.Terminal.Settings.Model.winmd",
    "app/Microsoft.Terminal.UI.dll", "app/Microsoft.Terminal.UI.winmd",
    "app/Microsoft.Terminal.UI.Markdown.dll", "app/Microsoft.Terminal.UI.Markdown.winmd",
    "app/TerminalConnection.dll", "app/Microsoft.Terminal.TerminalConnection.winmd",
    "app/TerminalThemeHelpers.dll", "app/TE.AppxUnitTestClient.dll",
    "app/Microsoft.VisualStudio.TestPlatform.TestExecutor.WinRTCore.winmd",
    "app/Wex.Common.dll", "app/Wex.Logger.dll", "app/Wex.Communication.dll",
    "app/TE.Common.dll", "app/Conduit.Broker.dll",
}
UNIT_DLLS = {
    "Conhost.Unit.Tests.dll", "Conhost.Interactivity.Win32.Unit.Tests.dll",
    "til.unit.tests.dll", "Types.Unit.Tests.dll", "TextBuffer.Unit.Tests.dll",
    "ConAdapter.Unit.Tests.dll", "ConParser.Unit.Tests.dll", "Control.Unit.Tests.dll",
    "SettingsModel.Unit.Tests.dll", "Terminal.Core.Unit.Tests.dll", "Terminal.App.Unit.Tests.dll",
}
SURFACES = {"fre": "NextButton", "agents": "AcpAgentComboBox", "fre-settings": "AgentComboBox",
            "sidebar": "FixtureSidebar"}


def sidebar_state(tree, process):
    """Require the production TreeView list and a visible row under the sidebar marker."""
    parents = {tuple(n.get("runtime_id", [])): n for n in tree if n.get("runtime_id")}

    def visible(node):
        return node.get("process_id") == process and node.get("is_offscreen") is False

    def beneath(node, ancestor):
        seen = set()
        parent = tuple(node.get("parent_runtime_id", []))
        target = tuple(ancestor.get("runtime_id", []))
        while parent and parent not in seen:
            if parent == target:
                return True
            seen.add(parent)
            parent = tuple(parents.get(parent, {}).get("parent_runtime_id", []))
        return False

    markers = [n for n in tree if visible(n) and n.get("automation_id") == SURFACES["sidebar"] and
               n.get("control_type") == "ControlType.Tab" and n.get("class_name") == "TabStrip"]
    lists = [n for n in tree if visible(n) and n.get("automation_id") == "ItemsList"]
    # HistoryHeaderButton is a TreeItem too. Require the deterministic live tab,
    # not merely any row in the fixture's production TreeView.
    rows = [n for n in tree if visible(n) and n.get("control_type") == "ControlType.TreeItem" and
            n.get("class_name") == "Microsoft.UI.Xaml.Controls.TreeViewItem" and
            n.get("name") == "PowerShell" and n.get("automation_id") != "HistoryHeaderButton"]
    return any(beneath(items, marker) and any(beneath(row, items) for row in rows)
               for marker in markers for items in lists)


def safe_path(value):
    if not isinstance(value, str) or not value or "\\" in value or ":" in value or "\x00" in value:
        raise ValueError("Unsafe payload path")
    parts = value.split("/")
    if any(not p or p in (".", "..") or p[-1] in ". " or
           re.match(r"^(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(\.|$)", p, re.I) or
           any(ord(c) < 32 or c in '<>"|?*' for c in p) for p in parts):
        raise ValueError("Unsafe payload path")
    return str(PurePosixPath(value))


def regular(path):
    path = Path(path)
    for parent in (path, *path.parents):
        info = parent.lstat()
        if stat.S_ISLNK(info.st_mode) or getattr(info, "st_file_attributes", 0) & 0x400:
            raise ValueError("Payload source contains a link or reparse point")
    if not path.is_file():
        raise ValueError("Missing payload file")
    return path


def digest(path):
    with open(path, "rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def identity(environment, require_pr=True):
    result = {key: environment.get(key) for key in
              ("GITHUB_REPOSITORY", "GITHUB_REPOSITORY_ID", "EXPECTED_HEAD_SHA", "CI_RUN_ID", "CI_RUN_ATTEMPT", "PR_NUMBER")}
    if (not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", result["GITHUB_REPOSITORY"] or "") or
        not re.fullmatch(r"[0-9a-f]{40}", result["EXPECTED_HEAD_SHA"] or "") or
        any(not re.fullmatch(r"[1-9][0-9]*", result[k] or "") for k in
            ("GITHUB_REPOSITORY_ID", "CI_RUN_ID", "CI_RUN_ATTEMPT")) or
        not re.fullmatch(r"[1-9][0-9]*" if require_pr else r"(0|[1-9][0-9]*)", result["PR_NUMBER"] or "")):
        raise ValueError("Invalid payload provenance")
    return result


def xml(data):
    if b"<!DOCTYPE" in data.upper() or b"<!ENTITY" in data.upper():
        raise ValueError("XML declarations are not allowed")
    return ET.fromstring(data)


def children(root, tag):
    return [element for element in root.iter() if element.tag.rsplit("}", 1)[-1] == tag]


def field(element, tag):
    matches = children(element, tag)
    if len(matches) != 1 or not matches[0].text:
        raise ValueError("Missing/ambiguous recipe field")
    return matches[0].text


def framework_identity(data):
    with zipfile.ZipFile(data) as archive:
        if archive.getinfo("AppxManifest.xml").file_size > 1024**2:
            raise ValueError("Oversized framework manifest")
        manifest = xml(archive.read("AppxManifest.xml"))
    items = children(manifest, "Identity")
    if len(items) != 1:
        raise ValueError("Invalid framework identity")
    item = items[0].attrib
    if (item.get("Name") not in FRAMEWORKS or item.get("Publisher") != PUBLISHER or
            item.get("ProcessorArchitecture") != "x64" or
            len(children(manifest, "Framework")) != 1 or field(manifest, "Framework").lower() != "true"):
        raise ValueError("Unexpected framework package")
    return item


def version(value):
    if not re.fullmatch(r"[0-9]+(?:\.[0-9]+){3}", value or ""):
        raise ValueError("Invalid package version")
    return tuple(int(part) for part in value.split("."))


def validate_host(manifest_bytes, frameworks):
    manifest = xml(manifest_bytes)
    identities = children(manifest, "Identity")
    applications = children(manifest, "Application")
    if (len(identities) != 1 or identities[0].get("Name") != "WindowsTerminal.TestHost" or
            identities[0].get("Publisher") != "CN=Windows Terminal Team" or
            identities[0].get("ProcessorArchitecture") != "x64" or
            len(applications) != 1 or applications[0].get("Executable") != "TestHostApp.exe" or
            applications[0].get("Id") != "taef.executionengine.universal.App"):
        raise ValueError("Unexpected TestHost application identity")
    dependencies = children(manifest, "PackageDependency")
    if len(dependencies) != len(FRAMEWORKS) or {d.get("Name") for d in dependencies} != FRAMEWORKS:
        raise ValueError("Incomplete TestHost framework dependencies")
    for dependency in dependencies:
        package = frameworks[dependency.get("Name")]
        if dependency.get("Publisher") != PUBLISHER or version(package.get("Version")) < version(dependency.get("MinVersion")):
            raise ValueError("Framework does not satisfy the TestHost manifest")


def sdk_root(environment):
    # Not a recipe-selected root. Only the installed Microsoft extension SDKs.
    root = environment.get("ProgramFiles(x86)") or environment.get("PROGRAMFILES(X86)")
    if not root:
        raise ValueError("Missing installed SDK root")
    return Path(root) / "Microsoft SDKs/Windows Kits/10/ExtensionSDKs"


def recipe_source(source, target, workspace, environment):
    source = regular(Path(unquote(source))).resolve(strict=True)
    if any(source.is_relative_to(workspace / folder) for folder in ("bin/x64/Debug", "src", "packages")):
        return source
    # Actual MSBuild recipes include this loose SDK runtime, not a framework appx.
    # Do not permit a general SDK directory, arbitrary recipe paths or machine DLLs.
    root = sdk_root(environment).resolve()
    relative = source.relative_to(root).as_posix() if source.is_relative_to(root) else ""
    if (target == "ucrtbased.dll" and re.fullmatch(
            r"Microsoft\.UniversalCRT\.Debug/[0-9]+(?:\.[0-9]+){3}/redist/Debug/x64/ucrtbased\.dll", relative, re.I)):
        with source.open("rb") as stream:
            header = stream.read(64)
            if len(header) == 64 and header[:2] == b"MZ":
                stream.seek(int.from_bytes(header[60:64], "little"))
                if stream.read(6) == b"PE\x00\x00\x64\x86":
                    return source
        raise ValueError(f"SDK runtime is not an x64 PE: {source}")
    raise ValueError(f"Recipe source is outside the active workspace: {source} -> {target}")


def framework_source(source, workspace, environment):
    source = regular(Path(unquote(source))).resolve(strict=True)
    if source.is_relative_to(workspace / "packages"):
        return source
    expected = sdk_root(environment) / "Microsoft.VCLibs/14.0/AppX/Debug/x64/Microsoft.VCLibs.x64.Debug.14.00.appx"
    if source != expected.resolve():
        raise ValueError(f"Framework source is outside the approved package locations: {source}")
    return source


def stage(workspace, output, environment):
    """Gather the actual recipe and loose AfterBuild runtime content, never PDBs."""
    workspace = Path(workspace).resolve(strict=True)
    host = workspace / "bin/x64/Debug/TestHostApp"
    recipe = xml(regular(host / "TestHostApp.build.appxrecipe").read_bytes())
    if field(recipe, "PackageArchitecture") != "x64":
        raise ValueError("TestHost architecture differs")
    files = {}

    def add(name, source):
        name = safe_path(name)
        source = regular(source)
        prior = next((p for p in files if p.casefold() == name.casefold()), None)
        if prior:
            if digest(files[prior]) != digest(source):
                raise ValueError("Stale or colliding recipe/loose payload")
            return  # Windows recipe/output casing can differ; retain recipe spelling.
        files[name] = source

    for tag in ("AppXManifest", "AppxPackagedFile"):
        for item in children(recipe, tag):
            target = field(item, "PackagePath").replace("\\", "/")
            add("app/" + target, recipe_source(item.attrib["Include"], target, workspace, environment))
    for source in host.rglob("*"):
        if source.is_dir():
            if source.is_symlink() or getattr(source.lstat(), "st_file_attributes", 0) & 0x400:
                raise ValueError("Linked payload directory")
            continue
        if source.suffix.lower() not in (".pdb", ".lib", ".exp", ".appxrecipe"):
            add("app/" + source.relative_to(host).as_posix(), source)
    frameworks = {}
    framework_ids = {}
    for item in children(recipe, "ResolvedSDKReference"):
        if field(item, "Architecture").lower() != "x64":
            continue
        source = framework_source(field(item, "AppxLocation"), workspace, environment)
        package = framework_identity(source)
        name = package["Name"]
        if name in frameworks:
            raise ValueError("Ambiguous framework recipe")
        frameworks[name] = f"frameworks/{name}.appx"
        framework_ids[name] = package
        add(frameworks[name], source)
    if set(frameworks) != FRAMEWORKS or not REQUIRED <= set(files):
        raise ValueError("Incomplete TestHost runtime payload")
    validate_host(files["app/AppxManifest.xml"].read_bytes(), framework_ids)
    output = Path(output)
    output.mkdir(parents=True, exist_ok=False)
    for name, source in files.items():
        target = output / name
        target.parent.mkdir(parents=True, exist_ok=True)
        # Exclusive destination; all input links and collisions were rejected.
        with target.open("xb") as stream, source.open("rb") as original:
            shutil.copyfileobj(original, stream)
    # Reconstruct a portable deployment recipe; no build-machine absolute paths.
    portable = ET.Element("Project")
    props = ET.SubElement(portable, "PropertyGroup")
    ET.SubElement(props, "PackageArchitecture").text = "x64"
    group = ET.SubElement(portable, "ItemGroup")
    for name in sorted(files):
        if name.startswith("app/"):
            item = ET.SubElement(group, "AppXManifest" if name == "app/AppxManifest.xml" else "AppxPackagedFile",
                                 Include=name)
            ET.SubElement(item, "PackagePath").text = name[4:]
    for name, path in sorted(frameworks.items()):
        item = ET.SubElement(group, "ResolvedSDKReference", Include=path)
        ET.SubElement(item, "Architecture").text = "x64"
        ET.SubElement(item, "Name").text = name
        ET.SubElement(item, "AppxLocation").text = path
    ET.ElementTree(portable).write(output / "TestHostApp.build.appxrecipe", encoding="utf-8", xml_declaration=True)
    paths = sorted(p for p in output.rglob("*") if p.is_file())
    inventory = {p.relative_to(output).as_posix(): {"size": p.stat().st_size, "sha256": digest(p)} for p in paths}
    if len(inventory) > MAX_FILES or sum(p["size"] for p in inventory.values()) > MAX_BYTES:
        raise ValueError("Payload exceeds transport limits")
    (output / "payload.json").write_text(json.dumps({
        "version": 1, "identity": identity(environment), "configuration": "Debug", "platform": "x64",
        "files": inventory,
    }, sort_keys=True), encoding="utf-8")


def validate_unit_layout(paths):
    # Match the existing PowerShell runner's Windows case-insensitive semantics.
    # MSBuild actually emits til.Unit.Tests.dll, not the expectation's lowercase spelling.
    names = {name.casefold() for name in paths}
    if (len(names) != len(paths) or
            not {name.casefold() for name in UNIT_DLLS} <= {PurePosixPath(n).name for n in names}):
        raise ValueError("Incomplete or colliding ordinary C++ unit test payload")
    for name in names:
        if re.fullmatch(r".*unit\.test.*\.dll", PurePosixPath(name).name):
            local = (PurePosixPath(name).parent / "te.exe").as_posix()
            if "te.exe" not in names and local not in names:
                raise ValueError(f"Missing ordinary C++ test runner for {name}")


def stage_units(workspace, output, environment):
    """Transport all loose Debug runtime outputs, not symbols, libraries or TestHost."""
    root = Path(workspace).resolve(strict=True) / "bin/x64/Debug"
    files = {p.relative_to(root).as_posix(): regular(p) for p in root.rglob("*")
             if p.is_file() and p.relative_to(root).parts[0] != "TestHostApp" and
             p.suffix.lower() not in (".pdb", ".lib", ".exp", ".ilk", ".appxrecipe")}
    validate_unit_layout(files)
    output = Path(output)
    output.mkdir(parents=True, exist_ok=False)
    for name, source in files.items():
        target = output / safe_path(name)
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
    inventory = {name: {"size": source.stat().st_size, "sha256": digest(source)} for name, source in files.items()}
    if len(inventory) > MAX_FILES or sum(p["size"] for p in inventory.values()) > MAX_BYTES:
        raise ValueError("Unit payload exceeds transport limits")
    (output / "unit-payload.json").write_text(json.dumps({
        "version": 1, "identity": identity(environment, require_pr=False), "files": inventory,
    }, sort_keys=True), encoding="utf-8")


def check_units(output, environment):
    root = Path(output)
    metadata = json.loads(regular(root / "unit-payload.json").read_bytes())
    inventory = metadata.get("files", {})
    actual = {p.relative_to(root).as_posix() for p in root.rglob("*") if p.is_file()}
    if (metadata.get("version") != 1 or metadata.get("identity") != identity(environment, require_pr=False) or
            actual != set(inventory) | {"unit-payload.json"}):
        raise ValueError("Unit payload inventory or provenance differs")
    validate_unit_layout(inventory)
    for name, record in inventory.items():
        path = regular(root / safe_path(name))
        if path.stat().st_size != record["size"] or digest(path) != record["sha256"]:
            raise ValueError("Unit payload file differs")


def native_summary(read, environment):
    """Derive status from real Axe/UIA output; missing/invalid evidence is incomplete."""
    surfaces = {}
    for surface, marker in SURFACES.items():
        summary = {"status": "INCOMPLETE", "error_count": None, "process_id": None,
                   "source_sha": environment["EXPECTED_HEAD_SHA"]}
        try:
            result, scan, tree = [json.loads(read(f"{surface}/{name}.json"))
                                  for name in ("result", "axe-results", "uia-tree")]
            process = result["process_id"]
            errors = scan["error_count"]
            scan_path = PureWindowsPath(result["axe_results"])
            tree_path = PureWindowsPath(scan["uia_tree"])
            if (result["version"] != 1 or scan["version"] != 1 or
                    result["source_sha"] != environment["EXPECTED_HEAD_SHA"] or result["surface"] != surface or
                    type(process) is not int or process <= 0 or scan["process_id"] != process or
                    scan["scan_id"] != surface or type(scan["window_count"]) is not int or scan["window_count"] <= 0 or
                    scan["visible_state_marker"] != marker or type(errors) is not int or errors < 0 or
                    not isinstance(scan["errors"], list) or len(scan["errors"]) != errors or scan_path.name != "axe-results.json" or
                    scan_path.parent.name != surface or tree_path != scan_path.with_name("uia-tree.json") or
                    not isinstance(tree, list) or not all(isinstance(n, dict) for n in tree) or not any(
                        n.get("automation_id") == marker and n.get("is_offscreen") is False and
                        n.get("process_id") == process for n in tree) or
                    (surface == "sidebar" and not sidebar_state(tree, process))):
                raise ValueError("Native surface identity/state differs")
            summary.update(process_id=process, error_count=errors)
            if result["status"] == "PASS" and errors == 0:
                summary["status"] = "PASS"
            elif result["status"] == "FAIL" and errors > 0:
                summary["status"] = "FAIL"
        except (OSError, KeyError, ValueError, TypeError):
            pass
        surfaces[surface] = summary
    states = {s["status"] for s in surfaces.values()}
    return {"version": 1, "identity": identity(environment),
            "status": "INCOMPLETE" if "INCOMPLETE" in states else ("FAIL" if "FAIL" in states else "PASS"),
            "surfaces": surfaces}


def record_native(output, environment):
    root = Path(output)
    root.mkdir(parents=True, exist_ok=True)
    receipt = native_summary(lambda name: regular(root / name).read_bytes(), environment)
    receipt["files"] = {p.relative_to(root).as_posix(): {"size": regular(p).stat().st_size, "sha256": digest(p)}
                        for p in root.rglob("*") if p.is_file() and p.name != "native-ci.json"}
    (root / "native-ci.json").write_text(json.dumps(receipt, sort_keys=True), encoding="utf-8")
    return receipt


def extract(archive_path, destination, expected_digest, environment, native=False):
    """Authenticate and preflight the entire Actions ZIP before writing any file."""
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", expected_digest or "") or (
            "sha256:" + digest(regular(archive_path))) != expected_digest:
        raise ValueError("Artifact digest differs from the authenticated GitHub identity")
    with zipfile.ZipFile(archive_path) as archive:
        entries = archive.infolist()
        if len(entries) > MAX_FILES + 1 or sum(i.file_size for i in entries) > MAX_BYTES:
            raise ValueError("Archive exceeds transport limits")
        names = {}
        for item in entries:
            name = safe_path(item.filename)
            mode = item.external_attr >> 16
            if item.is_dir() or stat.S_IFMT(mode) not in (0, stat.S_IFREG) or item.flag_bits & 1:
                raise ValueError("Archive entry is not a regular unencrypted file")
            if name.casefold() in names:
                raise ValueError("Archive contains duplicate/colliding paths")
            names[name.casefold()] = name
        if any(str(parent).casefold() in names for name in names.values()
               for parent in PurePosixPath(name).parents if str(parent) != "."):
            raise ValueError("Archive file/directory collision")
        manifest_name = "native-ci.json" if native else "payload.json"
        if manifest_name not in archive.namelist() or archive.getinfo(manifest_name).file_size > 4 * 1024**2:
            raise ValueError("Missing or oversized payload inventory")
        metadata = json.loads(archive.read(manifest_name))
        inventory = metadata.get("files", {})
        if (metadata.get("version") != 1 or metadata.get("identity") != identity(environment) or
                not isinstance(inventory, dict) or set(archive.namelist()) != set(inventory) | {manifest_name} or
                (not native and (metadata.get("configuration") != "Debug" or metadata.get("platform") != "x64" or
                 not REQUIRED | {"TestHostApp.build.appxrecipe"} <= set(inventory) or
                 {f"frameworks/{n}.appx" for n in FRAMEWORKS} - set(inventory)))):
            raise ValueError("Payload inventory, completeness or provenance differs")
        for name, record in inventory.items():
            safe_path(name)
            item = archive.getinfo(name)
            if not isinstance(record, dict) or item.file_size != record.get("size"):
                raise ValueError("Payload size differs")
            with archive.open(item) as stream:
                if hashlib.file_digest(stream, "sha256").hexdigest() != record.get("sha256"):
                    raise ValueError("Payload file digest differs")
        if native:
            expected = native_summary(archive.read, environment)
            if metadata != {**expected, "files": inventory} or expected["status"] != "PASS":
                raise ValueError("Native CI evidence is incomplete or failed")
        else:
            validate_portable(archive, inventory)
        destination = Path(destination)
        destination.mkdir(parents=True, exist_ok=False)
        for name in archive.namelist():
            path = destination / name
            path.parent.mkdir(parents=True, exist_ok=True)
            with path.open("xb") as target, archive.open(name) as source:
                shutil.copyfileobj(source, target)


def validate_portable(archive, inventory):
    portable = xml(archive.read("TestHostApp.build.appxrecipe"))
    declared = {item.attrib["Include"] for tag in ("AppXManifest", "AppxPackagedFile")
                for item in children(portable, tag)}
    if declared != {p for p in inventory if p.startswith("app/")}:
        raise ValueError("Portable recipe does not describe the complete application")
    for item in children(portable, "AppXManifest") + children(portable, "AppxPackagedFile"):
        name = safe_path(item.attrib["Include"])
        if name != "app/" + safe_path(field(item, "PackagePath")):
            raise ValueError("Portable recipe has a stale or unsafe mapping")
    frameworks = {field(item, "Name"): safe_path(field(item, "AppxLocation"))
                  for item in children(portable, "ResolvedSDKReference")}
    if frameworks != {n: f"frameworks/{n}.appx" for n in FRAMEWORKS}:
        raise ValueError("Portable framework recipe differs")
    validate_host(archive.read("app/AppxManifest.xml"), {
        n: framework_identity(io.BytesIO(archive.read(path))) for n, path in frameworks.items()
    })


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("stage", "extract", "stage-units", "check-units",
                                              "record-native", "check-native", "extract-native"))
    parser.add_argument("--workspace")
    parser.add_argument("--archive")
    parser.add_argument("--digest")
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    if args.operation == "stage":
        stage(args.workspace, args.output, os.environ)
    elif args.operation == "stage-units":
        stage_units(args.workspace, args.output, os.environ)
    elif args.operation == "check-units":
        check_units(args.output, os.environ)
    elif args.operation == "record-native":
        receipt = record_native(args.output, os.environ)
        print(json.dumps({key: value for key, value in receipt.items() if key != "files"}, sort_keys=True))
    elif args.operation == "check-native":
        receipt = json.loads(regular(Path(args.output) / "native-ci.json").read_bytes())
        expected = native_summary(lambda name: regular(Path(args.output) / name).read_bytes(), os.environ)
        if receipt != {**expected, "files": receipt.get("files")} or expected["status"] != "PASS":
            raise ValueError("Native CI scan failed or is incomplete; see native-ci.json and surface results")
    else:
        extract(args.archive, args.output, args.digest, os.environ, native=args.operation == "extract-native")
