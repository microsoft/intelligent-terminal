# Intelligent Terminal (Windows Terminal Fork)

AI-native Windows Terminal — agents (Copilot, Claude, Gemini, custom) can understand, fix, and automate terminal workflows.

## Core Components

- **WTA** (Windows Terminal Agent) — orchestrator binary. Launches agents, passes Terminal Protocol connection info. Agents control WT via `wtcli`.
  - Launch: `wta delegate --agent <agent> --delegate-agent <delegate> --cwd <cwd> "<prompt>"`
- **WT Protocol** (`IProtocolServer`) — sole integration surface. WinRT IDL + COM out-of-process server (MBM marshaling, MTA thread). Discovery via `WT_COM_CLSID` env var.
  - IDL: `src/cascadia/TerminalProtocol/TerminalProtocol.idl`
  - Server: `src/cascadia/WindowsTerminal/TerminalProtocolComServer.cpp`
- **WTCLI** — CLI client consuming `IProtocolServer` via `CoCreateInstance(CLSCTX_LOCAL_SERVER)`. Agents shell out to `wtcli list-panes`, `wtcli capture-pane`, etc.
- **ACP** (Agent Control Protocol) — JSON-RPC 2.0 over stdio for in-pane agent experience (`AcpConnection.cpp`).

## UX

| Trigger | Behavior |
|---------|----------|
| `>Toggle AI assistant` | Opens/toggles agent pane (`openAgentPane` action) |
| `?<prompt>` | Delegates to hidden background WTA process |
| `?` (empty) | No-op |
| `&` | Background task mode (future, C9) |

Agent pane: position configurable (`bottom`/`right`/`top`/`left`). Color-coded VT output.

## Settings (`settings.json`)

```jsonc
{
    "acpAgent": "copilot",           // "copilot", "gemini", or "custom:<cmd>"
    "acpModel": "",                  // Model override
    "acpCustomCommand": "",          // Command for custom agent
    "agentPanePosition": "bottom",
    "delegateAgent": "copilot",      // Agent for ?<prompt> delegation
    "delegateModel": "",
    "delegateCustomCommand": "",
    "autoFixEnabled": true,
    "aiIntegration.coordinator.enabled": false,
    "aiIntegration.coordinator.commandline": "wta",
    "aiIntegration.coordinator.profile": "{fd19208a-412b-4857-8a2d-9ca592b4b16e}",
    "aiIntegration.confirmation.readOperations": "auto",
    "aiIntegration.confirmation.createOperations": "auto",
    "aiIntegration.confirmation.inputOperations": "auto",
}
```

## Architecture

```
WindowEmperor
  |-- TerminalProtocolComServer (COM, MTA thread, WT_COM_CLSID)
  +-- AppHost[] → TerminalWindow → TerminalPage
        |-- CommandPalette (? / & prefixes)
        |-- Agent panes (AcpConnection)
        +-- Protocol bridge (TerminalPage.Protocol.cpp)

External: Agent → wtcli → COM (IProtocolServer) → TerminalProtocolComServer → WindowEmperor
```

## Key Files

| Area | Path |
|------|------|
| Agent integration | `src/cascadia/TerminalApp/TerminalPage.cpp`, `TerminalPage.Protocol.cpp` |
| Command Palette | `src/cascadia/TerminalApp/CommandPalette.cpp` |
| Protocol IDL | `src/cascadia/TerminalProtocol/TerminalProtocol.idl` |
| COM Server | `src/cascadia/WindowsTerminal/TerminalProtocolComServer.cpp` |
| ACP Connection | `src/cascadia/TerminalConnection/AcpConnection.cpp` |
| Settings | `src/cascadia/TerminalSettingsModel/GlobalAppSettings.idl`, `MTSMSettings.h` |
| Settings UI | `src/cascadia/TerminalSettingsEditor/AIAgents.xaml` |
| Process coord | `src/cascadia/WindowsTerminal/WindowEmperor.cpp` |

## Autofix

Detects command failures in other panes and auto-suggests fixes via the agent.

**Pipeline**: Shell emits `OSC 133;D;<exit_code>` → `TerminalPage` raises `ProtocolVtSequenceReceived` → COM server forwards to clients → WTA (via `wtcli listen --json`) classifies → `maybe_trigger_autofix()`.

