# After-PR Review Handling: fix-in-PR vs. follow-up PR

Once the sync PR is open, reviewers (GitHub Copilot bot and humans) will
leave comments. This doc is the decision rubric and the mechanics.

## Why this exists

The cherry-pick PR's value to the reviewer is "**N small commits, each
faithful to one upstream commit, plus the bare minimum to make CI
green**". Two failure modes break that:

1. **Squashing fixes into the sync PR** (e.g. amending the cherry-pick
   commits or adding many follow-up commits to the same branch)
   destroys per-commit attribution, original author/committer dates, and
   makes "which upstream commit broke X?" un-bisectable.
2. **Substantive feedback bundled into the sync PR** forces the
   reviewer to mentally subtract those commits from every
   upstream-comparison check ("is this difference here because the
   cherry-pick was wrong, or because someone added a fix?").

So we keep the sync PR pristine and route everything else to a
follow-up PR.

## Decision rubric

| Comment / failure | Fix where | Why |
|---|---|---|
| Build error introduced by the cherry-pick batch | **Sync PR** — one focused commit | The sync PR is broken; the sync PR must be made buildable before merge. |
| Duplicate `.resw` keys / manifest collisions surfaced by `bz` but missed by the static-scan baseline | **Sync PR** — one focused commit | Same reason — without it the sync PR can't merge. |
| CI gate failure on the sync PR (check-spelling, lint, format) that is genuinely caused by the cherry-picked content | **Sync PR** — one focused commit | Same reason. |
| Copilot reviewer correctness finding (e.g. accumulator drift past clamp floor, missing cleanup on exception path) | **Follow-up PR** | Reviewer added value but the change is a *refactor* of upstream code, not a build fix. Cherry-pick PR stays faithful. |
| Translation error (`zh-TW` 工作模式 vs 工作階段) | **Follow-up PR** | The cherry-pick faithfully imported what upstream shipped. Correcting it is fork-local improvement. |
| Spelling-allowlist migration (move from `expect.txt` → `allow/*.txt`) | **Follow-up PR** | Repo-policy hygiene, not a build blocker. |
| Typo in a comment cherry-picked from upstream | **Follow-up PR** | Same. |
| Human reviewer asks for a design change / API tweak | **Follow-up PR** *or* push upstream | Fork-local refactor or upstream contribution, never bundled into the sync. |

When in doubt, ask: *"If this commit didn't exist, would the sync PR
still merge cleanly with a green build?"* — if yes, it belongs in the
follow-up PR.

## Mechanics

### Worktree setup

Stay on the [primary-worktree-clean-main
convention](./conflict-triage.md). Open a sibling worktree for the
follow-up so the main worktree stays clean:

```pwsh
$syncPr      = 220
$syncBranch  = 'upstream-sync/2026-06-04'   # the sync PR's head
$fixBranch   = "dev/$env:USERNAME/sync-$syncPr-review-fixes"
$fixWorktree = "..\it-$syncPr`fix"

# Sync branch tip must be local for the worktree to land at the right base
git fetch origin $syncBranch
git worktree add $fixWorktree -b $fixBranch "origin/$syncBranch"

Push-Location $fixWorktree
# … apply fixes, commit, push from here …
Pop-Location
```

### Commit shape

One focused commit per concern, mirroring the
[`copilot-pr-review-loop`](../../copilot-pr-review-loop/SKILL.md)
"one commit per round" rule:

```
fix(terminal): <concern-1>   # e.g. code-path corrections from Copilot
fix(loc):      <concern-2>   # e.g. translation corrections
build(spelling): <concern-3> # e.g. allow/expect migrations
```

Don't bundle them into a single mega-commit — `git blame` should still
point each line back to the specific finding that justified it.

### PR base

```pwsh
gh pr create `
  --repo microsoft/intelligent-terminal `
  --base $syncBranch `             # NOT main
  --head $fixBranch `
  --title "fix(sync-<syncPr>): <one-line summary>" `
  --body-file .pr-body.txt
```

The base is the **sync branch**, not `main`. That way the follow-up
rides along with the sync PR and only its fix commits show in the diff.
If the sync PR merges to `main` before the follow-up does, rebase the
follow-up onto `main` (the merge strategy will collapse it cleanly).

### Reply + resolve on the sync PR

After opening the follow-up, walk every review thread on the sync PR
that was deferred to the follow-up and reply + resolve:

> Addressed in #<follow-up-pr> (branch
> `dev/<alias>/sync-<sync-pr>-review-fixes` based on this PR's head).
> Will be merged together with this PR (or rebased onto `main` if this
> lands first).

Use [`copilot-pr-review-loop/scripts/06-reply-and-resolve.ps1`](../../copilot-pr-review-loop/scripts/06-reply-and-resolve.ps1)
to do this in a loop:

```pwsh
$ids = @('PRRT_kw...','PRRT_kw...',...)
$script = '.github/skills/copilot-pr-review-loop/scripts/06-reply-and-resolve.ps1'
$body = "Addressed in #<follow-up-pr> ..."
foreach ($id in $ids) { pwsh -NoProfile -File $script -ThreadId $id -Body $body }
```

### When the sync PR merges first

Rebase the follow-up onto `main`:

```pwsh
cd $fixWorktree
git fetch origin main
git rebase origin/main
git push --force-with-lease
```

Then change the follow-up's base on GitHub:

```pwsh
gh pr edit <follow-up-pr> --repo microsoft/intelligent-terminal --base main
```

## Anti-patterns

- ❌ Amending or rewriting the cherry-pick commits to "fix" a review
  finding. Destroys the per-commit attribution that was the whole
  reason for cherry-picking.
- ❌ Adding more than one extra commit to the sync PR. If a second
  build-fix commit is needed, that's a smell — investigate whether the
  first fix was incomplete or whether the second item really is a
  follow-up.
- ❌ Opening the follow-up against `main` while the sync PR is still
  unmerged. Diff fills up with the cherry-pick commits and the
  reviewer can't see what changed.
- ❌ Resolving a sync-PR thread without leaving a pointer to the
  follow-up PR. Future-you will have no record of where the fix went.

## Cross-references

- [SKILL.md § After-PR review handling](../SKILL.md#after-pr-review-handling--fix-in-pr-vs-follow-up-pr) — short version of this doc.
- [`copilot-pr-review-loop`](../../copilot-pr-review-loop/SKILL.md) — the skill that drives the follow-up PR through its own review rounds.
- [Branch + worktree convention](./conflict-triage.md) — keep `Q:\official\intelligent-terminal` on `main`; use sibling worktrees for PR work.
