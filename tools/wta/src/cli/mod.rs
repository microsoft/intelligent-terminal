pub(crate) mod agent_tools;
pub(crate) mod args;
pub(crate) mod delegate;
pub(crate) mod hooks;
pub(crate) mod native_agent;
pub(crate) mod probes;
pub(crate) mod sessions;
pub(crate) mod wt;

use anyhow::Result;

use args::{Command, HooksAction, SessionsAction};

pub(crate) async fn run(command: Command, json_mode: bool) -> Result<()> {
    match command {
        command @ (Command::Info
        | Command::TestPipe
        | Command::ListWindows
        | Command::ListTabs { .. }
        | Command::ListPanes { .. }
        | Command::NewTab { .. }
        | Command::SplitPane { .. }
        | Command::CapturePane { .. }
        | Command::KillPane { .. }
        | Command::ActivePane
        | Command::PaneStatus { .. }
        | Command::WaitFor { .. }
        | Command::PipeId
        | Command::SetEnv { .. }
        | Command::Listen { .. }) => wt::run(command, json_mode).await,
        Command::ResolveCommand { token, shell, cwd } => {
            agent_tools::run_command_resolution(&token, &shell, cwd.as_deref(), json_mode).await
        }
        Command::ProposeTerminalActions {
            channel,
            payload_json,
        } => agent_tools::run_action_proposal(channel, payload_json).await,
        Command::Delegate {
            prompt,
            agent,
            delegate_agent,
            delegate_agent_id,
            delegate_model,
            delegate_source,
            delegate_wsl_distro,
            cwd,
            preserve_sidebar_view,
            split_pane,
            split_session,
            split_direction,
            split_size,
        } => {
            delegate::run(
                prompt.as_deref(),
                &agent,
                delegate_agent.as_deref(),
                delegate_model.as_deref(),
                delegate_source.as_deref(),
                delegate_wsl_distro.as_deref(),
                cwd.as_deref(),
                preserve_sidebar_view,
                split_pane.as_deref(),
                split_session.as_deref(),
                &split_direction,
                split_size,
                delegate_agent_id.as_deref(),
            )
            .await
        }
        Command::Sessions { action } => match action {
            SessionsAction::List {
                master,
                origin,
                include_status,
            } => {
                sessions::run_list(master, origin.to_filter(), false, json_mode, include_status)
                    .await
            }
            SessionsAction::Refresh { master } => {
                sessions::run_list(
                    master,
                    crate::agent_sessions::OriginFilter::All,
                    true,
                    json_mode,
                    json_mode,
                )
                .await
            }
            SessionsAction::Activate {
                session_id,
                provider,
                location,
                wsl_distro,
                universe,
                window_id,
                activation_id,
                status_only,
            } => {
                sessions::run_activate(
                    &session_id,
                    &provider,
                    &location,
                    wsl_distro.as_deref(),
                    universe,
                    window_id,
                    activation_id,
                    status_only,
                    json_mode,
                )
                .await
            }
        },
        Command::Hooks { action } => match action {
            HooksAction::Install { cli, force } => hooks::run_install(cli, force, json_mode),
            HooksAction::Status => hooks::run_status(json_mode),
            HooksAction::Uninstall { cli } => hooks::run_uninstall(cli, json_mode),
        },
        Command::ProbeModels { agent } => probes::run_models(&agent).await,
        Command::ProbeAgentSources { wsl_distro } => probes::run_agent_sources(&wsl_distro).await,
        Command::ProbeHostAgents => probes::run_host_agents(),
        Command::ProbeProfileAgents => native_agent::probe(),
        Command::LaunchAgent {
            agent_id,
            model,
            permission_mode,
            additional_args,
        } => {
            let code = native_agent::launch(
                &agent_id,
                model.as_deref(),
                permission_mode.as_deref(),
                &additional_args,
            )
            .await?;
            crate::logging::shutdown_flush();
            std::process::exit(code);
        }
        Command::ProbeSessions { agent } => probes::run_sessions(&agent).await,
        Command::ProbeHostSessions { agent } => probes::run_host_sessions(&agent).await,
    }
}
