# Built-in Git Status Rich Tab Provider

This first-party provider is packaged with Intelligent Terminal and enabled by
default. It renders the active pane's repository, branch, clean/dirty state,
and upstream ahead/behind counts.

The provider intentionally returns an empty snapshot until shell integration
has reported an authoritative working directory.

For offline development:

```powershell
wtcli provider validate .\provider.json
wtcli provider test .\provider.json --cwd C:\path\to\repo
```
