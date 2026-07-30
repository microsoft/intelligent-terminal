#[macro_use]
extern crate rust_i18n;

mod agent_check;
mod agent_hooks_installer;
mod agent_pane_origin;
mod agent_registry;
mod agent_sessions;
mod agent_source;
mod app;
mod app_contracts;
mod cli;
mod clipboard_image;
mod command_recall;
mod commands;
mod coordinator;
mod cwd_util;
mod event;
mod helper;
mod history_loader;
#[cfg(test)]
#[path = "locale_parity_tests.rs"]
mod locale_parity_tests;
mod logging;
mod master;
mod osc52;
mod pane_context;
mod protocol;
mod resolve_command;
mod rtl;
mod runtime_paths;
mod session_history;
mod session_mgmt;
mod session_registry;
mod session_watcher;
mod shell;
mod telemetry;
#[cfg(test)]
mod test_support;
mod text_selection;
mod theme;
mod ui;
mod ui_trace;
mod win32;
mod wsl;
mod wsl_acp;
mod wt_protocol_events;

use anyhow::{bail, Context, Result};
use clap::Parser;
use serde_json::json;
use std::sync::Arc;

use cli::args::{Cli, Command, HooksAction, InitialView, SessionsAction};
#[cfg(test)]
use cli::args::{HooksCliFilter, SessionsOriginArg};
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

// ─── Entry Point ────────────────────────────────────────────────────────────

fn helper_config(cli: Cli) -> helper::config::HelperConfig {
    helper::config::HelperConfig {
        prompt: cli.prompt,
        agent: cli.agent,
        agent_id: cli.agent_id,
        agent_source: cli.agent_source,
        agent_wsl_distro: cli.agent_wsl_distro,
        agent_source_cwd: cli.agent_source_cwd,
        allowed_agent_ids: cli.allowed_agent_ids,
        initial_auth_agent: cli.initial_auth_agent,
        acp_model: cli.acp_model,
        delegate_agent: cli.delegate_agent,
        delegate_model: cli.delegate_model,
        no_autofix: cli.no_autofix,
        setup: cli.setup,
        initial_view: match cli.initial_view {
            InitialView::Chat => helper::config::InitialView::Chat,
            InitialView::Sessions => helper::config::InitialView::Sessions,
        },
        owner_tab_id: cli.owner_tab_id,
        owner_window_id: cli.owner_window_id,
        initial_load_session_id: cli.initial_load_session_id,
        initial_load_cwd: cli.initial_load_cwd,
        start_stashed: cli.start_stashed,
        assume_master_down: cli.assume_master_down,
    }
}

