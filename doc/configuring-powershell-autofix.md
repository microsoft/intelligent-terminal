# Configuring Shells for Auto-Fix

The WTA auto-fix feature automatically detects when a command fails in another pane and suggests a fix. It works by listening for **OSC 133** shell integration sequences that the shell emits after each command.

The downstream pipeline (autofix detection, classification, VT-event forwarding) is **shell-agnostic** — it only cares about the OSC 133 marks on the wire. Any shell that emits them works. Today the installer ships ready-to-go integrations for:

- **PowerShell 7+** (`pwsh.exe`) — written to `Documents\PowerShell\Microsoft.PowerShell_profile.ps1`
- **Windows PowerShell 5.1** (`powershell.exe`) — written to `Documents\WindowsPowerShell\Microsoft.PowerShell_profile.ps1`
- **Bash** (Git Bash on Windows) — block written to `~/.bashrc`; the block sources `$HOME/.intelligent-terminal/shell-integration_v1.sh` (which is `%USERPROFILE%\.intelligent-terminal\shell-integration_v1.sh` on Git Bash, where `$HOME` resolves to `%USERPROFILE%`)
- **WSL** (one install per WSL distro you have a Windows Terminal profile for) — block written to the distro's `~/.bashrc`; the block sources `$HOME/.intelligent-terminal/shell-integration_v1.sh` inside the distro filesystem. We write both via the `\\wsl$\<distName>\` UNC mount from the Windows side

> **Distro discovery.** The installer iterates `_settings.AllProfiles()` and picks every profile whose `Source` is `Windows.Terminal.Wsl` (the dynamic-profile namespace used by `WslDistroGenerator`). Add a distro to WT (Settings → "+ Add a new profile" picks up new WSL distros automatically; or `wsl --install <Distro>` followed by relaunching WT) and the next FRE save or Settings install will cover it.

> **Cold-start cost.** The first `wsl.exe` invocation in a Windows session spins up the WSL2 VM (~5–15s). The installer's per-distro `$HOME` probe pays this cost once; subsequent invocations are fast.

## How It Works

1. The shell emits `OSC 133;D;<exit_code>` after every command finishes
2. Windows Terminal forwards this as a `vt_sequence` event to WTA
3. If `exit_code != 0`, WTA reads the pane's terminal buffer and asks the AI to diagnose the error and suggest a fix
4. The user reviews and confirms the suggestion before it runs

## Requirements

- **Windows Terminal** with the Intelligent Terminal build (handles event forwarding)
- A supported shell with integration enabled (FRE / Settings UI installs both PowerShell flavors and Bash automatically)

## Enabling Shell Integration

The FRE wizard and the Settings UI "Install" button handle this for you. The sections below document the snippets they install, in case you want to install manually or audit them.

### PowerShell (manual)

Add the following to your PowerShell profile (open it with `notepad $PROFILE`):

```powershell
# Shell integration for Windows Terminal (OSC 133 marks)
$__origPrompt = $function:prompt
function prompt {
    $ec = if ($?) { 0 } else { 1 }
    "`e]133;D;$ec`a`e]133;A`a$($__origPrompt.Invoke())`e]133;B`a"
}
```

This wraps your existing prompt to emit three OSC 133 sequences on every command:

| Sequence | Meaning | Role |
|----------|---------|------|
| `133;D;$ec` | Command finished with exit code | **Triggers auto-fix when `$ec != 0`** |
| `133;A` | Prompt start | Marks where the new prompt begins |
| `133;B` | Command input start | Marks where user input begins |

The key is `133;D` — it reports the previous command's exit code. WTA listens for this and triggers auto-fix whenever the exit code is non-zero.

### Manual bash setup

Add the following to your `~/.bashrc`:

```bash
__it_shellinteg_prompt() {
    local __ec=$?
    printf '\033]133;D;%s\007\033]133;A\007\033]9;9;"%s"\007' "$__ec" "$PWD"
}
PROMPT_COMMAND=__it_shellinteg_prompt
PS1="${PS1}\[$'\033]133;B\007'\]"
```

This produces the same `133;D` / `133;A` / `133;B` marks as the PowerShell snippet (plus OSC `9;9` to report the current working directory). The `\[ \]` brackets tell readline the embedded escape sequence is zero-width so line wrap stays correct.

For Git Bash users on Windows, the FRE / Settings installer takes care of all of this for you — including a more careful version that preserves any existing `PROMPT_COMMAND` and guards on `$BASH_VERSION` so non-bash shells silently no-op.

### Verifying It Works

1. Open a pane in Intelligent Terminal
2. Run a command that fails, e.g.: `Get-Item "C:\nonexistent-path"` (pwsh) or `ls /nonexistent` (bash)
3. The WTA agent pane should show a notification and automatically suggest a fix

### Checking the Diagnostic Log

Autofix events are logged by the shared host process. Find the log directory:

```powershell
# Packaged install (F5 / MSIX):
$pkg = Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' } | Select-Object -First 1
$logDir = "$env:LOCALAPPDATA\Packages\$($pkg.PackageFamilyName)\LocalCache\Local\IntelligentTerminal\logs"

# Unpackaged:
$logDir = "$env:LOCALAPPDATA\IntelligentTerminal\logs"

Get-Content "$logDir\wta-ensure-host.log" -Tail 20
```

Look for `target: "autofix"` lines — they show received events, classification, and whether auto-fix was triggered.

## Behavior Notes

- **One-shot**: Auto-fix triggers only once per user prompt. After a fix is suggested (whether accepted or not), it won't trigger again until the user manually submits a new prompt. This prevents cascading loops.
- **Idle only**: Auto-fix only fires when the agent is connected and not already processing a request.
- **Own-pane filtering**: Events from WTA's own pane are ignored to avoid self-triggering.
- **Buffer context**: When auto-fix triggers, it reads the last ~30 lines from the failing pane to provide error context to the AI.
