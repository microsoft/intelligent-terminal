#!/usr/bin/env python3
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

"""Check bundled local-executable ACP metadata against the pinned registry.

No agent installation, execution, authentication or runtime network requests.
This checker is offline. It does not install, execute or authenticate providers.
"""

import argparse
import json
from pathlib import Path
import re
import xml.etree.ElementTree as ET

REVISION = "2a27ff18b5816f47d7ec538e22943ff194e05131"

# Exact-version package.json bin fields, linked in the accompanying spec.
NPM_BINS = {
    "bob": "bob", "cline": "cline", "codebuddy-code": "codebuddy-code",
    "dimcode": "dimcode", "dirac": "dirac",
    "github-copilot": "copilot-language-server", "glm-acp-agent": "glm-acp-agent",
    "grok-build": "grok", "kimi-code": "kimi", "minimax-code": "code-acp",
    "nova": "nova", "pi-acp": "pi-acp", "qoder": "qodercli",
    "qwen-code": "qwen", "sigit": "sigit",
}


def check_registry_svg(path, id_):
    root = ET.parse(path).getroot()
    assert root.find(".//{http://www.w3.org/2000/svg}text") is None, f"{id_}: native SVG cannot render text"
    if id_ == "codebuddy-code":
        # The pinned source's paths extend beyond its original 16x16 canvas.
        assert root.attrib["viewBox"] == "0 0 21 21", f"{id_}: artwork would be clipped"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--registry", type=Path, required=True)
    parser.add_argument("--revision", required=True, choices=[REVISION])
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[2]
    header = (repo / "src/cascadia/inc/AcpAgentCatalog.h").read_text(encoding="utf-8")
    assert REVISION in header
    rows = re.findall(
        r'Agent\s*\{\s*L"([^"]+)"\s*,\s*L"([^"]+)"\s*,'
        r'\s*L"([^"]+)"\s*,\s*L"([^"]*)"(?:\s*,\s*L"([^"]*)")?'
        r'(?:\s*,\s*(PoolsideArchiveExecutable))?\s*\}',
        header,
    )
    assert len(rows) == 30 and len({r[0] for r in rows}) == len(rows), "30 distinct local-command candidates required"
    catalog = {row[0]: row for row in rows}
    manifests = {
        p.parent.name: json.loads(p.read_text(encoding="utf-8"))
        for p in args.registry.glob("*/agent.json")
    }
    quarantine_data = json.loads((args.registry / "quarantine.json").read_text(encoding="utf-8"))
    # The pinned quarantine file is an array of registry IDs.
    quarantined = set(quarantine_data)
    assert quarantined == {"agoragentic-acp", "deepagents", "minion-code", "vtcode"}
    excluded = {
        "antigravity-acp", "claude-acp", "codex-acp", "gemini",
        "github-copilot-cli", "opencode",  # Existing built-ins, no duplicates.
        "auggie", "factory-droid", "fast-agent", "muse-code",  # Required env.
        "kiro",  # No pinned Windows launch.
        "kimi",  # Same installed executable/ACP invocation as kimi-code.
    }
    assert len(manifests) == 46 and set(catalog) == set(manifests) - quarantined - excluded
    assets = repo / "src/cascadia/CascadiaPackage/AgentIcons"
    expected_assets = {"NOTICE.txt", "LICENSE-ACP-Registry.txt"}
    for id_, row in catalog.items():
        manifest = manifests[id_]
        distribution = manifest["distribution"]
        _, _, executable, arguments, dependency, alternate = row
        assert executable not in {"npx", "uvx", "node", "python"}, "runtime is not agent installation"
        assert alternate == ("PoolsideArchiveExecutable" if id_ == "poolside" else "")
        if id_ in NPM_BINS:
            source = distribution["npx"]
            assert executable == NPM_BINS[id_], f"{id_}: published bin drift"
        else:
            source = distribution["binary"]["windows-x86_64"]
            leaf = source["cmd"].replace("\\", "/").split("/")[-1]
            if id_ == "poolside":
                # The pinned provider README documents the installed pool
                # launcher; both archive aliases must match pinned manifests.
                assert executable == "pool"
                aliases = re.findall(r'PoolsideArchiveExecutable\s*\{\s*L"([^"]+)"', header)
                expected_aliases = {
                    Path(distribution["binary"][platform]["cmd"]).stem
                    for platform in ["windows-x86_64", "windows-aarch64"]
                }
                assert set(aliases) == expected_aliases
            else:
                assert executable == leaf.removesuffix(".exe").removesuffix(".cmd"), f"{id_}: executable drift"
        assert not source.get("env"), f"{id_}: required environment must not be dropped"
        expected_args = "--acp" if id_ == "sigit" else " ".join(source.get("args", []))
        assert arguments == expected_args, f"{id_}: ACP argument drift"
        assert dependency == {"amp-acp": "amp", "pi-acp": "pi"}.get(id_, "")
        builtin = id_ == "github-copilot"
        icon_root = assets if builtin else assets / "Registry"
        icon_id = "copilot" if builtin else id_
        for path in [icon_root / (icon_id + ".svg"), icon_root / "Masks" / (icon_id + ".png")]:
            assert path.is_file(), f"missing installed-agent icon: {path}"
        if not builtin:
            for suffix in [".svg", "-light.svg"]:
                check_registry_svg(icon_root / (icon_id + suffix), id_)
                expected_assets.add(icon_id + suffix)
            expected_assets.add(f"Masks/{icon_id}.png")
    actual_assets = {str(p.relative_to(assets / "Registry")).replace("\\", "/")
                     for p in (assets / "Registry").rglob("*") if p.is_file()}
    assert actual_assets == expected_assets, f"dead/missing artwork: {actual_assets ^ expected_assets}"
    xaml = (repo / "src/cascadia/TerminalSettingsEditor/AIAgents.xaml").read_text(encoding="utf-8")
    assert "AcpRegistryPreset" not in xaml and "AcpPresetList" not in xaml
    assert "ViewModel.AcpAgentList" in xaml and "CustomAcpCommandBox" in xaml
    print(f"PASS: {len(rows)} installed-command candidates; {len(actual_assets)} consumed artwork/license files; "
          "pinned arguments/bin evidence, no runtime-only discovery, no separate registry picker.")


if __name__ == "__main__":
    main()
