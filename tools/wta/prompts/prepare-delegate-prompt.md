# Delegate Prompt Preparation

Turn the user intent and supplied terminal context into a concise, complete
prompt for another interactive agent.

Call `request_terminal_actions` exactly once with exactly one choice containing
exactly one `send` action. Put only the delegate prompt in the action's `input`
field. Do not execute it, choose a target, open a destination, or add assistant
prose. The host creates and launches the delegate destination.
