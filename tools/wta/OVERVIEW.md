# WTA (Windows Terminal Agent) — Project Overview

## One-line summary

WTA is a Rust command-line tool that **bridges AI agent CLIs and Windows
Terminal**. It lets AI (GitHub Copilot, Claude, Gemini, Codex, or a custom
command) drive your terminal directly — create tabs, split panes, run commands,
read output — and surfaces an in-terminal chat UI inside a Windows Terminal
**agent pane**.

---

## What problem does it solve?

Today's AI coding assistants can "talk about" code but can't "do" anything in the
terminal. WTA fills that gap:

- **AI wants to run a command?** → WTA opens a pane in Windows Terminal and runs it
- **AI wants to read command output?** → WTA pulls the content from the terminal and returns it
- **User wants to drive the terminal via natural-language chat?** → WTA renders a TUI chat surface inside an agent pane; AI executes on your behalf
- **A command just failed?** → WTA's autofix detects it and offers a fix via the agent

---

## Architecture in one sentence

WTA runs as a **helper + master** pair, never as a standalone process: Windows
Terminal spawns one **`wta-master`** singleton that owns the single connection to
the agent CLI, and one **`wta-helper`** per agent pane that renders the TUI and
talks ACP to master over a named pipe. A third, stateless role is the **CLI
helpers** (`wta list-panes`, `wta capture-pane`, …) used for one-shot WT control.

> There is **no standalone agent / TUI mode and no MCP server** anymore. Bare
> `wta` with neither `--master` nor `--connect-master` exits with an error
> (`main.rs`). The earlier single-process "ACP TUI" and "`wta mcp`" modes were
> removed.

---

## Three process roles

### 1. `wta-master` — the ACP multiplexer (singleton)

```
wta --master \\.\pipe\wta-master-<GUID>
```

Spawned **once** by the C++ `SharedWta` singleton (`WindowEmperor` side). It:

1. Spawns the agent CLI subprocess (copilot / claude / gemini / codex) and wraps
   its stdio in an `acp::ClientSideConnection` — master is the *client* of the
   agent CLI.
2. Listens on the named pipe; accepts one `wta-helper` per connect.
3. For each helper, runs an `acp::AgentSideConnection` (master plays the *agent*
   role), forwards helper requests to the agent CLI, and routes inbound
   `session_notification`s back to the owning helper via the `session_to_helper`
   map.

Implementation: `src/master/mod.rs`.

### 2. `wta-helper` — the per-pane TUI

```
wta --connect-master \\.\pipe\wta-master-<GUID> [--owner-tab-id <GUID>] [--start-stashed] …
```

