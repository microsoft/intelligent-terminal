# Intelligent Terminal (Windows Terminal Fork)

AI-native Windows Terminal — agents (Copilot, Claude, Gemini, custom) can understand, fix, and automate terminal workflows.

## Core Components

- **WTA** (Windows Terminal Agent) — orchestrator binary. Launches agents, passes Terminal Protocol connection info. Agents control WT via `wtcli`.
  - Launch: `wta delegate --agent <agent> --delegate-agent <delegate> --cwd <cwd> "<prompt>"`
- **WT Protocol** (`IProtocolServer`) — sole integration surface. WinRT IDL + COM out-of-process server (MBM marshaling, MTA thread). Discovery via `WT_COM_CLSID` env var.
  - IDL: `src/cascadia/TerminalProtocol/TerminalProtocol.idl`
  - Server: `src/cascadia/WindowsTerminal/TerminalProtocolComServer.cpp`
- **WTCLI** — CLI client consuming `IProtocolServer` via `CoCreateInstance(CLSCTX_LOCAL_SERVER)`. Agents shell out to `wtcli list-panes`, `wtcli capture-pane`, etc.
- **ACP** (Agent Control Protocol) — JSON-RPC 2.0 spoken inside the helper+master architecture. `wta-helper` ↔ `wta-master` over a named pipe; `wta-master` ↔ agent CLI subprocess over stdio. The C++ side no longer participates in ACP directly — agent panes are plain `ConptyConnection`s hosting a `wta-helper` child. See `doc/specs/Multi-window-agent-pane.md`.

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
WindowEmperor (one WT process, N AppHosts/windows)
  |-- TerminalProtocolComServer (COM, MTA thread, WT_COM_CLSID)
  |-- SharedWta (singleton) -- spawns --> wta-master ──► agent CLI (ACP/stdio)
  |                                          ▲
  |                                          │ ACP/JSON-RPC over named pipe
  +-- AppHost[] → TerminalWindow → TerminalPage
        |-- CommandPalette (? / & prefixes)
        |-- Per-tab agent pane: ConptyConnection ───► wta-helper (conpty child)
        |                                            (one helper per tab, pre-warmed)
        +-- Protocol bridge (TerminalPage.Protocol.cpp)

