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
mod preflight;
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

    // Legacy flags (hidden, backward compat)
    #[arg(long, hide = true)]
    info: bool,
    #[arg(long, hide = true)]
    test_pipe: bool,

    /// Output raw JSON instead of human-readable format
    #[arg(long, global = true)]
    json: bool,

    /// Windows Terminal pipe name (overrides VT discovery and WT_PIPE_NAME env var)
    #[arg(long, global = true)]
    pipe_name: Option<String>,

    /// Windows Terminal auth token (overrides WT_MCP_TOKEN env var, use with --pipe-name)
    #[arg(long, global = true)]
    pipe_token: Option<String>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Show Windows Terminal protocol connection info
    Info,

    /// Test pipe connection to Windows Terminal
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

    /// Send keys to a pane (like tmux send-keys)
    #[command(alias = "send")]
    SendKeys {
        /// Target pane ID (defaults to active pane)
        #[arg(short = 't', long)]
        target: Option<String>,

        /// Keys to send (supports Enter, Space, C-c, Escape, Tab, BSpace, C-{letter})
        #[arg(required = true, trailing_var_arg = true)]
        keys: Vec<String>,
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

    /// Discover and print the Windows Terminal pipe name and token
    PipeId,

    /// Print shell commands to set WT_PIPE_NAME/WT_MCP_TOKEN environment variables
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

    /// Show a quick-pick dialog in Windows Terminal and print the user's selection
    QuickPick {
        /// Choices to present (1 or more, all positional args)
        #[arg(required = true)]
        choices: Vec<String>,

        /// Title/question shown above the choices
        #[arg(long, default_value = "Select an option")]
        title: String,

        /// Allow freeform text input in addition to choices
        #[arg(long)]
        free_input: bool,
    },

    /// Manage the wt-agent-hooks bridge for supported CLI agents
    /// (Copilot / Claude / Gemini). See `agent_hooks_installer` for
    /// what each action does.
    Hooks {
        #[command(subcommand)]
        action: HooksAction,
    },
}

/// Subcommands for `wta hooks`.
#[derive(Subcommand, Debug)]
enum HooksAction {
    /// (Re-)install the wt-agent-hooks bridge for every supported CLI.
    Install,

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

// ─── Entry Point ────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Extract global pipe overrides for all code paths
    let pipe_override = PipeOverride {
        pipe_name: cli.pipe_name.clone(),
        pipe_token: cli.pipe_token.clone(),
    };

    // Legacy flags first (backward compat)
    if cli.test_pipe {
        return run_test_pipe(&pipe_override).await;
    }
    if cli.info {
        return run_info_mode(&pipe_override).await;
    }
    let json_mode = cli.json;

