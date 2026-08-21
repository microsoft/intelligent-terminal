pub(crate) mod agent_tools;
pub(crate) mod args;
pub(crate) mod delegate;
pub(crate) mod hooks;
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
            delegate_model,
            delegate_source,
            delegate_wsl_distro,
            cwd,
        } => {
            delegate::run(
                prompt.as_deref(),
                &agent,
                delegate_agent.as_deref(),
                delegate_model.as_deref(),
                delegate_source.as_deref(),
                delegate_wsl_distro.as_deref(),
                cwd.as_deref(),
            )
            .await
        }
        Command::Sessions { action } => match action {
            SessionsAction::List { master, origin } => {
                sessions::run_list(master, origin.to_filter(), json_mode).await
            }
        },
        Command::Hooks { action } => match action {
            HooksAction::Install { cli } => hooks::run_install(cli, json_mode),
            HooksAction::Status => hooks::run_status(json_mode),
            HooksAction::Uninstall { cli } => hooks::run_uninstall(cli, json_mode),
        },
        Command::ProbeModels { agent } => probes::run_models(&agent).await,
        Command::ProbeAgentSources { wsl_distro } => probes::run_agent_sources(&wsl_distro).await,
        Command::ProbeSessions { agent } => probes::run_sessions(&agent).await,
        Command::ProbeHostSessions { agent } => probes::run_host_sessions(&agent).await,
    }
}
