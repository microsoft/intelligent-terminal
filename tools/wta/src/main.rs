#[macro_use]
extern crate rust_i18n;

mod agent_check;
mod agent_hooks_installer;
mod agent_pane_origin;
mod agent_registry;
mod agent_sessions;
mod app;
mod commands;
mod coordinator;
mod cwd_util;
mod event;
mod helper;
mod history_loader;
mod logging;
#[cfg(test)]
#[path = "locale_parity_tests.rs"]
mod locale_parity_tests;
mod master;
mod osc52;
mod pane_context;
mod proc_bind;
mod protocol;
mod rtl;
mod runtime_paths;
mod session_mgmt;
mod session_registry;
mod session_watcher;
mod shell;
mod telemetry;
#[cfg(test)]
mod test_support;
mod theme;
mod ui;
mod ui_trace;

use acp::Agent as _;
use agent_client_protocol as acp;
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use crossterm::{
    cursor::SetCursorStyle,
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::prelude::*;
use serde_json::json;
use std::io;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use shell::wt_channel::{CliChannel, WtChannel};
use shell::ShellManager;

i18n!("locales", fallback = "en-US");

/// Normalize a detected OS locale to the closest available locale file.
/// Mimics Windows MRT behavior with script-aware affinity matching.
///
/// Examples:
///   - `de-AT` → `de-DE` (only one German variant available)
///   - `zh-HK` → `zh-TW` (Traditional Chinese affinity)
///   - `zh-SG` → `zh-CN` (Simplified Chinese affinity)
///   - `pt-MZ` → `pt-PT` (European Portuguese affinity)
///   - `fr-BE` → `fr-FR` (only one French variant available)
///   - `en-US` → `en-US` (exact match)
fn normalize_locale(locale: &str) -> String {
    let available = rust_i18n::available_locales!();

    // 1. Exact match (case-insensitive)
    if available.iter().any(|l| l.eq_ignore_ascii_case(locale)) {
        return locale.to_string();
    }

    // 2. Script/region affinity for languages with multiple variants.
    //    Aligns with Windows MRT language-distance behavior for our locale set.
    let affinity_target = match locale.to_lowercase().as_str() {
        // Chinese: script-based split
        "zh-hk" | "zh-mo" | "zh-hant" | "zh-hant-tw" | "zh-hant-hk" | "zh-hant-mo" => Some("zh-TW"),
        "zh-sg" | "zh-hans" | "zh-hans-cn" | "zh-hans-sg" => Some("zh-CN"),
        // English: Commonwealth regions → en-GB
        "en-au" | "en-nz" | "en-ie" | "en-in" | "en-sg" | "en-za" | "en-hk" | "en-my" | "en-ph"
        | "en-pk" | "en-ng" | "en-ke" | "en-gh" => Some("en-GB"),
        // Spanish: Latin American regions → es-MX
        "es-ar" | "es-co" | "es-cl" | "es-pe" | "es-ve" | "es-ec" | "es-gt" | "es-cu" | "es-bo"
        | "es-do" | "es-hn" | "es-py" | "es-sv" | "es-ni" | "es-cr" | "es-pa" | "es-uy"
        | "es-pr" | "es-us" | "es-419" => Some("es-MX"),
        // French: non-Canadian → fr-FR
        "fr-be" | "fr-ch" | "fr-lu" | "fr-mc" | "fr-sn" | "fr-ci" | "fr-ml" | "fr-cm" | "fr-mg"
        | "fr-cd" | "fr-dz" | "fr-tn" | "fr-ma" => Some("fr-FR"),
        // Portuguese: non-Brazilian → pt-PT
        "pt-ao" | "pt-mz" | "pt-gw" | "pt-tl" | "pt-cv" | "pt-st" => Some("pt-PT"),
        // Serbian: script-based split
        "sr-latn-ba" | "sr-latn-me" | "sr-latn-xk" => Some("sr-Latn-RS"),
        "sr-cyrl-ba" | "sr-cyrl-me" | "sr-cyrl-xk" => Some("sr-Cyrl-RS"),
        _ => None,
    };

    if let Some(target) = affinity_target {
        if available.iter().any(|l| l.eq_ignore_ascii_case(target)) {
            return target.to_string();
        }
    }

    // 3. Fallback: strip territory, find any locale with same language prefix.
    //    Safe for languages where we only have one regional variant (de, fr, ja, etc.)
    if let Some(lang) = locale.split('-').next() {
        let prefix = format!("{}-", lang.to_lowercase());
        if let Some(found) = available
            .iter()
            .find(|l| l.to_lowercase().starts_with(&prefix))
        {
            return found.to_string();
        }
    }

    "en-US".to_string()
}

// ─── CLI Definition ─────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    name = "wta",
    about = "Windows Terminal Agent — ACP TUI client / tmux-like CLI"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Initial prompt to send to the agent (ACP mode only)
    #[arg(value_name = "PROMPT")]
    prompt: Option<String>,

    /// Agent CLI command (e.g. "copilot --acp --stdio")
    #[arg(long, default_value = agent_registry::DEFAULT_ACP_COMMAND)]
    agent: String,

    /// Canonical agent identifier (`copilot` / `claude` / `codex` / `gemini`
    /// / `custom:<name>`). When the host (Windows Terminal) launches wta it
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
    agent_id: Option<String>,

    /// Model override for the ACP agent. Sent via ACP setSessionModel after
    /// handshake. Used by adapter-style launches (claude, codex via npx)
    /// where the model can't be passed on the command line; native ACP
    /// agents (copilot, gemini) use their own --model flag in `agent`.
    #[arg(long)]
    acp_model: Option<String>,

    /// Delegate agent CLI command (e.g. "codex")
    #[arg(long)]
    delegate_agent: Option<String>,

    /// Model override for the delegate agent
    #[arg(long)]
    delegate_model: Option<String>,

    /// Disable auto-fix on command failure
    #[arg(long)]
    no_autofix: bool,

    /// Enter setup mode with the given reason. The agent pane shows a
    /// Getting Started screen instead of connecting directly.
    /// Values: first-run, agent-missing, agent-error, switch-agent
    #[arg(long)]
    setup: Option<String>,

    /// Initial TUI view to show on startup. `chat` (default) starts in the
    /// chat view; `sessions` starts in the Agents (session list) view —
    /// equivalent to the user pressing Ctrl+Shift+/ right after the pane opens.
    /// Wired to WT's Ctrl+Shift+/ binding via TerminalPage.
    #[arg(long, value_enum, default_value_t = InitialView::Chat)]
    initial_view: InitialView,

    /// UI language override, passed by Windows Terminal from the
    /// `settings.json` `Language` field. When present, wta uses this
    /// directly for i18n instead of detecting the OS locale — ensuring
    /// the agent pane displays the same language as the Terminal chrome.
    /// When absent, wta falls back to `sys_locale` (automatic detection).
    #[arg(long)]
    language: Option<String>,

    /// Stable GUID of the WT tab that owns this wta process. Passed in by
    /// TerminalPage when spawning the agent pane (both _OpenOrReuseAgentPane
    /// and _AutoCreateHiddenAgentPane). Seeded into app_state.tab_id before
    /// ACP init, so the first AgentConnected binds the session under the
    /// real tab GUID instead of falling back to the implicit DEFAULT_TAB_ID
    /// placeholder. Hidden because nothing outside WT should be setting it.
    #[arg(long, hide = true)]
    owner_tab_id: Option<String>,

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
    initial_load_session_id: Option<String>,

    /// Working directory associated with `--initial-load-session-id`.
    /// Passed to the agent CLI via the ACP `session/load` request so the
    /// resumed conversation runs against the right repo root. Hidden.
    #[arg(long, hide = true, value_name = "PATH")]
    initial_load_cwd: Option<String>,

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
    start_stashed: bool,

    /// Degraded-open mode: the helper is being spawned for a pane the user
    /// opened *while wta-master is known to be down* (it died unexpectedly and
    /// hasn't been recovered via /restart — see C++ `SharedWta::IsDegraded`).
    /// Rather than the helper retrying the dead master pipe for ~75s and
    /// showing a spinner, it comes up immediately in the disconnected state
    /// (the same transport-lost view an orphaned pane shows), so the user can
    /// /restart right there instead of hunting for another pane. Hidden — only
    /// WT's degraded-open path should set it.
    #[arg(long, hide = true)]
    assume_master_down: bool,

    // Legacy flags (hidden, backward compat)
    #[arg(long, hide = true)]
    info: bool,
    #[arg(long, hide = true)]
    test_pipe: bool,

    /// Output raw JSON instead of human-readable format
    #[arg(long, global = true)]
    json: bool,

    /// Run as the wta-master singleton (Z architecture). Listens on
    /// the named pipe whose name is passed here for wta-helper
    /// connections; owns the single ACP connection to the agent CLI
    /// subprocess; multiplexes per-helper ACP sessions onto it. Used
    /// by `SharedWta::AcquirePane` on the C++ side. Hidden — only
    /// Windows Terminal should spawn it.
    ///
    /// Pipe name is typically `\\.\pipe\wta-master-<GUID>`.
    #[arg(long, hide = true, value_name = "PIPE_NAME")]
    master: Option<String>,

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
    connect_master: Option<String>,
}

#[derive(Subcommand, Debug)]
enum Command {
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
}


/// Subcommands for `wta sessions`.
#[derive(Subcommand, Debug)]
enum SessionsAction {
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
/// [`agent_sessions::OriginFilter`] but lives in `main.rs` so the
/// clap derive can attach `ValueEnum` without polluting the library
/// crate with clap as a dependency.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum SessionsOriginArg {
    /// Shell-pane sessions only (Class B). Matches the MVP sessions picker.
    Shell,
    /// Agent-pane sessions only (Class A). Hidden from the MVP sessions
    /// picker; surfaced here for debugging.
    AgentPane,
    /// Every row in the registry — historical debug default.
    All,
}

impl SessionsOriginArg {
    fn to_filter(self) -> agent_sessions::OriginFilter {
        match self {
            SessionsOriginArg::Shell     => agent_sessions::OriginFilter::ShellOnly,
            SessionsOriginArg::AgentPane => agent_sessions::OriginFilter::AgentPaneOnly,
            SessionsOriginArg::All       => agent_sessions::OriginFilter::All,
        }
    }
}

/// Subcommands for `wta hooks`.
#[derive(Subcommand, Debug)]
enum HooksAction {
    /// (Re-)install the wt-agent-hooks bridge. Installs for all supported
    /// CLIs by default, or a single CLI with `--cli`.
    Install {
        /// Which CLI to install for. Default: `all`.
        #[arg(long, value_enum, default_value_t = HooksCliFilter::All)]
        cli: HooksCliFilter,
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
enum HooksCliFilter {
    All,
    Copilot,
    Claude,
    Gemini,
    Codex,
}

impl HooksCliFilter {
    fn into_scope(self) -> agent_hooks_installer::CliScope {
        use agent_hooks_installer::{CliKind, CliScope};
        match self {
            HooksCliFilter::All => CliScope::All,
            HooksCliFilter::Copilot => CliScope::One(CliKind::Copilot),
            HooksCliFilter::Claude => CliScope::One(CliKind::Claude),
            HooksCliFilter::Gemini => CliScope::One(CliKind::Gemini),
            HooksCliFilter::Codex => CliScope::One(CliKind::Codex),
        }
    }
}

/// `--initial-view` selector. Drives whether the TUI starts in the chat
/// view (default) or jumps straight to the Agents (session list) view.
#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
enum InitialView {
    Chat,
    Sessions,
}

// ─── Entry Point ────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    // Detect and set the system locale for i18n.
    // normalize_locale() maps unmatched regions to the canonical variant (e.g., de-AT → de-DE).
    //
    // Priority:
    //   1. --language flag (passed by Windows Terminal from settings.json Language)
    //      — aligns with C++ side's PrimaryLanguageOverride behavior
    //   2. sys_locale (GetUserPreferredUILanguages — automatic OS detection)
    //      — aligns with C++ side's MRT fallback when Language is empty
    let cli = Cli::parse();

    // Initialize file logging exactly once, as the very first thing after
    // arg parsing, so even early-startup failures (locale, ETW registration,
    // legacy-flag dispatch) are captured. The global tracing subscriber can
    // only be set once per process, so every mode routes through here — the
    // per-mode handlers below no longer init their own. The appender's guard
    // is held in a global and flushed via `logging::shutdown_flush()` on every
    // exit path (see the calls below and before each `process::exit`).
    logging::init(&process_label(&cli));
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "=== wta starting ===");

