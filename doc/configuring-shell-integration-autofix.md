# Configuring Shells for Auto error handling

Auto error handling uses **OSC 133** shell integration sequences emitted after each command to detect failures. Depending on the selected setting, Intelligent Terminal can stop after detection or send the error context to the agent for a proposed fix.

> [!NOTE]
> This guide retains its legacy `autofix` filename as a compatibility URL for
> existing bookmarks and external links. Auto error handling is the current
> feature name.

The downstream pipeline (failure detection, classification, and VT-event forwarding) is **shell-agnostic** — it only cares about the OSC 133 marks on the wire. Any shell that emits them works. Today the installer ships ready-to-go integrations for:

- **PowerShell 7+** (`pwsh.exe`) — written to `Documents\PowerShell\Microsoft.PowerShell_profile.ps1`
- **Windows PowerShell 5.1** (`powershell.exe`) — written to `Documents\WindowsPowerShell\Microsoft.PowerShell_profile.ps1`
- **Bash** (Git Bash on Windows) — block written to `~/.bashrc`; the block sources `$HOME/.intelligent-terminal/shell-integration_v1.sh` (which is `%USERPROFILE%\.intelligent-terminal\shell-integration_v1.sh` on Git Bash, where `$HOME` resolves to `%USERPROFILE%`)
- **WSL** (one install per WSL distro you have a Windows Terminal profile for) — block written to the distro's `~/.bashrc`; the block sources `$HOME/.intelligent-terminal/shell-integration_v1.sh` inside the distro filesystem. We write both via the `\\wsl$\<distName>\` UNC mount from the Windows side

> **Distro discovery.** The installer iterates `_settings.AllProfiles()` and picks every profile whose `Source` is `Windows.Terminal.Wsl` (the dynamic-profile namespace used by the legacy `WslDistroGenerator`) **or** `Microsoft.WSL` (the namespace used by the newer Store WSL package generator). Both work; the installer extracts the distro name from the commandline's `-d`/`--distribution` flag, or — for `Microsoft.WSL` profiles whose commandline is resolved at runtime — from the profile name. Add a distro to WT (Settings → "+ Add a new profile" picks up new WSL distros automatically; or `wsl --install <Distro>` followed by relaunching WT) and the next FRE save or Settings install will cover it.

> **Cold-start cost.** The first `wsl.exe` invocation in a Windows session spins up the WSL2 VM (~5–15s). The installer's per-distro `$HOME` probe pays this cost once; subsequent invocations are fast.

## How It Works

1. The shell emits `OSC 133;D;<exit_code>` after every command finishes
2. Windows Terminal forwards this as a `vt_sequence` event to WTA
3. If `exit_code != 0`, Intelligent Terminal applies the selected Auto error handling option:
   - `Off`
   - `Detect errors automatically`
   - `Detect errors and send them to the agent for fixes automatically.`
4. With the third option, WTA reads the pane's terminal buffer and asks the agent to diagnose the error and propose a fix
5. The user reviews and confirms the proposed fix before it runs

## Requirements

- **Windows Terminal** with the Intelligent Terminal build (handles event forwarding)
- A supported shell with integration enabled (FRE / Settings UI installs both PowerShell flavors, Bash, and per-distro WSL bash automatically)

## Enabling Shell Integration

The FRE wizard and the Settings UI "Install" button handle this for you. The snippets below are a simplified manual-install reference — useful for auditing what shell integration looks like or for installing by hand. The installer writes a slightly more careful version (preserving any existing `PROMPT_COMMAND`, guarding on `$BASH_VERSION` so the block silently no-ops in non-bash shells, and using the sentinel-bracketed `# >>> intelligent-terminal shell-integration >>>` markers so a later Uninstall finds the block byte-precisely).

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
| `133;D;$ec` | Command finished with exit code | **Reports a failure to Auto error handling when `$ec != 0`** |
| `133;A` | Prompt start | Marks where the new prompt begins |
| `133;B` | Command input start | Marks where user input begins |

The key is `133;D` — it reports the previous command's exit code. WTA listens for this and applies the selected Auto error handling option whenever the exit code is non-zero.

### Manual bash setup

Add the following to your `~/.bashrc`:

```bash
__it_shellinteg_prompt() {
    local __ec=$?
    printf '\033]133;D;%s\007\033]133;A\007\033]9;9;%s\007' "$__ec" "${PWD:-}"
}
PROMPT_COMMAND=__it_shellinteg_prompt
PS1="${PS1:-}"'\[\033]133;B\007\]'
```

This produces the same `133;D` / `133;A` / `133;B` marks as the PowerShell snippet (plus OSC `9;9` to report the current working directory). The `\[ \]` brackets tell readline the embedded escape sequence is zero-width so line wrap stays correct.

For Git Bash users on Windows, the FRE / Settings installer takes care of all of this for you — including a more careful version that preserves any existing `PROMPT_COMMAND` and guards on `$BASH_VERSION` so non-bash shells silently no-op.

### Verifying It Works

