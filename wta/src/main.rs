mod agent_registry;
mod app;
mod coordinator;
mod event;
mod logging;
mod preflight;
mod protocol;
mod runtime_paths;
mod shared_host;
mod shell;
mod theme;
mod ui;
mod ui_trace;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::prelude::*;
use serde_json::json;
use std::io;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use shell::wt_channel::{PipeChannel, WtChannel};
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

    /// Wait for a pane's process to exit (poll get_process_status)
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

    /// Pre-warm the shared host (called by Windows Terminal on startup)
    #[command(name = "ensure-host")]
    EnsureHost {
        /// Agent CLI command
        #[arg(long)]
        agent: Option<String>,

        /// Delegate agent CLI command
        #[arg(long)]
        delegate_agent: Option<String>,

        /// Model override for the delegate agent
        #[arg(long)]
        delegate_model: Option<String>,
    },

    /// Attach to a running shared host (lightweight agent pane TUI)
    #[command(name = "attach")]
    Attach {
        /// Override the host pipe name (auto-derived from WT pipe if omitted)
        #[arg(long)]
        host_pipe: Option<String>,

        /// Initial prompt to submit on attach
        #[arg(value_name = "PROMPT")]
        prompt: Option<String>,
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

        /// Source pane ID to read terminal context from
        #[arg(long)]
        source_pane: Option<String>,
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
                "pane_id": pane_id,
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
                .request("send_input", json!({ "pane_id": pane_id, "text": text }))
                .await?;
            Ok(())
        }

        // ── Capture pane ──
        Some(Command::CapturePane { target, max_lines }) => {
            let channel = connect_channel(&pipe_override).await?;
            let pane_id = resolve_pane_id(&channel, &target).await?;
            let mut params = json!({ "pane_id": pane_id });
            if let Some(n) = max_lines {
                params["max_lines"] = json!(n);
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
                .request("close_pane", json!({ "pane_id": pane_id }))
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
                .request("get_process_status", json!({ "pane_id": pane_id }))
                .await?;
            print_output(&result, json_mode, format_pane_status);
            Ok(())
        }

        // ── Wait for ──
        Some(Command::WaitFor {
            target,
            interval,
            timeout,
        }) => {
            let channel = connect_channel(&pipe_override).await?;
            let start = std::time::Instant::now();
            loop {
                let result = channel
                    .request(
                        "get_process_status",
                        json!({ "pane_id": target }),
                    )
                    .await?;

                let is_running = result
                    .get("state")
                    .and_then(|v| v.as_str())
                    .map(|s| s == "running")
                    .unwrap_or(false);

                if !is_running {
                    print_output(&result, json_mode, format_pane_status);
                    return Ok(());
                }

                if timeout > 0 && start.elapsed().as_secs() >= timeout {
                    bail!("Timeout after {}s waiting for pane {} to exit", timeout, target);
                }

                tokio::time::sleep(std::time::Duration::from_millis(interval)).await;
            }
        }

        // ── Pipe discovery ──
        Some(Command::PipeId) => {
            run_pipe_id(&pipe_override, json_mode)
        }

        // ── Set environment variables ──
        Some(Command::SetEnv { shell }) => {
            run_set_env(&pipe_override, &shell)
        }

        // ── Ensure host (called by WT on startup) ──
        Some(Command::EnsureHost {
            agent,
            delegate_agent,
            delegate_model,
        }) => {
            run_ensure_host(
                &pipe_override,
                agent.unwrap_or_else(|| agent_registry::DEFAULT_ACP_COMMAND.to_string()),
                delegate_agent,
                delegate_model,
            )
            .await
        }

        // ── Attach to shared host (lightweight pane TUI) ──
        Some(Command::Attach { host_pipe, prompt }) => {
            run_attach_tui(
                pipe_override,
                cli.agent,
                cli.delegate_agent,
                cli.delegate_model,
                cli.no_autofix,
                host_pipe,
                prompt,
            )
            .await
        }

        // ── Delegate prompt to new tab agent ──
        Some(Command::Delegate {
            prompt,
            agent,
            delegate_agent,
            delegate_model,
            cwd,
            source_pane,
        }) => {
            run_delegate(&pipe_override, &prompt, &agent, delegate_agent.as_deref(), delegate_model.as_deref(), cwd.as_deref(), source_pane.as_deref()).await
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

        // ── No subcommand = ACP TUI mode (default) ──
        None => run_default_tui(cli, pipe_override).await,
    }
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

    // 1. CLI override — highest priority
    if let Some(ref name) = po.pipe_name {
        return Some(ConnectionInfo {
            pipe_name: name.clone(),
            token: po.pipe_token.clone().unwrap_or_default(),
            source: DiscoverySource::EnvVar, // reuse; semantically "explicit"
        });
    }

    // 2. VT discovery + env var fallback
    discover_connection_info()
}

