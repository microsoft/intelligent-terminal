use clap::{Parser, Subcommand};

use crate::{
    agent_hooks_installer, agent_registry, agent_sessions, agent_tools::command_resolution,
};

#[derive(Parser, Debug)]
#[command(
    name = "wta",
    about = "Windows Terminal Agent — ACP TUI client / tmux-like CLI"
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Option<Command>,

    /// Initial prompt to send to the agent (ACP mode only)
    #[arg(value_name = "PROMPT")]
    pub(crate) prompt: Option<String>,

    /// Agent CLI command (e.g. "copilot --acp --stdio")
    #[arg(long, default_value = agent_registry::DEFAULT_ACP_COMMAND)]
    pub(crate) agent: String,

    /// Canonical agent identifier (`copilot` / `claude` / `codex` / `gemini`
    /// / `opencode` / `custom:<name>`). When the host (Windows Terminal) launches wta it
    /// already knows which entry the user picked in settings, so it passes
    /// the original `acpAgent` value through here. wta uses this id as the
    /// authoritative identity for `current_agent_id` — driving the session-
    /// management view's CLI filter, the preflight check, etc.
    ///
    /// When omitted (manual `wta` runs, older host builds, tests) wta falls
    /// back to inferring the id by parsing the `--agent` command line via
    /// `agent_registry::resolve_agent_id_from_cmd`. That fallback works for
    /// bare names but is fragile for adapter-style launches (`npx … claude-
    /// code-acp`) and full-path launches, so the host should always pass
    /// `--agent-id` explicitly.
    #[arg(long)]
    pub(crate) agent_id: Option<String>,

    /// Per-tab ACP execution source (`host` or `wsl`). Hidden because
    /// TerminalPage owns source compatibility checks.
    #[arg(long, hide = true, value_parser = ["host", "wsl"])]
    pub(crate) agent_source: Option<String>,

    /// WSL distro paired with `--agent-source wsl`.
    #[arg(long, hide = true)]
    pub(crate) agent_wsl_distro: Option<String>,

    /// Working-pane cwd captured when this helper was created.
    #[arg(long, hide = true)]
    pub(crate) agent_source_cwd: Option<String>,

    /// Master-only allowlist of agent ids a helper may request over the
    /// pipe (the GPO-filtered set; built by TerminalPage::
    /// _BuildSharedWtaExtraArgs from `FilteredAcpAgents()`). The master
    /// reconstructs a helper's requested agent command from its declared
    /// `agent_id` ONLY when that id is in this set — never executing a
    /// command string sent over the pipe. An id outside the set (or a
    /// custom/unknown id) falls back to `--agent` / `--agent-id`. An *absent*
    /// flag means "no host allowlist" (manual runs, older hosts): the master
    /// accepts any *known* agent id. A *present* flag is honored fail-closed —
    /// even when it filters down to nothing, every helper-selected id is then
    /// blocked (all panes fall back to the default) rather than widening back
    /// to accept-any. Helpers use the same list only to filter `/agent`;
    /// the master remains the authoritative enforcement point.
    #[arg(long, hide = true, value_name = "IDS", value_delimiter = ',')]
    pub(crate) allowed_agent_ids: Vec<String>,

    /// Boot-time hint from Windows Terminal: start directly on the auth screen
    /// for the given agent instead of attempting the initial ACP session. Used
    /// when FRE just installed Copilot, where the next expected action is
    /// signing in. Hidden — only Windows Terminal should pass it.
    #[arg(long, hide = true, value_name = "AGENT_ID")]
    pub(crate) initial_auth_agent: Option<String>,

    /// Model override for the ACP agent. Sent via ACP setSessionModel after
    /// handshake. Used by adapter-style launches (claude, codex via npx)
    /// where the model can't be passed on the command line; native ACP
    /// agents may use their own --model flag in `agent`.
    #[arg(long)]
    pub(crate) acp_model: Option<String>,

    /// This helper inherited `acp_model` from the global agent settings rather
    /// than a per-tab/profile pin. Hidden because TerminalPage is the
    /// authoritative source of this scope.
    #[arg(long, hide = true)]
    pub(crate) follows_global_acp_model: bool,

    /// Model configured through an Intelligent Terminal custom provider.
    /// Bounded helper bootstrap metadata; provider credentials remain
    /// master-only and the full catalog arrives over the protocol.
    #[arg(long, hide = true)]
    pub(crate) custom_model_selection: Option<String>,

    /// Legacy compatibility flag for old hosts that placed the full
    /// credential-free custom-provider catalog on argv. New hosts deliver it
    /// after helper connection over `agent_config_changed`.
    #[arg(long, hide = true)]
    pub(crate) custom_models: Option<String>,

    /// Legacy compatibility flag for old hosts that placed the cloud/native
    /// model catalog on argv. New hosts deliver it after helper connection.
    #[arg(long, hide = true)]
    pub(crate) cloud_models: Option<String>,

    /// Delegate agent CLI command (e.g. "codex")
    #[arg(long)]
    pub(crate) delegate_agent: Option<String>,

    /// Model override for the delegate agent
    #[arg(long)]
    pub(crate) delegate_model: Option<String>,

    /// Disable auto-fix on command failure
    #[arg(long)]
    pub(crate) no_autofix: bool,

    /// Enter diagnostic setup mode with the given reason instead of connecting directly.
    /// Values: agent-missing, agent-error
    #[arg(long)]
    pub(crate) setup: Option<String>,

    /// Initial TUI view to show on startup. `chat` (default) starts in the
    /// chat view; `sessions` starts in the Agents (session list) view —
    /// equivalent to the user pressing Ctrl+Shift+/ right after the pane opens.
    /// Wired to WT's Ctrl+Shift+/ binding via TerminalPage.
    #[arg(long, value_enum, default_value_t = InitialView::Chat)]
    pub(crate) initial_view: InitialView,

    /// UI language override, passed by Windows Terminal from the
    /// `settings.json` `Language` field. When present, wta uses this
    /// directly for i18n instead of detecting the OS locale — ensuring
    /// the agent pane displays the same language as the Terminal chrome.
    /// When absent, wta falls back to `sys_locale` (automatic detection).
    #[arg(long)]
    pub(crate) language: Option<String>,

    /// Stable GUID of the WT tab that owns this wta process. Passed in by
    /// TerminalPage when spawning the agent pane (both _OpenOrReuseAgentPane
    /// and _AutoCreateHiddenAgentPane). Seeded into app_state.tab_id before
    /// ACP init, so the first AgentConnected binds the session under the
    /// real tab GUID instead of falling back to the implicit DEFAULT_TAB_ID
    /// placeholder. Hidden because nothing outside WT should be setting it.
    #[arg(long, hide = true)]
    pub(crate) owner_tab_id: Option<String>,

    /// Window ID of the WT window that owns this helper. Passed alongside
    /// `--owner-tab-id` because PID-based pane discovery is best-effort and
    /// may not find a newly spawned ConPTY helper before `/agent` is used.
    #[arg(long, hide = true)]
    pub(crate) owner_window_id: Option<String>,

    /// Boot-time hint: instead of letting the helper create a fresh ACP
    /// session via `session/new`, immediately resume the given session id
    /// via `session/load`. Used by the "Enter on Historical/Ended row in
    /// session manager" path: C++ spawns a new helper for the new
    /// agent pane and bundles the resume request via these flags so the
    /// resume is atomic — no separate `load_session` VT broadcast that
    /// could race the helper's pipe-attach.
    ///
    /// Pair with `--initial-load-cwd`. Hidden — only Windows Terminal
    /// should pass it. No-op outside `--connect-master` (only the helper
    /// boot path consumes it).
    #[arg(long, hide = true, value_name = "SESSION_ID")]
    pub(crate) initial_load_session_id: Option<String>,

    /// Working directory associated with `--initial-load-session-id`.
    /// Passed to the agent CLI via the ACP `session/load` request so the
    /// resumed conversation runs against the right repo root. Hidden.
    #[arg(long, hide = true, value_name = "PATH")]
    pub(crate) initial_load_cwd: Option<String>,

    /// Pre-warm mode: the helper is being spawned for a tab whose agent
    /// pane is *already stashed* on the C++ side (see TerminalPage::
    /// _AutoCreateHiddenAgentPaneShared autoStash path). Without this
    /// flag, the helper's `--owner-tab-id` startup branch seeds
    /// `tab.pane_open = true` and echoes back `agent_state_changed
    /// { pane_open: true }`, which C++ interprets as "user opened the
    /// pane" and unstashes it — defeating pre-warm. With this flag the
    /// helper seeds `tab.pane_open = false`, matching the C++ stash
    /// state. Hidden because only WT's pre-warm path should set it.
    #[arg(long, hide = true)]
    pub(crate) start_stashed: bool,

    /// Degraded-open mode: the helper is being spawned for a pane the user
    /// opened *while wta-master is known to be down* (it died unexpectedly and
    /// hasn't been recovered via /restart — see C++ `SharedWta::IsDegraded`).
    /// Rather than the helper retrying the dead master pipe for ~75s and
    /// showing a spinner, it comes up immediately in the disconnected state
    /// (the same transport-lost view an orphaned pane shows), so the user can
    /// /restart right there instead of hunting for another pane. Hidden — only
    /// WT's degraded-open path should set it.
    #[arg(long, hide = true)]
    pub(crate) assume_master_down: bool,

    // Legacy flags (hidden, backward compat)
    #[arg(long, hide = true)]
    pub(crate) info: bool,
    #[arg(long, hide = true)]
    pub(crate) test_pipe: bool,

    /// Output raw JSON instead of human-readable format
    #[arg(long, global = true)]
    pub(crate) json: bool,

    /// Run as the wta-master singleton (Z architecture). Listens on
    /// the named pipe whose name is passed here for wta-helper
    /// connections; owns the single ACP connection to the agent CLI
    /// subprocess; multiplexes per-helper ACP sessions onto it. Used
    /// by `SharedWta::AcquirePane` on the C++ side. Hidden — only
    /// Windows Terminal should spawn it.
    ///
    /// Pipe name is typically `\\.\pipe\wta-master-<GUID>`.
    #[arg(long, hide = true, value_name = "PIPE_NAME")]
    pub(crate) master: Option<String>,

    /// Connect to a wta-master singleton over the named pipe whose
    /// path is passed here, rather than spawning our own agent CLI
    /// subprocess. Used when this wta is acting as a per-pane helper
    /// in the helper+master architecture (see
    /// doc/specs/Multi-window-agent-pane.md). Hidden — only the C++
    /// side should pass it.
    ///
    /// Logically mutually exclusive with `--master`: a process can be
    /// either the master or a helper, never both. Enforced by clap so
    /// a misconfigured invocation fails fast instead of silently
    /// preferring `--master` (the previous behavior).
    #[arg(long, hide = true, value_name = "PIPE_NAME", conflicts_with = "master")]
    pub(crate) connect_master: Option<String>,
}

