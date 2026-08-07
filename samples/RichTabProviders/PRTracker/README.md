# PowerShell PR Tracker Provider

This sample implements the Rich Tab Provider V1 one-shot protocol. It reads one
JSON request from standard input, queries the pull request associated with the
current branch through `gh`, writes one structured snapshot to standard output,
and exits.

Validate the manifest without a running Terminal:

```powershell
wtcli provider validate .\provider.json
```

The sample requires PowerShell 7 and an authenticated GitHub CLI. It disables
interactive GitHub prompts and pagers. Diagnostics are written only to standard
error; it does not emit OSC or modify shell history.
