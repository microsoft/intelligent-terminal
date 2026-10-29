use super::{agent_key, StartupFailure, StartupRegistry};
use crate::agent_source::AgentSource;
use crate::protocol::acp::spawn::ChildEnvironmentPolicy;

pub(crate) struct VerifiedStartup {
    pub executable: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
}

pub(crate) async fn verify(
    command: &str,
    agent_id: &str,
    source: &AgentSource,
    retry_startup: bool,
) -> Result<VerifiedStartup, StartupFailure> {
    let result = async {
        let registry = StartupRegistry::runtime()?;
        let key = agent_key(command, Some(agent_id), source, "native", false);
        if retry_startup {
            registry.retry(&key).await?;
        }
        let mut lease = registry.lock(&key).await?;
        lease.begin()?;
        let started = crate::protocol::acp::startup::start_probe(
            command,
            Some(agent_id),
            source,
            ChildEnvironmentPolicy::CleanCloudDiscovery,
            &mut lease,
        )
        .await;
        match started {
            Ok(mut probe) => {
                let info = probe.response.agent_info.take();
                if let Err(error) = probe.close().await {
                    return Err(lease.failed(error));
                }
                Ok(VerifiedStartup {
                    executable: lease.executable().map(str::to_string),
                    name: info.as_ref().map(|info| info.name.clone()),
                    version: info.map(|info| info.version),
                })
            }
            Err(error) => Err(lease.failed(error)),
        }
    }
    .await;
    result.map_err(|error| StartupFailure::from_error(&error, 0, None, false))
}
