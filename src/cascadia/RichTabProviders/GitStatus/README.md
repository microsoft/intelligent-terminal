# Built-in Git Status Rich Tab Provider

This first-party provider is packaged with Intelligent Terminal and enabled by
default. It renders up to two selected metadata fields from agent status,
current working directory, Git repository, Git branch, and Git changes.
Agent status comes from the authoritative master session registry. Git changes use the format
`~<changed files> +<added lines> -<deleted lines>`.

The provider can return first-party fields such as agent status before shell
integration reports a directory. Git metadata is omitted until the working
directory is authoritative.
