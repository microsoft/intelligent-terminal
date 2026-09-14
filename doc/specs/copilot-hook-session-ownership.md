# Copilot Hook Session Identity

`wtcli agent-hook` preserves a nonempty `COPILOT_SESSION_ID` for
`--cli-source copilot`. Copilot can emit transient `session_id` / `sessionId`
payload values for nested work, while `COPILOT_SESSION_ID` identifies the
durable CLI session. The bridge therefore uses payload identity only when the
Copilot environment value is empty. It accepts opaque environment values and
does not impose UUID syntax.

The change is deliberately limited to the hook envelope producer. TerminalPage
and WTA continue consuming `agent_session_id` exactly as before, and
non-Copilot sources retain payload-first behavior. The HookTrace E2E coverage
exercises both Copilot environment precedence and payload fallback. Mapping an
event does not itself trigger layout persistence.
