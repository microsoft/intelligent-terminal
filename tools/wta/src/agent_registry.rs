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
    /// `"npx -y @agentclientprotocol/claude-agent-acp"` for an adapter package).
    /// When empty, `build_acp_command` falls back to `id + acp_flags`.
    pub acp_launch_command: &'static str,
    /// Model flags accepted by the ACP server command. This may differ from
    /// `model_flags` when ACP model selection is protocol-only.
    pub acp_model_flags: &'static [&'static str],
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
    /// Flag the CLI uses to pin a caller-chosen id on a NEW session,
    /// e.g. "--session-id". `None` when unsupported.
    pub new_session_id_flag: Option<&'static str>,
}

// ─── Registry ────────────────────────────────────────────────────────────────

pub const KNOWN_AGENTS: &[AgentProfile] = &[
    AgentProfile {
        id: "copilot",
        display_name: "GitHub Copilot",
        exe_search_order: &[".exe", ".cmd"],
        acp_flags: &["--acp", "--stdio"],
        acp_launch_command: "",
        acp_model_flags: &["--model", "-m"],
        acp_auth_flow: AcpAuthFlow::External,
        delegate_prompt_flag: PromptFlag::Flag("-i"),
        model_flags: &["--model", "-m"],
        install_hint: "npm install -g @github/copilot",
        install_url: "https://github.com/github/copilot-cli",
        auth_check_command: "",
        auth_hint: "Run 'copilot' to launch the CLI, then type /login to sign in.",
        resume_flag: "--resume",
        new_session_id_flag: Some("--session-id"),
    },
    AgentProfile {
        id: "claude",
        display_name: "Claude",
        exe_search_order: &[".exe", ".cmd"],
        acp_flags: &[],
        // Claude CLI itself doesn't speak ACP. We launch the
        // ACP-project-maintained adapter via npx; npm-installed
        // `claude` shim implies node/npx are present, so this works whenever
        // delegate mode does. (Renamed from the deprecated
        // `@zed-industries/claude-code-acp`; see issue #257.)
        acp_launch_command: "npx -y @agentclientprotocol/claude-agent-acp",
        acp_model_flags: &[],
        acp_auth_flow: AcpAuthFlow::External,
        delegate_prompt_flag: PromptFlag::Positional,
        model_flags: &[],
        install_hint: "npm install -g @anthropic-ai/claude-code",
        install_url: "https://docs.anthropic.com/en/docs/claude-code",
        auth_check_command: "",
        auth_hint: "Run: claude login",
        resume_flag: "--resume",
        new_session_id_flag: Some("--session-id"),
    },
    AgentProfile {
        id: "codex",
        display_name: "Codex",
        exe_search_order: &[".exe", ".cmd"],
        acp_flags: &[],
        // Codex CLI itself doesn't speak ACP. Use the ACP-project-maintained
        // adapter, pinned so a future npm release cannot silently break startup.
        acp_launch_command: "npx -y @agentclientprotocol/codex-acp@1.1.4",
        acp_model_flags: &[],
        acp_auth_flow: AcpAuthFlow::External,
        delegate_prompt_flag: PromptFlag::Positional,
        model_flags: &[],
        install_hint: "npm install -g @openai/codex",
        install_url: "https://github.com/openai/codex",
        auth_check_command: "",
        // `codex resume <session-id>` is a subcommand (not a flag);
        // the command-synthesis template `format!("{cli} {flag} {key}")`
        // produces `codex resume <uuid>` which Codex CLI accepts.
        resume_flag: "resume",
        new_session_id_flag: None,
        auth_hint: "Run: codex auth (or set OPENAI_API_KEY)",
    },
    AgentProfile {
        id: "gemini",
        display_name: "Gemini",
        exe_search_order: &[".exe", ".cmd"],
        acp_flags: &["--experimental-acp"],
        acp_launch_command: "",
        acp_model_flags: &["--model", "-m"],
        acp_auth_flow: AcpAuthFlow::InProtocol,
        delegate_prompt_flag: PromptFlag::Positional,
        model_flags: &["--model", "-m"],
        install_hint: "npm install -g @google/gemini-cli",
        install_url: "https://github.com/google-gemini/gemini-cli",
        auth_check_command: "",
        auth_hint: "Authentication is handled in-protocol during connection.",
        resume_flag: "--resume",
        new_session_id_flag: Some("--session-id"),
    },
    AgentProfile {
        id: "opencode",
        display_name: "OpenCode",
        exe_search_order: &[".exe", ".cmd"],
        acp_flags: &["acp"],
        acp_launch_command: "",
        // `opencode acp` accepts model changes through ACP, while the
        // interactive TUI accepts `--model` and an initial `--prompt`.
        acp_model_flags: &[],
        acp_auth_flow: AcpAuthFlow::External,
        delegate_prompt_flag: PromptFlag::Flag("--prompt"),
        model_flags: &["--model", "-m"],
        install_hint: "npm install -g opencode-ai",
        install_url: "https://opencode.ai/docs/",
        auth_check_command: "",
        auth_hint: "Run: opencode auth login",
        resume_flag: "--session",
        new_session_id_flag: None,
    },
];

