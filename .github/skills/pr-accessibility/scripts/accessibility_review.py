#!/usr/bin/env python3
"""Prepare and validate the native WinUI accessibility review contract."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
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


def _git(root: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", *args],
        cwd=root,
        check=False,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    if result.returncode:
        raise RuntimeError(f"git {' '.join(args)} failed ({result.returncode}): {result.stderr.strip()}")
    return result.stdout


def _normalize(path: str) -> str:
    return path.strip().replace("\\", "/")


def _is_relevant(path: str) -> bool:
    normalized = _normalize(path)
    return normalized.startswith(UI_ROOTS) and Path(normalized).suffix.lower() in UI_SUFFIXES


def _changed_paths(root: Path, base: str, head: str) -> list[str]:
    output = _git(root, "diff", "--name-only", "--diff-filter=ACMRT", base, head, "--")
    return sorted({_normalize(line) for line in output.splitlines() if _is_relevant(line)})


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


def _overlaps(start: int, end: int, changed: set[int]) -> bool:
    return not changed or any(start <= line <= end for line in changed)


def _opening_tags(text: str):
    pattern = re.compile(
        r"<(?P<tag>(?:[A-Za-z_][\w.-]*:)?[A-Za-z_][\w.-]*)(?P<attrs>(?:\s[^<>]*?)?)(?P<self>/?)>",
        re.DOTALL,
    )
    for match in pattern.finditer(text):
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


def _has_name_source(attrs: str, body: str) -> bool:
    properties = (
        "AutomationProperties.Name",
        "AutomationProperties.LabeledBy",
        "x:Uid",
        "Header",
        "Content",
        "PlaceholderText",
        "Text",
        "Style",
    )
    if any(re.search(rf"\b{re.escape(prop)}\s*=\s*(['\"])(?:(?!\1).)+\1", attrs, re.DOTALL) for prop in properties):
        return True
    plain_text = re.sub(r"<[^>]+>", "", body)
    return bool(plain_text.strip())


def _scan_xaml(path: str, text: str, changed: set[int]) -> list[dict[str, Any]]:
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


def _review_surfaces(path: str, text: str, changed: set[int]) -> list[str]:
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


def prepare(root: Path, base: str, head: str, output: Path, changed_files: Path | None) -> None:
    if not re.fullmatch(r"[0-9a-fA-F]{40}", head):
        raise ValueError("head SHA must be an exact 40-character hexadecimal value")
    if changed_files:
        paths = sorted({_normalize(line) for line in changed_files.read_text(encoding="utf-8").splitlines() if _is_relevant(line)})
    else:
        if not re.fullmatch(r"[0-9a-fA-F]{40}", base):
            raise ValueError("base SHA must be an exact 40-character hexadecimal value")
        paths = _changed_paths(root, base, head)

    findings: list[dict[str, Any]] = []
    surfaces: dict[str, list[str]] = {}
    for path in paths:
        if changed_files:
            candidate = root / path
            if not candidate.is_file():
                continue
            text = candidate.read_text(encoding="utf-8-sig", errors="replace")
            changed = set()
        else:
            try:
                text = _git(root, "show", f"{head}:{path}")
            except RuntimeError:
                continue
            changed = _added_lines(root, base, head, path)
        if Path(path).suffix.lower() == ".xaml":
            findings.extend(_scan_xaml(path, text, changed))
        detected = _review_surfaces(path, text, changed)
        if detected:
            surfaces[path] = detected

    report = {
        "version": 1,
        "source_sha": head.lower(),
        "comparison_base_sha": base.lower(),
        "relevant": bool(paths),
        "changed_files": paths,
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
    if not _has_name_source(attrs, body):
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
            expected = _git(root, "show", f"{expected_head}:{path}")
            for item in sorted(items, key=lambda value: value["line"], reverse=True):
                expected = _remove_raw_view_at_line(expected, item["line"])
            candidate = (root / path).read_text(encoding="utf-8-sig", errors="strict")
            if candidate != expected:
                errors.append(
                    f"{path}: candidate contains changes beyond the trusted raw-view removal recipe"
                )
        except (OSError, RuntimeError, UnicodeError, ValueError) as error:
            errors.append(f"{path}: trusted static repair verification failed: {error}")
    return errors


def validate(
    root: Path,
    expected_head: str,
    current_head: str,
    same_repo: bool,
    prepared_path: Path,
    report_path: Path,
) -> None:
    prepared = json.loads(prepared_path.read_text(encoding="utf-8"))
    report = json.loads(report_path.read_text(encoding="utf-8"))
    errors: list[str] = []
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
    if patch_files:
        patch = _git(root, "diff", "--unified=0", expected_head, "--", *patch_files)
        for line in patch.splitlines():
            if line.startswith("+") and not line.startswith("+++") and re.search(
                r'AutomationProperties\.Name\s*=\s*["\'](?!\{)', line
            ):
                errors.append("patch introduces a literal accessible name; use the localization workflow")
                break
    if fixed and patch_files:
        errors.extend(_verify_static_repairs(root, expected_head, fixed, prepared, patch_files))

    known_files = set(prepared.get("changed_files", []))
    for item in findings:
        if isinstance(item, dict) and item.get("file") not in known_files and item.get("file") not in patch_files:
            errors.append(f"{item.get('stable_id', '<unknown>')}: finding file was not classified or patched")

    if errors:
        raise ValueError("\n".join(errors))

    for item in fixed:
        item["validation"] = [
            {
                "command": f"accessibility_review.py validate --recipe {STATIC_RAW_VIEW_RECIPE}",
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


def main() -> int:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    prepare_parser = subparsers.add_parser("prepare")
    prepare_parser.add_argument("--root", type=Path, required=True)
    prepare_parser.add_argument("--base", required=True)
    prepare_parser.add_argument("--head", required=True)
    prepare_parser.add_argument("--output", type=Path, required=True)
    prepare_parser.add_argument("--changed-files", type=Path)
    validate_parser = subparsers.add_parser("validate")
    validate_parser.add_argument("--root", type=Path, required=True)
    validate_parser.add_argument("--expected-head", required=True)
    validate_parser.add_argument("--current-head", required=True)
    validate_parser.add_argument("--same-repo", choices=("true", "false"), required=True)
    validate_parser.add_argument("--prepared", type=Path, required=True)
    validate_parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    try:
        if args.command == "prepare":
            prepare(args.root, args.base, args.head, args.output, args.changed_files)
        else:
            validate(
                args.root,
                args.expected_head,
                args.current_head,
                args.same_repo == "true",
                args.prepared,
                args.report,
            )
    except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
        print(f"accessibility review contract failed: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