// ─── Helper: connect to WT pipe (no debug channel, no ShellManager) ─────────

async fn connect_channel(po: &PipeOverride) -> Result<PipeChannel> {
    if let Some(info) = resolve_pipe_info(po) {
        return PipeChannel::connect_with(&info.pipe_name, &info.token).await;
    }
    bail!("Cannot find Windows Terminal pipe. Use --pipe-name or set WT_PIPE_NAME.");
}

/// Single-shot: connect + call + return JSON
async fn wt_call(po: &PipeOverride, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    let channel = connect_channel(po).await?;
    channel.request(method, params).await
}

/// Resolve -t target: Some(id) → use it, None → get_active_pane fallback
async fn resolve_pane_id(channel: &PipeChannel, target: &Option<String>) -> Result<String> {
    match target {
        Some(id) => Ok(id.clone()),
        None => {
            let result = channel.request("get_active_pane", json!({})).await?;
            let pane_id = result
                .get("pane_id")
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
async fn get_first_window_id(channel: &PipeChannel) -> Result<String> {
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
async fn get_first_tab_id(channel: &PipeChannel, window_id: &str) -> Result<String> {
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
            let id = json_str_or_num(p, "pane_id");
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
    let id = json_str_or_num(val, "pane_id");
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
    let pane_id = json_str_or_num(val, "pane_id");
    println!("Created tab {} (pane {})", tab_id, pane_id);
}

fn format_created_pane(val: &serde_json::Value) {
    let pane_id = json_str_or_num(val, "pane_id");
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
                .and_then(|p| p.get("pane_id"))
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
    source_pane: Option<&str>,
) -> Result<()> {
    let _guard = logging::init("delegate");
    tracing::info!(prompt, agent = agent_cmd, cwd, source_pane, "run_delegate started");

    let (debug_tx, _) = tokio::sync::mpsc::unbounded_channel::<app::DebugMessage>();
    let channel = match connect_to_wt_pipe(po, debug_tx).await {
        Ok(ch) => { tracing::info!("pipe connected"); ch }
        Err(e) => { tracing::warn!(error = %e, "pipe FAILED"); return Err(e); }
    };
    let shell_mgr = ShellManager::new()
        .with_wt_channel(Arc::new(channel) as Arc<dyn shell::wt_channel::WtChannel>);

    match delegate_with_context(&shell_mgr, prompt, agent_cmd, delegate_agent_cmd, delegate_model, source_pane, cwd).await {
        Ok(()) => { tracing::info!("delegate OK"); Ok(()) }
        Err(e) => { tracing::warn!(error = %e, "delegate FAILED"); Err(e) }
    }
}

/// Shared delegation logic: read context from source pane, build a rich prompt,
/// and create a new tab with the delegate agent. Used by both `wta delegate`
/// CLI and the auto-fix flow.
async fn delegate_with_context(
    shell_mgr: &ShellManager,
    prompt: &str,
    agent_cmd: &str,
    delegate_agent_cmd: Option<&str>,
    delegate_model: Option<&str>,
    source_pane_id: Option<&str>,
    cwd: Option<&str>,
) -> Result<()> {
    // Read terminal context from the source pane if provided.
    let pane_context = if let Some(pane_id) = source_pane_id {
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

    // Build a rich prompt with terminal context.
    let full_prompt = if let Some(ref context) = pane_context {
        format!(
            "{}\n\n## Terminal Context (pane {})\n```\n{}\n```",
            prompt,
            source_pane_id.unwrap_or("?"),
            context
        )
    } else {
        prompt.to_string()
    };

    // Build the delegate agent commandline.
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

    // Launch the delegate agent directly as the tab process.
    shell_mgr
        .wt_create_tab(Some(&commandline), cwd, None)
        .await?;

    Ok(())
}

// ─── Ensure host (background mode) ──────────────────────────────────────────

async fn run_ensure_host(
    po: &PipeOverride,
    agent_cmd: String,
    delegate_agent_cmd: Option<String>,
    delegate_model: Option<String>,
) -> Result<()> {
    let _guard = logging::init("ensure-host");
    tracing::info!("=== ensure-host starting ===");

    let local_set = tokio::task::LocalSet::new();
    local_set
        .run_until(async move {
            // Connect to the Windows Terminal protocol pipe.
            // Must be inside LocalSet because start_reader() uses spawn_local.
            let (debug_tx, _debug_rx) =
                tokio::sync::mpsc::unbounded_channel::<app::DebugMessage>();
            let mut shell_mgr = ShellManager::new();
            let wt_event_rx = match connect_to_wt_pipe(po, debug_tx).await {
                Ok(channel) => {
                    tracing::info!("Connected to WT pipe");
                    let event_rx = channel.subscribe_events();
                    let arc_channel = Arc::new(channel);
                    shell_mgr = shell_mgr.with_wt_channel(
                        arc_channel.clone() as Arc<dyn shell::wt_channel::WtChannel>,
                    );

                    tracing::info!("Starting pipe reader...");
                    arc_channel.start_reader().await;
                    tracing::info!("Pipe reader started, fetching capabilities...");
                    match arc_channel
                        .request("get_capabilities", serde_json::json!({}))
                        .await
                    {
                        Ok(v) => tracing::info!(result = %v, "get_capabilities OK"),
                        Err(e) => tracing::warn!(error = %e, "get_capabilities FAILED"),
                    }
                    Some(event_rx)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "No WT pipe");
                    None
                }
            };
            let shell_mgr = Arc::new(shell_mgr);

            // Compute shared host pipe name.  Use resolve_pipe_info so the
            // hash matches what attach clients compute (they also use it).
            let wt_info = resolve_pipe_info(&po);
            let host_pipe_name = shared_host::pipe_name_for(
                wt_info.as_ref(),
                Some(agent_cmd.as_str()),
                delegate_agent_cmd.as_deref(),
            );
            tracing::info!(pipe = %host_pipe_name, "Host pipe");

            // Autofix command channel. The host-side WT event listener pushes
            // Trigger on actionable failures and Execute when the user clicks
            // the bottom-bar icon / presses Ctrl+. without an attach TUI
            // running. run_host_server routes them into host_command_tx so
            // the existing state machine handles both cases.
            let (autofix_cmd_tx, autofix_cmd_rx) = tokio::sync::mpsc::unbounded_channel::<shared_host::HostAutofixCommand>();

            // Spawn background listener for WT events.
            if let Some(mut wt_rx) = wt_event_rx {
                let sm = Arc::clone(&shell_mgr);
                let agent_for_recs = agent_cmd.clone();
                let delegate_for_recs = delegate_agent_cmd.clone();
                let delegate_model_for_recs = delegate_model.clone();
                let host_autofix_tx = autofix_cmd_tx.clone();
                tokio::task::spawn_local(async move {
                    // Create a recommendation executor for delegation.
                    let (rec_tx, rec_rx) =
                        tokio::sync::mpsc::unbounded_channel();
                    let (evt_tx, _evt_rx) = tokio::sync::mpsc::unbounded_channel();
                    let delegate_agents =
                        crate::coordinator::default_delegate_agent_runtimes(
                            delegate_for_recs.as_deref(),
                            Some(agent_for_recs.as_str()),
                            delegate_model_for_recs.as_deref(),
                        );
                    let delegate_agent_id = delegate_agents
                        .first()
                        .map(|r| r.id.clone())
                        .unwrap_or_else(|| agent_registry::KNOWN_AGENTS[0].id.to_string());
                    tokio::spawn(crate::coordinator::run_recommendation_executor(
                        rec_rx,
                        evt_tx,
                        sm,
                        delegate_agents,
                    ));

                    while let Some(event_json) = wt_rx.recv().await {
                        let method = event_json
                            .get("method")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let pane_id = event_json
                            .get("params")
                            .and_then(|p| p.get("pane_id"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let params = event_json
                            .get("params")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);

                        if method == "agent_prompt" {
                            let prompt = params
                                .get("prompt")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            if !prompt.is_empty() {
                                tracing::info!(prompt = %prompt, "agent_prompt received");
                                let choice = crate::coordinator::RecommendationChoice {
                                    choice: 1,
                                    title: "Delegate to tab agent".into(),
                                    rationale: String::new(),
                                    actions: vec![
                                        crate::coordinator::RecommendedAction::OpenAndSend {
                                            target: crate::coordinator::OpenTarget::Tab,
                                            parent: None,
                                            input: prompt,
                                            agent: Some(delegate_agent_id.clone()),
                                            cwd: None,
                                            title: None,
                                        },
                                    ],
                                };
                                let _ = rec_tx.send(crate::coordinator::ChoiceExecution {
                                    choice,
                                    insert_only: false,
                                });
                            }
                            continue;
                        }

                        // User pressed Ctrl+. or clicked the bottom-bar
                        // autofix icon — execute the armed recommendation.
                        if method == "autofix_execute" {
                            tracing::info!(pane_id = %pane_id, "host autofix_execute");
                            let _ = host_autofix_tx.send(
                                shared_host::HostAutofixCommand::Execute {
                                    pane_id: pane_id.clone(),
                                },
                            );
                            continue;
                        }

                        // Classify any other event — if actionable (OSC 133;D
                        // non-zero exit, connection failures, etc.), fire the
                        // host-side autofix pipeline so the bottom bar goes
                        // from Idle → Pending → Armed even when no attach
                        // TUI (agent pane) is running.
                        let note = crate::app::classify_wt_event(&method, &pane_id, &params);
                        if note.severity == crate::app::WtEventSeverity::Actionable
                            && method != "agent_prompt"
                        {
                            tracing::info!(pane_id = %pane_id, summary = %note.summary, "host autofix trigger");
                            let _ = host_autofix_tx.send(
                                shared_host::HostAutofixCommand::Trigger {
                                    pane_id: pane_id.clone(),
                                    summary: note.summary.clone(),
                                    source_cwd: std::env::var("WTA_SOURCE_CWD")
                                        .ok()
                                        .filter(|s| !s.is_empty()),
                                },
                            );
                        }
                        // A successful command in an armed pane means the error was
                        // resolved without the fix. Dismiss the autofix state.
                        if method == "vt_sequence"
                            && note.severity == crate::app::WtEventSeverity::Informational
                        {
                            if let Some(seq) = params.get("sequence").and_then(|v| v.as_str()) {
                                let is_exit_zero = seq.strip_prefix("osc:133;")
                                    .and_then(|rest| rest.strip_prefix("D;"))
                                    .and_then(|code| code.trim().parse::<i32>().ok())
                                    .map(|c| c == 0)
                                    .unwrap_or(false);
                                if is_exit_zero {
                                    tracing::info!(pane_id = %pane_id, "host autofix clear on success");
                                    let _ = host_autofix_tx.send(
                                        shared_host::HostAutofixCommand::ClearOnSuccess {
                                            pane_id: pane_id.clone(),
                                        },
                                    );
                                }
                            }
                        }
                    }
                });
            }

            // Start the shared host server (ACP client + host service).
            tracing::info!("Starting shared host server...");
            match shared_host::run_host_server(
                host_pipe_name,
                agent_cmd,
                delegate_agent_cmd,
                shell_mgr,
                true, // wt_connected
                Some(autofix_cmd_rx),
            )
            .await
            {
                Ok(()) => tracing::info!("Shared host server exited normally"),
                Err(e) => tracing::warn!(error = %e, "Shared host server FAILED"),
            }
            Ok(())
        })
        .await
}

// ─── Attach TUI mode (lightweight pane client) ─────────────────────────────

async fn run_attach_tui(
    po: PipeOverride,
    agent: String,
    delegate_agent: Option<String>,
    _delegate_model: Option<String>,
    no_autofix: bool,
    host_pipe_override: Option<String>,
    initial_prompt: Option<String>,
) -> Result<()> {
    let _guard = logging::init("attach");
    tracing::info!("=== run_attach_tui started ===");

    let (debug_tx, debug_rx) = tokio::sync::mpsc::unbounded_channel::<app::DebugMessage>();

    // Connect to WT pipe (for pane identity discovery and event forwarding).
    let mut shell_mgr = ShellManager::new();
    let mut wt_event_rx = None;
    let mut wt_pipe_channel: Option<Arc<PipeChannel>> = None;
    let wt_connected = match connect_to_wt_pipe(&po, debug_tx.clone()).await {
        Ok(channel) => {
            tracing::info!("Connected to WT pipe OK");
            wt_event_rx = Some(channel.subscribe_events());
            let arc_channel = Arc::new(channel);
            wt_pipe_channel = Some(Arc::clone(&arc_channel));
            shell_mgr =
                shell_mgr.with_wt_channel(arc_channel as Arc<dyn shell::wt_channel::WtChannel>);
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, "NO WT pipe");
            false
        }
    };
    let shell_mgr = Arc::new(shell_mgr);

    // Discover our own pane identity.
    let pane_identity = if wt_connected {
        discover_pane_identity(&shell_mgr).await
    } else {
        None
    };
    tracing::info!(pane_identity = ?pane_identity, "pane_identity");

    // Compute the shared host pipe name (must match ensure-host).
    let host_pipe_name = if let Some(name) = host_pipe_override {
        name
    } else {
        let wt_info = resolve_pipe_info(&po);
        shared_host::pipe_name_for(
            wt_info.as_ref(),
            Some(agent.as_str()),
            delegate_agent.as_deref(),
        )
    };
    tracing::info!(host_pipe_name = %host_pipe_name, "host_pipe_name");

    // Trigger _ensurePageEventsRegistered on the WT server.
    if let Some(ref pipe_ch) = wt_pipe_channel {
        pipe_ch.start_reader().await;
        let _ = pipe_ch
            .request("get_capabilities", serde_json::json!({}))
            .await;
    }

    let autofix_enabled = !no_autofix;

    // ── Preflight: check agent CLI before connecting to shared host ──
    let preflight_result = preflight::check_agent(&agent).await;
    let start_in_setup = !preflight_result.all_passed();

    // Set up the TUI.
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let local_set = tokio::task::LocalSet::new();
    let result = local_set
        .run_until(async {
            let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
            let (prompt_tx, prompt_rx) = tokio::sync::mpsc::unbounded_channel();
            let (recommendation_tx, recommendation_rx) = tokio::sync::mpsc::unbounded_channel();
            let (permission_tx, permission_rx) = tokio::sync::mpsc::unbounded_channel();
            let debug_capture_enabled = Arc::new(AtomicBool::new(false));

            // Crossterm event reader.
            let evt_tx = event_tx.clone();
            tokio::task::spawn_local(event::read_crossterm_events(evt_tx));

            // Debug message forwarder.
            let dbg_event_tx = event_tx.clone();
            let mut debug_rx = debug_rx;
            tokio::task::spawn_local(async move {
                while let Some(msg) = debug_rx.recv().await {
                    let _ = dbg_event_tx.send(app::AppEvent::DebugPipeMessage(msg));
                }
            });

            // WT event reader (forwards push events to TUI).
            if let Some(mut wt_rx) = wt_event_rx {
                let wt_event_tx = event_tx.clone();
                tokio::task::spawn_local(async move {
                    while let Some(event_json) = wt_rx.recv().await {
                        let method = event_json
                            .get("method")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let pane_id = event_json
                            .get("params")
                            .and_then(|p| p.get("pane_id"))
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

            // Build PaneContext from discovered pane identity.
            let pane_context = shared_host::PaneContext {
                pane_id: pane_identity.as_ref().map(|(p, _, _)| p.clone()),
                tab_id: pane_identity.as_ref().map(|(_, t, _)| t.clone()),
                window_id: pane_identity.as_ref().map(|(_, _, w)| w.clone()),
                cwd: std::env::var("WTA_SOURCE_CWD").ok().filter(|s| !s.is_empty()),
                source_pane_id: std::env::var("WTA_SOURCE_PANE_ID")
                    .ok()
                    .filter(|s| !s.is_empty()),
            };

            // Spawn the attach client (replaces run_acp_client in shared mode).
            // In attach mode, all prompts/recommendations/permissions are forwarded
            // to the shared host — no local ACP client or recommendation executor.
            if !start_in_setup {
                let attach_event_tx = event_tx.clone();
                tokio::task::spawn_local(shared_host::run_attach_client(
                    host_pipe_name.clone(),
                    attach_event_tx,
                    prompt_rx,
                    recommendation_rx,
                    permission_rx,
                    pane_context.clone(),
                    initial_prompt.clone(),
                    debug_capture_enabled.clone(),
                ));
            }

            let (_ui_event_tx, ui_event_rx) = tokio::sync::mpsc::unbounded_channel();

            let mut app_state = app::App::new(
                prompt_tx,
                recommendation_tx,
                permission_tx,
                debug_capture_enabled.clone(),
                wt_connected,
                true, // shared_mode
                autofix_enabled,
            );

            // If preflight failed, enter Setup mode with static guidance
            // (no retry — user must close and reopen the agent pane).
            if start_in_setup {
                let _ = event_tx.send(app::AppEvent::PreflightComplete(preflight_result.clone()));
            }

            if let Some((pane_id, tab_id, window_id)) = pane_identity {
                app_state.pane_id = Some(pane_id);
                app_state.tab_id = Some(tab_id);
                app_state.window_id = Some(window_id);
            }
            app_state.source_pane_id =
                std::env::var("WTA_SOURCE_PANE_ID").ok().filter(|s| !s.is_empty());
            app_state.source_cwd =
                std::env::var("WTA_SOURCE_CWD").ok().filter(|s| !s.is_empty());

            app_state.run(&mut terminal, event_rx, ui_event_rx).await
        })
        .await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), DisableMouseCapture, LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Err(e) = result {
        eprintln!("Error: {e:?}");
        std::process::exit(1);
    }
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
    let mut wt_pipe_channel: Option<Arc<PipeChannel>> = None;
    let wt_connected = match connect_to_wt_pipe(&po, debug_tx.clone()).await {
        Ok(channel) => {
            tracing::info!("Connected to WT pipe OK — subscribing to events");
            // Subscribe to push events before wrapping in Arc.
            wt_event_rx = Some(channel.subscribe_events());
            let arc_channel = Arc::new(channel);
            wt_pipe_channel = Some(Arc::clone(&arc_channel));
            shell_mgr = shell_mgr.with_wt_channel(arc_channel as Arc<dyn shell::wt_channel::WtChannel>);
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
                        let pane_id = match pane.get("pane_id") {
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
    wt_pipe_channel: Option<Arc<PipeChannel>>,
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result =
        run_acp_app(&mut terminal, cli, shell_mgr, wt_connected, debug_rx, pane_identity, wt_event_rx, wt_pipe_channel).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), DisableMouseCapture, LeaveAlternateScreen)?;
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
) -> Result<shell::wt_channel::PipeChannel> {
    use shell::wt_channel::PipeChannel;

    if let Some(info) = resolve_pipe_info(po) {
        eprintln!(
            "[wta] Discovered pipe via {:?}: {}",
            info.source, info.pipe_name
        );
        let channel = PipeChannel::connect_with(&info.pipe_name, &info.token).await?;
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
        DiscoverySource::EnvVar => "WT_PIPE_NAME env var",
        DiscoverySource::ComClsid => "WT_COM_CLSID env var",
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

    let channel = match PipeChannel::connect_with(&info.pipe_name, &info.token).await {
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
                                                let pane_id = match pane.get("pane_id") {
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
    wt_pipe_channel: Option<Arc<PipeChannel>>,
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
                            .and_then(|p| p.get("pane_id"))
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

            // ── Preflight: check agent CLI availability before launching ──
            let preflight_result = preflight::check_agent(&agent_cmd).await;
            let start_in_setup = !preflight_result.all_passed();

            let shell_mgr_for_recs = Arc::clone(&shell_mgr);

            // Only spawn ACP client if preflight passed
            if !start_in_setup {
                let acp_event_tx = event_tx.clone();
                let shell_mgr_clone = Arc::clone(&shell_mgr);
                let agent_cmd_clone = agent_cmd.clone();
                let acp_initial_cwd = std::env::var("WTA_SOURCE_CWD").ok().filter(|s| !s.is_empty());
                tokio::task::spawn_local(protocol::acp::client::run_acp_client(
                    agent_cmd_clone,
                    acp_event_tx,
                    prompt_rx,
                    shell_mgr_clone,
                    wt_connected,
                    acp_initial_cwd,
                ));
            }

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
            let mut app_state = app::App::new(prompt_tx, recommendation_tx, permission_tx, debug_capture_enabled, wt_connected, false, autofix_enabled);

            // If preflight failed, start in Setup mode
            if start_in_setup {
                let _ = event_tx.send(app::AppEvent::PreflightComplete(preflight_result));
            }

            if let Some((pane_id, tab_id, window_id)) = pane_identity {
                app_state.pane_id = Some(pane_id);
                app_state.tab_id = Some(tab_id);
                app_state.window_id = Some(window_id);
            }
            // Read source pane context from env vars set by WT when creating the agent pane.
            app_state.source_pane_id = std::env::var("WTA_SOURCE_PANE_ID").ok().filter(|s| !s.is_empty());
            app_state.source_cwd = std::env::var("WTA_SOURCE_CWD").ok().filter(|s| !s.is_empty());

            // If a prompt was passed via CLI arg (e.g., from command palette creating
            // a new agent pane), delegate it to a new tab agent on startup.
            if let Some(ref initial_prompt) = cli.prompt {
                if !initial_prompt.is_empty() {
                    app_state.delegate_to_tab_agent(initial_prompt, None);
                }
            }

            app_state.run(terminal, event_rx, ui_event_rx).await
        })
        .await
}
