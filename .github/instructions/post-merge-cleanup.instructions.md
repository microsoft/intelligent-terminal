---
description: 'Uninstall packages registered from a completed PR worktree, then remove that worktree and its local branch.'
applyTo: '**'
---

# Post-merge cleanup

Run when asked to clean up a worktree after its PR is merged.

## 1. Uninstall packages from the worktree

- Resolve the exact worktree folder and use `Get-AppxPackage` to find packages
  whose `InstallLocation` is that folder or a child directory. Match the full
  directory boundary, not a partial path or package display name.
- If the user's regular installation uses this folder, stop and report that
  dependency instead of uninstalling it or removing its backing worktree.
- Preserve any needed application data before uninstalling; removal can delete
  package-private settings and sessions. Leave unrelated packages untouched.
- Close only the matching apps, uninstall each package by its exact
  `PackageFullName` using `Remove-AppxPackage`, and confirm removal succeeded.
  If a package cannot be removed, stop before deleting its backing folder.

## 2. Remove the worktree and local branch

- Check for uncommitted or untracked work and commits not included in the merged
  PR. Preserve them rather than discarding them.
- From another checkout, run `git worktree remove <worktree-path>`, then delete
  its local branch with `git branch -d <branch>`. If Git refuses either operation,
  report the blocker instead of forcing deletion.
- Report what was removed and anything left behind.