#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// Show Windows Terminal protocol connection info
    Info,
    /// Test protocol connection to Windows Terminal
    TestPipe,
    /// List all Windows Terminal windows
    #[command(alias = "lsw")]
    ListWindows,
    /// List tabs in a window
    #[command(alias = "lst")]
    ListTabs {
        /// Window ID (defaults to first window)
        #[arg(short = 'w', long)]
        window_id: Option<String>,
    },
    /// List panes in a tab
    #[command(alias = "lsp")]
    ListPanes {
        /// Tab ID (defaults to active tab)
        #[arg(short = 't', long)]
        tab_id: Option<String>,
        /// Window ID (used with tab_id)
        #[arg(short = 'w', long)]
        window_id: Option<String>,
    },
    /// Identify a command using sources applicable to the active shell
    ResolveCommand {
        /// Command name to identify (without arguments or a path)
        #[arg(value_parser = command_resolution::parse_non_empty)]
        token: String,
        /// Active shell identity; PowerShell hosts also load their user profile
        #[arg(long, default_value = "pwsh.exe", value_parser = command_resolution::parse_non_empty)]
        shell: String,
        /// Working directory to inspect
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
    },
    /// Create a new tab
    #[command(alias = "neww")]
    NewTab {
        /// Command to run in the new tab
        #[arg(short = 'c', long)]
        command: Option<String>,
        /// Working directory
        #[arg(short = 'd', long)]
        cwd: Option<String>,
        /// Tab title
        #[arg(short = 'n', long)]
        title: Option<String>,
    },
    /// Split the current pane
    #[command(alias = "splitw")]
    SplitPane {
        /// Target pane ID
        #[arg(short = 't', long)]
        target: Option<String>,
        /// Split horizontally (panes side by side)
        #[arg(short = 'h', long)]
        horizontal: bool,
        /// Split vertically (panes stacked)
        #[arg(short = 'v', long)]
        vertical: bool,
        /// Size as fraction (0.0-1.0)
        #[arg(short = 's', long)]
        size: Option<f64>,
        /// Command to run in the new pane
        #[arg(short = 'c', long)]
        command: Option<String>,
    },
    /// Capture pane output (like tmux capture-pane -p)
    #[command(alias = "capturep")]
    CapturePane {
        /// Target pane ID (defaults to active pane)
        #[arg(short = 't', long)]
        target: Option<String>,
        /// Maximum lines to capture
        #[arg(short = 'l', long)]
        max_lines: Option<u32>,
        /// Only return the most recent completed shell prompt
        /// (command + output). Requires OSC 133 shell integration.
        #[arg(long)]
        last_prompt: bool,
    },
    /// Close/kill a pane
    #[command(alias = "killp")]
    KillPane {
        /// Target pane ID (defaults to active pane)
        #[arg(short = 't', long)]
        target: Option<String>,
    },
    /// Show the currently active pane
    ActivePane,
    /// Show process status of a pane
    PaneStatus {
        /// Target pane ID (defaults to active pane)
        #[arg(short = 't', long)]
        target: Option<String>,
    },
    /// Wait for a pane's process to exit (delegates to `wtcli wait-for`)
    WaitFor {
        /// Target pane ID
        #[arg(short = 't', long)]
        target: String,
        /// Poll interval in milliseconds
        #[arg(long, default_value = "500")]
        interval: u64,
        /// Timeout in seconds (0 = wait forever)
        #[arg(long, default_value = "0")]
        timeout: u64,
    },
    /// Discover and print the WT COM CLSID used for protocol routing
    PipeId,
    /// Print shell commands to set WT_COM_CLSID
    #[command(alias = "setenv")]
    SetEnv {
        /// Shell syntax: bash (default), powershell, cmd
        #[arg(short = 's', long, default_value = "bash")]
        shell: String,
    },
    /// Listen for events from Windows Terminal (VT sequences, connection state changes)
    #[command(alias = "mon")]
    Listen {
        /// Filter by pane ID (show events from all panes if omitted)
        #[arg(short = 't', long)]
        target: Option<String>,
    },
    /// Open a configured delegate agent in a new tab (fire-and-forget). With a
    /// PROMPT, the prompt is baked into the agent's launch; omit PROMPT to open
    /// the agent interactively with no startup prompt.
    Delegate {
        /// The prompt to send to the delegate agent. Omit to open the agent
        /// interactively in a new tab with no startup prompt.
        #[arg(value_name = "PROMPT")]
        prompt: Option<String>,
        /// Agent CLI command (used to derive delegate agent commandline)
        #[arg(long, default_value = agent_registry::DEFAULT_ACP_COMMAND)]
        agent: String,
        /// Delegate agent CLI command (e.g. "codex")
        #[arg(long)]
        delegate_agent: Option<String>,
        /// Model override for the delegate agent
        #[arg(long)]
        delegate_model: Option<String>,
        /// Exact execution source (host or wsl). Defaults to host when
        /// omitted; never inferred from the active pane's shell/distro
        #[arg(long)]
        delegate_source: Option<String>,
        /// WSL distro for an explicit --delegate-source wsl selection
        #[arg(long)]
        delegate_wsl_distro: Option<String>,
        /// Working directory for the delegate agent tab
        #[arg(long)]
        cwd: Option<String>,
    },
    /// Manage the wt-agent-hooks bridge for supported CLI agents
    /// (Copilot / Claude / Gemini). See `agent_hooks_installer` for
    /// what each action does.
    Hooks {
        #[command(subcommand)]
        action: HooksAction,
    },
    /// Inspect sessions known to the shared wta-master.
    Sessions {
        #[command(subcommand)]
        action: SessionsAction,
    },
    /// One-shot ACP handshake to read an agent's advertised model list.
    /// Spawned by the Settings UI when the user picks a new ACP agent so
    /// the model dropdown can populate before any real agent pane is
    /// rebuilt. Prints a single JSON object to stdout:
    ///
    ///   {"available_models":[{"id":"...","name":"...","description":"..."}],
    ///    "current_model_id":"..."}
    ///
    /// On error: non-zero exit, message on stderr.
    ProbeModels {
        /// Full agent cmdline, same shape as `--agent` (e.g.
        /// "copilot --acp --stdio" or "npx -y @agentclientprotocol/claude-agent-acp").
        #[arg(long)]
        agent: String,
    },
    /// List built-in ACP agents installed inside one WSL distro.
    /// Used by the per-profile Settings picker.
    #[command(hide = true)]
    ProbeAgentSources {
        #[arg(long)]
        wsl_distro: String,
    },
    /// List built-in ACP agents that WTA can launch on the Windows host.
    /// Used by FRE, Settings, and automatic agent selection so those surfaces
    /// share WTA's executable resolution with the actual ACP spawn path.
    #[command(hide = true)]
    ProbeHostAgents,
    /// Diagnostic: spawn an agent CLI, ACP `initialize`, then call
    /// `session/list` (`list_sessions`) and print what it returns.
    /// Used to evaluate whether ACP session enumeration can replace
    /// reading on-disk transcripts. Prints a pretty JSON object to
    /// stdout; on error: non-zero exit, message on stderr.
    ProbeSessions {
        /// Full agent cmdline, same shape as `--agent` (e.g.
        /// "copilot --acp --stdio" or "npx -y @agentclientprotocol/claude-agent-acp").
        #[arg(long)]
        agent: String,
    },
    /// Diagnostic: spawn an agent CLI, call ACP `session/list`, filter
    /// agent-pane-origin rows, and print the host history rows WTA would
    /// seed from the already-running master agent.
    ProbeHostSessions {
        /// Full agent cmdline, same shape as `--agent` (e.g.
        /// "copilot --acp --stdio" or "npx -y @agentclientprotocol/claude-agent-acp").
        #[arg(long)]
        agent: String,
    },
    /// Submit a typed terminal-action proposal directly to the Helper that
    /// owns the current turn. Intended to be run by an agent session using
    /// the exact canonical command injected into its prompt.
    #[command(hide = true)]
    ProposeTerminalActions {
        /// Opaque per-turn channel from the Helper's runtime instruction.
        #[arg(long)]
        channel: String,
        /// Compact versioned proposal JSON. stdin and payload files are
        /// intentionally unsupported so permission matching has one form.
        #[arg(long)]
        payload_json: String,
    },
}