Spawned **once per agent pane** by Windows Terminal (`TerminalPage`). It drives
the ratatui chat UI (`app.rs`) but, instead of spawning its own agent CLI,
connects to master over the pipe and speaks ACP JSON-RPC. *From the helper's
perspective, master is the agent.* The helper owns the user-facing side effects:
the TUI, permission prompts, `ShellManager` (for the agent's `create_terminal`),
autofix, and the per-tab session model.

Entry: `src/helper/mod.rs` → `crate::run_default_tui_over_pipe` (in `main.rs`).

### 3. CLI helpers — one-shot WT control

```
wta list-windows                          # list all WT windows
wta list-tabs                             # list tabs
wta capture-pane -t 3 -l 50               # read the last 50 lines from pane 3
wta new-tab -c "pwsh.exe" -n "Build"      # create a new tab
wta split-pane -h                         # split the current pane horizontally
wta delegate "fix this build"             # open a delegate agent in a new tab
wta sessions list                         # inspect sessions known to master
wta hooks install                         # install the agent-hook bridge
```

Stateless, short-lived commands dispatched in `src/main.rs`. They talk directly
to Windows Terminal via `CliChannel` → `wtcli.exe` → COM and exit. Used by humans
debugging WTA and by agents that can shell out. (The agent CLI reaches WT this
way too — by shelling out to `wta` / `wtcli`, **not** via an MCP server.)

---

## Architecture diagram

```
            Windows Terminal (WindowEmperor, one WT process)
              |  spawns once (SharedWta)      |  spawns per agent pane
              |  --master <pipe>              |  --connect-master <pipe>
              v                               v
        +--------------+   named pipe   +------------------+
        | wta-master   |<-------------->| wta-helper       |  (one per pane)
        | (singleton)  |  ACP/JSON-RPC  | TUI: app.rs +    |
        | master/mod.rs|                | helper/mod.rs    |
        +------+-------+                +--------+---------+
               |  ACP/stdio                      |  ShellManager
               v                                 |  (create_terminal /
         Agent CLI                               |   permission)
      (copilot/claude/                           v
       gemini/codex)                        CliChannel (WtChannel)
                                                 |
 Human / agent shell-out:                        v
   wta <subcommand>  ----------------->  wtcli.exe -> COM IProtocolServer
   (main.rs CLI helpers)                         |
                                                 v
                                    TerminalProtocolComServer
                                                 |
                                                 v
                                         Windows Terminal
```

---

## Core modules

| Module | File | Responsibility |
|------|------|------|
| **Entry / CLI** | `src/main.rs` | clap parsing, role/subcommand dispatch, protocol discovery, locale normalization |
| **Master** | `src/master/mod.rs` | ACP multiplexer singleton: spawns the agent CLI, serves helpers over the pipe, routes per-session notifications |
| **Helper** | `src/helper/mod.rs` | Thin per-pane entry; reuses `run_default_tui_over_pipe` with the pipe as ACP transport |
| **App / TUI** | `src/app.rs` (+ `src/app/*.rs`) | TUI state machine and event loop; per-tab sessions, autofix, permission, session-management view |
| **ACP client** | `src/protocol/acp/client.rs` | Agent-CLI client + helper-side `WtaClient`; prompt templating, model select, probe, failure handling |
| **Coordinator** | `src/coordinator.rs` | `?<prompt>` delegate execution |
| **Session tracking** | `src/agent_sessions.rs`, `src/session_registry.rs`, `src/session_watcher/*` | Session registry + CLI-log status classification (claude/copilot/codex/gemini) |
| **ShellManager** | `src/shell/shell_manager.rs` | Terminal process manager: local child or WT pane |
| **CliChannel** | `src/shell/wt_channel/cli_channel.rs` | Shells out to `wtcli.exe` (the only WT transport) |
| **TUI views** | `src/ui/*.rs` | ratatui rendering: chat, input, permission, popups, agents view, status bar |
| **Hooks installer** | `src/agent_hooks_installer.rs` | Install / upgrade the `wt-agent-hooks` bridge per CLI |

---

## Communication protocols

### WTA ↔ AI Agent (ACP, two hops)

ACP (`agent-client-protocol = "0.10"`, JSON-RPC 2.0) is spoken on two hops:

- **master ↔ agent CLI** (stdio): master is the ACP **client**; it spawns and
  owns the agent CLI.
- **helper ↔ master** (named pipe): master is the ACP **agent** (server), the
  helper is the **client**. Master forwards helper requests to the agent CLI and
  fans notifications back to the owning helper.

### WTA ↔ Windows Terminal (COM)

- **Transport**: every WT operation shells out to `wtcli.exe`, which does
  `CoCreateInstance(WT_COM_CLSID)` and calls WT's `IProtocolServer` — including
  `send_input` (`wtcli send-keys`).
- **Discovery**: the `WT_COM_CLSID` environment variable, set by WT at startup
  and inherited by every conpty child (so `wta` and `wtcli` see it automatically).
- **Authorization**: gated by Windows packaged-COM / terminal activation policy.

---

## Tech stack

| Purpose | Crate |
|------|-----|
| Async runtime | tokio |
| CLI parsing | clap 4 |
| TUI rendering | ratatui 0.30 + crossterm 0.29 |
| ACP protocol | agent-client-protocol 0.10 |
| Serialization | serde + serde_json |
| Error handling | anyhow |
| i18n | rust-i18n |

---

## Build & run

```bash
cd tools/wta

# Kill any live wta.exe first (a running shared-host locks target/debug/wta.exe)
#   PowerShell: Get-Process wta -ErrorAction SilentlyContinue | Stop-Process -Force
cargo build
# Output binary: tools/wta/target/debug/wta.exe

# Run the WTA test suite (cargo build does NOT compile #[cfg(test)] code)
cargo test
```

WTA is normally launched **by Windows Terminal** (master + helper), not run by
hand. For ad-hoc inspection, the CLI helpers work standalone inside a WT pane:

```bash
wta pipe-id            # show the inherited WT_COM_CLSID
wta list-windows       # talk to WT over COM
wta capture-pane -l 5
wta sessions list      # ask master for the session registry
```

---

## Relationship to the Windows Terminal repo

WTA lives under `tools/wta/` of the Windows Terminal (Intelligent Terminal) source
tree. It is an independent Rust project but a **companion** to Windows Terminal:

- The C++ side ships `TerminalProtocolComServer`, exposing `IProtocolServer` via
  local COM activation, and `SharedWta`, which spawns/owns the `wta-master`
  singleton.
- `TerminalPage` spawns one `wta-helper` per agent pane (pre-warmed per tab) and
  hosts its `TermControl` inside `AgentPaneContent`.
- The Rust side reaches WT only indirectly, by shelling out to `wtcli.exe`.

See `doc/specs/Multi-window-agent-pane.md` for the full helper+master design, and
`tools/wta/AGENTS.md` for the per-crate conventions (logging layout, session
liveness model, hooks auto-upgrade, third-party notice generation).

---

## Process model in detail

### Process inventory

| Process | Binary | Lifetime | Role |
|------|-----------|---------|------|
| **Windows Terminal** | `WindowsTerminal.exe` | User-launched, long-lived | Window manager + renderer; hosts `TerminalProtocolComServer`; spawns master + helpers |
| **wta-master** | `wta.exe --master` | Spawned once by `SharedWta` | Owns the agent CLI; multiplexes ACP sessions for all helpers |
| **wta-helper** | `wta.exe --connect-master` | One per agent pane | TUI + per-pane side effects; ACP client of master |
| **Agent CLI** | `copilot`, `claude`, `gemini`, `codex` | Spawned once by master | The AI "brain"; shared across all helpers |
| **wtcli** | `wtcli.exe` | Per call (or long-running for `listen`) | COM client for `IProtocolServer`; bridges wta → WT |
| **Shell commands** | `pwsh`, `cargo`, `git`, … | Spawned by WT; exit when done | The actual tools doing the work |

### Key lifetime points

- One agent CLI is shared by **all** panes/tabs (master multiplexes). A helper's
  `session/new` round-trips to the CLI; `initialize` is a cached replay.
- Helpers are **pre-warmed per tab** at tab creation (`--start-stashed`), so the
  ACP session connects in the background even before the user opens the pane —
  this is what lets autofix work on a stashed pane.
- Toggling an agent pane **stashes** it (helper + conpty + ACP session survive);
  the pane is only destroyed on tab close or `Ctrl+C ×2` in the TUI.
- If master dies, helpers see `TransportLost` and the only recovery is `/restart`
  (routes via `wtcli publish` → C++ `SharedWta::Restart`, bypassing the dead pipe).

### Two paths for shell command execution

When the agent's ACP `create_terminal` lands on a helper, `ShellManager` picks:

```
                 ShellManager.create_terminal(config)
                           │
                 ┌─────────┴─────────┐
                 │ has_wt_channel()? │
                 └─────────┬─────────┘
                  Yes      │      No
               ┌───────────┴───────────┐
               ▼                       ▼
       Path A: WT pane            Path B: local child
       (via wtcli/COM)            (tokio::process::Command)
       visible to the user        invisible, dies with WTA
```

Fallback: if WT pane creation fails, WTA downgrades to the local-child path.

---

## Current status

- Helper+master architecture: ✅ current primary (and only) runtime model
- COM/CLI control plane: ✅ done; sole WT transport
- Autofix, delegate (`?<prompt>`), session-management view, hooks auto-upgrade: ✅ shipped
- MCP server mode, standalone single-process TUI: ❌ removed
