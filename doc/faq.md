# FAQ

Frequently asked questions about the current release of Intelligent Terminal. Some entries are first-run quirks with workarounds; others are intentional limitations that are planned to be improved. If your question isn't covered here, please [file an issue](https://github.com/microsoft/intelligent-terminal/issues).

## 1. Why is the first-run experience (FRE) taking so long, or failing?

Depending on which agent you pick, the first-run setup may need to download dependencies — [`winget`](https://learn.microsoft.com/windows/package-manager/winget/) is used to install GitHub Copilot CLI and (when needed) Node.js LTS, `npm install -g` fetches the bring-your-own agent CLIs, and `npx` fetches the ACP wrapper for Claude and Codex on first launch. On slow, throttled, or unreliable networks any of these downloads can take **more than 10 minutes**, and on intermittent connections they can fail outright.

**Workaround:**

- Make sure you're on a stable, unrestricted internet connection before running the FRE.
- If the FRE fails or times out, you can install the missing dependencies manually by following [`installing-dependencies.md`](./installing-dependencies.md), then re-open Intelligent Terminal — the FRE will detect what's already installed and skip those steps.

## 2. What Windows version does Intelligent Terminal require?

The package manifest sets `MinVersion="10.0.19041.0"` (Windows 10, version 2004), matching upstream Windows Terminal. Intelligent Terminal installs on **Windows 10 2004 (build 19041) or later**, including all of Windows 11.

**If the MSIX install is blocked:** your machine is on a Windows build older than 19041. Check your build with `winver`, then update via **Settings -> Windows Update**.

## 3. I installed a new agent CLI after the FRE — why isn't it tracked in agent session management?

You completed the FRE with one agent (say, Copilot), then later installed Claude or Codex (or another bring-your-own ACP-compatible CLI) and switched the **agent pane** to it in Settings. The agent pane may not work, or **agent session management** doesn't track its sessions.

The FRE only sets up the session-tracking hooks for the agents you went through it with. Agents installed *after* the FRE need a one-time manual setup. (The ACP wrapper itself is auto-fetched on demand via `npx`, so there is no wrapper "install" to run — see [Step 3.2.3](./installing-dependencies.md#step-323--acp-wrapper-no-install-action-required) / [Step 3.3.3](./installing-dependencies.md#step-333--acp-wrapper-no-install-action-required) — but you do need to make sure the prerequisites the wrapper depends on are in place.)

**Workaround:**

1. **Make sure the prerequisites are in place.** Follow the steps in [`installing-dependencies.md`](./installing-dependencies.md) that match your agent — install Node.js LTS and the agent's own CLI (via `npm install -g <package>`):
   - Claude: [Steps 3.2.1 – 3.2.3](./installing-dependencies.md#32-claude-code-bring-your-own) — Intelligent Terminal launches Claude through an ACP wrapper that is fetched automatically via `npx` on first launch.
   - Codex: [Steps 3.3.1 – 3.3.3](./installing-dependencies.md#33-openai-codex-bring-your-own) — same wrapper-via-`npx` pattern as Claude.
   - Gemini: [Section 3.4](./installing-dependencies.md#34-gemini-cli-bring-your-own) — Gemini speaks ACP natively, so no wrapper is needed; just install the CLI itself.

2. **Re-install the session-tracking hooks.** Open Intelligent Terminal **Settings → Agent**, scroll to the **Agent session tracking (hooks)** row ("Track sessions across agents. Required for agent session management."), expand it, and click the **Install hooks** button next to *Install agent hook script*. This wires the newly installed CLI into agent session management so its sessions show up in the panel.

## 4. Can I use a custom ACP-compatible agent (Qwen, Cline, Goose, Cursor, …)?

Yes. Intelligent Terminal can drive any agent CLI that implements the [Agent Client Protocol (ACP)](https://agentclientprotocol.com/get-started/agents) — the [linked list](https://agentclientprotocol.com/get-started/agents) on that page covers Qwen Code, Cline, Goose, Cursor, Kimi CLI, Kiro CLI, OpenHands, and many more, in addition to the ones Intelligent Terminal sets up for you (Copilot, Claude, Codex, Gemini).

**How to wire one up:** open **Settings → Agent**, then in either the **Agent** (agent pane) or **Delegate agent** dropdown, pick **+ Add new…** and enter the command that launches your CLI:

- For the **agent pane**, use the command that puts your CLI into ACP mode. Some CLIs need a flag — e.g. for Qwen Code: `qwen.cmd --acp`.
- For the **delegate agent**, you typically just need the bare CLI command — e.g. `qwen.cmd` — because the delegate launches it as a regular interactive session in a new tab.

You must install and authenticate the agent CLI yourself first (Intelligent Terminal does not install bring-your-own agents — see [`installing-dependencies.md`](./installing-dependencies.md) for the pattern).

**Limitation:** **Agent session management does not yet work for custom agents.** The session-tracking hooks ship only for the four built-in agents (Copilot, Claude, Codex, Gemini), so custom-agent sessions will not appear in the agent session management panel even after you install hooks. The agent pane and delegate flows themselves work normally.

## 5. Why does the Model dropdown stay greyed out / show "default" after I change agents?

After you change the **agent** in Settings → Agent (or save a custom-command agent), the **Model** dropdown for that agent first appears greyed out with `default` selected, then becomes enabled and populates a few seconds later.

This isn't a freeze — Intelligent Terminal is doing a one-shot ACP handshake against the newly selected CLI in the background to ask which models it offers. How long that takes depends on the agent's own responsiveness and your network connection at that moment.

The fastest way to confirm everything is healthy: open the **agent pane** for that agent. If it shows **Connected**, the Model dropdown in Settings is ready and you can pick a model. If the agent pane reports a connection timeout instead, run `/restart` inside the agent pane — that's the easiest way to retry the connection.

## 6. Why doesn't agent session management show my session on the first tab right after I install?

Immediately after installing Intelligent Terminal for the first time and selecting GitHub Copilot CLI as your agent, the **agent session management** panel (<kbd>Ctrl+Shift+/</kbd>) may not show your active session for the very first tab you open.

**Workaround:** Either open a **second tab**, or run `/restart` inside the agent pane of the first tab. The session will then show up in agent session management as expected.

This only affects the first tab of the first launch — subsequent tabs and subsequent app launches are unaffected.

## 7. Why is there no model picker for the delegate agent in Settings?

The Settings → Agent page exposes a **Model** dropdown for the **agent pane** agent, but there is no equivalent control for the **delegate agent** (the agent invoked by <kbd>Alt+Shift+/</kbd>, <kbd>Alt+Shift+B</kbd>, and the `?<prompt>` command-palette syntax). The delegate currently always runs against its agent CLI's default model. A Settings UI control for this is planned for a later release.

## 8. Why doesn't agent session management show my delegate-agent sessions?

In this release, **agent session management only tracks sessions for the agent CLI you selected as your agent-pane agent in Settings**. If your delegate agent (the one invoked by <kbd>Alt+Shift+/</kbd>, <kbd>Alt+Shift+B</kbd>, and the `?<prompt>` command-palette syntax) is a *different* CLI from your agent-pane agent, its sessions will not appear in the panel.

**Workaround:** Until a better design ships, select the **same agent** for both your agent pane and your delegate agent in Settings → Agent. With both pointed at the same CLI, the delegate's sessions will appear in agent session management alongside the agent pane's.

## 9. Why does Intelligent Terminal crash a few seconds after launch, or fail agent actions with "0x80010105 — The server threw an exception"?

**Symptom:** One of two related failures:

- The app launches, the agent pane starts, and then **`WindowsTerminal.exe` crashes a few seconds later**. Windows Event Viewer records an *Application Error* faulting in **`combase.dll`** with exception code **`0xc0000005`**.
- Or the app stays open, but agent-driven actions (for example, inserting a command into a pane) fail with **`Connection failed: 0x80010105 — The server threw an exception`** (`RPC_E_SERVERFAULT`) reported by `wtcli`.

**Cause:** This appears to be an **operating-system-level issue in the Windows COM/WinRT cross-process activation path** (the `combase.dll` activation/marshaling path) that is **triggered by Intelligent Terminal's use of Metadata-Based Marshaling** when activating its out-of-proc COM/WinRT service. The same underlying fault can surface either as the `combase.dll` crash or as the `0x80010105` "server threw an exception" error.


**Workaround:**

- **Update Windows to the latest available cumulative update for your version** — Windows 11 22H2, 23H2, 24H2, or 25H2 — via **Settings → Windows Update**, and install all pending updates. Moving to the latest patch level resolves the crash and the `0x80010105` failures. The latest cumulative update for each Windows version is listed on the [Windows 11 release information](https://learn.microsoft.com/windows/release-health/windows11-release-information) page.
- The Intelligent Terminal team is also **actively reworking how the app communicates with the terminal**, so the issue will be mitigated independently of the OS update in a future release.
- If it still reproduces on a fully updated machine, please [file an issue](https://github.com/microsoft/intelligent-terminal/issues/new) with your Windows build number (run `winver`) and, if available, the crash dump from `%LOCALAPPDATA%\CrashDumps\`.

---

*Last updated: 2026-06-09. See the [release notes](https://github.com/microsoft/intelligent-terminal/releases) for items resolved in newer versions.*
