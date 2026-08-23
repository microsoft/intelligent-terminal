# GitHub Copilot CLI YOLO Mode

`--yolo` enables all Copilot CLI permissions and skips confirmation prompts.
It is an alias for [`--allow-all`](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-programmatic-reference#command-line-options)
and is equivalent to:

```powershell
copilot --allow-all-tools --allow-all-paths --allow-all-urls
```

The three permissions are:

- [`--allow-all-tools`](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-programmatic-reference#command-line-options):
  Allows all available tools and shell commands without approval.
- [`--allow-all-paths`](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-programmatic-reference#command-line-options):
  Allows access to any filesystem path.
- [`--allow-all-urls`](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-programmatic-reference#command-line-options):
  Allows access to any URL.

It allows Copilot to:

- Run available tools, MCP tools, and shell commands.
- Read, create, modify, or delete files anywhere the current user can access.
- Access URLs and external services.
- Perform remote actions such as `git push` or creating pull requests.

YOLO does not grant administrator privileges, enable excluded tools, override
explicit deny rules, or bypass sandbox and organizational restrictions.