External: Agent → wtcli → COM (IProtocolServer) → TerminalProtocolComServer → WindowEmperor
```

**Per-tab + per-window routing.** Each agent pane has its own helper bound
to an `owner_tab_id` (= WT tab StableId) and a `window_id`. All inbound
events that mutate per-tab state (`set_agent_state`, `tab_changed`,
`tab_closed`, `tab_renamed`) carry both ids; helpers filter by `window_id`
and (for `tab_changed`) by owner-lock in `switch_tab_session`. Outbound
helper events (`agent_state_changed`, `agent_status`, `autofix_state`,
`close_agent_pane`) carry `tab_id` so C++ can route via
`_FindTabByStableId` instead of fanning out across every pane / window.
See `doc/specs/Multi-window-agent-pane.md` §7.

**Helper is pre-warmed per tab.** Every new tab spawns a stashed agent
pane on creation (`_InitializeTab` → `_AutoCreateHiddenAgentPaneShared`
with `autoStash=true`, `--start-stashed`), so the helper is running and
its ACP session connects in the background from the start — even if the
user never opens the pane. This is what lets autofix work on a tab the
user hasn't interacted with. The agent CLI itself is spawned once by
`wta-master` at startup and shared across all helpers (each helper's
`initialize` is a cached replay; only `session/new` round-trips to the
CLI). `--start-stashed` only seeds `pane_open=false`; it does not defer
the handshake. The pre-warm is skipped when wta is unavailable, GPO
blocks all agents, or the tab arrived with an agent pane via cross-window
drag-in (`agentLeavesSeen > 0`). See `TabManagement.cpp:366`.

**Agent pane toggle = stash, not destroy.** `Ctrl+Shift+.` /
`Ctrl+Shift+/` / the bottom-bar button toggle via
`Tab::StashAgentPane`/`RestoreStashedAgentPane` (built on WT's
`Pane::HidePane`/`RestorePane`). Helper + conpty + ACP session + chat
history all survive the toggle. The pane is only destroyed on tab close
or `Ctrl+C×2` in the TUI. See spec §8.

## Key Files

| Area | Path |
|------|------|
| Agent integration | `src/cascadia/TerminalApp/TerminalPage.cpp`, `TerminalPage.Protocol.cpp` |
| Agent pane wrapper | `src/cascadia/TerminalApp/AgentPaneContent.cpp` (XAML chrome around the helper's `TermControl`) |
| Tab-side stash | `src/cascadia/TerminalApp/Tab.cpp` (`StashAgentPane`, `RestoreStashedAgentPane`, `HasStashedAgentPane`) |
| Command Palette | `src/cascadia/TerminalApp/CommandPalette.cpp` |
| Protocol IDL | `src/cascadia/TerminalProtocol/TerminalProtocol.idl` |
| COM Server | `src/cascadia/WindowsTerminal/TerminalProtocolComServer.cpp` |
| Shared master spawn | `src/cascadia/TerminalApp/SharedWta.cpp` |
| wta-master | `tools/wta/src/master/mod.rs` |
| wta-helper / App | `tools/wta/src/app.rs`, `tools/wta/src/main.rs` |
| Settings | `src/cascadia/TerminalSettingsModel/GlobalAppSettings.idl`, `MTSMSettings.h` |
| Settings UI | `src/cascadia/TerminalSettingsEditor/AIAgents.xaml` |
| Process coord | `src/cascadia/WindowsTerminal/WindowEmperor.cpp` |

## Autofix

Detects command failures in other panes and auto-suggests fixes via the agent.

**Pipeline**: Shell emits `OSC 133;D;<exit_code>` → `TerminalPage` raises `ProtocolVtSequenceReceived` → COM server forwards to clients → WTA (via `wtcli listen --json`) classifies → `maybe_trigger_autofix()`.

**Requirements**: PowerShell shell integration (OSC 133 marks), a helper
whose ACP session has reached `Connected`, `wtcli` on PATH. The pane does
**not** need to be visible — the per-tab pre-warmed helper (see
Architecture) makes autofix work on a stashed pane. But a failure that
lands before the helper's session connects (cold start of master/agent
CLI, in-flight `session/new`, or a `Failed` agent) is **dropped**:
`trigger_autofix_inner` early-returns when `state != Connected`
(`app.rs:6820`). The bottom-bar notification banner still shows; only the
autofix pill / LLM call is skipped, and the failure is not re-triggered
once the session later connects.

**Key code**: `tools/wta/src/app.rs` (`classify_wt_event`, `maybe_trigger_autofix`), `TerminalPage.cpp:2650-2740` (event handlers), `TerminalProtocolComServer.cpp` (`_ensurePageEventsRegistered`).

**Diag log**: `wta-ensure-host.log` in the WTA log directory — shows event flow, classification, and autofix triggers.

## Hooks plugin auto-upgrade

When IT is installed or upgraded, the bundled `wt-agent-hooks` plugin
(`tools/wta/wt-agent-hooks/{copilot,claude,gemini-extension}/`) needs to
re-land into any agent CLI the user already opted into (via Settings UI /
FRE "Install hooks" or `wta hooks install`). This is handled silently by
`agent_hooks_installer::upgrade_installed_hooks`, fired once per
`wta-master` startup on a blocking-pool thread.

**Trigger model — bundle version is the upgrade signal.** A tiny state
file `<LocalCache>/IntelligentTerminal/hooks-upgrade-state.json` records
the bundle version this wta process last saw per CLI. At startup we read
each CLI's bundle `plugin.json` / `gemini-extension.json` (cheap, <5ms)
and compare; if all match, we return immediately (no spawns, no IO
beyond the cache compare). Only after the user installs / upgrades IT
does the bundle version change → cache miss → per-CLI flow runs once,
then the state file is rewritten and the fast path resumes.

**Opt-in only.** Even on cache miss, CLIs that don't already have
`wt-agent-hooks` installed are skipped. The auto-upgrade never installs
into a CLI the user hasn't accepted. Disabled plugins are also skipped
(`enabled: false` in Copilot's `config.json` / `claude plugin list`).

**Per-CLI strategy.** Copilot and Claude use their `plugin update`
subcommands; before invoking them we rewrite any stale marketplace
`source.path` to the current bundle dir (Copilot: existing
`cleanup_stale_copilot_marketplace`; Claude: new
`cleanup_stale_claude_marketplace`). Gemini's `extensions update`
silently returns `NOT_UPDATABLE` when the recorded install source no
longer exists (typical after an MSIX version-dir bump), so we peek at
`~/.gemini/extensions/wt-agent-hooks/.gemini-extension-install.json`
first: if `type==local` AND `source` is still under the current bundle,
run `extensions update` in place; otherwise fall back to
uninstall+install while preserving the `isActive` flag.

**Trigger-point caveat.** The agent CLI master spawns concurrently may
already be past its plugin-load step by the time `plugin update` writes
the new files — so the freshly upgraded hooks may not take effect until
the next agent restart. Acceptable because blocking master startup on a
Node-based `plugin update` (1-30s) would hurt every IT-upgrade boot.

**Diag**: `wta-install-hooks.log` (existing) plus `target=agent_hooks`
+ `target={copilot,gemini}_hooks` trace events in
`wta-main_master.log` show every per-CLI decision (`upgrade decision`
log line carries `installed_version`, `bundle_version`, `action`).

## Logs & runtime data layout

WTA runtime data lives under the **package-private** store, split by lifetime
into two roots (both resolved in `runtime_paths.rs`, both falling back to the
same bare path when the process has no package identity):

```
# Packaged (every production wta process — helper is a conpty child of the
# packaged WindowsTerminal.exe, master is spawned in-package by SharedWta):

  …\Packages\<PackageFamilyName>\LocalState\IntelligentTerminal\   <- STATE root
      prompts\                      (prompt overrides)             intelligent_terminal_root()
      agent-pane-sessions.jsonl     (session origin index)
      master-pipe.txt               (helper↔master rendezvous)

  …\Packages\<PackageFamilyName>\LocalCache\Local\IntelligentTerminal\  <- LOCAL/cache root
      logs\<pkgver>\                (ALL logs for that build — Rust wta-*.log,
                                     C++ terminal-agent-pane.log, PS hook-trace.log)
      hook-bundle-staging\ …        (hook-installer staging)
      hooks-upgrade-state.json      (per-CLI bundle version cache for the
                                     hooks auto-upgrade fast-path)

