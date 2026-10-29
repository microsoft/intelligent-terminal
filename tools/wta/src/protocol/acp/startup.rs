use agent_client_protocol as acp;
use anyhow::{Context, Result};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::conn;
use super::process::AgentProcess;
use super::spawn::{AgentStderrLog, ChildEnvironmentPolicy, SharedProviderSelection};
use crate::agent_lifecycle::{providers, StartupLease, StartupPhase};
use crate::agent_source::AgentSource;

pub(crate) struct InitializedProbe {
    pub process: AgentProcess,
    pub connection: conn::ClientLink,
    pub response: acp::schema::v1::InitializeResponse,
}

impl InitializedProbe {
    pub(crate) async fn close(&mut self) -> Result<()> {
        self.connection.shutdown();
        self.process
            .terminate()
            .await
            .context(StartupPhase::Cleanup)
    }
}

pub(crate) async fn start_probe(
    command: &str,
    agent_id: Option<&str>,
    source: &AgentSource,
    environment: ChildEnvironmentPolicy,
    startup: &mut StartupLease,
) -> Result<InitializedProbe> {
    start_probe_with_provider(
        command,
        agent_id,
        source,
        environment,
        SharedProviderSelection::Inherit,
        startup,
    )
    .await
}

pub(crate) async fn start_probe_with_provider(
    command: &str,
    agent_id: Option<&str>,
    source: &AgentSource,
    environment: ChildEnvironmentPolicy,
    provider: SharedProviderSelection<'_>,
    startup: &mut StartupLease,
) -> Result<InitializedProbe> {
    let preparation = providers::prepare(command, agent_id, source)
        .await
        .context(StartupPhase::Preparation)?;
    startup.prepared(preparation);
    let mut spawned = super::spawn::spawn_agent_process_for_source_with_provider(
        command,
        None,
        agent_id,
        source,
        environment,
        provider,
    )
    .context(StartupPhase::Spawn)?;
    tracing::info!(
        target: "acp_startup",
        pid = ?spawned.child.id(),
        source = source.kind(),
        "ACP startup process created"
    );
    let stderr = AgentStderrLog::new(spawned.label().to_string());
    let stderr_task = spawned.child.stderr.take().map(|pipe| stderr.drain(pipe));
    let result: Result<_> = async {
        let output = spawned.child.take_stdout().await?;
        let input = spawned
            .child
            .stdin
            .take()
            .context("ACP stdin is unavailable")?;
        let (connection, io) = conn::spawn_client(
            acp::Client.builder().name("wta-startup-probe"),
            conn::byte_streams(input.compat_write(), output.compat()),
        );
        tokio::task::spawn_local(async move {
            if let Err(error) = io.await {
                tracing::debug!(target: "acp_startup", %error, "probe transport stopped");
            }
        });
        let request = acp::schema::v1::InitializeRequest::new(acp::schema::ProtocolVersion::V1)
            .client_capabilities(acp::schema::v1::ClientCapabilities::new().terminal(true))
            .client_info(acp::schema::v1::Implementation::new(
                "wta-probe",
                env!("CARGO_PKG_VERSION"),
            ));
        let started = std::time::Instant::now();
        let initialized = tokio::time::timeout(
            crate::agent_lifecycle::INITIALIZE_TIMEOUT,
            connection.initialize(request),
        )
        .await;
        crate::telemetry::log_acp_initialize_complete(
            started.elapsed().as_secs_f64() * 1000.0,
            matches!(initialized, Ok(Ok(_))),
            "Probe",
            match &initialized {
                Ok(Ok(_)) => "",
                Ok(Err(_)) => "AcpError",
                Err(_) => "Timeout",
            },
            match &initialized {
                Ok(Err(error)) => error.code.into(),
                _ => 0,
            },
        );
        let response = initialized
            .context(StartupPhase::Initialize)?
            .map_err(anyhow::Error::new)
            .context(StartupPhase::Initialize)?;
        stderr.mark_initialized();
        startup.succeeded()?;
        Ok((connection, response))
    }
    .await;
    match result {
        Ok((connection, response)) => Ok(InitializedProbe {
            process: spawned.child,
            connection,
            response,
        }),
        Err(error) => {
            let lines = stderr
                .finish_failed_startup(&mut spawned.child, stderr_task)
                .await
                .context(StartupPhase::Cleanup)?;
            Err(crate::agent_lifecycle::StartupDetails::attach(
                error, &lines,
            ))
        }
    }
}