pub const DEFAULT_PROFILE: AgentProfile = AgentProfile {
    id: "unknown",
    display_name: "Agent",
    exe_search_order: &[".exe", ".cmd"],
    acp_flags: &[],
    acp_launch_command: "",
    acp_model_flags: &[],
    acp_auth_flow: AcpAuthFlow::None,
    delegate_prompt_flag: PromptFlag::Flag("-i"),
    model_flags: &["--model", "-m"],
    install_hint: "",
    install_url: "",
    auth_check_command: "",
    auth_hint: "",
    resume_flag: "",
    new_session_id_flag: None,
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
    let lower = basename.to_ascii_lowercase();
    let normalized = lower
        .strip_suffix(".exe")
        .or_else(|| lower.strip_suffix(".cmd"))
        .or_else(|| lower.strip_suffix(".bat"))
        .unwrap_or(&lower);
    KNOWN_AGENTS
        .iter()
        .find(|p| p.id == normalized)
        .unwrap_or(&DEFAULT_PROFILE)
}

/// Look up an agent profile by id.
pub fn lookup_profile_by_id(id: &str) -> &'static AgentProfile {
    KNOWN_AGENTS
        .iter()
        .find(|p| p.id == id)
        .unwrap_or(&DEFAULT_PROFILE)
}

// Identification-only aliases. The registry command remains the pinned launch
// command used for new Codex sessions.
const ACP_LAUNCH_COMMAND_ALIASES: &[(&str, &str)] = &[
    ("npx -y @agentclientprotocol/codex-acp", "codex"),
    ("npx -y @zed-industries/codex-acp", "codex"),
];

fn adapter_profile_from_tokens(tokens: &[String]) -> Option<&'static AgentProfile> {
    let matches_command = |command: &str| {
        let command_tokens = crate::coordinator::split_windows_commandline(command);
        tokens.starts_with(&command_tokens)
    };

    KNOWN_AGENTS
        .iter()
        .find(|profile| {
            !profile.acp_launch_command.is_empty() && matches_command(profile.acp_launch_command)
        })
        .or_else(|| {
            ACP_LAUNCH_COMMAND_ALIASES
                .iter()
                .find(|(command, _)| matches_command(command))
                .map(|(_, id)| lookup_profile_by_id(id))
        })
}

/// Returns `true` iff `id` is a real, selectable agent id present in
/// [`KNOWN_AGENTS`] (`"copilot"`, `"claude"`, `"codex"`, `"gemini"`,
/// `"opencode"`).
///
/// Prefer this over `lookup_profile_by_id(id).id != DEFAULT_PROFILE.id` when
/// distinguishing a known agent from the unknown/custom fallback: this checks
/// membership directly and is **decoupled from [`DEFAULT_PROFILE`]**, so it
/// never conflates a genuine agent with the fallback even if `DEFAULT_PROFILE.id`
/// is later changed to a real, selectable agent id. Expects an already-canonical
/// (lowercased) id — see [`resolve_agent_id_from_cmd`].
pub fn is_known_id(id: &str) -> bool {
    KNOWN_AGENTS.iter().any(|p| p.id == id)
}

