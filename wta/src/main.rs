mod app;
mod coordinator;
mod event;
mod protocol;
mod shell;
mod theme;
mod ui;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::prelude::*;
use serde_json::json;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use shell::wt_channel::{PipeChannel, WtChannel};
use shell::ShellManager;

// ─── CLI Definition ─────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    name = "wta",
    about = "Windows Terminal Agent — ACP TUI client / MCP tool server / tmux-like CLI"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Initial prompt to send to the agent (ACP mode only)
    #[arg(value_name = "PROMPT")]
    prompt: Option<String>,

    /// Agent CLI command (e.g. "copilot --acp --stdio")
    #[arg(long, default_value = "copilot --acp --stdio")]
    agent: String,

    // Legacy flags (hidden, backward compat)
    #[arg(long, hide = true)]
    mcp: bool,
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
    /// Run as MCP server (headless, no TUI)
    Mcp,

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

        /// Split horizontally (panes stacked, new pane below)
        #[arg(short = 'H', long)]
        horizontal: bool,

        /// Split vertically (panes side by side, new pane to the right)
        #[arg(short = 'v', long)]
        vertical: bool,

        /// Split with the new pane to the right
        #[arg(long)]
        right: bool,

        /// Split with the new pane to the left
        #[arg(long)]
        left: bool,

        /// Split with the new pane below
        #[arg(long)]
        down: bool,

        /// Split with the new pane above
        #[arg(long)]
        up: bool,

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
    if cli.mcp {
        return run_mcp_mode(&pipe_override).await;
    }

    let json_mode = cli.json;

    match cli.command {
        // Subcommand aliases for legacy modes
        Some(Command::Mcp) => run_mcp_mode(&pipe_override).await,
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
        Some(Command::ListPanes { tab_id, window_id }) => {
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
            right,
            left,
            down,
            up,
            size,
            command,
        }) => {
            let channel = connect_channel(&pipe_override).await?;
            let pane_id = resolve_pane_id(&channel, &target).await?;
            let split_dir = if right {
                "right"
            } else if left {
                "left"
            } else if down {
                "down"
            } else if up {
                "up"
            } else if horizontal {
                // WT "horizontal split" means a horizontal divider, so the new pane is below.
                "down"
            } else if vertical {
                // WT "vertical split" means a vertical divider, so the new pane is to the right.
                "right"
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
                    .request("get_process_status", json!({ "pane_id": target }))
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
                    bail!(
                        "Timeout after {}s waiting for pane {} to exit",
                        timeout,
                        target
                    );
                }

                tokio::time::sleep(std::time::Duration::from_millis(interval)).await;
            }
        }

        // ── Pipe discovery ──
        Some(Command::PipeId) => run_pipe_id(&pipe_override, json_mode),

        // ── Set environment variables ──
        Some(Command::SetEnv { shell }) => run_set_env(&pipe_override, &shell),

        // ── Quick pick ──
        Some(Command::QuickPick {
            title,
            choices,
            free_input,
        }) => {
            let channel = connect_channel(&pipe_override).await?;
            let choices_json: Vec<serde_json::Value> = choices
                .iter()
                .map(|c| serde_json::Value::String(c.clone()))
                .collect();
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
            let cancelled = result
                .get("cancelled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if cancelled {
                std::process::exit(1);
            }
            if let Some(selected) = result.get("selected").and_then(|v| v.as_str()) {
                println!("{}", selected);
            }
            Ok(())
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
    use shell::wt_channel::{discover_connection_info, ConnectionInfo, DiscoverySource};

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
async fn wt_call(
    po: &PipeOverride,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
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
                .ok_or_else(|| {
                    anyhow::anyhow!("No active pane found. Use -t to specify a pane ID.")
                })?;
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
            bail!(
                "Unknown shell type '{}'. Use: bash, powershell, cmd, fish",
                other
            );
        }
    }

    Ok(())
}

// ─── Default ACP TUI mode ───────────────────────────────────────────────────

