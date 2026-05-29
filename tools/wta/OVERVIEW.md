# WTA (Windows Terminal Agent) — Project Overview

## One-line summary

WTA is a Rust command-line tool that **bridges AI agents and Windows Terminal**. It lets AI (e.g. GitHub Copilot, Claude) drive your terminal directly: create tabs, split panes, run commands, read output — like an AI-powered tmux.

---

## What problem does it solve?

Today's AI coding assistants can "talk about" code but can't "do" anything in the terminal. WTA fills that gap:

- **AI wants to run a command?** → WTA opens a pane in Windows Terminal and runs it
- **AI wants to read command output?** → WTA pulls the content from the terminal and returns it
- **User wants to drive the terminal via natural-language chat?** → WTA provides a TUI chat surface; AI executes on your behalf

---

## Three operating modes

### 1. ACP TUI mode (default) — interactive chat UI

```
wta
wta --agent "copilot --acp --stdio"
wta "list all my git branches"
```

Starts an in-terminal chat surface (rendered with ratatui). WTA acts as an **ACP client**, spawns an AI agent subprocess (Copilot by default), and talks to it over JSON-RPC on stdin/stdout. The user types in the chat box; the AI can request command execution, terminal creation, etc., which WTA dispatches and renders.

When ACP mode is connected to Windows Terminal, the recommended agent control surface is the local `wta` CLI rather than MCP-in-the-same-session. That is, agents shell out to commands like `wta active-pane --json`, `wta list-panes --json`, `wta capture-pane --json`, and the CLI talks to WT over the protocol.

### 2. MCP server mode — tool server for AI

```
wta mcp
```

Headless. WTA runs as an **MCP server** exposing 15 tools to an external AI agent. Through these tools the agent can:
- Execute commands (`run_command`)
- Create/manage terminal sessions (`create_terminal`, `get_terminal_output`, `kill_terminal`)
- Query Windows Terminal state (`wt_list_windows`, `wt_list_tabs`, `wt_list_panes`)
- Control Windows Terminal (`wt_create_tab`, `wt_split_pane`, `wt_send_input`, `wt_close_pane`)
- Read terminal content (`wt_read_pane_output`, `wt_get_process_status`)

### 3. CLI mode — tmux-style commands

```
wta list-windows                          # list all WT windows
wta list-tabs                             # list tabs
wta capture-pane -t 3 -l 50               # read the last 50 lines from pane 3
wta new-tab -c "pwsh.exe" -n "Build"      # create a new tab
wta split-pane -h                         # split the current pane horizontally
```

One-shot commands that talk directly to Windows Terminal. Useful for both humans and agents.

---

## Architecture diagram

```
 AI Agent CLI (copilot/claude)     external AI agent     user / AI shell-out
       |  ACP/stdio                  |  MCP/stdio              |  CLI subcommands
       v                             v                          v
 +-----------+                +-----------+              +-------------+
 | ACP mode  |                | MCP mode  |              | CLI mode    |
 | (TUI)     |                | (headless)|              | (one-shot)  |
 | client.rs |                | server.rs |              | main.rs     |
 +-----+-----+                +-----+-----+              +------+------+
       |                             |                          |
       +-----------------+-----------+                          |
                         |                                      |
                   ShellManager                                 |
                    |         |                                 |
              local child   WtChannel (COM via wtcli) <---------+
                                  |
                                  v
                         Windows Terminal
                        (TerminalProtocolComServer)
```

---

## Core modules

| Module | File | Responsibility |
|------|------|------|
| **main.rs** | `src/main.rs` | CLI parsing (clap), mode dispatch, protocol discovery |
| **ACP Client** | `src/protocol/acp/client.rs` | ACP client; spawns the AI subprocess; handles JSON-RPC messages |
| **MCP Server** | `src/protocol/mcp/server.rs` | MCP server; exposes 15 tools via rmcp |
| **ShellManager** | `src/shell/shell_manager.rs` | Terminal process manager: local child or WT pane |
| **CliChannel** | `src/shell/wt_channel/cli_channel.rs` | Shells out to `wtcli.exe` to talk to WT's COM server (the only WT transport) |
| **TUI** | `src/ui/*.rs` | ratatui chat UI: message rendering, input box, permission prompts, status bar |
| **App** | `src/app.rs` | TUI state machine and event loop |

