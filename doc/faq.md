# FAQ

Frequently asked questions about the current release of Intelligent Terminal. Some entries are first-run quirks with workarounds; others are intentional limitations that are planned to be improved. If your question isn't covered here, please [file an issue](https://github.com/microsoft/intelligent-terminal/issues).

## 1. Why is the first-run experience (FRE) taking so long, or failing?

Depending on which agent you pick, the first-run setup may need to download dependencies — [`winget`](https://learn.microsoft.com/windows/package-manager/winget/) is used to install GitHub Copilot CLI and (when needed) Node.js LTS, and `npm`/`npx` is used to fetch ACP wrappers for the bring-your-own agents (Claude, Codex, Gemini). On slow, throttled, or unreliable networks any of these downloads can take **more than 10 minutes**, and on intermittent connections they can fail outright.

**Workaround:**

- Make sure you're on a stable, unrestricted internet connection before running the FRE.
- If the FRE fails or times out, you can install the missing dependencies manually by following [`installing-dependencies.md`](./installing-dependencies.md), then re-open Intelligent Terminal — the FRE will detect what's already installed and skip those steps.

## 2. Why won't Intelligent Terminal install on my Windows 10 machine?

**Symptom:** The package manifest sets `MinVersion="10.0.22621.6060"` (Windows 11 22H2), so the MSIX install is blocked on earlier OS builds. This release is Windows 11 only.

**Workaround:** Use Windows 11 (22H2 or later). Windows 10 support is planned for a later release.

## 3. I installed a new agent CLI after the FRE — why isn't it tracked in Agent Management?

You completed the FRE with one agent (say, Copilot), then later installed Claude or Codex (or another bring-your-own ACP-compatible CLI) and switched the **agent pane** to it in Settings. The agent pane may not work, or **Agent Management** doesn't track its sessions.

The FRE only sets up the session-tracking hooks for the agents you went through it with. Agents installed *after* the FRE need a one-time manual setup. (The ACP wrapper itself is auto-fetched on demand via `npx`, so there is no wrapper "install" to run — see [Step 3.2.3](./installing-dependencies.md#step-323--acp-wrapper-no-install-action-required) / [Step 3.3.3](./installing-dependencies.md#step-333--acp-wrapper-no-install-action-required) — but you do need to make sure the prerequisites the wrapper depends on are in place.)

**Workaround:**

1. **Make sure the prerequisites are in place.** Follow the steps in [`installing-dependencies.md`](./installing-dependencies.md) that match your agent — install Node.js LTS and the agent's own CLI (via `npm install -g <package>`). The ACP wrapper itself requires no install action; it will be downloaded automatically the first time the agent is launched:
   - Claude: [Steps 3.2.1 – 3.2.3](./installing-dependencies.md#32-claude-code-bring-your-own)
   - Codex: [Steps 3.3.1 – 3.3.3](./installing-dependencies.md#33-openai-codex-bring-your-own)
   - Gemini: [Section 3.4](./installing-dependencies.md#34-gemini-cli-bring-your-own)

2. **Re-install the session-tracking hooks.** Open Intelligent Terminal **Settings → Agent**, scroll to the **Agent session tracking (hooks)** row ("Track sessions across agents. Required for agent session management."), expand it, and click the **Install hooks** button next to *Install agent hook script*. This wires the newly installed CLI into Agent Management so its sessions show up in the panel.

## 4. Why does the Model dropdown stay greyed out / show "default" after I change agents?

After you change the **agent** in Settings → Agent (or save a custom-command agent), the **Model** dropdown for that agent first appears greyed out with `default` selected, then becomes enabled and populates a few seconds later.

This isn't a freeze — Intelligent Terminal is doing a one-shot ACP handshake against the newly selected CLI in the background to ask which models it offers. How long that takes depends on the agent's own responsiveness and your network connection at that moment.

The fastest way to confirm everything is healthy: open the **agent pane** for that agent. If it shows **Connected**, the Model dropdown in Settings is ready and you can pick a model. If the agent pane reports a connection timeout instead, run `/restart` inside the agent pane — that's the easiest way to retry the connection.

## 5. Why doesn't Agent Management show my session on the first tab right after I install?

Immediately after installing Intelligent Terminal for the first time and selecting GitHub Copilot CLI as your agent, the **Agent Management** panel (<kbd>Ctrl+Shift+/</kbd>) may not show your active session for the very first tab you open.

**Workaround:** Either open a **second tab**, or run `/restart` inside the agent pane of the first tab. The session will then show up in Agent Management as expected.

This only affects the first tab of the first launch — subsequent tabs and subsequent app launches are unaffected.

## 6. Why is there no model picker for the delegate agent in Settings?

The Settings → Agent page exposes a **Model** dropdown for the **agent pane** agent, but there is no equivalent control for the **delegate agent** (the agent invoked by <kbd>Alt+Shift+/</kbd>, <kbd>Alt+Shift+B</kbd>, and the `?<prompt>` command-palette syntax). The delegate currently always runs against its agent CLI's default model. A Settings UI control for this is planned for a later release.

## 7. Why doesn't Agent Management show my delegate-agent sessions?

In this release, **Agent Management only tracks sessions for the agent CLI you selected as your agent-pane agent in Settings**. If your delegate agent (the one invoked by <kbd>Alt+Shift+/</kbd>, <kbd>Alt+Shift+B</kbd>, and the `?<prompt>` command-palette syntax) is a *different* CLI from your agent-pane agent, its sessions will not appear in the panel.

**Workaround:** Until a better design ships, select the **same agent** for both your agent pane and your delegate agent in Settings → Agent. With both pointed at the same CLI, the delegate's sessions will appear in Agent Management alongside the agent pane's.

---

*Last updated: 2026-06-02. See the [release notes](https://github.com/microsoft/intelligent-terminal/releases) for items resolved in newer versions.*