async fn run_default_tui(cli: Cli, po: PipeOverride) -> Result<()> {
    fn acp_startup_log(msg: &str) {
        use std::io::Write;
        if std::env::var("WTA_DEBUG_LOG").as_deref() != Ok("1") {
            return;
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("wta-acp-debug.log")
        {
            let elapsed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            let _ = writeln!(f, "[{:.3}] {}", elapsed.as_secs_f64(), msg);
            let _ = f.flush();
        }
    }

    let startup = std::time::Instant::now();
    acp_startup_log(&format!(
        "run_default_tui start pid={} cwd={} agent={} (t+{:.3}s)",
        std::process::id(),
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<unknown>".to_string()),
        cli.agent,
        startup.elapsed().as_secs_f64()
    ));

    let debug_capture_enabled = Arc::new(AtomicBool::new(false));

    // Debug channel for TUI debug panel (pipe traffic viewer)
    let (debug_tx, debug_rx) = tokio::sync::mpsc::unbounded_channel::<app::DebugMessage>();
    acp_startup_log(&format!(
        "run_default_tui debug channel ready (t+{:.3}s)",
        startup.elapsed().as_secs_f64()
    ));

    // Try to connect to the Windows Terminal pipe.
    let mut shell_mgr = ShellManager::new();
    acp_startup_log(&format!(
        "run_default_tui connecting WT pipe (t+{:.3}s)",
        startup.elapsed().as_secs_f64()
    ));
    let wt_connected =
        match connect_to_wt_pipe(&po, debug_tx.clone(), debug_capture_enabled.clone()).await {
            Ok((info, channel)) => {
                eprintln!("[wta] Connected to Windows Terminal pipe");
                shell_mgr = shell_mgr
                    .with_wt_connection_info(info)
                    .with_wt_channel(Arc::new(channel));
                acp_startup_log(&format!(
                    "run_default_tui WT pipe connected (t+{:.3}s)",
                    startup.elapsed().as_secs_f64()
                ));
                true
            }
            Err(e) => {
                eprintln!("[wta] No WT pipe (local-only mode): {}", e);
                acp_startup_log(&format!(
                    "run_default_tui WT pipe unavailable: {} (t+{:.3}s)",
                    e,
                    startup.elapsed().as_secs_f64()
                ));
                false
            }
        };
    let shell_mgr = Arc::new(shell_mgr);
    acp_startup_log(&format!(
        "run_default_tui shell manager ready wt_connected={} (t+{:.3}s)",
        wt_connected,
        startup.elapsed().as_secs_f64()
    ));

    // Try to discover our own pane identity by PID matching
    let pane_identity = if wt_connected {
        acp_startup_log(&format!(
            "run_default_tui discovering pane identity (t+{:.3}s)",
            startup.elapsed().as_secs_f64()
        ));
        let pane_identity = discover_pane_identity(&shell_mgr).await;
        acp_startup_log(&format!(
            "run_default_tui pane identity discovered: {:?} (t+{:.3}s)",
            pane_identity,
            startup.elapsed().as_secs_f64()
        ));
        pane_identity
    } else {
        acp_startup_log(&format!(
            "run_default_tui skipping pane identity discovery (t+{:.3}s)",
            startup.elapsed().as_secs_f64()
        ));
        None
    };

    acp_startup_log(&format!(
        "run_default_tui entering ACP TUI mode (t+{:.3}s)",
        startup.elapsed().as_secs_f64()
    ));
    run_acp_tui_mode(
        cli,
        shell_mgr,
        wt_connected,
        debug_rx,
        debug_capture_enabled,
        pane_identity,
    )
    .await
}

async fn run_mcp_mode(po: &PipeOverride) -> Result<()> {
    fn mcp_startup_log(msg: &str) {
        use std::io::Write;
        if std::env::var("WTA_DEBUG_LOG").as_deref() == Ok("0") {
            return;
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("wta-mcp-debug.log")
        {
            let elapsed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            let _ = writeln!(f, "[{:.3}] {}", elapsed.as_secs_f64(), msg);
            let _ = f.flush();
        }
    }

    let startup = std::time::Instant::now();
    mcp_startup_log(&format!(
        "run_mcp_mode start pid={} cwd={}",
        std::process::id(),
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<unknown>".to_string())
    ));

    // Need a ShellManager with WT channel for MCP server
    let mut shell_mgr = ShellManager::new();
    let resolved_pipe = resolve_pipe_info(po);
    mcp_startup_log(&format!("resolved pipe info: {:?}", resolved_pipe));
    mcp_startup_log("connecting WT channel for MCP server...");
    match connect_channel(po).await {
        Ok(channel) => {
            mcp_startup_log(&format!(
                "WT channel connected for MCP server (t+{:.3}s)",
                startup.elapsed().as_secs_f64()
            ));
            shell_mgr = shell_mgr.with_wt_channel(Arc::new(channel));
        }
        Err(e) => {
            mcp_startup_log(&format!(
                "WT channel unavailable for MCP server (t+{:.3}s): {}",
                startup.elapsed().as_secs_f64(),
                e
            ));
            eprintln!("[wta] No WT pipe for MCP: {}", e);
        }
    }
    mcp_startup_log(&format!(
        "starting MCP stdio server (t+{:.3}s)",
        startup.elapsed().as_secs_f64()
    ));
    protocol::mcp::server::run_mcp_server(Arc::new(shell_mgr)).await
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::CommandFactory;

    #[test]
    fn cli_configuration_has_no_conflicting_flags() {
        Cli::command().debug_assert();
    }
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
    debug_capture_enabled: Arc<AtomicBool>,
    pane_identity: Option<(String, String, String)>,
) -> Result<()> {
    fn acp_startup_log(msg: &str) {
        use std::io::Write;
        if std::env::var("WTA_DEBUG_LOG").as_deref() != Ok("1") {
            return;
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("wta-acp-debug.log")
        {
            let elapsed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            let _ = writeln!(f, "[{:.3}] {}", elapsed.as_secs_f64(), msg);
            let _ = f.flush();
        }
    }

    let startup = std::time::Instant::now();
    acp_startup_log(&format!(
        "run_acp_tui_mode start wt_connected={} pane_identity={:?} (t+{:.3}s)",
        wt_connected,
        pane_identity,
        startup.elapsed().as_secs_f64()
    ));
    enable_raw_mode()?;
    acp_startup_log(&format!(
        "run_acp_tui_mode raw mode enabled (t+{:.3}s)",
        startup.elapsed().as_secs_f64()
    ));
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    acp_startup_log(&format!(
        "run_acp_tui_mode entered alternate screen (t+{:.3}s)",
        startup.elapsed().as_secs_f64()
    ));
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    acp_startup_log(&format!(
        "run_acp_tui_mode terminal initialized (t+{:.3}s)",
        startup.elapsed().as_secs_f64()
    ));

    acp_startup_log(&format!(
        "run_acp_tui_mode starting run_acp_app (t+{:.3}s)",
        startup.elapsed().as_secs_f64()
    ));
    let result = run_acp_app(
        &mut terminal,
        cli,
        shell_mgr,
        wt_connected,
        debug_rx,
        debug_capture_enabled,
        pane_identity,
    )
    .await;
    acp_startup_log(&format!(
        "run_acp_tui_mode run_acp_app returned (t+{:.3}s)",
        startup.elapsed().as_secs_f64()
    ));

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    acp_startup_log(&format!(
        "run_acp_tui_mode terminal restored (t+{:.3}s)",
        startup.elapsed().as_secs_f64()
    ));

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
    debug_capture_enabled: Arc<AtomicBool>,
) -> Result<(
    shell::wt_channel::ConnectionInfo,
    shell::wt_channel::PipeChannel,
)> {
    use shell::wt_channel::PipeChannel;

    if let Some(info) = resolve_pipe_info(po) {
        eprintln!(
            "[wta] Discovered pipe via {:?}: {}",
            info.source, info.pipe_name
        );
        let channel = PipeChannel::connect_with(&info.pipe_name, &info.token).await?;
        return Ok((
            info,
            channel.with_debug_sender(debug_tx, debug_capture_enabled),
        ));
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
                                                let pane_id = match pane.get("pane_id") {
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
    debug_capture_enabled: Arc<AtomicBool>,
    pane_identity: Option<(String, String, String)>,
) -> Result<()> {
    fn acp_startup_log(msg: &str) {
        use std::io::Write;
        if std::env::var("WTA_DEBUG_LOG").as_deref() != Ok("1") {
            return;
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("wta-acp-debug.log")
        {
            let elapsed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            let _ = writeln!(f, "[{:.3}] {}", elapsed.as_secs_f64(), msg);
            let _ = f.flush();
        }
    }

    let startup = std::time::Instant::now();
    let agent_cmd = cli.agent.clone();
    acp_startup_log(&format!(
        "run_acp_app start agent={} wt_connected={} pane_identity={:?} (t+{:.3}s)",
        agent_cmd,
        wt_connected,
        pane_identity,
        startup.elapsed().as_secs_f64()
    ));

    let local_set = tokio::task::LocalSet::new();
    acp_startup_log(&format!(
        "run_acp_app LocalSet created (t+{:.3}s)",
        startup.elapsed().as_secs_f64()
    ));
    local_set
        .run_until(async move {
            acp_startup_log(&format!(
                "run_acp_app LocalSet entered (t+{:.3}s)",
                startup.elapsed().as_secs_f64()
            ));
            let (ui_tx, ui_rx) = tokio::sync::mpsc::unbounded_channel();
            let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
            let (prompt_tx, prompt_rx) = tokio::sync::mpsc::unbounded_channel();
            let (recommendation_tx, recommendation_rx) = tokio::sync::mpsc::unbounded_channel();
            acp_startup_log(&format!(
                "run_acp_app channels created (t+{:.3}s)",
                startup.elapsed().as_secs_f64()
            ));

            tokio::task::spawn_local(event::read_crossterm_events(ui_tx));
            acp_startup_log(&format!(
                "run_acp_app crossterm reader spawned (t+{:.3}s)",
                startup.elapsed().as_secs_f64()
            ));

            let dbg_event_tx = event_tx.clone();
            let dbg_capture = debug_capture_enabled.clone();
            tokio::task::spawn_local(async move {
                while let Some(msg) = debug_rx.recv().await {
                    if dbg_capture.load(Ordering::Relaxed) {
                        let _ = dbg_event_tx.send(app::AppEvent::DebugPipeMessage(msg));
                    }
                }
            });
            acp_startup_log(&format!(
                "run_acp_app debug forwarder spawned (t+{:.3}s)",
                startup.elapsed().as_secs_f64()
            ));

            let executor_event_tx = event_tx.clone();
            let executor_shell_mgr = shell_mgr.clone();
            tokio::task::spawn_local(coordinator::run_recommendation_executor(
                recommendation_rx,
                executor_event_tx,
                executor_shell_mgr,
                coordinator::default_delegate_agent_runtimes(),
            ));
            acp_startup_log(&format!(
                "run_acp_app recommendation executor spawned (t+{:.3}s)",
                startup.elapsed().as_secs_f64()
            ));

            let acp_event_tx = event_tx.clone();
            let acp_shell_mgr = shell_mgr.clone();
            tokio::task::spawn_local(protocol::acp::client::run_acp_client(
                agent_cmd,
                cli.prompt.clone(),
                acp_event_tx,
                prompt_rx,
                acp_shell_mgr,
                wt_connected,
                pane_identity.clone(),
            ));
            acp_startup_log(&format!(
                "run_acp_app ACP client task spawned (t+{:.3}s)",
                startup.elapsed().as_secs_f64()
            ));

            let mut app_state = app::App::new(
                prompt_tx,
                recommendation_tx,
                debug_capture_enabled.clone(),
                wt_connected,
            );
            if let Some((pane_id, tab_id, window_id)) = pane_identity {
                app_state.pane_id = Some(pane_id);
                app_state.tab_id = Some(tab_id);
                app_state.window_id = Some(window_id);
            }
            acp_startup_log(&format!(
                "run_acp_app app state ready; entering UI loop (t+{:.3}s)",
                startup.elapsed().as_secs_f64()
            ));
            app_state.run(terminal, ui_rx, event_rx).await
        })
        .await
}