---

## Communication protocols

### WTA ↔ AI Agent
- **ACP (Agent Client Protocol)**: JSON-RPC 2.0 over stdio; WTA is the client
- **MCP (Model Context Protocol)**: JSON-RPC 2.0 over stdio; WTA is the server

### WTA ↔ Windows Terminal
- **Protocol**: COM (`IProtocolServer`) for all methods, including `SendInput` (direct shell input)
- **Discovery**: `WT_COM_CLSID` environment variable, set by WT at startup and inherited by every conpty child (so wta and wtcli see it automatically)
- **Authorization**: COM is gated by Windows packaged-COM / terminal activation policy

---

## Tech stack

| Purpose | Crate |
|------|-----|
| Async runtime | tokio |
| CLI parsing | clap 4 |
| TUI rendering | ratatui + crossterm |
| ACP protocol | agent-client-protocol 0.10 |
| MCP protocol | rmcp 1.1 |
| Serialization | serde + serde_json |
| Error handling | anyhow |

---

## Build & run

```bash
# Prereq: install Rust (rustup)
cd tools/wta
cargo build

# Output binary: tools/wta/target/debug/wta.exe

# Run ACP chat mode
wta

# Run MCP server mode
wta mcp

# Test the WT protocol connection
wta test-pipe

# tmux-style operations
wta list-windows
wta capture-pane -l 5
```

---

## Relationship to the Windows Terminal repo

WTA lives under the `tools/wta/` subdirectory of the Windows Terminal source tree. It is an independent Rust project, but by design it is a **companion tool** to Windows Terminal:

- The C++ side ships `TerminalProtocolComServer`, exposing `IProtocolServer` via local COM activation
- The Rust side (WTA) talks to it indirectly by shelling out to `wtcli.exe`, which is the COM client
- Current state: COM is the sole control plane for all WT operations, including direct shell input
- Future plans: deeper in-pane integration may extend WT's VT parser or grow the protocol's method set

---

## Process model in detail

The system involves **4 kinds of processes** and a small number of IPC channels. Each scenario is broken down below.

---

### Process inventory

| Process | Binary | Lifetime | Role |
|------|-----------|---------|------|
| **Windows Terminal** | `WindowsTerminal.exe` | User-launched, long-lived | Window manager + terminal renderer; hosts `TerminalProtocolComServer` |
| **WTA** | `wta.exe` | User- or AI-launched | Bridge layer; plays different roles depending on mode |
| **AI Agent** | `copilot`, `claude-agent-acp`, etc. | Spawned by WTA or running externally | The AI "brain"; makes decisions |
| **wtcli** | `wtcli.exe` | Spawned per call (or long-running for `listen`) | COM client for `IProtocolServer`; bridges wta → WT |
| **Shell commands** | `cargo`, `git`, `pwsh`, ... | Spawned by WTA or WT; exit when done | The actual tools doing the work |

---

### Scenario 1: ACP TUI mode (default `wta`)

User runs `wta` in a Windows Terminal pane; the chat UI comes up.

```
┌─────────────────────────────────────────────────────────────┐
│                   Windows Terminal (process A)               │
│                   PID: 1000                                  │
│  ┌─────────────────────────────┐ ┌────────────────────────┐ │
│  │ Pane 1: wta.exe (process B) │ │ Pane 2: cargo build    │ │
│  │ PID: 2000                   │ │ PID: 4000              │ │
│  │                             │ │ (created by AI via WTA)│ │
│  │  ┌──────────────────────┐   │ │                        │ │
│  │  │ copilot (process C)  │   │ │                        │ │
│  │  │ PID: 3000            │   │ │                        │ │
│  │  │ (WTA's child)        │   │ │                        │ │
│  │  └──────────────────────┘   │ │                        │ │
│  └─────────────────────────────┘ └────────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
```

**Process relationships and IPC:**

