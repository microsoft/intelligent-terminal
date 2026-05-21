#[macro_use]
extern crate rust_i18n;

mod agent_check;
mod agent_registry;
mod agent_sessions;
mod agent_hooks_installer;
mod app;
mod commands;
mod coordinator;
mod event;
mod history_loader;
mod logging;
mod osc52;
mod protocol;
mod runtime_paths;
mod pane_context;
mod shell;
mod theme;
mod ui;
mod ui_trace;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use crossterm::{
    cursor::SetCursorStyle,
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    style::Print,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::prelude::*;
use serde_json::json;
use std::io;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

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
        "zh-hk" | "zh-mo" | "zh-hant" => Some("zh-TW"),
        "zh-sg" | "zh-hans" => Some("zh-CN"),
        // English: Commonwealth regions → en-GB
        "en-au" | "en-nz" | "en-ie" | "en-in" | "en-sg" | "en-za" | "en-hk"
        | "en-my" | "en-ph" | "en-pk" | "en-ng" | "en-ke" | "en-gh" => Some("en-GB"),
        // Spanish: Latin American regions → es-MX
        "es-ar" | "es-co" | "es-cl" | "es-pe" | "es-ve" | "es-ec" | "es-gt"
        | "es-cu" | "es-bo" | "es-do" | "es-hn" | "es-py" | "es-sv" | "es-ni"
        | "es-cr" | "es-pa" | "es-uy" | "es-pr" | "es-us" | "es-419" => Some("es-MX"),
        // French: non-Canadian → fr-FR
        "fr-be" | "fr-ch" | "fr-lu" | "fr-mc" | "fr-sn" | "fr-ci" | "fr-ml"
        | "fr-cm" | "fr-mg" | "fr-cd" | "fr-dz" | "fr-tn" | "fr-ma" => Some("fr-FR"),
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
        if let Some(found) = available.iter().find(|l| l.to_lowercase().starts_with(&prefix)) {
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
    /// equivalent to the user pressing F2 right after the pane opens.
    /// Wired to WT's Ctrl+Shift+/ binding via TerminalPage.
    #[arg(long, value_enum, default_value_t = InitialView::Chat)]
    initial_view: InitialView,

    /// Stable GUID of the WT tab that owns this wta process. Passed in by
    /// TerminalPage when spawning the agent pane (both _OpenOrReuseAgentPane
    /// and _AutoCreateHiddenAgentPane). Seeded into app_state.tab_id before
    /// ACP init, so the first AgentConnected binds the session under the
    /// real tab GUID instead of falling back to the implicit DEFAULT_TAB_ID
    /// placeholder. Hidden because nothing outside WT should be setting it.
    #[arg(long, hide = true)]
    owner_tab_id: Option<String>,

    // Legacy flags (hidden, backward compat)
    #[arg(long, hide = true)]
    info: bool,
    #[arg(long, hide = true)]
    test_pipe: bool,

    /// Output raw JSON instead of human-readable format
    #[arg(long, global = true)]
    json: bool,
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

    /// Delegate a prompt to a new tab with a configured agent (fire-and-forget)
    Delegate {
        /// The prompt to send to the delegate agent
        #[arg(value_name = "PROMPT")]
        prompt: String,

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
        /// "copilot --acp --stdio" or "npx -y @zed-industries/claude-code-acp").
        #[arg(long)]
        agent: String,
    },
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
}

impl HooksCliFilter {
    fn into_scope(self) -> agent_hooks_installer::CliScope {
        use agent_hooks_installer::{CliKind, CliScope};
        match self {
            HooksCliFilter::All => CliScope::All,
            HooksCliFilter::Copilot => CliScope::One(CliKind::Copilot),
            HooksCliFilter::Claude => CliScope::One(CliKind::Claude),
            HooksCliFilter::Gemini => CliScope::One(CliKind::Gemini),
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
    if let Some(locale) = sys_locale::get_locale() {
        rust_i18n::set_locale(&normalize_locale(&locale));
    }

    let cli = Cli::parse();

    // Legacy flags first (backward compat)
    if cli.test_pipe {
        return run_test_pipe().await;
    }
    if cli.info {
        return run_info_mode().await;
    }
    let json_mode = cli.json;

    match cli.command {
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
        Some(Command::ListPanes {
            tab_id,
            window_id,
        }) => {
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
        Some(Command::CapturePane { target, max_lines, last_prompt }) => {
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
                println!("Pane {} closed.", pane_id);
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
                .context("Failed to spawn wtcli wait-for")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!("wtcli wait-for failed: {}", stderr.trim());
            }

            let stdout = String::from_utf8_lossy(&output.stdout);
            let trimmed = stdout.trim();
            if !trimmed.is_empty() {
                let val: serde_json::Value = serde_json::from_str(trimmed)
                    .context("Failed to parse wtcli wait-for output")?;
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
            run_delegate(&prompt, &agent, delegate_agent.as_deref(), delegate_model.as_deref(), cwd.as_deref()).await
        }

        // ── Listen for events ──
        Some(Command::Listen { target }) => run_listen(target.as_deref()).await,

        // ── Manage agent hooks (install/status/uninstall) ──
        Some(Command::Hooks { action }) => match action {
            HooksAction::Install { cli } => run_hooks_install(cli),
            HooksAction::Status => run_hooks_status(json_mode),
            HooksAction::Uninstall { cli } => run_hooks_uninstall(cli, json_mode),
        },

        // ── ACP model list probe ──
        Some(Command::ProbeModels { agent }) => run_probe_models(&agent).await,

        // ── No subcommand = ACP TUI mode (default) ──
        None => run_default_tui(cli).await,
    }
}

/// Drive [`protocol::acp::probe::probe_models`] on a tokio `LocalSet`
/// (the ACP client connection is `!Send`), serialize the result to
/// stdout, force-exit. See exit notes below.
async fn run_probe_models(agent: &str) -> Result<()> {
    // Logging must go to file, not stderr — the Settings UI captures
    // our stdout for the JSON payload, and stderr would be folded
    // into the same pipe and pollute the parser.
    let _guard = logging::init("probe");
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
            // See exit rationale below.
            std::process::exit(1);
        }
    };
    tracing::info!(
        "probe-models ok: {} model(s), current={:?}",
        result.available_models.len(),
        result.current_model_id
    );
    let payload = serde_json::to_string(&result)
        .context("serialize probe result")?;
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
    std::process::exit(0);
}

// ─── Hooks subcommand handlers ──────────────────────────────────────────────

fn run_hooks_install(cli: HooksCliFilter) -> Result<()> {
    // Initialize logging so the install attempt is observable in
    // %LOCALAPPDATA%\IntelligentTerminal\logs\wta-install-hooks.log.
    let _guard = logging::init("install-hooks");
    agent_hooks_installer::ensure_installed_scoped(cli.into_scope());
    println!(
        "wt-agent-hooks install attempted (idempotent). \
         Run `wta hooks status` to inspect the result. \
         Trace log: %LOCALAPPDATA%\\IntelligentTerminal\\logs\\wta-install-hooks.log"
    );
    Ok(())
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
    println!(
        "bundle source: {}{}",
        r.bundle_source.kind,
        r.bundle_source
            .path
            .as_deref()
            .map(|p| format!(" ({})", p))
            .unwrap_or_default(),
    );
    println!();
    for c in &r.clis {
        let summary = if !c.binary_on_path {
            "\u{2717} CLI not on PATH".to_string()
        } else if c.plugin_installed && c.plugin_enabled && c.marketplace_path_valid {
            "\u{2713} installed".to_string()
        } else if c.plugin_installed && !c.marketplace_path_valid {
            "\u{26a0} marketplace path stale".to_string()
        } else if c.plugin_installed {
            "\u{26a0} installed but disabled".to_string()
        } else {
            "\u{2717} not installed".to_string()
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
            "skipped (CLI not on PATH)".to_string()
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
                if c.staging_dir_removed { "ok" } else { "failed" },
            )
        };
        println!("  {:<10} {}", c.name, summary);
        for m in &c.messages {
            println!("    \u{00b7} {}", m);
        }
    }
}

fn yn(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
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
                .ok_or_else(|| anyhow::anyhow!("No active pane found. Use -t to specify a pane ID."))?;
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
        .ok_or_else(|| anyhow::anyhow!("No windows found"))
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
        .ok_or_else(|| anyhow::anyhow!("No tabs found in window {}", window_id))
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
            println!("No windows found.");
            return;
        }
        println!("{:<12} {:<30} {}", "WINDOW_ID", "TITLE", "FOCUSED");
        for w in windows {
            let id = json_str_or_num(w, "window_id");
            let title = w
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
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
            println!("No tabs found.");
            return;
        }
        println!("{:<10} {:<30} {}", "TAB_ID", "TITLE", "FOCUSED");
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
            println!("No panes found.");
            return;
        }
        println!(
            "{:<10} {:<8} {:<8} {:<10} {}",
            "PANE_ID", "PID", "ACTIVE", "ROWS", "COLS"
        );
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
    println!("Active pane: {} (tab: {}, window: {})", id, tab, win);
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
        println!("Running (PID: {})", pid);
    } else {
        println!("Exited (code: {}, PID: {})", exit_code, pid);
    }
}

