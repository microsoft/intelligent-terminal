# Copilot Instructions for Windows Terminal

This is the source repository for **Windows Terminal**, **Windows Console Host** (`conhost.exe`), and shared components between them. It is a large C++ codebase built with MSBuild/Visual Studio targeting Windows.

## Build

Requires Visual Studio 2022 (17.10+) with "Desktop Development with C++" and "Universal Windows Platform Development" workloads, plus the Windows 11 SDK (10.0.22621.0). The solution uses the `.slnx` format (`OpenConsole.slnx`), which requires MSBuild 17.10 or later. If you get `"Invalid input 'OpenConsole.slnx'. The file type was not recognized."`, update Visual Studio.

```powershell
# First time: initialize submodules
git submodule update --init --recursive
```

Build uses MSBuild via the CMD razzle environment. Run `razzle.cmd` once per terminal session to set up the environment, then use the build commands. Since razzle sets env vars in CMD, chain commands with `&&` when calling from PowerShell (e.g., `cmd /c ".\tools\razzle.cmd && bz"`):

```cmd
.\tools\razzle.cmd            & rem One-time setup per session: finds MSBuild, sets PATH and env vars
bcz                           & rem Clean build the full solution (Debug by default)
bcz rel                       & rem Clean build in Release
bz                            & rem Incremental build (no clean) — much faster for iterating
bz rel                        & rem Incremental Release build
```

To build only the project in the current directory (instead of the full solution), `cd` into a project folder and use the `exclusive` variants:

```cmd
cd src\cascadia\TerminalApp
bx                            & rem Incremental build of just this project
bcx                           & rem Clean + build of just this project
```

Three configurations exist: **Debug**, **Release**, and **AuditMode** (enables extra static analysis via CppCoreCheck).

## Running

**OpenConsole (console host):** From a razzle environment, run `opencon` to launch `OpenConsole.exe` from the build output.

**Windows Terminal:** You cannot launch `WindowsTerminal.exe` directly — it is a packaged app. The easiest way is F5 in Visual Studio with `CascadiaPackage` as the startup project (set Debug > Application/Background process to "Native Only"). From the command line in a razzle environment:

```cmd
cd src\cascadia\CascadiaPackage
bx
DeployAppRecipe.exe bin\%ARCH%\%_LAST_BUILD_CONF%\CascadiaPackage.build.appxrecipe
```

