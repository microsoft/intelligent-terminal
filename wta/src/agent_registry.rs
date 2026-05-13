// ─── Unified Agent Registry ──────────────────────────────────────────────────
//
// Single source of truth for everything about each supported agent CLI:
// executable resolution, ACP server flags, delegate prompt delivery, display
// names, model selection, and authentication flow.
//
// To add a new agent, just add an entry to KNOWN_AGENTS below.

/// How the agent CLI accepts a startup prompt in delegate mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptFlag {
    /// Flag before the prompt string, e.g. `-i "prompt"`.
    Flag(&'static str),
    /// Prompt is a bare positional argument, e.g. `codex "prompt"`.
    Positional,
}

/// How the agent handles authentication in ACP mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpAuthFlow {
    /// No ACP support (delegate-only agent).
    None,
    /// ACP supported; auth is handled externally (e.g. `gh auth login`).
    External,
    /// ACP supported; requires in-protocol OAuth/API-key auth.
    InProtocol,
}

/// Complete profile for a known agent CLI.
#[derive(Debug, Clone)]
pub struct AgentProfile {
    /// Short lowercase identifier, e.g. "copilot", "claude".
    pub id: &'static str,
    /// Human-friendly display name, e.g. "GitHub Copilot".
    pub display_name: &'static str,

    // ── CLI resolution ──
    /// Preferred extension search order when resolving a bare name on PATH.
    pub exe_search_order: &'static [&'static str],

    // ── ACP server mode ──
    /// Flags that put this agent into ACP mode (e.g. `["--acp", "--stdio"]`).
    /// Empty slice means the agent's own CLI does not speak ACP — but
    /// `acp_launch_command` may still provide a wrapper that does.
    pub acp_flags: &'static [&'static str],
    /// Override command for ACP mode. When non-empty, this is the full
    /// commandline used to spawn the ACP server (e.g.
    /// `"npx -y @zed-industries/claude-code-acp"` for an adapter package).
    /// When empty, `build_acp_command` falls back to `id + acp_flags`.
    pub acp_launch_command: &'static str,
    /// Authentication flow required for ACP sessions.
    pub acp_auth_flow: AcpAuthFlow,

    // ── Delegate mode ──
    /// How the agent CLI accepts a startup prompt.
    pub delegate_prompt_flag: PromptFlag,

    // ── Model selection ──
    /// Flag names used to specify a model (e.g. `["--model", "-m"]`).
    /// Empty slice means the agent doesn't support model selection.
    pub model_flags: &'static [&'static str],

    // ── Setup / OOBE ──
    /// Human-readable install instructions shown when binary is not found.
    pub install_hint: &'static str,
    /// URL for the agent's install page / documentation.
    pub install_url: &'static str,
    /// Command to check auth status (empty if N/A). Exit 0 = authenticated.
    pub auth_check_command: &'static str,
    /// Human-readable auth instructions shown when not logged in.
    pub auth_hint: &'static str,
    /// Flag the CLI uses to resume a session, e.g. `"--resume"` for Claude.
    /// Empty when resume is unsupported.
    pub resume_flag: &'static str,
}

// ─── Registry ────────────────────────────────────────────────────────────────

