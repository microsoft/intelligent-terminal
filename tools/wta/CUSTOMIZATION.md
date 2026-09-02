# Runtime Customization

## Change ACP CLI

Edit `agentCliPath` in the Terminal settings file you are using.

Packaged IntelligentTerminal:
- `%LOCALAPPDATA%\Packages\Microsoft.IntelligentTerminal_8wekyb3d8bbwe\LocalState\settings.json`

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

WTA sends `terminal-agent.md` once on an ACP session's first prompt, including
when that prompt starts Auto error handling. Later prompts send only current
runtime context, per-turn instructions, and user input. Auto error handling
turns add `auto-error-handling.md` as an instruction overlay; they do not create
a separate agent mode or cause the base prompt to be resent.
