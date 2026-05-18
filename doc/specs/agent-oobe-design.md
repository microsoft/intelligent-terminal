# Agent Pane OOBE: Setup Error Guidance Design

## Problem Statement

When a user triggers **Toggle AI Assistant** (`Ctrl+Shift+.` / `openAgentPane`), they hit three failure modes with poor UX:

| Failure Mode | Current Behavior | User Sees |
|---|---|---|
| **Agent CLI not installed** (e.g., `copilot` not on PATH) | WTA spawns `copilot --acp --stdio`, OS returns "file not found" | `Error: failed to spawn agent 'copilot --acp --stdio': The system cannot find the file specified. (os error 2)` |
| **Agent CLI installed but not logged in** | Copilot CLI spawns, but ACP init/session fails with an auth error from the CLI | Generic protocol error or timeout, message varies |
| **WTA not found** | `_DetectWtaPath()` returns empty, `_OpenOrReuseAgentPane` returns silently | **Nothing happens** — completely silent failure |

All three cases fail to tell the user **what** is wrong or **how to fix it**.

---

## Approach 1: Smart Error Detection + Actionable Messages

### Concept

Keep the current flow, but add **pre-flight checks** and **error classifiers** that detect specific failure modes and replace cryptic errors with clear, actionable messages including exact install/auth commands.

### Changes

#### 1.1 Agent Registry: Add Setup Metadata

**File:** `tools/wta/src/agent_registry.rs`

Add install/auth guidance fields to `AgentProfile`:

```rust
pub struct AgentProfile {
    // ... existing fields ...

    /// Human-readable install instruction shown when binary not found.
    pub install_hint: &'static str,
    /// Command the user should run to authenticate (empty if N/A).
    pub auth_hint: &'static str,
}
```

Per-agent values:

| Agent | `install_hint` | `auth_hint` |
|---|---|---|
| copilot | `"Install: npm install -g @githubnext/github-copilot-cli\n  Info: https://github.com/github/copilot-cli"` | `"Run: gh auth login\n or: copilot auth"` |
| claude | `"Install: npm install -g @anthropic/claude-cli\n  Info: https://claude.ai/cli"` | `""` |
| codex | `"Install: npm install -g @openai/codex\n  Info: https://github.com/openai/codex"` | `""` |
| gemini | `"Install: npm install -g @google/gemini-cli\n  Info: https://github.com/google-gemini/gemini-cli"` | `""` |

#### 1.2 Pre-flight Check in `run_inner`

**File:** `tools/wta/src/protocol/acp/client.rs`

Before `Command::spawn()`, add an explicit existence check:

```rust
// Pre-flight: check if agent binary exists on PATH
if !needs_cmd {
    let found = crate::agent_registry::resolve_bare_agent_name(raw_program);
    if which::which(&found).is_err() {
        let profile = crate::agent_registry::lookup_profile(raw_program);
        return Err(anyhow::anyhow!(
            "'{}' is not installed or not on your PATH.\n\n\
             {}\n\n\
             After installing, restart your terminal and try again.",
            profile.display_name,
            profile.install_hint,
        ));
    }
}
```

#### 1.3 Auth Error Classifier

**File:** `tools/wta/src/protocol/acp/client.rs`

After spawn, monitor stderr for auth-related failures. When ACP init fails, check stderr output:

```rust
// After init timeout or init failure:
let stderr_output = collect_stderr_so_far(&mut stderr_reader);
if is_auth_error(&stderr_output) {
    let profile = crate::agent_registry::lookup_profile(raw_program);
    return Err(anyhow::anyhow!(
        "'{}' is not authenticated.\n\n\
         {}\n\n\
         After logging in, try again.",
        profile.display_name,
        profile.auth_hint,
    ));
}
```

Auth error detection heuristics (check stderr for keywords):
- `"not logged in"`, `"not authenticated"`, `"auth"`, `"login required"`
- `"401"`, `"403"`, `"unauthorized"`
- `"token"`, `"credentials"`

#### 1.4 Silent Failure → InfoBar (C++ side)

**File:** `src/cascadia/TerminalApp/TerminalPage.cpp`

When `_OpenOrReuseAgentPane` would return silently (empty cmdline or no WTA), show an InfoBar instead:

```cpp
if (cmdline.empty())
{
    // Show an info bar telling the user what to do
    _ShowAgentSetupInfoBar();
    return;
}
```

The InfoBar shows:
> **AI Assistant is not configured.** Install an agent CLI (e.g., `copilot`) and ensure it is on your PATH. [Open Settings] [Learn More]

### UX Mockups (Terminal TUI)

**Not installed:**
```
Error: 'GitHub Copilot' is not installed or not on your PATH.

  Install: npm install -g @githubnext/github-copilot-cli
     Info: https://github.com/github/copilot-cli

  After installing, restart your terminal and try again.
```

**Not authenticated:**
```
Error: 'GitHub Copilot' is not authenticated.

  Run: gh auth login
   or: copilot auth

  After logging in, try again.
```

### Pros
- Minimal code changes, no new UI concepts
- Works immediately — no new states or flows to test
- Respects the existing TUI-based error rendering
- Each error is self-contained with fix instructions

### Cons
- Still reactive (error appears after user tries to connect)
- User must leave the terminal to install/auth, then come back
- No visual "checklist" to track setup progress

---

## Approach 2: Interactive OOBE / Setup Wizard in the Agent Pane

### Concept

When the agent pane opens and prerequisites aren't met, instead of showing an error, display an **interactive setup experience** directly in the TUI. This acts as a first-run wizard that checks prerequisites, guides installation, and offers to run auth commands.

### Design: New `AppMode` — Setup

**File:** `tools/wta/src/app.rs`

Add a new application mode:

```rust
pub enum AppMode {
    Chat,       // Normal agent chat (current behavior)
    Setup,      // OOBE setup wizard
}

pub struct SetupState {
    pub agent_id: String,
    pub checks: Vec<SetupCheck>,
    pub selected_index: usize,
    pub action_in_progress: bool,
}

pub struct SetupCheck {
    pub label: String,
    pub status: CheckStatus,
    pub help_text: String,
    pub action: Option<SetupAction>,
}

pub enum CheckStatus {
    Checking,    // ⠋ Spinner
    Passed,      // ✓ Green
    Failed,      // ✗ Red
    Skipped,     // - Dim
}

pub enum SetupAction {
    /// Open a URL in the default browser
    OpenUrl(String),
    /// Run a command in a sub-shell and capture output
    RunCommand(String),
    /// Retry all checks
    RetryChecks,
}
```

### Setup Flow

```
┌─────────────────────────────────────────────────────────┐
│  AI Assistant Setup                                     │
│                                                         │
│  Agent: GitHub Copilot                                  │
│                                                         │
│  ✗ copilot CLI             Not found on PATH            │
│    Install: npm install -g @githubnext/github-copilot-cli│
│    [Press Enter to open install page]                   │
│                                                         │
│  - Authentication          (requires CLI first)         │
│                                                         │
│  ─────────────────────────────                          │
│  [R] Retry checks   [S] Open Settings   [Esc] Close    │
└─────────────────────────────────────────────────────────┘
```

After CLI is installed and user presses `[R]`:

```
┌─────────────────────────────────────────────────────────┐
│  AI Assistant Setup                                     │
│                                                         │
│  Agent: GitHub Copilot                                  │
│                                                         │
│  ✓ copilot CLI             Found at C:\...\copilot.exe  │
│                                                         │
│  ✗ Authentication          Not logged in                │
│    Run: gh auth login                                   │
│    [Press Enter to run 'copilot auth' in a new tab]     │
│                                                         │
│  ─────────────────────────                              │
│  [R] Retry checks   [S] Open Settings   [Esc] Close    │
└─────────────────────────────────────────────────────────┘
```

After auth succeeds:

```
┌─────────────────────────────────────────────────────────┐
│  AI Assistant Setup                                     │
│                                                         │
│  Agent: GitHub Copilot                                  │
│                                                         │
│  ✓ copilot CLI             Found at C:\...\copilot.exe  │
│  ✓ Authentication          Logged in as @user           │
│                                                         │
│  All checks passed! Connecting...                       │
└─────────────────────────────────────────────────────────┘
```

Then automatically transitions to `AppMode::Chat`.

### Changes

