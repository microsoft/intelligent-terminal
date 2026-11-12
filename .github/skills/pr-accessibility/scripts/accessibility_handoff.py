#!/usr/bin/env python3
"""Fail-closed routing for the small agent-written Markdown handoffs.

This parses decisions, never renders the human summary or judges accessibility.
"""
import argparse
from pathlib import Path
import re
import subprocess


def triage(text, changed):
    lines = text.strip().splitlines()
    if not lines or lines[0] not in ("# Related", "# None", "# Uncertain"):
        raise ValueError("triage must start with # Related, # None or # Uncertain")
    if len(text.encode("utf-8")) > 16384:
        raise ValueError("triage exceeds 16 KiB")
    candidates = re.findall(r"^- `([^`\r\n]+)`$", text, re.MULTILINE)
    if any(line.startswith("#") for line in lines[1:]):
        raise ValueError("triage has more than one decision heading")
    if any(line.startswith("- ") and not re.fullmatch(r"- `[^`\r\n]+`", line) for line in lines):
        raise ValueError("candidate bullets must contain a backtick-quoted changed path")
    if len(candidates) != len(set(candidates)) or any(p not in changed for p in candidates):
        raise ValueError("candidate paths must be unique changed paths")
    if lines[0] == "# Related" and not candidates:
        raise ValueError("Related requires candidate changed paths")
    if len(lines) < 3 or not any(line.strip() and not line.startswith(("#", "- `")) for line in lines[1:]):
        raise ValueError("triage requires a short explanation")
    return lines[0][2:]


def native(text):
    # The deep reviewer selects applicability; uncertainty never selects a job.
    if not re.fullmatch(r"# Native checks: (Axe\.Windows|None)\n\n\S[\s\S]*", text):
        raise ValueError("invalid native-check selection")
    if len(text.encode("utf-8")) > 16384:
        raise ValueError("native selection exceeds 16 KiB")
    return "true" if text.startswith("# Native checks: Axe.Windows\n") else "false"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("kind", choices=("triage", "native"))
    parser.add_argument("path", type=Path)
    parser.add_argument("--base")
    parser.add_argument("--head")
    args = parser.parse_args()
    text = args.path.read_text(encoding="utf-8")
    if args.kind == "triage":
        if not all(value and re.fullmatch(r"[0-9a-fA-F]{40}", value) for value in (args.base, args.head)):
            raise ValueError("triage requires immutable base/head SHAs")
        data = subprocess.check_output(
            ["git", "diff", "--no-ext-diff", "--name-only", "-z", f"{args.base}...{args.head}"], text=True
        )
        result = triage(text, set(data.rstrip("\0").split("\0")))
    else:
        result = native(text)
    print(result)


if __name__ == "__main__":
    main()