    match cli.command {
        // Subcommand aliases for legacy modes
        Some(Command::Info) => run_info_mode(&pipe_override).await,
        Some(Command::TestPipe) => run_test_pipe(&pipe_override).await,

        // ── List commands ──
        Some(Command::ListWindows) => {
            let result = wt_call(&pipe_override, "list_windows", json!({})).await?;
            print_output(&result, json_mode, format_windows_human);
            Ok(())
        }
        Some(Command::ListTabs { window_id }) => {
            let channel = connect_channel(&pipe_override).await?;
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
            let channel = connect_channel(&pipe_override).await?;
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
            let result = wt_call(&pipe_override, "create_tab", params).await?;
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
            let channel = connect_channel(&pipe_override).await?;
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

        // ── Send keys ──
        Some(Command::SendKeys { target, keys }) => {
            let channel = connect_channel(&pipe_override).await?;
            let pane_id = resolve_pane_id(&channel, &target).await?;
            let text = translate_keys(&keys);
            channel
                .request("send_input", json!({ "session_id": pane_id, "text": text }))
                .await?;
            Ok(())
        }

        // ── Capture pane ──
        Some(Command::CapturePane { target, max_lines, last_prompt }) => {
            let channel = connect_channel(&pipe_override).await?;
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
            let channel = connect_channel(&pipe_override).await?;
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
            let result = wt_call(&pipe_override, "get_active_pane", json!({})).await?;
            print_output(&result, json_mode, format_active_pane);
            Ok(())
        }

        // ── Pane status ──
        Some(Command::PaneStatus { target }) => {
            let channel = connect_channel(&pipe_override).await?;
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
        Some(Command::PipeId) => {
            run_pipe_id(&pipe_override, json_mode)
        }

        // ── Set environment variables ──
        Some(Command::SetEnv { shell }) => {
            run_set_env(&pipe_override, &shell)
        }

        // ── Delegate prompt to new tab agent ──
        Some(Command::Delegate {
            prompt,
            agent,
            delegate_agent,
            delegate_model,
            cwd,
        }) => {
            run_delegate(&pipe_override, &prompt, &agent, delegate_agent.as_deref(), delegate_model.as_deref(), cwd.as_deref()).await
        }

        // ── Quick pick ──
        Some(Command::QuickPick {
            title,
            choices,
            free_input,
        }) => {
            let channel = connect_channel(&pipe_override).await?;
            let choices_json: Vec<serde_json::Value> =
                choices.iter().map(|c| serde_json::Value::String(c.clone())).collect();
            let result = channel
                .request(
                    "quick_pick",
                    json!({
                        "title": title,
                        "choices": choices_json,
                        "allow_free_input": free_input,
                    }),
                )
                .await?;
            let cancelled = result.get("cancelled").and_then(|v| v.as_bool()).unwrap_or(false);
            if cancelled {
                std::process::exit(1);
            }
            if let Some(selected) = result.get("selected").and_then(|v| v.as_str()) {
                println!("{}", selected);
            }
            Ok(())
        }

        // ── Listen for events ──
        Some(Command::Listen { target }) => {
            run_listen(&pipe_override, target.as_deref()).await
        }

        // ── Manage agent hooks (install/status/uninstall) ──
        Some(Command::Hooks { action }) => match action {
            HooksAction::Install => run_hooks_install(),
            HooksAction::Status => run_hooks_status(json_mode),
            HooksAction::Uninstall { cli } => run_hooks_uninstall(cli, json_mode),
        },

        // ── No subcommand = ACP TUI mode (default) ──
        None => run_default_tui(cli, pipe_override).await,
    }
}

// ─── Hooks subcommand handlers ──────────────────────────────────────────────

fn run_hooks_install() -> Result<()> {
    // Initialize logging so the install attempt is observable in
    // %LOCALAPPDATA%\IntelligentTerminal\logs\wta-install-hooks.log.
    let _guard = logging::init("install-hooks");
    agent_hooks_installer::ensure_installed();
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
            "✗ CLI not on PATH".to_string()
        } else if c.plugin_installed && c.plugin_enabled && c.marketplace_path_valid {
            "✓ installed".to_string()
        } else if c.plugin_installed && !c.marketplace_path_valid {
            "⚠ marketplace path stale".to_string()
        } else if c.plugin_installed {
            "⚠ installed but disabled".to_string()
        } else {
            "✗ not installed".to_string()
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
            println!("    · {}", m);
        }
    }
}

