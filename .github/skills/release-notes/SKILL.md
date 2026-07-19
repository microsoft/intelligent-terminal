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

The base is the **latest release tag** (the `vX.Y.Z` sequence, e.g. `v0.1.18`) and the head is **`main`**. The range is everything between them.

1. Find the **latest release tag** — the newest 3-part `vX.Y.Z` tag that is merged into `main`. The filter is what keeps this reliable: it excludes upstream Windows Terminal `v1.x` tags (not on this fork's history) and the legacy four-part `v0.1.NNNN.0` tags (stale, some point at ancient upstream commits). Do **not** anchor on `stable` — it is a cherry-picked subset that lags the release tag.
   ```bash
   git tag --list 'v*' --merged main --sort=-v:refname | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | head -1
   ```
   ```powershell
   git tag --list 'v*' --merged main | Where-Object { $_ -match '^v\d+\.\d+\.\d+$' } | Sort-Object { [version]($_ -replace '^v','') } | Select-Object -Last 1
   ```
   (The user may override with a specific base commit/tag; if the plain `git tag` list looks nothing like the above — e.g. only `v1.x` shows up — the `--merged main` filter is being skipped.)
2. List all commits from the base tag to `main` (replace `$BASE_TAG` with the tag from step 1):
   ```bash
   git log --oneline --reverse "$BASE_TAG"..main
   ```
3. Extract PR numbers from commit messages (pattern: `(#NNN)`).

> The `0.1.xxxx.0` build number is injected by CI at release time; the human-facing version is the `vX.Y.Z` tag sequence.

### Phase 2: Enrich with PR Metadata

For each PR number, look up linked issues and author info (replace `PR_NUMBER` and `OWNER/REPO`):

```bash
gh pr view PR_NUMBER --repo OWNER/REPO --json number,title,body,author,closingIssuesReferences
```

> Use `OWNER/REPO` = `microsoft/intelligent-terminal` for this repo's own PRs — they live on the canonical repo even when you're working from a fork/clone — or `microsoft/terminal` for upstream `#20xxx` PRs.

To batch this lookup across many PRs at once, run [`scripts/Get-PrMetadata.ps1`](./scripts/Get-PrMetadata.ps1) with a list of PR numbers — it reports linked issues and flags community contributors (authors not in `references/core-team.md`).

Collect:
- **PR → Issue mapping**: Which PRs fix/close GitHub issues
- **Community contributors**: Authors NOT in the core team list

#### Core Team (do NOT thank as community contributors)

See [core-team.md](./references/core-team.md) for the current list.

### Phase 3: Write Release Notes

Use the [release notes template](./templates/release-notes-template.md) and follow these rules:

#### Formatting Rules

1. **Use numbered lists** (`1.`, `2.`, `3.`) within every section — matches the house style shown in [`references/example-release-notes.md`](./references/example-release-notes.md), not bullet points.

2. **Every item uses "Verbed + Impact + Scenario"** — a human must understand what changed and why it matters to them
   - ✅ `**Press F5 to refresh the session list.** Re-scans history on demand so sessions that appeared after launch show up without restarting. #344`
   - ❌ `Fixed bug #344` (no impact or scenario)
   - ❌ `emit connection_state:closed on UI-initiated pane/tab close` (developer jargon)

3. **Every item ends with its PR number(s)** — a space then `#PR` after the trailing period (e.g. `… without restarting. #344` or `… in strict-mode shells. #340`).
   - Use the **PR number** as the primary reference (the shipped notes are PR-centric).
   - In the 💜 Community section you may add the linked issue too, e.g. `(#366, closes #351)`.
   - Group related items that share user impact into one entry with multiple numbers (e.g. `#305, #365`).

4. **Open with a metadata blockquote header**, as shown in [`references/example-release-notes.md`](./references/example-release-notes.md): the base release tag + commit + build, the head `main` commit, the new-PR count, the CI-injects-build-number note, and the "base is the tag, not `stable`" note.

5. **Frame the notes** with a one-paragraph plain-language intro (the release's headline theme) after the header, and a closing call-to-action inviting users to file issues.

6. **Omit purely internal changes** — CI fixes, code refactors, dev docs, test-only changes, localization bot updates — unless they have direct user-visible impact. (Prior releases sometimes collect these under a `## Development` section instead of omitting them; follow the maintainer's preference for the release at hand.)

7. **Avoid detailed security disclosures in public release notes** — fold security-related stability fixes into the Bug Fixes section rather than calling them out as security issues, and coordinate any security-sensitive wording with maintainers per [SECURITY.md](../../../SECURITY.md)

8. **Thank community contributors** in a 💜 Community section with GitHub profile links:
   ```markdown
   1. [@username](https://github.com/username) (Display Name) — what they contributed. (#PR, closes #issue)
   ```

#### Section Structure

Open with the metadata blockquote header and the intro paragraph, then use these sections in order:
- `## ✨ New Features` — new capabilities users can use
- `## 🔧 Improvements` — enhancements to existing features
- `## 🐛 Bug Fixes` — things that were broken and are now fixed
- `## 💜 Community` — external contributor thanks
- `## 🚀 Top 5 Elevator-Pitch Points` — the most compelling highlights for social media / announcements

Close with the call-to-action paragraph.

### Phase 4: Output

1. **Draft** into `Generated Files/release-notes-vX.Y.Z.md` — this folder is gitignored, so the working draft stays untracked while you iterate and get user sign-off.
2. Present the full notes to the user for review.
3. Also list the top 5 elevator-pitch points separately.
4. **Publish (only once approved).** Copy the finalized notes to `doc/release-notes/vX.Y.Z.md` and commit them (create the `doc/release-notes/` folder if it doesn't exist yet — this is the intended home for published release notes). Follow the format in [`references/example-release-notes.md`](./references/example-release-notes.md). This committed file is the source of truth for the version — do **not** commit the `Generated Files/` draft.

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
