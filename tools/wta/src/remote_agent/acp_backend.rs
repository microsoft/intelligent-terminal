//! Host-owned ACP process and session lifecycle adapter.

use std::collections::HashMap;
#[cfg(not(windows))]
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

use agent_client_protocol as acp;
use anyhow::{anyhow, Context, Result};
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::protocol::acp::conn;
use crate::protocol::acp::spawn::{spawn_agent_process, AgentStderrLog, ChildEnvironmentPolicy};

const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(60);
const NEW_SESSION_TIMEOUT: Duration = Duration::from_secs(30);
const CLOSE_SESSION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub(crate) struct HostAcpBackend {
    sender: mpsc::UnboundedSender<BackendCommand>,
}

impl HostAcpBackend {
    pub(crate) fn start() -> (Self, mpsc::UnboundedReceiver<BackendEvent>) {
        Self::start_inner(None)
    }

    #[cfg(test)]
    fn start_with_command(command: String) -> (Self, mpsc::UnboundedReceiver<BackendEvent>) {
        Self::start_inner(Some(command))
    }

    fn start_inner(
        command_override: Option<String>,
    ) -> (Self, mpsc::UnboundedReceiver<BackendEvent>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let (events, event_receiver) = mpsc::unbounded_channel();
        let (provider_events, provider_event_receiver) = mpsc::unbounded_channel();
        tokio::task::spawn_local(run_backend(
            receiver,
            events,
            provider_events,
            provider_event_receiver,
            command_override,
        ));
        (Self { sender }, event_receiver)
    }

    pub(crate) async fn create_session(
        &self,
        resource: String,
        provider: String,
        working_directories: Option<Vec<String>>,
    ) -> Result<String> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(BackendCommand::Create {
                resource,
                provider,
                working_directories,
                reply,
            })
            .map_err(|_| anyhow!("Host ACP backend stopped"))?;
        response
            .await
            .context("Host ACP backend stopped before creating the session")?
    }

    pub(crate) async fn dispose_session(&self, resource: String) -> Result<()> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(BackendCommand::Dispose { resource, reply })
            .map_err(|_| anyhow!("Host ACP backend stopped"))?;
        response
            .await
            .context("Host ACP backend stopped before disposing the session")?
    }

    pub(crate) async fn prompt(&self, resource: String, text: String) -> Result<()> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(BackendCommand::Prompt {
                resource,
                text,
                reply,
            })
            .map_err(|_| anyhow!("Host ACP backend stopped"))?;
        response
            .await
            .context("Host ACP backend stopped before accepting the prompt")?
    }

    pub(crate) async fn cancel(&self, resource: String) -> Result<()> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(BackendCommand::Cancel { resource, reply })
            .map_err(|_| anyhow!("Host ACP backend stopped"))?;
        response
            .await
            .context("Host ACP backend stopped before cancelling the prompt")?
    }
}

pub(crate) enum BackendEvent {
    AgentText {
        agent_session_id: String,
        content: String,
    },
    PromptFinished {
        agent_session_id: String,
        duration_ms: i64,
        cancelled: bool,
        error: Option<String>,
    },
    ProviderExited {
        agent_session_ids: Vec<String>,
        error: String,
    },
}

enum BackendCommand {
    Create {
        resource: String,
        provider: String,
        working_directories: Option<Vec<String>>,
        reply: oneshot::Sender<Result<String>>,
    },
    Dispose {
        resource: String,
        reply: oneshot::Sender<Result<()>>,
    },
    Prompt {
        resource: String,
        text: String,
        reply: oneshot::Sender<Result<()>>,
    },
    Cancel {
        resource: String,
        reply: oneshot::Sender<Result<()>>,
    },
}

struct AgentRuntime {
    connection: conn::ClientLink,
    generation: u64,
    _child: tokio::process::Child,
    _stderr_task: Option<tokio::task::JoinHandle<()>>,
}

struct BackendSession {
    provider: String,
    agent_session_id: acp::schema::v1::SessionId,
}