pub const KNOWN_AGENTS: &[AgentProfile] = &[
    AgentProfile {
        id: "copilot",
        display_name: "GitHub Copilot",
        exe_search_order: &[".exe", ".cmd"],
        acp_flags: &["--acp", "--stdio"],
        acp_launch_command: "",
        acp_auth_flow: AcpAuthFlow::External,
        delegate_prompt_flag: PromptFlag::Flag("-i"),
        model_flags: &["--model", "-m"],
        install_hint: "npm install -g @github/copilot",
        install_url: "https://github.com/github/copilot-cli",
        auth_check_command: "",
        auth_hint: "Run 'copilot' to launch the CLI, then type /login to sign in.",
        resume_flag: "--resume",
    },
    AgentProfile {
        id: "claude",
        display_name: "Claude",
        exe_search_order: &[".exe", ".cmd"],
        acp_flags: &[],
        // Claude CLI itself doesn't speak ACP. We launch the Zed-maintained
        // adapter via npx; npm-installed `claude` shim implies node/npx
        // are present, so this works whenever delegate mode does.
        acp_launch_command: "npx -y @zed-industries/claude-code-acp",
        acp_auth_flow: AcpAuthFlow::External,
        delegate_prompt_flag: PromptFlag::Positional,
        model_flags: &[],
        install_hint: "npm install -g @anthropic-ai/claude-code",
        install_url: "https://docs.anthropic.com/en/docs/claude-code",
        auth_check_command: "",
        auth_hint: "Run: claude login",
        resume_flag: "--resume",
    },
    AgentProfile {
        id: "codex",
        display_name: "Codex",
        exe_search_order: &[".exe", ".cmd"],
        acp_flags: &[],
        // Codex CLI itself doesn't speak ACP. Same npx-adapter pattern as Claude.
        acp_launch_command: "npx -y @zed-industries/codex-acp",
        acp_auth_flow: AcpAuthFlow::External,
        delegate_prompt_flag: PromptFlag::Positional,
        model_flags: &[],
        install_hint: "npm install -g @openai/codex",
        install_url: "https://github.com/openai/codex",
        auth_check_command: "",
        resume_flag: "",
        auth_hint: "Run: codex auth (or set OPENAI_API_KEY)",
    },
    AgentProfile {
        id: "gemini",
        display_name: "Gemini",
        exe_search_order: &[".exe", ".cmd"],
        acp_flags: &["--experimental-acp"],
        acp_launch_command: "",
        acp_auth_flow: AcpAuthFlow::InProtocol,
        delegate_prompt_flag: PromptFlag::Positional,
        model_flags: &["--model", "-m"],
        install_hint: "npm install -g @anthropic-ai/gemini-cli\n  or: pip install gemini-cli",
        install_url: "https://github.com/google-gemini/gemini-cli",
        auth_check_command: "",
        auth_hint: "Authentication is handled in-protocol during connection.",
        resume_flag: "--resume",
    },
];

pub const DEFAULT_PROFILE: AgentProfile = AgentProfile {
    id: "unknown",
    display_name: "Agent",
    exe_search_order: &[".exe", ".cmd"],
    acp_flags: &[],
    acp_launch_command: "",
    acp_auth_flow: AcpAuthFlow::None,
    delegate_prompt_flag: PromptFlag::Flag("-i"),
    model_flags: &["--model", "-m"],
    install_hint: "",
    install_url: "",
    auth_check_command: "",
    auth_hint: "",
    resume_flag: "",
};

/// Default ACP command used when no agent is configured.
pub const DEFAULT_ACP_COMMAND: &str = "copilot --acp --stdio";

// ─── Lookup ──────────────────────────────────────────────────────────────────

/// Look up an agent profile by executable name.
/// Strips path separators and extensions before matching.
pub fn lookup_profile(executable: &str) -> &'static AgentProfile {
    let basename = executable
        .rsplit(|ch: char| ch == '\\' || ch == '/')
        .next()
        .unwrap_or(executable);
    let lower = basename
        .strip_suffix(".exe")
        .or_else(|| basename.strip_suffix(".cmd"))
        .or_else(|| basename.strip_suffix(".bat"))
        .unwrap_or(basename)
        .to_ascii_lowercase();
    KNOWN_AGENTS
        .iter()
        .find(|p| p.id == lower)
        .unwrap_or(&DEFAULT_PROFILE)
}

/// Look up an agent profile by id.
pub fn lookup_profile_by_id(id: &str) -> &'static AgentProfile {
    KNOWN_AGENTS
        .iter()
        .find(|p| p.id == id)
        .unwrap_or(&DEFAULT_PROFILE)
}

// ─── ACP Command Building ────────────────────────────────────────────────────