    let locale = cli
        .language
        .clone()
        .or_else(|| sys_locale::get_locale())
        .unwrap_or_else(|| "en-US".to_string());
    rust_i18n::set_locale(&normalize_locale(&locale));

    // Register the WTA ETW TraceLogging provider once per process.
    // WTA registers under the SAME provider GUID as the C++ side
    // (`Microsoft.Windows.Terminal.App` / `g_hTerminalAppProvider`) so
    // listeners see a single merged event stream. See tools/wta/src/telemetry.rs.
    telemetry::register();

    // Legacy flags first (backward compat)
    if cli.test_pipe {
        let r = run_test_pipe().await;
        if let Err(err) = &r {
            tracing::error!(error = ?err, "wta exiting with error");
        }
        logging::shutdown_flush();
        return r;
    }
    if cli.info {
        let r = run_info_mode().await;
        if let Err(err) = &r {
            tracing::error!(error = ?err, "wta exiting with error");
        }
        logging::shutdown_flush();
        return r;
    }
    let json_mode = cli.json;

    let result = match cli.command {
        // Subcommand aliases for legacy modes
        Some(Command::Info) => run_info_mode().await,
        Some(Command::TestPipe) => run_test_pipe().await,

        // ── List commands ──
        Some(Command::ListWindows) => {
            let result = wt_call("list_windows", json!({})).await?;
            print_output(&result, json_mode, format_windows_human);
            Ok(())
        }
        Some(Command::ListTabs { window_id }) => {
            let channel = connect_channel().await?;
            let wid = match window_id {
                Some(id) => id,
                None => get_first_window_id(&channel).await?,
            };
            let result = channel
                .request("list_tabs", json!({ "window_id": wid }))
                .await?;
            print_output(&result, json_mode, format_tabs_human);
            Ok(())
        }
        Some(Command::ListPanes { tab_id, window_id }) => {
            let channel = connect_channel().await?;
            let tid = match tab_id {
                Some(id) => id,
                None => {
                    let wid = match window_id {
                        Some(id) => id,
                        None => get_first_window_id(&channel).await?,
                    };
                    get_first_tab_id(&channel, &wid).await?
                }
            };
            let result = channel
                .request("list_panes", json!({ "tab_id": tid }))
                .await?;
            print_output(&result, json_mode, format_panes_human);
            Ok(())
        }

        // ── Create/split ──
        Some(Command::NewTab {
            command,
            cwd,
            title,
        }) => {
            let mut params = json!({});
            if let Some(c) = command {
                params["command"] = json!(c);
            }
            if let Some(d) = cwd {
                params["cwd"] = json!(d);
            }
            if let Some(t) = title {
                params["title"] = json!(t);
            }
            let result = wt_call("create_tab", params).await?;
            print_output(&result, json_mode, format_created_tab);
            Ok(())
        }
        Some(Command::SplitPane {
            target,
            horizontal,
            vertical,
            size,
            command,
        }) => {
            let channel = connect_channel().await?;
            let pane_id = resolve_pane_id(&channel, &target).await?;
            let split_dir = if horizontal {
                "horizontal"
            } else if vertical {
                "vertical"
            } else {
                "automatic"
            };
            let mut params = json!({
                "session_id": pane_id,
                "direction": split_dir,
            });
            if let Some(s) = size {
                params["size"] = json!(s);
            }
            if let Some(c) = command {
                params["command"] = json!(c);
            }
            let result = channel.request("split_pane", params).await?;
            print_output(&result, json_mode, format_created_pane);
            Ok(())
        }

        // ── Capture pane ──
        Some(Command::CapturePane {
            target,
            max_lines,
            last_prompt,
        }) => {
            let channel = connect_channel().await?;
            let pane_id = resolve_pane_id(&channel, &target).await?;
            let mut params = json!({ "session_id": pane_id });
            if let Some(n) = max_lines {
                params["max_lines"] = json!(n);
            }
            if last_prompt {
                params["source"] = json!("last_prompt");
            }
            let result = channel.request("read_pane_output", params).await?;
            if json_mode {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else if let Some(output) = result.get("content").and_then(|v| v.as_str()) {
                print!("{}", output);
            }
            Ok(())
        }

        // ── Kill pane ──
        Some(Command::KillPane { target }) => {
            let channel = connect_channel().await?;
            let pane_id = resolve_pane_id(&channel, &target).await?;
            channel
                .request("close_pane", json!({ "session_id": pane_id }))
                .await?;
            if !json_mode {
                println!("{}", t!("output.pane_closed", pane_id = pane_id));
            }
            Ok(())
        }

        // ── Active pane ──
        Some(Command::ActivePane) => {
            let result = wt_call("get_active_pane", json!({})).await?;
            print_output(&result, json_mode, format_active_pane);
            Ok(())
        }

        // ── Pane status ──
        Some(Command::PaneStatus { target }) => {
            let channel = connect_channel().await?;
            let pane_id = resolve_pane_id(&channel, &target).await?;
            let result = channel
                .request("get_process_status", json!({ "session_id": pane_id }))
                .await?;
            print_output(&result, json_mode, format_pane_status);
            Ok(())
        }

        // ── Wait for ──
        // Delegate to `wtcli wait-for` so the poll loop runs inside a single
        // wtcli process (one COM handshake) instead of re-spawning wtcli per
        // tick through CliChannel.
        Some(Command::WaitFor {
            target,
            interval,
            timeout,
        }) => {
            let wtcli = shell::wt_channel::resolve_wtcli_path();
            let interval_str = interval.to_string();
            let timeout_str = timeout.to_string();
            let output = tokio::process::Command::new(&wtcli)
                .args([
                    "--json",
                    "wait-for",
                    "-t",
                    &target,
                    "--interval",
                    &interval_str,
                    "--timeout",
                    &timeout_str,
                ])
                .output()
                .await
                .with_context(|| t!("error.wtcli_wait_for_spawn").into_owned())?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!(
                    "{}",
                    t!("error.wtcli_wait_for_failed", stderr = stderr.trim())
                );
            }

            let stdout = String::from_utf8_lossy(&output.stdout);
            let trimmed = stdout.trim();
            if !trimmed.is_empty() {
                let val: serde_json::Value = serde_json::from_str(trimmed)
                    .with_context(|| t!("error.wtcli_wait_for_parse").into_owned())?;
                print_output(&val, json_mode, format_pane_status);
            }
            Ok(())
        }

        // ── Pipe discovery ──
        Some(Command::PipeId) => run_pipe_id(json_mode),

        // ── Set environment variables ──
        Some(Command::SetEnv { shell }) => run_set_env(&shell),

        // ── Delegate prompt to new tab agent ──
        Some(Command::Delegate {
            prompt,
            agent,
            delegate_agent,
            delegate_model,
            cwd,
        }) => {
            run_delegate(
                prompt.as_deref(),
                &agent,
                delegate_agent.as_deref(),
                delegate_model.as_deref(),
                cwd.as_deref(),
            )
            .await
        }

        // ── Listen for events ──
        Some(Command::Listen { target }) => run_listen(target.as_deref()).await,

        // ── Master session registry CLI ──
        Some(Command::Sessions { action }) => match action {
            SessionsAction::List { master, origin } => {
                run_sessions_list(master, origin.to_filter(), json_mode).await
            }
        },

        // ── Manage agent hooks (install/status/uninstall) ──
        Some(Command::Hooks { action }) => match action {
            HooksAction::Install { cli } => run_hooks_install(cli),
            HooksAction::Status => run_hooks_status(json_mode),
            HooksAction::Uninstall { cli } => run_hooks_uninstall(cli, json_mode),
        },

        // ── ACP model list probe ──
        Some(Command::ProbeModels { agent }) => run_probe_models(&agent).await,

        // ── No subcommand: a singleton-service mode, or an error. There
        //    is no standalone/default ACP TUI mode — the direct agent-spawn
        //    path was removed, so bare `wta` always runs as a WT-launched
        //    agent pane via `--connect-master`:
        //    - `--master <pipe>`: wta-master (Z architecture; owns
        //      agent CLI, serves helper connections over named pipe)
        //    - `--connect-master <pipe>`: wta-helper (Z architecture;
        //      per-pane child that speaks ACP to master over the pipe)
        //    - neither: error — there is no standalone agent mode.
        None => {
            if let Some(pipe_name) = cli.master.clone() {
                master::run_master_mode(cli, pipe_name).await
            } else if let Some(pipe_name) = cli.connect_master.clone() {
                helper::run_helper_mode(cli, pipe_name).await
            } else {
                Err(anyhow::anyhow!(
                    "wta has no standalone agent mode: it runs as a Windows \
                     Terminal agent pane (launched by WT with --connect-master) \
                     or via a subcommand (delegate, hooks, sessions, …)"
                ))
            }
        }
    };

    // Last-resort diagnostic: any propagated failure (named-pipe connect,
    // agent spawn, ACP initialize, etc.) is otherwise only printed to stderr
    // and lost. Log it to file so connection failures are always recoverable
    // from the logs. Mode-specific context (target=master / target=helper)
    // is added closer to the source in run_master_mode / the helper path.
    if let Err(err) = &result {
        tracing::error!(error = ?err, "wta exiting with error");
    }
    // Flush the file appender before returning (its guard lives in a global,
    // not a local, so it is not dropped automatically on return).
    logging::shutdown_flush();
    result
}

/// Pick the log file label for this process from its launch mode. Drives the
/// `wta-<label>.log` filename in [`logging::init`]. Singleton-service modes are
/// selected by flags (`--master` / `--connect-master`); everything else by the
/// subcommand. Short-lived `wtcli`-style commands all share `cli`.
fn process_label(cli: &Cli) -> String {
    if cli.master.is_some() {
        return "main_master".to_string();
    }
    if cli.connect_master.is_some() {
        // Per-PID so concurrent per-tab helpers don't interleave into one
        // file (and can be reclaimed individually — see logging::housekeeping).
        return format!("main_helper-{}", std::process::id());
    }
    // Legacy diagnostic flags are short-lived clients, not the TUI.
    if cli.test_pipe || cli.info {
        return "cli".to_string();
    }
    match &cli.command {
        None => "main".to_string(),
        Some(Command::Delegate { .. }) => "delegate".to_string(),
        Some(Command::ProbeModels { .. }) => "probe".to_string(),
        Some(Command::Hooks {
            action: HooksAction::Install { .. },
        }) => "install-hooks".to_string(),
        // All other subcommands are short-lived wtcli-style clients.
        Some(_) => "cli".to_string(),
    }
}