**Requirements**: PowerShell shell integration (OSC 133 marks), agent pane open, `wtcli` on PATH.

**Key code**: `tools/wta/src/app.rs` (`classify_wt_event`, `maybe_trigger_autofix`), `TerminalPage.cpp:2650-2740` (event handlers), `TerminalProtocolComServer.cpp` (`_ensurePageEventsRegistered`).

**Diag log**: `wta-ensure-host.log` in the WTA log directory — shows event flow, classification, and autofix triggers.

## Logs

WTA writes structured logs to the package-sandboxed LOCALAPPDATA:

```
%LOCALAPPDATA%\IntelligentTerminal\logs\
  wta-ensure-host.log   — background host startup / COM connection
  wta-attach.log        — agent pane TUI (attach mode)
  wta-agent-pane.log    — agent pane session
  wta-acp-debug.log     — ACP protocol debug trace
  wta-delegate.log      — ?<prompt> delegation flow
```

When running packaged (F5 / installed), `%LOCALAPPDATA%` is redirected to the
package sandbox:
```
C:\Users\<user>\AppData\Local\Packages\IntelligentTerminal_<id>\LocalCache\Local\IntelligentTerminal\logs\
```

Log level is controlled by the `WTA_LOG` env var (default: `info`). Set
`WTA_LOG=debug` for verbose output.

## Build

There are two independent build systems. **Both must be built** before F5.

### 1. WTA (Rust) — build first

```bash
# Kill stale WTA processes first
taskkill //f //im wta.exe 2>/dev/null; true

cargo build --target x86_64-pc-windows-msvc --manifest-path tools/wta/Cargo.toml
# Output: tools/wta/target/x86_64-pc-windows-msvc/debug/wta.exe
#
# Always pass --target explicitly — the wapproj prefers
# tools/wta/target/<triple>/<profile>/wta.exe over the bare target/<profile>
# fallback, and a stale explicit-target binary will silently shadow your
# fresh bare-target build.
```

### 2. Terminal (C++ / MSBuild)

**Command line (incremental):**
```bash
cmd.exe //c "tools\razzle.cmd && bcz no_clean"
# Release: bcz rel no_clean
# Output: bin/x64/Debug/
```

**Visual Studio F5 (debug):**
- Set `CascadiaPackage` as startup project → F5
- MSBuild copies `wta.exe` from Cargo output into the package layout
  (via Content items in `CascadiaPackage.wapproj`)
- The deployed `wta.exe` sits next to `WindowsTerminal.exe` in the
  package directory, inheriting package identity for COM access

### Full rebuild flow (typical dev cycle)

```bash
# 1. Build WTA (always use --target — see note above)
taskkill //f //im wta.exe 2>/dev/null; true
cargo build --target x86_64-pc-windows-msvc --manifest-path tools/wta/Cargo.toml

# 2. Build & run Terminal from VS
#    F5 in Visual Studio (CascadiaPackage project)
#    — or from command line:
cmd.exe //c "tools\razzle.cmd && bcz no_clean"
```

### Package identity & COM

The COM server (`TerminalProtocolComServer`) is registered under the
Terminal's package identity. `wtcli.exe` and `wta.exe` must also have
package identity to activate it via `CoCreateInstance`. This is why:

- `wta.exe` is deployed **inside the package** (next to `WindowsTerminal.exe`)
- `_DetectWtaPath()` prefers the co-located `wta.exe` over dev-build paths
- Running `wta.exe` from `tools/wta/target/debug/` directly will fail with
  `0x80073D54` (APPMODEL_ERROR_NO_PACKAGE) when calling COM methods

If autofix or the agent pane stops working after a debug launch, check
`%TEMP%\wta-ensure-host.log` for the `0x80073D54` error — it means
the wrong (unpackaged) `wta.exe` was used.

## Installer

See **[doc/building-installer.md](doc/building-installer.md)** for full details.

Two distribution formats:

| Format | Script | Output |
|--------|--------|--------|
| **MSIX ZIP** (packaged) | Manual assembly from MSBuild output | `artifacts/local-installer/*-msix.zip` |
| **Self-extracting EXE** (unpackaged) | `build/scripts/New-WtaLocalInstaller.ps1` | `artifacts/local-installer/*-setup.exe` |