# Unpackaged (dev builds run straight out of the Cargo target dir, tests):
# BOTH roots collapse to the legacy bare %LOCALAPPDATA%\IntelligentTerminal\.
```

Rationale for the split: **State** = persistent, must-survive, package-private
data → `LocalState` (alongside the WT app's own `settings.json` / `state.json`).
**Local/cache** = transient, regenerable diagnostics → `LocalCache\Local`, the
cache store that doesn't roam / back up.

Both roots are package-private — removed on uninstall and isolated between the
dev-sideload family (`IntelligentTerminal_rd9vj3e6a2mbr`) and the store family
(`Microsoft.IntelligentTerminal_8wekyb3d8bbwe`) — instead of sharing one bare
`%LOCALAPPDATA%\IntelligentTerminal` directory. The family name comes from
`GetCurrentPackageFamilyName` (windows-sys); the `Packages\<pfn>\LocalState` and
`…\LocalCache\Local` paths are what WinRT `ApplicationData.Current.LocalFolder`
/ `LocalCacheFolder` resolve to, so we construct them directly rather than
pulling in the WinRT projection.

**All three writers share one per-version dir** `logs\<pkgver>\`, where
`<pkgver>` is the **package version** (`GetCurrentPackageId`, e.g. `0.8.0.2`) —
read identically at runtime by Rust (`logging::package_version`) and C++
(`IntelligentTerminal::PackageVersionDir`), so no build-time version sync is
needed:
- Rust wta processes → `logging::log_dir()` (`logs\<pkgver>\wta-*.log`).
- C++ `AgentPaneLog.h` → `IntelligentTerminal::LogDirVersioned()` →
  `terminal-agent-pane.log` (renamed from the old `wta-agent-pane.log`).
- PowerShell hooks (`send-event.ps1`) → `hook-trace.log`, via the
  `WTA_HOOK_LOG_DIR` env var set to `LogDirVersioned()` (C++ ConptyConnection
  for shell panes; `spawn.rs` for agent-pane CLIs).

`IntelligentTerminal::LogDir()` stays the **root** (`…\logs`, no version) and is
used only by the bug-report-zip action so it archives every version at once.
Unpackaged (dev-from-cargo / tests) has no package identity → all writers fall
back to the flat bare `…\logs\`.

> Earlier builds wrote everything to the bare `%LOCALAPPDATA%\IntelligentTerminal`
> regardless of identity (the `LOCALAPPDATA` env var is **not** redirected into
> the sandbox on Win10/11). There is no migration — old data is left in place
> and simply ignored.

**Log level** is controlled by the `WTA_LOG` (or `RUST_LOG`) env var. When
unset, the default comes from the build: **debug builds default to `debug`,
release builds default to `info`** (`logging::default_filter_directive`). Set
`WTA_LOG=debug|trace` for the noisy traces, or `WTA_LOG=warn` to quiet a
release build further.

**Logging is initialized once** in `main()` immediately after arg parsing
(`logging::init(&process_label(&cli))`), before locale/ETW setup, so even
early-startup failures land on disk. The non-blocking appender's `WorkerGuard`
lives in a global and is flushed via `logging::shutdown_flush()` on every exit
path — including before each `std::process::exit` (which would otherwise skip
the guard drop and lose buffered records). Every launch mode — including
short-lived `wtcli`-style commands — now writes a log file (previously only 6
entry points did).

**Per-version storage + retention** (`logging::housekeeping`): each build's
logs live in their own subdir, `logs\<pkgver>\` (the package version — see
above). On every start, `prune_old_version_dirs` keeps **only the current
version's dir** and deletes all other version dirs wholesale. The current
version's dir is never a deletion target, so cleanup is **lock-free and
concurrency-safe** (no process can delete a file another is writing). Within the
current version's dir, per-PID helper logs older than **3 days** are pruned and
`wta-cli.log` rotates daily keeping 3 days (`max_log_files`).

### Log files in the helper+master architecture

```
wta-main_master.log        — wta-master process: agent CLI spawn, named pipe accept
                              loop, per-helper routing, session_to_helper map updates,
                              agent CLI exit detection, connection failures
