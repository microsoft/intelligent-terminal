# Local Model / BYOK Manual Test Plan

## Scope

Use this checklist to verify an OpenAI-compatible Chat Completions provider, with Ollama as the local-provider example. BYOK models are currently supported by the GitHub Copilot and OpenCode agent-pane integrations.

## Quick navigation

- If the machine supports running a local model, continue with [Set up a local provider (Ollama)](#set-up-a-local-provider-ollama).
- If the machine does not support Ollama or the required local model, skip directly to [Set up a cloud provider (OpenRouter)](#set-up-a-cloud-provider-openrouter).

## Prerequisites

- Install a BYOK-capable agent CLI: GitHub Copilot or OpenCode.
- Enable shell integration and automatic error suggestions in **Settings > AI Agents**.
- Ensure `cargo` is available on `PATH`.

## Set up a local provider (Ollama)

Install and start [Ollama](https://ollama.com/), then ensure `ollama` is available on `PATH`.

Prepare the sample model:

```powershell
ollama pull qwen3.6:27b
ollama list
```

Confirm that `qwen3.6:27b` appears in the list.

### Configure the Ollama provider

1. Open Intelligent Terminal.
2. Open **Settings**.
3. Select **AI Agents**.
4. Set the agent-pane provider to **GitHub Copilot** or **OpenCode**.
5. Expand **BYOK providers**.
6. Click **Add new**.
7. Confirm that the provider form describes an **OpenAI-compatible Chat Completions API**.
8. Enter:

   | Field | Value |
   |---|---|
   | Base URL | `http://localhost:11434/v1` |
   | Model ID | `qwen3.6:27b` |
   | API key | Leave blank |

9. Click **Save**.
10. Select the new `qwen3.6:27b` BYOK model in the agent-pane model picker.
11. Save the settings and open a new terminal tab.
12. Open the agent pane.

## Set up a cloud provider (OpenRouter)

Use this path when the machine cannot run Ollama or the required local model.

1. Open Intelligent Terminal.
2. Open **Settings**.
3. Select **AI Agents**.
4. Set the agent-pane provider to **GitHub Copilot** or **OpenCode**.
5. Expand **BYOK providers**.
6. Click **Add new**.
7. Enter:

   | Field | Value |
   |---|---|
   | Base URL | `https://openrouter.ai/api/v1` |
   | Model ID | The exact OpenRouter model ID you want to test |
   | API key | Your OpenRouter API key |

8. Click **Save**.
9. Select the new BYOK model in the agent-pane model picker.
10. Save the settings and open a new terminal tab.
11. Open the agent pane.

For another hosted Chat Completions API, use the provider's `/v1` base URL, exact model ID, and API key instead. Do not put `/chat/completions` in the Base URL unless the provider explicitly requires it.

## Test cases

### TC01 - Create and run a Rust application

1. Open the agent pane.
2. Send:

   ```text
   Create a new rust application here and make sure it builds and run successfully
   ```

### TC02 - Extend the Rust application

1. In the Rust application directory, open the agent pane.
2. Send:

   ```text
   Update the existing Rust application into a number guessing game. Generate a random number from 1 to 100, accept user guesses, provide higher or lower hints, and exit when the correct number is guessed. Build the application and verify that it runs successfully.
   ```

**Expected:** The agent updates the existing application into a working number guessing game, adds any required dependency, builds it successfully, and verifies that it runs.

### TC03 - Auto-fix a Rust build error

1. Open `src\main.rs` in an editor.
2. Introduce a deliberate error by changing:

   ```rust
   fn main()
   ```

   to:

   ```rust
   fn mian()
   ```

3. Save the file.
4. In the project directory, run:

   ```powershell
   cargo build
   ```

5. Wait for auto-fix to detect the failed build.
6. Review and apply the proposed fix.
7. Run:

   ```powershell
   cargo run
   ```

**Expected:** Auto-fix identifies the misspelled `main` function, restores `fn main()`, and the guessing game builds and runs successfully.

### TC04 - Initialize Git and auto-fix a Git command

1. In the Rust application directory, initialize a Git repository:

   ```powershell
   git init
   ```

2. Run a deliberately misspelled Git command:

   ```powershell
   git statsu
   ```

3. Wait for auto-fix to detect the failed command.
4. Review and apply the proposed fix.

**Expected:** Auto-fix identifies the typo, proposes `git status`, and runs the corrected command successfully in the initialized repository.

### TC05 - Delegate a to-do list website

1. Confirm that the delegate agent is configured as **GitHub Copilot**
2. Agent pane:
   ```text
   delegate an agent to implement it as a todo list website
   ```

**Expected:** Intelligent Terminal opens a new tab and starts a normal GitHub Copilot delegate session in the current Rust project directory. The delegate must not use the local/BYOM model from the agent pane.