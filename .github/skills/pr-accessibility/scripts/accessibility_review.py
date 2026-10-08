#!/usr/bin/env python3
"""Prepare and validate the native WinUI accessibility review contract."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any


UI_ROOTS = (
    "src/cascadia/TerminalApp/",
    "src/cascadia/TerminalControl/",
    "src/cascadia/TerminalSettingsEditor/",
    "src/cascadia/WindowsTerminal/",
    "src/cascadia/WindowsTerminal_UIATests/",
    "src/cascadia/UIMarkdown/",
)
UI_SUFFIXES = {".xaml", ".cpp", ".h", ".idl", ".resw", ".cs"}
PATCH_SUFFIXES = {".xaml", ".cpp", ".h", ".cs"}
INTERACTIVE_TAGS = {
    "AppBarButton",
    "AutoSuggestBox",
    "Button",
    "CheckBox",
    "ComboBox",
    "HyperlinkButton",
    "ListView",
    "ListViewItem",
    "MenuFlyoutItem",
    "NavigationViewItem",
    "NumberBox",
    "RadioButton",
    "Slider",
    "TabView",
    "TabViewItem",
    "TextBox",
    "ToggleButton",
    "ToggleSwitch",
    "TreeView",
    "TreeViewItem",
}
ICON_TAGS = ("FontIcon", "SymbolIcon", "PathIcon", "BitmapIcon", "Image", "Path")
REQUIRED_FINDING_FIELDS = {
    "stable_id",
    "severity",
    "confidence",
    "file",
    "line",
    "observed",
    "expected",
    "impact",
    "evidence",
    "proposed_fix",
    "validation",
    "disposition",
}
STATIC_RAW_VIEW_RECIPE = "AXSTATIC001-remove-raw-view"


def _git(root: Path, *args: str, env: dict[str, str] | None = None) -> str:
    result = subprocess.run(
        ["git", "--no-replace-objects", "-c", "core.fsmonitor=false", *args],
        cwd=root,
        check=False,
        capture_output=True,
        env={**os.environ, **env} if env else None,
    )
    if result.returncode:
        raise RuntimeError(f"git {' '.join(args)} failed ({result.returncode}): {result.stderr.decode('utf-8', errors='replace').strip()}")
    return result.stdout.decode("utf-8", errors="replace")


def _normalize(path: str) -> str:
    return path.strip().replace("\\", "/")


def _is_relevant(path: str) -> bool:
    normalized = _normalize(path)
    return normalized.startswith(UI_ROOTS) and Path(normalized).suffix.lower() in UI_SUFFIXES


def _changed_paths(root: Path, base: str, head: str) -> list[str]:
    # Expose both sides of renames, including moves out of the UI allowlist.
    output = _git(root, "diff", "--name-only", "-z", "--no-renames", "--diff-filter=ACDMRT", base, head, "--")
    return sorted({_normalize(path) for path in output.split("\0") if _is_relevant(path)})


def _source_blob(root: Path, revision: str, path: str) -> dict[str, str] | None:
    entry = _git(root, "ls-tree", "-z", revision, "--", path)
    if not entry:
        return None
    metadata, name = entry.rstrip("\0").split("\t", 1)
    _, kind, blob_sha = metadata.split()
    if kind != "blob" or name != path:
        raise ValueError(f"expected a source blob at {revision}:{path}")
    return {
        "source_sha": revision.lower(),
        "blob_sha": blob_sha,
        "file": path,
        "text": _git(root, "show", f"{revision}:{path}"),
    }


def _added_lines(root: Path, base: str, head: str, path: str) -> set[int]:
    diff = _git(root, "diff", "--unified=0", "--no-ext-diff", base, head, "--", path)
    lines: set[int] = set()
    for match in re.finditer(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@", diff, re.MULTILINE):
        start = int(match.group(1))
        count = int(match.group(2) or "1")
        lines.update(range(start, start + count))
    return lines


def _stable_id(rule: str, path: str, line: int, evidence: str) -> str:
    key = f"{rule}\n{_normalize(path)}\n{line}\n{' '.join(evidence.split())}"
    return f"{rule}-{hashlib.sha256(key.encode('utf-8')).hexdigest()[:12]}"


def _finding(
    rule: str,
    path: str,
    line: int,
    severity: str,
    confidence: str,
    observed: str,
    expected: str,
    impact: str,
    evidence: str,
) -> dict[str, Any]:
    return {
        "stable_id": _stable_id(rule, path, line, evidence),
        "rule": rule,
        "severity": severity,
        "confidence": confidence,
        "file": _normalize(path),
        "line": line,
        "observed": observed,
        "expected": expected,
        "impact": impact,
        "evidence": evidence,
        "proposed_fix": "",
        "validation": [],
        "disposition": "needs-review",
        "auto_fix_eligible": False,
    }


def _overlaps(start: int, end: int, changed: set[int] | None) -> bool:
    return changed is None or any(start <= line <= end for line in changed)


def _opening_tags(text: str):
    text_ranges = [
        (match.start(), match.end())
        for match in re.finditer(r"<!--.*?(?:-->|$)|<!\[CDATA\[.*?(?:\]\]>|$)", text, re.DOTALL)
    ]
    pattern = re.compile(
        r"<(?P<tag>(?:[A-Za-z_][\w.-]*:)?[A-Za-z_][\w.-]*)(?P<attrs>(?:\s[^<>]*?)?)(?P<self>/?)>",
        re.DOTALL,
    )
    for match in pattern.finditer(text):
        if any(start <= match.start() < end for start, end in text_ranges):
            continue
        tag = match.group("tag").split(":")[-1]
        start_line = text.count("\n", 0, match.start()) + 1
        end_line = text.count("\n", 0, match.end()) + 1
        yield match, tag, match.group("attrs"), start_line, end_line


def _element_body(text: str, match: re.Match[str], tag: str) -> str:
    if match.group("self"):
        return ""
    closing = re.search(rf"</(?:[A-Za-z_][\w.-]*:)?{re.escape(tag)}\s*>", text[match.end() :], re.DOTALL)
    if not closing:
        return ""
    return text[match.end() : match.end() + closing.start()]


def _has_name_source(attrs: str, body: str, *, include_style_hint: bool = True) -> bool:
    properties = (
        "AutomationProperties.Name",
        "AutomationProperties.LabeledBy",
        "x:Uid",
        "Header",
        "Content",
        "PlaceholderText",
        "Text",
    )
    if include_style_hint:
        properties += ("Style",)
    if any(re.search(rf"\b{re.escape(prop)}\s*=\s*(['\"])(?:(?!\1).)+\1", attrs, re.DOTALL) for prop in properties):
        return True
    visible_body = re.sub(r"<!--.*?(?:-->|$)", "", body, flags=re.DOTALL)
    plain_text = re.sub(r"<[^>]+>", "", visible_body)
    return bool(plain_text.strip())


def _scan_xaml(path: str, text: str, changed: set[int] | None) -> list[dict[str, Any]]:
    findings: list[dict[str, Any]] = []
    for match, tag, attrs, start_line, end_line in _opening_tags(text):
        if not _overlaps(start_line, end_line, changed):
            continue
        body = _element_body(text, match, tag)
        if tag in INTERACTIVE_TAGS:
            explicitly_inert = bool(
                re.search(r'\b(?:IsHitTestVisible|IsEnabled|IsTabStop)\s*=\s*["\']False["\']', attrs, re.I)
            )
            if (
                re.search(r'\bAutomationProperties\.AccessibilityView\s*=\s*["\']Raw["\']', attrs, re.I)
                and not explicitly_inert
            ):
                findings.append(
                    _finding(
                        "AXSTATIC001",
                        path,
                        start_line,
                        "HIGH",
                        "medium",
                        f"Interactive {tag} is explicitly placed only in the raw UIA view.",
                        "An operable control is exposed in the UIA control/content view unless an equivalent accessible operation is proven.",
                        "Keyboard, voice-access, and screen-reader users may be unable to discover or invoke the operation.",
                        match.group(0).strip(),
                    )
                )
            icon_only = any(re.search(rf"<(?:\w+:)?{icon}\b", body) for icon in ICON_TAGS)
            if icon_only and not _has_name_source(attrs, body):
                findings.append(
                    _finding(
                        "AXSTATIC002",
                        path,
                        start_line,
                        "MEDIUM",
                        "medium",
                        f"Changed {tag} appears icon-only and has no local name source.",
                        "Confirm a localized accessible name is supplied by content, x:Uid, style, binding, or an AutomationPeer.",
                        "An unnamed operation is ambiguous to screen-reader and voice-access users.",
                        match.group(0).strip(),
                    )
                )
        literal_name = re.search(
            r'\bAutomationProperties\.Name\s*=\s*(["\'])(?!\{)(?P<value>[^"\']*[A-Za-z][^"\']*)\1',
            attrs,
            re.DOTALL,
        )
        if literal_name:
            findings.append(
                _finding(
                    "AXSTATIC003",
                    path,
                    start_line,
                    "MEDIUM",
                    "high",
                    "A changed accessible name is a literal source string.",
                    "Customer-facing accessible text uses x:Uid, a resource lookup, or an existing localized binding.",
                    "The accessible experience can remain English in localized builds.",
                    literal_name.group(0).strip(),
                )
            )
    return findings


def _review_surfaces(path: str, text: str, changed: set[int] | None) -> list[str]:
    selected = "\n".join(
        line for number, line in enumerate(text.splitlines(), 1) if not changed or number in changed
    )
    checks: list[tuple[str, str]] = [
        ("names_roles_values", r"AutomationProperties|AutomationPeer|ControlType|GetPattern"),
        ("keyboard_focus", r"KeyDown|KeyUp|TabFocusNavigation|XYFocus|\.Focus\s*\(|FocusState"),
        ("announcements", r"LiveSetting|RaiseNotificationEvent|NotificationKind|UIA_.*Event"),
        ("disabled_busy", r"IsEnabled|IsBusy|ProgressRing|VisualState"),
        ("high_contrast_theme", r"HighContrast|ThemeResource|ActualTheme|RequestedTheme"),
        ("scaling_clipping", r"MaxWidth|MaxHeight|MinWidth|MinHeight|TextTrimming|TextWrapping|ScrollViewer"),
        ("tabs_panes_settings_agent_cards", r"Tab|Pane|Setting|Agent|Card"),
    ]
    return [name for name, pattern in checks if re.search(pattern, selected, re.I)]


def _verify_instruction_boundary(root: Path, base: str, head: str) -> None:
    changed = _git(root, "diff", "--name-only", "-z", "--no-renames", base, head, "--").split("\0")
    configuration_directories = {".github", ".claude", ".copilot", ".agents", ".gemini"}
    instruction_names = {"agents.md", "claude.md", "gemini.md", "copilot-instructions.md", "skill.md", ".mcp.json"}
    trusted_harness_paths = ("test/accessibility/", "build/scripts/get-dependenciesfromappxrecipe.ps1")
    blocked = []
    for path in changed:
        if not path:
            continue
        parts = Path(path.lower()).parts
        if (configuration_directories.intersection(parts)
                or path.lower().startswith(trusted_harness_paths)
                or parts[-1] in instruction_names
                or parts[-1].endswith((".instructions.md", ".agent.md"))):
            blocked.append(path)
    if blocked:
        raise ValueError(
            "PR changes operating instructions/configuration; source review is blocked before agent startup: "
            + ", ".join(sorted(blocked))
        )


def prepare(root: Path, base: str, head: str, output: Path,
            publication: dict[str, Any] | None = None) -> None:
    if not re.fullmatch(r"[0-9a-fA-F]{40}", head):
        raise ValueError("head SHA must be an exact 40-character hexadecimal value")
    if not re.fullmatch(r"[0-9a-fA-F]{40}", base):
        raise ValueError("base SHA must be an exact 40-character hexadecimal value")
    comparison_base = _git(root, "merge-base", base, head).strip()
    _verify_instruction_boundary(root, comparison_base, head)
    paths = _changed_paths(root, comparison_base, head)

    findings: list[dict[str, Any]] = []
    surfaces: dict[str, list[str]] = {}
    source_evidence: dict[str, dict[str, Any]] = {}
    for path in paths:
        base_blob = _source_blob(root, comparison_base, path)
        head_blob = _source_blob(root, head, path)
        if base_blob is None and head_blob is None:
            raise ValueError(f"classified changed source is absent from both revisions: {path}")
        source_evidence[path] = {
            "change": "deleted" if head_blob is None else "added" if base_blob is None else "modified",
            "base": base_blob,
            "head": head_blob,
            "diff": _git(root, "diff", "--no-renames", "--no-ext-diff", "--no-color", comparison_base, head, "--", path),
        }
        if head_blob is None:
            # Deleted controls are evidence, never head repair candidates.
            detected = _review_surfaces(path, base_blob["text"], set())
            if detected:
                surfaces[path] = detected
            continue
        text = head_blob["text"]
        changed = _added_lines(root, comparison_base, head, path)
        if Path(path).suffix.lower() == ".xaml":
            findings.extend(_scan_xaml(path, text, changed))
        detected = _review_surfaces(path, text, changed)
        if detected:
            surfaces[path] = detected

    report = {
        "version": 1,
        "source_sha": head.lower(),
        "comparison_base_sha": comparison_base.lower(),
        "trusted_base_sha": base.lower(),
        "relevant": bool(paths),
        "changed_files": paths,
        "source_evidence": source_evidence,
        "static_findings": sorted(findings, key=lambda item: (item["file"], item["line"], item["rule"])),
        "review_surfaces": surfaces,
        "runtime_checks": [
            {
                "id": "axe-windows-uia",
                "status": "SKIPPED",
                "reason": "Requires Windows, the built application, Axe.Windows, and an interactive desktop; this source-review job runs on Ubuntu.",
            },
            {
                "id": "keyboard-focus-screen-reader",
                "status": "SKIPPED",
                "reason": "Keyboard traversal, focus restoration/traps, and announcements require an interactive Windows desktop and assistive-technology observation.",
            },
            {
                "id": "contrast-theme-scale",
                "status": "SKIPPED",
                "reason": "High contrast, theme, text/display scaling, and clipping require rendered-state checks on Windows.",
            },
        ],
    }
    if publication is not None:
        if not publication.get("head_ref") or not publication.get("repository") or not isinstance(publication.get("pr_number"), int) or publication["pr_number"] < 1:
            raise ValueError("publication context requires branch, repository, and positive PR number")
        report["publication"] = publication
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def _validate_finding(item: Any) -> list[str]:
    errors: list[str] = []
    if not isinstance(item, dict):
        return ["finding is not an object"]
    missing = REQUIRED_FINDING_FIELDS - item.keys()
    if missing:
        errors.append(f"finding is missing fields: {', '.join(sorted(missing))}")
    if item.get("severity") not in {"HIGH", "MEDIUM", "LOW"}:
        errors.append("finding severity must be HIGH, MEDIUM, or LOW")
    if item.get("confidence") not in {"high", "medium", "low"}:
        errors.append("finding confidence must be high, medium, or low")
    if item.get("disposition") not in {"fixed", "remaining", "advice", "skipped", "blocked"}:
        errors.append("finding disposition is invalid")
    if item.get("severity") == "HIGH" and item.get("disposition") not in {"fixed", "remaining", "blocked"}:
        errors.append("HIGH findings must be fixed, remaining, or blocked")
    if item.get("severity") in {"MEDIUM", "LOW"} and item.get("disposition") not in {"advice", "skipped"}:
        errors.append("MEDIUM/LOW findings must be advice or skipped")
    if not isinstance(item.get("line"), int) or item.get("line", 0) < 1:
        errors.append("finding line must be a positive integer")
    for field in ("stable_id", "file", "observed", "expected", "impact", "evidence"):
        if not isinstance(item.get(field), str) or not item.get(field, "").strip():
            errors.append(f"finding {field} must be a non-empty string")
    if not isinstance(item.get("validation"), list):
        errors.append("finding validation must be an array")
    return errors


def _untracked_paths(root: Path) -> list[str]:
    output = _git(root, "ls-files", "--others", "--exclude-standard")
    return sorted({_normalize(line) for line in output.splitlines() if line.strip()})


def _validate_signal_coverage(prepared: dict[str, Any], report: dict[str, Any], findings: list[Any]) -> list[str]:
    errors: list[str] = []
    prepared_by_id = {
        item["stable_id"]: item for item in prepared.get("static_findings", [])
        if isinstance(item, dict) and isinstance(item.get("stable_id"), str)
    }
    prepared_ids = set(prepared_by_id)
    finding_ids: set[str] = set()
    for item in findings:
        if not isinstance(item, dict) or not isinstance(item.get("stable_id"), str):
            continue
        stable_id = item["stable_id"]
        if stable_id in finding_ids:
            errors.append(f"{stable_id}: duplicate finding stable_id")
        signal = prepared_by_id.get(stable_id)
        if signal and (item.get("file") != signal.get("file") or item.get("line") != signal.get("line")):
            errors.append(f"{stable_id}: reported signal location must match its prepared source location")
        finding_ids.add(stable_id)

    dismissals = report.get("dismissed_signals", [])
    if not isinstance(dismissals, list):
        errors.append("report dismissed_signals must be an array")
        dismissals = []
    dismissed_ids: set[str] = set()
    for index, item in enumerate(dismissals):
        if not isinstance(item, dict):
            errors.append(f"dismissed_signals[{index}]: dismissal must be an object")
            continue
        stable_id = item.get("stable_id")
        if not isinstance(stable_id, str) or not stable_id.strip():
            errors.append(f"dismissed_signals[{index}]: stable_id must be a non-empty string")
        else:
            if stable_id not in prepared_ids:
                errors.append(f"{stable_id}: dismissal must reference a known prepared stable_id")
            if stable_id in dismissed_ids:
                errors.append(f"{stable_id}: duplicate dismissed stable_id")
            if stable_id in finding_ids:
                errors.append(f"{stable_id}: signal cannot be both reported and dismissed")
            dismissed_ids.add(stable_id)
        # Require an explanation, not a verdict; domain review still judges its truth.
        for field in ("reason", "evidence"):
            value = item.get(field)
            if not isinstance(value, str) or len(value.strip()) < 12 or len(re.findall(r"\w+", value)) < 3:
                errors.append(f"dismissed_signals[{index}]: {field} must be a meaningful explanation")
    for stable_id in sorted(prepared_ids - finding_ids - dismissed_ids):
        errors.append(f"{stable_id}: prepared signal must be reported as a finding or explicitly dismissed")
    return errors


def _remove_raw_view_at_line(text: str, line: int) -> str:
    candidates = [
        (match, tag, attrs)
        for match, tag, attrs, start_line, _ in _opening_tags(text)
        if start_line == line
        and tag in INTERACTIVE_TAGS
        and re.search(r'\bAutomationProperties\.AccessibilityView\s*=\s*["\']Raw["\']', attrs, re.I)
    ]
    if len(candidates) != 1:
        raise ValueError(f"trusted static repair requires exactly one interactive raw-view control at line {line}")
    match, _, attrs = candidates[0]
    body = _element_body(text, match, match.group("tag").split(":")[-1])
    if not _has_name_source(attrs, body, include_style_hint=False):
        raise ValueError(f"trusted static repair requires an existing accessible name source at line {line}")
    repaired_opening, count = re.subn(
        r'\s+AutomationProperties\.AccessibilityView\s*=\s*["\']Raw["\']',
        "",
        match.group(0),
        count=1,
        flags=re.I,
    )
    if count != 1:
        raise ValueError(f"trusted static repair could not remove the raw-view property at line {line}")
    return text[: match.start()] + repaired_opening + text[match.end() :]


def _verify_static_repairs(
    root: Path,
    expected_head: str,
    fixed: list[dict[str, Any]],
    prepared: dict[str, Any],
    patch_files: list[str],
) -> list[str]:
    errors: list[str] = []
    prepared_by_id = {
        item.get("stable_id"): item
        for item in prepared.get("static_findings", [])
        if isinstance(item, dict) and item.get("stable_id")
    }
    recipe_files: set[str] = set()
    recipes_by_file: dict[str, list[dict[str, Any]]] = {}
    for item in fixed:
        stable_id = item.get("stable_id", "<unknown>")
        if item.get("repair_recipe") != STATIC_RAW_VIEW_RECIPE:
            errors.append(f"{stable_id}: no trusted static repair recipe was selected")
            continue
        signal = prepared_by_id.get(stable_id)
        if not signal or signal.get("rule") != "AXSTATIC001":
            errors.append(f"{stable_id}: trusted static repair requires the matching prepared AXSTATIC001 signal")
            continue
        path = _normalize(str(item.get("file", "")))
        if path != _normalize(str(signal.get("file", ""))) or item.get("line") != signal.get("line"):
            errors.append(f"{stable_id}: finding location does not match the prepared static signal")
            continue
        recipe_files.add(path)
        recipes_by_file.setdefault(path, []).append(item)

    if sorted(recipe_files) != patch_files:
        errors.append("every patch file must be attributable only to fixed findings with a trusted static recipe")

    for path, items in recipes_by_file.items():
        try:
            source_metadata = _git(root, "ls-tree", "-z", expected_head, "--", path).partition("\t")[0].split()
            if (len(source_metadata) != 3 or source_metadata[0] not in {"100644", "100755"}
                    or source_metadata[1] != "blob"):
                raise ValueError("reviewed source must be a regular Git blob")
            candidate_path = root / path
            if candidate_path.is_symlink() or not candidate_path.is_file():
                raise ValueError("candidate must be a regular file, not a symlink")
            index_entries = [
                entry for entry in _git(root, "ls-files", "--stage", "-z", "--", path).split("\0") if entry
            ]
            index_metadata = index_entries[0].partition("\t")[0].split() if len(index_entries) == 1 else []
            if (len(index_metadata) != 3 or index_metadata[0] != source_metadata[0]
                    or index_metadata[2] != "0"):
                raise ValueError("candidate must preserve the reviewed regular-file Git mode")
            expected = _git(root, "show", f"{expected_head}:{path}")
            for item in sorted(items, key=lambda value: value["line"], reverse=True):
                expected = _remove_raw_view_at_line(expected, item["line"])
            candidate = candidate_path.read_bytes().decode("utf-8", errors="strict")
            if candidate != expected:
                errors.append(
                    f"{path}: candidate contains changes beyond the trusted raw-view removal recipe"
                )
        except (OSError, RuntimeError, UnicodeError, ValueError) as error:
            errors.append(f"{path}: trusted static repair verification failed: {error}")
    return errors


def _verify_publication(root: Path, expected_head: str, fixed: list[dict[str, Any]],
                        prepared: dict[str, Any], queue: Path, transport_root: Path) -> list[str]:
    errors: list[str] = []
    try:
        if queue.is_symlink() or not queue.is_file():
            raise ValueError("safe-output queue must be a regular file")
        entries = [json.loads(line) for line in queue.read_text(encoding="utf-8").splitlines() if line.strip()]
        mode = "push_to_pull_request_branch" if fixed else "noop"
        if len(entries) != 1 or not isinstance(entries[0], dict) or entries[0].get("type") != mode:
            raise ValueError(f"publication requires exactly one {mode} request")
        if not fixed:
            return errors
        context = prepared.get("publication", {})
        entry = entries[0]
        if (entry.get("branch") != context.get("head_ref")
                or entry.get("head_repo") != context.get("repository")
                or (entry.get("repo") is not None and entry["repo"] != context.get("repository"))
                or str(entry.get("pull_request_number")) != str(context.get("pr_number"))
                or entry.get("base_commit") != expected_head):
            raise ValueError("queued repair identity does not match trusted PR context")
        if _git(root, "symbolic-ref", "--short", "HEAD").strip() != context.get("head_ref"):
            raise ValueError("repair must remain on the trusted PR head branch")
        if _git(root, "diff", "--name-only", "HEAD", "--").strip():
            raise ValueError("publication requires a clean committed candidate")
        parents = _git(root, "rev-list", "--parents", "-n", "1", "HEAD").split()
        if len(parents) != 2 or parents[1] != expected_head:
            raise ValueError("publication requires one repair commit directly on the reviewed head")
        candidate_head = parents[0]
        if any(transport_root.glob("aw-*.bundle")):
            raise ValueError("AM publication must not contain an alternate bundle transport")
        patches = list(transport_root.glob("aw-*.patch"))
        if len(patches) != 1 or patches[0].is_symlink() or not patches[0].is_file():
            raise ValueError("publication requires exactly one regular captured repair patch")
        patch = patches[0]
        if patch.stat().st_size > 4 * 1024 * 1024:
            raise ValueError("captured repair patch exceeds the publication size limit")
        headers = re.findall(rb"(?m)^From ([0-9a-f]{40}) Mon Sep 17 00:00:00 2001\r?$", patch.read_bytes())
        if headers != [candidate_head.encode("ascii")]:
            raise ValueError("captured repair patch does not identify the validated single commit")
        with tempfile.TemporaryDirectory(prefix="accessibility-patch-") as directory:
            index_env = {"GIT_INDEX_FILE": str(Path(directory) / "index")}
            _git(root, "read-tree", expected_head, env=index_env)
            _git(root, "apply", "--cached", "--whitespace=nowarn", str(patch.resolve()), env=index_env)
            captured_tree = _git(root, "write-tree", env=index_env).strip()
        candidate_tree = _git(root, "rev-parse", "HEAD^{tree}").strip()
        if captured_tree != candidate_tree:
            raise ValueError("captured repair patch tree differs from the validated candidate")
    except (OSError, RuntimeError, ValueError) as error:
        errors.append(f"publication verification failed: {error}")
    return errors


def validate(
    root: Path,
    expected_head: str,
    current_head: str,
    same_repo: bool,
    prepared_path: Path,
    report_path: Path,
    safe_output_queue: Path | None = None,
    transport_root: Path | None = None,
    summary_path: Path | None = None,
) -> None:
    prepared = json.loads(prepared_path.read_text(encoding="utf-8"))
    report = json.loads(report_path.read_text(encoding="utf-8"))
    errors: list[str] = []
    if summary_path is not None:
        try:
            if summary_path.is_symlink() or not summary_path.is_file():
                raise ValueError("summary must be a regular file, not a symlink")
            if not 0 < summary_path.stat().st_size <= 32 * 1024:
                raise ValueError("summary must contain at most 32 KiB of UTF-8 Markdown")
            if not summary_path.read_text(encoding="utf-8-sig", errors="strict").strip():
                raise ValueError("summary must not be empty")
        except (OSError, UnicodeError, ValueError) as error:
            errors.append(f"agent summary verification failed: {error}")
    if str(prepared.get("source_sha", "")).lower() != expected_head.lower():
        errors.append("prepared evidence does not match the immutable reviewed head")
    if report.get("version") != 1:
        errors.append("report version must be 1")
    if report.get("source_sha", "").lower() != expected_head.lower():
        errors.append("report source_sha does not match the immutable reviewed head")
    if current_head.lower() != expected_head.lower():
        errors.append("live pull request head no longer matches the immutable reviewed head")
    findings = report.get("findings")
    if not isinstance(findings, list):
        errors.append("report findings must be an array")
        findings = []
    errors.extend(_validate_signal_coverage(prepared, report, findings))
    runtime_checks = report.get("runtime_checks")
    expected_runtime = prepared.get("runtime_checks", [])
    if not isinstance(runtime_checks, list):
        errors.append("report runtime_checks must be an array")
        runtime_checks = []
    expected_runtime_ids = {item.get("id") for item in expected_runtime if isinstance(item, dict)}
    actual_runtime_ids = {item.get("id") for item in runtime_checks if isinstance(item, dict)}
    if actual_runtime_ids != expected_runtime_ids:
        errors.append("report runtime_checks must preserve every prepared runtime check")
    for check in runtime_checks:
        if not isinstance(check, dict) or check.get("status") not in {"SKIPPED", "BLOCKED"} or not check.get("reason"):
            errors.append("runtime checks require SKIPPED/BLOCKED status and a reason")
    for index, item in enumerate(findings):
        errors.extend(f"finding[{index}]: {error}" for error in _validate_finding(item))
        if isinstance(item, dict) and item.get("validation") != []:
            errors.append(
                f"{item.get('stable_id', f'finding[{index}]')}: model-authored validation is not trusted; "
                "leave validation empty for the native gate"
            )

    fixed = [item for item in findings if isinstance(item, dict) and item.get("disposition") == "fixed"]
    if not same_repo and fixed:
        errors.append("fork pull requests are read-only and cannot report fixed findings")
    for item in fixed:
        if item.get("severity") != "HIGH" or item.get("confidence") != "high":
            errors.append(f"{item.get('stable_id', '<unknown>')}: only high-confidence HIGH findings may be fixed")

    patch_output = _git(root, "diff", "--name-only", expected_head, "--")
    patch_files = sorted({_normalize(line) for line in patch_output.splitlines() if line.strip()})
    untracked_paths = _untracked_paths(root)
    if untracked_paths:
        errors.append(f"untracked files are not eligible for publication: {', '.join(untracked_paths)}")
    patch_declaration = report.get("patch_files")
    if not isinstance(patch_declaration, list):
        errors.append("report patch_files must be an array")
        patch_declaration = []
    declared_patch = sorted({_normalize(path) for path in patch_declaration if isinstance(path, str)})
    if patch_files != declared_patch:
        errors.append("report patch_files does not exactly match the final diff")
    if patch_files and not same_repo:
        errors.append("fork pull requests must not contain a patch")
    if patch_files and not fixed:
        errors.append("a patch requires at least one fixed finding")
    if fixed and not patch_files:
        errors.append("fixed findings require an actual candidate patch")
    for path in patch_files:
        if not path.startswith(UI_ROOTS) or Path(path).suffix.lower() not in PATCH_SUFFIXES:
            errors.append(f"patch path is outside the native UI/test allowlist: {path}")
        if path.endswith(".resw"):
            errors.append(f"resource edits must use the localization workflow, not accessibility auto-fix: {path}")
    if fixed and patch_files:
        errors.extend(_verify_static_repairs(root, expected_head, fixed, prepared, patch_files))
    if safe_output_queue is not None:
        if transport_root is None:
            errors.append("publication verification requires the transport directory")
        else:
            errors.extend(_verify_publication(root, expected_head, fixed, prepared, safe_output_queue, transport_root))

    known_files = set(prepared.get("changed_files", []))
    for item in findings:
        if isinstance(item, dict) and item.get("file") not in known_files and item.get("file") not in patch_files:
            errors.append(f"{item.get('stable_id', '<unknown>')}: finding file was not classified or patched")

    if errors:
        raise ValueError("\n".join(errors))

    for item in fixed:
        item["validation"] = [
            {
                "command": "accessibility_review.py validate",
                "recipe": STATIC_RAW_VIEW_RECIPE,
                "result": "PASS",
                "scope": "exact-candidate",
            }
        ]
    if fixed:
        report_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    counts = {severity: sum(1 for item in findings if item.get("severity") == severity) for severity in ("HIGH", "MEDIUM", "LOW")}
    print("## Native WinUI accessibility review")
    print("")
    print(f"- Source SHA: `{expected_head}`")
    print(f"- Relevant files: **{len(prepared.get('changed_files', []))}**")
    print(f"- Findings: **{counts['HIGH']} high**, **{counts['MEDIUM']} medium**, **{counts['LOW']} low**")
    print(f"- Fixed: **{len(fixed)}**; final patch files: **{len(patch_files)}**")
    print(f"- Native runtime checks skipped/blocked: **{len(runtime_checks)}**")
    remaining = [item for item in findings if item.get("disposition") in {"remaining", "blocked"}]
    if remaining:
        print("")
        print("### Must fix / blocked")
        for item in remaining:
            print(f"- `{item['stable_id']}` `{item['file']}:{item['line']}` — {item['impact']}")
    advisory = [item for item in findings if item.get("disposition") in {"advice", "skipped"}]
    if advisory:
        print("")
        print("### Advisory findings")
        for item in advisory:
            print(f"- `{item['stable_id']}` `{item['file']}:{item['line']}` — {item['impact']}")
            for field in ("observed", "expected", "evidence", "proposed_fix"):
                print(f"  {field}: {json.dumps(item[field], ensure_ascii=True)}")


def main() -> int:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    prepare_parser = subparsers.add_parser("prepare")
    prepare_parser.add_argument("--root", type=Path, required=True)
    prepare_parser.add_argument("--base", required=True)
    prepare_parser.add_argument("--head", required=True)
    prepare_parser.add_argument("--output", type=Path, required=True)
    prepare_parser.add_argument("--publication-branch")
    prepare_parser.add_argument("--publication-repository")
    prepare_parser.add_argument("--publication-pr-number", type=int)
    validate_parser = subparsers.add_parser("validate")
    validate_parser.add_argument("--root", type=Path, required=True)
    validate_parser.add_argument("--expected-head", required=True)
    validate_parser.add_argument("--current-head", required=True)
    validate_parser.add_argument("--same-repo", choices=("true", "false"), required=True)
    validate_parser.add_argument("--prepared", type=Path, required=True)
    validate_parser.add_argument("--report", type=Path, required=True)
    validate_parser.add_argument("--safe-output-queue", type=Path)
    validate_parser.add_argument("--transport-root", type=Path)
    validate_parser.add_argument("--summary", type=Path)
    args = parser.parse_args()
    try:
        if args.command == "prepare":
            publication = None
            if any(value is not None for value in (args.publication_branch, args.publication_repository, args.publication_pr_number)):
                publication = {"head_ref": args.publication_branch, "repository": args.publication_repository,
                               "pr_number": args.publication_pr_number}
            prepare(args.root, args.base, args.head, args.output, publication)
        else:
            validate(
                args.root,
                args.expected_head,
                args.current_head,
                args.same_repo == "true",
                args.prepared,
                args.report,
                args.safe_output_queue,
                args.transport_root,
                args.summary,
            )
    except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
        print(f"accessibility review contract failed: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
