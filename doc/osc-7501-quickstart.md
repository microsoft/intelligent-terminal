# See program status in the Intelligent Terminal sidebar

If your program reports status using [OSC 7501 (Program Status Protocol)](https://www.superlogical.com/rex/docs/build/program-status), Intelligent Terminal updates the **Agent status** field for its open tab or pane automatically. For example, when a running tool asks a question, its status can change from **Working** to **Waiting for input**. This works over SSH, too, as long as the reports reach your local terminal. Multiplexers such as tmux may filter them.

**If you maintain the program**, have it write an OSC 7501 sequence to the terminal whenever its state changes. Intelligent Terminal displays the reported state directly in the sidebar; no shell integration or plugin is needed. For example, a PowerShell 7 program can report that it needs an answer, then switch back to working:

```powershell
[Console]::Write("`e]7501;state=blocked:kind=question`a")
$name = Read-Host 'What is the project name?'
[Console]::Write("`e]7501;state=working`a")
```

Send `state=done` when the task succeeds or `state=error` when it fails. Programs written in other languages can emit the same bytes. See the [protocol examples](https://www.superlogical.com/rex/docs/build/program-status#examples) for progress and messages.

**If you can't change the program**, the [sample PowerShell integration](samples/osc-7501-shell-integration.ps1) reports when a foreground command starts and whether it succeeds or fails. You run commands normally. The shell can't tell whether a program is halfway through a build or waiting for approval.

To try the sample in PowerShell 7 on Windows:

1. In an Intelligent Terminal PowerShell tab, run `Select-String -LiteralPath $PROFILE -Pattern 'shell-integration_v'` to find the script your profile loads. If there's no entry, [install shell integration](configuring-shell-integration-autofix.md#enabling-shell-integration) first.
2. If the entry points to `PowerShell\shell-integration_v15.ps1` under your Documents folder, back up that file, then replace it with the linked sample, keeping the installed filename. **Do not replace `$PROFILE` itself.** The sample is based on v15; don't overwrite a different version with it.
3. Open a new PowerShell tab, run `Start-Sleep -Seconds 10`, then switch to another tab before the command finishes. Watch the original tab in the sidebar: **Working** changes to **Completed**. Returning to the original tab acknowledges the result. If you stay in that tab, the result is acknowledged immediately and you may see **Idle** instead.

To undo the change, restore the backup and open a new tab. A future integration update may switch your profile to a newer script.
