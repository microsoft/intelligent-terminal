# Installing dependencies for Intelligent Terminal

Intelligent Terminal's **first-run experience (FRE)** is designed to install
the dependencies it owns for you automatically — the default agent
(GitHub Copilot), Node.js when it is needed, shell integration for the shells
it supports (PowerShell, bash, and WSL bash), and so on. For most users on a
typical Windows machine who stay with the default agent, finishing the FRE is
all you ever need to do.

This document exists for the circumstances where the FRE cannot do the job
on its own, including:

- You are switching to (or adding) **Claude Code**, **OpenAI Codex**, or
  **Gemini** — agent CLIs the FRE does **not** install for you.
- An FRE step failed — for example, `winget` is missing or blocked, your
  PowerShell execution policy is locked down by Group Policy, or the
  Node.js install did not pick up on `PATH` — and you need to finish the
  installation by hand.
- You followed a **deep link to a specific step** from the FRE's manual-
  resolution guidance or from a GitHub issue. Each dependency has its own
  section with a stable anchor so it can be linked to directly.

## Table of contents

1. [WinGet (Windows Package Manager)](#1-winget-windows-package-manager)
2. [Node.js LTS — shared prerequisite](#2-nodejs-lts--shared-prerequisite)
3. [Agent CLIs](#agent-clis) — install and sign in to an agent
   - 3.1 [GitHub Copilot CLI](#31-github-copilot-cli) (installed by the FRE)
   - 3.2 [Claude Code (bring your own)](#32-claude-code-bring-your-own)
   - 3.3 [OpenAI Codex (bring your own)](#33-openai-codex-bring-your-own)
   - 3.4 [Gemini CLI (bring your own)](#34-gemini-cli-bring-your-own)
   - 3.5 [Signing in to your agent](#35-signing-in-to-your-agent)
   - 3.6 [Agent hooks for session management](#36-agent-hooks-for-session-management)
4. [Shell integration](#4-shell-integration) — supported shells + setup
   - 4.1 [PowerShell](#41-powershell) (PowerShell 7+ and Windows PowerShell 5.1)
   - 4.2 [Bash (Git Bash)](#42-bash-git-bash)
   - 4.3 [WSL bash](#43-wsl-bash)

---

## 1. WinGet (Windows Package Manager)

**Why you need it:** Intelligent Terminal can use `winget` to install
GitHub Copilot CLI and Node.js for you during the first-run experience,
depending on which agent you choose. Several sections below also use
`winget` as the recommended manual install method.

### Check whether winget is already installed

Open any PowerShell or Command Prompt window and run:

```powershell
winget --version
```

If a version number is printed (for example, `v1.8.1911`), you are done —
skip to the next section.

### Install winget from the Microsoft Store

If the command is not recognized, install the **App Installer** package from
the Microsoft Store:

> [Install App Installer (winget) from the Microsoft Store](https://apps.microsoft.com/detail/9nblggh4nns1)

After the Store finishes installing, close and reopen your terminal so the
new `winget.exe` is picked up on `PATH`, then verify with
`winget --version` again.

> [!TIP]
> WinGet ships in-box on Windows 11 and on modern Windows 10 builds. If the
> Microsoft Store link above does not work — for example, on a locked-down
> or Store-disabled machine — you have two fallbacks:
>
> - **Sideload the latest release from GitHub.** Download the
>   `Microsoft.DesktopAppInstaller_*.msixbundle` from the
>   [winget-cli releases page](https://github.com/microsoft/winget-cli/releases/latest)
>   and install it with
>   `Add-AppxPackage -Path .\Microsoft.DesktopAppInstaller_*.msixbundle`.
>   The same page lists the prerequisite VC++ and UI XAML packages if your
>   machine is missing them.
> - **Ask your IT administrator** to deploy the
>   **Microsoft.DesktopAppInstaller** package via Intune, Configuration
>   Manager, or Group Policy.

---

## 2. Node.js LTS — shared prerequisite

**Why you need it:** Claude Code, OpenAI Codex, and Gemini CLI are all
distributed as npm packages. Intelligent Terminal also launches Claude and
Codex through `npx` wrappers
(`npx -y @agentclientprotocol/claude-agent-acp` and
`npx -y @zed-industries/codex-acp`), which require a working Node.js +
`npm` + `npx` toolchain on `PATH`. You can skip this section if you only
plan to use GitHub Copilot CLI.

### Check whether Node.js is already installed

```powershell
node --version
npm --version
```

Both commands should print a version number. Intelligent Terminal targets
whichever version `winget` installs from the **OpenJS.NodeJS.LTS** package
(i.e. the current Node.js LTS line).

### Install Node.js LTS with winget

```powershell
winget install --id OpenJS.NodeJS.LTS --exact --silent `
  --source winget `
  --accept-source-agreements --accept-package-agreements `
  --disable-interactivity
```

This matches the command the Intelligent Terminal first-run experience runs
when it detects that Node.js is missing — you only need to run it manually
if you are setting up a machine outside the FRE flow. After the install
finishes, close and reopen your terminal so `PATH` picks up `node.exe`,
`npm.cmd`, and `npx.cmd`.

---

## Agent CLIs

Intelligent Terminal supports four agents out of the box — **GitHub
Copilot**, **Claude Code**, **OpenAI Codex**, and **Gemini**. The
first-run experience installs **GitHub Copilot** (the default) for you;
the other three are **bring-your-own** — install the CLI yourself
(sub-sections below) before selecting it in the FRE.

Intelligent Terminal talks to all four through the
[**Agent Control Protocol (ACP)**](https://agentclientprotocol.com/get-started/agents).
**Copilot** and **Gemini** speak ACP natively, so no extra layer is
required. **Claude Code** and **OpenAI Codex** do not speak ACP directly
— Intelligent Terminal launches them through an `npx` wrapper that is
fetched on demand at run time, so its only prerequisite is Node.js.

> [!NOTE]
> **Bringing your own ACP agent.** Any CLI that speaks ACP can also be
> wired up from **Settings → AI Agents → Add custom agent**. Custom
> agents work in the agent pane today, but **session management** (the
> multi-session sidebar in the agent pane) is not yet supported for
> custom agents — only the four built-in agents above get the full
> session experience.

### 3.1 GitHub Copilot CLI

**Why you need it:** GitHub Copilot is the **default agent** in Intelligent
Terminal and the only agent the first-run experience installs on your
behalf. It powers the agent pane, the `?<prompt>` command-palette
delegation, and the auto-fix workflow.

#### Installed automatically by the first-run experience

When you complete the FRE with Copilot selected, Intelligent Terminal
installs the Copilot CLI for you (skipped if it is already on `PATH`):

```powershell
winget install --id GitHub.Copilot --exact --silent `
  --source winget `
  --accept-source-agreements --accept-package-agreements `
  --disable-interactivity
```

Copilot speaks ACP natively, so no wrapper is required.

#### Install manually (only if you are not using the FRE)

If the FRE did not install Copilot CLI for you, run the `winget` command
above, then verify:

```powershell
copilot --version
```

If the command is not found, close and reopen your terminal so the new
install directory is added to `PATH`.

> [!IMPORTANT]
> After installing, you must sign in before the agent will respond. See
> [Section 3.5 — Signing in to your agent](#35-signing-in-to-your-agent).

---

### 3.2 Claude Code (bring your own)

**Status:** Supported, but **not installed by Intelligent Terminal**. You
must install Anthropic's Claude Code CLI yourself before selecting Claude
in the FRE. Intelligent Terminal launches Claude through the
`@agentclientprotocol/claude-agent-acp` npx wrapper.

#### Step 3.2.1 — Install Node.js

Complete [Section 2 — Node.js LTS](#2-nodejs-lts--shared-prerequisite)
first. Claude Code is an npm package and cannot run without Node.js. If you
have not yet installed Node.js, the FRE will install it for you the first
time you select Claude.

#### Step 3.2.2 — Install the Claude Code CLI

```powershell
npm install -g @anthropic-ai/claude-code
```

Verify:

```powershell
claude --version
```

#### Step 3.2.3 — ACP wrapper (no install action required)

Claude Code does not speak the Agent Control Protocol (ACP) directly, so
Intelligent Terminal launches it through the
[`@agentclientprotocol/claude-agent-acp`](https://www.npmjs.com/package/@agentclientprotocol/claude-agent-acp)
wrapper. The wrapper is fetched on demand at run time with:

```powershell
npx -y @agentclientprotocol/claude-agent-acp
```

You do **not** need to install anything for this — the only prerequisite
is a working Node.js + `npx` (which you already installed in Step 3.2.1).
The first launch may take a few seconds while `npx` downloads the wrapper.

> [!IMPORTANT]
> After installing, you must sign in before the agent will respond. See
> [Section 3.5 — Signing in to your agent](#35-signing-in-to-your-agent).

---

### 3.3 OpenAI Codex (bring your own)

**Status:** Supported, but **not installed by Intelligent Terminal**. You
must install OpenAI's Codex CLI yourself before selecting Codex in the FRE.
Intelligent Terminal launches Codex through the
`@zed-industries/codex-acp` npx wrapper.

#### Step 3.3.1 — Install Node.js

Complete [Section 2 — Node.js LTS](#2-nodejs-lts--shared-prerequisite)
first. Codex is an npm package and cannot run without Node.js. If you have
not yet installed Node.js, the FRE will install it for you the first time
you select Codex.

#### Step 3.3.2 — Install the Codex CLI

```powershell
npm install -g @openai/codex
```

Verify:

```powershell
codex --version
```

#### Step 3.3.3 — ACP wrapper (no install action required)

Codex does not speak the Agent Control Protocol (ACP) directly, so
Intelligent Terminal launches it through the
[`@zed-industries/codex-acp`](https://www.npmjs.com/package/@zed-industries/codex-acp)
wrapper. The wrapper is fetched on demand at run time with:

```powershell
npx -y @zed-industries/codex-acp
```

You do **not** need to install anything for this — the only prerequisite
is a working Node.js + `npx` (which you already installed in Step 3.3.1).
The first launch may take a few seconds while `npx` downloads the wrapper.

> [!IMPORTANT]
> After installing, you must sign in before the agent will respond. See
> [Section 3.5 — Signing in to your agent](#35-signing-in-to-your-agent).

---

### 3.4 Gemini CLI (bring your own)

**Status:** Supported, but **not installed by Intelligent Terminal**. You
must install Google's Gemini CLI yourself before selecting Gemini in the
FRE. Gemini speaks the Agent Control Protocol (ACP) natively, so no
wrapper is required at runtime, but the CLI itself is still distributed as
an npm package.

#### Step 3.4.1 — Install Node.js

Complete [Section 2 — Node.js LTS](#2-nodejs-lts--shared-prerequisite)
first.

#### Step 3.4.2 — Install the Gemini CLI

```powershell
npm install -g @google/gemini-cli
```

Verify:

```powershell
gemini --version
```

Gemini speaks ACP natively, so no wrapper is required.

> [!IMPORTANT]
> Gemini requires you to sign in with your Google account before it will
> respond. See [Section 3.5 — Signing in to your agent](#35-signing-in-to-your-agent).

---

### 3.5 Signing in to your agent

Installing an agent's CLI is not enough — you must also sign in before
Intelligent Terminal can talk to it. Pick the row for the agent you
installed:

| Agent             | Sign-in command                                            | Official docs |
|-------------------|------------------------------------------------------------|---------------|
| GitHub Copilot    | `copilot login`                                            | [GitHub Copilot CLI docs](https://docs.github.com/en/copilot/how-tos/use-copilot-agents/use-copilot-in-the-cli) |
| Claude Code       | `claude login`                                             | [Claude Code setup](https://docs.claude.com/en/docs/claude-code/setup) |
| OpenAI Codex      | `codex auth` *(or set the `OPENAI_API_KEY` environment variable)* | [OpenAI Codex CLI docs](https://developers.openai.com/codex/cli/) |
| Gemini CLI        | Run `gemini` once — it opens a browser to sign in with your Google account *(or set the `GEMINI_API_KEY` environment variable)* | [Gemini CLI authentication](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/authentication.md) |

After signing in, restart Intelligent Terminal once so the agent pane picks
up the new credentials.

---

### 3.6 Agent hooks for session management

**Why you need it:** Intelligent Terminal's **session management** feature
— the multi-session sidebar in the agent pane that lets you switch between
parallel conversations on the same tab — is powered by a small bundle of
**agent hooks** (`wt-agent-hooks`) that the agent CLI loads as a plugin /
extension. The first-run experience installs these hooks for you the
first time you save the FRE with **Session management** enabled.

You only need to follow this section if:

- The FRE reported that hooks installation failed (for example because
  the agent CLI was not yet on `PATH`, or your network blocked the agent
  CLI's plugin store).
- You installed the agent CLI **after** completing the FRE and now want
  to enable session management.

#### Step 3.6.1 — Make sure the agent CLI is installed and on PATH

The hooks are registered through the agent CLI's own plugin / extension
system, so the CLI itself must be installed first. Complete the matching
sub-section above before continuing:

- **GitHub Copilot** → [Section 3.1](#31-github-copilot-cli)
- **Claude Code** → [Section 3.2](#32-claude-code-bring-your-own)
- **OpenAI Codex** → [Section 3.3](#33-openai-codex-bring-your-own)
- **Gemini CLI** → [Section 3.4](#34-gemini-cli-bring-your-own)

Verify the CLI you intend to use prints a version number, then close and
reopen your terminal so any new install directory is on `PATH`.

#### Step 3.6.2 — Run `wta hooks install`

Intelligent Terminal ships a `wta hooks install` command that runs the
same installer the FRE uses. Open a **new** Intelligent Terminal window
(so `wta` and the agent CLI both pick up the current `PATH`) and run
**one** of the following — whichever matches the agent you selected in
the FRE:

```powershell
wta hooks install --cli copilot
wta hooks install --cli claude
wta hooks install --cli codex
wta hooks install --cli gemini
```

Or install for every agent CLI that is currently on `PATH` in one go:

```powershell
wta hooks install
```

Under the hood this runs each agent CLI's native plugin / extension
command against the `wt-agent-hooks` bundle shipped inside the
Intelligent Terminal package:

| Agent          | Native commands invoked by `wta hooks install`                                                                              |
|----------------|-----------------------------------------------------------------------------------------------------------------------------|
| GitHub Copilot | `copilot plugin marketplace add <bundle>\copilot` then `copilot plugin install wt-agent-hooks@wt-local`                     |
| Claude Code    | `claude plugin marketplace add <bundle>\claude` then `claude plugin install wt-agent-hooks@wt-local`                        |
| OpenAI Codex   | `codex plugin marketplace add <bundle>\codex` then `codex plugin add wt-agent-hooks@wt-local` *(note: `add`, not `install`)* |
| Gemini CLI     | `gemini extensions install <bundle>\gemini-extension --consent --skip-settings` *(with `GEMINI_CLI_TRUST_WORKSPACE=true` to bypass Gemini's folder-trust prompt)* |

You do not need to run these directly — `wta hooks install` is the
supported entry point and handles bundle staging, idempotency, and
diagnostic logging for you.

> [!IMPORTANT]
> **Codex needs a one-time `/hooks` trust step.** After
> `wta hooks install --cli codex` succeeds, hook events will not fire
> until you launch Codex once and run the `/hooks` slash command to
> trust the `wt-agent-hooks` plugin. This is a Codex security
> requirement that no external installer can satisfy on your behalf;
> the other three CLIs do not need this step.

#### Step 3.6.3 — Verify the install and re-enable session management

Check the status report to confirm each agent CLI is wired up:

```powershell
wta hooks status
```

You should see `installed` for the agent you selected. Then, back in
**Settings → AI Agents**, turn **Session management** back on (the FRE
turns it off when hooks installation fails so you can save and continue
without it) and restart Intelligent Terminal once.

> [!TIP]
> If `wta hooks install` still fails, diagnostics are written to
> `wta-install-hooks.log` under the Intelligent Terminal package's log
> directory:
>
> ```text
> %LOCALAPPDATA%\Packages\<PackageFamilyName>\LocalCache\Local\IntelligentTerminal\logs\<version>\wta-install-hooks.log
> ```
>
> The most common causes of failure are: the agent CLI was not on
> `PATH` when `wta` ran (open a fresh window and retry), or the agent
> CLI has not been signed in yet (see
> [Section 3.5](#35-signing-in-to-your-agent)). The log identifies
> which step failed and prints the exit code from the underlying
> `plugin install` / `plugin add` / `extensions install` invocation.

> [!WARNING]
> If your organization disables agent session hooks through Group
> Policy, the **Session management** toggle in the FRE and in
> **Settings → AI Agents** is locked off and the FRE will not run the
> hooks installer for you. Contact your IT administrator if you
> believe this is in error.

---

## 4. Shell integration

**Why you need it:** Shell integration teaches your shell to emit **OSC 133**
marks after every prompt. Intelligent Terminal uses these marks to detect
command boundaries and exit codes, which powers the auto-fix feature, command
navigation, and the bottom-bar agent state. Without these marks Intelligent
Terminal cannot tell when a command finished or whether it failed.

The first-run experience installs shell integration automatically for every
**supported shell it detects** on your system — you usually don't have to do
anything. This section documents which shells are supported and the only
manual step you may occasionally need (adjusting the PowerShell execution
policy).

### What's supported

| Shell | Supported | Where integration is installed |
|---|---|---|
| PowerShell 7+ (`pwsh.exe`) | ✅ | `$PROFILE` (see [4.1](#41-powershell)) |
| Windows PowerShell 5.1 (`powershell.exe`) | ✅ | `$PROFILE` (see [4.1](#41-powershell)) |
| Bash — Git Bash on Windows | ✅ | `~/.bashrc` (see [4.2](#42-bash-git-bash)) |
| WSL bash | ✅ | `\\wsl$\<distro>\…\.bashrc` (see [4.3](#43-wsl-bash)) |
| Command Prompt (`cmd.exe`) | ❌ | `cmd.exe` cannot emit OSC 133 marks |
| Other shells (zsh, fish, Nushell, …) | ❌ | Not yet supported |
| Non-bash WSL login shells (zsh, fish, …) | ❌ | Only WSL distros whose login shell is bash are covered |

> [!NOTE]
> The set of supported shells grows over time. This page is the **authoritative
> list** — if a shell you use isn't here yet, check back after updating
> Intelligent Terminal.

### 4.1 PowerShell

The first-run experience writes the shell-integration profile snippet for you,
for both **PowerShell 7+** (`pwsh.exe`) and **Windows PowerShell 5.1**
(`powershell.exe`). The snippet is appended to each host's
**current-user / current-host profile** — the same file `$PROFILE` (also
known as `$PROFILE.CurrentUserCurrentHost`) points at when you run that
host:

| PowerShell host | Profile path |
|---|---|
| PowerShell 7+ (`pwsh.exe`) | `$HOME\Documents\PowerShell\Microsoft.PowerShell_profile.ps1` |
| Windows PowerShell 5.1 (`powershell.exe`) | `$HOME\Documents\WindowsPowerShell\Microsoft.PowerShell_profile.ps1` |

You can always resolve the exact location from inside either host with:

```powershell
$PROFILE.CurrentUserCurrentHost
```

The only step you may need to perform by hand is adjusting the PowerShell
execution policy so the profile is allowed to run.

#### Set the PowerShell execution policy

Shell-integration scripts are PowerShell `.ps1` files loaded from your
profile. PowerShell will refuse to run them under the default `Restricted`
or `AllSigned` execution policies. Lower the policy for the current user
to **at least** `RemoteSigned`:

```powershell
Set-ExecutionPolicy -Scope CurrentUser -ExecutionPolicy RemoteSigned
```

> [!IMPORTANT]
> Make sure to run this command in **both** hosts:
>
> - **PowerShell 7+** (`pwsh.exe`)
> - **Windows PowerShell 5.1** (`powershell.exe`)

`RemoteSigned` allows local scripts (such as your profile) to run while
still requiring a signature on scripts downloaded from the internet, which
is the recommended Microsoft default for developer machines.

> [!TIP]
> **Symptom that tells you this is the problem.** When a new
> PowerShell session starts, you see an error similar to:
>
> ```text
> . : File <path>\Microsoft.PowerShell_profile.ps1 cannot be loaded
> because running scripts is disabled on this system.
> ...
>     + CategoryInfo          : SecurityError: (:) [], PSSecurityException
>     + FullyQualifiedErrorId : UnauthorizedAccess
> ```
>
> The key phrases to search for are **"running scripts is disabled on this
> system"** and **`UnauthorizedAccess`** — both indicate the execution
> policy is blocking your profile, and the `Set-ExecutionPolicy` command
> above is the fix.

> [!WARNING]
> If your organization enforces execution policy through Group Policy,
> `Set-ExecutionPolicy` will succeed but the policy will not change.
> Contact your IT administrator if `Get-ExecutionPolicy -List` still
> shows `Restricted` or `AllSigned` for your scope after running the
> command above.

### 4.2 Bash (Git Bash)

For **Git Bash on Windows**, the first-run experience installs a small,
versioned script under `~/.intelligent-terminal/` (resolved from your
`%USERPROFILE%`) and sources it from a guarded block appended to **`~/.bashrc`**:

| What | Path |
|---|---|
| Integration script | `%USERPROFILE%\.intelligent-terminal\shell-integration_v1.sh` |
| Sourced from | `%USERPROFILE%\.bashrc` |

There is **no execution-policy equivalent** for bash, so no manual step is
normally required. A few notes:

- The script targets **bash 3.2+** and is safe to source multiple times. It
  silently no-ops in non-interactive shells and in non-bash shells, so a
  `.bashrc` that roams (via dotfiles / OneDrive) to a machine without
  Intelligent Terminal won't break.
- To uninstall, remove the Intelligent Terminal block from `~/.bashrc`. The
  versioned script files under `~/.intelligent-terminal/` are intentionally
  left in place to support side-by-side rollback; delete that directory by
  hand if you want a full sweep.

### 4.3 WSL bash

For **WSL** distributions, integration is installed **per distro** using the
same bash script as above, written through the distro's `\\wsl$\` UNC mount:

| What | Path |
|---|---|
| Integration script | `\\wsl$\<distro>\<home>\.intelligent-terminal\shell-integration_v1.sh` |
| Sourced from | `\\wsl$\<distro>\<home>\.bashrc` |

Notes and limitations:

- The `\\wsl$\` mount requires **Windows 10 1903 (build 18362)+**, which every
  supported Intelligent Terminal host meets. The first access auto-starts the
  distro, so the very first install pays a one-time VM cold-start cost.
- Only distros whose **login shell is bash** are covered (integration is
  written to `~/.bashrc`). Distros that default to — or where you have
  changed your default shell to — **zsh, fish, or another non-bash shell**,
  or that ship **without bash** (for example, Alpine), are **not yet
  supported**.

### Enable auto-error detection and auto-error fix

Once shell integration is in place (and, for PowerShell, the execution policy
is set), open **Settings → AI Agents** inside Intelligent Terminal and turn on
**Auto-error detection** (and, optionally, the auto-fix follow-up). With shell
integration loading correctly, the agent pane will now:

- Detect failing commands automatically (via the OSC 133 exit-code marks).
- Offer to diagnose and propose a fix for the most recent failure.

If anything is not behaving as expected, see Microsoft's
[Shell integration in Windows Terminal](https://learn.microsoft.com/en-us/windows/terminal/tutorials/shell-integration)
tutorial for the full walkthrough of OSC 133 marks, the profile snippet,
and per-shell troubleshooting.

---

## Related documentation

- [Shell integration in Windows Terminal](https://learn.microsoft.com/en-us/windows/terminal/tutorials/shell-integration)
  — Microsoft Learn tutorial covering OSC 133 marks, the per-shell profile
  snippet, and troubleshooting.
- [Windows Terminal documentation](https://learn.microsoft.com/windows/terminal/)
  — base product documentation that Intelligent Terminal builds on.