fn master_config(cli: Cli) -> master::config::MasterConfig {
    master::config::MasterConfig {
        agent: cli.agent,
        agent_id: cli.agent_id,
        allowed_agent_ids: cli.allowed_agent_ids,
    }
}

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
    let mut cli = Cli::parse();

    // Initialize file logging exactly once, as the very first thing after
    // arg parsing, so even early-startup failures (locale, ETW registration,
    // legacy-flag dispatch) are captured. The global tracing subscriber can
    // only be set once per process, so every mode routes through here — the
    // per-mode handlers below no longer init their own. The appender's guard
    // is held in a global and flushed via `logging::shutdown_flush()` on every
    // exit path (see the calls below and before each `process::exit`).
    logging::init(&process_label(&cli));
    // Log + flush on console teardown signals (pane/tab/window close, logoff,
    // shutdown) so a torn-down helper isn't a silent disappearance. Installed
    // process-wide; see `install_ctrl_handler` for coverage limits — notably
    // the master is job-killed (KILL_ON_JOB_CLOSE) and won't observe these, so
    // *this handler* doesn't trace routine master teardown. That teardown is
    // still logged, just by the C++ parent: `SharedWta` records both the
    // deliberate job-close and an unexpected exit to terminal-agent-pane.log.
    logging::install_ctrl_handler();
    // Record panics to disk (+ a synchronous wta-panic.log backstop) so a
    // panic isn't a silent death — stderr is invisible for a ConPTY helper /
    // CREATE_NO_WINDOW master. Chains the default hook; semantics unchanged.
    logging::install_panic_hook();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "=== wta starting ===");

    let locale = cli
        .language
        .clone()
        .or_else(|| sys_locale::get_locale())
        .unwrap_or_else(|| "en-US".to_string());
    rust_i18n::set_locale(&normalize_locale(&locale));

    // Register WTA's own ETW TraceLogging provider once per process. WTA uses
    // its own provider (`Microsoft.Windows.Terminal.WTA`), separate from the
    // C++ side. See tools/wta/src/telemetry.rs.
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

    let command = cli.command.take();
    let result = match command {
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

        // ── Profile-aware command resolution ──
        Some(Command::ResolveCommand { token, shell }) => {
            let result = resolve_command::resolve(&token, &shell).await;
            if json_mode {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("{}", resolve_command::format_human(&result));
            }
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
                cli::sessions::run_list(master, origin.to_filter(), json_mode).await
            }
        },

        // ── Manage agent hooks (install/status/uninstall) ──
        Some(Command::Hooks { action }) => match action {
            HooksAction::Install { cli } => cli::hooks::run_install(cli),
            HooksAction::Status => cli::hooks::run_status(json_mode),
            HooksAction::Uninstall { cli } => cli::hooks::run_uninstall(cli, json_mode),
        },

        // ── ACP model list probe ──
        Some(Command::ProbeModels { agent }) => cli::probes::run_models(&agent).await,
        Some(Command::ProbeAgentSources { wsl_distro }) => {
            cli::probes::run_agent_sources(&wsl_distro).await
        }

        // ── ACP session/list probe (diagnostic) ──
        Some(Command::ProbeSessions { agent }) => cli::probes::run_sessions(&agent).await,

        // ── Filtered host ACP history probe (diagnostic) ──
        Some(Command::ProbeHostSessions { agent }) => cli::probes::run_host_sessions(&agent).await,

        // ── WSL ACP history-scan probe (diagnostic) ──
        Some(Command::ProbeWslSessions { cli }) => {
            cli::probes::run_wsl_sessions(cli.as_deref()).await
        }

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
                master::run_master_mode(master_config(cli), pipe_name).await
            } else if let Some(pipe_name) = cli.connect_master.clone() {
                helper::run_helper_mode(helper_config(cli), pipe_name).await
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
        Some(Command::ProbeAgentSources { .. }) => "probe".to_string(),
        Some(Command::ProbeSessions { .. }) => "probe".to_string(),
        Some(Command::ProbeHostSessions { .. }) => "probe".to_string(),
        Some(Command::ProbeWslSessions { .. }) => "probe".to_string(),
        Some(Command::Hooks {
            action: HooksAction::Install { .. },
        }) => "install-hooks".to_string(),
        // All other subcommands are short-lived wtcli-style clients.
        Some(_) => "cli".to_string(),
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
    tracing::info!(
        prompt_chars = prompt.map(|p| p.chars().count()),
        agent = agent_cmd,
        "run_delegate started"
    );
    tracing::trace!(target: "delegate.content", prompt = ?prompt, "run_delegate prompt");

    let (debug_tx, _) = tokio::sync::mpsc::unbounded_channel::<app::DebugMessage>();
    let channel = match CliChannel::connect()
        .await
        .map(|channel| channel.with_debug_sender(debug_tx))
    {
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

/// The WSL distro backing the delegate's active pane, if any — i.e. its shell,
/// reported via `OSC 9001;ShellType`, is `wsl:<distro>` with a **non-empty**
/// distro name (e.g. `wsl:Ubuntu`). The shipped Bash shell integration only
/// emits `wsl:<distro>` when `$WSL_DISTRO_NAME` is set (otherwise it reports
/// `bash`), so a bare `wsl:` never occurs in practice; rejecting it defensively
/// keeps us from ever building a `wsl -d "" …` command. Returns `None` when the
/// pane is missing, has no `shell` field, or the shell is anything else
/// (PowerShell, cmd, …).
/// Whether the delegate agent CLI is actually available inside `distro`.
///
/// PR375 routes a `?<prompt>` from a WSL pane into the distro
/// (`wsl -d <distro> -- bash -lc "<agent> …"`), but the agent may be installed
/// only on the Windows host — the Settings UI verifies the host CLI, never the
/// distro. Probe the distro under a **login** shell (`bash -lc`): the shipped
/// integration and the common CLI installs (npm-global, snap, `~/.local/bin`)
/// only put the agent on the login PATH, so a non-login `bash -c` would miss it.
/// The probe resolves the agent's PATH location and accepts it only when it is a
/// native Linux install — a Windows CLI leaking in via `appendWindowsPath`
/// (resolving under `/mnt/…`) is rejected, so it falls back to the host CLI that
/// can actually run it (see [`wsl_agent_probe_script`]). Returns `false` on any
/// spawn/exec error or timeout so the caller falls back to the known-good
/// Windows host CLI instead of launching a doomed in-distro command that would
/// silently drop the prompt.
async fn wsl_delegate_agent_available(distro: &str, agent_exe: &str) -> bool {
    crate::agent_check::find_wsl_exe(distro, agent_exe)
        .await
        .is_some()
}

/// Whether the delegate agent should be treated as launchable for the active
/// pane's *target* environment.
///
/// `host_launchable` comes from [`crate::coordinator::delegate_command_launchable`],
/// which only inspects the Windows PATH. `wsl_agent_available` is true when the
/// active pane is a WSL distro **and** the agent CLI is installed inside it (see
/// [`wsl_delegate_agent_available`]). Either path makes the delegate
/// launchable: the Windows host, or the in-distro CLI. Without the WSL term a
/// Copilot/Claude installed only in the distro would be treated as
/// non-launchable and silently drop its `?<prompt>` text; with it, a WSL pane
/// whose distro lacks the CLI still falls through to the host term rather than
/// being force-routed into a doomed in-distro launch. The prompt-enrichment and
/// session-pin gates in `delegate_with_context` both key off this value.
fn delegate_launchable_for_target(host_launchable: bool, wsl_agent_available: bool) -> bool {
    host_launchable || wsl_agent_available
}

/// Max bytes of captured terminal context baked into a delegate prompt.
///
/// The enriched prompt rides the `wt_create_tab` commandline (base64-encoded).
/// Windows caps a process commandline at ~32,767 chars, and base64 inflates by
/// 4/3, so an unbounded 30-line capture from a very wide pane could overflow it
/// and fail the launch with "filename or extension is too long". Capping the
/// context keeps the encoded commandline comfortably under that limit; the user
/// prompt itself is assumed small.
const MAX_DELEGATE_CONTEXT_BYTES: usize = 12 * 1024;

/// Trim captured terminal context to at most `max_bytes`, including the
/// truncation marker, while keeping the **tail** (most recent output). Cuts on a
/// UTF-8 char boundary. If the marker does not fit, returns only the valid tail.
fn cap_delegate_context(context: &str, max_bytes: usize) -> String {
    if context.len() <= max_bytes {
        return context.to_string();
    }
    const TRUNCATION_MARKER: &str = "…(truncated)\n";
    let marker = if TRUNCATION_MARKER.len() <= max_bytes {
        TRUNCATION_MARKER
    } else {
        ""
    };
    let tail_bytes = max_bytes - marker.len();
    let mut start = context.len() - tail_bytes;
    while start < context.len() && !context.is_char_boundary(start) {
        start += 1;
    }
    format!("{marker}{}", &context[start..])
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

    // Pre-flight: can the configured delegate agent actually be launched? A
    // misconfigured / nonexistent command still gets its own tab and stays
    // there showing the real failure — cmd's "'<agent>' is not recognized …",
    // then WT's "[process exited with code 1] … press Enter to restart" — just
    // like mistyping a command in any shell. WT keeps a non-zero-exit pane open
    // under closeOnExit=automatic, so there's nothing to "fix" for the common
    // case; we do NOT open a second, canned-message tab.
    //
    // The flag is only used to keep a doomed launch OUT of the prompt-baking
    // path below. Baking the active pane's output into `cmd /c <agent>
    // -i "<context>"` is fragile: a stray `"`/`&` in that arbitrary text can
    // unbalance cmd's quote tracking so cmd runs a trailing token and exits 0,
    // which — under closeOnExit=automatic — closes the pane before the error is
    // readable (the original "flash shut"). A bare `cmd /c <agent>` instead
    // fails cleanly with a non-zero code and stays put.
    let launchable = crate::coordinator::delegate_command_launchable(&runtime.commandline);

    // A WSL pane runs the agent *inside the distro* (`wsl -d <distro> -- …`), so
    // the Windows-host launchable check does not apply to it. Fetch the active
    // pane up front so the gate below and the WSL branch further down can see
    // it. See `delegate_launchable_for_target`.
    let active = shell_mgr.wt_get_active_pane().await.ok();

    // If the active pane is a WSL distro, prefer running the agent inside it —
    // but only when the agent CLI is actually installed there. Otherwise, fall
    // back to the Windows host CLI (which the Settings UI already verified is
    // installed): an in-distro launch would just print "<agent>: command not
    // found" and drop the prompt. Probe the distro once, up front, so the
    // launchable gate, the WSL branch, and the host fallback all agree.
    let wsl_distro: Option<String> =
        crate::agent_source::active_pane_wsl_distro(active.as_ref()).map(str::to_string);
    let wsl_agent_available = match wsl_distro.as_deref() {
        Some(distro) => {
            let agent_exe =
                crate::coordinator::split_windows_commandline(runtime.commandline.trim())
                    .into_iter()
                    .next()
                    .unwrap_or_default();
            let available = wsl_delegate_agent_available(distro, &agent_exe).await;
            if !available {
                tracing::info!(
                    target: "delegate",
                    distro,
                    agent = %agent_exe,
                    "delegate agent not available in WSL distro — falling back to Windows host CLI",
                );
            }
            available
        }
        None => false,
    };

    let launchable_for_target = delegate_launchable_for_target(launchable, wsl_agent_available);

    if !launchable_for_target {
        // Log only the executable (first token), never the full commandline: a
        // custom agent command can embed tokens/credentials that shouldn't land
        // in the log. The full commandline stays trace-only (below).
        let exe = crate::coordinator::split_windows_commandline(&runtime.commandline)
            .into_iter()
            .next()
            .unwrap_or_default();
        tracing::warn!(
            target: "delegate",
            agent = %exe,
            "delegate agent not launchable — opening its tab with the bare command so the real error stays visible",
        );
    }

    // Pin a session id we choose, so the launched CLI writes its session under a
    // known id and we can bind it to the pane without hooks. Only for agents that
    // advertise `--session-id` (Copilot/Claude/Gemini); `None` otherwise. We
    // identify the agent with `resolve_agent_id_from_cmd` (not a naive
    // `split_whitespace`) so quoted/space-containing paths and adapter launches
    // resolve correctly -- and so this decision matches the one the command
    // builder makes when it appends the flag, keeping the pinned id and the
    // actual launch flag in agreement. A non-launchable command will never
    // produce a session, so skip pinning (and the born-bound registration
    // below). A WSL pane is launchable via the distro, so it pins like any
    // other supported agent.
    let pinned_session_id: Option<String> = if launchable_for_target {
        crate::agent_registry::lookup_profile_by_id(
            crate::agent_registry::resolve_agent_id_from_cmd(&runtime.commandline),
        )
        .new_session_id_flag
        .map(|_| uuid::Uuid::new_v4().to_string())
    } else {
        None
    };

    // ── Enriched prompt ──────────────────────────────────────────────────
    // Bake the active pane's output into the prompt when the agent is
    // launchable for the target environment — the Windows pre-flight, or a WSL
    // pane that will run the agent inside the distro. A non-launchable agent
    // stays in the bare-command path so its failure is clean and visible.
    let enriched_prompt: Option<String> = match prompt {
        Some(prompt) if !prompt.trim().is_empty() && launchable_for_target => {
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

            // The `## Terminal Context (pane …)` heading is built from
            // `TERMINAL_CONTEXT_TITLE_MARKER` (the single source of truth) so the
            // master-side title filter (`host_titles_via_acp`) can recognise —
            // and skip — this injected block if an agent CLI echoes the first
            // user message back as a `session/list` title.
            Some(match (pane_context, active_pane_id) {
                (Some(context), Some(pane_id)) => format!(
                    "{}\n\n{}{})\n```\n{}\n```",
                    prompt,
                    crate::session_registry::TERMINAL_CONTEXT_TITLE_MARKER,
                    pane_id,
                    cap_delegate_context(&context, MAX_DELEGATE_CONTEXT_BYTES)
                ),
                _ => prompt.to_string(),
            })
        }
        _ => None,
    };

    // ── Windows-native commandline (fallback for non-WSL) ────────────────
    let commandline = crate::coordinator::build_delegate_launch_commandline_with_session(
        runtime,
        enriched_prompt.as_deref(),
        pinned_session_id.as_deref(),
    )?;

    // ── WSL delegate path ───────────────────────────────────────────────────
    // Taken only when the active pane is a WSL distro AND the agent CLI is
    // installed inside it (`wsl_agent_available`). Build a WSL-native command
    // that runs the agent CLI inside the distro (using the Linux toolchain and
    // filesystem). When the distro lacks the CLI we fall through to the Windows
    // host path below, which sanitizes the pane's POSIX cwd to the Windows home.
    //
    // Delivery (see `build_wsl_delegate_commandline`): the prompt rides as an
    // inline base64 payload decoded in-distro — base64's alphabet has no shell
    // syntax characters and no `%`, so it survives WT's `ExpandEnvironmentStringsW`
    // and the `wsl.exe` interop's expansion pass. The bash command is escaped for
    // that pass, then wrapped once for Windows `CommandLineToArgvW`:
    //   1. build_wsl_delegate_commandline() → base64-inline bash command,
    //      pre-escaped for the wsl.exe expansion pass (`\`, `$`, backtick)
    //   2. quote_windows_commandline_arg() → Windows CommandLineToArgvW escaping
    //      → embed in format!("bash -lc {}")
    //   3. → wsl -d <distro> --cd "<cwd>" -- bash -lc <escaped>
    //
    // Composability works because the two layers have disjoint special
    // characters: ' is special to bash, " is special to Windows.
    if wsl_agent_available {
        // `wsl_agent_available` implies both `wsl_distro` and `active` are set
        // (it is derived from them above); the `if let` is a defensive guard
        // that falls through to the host path in the impossible None case.
        if let (Some(distro), Some(active_pane)) = (wsl_distro.as_deref(), active.as_ref()) {
            let wsl_agent_cmd = crate::coordinator::build_wsl_delegate_commandline(
                runtime,
                enriched_prompt.as_deref(),
                pinned_session_id.as_deref(),
            )?;
            let escaped = crate::coordinator::quote_windows_commandline_arg(&wsl_agent_cmd);
            let login_invocation = format!("bash -lc {}", escaped);
            let distro_arg = crate::coordinator::quote_windows_commandline_arg(distro);
            let wsl_cwd = active_pane
                .get("cwd")
                .and_then(|v| v.as_str())
                .filter(|s| s.starts_with('/') && !s.contains('"'));
            let wsl_commandline = match wsl_cwd {
                Some(cwd) => {
                    format!("wsl -d {distro_arg} --cd \"{cwd}\" -- {login_invocation}")
                }
                None => format!("wsl -d {distro_arg} -- {login_invocation}"),
            };

            tracing::debug!("delegate_with_context: launching in WSL ({distro})");
            tracing::trace!(
                target: "delegate.content",
                commandline = %wsl_commandline,
                "wsl delegate commandline",
            );

            let create_resp = shell_mgr
                .wt_create_tab(Some(&wsl_commandline), None, None, None)
                .await?;
            let pane_guid = create_resp
                .get("session_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            tracing::info!(
                target: "delegate",
                pane_guid = ?pane_guid,
                pinned = ?pinned_session_id,
                distro,
                "delegate WSL tab created",
            );

            // Born-bound registration for the WSL delegate session — but only
            // when WSL sessions are enabled. The whole WSL surface is gated on
            // `WTA_WSL_SESSIONS`; with it off we must not surface *any* WSL
            // session, born-bound delegate rows included (the master-side
            // historical WSL scan is already gated, so skipping this registration
            // keeps a `?<prompt>` WSL delegate out of the session view). The tab
            // still opens and the CLI still runs — it's just untracked, exactly
            // like every other WSL session while the flag is off.
            //
            // The distro is threaded through so the master stamps the row
            // `Wsl { distro }` → the session view shows the `[WSL-<distro>]`
            // prefix.
            if crate::history_loader::wsl_sessions_enabled() {
                if let (Some(sid), Some(pane)) =
                    (pinned_session_id.as_deref(), pane_guid.as_deref())
                {
                    cli::sessions::register_launched(
                        sid,
                        pane,
                        &runtime.id,
                        wsl_cwd.or(cwd),
                        Some(distro),
                    )
                    .await;
                }
            }
            return Ok(());
        }
    }

    // ── Windows (existing) path ────────────────────────────────────────────
    // The delegate always launches a Windows agent CLI (Copilot/Claude/Gemini).
    // If the active pane is WSL, `cwd` is a POSIX path (e.g. "/home/user") that
    // a Windows process can't use as a working directory — sanitize it to the
    // Windows home so the CLI still launches.
    tracing::debug!("delegate_with_context: launching");
    tracing::trace!(target: "delegate.content", commandline, "delegate_with_context commandline");

    let windows_home = std::env::var("USERPROFILE").ok();
    let sanitized_cwd =
        crate::coordinator::sanitize_windows_agent_cwd(cwd, windows_home.as_deref());

    let create_resp = shell_mgr
        .wt_create_tab(Some(&commandline), sanitized_cwd.as_deref(), None, None)
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
        cli::sessions::register_launched(sid, pane, &runtime.id, cwd, None).await;
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

#[cfg(test)]
mod cli_tests;

#[cfg(test)]
mod delegate_context_tests {
    use super::cap_delegate_context;

    #[test]
    fn cap_returns_short_context_unchanged() {
        let ctx = "small output";
        assert_eq!(cap_delegate_context(ctx, 1024), ctx);
    }

    #[test]
    fn cap_keeps_tail_and_marks_truncation() {
        let ctx: String = (0..5000u32)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        let out = cap_delegate_context(&ctx, 1000);
        assert!(out.starts_with("…(truncated)\n"));
        // keeps the tail (most recent output)
        assert!(out.ends_with(&ctx[ctx.len() - 100..]));
        assert!(out.len() <= 1000);
    }

    #[test]
    fn cap_is_char_boundary_safe() {
        // Each '⭐' is 3 bytes; cutting must land on a char boundary (no panic).
        let ctx: String = std::iter::repeat('⭐').take(500).collect();
        let out = cap_delegate_context(&ctx, 100);
        assert!(out.len() <= 100);
        assert!(out.ends_with('⭐'));
        assert!(out
            .chars()
            .all(|c| c == '⭐' || "…(truncated)\n".contains(c)));
    }

    #[test]
    fn cap_omits_marker_when_limit_is_too_small() {
        assert_eq!(cap_delegate_context("prefix-tail", 4), "tail");
    }
}