fn format_created_tab(val: &serde_json::Value) {
    let tab_id = json_str_or_num(val, "tab_id");
    let pane_id = json_str_or_num(val, "session_id");
    println!("Created tab {} (pane {})", tab_id, pane_id);
}

fn format_created_pane(val: &serde_json::Value) {
    let pane_id = json_str_or_num(val, "session_id");
    println!("Created pane {}", pane_id);
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
        .map_err(|_| anyhow::anyhow!("WT_COM_CLSID not set. Run inside a Windows Terminal pane."))?;
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
        .map_err(|_| anyhow::anyhow!("WT_COM_CLSID not set. Run inside a Windows Terminal pane."))?;

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
            bail!("Unknown shell type '{}'. Use: bash, powershell, cmd, fish", other);
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
    prompt: &str,
    agent_cmd: &str,
    delegate_agent_cmd: Option<&str>,
    delegate_model: Option<&str>,
    cwd: Option<&str>,
) -> Result<()> {
    let _guard = logging::init("delegate");
    tracing::info!(prompt, agent = agent_cmd, cwd, "run_delegate started");

    let (debug_tx, _) = tokio::sync::mpsc::unbounded_channel::<app::DebugMessage>();
    let channel = match connect_to_wt_protocol(debug_tx).await {
        Ok(ch) => { tracing::info!("WT protocol connected"); ch }
        Err(e) => { tracing::warn!(error = %e, "WT protocol connection FAILED"); return Err(e); }
    };
    let shell_mgr = ShellManager::new()
        .with_wt_channel(Arc::new(channel) as Arc<dyn shell::wt_channel::WtChannel>);

    match delegate_with_context(&shell_mgr, prompt, agent_cmd, delegate_agent_cmd, delegate_model, cwd).await {
        Ok(()) => { tracing::info!("delegate OK"); Ok(()) }
        Err(e) => { tracing::warn!(error = %e, "delegate FAILED"); Err(e) }
    }
}

