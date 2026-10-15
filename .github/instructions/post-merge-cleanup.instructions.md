---
description: 'Clean up PR-owned Dev packages, test hosts and generated worktree outputs after merge without losing user data or other work.'
applyTo: '**'
---

# Post-merge cleanup

Apply this workflow when finishing an agent-owned PR or when asked to clean up
its development environment. Creating or merging a PR alone is not permission
to uninstall apps or delete data. Do not run cleanup during ordinary coding.

## Record ownership before deployment

Use the [isolated worktree package workflow](../../doc/dev-worktree-package.md)
when another worktree owns the normal Dev package. Keep a small ownership record
in session artifacts outside directories that will be cleaned. Record:

- PR URL, branch, absolute worktree path and worktree token.
- Each installed Dev or test-host package's exact `PackageFullName`,
  `PackageFamilyName`, publisher and `InstallLocation`, with the deployment
  command and generated manifest or recipe that ties it to this worktree.
- Private extracted package layouts outside the worktree, and exact generated
  output directories created for the PR.

Include test hosts installed by test runners, not just the Terminal package.
Compare current-user registrations before and after deployment or testing to
identify additions and replacements. Never claim ownership of a pre-existing
registration merely because a test reused it.

## Review the cleanup scope

1. Verify the exact PR is merged, for example with
   `gh pr view <PR-URL> --json state,mergedAt,url`. An open or closed-unmerged PR
   requires a separate explicit cleanup request.
2. Inspect `git worktree list --porcelain`, the target's `git status --short`
   (including untracked files) and its local commits. Preserve source changes,
   branches, research and evidence; a squash merge need not make the feature
   branch an ancestor of main.
3. Re-query current-user packages with `Get-AppxPackage`. Match the ownership
   record against their current identity and layout. Show a table of exact
   package names, paths, ownership evidence and proposed actions; mark unknown
   ownership or active use as skipped. A Start-menu name such as `TestHostApp`,
   a naming prefix or an old timestamp is not proof of ownership.
4. Obtain approval for the exact uninstall and generated-directory removal
   scope before making destructive changes. Explain that uninstalling a package
   can delete its package-private settings, sessions and other application data.
   Missing ownership records require investigation, not a wildcard cleanup.

Never uninstall normal Intelligent Terminal Dev, Store or another worktree's
package. Never remove shared framework/dependency packages or use `-AllUsers`.
The source TestHostApp manifest uses `WindowsTerminal.TestHost`; inspect the
generated and installed identities rather than assuming that name identifies
every test-host registration. Preserve a shared or reused host.

### When the original worktree is gone

Compare each installed layout's original worktree with Git's worktree list and
the worktree's `.git` marker. A leftover directory containing only a retained
layout does not mean the checkout still exists. For layouts outside the
worktree, use deployment records or session evidence to recover the source
worktree; the layout location alone cannot establish its ownership.

Classify a package as an orphaned-worktree cleanup candidate only with evidence
that its original checkout is gone, it is a disposable PR installation and no
remaining worker uses it. Still require the approval and data backup above.
Preserve unknown provenance and blank/unreadable install locations for separate
investigation; they are not automatic uninstall targets.

Find test hosts by installed manifest identity, application executable and
test-run deployment records, not only `Name` or `IsDevelopmentMode`. Start-menu
entries can help map an AppID to its package family but are not an authoritative
installed-package inventory. If no matching registration exists, report that
fact rather than uninstalling an unrelated app to remove a stale recommendation.

## Clean up approved targets

Perform these steps from a shell outside the target outputs and package layouts.
Recheck identity and active use immediately before each destructive operation.

1. Close only processes proven to belong to the approved PR packages or test
   run. Use bounded waits, then terminate only exact owned PIDs and proven
   descendants if necessary. Never kill by executable name or interrupt another
   worker. If ownership or active use is uncertain, skip the target.
2. Back up useful package-private data, including settings and sessions, and
   retain required logs and test evidence outside every cleanup target. Verify
   the backup is readable before uninstalling. If data cannot be preserved,
   stop that target and report the blocker.
3. Uninstall each approved, disposable current-user package by its revalidated
   exact full name:

   ```powershell
   Remove-AppxPackage -Package $approvedPackageFullName -ErrorAction Stop
   ```

   Never pipe a broad name search into removal. Verify that the exact
   registration is absent before deleting its backing files.
4. Remove only approved, explicitly resolved generated directories and private
   extracted layouts. Check tracked files and inspect untracked content rather
   than treating every ignored or untracked file as disposable. Check that no
   remaining registration uses a target
   layout, including a layout inside build output. If a registration remains
   or cannot be inspected, preserve its files. Inspect links/junctions and do
   not follow them into other worktrees or shared caches. Do not use
   `git clean -fdx`, recursive wildcard deletion or recursive deletion of a
   repository/worktree root.
5. Keep the worktree folder and branch by default; "clean up the worktree" means
   reclaim generated build/test output, not erase the checkout. Only an explicit
   request to retire that exact worktree permits `git worktree remove <path>`,
   run from another checkout after preserving all local work and verifying that
   no registration depends on it. Never use `--force`. Delete a branch only
   with separate explicit authorization.

Do not delete Copilot conversations, user-wide agent configuration or unrelated
artifacts as part of PR cleanup.

## Verify and report

Re-query registrations and inspect every approved path. Report exact packages
uninstalled, generated directories removed, retained backup/evidence locations,
and every skipped or failed target with its reason. Verify protected packages
remain registered at their original locations and source changes are intact.
Do not claim cleanup completed when an uninstall failed or backing files remain;
report partial completion explicitly.