/// Drive [`protocol::acp::probe::probe_models`] on a tokio `LocalSet`
/// (the ACP client connection is `!Send`), serialize the result to
/// stdout, force-exit. See exit notes below.
async fn run_probe_models(agent: &str) -> Result<()> {
    // Logging is initialized in `main()` (file, not stderr — the Settings UI
    // captures our stdout for the JSON payload and stderr would pollute it).
    tracing::info!("probe-models start: agent={}", agent);

    let local = tokio::task::LocalSet::new();
    let result = match local
        .run_until(protocol::acp::probe::probe_models(agent))
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("probe-models failed: {:#}", e);
            eprintln!("probe-models failed: {:#}", e);
            let _ = std::io::Write::flush(&mut std::io::stderr());
            // Flush the file appender — process::exit skips the guard drop.
            logging::shutdown_flush();
            // See exit rationale below.
            std::process::exit(1);
        }
    };
    tracing::info!(
        "probe-models ok: {} model(s), current={:?}",
        result.available_models.len(),
        result.current_model_id
    );
    let payload = serde_json::to_string(&result).context("serialize probe result")?;
    println!("{}", payload);

    // Force-exit before the tokio runtime tries to drop. The agent we
    // spawned is e.g. `cmd /c npx ...`; kill_on_drop kills cmd but
    // the npx → node grandchildren survive as orphans. Tokio's IOCP
    // reactor stays blocked on handles those orphans inherited and
    // the runtime drop hangs for ~35s. Runtime cleanup is meaningless
    // for a one-shot CLI — the caller is blocked on our process
    // handle, exit now. Orphan grandchildren self-exit shortly after
    // when they notice their pipes are broken.
    let _ = std::io::Write::flush(&mut std::io::stdout());
    // Flush the file appender — process::exit skips the guard drop.
    logging::shutdown_flush();
    std::process::exit(0);
}

// ─── Hooks subcommand handlers ──────────────────────────────────────────────

fn run_hooks_install(cli: HooksCliFilter) -> Result<()> {
    // Logging is initialized in `main()`; the install attempt is observable in
    // %LOCALAPPDATA%\IntelligentTerminal\logs\wta-install-hooks.log.
    let scope = cli.into_scope();
    agent_hooks_installer::ensure_installed_scoped(scope);

    // Verify the install actually landed by checking on-disk status.
    // ensure_installed_scoped is fire-and-forget (silent on failure),
    // so we inspect the result independently. `status_scoped(scope)`
    // skips the Node-CLI spawns for CLIs outside the requested scope —
    // a `--cli copilot` install no longer pays for `claude plugin list`
    // and `gemini extensions list` (each ~1-3s of Node startup).
    let report = agent_hooks_installer::status_scoped(scope);
    let failed: Vec<&str> = report
        .clis
        .iter()
        .filter(|c| {
            let in_scope = match scope {
                agent_hooks_installer::CliScope::All => true,
                agent_hooks_installer::CliScope::One(kind) => c.name == kind.name(),
            };
            // A CLI is "failed" if it's in scope, present on the machine
            // (cli_found), but hooks are not installed.
            in_scope && c.binary_on_path && !c.plugin_installed
        })
        .map(|c| c.name)
        .collect();

    if failed.is_empty() {
        println!("{}", t!("hooks.install_attempted"));
        Ok(())
    } else {
        let names = failed.join(", ");
        tracing::error!(target: "agent_hooks", clis = %names, "hooks install verification failed");
        anyhow::bail!("hooks installation failed for: {}", names)
    }
}

fn run_hooks_status(json_mode: bool) -> Result<()> {
    let report = agent_hooks_installer::status();
    if json_mode {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .unwrap_or_else(|_| serde_json::to_string(&report).unwrap_or_default())
        );
    } else {
        format_hooks_status_human(&report);
    }
    Ok(())
}

fn run_hooks_uninstall(cli: HooksCliFilter, json_mode: bool) -> Result<()> {
    let report = agent_hooks_installer::uninstall(cli.into_scope());
    if json_mode {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .unwrap_or_else(|_| serde_json::to_string(&report).unwrap_or_default())
        );
    } else {
        format_hooks_uninstall_human(&report);
    }
    Ok(())
}

fn format_hooks_status_human(r: &agent_hooks_installer::StatusReport) {
    let path_suffix = r
        .bundle_source
        .path
        .as_deref()
        .map(|p| format!(" ({})", p))
        .unwrap_or_default();
    println!(
        "{}",
        t!(
            "hooks.bundle_source",
            source = r.bundle_source.kind,
            path_suffix = path_suffix,
        )
    );
    println!();
    for c in &r.clis {
        let summary = if !c.binary_on_path {
            t!("hooks.cli_not_on_path").into_owned()
        } else if c.plugin_installed && c.plugin_enabled && c.marketplace_path_valid {
            t!("hooks.installed").into_owned()
        } else if c.plugin_installed && !c.marketplace_path_valid {
            t!("hooks.marketplace_path_stale").into_owned()
        } else if c.plugin_installed {
            t!("hooks.installed_but_disabled").into_owned()
        } else {
            t!("hooks.not_installed").into_owned()
        };
        let detail = format!(
            "marketplace={}, path_valid={}, plugin={}, enabled={}{}",
            yn(c.marketplace_registered),
            yn(c.marketplace_path_valid),
            yn(c.plugin_installed),
            yn(c.plugin_enabled),
            c.detection_fallback
                .map(|m| format!(", detection={}", m))
                .unwrap_or_default(),
        );
        println!("  {:<10} {:<28}  ({})", c.name, summary, detail);
        if let Some(p) = c.marketplace_path.as_deref() {
            println!("    path: {}", p);
        }
    }
}

fn format_hooks_uninstall_human(r: &agent_hooks_installer::UninstallReport) {
    for c in &r.clis {
        let summary = if !c.attempted {
            t!("hooks.uninstall_skipped").into_owned()
        } else {
            let plugin = c
                .plugin_uninstalled
                .map(|b| if b { "ok" } else { "failed" })
                .unwrap_or("-");
            let mkt = c
                .marketplace_removed
                .map(|b| if b { "ok" } else { "failed" })
                .unwrap_or("-");
            format!(
                "plugin={} marketplace={} staging={}",
                plugin,
                mkt,
                if c.staging_dir_removed {
                    "ok"
                } else {
                    "failed"
                },
            )
        };
        println!("  {:<10} {}", c.name, summary);
        for m in &c.messages {
            println!("    \u{00b7} {}", m);
        }
    }
}

fn yn(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

// ─── Helper: connect to WT COM protocol (no debug channel, no ShellManager) ─────────

async fn connect_channel() -> Result<CliChannel> {
    CliChannel::connect().await
}

/// Single-shot: connect + call + return JSON
async fn wt_call(method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    let channel = connect_channel().await?;
    channel.request(method, params).await
}

/// Resolve -t target: Some(id) -> use it, None -> get_active_pane fallback
async fn resolve_pane_id(channel: &CliChannel, target: &Option<String>) -> Result<String> {
    match target {
        Some(id) => Ok(id.clone()),
        None => {
            let result = channel.request("get_active_pane", json!({})).await?;
            let pane_id = result
                .get("session_id")
                .and_then(|v| match v {
                    serde_json::Value::String(s) => Some(s.clone()),
                    serde_json::Value::Number(n) => Some(n.to_string()),
                    _ => None,
                })
                .ok_or_else(|| anyhow::anyhow!("{}", t!("error.no_active_pane")))?;
            Ok(pane_id)
        }
    }
}

/// Get the first window ID from list_windows.
async fn get_first_window_id(channel: &CliChannel) -> Result<String> {
    let result = channel.request("list_windows", json!({})).await?;
    result
        .get("windows")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|w| w.get("window_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("{}", t!("output.no_windows_in_list")))
}

/// Get the first tab ID from a window.
async fn get_first_tab_id(channel: &CliChannel, window_id: &str) -> Result<String> {
    let result = channel
        .request("list_tabs", json!({ "window_id": window_id }))
        .await?;
    result
        .get("tabs")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|t| match t.get("tab_id") {
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            Some(serde_json::Value::Number(n)) => Some(n.to_string()),
            _ => None,
        })
        .ok_or_else(|| anyhow::anyhow!("{}", t!("output.no_tabs_in_window", window_id = window_id)))
}


// ─── sessions CLI helpers ───────────────────────────────────────────────────

const MASTER_NOT_RUNNING: &str = "wta-master not running. Start Windows Terminal first.";

struct SessionsCliClient;

#[async_trait::async_trait(?Send)]
impl acp::Client for SessionsCliClient {
    async fn request_permission(
        &self,
        _args: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        Err(acp::Error::internal_error().data("sessions CLI cannot answer permission requests"))
    }

    async fn session_notification(&self, _args: acp::SessionNotification) -> acp::Result<()> {
        Ok(())
    }
}

async fn run_sessions_list(
    master_override: Option<String>,
    origin_filter: agent_sessions::OriginFilter,
    json_mode: bool,
) -> Result<()> {
    let local = tokio::task::LocalSet::new();
    let sessions = local
        .run_until(fetch_sessions_from_master(master_override))
        .await?;
    // Origin filter is applied client-side: master always returns the
    // full registry so this command can act as the debug eye-of-god
    // view (default `--origin all`). `--origin shell` matches what
    // the MVP sessions picker shows; `--origin agent-pane` surfaces the
    // rows MVP sessions hides.
    let filtered: Vec<session_registry::SessionInfo> = sessions
        .into_iter()
        .filter(|s| origin_filter.matches_opt(s.origin.as_ref()))
        .collect();
    if json_mode {
        print!("{}", format_sessions_json_lines(&filtered)?);
    } else {
        print!("{}", format_sessions_table(&filtered));
    }
    Ok(())
}

async fn fetch_sessions_from_master(
    master_override: Option<String>,
) -> Result<Vec<session_registry::SessionInfo>> {
    let pipe_name = resolve_master_pipe(master_override).await?;
    let pipe = open_master_pipe_for_cli(&pipe_name).await?;
    let (read_half, write_half) = tokio::io::split(pipe);
    let outgoing = write_half.compat_write();
    let incoming = read_half.compat();
    let (conn, handle_io) = acp::ClientSideConnection::new(SessionsCliClient, outgoing, incoming, |fut| {
        tokio::task::spawn_local(fut);
    });
    tokio::task::spawn_local(async move {
        let _ = handle_io.await;
    });

    let init_started = std::time::Instant::now();
    let init_result = conn.initialize(
        acp::InitializeRequest::new(acp::ProtocolVersion::V1)
            .client_capabilities(acp::ClientCapabilities::new())
            .client_info(
                acp::Implementation::new("wta-sessions", env!("CARGO_PKG_VERSION"))
                    .title("Windows Terminal Agent sessions CLI"),
            ),
    )
    .await;
    telemetry::log_acp_initialize_complete(
        init_started.elapsed().as_secs_f64() * 1000.0,
        init_result.is_ok(),
        "SessionsCli",
        if init_result.is_ok() { "" } else { "AcpError" },
        init_result
            .as_ref()
            .err()
            .map(|e| e.code.into())
            .unwrap_or(0),
    );
    init_result.map_err(|_| anyhow::anyhow!(MASTER_NOT_RUNNING))?;

    let req = session_registry::build_sessions_list_request();
    let resp = conn
        .ext_method(req)
        .await
        .map_err(|_| anyhow::anyhow!(MASTER_NOT_RUNNING))?;
    let parsed = session_registry::parse_sessions_list_response(&resp.0)
        .context("parse sessions/list response")?;
    Ok(parsed.sessions)
}

/// Best-effort: register a WTA-launched CLI session with `wta-master` as a
/// *born-bound* row — bound to its pane, with no hooks involved. Sends a
/// `SessionStarted` over the `intellterm.wta/session_born_bound` method, which
/// the master turns into a Class-B (`origin = Unknown`) row whose
/// `pane_session_id` is the pane we just created and records as binding-only
/// (so the file watcher may still supply activity/status when no hook is
/// installed). Best-effort: if master is unreachable there is no registry to
/// populate, so the registration is dropped (logged at `warn`) and the tab
/// still opens normally.
async fn register_launched_session_with_master(
    session_id: &str,
    pane_session_id: &str,
    cli_id: &str,
    cwd: Option<&str>,
) {
    let event = crate::agent_sessions::SessionEvent::SessionStarted {
        key: session_id.to_string(),
        cli_source: crate::agent_sessions::CliSource::from(
            crate::session_registry::SessionHookCliSource::Known(cli_id.to_string()),
        ),
        pane_session_id: pane_session_id.to_string(),
        cwd: cwd.map(std::path::PathBuf::from).unwrap_or_default(),
        // Empty title: the master refreshes the row's title from the CLI's
        // on-disk session artefacts once they appear.
        title: String::new(),
    };
    let req = session_registry::build_born_bound_request(&event);

    // Own LocalSet so the `spawn_local` transport works regardless of how the
    // delegate's runtime was set up (mirrors `run_sessions_list`).
    let local = tokio::task::LocalSet::new();
    let result: Result<()> = local
        .run_until(async move {
            let pipe_name = resolve_master_pipe(None).await?;
            let pipe = open_master_pipe_for_cli(&pipe_name).await?;
            let (read_half, write_half) = tokio::io::split(pipe);
            let outgoing = write_half.compat_write();
            let incoming = read_half.compat();
            let (conn, handle_io) =
                acp::ClientSideConnection::new(SessionsCliClient, outgoing, incoming, |fut| {
                    tokio::task::spawn_local(fut);
                });
            tokio::task::spawn_local(async move {
                let _ = handle_io.await;
            });

            conn.initialize(
                acp::InitializeRequest::new(acp::ProtocolVersion::V1)
                    .client_capabilities(acp::ClientCapabilities::new())
                    .client_info(
                        acp::Implementation::new("wta-delegate", env!("CARGO_PKG_VERSION"))
                            .title("Windows Terminal Agent delegate"),
                    ),
            )
            .await
            .map_err(|_| anyhow::anyhow!(MASTER_NOT_RUNNING))?;

            conn.ext_method(req)
                .await
                .map_err(|_| anyhow::anyhow!(MASTER_NOT_RUNNING))?;
            Ok(())
        })
        .await;

    if let Err(e) = result {
        tracing::warn!(
            target: "delegate",
            error = %e,
            "register born-bound session with master failed (best-effort)"
        );
    }
}

async fn resolve_master_pipe(master_override: Option<String>) -> Result<String> {
    if let Some(pipe) = master_override.filter(|s| !s.trim().is_empty()) {
        return Ok(pipe);
    }

    for attempt in 0..2 {
        if let Some(path) = runtime_paths::master_pipe_file_path() {
            if let Ok(contents) = std::fs::read_to_string(path) {
                let pipe = contents.trim();
                if !pipe.is_empty() {
                    return Ok(pipe.to_string());
                }
            }
        }
        if attempt == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
    Err(anyhow::anyhow!(MASTER_NOT_RUNNING))
}

async fn open_master_pipe_for_cli(
    pipe_name: &str,
) -> Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    for attempt in 0..2 {
        match tokio::net::windows::named_pipe::ClientOptions::new().open(pipe_name) {
            Ok(pipe) => return Ok(pipe),
            Err(_) if attempt == 0 => {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await
            }
            Err(_) => return Err(anyhow::anyhow!(MASTER_NOT_RUNNING)),
        }
    }
    Err(anyhow::anyhow!(MASTER_NOT_RUNNING))
}

fn format_sessions_json_lines(sessions: &[session_registry::SessionInfo]) -> Result<String> {
    let mut out = String::new();
    for session in sessions {
        out.push_str(&serde_json::to_string(session)?);
        out.push('\n');
    }
    Ok(out)
}

fn format_sessions_table(sessions: &[session_registry::SessionInfo]) -> String {
    let mut out = String::new();
    if sessions.is_empty() {
        out.push_str("No sessions.\n");
        return out;
    }
    out.push_str(&format!(
        "{:<24} {:<10} {:<10} {:<10} {:<20} {:<20} {}\n",
        "SESSION", "STATUS", "CLI", "ORIGIN", "PANE", "UPDATED", "TITLE"
    ));
    for session in sessions {
        let sid = session.session_id.to_string();
        let short_sid = if sid.len() > 24 { &sid[..24] } else { sid.as_str() };
        out.push_str(&format!(
            "{:<24} {:<10} {:<10} {:<10} {:<20} {:<20} {}\n",
            short_sid,
            status_label(session.status.as_ref()),
            cli_source_label(session.cli_source.as_ref()),
            origin_label(session.origin.as_ref()),
            session.pane_session_id.as_deref().unwrap_or("-"),
            session.updated_at.as_deref().unwrap_or("-"),
            session.title.as_deref().unwrap_or("-"),
        ));
    }
    out
}

fn status_label(status: Option<&agent_sessions::AgentStatus>) -> String {
    status.map(|s| format!("{s:?}")).unwrap_or_else(|| "-".to_string())
}

fn cli_source_label(source: Option<&agent_sessions::CliSource>) -> String {
    match source {
        Some(agent_sessions::CliSource::Claude)  => "Claude".to_string(),
        Some(agent_sessions::CliSource::Codex)   => "Codex".to_string(),
        Some(agent_sessions::CliSource::Copilot) => "Copilot".to_string(),
        Some(agent_sessions::CliSource::Gemini)  => "Gemini".to_string(),
        Some(agent_sessions::CliSource::Unknown(s)) if !s.is_empty() => s.clone(),
        _ => "-".to_string(),
    }
}

/// Render a `SessionOrigin` for the `wta sessions list` table. `None`
/// is the on-the-wire representation for "field absent" (legacy rows
/// or notification paths that don't carry origin) — we print `-`
/// rather than fabricating an origin so the operator can tell
/// "untagged" from "shell".
fn origin_label(origin: Option<&agent_sessions::SessionOrigin>) -> &'static str {
    match origin {
        Some(agent_sessions::SessionOrigin::AgentPane) => "AgentPane",
        Some(agent_sessions::SessionOrigin::Unknown)   => "Shell",
        None                                           => "-",
    }
}

