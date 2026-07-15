# Intelligent Terminal v0.1.1

> Base = **v0.1.0** release tag (`cfef0fa81`, build `0.1.1502.0`) → **latest `main`** (`008c4f645`).
> ~30 new PRs. The exact build number (`0.1.xxxx.0`) is injected by CI at release time.
>
> Note on the `stable` branch: `stable` is a cherry-picked subset of what already shipped in the
> previous release, so it is *behind* the release tag. The clean "since last release" base is the
> **v0.1.0 tag**, not `stable`.

This release brings smarter AI assistance, broader shell support, a smoother first-run experience,
and a batch of important stability fixes.

## ✨ New Features

1. **Added the `/fix` slash command** to diagnose and resolve a failed command on demand, so you get help even when autofix doesn't trigger automatically. #206
2. **Added a per-pane `/model` picker** so you can choose the best AI model for each task right in the agent pane, with your local choice overriding global settings. #227
3. **Extended autofix to Bash and WSL** so automatic error detection and fix suggestions now work beyond PowerShell, no matter which shell you use. #222
4. **Added the `safeUriSchemes` setting** to control which link types are clickable from the terminal, protecting you from opening risky links. #20207
5. **Redesigned the new-tab menu into a fast dropdown** so launching the profile you want is quicker than the old full-page menu. #20203
6. **Enabled winget installation** as `Microsoft.IntelligentTerminal` for simple command-line install and updates. #235

## 🔧 Improvements

1. **Enabled live hot-reload of AI model and delegate settings** so changes apply instantly without restarting the agent pane. #219
2. **Made the agent pane follow your terminal color scheme** instead of a hardcoded dark palette, giving the AI panel a consistent themed look. #241
3. **Improved the first-run setup experience** with a clearer GitHub Copilot install flow, a modal progress overlay during save/install, and a faster, optimized hooks installation step. #201, #262, #281
4. **Hardened PowerShell setup guidance** so the wizard stops and explains the fix when an execution policy blocks installation, regardless of your profile setup. #292
5. **Repositioned the `/model` and autocomplete popups** directly above the input box where you'd expect to find them. #232
6. **Preserved your font size across settings reloads** so zoom adjustments no longer reset unexpectedly. #20230
7. **Added a live preview to tab search** so you can see the selected tab while searching, making the right one easier to find. #20256
8. **Switched profile bell sounds to a more dependable playback method** so custom bells play reliably. #20031
9. **Improved accessibility** by announcing navigation-pane open/close events to screen readers. #20275
10. **Expanded in-app help** with a new FAQ, the Alt+Shift+B shortcut, log-collection instructions for bug reports, and troubleshooting for known crashes. #185, #205

## 🐛 Bug Fixes

1. **Fixed empty agent session views after first login** so the first tab's AI session reconnects and shows your conversation instead of appearing blank. #186
2. **Fixed stale session/tab state on pane close** so closing a pane or tab from the UI correctly reports the connection as closed and keeps the AI's view accurate. #208
3. **Fixed crashes and agent pane initialization errors** — including the `0x80010105` and `0xc0000005` errors — by rebuilding the AI communication layer on a more robust foundation. #237, #268
4. **Fixed AI executable path resolution** so the correct agent is always found, preventing the agent pane and autofix from silently failing. #217
5. **Fixed odd window resizing and title behavior** so the terminal ignores redundant resize requests and handles cursor/title fallback like standard terminals. #20183, #20214
6. **Removed a duplicate Default Terminal policy** from the group-policy templates so administrators no longer see a conflicting entry. #225
7. **Trimmed unnecessary data from internal event reporting** so AI command-result monitoring runs lighter and faster. #216

## 💜 Community

A huge thank you to the external contributors who helped make this release:

1. [@arkthur](https://github.com/arkthur) (Ítalo Masserano) — added the Execution Policy setting command note for both PowerShell hosts. #213
2. [@ltrzesniewski](https://github.com/ltrzesniewski) (Lucas Trzesniewski) — added the `safeUriSchemes` setting. (#20207, closes #20191)

---

## 🚀 Top 5 Elevator-Pitch Points

1. **Type `/fix` and let AI solve your failed command instantly** — on-demand error fixing right in your terminal. #206
2. **Autofix now speaks Bash, PowerShell, and WSL** — automatic error help for every shell you use. #222
3. **Pick the perfect AI model per pane with `/model`** — and hot-reload model settings live, with zero restarts. #219
4. **Rebuilt AI engine squashes the startup crashes** — no more `0x80010105` / `0xc0000005` errors when the agent pane starts. #237
5. **Install with one command** — `winget install Microsoft.IntelligentTerminal` and you're ready to go. #235

---

We'll keep iterating on bugs and feature requests rapidly while Intelligent Terminal is in this
experimental stage. Please [file issues](https://github.com/microsoft/intelligent-terminal/issues)
to help us make this product better!
