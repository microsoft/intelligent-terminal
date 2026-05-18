# WTA Skills — Windows Terminal CLI Commands

> This file is for AI agents. It documents the `wta` CLI commands you can use
> to inspect and control Windows Terminal panes from the shell.
>
> **Prerequisite**: `WT_COM_CLSID` must be set in the environment (Windows
> Terminal injects it automatically into every pane it spawns).

## Quick Reference

| Command | Alias | Description |
|---------|-------|-------------|
| `wta list-windows` | `lsw` | List all WT windows |
| `wta list-tabs` | `lst` | List tabs in a window |
| `wta list-panes` | `lsp` | List panes in a tab |
| `wta active-pane` | — | Show the focused pane's ID |
| `wta capture-pane` | `capturep` | Read a pane's terminal output |
| `wta pane-status` | — | Check if a pane's process is running |
| `wta new-tab` | `neww` | Create a new tab |
| `wta split-pane` | `splitw` | Split a pane horizontally/vertically |
| `wta kill-pane` | `killp` | Close a pane |
| `wta wait-for` | — | Block until a pane's process exits |

## Discovering Panes

```bash
# List all panes (shows PANE_ID, PID, ACTIVE, ROWS, COLS)
wta list-panes

# Get just the active pane
wta active-pane

# Full hierarchy: windows → tabs → panes
wta list-windows
wta list-tabs -w 1
wta list-panes -t 0
```

Use `--json` on any command for machine-readable output.

## Sending Input to a Pane

`wta` no longer exposes a CLI verb for keystroke injection. Direct shell
input goes through a per-WTA capability pipe and is only reachable from the
`wta` process Windows Terminal itself launched for a given pane. To deliver
a prompt to a fresh agent, embed it in the pane's startup commandline via
`wta new-tab -c "<agent> <prompt>"` (see Creating New Sessions below).

## Reading Pane Output

```bash
# Capture last 20 lines from a pane
wta capture-pane -t <PANE_ID> -l 20

# Capture from the active pane (default 200 lines)
wta capture-pane
```

## Checking Process Status

```bash
# Is the pane's shell still running?
wta pane-status -t <PANE_ID>

# Block until a process finishes (with 30s timeout)
wta wait-for -t <PANE_ID> --timeout 30
```

## Creating New Sessions

```bash
# New tab with default shell
wta new-tab

# New tab running a specific command
wta new-tab -c "python server.py" -n "Server"

# Split the active pane vertically
wta split-pane -v

# Split a specific pane horizontally, running a command
wta split-pane -t <PANE_ID> -H -c "npm run dev"
```

## Asking the User

Use the agent's built-in permission/confirmation flow (ACP `request_permission`)
or prompt via the agent UI. WTA no longer ships a dedicated `quick-pick` CLI.

## Common Workflows

### Run a command in a new pane and monitor it
```bash
# 1. Split a pane for the build
wta split-pane -v -c "cargo build 2>&1"
# Response: "Created pane 5"

# 2. Wait for it to finish
wta wait-for -t 5 --timeout 120

# 3. Check the result
wta pane-status -t 5
wta capture-pane -t 5 -l 30
```

### Delegate work to another AI instance
```bash
# 1. Create a new tab for the delegate with the prompt baked into argv
wta new-tab -c 'claude "Fix the failing test in src/auth.rs"' -n "Delegate"

# 2. Monitor progress (capture the new pane's output)
wta capture-pane -t <NEW_PANE_ID> -l 50
```

### Interactive user confirmation

For confirmations, prefer the agent's built-in permission flow rather than
shell prompts.
