# Report a command's status from Bash

If a command doesn't report [OSC 7501 program status](https://www.superlogical.com/rex/docs/build/program-status), Bash can report on its behalf: `working` before it starts, then `done` or `error` based on its exit code. The shell can't tell whether the command is waiting for input.

Add this function to `~/.bashrc` in the shell that runs the command (on the remote host for SSH). It works alongside Intelligent Terminal's [built-in shell integration](configuring-shell-integration-autofix.md): leave its OSC 133 prompt and exit-code marks in place. Use the wrapper only for commands without native status reporting so it won't overwrite a program's own reports.

```bash
with_status() {
    if [[ ! -t 2 ]]; then "$@"; return $?; fi
    local rc=0 state=done
    printf '\033]7501;state=working\007' >&2
    "$@" || { rc=$?; state=error; }
    printf '\033]7501;state=%s\007' "$state" >&2
    return "$rc"
}
```

In a new Bash session, try `with_status sleep 3` to see **Active** then **Completed**, or `with_status false` to see **Error**. Reports go to the terminal on stderr, leaving stdout and the command's exit code unchanged.