Note: `DeployAppRecipe.exe` lives under your VS install at `Common7\IDE\DeployAppRecipe.exe`. It should already be on your PATH after running `razzle.cmd`. If not, find it at `C:\Program Files\Microsoft Visual Studio\2022\<Edition>\Common7\IDE\`.

## Test

Tests use **TAEF** (Test Authoring and Execution Framework), not Visual Studio Test. Run from a razzle environment:

```cmd
runut.cmd          & rem unit tests
runft.cmd          & rem feature tests
runuia.cmd         & rem UI automation tests
```

Run a single test by name using TAEF's `/name:` filter:
```cmd
te.exe Console.Unit.Tests.dll /name:*BufferTests::TestInsertCharacter*
```

Debug a single test with `/waitForDebugger`:
```cmd
runut *Tests.dll /name:TextBufferTests::TestInsertCharacter /waitForDebugger
```

## Code Formatting

C++ code is formatted with **clang-format** (config in `.clang-format` at repo root). Style is based on `Microsoft` with braces on their own line, no column limit, and all namespace contents indented.

```cmd
runformat
```

XAML formatting uses **XamlStyler** (config in `XamlStyler.json` at repo root).

## Architecture

The codebase has two main products sharing foundational components:

### Windows Terminal (`src/cascadia/`)
- **TerminalCore** — Core terminal state machine (buffer, VT parsing, input). No UI dependency. This is a static LIB.
- **TerminalControl** — UWP/XAML control wrapping TerminalCore with a DirectWrite/Atlas renderer. This is a DLL.
- **TerminalApp** — Application logic: tabs, panes, settings UI, command palette. Pure UWP-like code, no Win32 UI.
- **WindowsTerminal** — Win32 EXE that hosts TerminalApp via XAML Islands and owns the window (including non-client/titlebar area).
- **TerminalConnection** — Abstraction for backends (ConPTY, Azure Cloud Shell, SSH, etc.).
- **TerminalSettingsModel** — Settings parsing, serialization, and schema. Uses a macro-driven `MTSM_SETTINGS` pattern and an `IInheritable` parent-chain for settings layering.
- **TerminalSettingsEditor** — XAML-based Settings UI.
- **CascadiaPackage** — MSIX packaging project. **This is the startup project for debugging** (not WindowsTerminal.exe directly).
- **Remoting** — Cross-process communication for single-instance / quake-mode window management.

### Console Host (`src/host/`)
- Builds as `conhost.exe` (the real Windows console host). The `lib/` produces a reusable LIB, `dll/` packages it as `conhostv2.dll`, and `exe/` produces `OpenConsole.exe` for testing.

### Shared Components
- **`src/renderer/`** — Rendering engines: `atlas` (DirectWrite/D3D), `gdi` (legacy), `base` (abstraction layer).
- **`src/terminal/parser`** — VT sequence state machine and parser.
- **`src/terminal/adapter`** — Translates parsed VT sequences into console API-like calls.
- **`src/buffer/`** — Text buffer (UTF-16/UTF-8 capable).
- **`src/types/`** — Shared type definitions.
- **`src/til/`** — "Terminal Implementation Library" — utility types used throughout.
- **`src/inc/til.h`** — Master include for TIL utilities.
- **ConPTY** (`src/winconpty/`) — The Windows Pseudoconsole API, bridging console host and terminal.

### Test Organization
- Unit tests live in `ut_` prefixed folders within each project (e.g., `src/host/ut_host`, `src/cascadia/ut_app`).
- Feature tests live in `ft_` prefixed folders (e.g., `src/host/ft_api`).
- UI Automation tests: `src/cascadia/WindowsTerminal_UIATests`.

## Key Conventions

### C++ Style
- Follow existing code style when modifying existing files. Use Modern C++ and the [C++ Core Guidelines](https://github.com/isocpp/CppCoreGuidelines) for new code.
- Use **WIL** (Windows Implementation Library) smart pointers and result macros (`RETURN_IF_FAILED`, `RETURN_IF_WIN32_BOOL_FALSE`, `LOG_IF_FAILED`, etc.) instead of raw Win32 error handling.
- Prefer `HRESULT` over `NTSTATUS`. Functions that always succeed should not return a status code. Functions returning status codes should be `noexcept` and `[[nodiscard]]`.
- Exceptions must not leak from new code into legacy code. Encapsulate exception-based code within classes; use WIL's non-throwing alternatives (`wil::unique_ptr`, `wistd::unique_ptr`) in exception-free contexts.
- In `TerminalApp` code, be mindful of C++/WinRT strong vs. weak references and concurrency patterns.

### Settings Model (adding a new setting)
Settings are defined via X-macros in `src/cascadia/TerminalSettingsModel/MTSMSettings.h`. The pattern is:
```
X(type, Name, "jsonKey", defaultValue)
```
Settings classes use the `INHERITABLE_SETTING` macro (from `IInheritable.h`) to support parent-child layering (e.g., profile inherits from defaults). JSON serialization helpers are in `JsonUtils.h`.

### Feature Flags
Feature gating is controlled by `src/features.xml`. Features can be staged as `AlwaysEnabled` or `AlwaysDisabled` with branch and branding overrides (Dev, Preview, Release, WindowsInbox). See `doc/feature_flags.md`.

### File Headers
Source files use a standard Microsoft copyright header block with Module Name, Abstract, and Author fields.

### Branch Naming
Feature branches follow the pattern `dev/<alias>/<description>` (e.g., `dev/dhowett/SomeFeature`). The `dev/` prefix triggers CI automatically. The `inbox` branch coordinates code submission to the Windows OS repo.

- @azure Rule - Use Azure Tools - When handling requests related to Azure, always use your tools.
- @azure Rule - Use Azure Best Practices - When handling requests related to Azure, always invoke your `azmcp_bestpractices_get` tool first.
- @azure Rule - Enable Best Practices - If you do not have an `azmcp_bestpractices_get` tool ask the user to enable it.