struct ProviderEvent {
    provider: String,
    generation: u64,
    error: String,
}

async fn run_backend(
    mut receiver: mpsc::UnboundedReceiver<BackendCommand>,
    events: mpsc::UnboundedSender<BackendEvent>,
    provider_events: mpsc::UnboundedSender<ProviderEvent>,
    mut provider_event_receiver: mpsc::UnboundedReceiver<ProviderEvent>,
    command_override: Option<String>,
) {
    let mut agents = HashMap::<String, AgentRuntime>::new();
    let mut sessions = HashMap::<String, BackendSession>::new();
    let mut active_prompts = HashMap::<String, oneshot::Sender<()>>::new();
    let mut next_generation = 1u64;

    loop {
        let command = tokio::select! {
            command = receiver.recv() => {
                let Some(command) = command else {
                    break;
                };
                command
            }
            provider_event = provider_event_receiver.recv() => {
                let Some(provider_event) = provider_event else {
                    break;
                };
                if agents
                    .get(&provider_event.provider)
                    .is_some_and(|agent| agent.generation == provider_event.generation)
                {
                    evict_provider(
                        &mut agents,
                        &mut sessions,
                        &mut active_prompts,
                        &events,
                        &provider_event.provider,
                        provider_event.error,
                    );
                }
                continue;
            }
        };
        match command {
            BackendCommand::Create {
                resource,
                provider,
                working_directories,
                reply,
            } => {
                let result = create_session(
                    &mut agents,
                    &mut sessions,
                    &mut active_prompts,
                    &events,
                    &provider_events,
                    &mut next_generation,
                    command_override.as_deref(),
                    resource,
                    provider,
                    working_directories,
                )
                .await;
                let _ = reply.send(result);
            }
            BackendCommand::Dispose { resource, reply } => {
                if let Some(cancel) = active_prompts.remove(&resource) {
                    let _ = cancel.send(());
                }
                let result = dispose_session(&agents, &mut sessions, &resource).await;
                let _ = reply.send(result);
            }
            BackendCommand::Prompt {
                resource,
                text,
                reply,
            } => {
                let result = sessions
                    .get(&resource)
                    .and_then(|session| {
                        agents.get(&session.provider).map(|agent| {
                            (
                                agent.connection.clone(),
                                agent.generation,
                                session.provider.clone(),
                                session.agent_session_id.clone(),
                            )
                        })
                    })
                    .ok_or_else(|| anyhow!("ACP session is unavailable"));
                match result {
                    Ok((connection, generation, provider, agent_session_id)) => {
                        let events = events.clone();
                        let provider_events = provider_events.clone();
                        let (cancel, cancelled) = oneshot::channel();
                        active_prompts.insert(resource, cancel);
                        tokio::task::spawn_local(async move {
                            let started = Instant::now();
                            tokio::select! {
                                result = connection.prompt(acp::schema::v1::PromptRequest::new(
                                    agent_session_id.clone(),
                                    vec![text.into()],
                                )) => {
                                    let cancelled = result
                                        .as_ref()
                                        .is_ok_and(|response| {
                                            response.stop_reason
                                                == acp::schema::v1::StopReason::Cancelled
                                        });
                                    let error = result.err().map(|error| error.to_string());
                                    let _ = events.send(BackendEvent::PromptFinished {
                                        agent_session_id: agent_session_id.to_string(),
                                        duration_ms: i64::try_from(started.elapsed().as_millis())
                                            .unwrap_or(i64::MAX),
                                        cancelled,
                                        error: error.clone(),
                                    });
                                    if let Some(error) = error {
                                        let _ = provider_events.send(ProviderEvent {
                                            provider,
                                            generation,
                                            error: format!("ACP prompt failed: {error}"),
                                        });
                                    }
                                }
                                _ = cancelled => {}
                            }
                        });
                        let _ = reply.send(Ok(()));
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
            BackendCommand::Cancel { resource, reply } => {
                if let Some(cancel) = active_prompts.remove(&resource) {
                    let _ = cancel.send(());
                }
                let result = sessions
                    .get(&resource)
                    .and_then(|session| {
                        agents.get(&session.provider).map(|agent| {
                            (agent.connection.clone(), session.agent_session_id.clone())
                        })
                    })
                    .ok_or_else(|| anyhow!("ACP session is unavailable"));
                let result = match result {
                    Ok((connection, agent_session_id)) => connection
                        .cancel(acp::schema::v1::CancelNotification::new(agent_session_id))
                        .await
                        .map_err(|error| anyhow!("ACP session/cancel failed: {error}")),
                    Err(error) => Err(error),
                };
                let _ = reply.send(result);
            }
        }
    }
}

fn evict_provider(
    agents: &mut HashMap<String, AgentRuntime>,
    sessions: &mut HashMap<String, BackendSession>,
    active_prompts: &mut HashMap<String, oneshot::Sender<()>>,
    events: &mpsc::UnboundedSender<BackendEvent>,
    provider: &str,
    error: String,
) {
    let Some(mut runtime) = agents.remove(provider) else {
        return;
    };
    let _ = runtime._child.start_kill();
    let resources = sessions
        .iter()
        .filter(|(_, session)| session.provider == provider)
        .map(|(resource, _)| resource.clone())
        .collect::<Vec<_>>();
    let mut agent_session_ids = Vec::with_capacity(resources.len());
    for resource in resources {
        if let Some(cancel) = active_prompts.remove(&resource) {
            let _ = cancel.send(());
        }
        if let Some(session) = sessions.remove(&resource) {
            agent_session_ids.push(session.agent_session_id.to_string());
        }
    }
    if !agent_session_ids.is_empty() {
        let _ = events.send(BackendEvent::ProviderExited {
            agent_session_ids,
            error,
        });
    }
}

async fn create_session(
    agents: &mut HashMap<String, AgentRuntime>,
    sessions: &mut HashMap<String, BackendSession>,
    active_prompts: &mut HashMap<String, oneshot::Sender<()>>,
    events: &mpsc::UnboundedSender<BackendEvent>,
    provider_events: &mpsc::UnboundedSender<ProviderEvent>,
    next_generation: &mut u64,
    command_override: Option<&str>,
    resource: String,
    provider: String,
    working_directories: Option<Vec<String>>,
) -> Result<String> {
    anyhow::ensure!(
        crate::agent_registry::is_known_id(&provider),
        "unknown ACP provider '{provider}'"
    );
    let cwd = working_directory(working_directories.as_deref())?;
    if !agents.contains_key(&provider) {
        let generation = *next_generation;
        *next_generation = next_generation.saturating_add(1);
        let runtime = spawn_agent(
            &provider,
            generation,
            command_override,
            events.clone(),
            provider_events.clone(),
        )
        .await?;
        agents.insert(provider.clone(), runtime);
    }
    let result = {
        let agent = agents
            .get(&provider)
            .context("spawned ACP provider is unavailable")?;
        tokio::time::timeout(
            NEW_SESSION_TIMEOUT,
            agent
                .connection
                .new_session(acp::schema::v1::NewSessionRequest::new(cwd)),
        )
        .await
    };
    let response = match result {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            evict_provider(
                agents,
                sessions,
                active_prompts,
                events,
                &provider,
                format!("ACP session/new failed: {error}"),
            );
            return Err(anyhow!("ACP session/new failed: {error}"));
        }
        Err(_) => {
            evict_provider(
                agents,
                sessions,
                active_prompts,
                events,
                &provider,
                format!(
                    "ACP session/new timed out after {}s",
                    NEW_SESSION_TIMEOUT.as_secs()
                ),
            );
            return Err(anyhow!(
                "ACP session/new timed out after {}s",
                NEW_SESSION_TIMEOUT.as_secs()
            ));
        }
    };
    let agent_session_id = response.session_id;
    let id = agent_session_id.to_string();
    sessions.insert(
        resource,
        BackendSession {
            provider,
            agent_session_id,
        },
    );
    Ok(id)
}

async fn dispose_session(
    agents: &HashMap<String, AgentRuntime>,
    sessions: &mut HashMap<String, BackendSession>,
    resource: &str,
) -> Result<()> {
    let Some(session) = sessions.remove(resource) else {
        return Ok(());
    };
    let Some(agent) = agents.get(&session.provider) else {
        return Ok(());
    };
    tokio::time::timeout(
        CLOSE_SESSION_TIMEOUT,
        agent
            .connection
            .close_session(acp::schema::v1::CloseSessionRequest::new(
                session.agent_session_id,
            )),
    )
    .await
    .map_err(|_| {
        anyhow!(
            "ACP session/close timed out after {}s",
            CLOSE_SESSION_TIMEOUT.as_secs()
        )
    })?
    .map_err(|error| anyhow!("ACP session/close failed: {error}"))?;
    Ok(())
}

async fn spawn_agent(
    provider: &str,
    generation: u64,
    command_override: Option<&str>,
    events: mpsc::UnboundedSender<BackendEvent>,
    provider_events: mpsc::UnboundedSender<ProviderEvent>,
) -> Result<AgentRuntime> {
    let command = command_override
        .map(str::to_string)
        .unwrap_or_else(|| crate::agent_registry::build_acp_command(provider, None));
    let mut spawned = spawn_agent_process(
        &command,
        None,
        Some(provider),
        ChildEnvironmentPolicy::ApplySharedProvider,
    )
    .with_context(|| format!("spawn ACP provider '{provider}'"))?;
    let stdin = spawned
        .child
        .stdin
        .take()
        .context("agent CLI child has no stdin")?;
    let stdout = spawned
        .child
        .stdout
        .take()
        .context("agent CLI child has no stdout")?;
    let stderr_log = AgentStderrLog::new(provider.to_string());
    let stderr_task = spawned
        .child
        .stderr
        .take()
        .map(|stderr| stderr_log.drain(stderr));

    let builder = acp::Client
        .builder()
        .name("wta-remote-agent-host")
        .on_receive_request(
            |_request: acp::schema::v1::AgentRequest,
             responder: acp::Responder<serde_json::Value>,
             _connection| async move {
                responder.respond_with_error(acp::Error::method_not_found())
            },
            acp::on_receive_request!(),
        )
        .on_receive_notification(
            move |notification: acp::schema::v1::AgentNotification, _connection| {
                let events = events.clone();
                async move {
                    if let acp::schema::v1::AgentNotification::SessionNotification(notification) =
                        notification
                    {
                        if let acp::schema::v1::SessionUpdate::AgentMessageChunk(chunk) =
                            notification.update
                        {
                            if let acp::schema::v1::ContentBlock::Text(text) = chunk.content {
                                let _ = events.send(BackendEvent::AgentText {
                                    agent_session_id: notification.session_id.to_string(),
                                    content: text.text,
                                });
                            }
                        }
                    }
                    Ok(())
                }
            },
            acp::on_receive_notification!(),
        );
    let (connection, handle_io) = conn::spawn_client(
        builder,
        conn::byte_streams(stdin.compat_write(), stdout.compat()),
    );
    let provider_label = provider.to_string();
    tokio::task::spawn_local(async move {
        let error = match handle_io.await {
            Ok(()) => {
                tracing::info!(
                target = "remote_agent_host",
                provider = %provider_label,
                "Host ACP provider connection ended"
                );
                "ACP provider connection ended".to_string()
            }
            Err(error) => {
                tracing::error!(
                target = "remote_agent_host",
                provider = %provider_label,
                %error,
                "Host ACP provider connection failed"
                );
                format!("ACP provider connection failed: {error}")
            }
        };
        let _ = provider_events.send(ProviderEvent {
            provider: provider_label,
            generation,
            error,
        });
    });

    let request = acp::schema::v1::InitializeRequest::new(acp::schema::ProtocolVersion::V1)
        .client_capabilities(acp::schema::v1::ClientCapabilities::new())
        .client_info(
            acp::schema::v1::Implementation::new(
                "wta-remote-agent-host",
                env!("CARGO_PKG_VERSION"),
            )
            .title("Intelligent Terminal Remote Agent Host"),
        );
    let initialize = tokio::time::timeout(INITIALIZE_TIMEOUT, connection.initialize(request)).await;
    match initialize {
        Ok(Ok(_)) => stderr_log.mark_initialized(),
        Ok(Err(error)) => {
            let stderr = stderr_log
                .finish_failed_startup(&mut spawned.child, stderr_task)
                .await;
            return Err(anyhow!(
                "ACP initialize failed for '{provider}': {error}{}",
                format_startup_stderr(&stderr)
            ));
        }
        Err(_) => {
            let stderr = stderr_log
                .finish_failed_startup(&mut spawned.child, stderr_task)
                .await;
            return Err(anyhow!(
                "ACP initialize timed out after {}s for '{provider}'{}",
                INITIALIZE_TIMEOUT.as_secs(),
                format_startup_stderr(&stderr)
            ));
        }
    }

    Ok(AgentRuntime {
        connection,
        generation,
        _child: spawned.child,
        _stderr_task: stderr_task,
    })
}

fn working_directory(working_directories: Option<&[String]>) -> Result<PathBuf> {
    let Some(uri) = working_directories.and_then(|directories| directories.first()) else {
        return std::env::current_dir().context("resolve Remote Agent Host working directory");
    };
    file_uri_to_path(uri)
}

fn file_uri_to_path(uri: &str) -> Result<PathBuf> {
    let path = uri
        .strip_prefix("file:///")
        .ok_or_else(|| anyhow!("working directory must be an absolute local file URI"))?;
    anyhow::ensure!(
        !path.is_empty() && !path.contains('%') && !path.contains('#') && !path.contains('?'),
        "working directory URI must be unescaped and contain no query or fragment"
    );
    #[cfg(windows)]
    {
        Ok(PathBuf::from(path.replace('/', "\\")))
    }
    #[cfg(not(windows))]
    {
        Ok(Path::new("/").join(path))
    }
}

fn format_startup_stderr(lines: &[String]) -> String {
    lines
        .last()
        .map(|line| format!("\n  agent stderr: {line}"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use std::path::Path;
    #[cfg(windows)]
    use std::time::Duration;

    #[cfg(windows)]
    use tokio::sync::mpsc;
    #[cfg(windows)]
    use tokio::task::LocalSet;

    use super::file_uri_to_path;
    #[cfg(windows)]
    use super::{BackendEvent, HostAcpBackend};

    #[test]
    fn local_file_uri_is_converted_to_an_acp_working_directory() {
        let path = file_uri_to_path("file:///C:/repo").expect("convert file URI");
        #[cfg(windows)]
        assert_eq!(path.to_string_lossy(), "C:\\repo");
    }

    #[test]
    fn encoded_or_remote_working_directory_is_rejected() {
        assert!(file_uri_to_path("file://server/share").is_err());
        assert!(file_uri_to_path("file:///C:/repo%20name").is_err());
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "current_thread")]
    async fn process_mock_exercises_stdio_lifecycle_and_provider_restart() {
        LocalSet::new()
            .run_until(async {
                let log_path = std::env::temp_dir().join(format!(
                    "wta-mock-acp-{}-{}.log",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .expect("system time")
                        .as_nanos()
                ));
                let command = mock_agent_command(&log_path);
                let (backend, mut events) = HostAcpBackend::start_with_command(command);

                let first_session = backend
                    .create_session("ahp-session:/one".to_string(), "copilot".to_string(), None)
                    .await
                    .expect("create first process-backed ACP session");
                assert_eq!(first_session, "mock-session-1");

                backend
                    .prompt("ahp-session:/one".to_string(), "hello".to_string())
                    .await
                    .expect("start process-backed ACP prompt");
                assert_agent_text(&mut events, "mock-session-1", "mock ").await;
                assert_agent_text(&mut events, "mock-session-1", "reply").await;
                match next_event(&mut events).await {
                    BackendEvent::PromptFinished {
                        agent_session_id,
                        cancelled,
                        error,
                        ..
                    } => {
                        assert_eq!(agent_session_id, "mock-session-1");
                        assert!(!cancelled);
                        assert_eq!(error, None);
                    }
                    _ => panic!("expected prompt completion"),
                }

                backend
                    .prompt("ahp-session:/one".to_string(), "WAIT".to_string())
                    .await
                    .expect("start cancellable ACP prompt");
                backend
                    .cancel("ahp-session:/one".to_string())
                    .await
                    .expect("cancel ACP prompt");
                backend
                    .dispose_session("ahp-session:/one".to_string())
                    .await
                    .expect("close ACP session");

                let crash_session = backend
                    .create_session(
                        "ahp-session:/crash".to_string(),
                        "copilot".to_string(),
                        None,
                    )
                    .await
                    .expect("create crash ACP session");
                assert_eq!(crash_session, "mock-session-2");
                backend
                    .prompt("ahp-session:/crash".to_string(), "CRASH".to_string())
                    .await
                    .expect("start crashing ACP prompt");

                loop {
                    if let BackendEvent::ProviderExited {
                        agent_session_ids, ..
                    } = next_event(&mut events).await
                    {
                        assert_eq!(agent_session_ids, vec!["mock-session-2"]);
                        break;
                    }
                }

                let replacement_session = backend
                    .create_session(
                        "ahp-session:/replacement".to_string(),
                        "copilot".to_string(),
                        None,
                    )
                    .await
                    .expect("respawn ACP provider");
                assert_eq!(replacement_session, "mock-session-1");
                backend
                    .dispose_session("ahp-session:/replacement".to_string())
                    .await
                    .expect("close replacement ACP session");

                let log = std::fs::read_to_string(&log_path).expect("read mock ACP process log");
                assert_eq!(log.matches("initialize").count(), 2);
                assert!(log.contains("prompt:mock-session-1:hello"));
                assert!(log.contains("cancel:mock-session-1"));
                assert!(log.contains("close:mock-session-1"));
                assert!(log.contains("prompt:mock-session-2:CRASH"));
                std::fs::remove_file(log_path).expect("remove mock ACP process log");
            })
            .await;
    }

    #[cfg(windows)]
    async fn assert_agent_text(
        events: &mut mpsc::UnboundedReceiver<BackendEvent>,
        expected_session_id: &str,
        expected_content: &str,
    ) {
        match next_event(events).await {
            BackendEvent::AgentText {
                agent_session_id,
                content,
            } => {
                assert_eq!(agent_session_id, expected_session_id);
                assert_eq!(content, expected_content);
            }
            _ => panic!("expected agent text"),
        }
    }

    #[cfg(windows)]
    async fn next_event(events: &mut mpsc::UnboundedReceiver<BackendEvent>) -> BackendEvent {
        tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("timed out waiting for backend event")
            .expect("backend event channel closed")
    }

    #[cfg(windows)]
    fn mock_agent_command(log_path: &Path) -> String {
        let log_path = log_path.to_string_lossy().replace('\'', "''");
        let script = format!(
            "$env:WTA_MOCK_ACP_LOG = '{log_path}'\n{}",
            include_str!("../../testdata/mock-acp-agent.ps1")
        );
        let bytes = script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        format!(
            "powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand {}",
            base64_encode(&bytes)
        )
    }

    #[cfg(windows)]
    fn base64_encode(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let value = (u32::from(chunk[0]) << 16)
                | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
                | u32::from(*chunk.get(2).unwrap_or(&0));
            encoded.push(ALPHABET[((value >> 18) & 0x3f) as usize] as char);
            encoded.push(ALPHABET[((value >> 12) & 0x3f) as usize] as char);
            encoded.push(if chunk.len() > 1 {
                ALPHABET[((value >> 6) & 0x3f) as usize] as char
            } else {
                '='
            });
            encoded.push(if chunk.len() > 2 {
                ALPHABET[(value & 0x3f) as usize] as char
            } else {
                '='
            });
        }
        encoded
    }
}
