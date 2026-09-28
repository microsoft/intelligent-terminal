# Built-in Git Status Rich Tab Provider

This first-party provider is packaged with Intelligent Terminal and enabled by
default. It renders up to two selected metadata fields from agent status,
current working directory, Git repository, Git branch, and Git changes.
Agent status comes from the authoritative master session registry. Git changes use the format
`~<changed files> +<added lines> -<deleted lines>`.

The provider intentionally returns an empty snapshot until shell integration
has reported an authoritative working directory.

For offline development:

```powershell
wtcli provider validate .\provider.json
wtcli provider test .\provider.json --cwd C:\path\to\repo
```
