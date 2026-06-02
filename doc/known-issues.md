# Known Issues

This page lists known issues in the current release of Intelligent Terminal along with their workarounds. If you hit something that isn't covered here, please [file an issue](https://github.com/microsoft/intelligent-terminal/issues).

## 1. Session management may not work on the very first tab after install

**Symptom:** Immediately after installing Intelligent Terminal for the first time and selecting GitHub Copilot CLI as your agent, the **Agent Management** panel (<kbd>Ctrl+Shift+/</kbd>) may not show your active session for the very first tab you open.

**Workaround:** Either open a **second tab**, or run `/restart` inside the agent pane of the first tab. The session will then show up in Agent Management as expected.

This only affects the first tab of the first launch — subsequent tabs and subsequent app launches are unaffected.

## 2. First Run Experience (FRE) install can be slow or fail on poor networks

**Symptom:** The first-run setup uses [`winget`](https://learn.microsoft.com/windows/package-manager/winget/) to install your chosen agent CLI (e.g. GitHub Copilot CLI, Claude, Gemini). On slow, throttled, or unreliable networks this step can take **more than 10 minutes**, and on intermittent connections it can fail outright.

**Workaround:**

- Make sure you're on a stable, unrestricted internet connection before running the FRE.
- If the FRE fails or times out, you can install the agent CLI manually following [`installing-dependencies.md`](./installing-dependencies.md), then re-open Intelligent Terminal — the FRE will detect the already-installed CLI and skip the download step.

## 3. Windows 11 only in the current release

**Symptom:** Intelligent Terminal will not install on Windows 10.

**Why:** The package manifest sets `MinVersion="10.0.22621.6060"` (Windows 11 22H2), so the MSIX install is blocked on earlier OS builds.

**Workaround:** Use Windows 11 (22H2 or later). Windows 10 support is planned for a later release.

## 4. Installing a new agent CLI after the FRE doesn't auto-install the ACP wrapper / hooks

**Symptom:** You completed the FRE with one agent (say, Copilot), then later installed Claude or Codex (or another ACP-compatible CLI) and switched the **agent pane** to it in Settings. The agent pane appears not to work, or **Agent Management** doesn't track its sessions.

**Why:** The FRE only sets up the ACP wrapper and session-tracking hooks for the agents you went through it with. Agents installed *after* the FRE need a one-time manual setup.

**Workaround:**

1. **Install the ACP wrapper for the new agent.** Follow the steps in [`installing-dependencies.md`](./installing-dependencies.md) that match your agent:
   - Claude: [Step 3.2.3 — ACP wrapper (no install action required)](./installing-dependencies.md#step-323--acp-wrapper-no-install-action-required)
   - Codex (or other): [Step 3.3.3 — ACP wrapper (no install action required)](./installing-dependencies.md#step-333--acp-wrapper-no-install-action-required)

2. **Re-install the session-tracking hooks.** Open Intelligent Terminal **Settings → Agent**, scroll to the **Agent session tracking (hooks)** row ("Track sessions across agents. Required for agent session management."), expand it, and click the **Install hooks** button next to *Install agent hook script*. This wires the newly installed CLI into Agent Management so its sessions show up in the panel.

---

*Last updated: 2026-06-02. See the [release notes](https://github.com/microsoft/intelligent-terminal/releases) for issues fixed in newer versions.*
