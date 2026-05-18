# Runtime Customization

## Change ACP CLI

Edit `agentCliPath` in the Terminal settings file you are using.

Packaged Windows Terminal:
- `%LOCALAPPDATA%\Packages\Microsoft.WindowsTerminal_8wekyb3d8bbwe\LocalState\settings.json`

Portable/local IntelligentTerminal:
- `%LOCALAPPDATA%\Programs\IntelligentTerminal\settings\settings.json`

Example:

```json
"agentCliPath": "copilot --acp --stdio --model claude-haiku-4.5"
```

Restart Terminal after changing it.

## Change Spawned Delegate Agent CLI

Edit `delegateAgentCliPath` in the same Terminal settings file.

Example:

```json
"delegateAgentCliPath": "copilot --model claude-haiku-4.5"
```

This is used for spawned delegate tabs and panels, separately from `agentCliPath`.

## Change Runtime Prompt

Edit:
- `%LOCALAPPDATA%\IntelligentTerminal\prompts\terminal-agent.md`

Reference copy:
- `%LOCALAPPDATA%\IntelligentTerminal\prompts\terminal-agent.default.md`

WTA reloads `terminal-agent.md` on each prompt submission.
