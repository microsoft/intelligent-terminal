---
description: 'File-scoped conventions for the WTA Rust crate'
applyTo: 'tools/wta/**/*.rs'
---

# WTA Rust conventions

These rules complement `rust.instructions.md`. Architecture, build commands,
and product behavior are defined in `AGENTS.md` and `tools/wta/AGENTS.md`.

## Toolchain and dependencies

- Keep `tools/wta/rust-toolchain.toml` pinned unless a toolchain update is the
  explicit task.
- New dependencies must build with the repo's static-CRT Windows MSVC
  configuration.
- Use the explicit Windows target from the repo-level build instructions; do
  not mix host-target and explicit-target outputs.
- When the shipped dependency graph changes, run
  `build/scripts/Generate-WtaThirdPartyNotices.ps1` with PowerShell 7 and commit
  both `tools/wta/cgmanifest.json` and `/NOTICE.md`.

## Runtime behavior

- Resolve state through `runtime_paths::intelligent_terminal_root()` and logs
  through `logging::log_dir()`. Never construct LocalAppData/package paths by
  hand.
- Use structured `tracing` targets and fields. Never log secrets, provider
  credentials, or session MCP bearer capabilities.
- Initialize logging once before substantive startup work and call
  `logging::shutdown_flush()` before every `std::process::exit`.
- Localize user-facing strings through `t!(...)`; follow
  `rust-localization.instructions.md` for locale-file changes.
- Preserve tab, window, helper, and ACP session identity across asynchronous
  routing. Do not replace typed identities with loosely related strings.

## Testing

- Cover reducers and routing decisions with deterministic unit tests.
- Use mock ACP and render harnesses for protocol/UI behavior instead of timing
  sleeps or live services.
- Run the explicit-target WTA test command from the repo-level `AGENTS.md`
  before committing or pushing behavior changes.
