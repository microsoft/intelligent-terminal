#!/usr/bin/env python3
"""Validate producer delivery and the small machine-consumed decisions.

Human Markdown is passed through unchanged, not parsed or rendered.
"""
import argparse
import json
from pathlib import Path
import re
import subprocess
import sys


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


def read_output(path, limit):
    if path.is_symlink() or not path.is_file():
        raise ValueError(f"missing regular producer output: {path.name}")
    if path.stat().st_size > limit:
        raise ValueError(f"producer output exceeds {limit} bytes: {path.name}")
    text = path.read_text(encoding="utf-8")
    if not text.lstrip("\ufeff").strip():
        raise ValueError(f"empty producer output: {path.name}")
    return text


def producer(outcome, safe_outputs):
    if outcome != "success":
        raise ValueError(f"producer execution was {outcome or 'unknown'}")
    # Native infrastructure failures are recorded in the real safe-output queue,
    # not inferred from the framework's {"items": []} output placeholder.
    for line in read_output(safe_outputs, 1048576).splitlines():
        if not line.strip():
            continue
        item = json.loads(line)
        if not isinstance(item, dict) or not isinstance(item.get("type"), str):
            raise ValueError("invalid native safe-output entry")
        if item.get("type") in ("report_incomplete", "missing_data", "missing_tool"):
            raise ValueError(f"producer reported {item['type']}")


def deep(path):
    report = json.loads(read_output(path / "final.json", 1048576))
    required = {"version", "source_sha", "runtime_checks", "findings", "patch_files"}
    if not isinstance(report, dict) or not required.issubset(report):
        raise ValueError("incomplete deep structured output")
    read_output(path / "deep-summary.md", 32768)
    native(read_output(path / "native-checks.md", 16384))
    return "Complete"

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("kind", choices=("triage", "deep", "summary"))
    parser.add_argument("path", type=Path)
    parser.add_argument("--base")
    parser.add_argument("--head")
    parser.add_argument("--producer-outcome")
    parser.add_argument("--safe-outputs", type=Path)
    args = parser.parse_args()
    if args.producer_outcome is not None:
        if args.safe_outputs is None:
            raise ValueError("producer validation requires the native safe-output queue")
        producer(args.producer_outcome, args.safe_outputs)
    if args.kind == "deep":
        print(deep(args.path))
        return
    text = read_output(args.path, 32768 if args.kind == "summary" else 16384)
    if args.kind == "triage":
        if not all(value and re.fullmatch(r"[0-9a-fA-F]{40}", value) for value in (args.base, args.head)):
            raise ValueError("triage requires immutable base/head SHAs")
        data = subprocess.check_output(
            ["git", "diff", "--no-ext-diff", "--name-only", "-z", f"{args.base}...{args.head}"], text=True
        )
        result = triage(text, set(data.rstrip("\0").split("\0")))
    else:
        result = "Complete"
    print(result)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"No result: {error}", file=sys.stderr)
        sys.exit(1)