```
process A: Windows Terminal       process B: wta.exe            process C: copilot
(WindowsTerminal.exe)             (chat TUI)                    (AI agent child)
      │                                │                            │
      │◄── COM (via wtcli) ───────────►│◄── stdio (ACP JSON-RPC) ──►│
      │  (all methods, incl. SendInput)│    stdin/stdout             │
      │                                │                            │
      │                                │  B is C's parent           │
      │                                │  B spawns C                │
```

**Information flow (user says "run cargo build"):**

1. User types "run cargo build" in the WTA TUI
2. **WTA → Agent** (ACP stdio): `prompt("run cargo build")`
3. **Agent → WTA** (ACP stdio): `create_terminal({command: "cargo", args: ["build"]})`
4. **WTA → WT** (COM via wtcli): `create_tab({commandline: "cargo build", background: true})`
5. **WT** creates a new pane and spawns the `cargo build` process (process D, PID 4000)
6. **WT → WTA** (COM): `{pane_id: "2"}`
7. WTA maps pane_id to terminal_id and returns to the agent
8. The agent later calls `terminal_output` → WTA invokes `read_pane_output` over COM
9. The agent calls `wait_for_terminal_exit` → WTA polls `get_process_status`

**Process count:** at minimum 3 (WT + WTA + agent); one extra shell process per command in WT.

---

### Scenario 2: MCP server mode (`wta mcp`)

WTA runs as a headless tool server, called by an external AI agent over MCP.

```
┌─────────────────────────────────────────────────────────────┐
│                   Windows Terminal (process A)               │
│  ┌─────────────────────────────┐ ┌────────────────────────┐ │
│  │ Pane 1                      │ │ Pane 2: pwsh           │ │
│  │ (some shell)                │ │ (created by wta)       │ │
│  └─────────────────────────────┘ └────────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
        ▲                          COM
        │                             │
┌───────┴───────────────────────────────┐
│            wta.exe mcp (process B)     │  ← MCP server
│            PID: 2000                    │
└───────────────────┬────────────────────┘
                    │ stdio (MCP JSON-RPC)
                    │
┌───────────────────▼────────────────────┐
│        External AI agent (process C)   │  ← MCP client
│        (VS Code Copilot, Claude Desktop│
│         or any MCP client)              │
│        C is B's parent!                 │
└────────────────────────────────────────┘
```

**Key contrast:** in MCP mode, **the call direction is inverted**:

| | ACP mode | MCP mode |
|---|---|---|
| Who spawns whom | WTA spawns Agent | Agent spawns WTA |
| WTA's role | Client (sends requests) | Server (handles requests) |
| Agent's role | Server (handles prompts) | Client (calls tools) |
| stdio direction | WTA → Agent stdin | Agent → WTA stdin |

**Information flow (AI wants to run `git status`):**

1. **Agent → WTA** (MCP stdio): `tools/call run_command {command: "git", args: ["status"]}`
2. WTA's ShellManager picks a route:
   - **WT protocol available** → `create_tab` in WT, read output via COM
   - **WT protocol unavailable** → local `tokio::process::Command` spawn
3. **WTA → Agent** (MCP stdio): `{stdout: "On branch main\n...", exit_code: 0}`

---

### Scenario 3: CLI mode (`wta list-windows`, etc.)

Simplest case. WTA is a one-shot command-line tool that talks to WT and exits.

```
┌─────────────────────────────────────┐
│    Windows Terminal (process A)      │
└──────────────┬──────────────────────┘
               │ COM (via wtcli)
┌──────────────▼──────────────────────┐
│    wta list-windows (process B)      │
│    connect → send request → print → exit │
└─────────────────────────────────────┘
```

**No AI agent, no ShellManager.** WTA just shells out to `wtcli` once, prints the result, and exits. Lifetime: a few hundred milliseconds.

---

### Two paths for shell command execution

When an AI agent needs to run a command, ShellManager picks one of two routes:

