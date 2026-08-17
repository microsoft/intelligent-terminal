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

WTA reads `terminal-agent.md` on each normal prompt submission, but sends the
full template only on an ACP session's first normal prompt. Later normal prompts
send only current runtime context and user input. Auto-fix turns read
`auto-fix.md` and do not cause the planner template to be resent.