// ─── Output helpers ─────────────────────────────────────────────────────────

fn print_output(val: &serde_json::Value, json_mode: bool, formatter: fn(&serde_json::Value)) {
    if json_mode {
        println!(
            "{}",
            serde_json::to_string_pretty(val).unwrap_or_else(|_| val.to_string())
        );
    } else {
        formatter(val);
    }
}

fn format_windows_human(val: &serde_json::Value) {
    if let Some(windows) = val.get("windows").and_then(|v| v.as_array()) {
        if windows.is_empty() {
            println!("{}", t!("output.no_windows"));
            return;
        }
        println!("{}", t!("output.header.windows"));
        for w in windows {
            let id = json_str_or_num(w, "window_id");
            let title = w.get("title").and_then(|v| v.as_str()).unwrap_or("-");
            let focused = w
                .get("is_focused")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            println!(
                "{:<12} {:<30} {}",
                id,
                title,
                if focused { "*" } else { "" }
            );
        }
    } else {
        println!("{}", serde_json::to_string_pretty(val).unwrap_or_default());
    }
}

fn format_tabs_human(val: &serde_json::Value) {
    if let Some(tabs) = val.get("tabs").and_then(|v| v.as_array()) {
        if tabs.is_empty() {
            println!("{}", t!("output.no_tabs"));
            return;
        }
        println!("{}", t!("output.header.tabs"));
        for t in tabs {
            let id = json_str_or_num(t, "tab_id");
            let title = t.get("title").and_then(|v| v.as_str()).unwrap_or("-");
            let focused = t
                .get("is_active")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            println!(
                "{:<10} {:<30} {}",
                id,
                title,
                if focused { "*" } else { "" }
            );
        }
    } else {
        println!("{}", serde_json::to_string_pretty(val).unwrap_or_default());
    }
}

fn format_panes_human(val: &serde_json::Value) {
    if let Some(panes) = val.get("panes").and_then(|v| v.as_array()) {
        if panes.is_empty() {
            println!("{}", t!("output.no_panes"));
            return;
        }
        println!("{}", t!("output.header.panes"));
        for p in panes {
            let id = json_str_or_num(p, "session_id");
            let pid = p
                .get("pid")
                .and_then(|v| v.as_u64())
                .map(|n| n.to_string())
                .unwrap_or_else(|| "-".to_string());
            let active = p
                .get("is_active")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let size = p.get("size");
            let rows = size
                .and_then(|s| s.get("rows"))
                .and_then(|v| v.as_u64())
                .map(|n| n.to_string())
                .unwrap_or_else(|| "-".to_string());
            let cols = size
                .and_then(|s| s.get("columns"))
                .and_then(|v| v.as_u64())
                .map(|n| n.to_string())
                .unwrap_or_else(|| "-".to_string());
            println!(
                "{:<10} {:<8} {:<8} {:<10} {}",
                id,
                pid,
                if active { "*" } else { "" },
                rows,
                cols
            );
        }
    } else {
        println!("{}", serde_json::to_string_pretty(val).unwrap_or_default());
    }
}

fn format_active_pane(val: &serde_json::Value) {
    let id = json_str_or_num(val, "session_id");
    let tab = json_str_or_num(val, "tab_id");
    let win = json_str_or_num(val, "window_id");
    println!(
        "{}",
        t!("output.active_pane", pane = id, tab = tab, window = win)
    );
}

fn format_pane_status(val: &serde_json::Value) {
    let state = val
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let running = state == "running";
    let exit_code = val
        .get("exit_code")
        .and_then(|v| v.as_i64())
        .map(|n| n.to_string())
        .unwrap_or_else(|| "-".to_string());
    let pid = val
        .get("pid")
        .and_then(|v| v.as_u64())
        .map(|n| n.to_string())
        .unwrap_or_else(|| "-".to_string());
    if running {
        println!("{}", t!("output.pane_running", pid = pid));
    } else {
        println!("{}", t!("output.pane_exited", code = exit_code, pid = pid));
    }
}

fn format_created_tab(val: &serde_json::Value) {
    let tab_id = json_str_or_num(val, "tab_id");
    let pane_id = json_str_or_num(val, "session_id");
    println!(
        "{}",
        t!("output.created_tab", tab_id = tab_id, pane_id = pane_id)
    );
}

fn format_created_pane(val: &serde_json::Value) {
    let pane_id = json_str_or_num(val, "session_id");
    println!("{}", t!("output.created_pane", pane_id = pane_id));
}

/// Extract a field that may be string or number from JSON.
fn json_str_or_num(val: &serde_json::Value, key: &str) -> String {
    match val.get(key) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => "-".to_string(),
    }
}

// ─── pipe-id / set-env: surface the inherited WT_COM_CLSID env var ─────────

fn run_pipe_id(json_mode: bool) -> Result<()> {
    let clsid = std::env::var("WT_COM_CLSID")
        .map_err(|_| anyhow::anyhow!("{}", t!("error.wt_com_clsid_not_set")))?;
    if json_mode {
        let val = json!({ "connection_id": clsid, "env": "WT_COM_CLSID" });
        println!("{}", serde_json::to_string_pretty(&val)?);
    } else {
        println!("{}", clsid);
    }
    Ok(())
}

fn run_set_env(shell_type: &str) -> Result<()> {
    let clsid = std::env::var("WT_COM_CLSID")
        .map_err(|_| anyhow::anyhow!("{}", t!("error.wt_com_clsid_not_set")))?;

    match shell_type {
        "bash" | "sh" | "zsh" => {
            println!("export WT_COM_CLSID='{}'", clsid);
            eprintln!("# Run: eval \"$(wta set-env)\"");
        }
        "powershell" | "pwsh" | "ps" => {
            println!("$env:WT_COM_CLSID = '{}'", clsid);
            eprintln!("# Run: wta set-env -s powershell | Invoke-Expression");
        }
        "cmd" => {
            println!("set WT_COM_CLSID={}", clsid);
            eprintln!("REM Run in a for /f loop or copy-paste");
        }
        "fish" => {
            println!("set -gx WT_COM_CLSID '{}'", clsid);
            eprintln!("# Run: wta set-env -s fish | source");
        }
        other => {
            bail!("{}", t!("error.unknown_shell_type", shell = other));
        }
    }

    Ok(())
}

