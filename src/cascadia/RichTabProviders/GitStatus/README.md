# Built-in Git Status Rich Tab Provider

This first-party provider is packaged with Intelligent Terminal and enabled by
default. It renders up to two selected metadata fields from agent status,
current working directory, Git repository, Git branch, and Git changes.
Agent status comes from the authoritative master session registry. Git changes use the format
`~<changed files> +<added lines> -<deleted lines>`.

The provider can return first-party fields such as agent status before shell
integration reports a directory. Git metadata is omitted until the working
directory is authoritative.

The broker resolves Windows `git.exe` once using the terminal process's explicit
`PATH`, without adding an implicit current-directory search. That immutable
absolute path determines both UI availability and the `firstPartyFields.gitBinary`
sent on every Git Status request. An explicit empty value means Git is unavailable
and never falls back to lookup. This does not discover Git inside WSL.

Standalone requests that omit `gitBinary` retain legacy `git.exe` lookup.
Direct provider tests can inject an absolute fake or missing executable through
that request field; there is no process-global Git override.
