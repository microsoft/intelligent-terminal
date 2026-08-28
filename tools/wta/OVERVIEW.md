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
talks ACP to master over a named pipe. Stateless **CLI helpers** provide one-shot
WT control, and the master-owned **session MCP endpoint** accepts typed terminal
action and user-input requests with per-session capabilities.

> There is no standalone agent / TUI mode. Bare `wta` with neither a role flag
> nor a subcommand exits with an error (`main.rs`). The session MCP endpoint is
> not a general WT-control server and exposes no read or execution tools.

---

## Process roles and MCP endpoint

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
wta --connect-master \\.\pipe\wta-master-<GUID> [--owner-tab-id <GUID>] [--owner-window-id <ID>] [--start-stashed] …
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
wta resolve-command which --cwd . --json  # resolve from cwd + PATH + shell-specific sources
```

Stateless, short-lived commands dispatched in `src/main.rs`. They talk directly
to Windows Terminal via `CliChannel` → `wtcli.exe` → COM and exit, except local
helpers such as `resolve-command`, which inspect cwd and machine state directly. Used by
humans debugging WTA and by agents that can shell out. (The agent CLI reaches WT
this way too — by shelling out to `wta` / `wtcli`. The session MCP endpoint
is separate and cannot perform these operations.)

Packaged builds register `wta.exe` as an App Execution Alias. WTA prepends the
current package family's alias directory to the agent process `PATH`, so a short
`wta.exe` invocation selects the matching Dev, Preview, or Store installation
even when multiple variants are installed. Unpackaged builds prepend the
running executable's directory instead.

### 4. Session MCP endpoint — master-owned

`wta-master` owns one stateless Streamable HTTP endpoint on Windows loopback.
Host Agents use it directly; WSL Agents use an on-demand loopback relay inside
their distro, avoiding inbound Windows firewall requirements. Relays are
master-lifetime services with bounded request handling and a master-owned stdin
pipe that terminates the distro process if master exits; unexpected relay
failure is restarted on the same port so existing sessions recover. Each ACP
session's `McpServer::Http` configuration carries an independent public server
name and a distinct bearer capability, so name-keyed Agent caches cannot
overwrite another session's header. Master maps the capability to SessionId,
resolves the current Helper through `session_to_helper`, and forwards the typed
input over the existing ACP pipe.
The endpoint exposes `terminal_send`, `terminal_open`, and
`terminal_open_and_send`, which return after the Helper
confirms that the recommendation card was presented, and `request_user_input`,
which blocks until the user answers, cancels, disconnects, or the request times
out. The latter is a WTA-owned fallback and does not intercept provider-native
question tools.

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
| **Master** | `src/master/mod.rs` | ACP multiplexer singleton: owns the lazy agent CLI pool, serves helpers over the pipe, routes per-session notifications |
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

ACP (`agent-client-protocol = "1.3.0"`, JSON-RPC 2.0) is spoken on two hops:

- **master ↔ agent CLI** (stdio): master is the ACP **client**; it lazily
  spawns and owns one process per distinct agent key.
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
| ACP protocol | agent-client-protocol 1.3.0 |
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
| **wta-master** | `wta.exe --master` | Spawned once by `SharedWta` | Owns the agent CLI pool; multiplexes ACP sessions for all helpers |
| **wta-helper** | `wta.exe --connect-master` | One per agent pane | TUI + per-pane side effects; ACP client of master |
| **Agent CLI** | `copilot`, `claude`, `gemini`, `codex`, `opencode` | Spawned lazily by master | One warm process per agent key; shared by helpers using that key |
| **wtcli** | `wtcli.exe` | Per call (or long-running for `listen`) | COM client for `IProtocolServer`; bridges wta → WT |
| **Shell commands** | `pwsh`, `cargo`, `git`, … | Spawned by WT; exit when done | The actual tools doing the work |

### Key lifetime points

- One agent CLI is shared by panes/tabs with the **same agent key**. A helper's
  `session/new` round-trips to that CLI; `initialize` is cached per process.
- Helpers are **pre-warmed per tab** at tab creation (`--start-stashed`), so the
  ACP session connects in the background even before the user opens the pane —
  this is what lets autofix work on a stashed pane.
- Toggling an agent pane **stashes** it (helper + conpty + ACP session survive);
  the pane is only destroyed on tab close or `Ctrl+C ×2` in the TUI.
- If master dies, helpers exit on pipe EOF and `closeOnExit:"always"` closes
  their panes. There is no reconnect or automatic `session/load`; a later
  user-initiated pane open creates a fresh master, helper, and ACP session.
  `/restart` applies only while a live helper can explicitly request a fresh
  stack.

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
