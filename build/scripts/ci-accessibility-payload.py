"""Portable TestHost layout transport. Does not build, deploy or execute payloads."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
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


def identity(environment):
    result = {key: environment.get(key) for key in
              ("GITHUB_REPOSITORY", "GITHUB_REPOSITORY_ID", "EXPECTED_HEAD_SHA", "CI_RUN_ID", "CI_RUN_ATTEMPT", "PR_NUMBER")}
    if (not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", result["GITHUB_REPOSITORY"] or "") or
        not re.fullmatch(r"[0-9a-f]{40}", result["EXPECTED_HEAD_SHA"] or "") or
        any(not re.fullmatch(r"[1-9][0-9]*", result[k] or "") for k in
            ("GITHUB_REPOSITORY_ID", "CI_RUN_ID", "CI_RUN_ATTEMPT", "PR_NUMBER"))):
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
            source = Path(unquote(item.attrib["Include"]))
            regular(source)
            resolved = source.resolve(strict=True)
            if not any(resolved.is_relative_to(workspace / folder) for folder in ("bin/x64/Debug", "src", "packages")):
                raise ValueError("Recipe source is outside the active workspace")
            add("app/" + field(item, "PackagePath").replace("\\", "/"), resolved)
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
        source = regular(Path(unquote(field(item, "AppxLocation"))))
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


def extract(archive_path, destination, expected_digest, environment):
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
        if "payload.json" not in archive.namelist() or archive.getinfo("payload.json").file_size > 4 * 1024**2:
            raise ValueError("Missing or oversized payload inventory")
        metadata = json.loads(archive.read("payload.json"))
        inventory = metadata.get("files", {})
        if (metadata.get("version") != 1 or metadata.get("identity") != identity(environment) or
                metadata.get("configuration") != "Debug" or metadata.get("platform") != "x64" or
                not isinstance(inventory, dict) or set(archive.namelist()) != set(inventory) | {"payload.json"} or
                not REQUIRED | {"TestHostApp.build.appxrecipe"} <= set(inventory) or
                {f"frameworks/{n}.appx" for n in FRAMEWORKS} - set(inventory)):
            raise ValueError("Payload inventory, completeness or provenance differs")
        for name, record in inventory.items():
            safe_path(name)
            item = archive.getinfo(name)
            if not isinstance(record, dict) or item.file_size != record.get("size"):
                raise ValueError("Payload size differs")
            with archive.open(item) as stream:
                if hashlib.file_digest(stream, "sha256").hexdigest() != record.get("sha256"):
                    raise ValueError("Payload file digest differs")
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
        destination = Path(destination)
        destination.mkdir(parents=True, exist_ok=False)
        for name in archive.namelist():
            path = destination / name
            path.parent.mkdir(parents=True, exist_ok=True)
            with path.open("xb") as target, archive.open(name) as source:
                shutil.copyfileobj(source, target)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("stage", "extract"))
    parser.add_argument("--workspace")
    parser.add_argument("--archive")
    parser.add_argument("--digest")
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    if args.operation == "stage":
        stage(args.workspace, args.output, os.environ)
    else:
        extract(args.archive, args.output, args.digest, os.environ)
