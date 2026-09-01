#[macro_use]
extern crate rust_i18n;

mod action_links;
mod agent_check;
mod agent_hooks_installer;
mod agent_pane_origin;
mod agent_registry;
mod agent_sessions;
mod agent_source;
mod agent_tools;
mod app;
mod app_contracts;
mod cli;
mod clipboard_image;
mod command_recall;
mod commands;
mod coordinator;
mod custom_model_provider;
mod cwd_util;
mod event;
mod helper;
#[cfg(test)]
#[path = "hook_contract_tests.rs"]
mod hook_contract_tests;
#[cfg(test)]
#[path = "locale_parity_tests.rs"]
mod locale_parity_tests;
mod logging;
mod master;
mod osc52;
mod pane_context;
mod protocol;
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
mod turn_context;
mod ui;
mod ui_trace;
mod usage;
mod win32;
mod wt_protocol_events;

use anyhow::Result;
use clap::Parser;

use cli::args::{Cli, Command, HooksAction, InitialView};
#[cfg(test)]
use cli::args::{HooksCliFilter, SessionsAction, SessionsOriginArg};

i18n!("locales", fallback = "en-US");

/// Normalize a detected OS locale to the closest available locale file.
/// Mimics Windows MRT behavior with script-aware affinity matching.
fn normalize_locale(locale: &str) -> String {
    let available = rust_i18n::available_locales!();

    if available.iter().any(|l| l.eq_ignore_ascii_case(locale)) {
        return locale.to_string();
    }

    let affinity_target = match locale.to_lowercase().as_str() {
        "zh-hk" | "zh-mo" | "zh-hant" | "zh-hant-tw" | "zh-hant-hk" | "zh-hant-mo" => Some("zh-TW"),
        "zh-sg" | "zh-hans" | "zh-hans-cn" | "zh-hans-sg" => Some("zh-CN"),
        "en-au" | "en-nz" | "en-ie" | "en-in" | "en-sg" | "en-za" | "en-hk" | "en-my" | "en-ph"
        | "en-pk" | "en-ng" | "en-ke" | "en-gh" => Some("en-GB"),
        "es-ar" | "es-co" | "es-cl" | "es-pe" | "es-ve" | "es-ec" | "es-gt" | "es-cu" | "es-bo"
        | "es-do" | "es-hn" | "es-py" | "es-sv" | "es-ni" | "es-cr" | "es-pa" | "es-uy"
        | "es-pr" | "es-us" | "es-419" => Some("es-MX"),
        "fr-be" | "fr-ch" | "fr-lu" | "fr-mc" | "fr-sn" | "fr-ci" | "fr-ml" | "fr-cm" | "fr-mg"
        | "fr-cd" | "fr-dz" | "fr-tn" | "fr-ma" => Some("fr-FR"),
        "pt-ao" | "pt-mz" | "pt-gw" | "pt-tl" | "pt-cv" | "pt-st" => Some("pt-PT"),
        "sr-latn-ba" | "sr-latn-me" | "sr-latn-xk" => Some("sr-Latn-RS"),
        "sr-cyrl-ba" | "sr-cyrl-me" | "sr-cyrl-xk" => Some("sr-Cyrl-RS"),
        _ => None,
    };

    if let Some(target) = affinity_target {
        if available.iter().any(|l| l.eq_ignore_ascii_case(target)) {
            return target.to_string();
        }
    }

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
        follows_global_acp_model: cli.follows_global_acp_model,
        custom_model_selection: cli.custom_model_selection,
        custom_models: cli.custom_models,
        cloud_models: cli.cloud_models,
        delegate_agent: cli.delegate_agent,
        delegate_model: cli.delegate_model,
        no_autofix: cli.no_autofix,
        yolo_mode: cli.yolo_mode,
        yolo_command_blocked: cli.yolo_command_blocked,
        setup: cli.setup,
        initial_view: match cli.initial_view {
            InitialView::Chat => helper::config::InitialView::Chat,
            InitialView::Sessions => helper::config::InitialView::Sessions,
        },
        initial_pane_position: cli.initial_pane_position,
        owner_tab_id: cli.owner_tab_id,
        owner_window_id: cli.owner_window_id,
        initial_load_session_id: cli.initial_load_session_id,
        initial_load_cwd: cli.initial_load_cwd,
        start_stashed: cli.start_stashed,
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
    let mut cli = Cli::parse();

    // Logging must be initialized before locale, telemetry, or dispatch work.
    logging::init(&process_label(&cli));
    logging::install_ctrl_handler();
    logging::install_panic_hook();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "=== wta starting ===");

    let locale = cli
        .language
        .clone()
        .or_else(sys_locale::get_locale)
        .unwrap_or_else(|| "en-US".to_string());
    rust_i18n::set_locale(&normalize_locale(&locale));

    telemetry::register();

    if cli.test_pipe {
        let result = cli::wt::run_test_pipe().await;
        if let Err(err) = &result {
            tracing::error!(error = ?err, "wta exiting with error");
        }
        logging::shutdown_flush();
        return result;
    }
    if cli.info {
        let result = cli::wt::run_info_mode().await;
        if let Err(err) = &result {
            tracing::error!(error = ?err, "wta exiting with error");
        }
        logging::shutdown_flush();
        return result;
    }

    let json_mode = cli.json;
    let command = cli.command.take();
    let result = match command {
        Some(command) => cli::run(command, json_mode).await,
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

    if let Err(err) = &result {
        tracing::error!(error = ?err, "wta exiting with error");
    }
    logging::shutdown_flush();
    result
}

/// Pick the log file label for this process from its launch mode.
fn process_label(cli: &Cli) -> String {
    if cli.master.is_some() {
        return "main_master".to_string();
    }
    if cli.connect_master.is_some() {
        return format!("main_helper-{}", std::process::id());
    }
    if cli.test_pipe || cli.info {
        return "cli".to_string();
    }
    match &cli.command {
        None => "main".to_string(),
        Some(Command::Delegate { .. }) => "delegate".to_string(),
        Some(Command::ProbeModels { .. })
        | Some(Command::ProbeAgentSources { .. })
        | Some(Command::ProbeSessions { .. })
        | Some(Command::ProbeHostSessions { .. }) => "probe".to_string(),
        Some(Command::Hooks {
            action: HooksAction::Install { .. },
        }) => "install-hooks".to_string(),
        Some(_) => "cli".to_string(),
    }
}

#[cfg(test)]
mod cli_tests;
