# WTA Skills — Windows Terminal CLI Commands

> This file is for AI agents. It documents the `wta` CLI commands you can use
> to inspect and control Windows Terminal panes from the shell.
>
> **Prerequisite**: `WT_PIPE_NAME` must be set in the environment (Windows
> Terminal injects it automatically into every pane).

## Quick Reference

| Command | Alias | Description |
|---------|-------|-------------|
| `wta list-windows` | `lsw` | List all WT windows |
| `wta list-tabs` | `lst` | List tabs in a window |
| `wta list-panes` | `lsp` | List panes in a tab |
| `wta active-pane` | — | Show the focused pane's ID |
| `wta send-keys` | `send` | Type text/keys into a pane |
| `wta capture-pane` | `capturep` | Read a pane's terminal output |
| `wta pane-status` | — | Check if a pane's process is running |
| `wta new-tab` | `neww` | Create a new tab |
| `wta split-pane` | `splitw` | Split a pane horizontally/vertically |
| `wta kill-pane` | `killp` | Close a pane |
| `wta wait-for` | — | Block until a pane's process exits |
| `wta quick-pick` | — | Show a choice dialog in WT and get user selection |

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

```bash
# Send a command and press Enter
wta send-keys -t <PANE_ID> "ls -la" Enter

# Send Ctrl+C to interrupt
wta send-keys -t <PANE_ID> C-c

# Send multiple keys
wta send-keys -t <PANE_ID> "git status" Enter
```

**Supported key names**: `Enter`, `Space`, `Escape`, `Tab`, `BSpace`,
`C-c`, `C-d`, `C-z`, `C-l`, `C-a`, `C-e`, `C-k`, `C-u`, `C-w`.

If `-t` is omitted, the active pane is used.

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

## Asking the User (Quick Pick)

```bash
# Show a choice dialog — blocks until user picks or types
wta quick-pick "What should I do next?" "Fix the bug" "Add tests" "Refactor code"

# Allow freeform text input alongside choices
wta quick-pick --free-input "Pick a database:" "PostgreSQL" "MySQL" "SQLite"

# The selected text is printed to stdout
result=$(wta quick-pick "Continue?" "Yes" "No")
```

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
# 1. Create a new tab for the delegate
wta new-tab -c "claude" -n "Delegate"

# 2. Send it a task
wta send-keys -t <NEW_PANE_ID> "Fix the failing test in src/auth.rs" Enter

# 3. Monitor progress
wta capture-pane -t <NEW_PANE_ID> -l 50
```

### Interactive user confirmation
```bash
# Ask the user before proceeding
choice=$(wta quick-pick "Deploy to production?" "Yes, deploy" "No, cancel")
if [ "$choice" = "Yes, deploy" ]; then
    wta send-keys -t 3 "make deploy" Enter
fi
```
