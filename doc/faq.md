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

1. **Make sure the prerequisites are in place.** Follow the steps in [`installing-dependencies.md`](./installing-dependencies.md) that match your agent — install Node.js LTS and the agent's own CLI (via `npm install -g …`). The ACP wrapper itself requires no install action; it will be downloaded automatically the first time the agent is launched:
   - Claude: [Steps 3.2.1 – 3.2.3](./installing-dependencies.md#32-claude-code-bring-your-own)
   - Codex: [Steps 3.3.1 – 3.3.3](./installing-dependencies.md#33-openai-codex-bring-your-own)
   - Gemini: [Section 3.4](./installing-dependencies.md#34-gemini-cli-bring-your-own)

2. **Re-install the session-tracking hooks.** Open Intelligent Terminal **Settings → Agent**, scroll to the **Agent session tracking (hooks)** row ("Track sessions across agents. Required for agent session management."), expand it, and click the **Install hooks** button next to *Install agent hook script*. This wires the newly installed CLI into Agent Management so its sessions show up in the panel.

## 4. Why does the Model dropdown take a while to populate after I change agents?

After you change the **agent** in Settings → Agent (or save a custom-command agent), the **Model** dropdown for that agent briefly shows nothing (or stays disabled), then populates a few seconds later.

This isn't a freeze — Intelligent Terminal is running a one-shot `wta probe-models` against the newly selected CLI in the background. It does a full ACP handshake (`initialize` + a throwaway `session/new`) so the agent can tell us which models it actually offers, then exits and the dropdown re-renders from the result. How long that takes is dictated by the agent's own startup and ACP response time:

- A cached / warm agent typically returns in **under 2 seconds**.
- A bring-your-own agent (Claude / Codex / Gemini) on a cold `npx` cache can take noticeably longer the first time, because `npx` has to download the ACP wrapper before the agent can even start. Subsequent probes hit the cache and are fast.
- The probe is capped at **40 seconds** total (25 s for `initialize` on `npx`-launched agents + 10 s for `session/new` + slack). If it doesn't complete in time, Intelligent Terminal falls back to a free-form model textbox and you can type a model name manually.

**Workaround:** just wait. If the dropdown is still empty after ~40 seconds, type your model name into the textbox that appears in place of the dropdown, or check `wta-probe.log` in the logs folder (use the **Report a bug (collect logs)** command palette action to grab it) to see why the probe failed.

## 5. Why doesn't Agent Management show my session on the first tab right after I install?

Immediately after installing Intelligent Terminal for the first time and selecting GitHub Copilot CLI as your agent, the **Agent Management** panel (<kbd>Ctrl+Shift+/</kbd>) may not show your active session for the very first tab you open.

**Workaround:** Either open a **second tab**, or run `/restart` inside the agent pane of the first tab. The session will then show up in Agent Management as expected.

This only affects the first tab of the first launch — subsequent tabs and subsequent app launches are unaffected.

## 6. Why is there no model picker for the delegate agent in Settings?

The Settings → Agent page exposes a **Model** dropdown for the **agent pane** agent, but there is no equivalent control for the **delegate agent** (the agent invoked by <kbd>Alt+Shift+/</kbd>, <kbd>Alt+Shift+B</kbd>, and the `?<prompt>` command-palette syntax). The delegate currently always runs against its agent CLI's default model.

**Workaround:** The underlying `delegateModel` setting exists and is honored at runtime — you can set it directly in `settings.json`:

```jsonc
{
    "delegateAgent": "copilot",
    "delegateModel": "gpt-5"   // or any model string your delegate CLI accepts
}
```

Save the file and Intelligent Terminal will pick the new value up on the next delegate launch. A Settings UI control is planned for a later release.

## 7. Why doesn't Agent Management show my delegate-agent sessions?

Open the **Agent Management** panel (<kbd>Ctrl+Shift+/</kbd>) after using the delegate agent (via <kbd>Alt+Shift+/</kbd> / <kbd>Alt+Shift+B</kbd> / `?<prompt>`). The panel lists sessions belonging to your **agent pane** agent — the delegate agent's running session is not represented.

Session tracking in this release is wired to the agent-pane lifecycle. Delegate agents run in their own tab (or in the background for `?<prompt>`) but are not yet surfaced in the management UI.

**Workaround:** Switch to the delegate's tab directly (or, for background `?<prompt>` work, watch the **Agent Status Bar** at the bottom of the window) to monitor its progress. Unified tracking of both agent-pane and delegate sessions is planned for a later release.

---

*Last updated: 2026-06-02. See the [release notes](https://github.com/microsoft/intelligent-terminal/releases) for items resolved in newer versions.*