1. Open **Settings → AI Agents** and set **Auto error handling** to `Detect errors and send them to the agent for fixes automatically.`
2. Open a pane in Intelligent Terminal
3. Run a command that fails, e.g.: `Get-Item "C:\nonexistent-path"` (pwsh) or `ls /nonexistent` (bash)
4. The agent pane should show the detected failure and the agent's proposed fix

The equivalent JSON setting is:

```json
{
  "autoErrorHandling": "detectErrorsAndSendToAgentForFixesAutomatically"
}
```

### Checking the Diagnostic Log

Auto error handling events are logged by the shared host process. Find the log directory:

```powershell
# Packaged install (F5 / MSIX):
$pkg = Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' } | Select-Object -First 1
$logDir = "$env:LOCALAPPDATA\Packages\$($pkg.PackageFamilyName)\LocalCache\Local\IntelligentTerminal\logs"

# Unpackaged:
$logDir = "$env:LOCALAPPDATA\IntelligentTerminal\logs"

Get-Content "$logDir\wta-ensure-host.log" -Tail 20
```

Look for the `target: "auto_error_handling"` tracing target — those lines show received events, classification, and whether Auto error handling sent the failure to the agent.

## Behavior Notes

- **One-shot**: Auto error handling sends only one failure to the agent per user prompt. After a fix is proposed (whether accepted or not), it won't send another until the user manually submits a new prompt. This prevents cascading loops.
- **Idle only**: Auto error handling sends a failure to the agent only when the agent is connected and not already processing a request.
- **Own-pane filtering**: Events from WTA's own pane are ignored to avoid self-triggering.
- **Buffer context**: When the third option sends a failure to the agent, it reads the last ~30 lines from the failing pane to provide error context.

## Troubleshooting

### Checking which shells got integration

Every block written by the installer is bracketed with sentinel markers, so you can grep your profile files to see what the installer touched:

```powershell
# PowerShell 7+ and Windows PowerShell
Select-String -Path "$env:USERPROFILE\Documents\PowerShell\Microsoft.PowerShell_profile.ps1",
                    "$env:USERPROFILE\Documents\WindowsPowerShell\Microsoft.PowerShell_profile.ps1" `
              -Pattern 'intelligent-terminal shell-integration' -ErrorAction SilentlyContinue

# Bash on Git Bash (Git Bash uses %USERPROFILE% as $HOME, so its ~/.bashrc IS %USERPROFILE%\.bashrc)
Get-Content "$env:USERPROFILE\.bashrc" -ErrorAction SilentlyContinue |
    Select-String 'intelligent-terminal shell-integration'

# Every WSL distro you have a Windows Terminal profile for — each distro has its OWN ~/.bashrc
# inside the distro filesystem (NOT %USERPROFILE%\.bashrc), so check each one individually:
wsl.exe -d <DistroName> -e bash -c "grep 'intelligent-terminal shell-integration' ~/.bashrc"
```

If the markers aren't present, the installer didn't run for that shell — re-run FRE or use Settings → Install.

### Removing the integration manually

Each block is self-contained between the open marker (`# >>> intelligent-terminal shell-integration >>>`) and the close marker (`# <<< intelligent-terminal shell-integration <<<`). Delete everything between (and including) those two lines, save the file, and restart the shell. The Settings UI "Uninstall" button does the same thing — including for every WSL distro it can find.

The sourced helper script (`~/.intelligent-terminal/shell-integration_v1.sh` for bash; equivalent versioned files for the other shells) is also safe to delete by hand. The installer leaves stale versions in place to support side-by-side rollback; remove them after you've confirmed the latest version works.

### WSL: distro not detected

The installer discovers WSL distros by iterating `_settings.AllProfiles()` and filtering on `Source == "Windows.Terminal.Wsl"` (legacy `WslDistroGenerator`) **or** `Source == "Microsoft.WSL"` (newer Store WSL package generator). A distro is invisible to the installer if Windows Terminal hasn't generated a profile for it yet. Two fixes:

1. **New distro**: open WT Settings → "+ Add a new profile" (the "from default profiles" picker lists newly registered WSL distros), or just relaunch WT after `wsl --install <Distro>`. Then re-run the installer.
2. **Hidden profile**: if the WSL profile exists but is marked `"hidden": true` in `settings.json`, the installer still picks it up (the filter is on `Source`, not visibility). If it doesn't, check that the profile's `source` field literally reads `"Windows.Terminal.Wsl"` or `"Microsoft.WSL"` — `wsl-distro-launcher`-style manual profiles use a different generator and are skipped.

### `set -u` / strict bash mode

The installed bash block uses `${VAR:-}` defaulting throughout (e.g. `${PROMPT_COMMAND:-}`, `${BASH_VERSION:-}`), so it's safe under `set -u`. If you've added `set -u` to your `.bashrc` ABOVE the integration block and a prior `PROMPT_COMMAND` was undefined, the integration still works — earlier versions would have errored here.

### Multiple PowerShell versions

The PowerShell 7+ block writes to `Documents\PowerShell\` and the Windows PowerShell 5.1 block writes to `Documents\WindowsPowerShell\` — two different files. Both get the same integration but each is independent. Uninstalling from one does not affect the other.
