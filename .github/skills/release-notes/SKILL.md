---
name: release-notes
description: 'Generate user-facing release notes for Intelligent Terminal. Use when asked to write release notes, changelog, what-is-new summary, or prepare a release. Compares git commits between releases, looks up PR-linked issues and community contributors, then outputs formatted notes with "Verbed + Impact + Scenario" style bullets, issue references, and contributor thanks.'
---

# Release Notes Generator

Generate polished, user-facing release notes for Intelligent Terminal releases by analyzing git history, PR metadata, and issue links.

## When to Use This Skill

- User asks to write release notes or changelog
- User asks "what's new since last release"
- User asks to prepare a release
- User asks to summarize recent commits for end-users

## Step-by-Step Workflow

### Phase 1: Identify the Commit Range

The base is the **previous release tag** (the `vX.Y.Z` sequence, e.g. `v0.1.2`) and the head is **`main`**. Do **not** anchor on `stable` (it is a cherry-picked subset that lags the release tag), on the legacy four-part `v0.1.NNNN.0` tags, or on upstream Windows Terminal `v1.x` tags — none of those are this fork's release line, and a plain `git tag --sort=-version:refname | head` surfaces exactly those wrong anchors.

1. Determine the **last release point** (in order of preference):
   - **From the release-notes files (source of truth).** The newest `doc/release-notes/vX.Y.Z.md` names the last shipped version; use its `vX.Y.Z` tag as the base.
     ```bash
     ls doc/release-notes/v*.md | sort -V | tail -1   # e.g. v0.1.2.md → base tag v0.1.2
     ```
     ```powershell
     Get-ChildItem doc/release-notes/v*.md | Sort-Object { [version]($_.BaseName -replace '^v','') } | Select-Object -Last 1
     ```
   - **From tags (confirm before trusting).** A *candidate* base is the newest 3-part tag merged into `main` (the filter below drops upstream `v1.x` and the stale `v0.1.NNNN.0` tags). **Verify it is really the last release** — the `v0.1.x` line has out-of-band tags (e.g. `v0.1.18`) that outrank the true last release under version sort, so this is a hint, not an authority:
     ```bash
     git tag --list 'v*' --merged main | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | sort -V | tail -1
     ```
     ```powershell
     git tag --list 'v*' --merged main | Where-Object { $_ -match '^v\d+\.\d+\.\d+$' } | Sort-Object { [version]($_ -replace '^v','') } | Select-Object -Last 1
     ```
   - The user may specify the base commit/tag directly.
   - If still unsure, **ask the user** — never guess from `git tag | head` in this repo.
2. List all commits from the base to `main`:
   ```bash
   git log --oneline --reverse <last-release-tag>..main
   ```
3. Extract PR numbers from commit messages (pattern: `(#NNN)`).

> The `0.1.xxxx.0` build number is injected by CI at release time; the human-facing version is the `vX.Y.Z` sequence.

### Phase 2: Enrich with PR Metadata

For each PR number, look up linked issues and author info:

```bash
gh pr view <number> --repo <owner>/<repo> --json number,title,body,author,closingIssuesReferences
```

> `<owner>/<repo>` is `microsoft/intelligent-terminal` for this repo's own PRs, or `microsoft/terminal` for upstream `#20xxx` PRs.

To batch this lookup across many PRs at once, run [`scripts/Get-PrMetadata.ps1`](./scripts/Get-PrMetadata.ps1) with a list of PR numbers — it reports linked issues and flags community contributors (authors not in `references/core-team.md`).

Collect:
- **PR → Issue mapping**: Which PRs fix/close GitHub issues
- **Community contributors**: Authors NOT in the core team list

#### Core Team (do NOT thank as community contributors)

See [core-team.md](./references/core-team.md) for the current list.

### Phase 3: Write Release Notes

Use the [release notes template](./templates/release-notes-template.md) and follow these rules:

#### Formatting Rules

1. **Every bullet uses "Verbed + Impact + Scenario"** — a human must understand what changed and why it matters to them
   - ✅ `**Fixed empty agent session views after first login** so the first tab's AI session reconnects and shows your conversation instead of appearing blank: #188`
   - ❌ `Fixed bug #188` (no impact or scenario)
   - ❌ `emit connection_state:closed on UI-initiated pane/tab close` (developer jargon)

2. **Every bullet has a PR or issue number** at the end (e.g., `: #195` or `: #188, #199`)
   - If the PR has a linked issue, use the issue number
   - If no linked issue, use the PR number
   - Group related fixes that share user impact into one bullet with multiple numbers

3. **Omit purely internal changes** — CI fixes, code refactors, dev docs, test-only changes, localization bot updates — unless they have direct user-visible impact

4. **Avoid detailed security disclosures in public release notes** — fold security-related stability fixes into the Bug Fixes section rather than calling them out as security issues, and coordinate any security-sensitive wording with maintainers per [SECURITY.md](../../../SECURITY.md)

5. **Thank community contributors** in a 💜 Community section with GitHub profile links:
   ```markdown
   - [@username](https://github.com/username) (Display Name) — what they contributed (#PR)
   ```

#### Section Structure

Use these sections in order:
- `## ✨ New Features` — new capabilities users can use
- `## 🔧 Improvements` — enhancements to existing features
- `## 🐛 Bug Fixes` — things that were broken and are now fixed
- `## 💜 Community` — external contributor thanks
- `## 🚀 Top 5 Elevator-Pitch Points` — the most compelling highlights for social media / announcements

### Phase 4: Output

1. Save the final release notes to `doc/release-notes/vX.Y.Z.md` (committed alongside prior releases, e.g. `doc/release-notes/v0.1.2.md`). Draft in `Generated Files/` first if you want a gitignored scratch copy.
2. Present the full notes to the user
3. Also list the top 5 elevator-pitch points separately

## Gotchas

- **Avoid a dedicated 🔒 Security section** — prefer folding security-related stability improvements into Bug Fixes rather than publicly detailing security issues; see [SECURITY.md](../../../SECURITY.md) for vulnerability handling and coordinate sensitive wording with maintainers.
- **Upstream Windows Terminal PRs** (#20xxx numbers) should be looked up on `microsoft/terminal`, not `microsoft/intelligent-terminal`.
- **The "Verbed + Impact + Scenario" format is non-negotiable** — every single bullet must follow it. "Fixed X" alone is not enough; you must explain the user impact.
- **Some commits are cherry-picks from upstream** — check both repos when looking up PRs.
- **Group related commits** — if 3 PRs all improve the FRE, combine into one bullet with multiple issue numbers rather than 3 separate bullets.

## References

- [Core team list](./references/core-team.md)
- [Release notes template](./templates/release-notes-template.md)
- [Previous release notes example](./references/example-release-notes.md)
