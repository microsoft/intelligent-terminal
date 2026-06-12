---
description: 'WTA (Windows Terminal Agent) Rust crate conventions — overrides and complements the generic rust.instructions.md'
applyTo: 'tools/wta/**/*.rs'
---

# WTA Rust Conventions

Project-specific Rust guidance for the WTA crate (`tools/wta/`). Applies **in addition to** `rust.instructions.md`; where the two conflict, this file wins.

The repo contains multiple Rust crates (e.g. `installer/bootstrap/`); this file's conventions apply only to the WTA crate under `tools/wta/`. Other Rust code in the repo is governed by the generic `rust.instructions.md` only.

## Toolchain & Build

- **Toolchain is pinned.** `tools/wta/rust-toolchain.toml` pins the channel to `ms-prod-1.93` for CI reproducibility. Do not bump it casually.
- **Static CRT on Windows.** The repo-root `.cargo/config.toml` forces `+crt-static` rustflags for all Windows MSVC targets (`x86_64`, `i686`, `aarch64`). Avoid dependencies that break under static CRT.
- **Two supported build invocations — don't mix them.** Both of these are valid for WTA local dev:

  ```bash
  cargo build --manifest-path tools/wta/Cargo.toml                              # host target (bare target/)
  cargo build --target x86_64-pc-windows-msvc --manifest-path tools/wta/Cargo.toml   # explicit target
  ```

  Pick one and stay with it within a single dev session. The `CascadiaPackage.wapproj` deploy step prefers `tools/wta/target/x86_64-pc-windows-msvc/<profile>/wta.exe` over the bare `tools/wta/target/<profile>/wta.exe`, so if you build once with `--target` and later iterate with plain `cargo build`, the wapproj will silently keep deploying the stale explicit-target binary. See `tools/wta/README.md` and `tools/wta/AGENTS.md` for the host-target workflow.

## Localization

User-facing strings go through `t!(...)` (rust-i18n) — see `rust-localization.instructions.md` for the full pattern (locale YAML structure, `{Locked}` markers, fallback chain).

## Logging

- Use `tracing` with structured `target` + key=value fields.
- Initialize **once** in `main()` via `logging::init(&process_label(&cli))` **before** any work — even arg-parsing failures should land on disk.
- Call `logging::shutdown_flush()` on every exit path, **including before `std::process::exit`** (which otherwise skips the `WorkerGuard` drop and loses buffered records).
- Default level: debug builds → `debug`, release → `info` (`logging::default_filter_directive`).
- Override with `WTA_LOG` or `RUST_LOG` env vars.

## Runtime Paths

- Resolve all runtime data paths through `runtime_paths.rs`:
  - `intelligent_terminal_root()` for state (`LocalState`)
  - `logging::log_dir()` for logs (`LocalCache\Local`)
- **Do not** hand-roll `%LOCALAPPDATA%\IntelligentTerminal` paths — the helper has to handle package-private (`Packages\<pfn>\LocalState`) vs unpackaged (bare `%LOCALAPPDATA%`) identity correctly.
- All Rust log writers, the C++ `AgentPaneLog.h`, and PowerShell hooks share the same per-version dir `logs\<pkgver>\` so the bug-report zip captures everything.

## Third-Party Dependency Notices

`tools/wta/cgmanifest.json` (Component Governance manifest) and the `<!-- BEGIN wta-rust-deps -->` block in `/NOTICE.md` are **generated** from `cargo metadata`. Whenever you change the dependency graph — add/remove/upgrade a direct dep in `tools/wta/Cargo.toml`, run a `cargo update` that substantially shifts `Cargo.lock`, or flip a feature flag that pulls in/drops transitive crates — regenerate both and commit the diff alongside the Cargo change:

```powershell
$env:RUSTUP_TOOLCHAIN = 'stable'   # bypass the rust-toolchain.toml pin
pwsh -File .\build\scripts\Generate-WtaThirdPartyNotices.ps1
```

Requires **PowerShell 7+** (`pwsh.exe`); fails fast under Windows PowerShell 5.1. Inspect the diff to both files and include them in the same commit. See `tools/wta/AGENTS.md` → "Third-party Rust crate attribution" for the full pipeline.

## WTA-Specific Quality Checklist

In addition to the generic checklist in `rust.instructions.md`:

- [ ] **Logging**: New entry points call `logging::init` and ensure `shutdown_flush` runs before any `std::process::exit`
- [ ] **Paths**: Runtime data paths go through `runtime_paths.rs` (no hand-rolled `%LOCALAPPDATA%`)
- [ ] **Dep changes**: If `tools/wta/Cargo.toml` direct deps or `Cargo.lock` shift meaningfully, regenerate `cgmanifest.json` + `NOTICE.md` via `Generate-WtaThirdPartyNotices.ps1` and commit together
- [ ] **Static CRT**: New deps build cleanly under `+crt-static` for `x86_64-pc-windows-msvc`
- [ ] **Toolchain**: No changes to `rust-toolchain.toml` channel unless explicitly intended