/// Resolve a full agent command line (e.g. the value of `--agent`) into the
/// canonical agent id known to [`KNOWN_AGENTS`] — `"copilot"`, `"claude"`,
/// `"codex"`, `"gemini"`, `"opencode"` — or `"unknown"` if nothing matches.
///
/// This is the right thing to use whenever we need to *identify* the agent
/// from a launch command rather than execute it. It handles three input
/// shapes:
///
///   1. Bare names with flags:  `"copilot --acp --stdio"` → `"copilot"`.
///   2. Adapter launches:        `"npx -y @agentclientprotocol/claude-agent-acp"`
///      → `"claude"` (matched against each profile's `acp_launch_command`).
///   3. Full executable paths:   `"C:\\Tools\\copilot.exe --acp --stdio"`
///      → `"copilot"` (via [`lookup_profile`] which strips path and
///      extension before matching).
///
/// Empty / whitespace-only input returns `"unknown"`.
pub fn resolve_agent_id_from_cmd(agent_cmd: &str) -> &'static str {
    let trimmed = agent_cmd.trim();
    if trimmed.is_empty() {
        return DEFAULT_PROFILE.id;
    }

    let tokens = crate::coordinator::split_windows_commandline(trimmed);

    // Match complete command tokens so compatibility aliases cannot identify
    // unrelated packages whose names merely contain an adapter package name.
    if let Some(profile) = adapter_profile_from_tokens(&tokens) {
        return profile.id;
    }

    // Bare / path form: parse the first Windows commandline token so a quoted
    // executable path containing spaces stays intact, then let `lookup_profile`
    // strip path and extension before matching.
    tokens
        .first()
        .map(|first| lookup_profile(first).id)
        .unwrap_or(DEFAULT_PROFILE.id)
}

// ─── ACP Command Building ────────────────────────────────────────────────────