/// Build the full ACP agent command from an agent id and optional model.
/// E.g. `build_acp_command("copilot", Some("gpt-5"))` → `"copilot --acp --stdio --model gpt-5"`.
/// For agents whose CLI doesn't speak ACP natively (claude, codex), this
/// returns the adapter launch command instead — e.g.
/// `build_acp_command("claude", None)` → `"npx -y @zed-industries/claude-code-acp"`.
pub fn build_acp_command(agent_id: &str, model: Option<&str>) -> String {
    let profile = lookup_profile_by_id(agent_id);

    // Adapter-style launch (e.g. claude, codex via npx). The adapter doesn't
    // accept --model on the command line — model is sent via ACP setSessionModel
    // after handshake — so we ignore the `model` arg here.
    if !profile.acp_launch_command.is_empty() {
        let _ = model;
        return profile.acp_launch_command.to_string();
    }

    let mut parts = vec![agent_id.to_string()];
    for flag in profile.acp_flags {
        parts.push(flag.to_string());
    }
    if let Some(model) = model {
        if let Some(flag) = profile.model_flags.first() {
            parts.push(flag.to_string());
            parts.push(model.to_string());
        }
    }
    parts.join(" ")
}

// ─── ACP Flag Stripping ─────────────────────────────────────────────────────

/// Given an ACP agent commandline like `"copilot --acp --stdio --model gpt-5"`,
/// strip ACP-specific flags to produce a clean delegate commandline,
/// preserving model flags.  Returns `None` if the command is not a known ACP agent.
///
/// For adapter-style launches (e.g. `"npx -y @zed-industries/claude-code-acp"`),
/// returns the bare agent id (e.g. `"claude"`) — delegate mode invokes the
/// agent's own CLI directly, not the ACP adapter.
pub fn strip_acp_flags_for_delegate(agent_cmd: &str) -> Option<String> {
    let tokens = crate::coordinator::split_windows_commandline(agent_cmd);
    let command = tokens.first()?;

    // Adapter-style: input is something like "npx -y @zed/claude-code-acp".
    // Find which agent owns this launch command and return its bare id.
    let trimmed = agent_cmd.trim();
    if let Some(profile) = KNOWN_AGENTS
        .iter()
        .find(|p| !p.acp_launch_command.is_empty() && p.acp_launch_command == trimmed)
    {
        return Some(profile.id.to_string());
    }

    let profile = lookup_profile(command);
    if profile.acp_flags.is_empty() {
        return None; // Not an ACP agent, nothing to strip.
    }

    let mut args = vec![command.as_str()];
    let model = extract_model_from_token_slice(&tokens[1..], profile);
    if let Some(model) = model.as_deref() {
        if let Some(flag) = profile.model_flags.first() {
            args.push(flag);
            args.push(model);
        }
    }
    Some(crate::coordinator::join_windows_commandline(&args))
}

// ─── Model Extraction ────────────────────────────────────────────────────────

