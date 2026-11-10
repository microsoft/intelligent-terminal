#!/usr/bin/env python3
"""Fail-closed routing for the small agent-written Markdown handoffs.

This parses decisions, never renders the human summary or judges accessibility.
"""
import argparse
import json
from pathlib import Path
import re
import subprocess


def prior_reviews(repository, number):
    """Bounded existing PR evidence, not a novelty classifier or instruction source."""
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository) or number < 1:
        raise ValueError("review context requires a repository and positive PR number")
    endpoint = f"repos/{repository}/pulls/{number}/comments?per_page=20&sort=updated&direction=desc"
    result = {"repository": repository, "pr_number": number, "endpoint": endpoint,
              "coverage": "unavailable", "comments": []}
    try:
        comments = json.loads(subprocess.check_output(
            ["gh", "api", endpoint], timeout=30, stderr=subprocess.PIPE))
        if not isinstance(comments, list):
            raise ValueError("review comments response must be an array")
        # Keep reply IDs so acknowledgements are not confused with allegations.
        # Bound prompt input; links retain access to any clipped evidence.
        fields = ("id", "in_reply_to_id", "path", "line", "commit_id", "body",
                  "html_url", "created_at", "updated_at")
        result["comments"] = [
            {**{field: item.get(field) for field in fields if field != "body"},
             "body": (item.get("body") or "")[:2000],
             "body_truncated": len(item.get("body") or "") > 2000,
             "source_commit_url": f"https://github.com/{repository}/commit/{item['commit_id']}"
             if isinstance(item.get("commit_id"), str) and re.fullmatch(r"[0-9a-fA-F]{40}", item["commit_id"]) else None,
             "author": item.get("user", {}).get("login")}
            for item in comments[:20]
        ]
        result["coverage"] = "limited" if (
            len(comments) >= 20 or any(item["body_truncated"] for item in result["comments"])) else "available"
    except (subprocess.SubprocessError, OSError, ValueError, TypeError, AttributeError):
        # Optional context never blocks the source review. No response/error body
        # is serialized: it can contain credentials or unrelated server details.
        result["reason"] = "Existing PR review comments could not be read; novelty and disposition are unknown."
    return result


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
    parser.add_argument("kind", choices=("triage", "native", "context"))
    parser.add_argument("path", type=Path)
    parser.add_argument("--base")
    parser.add_argument("--head")
    parser.add_argument("--repository")
    parser.add_argument("--pr-number", type=int)
    args = parser.parse_args()
    if args.kind == "context":
        if not args.repository or args.pr_number is None:
            parser.error("context requires --repository and --pr-number")
        args.path.parent.mkdir(parents=True, exist_ok=True)
        args.path.write_text(json.dumps(prior_reviews(args.repository, args.pr_number),
                                        ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        return
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