// ─── Listen mode ────────────────────────────────────────────────────────────

async fn run_listen(pane_filter: Option<&str>) -> Result<()> {
    let channel = connect_channel().await?;
    let arc_channel = std::sync::Arc::new(channel);

    // Subscribe to events and start the background reader.
    let mut event_rx = arc_channel.subscribe_events();
    arc_channel.start_reader().await;

    // Send any request to trigger lazy page event registration on the server.
    let _ = arc_channel.request("get_capabilities", json!({})).await;

    eprintln!("Connected. Listening for events... (Ctrl+C to stop)");
    if let Some(pane) = pane_filter {
        eprintln!("Filtering: pane_id={}", pane);
    }

    while let Some(msg) = event_rx.recv().await {
        // Only print events, skip responses.
        if msg.get("type").and_then(|v| v.as_str()) != Some("event") {
            continue;
        }

        // Optional pane_id filter.
        if let Some(filter) = pane_filter {
            let pane_id = msg
                .get("params")
                .and_then(|p| p.get("session_id"))
                .and_then(|v| v.as_str());
            if pane_id != Some(filter) {
                continue;
            }
        }

        // Re-serialize to guarantee compact single-line JSON (safe for jq piping).
        println!("{}", serde_json::to_string(&msg).unwrap_or_default());
    }

    eprintln!("Event stream closed.");
    Ok(())
}

// ─── Delegate prompt to new tab agent ────────────────────────────────────────

async fn run_delegate(
    prompt: Option<&str>,
    agent_cmd: &str,
    delegate_agent_cmd: Option<&str>,
    delegate_model: Option<&str>,
    cwd: Option<&str>,
) -> Result<()> {
    // Log the prompt length, not the text — the prompt is user content.
    tracing::info!(prompt_chars = prompt.map(|p| p.chars().count()), agent = agent_cmd, "run_delegate started");
    tracing::trace!(target: "delegate.content", prompt = ?prompt, "run_delegate prompt");

    let (debug_tx, _) = tokio::sync::mpsc::unbounded_channel::<app::DebugMessage>();
    let channel = match connect_to_wt_protocol(debug_tx).await {
        Ok(ch) => {
            tracing::info!("WT protocol connected");
            ch
        }
        Err(e) => {
            tracing::warn!(error = %e, "WT protocol connection FAILED");
            return Err(e);
        }
    };
    let shell_mgr = ShellManager::new()
        .with_wt_channel(Arc::new(channel) as Arc<dyn shell::wt_channel::WtChannel>);

    match delegate_with_context(
        &shell_mgr,
        prompt,
        agent_cmd,
        delegate_agent_cmd,
        delegate_model,
        cwd,
    )
    .await
    {
        Ok(()) => {
            tracing::info!("delegate OK");
            Ok(())
        }
        Err(e) => {
            tracing::warn!(error = %e, "delegate FAILED");
            Err(e)
        }
    }
}

/// Shared delegation logic: enrich the prompt with the active pane's recent
/// output (when available), build the delegate-agent commandline, and create a
/// new tab to launch it. WT's GetActivePane already resolves the agent pane to
/// the user's working pane, so a single query is enough.
async fn delegate_with_context(
    shell_mgr: &ShellManager,
    prompt: Option<&str>,
    agent_cmd: &str,
    delegate_agent_cmd: Option<&str>,
    delegate_model: Option<&str>,
    cwd: Option<&str>,
) -> Result<()> {
    let delegate_agents = crate::coordinator::default_delegate_agent_runtimes(
        delegate_agent_cmd,
        Some(agent_cmd),
        delegate_model,
    );
    let runtime = delegate_agents
        .first()
        .ok_or_else(|| anyhow::anyhow!("no delegate agent configured"))?;

    // Pin a session id we choose, so the launched CLI writes its session under a
    // known id and we can bind it to the pane without hooks. Only for agents that
    // advertise `--session-id` (Copilot/Claude/Gemini); `None` otherwise. We
    // identify the agent with `resolve_agent_id_from_cmd` (not a naive
    // `split_whitespace`) so quoted/space-containing paths and adapter launches
    // resolve correctly -- and so this decision matches the one the command
    // builder makes when it appends the flag, keeping the pinned id and the
    // actual launch flag in agreement.
    let pinned_session_id: Option<String> = crate::agent_registry::lookup_profile_by_id(
        crate::agent_registry::resolve_agent_id_from_cmd(&runtime.commandline),
    )
    .new_session_id_flag
    .map(|_| uuid::Uuid::new_v4().to_string());

    let commandline = match prompt {
        // Prompt present → enrich it with the active pane's recent output and
        // bake it into the new tab's agent CLI (the `?<prompt>` path).
        Some(prompt) if !prompt.trim().is_empty() => {
            let active = shell_mgr.wt_get_active_pane().await.ok();
            let active_pane_id = active
                .as_ref()
                .and_then(|v| v.get("session_id"))
                .and_then(|v| match v {
                    serde_json::Value::String(s) => Some(s.clone()),
                    serde_json::Value::Number(n) => Some(n.to_string()),
                    _ => None,
                });

            let pane_context = if let Some(ref pane_id) = active_pane_id {
                match shell_mgr.wt_read_pane_output(pane_id, Some(30)).await {
                    Ok(value) => value
                        .get("content")
                        .and_then(|c| c.as_str())
                        .map(|s| s.to_string()),
                    Err(_) => None,
                }
            } else {
                None
            };

            let full_prompt = match (pane_context, active_pane_id) {
                (Some(context), Some(pane_id)) => format!(
                    "{}\n\n## Terminal Context (pane {})\n```\n{}\n```",
                    prompt, pane_id, context
                ),
                _ => prompt.to_string(),
            };

            crate::coordinator::build_delegate_launch_commandline_with_session(
                runtime,
                Some(&full_prompt),
                pinned_session_id.as_deref(),
            )?
        }
        // No prompt → open the delegate agent interactively in the new tab.
        _ => crate::coordinator::build_delegate_launch_commandline_with_session(
            runtime,
            None,
            pinned_session_id.as_deref(),
        )?,
    };

    // The commandline bakes in the user prompt (`-i "<prompt>"`); keep it out
    // of the debug log and only emit it at trace.
    tracing::debug!("delegate_with_context: launching");
    tracing::trace!(target: "delegate.content", commandline, "delegate_with_context commandline");

    let create_resp = shell_mgr
        .wt_create_tab(Some(&commandline), cwd, None)
        .await?;
    let pane_guid = create_resp
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    tracing::info!(
        target: "delegate",
        pane_guid = ?pane_guid,
        pinned = ?pinned_session_id,
        "delegate tab created",
    );

    // Born-bound registration: WTA created this tab and pinned the CLI's
    // session id, so we know (session id, pane) at launch. Tell master to
    // bind them with no hooks (best-effort). Only when both are known —
    // i.e. a pinnable agent (Copilot/Claude/Gemini) whose tab was created.
    if let (Some(sid), Some(pane)) = (pinned_session_id.as_deref(), pane_guid.as_deref()) {
        register_launched_session_with_master(sid, pane, &runtime.id, cwd).await;
    }

    Ok(())
}

// ─── Default ACP TUI mode ───────────────────────────────────────────────────

/// Drive the standard ACP TUI but use `pipe_name` as the ACP transport
/// (helper mode). The helper attaches to wta-master over the supplied
/// named pipe and forwards ACP traffic over it.
pub(crate) async fn run_default_tui_over_pipe(cli: Cli, pipe_name: String) -> Result<()> {
    tracing::info!(target: "helper", pipe = %pipe_name, "=== wta-helper starting (TUI) ===");

    // Debug channel for the helper TUI.
    let (debug_tx, debug_rx) = tokio::sync::mpsc::unbounded_channel::<app::DebugMessage>();

    let mut shell_mgr = ShellManager::new();
    let mut wt_event_rx = None;
    let mut wt_protocol_channel: Option<Arc<CliChannel>> = None;
    let wt_connected = match connect_to_wt_protocol(debug_tx.clone()).await {
        Ok(channel) => {
            tracing::info!(target: "helper", "Connected to WT COM protocol — subscribing to events");
            wt_event_rx = Some(channel.subscribe_events());
            let cli_arc = Arc::new(channel);
            wt_protocol_channel = Some(Arc::clone(&cli_arc));
            shell_mgr =
                shell_mgr.with_wt_channel(cli_arc.clone() as Arc<dyn shell::wt_channel::WtChannel>);
            true
        }
        Err(e) => {
            tracing::warn!(target: "helper", error = %e, "NO WT protocol connection");
            false
        }
    };
    let shell_mgr = Arc::new(shell_mgr);

    let pane_identity = if wt_connected {
        discover_pane_identity(&shell_mgr).await
    } else {
        None
    };

    // Connection failures to wta-master (pipe connect give-up, ACP initialize
    // timeout/failure) are logged at their source (target=helper) and again in
    // `run_acp_tui_mode`'s exit branch, which `process::exit`s rather than
    // returning Err — so there's no point wrapping the result here.
    run_acp_tui_mode(
        cli,
        shell_mgr,
        wt_connected,
        debug_rx,
        pane_identity,
        wt_event_rx,
        wt_protocol_channel,
        pipe_name,
    )
    .await
}

// ─── Existing functions (preserved) ─────────────────────────────────────────

/// Discover our own pane identity by matching our PID against WT's pane list.
async fn discover_pane_identity(shell_mgr: &ShellManager) -> Option<(String, String, String)> {
    let our_pid = std::process::id();

    let windows = shell_mgr.wt_list_windows().await.ok()?;
    let windows_arr = windows.get("windows")?.as_array()?;

    for win in windows_arr {
        let window_id = win.get("window_id")?.as_str()?;
        let tabs = shell_mgr.wt_list_tabs(window_id).await.ok()?;
        let tabs_arr = tabs.get("tabs")?.as_array()?;

        for tab in tabs_arr {
            let tab_id_str = match tab.get("tab_id") {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(serde_json::Value::Number(n)) => n.to_string(),
                _ => continue,
            };
            let panes = shell_mgr.wt_list_panes(&tab_id_str).await.ok()?;
            let panes_arr = panes.get("panes")?.as_array()?;

            for pane in panes_arr {
                if let Some(pid) = pane.get("pid").and_then(|v| v.as_u64()) {
                    if pid == our_pid as u64 {
                        let pane_id = match pane.get("session_id") {
                            Some(serde_json::Value::String(s)) => s.clone(),
                            Some(serde_json::Value::Number(n)) => n.to_string(),
                            _ => continue,
                        };
                        return Some((pane_id, tab_id_str.clone(), window_id.to_string()));
                    }
                }
            }
        }
    }
    None
}

async fn run_acp_tui_mode(
    cli: Cli,
    shell_mgr: Arc<ShellManager>,
    wt_connected: bool,
    debug_rx: tokio::sync::mpsc::UnboundedReceiver<app::DebugMessage>,
    pane_identity: Option<(String, String, String)>,
    wt_event_rx: Option<tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>>,
    wt_protocol_channel: Option<Arc<CliChannel>>,
    connect_master_pipe: String,
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // NOTE: We intentionally do NOT call EnableMouseCapture. Without mouse
    // tracking, the host terminal emulator (Windows Terminal, xterm, kitty,
    // alacritty, wezterm) translates mouse-wheel events into Up/Down arrow
    // keystrokes while we are in the alternate screen buffer. That gives us
    // wheel-driven chat scrolling for free, and — crucially — leaves native
    // click-drag text selection working so users can highlight and copy
    // from the agent pane the way they would from any other terminal.
    execute!(stdout, EnterAlternateScreen)?;
    // Deliberately do NOT emit `OSC 11` to force a background color: the pane
    // must inherit the profile's color scheme background so it tracks the
    // user's theme like any other pane (#234). Cells render on the terminal's
    // default (scheme) background; `Color::Reset` resolves to it.
    // Steady block (DECSCUSR Ps=2): solid filled rectangle, no blink.
    // Survives the alt-screen swap; restored on exit below.
    execute!(stdout, SetCursorStyle::SteadyBlock)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_acp_app(
        &mut terminal,
        cli,
        shell_mgr,
        wt_connected,
        debug_rx,
        pane_identity,
        wt_event_rx,
        wt_protocol_channel,
        connect_master_pipe,
    )
    .await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        SetCursorStyle::DefaultUserShape,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    if let Err(e) = result {
        // This is the real exit point for a TUI/helper failure (connection
        // failures to wta-master propagate up to here). `process::exit` below
        // bypasses both `main()`'s catch-all and any caller's wrapper, so log
        // it here before exiting — it lands in this process's log file
        // (wta-main_helper-{pid}.log in helper mode).
        tracing::error!(error = ?e, "wta TUI exiting with error");
        eprintln!("Error: {e:?}");
        // Flush the file appender — process::exit skips the guard drop.
        logging::shutdown_flush();
        std::process::exit(1);
    }
    Ok(())
}