/// Extract a model value from string-slice args using the profile's model flags.
pub fn extract_model_from_args<'a>(args: &'a [&'a str], profile: &AgentProfile) -> Option<&'a str> {
    if profile.model_flags.is_empty() {
        return None;
    }
    let mut iter = args.iter().copied();
    while let Some(arg) = iter.next() {
        if profile.model_flags.contains(&arg) {
            if let Some(value) = iter.next() {
                let trimmed = value.trim_matches(|ch| ch == '"' || ch == '\'');
                if !trimmed.is_empty() {
                    return Some(trimmed);
                }
            }
            continue;
        }
        for flag in profile.model_flags {
            if let Some(value) = arg.strip_prefix(&format!("{}=", flag)) {
                let trimmed = value.trim_matches(|ch| ch == '"' || ch == '\'');
                if !trimmed.is_empty() {
                    return Some(trimmed);
                }
            }
        }
    }
    None
}

/// Same as `extract_model_from_args` but for `&[String]` slices (used by coordinator).
fn extract_model_from_token_slice(args: &[String], profile: &AgentProfile) -> Option<String> {
    let str_args: Vec<&str> = args.iter().map(String::as_str).collect();
    extract_model_from_args(&str_args, profile).map(str::to_string)
}

// ─── Executable Resolution ───────────────────────────────────────────────────

/// Resolve a bare agent name (e.g. "claude") to the concrete executable found
/// on PATH (e.g. "claude.exe") using the agent's preferred search order.
/// Returns the input unchanged if it already has a path separator or extension.
pub fn resolve_bare_agent_name(bare_name: &str) -> String {
    let trimmed = bare_name.trim().trim_matches('"');
    if trimmed.contains('\\') || trimmed.contains('/') {
        return bare_name.to_string();
    }
    if std::path::Path::new(trimmed).extension().is_some() {
        return bare_name.to_string();
    }

    let profile = lookup_profile(trimmed);
    let path_var = match std::env::var("PATH") {
        Ok(v) => v,
        Err(_) => return bare_name.to_string(),
    };

    for ext in profile.exe_search_order {
        let candidate = format!("{}{}", trimmed, ext);
        for dir in std::env::split_paths(&path_var) {
            if dir.join(&candidate).is_file() {
                return candidate;
            }
        }
    }

    bare_name.to_string()
}

/// Check whether a bare agent CLI (e.g. "gemini") is installed and reachable
/// via PATH using its profile's preferred extension order. Returns `true` if
/// at least one matching executable exists. Used to pre-flight resume launches
/// so missing CLIs surface a friendly error in the UI instead of failing
/// silently in CreateProcess.
pub fn is_cli_available(bare_name: &str) -> bool {
    let trimmed = bare_name.trim().trim_matches('"');
    if trimmed.is_empty() {
        return false;
    }
    let path_var = match std::env::var("PATH") {
        Ok(v) => v,
        Err(_) => return false,
    };
    let profile = lookup_profile(trimmed);
    for ext in profile.exe_search_order {
        let candidate = format!("{}{}", trimmed, ext);
        for dir in std::env::split_paths(&path_var) {
            if dir.join(&candidate).is_file() {
                return true;
            }
        }
    }
    false
}

// ─── Display ─────────────────────────────────────────────────────────────────

/// Human-friendly display name for an agent executable.
pub fn display_name_for(executable: &str) -> String {
    let profile = lookup_profile(executable);
    if profile.id != "unknown" {
        return profile.display_name.to_string();
    }
    // Unknown agent — title-case the basename.
    let basename = executable
        .rsplit(|ch: char| ch == '\\' || ch == '/')
        .next()
        .unwrap_or(executable)
        .strip_suffix(".exe")
        .or_else(|| executable.strip_suffix(".cmd"))
        .unwrap_or(executable);
    let mut chars = basename.chars();
    match chars.next() {
        Some(first) => {
            let mut title = String::with_capacity(basename.len());
            title.push(first.to_ascii_uppercase());
            title.extend(chars);
            title
        }
        None => basename.to_string(),
    }
}

// ─── Delegate Agent Helpers ──────────────────────────────────────────────────

/// List all agents that can serve as delegates (all known agents).
pub fn supported_delegate_agents() -> Vec<crate::coordinator::SupportedDelegateAgent> {
    KNOWN_AGENTS
        .iter()
        .map(|p| crate::coordinator::SupportedDelegateAgent {
            id: p.id.to_string(),
            name: p.display_name.to_string(),
            description: format!(
                "Launches `{}` in a new terminal target with a self-contained startup task prompt.",
                p.id
            ),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_cli_available_handles_empty_string() {
        assert!(!is_cli_available(""));
        assert!(!is_cli_available("   "));
    }

    #[test]
    fn is_cli_available_returns_false_for_obviously_bogus_name() {
        // A 64-char random-looking name will not exist on any sane PATH.
        assert!(!is_cli_available("zzzzz_does_not_exist_anywhere_qqqqq_82h3kf9"));
    }
}