```
                    ShellManager.create_terminal(config)
                              │
                    ┌─────────┴─────────┐
                    │ has_wt_channel()?  │
                    └─────────┬─────────┘
                     Yes      │      No
                  ┌───────────┴───────────┐
                  ▼                       ▼
         Path A: WT pane            Path B: local child
         (via COM)                  (tokio::process::Command)
                  │                       │
     ┌────────────┴────────────┐   ┌──────┴──────────────────┐
     │ 1. WTA → WT (COM):      │   │ 1. WTA spawns child       │
     │    create_tab(cmd)      │   │    directly (kill_on_drop)│
     │ 2. WT spawns in new pane│   │ 2. stdout/stderr piped    │
     │ 3. Read: read_pane_output│  │    into a WTA buffer      │
     │ 4. Status: get_process_status│ │ 3. Read buffer directly │
     │                          │   │ 4. child.wait()           │
     │ Pro: pane visible to user│   │                          │
     │ Con: requires WT         │   │ Pro: no dependencies      │
     └─────────────────────────┘   │ Con: invisible in TUI     │
                                    └──────────────────────────┘
```

**Fallback:** if WT pane creation fails, WTA automatically downgrades to the local-child path.

---

### IPC channel summary

| Channel | Transport | Protocol | Direction | Purpose |
|------|--------|------|------|------|
| **WTA ↔ Agent (ACP)** | stdio (stdin/stdout pipes) | JSON-RPC 2.0 (ACP) | bidirectional | WTA sends prompts; agent streams messages and requests command execution |
| **WTA ↔ Agent (MCP)** | stdio (stdin/stdout pipes) | JSON-RPC 2.0 (MCP) | bidirectional | Agent calls tools; WTA returns results |
| **WTA ↔ WT (COM)** | `wtcli.exe` subprocess → `CoCreateInstance(WT_COM_CLSID)` → `IProtocolServer` | WinRT IDL methods | bidirectional (push events via subscribed callback) | Tab/pane management, output reads, settings, events, direct shell input (`SendInput`) |

---

### Process lifetimes

```
time ────────────────────────────────────────────────────────────────►

Windows Terminal ════════════════════════════════════════════════════
  (user-launched, long-running)

  wta.exe ────────────────────────────────┐ (user Ctrl+C)
    │                                      │
    ├─ copilot (child) ───────────────────┤ (killed via kill_on_drop)
    │                                      │
    ├─[AI req] WT creates pane ────────────┼── cargo build ─── (done, exits)
    │                                      │
    ├─[AI req] WT creates pane ────────────┼── git status ──── (done, exits)
    │                                      │
    └──────────────────────────────────────┘

  wta list-windows ─┐ (one-shot, exits immediately)
                    └─
```

**Key points:**
- When WTA exits, `kill_on_drop(true)` ensures the agent child is killed
- Panes created by WT **outlive WTA** (they are WT's children, not WTA's)
- Local children (the fallback path) **die with WTA** (`kill_on_drop`)
- CLI-mode WTA processes are extremely short-lived: one request and out

---

### Overall process tree

```
WindowsTerminal.exe (PID 1000)        ← OS-level process tree
  ├── conhost / conpty
  │   ├── pwsh.exe (Pane 1's shell)
  │   │   └── wta.exe (PID 2000)      ← user runs wta in the pane
  │   │       └── copilot (PID 3000)   ← AI agent spawned by WTA
  │   │
  │   ├── pwsh.exe (Pane 2's shell)    ← user's own pane
  │   │
  │   ├── cargo.exe (Pane 3)           ← AI asked WTA to ask WT to create it
  │   └── git.exe   (Pane 4)           ← same
  │
  └── TerminalProtocolComServer        ← WT-internal component, not a separate process
      (runs on WT's MTA thread pool, services COM requests)
```

> **Note:** `TerminalProtocolComServer` is not a standalone process — it is a component inside Windows Terminal, running on the MTA thread pool to service COM requests via metadata-based marshaling.

---

## Current status

- **Part 1** (dual-mode architecture, ACP + MCP): ✅ done
- **Part 2** (Windows Terminal integration — COM protocol + CLI commands): ✅ done; this is the current primary path
- **Part 3** (CLI subcommands): ✅ done
- **Future**: deeper VT/OSC integration; `focus_pane`, `rename-window`, `resize-pane`, etc.