/// Subcommands for `wta sessions`.
#[derive(Subcommand, Debug)]
pub(crate) enum SessionsAction {
    /// List sessions in the master registry.
    List {
        /// Override the wta-master named pipe path.
        #[arg(long, value_name = "PIPE_NAME")]
        master: Option<String>,
        /// Restrict the list to a session origin. `all` (default) shows
        /// every row — that matches the historical debug behavior.
        /// `shell` shows only user-started shell-pane sessions (the
        /// MVP sessions default). `agent-pane` shows only sessions that
        /// WTA spawned for an Intelligent Terminal agent pane.
        #[arg(long, value_enum, default_value_t = SessionsOriginArg::All)]
        origin: SessionsOriginArg,
    },
}

/// CLI value for `wta sessions list --origin`. Mirrors
/// [`agent_sessions::OriginFilter`] but lives in `cli::args` so the
/// clap derive can attach `ValueEnum` without polluting runtime modules
/// with clap types.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionsOriginArg {
    /// Shell-pane sessions only (Class B). Matches the MVP sessions picker.
    Shell,
    /// Agent-pane sessions only (Class A). Hidden from the MVP sessions
    /// picker; surfaced here for debugging.
    AgentPane,
    /// Every row in the registry — historical debug default.
    All,
}

impl SessionsOriginArg {
    pub(crate) fn to_filter(self) -> agent_sessions::OriginFilter {
        match self {
            SessionsOriginArg::Shell => agent_sessions::OriginFilter::ShellOnly,
            SessionsOriginArg::AgentPane => agent_sessions::OriginFilter::AgentPaneOnly,
            SessionsOriginArg::All => agent_sessions::OriginFilter::All,
        }
    }
}

