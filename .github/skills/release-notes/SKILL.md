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

1. Determine the **last release point**. Ask the user or check:
   - The latest git tag: `git tag --sort=-version:refname | head -5`
     (PowerShell: `git tag --sort=-version:refname | Select-Object -First 5`)
   - The last release notes file
   - The user may specify a commit hash directly
2. List all commits since that point:
   ```bash
   git log --oneline --reverse <last-release>..HEAD
   ```
3. Extract PR numbers from commit messages (pattern: `(#NNN)`)

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

1. Save the release notes to `Generated Files/release-notes-v<version>.md` (this folder is gitignored — generated output only)
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
