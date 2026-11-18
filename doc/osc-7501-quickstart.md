# See program status in the Intelligent Terminal sidebar

If your program reports status using [OSC 7501 (Program Status Protocol)](https://www.superlogical.com/rex/docs/build/program-status), Intelligent Terminal updates the **Agent status** field for its open tab or pane automatically. For example, when a running tool asks a question, its status can change from **Active** to **Waiting for input**. This works over SSH, too, as long as the reports reach your local terminal. Multiplexers such as tmux or screen may filter them.

**If your program doesn't support OSC 7501**, the [sample PowerShell integration](samples/osc-7501-shell-integration.ps1) reports when a foreground command starts and whether it succeeds or fails. You run commands normally. The shell can't tell whether a program is halfway through a build or waiting for approval.

To try the sample in PowerShell 7 on Windows:

1. In an Intelligent Terminal PowerShell tab, run `Select-String -LiteralPath $PROFILE -Pattern 'shell-integration_v'` to find the script your profile loads. If there's no entry, [install shell integration](configuring-shell-integration-autofix.md#enabling-shell-integration) first.
2. If the entry points to `PowerShell\shell-integration_v15.ps1` under your Documents folder, back up that file, then replace it with the linked sample, keeping the installed filename. **Do not replace `$PROFILE` itself.** The sample is based on v15; don't overwrite a different version with it.
3. Open a new PowerShell tab and run `Start-Sleep -Seconds 3`. The sidebar's **Agent status** should show **Active**, then **Completed**. To undo the change, restore the backup and open another tab. A future integration update may switch your profile to a newer script.

**If you write your own script**, you can report those milestones directly. For example, in PowerShell 7, emit this just before asking the user a question:

```powershell
[Console]::Write("`e]7501;state=blocked:kind=question`a")
```

Once work resumes, send `state=working`; when it finishes, send `state=done` or `state=error`. The [public specification](https://www.superlogical.com/rex/docs/build/program-status#examples) includes examples with progress, messages, and a reusable shell function.
