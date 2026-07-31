---
name: wta-rust-dependency-update
description: 'Regenerate and verify WTA Rust third-party attribution after Cargo dependency changes. Use when handling Dependabot PRs for /tools/wta, editing tools/wta/Cargo.toml or Cargo.lock, changing dependency features, finishing or reviewing a WTA dependency PR, or fixing stale NOTICE.md or cgmanifest CI failures.'
---

# WTA Rust Dependency Update

Keep WTA Cargo dependency changes synchronized with the generated Component
Governance manifest and third-party notices required for the statically linked
`wta.exe`.

## When to Use This Skill

- Complete or review a Dependabot PR for `/tools/wta`.
- Add, remove, or upgrade a dependency in `tools/wta/Cargo.toml`.
- Update `tools/wta/Cargo.lock` in a way that changes runtime dependencies.
- Change dependency features that add or remove transitive runtime crates.
- Fix the **Verify WTA third-party notices** CI failure.

Do not run this workflow for Rust source-only changes that leave the dependency
graph unchanged.

## Prerequisites

- Run commands from the repository root.
- Use PowerShell 7 or later (`pwsh`), not Windows PowerShell 5.1.
- Read `tools/wta/AGENTS.md` and the WTA Rust instructions before changing
  Cargo files.
- Preserve unrelated worktree changes; never reset or overwrite them.

## Workflow

1. Inspect the PR or worktree diff and confirm which Cargo manifest, lockfile,
   or feature change triggered the dependency update.
2. Regenerate both attribution artifacts:

   ```powershell
   pwsh -File .\build\scripts\Generate-WtaThirdPartyNotices.ps1
   ```

3. Review the dependency and generated changes together:

   ```powershell
   git diff --check
   git diff -- tools/wta/Cargo.toml tools/wta/Cargo.lock `
       tools/wta/cgmanifest.json NOTICE.md
   ```

4. Run the same offline consistency check used by CI:

   ```powershell
   pwsh -File .\build\scripts\Generate-WtaThirdPartyNotices.ps1 -Verify
   ```

5. Finish according to the result:
   - If generation produced changes, keep `NOTICE.md` and
     `tools/wta/cgmanifest.json` in the same PR as the Cargo update.
   - For a Dependabot branch, add a follow-up commit; do not rewrite the bot
     commit unless the user explicitly requests it.
   - If generation produced no changes, do not create an empty attribution
     commit; report that `-Verify` passed.
   - Commit or push only when requested. Include all generated attribution
     changes together and describe them as generated output.

## Completion Gate

- The normal generator exits successfully.
- Generated changes contain only dependency attribution expected from the
  Cargo update.
- `git diff --check` passes.
- `Generate-WtaThirdPartyNotices.ps1 -Verify` reports the same runtime crate
  count in `cgmanifest.json` and `NOTICE.md`.
- No dependency PR is declared ready while generated artifacts are stale.

## Gotchas

- **Do not edit generated attribution text by hand.** Fix the Cargo metadata or
  generator and rerun it.
- **Do not run only `-Verify` after a dependency change.** Verification detects
  stale artifacts but does not regenerate them.
- **Do not assume every lockfile change needs an attribution diff.** Dev-only,
  build-only, or platform-excluded dependency changes may leave runtime output
  unchanged.
- **Do not omit `NOTICE.md` when `cgmanifest.json` changes, or vice versa.**
  They are generated from the same runtime dependency graph.
- **Review major updates separately.** In particular,
  `agent-client-protocol*` updates require focused ACP compatibility review.
