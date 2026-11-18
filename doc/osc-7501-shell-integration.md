# Report a command's status from PowerShell

If an executable doesn't report [OSC 7501 program status](https://www.superlogical.com/rex/docs/build/program-status), PowerShell can report `working` when you launch it, then `done` or `error` based on its exit code. The shell can't tell whether it's waiting for input.

Add this function to your PowerShell 7 `$PROFILE` in the shell that runs the command (on the remote host for SSH). Place it outside the [built-in shell integration](configuring-shell-integration-autofix.md) block so its OSC 133 prompt and exit-code marks stay in place. Run programs with native OSC 7501 support directly.

```powershell
function Invoke-WithProgramStatus {
    param([string]$FileName)
    $report = -not [Console]::IsErrorRedirected
    $state = 'error'
    try {
        if ($report) { [Console]::Error.Write("`e]7501;state=working`a") }
        $exe = Get-Command $FileName -CommandType Application -ErrorAction Stop | Select-Object -First 1
        & $exe.Source @args
        if ($?) { $state = 'done' }
    } finally {
        if ($report) { [Console]::Error.Write("`e]7501;state=$state`a") }
    }
}
```

In a new PowerShell pane on Windows, try `Invoke-WithProgramStatus ping.exe -n 4 127.0.0.1` to see **Active** then **Completed**. Run `Invoke-WithProgramStatus cmd.exe /c exit 7` to see **Error**; `$LASTEXITCODE` is still `7`.