fn yn(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

// ─── Pipe override (CLI --pipe-name / --pipe-token) ─────────────────────────

#[derive(Debug, Clone)]
struct PipeOverride {
    pipe_name: Option<String>,
    pipe_token: Option<String>,
}

/// Resolve pipe connection info. Priority: CLI args > VT discovery > env vars.
fn resolve_pipe_info(po: &PipeOverride) -> Option<shell::wt_channel::ConnectionInfo> {
    use shell::wt_channel::{ConnectionInfo, DiscoverySource, discover_connection_info};

    // 1. CLI override — highest priority. Reuse ComClsid as the discovery
    // tag for explicit overrides (the legacy EnvVar variant is gone).
    if let Some(ref name) = po.pipe_name {
        return Some(ConnectionInfo {
            pipe_name: name.clone(),
            token: po.pipe_token.clone().unwrap_or_default(),
            source: DiscoverySource::ComClsid,
        });
    }

    // 2. VT discovery + env var fallback
    discover_connection_info()
}

// ─── Helper: connect to WT pipe (no debug channel, no ShellManager) ─────────

async fn connect_channel(po: &PipeOverride) -> Result<CliChannel> {
    if let Some(info) = resolve_pipe_info(po) {
        return CliChannel::connect_with(&info.pipe_name, &info.token).await;
    }
    bail!("Cannot find Windows Terminal pipe. Use --pipe-name or set WT_PIPE_NAME.");
}

/// Single-shot: connect + call + return JSON
async fn wt_call(po: &PipeOverride, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    let channel = connect_channel(po).await?;
    channel.request(method, params).await
}

/// Resolve -t target: Some(id) → use it, None → get_active_pane fallback
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

/// Translate tmux key names to actual characters.
///
/// Handles: Enter, Space, Escape, Tab, BSpace, C-c, C-d, C-{letter}
/// Bare strings are passed through as-is (so "echo hello" Enter becomes "echo hello\r").
fn translate_keys(keys: &[String]) -> String {
    let mut out = String::new();
    for key in keys {
        match key.as_str() {
            "Enter" | "CR" => out.push('\r'),
            "Space" => out.push(' '),
            "Escape" | "Esc" => out.push('\x1b'),
            "Tab" => out.push('\t'),
            "BSpace" | "Backspace" => out.push('\x08'),
            "C-c" => out.push('\x03'),
            "C-d" => out.push('\x04'),
            "C-z" => out.push('\x1a'),
            "C-l" => out.push('\x0c'),
            "C-a" => out.push('\x01'),
            "C-e" => out.push('\x05'),
            "C-k" => out.push('\x0b'),
            "C-u" => out.push('\x15'),
            "C-w" => out.push('\x17'),
            other => {
                // Generic C-{letter} pattern
                if other.len() == 3
                    && other.starts_with("C-")
                    && other.as_bytes()[2].is_ascii_alphabetic()
                {
                    let letter = other.as_bytes()[2].to_ascii_lowercase();
                    out.push((letter & 0x1f) as char);
                } else {
                    out.push_str(other);
                }
            }
        }
    }
    out
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

// ─── pipe-id / set-env commands ─────────────────────────────────────────────

fn run_pipe_id(po: &PipeOverride, json_mode: bool) -> Result<()> {
    match resolve_pipe_info(po) {
        Some(info) => {
            if json_mode {
                let val = json!({
                    "pipe_name": info.pipe_name,
                    "token_set": !info.token.is_empty(),
                    "source": format!("{:?}", info.source),
                });
                println!("{}", serde_json::to_string_pretty(&val)?);
            } else {
                println!("{}", info.pipe_name);
            }
            Ok(())
        }
        None => {
            bail!("Cannot discover pipe. Use --pipe-name or set WT_PIPE_NAME, or run inside Windows Terminal.");
        }
    }
}

fn run_set_env(po: &PipeOverride, shell_type: &str) -> Result<()> {
    let info = resolve_pipe_info(po).ok_or_else(|| {
        anyhow::anyhow!("Cannot discover pipe. Use --pipe-name or set WT_PIPE_NAME, or run inside Windows Terminal.")
    })?;

    match shell_type {
        "bash" | "sh" | "zsh" => {
            println!("export WT_PIPE_NAME='{}'", info.pipe_name);
            if !info.token.is_empty() {
                println!("export WT_MCP_TOKEN='{}'", info.token);
            }
            eprintln!("# Run: eval \"$(wta set-env)\"");
        }
        "powershell" | "pwsh" | "ps" => {
            println!("$env:WT_PIPE_NAME = '{}'", info.pipe_name);
            if !info.token.is_empty() {
                println!("$env:WT_MCP_TOKEN = '{}'", info.token);
            }
            eprintln!("# Run: wta set-env -s powershell | Invoke-Expression");
        }
        "cmd" => {
            println!("set WT_PIPE_NAME={}", info.pipe_name);
            if !info.token.is_empty() {
                println!("set WT_MCP_TOKEN={}", info.token);
            }
            eprintln!("REM Run in a for /f loop or copy-paste");
        }
        "fish" => {
            println!("set -gx WT_PIPE_NAME '{}'", info.pipe_name);
            if !info.token.is_empty() {
                println!("set -gx WT_MCP_TOKEN '{}'", info.token);
            }
            eprintln!("# Run: wta set-env -s fish | source");
        }
        other => {
            bail!("Unknown shell type '{}'. Use: bash, powershell, cmd, fish", other);
        }
    }

    Ok(())
}

// ─── Listen mode ────────────────────────────────────────────────────────────

async fn run_listen(po: &PipeOverride, pane_filter: Option<&str>) -> Result<()> {
    let channel = connect_channel(po).await?;
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
    po: &PipeOverride,
    prompt: &str,
    agent_cmd: &str,
    delegate_agent_cmd: Option<&str>,
    delegate_model: Option<&str>,
    cwd: Option<&str>,
) -> Result<()> {
    let _guard = logging::init("delegate");
    tracing::info!(prompt, agent = agent_cmd, cwd, "run_delegate started");

    let (debug_tx, _) = tokio::sync::mpsc::unbounded_channel::<app::DebugMessage>();
    let channel = match connect_to_wt_pipe(po, debug_tx).await {
        Ok(ch) => { tracing::info!("pipe connected"); ch }
        Err(e) => { tracing::warn!(error = %e, "pipe FAILED"); return Err(e); }
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

async fn run_default_tui(cli: Cli, po: PipeOverride) -> Result<()> {
    let _guard = logging::init("main");
    tracing::info!("=== run_default_tui started ===");

    // Debug channel for TUI debug panel (pipe traffic viewer)
    let (debug_tx, debug_rx) = tokio::sync::mpsc::unbounded_channel::<app::DebugMessage>();

    // Try to connect to the Windows Terminal pipe.
    let mut shell_mgr = ShellManager::new();
    let mut wt_event_rx = None;
    let mut wt_pipe_channel: Option<Arc<CliChannel>> = None;
    let wt_connected = match connect_to_wt_pipe(&po, debug_tx.clone()).await {
        Ok(channel) => {
            tracing::info!("Connected to WT pipe OK — subscribing to events");
            // Subscribe to push events before wrapping in Arc.
            wt_event_rx = Some(channel.subscribe_events());
            let cli_arc = Arc::new(channel);
            wt_pipe_channel = Some(Arc::clone(&cli_arc));

            // If WT inherited a duplex pipe pair into our process via
            // STARTUPINFOEX HANDLE_LIST, prefer it for the methods it carries
            // (initially: send_input). All other methods fall through to the
            // CliChannel (wtcli + COM) until they migrate too.
            let wt_channel_for_mgr: Arc<dyn shell::wt_channel::WtChannel> =
                match shell::wt_channel::PipeChannel::from_env() {
                    Ok(Some(pipe)) => match pipe.handshake().await {
                        Ok(()) => {
                            tracing::info!(
                                "PipeChannel handshake OK — routing send_input via inherited pipe"
                            );
                            let pipe_arc: Arc<dyn shell::wt_channel::WtChannel> =
                                Arc::new(pipe);
                            let cli_dyn: Arc<dyn shell::wt_channel::WtChannel> =
                                cli_arc.clone();
                            Arc::new(shell::wt_channel::RoutedChannel::new(
                                pipe_arc,
                                cli_dyn,
                                &["send_input"],
                            )) as Arc<dyn shell::wt_channel::WtChannel>
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "PipeChannel handshake failed; falling back to CliChannel"
                            );
                            cli_arc.clone() as Arc<dyn shell::wt_channel::WtChannel>
                        }
                    },
                    Ok(None) => {
                        tracing::debug!(
                            "No inherited pipe handles in env; using CliChannel only"
                        );
                        cli_arc.clone() as Arc<dyn shell::wt_channel::WtChannel>
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "PipeChannel::from_env error; using CliChannel only"
                        );
                        cli_arc.clone() as Arc<dyn shell::wt_channel::WtChannel>
                    }
                };

            shell_mgr = shell_mgr.with_wt_channel(wt_channel_for_mgr);
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, "NO WT pipe");
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

    run_acp_tui_mode(cli, shell_mgr, wt_connected, debug_rx, pane_identity, wt_event_rx, wt_pipe_channel).await
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
    wt_pipe_channel: Option<Arc<CliChannel>>,
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    execute!(stdout, Print("\x1b]11;#0c0c0c\x07"))?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result =
        run_acp_app(&mut terminal, cli, shell_mgr, wt_connected, debug_rx, pane_identity, wt_event_rx, wt_pipe_channel).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), Print("\x1b]111\x07"), DisableMouseCapture, LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Err(e) = result {
        eprintln!("Error: {e:?}");
        std::process::exit(1);
    }
    Ok(())
}

async fn run_test_pipe(po: &PipeOverride) -> Result<()> {
    use shell::wt_channel::WtChannel;

    println!("Connecting to Windows Terminal pipe...");
    let channel = connect_channel(po).await?;
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

/// Try to connect to the WT pipe using CLI override, VT discovery, or env var fallback.
async fn connect_to_wt_pipe(
    po: &PipeOverride,
    debug_tx: tokio::sync::mpsc::UnboundedSender<app::DebugMessage>,
) -> Result<shell::wt_channel::CliChannel> {
    use shell::wt_channel::CliChannel;

    if let Some(info) = resolve_pipe_info(po) {
        eprintln!(
            "[wta] Discovered pipe via {:?}: {}",
            info.source, info.pipe_name
        );
        let channel = CliChannel::connect_with(&info.pipe_name, &info.token).await?;
        return Ok(channel.with_debug_sender(debug_tx));
    }

    bail!("Cannot find Windows Terminal pipe. Use --pipe-name or set WT_PIPE_NAME.");
}

/// Show Windows Terminal protocol connection info and pane identity.
async fn run_info_mode(po: &PipeOverride) -> Result<()> {
    use shell::wt_channel::{DiscoverySource, WtChannel};

    println!("Windows Terminal Protocol Info");
    println!("========================================");

    let info = match resolve_pipe_info(po) {
        Some(info) => info,
        None => {
            println!("  Status: Not running inside Windows Terminal");
            println!("  (No VT response, WT_PIPE_NAME not set, no --pipe-name)");
            return Ok(());
        }
    };

    let source_str = match info.source {
        DiscoverySource::VtOsc => "VT OSC discovery",
        DiscoverySource::ComClsid => "WT_COM_CLSID env var",
        DiscoverySource::InheritedPipe => "inherited pipe (WT_PROTOCOL_PIPE_R/W)",
    };
    let token_display = if info.token.is_empty() {
        "(dev bypass)"
    } else {
        "(set)"
    };

    println!("  Pipe:   {}", info.pipe_name);
    println!("  Token:  {}", token_display);
    println!("  Source: {}", source_str);
    println!();

    let channel = match CliChannel::connect_with(&info.pipe_name, &info.token).await {
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
    wt_pipe_channel: Option<Arc<CliChannel>>,
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

            // Start the background pipe reader and trigger lazy event registration.
            // start_reader() splits the pipe and must complete before any requests.
            // get_capabilities triggers _ensurePageEventsRegistered() on the WT server.
            if let Some(ref pipe_ch) = wt_pipe_channel {
                tracing::info!("start_reader: starting...");
                pipe_ch.start_reader().await;
                tracing::info!("start_reader: done, sending get_capabilities...");
                match pipe_ch.request("get_capabilities", serde_json::json!({})).await {
                    Ok(v) => tracing::info!(result = %v, "get_capabilities OK"),
                    Err(e) => tracing::warn!(error = %e, "get_capabilities FAILED"),
                }
            } else {
                tracing::warn!("no wt_pipe_channel — events won't work");
            }

            // Background WT event reader: forwards push events from the pipe to the TUI.
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
            // /restart channel: App emits a RestartRequest, the ACP client
            // kills the agent child process, drops the connection, and
            // respawns from scratch. State is cleaned up on both sides.
            let (restart_tx, restart_rx) = tokio::sync::mpsc::unbounded_channel();

            // Spawn the ACP client directly. If the agent isn't installed or
            // fails to authenticate, the error surfaces inline as a Failed
            // connection state — no FRE / setup wizard.
            tokio::task::spawn_local(protocol::acp::client::run_acp_client(
                agent_cmd.clone(),
                cli.acp_model.clone(),
                event_tx.clone(),
                prompt_rx,
                cancel_rx,
                new_session_rx,
                restart_rx,
                Arc::clone(&shell_mgr),
                wt_connected,
            ));

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
            let mut app_state = app::App::new(prompt_tx, recommendation_tx, permission_tx, cancel_tx, new_session_tx, restart_tx, debug_capture_enabled, wt_connected, autofix_enabled);

            // ── Preflight: check the agent CLI before connecting ──────────
            // If the CLI is missing or unauthenticated we surface the
            // Setup wizard via AppEvent::PreflightComplete. Sent before
            // the run loop drains the channel so the wizard appears on
            // first frame.
            let preflight_result = preflight::check_agent(&agent_cmd).await;
            tracing::info!(
                target: "preflight",
                agent_id = %preflight_result.agent_id,
                cli = ?preflight_result.cli_status,
                auth = ?preflight_result.auth_status,
                "preflight done"
            );
            let _ = event_tx.send(app::AppEvent::PreflightComplete(preflight_result));

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

            // ── seed Agents view from on-disk history ─────────────────────
            // history_loader scans Claude/Copilot/Gemini per-CLI session
            // dirs and returns historical AgentSession rows. merge_historical
            // inserts only those whose key isn't already live.
            app_state
                .agent_sessions
                .merge_historical(history_loader::load_all());

            if let Some((pane_id, tab_id, window_id)) = pane_identity {
                app_state.pane_id = Some(pane_id);
                app_state.tab_id = Some(tab_id);
                app_state.window_id = Some(window_id);
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
