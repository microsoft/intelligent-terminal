# Quick start for local development


The fast path for local development. Intelligent Terminal is a dual-stack project: the Rust
**WTA** agent (`tools/wta/`) plus the C++ **Windows Terminal** app (`src/`). For command-line
builds, CI, packaging, and troubleshooting, see [building.md](./building.md).

## 1. First-time setup

**1.1. Install:**

- **Visual Studio 2026 (18.x)** with the **Desktop development with C++** and **Universal Windows
  Platform development** workloads.
- **Rust** via [rustup](https://rustup.rs/) (standard rustup; the repo's toolchain pin falls back
  to stable).

Then open `OpenConsole.slnx` in Visual Studio and click **Install** on the "extra components"
prompt. It reads `.vsconfig` and adds what the build needs, including **C++ Universal Windows
Platform tools (Latest MSVC)** (required for `WindowsTerminal` to load; a separate item from the
UWP workload). NuGet and vcpkg dependencies restore automatically during the build, so that is all
the setup needed.

**1.2. Build and run** (two build systems, in order):

1. Build the Rust agent:
   `cargo build --target <the target triple> --manifest-path <the toml file>`

   For instance,
   ```powershell
   cargo build --target x86_64-pc-windows-msvc --manifest-path tools/wta/Cargo.toml
   ```
2. In Visual Studio:
   - Set startup project: **`CascadiaPackage`**
   - Select platform, **x64** for instance.
   - Go to `CascadiaPackage` > Properties > Debug: set **Application process** and **Background task
     process** to **Native Only**
   - Run (**F5**)

F5 builds the app, deploys, and launches Windows Terminal (Dev) with the debugger attached. The
first build is slow; later ones are incremental.

## 2. After changing code

| Changed | Do this |
|---------|---------|
| **Rust** (`tools/wta/`) | Rebuild via `cargo build`. For instance,<br>`cargo build --target x86_64-pc-windows-msvc --manifest-path tools/wta/Cargo.toml` |
| **C++** (`src/`) | Press **F5** in Visual Studio |

`cargo build` is incremental (seconds for a small change). To see a WTA change inside the running
Terminal (agent pane, autofix), press **F5** afterward so the new `wta.exe` is copied in.

> If a rebuild reports `wta.exe` in use, stop the running instance first: close the Dev Terminal,
> or run `taskkill /f /im wta.exe`.

## 3. Running tests

| Side | Command |
|------|---------|
| **Rust** | `cargo test --manifest-path tools/wta/Cargo.toml` |
| **C++** (TAEF) | `runut.cmd` (unit), `runft.cmd` (feature), `runuia.cmd` (UIA), from a dev environment |

Run one C++ test with `te.exe <Tests.dll> /name:<pattern>`. See [building.md](./building.md) for
the dev environment and [TAEF.md](./TAEF.md) for details.