async fn run_test_pipe() -> Result<()> {
    use shell::wt_channel::WtChannel;

    println!("Connecting to Windows Terminal protocol...");
    let channel = connect_channel().await?;
    println!("Connected and authenticated!\n");

    let result: serde_json::Value = channel
        .request("list_windows", serde_json::json!({}))
        .await?;
    println!("list_windows:");
    println!("{}\n", serde_json::to_string_pretty(&result)?);

    let result: serde_json::Value = channel
        .request("get_capabilities", serde_json::json!({}))
        .await?;
    println!("get_capabilities:");
    println!("{}", serde_json::to_string_pretty(&result)?);

    Ok(())
}

/// Try to connect to the WT protocol via the inherited WT_COM_CLSID env var.
async fn connect_to_wt_protocol(
    debug_tx: tokio::sync::mpsc::UnboundedSender<app::DebugMessage>,
) -> Result<shell::wt_channel::CliChannel> {
    use shell::wt_channel::CliChannel;
    let channel = CliChannel::connect().await?;
    Ok(channel.with_debug_sender(debug_tx))
}

/// Show Windows Terminal protocol connection info and pane identity.
async fn run_info_mode() -> Result<()> {
    use shell::wt_channel::WtChannel;

    println!("Windows Terminal Protocol Info");
    println!("========================================");

    let clsid = match std::env::var("WT_COM_CLSID") {
        Ok(v) => v,
        Err(_) => {
            println!("  Status: Not running inside Windows Terminal");
            println!("  (WT_COM_CLSID not set)");
            return Ok(());
        }
    };

    println!("  COM CLSID: {}", clsid);
    println!("  Source: WT_COM_CLSID env var");
    println!();

    let channel = match CliChannel::connect().await {
        Ok(ch) => ch,
        Err(e) => {
            println!("  Connection failed: {}", e);
            return Ok(());
        }
    };

    let our_pid = std::process::id();
    let mut pane_info: Option<(String, String, String)> = None;
    let mut total_windows = 0u32;
    let mut total_tabs = 0u32;
    let mut total_panes = 0u32;

    if let Ok(windows) = channel.request("list_windows", serde_json::json!({})).await {
        if let Some(windows_arr) = windows.get("windows").and_then(|v| v.as_array()) {
            total_windows = windows_arr.len() as u32;

            for win in windows_arr {
                let window_id = match win.get("window_id").and_then(|v| v.as_str()) {
                    Some(id) => id,
                    None => continue,
                };

                if let Ok(tabs) = channel
                    .request("list_tabs", serde_json::json!({ "window_id": window_id }))
                    .await
                {
                    if let Some(tabs_arr) = tabs.get("tabs").and_then(|v| v.as_array()) {
                        total_tabs += tabs_arr.len() as u32;

                        for tab in tabs_arr {
                            let tab_id_str = match tab.get("tab_id") {
                                Some(serde_json::Value::String(s)) => s.clone(),
                                Some(serde_json::Value::Number(n)) => n.to_string(),
                                _ => continue,
                            };

                            if let Ok(panes) = channel
                                .request("list_panes", serde_json::json!({ "tab_id": tab_id_str }))
                                .await
                            {
                                if let Some(panes_arr) =
                                    panes.get("panes").and_then(|v| v.as_array())
                                {
                                    total_panes += panes_arr.len() as u32;

                                    for pane in panes_arr {
                                        if let Some(pid) = pane.get("pid").and_then(|v| v.as_u64())
                                        {
                                            if pid == our_pid as u64 {
                                                let pane_id = match pane.get("session_id") {
                                                    Some(serde_json::Value::String(s)) => s.clone(),
                                                    Some(serde_json::Value::Number(n)) => {
                                                        n.to_string()
                                                    }
                                                    _ => "?".to_string(),
                                                };
                                                pane_info = Some((
                                                    pane_id,
                                                    tab_id_str.clone(),
                                                    window_id.to_string(),
                                                ));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some((pane_id, tab_id, window_id)) = pane_info {
        println!("Current Pane (PID {}):", our_pid);
        println!("  Window ID: {}", window_id);
        println!("  Tab ID:    {}", tab_id);
        println!("  Pane ID:   {}", pane_id);
    } else {
        println!("Current Pane (PID {}): not found in WT pane list", our_pid);
    }

    println!();
    println!("Summary:");
    println!(
        "  Windows: {}, Tabs: {}, Panes: {}",
        total_windows, total_tabs, total_panes
    );

    Ok(())
}

async fn run_acp_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    cli: Cli,
    shell_mgr: Arc<ShellManager>,
    wt_connected: bool,
    mut debug_rx: tokio::sync::mpsc::UnboundedReceiver<app::DebugMessage>,
    pane_identity: Option<(String, String, String)>,
    wt_event_rx: Option<tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>>,
    wt_protocol_channel: Option<Arc<CliChannel>>,
    connect_master_pipe: String,
) -> Result<()> {
    let agent_cmd = cli.agent.clone();

    let local_set = tokio::task::LocalSet::new();
    local_set
        .run_until(async move {
            let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
            let (prompt_tx, prompt_rx) = tokio::sync::mpsc::unbounded_channel();

            let evt_tx = event_tx.clone();
            tokio::task::spawn_local(event::read_crossterm_events(evt_tx));

            let dbg_event_tx = event_tx.clone();
            tokio::task::spawn_local(async move {
                while let Some(msg) = debug_rx.recv().await {
                    let _ = dbg_event_tx.send(app::AppEvent::DebugPipeMessage(msg));
                }
            });

            // Start the background protocol reader and trigger lazy event registration.
            // start_reader() claims stdout/stderr streams and must complete before any requests.
            // get_capabilities triggers _ensurePageEventsRegistered() on the WT server.
            if let Some(ref protocol_ch) = wt_protocol_channel {
                tracing::info!("start_reader: starting...");
                protocol_ch.start_reader().await;
                tracing::info!("start_reader: done, sending get_capabilities...");
                match protocol_ch.request("get_capabilities", serde_json::json!({})).await {
                    Ok(v) => tracing::info!(result = %v, "get_capabilities OK"),
                    Err(e) => tracing::warn!(error = %e, "get_capabilities FAILED"),
                }
            } else {
                tracing::warn!("no wt_pipe_channel — events won't work");
            }

            // Background WT event reader: forwards push events from the protocol channel to the TUI.
            if let Some(mut wt_rx) = wt_event_rx {
                tracing::info!("wt_event_rx: starting background reader task");
                let wt_event_tx = event_tx.clone();
                tokio::task::spawn_local(async move {
                    while let Some(event_json) = wt_rx.recv().await {
                        let method = event_json
                            .get("method")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        // The full event envelope carries `vt_sequence` (raw
                        // terminal output/scrollback) — keep it out of debug;
                        // log only the method there, full JSON at trace.
                        tracing::debug!(method = %method, "wt_event_rx: received event");
                        tracing::trace!(target: "wt_event.content", event = %event_json, "wt_event_rx: full event");

                        let params = event_json
                            .get("params")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        // Read `pane_id` (current name) with a fallback
                        // to `session_id` (the old name before the
                        // per-tab autofix routing PR renamed it). The
                        // C++ TerminalPage side now emits `pane_id` for
                        // `connection_state` / `vt_sequence`, but the
                        // wtcli `send-event` builder
                        // (`BuildSendEventJson`) was missed in that
                        // rename pass — `agent_event` envelopes from
                        // hook bridge still carried `session_id`.
                        // Without this fallback every hook event
                        // arrived with `pane_id = ""`, and downstream
                        // `route_agent_event_to_registry` collided all
                        // sessions on the empty-string key in
                        // `active_by_pane`, triggering spurious
                        // orphan-handover demotions whenever a second
                        // session started in the same window (e.g.
                        // session A → Ended the moment session B's
                        // first hook fires). Keep the fallback even
                        // after wtcli is fixed so an old wtcli build
                        // can talk to a new wta without surprises.
                        let pane_id = params
                            .get("pane_id")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .or_else(|| params.get("session_id").and_then(|v| v.as_str()))
                            .unwrap_or("")
                            .to_string();
                        let tab_id = params
                            .get("tab_id")
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                        let _ = wt_event_tx.send(app::AppEvent::WtEvent {
                            method,
                            pane_id,
                            tab_id,
                            params,
                        });
                    }
                });
            }

            let shell_mgr_for_recs = Arc::clone(&shell_mgr);

            // Cancel channel for Ctrl+C handling: App produces, ACP client
            // task consumes (one listener task inside the ACP client loop).
            let (cancel_tx, cancel_rx) = tokio::sync::mpsc::unbounded_channel();
            // /new channel: App emits a NewSessionForTab, the ACP client
            // drops the cached SessionId for that tab and re-issues
            // new_session(). The resulting SessionAttached event flows
            // back through event_tx like the lazy-create path.
            let (new_session_tx, new_session_rx) = tokio::sync::mpsc::unbounded_channel();
            // load_session channel: App emits a LoadSessionForTab in
            // response to WT's `load_session` event (the back-half of
            // the session management view's Shift+Enter -> "resume in
            // new tab's agent pane" flow). The ACP client calls
            // `conn.load_session` and binds the rehydrated session to
            // the tab via SessionAttached.
            let (load_session_tx, load_session_rx) = tokio::sync::mpsc::unbounded_channel();
            // Clone for the boot-time initial-load injection below. The
            // primary `load_session_tx` is moved into `App::new` further
            // down; this clone is used once (if `--initial-load-session-id`
            // was passed) to synthesize a LoadSessionForTab as soon as the
            // helper has finished its owner_tab_id seed. The receiver in
            // `run_acp_client_over_pipe` then drives `session/load` through
            // its standard runtime arm — no race vs. a separate VT
            // `load_session` broadcast.
            let initial_load_tx = load_session_tx.clone();
            // /restart channel: App emits a RestartRequest, the ACP client
            // kills the agent child process, drops the connection, and
            // respawns from scratch. State is cleaned up on both sides.
            let (restart_tx, restart_rx) = tokio::sync::mpsc::unbounded_channel();
            // reset_tab_session channel: App emits a DropSessionRequest when
            // WT tells us to release a tab's binding (Ctrl+C×2 hide path).
            // ACP client removes the SessionId from tab_to_session and
            // cancels any in-flight prompt for it; the next prompt on that
            // tab lazily creates a fresh session.
            let (drop_session_tx, drop_session_rx) = tokio::sync::mpsc::unbounded_channel();
            // tab-drag rename channel: App emits a RenameSessionRequest when
            // WT mints a new stable tab id for an existing tab (cross-window
            // tab drag). ACP client rekeys tab_to_session so the next prompt
            // on the dragged tab finds the existing ACP SessionId — without
            // this the agent loses turn context after a drag.
            let (rename_session_tx, rename_session_rx) =
                tokio::sync::mpsc::unbounded_channel();
            // Helper mode always speaks to wta-master, so the session-hook
            // channel is always live.
            let (session_hook_tx, session_hook_rx) = tokio::sync::mpsc::unbounded_channel();
            let (master_ext_tx, master_ext_rx) = tokio::sync::mpsc::unbounded_channel();

            // Seed the process-wide owner tab StableId so `inject_wta_pane_meta`
            // stamps `_meta.wta.owner_tab_id` on every session/new + session/load.
            // Master needs it to address `restart_agent_pane` crash-recovery
            // events by the same StableId C++ routes per-tab events with.
            protocol::acp::client::set_helper_owner_tab_id(cli.owner_tab_id.as_deref());

            // Spawn the ACP client. In helper mode (`--connect-master <pipe>`)
            // master owns the agent lifecycle, so we always spawn the
            // pipe-attached variant immediately — there's no FRE flow to defer
            // to. If the user is mid-FRE the initial handshake fails with
            // `Authentication required` and `try_start_acp` re-spawns this task
            // post-login.
            if cli.assume_master_down {
                // Degraded open: master is known down, so don't even try the
                // (dead) pipe — go straight to the disconnected view that an
                // orphaned pane shows, where /restart is the one available
                // command. /restart routes via wtcli→COM (not the dead pipe),
                // so it recovers the whole stack from right here.
                tracing::info!(
                    target: "helper",
                    "assume-master-down: starting in disconnected state (master is degraded)"
                );
                let _ = event_tx.send(app::AppEvent::AgentError {
                    session_id: None,
                    failure: protocol::acp::failure::AgentFailure::TransportLost,
                    message: t!("connection.lost").into_owned(),
                });
                // Keep the /restart path alive even with no master: /restart
                // doesn't talk to master, it asks the C++ side (via wtcli->COM)
                // to force-restart the whole agent stack — which respawns
                // master and reconnects EVERY pane. So we must keep consuming
                // `restart_rx` and forward it as a `restart_agent_stack` event.
                // The other receivers (prompt/new_session/…) genuinely have no
                // master to reach, so they're dropped; they're re-created when
                // /restart reopens this pane fresh.
                {
                    let mut restart_rx: tokio::sync::mpsc::UnboundedReceiver<
                        protocol::acp::client::RestartRequest,
                    > = restart_rx;
                    tokio::task::spawn_local(async move {
                        while let Some(req) = restart_rx.recv().await {
                            tracing::info!(
                                target: "helper",
                                new_agent = ?req.agent_cmd,
                                "restart requested while disconnected — asking WT to force-restart the agent stack"
                            );
                            let evt = serde_json::json!({
                                "type": "event",
                                "method": "restart_agent_stack",
                                "params": {},
                            });
                            crate::app::send_wt_protocol_event(evt.to_string());
                        }
                    });
                }
                // The remaining receivers have no master to forward to. They
                // get re-created when /restart respawns the stack and reopens
                // this pane fresh.
                drop((
                    prompt_rx,
                    cancel_rx,
                    new_session_rx,
                    load_session_rx,
                    drop_session_rx,
                    rename_session_rx,
                    session_hook_rx,
                    master_ext_rx,
                ));
            } else {
                let pipe_name = connect_master_pipe.clone();
                let event_tx_for_pipe = event_tx.clone();
                let shell_mgr_for_pipe = Arc::clone(&shell_mgr);
                let acp_model = cli.acp_model.clone();
                let owner_tab = cli.owner_tab_id.clone();
                let initial_load_sid = cli.initial_load_session_id.clone();
                tokio::task::spawn_local(async move {
                    if let Err(e) = protocol::acp::client::run_acp_client_over_pipe(
                        pipe_name,
                        acp_model,
                        owner_tab,
                        initial_load_sid,
                        event_tx_for_pipe.clone(),
                        prompt_rx,
                        cancel_rx,
                        new_session_rx,
                        load_session_rx,
                        drop_session_rx,
                        rename_session_rx,
                        restart_rx,
                        session_hook_rx,
                        master_ext_rx,
                        shell_mgr_for_pipe,
                        wt_connected,
                    )
                    .await
                    {
                        tracing::error!(
                            target: "helper",
                            error = %e,
                            "run_acp_client_over_pipe failed"
                        );
                        // Recover the typed classification: an auth error
                        // attached at the handshake `new_session` site survives
                        // the `?`-collapse into `anyhow` via downcast, so it
                        // still routes to the sign-in screen; other handshake
                        // failures fall back to `HandshakeFailed`. The raw
                        // `{e:#}` is also in the log above for diagnosis.
                        let failure = protocol::acp::failure::classify_anyhow(
                            &e,
                            protocol::acp::failure::HandshakeStage::Initialize,
                        );
                        let _ = event_tx_for_pipe.send(app::AppEvent::AgentError {
                            session_id: None,
                            failure,
                            message: format!("helper ACP transport failed: {e:#}"),
                        });
                    }
                });
            }

            let (recommendation_tx, recommendation_rx) = tokio::sync::mpsc::unbounded_channel();
            let (permission_tx, _permission_rx) = tokio::sync::mpsc::unbounded_channel();
            let debug_capture_enabled = Arc::new(AtomicBool::new(false));
            let (_ui_event_tx, ui_event_rx) = tokio::sync::mpsc::unbounded_channel();

            // Spawn the recommendation executor so selected choices actually run.
            let rec_event_tx = event_tx.clone();
            // Shared so a runtime `agent_config_changed` settings update can
            // hot-swap the configured delegate agent/model in place (handled
            // in App::handle_event) without restarting the agent pane. The
            // executor snapshots it per choice; the App rebuilds it on change.
            let delegate_agents = Arc::new(std::sync::Mutex::new(
                crate::coordinator::default_delegate_agent_runtimes(
                    cli.delegate_agent.as_deref(),
                    Some(cli.agent.as_str()),
                    cli.delegate_model.as_deref(),
                ),
            ));
            tokio::spawn(crate::coordinator::run_recommendation_executor(
                recommendation_rx,
                rec_event_tx,
                shell_mgr_for_recs,
                Arc::clone(&delegate_agents),
            ));

            let autofix_enabled = !cli.no_autofix;
            let mut app_state = app::App::new(prompt_tx, recommendation_tx, permission_tx, cancel_tx, new_session_tx, load_session_tx, drop_session_tx, rename_session_tx, restart_tx, master_ext_tx, debug_capture_enabled, wt_connected, autofix_enabled, Arc::clone(&shell_mgr));
            // Seed the hot-updatable runtime agent config: the shared
            // delegate runtime table, the helper's own agent_cmd (needed to
            // re-derive the delegate commandline when only the delegate
            // agent/model change), and the configured acp-model override
            // (re-applied to future sessions so /new stays on the model).
            app_state.set_runtime_agent_config(
                Arc::clone(&delegate_agents),
                cli.agent.clone(),
                cli.acp_model.clone(),
            );
            app_state.set_session_hook_tx(session_hook_tx);

            // Pipe-mode reconnect pre-stash. In helper mode the initial
            // `run_acp_client_over_pipe` task fails immediately with
            // `Authentication required` if the user is in FRE (not yet
            // logged in). The post-login `LoginComplete` handler fires
            // `try_start_acp`; without this stash it would have no master
            // pipe to reconnect with and could not resume the agent pane
            // — breaking every `intellterm.wta/...`
            // ext-method (e.g. `sessions/list` — session view would stay
            // empty on the first tab forever). With the stash in place,
            // `try_start_acp` sees `master_pipe_name = Some(...)` and
            // routes the reconnect back through master.
            //
            // No effect when the initial connection succeeds: the
            // stashed params just sit unused for the helper's lifetime.
            app_state.set_master_pipe_acp_params(
                connect_master_pipe.clone(),
                agent_cmd.clone(),
                cli.acp_model.clone(),
                cli.owner_tab_id.clone(),
                Arc::clone(&shell_mgr),
                wt_connected,
            );

            // ── Preflight: check the agent CLI before connecting ──────────
            // Skip preflight when FRE is active — FRE has its own agent
            // selection + auth flow and doesn't need the preflight wizard.
            if cli.setup.is_none() {
                // Prefer the canonical id the host passed via `--agent-id`
                // — that's the user's actual setting value (`acpAgent`).
                // Fall back to reverse-parsing the `--agent` command line
                // for manual runs / older hosts.
                let canonical_id: String = cli
                    .agent_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_ascii_lowercase)
                    .unwrap_or_else(|| {
                        agent_registry::resolve_agent_id_from_cmd(&agent_cmd).to_string()
                    });
                app_state.current_agent_id = canonical_id.clone();
                tracing::info!(
                    target: "agents_view_filter",
                    agent_id = %canonical_id,
                    agent_cmd = %agent_cmd,
                    source = if cli.agent_id.is_some() { "--agent-id" } else { "resolved-from-cmd" },
                    "current_agent_id assigned",
                );
                let agent_id = canonical_id.as_str();
                let preflight_result = if agent_id.starts_with("custom:")
                    || agent_registry::lookup_profile_by_id(agent_id).id == "unknown"
                {
                    // Custom/unknown agents: command is opaque (`.cmd`, `node script.js`,
                    // shell function, …); a PATH probe would lie. The real spawn produces
                    // the authoritative error via `ConnectionFailed`, so skip preflight.
                    app::PreflightResult::passed_for_custom_agent(&canonical_id)
                } else {
                    let status = agent_check::check_agent(agent_id);
                    app::PreflightResult {
                        agent_id: canonical_id.clone(),
                        display_name: status.display_name.clone(),
                        cli_status: if status.cli_found {
                            app::CheckStatus::Passed
                        } else {
                            app::CheckStatus::Failed("Not found on PATH".to_string())
                        },
                        cli_path: status.cli_path.clone(),
                        auth_status: if !status.cli_found {
                            app::CheckStatus::Skipped
                        } else if status.has_credential {
                            app::CheckStatus::Passed
                        } else {
                            app::CheckStatus::Skipped
                        },
                        install_hint: status.install_hint.clone(),
                        install_url: String::new(),
                        auth_hint: status.auth_hint.clone(),
                    }
                };
                tracing::info!(
                    target: "preflight",
                    agent_id = %preflight_result.agent_id,
                    cli = ?preflight_result.cli_status,
                    auth = ?preflight_result.auth_status,
                    "preflight done (via agent_check)"
                );
                let _ = event_tx.send(app::AppEvent::PreflightComplete(preflight_result));
            }

            // ── install-hooks request channel ─────────────────────────────
            // The Settings UI / in-TUI install button signals via this
            // channel; main.rs runs `agent_hooks_installer::ensure_installed`
            // off the UI thread so the TUI stays responsive.
            let (install_req_tx, mut install_req_rx) =
                tokio::sync::mpsc::unbounded_channel::<()>();
            tokio::task::spawn_local(async move {
                while let Some(()) = install_req_rx.recv().await {
                    tracing::info!(target: "install_hooks", "received install request");
                    // Run the (potentially slow, IO-bound) installer on the
                    // blocking pool so we don't park the LocalSet.
                    let _ = tokio::task::spawn_blocking(|| {
                        agent_hooks_installer::ensure_installed();
                    })
                    .await;
                }
            });
            app_state.set_install_request_tx(install_req_tx);

            // Wire the agent_event channel so dispatch_resume's split-pane
            // background callback can post AgentSessionEvent (specifically
            // ResumePaneAssigned) back into the event loop.
            app_state.set_agent_event_tx(event_tx.clone());

            // Seed `app_state.tab_id` + `pane_open` from `--owner-tab-id`
            // BEFORE the `--initial-view` block + the `project_active_tab_state`
            // emit below. Two failure modes if we don't:
            //   1. `current_tab_mut` in the --initial-view block falls back
            //      to DEFAULT_TAB_ID — the view setting lands on the wrong
            //      tab, the echo C++ receives doesn't match any real tab
            //      and is dropped.
            //   2. The initial echo has `pane_open=false` (default), which
            //      C++'s `OnAgentStateChanged` interprets as "hide" and
            //      stashes the just-spawned agent pane.
            // The full seed block further down (which logs + redundantly
            // sets the same fields) becomes idempotent now.
            //
            // `--start-stashed` inverts (2): in the pre-warm path the
            // C++ side has *already stashed* the pane after spawning the
            // helper, so the helper must seed `pane_open = false` to
            // match. Without this, helper echoes `pane_open=true`, C++
            // sees a stashed pane and a `pane_open=true` echo, and
            // restores the pane — defeating pre-warm.
            if let Some(ref owner_tab_id) = cli.owner_tab_id {
                if !owner_tab_id.is_empty() && app_state.tab_id.is_none() {
                    let tab = app_state
                        .tab_sessions
                        .entry(owner_tab_id.clone())
                        .or_default();
                    tab.pane_open = !cli.start_stashed;
                    app_state.tab_id = Some(owner_tab_id.clone());
                    app_state.owner_tab_id = Some(owner_tab_id.clone());
                }
            }

            // Plan-C boot-time initial-load: if WT spawned us with
            // `--initial-load-session-id` (+ optional `--initial-load-cwd`)
            // synthesize an `AppEvent::WtEvent { method:"load_session" }`
            // and queue it on `event_tx`. The App's event loop will pick
            // it up after startup and route it through the same handler
            // that the runtime `wt_event` path uses (app.rs ~4039) —
            // which:
            //   1) clears the tab's chat and sets `loading_session=true`,
            //      so the chunk handlers ACCEPT replay chunks during the
            //      ensuing `session/load`. Going through the channel
            //      directly (the old design) skipped this, and the
            //      master DID route the replay chunks back to the
            //      helper, but the App's AgentMessageChunk handler
            //      dropped them because `turn.is_in_flight() == false`
            //      and `loading_session == false` — user-visible
            //      symptom: "Session loaded." footer with no past
            //      content above.
            //   2) emits a "Resuming session …" system message so the
            //      user has a visible cue while the load is in flight,
            //   3) forwards into the same `load_session_tx` channel the
            //      runtime arm uses, which drives `conn.load_session`
            //      on the ACP client side — atomically replacing the
            //      bootstrap session created by `session/new` moments
            //      earlier.
            //
            // This replaces the prior race-prone design where C++
            // broadcast a separate `load_session` VT event right after
            // spawning the helper — which often landed in the wrong
            // helper because the new helper's pipe attach hadn't yet
            // completed.
            //
            // Pair-only: both flags meaningless without `--owner-tab-id`
            // (the load_session handler routes by tab id), so we
            // silently skip if owner_tab_id is unset. Logged so a
            // misconfigured spawn is easy to diagnose.
            if let Some(ref sid) = cli.initial_load_session_id {
                if !sid.is_empty() {
                    let tab_id_opt = app_state
                        .owner_tab_id
                        .clone()
                        .or_else(|| cli.owner_tab_id.clone());
                    match tab_id_opt {
                        Some(tab_id) if !tab_id.is_empty() => {
                            let cwd = cli
                                .initial_load_cwd
                                .as_deref()
                                .map(str::to_string)
                                .filter(|s| !s.is_empty())
                                .and_then(|s| {
                                    let v = crate::cwd_util::validate_starting_directory(&s);
                                    if v.is_none() {
                                        tracing::warn!(
                                            target: "acp_load_session",
                                            "--initial-load-cwd refers to a missing directory; dropping from load_session params",
                                        );
                                    }
                                    v
                                });
                            tracing::info!(
                                target: "acp_load_session",
                                session_id = sid,
                                tab_id = %tab_id,
                                "queueing boot-time initial load_session via AppEvent::WtEvent"
                            );
                            let mut params = serde_json::Map::new();
                            params.insert(
                                "tab_id".to_string(),
                                serde_json::Value::String(tab_id.clone()),
                            );
                            params.insert(
                                "session_id".to_string(),
                                serde_json::Value::String(sid.clone()),
                            );
                            if let Some(cwd_str) = cwd {
                                params.insert(
                                    "cwd".to_string(),
                                    serde_json::Value::String(cwd_str),
                                );
                            }
                            let _ = event_tx.send(app::AppEvent::WtEvent {
                                method: "load_session".to_string(),
                                pane_id: String::new(),
                                tab_id: Some(tab_id),
                                params: serde_json::Value::Object(params),
                            });
                        }
                        _ => {
                            tracing::warn!(
                                target: "acp_load_session",
                                "--initial-load-session-id given without --owner-tab-id; ignoring"
                            );
                        }
                    }
                }
            }
            // `initial_load_tx` is no longer used (the runtime
            // `load_session_tx` path is now reached via the App's
            // WtEvent handler) but we still need to drop the cloned
            // sender so the receiver future inside the ACP client loop
            // doesn't keep an extra producer alive past shutdown.
            drop(initial_load_tx);

            // Apply --initial-view: if `sessions`, jump straight into the
            // agent session view (mirrors the Chat→Agents toggle). Wired to
            // WT's Ctrl+Shift+/ binding via `--initial-view sessions` on
            // the wta cmdline. `open_agents_view_for_tab` fires the
            // `session/list` refetch to master that populates the view.
            //
            // Skip in setup mode: --setup takes the FRE path and the user
            // shouldn't be dropped into an empty session list.
            if cli.setup.is_none() && cli.initial_view == InitialView::Sessions {
                tracing::info!(target: "initial_view", "starting in agent session view");
                let tab_id = app_state
                    .tab_id
                    .clone()
                    .unwrap_or_else(|| app::DEFAULT_TAB_ID.to_string());
                app_state.open_agents_view_for_tab(tab_id);
            }

            // Project the initial active-tab state to C++ once, after the
            // --initial-view block has had its say. Without this push,
            // C++'s `_agentSessionsViewActive` and `Tab.AgentPaneOpen`
            // mirrors (single writer lives in `OnAgentStateChanged`)
            // would stay on their defaults until the user's first
            // interaction, leaving the bar mislabelled in the
            // `--initial-view sessions` case and the pane-open flag
            // out of sync with the seeded `pane_open=true` on the
            // owner tab. Cheap and idempotent.
            //
            // Safe before the `Setup` mode block below: that block runs
            // its own UI and doesn't read the view flag; if we end up in
            // setup mode the initial "chat" emission is harmless.
            if wt_connected {
                app_state.project_active_tab_state();
            }

            // NOTE: the helper no longer scans on-disk history at all. The
            // session view renders from master's `session/list` snapshot, and
            // master performs the single CLI-filtered scan at its startup.
            // See doc/specs/per-cli-history-filtering.md.

            // Enter setup mode if --setup <reason> was passed.
            tracing::info!("cli.setup = {:?}", cli.setup);
            if let Some(ref reason_str) = cli.setup {
                tracing::info!("Entering FRE setup mode: reason={}", reason_str);
                let reason = app::SetupReason::from_str(reason_str);

                app_state.mode = app::AppMode::Setup;
                let all_agent_statuses = agent_check::check_all_agents();
                let options = app::build_setup_options(&reason, None, &all_agent_statuses);
                let title = reason.title().to_string();
                let subtitle = match reason {
                    app::SetupReason::FirstRun => "Getting started".to_string(),
                    _ => "Fix the issue to continue".to_string(),
                };
                app_state.setup = Some(app::SetupState {
                    reason,
                    selected_index: 0,
                    preflight: app::PreflightResult {
                        agent_id: String::new(),
                        display_name: String::new(),
                        cli_status: app::CheckStatus::Skipped,
                        cli_path: None,
                        auth_status: app::CheckStatus::Skipped,
                        install_hint: String::new(),
                        install_url: String::new(),
                        auth_hint: String::new(),
                    },
                    install_in_progress: false,
                    install_log: Vec::new(),
                    install_error: None,
                    options,
                    title,
                    subtitle,
                });
            }

            app_state.set_event_tx(event_tx.clone());

            // The helper does not scan on-disk history: master performs the
            // single (CLI-filtered) scan and the session view renders from
            // its `session/list` snapshot. See
            // doc/specs/per-cli-history-filtering.md.

            if let Some((pane_id, _tab_id, window_id)) = pane_identity {
                app_state.pane_id = Some(pane_id);
                // discover_pane_identity returns the legacy unstable tab
                // index, not the GUID — ignore it. The stable owner-tab GUID
                // is passed by WT via --owner-tab-id (see below) and seeded
                // directly into app_state.tab_id.
                app_state.window_id = Some(window_id);
            }

            // Seed tab_id from --owner-tab-id (passed by TerminalPage when
            // spawning the agent pane). With this set, AgentConnected binds
            // the initial session under the correct GUID immediately, and
            // tab_changed events later are plain switches — no implicit
            // DEFAULT_TAB_ID placeholder, no migration heuristics. Falls
            // back to None for non-pane invocations (manual `wta` runs, the
            // `wta delegate` subcommand), where the legacy DEFAULT_TAB_ID
            // path handles routing.
            //
            // Materialize the matching `tab_sessions` entry alongside the
            // tab_id assignment — `current_tab()` borrows immutably and
            // expects the active key to already be present, so without
            // pre-inserting we'd panic on the first render before any
            // event has had a chance to lazy-create it.
            if let Some(owner_tab_id) = cli.owner_tab_id.clone() {
                if !owner_tab_id.is_empty() {
                    tracing::info!(
                        target: "tab_session",
                        tab_id = %owner_tab_id,
                        "seeded app_state.tab_id from --owner-tab-id"
                    );
                    let tab = app_state
                        .tab_sessions
                        .entry(owner_tab_id.clone())
                        .or_default();
                    // wta is the source of truth for "does this tab want
                    // the pane visible". The pane is being spawned right
                    // now for this owner tab; under the normal user-
                    // initiated open the user wants it visible, so default
                    // pane_open=true. The exception is `--start-stashed`
                    // (pre-warm path) where C++ has already stashed the
                    // pane — see comment on the earlier seed block.
                    tab.pane_open = !cli.start_stashed;
                    app_state.tab_id = Some(owner_tab_id.clone());

                    // Publish an initial chip-target state for this tab so
                    // the C++ side can sync regardless of which transitions
                    // it has seen so far. At startup no Send card is
                    // selected, so the published target is `None` — i.e.
                    // "release any override, fall back to the source-of-
                    // agent flag". This is harmless when the C++ side is
                    // already in that state and load-bearing in the race
                    // where the agent pane was just restored from a stash
                    // and the chip-visibility hook on the C++ side hasn't
                    // run with the right `previousActive` yet.
                    app_state.recompute_chip_override_initial(&owner_tab_id);
                }
            }

            // ── source-pane context (autofix attribution) ─────────────────
            app_state.source_session_id = std::env::var("WTA_SOURCE_SESSION_ID")
                .ok()
                .filter(|s| !s.is_empty());
            app_state.source_cwd = std::env::var("WTA_SOURCE_CWD")
                .ok()
                .filter(|s| !s.is_empty());

            // ── env-gated raw agent_event chat logging (diagnostics) ──────
            app_state.log_agent_events = std::env::var("WTA_LOG_AGENT_EVENT")
                .map(|v| matches!(v.as_str(), "1" | "true" | "yes"))
                .unwrap_or(false);

            // If a prompt was passed via CLI arg (e.g., from command palette creating
            // a new agent pane), delegate it to a new tab agent on startup.
            if let Some(ref initial_prompt) = cli.prompt {
                if !initial_prompt.is_empty() {
                    app_state.delegate_to_tab_agent(initial_prompt);
                }
            }

            app_state.run(terminal, event_rx, ui_event_rx).await
        })
        .await
}

#[cfg(test)]
mod cli_tests;