/// Shared delegation logic: enrich the prompt with the active pane's recent
/// output (when available), build the delegate-agent commandline, and create a
/// new tab to launch it. WT's GetActivePane already resolves the agent pane to
/// the user's working pane, so a single query is enough.
async fn delegate_with_context(
    shell_mgr: &ShellManager,
    prompt: &str,
    agent_cmd: &str,
    delegate_agent_cmd: Option<&str>,
    delegate_model: Option<&str>,
    cwd: Option<&str>,
) -> Result<()> {
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

    let delegate_agents = crate::coordinator::default_delegate_agent_runtimes(
        delegate_agent_cmd,
        Some(agent_cmd),
        delegate_model,
    );
    let runtime = delegate_agents
        .first()
        .ok_or_else(|| anyhow::anyhow!("no delegate agent configured"))?;

    let commandline = crate::coordinator::build_delegate_commandline(runtime, &full_prompt)?;

    tracing::debug!(commandline, cwd, "delegate_with_context: launching");

    shell_mgr
        .wt_create_tab(Some(&commandline), cwd, None)
        .await?;

    Ok(())
}

// ─── Default ACP TUI mode ───────────────────────────────────────────────────

async fn run_default_tui(cli: Cli) -> Result<()> {
    let _guard = logging::init("main");
    tracing::info!("=== run_default_tui started ===");

    // Debug channel for TUI debug panel (WT protocol traffic viewer)
    let (debug_tx, debug_rx) = tokio::sync::mpsc::unbounded_channel::<app::DebugMessage>();

    // Try to connect to the Windows Terminal protocol.
    let mut shell_mgr = ShellManager::new();
    let mut wt_event_rx = None;
    let mut wt_protocol_channel: Option<Arc<CliChannel>> = None;
    let wt_connected = match connect_to_wt_protocol(debug_tx.clone()).await {
        Ok(channel) => {
            tracing::info!("Connected to WT COM protocol — subscribing to events");
            // Subscribe to push events before wrapping in Arc.
            wt_event_rx = Some(channel.subscribe_events());
            let cli_arc = Arc::new(channel);
            wt_protocol_channel = Some(Arc::clone(&cli_arc));

            shell_mgr = shell_mgr.with_wt_channel(cli_arc.clone() as Arc<dyn shell::wt_channel::WtChannel>);
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, "NO WT protocol connection");
            false
        }
    };
    let shell_mgr = Arc::new(shell_mgr);

    // Try to discover our own pane identity by PID matching
    let pane_identity = if wt_connected {
        discover_pane_identity(&shell_mgr).await
    } else {
        None
    };

    run_acp_tui_mode(cli, shell_mgr, wt_connected, debug_rx, pane_identity, wt_event_rx, wt_protocol_channel).await
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
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    execute!(stdout, Print("\x1b]11;#0c0c0c\x07"))?;
    // Steady block (DECSCUSR Ps=2): solid filled rectangle, no blink.
    // Survives the alt-screen swap; restored on exit below.
    execute!(stdout, SetCursorStyle::SteadyBlock)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result =
        run_acp_app(&mut terminal, cli, shell_mgr, wt_connected, debug_rx, pane_identity, wt_event_rx, wt_protocol_channel).await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        SetCursorStyle::DefaultUserShape,
        // OSC 111: reset bg to terminal default so the host shell isn't
        // left with our override.
        Print("\x1b]111\x07"),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    if let Err(e) = result {
        eprintln!("Error: {e:?}");
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
                                .request(
                                    "list_panes",
                                    serde_json::json!({ "tab_id": tab_id_str }),
                                )
                                .await
                            {
                                if let Some(panes_arr) =
                                    panes.get("panes").and_then(|v| v.as_array())
                                {
                                    total_panes += panes_arr.len() as u32;

                                    for pane in panes_arr {
                                        if let Some(pid) =
                                            pane.get("pid").and_then(|v| v.as_u64())
                                        {
                                            if pid == our_pid as u64 {
                                                let pane_id = match pane.get("session_id") {
                                                    Some(serde_json::Value::String(s)) => {
                                                        s.clone()
                                                    }
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
                        tracing::debug!(event = %event_json, "wt_event_rx: received event");
                        let method = event_json
                            .get("method")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let pane_id = event_json
                            .get("params")
                            .and_then(|p| p.get("session_id"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let params = event_json
                            .get("params")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        let _ = wt_event_tx.send(app::AppEvent::WtEvent {
                            method,
                            pane_id,
                            params,
                        });
                    }
                });
            }

            let shell_mgr_for_recs = Arc::clone(&shell_mgr);

            // Cancel channel for Ctrl+C handling: App produces, ACP client
            // task consumes (one listener task inside run_acp_client).
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

            // Spawn the ACP client -- but not in setup mode, where the user
            // hasn't chosen an agent yet. Store params for deferred start.
            let deferred_channels = if cli.setup.is_none() {
                tokio::task::spawn_local(protocol::acp::client::run_acp_client(
                    agent_cmd.clone(),
                    cli.acp_model.clone(),
                    cli.owner_tab_id.clone(),
                    event_tx.clone(),
                    prompt_rx,
                    cancel_rx,
                    new_session_rx,
                    load_session_rx,
                    drop_session_rx,
                    restart_rx,
                    Arc::clone(&shell_mgr),
                    wt_connected,
                ));
                None
            } else {
                Some((prompt_rx, cancel_rx, new_session_rx, load_session_rx, drop_session_rx, restart_rx))
            };

            let (recommendation_tx, recommendation_rx) = tokio::sync::mpsc::unbounded_channel();
            let (permission_tx, _permission_rx) = tokio::sync::mpsc::unbounded_channel();
            let debug_capture_enabled = Arc::new(AtomicBool::new(false));
            let (_ui_event_tx, ui_event_rx) = tokio::sync::mpsc::unbounded_channel();

            // Spawn the recommendation executor so selected choices actually run.
            let rec_event_tx = event_tx.clone();
            let delegate_agents = crate::coordinator::default_delegate_agent_runtimes(
                cli.delegate_agent.as_deref(),
                Some(cli.agent.as_str()),
                cli.delegate_model.as_deref(),
            );
            tokio::spawn(crate::coordinator::run_recommendation_executor(
                recommendation_rx,
                rec_event_tx,
                shell_mgr_for_recs,
                delegate_agents,
            ));

            let autofix_enabled = !cli.no_autofix;
            let mut app_state = app::App::new(prompt_tx, recommendation_tx, permission_tx, cancel_tx, new_session_tx, load_session_tx, drop_session_tx, restart_tx, debug_capture_enabled, wt_connected, autofix_enabled);

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
                let status = agent_check::check_agent(agent_id);
                let preflight_result = app::PreflightResult {
                    agent_id: status.id.clone(),
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

            // Apply --initial-view: if `sessions`, jump straight into the
            // Agents view (mirrors the F2 Chat→Agents toggle). Wired to
            // WT's Ctrl+Shift+/ binding via `--initial-view sessions` on
            // the wta cmdline. Must run after set_agent_event_tx so that
            // ensure_history_loaded()'s event_tx clone is populated —
            // otherwise the lazy scan would early-return and the Agents
            // list would never populate.
            //
            // Skip in setup mode: --setup takes the FRE path and the user
            // shouldn't be dropped into an empty session list.
            if cli.setup.is_none() && cli.initial_view == InitialView::Sessions {
                tracing::info!(target: "initial_view", "starting in Agents view");
                app_state.current_tab_mut().current_view = app::View::Agents;
                // Seed selection so Enter activates the first row immediately
                // (mirrors the F2 enter-Agents path). Honor the CLI filter so
                // we don't seed Some(0) when there are no rows for the
                // current agent CLI.
                let has_sessions = !app_state
                    .agent_sessions
                    .iter_sorted_filtered(app_state.current_cli_filter().as_ref())
                    .is_empty();
                if has_sessions {
                    app_state.current_tab_mut().agents_list_state.select(Some(0));
                }
                app_state.ensure_history_loaded();
            }

            // NOTE: historical agent sessions used to be loaded here via
            // `history_loader::load_all()` (later as a `spawn_blocking`).
            // That work is now deferred — the registry is scanned lazily
            // on the first F2 press via `App::ensure_history_loaded()`.
            //
            // Why: load_all() is hundreds of file opens (one per Copilot
            // session-state dir, reading events.jsonl for the autofix
            // fingerprint). On a populated machine it's ~10s of disk I/O.
            // Every wta spawn — including every model switch in the agent
            // pane — paid that cost, even though the data is only ever
            // consumed by the Agents view. Lazy-loading on F2 keeps the
            // model-switch path free of this overhead entirely.

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

            // Kick the historical-session scan immediately on agent-pane
            // startup so the F2 sessions view is populated by the time the
            // user opens it. The scan runs on a `spawn_blocking` thread and
            // posts `HistoricalSessionsLoaded` back, so it never blocks the
            // LocalSet or the first frame. Subsequent `ensure_history_loaded`
            // calls (from F2 / `/sessions`) short-circuit on `Loading`/`Loaded`.
            //
            // Only the ACP TUI path reaches here — `wta delegate`, `wta mcp`,
            // and CLI subcommands never construct an App that wires
            // `event_tx`, so they don't pay this cost.
            app_state.ensure_history_loaded();

            // If in setup mode, store ACP params for deferred start after login.
            if let Some((prompt_rx, cancel_rx, new_session_rx, load_session_rx, drop_session_rx, restart_rx)) = deferred_channels {
                app_state.set_acp_params(
                    agent_cmd.clone(),
                    cli.acp_model.clone(),
                    prompt_rx,
                    cancel_rx,
                    new_session_rx,
                    load_session_rx,
                    drop_session_rx,
                    restart_rx,
                    Arc::clone(&shell_mgr),
                    wt_connected,
                );
            }

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
                    app_state
                        .tab_sessions
                        .entry(owner_tab_id.clone())
                        .or_default();
                    app_state.tab_id = Some(owner_tab_id);
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