wta-main_helper-{pid}.log  — each wta-helper process (one file per PID, so concurrent
                              per-tab helpers don't interleave): pipe connect, ACP
                              initialize, session/new, prompts, agent responses,
                              TUI lifecycle, connection failures
wta-cli.log                — short-lived wtcli-style commands (list-*, capture-pane,
                              listen, sessions, …); daily-rotated, 3-day retention
wta-delegate.log           — `?<prompt>` delegation flow (separate from agent pane)
wta-probe.log              — `probe-models` ACP model-list probe
wta-install-hooks.log      — `hooks install` agent-hook bridge installation
wta-ensure-host.log        — WT-side background ensure-running diagnostics (kept from
                              M3-M6 era; remains useful for SharedWta lifecycle)
wta-acp-debug.log          — low-level ACP JSON-RPC wire trace
```

Two files in the per-version dir are **not** written by the Rust wta binary —
`hook-trace.log` (PowerShell hooks) and `terminal-agent-pane.log` (C++ side);
see **All three writers share one per-version dir** above. They live in the
same `logs\<pkgver>\` and so are cleaned together with the Rust logs when that
version's dir ages out.

### Tracking flows by `target` field

All tracing uses structured `target` + key=value fields. Grep patterns for common
scenarios:

| Goal | Grep |
|---|---|
| Master process lifecycle | `target=master` (in `wta-main_master.log`) |
| Who's connected to master right now | `live_helpers=` in `wta-main_master.log` (climbs on connect, drops on disconnect) |
| Which helper owns a SessionId | `step="helper→agent" op="new_session" session_id=…` |
| Trace one prompt end-to-end | grep `session_id="X"`, look for `step="helper→agent" op="prompt"` (sent) then `step="master→helper" op="session_notification"` (response chunks) |
| Helper pipe lifecycle | `target=master helper_id=…` shows connect+exit |
| Agent CLI failures | `target=agent_stderr` |
| Connection failures (either side) | `"exiting with error"` — `target=master` in `wta-main_master.log`, `target=helper` in `wta-main_helper-{pid}.log`; plus inline `step="acp_initialize"` / `step="pipe_connect"` for the helper handshake |
| Internal control routing | `target=internal_control` (legacy; mostly empty post-Z) |

### Example: end-to-end trace of one user prompt

```
[helper] target=acp_client                — pipe connected to master
[helper] target=acp_client                — ACP initialize sent
[helper] target=acp_client                — session/new → session_id=abc-123
[master] step=helper→agent op=new_session — registered abc-123 → helper_id=2
[helper]                                  — user pressed Enter, sending prompt
[master] step=helper→agent op=prompt      — forwarding to agent CLI (sid=abc-123)
[master] step=agent→helper kind=agent_message_chunk — agent CLI streamed first chunk
[master] step=master→helper               — wrote chunk back to helper_id=2 pipe
[helper]                                  — chunk applied to TabSession.messages
[master] step=helper→agent op=prompt elapsed_ms=842 stop_reason=…  — turn ended
```

If any step is missing, the failure is at the previous step.

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
