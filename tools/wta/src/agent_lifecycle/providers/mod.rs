mod antigravity;
mod claude;
mod codex;
mod copilot;
mod gemini;
mod opencode;

use anyhow::Result;
use futures::future::BoxFuture;

use crate::agent_source::AgentSource;
use crate::protocol::acp::process::{PreparedProcessResources, ProcessResources, ResourceRecovery};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DependencyKind {
    NativeCli,
    NpmAdapter,
    StandaloneAcp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreparationOutcome {
    NotRequired,
    DeferredToLaunch,
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedArtifact {
    pub outcome: PreparationOutcome,
    pub executable: Option<String>,
}

impl From<PreparationOutcome> for PreparedArtifact {
    fn from(outcome: PreparationOutcome) -> Self {
        Self {
            outcome,
            executable: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallerKind {
    Manual,
    CopilotCli,
    StandaloneAcp,
}

pub(crate) trait ProviderLifecycleAdapter: Sync {
    fn family_id(&self) -> &'static str;
    fn dependency_kind(&self) -> DependencyKind;
    fn installer_kind(&self) -> InstallerKind {
        InstallerKind::Manual
    }
    fn find_host_acp(&self) -> Option<String> {
        crate::agent_check::find_exe(self.family_id())
    }
    fn host_bootstrap_available(&self, _launch_ready: bool) -> bool {
        false
    }
    fn runtime_resources(
        &self,
        _source: &AgentSource,
    ) -> Result<Option<Box<dyn ProcessResources>>> {
        Ok(None)
    }
    fn recover_runtime_resources(&self) -> Result<ResourceRecovery> {
        Ok(ResourceRecovery::default())
    }
    fn prepare<'a>(
        &'a self,
        _command: &'a str,
        _source: &'a AgentSource,
    ) -> BoxFuture<'a, Result<PreparedArtifact>> {
        Box::pin(async move {
            Ok(match self.dependency_kind() {
                DependencyKind::NativeCli => PreparationOutcome::NotRequired,
                DependencyKind::NpmAdapter => PreparationOutcome::DeferredToLaunch,
                DependencyKind::StandaloneAcp => {
                    anyhow::bail!("standalone ACP provider has no preparation implementation")
                }
            }
            .into())
        })
    }
}

pub(super) struct NativeProvider {
    pub id: &'static str,
    pub installer: InstallerKind,
}

impl ProviderLifecycleAdapter for NativeProvider {
    fn family_id(&self) -> &'static str {
        self.id
    }

    fn dependency_kind(&self) -> DependencyKind {
        if crate::agent_registry::lookup_profile_by_id(self.id)
            .acp_launch_command
            .starts_with("npx ")
        {
            DependencyKind::NpmAdapter
        } else {
            DependencyKind::NativeCli
        }
    }

    fn installer_kind(&self) -> InstallerKind {
        self.installer
    }
}

static PROVIDERS: [&dyn ProviderLifecycleAdapter; 6] = [
    &copilot::ADAPTER,
    &claude::ADAPTER,
    &codex::ADAPTER,
    &gemini::ADAPTER,
    &opencode::ADAPTER,
    &antigravity::ADAPTER,
];

pub(crate) fn lookup(agent_id: &str) -> Option<&'static dyn ProviderLifecycleAdapter> {
    PROVIDERS
        .iter()
        .copied()
        .find(|provider| provider.family_id().eq_ignore_ascii_case(agent_id))
}

pub(crate) async fn prepare(
    command: &str,
    agent_id: Option<&str>,
    source: &AgentSource,
) -> Result<PreparedArtifact> {
    let id = agent_id.unwrap_or_else(|| crate::agent_registry::resolve_agent_id_from_cmd(command));
    match lookup(id) {
        Some(provider) => provider.prepare(command, source).await,
        None => Ok(PreparationOutcome::NotRequired.into()),
    }
}

pub(crate) async fn prepare_runtime_resources(
    command: &str,
    agent_id: Option<&str>,
    source: &AgentSource,
) -> Result<Option<PreparedProcessResources>> {
    let id = agent_id.unwrap_or_else(|| crate::agent_registry::resolve_agent_id_from_cmd(command));
    let Some(provider) = lookup(id) else {
        return Ok(None);
    };
    let source = source.clone();
    prepare_runtime_resources_with(move || provider.runtime_resources(&source)).await
}

async fn prepare_runtime_resources_with(
    prepare: impl FnOnce() -> Result<Option<Box<dyn ProcessResources>>> + Send + 'static,
) -> Result<Option<PreparedProcessResources>> {
    use anyhow::Context;

    tokio::task::spawn_blocking(move || {
        // Cancellation detaches blocking work. Establish cleanup ownership
        // before the result can be discarded by the cancelled receiver.
        prepare().map(|resources| resources.map(PreparedProcessResources::new))
    })
    .await
    .context("prepare ACP process resources task failed")?
}

pub(crate) fn recover_runtime_resources() -> ResourceRecovery {
    let mut result = ResourceRecovery::default();
    for provider in PROVIDERS {
        match provider.recover_runtime_resources() {
            Ok(report) => {
                result.reclaimed += report.reclaimed;
                result.deferred += report.deferred;
                result.failed += report.failed;
            }
            Err(error) => {
                result.failed += 1;
                tracing::warn!(target: "acp_process", agent = provider.family_id(), error = %format!("{error:#}"),
                    "provider runtime recovery incomplete");
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CleanupWitness {
        cleaned: bool,
        dropped: Option<tokio::sync::oneshot::Sender<bool>>,
    }

    impl ProcessResources for CleanupWitness {
        fn job_name(&self) -> &str {
            unreachable!("cancelled preparation must not create a process")
        }

        fn configure(&self, _command: &mut tokio::process::Command) {
            unreachable!("cancelled preparation must not configure a process")
        }

        fn cleanup(&mut self) -> Result<()> {
            assert!(
                !self.cleaned,
                "unused resources must be reclaimed only once"
            );
            self.cleaned = true;
            Ok(())
        }
    }

    impl Drop for CleanupWitness {
        fn drop(&mut self) {
            if let Some(dropped) = self.dropped.take() {
                let _ = dropped.send(self.cleaned);
            }
        }
    }

    #[tokio::test]
    async fn rust_review_cancelled_blocking_preparation_reclaims_unused_resources() {
        let (started, preparing) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let (dropped, reclaimed) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(prepare_runtime_resources_with(move || {
            let resources = CleanupWitness {
                cleaned: false,
                dropped: Some(dropped),
            };
            started.send(()).unwrap();
            released
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            Ok(Some(Box::new(resources) as Box<dyn ProcessResources>))
        }));
        preparing.await.unwrap();
        task.abort();
        assert!(task.await.err().unwrap().is_cancelled());
        release.send(()).unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), reclaimed)
                .await
                .unwrap()
                .unwrap(),
            "the detached preparation must reclaim resources that no process ever owned"
        );
    }

    #[tokio::test]
    async fn rust_review_unclaimed_preparation_result_reclaims_unused_resources() {
        let (dropped, mut reclaimed) = tokio::sync::oneshot::channel();
        let resources = prepare_runtime_resources_with(move || {
            Ok(Some(Box::new(CleanupWitness {
                cleaned: false,
                dropped: Some(dropped),
            }) as Box<dyn ProcessResources>))
        })
        .await
        .unwrap();
        assert_eq!(
            reclaimed.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty),
            "preparation must retain resources until handoff or cancellation"
        );
        drop(resources);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), reclaimed)
                .await
                .unwrap()
                .unwrap()
        );
    }

    #[test]
    fn provider_adapters_cover_the_registry_without_owning_custom_commands() {
        for profile in crate::agent_registry::KNOWN_AGENTS {
            let provider = lookup(profile.id).expect("every built-in has a lifecycle adapter");
            assert_eq!(provider.family_id(), profile.id);
            assert_eq!(
                provider.dependency_kind() == DependencyKind::NpmAdapter,
                profile.acp_launch_command.starts_with("npx ")
            );
        }
        assert_eq!(PROVIDERS.len(), crate::agent_registry::KNOWN_AGENTS.len());
        assert!(lookup("custom:antigravity").is_none());
        assert!(lookup("unknown").is_none());
    }

    #[tokio::test]
    async fn native_and_adapter_preparation_preserve_the_existing_launch_contracts() {
        for profile in crate::agent_registry::KNOWN_AGENTS {
            if profile.id == crate::agent_registry::ANTIGRAVITY_AGENT_ID {
                continue;
            }
            let expected = if profile.acp_launch_command.starts_with("npx ") {
                PreparationOutcome::DeferredToLaunch
            } else {
                PreparationOutcome::NotRequired
            };
            assert_eq!(
                prepare(
                    &crate::agent_registry::build_acp_command(profile.id, None),
                    Some(profile.id),
                    &AgentSource::Host,
                )
                .await
                .unwrap()
                .outcome,
                expected
            );
        }
        assert_eq!(
            prepare(
                "agy_acp_server.exe",
                Some("custom:antigravity"),
                &AgentSource::Host,
            )
            .await
            .unwrap()
            .outcome,
            PreparationOutcome::NotRequired
        );
    }
}
