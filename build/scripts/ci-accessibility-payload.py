"""Stage the MSBuild TestHost recipe and summarize native scan output on Windows."""
import argparse
import json
import os
from pathlib import Path, PurePosixPath, PureWindowsPath
import re
import shutil
import stat
from urllib.parse import unquote
import xml.etree.ElementTree as ET
import zipfile

FRAMEWORKS = {"Microsoft.UI.Xaml.2.8", "Microsoft.VCLibs.140.00.Debug"}
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
SURFACES = {"fre": "NextButton", "agents": "AcpAgentComboBox", "fre-settings": "AgentComboBox",
            "sidebar": "FixtureSidebar"}


def sidebar_state(tree, process):
    """Admit legacy Tab or corrected Group around the production Tree and live row."""
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
               n.get("control_type") in {"ControlType.Tab", "ControlType.Group"} and
               n.get("class_name") == "TabStrip"]
    lists = [n for n in tree if visible(n) and n.get("automation_id") == "ItemsList" and
             n.get("control_type") == "ControlType.Tree" and n.get("class_name") == "ListView"]
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


def identity(environment):
    result = {key: environment.get(key) for key in
              ("GITHUB_REPOSITORY", "GITHUB_REPOSITORY_ID", "EXPECTED_HEAD_SHA", "CI_RUN_ID", "CI_RUN_ATTEMPT", "PR_NUMBER")}
    if (not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", result["GITHUB_REPOSITORY"] or "") or
        not re.fullmatch(r"[0-9a-f]{40}", result["EXPECTED_HEAD_SHA"] or "") or
        any(not re.fullmatch(r"[1-9][0-9]*", result[k] or "") for k in
            ("GITHUB_REPOSITORY_ID", "CI_RUN_ID", "CI_RUN_ATTEMPT")) or
        not re.fullmatch(r"[1-9][0-9]*", result["PR_NUMBER"] or "")):
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
            if files[prior].read_bytes() != source.read_bytes():
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
    for item in children(recipe, "ResolvedSDKReference"):
        if field(item, "Architecture").lower() != "x64":
            continue
        source = framework_source(field(item, "AppxLocation"), workspace, environment)
        with zipfile.ZipFile(source) as package:
            name = children(xml(package.read("AppxManifest.xml")), "Identity")[0].get("Name")
        if name not in FRAMEWORKS or name in frameworks:
            raise ValueError("Unexpected or ambiguous framework recipe")
        frameworks[name] = f"frameworks/{name}.appx"
        add(frameworks[name], source)
    if set(frameworks) != FRAMEWORKS or not REQUIRED <= set(files):
        raise ValueError("Incomplete TestHost runtime payload")
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


def native_summary(read, environment):
    """Derive status from real Axe/UIA output; missing/invalid evidence is incomplete."""
    surfaces = {}
    for surface, marker in SURFACES.items():
        summary = {"status": "INCOMPLETE", "error_count": None, "process_id": None,
                   "source_sha": environment["EXPECTED_HEAD_SHA"]}
        try:
            for name in ("axe.stdout.log", "axe.stderr.log"):
                read(f"{surface}/{name}")
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
    (root / "native-ci.json").write_text(json.dumps(receipt, sort_keys=True), encoding="utf-8")
    lines = ["## Native accessibility", "", f"Source: `{environment['EXPECTED_HEAD_SHA']}`",
             f"Run: {environment['CI_RUN_ID']} / attempt {environment['CI_RUN_ATTEMPT']}",
             f"Status: **{receipt['status']}**", "", "| Surface | Status | Axe errors |",
             "| --- | --- | --- |"]
    lines += [f"| {name} | {data['status']} | {data['error_count'] if data['error_count'] is not None else 'unavailable'} |"
              for name, data in receipt["surfaces"].items()]
    lines += ["", "Raw result, Axe, UIA JSON and scanner logs accompany this summary.",
              "Smoke states only; no keyboard, Narrator, contrast or scaling certification.", ""]
    (root / "native-summary.md").write_text("\n".join(lines), encoding="utf-8")
    return receipt


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("stage", "record-native"))
    parser.add_argument("--workspace")
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    if args.operation == "stage":
        stage(args.workspace, args.output, os.environ)
    else:
        receipt = record_native(args.output, os.environ)
        print(json.dumps(receipt, sort_keys=True))
        raise SystemExit(0 if receipt["status"] == "PASS" else 1)
