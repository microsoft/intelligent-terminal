pub(crate) mod agent_tools;
pub(crate) mod args;
pub(crate) mod delegate;
pub(crate) mod hooks;
pub(crate) mod probes;
pub(crate) mod sessions;
pub(crate) mod wt;

use anyhow::Result;

use args::{Command, HooksAction, SessionsAction};

async fn open_named_pipe(
    pipe_name: &str,
) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    retry_named_pipe_open(pipe_name, || {
        tokio::net::windows::named_pipe::ClientOptions::new().open(pipe_name)
    })
    .await
}

async fn retry_named_pipe_open<T>(
    pipe_name: &str,
    mut open: impl FnMut() -> std::io::Result<T>,
) -> std::io::Result<T> {
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY};
    const BACKOFF_MS: &[u64] = &[20, 50, 100, 200, 500, 1000];

    let mut attempt = 0;
    loop {
        match open() {
            Ok(pipe) => return Ok(pipe),
            Err(error) => {
                let retryable = matches!(
                    error.raw_os_error().map(|code| code as u32),
                    Some(ERROR_FILE_NOT_FOUND | ERROR_PIPE_BUSY)
                );
                if !retryable || attempt == BACKOFF_MS.len() {
                    return Err(error);
                }
                let wait_ms = BACKOFF_MS[attempt];
                tracing::debug!(
                    target: "cli_pipe",
                    pipe = pipe_name,
                    attempt = attempt + 1,
                    wait_ms,
                    error = %error,
                    "named pipe temporarily unavailable; retrying"
                );
                tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
                attempt += 1;
            }
        }
    }
}

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
        Command::ProbeSessions { agent } => probes::run_sessions(&agent).await,
        Command::ProbeHostSessions { agent } => probes::run_host_sessions(&agent).await,
    }
}

#[cfg(test)]
mod pipe_tests {
    use super::retry_named_pipe_open;
    use std::io;
    use std::time::Duration;

    #[tokio::test(start_paused = true)]
    async fn busy_pipe_retries_past_the_previous_session_connect_window() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};

        let name = format!(r"\\.\pipe\wta-retry-test-{}", uuid::Uuid::new_v4());
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .unwrap();
        let client = ClientOptions::new().open(&name).unwrap();
        server.connect().await.unwrap();
        let _occupied = (server, client);
        let mut available = None;
        let started = tokio::time::Instant::now();
        let mut attempts = 0;
        let result = retry_named_pipe_open(&name, || {
            attempts += 1;
            if attempts == 5 {
                available = Some(ServerOptions::new().create(&name).unwrap());
            }
            let result = ClientOptions::new().open(&name);
            if attempts <= 4 {
                assert_eq!(result.as_ref().unwrap_err().raw_os_error(), Some(231));
            }
            result
        })
        .await;
        let mut client = result.unwrap();
        assert_eq!(attempts, 5);
        assert!(started.elapsed() > Duration::from_millis(100));
        let mut server = available.unwrap();
        server.connect().await.unwrap();
        client.write_all(b"ready").await.unwrap();
        let mut message = [0; 5];
        server.read_exact(&mut message).await.unwrap();
        assert_eq!(&message, b"ready");
    }

    #[tokio::test(start_paused = true)]
    async fn missing_pipe_retries_but_access_denied_does_not() {
        let mut attempts = 0;
        let result = retry_named_pipe_open("starting-master", || {
            attempts += 1;
            if attempts == 1 {
                Err(io::Error::from_raw_os_error(2))
            } else {
                Ok("connected")
            }
        })
        .await;
        assert_eq!(result.unwrap(), "connected");
        assert_eq!(attempts, 2);
        let started = tokio::time::Instant::now();
        let mut attempts = 0;
        let result = retry_named_pipe_open::<()>("denied-master", || {
            attempts += 1;
            Err(io::Error::from_raw_os_error(5))
        })
        .await;
        assert_eq!(result.unwrap_err().raw_os_error(), Some(5));
        assert_eq!(attempts, 1);
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn permanently_busy_pipe_preserves_the_error_with_a_bounded_wait() {
        let started = tokio::time::Instant::now();
        let mut attempts = 0;
        let result = retry_named_pipe_open::<()>("unavailable-master", || {
            attempts += 1;
            Err(io::Error::from_raw_os_error(231))
        })
        .await;
        assert_eq!(result.unwrap_err().raw_os_error(), Some(231));
        assert_eq!(attempts, 7);
        assert_eq!(started.elapsed(), Duration::from_millis(1870));
    }
}