#### 2.1 Agent Registry: Add Setup Checks

**File:** `tools/wta/src/agent_registry.rs`

```rust
pub struct AgentProfile {
    // ... existing fields ...
    pub install_url: &'static str,
    pub install_command: &'static str,
    pub auth_check_command: &'static str,  // e.g., "copilot auth status"
    pub auth_command: &'static str,        // e.g., "copilot auth"
}
```

#### 2.2 Pre-flight Check Module

**File:** `tools/wta/src/preflight.rs` (new)

```rust
pub struct PreflightResult {
    pub cli_found: bool,
    pub cli_path: Option<String>,
    pub auth_ok: bool,
    pub auth_user: Option<String>,
    pub auth_error: Option<String>,
}

pub async fn check_agent(agent_id: &str) -> PreflightResult {
    let profile = agent_registry::lookup_profile(agent_id);

    // 1. Check CLI on PATH
    let cli_path = which::which(agent_id).ok();

    // 2. If CLI found, check auth status
    let auth_result = if cli_path.is_some() && !profile.auth_check_command.is_empty() {
        check_auth(profile.auth_check_command).await
    } else {
        AuthResult::Skipped
    };

    PreflightResult { ... }
}
```

#### 2.3 Setup UI

**File:** `tools/wta/src/ui/setup.rs` (new)

A new TUI view that renders the checklist and handles keyboard input (`Enter` to run actions, `R` to retry, `Esc` to close).

#### 2.4 App Integration

**File:** `tools/wta/src/app.rs`

In `App::new()`, start in `AppMode::Setup` instead of directly spawning the ACP client. Run pre-flight checks. If all pass, transition to `AppMode::Chat` and start the agent. If any fail, stay in `AppMode::Setup`.

#### 2.5 Auto-transition

When all checks pass (either on first run or after retry), automatically call `run_acp_client` and switch to `Chat` mode with a `System` message: `"Connected to GitHub Copilot. Type a message to begin."`.

### Pros
- Polished, guided experience — user doesn't need to know what to do
- Can run install/auth commands from within the terminal
- Visual progress (checkmarks) gives clear feedback
- Auto-transitions to chat when ready — no manual retry needed
- Can be extended with more checks (e.g., version compatibility, network)

### Cons
- More code to write and maintain (new module, new TUI view, new mode)
- Need to handle edge cases (partial install, auth timeout, etc.)
- Auth check commands vary by agent and may not exist for all
- Running `copilot auth` in a sub-process has its own complexity

---

## Recommendation

**Start with Approach 1** — it's the 80/20 solution that addresses the immediate pain with minimal changes. Ship it, then iterate toward Approach 2 for the full OOBE if user feedback warrants it.

Alternatively, **do both** — Approach 1 is a strict subset of Approach 2. The pre-flight checks and error messages from Approach 1 become the foundation for the Setup wizard in Approach 2.

### Priority Order

1. **P0**: Fix the silent failure when WTA is not found (C++ InfoBar)
2. **P0**: Replace `"failed to spawn agent"` with `"not installed"` + install instructions
3. **P1**: Detect auth failures and show `"not logged in"` + auth instructions
4. **P2**: Interactive setup wizard (Approach 2)

---

## Files to Modify

### Approach 1 (minimal)
| File | Change |
|---|---|
| `tools/wta/src/agent_registry.rs` | Add `install_hint`, `auth_hint` fields |
| `tools/wta/src/protocol/acp/client.rs` | Pre-flight binary check, auth error classifier |
| `src/cascadia/TerminalApp/TerminalPage.cpp` | InfoBar on silent failure |
| `src/cascadia/TerminalApp/TerminalPage.xaml` | InfoBar XAML definition |

### Approach 2 (full OOBE, additive)
| File | Change |
|---|---|
| All Approach 1 files | Same |
| `tools/wta/src/preflight.rs` | New: pre-flight check logic |
| `tools/wta/src/ui/setup.rs` | New: setup wizard TUI view |
| `tools/wta/src/app.rs` | `AppMode` enum, `SetupState`, mode transitions |
| `tools/wta/src/ui/layout.rs` | Route rendering to `setup::render` when in Setup mode |
| `tools/wta/Cargo.toml` | Add `which` crate dependency |