/// Subcommands for `wta hooks`.
#[derive(Subcommand, Debug)]
pub(crate) enum HooksAction {
    /// (Re-)install the wt-agent-hooks bridge. Installs for all supported
    /// CLIs by default, or a single CLI with `--cli`. `--only-missing` skips
    /// CLIs that are already current and upgrades the ones that are behind.
    /// With `--json` returns a structured per-CLI outcome report.
    Install {
        /// Which CLI to install for. Default: `all`.
        #[arg(long, value_enum, default_value_t = HooksCliFilter::All)]
        cli: HooksCliFilter,
        /// Skip CLIs whose hook bridge is already complete and current. A
        /// complete but out-of-date bridge is upgraded rather than
        /// re-installed, because a second `install` no-ops; missing, partial,
        /// disabled and stale-path bridges are installed as usual.
        #[arg(long)]
        only_missing: bool,
    },
    /// Print per-CLI install state. Returns JSON with `--json`,
    /// or a human-readable table by default.
    Status,
    /// Uninstall the bridge for one or all CLIs. Best-effort: missing
    /// CLIs are skipped at info level. With `--json` returns a structured
    /// per-CLI result report.
    Uninstall {
        /// Which CLI(s) to uninstall for. Default: `all`.
        #[arg(long, value_enum, default_value_t = HooksCliFilter::All)]
        cli: HooksCliFilter,
    },
}

/// `--cli` filter for `wta hooks uninstall`.
#[derive(Copy, Clone, Debug, clap::ValueEnum)]
pub(crate) enum HooksCliFilter {
    All,
    Copilot,
    Claude,
    Gemini,
    Codex,
    #[value(name = "opencode")]
    OpenCode,
}

impl HooksCliFilter {
    pub(crate) fn into_scope(self) -> agent_hooks_installer::CliScope {
        use agent_hooks_installer::{CliKind, CliScope};
        match self {
            HooksCliFilter::All => CliScope::All,
            HooksCliFilter::Copilot => CliScope::One(CliKind::Copilot),
            HooksCliFilter::Claude => CliScope::One(CliKind::Claude),
            HooksCliFilter::Gemini => CliScope::One(CliKind::Gemini),
            HooksCliFilter::Codex => CliScope::One(CliKind::Codex),
            HooksCliFilter::OpenCode => CliScope::One(CliKind::OpenCode),
        }
    }
}

/// `--initial-view` selector. Drives whether the TUI starts in the chat
/// view (default) or jumps straight to the Agents (session list) view.
#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum InitialView {
    Chat,
    Sessions,
}