/// Build the full ACP agent command from an agent id and optional model.
/// E.g. `build_acp_command("copilot", Some("gpt-5"))` → `"copilot --acp --stdio --model gpt-5"`.
/// For agents whose CLI doesn't speak ACP natively (claude, codex), this
/// returns the adapter launch command instead — e.g.
/// `build_acp_command("claude", None)` → `"npx -y @agentclientprotocol/claude-agent-acp"`.
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
        if let Some(flag) = profile.acp_model_flags.first() {
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
/// For adapter-style launches (e.g. `"npx -y @agentclientprotocol/claude-agent-acp"`),
/// returns the bare agent id (e.g. `"claude"`) — delegate mode invokes the
/// agent's own CLI directly, not the ACP adapter.
pub fn strip_acp_flags_for_delegate(agent_cmd: &str) -> Option<String> {
    let tokens = crate::coordinator::split_windows_commandline(agent_cmd);
    let command = tokens.first()?;

    // Adapter-style: input is something like
    // "npx -y @agentclientprotocol/claude-agent-acp".
    // Find which agent owns this launch command and return its bare id.
    if let Some(profile) = adapter_profile_from_tokens(&tokens) {
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

    #[test]
    fn resolve_agent_id_from_cmd_recognises_bare_names_with_flags() {
        assert_eq!(resolve_agent_id_from_cmd("copilot --acp --stdio"), "copilot");
        assert_eq!(resolve_agent_id_from_cmd("gemini --experimental-acp"), "gemini");
        assert_eq!(resolve_agent_id_from_cmd("opencode acp"), "opencode");
        assert_eq!(resolve_agent_id_from_cmd("claude --resume foo"), "claude");
    }

    #[test]
    fn resolve_agent_id_from_cmd_recognises_adapter_launches() {
        // Exact match against the known adapter command.
        assert_eq!(
            resolve_agent_id_from_cmd("npx -y @agentclientprotocol/claude-agent-acp"),
            "claude",
        );
        assert_eq!(
            resolve_agent_id_from_cmd("npx -y @agentclientprotocol/codex-acp@1.1.4"),
            "codex",
        );
        assert_eq!(
            resolve_agent_id_from_cmd("npx -y @agentclientprotocol/codex-acp"),
            "codex",
        );
        assert_eq!(
            resolve_agent_id_from_cmd("npx -y @zed-industries/codex-acp"),
            "codex",
        );
        // Adapter prefix with extra trailing args still resolves.
        assert_eq!(
            resolve_agent_id_from_cmd("npx -y @agentclientprotocol/claude-agent-acp --debug"),
            "claude",
        );
        assert_eq!(
            resolve_agent_id_from_cmd("npx -y @zed-industries/codex-acp --debug"),
            "codex",
        );
    }

    #[test]
    fn codex_adapter_recognition_requires_complete_command_tokens() {
        for command in [
            "npx -y @agentclientprotocol/codex-acp-extra",
            "npx -y prefix-@agentclientprotocol/codex-acp",
            "echo npx -y @agentclientprotocol/codex-acp",
        ] {
            assert_eq!(resolve_agent_id_from_cmd(command), "unknown");
            assert_eq!(strip_acp_flags_for_delegate(command), None);
        }
    }

    #[test]
    fn strip_acp_flags_recognises_codex_adapter_compatibility_commands() {
        for command in [
            "npx -y @agentclientprotocol/codex-acp@1.1.4",
            "npx -y @agentclientprotocol/codex-acp",
            "npx -y @zed-industries/codex-acp",
            "npx -y @zed-industries/codex-acp --debug",
        ] {
            assert_eq!(
                strip_acp_flags_for_delegate(command),
                Some("codex".to_string()),
            );
        }
    }

    #[test]
    fn codex_acp_launch_command_stays_pinned() {
        assert_eq!(
            build_acp_command("codex", None),
            "npx -y @agentclientprotocol/codex-acp@1.1.4",
        );
    }

    #[test]
    fn opencode_builds_native_acp_and_delegate_commands() {
        assert_eq!(build_acp_command("opencode", None), "opencode acp");
        assert_eq!(
            build_acp_command("opencode", Some("anthropic/claude-sonnet-4-5")),
            "opencode acp",
            "OpenCode model selection is sent through ACP"
        );
        assert_eq!(
            strip_acp_flags_for_delegate("opencode acp"),
            Some("opencode".to_string())
        );
        let profile = lookup_profile_by_id("opencode");
        assert_eq!(profile.delegate_prompt_flag, PromptFlag::Flag("--prompt"));
        assert_eq!(profile.model_flags, &["--model", "-m"]);
        assert_eq!(profile.resume_flag, "--session");
    }

    #[test]
    fn resolve_agent_id_from_cmd_strips_path_and_extension() {
        assert_eq!(
            resolve_agent_id_from_cmd(r"C:\Tools\copilot.exe --acp --stdio"),
            "copilot",
        );
        assert_eq!(
            resolve_agent_id_from_cmd("/usr/local/bin/gemini --experimental-acp"),
            "gemini",
        );
        assert_eq!(resolve_agent_id_from_cmd("copilot.cmd"), "copilot");
        assert_eq!(
            resolve_agent_id_from_cmd(r#""C:\npm tools\codex.cmd" --search"#),
            "codex",
        );
    }

    #[test]
    fn lookup_and_resolve_recognize_mixed_case_batch_extensions() {
        assert_eq!(lookup_profile(r"C:\Tools\codex.CMD").id, "codex");
        assert_eq!(lookup_profile(r"C:\Tools\copilot.BaT").id, "copilot");
        assert_eq!(
            resolve_agent_id_from_cmd(r#""C:\npm tools\codex.CMD" --search"#),
            "codex",
        );
        assert_eq!(
            resolve_agent_id_from_cmd(r"C:\npm\copilot.BaT --model gpt-5"),
            "copilot",
        );
    }

    #[test]
    fn resolve_agent_id_from_cmd_falls_back_to_unknown() {
        assert_eq!(resolve_agent_id_from_cmd(""),           "unknown");
        assert_eq!(resolve_agent_id_from_cmd("   "),        "unknown");
        assert_eq!(resolve_agent_id_from_cmd("npx"),        "unknown");
        assert_eq!(resolve_agent_id_from_cmd("my-bot --x"), "unknown");
    }

    #[test]
    fn is_known_id_matches_registry_membership_only() {
        // Every real agent id is known.
        for p in KNOWN_AGENTS {
            assert!(is_known_id(p.id), "{} should be known", p.id);
        }
        // The unknown/custom fallback ids are NOT known. Crucially,
        // `is_known_id` doesn't depend on DEFAULT_PROFILE at all, so the
        // literal "unknown" is rejected because it isn't in KNOWN_AGENTS —
        // not because it happens to equal DEFAULT_PROFILE.id. This is what
        // keeps the default agent from being conflated with the fallback.
        assert!(!is_known_id(DEFAULT_PROFILE.id));
        for bogus in ["unknown", "custom", "custom:calc.exe", "totally-bogus", ""] {
            assert!(!is_known_id(bogus), "{bogus} must not be known");
        }
        // Case-sensitive: callers canonicalize to lowercase first.
        assert!(!is_known_id("Copilot"));
    }

    #[test]
    fn codex_profile_advertises_resume_support() {
        let profile = lookup_profile_by_id("codex");
        assert_eq!(
            profile.resume_flag, "resume",
            "Codex CLI uses `codex resume <id>` (subcommand form, no dash). \
             An empty resume_flag would make session_mgmt classify Codex rows \
             as Class B (not-resumable) and silently break F2 Enter."
        );
    }

    #[test]
    fn pinnable_agents_advertise_session_id_flag() {
        let pinnable = ["copilot", "claude", "gemini"];
        for id in pinnable {
            let p = lookup_profile(id);
            assert_eq!(
                p.new_session_id_flag,
                Some("--session-id"),
                "{id} should pin via --session-id"
            );
        }
        assert_eq!(
            lookup_profile("codex").new_session_id_flag,
            None,
            "codex cannot pin"
        );
    }
}
